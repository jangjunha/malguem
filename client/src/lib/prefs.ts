/**
 * Per-device user preferences (voice, keybinds, per-user volumes), persisted
 * in localStorage. Nothing here is secret. Every read tolerates missing or
 * malformed data so a bad value can never keep the app from starting.
 */
import type { GateSettings } from './gate';
import { defaultBinding } from './keybinds';

export interface VoicePrefs {
  /** Mic device ('' = system default). */
  inputDeviceId: string;
  /** Call audio output device ('' = system default). */
  outputDeviceId: string;
  /** Mic input volume, linear (1 = 100%, up to 2). */
  inputGain: number;
  /** Input sensitivity (noise gate). */
  gate: GateSettings;
  echoCancellation: boolean;
  noiseSuppression: boolean;
  autoGainControl: boolean;
}

export interface KeybindPrefs {
  /** Accelerators like "Ctrl+Shift+KeyM" (see lib/keybinds; '' = unbound). */
  toggleMute: string;
  toggleDeafen: string;
  /** Register as OS-wide shortcuts while in a call (desktop app), so they work in-game. */
  global: boolean;
}

export const DEFAULT_VOICE: VoicePrefs = {
  inputDeviceId: '',
  outputDeviceId: '',
  inputGain: 1,
  gate: { auto: true, thresholdDb: -50 },
  echoCancellation: true,
  noiseSuppression: true,
  autoGainControl: true,
};

/** Discord's defaults. */
export const DEFAULT_KEYBINDS: KeybindPrefs = {
  toggleMute: defaultBinding('toggleMute'),
  toggleDeafen: defaultBinding('toggleDeafen'),
  global: true,
};

const K_VOICE = 'malguem.voice';
const K_KEYS = 'malguem.keybinds';
const K_VOLUMES = 'malguem.userVolumes';
const K_SELF = 'malguem.selfState';

function read<T extends object>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return structuredClone(fallback);
    const parsed = JSON.parse(raw) as Partial<T>;
    if (typeof parsed !== 'object' || parsed === null) return structuredClone(fallback);
    // Shallow merge (one level deeper for nested objects) so new fields get defaults.
    const out = structuredClone(fallback) as Record<string, unknown>;
    for (const [k, v] of Object.entries(parsed)) {
      const def = out[k];
      if (def === undefined) continue;
      if (typeof def === 'object' && def !== null && typeof v === 'object' && v !== null) {
        out[k] = { ...def, ...v };
      } else if (typeof v === typeof def) {
        out[k] = v;
      }
    }
    return out as T;
  } catch {
    return structuredClone(fallback);
  }
}

function write(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* storage full or blocked: prefs just won't persist */
  }
}

export const prefs = {
  loadVoice: (): VoicePrefs => read(K_VOICE, DEFAULT_VOICE),
  saveVoice: (v: VoicePrefs) => write(K_VOICE, v),
  loadKeybinds: (): KeybindPrefs => read(K_KEYS, DEFAULT_KEYBINDS),
  saveKeybinds: (v: KeybindPrefs) => write(K_KEYS, v),
  /** `serverId|userId` → playback volume. */
  loadVolumes: (): Record<string, number> => {
    const v: Record<string, number> = {};
    try {
      const raw = JSON.parse(localStorage.getItem(K_VOLUMES) ?? '{}') as Record<string, unknown>;
      for (const [k, n] of Object.entries(raw)) if (typeof n === 'number' && Number.isFinite(n)) v[k] = n;
    } catch {
      /* malformed: start fresh */
    }
    return v;
  },
  saveVolumes: (v: Record<string, number>) => write(K_VOLUMES, v),
  loadSelf: (): { micMuted: boolean; deafened: boolean } =>
    read(K_SELF, { micMuted: false, deafened: false }),
  saveSelf: (v: { micMuted: boolean; deafened: boolean }) => write(K_SELF, v),
};
