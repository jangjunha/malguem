import { describe, expect, it } from 'vitest';
import {
  defaultBinding,
  eventToAccelerator,
  formatAccelerator,
  matches,
  normalize,
  safeForGlobal,
} from './keybinds';

const key = (code: string, mods: Partial<Record<'ctrlKey' | 'altKey' | 'shiftKey' | 'metaKey', boolean>> = {}) => ({
  code,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  metaKey: false,
  ...mods,
});

describe('keybinds', () => {
  it('defaults to Discord bindings', () => {
    expect(defaultBinding('toggleMute', false)).toBe('Ctrl+Shift+KeyM');
    expect(defaultBinding('toggleDeafen', false)).toBe('Ctrl+Shift+KeyD');
    expect(defaultBinding('toggleMute', true)).toBe('Super+Shift+KeyM');
  });

  it('turns a key event into an accelerator, ignoring bare modifiers', () => {
    expect(eventToAccelerator(key('KeyM', { ctrlKey: true, shiftKey: true }))).toBe('Ctrl+Shift+KeyM');
    expect(eventToAccelerator(key('ShiftLeft', { shiftKey: true }))).toBeNull();
    expect(eventToAccelerator(key('F8'))).toBe('F8');
  });

  it('matches regardless of modifier order or legacy spelling', () => {
    expect(normalize('shift+control+m')).toBe('Ctrl+Shift+KeyM');
    expect(matches(key('KeyM', { ctrlKey: true, shiftKey: true }), 'Shift+Ctrl+M')).toBe(true);
    expect(matches(key('KeyM', { ctrlKey: true }), 'Ctrl+Shift+KeyM')).toBe(false);
    expect(matches(key('KeyM', { ctrlKey: true }), '')).toBe(false);
  });

  it('refuses to grab plain keys OS-wide', () => {
    expect(safeForGlobal('KeyM')).toBe(false);
    expect(safeForGlobal('Shift+KeyM')).toBe(false);
    expect(safeForGlobal('Ctrl+Shift+KeyM')).toBe(true);
    expect(safeForGlobal('F9')).toBe(true);
  });

  it('formats for display', () => {
    expect(formatAccelerator('Ctrl+Shift+KeyM', false)).toBe('Ctrl+Shift+M');
    expect(formatAccelerator('Super+Shift+KeyM', true)).toBe('⇧⌘M') // Apple's modifier order: ⌃⌥⇧⌘;
    expect(formatAccelerator('Alt+Backquote', false)).toBe('Alt+`');
    expect(formatAccelerator('', false)).toBe('Not set');
  });
});
