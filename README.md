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
│   ├── guest-sdk/             # Safe ergonomic SDK for game developers
│   │   ├── templates/         #   session-shooter, open-world templates
│   │   └── src/bin/create_game.rs  # `cargo new --template`-style scaffold tool
│   ├── guest-sdk-macros/      # Proc-macro crate (#[export(MyGame)])
│   ├── guest/                 # The Chase Tag reference game — Rust compiled to Wasm
│   │   ├── src/lib.rs         #   Chase/Tag logic (no_std, core only)
│   │   ├── guest.wasm         #   Committed, hash-pinned game artifact
│   │   └── game_manifest.json #   Game package manifest (pins the wasm hash)
│   ├── host/                  # The runtime
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── host_functions.rs   # All sandbox escape hatches (env module)
│   │       ├── host_state.rs       # Per-instance host state
│   │       ├── avatar_standard.rs  # Avatar-standard validator
│   │       ├── cosmetic.rs         # Signed cosmetic verification
│   │       ├── ipfs.rs             # IPFS publish/fetch (Kubo HTTP API)
│   │       ├── manifest.rs         # Game manifest validation + hash verify
│   │       ├── peer_connection.rs  # UDP + WebSocket signaling
│   │       ├── renderer.rs         # Bevy scene, game loop, keyboard, HUD
│   │       ├── avatar_state.rs / input_state.rs / input_poller.rs
│   │       ├── assets/avatar_standard.glb  # The standardized reference avatar
│   │       ├── assets/avatars/blue.glb     # A second validated avatar variant
│   │       ├── assets/cosmetics/           # Signed cosmetic packages
│   │       ├── src/bin/            # play_game + framework tests/tools
│   │       └── src/test_modules/   # WAT test guests (security/network/render)
│   └── signaling_server/      # WebSocket discovery server
│   └── registry_server/       # Lightweight game registry (only central component)
├── docs/
│   ├── AVATAR_STANDARD.md     # The Universal Avatar standard
│   ├── GAME_MANIFEST.md       # game_manifest.json format + validation
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

### Game packages & manifest integrity

The host loads a **game package**: a `game_manifest.json` plus the Wasm module
it describes (see [`docs/GAME_MANIFEST.md`](docs/GAME_MANIFEST.md)). Before any
Wasm runs, the host validates the manifest and refuses to load the module if
its SHA-256 doesn't match the pinned `wasm_hash`.

`platform/guest/` ships a committed, hash-pinned `guest.wasm` +
`game_manifest.json`, so `play_game` works out of the box. After editing the
guest, re-pin the artifact:

```sh
cargo build -p guest --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/guest.wasm platform/guest/guest.wasm
cargo run -p host --bin hash_wasm -- platform/guest/guest.wasm
# -> sha256:<hex>  (paste into game_manifest.json "wasm_hash")
```

If the hash ever mismatches, the host refuses to load the tampered artifact.

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

### Avatar customization

The avatar is loaded from a configurable path (relative to the host asset
folder) and validated against the avatar standard before the game starts, and
the guest can change it at runtime through `load_avatar`. Cosmetics are
**signed packages**: the host verifies the creator's ed25519 signature over the
manifest + mesh before rendering, and silently skips invalid ones.

```sh
# player A: default avatar, verified golden sword on the right hand
cargo run -p host --bin play_game -- --role A \
  --cosmetic cosmetics/golden_sword/cosmetic_manifest.json

# player B: a different, blue avatar with a signed hat on the head
cargo run -p host --bin play_game -- --role B \
  --avatar avatars/blue.glb \
  --cosmetic cosmetics/simple_hat/cosmetic_manifest.json
```

Author a cosmetic package (generates the mesh glb + signs it):

```sh
cargo run -p host --bin make_cosmetic -- \
  --kind sword --item my_sword_v1 --point RightHand \
  --out assets/cosmetics/my_sword
```

### Headless / automated verification

Both players can be driven by scripted input for CI-style verification (role A
auto-drives toward role B and tags):

```sh
cargo run -p host --bin play_game -- --role A --auto --frames 240
cargo run -p host --bin play_game -- --role B --auto --frames 240
```

Additional flags: `--frames N` (auto-exit after N frames, default 900) and
`--no-exit` (run until the window is closed).


## Guest SDK & game templates

Non-Rust developers don't touch wasm. The **guest SDK** (`platform/guest-sdk`)
wraps every raw host function in a safe, ergonomic Rust API:

```rust
use guest_sdk::prelude::*;

struct MyGame { x: f32, z: f32 }

impl Game for MyGame {
    fn new() -> Self { Self { x: 0.0, z: 0.0 } }
    fn tick(&mut self, ctx: &mut Context) {
        let input = ctx.input();
        if input.up { self.z -= 0.05; }
        ctx.set_avatar_transform(self.x, 0.0, self.z, 0.0);
        ctx.broadcast_pose(self.x, 0.0, self.z, 0.0);
    }
}

#[export(MyGame)]
fn game_tick() {}
```

`Context` exposes input, avatar pose, network, pose sync, scoring, and avatar
selection; the `#[export]` attribute turns a function into the wasm entry point
and the SDK dispatches every tick to a single persistent game instance.

**Scaffold a game from a template** (the `cargo new --template` equivalent):

```sh
cargo run -p guest-sdk --bin create_game -- \
  --template session-shooter --name my_shooter --author solcacto
# templates: session-shooter, open-world

# play it:
cargo run -p host --bin play_game -- --role A --package platform/games/my_shooter
```

`create_game` copies the template, compiles it to wasm, and writes a
`game_manifest.json` pinned to the built wasm's SHA-256 — a ready-to-play,
self-contained game package.

## Decentralized distribution (IPFS)

Games are distributed peer-to-peer, not from a server. A game is bundled into
a `.tar` (`guest.wasm` + `game_manifest.json` + assets) and pinned on a local
**Kubo/IPFS node**; its **CID is the game's permanent address**. Players fetch
the exact same bytes from the swarm by that CID.


a `.tar` (`guest.wasm` + `game_manifest.json` + assets) and pinned on a local
**Kubo/IPFS node**; its **CID is the game's permanent address**. Players fetch
the exact same bytes from the swarm by that CID.

Prerequisites: a running Kubo daemon on `127.0.0.1:5001`
(`brew install ipfs && ipfs init && ipfs daemon`).

**Publish the game:**

```sh
cargo run -p host --bin publish_game
# -> CID = QmZMLbVr5jVnUEMKviW1qnQNGXH93HsqAejBULUGGMtTpB
```

**Play a game by CID** (fetches the bundle from IPFS, extracts it, validates
the manifest + wasm hash, then runs):

```sh
cargo run -p host --bin play_game -- --role A --cid <CID>
cargo run -p host --bin play_game -- --role B --cid <CID>
```

Downloaded bundles are cached under `platform/host/games/<CID>/` and **pinned**
on the local node, so a player who downloads a game automatically becomes a
**seeder** for the swarm. The `distribution_test` verifies the full round-trip
(publish → fetch → extract → hash-verify → byte-identical).

### P2P game caching

Once any player downloads a game, the game is served by the swarm — the
original publisher can even go offline. `play_game`/`list_games` accept
`--ipfs <api>` to point at a specific node (one node per simulated machine).

```sh
# Machine A publishes (its node seeds the game):
cargo run -p host --bin publish_game
CID=$(ls platform/host/games/*.tar | sed 's/.*games\///;s/\.tar//')

# Machine B downloads the game from A and becomes a seeder:
cargo run -p host --bin cache_test -- $CID http://127.0.0.1:5002/api/v0

# (Machine A goes offline)

# Machine C downloads the same game from the swarm (served by B):
cargo run -p host --bin cache_test -- $CID http://127.0.0.1:5003/api/v0
```

Or just play through a seeder node: `play_game --cid $CID --ipfs <api>`.

## Game discovery (registry)

The **only centralized component** is a lightweight game registry: a
`games.json` list of available games (name, CID, description, author, mode). It
is trivially replaceable — anyone can host a mirror, serve a static
`games.json`, or pin the file on IPFS.

```sh
# 1. Start the registry (the default games.json lives in the crate):
cargo run -p registry_server

# 2. Publish and register a game:
cargo run -p host --bin publish_game -- --register --description "A simple tag game"

# 3. Browse available games:
cargo run -p host --bin list_games

# 4. Download a game by its registry entry (fetches from IPFS by CID):
cargo run -p host --bin list_games -- --download "Chase Tag"

# 5. Launch it:
cargo run -p host --bin play_game -- --role A --cid <CID>
```

The `discovery_test` verifies list → download → verify against a running
registry + IPFS node.


## Test suite

Each framework test is a binary under `platform/host/src/bin/`; the networking
tests need the signaling server running first. The guest SDK has its own unit
tests (`cargo test -p guest-sdk`).

```sh
cargo run -p host                                   # host-state smoke test
cargo run -p host --bin security_test               # sandbox attack suite
cargo run -p host --bin game_loop                   # frame loop
cargo run -p host --bin input_test                  # input getters
cargo run -p host --bin network_wasm_test           # wasm<->wasm messages
cargo run -p host --bin manifest_test               # manifest validation + hash gate
cargo run -p host --bin avatar_standard_test        # avatar-standard validation
cargo run -p host --bin avatar_customization_test    # load_avatar + avatar selection
cargo run -p host --bin cosmetic_signature_test      # signed cosmetic verification
cargo run -p host --bin distribution_test            # IPFS publish/fetch round-trip*
cargo run -p host --bin cache_test <CID> <api>       # fetch + pin from the swarm*
cargo run -p host --bin discovery_test               # registry list->download->verify†
cargo run -p host --bin trap_test                   # graceful handling of wasm traps
cargo run -p host --bin render_test                 # 3D scene + wasm-driven pose
cargo run -p host --bin multiplayer_test -- --role A   # needs 2 terminals
cargo run -p host --bin multiplayer_test -- --role B

# networking tests require the signaling server:
cargo run -p signaling_server &   # in another terminal
cargo run -p host --bin network_test
```

`*distribution_test` requires a Kubo/IPFS node running on `127.0.0.1:5001`.
`†discovery_test` additionally requires the registry server on `127.0.0.1:9002`.

## Lifecycle & error handling

The runtime handles failure without crashing:

- **Wasm traps** — a trapping guest is recorded, shown in the HUD, and no longer
  called; the app exits cleanly (code 0) after a short grace period
  (`trap_test` verifies this).
- **Peer disconnects** — when a remote stops sending poses for ~2s its avatar
  is despawned and the HUD shows `Peer: DISCONNECTED (avatar removed)`.
- **Signaling server outages** — the host probes the WebSocket link, reconnects
  with exponential backoff (0.5s → 30s) keeping the P2P UDP socket alive, and
  logs/HUDs the status (`signaling link lost` → `reconnected`).
- **Graceful shutdown** — on exit the signaling WebSocket sends a close frame
  and flushes before the process drops the app and exits 0.

Lint and docs:

```sh
cargo clippy --workspace --all-targets
cargo doc --workspace --no-deps
```

---

## Documentation

- [`docs/AVATAR_STANDARD.md`](docs/AVATAR_STANDARD.md) — the Universal Avatar
  standard (52-bone skeleton, attachment points, budgets, animations) and its
  host-side validator.
- [`docs/GAME_MANIFEST.md`](docs/GAME_MANIFEST.md) — the `game_manifest.json`
  format, validation rules, and authoring workflow.
- [`docs/HOST_FUNCTIONS.md`](docs/HOST_FUNCTIONS.md) — every host function:
  signature, security guarantees, and usage.
- [`docs/SECURITY.md`](docs/SECURITY.md) — full threat model: what the sandbox
  prevents, what it doesn't, and the developer's responsibilities.
- API docs: `cargo doc --workspace --no-deps` (or browse `target/doc`).
