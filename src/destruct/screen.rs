//! The self-destruct screen: octagon, animated X, double-height counter and
//! the questionnaire strip. Geometry is for 80x43; the constants below are
//! tuned on the WY-60 through `muthur destruct`.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::{Destruct, Phase};
use crate::dialect::LineAttr;

const HEADING_ROW: u16 = 0;
/// Octagon outline: left/right columns, top/bottom rows, corner cut size.
const OCT_LEFT: i32 = 8;
const OCT_RIGHT: i32 = 72;
const OCT_TOP: i32 = 3;
const OCT_BOTTOM: i32 = 37;
const OCT_CUT: i32 = 8;
/// The X's centre and the columns its centrelines move per row.
const X_CX: i32 = 40;
const X_CY: i32 = 20;
const X_SLOPE: f64 = 1.5;
/// The DECDHL pair holding the counter; its frame rows are just above and below.
const COUNTER_ROW: u16 = 20;
/// Counter box in double-width cells: `│d│d│d│d│d│` from cell 4 to 14.
const BOX_FIRST: u16 = 4;
const BOX_LAST: u16 = 14;
/// Double-width cells where the X may go on the counter rows.
const X_HALF_CELLS: std::ops::RangeInclusive<i32> = 15..=35;
/// The octagon's right edge in double-width cells. A double-width cell `k`
/// covers screen columns `2k` and `2k+1`, so both edges sit on even columns
/// (`OCT_LEFT`, `OCT_RIGHT`) to line up with the single-width rows.
const RIGHT_HALF_CELL: u16 = (OCT_RIGHT / 2) as u16;
const STRIP_ROW: u16 = 42;
const STRIP_FIRST: u16 = 1;
const STRIP_LAST: u16 = 78;
/// The frozen strip turns to `SELF-DESTRUCT ABORTED` after this.
const ABORTED_AFTER: Duration = Duration::from_millis(1500);
const CYCLE_MS: u128 = 6000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum XPhase {
    /// Two parallel lines per arm, this many columns apart.
    Gap(u16),
    /// Single lines, bold.
    Glow,
    /// Single lines, dim.
    Dim,
    /// No X.
    Blank,
}

/// The X's phase `elapsed` after arming: a 6 s cycle. The narrowing steps are
/// slow (1.5 s each) so the link has room for typing and the clock.
pub fn x_phase(elapsed: Duration) -> XPhase {
    match elapsed.as_millis() % CYCLE_MS {
        0..1500 => XPhase::Gap(6),
        1500..3000 => XPhase::Gap(4),
        3000..4500 => XPhase::Gap(2),
        4500..5000 => XPhase::Glow,
        5000..5500 => XPhase::Dim,
        _ => XPhase::Blank,
    }
}

/// How far in from the left/right edge the outline is on row `y` (the cuts).
fn inset(y: i32) -> i32 {
    (OCT_CUT - (y - OCT_TOP)).max(OCT_CUT - (OCT_BOTTOM - y)).max(0)
}

/// Strictly inside the octagon outline.
fn interior(x: i32, y: i32) -> bool {
    y > OCT_TOP && y < OCT_BOTTOM && x > OCT_LEFT + inset(y) && x < OCT_RIGHT - inset(y)
}

/// The octagon outline: (column, row, glyph).
fn octagon() -> Vec<(i32, i32, &'static str)> {
    let mut out = Vec::new();
    for x in OCT_LEFT + OCT_CUT..=OCT_RIGHT - OCT_CUT {
        out.push((x, OCT_TOP, "─"));
        out.push((x, OCT_BOTTOM, "─"));
    }
    for y in OCT_TOP + OCT_CUT + 1..OCT_BOTTOM - OCT_CUT {
        out.push((OCT_LEFT, y, "│"));
        out.push((OCT_RIGHT, y, "│"));
    }
    for i in 1..=OCT_CUT {
        out.push((OCT_LEFT + OCT_CUT - i, OCT_TOP + i, "/"));
        out.push((OCT_RIGHT - OCT_CUT + i, OCT_TOP + i, "\\"));
        out.push((OCT_LEFT + OCT_CUT - i, OCT_BOTTOM - i, "\\"));
        out.push((OCT_RIGHT - OCT_CUT + i, OCT_BOTTOM - i, "/"));
    }
    out
}

/// Column offset of a centreline from the X's centre on row `y`, rounded half
/// away from zero.
fn offset(y: i32) -> i32 {
    (X_SLOPE * f64::from(y - X_CY)).round() as i32
}

/// The outline of the union of the X's two bands, gap `g`, clipped to the
/// octagon's inside: (column, row, glyph). `\` is the band falling left to
/// right, `/` the other. Where an edge of each lands on one cell, that cell is
/// a vertex of the union: `V` above the centre, `^` below, `>`/`<` either
/// side of it, `X` at the crossing of single lines.
fn x_cells(g: i32) -> Vec<(i32, i32, &'static str)> {
    let h = g / 2;
    let mut out: Vec<(i32, i32, &'static str)> = Vec::new();
    for y in OCT_TOP + 1..OCT_BOTTOM {
        let falling = X_CX + offset(y);
        let rising = X_CX - offset(y);
        let inside = |c: i32, x: i32| x > c - h && x < c + h;
        for (own, other, glyph) in [(falling, rising, "\\"), (rising, falling, "/")] {
            for x in [own - h, own + h] {
                if inside(other, x) || !interior(x, y) {
                    continue;
                }
                match out.iter_mut().find(|c| (c.0, c.1) == (x, y)) {
                    Some(c) if c.2 != glyph => c.2 = vertex(x, y),
                    Some(_) => {}
                    None => out.push((x, y, glyph)),
                }
            }
        }
    }
    out
}

fn vertex(x: i32, y: i32) -> &'static str {
    match (y.cmp(&X_CY), x.cmp(&X_CX)) {
        (std::cmp::Ordering::Less, _) => "V",
        (std::cmp::Ordering::Greater, _) => "^",
        (_, std::cmp::Ordering::Less) => ">",
        (_, std::cmp::Ordering::Greater) => "<",
        _ => "X",
    }
}

fn put(buf: &mut Buffer, area: Rect, x: i32, y: i32, s: &str, style: Style) {
    let width = if is_counter_row(y) { area.width / 2 } else { area.width };
    if x < 0 || y < 0 || x >= i32::from(width) || y >= i32::from(area.height) {
        return;
    }
    let cell = &mut buf[(area.x + x as u16, area.y + y as u16)];
    cell.set_symbol(s);
    cell.set_style(style);
}

fn put_str(buf: &mut Buffer, area: Rect, x: i32, y: i32, s: &str, style: Style) {
    let mut b = [0u8; 4];
    for (i, ch) in s.chars().enumerate() {
        put(buf, area, x + i as i32, y, ch.encode_utf8(&mut b), style);
    }
}

fn is_counter_row(y: i32) -> bool {
    y == i32::from(COUNTER_ROW) || y == i32::from(COUNTER_ROW) + 1
}

/// The counter's frame rows: screen columns `2k` for the box's cells.
fn frame_cols() -> std::ops::RangeInclusive<i32> {
    i32::from(BOX_FIRST) * 2..=i32::from(BOX_LAST) * 2
}

impl Destruct {
    /// The DECDHL rows the screen needs (absolute screen rows).
    pub fn line_attrs(&self, area: Rect) -> Vec<(u16, LineAttr)> {
        vec![(area.y + COUNTER_ROW, LineAttr::DoubleTop), (area.y + COUNTER_ROW + 1, LineAttr::DoubleBottom)]
    }

    /// When the X stops moving: the freeze, or the clock reaching zero.
    fn x_clock(&self, now: Instant) -> Instant {
        match self.phase {
            Phase::Running => now,
            Phase::Frozen { since } => since,
            Phase::Executing | Phase::Failed(_) | Phase::Zero => self.zero_at(),
        }
    }

    pub fn render(&self, now: Instant, area: Rect, buf: &mut Buffer) {
        let plain = Style::default();
        let heading = format!("EMERGENCY DESTRUCT SYSTEM ── {}", self.kind.label());
        let hx = (i32::from(area.width) - heading.chars().count() as i32) / 2;
        put_str(buf, area, hx, i32::from(HEADING_ROW), &heading, plain);

        // Octagon; the counter rows get their edges at half resolution below.
        for (x, y, g) in octagon() {
            if !is_counter_row(y) {
                put(buf, area, x, y, g, plain);
            }
        }

        // The X.
        let elapsed = self.x_clock(now).saturating_duration_since(self.started);
        let (gap, style) = match x_phase(elapsed) {
            XPhase::Gap(g) => (Some(i32::from(g)), plain),
            XPhase::Glow => (Some(0), plain.add_modifier(Modifier::BOLD)),
            XPhase::Dim => (Some(0), plain.add_modifier(Modifier::DIM)),
            XPhase::Blank => (None, plain),
        };
        let top = i32::from(COUNTER_ROW) - 1;
        let bottom = i32::from(COUNTER_ROW) + 2;
        for (x, y, g) in gap.map(x_cells).unwrap_or_default() {
            if is_counter_row(y) {
                if X_HALF_CELLS.contains(&(x / 2)) {
                    put(buf, area, x / 2, y, g, style);
                }
            } else if !((y == top || y == bottom) && frame_cols().contains(&x)) {
                put(buf, area, x, y, g, style);
            }
        }

        // Counter frame rows.
        for (y, mid, end) in [(top, "┬", "┐"), (bottom, "┴", "┘")] {
            for x in frame_cols() {
                let k = x / 2;
                let g = match x {
                    _ if x == *frame_cols().start() => "├",
                    _ if x == *frame_cols().end() => end,
                    _ if x % 2 == 0 && k % 2 == 0 => mid,
                    _ => "─",
                };
                put(buf, area, x, y, g, plain);
            }
        }

        // Counter digits and the octagon's right edge, at double width.
        let digits = self.counter(now);
        for y in [i32::from(COUNTER_ROW), i32::from(COUNTER_ROW) + 1] {
            for k in BOX_FIRST..=BOX_LAST {
                let i = usize::from(k - BOX_FIRST);
                if i % 2 == 0 {
                    put(buf, area, i32::from(k), y, "│", plain);
                } else {
                    let d = i / 2;
                    let style = if d == 2 || d == 3 { plain.add_modifier(Modifier::BOLD) } else { plain };
                    put(buf, area, i32::from(k), y, &digits[d].to_string(), style);
                }
            }
            put(buf, area, i32::from(RIGHT_HALF_CELL), y, "│", plain);
        }

        self.render_strip(now, area, buf);
    }

    fn render_strip(&self, now: Instant, area: Rect, buf: &mut Buffer) {
        let rev = Style::default().add_modifier(Modifier::REVERSED);
        let y = i32::from(STRIP_ROW);
        for x in 0..i32::from(area.width) {
            put(buf, area, x, y, " ", rev);
        }
        let room = usize::from(STRIP_LAST - STRIP_FIRST + 1);
        let (left, input) = match &self.phase {
            Phase::Running => (self.quiz.question(now), self.quiz.input()),
            Phase::Frozen { since } if now >= *since + ABORTED_AFTER => ("SELF-DESTRUCT ABORTED".into(), ""),
            Phase::Frozen { .. } => (self.quiz.question(now), self.quiz.input()),
            Phase::Failed(reason) => (format!("UNABLE TO COMPLY -- {reason}"), ""),
            Phase::Executing | Phase::Zero => (self.quiz.question(now), ""),
        };
        let left: String = left.chars().take(room).collect();
        put_str(buf, area, i32::from(STRIP_FIRST), y, &left, rev);
        let n = input.chars().count() as i32;
        put_str(buf, area, i32::from(STRIP_LAST) + 1 - n, y, input, rev);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;

    use super::*;
    use crate::destruct::{Destruct, DestructRequest, Kind};
    use crate::dialect::LineAttr;
    use crate::secret::Secret;

    fn s(x: f64) -> Duration {
        Duration::from_secs_f64(x)
    }

    fn mk(total: u64) -> (Destruct, Instant) {
        let t0 = Instant::now();
        let req = DestructRequest {
            kind: Kind::Shutdown,
            total: Duration::from_secs(total),
            password: Secret::new(),
        };
        (Destruct::new(req, t0, 7), t0)
    }

    fn draw(d: &Destruct, now: Instant) -> Buffer {
        let area = Rect::new(0, 0, 80, 43);
        let mut buf = Buffer::empty(area);
        d.render(now, area, &mut buf);
        buf
    }

    /// A double-width cell `k` starts at screen column `2k`: the counter rows'
    /// edges must land on the same columns as the single-width rows' edges.
    #[test]
    fn counter_row_edges_line_up_with_octagon() {
        assert_eq!(i32::from(BOX_FIRST) * 2, OCT_LEFT);
        assert_eq!(i32::from(RIGHT_HALF_CELL) * 2, OCT_RIGHT);
        let buf = draw(
            &Destruct::preview(super::super::Kind::Shutdown, Duration::from_secs(70), Instant::now(), 1),
            Instant::now(),
        );
        // Single-width row 25 and double-width row 20 both carry the right edge.
        assert_eq!(buf[(OCT_RIGHT as u16, 25)].symbol(), "│");
        assert_eq!(buf[(RIGHT_HALF_CELL, COUNTER_ROW)].symbol(), "│");
    }

    #[test]
    fn x_phase_cycle() {
        let got: Vec<XPhase> = [0.0, 1.6, 3.1, 4.6, 5.1, 5.6, 6.0].into_iter().map(|t| x_phase(s(t))).collect();
        assert_eq!(
            got,
            [
                XPhase::Gap(6),
                XPhase::Gap(4),
                XPhase::Gap(2),
                XPhase::Glow,
                XPhase::Dim,
                XPhase::Blank,
                XPhase::Gap(6)
            ]
        );
    }

    #[test]
    fn counter_row_has_digits_and_bold_seconds() {
        let (d, t0) = mk(70);
        let buf = draw(&d, t0);
        let digits: Vec<&str> = [5, 7, 9, 11, 13].into_iter().map(|x| buf[(x, 20)].symbol()).collect();
        assert_eq!(digits, ["0", "1", "1", "0", "0"]);
        for x in [9, 11] {
            assert!(buf[(x, 20)].modifier.contains(Modifier::BOLD), "seconds cell {x} is bold");
        }
        for x in [5, 7, 13] {
            assert!(!buf[(x, 20)].modifier.contains(Modifier::BOLD), "cell {x} is not bold");
        }
        assert_eq!(
            d.line_attrs(Rect::new(0, 0, 80, 43)),
            vec![(20, LineAttr::DoubleTop), (21, LineAttr::DoubleBottom)]
        );
    }

    fn x_cells(buf: &Buffer) -> Vec<(u16, u16, Modifier)> {
        let mut out = Vec::new();
        for y in (OCT_TOP as u16 + 1)..(OCT_BOTTOM as u16) {
            if y == COUNTER_ROW || y == COUNTER_ROW + 1 {
                continue;
            }
            for x in 0..80 {
                let c = &buf[(x, y)];
                if (c.symbol() == "\\" || c.symbol() == "/") && interior(i32::from(x), i32::from(y)) {
                    out.push((x, y, c.modifier));
                }
            }
        }
        out
    }

    #[test]
    fn glow_is_bold_dim_is_dim() {
        let (d, t0) = mk(70);
        let glow = x_cells(&draw(&d, t0 + s(4.6)));
        assert!(glow.iter().any(|&(_, _, m)| m.contains(Modifier::BOLD)), "{glow:?}");
        let dim = x_cells(&draw(&d, t0 + s(5.1)));
        assert!(!dim.is_empty() && dim.iter().all(|&(_, _, m)| m.contains(Modifier::DIM)), "{dim:?}");
        let blank = x_cells(&draw(&d, t0 + s(5.6)));
        assert!(blank.is_empty(), "{blank:?}");
    }
}
