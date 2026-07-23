# 启动 dms-ai-server（先编译后运行，settings.json 在仓库根）
& "$PSScriptRoot\build.ps1"
Set-Location "$PSScriptRoot\.."
Start-Process -FilePath ".\target\debug\dms-ai-server.exe" -WorkingDirectory (Get-Location) -RedirectStandardOutput "$env:TEMP\dms-ai-server.out.log" -RedirectStandardError "$env:TEMP\dms-ai-server.err.log"
# 轮询 health
for ($i = 0; $i -lt 20; $i++) {
    Start-Sleep -Milliseconds 500
    try {
        $r = Invoke-RestMethod http://127.0.0.1:8100/health -TimeoutSec 2
        Write-Host "health: $($r | ConvertTo-Json -Depth 5)"
        break
    } catch {}
}
