import { useEffect, useState, type ReactNode } from 'react';
import { closeSettings, listDevices, loadSettings, openFolder, pauseHotkeys, resumeHotkeys, saveSettings, type Device, type HotkeyState, type Issue, type Settings } from './api';
import { hotkeyFromEvent } from './hotkey';
import type { Config, Encoder, NotifyMode, SystemCapture } from './types';

const RESOLUTIONS = [
  { label: '1920 × 1080（推荐）', long: 1920, short: 1080 },
  { label: '1280 × 720（文件更小）', long: 1280, short: 720 },
  { label: '2560 × 1440', long: 2560, short: 1440 },
];

const ENCODERS: [Encoder, string][] = [
  ['auto', '自动选择（NVENC → QSV → AMF）'],
  ['h264_nvenc', 'NVIDIA NVENC'],
  ['h264_qsv', 'Intel QSV'],
  ['h264_amf', 'AMD AMF'],
  ['libopenh264', '软件编码（占用 CPU 多，没有硬件编码器时才手动选）'],
];

const SYSTEM_CAPTURES: [SystemCapture, string][] = [
  ['default_endpoint', '默认播放设备（推荐）'],
  ['communications_endpoint', '默认通信播放设备（会议软件常用）'],
];

const HOTKEY_ROWS = [
  { command: 'start', label: '开始录制' },
  { command: 'stop', label: '保存并结束本场' },
  { command: 'quit', label: '保存并退出程序' },
] as const;

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section>
      <h2>{title}</h2>
      {children}
    </section>
  );
}

function Row({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="row">
      <label>{label}</label>
      <div className="control">
        {children}
        {hint && <p className="hint">{hint}</p>}
      </div>
    </div>
  );
}

/** 点进去后按下想用的组合键即可；录入期间暂停全局热键，免得按下旧组合时真的开始或结束录制。 */
function HotkeyRow({ label, value, state, onChange }: { label: string; value: string; state?: HotkeyState; onChange: (hotkey: string) => void }) {
  const [capturing, setCapturing] = useState(false);
  return (
    <Row label={label}>
      <input
        readOnly
        className={capturing ? 'hotkey capturing' : 'hotkey'}
        value={capturing ? '请按下新的组合键…' : value}
        onFocus={() => {
          setCapturing(true);
          void pauseHotkeys();
        }}
        onBlur={() => {
          setCapturing(false);
          void resumeHotkeys();
        }}
        onKeyDown={(e) => {
          e.preventDefault();
          const hotkey = hotkeyFromEvent(e.nativeEvent);
          if (hotkey) {
            onChange(hotkey);
            e.currentTarget.blur(); // 录入完成：失焦后显示新组合，并恢复全局热键
          }
        }}
      />
      {state && (state.ok ? <span className="badge ok">已注册</span> : <span className="badge bad" title={state.error ?? ''}>{state.error ?? '没注册成功'}</span>)}
    </Row>
  );
}

function DeviceSelect({ value, devices, defaultLabel, onChange }: { value: string; devices: Device[]; defaultLabel: string; onChange: (id: string) => void }) {
  const known = value === 'default' || devices.some((d) => d.id === value);
  return (
    <select value={value} onChange={(e) => onChange(e.target.value)}>
      <option value="default">{defaultLabel}</option>
      {devices.map((d) => (
        <option key={d.id} value={d.id}>
          {d.name}
          {d.is_default ? '（当前默认）' : ''}
        </option>
      ))}
      {!known && <option value={value}>{value}（现在找不到这个设备）</option>}
    </select>
  );
}

export function SettingsView() {
  const [data, setData] = useState<Settings | null>(null);
  const [config, setConfig] = useState<Config | null>(null);
  const [devices, setDevices] = useState<Device[]>([]);
  const [hotkeys, setHotkeys] = useState<HotkeyState[]>([]);
  const [issues, setIssues] = useState<Issue[]>([]);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    loadSettings()
      .then((s) => {
        setData(s);
        setConfig(s.config);
        setHotkeys(s.hotkeys);
      })
      .catch((e) => setError(String(e)));
    listDevices()
      .then(setDevices)
      .catch(() => setDevices([]));
  }, []);

  if (error && !config) return <main className="page"><p className="error">读取设置失败：{error}</p></main>;
  if (!data || !config) return <main className="page"><p>正在读取设置…</p></main>;

  const firstRun = data.first_run;
  const patch = (change: Partial<Config>) => setConfig({ ...config, ...change });
  const resolution = RESOLUTIONS.findIndex((r) => r.long === config.video.max_long_edge && r.short === config.video.max_short_edge);

  async function save() {
    if (!config) return;
    setBusy(true);
    setMessage('');
    setError('');
    try {
      const result = await saveSettings(config);
      setIssues(result.issues);
      setHotkeys(result.hotkeys);
      if (result.issues.length > 0) return;
      if (firstRun) {
        await closeSettings();
        return;
      }
      setMessage(result.hotkeys.some((h) => !h.ok) ? '已保存，但有热键没注册成功，见上面的提示。' : '已保存。画面、声音、保护方面的修改从下一场录制开始生效。');
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const hotkeySection = (
    <Section title="热键">
      {HOTKEY_ROWS.map(({ command, label }) => (
        <HotkeyRow key={command} label={label} value={config.hotkeys[command]} state={hotkeys.find((h) => h.command === command)} onChange={(hotkey) => patch({ hotkeys: { ...config.hotkeys, [command]: hotkey } })} />
      ))}
      <p className="hint">点一下输入框，再按下想用的组合键（至少带 Ctrl、Alt、Shift、Win 之一；主键可以是字母、数字、F1–F24 或空格）。热键在任何程序前台时都有效，包括全屏的会议软件。</p>
    </Section>
  );

  const folderSection = (
    <Section title="保存位置">
      <Row label="保存目录" hint={`留空则保存到 ${data.default_output_dir}`}>
        <input type="text" value={config.output_dir ?? ''} placeholder={data.default_output_dir} onChange={(e) => patch({ output_dir: e.target.value })} />
        <button type="button" onClick={() => void openFolder('recordings')}>打开</button>
      </Row>
    </Section>
  );

  const autostartSection = (
    <Section title="开机自启">
      <Row label="登录 Windows 后自动运行">
        <input type="checkbox" checked={config.autostart} onChange={(e) => patch({ autostart: e.target.checked })} />
      </Row>
    </Section>
  );

  return (
    <main className="page">
      <h1>{firstRun ? '欢迎使用 Record' : 'Record 设置'}</h1>
      {data.config_error && <p className="error">配置文件读不懂（{data.config_error}），现在显示的是默认值。保存时会把原文件改名为 config.toml.broken 留底。</p>}
      {firstRun && (
        <div className="intro">
          <p>Record 常驻在系统托盘里，用热键开始和保存会议录制，同时录下屏幕画面、麦克风和对方的声音。先确认下面三件事，之后随时可以从托盘菜单的“设置…”再改。</p>
          <p>录制前请告知与会者并取得同意。Record 没有隐身模式：录制期间托盘图标会变红。</p>
          <p>Windows 11 默认把新图标收在任务栏右下角的“^”里：点开“^”，把 Record 的图标拖到任务栏上，录制状态就能一直看到。</p>
        </div>
      )}
      {issues.length > 0 && (
        <div className="error">
          <p>没有保存，请先改正：</p>
          <ul>{issues.map((i) => <li key={i.key + i.message}>{i.message}</li>)}</ul>
        </div>
      )}
      {error && <p className="error">{error}</p>}

      {folderSection}
      {hotkeySection}

      {!firstRun && (
        <>
          <Section title="画面">
            <Row label="帧率">
              <select value={config.video.fps} onChange={(e) => patch({ video: { ...config.video, fps: Number(e.target.value) } })}>
                {[10, 15, 30].map((fps) => <option key={fps} value={fps}>{fps} 帧/秒{fps === 15 ? '（推荐）' : ''}</option>)}
              </select>
            </Row>
            <Row label="分辨率上限" hint="超过的画面会缩小后再编码；比这个小的不放大。">
              <select value={resolution} onChange={(e) => { const r = RESOLUTIONS[Number(e.target.value)]; if (r) patch({ video: { ...config.video, max_long_edge: r.long, max_short_edge: r.short } }); }}>
                {resolution < 0 && <option value={-1}>自定义（{config.video.max_long_edge} × {config.video.max_short_edge}）</option>}
                {RESOLUTIONS.map((r, i) => <option key={r.label} value={i}>{r.label}</option>)}
              </select>
            </Row>
            <Row label="视频编码器">
              <select value={config.video.encoder} onChange={(e) => patch({ video: { ...config.video, encoder: e.target.value as Encoder } })}>
                {ENCODERS.map(([id, label]) => <option key={id} value={id}>{label}</option>)}
              </select>
            </Row>
            <Row label="录哪块显示器">
              <select value={String(config.monitor)} onChange={(e) => patch({ monitor: e.target.value === 'foreground' ? 'foreground' : Number(e.target.value) })}>
                <option value="foreground">按下热键时前台窗口所在的显示器</option>
                {[1, 2, 3, 4].map((n) => <option key={n} value={String(n)}>显示器 {n}</option>)}
              </select>
            </Row>
          </Section>

          <Section title="声音">
            <Row label="麦克风">
              <DeviceSelect value={config.audio.mic_device} devices={devices.filter((d) => d.flow === 'capture')} defaultLabel="系统默认麦克风" onChange={(id) => patch({ audio: { ...config.audio, mic_device: id } })} />
            </Row>
            <Row label="系统声音（对方的声音）" hint="从这个播放设备回环采集；戴蓝牙耳机开会时，播放设备就是耳机。">
              <DeviceSelect value={config.audio.system_device} devices={devices.filter((d) => d.flow === 'render')} defaultLabel="系统默认播放设备" onChange={(id) => patch({ audio: { ...config.audio, system_device: id } })} />
            </Row>
            <Row label="系统声音的采集方案">
              <select value={config.audio.system_capture} onChange={(e) => patch({ audio: { ...config.audio, system_capture: e.target.value as SystemCapture } })}>
                {SYSTEM_CAPTURES.map(([id, label]) => <option key={id} value={id}>{label}</option>)}
              </select>
            </Row>
          </Section>

          <Section title="提示方式">
            <Row label="气泡通知" hint="两种模式下，出错、自动保存、热键没注册成功都会提示。">
              <select value={config.notify.mode} onChange={(e) => patch({ notify: { mode: e.target.value as NotifyMode } })}>
                <option value="quiet">安静：只有托盘图标变化，出错时才弹提示</option>
                <option value="standard">标准：另外在开始和保存时弹提示</option>
              </select>
            </Row>
          </Section>

          <Section title="自动保护">
            <Row label="单场最长（小时）" hint="到时间自动保存并结束。">
              <input type="number" min={0.5} max={24} step={0.5} value={config.limits.max_hours} onChange={(e) => patch({ limits: { ...config.limits, max_hours: Number(e.target.value) } })} />
            </Row>
            <Row label="磁盘剩余下限（GB）" hint="开始前和录制中剩余空间低于它，就不开始或自动保存。">
              <input type="number" min={1} max={1000} step={1} value={config.limits.min_free_gb} onChange={(e) => patch({ limits: { ...config.limits, min_free_gb: Number(e.target.value) } })} />
            </Row>
          </Section>
        </>
      )}

      {autostartSection}

      <footer>
        <button type="button" className="primary" disabled={busy} onClick={() => void save()}>{firstRun ? '完成' : '保存'}</button>
        {!firstRun && <button type="button" onClick={() => void closeSettings()}>关闭</button>}
        {!firstRun && <button type="button" onClick={() => void openFolder('logs')}>打开日志文件夹</button>}
        {message && <span className="message">{message}</span>}
      </footer>
      <p className="hint">版本 {data.version} · 配置文件 {data.config_path}</p>
    </main>
  );
}
