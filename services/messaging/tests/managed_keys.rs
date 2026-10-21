//! Part V Appendix J: MANAGED content uses envelope encryption — a per-environment DEK
//! wrapped by the key provider's KEK; PostgreSQL holds no usable key.

mod common;

use std::sync::Arc;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use common::{flows::*, *};
use messaging::messages::managed::{self, DecryptPurpose, ManagedKeyring};
use uuid::Uuid;

fn ring_text(seed: u8) -> String {
    format!("kek-1 {}\n", URL_SAFE_NO_PAD.encode([seed; 32]))
}

#[tokio::test]
async fn managed_content_is_sealed_under_wrapped_per_environment_keys() {
    let Some(h) = Harness::build(|s| {
        s.managed_keys = Some(Arc::new(ManagedKeyring::parse(&ring_text(1)).unwrap()))
    })
    .await
    else {
        return;
    };
    let a = h.customer(ALL_SCOPES);
    let b = h.customer(ALL_SCOPES);
    let (ca, pa) = h.managed_group(&a, 1).await;
    let (cb, pb) = h.managed_group(&b, 1).await;
    let ma = h.send_managed(&a, &ca, pa[0], "tenant a secret").await;
    h.send_managed(&b, &cb, pb[0], "tenant b secret").await;
    h.send_managed(&a, &ca, pa[0], "tenant a again").await;

    // One wrapped DEK per environment; content names it; nothing usable is in the row.
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_managed_deks WHERE status = 'ACTIVE'")
            .await,
        2
    );
    assert_eq!(
        h.scalar("SELECT count(DISTINCT managed_seal_key_id) FROM messaging_messages WHERE security_profile = 'MANAGED' AND message_kind = 'APPLICATION'").await,
        2,
        "one key per environment"
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_messages WHERE security_profile = 'MANAGED' AND managed_sealed IS NOT NULL AND managed_seal_key_id NOT LIKE 'dek:%'").await,
        0
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_managed_deks WHERE kek_id = 'file:kek-1' AND octet_length(wrapped_dek) = 48").await,
        2,
        "wrapped (32-byte key + tag), never stored in the clear"
    );

    // A fresh process with the same key provider unwraps the DEK from the database ...
    let mid = Uuid::parse_str(ma.json["messageId"].as_str().unwrap()).unwrap();
    let cid = Uuid::parse_str(ca["conversationId"].as_str().unwrap()).unwrap();
    let row = messaging::messages::load_message(&h.state.db, cid, mid, false)
        .await
        .unwrap()
        .unwrap();
    let mut restarted = h.state.clone();
    restarted.managed_keys = Some(Arc::new(ManagedKeyring::parse(&ring_text(1)).unwrap()));
    assert_eq!(
        managed::text_of(&restarted, &row, DecryptPurpose::MemberRead)
            .await
            .as_deref(),
        Some("tenant a secret")
    );
    // ... while the database alone (another KEK) opens nothing.
    let mut stolen = h.state.clone();
    stolen.managed_keys = Some(Arc::new(ManagedKeyring::parse(&ring_text(2)).unwrap()));
    assert_eq!(
        managed::text_of(&stolen, &row, DecryptPurpose::MemberRead).await,
        None
    );

    // Wrapped keys are immutable and never deleted.
    assert!(h
        .sql("UPDATE messaging_managed_deks SET kek_id = 'file:other'")
        .await
        .is_err());
    assert!(h.sql("DELETE FROM messaging_managed_deks").await.is_err());
    // A second ACTIVE key for an environment is refused.
    assert!(h
        .sql("INSERT INTO messaging_managed_deks SELECT gen_random_uuid(), tenant_id, application_id, environment_id, kek_id, wrap_nonce, wrapped_dek, 'ACTIVE', now() FROM messaging_managed_deks LIMIT 1")
        .await
        .is_err());
}
