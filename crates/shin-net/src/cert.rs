//! Development certificates: the server's self-signed certificate
//! ([`DevCert`]) and how a client decides to trust it ([`ServerTrust`]).
//!
//! QUIC always encrypts, so a server needs a certificate. There is no real
//! PKI here (that is out of scope): the server makes a fresh self-signed
//! certificate for `localhost` every time it starts, and a client either
//! trusts that exact certificate (a copy of its DER bytes) or, for
//! development only, any certificate at all.

use std::fmt;
use std::sync::Arc;

use anyhow::Context as _;
use quinn::crypto::rustls::QuicClientConfig;
use quinn::rustls::{
    self, DigitallySignedStruct, RootCertStore, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, ring},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
};

/// The name the server's certificate is issued to and the client checks.
/// It is the same whatever address the client dials, so a trusted
/// certificate works over the LAN too.
pub(crate) const SERVER_NAME: &str = "localhost";

/// A freshly generated self-signed certificate and its private key.
pub(crate) struct DevCert {
    /// The certificate, DER-encoded.
    pub(crate) cert: CertificateDer<'static>,
    /// Its PKCS #8 private key.
    pub(crate) key: PrivateKeyDer<'static>,
}

impl DevCert {
    /// A new self-signed certificate for [`SERVER_NAME`].
    pub(crate) fn generate() -> anyhow::Result<Self> {
        let certified = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_string()])
            .context("generating a self-signed development certificate")?;
        Ok(Self {
            cert: certified.cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()).into(),
        })
    }
}

/// The SHA-256 fingerprint of a DER-encoded certificate, as colon-separated
/// upper-case hex (`AB:CD:...`), the form browsers and `openssl x509
/// -fingerprint -sha256` print.
pub fn fingerprint(cert_der: &[u8]) -> String {
    let sha256 = ring::cipher_suite::TLS13_AES_128_GCM_SHA256
        .tls13()
        .expect("a TLS 1.3 suite")
        .common
        .hash_provider;
    let digest = sha256.hash(cert_der);
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Which server certificates a client accepts.
#[derive(Clone)]
pub enum ServerTrust {
    /// Any certificate at all. **Development only**: the connection is
    /// still encrypted, but anyone on the path can pretend to be the
    /// server. [`crate::QuicTransport::connect`] logs a warning.
    Insecure,
    /// Exactly this certificate (DER bytes), e.g. the file a server wrote
    /// with [`crate::QuicTransport::cert_der`].
    Cert(Vec<u8>),
}

impl fmt::Debug for ServerTrust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Insecure => f.write_str("Insecure"),
            Self::Cert(der) => write!(f, "Cert({})", fingerprint(der)),
        }
    }
}

/// The crypto backend: *ring*, the one quinn is built with here.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

/// The quinn client configuration for `trust`.
pub(crate) fn client_config(trust: &ServerTrust) -> anyhow::Result<quinn::ClientConfig> {
    match trust {
        ServerTrust::Cert(der) => {
            let mut roots = RootCertStore::empty();
            roots
                .add(CertificateDer::from(der.clone()))
                .context("the trusted server certificate is not a valid DER certificate")?;
            Ok(quinn::ClientConfig::with_root_certificates(Arc::new(
                roots,
            ))?)
        }
        ServerTrust::Insecure => {
            let provider = provider();
            let tls = rustls::ClientConfig::builder_with_provider(provider.clone())
                .with_protocol_versions(&[&rustls::version::TLS13])?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
                .with_no_client_auth();
            Ok(quinn::ClientConfig::new(Arc::new(
                QuicClientConfig::try_from(tls)?,
            )))
        }
    }
}

/// A verifier that accepts every certificate but still checks that the
/// handshake is signed by the key in it (so the connection is encrypted
/// to whoever holds that key).
#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_sha256_as_colon_hex() {
        // SHA-256 of the empty input.
        assert_eq!(
            fingerprint(b""),
            "E3:B0:C4:42:98:FC:1C:14:9A:FB:F4:C8:99:6F:B9:24:\
             27:AE:41:E4:64:9B:93:4C:A4:95:99:1B:78:52:B8:55"
        );
    }

    #[test]
    fn generated_certificates_differ_and_configure_a_client() -> anyhow::Result<()> {
        let (a, b) = (DevCert::generate()?, DevCert::generate()?);
        assert_ne!(fingerprint(&a.cert), fingerprint(&b.cert));
        client_config(&ServerTrust::Cert(a.cert.to_vec()))?;
        client_config(&ServerTrust::Insecure)?;
        assert!(client_config(&ServerTrust::Cert(vec![1, 2, 3])).is_err());
        Ok(())
    }
}
