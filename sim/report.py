#!/usr/bin/env python3
"""
Aggregate results/<scenario>[@run]/summary.json into Markdown tables.
Repeated runs (name@1, name@2, …) are reported as mean with min–max range.

  python3 report.py > results/tables.md
"""
import json
import os
import re
import statistics
import sys
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
RES = os.path.join(HERE, "results")


def load():
    runs = defaultdict(list)
    for d in sorted(os.listdir(RES)):
        p = os.path.join(RES, d, "summary.json")
        if not os.path.exists(p):
            continue
        name = re.sub(r"@\d+$", "", d)
        with open(p) as f:
            runs[name].append(json.load(f))
    return runs


def fmt(xs, digits=0, unit=""):
    xs = [x for x in xs if x is not None]
    if not xs:
        return "–"
    m = statistics.mean(xs)
    f = f"{{:.{digits}f}}"
    if len(xs) == 1 or max(xs) == min(xs):
        return f.format(m) + unit
    return f"{f.format(m)}{unit} ({f.format(min(xs))}–{f.format(max(xs))})"


def overall(runs, key, digits=0, unit=""):
    return fmt([r["overall"].get(key) for r in runs], digits, unit)


def per_viewer_mean(runs, key, who):
    return [r["perViewer"][who][key] for r in runs if who in r["perViewer"]]


def main():
    runs = load()
    out = sys.stdout
    out.write("### Friend group (synthetic 8 Mb/s 1080p60-sized stream)\n\n")
    out.write("| Scenario | runs | fps (avg) | full quality | latency p50 | worst p95 | freeze total | streamer up | busiest relay up | keyframes |\n")
    out.write("|---|---|---|---|---|---|---|---|---|---|\n")
    for name in [
        "star-busy", "tree-busy", "tree-busy-partial", "tree-busy-reliable", "tree-relay-crash", "tree-relay-leave",
        "tree-overclaim", "tree-uplink-drop", "tree-churn", "tree-relay-games", "tree-streamer-games",
        "star-lossy-all", "tree-lossy-all",
    ]:
        r = runs.get(name)
        if not r:
            continue
        out.write(
            f"| `{name}` | {len(r)} | {overall(r, 'fpsAvg', 1)} | {overall(r, 'fullQualityPct', 0, '%')} "
            f"| {overall(r, 'latP50ms', 0, ' ms')} | {overall(r, 'latWorstP95ms', 0, ' ms')} "
            f"| {fmt([x['overall']['freezeMsTotal'] / 1000 for x in r], 1, ' s')} "
            f"| {overall(r, 'broadcasterUpMbps', 1, ' Mb/s')} | {overall(r, 'maxRelayUpMbps', 1, ' Mb/s')} "
            f"| {overall(r, 'keyframes')} |\n"
        )

    out.write("\n### Per viewer (mean over runs): fps / full-quality share / latency p50\n\n")
    viewers = ["minji", "junho", "seoyeon", "dohyun", "haeun", "jiwoo", "taeyang"]
    out.write("| Scenario | " + " | ".join(viewers) + " |\n|---|" + "---|" * len(viewers) + "\n")
    for name in ["star-busy", "tree-busy", "tree-overclaim", "tree-uplink-drop", "tree-relay-crash", "star-lossy-all", "tree-lossy-all"]:
        r = runs.get(name)
        if not r:
            continue
        cells = []
        for v in viewers:
            fps = per_viewer_mean(r, "fpsAvg", v)
            fq = per_viewer_mean(r, "fullQualityPct", v)
            lat = per_viewer_mean(r, "latP50ms", v)
            if not fps:
                cells.append("–")
                continue
            cells.append(
                f"{statistics.mean(fps):.0f} / {statistics.mean(fq):.0f}% / "
                f"{statistics.mean([x for x in lat if x is not None] or [0]):.0f} ms"
            )
        out.write(f"| `{name}` | " + " | ".join(cells) + " |\n")

    out.write("\n### Mesh vs relay, real codecs (360p30, 1.5 Mb/s, streamer uplink not limiting)\n\n")
    out.write("| Viewers | streamer CPU mesh → relay | streamer upload mesh → relay | g2g latency p50 mesh → relay | worst p95 mesh → relay | fps mesh → relay | busiest relay CPU / upload |\n")
    out.write("|---|---|---|---|---|---|---|\n")
    for n in [2, 4, 6]:
        m, rl = runs.get(f"h2h-mesh-{n}"), runs.get(f"h2h-relay-{n}")
        if not m or not rl:
            continue
        out.write(
            f"| {n} | {overall(m, 'broadcasterCpuPct', 0, '%')} → {overall(rl, 'broadcasterCpuPct', 0, '%')} "
            f"| {overall(m, 'broadcasterUpMbps', 1)} → {overall(rl, 'broadcasterUpMbps', 1, ' Mb/s')} "
            f"| {overall(m, 'latP50ms', 0)} → {overall(rl, 'latP50ms', 0, ' ms')} "
            f"| {overall(m, 'latWorstP95ms', 0)} → {overall(rl, 'latWorstP95ms', 0, ' ms')} "
            f"| {overall(m, 'fpsAvg', 1)} → {overall(rl, 'fpsAvg', 1)} "
            f"| {overall(rl, 'maxRelayCpuPct', 0, '%')} / {overall(rl, 'maxRelayUpMbps', 1, ' Mb/s')} |\n"
        )

    out.write("\n### Single viewer over a lossy path (SCTP transport limit)\n\n")
    out.write("| Residual loss | RTT ≈ 12 ms | RTT ≈ 30 ms |\n|---|---|---|\n")
    for loss in ["0.0", "0.1", "0.3", "0.8"]:
        cells = []
        for rtt in ["12", "30"]:
            r = runs.get(f"loss-{loss}pct-rtt{rtt}")
            cells.append(
                f"{overall(r, 'fpsAvg', 0)} fps, {overall(r, 'fullQualityPct', 0, '%')} full, p50 {overall(r, 'latP50ms', 0, ' ms')}"
                if r
                else "–"
            )
        out.write(f"| {loss} % | " + " | ".join(cells) + " |\n")

    r = runs.get("codec-tree")
    if r:
        out.write("\n### Real codec through relays (`codec-tree`)\n\n")
        s = r[-1]
        out.write("| Viewer | fps | full quality | g2g p50 | parent at end |\n|---|---|---|---|---|\n")
        for v, x in s["perViewer"].items():
            out.write(f"| {v} | {x['fpsAvg']} | {x['fullQualityPct']}% | {x['latP50ms']} ms | {x['parentAtEnd']} |\n")


if __name__ == "__main__":
    main()
