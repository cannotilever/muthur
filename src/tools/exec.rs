//! Executes a validated `Invocation` and maps the result to model-facing text.

use crate::ops::runner::{RunOutput, Runner};
use super::Invocation;
use crate::ops::cap;
use crate::config::Elevate;
use crate::metrics::Snapshot;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const TIER1_TIMEOUT: Duration = Duration::from_secs(5);
pub const TIER2_TIMEOUT: Duration = Duration::from_secs(30);

pub type SnapshotSlot = Arc<Mutex<Option<Snapshot>>>;

#[derive(Clone)]
pub struct ExecCtx {
    pub runner: Arc<dyn Runner>,
    pub snapshot: SnapshotSlot,
    pub elevate: Elevate,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutcome {
    pub content: String,
    pub is_error: bool,
    pub argv: Option<Vec<String>>,
    pub exit: Option<Value>,
    pub ms: u64,
}

fn strs(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

pub fn argv_for(inv: &Invocation, elevate: Elevate) -> Option<Vec<String>> {
    Some(match inv {
        Invocation::ShipStatus => return None,
        Invocation::ServiceStatus { unit } => {
            strs(&["systemctl", "status", "--no-pager", "--lines=0", unit])
        }
        Invocation::Journal { unit, lines } => strs(&[
            "journalctl",
            "-u",
            unit,
            "-n",
            &lines.to_string(),
            "--no-pager",
            "-o",
            "short-iso",
        ]),
        Invocation::ListUnits { state } => strs(&[
            "systemctl",
            "list-units",
            "--type=service",
            &format!("--state={}", state.word()),
            "--no-pager",
            "--plain",
            "--no-legend",
        ]),
        Invocation::ListeningPorts => strs(&["ss", "-tlnH"]),
        Invocation::Service { verb, unit } => {
            let who = match elevate {
                Elevate::Sudo => "sudo",
                Elevate::Doas => "doas",
            };
            strs(&[who, "-n", "/usr/bin/systemctl", verb.word(), unit])
        }
    })
}

fn outcome(
    content: String,
    is_error: bool,
    argv: Vec<String>,
    exit: Value,
    ms: u64,
) -> ToolOutcome {
    ToolOutcome {
        content: cap(content),
        is_error,
        argv: Some(argv),
        exit: Some(exit),
        ms,
    }
}

/// Elevation refusals (sudo, doas), matched lowercase against stderr.
const REFUSALS: [&str; 6] = [
    "password is required",
    "not allowed",
    "not permitted",
    "not in the sudoers",
    "authentication required",
    "authorization required",
];

pub fn execute(inv: &Invocation, ctx: &ExecCtx) -> ToolOutcome {
    let start = Instant::now();
    let Some(argv) = argv_for(inv, ctx.elevate) else {
        let slot = ctx.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        return match slot.as_ref() {
            Some(s) => ToolOutcome {
                content: cap(format_snapshot(s)),
                is_error: false,
                argv: None,
                exit: None,
                ms: 0,
            },
            None => ToolOutcome {
                content: "TELEMETRY NOT YET AVAILABLE".into(),
                is_error: true,
                argv: None,
                exit: None,
                ms: 0,
            },
        };
    };
    let tier2 = inv.tier() == 2;
    let timeout = if tier2 { TIER2_TIMEOUT } else { TIER1_TIMEOUT };
    let res = ctx.runner.run(&argv, timeout);
    let ms = start.elapsed().as_millis() as u64;
    match res {
        RunOutput::NotFound => outcome(
            "NOT AVAILABLE ON THIS SHIP".into(),
            true,
            argv,
            json!("not_found"),
            ms,
        ),
        RunOutput::TimedOut => outcome("NO RESPONSE".into(), true, argv, json!("timeout"), ms),
        RunOutput::Cancelled => outcome("ABORTED BY OPERATOR".into(), true, argv, json!("cancelled"), ms),
        RunOutput::Exited {
            code,
            stdout,
            stderr,
        } => {
            let exit = json!(code);
            if code == 0 {
                let content = match inv {
                    Invocation::Service { verb, unit } => {
                        format!("COMPLETED: {} {unit}", verb.word()).to_uppercase()
                    }
                    _ => {
                        let text = format!("{stdout}{stderr}");
                        if text.trim().is_empty() { "NO MATCHING ENTRIES".into() } else { text }
                    }
                };
                return outcome(content, false, argv, exit, ms);
            }
            if tier2 {
                let lower = stderr.to_lowercase();
                if REFUSALS.iter().any(|p| lower.contains(p)) {
                    let first = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
                    return outcome(
                        format!("INSUFFICIENT CLEARANCE: {first}"),
                        true,
                        argv,
                        exit,
                        ms,
                    );
                }
            }
            if matches!(inv, Invocation::ServiceStatus { .. }) && !stdout.trim().is_empty() {
                return outcome(format!("{stdout}{stderr}"), false, argv, exit, ms);
            }
            if stdout.trim().is_empty() {
                outcome(
                    format!("EXIT {code}: {}", stderr.trim()),
                    true,
                    argv,
                    exit,
                    ms,
                )
            } else {
                outcome(format!("{stdout}{stderr}"), true, argv, exit, ms)
            }
        }
    }
}

pub fn format_snapshot(s: &Snapshot) -> String {
    use crate::metrics::{human_bytes, human_rate, human_uptime};
    let mut l = vec![
        format!("HOST {}", s.host),
        format!("UPTIME {}", human_uptime(s.uptime_secs)),
        format!("LOAD {:.2} {:.2} {:.2}", s.load[0], s.load[1], s.load[2]),
        format!("CPU {:.0}%", s.cpu_total),
        format!(
            "MEMORY {}/{}",
            human_bytes(s.mem.used),
            human_bytes(s.mem.total)
        ),
        format!(
            "SWAP {}/{}",
            human_bytes(s.swap.used),
            human_bytes(s.swap.total)
        ),
    ];
    for d in &s.disks {
        l.push(format!("DISK {} {:.0}%", d.mount, d.usage.percent()));
    }
    l.push(format!(
        "NET RX {} TX {}",
        human_rate(s.net.rx_bps),
        human_rate(s.net.tx_bps)
    ));
    for p in s.top.iter().take(5) {
        l.push(format!(
            "PROC {} CPU {:.0}% MEM {}",
            p.name,
            p.cpu,
            human_bytes(p.mem)
        ));
    }
    l.push(match &s.failed_units {
        None => "FAILED UNITS N/A".into(),
        Some(u) if u.is_empty() => "FAILED UNITS NONE".into(),
        Some(u) => format!("FAILED UNITS {}", u.join(", ")),
    });
    l.push(match &s.containers {
        None => "CONTAINERS N/A".into(),
        Some(c) if c.is_empty() => "CONTAINERS NONE".into(),
        Some(c) => format!(
            "CONTAINERS {}",
            c.iter()
                .map(|c| format!("{} {}", c.name.to_uppercase(), c.status))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    });
    l.join("\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn ship_status_upper_cases_container_names() {
        use crate::metrics::Container;
        let s = Snapshot {
            containers: Some(vec![Container { id: "x".into(), name: "web".into(), status: "UP".into(), detail: "Up".into() }]),
            ..Default::default()
        };
        assert!(format_snapshot(&s).contains("CONTAINERS WEB UP"), "{}", format_snapshot(&s));
    }

    use super::*;
    use crate::tools::{UnitState, Verb};

    struct FakeRunner {
        calls: Mutex<Vec<(Vec<String>, Duration)>>,
        reply: RunOutput,
    }
    impl Runner for FakeRunner {
        fn run(&self, argv: &[String], timeout: Duration) -> RunOutput {
            self.calls.lock().unwrap().push((argv.to_vec(), timeout));
            self.reply.clone()
        }
    }

    fn ctx(reply: RunOutput) -> (Arc<FakeRunner>, ExecCtx) {
        let fr = Arc::new(FakeRunner {
            calls: Mutex::new(vec![]),
            reply,
        });
        let c = ExecCtx {
            runner: fr.clone(),
            snapshot: Arc::new(Mutex::new(None)),
            elevate: Elevate::Sudo,
        };
        (fr, c)
    }
    fn exited(code: i32, o: &str, e: &str) -> RunOutput {
        RunOutput::Exited {
            code,
            stdout: o.into(),
            stderr: e.into(),
        }
    }
    fn status() -> Invocation {
        Invocation::ServiceStatus {
            unit: "nginx.service".into(),
        }
    }
    fn restart() -> Invocation {
        Invocation::Service {
            verb: Verb::Restart,
            unit: "nginx.service".into(),
        }
    }

    #[test]
    fn argv_exact() {
        assert_eq!(
            argv_for(&status(), Elevate::Sudo).unwrap(),
            [
                "systemctl",
                "status",
                "--no-pager",
                "--lines=0",
                "nginx.service"
            ]
        );
        assert_eq!(
            argv_for(
                &Invocation::Journal {
                    unit: "a.service".into(),
                    lines: 20
                },
                Elevate::Sudo
            )
            .unwrap(),
            [
                "journalctl",
                "-u",
                "a.service",
                "-n",
                "20",
                "--no-pager",
                "-o",
                "short-iso"
            ]
        );
        assert_eq!(
            argv_for(
                &Invocation::ListUnits {
                    state: UnitState::Failed
                },
                Elevate::Sudo
            )
            .unwrap(),
            [
                "systemctl",
                "list-units",
                "--type=service",
                "--state=failed",
                "--no-pager",
                "--plain",
                "--no-legend"
            ]
        );
        assert_eq!(
            argv_for(&Invocation::ListeningPorts, Elevate::Sudo).unwrap(),
            ["ss", "-tlnH"]
        );
        assert_eq!(
            argv_for(&restart(), Elevate::Doas).unwrap(),
            [
                "doas",
                "-n",
                "/usr/bin/systemctl",
                "restart",
                "nginx.service"
            ]
        );
        assert_eq!(argv_for(&restart(), Elevate::Sudo).unwrap()[0], "sudo");
        assert_eq!(argv_for(&Invocation::ShipStatus, Elevate::Sudo), None);
    }

    #[test]
    fn success_and_completed() {
        let (fr, c) = ctx(exited(0, "active", ""));
        let o = execute(&status(), &c);
        assert_eq!(o.content, "active");
        assert!(!o.is_error);
        assert_eq!(o.exit, Some(json!(0)));
        assert_eq!(fr.calls.lock().unwrap().len(), 1);
        assert_eq!(o.argv.as_ref(), Some(&fr.calls.lock().unwrap()[0].0));
        let (_fr, c) = ctx(exited(0, "", ""));
        let o = execute(&restart(), &c);
        assert_eq!(o.content, "COMPLETED: RESTART NGINX.SERVICE");
        assert!(!o.is_error);
    }

    #[test]
    fn empty_success_is_no_matching_entries() {
        for out in ["", "  \n", "\n"] {
            let (_fr, c) = ctx(exited(0, out, ""));
            let o = execute(&Invocation::Journal { unit: "nginx.service".into(), lines: 5 }, &c);
            assert_eq!(o.content, "NO MATCHING ENTRIES");
            assert!(!o.is_error);
            assert_eq!(o.exit, Some(json!(0)));
        }
        let (_fr, c) = ctx(exited(0, "", "\n"));
        assert_eq!(execute(&Invocation::ListUnits { state: UnitState::Failed }, &c).content, "NO MATCHING ENTRIES");
    }

    #[test]
    fn doas_refusals_are_insufficient_clearance() {
        for e in ["doas: Authentication required", "doas: authorization required", "doas: AUTHORIZATION REQUIRED"] {
            let (_fr, c) = ctx(exited(1, "", e));
            let o = execute(&restart(), &c);
            assert!(o.is_error);
            assert_eq!(o.content, format!("INSUFFICIENT CLEARANCE: {e}"));
        }
    }

    #[test]
    fn inactive_unit_status_is_not_an_error() {
        let (_fr, c) = ctx(exited(3, "inactive (dead)", ""));
        let o = execute(&status(), &c);
        assert!(!o.is_error);
        assert!(o.content.contains("inactive"));
    }

    #[test]
    fn sudo_refusal_is_insufficient_clearance() {
        let (_fr, c) = ctx(exited(1, "", "sudo: a password is required\nmore"));
        let o = execute(&restart(), &c);
        assert!(o.is_error);
        assert_eq!(
            o.content,
            "INSUFFICIENT CLEARANCE: sudo: a password is required"
        );
    }

    #[test]
    fn refusal_uses_first_non_empty_line() {
        let (_fr, c) = ctx(exited(1, "", "\n  \nsudo: not allowed\nmore"));
        assert_eq!(execute(&restart(), &c).content, "INSUFFICIENT CLEARANCE: sudo: not allowed");
    }

    #[test]
    fn not_found_and_exit_mapping() {
        let (_fr, c) = ctx(RunOutput::NotFound);
        let o = execute(&status(), &c);
        assert_eq!(o.content, "NOT AVAILABLE ON THIS SHIP");
        assert!(o.is_error);
        assert_eq!(o.exit, Some(json!("not_found")));
        let (_fr, c) = ctx(exited(4, "", "boom\n"));
        let o = execute(&Invocation::ListeningPorts, &c);
        assert_eq!(o.content, "EXIT 4: boom");
        assert!(o.is_error);
        let (_fr, c) = ctx(exited(4, "partial", "boom"));
        let o = execute(&Invocation::ListeningPorts, &c);
        assert!(o.is_error);
        assert!(o.content.contains("partial") && o.content.contains("boom"));
    }

    #[test]
    fn timeout_maps_to_no_response() {
        let (fr, c) = ctx(RunOutput::TimedOut);
        let o = execute(&status(), &c);
        assert_eq!(o.content, "NO RESPONSE");
        assert!(o.is_error);
        assert_eq!(o.exit, Some(json!("timeout")));
        execute(&restart(), &c);
        let calls = fr.calls.lock().unwrap();
        assert_eq!(calls[0].1, Duration::from_secs(5));
        assert_eq!(calls[1].1, Duration::from_secs(30));
    }

    #[test]
    fn output_is_capped() {
        let (_fr, c) = ctx(exited(0, &"x".repeat(10000), ""));
        let o = execute(&status(), &c);
        assert!(o.content.ends_with("[TRUNCATED]"));
    }

    #[test]
    fn ship_status_reads_slot() {
        let (fr, c) = ctx(exited(0, "", ""));
        let o = execute(&Invocation::ShipStatus, &c);
        assert_eq!(o.content, "TELEMETRY NOT YET AVAILABLE");
        assert!(o.is_error);
        assert_eq!(o.argv, None);
        assert_eq!(o.exit, None);
        *c.snapshot.lock().unwrap() = Some(Snapshot {
            host: "NOSTROMO".into(),
            failed_units: None,
            ..Default::default()
        });
        let o = execute(&Invocation::ShipStatus, &c);
        assert!(!o.is_error);
        assert!(o.content.contains("HOST NOSTROMO"));
        assert!(o.content.contains("FAILED UNITS N/A"));
        assert!(fr.calls.lock().unwrap().is_empty());
    }
}
