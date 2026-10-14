export type Encoder = 'auto' | 'h264_nvenc' | 'h264_qsv' | 'h264_amf' | 'libopenh264';
export type SystemCapture = 'default_endpoint' | 'communications_endpoint' | 'all_endpoints';
export type NotifyMode = 'quiet' | 'standard';

/**
 * 与 Rust 端的 `Config` 一一对应。这里只写设置界面会改的键；其余（timeline、health、
 * 以后子项目 B、C 的键）在索引签名里原样带回去，保存时不会丢。
 */
export interface Config {
  output_dir?: string | null;
  hotkeys: { start: string; stop: string; quit: string };
  video: { fps: number; max_long_edge: number; max_short_edge: number; encoder: Encoder };
  /** `"foreground"` 或从 1 开始的显示器序号。 */
  monitor: string | number;
  audio: { mic_device: string; system_device: string; system_capture: SystemCapture; [key: string]: unknown };
  notify: { mode: NotifyMode };
  limits: { max_hours: number; min_free_gb: number };
  autostart: boolean;
  [key: string]: unknown;
}
