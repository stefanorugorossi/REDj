# REDj architecture

REDj is a desktop DJ console written in Rust with an AI "director" attached
through MCP. It runs **live** (a human and/or the AI plays in real time) or
**offline** (the AI builds a set from local files and the engine renders it).

## Core principle: AI on the control plane only

The AI never touches the audio path. It produces a **plan**: structured,
validated data expressed in **bars/beats**. The engine evaluates that plan as
**automation**: every parameter is a function of musical position, like DAW
automation, not a list of one-shot triggers. So:

- tempo changes do not break the plan (positions are musical, not seconds);
- loops, hot cues and jumps always yield the correct state at any position;
  jumps are rendered with a short (5–10 ms) crossfade between read heads;
- a slow model (seconds to tens of seconds per answer, CPU-only) is fine,
  because plans are made well ahead of time.

## Workspace

```
crates/
  redj-core      units and shared types (beats, bars, ids, BeatGrid, TrackInfo,
                 Key, Library)
  redj-dsp       real-time safe DSP blocks (isolator EQ, filter, echo,
                 crossfader curves, true-peak limiter, sinc interpolation,
                 WSOLA time-stretch)
  redj-media     decoding (symphonia) and sample-rate conversion (rubato)
  redj-plan      mix plan model, JSON Schema, validation with AI-readable
                 errors, compilation into an automation timeline
  redj-analysis  tempo / beat grid / downbeats / sections / energy / LUFS / key,
                 tag metadata, JSON cache keyed by content hash
  redj-engine    track audio, channel strips, plan player (per-sample
                 automation), master section; driven by the offline renderer
                 and the real-time (cpal) driver
  redj-render    offline render: engine + offline driver, encoders, cue, tags
  redj-control   session core: parameter ownership, pending plans, autopilot
  redj-mcp       MCP server (rmcp), stdio + Streamable HTTP
  redj-ai        AI host: Ollama client, embedded MCP client, tool loop, mock LLM
  redj-midi      controller I/O, mapping profiles, MIDI learn, LED feedback
  redj-ui        egui/eframe interface
  redj-testkit   synthetic reference tracks with known ground truth (tests)
apps/
  redj           GUI application
  redj-cli       headless: analyze, validate, render, synth, `mcp --stdio`
```

Dependency direction (no cycles): `core` ← `dsp`, `plan`, `midi`;
`media` (standalone); `analysis` ← `core` + `media`; `engine` ← `dsp` +
`plan`; `render` ← `engine` + `media`; `control` ← `engine` + `plan` +
`analysis`; `mcp` ← `control`; `ai` ← `mcp`; `ui` ← `control`.

`TrackInfo` (beat grid, sections, energy, loudness, key) lives in `redj-core`
so that `plan` can validate against tracks without depending on the analyzer.

## Time model

- Each track has a **beat grid**: constant tempo (BPM + first-beat offset +
  downbeat phase) as the primary case, since the target material is 4/4 EDM;
  a list of beat anchors covers variable-tempo tracks.
- The set has a **master timeline in beats** with a master-tempo curve.
- Each deck has a **playhead map**: piecewise segments mapping master beats to
  track beats. Loops, hot cues and jumps are segments of this map.
- Automation lanes are piecewise curves (linear, exponential, S-curve, step)
  over master-beat ranges, per parameter.
- The scheduler converts beat positions to sample positions for the current
  block and **splits blocks exactly at event boundaries**; parameters are
  smoothed per sample to avoid zipper noise.

## Mix plan

The plan (`redj-plan`, JSON Schema via `redj-cli schema`) is deliberately
high level so that a language model can write it:

- `entries`: tracks in playing order, each with `start_bar`, optional
  `loops` (`at_bar`, `length_bars`, `repeats`) and, for every entry after the
  first, a `transition`: `at_bar` (bar of the **outgoing** track), 
  `length_bars`, `style` (`blend`, `bass_swap`, `eq_swap`, `filter`,
  `echo_out`, `cut`), `tempo` (`glide`, `incoming`, `outgoing`) and optional
  custom `automation` lanes that replace the style's lanes.
- Global: `tempo` (`follow` the tracks or `fixed`), `keylock`, `loudness`
  (auto gain to a target LUFS per track).
- Bars always refer to the track's own grid (bar 0 = first downbeat).

`compile` validates and compiles in one pass, so checks and behaviour cannot
disagree. Every issue has a JSON pointer, a message and, where possible, a
concrete fix (`use at_bar <= 112`). The output is a `Timeline`: tempo map,
clips with playhead maps and automation lanes, markers for the tracklist.

## Engine and real-time rules

- The mix graph processes fixed-size blocks and is independent of the audio
  device. A **real-time driver** (cpal) and an **offline driver** run the very
  same graph: this is what makes the offline render identical to live DSP.
- Inside the audio callback: no allocation, no locks, no I/O.
  - Commands arrive on a lock-free SPSC queue (`rtrb`).
  - Status snapshots go out through a latest-value slot written with
    `Mutex::try_lock`, which never waits: if the UI is reading, the audio
    thread skips that update and keeps accumulating meter peaks.
  - New tracks/plans are handed over as prebuilt `Arc` snapshots; replaced
    objects are sent back on a return queue and dropped off the audio thread.
  - Tests run `process` under `assert_no_alloc` to prove it never allocates.
- Track audio is stored in fixed-size chunks published through `OnceLock`,
  so a deck can play while chunks are still arriving (unfilled chunks read
  as silence). The current loader decodes and resamples a whole track
  (about half a second in release builds) before publishing it.
- Stereo throughout for now (decks, master, headphones); stems and
  multichannel buses are a later extension.
- Master and headphone (cue) outputs are routed to any channel pairs of one
  device (e.g. a 4-channel controller interface). Two separate devices need
  adaptive resampling for clock drift and are deferred.
- Time-stretch with keylock goes through a `TimeStretcher` trait; scratching
  uses variable-rate resampling without keylock.

## Control arbitration (live)

- Every automatable parameter has an **owner**: `Plan` or `Human`. Any UI or
  MIDI action on a parameter takes ownership until the user hands it back.
- AI actions **apply automatically**; the user can reject them explicitly from
  the timeline. Rejected or overridden actions are logged with their reason.
- A **frozen horizon** (configurable, in bars) protects the near future from
  plan edits so changes never land mid-gesture.
- The AI must deliver the next transition at least **N bars** before it starts
  (N configurable). Otherwise the **autopilot** applies a safe default
  (configurable: default blend at the outgoing track's outro, or loop the
  current track).
- **AI off** is a first-class mode: the console works as a plain DJ console.
- Every AI action carries a short textual rationale, shown in the UI.

## AI integration

- `redj-mcp` is the single AI-facing interface. `redj-ai` talks to it
  in-process through the MCP client, so Ollama and external clients (Claude
  Code/Desktop) see exactly the same tools.
- Streamable HTTP attaches external clients to the **running GUI** (live);
  stdio starts a **headless** instance (offline sets).
- Tools: read (library, track analysis, deck state, position, active plan)
  and act (load track, propose/update plan, schedule transition, cancel
  action). Parameters are typed with explicit units and ranges.
- Ollama: `/api/tags` lists models, `/api/show` `capabilities` tells which
  support `tools` and `thinking`; `think` is toggled per request (off for
  routine calls, on for whole-set planning).
- Slow-model hygiene: a stable system prompt and tool list at the head of
  every request (KV-cache reuse), compact JSON summaries, explicit `num_ctx`
  (Ollama's default context is only 4096 tokens), `keep_alive` to avoid
  reloads, configurable timeouts and retries.
- Default model: `qwen3.6:35b-a3b-q4_K_M`. Measured on the development
  machine (Core Ultra 7 265H, 32 GB, CPU only): ~35 s cold load, ~11 tokens/s
  generation.

## Analysis

Pure Rust, no ONNX: for 4/4 four-on-the-floor material classical DSP is
accurate enough and avoids a heavy native runtime to maintain.

- Tempo: spectral-flux onset envelope (22.05 kHz, 5.8 ms hop); coarse tempo
  by autocorrelation with a mild club-tempo prior; octave errors resolved on
  the kick-band envelope (the kick hits every beat); the constant period is
  then fitted over the whole track by maximizing a phase-invariant harmonic
  score (no histogram binning, so no bias), and snapped to integer/half BPM
  when that fits as well.
- Beat phase: the fundamental's phase gives the beat; a time-domain pass
  averages the low-passed envelope around every beat and puts the grid line
  at the start of the kick transient (sub-millisecond on the reference
  tracks).
- Variable tempo: if the kick pulse drifts off the constant grid in more
  than a quarter of 32-beat windows, a dynamic-programming beat tracker
  (Ellis 2007) produces a grid with anchors.
- Downbeats: spectral novelty (band energies plus bass spectrum) between
  consecutive beats and consecutive bars, plus onset accents (crashes),
  summed per beat phase.
- Sections (intro, build-up, drop, breakdown, outro, body): per-bar features,
  checkerboard novelty, boundaries on the phrase grid (multiples of 4 bars,
  at least 8 apart), labelled from kick presence, energy relative to the
  loudest kick section, energy slope and position.
- Loudness and true peak: `ebur128`. Key: chroma + key profiles (expected
  accuracy is modest; it is documented as such).
- Tags: `lofty`. Cache: one JSON file per track, keyed by the content hash
  (`blake3`) plus the analyzer version.

## Offline render

- Same graph as live, fixed block size, single thread: same plan ⇒ same file
  (bit-exact on the same OS; golden hashes are per-OS, with a tolerance
  comparison across platforms because floating-point libraries differ).
- Output: 24-bit PCM WAV at the prevailing source sample rate (default
  44.1 kHz), high-quality resampling only where needed; optional MP3
  320 kbps CBR.
- Master limiter with oversampled true-peak detection, ceiling −1 dBTP.
  Optional loudness normalization (two passes), off by default.
- Also exports the final plan, a `.cue` tracklist, and tracklist tags.

## UI

egui/eframe. Immediate mode suits meters and scrolling waveforms that update
every frame. Waveforms are three-band summaries (256-frame bins computed at
load time) drawn with the egui painter, with the beat grid, cue, hot cues,
loops and AI regions on top; this is fast enough, so the planned custom
wgpu paint callback was not needed. iced (Elm-style retained state) and
Slint (weak free-form drawing) fit this workload less well. UI language:
English only. `redj --demo <music folder>` loads two tracks and plays one
for a quick look.

## MIDI controllers

- Profiles are JSON data (`crates/redj-midi/profiles`): input bindings
  (status, data1, optional LSB for 14-bit controls, inversion, absolute or
  relative encoding) to abstract `Action`s, and output bindings from console
  state to LEDs. The mapper only sees `MidiMessage`s, so a virtual controller
  drives the same path in tests on every OS.
- 14-bit controls emit a value on the LSB only (the hardware sends MSB then
  LSB; emitting both would make the value jitter).
- MIDI learn binds the next moved control to an action; learned bindings
  override the profile.
- LED feedback sends only changed values.
- DDJ-FLX4: message numbers come from the Pioneer message list as encoded in
  the Mixxx mapping (GPL-2.0-or-later); the profile is tested against the
  virtual controller only, not against hardware.

## Licensing and dependencies

REDj is **GPL-3.0-or-later**. Pure Rust is preferred; a native dependency is
accepted only where no mature Rust alternative exists, and must be
GPL-compatible (e.g. LAME for MP3 encoding, a C++ time-stretch library).

## Testing

- Synthetic reference tracks generated in tests (known BPM, downbeats and
  sections): deterministic and license-free. Real tracks can be used locally
  but are never committed.
- Unit tests for analysis, plan validation, scheduler sample accuracy.
- Golden render hashes.
- Mock LLM for `redj-ai` and fallback paths without Ollama.
- Virtual MIDI controller: the mapping layer consumes an abstract message
  source, so tests inject messages on every OS (midir has no virtual ports
  on Windows).
- Audio callback benchmarks and xrun checks under load.

## Milestones

0. Toolchain, workspace skeleton, CI, this document.
1. Analysis + offline render from a hand-written plan (no AI).
2. MCP server + AI host with Ollama: fully AI-driven offline mix.
3. Live 2-deck engine with UI, headphone cue, DDJ-FLX4 MIDI profile; no AI.
4. Live AI director: ahead-of-time planning, fallback, overrides.
5. Polish, 4 decks, more MIDI profiles, packaging for the three platforms.
