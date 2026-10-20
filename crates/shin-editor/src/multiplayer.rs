//! The Multiplayer panel and the status bar's connection state (GE-046).
//!
//! The editor is a solo server while it plays (like the game run alone,
//! with [`shin::net::NetServer::solo`]), so the project's
//! `scripts/server.rhai` greets the person at the editor as player 1. With
//! **Host on Play** checked, Play runs `net.host` instead: the editor is a
//! listen server on the port shown, and a game started with `cargo run --
//! --join` sees the editor's world. **Join** runs `net.join`: the editor
//! becomes a client of another server. Stop ends either session
//! (`sim.stop` ends it), and the editor is solo again.
//!
//! Everything goes through the tools, so the panel shows what `net.status`
//! and `net.players` report: the players with their round-trip times and
//! a sparkline of the bytes sent and received per tick.

use egui::{Color32, Sense, Stroke, Ui, pos2};
use serde_json::{Value, json};
use shin::headless::Headless;
use shin::net::DEFAULT_PORT;
use shin::tool::{ToolOutput, ToolRegistry};

use crate::ui::Action;

/// The panel's fields, kept between frames.
#[derive(Debug)]
pub struct Multiplayer {
    /// Whether Play hosts a session (`net.host`) instead of playing solo.
    pub host_on_play: bool,
    /// The UDP port Play hosts on.
    pub port: u16,
    /// The `host:port` Join connects to.
    pub join_addr: String,
    /// Whether Join trusts any server certificate (development only);
    /// otherwise it trusts the one a host in this project saved.
    pub trust_any: bool,
}

impl Default for Multiplayer {
    fn default() -> Self {
        Self {
            host_on_play: false,
            port: DEFAULT_PORT,
            join_addr: format!("127.0.0.1:{DEFAULT_PORT}"),
            trust_any: false,
        }
    }
}

impl Multiplayer {
    /// The `net.host` arguments Play uses when hosting.
    pub fn host_args(&self) -> Value {
        json!({ "port": self.port })
    }

    /// The `net.join` arguments for `addr`.
    pub fn join_args(&self, addr: &str) -> Value {
        if self.trust_any {
            json!({ "addr": addr, "insecure": true })
        } else {
            json!({ "addr": addr })
        }
    }
}

/// What `net.status` and `net.players` reported this frame.
#[derive(Debug, Clone)]
pub struct NetView {
    /// The `net.status` result.
    pub status: Value,
    /// The `net.players` result's `players` array.
    pub players: Vec<Value>,
}

impl Default for NetView {
    fn default() -> Self {
        Self {
            status: json!({ "role": "none" }),
            players: Vec::new(),
        }
    }
}

impl NetView {
    /// Calls `net.status` and `net.players` on `engine`. A failing call
    /// (which these never do) reads as no session.
    pub fn read(reg: &ToolRegistry, engine: &mut Headless) -> Self {
        let mut call = |tool: &str| match reg.call(engine, tool, json!({})) {
            Ok(ToolOutput::Json(value)) => Some(value),
            _ => None,
        };
        let status = call("net.status").unwrap_or_else(|| json!({ "role": "none" }));
        let players = call("net.players")
            .and_then(|v| v["players"].as_array().cloned())
            .unwrap_or_default();
        Self { status, players }
    }

    /// `"none"`, `"host"`, or `"client"`.
    pub fn role(&self) -> &str {
        self.status["role"].as_str().unwrap_or("none")
    }
}

/// The status bar's description of the connection state.
pub fn status_text(status: &Value) -> String {
    let rtt = |player: &Value| {
        player["rtt_ms"]
            .as_f64()
            .map_or(String::new(), |ms| format!(", {ms:.0} ms"))
    };
    match status["role"].as_str() {
        Some("host") => {
            let joined = status["players"].as_array().map_or(0, Vec::len);
            format!(
                "Hosting on port {} · {joined} joined · tick {}",
                status["port"], status["tick"]
            )
        }
        Some("client") => {
            let addr = status["addr"].as_str().unwrap_or("?");
            match status["state"].as_str() {
                Some("connected") => format!(
                    "Connected to {addr} as player {}{}",
                    status["player"],
                    rtt(&status["players"][0])
                ),
                Some("disconnected") => format!(
                    "Disconnected from {addr}: {}",
                    status["reason"].as_str().unwrap_or("closed")
                ),
                _ => format!("Joining {addr}…"),
            }
        }
        _ => "Offline".to_string(),
    }
}

/// Draws the Multiplayer panel. `playing` disables the session controls,
/// which start a session (and play mode) themselves; Stop ends it.
pub fn panel_ui(
    ui: &mut Ui,
    mp: &mut Multiplayer,
    view: &NetView,
    playing: bool,
    actions: &mut Vec<Action>,
) {
    let idle = view.role() == "none" && !playing;
    ui.add_enabled_ui(idle, |ui| {
        ui.horizontal(|ui| {
            ui.checkbox(&mut mp.host_on_play, "Host on Play")
                .on_hover_text("Play hosts a session others can join (net.host)");
            ui.label("port");
            ui.add(egui::DragValue::new(&mut mp.port));
        });
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut mp.join_addr)
                    .hint_text("host:port")
                    .desired_width(140.0),
            );
            let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let addr = mp.join_addr.trim().to_string();
            if (ui.button("Join").clicked() || submitted) && !addr.is_empty() {
                actions.push(Action::Join(addr));
            }
        });
        ui.checkbox(&mut mp.trust_any, "Trust any server (development only)");
    });
    ui.separator();
    ui.label(status_text(&view.status));
    if view.role() == "none" {
        return;
    }
    egui::Grid::new("net_players").striped(true).show(ui, |ui| {
        ui.strong("Player");
        ui.strong("RTT");
        ui.end_row();
        for player in &view.players {
            let you = if player["local"] == true {
                " (you)"
            } else {
                ""
            };
            ui.label(format!("{}{you}", player["id"]));
            ui.label(
                player["rtt_ms"]
                    .as_f64()
                    .map_or("–".to_string(), |ms| format!("{ms:.0} ms")),
            );
            ui.end_row();
        }
    });
    let rate = |key: &str| view.status[key].as_f64().unwrap_or(0.0);
    ui.label(format!(
        "{:.0} B/s in · {:.0} B/s out",
        rate("bytes_in_per_s"),
        rate("bytes_out_per_s")
    ));
    let bandwidth: Vec<f64> = view.status["bandwidth"]
        .as_array()
        .map(|values| values.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    sparkline(ui, &bandwidth);
}

/// A line through `values` (bytes per tick, oldest first), scaled to its
/// largest value, filling the panel's width.
fn sparkline(ui: &mut Ui, values: &[f64]) {
    let size = egui::vec2(ui.available_width().max(60.0), 32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
    let max = values.iter().copied().fold(0.0, f64::max);
    if values.len() < 2 || max <= 0.0 {
        return;
    }
    let step = rect.width() / (values.len() - 1) as f32;
    let points: Vec<egui::Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let y = rect.bottom() - 2.0 - (v / max) as f32 * (rect.height() - 4.0);
            pos2(rect.left() + i as f32 * step, y)
        })
        .collect();
    painter.line(points, Stroke::new(1.5, Color32::from_rgb(90, 170, 255)));
    response.on_hover_text(format!("Bytes per tick, last {} ticks", values.len()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_text_names_the_connection_state() {
        assert_eq!(status_text(&json!({ "role": "none" })), "Offline");
        assert_eq!(
            status_text(&json!({ "role": "host", "port": 7777, "tick": 9, "players": [{}] })),
            "Hosting on port 7777 · 1 joined · tick 9"
        );
        let client = |state: &str| {
            json!({
                "role": "client", "addr": "h:1", "state": state, "player": 2,
                "players": [{ "id": 2, "rtt_ms": 12.4 }], "reason": "bye",
            })
        };
        assert_eq!(status_text(&client("connecting")), "Joining h:1…");
        assert_eq!(
            status_text(&client("connected")),
            "Connected to h:1 as player 2, 12 ms"
        );
        assert_eq!(
            status_text(&client("disconnected")),
            "Disconnected from h:1: bye"
        );
    }

    #[test]
    fn join_args_trust_the_project_cert_unless_told_otherwise() {
        let mut mp = Multiplayer::default();
        assert_eq!(mp.join_args("a:1"), json!({ "addr": "a:1" }));
        mp.trust_any = true;
        assert_eq!(
            mp.join_args("a:1"),
            json!({ "addr": "a:1", "insecure": true })
        );
        assert_eq!(mp.host_args(), json!({ "port": DEFAULT_PORT }));
    }
}
