# Security

This platform runs untrusted WebAssembly game modules inside a wasmtime
sandbox. The full threat model — what the sandbox guarantees, what it does not,
the tested attacks, and developer responsibilities — lives in
[`docs/SECURITY.md`](docs/SECURITY.md).

## Summary

- **Guests are sandboxed.** They have no access to the host except the
  explicitly registered `env` host functions (`docs/HOST_FUNCTIONS.md`).
- **Memory is isolated.** All guest memory access is bounds-checked; host
  functions bounds-check every `(ptr, len)` argument before reading or writing.
- **Imports are gated.** Unknown imports are rejected at instantiation.
- **CPU is metered.** wasmtime fuel interrupts infinite loops.
- **State is per-instance.** No state leaks between modules or instances.

## Tested attacks

Run with `cargo run -p host --bin security_test`:

1. Out-of-bounds memory write — blocked (trap).
2. Undefined host import — rejected at instantiation.
3. Infinite loop — interrupted by fuel exhaustion.
4. Host-state integrity after traps — verified uncorrupted.
5. Out-of-bounds receive buffer — returns `-1`, memory untouched.

## Out of scope (before untrusted networks)

No side-channel/timing protection, no cap on memory growth/instance count, no
authentication or encryption of P2P datagrams, no anti-cheat, and host
functions are trusted code. See `docs/SECURITY.md` for details and the
deployment checklist.
