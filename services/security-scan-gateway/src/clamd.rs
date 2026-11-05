//! clamd TCP protocol (null-terminated `z` commands): `PING`, `VERSION`, `INSTREAM`.
//!
//! `INSTREAM` is fed frame by frame (4-byte big-endian length + at most [`FRAME`] bytes) as
//! the request body arrives, so the gateway never holds a whole file. Every reply is
//! bounded and parsed fail closed: only an exact `stream: OK` is clean.

use std::time::Duration;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

/// Largest INSTREAM frame sent to the engine.
pub const FRAME: usize = 64 * 1024;
/// Largest reply accepted from the engine.
pub const MAX_REPLY: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Clean,
    Infected(String),
    /// `INSTREAM size limit exceeded` — the engine's own StreamMaxLength.
    SizeLimit,
    /// Any other `... ERROR` reply.
    Error,
    /// Anything that is not a recognized clamd answer.
    Malformed,
}

/// Interprets an INSTREAM reply.
pub fn parse_reply(raw: &str) -> Reply {
    let reply = raw.trim_end_matches(['\0', '\n', '\r']);
    if reply.contains('\0') || reply.contains('\n') {
        return Reply::Malformed;
    }
    let Some(body) = reply.strip_prefix("stream: ") else {
        if reply.contains("size limit exceeded") {
            return Reply::SizeLimit;
        }
        return if reply.ends_with("ERROR") {
            Reply::Error
        } else {
            Reply::Malformed
        };
    };
    if body == "OK" {
        return Reply::Clean;
    }
    if let Some(name) = body.strip_suffix(" FOUND") {
        let name: String = name
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(128)
            .collect();
        let name = name.trim().to_string();
        return if name.is_empty() {
            Reply::Malformed
        } else {
            Reply::Infected(name)
        };
    }
    if body.contains("size limit exceeded") {
        return Reply::SizeLimit;
    }
    if body.ends_with("ERROR") {
        Reply::Error
    } else {
        Reply::Malformed
    }
}

/// Engine and signature database version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineVersion {
    pub engine: String,
    pub signature_version: u32,
    /// Build time of the daily database (unix seconds).
    pub signature_time: i64,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parses `ClamAV 1.4.6/28173/Tue Nov  3 06:27:52 2026`.
pub fn parse_version(raw: &str) -> Option<EngineVersion> {
    let raw = raw.trim_end_matches(['\0', '\n']).trim();
    let rest = raw.strip_prefix("ClamAV ")?;
    let mut parts = rest.splitn(3, '/');
    let engine = parts.next()?.trim();
    let signature_version: u32 = parts.next()?.trim().parse().ok()?;
    let date: Vec<&str> = parts.next()?.split_whitespace().collect();
    if engine.is_empty()
        || engine.len() > 32
        || !engine
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        || date.len() != 5
    {
        return None;
    }
    let month = MONTHS.iter().position(|m| *m == date[1])? as i64 + 1;
    let day: i64 = date[2].parse().ok().filter(|d| (1..=31).contains(d))?;
    let year: i64 = date[4].parse().ok().filter(|y| (2000..=2200).contains(y))?;
    let hms: Vec<i64> = date[3]
        .split(':')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    if hms.len() != 3 || hms[0] > 23 || hms[1] > 59 || hms[2] > 60 {
        return None;
    }
    let signature_time =
        days_from_civil(year, month, day) * 86_400 + hms[0] * 3600 + hms[1] * 60 + hms[2];
    Some(EngineVersion {
        engine: engine.to_string(),
        signature_version,
        signature_time,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("engine unreachable")]
    Unreachable,
    #[error("engine i/o failed")]
    Io,
    #[error("engine reply malformed")]
    Malformed,
}

#[derive(Debug, Clone)]
pub struct Clamd {
    address: String,
    connect_timeout: Duration,
}

async fn read_reply(stream: &mut TcpStream) -> Result<String, BackendError> {
    let mut buf = Vec::with_capacity(128);
    let mut chunk = [0u8; 512];
    loop {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|_| BackendError::Io)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_REPLY {
            return Err(BackendError::Malformed);
        }
        if buf.contains(&0) {
            break;
        }
    }
    if buf.is_empty() {
        return Err(BackendError::Io);
    }
    String::from_utf8(buf).map_err(|_| BackendError::Malformed)
}

impl Clamd {
    pub fn new(address: impl Into<String>, connect_timeout: Duration) -> Self {
        Self {
            address: address.into(),
            connect_timeout,
        }
    }

    async fn connect(&self) -> Result<TcpStream, BackendError> {
        let s = tokio::time::timeout(self.connect_timeout, TcpStream::connect(&self.address))
            .await
            .map_err(|_| BackendError::Unreachable)?
            .map_err(|_| BackendError::Unreachable)?;
        let _ = s.set_nodelay(true);
        Ok(s)
    }

    async fn command(&self, command: &[u8]) -> Result<String, BackendError> {
        let fut = async {
            let mut s = self.connect().await?;
            s.write_all(command).await.map_err(|_| BackendError::Io)?;
            read_reply(&mut s).await
        };
        tokio::time::timeout(Duration::from_secs(10), fut)
            .await
            .map_err(|_| BackendError::Io)?
    }

    pub async fn ping(&self) -> bool {
        matches!(self.command(b"zPING\0").await.as_deref(), Ok(r) if r.trim_end_matches(['\0', '\n']) == "PONG")
    }

    pub async fn version(&self) -> Result<EngineVersion, BackendError> {
        parse_version(&self.command(b"zVERSION\0").await?).ok_or(BackendError::Malformed)
    }

    pub async fn start_instream(&self) -> Result<Instream, BackendError> {
        let mut stream = self.connect().await?;
        stream
            .write_all(b"zINSTREAM\0")
            .await
            .map_err(|_| BackendError::Io)?;
        Ok(Instream { stream })
    }
}

/// One INSTREAM scan in progress.
pub struct Instream {
    stream: TcpStream,
}

impl Instream {
    /// Sends `data` as one or more frames. A write failure usually means the engine closed
    /// the stream early (e.g. its size limit): call [`Instream::reply`] to learn why.
    pub async fn send(&mut self, data: &[u8]) -> Result<(), BackendError> {
        for frame in data.chunks(FRAME) {
            self.stream
                .write_all(&(frame.len() as u32).to_be_bytes())
                .await
                .map_err(|_| BackendError::Io)?;
            self.stream
                .write_all(frame)
                .await
                .map_err(|_| BackendError::Io)?;
        }
        Ok(())
    }

    /// Terminates the stream and reads the verdict.
    pub async fn finish(mut self) -> Result<Reply, BackendError> {
        self.stream
            .write_all(&0u32.to_be_bytes())
            .await
            .map_err(|_| BackendError::Io)?;
        self.reply().await
    }

    /// Reads whatever the engine answered (also after an early close).
    pub async fn reply(mut self) -> Result<Reply, BackendError> {
        Ok(parse_reply(&read_reply(&mut self.stream).await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_exact_ok_is_clean() {
        assert_eq!(parse_reply("stream: OK\0"), Reply::Clean);
        assert_eq!(
            parse_reply("stream: Eicar-Test-Signature FOUND\0"),
            Reply::Infected("Eicar-Test-Signature".into())
        );
        assert_eq!(
            parse_reply("INSTREAM size limit exceeded. ERROR\0"),
            Reply::SizeLimit
        );
        assert_eq!(
            parse_reply("stream: Can't allocate memory ERROR"),
            Reply::Error
        );
        for bad in [
            "",
            "OK",
            "stream: OK extra",
            "stream:  FOUND",
            "stream: OK\0stream: OK",
            "PONG",
            "stream: ok",
        ] {
            assert!(
                !matches!(parse_reply(bad), Reply::Clean | Reply::Infected(_)),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn versions_and_signature_times_parse() {
        let v = parse_version("ClamAV 1.4.6/28173/Tue Nov  3 06:27:52 2026\n").unwrap();
        assert_eq!(v.engine, "1.4.6");
        assert_eq!(v.signature_version, 28173);
        // 2026-11-03T06:27:52Z
        assert_eq!(v.signature_time, 1_793_687_272);
        assert!(parse_version("ClamAV 1.4.6").is_none());
        assert!(parse_version("ClamAV 1.4.6/x/Tue Nov  3 06:27:52 2026").is_none());
        assert!(parse_version("ClamAV 1.4.6/1/Tue Foo  3 06:27:52 2026").is_none());
        assert!(parse_version("clamd 1.4.6/1/Tue Nov  3 06:27:52 2026").is_none());
    }
}
