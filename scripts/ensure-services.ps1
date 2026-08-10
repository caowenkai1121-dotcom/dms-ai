param([switch]$Parser)

$ErrorActionPreference = 'Stop'
$root = "$PSScriptRoot\.."
Set-Location $root

function Test-Health([string]$Url, [scriptblock]$Check) {
    try { return [bool](& $Check (Invoke-RestMethod $Url -TimeoutSec 3)) } catch { return $false }
}

$embedUp = Test-Health 'http://127.0.0.1:8077/health' { param($r) $r.ok }
if (-not $embedUp) {
    Write-Host 'embed 服务未运行，启动中（模型加载约 5~15s）…'
    $py = if (Test-Path "$root\.venv\Scripts\python.exe") {
        "$root\.venv\Scripts\python.exe"
    } else {
        'python'
    }
    Start-Process -FilePath $py -ArgumentList 'tools\embed_service.py serve 8077' `
        -WorkingDirectory $root -WindowStyle Hidden `
        -RedirectStandardOutput "$env:TEMP\dms-ai-embed.out.log" `
        -RedirectStandardError "$env:TEMP\dms-ai-embed.err.log"
    for ($i = 0; $i -lt 40; $i++) {
        Start-Sleep -Milliseconds 500
        if (Test-Health 'http://127.0.0.1:8077/health' { param($r) $r.ok }) {
            $embedUp = $true
            break
        }
    }
}
if (-not $embedUp) { throw 'embed 服务健康检查失败' }
Write-Host 'embed: up :8077'

if (-not $Parser) { return }

$parserUp = Test-Health 'http://127.0.0.1:8078/health' { param($r) $r.parse_ok.text }
if (-not $parserUp) {
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
