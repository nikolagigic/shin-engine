//! How messages travel: the [`Transport`] trait, synchronous and polled
//! once per frame, and [`LoopbackTransport`], an in-process implementation
//! that needs no network. [`LoopbackOptions`] give a loopback simulated
//! latency, jitter, and loss, deterministic from a seed (GE-043).

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

/// Simulated network conditions for a [`LoopbackTransport`]
/// ([`LoopbackTransport::pair_with`]). The default is a perfect network:
/// no delay and no loss, as [`LoopbackTransport::pair`] gives.
///
/// Time is counted in polls of the receiving end, which a game makes once
/// per tick: a message sent with a latency of `n` arrives on the
/// receiver's `(n + 1)`-th poll after the send, so latency `0` means the
/// next poll. Each direction is delayed separately; a round trip takes
/// twice the latency. At 60 ticks per second, 100 ms is 6 ticks.
///
/// Jitter adds a further `0..=jitter_ticks` polls to each message, drawn
/// at random. Unreliable messages may then arrive out of order; reliable
/// ones never do (a reliable message waits for the one sent before it),
/// and a hang-up arrives after everything sent before it.
/// `drop_unreliable` is the probability that an
/// [`Unreliable`](Channel::Unreliable) message is lost (each recipient of
/// a broadcast draws separately); reliable messages are never lost.
///
/// The randomness comes from a small generator seeded from `seed`, one
/// stream per end, so the same options and the same sends give the same
/// deliveries every run.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoopbackOptions {
    /// Fixed one-way delay, in receiver polls (ticks).
    pub latency_ticks: u32,
    /// Extra random delay of up to this many polls per message.
    pub jitter_ticks: u32,
    /// Probability, from 0 to 1, that an unreliable message is dropped.
    pub drop_unreliable: f64,
    /// Seed for jitter and loss.
    pub seed: u64,
}

/// What arrives in an inbox: bytes from a peer, or word that it hung up.
enum Packet {
    Data(Vec<u8>),
    Closed,
}

/// A packet on its way.
struct InFlight {
    /// The sender, as the receiver numbers it.
    from: PeerId,
    /// The receiver poll that delivers it.
    due: u64,
    packet: Packet,
}

/// What one end receives into.
#[derive(Default)]
struct InboxState {
    /// How many times the receiving end has polled: its clock.
    polls: u64,
    /// Packets not yet delivered, in the order they were sent.
    queue: Vec<InFlight>,
}

/// The shared queue one end receives into.
type Inbox = Arc<Mutex<InboxState>>;

/// A connection from this end to one peer.
struct Link {
    /// How this end numbers the peer.
    peer: PeerId,
    /// How the peer numbers this end.
    me: PeerId,
    /// The peer's inbox, which this end sends into.
    inbox: Inbox,
    /// The delivery poll of the last reliable packet sent on this link, so
    /// jitter never reorders reliable messages.
    reliable_due: u64,
    /// The latest delivery poll of any packet sent on this link, so the
    /// hang-up arrives after all of them.
    last_due: u64,
}

impl Link {
    fn new(peer: PeerId, me: PeerId, inbox: Inbox) -> Self {
        Self {
            peer,
            me,
            inbox,
            reliable_due: 0,
            last_due: 0,
        }
    }
}

/// A tiny deterministic random number generator (SplitMix64): enough for
/// simulated jitter and loss, without a dependency.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0.0..1.0`.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1_u64 << 53) as f64
    }

    /// A number in `0..=max`.
    fn up_to(&mut self, max: u32) -> u64 {
        if max == 0 {
            0
        } else {
            self.next_u64() % (u64::from(max) + 1)
        }
    }
}

/// An in-process [`Transport`]: [`Self::pair`] makes a server end and
/// client ends that talk through shared queues.
///
/// Messages are encoded on send and decoded when the receiver's
/// [`Transport::poll`] delivers them, so they exercise the real codec.
/// From [`Self::pair`], delivery is on the receiver's next poll, FIFO and
/// lossless on both channels; [`Self::pair_with`] simulates latency,
/// jitter, and loss instead ([`LoopbackOptions`]). Each end reports
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
    /// The simulated network conditions.
    opts: LoopbackOptions,
    /// This end's jitter and loss draws.
    rng: Rng,
}

impl LoopbackTransport {
    /// One server end and `clients` client ends, each client connected to
    /// the server (clients do not see each other), on a perfect network.
    pub fn pair(clients: usize) -> (LoopbackTransport, Vec<LoopbackTransport>) {
        Self::pair_with(clients, LoopbackOptions::default())
    }

    /// [`Self::pair`] with simulated network conditions on every link, in
    /// both directions.
    pub fn pair_with(
        clients: usize,
        opts: LoopbackOptions,
    ) -> (LoopbackTransport, Vec<LoopbackTransport>) {
        let server_inbox = Inbox::default();
        let mut server_links = Vec::with_capacity(clients);
        let client_ends = (0..clients)
            .map(|i| {
                let id = PeerId(u32::try_from(i + 1).expect("fewer than 2^32 clients"));
                let inbox = Inbox::default();
                server_links.push(Link::new(id, PeerId(0), inbox.clone()));
                let link = Link::new(PeerId(0), id, server_inbox.clone());
                Self::new(vec![link], inbox, opts, id)
            })
            .collect();
        let server = Self::new(server_links, server_inbox, opts, PeerId(0));
        (server, client_ends)
    }

    /// The end the server numbers `me`, with `links`, receiving into
    /// `inbox`. Its random stream is derived from the seed and `me`.
    fn new(links: Vec<Link>, inbox: Inbox, opts: LoopbackOptions, me: PeerId) -> Self {
        let unannounced = links.iter().map(|link| link.peer).collect();
        let rng = Rng(opts.seed ^ u64::from(me.0).wrapping_mul(0xD1B5_4A32_D192_ED03));
        Self {
            links,
            inbox,
            unannounced,
            opts,
            rng,
        }
    }

    /// Queues `bytes` on the link at `index`, unless the simulated network
    /// loses them, due after the simulated delay.
    fn send_on(&mut self, index: usize, bytes: Vec<u8>, channel: Channel) {
        let lost = channel == Channel::Unreliable
            && self.opts.drop_unreliable > 0.0
            && self.rng.next_f64() < self.opts.drop_unreliable;
        if lost {
            log::trace!("loopback: simulated loss of a message to {:?}", self.links[index].peer);
            return;
        }
        let delay = u64::from(self.opts.latency_ticks) + self.rng.up_to(self.opts.jitter_ticks);
        let link = &mut self.links[index];
        let mut inbox = lock(&link.inbox);
        let mut due = inbox.polls + 1 + delay;
        if channel == Channel::Reliable {
            due = due.max(link.reliable_due);
            link.reliable_due = due;
        }
        link.last_due = link.last_due.max(due);
        inbox.queue.push(InFlight {
            from: link.me,
            due,
            packet: Packet::Data(bytes),
        });
    }
}

/// Locks an inbox, recovering it if another end panicked while holding it
/// (the queue itself is always in a valid state).
fn lock(inbox: &Inbox) -> std::sync::MutexGuard<'_, InboxState> {
    inbox
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Transport for LoopbackTransport {
    fn send(&mut self, to: PeerId, msg: &Message, channel: Channel) {
        match self.links.iter().position(|link| link.peer == to) {
            Some(index) => self.send_on(index, encode(msg), channel),
            None => log::debug!("loopback: dropping a message to unknown peer {to:?}"),
        }
    }

    fn broadcast(&mut self, msg: &Message, channel: Channel) {
        let bytes = encode(msg);
        for index in 0..self.links.len() {
            self.send_on(index, bytes.clone(), channel);
        }
    }

    fn poll(&mut self) -> Vec<Event> {
        let mut events: Vec<Event> = self.unannounced.drain(..).map(Event::Connected).collect();
        let mut arrived: Vec<InFlight> = {
            let mut inbox = lock(&self.inbox);
            inbox.polls += 1;
            let now = inbox.polls;
            let (ready, waiting) = std::mem::take(&mut inbox.queue)
                .into_iter()
                .partition(|p| p.due <= now);
            inbox.queue = waiting;
            ready
        };
        // Oldest due first; packets due on the same poll keep send order.
        arrived.sort_by_key(|p| p.due);
        for InFlight { from, packet, .. } in arrived {
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
    /// Tells every peer this end is gone, after everything already sent.
    fn drop(&mut self) {
        for link in &self.links {
            let mut inbox = lock(&link.inbox);
            let due = (inbox.polls + 1).max(link.last_due);
            inbox.queue.push(InFlight {
                from: link.me,
                due,
                packet: Packet::Closed,
            });
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

    fn input(tick: u64) -> Message {
        Message::Input(InputFrame { tick })
    }

    /// The input ticks in `events`.
    fn input_ticks(events: Vec<Event>) -> Vec<u64> {
        messages(events)
            .into_iter()
            .filter_map(|(_, msg)| match msg {
                Message::Input(frame) => Some(frame.tick),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn latency_delays_delivery_by_whole_polls() {
        let opts = LoopbackOptions {
            latency_ticks: 3,
            ..LoopbackOptions::default()
        };
        let (mut server, mut clients) = LoopbackTransport::pair_with(1, opts);
        server.send(PeerId(1), &input(7), Channel::Unreliable);
        server.send(PeerId(1), &goodbye("late"), Channel::Reliable);
        let client = &mut clients[0];
        assert_eq!(client.poll(), [Event::Connected(PeerId(0))]);
        assert_eq!(client.poll(), []);
        assert_eq!(client.poll(), []);
        assert_eq!(
            client.poll(),
            [
                Event::Message(PeerId(0), input(7)),
                Event::Message(PeerId(0), goodbye("late")),
            ],
            "the fourth poll: latency 3"
        );
        assert_eq!(client.poll(), []);
    }

    #[test]
    fn jitter_never_reorders_reliable_messages() {
        let opts = LoopbackOptions {
            latency_ticks: 1,
            jitter_ticks: 5,
            seed: 42,
            ..LoopbackOptions::default()
        };
        let (mut server, mut clients) = LoopbackTransport::pair_with(1, opts);
        let mut got = Vec::new();
        for tick in 0..200 {
            clients[0].send(PeerId(0), &input(tick), Channel::Reliable);
            got.extend(input_ticks(server.poll()));
        }
        for _ in 0..10 {
            got.extend(input_ticks(server.poll()));
        }
        assert_eq!(got, (0..200).collect::<Vec<_>>());
    }

    /// The unreliable inputs 0..1000 a client receives through `opts`, in
    /// arrival order.
    fn unreliable_run(opts: LoopbackOptions) -> Vec<u64> {
        let (mut server, mut clients) = LoopbackTransport::pair_with(1, opts);
        let mut got = Vec::new();
        for tick in 0..1000 {
            server.send(PeerId(1), &input(tick), Channel::Unreliable);
            got.extend(input_ticks(clients[0].poll()));
        }
        for _ in 0..20 {
            got.extend(input_ticks(clients[0].poll()));
        }
        got
    }

    #[test]
    fn loss_and_jitter_are_deterministic_from_the_seed() {
        let opts = LoopbackOptions {
            latency_ticks: 2,
            jitter_ticks: 3,
            drop_unreliable: 0.1,
            seed: 7,
        };
        let a = unreliable_run(opts);
        assert_eq!(a, unreliable_run(opts), "same seed, same deliveries");
        let lost = 1000 - a.len();
        assert!((60..140).contains(&lost), "about 10 % lost: {lost}");
        assert!(a.windows(2).any(|w| w[1] < w[0]), "jitter reorders");
        let mut sorted = a.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), a.len(), "nothing delivered twice");
        let other = unreliable_run(LoopbackOptions { seed: 8, ..opts });
        assert_ne!(a, other, "another seed, other deliveries");
    }

    #[test]
    fn reliable_messages_are_never_dropped() {
        let opts = LoopbackOptions {
            drop_unreliable: 1.0,
            ..LoopbackOptions::default()
        };
        let (mut server, mut clients) = LoopbackTransport::pair_with(1, opts);
        server.send(PeerId(1), &input(1), Channel::Unreliable);
        server.send(PeerId(1), &goodbye("kept"), Channel::Reliable);
        assert_eq!(messages(clients[0].poll()), [(PeerId(0), goodbye("kept"))]);
    }

    #[test]
    fn a_hang_up_arrives_after_delayed_messages() {
        let opts = LoopbackOptions {
            latency_ticks: 2,
            jitter_ticks: 2,
            seed: 3,
            ..LoopbackOptions::default()
        };
        let (mut server, mut clients) = LoopbackTransport::pair_with(1, opts);
        for tick in 0..5 {
            server.send(PeerId(1), &input(tick), Channel::Unreliable);
        }
        drop(server);
        let mut events = Vec::new();
        for _ in 0..6 {
            events.extend(clients[0].poll());
        }
        assert_eq!(events.last(), Some(&Event::Disconnected(PeerId(0))));
        assert_eq!(input_ticks(events).len(), 5, "all arrived first");
    }
}
