//! A minimal RESP client (GET / PING) for reading state the Meeting Service keeps in
//! the shared Redis — the e-mail verification code it would otherwise mail to the user.

use anyhow::{bail, Context};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

/// `redis://[user:pass@]host:port[/db]` -> (host:port, password, db).
fn parse(url: &str) -> anyhow::Result<(String, Option<String>, Option<u32>)> {
    let rest = url
        .strip_prefix("redis://")
        .context("only redis:// URLs are supported by the harness")?;
    let (auth, hostpart) = match rest.rsplit_once('@') {
        Some((a, h)) => (Some(a), h),
        None => (None, rest),
    };
    let (host, db) = match hostpart.split_once('/') {
        Some((h, d)) if !d.is_empty() => (h, Some(d.parse::<u32>().context("redis db index")?)),
        Some((h, _)) => (h, None),
        None => (hostpart, None),
    };
    let password = auth.map(|a| a.rsplit_once(':').map(|(_, p)| p).unwrap_or(a).to_string());
    let host = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:6379")
    };
    Ok((host, password.filter(|p| !p.is_empty()), db))
}

fn command(args: &[&str]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", args.len()).into_bytes();
    for a in args {
        out.extend_from_slice(format!("${}\r\n{a}\r\n", a.len()).as_bytes());
    }
    out
}

async fn reply(r: &mut BufReader<TcpStream>) -> anyhow::Result<Option<String>> {
    let mut line = String::new();
    r.read_line(&mut line).await?;
    let line = line.trim_end();
    match line.chars().next() {
        Some('+') => Ok(Some(line[1..].to_string())),
        Some(':') => Ok(Some(line[1..].to_string())),
        Some('-') => bail!("redis error: {}", &line[1..]),
        Some('$') => {
            let n: i64 = line[1..].parse()?;
            if n < 0 {
                return Ok(None);
            }
            let mut buf = vec![0u8; n as usize + 2];
            r.read_exact(&mut buf).await?;
            buf.truncate(n as usize);
            Ok(Some(String::from_utf8(buf)?))
        }
        _ => bail!("unexpected redis reply {line:?}"),
    }
}

async fn connect(url: &str) -> anyhow::Result<BufReader<TcpStream>> {
    let (host, password, db) = parse(url)?;
    let stream = TcpStream::connect(&host)
        .await
        .with_context(|| format!("connecting to Redis at {host}"))?;
    let mut r = BufReader::new(stream);
    if let Some(p) = password {
        r.get_mut().write_all(&command(&["AUTH", &p])).await?;
        reply(&mut r).await?;
    }
    if let Some(db) = db {
        r.get_mut()
            .write_all(&command(&["SELECT", &db.to_string()]))
            .await?;
        reply(&mut r).await?;
    }
    Ok(r)
}

pub async fn ping(url: &str) -> anyhow::Result<()> {
    let mut r = connect(url).await?;
    r.get_mut().write_all(&command(&["PING"])).await?;
    reply(&mut r).await?;
    Ok(())
}

pub async fn get(url: &str, key: &str) -> anyhow::Result<Option<String>> {
    let mut r = connect(url).await?;
    r.get_mut().write_all(&command(&["GET", key])).await?;
    reply(&mut r).await
}

/// One raw command (`CLIENT PAUSE 5000 ALL`, `CLIENT KILL TYPE normal`, …); returns
/// the simple / integer / bulk reply.
pub async fn run(url: &str, args: &[&str]) -> anyhow::Result<Option<String>> {
    let mut r = connect(url).await?;
    r.get_mut().write_all(&command(args)).await?;
    reply(&mut r).await
}

/// Notification Service N0-8: a verification / reset code for a controlled test account.
/// The Meeting Service stores codes only as `<salt>:<SHA-256(salt || code)>` under
/// `<prefix>:h:<SHA-256(lower(email))>`, so an issued code cannot be read back: it is
/// replaced by a fresh controlled code in the same format (only if a code was issued, `XX`).
/// A plaintext code stored by an older image under `<prefix>:<email>` is still read.
pub async fn controlled_otp(
    url: &str,
    prefix: &str,
    email: &str,
) -> anyhow::Result<Option<String>> {
    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(email.trim().to_lowercase().as_bytes()));
    let hashed = format!("{prefix}:h:{digest}");
    if run(url, &["EXISTS", &hashed]).await?.as_deref() == Some("1") {
        let code = format!("{:06}", rand::random::<u32>() % 1_000_000);
        let sealed = sealed_code(&code, rand::random());
        let set = run(url, &["SET", &hashed, &sealed, "EX", "86400", "XX"]).await?;
        return Ok((set.as_deref() == Some("OK")).then_some(code));
    }
    get(url, &format!("{prefix}:{}", email.to_lowercase())).await
}

/// `<salt hex>:<SHA-256(salt || code) hex>` (the Meeting Service's stored-code format).
pub fn sealed_code(code: &str, salt: [u8; 16]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(salt);
    h.update(code.as_bytes());
    format!("{}:{}", hex::encode(salt), hex::encode(h.finalize()))
}

#[cfg(test)]
mod tests {
    /// Shared vector with services/meeting-service/src/services/otp.rs.
    #[test]
    fn controlled_codes_use_the_meeting_service_format() {
        let salt: [u8; 16] = core::array::from_fn(|i| i as u8);
        assert_eq!(
            super::sealed_code("123456", salt),
            "000102030405060708090a0b0c0d0e0f:99c90773ea9ce66a355bd6a2a6e55122c3caf5a164785c90b1d58c2d63bb8680"
        );
    }

    #[test]
    fn urls_parse() {
        assert_eq!(
            super::parse("redis://127.0.0.1:6379").unwrap(),
            ("127.0.0.1:6379".into(), None, None)
        );
        assert_eq!(
            super::parse("redis://:pw@cache/3").unwrap(),
            ("cache:6379".into(), Some("pw".into()), Some(3))
        );
    }
}
