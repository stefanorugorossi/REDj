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

- The AI plans one transition at a time while the current track plays. A
  scheduled transition is a compiled two-entry plan (playing track + next
  track) whose master beat is the outgoing deck's own playhead, so the
  outgoing deck keeps its tempo (`tempo: fixed` at the current BPM) and the
  plan simply follows it; the incoming playhead is locked to the plan and
  automation lanes drive both channel strips, sample by sample.
- Ownership per parameter: during a transition every automatable parameter
  of the two decks is owned by the plan until the human moves it; from then
  on that parameter follows the human for the rest of the transition.
  Transport actions (play, seek, jog, loops, sync, load) on the incoming
  deck, or on the outgoing deck once the transition runs, cancel it: the
  human has taken over. At the end the engine hands back: the outgoing deck
  stops with its fader down, the incoming deck keeps the final automation
  values and the current tempo on its own fader.
- AI actions **apply automatically**; the UI shows the next transition with
  its rationale and a **Reject** button. Rejections, overrides and
  completions are logged.
- A **frozen horizon** (default 4 bars) rejects transitions scheduled to
  start too soon, so changes never land mid-gesture.
- The AI must schedule the next transition a **deadline** (default 16 bars)
  before the playing track's outro. Otherwise the **autopilot** steps in:
  either a blend into the most compatible unplayed track at the outro
  (default) or a 32-beat loop of the outro while waiting.
- **AI off** is a first-class mode: the console works as a plain DJ console.

## AI integration

- `redj-mcp` is the single AI-facing interface. `redj-ai` talks to it
  in-process through the MCP client, so Ollama and external clients (Claude
  Code/Desktop) see exactly the same tools.
- Transports: stdio (`redj-cli mcp`, headless, offline sets) and Streamable
  HTTP on `127.0.0.1:<port>/mcp` from the GUI (Settings), which lets an
  external client join the live set.
- Offline tools: `list_tracks`, `get_track`, `compatibility`,
  `suggest_plan`, `validate_plan`, `submit_plan`, `get_plan`, `plan_schema`,
  `render_mix`. Live tools: `get_live_state` (decks, bars, tempo, played
  tracks, next transition, earliest bar, deadline, suggested start and
  length), `schedule_transition`, `cancel_transition`.
- Live requests are validated by the console on the UI thread; the MCP tool
  waits for the answer, so the model gets errors with concrete fixes.
- Ollama: `/api/tags` lists models, `/api/show` `capabilities` tells which
  support `tools` and `thinking`; `think` is sent explicitly (off by default:
  recent models think by default, which is slow on CPU).
- Slow-model hygiene: a stable system prompt and tool list at the head of
  every request (KV-cache reuse), only the tools a task needs, compact JSON
  summaries with derived facts (mix-in/out ranges, suggested transition),
  explicit `num_ctx` (Ollama's default is 4096 tokens), `keep_alive`,
  configurable timeouts and retries.
- Default model: `qwen3.6:35b-a3b-q4_K_M`. On the development machine (Core
  Ultra 7 265H, 32 GB, CPU only): an offline five-track set took five tool
  calls and about 3.5 minutes including a 103 s cold first turn; a live
  transition is planned in 25-70 s, far ahead of the deadline.

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

- Synthetic reference tracks generated in tests (known BPM, downbeats,
  sections, key): deterministic and license-free. Real tracks can be used
  locally but are never committed.
- Analysis accuracy on the reference tracks: exact BPM (including 174.5),
  grid within 1 ms of the true downbeat, every section boundary within one
  bar with the right label, key within one Camelot step.
- Plan validation and compilation, sample-accurate loops, block-size
  independence, beat lock through tempo glides.
- Determinism: two renders of the same plan are bit-identical (compared in
  the test rather than against stored golden hashes, which would differ
  between platforms).
- Live engine: transport, sync, loops, AI transitions with overrides, all
  run under `assert_no_alloc` to prove the callback never allocates.
- Mock LLM for `redj-ai`: offline set planning, live planning through MCP,
  autopilot and rejection; an ignored test runs the real Ollama model.
- Virtual MIDI controller: the mapping layer consumes an abstract message
  source, so tests inject messages on every OS (midir has no virtual ports
  on Windows); every built-in profile is checked for the essential controls.
- `cargo bench -p redj-engine --bench live`: on the development machine a
  512-frame block with four keylocked decks at different tempos takes
  0.39 ms on average (3.4% of the 11.6 ms budget), 2.0 ms worst case.

## MIDI profiles

Built in: Pioneer DDJ-FLX4 (hand-written from the message list), Pioneer
DDJ-400 and Hercules DJControl Inpulse 200/300 (generated with
`cargo run -p redj-midi --example mixxx2redj -- <mapping.midi.xml> <out.json>`
from the Mixxx mappings, GPL-2.0-or-later). The converter translates standard
Mixxx controls and recognizes script-bound jog, shift, play, cue, sync, load
and tempo controls by name; controllers whose mappings are mostly scripts
(e.g. Numark Mixtrack FX, DDJ-SB3) do not convert usefully. None of the
profiles has been tested on hardware; MIDI learn covers the gaps.

## Packaging

`cargo packager --release` in `apps/redj` (cargo-packager) builds the
installer for the current platform: NSIS `.exe` on Windows (verified),
`.app`/`.dmg` on macOS, `.deb`/AppImage on Linux. Both binaries (`redj`,
`redj-cli`) are included. `.github/workflows/release.yml` builds all three
on tagged releases once the repository has a GitHub remote.

## Milestones

0. Toolchain, workspace skeleton, CI, this document. Done.
1. Analysis + offline render from a hand-written plan (no AI). Done.
2. MCP server + AI host with Ollama: fully AI-driven offline mix. Done.
3. Live 2-deck engine with UI, headphone cue, DDJ-FLX4 MIDI profile; no AI.
   Done.
4. Live AI director: ahead-of-time planning, fallback, overrides. Done.
5. Polish, 4 decks, more MIDI profiles, packaging for the three platforms.
   Done (macOS and Linux packages built by CI only).

Known gaps: MIDI profiles untested on hardware; the loader publishes a track
after decoding it completely; the headphone output must share the master's
device; MP3 output needs a C compiler (LAME).
