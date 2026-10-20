//! Fault injection: a cuttable TCP proxy.

use std::{sync::Arc, time::Duration};

/// A TCP gate between a service and one of its dependencies (PostgreSQL, Redis):
/// `cut()` drops every live connection and refuses new ones until `restore()`; nothing
/// else using the same dependency is affected.
#[derive(Clone)]
pub struct TcpGate {
    open: Arc<std::sync::atomic::AtomicBool>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
    pub port: u16,
}

impl TcpGate {
    pub async fn start(target: std::net::SocketAddr) -> TcpGate {
        use std::sync::atomic::Ordering;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gate = TcpGate {
            open: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            port: listener.local_addr().unwrap().port(),
        };
        let g = gate.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else {
                    continue;
                };
                if !g.open.load(Ordering::SeqCst) {
                    drop(inbound);
                    continue;
                }
                let g2 = g.clone();
                let epoch = g.epoch.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await else {
                        return;
                    };
                    let watch = async {
                        loop {
                            if g2.epoch.load(Ordering::SeqCst) != epoch {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    };
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                        _ = watch => {}
                    }
                });
            }
        });
        gate
    }

    pub fn cut(&self) {
        use std::sync::atomic::Ordering;
        self.open.store(false, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    pub fn restore(&self) {
        self.open.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
