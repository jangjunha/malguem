/**
 * WebCodecs glue for the relayed broadcast: encode a screen track once, and
 * decode relayed frames back into a MediaStreamTrack for the existing tiles.
 *
 * Temporal scalability (L1T3) is what lets relays shed load without
 * re-encoding, so encoder selection prefers any codec that offers it with
 * hardware acceleration. Next comes plain (L1T1) hardware H.264 — the
 * streamer's CPU is busy running the game, and a software encoder there is
 * the very cost this transport exists to remove — and only then software
 * L1T3. Without layers a congested relay link can only drop whole
 * dependency chains and wait for a keyframe.
 */

export interface RelayCodecConfig {
  /** WebCodecs codec string, e.g. 'avc1.640033', 'vp09.00.40.08', 'av01.0.08M.08'. */
  codec: string;
  width: number;
  height: number;
  /** Encoder produced temporal layers; if false relays can't shed load. */
  svc: boolean;
  hardware: boolean;
}

interface Candidate {
  codec: string;
  svc: boolean;
  hw: HardwareAcceleration;
  extra?: Partial<VideoEncoderConfig>;
}

const CANDIDATES: Candidate[] = [
  { codec: 'av01.0.08M.08', svc: true, hw: 'prefer-hardware' },
  { codec: 'avc1.640033', svc: true, hw: 'prefer-hardware', extra: { avc: { format: 'annexb' } } },
  { codec: 'vp09.00.40.08', svc: true, hw: 'prefer-hardware' },
  { codec: 'avc1.640033', svc: false, hw: 'prefer-hardware', extra: { avc: { format: 'annexb' } } },
  { codec: 'vp09.00.40.08', svc: true, hw: 'prefer-software' },
  { codec: 'vp8', svc: true, hw: 'prefer-software' },
];

/** Pick the best encoder configuration this webview supports. */
export async function chooseEncoder(
  width: number,
  height: number,
  fps: number,
  bitrateKbps: number,
): Promise<{ config: VideoEncoderConfig; info: RelayCodecConfig }> {
  // Most encoders want even dimensions.
  width -= width % 2;
  height -= height % 2;
  for (const c of CANDIDATES) {
    const config: VideoEncoderConfig = {
      codec: c.codec,
      width,
      height,
      framerate: fps,
      bitrate: bitrateKbps * 1000,
      bitrateMode: 'variable',
      latencyMode: 'realtime',
      hardwareAcceleration: c.hw,
      ...(c.svc ? { scalabilityMode: 'L1T3' } : {}),
      ...(c.extra ?? {}),
    };
    try {
      const res = await VideoEncoder.isConfigSupported(config);
      if (res.supported) {
        return { config, info: { codec: c.codec, width, height, svc: c.svc, hardware: c.hw === 'prefer-hardware' } };
      }
    } catch {
      /* try the next one */
    }
  }
  throw new Error('no usable video encoder');
}

export interface EncodedOut {
  key: boolean;
  tl: number;
  data: Uint8Array;
  mediaTs: number;
  captureTs: number;
}

/**
 * Encodes a video track. Emits at least `minFps` frames even when the screen
 * is static (Chromium only delivers captured frames on change), so new
 * viewers get a keyframe promptly and relays can tell "idle" from "dead".
 */
export class TrackEncoder {
  private encoder: VideoEncoder | null = null;
  private reader: ReadableStreamDefaultReader<VideoFrame> | null = null;
  private last: VideoFrame | null = null;
  private lastEncodeAt = 0;
  private forceKey = true;
  private idleTimer: ReturnType<typeof setInterval> | null = null;
  private captureTimes = new Map<number, number>();
  private stopped = false;
  info: RelayCodecConfig | null = null;

  constructor(
    private track: MediaStreamTrack,
    private opts: { fps: number; bitrateKbps: number; minFps?: number; now: () => number },
    private onFrame: (f: EncodedOut) => void,
  ) {}

  async start(): Promise<RelayCodecConfig> {
    const settings = this.track.getSettings();
    const width = settings.width ?? 1920;
    const height = settings.height ?? 1080;
    const { config, info } = await chooseEncoder(width, height, this.opts.fps, this.opts.bitrateKbps);
    this.info = info;
    this.encoder = new VideoEncoder({
      output: (chunk, meta) => {
        const data = new Uint8Array(chunk.byteLength);
        chunk.copyTo(data);
        const captureTs = this.captureTimes.get(chunk.timestamp) ?? this.opts.now();
        this.captureTimes.delete(chunk.timestamp);
        this.onFrame({
          key: chunk.type === 'key',
          // `svc` isn't in TypeScript's DOM lib yet.
          tl: (meta as { svc?: { temporalLayerId: number } } | undefined)?.svc?.temporalLayerId ?? 0,
          data,
          mediaTs: chunk.timestamp,
          captureTs,
        });
      },
      error: (e) => console.warn('relay encoder error', e),
    });
    this.encoder.configure(config);

    const Processor = (globalThis as unknown as { MediaStreamTrackProcessor?: new (o: { track: MediaStreamTrack }) => { readable: ReadableStream<VideoFrame> } })
      .MediaStreamTrackProcessor;
    if (!Processor) throw new Error('MediaStreamTrackProcessor unavailable in this webview');
    this.reader = new Processor({ track: this.track }).readable.getReader();
    void this.pump();
    const minFps = this.opts.minFps ?? 4;
    this.idleTimer = setInterval(() => {
      if (this.last && performance.now() - this.lastEncodeAt > 1000 / minFps) this.encode(this.last.clone());
    }, 1000 / minFps / 2);
    return info;
  }

  get fps(): number {
    return this.opts.fps;
  }

  get bitrateKbps(): number {
    return this.opts.bitrateKbps;
  }

  requestKeyframe(): void {
    this.forceKey = true;
  }

  setBitrate(kbps: number): void {
    this.opts.bitrateKbps = kbps;
    // WebCodecs has no live bitrate change; reconfigure (next frame is a keyframe).
    if (this.encoder && this.info) {
      this.encoder.configure({
        codec: this.info.codec,
        width: this.info.width,
        height: this.info.height,
        framerate: this.opts.fps,
        bitrate: kbps * 1000,
        bitrateMode: 'variable',
        latencyMode: 'realtime',
        hardwareAcceleration: this.info.hardware ? 'prefer-hardware' : 'prefer-software',
        ...(this.info.svc ? { scalabilityMode: 'L1T3' } : {}),
        ...(this.info.codec.startsWith('avc1') ? { avc: { format: 'annexb' } } : {}),
      });
      this.forceKey = true;
    }
  }

  stop(): void {
    this.stopped = true;
    if (this.idleTimer) clearInterval(this.idleTimer);
    void this.reader?.cancel().catch(() => {});
    this.last?.close();
    this.last = null;
    if (this.encoder && this.encoder.state !== 'closed') this.encoder.close();
  }

  private async pump(): Promise<void> {
    while (!this.stopped && this.reader) {
      const { value, done } = await this.reader.read();
      if (done || !value) break;
      this.last?.close();
      this.last = value.clone();
      this.encode(value);
    }
  }

  private encode(frame: VideoFrame): void {
    const enc = this.encoder;
    if (!enc || enc.state !== 'configured') {
      frame.close();
      return;
    }
    // Encoder falling behind (e.g. software fallback on a busy machine): drop
    // frames at the source rather than building latency.
    if (enc.encodeQueueSize > 2 && !this.forceKey) {
      frame.close();
      return;
    }
    // Media timestamps must strictly increase, even for idle re-encodes.
    const ts = Math.max(Math.round(performance.now() * 1000), this.lastTs + 1);
    this.lastTs = ts;
    const f = new VideoFrame(frame, { timestamp: ts });
    frame.close();
    this.captureTimes.set(ts, this.opts.now());
    if (this.captureTimes.size > 120) this.captureTimes.delete(this.captureTimes.keys().next().value!);
    enc.encode(f, { keyFrame: this.forceKey });
    this.forceKey = false;
    this.lastEncodeAt = performance.now();
    f.close();
  }

  private lastTs = 0;
}

/**
 * Decodes relayed frames into a MediaStream (video track) that can be shown
 * like any other remote stream.
 */
export class TrackDecoder {
  readonly stream: MediaStream;
  private decoder: VideoDecoder;
  private writer: WritableStreamDefaultWriter<VideoFrame> | null = null;
  private canvas: HTMLCanvasElement | null = null;
  private ctx: CanvasRenderingContext2D | null = null;
  private needKey = true;
  decoded = 0;
  errors = 0;

  constructor(
    private info: RelayCodecConfig,
    private onNeedKeyframe: () => void,
  ) {
    const Generator = (globalThis as unknown as { MediaStreamTrackGenerator?: new (o: { kind: 'video' }) => MediaStreamTrack & { writable: WritableStream<VideoFrame> } })
      .MediaStreamTrackGenerator;
    if (Generator) {
      const track = new Generator({ kind: 'video' });
      this.writer = track.writable.getWriter();
      this.stream = new MediaStream([track]);
    } else {
      // WebKit: no track generator on the main thread; paint to a canvas.
      this.canvas = document.createElement('canvas');
      this.canvas.width = info.width;
      this.canvas.height = info.height;
      this.ctx = this.canvas.getContext('2d');
      this.stream = this.canvas.captureStream();
    }
    this.decoder = this.makeDecoder();
  }

  private makeDecoder(): VideoDecoder {
    const d = new VideoDecoder({
      output: (frame) => {
        this.decoded++;
        if (this.writer) {
          void this.writer.write(frame).catch(() => frame.close());
        } else {
          this.ctx?.drawImage(frame, 0, 0, this.info.width, this.info.height);
          frame.close();
        }
      },
      error: (e) => {
        console.warn('relay decoder error', e);
        this.errors++;
        this.needKey = true;
        this.decoder = this.makeDecoder();
        this.onNeedKeyframe();
      },
    });
    d.configure({ codec: this.info.codec, codedWidth: this.info.width, codedHeight: this.info.height, optimizeForLatency: true });
    return d;
  }

  decode(f: { key: boolean; data: Uint8Array; mediaTs: number }): void {
    if (this.needKey && !f.key) return;
    this.needKey = false;
    if (this.decoder.state !== 'configured') return;
    // Don't let a slow decoder build a backlog: skip non-key frames when behind.
    if (this.decoder.decodeQueueSize > 3 && !f.key) return;
    this.decoder.decode(new EncodedVideoChunk({ type: f.key ? 'key' : 'delta', timestamp: f.mediaTs, data: f.data }));
  }

  close(): void {
    if (this.decoder.state !== 'closed') this.decoder.close();
    void this.writer?.close().catch(() => {});
    for (const t of this.stream.getTracks()) t.stop();
  }
}
