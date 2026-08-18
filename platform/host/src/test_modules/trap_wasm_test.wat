(module
  ;; Malicious/buggy guest that traps immediately on every tick. Used to prove
  ;; the host survives Wasm traps: the error is recorded and shown in the HUD,
  ;; the guest is no longer called, and the app exits gracefully.
  (func (export "game_tick")
    unreachable
  )
)