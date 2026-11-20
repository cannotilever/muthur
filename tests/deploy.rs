//! Lints the muthur-agent deploy files. Needs no root: each check that needs a
//! missing tool (or privileges) prints a note and skips.

use std::path::PathBuf;
use std::process::Command;

fn file(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(file(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Runs `cmd`; `None` (with a note) when the tool is not installed.
fn run(test: &str, cmd: &mut Command) -> Option<std::process::Output> {
    match cmd.output() {
        Ok(o) => Some(o),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("{test}: tool not installed, skipping");
            None
        }
        Err(e) => panic!("{test}: {e}"),
    }
}

#[test]
fn sudoers_template_is_valid() {
    let text = read("deploy/sudoers.d/muthur-agent").replace("OPERATOR", "nobody");
    let tmp = std::env::temp_dir().join(format!("muthur-sudoers-{}", std::process::id()));
    std::fs::write(&tmp, text).unwrap();
    let out = run("sudoers_template_is_valid", Command::new("visudo").arg("-cf").arg(&tmp));
    let _ = std::fs::remove_file(&tmp);
    if let Some(o) = out {
        assert!(o.status.success(), "visudo: {}", String::from_utf8_lossy(&o.stderr));
    }
}

#[test]
fn nft_file_is_valid() {
    let path = file("deploy/nftables/muthur-agent.nft");
    let Some(o) = run("nft_file_is_valid", Command::new("nft").arg("-c").arg("-f").arg(&path))
    else {
        return;
    };
    let err = String::from_utf8_lossy(&o.stderr);
    if !o.status.success() && (err.contains("Operation not permitted") || err.contains("Permission denied")) {
        eprintln!("nft_file_is_valid: needs privileges, skipping");
        return;
    }
    assert!(o.status.success(), "nft: {err}");
}

#[test]
fn sysusers_dry_run() {
    let path = file("deploy/sysusers.d/muthur-agent.conf");
    let out = run(
        "sysusers_dry_run",
        Command::new("systemd-sysusers").arg("--dry-run").arg(&path),
    );
    if let Some(o) = out {
        let err = String::from_utf8_lossy(&o.stderr);
        if !o.status.success() && (err.contains("unrecognized option") || err.contains("unknown option")) {
            eprintln!("sysusers_dry_run: --dry-run unsupported, skipping");
            return;
        }
        assert!(o.status.success(), "sysusers: {err}");
    }
}

#[test]
fn files_have_expected_lines() {
    let sudoers = read("deploy/sudoers.d/muthur-agent");
    assert!(sudoers.contains("OPERATOR ALL=(muthur-agent) NOPASSWD: ALL"));
    assert!(sudoers.contains("Defaults>muthur-agent !use_pty"));
    let grants = sudoers
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.contains("ALL="))
        .count();
    assert_eq!(grants, 1, "exactly one grant line");
    // The chain's rules, in order. UDP must stay `drop`: the startup probe
    // needs the immediate EPERM, and a rejected UDP send reports success.
    let nft = read("deploy/nftables/muthur-agent.nft");
    let rules: Vec<&str> = nft
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("meta ") || l.starts_with("oif "))
        .collect();
    assert_eq!(rules, [
        "meta skuid \"muthur-agent\" udp dport { 53, 853 } drop",
        "meta skuid \"muthur-agent\" tcp dport { 53, 853 } reject with tcp reset",
        "oif \"lo\" accept",
        "meta skuid \"muthur-agent\" meta l4proto tcp reject with tcp reset",
        "meta skuid \"muthur-agent\" drop",
    ]);
    let sysusers = read("deploy/sysusers.d/muthur-agent.conf");
    assert!(sysusers.contains("m muthur-agent adm"));
    assert!(sysusers.contains("m muthur-agent systemd-journal"));
}

/// `install` does not create parent directories, and a minimal host may lack
/// any `/etc/*.d` (Arch ships no `/etc/sysusers.d`). Each one the Makefile
/// installs into must be created first. `/etc/sudoers.d` comes with sudo and
/// keeps its 0750 mode, so it is left alone.
#[test]
fn makefile_creates_each_drop_in_dir_first() {
    let mk = read("Makefile");
    // Shell statements, split on `;` so each install is one statement.
    let stmts: Vec<&str> = mk.split(';').map(str::trim).collect();
    let mut seen = 0;
    for (n, st) in stmts.iter().enumerate() {
        let Some(rest) = st.strip_prefix("sudo install -m ") else { continue };
        let dest = rest.split_whitespace().nth(2).expect("install -m MODE SRC DEST");
        let dir = &dest[..dest.rfind('/').unwrap()];
        if !dir.ends_with(".d") || dir == "/etc/sudoers.d" {
            continue;
        }
        seen += 1;
        let made = stmts[..n]
            .iter()
            .any(|p| p.starts_with("sudo install -d") && p.split_whitespace().any(|w| w == dir));
        assert!(made, "{dir} is installed into without `install -d` first");
    }
    assert!(seen >= 4, "found only {seen} drop-in installs");
}
