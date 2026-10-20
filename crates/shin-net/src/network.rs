//! [`QuicNetwork`]: the engine's [`Network`] over QUIC, which `net.host`
//! and `net.join` use once it is installed as the
//! [`shin::net::Networking`] resource (GE-046).

use std::net::{Ipv4Addr, ToSocketAddrs};

use anyhow::Context as _;
use shin::net::{Listening, Network, Transport};

use crate::{QuicTransport, ServerTrust};

/// QUIC for `net.host` and `net.join`: a host listens with
/// [`QuicTransport::listen`] on every IPv4 interface, a client connects
/// with [`QuicTransport::connect`], trusting the certificate it is given
/// ([`ServerTrust::Cert`]) or, without one, any server
/// ([`ServerTrust::Insecure`]).
///
/// ```no_run
/// # fn main() -> anyhow::Result<()> {
/// let mut engine = shin::headless::Headless::new(64, 64)?;
/// engine
///     .resources
///     .insert(shin::net::Networking(Box::new(shin_net::QuicNetwork)));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct QuicNetwork;

impl Network for QuicNetwork {
    fn listen(&mut self, port: u16) -> anyhow::Result<Listening> {
        let transport = QuicTransport::listen((Ipv4Addr::UNSPECIFIED, port).into())?;
        Ok(Listening {
            port: transport.local_addr()?.port(),
            cert_der: transport.cert_der().map(<[u8]>::to_vec),
            transport: Box::new(transport),
        })
    }

    fn connect(
        &mut self,
        addr: &str,
        cert: Option<&[u8]>,
    ) -> anyhow::Result<Box<dyn Transport + Send>> {
        let resolved = addr
            .to_socket_addrs()
            .with_context(|| format!("`{addr}` is not a host:port address"))?
            .next()
            .with_context(|| format!("`{addr}` resolves to no address"))?;
        let trust = cert.map_or(ServerTrust::Insecure, |der| ServerTrust::Cert(der.to_vec()));
        Ok(Box::new(QuicTransport::connect(resolved, &trust)?))
    }
}
