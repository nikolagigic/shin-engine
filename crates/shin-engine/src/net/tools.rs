//! Network tools: `net.status`. GE-046 adds hosting and joining; until
//! then the engine is a server only when code inserts a [`NetServer`].

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::NetServer;
use crate::headless::Headless;
use crate::tool::{ToolOutput, ToolRegistry, TypedTool};

/// Registers `net.status`.
pub fn register_tools(reg: &mut ToolRegistry) {
    reg.register(TypedTool::new(
        "net.status",
        "Reports this engine's multiplayer role. { role: \"none\" } when it is \
         not networked. A server reports { role: \"server\", tick, players, \
         bandwidth: { full_bytes_per_s, sent_bytes_per_s, compression_ratio } }: \
         snapshot bytes per second to all remote players over the last second, \
         as full snapshots would cost (before) and as actually sent with deltas \
         (after), and their ratio (null while nothing was sent).",
        net_status,
    ));
}

/// Arguments of `net.status`: none.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

fn net_status(engine: &mut Headless, _: NoArgs) -> anyhow::Result<ToolOutput> {
    let Some(server) = engine.resources.get::<NetServer>() else {
        return Ok(ToolOutput::Json(json!({ "role": "none" })));
    };
    let bandwidth = server.bandwidth();
    Ok(ToolOutput::Json(json!({
        "role": "server",
        "tick": server.tick(),
        "players": server.players().len(),
        "bandwidth": {
            "full_bytes_per_s": bandwidth.full_bytes_per_s,
            "sent_bytes_per_s": bandwidth.sent_bytes_per_s,
            "compression_ratio": bandwidth.compression_ratio(),
        },
    })))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::headless::Headless;
    use crate::math::Transform;
    use crate::net::{
        LoopbackTransport, NetClient, NetServer, Replicated, ServerOptions, net_client_system,
        net_server_system,
    };
    use crate::tool::{ToolOutput, ToolRegistry};

    fn status(reg: &ToolRegistry, engine: &mut Headless) -> anyhow::Result<Value> {
        let ToolOutput::Json(status) = reg.call(engine, "net.status", json!({}))? else {
            panic!("expected JSON");
        };
        Ok(status)
    }

    #[test]
    fn net_status_reports_no_role() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        assert!(reg.list().iter().any(|tool| tool.name == "net.status"));
        let mut engine = Headless::new(8, 8)?;
        assert_eq!(status(&reg, &mut engine)?, json!({ "role": "none" }));
        assert!(
            reg.call(&mut engine, "net.status", json!({ "x": 1 }))
                .is_err()
        );
        Ok(())
    }

    /// A server with 100 replicated entities, 5 of which move every tick,
    /// and one client that acknowledges what it receives: `net.status`
    /// reports the bytes sent per second, what full snapshots would have
    /// cost, and a ratio well above 1. With deltas off the ratio is 1.
    #[test]
    fn net_status_reports_server_bandwidth() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let mut ratios = Vec::new();
        for full_snapshot_every in [60, 1] {
            let (server_end, mut client_ends) = LoopbackTransport::pair(1);
            let mut server = Headless::new(8, 8)?;
            let entities: Vec<_> = (0..100)
                .map(|i| {
                    let at = Transform::from_translation(glam::Vec3::X * i as f32);
                    server.world.spawn((at, Replicated))
                })
                .collect();
            server.resources.insert(NetServer::with_options(
                Box::new(server_end),
                ServerOptions {
                    snapshot_every: 1,
                    full_snapshot_every,
                },
            ));
            server.schedule.add_fixed("net_server", net_server_system);
            // A bare client engine: only its acknowledgements matter here.
            let mut client = Headless::new(8, 8)?;
            client
                .resources
                .insert(NetClient::new(Box::new(client_ends.remove(0)), "ada"));
            client.schedule.add_fixed("net_client", net_client_system);
            server.play()?;
            client.play()?;
            for tick in 0..90 {
                for &entity in entities.iter().step_by(20) {
                    server.world.get::<&mut Transform>(entity)?.translation.y = tick as f32;
                }
                server.step(1);
                client.step(1);
            }
            let status = status(&reg, &mut server)?;
            assert_eq!(status["role"], "server");
            assert_eq!(status["tick"], 90);
            assert_eq!(status["players"], 1);
            let bandwidth = &status["bandwidth"];
            let full = bandwidth["full_bytes_per_s"].as_f64().unwrap();
            let sent = bandwidth["sent_bytes_per_s"].as_f64().unwrap();
            assert!(full > 0.0 && sent > 0.0, "{status}");
            ratios.push(bandwidth["compression_ratio"].as_f64().unwrap());
        }
        assert!(ratios[0] > 5.0, "deltas on: {ratios:?}");
        assert_eq!(ratios[1], 1.0, "deltas off");
        Ok(())
    }
}
