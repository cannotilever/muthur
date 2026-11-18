//! Live round trips against real endpoints. User-run only:
//!   MUTHUR_LIVE_ANTHROPIC_KEY=... cargo test --test llm_live anthropic -- --ignored --nocapture
//!   MUTHUR_LIVE_OPENAI_URL=... MUTHUR_LIVE_OPENAI_MODEL=... cargo test --test llm_live openai -- --ignored --nocapture
//! Token usage (cache hits etc.) is logged by the client to muthur.log; cache
//! hits are not asserted because short chats sit under the 512-token minimum.

use muthur::chatlog::{Assistant, ChatLog};
use muthur::config::{MuthurSection, Provider};
use muthur::llm::client::LlmClient;
use muthur::llm::{Stop, StreamEvent};

const SYSTEM: &str = "You are a terse test responder. Answer in at most ten words.";

/// One turn: stream the window, assert text and a clean end, return (text, raw, model).
async fn turn(client: &LlmClient, cfg: &MuthurSection, log: &mut ChatLog, user: &str) -> Assistant {
    log.append_user(user, "2026-11-18T00:00:00Z").unwrap();
    let messages = log.window(cfg.provider, SYSTEM, cfg.context_tokens).unwrap();
    let mut text = String::new();
    let mut done = None;
    client
        .stream(muthur::llm::ChatRequest { system: SYSTEM.into(), messages }, |ev| match ev {
            StreamEvent::Text(t) => text.push_str(&t),
            StreamEvent::Done { stop, model, raw } => done = Some((stop, model, raw)),
            StreamEvent::Error(e) => panic!("stream error: {e:?}"),
            StreamEvent::ToolCall { .. } => {}
        })
        .await;
    let (stop, model, raw) = done.expect("no Done event");
    assert_eq!(stop, Stop::End);
    assert!(!text.trim().is_empty(), "empty reply");
    println!("--- reply: {text}");
    Assistant { provider: cfg.provider, model: model.unwrap_or_default(), text, stop: "end".into(), raw }
}

async fn round_trip(cfg: MuthurSection) {
    let dir = tempfile::tempdir().unwrap();
    let mut log = ChatLog::open(dir.path().join("chat.jsonl")).unwrap();
    let client = LlmClient::new(cfg.clone()).unwrap();
    let a = turn(&client, &cfg, &mut log, "STATE YOUR DESIGNATION.").await;
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
    round_trip(MuthurSection { provider: Provider::Anthropic, api_key: Some(key), ..MuthurSection::default() }).await;
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
    })
    .await;
}
