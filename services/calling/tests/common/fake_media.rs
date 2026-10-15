//! Test-only media provider implementing the Calling media-provider contract in memory
//! (Part C §120). It models what the contract promises — idempotent resources, join-only
//! credentials bound to one resource / identity / generation, server-side permission
//! updates, removal, termination, listing, signed callbacks — plus failure injection.
//! "Clients" join with [`FakeMediaProvider::join`], exactly as a real SDK would present a
//! credential. Never compiled into the service.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};

use async_trait::async_trait;
use calling::{
    calls::kinds::MediaEventKind,
    media::{
        provider::{
            CallMediaProvider, EnsuredMediaSession, EventVerificationError, IssuedMediaCredential,
            MediaCredentialRequest, MediaParticipantRef, MediaProviderCapabilities,
            MediaProviderError, MediaSessionRef, MediaSessionSpec, MediaToken, PermissionOutcome,
            ProviderParticipant, ProviderSessionSnapshot, RemovalOutcome, TerminationOutcome,
            VerifiedMediaEvent,
        },
        MediaPermissions,
    },
};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

const SECRET: &[u8] = b"fake-media-provider-callback-secret";

/// Operations that can be made to fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FakeOp {
    Ensure,
    Issue,
    Remove,
    UpdatePermissions,
    Terminate,
    List,
}

#[derive(Debug, Clone)]
struct Grant {
    room: String,
    identity: String,
    generation: i32,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
struct Room {
    participants: BTreeMap<String, ProviderParticipant>,
    max_participants: u32,
}

#[derive(Default)]
struct State {
    rooms: BTreeMap<String, Room>,
    grants: HashMap<String, Grant>,
    failures: HashMap<FakeOp, VecDeque<MediaProviderError>>,
    log: Vec<String>,
    ensure_calls: u64,
}

pub struct FakeMediaProvider {
    state: Mutex<State>,
    capabilities: MediaProviderCapabilities,
    next_token: AtomicU64,
    next_event: AtomicU64,
}

impl Default for FakeMediaProvider {
    fn default() -> Self {
        Self::with_capabilities(Self::default_capabilities())
    }
}

impl FakeMediaProvider {
    /// Everything an application call needs; no quality metrics.
    pub fn default_capabilities() -> MediaProviderCapabilities {
        MediaProviderCapabilities {
            session_control: true,
            credential_issuance: true,
            participant_removal: true,
            permission_update: true,
            participant_listing: true,
            verified_callbacks: true,
            quality_metrics: false,
        }
    }
}

impl FakeMediaProvider {
    pub fn with_capabilities(capabilities: MediaProviderCapabilities) -> Self {
        Self {
            state: Mutex::new(State::default()),
            capabilities,
            next_token: AtomicU64::new(1),
            next_event: AtomicU64::new(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The next `times` calls of `op` fail with `error`.
    pub fn fail_next(&self, op: FakeOp, error: MediaProviderError, times: usize) {
        let mut state = self.lock();
        let queue = state.failures.entry(op).or_default();
        for _ in 0..times {
            queue.push_back(error);
        }
    }

    pub fn clear_failures(&self) {
        self.lock().failures.clear();
    }

    fn check(&self, op: FakeOp) -> Result<(), MediaProviderError> {
        let mut state = self.lock();
        state.log.push(format!("{op:?}"));
        match state.failures.get_mut(&op).and_then(VecDeque::pop_front) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn log(&self) -> Vec<String> {
        self.lock().log.clone()
    }

    pub fn ensure_calls(&self) -> u64 {
        self.lock().ensure_calls
    }

    pub fn room_exists(&self, room: &str) -> bool {
        self.lock().rooms.contains_key(room)
    }

    pub fn rooms(&self) -> Vec<String> {
        self.lock().rooms.keys().cloned().collect()
    }

    pub fn participants(&self, room: &str) -> Vec<ProviderParticipant> {
        self.lock()
            .rooms
            .get(room)
            .map(|r| r.participants.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn participant(&self, room: &str, identity: &str) -> Option<ProviderParticipant> {
        self.lock()
            .rooms
            .get(room)
            .and_then(|r| r.participants.get(identity).cloned())
    }

    /// The grant behind a token: (room, identity, generation).
    pub fn grant_of(&self, token: &str) -> Option<(String, String, i32)> {
        self.lock()
            .grants
            .get(token)
            .map(|g| (g.room.clone(), g.identity.clone(), g.generation))
    }

    /// A client presents `token`: it joins the token's room under the token's identity
    /// with NO media permission (exactly what the contract's join credential grants).
    /// A second connection with the same identity replaces the first.
    pub fn join(&self, token: &str) -> Result<ProviderParticipant, &'static str> {
        let mut state = self.lock();
        let grant = state
            .grants
            .get(token)
            .cloned()
            .ok_or("unknown credential")?;
        if grant.expires_at <= Utc::now() {
            return Err("credential expired");
        }
        let room = state.rooms.get_mut(&grant.room).ok_or("room absent")?;
        if room.participants.len() as u32 >= room.max_participants
            && !room.participants.contains_key(&grant.identity)
        {
            return Err("room full");
        }
        let participant = ProviderParticipant {
            identity: grant.identity.clone(),
            generation: Some(grant.generation),
            permissions: MediaPermissions::NONE,
            active: true,
            published_audio: false,
            published_video: false,
            joined_at: Some(Utc::now()),
        };
        room.participants
            .insert(grant.identity.clone(), participant.clone());
        Ok(participant)
    }

    /// A joined client tries to publish: allowed only by the server-applied permissions.
    pub fn publish(&self, room: &str, identity: &str, video: bool) -> Result<(), &'static str> {
        let mut state = self.lock();
        let p = state
            .rooms
            .get_mut(room)
            .and_then(|r| r.participants.get_mut(identity))
            .ok_or("not connected")?;
        if video {
            if !p.permissions.can_publish_video {
                return Err("video publish denied");
            }
            p.published_video = true;
        } else {
            if !p.permissions.can_publish_audio {
                return Err("audio publish denied");
            }
            p.published_audio = true;
        }
        Ok(())
    }

    pub fn leave(&self, room: &str, identity: &str) {
        if let Some(r) = self.lock().rooms.get_mut(room) {
            r.participants.remove(identity);
        }
    }

    /// Injects a participant the server never issued a credential for.
    pub fn inject(&self, room: &str, participant: ProviderParticipant) {
        if let Some(r) = self.lock().rooms.get_mut(room) {
            r.participants
                .insert(participant.identity.clone(), participant);
        }
    }

    /// Injects a resource (e.g. one left behind for a terminal Call).
    pub fn inject_room(&self, room: &str) {
        self.lock()
            .rooms
            .entry(room.to_string())
            .or_insert_with(|| Room {
                participants: BTreeMap::new(),
                max_participants: 100,
            });
    }

    /// Forces a connection's applied permissions (drift injection).
    pub fn force_permissions(&self, room: &str, identity: &str, permissions: MediaPermissions) {
        if let Some(p) = self
            .lock()
            .rooms
            .get_mut(room)
            .and_then(|r| r.participants.get_mut(identity))
        {
            p.permissions = permissions;
        }
    }

    /// A signed callback as the provider would send it: (authorization, body).
    pub fn signed_event(
        &self,
        kind: &str,
        room: &str,
        participant: Option<&ProviderParticipant>,
    ) -> (String, Vec<u8>) {
        let id = format!(
            "EV_fake_{}",
            self.next_event.fetch_add(1, Ordering::Relaxed)
        );
        self.signed_event_with_id(&id, kind, room, participant)
    }

    pub fn signed_event_with_id(
        &self,
        id: &str,
        kind: &str,
        room: &str,
        participant: Option<&ProviderParticipant>,
    ) -> (String, Vec<u8>) {
        let body = json!({
            "id": id,
            "event": kind,
            "room": room,
            "participant": participant.map(|p| json!({
                "identity": p.identity,
                "generation": p.generation,
                "active": p.active,
            })),
        })
        .to_string()
        .into_bytes();
        let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
        mac.update(&body);
        (hex::encode(mac.finalize().into_bytes()), body)
    }
}

#[async_trait]
impl CallMediaProvider for FakeMediaProvider {
    fn adapter_name(&self) -> &'static str {
        "fake"
    }

    fn capabilities(&self) -> MediaProviderCapabilities {
        self.capabilities
    }

    async fn create_or_ensure_session(
        &self,
        session: &MediaSessionRef,
        spec: &MediaSessionSpec,
    ) -> Result<EnsuredMediaSession, MediaProviderError> {
        self.check(FakeOp::Ensure)?;
        let mut state = self.lock();
        state.ensure_calls += 1;
        let room = state
            .rooms
            .entry(session.name.as_str().to_string())
            .or_default();
        room.max_participants = spec.max_participants;
        Ok(EnsuredMediaSession {
            resource_ref: Some(format!("RM_fake_{}", session.name.as_str().len())),
        })
    }

    async fn issue_credentials(
        &self,
        request: &MediaCredentialRequest,
    ) -> Result<IssuedMediaCredential, MediaProviderError> {
        self.check(FakeOp::Issue)?;
        let token = format!("fake.{}", self.next_token.fetch_add(1, Ordering::Relaxed));
        let expires_at = Utc::now() + request.ttl;
        self.lock().grants.insert(
            token.clone(),
            Grant {
                room: request.session.name.as_str().to_string(),
                identity: request.identity.clone(),
                generation: request.generation,
                expires_at,
            },
        );
        Ok(IssuedMediaCredential {
            url: "wss://media.fake.invalid".into(),
            token: MediaToken::new(token),
            expires_at,
        })
    }

    async fn remove_session(
        &self,
        participant: &MediaParticipantRef,
    ) -> Result<RemovalOutcome, MediaProviderError> {
        self.check(FakeOp::Remove)?;
        let mut state = self.lock();
        match state
            .rooms
            .get_mut(participant.session.name.as_str())
            .and_then(|r| r.participants.remove(&participant.identity))
        {
            Some(_) => Ok(RemovalOutcome::Removed),
            None => Ok(RemovalOutcome::NotPresent),
        }
    }

    async fn update_permissions(
        &self,
        participant: &MediaParticipantRef,
        permissions: MediaPermissions,
    ) -> Result<PermissionOutcome, MediaProviderError> {
        if !self.capabilities.permission_update {
            return Err(MediaProviderError::Unsupported);
        }
        self.check(FakeOp::UpdatePermissions)?;
        let mut state = self.lock();
        match state
            .rooms
            .get_mut(participant.session.name.as_str())
            .and_then(|r| r.participants.get_mut(&participant.identity))
        {
            Some(p) => {
                p.permissions = permissions;
                if !permissions.can_publish_audio {
                    p.published_audio = false;
                }
                if !permissions.can_publish_video {
                    p.published_video = false;
                }
                Ok(PermissionOutcome::Applied)
            }
            None => Ok(PermissionOutcome::NotPresent),
        }
    }

    async fn terminate_session(
        &self,
        session: &MediaSessionRef,
    ) -> Result<TerminationOutcome, MediaProviderError> {
        self.check(FakeOp::Terminate)?;
        match self.lock().rooms.remove(session.name.as_str()) {
            Some(_) => Ok(TerminationOutcome::Terminated),
            None => Ok(TerminationOutcome::AlreadyAbsent),
        }
    }

    async fn list_participants(
        &self,
        session: &MediaSessionRef,
    ) -> Result<Vec<ProviderParticipant>, MediaProviderError> {
        self.check(FakeOp::List)?;
        Ok(self.participants(session.name.as_str()))
    }

    async fn reconcile(
        &self,
        session: &MediaSessionRef,
    ) -> Result<ProviderSessionSnapshot, MediaProviderError> {
        self.check(FakeOp::List)?;
        Ok(ProviderSessionSnapshot {
            exists: self.room_exists(session.name.as_str()),
            participants: self.participants(session.name.as_str()),
        })
    }

    fn verify_event(
        &self,
        authorization: Option<&str>,
        body: &[u8],
    ) -> Result<VerifiedMediaEvent, EventVerificationError> {
        let signature = authorization.ok_or(EventVerificationError::MissingSignature)?;
        let expected =
            hex::decode(signature).map_err(|_| EventVerificationError::InvalidSignature)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
        mac.update(body);
        mac.verify_slice(&expected)
            .map_err(|_| EventVerificationError::InvalidSignature)?;
        let v: Value =
            serde_json::from_slice(body).map_err(|_| EventVerificationError::Malformed)?;
        let kind = match v["event"].as_str() {
            Some("room_started") => MediaEventKind::ResourceStarted,
            Some("room_finished") => MediaEventKind::ResourceFinished,
            Some("participant_joined") => MediaEventKind::ParticipantJoined,
            Some("participant_left") => MediaEventKind::ParticipantLeft,
            Some(_) => MediaEventKind::Other,
            None => return Err(EventVerificationError::Malformed),
        };
        let participant = v["participant"].as_object().map(|p| ProviderParticipant {
            identity: p["identity"].as_str().unwrap_or_default().to_string(),
            generation: p["generation"].as_i64().map(|g| g as i32),
            permissions: MediaPermissions::NONE,
            active: p["active"].as_bool().unwrap_or(true),
            published_audio: false,
            published_video: false,
            joined_at: None,
        });
        Ok(VerifiedMediaEvent {
            event_id: v["id"]
                .as_str()
                .ok_or(EventVerificationError::Malformed)?
                .to_string(),
            kind,
            resource_name: v["room"].as_str().map(str::to_string),
            participant,
            occurred_at: None,
        })
    }
}

/// The provider's callback signature over arbitrary `body` bytes (for malformed-body
/// probes: a correctly signed body that is not a provider event).
pub fn sign_raw(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}
