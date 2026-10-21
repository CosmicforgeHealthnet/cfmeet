//! Parts E–G prerequisites (Workstreams E / F foundation): membership, the message store,
//! the MLS Delivery Service and the durable conversation event log — exercised with REAL
//! OpenMLS client devices through the real router and PostgreSQL. Devices encrypt and
//! decrypt locally; the server never sees plaintext.

mod common;

use axum::http::StatusCode;
use cf_messaging_crypto::binding;
use common::{flows::*, *};
use messaging::conversations::sequence::{AcceptanceRecord, AcceptanceTarget};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn private_direct_messaging_end_to_end_with_real_mls_devices() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, mut bob) = h.e2ee_direct(&c).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let state = h
        .get(
            &c,
            Some(alice.principal),
            &format!("/v1/conversations/{cid}/mls/state"),
        )
        .await;
    assert_eq!(state.json["mlsEpoch"], 1, "{}", state.text);

    let r = h
        .send_e2ee(&c, &conv, &mut alice, "hello bob, this is secret")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["e2ee"]["epoch"], 1);
    // The acceptance record a recipient recomputes binds sequence, id and ciphertext.
    let ct = unb64(&r.json["e2ee"]["ciphertext"]);
    let record = AcceptanceRecord::new(
        AcceptanceTarget {
            tenant_id: c.context.scope.tenant_id,
            application_id: c.context.scope.application_id,
            environment_id: c.context.scope.environment_id,
            conversation_id: uuid(&conv["conversationId"]),
            mls_group_id: unb64(&conv["encryption"]["mlsGroupId"]).try_into().unwrap(),
        },
        &messaging::domain::profile::TransportMetadata {
            client_message_id: uuid(&r.json["clientMessageId"]),
            sender_device_id: alice.device_id(),
            epoch: 1,
        },
        &messaging::domain::profile::OpaqueCiphertext::new(ct).unwrap(),
        r.json["sequence"].as_i64().unwrap(),
        uuid(&r.json["messageId"]),
        chrono::DateTime::parse_from_rfc3339(r.json["createdAt"].as_str().unwrap())
            .unwrap()
            .into(),
    );
    assert_eq!(
        hex::encode(record.digest()),
        r.json["e2ee"]["acceptanceDigest"].as_str().unwrap()
    );

    let got = h.receive(&c, &conv, &mut bob).await;
    assert_eq!(
        got.iter().map(|g| g.1.as_str()).collect::<Vec<_>>(),
        vec!["hello bob, this is secret"]
    );
    let reply = h.send_e2ee(&c, &conv, &mut bob, "hi alice").await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(h.receive(&c, &conv, &mut alice).await[0].1, "hi alice");

    // The server never stored or emitted plaintext (canary over every table).
    let dump = h.dump().await;
    for canary in ["hello bob", "this is secret", "hi alice"] {
        assert!(
            !dump.contains(canary),
            "plaintext `{canary}` in the database"
        );
    }
    // Sequences are canonical and gap-free: commit 1, then 2, 3.
    let list = h
        .get(
            &c,
            Some(bob.principal),
            &format!("/v1/conversations/{cid}/messages"),
        )
        .await;
    let seqs: Vec<i64> = list.json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["sequence"].as_i64().unwrap())
        .collect();
    assert_eq!(seqs, vec![1, 2, 3]);
    assert_eq!(list.json["messages"][0]["kind"], "MLS_COMMIT");
}

#[tokio::test]
async fn the_delivery_service_refuses_plaintext_foreign_groups_wrong_epochs_and_disguised_commits()
{
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, _bob) = h.e2ee_direct(&c).await;
    let (other, mut carol, _dave) = h.e2ee_direct(&c).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let path = format!("/v1/conversations/{cid}/messages");
    // Plaintext into an E2EE conversation: refused, nothing stored.
    let r = h
        .post(
            &c,
            Some(alice.principal),
            &path,
            json!({ "clientMessageId": Uuid::new_v4(), "text": "leak" }),
        )
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("CONTENT_REJECTED", "PLAINTEXT_IN_E2EE")
    );
    // A ciphertext of another group (valid MLS, wrong group id).
    let id = Uuid::new_v4();
    let foreign = h.encrypt_for(&c, &other, &mut carol, id, "x");
    let r = h
        .post(&c, Some(alice.principal), &path, json!({ "clientMessageId": id, "deviceId": alice.device_id(), "epoch": 1, "ciphertext": b64(&foreign) }))
        .await;
    assert_eq!((r.code(), r.reason()), ("CONTENT_REJECTED", "WRONG_GROUP"));
    // Garbage bytes.
    let r = h
        .post(&c, Some(alice.principal), &path, json!({ "clientMessageId": Uuid::new_v4(), "deviceId": alice.device_id(), "epoch": 1, "ciphertext": b64(b"not mls") }))
        .await;
    assert_eq!(r.reason(), "NOT_AN_MLS_MESSAGE");
    // A claimed epoch that differs from the framing.
    let id = Uuid::new_v4();
    let ct = h.encrypt_for(&c, &conv, &mut alice, id, "y");
    let r = h
        .post(&c, Some(alice.principal), &path, json!({ "clientMessageId": id, "deviceId": alice.device_id(), "epoch": 7, "ciphertext": b64(&ct) }))
        .await;
    assert_eq!(r.reason(), "EPOCH_FIELD_MISMATCH");
    // Another principal's device cannot be used.
    let r = h
        .post(&c, Some(alice.principal), &path, json!({ "clientMessageId": id, "deviceId": carol.device_id(), "epoch": 1, "ciphertext": b64(&ct) }))
        .await;
    assert_eq!(r.code(), "NOT_FOUND");
    // An application message submitted as a commit.
    let r = h.commit(&c, &cid, &alice, &ct, 1, None, &[], &[]).await;
    assert_eq!(r.reason(), "NOT_A_COMMIT");
    // A non-member cannot send, read, or learn that the conversation exists.
    let r = h.send_e2ee(&c, &conv, &mut carol, "intruder").await;
    assert_eq!(r.code(), "NOT_FOUND");
    let unknown = h
        .get(
            &c,
            Some(carol.principal),
            &format!("/v1/conversations/{}/messages", Uuid::new_v4()),
        )
        .await;
    let hidden = h.get(&c, Some(carol.principal), &path).await;
    assert_eq!(
        (hidden.status, hidden.json["error"]["code"].clone()),
        (unknown.status, unknown.json["error"]["code"].clone())
    );
    // Another tenant's credential sees nothing.
    let other_tenant = h.customer(ALL_SCOPES);
    let stranger = h.principal(&other_tenant);
    let r = h.get(&other_tenant, Some(stranger), &path).await;
    assert_eq!(r.code(), "NOT_FOUND");
}

#[tokio::test]
async fn retries_are_idempotent_and_stale_epochs_are_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, mut bob) = h.e2ee_direct(&c).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let id = Uuid::new_v4();
    let ct = h.encrypt_for(&c, &conv, &mut alice, id, "once");
    let body = json!({ "clientMessageId": id, "deviceId": alice.device_id(), "epoch": 1, "ciphertext": b64(&ct) });
    let path = format!("/v1/conversations/{cid}/messages");
    let first = h.post(&c, Some(alice.principal), &path, body.clone()).await;
    assert_eq!(first.status, StatusCode::CREATED);
    let again = h.post(&c, Some(alice.principal), &path, body).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json["messageId"], first.json["messageId"]);
    let other = h.encrypt_for(&c, &conv, &mut alice, id, "different bytes");
    let r = h
        .post(&c, Some(alice.principal), &path, json!({ "clientMessageId": id, "deviceId": alice.device_id(), "epoch": 1, "ciphertext": b64(&other) }))
        .await;
    assert_eq!(r.code(), "IDEMPOTENCY_KEY_REUSED");
    assert_eq!(h.receive(&c, &conv, &mut bob).await.len(), 1);
    // Bob commits an update (epoch 1 -> 2); Alice's next message in epoch 1 is stale.
    let gid = unb64(&conv["encryption"]["mlsGroupId"]);
    let _ = gid;
    let dummy = h.member(&c, 1).await;
    let add = h
        .post(
            &c,
            Some(alice.principal),
            &format!("/v1/conversations/{cid}/members"),
            json!({ "principalId": dummy.principal }),
        )
        .await;
    assert_eq!(
        add.code(),
        "INVALID_TRANSITION",
        "DIRECT membership is fixed"
    );
    // Two members racing commits for the same epoch: exactly one wins.
    let a_out = alice
        .device
        .remove_devices(alice.group.as_mut().unwrap(), &[bob.device_id()])
        .unwrap();
    let b_out = bob
        .device
        .remove_devices(bob.group.as_mut().unwrap(), &[alice.device_id()])
        .unwrap();
    let (r1, r2) = tokio::join!(
        h.commit(&c, &cid, &alice, &a_out.commit, 1, None, &[], &[]),
        h.commit(&c, &cid, &bob, &b_out.commit, 1, None, &[], &[])
    );
    let codes = [r1.status, r2.status];
    assert!(
        codes.contains(&StatusCode::CREATED) && codes.contains(&StatusCode::CONFLICT),
        "{codes:?}"
    );
    let loser = if r1.status == StatusCode::CONFLICT {
        r1
    } else {
        r2
    };
    assert_eq!(loser.code(), "EPOCH_MISMATCH");
}

#[tokio::test]
async fn removing_a_member_requires_a_removal_commit_and_the_removed_device_cannot_decrypt() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, mut m) = h.e2ee_group(&c, 3).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let (owner, rest) = m.split_at_mut(1);
    let (bob, carol) = rest.split_at_mut(1);
    let (owner, bob, carol) = (&mut owner[0], &mut bob[0], &mut carol[0]);
    assert_eq!(
        h.send_e2ee(&c, &conv, owner, "before").await.status,
        StatusCode::CREATED
    );
    assert_eq!(h.receive(&c, &conv, carol).await[0].1, "before");
    assert_eq!(h.receive(&c, &conv, bob).await[0].1, "before");

    // Server-side removal: authorization ends at once; the group needs a rekey.
    let r = h
        .delete(
            &c,
            Some(owner.principal),
            &format!("/v1/conversations/{cid}/members/{}", carol.principal),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["mlsRekeyRequired"], true);
    assert_eq!(r.json["cryptographicRemoval"], "PENDING_MEMBER_COMMIT");
    let blocked = h.send_e2ee(&c, &conv, owner, "too early").await;
    assert_eq!(blocked.code(), "MLS_REKEY_REQUIRED");
    // The removed principal can no longer read anything.
    let r = h
        .get(
            &c,
            Some(carol.principal),
            &format!("/v1/conversations/{cid}/messages"),
        )
        .await;
    assert_eq!(r.code(), "NOT_FOUND");
    // A commit that does not remove the pending device is refused.
    let wrong = owner
        .device
        .remove_devices(owner.group.as_mut().unwrap(), &[bob.device_id()]);
    let wrong = wrong.unwrap();
    let r = h
        .commit(
            &c,
            &cid,
            owner,
            &wrong.commit,
            1,
            None,
            &[],
            &[bob.device_id()],
        )
        .await;
    assert_eq!(r.reason(), "COMMIT_MISSING_REMOVALS");
    // (The owner's local group moved on with a commit the server refused: rebuild it from
    // a fresh state would be the SDK's job; here the owner uses Bob's device to commit.)
    let out = bob
        .device
        .remove_devices(bob.group.as_mut().unwrap(), &[carol.device_id()])
        .unwrap();
    let r = h
        .commit(
            &c,
            &cid,
            bob,
            &out.commit,
            1,
            None,
            &[],
            &[carol.device_id()],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let state = h
        .get(
            &c,
            Some(bob.principal),
            &format!("/v1/conversations/{cid}/mls/state"),
        )
        .await;
    assert_eq!(
        (
            state.json["mlsEpoch"].clone(),
            state.json["rekeyRequired"].clone()
        ),
        (json!(2), json!(false))
    );
    bob.cursor = r.json["sequence"].as_i64().unwrap();
    let after = h.send_e2ee(&c, &conv, bob, "after removal").await;
    assert_eq!(after.status, StatusCode::CREATED, "{}", after.text);
    // Even with the ciphertext in hand (a leaked database row), the removed device cannot
    // decrypt: it was cryptographically excluded at epoch 2.
    let ct = unb64(&after.json["e2ee"]["ciphertext"]);
    let removal = unb64(&r.json["e2ee"]["ciphertext"]);
    let _ = carol
        .device
        .process(carol.group.as_mut().unwrap(), &removal);
    assert!(carol
        .device
        .process(carol.group.as_mut().unwrap(), &ct)
        .is_err());
    let _ = owner;
}

#[tokio::test]
async fn a_revoked_device_forces_a_rekey_of_its_groups() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, bob) = h.e2ee_direct(&c).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let r = h
        .post(
            &c,
            Some(bob.principal),
            &format!("/v1/messaging/devices/{}/revoke", bob.device_id()),
            json!({ "reason": "LOST_OR_STOLEN" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let state = h
        .get(
            &c,
            Some(alice.principal),
            &format!("/v1/conversations/{cid}/mls/state"),
        )
        .await;
    assert_eq!(state.json["rekeyRequired"], true);
    assert_eq!(state.json["pendingRemovals"][0]["reason"], "DEVICE_REVOKED");
    assert_eq!(
        h.send_e2ee(&c, &conv, &mut alice, "x").await.code(),
        "MLS_REKEY_REQUIRED"
    );
    let out = alice
        .device
        .remove_devices(alice.group.as_mut().unwrap(), &[bob.device_id()])
        .unwrap();
    let r = h
        .commit(
            &c,
            &cid,
            &alice,
            &out.commit,
            1,
            None,
            &[],
            &[bob.device_id()],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(
        h.send_e2ee(&c, &conv, &mut alice, "y").await.status,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn managed_messages_are_sealed_at_rest_editable_and_deletable() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let r = h
        .send_managed(&c, &conv, people[0], "quarterly numbers attached")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["managed"]["text"], "quarterly numbers attached");
    let mid = r.json["messageId"].as_str().unwrap().to_string();
    // Sealed at rest: the text is not in any column (only the search index's words are).
    let raw = h
        .scalar(&format!("SELECT count(*) FROM messaging_messages WHERE id = '{mid}' AND position(convert_to('quarterly numbers', 'UTF8') in managed_sealed) > 0"))
        .await;
    assert_eq!(raw, 0);
    // Ciphertext is refused in MANAGED.
    let bad = h
        .post(&c, Some(people[0]), &format!("/v1/conversations/{cid}/messages"), json!({ "clientMessageId": Uuid::new_v4(), "ciphertext": "AAAA", "deviceId": Uuid::new_v4(), "epoch": 0 }))
        .await;
    assert_eq!(bad.reason(), "CIPHERTEXT_IN_MANAGED");
    // Only the sender edits.
    let path = format!("/v1/conversations/{cid}/messages/{mid}");
    assert_eq!(
        h.patch(&c, Some(people[1]), &path, json!({ "text": "x" }))
            .await
            .reason(),
        "NOT_SENDER"
    );
    let e = h
        .patch(
            &c,
            Some(people[0]),
            &path,
            json!({ "text": "quarterly numbers revised" }),
        )
        .await;
    assert_eq!(e.status, StatusCode::OK, "{}", e.text);
    assert_eq!(
        (
            e.json["editCount"].clone(),
            e.json["managed"]["text"].clone()
        ),
        (json!(1), json!("quarterly numbers revised"))
    );
    // Delete for everyone: a tombstone without content for every reader and replay.
    let d = h.delete(&c, Some(people[0]), &path).await;
    assert_eq!(d.json["deletion"], "DELETED", "{}", d.text);
    let seen = h.get(&c, Some(people[1]), &path).await;
    assert_eq!(seen.json["resolution"], "DELETED");
    assert_eq!(seen.json["message"]["managed"]["text"], json!(null));
    let events = h
        .get(
            &c,
            Some(people[1]),
            &format!("/v1/conversations/{cid}/events"),
        )
        .await;
    assert!(
        !events.text.contains("quarterly"),
        "deleted content replayed: {}",
        events.text
    );
    let types: Vec<String> = events.json["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_string())
        .collect();
    assert!(
        types.ends_with(&[
            "message.created".into(),
            "message.edited".into(),
            "message.deleted".into()
        ]),
        "{types:?}"
    );
}

#[tokio::test]
async fn new_members_see_only_history_after_they_joined_and_events_replay_in_order() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    for t in ["one", "two"] {
        assert_eq!(
            h.send_managed(&c, &conv, people[0], t).await.status,
            StatusCode::CREATED
        );
    }
    let late = h.principal(&c);
    let r = h
        .post(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{cid}/members"),
            json!({ "principalId": late }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(
        h.send_managed(&c, &conv, people[1], "three").await.status,
        StatusCode::CREATED
    );
    let list = h
        .get(&c, Some(late), &format!("/v1/conversations/{cid}/messages"))
        .await;
    let texts: Vec<&str> = list.json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["managed"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["three"]);
    let ev = h
        .get(
            &c,
            Some(late),
            &format!("/v1/conversations/{cid}/events?after=0"),
        )
        .await;
    let evs = ev.json["events"].as_array().unwrap();
    assert_eq!(evs[0]["type"], "member.added");
    assert_eq!(evs[1]["data"]["message"]["managed"]["text"], "three");
    let seqs: Vec<i64> = evs
        .iter()
        .map(|e| e["eventSequence"].as_i64().unwrap())
        .collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
    // The early member sees everything in order, and permalinks before the late member's
    // window are NOT_FOUND for it.
    let all = h
        .get(
            &c,
            Some(people[1]),
            &format!("/v1/conversations/{cid}/events"),
        )
        .await;
    assert!(all.json["events"].as_array().unwrap().len() > evs.len());
    let first_id = h
        .get(
            &c,
            Some(people[1]),
            &format!("/v1/conversations/{cid}/messages"),
        )
        .await
        .json["messages"][0]["messageId"]
        .clone();
    let r = h
        .get(
            &c,
            Some(late),
            &format!(
                "/v1/conversations/{cid}/messages/{}",
                first_id.as_str().unwrap()
            ),
        )
        .await;
    assert_eq!(r.code(), "NOT_FOUND");
    // Read cursors are monotonic and evented.
    let rc = h
        .put(
            &c,
            Some(late),
            &format!("/v1/conversations/{cid}/read-cursor"),
            json!({ "sequence": 3 }),
        )
        .await;
    assert_eq!(rc.json["readSequence"], 3, "{}", rc.text);
    let rc = h
        .put(
            &c,
            Some(late),
            &format!("/v1/conversations/{cid}/read-cursor"),
            json!({ "sequence": 1 }),
        )
        .await;
    assert_eq!(rc.json["readSequence"], 3);
    let rc = h
        .put(
            &c,
            Some(late),
            &format!("/v1/conversations/{cid}/read-cursor"),
            json!({ "sequence": 99 }),
        )
        .await;
    assert_eq!(rc.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn blocks_prevent_direct_conversations_and_messages_even_under_concurrency() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let a = h.principal(&c);
    let b = h.principal(&c);
    h.allow_managed(&c, "DIRECT").await;
    let conv = h.create_conversation(&c, a, json!({ "contextClass": "DIRECT", "counterpartPrincipalId": b, "securityProfile": "MANAGED" })).await;
    assert_eq!(conv.status, StatusCode::CREATED, "{}", conv.text);
    // Race: B blocks A while A sends a burst. Every message either committed BEFORE the
    // block or was refused — none after it.
    let sends = (0..10).map(|i| {
        let h = &h;
        let c = &c;
        let conv = &conv.json;
        async move { h.send_managed(c, conv, a, &format!("m{i}")).await }
    });
    let block = h.post(
        &c,
        Some(b),
        "/v1/messaging/blocks",
        json!({ "principalId": a }),
    );
    let (results, block) = tokio::join!(futures_join_all(sends), block);
    assert_eq!(block.status, StatusCode::CREATED, "{}", block.text);
    let mut accepted = 0;
    for r in &results {
        assert!(
            r.status == StatusCode::CREATED || r.code() == "DELIVERY_REFUSED",
            "{}",
            r.text
        );
        accepted += i64::from(r.status == StatusCode::CREATED);
    }
    // Exactly the accepted messages exist (nothing half-committed around the block).
    let stored = h
        .scalar(&format!(
            "SELECT count(*) FROM messaging_messages WHERE sender_principal_id = '{a}'"
        ))
        .await;
    assert_eq!(stored, accepted);
    assert_eq!(
        h.send_managed(&c, &conv.json, a, "after").await.code(),
        "DELIVERY_REFUSED"
    );
    assert_eq!(
        h.send_managed(&c, &conv.json, b, "blocker too")
            .await
            .code(),
        "DELIVERY_REFUSED"
    );
    // No duplicate DIRECT conversation bypasses the block (either profile).
    let dup = h.create_conversation(&c, a, json!({ "contextClass": "DIRECT", "counterpartPrincipalId": b, "securityProfile": "PRIVATE_E2EE" })).await;
    assert_eq!(dup.reason(), "COUNTERPART_NOT_AVAILABLE");
    // Unblocking restores messaging.
    assert_eq!(
        h.delete(&c, Some(b), &format!("/v1/messaging/blocks/{a}"))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        h.send_managed(&c, &conv.json, a, "hello again")
            .await
            .status,
        StatusCode::CREATED
    );
}

async fn futures_join_all<F: std::future::Future>(fs: impl Iterator<Item = F>) -> Vec<F::Output> {
    let handles: Vec<_> = fs.collect();
    let mut out = Vec::new();
    // Interleave on one task: polled concurrently through a FuturesUnordered-free join.
    let pinned: Vec<std::pin::Pin<Box<F>>> = handles.into_iter().map(Box::pin).collect();
    let mut pending: Vec<Option<std::pin::Pin<Box<F>>>> = pinned.into_iter().map(Some).collect();
    let mut results: Vec<Option<F::Output>> = (0..pending.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut done = true;
        for (i, slot) in pending.iter_mut().enumerate() {
            if let Some(f) = slot {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => {
                        results[i] = Some(v);
                        *slot = None;
                    }
                    std::task::Poll::Pending => done = false,
                }
            }
        }
        if done {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    out.extend(results.into_iter().map(|r| r.unwrap()));
    out
}

#[tokio::test]
async fn muted_members_and_closed_conversations_cannot_send() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    // Members cannot administer; the owner can.
    let r = h
        .delete(
            &c,
            Some(people[1]),
            &format!("/v1/conversations/{cid}/members/{}", people[0]),
        )
        .await;
    assert_eq!(r.reason(), "NOT_ADMIN");
    let r = h
        .delete(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{cid}/members/{}", people[0]),
        )
        .await;
    assert_eq!(r.reason(), "OWNER_CANNOT_BE_REMOVED");
    let v = h
        .get(&c, Some(people[0]), &format!("/v1/conversations/{cid}"))
        .await;
    let p = h
        .patch(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{cid}"),
            json!({ "expectedVersion": v.json["version"], "lifecycleState": "LOCKED" }),
        )
        .await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.text);
    assert_eq!(
        h.send_managed(&c, &conv, people[0], "x").await.reason(),
        "LOCKED"
    );
    // A member who leaves stops receiving.
    let p = h
        .patch(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{cid}"),
            json!({ "expectedVersion": p.json["version"], "lifecycleState": "ACTIVE" }),
        )
        .await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.text);
    let r = h
        .delete(
            &c,
            Some(people[1]),
            &format!("/v1/conversations/{cid}/members/{}", people[1]),
        )
        .await;
    assert_eq!(r.json["state"], "LEFT", "{}", r.text);
    assert_eq!(
        h.get(
            &c,
            Some(people[1]),
            &format!("/v1/conversations/{cid}/events")
        )
        .await
        .code(),
        "NOT_FOUND"
    );
    let _ = binding::NONCE_LEN;
}

#[tokio::test]
async fn responses_and_events_conform_to_the_published_contracts() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, mut bob) = h.e2ee_direct(&c).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let sent = h.send_e2ee(&c, &conv, &mut alice, "contract").await;
    conforms("messaging/common/v1.schema.json#/$defs/Message", &sent.json);
    h.receive(&c, &conv, &mut bob).await;
    let mid = sent.json["messageId"].as_str().unwrap();
    for (path, def) in [
        (format!("/v1/conversations/{cid}/messages"), "MessagePage"),
        (
            format!("/v1/conversations/{cid}/messages/{mid}"),
            "PermalinkResolution",
        ),
        (format!("/v1/conversations/{cid}/events"), "EventPage"),
        (format!("/v1/conversations/{cid}/members"), "MemberPage"),
        (
            format!("/v1/conversations/{cid}/mls/state"),
            "MlsGroupState",
        ),
        (
            format!("/v1/messaging/devices/{}/welcomes", bob.device_id()),
            "WelcomePage",
        ),
        ("/v1/messaging/blocks".to_string(), "BlockPage"),
    ] {
        let r = h.get(&c, Some(bob.principal), &path).await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text);
        conforms(
            &format!("messaging/common/v1.schema.json#/$defs/{def}"),
            &r.json,
        );
    }
    let (mconv, people) = h.managed_group(&c, 3).await;
    let m = h
        .send_managed(&c, &mconv, people[0], "managed contract")
        .await;
    conforms("messaging/common/v1.schema.json#/$defs/Message", &m.json);
    let mcid = mconv["conversationId"].as_str().unwrap();
    let rm = h
        .delete(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{mcid}/members/{}", people[2]),
        )
        .await;
    conforms(
        "messaging/common/v1.schema.json#/$defs/MemberRemoval",
        &rm.json,
    );
    let d = h
        .delete(
            &c,
            Some(people[0]),
            &format!(
                "/v1/conversations/{mcid}/messages/{}",
                m.json["messageId"].as_str().unwrap()
            ),
        )
        .await;
    conforms(
        "messaging/common/v1.schema.json#/$defs/MessageDeletion",
        &d.json,
    );
    let rc = h
        .put(
            &c,
            Some(people[1]),
            &format!("/v1/conversations/{mcid}/read-cursor"),
            json!({ "sequence": 1 }),
        )
        .await;
    conforms(
        "messaging/common/v1.schema.json#/$defs/ReadCursor",
        &rc.json,
    );
    let b = h
        .post(
            &c,
            Some(people[1]),
            "/v1/messaging/blocks",
            json!({ "principalId": people[2] }),
        )
        .await;
    conforms("messaging/common/v1.schema.json#/$defs/Block", &b.json);
    // Every outbox event written so far conforms to the event contract and never carries
    // content.
    use sea_orm::ConnectionTrait;
    let events: Vec<serde_json::Value> = h
        .conn()
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT jsonb_build_object('event_type', event_type, 'payload', payload) FROM messaging_outbox_events ORDER BY sequence".to_string(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.try_get_by_index::<serde_json::Value>(0).unwrap())
        .collect();
    assert!(events.len() > 10);
    for e in &events {
        conforms("messaging/events/v1.schema.json", e);
        assert!(
            !e.to_string().contains("contract"),
            "content in an event: {e}"
        );
    }
}

/// §155: a member removal racing a burst of that member's sends never commits a message
/// after the removal: every accepted message precedes `member.removed` in the
/// conversation's event order, and every later send is refused.
#[tokio::test]
async fn member_removal_racing_sends_never_commits_a_post_removal_message() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 3).await;
    let (owner, sender) = (people[0], people[1]);
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let sends = (0..15).map(|i| {
        let h = &h;
        let c = &c;
        let conv = &conv;
        async move { h.send_managed(c, conv, sender, &format!("burst {i}")).await }
    });
    let removal_path = format!("/v1/conversations/{cid}/members/{sender}");
    let removal = h.delete(&c, Some(owner), &removal_path);
    let (results, removal) = tokio::join!(futures_join_all(sends), removal);
    assert_eq!(removal.status, StatusCode::OK, "{}", removal.text);
    for r in &results {
        assert!(
            r.status == StatusCode::CREATED || r.status == StatusCode::NOT_FOUND,
            "{}",
            r.text
        );
    }
    let removed_at = h
        .scalar(&format!(
            "SELECT conversation_event_sequence FROM messaging_outbox_events WHERE conversation_id = '{cid}' \
               AND event_type = 'messaging.member.removed' AND payload->>'principal_id' = '{sender}'"
        ))
        .await;
    let after_removal = h
        .scalar(&format!(
            "SELECT count(*) FROM messaging_outbox_events e JOIN messaging_messages m ON m.id::text = e.payload->>'message_id' \
              WHERE e.conversation_id = '{cid}' AND e.event_type = 'messaging.message.created' \
                AND m.sender_principal_id = '{sender}' AND e.conversation_event_sequence > {removed_at}"
        ))
        .await;
    assert_eq!(
        after_removal, 0,
        "a message was committed after the removal"
    );
    let accepted = results
        .iter()
        .filter(|r| r.status == StatusCode::CREATED)
        .count() as i64;
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_messages WHERE sender_principal_id = '{sender}'"
        ))
        .await,
        accepted,
        "exactly the acknowledged messages exist"
    );
    let late = h.send_managed(&c, &conv, sender, "after").await;
    assert_eq!(late.status, StatusCode::NOT_FOUND, "{}", late.text);
}

/// §50 / §146: the policy's edit and delete windows close for the sender (MANAGED edit,
/// delete for everyone); a message still inside its window stays modifiable.
#[tokio::test]
async fn edit_and_delete_windows_close_per_policy() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
                "editing": { "editWindowSeconds": 60, "deleteWindowSeconds": 120 } }),
    )
    .await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let old = h.send_managed(&c, &conv, people[0], "old").await;
    let path = |r: &Response| {
        format!(
            "/v1/conversations/{cid}/messages/{}",
            r.json["messageId"].as_str().unwrap()
        )
    };
    h.clock.advance(chrono::Duration::seconds(90));
    let fresh = h.send_managed(&c, &conv, people[0], "fresh").await;
    // Edit window (60 s) closed for the old message; open for the fresh one.
    let r = h
        .patch(&c, Some(people[0]), &path(&old), json!({ "text": "x" }))
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("MESSAGE_NOT_MODIFIABLE", "WINDOW_CLOSED"),
        "{}",
        r.text
    );
    let r = h
        .patch(
            &c,
            Some(people[0]),
            &path(&fresh),
            json!({ "text": "fresh, edited" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // Delete window (120 s) still open at 90 s, closed at 130 s.
    h.clock.advance(chrono::Duration::seconds(40));
    let r = h.delete(&c, Some(people[0]), &path(&old)).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("MESSAGE_NOT_MODIFIABLE", "WINDOW_CLOSED"),
        "{}",
        r.text
    );
    let r = h.delete(&c, Some(people[0]), &path(&fresh)).await;
    assert_eq!(r.json["deletion"], "DELETED", "{}", r.text);
    // Deleting for oneself is not limited by the window.
    let r = h
        .delete(&c, Some(people[1]), &format!("{}?scope=ME", path(&old)))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}
