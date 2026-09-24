/**
 * P2P mesh call engine. One RTCPeerConnection per remote participant; voice
 * and screen share travel over the same connection as separate tracks.
 * Negotiation uses the "perfect negotiation" pattern (polite peer = the
 * lexicographically smaller user id), so adding/removing the screen-share
 * track mid-call renegotiates safely in both directions.
 *
 * All signaling payloads are signed with the sender's identity key and
 * verified against the space's pinned member keys (docs/CRYPTO.md).
 */
import type { Identity } from './crypto';
import { canonicalJson, signSignal, verifySignal } from './crypto';
import { TrackDecoder, TrackEncoder, type RelayCodecConfig } from './relay/codec';
import { RelayHub, type HubStats, type StreamInfo } from './relay/hub';
import type { EventSocket, ServerEvent } from './ws';

export interface BroadcastSettings {
  /** Preferred codec; 'auto' lets the browser negotiate. */
  codec: 'auto' | 'H264' | 'VP9' | 'AV1' | 'H265';
  maxBitrateKbps: number;
  height: number; // capture scaled to this height, 0 = native
  frameRate: number;
  /** Capture system (game) audio with the screen. */
  systemAudio: boolean;
  /**
   * How the screen video reaches viewers. 'webrtc': a separate encode per
   * viewer over the mesh (upload and encoder load grow with every viewer).
   * 'relay' (experimental): encoded once with WebCodecs and relayed viewer to
   * viewer as encoded bytes (see lib/relay). System audio stays on the mesh.
   */
  transport: 'webrtc' | 'relay';
  /** Upload this participant offers for relaying others' broadcasts, Mb/s. */
  relayUploadMbps: number;
}

export const DEFAULT_BROADCAST: BroadcastSettings = {
  codec: 'auto',
  maxBitrateKbps: 8000,
  height: 1080,
  frameRate: 60,
  systemAudio: true,
  transport: 'webrtc',
  relayUploadMbps: 20,
};

export interface PeerStats {
  rttMs: number | null;
  outKbps: number;
  inKbps: number;
  outFps: number | null;
  inFps: number | null;
  encoder: string | null;
  qualityLimitation: string | null;
  jitterBufferMs: number | null;
  transport: 'direct' | 'relay' | null;
}

interface SignalPayload {
  kind: 'sdp' | 'ice';
  description?: RTCSessionDescriptionInit;
  candidate?: RTCIceCandidateInit | null;
}

interface Peer {
  pc: RTCPeerConnection;
  polite: boolean;
  makingOffer: boolean;
  ignoreOffer: boolean;
  streams: Map<string, MediaStream>;
  prev: Map<string, { bytes: number; ts: number; jbDelay: number; jbCount: number }>;
}

interface VisibleTrack {
  readyState: MediaStreamTrackState;
  muted: boolean;
}
interface VisibleStream {
  getTracks(): unknown[];
  getVideoTracks(): VisibleTrack[];
}

/**
 * Which of a peer's streams to show/play right now.
 *
 * When a peer stops a screen share the sender removes the track and the m-line
 * goes inactive, which mutes the receiver's video track (it does *not* end — the
 * transceiver is reused). Without this filter the last frame freezes on screen
 * forever, and a re-share arrives as a second stream that stacks a duplicate
 * tile. So a stream with video is shown only while it has a live, unmuted video
 * track; audio-only voice streams are always kept.
 */
export function visibleStreams<T extends VisibleStream>(streams: T[]): T[] {
  return streams.filter((s) => {
    if (s.getTracks().length === 0) return false;
    const video = s.getVideoTracks();
    if (video.length === 0) return true; // voice
    return video.some((t) => t.readyState === 'live' && !t.muted);
  });
}

export interface CallCallbacks {
  onPeersChanged: (participants: string[]) => void;
  onRemoteStreams: (userId: string, streams: MediaStream[]) => void;
  onStats: (stats: Map<string, PeerStats>) => void;
  onBroadcastChanged: (broadcasting: boolean) => void;
  /** Another participant joined the call (fires only for changes after our own join). */
  onPeerJoined: (userId: string) => void;
  /** Another participant left the call. */
  onPeerLeft: (userId: string) => void;
  onEnded: () => void;
  /** Relay engine snapshot, once per second (relay tree, per-link state). */
  onRelayStats?: (stats: HubStats) => void;
}

export class CallManager {
  private peers = new Map<string, Peer>();
  private micStream: MediaStream | null = null;
  private screenStream: MediaStream | null = null;
  private statsTimer: ReturnType<typeof setInterval> | null = null;
  private unsubscribe: (() => void) | null = null;
  private reopenUnsub: (() => void) | null = null;
  private joined = false;
  /** Other participants seen in the last roster, for join/leave chimes. */
  private lastRoster = new Set<string>();
  /** Skip chimes on the very first roster (the people already present at join). */
  private rosterKnown = false;
  /** Relayed-broadcast engine; attached to every peer connection. */
  private relay: RelayHub | null = null;
  private relayEncoder: TrackEncoder | null = null;
  /** broadcasterId → decoder for the relayed stream we are watching. */
  private relayDecoders = new Map<string, { streamId: number; dec: TrackDecoder }>();
  /** What the mesh carries for our broadcast: the whole screen stream, or only its audio in relay mode. */
  private meshBroadcast: MediaStream | null = null;
  settings: BroadcastSettings = { ...DEFAULT_BROADCAST };

  constructor(
    private socket: EventSocket,
    private identity: Identity,
    private myId: string,
    private spaceId: string,
    private channelId: string,
    private getSignPub: (userId: string) => Uint8Array | undefined,
    private iceServers: RTCIceServer[],
    private cb: CallCallbacks,
  ) {}

  async join(): Promise<void> {
    this.micStream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
    });
    this.relay = new RelayHub(
      {
        myId: this.myId,
        uploadCapacityKbps: this.settings.relayUploadMbps * 1000,
        log: (m) => console.debug('[relay]', m),
      },
      {
        onStreams: (list) => this.onRelayStreams(list),
        onFrame: (from, f) => {
          if (!f.audio) this.relayDecoders.get(from)?.dec.decode(f);
        },
        onKeyframeRequest: () => this.relayEncoder?.requestKeyframe(),
      },
    );
    this.unsubscribe = this.socket.onEvent((ev) => this.handleEvent(ev));
    // A reconnect drops us from the server-side roster; re-join on reopen.
    this.reopenUnsub = this.socket.onOpen(() =>
      this.socket.send({ type: 'call_join', channel_id: this.channelId }),
    );
    this.socket.send({ type: 'call_join', channel_id: this.channelId });
    this.joined = true;
    this.statsTimer = setInterval(() => void this.collectStats(), 1000);
  }

  leave(): void {
    this.joined = false;
    this.socket.send({ type: 'call_leave', channel_id: this.channelId });
    this.unsubscribe?.();
    this.reopenUnsub?.();
    if (this.statsTimer) clearInterval(this.statsTimer);
    this.relayEncoder?.stop();
    this.relayEncoder = null;
    for (const { dec } of this.relayDecoders.values()) dec.close();
    this.relayDecoders.clear();
    this.relay?.dispose();
    this.relay = null;
    for (const [id, peer] of this.peers) {
      peer.pc.close();
      this.cb.onRemoteStreams(id, []);
    }
    this.peers.clear();
    this.stopTracks(this.micStream);
    this.stopTracks(this.screenStream);
    this.micStream = null;
    this.screenStream = null;
    this.cb.onEnded();
  }

  setMicMuted(muted: boolean): void {
    for (const t of this.micStream?.getAudioTracks() ?? []) t.enabled = !muted;
  }

  get isBroadcasting(): boolean {
    return this.screenStream !== null;
  }

  get localScreen(): MediaStream | null {
    return this.screenStream;
  }

  /** Start the screen broadcast with the current sender-side settings. */
  async startBroadcast(): Promise<void> {
    if (this.screenStream) return;
    const s = this.settings;
    const video: MediaTrackConstraints = { frameRate: { ideal: s.frameRate, max: s.frameRate } };
    if (s.height > 0) video.height = { ideal: s.height, max: s.height };
    const options: DisplayMediaStreamOptions & Record<string, unknown> = {
      video,
      audio: s.systemAudio
        ? { echoCancellation: false, noiseSuppression: false, autoGainControl: false }
        : false,
      // Chromium extensions: prefer full monitors and include system audio.
      systemAudio: s.systemAudio ? 'include' : 'exclude',
      monitorTypeSurfaces: 'include',
    };
    const stream = await navigator.mediaDevices.getDisplayMedia(options);
    for (const track of stream.getVideoTracks()) {
      // Bias the encoder toward keeping frame rate up under load — games are motion.
      track.contentHint = 'motion';
      track.onended = () => this.stopBroadcast(); // user hit the browser/OS "stop sharing"
    }
    this.screenStream = stream;
    this.meshBroadcast = stream;
    if (s.transport === 'relay' && this.relay) {
      try {
        await this.startRelayEncoder();
        // Only the system audio rides the mesh; video goes through the relay tree.
        this.meshBroadcast = stream.getAudioTracks().length ? new MediaStream(stream.getAudioTracks()) : null;
      } catch (e) {
        // e.g. WebKit without MediaStreamTrackProcessor: fall back to the mesh.
        console.warn('relay broadcast unavailable, using WebRTC mesh', e);
        this.relayEncoder?.stop();
        this.relayEncoder = null;
      }
    }
    if (this.meshBroadcast) {
      for (const peer of this.peers.values()) this.addStreamTracks(peer, this.meshBroadcast);
    }
    await this.applySenderSettings();
    this.cb.onBroadcastChanged(true);
  }

  /** (Re)start the WebCodecs encoder for the current screen and announce the stream. */
  private async startRelayEncoder(): Promise<void> {
    const video = this.screenStream?.getVideoTracks()[0];
    if (!video || !this.relay) return;
    this.relayEncoder?.stop();
    const s = this.settings;
    const relay = this.relay;
    const enc = new TrackEncoder(
      video,
      { fps: s.frameRate, bitrateKbps: s.maxBitrateKbps, now: () => performance.timeOrigin + performance.now() },
      (f) => relay.sendFrame(f),
    );
    this.relayEncoder = enc;
    const info = await enc.start();
    console.info('[relay] encoder', info); // which codec, hardware or not, temporal layers or not
    relay.startBroadcast(s.maxBitrateKbps, info);
    // Frames encoded before the announce were dropped; restart the chain.
    enc.requestKeyframe();
  }

  stopBroadcast(): void {
    const stream = this.screenStream;
    if (!stream) return;
    this.screenStream = null;
    this.meshBroadcast = null;
    if (this.relayEncoder) {
      this.relayEncoder.stop();
      this.relayEncoder = null;
      this.relay?.stopBroadcast();
    }
    for (const peer of this.peers.values()) {
      for (const sender of peer.pc.getSenders()) {
        if (sender.track && stream.getTracks().includes(sender.track)) {
          peer.pc.removeTrack(sender); // fires negotiationneeded
        }
      }
    }
    this.stopTracks(stream);
    this.cb.onBroadcastChanged(false);
  }

  /** Re-apply codec/bitrate/fps caps to all live senders. */
  async applySenderSettings(): Promise<void> {
    const s = this.settings;
    this.relay?.setUploadCapacity(s.relayUploadMbps * 1000);
    const screenVideo = this.screenStream?.getVideoTracks()[0];
    if (screenVideo) {
      const before = screenVideo.getSettings();
      await screenVideo
        .applyConstraints({
          frameRate: { ideal: s.frameRate, max: s.frameRate },
          ...(s.height > 0 ? { height: { ideal: s.height, max: s.height } } : {}),
        })
        .catch(() => {});
      const enc = this.relayEncoder;
      if (enc?.info) {
        const after = screenVideo.getSettings();
        if (after.height !== before.height || after.width !== before.width || s.frameRate !== enc.fps) {
          await this.startRelayEncoder(); // size/rate is fixed per encoder: new stream
        } else if (s.maxBitrateKbps !== enc.bitrateKbps) {
          enc.setBitrate(s.maxBitrateKbps);
          this.relay?.setBroadcastBitrate(s.maxBitrateKbps);
        }
      }
    }
    for (const peer of this.peers.values()) {
      for (const tr of peer.pc.getTransceivers()) {
        const sender = tr.sender;
        if (!sender.track || sender.track !== screenVideo) continue;
        if (s.codec !== 'auto' && typeof tr.setCodecPreferences === 'function') {
          const codecs = RTCRtpReceiver.getCapabilities('video')?.codecs ?? [];
          const want = codecs.filter((c) => c.mimeType.toLowerCase() === `video/${s.codec.toLowerCase()}`);
          const rest = codecs.filter((c) => !want.includes(c));
          if (want.length > 0) tr.setCodecPreferences([...want, ...rest]);
        }
        const params = sender.getParameters();
        params.encodings = params.encodings?.length ? params.encodings : [{}];
        for (const enc of params.encodings) {
          enc.maxBitrate = s.maxBitrateKbps * 1000;
        }
        (params as { degradationPreference?: string }).degradationPreference = 'maintain-framerate';
        await sender.setParameters(params).catch(() => {});
      }
    }
  }

  /**
   * Upload needed = per-viewer bitrate × viewers we feed directly: every
   * viewer over the mesh, only our relay-tree children in relay mode.
   */
  estimatedUploadKbps(): number {
    const tree = this.relayEncoder ? this.relay?.stats().outgoing?.tree : null;
    const direct = tree ? Object.values(tree).filter((p) => p === this.myId).length : this.peers.size;
    return this.settings.maxBitrateKbps * Math.max(direct, 1);
  }

  /** Whether our current broadcast goes through the relay tree. */
  get relayBroadcasting(): boolean {
    return this.relayEncoder !== null;
  }

  private onRelayStreams(list: StreamInfo[]): void {
    const live = new Map(list.map((s) => [s.broadcasterId, s]));
    for (const [id, d] of this.relayDecoders) {
      const s = live.get(id);
      if (!s || s.streamId !== d.streamId) {
        d.dec.close();
        this.relayDecoders.delete(id);
        this.emitStreams(id);
      }
    }
    for (const s of list) {
      if (s.broadcasterId === this.myId || this.relayDecoders.has(s.broadcasterId)) continue;
      try {
        const dec = new TrackDecoder(s.config as RelayCodecConfig, () => {});
        this.relayDecoders.set(s.broadcasterId, { streamId: s.streamId, dec });
      } catch (e) {
        console.warn('cannot decode relayed stream', e);
        continue;
      }
      // Everyone watches, as with the mesh today (opt-in watching can come later).
      this.relay?.watch(s.broadcasterId);
      this.emitStreams(s.broadcasterId);
    }
  }

  /** Tell the UI/mixer which streams of a participant to show/play. */
  private emitStreams(userId: string): void {
    const peer = this.peers.get(userId);
    const mesh = peer ? visibleStreams([...peer.streams.values()]) : [];
    const relayed = this.relayDecoders.get(userId)?.dec.stream;
    this.cb.onRemoteStreams(userId, relayed ? [...mesh, relayed] : mesh);
  }

  // ---------- internals ----------

  private stopTracks(stream: MediaStream | null) {
    for (const t of stream?.getTracks() ?? []) t.stop();
  }

  private handleEvent(ev: ServerEvent) {
    if (!('channel_id' in ev) || ev.channel_id !== this.channelId) return;
    switch (ev.type) {
      case 'call_roster': {
        if (!this.joined) break;
        // Presence chimes: diff this roster against the last one. The first
        // roster establishes a baseline silently so we don't play a join sound
        // for everyone who was already in the call when we arrived.
        const others = new Set(ev.participants.filter((p) => p !== this.myId));
        if (this.rosterKnown) {
          for (const id of others) if (!this.lastRoster.has(id)) this.cb.onPeerJoined(id);
          for (const id of this.lastRoster) if (!others.has(id)) this.cb.onPeerLeft(id);
        }
        this.lastRoster = others;
        this.rosterKnown = true;

        for (const userId of ev.participants) {
          if (userId !== this.myId && !this.peers.has(userId)) {
            this.createPeer(userId);
          }
        }
        // Drop peers no longer present.
        for (const [id, peer] of this.peers) {
          if (!ev.participants.includes(id)) {
            this.relay?.detachPeer(id);
            peer.pc.close();
            this.peers.delete(id);
            this.cb.onRemoteStreams(id, []);
          }
        }
        this.cb.onPeersChanged(ev.participants);
        break;
      }
      case 'call_peer_left': {
        const peer = this.peers.get(ev.user_id);
        if (peer) {
          this.relay?.detachPeer(ev.user_id);
          peer.pc.close();
          this.peers.delete(ev.user_id);
          this.cb.onRemoteStreams(ev.user_id, []);
        }
        break;
      }
      case 'signal':
        void this.handleSignal(ev.from, ev.payload as SignalPayload, ev.sig);
        break;
    }
  }

  private createPeer(userId: string): Peer {
    const pc = new RTCPeerConnection({ iceServers: this.iceServers });
    const peer: Peer = {
      pc,
      polite: this.myId < userId,
      makingOffer: false,
      ignoreOffer: false,
      streams: new Map(),
      prev: new Map(),
    };
    this.peers.set(userId, peer);

    if (this.micStream) this.addStreamTracks(peer, this.micStream);
    if (this.meshBroadcast) this.addStreamTracks(peer, this.meshBroadcast);
    // Pre-negotiated relay data channels ride this same connection.
    this.relay?.attachPeer(userId, pc);

    pc.onnegotiationneeded = async () => {
      try {
        peer.makingOffer = true;
        await pc.setLocalDescription();
        this.sendSignal(userId, { kind: 'sdp', description: pc.localDescription!.toJSON() });
      } catch (e) {
        console.warn('negotiation failed', e);
      } finally {
        peer.makingOffer = false;
      }
    };
    pc.onicecandidate = ({ candidate }) => {
      this.sendSignal(userId, { kind: 'ice', candidate: candidate?.toJSON() ?? null });
    };
    pc.ontrack = ({ track, streams }) => {
      const refresh = () => this.refreshStreams(userId, peer);
      for (const s of streams) {
        if (!peer.streams.has(s.id)) {
          peer.streams.set(s.id, s);
          // A re-share can move a track between streams; recompute on either.
          s.addEventListener('removetrack', refresh);
          s.addEventListener('addtrack', refresh);
        }
      }
      // mute/unmute fire when the sender stops/resumes sending this track;
      // ended fires if the transceiver is torn down. All change visibility.
      track.addEventListener('mute', refresh);
      track.addEventListener('unmute', refresh);
      track.addEventListener('ended', refresh);
      refresh();
    };
    pc.onconnectionstatechange = () => {
      if (pc.connectionState === 'failed') {
        // ICE restart; perfect negotiation handles the rest.
        pc.restartIce();
      }
    };
    return peer;
  }

  /** Recompute the streams a peer should be showing and notify the UI/mixer. */
  private refreshStreams(userId: string, peer: Peer) {
    for (const [id, s] of peer.streams) {
      if (s.getTracks().length === 0) peer.streams.delete(id); // fully gone
    }
    this.emitStreams(userId);
  }

  private addStreamTracks(peer: Peer, stream: MediaStream) {
    for (const track of stream.getTracks()) {
      peer.pc.addTrack(track, stream);
    }
    if (stream === this.screenStream) void this.applySenderSettings();
  }

  private sendSignal(to: string, payload: SignalPayload) {
    // Canonical (sorted-key) JSON so the signature survives the server's
    // re-serialization of the payload (which reorders object keys).
    const payloadJson = canonicalJson(payload);
    const sig = signSignal(this.identity, this.spaceId, this.channelId, this.myId, to, payloadJson);
    this.socket.send({ type: 'signal', channel_id: this.channelId, to, payload, sig });
  }

  private async handleSignal(from: string, payload: SignalPayload, sig: string) {
    const pub = this.getSignPub(from);
    if (!pub) return;
    // The payload was re-serialized (and key-reordered) by the server; verify
    // against the same canonical form the sender signed.
    const payloadJson = canonicalJson(payload);
    if (!verifySignal(pub, sig, this.spaceId, this.channelId, from, this.myId, payloadJson)) {
      console.warn(`rejected signal with bad signature from ${from}`);
      return;
    }
    const peer = this.peers.get(from) ?? this.createPeer(from);
    const { pc } = peer;
    try {
      if (payload.kind === 'sdp' && payload.description) {
        const desc = payload.description;
        const collision = desc.type === 'offer' && (peer.makingOffer || pc.signalingState !== 'stable');
        peer.ignoreOffer = !peer.polite && collision;
        if (peer.ignoreOffer) return;
        await pc.setRemoteDescription(desc);
        if (desc.type === 'offer') {
          await pc.setLocalDescription();
          this.sendSignal(from, { kind: 'sdp', description: pc.localDescription!.toJSON() });
        }
      } else if (payload.kind === 'ice') {
        try {
          await pc.addIceCandidate(payload.candidate ?? undefined);
        } catch (e) {
          if (!peer.ignoreOffer) throw e;
        }
      }
    } catch (e) {
      console.warn('signal handling failed', e);
    }
  }

  private async collectStats() {
    const out = new Map<string, PeerStats>();
    for (const [userId, peer] of this.peers) {
      const stats: PeerStats = {
        rttMs: null,
        outKbps: 0,
        inKbps: 0,
        outFps: null,
        inFps: null,
        encoder: null,
        qualityLimitation: null,
        jitterBufferMs: null,
        transport: null,
      };
      let report: RTCStatsReport;
      try {
        report = await peer.pc.getStats();
      } catch {
        continue;
      }
      const now = performance.now();
      report.forEach((s) => {
        if (s.type === 'candidate-pair' && s.state === 'succeeded' && s.nominated) {
          stats.rttMs = s.currentRoundTripTime != null ? s.currentRoundTripTime * 1000 : stats.rttMs;
          const local = report.get(s.localCandidateId);
          if (local) stats.transport = local.candidateType === 'relay' ? 'relay' : 'direct';
        }
        if (s.type === 'outbound-rtp' && s.kind === 'video') {
          stats.outFps = s.framesPerSecond ?? stats.outFps;
          stats.encoder = s.encoderImplementation ?? stats.encoder;
          stats.qualityLimitation = s.qualityLimitationReason ?? stats.qualityLimitation;
          const prev = peer.prev.get(s.id);
          if (prev) stats.outKbps += Math.max(0, ((s.bytesSent - prev.bytes) * 8) / (now - prev.ts));
          peer.prev.set(s.id, { bytes: s.bytesSent, ts: now, jbDelay: 0, jbCount: 0 });
        }
        if (s.type === 'inbound-rtp' && s.kind === 'video') {
          stats.inFps = s.framesPerSecond ?? stats.inFps;
          const prev = peer.prev.get(s.id);
          if (prev) {
            stats.inKbps += Math.max(0, ((s.bytesReceived - prev.bytes) * 8) / (now - prev.ts));
            const dCount = (s.jitterBufferEmittedCount ?? 0) - prev.jbCount;
            const dDelay = (s.jitterBufferDelay ?? 0) - prev.jbDelay;
            if (dCount > 0) stats.jitterBufferMs = (dDelay / dCount) * 1000;
          }
          peer.prev.set(s.id, {
            bytes: s.bytesReceived,
            ts: now,
            jbDelay: s.jitterBufferDelay ?? 0,
            jbCount: s.jitterBufferEmittedCount ?? 0,
          });
        }
      });
      out.set(userId, stats);
    }
    this.cb.onStats(out);
    if (this.relay) this.cb.onRelayStats?.(this.relay.stats());
  }
}
