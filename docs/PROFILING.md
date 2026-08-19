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
  load assets — summaries skip the first 60 frames automatically.
- **RTT** is measured with ping/pong datagrams (`PeerConnection::ping_all`),
  reported as `PeerStats::avg_rtt_ms`.

## Rendering optimization (Commit 27)

The Commit 26 baseline showed Bevy rendering at ~18.75 ms/frame. The profiling
data drove the Commit 27 optimizations:

- Avatars render as a **single merged low-poly mesh** (one entity, one draw
  call) instead of full glb scenes (61 entities each), with **shared
  materials** so all avatars batch.
- **Shadows** are enabled (1024 map) with only the local avatar casting.
- **Continuous updates + no vsync** (`WinitSettings::Continuous`,
  `PresentMode::Immediate`) so the loop isn't capped at 60 Hz.
- **Render diagnostics** (`RenderDiagnosticsPlugin`) show render passes are
  ~0.04 ms; a **frustum-culling / batching** debug system logs visible vs
  culled entities and unique material count.

Avatar **color variants were restored** after the optimization: the shared
material is now a palette keyed by avatar path (`avatar_standard.glb` = gray,
`avatars/blue.glb` = blue), so `--avatar`/`load_avatar` visibly change the
local avatar again while keeping batching (still 3 unique materials).

Result (Apple M1): average frame time **18.75 ms → 7.9 ms debug (124 FPS) /
3.5 ms release (284 FPS)** with shadows + palette restored. The remaining frame
time is macOS window-server pacing, not render work.

## Network optimization (Commit 29)

The network layer was optimized for latency, loss, and bandwidth:

- **Packet batching** — high-frequency traffic (poses, scores) is accumulated
  and flushed as **one UDP datagram per peer** per 16 ms window (or once a batch
  holds 10 messages), cutting packet count.
- **Delta compression** — avatar poses ship as **8-byte deltas** (4×i16, mm
  precision) instead of 16-byte full f32 poses (**50% bandwidth reduction**);
  full poses are sent on the first message and on teleports.
- **Reliable channel** — `send_reliable_message` delivers with ack + retry
  (100 ms, 3 retries) for chat/game events.
- **Enhanced `PeerStats`** — messages/packets, packet-loss %, bandwidth KiB/s,
  and average batch size, plus UDP socket tuning (`set_ttl`, buffers).

Measured loopback network performance (debug, Apple M1):

| Metric | Before | After |
|--------|--------|-------|
| RTT | 5.9 ms (measurement artifact) | **0.02 ms** |
| Packet loss | 0.2 % | **0.0 %** |
| Bandwidth | 12.4 KiB/s | **3.19 KiB/s** |
| Avg batch size | 1.0 | **6.0 messages/packet** |
| Pose bytes | 16 | **8 (50 % smaller)** |
