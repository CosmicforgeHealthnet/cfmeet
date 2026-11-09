use crate::{
    error::{Error, Result},
    model::*,
};
use cf_communications_policy::Scope;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait, Value,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value as Json};
use uuid::Uuid;

pub fn stmt(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}
pub async fn execute<C: ConnectionTrait>(db: &C, sql: &str, values: Vec<Value>) -> Result<u64> {
    Ok(db.execute(stmt(sql, values)).await?.rows_affected())
}
pub async fn rows<C: ConnectionTrait, T: DeserializeOwned>(
    db: &C,
    sql: &str,
    values: Vec<Value>,
) -> Result<Vec<T>> {
    db.query_all(stmt(sql, values))
        .await?
        .into_iter()
        .map(|r| {
            let doc: Json = r.try_get("", "doc")?;
            serde_json::from_value(doc).map_err(|_| Error::Unavailable)
        })
        .collect()
}
pub async fn one<C: ConnectionTrait, T: DeserializeOwned>(
    db: &C,
    sql: &str,
    values: Vec<Value>,
) -> Result<Option<T>> {
    Ok(rows(db, sql, values).await?.into_iter().next())
}
pub fn scoped(s: Scope) -> Vec<Value> {
    vec![
        s.tenant_id.into(),
        s.application_id.into(),
        s.environment_id.into(),
    ]
}
pub fn scoped_id(s: Scope, id: Uuid) -> Vec<Value> {
    let mut v = scoped(s);
    v.push(id.into());
    v
}

#[derive(Clone)]
pub struct Store {
    pub db: DatabaseConnection,
}
impl Store {
    pub async fn audit_system<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: Scope,
        operation: &str,
        resource: Option<Uuid>,
        details: Json,
    ) -> Result<()> {
        let mut v = scoped(scope);
        v.extend([operation.into(), resource.into(), details.into()]);
        execute(db,"INSERT INTO communication_audit(tenant_id,application_id,environment_id,operation,resource_id,details) VALUES($1,$2,$3,$4,$5,$6)",v).await?;
        Ok(())
    }
    pub async fn begin(&self) -> Result<DatabaseTransaction> {
        Ok(self.db.begin().await?)
    }
    pub async fn profile<C: ConnectionTrait>(
        &self,
        db: &C,
        s: Scope,
        id: Uuid,
        lock: bool,
    ) -> Result<Profile> {
        one(db,&format!("SELECT to_jsonb(p) doc FROM communication_profiles p WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND id=$4{}",if lock {" FOR UPDATE"} else {""}),scoped_id(s,id)).await?.ok_or(Error::NotFound)
    }
    pub async fn principal<C: ConnectionTrait>(
        &self,
        db: &C,
        s: Scope,
        id: Uuid,
    ) -> Result<Profile> {
        one(db,"SELECT to_jsonb(p) doc FROM communication_profiles p WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND principal_id=$4",scoped_id(s,id)).await?.ok_or(Error::NotFound)
    }
    pub async fn privacy<C: ConnectionTrait>(&self, db: &C, id: Uuid) -> Result<PrivacyRow> {
        one(
            db,
            "SELECT to_jsonb(p) doc FROM communication_privacy p WHERE profile_id=$1",
            vec![id.into()],
        )
        .await?
        .ok_or(Error::NotFound)
    }
    pub async fn current_public_id<C: ConnectionTrait>(&self, db: &C, id: Uuid) -> Result<String> {
        let value:Json=one(db,"SELECT jsonb_build_object('value',public_id) doc FROM communication_public_ids WHERE profile_id=$1 AND current",vec![id.into()]).await?.ok_or(Error::NotFound)?;
        value["value"]
            .as_str()
            .map(str::to_owned)
            .ok_or(Error::Unavailable)
    }
    pub async fn by_public_id<C: ConnectionTrait>(
        &self,
        db: &C,
        s: Scope,
        public_id: &str,
    ) -> Result<Profile> {
        if !crate::crypto::valid_public_id(public_id) {
            return Err(Error::NotFound);
        }
        let mut v = scoped(s);
        v.push(public_id.into());
        one(db,"SELECT to_jsonb(p) doc FROM communication_profiles p JOIN communication_public_ids i ON i.profile_id=p.id AND i.current WHERE p.tenant_id=$1 AND p.application_id=$2 AND p.environment_id=$3 AND i.public_id=$4",v).await?.ok_or(Error::NotFound)
    }
    pub async fn pair<C: ConnectionTrait>(
        &self,
        db: &C,
        s: Scope,
        a: Uuid,
        b: Uuid,
        lock: bool,
    ) -> Result<Option<Connection>> {
        if a == b {
            return Err(Error::Validation);
        }
        let mut v = scoped(s);
        v.extend([a.min(b).into(), a.max(b).into()]);
        one(db,&format!("SELECT to_jsonb(c) doc FROM communication_connections c WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND left_profile_id=$4 AND right_profile_id=$5{}",if lock {" FOR UPDATE"} else {""}),v).await
    }
    /// The same pair lock serializes requests, accept/remove and either-side blocks.
    pub async fn lock_pair(
        &self,
        db: &DatabaseTransaction,
        s: Scope,
        a: Uuid,
        b: Uuid,
    ) -> Result<Connection> {
        if a == b {
            return Err(Error::Validation);
        }
        let key = format!(
            "{}:{}:{}:{}:{}",
            s.tenant_id,
            s.application_id,
            s.environment_id,
            a.min(b),
            a.max(b)
        );
        db.query_one(stmt(
            "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
            vec![key.into()],
        ))
        .await?;
        let mut v = vec![Uuid::new_v4().into()];
        v.extend(scoped(s));
        v.extend([a.min(b).into(), a.max(b).into()]);
        execute(db,"INSERT INTO communication_connections(id,tenant_id,application_id,environment_id,left_profile_id,right_profile_id) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING",v).await?;
        self.pair(db, s, a, b, true)
            .await?
            .ok_or(Error::Unavailable)
    }
    pub async fn blocked<C: ConnectionTrait>(&self, db: &C, a: Uuid, b: Uuid) -> Result<bool> {
        let v:Json=one(db,"SELECT jsonb_build_object('blocked',EXISTS(SELECT 1 FROM communication_block_bridge WHERE active AND ((blocker_profile_id=$1 AND blocked_profile_id=$2) OR (blocker_profile_id=$2 AND blocked_profile_id=$1)))) doc",vec![a.into(),b.into()]).await?.ok_or(Error::Unavailable)?;
        Ok(v["blocked"] == true)
    }
    pub async fn relationship<C: ConnectionTrait>(
        &self,
        db: &C,
        a: &Profile,
        b: &Profile,
    ) -> Result<String> {
        if self.blocked(db, a.id, b.id).await? {
            return Ok("BLOCKED".into());
        }
        Ok(self
            .pair(db, a.scope(), a.id, b.id, false)
            .await?
            .map(|c| c.state)
            .unwrap_or_else(|| "NONE".into()))
    }
    pub async fn rate(
        &self,
        db: &DatabaseTransaction,
        owner: Uuid,
        operation: &str,
        limit: i64,
    ) -> Result<()> {
        let v:Json=one(db,"INSERT INTO communication_rate_windows(owner_profile_id,operation,window_start,count) VALUES($1,$2,date_trunc('hour',now()),1) ON CONFLICT(owner_profile_id,operation,window_start) DO UPDATE SET count=communication_rate_windows.count+1 RETURNING jsonb_build_object('count',count) doc",vec![owner.into(),operation.into()]).await?.ok_or(Error::Unavailable)?;
        if v["count"].as_i64().unwrap_or(limit + 1) > limit {
            Err(Error::RateLimited)
        } else {
            Ok(())
        }
    }
    pub async fn audit<C: ConnectionTrait>(
        &self,
        db: &C,
        p: &Profile,
        operation: &str,
        resource: Option<Uuid>,
        details: Json,
    ) -> Result<()> {
        let mut v = scoped(p.scope());
        v.extend([
            p.id.into(),
            operation.into(),
            resource.into(),
            details.into(),
        ]);
        execute(db,"INSERT INTO communication_audit(tenant_id,application_id,environment_id,actor_profile_id,operation,resource_id,details) VALUES($1,$2,$3,$4,$5,$6,$7)",v).await?;
        Ok(())
    }
    pub async fn event<C: ConnectionTrait>(
        &self,
        db: &C,
        p: &Profile,
        event_type: &str,
        aggregate_id: Uuid,
        version: i64,
        payload: Json,
    ) -> Result<()> {
        self.scoped_event(db, p.scope(), event_type, aggregate_id, version, payload)
            .await
    }
    /// The same durable fan-out to Calling and Messaging for a scope without a Hub
    /// profile (Section 14, ACC-010: account revocations of principals that never
    /// opened the Hub). The payload carries only references and epochs.
    pub async fn scoped_event<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: Scope,
        event_type: &str,
        aggregate_id: Uuid,
        version: i64,
        payload: Json,
    ) -> Result<()> {
        let event = Uuid::new_v4();
        for destination in ["CALLING", "MESSAGING"] {
            let mut v = vec![event.into()];
            v.extend(scoped(scope));
            v.extend([
                destination.into(),
                aggregate_id.into(),
                version.into(),
                event_type.into(),
                payload.clone().into(),
            ]);
            execute(db,"INSERT INTO communication_outbox(event_id,tenant_id,application_id,environment_id,destination,aggregate_id,aggregate_version,event_type,payload) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)",v).await?;
        }
        Ok(())
    }
    /// A durable event for Messaging alone (MSG-009 timeline nudges: Calling has no use
    /// for them). Same outbox, delivery, retry and dead-letter path as [`Self::scoped_event`].
    pub async fn messaging_event<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: Scope,
        event_type: &str,
        aggregate_id: Uuid,
        version: i64,
        payload: Json,
    ) -> Result<()> {
        let mut v = vec![Uuid::new_v4().into()];
        v.extend(scoped(scope));
        v.extend([
            "MESSAGING".into(),
            aggregate_id.into(),
            version.into(),
            event_type.into(),
            payload.into(),
        ]);
        execute(db,"INSERT INTO communication_outbox(event_id,tenant_id,application_id,environment_id,destination,aggregate_id,aggregate_version,event_type,payload) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)",v).await?;
        Ok(())
    }
    pub async fn idempotency(
        &self,
        db: &DatabaseTransaction,
        p: &Profile,
        operation: &str,
        key: Uuid,
        request: &Json,
    ) -> Result<Option<Json>> {
        let digest = crate::crypto::digest(request);
        execute(db,"INSERT INTO communication_idempotency(owner_profile_id,operation,key,request_digest) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING",vec![p.id.into(),operation.into(),key.into(),digest.clone().into()]).await?;
        let row=db.query_one(stmt("SELECT request_digest,response FROM communication_idempotency WHERE owner_profile_id=$1 AND operation=$2 AND key=$3 FOR UPDATE",vec![p.id.into(),operation.into(),key.into()])).await?.ok_or(Error::Unavailable)?;
        if row.try_get::<Vec<u8>>("", "request_digest")? != digest {
            return Err(Error::Idempotency);
        }
        Ok(row.try_get::<Option<Json>>("", "response")?)
    }
    pub async fn finish_idempotency(
        &self,
        db: &DatabaseTransaction,
        p: &Profile,
        operation: &str,
        key: Uuid,
        response: Json,
    ) -> Result<()> {
        execute(db,"UPDATE communication_idempotency SET response=$4 WHERE owner_profile_id=$1 AND operation=$2 AND key=$3",vec![p.id.into(),operation.into(),key.into(),response.into()]).await?;
        Ok(())
    }
    /// Append one entry to the owner's monotonic change feed (same transaction as the
    /// change itself, so a committed change always has a feed entry).
    pub async fn change<C: ConnectionTrait>(
        &self,
        db: &C,
        profile: Uuid,
        domain: &str,
        resource: Option<Uuid>,
        version: Option<i64>,
    ) -> Result<()> {
        execute(db,"INSERT INTO communication_changes(profile_id,domain,resource_id,resource_version) VALUES($1,$2,$3,$4)",vec![profile.into(),domain.into(),resource.into(),version.into()]).await?;
        Ok(())
    }
    /// A CONTACT feed entry for every participant of a relationship change.
    pub async fn relationship_changed<C: ConnectionTrait>(
        &self,
        db: &C,
        profiles: &[Uuid],
        connection: Uuid,
        version: i64,
    ) -> Result<()> {
        for profile in profiles {
            self.change(db, *profile, "CONTACT", Some(connection), Some(version))
                .await?;
        }
        Ok(())
    }
    pub fn public_profile(public_id: String, p: &Profile, relationship: String) -> Json {
        json!({"schemaVersion":1,"cfMeetId":public_id,"displayName":p.display_name,"avatarReference":p.avatar_reference,"relationship":relationship})
    }
}
