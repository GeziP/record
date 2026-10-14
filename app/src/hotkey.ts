/** `KeyboardEvent` 里这里用到的部分，方便不依赖 DOM 来测试。 */
export interface KeyLike {
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

/** 与 Rust 端 `Hotkey::parse` 一致：字母、数字、F1–F24、Space。 */
function mainKey(code: string): string | null {
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter) return letter[1] ?? null;
  const digit = /^Digit([0-9])$/.exec(code);
  if (digit) return digit[1] ?? null;
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  if (code === 'Space') return 'Space';
  return null;
}

/**
 * 把一次按键翻译成配置里的写法（如 `Ctrl+Alt+R`，修饰键顺序固定为 Ctrl、Alt、Shift、Win）。
 * 只按了修饰键、没有修饰键、或主键不被支持时返回 `null`，界面继续等下一次按键。
 */
export function hotkeyFromEvent(e: KeyLike): string | null {
  const key = mainKey(e.code);
  if (!key) return null;
  const mods = [e.ctrlKey && 'Ctrl', e.altKey && 'Alt', e.shiftKey && 'Shift', e.metaKey && 'Win'].filter((m): m is string => Boolean(m));
  if (mods.length === 0) return null;
  return [...mods, key].join('+');
}
