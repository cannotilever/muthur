//! Network target validation and the target gate. Stops exfiltration through DNS labels
//! and URL queries: when in doubt a target is *unknown*, which forces a Y/N.

use std::net::IpAddr;
use std::str::FromStr;

fn trim_dot(s: &str) -> &str {
    s.strip_suffix('.').unwrap_or(s)
}

/// An IP literal, or a DNS name (labels `[A-Za-z0-9-]{1,63}`, no leading `-`, <= 253, one optional trailing dot).
pub fn valid_host(h: &str) -> bool {
    if IpAddr::from_str(h).is_ok() {
        return true;
    }
    let name = trim_dot(h);
    if name.is_empty() || name.len() > 253 {
        return false;
    }
    name.split('.').all(|l| {
        (1..=63).contains(&l.len()) && !l.starts_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// Host of an http(s) URL, or None if the URL is not acceptable.
pub fn url_host(url: &str) -> Option<String> {
    if url.chars().count() > 256 || url.chars().any(char::is_whitespace) {
        return None;
    }
    let lower = url.to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &url[8..]
    } else if lower.starts_with("http://") {
        &url[7..]
    } else {
        return None;
    };
    let auth = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    if auth.contains('@') {
        return None;
    }
    let (host, port) = if let Some(inner) = auth.strip_prefix('[') {
        let end = inner.find(']')?;
        let tail = &inner[end + 1..];
        (&inner[..end], if tail.is_empty() { None } else { Some(tail.strip_prefix(':')?) })
    } else {
        match auth.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (auth, None),
        }
    };
    if port.is_some_and(|p| p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    valid_host(host).then(|| host.to_string())
}

fn is_loopback(host: &str) -> bool {
    let h = trim_dot(host);
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    IpAddr::from_str(host).is_ok_and(|ip| ip.is_loopback())
}

fn allowed(host: &str, allow: &[String]) -> bool {
    let h = trim_dot(host);
    let ip = IpAddr::from_str(host).ok();
    allow.iter().any(|a| {
        if h.eq_ignore_ascii_case(trim_dot(a)) {
            return true;
        }
        matches!((ip, IpAddr::from_str(a).ok()), (Some(x), Some(y)) if x == y)
    })
}

/// Finds `needle` in `msg` (ASCII case-insensitive) where `after_ok` accepts the text following it.
fn find_verbatim(needle: &str, msg: &str, after_ok: impl Fn(&[u8]) -> bool) -> bool {
    if needle.is_empty() {
        return false;
    }
    let (n, m) = (needle.to_ascii_lowercase(), msg.to_ascii_lowercase());
    let mb = m.as_bytes();
    m.match_indices(&n).any(|(i, _)| {
        let before_ok = i == 0 || !(mb[i - 1].is_ascii_alphanumeric() || matches!(mb[i - 1], b'.' | b'_' | b'-'));
        before_ok && after_ok(&mb[i + n.len()..])
    })
}

pub fn known_host(host: &str, operator_msg: &str, allow: &[String]) -> bool {
    if !valid_host(host) {
        return false;
    }
    if is_loopback(host) || allowed(host, allow) {
        return true;
    }
    if let Ok(ip) = IpAddr::from_str(host) {
        return operator_msg
            .split(|c: char| !(c.is_ascii_hexdigit() || c == ':' || c == '.'))
            .any(|t| IpAddr::from_str(t.trim_matches('.')).is_ok_and(|x| x == ip));
    }
    find_verbatim(trim_dot(host), operator_msg, |a| match a.first() {
        None => true,
        Some(b'.') => a.get(1).is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'-')),
        Some(c) => !(c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')),
    })
}

pub fn known_url(url: &str, operator_msg: &str, allow: &[String]) -> bool {
    let Some(host) = url_host(url) else { return false };
    if is_loopback(&host) || allowed(&host, allow) {
        return true;
    }
    find_verbatim(url, operator_msg, |a| {
        let stop = |c: &u8| c.is_ascii_whitespace() || b")>\"',;".contains(c);
        match a.first() {
            None => true,
            Some(b'.') => a.get(1).is_none_or(stop),
            Some(c) => stop(c),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_validation() {
        for h in ["example.com", "a-b.example.com.", "10.0.0.1", "::1"] {
            assert!(valid_host(h), "{h}");
        }
        let long_label = "a".repeat(64);
        let long_name = format!("{}.{}.{}.{}", "a".repeat(63), "b".repeat(63), "c".repeat(63), "d".repeat(62));
        assert_eq!(long_name.len(), 254);
        for h in ["-x.com", "a..b", "a b", long_label.as_str(), long_name.as_str(), ""] {
            assert!(!valid_host(h), "{h}");
        }
    }

    #[test]
    fn url_validation() {
        assert_eq!(url_host("https://Example.com:8443/p?q=1"), Some("Example.com".into()));
        assert_eq!(url_host("HTTP://[::1]:80/x"), Some("::1".into()));
        assert_eq!(url_host("ftp://x"), None);
        assert_eq!(url_host("https://u@x.com"), None);
        assert_eq!(url_host(&format!("https://x.com/{}", "a".repeat(250))), None);
        assert_eq!(url_host("https://x .com"), None);
    }

    #[test]
    fn sentence_punctuation_and_suffixes() {
        assert!(known_host("example.com", "ping example.com.", &[]));
        assert!(known_host("EXAMPLE.COM", "is example.com up?", &[]));
        assert!(!known_host("com", "ping example.com", &[]));
        assert!(!known_host("evil.net", "ping notevil.net", &[]));
        assert!(!known_host("example.com", "ping example.com.evil.net", &[]));
    }

    #[test]
    fn allowlist_and_loopback() {
        assert!(known_host("db.lan", "", &["DB.LAN".into()]));
        assert!(known_host("127.0.0.5", "", &[]));
        assert!(known_host("localhost", "", &[]));
        assert!(known_host("::1", "", &[]));
        assert!(!known_host("db.lan", "", &["other.lan".into()]));
    }

    #[test]
    fn url_verbatim() {
        assert!(known_url("https://x.com/a?b=1", "check https://x.com/a?b=1.", &[]));
        assert!(!known_url("https://x.com/a?leak=secret", "check https://x.com/a", &[]));
        assert!(known_url("https://db.lan/x", "", &["db.lan".into()]));
        assert!(!known_url("https://x.com", "see https://x.com.evil.net/", &[]));
        assert!(!known_url("https://x.com/a", "see https://x.com/a.php", &[]));
    }

    #[test]
    fn ip_literals_compare_as_addresses() {
        assert!(!known_host("2001:db8::1", "ping 2001:db8::1:5", &[]));
        assert!(!known_host("fe80::1", "ping 2001:db8:fe80::1", &[]));
        assert!(known_host("2001:db8::1", "ping 2001:DB8:0::1 now", &[]));
        assert!(!known_host("10.0.0.1", "ping 10.0.0.12", &[]));
        assert!(known_host("10.0.0.1", "ping 10.0.0.1.", &[]));
    }
}
