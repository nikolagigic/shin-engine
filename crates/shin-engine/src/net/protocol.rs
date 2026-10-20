//! What peers say to each other: [`Message`], its payloads, and the
//! binary codec ([`encode`], [`decode`]), which is postcard.
//!
//! A client opens with [`Message::Hello`] carrying [`PROTOCOL_VERSION`];
//! a server that speaks another version answers with the
//! [`Message::Goodbye`] that [`reject_version`] builds and drops the peer.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use super::{NetId, PlayerId};
use crate::input::ActionValue;
use crate::math::{Quat, Transform, Vec3};

/// The wire-format version. Bump it whenever [`Message`] or anything it
/// contains changes shape, since postcard is not self-describing.
pub const PROTOCOL_VERSION: u32 = 3;

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

/// The replicated entities at one tick: a full snapshot, or a delta
/// against an earlier one (GE-049).
///
/// A **full** snapshot (`baseline: None`) lists every replicated entity.
/// A **delta** (`baseline: Some(b)`) lists only the entities that are new
/// or whose [`EntitySnap`] differs from the snapshot of tick `b`, and in
/// `removed` every entity of `b` that is gone; [`super::snapshot::diff`]
/// makes one and [`super::snapshot::patch`] turns it back into a full
/// snapshot. The server sends deltas against the newest snapshot the
/// client acknowledged ([`InputFrame::ack`]).
///
/// [`super::apply`] takes full snapshots: it spawns or updates every
/// entity in `entities`, despawns every one in `removed` (with its
/// children), and leaves entities the snapshot does not mention alone.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The server tick this snapshot was captured at.
    pub tick: u64,
    /// For a delta, the tick of the snapshot it is relative to; `None`
    /// for a full snapshot.
    pub baseline: Option<u64>,
    /// Every replicated entity (full) or the changed ones (delta), in
    /// [`NetId`] order.
    pub entities: Vec<EntitySnap>,
    /// Entities that stopped being replicated (despawned, or lost
    /// [`super::Replicated`]): since the previous capture in a full
    /// snapshot, since the baseline in a delta.
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
    pub transform: NetTransform,
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

/// A [`Transform`] as it travels: translation and scale as `f32`, the
/// rotation packed into 7 bytes ("smallest three", 50 of the 56 bits used).
///
/// The rotation's largest quaternion component is dropped (its sign is
/// made positive, since `q` and `-q` are the same rotation) and rebuilt
/// from the other three, which lie in `±1/√2`. Those are stored in 16 bits
/// each, as one of the 65535 steps from `-1/√2` to `1/√2` (so `0` is
/// exact), and 2 bits say which component was dropped. A component is
/// off by at most half a step (`1.1e-5`); a rotation comes back within
/// 0.004° of the original (worst case measured over a million random
/// rotations: 0.0037°; `net::protocol` tests check it). Rotations about
/// one axis by multiples of 90°, the identity included, come back
/// exactly. The rotation takes 7 bytes on the wire, against 12 for the
/// three `f32` Euler angles of [`crate::scene::TransformDesc`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetTransform {
    /// Position.
    pub translation: [f32; 3],
    /// Rotation, smallest-three packed, little-endian (see the type docs).
    pub rotation: [u8; 7],
    /// Per-axis scale.
    pub scale: [f32; 3],
}

impl Default for NetTransform {
    /// The identity transform.
    fn default() -> Self {
        Self::from_transform(&Transform::default())
    }
}

/// Quantization steps from `0` to `±1/√2`: a component is stored as
/// `-STEPS..=STEPS`, offset by `STEPS` into 16 bits.
const STEPS: f32 = 32767.0;

impl NetTransform {
    /// `t` quantized for the wire.
    pub fn from_transform(t: &Transform) -> Self {
        Self {
            translation: t.translation.to_array(),
            rotation: pack_rotation(t.rotation),
            scale: t.scale.to_array(),
        }
    }

    /// The transform this describes.
    pub fn to_transform(&self) -> Transform {
        Transform {
            translation: Vec3::from(self.translation),
            rotation: unpack_rotation(self.rotation),
            scale: Vec3::from(self.scale),
        }
    }
}

/// `q` (normalized first) packed as smallest three: bits 48..50 name the
/// dropped component, bits 32..48, 16..32, and 0..16 hold the other three
/// in order.
fn pack_rotation(q: Quat) -> [u8; 7] {
    let mut c = q.normalize().to_array();
    if !c.iter().all(|v| v.is_finite()) {
        c = Quat::IDENTITY.to_array();
    }
    let largest = (0..4)
        .max_by(|&a, &b| c[a].abs().total_cmp(&c[b].abs()))
        .expect("four components");
    if c[largest] < 0.0 {
        c = c.map(|v| -v);
    }
    let mut packed = largest as u64;
    for (i, v) in c.into_iter().enumerate() {
        if i != largest {
            let step = (v * std::f32::consts::SQRT_2 * STEPS)
                .round()
                .clamp(-STEPS, STEPS);
            packed = (packed << 16) | (step + STEPS) as u64;
        }
    }
    let bytes = packed.to_le_bytes();
    std::array::from_fn(|i| bytes[i])
}

/// The rotation [`pack_rotation`] packed into `bytes`.
fn unpack_rotation(bytes: [u8; 7]) -> Quat {
    let mut wide = [0; 8];
    wide[..7].copy_from_slice(&bytes);
    let bits = u64::from_le_bytes(wide);
    let largest = ((bits >> 48) & 0b11) as usize;
    let mut c = [0.0_f32; 4];
    let mut shift = 48;
    for (i, slot) in c.iter_mut().enumerate() {
        if i != largest {
            shift -= 16;
            let step = ((bits >> shift) & 0xffff) as f32 - STEPS;
            *slot = step / STEPS * std::f32::consts::FRAC_1_SQRT_2;
        }
    }
    let rest: f32 = c.iter().map(|v| v * v).sum();
    c[largest] = (1.0 - rest).max(0.0).sqrt();
    Quat::from_array(c).normalize()
}

/// A player's input for one client tick (GE-044): the client's
/// [`crate::input::Actions`] as they stood when it was sent.
///
/// Only actions that are not at rest are listed (held, off-centre, or
/// just pressed); an action missing from the frame is released with axis
/// `0`. So every frame is complete on its own, and a lost frame is
/// corrected by the next one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InputFrame {
    /// The client tick the input was sampled at. Counts up, so the server
    /// can tell the newest frame and drop late ones.
    pub tick: u64,
    /// Every action not at rest, by name, in name order.
    pub actions: Vec<(String, ActionValue)>,
    /// The newest server snapshot tick this client holds in full, if any
    /// (GE-049). The server sends deltas against it.
    pub ack: Option<u64>,
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
            baseline: Some(96),
            entities: vec![
                EntitySnap {
                    id: NetId(1),
                    parent: None,
                    name: Some("player".into()),
                    transform: NetTransform::from_transform(&Transform {
                        translation: Vec3::new(1.0, 2.0, -3.5),
                        rotation: Quat::from_rotation_y(45f32.to_radians()),
                        scale: Vec3::new(1.0, 2.0, 1.0),
                    }),
                    mesh: Some("builtin://cube".into()),
                    color: Some([0.8, 0.1, 0.1, 1.0]),
                    texture: Some("assets/checker.png".into()),
                    owner: Some(PlayerId(3)),
                },
                EntitySnap {
                    id: NetId(2),
                    parent: Some(NetId(1)),
                    name: None,
                    transform: NetTransform::default(),
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
            Message::Input(InputFrame::default()),
            Message::Input(InputFrame {
                tick: 77,
                actions: vec![
                    (
                        "jump".into(),
                        ActionValue {
                            pressed: true,
                            just_pressed: true,
                            axis: 1.0,
                        },
                    ),
                    (
                        "move_x".into(),
                        ActionValue {
                            pressed: false,
                            just_pressed: false,
                            axis: -0.25,
                        },
                    ),
                ],
                ack: Some(75),
            }),
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

    /// The angle in degrees between two rotations.
    /// Computed in `f64` from the relative rotation's vector part:
    /// `acos` of an `f32` dot product cannot resolve angles below ~0.04°.
    fn angle_deg(a: Quat, b: Quat) -> f32 {
        let d = a.as_dquat().inverse() * b.as_dquat();
        (2.0 * d.xyz().length().atan2(d.w.abs())).to_degrees() as f32
    }

    #[test]
    fn rotations_survive_packing_within_the_documented_precision() {
        // A deterministic spread of rotations: a small LCG drives the
        // axis and angle.
        let mut state: u64 = 49;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 40) as f32 / (1u64 << 24) as f32
        };
        let mut worst: f32 = 0.0;
        for _ in 0..100_000 {
            let axis = Vec3::new(next() - 0.5, next() - 0.5, next() - 0.5);
            let q = Quat::from_axis_angle(axis.normalize(), next() * std::f32::consts::TAU);
            worst = worst.max(angle_deg(q, unpack_rotation(pack_rotation(q))));
        }
        assert!(worst < 0.004, "worst error {worst}°");
        assert!(worst > 0.0, "packing is lossy");
    }

    #[test]
    fn axis_rotations_by_quarter_turns_pack_exactly() {
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            for quarter in 0..4 {
                let angle = quarter as f32 * std::f32::consts::FRAC_PI_2;
                let q = Quat::from_axis_angle(axis, angle);
                let back = unpack_rotation(pack_rotation(q));
                assert!(angle_deg(q, back) < 1e-3, "{axis} by {quarter}/4 turn");
            }
        }
        let t = Transform::default();
        assert_eq!(NetTransform::from_transform(&t).to_transform(), t);
    }

    #[test]
    fn packing_ignores_the_sign_and_length_of_the_quaternion() {
        let q = Quat::from_euler(glam::EulerRot::XYZ, 0.3, -1.2, 2.0);
        assert_eq!(pack_rotation(q), pack_rotation(-q));
        assert_eq!(pack_rotation(q), pack_rotation(q * 3.0));
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
