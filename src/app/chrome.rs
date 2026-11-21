//! The personal-terminal chrome: title bar, FOLDERS sidebar, connector,
//! footer, prefix strip and overlays. Laid out for 80×43.

use super::tabs::{TABS, TabId};
use chrono::{Datelike, NaiveDateTime};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Clear, Paragraph, Widget};

pub const TITLE_ROW: u16 = 0;
pub const FOOTER_ROW: u16 = 42;
pub const BOX_X: u16 = 1;
pub const BOX_W: u16 = 16;
pub const BOX_H: u16 = 5;
pub const BOX_TOP: u16 = 4;
pub const BOX_GAP: u16 = 1;
/// The tab's area in windowed mode.
pub const CONTENT: Rect = Rect {
    x: 19,
    y: 2,
    width: 61,
    height: 40,
};

const FOOTER: &str = "F1-F4 SELECT .... ^] F FULLSCREEN .... SH-F15/16 SCROLL .... ^] ? HELP";
const PREFIX: &str = "PREFIX: F FULL  R REDRAW  D DATE  ? HELP  Q QUIT  ] SEND ^]";
const HELP: [&str; 12] = [
    "F1 - F4          SELECT FOLDER",
    "SH-F15 / SH-F16  SCROLL UP / DOWN",
    "^] F             FULLSCREEN",
    "^] R             REDRAW SCREEN",
    "^] D             REAL / SHIP DATE",
    "^] [             SCROLLBACK (SHELL)",
    "^] P             RAW PASSTHROUGH (SHELL)",
    "^] ] OR ^] ^]    SEND ^] TO FOLDER",
    "^] Q             QUIT",
    "BREAK            REINIT SCREEN",
    "",
    "ANY KEY TO CLOSE",
];

fn reverse() -> Style {
    Style::new().add_modifier(Modifier::REVERSED)
}

pub fn box_top(id: TabId) -> u16 {
    BOX_TOP + id.index() as u16 * (BOX_H + BOX_GAP)
}

/// `17 NOV 2137 14:02`, the year shifted by `year_offset`.
pub fn format_date(now: NaiveDateTime, year_offset: i32) -> String {
    let month = now.format("%b").to_string().to_uppercase();
    format!("{:02} {} {:04} {}", now.day(), month, now.year() + year_offset, now.format("%H:%M"))
}

pub fn title_bar(buf: &mut Buffer, width: u16, ship: &str, date: &str) {
    let left = format!(" {ship} ── MU/TH/UR 6000");
    let right = format!("{date} ");
    let pad = (width as usize).saturating_sub(left.chars().count() + right.chars().count());
    let line = format!("{left}{}{right}", " ".repeat(pad));
    buf.set_stringn(0, TITLE_ROW, line, width as usize, reverse());
}

pub fn sidebar(buf: &mut Buffer, active: TabId, activity: &[bool; TABS]) {
    buf.set_string(BOX_X, 2, "FOLDERS", Style::new());
    for id in TabId::ALL {
        let top = box_top(id);
        Block::bordered().render(Rect::new(BOX_X, top, BOX_W, BOX_H), buf);
        let style = if id == active { reverse() } else { Style::new() };
        buf.set_string(BOX_X + 2, top + 1, id.label(), style);
        if activity[id.index()] {
            buf.set_string(BOX_X + BOX_W - 2, top + 1, "*", Style::new().add_modifier(Modifier::SLOW_BLINK));
        }
    }
    // Connector from the active folder to the content pane, as in the game.
    let mid = box_top(active) + 2;
    buf[(BOX_X + BOX_W - 1, mid)].set_symbol("├");
    for x in BOX_X + BOX_W..CONTENT.x {
        buf[(x, mid)].set_symbol("─");
    }
    buf[(CONTENT.x, mid)].set_symbol("┤");
}

pub fn footer(buf: &mut Buffer) {
    buf.set_string(1, FOOTER_ROW, FOOTER, Style::new());
}

pub fn prefix_strip(buf: &mut Buffer, width: u16, row: u16) {
    let line = format!("{PREFIX:<w$}", w = width as usize);
    buf.set_stringn(0, row, line, width as usize, reverse());
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

pub fn help(buf: &mut Buffer, area: Rect) {
    let r = centered(area, 40, HELP.len() as u16 + 2);
    Clear.render(r, buf);
    Paragraph::new(HELP.join("\n")).block(Block::bordered().title(" HELP ")).render(r, buf);
}

pub fn message(buf: &mut Buffer, area: Rect, text: &str) {
    let r = centered(area, text.len() as u16 + 4, 3);
    Clear.render(r, buf);
    Paragraph::new(text).block(Block::bordered()).render(r, buf);
}
