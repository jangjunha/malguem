/**
 * Central app state (Svelte 5 runes) and orchestration: multi-server auth,
 * spaces, key management, message encrypt/decrypt, and call lifecycle.
 *
 * Multi-server model: the client logs into N servers in parallel, each with its
 * own Identity{sign_key, kem_key}, Api, and EventSocket. All per-server reactive
 * state (spaces/members/messages/locked) is keyed by `serverId`; the non-reactive
 * connection objects (identity, api, socket, space keys) live in `conns`.
 */
import { Api, type Member, type Space, type StickerMeta, type WireMessage } from './api';
import {
  CallManager,
  DEFAULT_BROADCAST,
  type BroadcastSettings,
  type PeerLink,
  type PeerState,
  type PeerStats,
} from './call';
import type { HubStats } from './relay/hub';
import { MicInput, type MicLevel } from './mic';
import { AudioMixer } from './mixer';
import { prefs, type CallPrefs, type KeybindPrefs, type VoicePrefs } from './prefs';
import { credentials, migrateLegacyAccount, type AccountIndex } from './credentials';
import {
  b64u,
  decryptBlob,
  decryptMessage,
  deserializeIdentity,
  encryptBlob,
  encryptMessage,
  fingerprint,
  generateIdentity,
  generateSpaceKey,
  loginSignature,
  openBox,
  sealBox,
  serializeIdentity,
  unb64u,
  type Identity,
} from './crypto';
import {
  playJoin,
  playLeave,
  playStreamStart,
  playStreamStop,
  playToggle,
  setSoundPrefs,
  type SoundPrefs,
} from './sounds';
import { EventSocket, type ServerEvent, type SocketStatus } from './ws';

export interface ChatMessage {
  id: string;
  senderId: string;
  senderName: string;
  /** Plain text body for `kind === 'text'`; empty for stickers. */
  body: string;
  /** Discriminates how the message renders. */
  kind: 'text' | 'sticker';
  /** Sticker id for `kind === 'sticker'`. */
  stickerId?: string;
  createdAt: number;
  ok: boolean; // false = could not decrypt/verify
}

export interface AudioDevice {
  deviceId: string;
  label: string;
}

export interface ActiveCall {
  serverId: string;
  selfId: string;
  channelId: string;
  spaceId: string;
  manager: CallManager;
  participants: string[];
  remoteStreams: Record<string, MediaStream[]>;
  stats: Record<string, PeerStats>;
  /** Relay-tree engine state (experimental relay transport). */
  relay: HubStats | null;
  broadcasting: boolean;
  /** The share picker is open / the broadcast is starting. */
  broadcastStarting: boolean;
  /** userId → the mute/deafen/streaming state they announced. */
  peerStates: Record<string, PeerState>;
  /** userId → media connection health. */
  links: Record<string, PeerLink>;
  /** Broadcasters whose screen we're watching. */
  watching: string[];
}

/** Who's in a channel's call, as seen from outside it. */
export interface CallPresence {
  participants: string[];
  streaming: string[];
}

/** Reactive, non-secret view of a connected server, shown in the UI. */
export interface ServerView {
  id: string;
  serverUrl: string;
  userId: string;
  userName: string;
  status: 'connecting' | 'online' | 'offline';
  error: string | null;
}

const LS_LAST_SERVER = 'vc.lastServerUrl';

/** Composite key for per-server reactive maps. `|` never appears in UUIDs. */
function ck(serverId: string, id: string): string {
  return `${serverId}|${id}`;
}

/**
 * Non-reactive holder for one server connection: secrets and live handles that
 * must never land in reactive state.
 */
class ServerConn {
  socket: EventSocket | null = null;
  /** epoch → key, per space. Sensitive: kept out of reactive state. */
  spaceKeys = new Map<string, Map<number, Uint8Array>>();

  constructor(
    public id: string,
    public serverUrl: string,
    public userId: string,
    public identity: Identity,
    public api: Api,
  ) {}
}

class AppStore {
  phase = $state<'loading' | 'onboarding' | 'main'>('loading');
  error = $state<string | null>(null);

  servers = $state<ServerView[]>([]);
  /** serverId → spaces */
  spaces = $state<Record<string, Space[]>>({});
  /** `serverId|spaceId` → members */
  members = $state<Record<string, Member[]>>({});
  /** `serverId|channelId` → messages */
  messages = $state<Record<string, ChatMessage[]>>({});
  /** `serverId|spaceId` → sticker metadata (blobs fetched + decrypted lazily) */
  stickers = $state<Record<string, StickerMeta[]>>({});
  /** `serverId|spaceId` → true when we lack the current epoch key */
  lockedSpaces = $state<Record<string, boolean>>({});

  activeServerId = $state<string | null>(null);
  activeSpaceId = $state<string | null>(null);
  activeChannelId = $state<string | null>(null);

  call = $state<ActiveCall | null>(null);
  broadcastSettings = $state<BroadcastSettings>({ ...DEFAULT_BROADCAST });

  /** Mic muted — kept outside the call so you can mute before joining, like Discord. */
  micMuted = $state(prefs.loadSelf().micMuted);
  /** Incoming audio silenced (and mic forced muted, Discord-style). */
  deafened = $state(prefs.loadSelf().deafened);
  voice = $state<VoicePrefs>(prefs.loadVoice());
  keybinds = $state<KeybindPrefs>(prefs.loadKeybinds());
  /** `serverId|userId` → playback volume (1 = 100%, up to 2 = 200%), remembered across calls. */
  userVolumes = $state<Record<string, number>>(prefs.loadVolumes());
  /** Live mic meter while in a call or testing the mic. */
  micLevel = $state<MicLevel | null>(null);
  /** userIds currently talking in the call (self included). */
  speaking = $state<Record<string, boolean>>({});
  inputDevices = $state<AudioDevice[]>([]);
  /** Pickable audio outputs, when the webview supports choosing one. */
  outputDevices = $state<AudioDevice[]>([]);
  sounds = $state<SoundPrefs>(prefs.loadSounds());
  callPrefs = $state<CallPrefs>(prefs.loadCall());
  /** `serverId|channelId` → running call in that channel (from the server). */
  presence = $state<Record<string, CallPresence>>({});
  /** serverId → event-socket status (reconnect banner). */
  socketStatus = $state<Record<string, SocketStatus>>({});
  /** Result of the last hotkey registration (OS-wide shortcuts, conflicts). */
  hotkeyStatus = $state<{ global: string[]; errors: string[] }>({ global: [], errors: [] });
  /** Settings dialog, opened from the user panel. */
  settingsOpen = $state<null | 'voice' | 'keybinds' | 'sounds'>(null);

  /** WebAudio playback graph for the current call. Never reactive. */
  private mixer: AudioMixer | null = null;
  /** Mic capture for the call (or a mic test in settings). Never reactive. */
  private mic: MicInput | null = null;
  private speakingTimer: ReturnType<typeof setInterval> | null = null;
  private micTestWanted = false;
  /** Mic muted-state captured when deafening, restored on undeafen. */
  private preDeafenMicMuted = false;

  /** serverId → live connection. Never reactive. */
  private conns = new Map<string, ServerConn>();

  /** `serverId|stickerId` → object URL of the decrypted webp. Non-reactive. */
  private stickerUrls = new Map<string, string>();
  /** In-flight sticker decrypts, deduped so concurrent renders share one fetch. */
  private stickerLoads = new Map<string, Promise<string | null>>();

  // ---------- selectors ----------

  get activeServer(): ServerView | null {
    return this.servers.find((s) => s.id === this.activeServerId) ?? null;
  }

  get activeSpace(): Space | null {
    if (!this.activeServerId) return null;
    return this.spaces[this.activeServerId]?.find((s) => s.id === this.activeSpaceId) ?? null;
  }

  spacesOf(serverId: string): Space[] {
    return this.spaces[serverId] ?? [];
  }

  membersOf(serverId: string, spaceId: string): Member[] {
    return this.members[ck(serverId, spaceId)] ?? [];
  }

  messagesOf(serverId: string, channelId: string): ChatMessage[] {
    return this.messages[ck(serverId, channelId)] ?? [];
  }

  stickersOf(serverId: string, spaceId: string): StickerMeta[] {
    return this.stickers[ck(serverId, spaceId)] ?? [];
  }

  presenceOf(serverId: string, channelId: string): CallPresence | null {
    return this.presence[ck(serverId, channelId)] ?? null;
  }

  lockedOf(serverId: string, spaceId: string): boolean {
    return this.lockedSpaces[ck(serverId, spaceId)] === true;
  }

  userIdOf(serverId: string): string | null {
    return this.servers.find((s) => s.id === serverId)?.userId ?? null;
  }

  memberName(serverId: string, spaceId: string, userId: string): string {
    return (
      this.membersOf(serverId, spaceId).find((m) => m.user_id === userId)?.name ??
      userId.slice(0, 8)
    );
  }

  signPubOf(serverId: string, spaceId: string, userId: string): Uint8Array | undefined {
    const m = this.membersOf(serverId, spaceId).find((m) => m.user_id === userId);
    return m ? unb64u(m.sign_pub) : undefined;
  }

  private conn(serverId: string): ServerConn | undefined {
    return this.conns.get(serverId);
  }

  // ---------- server view bookkeeping ----------

  private upsertServer(view: ServerView) {
    const i = this.servers.findIndex((s) => s.id === view.id);
    if (i === -1) this.servers = [...this.servers, view];
    else this.servers[i] = view;
  }

  private patchServer(serverId: string, patch: Partial<ServerView>) {
    const i = this.servers.findIndex((s) => s.id === serverId);
    const cur = this.servers[i];
    if (cur) this.servers[i] = { ...cur, ...patch };
  }

  // ---------- lifecycle ----------

  async bootstrap() {
    setSoundPrefs($state.snapshot(this.sounds));
    await migrateLegacyAccount();
    const vault = credentials.loadVault();
    if (vault.length === 0) {
      this.phase = 'onboarding';
      return;
    }
    // Log into every account in parallel; one failure doesn't block the rest.
    await Promise.all(vault.map((acc) => this.connectAccount(acc)));
    this.phase = 'main';
    this.selectFirstAvailable();
  }

  private selectFirstAvailable() {
    if (this.activeServerId && this.activeSpace) return;
    for (const sv of this.servers) {
      const sp = this.spaces[sv.id]?.[0];
      if (sp) {
        void this.selectChannel(sv.id, sp.id, sp.channels[0]?.id ?? null);
        return;
      }
    }
  }

  private async connectAccount(acc: AccountIndex) {
    this.upsertServer({
      id: acc.id,
      serverUrl: acc.serverUrl,
      userId: acc.userId,
      userName: acc.name,
      status: 'connecting',
      error: null,
    });
    const secret = await credentials.getSecret(acc.id);
    if (!secret) {
      this.patchServer(acc.id, { status: 'offline', error: 'missing credentials' });
      return;
    }
    const identity = deserializeIdentity(secret.identity);
    const api = new Api(acc.serverUrl, secret.token);
    const conn = new ServerConn(acc.id, acc.serverUrl, acc.userId, identity, api);
    this.conns.set(acc.id, conn);
    try {
      await this.relogin(conn);
      await this.refreshSpaces(acc.id);
      this.connectSocket(conn);
      this.patchServer(acc.id, { status: 'online' });
    } catch (e) {
      this.patchServer(acc.id, { status: 'offline', error: e instanceof Error ? e.message : String(e) });
    }
  }

  private async relogin(conn: ServerConn) {
    const { nonce } = await conn.api.challenge(conn.userId);
    const { token, user } = await conn.api.login(
      conn.userId,
      nonce,
      loginSignature(conn.identity, nonce),
    );
    conn.api.token = token;
    this.patchServer(conn.id, { userName: user.name });
    await this.persistSecret(conn);
  }

  private async persistSecret(conn: ServerConn) {
    await credentials.setSecret(conn.id, {
      identity: serializeIdentity(conn.identity),
      token: conn.api.token,
    });
  }

  private saveIndex(acc: AccountIndex) {
    const vault = credentials.loadVault();
    const i = vault.findIndex((a) => a.id === acc.id);
    if (i === -1) vault.push(acc);
    else vault[i] = acc;
    credentials.saveVault(vault);
  }

  /** Register a fresh identity on a server and connect. */
  async addServer(serverUrl: string, name: string) {
    const url = serverUrl.replace(/\/+$/, '');
    localStorage.setItem(LS_LAST_SERVER, url);
    const identity = generateIdentity();
    const api = new Api(url);
    const { user_id } = await api.register(name, b64u(identity.signPub), b64u(identity.kemPub));
    await this.attachConn(crypto.randomUUID(), url, user_id, name, identity, api);
  }

  /** Connect to a server using an exported identity backup. */
  async importServer(serverUrl: string, userId: string, identityJson: string) {
    const url = serverUrl.replace(/\/+$/, '');
    localStorage.setItem(LS_LAST_SERVER, url);
    const identity = deserializeIdentity(identityJson);
    const api = new Api(url);
    await this.attachConn(crypto.randomUUID(), url, userId.trim(), userId.trim(), identity, api);
  }

  private async attachConn(
    id: string,
    serverUrl: string,
    userId: string,
    name: string,
    identity: Identity,
    api: Api,
  ) {
    const conn = new ServerConn(id, serverUrl, userId, identity, api);
    this.conns.set(id, conn);
    this.upsertServer({ id, serverUrl, userId, userName: name, status: 'connecting', error: null });
    // Persist identity immediately so a later login failure doesn't lose the key.
    await this.persistSecret(conn);
    this.saveIndex({ id, serverUrl, userId, name });
    await this.relogin(conn);
    this.saveIndex({ id, serverUrl, userId, name: this.servers.find((s) => s.id === id)?.userName ?? name });
    await this.refreshSpaces(id);
    this.connectSocket(conn);
    this.patchServer(id, { status: 'online' });
    if (this.phase !== 'main') this.phase = 'main';
    this.selectFirstAvailable();
  }

  /** Disconnect, forget, and wipe credentials for a server. */
  async removeServer(serverId: string) {
    if (this.call?.serverId === serverId) this.leaveCall();
    const conn = this.conn(serverId);
    conn?.socket?.close();
    this.conns.delete(serverId);

    this.servers = this.servers.filter((s) => s.id !== serverId);
    delete this.spaces[serverId];
    for (const key of Object.keys(this.members)) if (key.startsWith(serverId + '|')) delete this.members[key];
    for (const key of Object.keys(this.messages)) if (key.startsWith(serverId + '|')) delete this.messages[key];
    for (const key of Object.keys(this.stickers)) if (key.startsWith(serverId + '|')) delete this.stickers[key];
    for (const key of Object.keys(this.lockedSpaces)) if (key.startsWith(serverId + '|')) delete this.lockedSpaces[key];
    this.clearPresence(serverId);
    delete this.socketStatus[serverId];
    for (const key of [...this.stickerUrls.keys()]) {
      if (key.startsWith(serverId + '|')) {
        URL.revokeObjectURL(this.stickerUrls.get(key)!);
        this.stickerUrls.delete(key);
      }
    }

    credentials.saveVault(credentials.loadVault().filter((a) => a.id !== serverId));
    await credentials.deleteSecret(serverId);

    if (this.activeServerId === serverId) {
      this.activeServerId = null;
      this.activeSpaceId = null;
      this.activeChannelId = null;
      this.selectFirstAvailable();
    }
    if (this.servers.length === 0) this.phase = 'onboarding';
  }

  // ---------- realtime ----------

  private connectSocket(conn: ServerConn) {
    if (!conn.api.token) return;
    conn.socket?.close();
    const socket = new EventSocket(conn.serverUrl, conn.api.token);
    conn.socket = socket;
    socket.onEvent((ev) => void this.handleEvent(conn.id, ev));
    // The server sends a fresh presence snapshot right after (re)connecting;
    // drop what we knew so calls that ended while we were away disappear.
    socket.onOpen(() => this.clearPresence(conn.id));
    socket.onStatus((st) => {
      if (conn.socket !== socket) return;
      this.socketStatus[conn.id] = st;
      if (st.state === 'open') this.patchServer(conn.id, { status: 'online', error: null });
      else if (st.state === 'reconnecting') this.patchServer(conn.id, { status: 'connecting', error: 'reconnecting…' });
    });
    socket.connect();
  }

  /** Reconnect-banner "Retry now". */
  retryConnection(serverId: string) {
    this.conn(serverId)?.socket?.retryNow();
  }

  private clearPresence(serverId: string) {
    for (const key of Object.keys(this.presence)) if (key.startsWith(serverId + '|')) delete this.presence[key];
  }

  private async handleEvent(serverId: string, ev: ServerEvent) {
    switch (ev.type) {
      case 'call_presence': {
        const key = ck(serverId, ev.channel_id);
        if (ev.participants.length === 0) delete this.presence[key];
        else this.presence[key] = { participants: ev.participants, streaming: ev.streaming ?? [] };
        break;
      }
      case 'message_new': {
        const msg = await this.decryptWire(serverId, ev.channel_id, ev.message);
        const key = ck(serverId, ev.channel_id);
        const list = this.messages[key];
        if (list && !list.some((m) => m.id === msg.id)) list.push(msg);
        break;
      }
      case 'channel_created': {
        const space = this.spaces[serverId]?.find((s) => s.id === ev.space_id);
        if (space && !space.channels.some((c) => c.id === ev.channel.id)) {
          space.channels.push(ev.channel);
        }
        break;
      }
      case 'member_joined':
        await this.refreshMembers(serverId, ev.space_id);
        break;
      case 'member_removed':
        if (ev.user_id === this.userIdOf(serverId)) {
          this.spaces[serverId] = (this.spaces[serverId] ?? []).filter((s) => s.id !== ev.space_id);
          this.conn(serverId)?.spaceKeys.delete(ev.space_id);
          if (this.activeServerId === serverId && this.activeSpaceId === ev.space_id) {
            this.activeSpaceId = null;
            this.activeChannelId = null;
          }
        } else {
          await this.refreshMembers(serverId, ev.space_id);
        }
        break;
      case 'key_request':
        await this.wrapKeysFor(serverId, ev.space_id, ev.user.user_id, ev.user.kem_pub);
        break;
      case 'keys_updated':
        await this.loadSpaceKeys(serverId, ev.space_id);
        break;
      case 'sticker_added': {
        const key = ck(serverId, ev.space_id);
        const list = this.stickers[key];
        if (list && !list.some((s) => s.id === ev.sticker.id)) {
          this.stickers[key] = [...list, ev.sticker];
        }
        break;
      }
      case 'sticker_removed': {
        const key = ck(serverId, ev.space_id);
        const list = this.stickers[key];
        if (list) this.stickers[key] = list.filter((s) => s.id !== ev.sticker_id);
        this.revokeSticker(serverId, ev.sticker_id);
        break;
      }
    }
  }

  // ---------- spaces & members ----------

  async refreshSpaces(serverId: string) {
    const conn = this.conn(serverId);
    if (!conn) return;
    this.spaces[serverId] = (await conn.api.listSpaces()).spaces;
    await Promise.all(
      this.spaces[serverId].map((s) =>
        Promise.all([this.refreshMembers(serverId, s.id), this.loadSpaceKeys(serverId, s.id)]),
      ),
    );
  }

  async refreshMembers(serverId: string, spaceId: string) {
    const conn = this.conn(serverId);
    if (!conn) return;
    this.members[ck(serverId, spaceId)] = (await conn.api.listMembers(spaceId)).members;
  }

  async createSpace(serverId: string, name: string) {
    const conn = this.conn(serverId);
    if (!conn) return;
    const space = await conn.api.createSpace(name);
    // Generate epoch 1 and store our own wrap so other devices/members can follow.
    const key = generateSpaceKey();
    await conn.api.uploadKeys(space.id, 1, [
      { user_id: conn.userId, wrapped: sealBox(conn.identity.kemPub, key) },
    ]);
    conn.spaceKeys.set(space.id, new Map([[1, key]]));
    await this.refreshSpaces(serverId);
    await this.selectChannel(serverId, space.id, space.channels[0]?.id ?? null);
  }

  async acceptInvite(serverId: string, token: string) {
    const conn = this.conn(serverId);
    if (!conn) return;
    const space = await conn.api.acceptInvite(token.trim());
    await this.refreshSpaces(serverId);
    await this.selectChannel(serverId, space.id, space.channels[0]?.id ?? null);
  }

  async createInvite(serverId: string, spaceId: string): Promise<string> {
    const conn = this.conn(serverId);
    if (!conn) throw new Error('server not connected');
    const { token } = await conn.api.createInvite(spaceId);
    return token;
  }

  async createChannel(serverId: string, spaceId: string, name: string) {
    const conn = this.conn(serverId);
    if (!conn) return;
    await conn.api.createChannel(spaceId, name);
    await this.refreshSpaces(serverId);
  }

  async removeMember(serverId: string, spaceId: string, userId: string) {
    const conn = this.conn(serverId);
    if (!conn) return;
    await conn.api.removeMember(spaceId, userId);
    await this.refreshMembers(serverId, spaceId);
    // Rotate the space key so the removed member can't read new messages.
    const space = this.spaces[serverId]?.find((s) => s.id === spaceId);
    const remaining = this.membersOf(serverId, spaceId);
    if (!space) return;
    const newEpoch = space.current_epoch + 1;
    const key = generateSpaceKey();
    await conn.api.uploadKeys(
      spaceId,
      newEpoch,
      remaining.map((m) => ({ user_id: m.user_id, wrapped: sealBox(unb64u(m.kem_pub), key) })),
    );
    space.current_epoch = newEpoch;
    conn.spaceKeys.get(spaceId)?.set(newEpoch, key);
  }

  // ---------- keys ----------

  private async loadSpaceKeys(serverId: string, spaceId: string) {
    const conn = this.conn(serverId);
    if (!conn) return;
    const { current_epoch, wraps } = await conn.api.fetchKeys(spaceId);
    const map = conn.spaceKeys.get(spaceId) ?? new Map<number, Uint8Array>();
    for (const w of wraps) {
      if (map.has(w.epoch)) continue;
      try {
        map.set(w.epoch, openBox(conn.identity, w.wrapped));
      } catch (e) {
        console.warn(`failed to unwrap key epoch ${w.epoch} for space ${spaceId}`, e);
      }
    }
    conn.spaceKeys.set(spaceId, map);
    const space = this.spaces[serverId]?.find((s) => s.id === spaceId);
    if (space) space.current_epoch = current_epoch;
    this.lockedSpaces[ck(serverId, spaceId)] = !map.has(current_epoch);
  }

  private async wrapKeysFor(serverId: string, spaceId: string, userId: string, kemPubB64: string) {
    const conn = this.conn(serverId);
    if (!conn) return;
    const keys = conn.spaceKeys.get(spaceId);
    if (!keys || keys.size === 0) return;
    const kemPub = unb64u(kemPubB64);
    for (const [epoch, key] of keys) {
      // First-wrap-wins on the server, so concurrent members racing is fine.
      await conn.api
        .uploadKeys(spaceId, epoch, [{ user_id: userId, wrapped: sealBox(kemPub, key) }])
        .catch((e) => console.warn('key wrap upload failed', e));
    }
  }

  // ---------- messages ----------

  async selectChannel(serverId: string, spaceId: string, channelId: string | null) {
    this.activeServerId = serverId;
    this.activeSpaceId = spaceId;
    this.activeChannelId = channelId;
    if (channelId && !this.messages[ck(serverId, channelId)]) {
      await this.loadHistory(serverId, channelId);
    }
  }

  private spaceOfChannel(serverId: string, channelId: string): Space | null {
    return this.spaces[serverId]?.find((s) => s.channels.some((c) => c.id === channelId)) ?? null;
  }

  private async decryptWire(serverId: string, channelId: string, wire: WireMessage): Promise<ChatMessage> {
    const conn = this.conn(serverId);
    const space = this.spaceOfChannel(serverId, channelId);
    const base = {
      id: wire.id,
      senderId: wire.sender_id,
      senderName: space ? this.memberName(serverId, space.id, wire.sender_id) : wire.sender_id,
      createdAt: wire.created_at,
    };
    if (!space || !conn) return { ...base, kind: 'text', body: '[unknown space]', ok: false };
    const key = conn.spaceKeys.get(space.id)?.get(wire.epoch);
    const pub = this.signPubOf(serverId, space.id, wire.sender_id);
    if (!key || !pub) return { ...base, kind: 'text', body: '[no key for this message]', ok: false };
    try {
      const body = decryptMessage(pub, key, {
        spaceId: space.id,
        channelId,
        epoch: wire.epoch,
        senderId: wire.sender_id,
      }, wire) as { t: string; body?: string; id?: string };
      if (body.t === 'sticker' && typeof body.id === 'string') {
        return { ...base, kind: 'sticker', body: '', stickerId: body.id, ok: true };
      }
      return {
        ...base,
        kind: 'text',
        body: body.t === 'text' ? (body.body ?? '') : `[${body.t}]`,
        ok: true,
      };
    } catch {
      return { ...base, kind: 'text', body: '[failed to decrypt]', ok: false };
    }
  }

  async loadHistory(serverId: string, channelId: string) {
    const conn = this.conn(serverId);
    if (!conn) return;
    const { messages } = await conn.api.fetchMessages(channelId);
    const decrypted = await Promise.all(messages.map((m) => this.decryptWire(serverId, channelId, m)));
    this.messages[ck(serverId, channelId)] = decrypted.reverse();
  }

  async sendMessage(serverId: string, channelId: string, text: string) {
    const conn = this.conn(serverId);
    if (!conn) return;
    const space = this.spaceOfChannel(serverId, channelId);
    if (!space) return;
    const key = conn.spaceKeys.get(space.id)?.get(space.current_epoch);
    if (!key) {
      this.error = 'Waiting for another member to share the space key.';
      return;
    }
    const enc = encryptMessage(conn.identity, key, {
      spaceId: space.id,
      channelId,
      epoch: space.current_epoch,
      senderId: conn.userId,
    }, { t: 'text', body: text });
    await conn.api.postMessage(channelId, { epoch: space.current_epoch, ...enc });
  }

  // ---------- stickers ----------

  async loadStickers(serverId: string, spaceId: string) {
    const conn = this.conn(serverId);
    if (!conn) return;
    this.stickers[ck(serverId, spaceId)] = (await conn.api.listStickers(spaceId)).stickers;
  }

  /**
   * Decrypt a sticker's webp into an object URL, cached for the session. Uses
   * the sticker's own epoch key (older stickers stay readable after rotation).
   */
  stickerUrl(serverId: string, spaceId: string, stickerId: string): Promise<string | null> {
    const cacheKey = ck(serverId, stickerId);
    const cached = this.stickerUrls.get(cacheKey);
    if (cached) return Promise.resolve(cached);
    let inflight = this.stickerLoads.get(cacheKey);
    if (!inflight) {
      inflight = this.decryptSticker(serverId, spaceId, stickerId).finally(() =>
        this.stickerLoads.delete(cacheKey),
      );
      this.stickerLoads.set(cacheKey, inflight);
    }
    return inflight;
  }

  private async decryptSticker(
    serverId: string,
    spaceId: string,
    stickerId: string,
  ): Promise<string | null> {
    const conn = this.conn(serverId);
    if (!conn) return null;
    try {
      const s = await conn.api.fetchSticker(spaceId, stickerId);
      const key = conn.spaceKeys.get(spaceId)?.get(s.epoch);
      if (!key) return null;
      const bytes = decryptBlob(key, spaceId, s.epoch, s.nonce, s.ct);
      const url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: 'image/webp' }));
      this.stickerUrls.set(ck(serverId, stickerId), url);
      return url;
    } catch (e) {
      console.warn('failed to load sticker', e);
      return null;
    }
  }

  /** Owner only: encrypt a webp under the current space key and upload it. */
  async addSticker(serverId: string, spaceId: string, name: string, bytes: Uint8Array) {
    const conn = this.conn(serverId);
    if (!conn) return;
    const space = this.spaces[serverId]?.find((s) => s.id === spaceId);
    if (!space) return;
    const epoch = space.current_epoch;
    const key = conn.spaceKeys.get(spaceId)?.get(epoch);
    if (!key) throw new Error('no space key available to encrypt the sticker');
    const { nonce, ct } = encryptBlob(key, spaceId, epoch, bytes);
    const meta = await conn.api.createSticker(spaceId, { name, epoch, nonce, ct });
    const cacheKey = ck(serverId, spaceId);
    const list = this.stickers[cacheKey] ?? [];
    if (!list.some((s) => s.id === meta.id)) this.stickers[cacheKey] = [...list, meta];
  }

  async deleteSticker(serverId: string, spaceId: string, stickerId: string) {
    const conn = this.conn(serverId);
    if (!conn) return;
    await conn.api.deleteSticker(spaceId, stickerId);
    const cacheKey = ck(serverId, spaceId);
    const list = this.stickers[cacheKey];
    if (list) this.stickers[cacheKey] = list.filter((s) => s.id !== stickerId);
    this.revokeSticker(serverId, stickerId);
  }

  async sendSticker(serverId: string, channelId: string, stickerId: string) {
    const conn = this.conn(serverId);
    if (!conn) return;
    const space = this.spaceOfChannel(serverId, channelId);
    if (!space) return;
    const key = conn.spaceKeys.get(space.id)?.get(space.current_epoch);
    if (!key) {
      this.error = 'Waiting for another member to share the space key.';
      return;
    }
    const enc = encryptMessage(conn.identity, key, {
      spaceId: space.id,
      channelId,
      epoch: space.current_epoch,
      senderId: conn.userId,
    }, { t: 'sticker', id: stickerId });
    await conn.api.postMessage(channelId, { epoch: space.current_epoch, ...enc });
  }

  private revokeSticker(serverId: string, stickerId: string) {
    const cacheKey = ck(serverId, stickerId);
    const url = this.stickerUrls.get(cacheKey);
    if (url) {
      URL.revokeObjectURL(url);
      this.stickerUrls.delete(cacheKey);
    }
  }

  // ---------- identity backup ----------

  /** Our signing public key on this server (base64url) — safe to share. */
  selfSignPub(serverId: string): string {
    const conn = this.conn(serverId);
    return conn ? b64u(conn.identity.signPub) : '';
  }

  /** Short human-comparable fingerprint of our public keys on this server. */
  selfFingerprint(serverId: string): string {
    const conn = this.conn(serverId);
    return conn ? fingerprint(conn.identity.signPub, conn.identity.kemPub) : '';
  }

  /** Identity backup the user can save to move devices (docs/CRYPTO.md). */
  exportIdentity(serverId: string): string {
    const conn = this.conn(serverId);
    if (!conn) throw new Error('server not connected');
    return JSON.stringify({
      user_id: conn.userId,
      server: conn.serverUrl,
      identity: JSON.parse(serializeIdentity(conn.identity)),
    });
  }

  // ---------- calls ----------

  async joinCall(serverId: string, channelId: string) {
    const conn = this.conn(serverId);
    if (!conn || !conn.socket) return;
    if (this.call) this.leaveCall();
    const space = this.spaceOfChannel(serverId, channelId);
    if (!space) return;

    const turn = await conn.api.turnCredentials();
    const iceServers: RTCIceServer[] = turn.username
      ? [{ urls: turn.urls, username: turn.username, credential: turn.credential }]
      : [{ urls: turn.urls }];

    this.stopMicTest();
    let mic: MicInput;
    try {
      mic = await MicInput.open($state.snapshot(this.voice));
    } catch (e) {
      this.error = `Could not open the microphone: ${e instanceof Error ? e.message : e}`;
      return;
    }
    mic.setMuted(this.micMuted);
    this.attachMicMeter(mic, conn.userId);
    this.mic = mic;

    const mixer = new AudioMixer();
    this.mixer = mixer;
    mixer.setDeafened(this.deafened);
    if (this.voice.outputDeviceId) {
      mixer.setSinkId(this.voice.outputDeviceId).catch((e) => console.warn('output device', e));
    }
    const manager = new CallManager(
      conn.socket,
      conn.identity,
      conn.userId,
      space.id,
      channelId,
      (userId) => this.signPubOf(serverId, space.id, userId),
      iceServers,
      {
        onPeersChanged: (participants) => {
          if (this.call) this.call.participants = participants;
        },
        onRemoteStreams: (userId, streams) => {
          if (!this.call) return;
          // The mixer plays all audio (voice + shared system audio); the UI
          // only renders the video tiles from these same streams.
          const v = this.userVolumes[ck(serverId, userId)];
          if (v !== undefined) this.mixer?.setVolume(userId, v);
          this.mixer?.setUserStreams(userId, streams);
          if (streams.length === 0) delete this.call.remoteStreams[userId];
          else this.call.remoteStreams[userId] = streams;
        },
        onStats: (stats) => {
          if (this.call) this.call.stats = Object.fromEntries(stats);
        },
        onRelayStats: (relay) => {
          if (this.call) this.call.relay = relay;
        },
        onBroadcastChanged: (broadcasting) => {
          if (!this.call) return;
          if (this.call.broadcasting !== broadcasting) (broadcasting ? playStreamStart : playStreamStop)();
          this.call.broadcasting = broadcasting;
        },
        onPeerState: (userId, state) => {
          if (!this.call) return;
          const was = this.call.peerStates[userId]?.streaming ?? false;
          this.call.peerStates[userId] = state;
          if (state.streaming !== was) {
            (state.streaming ? playStreamStart : playStreamStop)();
            if (state.streaming && this.callPrefs.autoWatch) this.call.manager.setWatching(userId, true);
          }
        },
        onPeerLink: (userId, link) => {
          if (this.call) this.call.links[userId] = link;
        },
        onWatchingChanged: (watching) => {
          if (this.call) this.call.watching = watching;
        },
        onPeerJoined: () => playJoin(),
        onPeerLeft: (userId) => {
          playLeave();
          if (this.call) {
            delete this.call.peerStates[userId];
            delete this.call.links[userId];
          }
        },
        onEnded: () => {
          this.call = null;
        },
      },
    );
    manager.settings = this.broadcastSettings;
    this.call = {
      serverId,
      selfId: conn.userId,
      channelId,
      spaceId: space.id,
      manager,
      participants: [conn.userId],
      remoteStreams: {},
      stats: {},
      relay: null,
      broadcasting: false,
      broadcastStarting: false,
      peerStates: {},
      links: {},
      watching: [],
    };
    try {
      await manager.join(mic.stream, { muted: this.micMuted, deafened: this.deafened });
      playJoin();
      void this.refreshDevices();
      this.speakingTimer = setInterval(() => this.pollSpeaking(), 100);
    } catch (e) {
      this.teardownAudio();
      this.call = null;
      this.error = `Could not join call: ${e instanceof Error ? e.message : e}`;
    }
  }

  leaveCall() {
    if (this.call) playLeave();
    this.call?.manager.leave();
    this.teardownAudio();
    this.call = null;
  }

  private teardownAudio() {
    if (this.speakingTimer) clearInterval(this.speakingTimer);
    this.speakingTimer = null;
    this.mixer?.close();
    this.mixer = null;
    this.mic?.close();
    this.mic = null;
    this.micLevel = null;
    this.speaking = {};
  }

  /** Feed the mic meter + our own speaking ring from the mic worklet. */
  private attachMicMeter(mic: MicInput, selfId: string | null) {
    mic.onLevel = (level) => {
      this.micLevel = level;
      if (!selfId) return;
      const talking = level.open && !this.micMuted;
      if (this.speaking[selfId] !== talking) this.speaking[selfId] = talking;
    };
  }

  private pollSpeaking() {
    if (!this.mixer || !this.call || document.hidden) return;
    const now = this.mixer.speaking();
    for (const p of this.call.participants) {
      if (p === this.call.selfId) continue;
      const talking = now.has(p) && !this.deafened;
      if ((this.speaking[p] ?? false) !== talking) this.speaking[p] = talking;
    }
  }

  private announceSelf() {
    prefs.saveSelf({ micMuted: this.micMuted, deafened: this.deafened });
    this.mic?.setMuted(this.micMuted);
    this.mixer?.setDeafened(this.deafened);
    this.call?.manager.setSelfState({ muted: this.micMuted, deafened: this.deafened });
  }

  toggleMic() {
    this.micMuted = !this.micMuted;
    // Unmuting means you want to talk — so you're no longer deafened either.
    if (!this.micMuted && this.deafened) this.deafened = false;
    playToggle(!this.micMuted);
    this.announceSelf();
  }

  /** Deafen mutes everyone you hear and (Discord-style) forces your mic muted. */
  toggleDeafen() {
    this.deafened = !this.deafened;
    if (this.deafened) {
      this.preDeafenMicMuted = this.micMuted;
      this.micMuted = true;
    } else {
      this.micMuted = this.preDeafenMicMuted;
    }
    playToggle(!this.deafened);
    this.announceSelf();
  }

  setPeerVolume(userId: string, volume: number) {
    if (!this.call) return;
    this.userVolumes[ck(this.call.serverId, userId)] = volume;
    prefs.saveVolumes($state.snapshot(this.userVolumes));
    this.mixer?.setVolume(userId, volume);
  }

  peerVolume(userId: string): number {
    if (!this.call) return 1;
    return this.userVolumes[ck(this.call.serverId, userId)] ?? 1;
  }

  // ---------- voice settings ----------

  /** Persist voice prefs and apply them to the live mic/mixer. */
  async applyVoice() {
    const v = $state.snapshot(this.voice);
    prefs.saveVoice(v);
    try {
      await this.mic?.apply(v);
    } catch (e) {
      this.error = `Could not switch microphone: ${e instanceof Error ? e.message : e}`;
    }
  }

  async setOutputDevice(deviceId: string) {
    try {
      await this.mixer?.setSinkId(deviceId);
      this.voice.outputDeviceId = deviceId;
      prefs.saveVoice($state.snapshot(this.voice));
    } catch (e) {
      this.error = `Could not switch audio output: ${e instanceof Error ? e.message : e}`;
    }
  }

  /** Start/stop watching someone's screen share. */
  watch(userId: string, on: boolean) {
    this.call?.manager.setWatching(userId, on);
  }

  saveSounds() {
    const v = $state.snapshot(this.sounds);
    setSoundPrefs(v);
    prefs.saveSounds(v);
  }

  saveCallPrefs() {
    prefs.saveCall($state.snapshot(this.callPrefs));
  }

  saveKeybinds() {
    prefs.saveKeybinds($state.snapshot(this.keybinds));
  }

  /** Open the mic just to show the meter in settings (no-op while in a call). */
  async startMicTest() {
    this.micTestWanted = true;
    if (this.mic) return;
    try {
      const mic = await MicInput.open($state.snapshot(this.voice));
      if (this.mic || this.call || !this.micTestWanted) {
        mic.close(); // settings closed, or a call started meanwhile and owns the mic
        return;
      }
      this.attachMicMeter(mic, null);
      this.mic = mic;
      void this.refreshDevices();
    } catch (e) {
      this.error = `Could not open the microphone: ${e instanceof Error ? e.message : e}`;
    }
  }

  stopMicTest() {
    this.micTestWanted = false;
    if (this.call || !this.mic) return;
    this.mic.close();
    this.mic = null;
    this.micLevel = null;
  }

  async refreshDevices() {
    if (!navigator.mediaDevices?.enumerateDevices) return;
    try {
      const devices = await navigator.mediaDevices.enumerateDevices();
      const pick = (kind: MediaDeviceKind) =>
        devices
          .filter((d) => d.kind === kind && d.deviceId && d.deviceId !== 'default' && d.deviceId !== 'communications')
          .map((d, i) => ({ deviceId: d.deviceId, label: d.label || `Device ${i + 1}` }));
      this.inputDevices = pick('audioinput');
      this.outputDevices = AudioMixer.outputDeviceSelectable() ? pick('audiooutput') : [];
    } catch {
      /* device labels need permission we may lack; leave the lists as they are */
    }
  }

  // ---------- broadcast ----------

  async toggleBroadcast() {
    const call = this.call;
    // While the share picker is open, extra clicks do nothing (a double
    // click must neither start a second capture nor cancel the first).
    if (!call || call.broadcastStarting) return;
    if (call.broadcasting) {
      call.manager.stopBroadcast();
      call.broadcasting = false;
      return;
    }
    call.manager.settings = { ...this.broadcastSettings };
    call.broadcastStarting = true;
    try {
      await call.manager.startBroadcast();
    } catch (e) {
      // Closing the picker is a NotAllowedError/AbortError: not worth a toast.
      const name = e instanceof DOMException ? e.name : '';
      if (name !== 'NotAllowedError' && name !== 'AbortError') {
        this.error = `Could not share screen: ${e instanceof Error ? e.message : e}`;
      }
    } finally {
      call.broadcastStarting = false;
      call.broadcasting = call.manager.isBroadcasting;
    }
  }

  async applyBroadcastSettings() {
    if (!this.call) return;
    this.call.manager.settings = { ...this.broadcastSettings };
    await this.call.manager.applySenderSettings();
  }
}

export const store = new AppStore();
