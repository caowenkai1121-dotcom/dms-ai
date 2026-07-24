# 启动 dms-ai 全栈：PG 容器 → embed 向量服务(:8077) → 后端(:8100)
# embed 缺席不致命（熔断降级），但语义缓存/向量召回会静默掉线——常驻化保住快路径
$root = "$PSScriptRoot\.."
Set-Location $root

# 0. PG 元数据库容器（缺席则拉起）
$pgUp = docker ps --format '{{.Names}}' 2>$null | Select-String -Quiet '^dms-ai-pg$'
if (-not $pgUp) {
    Write-Host "PG 容器未运行，docker compose up -d…"
    docker compose -f "$root\docker\age\docker-compose.yml" up -d | Select-Object -Last 2
}

# 1. embed 向量服务（bge-small-zh 本地模型，:8077）
$embedUp = $false
try { $embedUp = (Invoke-RestMethod http://127.0.0.1:8077/health -TimeoutSec 1).ok } catch {}
if (-not $embedUp) {
    Write-Host "embed 服务未运行，启动中（模型加载约 5~15s）…"
    Start-Process -FilePath "python" -ArgumentList "tools\embed_service.py serve 8077" -WorkingDirectory (Get-Location) `
        -RedirectStandardOutput "$env:TEMP\dms-ai-embed.out.log" -RedirectStandardError "$env:TEMP\dms-ai-embed.err.log"
    for ($i = 0; $i -lt 40; $i++) {
        Start-Sleep -Milliseconds 500
        try { if ((Invoke-RestMethod http://127.0.0.1:8077/health -TimeoutSec 1).ok) { $embedUp = $true; break } } catch {}
    }
}
Write-Host "embed: $(if ($embedUp) { 'up :8077' } else { 'DOWN（熔断降级，语义缓存/向量召回停用）' })"

# 2. 后端（先编译后运行，settings.json 在仓库根）
& "$PSScriptRoot\build.ps1"
Start-Process -FilePath ".\target\debug\dms-ai-server.exe" -WorkingDirectory (Get-Location) -RedirectStandardOutput "$env:TEMP\dms-ai-server.out.log" -RedirectStandardError "$env:TEMP\dms-ai-server.err.log"
# 轮询 health
for ($i = 0; $i -lt 20; $i++) {
    Start-Sleep -Milliseconds 500
    try {
        $r = Invoke-RestMethod http://127.0.0.1:8100/api/health -TimeoutSec 2
        Write-Host "health: $($r | ConvertTo-Json -Depth 5)"
        break
    } catch {}
}
