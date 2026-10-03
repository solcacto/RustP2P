"use strict";
/* RustP2P browser host (single-player core).
 * Implements the guest `env` ABI (same 28 imports as the native host), runs
 * game_tick() on requestAnimationFrame, and renders draw_box calls on a 2D
 * canvas (top-down: world x -> right, world z -> down).
 * MULTIPLAYER SEAM: search for NET-STUB below. Broadcast/receive/remote
 * functions are stubbed today; the WebRTC phase fills them in without
 * changing any other line. */
const BrowserHost = (() => {
  const DT = 1 / 60;
  const keys = {};
  const GAME_KEYS = ["KeyW", "KeyA", "KeyS", "KeyD", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Space", "ShiftLeft", "ShiftRight"];
  window.addEventListener("keydown", (e) => {
    keys[e.code] = true;
    if (GAME_KEYS.indexOf(e.code) >= 0) { e.preventDefault(); }
  });
  window.addEventListener("keyup", (e) => { keys[e.code] = false; });

  function gamepad() {
    const pads = (navigator.getGamepads ? navigator.getGamepads() : []);
    for (const p of pads) { if (p && p.connected) { return p; } }
    return null;
  }

  function axisX(pad) {
    if (!pad || !pad.axes || pad.axes.length < 2) { return 0; }
    const v = pad.axes[0];
    return (typeof v === "number" && isFinite(v)) ? v : 0;
  }
  function axisY(pad) {
    if (!pad || !pad.axes || pad.axes.length < 2) { return 0; }
    const v = pad.axes[1];
    return (typeof v === "number" && isFinite(v)) ? v : 0;
  }

  // Per-frame guest state, reset by the renderer each frame like native.
  const state = { frame: 0, boxes: [], score: 0 };
  let memory = null;
  let hudEl = null;
  let errEl = null;

  function view() { return new DataView(memory.buffer); }
  function readString(ptr, len) {
    // Mirrors the native bounds checks: out-of-range reads fail safely.
    if (ptr < 0 || len < 0 || (ptr + len) > memory.buffer.byteLength) { return null; }
    const bytes = new Uint8Array(memory.buffer, ptr, len);
    return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  }

  // The full 28-import `env` module. Names and signatures must match the
  // native host exactly (see guest-sdk bridge.rs). i64 results use BigInt.
  const env = {
    get_frame_count: () => BigInt(state.frame),
    get_delta_seconds: () => DT,
    draw_box: (x, y, z, sx, sy, sz, r, g, b) => {
      if (![x, y, z, sx, sy, sz, r, g, b].every(isFinite)) { return; }
      state.boxes.push({ x, y, z, sx, sy, sz, r, g, b });
    },
    get_input_move_up: () => (keys["KeyW"] || keys["ArrowUp"] ? 1 : 0),
    get_input_move_down: () => (keys["KeyS"] || keys["ArrowDown"] ? 1 : 0),
    get_input_move_left: () => (keys["KeyA"] || keys["ArrowLeft"] ? 1 : 0),
    get_input_move_right: () => (keys["KeyD"] || keys["ArrowRight"] ? 1 : 0),
    get_input_action_1: () => (keys["Space"] ? 1 : 0),
    get_input_action_2: () => ((keys["ShiftLeft"] || keys["ShiftRight"]) ? 1 : 0),
    get_input_gamepad_connected: () => (gamepad() ? 1 : 0),
    get_input_gamepad_axis_x: () => axisX(gamepad()),
    get_input_gamepad_axis_y: () => axisY(gamepad()),
    // NET-STUB: single player has no peers; the WebRTC phase implements
    // these against DataChannels without touching the rest of this file.
    send_network_message: (peerPtr, peerLen, msgPtr, msgLen) => 0,
    receive_network_message: (bufPtr, bufLen) => 0,
    update_avatar_transform: (x, y, z, ry) => { state.avatar = { x, y, z, ry }; },
    broadcast_avatar_pose: (x, y, z, ry) => {},
    get_remote_avatar_pose: (peerPtr, peerLen, outPtr) => 0,
    get_movement_axis: () => 0,
    set_tag_score: (s) => {
      state.score = s >>> 0;
      if (hudEl) { hudEl.textContent = "Score: " + state.score; }
    },
    broadcast_tag_score: (s) => {},
    load_avatar: (pathPtr, pathLen) => 0,
    broadcast_chunk_claim: (ox, oz, ex, ez) => {},
    get_chunk_owner: (cx, cz, outPtr, outLen) => 0,
    send_reliable_message: (peerPtr, peerLen, msgPtr, msgLen) => 0,
    receive_reliable_message: (bufPtr, bufLen) => 0,
    save_chunk_state: (x, z, kindPtr, kindLen, value) => 0,
    publish_chunk_states: () => 0,
    load_remote_chunk_state: (cx, cz, outBuf, outLen) => 0,
  };

  function css(r, g, b) {
    const q = (v) => Math.max(0, Math.min(255, Math.round(v * 255)));
    return "rgb(" + q(r) + "," + q(g) + "," + q(b) + ")";
  }

  // Top-down view: world x -> right, world z -> down. Arena-scale games
  // fit by scaling to the smaller canvas dimension over ~18 world units.
  function render(canvas) {
    const ctx = canvas.getContext("2d");
    const S = Math.min(canvas.width, canvas.height) / 18;
    const cx = canvas.width / 2 - 8 * S;
    const cy = canvas.height / 2 - 8 * S;
    ctx.fillStyle = "#181822";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    for (const b of state.boxes) {
      ctx.fillStyle = css(b.r, b.g, b.b);
      ctx.fillRect(cx + b.x * S - (b.sx * S) / 2, cy + b.z * S - (b.sz * S) / 2, b.sx * S, b.sz * S);
    }
    state.boxes.length = 0;
  }

  async function play(id) {
    hudEl = document.getElementById("hud");
    errEl = document.getElementById("err");
    const titleEl = document.getElementById("title");
    const canvas = document.getElementById("game");
    const listRes = await fetch("games/games.json");
    if (!listRes.ok) { throw new Error("games/games.json not found"); }
    const list = await listRes.json();
    const game = (list.games || []).find((g) => g.id === id);
    if (!game) { throw new Error("unknown game: " + id); }
    if (titleEl) { titleEl.textContent = game.title || game.id; }
    const url = "games/" + game.dir + "/" + game.wasm;
    const res = await fetch(url);
    if (!res.ok) { throw new Error("game wasm not found: " + url); }
    const bytes = await res.arrayBuffer();
    let instance;
    try {
      const out = await WebAssembly.instantiate(bytes, { env });
      instance = out.instance;
    } catch (e) {
      throw new Error("wasm failed to start (missing imports?): " + e);
    }
    memory = instance.exports.memory;
    const tick = instance.exports.game_tick;
    if (typeof tick !== "function") { throw new Error("game has no game_tick export"); }
    let dead = false;
    const loop = () => {
      if (dead) { return; }
      try {
        state.frame += 1;
        tick();
        render(canvas);
      } catch (e) {
        // Graceful trap handling, mirroring the native host: stop the
        // guest, keep the page alive, show the error.
        dead = true;
        if (errEl) { errEl.textContent = "Game stopped: " + e; }
        return;
      }
      requestAnimationFrame(loop);
    };
    requestAnimationFrame(loop);
  }

  return { play };
})();
