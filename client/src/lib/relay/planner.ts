/**
 * Relay-tree planner. Runs on the broadcaster, which is the only node that
 * sees every viewer's reports, so the tree is always globally consistent.
 *
 * The problem is a degree-constrained minimum-delay spanning tree: every node
 * can feed `slots = floor(capacity * headroom / bitrate)` children, and we want
 * each viewer's path delay from the broadcaster as small as possible. Exact
 * solutions are NP-hard; for a friend group (≲ 20 viewers) this greedy is
 * near-optimal and, more importantly, predictable:
 *
 *   1. Relay-capable viewers (slots ≥ 1) are attached first, most slots first,
 *      because each one placed near the root multiplies the capacity below it.
 *   2. Pure leaves are attached afterwards to whichever parent gives them the
 *      lowest delay.
 *
 * Each attachment picks the parent with a free slot minimising
 * `delay(parent) + rtt(parent, v) / 2 + hopMs`, minus a stickiness bonus for
 * the viewer's current parent so the tree does not flap on small RTT changes.
 * If no parent has a free slot, the viewer goes to the relay-capable parent
 * that is least overcommitted and is marked `degraded` (it will receive
 * fewer temporal layers). Nodes that can't relay at all are never parents.
 */

export interface PlanNode {
  id: string;
  /** Estimated upload capacity available for relaying, kbps. */
  capacityKbps: number;
  /** Measured RTT to other nodes (including the root), ms. */
  rttMs: Record<string, number>;
}

export interface PlanInput {
  root: string;
  rootCapacityKbps: number;
  bitrateKbps: number;
  viewers: PlanNode[];
  /** Current parent of each viewer, for stickiness. */
  prevParent?: Map<string, string>;
  /** Edges (parent→child) that must not be used, e.g. ones that just failed. */
  forbidden?: Set<string>;
  /** Viewers that must not be given children (e.g. known-weak links). */
  noRelay?: Set<string>;
  /** Fraction of capacity usable for the stream. */
  headroom?: number;
  /** Per-hop forwarding cost added on top of propagation, ms. */
  hopMs?: number;
  /** Bonus for keeping the current parent, ms. */
  stickyMs?: number;
  /** Used when an RTT is unknown, ms. */
  defaultRttMs?: number;
  /** Cap on tree depth (root = 0). */
  maxDepth?: number;
  /** Most viewers the root feeds itself when others can (busy broadcaster). */
  rootMaxSlots?: number;
}

export interface Plan {
  parent: Map<string, string>;
  children: Map<string, string[]>;
  depth: Map<string, number>;
  /** Estimated one-way delay from the root, ms. */
  delayMs: Map<string, number>;
  degraded: Set<string>;
}

export const edgeKey = (parent: string, child: string) => `${parent}>${child}`;

export function slotsFor(capacityKbps: number, bitrateKbps: number, headroom = 0.8): number {
  if (bitrateKbps <= 0) return 0;
  return Math.max(0, Math.floor((capacityKbps * headroom) / bitrateKbps));
}

export function planTree(input: PlanInput): Plan {
  const headroom = input.headroom ?? 0.8;
  const hopMs = input.hopMs ?? 3;
  const stickyMs = input.stickyMs ?? 8;
  const defaultRtt = input.defaultRttMs ?? 30;
  const maxDepth = input.maxDepth ?? 6;
  const forbidden = input.forbidden ?? new Set<string>();
  const noRelay = input.noRelay ?? new Set<string>();
  const prev = input.prevParent ?? new Map<string, string>();

  const byId = new Map(input.viewers.map((v) => [v.id, v]));
  const rtt = (a: string, b: string): number => {
    const x = byId.get(a)?.rttMs[b] ?? byId.get(b)?.rttMs[a];
    return x ?? defaultRtt;
  };

  const plan: Plan = {
    parent: new Map(),
    children: new Map([[input.root, []]]),
    depth: new Map([[input.root, 0]]),
    delayMs: new Map([[input.root, 0]]),
    degraded: new Set(),
  };
  // The broadcaster always feeds at least one viewer, whatever its estimate.
  const slots = new Map<string, number>([
    [
      input.root,
      Math.max(1, Math.min(input.rootMaxSlots ?? Infinity, slotsFor(input.rootCapacityKbps, input.bitrateKbps, headroom))),
    ],
  ]);
  const used = new Map<string, number>([[input.root, 0]]);

  const viewerSlots = (v: PlanNode) =>
    noRelay.has(v.id) ? 0 : slotsFor(v.capacityKbps, input.bitrateKbps, headroom);

  // Relays first (more slots first, then nearer the root), then leaves by
  // distance to the root. Ties by id keep the order deterministic.
  const order = [...input.viewers].sort((a, b) => {
    const sa = viewerSlots(a);
    const sb = viewerSlots(b);
    if ((sa > 0) !== (sb > 0)) return sa > 0 ? -1 : 1;
    if (sa !== sb) return sb - sa;
    const d = rtt(input.root, a.id) - rtt(input.root, b.id);
    return d !== 0 ? d : a.id < b.id ? -1 : 1;
  });

  for (const v of order) {
    let best: { p: string; cost: number } | null = null;
    let fallback: { p: string; load: number; cost: number } | null = null;
    // Forbidden edges are a preference ("try elsewhere"), not a reason to
    // leave a viewer with nothing: if every usable parent is forbidden,
    // ignore the ban for this viewer.
    const usable = (p: string) => (slots.get(p) ?? 0) > 0 && plan.depth.get(p)! + 1 <= maxDepth;
    const allBanned = [...plan.depth.keys()].filter(usable).every((p) => forbidden.has(edgeKey(p, v.id)));
    for (const p of plan.depth.keys()) {
      if (!allBanned && forbidden.has(edgeKey(p, v.id))) continue;
      const depth = plan.depth.get(p)!;
      if (depth + 1 > maxDepth) continue;
      let cost = plan.delayMs.get(p)! + rtt(p, v.id) / 2 + hopMs;
      if (prev.get(v.id) === p) cost -= stickyMs;
      const cap = slots.get(p) ?? 0;
      const spare = cap - (used.get(p) ?? 0);
      if (spare > 0) {
        if (!best || cost < best.cost) best = { p, cost };
      } else if (cap > 0) {
        // Overcommit the parent that is least overcommitted relative to what
        // it can carry; never pick a node that can't relay at all.
        const load = (used.get(p)! + 1) / cap;
        if (!fallback || load < fallback.load || (load === fallback.load && cost < fallback.cost)) {
          fallback = { p, load, cost };
        }
      }
    }
    const chosen = best ?? fallback;
    if (!chosen) continue; // only when the depth cap leaves no parent at all
    if (!best) plan.degraded.add(v.id);
    const p = chosen.p;
    plan.parent.set(v.id, p);
    plan.children.get(p)!.push(v.id);
    plan.children.set(v.id, []);
    plan.depth.set(v.id, plan.depth.get(p)! + 1);
    plan.delayMs.set(v.id, plan.delayMs.get(p)! + rtt(p, v.id) / 2 + hopMs);
    used.set(p, (used.get(p) ?? 0) + 1);
    slots.set(v.id, viewerSlots(v));
    used.set(v.id, 0);
  }
  return plan;
}

/** Star topology (everyone fed by the root): the mesh baseline. */
export function planStar(root: string, viewers: PlanNode[]): Plan {
  const plan: Plan = {
    parent: new Map(),
    children: new Map([[root, viewers.map((v) => v.id)]]),
    depth: new Map([[root, 0]]),
    delayMs: new Map([[root, 0]]),
    degraded: new Set(),
  };
  for (const v of viewers) {
    plan.parent.set(v.id, root);
    plan.children.set(v.id, []);
    plan.depth.set(v.id, 1);
    plan.delayMs.set(v.id, (v.rttMs[root] ?? 30) / 2);
  }
  return plan;
}
