# Security Model

This document describes the security boundary of the Wasm sandbox in the RustP2P
game platform, the attacks that have been tested against it, and what the
architecture does — and does not — guarantee.

## Architecture

Guest Wasm modules run inside the wasmtime WebAssembly runtime. The Wasm
sandbox has **zero direct access** to system resources. The only way a Wasm
guest can interact with the outside world is through host functions explicitly
registered on the wasmtime `Linker` (see `host/src/host_functions.rs`). Host
state is owned per-instance by the `Store` (`host/src/host_state.rs`), so no
state can leak between modules or instances.

## Tested Attacks

All tests live in `host/src/bin/security_test.rs` with the malicious modules in
`host/src/test_modules/`.

### 1. Out-of-bounds memory access

- **Module:** `malicious_out_of_bounds.wat`
- **Attack:** allocates a 1-page (64KB) linear memory, then writes to offset
  1,000,000 — far beyond the bounds.
- **Defense:** wasmtime enforces bounds on every linear-memory access. The
  access is checked at runtime and traps before any host memory can be touched.
- **Result:** `✓ Out-of-bounds access blocked`; host process continues normally.

### 2. Undefined host import

- **Module:** `malicious_undefined_import.wat`
- **Attack:** imports `env::delete_file_system` — a host function that does not
  exist — and tries to call it.
- **Defense:** the `Linker` resolves every import at instantiation time. An
  unknown import is rejected *before* the module can execute.
- **Result:** `✓ Undefined import blocked`; instantiation fails cleanly.

### 3. Infinite loop (CPU exhaustion)

- **Module:** `malicious_infinite_loop.wat`
- **Attack:** an unconditional loop that never terminates.
- **Defense:** wasmtime fuel metering (`Config::consume_fuel` +
  `Store::set_fuel`). Each executed instruction consumes fuel; when fuel runs
  out the runtime traps, interrupting the loop.
- **Result:** `✓ Infinite loop blocked (fuel exhausted)`; the host is not hung.

## What the security boundary guarantees

- All linear-memory accesses are bounds-checked; guests cannot read or write
  host memory.
- Guests can only reach the outside world through explicitly registered,
  type-safe host functions (no raw pointers, no `unsafe`).
- Unknown or unregistered imports are rejected at instantiation.
- Non-terminating or CPU-hungry code can be interrupted via fuel metering.
- Host state is isolated per instance (`Store`-owned), and remains uncorrupted
  even when a guest traps.
- A trapped or failed module never crashes the host; each failure is handled
  and the process exits cleanly.

## What it does NOT guarantee

- **Side-channel attacks:** no protection against cache-timing, power, or other
  microarchitectural side channels.
- **Timing attacks:** the host does not currently guard against a guest leaking
  information through fine-grained timing behavior.
- **All resource exhaustion:** fuel bounds CPU, but a guest can still grow its
  own linear memory up to Wasm limits; memory exhaustion must be managed at the
  host level (memory limits, instance caps).
- **Faulty host functions:** a bug in a registered host function runs in the
  host's privilege context; host functions must be reviewed as trusted code.
- **DDoS by instantiation:** the host does not currently rate-limit module
  instantiation; many instances can still consume host resources.