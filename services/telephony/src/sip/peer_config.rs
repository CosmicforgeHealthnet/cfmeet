//! Edge peer configuration consistency (Part H §44, §50).
//!
//! The edge identifies carrier peers by SOURCE ADDRESS only (`identify` sections in the
//! generated trunk files; username identification is disabled). Telephony's
//! `sip_peer_config_refs.allowed_source_cidrs` is the operator's declaration of the same
//! networks. Both must agree: this module parses the `identify` sections of the mounted
//! trunk files and compares them with the database rows (`telephony-admin peers
//! verify`), reporting peers that are missing on either side or whose networks differ.
//! It parses the INI-style edge format only; no credential field is ever read.

use std::collections::BTreeMap;

/// `identify` sections: endpoint name -> `match` networks, as written in the file.
pub fn parse_identify_sections(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut section: Option<String> = None;
    let mut is_identify = false;
    let mut endpoint: Option<String> = None;
    let mut matches: Vec<String> = Vec::new();
    let flush = |section: &Option<String>,
                 is_identify: bool,
                 endpoint: &Option<String>,
                 matches: &mut Vec<String>,
                 out: &mut BTreeMap<String, Vec<String>>| {
        if is_identify {
            if let Some(name) = endpoint.clone().or_else(|| section.clone()) {
                out.entry(name).or_default().append(matches);
            }
        }
        matches.clear();
    };
    for raw in text.lines() {
        let line = raw.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            flush(&section, is_identify, &endpoint, &mut matches, &mut out);
            let name = rest.split(']').next().unwrap_or("").trim();
            section = Some(name.to_string());
            is_identify = false;
            endpoint = None;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "type" => is_identify = value == "identify",
            "endpoint" => endpoint = Some(value.to_string()),
            "match" => matches.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .map(str::to_string),
            ),
            _ => {}
        }
    }
    flush(&section, is_identify, &endpoint, &mut matches, &mut out);
    out
}

/// Canonical form of a network for comparison: a bare address becomes a host network.
pub fn canonical_network(value: &str) -> String {
    match value.split_once('/') {
        Some((ip, bits)) => format!("{}/{}", ip.trim(), bits.trim()),
        None => {
            if value.contains(':') {
                format!("{value}/128")
            } else {
                format!("{value}/32")
            }
        }
    }
}

/// One peer's verification result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerCheck {
    pub peer: String,
    /// `OK`, `MISSING_AT_EDGE`, `MISSING_IN_DATABASE` or `NETWORKS_DIFFER`.
    pub outcome: &'static str,
    pub database_networks: Vec<String>,
    pub edge_networks: Vec<String>,
}

/// Compares the database declaration with the edge `identify` sections.
pub fn verify(
    database: &BTreeMap<String, Vec<String>>,
    edge: &BTreeMap<String, Vec<String>>,
) -> Vec<PeerCheck> {
    let canon = |v: &[String]| -> Vec<String> {
        let mut c: Vec<String> = v.iter().map(|n| canonical_network(n)).collect();
        c.sort();
        c.dedup();
        c
    };
    let mut peers: Vec<&String> = database.keys().chain(edge.keys()).collect();
    peers.sort();
    peers.dedup();
    peers
        .into_iter()
        .map(|peer| {
            let db_ = database.get(peer).map(|v| canon(v)).unwrap_or_default();
            let ed = edge.get(peer).map(|v| canon(v)).unwrap_or_default();
            let outcome = match (database.contains_key(peer), edge.contains_key(peer)) {
                (true, false) => "MISSING_AT_EDGE",
                (false, true) => "MISSING_IN_DATABASE",
                _ if db_ == ed => "OK",
                _ => "NETWORKS_DIFFER",
            };
            PeerCheck {
                peer: peer.clone(),
                outcome,
                database_networks: db_,
                edge_networks: ed,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_sections_are_parsed_and_compared() {
        let text = "; trunk\n[peer-ng-a](trunk-defaults)\ntype = endpoint\naors = peer-ng-a\n\n\
                    [peer-ng-a]\ntype = auth\nusername = u\npassword = p\n\n\
                    [peer-ng-a]\ntype = identify\nendpoint = peer-ng-a\nmatch = 192.0.2.0/24\nmatch = 198.51.100.7 ; sbc\n\n\
                    [peer-ng-b]\ntype = identify\nendpoint = peer-ng-b\nmatch = 203.0.113.0/24, 203.0.113.0/25\n";
        let edge = parse_identify_sections(text);
        assert_eq!(
            edge["peer-ng-a"],
            vec!["192.0.2.0/24".to_string(), "198.51.100.7".to_string()]
        );
        assert_eq!(edge["peer-ng-b"].len(), 2);
        let mut database = BTreeMap::new();
        database.insert(
            "peer-ng-a".to_string(),
            vec!["198.51.100.7/32".to_string(), "192.0.2.0/24".to_string()],
        );
        database.insert("peer-ng-c".to_string(), vec!["192.0.2.0/24".to_string()]);
        let report = verify(&database, &edge);
        let outcome = |p: &str| report.iter().find(|c| c.peer == p).unwrap().outcome;
        assert_eq!(outcome("peer-ng-a"), "OK");
        assert_eq!(outcome("peer-ng-b"), "MISSING_IN_DATABASE");
        assert_eq!(outcome("peer-ng-c"), "MISSING_AT_EDGE");
        database.insert("peer-ng-b".to_string(), vec!["203.0.113.0/24".to_string()]);
        let report = verify(&database, &edge);
        assert_eq!(
            report
                .iter()
                .find(|c| c.peer == "peer-ng-b")
                .unwrap()
                .outcome,
            "NETWORKS_DIFFER"
        );
        // No credential is ever part of the result.
        assert!(!format!("{report:?}").contains("password"));
    }
}
