/**
 * Keyboard shortcuts. A binding is an accelerator string such as
 * "Ctrl+Shift+KeyM": modifiers in a fixed order, then a KeyboardEvent.code.
 * `code` is layout independent (a Korean IME or AZERTY layout still presses
 * KeyM), and it is the key naming Tauri's global-shortcut plugin accepts, so
 * the same string drives both the in-app handler and the OS-wide hotkey.
 */

const MODIFIERS = ['Ctrl', 'Alt', 'Shift', 'Super'] as const;

const MODIFIER_CODES = new Set([
  'ControlLeft', 'ControlRight', 'ShiftLeft', 'ShiftRight',
  'AltLeft', 'AltRight', 'MetaLeft', 'MetaRight', 'OSLeft', 'OSRight',
]);

export function isMac(): boolean {
  return typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform ?? '');
}

/** Discord's defaults: Ctrl+Shift+M / Ctrl+Shift+D (⌘ on macOS). */
export function defaultBinding(action: 'toggleMute' | 'toggleDeafen', mac = isMac()): string {
  const mod = mac ? 'Super' : 'Ctrl';
  return `${mod}+Shift+${action === 'toggleMute' ? 'KeyM' : 'KeyD'}`;
}

interface KeyLike {
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

/** The accelerator for a keydown, or null while only modifiers are held. */
export function eventToAccelerator(e: KeyLike): string | null {
  if (!e.code || MODIFIER_CODES.has(e.code)) return null;
  const mods: string[] = [];
  if (e.ctrlKey) mods.push('Ctrl');
  if (e.altKey) mods.push('Alt');
  if (e.shiftKey) mods.push('Shift');
  if (e.metaKey) mods.push('Super');
  return [...mods, e.code].join('+');
}

/** Normalize a stored accelerator (modifier order/case) so comparisons are exact. */
export function normalize(acc: string): string {
  if (!acc) return '';
  const parts = acc.split('+').map((p) => p.trim()).filter(Boolean);
  const key = parts.pop() ?? '';
  const mods = new Set(
    parts.map((p) => {
      const l = p.toLowerCase();
      if (l === 'control' || l === 'ctrl') return 'Ctrl';
      if (l === 'option' || l === 'alt') return 'Alt';
      if (l === 'cmd' || l === 'command' || l === 'meta' || l === 'super') return 'Super';
      return l === 'shift' ? 'Shift' : p;
    }),
  );
  // Legacy single-letter / digit keys → codes.
  const code = /^[A-Za-z]$/.test(key) ? `Key${key.toUpperCase()}` : /^[0-9]$/.test(key) ? `Digit${key}` : key;
  return [...MODIFIERS.filter((m) => mods.has(m)), code].join('+');
}

export function matches(e: KeyLike, acc: string): boolean {
  return !!acc && eventToAccelerator(e) === normalize(acc);
}

/**
 * A binding with no modifier (or only Shift) would swallow that key in every
 * other app once registered globally — e.g. "M" in chat, in-game.
 */
export function safeForGlobal(acc: string): boolean {
  const mods = normalize(acc).split('+').slice(0, -1);
  const key = normalize(acc).split('+').pop() ?? '';
  return mods.some((m) => m !== 'Shift') || /^F([1-9]|1[0-9]|2[0-4])$/.test(key);
}

const KEY_NAMES: Record<string, string> = {
  Backquote: '`', Minus: '-', Equal: '=', BracketLeft: '[', BracketRight: ']',
  Backslash: '\\', Semicolon: ';', Quote: "'", Comma: ',', Period: '.', Slash: '/',
  Space: 'Space', Escape: 'Esc', ArrowUp: '↑', ArrowDown: '↓', ArrowLeft: '←', ArrowRight: '→',
};

/** Human label, e.g. "Ctrl+Shift+M" / "⌘⇧M". */
export function formatAccelerator(acc: string, mac = isMac()): string {
  if (!acc) return 'Not set';
  const parts = normalize(acc).split('+');
  const code = parts.pop() ?? '';
  const key =
    KEY_NAMES[code] ??
    code.replace(/^Key/, '').replace(/^Digit/, '').replace(/^Numpad/, 'Num ');
  if (mac) {
    const sym: Record<string, string> = { Ctrl: '⌃', Alt: '⌥', Shift: '⇧', Super: '⌘' };
    return parts.map((p) => sym[p] ?? p).join('') + key;
  }
  return [...parts.map((p) => (p === 'Super' ? 'Win' : p)), key].join('+');
}
