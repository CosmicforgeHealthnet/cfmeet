//! Conference realtime subscription (spec Part E §15–§16).
//!
//! - `POST /api/v2/conferences/{id}/realtime/ticket` — authorize and mint a one-shot,
//!   60-second ticket (browser WebSockets cannot send headers).
//! - `GET  /api/v2/conferences/{id}/realtime` — WebSocket upgrade, authenticated by the
//!   ticket or by the usual bearer / participant-token headers. Authorization happens
//!   before the upgrade and is re-checked during the connection.
//! - `GET  /api/v2/conferences/{id}/events?after=` — REST replay for recovery.
//!
//! Frames (JSON text): `subscribed`, `event` (with the stable envelope), `degraded`,
//! `resync_required`, `revoked`, `closed`, `pong`. Delivery is ordered by the
//! conference sequence; consumers must still tolerate duplicates across reconnects and
//! refetch `GET /api/v2/conferences/{id}` whenever told to resync.

use std::{pin::Pin, sync::Arc, time::Duration};

use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::Response,
    Json,
};
use futures_util::{SinkExt, Stream, StreamExt};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::V2Path;
use crate::{
    dto::v2::*,
    error::{ApiError, ApiResult, ErrorCode},
    services::{
        actor::ActorContext,
        meeting_events::event,
        realtime::{
            conference_channel, envelope, RealtimeService, Subscriber, SubscriberPrincipal,
            SubscriptionScope, TicketSubject,
        },
        rtc_metrics::{inc, RTC_METRICS as M},
    },
    state::AppState,
};

fn scope_name(scope: SubscriptionScope) -> &'static str {
    match scope {
        SubscriptionScope::Full => "FULL",
        SubscriptionScope::SelfOnly(_) => "SELF_ONLY",
    }
}

fn canonical(conference_id: Uuid) -> String {
    format!("/api/v2/conferences/{conference_id}")
}

/// Mint a one-shot realtime subscription ticket.
#[utoipa::path(
    post, path = "/api/v2/conferences/{id}/realtime/ticket", tag = "Realtime (v2)", operation_id = "v2_realtime_ticket",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses(
        (status = 200, description = "Ticket", body = RealtimeTicketResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED / MEETING_ACCESS_DENIED", body = ErrorEnvelope),
        (status = 404, description = "CONFERENCE_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn create_ticket(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Json<RealtimeTicketResponse>> {
    let rt = &state.realtime;
    rt.require_enabled()?;
    let sub = rt.authorize(&actor, id).await?;
    let (ticket, expires_at) = rt.issue_ticket(&sub)?;
    Ok(Json(RealtimeTicketResponse {
        ticket,
        expires_at,
        websocket_path: format!("/api/v2/conferences/{id}/realtime"),
        cursor: rt.current_sequence(id).await?,
        scope: scope_name(sub.scope).into(),
    }))
}

/// Replay committed conference events after a cursor (recovery without WebSocket).
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/events", tag = "Realtime (v2)", operation_id = "v2_replay_events",
    params(("id" = Uuid, Path, description = "Conference id"), ("after" = Option<i64>, Query, description = "Last processed sequence")),
    responses(
        (status = 200, description = "Events or resync instruction", body = EventReplayResponse),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "CONFERENCE_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn replay_events(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    query: Result<Query<EventReplayQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult<Json<EventReplayResponse>> {
    let Query(q) = query
        .map_err(|_| ApiError::domain(ErrorCode::ValidationError, "Invalid query parameters"))?;
    let rt = &state.realtime;
    rt.require_enabled()?;
    let sub = rt.authorize(&actor, id).await?;
    let after = q.after.unwrap_or(0);
    Ok(Json(match rt.replay(&sub, after).await? {
        Some((events, cursor)) => EventReplayResponse {
            events,
            cursor,
            resync_required: false,
            canonical: canonical(id),
        },
        None => {
            inc(&M.ws_resync_required);
            EventReplayResponse {
                events: vec![],
                cursor: rt.current_sequence(id).await?,
                resync_required: true,
                canonical: canonical(id),
            }
        }
    }))
}

#[derive(Debug, Deserialize, Default)]
pub struct WsQuery {
    pub ticket: Option<String>,
    pub cursor: Option<i64>,
}

/// WebSocket conference event stream.
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/realtime", tag = "Realtime (v2)", operation_id = "v2_realtime_subscribe",
    params(("id" = Uuid, Path, description = "Conference id"), ("ticket" = Option<String>, Query, description = "One-shot ticket"), ("cursor" = Option<i64>, Query, description = "Resume after this sequence")),
    responses(
        (status = 101, description = "Switching protocols (WebSocket)"),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "CONFERENCE_NOT_FOUND / FEATURE_DISABLED", body = ErrorEnvelope),
    ),
    security((), ("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn subscribe(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
    query: Result<Query<WsQuery>, axum::extract::rejection::QueryRejection>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let Query(q) = query
        .map_err(|_| ApiError::domain(ErrorCode::ValidationError, "Invalid query parameters"))?;
    let rt = state.realtime.clone();
    rt.require_enabled()?;
    let authorized = async {
        match &q.ticket {
            Some(ticket) => {
                let principal = match rt.verify_ticket(ticket, id)? {
                    TicketSubject::User(uid) => {
                        SubscriberPrincipal::User(rt.user_actor(uid).await?)
                    }
                    TicketSubject::Participant(pid) => SubscriberPrincipal::Participant(pid),
                };
                let conference =
                    crate::services::meeting_core::find_conference(rt.db(), id).await?;
                rt.resolve(&conference, principal).await
            }
            None => rt.authorize(&actor, id).await,
        }
    }
    .await;
    let sub = match authorized {
        Ok(s) => s,
        Err(e) => {
            inc(&M.ws_rejected);
            return Err(e);
        }
    };
    let cursor = q.cursor;
    Ok(ws.on_upgrade(move |socket| run_socket(rt, sub, cursor, socket)))
}

type RedisStream = Pin<Box<dyn Stream<Item = redis::Msg> + Send>>;

async fn next_signal(stream: &mut Option<RedisStream>) -> Option<Option<redis::Msg>> {
    match stream {
        Some(s) => Some(s.next().await),
        None => std::future::pending().await,
    }
}

async fn send_json(
    tx: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    v: serde_json::Value,
) -> bool {
    tx.send(Message::Text(v.to_string())).await.is_ok()
}

async fn close(
    tx: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    code: u16,
    reason: &'static str,
) {
    let _ = tx
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

async fn run_socket(
    rt: Arc<RealtimeService>,
    mut sub: Subscriber,
    cursor: Option<i64>,
    socket: WebSocket,
) {
    inc(&M.ws_connections);
    let conference_id = sub.conference_id;
    let (mut tx, mut rx) = socket.split();
    // Subscribe to the fanout BEFORE reading the replay so nothing committed in between
    // is missed. Redis down → degraded: events are tailed from PostgreSQL instead.
    let mut stream: Option<RedisStream> = match rt.redis() {
        Some(r) => match r.subscribe(&conference_channel(conference_id)).await {
            Ok(p) => Some(Box::pin(p.into_on_message())),
            Err(e) => {
                tracing::warn!(conference_id = %conference_id, "realtime fanout unavailable: {e}");
                None
            }
        },
        None => None,
    };
    let degraded = stream.is_none();
    let current = match rt.current_sequence(conference_id).await {
        Ok(c) => c,
        Err(_) => return close(&mut tx, 1011, "unavailable").await,
    };
    let mut last = current;
    let mut start = current;
    let mut replay = vec![];
    if let Some(c) = cursor {
        match rt.replay(&sub, c).await {
            Ok(Some((events, l))) => {
                replay = events;
                last = l;
                start = c;
            }
            Ok(None) => {
                inc(&M.ws_resync_required);
                if !send_json(&mut tx, json!({ "type": "resync_required", "cursor": current, "canonical": canonical(conference_id) })).await {
                    return;
                }
            }
            Err(_) => return close(&mut tx, 1011, "unavailable").await,
        }
    }
    if !send_json(
        &mut tx,
        json!({
            "type": "subscribed",
            "conferenceId": conference_id,
            "cursor": start,
            "scope": scope_name(sub.scope),
            "degraded": degraded,
            "canonical": canonical(conference_id),
        }),
    )
    .await
    {
        return;
    }
    for env in replay {
        if !send_json(&mut tx, json!({ "type": "event", "event": env })).await {
            return;
        }
    }
    if degraded
        && !send_json(&mut tx, json!({ "type": "degraded", "reason": "REALTIME_FANOUT_UNAVAILABLE", "canonical": canonical(conference_id) })).await
    {
        return;
    }
    let mut poll = tokio::time::interval(if degraded {
        Duration::from_secs(1)
    } else {
        Duration::from_secs(5)
    });
    let mut recheck = tokio::time::interval(Duration::from_secs(15));
    recheck.tick().await;
    loop {
        let mut wake = false;
        tokio::select! {
            signal = next_signal(&mut stream) => match signal {
                Some(Some(_)) => wake = true,
                _ => {
                    // Fanout lost mid-connection: degrade to PostgreSQL tailing.
                    stream = None;
                    poll = tokio::time::interval(Duration::from_secs(1));
                    if !send_json(&mut tx, json!({ "type": "degraded", "reason": "REALTIME_FANOUT_UNAVAILABLE", "canonical": canonical(conference_id) })).await {
                        return;
                    }
                }
            },
            _ = poll.tick() => wake = true,
            _ = recheck.tick() => match rt.recheck(&sub).await {
                Ok(s) => sub = s,
                Err(_) => {
                    let _ = send_json(&mut tx, json!({ "type": "revoked" })).await;
                    return close(&mut tx, 4403, "revoked").await;
                }
            },
            incoming = rx.next() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(Message::Ping(p))) => { let _ = tx.send(Message::Pong(p)).await; }
                Some(Ok(Message::Text(t)))
                    if t.trim() == "ping"
                        && !send_json(&mut tx, json!({ "type": "pong", "cursor": last })).await =>
                {
                    return;
                }
                _ => {}
            },
        }
        if !wake {
            continue;
        }
        loop {
            let rows = match rt.events_after(conference_id, last, 200).await {
                Ok(r) => r,
                Err(_) => break,
            };
            let n = rows.len();
            for row in rows {
                let Some(seq) = row.conference_seq else {
                    continue;
                };
                last = seq;
                let Some(env) = envelope(&row) else { continue };
                if sub.may_receive(&env)
                    && !send_json(&mut tx, json!({ "type": "event", "event": env })).await
                {
                    return;
                }
                if sub.requires_recheck(&env) {
                    match rt.recheck(&sub).await {
                        Ok(s) => sub = s,
                        Err(_) => {
                            let _ = send_json(&mut tx, json!({ "type": "revoked" })).await;
                            return close(&mut tx, 4403, "revoked").await;
                        }
                    }
                }
                if matches!(
                    env.event_type.as_str(),
                    event::CONFERENCE_ENDED
                        | event::CONFERENCE_CANCELLED
                        | event::CONFERENCE_FAILED
                ) {
                    let _ = send_json(
                        &mut tx,
                        json!({ "type": "closed", "reason": env.event_type }),
                    )
                    .await;
                    return close(&mut tx, 1000, "conference closed").await;
                }
            }
            if n < 200 {
                break;
            }
        }
    }
}
