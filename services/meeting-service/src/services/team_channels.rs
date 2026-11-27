//! Team chats follow their team (the team chat a manager created with
//! `PUT /api/v2/organizations/{id}/teams/{teamId}/messaging-channel`).
//!
//! * When the chat is created or re-synced by a manager, it is remembered here with that
//!   manager (`organization_team_channels.managed_by`).
//! * Organizations delivers `team.member_added`, `team.member_removed` and
//!   `team.archived` to the Meeting Service; for a remembered chat the event makes it due
//!   ([`mark_due`], in the event's transaction).
//! * The worker ([`run_once`]) re-runs the same sync as the remembered manager:
//!   Organizations still decides `organization.teams.manage` and the team's members for
//!   that person on every run. An archived team's chat loses its team members (the chat
//!   and its history stay). A manager who has lost the right, or a team Organizations no
//!   longer shows them, stops the chat following (`NEEDS_ADMIN`, logged) until a manager
//!   presses "Team chat" again, which re-binds it to them. Outages retry with backoff.

use std::time::Duration;

use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use uuid::Uuid;

use crate::{
    error::{ApiResult, ErrorCode},
    services::{
        organization_calling::OrganizationCalling,
        organizations_authority::{ActingUser, OrganizationsAuthority},
    },
};

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Remember (or re-bind to `manager`) a team chat that was just synced.
pub async fn remember<C: ConnectionTrait>(
    db: &C,
    organization: Uuid,
    team: Uuid,
    manager: Uuid,
    title: Option<&str>,
) -> ApiResult<()> {
    db.execute(stmt(
        "INSERT INTO organization_team_channels (organization_id, team_id, managed_by, title, state, sync_due, archive_due, attempts, last_error, last_synced_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'ACTIVE', false, false, 0, NULL, now(), now()) \
         ON CONFLICT (organization_id, team_id) DO UPDATE SET managed_by = EXCLUDED.managed_by, \
           title = COALESCE(EXCLUDED.title, organization_team_channels.title), state = 'ACTIVE', \
           sync_due = false, archive_due = false, attempts = 0, last_error = NULL, \
           last_synced_at = now(), updated_at = now()",
        vec![
            organization.into(),
            team.into(),
            manager.into(),
            title.map(|t| t.chars().take(200).collect::<String>()).into(),
        ],
    ))
    .await?;
    Ok(())
}

/// A team event arrived: a remembered, following chat of that team becomes due.
/// Returns whether such a chat exists.
pub async fn mark_due<C: ConnectionTrait>(
    db: &C,
    organization: Uuid,
    team: Uuid,
    archived: bool,
) -> Result<bool, sea_orm::DbErr> {
    let r = db
        .execute(stmt(
            "UPDATE organization_team_channels SET sync_due = true, archive_due = archive_due OR $3, \
               attempts = 0, next_attempt_at = now(), updated_at = now() \
             WHERE organization_id = $1 AND team_id = $2 AND state = 'ACTIVE'",
            vec![organization.into(), team.into(), archived.into()],
        ))
        .await?;
    Ok(r.rows_affected() > 0)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TeamChannelPass {
    pub synced: u32,
    pub retried: u32,
    pub stopped: u32,
}

/// One pass over due chats (at most `limit`).
pub async fn run_once(
    db: &DatabaseConnection,
    relay: &OrganizationCalling,
    authority: &OrganizationsAuthority,
    limit: i64,
) -> ApiResult<TeamChannelPass> {
    let due = db
        .query_all(stmt(
            "SELECT organization_id, team_id, managed_by, title, archive_due, attempts \
             FROM organization_team_channels \
             WHERE sync_due AND state = 'ACTIVE' AND next_attempt_at <= now() \
             ORDER BY next_attempt_at LIMIT $1",
            vec![limit.into()],
        ))
        .await?;
    let mut pass = TeamChannelPass::default();
    for row in due {
        let organization: Uuid = row.try_get("", "organization_id")?;
        let team: Uuid = row.try_get("", "team_id")?;
        let manager: Uuid = row.try_get("", "managed_by")?;
        let title: Option<String> = row.try_get("", "title")?;
        let archived: bool = row.try_get("", "archive_due")?;
        let attempts: i32 = row.try_get("", "attempts")?;
        let started = Utc::now();
        let result = relay
            .team_channel_sync(
                db,
                authority,
                &ActingUser::for_user(manager),
                organization,
                team,
                title,
                archived,
            )
            .await;
        match result {
            Ok((status, _)) if (200..300).contains(&status) => {
                // Settled only if no newer event arrived while syncing.
                db.execute(stmt(
                    "UPDATE organization_team_channels SET \
                       sync_due = updated_at > $3, \
                       state = CASE WHEN archive_due THEN 'ARCHIVED' ELSE state END, \
                       attempts = 0, last_error = NULL, last_synced_at = now() \
                     WHERE organization_id = $1 AND team_id = $2",
                    vec![organization.into(), team.into(), started.into()],
                ))
                .await?;
                pass.synced += 1;
            }
            Ok((status, _)) => {
                stop(db, organization, team, &format!("MESSAGING_{status}")).await?;
                pass.stopped += 1;
            }
            Err(e) if e.code() == Some(ErrorCode::ServiceUnavailable) => {
                let backoff = 30i64 * 2i64.pow(attempts.clamp(0, 6) as u32);
                db.execute(stmt(
                    "UPDATE organization_team_channels SET attempts = attempts + 1, \
                       next_attempt_at = now() + make_interval(secs => $3), last_error = 'UNAVAILABLE' \
                     WHERE organization_id = $1 AND team_id = $2",
                    vec![organization.into(), team.into(), (backoff as f64).into()],
                ))
                .await?;
                pass.retried += 1;
            }
            Err(e) => {
                let class = e
                    .code()
                    .map(|c| format!("{c:?}").to_ascii_uppercase())
                    .unwrap_or_else(|| "REFUSED".into());
                stop(db, organization, team, &class).await?;
                pass.stopped += 1;
            }
        }
    }
    Ok(pass)
}

/// The chat stops following its team until a manager re-binds it.
async fn stop(
    db: &DatabaseConnection,
    organization: Uuid,
    team: Uuid,
    class: &str,
) -> ApiResult<()> {
    tracing::warn!(
        organization_id = %organization,
        team_id = %team,
        reason = class,
        "team chat stopped following its team; a manager must press \"Team chat\" again"
    );
    db.execute(stmt(
        "UPDATE organization_team_channels SET state = 'NEEDS_ADMIN', sync_due = false, \
           last_error = $3, updated_at = now() WHERE organization_id = $1 AND team_id = $2",
        vec![
            organization.into(),
            team.into(),
            class.chars().take(64).collect::<String>().into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Runs [`run_once`] every `interval` (forever; the process owns the task).
pub fn spawn(
    db: DatabaseConnection,
    relay: std::sync::Arc<OrganizationCalling>,
    authority: std::sync::Arc<OrganizationsAuthority>,
    interval: Duration,
) {
    tokio::spawn(async move {
        loop {
            match run_once(&db, &relay, &authority, 20).await {
                Ok(p) if p != TeamChannelPass::default() => {
                    tracing::info!(
                        synced = p.synced,
                        retried = p.retried,
                        stopped = p.stopped,
                        "team chat pass"
                    )
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("team chat pass failed: {e}"),
            }
            tokio::time::sleep(interval).await;
        }
    });
}
