//! Enterprise SIP execution primitives (Part F §54, §57, §179; ADR 0018).
//!
//! Enterprise devices (desk phones, softphones) register on CF Meet's OWN SIP / media edge.
//! A dialog from a device is an ENTERPRISE_INTERNAL leg: no carrier, no provider, no
//! spend, no usage fact. Telephony executes; Calling decides:
//!
//! 1. kill switches (Telephony, enterprise SIP);
//! 2. the edge endpoint must be a registered, ENABLED enterprise device (anything else is
//!    rejected and raises ENTERPRISE_DEVICE_REJECTED);
//! 3. bounded ingress rate per device;
//! 4. the caller hears ringing; Calling receives an authenticated offer (opaque device
//!    reference + dialled extension) and creates the ONE canonical Call — or refuses;
//! 5. Calling then commands the leg: ANSWER, PROMPT (BUILT-IN prompts only — Calling never
//!    names an edge sound file), CAPTURE (voicemail; see [`crate::capture`]), TERMINATE,
//!    HOLD / RESUME, bridges to application media.
//!
//! Keypad input (DTMF RFC 4733) of an enterprise leg is forwarded to Calling's IVR as it
//! happens and is never persisted or logged; Calling keeps only the selected branch.

use sea_orm::{FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    commands::{executor::Executed, CommandRow},
    db,
    domain::{CommandKind, EndReason, LegStatus, NormalizedEvent},
    error::TelephonyResult,
    incidents::{self, Incident, Severity},
    legs::{self, Change, LegRow, LEG_COLUMNS},
    service::TelephonyService,
    sip::{ChannelState, EdgeEnterpriseDialog, EdgeError, EdgeEvent, EdgeEventKind},
};

/// Enterprise dialogs accepted per device per minute.
pub const MAX_ENTERPRISE_DIALOGS_PER_DEVICE_PER_MINUTE: i64 = 30;
/// The longest an internal enterprise leg may last.
pub const ENTERPRISE_MAX_LEG_SECONDS: i32 = 4 * 3600;

/// The built-in prompt catalog (Calling's vocabulary) → edge-local sounds and the
/// prompt's nominal length in seconds (the contract's catalog: Calling starts a menu's
/// input timer after it, so the sound must fit). Calling never names a sound file; an
/// unknown prompt is refused.
pub const PROMPT_SOUNDS: &[(&str, &str, u32)] = &[
    ("WELCOME", "hello", 2),
    ("MENU", "dir-pls-enter", 2),
    ("INVALID_OPTION", "pm-invalid-option", 4),
    ("NO_INPUT", "please-try-again", 2),
    ("PLEASE_HOLD", "pls-hold-while-try", 3),
    ("TRANSFERRING", "transfer", 3),
    ("GOODBYE", "goodbye", 2),
    ("CLOSED", "please-try-call-later", 3),
    ("VOICEMAIL_GREETING", "vm-intro", 6),
    ("BEEP", "beep", 1),
];

pub fn prompt_sound(prompt: &str) -> Option<&'static str> {
    PROMPT_SOUNDS
        .iter()
        .find(|(p, _, _)| *p == prompt)
        .map(|(_, s, _)| *s)
}

/// Why an enterprise dialog was rejected (bounded codes; logs only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnterpriseRejection {
    Disabled,
    UnknownDevice,
    InvalidExtension,
    RateLimited,
    CallingRefused,
    CallingUnavailable,
    Internal,
}

impl EnterpriseRejection {
    pub fn as_str(&self) -> &'static str {
        match self {
            EnterpriseRejection::Disabled => "DISABLED",
            EnterpriseRejection::UnknownDevice => "UNKNOWN_DEVICE",
            EnterpriseRejection::InvalidExtension => "INVALID_EXTENSION",
            EnterpriseRejection::RateLimited => "RATE_LIMITED",
            EnterpriseRejection::CallingRefused => "CALLING_REFUSED",
            EnterpriseRejection::CallingUnavailable => "CALLING_UNAVAILABLE",
            EnterpriseRejection::Internal => "INTERNAL",
        }
    }
}

#[derive(Debug, FromQueryResult)]
struct Device {
    id: Uuid,
    device_ref: String,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
}

impl TelephonyService {
    /// A new dialog of an enterprise device entered the control application.
    pub(crate) async fn handle_enterprise_dialog(
        &self,
        event: &EdgeEvent,
        dialog: EdgeEnterpriseDialog,
    ) -> TelephonyResult<()> {
        match self.accept_enterprise(event, &dialog).await {
            Ok(leg_id) => {
                crate::metrics::inc(
                    "telephony_enterprise_dialogs_total",
                    &[("result", "OFFERED")],
                );
                tracing::info!(telephony_leg_id = %leg_id, "enterprise SIP dialog offered to Calling");
            }
            Err(rejection) => {
                crate::metrics::inc(
                    "telephony_enterprise_dialogs_total",
                    &[("result", rejection.as_str())],
                );
                tracing::info!(
                    reason = rejection.as_str(),
                    "enterprise SIP dialog rejected"
                );
                let _ = self.edge.reject(&event.channel).await;
            }
        }
        Ok(())
    }

    async fn accept_enterprise(
        &self,
        event: &EdgeEvent,
        dialog: &EdgeEnterpriseDialog,
    ) -> Result<Uuid, EnterpriseRejection> {
        let internal = |_| EnterpriseRejection::Internal;
        if !self.flags.telephony.enabled() || !self.flags.enterprise_sip.enabled() {
            return Err(EnterpriseRejection::Disabled);
        }
        let device: Option<Device> = db::one(
            &self.db,
            "SELECT id, device_ref, tenant_id, application_id, environment_id \
               FROM telephony_enterprise_devices WHERE edge_endpoint = $1 AND status = 'ENABLED'",
            vec![dialog.endpoint.clone().into()],
        )
        .await
        .map_err(internal)?;
        let Some(device) = device else {
            let now = self.now();
            let incident = Incident::new(
                "ENTERPRISE_DEVICE_REJECTED",
                Severity::Low,
                "enterprise-device:unknown".to_string(),
            )
            .evidence(json!({ "known": false }));
            incidents::raise(&self.db, &incident, now)
                .await
                .map_err(internal)?;
            return Err(EnterpriseRejection::UnknownDevice);
        };
        if dialog.extension.is_empty() {
            return Err(EnterpriseRejection::InvalidExtension);
        }
        let hits = self
            .count_ingress(&dialog.endpoint.to_ascii_lowercase())
            .await
            .map_err(internal)?;
        if hits > MAX_ENTERPRISE_DIALOGS_PER_DEVICE_PER_MINUTE {
            return Err(EnterpriseRejection::RateLimited);
        }
        // The device hears ringing while Calling decides.
        let _ = self.edge.ring(&event.channel).await;
        let correlation = cf_service_auth::CorrelationId::generate();
        let leg_id = Uuid::new_v4();
        let offer = json!({
            "schemaVersion": "1",
            "telephonyLegId": leg_id,
            "correlationId": correlation.as_str(),
            "scope": {
                "tenantId": device.tenant_id,
                "applicationId": device.application_id,
                "environmentId": device.environment_id,
            },
            "deviceRef": device.device_ref,
            "extension": dialog.extension,
            "maxDurationSeconds": ENTERPRISE_MAX_LEG_SECONDS,
        });
        let accepted: Value = match self.calling.offer_enterprise(&offer).await {
            Ok(v) => v,
            Err(crate::ports::PortError::Unavailable) => {
                return Err(EnterpriseRejection::CallingUnavailable)
            }
            Err(_) => return Err(EnterpriseRejection::CallingRefused),
        };
        let parse = |k: &str| accepted[k].as_str().and_then(|s| Uuid::parse_str(s).ok());
        let (Some(call_id), Some(call_leg_id)) = (parse("callId"), parse("callLegId")) else {
            return Err(EnterpriseRejection::CallingUnavailable);
        };
        let now = self.now();
        let persisted: TelephonyResult<()> = async {
            let txn = self.db.begin().await?;
            db::exec(
                &txn,
                "INSERT INTO telephony_legs (id, calling_call_id, calling_call_leg_id, tenant_id, application_id, \
                   environment_id, direction, route_class, status, generation, max_duration_seconds, \
                   media_attachment, usage_status, enterprise_device_id, edge_channel_ref, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'INBOUND', 'ENTERPRISE_INTERNAL', 'PLANNED', 1, $7, \
                   'APPLICATION', 'NOT_APPLICABLE', $8, $9, $10, $10)",
                vec![
                    leg_id.into(),
                    call_id.into(),
                    call_leg_id.into(),
                    device.tenant_id.into(),
                    device.application_id.into(),
                    device.environment_id.into(),
                    ENTERPRISE_MAX_LEG_SECONDS.into(),
                    device.id.into(),
                    event.channel.clone().into(),
                    now.into(),
                ],
            )
            .await?;
            let leg = legs::lock(&txn, leg_id).await?;
            let leg = legs::transition_in(
                &txn,
                &leg,
                &Change::to(LegStatus::Originating, NormalizedEvent::LegAccepted, now).silent(),
            )
            .await?
            .unwrap_or(leg);
            legs::transition_in(
                &txn,
                &leg,
                &Change::to(LegStatus::Ringing, NormalizedEvent::LegRinging, now),
            )
            .await?;
            txn.commit().await?;
            Ok(())
        }
        .await;
        if persisted.is_err() {
            return Err(EnterpriseRejection::Internal);
        }
        Ok(leg_id)
    }

    /// The live enterprise leg of an edge channel, if any.
    pub(crate) async fn enterprise_leg_of_channel(
        &self,
        channel: &str,
    ) -> TelephonyResult<Option<LegRow>> {
        db::one(
            &self.db,
            &format!(
                "SELECT {LEG_COLUMNS} FROM telephony_legs WHERE edge_channel_ref = $1 \
                 AND route_class = 'ENTERPRISE_INTERNAL'"
            ),
            vec![channel.to_string().into()],
        )
        .await
    }

    /// Progress of an enterprise leg's channel (answer, hang-up, hold) becomes the leg's
    /// normalized events; a key goes to Calling's IVR (never stored, never logged).
    pub(crate) async fn handle_enterprise_channel_event(
        &self,
        leg: LegRow,
        event: &EdgeEvent,
    ) -> TelephonyResult<()> {
        let now = self.now();
        match &event.kind {
            EdgeEventKind::Dtmf { digit } => {
                if leg.status()?.is_terminal() {
                    return Ok(());
                }
                let payload = json!({
                    "schemaVersion": "1",
                    "telephonyLegId": leg.id,
                    "callId": leg.calling_call_id,
                    "callLegId": leg.calling_call_leg_id,
                    "digit": digit.to_string(),
                });
                let result = match self.calling.deliver_keypad(&payload).await {
                    Ok(()) => "DELIVERED",
                    Err(_) => "NOT_DELIVERED",
                };
                crate::metrics::inc("telephony_enterprise_keypad_total", &[("result", result)]);
                Ok(())
            }
            kind => {
                let change = match kind {
                    EdgeEventKind::Answered => {
                        Change::to(LegStatus::Answered, NormalizedEvent::LegAnswered, now)
                    }
                    EdgeEventKind::Held => {
                        Change::to(LegStatus::Held, NormalizedEvent::LegHeld, now)
                    }
                    EdgeEventKind::Unheld => {
                        Change::to(LegStatus::Connected, NormalizedEvent::LegResumed, now)
                    }
                    EdgeEventKind::Hangup { .. } => {
                        let reason = if leg.answered_at.is_some() {
                            EndReason::Completed
                        } else {
                            EndReason::Cancelled
                        };
                        Change::ended(NormalizedEvent::LegEnded, reason, now)
                    }
                    _ => return Ok(()),
                };
                let txn = self.db.begin().await?;
                let locked = legs::lock(&txn, leg.id).await?;
                if !locked.status()?.is_terminal() {
                    legs::transition_in(&txn, &locked, &change).await?;
                }
                txn.commit().await?;
                Ok(())
            }
        }
    }

    /// Executes ANSWER / PROMPT / TERMINATE / HOLD / RESUME on an enterprise leg directly
    /// on the edge (there is no provider). Unavailable edge: retried; a channel that is
    /// gone: the leg ends.
    pub(crate) async fn execute_enterprise_control(
        &self,
        command: CommandRow,
        kind: CommandKind,
        leg: LegRow,
    ) -> TelephonyResult<Executed> {
        let now = self.now();
        let Some(channel) = leg.edge_channel_ref.clone() else {
            self.finish_command_pub(command.id, "FAILED", "NO_CHANNEL", now)
                .await?;
            return Ok(Executed::DefiniteFailure("NO_CHANNEL"));
        };
        let status = leg.status()?;
        if status.is_terminal() {
            self.finish_command_pub(command.id, "SUCCEEDED", "ALREADY_ENDED", now)
                .await?;
            return Ok(Executed::Done);
        }
        let outcome = match kind {
            CommandKind::Answer if status == LegStatus::Ringing => self.edge.answer(&channel).await,
            CommandKind::Answer => Ok(()),
            CommandKind::Prompt => {
                let prompt = command.payload["prompt"].as_str().unwrap_or("");
                let Some(sound) = prompt_sound(prompt) else {
                    self.finish_command_pub(command.id, "FAILED", "INVALID_PROMPT", now)
                        .await?;
                    return Ok(Executed::DefiniteFailure("INVALID_PROMPT"));
                };
                self.edge.play(&channel, sound).await
            }
            CommandKind::Capture => {
                return self.start_capture(command, leg, channel).await;
            }
            CommandKind::CaptureStop => {
                return self.stop_capture(command).await;
            }
            CommandKind::Terminate => self.edge.hangup(&channel).await,
            CommandKind::Hold => self.edge.hold(&channel).await,
            CommandKind::Resume => self.edge.resume(&channel).await,
            _ => Err(EdgeError::Refused),
        };
        let change = match (&outcome, kind) {
            (Ok(()), CommandKind::Answer) if status == LegStatus::Ringing => Some(Change::to(
                LegStatus::Answered,
                NormalizedEvent::LegAnswered,
                now,
            )),
            (Ok(()), CommandKind::Hold) => {
                Some(Change::to(LegStatus::Held, NormalizedEvent::LegHeld, now))
            }
            (Ok(()), CommandKind::Resume) => Some(Change::to(
                LegStatus::Connected,
                NormalizedEvent::LegResumed,
                now,
            )),
            (Ok(()) | Err(EdgeError::NotFound), CommandKind::Terminate)
            | (Err(EdgeError::NotFound), _) => {
                let reason = if leg.answered_at.is_some() {
                    EndReason::Completed
                } else {
                    EndReason::Cancelled
                };
                Some(Change::ended(NormalizedEvent::LegEnded, reason, now))
            }
            _ => None,
        };
        match outcome {
            Ok(()) | Err(EdgeError::NotFound) => {
                let txn = self.db.begin().await?;
                let locked = legs::lock(&txn, leg.id).await?;
                if let Some(change) = change {
                    if !locked.status()?.is_terminal() {
                        legs::transition_in(&txn, &locked, &change).await?;
                    }
                }
                txn.commit().await?;
                let code = if outcome.is_ok() {
                    "DONE"
                } else {
                    "CHANNEL_GONE"
                };
                self.finish_command_pub(command.id, "SUCCEEDED", code, now)
                    .await?;
                crate::metrics::inc(
                    "telephony_enterprise_commands_total",
                    &[("kind", kind.as_str()), ("result", code)],
                );
                Ok(Executed::Done)
            }
            Err(EdgeError::Refused) => {
                self.finish_command_pub(command.id, "FAILED", "EDGE_REFUSED", now)
                    .await?;
                crate::metrics::inc(
                    "telephony_enterprise_commands_total",
                    &[("kind", kind.as_str()), ("result", "EDGE_REFUSED")],
                );
                Ok(Executed::DefiniteFailure("EDGE_REFUSED"))
            }
            Err(EdgeError::Unavailable | EdgeError::Timeout) => {
                self.reschedule_pub(&command, "EDGE_UNAVAILABLE").await
            }
        }
    }

    /// Reconciles one enterprise leg against the edge (called by the leg reconciler): a
    /// channel that vanished ends the leg (Calling learns) and raises an incident.
    pub(crate) async fn reconcile_enterprise_leg(&self, leg: &LegRow) -> TelephonyResult<bool> {
        let now = self.now();
        if leg.status()?.is_terminal() {
            return Ok(false);
        }
        let Some(channel) = &leg.edge_channel_ref else {
            return Ok(false);
        };
        match self.edge.query(channel).await {
            ChannelState::Gone => {
                let txn = self.db.begin().await?;
                let locked = legs::lock(&txn, leg.id).await?;
                if !locked.status()?.is_terminal() {
                    let reason = if locked.answered_at.is_some() {
                        EndReason::Completed
                    } else {
                        EndReason::Cancelled
                    };
                    legs::transition_in(
                        &txn,
                        &locked,
                        &Change::ended(NormalizedEvent::LegEnded, reason, now),
                    )
                    .await?;
                }
                incidents::raise(
                    &txn,
                    &Incident::new(
                        "ORPHAN_ENTERPRISE_SIP_LEG",
                        Severity::Low,
                        format!("enterprise-leg:{}", leg.id),
                    )
                    .leg(leg),
                    now,
                )
                .await?;
                txn.commit().await?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Enterprise legs live beyond their bound are hung up at the edge first.
    pub(crate) async fn hang_up_enterprise(&self, leg: &LegRow) {
        if let Some(channel) = &leg.edge_channel_ref {
            let _ = self.edge.hangup(channel).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_prompt_maps_to_a_safe_sound() {
        for (prompt, sound, _) in PROMPT_SOUNDS {
            assert_eq!(prompt_sound(prompt), Some(*sound));
            assert!(sound
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        }
        assert_eq!(prompt_sound("../../etc/passwd"), None);
        assert_eq!(prompt_sound("tt-monkeys"), None);
    }

    /// Where the edge's GSM sounds are installed (asterisk-core-sounds-en-gsm), each fits
    /// its prompt's nominal length (GSM: 1650 bytes per second).
    #[test]
    fn installed_sounds_fit_the_nominal_prompt_length() {
        let dir = std::path::Path::new("/usr/share/asterisk/sounds/en");
        for (prompt, sound, seconds) in PROMPT_SOUNDS {
            let Ok(meta) = std::fs::metadata(dir.join(format!("{sound}.gsm"))) else {
                continue;
            };
            assert!(
                meta.len() <= u64::from(*seconds) * 1650,
                "{prompt}: {sound} is longer than {seconds} s"
            );
        }
    }
}
