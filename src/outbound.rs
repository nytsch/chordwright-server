//! Requests from the server to services elsewhere: Anthropic for the
//! assistant (assistant.rs), ChurchTools (churchtools.rs). One request per
//! connection, http/1.1, https with the usual web roots.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::Request;
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::net::TcpStream;

/// `scheme://host:port/prefix`, taken apart: (https, host, port, prefix).
pub fn endpoint_of(url: &str) -> Result<(bool, String, u16, String), String> {
    let (tls, rest) = match url.split_once("://") {
        Some(("https", rest)) => (true, rest),
        Some(("http", rest)) => (false, rest),
        _ => return Err(format!("{url}: not an http(s) address")),
    };
    let (authority, prefix) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|_| format!("{url}: bad port"))?),
        None => (authority, if tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(format!("{url}: no host"));
    }
    let prefix = prefix.trim_end_matches('/');
    let prefix = if prefix.is_empty() {
        String::new()
    } else {
        format!("/{prefix}")
    };
    Ok((tls, host.to_string(), port, prefix))
}

fn tls_connector() -> tokio_rustls::TlsConnector {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    let config = CONFIG.get_or_init(|| {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(config)
    });
    tokio_rustls::TlsConnector::from(config.clone())
}

/// What came back: the status, the content type, the body.
pub struct Reply {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Bytes,
}

impl Reply {
    /// The body as JSON; Null when it is none.
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// One request to `base` (`scheme://host:port/prefix`) at `path`, which
/// starts with `/`; `host` is set here. At most `max` bytes come back.
pub async fn fetch(
    base: &str,
    request: hyper::http::request::Builder,
    path: &str,
    body: Bytes,
    timeout: Duration,
    max: usize,
) -> Result<Reply, String> {
    let (tls, host, port, prefix) = endpoint_of(base)?;
    let authority = match (tls, port) {
        (true, 443) | (false, 80) => host.clone(),
        _ => format!("{host}:{port}"),
    };
    let request = request
        .uri(format!("{prefix}{path}"))
        .header("host", authority)
        .body(Full::new(body))
        .map_err(|e| e.to_string())?;
    let work = async {
        let stream = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| e.to_string())?;
        if tls {
            let name =
                rustls_pki_types::ServerName::try_from(host.clone()).map_err(|e| e.to_string())?;
            let stream = tls_connector()
                .connect(name, stream)
                .await
                .map_err(|e| e.to_string())?;
            send(TokioIo::new(stream), request, max).await
        } else {
            send(TokioIo::new(stream), request, max).await
        }
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| "timed out".to_string())?
}

async fn send<T>(io: TokioIo<T>, request: Request<Full<Bytes>>, max: usize) -> Result<Reply, String>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(conn);
    let response = sender
        .send_request(request)
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body = Limited::new(response.into_body(), max)
        .collect()
        .await
        .map_err(|_| "answer too large".to_string())?
        .to_bytes();
    Ok(Reply {
        status,
        content_type,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints() {
        assert_eq!(
            endpoint_of("https://api.anthropic.com").unwrap(),
            (true, "api.anthropic.com".into(), 443, String::new())
        );
        assert_eq!(
            endpoint_of("http://127.0.0.1:9/x/").unwrap(),
            (false, "127.0.0.1".into(), 9, "/x".into())
        );
        assert!(endpoint_of("ftp://x").is_err());
        assert!(endpoint_of("https://").is_err());
    }
}
