#!/bin/bash
#
# publish.sh — zero-cost game publishing via Pinata (IPFS pinning).
#
#   Usage:
#     PINATA_JWT=<your-jwt> ./scripts/publish.sh [guest.wasm manifest.json]
#     PINATA_JWT=<your-jwt> ./scripts/publish.sh <bundle-dir>
#
#   Without arguments the script looks for ./build/guest.wasm and
#   ./build/manifest.json. With a single directory argument it looks for
#   <dir>/guest.wasm and <dir>/game_manifest.json (the layout the host's
#   bundle-preparation tooling produces). With two arguments, the files are
#   taken in order.
#
#   The script bundles both files into a single .tar.gz (the exact format the
#   host's IPFS `extract` understands), uploads it to Pinata, and prints the
#   resulting IPFS CID. Add that CID + a description to your GitHub Pages
#   games.json so players can discover and download the game.
#
# ---------------------------------------------------------------------------
#  SETTING PINATA_JWT
# ---------------------------------------------------------------------------
#   Pinata requires a JWT from https://app.pinata.cloud/developers/api-keys
#   (free tier: 1 GB of pinning, plenty for beta game bundles).
#
#   1. Create an API key in the Pinata dashboard ("API Keys" -> "New Key",
#      scope "pinFileToIPFS"). Copy the JWT.
#   2. Export it in your shell before running this script:
#
#        export PINATA_JWT="eyJhbGciOi..."
#
#      (Optionally add that export to ~/.zshrc / ~/.bashrc, or use a dotenv /
#       secrets manager. NEVER hardcode the JWT into this script or commit it.)
#
#   Verify with:
#        echo "${PINATA_JWT:0:12}... (set)"
#
# ---------------------------------------------------------------------------

set -euo pipefail

# --- 1. Load the Pinata JWT from the environment (never hardcode it). -------
if [ -z "${PINATA_JWT:-}" ]; then
    echo "Error: PINATA_JWT environment variable is not set." >&2
    echo "Get one at https://app.pinata.cloud/developers/api-keys and then run:" >&2
    echo '  export PINATA_JWT="<your-jwt>"' >&2
    exit 1
fi

# --- 2. Locate the game files (explicit args, bundle dir, or build/). ---------
# A single directory argument means <dir>/game_manifest.json plus the wasm file
# that manifest's `wasm_entry` names (so any game, not just `guest.wasm`, works).
if [ "$#" -eq 1 ] && [ -d "$1" ]; then
    MANIFEST="$1/game_manifest.json"
    WASM="$1/$(jq -r '.wasm_entry // "guest.wasm"' "$MANIFEST" 2>/dev/null || echo guest.wasm)"
else
    WASM="${1:-build/guest.wasm}"
    MANIFEST="${2:-build/manifest.json}"
fi

if [ ! -f "$WASM" ]; then
    echo "Error: game wasm not found at '$WASM'" >&2
    echo "Pass it explicitly, or build it into ./build/ first." >&2
    exit 1
fi
if [ ! -f "$MANIFEST" ]; then
    echo "Error: manifest not found at '$MANIFEST'" >&2
    exit 1
fi

# --- 3. Bundle into a .tar.gz (same layout the host IPFS extract expects). ---
# The host reads `game_manifest.json` at the bundle root and loads the wasm
# entry the manifest names, so the manifest is stored under its canonical name
# and the wasm under whatever `wasm_entry` the manifest declares.
STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT
cp "$MANIFEST" "$STAGING/game_manifest.json"

WASM_ENTRY="$(jq -r '.wasm_entry // "guest.wasm"' "$MANIFEST" 2>/dev/null || echo "guest.wasm")"
cp "$WASM" "$STAGING/$WASM_ENTRY"

BUNDLE="${STAGING}/game-bundle.tar.gz"
tar -czf "$BUNDLE" -C "$STAGING" game_manifest.json "$WASM_ENTRY"
echo "Bundled $(du -h "$BUNDLE" | cut -f1) game bundle (wasm: $WASM, manifest: $MANIFEST)"

# --- 4. Upload to Pinata's pinFileToIPFS endpoint. ---------------------------
# The bundle is attached as multipart form data; the JWT goes in the Bearer
# Authorization header. Metadata (name + a keyed value) makes the pin
# searchable in the Pinata dashboard.
RESPONSE="$(curl --fail --silent --show-error \
    -X POST "https://api.pinata.cloud/pinning/pinFileToIPFS" \
    -H "Authorization: Bearer ${PINATA_JWT}" \
    -F "file=@${BUNDLE};filename=game-bundle.tar.gz;type=application/gzip" \
    -F "pinataMetadata={\"name\":\"$(basename "$MANIFEST" .json)\",\"keyvalues\":{\"source\":\"rustp2p\"}}")"

# --- 5. Extract and print the IPFS CID. ---------------------------------------
# jq is preferred; fall back to grep/sed if it is not installed.
CID="$(echo "$RESPONSE" | jq -r '.IpfsHash // empty' 2>/dev/null)"
if [ -z "$CID" ]; then
    CID="$(echo "$RESPONSE" | grep -o '"IpfsHash"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"\([^"]*\)"$/\1/')"
fi
if [ -z "$CID" ]; then
    echo "Error: could not parse an IpfsHash from Pinata's response:" >&2
    echo "$RESPONSE" >&2
    exit 1
fi

echo "✓ Published to IPFS via Pinata"
echo "  CID: $CID"
echo "  Gateway URL: https://cloudflare-ipfs.com/ipfs/$CID"
echo
echo "Next: add this game to your GitHub Pages games.json (registry_url in"
echo "platform/host/config.toml), e.g.:"
echo '  {"name":"<game>","cid":"'"$CID"'","description":"...","author":"you","mode":"session"}'
echo "Then players can: cargo run -p host --bin launcher -- --cid $CID"