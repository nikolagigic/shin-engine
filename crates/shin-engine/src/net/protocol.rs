//! What peers say to each other: [`Message`], its payloads, and the
//! binary codec ([`encode`], [`decode`]), which is postcard.
//!
//! A client opens with [`Message::Hello`] carrying [`PROTOCOL_VERSION`];
//! a server that speaks another version answers with the
//! [`Message::Goodbye`] that [`reject_version`] builds and drops the peer.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use super::{NetId, PlayerId};
use crate::scene::TransformDesc;

/// The wire-format version. Bump it whenever [`Message`] or anything it
/// contains changes shape, since postcard is not self-describing.
pub const PROTOCOL_VERSION: u32 = 1;

/// Everything a server and a client send each other.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Message {
    /// Client to server, first: who is joining and which protocol they speak.
    Hello {
        /// The client's [`PROTOCOL_VERSION`].
        protocol: u32,
        /// Display name of the player.
        name: String,
    },
    /// Server to client, after an accepted [`Message::Hello`].
    Welcome {
        /// The id the server gave this player.
        player: PlayerId,
        /// The server's current simulation tick.
        tick: u64,
        /// The server's fixed timestep in seconds.
        fixed_dt: f64,
    },
    /// Server to client: the replicated world at a tick.
    Snapshot(Snapshot),
    /// Client to server: the player's input for a tick.
    Input(InputFrame),
    /// Either way: the connection is closing, and why.
    Goodbye {
        /// Human-readable reason, e.g. a version mismatch.
        reason: String,
    },
}

/// The replicated entities at one tick.
///
/// [`super::apply`] spawns or updates every entity in `entities`, despawns
/// every one in `removed` (with its children), and leaves entities the
/// snapshot does not mention alone.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The server tick this snapshot was captured at.
    pub tick: u64,
    /// Every replicated entity, in [`NetId`] order.
    pub entities: Vec<EntitySnap>,
    /// Entities that stopped being replicated (despawned, or lost
    /// [`super::Replicated`]) since the previous capture.
    pub removed: Vec<NetId>,
}

/// One replicated entity: a flat subset of [`crate::scene::EntityDesc`],
/// with the parent named by [`NetId`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntitySnap {
    /// The entity's network id.
    pub id: NetId,
    /// The parent's network id, if the parent is replicated too.
    pub parent: Option<NetId>,
    /// The entity's [`crate::ecs::Name`].
    pub name: Option<String>,
    /// Local to `parent` when there is one, otherwise world space.
    pub transform: TransformDesc,
    /// Mesh source string (`builtin://cube`, `assets/x.gltf#1`), if the
    /// entity draws one.
    pub mesh: Option<String>,
    /// Linear RGBA colour of the mesh.
    pub color: Option<[f32; 4]>,
    /// Texture source string, if the mesh is textured.
    pub texture: Option<String>,
    /// The player who controls it ([`super::Owner`]).
    pub owner: Option<PlayerId>,
}

/// A player's input for one tick. GE-044 adds the actions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InputFrame {
    /// The client tick the input was sampled at.
    pub tick: u64,
}

/// `msg` as bytes.
pub fn encode(msg: &Message) -> Vec<u8> {
    postcard::to_allocvec(msg).expect("every Message serialises")
}

/// The message in `bytes`. Fails on truncated or malformed input, or
/// bytes left over after the message.
pub fn decode(bytes: &[u8]) -> anyhow::Result<Message> {
    let (msg, rest) = postcard::take_from_bytes(bytes).context("malformed network message")?;
    anyhow::ensure!(
        rest.is_empty(),
        "malformed network message: {} trailing byte(s)",
        rest.len()
    );
    Ok(msg)
}

/// The [`Message::Goodbye`] a server sends a client whose
/// [`Message::Hello`] carries `protocol`, or `None` if it is
/// [`PROTOCOL_VERSION`] and the client may join.
pub fn reject_version(protocol: u32) -> Option<Message> {
    (protocol != PROTOCOL_VERSION).then(|| Message::Goodbye {
        reason: format!(
            "protocol version mismatch: the server speaks {PROTOCOL_VERSION}, \
             the client {protocol}"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        Snapshot {
            tick: 99,
            entities: vec![
                EntitySnap {
                    id: NetId(1),
                    parent: None,
                    name: Some("player".into()),
                    transform: TransformDesc {
                        translation: [1.0, 2.0, -3.5],
                        rotation_euler_deg: [0.0, 45.0, 0.0],
                        scale: [1.0, 2.0, 1.0],
                    },
                    mesh: Some("builtin://cube".into()),
                    color: Some([0.8, 0.1, 0.1, 1.0]),
                    texture: Some("assets/checker.png".into()),
                    owner: Some(PlayerId(3)),
                },
                EntitySnap {
                    id: NetId(2),
                    parent: Some(NetId(1)),
                    name: None,
                    transform: TransformDesc::default(),
                    mesh: None,
                    color: None,
                    texture: None,
                    owner: None,
                },
            ],
            removed: vec![NetId(7), NetId(u64::MAX)],
        }
    }

    /// Every variant, with non-default payloads.
    fn every_message() -> Vec<Message> {
        vec![
            Message::Hello {
                protocol: PROTOCOL_VERSION,
                name: "Ada ✨".into(),
            },
            Message::Welcome {
                player: PlayerId(2),
                tick: 1234,
                fixed_dt: 1.0 / 60.0,
            },
            Message::Snapshot(snapshot()),
            Message::Snapshot(Snapshot::default()),
            Message::Input(InputFrame { tick: 77 }),
            Message::Goodbye {
                reason: "bye".into(),
            },
        ]
    }

    #[test]
    fn every_message_round_trips() {
        for msg in every_message() {
            let bytes = encode(&msg);
            assert_eq!(decode(&bytes).unwrap(), msg, "{bytes:?}");
            assert_eq!(encode(&decode(&bytes).unwrap()), bytes);
        }
    }

    #[test]
    fn malformed_bytes_are_an_error() {
        let bytes = encode(&Message::Snapshot(snapshot()));
        assert!(decode(&bytes[..bytes.len() - 1]).is_err(), "truncated");
        assert!(decode(&[0xff]).is_err(), "unknown variant");
        let mut long = bytes.clone();
        long.push(0);
        let err = decode(&long).unwrap_err().to_string();
        assert!(err.contains("trailing"), "{err}");
    }

    #[test]
    fn another_protocol_version_gets_goodbye() {
        assert_eq!(reject_version(PROTOCOL_VERSION), None);
        let Some(Message::Goodbye { reason }) = reject_version(PROTOCOL_VERSION + 1) else {
            panic!("expected Goodbye");
        };
        assert!(reason.contains("version"), "{reason}");
    }
}
