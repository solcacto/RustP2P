(module
  (import "env" "get_frame_count" (func $get_frame_count (result i64)))
  (import "env" "update_avatar_transform" (func $update_transform (param f32 f32 f32 f32)))

  ;; Walks the avatar back and forth along the X axis using a triangle wave,
  ;; plus a slow rotation, based purely on the frame count.
  ;;
  ;;   t    = frame_count % 200
  ;;   x    = |t - 100| * 0.025 - 1.5   (oscillates between -1.5 and +1.5)
  ;;   y    = 0
  ;;   z    = 0
  ;;   rot_y = t * 0.02
  (func (export "render_tick")
    (local $t f64)

    call $get_frame_count
    i64.const 200
    i64.rem_u
    f64.convert_i64_s
    local.set $t

    ;; x
    local.get $t
    f64.const 100.0
    f64.sub
    f64.abs
    f64.const 0.025
    f64.mul
    f64.const 1.5
    f64.sub
    f32.demote_f64

    ;; y
    f32.const 0.0

    ;; z
    f32.const 0.0

    ;; rot_y
    local.get $t
    f64.const 0.02
    f64.mul
    f32.demote_f64

    call $update_transform
  )
)