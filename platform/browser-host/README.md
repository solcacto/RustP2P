# RustP2P browser host (static site, single-player core)

Upload this folder to any static host (InfinityFree, GitHub Pages). No server,
no Node, no build step for deploy.

## Layout

index.html (landing) - play.html (player) - host.js (wasm runtime + 2D canvas)
games/games.json (game list) - games/ID/game.json + game.wasm (one folder per game)

## Add a game (2 steps, no code changes to the site)

1. Copy games/dodge to games/MYID. Replace game.wasm with your build (keep
   the name game.wasm) and edit game.json (id, title, description).
2. Add one entry to games/games.json (copy the dodge block, change id/title/
   description/dir). The landing page lists it automatically.

Games must only use portable guest APIs (input, draw_box, score, transforms).
Networking calls safely do nothing until the multiplayer update.

## Build a game wasm

rustup target add wasm32-unknown-unknown
cd games/dodge  # or your game dir (needs [workspace] in its Cargo.toml)
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/dodge.wasm game.wasm

See games/dodge/src/lib.rs for the commented template (top-down collector).

## Test locally

Browsers block fetch() on file://, so serve the folder over http. Any static
server works, for example VS Code Live Server, or: python3 -m http.server
Then open http://127.0.0.1:8000/index.html

## Upload (InfinityFree / FTP)

Needed on the server: index.html, play.html, host.js, games/games.json, and
for each game its game.json + game.wasm. Rust sources (Cargo.toml, src/)
and target/ stay home. With FileZilla: connect, open htdocs, drag the
folder contents in, keeping the same relative layout.

## Multiplayer status

Single player only. Network imports are stubbed (marked NET-STUB in host.js)
for the WebRTC phase: DataChannels, lobby join, remote ghosts.
