//! Owner events the bridge accepts (design §11). One adapter per (source service,
//! export type): HQ rejects an event whose source does not match its adapter.
//! Field-level adapters are defined with the event pipeline (phase A7).

use serde_json::{json, Value};

use super::model::{Owner, Privacy};

pub const EXPORT_SCHEMA: &str = "cosmicforge.product-admin-event.v1";
pub const MAX_EVENT_BYTES: u32 = 16_384;

pub struct Adapter {
    pub source: Owner,
    pub export_type: &'static str,
    pub entity_type: &'static str,
    pub classification: Privacy,
}

const fn a(
    source: Owner,
    export_type: &'static str,
    entity_type: &'static str,
    classification: Privacy,
) -> Adapter {
    Adapter {
        source,
        export_type,
        entity_type,
        classification,
    }
}

pub const ADAPTERS: &[Adapter] = &[
    a(
        Owner::DeveloperPlatform,
        "verification.case.changed.v1",
        "verification_case",
        Privacy::OperationsMinimized,
    ),
    a(
        Owner::Messaging,
        "abuse.report.changed.v1",
        "abuse_report",
        Privacy::SafetyEvidence,
    ),
    a(
        Owner::Calling,
        "abuse.report.changed.v1",
        "abuse_report",
        Privacy::SafetyEvidence,
    ),
    a(
        Owner::Communications,
        "abuse.report.changed.v1",
        "abuse_report",
        Privacy::SafetyEvidence,
    ),
    a(
        Owner::MeetingService,
        "abuse.report.changed.v1",
        "abuse_report",
        Privacy::SafetyEvidence,
    ),
    a(
        Owner::DeveloperPlatform,
        "partner.workspace.changed.v1",
        "partner_workspace",
        Privacy::OperationsMinimized,
    ),
    a(
        Owner::DeveloperPlatform,
        "app.review.changed.v1",
        "app",
        Privacy::OperationsMinimized,
    ),
    a(
        Owner::Messaging,
        "incident.changed.v1",
        "incident",
        Privacy::OperationsMinimized,
    ),
    a(
        Owner::Calling,
        "incident.changed.v1",
        "incident",
        Privacy::OperationsMinimized,
    ),
    a(
        Owner::Notifications,
        "incident.changed.v1",
        "incident",
        Privacy::OperationsMinimized,
    ),
    a(
        Owner::MeetingService,
        "incident.changed.v1",
        "incident",
        Privacy::OperationsMinimized,
    ),
    a(
        Owner::DeveloperPlatform,
        "incident.changed.v1",
        "incident",
        Privacy::OperationsMinimized,
    ),
    a(
        Owner::DeveloperPlatform,
        "pricing.version.changed.v1",
        "price_version",
        Privacy::FinancialSensitive,
    ),
];

pub fn adapters_document() -> Value {
    json!({
        "contract": "cfmeet.admin-events.v1",
        "export_schema": EXPORT_SCHEMA,
        "max_event_bytes": MAX_EVENT_BYTES,
        "status": "PROVISIONAL: adapter fields are defined in phase A7; until then the bridge accepts no owner event",
        "exportable_classifications": ["INTERNAL", "OPERATIONS_MINIMIZED", "FINANCIAL_SENSITIVE", "SAFETY_EVIDENCE"],
        "adapters": ADAPTERS.iter().map(|a| json!({
            "source_service": a.source.as_str(),
            "export_type": a.export_type,
            "entity_type": a.entity_type,
            "classification": a.classification.as_str(),
            "fields": {}
        })).collect::<Vec<_>>()
    })
}
