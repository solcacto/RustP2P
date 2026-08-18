# RustP2P — Sandboxed WebAssembly P2P Game Platform

A decentralized, peer-to-peer multiplayer game platform. Untrusted game logic
runs inside a **WebAssembly sandbox** (wasmtime); the host provides rendering,
input, networking, and the score HUD — and the guest's only view of the world is
a small, explicitly registered set of host functions.

The current deliverable is **Chase/Tag**: two players control 3D avatars in
separate windows over a direct P2P link, tag each other by getting close, and
watch the score update live.

---

## Architecture

```
┌──────────────────────────────┐         ┌──────────────────────────────┐
│ Host A  (play_game --role A) │         │ Host B  (play_game --role B) │
│                              │         │                              │
│  ┌────────────────────────┐  │         │  ┌────────────────────────┐  │
│  │ wasmtime sandbox       │  │         │  │ wasmtime sandbox       │  │
│  │   guest.wasm (game)    │  │         │  │   guest.wasm (game)    │  │
│  │   game_tick()          │  │         │  │   game_tick()          │  │
│  └───────────┬────────────┘  │         │  └───────────┬────────────┘  │
│              │ env host fns  │         │              │ env host fns  │
│  ┌───────────▼────────────┐  │         │  ┌───────────▼────────────┐  │
│  │ HostState (per-instance)│ │         │  │ HostState (per-instance)│ │
│  └───────────┬────────────┘  │         │  └───────────┬────────────┘  │
│  ┌───────────▼────────────┐  │         │  ┌───────────▼────────────┐  │
│  │ PeerConnection (UDP)   │◄─┼─────────┼──│ PeerConnection (UDP)   │  │
│  └────────────────────────┘  │         │  └────────────────────────┘  │
│                              │         │                              │
│  Bevy renderer: local avatar │         │  Bevy renderer: local avatar │
│  + remote avatar + score HUD │         │  + remote avatar + score HUD │
└──────────────┬───────────────┘         └──────────────┬───────────────┘
               │  P2P UDP datagrams                      │
               │  (16-byte poses, 4-byte scores)         │
               └─────────────────────────────────────────┘
               ▲  WebSocket signaling (discovery only —
               │  never relays game traffic)
       ┌───────┴────────┐
       │ signaling_server│  ws://127.0.0.1:9001
       └────────────────┘
```

Key properties:

- **Guest isolation** — game code is a `wasm32-unknown-unknown` module with no
  access to the host except the registered `env` host functions
  ([`docs/HOST_FUNCTIONS.md`](docs/HOST_FUNCTIONS.md)). Memory access is
  bounds-checked; CPU is metered with wasmtime fuel
  ([`docs/SECURITY.md`](docs/SECURITY.md)).
- **Direct P2P** — the signaling server only exchanges UDP addresses. Once
  peers know each other's addresses, poses and scores flow straight over UDP.
- **One guest tick per rendered frame** — the Bevy app runs `game_tick()` as an
  `Update` system, so the sandboxed game logic drives the scene graph at 60fps.

---

## Repository layout

```
.
├── platform/
│   ├── guest/                # The game — a Rust crate compiled to Wasm
│   │   └── src/lib.rs        #   Chase/Tag logic (no_std, core only)
│   ├── host/                 # The runtime
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── host_functions.rs   # All sandbox escape hatches (env module)
│   │       ├── host_state.rs       # Per-instance host state
│   │       ├── peer_connection.rs  # UDP + WebSocket signaling
│   │       ├── renderer.rs         # Bevy scene, game loop, keyboard, HUD
│   │       ├── avatar_state.rs / input_state.rs / input_poller.rs
│   │       ├── assets/avatar.glb   # The avatar model
│   │       ├── src/bin/            # play_game + framework tests
│   │       └── src/test_modules/   # WAT test guests (security/network/render)
│   └── signaling_server/      # WebSocket discovery server
├── docs/
│   ├── HOST_FUNCTIONS.md      # Host function reference
│   └── SECURITY.md            # Full threat model
├── SECURITY.md                # Short summary + pointer to docs/SECURITY.md
└── Cargo.toml                 # Workspace root
```

---

## Prerequisites

- Rust (stable, 1.70+) and Cargo
- The `wasm32-unknown-unknown` target:

```sh
rustup target add wasm32-unknown-unknown
```

## Build

```sh
# 1. Compile the guest game to WebAssembly
cargo build -p guest --target wasm32-unknown-unknown --release

# 2. Build the host and signaling server
cargo build --workspace
```

The host loads the guest from
`target/wasm32-unknown-unknown/release/guest.wasm`. If you forget step 1,
`play_game` prints an error telling you to run it.

## Run the game

Open three terminals.

**Terminal 1 — the signaling server** (address discovery only):

```sh
cargo run -p signaling_server
```

**Terminal 2 — player A:**

```sh
cargo run -p host --bin play_game -- --role A
```

**Terminal 3 — player B:**

```sh
cargo run -p host --bin play_game -- --role B
```

### Controls

| Key                 | Action                        |
|---------------------|-------------------------------|
| `W` / `S` / `A` / `D` | Move forward / back / left / right |
| `Space`             | Rotate right                  |
| `Shift` (left)      | Rotate left                   |

Each window shows your avatar, a blue-tinted clone of the remote player's
avatar, and the HUD `Local Score: X | Remote Score: Y`. A tag registers when
the two avatars come within **2.0 units** of each other.

### Headless / automated verification

Both players can be driven by scripted input for CI-style verification (role A
auto-drives toward role B and tags):

```sh
cargo run -p host --bin play_game -- --role A --auto --frames 240
cargo run -p host --bin play_game -- --role B --auto --frames 240
```

Additional flags: `--frames N` (auto-exit after N frames, default 900) and
`--no-exit` (run until the window is closed).

---

## Test suite

Each framework test is a binary under `platform/host/src/bin/`; the networking
tests need the signaling server running first.

```sh
cargo run -p host                                   # host-state smoke test
cargo run -p host --bin security_test               # sandbox attack suite
cargo run -p host --bin game_loop                   # frame loop
cargo run -p host --bin input_test                  # input getters
cargo run -p host --bin network_wasm_test           # wasm<->wasm messages
cargo run -p host --bin render_test                 # 3D scene + wasm-driven pose
cargo run -p host --bin multiplayer_test -- --role A   # needs 2 terminals
cargo run -p host --bin multiplayer_test -- --role B

# networking tests require the signaling server:
cargo run -p signaling_server &   # in another terminal
cargo run -p host --bin network_test
```

Lint and docs:

```sh
cargo clippy --workspace --all-targets
cargo doc --workspace --no-deps
```

---

## Documentation

- [`docs/HOST_FUNCTIONS.md`](docs/HOST_FUNCTIONS.md) — every host function:
  signature, security guarantees, and usage.
- [`docs/SECURITY.md`](docs/SECURITY.md) — full threat model: what the sandbox
  prevents, what it doesn't, and the developer's responsibilities.
- API docs: `cargo doc --workspace --no-deps` (or browse `target/doc`).
