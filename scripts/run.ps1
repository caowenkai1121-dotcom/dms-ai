# 启动后端栈：PG -> 向量/解析服务 -> Rust API。
# Rust 统一在 Docker 内构建，避免 Windows Smart App Control 拦截本地产物。
$ErrorActionPreference = 'Stop'
$root = "$PSScriptRoot\.."
Set-Location $root

# PG 密码只从 gitignore 的运行时配置读取，再临时注入 compose。
$settingsPath = if (Test-Path "$root\settings.docker.json") {
    "$root\settings.docker.json"
} elseif (Test-Path "$root\settings.json") {
    # 下游 serve.ps1 只读 settings.docker.json，回退这条路走不完整，先明说
    Write-Host '警告：回退使用 settings.json；后续 serve.ps1 仍需 settings.docker.json，请确认其已创建' -ForegroundColor Yellow
    "$root\settings.json"
} else {
    throw '缺少 settings.docker.json 或 settings.json，请从 settings.example.json 创建'
}
# 先探 daemon：docker 未运行时 docker ps 静默得 false，会走到 compose up 才报错，文案对不上根因
docker info *> $null
if ($LASTEXITCODE -ne 0) { throw 'Docker 未运行，请先启动 Docker Desktop' }
$pgUp = docker ps --format '{{.Names}}' 2>$null | Select-String -Quiet '^dms-ai-pg$'
if (-not $pgUp) {
    try {
        $settings = Get-Content $settingsPath -Raw | ConvertFrom-Json
    } catch {
        throw "$settingsPath 不是合法 JSON：$($_.Exception.Message)"
    }
    if (-not $settings.pg_url) { throw "$settingsPath 缺少 pg_url 字段" }
    try {
        $pgUri = [Uri]$settings.pg_url
    } catch {
        throw "pg_url 不是合法 URI：$($settings.pg_url)"
    }
    # 用户名与密码同属 UserInfo，两段都做 Unescape（含 %40 等编码时才不会带错）
    $pgUserInfo = $pgUri.UserInfo -split ':', 2 | ForEach-Object { [Uri]::UnescapeDataString($_) }
    if ($pgUserInfo.Count -ne 2 -or -not $pgUserInfo[1]) { throw 'pg_url 未包含密码' }
    $env:DMS_AI_PG_PASSWORD = $pgUserInfo[1]
    try {
        Write-Host 'PG 容器未运行，docker compose up -d...'
        # 元数据库端口只绑定本机回环（绑定事实在 docker/age/docker-compose.yml，本脚本不做收窄）。
        docker compose -f "$root\docker\age\docker-compose.yml" up -d | Select-Object -Last 2
        if ($LASTEXITCODE -ne 0) {
            Write-Host 'compose 输出已截断，完整日志：docker logs dms-ai-pg' -ForegroundColor Yellow
            throw 'PG 容器启动失败'
        }
    } finally {
        Remove-Item Env:DMS_AI_PG_PASSWORD -ErrorAction SilentlyContinue
    }
}

& "$PSScriptRoot\ensure-services.ps1" -Parser
if ($LASTEXITCODE -ne 0) { throw '向量或解析服务启动失败' }

& "$PSScriptRoot\serve.ps1" -Build
exit $LASTEXITCODE
