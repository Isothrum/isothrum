//! A test's HTTPS server on the loopback, its certificate self-signed (`https/server.crt.der`,
//! the runner's test certificate): each request answered by the test, and how many reached it
//! counted, so a test tells a request refused before it was sent.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use isothrum_runner::tls::Fingerprint;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const CERT: &[u8] = include_bytes!("https/server.crt.der");
const KEY: &[u8] = include_bytes!("https/server.key.der");

/// The server's certificate's fingerprint: what a client pins.
pub(crate) fn fingerprint() -> Fingerprint {
    Fingerprint::of(CERT)
}

/// How the test answers a request (its method and target): an HTTP status and a JSON body.
type Answer = dyn Fn(&str, &str) -> (u16, String) + Send + Sync;

/// The server, serving until it drops.
pub(crate) struct Server {
    /// Its loopback port.
    pub(crate) port: u16,
    reached: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    /// Serves on a free loopback port, each request answered by `answer`.
    pub(crate) async fn start(answer: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static) -> Server {
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY.to_vec()));
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(CERT.to_vec())], key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let reached = Arc::new(AtomicUsize::new(0));
        let answer: Arc<Answer> = Arc::new(answer);
        let count = Arc::clone(&reached);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (acceptor, answer, count) = (acceptor.clone(), Arc::clone(&answer), Arc::clone(&count));
                tokio::spawn(async move {
                    if let Ok(tls) = acceptor.accept(stream).await {
                        serve(tls, &*answer, &count).await;
                    }
                });
            }
        });
        Server { port, reached, task }
    }

    /// How many requests reached it.
    pub(crate) fn reached(&self) -> usize {
        self.reached.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>, answer: &Answer, count: &AtomicUsize) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).await.is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or_default().to_owned(), parts.next().unwrap_or_default().to_owned());
    let mut length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await.is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).await.is_err() {
        return;
    }
    count.fetch_add(1, Ordering::SeqCst);
    let (status, text) = answer(&method, &target);
    let response = format!(
        "HTTP/1.1 {status} Answer\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    let mut writer = reader.into_inner();
    if writer.write_all(response.as_bytes()).await.is_ok() {
        writer.shutdown().await.ok();
    }
}
