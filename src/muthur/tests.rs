use super::view::{wrap, wrap_hanging};
use super::*;
use crate::chatlog::{Assistant, ChatLog, Lane};
use crate::config::{MuthurSection, Provider};
use crate::input::Key;
use crate::llm::{ChatRequest, LlmError, MessageContent, Stop, StreamEvent};
use crate::ops::audit::AuditLog;
use crate::ops::AgentId;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The tool turns, prompts and direct mode of the `agent` build.
#[cfg(feature = "agent")]
mod agentic;

type Sent = Rc<RefCell<Vec<(u64, ChatRequest)>>>;
#[cfg(feature = "agent")]
type Ran = Rc<RefCell<Vec<(u64, String, crate::tools::Invocation)>>>;
#[cfg(feature = "agent")]
type Secrets = Rc<RefCell<Vec<Option<crate::secret::Secret>>>>;
#[cfg(feature = "agent")]
type Cancels = Rc<RefCell<Vec<std::sync::Arc<std::sync::atomic::AtomicBool>>>>;
#[cfg(feature = "agent")]
type Reviews = Rc<RefCell<Vec<(u64, String, crate::review::ReviewInput)>>>;

struct H {
    t: MuthurTab,
    sent: Sent,
    /// The agent of each spawned request, in order.
    agents: Rc<RefCell<Vec<AgentId>>>,
    #[cfg(feature = "agent")]
    ran: Ran,
    /// The password handed to each `run_tool` call, in order.
    #[cfg(feature = "agent")]
    secrets: Secrets,
    #[cfg(feature = "agent")]
    cancels: Cancels,
    #[cfg(feature = "agent")]
    reviews: Reviews,
    #[cfg(feature = "agent")]
    review_aborted: Rc<Cell<bool>>,
    aborted: Rc<Cell<bool>>,
    clock: Rc<Cell<Instant>>,
    path: PathBuf,
    #[cfg_attr(not(feature = "agent"), allow(dead_code))]
    audit_path: PathBuf,
    #[cfg_attr(not(feature = "agent"), allow(dead_code))]
    audit: crate::ops::audit::SharedAudit,
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

/// The tab under test, made by `new_tab` from the config, the prefilled log,
/// the audit log and recording deps.
fn harness_base(
    cfg: MuthurSection,
    audit_online: bool,
    prefill: impl FnOnce(&mut ChatLog),
    new_tab: impl FnOnce(MuthurSection, ChatLog, crate::ops::audit::SharedAudit, MuthurDeps) -> MuthurTab,
) -> H {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("muthur/chat.jsonl");
    let audit_path = dir.path().join("muthur/audit.jsonl");
    let mut log = ChatLog::open(path.clone()).unwrap();
    prefill(&mut log);
    let audit = crate::ops::audit::shared(if audit_online { AuditLog::open(audit_path.clone()).unwrap() } else { AuditLog::offline() });
    let shared_audit = audit.clone();
    let sent: Sent = Rc::default();
    let agents: Rc<RefCell<Vec<AgentId>>> = Rc::default();
    let aborted = Rc::new(Cell::new(false));
    let clock = Rc::new(Cell::new(Instant::now()));
    let (s, g, a, c) = (sent.clone(), agents.clone(), aborted.clone(), clock.clone());
    #[cfg(feature = "agent")]
    let (ran, secrets, cancels, reviews, review_aborted) =
        (Ran::default(), Secrets::default(), Cancels::default(), Reviews::default(), Rc::new(Cell::new(false)));
    #[cfg(feature = "agent")]
    let (r, sec, can, rev, ra) = (ran.clone(), secrets.clone(), cancels.clone(), reviews.clone(), review_aborted.clone());
    let deps = MuthurDeps {
        spawn: Box::new(move |id, agent, req| {
            s.borrow_mut().push((id, req));
            g.borrow_mut().push(agent);
            let a = a.clone();
            Box::new(move || a.set(true))
        }),
        now: Box::new(move || c.get()),
        ship_time: Box::new(|| "18 NOV 2137 14:02".into()),
        timestamp: Box::new(|| "2026-11-18T14:02:00Z".into()),
        #[cfg(feature = "agent")]
        run_tool: Box::new(move |req, id, inv, secret, cancel| {
            r.borrow_mut().push((req, id, inv));
            sec.borrow_mut().push(secret);
            can.borrow_mut().push(cancel);
        }),
        #[cfg(feature = "agent")]
        review: Box::new(move |req, id, input| {
            rev.borrow_mut().push((req, id, input));
            let ra = ra.clone();
            Box::new(move || ra.set(true))
        }),
    };
    let t = new_tab(cfg, log, audit, deps);
    H {
        t,
        sent,
        agents,
        #[cfg(feature = "agent")]
        ran,
        #[cfg(feature = "agent")]
        secrets,
        #[cfg(feature = "agent")]
        cancels,
        #[cfg(feature = "agent")]
        reviews,
        #[cfg(feature = "agent")]
        review_aborted,
        aborted,
        clock,
        path,
        audit_path,
        audit: shared_audit,
        _dir: dir,
    }
}

#[cfg(feature = "agent")]
fn harness_with(cfg: MuthurSection, prefill: impl FnOnce(&mut ChatLog)) -> H {
    agentic::harness_full(cfg, agentic::tools_cfg(), true, prefill)
}

#[cfg(not(feature = "agent"))]
fn harness_with(cfg: MuthurSection, prefill: impl FnOnce(&mut ChatLog)) -> H {
    harness_base(cfg, true, prefill, |cfg, log, audit, deps| MuthurTab::new(cfg, "USCSS NOSTROMO", log, audit, deps))
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
    log.append_user(user, "ts", AgentId::Muthur).unwrap();
    let a = Assistant { provider: Provider::Anthropic, model: "m".into(), text: reply.into(), stop: stop.into(), raw: None, lane: Lane::Muthur };
    log.append_assistant(a, "ts").unwrap();
}

#[test]
fn wrap_breaks_long_words() {
    assert_eq!(wrap("AB CDEFGHIJ K", 4), vec!["AB", "CDEF", "GHIJ", "K"]);
    assert_eq!(wrap("A\n\nB", 10), vec!["A", "", "B"]);
}

#[test]
fn wrap_hanging_indents_continuations() {
    assert_eq!(wrap_hanging("RUN  ls -la /var/log", 13, 5), vec!["RUN  ls -la", "     /var/log"]);
    assert_eq!(wrap_hanging("ABCDEFGHIJKLMN", 8, 5), vec!["ABCDEFGH", "     IJK", "     LMN"]);
    assert_eq!(wrap_hanging("RUN  a\nb", 20, 5), vec!["RUN  a", "     b"]);
}

#[test]
fn system_prompt_override_and_ship() {
    let s = system_prompt(&cfg(), "USCSS NOSTROMO", "");
    assert!(s.starts_with("You are MU/TH/UR 6000, the mainframe computer of the USCSS NOSTROMO."), "{s}");
    let blank = MuthurSection { system_prompt: Some("  ".into()), ..cfg() };
    assert_eq!(system_prompt(&blank, "X", ""), system_prompt(&cfg(), "X", ""));
    let own = MuthurSection { system_prompt: Some("Computer of {SHIP}.".into()), ..cfg() };
    assert_eq!(system_prompt(&own, "X", ""), "Computer of X.");
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
    assert!(s.contains("/PURGE   CLEAR MEMORY BANKS") && s.contains("PGUP/PGDN SCROLL"), "{s}");
    assert_eq!(s.contains("/DIRECT  OPEN DIRECT LINK") && s.contains("/MUTHUR  CLOSE DIRECT LINK"), cfg!(feature = "agent"), "{s}");
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


// ---- Without the `agent` feature: M4's chat ----

#[cfg(not(feature = "agent"))]
mod plain {
    use super::*;
    use crate::llm::{ToolChoice, ToolResult};
    use serde_json::json;

    #[test]
    fn persona_has_no_tool_lines() {
        let s = system_prompt(&cfg(), "X", "");
        assert!(s.ends_with("Never invent readings."), "{s}");
        assert!(!s.contains("tools") && !s.contains("/DIRECT") && !s.contains("delegate"), "{s}");
    }

    #[test]
    fn no_tools_in_request() {
        let mut h = harness();
        submit(&mut h, "status");
        let s = h.sent.borrow();
        assert_eq!(s.len(), 1);
        assert!(s[0].1.tools.is_empty());
        assert_eq!(s[0].1.tool_choice, ToolChoice::Auto);
    }

    #[test]
    fn tool_use_stop_finishes() {
        let mut h = harness();
        submit(&mut h, "status");
        h.t.on_llm(1, StreamEvent::Text("NOMINAL.".into()));
        h.t.on_llm(1, StreamEvent::ToolCall { id: "c1".into(), name: "ship_status".into(), input: json!({}) });
        let raw = vec![json!({"type":"tool_use","id":"c1","name":"ship_status","input":{}})];
        h.t.on_llm(1, StreamEvent::Done { stop: Stop::ToolUse, model: Some("claude-test".into()), raw: Some(raw) });
        assert_eq!(h.t.state, State::Ready);
        assert_eq!(h.sent.borrow().len(), 1, "nothing is dispatched");
        h.tick(1000);
        let s = text(&mut h.t);
        assert!(s.contains("NOMINAL.") && !s.contains("ACCESSING"), "{s}");
        assert!(h.t.take_activity());
    }

    #[test]
    fn direct_is_unknown_command() {
        let mut h = harness();
        submit(&mut h, "/direct");
        let s = text(&mut h.t);
        assert!(s.contains("UNKNOWN COMMAND") && !s.contains("DIRECT LINK"), "{s}");
        h.t.handle_key(&Key::Ctrl('u'));
        submit(&mut h, "/MUTHUR");
        assert!(text(&mut h.t).contains("UNKNOWN COMMAND"));
        assert!(h.sent.borrow().is_empty());
    }

    #[test]
    fn agent_log_is_readable() {
        let asst = |agent: AgentId, text: &str, stop: &str, raw| Assistant {
            provider: Provider::Anthropic,
            model: "m".into(),
            text: text.into(),
            stop: stop.into(),
            raw,
            lane: agent.into(),
        };
        let mut h = harness_with(cfg(), |l| {
            // A: a persona exchange with a tool round.
            l.append_user("[SHIP TIME X] ALPHA", "ts", AgentId::Muthur).unwrap();
            let raw = vec![json!({"type":"tool_use","id":"c1","name":"ship_status","input":{}})];
            l.append_assistant(asst(AgentId::Muthur, "", "tool_use", Some(raw)), "ts").unwrap();
            let r = ToolResult { id: "c1".into(), name: "ship_status".into(), content: "UP".into(), is_error: false, label: "SHIP STATUS".into() };
            l.append_tool(&[r], "ts", &Lane::Muthur).unwrap();
            l.append_assistant(asst(AgentId::Muthur, "ALPHA DONE.", "end_turn", None), "ts").unwrap();
            // B: a direct-link exchange.
            l.append_user("[SHIP TIME X] uptime", "ts", AgentId::Tech).unwrap();
            l.append_assistant(asst(AgentId::Tech, "bravo secret", "end_turn", None), "ts").unwrap();
            // C: a plain persona exchange.
            l.append_user("[SHIP TIME X] CHARLIE", "ts", AgentId::Muthur).unwrap();
            l.append_assistant(asst(AgentId::Muthur, "CHARLIE DONE.", "end_turn", None), "ts").unwrap();
        });
        let s = text(&mut h.t);
        assert!(s.contains("> ALPHA") && s.contains("ALPHA DONE.") && s.contains("> CHARLIE"), "{s}");
        assert!(!s.contains("UPTIME") && !s.contains("BRAVO") && !s.contains("DIRECT LINK"), "{s}");
        assert!(!s.contains("ACCESSING") && !s.contains("SHIP STATUS"), "{s}");
        submit(&mut h, "delta");
        assert_eq!(*h.agents.borrow(), [AgentId::Muthur]);
        let sent = h.sent.borrow();
        let texts: Vec<String> = sent[0]
            .1
            .messages
            .iter()
            .map(|m| match &m.content {
                MessageContent::Text(t) => t.clone(),
                c => panic!("not report-only: {c:?}"),
            })
            .collect();
        assert_eq!(texts.len(), 5, "{texts:?}");
        assert_eq!(texts[..4], ["[SHIP TIME X] ALPHA", "ALPHA DONE.", "[SHIP TIME X] CHARLIE", "CHARLIE DONE."]);
        // The epoch hash is taken with no tools.
        let hash = crate::chatlog::fnv64(&format!("{:?}\n{}\n{}\n", Provider::Anthropic, cfg().model().unwrap_or_default(), sent[0].1.system));
        let log = std::fs::read_to_string(&h.path).unwrap();
        assert!(log.contains(&format!("\"system_hash\":\"{hash}\"")), "{log}");
    }
}

#[test]
fn user_and_command_text_is_underlined_after_the_marker() {
    let e = [Entry::User("ABC DEF".into()), Entry::Reply("OK".into()), Entry::Command("ls -la".into())];
    let lines = view::transcript_lines(&e, None, 5);
    let got: Vec<(&str, Option<usize>)> = lines.iter().map(|l| (l.text.as_str(), l.underline_from)).collect();
    assert_eq!(
        got,
        vec![
            (view::BANNER, None),
            ("", None),
            ("> ABC", Some(2)),
            ("DEF", Some(0)),
            ("", None),
            ("OK", None),
            ("", None),
            ("$ ls", Some(2)),
            ("-la", Some(0)),
        ]
    );
}

#[test]
fn rendered_user_message_is_underlined_but_not_its_marker() {
    let mut h = harness();
    submit(&mut h, "status");
    let area = Rect::new(0, 0, 61, 40);
    let mut buf = Buffer::empty(area);
    h.t.render(area, &mut buf);
    let y = (0..40).find(|&y| (0..61).map(|x| buf[(x, y)].symbol()).collect::<String>().contains("> STATUS")).unwrap();
    let x0 = (0..61).find(|&x| buf[(x, y)].symbol() == ">").unwrap();
    let ul = |x: u16| buf[(x, y)].modifier.contains(Modifier::UNDERLINED);
    assert!(!ul(x0) && !ul(x0 + 1));
    assert!((x0 + 2..x0 + 8).all(ul));
    assert!(!ul(x0 + 8));
}
