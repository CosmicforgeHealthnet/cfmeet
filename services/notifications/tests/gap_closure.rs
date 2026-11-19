//! The Notification Service's share of the 17 November backend gap audit, against a real
//! database:
//!
//! * C1 — the call fast lane's cancellation (I3c,
//!   `POST /internal/v1/push/call-invitations/cancellations`): same collapse id as the
//!   ring, short TTL, metadata only, never a VoIP push, only the invited account's
//!   endpoints, off until `CALL_INVITATIONS` is ACTIVE;
//! * C2 — missed calls, ready voicemails and ready recordings reach the named recipient's
//!   inbox and push, under their own preference categories;
//! * S1 — an organization conversation's wake-up (identified by its conversation id only)
//!   pushes like a personal one, and a per-conversation mute applies to it;
//! * S15 — the Hub's incoming-call, missed-call, contact-request and group-invite switches
//!   (copied by N10) are honoured where the service routes those notifications;
//! * M12 — meeting notices reach the inbox and push, under the meeting preferences;
//! * G19 — organization archived / reactivated notices reach the inbox and push.

mod common;

use axum::http::Method;
use cf_notification_client::Producer;
use chrono::Utc;
use common::*;
use notifications::{
    authorities::{
        AccountDevice, ConferenceRecipient, ConferenceRecipients, EntitledPrincipal,
        WakeupAuthorization,
    },
    preferences::{copy_account, HubPreferences},
};
use serde_json::{json, Value};
use uuid::Uuid;

const U: &str = "5e2a1b3c-4d5e-4f60-8a71-b2c3d4e5f607";

/// Registers a LOCAL_TEST push endpoint for one of `account`'s Account devices.
async fn endpoint(h: &Harness, account: &str, handle: &str) -> Value {
    let device = Uuid::new_v4();
    h.owners
        .contacts
        .lock()
        .unwrap()
        .get_mut(account)
        .unwrap()
        .devices
        .push(AccountDevice {
            account_device_ref: device.to_string(),
            state: "ACTIVE".into(),
        });
    h.user(
        account,
        "PUT",
        &format!("/devices/{device}/push"),
        None,
        Some(json!({"platform": "LOCAL_TEST", "handle": handle})),
    )
    .await
    .expect(200)["pushTargetRef"]
        .clone()
}

/// The Hub's seven account switches as N10 copies them (`version` is the Hub row version).
fn hub(account: &str, version: i64) -> HubPreferences {
    HubPreferences {
        account_ref: account.to_string(),
        version,
        direct_messages: true,
        group_messages: true,
        incoming_calls: true,
        missed_calls: true,
        contact_requests: true,
        group_invitations: true,
        meeting_invitations: true,
    }
}

async fn ring(h: &Harness, target: &Value, account: &str) -> Value {
    let calling = h.peers.calling.clone();
    h.signed(
        &calling,
        Method::POST,
        "/internal/v1/push/call-invitations",
        Some(json!({
            "pushTargetRefs": [target],
            "payload": {"v": 1},
            "expiresAt": (Utc::now() + chrono::Duration::seconds(30)).to_rfc3339(),
            "recipientAccountRef": account,
        })),
        &[],
    )
    .await
    .expect(200)["results"][0]
        .clone()
}

async fn cancel(h: &Harness, targets: Vec<Value>, account: &str) -> Vec<Value> {
    let calling = h.peers.calling.clone();
    h.signed(
        &calling,
        Method::POST,
        "/internal/v1/push/call-invitations/cancellations",
        Some(json!({ "pushTargetRefs": targets, "recipientAccountRef": account })),
        &[],
    )
    .await
    .expect(200)["results"]
        .as_array()
        .unwrap()
        .clone()
}

/// Push deliveries of `channel` per status for every notification of `route_category`.
async fn push_status(h: &Harness, recipient: &str, route_category: &str) -> Vec<String> {
    let rows = h
        .text(&format!(
            "SELECT string_agg(d.status || COALESCE(':' || d.last_error_class, ''), ',' ORDER BY d.status)
               FROM notif_deliveries d JOIN notif_notifications n ON n.id = d.notification_id
              WHERE d.channel = 'PUSH' AND n.recipient_ref = '{recipient}' AND n.route_category = '{route_category}'"
        ))
        .await;
    rows.map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn c1_rings_are_cancelled_on_the_invited_accounts_endpoints_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let mine = endpoint(&h, &alice(), "alice-h").await;
    let theirs = endpoint(&h, &bob(), "bob-h").await;

    // Off until the operator activates CALL_INVITATIONS: nothing is sent.
    let off = cancel(&h, vec![mine.clone()], &alice()).await;
    assert_eq!(off[0]["outcome"], "NOT_CONFIGURED");
    assert!(h.push.sent.lock().unwrap().is_empty());

    h.set_category("CALL_INVITATIONS", "ACTIVE").await;
    assert_eq!(ring(&h, &mine, &alice()).await["outcome"], "SENT");
    let results = cancel(&h, vec![mine.clone(), theirs.clone()], &alice()).await;
    assert_eq!(results[0]["outcome"], "SENT");
    // Another account's endpoint is indistinguishable from an unknown one.
    assert_eq!(results[1]["outcome"], "UNKNOWN_TARGET");

    let sent = h.push.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2, "one ring and one cancellation");
    assert_eq!(sent[1].0, "alice-h");
    // Metadata only: no call id, number, name or private detail.
    assert_eq!(sent[1].1, json!({ "v": 1, "type": "call-cancel" }));
    let options = h.push.options.lock().unwrap().clone();
    // The cancellation replaces a ring still waiting at the provider (same collapse id),
    // lives briefly and is never a PushKit VoIP push.
    assert_eq!(options[1].collapse_id, options[0].collapse_id);
    assert_eq!(options[1].collapse_id.as_deref(), Some("cfmeet-call"));
    assert_eq!(
        options[1].ttl_seconds,
        Some(notifications::push::CALL_CANCEL_TTL_SECONDS)
    );
    assert!(!options[1].voip);

    // A PushKit (APNS_VOIP) endpoint never receives a cancellation: every VoIP push must
    // report a new call to CallKit.
    h.exec(&format!(
        "UPDATE notif_push_endpoints SET platform = 'APNS_VOIP' WHERE id = '{}'",
        mine.as_str().unwrap()
    ))
    .await
    .unwrap();
    let r = cancel(&h, vec![mine.clone()], &alice()).await;
    assert_eq!(r[0]["outcome"], "SKIPPED_VOIP");
    assert_eq!(h.push.sent.lock().unwrap().len(), 2);

    // Validation: a recipient is required and must be canonical; 1–20 targets; no call
    // detail is accepted.
    let calling = h.peers.calling.clone();
    for body in [
        json!({ "pushTargetRefs": [theirs] }),
        json!({ "pushTargetRefs": [theirs], "recipientAccountRef": "cfmeet-user:NOT-A-UUID-0000-0000-000000000000" }),
        json!({ "pushTargetRefs": [], "recipientAccountRef": alice() }),
        json!({ "pushTargetRefs": [theirs], "recipientAccountRef": alice(), "callId": U }),
    ] {
        h.signed(
            &calling,
            Method::POST,
            "/internal/v1/push/call-invitations/cancellations",
            Some(body),
            &[],
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    }
    // Only a holder of notifications-call-push.dispatch may call it.
    let messaging = h.peers.messaging.clone();
    let denied = h
        .signed(
            &messaging,
            Method::POST,
            "/internal/v1/push/call-invitations/cancellations",
            Some(json!({ "pushTargetRefs": [mine], "recipientAccountRef": alice() })),
            &[],
        )
        .await;
    assert_eq!(denied.status.as_u16(), 403);
}

#[tokio::test]
async fn s15_the_hub_incoming_call_switch_silences_rings_and_their_cancellations() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let target = endpoint(&h, &alice(), "alice-h").await;
    h.set_category("CALL_INVITATIONS", "ACTIVE").await;
    // The Hub switch, as the N10 copy writes it.
    let mut off = hub(&alice(), 3);
    off.incoming_calls = false;
    assert!(copy_account(&h.state.db, &off).await.unwrap());
    assert_eq!(
        ring(&h, &target, &alice()).await["outcome"],
        "PREFERENCE_OFF"
    );
    assert_eq!(
        cancel(&h, vec![target.clone()], &alice()).await[0]["outcome"],
        "PREFERENCE_OFF"
    );
    assert!(h.push.sent.lock().unwrap().is_empty());
    // Switched back on in the Hub (a newer version): rings again.
    assert!(copy_account(&h.state.db, &hub(&alice(), 4)).await.unwrap());
    assert_eq!(ring(&h, &target, &alice()).await["outcome"], "SENT");
    // The push channel of INCOMING_CALLS (service-side channel matrix) also applies.
    let version = h
        .user(&alice(), "GET", "/preferences", None, None)
        .await
        .json["version"]
        .clone();
    h.user(
        &alice(),
        "PUT",
        "/preferences",
        None,
        Some(json!({"expectedVersion": version, "channels": {"incomingCalls": {"push": false}}})),
    )
    .await
    .expect(200);
    assert_eq!(
        ring(&h, &target, &alice()).await["outcome"],
        "PREFERENCE_OFF"
    );
}

#[tokio::test]
async fn s15_and_c2_hub_switches_govern_missed_calls_contact_requests_and_group_invites() {
    let Some(h) = Harness::new().await else {
        return;
    };
    endpoint(&h, &alice(), "alice-h").await;
    endpoint(&h, &bob(), "bob-h").await;
    for c in [
        "MISSED_CALLS",
        "VOICEMAIL",
        "CONTACT_REQUESTS",
        "GROUP_INVITATIONS",
    ] {
        h.set_category(c, "ACTIVE").await;
    }
    // Alice turned the three switches off in the Hub; Bob left them on.
    let mut off = hub(&alice(), 2);
    off.missed_calls = false;
    off.contact_requests = false;
    off.group_invitations = false;
    assert!(copy_account(&h.state.db, &off).await.unwrap());

    let conversation = Uuid::new_v4().to_string();
    for who in [alice(), bob()] {
        let calling = h.peers.calling.clone();
        h.ingest(
            &calling,
            vec![
                envelope(
                    Producer::Calling,
                    "calling.call.participant-missed",
                    std::slice::from_ref(&who),
                    &[("callId", json!(U)), ("callKind", json!("AUDIO"))],
                    None,
                ),
                envelope(
                    Producer::Calling,
                    "calling.voicemail.ready",
                    std::slice::from_ref(&who),
                    &[("callId", json!(U)), ("voiceMessageId", json!(U))],
                    None,
                ),
                envelope(
                    Producer::Calling,
                    "calling.recording.ready",
                    std::slice::from_ref(&who),
                    &[("callId", json!(U)), ("recordingId", json!(U))],
                    None,
                ),
            ],
        )
        .await
        .expect(200);
        let other = if who == alice() { bob() } else { alice() };
        let hub_peer = h.peers.communications.clone();
        h.ingest(
            &hub_peer,
            vec![envelope(
                Producer::Communications,
                "communications.connection.requested",
                std::slice::from_ref(&who),
                &[("actorRef", json!(other)), ("connectionId", json!(U))],
                None,
            )],
        )
        .await
        .expect(200);
        let messaging = h.peers.messaging.clone();
        h.ingest(
            &messaging,
            vec![envelope(
                Producer::Messaging,
                "messaging.member.invited",
                std::slice::from_ref(&who),
                &[
                    ("conversationId", json!(conversation)),
                    ("eventSequence", json!(3)),
                ],
                None,
            )],
        )
        .await
        .expect(200);
    }
    h.process_all().await;
    for category in ["MISSED_CALLS", "CONTACT_REQUESTS", "GROUP_INVITATIONS"] {
        assert_eq!(
            push_status(&h, &alice(), category).await,
            vec!["SUPPRESSED:CATEGORY_OFF"],
            "alice {category}"
        );
        assert_eq!(
            push_status(&h, &bob(), category).await,
            vec!["PENDING"],
            "bob {category}"
        );
    }
    // C2: voicemail and recording (VOICEMAIL category, left on) reach both.
    assert_eq!(
        push_status(&h, &alice(), "VOICEMAIL").await,
        vec!["PENDING", "PENDING"]
    );
    // Category off keeps a silent inbox item; both recipients have one item per notice.
    for who in [alice(), bob()] {
        assert_eq!(
            h.count(&format!(
                "SELECT count(*)::bigint FROM notif_inbox_items WHERE recipient_ref = '{who}'"
            ))
            .await,
            5,
            "{who}"
        );
    }
    assert_eq!(
        h.count(&format!(
            "SELECT count(*)::bigint FROM notif_inbox_items WHERE recipient_ref = '{}' AND silent",
            alice()
        ))
        .await,
        3
    );
    // The inbox actions point at the call history, the voicemail and the recording.
    for (template, action) in [
        ("call.missed", "OPEN_CALL_HISTORY"),
        ("call.voicemail", "OPEN_VOICEMAIL"),
        ("call.recording", "OPEN_RECORDING"),
    ] {
        assert_eq!(
            h.text(&format!(
                "SELECT action_kind FROM notif_inbox_items WHERE recipient_ref = '{}' AND template_key = '{template}'",
                bob()
            ))
            .await
            .as_deref(),
            Some(action)
        );
    }
}

#[tokio::test]
async fn s1_organization_conversations_wake_up_and_honour_per_conversation_mutes() {
    let Some(h) = Harness::new().await else {
        return;
    };
    endpoint(&h, &alice(), "alice-h").await;
    endpoint(&h, &bob(), "bob-h").await;
    h.set_category("MESSAGE_WAKEUPS", "ACTIVE").await;
    // Two organization chats (an organization's own scope: the service only sees ids).
    let (muted, open) = (Uuid::new_v4(), Uuid::new_v4());
    for c in [muted, open] {
        h.owners.wakeups.lock().unwrap().insert(
            c,
            WakeupAuthorization {
                conversation_active: true,
                security_profile: "MANAGED".into(),
                conversation_kind: "GROUP".into(),
                entitled: vec![
                    EntitledPrincipal {
                        principal: Uuid::new_v4().to_string(),
                        account_ref: Some(alice()),
                        mentions_recipient: false,
                    },
                    EntitledPrincipal {
                        principal: Uuid::new_v4().to_string(),
                        account_ref: Some(bob()),
                        mentions_recipient: false,
                    },
                ],
            },
        );
    }
    // Bob mutes one of them (per conversation, not per organization).
    h.user(
        &bob(),
        "PUT",
        "/preferences",
        None,
        Some(json!({"expectedVersion": 0, "conversations": {muted.to_string(): {"notifications": "MUTED"}}})),
    )
    .await
    .expect(200);
    let messaging = h.peers.messaging.clone();
    let wake = |c: Uuid| {
        envelope(
            Producer::Messaging,
            "messaging.message.created",
            &[alice(), bob()],
            &[
                ("conversationId", json!(c.to_string())),
                ("eventSequence", json!(12)),
                ("conversationKind", json!("GROUP")),
                ("securityProfile", json!("MANAGED")),
            ],
            None,
        )
    };
    h.ingest(&messaging, vec![wake(muted), wake(open)])
        .await
        .expect(200);
    h.process_all().await;
    let status = |who: String, c: Uuid| {
        let h = &h;
        async move {
            h.text(&format!(
                "SELECT d.status || COALESCE(':' || d.last_error_class, '') FROM notif_deliveries d
                   JOIN notif_notifications n ON n.id = d.notification_id
                  WHERE d.channel = 'PUSH' AND n.recipient_ref = '{who}' AND n.params->>'conversationId' = '{c}'"
            ))
            .await
        }
    };
    assert_eq!(status(alice(), muted).await.as_deref(), Some("PENDING"));
    assert_eq!(status(alice(), open).await.as_deref(), Some("PENDING"));
    assert_eq!(
        status(bob(), muted).await.as_deref(),
        Some("SUPPRESSED:MUTED")
    );
    assert_eq!(status(bob(), open).await.as_deref(), Some("PENDING"));
    // The wake-up carries ids only (never text, ciphertext or a sender).
    h.push_worker().tick(Utc::now()).await.unwrap();
    for (_, payload) in h.push.sent.lock().unwrap().iter() {
        let keys: Vec<&str> = payload
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for k in keys {
            assert!(
                ["v", "type", "conversationId", "eventSequence", "sequence"].contains(&k),
                "{k}"
            );
        }
    }
}

#[tokio::test]
async fn m12_meeting_notices_reach_the_inbox_and_push_under_the_meeting_preferences() {
    let Some(h) = Harness::new().await else {
        return;
    };
    endpoint(&h, &alice(), "alice-h").await;
    endpoint(&h, &bob(), "bob-h").await;
    // Push and in-app only (email stays as it is; no email is sent by this test).
    h.set_mode("MEETING_NOTIFICATION", "IN_APP", "ACTIVE").await;
    h.set_mode("MEETING_NOTIFICATION", "PUSH", "ACTIVE").await;
    let conference = Uuid::new_v4();
    h.owners.conferences.lock().unwrap().insert(
        conference,
        ConferenceRecipients {
            conference_id: conference.to_string(),
            still_true: true,
            presented_title: "Weekly sync".into(),
            starts_at: "2026-11-20T09:00:00Z".into(),
            time_zone: Some("UTC".into()),
            join_path: Some("/abc-defg-hij".into()),
            ends_at: None,
            recipients: vec![
                ConferenceRecipient {
                    reference: alice(),
                    email: None,
                },
                ConferenceRecipient {
                    reference: bob(),
                    email: None,
                },
            ],
        },
    );
    // Bob turned meeting-invitation notices off (rescheduled / cancelled use it).
    h.user(
        &bob(),
        "PUT",
        "/preferences",
        None,
        Some(json!({"expectedVersion": 0, "categories": {"meetingInvitations": false}})),
    )
    .await
    .expect(200);
    let meeting = h.peers.meeting.clone();
    h.ingest(
        &meeting,
        vec![envelope(
            Producer::MeetingService,
            "meeting-service.conference.cancelled",
            &[alice(), bob()],
            &[
                ("conferenceId", json!(conference.to_string())),
                ("startsAt", json!("2026-11-20T09:00:00Z")),
            ],
            Some(&format!("meeting-notification:{conference}:1")),
        )],
    )
    .await
    .expect(200);
    h.process_all().await;
    assert_eq!(
        push_status(&h, &alice(), "MEETING_NOTIFICATION").await,
        vec!["PENDING"]
    );
    assert_eq!(
        push_status(&h, &bob(), "MEETING_NOTIFICATION").await,
        vec!["SUPPRESSED:CATEGORY_OFF"]
    );
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_inbox_items WHERE template_key = 'meeting.cancelled'")
            .await,
        2
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*)::bigint FROM notif_inbox_items WHERE template_key = 'meeting.cancelled' AND silent AND recipient_ref = '{}'",
            bob()
        ))
        .await,
        1
    );
    // The push wakes the device to its inbox: metadata only, no title or link.
    assert_eq!(h.push_worker().tick(Utc::now()).await.unwrap().sent, 1);
    let sent = h.push.sent.lock().unwrap().clone();
    assert_eq!(sent[0].0, "alice-h");
    assert_eq!(sent[0].1["type"], "inbox");
    assert!(!sent[0].1.to_string().contains("Weekly"));
    // No email was produced: the EMAIL channel is still OFF.
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_email_messages")
            .await,
        0
    );
}

#[tokio::test]
async fn g19_organization_archive_and_reactivation_reach_members() {
    let Some(h) = Harness::new().await else {
        return;
    };
    endpoint(&h, &alice(), "alice-h").await;
    h.set_category("ORGANIZATION_UPDATES", "ACTIVE").await;
    let orgs = h.peers.organizations.clone();
    let out = h
        .ingest(
            &orgs,
            [
                "organizations.organization.archived",
                "organizations.organization.reactivated",
            ]
            .iter()
            .map(|t| {
                envelope(
                    Producer::Organizations,
                    t,
                    &[alice()],
                    &[("organizationId", json!(U))],
                    None,
                )
            })
            .collect(),
        )
        .await;
    for r in out.expect(200)["results"].as_array().unwrap() {
        assert_eq!(r["outcome"], "ACCEPTED", "{r}");
    }
    h.process_all().await;
    for template in ["org.organization.archived", "org.organization.reactivated"] {
        assert_eq!(
            h.text(&format!(
                "SELECT action_kind || ':' || action_ref::text FROM notif_inbox_items WHERE template_key = '{template}'"
            ))
            .await,
            Some(format!("OPEN_ORGANIZATION:{U}")),
            "{template}"
        );
    }
    assert_eq!(
        push_status(&h, &alice(), "ORGANIZATION_UPDATES").await,
        vec!["PENDING", "PENDING"]
    );
    let items = h.user(&alice(), "GET", "", None, None).await;
    let text = items.expect(200).to_string();
    assert!(text.contains("Organization archived"), "{text}");
    assert!(text.contains("Organization reactivated"), "{text}");
    // Another producer may not publish them.
    let meeting = h.peers.meeting.clone();
    let refused = h
        .ingest(
            &meeting,
            vec![envelope(
                Producer::Organizations,
                "organizations.organization.archived",
                &[alice()],
                &[("organizationId", json!(U))],
                None,
            )],
        )
        .await;
    assert_ne!(
        refused.json["results"][0]["outcome"].as_str(),
        Some("ACCEPTED")
    );
}
