//! GE-046: `net.host` and `net.join` through the tool registry on two
//! engines in one process, over real QUIC on localhost ([`QuicNetwork`]).
//!
//! Waits are bounded by a generous deadline, never by fixed sleeps.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use shin::headless::Headless;
use shin::math::Transform;
use shin::net::{Networking, Replicated};
use shin::tool::{ToolOutput, ToolRegistry};
use shin_net::QuicNetwork;

/// How long the session may take to come up before the test fails.
const DEADLINE: Duration = Duration::from_secs(10);

fn call(reg: &ToolRegistry, engine: &mut Headless, tool: &str, args: Value) -> Value {
    match reg.call(engine, tool, args) {
        Ok(ToolOutput::Json(value)) => value,
        other => panic!("{tool}: expected JSON, got {other:?}"),
    }
}

fn engine() -> anyhow::Result<Headless> {
    let mut engine = Headless::new(8, 8)?;
    engine.resources.insert(Networking(Box::new(QuicNetwork)));
    engine.world.spawn((Transform::default(), Replicated));
    Ok(engine)
}

#[test]
fn host_and_join_over_quic() -> anyhow::Result<()> {
    let reg = ToolRegistry::with_engine_tools();
    let mut host = engine()?;
    let mut client = engine()?;

    let hosted = call(&reg, &mut host, "net.host", json!({ "port": 0 }));
    let port = hosted["port"].as_u64().expect("a port");
    assert_ne!(port, 0, "port 0 picks a free port: {hosted}");
    call(
        &reg,
        &mut client,
        "net.join",
        json!({ "addr": format!("127.0.0.1:{port}"), "insecure": true }),
    );

    let start = Instant::now();
    loop {
        client.step(1);
        host.step(1);
        let status = call(&reg, &mut host, "net.status", json!({}));
        let joined = call(&reg, &mut client, "net.status", json!({}));
        if status["players"].as_array().is_some_and(|p| p.len() == 1)
            && joined["state"] == "connected"
        {
            assert_eq!(status["role"], "host");
            assert_eq!(joined["role"], "client");
            assert_eq!(joined["player"], status["players"][0]["id"]);
            assert!(status["players"][0]["rtt_ms"].is_f64(), "{status}");
            assert!(joined["players"][0]["rtt_ms"].is_f64(), "{joined}");
            break;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "never joined: {status} {joined}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // quinn counts UDP bytes as its driver sends them, a moment after the
    // tick queued them, so wait for the count to show.
    let start = Instant::now();
    loop {
        host.step(1);
        let status = call(&reg, &mut host, "net.status", json!({}));
        if status["bandwidth"]["out_bytes_per_s"].as_f64().unwrap() > 0.0 {
            break;
        }
        assert!(start.elapsed() < DEADLINE, "no bytes counted: {status}");
        std::thread::sleep(Duration::from_millis(5));
    }

    reg.call(&mut client, "sim.stop", json!({}))?;
    reg.call(&mut host, "sim.stop", json!({}))?;
    assert_eq!(
        call(&reg, &mut host, "net.status", json!({})),
        json!({ "role": "none" })
    );
    assert_eq!(
        call(&reg, &mut client, "net.status", json!({})),
        json!({ "role": "none" })
    );
    Ok(())
}
