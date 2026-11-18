//! Server-side forwarding of MANAGED messages (S11).
//!
//! `POST /v1/conversations/{id}/messages/{messageId}/forward` `{ targetConversationId,
//! clientMessageId }` copies a message the acting member can read into another
//! conversation it can send to, as a NEW message of the acting member:
//!
//! * both conversations must be MANAGED: the server holds the text of neither side of a
//!   PRIVATE_E2EE conversation, so an E2EE message is forwarded CLIENT-SIDE (the client
//!   decrypts and sends new ciphertext to the target group); the server refuses it
//!   (`CONTENT_REJECTED` / `E2EE_FORWARD_CLIENT_SIDE`), and likewise into an E2EE target;
//! * the source message must be ACTIVE, an application message inside the member's history
//!   window, and carry no attachments (attachments are not copied);
//! * the environment's policy must allow forwarding (`messages.forwarding`, default on);
//! * the TARGET's send rules all apply, exactly as for a normal send (membership, read-only
//!   and muted members, announcement-only, blocks, the Communications personal policy,
//!   kill switches and rate limits);
//! * provenance: the copy is marked forwarded. The source conversation and message are
//!   recorded server-side only (never shown); the ORIGINAL sender is recorded and shown only
//!   when the policy says so (`messages.forwardShowsOriginalSender`, default off). Forwarding
//!   a forward keeps the original sender.
//! * idempotent per `clientMessageId` like any send.

use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    load_message,
    managed::{self, DecryptPurpose, Forwarded},
    SendRequest,
};
use crate::{
    conversations::{access, policy},
    domain::SecurityProfile,
    error::{Error, ErrorCode, Result},
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForwardRequest {
    pub target_conversation_id: Uuid,
    pub client_message_id: Uuid,
}

fn client_side() -> Error {
    Error::reason(ErrorCode::ContentRejected, "E2EE_FORWARD_CLIENT_SIDE")
}

pub async fn forward(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    source_id: Uuid,
    message_id: Uuid,
    request: ForwardRequest,
) -> Result<(u16, Value)> {
    if request.client_message_id.is_nil() {
        return Err(Error::validation("clientMessageId must not be nil"));
    }
    if request.target_conversation_id == source_id {
        return Err(Error::validation(
            "targetConversationId must be another conversation",
        ));
    }
    let (source, member) =
        access::as_member(&state.db, ctx, source_id, acting.principal_id).await?;
    let row = load_message(&state.db, source.id, message_id, false)
        .await?
        .filter(|r| r.sequence >= member.visible_from_sequence && r.message_kind == "APPLICATION")
        .ok_or(Error::code(ErrorCode::NotFound))?;
    if source.profile() != SecurityProfile::Managed {
        return Err(client_side());
    }
    if !row.is_active() {
        return Err(Error::reason(ErrorCode::ContentRejected, "NOT_ACTIVE"));
    }
    if row.attachment_count > 0 {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "FORWARD_ATTACHMENTS_UNSUPPORTED",
        ));
    }
    let effective = policy::current(&state.db, ctx.scope.environment_id).await?;
    if !effective.messages.forwarding {
        return Err(Error::reason(
            ErrorCode::FeatureDisabled,
            "FORWARDING_DISABLED",
        ));
    }
    // Every rule of a normal send into the TARGET.
    let (target, _member, effective) =
        super::send_prelude(state, ctx, acting, request.target_conversation_id).await?;
    if target.profile() != SecurityProfile::Managed {
        return Err(client_side());
    }
    let text = managed::text_of(state, &row, DecryptPurpose::MemberRead)
        .await
        .ok_or(Error::reason(ErrorCode::ContentRejected, "NOT_ACTIVE"))?;
    let original_sender = row
        .forwarded_from_message_id
        .map(|_| row.forwarded_sender_principal_id)
        .unwrap_or(Some(row.sender_principal_id));
    let provenance = Forwarded {
        from_message: row.id,
        from_conversation: source.id,
        sender: if effective.messages.forward_shows_original_sender {
            original_sender
        } else {
            None
        },
    };
    let send: SendRequest = serde_json::from_value(
        json!({ "clientMessageId": request.client_message_id, "text": text }),
    )
    .map_err(|_| Error::code(ErrorCode::InternalError))?;
    let out = managed::send_with(
        state,
        ctx,
        acting,
        &target,
        &effective,
        send,
        Some(provenance),
    )
    .await?;
    if out.0 == 201 {
        crate::metrics::inc("messaging_messages_forwarded_total", &[]);
    }
    Ok(out)
}
