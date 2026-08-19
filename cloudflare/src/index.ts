/**
 * p2p-signal — free-tier Cloudflare Worker WebSocket signaling server.
 *
 * The host client uses this only to exchange WebRTC signaling messages
 * (SDP offers/answers and ICE candidates). All game traffic flows over the
 * WebRTC data channel or UDP directly between peers — this worker never sees a
 * single game byte.
 *
 * Protocol (mirrors platform/signaling_server):
 *   - On connect the client sends {"type":"register","peer_id":"<id>",...} so
 *     this socket is associated with a peer id.
 *   - Signaling messages are routed by `to_peer`:
 *       {"type":"webrtc_offer","from_peer":A,"to_peer":B,"sdp":...}
 *       {"type":"webrtc_answer","from_peer":B,"to_peer":A,"sdp":...}
 *       {"type":"ice_candidate","from_peer":A,"to_peer":B,"candidate":...}
 *
 * State is an in-memory Map<string, WebSocket[]> — it resets on every deploy,
 * which is fine for a beta. A production deployment would move this into a
 * Durable Object, but the client protocol is identical either way.
 */

interface Env {
  // No bindings needed for the MVP.
}

/** Every open server-side socket, keyed by the registered peer id. */
const peers = new Map<string, WebSocket[]>();

const SIGNAL_TYPES = new Set(["webrtc_offer", "webrtc_answer", "ice_candidate"]);

function registerPeer(peerId: string, socket: WebSocket): void {
  const list = peers.get(peerId) ?? [];
  list.push(socket);
  peers.set(peerId, list);
}

function unregisterPeer(peerId: string, socket: WebSocket): void {
  const list = peers.get(peerId);
  if (!list) return;
  const remaining = list.filter((s) => s !== socket);
  if (remaining.length === 0) {
    peers.delete(peerId);
  } else {
    peers.set(peerId, remaining);
  }
}

/** Relays `text` to every open socket of `toPeer`, skipping `sender`. */
function relayTo(toPeer: string, text: string, sender: WebSocket): void {
  for (const socket of peers.get(toPeer) ?? []) {
    if (socket !== sender && socket.readyState === WebSocket.OPEN) {
      socket.send(text);
    }
  }
}

export default {
  async fetch(request: Request, _env: Env, _ctx: ExecutionContext): Promise<Response> {
    const upgradeHeader = request.headers.get("Upgrade");
    if (upgradeHeader !== "websocket") {
      return new Response("Expected WebSocket", { status: 400 });
    }

    const webSocketPair = new WebSocketPair();
    const [client, server] = Object.values(webSocketPair);
    server.accept();

    server.addEventListener("message", (event) => {
      let msg: any;
      try {
        msg = JSON.parse(event.data as string);
      } catch {
        return; // ignore malformed frames
      }

      if (msg.type === "register" && typeof msg.peer_id === "string") {
        registerPeer(msg.peer_id, server);
        return;
      }

      // Route WebRTC signaling to the addressed peer only.
      if (SIGNAL_TYPES.has(msg.type) && typeof msg.to_peer === "string") {
        relayTo(msg.to_peer, JSON.stringify(msg), server);
      }
    });

    server.addEventListener("close", () => {
      for (const peerId of [...peers.keys()]) {
        const list = peers.get(peerId);
        if (list && list.includes(server)) {
          unregisterPeer(peerId, server);
        }
      }
    });

    return new Response(null, { status: 101, webSocket: client });
  },
};
