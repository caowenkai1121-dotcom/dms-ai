# 编译 dms-ai-server：WinLibs mingw 置于 PATH 最前，压住 Git 自带 mingw 的残缺 ld
$mingw = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin"
$env:PATH = "$mingw;$env:PATH"
# 旧服务进程占用 exe 会导致链接期 拒绝访问(os error 5)，先停掉
Get-Process dms-ai-server -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
Set-Location "$PSScriptRoot\.."
cargo build -p dms-ai-server 2>&1 | Select-Object -Last 15
