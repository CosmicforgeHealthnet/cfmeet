//! Retention of the relay-number boundary's rows (§80): the inventory ledger
//! (`relay_number_events`: statuses, generations, actors — never a number) ages out
//! after its bound, and the assignments / ledger rows of a purged context go with it.
//! Called by the retention worker inside its flagged transaction only.

use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use uuid::Uuid;

use crate::{db, error::RelayResult, retention::DataClass};

/// Deletes up to `limit` ledger rows that occurred at or before `cutoff`, skipping rows of
/// a relay number an active hold covers. Returns the number of deleted rows.
pub async fn purge_number_events<C: ConnectionTrait>(
    conn: &C,
    cutoff: DateTime<Utc>,
    limit: u64,
) -> RelayResult<u64> {
    db::exec(
        conn,
        "DELETE FROM relay_number_events WHERE id IN ( \
           SELECT e.id FROM relay_number_events e \
           WHERE e.occurred_at <= $1 \
             AND NOT privacy_relay_retention_held($2, e.relay_number_id, NULL, NULL, NULL) \
             AND (e.assignment_id IS NULL OR NOT privacy_relay_retention_held($2, e.assignment_id, NULL, NULL, NULL)) \
           ORDER BY e.occurred_at, e.id LIMIT $3 FOR UPDATE SKIP LOCKED)",
        vec![
            cutoff.into(),
            DataClass::RelayNumberEvents.as_str().into(),
            (limit as i64).into(),
        ],
    )
    .await
}

/// Deletes the ledger rows and the (ended) assignments of the given contexts, before
/// the sessions they reference are deleted. Returns the number of deleted assignments.
pub async fn purge_assignments_of_contexts<C: ConnectionTrait>(
    conn: &C,
    contexts: &[Uuid],
) -> RelayResult<u64> {
    db::exec(
        conn,
        "DELETE FROM relay_number_events WHERE assignment_id IN ( \
           SELECT id FROM relay_number_assignments WHERE context_id = ANY($1))",
        vec![contexts.to_vec().into()],
    )
    .await?;
    db::exec(
        conn,
        "DELETE FROM relay_number_assignments WHERE context_id = ANY($1) AND status NOT IN ('ACTIVE', 'COOLDOWN')",
        vec![contexts.to_vec().into()],
    )
    .await
}
