# Host Function Reference

The Wasm guest's **entire view of the outside world**. All functions live in the
`env` import module of a wasmtime `Linker` and are registered by
`host::host_functions::register` (`platform/host/src/host_functions.rs`).

Nothing else is reachable from inside the sandbox — no filesystem, sockets,
processes, or memory other than the guest's own linear memory.

---

## Calling conventions

- **Module:** every import is `env::<name>`.
- **Pointers:** pointer arguments are `i32` byte offsets into the guest's
  exported linear memory (`memory`). Because `wasm32` pointers are 32-bit, the
  Rust guest declares them as `*const u8` / `*mut T`, which map to `i32`.
- **Strings:** passed as `(ptr, len)`. The host reads exactly `len` bytes at
  `ptr` and treats them as UTF-8 (lossy).
- **Integers:** `u32`/`i32` map directly; `u64`/`i64` map to wasm `i64`.
- **Security:** every pointer range is checked against the memory's current
  size before any read or write. An out-of-range argument returns `0` (or
  `-1` for a receive buffer) rather than trapping or corrupting host memory.

---

## Frame & clock

### `env::update() -> f64`

Advances the frame clock and returns the delta time since the previous call, in
seconds. Called automatically once per rendered frame by the host before the
guest tick; games that manage their own loop may call it instead.

### `env::get_frame_count() -> u64`

Returns the number of frames rendered so far. The canonical timebase for game
logic (e.g. the guest drives its animation from it).

---

## Demo counter

### `env::increment_counter()`

Increments a per-instance demo counter. Exists for framework smoke tests;
games can ignore it.

---

## Input

Each getter returns the current frame's input snapshot (see
`host::input_state::InputState`). Values are `0` (not held) or `1` (held).

| Function | Returns |
|----------|---------|
| `get_input_move_up() -> u32` | 1 if "move up" held |
| `get_input_move_down() -> u32` | 1 if "move down" held |
| `get_input_move_left() -> u32` | 1 if "move left" held |
| `get_input_move_right() -> u32` | 1 if "move right" held |
| `get_input_action_1() -> u32` | 1 if primary action held |
| `get_input_action_2() -> u32` | 1 if secondary action held |
| `get_input_gamepad_connected() -> u32` | 1 if a gamepad is connected |
| `get_input_gamepad_axis_x() -> f32` | stick X in `[-1.0, 1.0]` |
| `get_input_gamepad_axis_y() -> f32` | stick Y in `[-1.0, 1.0]` |

*Guarantee:* pure reads of per-instance state; no guest state is mutated.

---

## Networking

### `env::send_network_message(peer_id_ptr: i32, peer_id_len: i32, msg_ptr: i32, msg_len: i32) -> i32`

Sends `msg_len` bytes at `msg_ptr` as one UDP datagram to the peer whose id is
at `peer_id_ptr`/`peer_id_len`.

- Returns `1` on success, `0` if the peer is unknown, the memory range is out of
  bounds, or the send fails.
- *Security:* peer-id and message ranges are bounds-checked before reading.

### `env::receive_network_message(buffer_ptr: i32, buffer_len: i32) -> i32`

Pops the next inbound datagram (FIFO) and copies it into the guest's buffer.

- Returns the number of bytes written, `0` if the queue is empty, or `-1` if
  `buffer_ptr`/`buffer_len` is invalid or the message does not fit.
- *Security:* the write is bounds-checked against the guest memory; a message
  that does not fit is rejected rather than truncated into host memory.

---

## Avatar & pose sync

Poses are four `f32`s — `x`, `y`, `z`, `rot_y` — packed as **16 bytes of
little-endian data** on the wire. The guest should mirror the host's
`#[repr(C)] AvatarPose` layout:

```rust
#[repr(C)]
pub struct AvatarPose { pub x: f32, pub y: f32, pub z: f32, pub rot_y: f32 }
```

### `env::update_avatar_transform(x: f32, y: f32, z: f32, rot_y: f32)`

Sets the local avatar's target pose. The Bevy scene applies it to the local
avatar's `Transform` before the frame is drawn.

### `env::broadcast_avatar_pose(x: f32, y: f32, z: f32, rot_y: f32)`

Broadcasts the local pose as a 16-byte datagram to **every** known peer.
Non-finite values are dropped at the host. The receiver validates that all four
values are finite before accepting the pose.

### `env::get_remote_avatar_pose(peer_id_ptr: i32, peer_id_len: i32, out_pose_ptr: i32) -> i32`

Writes the latest known pose of the peer `peer_id` as 16 bytes into the guest
buffer at `out_pose_ptr`.

- Returns `1` if the peer's pose is known (and `out_pose_ptr..+16` is in
  bounds), `0` otherwise.
- Only **remote** peers appear in the pose map, so probing your own peer id
  always returns `0` — a simple way to discover the remote peer when ids are
  known in advance.

---

## Session / game mechanics

### `env::get_movement_axis() -> u32`

Returns this instance's role axis: `0` = X (role A), `1` = Z (role B). Set by
the host from the `--role` flag; lets a single guest binary specialize its
spawn point and movement direction per role.

### `env::set_tag_score(score: u32)`

Reports the guest's local tag score to the host, which surfaces it in the score
HUD (`Local Score: X | Remote Score: Y`). Called by the guest whenever its
score changes.

### `env::broadcast_tag_score(score: u32)`

Broadcasts the local score to every known peer as a **4-byte little-endian
datagram**. The receiving host records it per-peer and shows it as the remote
score.

---

## Wire formats (datagrams)

| Type | Length | Layout |
|------|--------|--------|
| Pose | 16 B   | `f32 x`, `f32 y`, `f32 z`, `f32 rot_y`, all little-endian |
| Score | 4 B   | `u32` little-endian |

The host's receive path dispatches purely on length: 16-byte datagrams are
validated and stored as remote poses; 4-byte datagrams are stored as remote
scores; anything else is dropped.
