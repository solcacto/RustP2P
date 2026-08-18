//! RustP2P host: the sandboxed Wasm game runtime.
//!
//! This crate hosts untrusted WebAssembly game modules in the wasmtime sandbox
//! and binds them to a Bevy 3D renderer, a P2P networking layer, and a real
//! keyboard. The guest's entire interface to the outside world is the set of
//! host functions registered by [`host_functions::register`]; everything else
//! is unreachable from inside the sandbox.
//!
//! # Architecture
//!
//! - [`host_functions`] — every host function a guest can call, with
//!   bounds-checked memory access.
//! - [`host_state`] — per-instance state owned by the `Store`.
//! - [`peer_connection`] — UDP + WebSocket signaling for direct P2P links.
//! - [`renderer`] — Bevy scene, per-frame game loop, keyboard input, score HUD.
//! - [`avatar_state`] / [`input_state`] — shared pose and input data types.
//! - [`input_poller`] — mock input used by the framework tests.
//!
//! # Running
//!
//! The playable game lives in `platform/host/src/bin/play_game.rs` (loads the
//! compiled `platform/guest` Wasm module) alongside the framework tests in the
//! same directory. See the repository `README.md` for build and run
//! instructions.

pub mod avatar_state;
pub mod host_functions;
pub mod host_state;
pub mod input_poller;
pub mod input_state;
pub mod peer_connection;
pub mod renderer;