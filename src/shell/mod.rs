//! The Shell tab: a PTY, a virtual terminal, and translation to the WY-60.
//!
//! PTY bytes → `AcsFilter` → `vt100::Parser` (screen + scrollback). Each frame
//! the virtual screen is copied into the ratatui buffer; the backend maps
//! glyphs to what the WY-60 can show. Row hashes detect scrolling and leave a
//! hint so the backend can scroll instead of repaint.

pub mod acs;
pub mod keys;
pub mod pty;

use crate::app::Tab;
use crate::input::Key;
use crate::scroll::{HintSlot, ScrollHint, detect_shift};
use acs::AcsFilter;
use keys::encode_key;
pub use pty::{Pty, PtyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Widget};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::num::NonZeroU16;
use std::sync::Arc;

pub struct ShellConfig {
    pub command: String,
    pub args: Vec<String>,
    pub scrollback: usize,
}

type Events = Arc<dyn Fn(PtyEvent) + Send + Sync>;

pub struct ShellTab {
    cfg: ShellConfig,
    hints: HintSlot,
    events: Events,
    pty: Option<Pty>,
    parser: vt100::Parser<Replies>,
    /// `^] p` is on: the real terminal sees the PTY's queries and answers them.
    passthrough: bool,
    acs: AcsFilter,
    /// Where the virtual screen was last drawn.
    pane: Rect,
    prev_hashes: Vec<u64>,
    scrollback_mode: bool,
    exited: bool,
    error: Option<String>,
    activity: bool,
}

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).unwrap()
}

/// Primary Device Attributes reply: a VT100 with Advanced Video, which is
/// what the WY-60's VT100 personality is. fish 4 waits 10 s for this.
const DA1_REPLY: &[u8] = b"\x1b[?1;2c";

/// Answers terminal queries the shell sends (only DA1 so far). Replies are
/// collected while parsing and written to the PTY afterwards.
#[derive(Default)]
struct Replies(Vec<u8>);

impl vt100::Callbacks for Replies {
    fn unhandled_csi(&mut self, _: &mut vt100::Screen, i1: Option<u8>, i2: Option<u8>, params: &[&[u16]], c: char) {
        let default_param = matches!(params, [] | [[0]]);
        if c == 'c' && i1.is_none() && i2.is_none() && default_param {
            self.0.extend_from_slice(DA1_REPLY);
        }
    }
}

fn new_parser(rows: u16, cols: u16, scrollback: usize) -> vt100::Parser<Replies> {
    vt100::Parser::new_with_callbacks(nz(rows), nz(cols), scrollback, Replies::default())
}

fn is_bright(c: vt100::Color) -> bool {
    match c {
        vt100::Color::Idx(8..=15) => true,
        vt100::Color::Rgb(r, g, b) => 299 * r as u32 + 587 * g as u32 + 114 * b as u32 > 127_500,
        _ => false,
    }
}

/// Monochrome rendering of a vt100 cell: colours become reverse / bold.
fn modifier(cell: &vt100::Cell) -> Modifier {
    let mut m = Modifier::empty();
    if cell.bold() || is_bright(cell.fgcolor()) {
        m |= Modifier::BOLD;
    }
    if cell.dim() {
        m |= Modifier::DIM;
    }
    if cell.underline() {
        m |= Modifier::UNDERLINED;
    }
    if cell.inverse() != (cell.bgcolor() != vt100::Color::Default) {
        m |= Modifier::REVERSED;
    }
    m
}

impl ShellTab {
    /// The shell starts on the first `render`, at that pane's size.
    pub fn new(cfg: ShellConfig, hints: HintSlot, events: impl Fn(PtyEvent) + Send + Sync + 'static) -> Self {
        let parser = new_parser(24, 80, cfg.scrollback);
        Self {
            cfg,
            hints,
            events: Arc::new(events),
            pty: None,
            parser,
            passthrough: false,
            acs: AcsFilter::new(),
            pane: Rect::default(),
            prev_hashes: Vec::new(),
            scrollback_mode: false,
            exited: false,
            error: None,
            activity: false,
        }
    }

    pub fn on_output(&mut self, bytes: &[u8]) {
        let filtered = self.acs.filter(bytes);
        self.parser.process(&filtered);
        let replies = std::mem::take(&mut self.parser.callbacks_mut().0);
        if !replies.is_empty()
            && !self.passthrough
            && let Some(p) = &mut self.pty
        {
            let _ = p.write(&replies);
        }
        self.activity = true;
    }

    pub fn on_exit(&mut self) {
        self.exited = true;
        self.pty = None;
        self.activity = true;
    }

    pub fn enter_scrollback(&mut self) {
        self.scrollback_mode = true;
    }

    /// Virtual screen size as (rows, cols).
    pub fn size(&self) -> (u16, u16) {
        let (r, c) = self.parser.screen().size();
        (r.get(), c.get())
    }

    /// Where the WY-60 cursor belongs, in screen coordinates.
    pub fn cursor(&self) -> Option<Position> {
        let s = self.parser.screen();
        if self.scrollback_mode || self.pty.is_none() || s.hide_cursor() {
            return None;
        }
        let (row, col) = s.cursor_position();
        (row < self.pane.height && col < self.pane.width)
            .then(|| Position { x: self.pane.x + col, y: self.pane.y + row })
    }

    fn spawn(&mut self) {
        let (rows, cols) = (self.pane.height, self.pane.width);
        self.parser = new_parser(rows, cols, self.cfg.scrollback);
        self.acs = AcsFilter::new();
        let events = self.events.clone();
        match Pty::spawn(&self.cfg.command, &self.cfg.args, cols, rows, move |e| events(e)) {
            Ok(p) => {
                self.pty = Some(p);
                self.error = None;
            }
            Err(e) => {
                tracing::error!("shell: {e:#}");
                self.error = Some(format!("{e:#}").to_uppercase());
                self.exited = true;
            }
        }
    }

    fn resize_to_pane(&mut self) {
        if self.size() != (self.pane.height, self.pane.width) {
            self.parser.screen_mut().set_size(nz(self.pane.height), nz(self.pane.width));
            if let Some(p) = &self.pty {
                let _ = p.resize(self.pane.width, self.pane.height);
            }
            self.prev_hashes.clear();
        }
    }

    fn render_message(&self, buf: &mut Buffer, lines: Vec<Line<'static>>) {
        let h = lines.len() as u16;
        let r = Rect { y: self.pane.y + self.pane.height.saturating_sub(h) / 2, height: h.min(self.pane.height), ..self.pane };
        Paragraph::new(lines).alignment(Alignment::Center).render(r, buf);
    }

    fn copy_screen(&mut self, buf: &mut Buffer) {
        let screen = self.parser.screen();
        let mut hashes = Vec::with_capacity(self.pane.height as usize);
        for row in 0..self.pane.height {
            let mut h = DefaultHasher::new();
            for col in 0..self.pane.width {
                let Some(cell) = screen.cell(row, col) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let sym = match cell.contents() {
                    "" => " ",
                    s => s,
                };
                let m = modifier(cell);
                (sym, m.bits()).hash(&mut h);
                buf[(self.pane.x + col, self.pane.y + row)].set_symbol(sym).set_style(Style::new().add_modifier(m));
            }
            hashes.push(h.finish());
        }
        if hashes.len() == self.prev_hashes.len()
            && let Some(lines) = detect_shift(&self.prev_hashes, &hashes)
        {
            self.hints.set(Some(ScrollHint { top: self.pane.y, bottom: self.pane.y + self.pane.height, lines }));
        }
        self.prev_hashes = hashes;
    }
}

impl Tab for ShellTab {
    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        // The windowed content pane never starts at the origin; fullscreen does.
        let fullscreen = area.x == 0 && area.y == 0;
        let pane = if fullscreen {
            area
        } else {
            let offset = self.parser.screen().scrollback();
            let title = if self.scrollback_mode { format!(" SHELL - SCROLLBACK -{offset} ") } else { " SHELL ".into() };
            let block = Block::bordered().title(title);
            let inner = block.inner(area);
            block.render(area, buf);
            inner
        };
        if pane != self.pane {
            self.prev_hashes.clear();
        }
        self.pane = pane;

        if self.exited {
            let mut lines = vec![Line::from("SHELL TERMINATED - PRESS RETURN TO RESTART")];
            if let Some(e) = &self.error {
                lines.insert(0, Line::from(e.clone()));
            }
            self.render_message(buf, lines);
            self.prev_hashes.clear();
            return;
        }
        if self.pty.is_none() {
            self.spawn();
            if self.exited {
                return self.render(area, buf);
            }
        }
        self.resize_to_pane();
        self.copy_screen(buf);
        if fullscreen && self.scrollback_mode {
            // No border to carry the title: tag the top-right corner instead.
            let tag = format!(" SCROLLBACK -{} ", self.parser.screen().scrollback());
            let x = pane.x + pane.width.saturating_sub(tag.len() as u16);
            buf.set_stringn(x, pane.y, &tag, pane.width as usize, Style::new().add_modifier(Modifier::REVERSED));
        }
    }

    fn handle_key(&mut self, key: &Key) {
        if self.scrollback_mode {
            let page = self.pane.height.max(1) as usize;
            let cur = self.parser.screen().scrollback();
            let next = match key {
                Key::PageUp => cur + page,
                Key::Up => cur + 1,
                Key::PageDown => cur.saturating_sub(page),
                Key::Down => cur.saturating_sub(1),
                _ => {
                    self.scrollback_mode = false;
                    0
                }
            };
            self.parser.screen_mut().set_scrollback(next);
            return;
        }
        if self.exited {
            if *key == Key::Enter {
                self.exited = false;
                self.error = None;
                self.pty = None; // respawned on the next render
            }
            return;
        }
        let app_cursor = self.parser.screen().application_cursor();
        if let (Some(p), Some(bytes)) = (&mut self.pty, encode_key(key, app_cursor)) {
            let _ = p.write(&bytes);
        }
    }

    fn take_activity(&mut self) -> bool {
        std::mem::take(&mut self.activity)
    }

    fn cursor(&self) -> Option<Position> {
        ShellTab::cursor(self)
    }

    fn on_pty(&mut self, ev: PtyEvent) {
        match ev {
            PtyEvent::Output(b) => self.on_output(&b),
            PtyEvent::Exited => self.on_exit(),
        }
    }

    fn enter_scrollback(&mut self) -> bool {
        ShellTab::enter_scrollback(self);
        true
    }

    fn supports_passthrough(&self) -> bool {
        true
    }

    fn set_passthrough(&mut self, on: bool) {
        self.passthrough = on;
    }
}

#[cfg(test)]
mod tests;
