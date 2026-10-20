//! GE-043, GE-049: how big a snapshot is on the wire, and how long
//! encoding and decoding it take, for 100 and 1000 replicated entities;
//! then how much smaller deltas are when 5 % of 1000 entities move each
//! tick.
//!
//! ```text
//! cargo bench -p shin-engine --bench net_snapshot
//! ```
//!
//! Each entity is a typical replicated prop: a name, a non-trivial
//! transform, `builtin://cube`, and a colour, as `capture` would send it.
//! The sizes are exact and deterministic (the guide's "Bandwidth" section
//! quotes them); the timings are a rough guide that depends on the
//! machine. A plain `main` (`harness = false`), so it needs no benchmark
//! framework; `cargo test --all-targets` runs it too, which is quick.

use std::hint::black_box;
use std::time::Instant;

use shin::math::{Quat, Transform, Vec3};
use shin::net::{EntitySnap, Message, NetId, NetTransform, Snapshot, decode, diff, encode, patch};

/// Entity `i` (net id `i + 1`) at `tick`: spread over a field, each turned
/// and scaled a little differently, and, if `moving`, sliding along x by
/// a centimetre per tick.
fn entity(i: u64, tick: u64, moving: bool) -> EntitySnap {
    let f = i as f32;
    let slide = if moving { tick as f32 * 0.01 } else { 0.0 };
    let transform = Transform {
        translation: Vec3::new(f * 0.37 - 20.0 + slide, 0.5 + (f * 0.11).sin(), f * -0.23),
        rotation: Quat::from_euler(glam::EulerRot::XYZ, 0.1 * f, 0.7 * f, 0.03 * f),
        scale: Vec3::splat(0.5 + (i % 4) as f32 * 0.25),
    };
    EntitySnap {
        id: NetId(i + 1),
        parent: None,
        name: Some(format!("crate {}", i + 1)),
        transform: NetTransform::from_transform(&transform),
        mesh: Some("builtin://cube".into()),
        color: Some([0.55, 0.38, 0.2, 1.0]),
        texture: None,
        owner: None,
    }
}

/// A full snapshot at `tick` of `count` entities, of which every
/// `moving_every`-th moves (`0`: none).
fn snapshot(count: u64, tick: u64, moving_every: u64) -> Snapshot {
    Snapshot {
        tick,
        baseline: None,
        entities: (0..count)
            .map(|i| entity(i, tick, moving_every != 0 && i % moving_every == 0))
            .collect(),
        removed: Vec::new(),
    }
}

/// Mean microseconds per call of `f` over `iterations` calls.
fn time_us(iterations: u32, mut f: impl FnMut()) -> f64 {
    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    start.elapsed().as_secs_f64() * 1e6 / f64::from(iterations)
}

fn main() {
    println!("net_snapshot: one full snapshot message (postcard), per entity count");
    for count in [100, 1000] {
        let msg = Message::Snapshot(snapshot(count, 123_456, 0));
        let bytes = encode(&msg);
        assert_eq!(decode(&bytes).expect("round trip"), msg);
        let iterations = 200;
        let encode_us = time_us(iterations, || {
            black_box(encode(black_box(&msg)));
        });
        let decode_us = time_us(iterations, || {
            black_box(decode(black_box(&bytes)).expect("decodes"));
        });
        println!(
            "{count:>5} entities: {:>6} bytes/snapshot ({:.1} bytes/entity), \
             {:.1} KB/s per client at 20 snapshots/s; encode {encode_us:.0} µs, \
             decode {decode_us:.0} µs",
            bytes.len(),
            bytes.len() as f64 / count as f64,
            bytes.len() as f64 * 20.0 / 1000.0,
        );
    }

    println!("net_snapshot: 1000 entities, 5 % (every 20th) moving each tick");
    let (count, moving_every, ticks) = (1000, 20, 60);
    let (mut full_total, mut delta_total) = (0, 0);
    let mut base = snapshot(count, 0, moving_every);
    let mut diff_us = 0.0;
    for tick in 1..=ticks {
        let now = snapshot(count, tick, moving_every);
        let delta = diff(&base, &now);
        assert_eq!(delta.entities.len() as u64, count / moving_every);
        assert_eq!(
            patch(&base, &delta).expect("patches").entities,
            now.entities
        );
        diff_us += time_us(10, || {
            black_box(diff(black_box(&base), black_box(&now)));
        });
        full_total += encode(&Message::Snapshot(now.clone())).len();
        delta_total += encode(&Message::Snapshot(delta)).len();
        base = now;
    }
    let (full, delta) = (
        full_total as f64 / ticks as f64,
        delta_total as f64 / ticks as f64,
    );
    let ratio = full / delta;
    println!(
        "full {full:.0} bytes/tick, delta {delta:.0} bytes/tick: {ratio:.1}x fewer; \
         {:.1} KB/s per client at 20 snapshots/s (was {:.1}); diff {:.0} µs",
        delta * 20.0 / 1000.0,
        full * 20.0 / 1000.0,
        diff_us / ticks as f64,
    );
    assert!(
        ratio >= 10.0,
        "deltas must be at least 10x smaller: {ratio:.1}x"
    );
}
