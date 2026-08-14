# 架构门禁：依赖方向 + 只读红线 + kernel 纯度。CI 与本地随时可跑，非零退出即违规。
# 规则来源 docs/ARCHITECTURE.md §1。
$ErrorActionPreference = 'Stop'
Set-Location "$PSScriptRoot\.."
$fail = 0
# 执行过的 Deny 条数。末尾与 $EXPECT_RULES 对账 —— 缺席判红管不到「Deny 那一行被删/注释掉」，
# 而那也是一种「规则静默消失」。两道闸形态不同，都留着。
$rules = 0

function Deny($label, $path, $pattern, [switch]$WarnOnly, [switch]$ProductionOnly) {
    $script:rules++
    if (-not (Test-Path $path)) {
        # 🔴 原来是 `return`：缺席既不打 [ok] 也不打 [FAIL]，$fail 保持 0。
        # T1-T10 正在成批搬 crate：目录改名或搬走的那一刻，10 条 Deny 会一条不剩地
        # 静默消失，报告照旧「架构门禁全绿」。消失的里头包括红线
        # 「knowledge 结构上不得产 SQL」（不变量 I5 的唯一结构性保证）。
        # 规则无法执行 ≠ 规则通过：本项目已经栽在这个形态上四次（见 ⑤ 的 cargo tree 那段）。
        Write-Host "[FAIL] $label：$path 不存在（规则无法执行 ≠ 规则通过）" -ForegroundColor Red
        $script:fail = 1
        return
    }
    # 注释行不参与匹配：规则本身要写在 crate 文档里（「不得引 axum」这句话不能把自己判红）。
    # 边界要自知：过滤只认行首的 //、*、/* —— 块注释中间行以别的字符开头、或代码行尾
    # 注释里含关键词仍会命中（假红方向，需人工看行内容）；代码行不会因此漏检。
    $hit = Get-ChildItem -Path $path -Recurse -Filter *.rs -ErrorAction SilentlyContinue |
        ForEach-Object {
            $file = $_
            # kernel 的“零业务语料”约束针对生产内核；测试必须能用真实业务样例证明
            # 泛化算法没有误判。Rust 文件的 #[cfg(test)] 模块统一置于文件末尾，因此只在
            # ProductionOnly 规则中截掉该模块，其他依赖/SQL 红线仍扫描完整源码。
            $testStart = if ($ProductionOnly) {
                (Select-String -Path $file.FullName -Pattern '^\s*#\[cfg\(test\)\]' |
                    Select-Object -First 1).LineNumber
            }
            Select-String -Path $file.FullName -Pattern $pattern |
                Where-Object {
                    $_.Line.Trim() -notmatch '^(//|\*|/\*)' -and
                    (-not $testStart -or $_.LineNumber -lt $testStart)
                }
        }
    if ($hit) {
        if ($WarnOnly) {
            Write-Host "[warn] $label（$($hit.Count) 处，迁移中）" -ForegroundColor Yellow
        } else {
            Write-Host "[FAIL] $label" -ForegroundColor Red
            $hit | ForEach-Object { Write-Host "       $($_.Path):$($_.LineNumber): $($_.Line.Trim())" }
            $script:fail = 1
        }
    } else {
        Write-Host "[ok]   $label"
    }
}

# ① 只有 connector 能造池、能拼 SQL 串
foreach ($c in 'kernel', 'policy', 'agent') {
    Deny "$c 不得造连接池 / 不得 sqlx::query*" "crates/$c/src" 'MySqlPoolOptions|PgPoolOptions|sqlx::query'
}
# semantic 只守「不造池」——**不**守 sqlx::query，这是裁决 F1（T7a）而非放水：
# ARCHITECTURE §2 的 I2 残缺列从一开始就写着「semantic 的 30+ 处 &PgPool 是字面量 SQL，靠 grep 守」。
# 原规则是 semantic 只有 present.rs 时写的；registry/recall/ingest 落地后它是 meta.* 的唯一读写口，
# 召回 SQL 必须运行时拼 `{ds_pred}`（谓词的 bind 序号随查询变），进不了 `&'static str` 通道。
# 真正该守的两条更紧、且都在 `crates/semantic/tests/drift.rs` 里（cargo test 跑得到，这个脚本跑不到）：
#   every_meta_recall_is_ds_scoped        —— 每条 FROM meta. 必带 ds 谓词
#   sql_interpolation_is_allowlisted      —— SQL 里只许插 ds_pred 与白名单，别的一律红
# 想恢复 FAIL 的路：把召回改成固定模板 + ds 恒为 $1，全部 SQL 走 OwnedStore::fixed。那要重写全部
# SQL 文本，会让「逐行搬运对拍」失效，故不在搬迁轮做（T8 之后再议）。
Deny 'semantic 不得造连接池' 'crates/semantic/src' 'MySqlPoolOptions|PgPoolOptions'
# server 是唯一剩下的 WarnOnly：业务代码全部搬出（T10）后删掉 -WarnOnly。
# knowledge 已于 T4 转正（OwnedStore::fixed 通道落地，25 处 → 0），从此按 FAIL 守——
# 再出现一行 sqlx::query 就意味着有人绕开了字面量通道，那是「把问句拼进 SQL」的入口。
# T8/T10 收尾（2026-08-13）：direct.rs(7363) 与 corrector.rs(1758) 整文件删除后，server 只剩
# 装配、协议与认证；剩下的 sqlx 命中全是 FromRow 派生与 server 自有表（chat/artifact/日志面），
# 不是业务算法。**去掉 -WarnOnly** —— 没有这一步，搬完也拿不出「真的搬完了」的凭据。
Deny 'server 不得造连接池' 'crates/server/src' 'MySqlPoolOptions|PgPoolOptions'
Deny 'knowledge 不得造连接池 / 不得 sqlx::query*' 'crates/knowledge/src' 'MySqlPoolOptions|PgPoolOptions|sqlx::query'
# 🔴 不变量 I5 的结构性保证：知识库路径**产不出 SQL**。此前它只由两行注释支撑，零守卫。
# 注意措辞：不是「依赖树里没有 sqlparser」——`dms-kernel` 就依赖它，传递必然在树里；
# 可检查且真正成立的是「**源码**不 import 它、不构造 SQL newtype」。
# 为什么值得一条硬规则：上传文档与表头是不可信输入，一旦知识库侧能拼出 SQL，
# 「外部文本永不成为指令」就从编译期保证退化成一句纪律。
Deny 'knowledge 结构上不得产 SQL' 'crates/knowledge/src' 'use sqlparser|sqlparser::|RawSql|CheckedSql|ScopedSql'
# ② kernel 纯度：零 IO 依赖、零 DMS 业务语料
Deny 'kernel 不得引 IO 依赖' 'crates/kernel/src' 'sqlx|reqwest|axum::|tokio::|chrono::'
Deny 'kernel 生产代码不得含 DMS 表名' 'crates/kernel/src' '\bt_[a-z_]{3,}\b' -ProductionOnly
Deny 'kernel 生产代码不得含 DMS 业务名词' 'crates/kernel/src' '销售额|客单价|门店|经销商|有效订单' -ProductionOnly
# ③ agent 不配 HTTP；semantic/knowledge 不依赖 policy
Deny 'agent 不得引 axum' 'crates/agent/src' 'axum'
foreach ($c in 'semantic', 'knowledge') {
    Deny "$c 不得依赖 policy" "crates/$c/src" 'dms_policy'
}
# ④ server 只在身份面用 reqwest
#
# 🔴 `-notmatch '^(//|\*|/\*)'` 这道注释过滤不能省，而这段自己写的管道原来**漏了它**
#（`Deny` 函数里有、这里没有 —— 同一条纪律的两处渲染漂了）。
# 实测后果：`mcp_api.rs` 里一句纯注释「英文串先小写化再匹配（sqlx/reqwest 的原文）」
# 把整条门禁判红，而 `grep -n reqwest crates/server/src/*.rs` 显示真实用点只有
# auth.rs / llm.rs / wework.rs 三个白名单文件 —— mcp_api.rs 一次都没用过 reqwest。
# （其后 xcx_api.rs 作为小程序 token 校验的身份面文件加入白名单：server-to-server
#  回调商城后端 getLoginInfo，与 auth/wework 同一性质 —— 对外 HTTP 只许出现在身份面。）
# 假红的代价不是「多一条红」：它把整个门禁染红，**掩盖同一趟里的真违规**
#（那一趟真红是 agent crate 造了连接池，被这条假红盖在下面）。
$bad = Get-ChildItem crates/server/src -Recurse -Filter *.rs -ErrorAction SilentlyContinue |
    Select-String -Pattern 'reqwest' |
    Where-Object {
        $_.Path -notmatch '(identity|wework|auth|llm|embed|xcx_api)\.rs$' -and
        $_.Line.Trim() -notmatch '^(//|\*|/\*)'
    }
if ($bad) {
    Write-Host '[FAIL] server 的 reqwest 只许出现在身份面文件' -ForegroundColor Red
    $bad | ForEach-Object { Write-Host "       $($_.Path):$($_.LineNumber)" }
    $fail = 1
} else { Write-Host '[ok]   server 的 reqwest 仅在身份面' }

# ⑤ 依赖方向无反向边。
#
# 🔴 原实现用 `cargo tree -p <crate>`，在**本机恒空转**：Smart App Control 下 cargo 退出 1
# 且不产出任何行 → 「发现 0 个依赖」→「0 条反向边」→ 打 `[ok]`。
# 也就是说这条检查从来没有真正跑过，而它恰恰是「agent 不许反向依赖 server」的唯一脚本证据
# （那条不变量本身成立 —— cargo 拒绝循环依赖，是编译器在管；但脚本没在证明它）。
# 这是本项目第四次踩同一形态：**判据的入参变空，断言就悄悄变成恒真，报告只显示绿。**
#
# 改成机械解析各 crate 的 `Cargo.toml` 直接依赖：不依赖 cargo、各平台都能跑。
# 原注释说「不靠人读 Cargo.toml」——解析清单是机械的，不是人读；而且**层级违规看直接依赖就够**
# （传递边必然由链上某条直接边构成，那条边自己会被判红）。
$order = @{ 'kernel' = 0; 'connector' = 1; 'policy' = 2; 'semantic' = 2; 'knowledge' = 2; 'agent' = 3; 'server' = 4 }
$edges = 0
foreach ($c in $order.Keys) {
    $toml = "crates/$c/Cargo.toml"
    if (-not (Test-Path $toml)) { continue }
    foreach ($m in (Select-String -Path $toml -Pattern '^\s*dms-([a-z]+)\s*=' -AllMatches)) {
        $d = $m.Matches[0].Groups[1].Value
        if ($d -eq 'ai') { $d = 'server' }   # 包名是 dms-ai-server
        $edges++
        if ($order.ContainsKey($d) -and $order[$d] -ge $order[$c]) {
            Write-Host "[FAIL] 反向/同层依赖: $c -> $d" -ForegroundColor Red
            $fail = 1
        }
    }
}
# 空转跳闸：解析漂了会一条边都找不到而「永远绿」，那正是原实现的病
if ($edges -lt 10) {
    Write-Host "[FAIL] 依赖边只解析到 $edges 条（应 ≥10），本检查已成空转" -ForegroundColor Red
    $fail = 1
} elseif ($fail -eq 0) {
    Write-Host "[ok]   依赖方向单向无环（$edges 条直接依赖边）"
}

# 规则条数对账：Deny 的调用行被删掉/注释掉时，「目录缺席判红」那道闸够不着
#（函数根本没被调用），报告依旧全绿。数字写死是刻意的 —— 加规则要顺手改这里，
# 那一步正是「有人动了门禁」的签收。
$EXPECT_RULES = 13
if ($rules -ne $EXPECT_RULES) {
    Write-Host "[FAIL] 只执行了 $rules 条 Deny 规则（应 $EXPECT_RULES）—— 有规则被删或被跳过" -ForegroundColor Red
    $fail = 1
} else { Write-Host "[ok]   Deny 规则条数 $rules/$EXPECT_RULES" }

if ($fail -ne 0) { Write-Host '架构门禁未通过' -ForegroundColor Red; exit 1 }
Write-Host '架构门禁全绿' -ForegroundColor Green
# 显式 exit 0：没有它时脚本的退出码继承最后一个外部命令 —— 原实现末尾是失败的 `cargo tree`，
# 于是同一份代码会跑出 0/1/1 三种结果（H2 agent 实测到的「退出码不可靠」就是这个）。
exit 0
