(module
  (import "env" "send_network_message"
    (func $send_network_message (param i32 i32 i32 i32) (result i32)))
  (import "env" "receive_network_message"
    (func $receive_network_message (param i32 i32) (result i32)))

  (memory (export "memory") 1)

  ;; (peer_id_ptr, peer_id_len, msg_ptr, msg_len) -> received length
  ;; Sends the message, then tries to receive into a buffer at offset 200.
  (func (export "tick_network") (param i32 i32 i32 i32) (result i32)
    local.get 0
    local.get 1
    local.get 2
    local.get 3
    call $send_network_message
    drop

    i32.const 200
    i32.const 64
    call $receive_network_message
  )

  ;; (buffer_ptr, buffer_len) -> received length (0 none, -1 invalid buffer)
  (func (export "receive_at") (param i32 i32) (result i32)
    local.get 0
    local.get 1
    call $receive_network_message
  )
)