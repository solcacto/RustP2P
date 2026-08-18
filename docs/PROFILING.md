# Profiling Infrastructure

The platform measures performance at every layer so optimizations are guided by
data, not guesswork. There are three complementary tools: per-frame tracing
logs, a summary performance report, and a criterion benchmark suite.

## 1. Frame-timing logs (`play_game`)

The game loop is instrumented at every phase — input polling, network polling,
Wasm execution, and the rest of the frame (Bevy rendering + other systems) —
via the `host::profiling` module. Bevy's `LogPlugin` honors `RUST_LOG`:

```sh
RUST_LOG=debug cargo run -p host --bin play_game -- --role A   # per-frame breakdown
RUST_LOG=info  cargo run -p host --bin play_game -- --role A   # periodic summaries + Bevy diagnostics
RUST_LOG=warn  cargo run -p host --bin play_game -- --role A   # quiet
```

With `RUST_LOG=debug`, each frame logs something like:

```
DEBUG host::renderer: Frame timing frame=2 frame_ms=46.9 input_ms=0.002 network_ms=0.014 wasm_ms=0.109
```

Bevy's `FrameTimeDiagnosticsPlugin` / `LogDiagnosticsPlugin` additionally log FPS,
frame time, and entity count once a second. Critical host functions log a
`warn` when a single call exceeds 1 ms (e.g. lock contention).

## 2. Performance report (`perf_report`)

`perf_report` runs the game for N frames, collects all timing samples, measures
memory, and prints a summary:

```sh
cargo run -p host --bin perf_report -- --frames 1000
```

The report shows the frame-time breakdown (input / network / Wasm / Bevy),
percentiles, loopback RTT, process RSS delta, wasm module size, and a ranked
bottleneck list. Note that `perf_report` runs standalone (no peer), so live
per-peer RTT comes from `play_game` (logged by the `measure_rtt` system every
60 frames).

## 3. Benchmark suite (`cargo bench`)

Criterion benchmarks live in `platform/host/benches/` and write HTML reports to
`target/criterion/`:

```sh
cargo bench -p host
```

| Benchmark | What it measures |
|-----------|------------------|
| `wasm_game_tick` | time to run one sandboxed `game_tick` (313 ns baseline) |
| `wasm_instantiation` | time to load + instantiate the guest module (492 µs baseline) |
| `udp_round_trip` | UDP send/receive round trip between two peers |
| `udp_send_recv_128b` | one-way send + receive of a 128-byte datagram |

These catch performance regressions as the platform evolves.

## Reading the numbers

- **Frame time** is measured from the end of one frame's update chain to the
  start of the next, so it includes rendering — the real per-frame cost.
- **Breakdown percentages** tell you where the frame time goes. If Bevy
  rendering dominates (>95%), optimization effort belongs in the renderer; if
  Wasm execution grows, look at guest-side work or host-function cost.
- **Warm-up**: wasmtime compiles the module on first use and the first frames
  load assets — ignore the first ~10 frames when comparing runs (the report
  percentiles naturally down-weight outliers, but prefer `p95`/`p99` over the
  mean for spike analysis).
- **RTT** is measured with ping/pong datagrams (`PeerConnection::ping_all`),
  reported as `PeerStats::avg_rtt_ms`.
