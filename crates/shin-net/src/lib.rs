//! Shin's network transport (GE-045): [`QuicTransport`], the engine's
//! polled [`shin::net::Transport`] over QUIC, built on quinn.
//!
//! QUIC gives each connection a reliable, ordered stream (for
//! [`shin::net::Channel::Reliable`]) and unreliable datagrams (for
//! [`shin::net::Channel::Unreliable`]: snapshots and inputs), encrypted
//! with TLS 1.3.
//!
//! ```no_run
//! use shin_net::{QuicTransport, ServerTrust};
//!
//! # fn main() -> anyhow::Result<()> {
//! // A server on port 7777, with a fresh self-signed certificate ...
//! let server = QuicTransport::listen("0.0.0.0:7777".parse()?)?;
//! std::fs::write("server-cert.der", server.cert_der().unwrap())?;
//! // ... and a client that trusts exactly that certificate.
//! let cert = std::fs::read("server-cert.der")?;
//! let client = QuicTransport::connect("127.0.0.1:7777".parse()?, &ServerTrust::Cert(cert))?;
//! # Ok(())
//! # }
//! ```
//!
//! Both ends are `Send`, so they go into a [`shin::net::NetServer`] (or a
//! client) as a `Box<dyn Transport + Send>`.
//!
//! Certificates are for development: the server makes a new self-signed
//! one for `localhost` at every start ([`QuicTransport::cert_der`],
//! [`fingerprint`]), and a client trusts that file
//! ([`ServerTrust::Cert`]) or, loudly, anything ([`ServerTrust::Insecure`]).
//! There is no NAT traversal or relay: the server's UDP port must be
//! reachable.
//!
//! The engine library does not depend on this crate; a game does.

mod cert;
mod quic;

pub use cert::{ServerTrust, fingerprint};
pub use quic::QuicTransport;
