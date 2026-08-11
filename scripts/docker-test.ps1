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
# 首次会拉 rust:1-slim-bookworm 并编依赖（约 1-2 分钟）；volume 预热后全量 build 约 30s。
#
# 参数名不叫 `-Args`：`$Args` 是 PowerShell 的**自动变量**，param 同名时拿不到传入值
# （第一版就踩了这个，表现为「参数写了但没生效」）。
param(
    [ValidateSet('all', 'build', 'test')] [string]$Only = 'all',
    [string]$Sel = '--workspace'
)
$ErrorActionPreference = 'Stop'
# 空串 -Sel 会让 bash 侧悄悄退化成 cargo 默认（非 --workspace），归一并明说
if ([string]::IsNullOrWhiteSpace($Sel)) {
    Write-Host '未指定 -Sel，按 --workspace 全量处理'
    $Sel = '--workspace'
}
# $Sel 下方被字面拼进 bash -c（.Replace），先拦掉引号、$、; 等 bash 元字符
if ($Sel -notmatch '^[a-zA-Z0-9 \-=]+$') { throw "-Sel 含非法字符（仅允许字母数字、空格、-、=）：$Sel" }
Set-Location "$PSScriptRoot\.."
$repo = (Get-Location).Path

# rust-toolchain.toml 钉的是 windows-gnu（主机需要它），容器里必须绕开 —— 否则 rustup 会去
# 拉一个跑不了的 target。rust-toolchain.toml 只写 stable 不钉版本号，这里钉死 1.97.1 保证
# 容器内可复现；主机 stable 升版后需人工同步本行，否则会「容器绿主机红」。
# 镜像同理钉 bookworm 变体：slim 基底随 Debian 大版本漂移，会部分抵消钉 toolchain 的意义。
# volume 名带仓目录哈希后缀：同机第二个工作副本不共享 cargo/target volume（锁竞争、产物互串）。
$volSuffix = [System.BitConverter]::ToString([System.Security.Cryptography.MD5]::Create().ComputeHash(
    [System.Text.Encoding]::UTF8.GetBytes($repo.ToLowerInvariant()))).Replace('-', '').Substring(0, 8).ToLowerInvariant()
$env:MSYS_NO_PATHCONV = '1'
$common = @(
    'run', '--rm',
    '-e', 'RUSTUP_TOOLCHAIN=1.97.1-x86_64-unknown-linux-gnu',
    '-e', 'CARGO_TARGET_DIR=/target',
    '-e', 'CARGO_INCREMENTAL=0',
    '-v', "${repo}:/src:ro",
    '-v', "dmsai_cargo_${volSuffix}:/usr/local/cargo/registry",
    '-v', "dmsai_target_${volSuffix}:/target",
    '-w', '/src', 'rust:1-slim-bookworm', 'bash', '-c'
)

function Run-InDocker([string]$cmd, [string]$label) {
    Write-Host "[docker] $label" -ForegroundColor Cyan
    & docker @common $cmd
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[FAIL] $label" -ForegroundColor Red
        # build 红则 -Only all 的 test 直接不跑；build 的首个 error 常在 tail -20 截断之上
        Write-Host '提示：可用 -Only build / -Only test 单跑某阶段；完整日志去掉 tail 截断重跑（如 cargo build --locked）' -ForegroundColor Yellow
        exit 1
    }
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
# ^failures: 及其缩进名单行进摘要：只剩计数时，谁红必须重跑才知道
echo "$out" | grep -E 'Running|Doc-tests|test result|^error|panicked|^failures:|^    \S'
pass=$(echo "$out" | grep -oE '[0-9]+ passed' | awk '{s+=$1} END {print s+0}')
fail=$(echo "$out" | grep -oE '[0-9]+ failed' | awk '{s+=$1} END {print s+0}')
targets=$(echo "$out" | grep -cE 'test result')
echo "== 合计 ${pass} passed / ${fail} failed（${targets} 个 target 执行）=="
[ "$fail" -eq 0 ] && [ "$targets" -gt 0 ]
'@.Replace('SEL', $Sel) "test + 汇总 $Sel"
}
Write-Host '[ok] docker 侧全绿' -ForegroundColor Green
