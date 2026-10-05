//! Model → `/api/v2` DTO projection. Secrets (passcode hash, token hashes, participant
//! credential hashes) and provider room identifiers are never projected.

use crate::{
    domain::{RecordOrigin, SecurityProfile},
    dto::v2::*,
    models::{
        conference_participants, conference_sessions, meeting_invitations, meeting_members,
        meeting_recurrence_rules,
    },
    services::{
        meeting_permissions::RolePolicy,
        meeting_space::{access_config, schedule_policy, SpaceView},
    },
};

pub fn space(view: &SpaceView) -> MeetingSpaceResponse {
    let s = &view.space;
    let hide_title = !view.full
        && matches!(
            s.security_profile,
            SecurityProfile::Healthcare | SecurityProfile::HighSecurity
        );
    let full = view.full;
    MeetingSpaceResponse {
        id: s.id,
        meeting_code: s.meeting_code.clone(),
        view: if full {
            ResourceView::Full
        } else {
            ResourceView::Summary
        },
        title: (!hide_title).then(|| s.title.clone()),
        meeting_type: s.meeting_type,
        status: s.status,
        access_policy: s.access_policy,
        requires_passcode: s.access_policy == crate::domain::AccessPolicy::PublicPasscode,
        is_legacy: s.origin == RecordOrigin::Legacy,
        viewer_role: view.viewer_role,
        description: full.then(|| s.description.clone()).flatten(),
        timezone: full.then(|| s.timezone.clone()),
        waiting_room_policy: full.then_some(s.waiting_room_policy),
        security_profile: full.then_some(s.security_profile),
        default_role: full.then_some(s.default_role),
        interactive_capacity: full.then_some(s.interactive_capacity),
        viewer_capacity: full.then_some(s.viewer_capacity),
        recording_policy: full.then_some(s.recording_policy),
        transcription_policy: full.then_some(s.transcription_policy),
        retention_policy_ref: full.then(|| s.retention_policy_ref.clone()).flatten(),
        organization_id: full.then_some(s.organization_id).flatten(),
        owner_user_id: full.then_some(s.owner_user_id),
        access_config: full.then(|| access_config(s)),
        role_policy: full.then(|| RolePolicy::from_json(&s.role_policy)),
        schedule_policy: full.then(|| schedule_policy(s)),
        collaboration_policy: full.then(|| {
            crate::services::collaboration::policy::CollaborationPolicyPatch::from_json(
                &s.collaboration_policy,
            )
        }),
        created_at: full.then_some(s.created_at),
        updated_at: full.then_some(s.updated_at),
        archived_at: full.then_some(s.archived_at).flatten(),
        version: full.then_some(s.version),
    }
}

pub fn conference(c: &conference_sessions::Model, full: bool) -> ConferenceResponse {
    ConferenceResponse {
        id: c.id,
        meeting_space_id: c.meeting_space_id,
        status: c.status,
        view: if full {
            ResourceView::Full
        } else {
            ResourceView::Summary
        },
        kind: full.then_some(c.kind),
        occurrence_key: full.then(|| c.occurrence_key.clone()),
        scheduled_start_at: full.then_some(c.scheduled_start_at).flatten(),
        scheduled_end_at: full.then_some(c.scheduled_end_at).flatten(),
        started_at: full.then_some(c.started_at).flatten(),
        ending_requested_at: full.then_some(c.ending_requested_at).flatten(),
        ended_at: full.then_some(c.ended_at).flatten(),
        termination_reason: full.then_some(c.termination_reason).flatten(),
        is_locked: full.then_some(c.is_locked),
        locked_at: full.then_some(c.locked_at).flatten(),
        is_exception: full.then_some(c.is_exception),
        original_start_at: full.then_some(c.original_start_at).flatten(),
        is_legacy: full.then_some(c.origin == RecordOrigin::Legacy),
        version: full.then_some(c.version),
        recording_state: full.then(|| c.recording_state.clone()),
        transcription_state: full.then(|| c.transcription_state.clone()),
    }
}

pub fn participant(p: &conference_participants::Model) -> ParticipantResponse {
    ParticipantResponse {
        id: p.id,
        conference_id: p.conference_id,
        identity_type: p.identity_type,
        display_name: p.display_name.clone(),
        role: p.role,
        admission_state: p.admission_state,
        requested_at: p.requested_at,
        admitted_at: p.admitted_at,
        denial_message: p.denial_message.clone(),
        denial_reason_code: p.denial_reason_code.clone(),
        version: p.version,
    }
}

pub fn member(m: &meeting_members::Model) -> MemberResponse {
    MemberResponse {
        id: m.id,
        meeting_space_id: m.meeting_space_id,
        principal_type: m.principal_type,
        user_id: m.user_id,
        email: (m.principal_type == crate::domain::PrincipalType::Email)
            .then(|| m.principal_ref.clone()),
        role: m.role,
        status: m.status,
        created_at: m.created_at,
        updated_at: m.updated_at,
        revoked_at: m.revoked_at,
        version: m.version,
    }
}

pub fn invitation(i: &meeting_invitations::Model) -> InvitationResponse {
    InvitationResponse {
        id: i.id,
        meeting_space_id: i.meeting_space_id,
        conference_id: i.conference_id,
        requested_role: i.requested_role,
        invitee_email: i.invitee_email.clone(),
        expires_at: i.expires_at,
        use_limit: i.use_limit,
        usage_count: i.usage_count,
        status: i.status,
        created_at: i.created_at,
    }
}

pub fn recurrence(r: &meeting_recurrence_rules::Model) -> RecurrenceResponse {
    RecurrenceResponse {
        id: r.id,
        meeting_space_id: r.meeting_space_id,
        rrule: r.rrule.clone(),
        timezone: r.timezone.clone(),
        start_local: r.dtstart_local,
        duration_minutes: r.duration_minutes,
        status: r.status,
        materialized_until: r.materialized_until,
        version: r.version,
    }
}

pub fn bulk(o: crate::services::admission::BulkOutcome) -> BulkAdmissionResponse {
    BulkAdmissionResponse {
        changed: o.changed,
        unchanged: o.unchanged,
        skipped: o
            .skipped
            .into_iter()
            .map(|(participant_id, admission_state)| SkippedParticipant {
                participant_id,
                admission_state,
            })
            .collect(),
    }
}
