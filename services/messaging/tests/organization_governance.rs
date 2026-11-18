//! Gap audit G6 / G7 — organization governance in an organization's own first-party
//! Messaging scope, relayed by the Meeting Service (`messaging-organization.govern`):
//!
//! * organization moderation reuses the moderation handlers under
//!   `/internal/v1/organization-moderation/v1/messaging/...`: the signed initiating
//!   principal is the moderator (required, never an acting member), recorded on report
//!   views, actions and audit entries; nothing outside the organization's scope is seen;
//! * team channels (`PUT /internal/v1/team-channels`) are created and resynchronized from
//!   a team snapshot, version-fenced; only team-sourced members are removed.

mod common;

use axum::http::{Method, StatusCode};
use cf_service_auth::DelegatedPrincipalKind;
use common::*;
use messaging::identity::PrincipalStatus;
use serde_json::{json, Value};
use uuid::Uuid;

const MOD: &str = "/internal/v1/organization-moderation/v1/messaging";

impl Harness {
    async fn as_org_moderator(
        &self,
        c: &Customer,
        moderator: Option<Uuid>,
        method: Method,
        path: &str,
        body: Option<Value>,
        extra: &[(&str, &str)],
    ) -> Response {
        let mut d = c.context.scope.delegated();
        if let Some(m) = moderator {
            d = d.with_initiating_principal(m, DelegatedPrincipalKind::NativeCfUser);
        }
        self.call_internal(&peers().meeting, method, path, body, Some(d), extra)
            .await
    }

    async fn team_sync(&self, c: &Customer, body: Value) -> Response {
        self.call_internal(
            &peers().meeting,
            Method::PUT,
            "/internal/v1/team-channels",
            Some(body),
            Some(c.context.scope.delegated()),
            &[],
        )
        .await
    }
}

#[tokio::test]
async fn organization_moderators_use_the_moderation_module_attributed_and_scoped() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let org = h.first_party(true);
    let (conv, people) = h.managed_group(&org, 3).await;
    let (alice, bob) = (people[0], people[1]);
    let id = conv["conversationId"].as_str().unwrap().to_string();
    let sent = h.send_managed(&org, &conv, bob, "spam spam").await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    let mid = sent.json["messageId"].as_str().unwrap().to_string();
    let r = h
        .post(
            &org,
            Some(alice),
            &format!("/v1/conversations/{id}/reports"),
            json!({ "category": "SPAM", "messageIds": [mid] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let rid = r.json["reportId"].as_str().unwrap().to_string();
    let moderator = h.principal_of_kind(&org, "NATIVE_CF_USER");

    // The queue of THIS organization.
    let q = h
        .as_org_moderator(
            &org,
            Some(moderator),
            Method::GET,
            &format!("{MOD}/reports?status=OPEN"),
            None,
            &[],
        )
        .await;
    assert_eq!(q.status, StatusCode::OK, "{}", q.text);
    assert!(q.text.contains(&rid), "{}", q.text);
    let v = h
        .as_org_moderator(
            &org,
            Some(moderator),
            Method::GET,
            &format!("{MOD}/reports/{rid}"),
            None,
            &[],
        )
        .await;
    assert_eq!(v.status, StatusCode::OK, "{}", v.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/ReportDetail",
        &v.json,
    );

    // Remove the message and restrict the member: attributed to the moderator.
    let a = h
        .as_org_moderator(
            &org,
            Some(moderator),
            Method::POST,
            &format!("{MOD}/moderation/actions"),
            Some(json!({ "kind": "REMOVE_MESSAGE", "conversationId": id, "messageId": mid, "reportId": rid, "reason": "SPAM" })),
            &[],
        )
        .await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    assert_eq!(a.json["moderatorPrincipalId"], json!(moderator));
    conforms(
        "messaging/common/v1.schema.json#/$defs/ModerationAction",
        &a.json,
    );
    let a = h
        .as_org_moderator(
            &org,
            Some(moderator),
            Method::POST,
            &format!("{MOD}/moderation/actions"),
            Some(json!({ "kind": "MUTE_MEMBER", "conversationId": id, "principalId": bob, "reason": "SPAM", "durationSeconds": 3600 })),
            &[],
        )
        .await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    assert_eq!(
        h.send_managed(&org, &conv, bob, "more").await.code(),
        "SEND_RESTRICTED"
    );
    let audited = h
        .scalar(&format!(
            "SELECT count(*) FROM messaging_audit WHERE principal_id = '{moderator}' \
               AND action IN ('MODERATION_ACTION', 'REPORT_VIEWED')"
        ))
        .await;
    assert_eq!(audited, 3);
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_moderation_actions WHERE moderator_principal_id = '{moderator}'"
        ))
        .await,
        2
    );
    let list = h
        .as_org_moderator(
            &org,
            Some(moderator),
            Method::GET,
            &format!("{MOD}/moderation/actions"),
            None,
            &[],
        )
        .await;
    assert_eq!(
        list.json["actions"].as_array().unwrap().len(),
        2,
        "{}",
        list.text
    );

    // Another organization's moderator sees nothing of this one.
    let other = h.first_party(true);
    let theirs = h.principal_of_kind(&other, "NATIVE_CF_USER");
    let q = h
        .as_org_moderator(
            &other,
            Some(theirs),
            Method::GET,
            &format!("{MOD}/reports"),
            None,
            &[],
        )
        .await;
    assert_eq!(q.status, StatusCode::OK, "{}", q.text);
    assert!(!q.text.contains(&rid));
    let v = h
        .as_org_moderator(
            &other,
            Some(theirs),
            Method::GET,
            &format!("{MOD}/reports/{rid}"),
            None,
            &[],
        )
        .await;
    assert_eq!(v.status, StatusCode::NOT_FOUND, "{}", v.text);

    // Refusals: no signed moderator, an acting header, an inactive moderator, an external
    // (non first-party) scope, and the ordinary delegated mirror (never moderation).
    let r = h
        .as_org_moderator(
            &org,
            None,
            Method::GET,
            &format!("{MOD}/reports"),
            None,
            &[],
        )
        .await;
    assert_eq!(r.reason(), "MODERATOR_REQUIRED", "{}", r.text);
    let a = alice.to_string();
    let r = h
        .as_org_moderator(
            &org,
            Some(moderator),
            Method::GET,
            &format!("{MOD}/reports"),
            None,
            &[("cf-acting-principal", a.as_str())],
        )
        .await;
    assert_eq!(r.reason(), "ADMINISTRATION_ONLY", "{}", r.text);
    let gone = h.principal_of_kind(&org, "NATIVE_CF_USER");
    h.set_principal(&org, gone, PrincipalStatus::Inactive);
    let r = h
        .as_org_moderator(
            &org,
            Some(gone),
            Method::GET,
            &format!("{MOD}/reports"),
            None,
            &[],
        )
        .await;
    assert_eq!(r.code(), "PRINCIPAL_NOT_AUTHORIZED", "{}", r.text);
    let external = h.first_party(false);
    let ext = h.principal_of_kind(&external, "NATIVE_CF_USER");
    let r = h
        .as_org_moderator(
            &external,
            Some(ext),
            Method::GET,
            &format!("{MOD}/reports"),
            None,
            &[],
        )
        .await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_INVALID", "{}", r.text);
    let r = h
        .as_org_moderator(
            &org,
            Some(moderator),
            Method::GET,
            "/internal/v1/delegated/v1/messaging/reports",
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // Another peer cannot use the organization surface.
    let r = h
        .call_internal(
            &peers().privacy_relay,
            Method::GET,
            &format!("{MOD}/reports"),
            None,
            Some(
                org.context
                    .scope
                    .delegated()
                    .with_initiating_principal(moderator, DelegatedPrincipalKind::NativeCfUser),
            ),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
}

#[tokio::test]
async fn team_channels_follow_team_snapshots_version_fenced() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let org = h.first_party(true);
    h.allow_managed(&org, "GROUP").await;
    let owner = h.principal_of_kind(&org, "NATIVE_CF_USER");
    let (a, b, c) = (
        h.principal_of_kind(&org, "NATIVE_CF_USER"),
        h.principal_of_kind(&org, "NATIVE_CF_USER"),
        h.principal_of_kind(&org, "NATIVE_CF_USER"),
    );
    let team_ref = format!("organization:{}:team:{}", Uuid::new_v4(), Uuid::new_v4());
    let snap = |members: Vec<Uuid>, v: i64| {
        json!({ "teamRef": team_ref, "ownerPrincipalId": owner, "title": "Design team",
                "memberPrincipalIds": members, "sourceVersion": v })
    };
    let r = h.team_sync(&org, snap(vec![owner, a, b], 10)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["created"], true);
    assert_eq!(r.json["applied"], true);
    let conv = r.json["conversation"].clone();
    conforms("messaging/common/v1.schema.json#/$defs/Conversation", &conv);
    assert_eq!(conv["contextClass"], "GROUP");
    assert_eq!(conv["securityProfile"], "MANAGED");
    assert_eq!(conv["context"]["namespace"], "organization-team");
    assert_eq!(r.json["added"].as_array().unwrap().len(), 2, "{}", r.text);
    let id = conv["conversationId"].as_str().unwrap().to_string();

    // An admin adds a guest by hand; the guest is never removed by the sync.
    let guest = h.principal_of_kind(&org, "NATIVE_CF_USER");
    let g = h
        .post(
            &org,
            Some(owner),
            &format!("/v1/conversations/{id}/members"),
            json!({ "principalId": guest }),
        )
        .await;
    assert_eq!(g.status, StatusCode::CREATED, "{}", g.text);

    // b leaves the team, c joins.
    let r = h.team_sync(&org, snap(vec![owner, a, c], 20)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["created"], false);
    assert_eq!(r.json["added"], json!([c]));
    assert_eq!(r.json["removed"], json!([b]));
    // A delayed older snapshot cannot bring b back.
    let r = h.team_sync(&org, snap(vec![owner, a, b, c], 15)).await;
    assert_eq!(
        (r.json["applied"].clone(), r.json["reason"].clone()),
        (json!(false), json!("STALE_VERSION")),
        "{}",
        r.text
    );
    let active = |p: Uuid| {
        format!(
            "SELECT count(*) FROM messaging_conversation_members WHERE conversation_id = '{id}' \
               AND principal_id = '{p}' AND state = 'ACTIVE'"
        )
    };
    assert_eq!(h.scalar(&active(b)).await, 0);
    assert_eq!(h.scalar(&active(c)).await, 1);
    assert_eq!(h.scalar(&active(guest)).await, 1);
    assert_eq!(h.scalar(&active(owner)).await, 1);
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_conversation_members WHERE conversation_id = '{id}' \
               AND authority_source = 'ORGANIZATION_TEAM' AND state = 'ACTIVE'"
        ))
        .await,
        2
    );
    // An inactive principal is never added (and is reported).
    h.set_principal(&org, a, PrincipalStatus::Inactive);
    let r = h.team_sync(&org, snap(vec![owner, a, c], 30)).await;
    assert_eq!(r.json["removed"], json!([a]), "{}", r.text);
    assert_eq!(r.json["skipped"][0]["reason"], "PRINCIPAL_INACTIVE");

    // Customers cannot create a team channel themselves in a first-party environment.
    let r = h
        .create_conversation(
            &org,
            owner,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED",
                    "context": { "namespace": "organization-team", "reference": "x", "version": 1 } }),
        )
        .await;
    assert_eq!(r.reason(), "CONTEXT_NAMESPACE_RESERVED", "{}", r.text);
    // Another peer cannot sync, and an external scope is refused.
    let r = h
        .call_internal(
            &peers().privacy_relay,
            Method::PUT,
            "/internal/v1/team-channels",
            Some(snap(vec![owner], 40)),
            Some(org.context.scope.delegated()),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let external = h.first_party(false);
    let r = h.team_sync(&external, snap(vec![owner], 50)).await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_INVALID", "{}", r.text);
}
