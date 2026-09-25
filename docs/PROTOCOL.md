# Client–server protocol

Transport: HTTPS REST for state changes and history; one authenticated
WebSocket per client for real-time events and call signaling. All bodies are
JSON. Authentication: `Authorization: Bearer <token>` (see CRYPTO.md §Login).

The server never sees plaintext message content or media. Binary fields
(keys, nonces, ciphertext, signatures) are base64url-encoded strings.

IDs: users/spaces/channels use UUIDv4; messages use ULID (lexicographically
time-sortable, used as the pagination cursor).

## REST

### Auth

| Method | Path | Body → Response |
|---|---|---|
| POST | `/api/register` | `{name, sign_pub, kem_pub}` → `{user_id}` |
| GET | `/api/auth/challenge?user_id=` | → `{nonce}` (single-use, 60 s TTL) |
| POST | `/api/auth/login` | `{user_id, nonce, sig}` → `{token, user}` |

### Spaces, channels, members, invites

| Method | Path | Notes |
|---|---|---|
| POST | `/api/spaces` | `{name}` → space. Creator becomes `owner`. |
| GET | `/api/spaces` | Spaces the caller belongs to, with channels. |
| GET | `/api/spaces/:id/members` | `[{user_id, name, role, sign_pub, kem_pub}]` — source for key pinning. |
| DELETE | `/api/spaces/:id/members/:user_id` | Owner only. Deletes the member's key wraps. Follow with a key rotation. |
| POST | `/api/spaces/:id/channels` | `{name}` → channel. Owner only. |
| POST | `/api/spaces/:id/invites` | `{expires_in_secs?}` → `{token}`. Owner only. |
| POST | `/api/invites/:token/accept` | Joins the space → space. Emits `key_request` to online members. |

### Space keys (wrapped, opaque to server)

| Method | Path | Notes |
|---|---|---|
| POST | `/api/spaces/:id/keys` | `{epoch, wraps: [{user_id, wrapped}]}`. Any member may upload wraps for an existing epoch (join flow); only the owner may introduce a **new** epoch (rotation). |
| GET | `/api/spaces/:id/keys` | `{current_epoch, wraps: [{epoch, wrapped}]}` — only the caller's own wraps. |

### Messages (ciphertext)

| Method | Path | Notes |
|---|---|---|
| POST | `/api/channels/:id/messages` | `{epoch, nonce, ct, sig}` → `{id, created_at}`. Fans out `message_new` to online space members. |
| GET | `/api/channels/:id/messages?before=<ulid>&limit=50` | Newest-first page of `{id, sender_id, epoch, nonce, ct, sig, created_at}`. |

The decrypted message plaintext is a small JSON object tagged by `t`: `{"t":"text","body":"…"}` for chat, `{"t":"sticker","id":"<sticker_id>"}` to send a space sticker. Unknown tags render as a placeholder, so new body types are backward-compatible.

### Stickers (owner-managed, ciphertext blobs)

Stickers are space assets — (animated) webp images uploaded by the owner,
end-to-end encrypted with the space key just like messages. The server stores
and serves the ciphertext blob and never sees the image. `ct` is the base64url
AEAD ciphertext of the webp bytes under the space key for `epoch` (AAD binds
`space_id` and `epoch`; see CRYPTO.md).

| Method | Path | Notes |
|---|---|---|
| GET | `/api/spaces/:id/stickers` | `[{id, name, epoch, created_by, created_at}]` — metadata only. Any member. |
| GET | `/api/spaces/:id/stickers/:sticker_id` | `{id, name, epoch, nonce, ct, created_at}` — the encrypted blob, fetched on demand. Any member. |
| POST | `/api/spaces/:id/stickers` | `{name, epoch, nonce, ct}` → metadata. Owner only. `epoch` must reference an existing space key; `ct` ≤ 2 MiB. Fans out `sticker_added`. |
| DELETE | `/api/spaces/:id/stickers/:sticker_id` | Owner only. Fans out `sticker_removed`. |

### TURN

| Method | Path | Notes |
|---|---|---|
| GET | `/api/turn-credentials` | → `{urls: [...], username, credential, ttl_secs}`. Ephemeral HMAC credentials (coturn `use-auth-secret`): `username = "<unix_expiry>:<user_id>"`, `credential = base64(HMAC-SHA1(secret, username))`. |

Errors: non-2xx with `{error: "machine_readable_code", message: "human text"}`.

## WebSocket `/api/ws`

Connect with the bearer token (`Sec-WebSocket-Protocol: bearer,<token>` or
`?token=` during development). One connection per client. Messages are JSON
with a `type` tag.

### Client → server

```jsonc
{"type": "call_join",  "channel_id": "…"}
{"type": "call_leave", "channel_id": "…"}
{"type": "call_streaming", "channel_id": "…", "streaming": true}
{"type": "signal", "channel_id": "…", "to": "<user_id>",
 "payload": { /* opaque to server */ }, "sig": "…"}
{"type": "ping"}
```

`signal.payload` is producer-defined (SDP / ICE / renegotiation), signed per
CRYPTO.md §Call security. The server relays it only if both users share the
space and the recipient is online; it never inspects it.

### Server → client

```jsonc
{"type": "hello", "user_id": "…"}                 // on connect
{"type": "pong"}
{"type": "message_new", "channel_id": "…", "message": {…}}   // same shape as REST history
{"type": "call_roster", "channel_id": "…", "participants": ["user_id", …]}
{"type": "call_presence", "space_id": "…", "channel_id": "…",
 "participants": ["user_id", …], "streaming": ["user_id", …]}
{"type": "call_peer_joined", "channel_id": "…", "user_id": "…"}
{"type": "call_peer_left",   "channel_id": "…", "user_id": "…"}
{"type": "signal", "channel_id": "…", "from": "…", "payload": {…}, "sig": "…"}
{"type": "member_joined", "space_id": "…",
 "user": {"user_id": "…", "name": "…", "sign_pub": "…", "kem_pub": "…"}}
{"type": "member_removed", "space_id": "…", "user_id": "…"}
{"type": "channel_created", "space_id": "…", "channel": {"id": "…", "name": "…"}}
{"type": "key_request", "space_id": "…",          // wrap your epochs for this user
 "user": {"user_id": "…", "kem_pub": "…"}}
{"type": "keys_updated", "space_id": "…", "current_epoch": 2}
{"type": "sticker_added", "space_id": "…",
 "sticker": {"id": "…", "name": "…", "epoch": 1, "created_by": "…", "created_at": 0}}
{"type": "sticker_removed", "space_id": "…", "sticker_id": "…"}
```

### Call membership semantics

- `call_join` adds the sender to the channel's roster and broadcasts
  `call_peer_joined` plus a fresh `call_roster` to roster members; the joiner
  receives the existing roster and **initiates offers to every existing
  participant** (deterministic initiator = the joiner; later renegotiation
  uses perfect negotiation with the lexicographically smaller user id as the
  polite peer).
- Disconnecting the WebSocket implies `call_leave` from all rosters.
- `call_presence` goes to **every member of the channel's space** (in the
  call or not) whenever its roster or streaming set changes, so clients can
  show who is in which call before joining; an empty `participants` means the
  call ended. Each new connection first gets a snapshot: one `call_presence`
  per running call in the user's spaces. `call_streaming` (accepted only from
  a participant) marks them LIVE in that presence; leaving clears it.
- Voice and screen-share travel over the same peer connections; screen share
  is an extra video (+ audio) track added by the broadcaster with sender-side
  codec/bitrate/resolution/fps settings applied locally.
- `signal` payloads are opaque to the server. Clients send `{"kind": "sdp", …}`
  and `{"kind": "ice", …}` for negotiation, and
  `{"kind": "state", "state": {"muted": bool, "deafened": bool, "streaming": bool}}`
  to show their mic/deafen/screen-share state to each peer (sent on connect
  and on every change). Screen shares are opt-in: a viewer sends
  `{"kind": "watch", "watch": true|false}` to the broadcaster, who adds the
  screen tracks to (or removes them from) only that viewer's connection.
  Clients ignore kinds they don't know.

## Versioning

The WebSocket handshake path is unversioned during pre-1.0; breaking protocol
changes bump `/api` → `/api/v2` and are listed here. The protocol is the
contract a future mobile or native client builds against.
