//! In-process mock of the LiveKit RoomService (Twirp/protobuf over HTTP) used by the
//! Part E "mocked provider" suites. It speaks the same wire protocol as livekit-server:
//! requests are protobuf-decoded with the official `livekit-protocol` types, admin
//! calls require a JWT signed with the API secret carrying the right video grants, and
//! errors use Twirp JSON bodies.
//!
//! It also simulates media clients: `connect(credential)` verifies a CF Meet-issued
//! participant credential exactly like the SFU would (signature, room, grants) and
//! `publish` enforces the credential's publish grants — so a modified client cannot
//! publish what its grants do not allow. Fault injection covers outages, timeouts and
//! a delete that silently does nothing.
#![allow(dead_code)]

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use livekit_api::access_token::{AccessToken, TokenVerifier};
use livekit_protocol as proto;
use prost::Message;
use serde_json::json;
use sha2::{Digest, Sha256};

pub const API_KEY: &str = "mock-key";
pub const API_SECRET: &str = "mock-secret-mock-secret-mock-secret-0123";

#[derive(Clone, Debug)]
pub struct MockParticipant {
    pub sid: String,
    pub identity: String,
    pub permission: proto::ParticipantPermission,
    pub tracks: Vec<proto::TrackInfo>,
}

#[derive(Clone, Debug)]
pub struct MockRoom {
    pub sid: String,
    pub name: String,
    pub participants: BTreeMap<String, MockParticipant>,
}

#[derive(Default)]
pub struct Faults {
    pub fail_all: AtomicBool,
    pub fail_create: AtomicBool,
    pub fail_delete: AtomicBool,
    /// DeleteRoom answers OK but the room stays (forces "absence not confirmed").
    pub delete_noop: AtomicBool,
    pub fail_remove: AtomicBool,
    pub fail_update: AtomicBool,
    /// Respond after this many milliseconds (timeout testing).
    pub delay_ms: AtomicU64,
}

#[derive(Default)]
pub struct MockLiveKit {
    pub rooms: Mutex<HashMap<String, MockRoom>>,
    pub calls: Mutex<Vec<(String, String, String)>>,
    pub faults: Faults,
    seq: AtomicU64,
    pub url: Mutex<String>,
}

fn twirp_error(status: StatusCode, code: &str, msg: &str) -> Response {
    (
        status,
        [("content-type", "application/json")],
        json!({ "code": code, "msg": msg }).to_string(),
    )
        .into_response()
}

fn ok<M: Message>(m: M) -> Response {
    (
        StatusCode::OK,
        [("content-type", "application/protobuf")],
        m.encode_to_vec(),
    )
        .into_response()
}

fn source_name(source: i32) -> &'static str {
    match proto::TrackSource::try_from(source).unwrap_or(proto::TrackSource::Unknown) {
        proto::TrackSource::Camera => "camera",
        proto::TrackSource::Microphone => "microphone",
        proto::TrackSource::ScreenShare => "screen_share",
        proto::TrackSource::ScreenShareAudio => "screen_share_audio",
        proto::TrackSource::Unknown => "unknown",
    }
}

impl MockLiveKit {
    fn next(&self, prefix: &str) -> String {
        format!("{prefix}_{}", self.seq.fetch_add(1, Ordering::SeqCst))
    }

    pub fn ws_url(&self) -> String {
        self.url.lock().unwrap().replace("http://", "ws://")
    }

    pub fn http_url(&self) -> String {
        self.url.lock().unwrap().clone()
    }

    pub fn calls(&self, method: &str) -> Vec<(String, String)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _, _)| m == method)
            .map(|(_, r, i)| (r.clone(), i.clone()))
            .collect()
    }

    pub fn call_count(&self, method: &str) -> usize {
        self.calls(method).len()
    }

    pub fn has_room(&self, name: &str) -> bool {
        self.rooms.lock().unwrap().contains_key(name)
    }

    pub fn room_names(&self) -> Vec<String> {
        self.rooms.lock().unwrap().keys().cloned().collect()
    }

    pub fn participant(&self, room: &str, identity: &str) -> Option<MockParticipant> {
        self.rooms
            .lock()
            .unwrap()
            .get(room)
            .and_then(|r| r.participants.get(identity).cloned())
    }

    pub fn identities(&self, room: &str) -> Vec<String> {
        self.rooms
            .lock()
            .unwrap()
            .get(room)
            .map(|r| r.participants.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Create a room out-of-band (orphan / unmapped room scenarios).
    pub fn insert_room(&self, name: &str) {
        let sid = self.next("RM");
        self.rooms.lock().unwrap().insert(
            name.to_string(),
            MockRoom {
                sid,
                name: name.to_string(),
                participants: BTreeMap::new(),
            },
        );
    }

    /// Delete a room out-of-band (provider-side loss).
    pub fn drop_room(&self, name: &str) {
        self.rooms.lock().unwrap().remove(name);
    }

    /// Insert a participant out-of-band (rogue / unknown provider identity).
    pub fn insert_participant(
        &self,
        room: &str,
        identity: &str,
        permission: proto::ParticipantPermission,
    ) {
        let sid = self.next("PA");
        if let Some(r) = self.rooms.lock().unwrap().get_mut(room) {
            r.participants.insert(
                identity.to_string(),
                MockParticipant {
                    sid,
                    identity: identity.to_string(),
                    permission,
                    tracks: vec![],
                },
            );
        }
    }

    /// Simulated SFU connect: verify the credential like livekit-server does.
    pub fn connect(&self, credential: &str) -> Result<String, String> {
        let claims = TokenVerifier::with_api_key(API_KEY, API_SECRET)
            .verify(credential)
            .map_err(|e| format!("invalid credential: {e}"))?;
        if !claims.video.room_join {
            return Err("credential has no roomJoin grant".into());
        }
        let mut rooms = self.rooms.lock().unwrap();
        let room = rooms
            .get_mut(&claims.video.room)
            .ok_or_else(|| "room does not exist".to_string())?;
        let sources = claims
            .video
            .can_publish_sources
            .iter()
            .map(|s| match s.as_str() {
                "camera" => proto::TrackSource::Camera as i32,
                "microphone" => proto::TrackSource::Microphone as i32,
                "screen_share" => proto::TrackSource::ScreenShare as i32,
                "screen_share_audio" => proto::TrackSource::ScreenShareAudio as i32,
                _ => proto::TrackSource::Unknown as i32,
            })
            .collect();
        let sid = format!("PA_{}", claims.sub);
        room.participants.insert(
            claims.sub.clone(),
            MockParticipant {
                sid,
                identity: claims.sub.clone(),
                permission: proto::ParticipantPermission {
                    can_subscribe: claims.video.can_subscribe,
                    can_publish: claims.video.can_publish,
                    can_publish_data: claims.video.can_publish_data,
                    can_publish_sources: sources,
                    ..Default::default()
                },
                tracks: vec![],
            },
        );
        Ok(claims.sub)
    }

    /// Simulated client publish; the SFU enforces the participant's CURRENT grants.
    pub fn publish(
        &self,
        room: &str,
        identity: &str,
        source: proto::TrackSource,
    ) -> Result<String, String> {
        let mut rooms = self.rooms.lock().unwrap();
        let p = rooms
            .get_mut(room)
            .and_then(|r| r.participants.get_mut(identity))
            .ok_or_else(|| "not connected".to_string())?;
        let allowed = p.permission.can_publish
            && (p.permission.can_publish_sources.is_empty()
                || p.permission.can_publish_sources.contains(&(source as i32)));
        if !allowed {
            return Err(format!(
                "permission denied: cannot publish {}",
                source_name(source as i32)
            ));
        }
        let sid = format!("TR_{}_{}", identity, p.tracks.len());
        let kind = match source {
            proto::TrackSource::Microphone | proto::TrackSource::ScreenShareAudio => {
                proto::TrackType::Audio
            }
            _ => proto::TrackType::Video,
        };
        p.tracks.push(proto::TrackInfo {
            sid: sid.clone(),
            r#type: kind as i32,
            source: source as i32,
            muted: false,
            ..Default::default()
        });
        Ok(sid)
    }

    /// Simulated client disconnect (no provider admin call).
    pub fn disconnect(&self, room: &str, identity: &str) {
        if let Some(r) = self.rooms.lock().unwrap().get_mut(room) {
            r.participants.remove(identity);
        }
    }

    #[allow(clippy::result_large_err)]
    fn authorize(&self, headers: &HeaderMap, need: &str, room: &str) -> Result<(), Response> {
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| {
                twirp_error(StatusCode::UNAUTHORIZED, "unauthenticated", "missing token")
            })?;
        let claims = TokenVerifier::with_api_key(API_KEY, API_SECRET)
            .verify(token)
            .map_err(|_| {
                twirp_error(StatusCode::UNAUTHORIZED, "unauthenticated", "invalid token")
            })?;
        let v = claims.video;
        let granted = match need {
            "create" => v.room_create,
            "list" => v.room_list,
            "admin" => v.room_admin && (v.room == room),
            _ => false,
        };
        if granted {
            Ok(())
        } else {
            Err(twirp_error(
                StatusCode::FORBIDDEN,
                "permission_denied",
                "insufficient grants",
            ))
        }
    }

    fn room_proto(r: &MockRoom) -> proto::Room {
        proto::Room {
            sid: r.sid.clone(),
            name: r.name.clone(),
            num_participants: r.participants.len() as u32,
            ..Default::default()
        }
    }

    fn participant_proto(p: &MockParticipant) -> proto::ParticipantInfo {
        proto::ParticipantInfo {
            sid: p.sid.clone(),
            identity: p.identity.clone(),
            state: proto::participant_info::State::Active as i32,
            tracks: p.tracks.clone(),
            permission: Some(p.permission.clone()),
            ..Default::default()
        }
    }

    async fn handle(self: Arc<Self>, method: String, headers: HeaderMap, body: Bytes) -> Response {
        let delay = self.faults.delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        if self.faults.fail_all.load(Ordering::SeqCst) {
            return twirp_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "injected outage",
            );
        }
        macro_rules! decode {
            ($t:ty) => {
                match <$t>::decode(body.clone()) {
                    Ok(v) => v,
                    Err(_) => {
                        return twirp_error(StatusCode::BAD_REQUEST, "malformed", "bad protobuf")
                    }
                }
            };
        }
        let log = |room: &str, identity: &str| {
            self.calls.lock().unwrap().push((
                method.clone(),
                room.to_string(),
                identity.to_string(),
            ));
        };
        match method.as_str() {
            "CreateRoom" => {
                let req = decode!(proto::CreateRoomRequest);
                if let Err(r) = self.authorize(&headers, "create", &req.name) {
                    return r;
                }
                log(&req.name, "");
                if self.faults.fail_create.load(Ordering::SeqCst) {
                    return twirp_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "unavailable",
                        "create failed",
                    );
                }
                let mut rooms = self.rooms.lock().unwrap();
                let room = rooms.entry(req.name.clone()).or_insert_with(|| MockRoom {
                    sid: format!("RM_{}", self.seq.fetch_add(1, Ordering::SeqCst)),
                    name: req.name.clone(),
                    participants: BTreeMap::new(),
                });
                ok(Self::room_proto(room))
            }
            "ListRooms" => {
                let req = decode!(proto::ListRoomsRequest);
                if let Err(r) = self.authorize(&headers, "list", "") {
                    return r;
                }
                log(&req.names.join(","), "");
                let rooms = self.rooms.lock().unwrap();
                ok(proto::ListRoomsResponse {
                    rooms: rooms
                        .values()
                        .filter(|r| req.names.is_empty() || req.names.contains(&r.name))
                        .map(Self::room_proto)
                        .collect(),
                })
            }
            "DeleteRoom" => {
                let req = decode!(proto::DeleteRoomRequest);
                if let Err(r) = self.authorize(&headers, "create", &req.room) {
                    return r;
                }
                log(&req.room, "");
                if self.faults.fail_delete.load(Ordering::SeqCst) {
                    return twirp_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "unavailable",
                        "delete failed",
                    );
                }
                if self.faults.delete_noop.load(Ordering::SeqCst) {
                    return ok(proto::DeleteRoomResponse {});
                }
                match self.rooms.lock().unwrap().remove(&req.room) {
                    Some(_) => ok(proto::DeleteRoomResponse {}),
                    None => twirp_error(StatusCode::NOT_FOUND, "not_found", "room not found"),
                }
            }
            "ListParticipants" => {
                let req = decode!(proto::ListParticipantsRequest);
                if let Err(r) = self.authorize(&headers, "admin", &req.room) {
                    return r;
                }
                log(&req.room, "");
                let rooms = self.rooms.lock().unwrap();
                match rooms.get(&req.room) {
                    Some(r) => ok(proto::ListParticipantsResponse {
                        participants: r
                            .participants
                            .values()
                            .map(Self::participant_proto)
                            .collect(),
                    }),
                    None => twirp_error(StatusCode::NOT_FOUND, "not_found", "room not found"),
                }
            }
            "GetParticipant" => {
                let req = decode!(proto::RoomParticipantIdentity);
                if let Err(r) = self.authorize(&headers, "admin", &req.room) {
                    return r;
                }
                log(&req.room, &req.identity);
                match self.participant(&req.room, &req.identity) {
                    Some(p) => ok(Self::participant_proto(&p)),
                    None => {
                        twirp_error(StatusCode::NOT_FOUND, "not_found", "participant not found")
                    }
                }
            }
            "RemoveParticipant" => {
                let req = decode!(proto::RoomParticipantIdentity);
                if let Err(r) = self.authorize(&headers, "admin", &req.room) {
                    return r;
                }
                log(&req.room, &req.identity);
                if self.faults.fail_remove.load(Ordering::SeqCst) {
                    return twirp_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "unavailable",
                        "remove failed",
                    );
                }
                let mut rooms = self.rooms.lock().unwrap();
                match rooms
                    .get_mut(&req.room)
                    .and_then(|r| r.participants.remove(&req.identity))
                {
                    Some(_) => ok(proto::RemoveParticipantResponse {}),
                    None => {
                        twirp_error(StatusCode::NOT_FOUND, "not_found", "participant not found")
                    }
                }
            }
            "UpdateParticipant" => {
                let req = decode!(proto::UpdateParticipantRequest);
                if let Err(r) = self.authorize(&headers, "admin", &req.room) {
                    return r;
                }
                log(&req.room, &req.identity);
                if self.faults.fail_update.load(Ordering::SeqCst) {
                    return twirp_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "unavailable",
                        "update failed",
                    );
                }
                let mut rooms = self.rooms.lock().unwrap();
                let Some(p) = rooms
                    .get_mut(&req.room)
                    .and_then(|r| r.participants.get_mut(&req.identity))
                else {
                    return twirp_error(
                        StatusCode::NOT_FOUND,
                        "not_found",
                        "participant not found",
                    );
                };
                if let Some(perm) = req.permission {
                    // Losing a source unpublishes its tracks, as the SFU does.
                    let allowed = |t: &proto::TrackInfo| {
                        perm.can_publish
                            && (perm.can_publish_sources.is_empty()
                                || perm.can_publish_sources.contains(&t.source))
                    };
                    p.tracks.retain(allowed);
                    p.permission = perm;
                }
                ok(Self::participant_proto(p))
            }
            "MutePublishedTrack" => {
                let req = decode!(proto::MuteRoomTrackRequest);
                if let Err(r) = self.authorize(&headers, "admin", &req.room) {
                    return r;
                }
                log(&req.room, &req.identity);
                let mut rooms = self.rooms.lock().unwrap();
                let Some(t) = rooms
                    .get_mut(&req.room)
                    .and_then(|r| r.participants.get_mut(&req.identity))
                    .and_then(|p| p.tracks.iter_mut().find(|t| t.sid == req.track_sid))
                else {
                    return twirp_error(StatusCode::NOT_FOUND, "not_found", "track not found");
                };
                t.muted = req.muted;
                ok(proto::MuteRoomTrackResponse {
                    track: Some(t.clone()),
                })
            }
            _ => twirp_error(StatusCode::NOT_FOUND, "bad_route", "unknown method"),
        }
    }
}

async fn route(
    State(mock): State<Arc<MockLiveKit>>,
    Path(method): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    mock.handle(method, headers, body).await
}

/// Start the mock on an ephemeral port.
pub async fn start() -> Arc<MockLiveKit> {
    let mock = Arc::new(MockLiveKit::default());
    let app = Router::new()
        .route("/twirp/livekit.RoomService/:method", post(route))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    *mock.url.lock().unwrap() = format!("http://{addr}");
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    mock
}

/// Sign a LiveKit-style webhook body (Authorization header value).
pub fn sign_webhook(body: &str, key: &str, secret: &str) -> String {
    use base64::Engine;
    let sha = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(body.as_bytes()));
    AccessToken::with_api_key(key, secret)
        .with_sha256(&sha)
        .to_jwt()
        .unwrap()
}

/// LiveKit webhook JSON (protojson camelCase, as livekit-server sends it).
pub fn webhook_body(
    event: &str,
    id: &str,
    room: &str,
    room_sid: Option<&str>,
    identity: Option<&str>,
) -> String {
    let mut v = json!({
        "event": event,
        "id": id,
        "createdAt": chrono::Utc::now().timestamp().to_string(),
        "room": { "name": room },
    });
    if let Some(sid) = room_sid {
        v["room"]["sid"] = json!(sid);
    }
    if let Some(i) = identity {
        v["participant"] = json!({ "identity": i, "sid": format!("PA_{i}"), "state": "ACTIVE" });
    }
    v.to_string()
}

/// Track webhook (`track_published` / `track_unpublished`) as livekit-server sends it.
pub fn track_webhook_body(
    event: &str,
    id: &str,
    room: &str,
    identity: &str,
    track_sid: &str,
    source: &str,
) -> String {
    let mut v: serde_json::Value =
        serde_json::from_str(&webhook_body(event, id, room, None, Some(identity))).unwrap();
    let kind = if source == "MICROPHONE" || source == "SCREEN_SHARE_AUDIO" {
        "AUDIO"
    } else {
        "VIDEO"
    };
    v["track"] = json!({ "sid": track_sid, "source": source, "type": kind });
    v.to_string()
}

impl MockLiveKit {
    /// Simulated client unpublish (no admin call).
    pub fn unpublish(&self, room: &str, identity: &str, sid: &str) -> bool {
        let mut rooms = self.rooms.lock().unwrap();
        match rooms
            .get_mut(room)
            .and_then(|r| r.participants.get_mut(identity))
        {
            Some(p) => {
                let before = p.tracks.len();
                p.tracks.retain(|t| t.sid != sid);
                before != p.tracks.len()
            }
            None => false,
        }
    }

    /// Tracks of `source` currently published (and whether muted).
    pub fn tracks_of(
        &self,
        room: &str,
        identity: &str,
        source: proto::TrackSource,
    ) -> Vec<(String, bool)> {
        self.participant(room, identity)
            .map(|p| {
                p.tracks
                    .iter()
                    .filter(|t| t.source == source as i32)
                    .map(|t| (t.sid.clone(), t.muted))
                    .collect()
            })
            .unwrap_or_default()
    }
}
