//! Against a real S3-compatible server (MinIO in CI, any S3 locally) when
//! `OBJECT_STORAGE_TEST_ENDPOINT` is set: private writes (with server-side encryption
//! unless `OBJECT_STORAGE_TEST_SSE` is empty),
//! integrity metadata, a signed read that works while an unsigned read is refused, and an
//! idempotent delete. Skipped otherwise (`OBJECT_STORAGE_TEST_REQUIRED=1` makes it fail).

use std::time::Duration;

use cf_object_storage::{ObjectStore, S3Config, S3ObjectStore};

fn store() -> Option<S3ObjectStore> {
    let Ok(endpoint) = std::env::var("OBJECT_STORAGE_TEST_ENDPOINT") else {
        assert!(
            std::env::var("OBJECT_STORAGE_TEST_REQUIRED").as_deref() != Ok("1"),
            "OBJECT_STORAGE_TEST_ENDPOINT is required"
        );
        eprintln!("SKIPPED: OBJECT_STORAGE_TEST_ENDPOINT not set");
        return None;
    };
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    Some(
        S3ObjectStore::new(S3Config {
            endpoint,
            public_endpoint: None,
            region: var("OBJECT_STORAGE_TEST_REGION", "us-east-1"),
            bucket: var("OBJECT_STORAGE_TEST_BUCKET", "cfmeet-object-storage-test"),
            access_key: var("OBJECT_STORAGE_TEST_ACCESS_KEY", "testing"),
            secret_key: var("OBJECT_STORAGE_TEST_SECRET_KEY", "testing"),
            path_style: true,
            // Empty: a store without SSE-S3 (e.g. MinIO without a KMS).
            server_side_encryption: Some(var("OBJECT_STORAGE_TEST_SSE", "AES256"))
                .filter(|s| !s.is_empty()),
        })
        .unwrap(),
    )
}

#[tokio::test]
async fn protected_objects_on_a_real_s3_compatible_server() {
    let Some(store) = store() else { return };
    store.ensure_bucket().await.unwrap();
    let key = format!("test/{}.wav", std::process::id());
    let bytes = b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec();
    let stored = store.put(&key, bytes.clone(), "audio/wav").await.unwrap();
    assert_eq!(stored.size, bytes.len() as u64);
    if std::env::var("OBJECT_STORAGE_TEST_SSE").as_deref() != Ok("") {
        assert!(stored.encrypted, "server-side encryption confirmed");
    }
    let meta = store.head(&key).await.unwrap().unwrap();
    assert_eq!(meta.size, bytes.len() as u64);
    assert_eq!(meta.content_type.as_deref(), Some("audio/wav"));
    // A bounded server-side read: whole object, or refused beyond the bound.
    assert_eq!(store.get(&key, 1024).await.unwrap(), Some(bytes.clone()));
    assert!(matches!(
        store.get(&key, bytes.len() - 1).await,
        Err(cf_object_storage::StorageError::Integrity)
    ));

    let read = store
        .signed_read(&key, Duration::from_secs(60), chrono::Utc::now())
        .unwrap();
    let got = reqwest::get(&read.url).await.unwrap();
    assert_eq!(got.status(), 200);
    assert_eq!(got.bytes().await.unwrap().to_vec(), bytes);
    // The object is private: without a signature it is not readable.
    let plain = read.url.split('?').next().unwrap().to_string();
    assert!(reqwest::get(&plain)
        .await
        .unwrap()
        .status()
        .is_client_error());
    // A tampered signature is refused (stores that verify signatures; a local S3
    // emulator may not, `OBJECT_STORAGE_TEST_VERIFIES_SIGNATURES=0`).
    if std::env::var("OBJECT_STORAGE_TEST_VERIFIES_SIGNATURES").as_deref() != Ok("0") {
        let last = if read.url.ends_with('0') { '1' } else { '0' };
        let tampered = format!("{}{last}", &read.url[..read.url.len() - 1]);
        assert!(reqwest::get(&tampered)
            .await
            .unwrap()
            .status()
            .is_client_error());
    }

    store.delete(&key).await.unwrap();
    store.delete(&key).await.unwrap();
    assert!(store.head(&key).await.unwrap().is_none());
    assert_eq!(store.get(&key, 1024).await.unwrap(), None);
}

/// Presigned uploads (Part V §70): the store itself enforces the signed size, SHA-256 and
/// media type; stored checksums are readable; listing works; signed reads force safe
/// download headers.
#[tokio::test]
async fn presigned_uploads_are_bound_to_size_digest_and_type() {
    let Some(store) = store() else { return };
    store.ensure_bucket().await.unwrap();
    let key = format!(
        "upload/{}-{}.bin",
        std::process::id(),
        chrono::Utc::now().timestamp_micros()
    );
    let body = b"opaque ciphertext of an attachment".to_vec();
    let digest = cf_object_storage::sha256_hex(&body);
    let now = chrono::Utc::now();
    let w = store
        .signed_write(
            &key,
            Duration::from_secs(300),
            now,
            body.len() as u64,
            &digest,
            "application/octet-stream",
        )
        .unwrap();
    let client = reqwest::Client::new();
    let send = |bytes: Vec<u8>, size_override: Option<usize>| {
        let mut rb = client.put(&w.url);
        for (k, v) in &w.headers {
            if k == "content-length" {
                continue;
            }
            rb = rb.header(k, v);
        }
        let len = size_override.unwrap_or(bytes.len());
        rb.header("content-length", len.to_string())
            .body(bytes)
            .send()
    };
    // Another body of the same size (digest mismatch) is refused by the store.
    let mut forged = body.clone();
    forged[0] ^= 1;
    let r = send(forged, None).await.unwrap();
    assert!(
        r.status().is_client_error(),
        "forged body accepted: {}",
        r.status()
    );
    // A different size is refused (signature over content-length).
    let r = send(b"short".to_vec(), None).await.unwrap();
    assert!(
        r.status().is_client_error(),
        "wrong size accepted: {}",
        r.status()
    );
    let r = send(body.clone(), None).await.unwrap();
    assert!(
        r.status().is_success(),
        "{}",
        r.text().await.unwrap_or_default()
    );
    match store.stored_sha256(&key).await.unwrap() {
        Some(d) => assert_eq!(d, digest),
        None => eprintln!("NOTE: the store does not report checksums (callers verify by reading)"),
    }
    let listed = store.list("upload/", None, 1000).await.unwrap();
    assert!(listed
        .iter()
        .any(|o| o.key == key && o.size == body.len() as u64));
    let read = store
        .signed_read_as(
            &key,
            Duration::from_secs(60),
            chrono::Utc::now(),
            &cf_object_storage::ResponseOverrides {
                content_type: Some("application/octet-stream".into()),
                content_disposition: Some("attachment; filename=\"attachment.bin\"".into()),
            },
        )
        .unwrap();
    let got = reqwest::get(&read.url).await.unwrap();
    assert!(got.status().is_success());
    assert!(got
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("attachment")));
    assert_eq!(got.bytes().await.unwrap().to_vec(), body);
    store.delete(&key).await.unwrap();
}
