/**
 * AudioWorklet: mic input volume + noise gate ("input sensitivity"). Runs on
 * the audio thread, so it keeps working while the window is in the background
 * (timers and rAF get throttled there — exactly when a gamer is talking).
 *
 * Loaded via `?worker&url` so Vite bundles gate.ts into it.
 */
import { NoiseGate, rmsDb, type GateSettings } from './gate';

declare const sampleRate: number;
declare class AudioWorkletProcessor {
  readonly port: MessagePort;
}
declare function registerProcessor(name: string, ctor: unknown): void;

export interface MicGateSettings {
  /** Linear input gain (1 = 100%). */
  gain: number;
  gate: GateSettings;
}

/** What the processor reports to the page, ~20×/s. */
export interface MicLevel {
  /** Peak block level since the last report, dBFS, after input gain. */
  levelDb: number;
  open: boolean;
  thresholdDb: number;
}

const REPORT_MS = 50;
const ATTACK_MS = 5;
const RELEASE_MS = 80;

class MicGateProcessor extends AudioWorkletProcessor {
  private gain = 1;
  private gate = new NoiseGate({ auto: true, thresholdDb: -50 });
  /** Smoothed gate gain (0..1), avoids clicks when opening/closing. */
  private env = 0;
  private sinceReport = 0;
  private peakDb = -100;

  constructor() {
    super();
    this.port.onmessage = (e: MessageEvent<MicGateSettings>) => {
      this.gain = e.data.gain;
      this.gate.settings = { ...e.data.gate };
    };
  }

  process(inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const input = inputs[0];
    const output = outputs[0];
    if (!output) return true;
    const first = input?.[0];
    if (!input || !first) {
      for (const ch of output) ch.fill(0);
      return true;
    }
    const blockMs = (first.length / sampleRate) * 1000;
    // Gain first, so the meter and threshold line up with what peers hear.
    for (let c = 0; c < output.length; c++) {
      const src = input[Math.min(c, input.length - 1)]!;
      const dst = output[c]!;
      for (let i = 0; i < dst.length; i++) dst[i] = src[i]! * this.gain;
    }
    const level = rmsDb(output[0]!);
    const open = this.gate.push(level, blockMs);
    const target = open ? 1 : 0;
    const step = blockMs / (open ? ATTACK_MS : RELEASE_MS);
    const start = this.env;
    this.env = target > start ? Math.min(1, start + step) : Math.max(0, start - step);
    // Linear ramp across the block from the old envelope to the new one.
    for (const dst of output) {
      for (let i = 0; i < dst.length; i++) {
        dst[i]! *= start + ((this.env - start) * i) / dst.length;
      }
    }
    this.peakDb = Math.max(this.peakDb, level);
    this.sinceReport += blockMs;
    if (this.sinceReport >= REPORT_MS) {
      const msg: MicLevel = { levelDb: this.peakDb, open, thresholdDb: this.gate.thresholdDb };
      this.port.postMessage(msg);
      this.sinceReport = 0;
      this.peakDb = -100;
    }
    return true;
  }
}

registerProcessor('mic-gate', MicGateProcessor);
