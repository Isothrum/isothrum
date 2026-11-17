//! How a TLS client trusts a server: by the system's trust store (the chain to a trusted
//! authority, and the name in it), or by one certificate's SHA-256 fingerprint alone, for a server
//! whose certificate is self-signed. A pinned certificate is still made to prove it holds its key:
//! the handshake's signatures are checked against it as for any certificate; only the chain and
//! the name are replaced by the fingerprint.
//!
//! [`Tls::new`] gives the `rustls` configuration of either, and, for a pinned one, the certificate
//! the server offered once it was refused ([`Tls::refused`]): what an embedder shows a user asked
//! to confirm a certificate, or told it changed.

use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex, PoisonError};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

/// A certificate's SHA-256 fingerprint. Written as upper-case hex pairs joined by colons (as
/// Proxmox VE's web interface and `openssl x509 -fingerprint -sha256` show it); read with or
/// without the colons, in any case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// The fingerprint of the certificate `der` (its DER bytes).
    #[must_use]
    pub fn of(der: &[u8]) -> Fingerprint {
        Fingerprint(Sha256::digest(der).into())
    }

    /// A fingerprint as typed or stored: 64 hex digits, with or without colons and spaces, in any
    /// case; none for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Fingerprint> {
        let mut digits = text.chars().filter(|c| *c != ':' && !c.is_whitespace()).map(|c| c.to_digit(16));
        let mut bytes = [0u8; 32];
        for byte in &mut bytes {
            let (high, low) = (digits.next()??, digits.next()??);
            *byte = u8::try_from(high * 16 + low).ok()?;
        }
        digits.next().is_none().then_some(Fingerprint(bytes))
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, byte) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(":")?;
            }
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

impl FromStr for Fingerprint {
    type Err = NotAFingerprint;

    fn from_str(text: &str) -> Result<Fingerprint, NotAFingerprint> {
        Fingerprint::parse(text).ok_or(NotAFingerprint)
    }
}

impl serde::Serialize for Fingerprint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Fingerprint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Fingerprint, D::Error> {
        let text = String::deserialize(d)?;
        Fingerprint::parse(&text).ok_or_else(|| serde::de::Error::custom(NotAFingerprint))
    }
}

/// Text that isn't a SHA-256 certificate fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a SHA-256 certificate fingerprint (64 hex digits, colons optional)")]
pub struct NotAFingerprint;

/// How a server's certificate is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// By the system's trust store, as any HTTPS site.
    System,
    /// By its fingerprint alone; none before one was confirmed (every certificate is then
    /// refused, and the one offered is told: [`Tls::refused`]).
    Pinned(Option<Fingerprint>),
}

/// Why a pinned configuration refused a server's certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No certificate was confirmed for the server yet: this is the one it offers.
    Unconfirmed {
        /// The certificate the server offered.
        presented: Fingerprint,
    },
    /// The server offered another certificate than the one pinned.
    Changed {
        /// The certificate pinned for it.
        pinned: Fingerprint,
        /// The one it offered.
        presented: Fingerprint,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Unconfirmed { presented } => write!(f, "the server's certificate (SHA-256 {presented}) isn't confirmed yet"),
            Refusal::Changed { pinned, presented } => {
                write!(f, "the server's certificate (SHA-256 {presented}) isn't the one pinned for it ({pinned})")
            }
        }
    }
}

/// TLS couldn't be set up (the crypto provider refused the protocol versions or the verifier).
#[derive(Debug, Clone, thiserror::Error)]
#[error("TLS couldn't be set up: {0}")]
pub struct TlsSetupError(String);

/// The certificate a pinned connection was last offered.
type Seen = Arc<Mutex<Option<Fingerprint>>>;

/// A client's TLS configuration for one server, trusted as [`Trust`] says, and, pinned, the
/// certificate that server last offered.
#[derive(Debug, Clone)]
pub struct Tls {
    config: Arc<ClientConfig>,
    pin: Option<Fingerprint>,
    /// What a pinned configuration's verifier saw; none for the system's trust store.
    seen: Option<Seen>,
}

impl Tls {
    /// The configuration trusting the server as `trust` says (ring's crypto, TLS 1.2 and 1.3,
    /// HTTP/1.1 offered by ALPN).
    ///
    /// # Errors
    ///
    /// The provider refuses the protocol versions, or the system's verifier can't be made.
    pub fn new(trust: Trust) -> Result<Tls, TlsSetupError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .map_err(|e| TlsSetupError(e.to_string()))?;
        let (mut config, pin, seen) = match trust {
            Trust::System => {
                let verifier = rustls_platform_verifier::Verifier::new(provider).map_err(|e| TlsSetupError(e.to_string()))?;
                let config = builder.dangerous().with_custom_certificate_verifier(Arc::new(verifier)).with_no_client_auth();
                (config, None, None)
            }
            Trust::Pinned(pin) => {
                let seen = Seen::default();
                let verifier = PinVerifier {
                    pin,
                    seen: Arc::clone(&seen),
                    algorithms: provider.signature_verification_algorithms,
                };
                let config = builder.dangerous().with_custom_certificate_verifier(Arc::new(verifier)).with_no_client_auth();
                (config, pin, Some(seen))
            }
        };
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Tls {
            config: Arc::new(config),
            pin,
            seen,
        })
    }

    /// The `rustls` client configuration.
    #[must_use]
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Why a connection made with this configuration failed on the server's certificate: none
    /// confirmed yet, or not the pinned one. None when the certificate isn't why (or none was
    /// offered, or the system's trust store judged it).
    #[must_use]
    pub fn refused(&self) -> Option<Refusal> {
        refusal(self.pin, self.seen.as_ref()?)
    }
}

/// Why a verifier pinned to `pin` refused the certificate it last saw (`seen`); none when it saw
/// none, or the pinned one.
fn refusal(pin: Option<Fingerprint>, seen: &Seen) -> Option<Refusal> {
    let presented = (*seen.lock().unwrap_or_else(PoisonError::into_inner))?;
    match pin {
        None => Some(Refusal::Unconfirmed { presented }),
        Some(pinned) if pinned != presented => Some(Refusal::Changed { pinned, presented }),
        Some(_) => None,
    }
}

/// Accepts the one certificate whose fingerprint is `pin` (none before one was confirmed), and
/// records every certificate it is offered in `seen`.
#[derive(Debug)]
struct PinVerifier {
    pin: Option<Fingerprint>,
    seen: Seen,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let presented = Fingerprint::of(end_entity.as_ref());
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(presented);
        if self.pin == Some(presented) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("the server's certificate isn't the one pinned for it".to_owned()))
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89";

    #[test]
    fn a_fingerprint_reads_as_typed_and_writes_as_proxmox_shows_it() {
        let fp = Fingerprint::parse(HEX).unwrap();
        assert_eq!(fp.to_string(), HEX);
        assert_eq!(Fingerprint::parse(&HEX.replace(':', "").to_lowercase()), Some(fp));
        assert_eq!(Fingerprint::parse(&format!(" {} ", HEX.replace(':', " "))), Some(fp));
        assert_eq!(HEX.parse::<Fingerprint>(), Ok(fp));
        for bad in ["", "AB:CD", &format!("{HEX}:00"), &HEX.replace('A', "G"), &HEX.replacen("AB", "ÀB", 1)] {
            assert_eq!(Fingerprint::parse(bad), None, "{bad}");
        }
        let json = serde_json::to_string(&fp).unwrap();
        assert_eq!(json, format!("\"{HEX}\""));
        assert_eq!(serde_json::from_str::<Fingerprint>(&json).unwrap(), fp);
        assert!(serde_json::from_str::<Fingerprint>("\"nope\"").is_err());
    }

    #[test]
    fn a_fingerprint_is_the_certificates_sha256() {
        // SHA-256 of the empty input.
        assert_eq!(
            Fingerprint::of(b"").to_string(),
            "E3:B0:C4:42:98:FC:1C:14:9A:FB:F4:C8:99:6F:B9:24:27:AE:41:E4:64:9B:93:4C:A4:95:99:1B:78:52:B8:55"
        );
    }

    /// Whether a verifier pinned to `pin` accepts certificate `der`, and the refusal it then tells.
    fn offered(pin: Option<Fingerprint>, der: &[u8]) -> (bool, Option<Refusal>) {
        let seen = Seen::default();
        let verifier = PinVerifier {
            pin,
            seen: Arc::clone(&seen),
            algorithms: rustls::crypto::ring::default_provider().signature_verification_algorithms,
        };
        let name = ServerName::try_from("server.example").unwrap();
        let accepted = verifier
            .verify_server_cert(&CertificateDer::from(der), &[], &name, &[], UnixTime::now())
            .is_ok();
        (accepted, refusal(pin, &seen))
    }

    #[test]
    fn only_the_pinned_certificate_is_accepted_and_the_one_offered_is_told() {
        let (mine, other) = (b"the pinned certificate".as_slice(), b"another one".as_slice());
        let pin = Some(Fingerprint::of(mine));
        assert_eq!(offered(pin, mine), (true, None));
        let changed = Refusal::Changed {
            pinned: Fingerprint::of(mine),
            presented: Fingerprint::of(other),
        };
        assert_eq!(offered(pin, other), (false, Some(changed)));
        let unconfirmed = Refusal::Unconfirmed {
            presented: Fingerprint::of(mine),
        };
        assert_eq!(offered(None, mine), (false, Some(unconfirmed)), "nothing is accepted before a pin");
        assert!(changed.to_string().contains(&Fingerprint::of(other).to_string()), "{changed}");
    }

    #[test]
    fn the_system_trust_store_tells_no_pin_refusal() {
        let tls = Tls::new(Trust::System).unwrap();
        assert_eq!(tls.refused(), None);
        assert_eq!(tls.config().alpn_protocols, [b"http/1.1".to_vec()]);
    }
}
