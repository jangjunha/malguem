import { describe, expect, it } from 'vitest';
import { ChildLink, type SendChannel } from './forwarder';
import { planTree, slotsFor, edgeKey, type PlanNode } from './planner';
import { chunkFrame, incrementHops, LayerDeps, readHeader, type EncodedFrame } from './protocol';
import { FrameReceiver, type DeliveredFrame } from './receiver';

function frame(seq: number, over: Partial<EncodedFrame> = {}): EncodedFrame {
  return {
    key: false,
    audio: false,
    tl: 0,
    seq,
    depSeq: seq,
    captureTs: 1000 + seq,
    mediaTs: seq * 16667,
    data: new Uint8Array(100).fill(seq & 0xff),
    ...over,
  };
}

describe('protocol', () => {
  it('round-trips headers and splits into chunks', () => {
    const data = new Uint8Array(40_000).map((_, i) => i & 0xff);
    const chunks = chunkFrame(7, frame(3, { key: true, tl: 1, depSeq: 3, data }), 16 * 1024);
    expect(chunks).toHaveLength(3);
    const h = readHeader(chunks[2]!)!;
    expect(h).toMatchObject({ key: true, tl: 1, streamId: 7, seq: 3, chunkIdx: 2, chunkCount: 3, frameBytes: 40_000, hops: 0 });
    incrementHops(chunks[2]!);
    expect(readHeader(chunks[2]!)!.hops).toBe(1);
  });

  it('derives L1T3 dependencies', () => {
    const d = new LayerDeps();
    const pattern = [0, 2, 1, 2];
    const deps: number[] = [];
    for (let seq = 0; seq < 9; seq++) deps.push(d.next(seq, pattern[seq % 4]!, seq === 0));
    //            key T2 T1 T2 T0 T2 T1 T2 T0
    expect(deps).toEqual([0, 0, 0, 2, 0, 4, 4, 6, 4]);
  });
});

describe('planner', () => {
  const node = (id: string, cap: number, rtt: Record<string, number> = {}): PlanNode => ({ id, capacityKbps: cap, rttMs: rtt });

  it('is a star when the root has enough slots', () => {
    const p = planTree({ root: 'b', rootCapacityKbps: 100_000, bitrateKbps: 8000, viewers: [node('x', 0), node('y', 0), node('z', 0)] });
    expect([...p.parent.values()]).toEqual(['b', 'b', 'b']);
    expect(p.degraded.size).toBe(0);
  });

  it('uses relay-capable viewers when the root is constrained', () => {
    // Root can feed 2 at 8 Mbps; a and c can feed 4 each.
    const viewers = [node('a', 50_000), node('l1', 0), node('l2', 0), node('c', 50_000), node('l3', 0), node('l4', 0)];
    const p = planTree({ root: 'b', rootCapacityKbps: 20_000, bitrateKbps: 8000, viewers });
    expect(p.parent.get('a')).toBe('b');
    expect(p.parent.get('c')).toBe('b');
    for (const l of ['l1', 'l2', 'l3', 'l4']) expect(['a', 'c']).toContain(p.parent.get(l));
    expect(p.degraded.size).toBe(0);
    expect(Math.max(...p.depth.values())).toBe(2);
  });

  it('marks viewers degraded when total capacity is short', () => {
    const p = planTree({ root: 'b', rootCapacityKbps: 10_000, bitrateKbps: 8000, viewers: [node('x', 0), node('y', 0)] });
    expect(p.degraded.size).toBe(1);
  });

  it('never overcommits a node that cannot relay', () => {
    const p = planTree({ root: 'b', rootCapacityKbps: 10_000, bitrateKbps: 8000, viewers: [node('x', 0), node('y', 0), node('z', 0)] });
    expect([...p.parent.values()]).toEqual(['b', 'b', 'b']);
    expect(p.degraded.size).toBe(2);
  });

  it('ignores a ban rather than leave a viewer without a parent', () => {
    const p = planTree({
      root: 'b',
      rootCapacityKbps: 20_000,
      bitrateKbps: 8000,
      viewers: [node('v', 0)],
      forbidden: new Set([edgeKey('b', 'v')]),
    });
    expect(p.parent.get('v')).toBe('b');
  });

  it('respects forbidden edges and noRelay', () => {
    const viewers = [node('a', 80_000), node('c', 80_000), node('l', 0)];
    const p = planTree({
      root: 'b',
      rootCapacityKbps: 10_000,
      bitrateKbps: 8000,
      viewers,
      noRelay: new Set(['a']),
      forbidden: new Set([edgeKey('b', 'l')]),
    });
    expect(p.children.get('a')).toEqual([]);
    expect(p.parent.get('l')).toBe('c');
  });

  it('prefers lower-latency parents and keeps the current parent on ties', () => {
    const viewers = [
      node('a', 80_000, { b: 10 }),
      node('c', 80_000, { b: 10 }),
      node('l', 0, { a: 40, c: 6, b: 10 }),
    ];
    const base = { root: 'b', rootCapacityKbps: 24_000, bitrateKbps: 8000, viewers };
    expect(planTree(base).parent.get('l')).toBe('c');
    // a is only slightly worse than c here; stickiness keeps the old choice.
    const close = viewers.map((v) => (v.id === 'l' ? node('l', 0, { a: 10, c: 6, b: 10 }) : v));
    expect(planTree({ ...base, viewers: close, prevParent: new Map([['l', 'a']]) }).parent.get('l')).toBe('a');
  });

  it('computes slots with headroom', () => {
    expect(slotsFor(100_000, 8000)).toBe(10);
    expect(slotsFor(9000, 8000)).toBe(0);
  });
});

describe('receiver', () => {
  function setup() {
    let t = 0;
    const got: DeliveredFrame[] = [];
    let kf = 0;
    const r = new FrameReceiver({ now: () => t, onFrame: (f) => got.push(f), onNeedKeyframe: () => kf++ });
    const feed = (f: EncodedFrame, order?: number[]) => {
      const cs = chunkFrame(1, f, 64);
      for (const i of order ?? cs.map((_, i) => i)) r.push(cs[i]!, readHeader(cs[i]!)!);
    };
    return { r, got, feed, kf: () => kf, advance: (ms: number) => (t += ms) };
  }

  it('waits for a keyframe, then delivers in dependency order', () => {
    const s = setup();
    s.feed(frame(0, { tl: 0 }));
    expect(s.got).toHaveLength(0);
    s.r.tick();
    expect(s.kf()).toBe(1);
    const d = new LayerDeps();
    const pat = [0, 2, 1, 2];
    const frames = Array.from({ length: 8 }, (_, i) => {
      const seq = 10 + i;
      const tl = pat[i % 4]!;
      return frame(seq, { key: i === 0, tl, depSeq: d.next(seq, tl, i === 0), data: new Uint8Array(200).fill(i) });
    });
    // Chunks of frame 11 arrive reversed, frame 13 before 12 (12 is a T1 it needs).
    s.feed(frames[0]!);
    s.feed(frames[1]!, [3, 2, 1, 0]);
    s.feed(frames[3]!);
    s.feed(frames[2]!);
    for (const f of frames.slice(4)) s.feed(f);
    expect(s.got.map((f) => f.seq)).toEqual([10, 11, 12, 13, 14, 15, 16, 17]);
    expect(s.got[1]!.data).toEqual(new Uint8Array(200).fill(1));
  });

  it('skips intentionally dropped T2 frames without waiting', () => {
    const s = setup();
    const d = new LayerDeps();
    const pat = [0, 2, 1, 2];
    for (let i = 0; i < 12; i++) {
      const tl = pat[i % 4]!;
      const f = frame(i, { key: i === 0, tl, depSeq: d.next(i, tl, i === 0) });
      if (tl === 2) continue; // relay shed T2
      s.feed(f);
    }
    expect(s.got.map((f) => f.seq)).toEqual([0, 2, 4, 6, 8, 10]);
    expect(s.kf()).toBe(0);
  });

  it('requests a keyframe when a base-layer frame is lost', () => {
    const s = setup();
    s.feed(frame(0, { key: true }));
    // seq 1 (T0 dep 0) lost; seq 2 (T0 dep 1) arrives.
    s.feed(frame(2, { depSeq: 1 }));
    expect(s.got.map((f) => f.seq)).toEqual([0]);
    s.advance(300);
    s.r.tick();
    expect(s.kf()).toBe(1);
    expect(s.r.waitingForKeyframe).toBe(true);
    s.feed(frame(3, { depSeq: 2 }));
    s.feed(frame(4, { key: true }));
    expect(s.got.map((f) => f.seq)).toEqual([0, 4]);
  });

  it('asks for a keyframe when audio flows but video stopped', () => {
    const s = setup();
    s.feed(frame(0, { key: true }));
    expect(s.kf()).toBe(0);
    s.advance(1200);
    s.feed(frame(0, { audio: true }));
    s.r.tick();
    expect(s.kf()).toBe(1);
  });

  it('ignores duplicate chunks from two parents', () => {
    const s = setup();
    s.feed(frame(0, { key: true }));
    s.feed(frame(0, { key: true }));
    s.feed(frame(1, { depSeq: 0 }));
    expect(s.got.map((f) => f.seq)).toEqual([0, 1]);
    expect(s.r.counters.dupChunks).toBeGreaterThan(0);
  });
});

describe('ChildLink', () => {
  class FakeChannel implements SendChannel {
    bufferedAmount = 0;
    readyState: RTCDataChannelState = 'open';
    sent: ArrayBuffer[] = [];
    send(d: ArrayBuffer) {
      this.sent.push(d);
    }
  }

  it('sheds temporal layers under queueing and probes back', () => {
    const ch = new FakeChannel();
    const link = new ChildLink('c', ch, null, { bitrateKbps: 8000, probeAfterMs: 1000, downAfterMs: 100 });
    const d = new LayerDeps();
    const pat = [0, 2, 1, 2];
    let seq = 0;
    const send = () => {
      const tl = pat[seq % 4]!;
      const f = frame(seq, { key: seq === 0, tl, depSeq: d.next(seq, tl, seq === 0) });
      seq++;
      const c = chunkFrame(1, f)[0]!;
      return link.offer(c, readHeader(c)!);
    };
    for (let i = 0; i < 4; i++) expect(send()).toBe(true);
    // A single burst (keyframe) that drains quickly is not congestion.
    ch.bufferedAmount = 200_000;
    link.adapt(0);
    ch.bufferedAmount = 0;
    link.adapt(100);
    expect(link.maxLayer).toBe(2);
    // A standing queue is.
    ch.bufferedAmount = 200_000; // ~200 ms at 8 Mbps
    for (let t = 200; t <= 1000; t += 100) link.adapt(t);
    expect(link.maxLayer).toBe(0);
    ch.bufferedAmount = 50_000; // below hard limit, above high water
    const results = Array.from({ length: 8 }, send);
    // Only T0 frames (every 4th) pass.
    expect(results).toEqual([true, false, false, false, true, false, false, false]);
    ch.bufferedAmount = 0;
    for (let t = 1100; t <= 2200; t += 100) link.adapt(t);
    expect(link.maxLayer).toBe(1);
    expect(link.degradedSince).not.toBeNull();
  });

  it('flags a broken chain when a base-layer frame must be dropped', () => {
    const ch = new FakeChannel();
    const link = new ChildLink('c', ch, null, { bitrateKbps: 8000 });
    const offer = (f: EncodedFrame) => {
      const c = chunkFrame(1, f)[0]!;
      return link.offer(c, readHeader(c)!);
    };
    expect(offer(frame(0, { key: true }))).toBe(true);
    ch.bufferedAmount = 10_000_000; // way past the hard limit
    expect(offer(frame(1, { depSeq: 0 }))).toBe(false);
    expect(link.chainBroken).toBe(true);
    ch.bufferedAmount = 0;
    // Everything depending on the dropped frame stays dropped until a keyframe.
    expect(offer(frame(2, { depSeq: 1 }))).toBe(false);
    expect(offer(frame(3, { key: true }))).toBe(true);
    expect(link.chainBroken).toBe(false);
  });
});
