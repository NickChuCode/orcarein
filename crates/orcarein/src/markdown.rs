//! Terminal-native Markdown rendering for the pager (v02-25, per the Claude
//! Design "OrcaRein 终端设计系统" §08). Parses CommonMark with pulldown-cmark and
//! emits styled [`crate::overlay::RenderedLine`]s — no terminal I/O, so the layout
//! (wrapping, table columns, CJK width) is unit-tested on the `plain` shadows.
//!
//! Terminals have no font sizes, so hierarchy is carried by color + bold +
//! prefix glyphs: headings get density-decreasing blocks `█ ▓ ▒` (so even a
//! NO_COLOR terminal tells H1/H2/H3 apart), code blocks a `▏` bar, quotes a `▌`
//! bar, tables box-drawing. With `rgb` off, colors drop but those structural
//! glyphs + bold/reverse remain.

#![cfg(feature = "tui")]

use crate::color::{self, Token};
use crate::header::disp_width;
use crate::overlay::RenderedLine;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};

/// Code-block / inline-code backgrounds (only applied when `rgb`).
const CODE_BG: Color = Color::Rgb(12, 20, 34); // #0C1422
const INLINE_BG: Color = Color::Rgb(22, 34, 60); // #16223C

/// Render `src` Markdown into styled pager lines at `width` columns. `rgb` off →
/// no color (structure kept via glyphs/bold). `wrap` is accepted for symmetry
/// with the pager toggle; prose is always wrapped to `width`, while code lines
/// are kept intact (the pager scrolls/​wraps them) regardless.
pub fn render(src: &str, width: u16, rgb: bool, _wrap: bool) -> Vec<RenderedLine> {
    let mut md = Md::new(width.max(8) as usize, rgb);
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    for ev in Parser::new_ext(src, opts) {
        md.event(ev);
    }
    md.finish()
}

/// A pending list level: `ordered` carries the next number (None = bullet).
struct ListCtx {
    ordered: Option<u64>,
}

struct Md {
    width: usize,
    rgb: bool,
    out: Vec<RenderedLine>,
    runs: Vec<(String, Style)>, // inline runs for the active block
    bold: bool,
    italic: bool,
    link: Option<String>,
    heading: Option<HeadingLevel>,
    quote: usize,
    lists: Vec<ListCtx>,
    // marker prefix for the current list item's first emitted line
    pending_marker: Option<(String, Style)>,
    // code block
    in_code: bool,
    code_lang: String,
    code_buf: String,
    // table
    table: Option<Table>,
    in_head: bool,
    cur_cell: String,
    cur_row: Vec<String>,
}

struct Table {
    head: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Md {
    fn new(width: usize, rgb: bool) -> Self {
        Md {
            width,
            rgb,
            out: Vec::new(),
            runs: Vec::new(),
            bold: false,
            italic: false,
            link: None,
            heading: None,
            quote: 0,
            lists: Vec::new(),
            pending_marker: None,
            in_code: false,
            code_lang: String::new(),
            code_buf: String::new(),
            table: None,
            in_head: false,
            cur_cell: String::new(),
            cur_row: Vec::new(),
        }
    }

    fn fg(&self, t: Token) -> Style {
        if self.rgb {
            Style::default().fg(color::rt(t))
        } else {
            Style::default()
        }
    }

    /// Current inline text style from the active flags / heading context.
    fn inline(&self) -> Style {
        if let Some(level) = self.heading {
            let t = if matches!(level, HeadingLevel::H2) {
                Token::Accent
            } else {
                Token::OrcaWhite
            };
            let mut s = Style::default().add_modifier(Modifier::BOLD);
            if self.rgb {
                s = s.fg(color::rt(t));
            }
            return s;
        }
        let mut s = Style::default();
        let mut tok = None;
        if self.italic {
            s = s.add_modifier(Modifier::ITALIC);
            tok = Some(Token::Dim);
        }
        if self.bold {
            s = s.add_modifier(Modifier::BOLD);
            tok = Some(Token::OrcaWhite);
        }
        if self.link.is_some() {
            s = s.add_modifier(Modifier::UNDERLINED);
            tok = Some(Token::Accent);
        }
        if self.quote > 0 && tok.is_none() {
            s = s.add_modifier(Modifier::ITALIC);
            tok = Some(Token::Dim);
        }
        if self.rgb {
            if let Some(t) = tok {
                s = s.fg(color::rt(t));
            }
        }
        s
    }

    fn inline_code(&self) -> Style {
        if self.rgb {
            Style::default().fg(color::rt(Token::Accent)).bg(INLINE_BG)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.text(&t),
            Event::Code(t) => self.code_inline(&t),
            Event::SoftBreak | Event::HardBreak => self.soft_break(),
            Event::Rule => self.rule(),
            _ => {} // html, footnotes, task markers, images: ignored
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => self.heading = Some(level),
            Tag::Paragraph => {}
            Tag::CodeBlock(kind) => {
                self.in_code = true;
                self.code_lang = match kind {
                    CodeBlockKind::Fenced(s) => s.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code_buf.clear();
            }
            Tag::BlockQuote(_) => self.quote += 1,
            Tag::List(start) => {
                // Flush the parent item's inline text before descending, so a tight
                // nested list doesn't merge "a"/"b"/"c" into one deepest-level line.
                if !self.lists.is_empty() && !self.runs.is_empty() {
                    self.emit_para();
                }
                self.lists.push(ListCtx { ordered: start });
            }
            Tag::Item => {
                let depth = self.lists.len().saturating_sub(1);
                let indent = "  ".repeat(depth);
                let nested = depth > 0;
                let mark_tok = if nested { Token::Dim } else { Token::Accent };
                let marker = match self.lists.last_mut() {
                    Some(ctx) => match ctx.ordered {
                        Some(ref mut n) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        None => "· ".to_string(),
                    },
                    None => "· ".to_string(),
                };
                self.pending_marker = Some((format!("{indent}{marker}"), self.fg(mark_tok)));
            }
            Tag::Emphasis => self.italic = true,
            Tag::Strong => self.bold = true,
            Tag::Strikethrough => {} // rendered as plain text
            Tag::Link { dest_url, .. } => self.link = Some(dest_url.to_string()),
            Tag::Table(_) => {
                self.table = Some(Table {
                    head: Vec::new(),
                    rows: Vec::new(),
                });
            }
            Tag::TableHead => self.in_head = true,
            Tag::TableRow => self.cur_row.clear(),
            Tag::TableCell => self.cur_cell.clear(),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.emit_heading();
                self.heading = None;
            }
            TagEnd::Paragraph => self.emit_para(),
            TagEnd::CodeBlock => {
                self.emit_code();
                self.in_code = false;
            }
            TagEnd::BlockQuote(_) => {
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            TagEnd::Item => {
                // Tight items carry inline text directly (no Paragraph) — flush it.
                if !self.runs.is_empty() {
                    self.emit_para();
                } else if self.pending_marker.is_some() {
                    // Empty item: still show the marker line.
                    self.emit_para();
                }
            }
            TagEnd::Emphasis => self.italic = false,
            TagEnd::Strong => self.bold = false,
            TagEnd::Link => {
                if let Some(url) = self.link.take() {
                    self.runs.push((format!(" ({url})"), self.fg(Token::Dim)));
                }
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.cur_cell);
                self.cur_row.push(cell);
            }
            TagEnd::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.head = std::mem::take(&mut self.cur_row);
                }
                self.in_head = false;
            }
            TagEnd::TableRow if !self.in_head => {
                if let Some(t) = self.table.as_mut() {
                    t.rows.push(std::mem::take(&mut self.cur_row));
                }
            }
            TagEnd::Table => self.emit_table(),
            _ => {}
        }
    }

    fn text(&mut self, t: &str) {
        if self.in_code {
            self.code_buf.push_str(t);
        } else if self.table.is_some() {
            self.cur_cell.push_str(t);
        } else {
            let st = self.inline();
            self.runs.push((t.to_string(), st));
        }
    }

    fn code_inline(&mut self, t: &str) {
        if self.table.is_some() {
            self.cur_cell.push_str(t);
        } else {
            let st = self.inline_code();
            self.runs.push((t.to_string(), st));
        }
    }

    fn soft_break(&mut self) {
        if self.in_code {
            self.code_buf.push('\n');
        } else if self.table.is_some() {
            self.cur_cell.push(' ');
        } else {
            self.runs.push((" ".to_string(), Style::default()));
        }
    }

    // ---- block emitters ----

    fn emit_heading(&mut self) {
        let (glyph, tok) = match self.heading {
            Some(HeadingLevel::H1) => ("█ ", Token::Brand),
            Some(HeadingLevel::H2) => ("▓ ", Token::Accent),
            _ => ("▒ ", Token::Dim),
        };
        let runs = std::mem::take(&mut self.runs);
        let first = [(glyph.to_string(), self.fg(tok))];
        let cont = [("  ".to_string(), Style::default())];
        self.blank();
        self.push_wrapped(runs, &first, &cont);
        self.blank();
    }

    fn emit_para(&mut self) {
        let runs = std::mem::take(&mut self.runs);
        if runs.is_empty() && self.pending_marker.is_none() {
            return;
        }
        // Compose the prefix: quote bars (one per nesting level) + an optional list
        // marker. The continuation prefix keeps the quote bars and pads the marker
        // width so wrapped text stays aligned under the first line's text.
        let quote_pfx: Vec<(String, Style)> = if self.quote > 0 {
            vec![("▌ ".repeat(self.quote), self.fg(Token::Dim))]
        } else {
            Vec::new()
        };
        let (first, cont) = if let Some(m) = self.pending_marker.take() {
            let cont_w = disp_width(&m.0);
            let mut f = quote_pfx.clone();
            f.push(m);
            let mut c = quote_pfx.clone();
            c.push((" ".repeat(cont_w), Style::default()));
            (f, c)
        } else {
            (quote_pfx.clone(), quote_pfx)
        };
        self.push_wrapped(runs, &first, &cont);
        if self.quote > 0 || self.lists.is_empty() {
            // top-level paragraphs get trailing space; list items stay tight
        }
    }

    fn emit_code(&mut self) {
        let bar = self.fg(Token::Brand);
        let body = self.code_body_style();
        // Inside a blockquote, prefix each code line with the quote bar(s).
        let qbar = "▌ ".repeat(self.quote);
        let qstyle = self.fg(Token::Dim);
        let qw = disp_width(&qbar);
        // Code content width: gutter "▏ " (2) for the first line, "▏ ↳ " (4) for
        // continuations, plus the quote bar(s).
        let first_avail = self.width.saturating_sub(2 + qw);
        let cont_avail = self.width.saturating_sub(4 + qw);
        self.blank();
        if !self.code_lang.is_empty() {
            let mut spans = Vec::new();
            let mut plain = String::new();
            if !qbar.is_empty() {
                spans.push((qbar.clone(), qstyle));
                plain.push_str(&qbar);
            }
            spans.push(("▏ ".to_string(), bar));
            spans.push((self.code_lang.clone(), self.fg(Token::Dim)));
            plain.push_str("▏ ");
            plain.push_str(&self.code_lang);
            self.out.push(RenderedLine { spans, plain });
        }
        let buf = std::mem::take(&mut self.code_buf);
        for line in buf.trim_end_matches('\n').split('\n') {
            for (i, seg) in wrap_code_line(line, first_avail, cont_avail)
                .into_iter()
                .enumerate()
            {
                let gutter = if i == 0 { "▏ " } else { "▏ ↳ " };
                let mut spans = Vec::new();
                let mut plain = String::new();
                if !qbar.is_empty() {
                    spans.push((qbar.clone(), qstyle));
                    plain.push_str(&qbar);
                }
                spans.push((gutter.to_string(), bar));
                plain.push_str(gutter);
                if !seg.is_empty() {
                    spans.push((seg.clone(), body));
                    plain.push_str(&seg);
                }
                self.out.push(RenderedLine { spans, plain });
            }
        }
        self.blank();
    }

    fn code_body_style(&self) -> Style {
        if self.rgb {
            Style::default().fg(color::rt(Token::Fg)).bg(CODE_BG)
        } else {
            Style::default()
        }
    }

    fn rule(&mut self) {
        let n = self.width.min(60);
        self.blank();
        self.out.push(RenderedLine {
            spans: vec![("─".repeat(n), self.fg(Token::Dim))],
            plain: "─".repeat(n),
        });
        self.blank();
    }

    fn emit_table(&mut self) {
        let Some(t) = self.table.take() else {
            return;
        };
        for line in render_table(&t, self.width, self.rgb) {
            self.out.push(line);
        }
        self.blank();
    }

    /// Wrap `runs` to `width`, prefixing the first line with `first` and the
    /// rest with `cont`, and push the resulting [`RenderedLine`]s.
    fn push_wrapped(
        &mut self,
        runs: Vec<(String, Style)>,
        first: &[(String, Style)],
        cont: &[(String, Style)],
    ) {
        for line in wrap_runs(&runs, self.width, first, cont) {
            self.out.push(line);
        }
    }

    fn blank(&mut self) {
        // Collapse consecutive blank lines.
        if matches!(self.out.last(), Some(l) if l.plain.is_empty()) {
            return;
        }
        self.out.push(RenderedLine {
            spans: Vec::new(),
            plain: String::new(),
        });
    }

    fn finish(mut self) -> Vec<RenderedLine> {
        // Trim leading/trailing blank lines.
        while matches!(self.out.first(), Some(l) if l.plain.is_empty()) {
            self.out.remove(0);
        }
        while matches!(self.out.last(), Some(l) if l.plain.is_empty()) {
            self.out.pop();
        }
        if self.out.is_empty() {
            self.out.push(RenderedLine {
                spans: vec![("(空)".to_string(), Style::default())],
                plain: "(空)".to_string(),
            });
        }
        self.out
    }
}

/// Coalesce a char/style run into `(String, Style)` spans.
fn coalesce(chs: &[(char, Style)]) -> Vec<(String, Style)> {
    let mut out: Vec<(String, Style)> = Vec::new();
    for (c, st) in chs {
        if let Some((s, last)) = out.last_mut() {
            if *last == *st {
                s.push(*c);
                continue;
            }
        }
        out.push((c.to_string(), *st));
    }
    out
}

fn char_w(c: char) -> usize {
    disp_width(&c.to_string())
}

/// Hard-wrap one code line, never splitting a char (CJK=2). The first segment
/// fits `first_avail` display columns, every continuation fits `cont_avail`
/// (the gutters differ: `▏ ` for the first line, `▏ ↳ ` for continuations).
/// Always returns at least one (possibly empty) segment.
fn wrap_code_line(line: &str, first_avail: usize, cont_avail: usize) -> Vec<String> {
    let first_avail = first_avail.max(1);
    let cont_avail = cont_avail.max(1);
    let mut segs: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut w = 0usize;
    for c in line.chars() {
        let cap = if segs.is_empty() {
            first_avail
        } else {
            cont_avail
        };
        let cw = char_w(c);
        if w + cw > cap && !cur.is_empty() {
            segs.push(std::mem::take(&mut cur));
            w = 0;
        }
        cur.push(c);
        w += cw;
    }
    segs.push(cur); // always emit at least one segment (possibly empty)
    segs
}

/// Word-wrap styled `runs` to `width` columns, prefixing line 0 with `first` and
/// the rest with `cont`. Breaks at the last space when possible, else hard-breaks
/// (CJK has no spaces → per-char). Never splits a char.
fn wrap_runs(
    runs: &[(String, Style)],
    width: usize,
    first: &[(String, Style)],
    cont: &[(String, Style)],
) -> Vec<RenderedLine> {
    let first_w: usize = first.iter().map(|(s, _)| disp_width(s)).sum();
    let avail = width.saturating_sub(first_w).max(1);
    let mut chars: Vec<(char, Style)> = Vec::new();
    for (t, st) in runs {
        for c in t.chars() {
            chars.push((c, *st));
        }
    }

    let mut lines: Vec<Vec<(char, Style)>> = Vec::new();
    let mut line: Vec<(char, Style)> = Vec::new();
    let mut w = 0usize;
    let mut last_space: Option<usize> = None;
    for (c, st) in chars {
        let cw = char_w(c);
        if w + cw > avail && !line.is_empty() {
            if let Some(sp) = last_space {
                let tail = line.split_off(sp + 1);
                line.pop(); // drop the breaking space
                lines.push(std::mem::take(&mut line));
                line = tail;
                w = line.iter().map(|(c, _)| char_w(*c)).sum();
            } else {
                lines.push(std::mem::take(&mut line));
                w = 0;
            }
            last_space = None;
        }
        if c == ' ' {
            last_space = Some(line.len());
        }
        line.push((c, st));
        w += cw;
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }

    lines
        .into_iter()
        .enumerate()
        .map(|(i, chs)| {
            let pfx = if i == 0 { first } else { cont };
            let mut spans: Vec<(String, Style)> =
                pfx.iter().filter(|(s, _)| !s.is_empty()).cloned().collect();
            spans.extend(coalesce(&chs));
            let plain: String = spans.iter().map(|(s, _)| s.as_str()).collect();
            RenderedLine { spans, plain }
        })
        .collect()
}

/// Render `s` into exactly `w` display columns (CJK=2): truncate with a trailing
/// '…' when too wide, else right-pad with spaces. Always returns exactly `w`
/// columns (including after '…', and when padding a width-2 CJK char that can't
/// fit an odd slot). Never splits a char.
fn fit_cell(s: &str, w: usize) -> String {
    let s = s.trim();
    if w == 0 {
        return String::new();
    }
    let sw = disp_width(s);
    if sw <= w {
        return format!("{s}{}", " ".repeat(w - sw));
    }
    // Too wide: keep w-1 columns + '…' (… is 1 column).
    let kept = crate::header::truncate_to_width(s, w - 1);
    let mut out = format!("{kept}…");
    let ow = disp_width(&out);
    if ow < w {
        out.push_str(&" ".repeat(w - ow)); // odd-width CJK slot: pad to w
    }
    out
}

/// Render a parsed table to box-drawn lines. Columns are sized by display width
/// (CJK = 2); if the total exceeds `width`, the rightmost columns are dropped and
/// a `╌` marker is appended to flag the truncation.
fn render_table(t: &Table, width: usize, rgb: bool) -> Vec<RenderedLine> {
    let frame = if rgb {
        Style::default().fg(color::rt(Token::Dim))
    } else {
        Style::default()
    };
    let head = if rgb {
        Style::default()
            .fg(color::rt(Token::OrcaWhite))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let cell = if rgb {
        Style::default().fg(color::rt(Token::Fg))
    } else {
        Style::default()
    };

    let ncol = t
        .head
        .len()
        .max(t.rows.iter().map(|r| r.len()).max().unwrap_or(0));
    if ncol == 0 {
        return Vec::new();
    }
    // Column display widths from header + rows.
    let mut cw = vec![0usize; ncol];
    let mut consider = |row: &[String]| {
        for (i, c) in row.iter().enumerate() {
            cw[i] = cw[i].max(disp_width(c.trim()));
        }
    };
    consider(&t.head);
    for r in &t.rows {
        consider(r);
    }
    // Fit columns into `width`. Each column costs `cw[i]+3` ("│ cell "), plus 1
    // for the closing "│", so the budget for the sum of column widths is
    // `width − 3·ncol − 1`. Prefer shrinking the widest column (water-fill) so all
    // columns stay visible; only drop right columns when even 1-wide each won't fit.
    let budget = width.saturating_sub(3 * ncol + 1);
    let sum: usize = cw.iter().sum();
    let (cols_owned, truncated): (Vec<usize>, bool) = if sum <= budget {
        (cw.clone(), false)
    } else if budget >= ncol {
        // Water-fill: shrink the widest column (lowest index on ties) until it fits.
        let mut c = cw.clone();
        let mut s = sum;
        while s > budget {
            let mut mi = 0;
            for i in 1..c.len() {
                if c[i] > c[mi] {
                    mi = i;
                }
            }
            if c[mi] <= 1 {
                break; // all at floor (shouldn't happen since budget >= ncol)
            }
            c[mi] -= 1;
            s -= 1;
        }
        (c, false)
    } else {
        // Too many columns for the width: drop right columns (legacy behavior).
        let mut keep = 0usize;
        let mut used = 1usize; // closing │
        for &c in &cw {
            let need = c + 3;
            if used + need <= width || keep == 0 {
                used += need;
                keep += 1;
            } else {
                break;
            }
        }
        (cw[..keep].to_vec(), keep < ncol)
    };
    let cols: &[usize] = &cols_owned;

    let mut out = Vec::new();
    let border = |left: char, mid: char, right: char| -> RenderedLine {
        let mut s = String::new();
        s.push(left);
        for (i, &c) in cols.iter().enumerate() {
            s.push_str(&"─".repeat(c + 2));
            s.push(if i + 1 < cols.len() { mid } else { right });
        }
        if truncated {
            s.push('╌');
        }
        RenderedLine {
            plain: s.clone(),
            spans: vec![(s, frame)],
        }
    };
    let data_row = |cells: &[String], style: Style| -> RenderedLine {
        let mut spans: Vec<(String, Style)> = Vec::new();
        for (i, &c) in cols.iter().enumerate() {
            spans.push(("│".to_string(), frame));
            let val = cells.get(i).map(|s| s.as_str()).unwrap_or("");
            spans.push((format!(" {} ", fit_cell(val, c)), style));
        }
        spans.push(("│".to_string(), frame));
        if truncated {
            spans.push((
                "╌".to_string(),
                if rgb {
                    Style::default().fg(color::rt(Token::Accent))
                } else {
                    Style::default()
                },
            ));
        }
        let plain: String = spans.iter().map(|(s, _)| s.as_str()).collect();
        RenderedLine { spans, plain }
    };

    out.push(border('┌', '┬', '┐'));
    out.push(data_row(&t.head, head));
    out.push(border('├', '┼', '┤'));
    for r in &t.rows {
        out.push(data_row(r, cell));
    }
    out.push(border('└', '┴', '┘'));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plains(src: &str, width: u16) -> Vec<String> {
        render(src, width, false, false)
            .into_iter()
            .map(|l| l.plain)
            .collect()
    }

    #[test]
    fn heading_gets_density_block_prefix() {
        let p = plains("# Title\n\n## Sub\n\n### Deep", 80);
        assert!(p.iter().any(|l| l == "█ Title"));
        assert!(p.iter().any(|l| l == "▓ Sub"));
        assert!(p.iter().any(|l| l == "▒ Deep"));
    }

    #[test]
    fn paragraph_wraps_to_width_and_keeps_words() {
        let p = plains("alpha beta gamma delta epsilon", 14);
        // Each wrapped line fits the width and no word is split.
        for l in &p {
            assert!(disp_width(l) <= 14, "line too wide: {l:?}");
        }
        assert!(p.len() >= 2);
        let joined = p.join(" ");
        assert!(joined.contains("alpha") && joined.contains("epsilon"));
    }

    #[test]
    fn code_block_has_bar_and_lang() {
        let p = plains("```rust\nlet x = 1;\n```", 80);
        assert!(p.iter().any(|l| l == "▏ rust"));
        assert!(p.iter().any(|l| l == "▏ let x = 1;"));
    }

    #[test]
    fn bullets_and_inline_code_in_plain() {
        let p = plains("- one\n- two `code`", 80);
        assert!(p.iter().any(|l| l.contains("· one")));
        assert!(p.iter().any(|l| l.contains("· two code")));
    }

    #[test]
    fn link_shows_text_then_url() {
        let p = plains("see [docs](https://x.io)", 80);
        let joined = p.join("\n");
        assert!(joined.contains("docs"));
        assert!(joined.contains("(https://x.io)"));
    }

    #[test]
    fn table_aligns_columns_and_keeps_cjk_width() {
        let src = "| 字段 | 类型 |\n| --- | --- |\n| risk | Enum |";
        let p = plains(src, 80);
        // A border row and the header are present; every line has consistent width.
        assert!(p.iter().any(|l| l.starts_with('┌')));
        assert!(p.iter().any(|l| l.contains("字段") && l.contains("类型")));
        let widths: Vec<usize> = p
            .iter()
            .filter(|l| {
                l.starts_with('┌') || l.starts_with('│') || l.starts_with('├') || l.starts_with('└')
            })
            .map(|l| disp_width(l))
            .collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "table rows misaligned: {widths:?}"
        );
    }

    #[test]
    fn never_panics_on_tiny_width() {
        let _ = render(
            "# Hi\n\n| a | b |\n|-|-|\n| 1 | 2 |\n\n```\nx\n```",
            4,
            true,
            false,
        );
        let _ = render("", 0, false, false);
    }

    #[test]
    fn code_block_soft_wraps_with_continuation_marker() {
        // A long code line wraps at a narrow width.
        let p = plains("```\nabcdefghij klmnopqrst\n```", 12);
        // At least one continuation line starts with "▏ ↳ ".
        assert!(
            p.iter().any(|l| l.starts_with("▏ ↳ ")),
            "no continuation: {p:?}"
        );
        // No code line exceeds the width.
        assert!(p
            .iter()
            .filter(|l| l.contains('▏'))
            .all(|l| disp_width(l) <= 12));
    }

    #[test]
    fn code_block_inside_blockquote_keeps_quote_bar() {
        let p = plains("> ```\n> let x = 1;\n> ```", 80);
        assert!(p
            .iter()
            .any(|l| l.contains('▌') && l.contains('▏') && l.contains("let x = 1;")));
    }

    #[test]
    fn list_item_inside_blockquote_keeps_quote_bar() {
        let p = plains("> - item one\n> - item two", 80);
        // Each list-item line carries both the quote bar ▌ and the bullet marker ·.
        assert!(p
            .iter()
            .any(|l| l.contains('▌') && l.contains('·') && l.contains("item one")));
    }

    #[test]
    fn table_shrinks_widest_column_keeping_all_columns() {
        // One very wide column: the old logic drops columns; the new logic shrinks
        // it and keeps both columns.
        let src =
            "| k | v |\n| - | - |\n| a | this_is_a_very_long_value_that_would_blow_the_table |";
        let p = plains(src, 30);
        // Both header columns present.
        assert!(p.iter().any(|l| l.contains('k') && l.contains('v')));
        // All border/data rows are equal width.
        let widths: Vec<usize> = p
            .iter()
            .filter(|l| {
                l.starts_with('┌') || l.starts_with('│') || l.starts_with('├') || l.starts_with('└')
            })
            .map(|l| disp_width(l))
            .collect();
        assert!(!widths.is_empty());
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "rows misaligned: {widths:?}"
        );
        // The over-wide cell is ellipsis-truncated.
        assert!(p.iter().any(|l| l.contains('…')));
        // No column-drop marker (both columns fit after shrinking).
        assert!(!p.iter().any(|l| l.contains('╌')));
    }

    #[test]
    fn deeply_nested_lists_indent_progressively() {
        let p = plains("- a\n  - b\n    - c", 80);
        // Each level renders as "· a" / "  · b" / "    · c"; locate by marker+body
        // to avoid fragile single-char matches.
        let lead = |needle: &str| -> usize {
            let l = p.iter().find(|l| l.contains(needle)).expect(needle);
            l.len() - l.trim_start_matches(' ').len()
        };
        assert!(lead("· b") > lead("· a"));
        assert!(lead("· c") > lead("· b"));
    }

    #[test]
    fn fit_cell_pads_truncates_and_keeps_exact_width() {
        use super::fit_cell;
        // fits → right-pad to w
        assert_eq!(fit_cell("ab", 4), "ab  ");
        // exact → unchanged
        assert_eq!(fit_cell("abcd", 4), "abcd");
        // too wide → truncate to w-1 + … (disp_width exactly w)
        let f = fit_cell("abcdef", 4);
        assert_eq!(disp_width(&f), 4);
        assert!(f.ends_with('…'));
        // w == 0 → empty
        assert_eq!(fit_cell("x", 0), "");
        // full-width CJK in an odd slot: can't fit half a glyph → … + pad, exactly w
        let g = fit_cell("中文", 3);
        assert_eq!(disp_width(&g), 3);
        // w == 1 and too wide → a lone …
        assert_eq!(fit_cell("中", 1), "…");
    }
}
