//! Streaming HTTP client: one ChatRequest in, StreamEvents out.
use super::{anthropic, openai, sse::SseDecoder, ChatRequest, LlmError, StreamEvent, StreamParser};
use crate::config::{MuthurSection, Provider};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub fn map_status(code: u16) -> LlmError {
    match code {
        401 | 403 => LlmError::Auth,
        429 | 529 | 500..=599 => LlmError::Overloaded,
        _ => LlmError::Malformed,
    }
}

pub struct LlmClient {
    http: reqwest::Client,
    cfg: Arc<MuthurSection>,
    idle: Duration,
}

impl LlmClient {
    pub fn new(cfg: MuthurSection) -> anyhow::Result<Self> {
        Self::with_idle(cfg, IDLE_TIMEOUT)
    }

    pub fn with_idle(cfg: MuthurSection, idle: Duration) -> anyhow::Result<Self> {
        super::install_crypto_provider();
        let http = reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT).redirect(reqwest::redirect::Policy::none()).build()?;
        Ok(Self { http, cfg: Arc::new(cfg), idle })
    }

    /// Stream one request; every event goes through `emit`. Stops after the
    /// first `Done` or `Error`.
    pub async fn stream(&self, req: ChatRequest, mut emit: impl FnMut(StreamEvent)) {
        let cfg = &*self.cfg;
        if let Err(e) = cfg.readiness() {
            emit(StreamEvent::Error(e));
            return;
        }
        let (path, headers, body, mut parser): (&str, _, _, Box<dyn StreamParser>) = match cfg.provider {
            Provider::Anthropic => (
                anthropic::PATH,
                anthropic::headers(cfg),
                anthropic::request_body(cfg, &req),
                Box::new(anthropic::Parser::new()),
            ),
            Provider::Openai => (
                openai::PATH,
                openai::headers(cfg),
                openai::request_body(cfg, &req),
                Box::new(openai::Parser::new()),
            ),
        };
        let Some(base) = cfg.base_url() else {
            emit(StreamEvent::Error(LlmError::NotConfigured));
            return;
        };
        let Ok(bytes) = serde_json::to_vec(&body) else {
            emit(StreamEvent::Error(LlmError::Malformed));
            return;
        };
        let mut rb = self.http.post(format!("{base}{path}")).body(bytes);
        for (k, v) in headers {
            rb = rb.header(k, v);
        }
        let mut resp = match tokio::time::timeout(self.idle, rb.send()).await {
            Ok(Ok(r)) => r,
            _ => {
                emit(StreamEvent::Error(LlmError::NoCarrier));
                return;
            }
        };
        let status = resp.status();
        if !status.is_success() {
            let body = tokio::time::timeout(self.idle, resp.bytes()).await.ok().and_then(Result::ok);
            let v: Option<serde_json::Value> = body.and_then(|b| serde_json::from_slice(&b).ok());
            let field = |k: &str| {
                v.as_ref()
                    .and_then(|v| v.get("error"))
                    .and_then(|e| e.get(k))
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            tracing::warn!(status = status.as_u16(), error_type = %field("type"), message = %field("message"), "llm http error");
            emit(StreamEvent::Error(map_status(status.as_u16())));
            return;
        }
        let mut dec = SseDecoder::default();
        loop {
            let events = match tokio::time::timeout(self.idle, resp.chunk()).await {
                Err(_) => vec![StreamEvent::Error(LlmError::SignalLost)],
                Ok(Err(_)) => vec![StreamEvent::Error(LlmError::Transmission)],
                Ok(Ok(None)) => {
                    for ev in parser.finish() {
                        emit(ev);
                    }
                    return;
                }
                Ok(Ok(Some(chunk))) => dec.push(&chunk).iter().flat_map(|r| parser.feed(r)).collect(),
            };
            for ev in events {
                let last = matches!(ev, StreamEvent::Done { .. } | StreamEvent::Error(_));
                let done = matches!(ev, StreamEvent::Done { .. });
                emit(ev);
                if last {
                    if done && let Some(u) = parser.usage_summary() {
                        tracing::info!(model = cfg.model().unwrap_or(""), usage = %u, "llm turn");
                    }
                    return;
                }
            }
        }
    }

    pub fn spawn(
        self: &Arc<Self>,
        req_id: u64,
        req: ChatRequest,
        tx: UnboundedSender<crate::run::Event>,
    ) -> tokio::task::JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.stream(req, |ev| {
                let _ = tx.send(crate::run::Event::Llm { req_id, ev });
            })
            .await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Message, MessageContent, Role};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/llm/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    struct Srv {
        status: &'static str,
        body: Vec<u8>,
        chunk: usize,
        delay: Duration,
        /// Sleep after the first chunk, then stop sending (connection stays open meanwhile).
        stall_after_first: Option<Duration>,
        extra: String,
    }

    fn srv(body: Vec<u8>) -> Srv {
        Srv { status: "200 OK", body, chunk: 64, delay: Duration::ZERO, stall_after_first: None, extra: String::new() }
    }

    /// Serves one connection; returns the base url and the captured request (head + body).
    async fn serve(s: Srv) -> (String, tokio::sync::oneshot::Receiver<Vec<u8>>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let (ctx, crx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut sock, _) = l.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let head_end = loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break p + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
            let want: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while buf.len() < head_end + want {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let _ = ctx.send(buf);
            let hdr = format!("HTTP/1.1 {}\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n{}\r\n", s.status, s.extra);
            let _ = sock.write_all(hdr.as_bytes()).await;
            for (i, c) in s.body.chunks(s.chunk).enumerate() {
                let mut out = format!("{:x}\r\n", c.len()).into_bytes();
                out.extend_from_slice(c);
                out.extend_from_slice(b"\r\n");
                if sock.write_all(&out).await.is_err() {
                    return;
                }
                if i == 0 && let Some(d) = s.stall_after_first {
                    tokio::time::sleep(d).await;
                    return;
                }
                tokio::time::sleep(s.delay).await;
            }
            let _ = sock.write_all(b"0\r\n\r\n").await;
            let _ = sock.flush().await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        (url, crx)
    }

    fn cfg(provider: Provider, base: &str) -> MuthurSection {
        MuthurSection {
            provider,
            base_url: Some(base.to_string()),
            model: Some("m".into()),
            api_key: Some("k".into()),
            ..Default::default()
        }
    }

    fn req() -> ChatRequest {
        ChatRequest {
            system: "sys".into(),
            messages: vec![Message { role: Role::User, content: MessageContent::Text("hi".into()) }],
        }
    }

    async fn collect(c: &LlmClient) -> Vec<StreamEvent> {
        let mut v = Vec::new();
        c.stream(req(), |e| v.push(e)).await;
        v
    }

    fn text(evs: &[StreamEvent]) -> String {
        evs.iter().filter_map(|e| if let StreamEvent::Text(t) = e { Some(t.as_str()) } else { None }).collect()
    }

    #[test]
    fn status_mapping() {
        assert_eq!(map_status(401), LlmError::Auth);
        assert_eq!(map_status(403), LlmError::Auth);
        assert_eq!(map_status(429), LlmError::Overloaded);
        assert_eq!(map_status(529), LlmError::Overloaded);
        assert_eq!(map_status(502), LlmError::Overloaded);
        assert_eq!(map_status(400), LlmError::Malformed);
    }

    #[tokio::test]
    async fn streams_openai_fixture() {
        let (url, _) = serve(srv(fixture("openai_text.sse"))).await;
        let c = LlmClient::new(cfg(Provider::Openai, &url)).unwrap();
        let evs = collect(&c).await;
        assert_eq!(text(&evs), "ALL SYSTEMS NOMINAL.");
        assert!(matches!(evs.last(), Some(StreamEvent::Done { .. })), "{evs:?}");
    }

    #[tokio::test]
    async fn streams_anthropic_fixture_and_sends_headers() {
        let (url, cap) = serve(srv(fixture("anthropic_text.sse"))).await;
        let c = LlmClient::new(cfg(Provider::Anthropic, &url)).unwrap();
        let evs = collect(&c).await;
        assert!(!text(&evs).is_empty(), "{evs:?}");
        assert!(matches!(evs.last(), Some(StreamEvent::Done { .. })), "{evs:?}");
        let raw = cap.await.unwrap();
        let s = String::from_utf8(raw).unwrap();
        let (head, body) = s.split_once("\r\n\r\n").unwrap();
        let head = head.to_lowercase();
        assert!(head.starts_with("post /v1/messages "), "{head}");
        assert!(head.contains("x-api-key: k"), "{head}");
        assert!(head.contains("anthropic-version: 2023-06-01"), "{head}");
        let j: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(j["stream"], true);
    }

    #[tokio::test]
    async fn http_401_is_auth() {
        let body = br#"{"type":"error","error":{"type":"authentication_error","message":"x"}}"#.to_vec();
        let (url, _) = serve(Srv { status: "401 Unauthorized", ..srv(body) }).await;
        let c = LlmClient::new(cfg(Provider::Anthropic, &url)).unwrap();
        assert_eq!(collect(&c).await, vec![StreamEvent::Error(LlmError::Auth)]);
    }

    #[tokio::test]
    async fn redirect_is_not_followed() {
        let other = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let loc = format!("Location: http://{}/\r\n", other.local_addr().unwrap());
        let (url, _) = serve(Srv { status: "302 Found", extra: loc, ..srv(Vec::new()) }).await;
        let c = LlmClient::new(cfg(Provider::Anthropic, &url)).unwrap();
        assert_eq!(collect(&c).await, vec![StreamEvent::Error(LlmError::Malformed)]);
        assert!(tokio::time::timeout(Duration::from_millis(200), other.accept()).await.is_err());
    }

    #[tokio::test]
    async fn refused_connection_is_no_carrier() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        let c = LlmClient::new(cfg(Provider::Openai, &url)).unwrap();
        assert_eq!(collect(&c).await, vec![StreamEvent::Error(LlmError::NoCarrier)]);
    }

    #[tokio::test]
    async fn idle_timeout_is_signal_lost() {
        let s = Srv { stall_after_first: Some(Duration::from_secs(2)), ..srv(fixture("openai_text.sse")) };
        let (url, _) = serve(s).await;
        let c = LlmClient::with_idle(cfg(Provider::Openai, &url), Duration::from_millis(200)).unwrap();
        let t = std::time::Instant::now();
        let evs = collect(&c).await;
        assert_eq!(evs.last(), Some(&StreamEvent::Error(LlmError::SignalLost)), "{evs:?}");
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn not_ready_emits_without_network() {
        let c = LlmClient::new(MuthurSection::default()).unwrap();
        assert_eq!(collect(&c).await, vec![StreamEvent::Error(LlmError::NoAccessCode)]);
    }

    #[tokio::test]
    async fn spawn_forwards_and_abort_stops() {
        let s = Srv { chunk: 40, delay: Duration::from_millis(100), ..srv(fixture("openai_text.sse")) };
        let (url, _) = serve(s).await;
        let c = Arc::new(LlmClient::new(cfg(Provider::Openai, &url)).unwrap());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let h = c.spawn(7, req(), tx);
        let first = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
        assert!(matches!(first, crate::run::Event::Llm { req_id: 7, .. }));
        h.abort();
        let t = std::time::Instant::now();
        assert!(h.await.unwrap_err().is_cancelled());
        assert!(t.elapsed() < Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        assert!(tokio::time::timeout(Duration::from_millis(300), rx.recv()).await.map_or(true, |e| e.is_none()));
    }
}
