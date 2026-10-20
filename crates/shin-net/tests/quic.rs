//! GE-045: a QUIC server and client on localhost, in one process.
//!
//! Waits are bounded by generous deadlines (the network is real, if
//! local), never by fixed sleeps, so a slow CI machine only takes longer.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use shin::ecs::{Resources, World};
use shin::math::Transform;
use shin::net::{
    Channel, EntitySnap, Event, InputFrame, Message, NetId, NetServer, PROTOCOL_VERSION, PeerId,
    PlayerId, Replicated, Snapshot, Transport, net_server_system,
};
use shin::scene::TransformDesc;
use shin_net::{QuicTransport, ServerTrust};

/// How long any one exchange may take before the test fails.
const DEADLINE: Duration = Duration::from_secs(10);

/// The minimum share of unreliable messages that must arrive on
/// localhost. Loopback practically never drops a datagram; the floor
/// leaves room for a busy CI machine overflowing a socket buffer.
const UNRELIABLE_FLOOR: f64 = 0.90;

/// A server on a free localhost port, and its address.
fn server() -> (QuicTransport, SocketAddr) {
    let server = QuicTransport::listen("127.0.0.1:0".parse().unwrap()).expect("listen");
    let addr = server.local_addr().unwrap();
    (server, addr)
}

/// A client connected to `addr`, trusting any certificate.
fn client(addr: SocketAddr) -> QuicTransport {
    QuicTransport::connect(addr, &ServerTrust::Insecure).expect("connect")
}

/// Polls `transport` until `done` holds for everything received so far, which it returns.
fn poll_until(
    transport: &mut impl Transport,
    mut done: impl FnMut(&[Event]) -> bool,
) -> Vec<Event> {
    let start = Instant::now();
    let mut events = Vec::new();
    loop {
        events.extend(transport.poll());
        if done(&events) {
            return events;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "timed out; received so far: {events:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The messages in `events`, without their senders.
fn messages(events: &[Event]) -> Vec<Message> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Message(_, msg) => Some(msg.clone()),
            _ => None,
        })
        .collect()
}

fn hello(protocol: u32) -> Message {
    Message::Hello {
        protocol,
        name: "tester".into(),
    }
}

/// A world with one replicated entity and a [`NetServer`] on `transport`.
fn server_engine(transport: QuicTransport) -> (World, Resources) {
    let mut world = World::new();
    world.spawn((Replicated, Transform::default()));
    let mut res = Resources::new();
    res.insert(NetServer::new(Box::new(transport), 1));
    (world, res)
}

/// Runs the server's system and polls `client` until `done` holds.
fn tick_until(
    world: &mut World,
    res: &mut Resources,
    client: &mut QuicTransport,
    mut done: impl FnMut(&[Event]) -> bool,
) -> Vec<Event> {
    let start = Instant::now();
    let mut events = Vec::new();
    loop {
        net_server_system(world, res);
        events.extend(client.poll());
        if done(&events) {
            return events;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "timed out; client received: {events:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn hello_welcome_snapshots_and_clean_disconnect() {
    let (transport, addr) = server();
    let (mut world, mut res) = server_engine(transport);
    let mut client = client(addr);

    let events = poll_until(&mut client, |e| e.contains(&Event::Connected(PeerId(0))));
    assert_eq!(events, [Event::Connected(PeerId(0))]);
    assert_eq!(client.peers(), [PeerId(0)]);
    client.send(PeerId(0), &hello(PROTOCOL_VERSION), Channel::Reliable);

    let events = tick_until(&mut world, &mut res, &mut client, |e| {
        messages(e)
            .iter()
            .any(|m| matches!(m, Message::Snapshot(s) if !s.entities.is_empty()))
    });
    let msgs = messages(&events);
    assert!(
        matches!(
            msgs.first(),
            Some(Message::Welcome {
                player: PlayerId(1),
                ..
            })
        ),
        "{msgs:?}"
    );
    assert_eq!(res.expect::<NetServer>().players(), [PlayerId(1)]);

    // Dropping the client closes its connection; the server notices and
    // the player leaves.
    drop(client);
    let start = Instant::now();
    while !res.expect::<NetServer>().players().is_empty() {
        assert!(start.elapsed() < DEADLINE, "the player never left");
        net_server_system(&mut world, &mut res);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn mismatched_protocol_version_gets_goodbye() {
    let (transport, addr) = server();
    let (mut world, mut res) = server_engine(transport);
    let mut client = client(addr);
    client.send(PeerId(0), &hello(PROTOCOL_VERSION + 1), Channel::Reliable);

    let events = tick_until(&mut world, &mut res, &mut client, |e| {
        messages(e)
            .iter()
            .any(|m| matches!(m, Message::Goodbye { .. }))
    });
    let msgs = messages(&events);
    let [Message::Goodbye { reason }] = msgs.as_slice() else {
        panic!("expected only a goodbye, got {msgs:?}");
    };
    assert!(reason.contains("version"), "{reason}");
    assert_eq!(res.expect::<NetServer>().players(), []);
}

/// Sends 100 unreliable inputs from `from` to `to_peer` and returns how
/// many arrive at `to`.
fn unreliable_arrivals(from: &mut QuicTransport, to_peer: PeerId, to: &mut QuicTransport) -> usize {
    const SENT: u64 = 100;
    for tick in 0..SENT {
        from.send(
            to_peer,
            &Message::Input(InputFrame {
                tick,
                actions: Vec::new(),
            }),
            Channel::Unreliable,
        );
    }
    // Wait until all arrive, or for a quiet second after the last one.
    let mut ticks = Vec::new();
    let mut last = Instant::now();
    while ticks.len() < SENT as usize && last.elapsed() < Duration::from_secs(1) {
        for event in to.poll() {
            if let Event::Message(_, Message::Input(frame)) = event {
                ticks.push(frame.tick);
                last = Instant::now();
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    ticks.sort_unstable();
    ticks.dedup();
    assert!(ticks.iter().all(|&t| t < SENT), "{ticks:?}");
    ticks.len()
}

#[test]
fn unreliable_messages_arrive_on_localhost() {
    let (mut server, addr) = server();
    let mut client = client(addr);
    poll_until(&mut client, |e| !e.is_empty());
    poll_until(&mut server, |e| e.contains(&Event::Connected(PeerId(1))));

    let floor = (100.0 * UNRELIABLE_FLOOR) as usize;
    let up = unreliable_arrivals(&mut client, PeerId(0), &mut server);
    assert!(up >= floor, "client to server: {up} of 100 arrived");
    let down = unreliable_arrivals(&mut server, PeerId(1), &mut client);
    assert!(down >= floor, "server to client: {down} of 100 arrived");
    eprintln!("unreliable on localhost: {up}/100 up, {down}/100 down (floor {floor})");
}

#[test]
fn an_unreliable_message_too_big_for_a_datagram_still_arrives() {
    let (mut server, addr) = server();
    let mut client = client(addr);
    poll_until(&mut server, |e| e.contains(&Event::Connected(PeerId(1))));

    let entities = (1..=200)
        .map(|id| EntitySnap {
            id: NetId(id),
            parent: None,
            name: Some(format!("crate {id}")),
            transform: TransformDesc::default(),
            mesh: Some("builtin://cube".into()),
            color: None,
            texture: None,
            owner: None,
            mover: None,
        })
        .collect();
    let big = Message::Snapshot(Snapshot {
        tick: 5,
        entities,
        removed: vec![],
        last_input_ticks: Vec::new(),
    });
    assert!(
        shin::net::encode(&big).len() > 1500,
        "bigger than any datagram"
    );
    server.broadcast(&big, Channel::Unreliable);
    let events = poll_until(&mut client, |e| !messages(e).is_empty());
    assert_eq!(messages(&events), [big]);
}

#[test]
fn a_goodbye_sent_just_before_dropping_arrives() {
    let (mut server, addr) = server();
    let mut client = client(addr);
    poll_until(&mut server, |e| e.contains(&Event::Connected(PeerId(1))));
    let bye = Message::Goodbye {
        reason: "shutting down".into(),
    };
    server.send(PeerId(1), &bye, Channel::Reliable);
    drop(server);

    let events = poll_until(&mut client, |e| e.contains(&Event::Disconnected(PeerId(0))));
    assert_eq!(
        events,
        [
            Event::Connected(PeerId(0)),
            Event::Message(PeerId(0), bye),
            Event::Disconnected(PeerId(0)),
        ]
    );
    assert_eq!(client.peers(), []);
}

#[test]
fn clients_are_numbered_in_order_and_trust_the_server_certificate() {
    let (mut server, addr) = server();
    let cert = server
        .cert_der()
        .expect("a server has a certificate")
        .to_vec();
    let _first = QuicTransport::connect(addr, &ServerTrust::Cert(cert.clone())).expect("trusted");
    poll_until(&mut server, |e| e.contains(&Event::Connected(PeerId(1))));
    let _second = QuicTransport::connect(addr, &ServerTrust::Cert(cert)).expect("trusted");
    poll_until(&mut server, |e| e.contains(&Event::Connected(PeerId(2))));
    assert_eq!(server.peers(), [PeerId(1), PeerId(2)]);

    // A client that trusts another server's certificate refuses this one.
    let (other, _) = self::server();
    let wrong = other.cert_der().unwrap().to_vec();
    let err = QuicTransport::connect(addr, &ServerTrust::Cert(wrong))
        .err()
        .expect("an untrusted certificate is refused");
    assert!(format!("{err:#}").contains("connecting"), "{err:#}");
}

#[test]
fn connecting_to_nobody_fails() {
    // Bind a port, then free it, so nothing listens there.
    let addr = server().1;
    let start = Instant::now();
    assert!(QuicTransport::connect(addr, &ServerTrust::Insecure).is_err());
    assert!(start.elapsed() < DEADLINE);
}

#[test]
fn transports_are_send() {
    fn assert_send<T: Send>() {}
    assert_send::<QuicTransport>();
}
