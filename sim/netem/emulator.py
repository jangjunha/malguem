#!/usr/bin/env python3
"""
Userspace access-link network emulator for the relay-broadcast simulation.

Why userspace: the kernels we run on (e.g. Firecracker CI/cloud VMs) often
ship without sch_netem, so `tc netem` delay/loss is unavailable. Instead each
simulated peer lives in its own network namespace whose only route is a TUN
device owned by this process. Every IP packet any peer sends comes here, is
pushed through a model of the sender's home uplink, the path between the two
homes, and the receiver's home downlink, and is then written into the
receiver's TUN.

Per-node model (all directional, all optional):

  sender ──[uplink queue: rate, tail-drop at queue_ms]──[loss (Gilbert-Elliott)]
         ──[propagation: access_ms(src) + backbone(region,region) + access_ms(dst)]
         ──[wireless jitter (src/dst), never reorders a path]
         ──[downlink queue: rate, tail-drop]──[loss]── receiver

The queue model is a real FIFO with serialization delay, so bufferbloat,
congestion-induced delay and tail drop emerge naturally; SCTP/DTLS in the
browsers react to them exactly as they would on a real path.

Protocol (JSON lines):
  stdin  <- {"cmd": "set", "node": "a", "up_kbps": 20000, ...}   change a link live
            {"cmd": "down", "node": "a"}  /  {"cmd": "up", "node": "a"}   blackhole
            {"cmd": "quit"}
  stdout -> {"event": "ready", "nodes": {...name: ip...}}
            {"event": "stats", "t": <s>, "nodes": {name: {...}}}   once per second

Must run as root (or inside a user namespace with CAP_NET_ADMIN).
"""
import argparse
import fcntl
import heapq
import json
import os
import random
import selectors
import signal
import struct
import subprocess
import sys
import time

TUNSETIFF = 0x400454CA
IFF_TUN = 0x0001
IFF_NO_PI = 0x1000
L2_OVERHEAD = 38  # Ethernet framing + preamble/IFG, counted against link rate


def sh(*args: str) -> None:
    subprocess.run(args, check=True)


class Loss:
    """Gilbert-Elliott loss: average rate `p`, mean burst length `burst` packets."""

    def __init__(self, p: float, burst: float, rng: random.Random):
        self.rng = rng
        self.bad = False
        self.set(p, burst)

    def set(self, p: float, burst: float) -> None:
        self.p = max(0.0, min(p, 0.5))
        burst = max(1.0, burst)
        self.p_bg = 1.0 / burst
        self.p_gb = self.p * self.p_bg / (1.0 - self.p) if self.p > 0 else 0.0

    def drop(self) -> bool:
        if self.p <= 0:
            return False
        if self.bad:
            if self.rng.random() < self.p_bg:
                self.bad = False
        elif self.rng.random() < self.p_gb:
            self.bad = True
        return self.bad


class Node:
    FIELDS = ("up_kbps", "down_kbps", "access_ms", "jitter_ms", "loss", "loss_burst", "queue_ms", "region")

    def __init__(self, name: str, index: int, cfg: dict, rng: random.Random):
        self.name = name
        self.ns = f"mg-{name}"
        self.dev = f"mgt{index}"
        self.ip = f"10.77.0.{index + 2}"
        self.up_kbps = 0.0
        self.down_kbps = 0.0
        self.access_ms = 2.0
        self.jitter_ms = 0.0
        self.loss = 0.0
        self.loss_burst = 1.0
        self.queue_ms = 150.0
        self.region = "seoul"
        self.alive = True
        self.up_free = 0  # ns timestamp the uplink finishes its backlog
        self.down_free = 0
        self.up_loss = Loss(0, 1, rng)
        self.down_loss = Loss(0, 1, rng)
        self.fd = -1
        self.reset_counters()
        self.apply(cfg)

    def apply(self, cfg: dict) -> None:
        for k in self.FIELDS:
            if k in cfg:
                setattr(self, k, cfg[k] if k == "region" else float(cfg[k]))
        # Wireless loss happens on the radio hop in both directions.
        self.up_loss.set(self.loss, self.loss_burst)
        self.down_loss.set(self.loss, self.loss_burst)

    def reset_counters(self) -> None:
        self.c_up = 0
        self.c_down = 0
        self.c_qdrop_up = 0
        self.c_qdrop_down = 0
        self.c_loss = 0
        self.max_up_backlog = 0.0
        self.max_down_backlog = 0.0


class Emulator:
    def __init__(self, cfg: dict, seed: int):
        self.rng = random.Random(seed)
        self.backbone = cfg.get("backbone_ms", {})
        self.nodes: dict[str, Node] = {}
        for i, (name, ncfg) in enumerate(cfg["nodes"].items()):
            self.nodes[name] = Node(name, i, ncfg, self.rng)
        self.by_ip = {socket_ip(n.ip): n for n in self.nodes.values()}
        self.heap: list = []
        self.seq = 0
        self.path_last: dict[tuple[str, str], int] = {}
        self.sel = selectors.DefaultSelector()

    # ---------- setup / teardown ----------

    def setup(self) -> None:
        for n in self.nodes.values():
            subprocess.run(["ip", "netns", "del", n.ns], stderr=subprocess.DEVNULL)
            sh("ip", "netns", "add", n.ns)
            fd = os.open("/dev/net/tun", os.O_RDWR | os.O_NONBLOCK)
            fcntl.ioctl(fd, TUNSETIFF, struct.pack("16sH", n.dev.encode(), IFF_TUN | IFF_NO_PI))
            n.fd = fd
            sh("ip", "link", "set", n.dev, "netns", n.ns)
            sh("ip", "-n", n.ns, "link", "set", "lo", "up")
            sh("ip", "-n", n.ns, "addr", "add", f"{n.ip}/16", "dev", n.dev)
            sh("ip", "-n", n.ns, "link", "set", n.dev, "mtu", "1500", "up")
            # Chromium picks WebRTC host candidates from the default-route
            # interface (without a camera/mic grant), so there must be one.
            sh("ip", "-n", n.ns, "route", "add", "default", "dev", n.dev)
            self.sel.register(fd, selectors.EVENT_READ, n)

    def teardown(self) -> None:
        for n in self.nodes.values():
            try:
                os.close(n.fd)
            except OSError:
                pass
            subprocess.run(["ip", "netns", "del", n.ns], stderr=subprocess.DEVNULL)

    # ---------- model ----------

    def backbone_ms(self, a: Node, b: Node) -> float:
        if a.region == b.region:
            return float(self.backbone.get(a.region, self.backbone.get("default_same", 1.0)))
        key1, key2 = f"{a.region}-{b.region}", f"{b.region}-{a.region}"
        return float(self.backbone.get(key1, self.backbone.get(key2, self.backbone.get("default", 5.0))))

    def jitter_ns(self, n: Node) -> int:
        if n.jitter_ms <= 0:
            return 0
        # Wi-Fi style: mostly small, occasionally a long retry/aggregation stall.
        return int(min(self.rng.expovariate(1.0 / n.jitter_ms), n.jitter_ms * 8) * 1e6)

    def push(self, t: int, kind: int, node: Node, pkt: bytes) -> None:
        self.seq += 1
        heapq.heappush(self.heap, (t, self.seq, kind, node, pkt))

    def on_send(self, src: Node, pkt: bytes, now: int) -> None:
        if len(pkt) < 20 or (pkt[0] >> 4) != 4:
            return
        dst = self.by_ip.get(pkt[16:20])
        if dst is None or dst is src or not src.alive or not dst.alive:
            return
        size = len(pkt) + L2_OVERHEAD
        # Sender uplink FIFO.
        start = max(now, src.up_free)
        backlog_ms = (start - now) / 1e6
        if backlog_ms > src.queue_ms:
            src.c_qdrop_up += 1
            return
        src.max_up_backlog = max(src.max_up_backlog, backlog_ms)
        tx = int(size * 8e9 / (src.up_kbps * 1000)) if src.up_kbps > 0 else 0
        src.up_free = start + tx
        src.c_up += size
        if src.up_loss.drop():
            src.c_loss += 1
            return
        prop_ms = src.access_ms + self.backbone_ms(src, dst) + dst.access_ms
        arrive = src.up_free + int(prop_ms * 1e6) + self.jitter_ns(src) + self.jitter_ns(dst)
        # Jitter never reorders a single path (wireless retries are in-order).
        key = (src.name, dst.name)
        arrive = max(arrive, self.path_last.get(key, 0))
        self.path_last[key] = arrive
        self.push(arrive, 0, dst, pkt)

    def on_arrive(self, dst: Node, pkt: bytes, now: int) -> None:
        if not dst.alive:
            return
        size = len(pkt) + L2_OVERHEAD
        start = max(now, dst.down_free)
        backlog_ms = (start - now) / 1e6
        if backlog_ms > dst.queue_ms:
            dst.c_qdrop_down += 1
            return
        dst.max_down_backlog = max(dst.max_down_backlog, backlog_ms)
        tx = int(size * 8e9 / (dst.down_kbps * 1000)) if dst.down_kbps > 0 else 0
        dst.down_free = start + tx
        if dst.down_loss.drop():
            dst.c_loss += 1
            return
        self.push(dst.down_free, 1, dst, pkt)

    def deliver(self, dst: Node, pkt: bytes) -> None:
        if not dst.alive:
            return
        try:
            os.write(dst.fd, pkt)
            dst.c_down += len(pkt) + L2_OVERHEAD
        except OSError:
            pass

    # ---------- control ----------

    def on_command(self, line: str) -> bool:
        try:
            cmd = json.loads(line)
        except json.JSONDecodeError:
            return True
        c = cmd.get("cmd")
        if c == "quit":
            return False
        node = self.nodes.get(cmd.get("node", ""))
        if node is None:
            return True
        if c == "set":
            node.apply(cmd)
        elif c == "down":
            node.alive = False
        elif c == "up":
            node.alive = True
        return True

    def emit(self, obj: dict) -> None:
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()

    def stats(self, t0: int, now: int, dt: float) -> None:
        out = {}
        for n in self.nodes.values():
            out[n.name] = {
                "up_kbps": round(n.c_up * 8 / 1000 / dt, 1),
                "down_kbps": round(n.c_down * 8 / 1000 / dt, 1),
                "qdrop_up": n.c_qdrop_up,
                "qdrop_down": n.c_qdrop_down,
                "loss": n.c_loss,
                "max_up_backlog_ms": round(n.max_up_backlog, 1),
                "max_down_backlog_ms": round(n.max_down_backlog, 1),
            }
            n.reset_counters()
        self.emit({"event": "stats", "t": round((now - t0) / 1e9, 3), "nodes": out})

    # ---------- loop ----------

    def run(self) -> None:
        try:
            os.set_blocking(sys.stdin.fileno(), False)
            self.sel.register(sys.stdin.fileno(), selectors.EVENT_READ, None)
        except (PermissionError, ValueError):
            pass  # stdin is a regular file / /dev/null: no live control
        self.emit({"event": "ready", "nodes": {n.name: {"ip": n.ip, "ns": n.ns} for n in self.nodes.values()}})
        t0 = time.monotonic_ns()
        next_stats = t0 + 1_000_000_000
        last_stats = t0
        stdin_buf = ""
        while True:
            now = time.monotonic_ns()
            # Release everything due (1 ms slack keeps us from spinning on tiny waits).
            while self.heap and self.heap[0][0] <= now + 200_000:
                t_ev, _, kind, node, pkt = heapq.heappop(self.heap)
                if kind == 0:
                    self.on_arrive(node, pkt, t_ev)
                else:
                    self.deliver(node, pkt)
            if now >= next_stats:
                self.stats(t0, now, (now - last_stats) / 1e9)
                last_stats = now
                next_stats += 1_000_000_000
            timeout = 0.05
            if self.heap:
                timeout = max(0.0, (self.heap[0][0] - time.monotonic_ns()) / 1e9)
            timeout = min(timeout, max(0.0, (next_stats - now) / 1e9))
            for key, _ in self.sel.select(timeout):
                node = key.data
                if node is None:
                    try:
                        chunk = os.read(sys.stdin.fileno(), 65536).decode()
                    except BlockingIOError:
                        continue
                    if chunk == "":
                        return
                    stdin_buf += chunk
                    while "\n" in stdin_buf:
                        line, stdin_buf = stdin_buf.split("\n", 1)
                        if not self.on_command(line):
                            return
                    continue
                t = time.monotonic_ns()
                for _ in range(256):
                    try:
                        pkt = os.read(node.fd, 65535)
                    except BlockingIOError:
                        break
                    self.on_send(node, pkt, t)


def socket_ip(ip: str) -> bytes:
    return bytes(int(p) for p in ip.split("."))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("config", help="JSON topology file")
    ap.add_argument("--seed", type=int, default=1)
    args = ap.parse_args()
    with open(args.config) as f:
        cfg = json.load(f)
    emu = Emulator(cfg, args.seed)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))  # still tear namespaces down
    try:
        emu.setup()
        emu.run()
    finally:
        emu.teardown()


if __name__ == "__main__":
    main()
