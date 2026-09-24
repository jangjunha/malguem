### Friend group (synthetic 8 Mb/s 1080p60-sized stream)

| Scenario | runs | fps (avg) | full quality | latency p50 | worst p95 | freeze total | streamer up | busiest relay up | keyframes |
|---|---|---|---|---|---|---|---|---|---|
| `star-busy` | 3 | 7.9 (7.8–8.0) | 0% | 1204 ms (1106–1342) | 13021 ms (10450–16474) | 138.7 s (132.0–142.7) | 20.1 Mb/s | 0.3 Mb/s | 40 |
| `tree-busy` | 3 | 49.6 (49.1–50.1) | 78% (77–79) | 37 ms (36–37) | 816 ms (637–1145) | 1.9 s (0.8–3.8) | 16.0 Mb/s (15.8–16.4) | 33.4 Mb/s (32.5–34.5) | 6 (4–9) |
| `tree-busy-partial` | 1 | 48.9 | 78% | 33 ms | 552 ms | 8.9 s | 12.4 Mb/s | 39.0 Mb/s | 20 |
| `tree-busy-reliable` | 1 | 49.7 | 78% | 36 ms | 1242 ms | 1.7 s | 14.1 Mb/s | 35.7 Mb/s | 6 |
| `tree-relay-crash` | 3 | 45.1 (42.7–48.5) | 71% (64–80) | 37 ms (37–38) | 1228 ms (1127–1370) | 15.3 s (12.7–19.0) | 13.8 Mb/s (12.3–16.0) | 18.1 Mb/s (16.5–19.4) | 15 (13–16) |
| `tree-relay-leave` | 3 | 48.0 (45.5–49.4) | 75% (70–77) | 36 ms (35–37) | 1027 ms (547–1320) | 5.4 s (0.8–9.5) | 14.8 Mb/s (13.1–16.0) | 19.0 Mb/s (17.8–19.7) | 9 (4–13) |
| `tree-overclaim` | 3 | 38.1 (34.4–40.7) | 60% (58–64) | 50 ms (43–58) | 3069 ms (2744–3381) | 35.6 s (29.1–46.5) | 14.0 Mb/s (13.0–15.6) | 16.3 Mb/s (14.8–17.7) | 17 (14–22) |
| `tree-uplink-drop` | 3 | 39.9 (39.3–40.5) | 65% (62–67) | 36 ms (34–37) | 2083 ms (1583–2812) | 29.1 s (21.7–37.5) | 12.9 Mb/s (12.4–13.8) | 24.9 Mb/s (22.3–26.4) | 14 (12–15) |
| `tree-churn` | 3 | 46.9 (43.3–49.0) | 71% (63–76) | 33 ms (32–33) | 1217 ms (1028–1422) | 7.5 s (4.5–10.7) | 12.3 Mb/s (12.0–12.8) | 24.6 Mb/s (22.7–26.0) | 13 (10–16) |
| `star-lossy-all` | 1 | 7.3 | 0% | 1381 ms | 4365 ms | 137.1 s | 20.0 Mb/s | 0.2 Mb/s | 39 |
| `tree-lossy-all` | 1 | 10.8 | 5% | 802 ms | 3647 ms | 105.3 s | 17.3 Mb/s | 4.4 Mb/s | 36 |

### Per viewer (mean over runs): fps / full-quality share / latency p50

| Scenario | minji | junho | seoyeon | dohyun | haeun | jiwoo | taeyang |
|---|---|---|---|---|---|---|---|
| `star-busy` | 9 / 0% / 1235 ms | 13 / 0% / 729 ms | 10 / 0% / 985 ms | 13 / 0% / 816 ms | 8 / 0% / 1325 ms | 1 / 0% / 7136 ms | 1 / 0% / 6675 ms |
| `tree-busy` | 60 / 100% / 22 ms | 60 / 100% / 23 ms | 60 / 100% / 37 ms | 60 / 100% / 35 ms | 60 / 100% / 37 ms | 25 / 26% / 65 ms | 22 / 17% / 110 ms |
| `tree-overclaim` | 47 / 79% / 44 ms | 43 / 67% / 40 ms | 46 / 78% / 35 ms | 49 / 81% / 24 ms | 47 / 77% / 50 ms | 18 / 19% / 169 ms | 17 / 16% / 288 ms |
| `tree-uplink-drop` | 51 / 83% / 26 ms | 48 / 83% / 27 ms | 46 / 79% / 37 ms | 47 / 83% / 34 ms | 47 / 83% / 36 ms | 20 / 19% / 92 ms | 20 / 21% / 132 ms |
| `tree-relay-crash` | 57 / 91% / 18 ms | 55 / 91% / 26 ms | 54 / 90% / 38 ms | 53 / 89% / 32 ms | 53 / 88% / 37 ms | 24 / 26% / 67 ms | 21 / 25% / 179 ms |
| `star-lossy-all` | 9 / 0% / 1217 ms | 7 / 0% / 1381 ms | 6 / 0% / 1705 ms | 9 / 0% / 1242 ms | 7 / 0% / 1441 ms | 9 / 0% / 1070 ms | 3 / 0% / 2790 ms |
| `tree-lossy-all` | 11 / 6% / 711 ms | 13 / 11% / 764 ms | 12 / 9% / 450 ms | 10 / 0% / 802 ms | 9 / 0% / 1257 ms | 10 / 3% / 1042 ms | 10 / 3% / 917 ms |

### Mesh vs relay, real codecs (360p30, 1.5 Mb/s, streamer uplink not limiting)

| Viewers | streamer CPU mesh → relay | streamer upload mesh → relay | g2g latency p50 mesh → relay | worst p95 mesh → relay | fps mesh → relay | busiest relay CPU / upload |
|---|---|---|---|---|---|---|
| 2 | 34% → 27% | 3.1 → 1.7 Mb/s | 40 → 37 ms | 63 → 74 ms | 28.5 → 27.7 | 26% / 1.8 Mb/s |
| 4 | 61% → 28% | 6.3 → 1.7 Mb/s | 46 → 43 ms | 93 → 101 ms | 27.8 → 26.3 | 32% / 5.2 Mb/s |
| 6 | 85% → 28% | 9.3 → 1.7 Mb/s | 59 → 49 ms | 148 → 212 ms | 26.7 → 25.3 | 36% / 8.6 Mb/s |

### Single viewer over a lossy path (SCTP transport limit)

| Residual loss | RTT ≈ 12 ms | RTT ≈ 30 ms |
|---|---|---|
| 0.0 % | 60 fps, 100% full, p50 16 ms | 60 fps, 100% full, p50 23 ms |
| 0.1 % | 60 fps, 100% full, p50 17 ms | 55 fps, 90% full, p50 25 ms |
| 0.3 % | 60 fps, 100% full, p50 17 ms | 30 fps, 30% full, p50 48 ms |
| 0.8 % | 55 fps, 90% full, p50 20 ms | 21 fps, 10% full, p50 58 ms |

### Real codec through relays (`codec-tree`)

| Viewer | fps | full quality | g2g p50 | parent at end |
|---|---|---|---|---|
| minji | 24.9 | 100% | 109 ms | junho |
| junho | 26.4 | 100% | 61 ms | streamer |
| jiwoo | 23.4 | 84% | 95 ms | junho |
| taeyang | 22.9 | 84% | 120 ms | junho |
