//! The `run` tool: argv, result mapping, timeout cleanup and the startup probe.

use super::exec::ToolOutcome;
use crate::agent::ABORTED_ISSUED;
use crate::ops::head_tail;
use crate::ops::runner::{Kill, RunOutput, RunSpec, Runner};
use crate::secret::Secret;
use serde_json::json;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub const AGENT_HOME: &str = "/var/lib/muthur-agent";
const ENV: [&str; 3] = ["PATH=/usr/local/sbin:/usr/local/bin:/usr/bin", "LANG=C.UTF-8", "TERM=dumb"];
const STEP: Duration = Duration::from_secs(5);

fn strs(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn agent_prefix() -> Vec<String> {
    strs(&["sudo", "-n", "-u", "muthur-agent", "--"])
}

pub fn argv(command: &str, as_root: bool) -> Vec<String> {
    let (mut v, home, arg) = if as_root {
        (strs(&["sudo", "-k", "-S", "-p", "", "--"]), "/root", format!("exec </dev/null\n{command}"))
    } else {
        (agent_prefix(), AGENT_HOME, command.to_string())
    };
    v.extend(strs(&["/usr/bin/env", "-i", "-C", home]));
    v.push(format!("HOME={home}"));
    v.extend(ENV.map(String::from));
    v.extend(strs(&["/bin/bash", "--noprofile", "--norc", "-c"]));
    v.push(arg);
    v
}

pub fn cleanup_argv() -> Vec<String> {
    let mut v = agent_prefix();
    v.extend(strs(&["/usr/bin/kill", "-KILL", "-1"]));
    v
}

fn out(content: &str, is_error: bool, argv: Vec<String>, exit: serde_json::Value, ms: u64, audit: Option<String>) -> ToolOutcome {
    ToolOutcome { content: content.into(), is_error, argv: Some(argv), exit: Some(exit), ms, audit_output: audit }
}

pub fn run(
    runner: &dyn Runner,
    command: &str,
    as_root: bool,
    timeout: Duration,
    password: Option<&Secret>,
    cancel: &AtomicBool,
) -> ToolOutcome {
    let argv = argv(command, as_root);
    let stdin: Option<Zeroizing<String>> = password.map(|p| Zeroizing::new(format!("{}\n", p.expose())));
    let spec = RunSpec {
        argv: &argv,
        timeout,
        merged: true,
        stdin: stdin.as_ref().map(|s| s.as_bytes()),
        kill: Kill::Graceful(STEP),
        cancel: Some(cancel),
        keep: Some(65536),
    };
    let start = Instant::now();
    let res = runner.run_spec(&spec);
    let ms = start.elapsed().as_millis() as u64;
    let cleanup = || {
        let c = cleanup_argv();
        runner.run_spec(&RunSpec::new(&c, STEP));
    };
    match res {
        RunOutput::NotFound => out("NOT AVAILABLE ON THIS SHIP", true, argv, json!("not_found"), ms, None),
        RunOutput::TimedOut => {
            let text = if as_root {
                "NO RESPONSE — OUTCOME UNKNOWN"
            } else {
                cleanup();
                "NO RESPONSE"
            };
            out(text, true, argv, json!("timeout"), ms, None)
        }
        RunOutput::Cancelled => {
            let text = if as_root {
                ABORTED_ISSUED
            } else {
                cleanup();
                "ABORTED BY OPERATOR"
            };
            out(text, true, argv, json!("cancelled"), ms, None)
        }
        RunOutput::Exited { code, stdout, stderr } => {
            let text = format!("{stdout}{stderr}");
            let lower = text.trim().to_lowercase();
            let refused = code == 1
                && if as_root {
                    lower.contains("incorrect password attempt")
                } else {
                    lower.contains("a password is required") || lower.contains("is not allowed")
                };
            if refused {
                let msg = if as_root { "INSUFFICIENT CLEARANCE" } else { "AGENT ACCOUNT OFFLINE" };
                return out(msg, true, argv, json!(code), ms, Some(text));
            }
            let content = if text.trim().is_empty() {
                format!("EXIT {code} (NO OUTPUT)")
            } else {
                format!("EXIT {code}\n{}", head_tail(&text, 8192))
            };
            out(&content, false, argv, json!(code), ms, Some(text))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Ready,
    AccountOffline,
    NotIsolated,
}

impl Probe {
    pub fn status_line(self) -> Option<&'static str> {
        match self {
            Probe::Ready => None,
            Probe::AccountOffline => Some("AGENT ACCOUNT OFFLINE — SEE DEPLOY"),
            Probe::NotIsolated => Some("AGENT NETWORK NOT ISOLATED — SEE DEPLOY"),
        }
    }
}

pub fn probe(runner: &dyn Runner) -> Probe {
    let mut t = agent_prefix();
    t.push("/usr/bin/true".into());
    if !matches!(runner.run(&t, STEP), RunOutput::Exited { code: 0, .. }) {
        return Probe::AccountOffline;
    }
    let tcp = argv("exec 3<>/dev/tcp/192.0.2.1/9", false);
    let spec = RunSpec { merged: true, ..RunSpec::new(&tcp, STEP) };
    match runner.run_spec(&spec) {
        RunOutput::Exited { code, stdout, stderr } if code != 0 && format!("{stdout}{stderr}").contains("Operation not permitted") => {
            Probe::Ready
        }
        _ => Probe::NotIsolated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::runner::SystemRunner;
    use std::sync::Mutex;

    type Call = (Vec<String>, Option<Vec<u8>>);
    struct Fake {
        calls: Mutex<Vec<Call>>,
        replies: Mutex<Vec<RunOutput>>,
    }
    impl Fake {
        fn new(r: Vec<RunOutput>) -> Self {
            Fake { calls: Mutex::new(vec![]), replies: Mutex::new(r.into_iter().rev().collect()) }
        }
        fn next(&self, argv: &[String], stdin: Option<&[u8]>) -> RunOutput {
            self.calls.lock().unwrap().push((argv.to_vec(), stdin.map(<[u8]>::to_vec)));
            self.replies.lock().unwrap().pop().unwrap_or(RunOutput::NotFound)
        }
    }
    impl Runner for Fake {
        fn run(&self, argv: &[String], _t: Duration) -> RunOutput {
            self.next(argv, None)
        }
        fn run_spec(&self, s: &RunSpec) -> RunOutput {
            self.next(s.argv, s.stdin)
        }
    }
    fn exited(code: i32, s: &str) -> RunOutput {
        RunOutput::Exited { code, stdout: s.into(), stderr: String::new() }
    }
    fn go(f: &Fake, root: bool) -> ToolOutcome {
        run(f, "id", root, Duration::from_secs(9), None, &AtomicBool::new(false))
    }

    #[test]
    fn unprivileged_argv_exact() {
        assert_eq!(
            argv("ls -la", false),
            strs(&["sudo", "-n", "-u", "muthur-agent", "--", "/usr/bin/env", "-i", "-C", "/var/lib/muthur-agent",
                "HOME=/var/lib/muthur-agent", "PATH=/usr/local/sbin:/usr/local/bin:/usr/bin", "LANG=C.UTF-8", "TERM=dumb",
                "/bin/bash", "--noprofile", "--norc", "-c", "ls -la"])
        );
    }

    #[test]
    fn root_argv_exact() {
        assert_eq!(
            argv("id", true),
            strs(&["sudo", "-k", "-S", "-p", "", "--", "/usr/bin/env", "-i", "-C", "/root", "HOME=/root",
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/bin", "LANG=C.UTF-8", "TERM=dumb",
                "/bin/bash", "--noprofile", "--norc", "-c", "exec </dev/null\nid"])
        );
    }

    #[test]
    fn password_only_in_stdin() {
        let mut s = Secret::new();
        "hunter2".chars().for_each(|c| s.push(c));
        let f = Fake::new(vec![exited(0, "uid=0")]);
        let o = run(&f, "id", true, Duration::from_secs(9), Some(&s), &AtomicBool::new(false));
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls[0].1.as_deref(), Some(&b"hunter2\n"[..]));
        assert!(calls[0].0.iter().all(|a| !a.contains("hunter2")));
        assert!(!format!("{o:?}").contains("hunter2"));
    }

    #[test]
    fn root_prefix_closes_stdin() {
        let a = strs(&["bash", "--noprofile", "--norc", "-c", "exec </dev/null\ncat; echo done"]);
        let spec = RunSpec { merged: true, stdin: Some(b"secret\n"), ..RunSpec::new(&a, Duration::from_secs(10)) };
        match SystemRunner.run_spec(&spec) {
            RunOutput::Exited { stdout, stderr, .. } => assert_eq!(format!("{stdout}{stderr}"), "done\n"),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn output_caps() {
        let big = "a".repeat(100_000);
        let f = Fake::new(vec![exited(0, &big)]);
        let o = go(&f, false);
        assert!(o.content.len() <= 8192 + 64, "{}", o.content.len());
        assert!(o.content.contains("BYTES OMITTED"));
        assert_eq!(o.audit_output.as_deref(), Some(big.as_str()));
    }

    #[test]
    fn sudo_mappings() {
        let o = go(&Fake::new(vec![exited(1, "sudo: 1 incorrect password attempt\n")]), true);
        assert_eq!((o.content.as_str(), o.is_error), ("INSUFFICIENT CLEARANCE", true));
        let o = go(&Fake::new(vec![exited(1, "sudo: a password is required\n")]), false);
        assert_eq!((o.content.as_str(), o.is_error), ("AGENT ACCOUNT OFFLINE", true));
        let o = go(&Fake::new(vec![exited(3, "x")]), false);
        assert_eq!((o.content.as_str(), o.is_error), ("EXIT 3\nx", false));
        let o = go(&Fake::new(vec![exited(0, " \n")]), false);
        assert_eq!(o.content, "EXIT 0 (NO OUTPUT)");
    }

    #[test]
    fn timeout_runs_cleanup_unprivileged_only() {
        let f = Fake::new(vec![RunOutput::TimedOut]);
        let o = go(&f, false);
        assert_eq!(o.content, "NO RESPONSE");
        assert!(o.is_error);
        assert_eq!(f.calls.lock().unwrap()[1].0, cleanup_argv());
        let f = Fake::new(vec![RunOutput::TimedOut]);
        let o = go(&f, true);
        assert_eq!(o.content, "NO RESPONSE — OUTCOME UNKNOWN");
        assert_eq!(f.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn cancel_mapping() {
        let f = Fake::new(vec![RunOutput::Cancelled]);
        assert_eq!(go(&f, false).content, "ABORTED BY OPERATOR");
        assert_eq!(f.calls.lock().unwrap()[1].0, cleanup_argv());
        let f = Fake::new(vec![RunOutput::Cancelled]);
        assert_eq!(go(&f, true).content, ABORTED_ISSUED);
        assert_eq!(f.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn probe_states() {
        let p = |r: Vec<RunOutput>| probe(&Fake::new(r));
        assert_eq!(p(vec![exited(1, "")]), Probe::AccountOffline);
        assert_eq!(p(vec![exited(0, ""), exited(1, "bash: connect: Operation not permitted")]), Probe::Ready);
        assert_eq!(p(vec![exited(0, ""), exited(0, "")]), Probe::NotIsolated);
        assert_eq!(p(vec![exited(0, ""), exited(1, "Network is unreachable")]), Probe::NotIsolated);
        assert_eq!(p(vec![exited(0, ""), RunOutput::TimedOut]), Probe::NotIsolated);
        assert_eq!(Probe::Ready.status_line(), None);
        assert_eq!(Probe::NotIsolated.status_line(), Some("AGENT NETWORK NOT ISOLATED — SEE DEPLOY"));
    }
}
