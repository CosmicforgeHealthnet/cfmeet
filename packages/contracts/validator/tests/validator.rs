//! Contract-authority rules, proven by mutation: each test copies the real contract
//! package into a temporary repository, breaks exactly one rule and asserts the
//! validator reports it. The unmodified package must pass.

use std::{
    fs,
    path::{Path, PathBuf},
};

use cfmeet_contracts_validator::{validate, Report};
use serde_json::{json, Value};

fn real_contracts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "validator" || name == "target" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            copy_dir(&path, &to.join(&name));
        } else {
            fs::copy(&path, to.join(&name)).unwrap();
        }
    }
}

/// A temporary repository containing a copy of the contract package and the deployable
/// services that exist today.
struct Sandbox {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    contracts: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_path_buf();
        let contracts = repo.join("packages/contracts");
        copy_dir(&real_contracts(), &contracts);
        for service in [
            "api-gateway",
            "calling",
            "developer-platform",
            "meeting-service",
            "messaging",
            "numbering",
            "platform-access",
            "privacy-relay",
            "sms-gateway",
            "telephony",
        ] {
            fs::create_dir_all(repo.join("services").join(service)).unwrap();
        }
        Self {
            _dir: dir,
            repo,
            contracts,
        }
    }

    fn read(&self, rel: &str) -> Value {
        serde_json::from_str(&fs::read_to_string(self.contracts.join(rel)).unwrap()).unwrap()
    }

    fn write(&self, rel: &str, value: &Value) {
        let path = self.contracts.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
    }

    fn edit(&self, rel: &str, f: impl FnOnce(&mut Value)) {
        let mut value = self.read(rel);
        f(&mut value);
        self.write(rel, &value);
    }

    fn edit_manifest(&self, f: impl FnOnce(&mut Vec<Value>)) {
        self.edit("manifest.json", |m| {
            f(m["contracts"].as_array_mut().unwrap());
        });
    }

    fn run(&self) -> Report {
        validate(&self.contracts, &self.repo)
    }
}

fn assert_rejected(report: &Report, needle: &str) {
    assert!(
        report.errors.iter().any(|e| e.to_string().contains(needle)),
        "expected an error containing {needle:?}, got {:#?}",
        report.errors
    );
}

fn contract<'a>(contracts: &'a mut [Value], id: &str) -> &'a mut Value {
    contracts.iter_mut().find(|c| c["id"] == id).unwrap()
}

#[test]
fn the_real_contract_package_is_valid() {
    let report = validate(&real_contracts(), &real_contracts().join("../.."));
    assert!(report.is_ok(), "{:#?}", report.errors);
    assert!(report.contracts >= 9);
    assert!(report.examples >= 9);
    assert!(report.counter_examples >= 20);
}

#[test]
fn an_unmodified_copy_is_valid() {
    let report = Sandbox::new().run();
    assert!(report.is_ok(), "{:#?}", report.errors);
}

#[test]
fn duplicate_contract_version_is_rejected() {
    let s = Sandbox::new();
    let mut version = String::new();
    s.edit_manifest(|c| {
        let mut dup = contract(c, "cfmeet.platform.common").clone();
        version = dup["version"].as_str().unwrap().to_string();
        dup["path"] = json!("platform/common/v1.schema.json");
        c.push(dup);
    });
    let report = s.run();
    assert_rejected(
        &report,
        &format!("duplicate contract version cfmeet.platform.common@{version}"),
    );
    assert_rejected(&report, "duplicate canonical ownership");
}

#[test]
fn duplicate_contract_id_within_a_major_is_rejected() {
    let s = Sandbox::new();
    s.edit_manifest(|c| {
        let mut dup = contract(c, "cfmeet.platform.event-envelope").clone();
        dup["version"] = json!("1.1.0");
        c.push(dup);
    });
    assert_rejected(&s.run(), "registered more than once for major version 1");
}

#[test]
fn dangling_and_unsafe_paths_are_rejected() {
    let s = Sandbox::new();
    s.edit_manifest(|c| {
        contract(c, "cfmeet.platform.event-envelope")["path"] =
            json!("platform/events/envelope/v9.schema.json");
    });
    assert_rejected(&s.run(), "does not exist");

    let s = Sandbox::new();
    s.edit_manifest(|c| {
        contract(c, "cfmeet.platform.event-envelope")["path"] = json!("../outside/v1.schema.json");
    });
    assert!(!s.run().is_ok());
}

#[test]
fn unparseable_json_is_rejected() {
    let s = Sandbox::new();
    fs::write(
        s.contracts
            .join("platform/events/envelope/examples/full.json"),
        "{ not json",
    )
    .unwrap();
    assert_rejected(&s.run(), "invalid JSON");
}

#[test]
fn structurally_invalid_json_schema_is_rejected() {
    let s = Sandbox::new();
    s.edit("platform/events/envelope/v1.schema.json", |d| {
        d["properties"]["schema_version"]["type"] = json!(12);
    });
    assert_rejected(&s.run(), "not a valid JSON Schema");
}

#[test]
fn example_violating_its_contract_is_rejected() {
    let s = Sandbox::new();
    s.edit(
        "platform/service-auth/token-claims/examples/minimal.json",
        |d| d["aud"] = json!("*"),
    );
    assert_rejected(
        &s.run(),
        "example does not conform to cfmeet.platform.service-token-claims",
    );
}

/// Mutation of a contract rule: opening the closed claim set lets a token self-declare
/// permissions, which the counter-example fixture must catch.
#[test]
fn loosening_a_contract_rule_is_caught_by_its_counter_examples() {
    let s = Sandbox::new();
    s.edit("platform/service-auth/token-claims/v1.schema.json", |d| {
        d["additionalProperties"] = json!(true);
    });
    let report = s.run();
    assert_rejected(
        &report,
        "self-declared-permissions.json: counter-example is accepted",
    );
    assert_rejected(
        &report,
        "self-declared-scopes.json: counter-example is accepted",
    );

    let s = Sandbox::new();
    s.edit("platform/service-auth/token-header/v1.schema.json", |d| {
        d["properties"]["alg"] = json!({ "type": "string" });
    });
    assert_rejected(&s.run(), "alg-none.json: counter-example is accepted");

    let s = Sandbox::new();
    s.edit(
        "platform/service-auth/delegated-platform-context/v1.schema.json",
        |d| {
            d["additionalProperties"] = json!(true);
        },
    );
    assert_rejected(
        &s.run(),
        "telephone-number-field.json: counter-example is accepted",
    );
}

#[test]
fn unregistered_duplicate_authority_is_rejected() {
    let s = Sandbox::new();
    let copy = s.read("platform/service-auth/token-claims/v1.schema.json");
    s.write(
        "platform/service-auth/token-claims/v1-copy.schema.json",
        &copy,
    );
    assert_rejected(&s.run(), "neither a registered contract nor a fixture");
}

#[test]
fn contracts_for_unimplemented_services_are_rejected() {
    // `directory` is not a service in this repository (no services/directory): it cannot
    // own a contract.
    let s = Sandbox::new();
    s.write(
        "directory/http/v1.openapi.json",
        &json!({ "openapi": "3.1.0", "info": { "title": "x", "version": "1.0.0" }, "paths": {} }),
    );
    s.edit_manifest(|c| {
        c.push(json!({
            "id": "cfmeet.directory.http-api",
            "version": "1.0.0",
            "kind": "openapi",
            "path": "directory/http/v1.openapi.json",
            "owner": "directory",
            "visibility": "public",
            "lifecycle": "experimental",
            "authority": "contract-first",
            "description": "fabricated"
        }));
    });
    assert_rejected(
        &s.run(),
        "owner `directory` is not an existing deployable service",
    );
}

#[test]
fn messaging_contracts_require_the_messaging_service() {
    // The registered Messaging contracts are valid only because services/messaging exists:
    // without the service directory the same, unmodified contracts are rejected.
    let s = Sandbox::new();
    fs::remove_dir_all(s.repo.join("services/messaging")).unwrap();
    assert_rejected(
        &s.run(),
        "owner `messaging` is not an existing deployable service",
    );
}

#[test]
fn part_vi_contracts_require_their_services() {
    // Part VI: the Developer Platform and API Gateway contracts exist only with their
    // implemented services.
    for service in ["developer-platform", "api-gateway"] {
        let s = Sandbox::new();
        fs::remove_dir_all(s.repo.join("services").join(service)).unwrap();
        assert_rejected(
            &s.run(),
            &format!("owner `{service}` is not an existing deployable service"),
        );
    }
}

#[test]
fn invalid_openapi_documents_are_rejected() {
    let s = Sandbox::new();
    s.edit("calling/http/v1.openapi.json", |d| {
        d.as_object_mut().unwrap().remove("info");
    });
    assert_rejected(&s.run(), "not a valid OpenAPI 3.1.0 document");

    let s = Sandbox::new();
    s.edit("openapi/meeting-service.json", |d| {
        d["paths"]["/health"]["get"]["responses"] = json!("nope");
    });
    assert_rejected(&s.run(), "not a valid OpenAPI 3.0.3 document");
}

#[test]
fn dangling_openapi_references_are_rejected() {
    let s = Sandbox::new();
    s.edit("calling/http/v1.openapi.json", |d| {
        d["paths"]["/health"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
            ["$ref"] = json!("#/components/schemas/Missing");
    });
    assert_rejected(&s.run(), "points to a missing location");

    let s = Sandbox::new();
    s.edit("calling/http/v1.openapi.json", |d| {
        d["components"]["headers"]["XRequestId"]["schema"]["$ref"] =
            json!("https://example.com/elsewhere.json#/x");
    });
    assert_rejected(&s.run(), "does not resolve to a registered contract");
}

#[test]
fn schema_ids_and_versioned_paths_are_enforced() {
    let s = Sandbox::new();
    s.edit("platform/events/envelope/v1.schema.json", |d| {
        d["$id"] = json!("https://schemas.cfmeet.com/somewhere/else.json");
    });
    assert_rejected(&s.run(), "$id must be");

    let s = Sandbox::new();
    s.edit_manifest(|c| {
        contract(c, "cfmeet.platform.event-envelope")["version"] = json!("2.0.0");
    });
    assert_rejected(&s.run(), "must be named v2.schema.json");

    let s = Sandbox::new();
    s.edit_manifest(|c| {
        // Any version other than the document's own info.version.
        contract(c, "cfmeet.calling.http-api")["version"] = json!("1.99.0");
    });
    assert_rejected(&s.run(), "info.version");
}

#[test]
fn manifest_structure_is_enforced() {
    let s = Sandbox::new();
    s.edit_manifest(|c| {
        contract(c, "cfmeet.platform.common")["visibility"] = json!("everyone");
    });
    assert_rejected(&s.run(), "manifest.json");

    let s = Sandbox::new();
    s.edit_manifest(|c| {
        contract(c, "cfmeet.meeting-service.http-api")
            .as_object_mut()
            .unwrap()
            .remove("generated_by");
    });
    assert!(!s.run().is_ok());

    let s = Sandbox::new();
    s.edit_manifest(|c| {
        contract(c, "cfmeet.platform.common")["id"] = json!("cfmeet.calling.common");
    });
    assert_rejected(&s.run(), "id must be cfmeet.platform.<name>");
}

#[test]
fn schemas_without_examples_are_rejected() {
    let s = Sandbox::new();
    for f in fs::read_dir(s.contracts.join("platform/events/envelope/examples")).unwrap() {
        fs::remove_file(f.unwrap().path()).unwrap();
    }
    assert_rejected(&s.run(), "has no examples");
}

#[test]
fn findings_are_deterministic() {
    let s = Sandbox::new();
    s.edit("platform/events/envelope/v1.schema.json", |d| {
        d["$id"] = json!("x");
    });
    let a: Vec<String> = s.run().errors.iter().map(ToString::to_string).collect();
    let b: Vec<String> = s.run().errors.iter().map(ToString::to_string).collect();
    assert_eq!(a, b);
    assert!(!a.is_empty());
}
