/**
 * Smoke test page for the app integration: runs the real CallManager with
 * transport 'relay' against a fake EventSocket (the harness plays
 * malguem-server). getDisplayMedia is stubbed with a canvas animation.
 */
import { CallManager, DEFAULT_BROADCAST } from '../../client/src/lib/call';
import { generateIdentity } from '../../client/src/lib/crypto';
import type { EventSocket, ServerEvent } from '../../client/src/lib/ws';

declare global {
  interface Window {
    __out: (kind: string, data: unknown) => Promise<void>;
    __event: (ev: ServerEvent) => void;
    __init: (id: string) => string;
    __setPubs: (pubs: Record<string, string>) => void;
    __join: () => Promise<void>;
    __share: () => Promise<void>;
    __status: () => unknown;
  }
}

const listeners = new Set<(ev: ServerEvent) => void>();
const fakeSocket = {
  onEvent(l: (ev: ServerEvent) => void) {
    listeners.add(l);
    return () => listeners.delete(l);
  },
  onOpen(_l: () => void) {
    return () => {};
  },
  send(ev: unknown) {
    void window.__out('send', ev);
  },
} as unknown as EventSocket;
window.__event = (ev) => {
  for (const l of listeners) l(ev);
};

// Stub screen capture: a moving canvas at 1280x720.
navigator.mediaDevices.getDisplayMedia = async () => {
  const c = document.createElement('canvas');
  c.width = 1280;
  c.height = 720;
  const ctx = c.getContext('2d')!;
  let n = 0;
  setInterval(() => {
    ctx.fillStyle = `hsl(${n % 360},60%,40%)`;
    ctx.fillRect(0, 0, 1280, 720);
    ctx.fillStyle = '#fff';
    ctx.fillRect((n * 9) % 1180, 300, 100, 100);
    n++;
  }, 33);
  return c.captureStream(30);
};

const b64 = (u: Uint8Array) => btoa(String.fromCharCode(...u));
const unb64 = (s: string) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
let myId = '';
const identity = generateIdentity();
const pubs = new Map<string, Uint8Array>();
let manager: CallManager;
const frames: Record<string, number> = {};
let relay: unknown = null;

window.__init = (id) => {
  myId = id;
  return b64(identity.signPub);
};
window.__setPubs = (p) => {
  for (const [k, v] of Object.entries(p)) pubs.set(k, unb64(v));
};
window.__join = async () => {
  manager = new CallManager(fakeSocket, identity, myId, 'space1', 'chan1', (u) => pubs.get(u), [], {
    onPeersChanged: () => {},
    onRemoteStreams: (userId, streams) => {
      for (const s of streams) {
        if (s.getVideoTracks().length === 0) continue;
        const v = document.createElement('video');
        v.muted = true;
        v.autoplay = true;
        v.srcObject = s;
        document.body.append(v);
        void v.play().catch(() => {});
        const cb = () => {
          frames[userId] = (frames[userId] ?? 0) + 1;
          v.requestVideoFrameCallback(cb);
        };
        v.requestVideoFrameCallback(cb);
      }
    },
    onStats: () => {},
    onBroadcastChanged: () => {},
    onPeerJoined: () => {},
    onPeerLeft: () => {},
    onEnded: () => {},
    onRelayStats: (s) => {
      relay = s;
    },
  });
  // The sharer can only feed one viewer directly, so the other must be relayed.
  const relayUploadMbps = myId === 'alice' ? 4 : 20;
  manager.settings = { ...DEFAULT_BROADCAST, transport: 'relay', systemAudio: false, frameRate: 30, height: 720, maxBitrateKbps: 3000, relayUploadMbps };
  await manager.join();
};
window.__share = async () => {
  await manager.startBroadcast();
};
window.__status = () => ({ frames, relay, relayBroadcasting: manager?.relayBroadcasting ?? false });
