//! Local block refusals. Producers acknowledge a mutation only after this transaction
//! commits; versions preserve tombstones when delivery is repeated or reordered.
use sea_orm::ConnectionTrait;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    db,
    error::{Error, ErrorCode, Result},
    identity::OwnerScope,
};

/// Per-principal locks include application-wide Calling rules. A block's exclusive
/// lock waits for earlier sends; a later send sees the committed local refusal.
pub(super) async fn lock<C: ConnectionTrait>(
    db: &C,
    scope: OwnerScope,
    principals: &[Uuid],
    shared: bool,
) -> Result<()> {
    let mut ids = principals.to_vec();
    ids.sort();
    ids.dedup();
    for id in ids {
        let key = format!(
            "messaging-send-policy:{}:{}:{}:{id}",
            scope.tenant_id, scope.application_id, scope.environment_id
        );
        db::exec(
            db,
            if shared {
                "SELECT pg_advisory_xact_lock_shared(hashtextextended($1, 882))"
            } else {
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 882))"
            },
            vec![key.into()],
        )
        .await?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DomainRule {
    pub rule_id: Uuid,
    pub projection_generation: Uuid,
    pub actor_principal_id: Option<Uuid>,
    pub target_principal_id: Uuid,
    pub blocked: bool,
}

pub(super) async fn apply<C: ConnectionTrait>(
    db: &C,
    scope: OwnerScope,
    generation: Uuid,
    kind: &str,
    version: i64,
    data: &Value,
) -> Result<()> {
    if data["projectionGeneration"]
        .as_str()
        .and_then(|v| Uuid::parse_str(v).ok())
        != Some(generation)
    {
        return Ok(());
    }
    let (source, rule, actor, target, active) = match kind {
        "communications.block.changed" => {
            let parse = |field: &str| {
                data[field]
                    .as_str()
                    .and_then(|v| Uuid::parse_str(v).ok())
                    .ok_or_else(|| Error::validation("Invalid block projection"))
            };
            let actor = parse("actorPrincipalId")?;
            (
                "COMMUNICATIONS",
                actor,
                Some(actor),
                parse("targetPrincipalId")?,
                data["blocked"]
                    .as_bool()
                    .ok_or_else(|| Error::validation("Invalid block projection"))?,
            )
        }
        "communications.domain-block.changed" => {
            let r: DomainRule = serde_json::from_value(data.clone())
                .map_err(|_| Error::validation("Invalid block projection"))?;
            if r.projection_generation != generation {
                return Err(Error::validation("Invalid projection generation"));
            }
            (
                "CALLING",
                r.rule_id,
                r.actor_principal_id,
                r.target_principal_id,
                r.blocked,
            )
        }
        _ => return Ok(()),
    };
    if version < 1 || actor == Some(target) {
        return Err(Error::validation("Invalid block projection"));
    }
    let mut principals = vec![target];
    principals.extend(actor);
    lock(db, scope, &principals, false).await?;
    let changed = db::exec(db, "INSERT INTO messaging_send_block_projection(tenant_id,application_id,environment_id,generation,source,rule_id,actor_principal_id,target_principal_id,source_version,active) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(tenant_id,application_id,environment_id,generation,source,rule_id,target_principal_id) DO UPDATE SET source_version=EXCLUDED.source_version,active=EXCLUDED.active,updated_at=now() WHERE messaging_send_block_projection.source_version<EXCLUDED.source_version AND messaging_send_block_projection.actor_principal_id IS NOT DISTINCT FROM EXCLUDED.actor_principal_id", vec![scope.tenant_id.into(),scope.application_id.into(),scope.environment_id.into(),generation.into(),source.into(),rule.into(),actor.into(),target.into(),version.into(),active.into()]).await?;
    if changed == 0 {
        #[derive(sea_orm::FromQueryResult)]
        struct Existing {
            source_version: i64,
            active: bool,
            actor_principal_id: Option<Uuid>,
        }
        let existing: Existing = db::one(db, "SELECT source_version,active,actor_principal_id FROM messaging_send_block_projection WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND generation=$4 AND source=$5 AND rule_id=$6 AND target_principal_id=$7", vec![scope.tenant_id.into(),scope.application_id.into(),scope.environment_id.into(),generation.into(),source.into(),rule.into(),target.into()]).await?.ok_or_else(|| Error::code(ErrorCode::ServiceUnavailable))?;
        if existing.actor_principal_id != actor
            || (existing.source_version == version && existing.active != active)
        {
            return Err(Error::validation("Block source version conflict"));
        }
    }

    Ok(())
}

pub(super) async fn require_unblocked<C: ConnectionTrait>(
    db: &C,
    scope: OwnerScope,
    generation: Uuid,
    actor: Uuid,
    target: Uuid,
) -> Result<()> {
    lock(db, scope, &[actor, target], true).await?;
    let n = db::scalar_i64(db, "SELECT count(*) FROM messaging_send_block_projection WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND generation=$4 AND active AND ((actor_principal_id=$5 AND target_principal_id=$6) OR (actor_principal_id=$6 AND target_principal_id=$5) OR (actor_principal_id IS NULL AND target_principal_id IN ($5,$6)))", vec![scope.tenant_id.into(),scope.application_id.into(),scope.environment_id.into(),generation.into(),actor.into(),target.into()]).await?;
    if n > 0 {
        return Err(Error::code(ErrorCode::DeliveryRefused));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use messaging_migration::MigratorTrait;
    use sea_orm::{Database, DatabaseConnection, TransactionTrait};
    use serde_json::json;
    use std::time::Duration;

    struct Fixture {
        db: DatabaseConnection,
        admin: DatabaseConnection,
        name: String,
        scope: OwnerScope,
        generation: Uuid,
    }
    impl Fixture {
        async fn new() -> Option<Self> {
            let base = match std::env::var("TEST_DATABASE_URL") {
                Ok(value) => value,
                Err(_) if std::env::var("CFMEET_SKIP_DB_TESTS").ok().as_deref() == Some("1") => {
                    eprintln!("SKIPPED: projection database tests");
                    return None;
                }
                Err(_) => panic!("TEST_DATABASE_URL is required for projection tests"),
            };
            let admin = Database::connect(&base).await.unwrap();
            let name = format!(
                "msg_proj_{}_{}",
                std::process::id(),
                Uuid::new_v4().simple()
            );
            admin
                .execute_unprepared(&format!("CREATE DATABASE {name}"))
                .await
                .unwrap();
            let (prefix, _) = base.rsplit_once('/').unwrap();
            let db = Database::connect(format!("{prefix}/{name}")).await.unwrap();
            messaging_migration::SendPolicyProjectionMigrator::up(&db, None)
                .await
                .unwrap();
            Some(Self {
                db,
                admin,
                name,
                generation: Uuid::new_v4(),
                scope: OwnerScope {
                    tenant_id: Uuid::new_v4(),
                    application_id: Uuid::new_v4(),
                    environment_id: Uuid::new_v4(),
                    environment_kind: crate::identity::EnvironmentKind::Test,
                    service_account_id: Uuid::new_v4(),
                },
            })
        }
        async fn close(self) {
            self.db.close().await.unwrap();
            self.admin
                .execute_unprepared(&format!("DROP DATABASE {}", self.name))
                .await
                .unwrap();
            self.admin.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn reordered_updates_keep_tombstones_and_generations_separate() {
        let Some(f) = Fixture::new().await else {
            return;
        };
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut data = json!({"actorPrincipalId":a,"targetPrincipalId":b,"blocked":false,"projectionGeneration":f.generation});
        apply(
            &f.db,
            f.scope,
            f.generation,
            "communications.block.changed",
            2,
            &data,
        )
        .await
        .unwrap();
        data["blocked"] = json!(true);
        apply(
            &f.db,
            f.scope,
            f.generation,
            "communications.block.changed",
            1,
            &data,
        )
        .await
        .unwrap();
        require_unblocked(&f.db, f.scope, f.generation, a, b)
            .await
            .unwrap();
        assert!(
            apply(
                &f.db,
                f.scope,
                f.generation,
                "communications.block.changed",
                2,
                &data
            )
            .await
            .is_err(),
            "a source cannot confirm contradictory contents at the same version"
        );

        data["projectionGeneration"] = json!(Uuid::new_v4());
        apply(
            &f.db,
            f.scope,
            f.generation,
            "communications.block.changed",
            99,
            &data,
        )
        .await
        .unwrap();
        require_unblocked(&f.db, f.scope, f.generation, a, b)
            .await
            .unwrap();
        data["projectionGeneration"] = json!(f.generation);
        apply(
            &f.db,
            f.scope,
            f.generation,
            "communications.block.changed",
            3,
            &data,
        )
        .await
        .unwrap();
        assert!(require_unblocked(&f.db, f.scope, f.generation, b, a)
            .await
            .is_err());
        let other = OwnerScope {
            environment_id: Uuid::new_v4(),
            ..f.scope
        };
        require_unblocked(&f.db, other, f.generation, a, b)
            .await
            .unwrap();
        require_unblocked(&f.db, f.scope, Uuid::new_v4(), a, b)
            .await
            .unwrap();
        f.close().await;
    }

    #[tokio::test]
    async fn native_application_rules_cover_every_pair_without_releasing_other_rules() {
        let Some(f) = Fixture::new().await else {
            return;
        };
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let rule = Uuid::new_v4();
        let other = Uuid::new_v4();
        let data = |id, actor, blocked| json!({"ruleId":id,"actorPrincipalId":actor,"targetPrincipalId":b,"blocked":blocked,"projectionGeneration":f.generation});
        apply(
            &f.db,
            f.scope,
            f.generation,
            "communications.domain-block.changed",
            1,
            &data(rule, None::<Uuid>, true),
        )
        .await
        .unwrap();
        apply(
            &f.db,
            f.scope,
            f.generation,
            "communications.domain-block.changed",
            1,
            &data(other, Some(a), true),
        )
        .await
        .unwrap();
        assert!(
            require_unblocked(&f.db, f.scope, f.generation, Uuid::new_v4(), b)
                .await
                .is_err()
        );
        apply(
            &f.db,
            f.scope,
            f.generation,
            "communications.domain-block.changed",
            2,
            &data(rule, None::<Uuid>, false),
        )
        .await
        .unwrap();
        require_unblocked(&f.db, f.scope, f.generation, Uuid::new_v4(), b)
            .await
            .unwrap();
        assert!(require_unblocked(&f.db, f.scope, f.generation, a, b)
            .await
            .is_err());
        f.close().await;
    }

    #[tokio::test]
    async fn block_ack_waits_for_an_earlier_send_and_later_sends_refuse() {
        let Some(f) = Fixture::new().await else {
            return;
        };
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let send = f.db.begin().await.unwrap();
        require_unblocked(&send, f.scope, f.generation, a, b)
            .await
            .unwrap();
        let db = f.db.clone();
        let scope = f.scope;
        let generation = f.generation;
        let (started, start) = tokio::sync::oneshot::channel();
        let mut block = tokio::spawn(async move {
            let txn = db.begin().await.unwrap();
            started.send(()).unwrap();
            apply(&txn,scope,generation,"communications.block.changed",1,
                &json!({"actorPrincipalId":a,"targetPrincipalId":b,"blocked":true,"projectionGeneration":generation})).await.unwrap();
            txn.commit().await.unwrap();
        });
        start.await.unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(100), &mut block)
            .await
            .is_err());
        send.commit().await.unwrap();
        block.await.unwrap();
        assert!(require_unblocked(&f.db, f.scope, f.generation, a, b)
            .await
            .is_err());
        f.close().await;
    }
}
