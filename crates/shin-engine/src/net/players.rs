//! Players and their input (GE-044).
//!
//! Each client tick, [`super::net_client_system`] sends the client's
//! [`Actions`] to the server as an [`InputFrame`] (unreliable: a lost frame
//! is corrected by the next one, since every frame is complete). The
//! server keeps the newest frame of each player in the [`PlayerInputs`]
//! resource, which game systems and scripts (`player_input(id, "move_x")`)
//! read. A listen server's own player (and a solo game's, see
//! [`super::NetServer::solo`]) sends no frames: its input is copied from
//! the local [`Actions`] resource instead.
//!
//! On the client, [`LocalPlayer`] says which player it is once the server
//! has welcomed it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::PlayerId;
use super::protocol::InputFrame;
use crate::input::{ActionValue, Actions};

/// Resource on a client: the player this client is, from the server's
/// welcome. [`super::net_client_system`] inserts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalPlayer(pub PlayerId);

/// Resource on a server: every player's input actions as of this tick.
///
/// [`super::net_server_input_system`] updates it at the start of each
/// tick, before the game systems run: from the newest [`InputFrame`] each
/// remote player sent, and from the [`Actions`] resource for the local
/// player. An action is held (`pressed`, `axis`) until a newer frame says
/// otherwise, and `just_pressed` lasts exactly one tick. A press is not
/// lost when several frames arrive in one tick, or when the frame carrying
/// it is lost but a later one shows the action held.
///
/// Unknown players and actions read as released, with axis `0`.
#[derive(Clone, Debug, Default)]
pub struct PlayerInputs {
    by_player: HashMap<PlayerId, PlayerInput>,
}

/// One player's actions and the newest frame applied.
#[derive(Clone, Debug, Default)]
struct PlayerInput {
    /// Actions not at rest, by name; any other action is at rest.
    values: BTreeMap<String, ActionValue>,
    /// The client tick of the newest frame applied; older frames that
    /// arrive late are dropped.
    last_tick: Option<u64>,
}

impl PlayerInputs {
    /// No players.
    pub fn new() -> Self {
        Self::default()
    }

    /// `player`'s value of `action` this tick; at rest if either is
    /// unknown.
    pub fn value(&self, player: PlayerId, action: &str) -> ActionValue {
        self.by_player
            .get(&player)
            .and_then(|input| input.values.get(action))
            .copied()
            .unwrap_or_default()
    }

    /// Whether `player` holds `action`.
    pub fn pressed(&self, player: PlayerId, action: &str) -> bool {
        self.value(player, action).pressed
    }

    /// Whether `player` pressed `action` this tick.
    pub fn just_pressed(&self, player: PlayerId, action: &str) -> bool {
        self.value(player, action).just_pressed
    }

    /// `player`'s `action` in `-1..=1` (`0.0` or `1.0` for a button).
    pub fn axis(&self, player: PlayerId, action: &str) -> f32 {
        self.value(player, action).axis
    }

    /// Every player with input, in id order.
    pub fn players(&self) -> Vec<PlayerId> {
        let mut players: Vec<PlayerId> = self.by_player.keys().copied().collect();
        players.sort();
        players
    }

    /// Starts a server tick: forgets players not in `players`, ends last
    /// tick's presses, applies each player's newest frame among `frames`
    /// (received since the last tick, in any order), and copies `local`'s
    /// [`Actions`] to the local player.
    pub(crate) fn begin_tick(
        &mut self,
        players: &[PlayerId],
        frames: Vec<(PlayerId, InputFrame)>,
        local: Option<(PlayerId, &Actions)>,
    ) {
        self.by_player.retain(|player, _| players.contains(player));
        for &player in players {
            let input = self.by_player.entry(player).or_default();
            for value in input.values.values_mut() {
                value.just_pressed = false;
            }
            input.values.retain(|_, value| !at_rest(value));
        }

        let mut by_player: HashMap<PlayerId, Vec<InputFrame>> = HashMap::new();
        for (player, frame) in frames {
            by_player.entry(player).or_default().push(frame);
        }
        for (player, frames) in by_player {
            if let Some(input) = self.by_player.get_mut(&player) {
                input.apply(frames);
            }
        }

        if let Some((player, actions)) = local
            && let Some(input) = self.by_player.get_mut(&player)
        {
            input.values = actions
                .iter()
                .filter(|(_, _, value)| !at_rest(value))
                .map(|(name, _, value)| (name.to_string(), value))
                .collect();
        }
    }
}

impl PlayerInput {
    /// Applies the newest of `frames` that is newer than the last one
    /// applied. A press is kept if any of those frames carried one, or if
    /// the newest shows an action held that was released before.
    fn apply(&mut self, frames: Vec<InputFrame>) {
        let fresh: Vec<InputFrame> = frames
            .into_iter()
            .filter(|frame| self.last_tick.is_none_or(|last| frame.tick > last))
            .collect();
        let Some(newest) = fresh.iter().max_by_key(|frame| frame.tick) else {
            return;
        };
        let presses: BTreeSet<&str> = fresh
            .iter()
            .flat_map(|frame| &frame.actions)
            .filter(|(_, value)| value.just_pressed)
            .map(|(name, _)| name.as_str())
            .collect();
        let mut values: BTreeMap<String, ActionValue> = newest.actions.iter().cloned().collect();
        for (name, value) in &mut values {
            let was_held = self.values.get(name).is_some_and(|old| old.pressed);
            value.just_pressed |= presses.contains(name.as_str()) || (value.pressed && !was_held);
        }
        // Pressed and released again between two ticks: still a press.
        for name in presses {
            values
                .entry(name.to_string())
                .or_insert(ActionValue {
                    pressed: false,
                    just_pressed: true,
                    axis: 0.0,
                })
                .just_pressed = true;
        }
        self.last_tick = Some(newest.tick);
        self.values = values;
    }
}

/// Whether `value` is the resting value every unlisted action has.
fn at_rest(value: &ActionValue) -> bool {
    !value.pressed && !value.just_pressed && value.axis == 0.0
}

/// The [`InputFrame`] for client tick `tick`: every action of `actions`
/// not at rest, in name order.
pub(crate) fn input_frame(tick: u64, actions: Option<&Actions>) -> InputFrame {
    InputFrame {
        tick,
        actions: actions
            .into_iter()
            .flat_map(Actions::iter)
            .filter(|(_, _, value)| !at_rest(value))
            .map(|(name, _, value)| (name.to_string(), value))
            .collect(),
        ack: None,
    }
}

#[cfg(test)]
mod tests;
