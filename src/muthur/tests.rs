use super::view::wrap;
use super::*;
use crate::chatlog::{Assistant, ChatLog};
use crate::config::{MuthurSection, Provider, ServiceLists, ToolsSection};
use crate::input::Key;
use crate::llm::{ChatRequest, LlmError, MessageContent, Stop, StreamEvent, ToolResult};
use crate::tools::audit::AuditLog;
use crate::tools::exec::ToolOutcome;
use crate::tools::{Invocation, ToolDefs, Verb};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use serde_json::json;
use std::time::{Duration, Instant};

type Sent = Rc<RefCell<Vec<(u64, ChatRequest)>>>;
type Ran = Rc<RefCell<Vec<(u64, String, Invocation)>>>;

struct H {
    t: MuthurTab,
    sent: Sent,
    ran: Ran,
    aborted: Rc<Cell<bool>>,
    clock: Rc<Cell<Instant>>,
    path: PathBuf,
    audit_path: PathBuf,
    _dir: tempfile::TempDir,
}

impl H {
    fn tick(&self, ms: u64) {
        self.clock.set(self.clock.get() + Duration::from_millis(ms));
    }
    fn reopen(&self) -> ChatLog {
        ChatLog::open(self.path.clone()).unwrap()
    }
}

fn cfg() -> MuthurSection {
    MuthurSection { api_key: Some("sk-test".into()), ..MuthurSection::default() }
}

fn tools_cfg() -> ToolsSection {
    ToolsSection { services: ServiceLists { restart: vec!["nginx".into()], ..Default::default() }, ..Default::default() }
}

fn harness_with(cfg: MuthurSection, prefill: impl FnOnce(&mut ChatLog)) -> H {
    harness_full(cfg, tools_cfg(), true, prefill)
}

fn harness_full(cfg: MuthurSection, tools: ToolsSection, audit_online: bool, prefill: impl FnOnce(&mut ChatLog)) -> H {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("muthur/chat.jsonl");
    let audit_path = dir.path().join("muthur/audit.jsonl");
    let mut log = ChatLog::open(path.clone()).unwrap();
    prefill(&mut log);
    let audit = if audit_online { AuditLog::open(audit_path.clone()).unwrap() } else { AuditLog::offline() };
    let sent: Sent = Rc::default();
    let ran: Ran = Rc::default();
    let aborted = Rc::new(Cell::new(false));
    let clock = Rc::new(Cell::new(Instant::now()));
    let (s, a, c, r) = (sent.clone(), aborted.clone(), clock.clone(), ran.clone());
    let deps = MuthurDeps {
        spawn: Box::new(move |id, req| {
            s.borrow_mut().push((id, req));
            let a = a.clone();
            Box::new(move || a.set(true))
        }),
        now: Box::new(move || c.get()),
        ship_time: Box::new(|| "18 NOV 2137 14:02".into()),
        timestamp: Box::new(|| "2026-11-18T14:02:00Z".into()),
        run_tool: Box::new(move |req, id, inv| r.borrow_mut().push((req, id, inv))),
    };
    let t = MuthurTab::new(cfg, ToolDefs::new(&tools), "USCSS NOSTROMO", log, audit, deps);
    H { t, sent, ran, aborted, clock, path, audit_path, _dir: dir }
}

fn harness() -> H {
    harness_with(cfg(), |_| {})
}

fn type_str(t: &mut MuthurTab, s: &str) {
    for c in s.chars() {
        t.handle_key(&Key::Char(c));
    }
}

fn submit(h: &mut H, s: &str) {
    type_str(&mut h.t, s);
    h.t.handle_key(&Key::Enter);
}

fn screen(t: &mut MuthurTab, w: u16, h: u16) -> Vec<String> {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    t.render(area, &mut buf);
    (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect()).collect()
}

fn text(t: &mut MuthurTab) -> String {
    screen(t, 61, 40).join("\n")
}

fn done(stop: Stop) -> StreamEvent {
    StreamEvent::Done { stop, model: Some("claude-test".into()), raw: None }
}

fn exchange(log: &mut ChatLog, user: &str, reply: &str, stop: &str) {
    log.append_user(user, "ts").unwrap();
    let a = Assistant { provider: Provider::Anthropic, model: "m".into(), text: reply.into(), stop: stop.into(), raw: None };
    log.append_assistant(a, "ts").unwrap();
}

#[test]
fn wrap_breaks_long_words() {
    assert_eq!(wrap("AB CDEFGHIJ K", 4), vec!["AB", "CDEF", "GHIJ", "K"]);
    assert_eq!(wrap("A\n\nB", 10), vec!["A", "", "B"]);
}

#[test]
fn system_prompt_override_and_ship() {
    let s = system_prompt(&cfg(), "USCSS NOSTROMO");
    assert!(s.starts_with("You are MU/TH/UR 6000, the mainframe computer of the USCSS NOSTROMO."), "{s}");
    let blank = MuthurSection { system_prompt: Some("  ".into()), ..cfg() };
    assert_eq!(system_prompt(&blank, "X"), PERSONA.replace("{SHIP}", "X"));
    let own = MuthurSection { system_prompt: Some("Computer of {SHIP}.".into()), ..cfg() };
    assert_eq!(system_prompt(&own, "X"), "Computer of X.");
}

#[test]
fn submit_logs_with_ship_time_and_spawns() {
    let mut h = harness();
    type_str(&mut h.t, "status");
    h.t.handle_key(&Key::Enter);
    let s = h.sent.borrow();
    assert_eq!(s.len(), 1);
    let MessageContent::Text(u) = &s[0].1.messages.last().unwrap().content else { panic!() };
    assert_eq!(u, "[SHIP TIME 18 NOV 2137 14:02] STATUS");
    assert!(s[0].1.system.contains("MU/TH/UR 6000"));
}

#[test]
fn blank_enter_is_ignored() {
    let mut h = harness();
    type_str(&mut h.t, "   ");
    h.t.handle_key(&Key::Enter);
    assert!(h.sent.borrow().is_empty());
}

#[test]
fn reply_reveals_over_time_and_logs() {
    let mut h = harness();
    submit(&mut h, "status");
    h.t.on_llm(1, StreamEvent::Text("All **good**.".into()));
    h.tick(100);
    let s = text(&mut h.t);
    assert!(s.contains("ALL G"), "{s}");
    assert!(!s.contains("ALL GOOD."), "{s}");
    h.t.on_llm(1, done(Stop::End));
    h.tick(1000);
    let s = text(&mut h.t);
    assert!(s.contains("ALL GOOD."), "{s}");
    let hist = h.reopen().history();
    assert_eq!(hist[0].reply, Some(("All **good**.".into(), "end_turn".into())));
}

#[test]
fn typing_during_reply_does_not_skip_but_esc_does() {
    let mut h = harness();
    submit(&mut h, "status");
    h.t.on_llm(1, StreamEvent::Text("ABCDEFGHIJ".into()));
    type_str(&mut h.t, "X");
    assert_eq!(h.t.reveal.visible(), "");
    h.t.handle_key(&Key::Esc);
    assert_eq!(h.t.reveal.visible(), "ABCDEFGHIJ");
    let rows = screen(&mut h.t, 61, 40);
    assert!(rows[38].starts_with("│ > X "), "{}", rows[38]);
}

#[test]
fn enter_while_busy_flashes() {
    let mut h = harness();
    submit(&mut h, "status");
    type_str(&mut h.t, "Y");
    h.t.handle_key(&Key::Enter);
    assert_eq!(h.sent.borrow().len(), 1);
    let rows = screen(&mut h.t, 61, 40);
    assert!(rows[37].contains("PROCESSING -- STAND BY"), "{}", rows[37]);
}

#[test]
fn ctrl_c_aborts_and_marks_interrupted() {
    let mut h = harness();
    submit(&mut h, "status");
    h.t.on_llm(1, StreamEvent::Text("PART".into()));
    h.t.handle_key(&Key::Ctrl('c'));
    assert!(h.aborted.get());
    let s = text(&mut h.t);
    assert!(s.contains("[INTERRUPTED]"), "{s}");
    assert!(s.contains("PART"), "{s}");
    assert_eq!(h.reopen().history()[0].reply, Some(("PART".into(), "interrupted".into())));
    h.t.on_llm(1, StreamEvent::Text("MORE".into()));
    h.tick(1000);
    assert!(!text(&mut h.t).contains("MORE"));
}

#[test]
fn ctrl_c_when_idle_does_nothing() {
    let mut h = harness();
    h.t.handle_key(&Key::Ctrl('c'));
    assert!(!h.aborted.get());
    assert!(h.reopen().history().is_empty());
}

#[test]
fn error_after_partial_keeps_text() {
    let mut h = harness();
    submit(&mut h, "status");
    h.t.on_llm(1, StreamEvent::Text("HALF".into()));
    h.t.on_llm(1, StreamEvent::Error(LlmError::SignalLost));
    let s = text(&mut h.t);
    assert!(s.contains("HALF"), "{s}");
    assert!(s.contains("UNABLE TO COMPLY -- SIGNAL LOST"), "{s}");
    assert_eq!(h.reopen().history()[0].reply, Some(("HALF".into(), "interrupted".into())));
}

#[test]
fn error_without_text_logs_error_pair() {
    let mut h = harness();
    submit(&mut h, "first");
    h.t.on_llm(1, StreamEvent::Error(LlmError::Auth));
    assert_eq!(h.reopen().history()[0].reply, Some(("".into(), "error".into())));
    assert!(text(&mut h.t).contains("UNABLE TO COMPLY -- AUTHORIZATION REJECTED"));
    submit(&mut h, "second");
    let s = h.sent.borrow();
    assert_eq!(s.len(), 2);
    assert_eq!(s[1].1.messages.len(), 1, "failed pair must be left out");
}

#[test]
fn stale_req_id_ignored() {
    let mut h = harness();
    submit(&mut h, "one");
    h.t.on_llm(1, StreamEvent::Text("FIRST".into()));
    h.t.on_llm(1, done(Stop::End));
    submit(&mut h, "two");
    assert_eq!(h.sent.borrow()[1].0, 2);
    h.t.on_llm(1, StreamEvent::Text("OLD".into()));
    assert!(h.t.partial_raw.is_empty());
    h.tick(1000);
    assert!(!text(&mut h.t).contains("OLD"));
}

#[test]
fn purge_requires_confirmation() {
    let mut h = harness_with(cfg(), |l| exchange(l, "[SHIP TIME X] HI", "Hello", "end_turn"));
    submit(&mut h, "/purge");
    assert!(text(&mut h.t).contains("CONFIRM Y/N"));
    h.t.handle_key(&Key::Char('n'));
    let s = text(&mut h.t);
    assert!(!s.contains("CONFIRM Y/N") && s.contains("> HI"), "{s}");
    assert_eq!(h.reopen().history().len(), 1);
    submit(&mut h, "/PURGE");
    h.t.handle_key(&Key::Char('Y'));
    let s = text(&mut h.t);
    assert!(!s.contains("> HI") && s.contains("INTERFACE 2037 READY."), "{s}");
    assert!(!h.path.exists());
}

#[test]
fn help_and_unknown_command() {
    let mut h = harness();
    submit(&mut h, "/help");
    let s = text(&mut h.t);
    assert!(s.contains("/PURGE  CLEAR MEMORY BANKS") && s.contains("PGUP/PGDN SCROLL"), "{s}");
    h.t.handle_key(&Key::Ctrl('u'));
    submit(&mut h, "/frob");
    assert!(text(&mut h.t).contains("UNKNOWN COMMAND"));
    assert!(h.sent.borrow().is_empty());
}

#[test]
fn not_ready_flashes_no_access_code() {
    let mut h = harness_with(MuthurSection::default(), |_| {});
    submit(&mut h, "status");
    assert!(h.sent.borrow().is_empty());
    assert!(h.reopen().history().is_empty());
    assert!(text(&mut h.t).contains("NO ACCESS CODE"));
}

#[test]
fn history_redisplays_on_open() {
    let mut h = harness_with(cfg(), |l| exchange(l, "[SHIP TIME X] HI", "**Hello**", "end_turn"));
    assert_eq!(h.t.tick_interval(), None);
    let s = text(&mut h.t);
    assert!(s.contains("> HI") && !s.contains("SHIP TIME"), "{s}");
    assert!(s.contains("HELLO") && !s.contains("**"), "{s}");
    assert!(!s.contains('█'), "{s}");
}

#[test]
fn history_notes_and_error_pairs() {
    let mut h = harness_with(cfg(), |l| {
        exchange(l, "A", "CUT", "interrupted");
        exchange(l, "B", "LONG", "max_tokens");
        exchange(l, "C", "", "error");
    });
    let s = text(&mut h.t);
    assert!(s.contains("[INTERRUPTED]") && s.contains("[TRANSMISSION TRUNCATED]") && s.contains("> C"), "{s}");
}

#[test]
fn scrollback_and_marker() {
    let mut h = harness_with(cfg(), |l| {
        for i in 0..30 {
            exchange(l, &format!("Q{i}"), &format!("REPLY {i}"), "end_turn");
        }
    });
    let rows = screen(&mut h.t, 61, 40);
    assert!(rows[35].contains("REPLY 29"), "{}", rows[35]);
    h.t.handle_key(&Key::PageUp);
    let rows = screen(&mut h.t, 61, 40);
    assert!(rows[37].contains("[SCROLL -"), "{}", rows[37]);
    assert!(rows[37].trim_end_matches('│').trim_end().ends_with(']'), "{}", rows[37]);
    assert!(!rows[35].contains("REPLY 29"), "{}", rows[35]);
    h.t.handle_key(&Key::Enter);
    let rows = screen(&mut h.t, 61, 40);
    assert!(!rows[37].contains("[SCROLL -"));
    assert!(rows[35].contains("REPLY 29"));
}

#[test]
fn many_lines_fit_and_border_intact() {
    let mut h = harness();
    submit(&mut h, "status");
    h.t.on_llm(1, StreamEvent::Text("X".repeat(200)));
    h.t.on_llm(1, done(Stop::End));
    h.t.handle_key(&Key::Esc);
    let rows = screen(&mut h.t, 61, 40);
    for (i, r) in rows.iter().enumerate() {
        assert_eq!(r.chars().count(), 61, "row {i}: {r}");
        assert!(r.starts_with(['│', '┌', '└', '├']), "row {i}: {r}");
    }
    assert!(rows[36].starts_with('├') && rows[36].ends_with('┤'), "{}", rows[36]);
    assert!(rows.iter().any(|r| r.contains(&"X".repeat(57))), "{}", rows.join("\n"));
}

#[test]
fn input_scrolls_and_cursor_follows() {
    let mut h = harness();
    type_str(&mut h.t, &"A".repeat(70));
    let rows = screen(&mut h.t, 61, 40);
    let c = h.t.cursor().unwrap();
    assert_eq!(c.y, 38);
    assert!(c.x < 60, "{c:?}");
    assert!(rows[38].starts_with("│ > AAA"), "{}", rows[38]);
    h.t.handle_key(&Key::Left);
    h.t.handle_key(&Key::Backspace);
    h.t.handle_key(&Key::Pf(2));
    assert_eq!(h.t.input.len(), 68);
}

#[test]
fn tick_interval_only_while_busy() {
    let mut h = harness();
    assert_eq!(h.t.tick_interval(), None);
    submit(&mut h, "status");
    assert_eq!(h.t.tick_interval(), Some(Duration::from_millis(100)));
    h.t.on_llm(1, StreamEvent::Text("SOME TEXT".into()));
    h.t.on_llm(1, done(Stop::End));
    h.t.handle_key(&Key::Esc);
    assert_eq!(h.t.tick_interval(), None);
}

#[test]
fn activity_on_completion() {
    let mut h = harness();
    submit(&mut h, "status");
    assert!(!h.t.take_activity());
    h.t.on_llm(1, StreamEvent::Text("OK".into()));
    h.t.on_llm(1, done(Stop::End));
    assert!(h.t.take_activity());
    assert!(!h.t.take_activity());
}

#[test]
fn refusal_and_truncation() {
    let mut h = harness();
    submit(&mut h, "a");
    h.t.on_llm(1, StreamEvent::Text("NO".into()));
    h.t.on_llm(1, done(Stop::Refusal));
    assert!(text(&mut h.t).contains("UNABLE TO COMPLY -- DIRECTIVE RESTRICTED"));
    submit(&mut h, "b");
    h.t.on_llm(2, StreamEvent::Text("LONG".into()));
    h.t.on_llm(2, done(Stop::MaxTokens));
    h.tick(1000);
    let s = text(&mut h.t);
    let (l, t) = (s.find("LONG").unwrap(), s.find("[TRANSMISSION TRUNCATED]").unwrap());
    assert!(l < t, "{s}");
}

#[test]
fn scrolled_back_view_holds_while_output_arrives() {
    let mut h = harness_with(cfg(), |l| {
        for i in 0..30 {
            exchange(l, &format!("Q{i}"), &format!("REPLY {i}"), "end_turn");
        }
    });
    submit(&mut h, "status");
    screen(&mut h.t, 61, 40);
    h.t.handle_key(&Key::PageUp);
    let before = screen(&mut h.t, 61, 40);
    let n = |r: &str| r.split("[SCROLL -").nth(1).unwrap().split(']').next().unwrap().parse::<usize>().unwrap();
    let n0 = n(&before[37]);
    h.t.on_llm(1, StreamEvent::Text("WORD ".repeat(60)));
    h.tick(10_000);
    let after = screen(&mut h.t, 61, 40);
    assert_eq!(before[1..36], after[1..36], "view moved under the reader");
    assert!(n(&after[37]) > n0, "{} vs {}", after[37], before[37]);
}

// ---- M5: tool turns ----

fn ok(content: &str) -> ToolOutcome {
    ToolOutcome { content: content.into(), is_error: false, argv: Some(vec!["systemctl".into()]), exit: Some(json!(0)), ms: 5 }
}

fn tool_call(id: &str, name: &str, input: serde_json::Value) -> StreamEvent {
    StreamEvent::ToolCall { id: id.into(), name: name.into(), input }
}

/// Ends request `req` with a ToolUse stop carrying `calls` (id, name, input).
fn tool_use(h: &mut H, req: u64, calls: &[(&str, &str, serde_json::Value)]) {
    let mut raw = Vec::new();
    for (id, name, input) in calls {
        h.t.on_llm(req, tool_call(id, name, input.clone()));
        raw.push(json!({"type":"tool_use","id":id,"name":name,"input":input}));
    }
    h.t.on_llm(req, StreamEvent::Done { stop: Stop::ToolUse, model: Some("claude-test".into()), raw: Some(raw) });
}

fn restart_prompt(h: &mut H) {
    submit(h, "restart nginx");
    tool_use(h, 1, &[("c1", "restart_service", json!({"unit": "nginx"}))]);
}

fn last_results(h: &H) -> Vec<ToolResult> {
    let s = h.sent.borrow();
    let MessageContent::ToolResults(r) = &s.last().unwrap().1.messages.last().unwrap().content else {
        panic!("{:?}", s.last().unwrap().1.messages.last())
    };
    r.clone()
}

fn records(h: &H) -> Vec<serde_json::Value> {
    std::fs::read_to_string(&h.path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn status_row(h: &mut H) -> String {
    screen(&mut h.t, 80, 40)[37].clone()
}

#[test]
fn tool_round_runs_and_respawns() {
    let mut h = harness();
    submit(&mut h, "is nginx running");
    assert!(!h.sent.borrow()[0].1.tools.is_empty());
    assert!(!h.sent.borrow()[0].1.tool_choice_none);
    h.t.on_llm(1, StreamEvent::Text("CHECKING.".into()));
    tool_use(&mut h, 1, &[("c1", "service_status", json!({"unit": "nginx"}))]);
    assert_eq!(*h.ran.borrow(), vec![(1, "c1".to_string(), Invocation::ServiceStatus { unit: "nginx.service".into() })]);
    let s = text(&mut h.t);
    let (c, a) = (s.find("CHECKING.").unwrap(), s.find("ACCESSING: SERVICE STATUS NGINX.SERVICE").unwrap());
    assert!(c < a, "{s}");
    assert!(status_row(&mut h).contains("ACCESSING"));
    assert_eq!(h.t.tick_interval(), Some(Duration::from_millis(100)));
    h.t.on_tool_done(1, "c1".into(), ok("active"));
    {
        let s = h.sent.borrow();
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].0, 2);
        assert!(!s[1].1.tools.is_empty());
        assert!(!s[1].1.tool_choice_none);
    }
    let r = last_results(&h);
    assert_eq!(r.len(), 1);
    assert_eq!((r[0].id.as_str(), r[0].content.as_str()), ("c1", "active"));
    let audit = std::fs::read_to_string(&h.audit_path).unwrap();
    let lines: Vec<&str> = audit.lines().collect();
    assert_eq!(lines.len(), 1);
    let rec: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(rec["decision"], "auto");
    // The final answer lands after the note.
    h.t.on_llm(2, StreamEvent::Text("NGINX ACTIVE.".into()));
    h.t.on_llm(2, done(Stop::End));
    h.tick(2000);
    let s = text(&mut h.t);
    assert!(s.find("ACCESSING: SERVICE").unwrap() < s.find("NGINX ACTIVE.").unwrap(), "{s}");
    assert!(status_row(&mut h).contains("READY FOR INQUIRY"));
}

#[test]
fn override_prompt_confirms_after_arming() {
    let mut h = harness();
    restart_prompt(&mut h);
    assert!(h.t.take_activity());
    let row = status_row(&mut h);
    assert!(row.contains("OVERRIDE REQUIRED -- RESTART NGINX.SERVICE -- CONFIRM Y/N"), "{row}");
    assert!(h.ran.borrow().is_empty());
    assert_eq!(h.t.tick_interval(), Some(Duration::from_millis(100)));
    h.tick(1100);
    h.t.handle_key(&Key::Char('y'));
    assert_eq!(
        *h.ran.borrow(),
        vec![(1, "c1".to_string(), Invocation::Service { verb: Verb::Restart, unit: "nginx.service".into() })]
    );
    assert!(text(&mut h.t).contains("ACCESSING: RESTART NGINX.SERVICE"));
    assert!(status_row(&mut h).contains("ACCESSING"));
}

#[test]
fn override_ignores_keys_during_arming() {
    let mut h = harness();
    restart_prompt(&mut h);
    h.tick(200);
    h.t.handle_key(&Key::Char('y'));
    assert!(h.ran.borrow().is_empty());
    assert!(matches!(h.t.state, State::Override { .. }));
    assert_eq!(h.t.input, "");
    h.tick(900);
    h.t.handle_key(&Key::Char('n'));
    assert!(h.ran.borrow().is_empty());
    assert!(text(&mut h.t).contains("ACCESS DENIED: RESTART NGINX.SERVICE"));
    assert_eq!(h.sent.borrow().len(), 2);
    let r = last_results(&h);
    assert_eq!(r[0].content, "DENIED BY OPERATOR");
    assert!(r[0].is_error);
}

#[test]
fn override_other_keys_not_typed() {
    let mut h = harness();
    restart_prompt(&mut h);
    h.tick(1100);
    h.t.handle_key(&Key::Char('x'));
    assert_eq!(h.t.input, "");
    assert!(matches!(h.t.state, State::Override { .. }));
    assert!(status_row(&mut h).contains("OVERRIDE REQUIRED"));
}

#[test]
fn ctrl_c_during_override_aborts() {
    let mut h = harness();
    restart_prompt(&mut h);
    h.tick(1100);
    h.t.handle_key(&Key::Ctrl('c'));
    assert_eq!(h.t.state, State::Ready);
    assert!(h.ran.borrow().is_empty());
    let recs = records(&h);
    let n = recs.len();
    assert_eq!(recs[n - 2]["kind"], "tool");
    assert_eq!(recs[n - 2]["results"][0]["content"], "ABORTED BY OPERATOR");
    assert_eq!(recs[n - 1]["kind"], "assistant");
    assert_eq!(recs[n - 1]["stop"], "interrupted");
    assert!(text(&mut h.t).contains("[INTERRUPTED]"));
    let audit = std::fs::read_to_string(&h.audit_path).unwrap();
    assert!(audit.contains("\"aborted\""), "{audit}");
    submit(&mut h, "again");
    let s = h.sent.borrow();
    assert_eq!(s.len(), 2);
    assert_eq!(s[1].1.messages.len(), 1, "aborted exchange must be left out: {:?}", s[1].1.messages);
}

#[test]
fn stale_tool_done_ignored() {
    let mut h = harness();
    submit(&mut h, "x");
    h.t.on_llm(1, StreamEvent::Text("A".into()));
    h.t.on_llm(1, done(Stop::End));
    submit(&mut h, "status");
    tool_use(&mut h, 2, &[("c1", "ship_status", json!({}))]);
    h.t.on_tool_done(1, "c1".into(), ok("stale"));
    assert_eq!(h.sent.borrow().len(), 2);
    assert_eq!(h.t.state, State::Tools);
    h.t.on_tool_done(2, "c1".into(), ok("fresh"));
    assert_eq!(h.sent.borrow().len(), 3);
    assert_eq!(last_results(&h)[0].content, "fresh");
}

#[test]
fn round_limit_truncates() {
    let one = ToolsSection { max_rounds: 1, ..tools_cfg() };
    let mut h = harness_full(cfg(), one, true, |_| {});
    submit(&mut h, "status");
    assert!(!h.sent.borrow()[0].1.tool_choice_none);
    tool_use(&mut h, 1, &[("c1", "ship_status", json!({}))]);
    h.t.on_tool_done(1, "c1".into(), ok("ship"));
    assert!(h.sent.borrow()[1].1.tool_choice_none);
    // Tool calls anyway, twice: one more forced request, then truncation.
    tool_use(&mut h, 2, &[("c2", "ship_status", json!({}))]);
    assert_eq!(h.sent.borrow().len(), 3);
    assert!(h.sent.borrow()[2].1.tool_choice_none);
    tool_use(&mut h, 3, &[("c3", "ship_status", json!({}))]);
    assert_eq!(h.sent.borrow().len(), 3);
    assert_eq!(h.t.state, State::Ready);
    h.tick(1000);
    assert!(text(&mut h.t).trim_end().contains("[TRANSMISSION TRUNCATED]"));
    // max_rounds 0: the first request is already final.
    let zero = ToolsSection { max_rounds: 0, ..tools_cfg() };
    let mut h = harness_full(cfg(), zero, true, |_| {});
    submit(&mut h, "status");
    assert!(h.sent.borrow()[0].1.tool_choice_none);
}

#[test]
fn audit_offline_flash() {
    let mut h = harness_full(cfg(), tools_cfg(), false, |_| {});
    submit(&mut h, "status");
    tool_use(&mut h, 1, &[("c1", "service_status", json!({"unit": "nginx"}))]);
    assert_eq!(h.ran.borrow().len(), 1);
    h.t.on_tool_done(1, "c1".into(), ok("active"));
    assert_eq!(h.sent.borrow().len(), 2);
    let row = status_row(&mut h);
    assert!(row.contains("AUDIT LOG OFFLINE"), "{row}");
    tool_use(&mut h, 2, &[("c2", "restart_service", json!({"unit": "nginx"}))]);
    assert!(text(&mut h.t).contains("ACCESS DENIED: RESTART NGINX.SERVICE"));
    assert!(!matches!(h.t.state, State::Override { .. }));
    assert_eq!(h.ran.borrow().len(), 1);
    assert_eq!(h.sent.borrow().len(), 3);
    assert_eq!(last_results(&h)[0].content, "NOT AUTHORIZED: AUDIT LOG OFFLINE");
}

#[test]
fn history_shows_tool_notes() {
    let mut h = harness_with(cfg(), |l| {
        l.append_user("[SHIP TIME X] STATUS", "ts").unwrap();
        let raw = vec![json!({"type":"tool_use","id":"c1","name":"ship_status","input":{}})];
        let a = Assistant { provider: Provider::Anthropic, model: "m".into(), text: "".into(), stop: "tool_use".into(), raw: Some(raw) };
        l.append_assistant(a, "ts").unwrap();
        let r = |id: &str, content: &str, label: &str| ToolResult {
            id: id.into(),
            name: "x".into(),
            content: content.into(),
            is_error: false,
            label: label.into(),
        };
        l.append_tool(&[r("c1", "ok", "SHIP STATUS"), r("c2", "DENIED BY OPERATOR", "RESTART NGINX.SERVICE")], "ts").unwrap();
        let a = Assistant { provider: Provider::Anthropic, model: "m".into(), text: "NOMINAL.".into(), stop: "end_turn".into(), raw: None };
        l.append_assistant(a, "ts").unwrap();
    });
    let rows = screen(&mut h.t, 61, 40);
    let at = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("{needle}: {rows:#?}"));
    let (u, n, d, r) = (at("> STATUS"), at("ACCESSING: SHIP STATUS"), at("ACCESS DENIED: RESTART NGINX.SERVICE"), at("NOMINAL."));
    assert!(u < n && n < d && d < r, "{rows:#?}");
}

#[test]
fn persona_mentions_tools() {
    assert!(PERSONA.contains("operator override"));
    assert!(!PERSONA.contains("do not yet have access"));
}
