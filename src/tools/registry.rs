//! New tool registry: persona and technical tool sets with strict validation.

use super::netgate::{url_host, valid_host};
use super::spec;
use crate::config::{AgentSection, ToolsSection};
use crate::llm::ToolSpec;
use crate::ops::AgentId;
use serde_json::{Value, json};
use std::net::IpAddr;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DnsType {
    A,
    Aaaa,
    Cname,
    Mx,
    Ns,
    Txt,
    Soa,
    Srv,
    Caa,
    Ptr,
}

impl DnsType {
    const ALL: [DnsType; 10] = [
        DnsType::A,
        DnsType::Aaaa,
        DnsType::Cname,
        DnsType::Mx,
        DnsType::Ns,
        DnsType::Txt,
        DnsType::Soa,
        DnsType::Srv,
        DnsType::Caa,
        DnsType::Ptr,
    ];

    pub fn word(self) -> &'static str {
        match self {
            DnsType::A => "A",
            DnsType::Aaaa => "AAAA",
            DnsType::Cname => "CNAME",
            DnsType::Mx => "MX",
            DnsType::Ns => "NS",
            DnsType::Txt => "TXT",
            DnsType::Soa => "SOA",
            DnsType::Srv => "SRV",
            DnsType::Caa => "CAA",
            DnsType::Ptr => "PTR",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invocation {
    ShipStatus,
    Containers,
    ContainerInspect { name: String },
    ContainerLogs { name: String, lines: u32 },
    Ping { host: String, count: u32 },
    Traceroute { host: String },
    DnsLookup { name: String, rtype: DnsType },
    PortCheck { host: String, port: u16 },
    HttpCheck { url: String },
    Run { command: String, reason: String, as_root: bool, timeout_secs: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Free,
    Reviewed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetTarget {
    Host(String),
    Url(String),
}

impl Invocation {
    pub fn name(&self) -> &'static str {
        match self {
            Invocation::ShipStatus => "ship_status",
            Invocation::Containers => "containers",
            Invocation::ContainerInspect { .. } => "container_inspect",
            Invocation::ContainerLogs { .. } => "container_logs",
            Invocation::Ping { .. } => "ping",
            Invocation::Traceroute { .. } => "traceroute",
            Invocation::DnsLookup { .. } => "dns_lookup",
            Invocation::PortCheck { .. } => "port_check",
            Invocation::HttpCheck { .. } => "http_check",
            Invocation::Run { .. } => "run",
        }
    }

    pub fn label(&self) -> String {
        match self {
            Invocation::ShipStatus => "SHIP STATUS".into(),
            Invocation::Containers => "CONTAINERS".into(),
            Invocation::ContainerInspect { name } => format!("CONTAINER INSPECT {name}"),
            Invocation::ContainerLogs { name, lines } => format!("CONTAINER LOGS {name} {lines}"),
            Invocation::Ping { host, count } => format!("PING {host} {count}"),
            Invocation::Traceroute { host } => format!("TRACEROUTE {host}"),
            Invocation::DnsLookup { name, rtype } => format!("DNS LOOKUP {name} {}", rtype.word()),
            Invocation::PortCheck { host, port } => format!("PORT CHECK {host} {port}"),
            Invocation::HttpCheck { url } => format!("HTTP CHECK {url}"),
            Invocation::Run { command, .. } => command.clone(),
        }
    }

    pub fn gate(&self) -> Gate {
        match self {
            Invocation::ShipStatus
            | Invocation::Containers
            | Invocation::ContainerInspect { .. }
            | Invocation::ContainerLogs { .. } => Gate::Free,
            _ => Gate::Reviewed,
        }
    }

    pub fn as_root(&self) -> bool {
        matches!(self, Invocation::Run { as_root: true, .. })
    }

    pub fn target(&self) -> Option<NetTarget> {
        match self {
            Invocation::Ping { host, .. } | Invocation::Traceroute { host } | Invocation::PortCheck { host, .. } => {
                Some(NetTarget::Host(host.clone()))
            }
            Invocation::DnsLookup { name, .. } => Some(NetTarget::Host(name.clone())),
            Invocation::HttpCheck { url } => Some(NetTarget::Url(url.clone())),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolDefs {
    specs: Vec<ToolSpec>,
    max_rounds: u32,
    agent: AgentId,
    default_timeout_secs: u32,
    max_timeout_secs: u32,
}

fn fixed_specs() -> Vec<ToolSpec> {
    let name = json!({"type":"string","description":"Container name"});
    vec![
        spec(
            "ship_status",
            "Current ship telemetry: uptime, load, CPU, memory, disks, network, top processes, failed units, containers.",
            json!({}),
        ),
        spec("containers", "List the containers on the ship and their state.", json!({})),
        spec("container_inspect", "Details of one container.", json!({"name": name})),
        spec(
            "container_logs",
            "Recent log lines of one container.",
            json!({"name": name, "lines": {"type":"integer","description":"Number of lines, 1 to 200"}}),
        ),
    ]
}

impl ToolDefs {
    pub fn persona(t: &ToolsSection) -> Self {
        Self {
            specs: if t.enabled { fixed_specs() } else { Vec::new() },
            max_rounds: t.max_rounds,
            agent: AgentId::Muthur,
            default_timeout_secs: 0,
            max_timeout_secs: 0,
        }
    }

    pub fn technical(a: &AgentSection, run_available: bool) -> Self {
        let mut specs = Vec::new();
        if run_available {
            specs.push(spec(
                "run",
                "Run a shell command on the ship as an unprivileged account with no TTY and no network. \
                 With as_root it runs as root, which needs the operator's password. Output is truncated.",
                json!({
                    "command": {"type":"string","description":"Shell command to run"},
                    "reason": {"type":"string","description":"Why the command is needed, one short sentence"},
                    "as_root": {"type":"boolean","description":"Run as root; the operator must enter their password"},
                    "timeout_secs": {"type":"integer","description":format!(
                        "Seconds, 1 to {}; 0 for the default ({})", a.max_timeout_secs, a.default_timeout_secs)},
                }),
            ));
        }
        specs.extend(fixed_specs());
        let host = json!({"type":"string","description":"Host name or IP address"});
        specs.push(spec(
            "ping",
            "Ping a host.",
            json!({"host": host, "count": {"type":"integer","description":"Number of packets, 1 to 10"}}),
        ));
        specs.push(spec("traceroute", "Trace the network route to a host.", json!({"host": host})));
        let types: Vec<&str> = DnsType::ALL.iter().map(|t| t.word()).collect();
        specs.push(spec(
            "dns_lookup",
            "Look up DNS records. PTR needs an IP address as the name.",
            json!({
                "name": {"type":"string","description":"Domain name, or IP address for PTR"},
                "type": {"type":"string","enum":types},
            }),
        ));
        specs.push(spec(
            "port_check",
            "Check whether a TCP port accepts connections.",
            json!({"host": host, "port": {"type":"integer","description":"Port, 1 to 65535"}}),
        ));
        specs.push(spec(
            "http_check",
            "Make one HTTP request and report the status code and the total time.",
            json!({"url": {"type":"string","description":"An http or https URL"}}),
        ));
        Self {
            specs,
            max_rounds: a.max_rounds,
            agent: AgentId::Tech,
            default_timeout_secs: a.default_timeout_secs,
            max_timeout_secs: a.max_timeout_secs,
        }
    }

    pub fn specs(&self) -> &[ToolSpec] {
        &self.specs
    }

    pub fn json(&self) -> String {
        let v: Vec<Value> =
            self.specs.iter().map(|s| json!({"name": s.name, "description": s.description, "input_schema": s.schema})).collect();
        serde_json::to_string(&v).unwrap_or_default()
    }

    pub fn max_rounds(&self) -> u32 {
        self.max_rounds
    }

    pub fn agent(&self) -> AgentId {
        self.agent
    }

    pub fn default_timeout_secs(&self) -> u32 {
        self.default_timeout_secs
    }

    pub fn validate(&self, name: &str, input: &Value) -> Result<Invocation, String> {
        if self.specs.is_empty() {
            return Err("tools disabled".into());
        }
        let empty = serde_json::Map::new();
        let args = input.as_object().unwrap_or(&empty);
        let spec = self.specs.iter().find(|s| s.name == name).ok_or_else(|| format!("unknown tool {name}"))?;
        let props = spec.schema["properties"].as_object();
        let mut keys: Vec<&String> = args.keys().collect();
        keys.sort();
        for k in keys {
            if !props.is_some_and(|p| p.contains_key(k)) {
                return Err(format!("unexpected argument {k}"));
            }
        }
        let text = |k: &str| -> Result<&str, String> {
            args.get(k).ok_or_else(|| format!("missing {k}"))?.as_str().ok_or_else(|| format!("invalid {k}"))
        };
        let int = |k: &str| -> Result<Option<u64>, String> { Ok(args.get(k).ok_or_else(|| format!("missing {k}"))?.as_u64()) };
        let container = || -> Result<String, String> {
            let s = text("name")?;
            let ok = !s.is_empty()
                && s.chars().count() <= 128
                && !s.starts_with('-')
                && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
            if ok { Ok(s.to_string()) } else { Err("invalid container name".into()) }
        };
        let host = |k: &str| -> Result<String, String> {
            let s = text(k)?;
            if valid_host(s) { Ok(s.to_string()) } else { Err("INVALID TARGET".into()) }
        };
        match name {
            "ship_status" => Ok(Invocation::ShipStatus),
            "containers" => Ok(Invocation::Containers),
            "container_inspect" => Ok(Invocation::ContainerInspect { name: container()? }),
            "container_logs" => {
                let n = container()?;
                let lines = int("lines")?.filter(|n| (1..=200).contains(n)).ok_or("lines out of range")?;
                Ok(Invocation::ContainerLogs { name: n, lines: lines as u32 })
            }
            "ping" => {
                let h = host("host")?;
                let count = int("count")?.filter(|n| (1..=10).contains(n)).ok_or("count out of range")?;
                Ok(Invocation::Ping { host: h, count: count as u32 })
            }
            "traceroute" => Ok(Invocation::Traceroute { host: host("host")? }),
            "dns_lookup" => {
                let n = host("name")?;
                let t = text("type")?;
                let rtype = DnsType::ALL.into_iter().find(|d| d.word() == t).ok_or("invalid type")?;
                if rtype == DnsType::Ptr && IpAddr::from_str(&n).is_err() {
                    return Err("INVALID TARGET".into());
                }
                Ok(Invocation::DnsLookup { name: n, rtype })
            }
            "port_check" => {
                let h = host("host")?;
                let port = int("port")?.filter(|n| (1..=65535).contains(n)).ok_or("port out of range")?;
                Ok(Invocation::PortCheck { host: h, port: port as u16 })
            }
            "http_check" => {
                let u = text("url")?;
                url_host(u).ok_or("INVALID TARGET")?;
                Ok(Invocation::HttpCheck { url: u.to_string() })
            }
            "run" => {
                let command = text("command")?;
                if command.is_empty() || command.len() > 4096 || command.contains('\0') || command.trim().is_empty() {
                    return Err("invalid command".into());
                }
                let reason = text("reason")?;
                if reason.is_empty() || reason.len() > 300 {
                    return Err("invalid reason".into());
                }
                let as_root = args.get("as_root").ok_or("missing as_root")?.as_bool().ok_or("invalid as_root")?;
                let t = int("timeout_secs")?.filter(|n| *n <= u64::from(self.max_timeout_secs)).ok_or("timeout out of range")?;
                let timeout_secs = if t == 0 { self.default_timeout_secs } else { t as u32 };
                Ok(Invocation::Run { command: command.to_string(), reason: reason.to_string(), as_root, timeout_secs })
            }
            _ => Err(format!("unknown tool {name}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tech() -> ToolDefs {
        ToolDefs::technical(&AgentSection::default(), true)
    }
    fn names(d: &ToolDefs) -> Vec<&str> {
        d.specs().iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn persona_order() {
        let d = ToolDefs::persona(&ToolsSection::default());
        assert_eq!(names(&d), ["ship_status", "containers", "container_inspect", "container_logs"]);
        assert_eq!(d.agent(), AgentId::Muthur);
        assert_eq!(d.max_rounds(), 8);
        assert!(ToolDefs::persona(&ToolsSection { enabled: false, ..Default::default() }).specs().is_empty());
    }

    #[test]
    fn technical_order_with_and_without_run() {
        let d = tech();
        assert_eq!(
            names(&d),
            ["run", "ship_status", "containers", "container_inspect", "container_logs", "ping", "traceroute", "dns_lookup", "port_check", "http_check"]
        );
        assert_eq!(d.agent(), AgentId::Tech);
        assert_eq!(d.max_rounds(), 24);
        let n = ToolDefs::technical(&AgentSection::default(), false);
        assert_eq!(n.specs().len(), 9);
        assert_eq!(n.specs()[0].name, "ship_status");
    }

    #[test]
    fn run_validation() {
        let d = tech();
        let ok = json!({"command":"ls","reason":"look","as_root":false,"timeout_secs":0});
        assert_eq!(
            d.validate("run", &ok),
            Ok(Invocation::Run { command: "ls".into(), reason: "look".into(), as_root: false, timeout_secs: 30 })
        );
        let with = |k: &str, v: Value| {
            let mut m = ok.clone();
            m[k] = v;
            d.validate("run", &m)
        };
        assert_eq!(with("command", json!("a".repeat(4097))), Err("invalid command".into()));
        assert_eq!(with("command", json!("  ")), Err("invalid command".into()));
        assert_eq!(with("command", json!("a\0b")), Err("invalid command".into()));
        assert_eq!(with("reason", json!("")), Err("invalid reason".into()));
        assert_eq!(with("timeout_secs", json!(301)), Err("timeout out of range".into()));
        assert!(with("timeout_secs", json!(300)).is_ok());
        assert_eq!(d.validate("run", &json!({"command":"ls"})), Err("missing reason".into()));
        assert_eq!(with("x", json!(1)), Err("unexpected argument x".into()));
    }

    #[test]
    fn net_validation() {
        let d = tech();
        assert_eq!(d.validate("ping", &json!({"host":"-x","count":1})), Err("INVALID TARGET".into()));
        assert!(d.validate("ping", &json!({"host":"example.com","count":11})).is_err());
        assert!(d.validate("ping", &json!({"host":"example.com","count":0})).is_err());
        assert!(d.validate("dns_lookup", &json!({"name":"example.com","type":"PTR"})).is_err());
        assert!(d.validate("dns_lookup", &json!({"name":"10.0.0.1","type":"PTR"})).is_ok());
        assert!(d.validate("dns_lookup", &json!({"name":"example.com","type":"BOGUS"})).is_err());
        assert_eq!(d.validate("http_check", &json!({"url":"ftp://x"})), Err("INVALID TARGET".into()));
        assert!(d.validate("port_check", &json!({"host":"db.lan","port":0})).is_err());
        assert!(d.validate("port_check", &json!({"host":"db.lan","port":65536})).is_err());
        assert_eq!(
            d.validate("port_check", &json!({"host":"db.lan","port":5432})),
            Ok(Invocation::PortCheck { host: "db.lan".into(), port: 5432 })
        );
        assert_eq!(d.validate("container_inspect", &json!({"name":"-x"})), Err("invalid container name".into()));
        assert_eq!(d.validate("container_inspect", &json!({"name":"a b"})), Err("invalid container name".into()));
        assert!(d.validate("container_logs", &json!({"name":"web","lines":201})).is_err());
        assert!(d.validate("nope", &json!({})).is_err());
    }

    #[test]
    fn labels_and_gates() {
        let p = Invocation::Ping { host: "example.com".into(), count: 3 };
        assert_eq!(p.label(), "PING example.com 3");
        assert_eq!(p.gate(), Gate::Reviewed);
        assert_eq!(p.target(), Some(NetTarget::Host("example.com".into())));
        assert_eq!(Invocation::ShipStatus.gate(), Gate::Free);
        assert_eq!(Invocation::ShipStatus.label(), "SHIP STATUS");
        assert_eq!(Invocation::ContainerLogs { name: "web".into(), lines: 50 }.label(), "CONTAINER LOGS web 50");
        assert_eq!(Invocation::DnsLookup { name: "example.com".into(), rtype: DnsType::Mx }.label(), "DNS LOOKUP example.com MX");
        assert_eq!(Invocation::PortCheck { host: "db.lan".into(), port: 5432 }.label(), "PORT CHECK db.lan 5432");
        assert_eq!(Invocation::HttpCheck { url: "https://x".into() }.label(), "HTTP CHECK https://x");
        assert_eq!(Invocation::HttpCheck { url: "https://x".into() }.target(), Some(NetTarget::Url("https://x".into())));
        let r = Invocation::Run { command: "ls -l".into(), reason: "r".into(), as_root: true, timeout_secs: 5 };
        assert_eq!(r.label(), "ls -l");
        assert!(r.as_root());
        assert_eq!(r.gate(), Gate::Reviewed);
    }

    #[test]
    fn schemas_use_only_strict_keywords() {
        const ALLOWED: [&str; 10] =
            ["type", "properties", "required", "additionalProperties", "description", "enum", "const", "anyOf", "allOf", "$ref"];
        fn walk(v: &Value, path: &str) {
            let Some(o) = v.as_object() else { return };
            for (k, child) in o {
                assert!(ALLOWED.contains(&k.as_str()), "{path}: unsupported keyword {k}");
                if k == "additionalProperties" {
                    assert_eq!(child, &json!(false), "{path}");
                }
                if k == "properties" {
                    for (name, p) in child.as_object().unwrap() {
                        walk(p, &format!("{path}.{name}"));
                    }
                } else if matches!(k.as_str(), "anyOf" | "allOf") {
                    child.as_array().unwrap().iter().for_each(|c| walk(c, path));
                }
            }
        }
        let d = tech();
        for s in d.specs() {
            walk(&s.schema, &s.name);
        }
        assert_eq!(
            d.specs()[0].schema["properties"]["timeout_secs"]["description"],
            "Seconds, 1 to 300; 0 for the default (30)"
        );
    }
}
