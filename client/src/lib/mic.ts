/**
 * Microphone capture pipeline:
 *
 *   getUserMedia(device) → [mic-gate worklet: volume + noise gate] → stream
 *
 * `stream` is the track peers receive. It stays the same object when the input
 * device or the browser processing (echo cancellation etc.) changes — only the
 * source feeding the graph is swapped — so a device switch mid-call needs no
 * renegotiation.
 */
import type { MicGateSettings, MicLevel } from './mic-gate.worklet';
import workletUrl from './mic-gate.worklet.ts?worker&url';
import type { VoicePrefs } from './prefs';

export type { MicLevel };

function constraints(p: VoicePrefs): MediaTrackConstraints {
  return {
    ...(p.inputDeviceId ? { deviceId: { exact: p.inputDeviceId } } : {}),
    echoCancellation: p.echoCancellation,
    noiseSuppression: p.noiseSuppression,
    autoGainControl: p.autoGainControl,
  };
}

/** The part of VoicePrefs that needs a new getUserMedia when it changes. */
function captureKey(p: VoicePrefs): string {
  return JSON.stringify([p.inputDeviceId, p.echoCancellation, p.noiseSuppression, p.autoGainControl]);
}

export class MicInput {
  readonly stream: MediaStream;
  onLevel: ((level: MicLevel) => void) | null = null;

  private ctx: AudioContext;
  private dest: MediaStreamAudioDestinationNode;
  /** Worklet (volume + gate), or a plain GainNode if worklets are unavailable. */
  private proc: AudioWorkletNode | GainNode;
  private raw: MediaStream | null = null;
  private src: MediaStreamAudioSourceNode | null = null;
  private key = '';
  private muted = false;

  private constructor(ctx: AudioContext, proc: AudioWorkletNode | GainNode) {
    this.ctx = ctx;
    this.proc = proc;
    this.dest = ctx.createMediaStreamDestination();
    proc.connect(this.dest);
    this.stream = this.dest.stream;
    if (proc instanceof AudioWorkletNode) {
      proc.port.onmessage = (e: MessageEvent<MicLevel>) => this.onLevel?.(e.data);
    }
  }

  static async open(p: VoicePrefs): Promise<MicInput> {
    const ctx = new AudioContext();
    let proc: AudioWorkletNode | GainNode;
    try {
      await ctx.audioWorklet.addModule(workletUrl);
      proc = new AudioWorkletNode(ctx, 'mic-gate', { numberOfInputs: 1, numberOfOutputs: 1, outputChannelCount: [1] });
    } catch (e) {
      console.warn('mic worklet unavailable; no input sensitivity', e);
      proc = ctx.createGain();
    }
    const mic = new MicInput(ctx, proc);
    try {
      await mic.apply(p);
    } catch (e) {
      mic.close();
      throw e;
    }
    return mic;
  }

  /** Apply prefs; re-acquires the device only when capture settings changed. */
  async apply(p: VoicePrefs): Promise<void> {
    if (this.ctx.state === 'suspended') void this.ctx.resume();
    if (captureKey(p) !== this.key || !this.raw) {
      let raw: MediaStream;
      try {
        raw = await navigator.mediaDevices.getUserMedia({ audio: constraints(p) });
      } catch (e) {
        // A remembered device that's gone (unplugged headset): use the default.
        if (!p.inputDeviceId) throw e;
        raw = await navigator.mediaDevices.getUserMedia({ audio: constraints({ ...p, inputDeviceId: '' }) });
      }
      const old = this.raw;
      this.src?.disconnect();
      this.raw = raw;
      this.src = this.ctx.createMediaStreamSource(raw);
      this.src.connect(this.proc);
      for (const t of old?.getTracks() ?? []) t.stop();
      this.key = captureKey(p);
    }
    if (this.proc instanceof AudioWorkletNode) {
      const msg: MicGateSettings = { gain: p.inputGain, gate: { ...p.gate } };
      this.proc.port.postMessage(msg);
    } else {
      this.proc.gain.value = p.inputGain;
    }
    this.setMuted(this.muted);
  }

  setMuted(muted: boolean): void {
    this.muted = muted;
    for (const t of this.stream.getAudioTracks()) t.enabled = !muted;
  }

  /** Label of the device actually in use. */
  get deviceLabel(): string {
    return this.raw?.getAudioTracks()[0]?.label ?? '';
  }

  close(): void {
    this.onLevel = null;
    this.src?.disconnect();
    for (const t of this.raw?.getTracks() ?? []) t.stop();
    for (const t of this.stream.getTracks()) t.stop();
    this.raw = null;
    void this.ctx.close();
  }
}
