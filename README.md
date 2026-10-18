# REDj

AI DJ console in Rust: a native DJ mixer with an AI "director" connected
through MCP, selectable among the models installed in Ollama.

- **Live**: play in real time; the AI plans transitions ahead of time and the
  engine executes them sample-accurately. Human control always wins.
- **Offline**: pick a folder of `.wav`/`.mp3` files; REDj analyzes them, the
  AI builds a running order and mix plan, and the engine renders the mix.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Build

```sh
cargo build --workspace --release
cargo test --workspace
```

Linux needs `pkg-config`, `libasound2-dev` and `libjack-jackd2-dev`.
Windows needs the Visual Studio C++ Build Tools (also used to build LAME for
MP3 output; disable with `--no-default-features` on `redj-render`).

## Command line

```sh
# License-free demo tracks with known tempo, key and structure
redj-cli synth demo/ --count 4

# Analysis: tempo, beat grid, key, loudness, sections (cached by content hash)
redj-cli analyze demo/

# Mix plans: JSON Schema, validation, rendering
redj-cli schema > plan.schema.json
redj-cli validate plan.json --music demo/
redj-cli render plan.json --music demo/ --out mix.wav --mp3
```

A plan lists tracks by id (or a unique id prefix) with transitions placed in
bars of the outgoing track:

```json
{
  "title": "Demo set",
  "entries": [
    { "track": "9007842b" },
    { "track": "883af1bd",
      "transition": { "at_bar": 112, "length_bars": 16, "style": "eq_swap" } },
    { "track": "c97271fc",
      "loops": [ { "at_bar": 0, "length_bars": 8, "repeats": 1 } ],
      "transition": { "at_bar": 104, "length_bars": 16, "style": "filter" } }
  ]
}
```

The render writes a 24-bit WAV (limited to -1 dBTP), optionally a 320 kbps
MP3, a `.cue` tracklist and a copy of the plan.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
