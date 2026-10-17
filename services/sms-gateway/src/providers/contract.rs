//! The SMS provider adapter contract: one suite every adapter must pass (the
//! deterministic adapter in CI; a real aggregator adapter against its sandbox before it
//! may be enabled). It checks the guarantees the transport domain relies on.
//!
//! 1. Submission answers a valid provider reference (or one of the four outcomes).
//! 2. With native idempotency, the same key is the same message (no second effect).
//! 3. A status query by key finds the message; an unknown key is never "found".
//! 4. Callbacks: a correctly signed receipt / inbound message authenticates and parses;
//!    a tampered body, a missing signature or a stale timestamp is refused BEFORE
//!    parsing; an unparseable body is MALFORMED.
//! 5. No plaintext (numbers, content) in the debug form of requests or callbacks.

use axum::http::HeaderMap;
use chrono::{DateTime, Duration, Utc};
use zeroize::Zeroizing;

use super::{
    valid_reference, Callback, CallbackError, QueryOutcome, ReceiptStatus, SmsProvider,
    SubmitOutcome, SubmitRequest,
};
use crate::sms::segments::Encoding;

/// Produces callbacks signed exactly as the adapter's provider would.
pub trait ContractFixture: SmsProvider {
    fn signed_inbound(
        &self,
        from: &str,
        to: &str,
        text: &str,
        at: DateTime<Utc>,
    ) -> (HeaderMap, Vec<u8>);

    fn signed_receipt(
        &self,
        provider_message_ref: &str,
        status: ReceiptStatus,
        at: DateTime<Utc>,
    ) -> (HeaderMap, Vec<u8>);
}

fn request(key: &str) -> SubmitRequest {
    SubmitRequest {
        from: Zeroizing::new("+15550100001".into()),
        to: Zeroizing::new("+15550100002".into()),
        body: Zeroizing::new("contract check 7731".into()),
        encoding: Encoding::Gsm7,
        segments: 1,
        idempotency_key: key.to_string(),
        validity_until: Utc::now() + Duration::hours(1),
    }
}

/// Runs the contract; `Err` names the first violated guarantee.
pub async fn run<P: ContractFixture>(provider: &P, now: DateTime<Utc>) -> Result<(), String> {
    let id = provider.id();
    if !(1..=64).contains(&id.len())
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("1: the adapter id is not [a-z0-9-]{1,64}".into());
    }
    let key = format!("contract-{}", uuid::Uuid::new_v4().simple());
    let req = request(&key);
    let debug = format!("{req:?}");
    if debug.contains("15550100001") || debug.contains("7731") {
        return Err("5: a submission's debug form shows plaintext".into());
    }
    let reference = match provider.submit(req).await {
        SubmitOutcome::Accepted {
            provider_message_ref,
            ..
        } if valid_reference(&provider_message_ref) => provider_message_ref,
        SubmitOutcome::Accepted { .. } => return Err("1: invalid provider reference".into()),
        other => return Err(format!("1: an accepting provider answered {other:?}")),
    };
    if provider.native_idempotency() {
        match provider.submit(request(&key)).await {
            SubmitOutcome::Accepted {
                provider_message_ref,
                ..
            } if provider_message_ref == reference => {}
            other => {
                return Err(format!(
                    "2: a repeated key was not the same message: {other:?}"
                ))
            }
        }
    }
    match provider.query(&key).await {
        QueryOutcome::Found {
            provider_message_ref,
            ..
        } if provider_message_ref == reference => {}
        other => return Err(format!("3: the submitted key was not found: {other:?}")),
    }
    if matches!(
        provider
            .query(&format!(
                "contract-missing-{}",
                uuid::Uuid::new_v4().simple()
            ))
            .await,
        QueryOutcome::Found { .. }
    ) {
        return Err("3: an unknown key was found".into());
    }
    // Callbacks.
    let (headers, body) = provider.signed_receipt(&reference, ReceiptStatus::Delivered, now);
    provider
        .authenticate_callback(&headers, &body, now.timestamp())
        .map_err(|e| format!("4: a signed receipt was refused: {e}"))?;
    match provider.parse_callback(&body) {
        Ok(Callback::Receipt(r))
            if r.provider_message_ref == reference && r.status == ReceiptStatus::Delivered => {}
        other => return Err(format!("4: the receipt did not parse: {other:?}")),
    }
    let (headers, body) = provider.signed_inbound("+15550100002", "+15550100001", "hi 9182", now);
    provider
        .authenticate_callback(&headers, &body, now.timestamp())
        .map_err(|e| format!("4: a signed inbound message was refused: {e}"))?;
    match provider.parse_callback(&body) {
        Ok(cb @ Callback::Inbound(_)) => {
            if format!("{cb:?}").contains("9182") || format!("{cb:?}").contains("15550100002") {
                return Err("5: a callback's debug form shows plaintext".into());
            }
        }
        other => return Err(format!("4: the inbound message did not parse: {other:?}")),
    }
    let mut tampered = body.clone();
    if let Some(last) = tampered.iter_mut().rev().find(|b| b.is_ascii_digit()) {
        *last = if *last == b'9' { b'8' } else { b'9' };
    }
    if !matches!(
        provider.authenticate_callback(&headers, &tampered, now.timestamp()),
        Err(CallbackError::Unauthenticated(_))
    ) {
        return Err("4: a tampered body authenticated".into());
    }
    if !matches!(
        provider.authenticate_callback(&HeaderMap::new(), &body, now.timestamp()),
        Err(CallbackError::Unauthenticated(_))
    ) {
        return Err("4: an unsigned callback authenticated".into());
    }
    let (old_headers, old_body) = provider.signed_inbound(
        "+15550100002",
        "+15550100001",
        "late",
        now - Duration::hours(1),
    );
    if !matches!(
        provider.authenticate_callback(&old_headers, &old_body, now.timestamp()),
        Err(CallbackError::Unauthenticated(_))
    ) {
        return Err("4: a stale callback authenticated".into());
    }
    if !matches!(
        provider.parse_callback(b"{\"kind\":\"OTHER\"}"),
        Err(CallbackError::Malformed(_))
    ) {
        return Err("4: an unparseable callback was accepted".into());
    }
    Ok(())
}
