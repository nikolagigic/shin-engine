# Audio

Shin plays sound files: load an `.ogg` or `.wav` file, play it, and
optionally place it in the world so it fades with the distance from the
camera. It is deliberately small. There is no music streaming, no mixing
buses or effects, and no MP3.

## Sound files

A sound is named by an [asset source](scenes.md#asset-sources) ending in
`.ogg` (Ogg Vorbis) or `.wav` (PCM WAV), relative to the project
directory, for example `assets/sounds/blip.wav`. Mono and stereo files are
supported. Each file is decoded once, the first time it is used, and kept
in memory. Sounds are meant for short effects: a 3-minute song is decoded
into memory in full.

A new project ships one sound, `assets/sounds/blip.wav`: a 0.2 second
sine blip.

## Playing a sound

There are four ways to make a noise:

- **From a script**, `play_sound("assets/sounds/blip.wav")` plays it once,
  everywhere; `play_sound_at(entity, "assets/sounds/blip.wav")` plays it at
  the entity. See [Scripting](scripting.md#functions).
- **With the `audio.play` tool**, from the Console or an agent:
  `audio.play {"source": "assets/sounds/blip.wav"}`. Add `"volume": 0.5`
  for half volume, or `"entity": <id>` to play it at an entity.
- **With an audio source** on an entity (below) that has `autoplay` set:
  it starts when play starts.
- **In the editor**, the Inspector's **▶ Play** button previews the
  selected entity's sound.

`audio.stop_all` stops every sound. Stop in the editor (or `sim.stop`)
stops every sound too.

## Audio sources

An entity can carry one sound, its **audio source**, with these settings:

| Setting    | Default | Meaning |
|------------|---------|---------|
| `source`   |         | The `.ogg` or `.wav` file. |
| `volume`   | `1.0`   | Linear volume: `1.0` plays the file as recorded, `0.5` at half amplitude, `0.0` is silent. |
| `looping`  | `false` | Repeat until stopped. |
| `spatial`  | `false` | Fade with the distance from the camera (below). |
| `autoplay` | `false` | Start when play starts. |

Set it in any of three ways:

- **In the editor**, select the entity and type the path into the
  Inspector's **Audio** field; the Volume, Loop, Spatial, and Autoplay
  controls appear once it has a sound. Clearing the field removes it.
- **With the `entity.set_audio` tool**:
  `entity.set_audio {"entity": 4294967296, "source": "assets/sounds/blip.wav", "spatial": true}`.
  Omitted settings keep their values, and `"source": null` removes the
  sound. It is undoable like any other edit.
- **In a scene file**, with the entity's `audio` field (see
  [Scenes](scenes.md#audio)):

```ron
audio: (source: "assets/sounds/hum.ogg", volume: 0.5, looping: true, spatial: true, autoplay: true),
```

An audio source does nothing on its own unless it autoplays. Scripts and
tools play other sounds whenever they like.

## Spatial sounds

A sound played at an entity (`play_sound_at`, `audio.play` with
`entity`, or a spatial audio source) is positioned. The camera is the
listener, and the volume falls off with a simple inverse-distance law:
full volume within one metre, then `1 / distance`, so a sound 2 m away
plays at half volume and one 10 m away at a tenth. The sound follows its
entity while it plays, and its volume is updated every frame as the
camera and the entity move. There is no left/right panning and no HRTF.

## Without a sound device

The audio output is opened the first time a sound plays, not at
start-up. If the machine has no output device (a CI runner, a headless
server), Shin logs one warning and carries on **silently**: every play
succeeds and is counted, but nothing is heard. `audio.play` reports
`"silent": true` in that case, and `plays`, the number of sounds started,
either way, which is how tests check that a sound would have played.

## The starter project

In a new project, the moon carries `assets/sounds/blip.wav` as a spatial
audio source and runs `scripts/moon.rhai`, which plays the blip at the
moon each time it completes a revolution, every 2π seconds:

```rhai
{{#include ../../crates/shin-engine/src/project/template/moon.rhai}}
```

Open the project in the editor and press Play to hear it. Fly the camera
away from the moon and the blip gets quieter.

## Tools

| Tool               | What it does |
|--------------------|--------------|
| `audio.play`       | Plays a sound once, optionally at an entity. Returns the play count and whether the engine is silent. |
| `audio.stop_all`   | Stops every sound. |
| `entity.set_audio` | Sets or removes an entity's audio source. Undoable. |

See the [Tools reference](reference/tools.md).
