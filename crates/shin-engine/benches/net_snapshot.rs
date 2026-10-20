//! GE-043: how big a snapshot is on the wire, and how long encoding and
//! decoding it take, for 100 and 1000 replicated entities.
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
use shin::net::{EntitySnap, Message, NetId, Snapshot, decode, encode};
use shin::scene::TransformDesc;

/// A snapshot at a mid-game tick of `count` entities spread over a field,
/// each turned and scaled a little differently.
fn snapshot(count: u64) -> Snapshot {
    let entities = (0..count)
        .map(|i| {
            let f = i as f32;
            let transform = Transform {
                translation: Vec3::new(f * 0.37 - 20.0, 0.5 + (f * 0.11).sin(), f * -0.23),
                rotation: Quat::from_euler(glam::EulerRot::XYZ, 0.1 * f, 0.7 * f, 0.03 * f),
                scale: Vec3::splat(0.5 + (i % 4) as f32 * 0.25),
            };
            EntitySnap {
                id: NetId(i + 1),
                parent: None,
                name: Some(format!("crate {}", i + 1)),
                transform: TransformDesc::from_transform(&transform),
                mesh: Some("builtin://cube".into()),
                color: Some([0.55, 0.38, 0.2, 1.0]),
                texture: None,
                owner: None,
                mover: None,
            }
        })
        .collect();
    Snapshot {
        tick: 123_456,
        entities,
        removed: Vec::new(),
        last_input_ticks: Vec::new(),
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
        let msg = Message::Snapshot(snapshot(count));
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
}
