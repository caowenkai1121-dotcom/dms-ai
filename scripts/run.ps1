# 启动后端栈：PG -> 向量/解析服务 -> Rust API。
# Rust 统一在 Docker 内构建，避免 Windows Smart App Control 拦截本地产物。
$ErrorActionPreference = 'Stop'
$root = "$PSScriptRoot\.."
Set-Location $root

# PG 密码只从 gitignore 的运行时配置读取，再临时注入 compose。
$settingsPath = if (Test-Path "$root\settings.docker.json") {
    "$root\settings.docker.json"
} elseif (Test-Path "$root\settings.json") {
    "$root\settings.json"
} else {
    throw '缺少 settings.docker.json 或 settings.json，请从 settings.example.json 创建'
}
# 元数据库端口只绑定本机回环。
$pgUp = docker ps --format '{{.Names}}' 2>$null | Select-String -Quiet '^dms-ai-pg$'
if (-not $pgUp) {
    $settings = Get-Content $settingsPath -Raw | ConvertFrom-Json
    $pgUri = [Uri]$settings.pg_url
    $pgUserInfo = $pgUri.UserInfo -split ':', 2
    if ($pgUserInfo.Count -ne 2 -or -not $pgUserInfo[1]) { throw 'pg_url 未包含密码' }
    $env:DMS_AI_PG_PASSWORD = [Uri]::UnescapeDataString($pgUserInfo[1])
    try {
        Write-Host 'PG 容器未运行，docker compose up -d...'
        docker compose -f "$root\docker\age\docker-compose.yml" up -d | Select-Object -Last 2
        if ($LASTEXITCODE -ne 0) { throw 'PG 容器启动失败' }
    } finally {
        Remove-Item Env:DMS_AI_PG_PASSWORD -ErrorAction SilentlyContinue
    }
}

& "$PSScriptRoot\ensure-services.ps1" -Parser
if ($LASTEXITCODE -ne 0) { throw '向量或解析服务启动失败' }

& "$PSScriptRoot\serve.ps1" -Build
exit $LASTEXITCODE
