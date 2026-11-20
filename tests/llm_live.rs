//! Live round trips against real endpoints. User-run only:
//!   MUTHUR_LIVE_ANTHROPIC_KEY=... cargo test --test llm_live anthropic -- --ignored --nocapture
//!   MUTHUR_LIVE_OPENAI_URL=... MUTHUR_LIVE_OPENAI_MODEL=... cargo test --test llm_live openai -- --ignored --nocapture
//! Token usage (cache hits etc.) is logged by the client to muthur.log; cache
//! hits are not asserted because short chats sit under the 512-token minimum.

use muthur::chatlog::{Assistant, ChatLog};
use muthur::config::{MuthurSection, Provider};
use muthur::llm::client::LlmClient;
use muthur::llm::{Stop, StreamEvent, ToolResult};
use muthur::metrics::Snapshot;
use muthur::tools::exec::{ExecCtx, SnapshotSlot, execute};
use muthur::ops::runner::SystemRunner;
use muthur::tools::ToolDefs;
use muthur::config::ToolsSection;

const SYSTEM: &str = "You are a terse test responder. Answer in at most ten words.";

/// One turn: stream the window, assert text and a clean end, return (text, raw, model).
async fn turn(client: &LlmClient, cfg: &MuthurSection, log: &mut ChatLog, user: &str) -> Assistant {
    log.append_user(user, "2026-11-18T00:00:00Z").unwrap();
    let messages = log.window(cfg.provider, cfg.model().unwrap_or_default(), SYSTEM, "", cfg.context_tokens).unwrap();
    let mut text = String::new();
    let mut done = None;
    client
        .stream(muthur::llm::ChatRequest { system: SYSTEM.into(), messages, tools: vec![], tool_choice_none: false }, |ev| match ev {
            StreamEvent::Text(t) => text.push_str(&t),
            StreamEvent::Done { stop, model, raw } => done = Some((stop, model, raw)),
            StreamEvent::Error(e) => panic!("stream error: {e:?}"),
            StreamEvent::ToolCall { .. } => {}
        })
        .await;
    let (stop, model, raw) = done.expect("no Done event");
    assert_eq!(stop, Stop::End);
    let stop = stop.log_str().to_string();
    assert!(!text.trim().is_empty(), "empty reply");
    println!("--- reply: {text}");
    Assistant { provider: cfg.provider, model: model.unwrap_or_default(), text, stop, raw }
}

async fn round_trip(cfg: MuthurSection, first: &str) {
    let dir = tempfile::tempdir().unwrap();
    let mut log = ChatLog::open(dir.path().join("chat.jsonl")).unwrap();
    let client = LlmClient::new(cfg.clone()).unwrap();
    let a = turn(&client, &cfg, &mut log, first).await;
    if cfg.provider == Provider::Anthropic {
        // Not asserted: adaptive thinking may skip a short problem.
        let thought = a.raw.as_ref().is_some_and(|r| r.iter().any(|b| b["type"] == "thinking"));
        println!("--- turn 1 raw contains a thinking block: {thought}");
    }
    log.append_assistant(a, "2026-11-18T00:00:01Z").unwrap();
    // Replays the first answer (with thinking blocks on Anthropic) in the window.
    let b = turn(&client, &cfg, &mut log, "REPEAT YOUR LAST ANSWER IN THREE WORDS.").await;
    log.append_assistant(b, "2026-11-18T00:00:02Z").unwrap();
}

#[tokio::test]
#[ignore = "needs MUTHUR_LIVE_ANTHROPIC_KEY and network"]
async fn anthropic_round_trip() {
    let Ok(key) = std::env::var("MUTHUR_LIVE_ANTHROPIC_KEY") else {
        println!("skipped: MUTHUR_LIVE_ANTHROPIC_KEY not set");
        return;
    };
    // effort=high and a small reasoning problem so turn 1 likely carries a
    // signed thinking block that turn 2 must replay.
    round_trip(
        MuthurSection { provider: Provider::Anthropic, api_key: Some(key), effort: "high".into(), ..MuthurSection::default() },
        "A ship travels 3 light-years at 0.5c, then 2 light-years at 0.25c. Total travel time in years? Answer with the number only.",
    )
    .await;
}

#[tokio::test]
#[ignore = "needs MUTHUR_LIVE_OPENAI_URL, MUTHUR_LIVE_OPENAI_MODEL and network"]
async fn openai_round_trip() {
    let (Ok(url), Ok(model)) = (std::env::var("MUTHUR_LIVE_OPENAI_URL"), std::env::var("MUTHUR_LIVE_OPENAI_MODEL")) else {
        println!("skipped: MUTHUR_LIVE_OPENAI_URL / MUTHUR_LIVE_OPENAI_MODEL not set");
        return;
    };
    round_trip(MuthurSection {
        provider: Provider::Openai,
        base_url: Some(url),
        model: Some(model),
        api_key: std::env::var("MUTHUR_LIVE_OPENAI_KEY").ok(),
        ..MuthurSection::default()
    }, "STATE YOUR DESIGNATION.")
    .await;
}

/// Hand-driven tool loop (tier 1 only): stream, collect ToolCalls, execute,
/// append_tool, window, stream again; at most 3 rounds.
async fn tool_round_trip(cfg: MuthurSection) {
    let defs = ToolDefs::new(&ToolsSection::default());
    let slot: SnapshotSlot = SnapshotSlot::default();
    *slot.lock().unwrap() = Some(Snapshot { host: "NOSTROMO".into(), uptime_secs: 3 * 86400, ..Default::default() });
    let ctx = ExecCtx { runner: std::sync::Arc::new(SystemRunner), snapshot: slot, elevate: defs.elevate() };
    let dir = tempfile::tempdir().unwrap();
    let mut log = ChatLog::open(dir.path().join("chat.jsonl")).unwrap();
    let client = LlmClient::new(cfg.clone()).unwrap();
    log.append_user("WHAT IS THE SHIP'S UPTIME? USE YOUR TOOLS.", "2026-11-18T00:00:00Z").unwrap();
    let mut rounds = 0;
    let mut final_stop = None;
    let mut final_text = String::new();
    for _ in 0..4 {
        let messages = log.window(cfg.provider, cfg.model().unwrap_or_default(), SYSTEM, &defs.json(), cfg.context_tokens).unwrap();
        let mut text = String::new();
        let mut calls = Vec::new();
        let mut done = None;
        client
            .stream(
                muthur::llm::ChatRequest { system: SYSTEM.into(), messages, tools: defs.specs().to_vec(), tool_choice_none: rounds >= 3 },
                |ev| match ev {
                    StreamEvent::Text(t) => text.push_str(&t),
                    StreamEvent::ToolCall { id, name, input } => calls.push((id, name, input)),
                    StreamEvent::Done { stop, model, raw } => done = Some((stop, model, raw)),
                    StreamEvent::Error(e) => panic!("stream error: {e:?}"),
                },
            )
            .await;
        let (stop, model, raw) = done.expect("no Done event");
        if cfg.provider == Provider::Anthropic {
            let blocks = raw.clone().unwrap_or_default();
            let think = blocks.iter().position(|b| b["type"] == "thinking");
            let tool = blocks.iter().position(|b| b["type"] == "tool_use");
            println!("--- thinking block precedes tool_use: {}", matches!((think, tool), (Some(a), Some(b)) if a < b));
        }
        let a = Assistant { provider: cfg.provider, model: model.unwrap_or_default(), text: text.clone(), stop: stop.log_str().to_string(), raw };
        log.append_assistant(a, "2026-11-18T00:00:01Z").unwrap();
        if stop != Stop::ToolUse || calls.is_empty() {
            final_stop = Some(stop);
            final_text = text;
            break;
        }
        rounds += 1;
        let results: Vec<ToolResult> = calls
            .into_iter()
            .map(|(id, name, input)| {
                let (content, is_error) = match defs.validate(&name, &input) {
                    Ok(inv) => {
                        let o = execute(&inv, &ctx);
                        (o.content, o.is_error)
                    }
                    Err(e) => (e, true),
                };
                println!("--- tool {name}: {content}");
                ToolResult { id, name, content, is_error, label: String::new() }
            })
            .collect();
        log.append_tool(&results, "2026-11-18T00:00:02Z").unwrap();
    }
    println!("--- final text: {final_text}");
    assert!(rounds >= 1, "no tool round happened");
    assert_eq!(final_stop, Some(Stop::End));
}

#[tokio::test]
#[ignore = "needs MUTHUR_LIVE_ANTHROPIC_KEY and network"]
async fn anthropic_tool_round_trip() {
    let Ok(key) = std::env::var("MUTHUR_LIVE_ANTHROPIC_KEY") else {
        println!("skipped: MUTHUR_LIVE_ANTHROPIC_KEY not set");
        return;
    };
    tool_round_trip(MuthurSection { provider: Provider::Anthropic, api_key: Some(key), effort: "high".into(), ..MuthurSection::default() }).await;
}

#[tokio::test]
#[ignore = "needs MUTHUR_LIVE_OPENAI_URL, MUTHUR_LIVE_OPENAI_MODEL and network"]
async fn openai_tool_round_trip() {
    let (Ok(url), Ok(model)) = (std::env::var("MUTHUR_LIVE_OPENAI_URL"), std::env::var("MUTHUR_LIVE_OPENAI_MODEL")) else {
        println!("skipped: MUTHUR_LIVE_OPENAI_URL / MUTHUR_LIVE_OPENAI_MODEL not set");
        return;
    };
    tool_round_trip(MuthurSection {
        provider: Provider::Openai,
        base_url: Some(url),
        model: Some(model),
        api_key: std::env::var("MUTHUR_LIVE_OPENAI_KEY").ok(),
        ..MuthurSection::default()
    })
    .await;
}
