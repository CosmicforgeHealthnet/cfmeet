//! A. Migration certification: clean up / down / up (also a partial rollback of the call
//! domain on top of the Part B schema), required constraints / triggers / indexes, no
//! plaintext secret or contact columns, no provider columns, no Privacy Relay /
//! Telephony / Meeting Service tables.

mod common;

use calling_migration::{MigratorTrait, MIGRATION_TABLE};
use common::TestDb;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};

async fn strings(conn: &DatabaseConnection, sql: &str) -> Vec<String> {
    conn.query_all(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
        .collect()
}

async fn names(conn: &DatabaseConnection, sql: &str) -> Vec<String> {
    strings(conn, sql).await
}

async fn tables(conn: &DatabaseConnection) -> Vec<String> {
    strings(
        conn,
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename",
    )
    .await
}

const PLATFORM_TABLES: &[&str] = &[
    "platform_api_credential_scopes",
    "platform_api_credentials",
    "platform_application_limits",
    "platform_application_scopes",
    "platform_applications",
    "platform_credential_limits",
    "platform_environments",
    "platform_principals",
    "platform_request_rate_windows",
    "platform_service_accounts",
    "platform_tenants",
];

/// The Calling-owned call domain (Part C §15–§25).
const CALL_TABLES: &[&str] = &[
    "call_answer_claims",
    "call_answer_group_claims",
    "call_attempts",
    "call_authorizations",
    "call_commands",
    "call_endpoints",
    "call_events",
    "call_inbound_owners",
    "call_invitations",
    "call_legs",
    "call_media_provider_events",
    "call_media_sessions",
    "call_participant_media_permissions",
    "call_participants",
    "call_privacy_authorizations",
    "call_privacy_enforcements",
    "call_public_bridges",
    "call_public_parties",
    "call_quality_records",
    "call_routes",
    "call_routing_policies",
    "call_session_intervals",
    "call_sessions",
    "call_spend_policies",
    "call_spend_reservations",
    "call_telephony_commands",
    "call_telephony_events",
    "call_transfers",
    "call_usage_records",
    "calling_incidents",
    "calling_outbox_events",
    "calls",
];

/// Part F enterprise calling and continuity (Calling-owned, ADR 0018).
const ENTERPRISE_TABLES: &[&str] = &[
    "enterprise_admin_audit",
    "enterprise_business_hours",
    "enterprise_business_hours_exceptions",
    "enterprise_business_hours_windows",
    "enterprise_call_contexts",
    "enterprise_call_policies",
    "enterprise_call_queues",
    "enterprise_department_members",
    "enterprise_departments",
    "enterprise_extensions",
    "enterprise_forwarding_rules",
    "enterprise_ivr_flow_destinations",
    "enterprise_ivr_flow_versions",
    "enterprise_ivr_flows",
    "enterprise_ivr_sessions",
    "enterprise_public_contacts",
    "enterprise_queue_entries",
    "enterprise_queue_members",
    "enterprise_queue_offers",
    "enterprise_ring_group_members",
    "enterprise_ring_groups",
    "enterprise_routing_steps",
    "enterprise_sip_devices",
    "voicemail_mailboxes",
    "voicemail_messages",
    "call_recording_access_audit",
    "call_recording_authorizations",
    "call_recording_consents",
    "call_recording_policies",
    "call_recordings",
];

/// Part G §68: customer webhooks (Calling-owned, ADR 0028).
const WEBHOOK_TABLES: &[&str] = &[
    "webhook_deliveries",
    "webhook_delivery_attempts",
    "webhook_events",
    "webhook_subscriptions",
];

/// Derived from the migrator itself (never a brittle hand-maintained count).
fn migrations() -> usize {
    <calling_migration::Migrator as MigratorTrait>::migrations().len()
}
const PART_B_MIGRATIONS: u32 = 4;

#[tokio::test]
async fn migrations_apply_roll_back_and_reapply_on_a_clean_database() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let conn = &db.conn;
    assert!(tables(conn).await.is_empty());

    calling_migration::Migrator::up(conn, None).await.unwrap();
    let mut expected: Vec<String> = PLATFORM_TABLES
        .iter()
        .chain(CALL_TABLES)
        .chain(ENTERPRISE_TABLES)
        .chain(WEBHOOK_TABLES)
        .map(|s| s.to_string())
        .collect();
    expected.push(MIGRATION_TABLE.to_string());
    expected.sort();
    assert_eq!(tables(conn).await, expected);
    assert!(calling_migration::Migrator::get_pending_migrations(conn)
        .await
        .unwrap()
        .is_empty());

    // Roll back only the call domain: the Part B schema is intact, including the
    // principal ownership targets the call core added.
    calling_migration::Migrator::down(conn, Some(migrations() as u32 - PART_B_MIGRATIONS))
        .await
        .unwrap();
    let mut part_b: Vec<String> = PLATFORM_TABLES.iter().map(|s| s.to_string()).collect();
    part_b.push(MIGRATION_TABLE.to_string());
    part_b.sort();
    assert_eq!(tables(conn).await, part_b);
    let principal_constraints = names(
        conn,
        "SELECT conname::text FROM pg_constraint WHERE conname LIKE 'uq_platform_principals_owner%'",
    )
    .await;
    assert!(
        principal_constraints.is_empty(),
        "{principal_constraints:?}"
    );
    let calling_functions = names(
        conn,
        "SELECT proname::text FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
         WHERE n.nspname = 'public' AND (proname LIKE 'calling\\_%' OR proname LIKE 'enterprise\\_%')",
    )
    .await;
    assert!(calling_functions.is_empty(), "{calling_functions:?}");
    calling_migration::Migrator::up(conn, None).await.unwrap();
    assert_eq!(tables(conn).await, expected);

    // Full rollback (reset): only the migration bookkeeping table remains, and the shared
    // guard functions are gone.
    calling_migration::Migrator::down(conn, None).await.unwrap();
    assert_eq!(tables(conn).await, vec![MIGRATION_TABLE.to_string()]);
    let functions = strings(
        conn,
        "SELECT proname::text FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
         WHERE n.nspname = 'public' AND (proname LIKE 'platform\\_%' OR proname LIKE 'calling\\_%' \
         OR proname LIKE 'enterprise\\_%')",
    )
    .await;
    assert!(functions.is_empty(), "{functions:?}");
    assert_eq!(
        calling_migration::Migrator::get_pending_migrations(conn)
            .await
            .unwrap()
            .len(),
        migrations()
    );

    // And up again.
    calling_migration::Migrator::up(conn, None).await.unwrap();
    assert_eq!(tables(conn).await, expected);
    let applied = calling_migration::Migrator::get_applied_migrations(conn)
        .await
        .unwrap();
    assert_eq!(applied.len(), migrations());
}

#[tokio::test]
async fn migration_history_is_owned_by_calling() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let all = tables(&db.conn).await;
    assert!(all.contains(&MIGRATION_TABLE.to_string()));
    assert!(
        !all.contains(&"seaql_migrations".to_string()),
        "Calling must not share the default migration table"
    );
}

#[tokio::test]
async fn required_constraints_and_guards_exist() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let constraints = strings(
        &db.conn,
        "SELECT conname::text FROM pg_constraint c JOIN pg_namespace n ON n.oid = c.connamespace \
         WHERE n.nspname = 'public'",
    )
    .await;
    for required in [
        // Uniqueness
        "uq_platform_tenants_slug",
        "uq_platform_applications_tenant_slug",
        "uq_platform_environments_application_kind",
        "uq_platform_service_accounts_environment_name",
        "uq_platform_api_credentials_public_id",
        "uq_platform_principals_scoped_reference",
        "uq_platform_api_credentials_rotated_from",
        "uq_platform_api_credentials_superseded_by",
        // Ownership-consistent composite foreign keys
        "fk_platform_applications_tenant",
        "fk_platform_environments_application",
        "fk_platform_service_accounts_environment",
        "fk_platform_api_credentials_service_account",
        "fk_platform_api_credentials_rotated_from",
        "fk_platform_api_credential_scopes_credential",
        "fk_platform_api_credential_scopes_application_scope",
        "fk_platform_principals_environment",
        "fk_platform_principals_service_account",
        "fk_platform_application_limits_application",
        "fk_platform_credential_limits_credential",
        // Status / enum checks
        "ck_platform_tenants_classification",
        "ck_platform_tenants_status",
        "ck_platform_applications_status",
        "ck_platform_environments_kind",
        "ck_platform_environments_status",
        "ck_platform_service_accounts_status",
        "ck_platform_api_credentials_status",
        "ck_platform_principals_kind",
        "ck_platform_principals_status",
        // Security invariants
        "ck_platform_environments_live_telecom",
        "ck_platform_api_credentials_verifier_length",
        "ck_platform_api_credentials_verifier_algorithm",
        "ck_platform_api_credentials_supersession_never_extends",
        "ck_platform_api_credentials_revoked",
        "ck_platform_principals_reference_not_telephone",
        "ck_platform_principals_telephone_endpoint_opaque",
        "ck_platform_principals_service_account_link",
        "ck_platform_principals_service_account_reference",
        // Call domain: ownership-consistent composite keys …
        "uq_platform_principals_owner_id",
        "uq_platform_principals_owner_kind_id",
        "uq_calls_owner_id",
        "fk_calls_credential",
        "fk_calls_initiator_principal",
        "fk_call_participants_call",
        "fk_call_participants_principal",
        "fk_call_sessions_participant",
        "fk_call_sessions_replaces",
        "fk_call_session_intervals_session",
        "fk_call_routes_call",
        "fk_call_legs_call",
        "fk_call_legs_route",
        "fk_call_legs_participant",
        "fk_call_attempts_call",
        "fk_call_attempts_route",
        "fk_call_attempts_leg",
        "fk_call_attempts_previous",
        "fk_call_authorizations_call",
        "fk_call_authorizations_credential",
        "fk_call_authorizations_principal",
        "fk_call_authorizations_superseded_by",
        "fk_call_events_call",
        "fk_call_events_actor_credential",
        "fk_call_quality_records_session",
        "fk_call_quality_records_leg",
        "fk_call_usage_records_participant",
        "fk_call_usage_records_session",
        "fk_call_usage_records_leg",
        "fk_calling_outbox_events_source",
        "fk_calling_outbox_events_call",
        // … uniqueness …
        "uq_call_participants_call_principal",
        "uq_call_sessions_media_identity",
        "uq_call_session_intervals_session_number",
        "uq_call_attempts_call_number",
        "uq_call_events_call_sequence",
        "uq_call_usage_records_idempotency",
        "uq_calling_outbox_events_event_id",
        "uq_calling_outbox_events_source",
        // … and state / invariant checks.
        "ck_calls_state",
        "ck_calls_kind",
        "ck_calls_version",
        "ck_calls_terminal_complete",
        "ck_calls_terminal_reason_matches_state",
        "ck_calls_failure_only_when_failed",
        "ck_calls_timestamps_ordered",
        "ck_call_participants_display_name",
        "ck_call_sessions_media_identity",
        "ck_call_sessions_state",
        "ck_call_session_intervals_order",
        "ck_call_legs_state",
        "ck_call_attempts_unknown",
        "ck_call_authorizations_reason_matches_decision",
        "ck_call_events_state_fields",
        "ck_call_events_payload",
        "ck_call_quality_records_metrics",
        "ck_call_usage_records_quantity",
        "ck_calling_outbox_events_envelope",
        // Part C §20–§25: single-winner arbitration, idempotency, dedupe, one media
        // resource per Call with a deterministic name, transfer and invitation rules.
        "uq_call_answer_claims_participant",
        "uq_call_answer_claims_invitation",
        "uq_call_answer_claims_session",
        "uq_call_commands_idempotency",
        "uq_call_media_provider_events_dedupe",
        "uq_call_media_sessions_call",
        "uq_call_media_sessions_name",
        "uq_call_media_sessions_leg",
        "uq_call_participant_media_permissions_participant",
        "uq_call_participants_owner_principal",
        "ck_call_endpoints_key",
        "ck_call_invitations_expiry",
        "ck_call_invitations_status",
        "ck_call_media_sessions_name",
        "ck_call_commands_snapshot",
        "ck_call_transfers_pre_state",
        "ck_call_transfers_distinct_parties",
    ] {
        assert!(
            constraints.iter().any(|c| c == required),
            "missing constraint {required}"
        );
    }

    let triggers = strings(
        &db.conn,
        "SELECT tgname::text FROM pg_trigger WHERE NOT tgisinternal ORDER BY tgname",
    )
    .await;
    for required in [
        "trg_call_answer_claims_immutable",
        "trg_call_invitations_guard_insert",
        "trg_call_invitations_require_claim",
        "trg_call_invitations_status_transition",
        "trg_call_sessions_guard_media",
        "trg_call_sessions_guard_endpoint",
        "trg_call_sessions_media_access_transition",
        "trg_call_commands_status_transition",
        "trg_call_transfers_state_transition",
        "trg_call_media_sessions_status_transition",
        "trg_call_media_provider_events_immutable",
        "trg_call_endpoints_status_transition",
        "trg_platform_tenants_immutable",
        "trg_platform_applications_immutable",
        "trg_platform_environments_immutable",
        "trg_platform_service_accounts_immutable",
        "trg_platform_service_accounts_revoked_terminal",
        "trg_platform_api_credentials_immutable",
        "trg_platform_api_credentials_set_once",
        "trg_platform_api_credentials_revoked_terminal",
        "trg_platform_principals_immutable",
        "trg_calls_guard_insert",
        "trg_calls_guard_transition",
        "trg_calls_immutable",
        "trg_calls_set_once",
        "trg_calls_no_delete",
        "trg_calls_require_state_event_on_insert",
        "trg_calls_require_state_event_on_transition",
        "trg_call_participants_immutable",
        "trg_call_sessions_state_transition",
        "trg_call_sessions_monotonic",
        "trg_call_session_intervals_guard_insert",
        "trg_call_session_intervals_set_once",
        "trg_call_routes_state_transition",
        "trg_call_legs_state_transition",
        "trg_call_legs_set_once",
        "trg_call_attempts_guard_insert",
        "trg_call_attempts_outcome_transition",
        "trg_call_attempts_unknown_kept",
        "trg_call_authorizations_immutable",
        "trg_call_events_guard_insert",
        "trg_call_events_immutable",
        "trg_call_events_no_truncate",
        "trg_call_events_require_outbox",
        "trg_call_quality_records_immutable",
        "trg_call_usage_records_set_once",
        "trg_calling_outbox_events_immutable",
        "trg_calling_outbox_events_guard_delete",
    ] {
        assert!(
            triggers.iter().any(|t| t == required),
            "missing trigger {required}"
        );
    }

    // Every timestamp is TIMESTAMPTZ; every id is a UUID primary key.
    let naive = strings(
        &db.conn,
        "SELECT table_name || '.' || column_name FROM information_schema.columns \
         WHERE table_schema = 'public' AND data_type = 'timestamp without time zone'",
    )
    .await;
    assert!(naive.is_empty(), "naive timestamps: {naive:?}");
    let id_types = strings(
        &db.conn,
        "SELECT table_name || ':' || data_type FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name <> 'calling_seaql_migrations' AND column_name = 'id'",
    )
    .await;
    assert!(!id_types.is_empty());
    assert!(
        id_types.iter().all(|t| t.ends_with(":uuid")),
        "{id_types:?}"
    );

    // Ownership indexes exist.
    let indexes = strings(
        &db.conn,
        "SELECT indexname::text FROM pg_indexes WHERE schemaname = 'public'",
    )
    .await;
    for required in [
        "idx_platform_service_accounts_application",
        "idx_platform_api_credentials_service_account",
        "idx_platform_api_credentials_application_environment",
        "idx_platform_principals_environment",
        "idx_platform_application_scopes_tenant",
        "idx_calls_scope_created",
        "idx_calls_scope_state",
        "idx_calls_initiator_principal",
        "idx_calls_active_by_credential",
        "idx_calls_active_by_application",
        "idx_call_participants_principal",
        "idx_call_sessions_call",
        "idx_call_sessions_participant",
        "uq_call_session_intervals_one_open",
        "idx_call_session_intervals_call",
        "idx_call_routes_call",
        "idx_call_legs_call",
        "idx_call_legs_route",
        "idx_call_attempts_route",
        "idx_call_attempts_leg",
        "uq_call_authorizations_one_effective",
        "idx_call_authorizations_call_decided",
        "idx_call_authorizations_decision",
        "uq_call_events_state_change_per_version",
        "idx_call_quality_records_call",
        "idx_call_quality_records_session",
        "idx_call_quality_records_leg",
        "uq_call_participants_one_initiator",
        "idx_calling_outbox_events_pending",
        "idx_calling_outbox_events_aggregate",
        "uq_call_endpoints_active_key",
        "uq_call_invitations_one_live_per_endpoint",
        "uq_call_transfers_one_active",
        "idx_call_invitations_live_expiry",
    ] {
        assert!(
            indexes.iter().any(|i| i == required),
            "missing index {required}"
        );
    }
}

#[tokio::test]
async fn no_plaintext_secret_or_contact_columns_exist() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let columns = strings(
        &db.conn,
        "SELECT table_name || '.' || column_name FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name LIKE 'platform\\_%'",
    )
    .await;
    for column in &columns {
        let name = column.split('.').nth(1).unwrap();
        for forbidden in [
            "secret",
            "plaintext",
            "raw",
            "password",
            "token",
            "api_key",
            "phone",
            "e164",
            "msisdn",
            "email",
            "contact",
            "number",
        ] {
            assert!(
                !name.contains(forbidden),
                "column {column} looks like plaintext secret/contact storage"
            );
        }
    }
    let credential_columns = strings(
        &db.conn,
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = 'platform_api_credentials' ORDER BY column_name",
    )
    .await;
    assert_eq!(
        credential_columns,
        vec![
            "application_id",
            "created_at",
            "environment_id",
            "expires_at",
            "id",
            "label",
            "last_four",
            "not_before",
            "public_id",
            "revoked_at",
            "rotated_from_credential_id",
            "service_account_id",
            "status",
            "superseded_at",
            "superseded_by_credential_id",
            "supersession_valid_until",
            "tenant_id",
            "updated_at",
            "verifier",
            "verifier_algorithm",
            "verifier_key_version",
        ]
    );
}

#[tokio::test]
async fn call_domain_columns_hold_no_contact_secret_or_provider_data() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let columns = strings(
        &db.conn,
        "SELECT table_name || '.' || column_name FROM information_schema.columns \
         WHERE table_schema = 'public' AND (table_name LIKE 'call\\_%' OR table_name IN ('calls', 'calling_outbox_events'))",
    )
    .await;
    assert!(columns.len() > 100, "{}", columns.len());
    // Ordinal counters are the only `*_number` columns; they are integers.
    const ORDINALS: &[&str] = &[
        "call_session_intervals.interval_number",
        "call_attempts.attempt_number",
    ];
    // Calling-private media evidence (Part C §24, §46): the adapter name, the provider's
    // opaque resource reference and its event id (dedupe key) live only in the two
    // private media tables and are never exposed by an API, event or signal.
    const PRIVATE_PROVIDER_EVIDENCE: &[&str] = &[
        "call_media_sessions.provider_adapter",
        "call_media_sessions.provider_resource_ref",
        "call_media_provider_events.provider_adapter",
        "call_media_provider_events.provider_event_id",
    ];
    for column in PRIVATE_PROVIDER_EVIDENCE {
        assert!(columns.contains(&column.to_string()), "{column}");
    }
    // Part E: a public party is an OPAQUE ContactReference (a UUID issued by Privacy
    // Relay) — never a contact value. Only these columns may carry one, typed uuid.
    const OPAQUE_CONTACT_REFERENCES: &[&str] = &[
        "call_public_parties.contact_reference",
        "call_spend_reservations.contact_reference",
    ];
    for column in OPAQUE_CONTACT_REFERENCES {
        let (table, name) = column.split_once('.').unwrap();
        let types = strings(
            &db.conn,
            &format!(
                "SELECT data_type FROM information_schema.columns WHERE table_schema = 'public' \
                 AND table_name = '{table}' AND column_name = '{name}'"
            ),
        )
        .await;
        assert_eq!(types, vec!["uuid".to_string()], "{column}");
    }
    for column in &columns {
        let name = column.split('.').nth(1).unwrap();
        if PRIVATE_PROVIDER_EVIDENCE.contains(&column.as_str())
            || OPAQUE_CONTACT_REFERENCES.contains(&column.as_str())
        {
            continue;
        }
        for forbidden in [
            "secret",
            "plaintext",
            "raw",
            "password",
            "token",
            "api_key",
            "phone",
            "e164",
            "msisdn",
            "email",
            "contact",
            "telephone",
            "caller",
            "callee",
            "dialed",
            "provider",
            "carrier",
            "vendor",
            "sip",
            "pstn",
            "twilio",
            "livekit",
            "room",
            "dialog",
            "sid",
            "payload_raw",
        ] {
            assert!(
                !name.contains(forbidden),
                "column {column} looks like contact / secret / provider storage"
            );
        }
        if name.contains("number") {
            assert!(ORDINALS.contains(&column.as_str()), "column {column}");
        }
    }
    let ordinal_types = strings(
        &db.conn,
        "SELECT data_type::text FROM information_schema.columns \
         WHERE column_name IN ('interval_number', 'attempt_number') AND table_name LIKE 'call\\_%'",
    )
    .await;
    assert_eq!(ordinal_types, ["integer", "integer"]);
    // Opaque cross-service references are UUIDs (never free text).
    let references = strings(
        &db.conn,
        "SELECT table_name || '.' || column_name || ':' || data_type FROM information_schema.columns \
         WHERE column_name IN ('communication_context_ref', 'privacy_relay_ref', 'execution_binding_ref') \
         ORDER BY 1",
    )
    .await;
    assert_eq!(
        references,
        [
            "call_legs.execution_binding_ref:uuid",
            "call_privacy_authorizations.communication_context_ref:uuid",
            "call_privacy_authorizations.privacy_relay_ref:uuid",
            "call_privacy_enforcements.communication_context_ref:uuid",
            "calls.communication_context_ref:uuid",
            "calls.privacy_relay_ref:uuid",
        ]
    );
}

#[tokio::test]
async fn no_privacy_relay_telephony_or_meeting_service_tables_exist() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let all = tables(&db.conn).await;
    for forbidden in [
        // Privacy Relay (its own service and database), Contact Vault / Numbering.
        "communication_contexts",
        "privacy_relay_sessions",
        "relay_sessions",
        "protected_contacts",
        "relay_number_inventory",
        "contact_vault",
        // Telephony execution (other service).
        "provider_call_bindings",
        "provider_events",
        "telephony_commands",
        "telephony_routes",
        "carrier_events",
        "sip_dialogs",
        // Meeting Service.
        "users",
        "organizations",
        "organization_memberships",
        "meetings",
        "api_keys",
        "participants",
        "outbox_events",
    ] {
        assert!(
            !all.iter().any(|t| t == forbidden),
            "unexpected table {forbidden}"
        );
    }
    assert!(all.iter().all(|t| t.starts_with("platform_")
        || t.starts_with("call_")
        || t == "calls"
        || t == "calling_outbox_events"
        || t == "calling_incidents"
        || t.starts_with("enterprise_")
        || t.starts_with("voicemail_")
        || t.starts_with("webhook_")
        || t == MIGRATION_TABLE));
    // No second Call authority (Part F, ADR 0018).
    for duplicate in [
        "enterprise_calls",
        "pbx_calls",
        "voicemail_calls",
        "ivr_calls",
    ] {
        assert!(!all.iter().any(|t| t == duplicate), "{duplicate}");
    }
}

/// Part F tables hold no raw telephone number, provider / SIP / edge identifier, DTMF
/// digits, secret or public URL (§4–§5, §107, §123, §151–§154). Opaque cross-service
/// references are UUIDs and are listed explicitly.
#[tokio::test]
async fn enterprise_columns_hold_no_contact_secret_dtmf_or_provider_data() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let columns = strings(
        &db.conn,
        "SELECT table_name || '.' || column_name FROM information_schema.columns \
         WHERE table_schema = 'public' AND (table_name LIKE 'enterprise\\_%' OR table_name LIKE 'voicemail\\_%' \
         OR table_name LIKE 'call\\_recording%')",
    )
    .await;
    assert!(!columns.is_empty());
    // Opaque references to records other services own (never contact values).
    const OPAQUE_REFERENCES: &[&str] = &["enterprise_public_contacts.contact_reference"];
    for column in &columns {
        if OPAQUE_REFERENCES.contains(&column.as_str()) {
            continue;
        }
        let name = column.split('.').nth(1).unwrap();
        for forbidden in [
            "phone",
            "e164",
            "msisdn",
            "telephone",
            "number_raw",
            "raw",
            "dialed",
            "caller_id",
            "sip",
            "pstn",
            "provider",
            "carrier",
            "channel",
            "asterisk",
            "dialog",
            "secret",
            "password",
            "token",
            "url",
            "digit",
            "dtmf",
            "contact",
        ] {
            assert!(
                !name.contains(forbidden),
                "column {column} looks like contact / provider / secret / DTMF storage"
            );
        }
    }
    for column in OPAQUE_REFERENCES {
        let (table, name) = column.split_once('.').unwrap();
        let types = strings(
            &db.conn,
            &format!(
                "SELECT data_type FROM information_schema.columns WHERE table_schema = 'public' \
                 AND table_name = '{table}' AND column_name = '{name}'"
            ),
        )
        .await;
        assert!(types.is_empty() || types == ["uuid"], "{column}: {types:?}");
    }
}
