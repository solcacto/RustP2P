# Security Model

This document describes the security boundary of the WebAssembly sandbox in the
RustP2P game platform, what the architecture **guarantees**, what it explicitly
**does not** guarantee, the attacks that have been tested against it, and the
responsibilities each developer (guest, host, deployment) must uphold.

The short version lives in the repository-root `SECURITY.md`.

---

## Trust model

Three distinct trust zones:

1. **Host runtime** (`platform/host`) — fully trusted. Runs the wasmtime
   runtime, the Bevy renderer, and the P2P socket code. A vulnerability here is
   a full compromise of the process.
2. **Signaling server** (`platform/signaling_server`) — semi-trusted. It learns
   peer ids and UDP addresses and can observe who connects to whom, but it
   never sees or relays game traffic. A malicious signaling server can DoS
   discovery but cannot inject game state.
3. **Guests** (`platform/guest`) — untrusted. Arbitrary game code that arrives
   as a Wasm module and runs inside the sandbox. Guests may be buggy or
   actively malicious; they must never be able to touch the host's resources
   or other players' state except through the sanctioned host functions.

The security goal: a malicious guest must be able to do no more than move its
own avatar, emit poses/scores, and read poses/scores the host decides to show
it.

---

## What the sandbox guarantees

### Memory isolation

- Every guest gets its own linear memory (exported as `memory`). There is no
  shared address space.
- All linear-memory accesses are bounds-checked by wasmtime; an access past the
  end of the guest memory **traps**, it can never touch host memory.
- Host functions take **pointer + length** arguments and check the full range
  against the memory size *before* reading or writing. An out-of-range
  argument returns a failure code (`0` / `-1`) — it never reads or writes
  outside the guest's own memory.

### Import gating

- The only way out of the sandbox is the `env` host-function module registered
  by `host::host_functions::register`.
- Unknown imports are rejected at **instantiation** — a module that imports
  `env::delete_file_system` (or anything else unregistered) fails to load
  before a single instruction runs.
- All registered functions are type-checked by wasmtime against the guest's
  declared signatures.

### CPU / termination control

- wasmtime **fuel metering** charges every executed instruction against a fuel
  budget. When the budget runs out the runtime traps, so an infinite loop (or
  any unbounded computation) is interrupted instead of hanging the host.
- Fuel is set per-instance by the host, so one misbehaving guest cannot starve
  the process.

### State isolation & host robustness

- Host state lives in a per-instance `Store<HostState>`. No state is shared
  between instances or modules, so a guest cannot read or corrupt another
  guest's data.
- A trapped, panicked, or failed guest never crashes the host: traps are
  caught, errors propagate as `Result`s, and the test harness verifies host
  state is uncorrupted after each attack.

### Input validation on the wire

- Inbound pose datagrams are validated: all four `f32`s must be finite before a
  pose is accepted (`NaN`/`Inf` are rejected).
- Unknown or malformed datagrams (wrong length) are dropped.

### Content integrity

- Game packages are pinned by SHA-256 (`wasm_hash` in the manifest); a tampered
  wasm is refused before it runs.
- IPFS CIDs are content-addressed, so a fetched bundle is guaranteed to be
  byte-for-byte what was published at that CID; the wasm hash is still verified
  after download to guard against a malicious publisher shipping an inconsistent
  manifest+wasm pair.
- Cosmetic packages carry an ed25519 signature over
  `item_id | attachment_point | sha256(mesh)`; tampered cosmetics (manifest or
  mesh) fail verification and are **silently not rendered**.

---

## What the sandbox does NOT guarantee

These are **out of scope** for the current MVP. Before relying on the platform
for real adversarial multiplayer, these must be addressed (see
[Deployment responsibilities](#deployment-responsibilities)).

### Side channels & timing

- No protection against cache-timing, power, or other microarchitectural side
  channels.
- A guest can observe fine-grained timing (including `get_frame_count` deltas)
  and might infer host behavior from it.

### Resource exhaustion beyond CPU

- Fuel bounds CPU, but a guest can still **grow its own linear memory** up to
  wasm limits. The host does not yet cap memory growth or the number of
  instances, so memory exhaustion must be managed at the host level.
- UDP is connectionless and unauthenticated: a peer can flood the socket. There
  is currently no rate limiting, backpressure, or packet-size cap on inbound
  datagrams.

### Integrity / confidentiality of game traffic

- P2P datagrams are **plaintext and unauthenticated**. Any on-path actor can
  read, forge, reorder, or drop poses and scores.
- There is **no identity system**: peer ids are self-declared strings, so the
  platform cannot distinguish player A from an impostor claiming to be A.
- This is fine for LAN-style play, not for untrusted networks.

### Anti-cheat & game-logic trust

- The platform does **not** make the game cheat-proof. The guest is sandboxed,
  but a determined player runs their own modified guest and can send any
  poses/scores they like. Game rules that must be authoritative (who scored,
  who moved where) need host-side validation or a consensus/replication layer.
- Scores are broadcast as self-reported values.

### Faulty host functions

- A bug in a registered host function runs with the host's privileges. Host
  functions are **trusted code** and must be reviewed like any security-critical
  path. In particular: every new `(ptr, len)` argument must be bounds-checked.

### DoS via instantiation

- The host does not yet rate-limit module instantiation; many instances can
  still consume host resources.

---

## Tested attacks

The automated suite lives in `host/src/bin/security_test.rs` with the hostile
modules in `host/src/test_modules/`. Run it with `cargo run -p host --bin
security_test`.

| # | Attack | Module | Defense | Result |
|---|--------|--------|---------|--------|
| 1 | Out-of-bounds memory write (offset 1,000,000) | `malicious_out_of_bounds.wat` | wasmtime bounds checks on every access | trap; host continues |
| 2 | Call an unregistered import `env::delete_file_system` | `malicious_undefined_import.wat` | linker rejects unknown imports at instantiation | clean failure |
| 3 | Infinite loop (CPU exhaustion) | `malicious_infinite_loop.wat` | wasmtime fuel metering | trap on fuel exhaustion; host not hung |
| 4 | Host-state integrity after each attack | (in-process) | per-instance `Store` | counter/name uncorrupted after traps |
| 5 | Receive into an out-of-bounds guest buffer | (network receive path) | bounds-checked `receive_network_message` | returns `-1`, memory untouched |

---

## Developer responsibilities

### Guest developers

- The guest can only move its own avatar and emit poses/scores. Write game
  logic against the host functions in `docs/HOST_FUNCTIONS.md`; everything else
  (filesystem, network, rendering) is simply unavailable.
- Do **not** trust remote peers: their poses and scores are self-reported.
  Validate ranges and expect hostile input (e.g. `NaN`, absurd positions).
- Keep the guest small and `no_std`-friendly; it runs under a finite fuel
  budget.

### Host developers

- **Never add a host function without bounds-checking every pointer/length
  argument** — this is the most common way to escape the sandbox.
- Treat the guest as untrusted: validate finite floats, cap message sizes,
  budget fuel per instance, and put caps on memory growth and instance counts
  before untrusted networks are involved.
- Do not surface host-internal state through host functions without review.

### Deployment responsibilities (before untrusted networks)

1. **Authenticate and encrypt** game traffic (e.g. WireGuard-style session keys
   or a `libp2p`/QUIC transport) and bind peer ids to public keys.
2. **Validate the guest module** before loading it (hash allow-list or
   signatures — planned via IPFS/distribution layer) and set a fuel/memory
   budget per session.
3. **Enforce authoritative rules** for anything that must not be forgeable
   (scoring, physics) or accept that players can cheat.
4. **Rate-limit** instantiation and inbound datagrams; run the signaling server
   as a minimal, disposable service.
