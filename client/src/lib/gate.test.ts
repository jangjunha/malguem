import { describe, expect, it } from 'vitest';
import { ALWAYS_OPEN_DB, AUTO_MIN_DB, NoiseGate, rmsDb } from './gate';

const BLOCK_MS = 128 / 48; // one 128-frame render quantum at 48 kHz

function run(gate: NoiseGate, levelDb: number, ms: number): boolean {
  let open = gate.open;
  for (let t = 0; t < ms; t += BLOCK_MS) open = gate.push(levelDb, BLOCK_MS);
  return open;
}

describe('rmsDb', () => {
  it('measures a full-scale square wave as 0 dBFS and silence as the floor', () => {
    expect(rmsDb(new Float32Array([1, -1, 1, -1]))).toBeCloseTo(0);
    expect(rmsDb(new Float32Array(128))).toBe(-100);
    expect(rmsDb(new Float32Array([0.1, -0.1]))).toBeCloseTo(-20);
  });
});

describe('NoiseGate (manual)', () => {
  it('opens above the threshold and holds briefly after it drops', () => {
    const g = new NoiseGate({ auto: false, thresholdDb: -40 });
    expect(run(g, -60, 100)).toBe(false);
    expect(run(g, -30, 20)).toBe(true);
    // Gaps between words don't chop speech...
    expect(run(g, -60, 150)).toBe(true);
    // ...but real silence closes it.
    expect(run(g, -60, 300)).toBe(false);
  });

  it('the lowest threshold means always transmitting', () => {
    const g = new NoiseGate({ auto: false, thresholdDb: ALWAYS_OPEN_DB });
    expect(run(g, -100, 50)).toBe(true);
  });
});

describe('NoiseGate (auto)', () => {
  it('learns a noisy room and still opens for speech above it', () => {
    const g = new NoiseGate({ auto: true, thresholdDb: -50 });
    // A fan at -45 dB: the gate must settle closed.
    run(g, -45, 10_000);
    expect(g.open).toBe(false);
    expect(g.thresholdDb).toBeGreaterThan(-45);
    // Talking at -25 dB opens it.
    expect(run(g, -25, 20)).toBe(true);
  });

  it('never gates below the absolute floor in a silent room', () => {
    const g = new NoiseGate({ auto: true, thresholdDb: -50 });
    run(g, -100, 5000);
    expect(g.thresholdDb).toBe(AUTO_MIN_DB);
  });

  it('does not learn speech as the noise floor', () => {
    const g = new NoiseGate({ auto: true, thresholdDb: -50 });
    run(g, -70, 3000);
    // A 10 s monologue: syllables at -20 dB with short quiet gaps.
    for (let i = 0; i < 50; i++) {
      run(g, -20, 150);
      run(g, -68, 50);
    }
    expect(g.thresholdDb).toBeLessThan(-50);
    expect(g.open).toBe(true);
  });
});
