# Game Manifest Format

Every game package must ship a `game_manifest.json` next to its Wasm module.
The host **validates the manifest before loading any Wasm** and refuses to load
anything whose pinned hash doesn't match the actual artifact. This is the seed
of the platform's distribution and integrity story.

## Example

```json
{
  "name": "Chase Tag",
  "version": "0.1.0",
  "author": "dev_peer_id",
  "wasm_entry": "guest.wasm",
  "wasm_hash": "sha256:abc123...",
  "mode": "session",
  "max_players": 8,
  "avatar_skeleton": "standard_v1",
  "host_functions_required": ["input", "network", "render"]
}
```

## Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Human-readable game title. Must be non-empty. |
| `version` | string | yes | Game version (e.g. `0.1.0`). Must be non-empty. |
| `author` | string | yes | Peer id / identity of the developer. Must be non-empty. |
| `wasm_entry` | string | yes | Filename of the Wasm module in the package. Must be a **bare filename** (no `/`, `\`, or `..`). |
| `wasm_hash` | string | yes | Integrity digest: `sha256:` followed by exactly 64 hex digits. |
| `mode` | enum | yes | `session` (bounded multiplayer session). |
| `max_players` | integer | yes | Session capacity, within `1..=64`. |
| `avatar_skeleton` | enum | yes | `standard_v1` (the platform's built-in Universal Avatar). |
| `host_functions_required` | array of enums | yes | Host-function families the game needs: `input`, `network`, `render`, `session`. No duplicates; must be supported by the host. |

## Validation rules

The host (`host::manifest::GameManifest`) enforces, in order:

1. `name`, `version`, `author` are non-empty.
2. `wasm_entry` is a bare filename — path separators and `..` are rejected so a
   manifest can never make the host read outside the game package.
3. `wasm_hash` is `sha256:` + 64 hex digits.
4. `mode` and `avatar_skeleton` are known enum values (unknown values fail to
   parse).
5. `max_players` is within `1..=64`.
6. `host_functions_required` contains only supported, non-duplicate categories.

## Hash verification

Before instantiation the host:

1. Resolves `wasm_entry` inside the package directory.
2. Computes `sha256` of the Wasm bytes.
3. Compares it to `wasm_hash`.

If either the entry name or the digest mismatches, the module is **refused**
and the process exits with an error — the tampered artifact is never loaded.

## Authoring a manifest

After building the guest:

```sh
cargo build -p guest --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/guest.wasm platform/guest/guest.wasm
cargo run -p host --bin hash_wasm -- platform/guest/guest.wasm
# -> sha256:<64 hex digits> — paste into wasm_hash
```

Run the validation suite with:

```sh
cargo run -p host --bin manifest_test
```

The shipped example lives at `platform/guest/game_manifest.json` next to the
committed `platform/guest/guest.wasm`.
