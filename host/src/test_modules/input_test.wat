(module
  (import "env" "get_input_move_up" (func $move_up (result i32)))
  (import "env" "get_input_move_down" (func $move_down (result i32)))
  (import "env" "get_input_move_left" (func $move_left (result i32)))
  (import "env" "get_input_move_right" (func $move_right (result i32)))
  (import "env" "get_input_action_1" (func $action_1 (result i32)))
  (import "env" "get_input_action_2" (func $action_2 (result i32)))

  (func (export "process_input") (result i32)
    call $move_right
    call $move_left
    i32.sub

    call $move_down
    call $move_up
    i32.sub
    i32.const 10
    i32.mul
    i32.add
  )
)