use super::*;
use crate::config::{AgentSection, ToolsSection};
use crate::review::{self, Risk};
use crate::tools::deny::DenyCtx;
use serde_json::{Value, json};

fn tech() -> ToolDefs {
    ToolDefs::technical(&AgentSection::default(), true)
}
fn persona() -> ToolDefs {
    ToolDefs::persona(&ToolsSection::default())
}
fn policy() -> Policy {
    let ctx = DenyCtx {
        config_path: "/home/op/.config/muthur/config.toml".into(),
        state_dir: "/home/op/.local/state/muthur".into(),
        exe: "/home/op/.local/bin/muthur".into(),
        operator: "op".into(),
    };
    Policy {
        deny: Some(DenyRules::new(&ctx, &[]).unwrap()),
        net_allow: vec![],
    }
}
fn no_policy() -> Policy {
    Policy { deny: None, net_allow: vec![] }
}
fn call(id: &str, name: &str, input: Value) -> Call {
    Call {
        id: id.into(),
        name: name.into(),
        input,
    }
}
fn run_args(cmd: &str, as_root: bool) -> Value {
    json!({"command": cmd, "reason": "look", "as_root": as_root, "timeout_secs": 0})
}
fn run_call(id: &str, cmd: &str, as_root: bool) -> Call {
    call(id, "run", run_args(cmd, as_root))
}
fn run_inv(cmd: &str, as_root: bool) -> Invocation {
    Invocation::Run {
        command: cmd.into(),
        reason: "look".into(),
        as_root,
        timeout_secs: 30,
    }
}
fn ship(id: &str) -> Call {
    call(id, "ship_status", json!({}))
}
fn review(verdict: Verdict, risk: Risk, note: &str) -> Review {
    Review {
        verdict,
        risk,
        note: note.into(),
        model: "rev".into(),
        ms: 7,
        error: None,
    }
}
fn allow() -> Review {
    review(Verdict::Allow, Risk::Low, "ok")
}
fn ok(content: &str) -> ToolOutcome {
    ToolOutcome {
        content: content.into(),
        is_error: false,
        argv: None,
        exit: None,
        ms: 12,
        audit_output: None,
    }
}
/// A `run` outcome: model text, full audit text, exit 0, 100 ms.
fn run_ok(cmd: &str, as_root: bool, content: &str, audit: &str) -> ToolOutcome {
    ToolOutcome {
        content: content.into(),
        is_error: false,
        argv: Some(shell::argv(cmd, as_root)),
        exit: Some(json!(0)),
        ms: 100,
        audit_output: Some(audit.into()),
    }
}
fn tturn(msg: &str) -> Turn {
    Turn::new(24, AgentId::Tech, msg.into())
}
fn pturn() -> Turn {
    Turn::new(8, AgentId::Muthur, "status".into())
}
fn start(t: &mut Turn, calls: Vec<Call>, online: bool) -> Vec<Effect> {
    t.on_done(&Stop::ToolUse, calls, &tech(), &policy(), online)
}
fn audits(effects: &[Effect]) -> Vec<&AuditEntry> {
    effects.iter().filter_map(|e| if let Effect::Audit(a) = e { Some(a) } else { None }).collect()
}
fn notes(effects: &[Effect]) -> Vec<&str> {
    effects
        .iter()
        .filter_map(|e| if let Effect::Note(n) = e { Some(n.as_str()) } else { None })
        .collect()
}
fn logged(effects: &[Effect]) -> Vec<&Vec<ToolResult>> {
    effects.iter().filter_map(|e| if let Effect::LogTool(r) = e { Some(r) } else { None }).collect()
}
fn reviews(effects: &[Effect]) -> Vec<&ReviewInput> {
    effects
        .iter()
        .filter_map(|e| if let Effect::Review { input, .. } = e { Some(input) } else { None })
        .collect()
}
fn kind(e: &Effect) -> &'static str {
    match e {
        Effect::Spawn { .. } => "spawn",
        Effect::Review { .. } => "review",
        Effect::Confirm { .. } => "confirm",
        Effect::AskPassword { .. } => "password",
        Effect::AskOverride { .. } => "override",
        Effect::Run { .. } => "run",
        Effect::Delegate { .. } => "delegate",
        Effect::Note(_) => "note",
        Effect::LogTool(_) => "log",
        Effect::Audit(_) => "audit",
        Effect::Orphan(_) => "orphan",
        Effect::Finished { .. } => "finished",
    }
}
fn has(effects: &[Effect], k: &str) -> bool {
    effects.iter().any(|e| kind(e) == k)
}
/// One run call taken to its review verdict.
fn reviewed(cmd: &str, as_root: bool, r: Review, online: bool) -> (Turn, Vec<Effect>) {
    let mut t = tturn("check the box");
    let e = start(&mut t, vec![run_call("c1", cmd, as_root)], true);
    assert_eq!(e.iter().map(kind).collect::<Vec<_>>(), ["review"], "{e:?}");
    assert!(t.reviewing());
    let e = t.on_review("c1", r, online);
    (t, e)
}

#[test]
fn final_answer_finishes() {
    for (stop, calls) in [(Stop::End, vec![]), (Stop::ToolUse, vec![]), (Stop::MaxTokens, vec![ship("c1")])] {
        assert_eq!(
            pturn().on_done(&stop, calls, &persona(), &no_policy(), true),
            vec![Effect::Finished { truncated: false }]
        );
    }
}

#[test]
fn matrix_verdict_x_root() {
    let cases = [
        (Verdict::Allow, false, "run"),
        (Verdict::Allow, true, "password"),
        (Verdict::Ask, false, "confirm"),
        (Verdict::Ask, true, "password"),
        (Verdict::Deny, false, "override"),
        (Verdict::Deny, true, "override"),
    ];
    for (v, root, want) in cases {
        let (t, e) = reviewed("ls", root, review(v, Risk::Medium, "n"), true);
        assert_eq!(e.last().map(kind), Some(want), "{v:?} root={root}: {e:?}");
        assert!(!t.reviewing());
        if want == "run" {
            assert!(t.running());
            assert_eq!(t.pending_prompt(), None);
        } else {
            assert_eq!(t.pending_prompt(), Some("c1"));
        }
    }
}

#[test]
fn confirm_yes_no() {
    let (mut t, e) = reviewed("ls", false, review(Verdict::Ask, Risk::Medium, "changes things"), true);
    assert_eq!(
        e,
        vec![Effect::Confirm {
            call_id: "c1".into(),
            prompt: "ls".into(),
            note: "changes things".into()
        }]
    );
    // Answering for another call does nothing.
    assert_eq!(t.on_confirm("c9", true, true), vec![]);
    let e = t.on_confirm("c1", true, true);
    assert_eq!(
        e.last(),
        Some(&Effect::Run {
            call_id: "c1".into(),
            invocation: run_inv("ls", false)
        })
    );
    let e = t.on_tool_done("c1", run_ok("ls", false, "EXIT 0\nx", "x\n"));
    let a = audits(&e);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].decision, Decision::Confirmed);
    assert_eq!(a[0].agent, Some(AgentId::Tech));
    assert_eq!(a[0].reason.as_deref(), Some("look"));
    assert_eq!(a[0].review, Some(review(Verdict::Ask, Risk::Medium, "changes things").record()));
    assert_eq!(a[0].output, "x\n");
    assert_eq!(e.last(), Some(&Effect::Spawn { forced_final: false }));

    let (mut t, _) = reviewed("ls", false, review(Verdict::Ask, Risk::Medium, "changes things"), true);
    let e = t.on_confirm("c1", false, true);
    assert!(!has(&e, "run"), "{e:?}");
    let a = audits(&e);
    assert_eq!((a[0].decision, a[0].output.as_str()), (Decision::Denied, "DENIED BY OPERATOR"));
    assert_eq!(logged(&e)[0][0].content, "DENIED BY OPERATOR");
    assert!(logged(&e)[0][0].is_error);
    assert_eq!(notes(&e)[0], "RUN  ls  [DENIED BY OPERATOR]");
    assert_eq!(e.last(), Some(&Effect::Spawn { forced_final: false }));
}

#[test]
fn password_given_and_esc() {
    let (mut t, e) = reviewed("id", true, allow(), true);
    assert_eq!(
        e,
        vec![Effect::AskPassword {
            call_id: "c1".into(),
            prompt: "id".into()
        }]
    );
    let e = t.on_password("c1", true, true);
    assert_eq!(
        e.last(),
        Some(&Effect::Run {
            call_id: "c1".into(),
            invocation: run_inv("id", true)
        })
    );
    let e = t.on_tool_done("c1", run_ok("id", true, "EXIT 0\nroot", "uid=0\n"));
    let a = audits(&e);
    assert_eq!(a[0].decision, Decision::Confirmed);
    assert!(a[0].as_root);
    assert_eq!(a[0].argv, Some(shell::argv("id", true)));
    assert_eq!(a[0].exit, Some(json!(0)));

    let (mut t, _) = reviewed("id", true, allow(), true);
    let e = t.on_password("c1", false, true);
    assert!(!has(&e, "run"), "{e:?}");
    let a = audits(&e);
    assert_eq!((a[0].decision, a[0].output.as_str()), (Decision::Denied, "DENIED BY OPERATOR"));
    assert_eq!(notes(&e)[0], "RUN# id  [ROOT, DENIED BY OPERATOR]");
}

#[test]
fn override_then_password_is_overridden() {
    let (mut t, e) = reviewed("id", true, review(Verdict::Deny, Risk::High, "too risky"), true);
    assert_eq!(
        e,
        vec![Effect::AskOverride {
            call_id: "c1".into(),
            prompt: "id".into(),
            note: "too risky".into()
        }]
    );
    let e = t.on_override("c1", true, true);
    assert_eq!(
        e,
        vec![Effect::AskPassword {
            call_id: "c1".into(),
            prompt: "id".into()
        }]
    );
    let e = t.on_password("c1", true, true);
    assert_eq!(notes(&e), ["RUN# id  [ROOT, OVERRIDDEN]", "     REVIEW: HIGH -- too risky"]);
    assert!(has(&e, "run"));
    let e = t.on_tool_done("c1", run_ok("id", true, "EXIT 0", ""));
    assert_eq!(audits(&e)[0].decision, Decision::Overridden);
    // Unprivileged: OVERRIDE runs at once.
    let (mut t, _) = reviewed("ls", false, review(Verdict::Deny, Risk::High, "too risky"), true);
    let e = t.on_override("c1", true, true);
    assert_eq!(e.last().map(kind), Some("run"));
    let e = t.on_tool_done("c1", run_ok("ls", false, "EXIT 0", ""));
    assert_eq!(audits(&e)[0].decision, Decision::Overridden);
}

#[test]
fn override_esc_denied_by_reviewer() {
    let (mut t, _) = reviewed("ls", false, review(Verdict::Deny, Risk::High, "too risky"), true);
    let e = t.on_override("c1", false, true);
    assert!(!has(&e, "run"), "{e:?}");
    let a = audits(&e);
    assert_eq!((a[0].decision, a[0].output.as_str()), (Decision::Denied, "DENIED BY REVIEWER: too risky"));
    assert_eq!(logged(&e)[0][0].content, "DENIED BY REVIEWER: too risky");
    assert_eq!(notes(&e), ["RUN  ls  [DENIED BY REVIEWER]", "     REVIEW: HIGH -- too risky"]);
}

#[test]
fn deny_rule_never_reviewed() {
    let mut t = tturn("list it");
    let e = start(&mut t, vec![run_call("c1", "sudo ls", false)], true);
    assert!(!has(&e, "review"), "{e:?}");
    let a = audits(&e);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].decision, Decision::Rejected);
    assert_eq!(a[0].deny_rule.as_deref(), Some("USE AS_ROOT"));
    assert_eq!(a[0].review, None);
    assert_eq!(logged(&e)[0][0].content, "NOT AUTHORIZED: USE AS_ROOT");
    assert_eq!(notes(&e), ["RUN  sudo ls  [DENIED: USE AS_ROOT]"]);
    assert_eq!(logged(&e)[0][0].label, "RUN  sudo ls  [DENIED: USE AS_ROOT]");
    assert_eq!(e.last(), Some(&Effect::Spawn { forced_final: false }));
}

#[test]
fn tech_run_refused_without_deny_rules() {
    let mut t = tturn("list it");
    let e = t.on_done(&Stop::ToolUse, vec![run_call("c1", "ls", false), ship("c2")], &tech(), &no_policy(), true);
    assert_eq!(e.iter().filter(|e| kind(e) == "review").count(), 0, "{e:?}");
    let a = audits(&e);
    assert_eq!(a[0].decision, Decision::Rejected);
    assert_eq!(a[0].deny_rule, None);
    assert_eq!(a[0].output, "NOT AUTHORIZED: DENY RULES OFFLINE");
    assert_eq!(notes(&e)[0], "RUN  ls  [DENIED: DENY RULES OFFLINE]");
    // Fixed tools still run.
    assert!(e.iter().any(|e| matches!(e, Effect::Run { call_id, .. } if call_id == "c2")), "{e:?}");
}

#[test]
fn sequential_order() {
    let mut t = tturn("look around");
    let e = start(&mut t, vec![run_call("c1", "ls", false), run_call("c2", "df", false)], true);
    assert_eq!(e.iter().map(kind).collect::<Vec<_>>(), ["review"]);
    assert!(matches!(&e[0], Effect::Review { call_id, .. } if call_id == "c1"));
    let e = t.on_review("c1", allow(), true);
    assert!(!has(&e, "review"), "{e:?}");
    assert_eq!(e.last().map(kind), Some("run"));
    // A review answer for the queued call is ignored.
    assert_eq!(t.on_review("c2", allow(), true), vec![]);
    let e = t.on_tool_done("c1", run_ok("ls", false, "EXIT 0", ""));
    assert!(!has(&e, "log"), "{e:?}");
    assert!(matches!(e.last(), Some(Effect::Review { call_id, .. }) if call_id == "c2"));
    t.on_review("c2", allow(), true);
    let e = t.on_tool_done("c2", run_ok("df", false, "EXIT 0", ""));
    let ids: Vec<&str> = logged(&e)[0].iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["c1", "c2"]);
    assert_eq!(e.last(), Some(&Effect::Spawn { forced_final: false }));
}

#[test]
fn free_tools_skip_review() {
    let mut t = tturn("status");
    let e = start(&mut t, vec![ship("c1"), run_call("c2", "ls", false)], true);
    assert_eq!(
        e,
        vec![
            Effect::Note("SHIP STATUS".into()),
            Effect::Run {
                call_id: "c1".into(),
                invocation: Invocation::ShipStatus
            }
        ]
    );
    let e = t.on_tool_done("c1", ok("UP"));
    let a = audits(&e);
    assert_eq!((a[0].decision, a[0].review.clone()), (Decision::Auto, None));
    assert!(matches!(e.last(), Some(Effect::Review { call_id, .. }) if call_id == "c2"));
}

#[test]
fn unknown_target_forced_to_ask() {
    let ping = || call("c1", "ping", json!({"host": "example.com", "count": 3}));
    let mut t = tturn("check the site");
    let e = start(&mut t, vec![ping()], true);
    assert!(reviews(&e)[0].unknown_target);
    let e = t.on_review("c1", review(Verdict::Allow, Risk::Low, "harmless"), true);
    assert_eq!(
        e,
        vec![Effect::Confirm {
            call_id: "c1".into(),
            prompt: "PING example.com 3".into(),
            note: "harmless -- UNLISTED TARGET".into()
        }]
    );
    let e = t.on_confirm("c1", true, true);
    assert_eq!(notes(&e), ["PING example.com 3  [ASK: LOW]", "     REVIEW: LOW -- harmless -- UNLISTED TARGET"]);
    // Deny stays deny.
    let mut t = tturn("check the site");
    start(&mut t, vec![ping()], true);
    let e = t.on_review("c1", review(Verdict::Deny, Risk::High, "exfil"), true);
    assert_eq!(
        e,
        vec![Effect::AskOverride {
            call_id: "c1".into(),
            prompt: "PING example.com 3".into(),
            note: "exfil".into()
        }]
    );
    // Named by the operator: allowed.
    let mut t = tturn("ping example.com");
    let e = start(&mut t, vec![ping()], true);
    assert!(!reviews(&e)[0].unknown_target);
    let e = t.on_review("c1", review(Verdict::Allow, Risk::Low, "harmless"), true);
    assert_eq!(e.last().map(kind), Some("run"));
    // Or on the allow list.
    let mut t = tturn("check the site");
    let p = Policy {
        net_allow: vec!["example.com".into()],
        ..policy()
    };
    let e = t.on_done(&Stop::ToolUse, vec![ping()], &tech(), &p, true);
    assert!(!reviews(&e)[0].unknown_target);
}

#[test]
fn reviewer_never_sees_output() {
    let mut t = tturn("look around");
    start(&mut t, vec![run_call("c1", "ls", false), run_call("c2", "df", false)], true);
    t.on_review("c1", allow(), true);
    let e = t.on_tool_done("c1", run_ok("ls", false, "EXIT 0\nSENTINEL-OUTPUT", "SENTINEL-OUTPUT\n"));
    let input = reviews(&e)[0].clone();
    assert_eq!(
        input.prior,
        vec![PriorCall {
            tool: "run".into(),
            args: run_args("ls", false),
            decision: Decision::Auto,
            exit: Some(json!(0))
        }]
    );
    assert_eq!(input.operator_message, "look around");
    assert_eq!((input.tool.as_str(), &input.args), ("run", &run_args("df", false)));
    let req = format!("{:?}", review::request(&input));
    assert!(!req.contains("SENTINEL"), "{req}");
}

#[test]
fn prior_spans_rounds_and_rejections() {
    let mut t = tturn("look around");
    start(&mut t, vec![run_call("c1", "sudo ls", false), ship("c2")], true);
    t.on_tool_done("c2", ok("UP"));
    let e = start(&mut t, vec![run_call("c3", "ls", false)], true);
    let p = &reviews(&e)[0].prior;
    assert_eq!(
        p.iter().map(|p| (p.tool.as_str(), p.decision)).collect::<Vec<_>>(),
        [("run", Decision::Rejected), ("ship_status", Decision::Auto)]
    );
}

#[test]
fn audit_offline_refuses_root_and_override() {
    let refused = |e: &[Effect]| {
        assert!(!has(e, "run") && !has(e, "password") && !has(e, "override"), "{e:?}");
        let a = audits(e);
        assert_eq!((a[0].decision, a[0].output.as_str()), (Decision::Rejected, "NOT AUTHORIZED: AUDIT LOG OFFLINE"));
    };
    // Raised offline: allow + root, ask + root, deny.
    for v in [Verdict::Allow, Verdict::Ask] {
        let (_, e) = reviewed("id", true, review(v, Risk::Low, "ok"), false);
        refused(&e);
        assert_eq!(notes(&e)[0], "RUN# id  [ROOT, DENIED: AUDIT LOG OFFLINE]");
    }
    let (_, e) = reviewed("ls", false, review(Verdict::Deny, Risk::High, "no"), false);
    refused(&e);
    // Raised online, answered offline: the password, and the OVERRIDE.
    let (mut t, _) = reviewed("id", true, allow(), true);
    refused(&t.on_password("c1", true, false));
    let (mut t, _) = reviewed("ls", false, review(Verdict::Deny, Risk::High, "no"), true);
    refused(&t.on_override("c1", true, false));
    let (mut t, _) = reviewed("id", true, review(Verdict::Deny, Risk::High, "no"), true);
    refused(&t.on_override("c1", true, false));
    // Refusing while offline is still a plain denial.
    let (mut t, _) = reviewed("id", true, allow(), true);
    assert_eq!(audits(&t.on_password("c1", false, false))[0].decision, Decision::Denied);
    // Unprivileged allow and ask still run offline; so do free tools.
    let (_, e) = reviewed("ls", false, allow(), false);
    assert_eq!(e.last().map(kind), Some("run"));
    let (mut t, _) = reviewed("ls", false, review(Verdict::Ask, Risk::Low, "ok"), false);
    assert_eq!(t.on_confirm("c1", true, false).last().map(kind), Some("run"));
    let mut t = tturn("x");
    assert_eq!(start(&mut t, vec![ship("c1")], false).last().map(kind), Some("run"));
}

#[test]
fn abort_each_stage() {
    // (stage, turn at that stage, expected text, orphaned)
    let stages: Vec<(&str, Turn)> = vec![
        ("reviewing", {
            let mut t = tturn("x");
            start(&mut t, vec![run_call("c1", "ls", false)], true);
            t
        }),
        ("confirm", reviewed("ls", false, review(Verdict::Ask, Risk::Low, "ok"), true).0),
        ("password", reviewed("id", true, allow(), true).0),
        ("override", reviewed("ls", false, review(Verdict::Deny, Risk::High, "no"), true).0),
    ];
    for (stage, mut t) in stages {
        let e = t.abort();
        let a = audits(&e);
        assert_eq!(a.len(), 1, "{stage}: {e:?}");
        assert_eq!((a[0].decision, a[0].output.as_str()), (Decision::Aborted, ABORTED), "{stage}");
        assert!(!has(&e, "orphan"), "{stage}");
        assert_eq!(logged(&e).len(), 1, "{stage}");
        assert_eq!(logged(&e)[0][0].content, ABORTED, "{stage}");
        assert!(notes(&e)[0].ends_with("[ABORTED]") || notes(&e)[0].ends_with(", ABORTED]"), "{stage}: {e:?}");
        assert_eq!(e.last(), Some(&Effect::Finished { truncated: false }), "{stage}");
        assert_eq!(t.pending_prompt(), None);
        assert!(!t.reviewing() && !t.running());
    }

    // Running unprivileged: cleanup is the tab's job; the late outcome is an orphan.
    let (mut t, _) = reviewed("ls", false, allow(), true);
    let e = t.abort();
    let a = audits(&e);
    assert_eq!((a.len(), a[0].output.as_str(), a[0].argv.clone()), (1, ABORTED, None));
    assert_eq!(notes(&e), ["ABORTED"]);
    let Some(Effect::Orphan(o)) = e.iter().find(|x| kind(x) == "orphan") else {
        panic!("{e:?}")
    };
    assert_eq!(o.call_id, "c1");
    assert_eq!(o.audit(run_ok("ls", false, "EXIT 0", "late")).decision, Decision::Auto);
    assert_eq!(o.audit(run_ok("ls", false, "EXIT 0", "late")).output, "late");
    assert_eq!(logged(&e).len(), 1);
    assert_eq!(e.last(), Some(&Effect::Finished { truncated: false }));
    assert_eq!(t.on_tool_done("c1", ok("late")), vec![]);

    // A running review-free tool ends its label line with ABORTED too.
    let mut t = tturn("x");
    start(&mut t, vec![ship("c1")], true);
    assert!(t.running());
    let e = t.abort();
    assert_eq!(notes(&e), ["ABORTED"]);

    // Running root: issued, outcome unknown.
    let (mut t, _) = reviewed("id", true, allow(), true);
    t.on_password("c1", true, true);
    let e = t.abort();
    let a = audits(&e);
    assert_eq!(a.len(), 1);
    assert_eq!((a[0].decision, a[0].output.as_str()), (Decision::Aborted, ABORTED_ISSUED));
    assert_eq!(a[0].argv, Some(shell::argv("id", true)));
    assert!(a[0].as_root);
    assert_eq!(notes(&e), ["OUTCOME UNKNOWN"]);
    let Some(Effect::Orphan(o)) = e.iter().find(|x| kind(x) == "orphan") else {
        panic!("{e:?}")
    };
    let late = o.audit(run_ok("id", true, "EXIT 0", "done"));
    assert_eq!(
        (late.decision, late.as_root, late.argv),
        (Decision::Confirmed, true, Some(shell::argv("id", true)))
    );
    assert_eq!(logged(&e)[0][0].content, ABORTED_ISSUED);
    assert_eq!(e.last(), Some(&Effect::Finished { truncated: false }));

    // Pending behind a review: both aborted, one audit each.
    let mut t = tturn("x");
    start(&mut t, vec![run_call("c1", "ls", false), run_call("c2", "df", false)], true);
    let e = t.abort();
    let a = audits(&e);
    assert_eq!(
        a.iter().map(|a| (a.args["command"].as_str().unwrap(), a.decision)).collect::<Vec<_>>(),
        [("ls", Decision::Aborted), ("df", Decision::Aborted)]
    );
    assert_eq!(notes(&e), ["RUN  ls  [ABORTED]", "RUN  df  [ABORTED]"]);
    assert_eq!(logged(&e).len(), 1);
    assert_eq!(logged(&e)[0].len(), 2);
    assert_eq!(e.last(), Some(&Effect::Finished { truncated: false }));

    // With no open round, abort just finishes.
    assert_eq!(tturn("x").abort(), vec![Effect::Finished { truncated: false }]);
}

#[test]
fn tech_lines() {
    let (mut t, e) = reviewed("ls", false, allow(), true);
    assert_eq!(notes(&e), ["RUN  ls  [ALLOW]", "     REVIEW: LOW -- ok"]);
    let e = t.on_tool_done("c1", run_ok("ls", false, "EXIT 0\na\nb", "a\nb\n"));
    assert_eq!(notes(&e), ["EXIT 0  2 LINES  0.1 S"]);
    assert_eq!(logged(&e)[0][0].label, "RUN  ls  [ALLOW]");

    let (mut t, _) = reviewed("id", true, review(Verdict::Ask, Risk::Medium, "root id"), true);
    let e = t.on_password("c1", true, true);
    assert_eq!(notes(&e), ["RUN# id  [ROOT, ASK: MEDIUM]", "     REVIEW: MEDIUM -- root id"]);

    // A cut-down capture makes the line count a floor.
    let (mut t, _) = reviewed("ls", false, allow(), true);
    let big = format!("a\nb{}c\n", crate::ops::omitted(9000));
    let o = ToolOutcome {
        content: "EXIT 0\n…".into(),
        is_error: false,
        argv: None,
        exit: Some(json!(0)),
        ms: 100,
        audit_output: Some(big),
    };
    assert_eq!(notes(&t.on_tool_done("c1", o)), ["EXIT 0  4+ LINES  0.1 S"]);

    // Result words replace the EXIT line.
    for (content, line) in [
        ("NO RESPONSE", "NO RESPONSE"),
        ("NO RESPONSE — OUTCOME UNKNOWN", "NO RESPONSE — OUTCOME UNKNOWN"),
        ("INSUFFICIENT CLEARANCE", "INSUFFICIENT CLEARANCE"),
        (ABORTED_ISSUED, "OUTCOME UNKNOWN"),
        (ABORTED, "ABORTED"),
    ] {
        let (mut t, _) = reviewed("ls", false, allow(), true);
        let o = ToolOutcome {
            content: content.into(),
            is_error: true,
            argv: None,
            exit: Some(json!("timeout")),
            ms: 5,
            audit_output: None,
        };
        assert_eq!(notes(&t.on_tool_done("c1", o)), [line]);
    }

    // A reviewed fixed tool shows its label and tag; free ones just the label.
    let mut t = tturn("ping example.com");
    start(&mut t, vec![call("c1", "ping", json!({"host": "example.com", "count": 3}))], true);
    let e = t.on_review("c1", allow(), true);
    assert_eq!(notes(&e), ["PING example.com 3  [ALLOW]", "     REVIEW: LOW -- ok"]);
    assert_eq!(notes(&t.on_tool_done("c1", ok("3 packets"))), Vec::<&str>::new());

    // An invalid call is denied with its reason.
    let mut t = tturn("x");
    let e = start(&mut t, vec![call("c1", "run", json!({"command": "ls"}))], true);
    assert_eq!(notes(&e), ["RUN  [DENIED: missing reason]"]);
}

#[test]
fn persona_notes_unchanged() {
    let mut t = pturn();
    let e = t.on_done(&Stop::ToolUse, vec![ship("c1")], &persona(), &no_policy(), true);
    assert_eq!(
        e,
        vec![
            Effect::Note("ACCESSING: SHIP STATUS".into()),
            Effect::Run {
                call_id: "c1".into(),
                invocation: Invocation::ShipStatus
            }
        ]
    );
    assert!(t.running());
    let e = t.on_tool_done("c1", ok("UP"));
    assert_eq!(
        e,
        vec![
            Effect::Audit(AuditEntry {
                agent: Some(AgentId::Muthur),
                ms: 12,
                output: "UP".into(),
                ..AuditEntry::new("ship_status", json!({}), Decision::Auto)
            }),
            Effect::LogTool(vec![ToolResult {
                id: "c1".into(),
                name: "ship_status".into(),
                content: "UP".into(),
                is_error: false,
                label: "SHIP STATUS".into()
            }]),
            Effect::Spawn { forced_final: false },
        ]
    );
    // Rejections note ACCESS DENIED; the persona has no run tool.
    let mut t = pturn();
    let e = t.on_done(&Stop::ToolUse, vec![run_call("c1", "ls", false)], &persona(), &no_policy(), true);
    assert_eq!(notes(&e), ["ACCESS DENIED: RUN"]);
    assert_eq!(audits(&e)[0].output, "NOT AUTHORIZED: unknown tool run");
    assert_eq!(audits(&e)[0].agent, Some(AgentId::Muthur));
}

#[test]
fn duplicate_ids_rejected_after_first() {
    for id in ["c1", ""] {
        let mut t = pturn();
        let e = t.on_done(
            &Stop::ToolUse,
            vec![ship(id), call(id, "containers", json!({}))],
            &persona(),
            &no_policy(),
            true,
        );
        assert_eq!(
            e,
            vec![
                Effect::Note("ACCESS DENIED: CONTAINERS".into()),
                Effect::Audit(AuditEntry {
                    agent: Some(AgentId::Muthur),
                    output: "NOT AUTHORIZED: duplicate call id".into(),
                    ..AuditEntry::new("containers", json!({}), Decision::Rejected)
                }),
                Effect::Note("ACCESSING: SHIP STATUS".into()),
                Effect::Run {
                    call_id: id.into(),
                    invocation: Invocation::ShipStatus
                },
            ]
        );
        let e = t.on_tool_done(id, ok("UP"));
        assert_eq!(audits(&e)[0].decision, Decision::Auto);
        let r = logged(&e)[0];
        assert_eq!(
            r.iter().map(|r| r.content.as_str()).collect::<Vec<_>>(),
            ["UP", "NOT AUTHORIZED: duplicate call id"]
        );
        assert_eq!(e.last(), Some(&Effect::Spawn { forced_final: false }));
    }
}

#[test]
fn round_cap_forces_final_then_truncates() {
    let round = |t: &mut Turn, id: &str| {
        let e = t.on_done(&Stop::ToolUse, vec![ship(id)], &persona(), &no_policy(), true);
        assert!(matches!(e.last(), Some(Effect::Run { .. })), "{e:?}");
        t.on_tool_done(id, ok("UP"))
    };
    let mut t = Turn::new(2, AgentId::Muthur, "x".into());
    assert_eq!(round(&mut t, "c1").last(), Some(&Effect::Spawn { forced_final: false }));
    assert_eq!(round(&mut t, "c2").last(), Some(&Effect::Spawn { forced_final: true }));
    // ToolUse in the forced final: rejected, logged, one more forced request.
    let e = t.on_done(&Stop::ToolUse, vec![ship("c3")], &persona(), &no_policy(), true);
    assert!(!has(&e, "run"), "{e:?}");
    assert_eq!(audits(&e)[0].decision, Decision::Rejected);
    assert_eq!(logged(&e)[0][0].content, "NOT AUTHORIZED: ROUND LIMIT");
    assert_eq!(e.last(), Some(&Effect::Spawn { forced_final: true }));
    // Again: logged, then truncated.
    let e = t.on_done(&Stop::ToolUse, vec![ship("c4")], &persona(), &no_policy(), true);
    assert_eq!(logged(&e)[0][0].content, "NOT AUTHORIZED: ROUND LIMIT");
    assert_eq!(e.last(), Some(&Effect::Finished { truncated: true }));

    // 0: the first ToolUse already gets the forced-final treatment.
    let mut t = Turn::new(0, AgentId::Muthur, "x".into());
    let e = t.on_done(&Stop::ToolUse, vec![ship("c1")], &persona(), &no_policy(), true);
    assert_eq!(audits(&e)[0].output, "NOT AUTHORIZED: ROUND LIMIT");
    assert_eq!(e.last(), Some(&Effect::Spawn { forced_final: true }));
    let e = t.on_done(&Stop::ToolUse, vec![ship("c2")], &persona(), &no_policy(), true);
    assert_eq!(e.last(), Some(&Effect::Finished { truncated: true }));
    // 1: one real round, then forced.
    let mut t = Turn::new(1, AgentId::Muthur, "x".into());
    assert_eq!(round(&mut t, "c1").last(), Some(&Effect::Spawn { forced_final: true }));
}

#[test]
fn new_stream_closes_open_round_as_aborted() {
    let mut t = tturn("x");
    start(&mut t, vec![ship("c1"), run_call("c2", "ls", false)], true);
    assert!(t.running());
    let e = start(&mut t, vec![ship("c3")], true);
    assert_eq!(audits(&e).iter().take(2).map(|a| a.decision).collect::<Vec<_>>(), [Decision::Aborted; 2]);
    assert_eq!(e[0], Effect::Note("ABORTED".into()), "the running tool's line ends");
    assert!(matches!(&e[2], Effect::Orphan(o) if o.call_id == "c1"), "{e:?}");
    let r = logged(&e)[0];
    assert_eq!(r.iter().map(|r| r.content.as_str()).collect::<Vec<_>>(), [ABORTED, ABORTED]);
    assert_eq!(
        e.last(),
        Some(&Effect::Run {
            call_id: "c3".into(),
            invocation: Invocation::ShipStatus
        })
    );
    assert_eq!(t.on_tool_done("c1", ok("late")), vec![]);
    // A final answer over an open round: logged as aborted, then Finished.
    let e = t.on_done(&Stop::End, vec![], &tech(), &policy(), true);
    assert_eq!(audits(&e).len(), 1);
    assert_eq!(e.last(), Some(&Effect::Finished { truncated: false }));
}

#[test]
fn stale_outcome_ignored() {
    let mut t = pturn();
    assert_eq!(t.on_tool_done("nope", ok("x")), vec![]);
    t.on_done(&Stop::ToolUse, vec![ship("c1")], &persona(), &no_policy(), true);
    t.on_tool_done("c1", ok("UP"));
    assert_eq!(t.on_tool_done("c1", ok("again")), vec![]);
    assert_eq!(t.on_review("c1", allow(), true), vec![]);
    assert_eq!(t.on_password("c1", true, true), vec![]);
    assert_eq!(t.on_override("c1", true, true), vec![]);
}

fn delegate_call(id: &str, task: &str) -> Call {
    call(id, "delegate", json!({"task": task}))
}
fn report(lines: &[&str], text: &str, truncated: bool) -> DelegateOutcome {
    DelegateOutcome::Report {
        lines: lines.iter().map(|s| s.to_string()).collect(),
        report: text.into(),
        truncated,
    }
}
fn delegated(t: &mut Turn) -> Vec<Effect> {
    t.on_done(&Stop::ToolUse, vec![delegate_call("d1", "check nginx")], &persona(), &no_policy(), true)
}

#[test]
fn delegate_emits_effect_without_note() {
    let mut t = pturn();
    let e = delegated(&mut t);
    assert_eq!(
        e,
        vec![Effect::Delegate {
            call_id: "d1".into(),
            task: "check nginx".into(),
            operator_message: "status".into()
        }]
    );
    assert_eq!(t.delegating(), Some("d1"));
}

#[test]
fn delegate_result_content() {
    let r = report(&["SCIENCE OFFICER: uptime  [ALLOW]"], "Up 3 days.", false);
    assert_eq!(r.content("SCIENCE OFFICER"), "COMMANDS:\nSCIENCE OFFICER: uptime  [ALLOW]\nREPORT:\nUp 3 days.");
    assert_eq!(
        report(&[], "Up 3 days.", true).content("SCIENCE OFFICER"),
        "COMMANDS:\nNONE\nREPORT (TRUNCATED):\nUp 3 days."
    );
    let n = DelegateOutcome::NoResponse { lines: vec![] };
    assert_eq!(n.content("SCIENCE OFFICER"), "COMMANDS:\nNONE\nNO RESPONSE FROM SCIENCE OFFICER");
}

#[test]
fn on_delegate_resolves_and_spawns() {
    let mut t = pturn();
    delegated(&mut t);
    let out = report(&["SCIENCE OFFICER: uptime  [ALLOW]"], "Up 3 days.", false);
    let content = out.content("SCIENCE OFFICER");
    let e = t.on_delegate("d1", out, "SCIENCE OFFICER");
    let a = audits(&e);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].tool, "delegate");
    assert_eq!(a[0].agent, Some(AgentId::Muthur));
    assert_eq!(a[0].decision, Decision::Auto);
    assert_eq!(a[0].args, json!({"task": "check nginx"}));
    assert_eq!(a[0].output, content);
    let l = logged(&e);
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].len(), 1);
    assert!(!l[0][0].is_error);
    assert_eq!(l[0][0].label, "DELEGATE");
    assert_eq!(l[0][0].content, content);
    assert_eq!(e.last(), Some(&Effect::Spawn { forced_final: false }));
    assert!(notes(&e).is_empty());
    assert_eq!(t.delegating(), None);

    let mut t = pturn();
    delegated(&mut t);
    let e = t.on_delegate("d1", DelegateOutcome::NoResponse { lines: vec![] }, "SCIENCE OFFICER");
    assert!(logged(&e)[0][0].is_error);
}

#[test]
fn on_delegate_wrong_id_ignored() {
    let mut t = pturn();
    delegated(&mut t);
    assert_eq!(t.on_delegate("zz", DelegateOutcome::NoResponse { lines: vec![] }, "X"), vec![]);
    assert_eq!(t.delegating(), Some("d1"));
}

#[test]
fn abort_while_delegating() {
    let mut t = pturn();
    delegated(&mut t);
    let e = t.abort();
    let a = audits(&e);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].decision, Decision::Aborted);
    let r = logged(&e)[0];
    assert_eq!(r[0].content, ABORTED);
    assert!(r[0].is_error);
    assert!(!e.iter().any(|x| matches!(x, Effect::Orphan(_))));
    assert_eq!(e.last(), Some(&Effect::Finished { truncated: false }));
}

#[test]
fn delegate_then_ship_status_sequential() {
    let mut t = pturn();
    let e = t.on_done(
        &Stop::ToolUse,
        vec![delegate_call("d1", "check nginx"), ship("s1")],
        &persona(),
        &no_policy(),
        true,
    );
    assert_eq!(e.len(), 1);
    assert!(matches!(e[0], Effect::Delegate { .. }));
    let e = t.on_delegate("d1", DelegateOutcome::NoResponse { lines: vec![] }, "X");
    assert!(e.iter().any(|x| matches!(x, Effect::Run { call_id, .. } if call_id == "s1")), "{e:?}");
    assert!(!e.iter().any(|x| matches!(x, Effect::Spawn { .. })));
}

#[test]
fn delegate_counts_toward_round_cap() {
    let mut t = Turn::new(1, AgentId::Muthur, "status".into());
    delegated(&mut t);
    let e = t.on_delegate("d1", DelegateOutcome::NoResponse { lines: vec![] }, "X");
    assert_eq!(e.last(), Some(&Effect::Spawn { forced_final: true }));
}

fn dturn() -> Turn {
    Turn::delegated(24, "why is nginx down".into(), "check nginx".into(), "SCIENCE OFFICER".into(), "d1".into())
}

fn ping_call(id: &str, host: &str) -> Call {
    call(id, "ping", json!({"host": host, "count": 3}))
}

#[test]
fn delegated_review_input_separates_task() {
    let mut t = dturn();
    let e = start(&mut t, vec![run_call("c1", "ls", false)], true);
    let r = reviews(&e);
    assert_eq!(
        (r[0].operator_message.as_str(), r[0].delegated_task.as_deref()),
        ("why is nginx down", Some("check nginx"))
    );
    let mut t = tturn("why is nginx down");
    let e = start(&mut t, vec![run_call("c1", "ls", false)], true);
    assert_eq!(reviews(&e)[0].delegated_task, None);
}

#[test]
fn task_only_host_is_unknown_target() {
    let mk = |op: &str| Turn::delegated(24, op.into(), "ping evil.example 3 to verify".into(), "SO".into(), "d1".into());
    let mut t = mk("check the site");
    let e = start(&mut t, vec![ping_call("c1", "evil.example")], true);
    assert!(reviews(&e)[0].unknown_target, "{e:?}");
    let mut t = mk("check the site evil.example");
    let e = start(&mut t, vec![ping_call("c1", "evil.example")], true);
    assert!(!reviews(&e)[0].unknown_target, "{e:?}");
}

#[test]
fn condensed_strips_run_prefix() {
    assert_eq!(condensed("SO", "RUN  uptime  [ALLOW]"), "SO: uptime  [ALLOW]");
    assert_eq!(
        condensed("SO", "RUN# systemctl restart nginx  [ROOT, ASK: MEDIUM]"),
        "SO: systemctl restart nginx  [ROOT, ASK: MEDIUM]"
    );
    assert_eq!(condensed("SO", "SHIP STATUS"), "SO: SHIP STATUS");
}

#[test]
fn delegated_run_shows_one_condensed_line_and_audits_delegator() {
    let mut t = dturn();
    let mut all = start(&mut t, vec![run_call("r1", "uptime", false)], true);
    all.extend(t.on_review("r1", allow(), true));
    all.extend(t.on_tool_done("r1", run_ok("uptime", false, "EXIT 0\nup", "up")));
    let want = vec!["SCIENCE OFFICER: uptime  [ALLOW]".to_string()];
    assert_eq!(notes(&all), want.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(t.lines(), want);
    assert_eq!(logged(&all)[0][0].label, "RUN  uptime  [ALLOW]");
    let a = audits(&all);
    assert!(!a.is_empty());
    for x in a {
        assert_eq!((x.delegated_by.as_deref(), x.agent), (Some("d1"), Some(AgentId::Tech)));
    }
    assert!(tturn("x").lines().is_empty());
}

#[test]
fn delegated_refusals_are_condensed() {
    let mut t = dturn();
    let e = start(&mut t, vec![run_call("c1", "sudo ls", false)], true);
    let n = notes(&e);
    assert_eq!(n.len(), 1, "{e:?}");
    assert!(n[0].starts_with("SCIENCE OFFICER: ") && n[0].ends_with("[DENIED: USE AS_ROOT]"), "{n:?}");

    let mut t = dturn();
    start(&mut t, vec![run_call("c1", "ls", false)], true);
    let mut e = t.on_review("c1", review(Verdict::Deny, Risk::High, "no"), true);
    e.extend(t.on_override("c1", false, true));
    let n = notes(&e);
    assert_eq!(n.len(), 1, "{e:?}");
    assert!(n[0].starts_with("SCIENCE OFFICER: ") && n[0].ends_with("[DENIED BY REVIEWER]"), "{n:?}");
}

#[test]
fn delegated_abort_running_root_no_end_line() {
    let mut t = dturn();
    start(&mut t, vec![run_call("c1", "id", true)], true);
    t.on_review("c1", allow(), true);
    let e = t.on_password("c1", true, true);
    assert!(has(&e, "run"), "{e:?}");
    let e = t.abort();
    assert!(notes(&e).is_empty(), "{e:?}");
    let Some(Effect::Orphan(o)) = e.iter().find(|x| kind(x) == "orphan") else {
        panic!("{e:?}")
    };
    assert_eq!(o.audit(run_ok("id", true, "EXIT 0", "late")).delegated_by.as_deref(), Some("d1"));
}
