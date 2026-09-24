/**
 * RelayHub: relayed screen-broadcast engine for one local participant.
 *
 * Media never re-encodes on the way: the broadcaster encodes once, and viewers
 * forward the encoded chunks to the children the broadcaster assigned them.
 * Every pair of call participants already shares an RTCPeerConnection (the
 * voice mesh), so a relay edge is just a few negotiated data channels on that
 * existing connection: no new ICE, no server bandwidth.
 *
 *   ctl  (reliable, ordered)    — JSON control messages and OWD pings
 *   base (reliable, unordered)  — keyframes, base temporal layer, audio
 *   enh  (unreliable, unordered) — enhancement temporal layers
 *
 * Control plane (all JSON over `ctl`):
 *   broadcaster → everyone : announce / end
 *   everyone → everyone    : ping (2 Hz, for uplink-queue estimation)
 *   viewer → broadcaster   : watch / unwatch / report (1 Hz) / kf / orphan
 *   broadcaster → viewer   : assign {parent, children}
 *
 * The broadcaster is the planner (planner.ts). Capacity per node starts at
 * what the participant declares and is cut only on direct evidence that the
 * node's *uplink* is queueing: the median, over all peers, of the one-way
 * delay increase they observe from it (owd.ts). A viewer whose own feed is
 * degraded while its parent's uplink is fine is moved once (maybe that one
 * path is bad); if it stays degraded, its own downlink is the bottleneck, so
 * it is kept as a leaf and simply receives fewer temporal layers.
 *
 * Parent changes are make-before-break: a parent keeps feeding a removed
 * child for DRAIN_MS, so the child's reference chain survives the switch.
 */
import { ChildLink, MAX_LAYER, type LinkTuning, type SendChannel } from './forwarder';
import { median, OwdTracker } from './owd';
import { planStar, planTree, edgeKey, type Plan, type PlanNode } from './planner';
import { chunkFrame, incrementHops, LayerDeps, readHeader, type EncodedFrame, type FrameHeader } from './protocol';
import { FrameReceiver, type DeliveredFrame } from './receiver';

export const CTL_CHANNEL_ID = 100;
export const BASE_CHANNEL_ID = 101;
export const ENH_CHANNEL_ID = 102;
export const KEYFRAME_MIN_INTERVAL_MS = 1000;
/** How long a parent keeps feeding a child it lost in a re-plan. */
export const DRAIN_MS = 1500;
/** After a node's children change, how long its uplink isn't judged (join keyframes). */
const SETTLE_MS = 4000;
/** Uplink queueing (median OWD increase) that counts as a saturated uplink. */
export const UPLINK_QUEUE_MS = 40;

/**
 * Media channel reliability. 'split' (default) = reliable base layer +
 * unreliable enhancement layers; the others put everything on one channel
 * and exist for comparison in the simulator.
 */
export type Reliability =
  | { kind: 'split'; enhLifetimeMs: number }
  | { kind: 'partial'; maxPacketLifeTime: number }
  | { kind: 'reliable' };

export interface HubOptions {
  myId: string;
  /** Declared upload capacity this node offers for relaying, kbps. */
  uploadCapacityKbps: number;
  /** Wall-clock ms, shared semantics with captureTs. */
  now?: () => number;
  reliability?: Reliability;
  strategy?: 'tree' | 'star';
  planner?: { headroom?: number; hopMs?: number; stickyMs?: number; maxDepth?: number };
  link?: Omit<LinkTuning, 'bitrateKbps'>;
  log?: (msg: string) => void;
}

export interface StreamInfo {
  broadcasterId: string;
  streamId: number;
  bitrateKbps: number;
  /** Opaque decoder configuration published by the broadcaster. */
  config: unknown;
}

export interface HubEvents {
  /** The set of live broadcasts we know about changed. */
  onStreams?: (streams: StreamInfo[]) => void;
  /** A decodable frame for a stream we watch, in decode order. */
  onFrame?: (broadcasterId: string, f: DeliveredFrame) => void;
  /** (Broadcaster) someone needs a keyframe: make the next frame one. */
  onKeyframeRequest?: () => void;
  /** (Broadcaster) the tree changed; for UI/diagnostics. */
  onPlan?: (plan: Plan) => void;
}

type Ctl =
  | { t: 'announce'; streamId: number; bitrateKbps: number; config: unknown }
  | { t: 'end'; streamId: number }
  | { t: 'ping'; ts: number }
  | { t: 'watch'; streamId: number; capKbps: number }
  | { t: 'unwatch'; streamId: number }
  | { t: 'assign'; streamId: number; epoch: number; parent: string; children: string[] }
  | { t: 'kf'; streamId: number }
  | { t: 'orphan'; streamId: number; parent: string | null }
  | ({ t: 'report'; streamId: number } & Report);

interface Report {
  capKbps: number;
  txKbps: number;
  /** Queueing this node observes on the path *from* each peer, ms. */
  inQ: Record<string, number>;
  rttMs: Record<string, number>;
  /** Viewer's own feed: highest temporal layer seen in the last second (-1 = none). */
  rxLayer: number;
  rxFps: number;
  parent: string | null;
}

interface Link {
  peerId: string;
  pc: RTCPeerConnection;
  ctl: RTCDataChannel;
  base: RTCDataChannel;
  enh: RTCDataChannel | null;
  rttMs: number | null;
}

interface CapState {
  declared: number;
  estimate: number;
  cuts: number;
  lastCutAt: number;
  /** Don't judge the uplink before this: its children just changed (keyframe burst). */
  settleUntil: number;
}

interface ViewerState extends CapState {
  id: string;
  lastReport: Report | null;
  lastReportAt: number;
  joinedAt: number;
  degradedSince: number | null;
  movedAt: number | null;
  weakUntil: number;
}

/** Children of one node for one stream, with make-before-break draining. */
class ChildSet {
  active = new Map<string, ChildLink>();
  draining = new Map<string, { link: ChildLink; until: number }>();

  set(want: string[], make: (id: string) => ChildLink | null, now: number): void {
    const w = new Set(want);
    for (const [id, link] of this.active) {
      if (!w.has(id)) {
        this.active.delete(id);
        this.draining.set(id, { link, until: now + DRAIN_MS });
      }
    }
    for (const id of w) {
      if (this.active.has(id)) continue;
      const d = this.draining.get(id);
      if (d) {
        this.draining.delete(id);
        this.active.set(id, d.link);
        continue;
      }
      const link = make(id);
      if (link) this.active.set(id, link);
    }
  }

  remove(id: string): void {
    this.active.delete(id);
    this.draining.delete(id);
  }

  clear(): void {
    this.active.clear();
    this.draining.clear();
  }

  /** Offer a chunk to every active and draining child; returns bytes sent. */
  offer(buf: ArrayBuffer, h: FrameHeader): number {
    let sent = 0;
    for (const c of this.active.values()) if (c.offer(buf, h)) sent += buf.byteLength;
    for (const d of this.draining.values()) if (d.link.offer(buf, h)) sent += buf.byteLength;
    return sent;
  }

  tick(now: number): void {
    for (const c of this.active.values()) c.adapt(now);
    for (const [id, d] of this.draining) if (now > d.until) this.draining.delete(id);
  }

  /** True (once) if any child's reference chain was cut by a dropped base frame. */
  takeChainBroken(): boolean {
    let broken = false;
    for (const c of this.all()) {
      if (c.chainBroken) {
        broken = true;
        c.chainBroken = false;
      }
    }
    return broken;
  }

  all(): ChildLink[] {
    return [...this.active.values(), ...[...this.draining.values()].map((d) => d.link)];
  }

  setBitrate(kbps: number): void {
    for (const c of this.all()) c.setBitrate(kbps);
  }
}

interface Outgoing {
  streamId: number;
  bitrateKbps: number;
  config: unknown;
  deps: LayerDeps;
  seqV: number;
  seqA: number;
  viewers: Map<string, ViewerState>;
  plan: Plan | null;
  epoch: number;
  children: ChildSet;
  forbidden: Map<string, number>; // edge → until
  lastPlanAt: number;
  replanWanted: boolean;
  lastKfAt: number;
  pendingKf: boolean;
  self: CapState;
}

interface Incoming {
  broadcasterId: string;
  info: StreamInfo;
  watching: boolean;
  parent: string | null;
  epoch: number;
  children: ChildSet;
  receiver: FrameReceiver;
  lastParentChunkAt: number;
  lastOrphanAt: number;
  watchSentAt: number;
  winFrames: number;
  winMaxTl: number;
  rxLayer: number;
  rxFps: number;
}

/** Stats snapshot for UI and the simulator. */
export interface HubStats {
  txKbps: number;
  inQ: Record<string, number>;
  links: Record<string, { txKbps: number; queueMs: number; maxLayer: number; rttMs: number | null }>;
  incoming: Record<
    string,
    {
      parent: string | null;
      children: string[];
      rxFps: number;
      rxLayer: number;
      waitingKeyframe: boolean;
      counters: FrameReceiver['counters'];
    }
  >;
  outgoing: null | {
    viewers: string[];
    tree: Record<string, string>;
    degraded: string[];
    capEstimates: Record<string, number>;
    uplinkQ: Record<string, number | null>;
    weak: string[];
    keyframes: number;
  };
}

const wallClock = () => performance.timeOrigin + performance.now();

export class RelayHub {
  private links = new Map<string, Link>();
  private out: Outgoing | null = null;
  private inc = new Map<number, Incoming>(); // by streamId
  private timer: ReturnType<typeof setInterval> | null = null;
  private tickN = 0;
  private now: () => number;
  private txBytesWin = 0;
  private txKbps = 0;
  private keyframesSent = 0;
  private owd = new OwdTracker();
  private linkTxPrev = new Map<string, number>();
  private linkTxKbps = new Map<string, number>();

  constructor(
    private opts: HubOptions,
    private ev: HubEvents = {},
  ) {
    this.now = opts.now ?? wallClock;
    this.timer = setInterval(() => this.tick(), 100);
  }

  dispose(): void {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
    if (this.out) this.stopBroadcast();
    for (const id of [...this.links.keys()]) this.detachPeer(id);
  }

  setUploadCapacity(kbps: number): void {
    this.opts.uploadCapacityKbps = kbps;
    if (this.out) {
      this.out.self.declared = kbps;
      this.out.self.estimate = kbps;
      this.out.replanWanted = true;
    }
  }

  // ---------------- peers ----------------

  /**
   * Attach the data channels to a peer connection. Must be called on both
   * sides (channels are pre-negotiated with fixed ids), before or after the
   * connection is up.
   */
  attachPeer(peerId: string, pc: RTCPeerConnection): void {
    if (this.links.has(peerId)) this.detachPeer(peerId);
    const rel = this.opts.reliability ?? { kind: 'split', enhLifetimeMs: 300 };
    const ctl = pc.createDataChannel('relay-ctl', { negotiated: true, id: CTL_CHANNEL_ID, ordered: true });
    const base = pc.createDataChannel('relay-base', {
      negotiated: true,
      id: BASE_CHANNEL_ID,
      ...(rel.kind === 'partial' ? { ordered: false, maxPacketLifeTime: rel.maxPacketLifeTime } : { ordered: rel.kind === 'reliable' }),
    });
    const enh =
      rel.kind === 'split'
        ? pc.createDataChannel('relay-enh', { negotiated: true, id: ENH_CHANNEL_ID, ordered: false, maxPacketLifeTime: rel.enhLifetimeMs })
        : null;
    const link: Link = { peerId, pc, ctl, base, enh, rttMs: null };
    this.links.set(peerId, link);
    ctl.onmessage = (e) => {
      try {
        this.onCtl(peerId, JSON.parse(e.data as string) as Ctl);
      } catch (err) {
        this.log(`bad ctl from ${peerId}: ${err}`);
      }
    };
    ctl.onopen = () => {
      // Late joiner: tell them about our broadcast; re-send watch if needed.
      if (this.out) {
        this.sendCtl(peerId, { t: 'announce', streamId: this.out.streamId, bitrateKbps: this.out.bitrateKbps, config: this.out.config });
      }
      for (const inc of this.inc.values()) {
        if (inc.broadcasterId === peerId && inc.watching) {
          this.sendCtl(peerId, { t: 'watch', streamId: inc.info.streamId, capKbps: this.opts.uploadCapacityKbps });
        }
      }
    };
    // The connection closed (peer left, or ICE/DTLS gave up): stop relaying
    // through it right away instead of waiting for the roster.
    ctl.onclose = () => {
      if (this.links.get(peerId)?.ctl === ctl) this.detachPeer(peerId);
    };
    for (const ch of [base, enh]) {
      if (!ch) continue;
      ch.binaryType = 'arraybuffer';
      ch.onmessage = (e) => this.onMedia(peerId, e.data as ArrayBuffer);
    }
    for (const ch of [ctl, base, enh]) {
      if (!ch) continue;
      ch.addEventListener('error', (e) => {
        const err = (e as RTCErrorEvent).error;
        this.log(`channel ${ch.label}↔${peerId} error: ${err?.message} (${err?.errorDetail}, sctp cause ${err?.sctpCauseCode})`);
      });
      ch.addEventListener('close', () => this.log(`channel ${ch.label}↔${peerId} closed (pc ${pc.connectionState}, sctp ${pc.sctp?.state})`));
    }
  }

  detachPeer(peerId: string): void {
    const link = this.links.get(peerId);
    if (!link) return;
    this.links.delete(peerId);
    this.owd.forget(peerId);
    for (const ch of [link.ctl, link.base, link.enh]) {
      try {
        ch?.close();
      } catch {
        /* already closed */
      }
    }
    for (const [sid, inc] of this.inc) {
      inc.children.remove(peerId);
      if (inc.broadcasterId === peerId) {
        this.inc.delete(sid); // the broadcaster left: its stream is gone
        this.emitStreams();
      } else if (inc.parent === peerId) {
        this.reportOrphan(inc);
        inc.parent = null;
      }
    }
    if (this.out) {
      this.out.children.remove(peerId);
      if (this.out.viewers.delete(peerId)) this.replan('viewer left', true);
    }
  }

  // ---------------- broadcaster ----------------

  startBroadcast(bitrateKbps: number, config: unknown): { streamId: number } {
    if (this.out) this.stopBroadcast();
    const streamId = (Math.random() * 0xffffffff) >>> 0;
    const cap = this.opts.uploadCapacityKbps;
    this.out = {
      streamId,
      bitrateKbps,
      config,
      deps: new LayerDeps(),
      seqV: 0,
      seqA: 0,
      viewers: new Map(),
      plan: null,
      epoch: 0,
      children: new ChildSet(),
      forbidden: new Map(),
      lastPlanAt: 0,
      replanWanted: false,
      lastKfAt: -Infinity,
      pendingKf: false,
      self: { declared: cap, estimate: cap, cuts: 0, lastCutAt: -Infinity, settleUntil: 0 },
    };
    for (const peerId of this.links.keys()) this.sendCtl(peerId, { t: 'announce', streamId, bitrateKbps, config });
    return { streamId };
  }

  stopBroadcast(): void {
    if (!this.out) return;
    const { streamId } = this.out;
    this.out = null;
    for (const peerId of this.links.keys()) this.sendCtl(peerId, { t: 'end', streamId });
  }

  /** Change the target bitrate (e.g. user setting). Replans capacity. */
  setBroadcastBitrate(kbps: number): void {
    if (!this.out) return;
    this.out.bitrateKbps = kbps;
    this.out.children.setBitrate(kbps);
    for (const peerId of this.links.keys()) {
      this.sendCtl(peerId, { t: 'announce', streamId: this.out.streamId, bitrateKbps: kbps, config: this.out.config });
    }
    this.replan('bitrate', true);
  }

  /**
   * Send one encoded frame. `tl` is the temporal layer id reported by the
   * encoder (0 when not using SVC). Returns the assigned sequence number.
   */
  sendFrame(f: { key: boolean; audio?: boolean; tl: number; data: Uint8Array; mediaTs: number; captureTs?: number }): number {
    const out = this.out;
    if (!out) return -1;
    const audio = f.audio ?? false;
    const seq = audio ? out.seqA++ : out.seqV++;
    const depSeq = audio ? seq : out.deps.next(seq, f.tl, f.key);
    if (f.key && !audio) this.keyframesSent++;
    const frame: EncodedFrame = {
      key: !audio && f.key,
      audio,
      tl: audio ? 0 : f.tl,
      seq,
      depSeq,
      captureTs: f.captureTs ?? this.now(),
      mediaTs: f.mediaTs,
      data: f.data,
    };
    for (const c of chunkFrame(out.streamId, frame)) {
      this.txBytesWin += out.children.offer(c, readHeader(c)!);
    }
    return seq;
  }

  // ---------------- viewer ----------------

  /** Known live broadcasts. */
  streams(): StreamInfo[] {
    return [...this.inc.values()].map((i) => i.info);
  }

  watch(broadcasterId: string): void {
    const inc = this.findIncoming(broadcasterId);
    if (!inc || inc.watching) return;
    inc.watching = true;
    inc.watchSentAt = this.now();
    this.sendCtl(broadcasterId, { t: 'watch', streamId: inc.info.streamId, capKbps: this.opts.uploadCapacityKbps });
  }

  unwatch(broadcasterId: string): void {
    const inc = this.findIncoming(broadcasterId);
    if (!inc || !inc.watching) return;
    inc.watching = false;
    inc.parent = null;
    inc.children.clear();
    this.sendCtl(broadcasterId, { t: 'unwatch', streamId: inc.info.streamId });
  }

  // ---------------- stats ----------------

  stats(): HubStats {
    const links: HubStats['links'] = {};
    const allChildren = [...(this.out?.children.all() ?? []), ...[...this.inc.values()].flatMap((i) => i.children.all())];
    for (const [id, l] of this.links) {
      const cl = allChildren.find((c) => c.childId === id);
      links[id] = { txKbps: this.linkTxKbps.get(id) ?? 0, queueMs: cl?.queueMs ?? 0, maxLayer: cl?.maxLayer ?? MAX_LAYER, rttMs: l.rttMs };
    }
    const incoming: HubStats['incoming'] = {};
    for (const inc of this.inc.values()) {
      incoming[inc.broadcasterId] = {
        parent: inc.parent,
        children: [...inc.children.active.keys()],
        rxFps: inc.rxFps,
        rxLayer: inc.rxLayer,
        waitingKeyframe: inc.receiver.waitingForKeyframe,
        counters: { ...inc.receiver.counters },
      };
    }
    let outgoing: HubStats['outgoing'] = null;
    const out = this.out;
    if (out) {
      const tree: Record<string, string> = {};
      for (const [c, p] of out.plan?.parent ?? []) tree[c] = p;
      const caps: Record<string, number> = { [this.opts.myId]: Math.round(out.self.estimate) };
      const upQ: Record<string, number | null> = { [this.opts.myId]: this.uplinkQueue(this.opts.myId) };
      for (const v of out.viewers.values()) {
        caps[v.id] = Math.round(v.estimate);
        upQ[v.id] = this.uplinkQueue(v.id);
      }
      const t = this.now();
      outgoing = {
        viewers: [...out.viewers.keys()],
        tree,
        degraded: [...(out.plan?.degraded ?? [])],
        capEstimates: caps,
        uplinkQ: upQ,
        weak: [...out.viewers.values()].filter((v) => t < v.weakUntil).map((v) => v.id),
        keyframes: this.keyframesSent,
      };
    }
    return { txKbps: this.txKbps, inQ: this.owd.all(), links, incoming, outgoing };
  }

  // ---------------- internals ----------------

  private log(msg: string) {
    this.opts.log?.(msg);
  }

  private findIncoming(broadcasterId: string): Incoming | undefined {
    return [...this.inc.values()].find((i) => i.broadcasterId === broadcasterId);
  }

  private makeChild(bitrateKbps: number) {
    return (id: string): ChildLink | null => {
      const l = this.links.get(id);
      if (!l) return null;
      return new ChildLink(id, l.base as SendChannel, l.enh as SendChannel | null, { ...(this.opts.link ?? {}), bitrateKbps });
    };
  }

  private sendCtl(peerId: string, msg: Ctl): void {
    const l = this.links.get(peerId);
    if (!l || l.ctl.readyState !== 'open') return;
    try {
      l.ctl.send(JSON.stringify(msg));
    } catch {
      /* closing */
    }
  }

  private emitStreams() {
    this.ev.onStreams?.(this.streams());
  }

  private onCtl(from: string, m: Ctl): void {
    switch (m.t) {
      case 'ping':
        this.owd.sample(from, m.ts, this.now());
        break;
      case 'announce': {
        let inc = this.inc.get(m.streamId);
        if (!inc) {
          // Drop any older stream from the same broadcaster.
          for (const [sid, other] of this.inc) if (other.broadcasterId === from) this.inc.delete(sid);
          inc = this.newIncoming(from, { broadcasterId: from, streamId: m.streamId, bitrateKbps: m.bitrateKbps, config: m.config });
          this.inc.set(m.streamId, inc);
        } else {
          inc.info.bitrateKbps = m.bitrateKbps;
          inc.info.config = m.config;
          inc.children.setBitrate(m.bitrateKbps);
        }
        this.emitStreams();
        break;
      }
      case 'end':
        if (this.inc.delete(m.streamId)) this.emitStreams();
        break;
      case 'assign': {
        const inc = this.inc.get(m.streamId);
        if (!inc || from !== inc.broadcasterId || m.epoch < inc.epoch || !inc.watching) return;
        inc.epoch = m.epoch;
        if (inc.parent !== m.parent) inc.lastParentChunkAt = this.now();
        inc.parent = m.parent;
        inc.children.set(m.children, this.makeChild(inc.info.bitrateKbps), this.now());
        break;
      }
      // ----- broadcaster side -----
      case 'watch': {
        const out = this.out;
        if (!out || m.streamId !== out.streamId) return;
        if (!out.viewers.has(from)) {
          const t = this.now();
          out.viewers.set(from, {
            id: from,
            declared: m.capKbps,
            estimate: m.capKbps,
            cuts: 0,
            lastCutAt: -Infinity,
            settleUntil: 0,
            lastReport: null,
            lastReportAt: t,
            joinedAt: t,
            degradedSince: null,
            movedAt: null,
            weakUntil: 0,
          });
          this.replan(`watch ${from}`, true);
        }
        this.requestKeyframe();
        break;
      }
      case 'unwatch': {
        const out = this.out;
        if (!out || m.streamId !== out.streamId) return;
        if (out.viewers.delete(from)) {
          out.children.remove(from);
          this.replan(`unwatch ${from}`, true);
        }
        break;
      }
      case 'kf':
        if (this.out && m.streamId === this.out.streamId) this.requestKeyframe();
        break;
      case 'orphan': {
        const out = this.out;
        if (!out || m.streamId !== out.streamId || !out.viewers.has(from)) return;
        const p = m.parent;
        if (p && p !== this.opts.myId) {
          const pv = out.viewers.get(p);
          if (pv && this.now() - pv.lastReportAt > 1500) pv.estimate = 0; // parent looks dead
          out.forbidden.set(edgeKey(p, from), this.now() + 15000);
        }
        this.replan(`orphan ${from}`, true);
        break;
      }
      case 'report':
        this.onReport(from, m);
        break;
    }
  }

  private newIncoming(from: string, info: StreamInfo): Incoming {
    const inc: Incoming = {
      broadcasterId: from,
      info,
      watching: false,
      parent: null,
      epoch: -1,
      children: new ChildSet(),
      receiver: null as unknown as FrameReceiver,
      lastParentChunkAt: 0,
      lastOrphanAt: 0,
      watchSentAt: 0,
      winFrames: 0,
      winMaxTl: -1,
      rxLayer: -1,
      rxFps: 0,
    };
    inc.receiver = new FrameReceiver({
      now: this.now,
      onFrame: (f) => {
        if (!f.audio) {
          inc.winFrames++;
          inc.winMaxTl = Math.max(inc.winMaxTl, f.tl);
        }
        this.ev.onFrame?.(inc.broadcasterId, f);
      },
      onNeedKeyframe: () => this.sendCtl(inc.broadcasterId, { t: 'kf', streamId: inc.info.streamId }),
    });
    return inc;
  }

  private onMedia(from: string, buf: ArrayBuffer): void {
    const h = readHeader(buf);
    if (!h) return;
    const inc = this.inc.get(h.streamId);
    if (!inc || !inc.watching) return;
    if (from === inc.parent) {
      inc.lastParentChunkAt = this.now();
      // Cut-through: forward each chunk before reassembly/decoding. Only the
      // assigned parent's chunks are forwarded (a draining old parent's are
      // used locally only), so a switch never doubles traffic downstream.
      if (inc.children.active.size + inc.children.draining.size > 0) {
        incrementHops(buf); // `h` keeps the hop count as received
        this.txBytesWin += inc.children.offer(buf, h);
      }
    }
    inc.receiver.push(buf, h);
  }

  /**
   * Keyframes are ~8× a normal frame and every viewer in the tree pays for
   * each one, so requests are coalesced: at most one per
   * KEYFRAME_MIN_INTERVAL_MS; a request inside that window is deferred.
   */
  private requestKeyframe(): void {
    const out = this.out;
    if (!out) return;
    const t = this.now();
    if (t - out.lastKfAt < KEYFRAME_MIN_INTERVAL_MS) {
      out.pendingKf = true;
      return;
    }
    out.pendingKf = false;
    out.lastKfAt = t;
    this.ev.onKeyframeRequest?.();
  }

  private reportOrphan(inc: Incoming): void {
    const t = this.now();
    if (t - inc.lastOrphanAt < 1000) return;
    inc.lastOrphanAt = t;
    this.sendCtl(inc.broadcasterId, { t: 'orphan', streamId: inc.info.streamId, parent: inc.parent });
  }

  /**
   * Uplink queue of `id`: median over every peer that measures it (the
   * viewers' reports plus our own pings) of the OWD increase they see.
   */
  private uplinkQueue(id: string): number | null {
    const out = this.out;
    if (!out) return null;
    const xs: number[] = [];
    if (id !== this.opts.myId) {
      const mine = this.owd.queueMs(id);
      if (mine !== null) xs.push(mine);
    }
    for (const v of out.viewers.values()) {
      if (v.id === id || !v.lastReport || this.now() - v.lastReportAt > 3000) continue;
      const q = v.lastReport.inQ[id];
      if (q !== undefined) xs.push(q);
    }
    return xs.length >= 2 ? median(xs) : null;
  }

  private onReport(from: string, r: Report & { streamId: number }): void {
    const out = this.out;
    if (!out || r.streamId !== out.streamId) return;
    const v = out.viewers.get(from);
    if (!v) return;
    const t = this.now();
    const wasSilent = v.lastReport !== null && t - v.lastReportAt > 3000;
    v.lastReport = r;
    v.lastReportAt = t;
    if (v.declared !== r.capKbps) {
      v.declared = r.capKbps;
      v.estimate = r.capKbps;
      out.replanWanted = true;
    }
    if (wasSilent) {
      v.estimate = Math.max(v.estimate, Math.min(v.declared, out.bitrateKbps)); // back from the dead
      out.replanWanted = true;
    }

    // Is this viewer's feed degraded while its parent's uplink is fine?
    // Starving (no video at all while watching) counts as degraded too.
    const degraded = (r.rxLayer >= 0 && r.rxLayer < MAX_LAYER) || (r.rxFps === 0 && t - v.joinedAt > 5000);
    if (!degraded) {
      v.degradedSince = null;
      return;
    }
    v.degradedSince ??= t;
    if (t < v.weakUntil || t - v.degradedSince < 3000 || !r.parent) return;
    // A child can't get more layers than its parent receives: if the parent
    // is degraded too, the problem is upstream, not this viewer's.
    const pr = out.viewers.get(r.parent)?.lastReport;
    if (pr && (pr.rxLayer < MAX_LAYER || pr.rxFps === 0)) return;
    const pq = this.uplinkQueue(r.parent);
    if (pq !== null && pq > UPLINK_QUEUE_MS / 2) return; // parent's uplink; handled in everySecond()
    // Moving only makes sense if some other parent could take it.
    const alternatives =
      (out.self.estimate > 0 && r.parent !== this.opts.myId) ||
      [...out.viewers.values()].some((o) => o.id !== from && o.id !== r.parent && o.estimate >= out.bitrateKbps && t >= o.weakUntil);
    if (!alternatives) {
      v.weakUntil = t + 120000;
      return;
    }
    if (v.movedAt === null || t - v.movedAt > 30000) {
      // Maybe it's just this path (e.g. an ISP interconnect): try another parent once.
      this.log(`${from} degraded under ${r.parent} (parent uplink ok); trying another parent`);
      v.movedAt = t;
      v.degradedSince = null;
      out.forbidden.set(edgeKey(r.parent, from), t + 60000);
      this.replan(`move ${from}`, true);
    } else if (t - v.movedAt > 4000) {
      // Degraded under two parents with healthy uplinks: its own downlink.
      this.log(`${from} is limited by its own connection; keeping it a leaf`);
      v.weakUntil = t + 120000;
      out.replanWanted = true;
    }
  }

  private cut(c: CapState, to: number, who: string, why: string): void {
    this.log(`cap ${who}: ${Math.round(c.estimate)} → ${Math.round(to)} (${why})`);
    c.estimate = to;
    c.cuts++;
    c.lastCutAt = this.now();
    this.out!.replanWanted = true;
  }

  /**
   * Estimates only ever get cut by evidence, so they must also be allowed to
   * grow back (a backup that finished). After a quiet period that doubles
   * with every cut (10 s, 20 s, … 160 s), step back up towards the declared
   * value, one stream's worth every 5 s.
   */
  private recover(c: CapState, t: number, step: number): void {
    if (c.estimate >= c.declared) return;
    const quiet = 10_000 * 2 ** Math.min(4, Math.max(0, c.cuts - 1));
    if (t - c.lastCutAt < quiet) return;
    c.estimate = Math.min(c.declared, c.estimate + step);
    c.lastCutAt = t - quiet + 5000;
    this.out!.replanWanted = true;
  }

  /** Recompute the tree and push assignment changes. */
  private replan(reason: string, urgent = false): void {
    const out = this.out;
    if (!out) return;
    const t = this.now();
    if (!urgent && t - out.lastPlanAt < 1000) {
      out.replanWanted = true;
      return;
    }
    out.lastPlanAt = t;
    out.replanWanted = false;
    for (const [e, until] of out.forbidden) if (until < t) out.forbidden.delete(e);

    // Viewers silent for a while (crashed, lost network) are left out until
    // they report again; the roster will remove them for good.
    const live = [...out.viewers.values()].filter((v) => t - v.lastReportAt < 5000 || t - v.joinedAt < 5000);
    const viewers: PlanNode[] = live.map((v) => ({
      id: v.id,
      capacityKbps: t - v.lastReportAt > 3000 && t - v.joinedAt > 3000 ? 0 : v.estimate,
      rttMs: { ...(v.lastReport?.rttMs ?? {}) },
    }));
    for (const pn of viewers) {
      const r = this.links.get(pn.id)?.rttMs;
      if (r != null) pn.rttMs[this.opts.myId] = r;
    }
    const noRelay = new Set(live.filter((v) => t < v.weakUntil).map((v) => v.id));
    const prevParent = out.plan?.parent ?? new Map<string, string>();
    const plan =
      this.opts.strategy === 'star'
        ? planStar(this.opts.myId, viewers)
        : planTree({
            root: this.opts.myId,
            rootCapacityKbps: out.self.estimate,
            bitrateKbps: out.bitrateKbps,
            viewers,
            prevParent,
            forbidden: new Set(out.forbidden.keys()),
            noRelay,
            ...this.opts.planner,
          });
    const prevChildren = out.plan?.children ?? new Map<string, string[]>();
    out.plan = plan;
    out.epoch++;
    this.log(`plan (${reason}): ${[...plan.parent].map(([c, p]) => `${p}→${c}`).join(' ')}`);

    const sameKids = (a: string[], b: string[]) => a.length === b.length && a.every((c) => b.includes(c));
    const mine = plan.children.get(this.opts.myId) ?? [];
    if (!sameKids(mine, prevChildren.get(this.opts.myId) ?? [])) out.self.settleUntil = t + SETTLE_MS;
    out.children.set(mine, this.makeChild(out.bitrateKbps), t);
    for (const v of live) {
      const id = v.id;
      const parent = plan.parent.get(id);
      if (!parent) continue;
      const children = plan.children.get(id) ?? [];
      const before = prevChildren.get(id) ?? [];
      if (!sameKids(children, before)) v.settleUntil = t + SETTLE_MS;
      const changed = prevParent.get(id) !== parent || !sameKids(children, before);
      if (changed || reason === `watch ${id}`) {
        this.sendCtl(id, { t: 'assign', streamId: out.streamId, epoch: out.epoch, parent, children });
      }
    }
    this.ev.onPlan?.(plan);
  }

  private tick(): void {
    const t = this.now();
    this.tickN++;
    this.out?.children.tick(t);
    if (this.out && this.out.children.takeChainBroken()) this.requestKeyframe();
    for (const inc of this.inc.values()) {
      inc.children.tick(t);
      if (inc.children.takeChainBroken()) {
        this.sendCtl(inc.broadcasterId, { t: 'kf', streamId: inc.info.streamId });
      }
      if (!inc.watching) continue;
      inc.receiver.tick();
      // Silence from our parent → ask the broadcaster for a new one.
      if (inc.parent && t - inc.lastParentChunkAt > 800 && t - inc.watchSentAt > 1500) this.reportOrphan(inc);
    }
    if (this.out) {
      if (this.out.pendingKf && t - this.out.lastKfAt >= KEYFRAME_MIN_INTERVAL_MS) this.requestKeyframe();
      if (this.out.replanWanted && t - this.out.lastPlanAt > 1000) this.replan('periodic');
    }
    if (this.tickN % 5 === 0) {
      for (const id of this.links.keys()) this.sendCtl(id, { t: 'ping', ts: t });
    }
    if (this.tickN % 10 === 0) this.everySecond(t);
    if (this.tickN % 20 === 0) void this.pollRtt();
  }

  private everySecond(t: number): void {
    this.txKbps = (this.txBytesWin * 8) / 1000;
    this.txBytesWin = 0;
    const perLink = new Map<string, number>();
    const allChildren = [...(this.out?.children.all() ?? []), ...[...this.inc.values()].flatMap((i) => i.children.all())];
    for (const c of allChildren) perLink.set(c.childId, (perLink.get(c.childId) ?? 0) + c.bytesSent);
    this.linkTxKbps.clear();
    for (const [id, bytes] of perLink) {
      const prev = this.linkTxPrev.get(id) ?? 0;
      this.linkTxKbps.set(id, bytes >= prev ? ((bytes - prev) * 8) / 1000 : 0);
    }
    this.linkTxPrev = perLink;

    const rtt: Record<string, number> = {};
    for (const [id, l] of this.links) if (l.rttMs != null) rtt[id] = Math.round(l.rttMs);
    const inQ = this.owd.all();
    for (const inc of this.inc.values()) {
      inc.rxFps = inc.winFrames;
      inc.rxLayer = inc.winMaxTl;
      inc.winFrames = 0;
      inc.winMaxTl = -1;
      if (!inc.watching) continue;
      this.sendCtl(inc.broadcasterId, {
        t: 'report',
        streamId: inc.info.streamId,
        capKbps: this.opts.uploadCapacityKbps,
        txKbps: this.txKbps,
        inQ,
        rttMs: rtt,
        rxLayer: inc.rxLayer,
        rxFps: inc.rxFps,
        parent: inc.parent,
      });
    }

    const out = this.out;
    if (!out) return;
    // Uplink evidence for every node (including us): a standing queue on its
    // uplink means it is asked for more than it can send. Cut its estimate to
    // what it actually sustained.
    const step = out.bitrateKbps;
    const check = (id: string, c: CapState, txKbps: number) => {
      const q = this.uplinkQueue(id);
      // After a cut, give the re-plan time to drain the queue before judging
      // again; after a change of children, let their join keyframes drain.
      if (t - c.lastCutAt < 3000 || t < c.settleUntil) return;
      if (q !== null && q > UPLINK_QUEUE_MS && txKbps > 0) {
        // Multiplicative decrease, floored at what it demonstrably sent: the
        // measured send rate alone undershoots badly while SCTP is backing off.
        const to = Math.max(txKbps * 0.9, c.estimate * 0.5);
        if (to < c.estimate * 0.95) this.cut(c, to, id, `uplink queue ${Math.round(q)} ms`);
      } else if (q === null || q < UPLINK_QUEUE_MS / 4) {
        this.recover(c, t, step);
      }
    };
    check(this.opts.myId, out.self, this.txKbps);
    for (const v of out.viewers.values()) {
      if (v.lastReport && t - v.lastReportAt < 3000) check(v.id, v, v.lastReport.txKbps);
      // Stopped reporting (crash / network loss) while still feeding someone:
      // re-plan so replan() stops using it as a relay.
      if (t - v.lastReportAt > 3000 && (out.plan?.children.get(v.id)?.length ?? 0) > 0) {
        this.log(`${v.id} stopped reporting; not using it as a relay`);
        out.replanWanted = true;
      }
    }
  }

  private async pollRtt(): Promise<void> {
    for (const l of this.links.values()) {
      try {
        const report = await l.pc.getStats();
        report.forEach((s) => {
          if (s.type === 'candidate-pair' && s.nominated && s.state === 'succeeded' && s.currentRoundTripTime != null) {
            l.rttMs = s.currentRoundTripTime * 1000;
          }
        });
      } catch {
        /* closed */
      }
    }
  }
}
