//! Model → `/api/v2` DTO projection. Secrets (passcode hash, token hashes, participant
//! credential hashes) and provider room identifiers are never projected.

use crate::{
    domain::{RecordOrigin, SecurityProfile},
    dto::v2::{
        AgendaItem, AgendaSpace, BulkAdmissionResponse, ConferenceResponse, ConferenceSummary,
        InvitationResponse, MeetingSpaceResponse, MemberResponse, ParticipantResponse,
        RecurrenceResponse, ResourceView, SkippedParticipant,
    },
    models::{
        conference_participants, conference_sessions, meeting_invitations, meeting_members,
        meeting_recurrence_rules, meeting_spaces,
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
        owner_user_id: s.owner_user_id.filter(|_| full),
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
        latest_conference: view.latest_conference.as_ref().map(|c| {
            conference_summary(
                c,
                full || !crate::services::calendar::schedule_restricted(s),
            )
        }),
    }
}

/// Project a conference session to a minimal summary for space responses. MTG-011: the
/// timing is withheld from callers without the FULL view of a healthcare / high-security
/// space, exactly like the conference SUMMARY view.
fn conference_summary(c: &conference_sessions::Model, with_schedule: bool) -> ConferenceSummary {
    ConferenceSummary {
        id: c.id,
        status: c.status,
        scheduled_start_at: c.scheduled_start_at.filter(|_| with_schedule),
        scheduled_end_at: c.scheduled_end_at.filter(|_| with_schedule),
        started_at: c.started_at.filter(|_| with_schedule),
        ended_at: c.ended_at.filter(|_| with_schedule),
    }
}

/// A conference as seen by a caller of `space` (MTG-011): the SUMMARY view carries the
/// scheduled start/end (guest countdown) unless the space is healthcare / high-security.
pub fn conference_in(
    c: &conference_sessions::Model,
    space: &meeting_spaces::Model,
    full: bool,
) -> ConferenceResponse {
    let mut view = conference(c, full);
    if !full && !crate::services::calendar::schedule_restricted(space) {
        view.scheduled_start_at = c.scheduled_start_at;
        view.scheduled_end_at = c.scheduled_end_at;
    }
    view
}

/// One agenda entry: every caller with a relation sees the schedule (it is their own
/// calendar); management detail only in the FULL view; titles of healthcare /
/// high-security spaces only in the FULL view.
pub fn agenda_item(e: &crate::services::calendar::AgendaEntry) -> AgendaItem {
    let s = &e.space;
    let o = &e.occurrence;
    let full = e.full;
    AgendaItem {
        conference_id: o.conference_id,
        occurrence_key: o.occurrence_key.clone(),
        meeting_space_id: s.id,
        materialized: o.conference_id.is_some(),
        view: if full {
            ResourceView::Full
        } else {
            ResourceView::Summary
        },
        status: o.status,
        kind: o.kind,
        scheduled_start_at: o.start,
        scheduled_end_at: o.end,
        is_exception: o.is_exception,
        original_start_at: o.original_start_at.filter(|_| full),
        started_at: o.started_at.filter(|_| full),
        ended_at: o.ended_at.filter(|_| full),
        viewer_role: e.viewer_role,
        relations: e.relations.clone(),
        meeting_space: AgendaSpace {
            title: (full || !crate::services::calendar::title_restricted(s))
                .then(|| s.title.clone()),
            meeting_code: s.meeting_code.clone(),
            meeting_type: s.meeting_type,
            status: s.status,
            timezone: s.timezone.clone(),
            access_policy: s.access_policy,
            requires_passcode: s.access_policy == crate::domain::AccessPolicy::PublicPasscode,
            organization_id: s.organization_id.filter(|_| full),
        },
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
