# 在 Docker 里 build/test 整个 workspace。
#
# 为什么需要它：本机 Smart App Control 处于**强制态**
# （HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy 的 VerifiedAndReputablePolicyState = 1），
# 会按内容哈希拦截所有新链接的未签名产物 —— 不只是 test exe，连依赖的 build script、
# proc-macro DLL、甚至一行 `fn main(){}` 编出来的 exe 都是 `os error 4551`。
# 于是 Windows 侧 `cargo build` / `cargo test` 对**任何** crate 都不可用。
#
# 这不是绕过校验，是换一个没有该策略的环境**真跑**：仓库只读挂载，产物落 docker volume，
# 与主机 target/ 互不干扰。明确禁止的做法是「复制 exe 再追加字节改哈希」——
# 那让「验收到底跑没跑过」变得不可信。
#
# 用法：
#   .\scripts\docker-test.ps1                        # build + test 全量
#   .\scripts\docker-test.ps1 -Only build            # 只 build
#   .\scripts\docker-test.ps1 -Sel '-p dms-policy'   # 指定 crate
#
# 首次会拉 rust:1-slim 并编依赖（约 1-2 分钟）；volume 预热后全量 build 约 30s。
#
# 参数名不叫 `-Args`：`$Args` 是 PowerShell 的**自动变量**，param 同名时拿不到传入值
# （第一版就踩了这个，表现为「参数写了但没生效」）。
param(
    [ValidateSet('all', 'build', 'test')] [string]$Only = 'all',
    [string]$Sel = '--workspace'
)
$ErrorActionPreference = 'Stop'
Set-Location "$PSScriptRoot\.."
$repo = (Get-Location).Path

# rust-toolchain.toml 钉的是 windows-gnu（主机需要它），容器里必须绕开 —— 否则 rustup 会去
# 拉一个跑不了的 target。版本号与主机保持同一大版本，避免「容器绿主机红」。
$env:MSYS_NO_PATHCONV = '1'
$common = @(
    'run', '--rm',
    '-e', 'RUSTUP_TOOLCHAIN=1.97.1-x86_64-unknown-linux-gnu',
    '-e', 'CARGO_TARGET_DIR=/target',
    '-e', 'CARGO_INCREMENTAL=0',
    '-v', "${repo}:/src:ro",
    '-v', 'dmsai_cargo:/usr/local/cargo/registry',
    '-v', 'dmsai_target:/target',
    '-w', '/src', 'rust:1-slim', 'bash', '-c'
)

function Run-InDocker([string]$cmd, [string]$label) {
    Write-Host "[docker] $label" -ForegroundColor Cyan
    & docker @common $cmd
    if ($LASTEXITCODE -ne 0) { Write-Host "[FAIL] $label" -ForegroundColor Red; exit 1 }
}

if ($Only -ne 'test') {
    # `set -o pipefail` 不能省：管道退出码默认取**最后一段**，`tail` 永远成功
    # ⇒ 编译失败时 $LASTEXITCODE 仍是 0，脚本照打 [ok] 全绿。
    # 实测抓到过：同屏上方 `[ok] docker 侧全绿`、下方 `error: could not compile 'dms-semantic'`。
    # test 半边没这个洞（先 out=$(...) 再判 fail/targets），只有 build 半边有。
    Run-InDocker "set -o pipefail; cargo build --locked $Sel 2>&1 | tail -20" "build $Sel"
}
if ($Only -ne 'build') {
    # 一次跑、一次汇总（`bc` 不在 rust:1-slim 里，用 awk 求和 —— 第一版用 bc，汇总恒为空）。
    # 任何非零 failed 或任何 target 没能执行都让脚本非零退出：
    # 「跑不了」与「跑过且全绿」必须能区分，这正是 SAC 那几轮教的。
    Run-InDocker @'
out=$(cargo test --locked SEL --no-fail-fast 2>&1)
echo "$out" | grep -E 'Running|Doc-tests|test result|^error|panicked'
pass=$(echo "$out" | grep -oE '[0-9]+ passed' | awk '{s+=$1} END {print s+0}')
fail=$(echo "$out" | grep -oE '[0-9]+ failed' | awk '{s+=$1} END {print s+0}')
targets=$(echo "$out" | grep -cE 'test result')
echo "== 合计 ${pass} passed / ${fail} failed（${targets} 个 target 执行）=="
[ "$fail" -eq 0 ] && [ "$targets" -gt 0 ]
'@.Replace('SEL', $Sel) "test + 汇总 $Sel"
}
Write-Host '[ok] docker 侧全绿' -ForegroundColor Green
