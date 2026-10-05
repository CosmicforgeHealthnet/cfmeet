//! A toggleable TCP proxy for dependency outage *and recovery* tests (Part M §29–§31).
//! `down()` drops every live connection and refuses new ones; `up()` serves again. The
//! application keeps its configured address throughout, exactly like a real outage.
#![allow(dead_code)]

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use tokio::{
    net::{TcpListener, TcpStream},
    sync::watch,
};

pub struct ToggleProxy {
    pub port: u16,
    up: Arc<AtomicBool>,
    generation: watch::Sender<u64>,
    counter: Arc<AtomicU64>,
}

impl ToggleProxy {
    /// Forward `127.0.0.1:<port>` → `target` (`host:port`).
    pub async fn start(target: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let up = Arc::new(AtomicBool::new(true));
        let (generation, _) = watch::channel(0u64);
        let counter = Arc::new(AtomicU64::new(0));
        let (up2, gen_rx, counter2) = (up.clone(), generation.subscribe(), counter.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else {
                    continue;
                };
                if !up2.load(Ordering::SeqCst) {
                    drop(inbound);
                    continue;
                }
                counter2.fetch_add(1, Ordering::SeqCst);
                let target = target.clone();
                let mut kill = gen_rx.clone();
                let started = *kill.borrow();
                tokio::spawn(async move {
                    let Ok(mut outbound) = TcpStream::connect(&target).await else {
                        return;
                    };
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                        _ = async {
                            loop {
                                if kill.changed().await.is_err() || *kill.borrow() != started {
                                    break;
                                }
                            }
                        } => {}
                    }
                });
            }
        });
        Self {
            port,
            up,
            generation,
            counter,
        }
    }

    pub fn down(&self) {
        self.up.store(false, Ordering::SeqCst);
        self.generation.send_modify(|g| *g += 1);
    }

    pub fn up(&self) {
        self.up.store(true, Ordering::SeqCst);
    }

    pub fn connections(&self) -> u64 {
        self.counter.load(Ordering::SeqCst)
    }
}

/// `host:port` of `TEST_REDIS_URL`.
pub fn redis_target() -> String {
    let url = std::env::var("TEST_REDIS_URL").unwrap_or_default();
    url.trim_start_matches("redis://")
        .split('/')
        .next()
        .unwrap_or("127.0.0.1:6379")
        .to_string()
}
