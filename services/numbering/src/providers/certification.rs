//! The reusable NumberingProvider certification suite (Part IV Workstream G §16).
//!
//! Every adapter — the deterministic one now, real carrier adapters later — runs the same
//! contract cases through a [`CertificationHarness`]. Cases that change provider state
//! (holds, provisioning, release) run only when the harness allows destructive cases
//! (`destructive_allowed`: always for the synthetic provider; for a real adapter only with
//! an explicit opt-in such as `CFMEET_PROVIDER_LIVE_CERTIFICATION=1` and a disposable test
//! account). Fault-classification cases need scripted faults and run only on harnesses
//! that can inject them. A skipped case is reported as such, never as passed.
//!
//! The durable layers that every adapter shares (commands, callbacks storage,
//! reconciliation, restart recovery) are certified once by the Numbering integration
//! suite (`tests/provider_contract.rs`) on top of this adapter contract.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{
    deterministic::Fault, dispatch, registry::ProviderRegistry, CallbackRejection, CallbackRequest,
    InventoryQuery, NumberingProvider, OperationKind, OperationRequest, ProviderNumberStatus as St,
    ProviderOperation as Op, ProviderOutcome,
};
use crate::numbers::{Capability as C, NumberType};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaseStatus {
    Passed,
    Failed(String),
    Skipped(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseResult {
    pub name: &'static str,
    pub status: CaseStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CertificationReport {
    pub provider_id: String,
    pub cases: Vec<CaseResult>,
}

impl CertificationReport {
    pub fn passed(&self) -> usize {
        self.cases
            .iter()
            .filter(|c| c.status == CaseStatus::Passed)
            .count()
    }

    pub fn failures(&self) -> Vec<&CaseResult> {
        self.cases
            .iter()
            .filter(|c| matches!(c.status, CaseStatus::Failed(_)))
            .collect()
    }

    pub fn skipped(&self) -> Vec<&CaseResult> {
        self.cases
            .iter()
            .filter(|c| matches!(c.status, CaseStatus::Skipped(_)))
            .collect()
    }
}

/// What the suite needs from the environment of an adapter under test.
pub trait CertificationHarness: Send + Sync {
    fn provider(&self) -> &dyn NumberingProvider;
    /// A country the adapter serves and an inventory query that finds numbers.
    fn query(&self) -> InventoryQuery;
    fn now(&self) -> DateTime<Utc>;
    /// State-changing cases (holds, provisioning, release) may run.
    fn destructive_allowed(&self) -> bool;
    /// Scripts a fault for the next call of `op`; `false` when unsupported.
    fn inject(&self, op: Op, fault: Fault) -> bool;
    /// Provider outage simulation; `false` when unsupported.
    fn set_outage(&self, outage: bool) -> bool;
    /// Callbacks the provider emitted since the last call.
    fn drain_callbacks(&self) -> Vec<CallbackRequest>;
    /// Strings that must never appear in anything the adapter returns (secrets).
    fn secret_markers(&self) -> Vec<String>;
}

type Check = Result<(), String>;

fn ensure(ok: bool, why: impl Into<String>) -> Check {
    if ok {
        Ok(())
    } else {
        Err(why.into())
    }
}

fn req(external_ref: &str, operation: OperationKind) -> OperationRequest {
    OperationRequest {
        request_id: Uuid::new_v4(),
        external_ref: external_ref.into(),
        operation,
    }
}

async fn state(
    p: &dyn NumberingProvider,
    external_ref: &str,
) -> Result<super::ProviderNumberState, String> {
    match p.inspect(external_ref).await {
        ProviderOutcome::Succeeded(s) => Ok(s),
        other => Err(format!("inspect answered {}", other.label())),
    }
}

/// Finds an available offer with `caps` (first match).
async fn offer_with(
    h: &dyn CertificationHarness,
    caps: &[C],
) -> Result<super::InventoryOffer, String> {
    let mut q = h.query();
    q.capabilities = caps.to_vec();
    q.limit = 50;
    match h.provider().search(&q).await {
        ProviderOutcome::Succeeded(offers) => offers
            .into_iter()
            .next()
            .ok_or_else(|| format!("no offer with {caps:?}")),
        other => Err(format!("search answered {}", other.label())),
    }
}

/// Holds and provisions a number with `caps` (destructive fixture).
async fn provisioned(h: &dyn CertificationHarness, caps: &[C]) -> Result<String, String> {
    let p = h.provider();
    let offer = offer_with(h, caps).await?;
    let hold = req(
        &offer.external_ref,
        OperationKind::Reserve { hold_seconds: 600 },
    );
    ensure(
        matches!(p.reserve(&hold).await, ProviderOutcome::Succeeded(_)),
        "reserve failed",
    )?;
    let routable: Vec<C> = caps.iter().copied().filter(|c| c.is_routable()).collect();
    let prov = req(
        &offer.external_ref,
        OperationKind::Provision {
            capabilities: routable,
        },
    );
    ensure(
        matches!(p.provision(&prov).await, ProviderOutcome::Succeeded(_)),
        "provision failed",
    )?;
    Ok(offer.external_ref)
}

/// Runs the suite.
pub async fn certify(h: &dyn CertificationHarness) -> CertificationReport {
    let p = h.provider();
    let d = p.descriptor();
    let mut report = CertificationReport {
        provider_id: d.provider_id.clone(),
        cases: vec![],
    };
    macro_rules! case {
        ($name:literal, $destructive:expr, $body:expr) => {{
            let status = if $destructive && !h.destructive_allowed() {
                CaseStatus::Skipped("destructive cases are not enabled for this adapter")
            } else {
                let r: Check = $body.await;
                match r {
                    Ok(()) => CaseStatus::Passed,
                    Err(e) => CaseStatus::Failed(e),
                }
            };
            report.cases.push(CaseResult {
                name: $name,
                status,
            });
        }};
    }
    macro_rules! fault_case {
        ($name:literal, $op:expr, $fault:expr, $body:expr) => {{
            let status = if !h.destructive_allowed() {
                CaseStatus::Skipped("destructive cases are not enabled for this adapter")
            } else {
                let r: Check = $body.await;
                match r {
                    Ok(()) => CaseStatus::Passed,
                    Err(e) if e == "NO_INJECTION" => {
                        CaseStatus::Skipped("the harness cannot inject faults")
                    }
                    Err(e) => CaseStatus::Failed(e),
                }
            };
            let _ = ($op, $fault);
            report.cases.push(CaseResult {
                name: $name,
                status,
            });
        }};
    }

    case!("descriptor_is_consistent", false, async {
        ensure(
            crate::numbers::inventory::valid_provider(&d.provider_id),
            "invalid provider key",
        )?;
        ensure(
            !(d.synthetic && d.pstn_routable),
            "synthetic numbers must not be PSTN-routable",
        )?;
        for op in [
            Op::Search,
            Op::Inspect,
            Op::Reserve,
            Op::Provision,
            Op::Release,
            Op::Events,
            Op::Health,
        ] {
            ensure(d.supports(op), format!("required operation {op} missing"))?;
        }
        ensure(
            !d.number_types.contains(&NumberType::Unclassified),
            "UNCLASSIFIED is not a provider type",
        )
    });

    case!("environment_restrictions", false, async {
        use cf_service_auth::RuntimeEnvironment as R;
        for runtime in [R::Staging, R::Production] {
            let refused = ProviderRegistry::new(runtime).admits(&d).is_err();
            ensure(
                refused == d.synthetic,
                format!("{runtime:?}: synthetic adapters are refused, real ones accepted"),
            )?;
        }
        Ok(())
    });

    case!("health_is_reported", false, async {
        ensure(p.health().await.available, "provider reports unavailable")
    });

    case!("inventory_is_normalized", false, async {
        let mut q = h.query();
        q.capabilities = vec![];
        q.limit = 100;
        let offers = match p.search(&q).await {
            ProviderOutcome::Succeeded(o) => o,
            other => return Err(format!("search answered {}", other.label())),
        };
        ensure(!offers.is_empty(), "no inventory")?;
        for o in &offers {
            crate::numbers::e164::validate(&o.e164, &o.country)
                .map_err(|e| format!("{}: {e}", o.external_ref))?;
            ensure(
                o.number_type != NumberType::Unclassified,
                "unclassified offer",
            )?;
            ensure(!o.capabilities.is_empty(), "an offer serves something")?;
            ensure(
                o.capabilities.iter().all(|c| !o.unsupported.contains(c)),
                "supported and unsupported overlap",
            )?;
            ensure(
                o.fresh_for > chrono::Duration::zero(),
                "an offer has a freshness window",
            )?;
            ensure(
                !o.external_ref.is_empty() && o.external_ref.len() <= 200,
                "bounded provider reference",
            )?;
            if d.synthetic {
                ensure(
                    o.e164.starts_with("+1555"),
                    "synthetic numbers are in the fictional +1 555 range",
                )?;
            }
            // Class attributes follow the type (never inferred from SMS support).
            if o.capabilities.contains(&C::MobileNumberClass) {
                ensure(
                    o.number_type == NumberType::Mobile,
                    "mobile class only on MOBILE numbers",
                )?;
            }
            if o.capabilities.contains(&C::TollFreeBusiness) {
                ensure(
                    o.number_type == NumberType::TollFree,
                    "toll-free class only on TOLL_FREE numbers",
                )?;
            }
        }
        Ok(())
    });

    case!("capability_mapping_is_respected", false, async {
        for caps in [
            vec![C::VoiceInbound],
            vec![C::SmsOutbound],
            vec![C::VoiceInbound, C::SmsInbound],
            vec![C::MobileNumberClass],
        ] {
            let mut q = h.query();
            q.capabilities = caps.clone();
            q.limit = 100;
            if let ProviderOutcome::Succeeded(offers) = p.search(&q).await {
                ensure(
                    offers
                        .iter()
                        .all(|o| caps.iter().all(|c| o.capabilities.contains(c))),
                    format!("{caps:?} filter ignored"),
                )?;
            }
        }
        Ok(())
    });

    case!("read_back_of_an_unknown_object", false, async {
        let s = state(p, "certification-no-such-object").await?;
        ensure(
            s.status == St::NotFound && !s.owned_by_us,
            "unknown objects read back NOT_FOUND",
        )
    });

    case!("reservation_is_idempotent_and_exclusive", true, async {
        let offer = offer_with(h, &[C::VoiceInbound]).await?;
        let hold = req(
            &offer.external_ref,
            OperationKind::Reserve { hold_seconds: 600 },
        );
        let first = p.reserve(&hold).await;
        let again = p.reserve(&hold).await;
        ensure(matches!(first, ProviderOutcome::Succeeded(_)), "first hold")?;
        ensure(first == again, "the same request answers the same result")?;
        let s = state(p, &offer.external_ref).await?;
        ensure(
            s.status == St::Held && s.owned_by_us && s.held_by_request == Some(hold.request_id),
            "read-back shows our hold",
        )?;
        let other = req(
            &offer.external_ref,
            OperationKind::Reserve { hold_seconds: 600 },
        );
        ensure(
            matches!(p.reserve(&other).await, ProviderOutcome::Failed(f) if f.transmitted),
            "a second hold is a definite refusal",
        )?;
        let release = req(&offer.external_ref, OperationKind::Release);
        ensure(
            matches!(p.release(&release).await, ProviderOutcome::Succeeded(_)),
            "release of the hold",
        )
    });

    case!(
        "provisioning_produces_evidence_and_read_back",
        true,
        async {
            let r = provisioned(h, &[C::VoiceInbound, C::SmsInbound]).await?;
            let s = state(p, &r).await?;
            ensure(
                s.status == St::Provisioned && s.owned_by_us,
                "read-back shows provisioned",
            )?;
            ensure(
                s.capabilities.contains(&C::VoiceInbound)
                    && s.capabilities.contains(&C::SmsInbound),
                "served capabilities reported",
            )?;
            ensure(
                matches!(
                    p.release(&req(&r, OperationKind::Release)).await,
                    ProviderOutcome::Succeeded(_)
                ),
                "release",
            )
        }
    );

    case!("voice_and_sms_configuration", true, async {
        let r = provisioned(h, &[C::VoiceInbound, C::VoiceOutbound]).await?;
        let route = Uuid::new_v4();
        let ok = p
            .configure_voice(&req(&r, OperationKind::ConfigureVoice { route_ref: route }))
            .await;
        ensure(matches!(ok, ProviderOutcome::Succeeded(_)), "voice route")?;
        ensure(
            state(p, &r).await?.voice_route == Some(route),
            "read-back shows the voice route",
        )?;
        let sms = p
            .configure_sms(&req(&r, OperationKind::ConfigureSms { route_ref: route }))
            .await;
        ensure(
            matches!(sms, ProviderOutcome::Failed(f) if f.transmitted),
            "SMS on a voice-only provisioning is a definite refusal",
        )?;
        ensure(
            matches!(
                p.release(&req(&r, OperationKind::Release)).await,
                ProviderOutcome::Succeeded(_)
            ),
            "release",
        )
    });

    case!("suspension_and_restoration", true, async {
        let r = provisioned(h, &[C::VoiceInbound]).await?;
        let s1 = p
            .suspend(&req(
                &r,
                OperationKind::Suspend {
                    reason_code: "CERTIFICATION".into(),
                },
            ))
            .await;
        ensure(matches!(s1, ProviderOutcome::Succeeded(_)), "suspend")?;
        ensure(
            state(p, &r).await?.status == St::Suspended,
            "read-back suspended",
        )?;
        ensure(
            matches!(
                p.restore(&req(&r, OperationKind::Restore)).await,
                ProviderOutcome::Succeeded(_)
            ),
            "restore",
        )?;
        ensure(
            state(p, &r).await?.status == St::Provisioned,
            "read-back restored",
        )?;
        ensure(
            matches!(
                p.release(&req(&r, OperationKind::Release)).await,
                ProviderOutcome::Succeeded(_)
            ),
            "release",
        )
    });

    case!("release_is_final", true, async {
        let r = provisioned(h, &[C::VoiceInbound]).await?;
        let rel = req(&r, OperationKind::Release);
        ensure(
            matches!(p.release(&rel).await, ProviderOutcome::Succeeded(_)),
            "release",
        )?;
        let s = state(p, &r).await?;
        ensure(
            !s.owned_by_us && s.voice_route.is_none(),
            "released numbers are not ours and carry no route",
        )?;
        let after = p
            .configure_voice(&req(
                &r,
                OperationKind::ConfigureVoice {
                    route_ref: Uuid::new_v4(),
                },
            ))
            .await;
        ensure(
            matches!(after, ProviderOutcome::Failed(f) if f.transmitted),
            "no operation resurrects a released number",
        )
    });

    case!("optional_portability_is_declared", false, async {
        let outcome = p
            .port_in(&req(
                "certification-port",
                OperationKind::PortIn {
                    losing_carrier_ref: "x".into(),
                },
            ))
            .await;
        ensure(
            d.supports(Op::PortIn) || outcome == ProviderOutcome::Unsupported,
            "an adapter without porting answers UNSUPPORTED",
        )
    });

    case!("events_are_authenticated", true, async {
        let _ = h.drain_callbacks();
        let r = provisioned(h, &[C::VoiceInbound]).await?;
        let callbacks = h.drain_callbacks();
        ensure(!callbacks.is_empty(), "state changes emit callbacks")?;
        let now = h.now();
        for cb in &callbacks {
            let ev = p
                .verify_event(cb, now)
                .map_err(|e| format!("valid callback refused: {e}"))?;
            ensure(ev.external_ref == r, "events name the provider object")?;
            ensure(!ev.provider_event_id.is_empty(), "stable event id")?;
        }
        let mut tampered = callbacks[0].clone();
        tampered.body.push(b' ');
        ensure(
            p.verify_event(&tampered, now) == Err(CallbackRejection::Unauthenticated),
            "tampered body refused",
        )?;
        let mut unsigned = callbacks[0].clone();
        unsigned
            .headers
            .retain(|(k, _)| !k.eq_ignore_ascii_case(cf_webhooks::signing::HEADER_SIGNATURE));
        ensure(
            p.verify_event(&unsigned, now) == Err(CallbackRejection::Unauthenticated),
            "unsigned refused",
        )?;
        ensure(
            p.verify_event(&callbacks[0], now + chrono::Duration::hours(1))
                == Err(CallbackRejection::Replayed),
            "replays outside the window refused",
        )?;
        ensure(
            matches!(
                p.release(&req(&r, OperationKind::Release)).await,
                ProviderOutcome::Succeeded(_)
            ),
            "release",
        )
    });

    fault_case!(
        "timeout_before_transmission_is_definite",
        Op::Reserve,
        Fault::TimeoutBeforeTransmission,
        async {
            let offer = offer_with(h, &[C::VoiceInbound]).await?;
            if !h.inject(Op::Reserve, Fault::TimeoutBeforeTransmission) {
                return Err("NO_INJECTION".into());
            }
            let hold = req(
                &offer.external_ref,
                OperationKind::Reserve { hold_seconds: 600 },
            );
            ensure(
                matches!(p.reserve(&hold).await, ProviderOutcome::Failed(f) if !f.transmitted && f.retryable),
                "not transmitted, retryable",
            )?;
            ensure(
                state(p, &offer.external_ref).await?.status == St::Available,
                "nothing happened",
            )?;
            ensure(
                matches!(p.reserve(&hold).await, ProviderOutcome::Succeeded(_)),
                "a retry with the same id succeeds once",
            )?;
            ensure(
                matches!(
                    p.release(&req(&offer.external_ref, OperationKind::Release))
                        .await,
                    ProviderOutcome::Succeeded(_)
                ),
                "release",
            )
        }
    );

    fault_case!(
        "timeout_after_execution_is_unknown_and_read_back_shows_it",
        Op::Reserve,
        Fault::TimeoutAfterExecution,
        async {
            let offer = offer_with(h, &[C::VoiceInbound]).await?;
            if !h.inject(Op::Reserve, Fault::TimeoutAfterExecution) {
                return Err("NO_INJECTION".into());
            }
            let hold = req(
                &offer.external_ref,
                OperationKind::Reserve { hold_seconds: 600 },
            );
            ensure(
                matches!(p.reserve(&hold).await, ProviderOutcome::Unknown { .. }),
                "unknown",
            )?;
            let s = state(p, &offer.external_ref).await?;
            ensure(
                s.status == St::Held && s.held_by_request == Some(hold.request_id),
                "the effect is visible on read-back",
            )?;
            ensure(
                matches!(
                    p.release(&req(&offer.external_ref, OperationKind::Release))
                        .await,
                    ProviderOutcome::Succeeded(_)
                ),
                "release",
            )
        }
    );

    fault_case!(
        "lost_request_is_unknown_and_read_back_shows_nothing",
        Op::Reserve,
        Fault::LostRequest,
        async {
            let offer = offer_with(h, &[C::VoiceInbound]).await?;
            if !h.inject(Op::Reserve, Fault::LostRequest) {
                return Err("NO_INJECTION".into());
            }
            let hold = req(
                &offer.external_ref,
                OperationKind::Reserve { hold_seconds: 600 },
            );
            ensure(
                matches!(p.reserve(&hold).await, ProviderOutcome::Unknown { .. }),
                "unknown",
            )?;
            ensure(
                state(p, &offer.external_ref).await?.status == St::Available,
                "nothing happened",
            )
        }
    );

    fault_case!(
        "conflicts_are_definite_failures",
        Op::Reserve,
        Fault::ReservationConflict,
        async {
            let offer = offer_with(h, &[C::VoiceInbound]).await?;
            for fault in [Fault::ReservationConflict, Fault::AlreadyOwned] {
                if !h.inject(Op::Reserve, fault) {
                    return Err("NO_INJECTION".into());
                }
                let hold = req(
                    &offer.external_ref,
                    OperationKind::Reserve { hold_seconds: 600 },
                );
                ensure(
                    matches!(p.reserve(&hold).await, ProviderOutcome::Failed(f) if f.transmitted && !f.retryable),
                    "definite refusal",
                )?;
            }
            Ok(())
        }
    );

    fault_case!(
        "outage_is_classified",
        Op::Health,
        Fault::LostRequest,
        async {
            if !h.set_outage(true) {
                return Err("NO_INJECTION".into());
            }
            let health = p.health().await;
            let inspect = p.inspect("certification-no-such-object").await;
            let reserve = p
                .reserve(&req(
                    "certification-no-such-object",
                    OperationKind::Reserve { hold_seconds: 600 },
                ))
                .await;
            h.set_outage(false);
            ensure(!health.available, "health reports the outage")?;
            ensure(
                matches!(inspect, ProviderOutcome::Unknown { .. }),
                "read-back is inconclusive during an outage",
            )?;
            ensure(
                matches!(reserve, ProviderOutcome::Failed(f) if !f.transmitted),
                "calls fail before transmission",
            )
        }
    );

    case!("secrets_never_leave_the_adapter", true, async {
        let markers = h.secret_markers();
        let r = provisioned(h, &[C::VoiceInbound]).await?;
        let s = state(p, &r).await?;
        let mut text = format!("{:?}{:?}", d, s);
        text.push_str(&serde_json::to_string(&s).unwrap_or_default());
        for cb in h.drain_callbacks() {
            text.push_str(&String::from_utf8_lossy(&cb.body));
            if let Ok(ev) = p.verify_event(&cb, h.now()) {
                text.push_str(&format!("{ev:?}"));
            }
        }
        let receipt = dispatch(p, &req(&r, OperationKind::Release)).await;
        text.push_str(&format!("{receipt:?}"));
        for m in markers.iter().filter(|m| !m.is_empty()) {
            ensure(
                !text.contains(m.as_str()),
                "a secret appeared in adapter output",
            )?;
        }
        Ok(())
    });

    report
}
