//! Part F §52–§53 enterprise foundation: typed enterprise call policy, departments,
//! administrative scopes, versioning, durable audit and tenant / application /
//! environment isolation (a foreign resource id behaves exactly like a missing one).

mod common;

use axum::http::{Method, StatusCode};
use calling::{
    calls::command::CallScope,
    domain::{ApplicationStatus, EnvironmentKind, TenantStatus},
    platform::scopes::names,
};
use common::enterprise::{credential, default_rules, id_of, EnterpriseWorld};
use serde_json::{json, Value};

async fn world() -> Option<EnterpriseWorld> {
    EnterpriseWorld::new().await
}

#[tokio::test]
async fn policies_are_typed_versioned_and_audited() {
    let Some(e) = world().await else { return };
    let created = e
        .create(
            "/v1/enterprise/policies",
            json!({ "name": "Head office", "isDefault": true, "rules": default_rules() }),
        )
        .await;
    let id = id_of(&created, "policyId");
    assert_eq!(created["version"], 1);
    assert_eq!(created["isDefault"], true);
    assert_eq!(created["status"], "ACTIVE");
    assert_eq!(created["rules"]["maxRoutingHops"], 8);

    // Effective policy = the stored default.
    let r = e
        .admin(Method::GET, "/v1/enterprise/effective-policy", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["policyId"], id.to_string());

    // Update with the right version.
    let mut rules = default_rules();
    rules["maxRingGroupFanout"] = json!(4);
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/policies/{id}"),
            Some(json!({ "expectedVersion": 1, "rules": rules, "name": "HQ" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["version"], 2);
    assert_eq!(r.json["name"], "HQ");
    assert_eq!(r.json["rules"]["maxRingGroupFanout"], 4);

    // A stale version is refused and changes nothing.
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/policies/{id}"),
            Some(json!({ "expectedVersion": 1, "name": "stale" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "VERSION_CONFLICT");
    let r = e
        .admin(Method::GET, &format!("/v1/enterprise/policies/{id}"), None)
        .await;
    assert_eq!(r.json["name"], "HQ");
    assert_eq!(r.json["version"], 2);

    // Disable (DELETE with expectedVersion): never deleted, default cleared.
    let r = e
        .admin(
            Method::DELETE,
            &format!("/v1/enterprise/policies/{id}?expectedVersion=2"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "DISABLED");
    assert_eq!(r.json["isDefault"], false);
    assert!(r.json["disabledAt"].is_string());
    // Effective falls back to the built-in default.
    let r = e
        .admin(Method::GET, "/v1/enterprise/effective-policy", None)
        .await;
    assert!(r.json["policyId"].is_null());
    assert_eq!(r.json["version"], 0);

    // Audit: create, update, disable with versions and request / correlation ids.
    let r = e
        .admin(
            Method::GET,
            &format!("/v1/enterprise/audit?resourceId={id}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let records = r.json["records"].as_array().unwrap();
    let ops: Vec<&str> = records
        .iter()
        .map(|r| r["operation"].as_str().unwrap())
        .collect();
    assert_eq!(ops, ["DISABLE", "UPDATE", "CREATE"]);
    assert_eq!(records[0]["oldVersion"], 2);
    assert_eq!(records[0]["newVersion"], 3);
    assert_eq!(records[2]["newVersion"], 1);
    for record in records {
        assert_eq!(record["actorKind"], "CUSTOMER_APPLICATION");
        assert_eq!(
            record["actorCredentialId"],
            e.admin.credential.id.to_string()
        );
        assert!(record["requestId"].as_str().unwrap().starts_with("req_"));
        assert!(!record["correlationId"].as_str().unwrap().is_empty());
        assert_eq!(record["resourceType"], "CALL_POLICY");
    }
}

#[tokio::test]
async fn one_default_policy_at_a_time() {
    let Some(e) = world().await else { return };
    let a = e
        .create(
            "/v1/enterprise/policies",
            json!({ "name": "A", "isDefault": true, "rules": default_rules() }),
        )
        .await;
    let b = e
        .create(
            "/v1/enterprise/policies",
            json!({ "name": "B", "isDefault": true, "rules": default_rules() }),
        )
        .await;
    let r = e
        .admin(
            Method::GET,
            &format!("/v1/enterprise/policies/{}", id_of(&a, "policyId")),
            None,
        )
        .await;
    assert_eq!(r.json["isDefault"], false);
    assert_eq!(
        r.json["version"], 2,
        "clearing the default is a versioned change"
    );
    let r = e
        .admin(Method::GET, "/v1/enterprise/effective-policy", None)
        .await;
    assert_eq!(r.json["policyId"], b["policyId"]);
    // Duplicate active name.
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/policies",
            Some(json!({ "name": "b", "rules": default_rules() })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
}

#[tokio::test]
async fn policy_schema_is_validated() {
    let Some(e) = world().await else { return };
    let mut unknown = default_rules();
    unknown["recordEverything"] = json!(true);
    let mut bad_fanout = default_rules();
    bad_fanout["maxRingGroupFanout"] = json!(0);
    let mut bad_country = default_rules();
    bad_country["publicDestinationCountries"] = json!(["Nigeria"]);
    let mut inconsistent = default_rules();
    inconsistent["publicForwardingAllowed"] = json!(true);
    let mut long_numeric = default_rules();
    long_numeric["extensionMaxLength"] = json!(9);
    for (body, what) in [
        (json!({ "name": "x", "rules": unknown }), "unknown rule"),
        (json!({ "name": "x", "rules": bad_fanout }), "fanout"),
        (json!({ "name": "x", "rules": bad_country }), "country"),
        (
            json!({ "name": "x", "rules": inconsistent }),
            "public forwarding",
        ),
        (json!({ "name": "x", "rules": long_numeric }), "length"),
        (json!({ "name": "x" }), "missing rules"),
        (
            json!({ "name": "", "rules": default_rules() }),
            "empty name",
        ),
        (
            json!({ "name": "+2348012345678", "rules": default_rules() }),
            "number as name",
        ),
        (
            json!({ "name": "x", "rules": default_rules(), "tenantId": "x" }),
            "client scope",
        ),
        (
            json!({ "name": "x", "rules": default_rules(), "version": 7 }),
            "client version",
        ),
    ] {
        let r = e
            .admin(Method::POST, "/v1/enterprise/policies", Some(body))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{what}: {}", r.text);
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR", "{what}");
    }
    let count: i64 = common::scalar_i64(
        e.h().conn(),
        "SELECT count(*) FROM enterprise_call_policies",
    )
    .await;
    assert_eq!(count, 0);
}

#[tokio::test]
async fn public_forwarding_needs_an_operator_granted_capability() {
    let Some(e) = world().await else { return };
    let mut rules = default_rules();
    rules["externalPublicCallingAllowed"] = json!(true);
    rules["publicForwardingAllowed"] = json!(true);
    let body = json!({ "name": "Public", "rules": rules });
    let r = e
        .admin(Method::POST, "/v1/enterprise/policies", Some(body.clone()))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "ENTERPRISE_POLICY_DENIED");
    // Granting oneself the capability through the customer API is refused as well.
    let mut grab = default_rules();
    grab["adminCapabilities"] = json!(["MANAGE_ROUTING", "MANAGE_PUBLIC_FORWARDING"]);
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/policies",
            Some(json!({ "name": "grab", "isDefault": true, "rules": grab })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let mut grab_recording = default_rules();
    grab_recording["adminCapabilities"] = json!(["MANAGE_RECORDING"]);
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/policies",
            Some(json!({ "name": "rec", "rules": grab_recording })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);

    // The operator grants it (audited as OPERATOR); then the administrator may.
    let baseline = e
        .h()
        .state
        .enterprise
        .operator_set_capabilities(
            e.a.w.scope(),
            vec![
                calling::enterprise::policy::AdminCapability::ManageRouting,
                calling::enterprise::policy::AdminCapability::ManagePublicForwarding,
            ],
        )
        .await
        .unwrap();
    assert!(baseline.is_default);
    let r = e
        .admin(Method::POST, "/v1/enterprise/policies", Some(body))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = e
        .admin(
            Method::GET,
            &format!("/v1/enterprise/audit?resourceId={}", baseline.policy_id),
            None,
        )
        .await;
    assert_eq!(r.json["records"][0]["actorKind"], "OPERATOR");
    assert!(r.json["records"][0]["actorServiceAccountId"].is_null());
}

#[tokio::test]
async fn departments_and_membership() {
    let Some(e) = world().await else { return };
    let dept = e.department("Sales").await;
    let id = id_of(&dept, "departmentId");
    assert_eq!(dept["version"], 1);
    assert_eq!(dept["memberCount"], 0);

    let alice = e.a.principal("alice").await;
    let bob = e.a.principal("bob").await;
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/departments/{id}/members"),
            Some(json!({ "expectedVersion": 1, "members": [
                { "principalId": alice.id(), "role": "MANAGER" },
                { "principalId": bob.id() },
            ]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["version"], 2);
    assert_eq!(r.json["memberCount"], 2);

    // Duplicates and inactive / unknown principals are refused atomically.
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/departments/{id}/members"),
            Some(json!({ "expectedVersion": 2, "members": [
                { "principalId": alice.id() }, { "principalId": alice.id() }
            ]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/departments/{id}/members"),
            Some(json!({ "expectedVersion": 2, "members": [
                { "principalId": alice.id() }, { "principalId": uuid::Uuid::new_v4() }
            ]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let r = e
        .admin(
            Method::GET,
            &format!("/v1/enterprise/departments/{id}"),
            None,
        )
        .await;
    assert_eq!(r.json["memberCount"], 2);
    assert_eq!(r.json["version"], 2);

    // Rename, then disable.
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/departments/{id}"),
            Some(json!({ "expectedVersion": 2, "name": "Sales EMEA", "description": "Regional" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["description"], "Regional");
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/departments/{id}"),
            Some(json!({ "expectedVersion": 3, "description": null })),
        )
        .await;
    assert!(r.json["description"].is_null(), "{}", r.text);
    let r = e
        .admin(
            Method::DELETE,
            &format!("/v1/enterprise/departments/{id}?expectedVersion=4"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "DISABLED");
    assert_eq!(r.json["memberCount"], 2, "membership history is kept");
    // A disabled department cannot change.
    let r = e
        .admin(
            Method::PATCH,
            &format!("/v1/enterprise/departments/{id}"),
            Some(json!({ "expectedVersion": 5, "name": "again" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);

    // Listing: keyset pagination, status filter.
    for n in 0..3 {
        e.department(&format!("Team {n}")).await;
    }
    let r = e
        .admin(
            Method::GET,
            "/v1/enterprise/departments?status=ACTIVE&limit=2",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["departments"].as_array().unwrap().len(), 2);
    let cursor = r.json["nextCursor"].as_str().unwrap().to_string();
    let r = e
        .admin(
            Method::GET,
            &format!("/v1/enterprise/departments?status=ACTIVE&limit=2&cursor={cursor}"),
            None,
        )
        .await;
    assert_eq!(r.json["departments"].as_array().unwrap().len(), 1);
    assert!(r.json["nextCursor"].is_null());
    for bad in [
        "/v1/enterprise/departments?limit=0",
        "/v1/enterprise/departments?status=GONE",
        "/v1/enterprise/departments?cursor=zz",
        "/v1/enterprise/departments?tenantId=x",
        "/v1/enterprise/departments?limit=1&limit=2",
    ] {
        let r = e.admin(Method::GET, bad, None).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text);
    }
}

/// Every route of every resource kind, with ids from another scope: always 404, never
/// a change.
async fn assert_foreign_ids_are_not_found(
    e: &EnterpriseWorld,
    token: &str,
    policy: &Value,
    dept: &Value,
) {
    let p = id_of(policy, "policyId");
    let d = id_of(dept, "departmentId");
    for (method, path, body) in [
        (Method::GET, format!("/v1/enterprise/policies/{p}"), None),
        (
            Method::PATCH,
            format!("/v1/enterprise/policies/{p}"),
            Some(json!({ "expectedVersion": 1, "name": "hijack" })),
        ),
        (
            Method::DELETE,
            format!("/v1/enterprise/policies/{p}?expectedVersion=1"),
            None,
        ),
        (Method::GET, format!("/v1/enterprise/departments/{d}"), None),
        (
            Method::PATCH,
            format!("/v1/enterprise/departments/{d}"),
            Some(json!({ "expectedVersion": 1, "name": "hijack" })),
        ),
        (
            Method::PUT,
            format!("/v1/enterprise/departments/{d}/members"),
            Some(json!({ "expectedVersion": 1, "members": [] })),
        ),
        (
            Method::DELETE,
            format!("/v1/enterprise/departments/{d}?expectedVersion=1"),
            None,
        ),
    ] {
        let r = e.send(token, method.clone(), &path, body).await;
        assert_eq!(
            r.status,
            StatusCode::NOT_FOUND,
            "{method} {path}: {}",
            r.text
        );
        assert_eq!(r.json["error"]["code"], "NOT_FOUND");
    }
    // Listings and audit of the other scope show nothing of ours.
    for path in [
        "/v1/enterprise/policies",
        "/v1/enterprise/departments",
        "/v1/enterprise/audit",
    ] {
        let r = e.send(token, Method::GET, path, None).await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text);
        assert!(!r.text.contains(&p.to_string()), "{path}");
        assert!(!r.text.contains(&d.to_string()), "{path}");
    }
    let r = e
        .send(
            token,
            Method::GET,
            &format!("/v1/enterprise/audit?resourceId={p}"),
            None,
        )
        .await;
    assert_eq!(r.json["records"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn cross_tenant_application_and_environment_ids_behave_like_missing_ones() {
    let Some(e) = world().await else { return };
    let policy = e
        .create(
            "/v1/enterprise/policies",
            json!({ "name": "Ours", "rules": default_rules() }),
        )
        .await;
    let dept = e.department("Ours").await;

    // Another tenant.
    let (_, other_admin) = e.other_tenant("globex").await;
    assert_foreign_ids_are_not_found(&e, other_admin.secret.expose_once(), &policy, &dept).await;

    // Another application of the SAME tenant.
    let app_b = e.h().application_in(&e.a.w.consumer.tenant, "second").await;
    common::enterprise::allow_enterprise_scopes(e.h(), &app_b).await;
    let app_b_admin = credential(
        e.h(),
        &app_b,
        e.a.w.kind,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
        ],
    )
    .await;
    assert_foreign_ids_are_not_found(&e, app_b_admin.secret.expose_once(), &policy, &dept).await;

    // Another environment of the SAME application.
    let sandbox_admin = credential(
        e.h(),
        &e.a.w.consumer,
        EnvironmentKind::Sandbox,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
        ],
    )
    .await;
    assert_foreign_ids_are_not_found(&e, sandbox_admin.secret.expose_once(), &policy, &dept).await;

    // Nothing changed.
    let r = e
        .admin(
            Method::GET,
            &format!("/v1/enterprise/policies/{}", id_of(&policy, "policyId")),
            None,
        )
        .await;
    assert_eq!(r.json["name"], "Ours");
    assert_eq!(r.json["version"], 1);
    // Membership cannot pull in a foreign principal either.
    let foreign_principal = {
        let (consumer, _) = e.other_tenant("initech").await;
        let (view, _) = e
            .h()
            .platform
            .register_principal(
                consumer.env(e.a.w.kind).id,
                crate::common::NewPrincipal::External {
                    kind: calling::domain::PrincipalKind::EnterpriseMember,
                    external_reference: "outsider".to_string(),
                },
            )
            .await
            .unwrap();
        view
    };
    let r = e
        .admin(
            Method::PUT,
            &format!(
                "/v1/enterprise/departments/{}/members",
                id_of(&dept, "departmentId")
            ),
            Some(json!({ "expectedVersion": 1, "members": [{ "principalId": foreign_principal.id }] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
}

#[tokio::test]
async fn administration_needs_explicit_enterprise_scopes() {
    let Some(e) = world().await else { return };
    let dept = e.department("Ops").await;
    let id = id_of(&dept, "departmentId");
    // The ordinary calls.* credential: no enterprise administration at all.
    let calls_token = e.a.secret().to_string();
    // The enterprise caller (calls + enterprise.calls.use): still no administration.
    let caller = e.caller.secret.expose_once().to_string();
    for token in [calls_token.as_str(), caller.as_str()] {
        for (method, path, body) in [
            (Method::GET, "/v1/enterprise/departments".to_string(), None),
            (
                Method::GET,
                format!("/v1/enterprise/departments/{id}"),
                None,
            ),
            (
                Method::POST,
                "/v1/enterprise/departments".to_string(),
                Some(json!({ "name": "x" })),
            ),
            (
                Method::POST,
                "/v1/enterprise/policies".to_string(),
                Some(json!({ "name": "x", "rules": default_rules() })),
            ),
            (Method::GET, "/v1/enterprise/audit".to_string(), None),
        ] {
            let r = e.send(token, method.clone(), &path, body).await;
            assert_eq!(
                r.status,
                StatusCode::FORBIDDEN,
                "{method} {path}: {}",
                r.text
            );
            assert_eq!(r.json["error"]["code"], "INSUFFICIENT_SCOPE");
        }
    }
    // The read-only administrator reads but cannot write.
    let reader = e.reader.secret.expose_once().to_string();
    let r = e
        .send(
            &reader,
            Method::GET,
            &format!("/v1/enterprise/departments/{id}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    for (method, path, body) in [
        (
            Method::POST,
            "/v1/enterprise/departments".to_string(),
            Some(json!({ "name": "x" })),
        ),
        (
            Method::PATCH,
            format!("/v1/enterprise/departments/{id}"),
            Some(json!({ "expectedVersion": 1, "name": "y" })),
        ),
        (
            Method::DELETE,
            format!("/v1/enterprise/departments/{id}?expectedVersion=1"),
            None,
        ),
    ] {
        let r = e.send(&reader, method.clone(), &path, body).await;
        assert_eq!(
            r.status,
            StatusCode::FORBIDDEN,
            "{method} {path}: {}",
            r.text
        );
    }
    // No credential at all.
    let r = e
        .h()
        .request(Method::GET, "/v1/enterprise/departments", None, None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn suspended_tenants_and_applications_cannot_administer() {
    let Some(e) = world().await else { return };
    e.department("Before").await;
    e.h()
        .platform
        .set_application_status(e.a.w.consumer.application.id, ApplicationStatus::Suspended)
        .await
        .unwrap();
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/departments",
            Some(json!({ "name": "During" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    e.h()
        .platform
        .set_application_status(e.a.w.consumer.application.id, ApplicationStatus::Active)
        .await
        .unwrap();
    e.h()
        .platform
        .set_tenant_status(e.a.w.consumer.tenant.id, TenantStatus::Suspended)
        .await
        .unwrap();
    let r = e
        .admin(Method::GET, "/v1/enterprise/departments", None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    e.h()
        .platform
        .set_tenant_status(e.a.w.consumer.tenant.id, TenantStatus::Active)
        .await
        .unwrap();
    let r = e
        .admin(Method::GET, "/v1/enterprise/departments", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["departments"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn audit_is_append_only_and_versions_only_advance_by_one() {
    let Some(e) = world().await else { return };
    let dept = e.department("Audit").await;
    let id = id_of(&dept, "departmentId");
    let conn = e.h().conn();
    let err = common::exec(
        conn,
        "UPDATE enterprise_admin_audit SET operation = 'CREATE'",
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("append-only"), "{err}");
    assert!(common::exec(conn, "DELETE FROM enterprise_admin_audit")
        .await
        .is_err());
    assert!(common::exec(conn, "TRUNCATE enterprise_admin_audit")
        .await
        .is_err());
    // Version discipline holds even for a buggy writer.
    for sql in [
        format!("UPDATE enterprise_departments SET name = 'x' WHERE id = '{id}'"),
        format!("UPDATE enterprise_departments SET version = version + 2 WHERE id = '{id}'"),
        format!("UPDATE enterprise_departments SET version = 0 WHERE id = '{id}'"),
    ] {
        assert!(common::exec(conn, &sql).await.is_err(), "{sql}");
    }
    // Ownership columns are immutable.
    let other = e.other_tenant("hooli").await.0;
    let sql = format!(
        "UPDATE enterprise_departments SET tenant_id = '{}', version = version + 1 WHERE id = '{id}'",
        other.tenant.id
    );
    assert!(common::exec(conn, &sql).await.is_err());
    // An audit row cannot reference a service account of another scope.
    let scope = e.a.w.scope();
    let foreign_sa = other.sa(EnvironmentKind::Production);
    let sql = format!(
        "INSERT INTO enterprise_admin_audit (id, tenant_id, application_id, environment_id, actor_kind, \
         actor_service_account_id, resource_type, resource_id, operation, request_id, correlation_id, \
         details, occurred_at) VALUES (gen_random_uuid(), '{}', '{}', '{}', 'CUSTOMER_APPLICATION', '{}', \
         'DEPARTMENT', '{id}', 'UPDATE', 'r', 'c', '{{}}', now())",
        scope.tenant_id(),
        scope.application_id(),
        scope.environment_id(),
        foreign_sa
    );
    assert!(common::exec(conn, &sql).await.is_err());
    let _ = CallScope::of_platform(&e.a.w.ctx);
}

#[tokio::test]
async fn responses_conform_to_the_enterprise_contract() {
    use common::enterprise::conforms;
    let Some(e) = world().await else { return };
    let policy = e
        .create(
            "/v1/enterprise/policies",
            json!({ "name": "Contract", "isDefault": true, "rules": default_rules() }),
        )
        .await;
    conforms("CallPolicy", &policy);
    let r = e.admin(Method::GET, "/v1/enterprise/policies", None).await;
    conforms("CallPolicyList", &r.json);
    let r = e
        .admin(Method::GET, "/v1/enterprise/effective-policy", None)
        .await;
    conforms("EffectivePolicy", &r.json);
    let alice = e.a.principal("alice").await;
    let dept = e.department("Contract dept").await;
    conforms("Department", &dept);
    let r = e
        .admin(
            Method::PUT,
            &format!(
                "/v1/enterprise/departments/{}/members",
                id_of(&dept, "departmentId")
            ),
            Some(json!({ "expectedVersion": 1, "members": [{ "principalId": alice.id(), "role": "MANAGER" }] })),
        )
        .await;
    conforms("Department", &r.json);
    let r = e
        .admin(Method::GET, "/v1/enterprise/departments", None)
        .await;
    conforms("DepartmentList", &r.json);
    let r = e.admin(Method::GET, "/v1/enterprise/audit", None).await;
    assert_eq!(r.json["records"].as_array().unwrap().len(), 3);
    conforms("AuditRecordList", &r.json);
    // Request bodies the tests send conform as well.
    conforms(
        "CreatePolicyRequest",
        &json!({ "name": "x", "rules": default_rules() }),
    );
    conforms(
        "UpdateDepartmentRequest",
        &json!({ "expectedVersion": 1, "description": null }),
    );
}
