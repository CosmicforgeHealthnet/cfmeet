//! Bridges (Part E §44–§47, §79, §91, §98, §108).
//!
//! A bridge anchors media at the SIP / media edge between:
//!
//! * two public legs (phone-to-phone relay, callback relay) — `PUBLIC_LEGS`; or
//! * a public leg and the Call's application RTC media — `APPLICATION_MEDIA`: Telephony
//!   issues an opaque, single-use MEDIA JOIN CODE; Calling's media provider adapter
//!   attaches the RTC side to the edge with it (no SIP type ever reaches Calling, and the
//!   app participant never sees the private destination).
//!
//! A bridge is ACTIVE only on edge evidence that every member joined; then its public
//! legs are CONNECTED and BRIDGE_ESTABLISHED / LEG_CONNECTED are emitted (Calling's
//! CONNECTED needs this evidence). A member leaving ends the bridge (BRIDGE_ENDED);
//! Calling orchestrates what happens to the other legs. At most one live bridge per Call.

use chrono::{DateTime, Duration, Utc};
use rand::{distributions::Alphanumeric, Rng};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    commands::{executor::Executed, find_by_key, CommandRow, Requester},
    db,
    domain::{CommandKind, LegStatus, NormalizedEvent},
    error::{is_unique_violation, ErrorCode, TelephonyError, TelephonyResult},
    events::outbox,
    legs::{self, ts, Change},
    service::TelephonyService,
    sip::EdgeError,
};

/// How long an application-media join may take before the bridge fails.
pub const APP_MEDIA_JOIN_TIMEOUT_SECONDS: i64 = 30;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeCommand {
    pub schema_version: String,
    pub command_id: Uuid,
    pub idempotency_key: String,
    pub correlation_id: String,
    pub call_id: Uuid,
    pub kind: String,
    pub telephony_leg_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct BridgeRow {
    pub id: Uuid,
    pub calling_call_id: Uuid,
    pub kind: String,
    pub status: String,
    pub media_join_code: Option<String>,
    pub edge_bridge_ref: Option<String>,
    pub created_at: DateTime<Utc>,
    pub established_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub end_reason: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct MemberRow {
    pub id: Uuid,
    pub member_kind: String,
    pub telephony_leg_id: Option<Uuid>,
    pub edge_channel_ref: Option<String>,
    pub status: String,
}

const BRIDGE_COLUMNS: &str =
    "id, calling_call_id, kind, status, media_join_code, edge_bridge_ref, \
    created_at, established_at, ended_at, end_reason, version";

fn join_code() -> String {
    let random: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(|c| (c as char).to_ascii_lowercase())
        .collect();
    format!("cfmj{random}")
}

impl TelephonyService {
    pub(crate) async fn load_bridge<C: ConnectionTrait>(
        &self,
        db_: &C,
        id: Uuid,
        lock: bool,
    ) -> TelephonyResult<BridgeRow> {
        db::one(
            db_,
            &format!(
                "SELECT {BRIDGE_COLUMNS} FROM telephony_bridges WHERE id = $1{}",
                if lock { " FOR UPDATE" } else { "" }
            ),
            vec![id.into()],
        )
        .await?
        .ok_or(TelephonyError::Refused(ErrorCode::BridgeNotFound))
    }

    pub(crate) async fn members<C: ConnectionTrait>(
        &self,
        db_: &C,
        bridge: Uuid,
    ) -> TelephonyResult<Vec<MemberRow>> {
        db::all(
            db_,
            "SELECT id, member_kind, telephony_leg_id, edge_channel_ref, status FROM telephony_bridge_legs \
             WHERE bridge_id = $1 ORDER BY created_at, id",
            vec![bridge.into()],
        )
        .await
    }

    /// `GET /internal/v1/bridges/{id}`.
    pub async fn bridge_view(&self, id: Uuid) -> TelephonyResult<Value> {
        let bridge = self.load_bridge(&self.db, id, false).await?;
        let members = self.members(&self.db, id).await?;
        Ok(json!({ "bridge": view(&bridge, &members, false) }))
    }

    /// `POST /internal/v1/bridges`.
    pub async fn create_bridge(
        &self,
        requester: &Requester,
        command: BridgeCommand,
    ) -> TelephonyResult<(u16, Value)> {
        if command.schema_version != "1" {
            return Err(TelephonyError::Validation("unsupported schemaVersion"));
        }
        if !self.flags.telephony.enabled() {
            return Err(TelephonyError::Refused(ErrorCode::TelephonyDisabled));
        }
        let app_media = match command.kind.as_str() {
            "APPLICATION_MEDIA" => true,
            "PUBLIC_LEGS" => false,
            _ => {
                return Err(TelephonyError::Validation(
                    "kind must be PUBLIC_LEGS or APPLICATION_MEDIA",
                ))
            }
        };
        let mut legs_sorted = command.telephony_leg_ids.clone();
        legs_sorted.sort();
        legs_sorted.dedup();
        let expected = if app_media { 1 } else { 2 };
        if legs_sorted.len() != command.telephony_leg_ids.len() || legs_sorted.len() != expected {
            return Err(TelephonyError::Validation(
                "APPLICATION_MEDIA bridges one leg, PUBLIC_LEGS bridges two distinct legs",
            ));
        }
        let digest = {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(
                format!(
                    "BRIDGE|{}|{}|{:?}",
                    command.call_id, command.kind, legs_sorted
                )
                .as_bytes(),
            ))
        };
        if let Some(existing) =
            find_by_key(&self.db, &requester.service, &command.idempotency_key).await?
        {
            if existing.request_digest != digest {
                return Err(TelephonyError::Refused(ErrorCode::IdempotencyConflict));
            }
            let id = existing
                .bridge_id
                .ok_or(TelephonyError::Refused(ErrorCode::BridgeNotFound))?;
            let bridge = self.load_bridge(&self.db, id, false).await?;
            let members = self.members(&self.db, id).await?;
            return Ok((
                200,
                json!({ "bridge": view(&bridge, &members, true), "replayed": true }),
            ));
        }
        let now = self.now();
        let txn = self.db.begin().await?;
        let mut scope = None;
        for id in &legs_sorted {
            let leg = legs::lock(&txn, *id).await?;
            if leg.calling_call_id != command.call_id {
                return Err(TelephonyError::Refused(ErrorCode::LegNotFound));
            }
            if leg.status()?.is_terminal() || leg.status()? == LegStatus::Unknown {
                return Err(TelephonyError::Refused(ErrorCode::LegNotActive));
            }
            scope.get_or_insert((leg.tenant_id, leg.application_id, leg.environment_id));
        }
        let (tenant, application, environment) =
            scope.ok_or(TelephonyError::Refused(ErrorCode::LegNotFound))?;
        let bridge_id = Uuid::new_v4();
        let code = app_media.then(join_code);
        let inserted = db::exec(
            &txn,
            "INSERT INTO telephony_bridges (id, calling_call_id, tenant_id, application_id, environment_id, kind, \
               status, media_join_code, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, 'PENDING', $7, $8, $8)",
            vec![
                bridge_id.into(),
                command.call_id.into(),
                tenant.into(),
                application.into(),
                environment.into(),
                command.kind.clone().into(),
                code.clone().into(),
                now.into(),
            ],
        )
        .await;
        if let Err(e) = inserted {
            if is_unique_violation(&e, "uq_telephony_bridges_one_live_per_call") {
                return Err(TelephonyError::Refused(ErrorCode::BridgeConflict));
            }
            return Err(e);
        }
        for id in &legs_sorted {
            db::exec(
                &txn,
                "INSERT INTO telephony_bridge_legs (id, bridge_id, member_kind, telephony_leg_id, status, created_at, updated_at) \
                 VALUES ($1, $2, 'PUBLIC_LEG', $3, 'EXPECTED', $4, $4)",
                vec![Uuid::new_v4().into(), bridge_id.into(), (*id).into(), now.into()],
            )
            .await?;
        }
        if app_media {
            db::exec(
                &txn,
                "INSERT INTO telephony_bridge_legs (id, bridge_id, member_kind, status, created_at, updated_at) \
                 VALUES ($1, $2, 'APPLICATION_MEDIA', 'EXPECTED', $3, $3)",
                vec![Uuid::new_v4().into(), bridge_id.into(), now.into()],
            )
            .await?;
        }
        db::exec(
            &txn,
            "INSERT INTO telephony_commands (id, kind, bridge_id, requester_service, idempotency_key, request_digest, \
               status, max_attempts, next_attempt_at, payload, request_id, correlation_id, created_at, updated_at) \
             VALUES ($1, 'BRIDGE', $2, $3, $4, $5, 'PENDING', 20, $6, $7, $8, $9, $6, $6)",
            vec![
                command.command_id.into(),
                bridge_id.into(),
                requester.service.clone().into(),
                command.idempotency_key.clone().into(),
                digest.into(),
                now.into(),
                json!({ "schemaVersion": "1", "kind": command.kind }).into(),
                requester.request_id.clone().into(),
                command.correlation_id.clone().into(),
            ],
        )
        .await?;
        txn.commit().await?;
        let bridge = self.load_bridge(&self.db, bridge_id, false).await?;
        let members = self.members(&self.db, bridge_id).await?;
        Ok((202, json!({ "bridge": view(&bridge, &members, true) })))
    }

    /// `DELETE /internal/v1/bridges/{id}`: UNBRIDGE (idempotent).
    pub async fn end_bridge(
        &self,
        requester: &Requester,
        id: Uuid,
    ) -> TelephonyResult<(u16, Value)> {
        let now = self.now();
        let bridge = self.load_bridge(&self.db, id, false).await?;
        if matches!(bridge.status.as_str(), "ENDED" | "FAILED") {
            let members = self.members(&self.db, id).await?;
            return Ok((200, json!({ "bridge": view(&bridge, &members, false) })));
        }
        let key = format!("unbridge-{id}");
        if find_by_key(&self.db, &requester.service, &key)
            .await?
            .is_none()
        {
            let inserted = db::exec(
                &self.db,
                "INSERT INTO telephony_commands (id, kind, bridge_id, requester_service, idempotency_key, request_digest, \
                   status, max_attempts, next_attempt_at, payload, request_id, correlation_id, created_at, updated_at) \
                 VALUES ($1, 'UNBRIDGE', $2, $3, $4, $5, 'PENDING', 10, $6, $7, $8, $9, $6, $6)",
                vec![
                    Uuid::new_v4().into(),
                    id.into(),
                    requester.service.clone().into(),
                    key.clone().into(),
                    format!("{:0>64}", "0").into(),
                    now.into(),
                    json!({ "schemaVersion": "1" }).into(),
                    requester.request_id.clone().into(),
                    requester.correlation_id.clone().into(),
                ],
            )
            .await;
            if let Err(e) = inserted {
                if !is_unique_violation(&e, "uq_telephony_commands_idempotency") {
                    return Err(e);
                }
            }
        }
        let members = self.members(&self.db, id).await?;
        Ok((202, json!({ "bridge": view(&bridge, &members, false) })))
    }

    pub(crate) async fn execute_bridge_command(
        &self,
        command: CommandRow,
        kind: CommandKind,
    ) -> TelephonyResult<Executed> {
        let now = self.now();
        let Some(bridge_id) = command.bridge_id else {
            return Ok(Executed::Skipped);
        };
        match kind {
            CommandKind::Bridge => self.execute_bridge(&command, bridge_id, now).await,
            CommandKind::Unbridge => {
                self.finish_bridge(bridge_id, "UNBRIDGED", now).await?;
                self.finish_simple(command.id, "SUCCEEDED", "UNBRIDGED")
                    .await?;
                Ok(Executed::Done)
            }
            _ => {
                self.finish_simple(command.id, "FAILED", "UNSUPPORTED")
                    .await?;
                Ok(Executed::DefiniteFailure("UNSUPPORTED"))
            }
        }
    }

    async fn finish_simple(&self, id: Uuid, status: &str, code: &str) -> TelephonyResult<()> {
        let now = self.now();
        let finished = matches!(status, "SUCCEEDED" | "FAILED" | "DEAD_LETTER");
        db::exec(
            &self.db,
            "UPDATE telephony_commands SET status = $2, result_code = $3, lease_owner = NULL, lease_until = NULL, \
               updated_at = GREATEST(updated_at, $4), finished_at = CASE WHEN $5 THEN GREATEST(created_at, $4) END, \
               failure_category = CASE WHEN $2 = 'FAILED' THEN 'EXECUTION' END WHERE id = $1",
            vec![id.into(), status.into(), code.into(), now.into(), finished.into()],
        )
        .await
        .map(|_| ())
    }

    async fn requeue(
        &self,
        command: &CommandRow,
        code: &str,
        delay: Duration,
    ) -> TelephonyResult<Executed> {
        let now = self.now();
        if command.attempts >= command.max_attempts {
            self.finish_simple(command.id, "FAILED", code).await?;
            if let Some(b) = command.bridge_id {
                self.finish_bridge(b, "TIMEOUT", now).await?;
            }
            return Ok(Executed::DefiniteFailure("BRIDGE_TIMEOUT"));
        }
        db::exec(
            &self.db,
            "UPDATE telephony_commands SET status = 'PENDING', lease_owner = NULL, lease_until = NULL, \
               next_attempt_at = $2, result_code = $3, updated_at = GREATEST(updated_at, $4) WHERE id = $1",
            vec![command.id.into(), (now + delay).into(), code.into(), now.into()],
        )
        .await?;
        Ok(Executed::Rescheduled)
    }

    async fn execute_bridge(
        &self,
        command: &CommandRow,
        bridge_id: Uuid,
        now: DateTime<Utc>,
    ) -> TelephonyResult<Executed> {
        let bridge = self.load_bridge(&self.db, bridge_id, false).await?;
        if bridge.status != "PENDING" {
            self.finish_simple(command.id, "SUCCEEDED", "ALREADY_DONE")
                .await?;
            return Ok(Executed::Done);
        }
        let members = self.members(&self.db, bridge_id).await?;
        // Every public member must be live and (outbound) answered before media joins.
        for m in members.iter().filter(|m| m.member_kind == "PUBLIC_LEG") {
            let leg = legs::load(&self.db, m.telephony_leg_id.unwrap_or_default())
                .await?
                .ok_or(TelephonyError::Refused(ErrorCode::LegNotFound))?;
            let status = leg.status()?;
            if status.is_terminal() {
                self.finish_bridge(bridge_id, "MEMBER_LEFT", now).await?;
                self.finish_simple(command.id, "FAILED", "MEMBER_ENDED")
                    .await?;
                return Ok(Executed::DefiniteFailure("MEMBER_ENDED"));
            }
            if leg.direction() == crate::domain::Direction::Outbound
                && !matches!(
                    status,
                    LegStatus::Answered | LegStatus::Connected | LegStatus::Held
                )
            {
                return self
                    .requeue(command, "WAITING_FOR_ANSWER", Duration::milliseconds(500))
                    .await;
            }
        }
        // Create the edge bridge (idempotent: a ref is stored once).
        let edge_ref = match &bridge.edge_bridge_ref {
            Some(r) => r.clone(),
            None => match self
                .edge
                .create_bridge(&format!("cfm-{}", bridge_id.simple()))
                .await
            {
                Ok(r) => {
                    // Under the bridge row lock: an application-media channel that arrived
                    // before this ref was stored is held on its member (see
                    // `application_media_joined`) and attached below.
                    let txn = self.db.begin().await?;
                    self.load_bridge(&txn, bridge_id, true).await?;
                    db::exec(
                        &txn,
                        "UPDATE telephony_bridges SET edge_bridge_ref = $2, updated_at = GREATEST(updated_at, $3), \
                           version = version + 1 WHERE id = $1 AND edge_bridge_ref IS NULL",
                        vec![bridge_id.into(), r.clone().into(), now.into()],
                    )
                    .await?;
                    txn.commit().await?;
                    r
                }
                Err(EdgeError::Unavailable) | Err(EdgeError::Timeout) => {
                    return self
                        .requeue(command, "EDGE_UNAVAILABLE", Duration::seconds(1))
                        .await;
                }
                Err(_) => {
                    self.finish_bridge(bridge_id, "FAILED", now).await?;
                    self.finish_simple(command.id, "FAILED", "EDGE_REFUSED")
                        .await?;
                    return Ok(Executed::DefiniteFailure("EDGE_REFUSED"));
                }
            },
        };
        for m in members
            .iter()
            .filter(|m| m.member_kind == "PUBLIC_LEG" && m.status == "EXPECTED")
        {
            let leg = legs::load(&self.db, m.telephony_leg_id.unwrap_or_default())
                .await?
                .ok_or(TelephonyError::Refused(ErrorCode::LegNotFound))?;
            // Part F: an internal enterprise leg has no provider binding — its edge channel
            // is bound directly.
            let channel = match (leg.is_enterprise(), leg.edge_channel_ref.clone()) {
                (true, Some(channel)) => channel,
                _ => {
                    let Some((_, Some(channel), _)) = self.bound_ref(&self.db, &leg).await? else {
                        return self
                            .requeue(command, "WAITING_FOR_BINDING", Duration::milliseconds(500))
                            .await;
                    };
                    channel
                }
            };
            if leg.direction() == crate::domain::Direction::Inbound
                && leg.status()? == LegStatus::Ringing
            {
                // Answer the inbound caller only now (charges start on answer).
                match self.edge.answer(&channel).await {
                    Ok(()) => {
                        let txn = self.db.begin().await?;
                        let locked = legs::lock(&txn, leg.id).await?;
                        legs::transition_in(
                            &txn,
                            &locked,
                            &Change::to(LegStatus::Answered, NormalizedEvent::LegAnswered, now),
                        )
                        .await?;
                        txn.commit().await?;
                    }
                    Err(EdgeError::NotFound) => {
                        self.finish_bridge(bridge_id, "MEMBER_LEFT", now).await?;
                        self.finish_simple(command.id, "FAILED", "MEMBER_ENDED")
                            .await?;
                        return Ok(Executed::DefiniteFailure("MEMBER_ENDED"));
                    }
                    Err(_) => {
                        return self
                            .requeue(command, "EDGE_UNAVAILABLE", Duration::seconds(1))
                            .await
                    }
                }
            }
            match self.edge.add_to_bridge(&edge_ref, &channel).await {
                Ok(()) => {
                    self.member_joined(bridge_id, m.id, &channel, now).await?;
                }
                Err(EdgeError::NotFound) => {
                    self.finish_bridge(bridge_id, "MEMBER_LEFT", now).await?;
                    self.finish_simple(command.id, "FAILED", "MEMBER_ENDED")
                        .await?;
                    return Ok(Executed::DefiniteFailure("MEMBER_ENDED"));
                }
                Err(_) => {
                    return self
                        .requeue(command, "EDGE_UNAVAILABLE", Duration::seconds(1))
                        .await
                }
            }
        }
        // The application media that presented its code before the edge bridge existed.
        if let Some(held) = members
            .iter()
            .find(|m| m.member_kind == "APPLICATION_MEDIA" && m.status == "EXPECTED")
        {
            let held = self
                .members(&self.db, bridge_id)
                .await?
                .into_iter()
                .find(|m| m.id == held.id && m.status == "EXPECTED");
            if let Some(channel) = held.as_ref().and_then(|m| m.edge_channel_ref.clone()) {
                let attached = self.edge.answer(&channel).await.is_ok()
                    && self.edge.add_to_bridge(&edge_ref, &channel).await.is_ok();
                if attached {
                    self.member_joined(bridge_id, held.unwrap().id, &channel, now)
                        .await?;
                } else {
                    // The held media side is gone: it may not rejoin with this code.
                    let _ = self.edge.reject(&channel).await;
                    self.finish_bridge(bridge_id, "MEMBER_LEFT", now).await?;
                    self.finish_simple(command.id, "FAILED", "MEMBER_ENDED")
                        .await?;
                    return Ok(Executed::DefiniteFailure("MEMBER_ENDED"));
                }
            }
        }
        self.finish_simple(command.id, "SUCCEEDED", "BRIDGED")
            .await?;
        self.maybe_activate(bridge_id).await?;
        Ok(Executed::Done)
    }

    /// Records a member's join and activates the bridge when everyone is in.
    pub(crate) async fn member_joined(
        &self,
        bridge_id: Uuid,
        member: Uuid,
        channel: &str,
        at: DateTime<Utc>,
    ) -> TelephonyResult<()> {
        db::exec(
            &self.db,
            "UPDATE telephony_bridge_legs SET status = 'JOINED', joined_at = $3, edge_channel_ref = COALESCE(edge_channel_ref, $4), \
               updated_at = GREATEST(updated_at, $3) WHERE id = $1 AND bridge_id = $2 AND status = 'EXPECTED'",
            vec![member.into(), bridge_id.into(), at.into(), channel.into()],
        )
        .await?;
        self.maybe_activate(bridge_id).await
    }

    async fn maybe_activate(&self, bridge_id: Uuid) -> TelephonyResult<()> {
        let now = self.now();
        let txn = self.db.begin().await?;
        let bridge = self.load_bridge(&txn, bridge_id, true).await?;
        if bridge.status != "PENDING" {
            return Ok(());
        }
        let members = self.members(&txn, bridge_id).await?;
        if members.iter().any(|m| m.status != "JOINED") {
            return Ok(());
        }
        db::exec(
            &txn,
            "UPDATE telephony_bridges SET status = 'ACTIVE', established_at = $2, updated_at = GREATEST(updated_at, $2), \
               version = version + 1 WHERE id = $1",
            vec![bridge_id.into(), now.into()],
        )
        .await?;
        let mut leg_ids = Vec::new();
        for m in &members {
            if let Some(leg_id) = m.telephony_leg_id {
                leg_ids.push(leg_id);
                let leg = legs::lock(&txn, leg_id).await?;
                if matches!(
                    leg.status()?,
                    LegStatus::Answered | LegStatus::Ringing | LegStatus::Originating
                ) {
                    legs::transition_in(
                        &txn,
                        &leg,
                        &Change::to(LegStatus::Connected, NormalizedEvent::LegConnected, now),
                    )
                    .await?;
                }
            }
        }
        outbox::emit_bridge_event(
            &txn,
            bridge_id,
            bridge.calling_call_id,
            NormalizedEvent::BridgeEstablished,
            json!({
                "bridgeKind": bridge.kind,
                "telephonyLegIds": leg_ids,
                "applicationMedia": bridge.kind == "APPLICATION_MEDIA",
                "establishedAt": ts(now),
            }),
            now,
        )
        .await?;
        txn.commit().await?;
        tracing::info!(bridge_id = %bridge_id, call_id = %bridge.calling_call_id, "bridge established");
        Ok(())
    }

    /// Ends a live bridge (idempotent) and emits BRIDGE_ENDED.
    pub(crate) async fn finish_bridge(
        &self,
        bridge_id: Uuid,
        reason: &str,
        at: DateTime<Utc>,
    ) -> TelephonyResult<()> {
        let txn = self.db.begin().await?;
        let bridge = self.load_bridge(&txn, bridge_id, true).await?;
        if !matches!(bridge.status.as_str(), "PENDING" | "ACTIVE") {
            return Ok(());
        }
        if let Some(r) = &bridge.edge_bridge_ref {
            // Best effort: an edge that already lost the bridge is fine.
            let _ = self.edge.destroy_bridge(r).await;
        }
        // The application-media attachment exists only for this bridge: end it too.
        for m in self.members(&txn, bridge_id).await? {
            if m.member_kind == "APPLICATION_MEDIA" && m.status == "JOINED" {
                if let Some(channel) = &m.edge_channel_ref {
                    let _ = self.edge.hangup(channel).await;
                }
            }
        }
        let failed = reason == "FAILED" || (bridge.status == "PENDING" && reason != "UNBRIDGED");
        crate::metrics::inc(
            "telephony_bridges_ended_total",
            &[("outcome", if failed { "FAILED" } else { "ENDED" })],
        );
        db::exec(
            &txn,
            "UPDATE telephony_bridges SET status = $2, end_reason = $3, ended_at = $4, updated_at = GREATEST(updated_at, $4), \
               version = version + 1 WHERE id = $1",
            vec![
                bridge_id.into(),
                if failed { "FAILED" } else { "ENDED" }.into(),
                reason.into(),
                at.into(),
            ],
        )
        .await?;
        db::exec(
            &txn,
            "UPDATE telephony_bridge_legs SET status = 'LEFT', left_at = $2, updated_at = GREATEST(updated_at, $2) \
             WHERE bridge_id = $1 AND status <> 'LEFT'",
            vec![bridge_id.into(), at.into()],
        )
        .await?;
        outbox::emit_bridge_event(
            &txn,
            bridge_id,
            bridge.calling_call_id,
            NormalizedEvent::BridgeEnded,
            json!({ "bridgeKind": bridge.kind, "endReason": reason, "established": bridge.established_at.is_some() }),
            at,
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    /// The application media side presented `code` at the edge on `channel`: attaches it
    /// to its bridge (single use). Returns false when the code is unknown / used / late.
    pub async fn application_media_joined(
        &self,
        code: &str,
        channel: &str,
    ) -> TelephonyResult<bool> {
        let now = self.now();
        let bridge: Option<BridgeRow> = db::one(
            &self.db,
            &format!("SELECT {BRIDGE_COLUMNS} FROM telephony_bridges WHERE media_join_code = $1"),
            vec![code.into()],
        )
        .await?;
        let Some(bridge) = bridge else {
            return Ok(false);
        };
        if bridge.status != "PENDING"
            || now - bridge.created_at > Duration::seconds(APP_MEDIA_JOIN_TIMEOUT_SECONDS)
        {
            return Ok(false);
        }
        let members = self.members(&self.db, bridge.id).await?;
        let Some(member) = members
            .iter()
            .find(|m| m.member_kind == "APPLICATION_MEDIA" && m.status == "EXPECTED")
        else {
            return Ok(false);
        };
        let edge_ref = {
            // Serialized with the bridge command storing the edge bridge ref: either the
            // ref exists (attach now) or the channel is held on the member for the
            // command to attach — exactly one of the two attaches it.
            let txn = self.db.begin().await?;
            let locked = self.load_bridge(&txn, bridge.id, true).await?;
            match locked.edge_bridge_ref {
                Some(r) if locked.status == "PENDING" => {
                    let held_elsewhere = self
                        .members(&txn, bridge.id)
                        .await?
                        .into_iter()
                        .any(|m| m.id == member.id && m.edge_channel_ref.is_some());
                    txn.commit().await?;
                    if held_elsewhere {
                        return Ok(false);
                    }
                    r
                }
                Some(_) => {
                    txn.commit().await?;
                    return Ok(false);
                }
                None => {
                    let held = db::exec(
                        &txn,
                        "UPDATE telephony_bridge_legs SET edge_channel_ref = $3, updated_at = GREATEST(updated_at, $4) \
                         WHERE id = $1 AND bridge_id = $2 AND status = 'EXPECTED' AND edge_channel_ref IS NULL",
                        vec![member.id.into(), bridge.id.into(), channel.into(), now.into()],
                    )
                    .await?;
                    txn.commit().await?;
                    // Held (unanswered) until the bridge command attaches it; a second
                    // presentation of the code is refused.
                    return Ok(held == 1);
                }
            }
        };
        if self.edge.answer(&channel.to_string()).await.is_err() {
            return Ok(false);
        }
        if self
            .edge
            .add_to_bridge(&edge_ref, &channel.to_string())
            .await
            .is_err()
        {
            return Ok(false);
        }
        self.member_joined(bridge.id, member.id, channel, now)
            .await?;
        Ok(true)
    }

    /// A member channel left the bridge or hung up.
    pub async fn bridge_member_left(
        &self,
        leg_id: Option<Uuid>,
        channel: Option<&str>,
    ) -> TelephonyResult<()> {
        let now = self.now();
        let rows: Vec<IdRow> = db::all(
            &self.db,
            "SELECT b.id FROM telephony_bridges b JOIN telephony_bridge_legs m ON m.bridge_id = b.id \
             WHERE b.status IN ('PENDING', 'ACTIVE') AND ((m.telephony_leg_id = $1) OR (m.edge_channel_ref = $2))",
            vec![leg_id.into(), channel.map(str::to_string).into()],
        )
        .await?;
        for r in rows {
            self.finish_bridge(r.id, "MEMBER_LEFT", now).await?;
        }
        Ok(())
    }

    /// Bridges waiting too long for their application media (or members).
    pub async fn expire_pending_bridges(&self) -> TelephonyResult<usize> {
        let now = self.now();
        let rows: Vec<IdRow> = db::all(
            &self.db,
            "SELECT id FROM telephony_bridges WHERE status = 'PENDING' AND created_at < $1 LIMIT 100",
            vec![(now - Duration::seconds(APP_MEDIA_JOIN_TIMEOUT_SECONDS)).into()],
        )
        .await?;
        for r in &rows {
            self.finish_bridge(r.id, "TIMEOUT", now).await?;
        }
        Ok(rows.len())
    }
}

#[derive(Debug, FromQueryResult)]
struct IdRow {
    id: Uuid,
}

fn view(bridge: &BridgeRow, members: &[MemberRow], with_code: bool) -> Value {
    let mut v = json!({
        "bridgeId": bridge.id,
        "callId": bridge.calling_call_id,
        "kind": bridge.kind,
        "status": bridge.status,
        "version": bridge.version,
        "members": members.iter().map(|m| {
            let mut j = json!({ "memberKind": m.member_kind, "status": m.status });
            if let Some(l) = m.telephony_leg_id {
                j["telephonyLegId"] = json!(l);
            }
            j
        }).collect::<Vec<_>>(),
    });
    if with_code && bridge.status == "PENDING" {
        if let Some(code) = &bridge.media_join_code {
            v["mediaJoinCode"] = json!(code);
        }
    }
    if let Some(at) = bridge.established_at {
        v["establishedAt"] = json!(ts(at));
    }
    if let Some(at) = bridge.ended_at {
        v["endedAt"] = json!(ts(at));
    }
    if let Some(r) = &bridge.end_reason {
        v["endReason"] = json!(r);
    }
    v
}
