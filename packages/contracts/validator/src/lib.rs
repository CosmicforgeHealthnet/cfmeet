//! Deterministic validator for `packages/contracts`.
//!
//! The validator enforces that the contract package is genuinely authoritative:
//!
//! * `manifest.json` conforms to `manifest.schema.json`;
//! * contract ids are `cfmeet.<owner>.<name>`, unique per major version; versions are
//!   semantic; paths are unique, exist, stay inside the package and (for contract-first
//!   documents) carry the major version (`v<major>.…`);
//! * owners are `platform` or a deployable service that exists under `services/` — a
//!   contract cannot be registered for a service that has not been created;
//! * every JSON file in the package is the manifest, a registered contract, or a fixture
//!   (`examples/`, `counter-examples/`) of a registered JSON Schema contract — there are no
//!   unregistered "shadow" copies;
//! * JSON Schema contracts declare Draft 2020-12, carry `$id =
//!   https://schemas.cfmeet.com/<path>`, are valid against the meta-schema, compile with
//!   every reference resolved offline, and their examples validate while their
//!   counter-examples are rejected;
//! * OpenAPI documents validate against the official OpenAPI 3.0 / 3.1 meta-schemas
//!   (vendored under `meta/`), every `$ref` resolves (internal pointers and registered
//!   schema `$id`s), 3.1 component schemas compile, and `info.version` equals the
//!   manifest version.
//!
//! No network access is used: references resolve only to registered contracts.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use jsonschema::{Registry, Resource};
use serde_json::Value;

/// Base URI of every JSON Schema contract `$id`.
pub const SCHEMA_BASE_URI: &str = "https://schemas.cfmeet.com/";
pub const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

const OAS_30_META: &str = include_str!("../meta/oas-3.0-2021-09-28.schema.json");
const OAS_31_META: &str = include_str!("../meta/oas-3.1-2022-10-07.schema.json");

/// Directories inside the contract package that are tooling, not contracts.
const TOOLING_DIRS: &[&str] = &["validator"];
/// Package-level files that are not contracts.
const PACKAGE_FILES: &[&str] = &["manifest.json", "manifest.schema.json"];
const FIXTURE_DIRS: &[&str] = &["examples", "counter-examples"];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    /// Path relative to the contract package root (or `manifest.json`).
    pub location: String,
    pub message: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location, self.message)
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub errors: Vec<Finding>,
    pub contracts: usize,
    pub examples: usize,
    pub counter_examples: usize,
    pub json_files: usize,
}

impl Report {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    fn error(&mut self, location: impl Into<String>, message: impl Into<String>) {
        self.errors.push(Finding {
            location: location.into(),
            message: message.into(),
        });
    }
}

#[derive(Debug, Clone)]
struct Contract {
    id: String,
    version: String,
    major: u64,
    kind: String,
    path: String,
    owner: String,
    authority: String,
}

/// Validates the contract package at `contracts_root`. `repo_root` is the repository
/// root, used to check that contract owners exist.
pub fn validate(contracts_root: &Path, repo_root: &Path) -> Report {
    let mut report = Report::default();

    // 1. Every JSON file parses.
    let mut documents: BTreeMap<String, Value> = BTreeMap::new();
    for file in json_files(contracts_root) {
        let rel = relative(contracts_root, &file);
        report.json_files += 1;
        match fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
        {
            Ok(value) => {
                documents.insert(rel, value);
            }
            Err(e) => report.error(rel, format!("invalid JSON: {e}")),
        }
    }

    // 2. Manifest structure.
    let Some(manifest) = documents.get("manifest.json").cloned() else {
        report.error("manifest.json", "missing or unparseable manifest");
        return finish(report);
    };
    let Some(manifest_schema) = documents.get("manifest.schema.json").cloned() else {
        report.error(
            "manifest.schema.json",
            "missing or unparseable manifest schema",
        );
        return finish(report);
    };
    match jsonschema::options().build(&manifest_schema) {
        Ok(validator) => {
            for error in validator.iter_errors(&manifest) {
                report.error(
                    "manifest.json",
                    format!("{} (at {})", error, error.instance_path()),
                );
            }
        }
        Err(e) => report.error("manifest.schema.json", format!("invalid schema: {e}")),
    }
    if !report.is_ok() {
        return finish(report);
    }

    // 3. Manifest semantics.
    let contracts = manifest_contracts(&manifest);
    report.contracts = contracts.len();
    check_manifest_semantics(
        &contracts,
        contracts_root,
        repo_root,
        &documents,
        &mut report,
    );

    // 4. No unregistered JSON files.
    let registered: BTreeSet<&str> = contracts.iter().map(|c| c.path.as_str()).collect();
    let schema_dirs: Vec<String> = contracts
        .iter()
        .filter(|c| c.kind == "json-schema")
        .map(|c| parent_dir(&c.path))
        .collect();
    for rel in documents.keys() {
        if PACKAGE_FILES.contains(&rel.as_str()) || registered.contains(rel.as_str()) {
            continue;
        }
        if fixture_owner(rel, &schema_dirs).is_none() {
            report.error(
                rel.clone(),
                "JSON file is neither a registered contract nor a fixture of one (unregistered duplicate authority?)",
            );
        }
    }

    // 5. JSON Schema contracts: build an offline registry of every registered $id.
    let mut resources: Vec<(String, Value)> = Vec::new();
    for c in contracts.iter().filter(|c| c.kind == "json-schema") {
        let Some(doc) = documents.get(&c.path) else {
            continue;
        };
        let expected_id = format!("{SCHEMA_BASE_URI}{}", c.path);
        match doc.get("$schema").and_then(Value::as_str) {
            Some(DRAFT_2020_12) => {}
            other => report.error(
                c.path.clone(),
                format!("$schema must be {DRAFT_2020_12}, found {other:?}"),
            ),
        }
        match doc.get("$id").and_then(Value::as_str) {
            Some(id) if id == expected_id => {}
            other => report.error(
                c.path.clone(),
                format!("$id must be {expected_id}, found {other:?}"),
            ),
        }
        if let Err(e) = jsonschema::meta::validate(doc) {
            report.error(c.path.clone(), format!("not a valid JSON Schema: {e}"));
        }
        resources.push((expected_id, doc.clone()));
    }
    let registry = match build_registry(&resources) {
        Ok(registry) => Some(registry),
        Err(e) => {
            report.error("manifest.json", format!("schema registry: {e}"));
            None
        }
    };

    if let Some(registry) = &registry {
        for c in contracts.iter().filter(|c| c.kind == "json-schema") {
            if let Some(doc) = documents.get(&c.path) {
                check_schema_contract(c, doc, registry, &documents, &schema_dirs, &mut report);
            }
        }
        for c in contracts.iter().filter(|c| c.kind == "openapi") {
            if let Some(doc) = documents.get(&c.path) {
                check_openapi_contract(c, doc, registry, &resources, &mut report);
            }
        }
    }

    // Fixtures of contracts that are not JSON Schemas are not allowed (no schema to check).
    finish(report)
}

fn finish(mut report: Report) -> Report {
    report.errors.sort();
    report.errors.dedup();
    report
}

fn manifest_contracts(manifest: &Value) -> Vec<Contract> {
    manifest["contracts"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|c| {
                    let s = |k: &str| c[k].as_str().unwrap_or_default().to_string();
                    let version = s("version");
                    Contract {
                        major: version
                            .split('.')
                            .next()
                            .and_then(|m| m.parse().ok())
                            .unwrap_or(0),
                        id: s("id"),
                        version,
                        kind: s("kind"),
                        path: s("path"),
                        owner: s("owner"),
                        authority: s("authority"),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn check_manifest_semantics(
    contracts: &[Contract],
    contracts_root: &Path,
    repo_root: &Path,
    documents: &BTreeMap<String, Value>,
    report: &mut Report,
) {
    let mut by_version: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut by_major: BTreeMap<(String, u64), usize> = BTreeMap::new();
    let mut by_path: BTreeMap<String, usize> = BTreeMap::new();
    for c in contracts {
        *by_version
            .entry((c.id.clone(), c.version.clone()))
            .or_default() += 1;
        *by_major.entry((c.id.clone(), c.major)).or_default() += 1;
        *by_path.entry(c.path.clone()).or_default() += 1;
    }
    for ((id, version), n) in &by_version {
        if *n > 1 {
            report.error(
                "manifest.json",
                format!("duplicate contract version {id}@{version}"),
            );
        }
    }
    for ((id, major), n) in &by_major {
        if *n > 1 {
            report.error(
                "manifest.json",
                format!("contract id {id} registered more than once for major version {major}"),
            );
        }
    }
    for (path, n) in &by_path {
        if *n > 1 {
            report.error(
                "manifest.json",
                format!("path {path} is claimed by more than one contract (duplicate canonical ownership)"),
            );
        }
    }

    for c in contracts {
        let loc = format!("manifest.json[{}@{}]", c.id, c.version);
        // Path safety and existence.
        let path = Path::new(&c.path);
        if path.is_absolute()
            || path
                .components()
                .any(|p| !matches!(p, Component::Normal(_)))
        {
            report.error(
                loc.clone(),
                format!("path {} must be relative and normalized", c.path),
            );
            continue;
        }
        if TOOLING_DIRS
            .iter()
            .any(|d| c.path.starts_with(&format!("{d}/")))
        {
            report.error(loc.clone(), "contracts may not live in tooling directories");
        }
        if !contracts_root.join(path).is_file() {
            report.error(loc.clone(), format!("path {} does not exist", c.path));
        } else if !documents.contains_key(&c.path) {
            report.error(
                loc.clone(),
                format!("path {} is not parseable JSON", c.path),
            );
        }

        // Identity and ownership.
        let segments: Vec<&str> = c.id.split('.').collect();
        if segments.len() != 3 || segments[1] != c.owner {
            report.error(loc.clone(), format!("id must be cfmeet.{}.<name>", c.owner));
        }
        if c.owner != "platform" && !repo_root.join("services").join(&c.owner).is_dir() {
            report.error(
                loc.clone(),
                format!(
                    "owner `{}` is not an existing deployable service (services/{}): contracts are registered by implemented services only",
                    c.owner, c.owner
                ),
            );
        }
        if c.major == 0 {
            report.error(
                loc.clone(),
                "major version must be at least 1 for a registered contract",
            );
        }

        // Versioned, kind-consistent paths for contract-first documents.
        if c.authority == "contract-first" {
            let file = path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or_default();
            let expected_suffix = match c.kind.as_str() {
                "json-schema" => ".schema.json",
                _ => ".openapi.json",
            };
            let expected = format!("v{}{}", c.major, expected_suffix);
            if file != expected {
                report.error(
                    loc.clone(),
                    format!("contract-first file must be named {expected} (major-versioned), found {file}"),
                );
            }
        }
    }
}

fn build_registry(resources: &[(String, Value)]) -> Result<Registry<'static>, String> {
    let mut builder = Registry::new();
    for (uri, doc) in resources {
        builder = builder
            .add(uri.clone(), Resource::from_contents(doc.clone()))
            .map_err(|e| e.to_string())?;
    }
    builder.prepare().map_err(|e| e.to_string())
}

fn check_schema_contract(
    contract: &Contract,
    doc: &Value,
    registry: &Registry<'_>,
    documents: &BTreeMap<String, Value>,
    schema_dirs: &[String],
    report: &mut Report,
) {
    let validator = match jsonschema::options()
        .with_registry(registry)
        .should_validate_formats(true)
        .build(doc)
    {
        Ok(v) => v,
        Err(e) => {
            report.error(
                contract.path.clone(),
                format!("schema does not compile: {e}"),
            );
            return;
        }
    };
    let dir = parent_dir(&contract.path);
    let definitions_only = is_definitions_only(doc);
    let mut examples = 0;

    if let Some(inline) = doc.get("examples").and_then(Value::as_array) {
        for (i, example) in inline.iter().enumerate() {
            examples += 1;
            for error in validator.iter_errors(example) {
                report.error(
                    contract.path.clone(),
                    format!(
                        "inline example {i} is invalid: {error} (at {})",
                        error.instance_path()
                    ),
                );
            }
        }
    }
    for (rel, value) in documents {
        let Some((owner_dir, fixture_kind)) = fixture_owner(rel, schema_dirs) else {
            continue;
        };
        if owner_dir != dir {
            continue;
        }
        match fixture_kind {
            "examples" => {
                examples += 1;
                report.examples += 1;
                for error in validator.iter_errors(value) {
                    report.error(
                        rel.clone(),
                        format!(
                            "example does not conform to {}: {error} (at {})",
                            contract.id,
                            error.instance_path()
                        ),
                    );
                }
            }
            _ => {
                report.counter_examples += 1;
                if validator.is_valid(value) {
                    report.error(
                        rel.clone(),
                        format!(
                            "counter-example is accepted by {} but must be rejected",
                            contract.id
                        ),
                    );
                }
            }
        }
    }
    if examples == 0 && !definitions_only {
        report.error(
            contract.path.clone(),
            "JSON Schema contract has no examples (add examples/*.json)",
        );
    }
}

/// A schema whose root carries only metadata and `$defs` (a shared vocabulary).
fn is_definitions_only(doc: &Value) -> bool {
    doc.as_object().is_some_and(|o| {
        o.contains_key("$defs")
            && o.keys().all(|k| {
                matches!(
                    k.as_str(),
                    "$schema" | "$id" | "title" | "description" | "$defs"
                )
            })
    })
}

fn check_openapi_contract(
    contract: &Contract,
    doc: &Value,
    registry: &Registry<'_>,
    resources: &[(String, Value)],
    report: &mut Report,
) {
    let version = doc
        .get("openapi")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let meta_source = if version.starts_with("3.0.") {
        OAS_30_META
    } else if version.starts_with("3.1.") {
        OAS_31_META
    } else {
        report.error(
            contract.path.clone(),
            format!("unsupported OpenAPI version {version:?} (3.0.x or 3.1.x required)"),
        );
        return;
    };
    let meta: Value = serde_json::from_str(meta_source).expect("vendored meta-schema is JSON");
    match jsonschema::options().build(&meta) {
        Ok(validator) => {
            for error in validator.iter_errors(doc) {
                report.error(
                    contract.path.clone(),
                    format!(
                        "not a valid OpenAPI {version} document: {error} (at {})",
                        error.instance_path()
                    ),
                );
            }
        }
        Err(e) => report.error(
            contract.path.clone(),
            format!("meta-schema does not compile: {e}"),
        ),
    }

    match doc.pointer("/info/version").and_then(Value::as_str) {
        Some(v) if v == contract.version => {}
        other => report.error(
            contract.path.clone(),
            format!(
                "info.version {other:?} must equal the manifest version {}",
                contract.version
            ),
        ),
    }

    // Every $ref resolves: internal pointers, or registered schema $ids (+ pointer).
    let mut refs = Vec::new();
    collect_refs(doc, "", &mut refs);
    for (at, reference) in refs {
        let (base, fragment) = match reference.split_once('#') {
            Some((b, f)) => (b, f),
            None => (reference.as_str(), ""),
        };
        let target = if base.is_empty() {
            Some(doc)
        } else {
            resources
                .iter()
                .find(|(uri, _)| uri == base)
                .map(|(_, v)| v)
        };
        match target {
            None => report.error(
                contract.path.clone(),
                format!("$ref {reference} at {at} does not resolve to a registered contract"),
            ),
            Some(target) if !fragment.is_empty() && target.pointer(fragment).is_none() => report
                .error(
                    contract.path.clone(),
                    format!("$ref {reference} at {at} points to a missing location"),
                ),
            Some(_) => {}
        }
    }

    // OpenAPI 3.1 component schemas are JSON Schema 2020-12: they must compile.
    if version.starts_with("3.1.") {
        if let Some(schemas) = doc
            .pointer("/components/schemas")
            .and_then(Value::as_object)
        {
            let base = format!("{SCHEMA_BASE_URI}{}", contract.path);
            for name in schemas.keys() {
                let wrapper = serde_json::json!({
                    "$schema": DRAFT_2020_12,
                    "$ref": format!("{base}#/components/schemas/{}", escape_pointer(name)),
                });
                let local =
                    match registry.extend([(base.clone(), Resource::from_contents(doc.clone()))]) {
                        Ok(b) => b.prepare(),
                        Err(e) => Err(e),
                    };
                let result = local.map_err(|e| e.to_string()).and_then(|r| {
                    jsonschema::options()
                        .with_registry(&r)
                        .build(&wrapper)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                });
                if let Err(e) = result {
                    report.error(
                        contract.path.clone(),
                        format!("components.schemas.{name} does not compile as JSON Schema 2020-12: {e}"),
                    );
                }
            }
        }
    }
}

fn escape_pointer(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn collect_refs(value: &Value, at: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let here = format!("{at}/{}", escape_pointer(k));
                if k == "$ref" {
                    if let Some(r) = v.as_str() {
                        out.push((here.clone(), r.to_string()));
                    }
                }
                collect_refs(v, &here, out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                collect_refs(v, &format!("{at}/{i}"), out);
            }
        }
        _ => {}
    }
}

/// `(contract_dir, "examples" | "counter-examples")` when `rel` is a fixture of a JSON
/// Schema contract located in one of `schema_dirs`.
fn fixture_owner<'a>(rel: &str, schema_dirs: &'a [String]) -> Option<(&'a str, &'static str)> {
    for dir in schema_dirs {
        for kind in FIXTURE_DIRS {
            let prefix = if dir.is_empty() {
                format!("{kind}/")
            } else {
                format!("{dir}/{kind}/")
            };
            if let Some(rest) = rel.strip_prefix(&prefix) {
                if !rest.contains('/') {
                    return Some((dir.as_str(), kind));
                }
            }
        }
    }
    None
}

fn parent_dir(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default()
}

fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

fn json_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if dir == root && TOOLING_DIRS.contains(&name.as_str()) {
                continue;
            }
            if name == "target" || name.starts_with('.') {
                continue;
            }
            walk(root, &path, out);
        } else if name.ends_with(".json") {
            out.push(path);
        }
    }
}
