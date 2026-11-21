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

/// Reads a `.in` template and substitutes each `@K@`; no placeholder may remain.
fn render(rel: &str, vars: &[(&str, &str)]) -> String {
    let mut text = read(rel);
    for (k, v) in vars {
        text = text.replace(&format!("@{k}@"), v);
    }
    for (i, _) in text.match_indices('@') {
        let rest = &text[i + 1..];
        if let Some(end) = rest.find('@') {
            let name = &rest[..end];
            assert!(
                name.is_empty() || !name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'),
                "{rel}: unrendered placeholder @{name}@"
            );
        }
    }
    text
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
    let Some(o) = run("nft_file_is_valid", Command::new("nft").arg("-c").arg("-f").arg(&path)) else {
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
    let tmp = std::env::temp_dir().join(format!("muthur-sysusers-{}.conf", std::process::id()));
    std::fs::write(&tmp, render("deploy/sysusers.d/muthur-agent.conf.in", &[("NOLOGIN", "/usr/bin/nologin")])).unwrap();
    let out = run("sysusers_dry_run", Command::new("systemd-sysusers").arg("--dry-run").arg(&tmp));
    let _ = std::fs::remove_file(&tmp);
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
    let grants = sudoers.lines().filter(|l| !l.trim_start().starts_with('#') && l.contains("ALL=")).count();
    assert_eq!(grants, 1, "exactly one grant line");
    // The chain's rules, in order. UDP must stay `drop`: the startup probe
    // needs the immediate EPERM, and a rejected UDP send reports success.
    let nft = read("deploy/nftables/muthur-agent.nft");
    let rules: Vec<&str> = nft.lines().map(str::trim).filter(|l| l.starts_with("meta ") || l.starts_with("oif ")).collect();
    assert_eq!(
        rules,
        [
            "meta skuid \"muthur-agent\" udp dport { 53, 853 } drop",
            "meta skuid \"muthur-agent\" tcp dport { 53, 853 } reject with tcp reset",
            "oif \"lo\" accept",
            "meta skuid \"muthur-agent\" meta l4proto tcp reject with tcp reset",
            "meta skuid \"muthur-agent\" drop",
        ]
    );
    let sysusers = render("deploy/sysusers.d/muthur-agent.conf.in", &[("NOLOGIN", "/usr/bin/nologin")]);
    assert!(sysusers.contains("m muthur-agent adm"));
    assert!(sysusers.contains("m muthur-agent systemd-journal"));
}

#[test]
fn udev_template_renders() {
    let m = r#"ATTRS{idVendor}=="0403", ATTRS{idProduct}=="6001", ATTRS{serial}=="X""#;
    let text = render("deploy/udev/99-ttyMUTHUR.rules.in", &[("MATCH", m)]);
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains(r#"SYMLINK+="ttyMUTHUR""#));
    let tmp = std::env::temp_dir().join(format!("muthur-udev-{}.rules", std::process::id()));
    std::fs::write(&tmp, &text).unwrap();
    let out = run("udev_template_renders", Command::new("udevadm").arg("verify").arg(&tmp));
    let _ = std::fs::remove_file(&tmp);
    if let Some(o) = out {
        let err = String::from_utf8_lossy(&o.stderr);
        if !o.status.success() && (err.contains("nrecognized") || err.contains("nknown") || err.contains("nvalid command")) {
            eprintln!("udev_template_renders: verify unsupported, skipping");
            return;
        }
        assert!(o.status.success(), "udevadm: {err}{}", String::from_utf8_lossy(&o.stdout));
    }
}

#[test]
fn dropin_template_renders() {
    let text = render(
        "deploy/systemd/serial-getty@ttyMUTHUR.service.d/override.conf.in",
        &[("AGETTY", "/sbin/agetty"), ("BAUD", "38400")],
    );
    assert!(text.contains(r"ExecStart=-/sbin/agetty -o '-p -- \\u' 38400 %I vt100"));
}

/// The getty instance named in the udev rule must unescape to the symlink's
/// name: a dash would unescape to "/" and bind the unit to a device that never
/// appears (`serial-getty@a-b` waits on `/dev/a/b`).
#[test]
fn getty_instance_matches_symlink() {
    let text = read("deploy/udev/99-ttyMUTHUR.rules.in");
    let field = |key: &str| -> String {
        let start = text.find(key).unwrap_or_else(|| panic!("no {key} in the udev rule")) + key.len();
        let rest = &text[start..];
        rest[..rest.find('"').unwrap()].to_string()
    };
    let link = field("SYMLINK+=\"");
    let unit = field("ENV{SYSTEMD_WANTS}+=\"");
    let instance = unit
        .strip_prefix("serial-getty@")
        .and_then(|u| u.strip_suffix(".service"))
        .unwrap_or_else(|| panic!("unexpected unit {unit}"));
    assert_eq!(instance, link, "instance and symlink differ");
    assert!(
        file("deploy/systemd").join(format!("serial-getty@{instance}.service.d")).is_dir(),
        "drop-in dir for {instance} missing"
    );
    let Some(o) = run("getty_instance_matches_symlink", Command::new("systemd-escape").arg("-u").arg(instance)) else {
        return;
    };
    assert!(o.status.success(), "systemd-escape: {}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim_end(), link, "systemd-escape -u {instance}");
}
