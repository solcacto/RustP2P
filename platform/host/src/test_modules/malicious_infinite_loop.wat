(module
  (func (export "attempt_infinite_loop")
    (loop $loop
      br $loop
    )
  )
)