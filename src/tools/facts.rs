//! Host facts for the technical agent's system prompt.

use nix::unistd::{User, getuid};

fn os_name() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim().trim_matches('"').to_string()))
        })
        .unwrap_or_else(|| "unknown".into())
}

/// The operator's user name: `$USER`, else the name of the real uid.
pub fn operator() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(|| User::from_uid(getuid()).ok().flatten().map(|u| u.name))
        .unwrap_or_else(|| "unknown".into())
}

pub fn host_facts(runtime: Option<&str>) -> String {
    let host = nix::unistd::gethostname()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".into());
    let kernel = nix::sys::utsname::uname()
        .ok()
        .map(|u| u.release().to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into());
    let init = if std::path::Path::new("/run/systemd/system").exists() {
        "systemd"
    } else {
        "unknown"
    };
    format!(
        "HOST: {host}\nOS: {}\nKERNEL: {kernel}\nINIT: {init}\nCONTAINER RUNTIME: {}\nOPERATOR: {}",
        os_name(),
        runtime.unwrap_or("none"),
        operator()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_facts_has_every_line() {
        let f = host_facts(Some("docker"));
        for k in ["HOST: ", "OS: ", "KERNEL: ", "INIT: ", "CONTAINER RUNTIME: docker", "OPERATOR: "] {
            assert!(f.lines().any(|l| l.starts_with(k)), "{k}: {f}");
        }
        assert!(host_facts(None).contains("CONTAINER RUNTIME: none"));
    }
}
