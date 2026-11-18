//! Tool registry: definitions sent to the model and strict validation of its calls.

use crate::config::{Elevate, ToolsSection};
use crate::llm::ToolSpec;
pub mod exec;
pub mod runner;

use serde_json::{Value, json};

pub const CAP: usize = 4096;

/// Truncate to at most CAP bytes on a char boundary, marking the cut.
pub fn cap(text: String) -> String {
    if text.len() <= CAP {
        return text;
    }
    let mut end = CAP;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[TRUNCATED]", &text[..end])
}

const SUFFIXES: [&str; 11] =
    [".service", ".socket", ".timer", ".mount", ".target", ".path", ".slice", ".scope", ".swap", ".automount", ".device"];

/// Canonical unit name, or None if it could be anything but a plain unit name.
pub fn normalize_unit(s: &str) -> Option<String> {
    if s.is_empty() || s.len() > 128 || s.starts_with(['-', '.']) {
        return None;
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.' | '-' | '@')) {
        return None;
    }
    if SUFFIXES.iter().any(|x| s.ends_with(x)) {
        Some(s.to_string())
    } else {
        Some(format!("{s}.service"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Start,
    Stop,
    Restart,
}

impl Verb {
    fn word(self) -> &'static str {
        match self {
            Verb::Start => "start",
            Verb::Stop => "stop",
            Verb::Restart => "restart",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitState {
    Running,
    Failed,
    Inactive,
}

impl UnitState {
    fn word(self) -> &'static str {
        match self {
            UnitState::Running => "running",
            UnitState::Failed => "failed",
            UnitState::Inactive => "inactive",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invocation {
    ShipStatus,
    ServiceStatus { unit: String },
    Journal { unit: String, lines: u32 },
    ListUnits { state: UnitState },
    ListeningPorts,
    Service { verb: Verb, unit: String },
}

impl Invocation {
    pub fn tier(&self) -> u8 {
        if matches!(self, Invocation::Service { .. }) { 2 } else { 1 }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Invocation::ShipStatus => "ship_status",
            Invocation::ServiceStatus { .. } => "service_status",
            Invocation::Journal { .. } => "journal",
            Invocation::ListUnits { .. } => "list_units",
            Invocation::ListeningPorts => "listening_ports",
            Invocation::Service { verb, .. } => match verb {
                Verb::Start => "start_service",
                Verb::Stop => "stop_service",
                Verb::Restart => "restart_service",
            },
        }
    }

    pub fn label(&self) -> String {
        match self {
            Invocation::ShipStatus => "SHIP STATUS".into(),
            Invocation::ServiceStatus { unit } => format!("SERVICE STATUS {unit}"),
            Invocation::Journal { unit, lines } => format!("JOURNAL {unit} {lines}"),
            Invocation::ListUnits { state } => format!("LIST UNITS {}", state.word()),
            Invocation::ListeningPorts => "LISTENING PORTS".into(),
            Invocation::Service { verb, unit } => format!("{} {unit}", verb.word()),
        }
        .to_uppercase()
    }
}

#[derive(Clone, Debug)]
pub struct ToolDefs {
    specs: Vec<ToolSpec>,
    restart: Vec<String>,
    start: Vec<String>,
    stop: Vec<String>,
    enabled: bool,
    max_rounds: u32,
    journal_max_lines: u32,
    elevate: Elevate,
}

fn normalize_list(kind: &str, list: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for e in list {
        match normalize_unit(e.trim()) {
            Some(u) => {
                if !out.contains(&u) {
                    out.push(u);
                }
            }
            None => tracing::warn!("ignoring invalid [tools.services].{kind} entry {e:?}"),
        }
    }
    out
}

fn spec(name: &str, description: &str, properties: Value) -> ToolSpec {
    let required: Vec<&str> = properties.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
    ToolSpec {
        name: name.into(),
        description: description.into(),
        schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    }
}

impl ToolDefs {
    pub fn new(cfg: &ToolsSection) -> Self {
        let mut journal_max_lines = cfg.journal_max_lines;
        if journal_max_lines == 0 {
            tracing::warn!("[tools].journal_max_lines = 0; using 1");
            journal_max_lines = 1;
        }
        let restart = normalize_list("restart", &cfg.services.restart);
        let start = normalize_list("start", &cfg.services.start);
        let stop = normalize_list("stop", &cfg.services.stop);
        let unit_free = json!({"type":"string","description":"Unit name, e.g. nginx or nginx.service"});
        let mut specs = Vec::new();
        if cfg.enabled {
            specs.push(spec(
                "ship_status",
                "Current ship telemetry: uptime, load, CPU, memory, disks, network, top processes, failed units, containers.",
                json!({}),
            ));
            specs.push(spec("service_status", "Status of one systemd service on the ship.", json!({"unit": unit_free})));
            specs.push(spec(
                "journal",
                "Recent journal lines for one systemd unit.",
                json!({"unit": unit_free, "lines": {"type":"integer","minimum":1,"maximum":journal_max_lines}}),
            ));
            specs.push(spec(
                "list_units",
                "List systemd units in one state.",
                json!({"state": {"type":"string","enum":["running","failed","inactive"]}}),
            ));
            specs.push(spec("listening_ports", "List listening network ports and their processes.", json!({})));
            for (verb, list, desc) in [
                (Verb::Start, &start, "Start an allowlisted systemd service. Requires operator override; may be denied."),
                (Verb::Stop, &stop, "Stop an allowlisted systemd service. Requires operator override; may be denied."),
                (Verb::Restart, &restart, "Restart an allowlisted systemd service. Requires operator override; may be denied."),
            ] {
                if !list.is_empty() {
                    specs.push(spec(
                        &format!("{}_service", verb.word()),
                        desc,
                        json!({"unit": {"type":"string","enum":list}}),
                    ));
                }
            }
        }
        Self {
            specs,
            restart,
            start,
            stop,
            enabled: cfg.enabled,
            max_rounds: cfg.max_rounds,
            journal_max_lines,
            elevate: cfg.elevate,
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

    pub fn elevate(&self) -> Elevate {
        self.elevate
    }

    pub fn validate(&self, name: &str, input: &Value) -> Result<Invocation, String> {
        if !self.enabled {
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
        let unit = || -> Result<String, String> {
            let s = args.get("unit").and_then(Value::as_str).ok_or("missing unit")?;
            normalize_unit(s).ok_or_else(|| "invalid unit".to_string())
        };
        match name {
            "ship_status" => Ok(Invocation::ShipStatus),
            "listening_ports" => Ok(Invocation::ListeningPorts),
            "service_status" => Ok(Invocation::ServiceStatus { unit: unit()? }),
            "journal" => {
                let unit = unit()?;
                let lines = args.get("lines").ok_or("missing lines")?;
                let n = lines.as_u64().filter(|n| (1..=u64::from(self.journal_max_lines)).contains(n));
                let n = n.ok_or("lines out of range")?;
                Ok(Invocation::Journal { unit, lines: n as u32 })
            }
            "list_units" => {
                let s = args.get("state").and_then(Value::as_str).ok_or("missing state")?;
                let state = match s {
                    "running" => UnitState::Running,
                    "failed" => UnitState::Failed,
                    "inactive" => UnitState::Inactive,
                    _ => return Err("invalid state".into()),
                };
                Ok(Invocation::ListUnits { state })
            }
            "start_service" | "stop_service" | "restart_service" => {
                let (verb, list) = match name {
                    "start_service" => (Verb::Start, &self.start),
                    "stop_service" => (Verb::Stop, &self.stop),
                    _ => (Verb::Restart, &self.restart),
                };
                let unit = unit()?;
                if !list.contains(&unit) {
                    return Err("unit not on allowlist".into());
                }
                Ok(Invocation::Service { verb, unit })
            }
            _ => Err(format!("unknown tool {name}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServiceLists;

    fn defs() -> ToolDefs {
        ToolDefs::new(&ToolsSection::default())
    }
    fn defs_with_restart(l: &[&str]) -> ToolDefs {
        ToolDefs::new(&ToolsSection {
            services: ServiceLists { restart: l.iter().map(|s| s.to_string()).collect(), ..Default::default() },
            ..Default::default()
        })
    }

    #[test] fn normalizes_units() { assert_eq!(normalize_unit("nginx").as_deref(), Some("nginx.service")); assert_eq!(normalize_unit("getty@tty1.service").as_deref(), Some("getty@tty1.service")); assert_eq!(normalize_unit("foo.timer").as_deref(), Some("foo.timer")); }
    #[test] fn rejects_hostile_units() { for u in ["nginx; rm -rf /", "--help", "-x", "../x", "a b", "", &"a".repeat(129), "ng\ninx", "a\\b"] { assert_eq!(normalize_unit(u), None, "{u:?}"); } }
    #[test] fn definitions_order_and_allowlist_gating() { let d = ToolDefs::new(&ToolsSection{ services: ServiceLists{ restart: vec!["nginx".into()], ..Default::default() }, ..Default::default() }); let names: Vec<_> = d.specs().iter().map(|s| s.name.as_str()).collect(); assert_eq!(names, ["ship_status","service_status","journal","list_units","listening_ports","restart_service"]); assert_eq!(d.specs()[5].schema["properties"]["unit"]["enum"], json!(["nginx.service"])); }
    #[test] fn disabled_has_no_specs() { assert!(ToolDefs::new(&ToolsSection{ enabled: false, ..Default::default() }).specs().is_empty()); }
    #[test] fn validate_tier1() { let d = defs(); assert_eq!(d.validate("service_status", &json!({"unit":"nginx"})), Ok(Invocation::ServiceStatus{ unit: "nginx.service".into() })); assert_eq!(d.validate("journal", &json!({"unit":"nginx","lines":20})), Ok(Invocation::Journal{ unit: "nginx.service".into(), lines: 20 })); assert_eq!(d.validate("journal", &json!({"unit":"nginx","lines":51})), Err("lines out of range".into())); assert_eq!(d.validate("list_units", &json!({"state":"zombie"})), Err("invalid state".into())); }
    #[test] fn missing_required_argument_is_rejected() { assert_eq!(defs().validate("service_status", &json!({})), Err("missing unit".into())); }
    #[test] fn tier2_enforces_allowlist_even_past_schema() { let d = defs_with_restart(&["nginx"]); assert!(matches!(d.validate("restart_service", &json!({"unit":"nginx.service"})), Ok(Invocation::Service{ verb: Verb::Restart, .. }))); assert_eq!(d.validate("restart_service", &json!({"unit":"sshd"})), Err("unit not on allowlist".into())); assert_eq!(d.validate("stop_service", &json!({"unit":"nginx"})), Err("unknown tool stop_service".into())); }
    #[test] fn extra_argument_rejected() { assert_eq!(defs().validate("ship_status", &json!({"x":1})), Err("unexpected argument x".into())); }
    #[test] fn cap_truncates_on_char_boundary() { let s = "é".repeat(3000); let c = cap(s); assert!(c.ends_with("\n[TRUNCATED]")); assert!(c.len() <= CAP + 12); assert!(cap("short".into()) == "short"); }
    #[test] fn more_validation() {
        let d = defs();
        assert_eq!(d.validate("service_status", &json!({"unit":"a;b"})), Err("invalid unit".into()));
        assert_eq!(d.validate("journal", &json!({"unit":"nginx"})), Err("missing lines".into()));
        assert_eq!(d.validate("journal", &json!({"unit":"nginx","lines":2.5})), Err("lines out of range".into()));
        assert_eq!(d.validate("list_units", &json!({})), Err("missing state".into()));
        let off = ToolDefs::new(&ToolsSection { enabled: false, ..Default::default() });
        assert_eq!(off.validate("ship_status", &json!({})), Err("tools disabled".into()));
        let r = defs_with_restart(&["nginx"]);
        assert_eq!(r.validate("restart_service", &json!({"unit":"nginx","force":true})), Err("unexpected argument force".into()));
        let z = ToolDefs::new(&ToolsSection { journal_max_lines: 0, ..Default::default() });
        assert_eq!(z.specs()[2].schema["properties"]["lines"]["maximum"], json!(1));
    }
    #[test] fn cap_cuts_mid_character() { let s = format!("aa{}", "€".repeat(2000)); let c = cap(s); assert!(c.ends_with("\n[TRUNCATED]")); assert!(std::str::from_utf8(c.as_bytes()).is_ok()); assert!(c.len() < 4096 + 12 && c.len() > 4000); assert!(c.starts_with("aa€")); }
    #[test] fn labels() { assert_eq!(Invocation::Service{ verb: Verb::Restart, unit: "nginx.service".into() }.label(), "RESTART NGINX.SERVICE"); assert_eq!(Invocation::Journal{ unit: "a.service".into(), lines: 20 }.label(), "JOURNAL A.SERVICE 20"); }
}
