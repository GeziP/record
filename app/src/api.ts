import { invoke } from '@tauri-apps/api/core';
import type { Config } from './types';

export interface Issue {
  key: string;
  message: string;
}

export interface HotkeyState {
  command: 'start' | 'stop' | 'quit';
  text: string;
  ok: boolean;
  error: string | null;
}

export interface Settings {
  config: Config;
  first_run: boolean;
  config_error: string | null;
  default_output_dir: string;
  hotkeys: HotkeyState[];
  config_path: string;
  version: string;
}

export interface SaveResult {
  issues: Issue[];
  hotkeys: HotkeyState[];
}

export interface Device {
  id: string;
  name: string;
  flow: 'render' | 'capture';
  is_default: boolean;
}

export const loadSettings = () => invoke<Settings>('load_settings');
export const saveSettings = (config: Config) => invoke<SaveResult>('save_settings', { config });
export const listDevices = () => invoke<Device[]>('list_devices');
export const openFolder = (which: 'recordings' | 'logs') => invoke<void>('open_folder', { which });
export const closeSettings = () => invoke<void>('close_settings');
/** 录入热键时先暂停：否则按下已经注册的组合，会真的开始或结束一场录制。 */
export const pauseHotkeys = () => invoke<void>('pause_hotkeys');
export const resumeHotkeys = () => invoke<void>('resume_hotkeys');
