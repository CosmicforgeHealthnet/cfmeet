//! A scripted, in-memory SIP / media edge for the provider contract suite and the
//! failure-injection tests. It is NOT an edge and never claims to be one (the real one is
//! certified by `sip_certification.rs`); it only lets a test decide what the edge does —
//! including the outcomes a real network produces by accident (a timeout after the
//! channel was created, an event stream that was down while a channel ended).

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use telephony::{
    domain::{EndReason, Health, LegStatus},
    sip::{
        ChannelRef, ChannelState, ChannelSummary, EdgeError, EdgeEvent, EdgeEventKind,
        EdgeOriginate, EdgeResult, SipMediaEdge,
    },
};
use tokio::sync::broadcast;

/// What the next `originate` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeScript {
    Accept,
    /// Connection refused before any byte: nothing dialled.
    Unavailable,
    /// The edge answered with a refusal: nothing dialled.
    Refused,
    /// Sent; no answer; no channel was created.
    TimeoutWithoutEffect,
    /// Sent; no answer; the channel WAS created (a paid call may exist).
    TimeoutWithEffect,
}

#[derive(Default)]
struct State {
    script: VecDeque<EdgeScript>,
    live: HashMap<ChannelRef, LegStatus>,
    ended: HashMap<ChannelRef, EndReason>,
    /// Destinations the edge actually dialled (count only — never exposed elsewhere).
    dialled: usize,
    complete_since: Option<DateTime<Utc>>,
    health: Option<Health>,
    next: u64,
    /// Part F: spool recordings started (name -> (channel, max seconds, beep)) and the
    /// spool files that exist.
    recordings: HashMap<String, (ChannelRef, u32, bool)>,
    spool: HashMap<String, Vec<u8>>,
    spool_available: bool,
}

#[derive(Clone)]
pub struct ScriptedEdge {
    state: Arc<Mutex<State>>,
    events: broadcast::Sender<EdgeEvent>,
}

impl Default for ScriptedEdge {
    fn default() -> Self {
        let state = State {
            spool_available: true,
            ..Default::default()
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            events: broadcast::channel(256).0,
        }
    }
}

impl ScriptedEdge {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn script(&self, steps: &[EdgeScript]) {
        self.lock().script.extend(steps.iter().copied());
    }

    /// The edge's event history is complete from `since` (`None`: no proof of absence).
    pub fn set_complete_since(&self, since: Option<DateTime<Utc>>) {
        self.lock().complete_since = since;
    }

    pub fn set_health(&self, health: Health) {
        self.lock().health = Some(health);
    }

    pub fn dialled(&self) -> usize {
        self.lock().dialled
    }

    pub fn live(&self) -> usize {
        self.lock().live.len()
    }

    /// The remote party ends the call; `observed = false` models an end the event stream
    /// never delivered (the edge history then does not know it either).
    pub fn end_remote(&self, channel: &str, reason: EndReason, observed: bool) {
        let mut s = self.lock();
        if s.live.remove(channel).is_some() && observed {
            s.ended.insert(channel.to_string(), reason);
        }
    }

    /// Adds a live channel (e.g. an enterprise device's dialog).
    pub fn add_live(&self, channel: &str, status: LegStatus) {
        self.lock().live.insert(channel.to_string(), status);
    }

    /// Part F: the recordings started on the edge (name -> (channel, max, beep)).
    pub fn recordings(&self) -> HashMap<String, (ChannelRef, u32, bool)> {
        self.lock().recordings.clone()
    }

    /// Part F: the edge wrote `bytes` as spool file `name` (the recording finished).
    pub fn put_spool(&self, name: &str, bytes: Vec<u8>) {
        self.lock().spool.insert(name.to_string(), bytes);
    }

    pub fn spool_files(&self) -> Vec<String> {
        let mut out: Vec<String> = self.lock().spool.keys().cloned().collect();
        out.sort();
        out
    }

    /// Part F: failure injection — the spool cannot be read / deleted.
    pub fn set_spool_unavailable(&self, unavailable: bool) {
        self.lock().spool_available = !unavailable;
    }

    /// Moves a live channel (e.g. it answered).
    pub fn set_status(&self, channel: &str, status: LegStatus) {
        if let Some(st) = self.lock().live.get_mut(channel) {
            *st = status;
        }
    }

    /// Emits an edge event for a channel.
    pub fn emit(&self, channel: &str, kind: EdgeEventKind, at: DateTime<Utc>) {
        let n = {
            let mut s = self.lock();
            s.next += 1;
            s.next
        };
        let _ = self.events.send(EdgeEvent {
            event_id: format!("scripted-{n}"),
            channel: channel.to_string(),
            application_media: false,
            correlation_key: Some(channel.to_string()),
            kind,
            at,
        });
    }

    fn control(&self, channel: &ChannelRef, to: Option<LegStatus>) -> EdgeResult<()> {
        let mut s = self.lock();
        match (s.live.get_mut(channel), to) {
            (None, _) => Err(EdgeError::NotFound),
            (Some(_), None) => {
                s.live.remove(channel);
                s.ended.insert(channel.clone(), EndReason::Completed);
                Ok(())
            }
            (Some(st), Some(LegStatus::Held)) if *st == LegStatus::Connected => {
                *st = LegStatus::Held;
                Ok(())
            }
            (Some(st), Some(LegStatus::Connected)) if *st == LegStatus::Held => {
                *st = LegStatus::Connected;
                Ok(())
            }
            (Some(_), Some(_)) => Err(EdgeError::Refused),
        }
    }
}

#[async_trait]
impl SipMediaEdge for ScriptedEdge {
    fn name(&self) -> &'static str {
        "scripted"
    }

    async fn health(&self) -> Health {
        self.lock().health.unwrap_or(Health::Healthy)
    }

    async fn originate(&self, request: &EdgeOriginate) -> EdgeResult<ChannelRef> {
        let mut s = self.lock();
        let step = s.script.pop_front().unwrap_or(EdgeScript::Accept);
        let channel = request.correlation_key.clone();
        let create = |s: &mut State| {
            s.dialled += 1;
            s.live.insert(channel.clone(), LegStatus::Originating);
        };
        match step {
            EdgeScript::Accept => {
                create(&mut s);
                Ok(channel)
            }
            EdgeScript::Unavailable => Err(EdgeError::Unavailable),
            EdgeScript::Refused => Err(EdgeError::Refused),
            EdgeScript::TimeoutWithoutEffect => Err(EdgeError::Timeout),
            EdgeScript::TimeoutWithEffect => {
                create(&mut s);
                Err(EdgeError::Timeout)
            }
        }
    }

    async fn ring(&self, channel: &ChannelRef) -> EdgeResult<()> {
        if self.lock().live.contains_key(channel) {
            Ok(())
        } else {
            Err(EdgeError::NotFound)
        }
    }

    async fn answer(&self, channel: &ChannelRef) -> EdgeResult<()> {
        match self.lock().live.get_mut(channel) {
            Some(st) => {
                *st = LegStatus::Connected;
                Ok(())
            }
            None => Err(EdgeError::NotFound),
        }
    }

    async fn hangup(&self, channel: &ChannelRef) -> EdgeResult<()> {
        self.control(channel, None)
    }

    async fn create_bridge(&self, name: &str) -> EdgeResult<String> {
        Ok(name.to_string())
    }

    async fn add_to_bridge(&self, _: &str, channel: &ChannelRef) -> EdgeResult<()> {
        self.ring(channel).await
    }

    async fn remove_from_bridge(&self, _: &str, _: &ChannelRef) -> EdgeResult<()> {
        Ok(())
    }

    async fn destroy_bridge(&self, _: &str) -> EdgeResult<()> {
        Ok(())
    }

    async fn hold(&self, channel: &ChannelRef) -> EdgeResult<()> {
        self.control(channel, Some(LegStatus::Held))
    }

    async fn resume(&self, channel: &ChannelRef) -> EdgeResult<()> {
        self.control(channel, Some(LegStatus::Connected))
    }

    async fn record(
        &self,
        channel: &ChannelRef,
        name: &str,
        max_seconds: u32,
        beep: bool,
    ) -> EdgeResult<()> {
        let mut s = self.lock();
        if !s.live.contains_key(channel) {
            return Err(EdgeError::NotFound);
        }
        s.recordings
            .insert(name.to_string(), (channel.clone(), max_seconds, beep));
        Ok(())
    }

    async fn fetch_recording(&self, name: &str, max_bytes: usize) -> EdgeResult<Vec<u8>> {
        let s = self.lock();
        if !s.spool_available {
            return Err(EdgeError::Unavailable);
        }
        match s.spool.get(name) {
            Some(b) if b.len() > max_bytes => Err(EdgeError::Refused),
            Some(b) => Ok(b.clone()),
            None => Err(EdgeError::NotFound),
        }
    }

    async fn delete_recording(&self, name: &str) -> EdgeResult<()> {
        let mut s = self.lock();
        if !s.spool_available {
            return Err(EdgeError::Unavailable);
        }
        s.spool.remove(name);
        Ok(())
    }

    async fn send_dtmf(&self, channel: &ChannelRef, _: &str) -> EdgeResult<()> {
        self.ring(channel).await
    }

    async fn redirect(&self, channel: &ChannelRef, _: &str) -> EdgeResult<()> {
        self.ring(channel).await
    }

    async fn query(&self, channel: &ChannelRef) -> ChannelState {
        match self.lock().live.get(channel) {
            Some(st) => ChannelState::Active(*st),
            None => ChannelState::Gone,
        }
    }

    async fn find_by_correlation(
        &self,
        correlation_key: &str,
    ) -> EdgeResult<Option<ChannelSummary>> {
        let s = self.lock();
        if let Some(st) = s.live.get(correlation_key) {
            return Ok(Some(ChannelSummary {
                channel: correlation_key.to_string(),
                correlation_key: Some(correlation_key.to_string()),
                status: *st,
            }));
        }
        Ok(s.ended.get(correlation_key).map(|reason| ChannelSummary {
            channel: correlation_key.to_string(),
            correlation_key: Some(correlation_key.to_string()),
            status: if *reason == EndReason::Completed {
                LegStatus::Ended
            } else {
                LegStatus::Failed
            },
        }))
    }

    async fn list(&self) -> EdgeResult<Vec<ChannelSummary>> {
        Ok(self
            .lock()
            .live
            .iter()
            .map(|(c, st)| ChannelSummary {
                channel: c.clone(),
                correlation_key: Some(c.clone()),
                status: *st,
            })
            .collect())
    }

    fn history_complete_since(&self) -> Option<DateTime<Utc>> {
        self.lock().complete_since
    }

    fn subscribe(&self) -> broadcast::Receiver<EdgeEvent> {
        self.events.subscribe()
    }
}
