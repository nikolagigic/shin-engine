//! Network tools (GE-046): `net.host`, `net.join`, `net.disconnect`,
//! `net.status`, and `net.players`. None of them is recorded in the
//! command history: hosting and joining are not edits.
//!
//! Hosting and joining need a [`Networking`](super::Networking) resource;
//! see [`super::session`].

use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::session::{self, CERT_FILE, DEFAULT_PORT, NetSession, Role};
use super::{ClientState, NetClient, NetServer, ServerOptions};
use crate::ecs::Time;
use crate::headless::Headless;
use crate::project::ProjectRoot;
use crate::tool::{ToolOutput, ToolRegistry, TypedTool};

/// Registers `net.host`, `net.join`, `net.disconnect`, `net.status`, and
/// `net.players`.
pub fn register_tools(reg: &mut ToolRegistry) {
    reg.register(TypedTool::new(
        "net.host",
        "Starts hosting a multiplayer session on this engine, which becomes the server \
         (with its own local player), and starts play mode as sim.play does. Clients join \
         on UDP `port` (default 7777); a snapshot goes out every `snapshot_every` ticks \
         (default 3). The server certificate is saved to target/shin-server-cert.der in the \
         project. Fails if a session is already running. Returns net.status.",
        net_host,
    ));
    reg.register(TypedTool::new(
        "net.join",
        "Joins the multiplayer server at `addr` (host:port) as a client and starts play \
         mode; the server's replicated entities appear in the world. Trust: `insecure: \
         true` accepts any server (development only), `cert_path` trusts exactly that \
         certificate file, and with neither the project's target/shin-server-cert.der (what \
         a host on this machine saved) is trusted. Fails if a session is already running or \
         the server does not answer. Returns net.status.",
        net_join,
    ));
    reg.register(TypedTool::new(
        "net.disconnect",
        "Ends the multiplayer session: a host says goodbye to every player and stops \
         serving; a client leaves the server and its replicated entities are removed. Play \
         mode continues (sim.stop also ends the session). Returns `disconnected` (false if \
         there was no session) and the `role` before.",
        net_disconnect,
    ));
    reg.register(TypedTool::new(
        "net.status",
        "Reports this engine's multiplayer session: `role` is \"none\", \"host\", or \
         \"client\". In a session also `tick` (the server tick), `players` (the players \
         connected over the network, each `{ id, rtt_ms }`: on a host every joined client, \
         on a client itself once welcomed), `bytes_in_per_s` and `bytes_out_per_s` (over the \
         last simulated second), and `bandwidth` (bytes in plus out per tick, oldest first). \
         A host adds `port`, `cert_path`, and `local_player`; a client adds `addr`, `state` \
         (connecting, connected, or disconnected), `reason` when disconnected, and `player`.",
        net_status,
    ));
    reg.register(TypedTool::new(
        "net.players",
        "Lists the players in this engine's multiplayer session as `players`, each \
         `{ id, local, rtt_ms }`: on a host every player including its own (local: true); on \
         a client only itself, once welcomed. Empty without a session.",
        net_players,
    ));
}

/// Arguments of `net.host`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HostArgs {
    /// The UDP port clients join (default 7777; 0 picks a free one).
    port: Option<u16>,
    /// Send a snapshot every this many ticks (default 3: 20 per second at 60 Hz).
    snapshot_every: Option<u32>,
}

/// Arguments of `net.join`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct JoinArgs {
    /// The server's address, `host:port` (e.g. `127.0.0.1:7777`).
    addr: String,
    /// Accept any server certificate. Development only.
    insecure: Option<bool>,
    /// The server's certificate file (DER) to trust; relative to the project.
    cert_path: Option<String>,
    /// The player name the server sees (default "player").
    name: Option<String>,
}

/// Arguments of a tool that takes none.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

fn net_host(engine: &mut Headless, args: HostArgs) -> anyhow::Result<ToolOutput> {
    let every = args
        .snapshot_every
        .unwrap_or(ServerOptions::default().snapshot_every);
    session::host(engine, args.port.unwrap_or(DEFAULT_PORT), every)?;
    Ok(ToolOutput::Json(status(engine)))
}

fn net_join(engine: &mut Headless, args: JoinArgs) -> anyhow::Result<ToolOutput> {
    let cert = trusted_cert(engine, args.insecure.unwrap_or(false), args.cert_path)?;
    let name = args.name.as_deref().unwrap_or("player");
    session::join(engine, &args.addr, cert.as_deref(), name)?;
    Ok(ToolOutput::Json(status(engine)))
}

fn net_disconnect(engine: &mut Headless, _: NoArgs) -> anyhow::Result<ToolOutput> {
    let role = role(engine);
    let disconnected = session::end(engine);
    Ok(ToolOutput::Json(
        json!({ "disconnected": disconnected, "role": role }),
    ))
}

fn net_status(engine: &mut Headless, _: NoArgs) -> anyhow::Result<ToolOutput> {
    Ok(ToolOutput::Json(status(engine)))
}

fn net_players(engine: &mut Headless, _: NoArgs) -> anyhow::Result<ToolOutput> {
    let res = &engine.resources;
    let players: Vec<Value> = match res.get::<NetSession>().map(|s| &s.role) {
        Some(Role::Host { .. }) => {
            let server = res.expect::<NetServer>();
            let remote = server.remote_players();
            server
                .players()
                .into_iter()
                .map(|id| {
                    let rtt = remote.iter().find(|(p, _)| *p == id).and_then(|(_, r)| *r);
                    let local = server.local_player() == Some(id);
                    json!({ "id": id.0, "local": local, "rtt_ms": ms(rtt) })
                })
                .collect()
        }
        Some(Role::Client { .. }) => client_player(res.expect::<NetClient>())
            .map(|player| vec![json!({ "id": player.0, "local": true, "rtt_ms": ms(res.expect::<NetClient>().rtt()) })])
            .unwrap_or_default(),
        None => Vec::new(),
    };
    Ok(ToolOutput::Json(json!({ "players": players })))
}

/// The certificate `net.join` trusts: none (any server) with `insecure`,
/// the file at `cert_path`, or the project's [`CERT_FILE`].
fn trusted_cert(
    engine: &Headless,
    insecure: bool,
    cert_path: Option<String>,
) -> anyhow::Result<Option<Vec<u8>>> {
    let root = engine.resources.get::<ProjectRoot>();
    let path = match (insecure, cert_path) {
        (true, Some(_)) => anyhow::bail!("pass `insecure` or `cert_path`, not both"),
        (true, None) => return Ok(None),
        (false, Some(path)) => match root {
            Some(root) => root.0.join(path),
            None => Path::new(&path).to_path_buf(),
        },
        (false, None) => match root {
            Some(root) if root.0.join(CERT_FILE).exists() => root.0.join(CERT_FILE),
            _ => anyhow::bail!(
                "no server certificate to trust: pass `cert_path` (the file the host saved, \
                 {CERT_FILE} in its project) or `insecure: true` (development only)"
            ),
        },
    };
    let der = std::fs::read(&path)
        .with_context(|| format!("reading the server certificate {}", path.display()))?;
    Ok(Some(der))
}

/// `"none"`, `"host"`, or `"client"`.
fn role(engine: &Headless) -> &'static str {
    match engine.resources.get::<NetSession>().map(|s| &s.role) {
        None => "none",
        Some(Role::Host { .. }) => "host",
        Some(Role::Client { .. }) => "client",
    }
}

/// The player a client was welcomed as, if it was.
fn client_player(client: &NetClient) -> Option<super::PlayerId> {
    match client.state() {
        ClientState::Connected { player, .. } => Some(*player),
        _ => None,
    }
}

/// A round-trip time in milliseconds, or `null`.
fn ms(rtt: Option<Duration>) -> Value {
    rtt.map_or(Value::Null, |rtt| json!(rtt.as_secs_f64() * 1000.0))
}

/// What `net.status` returns.
fn status(engine: &Headless) -> Value {
    let res = &engine.resources;
    let Some(session) = res.get::<NetSession>() else {
        return json!({ "role": "none" });
    };
    let per_tick = session.per_tick();
    let fixed_dt = res.get::<Time>().map_or(engine.fixed_dt, |t| t.fixed_dt);
    let seconds = per_tick.len() as f64
        * if fixed_dt > 0.0 {
            fixed_dt
        } else {
            engine.fixed_dt
        };
    let rate = |bytes: u64| {
        if seconds > 0.0 {
            bytes as f64 / seconds
        } else {
            0.0
        }
    };
    let (bytes_in, bytes_out) = per_tick
        .iter()
        .fold((0, 0), |(i, o), (r, s)| (i + r, o + s));
    let bandwidth: Vec<u64> = per_tick.iter().map(|(r, s)| r + s).collect();
    let mut status = json!({
        "role": role(engine),
        "bytes_in_per_s": rate(bytes_in),
        "bytes_out_per_s": rate(bytes_out),
        "bandwidth": bandwidth,
    });
    let fields = match &session.role {
        Role::Host { port, cert_path } => {
            let server = res.expect::<NetServer>();
            let players: Vec<Value> = server
                .remote_players()
                .into_iter()
                .map(|(id, rtt)| json!({ "id": id.0, "rtt_ms": ms(rtt) }))
                .collect();
            json!({
                "port": port,
                "cert_path": cert_path.as_ref().map(|p| p.display().to_string()),
                "tick": server.tick(),
                "local_player": server.local_player().map(|p| p.0),
                "players": players,
            })
        }
        Role::Client { addr } => {
            let client = res.expect::<NetClient>();
            let (state, reason) = session::client_state(client);
            let tick = match client.state() {
                ClientState::Connected { server_tick, .. } => Some(*server_tick),
                _ => None,
            };
            let player = client_player(client);
            let players: Vec<Value> = player
                .map(|id| json!({ "id": id.0, "rtt_ms": ms(client.rtt()) }))
                .into_iter()
                .collect();
            json!({
                "addr": addr,
                "state": state,
                "reason": reason,
                "tick": tick,
                "player": player.map(|p| p.0),
                "players": players,
            })
        }
    };
    if let (Value::Object(status), Value::Object(fields)) = (&mut status, fields) {
        status.extend(fields);
    }
    status
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::headless::Headless;
    use crate::math::Transform;
    use crate::net::{LoopbackNetwork, Networking, Replicated};
    use crate::tool::{ToolOutput, ToolRegistry};

    fn call(reg: &ToolRegistry, engine: &mut Headless, tool: &str, args: Value) -> Value {
        match reg.call(engine, tool, args) {
            Ok(ToolOutput::Json(value)) => value,
            other => panic!("{tool}: expected JSON, got {other:?}"),
        }
    }

    /// An engine on `net`, with one replicated entity.
    fn engine(net: &LoopbackNetwork) -> anyhow::Result<Headless> {
        let mut engine = Headless::new(8, 8)?;
        engine.resources.insert(Networking(Box::new(net.clone())));
        engine.world.spawn((Transform::default(), Replicated));
        Ok(engine)
    }

    #[test]
    fn net_tools_are_listed_and_status_starts_at_none() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        for name in [
            "net.host",
            "net.join",
            "net.disconnect",
            "net.status",
            "net.players",
        ] {
            assert!(reg.list().iter().any(|tool| tool.name == name), "{name}");
        }
        let mut engine = Headless::new(8, 8)?;
        assert_eq!(
            call(&reg, &mut engine, "net.status", json!({})),
            json!({ "role": "none" })
        );
        assert!(
            reg.call(&mut engine, "net.status", json!({ "x": 1 }))
                .is_err()
        );
        assert_eq!(
            call(&reg, &mut engine, "net.players", json!({})),
            json!({ "players": [] })
        );
        let err = reg
            .call(&mut engine, "net.host", json!({}))
            .expect_err("no network");
        assert!(err.to_string().contains("no network"), "{err}");
        Ok(())
    }

    /// The acceptance test: host on one engine, join from a second, and
    /// both report their role and the one joined player.
    #[test]
    fn host_and_join_through_the_registry() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let net = LoopbackNetwork::new();
        let mut host = engine(&net)?;
        let mut client = engine(&net)?;

        let hosted = call(
            &reg,
            &mut host,
            "net.host",
            json!({ "port": 7777, "snapshot_every": 1 }),
        );
        assert_eq!(hosted["role"], "host");
        assert_eq!(hosted["port"], 7777);
        assert!(host.play_state().is_playing(), "net.host implies sim.play");
        assert!(
            reg.call(&mut host, "net.host", json!({})).is_err(),
            "already hosting"
        );

        let joined = call(
            &reg,
            &mut client,
            "net.join",
            json!({ "addr": "127.0.0.1:7777", "insecure": true }),
        );
        assert_eq!(
            (joined["role"].clone(), joined["state"].clone()),
            (json!("client"), json!("connecting"))
        );
        assert!(client.play_state().is_playing());

        for _ in 0..6 {
            client.step(1);
            host.step(1);
        }

        let status = call(&reg, &mut host, "net.status", json!({}));
        assert_eq!(status["role"], "host");
        assert_eq!(status["local_player"], 1);
        assert_eq!(status["players"], json!([{ "id": 2, "rtt_ms": null }]));
        assert_eq!(status["tick"], 6);
        assert!(
            status["bytes_out_per_s"].as_f64().unwrap() > 0.0,
            "{status}"
        );
        assert!(status["bytes_in_per_s"].as_f64().unwrap() > 0.0, "{status}");
        assert_eq!(status["bandwidth"].as_array().unwrap().len(), 5);

        let status = call(&reg, &mut client, "net.status", json!({}));
        assert_eq!(status["role"], "client");
        assert_eq!(status["state"], "connected");
        assert_eq!(status["player"], 2);
        assert_eq!(status["players"], json!([{ "id": 2, "rtt_ms": null }]));
        assert!(status["tick"].as_u64().unwrap() >= 3, "{status}");

        assert_eq!(
            call(&reg, &mut host, "net.players", json!({}))["players"],
            json!([
                { "id": 1, "local": true, "rtt_ms": null },
                { "id": 2, "local": false, "rtt_ms": null },
            ])
        );
        assert_eq!(
            call(&reg, &mut client, "net.players", json!({}))["players"],
            json!([{ "id": 2, "local": true, "rtt_ms": null }])
        );

        // The host's replicated entity reached the client.
        let remote = client
            .world
            .query::<&crate::net::RemoteEntity>()
            .iter()
            .count();
        assert_eq!(remote, 1);

        // The client leaves: its replica goes, and the host sees it go.
        let left = call(&reg, &mut client, "net.disconnect", json!({}));
        assert_eq!(left, json!({ "disconnected": true, "role": "client" }));
        assert_eq!(
            call(&reg, &mut client, "net.status", json!({})),
            json!({ "role": "none" })
        );
        assert_eq!(
            client
                .world
                .query::<&crate::net::RemoteEntity>()
                .iter()
                .count(),
            0
        );
        host.step(1);
        assert_eq!(
            call(&reg, &mut host, "net.status", json!({}))["players"],
            json!([])
        );

        // Stopping play ends hosting.
        reg.call(&mut host, "sim.stop", json!({}))?;
        assert_eq!(
            call(&reg, &mut host, "net.status", json!({})),
            json!({ "role": "none" })
        );
        assert_eq!(
            call(&reg, &mut host, "net.disconnect", json!({})),
            json!({ "disconnected": false, "role": "none" })
        );
        // The port is free again.
        call(&reg, &mut host, "net.host", json!({ "port": 7777 }));
        Ok(())
    }

    #[test]
    fn hosting_puts_back_the_server_it_replaced_and_is_not_recorded() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let net = LoopbackNetwork::new();
        let mut host = engine(&net)?;
        host.resources.insert(crate::net::NetServer::solo());
        crate::command::apply(&reg, &mut host, "net.host", json!({ "port": 0 }))?;
        assert!(
            host.resources
                .get::<crate::command::History>()
                .is_none_or(|history| history.undo_len() == 0),
            "hosting is not an edit"
        );
        host.step(3);
        assert_eq!(host.resources.expect::<crate::net::NetServer>().tick(), 3);
        reg.call(&mut host, "net.disconnect", json!({}))?;
        assert_eq!(
            host.resources.expect::<crate::net::NetServer>().tick(),
            0,
            "the solo server is back"
        );
        assert!(host.play_state().is_playing(), "disconnect keeps playing");
        Ok(())
    }

    #[test]
    fn join_needs_a_server_and_a_trust_choice() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let net = LoopbackNetwork::new();
        let mut client = engine(&net)?;
        let err = reg
            .call(&mut client, "net.join", json!({ "addr": "x:1" }))
            .expect_err("no trust");
        assert!(err.to_string().contains("insecure"), "{err}");
        let err = reg
            .call(
                &mut client,
                "net.join",
                json!({ "addr": "x:1", "insecure": true }),
            )
            .expect_err("nobody listens");
        assert!(format!("{err:#}").contains("nothing listens"), "{err:#}");
        assert_eq!(
            call(&reg, &mut client, "net.status", json!({})),
            json!({ "role": "none" })
        );
        assert!(!client.play_state().is_playing());
        Ok(())
    }
}
