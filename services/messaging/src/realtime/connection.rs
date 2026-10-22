//! One authenticated WebSocket connection (§58–§67).
//!
//! A single task owns the socket, so inbound frames, hub events, heartbeats and
//! re-validation are processed in one order and a slow socket backpressures only itself
//! (its bounded hub queue fills, the hub marks it lagging, it closes resumably).
//!
//! Durable delivery: the hub hands over POINTERS (conversation, event sequence); the
//! connection fetches the committed events after its last delivered cursor from the
//! database, so duplicates are dropped, gaps are filled, arrival order on the broker
//! never matters, and a deleted message replays as a tombstone. Every event is
//! hydrated for THIS viewer (history window, delete-for-me). A removal of the viewer
//! ends the subscription right after the `member.removed` event.
//!
//! Acknowledgements (`ack`) are application acknowledgements: accepted only up to what
//! was actually delivered on this connection, persisted per device, and used as the
//! default resume position. They are not read receipts.

use std::{
    collections::HashMap,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    broker::BrokerMessage,
    hub::{CloseNotice, ConnectionHandle, HubEvent},
    presence,
    sessions::{self, SessionRow},
};
use crate::{
    conversations::{
        access::{self, Membership},
        policy::{self, EffectivePolicy},
    },
    db,
    error::{Error, ErrorCode},
    identity::TrustedContext,
    messages,
    state::AppState,
};

const CLOSE_AUTH: u16 = 4401;
const CLOSE_POLICY: u16 = 1008;
const CLOSE_SLOW: u16 = 1013;
const CLOSE_RESTART: u16 = 1012;
const CLOSE_IDLE: u16 = 1001;
/// Typing "started" signals of one conversation are coalesced within this window.
const TYPING_COALESCE: Duration = Duration::from_secs(3);
/// Presence queries per connection.
const PRESENCE_QUERY_EVERY: Duration = Duration::from_secs(1);
/// Inbound rate-limit violations tolerated before the connection is closed.
const MAX_VIOLATIONS: u32 = 50;

struct Subscription {
    last_delivered: i64,
    member: Membership,
}

struct Bucket {
    tokens: f64,
    last: Instant,
    rate: f64,
}

impl Bucket {
    fn allow(&mut self) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * self.rate)
            .min(self.rate * 2.0);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

type Sink = futures_util::stream::SplitSink<WebSocket, Message>;

pub struct Connection {
    state: AppState,
    session: SessionRow,
    ctx: TrustedContext,
    handle: ConnectionHandle,
    sink: Sink,
    subs: HashMap<Uuid, Subscription>,
    typing: HashMap<Uuid, Instant>,
    bucket: Bucket,
    violations: u32,
    last_inbound: Instant,
    last_presence_query: Option<Instant>,
    policy: EffectivePolicy,
    announced_online: bool,
    /// When the connection opened (usage: connection seconds, recorded on close).
    opened_at: chrono::DateTime<chrono::Utc>,
}

enum Flow {
    Continue,
    Close(u16, &'static str),
}

fn error_frame(code: &str, message: &str, request_id: Option<&Value>) -> Value {
    json!({ "type": "error", "code": code, "message": message, "requestId": request_id })
}

impl Connection {
    /// Serves one upgraded connection until it closes.
    pub async fn serve(
        state: AppState,
        session: SessionRow,
        handle: ConnectionHandle,
        mut socket: WebSocket,
    ) {
        let id = handle.id;
        let (Some(ctx), Ok(policy)) = (
            session.trusted_context(),
            policy::current(&state.db, session.environment_id).await,
        ) else {
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: 1011,
                    reason: "UNAVAILABLE".into(),
                })))
                .await;
            state.realtime.hub.unregister(id);
            return;
        };
        let (sink, stream) = socket.split();
        let rate = state.realtime.settings.inbound_frames_per_second.max(1) as f64;
        let opened_at = state.now();
        let mut c = Self {
            state,
            session,
            ctx,
            handle,
            sink,
            subs: HashMap::new(),
            typing: HashMap::new(),
            bucket: Bucket {
                tokens: rate * 2.0,
                last: Instant::now(),
                rate,
            },
            violations: 0,
            last_inbound: Instant::now(),
            last_presence_query: None,
            policy,
            announced_online: false,
            opened_at,
        };
        c.run(stream).await;
    }

    async fn send(&mut self, value: Value) -> bool {
        let text = value.to_string();
        match tokio::time::timeout(
            self.state.realtime.settings.send_timeout,
            self.sink.send(Message::Text(text)),
        )
        .await
        {
            Ok(Ok(())) => true,
            _ => {
                crate::metrics::inc("messaging_realtime_send_failures_total", &[]);
                false
            }
        }
    }

    async fn close(&mut self, code: u16, reason: &'static str) {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.sink.send(Message::Close(Some(CloseFrame {
                code,
                reason: reason.into(),
            }))),
        )
        .await;
        crate::metrics::inc("messaging_realtime_closes_total", &[("reason", reason)]);
    }

    async fn run(&mut self, mut stream: futures_util::stream::SplitStream<WebSocket>) {
        let settings = self.state.realtime.settings.clone();
        let welcome = json!({
            "type": "welcome",
            "protocolVersion": super::PROTOCOL_VERSION,
            "connectionId": format!("conn_{}", self.handle.id),
            "sessionExpiresAt": crate::conversations::service::rfc3339(self.session.expires_at),
            "heartbeatIntervalMs": settings.heartbeat_interval.as_millis() as u64,
            "limits": {
                "maxFrameBytes": settings.max_frame_bytes,
                "maxSubscriptions": settings.max_subscriptions,
                "inboundFramesPerSecond": settings.inbound_frames_per_second,
                "maxReplay": settings.max_replay,
            },
        });
        if !self.send(welcome).await {
            return self.finish().await;
        }
        self.presence_online().await;
        let mut heartbeat = tokio::time::interval(settings.heartbeat_interval);
        heartbeat.tick().await;
        let mut revalidate = tokio::time::interval(settings.revalidate_interval);
        revalidate.tick().await;
        let close = self.handle.close.clone();
        let mut draining = self.handle.draining.clone();
        let flow = loop {
            let expires_in = (self.session.expires_at - self.state.now())
                .to_std()
                .unwrap_or(Duration::ZERO);
            let flow = tokio::select! {
                biased;
                _ = close.notified() => {
                    if self.handle.lagging.load(Ordering::SeqCst) {
                        let resume: Vec<Value> = self.subs.iter()
                            .map(|(c, s)| json!({ "conversationId": c, "afterEventSequence": s.last_delivered }))
                            .collect();
                        let _ = self.send(json!({
                            "type": "error", "code": "SLOW_CONSUMER",
                            "message": "The connection fell too far behind; reconnect and resume from these cursors.",
                            "resume": resume,
                        })).await;
                        Flow::Close(CLOSE_SLOW, "SLOW_CONSUMER")
                    } else {
                        let notice = self.handle.close_notice.lock().unwrap().take()
                            .unwrap_or(CloseNotice { code: CLOSE_AUTH, reason: "SESSION_REVOKED" });
                        let _ = self.send(json!({ "type": "error", "code": notice.reason, "message": "The connection's authorization ended." })).await;
                        Flow::Close(notice.code, notice.reason)
                    }
                }
                changed = draining.changed() => {
                    if changed.is_ok() && *draining.borrow() {
                        let jitter = rand::random::<u64>() % 5_000;
                        let _ = self.send(json!({ "type": "server.draining", "reconnectAfterMs": 500 + jitter })).await;
                        Flow::Close(CLOSE_RESTART, "SERVER_DRAINING")
                    } else {
                        Flow::Continue
                    }
                }
                inbound = stream.next() => match inbound {
                    None | Some(Err(_)) => Flow::Close(1000, "CLIENT_GONE"),
                    Some(Ok(m)) => self.inbound(m).await,
                },
                event = self.handle.rx.recv() => match event {
                    Some(e) => self.hub_event(e).await,
                    None => Flow::Close(1011, "HUB_CLOSED"),
                },
                _ = heartbeat.tick() => {
                    if self.last_inbound.elapsed() > settings.idle_timeout {
                        Flow::Close(CLOSE_IDLE, "IDLE_TIMEOUT")
                    } else {
                        self.presence_touch().await;
                        if tokio::time::timeout(settings.send_timeout, self.sink.send(Message::Ping(Vec::new()))).await.is_ok() {
                            Flow::Continue
                        } else {
                            Flow::Close(CLOSE_SLOW, "SEND_TIMEOUT")
                        }
                    }
                }
                _ = revalidate.tick() => self.revalidate().await,
                _ = tokio::time::sleep(expires_in) => {
                    let _ = self.send(json!({ "type": "error", "code": "SESSION_EXPIRED", "message": "Refresh the session with auth.refresh before it expires." })).await;
                    Flow::Close(CLOSE_AUTH, "SESSION_EXPIRED")
                }
            };
            if let Flow::Close(code, reason) = flow {
                break (code, reason);
            }
        };
        if flow.1 != "CLIENT_GONE" {
            self.close(flow.0, flow.1).await;
        }
        self.finish().await;
    }

    async fn finish(&mut self) {
        let typing: Vec<Uuid> = self.typing.drain().map(|(c, _)| c).collect();
        for c in typing {
            self.publish_typing(c, false).await;
        }
        self.presence_offline().await;
        self.state.realtime.hub.unregister(self.handle.id);
        let seconds = (self.state.now() - self.opened_at).num_seconds().max(0);
        crate::usage::record_best_effort(
            &self.state.db,
            crate::usage::Owner {
                tenant_id: self.session.tenant_id,
                application_id: self.session.application_id,
                environment_id: self.session.environment_id,
            },
            crate::usage::REALTIME_CONNECTION,
            seconds,
            self.session.device_id,
            None,
            self.state.now(),
        )
        .await;
    }

    // -----------------------------------------------------------------------------------
    // Inbound frames
    // -----------------------------------------------------------------------------------

    async fn inbound(&mut self, message: Message) -> Flow {
        self.last_inbound = Instant::now();
        let text = match message {
            Message::Text(t) => t,
            Message::Ping(_) | Message::Pong(_) => return Flow::Continue,
            Message::Close(_) => return Flow::Close(1000, "CLIENT_GONE"),
            Message::Binary(_) => {
                self.send(error_frame(
                    "UNSUPPORTED_FRAME",
                    "Binary frames are not part of the protocol.",
                    None,
                ))
                .await;
                return Flow::Continue;
            }
        };
        if !self.bucket.allow() {
            self.violations += 1;
            crate::metrics::inc("messaging_realtime_frames_throttled_total", &[]);
            if self.violations > MAX_VIOLATIONS {
                return Flow::Close(CLOSE_POLICY, "FRAME_RATE_EXCEEDED");
            }
            self.send(error_frame(
                "RATE_LIMITED",
                "Too many frames; slow down.",
                None,
            ))
            .await;
            return Flow::Continue;
        }
        let Ok(frame) = serde_json::from_str::<Value>(&text) else {
            self.send(error_frame(
                "VALIDATION_ERROR",
                "Frames are JSON objects with a type.",
                None,
            ))
            .await;
            return Flow::Continue;
        };
        let request_id = frame
            .get("requestId")
            .cloned()
            .filter(|v| v.as_str().is_some_and(|s| s.len() <= 64));
        let rid = request_id.as_ref();
        let uuid_of = |k: &str| {
            frame
                .get(k)
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok())
        };
        match frame.get("type").and_then(Value::as_str).unwrap_or("") {
            "ping" => {
                self.send(json!({ "type": "pong", "requestId": rid })).await;
                Flow::Continue
            }
            "subscribe" => match uuid_of("conversationId") {
                Some(c) => {
                    let after = frame.get("afterEventSequence").and_then(Value::as_i64);
                    self.subscribe(c, after, rid).await
                }
                None => self.invalid(rid).await,
            },
            "unsubscribe" => {
                match uuid_of("conversationId") {
                    Some(c) => {
                        self.drop_subscription(c).await;
                        self.send(json!({ "type": "unsubscribed", "conversationId": c, "requestId": rid })).await;
                        Flow::Continue
                    }
                    None => self.invalid(rid).await,
                }
            }
            "ack" => match (
                uuid_of("conversationId"),
                frame.get("eventSequence").and_then(Value::as_i64),
            ) {
                (Some(c), Some(seq)) => self.ack(c, seq, rid).await,
                _ => self.invalid(rid).await,
            },
            "typing" => match (
                uuid_of("conversationId"),
                frame.get("state").and_then(Value::as_str),
            ) {
                (Some(c), Some(s @ ("started" | "stopped"))) => {
                    self.typing(c, s == "started", rid).await
                }
                _ => self.invalid(rid).await,
            },
            "presence.set" => match frame.get("visibility").and_then(Value::as_str) {
                Some(v @ ("CONVERSATION_MEMBERS" | "HIDDEN")) => {
                    self.presence_set(v, frame.get("shareLastSeen").and_then(Value::as_bool), rid)
                        .await
                }
                _ => self.invalid(rid).await,
            },
            "presence.query" => match uuid_of("conversationId") {
                Some(c) => self.presence_query(c, rid).await,
                None => self.invalid(rid).await,
            },
            "auth.refresh" => match frame.get("ticket").and_then(Value::as_str) {
                Some(t) => self.refresh(t.to_string(), rid).await,
                None => self.invalid(rid).await,
            },
            _ => {
                // Unknown frame types are refused without closing: newer clients keep
                // working against an older gateway (forward compatibility).
                self.send(error_frame("UNSUPPORTED_FRAME", "Unknown frame type.", rid))
                    .await;
                Flow::Continue
            }
        }
    }

    async fn invalid(&mut self, rid: Option<&Value>) -> Flow {
        self.send(error_frame(
            "VALIDATION_ERROR",
            "The frame is missing a field or has an invalid value.",
            rid,
        ))
        .await;
        Flow::Continue
    }

    async fn api_error(&mut self, e: Error, rid: Option<&Value>) -> Flow {
        let code = e.error_code();
        self.send(error_frame(code.as_str(), code.default_message(), rid))
            .await;
        if code == ErrorCode::ServiceUnavailable {
            return Flow::Continue;
        }
        Flow::Continue
    }

    async fn subscribe(
        &mut self,
        conversation: Uuid,
        after: Option<i64>,
        rid: Option<&Value>,
    ) -> Flow {
        if !self.session.has_scope("messages.read") {
            return self
                .api_error(Error::code(ErrorCode::InsufficientScope), rid)
                .await;
        }
        if !self.subs.contains_key(&conversation)
            && self.subs.len() >= self.state.realtime.settings.max_subscriptions
        {
            self.send(error_frame(
                "SUBSCRIPTION_LIMIT",
                "Too many subscriptions on this connection.",
                rid,
            ))
            .await;
            return Flow::Continue;
        }
        let member = match access::as_member(
            &self.state.db,
            &self.ctx,
            conversation,
            self.session.principal_id,
        )
        .await
        {
            Ok((_, m)) => m,
            // Unknown, foreign and non-member conversations are indistinguishable.
            Err(e) => return self.api_error(e, rid).await,
        };
        if !self
            .state
            .realtime
            .hub
            .subscribe(self.handle.id, conversation)
        {
            self.send(error_frame(
                "SUBSCRIPTION_LIMIT",
                "Too many subscriptions on this connection.",
                rid,
            ))
            .await;
            return Flow::Continue;
        }
        let stored = db::scalar_i64(
            &self.state.db,
            "SELECT COALESCE(max(acked_event_sequence), -1) FROM messaging_ack_cursors WHERE conversation_id = $1 AND device_id = $2",
            vec![conversation.into(), self.session.device_id.into()],
        )
        .await
        .unwrap_or(-1);
        let head = self.head(conversation).await.unwrap_or(0);
        let start = after
            .or((stored >= 0).then_some(stored))
            .unwrap_or(head)
            .clamp(0, head.max(0))
            .max(member.visible_from_event);
        let oldest = messages::oldest_event(&self.state.db, conversation)
            .await
            .unwrap_or(0);
        self.subs.insert(
            conversation,
            Subscription {
                last_delivered: start,
                member,
            },
        );
        self.send(json!({
            "type": "subscribed",
            "conversationId": conversation,
            "afterEventSequence": start,
            "headEventSequence": head,
            "requestId": rid,
        }))
        .await;
        if oldest > 0 && start + 1 < oldest {
            return self.sync_required(conversation, start, head).await;
        }
        self.deliver_until(conversation, head).await
    }

    async fn sync_required(&mut self, conversation: Uuid, after: i64, head: i64) -> Flow {
        crate::metrics::inc("messaging_realtime_sync_required_total", &[]);
        let ok = self
            .send(json!({
                "type": "sync.required",
                "conversationId": conversation,
                "afterEventSequence": after,
                "headEventSequence": head,
                "path": format!("/v1/conversations/{conversation}/events?after={after}"),
            }))
            .await;
        if let Some(s) = self.subs.get_mut(&conversation) {
            s.last_delivered = s.last_delivered.max(head);
        }
        if ok {
            Flow::Continue
        } else {
            Flow::Close(CLOSE_SLOW, "SEND_TIMEOUT")
        }
    }

    /// The event position of this principal's latest removal from `conversation`.
    async fn own_removal(&self, conversation: Uuid) -> Option<i64> {
        db::scalar_i64(
            &self.state.db,
            "SELECT COALESCE((SELECT conversation_event_sequence FROM messaging_outbox_events \
                WHERE conversation_id = $1 AND event_type = 'messaging.member.removed' AND payload->>'principal_id' = $2 \
                ORDER BY conversation_event_sequence DESC LIMIT 1), 0)",
            vec![conversation.into(), self.session.principal_id.to_string().into()],
        )
        .await
        .ok()
        .filter(|s| *s > 0)
    }

    async fn head(&self, conversation: Uuid) -> Option<i64> {
        db::scalar_i64(
            &self.state.db,
            "SELECT next_event_sequence - 1 FROM messaging_conversation_sequences WHERE conversation_id = $1",
            vec![conversation.into()],
        )
        .await
        .ok()
    }

    /// Delivers the committed events after the subscription's cursor up to `target`
    /// (bounded by the replay limit; beyond it the client resynchronizes by REST).
    async fn deliver_until(&mut self, conversation: Uuid, target: i64) -> Flow {
        let max_replay = self.state.realtime.settings.max_replay;
        loop {
            let Some(sub) = self.subs.get(&conversation) else {
                return Flow::Continue;
            };
            let last = sub.last_delivered;
            if target <= last {
                return Flow::Continue;
            }
            if target - last > max_replay {
                return self.sync_required(conversation, last, target).await;
            }
            let rows = match messages::events_after(
                &self.state.db,
                conversation,
                last,
                (target - last).min(200),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => return Flow::Continue, // retried on the next event / revalidation
            };
            if rows.is_empty() {
                return Flow::Continue;
            }
            let member = sub.member.clone();
            let hydrated =
                match messages::hydrate(&self.state, self.session.principal_id, &member, &rows)
                    .await
                {
                    Ok(h) => h,
                    Err(_) => return Flow::Continue,
                };
            for (row, event) in rows.iter().zip(hydrated) {
                if !self.send(json!({ "type": "event", "event": event })).await {
                    return Flow::Close(CLOSE_SLOW, "SEND_TIMEOUT");
                }
                let seq = row.conversation_event_sequence.unwrap_or(last);
                if let Some(s) = self.subs.get_mut(&conversation) {
                    s.last_delivered = seq;
                }
                crate::metrics::inc("messaging_realtime_events_delivered_total", &[]);
                // The viewer's own removal ends the subscription right after the event.
                if row.event_type == "messaging.member.removed"
                    && row.payload.get("principal_id").and_then(Value::as_str)
                        == Some(self.session.principal_id.to_string().as_str())
                {
                    self.revoke_subscription(conversation, "MEMBERSHIP_ENDED")
                        .await;
                    return Flow::Continue;
                }
            }
        }
    }

    async fn revoke_subscription(&mut self, conversation: Uuid, reason: &'static str) {
        self.drop_subscription(conversation).await;
        self.send(json!({ "type": "subscription.revoked", "conversationId": conversation, "reason": reason })).await;
        crate::metrics::inc("messaging_realtime_subscriptions_revoked_total", &[]);
    }

    async fn drop_subscription(&mut self, conversation: Uuid) {
        self.subs.remove(&conversation);
        self.state
            .realtime
            .hub
            .unsubscribe(self.handle.id, conversation);
        if self.typing.remove(&conversation).is_some() {
            self.publish_typing(conversation, false).await;
        }
    }

    async fn ack(&mut self, conversation: Uuid, seq: i64, rid: Option<&Value>) -> Flow {
        let Some(sub) = self.subs.get(&conversation) else {
            self.send(error_frame(
                "NOT_SUBSCRIBED",
                "Subscribe before acknowledging.",
                rid,
            ))
            .await;
            return Flow::Continue;
        };
        if seq < 0 || seq > sub.last_delivered {
            // Never acknowledge what was not delivered on this connection.
            self.send(error_frame(
                "ACK_BEYOND_DELIVERED",
                "The acknowledged position was not delivered.",
                rid,
            ))
            .await;
            return Flow::Continue;
        }
        // The acknowledgement and the DELIVERED state it implies (§52) commit together.
        let (device, principal, now) = (
            self.session.device_id,
            self.session.principal_id,
            self.state.now(),
        );
        let scope = crate::events::outbox::EventScope {
            tenant_id: self.session.tenant_id,
            application_id: self.session.application_id,
            environment_id: self.session.environment_id,
        };
        let share = self.policy.privacy.delivery_receipts;
        let r = self
            .state
            .db
            .transaction::<_, (), crate::error::Error>(move |txn| {
                Box::pin(async move {
                    db::exec(
                        txn,
                        "INSERT INTO messaging_ack_cursors (conversation_id, device_id, principal_id, acked_event_sequence, updated_at) \
                         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (conversation_id, device_id) DO UPDATE \
                           SET acked_event_sequence = GREATEST(messaging_ack_cursors.acked_event_sequence, EXCLUDED.acked_event_sequence), \
                               updated_at = EXCLUDED.updated_at",
                        vec![conversation.into(), device.into(), principal.into(), seq.into(), now.into()],
                    )
                    .await?;
                    let delivered = messages::message_sequence_at(txn, conversation, seq).await?;
                    messages::advance_delivery(txn, scope, conversation, principal, delivered, share, now).await?;
                    Ok(())
                })
            })
            .await;
        if r.is_err() {
            self.send(error_frame(
                "SERVICE_UNAVAILABLE",
                "The acknowledgement was not recorded; retry.",
                rid,
            ))
            .await;
        } else if rid.is_some() {
            self.send(json!({ "type": "ack.recorded", "conversationId": conversation, "eventSequence": seq, "requestId": rid })).await;
        }
        Flow::Continue
    }

    async fn typing(&mut self, conversation: Uuid, started: bool, rid: Option<&Value>) -> Flow {
        if !self.subs.contains_key(&conversation) {
            self.send(error_frame(
                "NOT_SUBSCRIBED",
                "Subscribe before signalling typing.",
                rid,
            ))
            .await;
            return Flow::Continue;
        }
        if !self.policy.privacy.typing_indicators || !self.session.has_scope("messages.send") {
            self.send(error_frame(
                "FEATURE_DISABLED",
                "Typing indicators are disabled by policy.",
                rid,
            ))
            .await;
            return Flow::Continue;
        }
        if started {
            if self
                .typing
                .get(&conversation)
                .is_some_and(|t| t.elapsed() < TYPING_COALESCE)
            {
                crate::metrics::inc("messaging_realtime_typing_coalesced_total", &[]);
                return Flow::Continue;
            }
            self.typing.insert(conversation, Instant::now());
            self.publish_typing(conversation, true).await;
        } else if self.typing.remove(&conversation).is_some() {
            self.publish_typing(conversation, false).await;
        }
        Flow::Continue
    }

    async fn publish_typing(&self, conversation: Uuid, started: bool) {
        let _ = self
            .state
            .realtime
            .broker
            .publish(&BrokerMessage::Typing {
                conversation_id: conversation,
                principal_id: self.session.principal_id,
                origin_device_id: self.session.device_id,
                started,
                ttl_ms: self.state.realtime.settings.typing_ttl.as_millis() as u64,
            })
            .await;
    }

    async fn presence_online(&mut self) {
        self.announced_online = presence::connected(&self.state, &self.session, &self.policy).await;
    }

    async fn presence_touch(&mut self) {
        presence::touch(&self.state, &self.session).await;
    }

    async fn presence_offline(&mut self) {
        presence::disconnected(&self.state, &self.session, &self.policy).await;
    }

    async fn presence_set(
        &mut self,
        visibility: &str,
        share_last_seen: Option<bool>,
        rid: Option<&Value>,
    ) -> Flow {
        match presence::set_visibility(
            &self.state,
            &self.session,
            &self.policy,
            visibility,
            share_last_seen,
        )
        .await
        {
            Ok(v) => {
                self.send(json!({ "type": "presence.settings", "settings": v, "requestId": rid }))
                    .await;
            }
            Err(e) => {
                self.api_error(e, rid).await;
            }
        }
        Flow::Continue
    }

    async fn presence_query(&mut self, conversation: Uuid, rid: Option<&Value>) -> Flow {
        if !self.subs.contains_key(&conversation) {
            self.send(error_frame(
                "NOT_SUBSCRIBED",
                "Subscribe before querying presence.",
                rid,
            ))
            .await;
            return Flow::Continue;
        }
        if self
            .last_presence_query
            .is_some_and(|t| t.elapsed() < PRESENCE_QUERY_EVERY)
        {
            self.send(error_frame(
                "RATE_LIMITED",
                "Presence queries are rate limited.",
                rid,
            ))
            .await;
            return Flow::Continue;
        }
        self.last_presence_query = Some(Instant::now());
        match presence::snapshot(&self.state, &self.session, &self.policy, conversation).await {
            Ok(members) => {
                self.send(json!({ "type": "presence.snapshot", "conversationId": conversation, "members": members, "requestId": rid })).await;
            }
            Err(e) => {
                self.api_error(e, rid).await;
            }
        }
        Flow::Continue
    }

    async fn refresh(&mut self, ticket: String, rid: Option<&Value>) -> Flow {
        match sessions::consume_ticket(&self.state, &ticket).await {
            Ok(next)
                if next.principal_id == self.session.principal_id
                    && next.device_id == self.session.device_id
                    && next.environment_id == self.session.environment_id =>
            {
                self.session = next;
                if let Some(ctx) = self.session.trusted_context() {
                    self.ctx = ctx;
                }
                self.send(json!({
                    "type": "auth.refreshed",
                    "sessionExpiresAt": crate::conversations::service::rfc3339(self.session.expires_at),
                    "requestId": rid,
                }))
                .await;
                Flow::Continue
            }
            Ok(_) => {
                self.send(error_frame(
                    "AUTHENTICATION_FAILED",
                    "The ticket belongs to another device.",
                    rid,
                ))
                .await;
                Flow::Close(CLOSE_AUTH, "SESSION_MISMATCH")
            }
            Err(e) => self.api_error(e, rid).await,
        }
    }

    // -----------------------------------------------------------------------------------
    // Hub events and re-validation
    // -----------------------------------------------------------------------------------

    async fn hub_event(&mut self, event: HubEvent) -> Flow {
        match event {
            HubEvent::Durable {
                conversation_id,
                event_sequence,
                ..
            } => {
                let Some(sub) = self.subs.get(&conversation_id) else {
                    return Flow::Continue;
                };
                if event_sequence <= sub.last_delivered {
                    crate::metrics::inc("messaging_realtime_duplicates_dropped_total", &[]);
                    return Flow::Continue;
                }
                self.deliver_until(conversation_id, event_sequence).await
            }
            HubEvent::Device {
                event_type,
                reference_id,
                conversation_id,
            } => {
                let t = messages::client_type(&event_type);
                if self.send(json!({ "type": "device.event", "event": { "type": t, "referenceId": reference_id, "conversationId": conversation_id } })).await {
                    Flow::Continue
                } else {
                    Flow::Close(CLOSE_SLOW, "SEND_TIMEOUT")
                }
            }
            HubEvent::ReadCursor {
                conversation_id,
                read_sequence,
            } => {
                // Only this principal's own sessions get it (routed by the hub).
                if self.send(json!({ "type": "read-cursor.synced", "conversationId": conversation_id, "readSequence": read_sequence })).await {
                    Flow::Continue
                } else {
                    Flow::Close(CLOSE_SLOW, "SEND_TIMEOUT")
                }
            }
            HubEvent::Typing {
                conversation_id,
                principal_id,
                started,
                ttl_ms,
            } => {
                // Not shown to other devices of the typing principal itself.
                if !self.subs.contains_key(&conversation_id)
                    || principal_id == self.session.principal_id
                {
                    return Flow::Continue;
                }
                self.send(json!({
                    "type": "typing",
                    "conversationId": conversation_id,
                    "principalId": principal_id,
                    "state": if started { "started" } else { "stopped" },
                    "ttlMs": ttl_ms,
                }))
                .await;
                Flow::Continue
            }
            HubEvent::Presence {
                conversation_id,
                principal_id,
                state,
                last_seen_at,
            } => {
                if self.subs.contains_key(&conversation_id) {
                    self.send(json!({
                        "type": "presence",
                        "conversationId": conversation_id,
                        "principalId": principal_id,
                        "state": &*state,
                        "lastSeenAt": last_seen_at.as_deref(),
                    }))
                    .await;
                }
                Flow::Continue
            }
            HubEvent::Resync => self.catch_up().await,
        }
    }

    /// Delivers anything committed but not yet delivered (broker loss, reconnect, lag).
    async fn catch_up(&mut self) -> Flow {
        let ids: Vec<Uuid> = self.subs.keys().copied().collect();
        if ids.is_empty() {
            return Flow::Continue;
        }
        #[derive(FromQueryResult)]
        struct Head {
            conversation_id: Uuid,
            head: i64,
        }
        let heads: Vec<Head> = match db::all(
            &self.state.db,
            "SELECT conversation_id, next_event_sequence - 1 AS head FROM messaging_conversation_sequences WHERE conversation_id = ANY($1)",
            vec![ids.into()],
        )
        .await
        {
            Ok(h) => h,
            Err(_) => return Flow::Continue,
        };
        for h in heads {
            if let Flow::Close(c, r) = self.deliver_until(h.conversation_id, h.head).await {
                return Flow::Close(c, r);
            }
        }
        Flow::Continue
    }

    async fn revalidate(&mut self) -> Flow {
        if let Err(invalid) = sessions::validate(&self.state, &self.session).await {
            if invalid == sessions::Invalid::Unavailable {
                // Fail closed only on a definitive answer; a transient outage is retried.
                return Flow::Continue;
            }
            self.send(json!({ "type": "error", "code": invalid.reason(), "message": "The connection's authorization ended." })).await;
            return Flow::Close(CLOSE_AUTH, invalid.reason());
        }
        if let Ok(p) = policy::current(&self.state.db, self.session.environment_id).await {
            self.policy = p;
        }
        // Membership may have changed on another instance without an event reaching us.
        let ids: Vec<Uuid> = self.subs.keys().copied().collect();
        if !ids.is_empty() {
            #[derive(FromQueryResult)]
            struct Active {
                conversation_id: Uuid,
            }
            if let Ok(active) = db::all::<Active, _>(
                &self.state.db,
                "SELECT conversation_id FROM messaging_conversation_members WHERE principal_id = $1 AND state = 'ACTIVE' \
                   AND conversation_id = ANY($2)",
                vec![self.session.principal_id.into(), ids.clone().into()],
            )
            .await
            {
                for c in ids {
                    if !active.iter().any(|a| a.conversation_id == c) {
                        // Events committed before the removal were authorized for this
                        // member: deliver them, ending with its own removal (which revokes
                        // the subscription), exactly as the live path does.
                        if let Some(removed_at) = self.own_removal(c).await {
                            if let Flow::Close(code, reason) = self.deliver_until(c, removed_at).await {
                                return Flow::Close(code, reason);
                            }
                        }
                        if self.subs.contains_key(&c) {
                            self.revoke_subscription(c, "MEMBERSHIP_ENDED").await;
                        }
                    }
                }
            }
        }
        self.catch_up().await
    }
}
