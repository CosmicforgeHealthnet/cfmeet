use communications::{config::Config, store::Store, Metrics, State};
use sea_orm::{ConnectOptions, Database};
use secrecy::ExposeSecret;
use std::{sync::Arc, time::Duration};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "communications=info".into()))
        .init();
    let config = Arc::new(Config::from_env().map_err(anyhow::Error::msg)?);
    let mut options = ConnectOptions::new(config.database_url.expose_secret().to_owned());
    options
        .max_connections(12)
        .min_connections(1)
        .connect_timeout(Duration::from_secs(5))
        .sqlx_logging(false);
    let db = Database::connect(options)
        .await
        .map_err(|_| anyhow::anyhow!("Communications database connection failed"))?;
    if !communications_migration::ready(&db).await.unwrap_or(false) {
        anyhow::bail!("Communications migrations must be applied with the separate migration role");
    }
    let state = State {
        store: Store { db },
        config: config.clone(),
        metrics: Arc::new(Metrics::default()),
    };
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let worker_state = state.clone();
    let worker = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut count = 0;
        loop {
            tokio::select! {
                _=stopped.changed()=>break,
                _=tick.tick()=>{
                    if worker_state.deliver_due().await.is_err() {tracing::warn!("communications outbox iteration deferred");}
                    count+=1;if count%120==0 && worker_state.maintain().await.is_err() {tracing::warn!("communications retention iteration deferred");}
                    // Every 5 s: call cards converge on Calling's canonical outcome.
                    if count%10==0 && worker_state.reconcile_call_cards(20).await.is_err() {tracing::warn!("communications call-card reconciliation deferred");}
                }
            }
        }
    });
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(
        service = "communications",
        "private Communications listener ready"
    );
    axum::serve(listener, communications::api::app(state))
        .with_graceful_shutdown(async move {
            #[cfg(unix)]
            {
                let mut terminate =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("signal handler");
                tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
            let _ = stop.send(true);
        })
        .await?;
    let _ = tokio::time::timeout(Duration::from_secs(15), worker).await;
    Ok(())
}
