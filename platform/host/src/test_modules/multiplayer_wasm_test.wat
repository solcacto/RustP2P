(module
  (import "env" "get_frame_count" (func $get_frame_count (result i64)))
  (import "env" "update_avatar_transform" (func $update_transform (param f32 f32 f32 f32)))
  (import "env" "broadcast_avatar_pose" (func $broadcast (param f32 f32 f32 f32)))
  (import "env" "get_remote_avatar_pose" (func $get_remote (param i32 i32 i32) (result i32)))
  (import "env" "get_movement_axis" (func $get_axis (result i32)))

  (memory (export "memory") 1)

  ;; Peer ids the guest can query. One of them is the local peer (never present
  ;; in the remote pose map) and the other is the remote peer to observe.
  (data (i32.const 0) "PeerA")
  (data (i32.const 16) "PeerB")

  ;; Scratch buffer where get_remote_avatar_pose writes a 16-byte pose.
  (data (i32.const 64) "\00\00\00\00\00\00\00\00\00\00\00\00\00\00\00\00")

  (global $remote_seen (mut i32) (i32.const 0))
  (global $remote_x_bits (mut i32) (i32.const 0))

  ;; One game tick: move the local avatar along the role's axis (role A on X,
  ;; role B on Z) with a triangle wave, broadcast the pose to the remote peer,
  ;; then read the remote peer's latest pose back through the host function.
  (func (export "multiplayer_tick")
    (local $t f64)
    (local $axis i32)
    (local $v f32)
    (local $x f32)
    (local $z f32)

    ;; t = frame_count % 200
    call $get_frame_count
    i64.const 200
    i64.rem_u
    f64.convert_i64_s
    local.set $t

    ;; v = |t - 100| * 0.025 - 1.5   (oscillates between -1.5 and +1.5)
    local.get $t
    f64.const 100.0
    f64.sub
    f64.abs
    f64.const 0.025
    f64.mul
    f64.const 1.5
    f64.sub
    f32.demote_f64
    local.set $v

    ;; axis = get_movement_axis()
    call $get_axis
    local.set $axis

    ;; x = axis == 0 ? v : 0.0
    local.get $axis
    i32.eqz
    if
      local.get $v
      local.set $x
    else
      f32.const 0.0
      local.set $x
    end

    ;; z = axis == 0 ? 0.0 : v
    local.get $axis
    i32.eqz
    if
      f32.const 0.0
      local.set $z
    else
      local.get $v
      local.set $z
    end

    ;; update_avatar_transform(x, 0.0, z, rot_y)
    local.get $x
    f32.const 0.0
    local.get $z
    local.get $t
    f64.const 0.02
    f64.mul
    f32.demote_f64
    call $update_transform

    ;; broadcast_avatar_pose(x, 0.0, z, rot_y)
    local.get $x
    f32.const 0.0
    local.get $z
    local.get $t
    f64.const 0.02
    f64.mul
    f32.demote_f64
    call $broadcast

    ;; Probe "PeerA" (memory 0); if found, record that a remote pose arrived.
    (block $probe_peer_a
      i32.const 0
      i32.const 5
      i32.const 64
      call $get_remote
      i32.eqz
      br_if $probe_peer_a
      i32.const 1
      global.set $remote_seen
      i32.const 64
      f32.load
      i32.reinterpret_f32
      global.set $remote_x_bits
    )

    ;; Probe "PeerB" (memory 16) the same way.
    (block $probe_peer_b
      i32.const 16
      i32.const 5
      i32.const 64
      call $get_remote
      i32.eqz
      br_if $probe_peer_b
      i32.const 1
      global.set $remote_seen
      i32.const 64
      f32.load
      i32.reinterpret_f32
      global.set $remote_x_bits
    )
  )

  (func (export "remote_pose_seen") (result i32)
    global.get $remote_seen)

  (func (export "remote_pose_x_bits") (result i32)
    global.get $remote_x_bits)
)