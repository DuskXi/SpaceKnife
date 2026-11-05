//! The parts of Python's `rich` the command line printed through, ported so the text is the same
//! byte for byte (scripts and the external benchmark parse it): word wrapping and folding
//! (`rich._wrap.divide_line`, `cells.chop_cells`), `Text.truncate` with an ellipsis, `Panel`
//! (expanded, a title on the left), `Table` without a box (column measuring, collapsing a table
//! wider than the console, `ratio_reduce`), and `Console`'s width rules and line cropping.
//!
//! Styles become ANSI SGR sequences only when the console is a terminal (as rich decides the
//! colour system); piped output is plain text.

use std::io::IsTerminal as _;

use unicode_width::UnicodeWidthChar as _;

// ------------------------------------------------------------------------------------- styles

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Rgb(u8, u8, u8),
    /// one of the 16 standard colours (0–15)
    Ansi(u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub dim: bool,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
}

impl Style {
    pub const NONE: Style = Style { bold: false, italic: false, dim: false, fg: None, bg: None };
    pub const BOLD: Style = Style { bold: true, ..Style::NONE };
    pub const ITALIC: Style = Style { italic: true, ..Style::NONE };
    pub const DIM: Style = Style { dim: true, ..Style::NONE };
    pub const GREEN: Style = Style::ansi(2);
    pub const YELLOW: Style = Style::ansi(3);
    pub const RED: Style = Style::ansi(1);
    pub const CYAN: Style = Style::ansi(6);

    pub const fn rgb(hex: u32) -> Style {
        Style { fg: Some(Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)), ..Style::NONE }
    }
    pub const fn ansi(n: u8) -> Style {
        Style { fg: Some(Color::Ansi(n)), ..Style::NONE }
    }
    pub const fn bold(self) -> Style {
        Style { bold: true, ..self }
    }

    /// `self` with `over` on top (rich's `Style.__add__`).
    pub fn with(self, over: Style) -> Style {
        Style {
            bold: self.bold || over.bold,
            italic: self.italic || over.italic,
            dim: self.dim || over.dim,
            fg: over.fg.or(self.fg),
            bg: over.bg.or(self.bg),
        }
    }

    fn sgr(&self) -> String {
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".into());
        }
        if self.dim {
            codes.push("2".into());
        }
        if self.italic {
            codes.push("3".into());
        }
        let color = |c: Color, base: u8| match c {
            Color::Ansi(n) if n < 8 => format!("{}", base + n),
            Color::Ansi(n) => format!("{}", base + 60 + (n - 8)),
            Color::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", base + 8),
        };
        if let Some(c) = self.fg {
            codes.push(color(c, 30));
        }
        if let Some(c) = self.bg {
            codes.push(color(c, 40));
        }
        codes.join(";")
    }
}

/// Design tokens the CLI used (text.tertiary, warning, accent, surfaces).
pub const MUTED: Style = Style::rgb(0x8F95A2);
pub const WARN: Style = Style::rgb(0xF5C542);
pub const ACCENT: Style = Style::rgb(0x8B9CFF);
pub const BAR_EMPTY: Style = Style::rgb(0x2A2E37);
pub const BORDER: Style = Style::rgb(0x2A2E37);

// ------------------------------------------------------------------------------------- cells

/// Cells a character takes (rich's `get_character_cell_size`: control characters none).
pub fn char_width(c: char) -> usize {
    let cp = c as u32;
    if (cp != 0 && cp < 32) || (0x7F..0xA0).contains(&cp) {
        return 0;
    }
    c.width().unwrap_or(0)
}

pub fn cell_len(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// One styled character.
pub type Cell = (char, Style);

/// Styled text (may hold `\n`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Text(pub Vec<Cell>);

impl Text {
    pub fn new() -> Text {
        Text(Vec::new())
    }
    pub fn plain(s: &str) -> Text {
        Text::styled(s, Style::NONE)
    }
    pub fn styled(s: &str, style: Style) -> Text {
        Text(s.chars().map(|c| (c, style)).collect())
    }
    pub fn push(&mut self, s: &str, style: Style) -> &mut Self {
        self.0.extend(s.chars().map(|c| (c, style)));
        self
    }
    pub fn append(&mut self, t: Text) -> &mut Self {
        self.0.extend(t.0);
        self
    }
    pub fn with(mut self, s: &str, style: Style) -> Text {
        self.push(s, style);
        self
    }
    /// Every character's style with `base` underneath.
    pub fn under(mut self, base: Style) -> Text {
        for c in &mut self.0 {
            c.1 = base.with(c.1);
        }
        self
    }
    pub fn as_plain(&self) -> String {
        self.0.iter().map(|c| c.0).collect()
    }
    pub fn cell_len(&self) -> usize {
        cells_of(&self.0)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Lines at `\n` (rich's `split("\n", allow_blank=True)`).
    fn lines(&self) -> Vec<Vec<Cell>> {
        let mut out = vec![Vec::new()];
        for &c in &self.0 {
            if c.0 == '\n' {
                out.push(Vec::new());
            } else {
                out.last_mut().unwrap().push(c);
            }
        }
        out
    }
}

/// `tr`-style template filling with styled fields: `{name}` replaced by the field's text. The
/// template (already translated) is plain; unknown placeholders stay as they are.
pub fn fill(template: &str, base: Style, fields: &[(&str, Text)]) -> Text {
    let mut out = Text::new();
    let mut rest = template;
    while let Some(pos) = rest.find('{') {
        out.push(&rest[..pos], base);
        let tail = &rest[pos..];
        let Some(end) = tail.find('}') else {
            out.push(tail, base);
            return out;
        };
        let name = &tail[1..end];
        match fields.iter().find(|(k, _)| *k == name) {
            Some((_, t)) => {
                out.append(t.clone().under(base));
            }
            None => {
                out.push(&tail[..=end], base);
            }
        }
        rest = &tail[end + 1..];
    }
    out.push(rest, base);
    out
}

fn cells_of(cells: &[Cell]) -> usize {
    cells.iter().map(|c| char_width(c.0)).sum()
}

fn is_space(c: char) -> bool {
    c.is_whitespace()
}

/// Words as `rich._wrap.words` finds them (`\s*\S+\s*`): (start, end) char offsets.
fn words(chars: &[Cell]) -> Vec<(usize, usize)> {
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        let start = i;
        let mut j = i;
        while j < n && is_space(chars[j].0) {
            j += 1;
        }
        if j == n {
            break;
        }
        while j < n && !is_space(chars[j].0) {
            j += 1;
        }
        while j < n && is_space(chars[j].0) {
            j += 1;
        }
        out.push((start, j));
        i = j;
    }
    out
}

/// `cells.chop_cells`: pieces of at most `width` cells (as char counts of the pieces).
fn chop_cells(chars: &[Cell], width: usize) -> Vec<usize> {
    let mut lines = Vec::new();
    let mut size = 0;
    let mut offset = 0;
    for (i, c) in chars.iter().enumerate() {
        let w = char_width(c.0);
        if size + w > width {
            lines.push(i - offset);
            offset = i;
            size = 0;
        }
        size += w;
    }
    if size > 0 {
        lines.push(chars.len() - offset);
    }
    lines
}

/// `rich._wrap.divide_line`: char offsets to break a line at.
fn divide_line(chars: &[Cell], width: usize, fold: bool) -> Vec<usize> {
    let mut breaks = Vec::new();
    let mut cell_offset: usize = 0;
    for (start, end) in words(chars) {
        let word = &chars[start..end];
        let mut trimmed = word.len();
        while trimmed > 0 && is_space(word[trimmed - 1].0) {
            trimmed -= 1;
        }
        let word_length = cells_of(&word[..trimmed]);
        let remaining = width as isize - cell_offset as isize;
        if remaining >= word_length as isize {
            cell_offset += cells_of(word);
        } else if word_length > width {
            if fold {
                let pieces = chop_cells(word, width);
                let mut start = start;
                let mut at = 0;
                for (k, len) in pieces.iter().enumerate() {
                    if start > 0 {
                        breaks.push(start);
                    }
                    if k + 1 == pieces.len() {
                        cell_offset = cells_of(&word[at..at + len]);
                    } else {
                        start += len;
                    }
                    at += len;
                }
            } else {
                if start > 0 {
                    breaks.push(start);
                }
                cell_offset = cells_of(word);
            }
        } else if cell_offset > 0 && start > 0 {
            breaks.push(start);
            cell_offset = cells_of(word);
        }
    }
    breaks
}

/// `cells.set_cell_size`: crop (a wide character cut in half becomes a space) or pad with spaces.
fn set_cell_size(line: &mut Vec<Cell>, total: usize) {
    let size = cells_of(line);
    if size == total {
        return;
    }
    if size < total {
        let style = Style::NONE;
        line.extend(std::iter::repeat_n((' ', style), total - size));
        return;
    }
    let mut used = 0;
    let mut keep = 0;
    for (i, c) in line.iter().enumerate() {
        let w = char_width(c.0);
        if used + w > total {
            keep = i;
            break;
        }
        used += w;
        keep = i + 1;
    }
    let style = line.get(keep).map_or(Style::NONE, |c| c.1);
    line.truncate(keep);
    if used < total {
        line.push((' ', style));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overflow {
    Fold,
    Ellipsis,
    Crop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Justify {
    Default,
    Left,
    Right,
}

/// `Text.truncate`.
fn truncate(line: &mut Vec<Cell>, max_width: usize, overflow: Overflow, pad: bool) {
    let length = cells_of(line);
    if length > max_width {
        if overflow == Overflow::Ellipsis {
            let style = line.last().map_or(Style::NONE, |c| c.1);
            set_cell_size(line, max_width.saturating_sub(1));
            line.push(('…', style));
        } else {
            set_cell_size(line, max_width);
        }
    }
    if pad && length < max_width {
        line.extend(std::iter::repeat_n((' ', Style::NONE), max_width - length));
    }
}

fn rstrip(line: &mut Vec<Cell>) {
    while line.last().is_some_and(|c| is_space(c.0)) {
        line.pop();
    }
}

/// `Text.rstrip_end`: trailing white space past `size` characters.
fn rstrip_end(line: &mut Vec<Cell>, size: usize) {
    if line.len() > size {
        let excess = line.len() - size;
        let ws = line.iter().rev().take_while(|c| is_space(c.0)).count();
        let cut = ws.min(excess);
        line.truncate(line.len() - cut);
    }
}

/// `Text.wrap` (then `Lines.justify` and the final truncation).
pub fn wrap(text: &Text, width: usize, justify: Justify, overflow: Overflow, no_wrap: bool) -> Vec<Vec<Cell>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut new_lines: Vec<Vec<Cell>> = if no_wrap {
            vec![line]
        } else {
            let offsets = divide_line(&line, width, overflow == Overflow::Fold);
            let mut bounds = vec![0];
            bounds.extend(offsets);
            bounds.push(line.len());
            let mut pieces: Vec<Vec<Cell>> = bounds.windows(2).map(|w| line[w[0]..w[1]].to_vec()).collect();
            for p in &mut pieces {
                rstrip_end(p, width);
            }
            pieces
        };
        match justify {
            Justify::Default => {}
            Justify::Left => {
                for l in &mut new_lines {
                    truncate(l, width, overflow, true);
                }
            }
            Justify::Right => {
                for l in &mut new_lines {
                    rstrip(l);
                    truncate(l, width, overflow, false);
                    let len = cells_of(l);
                    if len < width {
                        let mut padded: Vec<Cell> = std::iter::repeat_n((' ', Style::NONE), width - len).collect();
                        padded.append(l);
                        *l = padded;
                    }
                }
            }
        }
        for l in &mut new_lines {
            truncate(l, width, overflow, false);
        }
        out.extend(new_lines);
    }
    out
}

/// A rendered line padded or cropped to exactly `width` cells (`Segment.set_shape`).
fn shape(mut line: Vec<Cell>, width: usize) -> Vec<Cell> {
    set_cell_size(&mut line, width);
    line
}

// ------------------------------------------------------------------------------------- measuring

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Measure {
    min: usize,
    max: usize,
}

impl Measure {
    fn normalize(self) -> Measure {
        let min = self.min.min(self.max);
        Measure { min, max: self.max.max(min) }
    }
    fn with_maximum(self, w: usize) -> Measure {
        Measure { min: self.min.min(w), max: self.max.min(w) }
    }
    fn with_minimum(self, w: usize) -> Measure {
        Measure { min: self.min.max(w), max: self.max.max(w) }
    }
    fn clamp(self, min: Option<usize>, max: Option<usize>) -> Measure {
        let mut m = self;
        if let Some(w) = min {
            m = m.with_minimum(w);
        }
        if let Some(w) = max {
            m = m.with_maximum(w);
        }
        m
    }
}

/// `Measurement.get` of a Text inside a `Padding` of `extra` cells.
fn measure_padded(text: &Text, max_width: usize, extra: usize) -> Measure {
    if max_width < 1 {
        return Measure { min: 0, max: 0 };
    }
    let inner = if max_width < extra + 1 {
        Measure { min: max_width, max: max_width }
    } else {
        let plain = text.as_plain();
        let lines: Vec<&str> = plain.lines().collect();
        let max = lines.iter().map(|l| cell_len(l)).max().unwrap_or(0);
        let min = plain.split_whitespace().map(cell_len).max().unwrap_or(max);
        let mut m = Measure { min, max }.normalize().with_maximum(max_width);
        if m.max < 1 {
            m = Measure { min: 0, max: 0 };
        }
        let m = m.normalize();
        Measure { min: m.min + extra, max: m.max + extra }.with_maximum(max_width)
    };
    let m = inner.normalize().with_maximum(max_width);
    if m.max < 1 { Measure { min: 0, max: 0 } } else { m.normalize() }
}

/// `_ratio.ratio_reduce` (Python's `round` rounds half to even).
fn ratio_reduce(total: usize, ratios: &[usize], maximums: &[usize], values: &[usize]) -> Vec<usize> {
    let ratios: Vec<usize> = ratios.iter().zip(maximums).map(|(&r, &m)| if m > 0 { r } else { 0 }).collect();
    let mut total_ratio: usize = ratios.iter().sum();
    if total_ratio == 0 {
        return values.to_vec();
    }
    let mut remaining = total as i64;
    let mut out = Vec::with_capacity(values.len());
    for ((&ratio, &maximum), &value) in ratios.iter().zip(maximums).zip(values) {
        if ratio > 0 && total_ratio > 0 {
            let share = (ratio as f64 * remaining as f64 / total_ratio as f64).round_ties_even() as i64;
            let distributed = (maximum as i64).min(share);
            out.push((value as i64 - distributed).max(0) as usize);
            remaining -= distributed;
            total_ratio -= ratio;
        } else {
            out.push(value);
        }
    }
    out
}

// ------------------------------------------------------------------------------------- table

#[derive(Clone, Debug)]
pub struct Column {
    pub header: Text,
    pub justify: Justify,
    pub style: Style,
    pub overflow: Overflow,
    pub no_wrap: bool,
    pub min_width: Option<usize>,
    pub max_width: Option<usize>,
}

impl Column {
    pub fn new(header: &str) -> Column {
        Column {
            header: Text::plain(header),
            justify: Justify::Left,
            style: Style::NONE,
            overflow: Overflow::Ellipsis,
            no_wrap: false,
            min_width: None,
            max_width: None,
        }
    }
    pub fn right(mut self) -> Self {
        self.justify = Justify::Right;
        self
    }
    pub fn style(mut self, s: Style) -> Self {
        self.style = s;
        self
    }
    pub fn no_wrap(mut self) -> Self {
        self.no_wrap = true;
        self
    }
    pub fn fold(mut self) -> Self {
        self.overflow = Overflow::Fold;
        self
    }
    pub fn min(mut self, w: usize) -> Self {
        self.min_width = Some(w);
        self
    }
    pub fn max(mut self, w: usize) -> Self {
        self.max_width = Some(w);
        self
    }
}

/// `rich.table.Table(box=None, padding=(0, 1), expand=False)`.
#[derive(Clone, Debug)]
pub struct Table {
    pub title: Option<String>,
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Text>>,
    pub pad_edge: bool,
    pub show_header: bool,
}

impl Table {
    pub fn new(title: Option<String>, pad_edge: bool) -> Table {
        Table { title, columns: Vec::new(), rows: Vec::new(), pad_edge, show_header: true }
    }
    pub fn column(&mut self, c: Column) -> &mut Self {
        self.columns.push(c);
        self
    }
    pub fn row(&mut self, cells: Vec<Text>) -> &mut Self {
        self.rows.push(cells);
        self
    }

    fn pads(&self, i: usize) -> (usize, usize) {
        let mut left = 1;
        let mut right = 1;
        if !self.pad_edge {
            if i == 0 {
                left = 0;
            }
            if i + 1 == self.columns.len() {
                right = 0;
            }
        }
        (left, right)
    }

    fn cells(&self, i: usize) -> impl Iterator<Item = &Text> {
        let header = self.show_header.then_some(&self.columns[i].header);
        header.into_iter().chain(self.rows.iter().map(move |r| &r[i]))
    }

    fn measure_column(&self, i: usize, max_width: usize) -> Measure {
        if max_width < 1 {
            return Measure { min: 0, max: 0 };
        }
        let (l, r) = self.pads(i);
        let pad = l + r;
        let mut mins = Vec::new();
        let mut maxs = Vec::new();
        for cell in self.cells(i) {
            let m = measure_padded(cell, max_width, pad);
            mins.push(m.min);
            maxs.push(m.max);
        }
        let col = &self.columns[i];
        Measure { min: mins.iter().copied().max().unwrap_or(1), max: maxs.iter().copied().max().unwrap_or(max_width) }
            .with_maximum(max_width)
            .clamp(col.min_width.map(|w| w + pad), col.max_width.map(|w| w + pad))
    }

    /// `Table._calculate_column_widths` (expand=False, no box: no extra width).
    fn widths(&self, max_width: usize) -> Vec<usize> {
        let n = self.columns.len();
        let mut widths: Vec<usize> = (0..n).map(|i| self.measure_column(i, max_width).max.max(1)).collect();
        let table_width: usize = widths.iter().sum();
        if table_width > max_width {
            let wrapable: Vec<bool> = self.columns.iter().map(|c| !c.no_wrap).collect();
            widths = collapse_widths(widths, &wrapable, max_width);
            let mut table_width: usize = widths.iter().sum();
            if table_width > max_width {
                let excess = table_width - max_width;
                widths = ratio_reduce(excess, &vec![1; n], &widths.clone(), &widths);
                table_width = widths.iter().sum();
            }
            let _ = table_width;
            widths = (0..n).map(|i| self.measure_column(i, widths[i]).max).collect();
        }
        widths
    }

    /// The table's lines at a console width (not yet cropped to it).
    pub fn render(&self, max_width: usize) -> Vec<Vec<Cell>> {
        let mut out = Vec::new();
        if self.columns.is_empty() {
            out.push(Vec::new());
            return out;
        }
        let widths = self.widths(max_width);
        let table_width: usize = widths.iter().sum();
        if let Some(title) = &self.title {
            let t = Text::styled(title, Style::ITALIC);
            out.extend(wrap(&t, table_width, Justify::Left, Overflow::Fold, false));
        }
        let header_row = self.show_header.then(|| self.columns.iter().map(|c| c.header.clone()).collect::<Vec<_>>());
        let rows = header_row.iter().map(|r| (true, r)).chain(self.rows.iter().map(|r| (false, r)));
        for (is_header, row) in rows {
            let mut cells: Vec<Vec<Vec<Cell>>> = Vec::new();
            for (i, text) in row.iter().enumerate() {
                let col = &self.columns[i];
                let (l, r) = self.pads(i);
                let base = if is_header { Style::BOLD } else { col.style };
                let inner = widths[i].saturating_sub(l + r);
                let lines = wrap(&text.clone().under(base), inner, col.justify, col.overflow, col.no_wrap);
                let lines: Vec<Vec<Cell>> = lines
                    .into_iter()
                    .map(|line| {
                        let mut full: Vec<Cell> = std::iter::repeat_n((' ', Style::NONE), l).collect();
                        full.extend(shape(line, inner));
                        full.extend(std::iter::repeat_n((' ', Style::NONE), r));
                        shape(full, widths[i])
                    })
                    .collect();
                cells.push(lines);
            }
            let height = cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
            for (i, c) in cells.iter_mut().enumerate() {
                let blank = vec![(' ', Style::NONE); widths[i]];
                let missing = height - c.len();
                if is_header {
                    // header cells align to the bottom
                    let mut padded = vec![blank; missing];
                    padded.append(c);
                    *c = padded;
                } else {
                    c.extend(std::iter::repeat_n(blank, missing));
                }
            }
            for line_no in 0..height {
                let mut line = Vec::with_capacity(table_width);
                for c in &cells {
                    line.extend_from_slice(&c[line_no]);
                }
                out.push(line);
            }
        }
        out
    }
}

/// `Table._collapse_widths`.
fn collapse_widths(mut widths: Vec<usize>, wrapable: &[bool], max_width: usize) -> Vec<usize> {
    let mut total: usize = widths.iter().sum();
    let mut excess = total as isize - max_width as isize;
    if wrapable.iter().any(|&w| w) {
        while total > 0 && excess > 0 {
            let max_column = widths.iter().zip(wrapable).filter(|(_, w)| **w).map(|(&x, _)| x).max().unwrap_or(0);
            let second = widths
                .iter()
                .zip(wrapable)
                .map(|(&x, &w)| if w && x != max_column { x } else { 0 })
                .max()
                .unwrap_or(0);
            let difference = max_column - second;
            let ratios: Vec<usize> = widths.iter().zip(wrapable).map(|(&x, &w)| usize::from(x == max_column && w)).collect();
            if !ratios.iter().any(|&r| r > 0) || difference == 0 {
                break;
            }
            let max_reduce = vec![(excess as usize).min(difference); widths.len()];
            widths = ratio_reduce(excess as usize, &ratios, &max_reduce, &widths);
            total = widths.iter().sum();
            excess = total as isize - max_width as isize;
        }
    }
    widths
}

// ------------------------------------------------------------------------------------- panel

/// `rich.panel.Panel(text, title=…, title_align="left")`, expanded to `width`.
pub fn panel(content: &Text, title: &str, border: Style, width: usize, legacy_windows: bool) -> Vec<Vec<Cell>> {
    // ROUNDED, or SQUARE where rich substitutes boxes for a legacy Windows console
    let (tl, tr, bl, br) = if legacy_windows { ('┌', '┐', '└', '┘') } else { ('╭', '╮', '╰', '╯') };
    let (h, v) = ('─', '│');
    let mut out = Vec::new();
    let title_text = Text::plain(&format!(" {} ", title.replace('\n', " ")));
    let child_width = (width.saturating_sub(2)).max(title_text.cell_len() + 2).min(width.saturating_sub(2));
    let width = child_width + 2;
    let b = |c: char| (c, border);
    if width <= 4 {
        let mut top = vec![b(tl)];
        top.extend(std::iter::repeat_n(b(h), width.saturating_sub(2)));
        top.push(b(tr));
        out.push(top);
    } else {
        let mut t = title_text.0.clone();
        let avail = width - 4;
        set_cell_size_crop(&mut t, avail);
        let excess = avail.saturating_sub(cells_of(&t));
        let mut top = vec![b(tl), b(h)];
        top.extend(t.into_iter().map(|(c, s)| (c, border.with(s))));
        top.extend(std::iter::repeat_n(b(h), excess));
        top.push(b(h));
        top.push(b(tr));
        out.push(top);
    }
    let inner = child_width.saturating_sub(2);
    for line in wrap(content, inner, Justify::Default, Overflow::Fold, false) {
        let mut row = vec![b(v), (' ', Style::NONE)];
        row.extend(shape(line, inner));
        row.push((' ', Style::NONE));
        row.push(b(v));
        out.push(row);
    }
    let mut bottom = vec![b(bl)];
    bottom.extend(std::iter::repeat_n(b(h), width - 2));
    bottom.push(b(br));
    out.push(bottom);
    out
}

/// `Text.truncate(width)` without padding (titles).
fn set_cell_size_crop(line: &mut Vec<Cell>, width: usize) {
    if cells_of(line) > width {
        set_cell_size(line, width);
    }
}

// ------------------------------------------------------------------------------------- console

/// Where output goes and how wide it is, decided as rich's `Console` decides.
pub struct Console {
    pub width: usize,
    pub color: bool,
    pub terminal: bool,
    /// a Windows console without VT processing (any pipe): width - 1, square boxes
    pub legacy_windows: bool,
    pub dumb: bool,
    buf: String,
    stderr: bool,
    capture: bool,
}

impl Console {
    /// stdout as rich's default `Console()` sees it.
    pub fn stdout() -> Console {
        Self::for_stream(std::io::stdout().is_terminal(), false)
    }

    pub fn stderr() -> Console {
        Self::for_stream(std::io::stderr().is_terminal(), true)
    }

    fn for_stream(tty: bool, stderr: bool) -> Console {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let force = env("FORCE_COLOR").is_some();
        let terminal = tty || force;
        let legacy_windows = cfg!(windows) && !tty;
        let dumb = terminal && env("TERM").is_some_and(|t| t.eq_ignore_ascii_case("dumb") || t.eq_ignore_ascii_case("unknown"));
        let digits = |k: &str| env(k).filter(|v| v.bytes().all(|b| b.is_ascii_digit())).and_then(|v| v.parse::<usize>().ok());
        let legacy = usize::from(legacy_windows);
        let width = match digits("COLUMNS") {
            Some(c) => {
                let w = c.saturating_sub(legacy);
                // rich subtracts it once more when LINES is set too
                if digits("LINES").is_some() { w.saturating_sub(legacy) } else { w }
            }
            None => {
                let w = if dumb { None } else if tty { terminal_width(stderr) } else { None };
                w.unwrap_or(80).saturating_sub(legacy)
            }
        };
        let color = terminal && env("NO_COLOR").is_none() && !dumb;
        if color {
            enable_vt(stderr);
        }
        Console { width: width.max(1), color, terminal, legacy_windows, dumb, buf: String::new(), stderr, capture: false }
    }

    /// A console writing into a buffer (tests).
    pub fn capture(width: usize) -> Console {
        Console { width, color: false, terminal: false, legacy_windows: false, dumb: false, buf: String::new(), stderr: false, capture: true }
    }

    pub fn interactive(&self) -> bool {
        self.terminal && !self.dumb
    }

    /// Lines as `Console.print` writes them: each cropped to the width (never padded).
    pub fn lines(&mut self, lines: Vec<Vec<Cell>>) {
        for mut line in lines {
            if cells_of(&line) > self.width {
                set_cell_size(&mut line, self.width);
            }
            self.write_line(&line);
        }
    }

    /// `console.print(text)`: wrapped (folding long words) at the console width.
    pub fn text(&mut self, text: &Text) {
        let lines = wrap(text, self.width, Justify::Default, Overflow::Fold, false);
        self.lines(lines);
    }

    /// `console.print()`.
    pub fn blank(&mut self) {
        self.write_line(&[]);
    }

    pub fn panel(&mut self, content: &Text, title: &str) {
        let lines = panel(content, title, BORDER, self.width, self.legacy_windows);
        self.lines(lines);
    }

    pub fn table(&mut self, table: &Table) {
        let lines = table.render(self.width);
        self.lines(lines);
    }

    fn write_line(&mut self, line: &[Cell]) {
        if self.color {
            let mut current = Style::NONE;
            for &(c, s) in line {
                if s != current {
                    if current != Style::NONE {
                        self.buf.push_str("\x1b[0m");
                    }
                    if s != Style::NONE {
                        self.buf.push_str(&format!("\x1b[{}m", s.sgr()));
                    }
                    current = s;
                }
                self.buf.push(c);
            }
            if current != Style::NONE {
                self.buf.push_str("\x1b[0m");
            }
        } else {
            self.buf.extend(line.iter().map(|c| c.0));
        }
        // Python's text-mode streams write \r\n on Windows
        self.buf.push_str(if cfg!(windows) && !self.capture { "\r\n" } else { "\n" });
    }

    /// What was printed so far (and forget it).
    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.buf)
    }

    /// Write out what was printed so far.
    pub fn flush(&mut self) {
        use std::io::Write as _;
        if self.buf.is_empty() || self.capture {
            return;
        }
        let text = std::mem::take(&mut self.buf);
        if self.stderr {
            let mut e = std::io::stderr().lock();
            let _ = e.write_all(text.as_bytes());
            let _ = e.flush();
        } else {
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(text.as_bytes());
            let _ = o.flush();
        }
    }
}

/// The terminal's width in columns, when there is one.
#[cfg(windows)]
fn terminal_width(stderr: bool) -> Option<usize> {
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };
    // SAFETY: plain Win32 calls with a zeroed out-parameter.
    unsafe {
        let h = GetStdHandle(if stderr { STD_ERROR_HANDLE } else { STD_OUTPUT_HANDLE });
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(h, &mut info) == 0 {
            return None;
        }
        let w = (info.srWindow.Right - info.srWindow.Left + 1) as i32;
        (w > 0).then_some(w as usize)
    }
}

#[cfg(unix)]
fn terminal_width(stderr: bool) -> Option<usize> {
    // SAFETY: TIOCGWINSZ fills a winsize.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        let fd = if stderr { libc::STDERR_FILENO } else { libc::STDOUT_FILENO };
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) != 0 || ws.ws_col == 0 {
            return None;
        }
        Some(ws.ws_col as usize)
    }
}

/// Windows consoles interpret ANSI sequences once asked to.
#[cfg(windows)]
fn enable_vt(stderr: bool) {
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        SetConsoleMode,
    };
    // SAFETY: plain Win32 calls on the process's own standard handle.
    unsafe {
        let h = GetStdHandle(if stderr { STD_ERROR_HANDLE } else { STD_OUTPUT_HANDLE });
        let mut mode = 0;
        if GetConsoleMode(h, &mut mode) != 0 {
            SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(windows))]
fn enable_vt(_stderr: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: Vec<Vec<Cell>>) -> Vec<String> {
        lines.into_iter().map(|l| l.into_iter().map(|c| c.0).collect()).collect()
    }

    #[test]
    fn wraps_and_folds_like_rich() {
        let t = Text::plain("Largest items in C:/a/very/long/path");
        assert_eq!(
            plain(wrap(&t, 10, Justify::Left, Overflow::Fold, false)),
            ["Largest   ", "items in  ", "C:/a/very/", "long/path "]
        );
        // a wide character never splits; it moves to the next line
        let t = Text::plain("ab中文");
        assert_eq!(plain(wrap(&t, 3, Justify::Default, Overflow::Fold, false)), ["ab", "中", "文"]);
    }

    #[test]
    fn ellipsis_and_right_justify() {
        let t = Text::plain("Size on disk");
        assert_eq!(plain(wrap(&t, 10, Justify::Right, Overflow::Ellipsis, true)), ["Size on d…"]);
        let t = Text::plain("12 KB");
        assert_eq!(plain(wrap(&t, 8, Justify::Right, Overflow::Ellipsis, true)), ["   12 KB"]);
    }

    #[test]
    fn ratio_reduce_rounds_half_to_even() {
        assert_eq!(ratio_reduce(8, &[1, 1, 1, 1, 1], &[45, 14, 8, 14, 6], &[45, 14, 8, 14, 6]), [43, 12, 7, 12, 5]);
    }
}
