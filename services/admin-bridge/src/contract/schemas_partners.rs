//! Closed result schemas of the partner and app reads (A2.1, Developer Platform).
//! Identifiers, names, states, counts and timestamps only: no address, no credential
//! material, no webhook URL path, no manifest URL. No property name is on HQ's
//! personal-data list.

use serde_json::{json, Map, Value};

use super::model::CURSOR_PATTERN;

pub fn insert(m: &mut Map<String, Value>) {
    let uuid = json!({"type": "string", "format": "uuid"});
    let nullable_uuid = json!({"type": ["string", "null"], "format": "uuid"});
    let ts = json!({"type": "string", "format": "date-time"});
    let nullable_ts = json!({"type": ["string", "null"], "format": "date-time"});
    let code = json!({"type": "string", "pattern": "^[A-Z][A-Z0-9_]{0,63}$"});
    let nullable_code = json!({"type": ["string", "null"], "pattern": "^[A-Z][A-Z0-9_]{0,63}$"});
    let count = json!({"type": "integer", "minimum": 0});
    let nullable_int = json!({"type": ["integer", "null"]});
    let name = |max: u32| json!({"type": "string", "minLength": 1, "maxLength": max});
    let nullable_name = |max: u32| json!({"type": ["string", "null"], "maxLength": max});
    let page = |item: &str| {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["items", "next_cursor"],
            "properties": {
                "items": {"type": "array", "maxItems": 100, "items": {"$ref": format!("#/components/schemas/{item}")}},
                "next_cursor": {"type": ["string", "null"], "pattern": CURSOR_PATTERN}
            }
        })
    };
    let object = |description: &str, props: Value| {
        let required: Vec<String> = props.as_object().expect("object").keys().cloned().collect();
        json!({"description": description, "type": "object", "additionalProperties": false, "required": required, "properties": props})
    };

    let workspace = json!({
        "workspace_id": uuid, "display_name": name(120), "state": code, "state_reason": nullable_code,
        "created_at": ts, "updated_at": ts, "closed_at": nullable_ts, "version": count
    });
    m.insert(
        "PartnerWorkspace".into(),
        object(
            "One partner workspace (Developer Platform organization). No support address.",
            workspace.clone(),
        ),
    );
    m.insert("PartnerWorkspacePage".into(), page("PartnerWorkspace"));
    let mut detail = workspace.as_object().cloned().unwrap_or_default();
    detail.insert("active_members".into(), count.clone());
    detail.insert("projects".into(), count.clone());
    m.insert(
        "PartnerWorkspaceDetail".into(),
        object(
            "One partner workspace with member and project counts. No support address.",
            Value::Object(detail),
        ),
    );

    m.insert(
        "PartnerActivityEvent".into(),
        object(
            "One operator command in the workspace's activity (who, what, outcome, when). The command's free-form summary is not relayed.",
            json!({
                "event_id": uuid, "scope": {"enum": ["ORGANIZATION", "PLATFORM"]}, "workspace_id": nullable_uuid,
                "operator": name(160), "action": {"type": "string", "pattern": "^[a-z][a-z0-9_.]{0,63}$"},
                "target_type": nullable_name(64), "target_id": nullable_name(64), "outcome": code, "occurred_at": ts
            }),
        ),
    );
    m.insert("PartnerActivityPage".into(), page("PartnerActivityEvent"));

    m.insert(
        "PartnerProject".into(),
        object(
            "One project of a workspace. No description, labels or support address.",
            json!({
                "project_id": uuid, "display_name": name(120), "status": code, "status_reason": nullable_code,
                "environments": count, "created_at": ts, "updated_at": ts, "archived_at": nullable_ts, "version": count
            }),
        ),
    );
    m.insert("PartnerProjectPage".into(), page("PartnerProject"));

    m.insert(
        "PartnerEnvironment".into(),
        object(
            "One environment of a workspace's project.",
            json!({
                "environment_id": uuid, "project_id": uuid, "kind": code, "display_name": name(120), "status": code,
                "production_access_state": code, "created_at": ts, "updated_at": ts, "version": count
            }),
        ),
    );
    m.insert("PartnerEnvironmentPage".into(), page("PartnerEnvironment"));

    m.insert(
        "PartnerMember".into(),
        object(
            "One workspace membership. The member is an opaque CF Meet user id; no name or address.",
            json!({
                "membership_id": uuid, "user_id": uuid, "role": code, "status": code, "joined_via": code,
                "created_at": ts, "updated_at": ts, "removed_at": nullable_ts, "version": count
            }),
        ),
    );
    m.insert("PartnerMemberPage".into(), page("PartnerMember"));

    m.insert(
        "PartnerInvitation".into(),
        object(
            "One workspace invitation. The invitee's address is never relayed.",
            json!({"invitation_id": uuid, "role": code, "status": code, "created_at": ts, "expires_at": ts, "decided_at": nullable_ts}),
        ),
    );
    m.insert("PartnerInvitationPage".into(), page("PartnerInvitation"));

    m.insert(
        "PartnerPushKey".into(),
        object(
            "Push credential DETAILS of an environment: provider identifiers and the credential fingerprint. The credential itself never leaves the owner's vault.",
            json!({
                "push_key_id": uuid, "project_id": uuid, "environment_id": uuid, "environment_kind": code,
                "provider": {"enum": ["APNS", "APNS_VOIP", "FCM"]},
                "key_id": {"type": ["string", "null"], "pattern": "^[A-Z0-9]{10}$"},
                "team_id": {"type": ["string", "null"], "pattern": "^[A-Z0-9]{10}$"},
                "bundle_id": nullable_name(155), "apns_host": {"enum": ["SANDBOX", "PRODUCTION", null]},
                "fcm_project": nullable_name(30), "fingerprint": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
                "verification": code, "last_verified_at": nullable_ts, "last_error_code": nullable_code,
                "created_at": ts, "updated_at": ts, "state": {"enum": ["ACTIVE", "DISABLED"]},
                "disabled_at": nullable_ts, "version": {"type": "integer", "minimum": 1}
            }),
        ),
    );
    m.insert("PartnerPushKeyPage".into(), page("PartnerPushKey"));

    m.insert(
        "PartnerUsage".into(),
        object(
            "Metered usage of one environment and usage type over the window (quantities only).",
            json!({
                "project_id": uuid, "environment_id": uuid, "environment_kind": name(16),
                "usage_type": name(64), "unit": name(32), "quantity": count, "fact_count": count
            }),
        ),
    );
    let mut usage_page = page("PartnerUsage");
    let day =
        json!({"type": "string", "format": "date", "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}$"});
    usage_page["required"] = json!(["items", "next_cursor", "from", "to"]);
    usage_page["properties"]["from"] = day.clone();
    usage_page["properties"]["to"] = day;
    usage_page["description"] =
        json!("The workspace's usage over the window's UTC days [from, to].");
    m.insert("PartnerUsagePage".into(), usage_page);

    m.insert(
        "PartnerIncident".into(),
        object(
            "One incident recorded against a workspace's support profile (the reference and time the operator gave).",
            json!({
                "incident_record_id": uuid, "workspace_id": uuid,
                "incident_ref": {"type": ["string", "null"], "maxLength": 160},
                "occurred_at": nullable_ts, "recorded_at": ts
            }),
        ),
    );
    m.insert("PartnerIncidentPage".into(), page("PartnerIncident"));

    m.insert(
        "PartnerWebhook".into(),
        object(
            "One live webhook subscription: the endpoint HOST only (never the URL path or query, the signing material or the description).",
            json!({
                "subscription_id": uuid, "project_id": uuid, "environment_id": uuid, "environment_kind": code,
                "endpoint_host": {"type": ["string", "null"], "pattern": "^[a-z0-9.-]{1,253}$"},
                "event_types": {"type": "array", "maxItems": 64, "items": {"type": "string", "pattern": "^[a-z][a-z0-9_.-]{0,127}$"}},
                "status": code, "consecutive_failures": count, "last_success_at": nullable_ts, "last_failure_at": nullable_ts,
                "disabled_reason": nullable_code, "origin": code, "created_at": ts, "updated_at": ts
            }),
        ),
    );
    m.insert("PartnerWebhookPage".into(), page("PartnerWebhook"));

    m.insert(
        "PartnerWebhookDelivery".into(),
        object(
            "One webhook delivery outcome (no payload, no response body).",
            json!({
                "delivery_id": uuid, "subscription_id": uuid, "event_id": name(36), "environment_id": uuid,
                "status": code, "attempt_count": count, "last_http_status": nullable_int,
                "failure_category": nullable_code, "created_at": ts, "completed_at": nullable_ts
            }),
        ),
    );
    m.insert(
        "PartnerWebhookDeliveryPage".into(),
        page("PartnerWebhookDelivery"),
    );

    let app = json!({
        "app_id": uuid, "workspace_id": uuid, "name": nullable_name(200), "distribution": nullable_code,
        "status": code, "current_version": count, "published_version": nullable_int,
        "published_at": nullable_ts, "created_at": ts, "updated_at": ts
    });
    m.insert(
        "AppSummary".into(),
        object(
            "One CF Meet App (the current manifest's name and distribution).",
            app.clone(),
        ),
    );
    m.insert("AppPage".into(), page("AppSummary"));
    let mut app_detail = app.as_object().cloned().unwrap_or_default();
    for (k, v) in [
        ("submitted_version", nullable_int.clone()),
        ("reviewed_version", nullable_int.clone()),
        ("listed_in_directory", json!({"type": "boolean"})),
        ("developer_name", nullable_name(120)),
        (
            "review",
            json!({"type": ["object", "null"], "additionalProperties": false, "required": ["reason_code", "decided_at"],
                   "properties": {"reason_code": nullable_code, "decided_at": ts}}),
        ),
        (
            "history",
            json!({"type": "array", "maxItems": 100, "items": {
                "type": "object", "additionalProperties": false,
                "required": ["from_status", "to_status", "version", "actor_kind", "reason_code", "occurred_at"],
                "properties": {"from_status": nullable_code, "to_status": code, "version": count,
                               "actor_kind": code, "reason_code": nullable_code, "occurred_at": ts}}}),
        ),
    ] {
        app_detail.insert(k.into(), v);
    }
    m.insert(
        "AppDetail".into(),
        object(
            "One CF Meet App for review: versions, directory listing, the developer's workspace name, the last decision and the review history. Manifest URLs and test accounts are not relayed.",
            Value::Object(app_detail),
        ),
    );
    m.insert(
        "AppVersion".into(),
        object("One manifest version of an app (name and distribution only).", json!({"version": count, "name": nullable_name(200), "distribution": nullable_code, "created_at": ts})),
    );
    m.insert("AppVersionPage".into(), page("AppVersion"));
    m.insert(
        "AppInstall".into(),
        object(
            "One installation grant of an app. Targets are opaque ids; scopes are counted, not listed.",
            json!({
                "grant_id": uuid, "target_kind": code, "organization_id": nullable_uuid, "principal_id": nullable_uuid,
                "scope_count": count, "status": code, "revoked_reason": nullable_code, "installed_at": ts, "updated_at": ts
            }),
        ),
    );
    m.insert("AppInstallPage".into(), page("AppInstall"));
}
