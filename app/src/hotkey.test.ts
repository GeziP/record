import { describe, expect, it } from 'vitest';
import { hotkeyFromEvent, type KeyLike } from './hotkey';

const press = (code: string, mods: Partial<Omit<KeyLike, 'code'>> = {}): KeyLike => ({ code, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...mods });

describe('hotkeyFromEvent', () => {
  it('writes modifiers in a fixed order whatever the order they were pressed in', () => {
    expect(hotkeyFromEvent(press('KeyR', { ctrlKey: true, altKey: true }))).toBe('Ctrl+Alt+R');
    expect(hotkeyFromEvent(press('F12', { shiftKey: true, ctrlKey: true }))).toBe('Ctrl+Shift+F12');
    expect(hotkeyFromEvent(press('Digit9', { metaKey: true, altKey: true }))).toBe('Alt+Win+9');
  });

  it('accepts the same main keys as the Rust side: letters, digits, F1-F24, Space', () => {
    expect(hotkeyFromEvent(press('Space', { ctrlKey: true, altKey: true }))).toBe('Ctrl+Alt+Space');
    expect(hotkeyFromEvent(press('F24', { altKey: true }))).toBe('Alt+F24');
    expect(hotkeyFromEvent(press('F1', { altKey: true }))).toBe('Alt+F1');
    expect(hotkeyFromEvent(press('F25', { altKey: true }))).toBeNull();
    expect(hotkeyFromEvent(press('F0', { altKey: true }))).toBeNull();
  });

  it('keeps waiting while only a modifier is down, or the key is not supported', () => {
    expect(hotkeyFromEvent(press('ControlLeft', { ctrlKey: true }))).toBeNull();
    expect(hotkeyFromEvent(press('AltRight', { altKey: true }))).toBeNull();
    expect(hotkeyFromEvent(press('Enter', { ctrlKey: true }))).toBeNull();
    expect(hotkeyFromEvent(press('Numpad1', { ctrlKey: true }))).toBeNull();
  });

  it('refuses a key without any modifier, which would swallow ordinary typing', () => {
    expect(hotkeyFromEvent(press('KeyR'))).toBeNull();
    expect(hotkeyFromEvent(press('F5'))).toBeNull();
  });
});
