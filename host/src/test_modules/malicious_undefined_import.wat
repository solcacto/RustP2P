(module
  (import "env" "delete_file_system" (func $bad))
  (func (export "attempt_undefined_import")
    call $bad
  )
)