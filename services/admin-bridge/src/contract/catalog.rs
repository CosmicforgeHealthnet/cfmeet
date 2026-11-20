//! The capability catalog: every route the bridge will ever expose (design §6), with its
//! risk, owner, roles, purposes and the phase that implements it. Single source of truth
//! for the runtime route table and for `docs/contract/`.
//!
//! Conventions (HQ interface v1):
//! * ids are `<module>.<resource>.<verb>`; reads use `GET`, commands use `POST` with an
//!   `Idempotency-Key`. The one read that must be `POST` is `pricing.charges.simulate`
//!   (its input is a usage vector, not a query).
//! * read permission = `<module>.<resource>.read`; command permission = the capability id.
//! * R3 / R4 commands are TWO_PERSON (derived in `model.rs`).

use super::model::*;
use Module as M;
use Owner as O;
use OwnerStatus as S;
use Phase as P;
use Privacy as V;
use Purpose as U;
use QueryShape as Q;
use Risk as R;
use Role as L;

struct Defaults {
    read_roles: &'static [Role],
    command_roles: &'static [Role],
    purposes: &'static [Purpose],
}

fn defaults(module: Module) -> Defaults {
    let d = |read_roles, command_roles, purposes| Defaults {
        read_roles,
        command_roles,
        purposes,
    };
    match module {
        M::Accounts => d(
            &[L::Support, L::TrustSafety, L::SecurityAdmin],
            &[L::Support, L::SecurityAdmin],
            &[U::SupportReview, U::AbuseReview, U::PrivacyRequest],
        ),
        M::Meetings => d(
            &[L::Support, L::TrustSafety, L::PlatformOps],
            &[L::TrustSafety, L::PlatformOps],
            &[
                U::SupportReview,
                U::AbuseReview,
                U::OpsIncident,
                U::PrivacyRequest,
            ],
        ),
        M::Messaging | M::Calling => d(
            &[L::TrustSafety, L::PlatformOps],
            &[L::TrustSafety, L::PlatformOps],
            &[U::AbuseReview, U::OpsIncident, U::PrivacyRequest],
        ),
        M::Communications => d(
            &[L::Support, L::TrustSafety],
            &[L::TrustSafety],
            &[U::SupportReview, U::AbuseReview, U::PrivacyRequest],
        ),
        M::Organizations => d(
            &[L::Support, L::PartnerOps],
            &[L::PartnerOps],
            &[U::SupportReview, U::VerificationReview, U::PrivacyRequest],
        ),
        M::Partners => d(
            &[L::PartnerOps, L::Support],
            &[L::PartnerOps],
            &[U::SupportReview, U::VerificationReview, U::OpsIncident],
        ),
        M::Identity => d(
            &[L::SecurityAdmin, L::PartnerOps],
            &[L::SecurityAdmin],
            &[U::SupportReview, U::VerificationReview, U::OpsIncident],
        ),
        M::Apps => d(
            &[L::PartnerOps, L::VerificationReviewer, L::TrustSafety],
            &[L::PartnerOps, L::TrustSafety],
            &[U::VerificationReview, U::AbuseReview],
        ),
        M::Verification => d(
            &[L::VerificationReviewer],
            &[L::VerificationReviewer],
            &[U::VerificationReview],
        ),
        M::Pricing => d(
            &[L::PricingEditor, L::PricingApprover, L::Finance],
            &[L::PricingEditor],
            &[U::PricingChange, U::BillingReview],
        ),
        M::Finance => d(&[L::Finance], &[], &[U::BillingReview]),
        M::Notifications => d(
            &[L::PlatformOps, L::Support],
            &[L::PlatformOps],
            &[U::OpsIncident, U::SupportReview],
        ),
        M::Privacy => d(
            &[L::SecurityAdmin],
            &[L::SecurityAdmin],
            &[U::PrivacyRequest],
        ),
        M::Operations => d(
            &[L::PlatformOps, L::ReleaseManager],
            &[L::PlatformOps],
            &[U::OpsIncident, U::ReleaseRequest],
        ),
        M::Audit => d(
            &[L::SecurityAdmin],
            &[],
            &[
                U::SupportReview,
                U::AbuseReview,
                U::OpsIncident,
                U::PrivacyRequest,
            ],
        ),
        M::Telecom => d(&[L::PlatformOps], &[], &[U::OpsIncident]),
        M::Bridge | M::Hq => d(&[], &[], &[]),
    }
}

/// Builder for one catalog entry.
struct B(Capability);

impl B {
    fn new(
        kind: Kind,
        method: Method,
        id: &'static str,
        path: &'static str,
        shape: QueryShape,
    ) -> B {
        let module = Module::parse(id.split('.').next().unwrap_or_default())
            .unwrap_or_else(|| panic!("{id}: first segment is not a module"));
        let d = defaults(module);
        let permission = match kind {
            Kind::Command => id.to_string(),
            _ => {
                let mut parts: Vec<&str> = id.split('.').collect();
                parts.pop();
                format!("{}.read", parts.join("."))
            }
        };
        let path_params = path
            .split('/')
            .filter_map(|seg| seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
            .map(|name| PathParam {
                name,
                format: ParamFormat::Opaque,
            })
            .collect();
        B(Capability {
            id,
            version: "1.0",
            kind,
            method,
            path,
            path_params,
            shape,
            owner: O::AdminBridge,
            owner_status: S::Local,
            source_endpoint: None,
            permission,
            risk: R::R1,
            privacy: V::Internal,
            roles: if kind == Kind::Command {
                d.command_roles
            } else {
                d.read_roles
            }
            .to_vec(),
            purposes: d.purposes.to_vec(),
            body: None,
            result: None,
            phase: P::A2,
            served: false,
            contact_search: false,
            tests: Vec::new(),
        })
    }
    fn at(mut self, owner: Owner, status: OwnerStatus, phase: Phase) -> B {
        self.0.owner = owner;
        self.0.owner_status = status;
        self.0.phase = phase;
        self
    }
    fn risk(mut self, risk: Risk, privacy: Privacy) -> B {
        self.0.risk = risk;
        self.0.privacy = privacy;
        self
    }
    fn roles(mut self, roles: &[Role]) -> B {
        self.0.roles = roles.to_vec();
        self
    }
    fn purposes(mut self, purposes: &[Purpose]) -> B {
        self.0.purposes = purposes.to_vec();
        self
    }
    fn perm(mut self, permission: &str) -> B {
        self.0.permission = permission.to_string();
        self
    }
    fn uuid(mut self, name: &str) -> B {
        for p in &mut self.0.path_params {
            if p.name == name {
                p.format = ParamFormat::Uuid;
            }
        }
        self
    }
    fn param(mut self, name: &str, format: ParamFormat) -> B {
        for p in &mut self.0.path_params {
            if p.name == name {
                p.format = format;
            }
        }
        self
    }
    fn body(mut self, schema: &'static str) -> B {
        self.0.body = Some(schema);
        self
    }
    fn result(mut self, schema: &'static str) -> B {
        self.0.result = Some(schema);
        self
    }
    fn source(mut self, endpoint: &'static str) -> B {
        self.0.source_endpoint = Some(endpoint);
        self
    }
    fn served(mut self, tests: &[&'static str]) -> B {
        self.0.served = true;
        self.0.tests = tests.to_vec();
        self
    }
}

fn get(id: &'static str, path: &'static str, shape: QueryShape) -> B {
    B::new(Kind::Read, Method::Get, id, path, shape)
}
fn cmd(id: &'static str, path: &'static str) -> B {
    B::new(Kind::Command, Method::Post, id, path, Q::Scoped)
}

const ALL_STAFF: &[Role] = &[
    L::Viewer,
    L::Support,
    L::TrustSafety,
    L::VerificationReviewer,
    L::PartnerOps,
    L::Finance,
    L::PricingEditor,
    L::PricingApprover,
    L::PlatformOps,
    L::SecurityAdmin,
    L::ReleaseManager,
];

/// Every capability, in contract order.
#[allow(clippy::vec_init_then_push)] // one push per catalog entry reads as a table
pub fn catalog() -> Vec<Capability> {
    use O::*;
    let mut c: Vec<B> = Vec::new();

    // ---- Platform (bridge-local) -------------------------------------------------
    c.push(
        get("bridge.capabilities.read", "/capabilities", Q::Scoped)
            .at(AdminBridge, S::Local, P::A1)
            .risk(R::R0, V::Internal)
            .perm("bridge.capabilities.read")
            .roles(ALL_STAFF)
            .purposes(&[U::AdminUiCapabilities])
            .result("Discovery")
            .served(&["discovery_reports_runtime_states_and_the_openapi_hash"]),
    );
    c.push(
        get("bridge.health.read", "/health/aggregate", Q::Scoped)
            .at(AdminBridge, S::Local, P::A1)
            .risk(R::R0, V::OperationsMinimized)
            .perm("bridge.health.read")
            .roles(&[
                L::Viewer,
                L::PlatformOps,
                L::ReleaseManager,
                L::SecurityAdmin,
            ])
            .purposes(&[U::MetricsRead, U::OpsIncident])
            .result("Health")
            .served(&["health_reports_unconfigured_owners_honestly"]),
    );
    c.push(
        get(
            "bridge.datasets.read",
            "/datasets/{dataset_id}",
            Q::ScopedWindow,
        )
        .at(AdminBridge, S::Local, P::A1)
        .risk(R::R0, V::Internal)
        .perm("bridge.datasets.read")
        .param("dataset_id", ParamFormat::DatasetId)
        .roles(&[ALL_STAFF, &[L::SourceSync]].concat())
        .purposes(&[U::MetricsRead])
        .result("Dataset")
        .served(&[
            "datasets_are_null_not_zero_until_sources_report",
            "worker_identity_reads_datasets_with_the_workload_acr",
        ]),
    );
    // HQ's generic settlement of a command (roles and purposes: every command's, below).
    c.push(
        get(
            "bridge.operations.read",
            "/hq/operations/{idempotency_key}",
            Q::Scoped,
        )
        .at(AdminBridge, S::Local, P::A3)
        .risk(R::R1, V::Internal)
        .perm("bridge.operations.read")
        .uuid("idempotency_key")
        .result("OperationReadback")
        .served(&[
            "operations_readback_settles_from_the_owner_record_and_never_resends",
            "operations_readback_is_private_to_the_staff_member_purpose_and_scope",
        ]),
    );
    c.push(
        get("hq.events.feed", "/hq/events", Q::ScopedWindow)
            .at(AdminBridge, S::Local, P::A7)
            .risk(R::R0, V::OperationsMinimized)
            .perm("hq.events.read")
            .roles(&[L::SourceSync])
            .purposes(&[U::EventSync]),
    );
    c.push(
        B::new(
            Kind::Sync,
            Method::Post,
            "hq.events.ack",
            "/hq/event-acknowledgements",
            Q::Scoped,
        )
        .at(AdminBridge, S::Local, P::A7)
        .risk(R::R0, V::OperationsMinimized)
        .perm("hq.events.ack")
        .roles(&[L::SourceSync])
        .purposes(&[U::EventSync]),
    );
    c.push(
        get("hq.projections.status", "/hq/projections/status", Q::Scoped)
            .at(AdminBridge, S::Local, P::A1)
            .risk(R::R0, V::OperationsMinimized)
            .perm("hq.projections.read")
            .roles(&[L::SourceSync, L::PlatformOps])
            .purposes(&[U::EventSync, U::OpsIncident])
            .result("ProjectionStatus")
            .served(&["projection_status_is_unavailable_until_owner_events"]),
    );

    // ---- 6.1 Accounts (Meeting Service) ------------------------------------------
    let ms = |b: B, st, ph| b.at(MeetingService, st, ph);
    c.push(
        ms(
            get("accounts.users.search", "/accounts/users", Q::ScopedSearch),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            get(
                "accounts.users.read",
                "/accounts/users/{user_id}",
                Q::Scoped,
            ),
            S::New,
            P::A2,
        )
        .uuid("user_id")
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            cmd(
                "accounts.user.status.change",
                "/accounts/users/{user_id}/status",
            ),
            S::Exists,
            P::A4,
        )
        .uuid("user_id")
        .risk(R::R3, V::Personal)
        .roles(&[L::Support, L::TrustSafety, L::SecurityAdmin]),
    );
    c.push(
        ms(
            cmd(
                "accounts.user.role.change",
                "/accounts/users/{user_id}/role",
            ),
            S::Exists,
            P::A4,
        )
        .uuid("user_id")
        .risk(R::R3, V::Personal)
        .roles(&[L::SecurityAdmin]),
    );
    c.push(
        ms(
            cmd(
                "accounts.user.email.change",
                "/accounts/users/{user_id}/email",
            ),
            S::Exists,
            P::A4,
        )
        .uuid("user_id")
        .risk(R::R3, V::Personal)
        .roles(&[L::Support]),
    );
    c.push(
        ms(
            get(
                "accounts.sessions.list",
                "/accounts/users/{user_id}/sessions",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .uuid("user_id")
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            cmd(
                "accounts.sessions.revoke-all",
                "/accounts/users/{user_id}/sessions/revoke-all",
            ),
            S::Exists,
            P::A4,
        )
        .uuid("user_id")
        .risk(R::R3, V::Personal)
        .roles(&[L::Support, L::TrustSafety, L::SecurityAdmin]),
    );
    c.push(
        ms(
            get(
                "accounts.devices.list",
                "/accounts/users/{user_id}/devices",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .uuid("user_id")
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            cmd(
                "accounts.device.revoke",
                "/accounts/users/{user_id}/devices/{device_id}/revoke",
            ),
            S::New,
            P::A4,
        )
        .uuid("user_id")
        .risk(R::R3, V::Personal)
        .roles(&[L::SecurityAdmin, L::TrustSafety]),
    );
    c.push(
        ms(
            get(
                "accounts.mfa.read",
                "/accounts/users/{user_id}/mfa",
                Q::Scoped,
            ),
            S::New,
            P::A2,
        )
        .uuid("user_id")
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            cmd("accounts.mfa.reset", "/accounts/users/{user_id}/mfa/reset"),
            S::New,
            P::A4,
        )
        .uuid("user_id")
        .risk(R::R3, V::Personal)
        .roles(&[L::SecurityAdmin]),
    );
    c.push(
        ms(
            get(
                "accounts.security-history.list",
                "/accounts/users/{user_id}/security-history",
                Q::ScopedWindow,
            ),
            S::Exists,
            P::A2,
        )
        .uuid("user_id")
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            get(
                "accounts.duplicates.list",
                "/accounts/duplicates",
                Q::ScopedPage,
            ),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            cmd(
                "accounts.duplicates.enforce",
                "/accounts/duplicates/{group_id}/enforce",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R3, V::Personal)
        .roles(&[L::SecurityAdmin, L::TrustSafety]),
    );
    c.push(
        ms(
            cmd("accounts.user.export", "/accounts/users/{user_id}/export"),
            S::New,
            P::A7,
        )
        .uuid("user_id")
        .risk(R::R4, V::Personal)
        .roles(&[L::SecurityAdmin])
        .purposes(&[U::PrivacyRequest]),
    );
    c.push(
        ms(
            cmd("accounts.user.delete", "/accounts/users/{user_id}/delete"),
            S::New,
            P::A7,
        )
        .uuid("user_id")
        .risk(R::R4, V::Personal)
        .roles(&[L::SecurityAdmin])
        .purposes(&[U::PrivacyRequest]),
    );
    c.push(
        ms(
            get(
                "accounts.test-accounts.list",
                "/accounts/test-accounts",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );

    // ---- 6.2 Meetings (Meeting Service) ------------------------------------------
    c.push(
        ms(
            get(
                "meetings.spaces.search",
                "/meetings/spaces",
                Q::ScopedSearch,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            get(
                "meetings.meetings.search",
                "/meetings/meetings",
                Q::ScopedSearch,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            get(
                "meetings.meetings.read",
                "/meetings/meetings/{meeting_id}",
                Q::Scoped,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            get("meetings.live.list", "/meetings/live", Q::ScopedPage),
            S::Exists,
            P::A2,
        )
        .risk(R::R0, V::OperationsMinimized),
    );
    c.push(
        ms(
            get(
                "meetings.live-health.read",
                "/meetings/live/{meeting_id}/health",
                Q::Scoped,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        ms(
            get(
                "meetings.participants.list",
                "/meetings/meetings/{meeting_id}/participants",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            cmd(
                "meetings.meeting.end",
                "/meetings/meetings/{meeting_id}/end",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::OperationsMinimized),
    );
    c.push(
        ms(
            cmd(
                "meetings.space.archive",
                "/meetings/spaces/{space_id}/archive",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R3, V::OperationsMinimized),
    );
    c.push(
        ms(
            cmd(
                "meetings.space.transfer",
                "/meetings/spaces/{space_id}/transfer",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R3, V::Personal),
    );
    c.push(
        ms(
            get(
                "meetings.recordings.list",
                "/meetings/recordings",
                Q::ScopedWindow,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            cmd(
                "meetings.recording.delete",
                "/meetings/recordings/{recording_id}/delete",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R4, V::Personal),
    );
    c.push(
        ms(
            get(
                "meetings.transcripts.list",
                "/meetings/transcripts",
                Q::ScopedWindow,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            cmd(
                "meetings.transcript.delete",
                "/meetings/transcripts/{transcript_id}/delete",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R4, V::Personal),
    );
    c.push(
        ms(
            get(
                "meetings.webinars.list",
                "/meetings/webinars",
                Q::ScopedWindow,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            get(
                "meetings.webinar-registrations.list",
                "/meetings/webinars/{webinar_id}/registrations",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        ms(
            get("meetings.audit.list", "/meetings/audit", Q::ScopedWindow),
            S::DbToApi,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        ms(
            get("meetings.reports.list", "/meetings/reports", Q::ScopedPage),
            S::New,
            P::A4,
        )
        .risk(R::R1, V::SafetyEvidence),
    );
    c.push(
        ms(
            cmd(
                "meetings.report.review",
                "/meetings/reports/{report_id}/review",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::SafetyEvidence)
        .roles(&[L::TrustSafety]),
    );
    c.push(
        ms(
            get(
                "meetings.chat-migration.read",
                "/meetings/chat-migration",
                Q::Scoped,
            ),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        ms(
            cmd(
                "meetings.chat-migration.change",
                "/meetings/chat-migration/stage",
            ),
            S::Exists,
            P::A5,
        )
        .risk(R::R3, V::OperationsMinimized)
        .roles(&[L::PlatformOps]),
    );
    c.push(
        ms(
            get(
                "meetings.email-delivery.read",
                "/meetings/email-delivery",
                Q::ScopedWindow,
            ),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        ms(
            get(
                "meetings.critical-lane.read",
                "/meetings/critical-lane",
                Q::Scoped,
            ),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        ms(
            cmd(
                "meetings.critical-lane.change",
                "/meetings/critical-lane/route",
            ),
            S::Exists,
            P::A5,
        )
        .risk(R::R3, V::OperationsMinimized)
        .roles(&[L::PlatformOps]),
    );

    // ---- 6.3 Chat (Messaging) ----------------------------------------------------
    let mg = |b: B, st, ph| b.at(Messaging, st, ph);
    c.push(
        mg(
            get(
                "messaging.reports.list",
                "/messaging/reports",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::SafetyEvidence),
    );
    c.push(
        mg(
            get(
                "messaging.reports.read",
                "/messaging/reports/{report_id}",
                Q::Scoped,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::SafetyEvidence),
    );
    c.push(
        mg(
            cmd(
                "messaging.message.remove",
                "/messaging/reports/{report_id}/remove-message",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::SafetyEvidence)
        .roles(&[L::TrustSafety]),
    );
    c.push(
        mg(
            cmd(
                "messaging.member.mute",
                "/messaging/conversations/{conversation_id}/members/{member_id}/mute",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::SafetyEvidence)
        .roles(&[L::TrustSafety]),
    );
    c.push(
        mg(
            cmd(
                "messaging.member.remove",
                "/messaging/conversations/{conversation_id}/members/{member_id}/remove",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::SafetyEvidence)
        .roles(&[L::TrustSafety]),
    );
    c.push(
        mg(
            cmd(
                "messaging.conversation.restrict",
                "/messaging/conversations/{conversation_id}/restrict",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::SafetyEvidence)
        .roles(&[L::TrustSafety]),
    );
    c.push(
        mg(
            cmd(
                "messaging.report.dismiss",
                "/messaging/reports/{report_id}/dismiss",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::SafetyEvidence)
        .roles(&[L::TrustSafety]),
    );
    c.push(
        mg(
            get("messaging.policy.read", "/messaging/policy", Q::Scoped),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        mg(
            cmd("messaging.policy.change", "/messaging/policy"),
            S::New,
            P::A5,
        )
        .risk(R::R3, V::Internal)
        .roles(&[L::PlatformOps]),
    );
    c.push(
        mg(
            get("messaging.holds.list", "/messaging/holds", Q::ScopedPage),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        mg(
            cmd("messaging.hold.place", "/messaging/holds"),
            S::New,
            P::A5,
        )
        .risk(R::R3, V::OperationsMinimized)
        .roles(&[L::TrustSafety]),
    );
    c.push(
        mg(
            cmd(
                "messaging.hold.release",
                "/messaging/holds/{hold_id}/release",
            ),
            S::New,
            P::A5,
        )
        .risk(R::R3, V::OperationsMinimized)
        .roles(&[L::TrustSafety]),
    );
    c.push(
        mg(
            get(
                "messaging.exports.list",
                "/messaging/exports",
                Q::ScopedPage,
            ),
            S::New,
            P::A7,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        mg(
            cmd("messaging.export.request", "/messaging/exports"),
            S::New,
            P::A7,
        )
        .risk(R::R4, V::Personal)
        .roles(&[L::TrustSafety, L::SecurityAdmin])
        .purposes(&[U::AbuseReview, U::PrivacyRequest]),
    );
    c.push(
        mg(
            get(
                "messaging.deletion-requests.list",
                "/messaging/deletion-requests",
                Q::ScopedPage,
            ),
            S::New,
            P::A7,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        mg(
            cmd(
                "messaging.deletion-request.create",
                "/messaging/deletion-requests",
            ),
            S::New,
            P::A7,
        )
        .risk(R::R4, V::Personal)
        .roles(&[L::SecurityAdmin])
        .purposes(&[U::PrivacyRequest]),
    );
    c.push(
        mg(
            get("messaging.audit.list", "/messaging/audit", Q::ScopedWindow),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        mg(
            get(
                "messaging.switches.list",
                "/messaging/switches",
                Q::ScopedPage,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R0, V::Internal),
    );
    c.push(
        mg(
            cmd("messaging.switch.change", "/messaging/switches/{switch_id}"),
            S::CliToApi,
            P::A5,
        )
        .risk(R::R3, V::Internal)
        .roles(&[L::PlatformOps])
        .purposes(&[U::OpsIncident]),
    );
    c.push(
        mg(
            get(
                "messaging.incidents.list",
                "/messaging/incidents",
                Q::ScopedWindow,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        mg(
            cmd("messaging.incident.record", "/messaging/incidents"),
            S::CliToApi,
            P::A5,
        )
        .risk(R::R2, V::OperationsMinimized)
        .roles(&[L::PlatformOps])
        .purposes(&[U::OpsIncident]),
    );
    c.push(
        mg(
            get(
                "messaging.failed-messages.list",
                "/messaging/failed-messages",
                Q::ScopedWindow,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        mg(
            cmd(
                "messaging.failed-message.retry",
                "/messaging/failed-messages/{message_id}/retry",
            ),
            S::CliToApi,
            P::A5,
        )
        .risk(R::R2, V::OperationsMinimized)
        .roles(&[L::PlatformOps])
        .purposes(&[U::OpsIncident]),
    );
    c.push(
        mg(
            get(
                "messaging.key-transparency.read",
                "/messaging/key-transparency",
                Q::Scoped,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R0, V::Internal),
    );
    c.push(
        mg(
            cmd(
                "messaging.key-transparency.checkpoint",
                "/messaging/key-transparency/checkpoint",
            ),
            S::CliToApi,
            P::A5,
        )
        .risk(R::R3, V::Internal)
        .roles(&[L::PlatformOps])
        .purposes(&[U::OpsIncident]),
    );

    // ---- 6.4 Calls (Calling) -----------------------------------------------------
    let cl = |b: B, st, ph| b.at(Calling, st, ph);
    c.push(
        cl(
            get("calling.calls.list", "/calling/calls", Q::ScopedWindow),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        cl(
            get("calling.calls.read", "/calling/calls/{call_id}", Q::Scoped),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        cl(
            get("calling.live.list", "/calling/live", Q::ScopedPage),
            S::DbToApi,
            P::A2,
        )
        .risk(R::R0, V::OperationsMinimized),
    );
    c.push(
        cl(
            get(
                "calling.abuse-reports.list",
                "/calling/abuse-reports",
                Q::ScopedPage,
            ),
            S::ExistsNotGranted,
            P::A2,
        )
        .risk(R::R1, V::SafetyEvidence),
    );
    c.push(
        cl(
            cmd(
                "calling.abuse-report.resolve",
                "/calling/abuse-reports/{report_id}/resolve",
            ),
            S::ExistsNotGranted,
            P::A4,
        )
        .risk(R::R2, V::SafetyEvidence)
        .roles(&[L::TrustSafety]),
    );
    c.push(
        cl(
            get("calling.blocks.list", "/calling/blocks", Q::ScopedPage),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::SafetyEvidence),
    );
    c.push(
        cl(
            cmd(
                "calling.block.release",
                "/calling/blocks/{block_id}/release",
            ),
            S::CliToApi,
            P::A5,
        )
        .risk(R::R3, V::SafetyEvidence)
        .roles(&[L::TrustSafety]),
    );
    c.push(
        cl(
            get("calling.limits.read", "/calling/limits", Q::Scoped),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        cl(
            cmd("calling.limits.change", "/calling/limits"),
            S::CliToApi,
            P::A5,
        )
        .risk(R::R3, V::Internal)
        .roles(&[L::PlatformOps]),
    );
    c.push(
        cl(
            get("calling.routing.read", "/calling/routing", Q::Scoped),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        cl(
            cmd("calling.routing.change", "/calling/routing"),
            S::CliToApi,
            P::A5,
        )
        .risk(R::R3, V::Internal)
        .roles(&[L::PlatformOps]),
    );
    c.push(
        cl(
            get(
                "calling.enterprise-features.read",
                "/calling/enterprise-features",
                Q::Scoped,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        cl(
            cmd(
                "calling.enterprise-features.change",
                "/calling/enterprise-features",
            ),
            S::CliToApi,
            P::A5,
        )
        .risk(R::R3, V::Internal)
        .roles(&[L::PlatformOps]),
    );
    c.push(
        cl(
            get(
                "calling.spend-limits.read",
                "/calling/spend-limits",
                Q::Scoped,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::FinancialSensitive),
    );
    c.push(
        cl(
            cmd("calling.spend-limits.change", "/calling/spend-limits"),
            S::CliToApi,
            P::A5,
        )
        .risk(R::R4, V::FinancialSensitive)
        .roles(&[L::PlatformOps])
        .purposes(&[U::OpsIncident, U::BillingReview]),
    );
    c.push(
        cl(
            get(
                "calling.recordings.list",
                "/calling/recordings",
                Q::ScopedWindow,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        cl(
            get(
                "calling.voicemail.list",
                "/calling/voicemail",
                Q::ScopedWindow,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        cl(
            cmd(
                "calling.recording.delete",
                "/calling/recordings/{recording_id}/delete",
            ),
            S::New,
            P::A5,
        )
        .risk(R::R4, V::Personal)
        .roles(&[L::TrustSafety]),
    );
    c.push(
        cl(
            get(
                "calling.retention-holds.list",
                "/calling/retention-holds",
                Q::ScopedPage,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        cl(
            cmd("calling.retention-hold.place", "/calling/retention-holds"),
            S::CliToApi,
            P::A5,
        )
        .risk(R::R4, V::OperationsMinimized)
        .roles(&[L::TrustSafety]),
    );
    c.push(
        cl(
            cmd(
                "calling.retention-hold.release",
                "/calling/retention-holds/{hold_id}/release",
            ),
            S::CliToApi,
            P::A5,
        )
        .risk(R::R4, V::OperationsMinimized)
        .roles(&[L::TrustSafety]),
    );
    c.push(
        cl(
            get(
                "calling.incidents.list",
                "/calling/incidents",
                Q::ScopedWindow,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        cl(
            cmd("calling.incident.record", "/calling/incidents"),
            S::CliToApi,
            P::A5,
        )
        .risk(R::R2, V::OperationsMinimized)
        .roles(&[L::PlatformOps])
        .purposes(&[U::OpsIncident]),
    );
    c.push(
        cl(
            get(
                "calling.webhook-keys.list",
                "/calling/webhook-keys",
                Q::ScopedPage,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        cl(
            cmd(
                "calling.webhook-key.reseal",
                "/calling/webhook-keys/{key_id}/reseal",
            ),
            S::CliToApi,
            P::A5,
        )
        .risk(R::R3, V::Internal)
        .roles(&[L::PlatformOps]),
    );
    c.push(
        cl(
            get("calling.audit.list", "/calling/audit", Q::ScopedWindow),
            S::DbToApi,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );

    // ---- 6.5 Profiles (Communications) -------------------------------------------
    let cm = |b: B, st, ph| b.at(Communications, st, ph);
    c.push(
        cm(
            get(
                "communications.profiles.read",
                "/communications/profiles/{cfmeet_id}",
                Q::Scoped,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        cm(
            cmd(
                "communications.profile.suspend",
                "/communications/profiles/{cfmeet_id}/suspend",
            ),
            S::ExistsNotGranted,
            P::A4,
        )
        .risk(R::R3, V::Personal),
    );
    c.push(
        cm(
            cmd(
                "communications.profile.restore",
                "/communications/profiles/{cfmeet_id}/restore",
            ),
            S::ExistsNotGranted,
            P::A4,
        )
        .risk(R::R3, V::Personal),
    );
    c.push(
        cm(
            get(
                "communications.reports.list",
                "/communications/reports",
                Q::ScopedPage,
            ),
            S::ExistsNotGranted,
            P::A2,
        )
        .risk(R::R1, V::SafetyEvidence),
    );
    c.push(
        cm(
            cmd(
                "communications.report.review",
                "/communications/reports/{report_id}/review",
            ),
            S::ExistsNotGranted,
            P::A4,
        )
        .risk(R::R3, V::SafetyEvidence),
    );
    c.push(
        cm(
            get(
                "communications.holds.list",
                "/communications/holds",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        cm(
            get(
                "communications.audit.list",
                "/communications/audit",
                Q::ScopedWindow,
            ),
            S::DbToApi,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        cm(
            get(
                "communications.metrics.read",
                "/communications/metrics",
                Q::Scoped,
            ),
            S::Exists,
            P::A2,
        )
        .risk(R::R0, V::Internal),
    );
    c.push(
        cm(
            cmd("communications.backfill.run", "/communications/backfill"),
            S::Exists,
            P::A5,
        )
        .risk(R::R2, V::OperationsMinimized)
        .roles(&[L::PlatformOps])
        .purposes(&[U::OpsIncident]),
    );
    c.push(
        cm(
            cmd("communications.events.replay", "/communications/replay"),
            S::Exists,
            P::A5,
        )
        .risk(R::R2, V::OperationsMinimized)
        .roles(&[L::PlatformOps])
        .purposes(&[U::OpsIncident]),
    );

    // ---- 6.6 Business organizations (Organizations) ------------------------------
    let og = |b: B, st, ph| b.at(Organizations, st, ph);
    c.push(
        og(
            get(
                "organizations.organizations.search",
                "/organizations/organizations",
                Q::ScopedSearch,
            ),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        og(
            get(
                "organizations.organizations.read",
                "/organizations/organizations/{organization_id}",
                Q::Scoped,
            ),
            S::Exists,
            P::A2,
        )
        .uuid("organization_id")
        .risk(R::R1, V::Internal),
    );
    c.push(
        og(
            cmd(
                "organizations.organization.create",
                "/organizations/organizations",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R2, V::Internal),
    );
    c.push(
        og(
            cmd(
                "organizations.organization.suspend",
                "/organizations/organizations/{organization_id}/suspend",
            ),
            S::Exists,
            P::A4,
        )
        .uuid("organization_id")
        .risk(R::R3, V::Internal),
    );
    c.push(
        og(
            cmd(
                "organizations.organization.archive",
                "/organizations/organizations/{organization_id}/archive",
            ),
            S::Exists,
            P::A4,
        )
        .uuid("organization_id")
        .risk(R::R3, V::Internal),
    );
    c.push(
        og(
            cmd(
                "organizations.organization.reactivate",
                "/organizations/organizations/{organization_id}/reactivate",
            ),
            S::Exists,
            P::A4,
        )
        .uuid("organization_id")
        .risk(R::R3, V::Internal),
    );
    c.push(
        og(
            cmd(
                "organizations.ownership.recover",
                "/organizations/organizations/{organization_id}/ownership-recovery",
            ),
            S::Exists,
            P::A4,
        )
        .uuid("organization_id")
        .risk(R::R3, V::Personal),
    );
    c.push(
        og(
            get(
                "organizations.bindings.list",
                "/organizations/organizations/{organization_id}/bindings",
                Q::ScopedPage,
            ),
            S::Exists,
            P::A2,
        )
        .uuid("organization_id")
        .risk(R::R1, V::Internal),
    );
    c.push(
        og(
            cmd(
                "organizations.binding.change",
                "/organizations/organizations/{organization_id}/bindings/{service}",
            ),
            S::Exists,
            P::A4,
        )
        .uuid("organization_id")
        .risk(R::R3, V::Internal),
    );
    c.push(
        og(
            cmd(
                "organizations.organization.export",
                "/organizations/organizations/{organization_id}/export",
            ),
            S::Exists,
            P::A7,
        )
        .uuid("organization_id")
        .risk(R::R4, V::Personal)
        .purposes(&[U::PrivacyRequest, U::SupportReview]),
    );
    c.push(
        og(
            get(
                "organizations.audit.list",
                "/organizations/organizations/{organization_id}/audit",
                Q::ScopedWindow,
            ),
            S::Exists,
            P::A2,
        )
        .uuid("organization_id")
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        og(
            get(
                "organizations.global-audit.list",
                "/organizations/audit",
                Q::ScopedWindow,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        og(
            cmd(
                "organizations.provisioning.retry",
                "/organizations/organizations/{organization_id}/provisioning/retry",
            ),
            S::Exists,
            P::A4,
        )
        .uuid("organization_id")
        .risk(R::R2, V::OperationsMinimized),
    );
    c.push(
        og(
            cmd(
                "organizations.events.replay",
                "/organizations/events/replay",
            ),
            S::Exists,
            P::A5,
        )
        .risk(R::R2, V::OperationsMinimized)
        .roles(&[L::PlatformOps, L::PartnerOps])
        .purposes(&[U::OpsIncident]),
    );
    c.push(
        og(
            get(
                "organizations.members.list",
                "/organizations/organizations/{organization_id}/members",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .uuid("organization_id")
        .risk(R::R1, V::Personal),
    );
    c.push(
        og(
            get(
                "organizations.teams.list",
                "/organizations/organizations/{organization_id}/teams",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .uuid("organization_id")
        .risk(R::R1, V::Internal),
    );
    c.push(
        og(
            get(
                "organizations.policies.read",
                "/organizations/organizations/{organization_id}/policies",
                Q::Scoped,
            ),
            S::New,
            P::A2,
        )
        .uuid("organization_id")
        .risk(R::R1, V::Internal),
    );
    c.push(
        og(
            cmd(
                "organizations.member.change",
                "/organizations/organizations/{organization_id}/members/{member_id}",
            ),
            S::New,
            P::A4,
        )
        .uuid("organization_id")
        .risk(R::R3, V::Personal),
    );
    c.push(
        og(
            cmd(
                "organizations.team.change",
                "/organizations/organizations/{organization_id}/teams/{team_id}",
            ),
            S::New,
            P::A4,
        )
        .uuid("organization_id")
        .risk(R::R3, V::Internal),
    );
    c.push(
        og(
            cmd(
                "organizations.policies.change",
                "/organizations/organizations/{organization_id}/policies",
            ),
            S::New,
            P::A4,
        )
        .uuid("organization_id")
        .risk(R::R3, V::Internal),
    );

    // ---- 6.7 Partners (Developer Platform) ---------------------------------------
    let dp = |b: B, st, ph| b.at(DeveloperPlatform, st, ph);
    c.push(
        dp(
            get(
                "partners.workspaces.search",
                "/partners/workspaces",
                Q::ScopedSearch,
            ),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        dp(
            get(
                "partners.workspaces.read",
                "/partners/workspaces/{workspace_id}",
                Q::Scoped,
            ),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        dp(
            get(
                "partners.workspace-activity.list",
                "/partners/workspaces/{workspace_id}/staff-activity",
                Q::ScopedWindow,
            ),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        dp(
            cmd(
                "partners.workspace.state.change",
                "/partners/workspaces/{workspace_id}/state",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R3, V::Internal),
    );
    c.push(
        dp(
            cmd(
                "partners.workspace.register-first-party",
                "/partners/workspaces/first-party",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R4, V::Internal),
    );
    c.push(
        dp(
            get(
                "partners.projects.list",
                "/partners/workspaces/{workspace_id}/projects",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        dp(
            get(
                "partners.environments.list",
                "/partners/workspaces/{workspace_id}/environments",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        dp(
            get(
                "partners.members.list",
                "/partners/workspaces/{workspace_id}/members",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        dp(
            get(
                "partners.invitations.list",
                "/partners/workspaces/{workspace_id}/invitations",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        get(
            "partners.keys.list",
            "/partners/workspaces/{workspace_id}/keys",
            Q::ScopedPage,
        )
        .at(PlatformAccess, S::New, P::A2)
        .risk(R::R1, V::Internal),
    );
    c.push(
        dp(
            cmd(
                "partners.key.revoke",
                "/partners/workspaces/{workspace_id}/keys/{key_id}/revoke",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::Internal),
    );
    c.push(
        get(
            "partners.service-accounts.list",
            "/partners/workspaces/{workspace_id}/service-accounts",
            Q::ScopedPage,
        )
        .at(PlatformAccess, S::New, P::A2)
        .risk(R::R1, V::Internal),
    );
    c.push(
        dp(
            cmd(
                "partners.service-account.suspend",
                "/partners/workspaces/{workspace_id}/service-accounts/{account_id}/suspend",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::Internal),
    );
    c.push(
        get(
            "partners.websites.list",
            "/partners/workspaces/{workspace_id}/websites",
            Q::ScopedPage,
        )
        .at(PlatformAccess, S::New, P::A2)
        .risk(R::R1, V::Internal),
    );
    c.push(
        dp(
            cmd(
                "partners.website.remove",
                "/partners/workspaces/{workspace_id}/websites/{website_id}/remove",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::Internal),
    );
    c.push(
        dp(
            get(
                "partners.push-keys.list",
                "/partners/workspaces/{workspace_id}/push-keys",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        dp(
            cmd(
                "partners.push-key.disable",
                "/partners/workspaces/{workspace_id}/push-keys/{push_key_id}/disable",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::Internal),
    );
    c.push(
        get(
            "partners.users.list",
            "/partners/workspaces/{workspace_id}/users",
            Q::ScopedSearch,
        )
        .at(PlatformAccess, S::New, P::A2)
        .risk(R::R1, V::Personal),
    );
    c.push(
        dp(
            cmd(
                "partners.user.sign-out",
                "/partners/workspaces/{workspace_id}/users/{user_id}/sign-out",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::Personal),
    );
    c.push(
        dp(
            get(
                "partners.webhooks.list",
                "/partners/workspaces/{workspace_id}/webhooks",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        dp(
            get(
                "partners.webhook-deliveries.list",
                "/partners/workspaces/{workspace_id}/webhook-deliveries",
                Q::ScopedWindow,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        dp(
            cmd(
                "partners.webhook-delivery.retry",
                "/partners/workspaces/{workspace_id}/webhook-deliveries/{delivery_id}/retry",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R2, V::OperationsMinimized),
    );
    c.push(
        dp(
            get(
                "partners.usage.read",
                "/partners/workspaces/{workspace_id}/usage",
                Q::ScopedWindow,
            ),
            S::Exists,
            P::A2,
        )
        .risk(R::R0, V::Internal),
    );
    c.push(
        dp(
            cmd("partners.usage.reprocess", "/partners/usage/reprocess"),
            S::Exists,
            P::A4,
        )
        .risk(R::R2, V::Internal)
        .purposes(&[U::OpsIncident, U::BillingReview]),
    );
    c.push(
        dp(
            cmd(
                "partners.support-tier.change",
                "/partners/workspaces/{workspace_id}/support-tier",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R2, V::Internal),
    );
    c.push(
        dp(
            get(
                "partners.incidents.list",
                "/partners/incidents",
                Q::ScopedWindow,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        dp(
            cmd("partners.incident.record", "/partners/incidents"),
            S::Exists,
            P::A4,
        )
        .risk(R::R2, V::OperationsMinimized)
        .purposes(&[U::OpsIncident]),
    );

    // ---- 6.8 Identity (Platform Access) ------------------------------------------
    let pa = |b: B, st, ph| b.at(PlatformAccess, st, ph);
    c.push(
        pa(
            get(
                "identity.tenants.list",
                "/identity/tenants",
                Q::ScopedSearch,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        pa(
            get(
                "identity.applications.list",
                "/identity/tenants/{tenant_id}/applications",
                Q::ScopedPage,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        pa(
            get(
                "identity.environments.list",
                "/identity/tenants/{tenant_id}/environments",
                Q::ScopedPage,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        pa(
            get(
                "identity.service-accounts.list",
                "/identity/tenants/{tenant_id}/service-accounts",
                Q::ScopedPage,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        pa(
            cmd(
                "identity.status.change",
                "/identity/{resource_type}/{resource_id}/status",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R3, V::Internal),
    );
    c.push(
        pa(
            get(
                "identity.scope-grants.list",
                "/identity/tenants/{tenant_id}/scope-grants",
                Q::ScopedPage,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        pa(
            cmd(
                "identity.scope-grant.change",
                "/identity/tenants/{tenant_id}/scope-grants",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R3, V::Internal),
    );
    c.push(
        pa(
            cmd(
                "identity.privileged-scope-grant.change",
                "/identity/tenants/{tenant_id}/privileged-scope-grants",
            ),
            S::CliToApi,
            P::A4,
        )
        .risk(R::R4, V::Internal),
    );
    c.push(
        pa(
            get(
                "identity.keys.list",
                "/identity/tenants/{tenant_id}/keys",
                Q::ScopedPage,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        pa(
            cmd("identity.key.issue", "/identity/tenants/{tenant_id}/keys"),
            S::Exists,
            P::A4,
        )
        .risk(R::R4, V::Internal),
    );
    c.push(
        pa(
            cmd(
                "identity.key.rotate",
                "/identity/tenants/{tenant_id}/keys/{key_id}/rotate",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R4, V::Internal),
    );
    c.push(
        pa(
            cmd(
                "identity.key.revoke",
                "/identity/tenants/{tenant_id}/keys/{key_id}/revoke",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R4, V::Internal),
    );
    c.push(
        pa(
            cmd(
                "identity.key.suspend",
                "/identity/tenants/{tenant_id}/keys/{key_id}/suspend",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R4, V::Internal),
    );
    c.push(
        pa(
            cmd(
                "identity.key.resume",
                "/identity/tenants/{tenant_id}/keys/{key_id}/resume",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R4, V::Internal),
    );
    c.push(
        pa(
            get(
                "identity.records.list",
                "/identity/records",
                Q::ScopedSearch,
            ),
            S::CliToApi,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        pa(
            cmd("identity.record.register", "/identity/records"),
            S::Exists,
            P::A4,
        )
        .risk(R::R4, V::Internal),
    );
    c.push(
        pa(
            cmd(
                "identity.record.status.change",
                "/identity/records/{record_id}/status",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R4, V::Internal),
    );
    c.push(
        pa(
            cmd(
                "identity.record.remove",
                "/identity/records/{record_id}/remove",
            ),
            S::Exists,
            P::A4,
        )
        .risk(R::R4, V::Internal),
    );
    c.push(
        pa(
            get(
                "identity.oauth-apps.list",
                "/identity/oauth-apps",
                Q::ScopedSearch,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        pa(
            get(
                "identity.app-grants.list",
                "/identity/oauth-apps/{app_id}/grants",
                Q::ScopedPage,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        pa(
            cmd(
                "identity.app-grant.revoke",
                "/identity/oauth-apps/{app_id}/grants/{grant_id}/revoke",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::Personal),
    );
    c.push(
        pa(
            get(
                "identity.partner-users.list",
                "/identity/tenants/{tenant_id}/partner-users",
                Q::ScopedSearch,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        pa(
            get("identity.audit.list", "/identity/audit", Q::ScopedWindow),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );

    // ---- 6.9 Apps (Developer Platform) -------------------------------------------
    c.push(
        dp(
            get("apps.apps.search", "/apps/apps", Q::ScopedSearch),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        dp(
            get("apps.apps.read", "/apps/apps/{app_id}", Q::Scoped),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        dp(
            get(
                "apps.versions.list",
                "/apps/apps/{app_id}/versions",
                Q::ScopedPage,
            ),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        dp(
            get(
                "apps.installs.list",
                "/apps/apps/{app_id}/installs",
                Q::ScopedPage,
            ),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        dp(
            cmd("apps.app.suspend", "/apps/apps/{app_id}/suspend"),
            S::Exists,
            P::A4,
        )
        .risk(R::R3, V::Internal),
    );
    c.push(
        dp(
            cmd(
                "apps.app.lift-suspension",
                "/apps/apps/{app_id}/lift-suspension",
            ),
            S::New,
            P::A4,
        )
        .risk(R::R3, V::Internal),
    );

    // ---- 6.10 Verification -------------------------------------------------------
    // A3: the verification queue (HQ's shared case shape) and its decisions, scoped to CF
    // Meet's first-party tenant (an operator function); served through the command core.
    const DP_CASES: &str = "GET /internal/v1/operator/verification/cases";
    c.push(
        dp(
            get(
                "verification.cases.list",
                "/verification/cases",
                Q::ScopedPage,
            ),
            S::New,
            P::A3,
        )
        .risk(R::R1, V::Internal)
        .source(DP_CASES)
        .result("VerificationCasePage")
        .served(&["verification_reads_are_strict_hq_case_shapes"]),
    );
    c.push(
        dp(
            get(
                "verification.cases.read",
                "/verification/cases/{case_id}",
                Q::Scoped,
            ),
            S::New,
            P::A3,
        )
        .risk(R::R1, V::Internal)
        .source("GET /internal/v1/operator/verification/cases/{caseId}")
        .result("VerificationCaseDetail")
        .served(&["verification_reads_are_strict_hq_case_shapes"]),
    );
    c.push(
        dp(
            get(
                "verification.case-history.list",
                "/verification/cases/{case_id}/history",
                Q::ScopedPage,
            ),
            S::New,
            P::A3,
        )
        .risk(R::R1, V::OperationsMinimized)
        .source("GET /internal/v1/operator/verification/cases/{caseId}/history")
        .result("VerificationCaseHistory")
        .served(&["verification_reads_are_strict_hq_case_shapes"]),
    );
    c.push(
        dp(
            cmd(
                "verification.case.claim",
                "/verification/cases/{case_id}/claim",
            ),
            S::Exists,
            P::A3,
        )
        .risk(R::R2, V::Internal)
        .source("POST /internal/v1/operator/{production-access-requests|apps}/{id}/review")
        .body("VerificationClaimRequest")
        .result("VerificationCommandResult")
        .served(&["verification_commands_reach_the_right_owner_route_once"]),
    );
    for (id, path, body, source) in [
        (
            "verification.partner-go-live.approve",
            "/verification/cases/{case_id}/partner-go-live/approve",
            "PartnerGoLiveApproveRequest",
            "POST /internal/v1/operator/production-access-requests/{requestId}/approve",
        ),
        (
            "verification.partner-go-live.reject",
            "/verification/cases/{case_id}/partner-go-live/reject",
            "VerificationRejectRequest",
            "POST /internal/v1/operator/production-access-requests/{requestId}/reject",
        ),
        (
            "verification.partner-go-live.suspend",
            "/verification/cases/{case_id}/partner-go-live/suspend",
            "VerificationDecisionRequest",
            "POST /internal/v1/operator/production-access-requests/{requestId}/suspend",
        ),
        (
            "verification.app-review.approve",
            "/verification/cases/{case_id}/app-review/approve",
            "VerificationDecisionRequest",
            "POST /internal/v1/operator/apps/{appId}/approve",
        ),
        (
            "verification.app-review.reject",
            "/verification/cases/{case_id}/app-review/reject",
            "VerificationRejectRequest",
            "POST /internal/v1/operator/apps/{appId}/reject",
        ),
        (
            "verification.app-review.publish",
            "/verification/cases/{case_id}/app-review/publish",
            "VerificationDecisionRequest",
            "POST /internal/v1/operator/apps/{appId}/publish",
        ),
    ] {
        c.push(
            dp(cmd(id, path), S::Exists, P::A3)
                .risk(R::R4, V::Internal)
                .source(source)
                .body(body)
                .result("VerificationCommandResult")
                .served(&["verification_commands_reach_the_right_owner_route_once"]),
        );
    }
    c.push(
        og(
            cmd(
                "verification.organization.decide",
                "/verification/cases/{case_id}/organization/decide",
            ),
            S::Future,
            P::Future,
        )
        .risk(R::R3, V::Internal)
        .roles(&[L::VerificationReviewer])
        .purposes(&[U::VerificationReview]),
    );
    c.push(
        get(
            "verification.number-kyc.list",
            "/verification/number-kyc",
            Q::ScopedPage,
        )
        .at(Numbering, S::TelecomOff, P::Future)
        .risk(R::R1, V::Personal),
    );
    c.push(
        cmd(
            "verification.number-kyc.decide",
            "/verification/number-kyc/{case_id}/decide",
        )
        .at(Numbering, S::TelecomOff, P::Future)
        .risk(R::R4, V::Personal),
    );

    // ---- 6.11 / 8 Pricing and plans (Developer Platform) -------------------------
    let pr = |b: B| b.at(DeveloperPlatform, S::New, P::A6);
    let approver: &[Role] = &[L::PricingApprover];
    c.push(
        pr(get(
            "pricing.price-books.list",
            "/pricing/price-books",
            Q::ScopedPage,
        ))
        .risk(R::R1, V::FinancialSensitive),
    );
    c.push(
        pr(get(
            "pricing.versions.list",
            "/pricing/price-books/{price_book_id}/versions",
            Q::ScopedPage,
        ))
        .risk(R::R1, V::FinancialSensitive),
    );
    c.push(
        pr(get(
            "pricing.versions.read",
            "/pricing/versions/{version_id}",
            Q::Scoped,
        ))
        .risk(R::R1, V::FinancialSensitive),
    );
    c.push(
        pr(cmd(
            "pricing.version.draft",
            "/pricing/price-books/{price_book_id}/versions",
        ))
        .risk(R::R3, V::FinancialSensitive),
    );
    c.push(
        pr(cmd(
            "pricing.version.edit",
            "/pricing/versions/{version_id}/edit",
        ))
        .risk(R::R3, V::FinancialSensitive),
    );
    c.push(
        pr(cmd(
            "pricing.version.submit",
            "/pricing/versions/{version_id}/submit",
        ))
        .risk(R::R3, V::FinancialSensitive),
    );
    c.push(
        pr(cmd(
            "pricing.version.approve",
            "/pricing/versions/{version_id}/approve",
        ))
        .risk(R::R4, V::FinancialSensitive)
        .roles(approver),
    );
    c.push(
        pr(cmd(
            "pricing.version.reject",
            "/pricing/versions/{version_id}/reject",
        ))
        .risk(R::R4, V::FinancialSensitive)
        .roles(approver),
    );
    c.push(
        pr(get(
            "pricing.activations.list",
            "/pricing/price-books/{price_book_id}/activations",
            Q::ScopedPage,
        ))
        .risk(R::R1, V::FinancialSensitive),
    );
    c.push(
        pr(cmd(
            "pricing.version.schedule",
            "/pricing/versions/{version_id}/schedule",
        ))
        .risk(R::R4, V::FinancialSensitive)
        .roles(approver),
    );
    c.push(
        pr(cmd(
            "pricing.version.activate",
            "/pricing/versions/{version_id}/activate",
        ))
        .risk(R::R4, V::FinancialSensitive)
        .roles(approver),
    );
    c.push(
        pr(cmd(
            "pricing.price-book.rollback",
            "/pricing/price-books/{price_book_id}/rollback",
        ))
        .risk(R::R4, V::FinancialSensitive)
        .roles(approver),
    );
    c.push(
        pr(get(
            "pricing.freezes.list",
            "/pricing/freezes",
            Q::ScopedPage,
        ))
        .risk(R::R1, V::FinancialSensitive),
    );
    c.push(
        pr(cmd(
            "pricing.market.freeze",
            "/pricing/markets/{market_code}/freeze",
        ))
        .risk(R::R4, V::FinancialSensitive)
        .roles(approver),
    );
    c.push(
        pr(cmd(
            "pricing.market.unfreeze",
            "/pricing/markets/{market_code}/unfreeze",
        ))
        .risk(R::R4, V::FinancialSensitive)
        .roles(approver),
    );
    c.push(
        pr(get("pricing.plans.list", "/pricing/plans", Q::ScopedPage))
            .risk(R::R1, V::FinancialSensitive),
    );
    c.push(
        pr(get(
            "pricing.plan-assignments.list",
            "/pricing/plan-assignments",
            Q::ScopedSearch,
        ))
        .risk(R::R1, V::FinancialSensitive),
    );
    c.push(
        pr(cmd("pricing.plan.assign", "/pricing/plan-assignments"))
            .risk(R::R3, V::FinancialSensitive),
    );
    c.push(
        pr(get(
            "pricing.overrides.list",
            "/pricing/overrides",
            Q::ScopedPage,
        ))
        .risk(R::R1, V::FinancialSensitive),
    );
    c.push(
        pr(cmd("pricing.override.change", "/pricing/overrides"))
            .risk(R::R4, V::FinancialSensitive)
            .roles(approver),
    );
    c.push(
        B::new(
            Kind::Read,
            Method::Post,
            "pricing.charges.simulate",
            "/pricing/simulations",
            Q::Scoped,
        )
        .at(DeveloperPlatform, S::New, P::A6)
        .risk(R::R1, V::FinancialSensitive),
    );

    // ---- 6.12 Finance reports ----------------------------------------------------
    let fi = |b: B| b.at(DeveloperPlatform, S::New, P::A6);
    c.push(
        fi(get(
            "finance.usage.report",
            "/finance/usage",
            Q::ScopedWindow,
        ))
        .risk(R::R1, V::FinancialSensitive),
    );
    c.push(
        fi(get(
            "finance.estimated-charges.report",
            "/finance/estimated-charges",
            Q::ScopedWindow,
        ))
        .risk(R::R1, V::FinancialSensitive),
    );
    c.push(
        fi(get(
            "finance.plan-spread.report",
            "/finance/plan-spread",
            Q::ScopedWindow,
        ))
        .risk(R::R1, V::FinancialSensitive),
    );

    // ---- 6.13 Notifications ------------------------------------------------------
    let nt = |b: B, st, ph| b.at(Notifications, st, ph);
    c.push(
        nt(
            get(
                "notifications.deliveries.list",
                "/notifications/deliveries",
                Q::ScopedWindow,
            ),
            S::ExistsNotGranted,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        nt(
            get(
                "notifications.failed-deliveries.list",
                "/notifications/failed-deliveries",
                Q::ScopedWindow,
            ),
            S::ExistsNotGranted,
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        nt(
            cmd(
                "notifications.delivery.retry",
                "/notifications/failed-deliveries/{delivery_id}/retry",
            ),
            S::ExistsNotGranted,
            P::A5,
        )
        .risk(R::R2, V::OperationsMinimized),
    );
    c.push(
        nt(
            get(
                "notifications.suppressions.list",
                "/notifications/suppressions",
                Q::ScopedSearch,
            ),
            S::Exists,
            P::A2,
        )
        .risk(R::R1, V::Personal),
    );
    c.push(
        nt(
            cmd(
                "notifications.suppression.remove",
                "/notifications/suppressions/{suppression_id}/remove",
            ),
            S::Exists,
            P::A5,
        )
        .risk(R::R2, V::Personal),
    );
    c.push(
        nt(
            get(
                "notifications.routing.read",
                "/notifications/routing",
                Q::Scoped,
            ),
            S::DbToApi,
            P::A2,
        )
        .risk(R::R1, V::Internal),
    );
    c.push(
        nt(
            cmd("notifications.routing.change", "/notifications/routing"),
            S::DbToApi,
            P::A5,
        )
        .risk(R::R3, V::Internal),
    );
    c.push(
        nt(
            cmd(
                "notifications.reroute.cancel",
                "/notifications/reroutes/{reroute_id}/cancel",
            ),
            S::Exists,
            P::A5,
        )
        .risk(R::R3, V::Internal),
    );
    c.push(
        nt(
            get(
                "notifications.providers.read",
                "/notifications/providers",
                Q::Scoped,
            ),
            S::New,
            P::A2,
        )
        .risk(R::R0, V::OperationsMinimized),
    );

    // ---- 6.14 Privacy requests (bridge-coordinated) ------------------------------
    c.push(
        get("privacy.requests.list", "/privacy/requests", Q::ScopedPage)
            .at(AdminBridge, S::Local, P::A7)
            .risk(R::R1, V::OperationsMinimized)
            .result("PrivacyRequestPage")
            .served(&["privacy_requests_complete_only_when_every_service_reports"]),
    );
    c.push(
        get(
            "privacy.requests.read",
            "/privacy/requests/{request_id}",
            Q::Scoped,
        )
        .at(AdminBridge, S::Local, P::A7)
        .uuid("request_id")
        .risk(R::R1, V::OperationsMinimized)
        .result("PrivacyRequest")
        .served(&["privacy_requests_complete_only_when_every_service_reports"]),
    );
    c.push(
        cmd("privacy.request.register", "/privacy/requests")
            .at(AdminBridge, S::Local, P::A7)
            .risk(R::R3, V::Personal)
            .body("PrivacyRegisterRequest")
            .result("PrivacyCommandResult")
            .served(&["privacy_requests_complete_only_when_every_service_reports"]),
    );
    c.push(
        cmd(
            "privacy.request.complete-service",
            "/privacy/requests/{request_id}/services/{service}/complete",
        )
        .at(AdminBridge, S::Local, P::A7)
        .uuid("request_id")
        .risk(R::R4, V::Personal)
        .body("PrivacyCompleteServiceRequest")
        .result("PrivacyCommandResult")
        .served(&["privacy_requests_complete_only_when_every_service_reports"]),
    );

    // ---- 6.15 Operations ---------------------------------------------------------
    let op = |b: B, ph| b.at(AdminBridge, S::Local, ph);
    c.push(
        op(
            get("operations.health.read", "/operations/health", Q::Scoped),
            P::A2,
        )
        .risk(R::R0, V::OperationsMinimized)
        .result("Health")
        .served(&["operations_health_is_the_aggregate_health"]),
    );
    c.push(
        op(
            get(
                "operations.incidents.list",
                "/operations/incidents",
                Q::ScopedWindow,
            ),
            P::A2,
        )
        .risk(R::R1, V::OperationsMinimized),
    );
    c.push(
        op(
            cmd(
                "operations.incident.resolve",
                "/operations/incidents/{incident_id}/resolve",
            ),
            P::A8,
        )
        .risk(R::R2, V::OperationsMinimized)
        .purposes(&[U::OpsIncident]),
    );
    c.push(
        op(
            get(
                "operations.switches.list",
                "/operations/switches",
                Q::ScopedPage,
            ),
            P::A2,
        )
        .risk(R::R0, V::Internal),
    );
    c.push(
        op(
            cmd(
                "operations.switch.change",
                "/operations/switches/{switch_id}",
            ),
            P::A8,
        )
        .risk(R::R3, V::Internal)
        .purposes(&[U::OpsIncident]),
    );
    c.push(
        op(
            get(
                "operations.activations.list",
                "/operations/activations",
                Q::ScopedPage,
            ),
            P::A8,
        )
        .risk(R::R0, V::Internal),
    );
    c.push(
        op(
            cmd("operations.activation.request", "/operations/activations"),
            P::A8,
        )
        .risk(R::R4, V::Internal)
        .roles(&[L::ReleaseManager])
        .purposes(&[U::ReleaseRequest]),
    );
    c.push(
        op(
            get(
                "operations.releases.list",
                "/operations/releases",
                Q::ScopedWindow,
            ),
            P::A8,
        )
        .risk(R::R0, V::Internal),
    );

    // ---- 6.16 Audit ------------------------------------------------------------
    c.push(
        get("audit.entries.list", "/audit/entries", Q::ScopedWindow)
            .at(AdminBridge, S::Local, P::A2)
            .risk(R::R1, V::OperationsMinimized)
            .result("AuditEntryPage")
            .served(&["audit_entries_page_the_hash_chained_audit"]),
    );

    // ---- 6.17 Telecom (read-only while telecom is off) ---------------------------
    for (id, path) in [
        ("telecom.numbers.summary", "/telecom/numbers"),
        ("telecom.calls.summary", "/telecom/calls"),
        ("telecom.sms.summary", "/telecom/sms"),
        ("telecom.privacy-relay.summary", "/telecom/privacy-relay"),
    ] {
        c.push(
            get(id, path, Q::Scoped)
                .at(Numbering, S::TelecomOff, P::Future)
                .risk(R::R0, V::OperationsMinimized),
        );
    }

    let mut caps: Vec<Capability> = c.into_iter().map(|b| b.0).collect();
    for (id, result, tests) in SERVED_OWNER_READS {
        let cap = caps
            .iter_mut()
            .find(|c| c.id == *id)
            .unwrap_or_else(|| panic!("{id}: not in the catalog"));
        assert_eq!(cap.kind, Kind::Read, "{id}: owner reads only");
        cap.result = Some(result);
        cap.served = true;
        cap.tests = tests.to_vec();
    }
    for id in CONTACT_SEARCH {
        let cap = caps
            .iter_mut()
            .find(|c| c.id == *id)
            .unwrap_or_else(|| panic!("{id}: not in the catalog"));
        assert_eq!(
            cap.shape,
            QueryShape::ScopedSearch,
            "{id}: only searches take q"
        );
        cap.contact_search = true;
    }
    // Owner commands served from their rows (contract::owner_commands).
    for row in super::owner_commands::COMMANDS {
        let cap = caps
            .iter_mut()
            .find(|c| c.id == row.id)
            .unwrap_or_else(|| panic!("{}: not in the catalog", row.id));
        assert_eq!(cap.kind, Kind::Command, "{}: commands only", row.id);
        assert_eq!(cap.owner, row.owner, "{}: owner", row.id);
        cap.served = true;
        cap.source_endpoint = Some(row.source);
        cap.body = Some(row.body_schema);
        cap.result = Some(row.result_schema);
        cap.tests = vec!["owner_commands_follow_their_rows"];
        if matches!(
            cap.owner_status,
            OwnerStatus::ExistsNotGranted
                | OwnerStatus::New
                | OwnerStatus::CliToApi
                | OwnerStatus::DbToApi
        ) {
            cap.owner_status = OwnerStatus::Exists;
        }
    }
    // bridge.operations.read: whoever may run a command may read its operation back, for
    // that command's purposes (the readback also checks the operation's own purpose).
    let mut roles: Vec<Role> = Vec::new();
    let mut purposes: Vec<Purpose> = Vec::new();
    for c in caps.iter().filter(|c| c.kind == Kind::Command) {
        roles.extend(c.roles.iter().copied());
        purposes.extend(c.purposes.iter().copied());
    }
    roles.sort();
    roles.dedup();
    purposes.sort();
    purposes.dedup();
    let readback = caps
        .iter_mut()
        .find(|c| c.id == "bridge.operations.read")
        .expect("bridge.operations.read");
    readback.roles = roles;
    readback.purposes = purposes;
    caps
}

/// Searches whose `q` may carry an email address or phone number (agreed with HQ Track B:
/// published as `x-contact-search`; HQ routes them through its POST search so a contact
/// identifier never lands in a logged URL). Add a capability here when its slice lands.
const CONTACT_SEARCH: &[&str] = &[
    "accounts.users.search",
    "partners.users.list",
    "identity.partner-users.list",
    "notifications.suppressions.list",
    // A partner-chosen principal reference may be an email address (plaintext telephone
    // numbers are refused by Platform Access at registration).
    "identity.records.list",
];

/// Owner reads the bridge serves, by phase: (capability, result schema, covering tests).
/// The owner endpoint must also be granted and reachable; until then the bridge answers
/// `OWNER_UNAVAILABLE`, never empty data.
#[allow(clippy::type_complexity)]
const SERVED_OWNER_READS: &[(&str, &str, &[&str])] = &[
    // A2.0 Organizations.
    (
        "organizations.organizations.search",
        "OrganizationPage",
        &["organizations_search_translates_and_minimizes"],
    ),
    (
        "organizations.organizations.read",
        "OrganizationDetail",
        &["organizations_detail_never_relays_contact_details"],
    ),
    (
        "organizations.audit.list",
        "OrganizationAuditPage",
        &["organizations_audit_maps_the_window_and_cursor"],
    ),
    (
        "organizations.global-audit.list",
        "OrganizationAuditPage",
        &["organizations_global_audit_is_served"],
    ),
    (
        "organizations.members.list",
        "OrganizationMemberPage",
        &["organizations_members_are_opaque_user_ids"],
    ),
    (
        "organizations.policies.read",
        "OrganizationPolicies",
        &["organizations_policies_are_relayed_field_by_field"],
    ),
    (
        "organizations.teams.list",
        "OrganizationTeamPage",
        &["organizations_teams_drop_free_text"],
    ),
    (
        "organizations.bindings.list",
        "OrganizationBindingPage",
        &["organizations_bindings_treat_absent_as_unbound"],
    ),
    // A2.2 Notifications.
    (
        "notifications.deliveries.list",
        "NotificationDeliveryPage",
        &["notifications_deliveries_map_the_window_and_carry_no_content"],
    ),
    (
        "notifications.failed-deliveries.list",
        "NotificationDeliveryPage",
        &["notifications_failed_deliveries_use_the_failure_route"],
    ),
    (
        "notifications.suppressions.list",
        "NotificationSuppressionPage",
        &["notifications_suppressions_relay_only_the_digest"],
    ),
    (
        "notifications.routing.read",
        "NotificationRouting",
        &["notifications_routing_drops_the_audit_digest"],
    ),
    (
        "notifications.providers.read",
        "NotificationProviders",
        &["notifications_providers_report_status_only"],
    ),
    // A2.1 Developer Platform.
    (
        "partners.workspaces.search",
        "PartnerWorkspacePage",
        &["partners_workspaces_search_drops_the_support_address"],
    ),
    (
        "partners.workspaces.read",
        "PartnerWorkspaceDetail",
        &["partners_workspace_detail_carries_counts"],
    ),
    (
        "partners.workspace-activity.list",
        "PartnerActivityPage",
        &["partners_activity_maps_window_and_workspace"],
    ),
    (
        "partners.projects.list",
        "PartnerProjectPage",
        &["partners_lists_translate_strictly"],
    ),
    (
        "partners.environments.list",
        "PartnerEnvironmentPage",
        &["partners_lists_translate_strictly"],
    ),
    (
        "partners.members.list",
        "PartnerMemberPage",
        &["partners_lists_translate_strictly"],
    ),
    (
        "partners.invitations.list",
        "PartnerInvitationPage",
        &["partners_lists_translate_strictly"],
    ),
    (
        "partners.push-keys.list",
        "PartnerPushKeyPage",
        &["partners_push_keys_are_details_only"],
    ),
    (
        "partners.webhooks.list",
        "PartnerWebhookPage",
        &["partners_webhooks_expose_the_host_only"],
    ),
    (
        "partners.webhook-deliveries.list",
        "PartnerWebhookDeliveryPage",
        &["partners_activity_maps_window_and_workspace"],
    ),
    ("apps.apps.search", "AppPage", &["apps_search_and_detail"]),
    ("apps.apps.read", "AppDetail", &["apps_search_and_detail"]),
    (
        "apps.versions.list",
        "AppVersionPage",
        &["partners_lists_translate_strictly"],
    ),
    (
        "apps.installs.list",
        "AppInstallPage",
        &["partners_lists_translate_strictly"],
    ),
    // A2.3 Platform Access.
    (
        "identity.tenants.list",
        "IdentityTenantPage",
        &["identity_tenants_search_translates"],
    ),
    (
        "identity.applications.list",
        "IdentityApplicationPage",
        &["identity_tenant_lists_translate_strictly"],
    ),
    (
        "identity.environments.list",
        "IdentityEnvironmentPage",
        &["identity_tenant_lists_translate_strictly"],
    ),
    (
        "identity.service-accounts.list",
        "IdentityServiceAccountPage",
        &["identity_tenant_lists_translate_strictly"],
    ),
    (
        "identity.scope-grants.list",
        "IdentityScopeGrantPage",
        &["identity_scope_grants_check_the_cursor"],
    ),
    (
        "identity.keys.list",
        "IdentityKeyPage",
        &["identity_keys_never_carry_secret_material"],
    ),
    (
        "identity.records.list",
        "IdentityPrincipalPage",
        &["identity_records_never_echo_the_reference"],
    ),
    (
        "identity.oauth-apps.list",
        "IdentityOAuthAppPage",
        &["identity_oauth_apps_and_grants_translate"],
    ),
    (
        "identity.app-grants.list",
        "IdentityAppGrantPage",
        &["identity_oauth_apps_and_grants_translate"],
    ),
    (
        "identity.partner-users.list",
        "IdentityPrincipalPage",
        &["identity_partner_users_filter_the_kind"],
    ),
    (
        "identity.audit.list",
        "IdentityAuditEntryPage",
        &["identity_audit_maps_the_window"],
    ),
    (
        "partners.keys.list",
        "IdentityKeyPage",
        &["partners_identity_reads_use_the_workspace_tenant"],
    ),
    (
        "partners.service-accounts.list",
        "IdentityServiceAccountPage",
        &["partners_identity_reads_use_the_workspace_tenant"],
    ),
    (
        "partners.websites.list",
        "PartnerWebsitesPage",
        &["partners_identity_reads_use_the_workspace_tenant"],
    ),
    (
        "partners.users.list",
        "IdentityPrincipalPage",
        &["partners_identity_reads_use_the_workspace_tenant"],
    ),
    // A2.4 Communications.
    (
        "communications.profiles.read",
        "CommunicationProfile",
        &["communications_profile_never_relays_contact_fields"],
    ),
    (
        "communications.reports.list",
        "CommunicationReportPage",
        &["communications_reports_and_holds_page_by_time_cursor"],
    ),
    (
        "communications.holds.list",
        "CommunicationHoldPage",
        &["communications_reports_and_holds_page_by_time_cursor"],
    ),
    (
        "communications.audit.list",
        "CommunicationAuditPage",
        &["communications_audit_maps_the_window_and_drops_details"],
    ),
    (
        "communications.metrics.read",
        "Summary",
        &["communications_metrics_are_an_hq_summary"],
    ),
    // A2.5 Meeting Service (accounts, meetings).
    (
        "accounts.users.search",
        "AccountUserPage",
        &["meetings_user_search_never_returns_an_address"],
    ),
    (
        "accounts.users.read",
        "AccountUserDetail",
        &["meetings_user_detail_masks_the_address"],
    ),
    (
        "accounts.sessions.list",
        "AccountSessionPage",
        &["meetings_account_lists_translate_strictly"],
    ),
    (
        "accounts.devices.list",
        "AccountDevicePage",
        &["meetings_account_lists_translate_strictly"],
    ),
    (
        "accounts.mfa.read",
        "AccountMfa",
        &["meetings_mfa_reports_state_only"],
    ),
    (
        "accounts.security-history.list",
        "AccountSecurityEventPage",
        &["meetings_security_history_maps_window_and_sequence_cursor"],
    ),
    (
        "meetings.spaces.search",
        "MeetingSpacePage",
        &["meetings_space_and_meeting_search"],
    ),
    (
        "meetings.meetings.search",
        "MeetingConferencePage",
        &["meetings_space_and_meeting_search"],
    ),
    (
        "meetings.meetings.read",
        "MeetingConference",
        &["meetings_detail_and_participants"],
    ),
    (
        "meetings.live.list",
        "MeetingConferencePage",
        &["meetings_live_list"],
    ),
    (
        "meetings.participants.list",
        "MeetingParticipantPage",
        &["meetings_detail_and_participants"],
    ),
    (
        "meetings.audit.list",
        "MeetingAuditEventPage",
        &["meetings_audit_maps_window_and_time_cursor"],
    ),
    // A2.6 Messaging.
    (
        "messaging.reports.list",
        "MessagingReportPage",
        &["messaging_reports_relay_no_evidence"],
    ),
    (
        "messaging.reports.read",
        "MessagingReportDetail",
        &["messaging_report_detail_carries_actions_not_evidence"],
    ),
    (
        "messaging.holds.list",
        "MessagingHoldPage",
        &["messaging_lists_translate_strictly"],
    ),
    (
        "messaging.exports.list",
        "MessagingExportPage",
        &["messaging_lists_translate_strictly"],
    ),
    (
        "messaging.deletion-requests.list",
        "MessagingDeletionRequestPage",
        &["messaging_lists_translate_strictly"],
    ),
    (
        "messaging.audit.list",
        "MessagingAuditPage",
        &["messaging_windowed_lists_map_since_and_until"],
    ),
    (
        "messaging.switches.list",
        "MessagingSwitchPage",
        &["messaging_switches_are_served"],
    ),
    (
        "messaging.incidents.list",
        "MessagingIncidentPage",
        &["messaging_windowed_lists_map_since_and_until"],
    ),
    (
        "messaging.failed-messages.list",
        "MessagingFailedMessagePage",
        &["messaging_windowed_lists_map_since_and_until"],
    ),
    (
        "messaging.key-transparency.read",
        "MessagingKeyTransparency",
        &["messaging_key_transparency_is_hashes_and_sizes_only"],
    ),
    // A2.7 Calling.
    (
        "calling.calls.list",
        "CallingCallPage",
        &["calling_calls_are_tenant_scoped_and_minimized"],
    ),
    (
        "calling.calls.read",
        "CallingCallDetail",
        &["calling_call_detail_lists_participants_by_id_only"],
    ),
    (
        "calling.live.list",
        "CallingCallPage",
        &["calling_lists_translate_strictly"],
    ),
    (
        "calling.abuse-reports.list",
        "CallingAbuseReportPage",
        &["calling_lists_translate_strictly"],
    ),
    (
        "calling.blocks.list",
        "CallingBlockRulePage",
        &["calling_blocks_never_name_a_contact"],
    ),
    (
        "calling.limits.read",
        "CallingLimits",
        &["calling_configuration_reads_are_bounded"],
    ),
    (
        "calling.routing.read",
        "CallingRoutingPolicies",
        &["calling_configuration_reads_are_bounded"],
    ),
    (
        "calling.enterprise-features.read",
        "CallingEnterprisePolicies",
        &["calling_configuration_reads_are_bounded"],
    ),
    (
        "calling.spend-limits.read",
        "CallingSpendPolicies",
        &["calling_configuration_reads_are_bounded"],
    ),
    (
        "calling.recordings.list",
        "CallingRecordingPage",
        &["calling_media_reads_are_metadata_only"],
    ),
    (
        "calling.voicemail.list",
        "CallingVoicemailPage",
        &["calling_media_reads_are_metadata_only"],
    ),
    (
        "calling.retention-holds.list",
        "CallingRetentionHoldPage",
        &["calling_lists_translate_strictly"],
    ),
    (
        "calling.incidents.list",
        "CallingIncidentPage",
        &["calling_lists_translate_strictly"],
    ),
    (
        "calling.webhook-keys.list",
        "CallingWebhookKeyPage",
        &["calling_lists_translate_strictly"],
    ),
    (
        "calling.audit.list",
        "CallingAuditEntryPage",
        &["calling_lists_translate_strictly"],
    ),
];
