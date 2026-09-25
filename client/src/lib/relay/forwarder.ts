/**
 * Per-child sending side of a relay (or of the broadcaster itself).
 *
 * The same chunk buffers are offered to every child; each ChildLink decides
 * per frame whether its child gets it. Congestion shows up as the data
 * channel's `bufferedAmount` growing (SCTP can't drain it), and the response
 * is to stop forwarding the highest temporal layer (60 → 30 → 15 fps) without
 * any re-encoding. When the queue stays short for a while we probe back up.
 *
 * Decisions are made on a frame's first chunk and remembered for its other
 * chunks, so a child never receives half a frame on purpose. A frame whose
 * reference was intentionally dropped for this child is dropped too.
 *
 * Two channels per child: base-layer frames, keyframes and audio go on a
 * reliable channel (the reference chain must not break, or every viewer pays
 * for a keyframe), enhancement layers on an unreliable one (a lost T1/T2
 * frame costs nothing downstream).
 */
import type { FrameHeader } from './protocol';

export interface SendChannel {
  readonly bufferedAmount: number;
  readonly readyState: RTCDataChannelState;
  send(data: ArrayBuffer): void;
}

export interface LinkTuning {
  bitrateKbps: number;
  /** Queue (as ms of stream bitrate) above which we shed a layer. */
  highWaterMs?: number;
  /** Queue below which the link counts as healthy. */
  lowWaterMs?: number;
  /** Queue above which even base-layer frames are dropped. */
  hardLimitMs?: number;
  /** Healthy time required before re-adding a layer. */
  probeAfterMs?: number;
  /** Minimum time between two layer reductions. */
  downAfterMs?: number;
  /** adapt() calls over which the standing queue (minimum) is taken. */
  windowTicks?: number;
}

export const MAX_LAYER = 2;

export class ChildLink {
  /** Highest temporal layer currently forwarded (congestion control). */
  maxLayer = MAX_LAYER;
  /** Highest layer the child asked for (busy or hidden child); not congestion. */
  ceiling = MAX_LAYER;
  bytesSent = 0;
  framesSent = 0;
  framesDropped = 0;
  /** Since when the link has been below full quality (null = healthy). */
  degradedSince: number | null = null;
  /**
   * Set when this link dropped a base-layer frame: the child's reference
   * chain is broken and nothing more can be forwarded to it until a
   * keyframe. The child can't notice (it simply receives nothing), so the
   * relay must ask for the keyframe itself. The hub clears it.
   */
  chainBroken = false;
  private decisions = new Map<string, boolean>();
  private skipped = new Set<number>();
  private lastChange = -Infinity;
  private healthySince: number | null = null;
  private window: number[] = [];

  constructor(
    readonly childId: string,
    private base: SendChannel,
    /** Enhancement-layer channel; null = everything on `base`. */
    private enh: SendChannel | null,
    private tuning: LinkTuning,
  ) {}

  private get buffered(): number {
    return this.base.bufferedAmount + (this.enh?.bufferedAmount ?? 0);
  }

  setBitrate(kbps: number): void {
    this.tuning.bitrateKbps = kbps;
  }

  private bytesForMs(ms: number): number {
    return (this.tuning.bitrateKbps / 8) * ms; // kbps/8 = bytes per ms
  }

  /** Queue length expressed in ms of stream bitrate. */
  get queueMs(): number {
    return this.buffered / Math.max(1, this.tuning.bitrateKbps / 8);
  }

  /** Offer one chunk; returns true if it was sent to this child. */
  offer(buf: ArrayBuffer, h: FrameHeader): boolean {
    const ch = this.enh && !h.audio && !h.key && h.tl > 0 ? this.enh : this.base;
    if (ch.readyState !== 'open') return false;
    const k = `${h.audio ? 'a' : 'v'}${h.seq}`;
    let send = this.decisions.get(k);
    if (send === undefined) {
      send = this.decide(h);
      this.decisions.set(k, send);
      if (this.decisions.size > 256) {
        const first = this.decisions.keys().next().value;
        if (first !== undefined) this.decisions.delete(first);
      }
      if (!h.audio) {
        if (send) this.framesSent++;
        else this.framesDropped++;
      }
    }
    if (!send) return false;
    try {
      ch.send(buf);
    } catch {
      return false; // channel closing; the hub will notice
    }
    this.bytesSent += buf.byteLength;
    return true;
  }

  private decide(h: FrameHeader): boolean {
    const q = this.buffered;
    const hard = this.bytesForMs(this.tuning.hardLimitMs ?? 500);
    if (h.audio) return q < hard * 2;
    if (h.key) {
      this.skipped.clear();
      this.chainBroken = false;
      return true;
    }
    if (h.tl > Math.min(this.maxLayer, this.ceiling) || this.skipped.has(h.depSeq) || q > hard) {
      if (h.tl === 0 && !this.skipped.has(h.depSeq)) this.chainBroken = true;
      this.skipped.add(h.seq);
      if (this.skipped.size > 256) {
        const first = this.skipped.values().next().value;
        if (first !== undefined) this.skipped.delete(first);
      }
      return false;
    }
    return true;
  }

  /**
   * Call every ~100 ms. Uses the *standing* queue (minimum over the last
   * ~500 ms, as in CoDel) rather than the instantaneous one: a keyframe is
   * handed to the channel in one go and legitimately sits in the buffer for a
   * few RTTs, which must not read as congestion. Only a queue that never
   * drains means the path can't carry the stream.
   */
  adapt(now: number): void {
    const ticks = this.tuning.windowTicks ?? 5;
    this.window.push(this.buffered);
    if (this.window.length > ticks) this.window.shift();
    // Until the window fills, treat the link as not (yet) congested.
    const q = this.window.length < ticks ? 0 : Math.min(...this.window);
    const high = this.bytesForMs(this.tuning.highWaterMs ?? 60);
    const low = this.bytesForMs(this.tuning.lowWaterMs ?? 15);
    if (q > high) {
      this.healthySince = null;
      if (this.maxLayer > 0 && now - this.lastChange >= (this.tuning.downAfterMs ?? 250)) {
        this.maxLayer--;
        this.lastChange = now;
      }
    } else if (q < low) {
      this.healthySince ??= now;
      if (
        this.maxLayer < MAX_LAYER &&
        now - this.healthySince >= (this.tuning.probeAfterMs ?? 2000) &&
        now - this.lastChange >= (this.tuning.probeAfterMs ?? 2000)
      ) {
        this.maxLayer++;
        this.lastChange = now;
        this.healthySince = now;
      }
    }
    if (this.maxLayer < MAX_LAYER || q > high) this.degradedSince ??= now;
    else this.degradedSince = null;
  }
}
