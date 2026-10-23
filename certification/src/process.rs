//! Child processes of the stack: every service writes stdout + stderr to its own log
//! file; failures print the tail of the relevant logs.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::bail;

/// One running service process.
pub struct Service {
    pub name: String,
    pub log: PathBuf,
    pub child: tokio::process::Child,
}

/// The last `lines` lines of a log file.
pub fn tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// What the stack hands a child: a clean environment (PATH only) plus `env`.
pub struct Launch<'a> {
    pub name: &'a str,
    pub bin: &'a Path,
    pub env: Vec<(String, String)>,
    pub dir: &'a Path,
    pub log_dir: &'a Path,
    /// HTTP readiness URL (`None`: the process only has to stay up for a moment).
    pub ready: Option<String>,
}

pub async fn spawn(launch: Launch<'_>, http: &reqwest::Client) -> anyhow::Result<Service> {
    let log = launch.log_dir.join(format!("{}.log", launch.name));
    let out = std::fs::File::create(&log)?;
    let err = out.try_clone()?;
    let mut c = tokio::process::Command::new(launch.bin);
    c.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(
            "RUST_LOG",
            std::env::var("CFMEET_SERVICE_LOG").unwrap_or_else(|_| "warn".into()),
        )
        .env("LOG_FORMAT", "pretty")
        .env("NO_COLOR", "1")
        .current_dir(launch.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .kill_on_drop(true);
    for (k, v) in &launch.env {
        c.env(k, v);
    }
    let mut child = match c.spawn() {
        Ok(c) => c,
        Err(e) => bail!(
            "{} ({}) failed to start: {e}",
            launch.name,
            launch.bin.display()
        ),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            bail!(
                "{} exited during startup ({status}); log {}:\n{}",
                launch.name,
                log.display(),
                tail(&log, 60)
            );
        }
        match &launch.ready {
            Some(url) => {
                if let Ok(r) = http.get(url).send().await {
                    if r.status().is_success() {
                        break;
                    }
                }
            }
            None => {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if let Ok(Some(status)) = child.try_wait() {
                    bail!(
                        "{} exited during startup ({status}); log {}:\n{}",
                        launch.name,
                        log.display(),
                        tail(&log, 60)
                    );
                }
                break;
            }
        }
        if std::time::Instant::now() > deadline {
            let _ = child.start_kill();
            bail!(
                "{} did not become ready at {:?} within 60 s; log {}:\n{}",
                launch.name,
                launch.ready,
                log.display(),
                tail(&log, 60)
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(Service {
        name: launch.name.to_string(),
        log,
        child,
    })
}
