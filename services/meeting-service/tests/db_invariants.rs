//! Database-level invariants (spec §B14): constraints, FKs, unique indexes and triggers
//! hold even if application code were bypassed.

mod common;

use common::*;
use migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use uuid::Uuid;

async fn ok(c: &DatabaseConnection, sql: &str) {
    c.execute_unprepared(sql)
        .await
        .unwrap_or_else(|e| panic!("expected success: {e}\n{sql}"));
}

async fn fails(c: &DatabaseConnection, sql: &str) {
    assert!(
        c.execute_unprepared(sql).await.is_err(),
        "expected constraint violation:\n{sql}"
    );
}

struct Ids {
    user: Uuid,
    space: Uuid,
    conf: Uuid,
    conf2: Uuid,
}

async fn base(c: &DatabaseConnection) -> Ids {
    let ids = Ids {
        user: Uuid::new_v4(),
        space: Uuid::new_v4(),
        conf: Uuid::new_v4(),
        conf2: Uuid::new_v4(),
    };
    ok(c, &format!("INSERT INTO users (id, username, email, status) VALUES ('{}', 'u{}', 'u@x.io', 'active')", ids.user, &ids.user.simple().to_string()[..8])).await;
    ok(c, &format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title) VALUES ('{}', '{}', 'aaa-bbbb-ccc', 't')", ids.space, ids.user)).await;
    for (id, key) in [(ids.conf, "k1"), (ids.conf2, "k2")] {
        ok(c, &format!("INSERT INTO conference_sessions (id, meeting_space_id, occurrence_key, kind) VALUES ('{id}', '{}', '{key}', 'SCHEDULED')", ids.space)).await;
    }
    ids
}

macro_rules! db {
    () => {
        match TestDb::new().await {
            Some(d) => d,
            None => return,
        }
    };
}

#[tokio::test]
async fn i01_space_identity_constraints() {
    let db = db!();
    let c = &db.conn;
    let ids = base(c).await;
    fails(c, &format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title) VALUES (gen_random_uuid(), '{}', 'aaa-bbbb-ccc', 'dup')", ids.user)).await;
    ok(c, &format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title) VALUES (gen_random_uuid(), '{}', 'zzz-yyyy-xxx', 'null legacy ok')", ids.user)).await;
    let legacy = Uuid::new_v4();
    ok(c, &format!("INSERT INTO meetings (id, meeting_identifier, host_id, title, start_time) VALUES ('{legacy}', 'LGCY2345', '{}', 'l', now())", ids.user)).await;
    ok(c, &format!("INSERT INTO meeting_spaces (id, legacy_meeting_id, owner_user_id, meeting_code, title, origin) VALUES (gen_random_uuid(), '{legacy}', '{}', 'LGCY2345', 'l', 'LEGACY')", ids.user)).await;
    fails(c, &format!("INSERT INTO meeting_spaces (id, legacy_meeting_id, owner_user_id, meeting_code, title) VALUES (gen_random_uuid(), '{legacy}', '{}', 'qqq-wwww-eee', 'dup legacy')", ids.user)).await;
    fails(c, &format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title, access_policy) VALUES (gen_random_uuid(), '{}', 'ppp-pppp-ppp', 'no hash', 'PUBLIC_PASSCODE')", ids.user)).await;
    fails(c, &format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title, default_role) VALUES (gen_random_uuid(), '{}', 'hhh-hhhh-hhh', 'x', 'HOST')", ids.user)).await;
    fails(c, &format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title, access_policy) VALUES (gen_random_uuid(), '{}', 'bad-bbbb-bad', 'x', 'EVERYBODY')", ids.user)).await;
    fails(c, &format!("INSERT INTO meeting_spaces (id, owner_user_id, meeting_code, title) VALUES (gen_random_uuid(), '{}', 'fk1-xxxx-xxx', 'x')", Uuid::new_v4())).await;
}

#[tokio::test]
async fn i02_conference_constraints() {
    let db = db!();
    let c = &db.conn;
    let ids = base(c).await;
    fails(c, &format!("INSERT INTO conference_sessions (id, meeting_space_id, occurrence_key, kind) VALUES (gen_random_uuid(), '{}', 'k1', 'SCHEDULED')", ids.space)).await;
    fails(c, &format!("INSERT INTO conference_sessions (id, meeting_space_id, occurrence_key, kind) VALUES (gen_random_uuid(), '{}', 'k9', 'SCHEDULED')", Uuid::new_v4())).await;
    ok(
        c,
        &format!(
            "UPDATE conference_sessions SET status='LIVE', started_at=now() WHERE id='{}'",
            ids.conf
        ),
    )
    .await;
    fails(
        c,
        &format!(
            "UPDATE conference_sessions SET status='LIVE', started_at=now() WHERE id='{}'",
            ids.conf2
        ),
    )
    .await;
    fails(
        c,
        &format!(
            "UPDATE conference_sessions SET status='FAILED' WHERE id='{}'",
            ids.conf2
        ),
    )
    .await;
    fails(c, &format!("UPDATE conference_sessions SET scheduled_start_at=now(), scheduled_end_at=now() - interval '1 hour' WHERE id='{}'", ids.conf2)).await;
    fails(
        c,
        &format!(
            "UPDATE conference_sessions SET is_locked=true WHERE id='{}'",
            ids.conf
        ),
    )
    .await;
}

#[tokio::test]
async fn i03_membership_and_invitation_constraints() {
    let db = db!();
    let c = &db.conn;
    let ids = base(c).await;
    let member = Uuid::new_v4();
    ok(
        c,
        &format!(
            "INSERT INTO users (id, username, email) VALUES ('{member}', 'm{}', 'm@x.io')",
            &member.simple().to_string()[..8]
        ),
    )
    .await;
    let ins = |status: &str| {
        format!(
        "INSERT INTO meeting_members (id, meeting_space_id, principal_type, principal_ref, user_id, role, status, revoked_at) \
         VALUES (gen_random_uuid(), '{}', 'USER', '{member}', '{member}', 'PARTICIPANT', '{status}', {})",
        ids.space,
        if status == "REVOKED" { "now()" } else { "NULL" }
    )
    };
    ok(c, &ins("ACTIVE")).await;
    fails(c, &ins("ACTIVE")).await;
    ok(c, &ins("REVOKED")).await;
    fails(c, &format!("INSERT INTO meeting_members (id, meeting_space_id, principal_type, principal_ref, role) VALUES (gen_random_uuid(), '{}', 'USER', 'x', 'PARTICIPANT')", ids.space)).await;
    fails(c, &format!("INSERT INTO meeting_members (id, meeting_space_id, principal_type, principal_ref, user_id, role) VALUES (gen_random_uuid(), '{}', 'USER', '{member}', '{member}', 'OWNER')", ids.space)).await;
    let inv = Uuid::new_v4();
    ok(c, &format!("INSERT INTO meeting_invitations (id, meeting_space_id, inviter_user_id, token_hash, expires_at, use_limit) VALUES ('{inv}', '{}', '{}', 'h1', now() + interval '1 day', 1)", ids.space, ids.user)).await;
    ok(
        c,
        &format!("UPDATE meeting_invitations SET usage_count = 1 WHERE id='{inv}'"),
    )
    .await;
    fails(
        c,
        &format!("UPDATE meeting_invitations SET usage_count = 2 WHERE id='{inv}'"),
    )
    .await;
    fails(c, &format!("INSERT INTO meeting_invitations (id, meeting_space_id, inviter_user_id, token_hash, expires_at) VALUES (gen_random_uuid(), '{}', '{}', 'h1', now() + interval '1 day')", ids.space, ids.user)).await;
    fails(c, &format!("INSERT INTO meeting_invitations (id, meeting_space_id, inviter_user_id, token_hash, expires_at) VALUES (gen_random_uuid(), '{}', '{}', 'h2', now() - interval '1 day')", ids.space, ids.user)).await;
    fails(
        c,
        &format!("UPDATE meeting_invitations SET status='REVOKED' WHERE id='{inv}'"),
    )
    .await;
}

#[tokio::test]
async fn i04_participant_and_session_scoping() {
    let db = db!();
    let c = &db.conn;
    let ids = base(c).await;
    let p = Uuid::new_v4();
    let ins_p = |id: Uuid, conf: Uuid, extra: &str, vals: &str| {
        format!(
        "INSERT INTO conference_participants (id, conference_id, identity_type, display_name, role, role_source, admission_state, admitted_at{extra}) \
         VALUES ('{id}', '{conf}', {vals})"
    )
    };
    ok(
        c,
        &ins_p(
            p,
            ids.conf,
            ", user_id",
            &format!(
                "'REGISTERED_USER', 'u', 'PARTICIPANT', 'DEFAULT', 'ADMITTED', now(), '{}'",
                ids.user
            ),
        ),
    )
    .await;
    fails(
        c,
        &ins_p(
            Uuid::new_v4(),
            ids.conf,
            ", user_id",
            &format!(
                "'REGISTERED_USER', 'u2', 'PARTICIPANT', 'DEFAULT', 'ADMITTED', now(), '{}'",
                ids.user
            ),
        ),
    )
    .await;
    ok(
        c,
        &ins_p(
            Uuid::new_v4(),
            ids.conf2,
            ", user_id",
            &format!(
                "'REGISTERED_USER', 'u', 'PARTICIPANT', 'DEFAULT', 'ADMITTED', now(), '{}'",
                ids.user
            ),
        ),
    )
    .await;
    fails(
        c,
        &ins_p(
            Uuid::new_v4(),
            ids.conf,
            "",
            "'REGISTERED_USER', 'nouser', 'PARTICIPANT', 'DEFAULT', 'WAITING', NULL",
        ),
    )
    .await;
    fails(
        c,
        &ins_p(
            Uuid::new_v4(),
            ids.conf,
            "",
            "'GUEST', 'noid', 'PARTICIPANT', 'DEFAULT', 'WAITING', NULL",
        ),
    )
    .await;
    fails(
        c,
        &ins_p(
            Uuid::new_v4(),
            ids.conf,
            ", guest_identity",
            "'GUEST', 'g', 'PARTICIPANT', 'DEFAULT', 'ADMITTED', NULL, 'gst_1'",
        ),
    )
    .await;
    ok(
        c,
        &ins_p(
            Uuid::new_v4(),
            ids.conf,
            ", guest_identity",
            "'GUEST', 'g', 'PARTICIPANT', 'DEFAULT', 'WAITING', NULL, 'gst_1'",
        ),
    )
    .await;
    fails(
        c,
        &ins_p(
            Uuid::new_v4(),
            ids.conf,
            ", guest_identity",
            "'GUEST', 'g', 'PARTICIPANT', 'DEFAULT', 'WAITING', NULL, 'gst_1'",
        ),
    )
    .await;

    let s = |id: Uuid, part: Uuid, conf: Uuid, media: &str| {
        format!(
        "INSERT INTO participant_sessions (id, conference_participant_id, conference_id, media_identity, state, established_at) \
         VALUES ('{id}', '{part}', '{conf}', '{media}', 'PENDING', now())"
    )
    };
    let s1 = Uuid::new_v4();
    ok(c, &s(s1, p, ids.conf, "m1")).await;
    ok(c, &s(Uuid::new_v4(), p, ids.conf, "m2")).await;
    fails(c, &s(Uuid::new_v4(), p, ids.conf, "m1")).await; // media identity unique in conference
    fails(c, &s(Uuid::new_v4(), p, ids.conf2, "m3")).await; // session cannot move conference
    fails(c, &s(Uuid::new_v4(), Uuid::new_v4(), ids.conf, "m4")).await;
    // Leaving ADMITTED terminates active sessions in the same statement.
    ok(c, &format!("UPDATE conference_participants SET admission_state='BANNED', banned_at=now() WHERE id='{p}'")).await;
    let active: i64 = c
        .query_one(sea_orm::Statement::from_string(sea_orm::DbBackend::Postgres, format!("SELECT count(*) FROM participant_sessions WHERE conference_participant_id='{p}' AND state IN ('PENDING','CONNECTED')")))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    assert_eq!(active, 0);
    fails(c, &s(Uuid::new_v4(), p, ids.conf, "m5")).await; // banned → no new active session
    fails(
        c,
        &format!("UPDATE participant_sessions SET state='CONNECTED' WHERE id='{s1}'"),
    )
    .await;
    fails(c, &format!("UPDATE conference_participants SET admission_state='ADMITTED', admitted_at=NULL WHERE id='{p}'")).await;
}

#[tokio::test]
async fn i05_audit_is_append_only_and_outbox_shape() {
    let db = db!();
    let c = &db.conn;
    let ids = base(c).await;
    let a = Uuid::new_v4();
    ok(c, &format!("INSERT INTO conference_audit_events (id, meeting_space_id, actor_type, action) VALUES ('{a}', '{}', 'SYSTEM', 'TEST')", ids.space)).await;
    fails(
        c,
        &format!("UPDATE conference_audit_events SET action='TAMPER' WHERE id='{a}'"),
    )
    .await;
    fails(
        c,
        &format!("DELETE FROM conference_audit_events WHERE id='{a}'"),
    )
    .await;
    fails(c, &format!("INSERT INTO conference_audit_events (id, meeting_space_id, actor_type, action) VALUES (gen_random_uuid(), '{}', 'SYSTEM', 'X')", Uuid::new_v4())).await;
    fails(c, "INSERT INTO outbox_events (id, aggregate_type, aggregate_id, event_type, status) VALUES (gen_random_uuid(), 'x', gen_random_uuid(), 'e', 'LOST')").await;
}

#[tokio::test]
async fn i06_bans_and_foundations() {
    let db = db!();
    let c = &db.conn;
    let ids = base(c).await;
    fails(c, &format!("INSERT INTO conference_bans (id, meeting_space_id, scope, subject_type, subject_ref, created_by) VALUES (gen_random_uuid(), '{}', 'CONFERENCE', 'USER', 'x', 'y')", ids.space)).await;
    ok(c, &format!("INSERT INTO conference_bans (id, meeting_space_id, scope, subject_type, subject_ref, created_by) VALUES (gen_random_uuid(), '{}', 'SPACE', 'USER', 'x', 'y')", ids.space)).await;
    // Collaboration foundations enforce conference scoping through composite FKs.
    let p = Uuid::new_v4();
    ok(c, &format!("INSERT INTO conference_participants (id, conference_id, identity_type, guest_identity, display_name, role, role_source, admission_state) VALUES ('{p}', '{}', 'GUEST', 'gst_x', 'g', 'PARTICIPANT', 'DEFAULT', 'WAITING')", ids.conf)).await;
    let room = Uuid::new_v4();
    ok(
        c,
        &format!(
            "INSERT INTO breakout_rooms (id, conference_id, name) VALUES ('{room}', '{}', 'r')",
            ids.conf2
        ),
    )
    .await;
    fails(c, &format!("INSERT INTO breakout_assignments (id, breakout_room_id, conference_id, participant_id) VALUES (gen_random_uuid(), '{room}', '{}', '{p}')", ids.conf2)).await;
    fails(c, &format!("INSERT INTO meeting_consents (id, conference_id, participant_id, consent_type, consent_version, decision) VALUES (gen_random_uuid(), '{}', '{p}', 'RECORDING', 'v1', 'GRANTED')", ids.conf2)).await;
    ok(c, &format!("INSERT INTO meeting_consents (id, conference_id, participant_id, consent_type, consent_version, decision) VALUES (gen_random_uuid(), '{}', '{p}', 'RECORDING', 'v1', 'GRANTED')", ids.conf)).await;
}

/// Migrations are idempotent: re-running `up` on a current schema applies nothing.
#[tokio::test]
async fn i07_migrations_idempotent() {
    let db = db!();
    Migrator::up(&db.conn, None).await.unwrap();
    let pending = Migrator::get_pending_migrations(&db.conn).await.unwrap();
    assert!(pending.is_empty());
    let applied = Migrator::get_applied_migrations(&db.conn).await.unwrap();
    // 18 through Parts A–D + 1 additive Part E migration (rtc control plane)
    // + 1 additive Part F migration (collaboration)
    // + 1 additive Parts G–I migration (artefacts, profiles, operations)
    // + 1 additive Part M index (main-room message cursor)
    // + 1 additive Part V H migration (Unified Messaging integration state)
    // + 1 additive remediation migration (breakout / private chat in Unified Messaging)
    // + 1 additive Part VI migration (environment-owned meetings, public-event channel).
    assert_eq!(applied.len(), 25);
}
