//! Self-built multiline vim modal editor replacing rustyline readline on
//! capable terminals (spec: 2026-06-28-orcarein-vim-modal-editor-design).
//! Pure logic in submodules is always compiled & unit-tested; the raw-mode
//! I/O loop lives here behind `tui`.

pub mod buffer;
pub mod clipboard;
pub mod command;
pub mod mention;
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
use crate::color;
#[cfg(feature = "tui")]
use crate::modal::buffer::{EditBuffer, Mode, VisualKind};
#[cfg(feature = "tui")]
use crate::modal::command::{apply, CommandParser, Effect, KeyAction};

/// Restores the terminal's default cursor shape on drop, so leaving the modal
/// editor never strands the user with a block/bar cursor we set per mode.
#[cfg(feature = "tui")]
struct CursorStyleGuard;

#[cfg(feature = "tui")]
impl Drop for CursorStyleGuard {
    fn drop(&mut self) {
        use ratatui::crossterm::cursor::SetCursorStyle;
        let _ = ratatui::crossterm::execute!(std::io::stdout(), SetCursorStyle::DefaultUserShape);
    }
}

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
/// displays/uses the returned string. Before returning we clear the inline
/// viewport (ratatui's `Terminal` drop restores the cursor but does NOT wipe the
/// drawn frame), so the editor's last frame never collides with REPL output.
#[cfg(feature = "tui")]
pub fn modal_readline(
    prompt: &str,
    history: &History,
    ctx: Option<(String, color::Token)>,
) -> std::io::Result<ReadOutcome> {
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use ratatui::crossterm::terminal::size;
    use ratatui::layout::{Constraint, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;
    use ratatui::{Terminal, TerminalOptions, Viewport};

    let _ = prompt; // not rendered (see doc comment)

    // Color capability (we're on a tty — modal is only used when overlay-capable).
    // Honors NO_COLOR / COLORTERM; RGB on capable terminals, reverse video else.
    let cmode = color::detect(true);

    // RAII: restores raw mode on return/panic. Inline — NO alternate screen.
    let _raw = crate::overlay::enter_raw()?;
    // RAII: restores the default cursor shape on return/panic (we swap block vs
    // bar per mode below).
    let _cursor_style = CursorStyleGuard;

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

    let outcome = loop {
        // 1. Desired inline height: body lines + 1 status row, clamped.
        let body_lines = buf.lines.len() as u16;
        let desired_h = (body_lines + 1).clamp(2, max_rows);

        // (Re)create the terminal only on a height change.
        if terminal.is_none() || desired_h != current_h {
            // CLEAR the old viewport before dropping it: ratatui's drop only
            // restores the cursor, so without this the old frame (notably its
            // status bar) lingers as garbage when the new, differently-sized
            // viewport is built. clear() on inline puts the cursor at the old
            // viewport's top-left and wipes from there down; the fresh terminal
            // then reserves its rows from that same anchor (grows/shrinks in
            // place). Then build a new one — the only way to change inline height.
            if let Some(mut old) = terminal.take() {
                let _ = old.clear();
                drop(old);
            }
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

        // 3. Draw: body lines (mode-colored gutter + content) on top, the status
        // bar (mode badge + position + hint) as the last row.
        let rgb = color::use_rgb(cmode);
        // The gutter / badge / selection share the mode's color.
        let mode_tok = match view.mode {
            Mode::Normal => color::Token::Brand,
            Mode::Insert => color::Token::Success,
            Mode::Visual(VisualKind::Char) => color::Token::Warning,
            Mode::Visual(VisualKind::Line) => color::Token::Accent,
        };
        let gutter_style = if rgb {
            Style::default().fg(color::rt(mode_tok))
        } else {
            Style::default()
        };
        let sel_style = if rgb {
            let bg = match view.mode {
                Mode::Visual(VisualKind::Line) => color::rt(color::Token::Accent),
                _ => color::rt(color::Token::Warning),
            };
            Style::default().bg(bg).fg(Color::Black)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        };
        term.draw(|f| {
            let chunks =
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());

            const GUTTER: &str = "▌ ";
            let body: Vec<Line> = view
                .lines
                .iter()
                .map(|rl| {
                    let mut spans = vec![Span::styled(GUTTER, gutter_style)];
                    for (s, hl) in &rl.spans {
                        if *hl {
                            spans.push(Span::styled(s.clone(), sel_style));
                        } else {
                            spans.push(Span::raw(s.clone()));
                        }
                    }
                    Line::from(spans)
                })
                .collect();
            f.render_widget(Paragraph::new(body), chunks[0]);

            // Status bar: ` BADGE ` (mode bg) + position + hint, on the status
            // ground. Narrow terminals drop the hint. NO_COLOR / 16-color reverse
            // the whole bar instead, telling the mode apart by the badge text.
            let show_hint = term_width >= crate::header::NARROW;
            let w = crate::header::disp_width;
            let status_line: Line = if rgb {
                let badge = format!(" {} ", view.badge);
                let badge_style = Style::default()
                    .fg(Color::Black)
                    .bg(color::rt(mode_tok))
                    .add_modifier(Modifier::BOLD);
                let bg = Style::default().bg(color::STATUS_BG);
                let mut spans = vec![Span::styled(badge.clone(), badge_style)];
                let mut used = w(&badge);
                spans.push(Span::styled("  ", bg));
                spans.push(Span::styled(
                    view.pos.clone(),
                    bg.fg(color::rt(color::Token::Fg)),
                ));
                used += 2 + w(&view.pos);
                if show_hint {
                    spans.push(Span::styled("   ", bg));
                    spans.push(Span::styled(
                        view.hint.clone(),
                        bg.fg(color::rt(color::Token::Dim)),
                    ));
                    used += 3 + w(&view.hint);
                }
                // Right-aligned ctx readout (colored by threshold), then pad the gap.
                let ctx_w = ctx.as_ref().map(|(l, _)| w(l)).unwrap_or(0);
                let target = (term_width as usize).saturating_sub(ctx_w);
                if used < target {
                    spans.push(Span::styled(" ".repeat(target - used), bg));
                }
                if let Some((label, tok)) = ctx.as_ref() {
                    spans.push(Span::styled(label.clone(), bg.fg(color::rt(*tok))));
                }
                Line::from(spans)
            } else {
                let mut s = if show_hint {
                    format!(" {}  {}   {} ", view.badge, view.pos, view.hint)
                } else {
                    format!(" {}  {} ", view.badge, view.pos)
                };
                let ctx_w = ctx.as_ref().map(|(l, _)| w(l)).unwrap_or(0);
                let target = (term_width as usize).saturating_sub(ctx_w);
                let used = w(&s);
                if used < target {
                    s.push_str(&" ".repeat(target - used));
                }
                if let Some((label, _)) = ctx.as_ref() {
                    s.push_str(label);
                }
                Line::from(s).style(Style::default().add_modifier(Modifier::REVERSED))
            };
            f.render_widget(Paragraph::new(status_line), chunks[1]);

            // The inline viewport is positioned ABSOLUTELY in the terminal and
            // `set_cursor_position` takes absolute coordinates — so the render's
            // viewport-relative (row, col) must be offset by the body chunk's
            // origin AND the gutter width, else the cursor jumps off. Clamp into
            // the body area so a long line can't push it off-screen.
            const GUTTER_W: u16 = 2;
            let body_area = chunks[0];
            let cx = (body_area.x + GUTTER_W + view.cursor_screen.1)
                .min(body_area.right().saturating_sub(1));
            let cy = (body_area.y + view.cursor_screen.0).min(body_area.bottom().saturating_sub(1));
            f.set_cursor_position((cx, cy));
        })?;

        // 3b. Cursor shape reflects the mode: a steady bar between chars while
        // inserting, a steady block sitting on a char in Normal/Visual (vim feel).
        // Emitted after draw so ratatui's own cursor handling doesn't override it.
        {
            use ratatui::crossterm::cursor::SetCursorStyle;
            let style = match buf.mode {
                Mode::Insert => SetCursorStyle::SteadyBar,
                _ => SetCursorStyle::SteadyBlock,
            };
            let _ = ratatui::crossterm::execute!(std::io::stdout(), style);
        }

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
            KeyCode::Tab => KeyAction::Tab,
            _ => continue, // other keys: skip
        };

        // 6. Apply to the buffer and handle the resulting effect.
        match apply(&mut buf, &mut parser, action, history) {
            Effect::Submit => break ReadOutcome::Submitted(buf.text()),
            Effect::Cancel => break ReadOutcome::Cancelled,
            Effect::Eof => break ReadOutcome::Eof,
            Effect::Yank(reg) => clipboard::write_osc52(&reg.text),
            Effect::None => {}
        }
    };

    // Clear the inline viewport before returning: ratatui's Terminal drop only
    // restores the cursor, it does NOT wipe the drawn frame — so without this the
    // last editor frame (text + status bar) collides with the REPL's own output
    // below it. clear() on an inline viewport moves the cursor to the viewport's
    // top-left and clears from there down, leaving a clean slate for the REPL.
    if let Some(term) = terminal.as_mut() {
        let _ = term.clear();
    }
    Ok(outcome)
}
