//! The deterministic synthetic numbering provider (Part IV Workstream G §13–§14).
//!
//! An independent, in-memory simulation of a provider's side: its own inventory, holds,
//! provisioning, routes, suspension, release and a monotonic per-number version. It is
//! keyed by the request id it receives (provider-side idempotency): a repeated request
//! returns the original result and never acts twice.
//!
//! * Synthetic inventory only (`+1 555 01xx xxxx`, fictional, not PSTN-routable); the
//!   registry refuses it in staging / production.
//! * Faults are scripted per operation ([`Fault`]) and consumed in order, so every failure
//!   scenario is reproducible and isolated per provider instance (never random).
//! * State changes emit signed callbacks (the `cf_webhooks` HMAC scheme) collected by
//!   [`DeterministicProvider::take_callbacks`]; tests deliver them duplicated, delayed,
//!   reordered or tampered.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    registry::SYNTHETIC_PROVIDER, CallbackRejection, CallbackRequest, InventoryOffer,
    InventoryQuery, NormalizedProviderEvent, NumberingProvider, OperationKind, OperationReceipt,
    OperationRequest, ProviderDescriptor, ProviderEventKind, ProviderFailure, ProviderHealth,
    ProviderNumberState, ProviderNumberStatus as St, ProviderOperation as Op, ProviderOutcome,
};
use crate::{
    domain::SharedClock,
    numbers::{Capability as C, FailureCategory, NumberType},
};

/// Callback timestamps outside this window are refused (replay protection).
pub const CALLBACK_TOLERANCE_SECONDS: i64 = 300;

/// A scripted fault, consumed by the next call of its operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Nothing reached the provider (definite, safe to retry).
    TimeoutBeforeTransmission,
    /// The provider executed the request, the answer was lost (UNKNOWN).
    TimeoutAfterExecution,
    /// The request was lost before the provider acted (UNKNOWN to the caller).
    LostRequest,
    /// Another account already owns the number.
    AlreadyOwned,
    /// The number left the provider's inventory.
    InventoryDisappears,
    /// A concurrent hold won.
    ReservationConflict,
    /// Provisioning half-applied (only the first capability), answer lost (UNKNOWN).
    PartialProvision,
    /// The next read-back returns the state before the latest change.
    StaleReadBack,
    /// A definite rejection with this code.
    Reject(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SimNumber {
    external_ref: String,
    e164: String,
    region: Option<String>,
    number_type: NumberType,
    capabilities: Vec<C>,
    unsupported: Vec<C>,
    kyc_required: bool,
    status: St,
    owned_by_us: bool,
    held_by_request: Option<Uuid>,
    hold_expires_at: Option<DateTime<Utc>>,
    provisioned: Vec<C>,
    voice_route: Option<Uuid>,
    sms_route: Option<Uuid>,
    sequence: i64,
    previous: Option<Box<SimNumber>>,
}

#[derive(Default)]
struct Sim {
    numbers: BTreeMap<String, SimNumber>,
    /// Provider-side idempotency: (request id, operation) → original outcome.
    processed: HashMap<(Uuid, Op), ProviderOutcome<OperationReceipt>>,
    faults: HashMap<Op, VecDeque<Fault>>,
    outage: bool,
    calls: Vec<(Op, Option<Uuid>, &'static str)>,
    callbacks: Vec<CallbackRequest>,
    next_event: u64,
    next_delivery: u64,
}

/// The deterministic provider (cloneable handle onto one simulated provider).
#[derive(Clone)]
pub struct DeterministicProvider {
    sim: Arc<Mutex<Sim>>,
    clock: SharedClock,
    secret: Arc<Vec<u8>>,
}

/// One synthetic inventory item.
#[derive(Debug, Clone)]
pub struct SyntheticNumber {
    pub external_ref: &'static str,
    pub suffix: u32,
    pub region: Option<&'static str>,
    pub number_type: NumberType,
    pub capabilities: &'static [C],
    pub unsupported: &'static [C],
    pub kyc_required: bool,
    pub available: bool,
}

/// The standard synthetic US inventory: every capability variation the certification
/// suite needs.
pub const STANDARD_INVENTORY: &[SyntheticNumber] = &[
    SyntheticNumber {
        external_ref: "syn-voice-only",
        suffix: 1,
        region: Some("US-CA"),
        number_type: NumberType::Geographic,
        capabilities: &[C::VoiceInbound, C::VoiceOutbound, C::GeographicLocal],
        unsupported: &[C::SmsInbound, C::SmsOutbound, C::MobileNumberClass],
        kyc_required: false,
        available: true,
    },
    SyntheticNumber {
        external_ref: "syn-sms-only",
        suffix: 2,
        region: None,
        number_type: NumberType::National,
        capabilities: &[C::SmsInbound, C::SmsOutbound],
        unsupported: &[C::VoiceInbound, C::VoiceOutbound, C::MobileNumberClass],
        kyc_required: false,
        available: true,
    },
    SyntheticNumber {
        external_ref: "syn-voice-sms-mobile",
        suffix: 3,
        region: None,
        number_type: NumberType::Mobile,
        capabilities: &[
            C::VoiceInbound,
            C::VoiceOutbound,
            C::SmsInbound,
            C::SmsOutbound,
            C::MobileNumberClass,
        ],
        unsupported: &[C::MmsRich],
        kyc_required: false,
        available: true,
    },
    SyntheticNumber {
        external_ref: "syn-inbound-only",
        suffix: 4,
        region: Some("US-NY"),
        number_type: NumberType::Geographic,
        capabilities: &[C::VoiceInbound, C::SmsInbound, C::GeographicLocal],
        unsupported: &[C::VoiceOutbound, C::SmsOutbound],
        kyc_required: false,
        available: true,
    },
    SyntheticNumber {
        external_ref: "syn-toll-free",
        suffix: 5,
        region: None,
        number_type: NumberType::TollFree,
        capabilities: &[C::VoiceInbound, C::TollFreeBusiness],
        unsupported: &[C::SmsInbound, C::SmsOutbound, C::VoiceOutbound],
        kyc_required: false,
        available: true,
    },
    SyntheticNumber {
        external_ref: "syn-kyc-mobile",
        suffix: 6,
        region: None,
        number_type: NumberType::Mobile,
        capabilities: &[
            C::VoiceInbound,
            C::VoiceOutbound,
            C::SmsInbound,
            C::SmsOutbound,
            C::MobileNumberClass,
        ],
        unsupported: &[],
        kyc_required: true,
        available: true,
    },
    SyntheticNumber {
        external_ref: "syn-restricted",
        suffix: 7,
        region: None,
        number_type: NumberType::Geographic,
        capabilities: &[C::VoiceInbound, C::GeographicLocal],
        unsupported: &[],
        kyc_required: false,
        available: false,
    },
    SyntheticNumber {
        external_ref: "syn-voice-sms-geo",
        suffix: 8,
        region: Some("US-TX"),
        number_type: NumberType::Geographic,
        capabilities: &[
            C::VoiceInbound,
            C::VoiceOutbound,
            C::SmsInbound,
            C::GeographicLocal,
        ],
        unsupported: &[C::SmsOutbound, C::MobileNumberClass, C::MmsRich],
        kyc_required: false,
        available: true,
    },
];

fn not_found(external_ref: &str) -> ProviderOutcome<OperationReceipt> {
    let _ = external_ref;
    ProviderOutcome::Failed(ProviderFailure::rejected(
        "NOT_FOUND",
        FailureCategory::NotObtainable,
    ))
}

fn reject(code: &str, category: FailureCategory) -> ProviderOutcome<OperationReceipt> {
    ProviderOutcome::Failed(ProviderFailure::rejected(code, category))
}

fn refuse(code: &str, category: FailureCategory) -> ProviderFailure {
    ProviderFailure::rejected(code, category)
}

impl SimNumber {
    fn state(&self, now: DateTime<Utc>) -> ProviderNumberState {
        ProviderNumberState {
            external_ref: self.external_ref.clone(),
            status: self.status,
            held_by_request: self.held_by_request,
            owned_by_us: self.owned_by_us,
            capabilities: if matches!(self.status, St::Provisioned | St::Suspended) {
                self.provisioned.clone()
            } else {
                self.capabilities.clone()
            },
            voice_route: self.voice_route,
            sms_route: self.sms_route,
            hold_expires_at: self.hold_expires_at,
            sequence: self.sequence,
            observed_at: now,
        }
    }

    /// Records the pre-change snapshot and bumps the version.
    fn change(&mut self) {
        let mut snapshot = self.clone();
        snapshot.previous = None;
        self.previous = Some(Box::new(snapshot));
        self.sequence += 1;
    }
}

impl DeterministicProvider {
    /// A provider with the standard synthetic inventory. `callback_secret` signs its
    /// callbacks (test material only).
    pub fn new(clock: SharedClock, callback_secret: &[u8]) -> Self {
        let p = Self::empty(clock, callback_secret);
        for n in STANDARD_INVENTORY {
            p.add_number(n.clone());
        }
        p
    }

    pub fn empty(clock: SharedClock, callback_secret: &[u8]) -> Self {
        Self {
            sim: Arc::new(Mutex::new(Sim::default())),
            clock,
            secret: Arc::new(callback_secret.to_vec()),
        }
    }

    pub fn add_number(&self, n: SyntheticNumber) {
        let mut sim = self.sim.lock().unwrap();
        sim.numbers.insert(
            n.external_ref.to_string(),
            SimNumber {
                external_ref: n.external_ref.to_string(),
                e164: format!("+1555010{:04}", n.suffix),
                region: n.region.map(str::to_string),
                number_type: n.number_type,
                capabilities: n.capabilities.to_vec(),
                unsupported: n.unsupported.to_vec(),
                kyc_required: n.kyc_required,
                status: if n.available {
                    St::Available
                } else {
                    St::Unavailable
                },
                owned_by_us: false,
                held_by_request: None,
                hold_expires_at: None,
                provisioned: vec![],
                voice_route: None,
                sms_route: None,
                sequence: 1,
                previous: None,
            },
        );
    }

    /// Scripts the next call(s) of `op`.
    pub fn inject(&self, op: Op, fault: Fault) {
        self.sim
            .lock()
            .unwrap()
            .faults
            .entry(op)
            .or_default()
            .push_back(fault);
    }

    /// Provider outage on / off: every call fails before transmission, read-back unknown.
    pub fn set_outage(&self, outage: bool) {
        self.sim.lock().unwrap().outage = outage;
    }

    /// `(operation, request id, outcome)` of every call received, in order.
    pub fn calls(&self) -> Vec<(Op, Option<Uuid>, &'static str)> {
        self.sim.lock().unwrap().calls.clone()
    }

    /// How many times `op` was EXECUTED for `request` (provider-side effects).
    pub fn executions(&self, op: Op, request: Uuid) -> usize {
        self.sim
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(o, r, outcome)| *o == op && *r == Some(request) && *outcome == "EXECUTED")
            .count()
    }

    /// The callbacks emitted so far (drained).
    pub fn take_callbacks(&self) -> Vec<CallbackRequest> {
        std::mem::take(&mut self.sim.lock().unwrap().callbacks)
    }

    /// Provider-side downgrade / upgrade of a number's capabilities (emits CAPABILITY_CHANGED).
    pub fn change_capabilities(&self, external_ref: &str, capabilities: &[C]) {
        let now = self.clock.now();
        let mut sim = self.sim.lock().unwrap();
        let n = sim.numbers.get_mut(external_ref).expect("known number");
        n.change();
        n.capabilities = capabilities.to_vec();
        n.provisioned.retain(|c| capabilities.contains(c));
        let seq = n.sequence;
        let caps = capabilities.to_vec();
        self.emit(
            &mut sim,
            ProviderEventKind::CapabilityChanged,
            external_ref,
            None,
            seq,
            now,
            Some(caps),
        );
    }

    /// The provider takes a number away (emits NUMBER_REVOKED).
    pub fn revoke(&self, external_ref: &str) {
        let now = self.clock.now();
        let mut sim = self.sim.lock().unwrap();
        let n = sim.numbers.get_mut(external_ref).expect("known number");
        n.change();
        n.status = St::Unavailable;
        n.owned_by_us = false;
        n.voice_route = None;
        n.sms_route = None;
        let seq = n.sequence;
        self.emit(
            &mut sim,
            ProviderEventKind::NumberRevoked,
            external_ref,
            None,
            seq,
            now,
            None,
        );
    }

    /// Lets holds past their expiry lapse (emits HOLD_EXPIRED).
    pub fn expire_holds(&self) {
        let now = self.clock.now();
        let mut sim = self.sim.lock().unwrap();
        let due: Vec<String> = sim
            .numbers
            .values()
            .filter(|n| n.status == St::Held && n.hold_expires_at.is_some_and(|t| t <= now))
            .map(|n| n.external_ref.clone())
            .collect();
        for r in due {
            let n = sim.numbers.get_mut(&r).unwrap();
            n.change();
            let request = n.held_by_request;
            n.status = St::Available;
            n.owned_by_us = false;
            n.held_by_request = None;
            n.hold_expires_at = None;
            let seq = n.sequence;
            self.emit(
                &mut sim,
                ProviderEventKind::HoldExpired,
                &r,
                request,
                seq,
                now,
                None,
            );
        }
    }

    /// Signs a callback body like the provider does (test fixture for tampering).
    pub fn sign(
        &self,
        body: &[u8],
        timestamp: i64,
        delivery_id: &str,
        event_id: &str,
    ) -> CallbackRequest {
        let signature = cf_webhooks::signing::sign(&self.secret, timestamp, delivery_id, body);
        CallbackRequest {
            headers: vec![
                (
                    cf_webhooks::signing::HEADER_DELIVERY_ID.into(),
                    delivery_id.into(),
                ),
                (
                    cf_webhooks::signing::HEADER_EVENT_ID.into(),
                    event_id.into(),
                ),
                (
                    cf_webhooks::signing::HEADER_TIMESTAMP.into(),
                    timestamp.to_string(),
                ),
                (cf_webhooks::signing::HEADER_SIGNATURE.into(), signature),
            ],
            body: body.to_vec(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        sim: &mut Sim,
        kind: ProviderEventKind,
        external_ref: &str,
        request: Option<Uuid>,
        sequence: i64,
        now: DateTime<Utc>,
        capabilities: Option<Vec<C>>,
    ) {
        sim.next_event += 1;
        sim.next_delivery += 1;
        let event_id = format!("synev-{:08}", sim.next_event);
        let body = json!({
            "eventId": event_id,
            "kind": kind.as_str(),
            "externalRef": external_ref,
            "requestId": request,
            "sequence": sequence,
            "occurredAt": now,
            "capabilities": capabilities,
        })
        .to_string()
        .into_bytes();
        let delivery = format!("syndl-{:08}", sim.next_delivery);
        let cb = self.sign(&body, now.timestamp(), &delivery, &event_id);
        sim.callbacks.push(cb);
    }

    fn next_fault(sim: &mut Sim, op: Op) -> Option<Fault> {
        sim.faults.get_mut(&op).and_then(VecDeque::pop_front)
    }

    /// The shared execution path: outage, faults, idempotency, then the operation.
    fn execute(
        &self,
        op: Op,
        request: &OperationRequest,
        apply: impl FnOnce(
            &mut SimNumber,
            DateTime<Utc>,
        ) -> Result<Option<ProviderEventKind>, ProviderFailure>,
    ) -> ProviderOutcome<OperationReceipt> {
        let now = self.clock.now();
        let mut sim = self.sim.lock().unwrap();
        if sim.outage {
            sim.calls.push((op, Some(request.request_id), "OUTAGE"));
            return ProviderOutcome::Failed(ProviderFailure::not_transmitted(
                "PROVIDER_UNREACHABLE",
            ));
        }
        let fault = Self::next_fault(&mut sim, op);
        if fault == Some(Fault::TimeoutBeforeTransmission) {
            sim.calls
                .push((op, Some(request.request_id), "NOT_TRANSMITTED"));
            return ProviderOutcome::Failed(ProviderFailure::not_transmitted("CONNECT_TIMEOUT"));
        }
        if fault == Some(Fault::LostRequest) {
            sim.calls.push((op, Some(request.request_id), "LOST"));
            return ProviderOutcome::Unknown {
                reason: "read timeout".into(),
            };
        }
        // Provider-side idempotency: the same request never acts twice.
        if let Some(previous) = sim.processed.get(&(request.request_id, op)).cloned() {
            sim.calls.push((op, Some(request.request_id), "REPLAYED"));
            return previous;
        }
        let outcome = match fault {
            Some(Fault::AlreadyOwned) => {
                if let Some(n) = sim.numbers.get_mut(&request.external_ref) {
                    n.change();
                    n.status = St::Unavailable;
                }
                reject("ALREADY_OWNED", FailureCategory::NotObtainable)
            }
            Some(Fault::InventoryDisappears) => {
                sim.numbers.remove(&request.external_ref);
                not_found(&request.external_ref)
            }
            Some(Fault::ReservationConflict) => {
                reject("RESERVATION_CONFLICT", FailureCategory::NotObtainable)
            }
            Some(Fault::Reject(ref code)) => reject(code, FailureCategory::ProviderRejected),
            Some(Fault::StaleReadBack)
            | None
            | Some(Fault::TimeoutAfterExecution)
            | Some(Fault::PartialProvision) => match sim.numbers.get_mut(&request.external_ref) {
                None => not_found(&request.external_ref),
                Some(n) => {
                    let mut working = n.clone();
                    working.change();
                    match apply(&mut working, now) {
                        Err(refusal) => ProviderOutcome::Failed(refusal),
                        Ok(event) => {
                            if fault == Some(Fault::PartialProvision) {
                                working.provisioned.truncate(1);
                            }
                            *n = working;
                            let state = n.state(now);
                            let seq = n.sequence;
                            let ext = n.external_ref.clone();
                            sim.calls.push((op, Some(request.request_id), "EXECUTED"));
                            if let Some(kind) = event {
                                self.emit(
                                    &mut sim,
                                    kind,
                                    &ext,
                                    Some(request.request_id),
                                    seq,
                                    now,
                                    None,
                                );
                            }
                            ProviderOutcome::Succeeded(OperationReceipt {
                                provider_request_ref: format!(
                                    "synreq-{}",
                                    request.request_id.simple()
                                ),
                                external_ref: ext,
                                state: Some(state),
                            })
                        }
                    }
                }
            },
            Some(Fault::TimeoutBeforeTransmission) | Some(Fault::LostRequest) => unreachable!(),
        };
        if !matches!(outcome, ProviderOutcome::Failed(ref f) if !f.transmitted) {
            sim.processed
                .insert((request.request_id, op), outcome.clone());
        }
        if !matches!(outcome, ProviderOutcome::Succeeded(_)) {
            sim.calls
                .push((op, Some(request.request_id), outcome.label()));
        }
        match fault {
            Some(Fault::TimeoutAfterExecution) | Some(Fault::PartialProvision) => {
                ProviderOutcome::Unknown {
                    reason: "response lost after execution".into(),
                }
            }
            _ => outcome,
        }
    }
}

fn owned(n: &SimNumber) -> Result<(), ProviderFailure> {
    if n.owned_by_us {
        Ok(())
    } else {
        Err(refuse("NOT_OWNED", FailureCategory::ProviderRejected))
    }
}

#[async_trait::async_trait]
impl NumberingProvider for DeterministicProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            provider_id: SYNTHETIC_PROVIDER.into(),
            synthetic: true,
            pstn_routable: false,
            operations: vec![
                Op::Search,
                Op::Inspect,
                Op::Reserve,
                Op::Provision,
                Op::ConfigureVoice,
                Op::ConfigureSms,
                Op::Suspend,
                Op::Restore,
                Op::Release,
                Op::Events,
                Op::Health,
            ],
            countries: vec!["US".into()],
            number_types: vec![
                NumberType::Geographic,
                NumberType::Mobile,
                NumberType::TollFree,
                NumberType::National,
            ],
        }
    }

    async fn search(&self, query: &InventoryQuery) -> ProviderOutcome<Vec<InventoryOffer>> {
        let now = self.clock.now();
        let mut sim = self.sim.lock().unwrap();
        if sim.outage {
            return ProviderOutcome::Failed(ProviderFailure::not_transmitted(
                "PROVIDER_UNREACHABLE",
            ));
        }
        sim.calls.push((Op::Search, None, "EXECUTED"));
        if query.country != "US" {
            return ProviderOutcome::Succeeded(vec![]);
        }
        ProviderOutcome::Succeeded(
            sim.numbers
                .values()
                .filter(|n| n.status == St::Available)
                .filter(|n| query.number_type.is_none_or(|t| t == n.number_type))
                .filter(|n| query.region.is_none() || query.region == n.region)
                .filter(|n| {
                    query
                        .capabilities
                        .iter()
                        .all(|c| n.capabilities.contains(c))
                })
                .take(query.limit.clamp(1, 100))
                .map(|n| InventoryOffer {
                    external_ref: n.external_ref.clone(),
                    e164: n.e164.clone(),
                    country: "US".into(),
                    region: n.region.clone(),
                    number_type: n.number_type,
                    capabilities: n.capabilities.clone(),
                    unsupported: n.unsupported.clone(),
                    kyc_required: n.kyc_required,
                    monthly_cost_minor: Some(0),
                    currency: Some("USD".into()),
                    observed_at: now,
                    fresh_for: Duration::minutes(15),
                })
                .collect(),
        )
    }

    async fn inspect(&self, external_ref: &str) -> ProviderOutcome<ProviderNumberState> {
        let now = self.clock.now();
        let mut sim = self.sim.lock().unwrap();
        if sim.outage {
            sim.calls.push((Op::Inspect, None, "OUTAGE"));
            return ProviderOutcome::Unknown {
                reason: "provider unreachable".into(),
            };
        }
        let stale = Self::next_fault(&mut sim, Op::Inspect) == Some(Fault::StaleReadBack);
        sim.calls.push((Op::Inspect, None, "EXECUTED"));
        match sim.numbers.get(external_ref) {
            None => ProviderOutcome::Succeeded(ProviderNumberState {
                external_ref: external_ref.into(),
                status: St::NotFound,
                held_by_request: None,
                owned_by_us: false,
                capabilities: vec![],
                voice_route: None,
                sms_route: None,
                hold_expires_at: None,
                sequence: 0,
                observed_at: now,
            }),
            Some(n) => match (&n.previous, stale) {
                (Some(prev), true) => ProviderOutcome::Succeeded(prev.state(now)),
                _ => ProviderOutcome::Succeeded(n.state(now)),
            },
        }
    }

    async fn reserve(&self, request: &OperationRequest) -> ProviderOutcome<OperationReceipt> {
        let OperationKind::Reserve { hold_seconds } = request.operation else {
            return reject("WRONG_OPERATION", FailureCategory::ValidationFailed);
        };
        self.execute(Op::Reserve, request, |n, now| {
            match n.status {
                St::Available => {}
                St::Held if n.held_by_request == Some(request.request_id) => {}
                St::Held | St::Provisioned | St::Suspended | St::Unavailable => {
                    return Err(refuse("NOT_AVAILABLE", FailureCategory::NotObtainable))
                }
                St::Released | St::NotFound => {
                    return Err(refuse("NOT_FOUND", FailureCategory::NotObtainable))
                }
            }
            n.status = St::Held;
            n.owned_by_us = true;
            n.held_by_request = Some(request.request_id);
            n.hold_expires_at = Some(now + Duration::seconds(hold_seconds.clamp(60, 86_400)));
            Ok(Some(ProviderEventKind::HoldConfirmed))
        })
    }

    async fn provision(&self, request: &OperationRequest) -> ProviderOutcome<OperationReceipt> {
        let OperationKind::Provision { capabilities } = &request.operation else {
            return reject("WRONG_OPERATION", FailureCategory::ValidationFailed);
        };
        let wanted = capabilities.clone();
        self.execute(Op::Provision, request, move |n, _| {
            owned(n)?;
            if n.status != St::Held {
                return Err(refuse("NOT_HELD", FailureCategory::ProviderRejected));
            }
            if wanted.iter().any(|c| !n.capabilities.contains(c)) {
                return Err(refuse(
                    "CAPABILITY_NOT_SUPPORTED",
                    FailureCategory::ValidationFailed,
                ));
            }
            n.status = St::Provisioned;
            n.hold_expires_at = None;
            n.provisioned = wanted.into_iter().filter(|c| c.is_routable()).collect();
            Ok(Some(ProviderEventKind::Provisioned))
        })
    }

    async fn configure_voice(
        &self,
        request: &OperationRequest,
    ) -> ProviderOutcome<OperationReceipt> {
        let OperationKind::ConfigureVoice { route_ref } = request.operation else {
            return reject("WRONG_OPERATION", FailureCategory::ValidationFailed);
        };
        self.execute(Op::ConfigureVoice, request, move |n, _| {
            owned(n)?;
            if !matches!(n.status, St::Provisioned | St::Suspended) {
                return Err(refuse("NOT_PROVISIONED", FailureCategory::ProviderRejected));
            }
            if !n
                .provisioned
                .iter()
                .any(|c| matches!(c, C::VoiceInbound | C::VoiceOutbound))
            {
                return Err(refuse(
                    "CAPABILITY_NOT_SUPPORTED",
                    FailureCategory::ValidationFailed,
                ));
            }
            n.voice_route = Some(route_ref);
            Ok(Some(ProviderEventKind::RouteConfigured))
        })
    }

    async fn configure_sms(&self, request: &OperationRequest) -> ProviderOutcome<OperationReceipt> {
        let OperationKind::ConfigureSms { route_ref } = request.operation else {
            return reject("WRONG_OPERATION", FailureCategory::ValidationFailed);
        };
        self.execute(Op::ConfigureSms, request, move |n, _| {
            owned(n)?;
            if !matches!(n.status, St::Provisioned | St::Suspended) {
                return Err(refuse("NOT_PROVISIONED", FailureCategory::ProviderRejected));
            }
            if !n
                .provisioned
                .iter()
                .any(|c| matches!(c, C::SmsInbound | C::SmsOutbound))
            {
                return Err(refuse(
                    "CAPABILITY_NOT_SUPPORTED",
                    FailureCategory::ValidationFailed,
                ));
            }
            n.sms_route = Some(route_ref);
            Ok(Some(ProviderEventKind::RouteConfigured))
        })
    }

    async fn suspend(&self, request: &OperationRequest) -> ProviderOutcome<OperationReceipt> {
        self.execute(Op::Suspend, request, |n, _| {
            owned(n)?;
            match n.status {
                St::Provisioned => {
                    n.status = St::Suspended;
                    Ok(Some(ProviderEventKind::Suspended))
                }
                St::Suspended => Ok(None),
                _ => Err(refuse("NOT_PROVISIONED", FailureCategory::ProviderRejected)),
            }
        })
    }

    async fn restore(&self, request: &OperationRequest) -> ProviderOutcome<OperationReceipt> {
        self.execute(Op::Restore, request, |n, _| {
            owned(n)?;
            match n.status {
                St::Suspended => {
                    n.status = St::Provisioned;
                    Ok(Some(ProviderEventKind::Restored))
                }
                St::Provisioned => Ok(None),
                _ => Err(refuse("NOT_SUSPENDED", FailureCategory::ProviderRejected)),
            }
        })
    }

    async fn release(&self, request: &OperationRequest) -> ProviderOutcome<OperationReceipt> {
        self.execute(Op::Release, request, |n, _| {
            owned(n)?;
            // Back in the provider's pool, no longer ours.
            n.status = St::Available;
            n.owned_by_us = false;
            n.held_by_request = None;
            n.hold_expires_at = None;
            n.voice_route = None;
            n.sms_route = None;
            n.provisioned.clear();
            Ok(Some(ProviderEventKind::Released))
        })
    }

    fn verify_event(
        &self,
        request: &CallbackRequest,
        now: DateTime<Utc>,
    ) -> Result<NormalizedProviderEvent, CallbackRejection> {
        use cf_webhooks::signing::{
            verify, VerifyError, HEADER_DELIVERY_ID, HEADER_EVENT_ID, HEADER_SIGNATURE,
            HEADER_TIMESTAMP,
        };
        let (Some(sig), Some(ts), Some(delivery), Some(event_id)) = (
            request.header(HEADER_SIGNATURE),
            request.header(HEADER_TIMESTAMP),
            request.header(HEADER_DELIVERY_ID),
            request.header(HEADER_EVENT_ID),
        ) else {
            return Err(CallbackRejection::Unauthenticated);
        };
        verify(
            &self.secret,
            sig,
            ts,
            delivery,
            &request.body,
            now.timestamp(),
            CALLBACK_TOLERANCE_SECONDS,
        )
        .map_err(|e| match e {
            VerifyError::StaleTimestamp => CallbackRejection::Replayed,
            VerifyError::Malformed | VerifyError::Mismatch => CallbackRejection::Unauthenticated,
        })?;
        let v: Value =
            serde_json::from_slice(&request.body).map_err(|_| CallbackRejection::Malformed)?;
        let id = v["eventId"].as_str().ok_or(CallbackRejection::Malformed)?;
        if id != event_id || id.is_empty() || id.len() > 128 {
            return Err(CallbackRejection::Malformed);
        }
        let kind = v["kind"]
            .as_str()
            .and_then(ProviderEventKind::parse)
            .ok_or(CallbackRejection::Malformed)?;
        let external_ref = v["externalRef"]
            .as_str()
            .filter(|r| !r.is_empty() && r.len() <= 200);
        let capabilities = match &v["capabilities"] {
            Value::Null => None,
            Value::Array(items) => Some(
                items
                    .iter()
                    .map(|c| c.as_str().and_then(C::parse))
                    .collect::<Option<Vec<_>>>()
                    .ok_or(CallbackRejection::Malformed)?,
            ),
            _ => return Err(CallbackRejection::Malformed),
        };
        Ok(NormalizedProviderEvent {
            provider_event_id: id.into(),
            kind,
            external_ref: external_ref.ok_or(CallbackRejection::Malformed)?.into(),
            request_id: v["requestId"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok()),
            sequence: v["sequence"].as_i64(),
            occurred_at: v["occurredAt"]
                .as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.with_timezone(&Utc))
                .ok_or(CallbackRejection::Malformed)?,
            capabilities,
        })
    }

    async fn health(&self) -> ProviderHealth {
        let outage = self.sim.lock().unwrap().outage;
        ProviderHealth {
            available: !outage,
            detail: if outage {
                "simulated outage".into()
            } else {
                "synthetic provider".into()
            },
        }
    }
}

impl DeterministicProvider {
    /// The E.164 of a synthetic number (fixtures).
    pub fn e164_of(&self, external_ref: &str) -> Option<String> {
        self.sim
            .lock()
            .unwrap()
            .numbers
            .get(external_ref)
            .map(|n| n.e164.clone())
    }
}
