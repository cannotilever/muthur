//! Fixed tools: ship status, containers and network checks, run as the operator.

use super::exec::{SnapshotSlot, ToolOutcome, format_snapshot};
use super::registry::{DnsType, Invocation};
use crate::ops::cap;
use crate::ops::runner::{RunOutput, Runner};
use serde_json::{Value, json};
use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct FixedCtx {
    pub runner: Arc<dyn Runner>,
    pub snapshot: SnapshotSlot,
}

fn strs(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// Exact argv per the spec's table. None for tools that run no command, and for
/// container tools without a runtime.
pub fn argv_for(inv: &Invocation, runtime: Option<&str>) -> Option<Vec<String>> {
    Some(match inv {
        Invocation::ShipStatus | Invocation::PortCheck { .. } | Invocation::Run { .. } => return None,
        Invocation::Containers => {
            strs(&[runtime?, "ps", "-a", "--no-trunc", "--format", "{{.Names}}\\t{{.Image}}\\t{{.Status}}"])
        }
        Invocation::ContainerInspect { name } => strs(&[runtime?, "inspect", "--type", "container", "--", name]),
        Invocation::ContainerLogs { name, lines } => strs(&[runtime?, "logs", "--tail", &lines.to_string(), "--", name]),
        Invocation::Ping { host, count } => strs(&["ping", "-c", &count.to_string(), "-W", "2", "--", host]),
        Invocation::Traceroute { host } => strs(&["traceroute", "-q", "1", "-w", "2", "-m", "30", "--", host]),
        Invocation::DnsLookup { name, rtype } => {
            if matches!(rtype, DnsType::Ptr) {
                strs(&["dig", "+short", "+time=2", "+tries=1", "-x", name])
            } else {
                strs(&["dig", "+short", "+time=2", "+tries=1", "--", name, rtype.word()])
            }
        }
        Invocation::HttpCheck { url } => strs(&[
            "curl", "-q", "-sS", "-o", "/dev/null", "--max-time", "10", "--max-redirs", "0", "--proto", "=http,https", "-w",
            "%{http_code} %{time_total}", "--", url,
        ]),
    })
}

fn timeout_for(inv: &Invocation) -> Duration {
    Duration::from_secs(match inv {
        Invocation::Ping { .. } => 30,
        Invocation::Traceroute { .. } => 90,
        Invocation::HttpCheck { .. } => 15,
        _ => 10,
    })
}

/// Replace every `Config.Env` value (after the first `=`) with `<REDACTED>`.
pub fn redact_env(inspect_json: &str) -> String {
    let Ok(mut v) = serde_json::from_str::<Value>(inspect_json) else { return inspect_json.to_string() };
    if let Some(items) = v.as_array_mut() {
        for item in items {
            let Some(env) = item.pointer_mut("/Config/Env").and_then(Value::as_array_mut) else { continue };
            for e in env {
                if let Some(s) = e.as_str() {
                    let key = s.split_once('=').map_or(s, |(k, _)| k);
                    *e = Value::String(format!("{key}=<REDACTED>"));
                }
            }
        }
    }
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| inspect_json.to_string())
}

fn outcome(content: impl Into<String>, is_error: bool, argv: Option<Vec<String>>, exit: Option<Value>, ms: u64) -> ToolOutcome {
    ToolOutcome { content: cap(content.into()), is_error, argv, exit, ms, audit_output: None }
}

fn port_check(host: &str, port: u16) -> ToolOutcome {
    let start = Instant::now();
    let Some(addr) = (host, port).to_socket_addrs().ok().and_then(|mut a| a.next()) else {
        return outcome("NO SUCH HOST", true, None, None, start.elapsed().as_millis() as u64);
    };
    let res = TcpStream::connect_timeout(&addr, Duration::from_secs(5));
    let ms = start.elapsed().as_millis() as u64;
    match res {
        Ok(_) => outcome(format!("OPEN {ms} MS"), false, None, None, ms),
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => outcome("CLOSED", false, None, None, ms),
        Err(e) if e.kind() == ErrorKind::TimedOut => outcome("FILTERED", false, None, None, ms),
        Err(_) => outcome("UNREACHABLE", false, None, None, ms),
    }
}

pub fn execute(inv: &Invocation, ctx: &FixedCtx) -> ToolOutcome {
    let snap = ctx.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match inv {
        Invocation::ShipStatus => {
            return match &snap {
                Some(s) => outcome(format_snapshot(s), false, None, None, 0),
                None => outcome("TELEMETRY NOT YET AVAILABLE", true, None, None, 0),
            };
        }
        Invocation::PortCheck { host, port } => return port_check(host, *port),
        Invocation::Run { .. } => return outcome("NOT AVAILABLE ON THIS SHIP", true, None, None, 0),
        _ => {}
    }
    let runtime = snap.as_ref().and_then(|s| s.container_runtime.as_deref());
    let container = match inv {
        Invocation::Containers => Some(None),
        Invocation::ContainerInspect { name } | Invocation::ContainerLogs { name, .. } => Some(Some(name)),
        _ => None,
    };
    if let Some(name) = container {
        if runtime.is_none() {
            return outcome("NOT AVAILABLE ON THIS SHIP", true, None, None, 0);
        }
        if let Some(name) = name {
            let known = snap.as_ref().and_then(|s| s.containers.as_ref()).is_some_and(|c| c.iter().any(|c| &c.name == name));
            if !known {
                return outcome("NOT AUTHORIZED: UNKNOWN CONTAINER", true, None, None, 0);
            }
        }
    }
    let Some(argv) = argv_for(inv, runtime) else { return outcome("NOT AVAILABLE ON THIS SHIP", true, None, None, 0) };
    let start = Instant::now();
    let res = ctx.runner.run(&argv, timeout_for(inv));
    let ms = start.elapsed().as_millis() as u64;
    match res {
        RunOutput::NotFound => outcome("NOT AVAILABLE ON THIS SHIP", true, Some(argv), Some(json!("not_found")), ms),
        RunOutput::TimedOut => outcome("NO RESPONSE", true, Some(argv), Some(json!("timeout")), ms),
        RunOutput::Cancelled => outcome("ABORTED BY OPERATOR", true, Some(argv), Some(json!("cancelled")), ms),
        RunOutput::Exited { code, stdout, stderr } => {
            let exit = Some(json!(code));
            if code == 0 {
                let content = match inv {
                    Invocation::HttpCheck { .. } => {
                        let mut it = stdout.split_whitespace();
                        format!("HTTP {} {} S", it.next().unwrap_or("000"), it.next().unwrap_or("0"))
                    }
                    Invocation::ContainerInspect { .. } => redact_env(&stdout),
                    Invocation::DnsLookup { .. } if stdout.trim().is_empty() => "NO RECORDS".into(),
                    _ if stdout.trim().is_empty() && stderr.trim().is_empty() => "NO OUTPUT".into(),
                    _ => format!("{stdout}{stderr}"),
                };
                return outcome(content, false, Some(argv), exit, ms);
            }
            let text = if stdout.trim().is_empty() {
                format!("EXIT {code}: {}", stderr.trim())
            } else {
                format!("{stdout}{stderr}")
            };
            outcome(text, true, Some(argv), exit, ms)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{Container, Snapshot};
    use crate::ops::runner::RunSpec;
    use crate::tools::registry::DnsType;
    use std::net::TcpListener;
    use std::sync::Mutex;

    struct Fake {
        calls: Mutex<Vec<Vec<String>>>,
        reply: RunOutput,
    }
    impl Runner for Fake {
        fn run(&self, argv: &[String], _t: Duration) -> RunOutput {
            self.calls.lock().unwrap().push(argv.to_vec());
            self.reply.clone()
        }
        fn run_spec(&self, s: &RunSpec) -> RunOutput {
            self.run(s.argv, s.timeout)
        }
    }
    fn ctx(reply: RunOutput, runtime: Option<&str>, names: &[&str]) -> (FixedCtx, Arc<Fake>) {
        let f = Arc::new(Fake { calls: Mutex::new(vec![]), reply });
        let snap = Snapshot {
            container_runtime: runtime.map(String::from),
            containers: Some(
                names.iter().map(|n| Container { id: "i".into(), name: n.to_string(), status: "UP".into(), detail: "Up".into() }).collect(),
            ),
            ..Default::default()
        };
        (FixedCtx { runner: f.clone(), snapshot: Arc::new(Mutex::new(Some(snap))) }, f)
    }
    fn ok(s: &str) -> RunOutput {
        RunOutput::Exited { code: 0, stdout: s.into(), stderr: String::new() }
    }

    #[test]
    fn argv_per_tool() {
        let d = Some("docker");
        let a = |i: Invocation| argv_for(&i, d).unwrap();
        assert_eq!(a(Invocation::Containers), strs(&["docker", "ps", "-a", "--no-trunc", "--format", "{{.Names}}\\t{{.Image}}\\t{{.Status}}"]));
        assert_eq!(a(Invocation::ContainerInspect { name: "web".into() }), strs(&["docker", "inspect", "--type", "container", "--", "web"]));
        assert_eq!(a(Invocation::ContainerLogs { name: "web".into(), lines: 50 }), strs(&["docker", "logs", "--tail", "50", "--", "web"]));
        assert_eq!(a(Invocation::Ping { host: "h".into(), count: 3 }), strs(&["ping", "-c", "3", "-W", "2", "--", "h"]));
        assert_eq!(a(Invocation::Traceroute { host: "h".into() }), strs(&["traceroute", "-q", "1", "-w", "2", "-m", "30", "--", "h"]));
        assert_eq!(
            a(Invocation::DnsLookup { name: "x.org".into(), rtype: DnsType::Mx }),
            strs(&["dig", "+short", "+time=2", "+tries=1", "--", "x.org", "MX"])
        );
        assert_eq!(
            a(Invocation::DnsLookup { name: "1.2.3.4".into(), rtype: DnsType::Ptr }),
            strs(&["dig", "+short", "+time=2", "+tries=1", "-x", "1.2.3.4"])
        );
        assert_eq!(
            a(Invocation::HttpCheck { url: "http://h/".into() }),
            strs(&["curl", "-q", "-sS", "-o", "/dev/null", "--max-time", "10", "--max-redirs", "0", "--proto", "=http,https", "-w", "%{http_code} %{time_total}", "--", "http://h/"])
        );
        assert_eq!(argv_for(&Invocation::ShipStatus, d), None);
        assert_eq!(argv_for(&Invocation::Containers, None), None);
    }

    #[test]
    fn unknown_container_rejected() {
        let (c, f) = ctx(ok("{}"), Some("docker"), &["web"]);
        let o = execute(&Invocation::ContainerInspect { name: "db".into() }, &c);
        assert_eq!(o.content, "NOT AUTHORIZED: UNKNOWN CONTAINER");
        assert!(o.is_error);
        assert!(f.calls.lock().unwrap().is_empty());
        let o = execute(&Invocation::ContainerLogs { name: "db".into(), lines: 5 }, &c);
        assert_eq!(o.content, "NOT AUTHORIZED: UNKNOWN CONTAINER");
    }

    #[test]
    fn no_runtime() {
        let (c, f) = ctx(ok(""), None, &["web"]);
        let o = execute(&Invocation::Containers, &c);
        assert_eq!(o.content, "NOT AVAILABLE ON THIS SHIP");
        assert!(o.is_error);
        assert!(f.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn redact_env() {
        let out = super::redact_env(r#"[{"Config":{"Env":["A=1","B=x=y"]}}]"#);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v[0]["Config"]["Env"], json!(["A=<REDACTED>", "B=<REDACTED>"]));
        assert_eq!(super::redact_env("not json"), "not json");
    }

    #[test]
    fn inspect_output_is_redacted() {
        let (c, _) = ctx(ok(r#"[{"Config":{"Env":["K=secret"]}}]"#), Some("docker"), &["web"]);
        let o = execute(&Invocation::ContainerInspect { name: "web".into() }, &c);
        assert!(!o.content.contains("secret"), "{}", o.content);
        assert!(o.content.contains("K=<REDACTED>"));
    }

    #[test]
    fn port_check_states() {
        let (c, _) = ctx(ok(""), None, &[]);
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let o = execute(&Invocation::PortCheck { host: "127.0.0.1".into(), port }, &c);
        assert!(o.content.starts_with("OPEN "), "{}", o.content);
        assert!(o.content.ends_with(" MS"));
        drop(l);
        let o = execute(&Invocation::PortCheck { host: "127.0.0.1".into(), port }, &c);
        assert_eq!(o.content, "CLOSED");
    }

    #[test]
    fn http_check_formats() {
        let (c, _) = ctx(ok("200 0.123"), None, &[]);
        let o = execute(&Invocation::HttpCheck { url: "http://h/".into() }, &c);
        assert_eq!(o.content, "HTTP 200 0.123 S");
        assert!(!o.is_error);
    }

    #[test]
    fn missing_binary_and_timeout() {
        let (c, _) = ctx(RunOutput::NotFound, None, &[]);
        assert_eq!(execute(&Invocation::Traceroute { host: "h".into() }, &c).content, "NOT AVAILABLE ON THIS SHIP");
        let (c, _) = ctx(RunOutput::TimedOut, None, &[]);
        assert_eq!(execute(&Invocation::Ping { host: "h".into(), count: 1 }, &c).content, "NO RESPONSE");
    }
}
