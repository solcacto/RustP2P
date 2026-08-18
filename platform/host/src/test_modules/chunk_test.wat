(module
  (import "env" "broadcast_chunk_claim" (func $claim (param i32 i32 i32 i32)))
  (import "env" "get_chunk_owner" (func $owner (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)

  (func (export "claim_test") (result i32)
    i32.const 0
    i32.const 0
    i32.const 2
    i32.const 1
    call $claim
    i32.const 0)

  (func (export "owner_test") (result i32)
    i32.const 3
    i32.const 0
    i32.const 64
    i32.const 32
    call $owner)
)