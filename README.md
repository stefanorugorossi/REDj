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
cargo build --workspace
cargo test --workspace
```

Linux needs `pkg-config`, `libasound2-dev` and `libjack-jackd2-dev`.
Windows needs the Visual Studio C++ Build Tools.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
