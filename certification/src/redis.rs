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

#[cfg(test)]
mod tests {
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
