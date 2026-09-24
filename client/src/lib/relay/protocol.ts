/**
 * Wire format for relayed broadcast media.
 *
 * The broadcaster encodes each frame exactly once. The encoded bytes are split
 * into chunks and sent over an RTCDataChannel; relays forward the very same
 * chunks to their children without decoding (cut-through, chunk by chunk), so
 * a relay costs a memcpy, not an encoder session.
 *
 * Every chunk carries the full frame header so any chunk can be routed and a
 * frame can be reassembled from chunks arriving out of order. Header layout
 * (little endian, HEADER_BYTES long):
 *
 *   0  u8   magic (0x4d)
 *   1  u8   flags: bit0 keyframe, bit1 audio, bits 2-3 temporal layer id
 *   2  u16  chunk index
 *   4  u16  chunk count
 *   6  u8   hops travelled (incremented in place by each relay)
 *   7  u8   reserved
 *   8  u32  stream id (random per broadcast session)
 *   12 u32  frame sequence number (per stream, video and audio share it)
 *   16 u32  sequence number of the frame this one depends on (self = keyframe/independent)
 *   20 u32  total encoded frame size in bytes
 *   24 f64  capture time, ms since epoch (glass-to-glass latency + A/V sync)
 *   32 f64  media timestamp, µs (EncodedVideoChunk/EncodedAudioChunk timestamp)
 */

export const MAGIC = 0x4d;
export const HEADER_BYTES = 40;
/** Payload bytes per chunk. Small enough that a relay forwards early, large
 * enough that per-message overhead stays negligible. */
export const CHUNK_PAYLOAD = 16 * 1024;

export interface FrameHeader {
  key: boolean;
  audio: boolean;
  /** Temporal layer id (0 = base). Relays drop higher layers under congestion. */
  tl: number;
  chunkIdx: number;
  chunkCount: number;
  hops: number;
  streamId: number;
  seq: number;
  depSeq: number;
  frameBytes: number;
  captureTs: number;
  mediaTs: number;
}

export interface EncodedFrame {
  key: boolean;
  audio: boolean;
  tl: number;
  seq: number;
  depSeq: number;
  captureTs: number;
  mediaTs: number;
  data: Uint8Array;
}

export function writeHeader(view: DataView, h: FrameHeader): void {
  view.setUint8(0, MAGIC);
  view.setUint8(1, (h.key ? 1 : 0) | (h.audio ? 2 : 0) | ((h.tl & 3) << 2));
  view.setUint16(2, h.chunkIdx, true);
  view.setUint16(4, h.chunkCount, true);
  view.setUint8(6, h.hops);
  view.setUint8(7, 0);
  view.setUint32(8, h.streamId, true);
  view.setUint32(12, h.seq, true);
  view.setUint32(16, h.depSeq, true);
  view.setUint32(20, h.frameBytes, true);
  view.setFloat64(24, h.captureTs, true);
  view.setFloat64(32, h.mediaTs, true);
}

export function readHeader(buf: ArrayBuffer): FrameHeader | null {
  if (buf.byteLength < HEADER_BYTES) return null;
  const v = new DataView(buf);
  if (v.getUint8(0) !== MAGIC) return null;
  const flags = v.getUint8(1);
  return {
    key: (flags & 1) !== 0,
    audio: (flags & 2) !== 0,
    tl: (flags >> 2) & 3,
    chunkIdx: v.getUint16(2, true),
    chunkCount: v.getUint16(4, true),
    hops: v.getUint8(6),
    streamId: v.getUint32(8, true),
    seq: v.getUint32(12, true),
    depSeq: v.getUint32(16, true),
    frameBytes: v.getUint32(20, true),
    captureTs: v.getFloat64(24, true),
    mediaTs: v.getFloat64(32, true),
  };
}

/** Bump the hop counter of a received chunk before forwarding it. */
export function incrementHops(buf: ArrayBuffer): void {
  const v = new DataView(buf);
  v.setUint8(6, Math.min(255, v.getUint8(6) + 1));
}

/** Split an encoded frame into self-describing chunks. */
export function chunkFrame(streamId: number, f: EncodedFrame, chunkPayload = CHUNK_PAYLOAD): ArrayBuffer[] {
  const count = Math.max(1, Math.ceil(f.data.byteLength / chunkPayload));
  if (count > 0xffff) throw new Error('frame too large');
  const out: ArrayBuffer[] = [];
  for (let i = 0; i < count; i++) {
    const start = i * chunkPayload;
    const end = Math.min(f.data.byteLength, start + chunkPayload);
    const buf = new ArrayBuffer(HEADER_BYTES + (end - start));
    writeHeader(new DataView(buf), {
      key: f.key,
      audio: f.audio,
      tl: f.tl,
      chunkIdx: i,
      chunkCount: count,
      hops: 0,
      streamId,
      seq: f.seq,
      depSeq: f.depSeq,
      frameBytes: f.data.byteLength,
      captureTs: f.captureTs,
      mediaTs: f.mediaTs,
    });
    new Uint8Array(buf, HEADER_BYTES).set(f.data.subarray(start, end));
    out.push(buf);
  }
  return out;
}

/**
 * Temporal-layer dependency tracking for an L1T3-style stream: a frame on
 * layer k references the most recent frame on a layer below k (T0 references
 * the previous T0). Keyframes and audio are independent. This holds for the
 * VP8/VP9/AV1/H.264 L1T3 structures WebCodecs produces, without having to
 * know where in the pattern a frame sits.
 */
export class LayerDeps {
  private lastT0: number | null = null;
  /** lastBelow[k] = most recent frame on a layer below k. */
  private lastBelow: (number | null)[] = [null, null, null, null];

  /** Returns depSeq for the next frame (seq itself when independent). */
  next(seq: number, tl: number, key: boolean): number {
    if (key) {
      this.lastT0 = seq;
      this.lastBelow = [null, seq, seq, seq];
      return seq;
    }
    const dep = tl === 0 ? this.lastT0 : this.lastBelow[tl];
    if (tl === 0) this.lastT0 = seq;
    // Frames on higher layers may now reference this one.
    for (let k = tl + 1; k < 4; k++) this.lastBelow[k] = seq;
    return dep ?? seq;
  }
}
