//! ADR 0069 — the retired channels export: complete for linked people, read-only,
//! deterministic, with whole media and the two-sided checksum; unlinked people are skipped
//! and counted, never guessed.
mod common;

use backend::services::social_export::{checksum, export, ExportScope};
use base64::Engine as _;
use common::*;
use sea_orm::ConnectionTrait;
use uuid::Uuid;

async fn sql(app: &TestApp, q: String) {
    app.db.conn.execute_unprepared(&q).await.unwrap();
}

#[tokio::test]
async fn channels_export_with_principals_media_and_checksum_without_writing() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let owner = app.user("chan-owner").await;
    let reader = app.user("chan-reader").await;
    let stranger = app.user("chan-stranger").await; // never used Messaging: no principal
    let env = Uuid::new_v4();
    let (p_owner, p_reader) = (Uuid::new_v4(), Uuid::new_v4());
    for (user, p) in [(owner.id, p_owner), (reader.id, p_reader)] {
        sql(&app, format!(
            "INSERT INTO communication_principal_links (subject_kind, subject_id, environment_id, principal_id, principal_kind) \
             VALUES ('USER', '{user}', '{env}', '{p}', 'NATIVE_CF_USER')"
        ))
        .await;
    }
    let (channel, orphan) = (Uuid::new_v4(), Uuid::new_v4());
    let (text_post, media_post, stranger_post, half_post) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let (media, half) = (Uuid::new_v4(), Uuid::new_v4());
    let png: Vec<u8> = [b"\x89PNG\r\n\x1a\n".as_slice(), &[7u8; 600 * 1024]].concat();
    let (first, second) = png.split_at(512 * 1024);
    sql(
        &app,
        format!(
            "INSERT INTO social_channels (id, owner_user_id, name, description) VALUES \
         ('{channel}', '{}', 'News', 'Daily'), ('{orphan}', '{}', 'Hidden', NULL)",
            owner.id, stranger.id
        ),
    )
    .await;
    sql(
        &app,
        format!(
            "INSERT INTO social_channel_followers (channel_id, user_id) VALUES \
         ('{channel}', '{}'), ('{channel}', '{}'), ('{channel}', '{}')",
            owner.id, reader.id, stranger.id
        ),
    )
    .await;
    sql(&app, format!(
        "INSERT INTO social_media (id, owner_user_id, purpose, kind, content_type, size_bytes, chunk_count, completed_at) VALUES \
         ('{media}', '{}', 'CHANNEL', 'IMAGE', 'image/png', {}, 2, now()), \
         ('{half}', '{}', 'CHANNEL', 'IMAGE', 'image/png', 10, 1, NULL)",
        owner.id,
        png.len(),
        owner.id
    ))
    .await;
    for (seq, bytes) in [(0, first), (1, second)] {
        sql(&app, format!(
            "INSERT INTO social_media_chunks (media_id, seq, bytes) VALUES ('{media}', {seq}, decode('{}', 'hex'))",
            hex::encode(bytes)
        ))
        .await;
    }
    sql(&app, format!(
        "INSERT INTO social_channel_posts (id, channel_id, author_user_id, body_text, media_id, kind) VALUES \
         ('{text_post}', '{channel}', '{0}', 'hello', NULL, 'TEXT'), \
         ('{media_post}', '{channel}', '{0}', NULL, '{media}', 'IMAGE'), \
         ('{half_post}', '{channel}', '{0}', NULL, '{half}', 'IMAGE'), \
         ('{stranger_post}', '{orphan}', '{1}', 'x', NULL, 'TEXT')",
        owner.id, stranger.id
    ))
    .await;
    sql(
        &app,
        format!(
            "INSERT INTO social_channel_reactions (post_id, user_id, emoji) VALUES \
         ('{text_post}', '{}', '👍'), ('{text_post}', '{}', '❤')",
            reader.id, stranger.id
        ),
    )
    .await;

    let count = "SELECT (SELECT count(*) FROM social_channels) + (SELECT count(*) FROM social_channel_posts) \
                 + (SELECT count(*) FROM social_channel_followers) + (SELECT count(*) FROM social_media)";
    let before = app.scalar(count).await;
    let scope = ExportScope {
        tenant_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        environment_id: env,
    };
    let doc = export(&app.db.conn, scope.clone()).await.unwrap();
    assert_eq!(app.scalar(count).await, before, "the export never writes");

    assert_eq!(doc.channels.len(), 1);
    assert_eq!(doc.channels[0].id, channel);
    assert_eq!(doc.channels[0].owner_principal_id, p_owner);
    let mut follower_principals: Vec<Uuid> = doc.followers.iter().map(|f| f.principal_id).collect();
    follower_principals.sort();
    let mut expected = vec![p_owner, p_reader];
    expected.sort();
    assert_eq!(follower_principals, expected);
    let ids: Vec<Uuid> = doc.posts.iter().map(|p| p.id).collect();
    assert!(ids.contains(&text_post) && ids.contains(&media_post));
    assert!(
        !ids.contains(&half_post),
        "a media-only post whose media is unfinished is skipped"
    );
    assert!(!ids.contains(&stranger_post));
    assert_eq!(doc.reactions.len(), 1);
    assert_eq!(doc.reactions[0].principal_id, p_reader);
    assert_eq!(doc.media.len(), 1);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&doc.media[0].data)
        .unwrap();
    assert_eq!(bytes, png, "media is exported whole and in order");
    assert_eq!(doc.skipped.unlinked_people, 1);
    assert_eq!(doc.skipped.channels, 1);
    assert_eq!(doc.skipped.followers, 1);
    assert_eq!(doc.skipped.media, 1);
    assert_eq!(doc.checksum, checksum(&doc));
    assert_eq!(doc.scope, scope);

    // Deterministic: a second export of unchanged data has the same checksum.
    assert_eq!(
        export(&app.db.conn, scope).await.unwrap().checksum,
        doc.checksum
    );
}
