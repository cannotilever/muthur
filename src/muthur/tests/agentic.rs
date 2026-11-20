use super::*;
use crate::agent::Policy;
use crate::config::{AgentSection, ToolsSection};
use crate::llm::ToolResult;
use crate::review::{Review, Risk, Verdict};
use crate::secret::Secret;
use crate::tools::shell::Probe;
use crate::tools::{Invocation, ToolDefs, ToolOutcome};
use serde_json::json;
use std::sync::atomic::Ordering;

pub(super) fn tools_cfg() -> ToolsSection {
    ToolsSection::default()
}

/// The technical agent's deps: `run` available, the default `[agent]`.
fn tech_deps(cfg: &MuthurSection, probe: Probe, reviewer_ready: bool) -> AgentDeps {
    let agent = AgentSection::default();
    AgentDeps {
        defs: ToolDefs::technical(&agent, true),
        cfg: cfg.technical_cfg(),
        prompt: tech_prompt(&agent),
        facts: Box::new(|| "HOST: testhost".into()),
        policy: Policy { deny: Some(crate::tools::deny::test_rules()), net_allow: Vec::new() },
        label: agent.label.clone(),
        max_rounds: agent.max_rounds,
        probe,
        reviewer_ready,
    }
}

pub(super) fn harness_full(cfg: MuthurSection, tools: ToolsSection, audit_online: bool, prefill: impl FnOnce(&mut ChatLog)) -> H {
    harness_tech(cfg, tools, audit_online, Probe::Ready, true, prefill)
}

fn harness_tech(
    cfg: MuthurSection,
    tools: ToolsSection,
    audit_online: bool,
    probe: Probe,
    reviewer_ready: bool,
    prefill: impl FnOnce(&mut ChatLog),
) -> H {
    let tech = tech_deps(&cfg, probe, reviewer_ready);
    harness_base(cfg, audit_online, prefill, |cfg, log, audit, deps| {
        MuthurTab::new(cfg, ToolDefs::persona(&tools), "USCSS NOSTROMO", log, audit, deps, tech)
    })
}

// ---- Persona tool turns ----

fn ok(content: &str) -> ToolOutcome {
    ToolOutcome { content: content.into(), is_error: false, argv: Some(vec!["docker".into()]), exit: Some(json!(0)), ms: 5, audit_output: None }
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

fn audit_records(h: &H) -> Vec<serde_json::Value> {
    std::fs::read_to_string(&h.audit_path).unwrap_or_default().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[test]
fn shared_audit_sees_muthur_entries() {
    let mut h = harness();
    submit(&mut h, "status");
    tool_use(&mut h, 1, &[("c1", "ship_status", json!({}))]);
    h.t.on_tool_done(1, "c1".into(), ok("UP"));
    assert!(h.audit.borrow().is_online());
    // Another writer through the same handle lands in the same file.
    let e = crate::ops::audit::AuditEntry::new("cargo_start", json!({}), crate::ops::audit::Decision::Auto);
    h.audit.borrow_mut().append(&e, "ts").unwrap();
    let audit = std::fs::read_to_string(&h.audit_path).unwrap();
    assert_eq!(audit.lines().count(), 2, "{audit}");
}

#[test]
fn tool_round_runs_and_respawns() {
    let mut h = harness();
    submit(&mut h, "what containers run");
    assert!(!h.sent.borrow()[0].1.tools.is_empty());
    assert_eq!(h.sent.borrow()[0].1.tool_choice, crate::llm::ToolChoice::Auto);
    h.t.on_llm(1, StreamEvent::Text("CHECKING.".into()));
    tool_use(&mut h, 1, &[("c1", "containers", json!({}))]);
    assert_eq!(*h.ran.borrow(), vec![(1, "c1".to_string(), Invocation::Containers)]);
    let s = text(&mut h.t);
    let (c, a) = (s.find("CHECKING.").unwrap(), s.find("ACCESSING: CONTAINERS").unwrap());
    assert!(c < a, "{s}");
    assert!(status_row(&mut h).contains("ACCESSING"));
    assert_eq!(h.t.tick_interval(), Some(Duration::from_millis(100)));
    h.t.on_tool_done(1, "c1".into(), ok("web up"));
    {
        let s = h.sent.borrow();
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].0, 2);
        assert!(!s[1].1.tools.is_empty());
        assert_eq!(s[1].1.tool_choice, crate::llm::ToolChoice::Auto);
    }
    let r = last_results(&h);
    assert_eq!(r.len(), 1);
    assert_eq!((r[0].id.as_str(), r[0].content.as_str()), ("c1", "web up"));
    let audit = std::fs::read_to_string(&h.audit_path).unwrap();
    let lines: Vec<&str> = audit.lines().collect();
    assert_eq!(lines.len(), 1);
    let rec: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(rec["decision"], "auto");
    assert_eq!(rec["agent"], "muthur");
    // The final answer lands after the note.
    h.t.on_llm(2, StreamEvent::Text("WEB UP.".into()));
    h.t.on_llm(2, done(Stop::End));
    h.tick(2000);
    let s = text(&mut h.t);
    assert!(s.find("ACCESSING: CONTAINERS").unwrap() < s.find("WEB UP.").unwrap(), "{s}");
    assert!(status_row(&mut h).contains("READY FOR INQUIRY"));
}

#[test]
fn delegate_placeholder_answers_no_response() {
    let mut h = harness();
    submit(&mut h, "why is nginx down");
    tool_use(&mut h, 1, &[("d1", "delegate", json!({"task": "check nginx"}))]);
    let r = last_results(&h);
    assert_eq!(r.len(), 1);
    assert!(r[0].content.contains("NO RESPONSE FROM SCIENCE OFFICER"), "{}", r[0].content);
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
    assert_eq!(h.sent.borrow()[0].1.tool_choice, crate::llm::ToolChoice::Auto);
    tool_use(&mut h, 1, &[("c1", "ship_status", json!({}))]);
    h.t.on_tool_done(1, "c1".into(), ok("ship"));
    assert_eq!(h.sent.borrow()[1].1.tool_choice, crate::llm::ToolChoice::None);
    // Tool calls anyway, twice: one more forced request, then truncation.
    tool_use(&mut h, 2, &[("c2", "ship_status", json!({}))]);
    assert_eq!(h.sent.borrow().len(), 3);
    assert_eq!(h.sent.borrow()[2].1.tool_choice, crate::llm::ToolChoice::None);
    tool_use(&mut h, 3, &[("c3", "ship_status", json!({}))]);
    assert_eq!(h.sent.borrow().len(), 3);
    assert_eq!(h.t.state, State::Ready);
    h.tick(1000);
    assert!(text(&mut h.t).trim_end().contains("[TRANSMISSION TRUNCATED]"));
    // max_rounds 0: the first request is already final.
    let zero = ToolsSection { max_rounds: 0, ..tools_cfg() };
    let mut h = harness_full(cfg(), zero, true, |_| {});
    submit(&mut h, "status");
    assert_eq!(h.sent.borrow()[0].1.tool_choice, crate::llm::ToolChoice::None);
}

#[test]
fn audit_offline_flash() {
    let mut h = harness_full(cfg(), tools_cfg(), false, |_| {});
    submit(&mut h, "status");
    tool_use(&mut h, 1, &[("c1", "containers", json!({}))]);
    assert_eq!(h.ran.borrow().len(), 1);
    h.t.on_tool_done(1, "c1".into(), ok("web up"));
    assert_eq!(h.sent.borrow().len(), 2);
    let row = status_row(&mut h);
    assert!(row.contains("AUDIT LOG OFFLINE"), "{row}");
    // Review-free tools still run with the audit log offline.
    tool_use(&mut h, 2, &[("c2", "ship_status", json!({}))]);
    assert_eq!(h.ran.borrow().len(), 2);
    h.t.on_tool_done(2, "c2".into(), ok("UP"));
    assert_eq!(h.sent.borrow().len(), 3);
    assert_eq!(last_results(&h)[0].content, "UP");
}

#[test]
fn history_shows_tool_notes() {
    let mut h = harness_with(cfg(), |l| {
        l.append_user("[SHIP TIME X] STATUS", "ts", AgentId::Muthur).unwrap();
        let raw = vec![json!({"type":"tool_use","id":"c1","name":"ship_status","input":{}})];
        let a = Assistant { provider: Provider::Anthropic, model: "m".into(), text: "".into(), stop: "tool_use".into(), raw: Some(raw), lane: Lane::Muthur };
        l.append_assistant(a, "ts").unwrap();
        let r = |id: &str, content: &str, label: &str| ToolResult {
            id: id.into(),
            name: "x".into(),
            content: content.into(),
            is_error: false,
            label: label.into(),
        };
        l.append_tool(&[r("c1", "ok", "SHIP STATUS"), r("c2", "NOT AUTHORIZED: UNKNOWN CONTAINER", "CONTAINER LOGS web 50")], "ts", &Lane::Muthur).unwrap();
        let a = Assistant { provider: Provider::Anthropic, model: "m".into(), text: "NOMINAL.".into(), stop: "end_turn".into(), raw: None, lane: Lane::Muthur };
        l.append_assistant(a, "ts").unwrap();
    });
    let rows = screen(&mut h.t, 61, 40);
    let at = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("{needle}: {rows:#?}"));
    let (u, n, d, r) = (at("> STATUS"), at("ACCESSING: SHIP STATUS"), at("ACCESS DENIED: CONTAINER LOGS web 50"), at("NOMINAL."));
    assert!(u < n && n < d && d < r, "{rows:#?}");
}

#[test]
fn persona_mentions_tools() {
    assert!(PERSONA.contains("You cannot change the ship. For changes, tell the operator to open a direct link with /DIRECT."));
    assert!(!PERSONA.contains("operator override"));
    assert!(!PERSONA.contains("do not yet have access"));
}

#[test]
fn stream_error_mid_turn_closes_log() {
    let mut h = harness();
    submit(&mut h, "status");
    tool_use(&mut h, 1, &[("c1", "ship_status", json!({}))]);
    // While tools run no stream is open: a stray error is ignored.
    h.t.on_llm(1, StreamEvent::Error(LlmError::SignalLost));
    assert_eq!(h.t.state, State::Tools);
    h.t.on_tool_done(1, "c1".into(), ok("ship"));
    h.t.on_llm(2, StreamEvent::Error(LlmError::SignalLost));
    assert_eq!(h.t.state, State::Ready);
    assert!(text(&mut h.t).contains("UNABLE TO COMPLY -- SIGNAL LOST"));
    let recs = records(&h);
    let kinds: Vec<&str> = recs.iter().filter_map(|r| r["kind"].as_str()).filter(|k| *k != "epoch").collect();
    assert_eq!(kinds, ["user", "assistant", "tool", "assistant"]);
    let last = recs.last().unwrap();
    assert_eq!(last["stop"], "error");
    // Late outcomes after the turn ended do nothing.
    h.t.on_tool_done(1, "c1".into(), ok("late"));
    submit(&mut h, "again");
    let s = h.sent.borrow();
    assert_eq!(s.len(), 3);
    assert_eq!(s[2].1.messages.len(), 1, "failed exchange must be left out: {:?}", s[2].1.messages);
}

#[test]
fn ctrl_c_while_tool_runs_orphan_audited_as_auto() {
    let mut h = harness();
    submit(&mut h, "status");
    tool_use(&mut h, 1, &[("c1", "containers", json!({}))]);
    h.t.handle_key(&Key::Ctrl('c'));
    let a = audit_records(&h);
    assert_eq!((a.len(), &a[0]["output"], &a[0]["argv"]), (1, &json!("ABORTED BY OPERATOR"), &json!(null)));
    let shown = text(&mut h.t);
    assert!(!shown.contains("ACCESS INTERRUPTED"), "{shown}");
    h.t.on_tool_done(1, "c1".into(), ok("web up"));
    let a = audit_records(&h);
    assert_eq!(a.len(), 2);
    assert_eq!(a[1]["decision"], "auto");
    assert_eq!(a[1]["output"], "web up");
    // Only once, and nothing else changes.
    h.t.on_tool_done(1, "c1".into(), ok("web up"));
    assert_eq!(audit_records(&h).len(), 2);
    assert_eq!(h.sent.borrow().len(), 1);
    assert_eq!(h.t.state, State::Ready);
}

#[test]
fn tool_note_labels() {
    let r = |content: &str, label: &str| ToolResult { id: "c1".into(), name: "x".into(), content: content.into(), is_error: true, label: label.into() };
    assert_eq!(turn::tool_note(&r("UP", "SHIP STATUS")), "ACCESSING: SHIP STATUS");
    assert_eq!(turn::tool_note(&r("NOT AUTHORIZED: UNKNOWN CONTAINER", "CONTAINER INSPECT db")), "ACCESS DENIED: CONTAINER INSPECT db");
    assert_eq!(turn::tool_note(&r("ABORTED BY OPERATOR", "CONTAINERS")), "ACCESS DENIED: CONTAINERS");
    assert_eq!(turn::tool_note(&r(crate::agent::ABORTED_ISSUED, "CONTAINERS")), "ACCESS INTERRUPTED: CONTAINERS");
}

#[test]
fn unknown_tool_is_refused_without_running() {
    let mut h = harness();
    submit(&mut h, "restart nginx");
    tool_use(&mut h, 1, &[("c1", "run", json!({"command": "systemctl restart nginx", "reason": "r", "as_root": true, "timeout_secs": 0}))]);
    assert!(h.ran.borrow().is_empty());
    assert!(text(&mut h.t).contains("ACCESS DENIED: RUN"));
    assert_eq!(last_results(&h)[0].content, "NOT AUTHORIZED: unknown tool run");
    assert_eq!(audit_records(&h)[0]["decision"], "rejected");
}

// ---- Direct mode: the technical agent ----

fn direct(h: &mut H) {
    submit(h, "/direct");
}

fn review(verdict: Verdict, note: &str) -> Review {
    Review { verdict, risk: Risk::Low, note: note.into(), model: "reviewer".into(), ms: 3, error: None }
}

fn run_call(command: &str, as_root: bool) -> serde_json::Value {
    json!({"command": command, "reason": "operator asked", "as_root": as_root, "timeout_secs": 0})
}

/// Direct mode, one `run` call for request 1, sent to review.
fn raise_run(h: &mut H, command: &str, as_root: bool) {
    direct(h);
    submit(h, "please check");
    tool_use(h, 1, &[("c1", "run", run_call(command, as_root))]);
    assert_eq!(h.reviews.borrow().len(), 1, "the call goes to review");
}

/// Renders twice: the first render draws a prompt, the second starts arming.
fn seen(h: &mut H) {
    screen(&mut h.t, 80, 40);
    screen(&mut h.t, 80, 40);
}

fn row_text(row: &str) -> &str {
    row.trim_end_matches('│').trim_end()
}

fn exit_outcome(code: i64, lines: usize) -> ToolOutcome {
    let text: String = (0..lines).map(|i| format!("line {i}\n")).collect();
    ToolOutcome {
        content: format!("EXIT {code}\n{text}"),
        is_error: false,
        argv: None,
        exit: Some(json!(code)),
        ms: 100,
        audit_output: Some(text),
    }
}

#[test]
fn direct_toggle_and_banner() {
    let mut h = harness();
    direct(&mut h);
    assert_eq!(h.t.transcript.last(), Some(&Entry::Note("DIRECT LINK: SCIENCE OFFICER".into())));
    let rows = screen(&mut h.t, 61, 40);
    assert!(rows[38].starts_with("│ $ "), "{}", rows[38]);
    // The technical agent gets its own prompt (with the host facts), tools and records.
    submit(&mut h, "uptime please");
    {
        let s = h.sent.borrow();
        assert_eq!(*h.agents.borrow(), vec![AgentId::Tech]);
        assert!(s[0].1.system.contains("HOST: testhost"), "{}", s[0].1.system);
        assert!(s[0].1.tools.iter().any(|t| t.name == "run"));
    }
    h.t.on_llm(1, StreamEvent::Text("Up 3 days.".into()));
    h.t.on_llm(1, done(Stop::End));
    let hist = h.reopen().history();
    assert_eq!((hist[0].agent, hist[0].reply.as_ref().unwrap().0.as_str()), (AgentId::Tech, "Up 3 days."));
    submit(&mut h, "/MUTHUR");
    assert_eq!(h.t.transcript.last(), Some(&Entry::Note("DIRECT LINK CLOSED".into())));
    let rows = screen(&mut h.t, 61, 40);
    assert!(rows[38].starts_with("│ > "), "{}", rows[38]);
    submit(&mut h, "status");
    assert_eq!(*h.agents.borrow(), vec![AgentId::Tech, AgentId::Muthur]);
    assert!(h.sent.borrow()[1].1.system.contains("MU/TH/UR 6000"));
}

#[test]
fn toggle_refused_mid_turn() {
    let mut h = harness();
    submit(&mut h, "status");
    submit(&mut h, "/direct");
    assert!(status_row(&mut h).contains("INQUIRY IN PROGRESS"));
    assert_eq!(h.t.mode, Mode::Persona);
    // And back: refused while the technical agent works.
    h.t.handle_key(&Key::Ctrl('c'));
    h.t.handle_key(&Key::Ctrl('u'));
    direct(&mut h);
    submit(&mut h, "uptime");
    submit(&mut h, "/muthur");
    assert!(status_row(&mut h).contains("INQUIRY IN PROGRESS"));
    assert_eq!(h.t.mode, Mode::Direct);
}

#[test]
fn probe_failure_in_banner() {
    let mut h = harness_tech(cfg(), tools_cfg(), true, Probe::NotIsolated, false, |_| {});
    direct(&mut h);
    assert_eq!(
        h.t.transcript.last(),
        Some(&Entry::Note("DIRECT LINK: SCIENCE OFFICER\nAGENT NETWORK NOT ISOLATED — SEE DEPLOY\nREVIEWER OFFLINE".into()))
    );
    let rows = screen(&mut h.t, 61, 40);
    let banner = rows.iter().position(|r| r.contains("DIRECT LINK: SCIENCE OFFICER")).unwrap();
    assert!(rows[banner + 1].contains("AGENT NETWORK NOT ISOLATED -- SEE DEPLOY"), "{rows:#?}");
    assert!(rows[banner + 2].contains("REVIEWER OFFLINE"), "{rows:#?}");
}

#[test]
fn direct_mode_keeps_case() {
    let mut h = harness_with(MuthurSection { uppercase: true, ..cfg() }, |_| {});
    direct(&mut h);
    type_str(&mut h.t, "ls -la /Var");
    assert_eq!(h.t.input, "ls -la /Var");
    h.t.handle_key(&Key::Enter);
    let s = h.sent.borrow();
    let MessageContent::Text(u) = &s[0].1.messages.last().unwrap().content else { panic!() };
    assert_eq!(u, "[SHIP TIME 18 NOV 2137 14:02] ls -la /Var");
    assert_eq!(h.t.transcript.last(), Some(&Entry::Command("ls -la /Var".into())));
    drop(s);
    // Replies keep their case too.
    h.t.on_llm(1, StreamEvent::Text("Found /Var.".into()));
    h.t.on_llm(1, done(Stop::End));
    h.t.handle_key(&Key::Esc);
    let shown = text(&mut h.t);
    assert!(shown.contains("$ ls -la /Var") && shown.contains("Found /Var."), "{shown}");
}

/// Runs `f` with every tracing event, at every level, written to a buffer.
fn traced(f: impl FnOnce()) -> String {
    #[derive(Clone, Default)]
    struct Buf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let w = buf.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || w.clone())
        .finish();
    tracing::subscriber::with_default(sub, f);
    String::from_utf8_lossy(&buf.0.lock().unwrap()).into_owned()
}

#[test]
fn password_never_reaches_tracing() {
    let log = traced(|| {
        tracing::trace!("capture-check");
        password_flow_body();
    });
    assert!(log.contains("capture-check"), "the subscriber must be live");
    assert!(!log.contains("hunter2"), "{log}");
}

#[test]
fn password_flow() {
    password_flow_body();
}

fn password_flow_body() {
    let mut h = harness();
    raise_run(&mut h, "id", true);
    h.t.on_review(1, "c1".into(), review(Verdict::Allow, "read-only identity check"));
    let row = status_row(&mut h);
    assert!(row.contains("CLEARANCE REQUIRED -- id -- PASSWORD: "), "{row}");
    assert!(h.ran.borrow().is_empty());
    seen(&mut h);
    h.tick(1100);
    type_str(&mut h.t, "hunter2");
    assert_eq!(h.t.input, "", "password chars never reach the input line");
    let row = status_row(&mut h);
    assert!(row_text(&row).ends_with("PASSWORD: *******"), "{row}");
    assert!(!row.contains("hunter2"));
    h.t.handle_key(&Key::Backspace);
    assert!(row_text(&status_row(&mut h)).ends_with("PASSWORD: ******"));
    h.t.handle_key(&Key::Char('2'));
    h.t.handle_key(&Key::Enter);
    assert_eq!(h.ran.borrow().len(), 1);
    {
        let secrets = h.secrets.borrow();
        assert_eq!(secrets[0].as_ref().map(Secret::expose), Some("hunter2"));
    }
    assert!(status_row(&mut h).contains("EXECUTING"));
    h.t.on_tool_done(1, "c1".into(), exit_outcome(0, 1));
    assert_eq!(h.sent.borrow().len(), 2);
    let shown = text(&mut h.t);
    assert!(shown.contains("RUN# id  [ROOT, ALLOW]") && shown.contains("EXIT 0  1 LINES  0.1 S"), "{shown}");
    let chat = std::fs::read_to_string(&h.path).unwrap();
    let audit = std::fs::read_to_string(&h.audit_path).unwrap();
    assert!(!chat.contains("hunter2"), "{chat}");
    assert!(!audit.contains("hunter2"), "{audit}");
    assert_eq!(audit_records(&h)[0]["decision"], "confirmed");
    // The secret is not kept anywhere in the tab.
    assert!(!format!("{:?}", h.t.state).contains("hunter2"));
}

#[test]
fn password_enter_with_nothing_typed_is_ignored() {
    let mut h = harness();
    raise_run(&mut h, "id", true);
    h.t.on_review(1, "c1".into(), review(Verdict::Allow, "ok"));
    seen(&mut h);
    h.tick(1100);
    h.t.handle_key(&Key::Enter);
    assert!(h.ran.borrow().is_empty());
    assert!(matches!(h.t.state, State::Password { .. }));
}

#[test]
fn password_esc_denies() {
    let mut h = harness();
    raise_run(&mut h, "id", true);
    h.t.on_review(1, "c1".into(), review(Verdict::Allow, "ok"));
    seen(&mut h);
    h.tick(1100);
    type_str(&mut h.t, "abc");
    h.t.handle_key(&Key::Esc);
    assert!(h.ran.borrow().is_empty());
    assert_eq!(h.sent.borrow().len(), 2);
    let r = last_results(&h);
    assert_eq!((r[0].content.as_str(), r[0].is_error), ("DENIED BY OPERATOR", true));
    assert_eq!(audit_records(&h)[0]["decision"], "denied");
    assert!(text(&mut h.t).contains("RUN# id  [ROOT, DENIED BY OPERATOR]"));
}

#[test]
fn ctrl_c_at_password_wipes_and_aborts() {
    let mut h = harness();
    raise_run(&mut h, "id", true);
    h.t.on_review(1, "c1".into(), review(Verdict::Allow, "ok"));
    seen(&mut h);
    h.tick(1100);
    type_str(&mut h.t, "abc");
    let State::Password { secret, .. } = &h.t.state else { panic!("{:?}", h.t.state) };
    assert_eq!(secret.chars(), 3);
    h.t.handle_key(&Key::Ctrl('c'));
    assert!(h.ran.borrow().is_empty());
    assert_eq!(h.t.state, State::Ready);
    let a = audit_records(&h);
    assert_eq!(a.last().unwrap()["decision"], "aborted");
    let recs = records(&h);
    let n = recs.len();
    assert_eq!(recs[n - 2]["results"][0]["content"], "ABORTED BY OPERATOR");
    assert_eq!(recs[n - 1]["stop"], "interrupted");
    // Typing now goes to the input line again.
    type_str(&mut h.t, "x");
    assert_eq!(h.t.input, "x");
}

#[test]
fn override_requires_exact_word() {
    let mut h = harness();
    raise_run(&mut h, "rm -rf /tmp/cache", false);
    h.t.on_review(1, "c1".into(), review(Verdict::Deny, "deletes files"));
    let row = status_row(&mut h);
    assert!(row_text(&row).ends_with("DENIED BY REVIEWER -- deletes files -- TYPE OVERRIDE TO FORCE, ESC TO REFUSE"), "{row}");
    assert!(text(&mut h.t).contains("     DENIED BY REVIEWER: deletes files"), "the whole note is shown above");
    seen(&mut h);
    h.tick(1100);
    type_str(&mut h.t, "OVERRID");
    assert!(row_text(&status_row(&mut h)).ends_with("REFUSE OVERRID"));
    h.t.handle_key(&Key::Enter);
    assert!(h.ran.borrow().is_empty());
    let State::OverridePrompt { typed, .. } = &h.t.state else { panic!("{:?}", h.t.state) };
    assert_eq!(typed, "");
    type_str(&mut h.t, "overridx");
    h.t.handle_key(&Key::Backspace);
    type_str(&mut h.t, "e");
    assert!(row_text(&status_row(&mut h)).ends_with("REFUSE OVERRIDE"));
    h.t.handle_key(&Key::Enter);
    assert_eq!(h.ran.borrow().len(), 1);
    assert!(h.secrets.borrow()[0].is_none(), "unprivileged runs get no password");
    h.t.on_tool_done(1, "c1".into(), exit_outcome(0, 0));
    assert_eq!(audit_records(&h)[0]["decision"], "overridden");
    assert!(text(&mut h.t).contains("RUN  rm -rf /tmp/cache  [OVERRIDDEN]"));
}

#[test]
fn override_esc_refuses() {
    let mut h = harness();
    raise_run(&mut h, "rm -rf /tmp/cache", false);
    h.t.on_review(1, "c1".into(), review(Verdict::Deny, "deletes files"));
    seen(&mut h);
    h.tick(1100);
    h.t.handle_key(&Key::Esc);
    assert!(h.ran.borrow().is_empty());
    assert_eq!(last_results(&h)[0].content, "DENIED BY REVIEWER: deletes files");
}

#[test]
fn ask_prompt_confirms_after_arming() {
    let mut h = harness();
    raise_run(&mut h, "touch /tmp/x", false);
    h.t.on_review(1, "c1".into(), review(Verdict::Ask, "creates a file"));
    assert!(h.t.take_activity());
    let row = status_row(&mut h);
    assert!(row.contains("CONFIRM -- touch /tmp/x -- Y/N"), "{row}");
    // The reviewer's note is shown while the operator decides.
    let shown = text(&mut h.t);
    assert!(shown.contains("touch /tmp/x\n") || shown.contains("│ touch /tmp/x "), "the pending command is shown: {shown}");
    assert!(shown.contains("     REVIEW: LOW -- creates a file"), "{shown}");
    assert_eq!(h.t.tick_interval(), Some(Duration::from_millis(100)));
    seen(&mut h);
    h.tick(1100);
    h.t.handle_key(&Key::Char('x'));
    assert!(matches!(h.t.state, State::Ask { .. }));
    assert_eq!(h.t.input, "");
    h.t.handle_key(&Key::Char('y'));
    assert_eq!(*h.ran.borrow(), vec![(1, "c1".to_string(), Invocation::Run {
        command: "touch /tmp/x".into(),
        reason: "operator asked".into(),
        as_root: false,
        timeout_secs: 30,
    })]);
    let rows = screen(&mut h.t, 61, 40);
    let run = rows.iter().position(|r| r.contains("RUN  touch /tmp/x  [ASK: LOW]")).unwrap_or_else(|| panic!("{rows:#?}"));
    assert!(rows[run + 1].starts_with("│      REVIEW: LOW -- creates a file"), "{rows:#?}");
    assert_eq!(rows.iter().filter(|r| r.contains("REVIEW: LOW -- creates a file")).count(), 1, "the pending block is gone: {rows:#?}");
    assert!(!rows.iter().any(|r| r.starts_with("│ touch /tmp/x")), "{rows:#?}");
}

#[test]
fn prompt_row_truncates_to_width() {
    let long = format!("find / -name {}", "x".repeat(120));
    for verdict in [Verdict::Ask, Verdict::Deny] {
        let mut h = harness();
        raise_run(&mut h, &long, false);
        h.t.on_review(1, "c1".into(), review(verdict, &"n".repeat(120)));
        let row = status_row(&mut h);
        let shown = row_text(&row);
        assert!(shown.contains("..."), "{row}");
        assert!(shown.ends_with("-- Y/N") || shown.ends_with("ESC TO REFUSE"), "{row}");
    }
    let mut h = harness();
    raise_run(&mut h, &long, true);
    h.t.on_review(1, "c1".into(), review(Verdict::Allow, "ok"));
    let row = status_row(&mut h);
    assert!(row_text(&row).ends_with("... -- PASSWORD:"), "{row}");
}

#[test]
fn narrow_prompt_rows_drop_the_part_then_the_head() {
    let row = |h: &mut H, w: u16| row_text(&screen(&mut h.t, w, 40)[37]).to_string();
    let mut h = harness();
    raise_run(&mut h, "rm -rf /var/log/journal", false);
    h.t.on_review(1, "c1".into(), review(Verdict::Deny, "deletes the journal"));
    // The windowed tab (61 wide): only the instructions fit.
    assert_eq!(row(&mut h, 61), "│ TYPE OVERRIDE TO FORCE, ESC TO REFUSE");
    assert_eq!(row(&mut h, 67), "│ DENIED BY REVIEWER -- TYPE OVERRIDE TO FORCE, ESC TO REFUSE");
    assert_eq!(row(&mut h, 80), "│ DENIED BY REVIEWER -- deletes th... -- TYPE OVERRIDE TO FORCE, ESC TO REFUSE");
    for w in [61, 67, 80, 120] {
        assert!(screen(&mut h.t, w, 40)[37].ends_with('│'), "row overflows at {w}");
    }
}

#[test]
fn ctrl_c_running_sets_cancel_flag() {
    let mut h = harness();
    raise_run(&mut h, "sleep 100", false);
    h.t.on_review(1, "c1".into(), review(Verdict::Allow, "harmless"));
    assert_eq!(h.ran.borrow().len(), 1);
    let flag = h.cancels.borrow()[0].clone();
    assert!(!flag.load(Ordering::SeqCst));
    h.t.handle_key(&Key::Ctrl('c'));
    assert!(flag.load(Ordering::SeqCst));
    assert_eq!(h.t.state, State::Ready);
    // The late outcome is audited under its real decision.
    h.t.on_tool_done(1, "c1".into(), exit_outcome(0, 0));
    let a = audit_records(&h);
    assert_eq!((a[0]["decision"].as_str(), a[1]["decision"].as_str()), (Some("aborted"), Some("auto")));
}

#[test]
fn reviewing_status_and_abort() {
    let mut h = harness();
    raise_run(&mut h, "uptime", false);
    assert_eq!(h.reviews.borrow()[0].0, 1);
    assert_eq!(h.reviews.borrow()[0].1, "c1");
    assert!(status_row(&mut h).contains("REVIEWING"));
    assert_eq!(h.t.tick_interval(), Some(Duration::from_millis(100)));
    // Stale or unknown verdicts are ignored.
    h.t.on_review(7, "c1".into(), review(Verdict::Allow, "ok"));
    h.t.on_review(1, "c9".into(), review(Verdict::Allow, "ok"));
    assert!(h.ran.borrow().is_empty());
    assert!(matches!(h.t.state, State::Reviewing));
    h.t.handle_key(&Key::Ctrl('c'));
    assert!(h.review_aborted.get());
    assert_eq!(h.t.state, State::Ready);
    assert_eq!(audit_records(&h)[0]["decision"], "aborted");
    // A verdict arriving after the abort does nothing.
    h.t.on_review(1, "c1".into(), review(Verdict::Allow, "ok"));
    assert!(h.ran.borrow().is_empty());
    assert_eq!(h.t.state, State::Ready);
}

#[test]
fn redisplay_interleaves_with_banners() {
    let mut h = harness_with(MuthurSection { uppercase: true, ..cfg() }, |l| {
        exchange(l, "[SHIP TIME X] HI", "HELLO", "end_turn");
        l.append_user("[SHIP TIME X] list files", "ts", AgentId::Tech).unwrap();
        let raw = vec![json!({"type":"tool_use","id":"c1","name":"run","input":run_call("ls", false)})];
        let a = Assistant { provider: Provider::Anthropic, model: "m".into(), text: "".into(), stop: "tool_use".into(), raw: Some(raw), lane: Lane::Tech };
        l.append_assistant(a, "ts").unwrap();
        let r = ToolResult { id: "c1".into(), name: "run".into(), content: "EXIT 0\na".into(), is_error: false, label: "RUN  ls  [ALLOW]".into() };
        l.append_tool(&[r], "ts", &Lane::Tech).unwrap();
        let a = Assistant { provider: Provider::Anthropic, model: "m".into(), text: "Found: one file.".into(), stop: "end_turn".into(), raw: None, lane: Lane::Tech };
        l.append_assistant(a, "ts").unwrap();
        exchange(l, "[SHIP TIME X] BYE", "GOODBYE", "end_turn");
    });
    assert_eq!(h.t.mode, Mode::Persona);
    let rows = screen(&mut h.t, 61, 40);
    let at = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("{needle}: {rows:#?}"));
    let order = [
        at("> HI"),
        at("DIRECT LINK: SCIENCE OFFICER"),
        at("$ list files"),
        at("RUN  ls  [ALLOW]"),
        at("Found: one file."),
        at("DIRECT LINK CLOSED"),
        at("> BYE"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{rows:#?}");
}

#[test]
fn redisplay_closes_a_trailing_direct_link() {
    let h = harness_with(cfg(), |l| {
        l.append_user("[SHIP TIME X] uptime", "ts", AgentId::Tech).unwrap();
        let a = Assistant { provider: Provider::Anthropic, model: "m".into(), text: "Up.".into(), stop: "end_turn".into(), raw: None, lane: Lane::Tech };
        l.append_assistant(a, "ts").unwrap();
    });
    // MUTHUR starts in persona mode, so the screen says the link is closed.
    assert_eq!(h.t.transcript.last(), Some(&Entry::Note("DIRECT LINK CLOSED".into())));
}

#[test]
fn harness_lines_sit_together() {
    let e = [Entry::Command("x".into()), Entry::Harness("RUN  ls  [ALLOW]".into()), Entry::Harness("EXIT 0  1 LINES  0.0 S".into())];
    let lines = view::transcript_lines(&e, None, 40);
    assert_eq!(lines, vec![view::BANNER, "", "$ x", "", "RUN  ls  [ALLOW]", "EXIT 0  1 LINES  0.0 S"]);
}

// ---- Prompt arming, for each of the three status-row prompts ----

#[derive(Clone, Copy, Debug)]
enum P {
    Ask,
    Password,
    Override,
}

const PROMPTS: [P; 3] = [P::Ask, P::Password, P::Override];

/// Raises prompt `p` without rendering it.
fn raise(h: &mut H, p: P) {
    let (root, verdict) = match p {
        P::Ask => (false, Verdict::Ask),
        P::Password => (true, Verdict::Allow),
        P::Override => (false, Verdict::Deny),
    };
    raise_run(h, "systemctl restart nginx", root);
    h.t.on_review(1, "c1".into(), review(verdict, "restarts a service"));
}

/// The keys that accept prompt `p`.
fn accept(h: &mut H, p: P) {
    match p {
        P::Ask => h.t.handle_key(&Key::Char('y')),
        P::Password => {
            type_str(&mut h.t, "pw");
            h.t.handle_key(&Key::Enter);
        }
        P::Override => {
            type_str(&mut h.t, "override");
            h.t.handle_key(&Key::Enter);
        }
    }
}

fn prompting(h: &H) -> bool {
    matches!(h.t.state, State::Ask { .. } | State::Password { .. } | State::OverridePrompt { .. })
}

#[test]
fn prompt_arms_from_second_render() {
    for p in PROMPTS {
        let mut h = harness();
        raise(&mut h, p);
        // In the background: no render for a long while, then the operator looks.
        h.tick(5000);
        accept(&mut h, p);
        assert!(h.ran.borrow().is_empty(), "{p:?}: unseen prompt must not be armed");
        // Drawn once: the frame may still be queued for the wire, so not armed yet.
        screen(&mut h.t, 80, 40);
        h.tick(5000);
        accept(&mut h, p);
        assert!(h.ran.borrow().is_empty(), "{p:?}: a prompt rendered only once must not be armed");
        // The second render starts the arming delay.
        screen(&mut h.t, 80, 40);
        accept(&mut h, p);
        assert!(h.ran.borrow().is_empty(), "{p:?}: keys right after the second render are ignored");
        h.tick(900);
        accept(&mut h, p);
        assert!(h.ran.borrow().is_empty(), "{p:?}");
        assert!(prompting(&h), "{p:?}");
        h.tick(200);
        accept(&mut h, p);
        assert_eq!(h.ran.borrow().len(), 1, "{p:?}");
    }
}

#[test]
fn prompt_ignores_keys_during_arming() {
    for p in PROMPTS {
        let mut h = harness();
        raise(&mut h, p);
        seen(&mut h);
        h.tick(200);
        accept(&mut h, p);
        assert!(h.ran.borrow().is_empty(), "{p:?}");
        assert!(prompting(&h), "{p:?}");
        assert_eq!(h.t.input, "", "{p:?}");
        match &h.t.state {
            State::Password { secret, .. } => assert!(secret.is_empty(), "keys before arming are not typed"),
            State::OverridePrompt { typed, .. } => assert_eq!(typed, ""),
            _ => {}
        }
        h.tick(900);
        accept(&mut h, p);
        assert_eq!(h.ran.borrow().len(), 1, "{p:?}");
    }
}

#[test]
fn prompt_disarmed_while_obscured() {
    for p in PROMPTS {
        // Raised under an overlay: rendering under it never arms.
        let mut h = harness();
        raise(&mut h, p);
        h.t.set_obscured(true);
        for _ in 0..5 {
            screen(&mut h.t, 80, 40);
            h.tick(1000);
        }
        accept(&mut h, p);
        assert!(h.ran.borrow().is_empty(), "{p:?}: a prompt under an overlay must not arm");
        // Uncovered: one render draws it, the next starts the delay.
        h.t.set_obscured(false);
        screen(&mut h.t, 80, 40);
        h.tick(2000);
        accept(&mut h, p);
        assert!(h.ran.borrow().is_empty(), "{p:?}");
        screen(&mut h.t, 80, 40);
        accept(&mut h, p);
        assert!(h.ran.borrow().is_empty(), "{p:?}");
        h.tick(1100);
        accept(&mut h, p);
        assert_eq!(h.ran.borrow().len(), 1, "{p:?}");

        // Already armed, then covered: arming starts over once uncovered.
        let mut h = harness();
        raise(&mut h, p);
        seen(&mut h);
        h.tick(1100);
        h.t.set_obscured(true);
        screen(&mut h.t, 80, 40);
        accept(&mut h, p);
        assert!(h.ran.borrow().is_empty(), "{p:?}: covered prompt is disarmed");
        h.t.set_obscured(false);
        seen(&mut h);
        accept(&mut h, p);
        assert!(h.ran.borrow().is_empty(), "{p:?}: re-armed from scratch");
        h.tick(1100);
        accept(&mut h, p);
        assert_eq!(h.ran.borrow().len(), 1, "{p:?}");
    }
}

#[test]
fn ctrl_c_aborts_prompt_during_arming() {
    for p in PROMPTS {
        for render in [false, true] {
            let mut h = harness();
            raise(&mut h, p);
            if render {
                seen(&mut h);
            }
            h.tick(100);
            h.t.handle_key(&Key::Ctrl('c'));
            assert_eq!(h.t.state, State::Ready, "{p:?}");
            assert!(h.ran.borrow().is_empty(), "{p:?}");
            let recs = records(&h);
            let n = recs.len();
            assert_eq!(recs[n - 2]["results"][0]["content"], "ABORTED BY OPERATOR", "{p:?}");
            assert_eq!(recs[n - 1]["stop"], "interrupted", "{p:?}");
        }
    }
}

#[test]
fn audit_offline_between_prompt_and_answer_refuses_root() {
    let mut h = harness();
    raise(&mut h, P::Password);
    seen(&mut h);
    *h.t.audit.borrow_mut() = AuditLog::offline();
    h.tick(1100);
    accept(&mut h, P::Password);
    assert!(h.ran.borrow().is_empty(), "root must not run unaudited");
    assert_eq!(last_results(&h)[0].content, "NOT AUTHORIZED: AUDIT LOG OFFLINE");
}

#[test]
fn full_command_visible_at_prompts() {
    let long = format!("find /var/lib -xdev -type f -name '*.log' -mtime +30 -size +10M -printf '%p %s\\n' {}", "| sort -k2 -n ".repeat(6));
    assert!(long.len() > 140, "{}", long.len());
    // Joins the transcript rows (without borders and indent) to look for the whole command.
    let joined = |h: &mut H| {
        let rows = screen(&mut h.t, 61, 40);
        rows[1..36].iter().map(|r| r.trim_start_matches('│').trim_end_matches('│').trim()).collect::<Vec<_>>().join(" ")
    };
    for (root, verdict, note_line) in [
        (true, Verdict::Allow, None),
        (true, Verdict::Ask, Some("REVIEW: LOW -- big logs")),
        (true, Verdict::Deny, Some("DENIED BY REVIEWER: big logs")),
        (false, Verdict::Ask, Some("REVIEW: LOW -- big logs")),
    ] {
        let mut h = harness();
        raise_run(&mut h, &long, root);
        h.t.on_review(1, "c1".into(), review(verdict, "big logs"));
        if verdict == Verdict::Deny && root {
            // OVERRIDE first, then the password.
            let all = joined(&mut h);
            assert!(all.contains(&long), "override: {all}");
            assert!(all.contains("DENIED BY REVIEWER: big logs"), "{all}");
            seen(&mut h);
            h.tick(1100);
            type_str(&mut h.t, "override");
            h.t.handle_key(&Key::Enter);
            assert!(matches!(h.t.state, State::Password { .. }));
        }
        let all = joined(&mut h);
        assert!(all.contains(&long), "{verdict:?} root={root}: {all}");
        if let Some(n) = note_line {
            assert!(all.contains(n), "{verdict:?}: {all}");
        }
        // Wrapped with the hanging indent.
        let rows = screen(&mut h.t, 61, 40);
        let first = rows.iter().position(|r| r.contains("find /var/lib")).unwrap();
        assert!(rows[first + 1].starts_with("│      "), "{rows:#?}");
    }
}

#[test]
fn password_prompt_clears_type_ahead() {
    let mut h = harness();
    raise_run(&mut h, "id", true);
    type_str(&mut h.t, "abc");
    assert_eq!(h.t.input, "abc", "typing while reviewing goes to the input line");
    h.t.on_review(1, "c1".into(), review(Verdict::Allow, "ok"));
    assert!(matches!(h.t.state, State::Password { .. }));
    assert_eq!((h.t.input.as_str(), h.t.cursor), ("", 0));
}
