(module
  (import "env" "save_chunk_state" (func $save (param f32 f32 i32 i32 f32) (result i32)))
  (import "env" "publish_chunk_states" (func $publish (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "ruin")

  (func (export "save_state") (result i32)
    f32.const 150.0
    f32.const 30.0
    i32.const 0
    i32.const 4
    f32.const 3.0
    call $save)

  (func (export "publish_state") (result i32)
    call $publish)
)