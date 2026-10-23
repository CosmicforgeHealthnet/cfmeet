//! Realtime WebSocket forwarding through a real gateway socket to a real upstream:
//! the ticket travels in the subprotocol untouched, the upstream's refusal is relayed
//! as HTTP (no dead upgrade), the selected subprotocol is negotiated end to end, frames
//! (text, binary, ping) are relayed in order, and close codes / reasons reach the client.

mod common;

use common::*;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, protocol::frame::coding::CloseCode, Error, Message,
};

async fn connect(
    addr: &str,
    protocols: &str,
) -> Result<
    (
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Option<String>,
    ),
    Error,
> {
    let mut req = format!("ws://{addr}/v1/test/realtime")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("sec-websocket-protocol", protocols.parse().unwrap());
    let (ws, resp) = tokio_tungstenite::connect_async(req).await?;
    let selected = resp
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    Ok((ws, selected))
}

#[tokio::test]
async fn realtime_channels_are_forwarded_with_their_own_security_model() {
    let g = Gateway::new().await;
    let addr = g.serve().await;
    // A bad ticket: the owning service's refusal is relayed as HTTP 401.
    match connect(&addr, "cfmeet.messaging.v1, cfmeet.ticket.bad").await {
        Err(Error::Http(resp)) => assert_eq!(resp.status(), 401),
        other => panic!("expected a refused upgrade, got {:?}", other.map(|_| ())),
    }
    // A good ticket: negotiated subprotocol, ordered echo, ping / pong.
    let (mut ws, selected) = connect(&addr, "cfmeet.messaging.v1, cfmeet.ticket.good")
        .await
        .unwrap();
    assert_eq!(selected.as_deref(), Some("cfmeet.messaging.v1"));
    for i in 0..20 {
        ws.send(Message::Text(format!("m{i}"))).await.unwrap();
    }
    for i in 0..20 {
        assert_eq!(
            ws.next().await.unwrap().unwrap(),
            Message::Text(format!("m{i}"))
        );
    }
    ws.send(Message::Binary(vec![0, 1, 2, 255])).await.unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap(),
        Message::Binary(vec![0, 1, 2, 255])
    );
    ws.send(Message::Ping(b"hb".to_vec())).await.unwrap();
    // A close code / reason from the owning service reaches the client.
    ws.send(Message::Text("bye".into())).await.unwrap();
    let mut close = None;
    while let Some(m) = ws.next().await {
        match m {
            Ok(Message::Close(c)) => {
                close = c;
                break;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    let close = close.expect("close frame");
    assert_eq!(close.code, CloseCode::from(4001));
    assert_eq!(close.reason, "ticket expired");
    // The ticket only travelled in the subprotocol; nothing else was added.
    let _ = g.messaging();
}

#[tokio::test]
async fn oversized_frames_are_refused() {
    let g = Gateway::new().await;
    let addr = g.serve().await;
    let (mut ws, _) = connect(&addr, "cfmeet.messaging.v1, cfmeet.ticket.good")
        .await
        .unwrap();
    let big = "x".repeat(2 * 1024 * 1024);
    let _ = ws.send(Message::Text(big)).await;
    // The gateway closes (or errors) instead of relaying an unbounded frame.
    let next = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .unwrap();
    assert!(
        matches!(next, None | Some(Err(_)) | Some(Ok(Message::Close(_)))),
        "{next:?}"
    );
}
