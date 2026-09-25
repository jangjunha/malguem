/**
 * Voice-activity noise gate: the "input sensitivity" of the mic. Pure logic so
 * it can run inside the AudioWorklet (see mic-gate.worklet.ts) and be unit
 * tested here.
 *
 * Fed one level per audio block (dBFS of that block's RMS), it decides whether
 * the mic is "open". Two modes, like Discord:
 *
 *  - manual: open while the level is above a fixed threshold.
 *  - auto: estimate the background noise floor and open when the level stands
 *    clearly above it. The floor is the quietest level seen over the last ~2 s
 *    ("minimum statistics"): speech always has quiet gaps between syllables
 *    where only the room is heard, while a steady fan never dips — so a noisy
 *    room gets learned even if it's loud enough to hold the gate open.
 *
 * Opening is instant; closing waits `holdMs` after the last loud block so the
 * gaps between words don't chop speech.
 */

export interface GateSettings {
  /** Adaptive threshold instead of `thresholdDb`. */
  auto: boolean;
  /** Manual threshold in dBFS (≤ ALWAYS_OPEN_DB = always open). */
  thresholdDb: number;
}

/** How far above the noise floor speech must be in auto mode. */
export const AUTO_MARGIN_DB = 12;
/** Auto threshold never drops below this (dead-silent mics still gate). */
export const AUTO_MIN_DB = -70;
/** Manual threshold at or below this (the slider's left end) = always transmit. */
export const ALWAYS_OPEN_DB = -80;
const HOLD_MS = 300;
/** Minimum-statistics window: SUB_WINDOWS × SUB_WINDOW_MS. */
const SUB_WINDOW_MS = 250;
const SUB_WINDOWS = 8;
/** Floor follows a louder room over about this long. */
const RISE_MS = 1500;

export function rmsDb(samples: Float32Array): number {
  let sum = 0;
  for (let i = 0; i < samples.length; i++) {
    const s = samples[i]!;
    sum += s * s;
  }
  const rms = Math.sqrt(sum / Math.max(1, samples.length));
  return rms > 0 ? Math.max(-100, 20 * Math.log10(rms)) : -100;
}

export class NoiseGate {
  settings: GateSettings;
  /** Estimated background level, dBFS (auto mode). */
  floorDb = -60;
  open = false;
  private heldMs = 0;
  /** Minimum level of each recent sub-window (oldest first) + the current one. */
  private mins: number[] = [];
  private curMin = 100;
  private curMs = 0;

  constructor(settings: GateSettings) {
    this.settings = { ...settings };
  }

  get thresholdDb(): number {
    if (!this.settings.auto) return this.settings.thresholdDb;
    return Math.max(AUTO_MIN_DB, this.floorDb + AUTO_MARGIN_DB);
  }

  /** Feed one block's level; returns whether the gate is open after it. */
  push(levelDb: number, blockMs: number): boolean {
    if (this.settings.auto) this.trackFloor(levelDb, blockMs);
    else if (this.settings.thresholdDb <= ALWAYS_OPEN_DB) return (this.open = true);
    if (levelDb >= this.thresholdDb) {
      this.open = true;
      this.heldMs = 0;
    } else if (this.open) {
      this.heldMs += blockMs;
      if (this.heldMs >= HOLD_MS) this.open = false;
    }
    return this.open;
  }

  private trackFloor(levelDb: number, blockMs: number) {
    this.curMin = Math.min(this.curMin, levelDb);
    this.curMs += blockMs;
    if (this.curMs >= SUB_WINDOW_MS) {
      this.mins.push(this.curMin);
      if (this.mins.length > SUB_WINDOWS) this.mins.shift();
      this.curMin = 100;
      this.curMs = 0;
    }
    // Plain loop: this runs ~375×/s on the audio thread; no per-block garbage.
    let target = this.curMin;
    for (const m of this.mins) if (m < target) target = m;
    if (target < this.floorDb) this.floorDb = target; // quieter: follow at once
    else this.floorDb += (target - this.floorDb) * Math.min(1, blockMs / RISE_MS);
  }
}
