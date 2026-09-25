/**
 * Mute/deafen hotkeys. In the desktop app they are registered OS-wide (Tauri
 * global-shortcut) so they work while a game has focus; otherwise, or if the
 * OS refuses one (another app owns it), they fall back to a keydown listener
 * that only works while our window is focused.
 */
import { matches, normalize, safeForGlobal } from './keybinds';
import { isTauri } from './platform';
import type { KeybindPrefs } from './prefs';

export type HotkeyAction = 'toggleMute' | 'toggleDeafen';
const ACTIONS: HotkeyAction[] = ['toggleMute', 'toggleDeafen'];

export interface HotkeyStatus {
  /** Accelerators currently registered OS-wide. */
  global: string[];
  /** Human-readable problems (e.g. a shortcut taken by another app). */
  errors: string[];
}

export class Hotkeys {
  private registered: string[] = [];
  private binds: KeybindPrefs | null = null;
  /** Serializes sync() calls; registration is async and must not interleave. */
  private queue: Promise<unknown> = Promise.resolve();

  constructor(private run: (action: HotkeyAction) => void) {}

  /** (Re)register for new bindings. Resolves with what actually got registered. */
  sync(binds: KeybindPrefs): Promise<HotkeyStatus> {
    const next = this.queue.then(() => this.doSync({ ...binds }));
    this.queue = next.catch(() => {});
    return next;
  }

  private async doSync(binds: KeybindPrefs): Promise<HotkeyStatus> {
    this.binds = binds;
    const status: HotkeyStatus = { global: [], errors: [] };
    if (!isTauri()) return status;
    const gs = await import('@tauri-apps/plugin-global-shortcut');
    if (this.registered.length) {
      await gs.unregister(this.registered).catch((e) => console.warn('unregister hotkeys', e));
      this.registered = [];
    }
    if (!binds.global) return status;
    for (const action of ACTIONS) {
      const acc = normalize(binds[action]);
      if (!acc || this.registered.includes(acc)) continue;
      if (!safeForGlobal(acc)) {
        status.errors.push(`${acc}: add Ctrl/Alt to use it outside the app`);
        continue;
      }
      try {
        await gs.register(acc, (e) => {
          if (e.state === 'Pressed') this.fire(acc);
        });
        this.registered.push(acc);
      } catch (e) {
        status.errors.push(`${acc}: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
    status.global = [...this.registered];
    return status;
  }

  private fire(acc: string) {
    for (const action of ACTIONS) {
      if (this.binds && normalize(this.binds[action]) === acc) this.run(action);
    }
  }

  /**
   * In-app fallback for keydown. Skips accelerators registered OS-wide: those
   * fire through the global handler even when our window is focused.
   */
  onKeydown(e: KeyboardEvent): boolean {
    if (!this.binds || e.repeat) return false;
    for (const action of ACTIONS) {
      const acc = this.binds[action];
      if (matches(e, acc) && !this.registered.includes(normalize(acc))) {
        e.preventDefault();
        this.run(action);
        return true;
      }
    }
    return false;
  }

  async dispose() {
    await this.queue;
    if (isTauri() && this.registered.length) {
      const gs = await import('@tauri-apps/plugin-global-shortcut');
      await gs.unregister(this.registered).catch(() => {});
    }
    this.registered = [];
  }
}
