//! The realtime signaling channel over a real WebSocket, recovery, and cross-instance
//! fan-out through PostgreSQL LISTEN / NOTIFY (Part C §8–§10, §73, §87, §122–§124).

mod common;

use std::{sync::Arc, time::Duration};

use axum::http::{Method, StatusCode};
use calling::{
    domain::{EnvironmentKind, ManualClock},
    signaling::{PgNotifySignalBus, RealtimeSignalBus},
};
use common::{
    app::AppWorld, calls::CallWorld, fake_media::FakeMediaProvider, test_runtime, Harness, TestDb,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest, http::HeaderValue, protocol::frame::coding::CloseCode, Message,
    },
    MaybeTlsStream, WebSocketStream,
};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Serves the production router on an ephemeral port.
async fn serve(h: &Harness) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = h.router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("ws://{address}/v1/calling/signaling")
}

async fn open(url: &str, headers: &[(&str, &str)]) -> Result<Socket, u16> {
    let mut request = url.into_client_request().unwrap();
    for (k, v) in headers {
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    match connect_async(request).await {
        Ok((socket, _)) => Ok(socket),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            Err(response.status().as_u16())
        }
        Err(other) => panic!("{other}"),
    }
}

async fn next_json(socket: &mut Socket) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a frame within 5 s")
            .expect("open")
            .unwrap();
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

fn realtime_schema() -> jsonschema::Validator {
    let root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts");
    let read = |rel: &str| -> Value {
        serde_json::from_str(&std::fs::read_to_string(root.join(rel)).unwrap()).unwrap()
    };
    let mut registry = jsonschema::Registry::new();
    for rel in [
        "calling/realtime/v1.schema.json",
        "calling/common/v1.schema.json",
        "platform/common/v1.schema.json",
    ] {
        let doc = read(rel);
        let id = doc["$id"].as_str().unwrap().to_string();
        registry = registry
            .add(id, jsonschema::Resource::from_contents(doc))
            .unwrap();
    }
    let registry = registry.prepare().unwrap();
    jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(&json!({ "$ref": "https://schemas.cfmeet.com/calling/realtime/v1.schema.json" }))
        .unwrap()
}

#[tokio::test]
async fn the_channel_authenticates_before_upgrading_and_delivers_committed_signals() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let url = serve(a.h()).await;
    let schema = realtime_schema();
    let caller = a.device(&a.w.initiator, "ws-caller").await;
    let callee = a.device(&a.w.recipient, "ws-callee").await;
    let token = a.signaling_token(&callee).await;

    // No credential, an API credential, a malformed one: refused before the upgrade.
    assert_eq!(open(&url, &[]).await.unwrap_err(), 401);
    let api = format!("Bearer {}", a.secret());
    assert_eq!(
        open(&url, &[("authorization", &api)]).await.unwrap_err(),
        401
    );
    assert_eq!(
        open(&url, &[("authorization", "Bearer cfsig_x.y.z")])
            .await
            .unwrap_err(),
        401
    );
    let offered = format!("cfmeet.signaling.v1, bearer.{}", a.secret());
    assert_eq!(
        open(&url, &[("sec-websocket-protocol", &offered)])
            .await
            .unwrap_err(),
        401
    );

    let bearer = format!("Bearer {token}");
    let mut socket = open(&url, &[("authorization", &bearer)]).await.unwrap();
    let ready = next_json(&mut socket).await;
    assert_eq!(ready["type"], "signaling.ready");
    assert_eq!(ready["endpointId"], callee.endpoint.to_string());
    assert!(schema.is_valid(&ready), "{ready}");
    // Presence is a hint the channel maintains.
    let r = a
        .get(&format!(
            "/v1/calling/endpoints?principalId={}",
            a.w.recipient.id()
        ))
        .await;
    assert_eq!(r.json["data"][0]["presence"], "ONLINE");

    socket
        .send(Message::Text(json!({ "type": "ping" }).to_string()))
        .await
        .unwrap();
    let pong = next_json(&mut socket).await;
    assert_eq!(pong["type"], "signaling.pong");
    assert!(schema.is_valid(&pong));
    socket
        .send(Message::Text(
            json!({ "type": "accept", "callId": "x" }).to_string(),
        ))
        .await
        .unwrap();
    let error = next_json(&mut socket).await;
    assert_eq!(
        error,
        json!({ "type": "signaling.error", "code": "UNSUPPORTED_MESSAGE" })
    );

    // A committed creation rings this endpoint: call.incoming, stamped for it.
    let r = a
        .create_call("VOICE", &a.w.initiator, &caller, &[&a.w.recipient], "ws-1")
        .await;
    let call = r.json["call"]["callId"].as_str().unwrap().to_string();
    let incoming = next_json(&mut socket).await;
    assert_eq!(incoming["type"], "call.incoming");
    assert_eq!(incoming["callId"], call);
    assert_eq!(
        incoming["target"]["endpointId"],
        callee.endpoint.to_string()
    );
    assert!(schema.is_valid(&incoming), "{incoming}");
    // The state the signal names is already committed.
    let view = a.call_view(call.parse().unwrap()).await;
    assert_eq!(view["state"], incoming["callState"]);
    assert!(view["version"].as_i64().unwrap() >= incoming["callVersion"].as_i64().unwrap());
    for forbidden in ["cfc-", "cfmid_", "token", "secret", a.secret()] {
        assert!(!incoming.to_string().contains(forbidden), "{forbidden}");
    }

    // Closing the channel turns the presence hint off again.
    socket.close(None).await.unwrap();
    let mut presence = Value::Null;
    for _ in 0..100 {
        let r = a
            .get(&format!(
                "/v1/calling/endpoints?principalId={}",
                a.w.recipient.id()
            ))
            .await;
        presence = r.json["data"][0]["presence"].clone();
        if presence == "OFFLINE" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(presence, "OFFLINE");
}

#[tokio::test]
async fn browsers_authenticate_with_the_subprotocol_and_expiry_closes_the_channel() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let url = serve(a.h()).await;
    let callee = a.device(&a.w.recipient, "ws-browser").await;
    let token = a.signaling_token(&callee).await;
    let offered = format!("cfmeet.signaling.v1, bearer.{token}");
    let mut request = url.as_str().into_client_request().unwrap();
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(&offered).unwrap(),
    );
    let (mut socket, response) = connect_async(request).await.unwrap();
    assert_eq!(
        response.headers()["sec-websocket-protocol"],
        "cfmeet.signaling.v1"
    );
    assert_eq!(next_json(&mut socket).await["type"], "signaling.ready");
    // Without the protocol name the bearer protocol is not looked at.
    let bare = format!("bearer.{token}");
    assert_eq!(
        open(&url, &[("sec-websocket-protocol", &bare)])
            .await
            .unwrap_err(),
        401
    );

    // A credential about to expire (service clock): the server closes with 4001.
    a.h().clock.advance(chrono::Duration::seconds(599));
    let bearer = format!("Bearer {token}");
    let mut socket = open(&url, &[("authorization", &bearer)]).await.unwrap();
    assert_eq!(next_json(&mut socket).await["type"], "signaling.ready");
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Close(Some(frame)))) => return Some(frame.code),
                Some(Ok(_)) => continue,
                _ => return None,
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(closed, Some(CloseCode::from(4001)));
    a.h().clock.advance(chrono::Duration::seconds(2));
    assert_eq!(
        open(&url, &[("authorization", &bearer)]).await.unwrap_err(),
        401
    );
}

#[tokio::test]
async fn a_disconnected_endpoint_recovers_from_the_api() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "rec-caller").await;
    let callee = a.device(&a.w.recipient, "rec-callee").await;
    let other = a.device(&a.w.recipient, "rec-other").await;
    let token = a.signaling_token(&callee).await;
    // Nobody is connected: the signal is lost, the state is not.
    let r = a
        .create_call("VIDEO", &a.w.initiator, &caller, &[&a.w.recipient], "rec-1")
        .await;
    let call = r.json["call"]["callId"].as_str().unwrap().to_string();
    let recovered = a
        .as_endpoint(&token, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    assert_eq!(recovered.status, StatusCode::OK);
    let offer = &recovered.json["data"][0];
    assert_eq!(offer["callId"], call);
    assert_eq!(offer["actionable"], true);
    assert_eq!(offer["callKind"], "VIDEO");
    // It never sees another endpoint's offers.
    assert_eq!(recovered.json["data"].as_array().unwrap().len(), 1);
    // Answered elsewhere while away: recovery says so; acting on the stale local offer fails.
    let other_offer = a
        .invitation_for(call.parse().unwrap(), other.endpoint)
        .await
        .unwrap();
    let (_, version) = a.state(call.parse().unwrap()).await;
    assert_eq!(
        a.accept(call.parse().unwrap(), other_offer, &other, version)
            .await
            .status,
        StatusCode::OK
    );
    let recovered = a
        .as_endpoint(&token, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    let offer = &recovered.json["data"][0];
    assert_eq!(offer["status"], "ANSWERED_ELSEWHERE");
    assert_eq!(offer["actionable"], false);
    assert_eq!(offer["callState"], "ANSWERED");
    let r = a
        .as_endpoint(
            &token,
            Method::POST,
            &format!(
                "/v1/calling/invitations/{}/accept",
                offer["invitationId"].as_str().unwrap()
            ),
            Some(json!({ "expectedVersion": offer["callVersion"] })),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
}

/// Two Calling instances on one database: a command executed on instance A reaches an
/// endpoint subscribed on instance B; the state stays owned by the database.
#[tokio::test]
async fn signals_fan_out_across_instances() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let db = Arc::new(db);
    let bus_a = Arc::new(PgNotifySignalBus::start(db.conn.clone()).await.unwrap());
    let bus_b = Arc::new(PgNotifySignalBus::start(db.conn.clone()).await.unwrap());
    let clock = ManualClock::new(common::t0());
    let media = Arc::new(FakeMediaProvider::default());
    let local = Arc::new(calling::signaling::InMemorySignalBus::new());
    let instance_a = Harness::with_runtime(
        db.clone(),
        clock.clone(),
        media.clone(),
        local.clone(),
        test_runtime(Some(media.clone()), bus_a.clone()),
    );
    let a = AppWorld {
        w: CallWorld::in_harness(instance_a, "fanout", EnvironmentKind::Production).await,
    };
    let caller = a.device(&a.w.initiator, "fan-caller").await;
    let callee = a.device(&a.w.recipient, "fan-callee").await;
    let mut on_b = bus_b.subscribe(callee.endpoint);
    let mut on_a = bus_a.subscribe(caller.endpoint);

    let r = a
        .create_call("VOICE", &a.w.initiator, &caller, &[&a.w.recipient], "fan-1")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call = r.json["call"]["callId"].as_str().unwrap().to_string();
    let signal = tokio::time::timeout(Duration::from_secs(5), on_b.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(signal["type"], "call.incoming");
    assert_eq!(signal["callId"], call);
    // Accept on A; B's endpoint and A's caller both hear about it.
    let offer = a
        .invitation_for(call.parse().unwrap(), callee.endpoint)
        .await
        .unwrap();
    let (_, version) = a.state(call.parse().unwrap()).await;
    assert_eq!(
        a.accept(call.parse().unwrap(), offer, &callee, version)
            .await
            .status,
        StatusCode::OK
    );
    let mut seen = Vec::new();
    while let Ok(Some(s)) = tokio::time::timeout(Duration::from_millis(500), on_a.recv()).await {
        seen.push(s["type"].as_str().unwrap().to_string());
    }
    assert!(seen.contains(&"call.accepted".to_string()), "{seen:?}");
    assert!(seen.contains(&"call.answered".to_string()), "{seen:?}");
    // Ordering: every signal carries the Call's version and event sequence.
    let mut on_b_seen = Vec::new();
    while let Ok(Some(s)) = tokio::time::timeout(Duration::from_millis(500), on_b.recv()).await {
        on_b_seen.push((
            s["sequence"].as_i64().unwrap(),
            s["callVersion"].as_i64().unwrap(),
        ));
    }
    let mut sorted = on_b_seen.clone();
    sorted.sort();
    assert_eq!(on_b_seen, sorted, "signals arrive in commit order");
    // Instance B reads the same authoritative state.
    let instance_b = Harness::with_runtime(
        db.clone(),
        clock,
        media.clone(),
        local,
        test_runtime(Some(media), bus_b.clone()),
    );
    let r = instance_b
        .request(
            Method::GET,
            &format!("/v1/calls/{call}"),
            Some(a.secret()),
            None,
        )
        .await;
    assert_eq!(r.json["state"], "ANSWERED");
}
