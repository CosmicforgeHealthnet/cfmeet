//! SIP edge configuration guard (Part H §75): the committed Asterisk templates of the
//! production edge (`infra/telephony/asterisk/`) — and the certification edge that
//! mirrors them — identify endpoints by source IP ONLY.
//!
//! * `endpoint_identifier_order` never names `username` / `auth_username` / `anonymous`
//!   / `header` (a forged From user must never select an endpoint);
//! * the module set is explicit (`autoload = no`) and never loads
//!   `res_pjsip_endpoint_identifier_user.so` or `..._anonymous.so`;
//! * every endpoint (carrier trunks, the media gateway, devices) has an `identify`
//!   section with at least one `match`, and no endpoint is anonymous.
//!
//! Pure file checks: no Asterisk needed (the real edge is certified by
//! `sip_certification.rs`).

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

fn infra() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../infra/telephony")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(infra().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// One `[name](templates)` section of an Asterisk configuration file.
#[derive(Debug, Clone)]
struct Section {
    name: String,
    /// `(!)` — a template, never instantiated.
    template: bool,
    parents: Vec<String>,
    values: Vec<(String, String)>,
}

impl Section {
    fn get(&self, key: &str) -> Vec<&str> {
        self.values
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

/// Parses an Asterisk `.conf` (comments `;`, `#include` lines ignored; sections may share
/// a name — e.g. an endpoint, its aor and its identify).
fn parse(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for raw in text.lines() {
        let line = raw.split(';').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let (name, tail) = rest.split_once(']').expect("section header");
            let options: Vec<String> = tail
                .trim()
                .strip_prefix('(')
                .and_then(|t| t.strip_suffix(')'))
                .map(|t| t.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            out.push(Section {
                name: name.trim().to_string(),
                template: options.iter().any(|o| o == "!"),
                parents: options.into_iter().filter(|o| o != "!").collect(),
                values: Vec::new(),
            });
            continue;
        }
        let (k, v) = line
            .split_once("=>")
            .or_else(|| line.split_once('='))
            .unwrap_or_else(|| panic!("not a key = value line: {raw}"));
        out.last_mut()
            .expect("value outside a section")
            .values
            .push((k.trim().to_string(), v.trim().to_string()));
    }
    out
}

/// A section's effective `type` (its own, or inherited from its templates).
fn type_of<'a>(section: &'a Section, all: &'a [Section]) -> Option<&'a str> {
    if let Some(t) = section.get("type").first() {
        return Some(t);
    }
    section.parents.iter().find_map(|p| {
        all.iter()
            .filter(|s| &s.name == p && s.template)
            .find_map(|s| type_of(s, all))
    })
}

const FORBIDDEN_MODULES: &[&str] = &[
    "res_pjsip_endpoint_identifier_user.so",
    "res_pjsip_endpoint_identifier_anonymous.so",
];

/// The violations of the IP-only identification rule in a set of PJSIP files.
fn pjsip_violations(files: &[(&str, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    let sections: Vec<Section> = files.iter().flat_map(|(_, text)| parse(text)).collect();
    let globals: Vec<&Section> = sections
        .iter()
        .filter(|s| type_of(s, &sections) == Some("global"))
        .collect();
    let orders: Vec<&str> = globals
        .iter()
        .flat_map(|g| g.get("endpoint_identifier_order"))
        .collect();
    if orders.is_empty() {
        problems.push("no explicit endpoint_identifier_order".into());
    }
    for order in orders {
        for id in order.split(',').map(str::trim) {
            if id != "ip" {
                problems.push(format!("endpoint_identifier_order names `{id}`"));
            }
        }
    }
    let mut identified: BTreeMap<&str, usize> = BTreeMap::new();
    for s in sections
        .iter()
        .filter(|s| !s.template && type_of(s, &sections) == Some("identify"))
    {
        for endpoint in s.get("endpoint") {
            *identified.entry(endpoint).or_default() += s.get("match").len();
        }
    }
    for s in sections
        .iter()
        .filter(|s| !s.template && type_of(s, &sections) == Some("endpoint"))
    {
        if s.name.eq_ignore_ascii_case("anonymous") {
            problems.push("an anonymous endpoint exists".into());
        }
        if identified.get(s.name.as_str()).copied().unwrap_or(0) == 0 {
            problems.push(format!(
                "endpoint `{}` has no identify section with a match",
                s.name
            ));
        }
    }
    problems
}

/// The violations of the module rule (explicit set, no username / anonymous identifier).
fn module_violations(modules_conf: &str, loaded: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    let autoload: Vec<String> = parse(modules_conf)
        .iter()
        .flat_map(|s| s.get("autoload").into_iter().map(str::to_string))
        .collect();
    if autoload != ["no"] {
        problems.push(format!(
            "modules must not autoload (autoload = {autoload:?})"
        ));
    }
    for text in loaded {
        // Line scan: `load` lines may stand outside any section (modules.minimal.conf).
        for line in text.lines() {
            let line = line.split(';').next().unwrap_or("").trim();
            if let Some((k, v)) = line.split_once("=>").or_else(|| line.split_once('=')) {
                let (k, v) = (k.trim(), v.trim());
                if matches!(k, "load" | "require") && FORBIDDEN_MODULES.contains(&v) {
                    problems.push(format!("{v} is loaded"));
                }
            }
        }
    }
    problems.sort();
    problems.dedup();
    problems
}

#[test]
fn the_production_edge_identifies_endpoints_by_ip_only() {
    let files = [
        ("asterisk/pjsip.conf", read("asterisk/pjsip.conf")),
        // The shape of every generated carrier trunk file.
        (
            "asterisk/trunk.example.conf",
            read("asterisk/trunk.example.conf"),
        ),
    ];
    let problems = pjsip_violations(&files);
    assert!(problems.is_empty(), "production PJSIP: {problems:?}");
    // Every carrier trunk (an endpoint from the carrier context) is identified.
    let sections: Vec<Section> = files.iter().flat_map(|(_, t)| parse(t)).collect();
    let carriers: Vec<&Section> = sections
        .iter()
        .filter(|s| !s.template && type_of(s, &sections) == Some("endpoint"))
        .filter(|s| s.parents.iter().any(|p| p == "trunk-defaults"))
        .collect();
    assert!(!carriers.is_empty(), "the trunk template shape is parsed");
}

#[test]
fn the_production_edge_never_loads_the_user_or_anonymous_identifier() {
    let modules = read("asterisk/modules.conf");
    let minimal = read("asterisk/modules.minimal.conf");
    let problems = module_violations(&modules, &[&modules, &minimal]);
    assert!(problems.is_empty(), "production modules: {problems:?}");
    assert!(
        minimal.contains("load = res_pjsip_endpoint_identifier_ip.so"),
        "IP identification stays loaded"
    );
}

#[test]
fn the_certification_edge_mirrors_the_ip_only_rule() {
    let problems =
        pjsip_violations(&[("certification/pjsip.conf", read("certification/pjsip.conf"))]);
    assert!(problems.is_empty(), "certification PJSIP: {problems:?}");
    let modules = read("certification/modules.conf");
    let problems = module_violations(&modules, &[&modules]);
    assert!(problems.is_empty(), "certification modules: {problems:?}");
}

#[test]
fn the_guard_rejects_username_identification_unidentified_and_anonymous_endpoints() {
    let base = read("asterisk/pjsip.conf");
    assert!(pjsip_violations(&[("ok", base.clone())]).is_empty());
    // Username identification re-enabled.
    let username = base.replace(
        "endpoint_identifier_order = ip",
        "endpoint_identifier_order = ip,username",
    );
    assert_ne!(username, base);
    assert!(!pjsip_violations(&[("x", username)]).is_empty());
    // A carrier endpoint without an identify section.
    let trunk = "[peer-x](trunk-defaults)\ntransport = transport-tls\naors = peer-x\n\
                 [peer-x]\ntype = aor\ncontact = sip:192.0.2.1\n";
    let problems = pjsip_violations(&[("x", base.clone()), ("t", trunk.into())]);
    assert!(
        problems.iter().any(|p| p.contains("peer-x")),
        "{problems:?}"
    );
    // An identify section without a match does not count.
    let empty = format!("{trunk}[peer-x]\ntype = identify\nendpoint = peer-x\n");
    assert!(!pjsip_violations(&[("x", base.clone()), ("t", empty)]).is_empty());
    // An anonymous endpoint.
    let anonymous = format!(
        "{base}\n[anonymous]\ntype = endpoint\ncontext = from-trunk\n\
         [anonymous]\ntype = identify\nendpoint = anonymous\nmatch = 0.0.0.0/0\n"
    );
    assert!(pjsip_violations(&[("x", anonymous)])
        .iter()
        .any(|p| p.contains("anonymous")));
    // Modules: the user identifier loaded, or autoload on.
    let modules = read("asterisk/modules.conf");
    let minimal = read("asterisk/modules.minimal.conf");
    let with_user = format!("{minimal}load = res_pjsip_endpoint_identifier_user.so\n");
    assert!(!module_violations(&modules, &[&with_user]).is_empty());
    let autoload = modules.replace("autoload = no", "autoload = yes");
    assert!(!module_violations(&autoload, &[&minimal]).is_empty());
}
