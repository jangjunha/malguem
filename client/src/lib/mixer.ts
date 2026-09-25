/**
 * Local playback mixer for a call. Every remote audio source — a peer's voice
 * and the system/game audio they share — is played through one AudioContext so
 * we can do three things a bare `<audio>` element can't:
 *
 *  - **Per-peer volume, including boost above 100%.** `HTMLMediaElement.volume`
 *    is capped at 1.0, so a peer who is simply too quiet can't be turned *up*.
 *    A GainNode per peer can.
 *  - **Deafen.** One master gain mutes everyone at once.
 *  - **Pick the output device.** Routing call audio to a different device than
 *    the one a system-audio broadcast captures stops our peers' voices from
 *    looping back into that broadcast (see CallPanel's loopback note).
 *
 * Remote WebRTC MediaStreams have historically produced silence through Web
 * Audio unless the stream is *also* attached to a media element, so each source
 * keeps a muted, never-rendered `<audio>` element alive purely as that
 * workaround — all audible output comes from the Web Audio graph.
 */

/** Maximum per-peer playback gain (200%, like Discord's user volume). */
export const MAX_GAIN = 2;

interface PeerSource {
  source: MediaStreamAudioSourceNode;
  /** Keepalive element for the WebRTC→WebAudio silence workaround (muted). */
  el: HTMLAudioElement;
  userId: string;
}

/** Level above which a peer's voice counts as speaking, dBFS. */
const SPEAKING_DB = -50;
/** Keep the speaking ring lit this long after the last loud sample. */
const SPEAKING_HOLD_MS = 300;

interface VoiceMeter {
  streamId: string;
  analyser: AnalyserNode;
  buf: Float32Array<ArrayBuffer>;
  lastLoud: number;
}

/** AudioContext gained an output-device API later than the DOM lib types. */
type SinkContext = AudioContext & { setSinkId?: (id: string) => Promise<void> };

export class AudioMixer {
  private ctx: AudioContext;
  private master: GainNode;
  /** userId → gain feeding the master bus. */
  private peerGains = new Map<string, GainNode>();
  /** userId → volume (0..MAX_GAIN), remembered so it survives reconnects. */
  private volumes = new Map<string, number>();
  /** streamId → live source + keepalive element. */
  private sources = new Map<string, PeerSource>();
  /** userId → analyser on their voice stream (for the speaking indicator). */
  private meters = new Map<string, VoiceMeter>();

  constructor() {
    this.ctx = new AudioContext();
    this.master = this.ctx.createGain();
    this.master.connect(this.ctx.destination);
  }

  private resume() {
    // Autoplay policy starts the context suspended; joining a call is a click,
    // so by the time streams arrive it can be resumed.
    if (this.ctx.state === 'suspended') void this.ctx.resume();
  }

  private peerGain(userId: string): GainNode {
    let gain = this.peerGains.get(userId);
    if (!gain) {
      gain = this.ctx.createGain();
      gain.gain.value = this.volumes.get(userId) ?? 1;
      gain.connect(this.master);
      this.peerGains.set(userId, gain);
    }
    return gain;
  }

  /** Reconcile the streams currently playing for one peer (add new, drop gone). */
  setUserStreams(userId: string, streams: MediaStream[]): void {
    this.resume();
    const wanted = new Set<string>();
    for (const stream of streams) {
      if (stream.getAudioTracks().length === 0) continue; // video-only: nothing to play
      wanted.add(stream.id);
      if (this.sources.has(stream.id)) continue;
      const source = this.ctx.createMediaStreamSource(stream);
      source.connect(this.peerGain(userId));
      const el = new Audio();
      el.srcObject = stream;
      el.muted = true; // workaround only — the Web Audio graph is the audible path
      void el.play().catch(() => {});
      this.sources.set(stream.id, { source, el, userId });
      // A peer's voice stream is the first audio-only stream they send (mic
      // tracks are added when the connection is created, before any share).
      if (!this.meters.has(userId) && stream.getVideoTracks().length === 0) {
        const analyser = this.ctx.createAnalyser();
        analyser.fftSize = 1024;
        source.connect(analyser);
        this.meters.set(userId, {
          streamId: stream.id,
          analyser,
          buf: new Float32Array(analyser.fftSize),
          lastLoud: 0,
        });
      }
    }
    for (const [id, src] of this.sources) {
      if (src.userId === userId && !wanted.has(id)) this.removeSource(id);
    }
  }

  private removeSource(streamId: string): void {
    const src = this.sources.get(streamId);
    if (!src) return;
    src.source.disconnect();
    const meter = this.meters.get(src.userId);
    if (meter?.streamId === streamId) this.meters.delete(src.userId);
    src.el.pause();
    src.el.srcObject = null;
    this.sources.delete(streamId);
  }

  setVolume(userId: string, volume: number): void {
    const v = Math.max(0, Math.min(MAX_GAIN, volume));
    this.volumes.set(userId, v);
    this.peerGain(userId).gain.value = v;
  }

  getVolume(userId: string): number {
    return this.volumes.get(userId) ?? 1;
  }

  /** Peers whose voice is currently audible (polled by the UI, ~10×/s). */
  speaking(): Set<string> {
    const now = performance.now();
    const out = new Set<string>();
    for (const [userId, m] of this.meters) {
      m.analyser.getFloatTimeDomainData(m.buf);
      let sum = 0;
      for (const v of m.buf) sum += v * v;
      const db = 10 * Math.log10(sum / m.buf.length || 1e-10);
      if (db > SPEAKING_DB) m.lastLoud = now;
      if (now - m.lastLoud < SPEAKING_HOLD_MS) out.add(userId);
    }
    return out;
  }

  setDeafened(deafened: boolean): void {
    this.master.gain.value = deafened ? 0 : 1;
  }

  /** Route all call audio to a specific output device (empty = system default). */
  async setSinkId(deviceId: string): Promise<void> {
    const ctx = this.ctx as SinkContext;
    if (typeof ctx.setSinkId !== 'function') {
      throw new Error('this webview cannot pick an audio output device');
    }
    await ctx.setSinkId(deviceId);
    this.resume();
  }

  static outputDeviceSelectable(): boolean {
    return (
      typeof AudioContext !== 'undefined' &&
      typeof (AudioContext.prototype as Partial<SinkContext>).setSinkId === 'function'
    );
  }

  close(): void {
    for (const id of [...this.sources.keys()]) this.removeSource(id);
    this.peerGains.clear();
    void this.ctx.close();
  }
}
