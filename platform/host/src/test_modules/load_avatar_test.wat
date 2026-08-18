(module
  (import "env" "load_avatar" (func $load_avatar (param i32 i32) (result i32)))
  (memory (export "memory") 1)

  (data (i32.const 0) "avatars/blue.glb")
  (data (i32.const 64) "../secret.glb")
  (data (i32.const 128) "not_a_glb.txt")

  ;; All three paths are 13-16 bytes; loads must be bounds-checked by the host.

  (func (export "try_valid") (result i32)
    i32.const 0
    i32.const 16
    call $load_avatar)

  (func (export "try_traversal") (result i32)
    i32.const 64
    i32.const 13
    call $load_avatar)

  (func (export "try_non_glb") (result i32)
    i32.const 128
    i32.const 13
    call $load_avatar)
)