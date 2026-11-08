//! SEC-009: server-side media eviction for legacy (`/api/v1`) meetings.
//!
//! A legacy room is named after the meeting identifier and a legacy participant's media
//! identity is its participant id (`handlers::meetings` join paths). Removal reuses the
//! v2 provider contract ([`RtcProvider::remove_participant`], already absent = success),
//! so the kicked person leaves the media room at once instead of staying connected until
//! their credential expires. A reconnect with a credential issued before the kick is
//! evicted again by the verified provider webhook ([`kicked_legacy_participant`]).

use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;

use crate::{
    error::ApiResult,
    services::rtc_provider::{RtcParticipantRef, RtcProvider},
};

/// Provider reference of one legacy participant in its legacy room.
pub fn participant_ref(
    meeting_id: Uuid,
    room_name: &str,
    participant_id: Uuid,
) -> RtcParticipantRef {
    RtcParticipantRef {
        conference_id: meeting_id,
        room_name: room_name.to_owned(),
        participant_session_id: participant_id,
        media_identity: participant_id.to_string(),
    }
}

/// Remove a legacy participant from its media room. `false` (logged) when the provider
/// could not be reached; the caller's database state stands either way.
pub async fn evict(
    rtc: &dyn RtcProvider,
    meeting_id: Uuid,
    room_name: &str,
    participant_id: Uuid,
    reason: &str,
) -> bool {
    match rtc
        .remove_participant(
            &participant_ref(meeting_id, room_name, participant_id),
            reason,
        )
        .await
    {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(
                meeting_id = %meeting_id,
                participant_id = %participant_id,
                reason,
                "legacy media eviction failed: {e}"
            );
            false
        }
    }
}

/// The legacy participant behind a provider identity in a legacy room, when it has been
/// removed (kicked or denied): `(meeting id, participant id)`.
pub async fn kicked_legacy_participant<C: ConnectionTrait>(
    db: &C,
    room_name: &str,
    media_identity: &str,
) -> ApiResult<Option<(Uuid, Uuid)>> {
    let Ok(participant_id) = Uuid::parse_str(media_identity) else {
        return Ok(None);
    };
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT m.id AS meeting_id FROM participants p JOIN meetings m ON m.id = p.meeting_id \
             WHERE p.id = $1 AND m.meeting_identifier = $2 AND p.status::text = 'kicked'",
            [participant_id.into(), room_name.into()],
        ))
        .await?;
    Ok(match row {
        Some(r) => Some((r.try_get::<Uuid>("", "meeting_id")?, participant_id)),
        None => None,
    })
}
