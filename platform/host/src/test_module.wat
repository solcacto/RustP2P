(module
  (import "env" "increment_counter" (func $increment_counter))
  (func (export "run_test")
    call $increment_counter
    call $increment_counter
    call $increment_counter
    call $increment_counter
    call $increment_counter
  )
)