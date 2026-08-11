# 编译 dms-ai-server：WinLibs mingw 置于 PATH 最前，压住 Git 自带 mingw 的残缺 ld
$ErrorActionPreference = 'Stop'
$mingw = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin"
if (-not (Test-Path $mingw)) { throw "未找到 WinLibs mingw：$mingw（请确认已安装 BrechtSanders.WinLibs.POSIX.UCRT）" }
$env:PATH = "$mingw;$env:PATH"
$root = (Resolve-Path "$PSScriptRoot\..").Path
# 旧服务进程占用 exe 会导致链接期 拒绝访问(os error 5)，先停掉：按路径过滤只杀本仓产物，
# 并等它真退出（固定 sleep 不确认，慢机器上句柄未放仍会 os error 5）
Get-Process dms-ai-server -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$root\*" } |
    Stop-Process -Force -PassThru -ErrorAction SilentlyContinue | Wait-Process -Timeout 5 -ErrorAction SilentlyContinue
Set-Location $root
# --locked 与 docker-test.ps1 对齐（本机与容器用同一份 lockfile 解析）；
# -Last 40：依赖编译失败时首个 error 常在最后 15 行之上
cargo build -p dms-ai-server --locked 2>&1 | Select-Object -Last 40
