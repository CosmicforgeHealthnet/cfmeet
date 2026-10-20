//! Self-hosted malware scanning of MANAGED attachments (§72).
//!
//! [`ClamAvScanner`] speaks clamd's `INSTREAM` protocol over TCP
//! (`MESSAGING_CLAMAV_ADDRESS`): `zINSTREAM\0`, then 4-byte big-endian length-prefixed
//! chunks, a zero-length terminator, and a `stream: OK` / `stream: <name> FOUND` reply.
//! No external scanning service is ever required.
//!
//! Policy (fail closed): where scanning is required, an attachment becomes READY only
//! after a CLEAN result; an unavailable scanner leaves it VERIFYING (retried, never
//! downloadable). PRIVATE_E2EE objects are ciphertext: they are NEVER sent to a scanner
//! and never reported as scanned (`NOT_APPLICABLE_ENCRYPTED`).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanOutcome {
    Clean,
    Infected(String),
    /// The scanner could not give an answer (retry later; never treated as clean).
    Unavailable(String),
}

#[async_trait::async_trait]
pub trait AttachmentScanner: Send + Sync {
    fn engine(&self) -> &'static str;
    fn configured(&self) -> bool {
        true
    }
    async fn scan(&self, bytes: &[u8]) -> ScanOutcome;
}

pub struct UnconfiguredScanner;

#[async_trait::async_trait]
impl AttachmentScanner for UnconfiguredScanner {
    fn engine(&self) -> &'static str {
        "none"
    }

    fn configured(&self) -> bool {
        false
    }

    async fn scan(&self, _bytes: &[u8]) -> ScanOutcome {
        ScanOutcome::Unavailable("no scanner configured".into())
    }
}

pub struct ClamAvScanner {
    address: String,
    timeout: Duration,
}

impl ClamAvScanner {
    pub fn new(address: impl Into<String>, timeout: Duration) -> Self {
        Self {
            address: address.into(),
            timeout,
        }
    }

    async fn run(&self, bytes: &[u8]) -> std::io::Result<String> {
        let mut s = tokio::net::TcpStream::connect(&self.address).await?;
        s.write_all(b"zINSTREAM\0").await?;
        for chunk in bytes.chunks(64 * 1024) {
            s.write_all(&(chunk.len() as u32).to_be_bytes()).await?;
            s.write_all(chunk).await?;
        }
        s.write_all(&0u32.to_be_bytes()).await?;
        let mut reply = Vec::new();
        s.read_to_end(&mut reply).await?;
        Ok(String::from_utf8_lossy(&reply)
            .trim_end_matches(['\0', '\n'])
            .to_string())
    }
}

/// Interprets a clamd reply.
pub fn parse_reply(reply: &str) -> ScanOutcome {
    let body = reply
        .strip_prefix("stream:")
        .map(str::trim)
        .unwrap_or(reply.trim());
    if body == "OK" {
        ScanOutcome::Clean
    } else if let Some(name) = body.strip_suffix("FOUND") {
        let name: String = name
            .trim()
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(128)
            .collect();
        ScanOutcome::Infected(if name.is_empty() {
            "UNKNOWN".into()
        } else {
            name
        })
    } else {
        ScanOutcome::Unavailable(format!(
            "scanner error: {}",
            body.chars().take(80).collect::<String>()
        ))
    }
}

#[async_trait::async_trait]
impl AttachmentScanner for ClamAvScanner {
    fn engine(&self) -> &'static str {
        "clamav"
    }

    async fn scan(&self, bytes: &[u8]) -> ScanOutcome {
        match tokio::time::timeout(self.timeout, self.run(bytes)).await {
            Ok(Ok(reply)) => parse_reply(&reply),
            Ok(Err(e)) => ScanOutcome::Unavailable(format!("scanner unreachable: {}", e.kind())),
            Err(_) => ScanOutcome::Unavailable("scanner timeout".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamd_replies_are_interpreted_fail_closed() {
        assert_eq!(parse_reply("stream: OK"), ScanOutcome::Clean);
        assert_eq!(
            parse_reply("stream: Eicar-Test-Signature FOUND"),
            ScanOutcome::Infected("Eicar-Test-Signature".into())
        );
        assert!(matches!(
            parse_reply("INSTREAM size limit exceeded. ERROR"),
            ScanOutcome::Unavailable(_)
        ));
        assert!(matches!(parse_reply(""), ScanOutcome::Unavailable(_)));
    }

    #[tokio::test]
    async fn an_unreachable_scanner_is_unavailable_never_clean() {
        let s = ClamAvScanner::new("127.0.0.1:1", Duration::from_millis(500));
        assert!(matches!(s.scan(b"x").await, ScanOutcome::Unavailable(_)));
        assert!(matches!(
            UnconfiguredScanner.scan(b"x").await,
            ScanOutcome::Unavailable(_)
        ));
    }
}
