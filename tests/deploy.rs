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
        assert!(o.status.success(), "sysusers: {}", String::from_utf8_lossy(&o.stderr));
    }
}

#[test]
fn files_have_expected_lines() {
    let sudoers = read("deploy/sudoers.d/muthur-agent");
    assert!(sudoers.contains("OPERATOR ALL=(muthur-agent) NOPASSWD: ALL"));
    assert!(sudoers.contains("Defaults>muthur-agent !use_pty"));
    let nft = read("deploy/nftables/muthur-agent.nft");
    assert!(nft.contains("meta skuid \"muthur-agent\" drop"));
    assert!(nft.contains("oif \"lo\" accept"));
    let sysusers = read("deploy/sysusers.d/muthur-agent.conf");
    assert!(sysusers.contains("m muthur-agent adm"));
    assert!(sysusers.contains("m muthur-agent systemd-journal"));
}
