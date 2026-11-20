//! The eight HQ datasets (design §11), served by `bridge.datasets.read`.

use super::model::Owner;

pub struct DatasetSource {
    pub owner: Owner,
    /// `PULL` (the bridge reads counts from the owner) or an event type.
    pub via: &'static str,
}

pub struct Dataset {
    pub id: &'static str,
    pub description: &'static str,
    pub metrics: &'static [&'static str],
    pub sources: &'static [DatasetSource],
}

const fn pull(owner: Owner) -> DatasetSource {
    DatasetSource { owner, via: "PULL" }
}
const fn event(owner: Owner, via: &'static str) -> DatasetSource {
    DatasetSource { owner, via }
}

pub const COVERAGE_RULE: &str = "COMPLETE only when every listed source reported for every day of the window; PARTIAL when some did; NONE when none did. A metric without a report is null, never zero.";

pub const DATASETS: &[Dataset] = &[
    Dataset {
        id: "cfmeet.engagement_daily",
        description: "Daily and monthly active CF Meet users and sign-ups",
        metrics: &["dau", "mau", "signups"],
        sources: &[pull(Owner::MeetingService)],
    },
    Dataset {
        id: "cfmeet.meetings_daily",
        description: "Meetings held, participant minutes and peak concurrent live meetings",
        metrics: &["meetings", "participant_minutes", "peak_live_meetings"],
        sources: &[pull(Owner::MeetingService)],
    },
    Dataset {
        id: "cfmeet.messaging_daily",
        description: "Messages sent (count only; no content, no conversation ids)",
        metrics: &["messages_sent"],
        sources: &[pull(Owner::Messaging)],
    },
    Dataset {
        id: "cfmeet.partners_daily",
        description: "Partner workspaces by state, partners live in production, partner monthly active users",
        metrics: &["workspaces_active", "workspaces_restricted", "workspaces_suspended", "partners_live", "partner_mau"],
        sources: &[
            event(Owner::DeveloperPlatform, "partner.workspace.changed.v1"),
            pull(Owner::DeveloperPlatform),
        ],
    },
    Dataset {
        id: "cfmeet.apps_daily",
        description: "CF Meet Apps by review state, and installs",
        metrics: &["apps_draft", "apps_in_review", "apps_published", "apps_suspended", "installs"],
        sources: &[
            event(Owner::DeveloperPlatform, "app.review.changed.v1"),
            pull(Owner::DeveloperPlatform),
        ],
    },
    Dataset {
        id: "cfmeet.verification_queue",
        description: "Verification cases by state (partner go-live, app review)",
        metrics: &["pending", "in_review", "approved", "rejected"],
        sources: &[event(Owner::DeveloperPlatform, "verification.case.changed.v1")],
    },
    Dataset {
        id: "cfmeet.trust_safety_queue",
        description: "Open abuse reports by source service",
        metrics: &["open_messaging", "open_calling", "open_communications", "open_meetings"],
        sources: &[
            event(Owner::Messaging, "abuse.report.changed.v1"),
            event(Owner::Calling, "abuse.report.changed.v1"),
            event(Owner::Communications, "abuse.report.changed.v1"),
            event(Owner::MeetingService, "abuse.report.changed.v1"),
        ],
    },
    Dataset {
        id: "cfmeet.operations_health",
        description: "Failed messages and notification deliveries, open incidents, and the bridge's own projection gaps and quarantined events (owner -> bridge sync; not HQ's local sync health)",
        metrics: &["failed_messages", "failed_deliveries", "open_incidents", "projection_gaps", "quarantined_events"],
        sources: &[
            event(Owner::Messaging, "incident.changed.v1"),
            event(Owner::Calling, "incident.changed.v1"),
            event(Owner::Notifications, "incident.changed.v1"),
            event(Owner::MeetingService, "incident.changed.v1"),
            event(Owner::DeveloperPlatform, "incident.changed.v1"),
            pull(Owner::Messaging),
            pull(Owner::Notifications),
            pull(Owner::AdminBridge),
        ],
    },
];

pub fn dataset(id: &str) -> Option<&'static Dataset> {
    DATASETS.iter().find(|d| d.id == id)
}
