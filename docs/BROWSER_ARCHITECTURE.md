# RustP2P Browser Host ï¿½ Architecture Report (Phase 1)

Branch: `feature/browser-host` (based on `experimental/seamless-session`).
Status: investigation only ï¿½ no implementation yet, no native code touched.

## 1. How the native platform fits together

Untrusted game code ships as `wasm32-unknown-unknown` modules exposing
`game_tick()`. The Bevy host (wasmtime 35) instantiates each game with an
`env` import module of 31 host functions (28 guest-reachable + 3 test-only),
then calls `game_tick()` once per rendered frame. The guest interface is
fully described by `platform/guest-sdk/src/bridge.rs` (raw imports) and
`docs/HOST_FUNCTIONS.md`: input getters, avatar pose sync, tag score,
unreliable + reliable messaging, `draw_box`, avatar loading, and chunk
claims/state. Nothing else is reachable from inside the sandbox.

Networking has two layers. `PeerConnection` is classic UDP plus a
tungstenite WebSocket to the signaling server (discovery only). `NetLink`
is the transport enum the game actually uses: `Udp(PeerConnection)` or
`Webrtc(WebRtcSession)`, chosen purely by signaling-URL shape. All game
traffic shares one framing (`platform/host/src/net.rs`): batched datagrams,
sequence/ack reliable overlay, ping/pong RTT, delta-compressed poses.
Chunk claims, state pointers, and zone handoffs ride the same datagrams.
Distribution is content-addressed: game `.tar` bundles on IPFS (Kubo API
or public gateways) listed in a tiny JSON game registry.

## 2. Platform-dependence matrix

| Component | Native-only | Reusable as-is | Browser needs new impl |
|---|---|---|
| Guest WASM modules (`guest.wasm`, SDK games) | | Byte-identical reuse | |
| `env` ABI: 28 imports, `(ptr,len)` strings, LE bytes, `0/-1/1` returns | | Contract reuse (bridge.rs + HOST_FUNCTIONS.md) | JS functions per import |
| Game manifest validation + hash/signature verify | | Logic reuse (pure) | WebCrypto + fetch instead of ureq/ed25519 crates |
| `.tar` bundle layout + extract | | Layout reuse | JS tar/gunzip |
| Registry `GET /games.json`, `POST /games` | | Protocol reuse | `fetch()` client |
| IPFS fetch | Kubo `add/cat/pin` | Gateway `GET /ipfs/<cid>` | `fetch()` (no local node needed) |
| `net::` framing (batch/reliable/ack/poses/scores) | | Byte-format reuse | JS codec (DataView) |
| Signaling handshake (offer/answer/ICE relay) | | Server unchanged | Browser `WebSocket` client |
| Lobby discovery (`list_peers`+game filter) | UDP path + Rust server | Worker needs the handler added | JS client + out-of-band IDs for demo |
| wasmtime (fuel, 32MiB cap, traps) | Entire runtime | Frame-budget concept only | rAF loop + time-slice guard, no fuel primitive exists |
| Bevy renderer, input, HUD | Entire renderer | Scene-graph ideas only | Three.js scene (already prototyped, see ï¿½3) |
| UDP sockets, tungstenite client, chunk DHT sockets | Transport + socket opts | Datagram logic above NetLink | WebRTC DataChannels only |
| Avatar GLB validation, cosmetics, ed25519 identity | Validator + fs sandbox | Budgets as policy | three.js GLTFLoader + ported checks |
| Chunk persistence/IPFS state, zone handoff | Logic (transport-agnostic) | Defer past demo | JS DHT + gateway fetch later |

## 3. Head start: the Studio preview already proves the ABI

`platform/studio/static/preview.html` is a working minimal browser host:
Three.js r160 scene, a JS `env` shim (input, `update_avatar_transform`,
`get_remote_avatar_pose` via DataView LE f32, HUD score), and
`WebAssembly.instantiate(bytes, { env })` running a real guest in a
sandboxed iframe. Gaps to close for multiplayer: `broadcast_*` are no-ops,
remote pose is faked drift, and `draw_box`, delta time, reliable messaging,
chunks, and avatar loading are missing. The browser host grows this file
into `platform/browser-host/` ï¿½ it is the correct foundation, not a toy.

## 4. Guest compatibility verdict

Existing game WASMs run **without modification** provided the JS host
implements their exact `env` imports (per-game subsets differ ï¿½ LTO strips
unused imports ï¿½ so offer all 28). No WASI, no threads, allocators live
inside guest linear memory. Three conversions are mandatory: `u64/i64` via
`BigInt`, bounds-checked `DataView` memory access with `0/-1` returns,
and lossy-UTF8 string reads. Fuel metering has no browser equivalent:
replace with a per-tick time budget plus worker termination on overrun.
Unknown imports must reject instantiation, mirroring the native linker.

## 5. Proposed browser architecture

```
platform/browser-host/          # static files only: html + js + wasm
  index.html                  # lobby (registry list, peer id, session join)
  host.js                     # env ABI (28 imports), rAF loop, input, HUD
  net.js                      # signaling WS client, RTCPeerConnection, DataChannel
                                # net:: framing (batch split, reliable overlay, ping/pong)
  render.js                   # three.js scene (avatars, boxes, score HUD)
  store.js                    # registry fetch, gateway IPFS fetch, manifest
                                # verify (WebCrypto sha256/ed25519), tar extract
```

Game traffic flows peer-to-peer over DataChannels; the signaling server
(hosted separately, never on InfinityFree) only exchanges SDP/ICE.
InfinityFree serves static files; PHP stays reserved for accounts and
leaderboards, out of the multiplayer path.

## 6. Transport design (browser side)

One `RTCDataChannel("game")` per peer, created with
`{ ordered: false, maxRetransmits: 0 }` to match the native endpoint
(lossy like UDP, so the reliable overlay stays meaningful on both ends).
The channel carries the existing `net::` bytes verbatim: 8/16-byte poses
(unsigned envelopes suffice; signed ones stay optional), `0x11` batches
split on receipt with sequence-gap loss counting, `0x12/0x13` reliable
frames with 100ms resend up to 3 tries, and 17-byte ping/pong for RTT.
Initiator is the lexicographically smaller peer id (mirrors native).
Native peers need no changes: their WebRTC session already speaks this
exact framing, so browser-to-native play works once the browser side
lands. Chunk/zone datagrams ride the same channel later; out of scope
for the demo.

## 7. Signaling, STUN/TURN, NAT

The Rust signaling server needs **no changes** for pairwise browser WebRTC:
it already relays offer/answer/ICE with multi-socket fanout and cleans up
on disconnect. The demo exchanges peer ids out-of-band (URL + copy-paste);
lobby discovery (`list_peers` + game filter) must be added to the Cloudflare
Worker later — it currently relays signaling only. Default STUN is Google
(stun.l.google.com:19302); no TURN is configured anywhere, so symmetric NATs
and restrictive firewalls will fail P2P — budget a TURN provider (or accept
LAN/full-cone-only connectivity) before any public demo.

## 8. Risks and blockers, ranked

1. **NAT traversal without TURN** — the likely cause if the demo cannot
   connect across networks. Mitigate with a TURN account first.
2. **DataChannel parameter mismatch** — browsers default to reliable
   ordered channels; must explicitly request unreliable/unordered to match
   the native side, or framing still works but latency characteristics differ.
3. **No execution metering** — a malicious or buggy game loop can only be
   bounded by time-slicing ticks and terminating runaway workers.
4. **Avatar art in browsers** — GLB loading via three.js GLTFLoader plus a
   JS port of the bone/budget validator; defer to box avatars for the demo.
5. **Safari/Firefox WebRTC quirks** — test Chrome first, then the rest;
   BigInt i64 imports and DataView LE floats are universal, ICE is not.
6. **InfinityFree constraints** — static files only, no WebSocket server:
   signaling stays external (brief confirms), gateway/CORS must be verified.

## 9. Smallest viable demo (Phase 4)

Extend the Studio preview into a 2-player session, in this order:
full `env` shim (28 imports) -> out-of-band peer-id join page ->
signaling WS client -> RTCPeerConnection + `"game"` channel -> pose
encode/decode + batching -> two box avatars + presence HUD +
join/leave/failure handling. Explicitly excluded: lobby discovery, chunk
DHT, IPFS persistence, cosmetics, avatar GLBs, reliable messaging.
Success = two browsers moving in one shared scene with live poses and
clean handling of join/leave/failure.

What this deliberately does not do: port Bevy, rewrite `net::`, replace
native UDP, touch native game flow, or add Node.js anywhere.

## 10. Open questions

- TURN provider for the public demo (account and credentials)?
- Demo game: existing Chase/Tag package (needs avatar art story) or a new
  minimal movement game (box avatars, no assets)?
- Is browser-to-native play (vs browser-to-browser) in scope for Phase 4?
- InfinityFree PHP scope: accounts/leaderboards now, or after multiplayer?
