/**
 * Uplink queueing from one-way-delay trends.
 *
 * Every participant pings every other one twice a second with its own clock's
 * send time. The receiver's `recv - sent` includes an unknown but constant
 * clock offset, so subtracting the minimum seen recently leaves only the
 * *extra* delay: queueing on the sender's uplink plus on the receiver's
 * downlink. Taking the median of that over all receivers of one sender (done
 * by the planner) isolates the sender's uplink queue — the thing a home
 * router's bufferbloat produces when a relay is asked for more than its
 * uplink can carry — from any single receiver's slow downlink.
 */

const BUCKET_MS = 5000;
const BUCKETS = 6; // minimum over the last ~30 s
const RECENT = 4; // median of the last 4 samples (2 s)

interface Track {
  buckets: { start: number; min: number }[];
  recent: number[];
}

export class OwdTracker {
  private tracks = new Map<string, Track>();

  sample(from: string, sentTs: number, recvTs: number): void {
    const rel = recvTs - sentTs;
    let tr = this.tracks.get(from);
    if (!tr) {
      tr = { buckets: [], recent: [] };
      this.tracks.set(from, tr);
    }
    const last = tr.buckets.at(-1);
    if (!last || recvTs - last.start >= BUCKET_MS) {
      tr.buckets.push({ start: recvTs, min: rel });
      if (tr.buckets.length > BUCKETS) tr.buckets.shift();
    } else {
      last.min = Math.min(last.min, rel);
    }
    tr.recent.push(rel);
    if (tr.recent.length > RECENT) tr.recent.shift();
  }

  /** Current queueing delay on the path from `from`, ms (null = no data yet). */
  queueMs(from: string): number | null {
    const tr = this.tracks.get(from);
    if (!tr || tr.recent.length < 2) return null;
    const base = Math.min(...tr.buckets.map((b) => b.min));
    const sorted = [...tr.recent].sort((a, b) => a - b);
    const med = sorted[Math.floor(sorted.length / 2)]!;
    return Math.max(0, med - base);
  }

  all(): Record<string, number> {
    const out: Record<string, number> = {};
    for (const id of this.tracks.keys()) {
      const q = this.queueMs(id);
      if (q !== null) out[id] = Math.round(q);
    }
    return out;
  }

  forget(from: string): void {
    this.tracks.delete(from);
  }
}

export function median(xs: number[]): number | null {
  if (xs.length === 0) return null;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.floor(s.length / 2)]!;
}
