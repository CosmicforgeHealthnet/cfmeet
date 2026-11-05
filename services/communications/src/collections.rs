//! Section 11.2–11.3: private favorites and contact lists. Neither grants any
//! communication permission; Calling and Messaging keep enforcing policy themselves.
use crate::{
    error::{Error, Result},
    model::*,
    relationships::uuid_field,
    store::{execute, one, rows},
    State,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

pub const MAX_FAVORITES: i64 = 200;
pub const MAX_LISTS: i64 = 50;
pub const MAX_LIST_MEMBERS: i64 = 500;

fn valid_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && n.chars().count() <= 80 && !n.chars().any(char::is_control)
}
fn list_view(v: &Value, members: i64) -> Value {
    json!({"schemaVersion":1,"listId":v["id"],"name":v["name"],"position":v["position"],
        "memberCount":members,"version":v["version"],"updatedAt":v["updated_at"]})
}

impl State {
    /// Current CONNECTED contact owned by `p`; anything else is indistinguishable 404.
    async fn connected_contact(&self, p: &Profile, contact: Uuid) -> Result<Value> {
        let (alias, pair, _) = self.contact(p, contact).await?;
        if pair.state != "CONNECTED" {
            return Err(Error::NotFound);
        }
        Ok(alias)
    }

    // ---- C07–C09 favorites ----
    pub async fn list_favorites(&self, p: &Profile) -> Result<Value> {
        let ids:Vec<Value>=rows(&self.store.db,"SELECT jsonb_build_object('id',a.id) doc FROM communication_contact_aliases a JOIN communication_connections c ON c.id=a.connection_id WHERE a.owner_profile_id=$1 AND a.favorite AND c.state='CONNECTED' ORDER BY a.favorite_rank NULLS LAST,a.id LIMIT $2",vec![p.id.into(),MAX_FAVORITES.into()]).await?;
        let mut data = Vec::new();
        for v in ids {
            let mut view = self.contact_view(p, uuid_field(&v, "id")?).await?;
            let rank:Value=one(&self.store.db,"SELECT jsonb_build_object('rank',favorite_rank) doc FROM communication_contact_aliases WHERE id=$1",vec![uuid_field(&v,"id")?.into()]).await?.ok_or(Error::Unavailable)?;
            view["favoriteRank"] = rank["rank"].clone();
            data.push(view);
        }
        Ok(json!({"schemaVersion":1,"data":data,"limit":MAX_FAVORITES}))
    }
    pub async fn set_favorite(
        &self,
        p: &Profile,
        contact: Uuid,
        on: bool,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize, Default)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Favorite {
            rank: Option<i32>,
        }
        let input: Favorite = if body.as_object().is_some_and(|o| o.is_empty()) {
            Favorite::default()
        } else {
            serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?
        };
        if input.rank.is_some_and(|r| !(0..=9999).contains(&r)) || (!on && input.rank.is_some()) {
            return Err(Error::Validation);
        }
        if on {
            self.connected_contact(p, contact).await?;
        } else {
            self.contact(p, contact).await?;
        }
        let txn = self.store.begin().await?;
        let op = if on {
            "favorite.add"
        } else {
            "favorite.remove"
        };
        if let Some(v) = self
            .store
            .idempotency(&txn, p, op, key, &json!({"contactId":contact,"body":body}))
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        // Lock the owner's profile row: serializes the favorites limit check.
        self.store.profile(&txn, p.scope(), p.id, true).await?;
        if on {
            let count:Value=one(&txn,"SELECT jsonb_build_object('n',count(*)) doc FROM communication_contact_aliases WHERE owner_profile_id=$1 AND favorite AND id<>$2",vec![p.id.into(),contact.into()]).await?.ok_or(Error::Unavailable)?;
            if count["n"].as_i64().unwrap_or(MAX_FAVORITES) >= MAX_FAVORITES {
                return Err(Error::PolicyValidation);
            }
        }
        let v:Value=one(&txn,"UPDATE communication_contact_aliases SET favorite=$3,favorite_rank=$4,version=version+1,updated_at=now() WHERE id=$1 AND owner_profile_id=$2 RETURNING jsonb_build_object('version',version) doc",vec![contact.into(),p.id.into(),on.into(),input.rank.into()]).await?.ok_or(Error::NotFound)?;
        let version = v["version"].as_i64().ok_or(Error::Unavailable)?;
        self.store
            .change(&txn, p.id, "CONTACT", Some(contact), Some(version))
            .await?;
        let result = json!({"schemaVersion":1,"contactId":contact,"favorite":on,"favoriteRank":if on {json!(input.rank)} else {Value::Null},"version":version});
        self.store
            .finish_idempotency(&txn, p, op, key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }

    // ---- C10–C16 contact lists ----
    pub async fn list_lists(&self, p: &Profile) -> Result<Value> {
        let lists:Vec<Value>=rows(&self.store.db,"SELECT to_jsonb(l)||jsonb_build_object('members',(SELECT count(*) FROM communication_contact_list_members m WHERE m.list_id=l.id)) doc FROM communication_contact_lists l WHERE owner_profile_id=$1 ORDER BY position,id LIMIT $2",vec![p.id.into(),MAX_LISTS.into()]).await?;
        let data: Vec<Value> = lists
            .iter()
            .map(|l| list_view(l, l["members"].as_i64().unwrap_or(0)))
            .collect();
        Ok(json!({"schemaVersion":1,"data":data,"limit":MAX_LISTS}))
    }
    async fn owned_list<C: sea_orm::ConnectionTrait>(
        &self,
        db: &C,
        p: &Profile,
        list: Uuid,
        lock: bool,
    ) -> Result<Value> {
        one(db,&format!("SELECT to_jsonb(l) doc FROM communication_contact_lists l WHERE id=$1 AND owner_profile_id=$2{}",if lock {" FOR UPDATE"} else {""}),vec![list.into(),p.id.into()]).await?.ok_or(Error::NotFound)
    }
    async fn member_count<C: sea_orm::ConnectionTrait>(&self, db: &C, list: Uuid) -> Result<i64> {
        let v:Value=one(db,"SELECT jsonb_build_object('n',count(*)) doc FROM communication_contact_list_members WHERE list_id=$1",vec![list.into()]).await?.ok_or(Error::Unavailable)?;
        v["n"].as_i64().ok_or(Error::Unavailable)
    }
    pub async fn create_list(&self, p: &Profile, body: Value, key: Uuid) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Create {
            name: String,
            position: Option<i32>,
        }
        let input: Create = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        if !valid_name(&input.name) || input.position.is_some_and(|x| !(0..=9999).contains(&x)) {
            return Err(Error::Validation);
        }
        let txn = self.store.begin().await?;
        if let Some(v) = self
            .store
            .idempotency(&txn, p, "contact-list.create", key, &body)
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        self.store.profile(&txn, p.scope(), p.id, true).await?;
        let stats:Value=one(&txn,"SELECT jsonb_build_object('n',count(*),'dup',count(*) FILTER (WHERE lower(name)=lower($2))) doc FROM communication_contact_lists WHERE owner_profile_id=$1",vec![p.id.into(),input.name.trim().into()]).await?.ok_or(Error::Unavailable)?;
        if stats["dup"].as_i64() != Some(0) {
            return Err(Error::Duplicate);
        }
        if stats["n"].as_i64().unwrap_or(MAX_LISTS) >= MAX_LISTS {
            return Err(Error::PolicyValidation);
        }
        let id = Uuid::new_v4();
        let v:Value=one(&txn,"INSERT INTO communication_contact_lists AS l (id,owner_profile_id,name,position) VALUES($1,$2,$3,$4) RETURNING to_jsonb(l) doc",vec![id.into(),p.id.into(),input.name.trim().into(),input.position.unwrap_or(0).into()]).await?.ok_or(Error::Unavailable)?;
        self.store
            .change(&txn, p.id, "CONTACT_LIST", Some(id), Some(1))
            .await?;
        let result = list_view(&v, 0);
        self.store
            .finish_idempotency(&txn, p, "contact-list.create", key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
    pub async fn get_list(
        &self,
        p: &Profile,
        list: Uuid,
        limit: i64,
        cursor: Uuid,
    ) -> Result<Value> {
        let l = self.owned_list(&self.store.db, p, list, false).await?;
        let members:Vec<Value>=rows(&self.store.db,"SELECT jsonb_build_object('id',m.contact_id) doc FROM communication_contact_list_members m JOIN communication_contact_aliases a ON a.id=m.contact_id JOIN communication_connections c ON c.id=a.connection_id WHERE m.list_id=$1 AND m.contact_id>$2 AND c.state='CONNECTED' ORDER BY m.contact_id LIMIT $3",vec![list.into(),cursor.into(),(limit+1).into()]).await?;
        let more = members.len() > limit as usize;
        let mut data = Vec::new();
        let mut last = None;
        for m in members.into_iter().take(limit as usize) {
            let id = uuid_field(&m, "id")?;
            data.push(self.contact_view(p, id).await?);
            last = Some(id);
        }
        let mut view = list_view(&l, self.member_count(&self.store.db, list).await?);
        view["members"] = json!(data);
        view["nextCursor"] = json!(if more { last } else { None });
        Ok(view)
    }
    pub async fn patch_list(
        &self,
        p: &Profile,
        list: Uuid,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Patch {
            expected_version: i64,
            name: Option<String>,
            position: Option<i32>,
        }
        let input: Patch = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        if input.name.as_deref().is_some_and(|n| !valid_name(n))
            || input.position.is_some_and(|x| !(0..=9999).contains(&x))
        {
            return Err(Error::Validation);
        }
        let txn = self.store.begin().await?;
        let op = "contact-list.patch";
        if let Some(v) = self
            .store
            .idempotency(&txn, p, op, key, &json!({"listId":list,"body":body}))
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let old = self.owned_list(&txn, p, list, true).await?;
        if old["version"].as_i64() != Some(input.expected_version) {
            return Err(Error::Version);
        }
        let name = input
            .name
            .map(|n| n.trim().to_owned())
            .unwrap_or_else(|| old["name"].as_str().unwrap_or_default().to_owned());
        let dup:Value=one(&txn,"SELECT jsonb_build_object('dup',EXISTS(SELECT 1 FROM communication_contact_lists WHERE owner_profile_id=$1 AND id<>$2 AND lower(name)=lower($3))) doc",vec![p.id.into(),list.into(),name.clone().into()]).await?.ok_or(Error::Unavailable)?;
        if dup["dup"] == true {
            return Err(Error::Duplicate);
        }
        let position = input
            .position
            .unwrap_or(old["position"].as_i64().unwrap_or(0) as i32);
        let v:Value=one(&txn,"UPDATE communication_contact_lists l SET name=$2,position=$3,version=l.version+1,updated_at=now() WHERE l.id=$1 RETURNING to_jsonb(l) doc",vec![list.into(),name.into(),position.into()]).await?.ok_or(Error::Unavailable)?;
        self.store
            .change(
                &txn,
                p.id,
                "CONTACT_LIST",
                Some(list),
                v["version"].as_i64(),
            )
            .await?;
        let result = list_view(&v, self.member_count(&txn, list).await?);
        self.store
            .finish_idempotency(&txn, p, op, key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
    pub async fn delete_list(
        &self,
        p: &Profile,
        list: Uuid,
        body: Value,
        key: Uuid,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Delete {
            expected_version: i64,
        }
        let input: Delete = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
        let txn = self.store.begin().await?;
        let op = "contact-list.delete";
        if let Some(v) = self
            .store
            .idempotency(&txn, p, op, key, &json!({"listId":list,"body":body}))
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        let old = self.owned_list(&txn, p, list, true).await?;
        if old["version"].as_i64() != Some(input.expected_version) {
            return Err(Error::Version);
        }
        // Members cascade; contacts and their relationships are never touched.
        execute(
            &txn,
            "DELETE FROM communication_contact_lists WHERE id=$1 AND owner_profile_id=$2",
            vec![list.into(), p.id.into()],
        )
        .await?;
        self.store
            .change(&txn, p.id, "CONTACT_LIST", Some(list), None)
            .await?;
        let result = json!({"schemaVersion":1,"listId":list,"deleted":true});
        self.store
            .finish_idempotency(&txn, p, op, key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
    pub async fn set_list_member(
        &self,
        p: &Profile,
        list: Uuid,
        contact: Uuid,
        add: bool,
        key: Uuid,
    ) -> Result<Value> {
        if add {
            self.connected_contact(p, contact).await?;
        }
        let txn = self.store.begin().await?;
        let op = if add {
            "contact-list.member.add"
        } else {
            "contact-list.member.remove"
        };
        if let Some(v) = self
            .store
            .idempotency(
                &txn,
                p,
                op,
                key,
                &json!({"listId":list,"contactId":contact}),
            )
            .await?
        {
            txn.commit().await?;
            return Ok(v);
        }
        self.owned_list(&txn, p, list, true).await?;
        let changed = if add {
            if self.member_count(&txn, list).await? >= MAX_LIST_MEMBERS {
                let exists:Value=one(&txn,"SELECT jsonb_build_object('x',EXISTS(SELECT 1 FROM communication_contact_list_members WHERE list_id=$1 AND contact_id=$2)) doc",vec![list.into(),contact.into()]).await?.ok_or(Error::Unavailable)?;
                if exists["x"] != true {
                    return Err(Error::PolicyValidation);
                }
            }
            execute(&txn,"INSERT INTO communication_contact_list_members(list_id,owner_profile_id,contact_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",vec![list.into(),p.id.into(),contact.into()]).await?
        } else {
            execute(&txn,"DELETE FROM communication_contact_list_members WHERE list_id=$1 AND owner_profile_id=$2 AND contact_id=$3",vec![list.into(),p.id.into(),contact.into()]).await?
        };
        let v:Value=if changed>0 {
            one(&txn,"UPDATE communication_contact_lists l SET version=l.version+1,updated_at=now() WHERE l.id=$1 RETURNING to_jsonb(l) doc",vec![list.into()]).await?
        } else {
            one(&txn,"SELECT to_jsonb(l) doc FROM communication_contact_lists l WHERE id=$1",vec![list.into()]).await?
        }.ok_or(Error::Unavailable)?;
        if changed > 0 {
            self.store
                .change(
                    &txn,
                    p.id,
                    "CONTACT_LIST",
                    Some(list),
                    v["version"].as_i64(),
                )
                .await?;
        }
        let result = json!({"schemaVersion":1,"listId":list,"contactId":contact,"member":add,"version":v["version"],"memberCount":self.member_count(&txn,list).await?});
        self.store
            .finish_idempotency(&txn, p, op, key, result.clone())
            .await?;
        txn.commit().await?;
        Ok(result)
    }
}
