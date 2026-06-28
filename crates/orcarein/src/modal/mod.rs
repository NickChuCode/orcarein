//! Self-built multiline vim modal editor replacing rustyline readline on
//! capable terminals (spec: 2026-06-28-orcarein-vim-modal-editor-design).
//! Pure logic in submodules is always compiled & unit-tested; the raw-mode
//! I/O loop lives here behind `tui`.
//!
//! Scaffolded incrementally: types and fields land here ahead of the tasks
//! that consume them (motions, editing, visual, undo, render, I/O loop), so
//! allow dead code module-wide until those tasks wire everything together.
#![allow(dead_code)]

pub mod buffer;
pub mod clipboard;
pub mod command;
pub mod render;

/// In-process, non-persisted input history (spec §5/§9). Owned by main.rs.
pub type History = Vec<String>;

// ---- Raw-mode I/O loop (Task 13). Only meaningful under `tui`. ----
//
// INLINE VIEWPORT + RECREATE-ON-HEIGHT-CHANGE (mechanism locked in Task 1):
// the height baked into `Viewport::Inline(h)` is IMMUTABLE for a `Terminal`'s
// life — `Terminal::resize` provably ignores it (verified against ratatui 0.29
// source). So a growing/shrinking inline box CANNOT be done by resizing the
// terminal; the ONLY way to change the inline height is to DROP the `Terminal`
// and build a fresh one with the new `Viewport::Inline(desired_h)`. We compute
// `desired_h` each loop iteration and recreate the `Terminal` ONLY when it
// changes (strictly gated on height, not per-keystroke) so we don't pollute
// scrollback or flicker on ordinary typing.
//
// This is an I/O SHELL: every pure piece it drives (buffer / command / render /
// clipboard) is already unit-tested, so there are no unit tests here — the
// acceptance is a clean compile in all feature combos (behavior is human-
// verified in Task 14).

#[cfg(feature = "tui")]
use crate::modal::buffer::EditBuffer;
#[cfg(feature = "tui")]
use crate::modal::command::{apply, CommandParser, Effect, KeyAction};

/// Outcome of one `modal_readline` call (the I/O loop's terminal states).
#[cfg(feature = "tui")]
pub enum ReadOutcome {
    /// User submitted the buffer (Enter in Normal mode). Carries the text.
    Submitted(String),
    /// User cancelled this line (Ctrl-C) — discard and re-prompt.
    Cancelled,
    /// End of input (Ctrl-D on an empty buffer) — caller should exit.
    Eof,
}

/// Read one (possibly multiline) input through the vim modal editor, rendered in
/// an INLINE ratatui viewport (no alternate screen). Returns when the user
/// submits, cancels, or signals EOF.
///
/// Starts in INSERT mode: at a REPL prompt the user expects to start typing
/// immediately (Esc drops to Normal for vim commands). `prompt` is accepted for
/// API symmetry with the old `"> "` readline but not rendered — the status line
/// already shows the mode; `let _ = prompt;` silences the unused warning.
///
/// On `Submitted` we do NOT echo the text here: main.rs's existing REPL flow
/// displays/uses the returned string, and the inline viewport's content vanishes
/// when the `Terminal` is dropped, so echoing here would double up.
#[cfg(feature = "tui")]
pub fn modal_readline(prompt: &str, history: &History) -> std::io::Result<ReadOutcome> {
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use ratatui::crossterm::terminal::size;
    use ratatui::layout::{Constraint, Layout};
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;
    use ratatui::{Terminal, TerminalOptions, Viewport};

    let _ = prompt; // not rendered (see doc comment)

    // RAII: restores raw mode on return/panic. Inline — NO alternate screen.
    let _raw = crate::overlay::enter_raw()?;

    let (term_width, term_height) = size().unwrap_or((80, 24));
    // Cap so the inline box never eats more than half the screen (min 3 rows).
    let max_rows = (term_height / 2).max(3);

    // Start in INSERT so the user can just type at the prompt.
    let mut buf = EditBuffer::new();
    buf.enter_insert_before();
    let mut parser = CommandParser::new();

    // The inline viewport height currently baked into `terminal`. `None` until
    // the first build; recreated only when `desired_h` differs (see header).
    let mut terminal: Option<Terminal<CrosstermBackend<std::io::Stdout>>> = None;
    let mut current_h: u16 = 0;

    loop {
        // 1. Desired inline height: body lines + 1 status row, clamped.
        let body_lines = buf.lines.len() as u16;
        let desired_h = (body_lines + 1).clamp(2, max_rows);

        // (Re)create the terminal only on a height change.
        if terminal.is_none() || desired_h != current_h {
            // Drop the old terminal first so its backend releases stdout, then
            // build a fresh one — the only way to change an inline height.
            drop(terminal.take());
            terminal = Some(Terminal::with_options(
                CrosstermBackend::new(std::io::stdout()),
                TerminalOptions {
                    viewport: Viewport::Inline(desired_h),
                },
            )?);
            current_h = desired_h;
        }
        let term = terminal.as_mut().expect("terminal just (re)created");

        // 2. Pure render of the buffer into the inline viewport.
        let view = render::render(&buf, term_width, desired_h);

        // 3. Draw: body lines in the top rows, status (reversed) as the last row.
        term.draw(|f| {
            let chunks =
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());

            let body: Vec<Line> = view
                .lines
                .iter()
                .map(|rl| {
                    Line::from(
                        rl.spans
                            .iter()
                            .map(|(s, hl)| {
                                if *hl {
                                    // Visual selection: reversed for a vim feel.
                                    Span::styled(
                                        s.clone(),
                                        Style::default().add_modifier(Modifier::REVERSED),
                                    )
                                } else {
                                    Span::raw(s.clone())
                                }
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            f.render_widget(Paragraph::new(body), chunks[0]);

            let status = Line::from(view.status.clone())
                .style(Style::default().add_modifier(Modifier::REVERSED));
            f.render_widget(Paragraph::new(status), chunks[1]);

            // ratatui takes (x = col, y = row); render gives (row, col).
            f.set_cursor_position((view.cursor_screen.1, view.cursor_screen.0));
        })?;

        // 4. Read one event; skip non-key and key-release events.
        let Event::Key(k) = event::read()? else {
            continue;
        };
        if k.kind == KeyEventKind::Release {
            continue; // Windows emits press+release; act on press only
        }

        // 5. Map the crossterm KeyEvent to a normalized KeyAction.
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let action = match k.code {
            KeyCode::Char(c) if ctrl => match c {
                'c' => KeyAction::CtrlC,
                'd' => KeyAction::CtrlD,
                'r' => KeyAction::CtrlR,
                _ => continue, // ignore other Ctrl-* combos
            },
            KeyCode::Char(c) => KeyAction::Char(c),
            KeyCode::Enter => KeyAction::Enter,
            KeyCode::Esc => KeyAction::Esc,
            KeyCode::Backspace => KeyAction::Backspace,
            KeyCode::Up => KeyAction::Up,
            KeyCode::Down => KeyAction::Down,
            KeyCode::Left => KeyAction::Left,
            KeyCode::Right => KeyAction::Right,
            _ => continue, // other keys: skip
        };

        // 6. Apply to the buffer and handle the resulting effect.
        match apply(&mut buf, &mut parser, action, history) {
            Effect::Submit => return Ok(ReadOutcome::Submitted(buf.text())),
            Effect::Cancel => return Ok(ReadOutcome::Cancelled),
            Effect::Eof => return Ok(ReadOutcome::Eof),
            Effect::Yank(reg) => clipboard::write_osc52(&reg.text),
            Effect::None => {}
        }
    }
}
