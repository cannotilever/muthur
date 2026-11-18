//! VT100-safe gauges and sparklines. `█` becomes a reverse-video space on the
//! wire; the scan-line glyphs `⎽⎼─⎻⎺` are DEC Special Graphics.

use ratatui::text::Line;
use ratatui::widgets::Block;

const LEVELS: [char; 5] = ['⎽', '⎼', '─', '⎻', '⎺'];
const MIN_BAR: usize = 4;

/// `LABEL [█████     ]  50% DETAIL`, exactly `width` wide when there is room.
/// Pad `detail` to a common width to line up several gauges.
pub fn gauge_line(label: &str, pct: f32, detail: &str, width: u16) -> Line<'static> {
    let pct = if pct.is_finite() { pct.clamp(0.0, 100.0) } else { 0.0 };
    let tail = if detail.is_empty() { String::new() } else { format!(" {detail}") };
    // label(5) + " [" + "]" + " 100%" + tail
    let fixed = 5 + 3 + 5 + tail.chars().count();
    let bar = (width as usize).saturating_sub(fixed).max(MIN_BAR);
    let filled = (bar as f32 * pct / 100.0).floor() as usize;
    let s = format!(
        "{label:<5.5} [{}{}] {:>3}%{tail}",
        "█".repeat(filled),
        " ".repeat(bar - filled),
        pct.round() as u32
    );
    Line::from(s.chars().take(width as usize).collect::<String>())
}

/// The last `width` values as scan-line levels, right-aligned.
pub fn sparkline(values: &[f32], max: f32, width: u16) -> String {
    let width = width as usize;
    let tail = &values[values.len().saturating_sub(width)..];
    let level = |v: f32| -> char {
        if max <= 0.0 || !v.is_finite() {
            return LEVELS[0];
        }
        LEVELS[((v / max).clamp(0.0, 1.0) * 4.0).round() as usize]
    };
    let mut s = " ".repeat(width - tail.len());
    s.extend(tail.iter().map(|&v| level(v)));
    s
}

/// A bordered panel titled ` TITLE `.
pub fn panel(title: &str) -> Block<'static> {
    Block::bordered().title(format!(" {} ", title.to_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::{Glyph, map_glyph};

    #[test]
    fn gauge_clamps() {
        let l = gauge_line("CPU", 250.0, "", 30).to_string();
        assert!(l.contains("100%"), "{l}");
        let l = gauge_line("DISK", -3.0, "", 30).to_string();
        assert!(l.contains("  0%") && !l.contains('█'), "{l}");
    }

    #[test]
    fn gauge_fills_proportionally() {
        let s = gauge_line("MEM", 50.0, "1.0G/2.0G", 40).to_string();
        let filled = s.matches('█').count();
        let bar = s[s.find('[').unwrap() + 1..s.find(']').unwrap()].chars().count();
        assert_eq!(filled, bar / 2, "{s}");
        assert!(s.ends_with("1.0G/2.0G"), "{s}");
        assert_eq!(s.chars().count(), 40, "{s}");
    }

    #[test]
    fn gauge_never_exceeds_width() {
        let s = gauge_line("DISK", 10.0, "999.9G/999.9G", 20).to_string();
        assert!(s.chars().count() <= 20, "{s}");
    }

    #[test]
    fn sparkline_levels_and_width() {
        assert_eq!(sparkline(&[0.0, 25.0, 50.0, 75.0, 100.0], 100.0, 7), "  ⎽⎼─⎻⎺");
        assert_eq!(sparkline(&[1.0; 10], 0.0, 3), "⎽⎽⎽");
        assert_eq!(sparkline(&[100.0, 0.0, 300.0], 100.0, 2), "⎽⎺");
    }

    #[test]
    fn widgets_render_dialect_safe() {
        for ch in sparkline(&[0., 25., 50., 75., 100.], 100., 5).chars().chain("█".chars()) {
            assert_ne!(map_glyph(&ch.to_string()), Glyph::Ascii(b'?'), "{ch}");
        }
    }
}
