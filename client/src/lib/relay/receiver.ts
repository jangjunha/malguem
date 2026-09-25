/**
 * Reassembles chunks into frames and releases them in a decodable order.
 *
 * Delivery rule ("as soon as decodable"): a complete frame newer than the last
 * delivered one is released once the frame it depends on has been released
 * (or it is independent). Older frames still in flight are then skipped for
 * good. With L1T3 (and plain L1T1) every reference frame is referenced by the
 * very next frame of the stream, so this never skips a frame a later one
 * needs — and intentional drops of higher temporal layers by a relay cost no
 * waiting at all.
 *
 * When the base-layer chain breaks (a lost T0 or keyframe) nothing more is
 * decodable until a keyframe, so the receiver asks for one (rate limited) and
 * drops everything else until it arrives.
 *
 * Video and audio are separate sequence spaces; audio frames are independent.
 */
import type { EncodedFrame, FrameHeader } from './protocol';
import { HEADER_BYTES } from './protocol';

export interface DeliveredFrame extends EncodedFrame {
  hops: number;
  /** When the last chunk arrived (same clock as captureTs). */
  completedAt: number;
}

export interface ReceiverOptions {
  now: () => number;
  /** How long a frame may wait for a missing reference before we give up. */
  lossTimeoutMs?: number;
  /** Minimum spacing between keyframe requests. */
  keyframeRetryMs?: number;
  /** No video for this long while audio flows → request a keyframe. */
  videoStallMs?: number;
  onFrame: (f: DeliveredFrame) => void;
  onNeedKeyframe: () => void;
}

interface Assembly {
  h: FrameHeader;
  parts: (Uint8Array | undefined)[];
  got: number;
  bytes: number;
  firstAt: number;
}

interface Complete {
  f: DeliveredFrame;
  at: number;
}

class Track {
  pending = new Map<number, Assembly>();
  complete = new Map<number, Complete>();
  lastDelivered = -1;
  delivered = new Set<number>();
  waitingKey: boolean;
  constructor(readonly video: boolean) {
    this.waitingKey = video;
  }
}

export class FrameReceiver {
  private video = new Track(true);
  private audio = new Track(false);
  private lastKfRequest = -Infinity;
  private lastVideoAt: number;
  private lastAudioAt = -Infinity;
  /** Counters for stats; callers may reset them. */
  counters = { delivered: 0, skipped: 0, dupChunks: 0, keyframeRequests: 0, brokenChains: 0 };

  constructor(private opts: ReceiverOptions) {
    this.lastVideoAt = opts.now();
  }

  /** Feed one chunk (header already parsed from `buf`). */
  push(buf: ArrayBuffer, h: FrameHeader): void {
    const tr = h.audio ? this.audio : this.video;
    if (h.seq <= tr.lastDelivered) {
      this.counters.dupChunks++;
      return;
    }
    let a = tr.pending.get(h.seq);
    if (!a) {
      if (tr.complete.has(h.seq)) {
        this.counters.dupChunks++;
        return;
      }
      a = { h, parts: new Array(h.chunkCount), got: 0, bytes: 0, firstAt: this.opts.now() };
      tr.pending.set(h.seq, a);
    }
    if (a.parts[h.chunkIdx]) {
      this.counters.dupChunks++;
      return;
    }
    const part = new Uint8Array(buf, HEADER_BYTES);
    a.parts[h.chunkIdx] = part;
    a.got++;
    a.bytes += part.byteLength;
    // Track the smallest hop count seen: duplicates can arrive via two parents.
    a.h.hops = Math.min(a.h.hops, h.hops);
    if (a.got < a.h.chunkCount) return;

    tr.pending.delete(h.seq);
    const data = new Uint8Array(a.bytes);
    let off = 0;
    for (const p of a.parts) {
      data.set(p!, off);
      off += p!.byteLength;
    }
    const now = this.opts.now();
    tr.complete.set(h.seq, {
      at: now,
      f: {
        key: a.h.key,
        audio: a.h.audio,
        tl: a.h.tl,
        seq: a.h.seq,
        depSeq: a.h.depSeq,
        captureTs: a.h.captureTs,
        mediaTs: a.h.mediaTs,
        data,
        hops: a.h.hops,
        completedAt: now,
      },
    });
    this.drain(tr);
  }

  /** Call periodically (≈ every 50–100 ms) to time out missing references. */
  tick(): void {
    this.drain(this.video);
    this.drain(this.audio);
    const now = this.opts.now();
    // Audio keeps arriving but video stopped: our feed's reference chain was
    // cut upstream without us seeing a broken frame. Ask for a keyframe.
    if (now - this.lastVideoAt > (this.opts.videoStallMs ?? 1000) && now - this.lastAudioAt < 500) {
      this.requestKeyframe();
    }
    for (const tr of [this.video, this.audio]) {
      for (const [seq, a] of tr.pending) {
        if (seq <= tr.lastDelivered || now - a.firstAt > 2000) tr.pending.delete(seq);
      }
    }
  }

  /** True while no keyframe has arrived yet since join/break. */
  get waitingForKeyframe(): boolean {
    return this.video.waitingKey;
  }

  private drain(tr: Track): void {
    const lossTimeout = this.opts.lossTimeoutMs ?? 250;
    const now = this.opts.now();
    for (;;) {
      const seqs = [...tr.complete.keys()].sort((a, b) => a - b);
      let progressed = false;
      for (const seq of seqs) {
        const c = tr.complete.get(seq)!;
        const f = c.f;
        if (tr.waitingKey) {
          if (!f.key) {
            // Undecodable until a keyframe; discard and (re)ask for one.
            tr.complete.delete(seq);
            this.counters.skipped++;
            this.requestKeyframe();
            continue;
          }
          tr.waitingKey = false;
        }
        const independent = f.depSeq === f.seq;
        if (independent || tr.delivered.has(f.depSeq)) {
          this.deliver(tr, f);
          progressed = true;
          break; // re-scan: older ones are now stale
        }
        // Missing reference. Is it gone for good?
        const refLost = f.depSeq <= tr.lastDelivered || now - c.at > lossTimeout;
        if (!refLost) continue;
        tr.complete.delete(seq);
        this.counters.skipped++;
        if (tr.video && f.tl === 0) {
          // Base-layer chain broken: nothing decodes until a keyframe.
          this.counters.brokenChains++;
          tr.waitingKey = true;
          this.requestKeyframe();
        }
      }
      if (!progressed) return;
    }
  }

  private deliver(tr: Track, f: DeliveredFrame): void {
    for (const s of tr.complete.keys()) {
      if (s < f.seq) {
        tr.complete.delete(s);
        this.counters.skipped++;
      }
    }
    tr.complete.delete(f.seq);
    tr.lastDelivered = f.seq;
    if (f.audio) this.lastAudioAt = this.opts.now();
    else this.lastVideoAt = this.opts.now();
    tr.delivered.add(f.seq);
    if (tr.delivered.size > 512) {
      const cut = f.seq - 256;
      for (const s of tr.delivered) if (s < cut) tr.delivered.delete(s);
    }
    this.counters.delivered++;
    this.opts.onFrame(f);
  }

  private requestKeyframe(): void {
    const now = this.opts.now();
    if (now - this.lastKfRequest < (this.opts.keyframeRetryMs ?? 400)) return;
    this.lastKfRequest = now;
    this.counters.keyframeRequests++;
    this.opts.onNeedKeyframe();
  }
}
