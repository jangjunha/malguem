# Experiment: relayed screen broadcast (encode once, viewers pass it on)

Status: simulated, not yet tried with real people. Engine in
`client/src/lib/relay/`, simulator in `sim/`, app integration behind the
"Transport: Relay tree (experimental)" broadcast setting.

## Problem

In the WebRTC mesh the streamer runs one video encoder and one congestion
controller **per viewer**, so both upload and encoder load grow linearly with
the audience. Consumer GPUs only allow a handful of concurrent hardware
encode sessions; beyond that Chromium falls back to software encoding and the
CPU spikes. An SFU would fix it but needs a media server whose bandwidth
someone's home connection pays for, against the project's premise.

## Idea

- **Encode once** with WebCodecs (hardware, L1T3 temporal layers).
- Ship the **encoded bytes** over data channels on the mesh's existing peer
  connections (no new ICE, no server).
- Viewers **forward those bytes unchanged** to other viewers (cut-through,
  chunk by chunk): a relay costs a memcpy, not an encoder.
- When a link can't keep up, the relay **drops the top temporal layer**
  (60 → 30 → 15 fps) instead of re-encoding.
- The streamer plans the tree from what everyone reports.

Viewers are call members allowed to see the stream anyway, so relaying
through them doesn't weaken the E2EE model (each hop is DTLS).

## Engine design (as tested)

| Piece | File | What it does |
|---|---|---|
| Wire format | `protocol.ts` | 40-byte header per 16 KiB chunk: stream, seq, dependency seq, temporal layer, keyframe, capture time, hop count |
| Planner | `planner.ts` | Degree-constrained min-delay tree: relay-capable viewers first (most slots first), then leaves; stickiness; the root always keeps ≥ 1 slot; nodes that can't relay are never parents |
| Forwarder | `forwarder.ts` | Per child: standing-queue congestion signal (min `bufferedAmount` over 500 ms, CoDel-style), sheds/restores temporal layers, flags a broken reference chain |
| Receiver | `receiver.ts` | Reassembly and "deliver as soon as decodable" ordering; keyframe requests on a broken base layer or when video stalls while audio flows |
| Uplink estimator | `owd.ts` | 2 Hz pings between all peers; one-way-delay rise over its 30 s minimum; the median over receivers = that node's uplink queue |
| Hub | `hub.ts` | Channels, control plane, planning, capacity estimates (cut on uplink queueing, stepwise recovery with backoff), make-before-break parent switches, keyframe coalescing (≤ 1/s) |
| Codec glue | `codec.ts` | Encoder choice (AV1/H.264/VP9 with L1T3, HW first), idle re-encode (≥ 4 fps on a static screen), decode to a `MediaStreamTrack` |

Channels per peer pair: `ctl` (reliable, ordered: JSON), `base` (reliable,
unordered: keyframes, T0, audio), `enh` (300 ms lifetime, unordered: T1/T2).

## Simulation method

See `sim/README.md`. In short: one headless Chromium per participant, each in
its own network namespace, with every packet passing through a userspace
access-link emulator (uplink/downlink FIFO queues with tail drop, regional
propagation delay, wireless jitter, bursty loss). The engine runs unmodified
over real `RTCPeerConnection` data channels. Most scenarios use synthetic
frames with realistic L1T3 sizes (8 Mb/s at 60 fps, keyframes ≈ 8× a frame).
The `codec-*` and `h2h-*` scenarios use real WebCodecs encode and decode of a
moving canvas.

### Environment constraints

The friend group is one streamer and seven viewers:

| Who | Profile | Up / down (effective) | Extra | Declared relay budget |
|---|---|---|---|---|
| streamer | busy uplink | 20 / 300 Mb/s | 200 ms router buffer | 20 Mb/s |
| minji | 기가 FTTH | 500 / 500 | | 100 |
| junho | 500M FTTH | 450 / 450 | | 60 |
| seoyeon | 100M FTTH, Busan | 90 / 90 | +4 ms each way | 25 |
| dohyun | 100M FTTH | 90 / 90 | | 25 |
| haeun | VDSL apartment | 35 / 90 | 250 ms buffer | 10 |
| jiwoo | weak Wi-Fi | 40 / 60 | 3 ms jitter, 0.8 % bursty loss | 8 |
| taeyang | phone tethering | 12 / 60 | 15 ms access, 5 ms jitter, 0.3 % loss | 0 |

The loss figures are deliberately pessimistic *residual* loss after Wi-Fi and
LTE link-layer retransmission.

## Results

Numbers are from the final engine. Scenarios marked ×3 were run three times
with different loss/jitter seeds: the value is the mean, with the min–max
range in parentheses. The full tables are in `sim/results/tables.md`
(regenerate with `npm run report`).

### 1. What the streamer pays: mesh vs relay, real codecs

A real WebRTC mesh (the app today: a video track per viewer, so one encoder
and one GCC per viewer) against the relay tree. Same moving 360p30 canvas at
1.5 Mb/s, software VP8/VP9 in headless Chromium, streamer uplink not the
bottleneck. CPU is the streamer's whole browser, in % of one core.

| Viewers | Streamer CPU, mesh → relay | Streamer upload, mesh → relay | Glass-to-glass p50, mesh → relay | Worst p95, mesh → relay |
|---|---|---|---|---|
| 2 | 34% → 27% | 3.1 → 1.7 Mb/s | 40 → 37 ms | 63 → 74 ms |
| 4 | 61% → 28% | 6.3 → 1.7 Mb/s | 46 → 43 ms | 93 → 101 ms |
| 6 | **85% → 28%** | **9.3 → 1.7 Mb/s** | 59 → 49 ms | 148 → 212 ms |

- **Mesh cost grows linearly with viewers (~+12 % CPU per viewer at 360p);
  relay cost is flat.** Encoding is per pixel, so at 1080p60 each extra mesh
  viewer costs about 9× this in software, which is the reported symptom once
  hardware encoder sessions run out.
- Glass-to-glass latency (capture → display, read back from a timestamp
  painted into the frame) is the same or better with the relay at the median.
  The tail is somewhat worse, from the extra hop.
- What relays pay instead: the busiest relay (feeding 5) used 36 % CPU
  (viewing alone costs ~25 %) and 8.6 Mb/s of upload.

### 2. The friend group under constraints (synthetic 8 Mb/s, 60 fps)

`star` is the same engine with every viewer fed directly by the streamer.
It's a *network* stand-in for the mesh, but harsher than real WebRTC, which
would lower each viewer's bitrate with GCC instead of queueing. It shows what
a 20 Mb/s uplink does with 7 × 8 Mb/s. Part 1 is the fair comparison with
the real mesh.

| Scenario | fps | Time at full quality | Latency p50 | Freeze total | Streamer upload |
|---|---|---|---|---|---|
| `star-busy` ×3 | 7.9 | 0% | 1204 ms | 139 s | 20.1 Mb/s (saturated) |
| **`tree-busy` ×3** | **49.6** | **78%** | **37 ms** | **1.9 s** | 16.0 Mb/s |
| `tree-relay-leave` ×3 | 48.0 | 75% | 36 ms | 5.4 s | 14.8 Mb/s |
| `tree-churn` ×3 | 46.9 | 71% | 33 ms | 7.5 s | 12.3 Mb/s |
| `tree-relay-crash` ×3 | 45.1 | 71% | 37 ms | 15.3 s | 13.8 Mb/s |
| `tree-uplink-drop` ×3 | 39.9 | 65% | 36 ms | 29.1 s | 12.9 Mb/s |
| `tree-overclaim` ×3 | 38.1 | 60% | 50 ms | 35.6 s | 14.0 Mb/s |

Averages hide the split between viewers. Per viewer in `tree-busy`:

| minji | junho | seoyeon | dohyun | haeun | jiwoo (weak Wi-Fi) | taeyang (tethering) |
|---|---|---|---|---|---|---|
| 60 fps, 100%, 22 ms | 60, 100%, 23 ms | 60, 100%, 37 ms | 60, 100%, 35 ms | 60, 100%, 37 ms | 25, 26%, 65 ms | 22, 17%, 110 ms |

**Every viewer on a sound connection gets full quality (60 fps) at about
20–40 ms**, while the two lossy viewers are identified as limited by their
own connections, kept as leaves, and get 15–30 fps without dragging anyone
else down.

Events (per affected viewer on a sound connection, over 3 runs each):

| Event | Freeze |
|---|---|
| The busiest relay **crashes** (no goodbye) | 1.1–1.9 s, then back to normal |
| A relay **leaves** normally | 0–0.5 s |
| A relay's **uplink collapses** 500 → 10 Mb/s | 0.5–1.0 s |
| A viewer joins | first frame in 0.1–0.4 s (one run: 3 s) |

`tree-overclaim` is the weakest case. A relay says it can spare 60 Mb/s but
has 12, and the tree has to find that out from uplink queueing. That costs
around 30 s of accumulated freezes over 40 s, mostly early.

### 3. The real limit: lossy links (SCTP)

Data channels run on SCTP, whose congestion control backs off on every loss.
WebRTC video's GCC is delay-based and tolerates random loss far better. One
viewer, fed directly, 8 Mb/s:

| Residual loss | RTT ≈ 12 ms | RTT ≈ 30 ms |
|---|---|---|
| 0 % | 60 fps | 60 fps |
| 0.1 % | 60 fps | 55 fps |
| 0.3 % | 60 fps | 30 fps |
| 0.8 % | 55 fps | 21 fps |

The loss × RTT product is what matters (as the Mathis formula predicts). Most
Korean domestic paths are short, which helps. The engine degrades gracefully
here (fewer temporal layers, no freezes) instead of collapsing. But **a group
where everyone is on bad Wi-Fi (`tree-lossy-all`) is poor with or without the
tree**: 10.8 fps vs 7.3 fps for `star-lossy-all`.

### 4. Correctness of the real codec path

`codec-tree`: real WebCodecs VP9 L1T3 (software) encoded once, relayed,
layers shed for weak viewers, and a relay leaving mid-stream. **0 decode
errors** at every viewer. `sim/smoke/` runs the actual app `CallManager`
(relay transport) three times against a fake server: both viewers render
~30 fps, one of them through the other.

## Conclusions

- The idea holds up. Encoding once and forwarding bytes removes the
  streamer's per-viewer CPU and upload cost (flat vs linear), without a
  server and without worse latency. On sound connections viewers get 60 fps
  at 20–40 ms even when the streamer's uplink could feed only two of them
  directly.
- The tree survives relays crashing, leaving, and losing their uplink, with
  about 1–2 s of freeze for the viewers below.
- The transport's weak spot is loss: SCTP data channels need decent links.
  Viewers on lossy Wi-Fi get a lower frame rate. That's acceptable for one
  or two of them, but a group of mostly-wireless viewers would do better on
  the WebRTC mesh at a lower bitrate.

## Open risks (verify with friends)

1. **Hardware codec paths are untested.** The simulator only has software
   codecs. On Windows, check which encoder `chooseEncoder` picks (the log
   shows `hardware: true/false` and the codec) and whether H.264 hardware
   offers L1T3. Without temporal layers, relays can't shed load gracefully.
2. **Relays on busy PCs.** A relay that is also running a game adds main-thread
   latency. In the simulator a saturated host CPU stalled whole SCTP
   associations. Watch the "relay via X" caption for viewers whose parent is
   gaming.
3. **Static screens:** Chromium delivers capture frames only on change. The
   encoder re-encodes the last frame at ≥ 4 fps so viewers aren't marked
   orphaned, which hasn't been tested with real capture.
4. **macOS (WKWebView)** has no `MediaStreamTrackProcessor`, so a Mac
   *sharer* falls back to the mesh automatically. Mac *viewers* decode to a
   canvas.
5. **Declared relay budget:** the "Relay upload" setting defaults to
   20 Mb/s. Overclaiming works but is the slowest-converging case.

## How to try it with friends

1. Everyone updates to the build from this branch (all participants need
   it: the data channels are pre-negotiated on both ends).
2. Each person sets **⚙ → Relay upload** to roughly a quarter of their real
   upload (or *none* on Wi-Fi or tethering).
3. The sharer sets **⚙ → Transport → Relay tree (experimental)**, then
   shares.
4. Watch the tile captions: viewers show `relay direct` or `relay via <name>`,
   the fps, and `(reduced fps)` when a layer is shed; the sharer's preview
   shows how many are watching and how many are fed directly.
5. Compare with the same group on **WebRTC mesh**: the sharer's CPU (Task
   Manager), upload, and how smooth it feels for the viewers.

## What the simulation changed in the design

The first version looked reasonable on paper. Every item below is a failure
the simulator surfaced, with the fix now in the engine.

1. **Keyframes read as congestion.** A keyframe (≈ 8 frames' worth) sits in
   `bufferedAmount` for a few RTTs even on a fast link, so every keyframe shed
   a layer. Fix: judge the *standing* queue (minimum over 500 ms), not the
   instantaneous one.
2. **Weak children blamed on their parent.** "Most of my child links are
   congested" looked like a saturated uplink whenever the two lossy viewers
   landed under the same parent, and even a 450 Mb/s relay had its capacity
   cut. The cascading re-plans caused most of the freezes. Fix: measure each
   node's uplink queue directly from one-way-delay trends, taking the median
   over all peers (`owd.ts`).
3. **One lossy viewer caused keyframes for everyone.** With a single
   partially reliable channel, every lost base-layer frame broke that
   viewer's chain, and the keyframe it requested cost the whole tree (17
   keyframes in 25 s). Fix: a reliable base-layer channel and an unreliable
   enhancement channel, keyframes coalesced to ≤ 1/s, and make-before-break
   parent switches.
4. **Silent starvation (a deadlock).** A relay that dropped one base frame
   for a child had to drop every later frame too (they all depend on it). The
   child received nothing, so it never saw a broken frame and never asked for
   a keyframe; one viewer sat frozen for 17 s with only audio arriving. Fix:
   the relay asks for the keyframe itself, and receivers also ask when video
   stalls while audio keeps flowing.
5. **Inherited degradation mistaken for a weak viewer.** A child can't get
   more layers than its parent receives. Fix: a viewer is judged only when
   its parent is at full quality.
6. **Nodes with no upload used as fallback parents.** When every slot was
   taken, a tethered phone (0 relay budget) got children. Fix: overcommit only
   relay-capable nodes, least-loaded first.
7. **"Try another parent" stranded a viewer.** Banning the only possible edge
   left a single lossy viewer with no parent (2 fps). Fix: bans are
   preferences; a viewer is never left without a parent.
8. **Capacity cuts overshot.** Cutting an estimate to the measured send rate
   undershoots badly while SCTP is backing off (a 20 Mb/s uplink was
   estimated at 3.6 Mb/s). Fix: halve at most per step (AIMD), don't judge a
   node for 4 s after its children change, recover stepwise with a backoff
   that doubles.

Two things were checked and ruled out. A fully reliable *ordered*
single channel does work, but head-of-line blocking gives it much worse tail
latency than the split. And SCTP associations stalling for good showed up
only while the simulation host's CPU was saturated by unrelated builds; it
never reproduced on an idle host (see the open risks below).
