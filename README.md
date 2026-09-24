# malguem

A lightweight, self-hostable Discord alternative for a small group of friends:
text chat, voice calls, and low-latency game screen sharing. Media is
peer-to-peer and end-to-end encrypted; the server only does signaling and
stores ciphertext.

Built to fix three Discord pain points: paid-tier screen-share quality limits,
a heavy client, and ads.

## Architecture at a glance

```
┌────────────┐   WebRTC (DTLS-SRTP, P2P mesh)   ┌────────────┐
│  Client A  │◄────────────────────────────────►│  Client B  │
│ Tauri +    │                                  │            │
│ WebView2   │      (TURN relay, last resort,   │            │
└─────┬──────┘       ciphertext only)           └─────┬──────┘
      │                                               │
      │  HTTPS + WebSocket (signaling, presence,      │
      │  ciphertext chat, wrapped keys)               │
      └────────────────┬──────────────────────────────┘
                       ▼
              ┌─────────────────┐
              │  malguem-server │  single Rust binary + SQLite
              │  (+ coturn)     │  runs on a home box or tiny VPS
              └─────────────────┘
```

- **Client**: Tauri v2 shell (Rust) + Svelte/TypeScript UI in WebView2.
  All real-time media uses the webview's built-in Chromium WebRTC stack:
  hardware encoding, congestion control, and `getStats()` telemetry for free.
- **Server**: one Rust binary (`malguem-server`), SQLite storage, WebSocket
  signaling. It never sees media and never sees plaintext messages.
- **Connectivity**: ICE with STUN; self-hosted TURN (coturn) as an automatic
  last resort for CGNAT/symmetric-NAT peers. TURN relays ciphertext only.
- **Encryption**: calls are E2E via pairwise DTLS-SRTP with identity-signed
  fingerprints; chat uses a per-space symmetric key wrapped to each member's
  device key. See [docs/CRYPTO.md](docs/CRYPTO.md).

See [docs/DECISIONS.md](docs/DECISIONS.md) for why each of these was chosen,
[docs/PROTOCOL.md](docs/PROTOCOL.md) for the client–server protocol, and
[docs/SPIKE.md](docs/SPIKE.md) for the Windows validation checklist
(milestone 0).

## Repository layout

```
server/    malguem-server: signaling + ciphertext storage (Rust, axum, SQLite)
client/    desktop app (Tauri v2 + Svelte 5 + TypeScript)
deploy/    docker-compose, Caddyfile, coturn config
docs/      decisions, protocol, crypto design, spike checklist, experiments
sim/       network-emulated simulator for the relayed broadcast (Linux, root)
```

## Running the server (operator quick start)

```sh
cd deploy
cp .env.example .env   # set DOMAIN and TURN_SECRET
docker compose up -d
```

This starts `malguem-server`, coturn (TURN relay), and Caddy (automatic TLS).
A home box behind a router needs ports 443/tcp, 3478/udp+tcp and the coturn
relay range 49160–49200/udp forwarded. See [deploy/README.md](deploy/README.md).

Bare-metal alternative: `cargo build --release -p malguem-server` produces a single
binary; point it at a SQLite path and put any TLS proxy in front.

## Developing

```sh
# server
cd server && cargo test && cargo run

# client (UI in a browser during development, full app via Tauri)
cd client && npm install && npm run dev      # Vite dev server
npm run tauri dev                            # full desktop app (Windows)
```

### Reproducible toolchain (Nix)

To pin the exact Rust/Node versions so builds match across machines, CI, and
Docker, use the flake:

```sh
nix develop                  # dev shell with pinned Rust + Node + Tauri deps
nix build .#malguem-server        # build the server binary reproducibly
nix run  .#malguem-server         # build and run it
```

The toolchain version lives in one place — `rustToolchain` in `flake.nix`.
With [direnv] installed, `direnv allow` loads the shell automatically. Commit
the generated `flake.lock` after the first `nix develop` to lock the inputs.

[direnv]: https://direnv.net

## Status

Early development. Feature scope for v1 (deliberately small):

- [x] Architecture & protocol design
- [x] Spaces, channels, invites
- [x] Encrypted text chat with offline catch-up
- [x] Chat niceties: clickable links with client-side OpenGraph previews and
      Slack-style `:shortcode:` emoji
- [x] Owner-managed (animated) webp stickers — E2E-encrypted with the space key
- [x] Voice calls (P2P mesh) with join/leave chimes — *built, pending Windows validation*
- [x] Screen share with sender-side codec/bitrate/resolution/fps controls
      and system-audio capture — *built, pending the
      [Windows spike](docs/SPIKE.md)*
- [ ] Relayed screen broadcast (encode once, viewers pass it on) —
      *experimental, simulated; see
      [docs/experiments/relay-broadcast.md](docs/experiments/relay-broadcast.md)*
- [ ] Windows installer + auto-update (Tauri bundling configured; updater
      and release pipeline still to do)

Non-goals for v1: DMs, message reactions, general file attachments, threads,
search, moderation tooling, mobile apps. (Stickers are a deliberately narrow
exception to "no uploads": owner-curated, webp-only, encrypted.) The data model
deliberately leaves room for DMs and attachments later.
