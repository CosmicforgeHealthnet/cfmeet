//! SSRF policy for customer webhook endpoints.
//!
//! * Registration: [`validate_url`] (parsed by the `url` crate — no home-grown parsing):
//!   `https` only (plain `http` only when the deployment explicitly allows it for local
//!   development), no embedded credentials, no fragment, port 443 or an unprivileged port
//!   (≥ 1024), no local / internal / metadata host names, no prohibited address literals.
//! * Delivery: the URL is validated again before every request, and [`client`] resolves
//!   names with [`GuardedResolver`], which drops every prohibited address — the address
//!   checked IS the address connected to (DNS rebinding safe). Redirects are never
//!   followed and no proxy is used (a proxy would connect on our behalf, unchecked).

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

/// Longest accepted endpoint URL.
pub const MAX_URL_LEN: usize = 2048;

/// What a deployment permits. The default is the strict production policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EndpointPolicy {
    /// Plain `http://` endpoints (local development / certification only).
    pub allow_insecure_http: bool,
    /// Loopback / private / link-local destinations (local development / certification
    /// only; NEVER in a shared or production deployment).
    pub allow_private_networks: bool,
}

impl EndpointPolicy {
    pub const STRICT: EndpointPolicy = EndpointPolicy {
        allow_insecure_http: false,
        allow_private_networks: false,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EndpointError {
    #[error("the endpoint is not a valid absolute URL")]
    Invalid,
    #[error("the endpoint must use https")]
    Scheme,
    #[error("the endpoint must not embed credentials")]
    Credentials,
    #[error("the endpoint port is not permitted")]
    Port,
    #[error("the endpoint host is not permitted")]
    Host,
    #[error("the endpoint resolves to a prohibited address")]
    ProhibitedAddress,
}

impl EndpointError {
    /// Bounded failure category for delivery records / metrics.
    pub fn category(&self) -> &'static str {
        match self {
            EndpointError::ProhibitedAddress | EndpointError::Host => "ENDPOINT_BLOCKED",
            _ => "ENDPOINT_INVALID",
        }
    }
}

/// Host names that always designate the local machine or cloud metadata services.
fn prohibited_name(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    h == "localhost"
        || h.ends_with(".localhost")
        || h.ends_with(".local")
        || h.ends_with(".internal")
        || h == "metadata"
        || h == "instance-data"
        || h.starts_with("metadata.")
}

/// Whether `ip` is outside the public internet (loopback, private, link-local incl. the
/// 169.254.169.254 metadata address, CGNAT, unspecified, multicast, reserved, documentation,
/// IPv4-mapped / NAT64 forms of those, IPv6 ULA / link-local).
pub fn is_prohibited(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => prohibited_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return prohibited_v4(v4);
            }
            let s = v6.segments();
            // NAT64 well-known prefix 64:ff9b::/96 embeds an IPv4 address.
            if s[0] == 0x64 && s[1] == 0xff9b && s[2..6].iter().all(|x| *x == 0) {
                let v4 =
                    Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
                return prohibited_v4(v4);
            }
            v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || v6 == Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)
                || (s[0..6].iter().all(|x| *x == 0)) // IPv4-compatible (deprecated) / ::
        }
    }
}

fn prohibited_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_unspecified()
        || v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_multicast()
        || v4.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24 protocol assignments
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18.0.0/15 benchmarking
        || o[0] >= 240 // reserved / broadcast
}

/// Validates an endpoint URL under `policy` (registration AND before every delivery).
pub fn validate_url(raw: &str, policy: EndpointPolicy) -> Result<Url, EndpointError> {
    if raw.len() > MAX_URL_LEN || raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(EndpointError::Invalid);
    }
    let url = Url::parse(raw).map_err(|_| EndpointError::Invalid)?;
    match url.scheme() {
        "https" => {}
        "http" if policy.allow_insecure_http => {}
        _ => return Err(EndpointError::Scheme),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(EndpointError::Credentials);
    }
    if url.fragment().is_some() {
        return Err(EndpointError::Invalid);
    }
    if let Some(port) = url.port() {
        if port != 443 && port < 1024 && !policy.allow_private_networks {
            return Err(EndpointError::Port);
        }
    }
    match url.host() {
        None => return Err(EndpointError::Invalid),
        Some(Host::Domain(d)) => {
            if prohibited_name(d) && !policy.allow_private_networks {
                return Err(EndpointError::Host);
            }
        }
        Some(Host::Ipv4(ip)) => {
            if is_prohibited(IpAddr::V4(ip)) && !policy.allow_private_networks {
                return Err(EndpointError::ProhibitedAddress);
            }
        }
        Some(Host::Ipv6(ip)) => {
            if is_prohibited(IpAddr::V6(ip)) && !policy.allow_private_networks {
                return Err(EndpointError::ProhibitedAddress);
            }
        }
    }
    Ok(url)
}

type LookupFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<Vec<SocketAddr>>> + Send>>;
/// A name lookup (the system resolver in production; injectable for tests).
pub type Lookup = Arc<dyn Fn(String) -> LookupFuture + Send + Sync>;

fn system_lookup() -> Lookup {
    Arc::new(|host: String| {
        Box::pin(async move { Ok(tokio::net::lookup_host((host.as_str(), 0)).await?.collect()) })
    })
}

/// Resolves host names and returns ONLY permitted addresses (the connection then uses
/// exactly these): a name that (re)binds to a prohibited address cannot be reached.
#[derive(Clone)]
pub struct GuardedResolver {
    policy: EndpointPolicy,
    lookup: Lookup,
}

impl std::fmt::Debug for GuardedResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardedResolver")
            .field("policy", &self.policy)
            .finish()
    }
}

impl GuardedResolver {
    pub fn new(policy: EndpointPolicy) -> Self {
        Self::with_lookup(policy, system_lookup())
    }

    pub fn with_lookup(policy: EndpointPolicy, lookup: Lookup) -> Self {
        Self { policy, lookup }
    }

    /// The filter applied to every resolution result.
    pub fn permit(&self, addrs: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, EndpointError> {
        let permitted: Vec<SocketAddr> = addrs
            .into_iter()
            .filter(|a| self.policy.allow_private_networks || !is_prohibited(a.ip()))
            .collect();
        if permitted.is_empty() {
            Err(EndpointError::ProhibitedAddress)
        } else {
            Ok(permitted)
        }
    }
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let this = self.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            if prohibited_name(&host) && !this.policy.allow_private_networks {
                return Err(
                    Box::new(EndpointError::Host) as Box<dyn std::error::Error + Send + Sync>
                );
            }
            let resolved = (this.lookup)(host)
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            let permitted = this
                .permit(resolved)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            Ok(Box::new(permitted.into_iter()) as Addrs)
        })
    }
}

/// The delivery HTTP client: guarded resolution, no redirects, no proxy, bounded time.
pub fn client(policy: EndpointPolicy, timeout: Duration) -> reqwest::Client {
    client_with(GuardedResolver::new(policy), timeout)
}

/// [`client`] with an explicit resolver (tests inject a rebinding lookup).
pub fn client_with(resolver: GuardedResolver, timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .dns_resolver(Arc::new(resolver))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(timeout)
        .connect_timeout(timeout)
        .user_agent("CF-Meet-Webhooks/1")
        .build()
        .expect("static webhook client configuration")
}

/// Whether a reqwest failure was caused by the guarded resolver refusing an address.
pub fn blocked_by_policy(error: &reqwest::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(e) = source {
        if e.downcast_ref::<EndpointError>().is_some() {
            return true;
        }
        source = e.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strict(u: &str) -> Result<Url, EndpointError> {
        validate_url(u, EndpointPolicy::STRICT)
    }

    #[test]
    fn public_https_endpoints_are_accepted() {
        assert!(strict("https://hooks.example.com/cfmeet").is_ok());
        assert!(strict("https://hooks.example.com:8443/x?y=1").is_ok());
        assert!(strict("https://93.184.216.34/x").is_ok());
    }

    #[test]
    fn ssrf_targets_are_refused() {
        for (u, e) in [
            ("http://hooks.example.com/x", EndpointError::Scheme),
            ("ftp://hooks.example.com/x", EndpointError::Scheme),
            ("file:///etc/passwd", EndpointError::Scheme),
            (
                "https://user:pw@hooks.example.com/x",
                EndpointError::Credentials,
            ),
            ("https://hooks.example.com:22/x", EndpointError::Port),
            ("https://localhost/x", EndpointError::Host),
            ("https://api.localhost/x", EndpointError::Host),
            ("https://metadata.google.internal/x", EndpointError::Host),
            ("https://127.0.0.1/x", EndpointError::ProhibitedAddress),
            ("https://127.1.2.3/x", EndpointError::ProhibitedAddress),
            ("https://10.0.0.5/x", EndpointError::ProhibitedAddress),
            ("https://172.16.1.1/x", EndpointError::ProhibitedAddress),
            ("https://192.168.1.10/x", EndpointError::ProhibitedAddress),
            (
                "https://169.254.169.254/latest/meta-data",
                EndpointError::ProhibitedAddress,
            ),
            ("https://100.64.0.1/x", EndpointError::ProhibitedAddress),
            ("https://0.0.0.0/x", EndpointError::ProhibitedAddress),
            ("https://[::1]/x", EndpointError::ProhibitedAddress),
            ("https://[fd00::1]/x", EndpointError::ProhibitedAddress),
            ("https://[fe80::1]/x", EndpointError::ProhibitedAddress),
            (
                "https://[::ffff:127.0.0.1]/x",
                EndpointError::ProhibitedAddress,
            ),
            (
                "https://[::ffff:169.254.169.254]/x",
                EndpointError::ProhibitedAddress,
            ),
            (
                "https://[64:ff9b::a9fe:a9fe]/x",
                EndpointError::ProhibitedAddress,
            ),
            // Alternative IPv4 spellings are normalised by the URL parser, then refused.
            ("https://2130706433/x", EndpointError::ProhibitedAddress),
            ("https://0x7f.1/x", EndpointError::ProhibitedAddress),
        ] {
            assert_eq!(strict(u).unwrap_err(), e, "{u}");
        }
        assert_eq!(
            strict("https://x.example.com/a#frag").unwrap_err(),
            EndpointError::Invalid
        );
        assert_eq!(strict("not a url").unwrap_err(), EndpointError::Invalid);
    }

    #[test]
    fn the_resolver_filter_drops_prohibited_addresses() {
        let r = GuardedResolver::new(EndpointPolicy::STRICT);
        let public: SocketAddr = "93.184.216.34:443".parse().unwrap();
        let local: SocketAddr = "127.0.0.1:443".parse().unwrap();
        let meta: SocketAddr = "169.254.169.254:80".parse().unwrap();
        assert_eq!(r.permit(vec![local, public]).unwrap(), vec![public]);
        assert_eq!(
            r.permit(vec![local, meta]),
            Err(EndpointError::ProhibitedAddress)
        );
        let dev = GuardedResolver::new(EndpointPolicy {
            allow_insecure_http: true,
            allow_private_networks: true,
        });
        assert_eq!(dev.permit(vec![local]).unwrap(), vec![local]);
    }

    /// DNS rebinding: a public-looking name accepted at registration that resolves to
    /// loopback at connection time is never connected to (the receiver sees nothing),
    /// and redirects to internal targets are never followed.
    #[tokio::test]
    async fn rebinding_and_redirects_never_reach_internal_targets() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                h.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1/admin\r\ncontent-length: 0\r\n\r\n")
                    .await;
            }
        });
        let url = format!("http://rebind.example.com:{port}/hook");
        let http_only = EndpointPolicy {
            allow_insecure_http: true,
            allow_private_networks: false,
        };
        // Registration-time check passes (a public-looking name)...
        assert!(validate_url(&url, http_only).is_ok());
        // ...but at connection time the name now resolves to loopback: refused, no TCP.
        let rebinding: Lookup = Arc::new(move |_host: String| {
            Box::pin(async move { Ok(vec![SocketAddr::from(([127, 0, 0, 1], 0))]) })
        });
        let c = client_with(
            GuardedResolver::with_lookup(http_only, rebinding.clone()),
            Duration::from_secs(2),
        );
        let err = c.post(&url).body("x").send().await.unwrap_err();
        assert!(blocked_by_policy(&err), "{err:?}");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "nothing reached the internal target"
        );
        // A permitted (dev) policy reaches it, and the 302 to an internal URL is returned,
        // never followed.
        let dev = EndpointPolicy {
            allow_insecure_http: true,
            allow_private_networks: true,
        };
        let c = client_with(
            GuardedResolver::with_lookup(dev, rebinding),
            Duration::from_secs(2),
        );
        let r = c.post(&url).body("x").send().await.unwrap();
        assert_eq!(r.status().as_u16(), 302);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "redirect not followed");
    }
}
