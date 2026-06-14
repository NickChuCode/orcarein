//! Terminal UX overlay: an opt-in, capability-gated alternate-screen surface.
//!
//! The pager (file content, conversation transcript) is the first consumer; a
//! GPIO live monitor will share the same primitive later. Everything here is
//! pure *presentation* — nothing written through the overlay ever enters the
//! persisted `Session` (so the model's prompt prefix, and its cache, stay
//! untouched). The terminal control (alternate screen, raw mode, ratatui
//! rendering) lives behind the `tui` feature; the decision logic below is pure
//! and always compiled, so it is unit-tested without a terminal.

/// Shows `content` to the user, paging it through an alternate-screen overlay
/// when that is both possible (capable tty) and warranted (doesn't fit one
/// screen); otherwise prints it in place. This is the single choke point for
/// "show a lot of text" — a future `$PAGER` escape hatch would slot in right
/// here, ahead of the built-in overlay.
pub fn show_paged(title: &str, content: &str) -> std::io::Result<()> {
    #[cfg(feature = "tui")]
    {
        if paged_overlay(title, content)? {
            return Ok(());
        }
    }
    print_in_place(title, content);
    Ok(())
}

/// The non-overlay path: a plain header + the content on the scrolling
/// terminal. Used when paging is impossible (piped / dumb / headless), when the
/// content fits one screen, or in `--no-default-features` (no `tui`) builds.
fn print_in_place(title: &str, content: &str) {
    if !title.is_empty() {
        println!("── {title} ──");
    }
    print!("{content}");
    if !content.ends_with('\n') {
        println!();
    }
}

/// Decides whether to page, and if so runs the overlay. Returns `Ok(true)` when
/// the overlay handled the content, `Ok(false)` to fall through to plain print.
#[cfg(feature = "tui")]
fn paged_overlay(title: &str, content: &str) -> std::io::Result<bool> {
    use ratatui::crossterm::terminal;
    use std::io::IsTerminal;

    let is_tty = std::io::stdout().is_terminal();
    let term = std::env::var("TERM").ok();
    if !overlay_capable(is_tty, term.as_deref()) {
        return Ok(false);
    }
    let (_cols, rows) = terminal::size().unwrap_or((80, 24));
    let usable = rows.saturating_sub(1); // reserve one row for the footer
    let lines: Vec<&str> = content.lines().collect();
    if !needs_pager(lines.len(), usable) {
        return Ok(false);
    }
    run_pager(title, &lines)?;
    Ok(true)
}

/// Shared overlay primitive: enter raw mode + the alternate screen and hand
/// back a ratatui `Terminal`. The returned guard restores the terminal on drop
/// (even on panic / early return). Both the pager and the GPIO live monitor
/// build their loops on top of this.
#[cfg(feature = "tui")]
pub(crate) struct OverlayGuard;

#[cfg(feature = "tui")]
impl Drop for OverlayGuard {
    fn drop(&mut self) {
        use ratatui::crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
        let _ = disable_raw_mode();
        let _ = ratatui::crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);
    }
}

#[cfg(feature = "tui")]
#[allow(clippy::type_complexity)]
pub(crate) fn enter_overlay() -> std::io::Result<(
    ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    OverlayGuard,
)> {
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
    use ratatui::Terminal;

    enable_raw_mode()?;
    ratatui::crossterm::execute!(std::io::stdout(), EnterAlternateScreen)?;
    let guard = OverlayGuard;
    let terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    Ok((terminal, guard))
}

#[cfg(feature = "tui")]
type Tui = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

/// Render one pager frame — the scrolled body window plus a reversed footer —
/// and hand back the body viewport height. `footer_fn` is called with that
/// height inside the draw, so callers can show the exact visible line range.
/// Shared by the scroll loop and the `/`-search input prompt.
#[cfg(feature = "tui")]
fn draw_view(
    terminal: &mut Tui,
    body: &str,
    offset: usize,
    footer_fn: impl Fn(usize) -> String,
) -> std::io::Result<usize> {
    use ratatui::layout::{Constraint, Layout};
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Line;
    use ratatui::widgets::Paragraph;

    let mut viewport_h = 1usize;
    terminal.draw(|f| {
        let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
        viewport_h = (chunks[0].height as usize).max(1);
        f.render_widget(Paragraph::new(body).scroll((offset as u16, 0)), chunks[0]);
        let footer = Line::from(footer_fn(viewport_h))
            .style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_widget(Paragraph::new(footer), chunks[1]);
    })?;
    Ok(viewport_h)
}

/// The `/`-search input prompt: a small modal loop that echoes the query being
/// typed in the footer. Returns the entered query on Enter (None if empty), or
/// None on Esc (cancelled). Pure terminal I/O; the match logic is [`find_match`].
#[cfg(feature = "tui")]
fn read_search_query(
    terminal: &mut Tui,
    body: &str,
    offset: usize,
) -> std::io::Result<Option<String>> {
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};

    let mut input = String::new();
    loop {
        draw_view(terminal, body, offset, |_vh| {
            format!(" /{input}   Enter 确认 · Esc 取消 ")
        })?;
        let Event::Key(k) = event::read()? else {
            continue;
        };
        if k.kind == KeyEventKind::Release {
            continue;
        }
        match k.code {
            KeyCode::Enter => return Ok((!input.is_empty()).then_some(input)),
            KeyCode::Esc => return Ok(None),
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => input.push(c),
            _ => {}
        }
    }
}

/// Drives the alternate-screen pager: render the visible window with a status
/// footer, loop on keystrokes until `q`. Terminal restore is handled by the
/// overlay guard from [`enter_overlay`].
#[cfg(feature = "tui")]
fn run_pager(title: &str, lines: &[&str]) -> std::io::Result<()> {
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};

    let (mut terminal, _guard) = enter_overlay()?;
    let body = lines.join("\n");
    let total = lines.len();
    let mut offset = 0usize;
    let mut query = String::new();
    let mut last_match: Option<usize> = None;

    // Jump so the matched line sits at the top of the viewport (clamped).
    let jump_to = |m: Option<usize>, off: &mut usize, max: usize| {
        if let Some(m) = m {
            *off = m.min(max);
        }
    };

    loop {
        let viewport_h = draw_view(&mut terminal, &body, offset, |vh| {
            let end = (offset + vh).min(total);
            let status = if query.is_empty() {
                String::new()
            } else {
                match last_match {
                    Some(m) => format!("  /{query}→行{}", m + 1),
                    None => format!("  /{query} 无匹配"),
                }
            };
            format!(
                " {title}  行 {}-{}/{}   j/k · /搜索 n/N · g/G · q 退出{status} ",
                (offset + 1).min(total),
                end,
                total
            )
        })?;
        let max = total.saturating_sub(viewport_h);

        let Event::Key(k) = event::read()? else {
            continue;
        };
        if k.kind == KeyEventKind::Release {
            continue; // Windows emits press+release; act on press only
        }
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Char('/') => {
                if let Some(q) = read_search_query(&mut terminal, &body, offset)? {
                    query = q;
                    last_match = find_match(lines, &query, offset, true);
                    jump_to(last_match, &mut offset, max);
                }
            }
            KeyCode::Char('n') if !query.is_empty() => {
                let from = last_match.map_or(offset, |m| (m + 1) % total);
                last_match = find_match(lines, &query, from, true);
                jump_to(last_match, &mut offset, max);
            }
            KeyCode::Char('N') if !query.is_empty() => {
                let from = last_match.map_or(offset, |m| (m + total - 1) % total);
                last_match = find_match(lines, &query, from, false);
                jump_to(last_match, &mut offset, max);
            }
            _ => {
                let pk = match k.code {
                    KeyCode::Char('j') | KeyCode::Down => PagerKey::Down,
                    KeyCode::Char('k') | KeyCode::Up => PagerKey::Up,
                    KeyCode::Char(' ') | KeyCode::PageDown => PagerKey::PageDown,
                    KeyCode::PageUp => PagerKey::PageUp,
                    KeyCode::Char('g') | KeyCode::Home => PagerKey::Top,
                    KeyCode::Char('G') | KeyCode::End => PagerKey::Bottom,
                    _ => PagerKey::Other,
                };
                offset = next_offset(offset, pk, total, viewport_h);
            }
        }
    }
    Ok(())
}

/// A normalized pager keystroke, decoupled from crossterm so the scroll logic
/// is testable without a real terminal.
#[cfg_attr(not(feature = "tui"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagerKey {
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
    Other,
}

/// Whether an alternate-screen overlay can safely be used. Requires an
/// interactive tty and a terminal that isn't `dumb` — serial consoles, dumb
/// terminals, and piped/redirected output return `false`, so the caller prints
/// in place instead. `term` is `$TERM` (None when unset → treated as capable,
/// since Windows leaves it unset yet supports VT sequences via crossterm).
#[cfg_attr(not(feature = "tui"), allow(dead_code))]
pub fn overlay_capable(is_tty: bool, term: Option<&str>) -> bool {
    is_tty && term.is_none_or(|t| !t.eq_ignore_ascii_case("dumb"))
}

/// `less -F` semantics: only page when the content cannot fit one screen.
#[cfg_attr(not(feature = "tui"), allow(dead_code))]
pub fn needs_pager(content_lines: usize, usable_height: u16) -> bool {
    content_lines > usable_height as usize
}

/// Pure scroll reducer: the next top-line offset after `key`, clamped to
/// `[0, max]` where `max = total_lines - viewport_h` (0 when it all fits).
#[cfg_attr(not(feature = "tui"), allow(dead_code))]
pub fn next_offset(offset: usize, key: PagerKey, total_lines: usize, viewport_h: usize) -> usize {
    let max = total_lines.saturating_sub(viewport_h);
    let page = viewport_h.max(1);
    let raw = match key {
        PagerKey::Down => offset + 1,
        PagerKey::Up => offset.saturating_sub(1),
        PagerKey::PageDown => offset + page,
        PagerKey::PageUp => offset.saturating_sub(page),
        PagerKey::Top => 0,
        PagerKey::Bottom => max,
        PagerKey::Other => offset,
    };
    raw.min(max)
}

/// Pure pager search: the index of the next line containing `query`, scanning
/// from `start` (inclusive) in the `forward`/backward direction and wrapping
/// around the ends. Matching is case-insensitive substring — forgiving for a
/// content viewer, and no regex surface. Returns `None` for an empty query, an
/// empty slice, or when no line matches anywhere.
#[cfg_attr(not(feature = "tui"), allow(dead_code))]
pub fn find_match(lines: &[&str], query: &str, start: usize, forward: bool) -> Option<usize> {
    if query.is_empty() || lines.is_empty() {
        return None;
    }
    let n = lines.len();
    let needle = query.to_lowercase();
    let start = start % n; // tolerate an out-of-range starting index
    (0..n)
        .map(|step| {
            if forward {
                (start + step) % n
            } else {
                (start + n - step) % n
            }
        })
        .find(|&idx| lines[idx].to_lowercase().contains(&needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_capable_requires_tty_and_non_dumb_term() {
        assert!(overlay_capable(true, Some("xterm-256color")));
        assert!(overlay_capable(true, None)); // Windows leaves TERM unset
        assert!(!overlay_capable(true, Some("dumb"))); // serial / dumb console
        assert!(!overlay_capable(false, Some("xterm"))); // piped / redirected
    }

    #[test]
    fn needs_pager_uses_less_f_semantics() {
        assert!(!needs_pager(10, 24)); // fits one screen → print in place
        assert!(!needs_pager(24, 24)); // exactly fills the screen
        assert!(needs_pager(25, 24)); // overflows by one → page
    }

    #[test]
    fn next_offset_scrolls_and_clamps() {
        // 100 lines, 10-row viewport → max top offset is 90.
        assert_eq!(next_offset(0, PagerKey::Down, 100, 10), 1);
        assert_eq!(next_offset(0, PagerKey::Up, 100, 10), 0); // saturate at top
        assert_eq!(next_offset(0, PagerKey::PageDown, 100, 10), 10);
        assert_eq!(next_offset(85, PagerKey::PageDown, 100, 10), 90); // clamp at bottom
        assert_eq!(next_offset(50, PagerKey::Bottom, 100, 10), 90);
        assert_eq!(next_offset(50, PagerKey::Top, 100, 10), 0);
        assert_eq!(next_offset(5, PagerKey::Other, 100, 10), 5); // unchanged
    }

    #[test]
    fn next_offset_zero_when_everything_fits() {
        assert_eq!(next_offset(0, PagerKey::Bottom, 5, 10), 0);
    }

    #[test]
    fn find_match_forward_is_case_insensitive_and_includes_start() {
        let lines = ["alpha", "BETA", "gamma beta", "delta"];
        // Forward from 0 → first match is "BETA" at 1 (case-insensitive).
        assert_eq!(find_match(&lines, "beta", 0, true), Some(1));
        // Start is inclusive: from 2, line 2 itself contains "beta".
        assert_eq!(find_match(&lines, "beta", 2, true), Some(2));
    }

    #[test]
    fn find_match_backward_finds_previous() {
        let lines = ["x match", "y", "z match", "w"];
        // Backward from 3 scans 3,2,1,0 → first hit at line 2.
        assert_eq!(find_match(&lines, "match", 3, false), Some(2));
    }

    #[test]
    fn find_match_wraps_around() {
        let lines = ["hit", "a", "b", "c"];
        // Forward from 1 → 1,2,3 miss, wrap to 0 → "hit".
        assert_eq!(find_match(&lines, "hit", 1, true), Some(0));
        // Backward from 0 → wrap to 3,2,1,0; only line 0 matches.
        assert_eq!(find_match(&lines, "hit", 0, false), Some(0));
    }

    #[test]
    fn find_match_none_on_empty_query_no_hit_or_empty_input() {
        let lines = ["a", "b"];
        assert_eq!(find_match(&lines, "", 0, true), None);
        assert_eq!(find_match(&lines, "zzz", 0, true), None);
        let empty: [&str; 0] = [];
        assert_eq!(find_match(&empty, "a", 0, true), None);
    }
}
