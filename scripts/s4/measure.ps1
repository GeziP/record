# 测量一个命令行程序的耗时、CPU 时间与峰值内存。只量被启动的这个进程，不含它再起的子进程（sherpa-onnx 的命令行不会再起子进程）。
# 用法：powershell -NoProfile -ExecutionPolicy Bypass -File measure.ps1 -Request <请求.json>
# 请求：{ "exe", "args": [], "stdout", "stderr", "result", "priority": "Normal|BelowNormal|Idle", "workDir", "timeoutSec": 0, "pollMs": 50 }
# 结果写到请求里的 result 路径：{ exitCode, wallMs, cpuMs, userMs, kernelMs, peakWorkingSetMb, peakPrivateMb, timedOut }
# 峰值内存是进程退出前最后一次轮询读到的“峰值”计数器，轮询间隔内（默认 50 毫秒）最后一刻的增长可能漏掉。
param([Parameter(Mandatory = $true)][string]$Request)
$ErrorActionPreference = 'Stop'

function Quote-Arg([string]$a) {
    if ($a -ne '' -and $a -notmatch '[\s"]') { return $a }
    $s = $a -replace '(\\*)"', '$1$1\"'   # 引号前的反斜杠加倍，引号本身加一个反斜杠
    $s = $s -replace '(\\+)$', '$1$1'     # 结尾的反斜杠加倍（后面还有收尾的引号）
    return '"' + $s + '"'
}

$req = Get-Content -LiteralPath $Request -Raw -Encoding UTF8 | ConvertFrom-Json
$argLine = (@($req.args) | ForEach-Object { Quote-Arg ([string]$_) }) -join ' '
$pollMs = if ($req.pollMs) { [int]$req.pollMs } else { 50 }
$start = @{
    FilePath               = $req.exe
    WorkingDirectory       = $req.workDir
    RedirectStandardOutput = $req.stdout
    RedirectStandardError  = $req.stderr
    NoNewWindow            = $true
    PassThru               = $true
}
if ($argLine -ne '') { $start.ArgumentList = $argLine }

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$p = Start-Process @start
$null = $p.Handle   # 先拿住句柄，进程退出后才读得到退出码与 CPU 时间
$p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]$req.priority

$peakWs = 0L
$peakPriv = 0L
$timedOut = $false
while (-not $p.HasExited) {
    try {
        $p.Refresh()
        if ($p.PeakWorkingSet64 -gt $peakWs) { $peakWs = $p.PeakWorkingSet64 }
        if ($p.PeakPagedMemorySize64 -gt $peakPriv) { $peakPriv = $p.PeakPagedMemorySize64 }
    } catch { }   # 进程恰好在这一刻退出
    if ($req.timeoutSec -gt 0 -and $sw.Elapsed.TotalSeconds -gt $req.timeoutSec) {
        $timedOut = $true
        & taskkill.exe /PID $p.Id /T /F | Out-Null
        break
    }
    Start-Sleep -Milliseconds $pollMs
}
$p.WaitForExit()
$sw.Stop()

$result = [ordered]@{
    exitCode         = $p.ExitCode
    wallMs           = [math]::Round($sw.Elapsed.TotalMilliseconds, 1)
    cpuMs            = [math]::Round($p.TotalProcessorTime.TotalMilliseconds, 1)
    userMs           = [math]::Round($p.UserProcessorTime.TotalMilliseconds, 1)
    kernelMs         = [math]::Round($p.PrivilegedProcessorTime.TotalMilliseconds, 1)
    peakWorkingSetMb = [math]::Round($peakWs / 1MB, 1)
    peakPrivateMb    = [math]::Round($peakPriv / 1MB, 1)
    timedOut         = $timedOut
}
[System.IO.File]::WriteAllText($req.result, ($result | ConvertTo-Json -Compress), (New-Object System.Text.UTF8Encoding($false)))
