(module
  (memory (export "memory") 1)
  (func (export "attempt_out_of_bounds")
    i32.const 1000000
    i32.const 42
    i32.store
  )
)