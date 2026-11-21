//! The standby screen: a static logo and caption shown after
//! `[chrome] standby_minutes` without a key, so nothing reaches the wire and
//! the WY-60's own screen saver can blank the CRT.

use crate::dialect::LineAttr;
use crate::splash::logo::{self, Logo};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const TITLE: &str = "MU/TH/UR 6000 ── INTERFACE STANDBY";
const PROMPT: &str = "PRESS ANY KEY";
/// Rows under the logo: one blank, then three caption lines.
const CAPTION_ROWS: usize = 4;

/// Fixed once built: every render is the same frame, so it costs no bytes.
pub struct Standby {
    since: String,
    logo: Logo,
}

impl Standby {
    /// `since` is the formatted date standby began.
    pub fn new(since: String) -> Standby {
        Self::with_logo(since, Logo::shipped().clone())
    }

    /// As `new`, with this logo (as `Splash::with_logo`).
    pub fn with_logo(since: String, logo: Logo) -> Standby {
        Standby { since, logo }
    }

    /// The logo's first screen row: logo and caption centred as one block.
    fn top(&self, area: Rect) -> i32 {
        let h = self.logo.height() + CAPTION_ROWS;
        (usize::from(area.height).saturating_sub(h) / 2) as i32
    }

    pub fn line_attrs(&self, area: Rect) -> Vec<(u16, LineAttr)> {
        logo::line_attrs(&self.logo, area, self.top(area))
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let l = &self.logo;
        let top = self.top(area);
        logo::draw(l, area, top, buf);
        let since = format!("STANDBY SINCE {}", self.since);
        let y0 = top + l.height() as i32 + 1;
        for (i, text) in [TITLE, since.as_str(), PROMPT].into_iter().enumerate() {
            let y = y0 + i as i32;
            if y < 0 || y >= i32::from(area.height) {
                continue;
            }
            let x = usize::from(area.width).saturating_sub(text.chars().count()) / 2;
            buf.set_stringn(
                area.x + x as u16,
                area.y + y as u16,
                text,
                usize::from(area.width) - x,
                ratatui::style::Style::default(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0, 0, 80, 43)
    }

    fn screen() -> Buffer {
        let mut buf = Buffer::empty(area());
        Standby::new("17 NOV 2137 14:02".into()).render(area(), &mut buf);
        buf
    }

    fn row(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect()
    }

    #[test]
    fn caption_is_centred_under_logo() {
        let buf = screen();
        for (y, want) in [
            (26, "MU/TH/UR 6000 ── INTERFACE STANDBY"),
            (27, "STANDBY SINCE 17 NOV 2137 14:02"),
            (28, "PRESS ANY KEY"),
        ] {
            let r = row(&buf, y);
            assert_eq!(r.trim(), want, "row {y}");
            let lead = r.chars().take_while(|c| *c == ' ').count();
            assert_eq!(lead, (80 - want.chars().count()) / 2, "row {y}");
        }
    }

    #[test]
    fn logo_sits_above_caption() {
        let buf = screen();
        assert_eq!(
            Standby::new(String::new()).line_attrs(area()),
            vec![(13, LineAttr::DoubleTop), (14, LineAttr::DoubleBottom)]
        );
        assert_eq!((buf[(6, 13)].symbol(), buf[(6, 14)].symbol()), ("W", "W"));
        assert_eq!(row(&buf, 24).trim(), "BUILDING BETTER WORLDS");
        assert!(row(&buf, 25).trim().is_empty());
    }

    #[test]
    fn layout_follows_the_given_logo() {
        let mut buf = Buffer::empty(area());
        let sb = Standby::with_logo("X".into(), Logo::parse("A\n"));
        sb.render(area(), &mut buf);
        // One logo row + four caption rows, centred: logo on 19, caption on 21-23.
        assert_eq!(row(&buf, 19).trim(), "A");
        assert_eq!(row(&buf, 22).trim(), "STANDBY SINCE X");
        assert!(sb.line_attrs(area()).is_empty());
    }

    #[test]
    fn nothing_outside_the_block() {
        let buf = screen();
        for y in (0..13).chain(29..43) {
            assert!(row(&buf, y).trim().is_empty(), "row {y}");
        }
    }
}
