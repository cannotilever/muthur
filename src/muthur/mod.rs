//! MU/TH/UR: the LLM chat tab.
pub mod text;
pub mod reveal;
pub mod view;

use crate::app::tabs::Tab;
use crate::chatlog::{Assistant, ChatLog};
use crate::config::MuthurSection;
use crate::input::Key;
use crate::llm::{ChatRequest, LlmError, Stop, StreamEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Widget};
use reveal::Reveal;
use std::time::{Duration, Instant};
use text::{ascii_safe, display_text, Cleaner};
use view::{put, separator, transcript_lines, Entry};

pub const PERSONA: &str = r#"You are MU/TH/UR 6000, the mainframe computer of the {SHIP}. "The ship" means the server you run on.
Answer in short, declarative statements, like a 1970s ship computer printing to a terminal.
Use plain text only: no markdown, no headings, no bold, no code blocks. If a list is needed, put each item on its own short line.
No greetings, pleasantries, apologies or filler.
If you cannot or will not do something, reply "UNABLE TO COMPLY." followed by a short reason.
If you do not know, reply "DATA INSUFFICIENT." Never invent readings.
You do not yet have access to the ship's sensors or systems. Do not claim to have checked anything on the ship."#;

const NOTE_INTERRUPTED: &str = "[INTERRUPTED]";
const NOTE_TRUNCATED: &str = "[TRANSMISSION TRUNCATED]";
const HELP: &str = "/PURGE  CLEAR MEMORY BANKS\n/HELP   THIS LIST\n^C     ABORT TRANSMISSION\nPGUP/PGDN SCROLL";
const STAND_BY: &str = "PROCESSING — STAND BY";
const TICK: Duration = Duration::from_millis(100);

/// The configured system prompt (blank counts as unset) or `PERSONA`, with "{SHIP}" filled in.
pub fn system_prompt(cfg: &MuthurSection, ship: &str) -> String {
    cfg.system_prompt().unwrap_or(PERSONA).replace("{SHIP}", ship)
}

fn unable(reason: &str) -> String {
    format!("UNABLE TO COMPLY — {reason}")
}

/// Drop the `[SHIP TIME …] ` prefix a logged user line carries.
fn strip_ship_time(s: &str) -> &str {
    match s.find("] ") {
        Some(i) if s.starts_with("[SHIP TIME ") => &s[i + 2..],
        _ => s,
    }
}

pub type Abort = Box<dyn FnOnce()>;

pub struct MuthurDeps {
    /// Start a streaming request; the returned closure cancels it.
    pub spawn: Box<dyn FnMut(u64, ChatRequest) -> Abort>,
    pub now: Box<dyn Fn() -> Instant>,
    /// e.g. "18 NOV 2137 14:02".
    pub ship_time: Box<dyn Fn() -> String>,
    /// RFC 3339, for the log.
    pub timestamp: Box<dyn Fn() -> String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ready,
    /// Request sent, no text yet.
    Waiting,
    Streaming,
    /// `/PURGE` asked; waiting for Y.
    Confirm,
}

pub struct MuthurTab {
    cfg: MuthurSection,
    system: String,
    log: ChatLog,
    deps: MuthurDeps,
    transcript: Vec<Entry>,
    input: String,
    /// Cursor position in `input`, in chars.
    cursor: usize,
    state: State,
    reveal: Reveal,
    cleaner: Cleaner,
    status_flash: Option<String>,
    /// Lines up from the bottom.
    scroll: usize,
    req_id: u64,
    abort: Option<Abort>,
    partial_raw: String,
    activity: bool,
    /// The reveal holds a reply not yet moved into the transcript.
    revealing: bool,
    /// Notes to add after the revealing reply lands.
    pending_notes: Vec<String>,
    started: Option<Instant>,
    /// Geometry from the last render.
    page: usize,
    max_scroll: usize,
    cursor_pos: Option<Position>,
}

impl MuthurTab {
    pub fn new(cfg: MuthurSection, ship: &str, log: ChatLog, deps: MuthurDeps) -> Self {
        let mut transcript = Vec::new();
        for ex in log.history() {
            transcript.push(Entry::User(strip_ship_time(&ex.user).to_string()));
            let Some((text, stop)) = ex.reply else { continue };
            if stop == "error" {
                continue;
            }
            let shown = display_text(&text, cfg.uppercase);
            let shown = shown.trim_end_matches('\n');
            if !shown.is_empty() {
                transcript.push(Entry::Reply(shown.to_string()));
            }
            match stop.as_str() {
                "interrupted" => transcript.push(Entry::Note(NOTE_INTERRUPTED.into())),
                "max_tokens" => transcript.push(Entry::Note(NOTE_TRUNCATED.into())),
                _ => {}
            }
        }
        Self {
            system: system_prompt(&cfg, ship),
            reveal: Reveal::new(cfg.reveal_cps),
            cfg,
            log,
            deps,
            transcript,
            input: String::new(),
            cursor: 0,
            state: State::Ready,
            cleaner: Cleaner::new(),
            status_flash: None,
            scroll: 0,
            req_id: 0,
            abort: None,
            partial_raw: String::new(),
            activity: false,
            revealing: false,
            pending_notes: Vec::new(),
            started: None,
            page: 1,
            max_scroll: 0,
            cursor_pos: None,
        }
    }

    fn busy(&self) -> bool {
        matches!(self.state, State::Waiting | State::Streaming)
    }

    fn display(&self, s: &str) -> String {
        ascii_safe(&if self.cfg.uppercase { s.to_uppercase() } else { s.to_string() })
    }

    /// Once idle and fully revealed, move the reply (and its notes) into the transcript.
    fn settle(&mut self) {
        if !self.revealing || self.state != State::Ready || !self.reveal.caught_up() {
            return;
        }
        let text = self.reveal.visible().trim_end_matches('\n').to_string();
        if !text.is_empty() {
            self.transcript.push(Entry::Reply(text));
        }
        self.transcript.extend(self.pending_notes.drain(..).map(Entry::Note));
        self.reveal.reset();
        self.revealing = false;
    }

    /// Show the rest of any finished reply at once, so new entries land after it.
    fn finish_reveal(&mut self) {
        if self.revealing && self.state == State::Ready {
            self.reveal.skip();
            self.settle();
        }
    }

    fn assistant(&self, model: Option<String>, stop: &str, raw: Option<Vec<serde_json::Value>>) -> Assistant {
        Assistant {
            provider: self.cfg.provider,
            model: model.or_else(|| self.cfg.model().map(str::to_string)).unwrap_or_default(),
            text: self.partial_raw.clone(),
            stop: stop.into(),
            raw,
        }
    }

    fn log_assistant(&mut self, a: Assistant) {
        let ts = (self.deps.timestamp)();
        if let Err(e) = self.log.append_assistant(a, &ts) {
            tracing::error!("chat log: cannot record reply: {e}");
        }
    }

    fn flush_cleaner(&mut self) {
        let tail = self.cleaner.finish();
        let s = self.display(&tail);
        self.reveal.push(&s, (self.deps.now)());
    }

    /// End the request early: an error (`Some`) or a Ctrl-C abort (`None`).
    fn end_early(&mut self, err: Option<LlmError>) {
        self.flush_cleaner();
        self.reveal.skip();
        if !self.partial_raw.is_empty() || err.is_none() {
            let a = self.assistant(None, "interrupted", None);
            self.log_assistant(a);
            self.pending_notes.push(NOTE_INTERRUPTED.into());
        } else {
            let a = self.assistant(None, "error", None);
            self.log_assistant(a);
        }
        if let Some(e) = err {
            self.status_flash = Some(unable(e.reason()));
            self.activity = true;
        }
        self.abort = None;
        self.state = State::Ready;
        self.settle();
    }

    fn enter(&mut self) {
        self.scroll = 0;
        let line = self.input.trim().to_string();
        if line.is_empty() {
            return;
        }
        if self.busy() {
            self.status_flash = Some(STAND_BY.into());
            return;
        }
        let cmd = line.to_uppercase();
        if cmd == "/PURGE" {
            self.clear_input();
            self.state = State::Confirm;
        } else if cmd == "/HELP" {
            self.clear_input();
            self.finish_reveal();
            self.transcript.push(Entry::Note(HELP.into()));
        } else if line.starts_with('/') && !line.contains(char::is_whitespace) {
            self.status_flash = Some("UNKNOWN COMMAND".into());
        } else {
            self.submit(line);
        }
    }

    fn submit(&mut self, line: String) {
        if let Err(e) = self.cfg.readiness() {
            self.status_flash = Some(unable(e.reason()));
            return;
        }
        self.finish_reveal();
        let logged = format!("[SHIP TIME {}] {line}", (self.deps.ship_time)());
        let ts = (self.deps.timestamp)();
        if let Err(e) = self.log.append_user(&logged, &ts) {
            tracing::error!("chat log: cannot record inquiry: {e}");
            self.status_flash = Some(unable(LlmError::Transmission.reason()));
            return;
        }
        self.transcript.push(Entry::User(line));
        self.clear_input();
        self.req_id += 1;
        let messages = match self.log.window(self.cfg.provider, &self.system, self.cfg.context_tokens) {
            Ok(m) => m,
            Err(e) => {
                tracing::error!("chat log: cannot build message window: {e}");
                self.status_flash = Some(unable(LlmError::Transmission.reason()));
                return;
            }
        };
        let req = ChatRequest { system: self.system.clone(), messages };
        self.abort = Some((self.deps.spawn)(self.req_id, req));
        self.state = State::Waiting;
        self.started = Some((self.deps.now)());
        self.reveal.reset();
        self.cleaner = Cleaner::new();
        self.partial_raw.clear();
        self.pending_notes.clear();
        self.revealing = true;
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.input.char_indices().nth(char_idx).map_or(self.input.len(), |(i, _)| i)
    }

    fn insert(&mut self, c: char) {
        let chars: Vec<char> = if self.cfg.uppercase { c.to_uppercase().collect() } else { vec![c] };
        for ch in chars {
            let at = self.byte_at(self.cursor);
            self.input.insert(at, ch);
            self.cursor += 1;
        }
    }

    fn delete_at(&mut self, char_idx: usize) {
        if char_idx < self.input.chars().count() {
            let at = self.byte_at(char_idx);
            self.input.remove(at);
        }
    }

    fn confirm_key(&mut self, key: &Key) {
        self.state = State::Ready;
        if matches!(key, Key::Char('y' | 'Y')) {
            if let Err(e) = self.log.purge() {
                tracing::error!("chat log: purge failed: {e}");
                self.status_flash = Some(unable(LlmError::Transmission.reason()));
                return;
            }
            self.transcript.clear();
            self.reveal.reset();
            self.revealing = false;
            self.pending_notes.clear();
            self.scroll = 0;
        }
    }

    fn status_text(&self, now: Instant) -> (String, bool) {
        if self.state == State::Confirm {
            return ("CONFIRM Y/N".into(), true);
        }
        if let Some(f) = &self.status_flash {
            return (ascii_safe(f), true);
        }
        let s = match self.state {
            State::Waiting => {
                let secs = self.started.map_or(0, |t| now.saturating_duration_since(t).as_secs());
                format!("PROCESSING{}", ".".repeat(1 + (secs % 3) as usize))
            }
            State::Streaming => "RECEIVING".into(),
            _ => "READY FOR INQUIRY".into(),
        };
        (s, false)
    }
}

impl Tab for MuthurTab {
    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let now = (self.deps.now)();
        self.reveal.advance(now);
        self.settle();

        let block = Block::bordered().title(" MU/TH/UR 6000 ");
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height < 3 || inner.width < 3 {
            self.cursor_pos = None;
            return;
        }
        let x = inner.x + 1;
        let w = inner.width - 2;
        let rows = inner.height - 3;
        let sep_y = inner.y + rows;
        let status_y = sep_y + 1;
        let input_y = sep_y + 2;

        // Transcript.
        let revealing = self.revealing.then(|| self.reveal.visible());
        let lines = transcript_lines(&self.transcript, revealing, w);
        let h = usize::from(rows);
        self.page = h.saturating_sub(1).max(1);
        self.max_scroll = lines.len().saturating_sub(h);
        self.scroll = self.scroll.min(self.max_scroll);
        let end = lines.len() - self.scroll;
        let start = end.saturating_sub(h);
        for (i, l) in lines[start..end].iter().enumerate() {
            put(buf, x, inner.y + i as u16, w, l, Style::default());
        }

        separator(buf, area, sep_y);

        // Status row.
        let (status, reverse) = self.status_text(now);
        let style = if reverse { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
        put(buf, x, status_y, w, &status, style);
        if self.scroll > 0 {
            let marker = format!("[SCROLL -{}]", self.scroll);
            let mw = marker.len() as u16;
            if mw <= w {
                put(buf, x + w - mw, status_y, mw, &marker, Style::default());
            }
        }

        // Input row: "> " then the input, scrolled so the cursor stays visible.
        put(buf, x, input_y, w, "> ", Style::default());
        let avail = usize::from(w.saturating_sub(2));
        if avail == 0 {
            self.cursor_pos = None;
            return;
        }
        let off = (self.cursor + 1).saturating_sub(avail);
        let shown: String = self.input.chars().skip(off).take(avail).collect();
        put(buf, x + 2, input_y, avail as u16, &ascii_safe(&shown), Style::default());
        self.cursor_pos = Some(Position::new(x + 2 + (self.cursor - off) as u16, input_y));
    }

    fn handle_key(&mut self, key: &Key) {
        self.status_flash = None;
        if self.state == State::Confirm {
            self.confirm_key(key);
            return;
        }
        match key {
            Key::Char(c) if !c.is_control() => self.insert(*c),
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.delete_at(self.cursor);
                }
            }
            Key::DeleteChar | Key::Pf(2) => self.delete_at(self.cursor),
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.input.chars().count()),
            Key::Ctrl('u') => self.clear_input(),
            Key::Ctrl('c') => {
                if self.busy() {
                    if let Some(abort) = self.abort.take() {
                        abort();
                    }
                    self.end_early(None);
                }
            }
            Key::PageUp => self.scroll = (self.scroll + self.page).min(self.max_scroll),
            Key::PageDown => self.scroll = self.scroll.saturating_sub(self.page),
            Key::Enter => self.enter(),
            _ => {
                if self.revealing {
                    self.reveal.skip();
                    self.settle();
                }
            }
        }
    }

    fn take_activity(&mut self) -> bool {
        std::mem::take(&mut self.activity)
    }

    fn cursor(&self) -> Option<Position> {
        self.cursor_pos
    }

    fn on_llm(&mut self, req_id: u64, ev: StreamEvent) {
        if req_id != self.req_id || !self.busy() {
            return;
        }
        match ev {
            StreamEvent::Text(t) => {
                self.partial_raw.push_str(&t);
                let cleaned = self.cleaner.push(&t);
                let s = self.display(&cleaned);
                self.reveal.push(&s, (self.deps.now)());
                self.state = State::Streaming;
            }
            StreamEvent::ToolCall { .. } => {}
            StreamEvent::Done { stop, model, raw } => {
                self.flush_cleaner();
                let a = self.assistant(model, stop.log_str(), raw);
                self.log_assistant(a);
                match stop {
                    Stop::Refusal => self.status_flash = Some(unable(LlmError::Restricted.reason())),
                    Stop::MaxTokens => self.pending_notes.push(NOTE_TRUNCATED.into()),
                    _ => {}
                }
                self.abort = None;
                self.state = State::Ready;
                self.activity = true;
                self.settle();
            }
            StreamEvent::Error(e) => self.end_early(Some(e)),
        }
    }

    fn tick_interval(&self) -> Option<Duration> {
        (!self.reveal.caught_up() || self.state == State::Waiting).then_some(TICK)
    }
}

#[cfg(test)]
mod tests;
