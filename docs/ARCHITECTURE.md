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
  redj-core      units and shared types (Beat, Bar, SamplePos, ids, TrackInfo)
  redj-dsp       real-time safe DSP blocks (EQ, filters, crossfader curves,
                 true-peak limiter, time-stretch, resampling)
  redj-plan      mix plan model, JSON Schema, validation with AI-readable errors
  redj-analysis  tempo / beat grid / downbeats / sections / energy / LUFS / key,
                 tag metadata, JSON cache keyed by content hash
  redj-engine    block-based mix graph, decks, automation evaluation,
                 sample-accurate scheduler; offline and real-time (cpal) drivers
  redj-render    offline render: engine + offline driver, encoders, cue, tags
  redj-control   session core: parameter ownership, pending plans, autopilot
  redj-mcp       MCP server (rmcp), stdio + Streamable HTTP
  redj-ai        AI host: Ollama client, embedded MCP client, tool loop, mock LLM
  redj-midi      controller I/O, mapping profiles, MIDI learn, LED feedback
  redj-ui        egui/eframe interface
apps/
  redj           GUI application
  redj-cli       headless: analyze, render, `mcp --stdio`
```

Dependency direction (no cycles): `core` ← `dsp`, `plan`, `analysis`, `midi`;
`engine` ← `dsp` + `plan`; `render` ← `engine` + `analysis`; `control` ←
`engine` + `plan`; `mcp` ← `control`; `ai` ← `mcp`; `ui` ← `control`.

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

## Engine and real-time rules

- The mix graph processes fixed-size blocks and is independent of the audio
  device. A **real-time driver** (cpal) and an **offline driver** run the very
  same graph: this is what makes the offline render identical to live DSP.
- Inside the audio callback: no allocation, no locks, no I/O.
  - Commands arrive on lock-free SPSC queues (`rtrb`).
  - New plans/timelines are handed over as prebuilt snapshots; replaced
    objects are sent back on a return queue and dropped off the audio thread.
  - Debug builds wrap the callback in `assert_no_alloc`.
- Tracks are decoded (symphonia) fully into memory by a background thread and
  are playable while decoding progresses.
- Buses have a **variable channel count** so stems and multichannel output
  remain possible later.
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

- Tempo/beats: onset-strength envelope, tempo by autocorrelation, beat
  tracking by dynamic programming, then fit to a constant grid.
- Downbeats: kick/bass emphasis plus phrase structure (8/16/32 bars).
- Sections (intro, breakdown, build-up, drop, outro): energy and novelty
  curves aligned to phrase boundaries.
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
every frame; waveforms use a custom wgpu paint callback. iced (Elm-style
retained state) and Slint (weak free-form drawing) fit this workload less
well. UI language: English only.

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
