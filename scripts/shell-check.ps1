# 外壳（record.exe）的真机检查：热键开始与保存、托盘与通知的决定、退出与崩溃恢复、睡眠与关机事件、
# 启动失败、热键冲突、配置错误、首次向导与设置窗口。
#
# 用法：
#   $env:RECORD_FFMPEG = '<ffmpeg.exe 的完整路径>'
#   powershell -ExecutionPolicy Bypass -File scripts\shell-check.ps1 [-Exe target\debug\record.exe] [-Parts basic,events,failures,ui]
#
# 隔离：被测程序的 APPDATA、LOCALAPPDATA 都指向 %TEMP%\record-shell 下的目录，保存目录也在那里，
# 不碰真实的配置、日志和录音；结束时整个目录删掉。检查过程中会真的录几秒屏幕和声音，
# 但录出来的文件从不打开查看，只用 ffprobe 看流的结构，用完即删。
# 设置窗口的检查借助 WebView2 的远程调试端口（只对这次启动的进程开，端口绑在本机回环地址）。
param(
    [string]$Exe = (Join-Path $PSScriptRoot '..\target\debug\record.exe'),
    [string[]]$Parts = @('basic', 'events', 'failures', 'ui'),
    [int]$CdpPort = 9333,
    [switch]$Keep
)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$Parts = @($Parts | ForEach-Object { $_ -split ',' })   # powershell -File 传进来的 basic,events 是一整个字符串
$Exe = (Resolve-Path $Exe).Path
if (-not $env:RECORD_FFMPEG -or -not (Test-Path $env:RECORD_FFMPEG)) { throw '先把环境变量 RECORD_FFMPEG 设为 ffmpeg.exe 的完整路径' }
$Ffmpeg = (Resolve-Path $env:RECORD_FFMPEG).Path
$Ffprobe = Join-Path (Split-Path $Ffmpeg) 'ffprobe.exe'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ShellNative {
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll", SetLastError = true)] public static extern bool RegisterHotKey(IntPtr hWnd, int id, uint mods, uint vk);
    [DllImport("user32.dll")] public static extern bool UnregisterHotKey(IntPtr hWnd, int id);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindowW(string cls, string title);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
    delegate bool EnumProc(IntPtr h, IntPtr p);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, System.Text.StringBuilder sb, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowTextW(IntPtr h, System.Text.StringBuilder sb, int n);
    [DllImport("user32.dll")] static extern int GetWindowLongW(IntPtr h, int index);
    [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr h, uint cmd);
    // 进程的全部顶层窗口，每个一行："可见|隐藏 宽x高 类名 标题"；会出现在任务栏上的（可见、无所有者、不是工具窗口或明确要求显示）前面加 [任务栏]
    public static string[] WindowsOf(uint pid) {
        var list = new System.Collections.Generic.List<string>();
        EnumWindows((h, p) => {
            uint owner; GetWindowThreadProcessId(h, out owner);
            if (owner == pid) {
                var cls = new System.Text.StringBuilder(256); GetClassNameW(h, cls, 256);
                var title = new System.Text.StringBuilder(256); GetWindowTextW(h, title, 256);
                RECT r; GetWindowRect(h, out r);
                int ex = GetWindowLongW(h, -20);
                bool visible = IsWindowVisible(h);
                bool taskbar = visible && GetWindow(h, 4) == IntPtr.Zero && ((ex & 0x80) == 0 || (ex & 0x40000) != 0);
                list.Add((taskbar ? "[任务栏] " : "") + (visible ? "可见" : "隐藏") + " " + (r.Right - r.Left) + "x" + (r.Bottom - r.Top) + " " + cls + " " + title);
            }
            return true;
        }, IntPtr.Zero);
        return list.ToArray();
    }
}
'@

$results = New-Object System.Collections.Generic.List[object]
function Check([string]$name, [bool]$ok, [string]$detail = '') {
    $results.Add([pscustomobject]@{ 项目 = $name; 结果 = $(if ($ok) { '通过' } else { '失败' }); 说明 = $detail })
    '{0}  {1}  {2}' -f $(if ($ok) { '[通过]' } else { '[失败]' }), $name, $detail
}
function Info([string]$text) { 'INFO ' + $text }

# ───────────── 辅助：隔离环境、启动、日志、按键 ─────────────
$work = Join-Path $env:TEMP 'record-shell'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$runBefore = (Get-ItemProperty $runKey).PSObject.Properties.Name | Where-Object { $_ -notlike 'PS*' } | Sort-Object
$procs = New-Object System.Collections.Generic.List[object]

function New-Sandbox([string]$name) {
    $root = Join-Path $work $name
    $box = [pscustomobject]@{
        Name = $name; Root = $root
        Appdata = Join-Path $root 'appdata'; Local = Join-Path $root 'local'; Out = Join-Path $root 'out'
    }
    foreach ($d in $box.Appdata, $box.Local, $box.Out) { New-Item -ItemType Directory -Force $d | Out-Null }
    $box | Add-Member ConfigFile (Join-Path $box.Appdata 'record\config.toml')
    $box | Add-Member LogDir (Join-Path $box.Local 'record\logs')
    $box
}
function Write-Config($box, [string]$body) {
    New-Item -ItemType Directory -Force (Split-Path $box.ConfigFile) | Out-Null
    [System.IO.File]::WriteAllText($box.ConfigFile, $body, (New-Object System.Text.UTF8Encoding($false)))
}
function Standard-Config($box, [string]$mode = 'standard', [string]$extra = '') {
    "output_dir = '$($box.Out)'`n$extra`n[notify]`nmode = `"$mode`"`n"
}
function Start-App($box, [switch]$Cdp) {
    $saved = @{}
    foreach ($k in 'APPDATA', 'LOCALAPPDATA', 'RECORD_FFMPEG', 'RECORD_LOG', 'WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS') { $saved[$k] = [Environment]::GetEnvironmentVariable($k) }
    $env:APPDATA = $box.Appdata; $env:LOCALAPPDATA = $box.Local; $env:RECORD_FFMPEG = $Ffmpeg
    $env:RECORD_LOG = 'info,record_shell=debug'
    if ($Cdp) { $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$CdpPort" }
    try { $p = Start-Process -FilePath $Exe -PassThru }
    finally { foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) } }
    $procs.Add($p)
    $p
}
function Stop-App($p) { if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force; [void]$p.WaitForExit(5000) } }
function Read-Log($box) {
    if (-not (Test-Path $box.LogDir)) { return @() }
    @(Get-ChildItem $box.LogDir -Filter 'record.log*' | Sort-Object Name | ForEach-Object {
            $fs = [System.IO.File]::Open($_.FullName, 'Open', 'Read', 'ReadWrite')
            try { $sr = New-Object System.IO.StreamReader($fs, [System.Text.Encoding]::UTF8); $sr.ReadToEnd() -split "`r?`n" } finally { $fs.Dispose() }
        } | Where-Object { $_ })
}
function Wait-Log($box, [string]$pattern, [int]$since = 0, [int]$timeoutMs = 5000) {
    $until = [DateTime]::Now.AddMilliseconds($timeoutMs)
    do {
        $hit = @(Read-Log $box | Select-Object -Skip $since | Where-Object { $_ -match $pattern })
        if ($hit.Count -gt 0) { return $hit }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::Now -lt $until)
    return @()
}
function Wait-Until([scriptblock]$cond, [int]$timeoutMs = 10000) {
    $until = [DateTime]::Now.AddMilliseconds($timeoutMs)
    do { if (& $cond) { return $true }; Start-Sleep -Milliseconds 100 } while ([DateTime]::Now -lt $until)
    return $false
}
function Ready($box, $p) {
    $hit = Wait-Log $box '就绪' 0 30000
    ($hit.Count -gt 0) -and -not $p.HasExited
}
$VK = @{ Ctrl = 0x11; Alt = 0x12; Shift = 0x10; R = 0x52; S = 0x53; Q = 0x51; F8 = 0x77; F9 = 0x78 }
function Press-Chord([string[]]$names) {
    $codes = @($names | ForEach-Object { [byte]$VK[$_] })
    foreach ($c in $codes) { [ShellNative]::keybd_event($c, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 30 }
    [array]::Reverse($codes)
    foreach ($c in $codes) { [ShellNative]::keybd_event($c, 0, 2, [UIntPtr]::Zero); Start-Sleep -Milliseconds 30 }
}
function Sessions($box) { @(Get-ChildItem $box.Out -Directory -ErrorAction SilentlyContinue | Where-Object { Test-Path (Join-Path $_.FullName 'session.json') } | Sort-Object Name) }
function Read-Session($dir) { Get-Content (Join-Path $dir 'session.json') -Raw -Encoding UTF8 | ConvertFrom-Json }
function Probe-Streams([string]$mp4) {
    $json = & $Ffprobe -v error -show_entries 'stream=codec_type,codec_name:format=duration' -of json $mp4 | ConvertFrom-Json
    [pscustomobject]@{ Video = @($json.streams | Where-Object { $_.codec_type -eq 'video' }).Count; Audio = @($json.streams | Where-Object { $_.codec_type -eq 'audio' }).Count; Duration = [double]$json.format.duration }
}
function Cpu-Percent($p, [int]$secs) {
    $a = (Get-Process -Id $p.Id).TotalProcessorTime.TotalSeconds
    Start-Sleep -Seconds $secs
    ((Get-Process -Id $p.Id).TotalProcessorTime.TotalSeconds - $a) / $secs * 100
}

# ───────────── 辅助：通过 WebView2 的远程调试端口操作设置窗口 ─────────────
function Connect-Cdp([string]$titleMatch, [int]$timeoutMs = 20000) {
    $until = [DateTime]::Now.AddMilliseconds($timeoutMs)
    do {
        try {
            $t = @(Invoke-RestMethod "http://127.0.0.1:$CdpPort/json" -TimeoutSec 2 | Where-Object { $_.type -eq 'page' -and $_.title -match $titleMatch })
            if ($t.Count -gt 0) {
                $ws = New-Object System.Net.WebSockets.ClientWebSocket
                $ws.ConnectAsync([Uri]$t[0].webSocketDebuggerUrl, [Threading.CancellationToken]::None).Wait()
                return [pscustomobject]@{ Ws = $ws; Id = 0 }
            }
        } catch { }
        Start-Sleep -Milliseconds 300
    } while ([DateTime]::Now -lt $until)
    $null
}
function Cdp-Call($c, [string]$method, $params = @{}) {
    $c.Id++
    $bytes = [Text.Encoding]::UTF8.GetBytes((@{ id = $c.Id; method = $method; params = $params } | ConvertTo-Json -Depth 20 -Compress))
    $c.Ws.SendAsync([ArraySegment[byte]]$bytes, 'Text', $true, [Threading.CancellationToken]::None).Wait()
    while ($true) {
        $sb = New-Object Text.StringBuilder
        do {
            $buf = New-Object byte[] 65536
            $r = $c.Ws.ReceiveAsync([ArraySegment[byte]]$buf, [Threading.CancellationToken]::None).Result
            [void]$sb.Append([Text.Encoding]::UTF8.GetString($buf, 0, $r.Count))
        } while (-not $r.EndOfMessage)
        $msg = $sb.ToString() | ConvertFrom-Json
        if ($msg.id -eq $c.Id) {
            if ($msg.error) { throw "CDP $method 出错：$($msg.error.message)" }
            return $msg.result
        }
    }
}
function Cdp-Eval($c, [string]$js) {
    $r = Cdp-Call $c 'Runtime.evaluate' @{ expression = $js; returnByValue = $true; awaitPromise = $true }
    if ($r.exceptionDetails) { throw "页面脚本出错：$($r.exceptionDetails.exception.description)" }
    $r.result.value
}
function Cdp-Shot($c, [string]$path) {
    $r = Cdp-Call $c 'Page.captureScreenshot' @{ format = 'png' }
    [System.IO.File]::WriteAllBytes($path, [Convert]::FromBase64String($r.data))
}
# 页面里用的小工具：按标签找行、给受控输入框赋值、点按钮
$jsLib = @'
window.__t = {
  row: (label) => [...document.querySelectorAll('.row')].find((r) => r.querySelector('label').innerText.trim() === label),
  set: (el, v) => { const d = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), 'value'); d.set.call(el, v); el.dispatchEvent(new Event(el.tagName === 'SELECT' ? 'change' : 'input', { bubbles: true })); },
  click: (text) => { const b = [...document.querySelectorAll('button')].find((x) => x.innerText.trim() === text); if (!b) throw new Error('没有按钮 ' + text); b.click(); },
  key: (el, code, mods) => el.dispatchEvent(new KeyboardEvent('keydown', Object.assign({ code, bubbles: true, cancelable: true }, mods))),
  text: () => document.body.innerText,
};
'@

Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $work | Out-Null
$free = [math]::Round((Get-PSDrive -Name ($work.Substring(0, 1))).Free / 1GB)

try {
    # ───────────────────────── basic：热键、托盘、通知、退出、崩溃恢复 ─────────────────────────
    if ($Parts -contains 'basic') {
        '=== basic ==='
        $mods = 0x0002 -bor 0x0001 -bor 0x4000
        foreach ($k in @{ R = 0x52; S = 0x53; Q = 0x51 }.GetEnumerator()) {
            $ok = [ShellNative]::RegisterHotKey([IntPtr]::Zero, 100, $mods, $k.Value)
            if ($ok) { [void][ShellNative]::UnregisterHotKey([IntPtr]::Zero, 100) }
            Check "启动前 Ctrl+Alt+$($k.Key) 没被别的程序占用" $ok ''
        }
        $box = New-Sandbox 'basic'
        Write-Config $box (Standard-Config $box 'standard')
        $app = Start-App $box
        Check '进程启动并就绪' (Ready $box $app) ''
        $log = Read-Log $box
        Check '三个热键都注册成功' (@($log | Where-Object { $_ -match '热键已注册' }).Count -eq 3) (($log | Where-Object { $_ -match '热键' }) -join ' | ')
        $wins = [ShellNative]::WindowsOf([uint32]$app.Id)
        # Tauri 和 tao 内部有几个 15×15 的“可见”工具窗口（单实例、事件循环），不会出现在任务栏上；这里按任务栏的规则判断
        Check '没有会出现在任务栏上的窗口（托盘常驻）' (@($wins | Where-Object { $_ -like '`[任务栏`]*' }).Count -eq 0) ($wins -join ' | ')
        # 资源管理器收到图标后才在注册表里留记录，要等一会儿
        $trayNow = { @(Get-ChildItem 'HKCU:\Control Panel\NotifyIconSettings' -ErrorAction SilentlyContinue | ForEach-Object { Get-ItemProperty $_.PSPath } | Where-Object { $_.ExecutablePath -eq $Exe }) }
        [void](Wait-Until { (& $trayNow).Count -gt 0 } 8000)
        $tray = & $trayNow
        Check '托盘图标创建成功' ([bool]($log -match '托盘图标已创建')) ''
        # 资源管理器只在第一次见到这个路径时留记录，清理后重复运行不一定再有，所以只报告不判定
        Info ('资源管理器的托盘登记：' + $(if ($tray.Count) { "有，提示文字 $($tray[0].InitialTooltip)" } else { '没有（重复运行时正常）' }))

        Start-Sleep -Seconds 2
        $p = Get-Process -Id $app.Id
        Info ('待命：工作集 {0:N1} MB，私有内存 {1:N1} MB' -f ($p.WorkingSet64 / 1MB), ($p.PrivateMemorySize64 / 1MB))
        $idleCpu = Cpu-Percent $app 8
        Check '待命时 CPU 低于 1%' ($idleCpu -lt 1) ('{0:N2}%' -f $idleCpu)

        # 热键开始：连按两次（间隔约 0.2 秒）只应开始一场
        $n = (Read-Log $box).Count
        Press-Chord 'Ctrl', 'Alt', 'R'
        Press-Chord 'Ctrl', 'Alt', 'R'
        $icon = Wait-Log $box '托盘图标更新.*Recording' $n 10000
        Check '热键开始 → 托盘变成录制中' ($icon.Count -gt 0) ($icon -join ' ')
        $started = Wait-Log $box '通知.*show=true.*开始录制' $n 3000
        Check '标准模式：开始时弹通知' ($started.Count -gt 0) ($started -join ' ')
        Start-Sleep -Milliseconds 800
        Check '连按两次只开始一场' ((Sessions $box).Count -eq 1) "会话数 $((Sessions $box).Count)"
        $dir = (Sessions $box)[0].FullName
        Check '录制中 session.json 的状态是 recording' ((Read-Session $dir).state -eq 'recording') ''
        Start-Sleep -Seconds 4
        $tips = @(Read-Log $box | Where-Object { $_ -match '托盘提示更新.*录制中' })
        Check '录制中托盘提示随时间更新' ($tips.Count -ge 2) "提示更新 $($tips.Count) 次，最后一条：$($tips[-1])"
        $recCpu = Cpu-Percent $app 6
        Info ('录制中：外壳进程 CPU {0:N1}%（单核口径）' -f $recCpu)

        # 热键保存
        $n = (Read-Log $box).Count
        Press-Chord 'Ctrl', 'Alt', 'S'
        $idle = Wait-Log $box '托盘图标更新.*Standby' $n 20000
        Check '热键保存 → 托盘回到待命' ($idle.Count -gt 0) ''
        $saved = Wait-Log $box '通知.*show=true.*已保存' $n 3000
        Check '标准模式：保存时弹通知' ($saved.Count -gt 0) ($saved -join ' ')
        $meeting = Join-Path $dir 'meeting.mp4'
        Check '生成了 meeting.mp4' (Test-Path $meeting) ''
        $meta = Read-Session $dir
        Check 'session.json：finalized，原因 user' (($meta.state -eq 'finalized') -and ($meta.end_reason -eq 'user')) "$($meta.state) / $($meta.end_reason)"
        if (Test-Path $meeting) {
            $s = Probe-Streams $meeting
            Check '成品里有 1 路画面和 1 路混音，时长 ≥ 8 秒' (($s.Video -eq 1) -and ($s.Audio -eq 1) -and ($s.Duration -ge 8)) ('画面 {0}，声音 {1}，{2:N1} 秒' -f $s.Video, $s.Audio, $s.Duration)
        }

        # 连续再录几场：标准模式下每场的开始与保存都弹系统通知。每场结束时采集线程全部退出，
        # 曾经因此让下一条通知撞上被卸载的组件，进程以 0xC0000005 崩溃（见 record_capture::com）
        $rounds = 5
        $alive = $true
        for ($i = 1; $i -le $rounds -and $alive; $i++) {
            $n = (Read-Log $box).Count
            Press-Chord 'Ctrl', 'Alt', 'R'
            $alive = (Wait-Log $box '托盘图标更新.*Recording' $n 10000).Count -gt 0
            if (-not $alive) { break }
            Start-Sleep -Seconds 2
            $n = (Read-Log $box).Count
            Press-Chord 'Ctrl', 'Alt', 'S'
            $alive = (Wait-Log $box '托盘图标更新.*Standby' $n 20000).Count -gt 0
            Start-Sleep -Milliseconds 400
        }
        Check "标准模式连续再录 $rounds 场（每场开始与保存都弹通知），进程不崩溃" ($alive -and -not $app.HasExited) $(if ($app.HasExited) { "进程已退出，退出码 $($app.ExitCode)" } else { '' })
        $all = @(Sessions $box)
        $unfinished = @($all | Where-Object { (Read-Session $_.FullName).state -ne 'finalized' })
        Check '这几场都正常保存（finalized）' (($all.Count -eq $rounds + 1) -and ($unfinished.Count -eq 0)) "共 $($all.Count) 场，没保存完的 $($unfinished.Count) 场"

        # 热键退出：录制中按 Ctrl+Alt+Q，保存后进程自己退出
        $n = (Read-Log $box).Count
        Press-Chord 'Ctrl', 'Alt', 'R'
        Check '第二场开始' ((Wait-Log $box '托盘图标更新.*Recording' $n 10000).Count -gt 0) ''
        Start-Sleep -Seconds 3
        Press-Chord 'Ctrl', 'Alt', 'Q'
        $exited = $app.WaitForExit(30000)
        Check '录制中按“保存并退出”：保存后进程自己退出' $exited $(if ($exited) { "退出码 $($app.ExitCode)" } else { '30 秒内没退出' })
        $second = (Sessions $box)[-1].FullName
        $meta2 = Read-Session $second
        Check '第二场已保存（finalized，user，有成品）' (($meta2.state -eq 'finalized') -and ($meta2.end_reason -eq 'user') -and (Test-Path (Join-Path $second 'meeting.mp4'))) "$($meta2.state) / $($meta2.end_reason)"

        # 崩溃恢复：安静模式，录制中强行结束进程，再启动
        Write-Config $box (Standard-Config $box 'quiet')
        $app = Start-App $box
        [void](Ready $box $app)
        $n = (Read-Log $box).Count
        Press-Chord 'Ctrl', 'Alt', 'R'
        Check '第三场开始' ((Wait-Log $box '托盘图标更新.*Recording' $n 10000).Count -gt 0) ''
        $quietStart = Wait-Log $box '通知.*show=false.*开始录制' $n 2000
        Check '安静模式：开始时不弹通知' ($quietStart.Count -gt 0) ''
        Start-Sleep -Seconds 4
        $third = (Sessions $box)[-1].FullName
        Stop-Process -Id $app.Id -Force
        [void]$app.WaitForExit(5000)
        Start-Sleep -Seconds 2
        Check '被杀后 session.json 仍是 recording（没来得及收尾）' ((Read-Session $third).state -eq 'recording') ''
        $app = Start-App $box
        Check '重新启动并就绪' (Ready $box $app) ''
        $fixed = Wait-Log $box '通知.*show=true.*已恢复上次中断的录制' 0 60000
        Check '启动时自动修复，安静模式下也弹通知' ($fixed.Count -gt 0) ($fixed -join ' ')
        $meta3 = Read-Session $third
        Check '被杀的那场：recovered，有成品' (($meta3.state -eq 'recovered') -and (Test-Path (Join-Path $third 'meeting.mp4'))) "$($meta3.state)"
        Stop-App $app
    }

    # ───────────────────────── events：睡眠与关机 ─────────────────────────
    if ($Parts -contains 'events') {
        '=== events ==='
        $box = New-Sandbox 'events'
        Write-Config $box (Standard-Config $box 'quiet')
        $app = Start-App $box
        Check '启动并就绪' (Ready $box $app) ''
        $hwnd = [ShellNative]::FindWindowW('RecordSessionEvents', [NullString]::Value)
        Check '找到接收系统事件的隐藏窗口' ($hwnd -ne [IntPtr]::Zero) ''

        # 睡眠（模拟 WM_POWERBROADCAST / PBT_APMSUSPEND）：存下本场，程序继续运行
        $n = (Read-Log $box).Count
        Press-Chord 'Ctrl', 'Alt', 'R'
        Check '开始一场' ((Wait-Log $box '托盘图标更新.*Recording' $n 10000).Count -gt 0) ''
        Start-Sleep -Seconds 3
        $t = [Diagnostics.Stopwatch]::StartNew()
        [void][ShellNative]::SendMessageW($hwnd, 0x218, [IntPtr]4, [IntPtr]::Zero)
        $t.Stop()
        Info ('睡眠通知的处理（含保存）用了 {0:N2} 秒' -f $t.Elapsed.TotalSeconds)
        $dir = (Sessions $box)[-1].FullName
        Check '睡眠：窗口过程返回时本场已保存（原因 sleep，有成品）' ((((Read-Session $dir).end_reason) -eq 'sleep') -and (Test-Path (Join-Path $dir 'meeting.mp4'))) "$((Read-Session $dir).state) / $((Read-Session $dir).end_reason)"
        Check '睡眠：安静模式下也弹“因睡眠已保存”' ((Wait-Log $box '通知.*show=true.*因睡眠已保存' $n 3000).Count -gt 0) ''
        Check '睡眠：程序仍在运行' (-not $app.HasExited) ''
        $n = (Read-Log $box).Count
        Press-Chord 'Ctrl', 'Alt', 'R'
        Check '“唤醒”后热键仍能开始新的一场' ((Wait-Log $box '托盘图标更新.*Recording' $n 10000).Count -gt 0) ''
        Start-Sleep -Seconds 3

        # 关机（模拟 WM_ENDSESSION）：存下本场并退出
        $t = [Diagnostics.Stopwatch]::StartNew()
        [void][ShellNative]::SendMessageW($hwnd, 0x16, [IntPtr]1, [IntPtr]::Zero)
        $t.Stop()
        Info ('关机通知的处理（含保存）用了 {0:N2} 秒' -f $t.Elapsed.TotalSeconds)
        $exited = $app.WaitForExit(20000)
        Check '关机：保存后进程退出' $exited ''
        $dir2 = (Sessions $box)[-1].FullName
        Check '关机：本场已保存（原因 shutdown，有成品）' ((((Read-Session $dir2).end_reason) -eq 'shutdown') -and (Test-Path (Join-Path $dir2 'meeting.mp4'))) "$((Read-Session $dir2).state) / $((Read-Session $dir2).end_reason)"
        Check '关机：先后两场是不同的会话' ($dir -ne $dir2) ''
        Stop-App $app
    }

    # ───────────────────────── failures：启动失败、热键冲突、配置错误 ─────────────────────────
    if ($Parts -contains 'failures') {
        '=== failures ==='
        # 磁盘剩余空间低于下限：不开始，不留会话目录
        if ($free -lt 1000) {
            $box = New-Sandbox 'disk'
            Write-Config $box (Standard-Config $box 'quiet' "[limits]`nmin_free_gb = 1000`nmax_hours = 6.0`n")
            $app = Start-App $box
            [void](Ready $box $app)
            $n = (Read-Log $box).Count
            Press-Chord 'Ctrl', 'Alt', 'R'
            $fail = Wait-Log $box '通知.*show=true.*没能开始录制' $n 15000
            Check '磁盘剩余空间不足：开始失败并弹通知（安静模式也弹）' ($fail.Count -gt 0) ($fail -join ' ')
            Check '失败后没有留下会话目录' ((Sessions $box).Count -eq 0) ''
            Check '失败后托盘仍是待命，热键还能用' ((Wait-Log $box '托盘图标更新.*Recording' $n 1500).Count -eq 0 -and -not $app.HasExited) ''
            Stop-App $app
        } else { Info "本机 $($work.Substring(0, 2)) 盘剩余 $free GB，超过校验上限 1000 GB，跳过“磁盘不足”检查" }

        # 热键被别的程序占着：其余热键照常，冲突的明确提示
        $box = New-Sandbox 'conflict'
        Write-Config $box (Standard-Config $box 'quiet')
        $mods = 0x0002 -bor 0x0001 -bor 0x4000
        $held = [ShellNative]::RegisterHotKey([IntPtr]::Zero, 200, $mods, 0x52)
        Check '（准备）测试进程先占住 Ctrl+Alt+R' $held ''
        $app = Start-App $box
        [void](Ready $box $app)
        $log = Read-Log $box
        Check 'Ctrl+Alt+R 注册失败并写明原因' ([bool]($log -match '热键没注册成功.*Start.*Ctrl\+Alt\+R')) (($log -match '没注册成功') -join ' ')
        Check '另外两个热键照常注册' (@($log | Where-Object { $_ -match '热键已注册' }).Count -eq 2) ''
        $menu = @($log | Where-Object { $_ -match '托盘菜单文字' })
        Check '托盘菜单里写明冲突的热键，其余写着热键' (($menu.Count -ge 1) -and ($menu[-1] -match 'start=开始录制（Ctrl\+Alt\+R 没注册成功，被占用）') -and ($menu[-1] -match 'stop=保存并结束本场（Ctrl\+Alt\+S）')) ($menu[-1])
        Check '热键冲突的通知弹出（安静模式也弹）' ((Wait-Log $box '通知.*show=true.*有热键没注册成功' 0 3000).Count -gt 0) ''
        [void][ShellNative]::UnregisterHotKey([IntPtr]::Zero, 200)
        Stop-App $app

        # 配置里的热键写错：三个都不注册，说明原因，配置文件不被改动
        $box = New-Sandbox 'badkey'
        $body = Standard-Config $box 'quiet' "[hotkeys]`nstart = `"R`"`nstop = `"Ctrl+Alt+S`"`nquit = `"Ctrl+Alt+Q`"`n"
        Write-Config $box $body
        $before = (Get-FileHash $box.ConfigFile).Hash
        $app = Start-App $box
        [void](Ready $box $app)
        $log = Read-Log $box
        Check '热键配置写错：三个都没注册，并说明原因' (@($log | Where-Object { $_ -match 'WARN.*热键没注册成功' }).Count -eq 3 -and [bool]($log -match '没有修饰键')) ''
        $line = @($log | Where-Object { $_ -match '通知.*有热键没注册成功' })
        Check '三个热键同一个原因时，通知里只说一遍' (($line.Count -eq 1) -and (([regex]::Matches($line[0], '没有修饰键')).Count -eq 1)) ($line -join ' ')
        Check '配置文件没有被改动' ((Get-FileHash $box.ConfigFile).Hash -eq $before) ''
        Stop-App $app

        # 配置文件读不懂：按默认值运行，提示用户，原文件原样保留
        $box = New-Sandbox 'badtoml'
        Write-Config $box "hotkeys = [这不是 toml`n"
        $before = (Get-FileHash $box.ConfigFile).Hash
        $app = Start-App $box
        Check '配置读不懂时仍能启动' (Ready $box $app) ''
        $log = Read-Log $box
        Check '弹出“配置文件有误”的通知' ((Wait-Log $box '通知.*show=true.*配置文件有误' 0 3000).Count -gt 0) ''
        Check '按默认热键运行（三个都注册）' (@($log | Where-Object { $_ -match '热键已注册' }).Count -eq 3) ''
        Check '原配置文件原样保留' ((Get-FileHash $box.ConfigFile).Hash -eq $before) ''
        Stop-App $app
    }

    # ───────────────────────── ui：首次向导与设置窗口 ─────────────────────────
    if ($Parts -contains 'ui') {
        '=== ui ==='
        $box = New-Sandbox 'ui'
        # 先占住 Ctrl+Alt+F8，稍后在设置里试“这个组合被别的程序占用”
        [void][ShellNative]::RegisterHotKey([IntPtr]::Zero, 201, (0x0002 -bor 0x0001 -bor 0x4000), 0x77)
        $app = Start-App $box -Cdp
        Check '没有配置文件时能启动' (Ready $box $app) ''
        $c = Connect-Cdp 'Record'
        Check '首次使用：自动打开设置窗口（向导）' ($null -ne $c) ''
        if ($c) {
            [void](Cdp-Call $c 'Emulation.setFocusEmulationEnabled' @{ enabled = $true })
            [void](Cdp-Eval $c $jsLib)
            Check '向导页面加载完成' (Wait-Until { (Cdp-Eval $c "document.querySelector('h1') && document.querySelector('h1').innerText") -eq '欢迎使用 Record' } 10000) ''
            $text = Cdp-Eval $c '__t.text()'
            $sections = (Cdp-Eval $c "[...document.querySelectorAll('h2')].map((h) => h.innerText).join(',')")
            Check '向导只有保存位置、热键、开机自启三项' ($sections -eq '保存位置,热键,开机自启') $sections
            Check '向导里有同意提示和托盘固定提示' (($text -match '取得同意') -and ($text -match '拖到任务栏')) ''
            Check '三个热键都显示“已注册”' ((Cdp-Eval $c "document.querySelectorAll('.badge.ok').length") -eq 3) ''
            Cdp-Shot $c (Join-Path $work 'wizard.png')
            [void](Cdp-Eval $c "__t.click('完成')")
            Check '点“完成”后写入配置文件' (Wait-Until { Test-Path $box.ConfigFile } 8000) ''
            Check '点“完成”后设置窗口关闭' (Wait-Until { @(Invoke-RestMethod "http://127.0.0.1:$CdpPort/json" | Where-Object { $_.type -eq 'page' -and $_.title -match 'Record' }).Count -eq 0 } 8000) ''
            Check '关窗后程序仍在运行，热键仍然有效' (-not $app.HasExited) ''
            $cfg = Get-Content $box.ConfigFile -Raw -Encoding UTF8
            Check '配置里有默认热键和 autostart = false' (($cfg -match 'start = "Ctrl\+Alt\+R"') -and ($cfg -match 'autostart = false')) ''
            $c.Ws.Dispose()
        }

        # 配置文件里有程序不认识的键（以后子项目 B、C 的设置）：重启后，设置界面保存时必须原样保留
        Stop-App $app
        [System.IO.File]::AppendAllText($box.ConfigFile, "`n[speech]`nmodel = `"keep-me`"`n", (New-Object System.Text.UTF8Encoding($false)))
        $app = Start-App $box -Cdp
        Check '有配置文件时重新启动并就绪（不再弹向导）' ((Ready $box $app) -and ($null -eq (Connect-Cdp 'Record' 1500))) ''

        # 再启动一次 = 唤起设置窗口
        $second = Start-Process -FilePath $Exe -PassThru
        $procs.Add($second)
        Check '第二个实例自己退出' ($second.WaitForExit(15000)) ''
        Check '第一个实例记下“又启动了一个实例”' ((Wait-Log $box '又启动了一个实例' 0 3000).Count -gt 0) ''
        $c = Connect-Cdp 'Record'
        Check '第二个实例唤起了设置窗口' ($null -ne $c) ''
        if ($c) {
            [void](Cdp-Call $c 'Emulation.setFocusEmulationEnabled' @{ enabled = $true })
            [void](Cdp-Eval $c $jsLib)
            Check '已有配置：显示完整设置' (Wait-Until { (Cdp-Eval $c "document.querySelector('h1') && document.querySelector('h1').innerText") -eq 'Record 设置' } 10000) ''
            $text = Cdp-Eval $c '__t.text()'
            Check '完整设置里有画面、声音、提示方式、自动保护' (($text -match '画面') -and ($text -match '声音') -and ($text -match '提示方式') -and ($text -match '自动保护')) ''
            $devs = Cdp-Eval $c "document.querySelectorAll('select')[4].options.length"
            Info "麦克风下拉框有 $devs 个选项（含系统默认那一项）"
            Cdp-Shot $c (Join-Path $work 'settings.png')

            # 录入热键：聚焦时暂停全局热键，按键后写入输入框，失焦后恢复
            $n = (Read-Log $box).Count
            [void](Cdp-Eval $c "__t.row('开始录制').querySelector('input').focus()")
            Check '聚焦热键输入框 → 全局热键暂停' ((Wait-Log $box '热键已暂停' $n 3000).Count -gt 0) ''
            Check '录入中输入框提示“请按下新的组合键”' ((Cdp-Eval $c "__t.row('开始录制').querySelector('input').value") -match '请按下') ''
            $before = (Read-Log $box).Count
            Press-Chord 'Ctrl', 'Alt', 'R'
            Start-Sleep -Milliseconds 800
            Check '暂停期间按旧组合不会开始录制' ((Sessions $box).Count -eq 0 -and (Wait-Log $box '热键 command=Start' $before 500).Count -eq 0) ''
            $n = (Read-Log $box).Count
            [void](Cdp-Eval $c "__t.key(__t.row('开始录制').querySelector('input'), 'F9', { ctrlKey: true, altKey: true })")
            Check '按下 Ctrl+Alt+F9 → 输入框显示新组合' ((Cdp-Eval $c "__t.row('开始录制').querySelector('input').value") -eq 'Ctrl+Alt+F9') ''
            Check '录入完成自动失焦 → 按已保存的配置恢复热键' ((Wait-Log $box '热键已注册' $n 3000).Count -eq 3) ''

            # 重复的热键：不保存，说明原因
            [void](Cdp-Eval $c "__t.row('开始录制').querySelector('input').focus()")
            [void](Cdp-Eval $c "__t.key(__t.row('开始录制').querySelector('input'), 'KeyS', { ctrlKey: true, altKey: true })")
            [void](Cdp-Eval $c "__t.click('保存')")
            Check '开始与保存用同一个组合：不保存，提示“不能相同”' (Wait-Until { (Cdp-Eval $c '__t.text()') -match '不能相同' } 5000) ''
            Check '配置文件没被改动' ((Get-Content $box.ConfigFile -Raw -Encoding UTF8) -match 'start = "Ctrl\+Alt\+R"') ''

            # 被别的程序占用的组合：保存成功，但明确标出
            [void](Cdp-Eval $c "__t.row('开始录制').querySelector('input').focus()")
            [void](Cdp-Eval $c "__t.key(__t.row('开始录制').querySelector('input'), 'F8', { ctrlKey: true, altKey: true })")
            $n = (Read-Log $box).Count
            [void](Cdp-Eval $c "__t.click('保存')")
            Check '组合被别的程序占用：保存后标出“已被别的程序占用”' (Wait-Until { (Cdp-Eval $c '__t.text()') -match '已被别的程序占用' } 5000) ''
            Check '同时弹出热键冲突通知' ((Wait-Log $box '通知.*show=true.*有热键没注册成功' $n 3000).Count -gt 0) ''
            Cdp-Shot $c (Join-Path $work 'conflict.png')

            # 换成真正空闲的组合并改提示方式：保存成功，新组合立刻生效，旧组合失效
            [void](Cdp-Eval $c "__t.row('开始录制').querySelector('input').focus()")
            [void](Cdp-Eval $c "__t.key(__t.row('开始录制').querySelector('input'), 'F9', { ctrlKey: true, altKey: true })")
            [void](Cdp-Eval $c "__t.set(__t.row('气泡通知').querySelector('select'), 'standard')")
            $n = (Read-Log $box).Count
            [void](Cdp-Eval $c "__t.click('保存')")
            Check '保存成功并提示已保存' (Wait-Until { (Cdp-Eval $c '__t.text()') -match '已保存' } 5000) ''
            $cfg = Get-Content $box.ConfigFile -Raw -Encoding UTF8
            Check '配置文件里是新热键和新的提示方式' (($cfg -match 'start = "Ctrl\+Alt\+F9"') -and ($cfg -match 'mode = "standard"')) ''
            Check '三个热键都重新注册成功' ((Wait-Log $box '热键已注册' $n 3000).Count -ge 3) ''
            $n = (Read-Log $box).Count
            Press-Chord 'Ctrl', 'Alt', 'R'
            Start-Sleep -Milliseconds 800
            Check '旧组合 Ctrl+Alt+R 不再有效' ((Sessions $box).Count -eq 0) ''
            Press-Chord 'Ctrl', 'Alt', 'F9'
            Check '新组合 Ctrl+Alt+F9 开始录制' ((Wait-Log $box '托盘图标更新.*Recording' $n 10000).Count -gt 0) ''
            Start-Sleep -Seconds 2
            $n = (Read-Log $box).Count
            Press-Chord 'Ctrl', 'Alt', 'S'
            Check '保存后回到待命' ((Wait-Log $box '托盘图标更新.*Standby' $n 20000).Count -gt 0) ''
            Check '程序不认识的键原样保留' ((Get-Content $box.ConfigFile -Raw -Encoding UTF8) -match '(?s)\[speech\]\s*model = "keep-me"') ''

            # 开机自启：勾选并保存 → 系统的 Run 项里出现本程序；取消并保存 → 移除
            $autoRow = "__t.row('登录 Windows 后自动运行').querySelector('input')"
            $runValues = { (Get-ItemProperty $runKey).PSObject.Properties | Where-Object { $_.Name -notlike 'PS*' -and [string]$_.Value -like "*$Exe*" } }
            Check '（准备）开机自启起始为未启用' (@(& $runValues).Count -eq 0) ''
            [void](Cdp-Eval $c "$autoRow.click()")
            [void](Cdp-Eval $c "__t.click('保存')")
            Check '勾选“开机自启”并保存 → Run 项里有本程序' (Wait-Until { @(& $runValues).Count -eq 1 } 5000) ((& $runValues | ForEach-Object { "$($_.Name) = $($_.Value)" }) -join ' ')
            Check '配置里 autostart = true' ((Get-Content $box.ConfigFile -Raw -Encoding UTF8) -match 'autostart = true') ''
            [void](Cdp-Eval $c "$autoRow.click()")
            [void](Cdp-Eval $c "__t.click('保存')")
            Check '取消勾选并保存 → Run 项被移除' (Wait-Until { @(& $runValues).Count -eq 0 } 5000) ''

            # 直接关闭窗口（不点保存）也应当恢复热键
            [void](Cdp-Eval $c "__t.row('保存并结束本场').querySelector('input').focus()")
            $n = (Read-Log $box).Count
            [void](Cdp-Eval $c "__t.click('关闭')")
            Check '录入中途关掉窗口：热键被恢复' ((Wait-Log $box '热键已注册' $n 5000).Count -ge 3) ''
            $c.Ws.Dispose()
        }
        [void][ShellNative]::UnregisterHotKey([IntPtr]::Zero, 201)
        Stop-App $app
    }
}
finally {
    foreach ($p in $procs) { if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } }
    Get-Process -Name 'record' -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $Exe } | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 1
    # 资源管理器为托盘图标留的登记；万一中途失败，也不要把本程序留在开机自启里
    Get-ChildItem 'HKCU:\Control Panel\NotifyIconSettings' -ErrorAction SilentlyContinue | Where-Object { (Get-ItemProperty $_.PSPath).ExecutablePath -eq $Exe } | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    (Get-ItemProperty $runKey).PSObject.Properties | Where-Object { $_.Name -notlike 'PS*' -and [string]$_.Value -like "*$Exe*" } | ForEach-Object { Remove-ItemProperty -Path $runKey -Name $_.Name -ErrorAction SilentlyContinue }
}
$runEnd = (Get-ItemProperty $runKey).PSObject.Properties.Name | Where-Object { $_ -notlike 'PS*' } | Sort-Object
Check '结束时没有动开机自启项' (($runBefore -join ',') -eq ($runEnd -join ',')) ''
''
'汇总：通过 {0} 项，失败 {1} 项' -f @($results | Where-Object { $_.结果 -eq '通过' }).Count, @($results | Where-Object { $_.结果 -eq '失败' }).Count
foreach ($shot in 'wizard.png', 'settings.png', 'conflict.png') { $f = Join-Path $work $shot; if (Test-Path $f) { Copy-Item $f (Join-Path $env:TEMP "record-shell-$shot") -Force } }
'截图（设置窗口自己的画面）：' + ((Get-ChildItem $env:TEMP -Filter 'record-shell-*.png' | ForEach-Object { $_.FullName }) -join '，')
if (-not $Keep) { Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue } else { '保留工作目录：' + $work }
