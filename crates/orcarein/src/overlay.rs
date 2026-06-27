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

/// Terminal width in columns; 80 when unknown or under `--no-default-features`.
/// The single shared width probe — the startup header (`header_env`) and the
/// overlay surfaces both read width through here so detection lives in one place.
#[cfg(feature = "tui")]
pub(crate) fn term_cols() -> u16 {
    ratatui::crossterm::terminal::size()
        .map(|(c, _)| c)
        .unwrap_or(80)
}
#[cfg(not(feature = "tui"))]
pub(crate) fn term_cols() -> u16 {
    80
}

/// The non-overlay path: a plain header + the content on the scrolling
/// terminal. Used when paging is impossible (piped / dumb / headless), when the
/// content fits one screen, or in `--no-default-features` (no `tui`) builds.
fn print_in_place(title: &str, content: &str) {
    if !title.is_empty() {
        println!("{}", crate::header::slim_title_bar(title, term_cols()));
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

/// Builds the body as ratatui `Text`, painting every `query` match with a
/// high-contrast highlight so the jumped-to term is visible. With an empty
/// query it's a plain borrowed `Text` (no per-line allocation). Span splitting
/// is the pure [`highlight_segments`].
#[cfg(feature = "tui")]
fn highlighted_text<'a>(body: &'a str, query: &str) -> ratatui::text::Text<'a> {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span, Text};

    if query.is_empty() {
        return Text::raw(body);
    }
    let hl = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let lines: Vec<Line> = body
        .split('\n')
        .map(|line| {
            Line::from(
                highlight_segments(line, query)
                    .into_iter()
                    .map(|(s, hit)| {
                        if hit {
                            Span::styled(s, hl)
                        } else {
                            Span::raw(s)
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    Text::from(lines)
}

/// Render one pager frame — the scrolled body window plus a reversed footer —
/// and hand back the body viewport height. `footer_fn` is called with that
/// height inside the draw, so callers can show the exact visible line range.
/// `query` (when non-empty) highlights matches in the body. Shared by the
/// scroll loop and the `/`-search input prompt.
#[cfg(feature = "tui")]
fn draw_view(
    terminal: &mut Tui,
    title: &str,
    body: &str,
    offset: usize,
    query: &str,
    footer_fn: impl Fn(usize) -> String,
) -> std::io::Result<usize> {
    use ratatui::layout::{Constraint, Layout};
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Line;
    use ratatui::widgets::Paragraph;

    let mut viewport_h = 1usize;
    terminal.draw(|f| {
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area());
        // Title row: the shared slim title bar (icon + box style).
        let bar = crate::header::slim_title_bar(title, f.area().width);
        f.render_widget(Paragraph::new(bar), chunks[0]);
        viewport_h = (chunks[1].height as usize).max(1);
        f.render_widget(
            Paragraph::new(highlighted_text(body, query)).scroll((offset as u16, 0)),
            chunks[1],
        );
        let footer = Line::from(footer_fn(viewport_h))
            .style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_widget(Paragraph::new(footer), chunks[2]);
    })?;
    Ok(viewport_h)
}

/// The `/`-search input prompt: a small modal loop that echoes the query being
/// typed in the footer. Returns the entered query on Enter (None if empty), or
/// None on Esc (cancelled). Pure terminal I/O; the match logic is [`find_match`].
#[cfg(feature = "tui")]
fn read_search_query(
    terminal: &mut Tui,
    title: &str,
    body: &str,
    offset: usize,
) -> std::io::Result<Option<String>> {
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};

    let mut input = String::new();
    loop {
        // Highlight live as the query is typed.
        draw_view(terminal, title, body, offset, &input, |_vh| {
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
        let viewport_h = draw_view(&mut terminal, title, &body, offset, &query, |vh| {
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
                if let Some(q) = read_search_query(&mut terminal, title, &body, offset)? {
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
pub(crate) fn overlay_capable(is_tty: bool, term: Option<&str>) -> bool {
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

/// Splits `line` into consecutive `(segment, is_match)` spans, where every
/// case-insensitive occurrence of `query` is flagged for highlighting. The
/// segments concatenate back to `line` exactly. Matching is ASCII-case-folded
/// so byte offsets stay valid for slicing (multi-byte UTF-8 is compared as-is);
/// an empty query (or no hit) yields a single plain span covering the line.
#[cfg_attr(not(feature = "tui"), allow(dead_code))]
pub fn highlight_segments<'a>(line: &'a str, query: &str) -> Vec<(&'a str, bool)> {
    if query.is_empty() {
        return vec![(line, false)];
    }
    let hay = line.to_ascii_lowercase();
    let needle = query.to_ascii_lowercase();
    let mut segs: Vec<(&str, bool)> = Vec::new();
    let mut cursor = 0usize; // byte index into `line`
    let mut plain_start = 0usize;
    while cursor < line.len() {
        let hit = line.is_char_boundary(cursor)
            && hay[cursor..].starts_with(&needle)
            && line.is_char_boundary(cursor + needle.len());
        if hit {
            if plain_start < cursor {
                segs.push((&line[plain_start..cursor], false));
            }
            segs.push((&line[cursor..cursor + needle.len()], true));
            cursor += needle.len();
            plain_start = cursor;
        } else {
            cursor += 1;
        }
    }
    if plain_start < line.len() {
        segs.push((&line[plain_start..], false));
    }
    if segs.is_empty() {
        segs.push((line, false));
    }
    segs
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

    #[test]
    fn highlight_segments_flags_each_case_insensitive_match() {
        assert_eq!(
            highlight_segments("Foo bar foo", "foo"),
            vec![("Foo", true), (" bar ", false), ("foo", true)]
        );
    }

    #[test]
    fn highlight_segments_handles_start_end_and_adjacent_matches() {
        assert_eq!(
            highlight_segments("aXa", "a"),
            vec![("a", true), ("X", false), ("a", true)]
        );
        // Adjacent matches stay as separate flagged spans.
        assert_eq!(
            highlight_segments("aa", "a"),
            vec![("a", true), ("a", true)]
        );
    }

    #[test]
    fn highlight_segments_single_plain_span_when_no_match_or_empty_query() {
        assert_eq!(highlight_segments("hello", "zzz"), vec![("hello", false)]);
        assert_eq!(highlight_segments("hello", ""), vec![("hello", false)]);
    }

    #[test]
    fn highlight_segments_preserves_multibyte_after_a_match() {
        // ASCII match must not corrupt following multi-byte chars.
        assert_eq!(
            highlight_segments("a中文", "a"),
            vec![("a", true), ("中文", false)]
        );
    }
}
