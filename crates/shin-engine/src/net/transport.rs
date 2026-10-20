//! How messages travel: the [`Transport`] trait, synchronous and polled
//! once per frame, and [`LoopbackTransport`], an in-process implementation
//! that needs no network.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::protocol::{Message, decode, encode};

/// A peer at the other end of a [`Transport`], as this end numbers it.
///
/// On a [`LoopbackTransport`] the server is `PeerId(0)` to every client
/// and client `i` (counting from 0) is `PeerId(i + 1)` to the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId(pub u32);

/// Delivery guarantee a message asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Delivered once, in order relative to other reliable messages.
    Reliable,
    /// May be dropped; newer state replaces it anyway (snapshots, inputs).
    Unreliable,
}

/// Something that happened since the last [`Transport::poll`].
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A peer connected.
    Connected(PeerId),
    /// A peer sent a message.
    Message(PeerId, Message),
    /// A peer went away; sending to it does nothing from now on.
    Disconnected(PeerId),
}

/// A message pipe to one or more peers. Sends are queued; everything
/// received is handed out by [`Self::poll`], which the game calls once per
/// frame (or tick). Implementations never block.
pub trait Transport {
    /// Queues `msg` for `to`. Unknown or disconnected peers are ignored.
    fn send(&mut self, to: PeerId, msg: &Message, channel: Channel);
    /// Queues `msg` for every connected peer.
    fn broadcast(&mut self, msg: &Message, channel: Channel);
    /// Everything that happened since the last poll, oldest first.
    fn poll(&mut self) -> Vec<Event>;
    /// The connected peers, in id order.
    fn peers(&self) -> Vec<PeerId>;
}

/// What arrives in an inbox: bytes from a peer, or word that it hung up.
enum Packet {
    Data(Vec<u8>),
    Closed,
}

/// The queue one end receives into: `(sender as the receiver numbers it,
/// packet)`, oldest first.
type Inbox = Arc<Mutex<VecDeque<(PeerId, Packet)>>>;

/// A connection from this end to one peer.
struct Link {
    /// How this end numbers the peer.
    peer: PeerId,
    /// How the peer numbers this end.
    me: PeerId,
    /// The peer's inbox, which this end sends into.
    inbox: Inbox,
}

/// An in-process [`Transport`]: [`Self::pair`] makes a server end and
/// client ends that talk through shared queues.
///
/// Messages are encoded on send and decoded on the receiver's next
/// [`Transport::poll`], so they exercise the real codec. Delivery is FIFO
/// and lossless on both channels. Each end reports
/// [`Event::Connected`] for its peers on its first poll, and dropping an
/// end makes its peers report [`Event::Disconnected`]. The ends are
/// `Send`, so they may live on different threads.
pub struct LoopbackTransport {
    /// This end's connections, in peer id order.
    links: Vec<Link>,
    /// What peers sent this end.
    inbox: Inbox,
    /// Peers whose [`Event::Connected`] the next poll still reports.
    unannounced: Vec<PeerId>,
}

impl LoopbackTransport {
    /// One server end and `clients` client ends, each client connected to
    /// the server (clients do not see each other).
    pub fn pair(clients: usize) -> (LoopbackTransport, Vec<LoopbackTransport>) {
        let server_inbox = Inbox::default();
        let mut server_links = Vec::with_capacity(clients);
        let client_ends = (0..clients)
            .map(|i| {
                let id = PeerId(u32::try_from(i + 1).expect("fewer than 2^32 clients"));
                let inbox = Inbox::default();
                server_links.push(Link {
                    peer: id,
                    me: PeerId(0),
                    inbox: inbox.clone(),
                });
                Self::new(
                    vec![Link {
                        peer: PeerId(0),
                        me: id,
                        inbox: server_inbox.clone(),
                    }],
                    inbox,
                )
            })
            .collect();
        (Self::new(server_links, server_inbox), client_ends)
    }

    fn new(links: Vec<Link>, inbox: Inbox) -> Self {
        let unannounced = links.iter().map(|link| link.peer).collect();
        Self {
            links,
            inbox,
            unannounced,
        }
    }

    /// Pushes `packet` into `link`'s inbox, signed as this end.
    fn push(link: &Link, packet: Packet) {
        lock(&link.inbox).push_back((link.me, packet));
    }
}

/// Locks an inbox, recovering it if another end panicked while holding it
/// (the queue itself is always in a valid state).
fn lock(inbox: &Inbox) -> std::sync::MutexGuard<'_, VecDeque<(PeerId, Packet)>> {
    inbox
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Transport for LoopbackTransport {
    fn send(&mut self, to: PeerId, msg: &Message, _channel: Channel) {
        match self.links.iter().find(|link| link.peer == to) {
            Some(link) => Self::push(link, Packet::Data(encode(msg))),
            None => log::debug!("loopback: dropping a message to unknown peer {to:?}"),
        }
    }

    fn broadcast(&mut self, msg: &Message, _channel: Channel) {
        let bytes = encode(msg);
        for link in &self.links {
            Self::push(link, Packet::Data(bytes.clone()));
        }
    }

    fn poll(&mut self) -> Vec<Event> {
        let mut events: Vec<Event> = self.unannounced.drain(..).map(Event::Connected).collect();
        let packets: Vec<(PeerId, Packet)> = lock(&self.inbox).drain(..).collect();
        for (from, packet) in packets {
            match packet {
                Packet::Data(bytes) => match decode(&bytes) {
                    Ok(msg) => events.push(Event::Message(from, msg)),
                    Err(err) => log::warn!("loopback: dropping a message from {from:?}: {err:#}"),
                },
                Packet::Closed => {
                    self.links.retain(|link| link.peer != from);
                    events.push(Event::Disconnected(from));
                }
            }
        }
        events
    }

    fn peers(&self) -> Vec<PeerId> {
        self.links.iter().map(|link| link.peer).collect()
    }
}

impl Drop for LoopbackTransport {
    /// Tells every peer this end is gone.
    fn drop(&mut self) {
        for link in &self.links {
            Self::push(link, Packet::Closed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::InputFrame;

    fn goodbye(reason: &str) -> Message {
        Message::Goodbye {
            reason: reason.into(),
        }
    }

    /// The messages in `events`, dropping connection events.
    fn messages(events: Vec<Event>) -> Vec<(PeerId, Message)> {
        events
            .into_iter()
            .filter_map(|event| match event {
                Event::Message(from, msg) => Some((from, msg)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn first_poll_reports_connections() {
        let (mut server, mut clients) = LoopbackTransport::pair(2);
        assert_eq!(server.peers(), [PeerId(1), PeerId(2)]);
        assert_eq!(
            server.poll(),
            [Event::Connected(PeerId(1)), Event::Connected(PeerId(2))]
        );
        assert_eq!(server.poll(), [], "reported once");
        assert_eq!(clients[1].peers(), [PeerId(0)]);
        assert_eq!(clients[1].poll(), [Event::Connected(PeerId(0))]);
    }

    #[test]
    fn broadcast_reaches_every_client_on_its_next_poll_in_order() {
        let (mut server, mut clients) = LoopbackTransport::pair(3);
        server.broadcast(&goodbye("one"), Channel::Reliable);
        server.broadcast(&goodbye("two"), Channel::Unreliable);
        server.broadcast(&goodbye("three"), Channel::Reliable);
        for client in &mut clients {
            let got = messages(client.poll());
            let want: Vec<_> = ["one", "two", "three"]
                .map(|r| (PeerId(0), goodbye(r)))
                .into();
            assert_eq!(got, want);
            assert_eq!(client.poll(), [], "delivered once");
        }
    }

    #[test]
    fn order_is_kept_per_channel() {
        let (mut server, mut clients) = LoopbackTransport::pair(1);
        for tick in 0..5 {
            let channel = if tick % 2 == 0 {
                Channel::Reliable
            } else {
                Channel::Unreliable
            };
            clients[0].send(PeerId(0), &Message::Input(InputFrame { tick }), channel);
        }
        let ticks: Vec<u64> = messages(server.poll())
            .into_iter()
            .map(|(from, msg)| {
                assert_eq!(from, PeerId(1));
                let Message::Input(frame) = msg else {
                    panic!("expected an input")
                };
                frame.tick
            })
            .collect();
        assert_eq!(ticks, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn send_reaches_only_its_peer() {
        let (mut server, mut clients) = LoopbackTransport::pair(2);
        server.send(PeerId(2), &goodbye("you"), Channel::Reliable);
        server.send(PeerId(9), &goodbye("nobody"), Channel::Reliable);
        assert_eq!(messages(clients[0].poll()), []);
        assert_eq!(messages(clients[1].poll()), [(PeerId(0), goodbye("you"))]);
    }

    #[test]
    fn dropping_an_end_disconnects_it() {
        let (mut server, mut clients) = LoopbackTransport::pair(2);
        server.poll();
        clients[0].send(PeerId(0), &goodbye("leaving"), Channel::Reliable);
        drop(clients.remove(0));
        assert_eq!(
            server.poll(),
            [
                Event::Message(PeerId(1), goodbye("leaving")),
                Event::Disconnected(PeerId(1)),
            ]
        );
        assert_eq!(server.peers(), [PeerId(2)]);
        drop(server);
        let events = clients[0].poll();
        assert_eq!(events.last(), Some(&Event::Disconnected(PeerId(0))));
        assert_eq!(clients[0].peers(), []);
    }

    #[test]
    fn ends_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<LoopbackTransport>();
    }
}
