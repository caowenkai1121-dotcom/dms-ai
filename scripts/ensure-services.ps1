param([switch]$Parser)

$ErrorActionPreference = 'Stop'
$root = "$PSScriptRoot\.."
Set-Location $root

function Test-Health([string]$Url, [scriptblock]$Check) {
    try { return [bool](& $Check (Invoke-RestMethod $Url -TimeoutSec 3)) } catch { return $false }
}

$embedUp = Test-Health 'http://127.0.0.1:8077/health' { param($r) $r.ok }
if ($embedUp) {
    Write-Host 'embed: 已在运行 :8077'
} else {
    Write-Host 'embed 服务未运行，启动中（模型加载约 5~15s）…'
    $py = if (Test-Path "$root\.venv\Scripts\python.exe") {
        "$root\.venv\Scripts\python.exe"
    } elseif (Get-Command python -ErrorAction SilentlyContinue) {
        'python'
    } else {
        throw '未找到 Python：.venv 缺失且 PATH 中无 python'
    }
    # 日志落仓内 target/ 而非 $env:TEMP：固定文件名会被其他实例/用户覆盖，排障看到的是别人的日志
    New-Item -ItemType Directory -Force "$root\target" | Out-Null
    $embedErrLog = "$root\target\dms-ai-embed.err.log"
    Start-Process -FilePath $py -ArgumentList 'tools\embed_service.py serve 8077' `
        -WorkingDirectory $root -WindowStyle Hidden `
        -RedirectStandardOutput "$root\target\dms-ai-embed.out.log" `
        -RedirectStandardError $embedErrLog
    # 80×500ms=40s：慢盘冷加载留余量（原 20s 窗口余量仅 5s，会误 throw）
    for ($i = 0; $i -lt 80; $i++) {
        Start-Sleep -Milliseconds 500
        if (Test-Health 'http://127.0.0.1:8077/health' { param($r) $r.ok }) {
            $embedUp = $true
            break
        }
    }
    if (-not $embedUp) {
        # throw 前先把 err 日志尾部打出来，免得用户自己去翻日志文件
        Get-Content $embedErrLog -Tail 20 -ErrorAction SilentlyContinue
        throw 'embed 服务健康检查失败'
    }
    Write-Host 'embed: 已启动 :8077'
}

if (-not $Parser) { return }

$parserUp = Test-Health 'http://127.0.0.1:8078/health' { param($r) $r.parse_ok.text }
if (-not $parserUp) {
    # 先探 daemon：docker 未运行时 image inspect 也非零，会误报「镜像构建失败」
    docker info *> $null
    if ($LASTEXITCODE -ne 0) { throw 'Docker 未运行，请先启动 Docker Desktop' }
    docker image inspect dms-ai-parser *> $null
    if ($LASTEXITCODE -ne 0) {
        & pwsh -NoProfile -File "$PSScriptRoot\parser.ps1" build
        if ($LASTEXITCODE -ne 0) { throw '文档解析镜像构建失败' }
    }
    & pwsh -NoProfile -File "$PSScriptRoot\parser.ps1" up
    if ($LASTEXITCODE -ne 0) { throw '文档解析服务启动失败' }
    $parserUp = Test-Health 'http://127.0.0.1:8078/health' { param($r) $r.parse_ok.text }
}
if (-not $parserUp) { throw '文档解析服务健康检查失败' }
Write-Host 'parser: up :8078'
