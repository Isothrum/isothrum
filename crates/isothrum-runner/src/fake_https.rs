//! A test's HTTPS server on the loopback, standing for a server whose certificate is self-signed
//! (`fake_https/server.crt.der`, made once with `openssl req -x509 -newkey ec`): each request answered
//! by the test, and every request that reached it kept, so a test tells a request refused before
//! it was sent.

use std::sync::{Arc, Mutex, PoisonError};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::tls::Fingerprint;

/// The server's certificate, DER.
const CERT: &[u8] = include_bytes!("fake_https/server.crt.der");

/// Its key, PKCS #8 DER.
const KEY: &[u8] = include_bytes!("fake_https/server.key.der");

/// The server's certificate's fingerprint: what a client pins.
#[must_use]
pub fn fingerprint() -> Fingerprint {
    Fingerprint::of(CERT)
}

/// One request the server was sent.
#[derive(Debug, Clone)]
pub struct Request {
    /// `GET`, `POST`.
    pub method: String,
    /// Its path and query.
    pub target: String,
    /// Its headers, names in lower case.
    pub headers: Vec<(String, String)>,
    /// Its body, as text.
    pub body: String,
}

impl Request {
    /// Header `name`'s value (any case), when it was sent.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str())
    }
}

/// How the test answers a request: its HTTP status and its body (JSON).
type Answer = dyn Fn(&Request) -> (u16, String) + Send + Sync;

/// The server, serving until it drops.
pub struct Server {
    /// `https://127.0.0.1:<port>`.
    pub endpoint: String,
    requests: Arc<Mutex<Vec<Request>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    /// Serves on a free loopback port, each request answered by `answer`.
    ///
    /// # Errors
    ///
    /// The loopback has no free port, or the test certificate isn't read.
    pub async fn start(answer: impl Fn(&Request) -> (u16, String) + Send + Sync + 'static) -> std::io::Result<Server> {
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY.to_vec()));
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(std::io::Error::other)?
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(CERT.to_vec())], key)
            .map_err(std::io::Error::other)?;
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("https://127.0.0.1:{}", listener.local_addr()?.port());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let answer: Arc<Answer> = Arc::new(answer);
        let kept = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (acceptor, answer, kept) = (acceptor.clone(), Arc::clone(&answer), Arc::clone(&kept));
                tokio::spawn(async move {
                    // A client that refuses the certificate ends the handshake: nothing to answer.
                    if let Ok(tls) = acceptor.accept(stream).await {
                        serve(tls, &*answer, &kept).await;
                    }
                });
            }
        });
        Ok(Server { endpoint, requests, task })
    }

    /// The requests that reached it, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Reads one request from `stream`, keeps it, writes `answer`'s answer, and closes.
async fn serve(stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>, answer: &Answer, kept: &Mutex<Vec<Request>>) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).await.is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or_default().to_owned(), parts.next().unwrap_or_default().to_owned());
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await.is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let length = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).await.is_err() {
        return;
    }
    let request = Request {
        method,
        target,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    let (status, text) = answer(&request);
    kept.lock().unwrap_or_else(PoisonError::into_inner).push(request);
    let response = format!(
        "HTTP/1.1 {status} Answer\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    let mut writer = reader.into_inner();
    if writer.write_all(response.as_bytes()).await.is_ok() {
        // The client may have gone: nothing left to tell it.
        writer.shutdown().await.ok();
    }
}
