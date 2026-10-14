# S5 外壳 spike 的自动检查：无窗口常驻、托盘、热键（含冲突与前台窗口）、单实例、开机自启、通知、内存。
# 用法：powershell -ExecutionPolicy Bypass -File scripts\s5-check.ps1 [-Exe target\debug\record-s5.exe]
# 它会启动 record-s5，用模拟按键触发热键，只读它自己的日志；结束时把自己启动的进程收掉，并确认没有留下开机自启项。
param([string]$Exe = (Join-Path $PSScriptRoot '..\target\debug\record-s5.exe'))
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$Exe = (Resolve-Path $Exe).Path

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class S5Native {
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll", SetLastError = true)] public static extern bool RegisterHotKey(IntPtr hWnd, int id, uint mods, uint vk);
    [DllImport("user32.dll")] public static extern bool UnregisterHotKey(IntPtr hWnd, int id);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
'@
Add-Type -AssemblyName System.Windows.Forms

$results = New-Object System.Collections.Generic.List[object]
function Check([string]$name, [bool]$ok, [string]$detail = '') {
    $results.Add([pscustomobject]@{ 项目 = $name; 结果 = $(if ($ok) { '通过' } else { '失败' }); 说明 = $detail })
    '{0}  {1}  {2}' -f $(if ($ok) { '[通过]' } else { '[失败]' }), $name, $detail
}

# 模拟 Ctrl+Alt+<key>：先按修饰键，再按主键，再倒序松开
function Press-Chord([byte]$key) {
    foreach ($vk in 0x11, 0x12, $key) { [S5Native]::keybd_event($vk, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 30 }
    foreach ($vk in $key, 0x12, 0x11) { [S5Native]::keybd_event($vk, 0, 2, [UIntPtr]::Zero); Start-Sleep -Milliseconds 30 }
}
function Read-Log { if (Test-Path $log) { Get-Content $log -Encoding UTF8 } else { @() } }
function Wait-Log([string]$pattern, [int]$sinceLine = 0, [int]$timeoutMs = 3000) {
    $until = [DateTime]::Now.AddMilliseconds($timeoutMs)
    do {
        $hit = @(Read-Log | Select-Object -Skip $sinceLine | Where-Object { $_ -match $pattern })
        if ($hit.Count -gt 0) { return $hit }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::Now -lt $until)
    return @()
}

$work = Join-Path $env:TEMP 'record-s5'
if (Test-Path $work) { Remove-Item $work -Recurse -Force }
New-Item -ItemType Directory $work | Out-Null
$log = Join-Path $work 's5.log'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$runBefore = (Get-ItemProperty $runKey).PSObject.Properties.Name | Where-Object { $_ -notlike 'PS*' } | Sort-Object

# ---- 0. 启动前：默认热键有没有被别的软件占着 ----
$mods = 0x0002 -bor 0x0001 -bor 0x4000   # Ctrl + Alt + NoRepeat
foreach ($k in @{ R = 0x52; S = 0x53; Q = 0x51; F9 = 0x78 }.GetEnumerator()) {
    $ok = [S5Native]::RegisterHotKey([IntPtr]::Zero, 100, $mods, $k.Value)
    $err = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    if ($ok) { [void][S5Native]::UnregisterHotKey([IntPtr]::Zero, 100) }
    Check "启动前 Ctrl+Alt+$($k.Key) 没被占用" $ok $(if ($ok) { '' } else { "RegisterHotKey 失败，错误码 $err（1409 表示已被别的程序注册）" })
}

# ---- 1. 启动 ----
$proc = Start-Process -FilePath $Exe -ArgumentList '--log', $log, '--notify', '--autostart-roundtrip' -PassThru
try {
    $ready = Wait-Log '就绪' 0 20000
    Check '进程启动并就绪（无窗口）' ($ready.Count -gt 0 -and -not $proc.HasExited) ''
    $lines = Read-Log
    Check '托盘图标 API 创建成功' ([bool]($lines -match '托盘图标已创建')) ''
    foreach ($h in 'Ctrl+Alt+R', 'Ctrl+Alt+S', 'Ctrl+Alt+Q', 'Ctrl+Alt+F9') {
        Check "热键 $h 注册" ([bool]($lines -match "热键 $([regex]::Escape($h)) 注册成功")) (($lines -match "热键 $([regex]::Escape($h)) 注册失败") -join '')
    }
    Check '通知 API 调用成功' ([bool]($lines -match '通知：Ok')) (($lines -match '通知：') -join '')
    $auto = @($lines -match '开机自启')
    Check '开机自启 enable → 启用，disable → 停用' ([bool]($auto -match 'enable：Ok\(\(\)\)，之后：Ok\(true\)') -and [bool]($auto -match 'disable：Ok\(\(\)\)，之后：Ok\(false\)')) ($auto -join ' | ')
    $runAfter = (Get-ItemProperty $runKey).PSObject.Properties.Name | Where-Object { $_ -notlike 'PS*' } | Sort-Object
    Check '往返后注册表 Run 项与启动前一致' ((($runBefore -join ',') -eq ($runAfter -join ',')) -and -not $proc.HasExited) "前 $($runBefore.Count) 项，后 $($runAfter.Count) 项"

    # 托盘：Windows 11 为每个通知区图标在 NotifyIconSettings 下留一条记录，说明资源管理器已收下这个图标
    $tray = @(Get-ChildItem 'HKCU:\Control Panel\NotifyIconSettings' -ErrorAction SilentlyContinue | ForEach-Object { Get-ItemProperty $_.PSPath } | Where-Object { $_.ExecutablePath -eq $Exe })
    Check '资源管理器已登记托盘图标（NotifyIconSettings）' ($tray.Count -gt 0) $(if ($tray.Count) { "提示文字：$($tray[0].InitialTooltip)，是否固定在任务栏：$($tray[0].IsPromoted)" } else { '未找到记录' })

    # ---- 2. 空闲占用 ----
    Start-Sleep -Seconds 2
    $p0 = Get-Process -Id $proc.Id
    $cpu0 = $p0.TotalProcessorTime.TotalSeconds
    Start-Sleep -Seconds 10
    $p1 = Get-Process -Id $proc.Id
    $cpuPct = ($p1.TotalProcessorTime.TotalSeconds - $cpu0) / 10 * 100
    'INFO 空闲：工作集 {0:N1} MB，私有内存 {1:N1} MB，线程 {2}，10 秒内 CPU {3:N2}%' -f ($p1.WorkingSet64 / 1MB), ($p1.PrivateMemorySize64 / 1MB), $p1.Threads.Count, $cpuPct
    Check '空闲 CPU 低于 1%' ($cpuPct -lt 1.0) ('{0:N2}%' -f $cpuPct)

    # ---- 3. 热键：桌面前台 ----
    foreach ($k in @{ R = 0x52; S = 0x53; Q = 0x51 }.GetEnumerator()) {
        $n = (Read-Log).Count
        Press-Chord $k.Value
        $hit = Wait-Log "热键 .* Pressed" $n 2000
        Check "热键 Ctrl+Alt+$($k.Key) 触发（桌面前台）" ($hit.Count -gt 0) ($hit -join ' ')
    }

    # ---- 4. 热键：另一个程序的窗口在前台（置顶、无边框、铺满屏幕的窗体，模拟全屏的会议软件）----
    $form = New-Object System.Windows.Forms.Form
    $form.Text = 'S5 前台窗口'
    $form.FormBorderStyle = 'None'
    $form.WindowState = 'Maximized'
    $form.TopMost = $true
    $form.Show()
    $form.Activate()
    Start-Sleep -Milliseconds 600
    [uint32]$fgPid = 0
    [void][S5Native]::GetWindowThreadProcessId([S5Native]::GetForegroundWindow(), [ref]$fgPid)
    Check '测试窗口确实在前台' ($fgPid -eq $PID) "前台进程 $fgPid，本进程 $PID"
    $n = (Read-Log).Count
    Press-Chord 0x53
    $hit = Wait-Log "热键 .* Pressed" $n 2000
    Check '热键在铺满屏幕的置顶窗口前台时触发' ($hit.Count -gt 0) ($hit -join ' ')
    $form.Close()
    $form.Dispose()

    # ---- 5. 热键冲突：同一个组合再注册一次会明确失败 ----
    $ok = [S5Native]::RegisterHotKey([IntPtr]::Zero, 101, $mods, 0x52)
    $err = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    if ($ok) { [void][S5Native]::UnregisterHotKey([IntPtr]::Zero, 101) }
    Check '组合已被占用时 RegisterHotKey 明确失败（错误码 1409）' ((-not $ok) -and $err -eq 1409) "成功=$ok，错误码=$err"

    # ---- 6. 单实例 ----
    $n = (Read-Log).Count
    $second = Start-Process -FilePath $Exe -ArgumentList '--log', (Join-Path $work 'second.log'), '--minimized' -PassThru
    $exited = $second.WaitForExit(10000)
    Check '第二个实例自己退出' $exited $(if ($exited) { "退出码 $($second.ExitCode)" } else { '10 秒内没退出' })
    $hit = Wait-Log '第二个实例被拦下' $n 3000
    Check '第一个实例收到第二个实例的参数' ($hit.Count -gt 0) ($hit -join ' ')
    Check '第一个实例仍在运行' (-not $proc.HasExited) ''

    # ---- 7. 窗口：打开后关掉，进程应继续常驻 ----
    $n = (Read-Log).Count
    Press-Chord 0x78   # F9
    $open = Wait-Log '打开窗口：Ok' $n 8000
    Check '热键打开窗口' ($open.Count -gt 0) ($open -join ' ')
    Start-Sleep -Seconds 3
    $web = @(Get-CimInstance Win32_Process -Filter "Name = 'msedgewebview2.exe'" | Where-Object { $_.CommandLine -match [regex]::Escape('record-s5') -or $_.CommandLine -match 'io.github.gezip.record' })
    $p2 = Get-Process -Id $proc.Id
    'INFO 开着窗口：主进程工作集 {0:N1} MB，私有内存 {1:N1} MB；WebView2 子进程 {2} 个，合计工作集 {3:N1} MB' -f ($p2.WorkingSet64 / 1MB), ($p2.PrivateMemorySize64 / 1MB), $web.Count, (($web | ForEach-Object { (Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue).WorkingSet64 } | Measure-Object -Sum).Sum / 1MB)
    $n = (Read-Log).Count
    [void](Get-Process -Id $proc.Id).CloseMainWindow()   # 像用户点了窗口右上角的 ×
    $intercepted = Wait-Log '已拦下' $n 5000
    Start-Sleep -Milliseconds 500
    Check '关掉最后一个窗口后进程继续常驻' ((-not $proc.HasExited) -and $intercepted.Count -gt 0) ($intercepted -join ' ')
    Start-Sleep -Seconds 4
    $webAfter = @(Get-CimInstance Win32_Process -Filter "Name = 'msedgewebview2.exe'" | Where-Object { $_.CommandLine -match 'io.github.gezip.record' })
    Check '关窗后 WebView2 子进程全部退出（常驻时不占那几百 MB）' ($webAfter.Count -eq 0) "还剩 $($webAfter.Count) 个"
    $p3 = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
    if ($p3) { 'INFO 关窗后：主进程工作集 {0:N1} MB，私有内存 {1:N1} MB' -f ($p3.WorkingSet64 / 1MB), ($p3.PrivateMemorySize64 / 1MB) }
    Check '关窗后热键仍然有效' $(
        $n = (Read-Log).Count; Press-Chord 0x51; (Wait-Log "热键 .* Pressed" $n 2000).Count -gt 0
    ) ''
}
finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
    Get-Process -Name 'record-s5' -ErrorAction SilentlyContinue | Stop-Process -Force
}
$runEnd = (Get-ItemProperty $runKey).PSObject.Properties.Name | Where-Object { $_ -notlike 'PS*' } | Sort-Object
Check '结束时没有留下开机自启项' (($runBefore -join ',') -eq ($runEnd -join ',')) ''
''
'汇总：通过 {0} 项，失败 {1} 项' -f @($results | Where-Object { $_.结果 -eq '通过' }).Count, @($results | Where-Object { $_.结果 -eq '失败' }).Count
'日志：' + $log
