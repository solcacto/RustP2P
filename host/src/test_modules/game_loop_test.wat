(module
  (import "env" "update" (func $update (result f64)))
  (import "env" "get_frame_count" (func $get_frame_count (result i64)))

  (memory (export "memory") 1)

  (func (export "game_tick") (result i64)
    i32.const 0
    call $update
    f64.store
    call $get_frame_count
  )
)