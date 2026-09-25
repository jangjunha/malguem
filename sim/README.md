# Relay-broadcast simulator

Runs the real relay engine (`client/src/lib/relay`) in one headless Chromium
per simulated participant, over real WebRTC data channels, with every
participant behind a modelled home connection. Used to test the relay tree
before asking friends to try it. Results and findings are in
[docs/experiments/relay-broadcast.md](../docs/experiments/relay-broadcast.md).

```
┌─────────── netns mg-streamer ───────────┐      ┌──── netns mg-minji ────┐
│ Chromium ── page/main.ts ── RelayHub     │      │ Chromium ── RelayHub   │ …
│        10.77.0.2 (TUN mgt0)              │      │  10.77.0.3 (TUN mgt1)  │
└──────────────────┬───────────────────────┘      └───────────┬────────────┘
                   │  every IP packet                          │
                   ▼                                           ▼
        netem/emulator.py: uplink queue+rate → loss → propagation+jitter
                           → downlink queue+rate → loss → deliver
                   ▲
harness.ts: starts emulator + browsers, relays signaling (stand-in for
malguem-server), plays the scenario timeline, writes results/<scenario>/
```

## Why a userspace emulator

The VM kernels this runs on (cloud sandboxes, Firecracker) often lack
`sch_netem`, so `tc netem` delay/loss isn't available. `netem/emulator.py`
owns one TUN device per network namespace and models each participant's
access link itself: FIFO uplink/downlink queues with serialization delay and
tail drop (so bufferbloat and congestion emerge naturally), propagation delay
by region, non-reordering wireless jitter, and Gilbert–Elliott bursty loss.
Sanity check on an idle host: configured 20 ms RTT measured 20.8–22 ms,
a 20 Mb/s uplink measured 20.0 Mb/s.

## Running

Linux, root (network namespaces + TUN), Python 3, Node ≥ 22.6, and a Chromium
(defaults to the Playwright build under `/opt/pw-browsers`; override with
`CHROME_PATH`).

```sh
cd sim
npm install
npm run build                                   # bundle page/main.ts
sudo node --experimental-strip-types harness.ts               # every scenario
sudo node --experimental-strip-types harness.ts tree-busy star-busy
SIM_DURATION=20 SIM_REPEAT=3 sudo -E node --experimental-strip-types harness.ts tree-busy
```

Each run writes `results/<scenario>/`:

- `summary.json`: per-viewer fps, share of time at full quality, latency
  (capture → frame complete), freezes, keyframe requests, per-node upload
  (measured by the emulator), final tree, capacity estimates, and the impact of
  each timeline event
- `stats.jsonl`: 1 Hz snapshot from every page (hub state, frame stats)
- `emu.jsonl`: 1 Hz ground truth from the emulator (per-node up/down rate,
  queue drops, max queueing), plus host CPU
- `events.jsonl`: scenario events, planner log, freezes, connection states

Keep the host otherwise idle while it runs: 8 browsers share the CPU, and
heavy background work (builds, type checks) starves the emulator and the
pages, which shows up as stalls that aren't the engine's fault.

## Scenarios

Defined in `scenarios.ts` along with the link profiles (Korean home
connections: 기가/500M/100M FTTH, VDSL, weak Wi-Fi, phone tethering, and a
streamer whose uplink is already busy). The friend group is one streamer and
seven viewers. `source: 'codec'` runs real WebCodecs encode/decode instead of
sized synthetic frames.
