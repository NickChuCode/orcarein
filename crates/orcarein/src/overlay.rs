//! Terminal UX overlay: an opt-in, capability-gated alternate-screen surface.
//!
//! The pager (file content, conversation transcript) is the first consumer; a
//! GPIO live monitor will share the same primitive later. Everything here is
//! pure *presentation* — nothing written through the overlay ever enters the
//! persisted `Session` (so the model's prompt prefix, and its cache, stay
//! untouched). The terminal control (alternate screen, raw mode, ratatui
//! rendering) lives behind the `tui` feature; the decision logic below is pure
//! and always compiled, so it is unit-tested without a terminal.

#[cfg(feature = "tui")]
use crate::color::{self, Token};

/// Whether pager content is plain text, Markdown to be rendered, or a standalone
/// code file to be syntax-highlighted (the `String` is the language name).
pub enum DocKind {
    Plain,
    Markdown,
    // The lang is only read by the `tui` code-doc renderer; under no-tui `/show`
    // prints in place and the field is unused.
    #[cfg_attr(not(feature = "tui"), allow(dead_code))]
    Code(String),
}

/// Shows `content` to the user, paging it through an alternate-screen overlay
/// when that is both possible (capable tty) and warranted (doesn't fit one
/// screen); otherwise prints it in place. `kind` selects plain vs Markdown
/// rendering. This is the single choke point for "show a lot of text".
pub fn show_paged(title: &str, content: &str, kind: DocKind) -> std::io::Result<()> {
    #[cfg(feature = "tui")]
    {
        if paged_overlay(title, content, kind)? {
            return Ok(());
        }
    }
    #[cfg(not(feature = "tui"))]
    {
        let _ = kind;
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
fn paged_overlay(title: &str, content: &str, kind: DocKind) -> std::io::Result<bool> {
    use ratatui::crossterm::terminal;
    use std::io::IsTerminal;

    let is_tty = std::io::stdout().is_terminal();
    let term = std::env::var("TERM").ok();
    if !overlay_capable(is_tty, term.as_deref()) {
        return Ok(false);
    }
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let usable = rows.saturating_sub(1); // reserve one row for the footer
    let rgb = color::use_rgb(color::detect(true));
    let doc = match kind {
        DocKind::Markdown => crate::markdown::render(content, cols, rgb, false),
        DocKind::Code(lang) => code_doc(content, &lang, rgb),
        DocKind::Plain => plain_doc(content, rgb),
    };
    if !needs_pager(doc.len(), usable) {
        return Ok(false);
    }
    run_pager(title, doc)?;
    Ok(true)
}

/// Build a single [`RenderedLine`] from a plain (or role-bar) line — exposed so
/// `/history` can assemble a doc of role bars + Markdown-rendered content.
#[cfg(feature = "tui")]
pub(crate) fn styled_line(line: &str, rgb: bool) -> RenderedLine {
    plain_line(line, rgb)
}

/// Page a pre-built doc (role bars + rendered content), or print it in place
/// when it fits a screen / isn't a capable tty.
#[cfg(feature = "tui")]
pub(crate) fn show_doc(title: &str, doc: Vec<RenderedLine>) -> std::io::Result<()> {
    use std::io::IsTerminal;
    let is_tty = std::io::stdout().is_terminal();
    let term = std::env::var("TERM").ok();
    let rows = ratatui::crossterm::terminal::size()
        .map(|(_, r)| r)
        .unwrap_or(24);
    if overlay_capable(is_tty, term.as_deref()) && needs_pager(doc.len(), rows.saturating_sub(1)) {
        return run_pager(title, doc);
    }
    if !title.is_empty() {
        println!("{}", crate::header::slim_title_bar(title, term_cols()));
    }
    for l in &doc {
        println!("{}", l.plain);
    }
    Ok(())
}

/// Raw-mode RAII guard: disables raw mode on drop. Shared by the pager
/// (via the alternate-screen overlay) and the modal editor (raw only).
#[cfg(feature = "tui")]
pub(crate) struct RawModeGuard;

#[cfg(feature = "tui")]
impl Drop for RawModeGuard {
    fn drop(&mut self) {
        use ratatui::crossterm::terminal::disable_raw_mode;
        let _ = disable_raw_mode();
    }
}

/// Enter raw mode and hand back a guard that restores it on drop. Does NOT
/// touch the alternate screen — the modal editor renders inline.
#[cfg(feature = "tui")]
pub(crate) fn enter_raw() -> std::io::Result<RawModeGuard> {
    ratatui::crossterm::terminal::enable_raw_mode()?;
    Ok(RawModeGuard)
}

/// Shared overlay primitive: enter raw mode + the alternate screen and hand
/// back a ratatui `Terminal`. The returned guard restores the terminal on drop
/// (even on panic / early return). Both the pager and the GPIO live monitor
/// build their loops on top of this.
///
/// Composition: `OverlayGuard` owns a [`RawModeGuard`]. Its own `Drop` body
/// leaves the alternate screen; afterwards the owned `RawModeGuard` field's
/// drop disables raw mode. End state is identical to the old single-guard:
/// alt screen left + raw mode off, with no double-disable.
#[cfg(feature = "tui")]
pub(crate) struct OverlayGuard {
    // Field drop runs AFTER OverlayGuard::drop's body → raw mode is disabled
    // after the alternate screen is left. Held only for its Drop side effect.
    _raw: RawModeGuard,
}

#[cfg(feature = "tui")]
impl Drop for OverlayGuard {
    fn drop(&mut self) {
        use ratatui::crossterm::terminal::LeaveAlternateScreen;
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
    use ratatui::crossterm::terminal::EnterAlternateScreen;
    use ratatui::Terminal;

    let raw = enter_raw()?;
    ratatui::crossterm::execute!(std::io::stdout(), EnterAlternateScreen)?;
    let guard = OverlayGuard { _raw: raw };
    let terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    Ok((terminal, guard))
}

#[cfg(feature = "tui")]
type Tui = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

/// One pre-rendered pager line: styled spans plus the plain-text shadow
/// (`plain` == the concatenation of the span texts) used for search and width.
#[cfg(feature = "tui")]
pub(crate) struct RenderedLine {
    pub spans: Vec<(String, ratatui::style::Style)>,
    pub plain: String,
}

/// Build a doc from plain text: each line one default-styled span, except
/// transcript role bars (`▌ …`) which are colored by role (when `rgb`). This is
/// the non-markdown path (`.md`-less `/show`, and the fallback).
#[cfg(feature = "tui")]
pub(crate) fn plain_doc(content: &str, rgb: bool) -> Vec<RenderedLine> {
    content.split('\n').map(|l| plain_line(l, rgb)).collect()
}

/// Build a doc from a standalone code file: each line syntax-highlighted by
/// `lang` (when `rgb`), full-width with no gutter (a `bat`/`less`-style viewer).
/// `rgb` off → plain lines. `plain == spans concat` holds (search/scroll rely
/// on it), since the lexer's runs reconstruct each line exactly.
#[cfg(feature = "tui")]
fn code_doc(content: &str, lang: &str, rgb: bool) -> Vec<RenderedLine> {
    use ratatui::style::Style;
    content
        .split('\n')
        .map(|line| {
            if !rgb {
                return plain_line(line, false);
            }
            let spans: Vec<(String, Style)> = crate::syntax::highlight(line, lang)
                .into_iter()
                .map(|(s, kind)| {
                    let st = match color::syn_color(kind) {
                        Some(c) => Style::default().fg(c),
                        None => Style::default(),
                    };
                    (s, st)
                })
                .collect();
            // An empty line highlights to nothing — keep one empty span so the
            // pager has a row (and `plain` stays the empty string).
            let spans = if spans.is_empty() {
                vec![(String::new(), Style::default())]
            } else {
                spans
            };
            RenderedLine {
                spans,
                plain: line.to_string(),
            }
        })
        .collect()
}

/// One plain (or role-bar) line → [`RenderedLine`].
#[cfg(feature = "tui")]
fn plain_line(line: &str, rgb: bool) -> RenderedLine {
    use ratatui::style::Style;
    let st = |t: Token| {
        if rgb {
            Style::default().fg(color::rt(t))
        } else {
            Style::default()
        }
    };
    let spans = if let Some(rest) = line.strip_prefix("▌ ") {
        let mut spans = vec![("▌ ".to_string(), st(Token::Brand))];
        if rest == "你" {
            spans.push((rest.to_string(), st(Token::OrcaWhite)));
        } else if let Some(after) = rest.strip_prefix("OrcaRein") {
            spans.push(("OrcaRein".to_string(), st(Token::Accent)));
            if !after.is_empty() {
                spans.push((after.to_string(), st(Token::Dim)));
            }
        } else {
            spans.push((rest.to_string(), st(Token::Dim)));
        }
        spans
    } else {
        vec![(line.to_string(), Style::default())]
    };
    RenderedLine {
        spans,
        plain: line.to_string(),
    }
}

/// Overlay search-hit styling onto a line's spans: every case-insensitive match
/// of `query` in `plain` becomes yellow-on-near-black, keeping the span's own
/// modifiers (so a hit on bold/code/link stays bold/underlined, only the color
/// is taken over). `plain` must equal the concatenation of the span texts.
#[cfg(feature = "tui")]
fn apply_query_highlight(
    spans: &[(String, ratatui::style::Style)],
    plain: &str,
    query: &str,
) -> Vec<(String, ratatui::style::Style)> {
    if query.is_empty() {
        return spans.to_vec();
    }
    // Hit byte-ranges in `plain` via the pure segmenter.
    let mut hits: Vec<(usize, usize)> = Vec::new();
    let mut b = 0usize;
    for (seg, hit) in highlight_segments(plain, query) {
        let n = seg.len();
        if hit {
            hits.push((b, b + n));
        }
        b += n;
    }
    if hits.is_empty() {
        return spans.to_vec();
    }
    let is_hit = |pos: usize| hits.iter().any(|&(s, e)| pos >= s && pos < e);

    let mut out = Vec::new();
    let mut cur = 0usize; // byte offset into `plain`
    for (text, st) in spans {
        let mut run_start = 0usize;
        let mut run_hit = is_hit(cur);
        for (bo, _ch) in text.char_indices() {
            let h = is_hit(cur + bo);
            if h != run_hit {
                push_run(&mut out, &text[run_start..bo], run_hit, *st);
                run_start = bo;
                run_hit = h;
            }
        }
        push_run(&mut out, &text[run_start..], run_hit, *st);
        cur += text.len();
    }
    out
}

/// Push a sub-run with hit styling (or the base style) — empty runs skipped.
#[cfg(feature = "tui")]
fn push_run(
    out: &mut Vec<(String, ratatui::style::Style)>,
    s: &str,
    hit: bool,
    base: ratatui::style::Style,
) {
    use ratatui::style::{Color, Modifier};
    if s.is_empty() {
        return;
    }
    let style = if hit {
        base.fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        base
    };
    out.push((s.to_string(), style));
}

/// A brand-framed slim title bar with an accent title (`╭─ <title> ─…─╮`),
/// exactly `width` wide. Falls back to the plain [`crate::header::slim_title_bar`]
/// string when color is off.
#[cfg(feature = "tui")]
fn title_bar(title: &str, width: u16, rgb: bool) -> ratatui::text::Line<'static> {
    use ratatui::style::Style;
    use ratatui::text::{Line, Span};
    let w = width as usize;
    if !rgb || w < 4 {
        return Line::from(crate::header::slim_title_bar(title, width));
    }
    let label = crate::header::truncate_to_width(title, w.saturating_sub(4));
    let fill = w.saturating_sub(5 + crate::header::disp_width(&label));
    let fg = |t: Token| Style::default().fg(color::rt(t));
    Line::from(vec![
        Span::styled("╭─ ", fg(Token::Brand)),
        Span::styled(label, fg(Token::Accent)),
        Span::styled(" ", fg(Token::Brand)),
        Span::styled("─".repeat(fill), fg(Token::Brand)),
        Span::styled("╮", fg(Token::Brand)),
    ])
}

/// Render one pager frame — the scrolled body window plus a footer bar — and
/// hand back the body viewport height. `footer_fn` is called with that height so
/// callers can show the visible line range. `offset`/`xoff` scroll the body
/// vertically/horizontally; `query` overlays search hits; `wrap` soft-wraps.
#[cfg(feature = "tui")]
#[allow(clippy::too_many_arguments)]
fn draw_view(
    terminal: &mut Tui,
    title: &str,
    doc: &[RenderedLine],
    offset: usize,
    xoff: u16,
    query: &str,
    wrap: bool,
    footer_fn: impl Fn(usize) -> String,
) -> std::io::Result<usize> {
    use ratatui::layout::{Constraint, Layout};
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span, Text};
    use ratatui::widgets::{Paragraph, Wrap};

    let rgb = color::use_rgb(color::detect(true));
    let mut viewport_h = 1usize;
    terminal.draw(|f| {
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area());
        // Title row: brand-framed bar with an accent title (plain when no color).
        f.render_widget(
            Paragraph::new(title_bar(title, f.area().width, rgb)),
            chunks[0],
        );
        viewport_h = (chunks[1].height as usize).max(1);

        // Body: pre-styled spans + per-line search overlay; ratatui handles the
        // vertical/horizontal scroll (and soft-wrap when enabled).
        let lines: Vec<Line> = doc
            .iter()
            .map(|rl| {
                Line::from(
                    apply_query_highlight(&rl.spans, &rl.plain, query)
                        .into_iter()
                        .map(|(s, st)| Span::styled(s, st))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let mut para = Paragraph::new(Text::from(lines)).scroll((offset as u16, xoff));
        if wrap {
            para = para.wrap(Wrap { trim: false });
        }
        f.render_widget(para, chunks[1]);

        // Footer: padded to a full bar, on the status ground (reverse if no color).
        let mut ft = footer_fn(viewport_h);
        let total = f.area().width as usize;
        let used = crate::header::disp_width(&ft);
        if used < total {
            ft.push_str(&" ".repeat(total - used));
        }
        let footer = if rgb {
            Line::from(ft).style(
                Style::default()
                    .bg(color::STATUS_BG)
                    .fg(color::rt(Token::Fg)),
            )
        } else {
            Line::from(ft).style(Style::default().add_modifier(Modifier::REVERSED))
        };
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
    doc: &[RenderedLine],
    offset: usize,
    xoff: u16,
    wrap: bool,
) -> std::io::Result<Option<String>> {
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};

    let mut input = String::new();
    loop {
        // Highlight live as the query is typed.
        draw_view(terminal, title, doc, offset, xoff, &input, wrap, |_vh| {
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
fn run_pager(title: &str, doc: Vec<RenderedLine>) -> std::io::Result<()> {
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};

    const XSTEP: u16 = 8;
    let (mut terminal, _guard) = enter_overlay()?;
    let total = doc.len();
    let plains: Vec<&str> = doc.iter().map(|l| l.plain.as_str()).collect();
    let mut offset = 0usize;
    let mut xoff: u16 = 0;
    let mut wrap = false;
    let mut query = String::new();
    let mut last_match: Option<usize> = None;

    // Jump so the matched line sits at the top of the viewport (clamped).
    let jump_to = |m: Option<usize>, off: &mut usize, max: usize| {
        if let Some(m) = m {
            *off = m.min(max);
        }
    };

    loop {
        let viewport_h = draw_view(
            &mut terminal,
            title,
            &doc,
            offset,
            xoff,
            &query,
            wrap,
            |vh| {
                let end = (offset + vh).min(total);
                let status = if query.is_empty() {
                    String::new()
                } else {
                    match last_match {
                        Some(m) => format!("  /{query}→行{}", m + 1),
                        None => format!("  /{query} 无匹配"),
                    }
                };
                let xinfo = if xoff > 0 {
                    format!(" ↔{xoff}")
                } else {
                    String::new()
                };
                let wmark = if wrap { " ⤶换行" } else { "" };
                format!(
                    " {title}  行 {}-{}/{}{}{}   j/k · ←/→ · z · /搜索 n/N · g/G · q 退出{} ",
                    (offset + 1).min(total),
                    end,
                    total,
                    xinfo,
                    wmark,
                    status
                )
            },
        )?;
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
                if let Some(q) = read_search_query(&mut terminal, title, &doc, offset, xoff, wrap)?
                {
                    query = q;
                    last_match = find_match(&plains, &query, offset, true);
                    jump_to(last_match, &mut offset, max);
                }
            }
            KeyCode::Char('n') if !query.is_empty() => {
                let from = last_match.map_or(offset, |m| (m + 1) % total);
                last_match = find_match(&plains, &query, from, true);
                jump_to(last_match, &mut offset, max);
            }
            KeyCode::Char('N') if !query.is_empty() => {
                let from = last_match.map_or(offset, |m| (m + total - 1) % total);
                last_match = find_match(&plains, &query, from, false);
                jump_to(last_match, &mut offset, max);
            }
            // Horizontal scroll (no-op when soft-wrap is on).
            KeyCode::Left => xoff = xoff.saturating_sub(XSTEP),
            KeyCode::Right => xoff = xoff.saturating_add(XSTEP),
            // Toggle soft-wrap; wrap removes horizontal overflow, so reset xoff.
            KeyCode::Char('z') => {
                wrap = !wrap;
                if wrap {
                    xoff = 0;
                }
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
