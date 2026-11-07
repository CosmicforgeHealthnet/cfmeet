//! Blueprint v1.0 Section 16 (INV-002) — the delegated mirror of the customer surface.
//!
//! `/internal/v1/delegated/v1/...` serves every `/v1/messaging/**` and
//! `/v1/conversations/**` operation of `cfmeet.messaging.http-api` again for first-party
//! backends acting through a peer service (`messaging-application.delegate`,
//! `services/messaging/src/http/mod.rs`). Most mirror paths were never named by a test.
//! For EVERY mirrored operation, against the real router and PostgreSQL with real
//! service-token verification, this suite proves:
//!
//! 1. no service token → `401 INTERNAL_AUTHENTICATION_FAILED`;
//! 2. a trusted peer without `messaging-application.delegate` → `403 INTERNAL_PERMISSION_DENIED`;
//! 3. the Meeting Service acting for an EXTERNAL (not first-party) environment →
//!    `403 DELEGATED_CONTEXT_INVALID`;
//! 4. refusals 1–3 leave every table unchanged;
//! 5. the Meeting Service acting for its first-party environment reaches the operation's
//!    handler: never the router fallback ("No such route.") and never `405`.
//!
//! The table below is checked against the contract, so a new customer operation fails
//! here until its mirror path is listed. Ids are random (nothing is found) and every JSON
//! body is `[]` (no handler accepts it), so step 5 never creates anything.

mod common;

use std::collections::BTreeSet;

use axum::http::{Method, StatusCode};
use common::*;
use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const PREFIX: &str = "/internal/v1/delegated";

/// Every mirrored operation: `(method, path)` with the contract's parameter names.
const MIRROR: &[(&str, &str)] = &[
    ("POST", "/internal/v1/delegated/v1/messaging/devices/registration-challenges"),
    ("POST", "/internal/v1/delegated/v1/messaging/devices"),
    ("POST", "/internal/v1/delegated/v1/messaging/devices/{deviceId}/key-packages"),
    ("POST", "/internal/v1/delegated/v1/messaging/devices/{deviceId}/revoke"),
    ("GET", "/internal/v1/delegated/v1/messaging/principals/{principalId}/devices"),
    ("GET", "/internal/v1/delegated/v1/messaging/key-transparency/proofs"),
    ("GET", "/internal/v1/delegated/v1/conversations"),
    ("POST", "/internal/v1/delegated/v1/conversations"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}"),
    ("PATCH", "/internal/v1/delegated/v1/conversations/{conversationId}"),
    ("GET", "/internal/v1/delegated/v1/messaging/policy"),
    ("PUT", "/internal/v1/delegated/v1/messaging/policy"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/members"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/members"),
    ("DELETE", "/internal/v1/delegated/v1/conversations/{conversationId}/members/{principalId}"),
    ("PATCH", "/internal/v1/delegated/v1/conversations/{conversationId}/members/{principalId}"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/messages"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/messages"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/messages/{messageId}"),
    ("DELETE", "/internal/v1/delegated/v1/conversations/{conversationId}/messages/{messageId}"),
    ("PATCH", "/internal/v1/delegated/v1/conversations/{conversationId}/messages/{messageId}"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/messages/{messageId}/revisions"),
    ("PUT", "/internal/v1/delegated/v1/conversations/{conversationId}/read-cursor"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/events"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/mls/commits"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/mls/state"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/mls/key-package-claims"),
    ("GET", "/internal/v1/delegated/v1/messaging/devices/{deviceId}/welcomes"),
    ("POST", "/internal/v1/delegated/v1/messaging/devices/{deviceId}/welcomes/{welcomeId}/consume"),
    ("GET", "/internal/v1/delegated/v1/messaging/blocks"),
    ("POST", "/internal/v1/delegated/v1/messaging/blocks"),
    ("DELETE", "/internal/v1/delegated/v1/messaging/blocks/{principalId}"),
    ("POST", "/internal/v1/delegated/v1/messaging/client-tokens"),
    ("POST", "/internal/v1/delegated/v1/messaging/client-sessions/{sessionId}/revoke"),
    ("GET", "/internal/v1/delegated/v1/messaging/presence/settings"),
    ("PUT", "/internal/v1/delegated/v1/messaging/presence/settings"),
    ("PUT", "/internal/v1/delegated/v1/messaging/devices/{deviceId}/push-registration"),
    ("DELETE", "/internal/v1/delegated/v1/messaging/devices/{deviceId}/push-registration"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/attachments"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/attachments/{attachmentId}"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/attachments/{attachmentId}/finalize"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/attachments/{attachmentId}/download-grants"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/attachments/{attachmentId}/content"),
    ("GET", "/internal/v1/delegated/v1/messaging/legal-holds"),
    ("POST", "/internal/v1/delegated/v1/messaging/legal-holds"),
    ("GET", "/internal/v1/delegated/v1/messaging/legal-holds/{holdId}"),
    ("POST", "/internal/v1/delegated/v1/messaging/legal-holds/{holdId}/release"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/search"),
    ("GET", "/internal/v1/delegated/v1/messaging/search"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/reports"),
    ("GET", "/internal/v1/delegated/v1/messaging/reports"),
    ("GET", "/internal/v1/delegated/v1/messaging/reports/{reportId}"),
    ("GET", "/internal/v1/delegated/v1/messaging/moderation/actions"),
    ("POST", "/internal/v1/delegated/v1/messaging/moderation/actions"),
    ("POST", "/internal/v1/delegated/v1/messaging/exports"),
    ("GET", "/internal/v1/delegated/v1/messaging/exports/{exportId}"),
    ("POST", "/internal/v1/delegated/v1/messaging/exports/{exportId}/download-grants"),
    ("POST", "/internal/v1/delegated/v1/messaging/deletion-requests"),
    ("GET", "/internal/v1/delegated/v1/messaging/deletion-requests/{requestId}"),
    ("GET", "/internal/v1/delegated/v1/messaging/audit"),
    ("POST", "/internal/v1/delegated/v1/messaging/client-sessions/revoke-all"),
    ("GET", "/internal/v1/delegated/v1/messaging/sms-bridge/disclosure"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/sms-bridges"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/sms-bridges"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/sms-bridges/{bridgeId}/close"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/messages/{messageId}/sms-transport"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/bans"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/bans"),
    ("DELETE", "/internal/v1/delegated/v1/conversations/{conversationId}/bans/{principalId}"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/messages/{messageId}/reactions"),
    ("PUT", "/internal/v1/delegated/v1/conversations/{conversationId}/messages/{messageId}/reactions/{reaction}"),
    ("DELETE", "/internal/v1/delegated/v1/conversations/{conversationId}/messages/{messageId}/reactions/{reaction}"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/pins"),
    ("PUT", "/internal/v1/delegated/v1/conversations/{conversationId}/pins/{messageId}"),
    ("DELETE", "/internal/v1/delegated/v1/conversations/{conversationId}/pins/{messageId}"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/unread"),
    ("GET", "/internal/v1/delegated/v1/messaging/usage"),
    ("GET", "/internal/v1/delegated/v1/messaging/history-transfers"),
    ("POST", "/internal/v1/delegated/v1/messaging/history-transfers"),
    ("GET", "/internal/v1/delegated/v1/messaging/history-transfers/{transferId}"),
    ("GET", "/internal/v1/delegated/v1/messaging/history-transfers/{transferId}/sealed-history"),
    ("PUT", "/internal/v1/delegated/v1/messaging/history-transfers/{transferId}/sealed-history"),
    ("POST", "/internal/v1/delegated/v1/messaging/history-transfers/{transferId}/complete"),
    ("POST", "/internal/v1/delegated/v1/messaging/history-transfers/{transferId}/cancel"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/attachments/{attachmentId}/thumbnail-grants"),
    ("POST", "/internal/v1/delegated/v1/messaging/backups"),
    ("GET", "/internal/v1/delegated/v1/messaging/backups/latest"),
    ("DELETE", "/internal/v1/delegated/v1/messaging/backups/{backupId}"),
    ("POST", "/internal/v1/delegated/v1/messaging/backups/{backupId}/finalize"),
    ("POST", "/internal/v1/delegated/v1/messaging/backups/{backupId}/download-grants"),
    ("GET", "/internal/v1/delegated/v1/conversations/{conversationId}/messages/{messageId}/thread"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/invitation/accept"),
    ("POST", "/internal/v1/delegated/v1/conversations/{conversationId}/invitation/decline"),
    ("GET", "/internal/v1/delegated/v1/messaging/invitations"),
    ("POST", "/internal/v1/delegated/v1/messaging/session-devices"),
    ("POST", "/internal/v1/delegated/v1/messaging/session-devices/revoke"),
];

/// The contract's customer operations that the delegated router mirrors.
fn mirrored_contract_operations() -> BTreeSet<(String, String)> {
    let doc: Value = serde_json::from_str(messaging::http::OPENAPI).unwrap();
    let mut out = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        if !(path.starts_with("/v1/messaging/") || path.starts_with("/v1/conversations")) {
            continue;
        }
        for method in ["get", "put", "post", "delete", "patch"] {
            if item.get(method).is_some() {
                out.insert((method.to_ascii_uppercase(), format!("{PREFIX}{path}")));
            }
        }
    }
    out
}

/// Concrete path: every `{param}` becomes a fresh id (a reaction gets a valid token).
fn concrete(template: &str) -> String {
    template
        .split('/')
        .map(|seg| match seg {
            "{reaction}" => "thumbs_up".to_string(),
            s if s.starts_with('{') && s.ends_with('}') => Uuid::new_v4().to_string(),
            s => s.to_string(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn body_for(method: &Method) -> Option<Value> {
    // `[]` satisfies no handler's JSON body, so nothing is ever created; operations that
    // take no body ignore it.
    (*method != Method::GET && *method != Method::DELETE).then(|| json!([]))
}

/// Row-level fingerprint of every table (rate windows excluded: a refused request still
/// counts against its window by design).
async fn fingerprint(h: &Harness) -> Vec<(String, String)> {
    let tables = h
        .conn()
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public' \
             AND table_type = 'BASE TABLE' AND table_name <> 'messaging_rate_windows' ORDER BY 1"
                .to_string(),
        ))
        .await
        .unwrap();
    let mut out = Vec::new();
    for t in tables {
        let t: String = t.try_get_by_index(0).unwrap();
        let r = h
            .conn()
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!(
                    "SELECT count(*)::text || ':' || coalesce(md5(string_agg(x::text, '|' ORDER BY x::text)), '') \
                     FROM (SELECT to_jsonb(t) AS x FROM {t} t) s"
                ),
            ))
            .await
            .unwrap()
            .unwrap();
        out.push((t, r.try_get_by_index::<String>(0).unwrap()));
    }
    out
}

fn is_router_fallback(r: &Response) -> bool {
    r.status == StatusCode::NOT_FOUND
        && r.json["error"]["message"].as_str() == Some(messaging::http::NO_ROUTE_MESSAGE)
}

#[test]
fn the_mirror_table_lists_exactly_the_mirrored_contract_operations() {
    let table: BTreeSet<(String, String)> = MIRROR
        .iter()
        .map(|(m, p)| (m.to_string(), p.to_string()))
        .collect();
    assert_eq!(table.len(), MIRROR.len(), "duplicate rows in MIRROR");
    let contract = mirrored_contract_operations();
    let missing: Vec<_> = contract.difference(&table).collect();
    let extra: Vec<_> = table.difference(&contract).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "MIRROR drifted from cfmeet.messaging.http-api: missing {missing:?}, not in the contract {extra:?}"
    );
}

#[tokio::test]
async fn every_mirrored_operation_is_guarded_and_routed_to_its_handler() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let peers = peers();
    let first_party = h.first_party(true);
    let external = h.first_party(false);
    let acting = h.principal_of_kind(&first_party, "NATIVE_CF_USER");
    let acting_external = h.principal_of_kind(&external, "NATIVE_CF_USER");
    let acting_header = acting.to_string();
    let acting_external_header = acting_external.to_string();

    // The detector itself: an unknown mirror path IS the router fallback.
    let r = h
        .call_internal(
            &peers.meeting,
            Method::GET,
            "/internal/v1/delegated/v1/messaging/no-such-operation",
            None,
            Some(first_party.context.scope.delegated()),
            &[("cf-acting-principal", &acting_header)],
        )
        .await;
    assert!(is_router_fallback(&r), "{} {}", r.status, r.text);

    let before = fingerprint(&h).await;
    for (method, template) in MIRROR {
        let method = Method::from_bytes(method.as_bytes()).unwrap();
        let path = concrete(template);

        // 1. No service token.
        let r = h
            .send(
                method.clone(),
                None,
                Some(acting),
                &path,
                body_for(&method),
                &[],
            )
            .await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::UNAUTHORIZED, "INTERNAL_AUTHENTICATION_FAILED"),
            "{method} {template}: {}",
            r.text
        );

        // 2. A trusted peer that may not delegate (Privacy Relay holds only its own
        //    context-authority permission).
        let r = h
            .call_internal(
                &peers.privacy_relay,
                method.clone(),
                &path,
                body_for(&method),
                Some(first_party.context.scope.delegated()),
                &[("cf-acting-principal", &acting_header)],
            )
            .await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED"),
            "{method} {template}: {}",
            r.text
        );

        // 3. The Meeting Service acting for an external customer's environment.
        let r = h
            .call_internal(
                &peers.meeting,
                method.clone(),
                &path,
                body_for(&method),
                Some(external.context.scope.delegated()),
                &[("cf-acting-principal", &acting_external_header)],
            )
            .await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::FORBIDDEN, "DELEGATED_CONTEXT_INVALID"),
            "{method} {template}: {}",
            r.text
        );
    }
    // 4. Refusals change nothing.
    assert_eq!(
        before,
        fingerprint(&h).await,
        "refused mirror calls changed state"
    );

    // 5. The first-party delegated call reaches the operation's own handler.
    for (method, template) in MIRROR {
        let method = Method::from_bytes(method.as_bytes()).unwrap();
        let path = concrete(template);
        let r = h
            .call_internal(
                &peers.meeting,
                method.clone(),
                &path,
                body_for(&method),
                Some(first_party.context.scope.delegated()),
                &[("cf-acting-principal", &acting_header)],
            )
            .await;
        assert!(
            !is_router_fallback(&r),
            "{method} {template}: the mirror does not route this operation ({})",
            r.text
        );
        assert_ne!(
            r.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {template}: {}",
            r.text
        );
        println!(
            "MIRROR {method} {template} -> {} {}",
            r.status.as_u16(),
            r.code()
        );
    }
}
