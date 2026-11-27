//! Updates: status (24 hours, contacts only, ≤ 60 s video segments) and channels.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

/// A raw (non-JSON) request: chunk uploads and downloads.
async fn raw(
    app: &TestApp,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, Vec<u8>) {
    let mut req = axum::http::Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    req = req.header("content-type", "application/octet-stream");
    let resp = app
        .router
        .clone()
        .oneshot(req.body(axum::body::Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, bytes)
}

/// Upload `bytes` as one complete piece; returns the media id.
async fn upload(
    app: &TestApp,
    user: &User,
    purpose: &str,
    kind: &str,
    content_type: &str,
    bytes: &[u8],
    duration_ms: Option<i32>,
) -> Uuid {
    let (s, v) = app
        .post(
            "/api/v2/social/media",
            Auth::Bearer(&user.token),
            json!({ "purpose": purpose, "kind": kind, "contentType": content_type, "sizeBytes": bytes.len(), "durationMs": duration_ms }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["chunkCount"], 1);
    let id = Uuid::parse_str(v["mediaId"].as_str().unwrap()).unwrap();
    let (s, b) = raw(
        app,
        "PUT",
        &format!("/api/v2/social/media/{id}/chunks/0"),
        Some(&user.token),
        bytes.to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{}", String::from_utf8_lossy(&b));
    let (s, v) = app
        .post(
            &format!("/api/v2/social/media/{id}/complete"),
            Auth::Bearer(&user.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["complete"], true);
    id
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRpixels";
const MP4: &[u8] = b"\0\0\0\x18ftypmp42\0\0\0\0mp42isom-video-bytes";

#[tokio::test]
async fn status_media_is_uploaded_checked_and_kept_to_its_owner() {
    if !require_test_redis() {
        return;
    }
    let Some(app) = TestApp::new().await else {
        return;
    };
    let ada = app.user("ada").await;
    let bob = app.user("bob").await;

    // Not signed in; a type that isn't a photo; too large.
    let (s, _) = app
        .post(
            "/api/v2/social/media",
            Auth::None,
            json!({ "purpose": "STATUS", "kind": "IMAGE", "contentType": "image/png", "sizeBytes": 10 }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = app
        .post(
            "/api/v2/social/media",
            Auth::Bearer(&ada.token),
            json!({ "purpose": "STATUS", "kind": "IMAGE", "contentType": "image/svg+xml", "sizeBytes": 10 }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
    let (s, _) = app
        .post(
            "/api/v2/social/media",
            Auth::Bearer(&ada.token),
            json!({ "purpose": "STATUS", "kind": "VIDEO", "contentType": "video/mp4", "sizeBytes": 65 * 1024 * 1024 }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // A piece of the wrong size; someone else's upload; an unfinished upload.
    let (_, v) = app
        .post(
            "/api/v2/social/media",
            Auth::Bearer(&ada.token),
            json!({ "purpose": "STATUS", "kind": "IMAGE", "contentType": "image/png", "sizeBytes": PNG.len() }),
        )
        .await;
    let pending = v["mediaId"].as_str().unwrap().to_string();
    let (s, _) = raw(
        &app,
        "PUT",
        &format!("/api/v2/social/media/{pending}/chunks/0"),
        Some(&ada.token),
        b"short".to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = raw(
        &app,
        "PUT",
        &format!("/api/v2/social/media/{pending}/chunks/0"),
        Some(&bob.token),
        PNG.to_vec(),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = app
        .post(
            &format!("/api/v2/social/media/{pending}/complete"),
            Auth::Bearer(&ada.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");

    // Content that isn't what it says is refused and deleted.
    let (_, v) = app
        .post(
            "/api/v2/social/media",
            Auth::Bearer(&ada.token),
            json!({ "purpose": "STATUS", "kind": "IMAGE", "contentType": "image/png", "sizeBytes": 12 }),
        )
        .await;
    let fake = v["mediaId"].as_str().unwrap().to_string();
    raw(
        &app,
        "PUT",
        &format!("/api/v2/social/media/{fake}/chunks/0"),
        Some(&ada.token),
        b"<html>hi</a>".to_vec(),
    )
    .await;
    let (s, _) = app
        .post(
            &format!("/api/v2/social/media/{fake}/complete"),
            Auth::Bearer(&ada.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // A real photo: its owner reads it back; nobody else does (no contacts here).
    let photo = upload(&app, &ada, "STATUS", "IMAGE", "image/png", PNG, None).await;
    let (s, v) = app
        .get(
            format!("/api/v2/social/media/{photo}"),
            Auth::Bearer(&ada.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["kind"], "IMAGE");
    let (s, bytes) = raw(
        &app,
        "GET",
        &format!("/api/v2/social/media/{photo}/chunks/0"),
        Some(&ada.token),
        vec![],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(bytes, PNG);
    let (s, _) = app
        .get(
            format!("/api/v2/social/media/{photo}"),
            Auth::Bearer(&bob.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = raw(
        &app,
        "GET",
        &format!("/api/v2/social/media/{photo}/chunks/0"),
        Some(&bob.token),
        vec![],
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn statuses_last_a_day_and_videos_are_posted_in_minute_segments() {
    if !require_test_redis() {
        return;
    }
    let Some(app) = TestApp::new().await else {
        return;
    };
    let ada = app.user("ada").await;
    let bob = app.user("bob").await;

    let (s, _) = app.get("/api/v2/social/status/feed", Auth::None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Text, photo with a caption, and a 5-minute video as five one-minute statuses.
    let (s, v) = app
        .post(
            "/api/v2/social/status",
            Auth::Bearer(&ada.token),
            json!({ "kind": "TEXT", "text": "Hello", "background": "#128C7E", "font": 1 }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let text_post = v["postId"].as_str().unwrap().to_string();
    let (s, v) = app
        .post(
            "/api/v2/social/status",
            Auth::Bearer(&ada.token),
            json!({ "kind": "TEXT", "text": "  " }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let photo = upload(&app, &ada, "STATUS", "IMAGE", "image/png", PNG, None).await;
    let (s, v) = app
        .post(
            "/api/v2/social/status",
            Auth::Bearer(&ada.token),
            json!({ "kind": "IMAGE", "mediaId": photo, "caption": "Sunset" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["caption"], "Sunset");
    let video = upload(
        &app,
        &ada,
        "STATUS",
        "VIDEO",
        "video/mp4",
        MP4,
        Some(300_000),
    )
    .await;
    for i in 0..5 {
        let (s, v) = app
            .post(
                "/api/v2/social/status",
                Auth::Bearer(&ada.token),
                json!({ "kind": "VIDEO", "mediaId": video, "startMs": i * 60_000, "endMs": (i + 1) * 60_000 }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    let (s, v) = app
        .post(
            "/api/v2/social/status",
            Auth::Bearer(&ada.token),
            json!({ "kind": "VIDEO", "mediaId": video, "startMs": 0, "endMs": 61_000 }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    // Someone else's upload can't be posted.
    let (s, _) = app
        .post(
            "/api/v2/social/status",
            Auth::Bearer(&bob.token),
            json!({ "kind": "IMAGE", "mediaId": photo }),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Ada sees her seven; Bob (not a contact) sees none of them.
    let (s, v) = app
        .get("/api/v2/social/status/feed", Auth::Bearer(&ada.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["mine"].as_array().unwrap().len(), 7);
    let (_, v) = app
        .get("/api/v2/social/status/feed", Auth::Bearer(&bob.token))
        .await;
    assert_eq!(v["contacts"].as_array().unwrap().len(), 0);
    let (s, _) = app
        .post(
            &format!("/api/v2/social/status/{text_post}/view"),
            Auth::Bearer(&bob.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app
        .post(
            &format!("/api/v2/social/status/{text_post}/view"),
            Auth::Bearer(&ada.token),
            json!({ "liked": true }),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // Viewers: the owner only.
    let (s, v) = app
        .get(
            format!("/api/v2/social/status/{text_post}/viewers"),
            Auth::Bearer(&ada.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        v.as_array().unwrap().is_empty(),
        "your own view isn't listed"
    );
    let (s, _) = app
        .get(
            format!("/api/v2/social/status/{text_post}/viewers"),
            Auth::Bearer(&bob.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Delete one; after 24 hours the rest go with their media.
    let (s, _) = app
        .delete(
            format!("/api/v2/social/status/{text_post}"),
            Auth::Bearer(&bob.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app
        .delete(
            format!("/api/v2/social/status/{text_post}"),
            Auth::Bearer(&ada.token),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    app.scalar(&format!(
        "WITH x AS (UPDATE status_posts SET expires_at = now() - interval '1 second', created_at = now() - interval '25 hours' WHERE user_id = '{}' RETURNING 1) SELECT count(*) FROM x",
        ada.id
    ))
    .await;
    app.scalar(&format!(
        "WITH x AS (UPDATE social_media SET created_at = now() - interval '25 hours' WHERE owner_user_id = '{}' RETURNING 1) SELECT count(*) FROM x",
        ada.id
    ))
    .await;
    backend::services::social::purge_expired(&app.db.conn)
        .await
        .unwrap();
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM status_posts WHERE user_id = '{}'",
            ada.id
        ))
        .await,
        0
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM social_media WHERE owner_user_id = '{}'",
            ada.id
        ))
        .await,
        0,
        "status media is deleted with its posts"
    );
    let (_, v) = app
        .get("/api/v2/social/status/feed", Auth::Bearer(&ada.token))
        .await;
    assert_eq!(v["mine"], json!([]));
}

fn id_of(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap().to_string()
}

#[tokio::test]
async fn channels_are_created_followed_posted_in_and_reacted_to() {
    if !require_test_redis() {
        return;
    }
    let Some(app) = TestApp::new().await else {
        return;
    };
    let ada = app.user("ada").await;
    let bob = app.user("bob").await;
    let missing = Uuid::new_v4();

    let (s, _) = app.get("/api/v2/social/channels", Auth::None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = app
        .get("/api/v2/social/channels/following", Auth::None)
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = app
        .post(
            "/api/v2/social/channels",
            Auth::Bearer(&ada.token),
            json!({ "name": "" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, v) = app
        .post(
            "/api/v2/social/channels",
            Auth::Bearer(&ada.token),
            json!({ "name": "Ada's Garden", "description": "Plants" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["isOwner"], true);
    assert_eq!(v["following"], true);
    let channel = id_of(&v, "channelId");

    // Found in the directory; Bob follows it.
    let (s, v) = app
        .get("/api/v2/social/channels?q=garden", Auth::Bearer(&bob.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["channelId"] == channel.as_str()));
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/api/v2/social/channels/{channel}/follow"),
            Auth::Bearer(&bob.token),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["followerCount"], 2);
    let (s, _) = app
        .call(
            Method::PUT,
            &format!("/api/v2/social/channels/{missing}/follow"),
            Auth::Bearer(&bob.token),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Only the owner edits and posts.
    let (s, _) = app
        .patch(
            &format!("/api/v2/social/channels/{channel}"),
            Auth::Bearer(&bob.token),
            json!({ "name": "Bob's now" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = app
        .patch(
            &format!("/api/v2/social/channels/{channel}"),
            Auth::Bearer(&ada.token),
            json!({ "description": "Plants and trees" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["description"], "Plants and trees");
    let (s, _) = app
        .post(
            &format!("/api/v2/social/channels/{channel}/posts"),
            Auth::Bearer(&bob.token),
            json!({ "text": "Hi" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = app
        .post(
            &format!("/api/v2/social/channels/{channel}/posts"),
            Auth::Bearer(&ada.token),
            json!({ "text": "First post" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let text_post = id_of(&v, "postId");
    let photo = upload(&app, &ada, "CHANNEL", "IMAGE", "image/png", PNG, None).await;
    let (s, v) = app
        .post(
            &format!("/api/v2/social/channels/{channel}/posts"),
            Auth::Bearer(&ada.token),
            json!({ "text": "A rose", "mediaId": photo }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["kind"], "IMAGE");

    // Bob has two unread, reads them (and the channel's photo), and reacts.
    let (s, v) = app
        .get(
            "/api/v2/social/channels/following",
            Auth::Bearer(&bob.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v[0]["unreadCount"], 2);
    let (s, v) = app
        .get(
            format!("/api/v2/social/channels/{channel}/posts"),
            Auth::Bearer(&bob.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v.as_array().unwrap().len(), 2);
    let (s, _) = app
        .get(
            format!("/api/v2/social/channels/{missing}/posts"),
            Auth::Bearer(&bob.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, bytes) = raw(
        &app,
        "GET",
        &format!("/api/v2/social/media/{photo}/chunks/0"),
        Some(&bob.token),
        vec![],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(bytes, PNG);
    let (_, v) = app
        .get(
            format!("/api/v2/social/channels/{channel}"),
            Auth::Bearer(&bob.token),
        )
        .await;
    assert_eq!(v["unreadCount"], 0);
    let (s, _) = app
        .get(
            format!("/api/v2/social/channels/{missing}"),
            Auth::Bearer(&bob.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let reaction = format!("/api/v2/social/channels/{channel}/posts/{text_post}/reaction");
    let (s, _) = app
        .call(
            Method::PUT,
            &reaction,
            Auth::Bearer(&bob.token),
            Some(json!({ "emoji": "abc" })),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = app
        .call(
            Method::PUT,
            &reaction,
            Auth::Bearer(&bob.token),
            Some(json!({ "emoji": "👍" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["myReaction"], "👍");
    assert_eq!(v["reactions"][0]["count"], 1);
    let (s, v) = app.delete(&reaction, Auth::Bearer(&bob.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["myReaction"], Value::Null);
    let (s, _) = app
        .delete(
            format!("/api/v2/social/channels/{channel}/posts/{missing}/reaction"),
            Auth::Bearer(&bob.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Unfollow; delete a post and the channel (owner only).
    let (s, v) = app
        .delete(
            format!("/api/v2/social/channels/{channel}/follow"),
            Auth::Bearer(&bob.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["following"], false);
    let (s, _) = app
        .delete(
            format!("/api/v2/social/channels/{missing}/follow"),
            Auth::Bearer(&bob.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let post_path = format!("/api/v2/social/channels/{channel}/posts/{text_post}");
    let (s, _) = app.delete(&post_path, Auth::Bearer(&bob.token)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app.delete(&post_path, Auth::Bearer(&ada.token)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let channel_path = format!("/api/v2/social/channels/{channel}");
    let (s, _) = app.delete(&channel_path, Auth::Bearer(&bob.token)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app.delete(&channel_path, Auth::Bearer(&ada.token)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = app.get(&channel_path, Auth::Bearer(&ada.token)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
