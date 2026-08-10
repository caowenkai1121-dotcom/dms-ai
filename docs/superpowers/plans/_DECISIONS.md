# dms-ai 架构迁移 · plan 裁决与契约对齐记录（2026-07-27）

> 用途：10 份迁移 plan 已产 9 份，team-lead 已统一拍板。本文件是**抗 compact 的真相源**，
> 执行任何 plan 前先读本文件。spec：docs/superpowers/specs/2026-07-27-generic-agent-arch-design.md。

## 一、必须修的契约冲突（跨 plan 不一致，执行前对齐）

| # | 冲突 | 裁决（统一为） |
|---|---|---|
| C1 | 动态 IN 占位符通道：T3 叫 `expand(n)`（{in}标记），T5 叫 `fixed_in(template,n)`（{ph}标记） | **统一为 `FixedStmt::expand(n)`**，模板含 `{in}` 标记，展开为 `?,?,...`，数据全走 bind。T5 的 `fixed_in(template,n)` 全部改为 `fixed(template).expand(n)`。多 `{in}` 同 n 展开（scope.rs 双 IN 场景），bind 顺序=展开后占位符顺序。 |
| C2 | kernel::inject 签名：T3 两参 `inject(CheckedSql,&ScopeSets)`（RuleSet 走全局），T5 三参 `inject(sql,&ScopeSets,&RuleSet)` | **终态三参（T5 对）**。T3 的 `inject` 实现内部从「全局 OnceLock」改为吃 `&RuleSet` 参数（RuleSet 类型由 T5 引入）；T3 阶段先用 T2 的 `rule_of` 闭包包装过渡，T5 落地后切到显式 RuleSet。每请求入口 `rules::snapshot()` clone 一次 Arc 全程用。 |
| C3 | 权限测试归属：T2「157 单测一字不改、不物理搬移」，T5「46 权限单测迁 policy/tests」 | **T5 对（46 个迁 policy/tests/）**。T2 阶段权限单测**原地留在 server**（靠 re-export 编译绿），不搬进 kernel；T5 才物理迁到 policy/tests（28 scope + 15 inject + 3 e2e），断言一字不改。 |
| C4 | semantic 骨架：T1 建空壳，T6 自建最小骨架（ddl.rs/seed.rs） | **顺序执行即无冲突**：T1 先建空 semantic crate（lib.rs 空），T6 在其上加 ddl.rs/seed.rs/migrations/seeds。T6 的「自建」仅在 T1 未做时的降级路径。严格按 T1→T10 顺序执行，不走降级。 |
| C5 | ingest MySQL 入参：T7 写 `&MySqlPool`（过渡） vs 红线 `&ReadOnlyMySql` | **以 T3 交付为准**：T3 先落地 `ReadOnlyMySql`，T7 一律用 `&ReadOnlyMySql`，不允许持裸池过渡。T7 的 autodiscover 动态 SQL 走 T3 的「RawSql→check→inject(unrestricted)→fetch」全管道，不开后门。 |

## 二、各 plan 裁决点拍板

**Task 3**（4 点）：
1. 动态 IN 用 `expand(n)` 方案 ✓（保「LLM 拼接串类型上进不来」，见 C1）。
2. autodiscover 走 unrestricted 全管道 ✓（不开 trusted_dyn 后门）。
3. `DMS_INJECT_STRICT` 开关**保留**（默认 1，成本一行，回退保险丝）。
4. `CheckedSql.tables` **保留**（v1 用于 trace/debug 展示，成本极低）。

**Task 5**（5 点）：
1. 46 测试唯一归属 = **T5 迁 policy/tests**（见 C3），与 plan-t2 已对齐。
2. kernel 内部类型（decide_base/BaseDecision/merge_*/expand_department_tree/dedup_*）**pub 化可接受**（46 断言零改需跨 crate 可见），不加 #[doc(hidden)]。
3. kernel::inject 三参签名 ✓（见 C2）。
4. connector 动态 IN 通道 ✓（见 C1，用 expand）。
5. `invalidate_scope` 生产接线**留 Task 10 管理面**（DMS 主系统改库，dms-ai 只读，无现成写入口）。

**Task 6**（5 点）：
1. semantic 骨架：按 T1→T10 顺序，T1 先建、T6 扩充（见 C4）。
2. 条数口径**以源码实测为准**：WARNS 23/KW_FORCE 36/METRICS 12/DIMENSIONS 9/scope_binding 32（任务书的 26/38/13/10/30 是旧记忆，作废），对拍全等兜底。
3. scope_binding 双真相源折中 ✓：灌库真相=seeds/scope_binding.sql，kernel::builtin_rules 作 PG 缺席兜底，drift 单测锁漂移。
4. seed_rules 删除连带：**知会 plan-t10**，「重置权限档案」入口归 dms_semantic::seed::run_seeds，Task 10 管理面不重复造。
5. 生产 baseline **用户/运维执行**，agent 只交付脚本 + B 库预演证据（不主动碰服务器）。

**Task 7**（6 点，含 plan-t7/fix-t7 补充）：
1. ingest MySQL 入参以 T3 为准 = `&ReadOnlyMySql`（见 C5），不允许 &MySqlPool 过渡。
2. OPT_OUT 入库致 meta.term +7 行：**对拍脚本豁免 status='opt-out' 行**（Task 6 基线不含它，Task 7 新增）。
3. `dim_hit` 死代码**保留**（守「13 测试一字不改」，随 7 个 filter 测试搬入 recall/filter.rs）。
4. meta.rs 删除前置 = Task 6 清场，降级路径（未清场则不删、留段标注）**可接受**。
5. **SchemaCorrector 不进 run_chain**：它的 hint 是 LLM repair() 的自修输入（pipeline.rs:597-603），与四个确定性改写器不同构。链 = GroupBy→Agg→Caliber→Value（四校正器），spec 4.1 的「run_chain(5 校正器)」视为 Task 9（AskRun Repair 轮）终态口径。schema_check 以独立 validator 存在。
6. **测试数口径修正**：实际 corrector.rs 33 个 + meta.rs 13 个（任务书「33 召回+13 校正」写反）；其中 collect×3 + split_top_and×1 共 4 个属 Task 2 kernel，semantic 落地 42 个，合计 46 为验收线。

**Task 8**（5 点）：
1. metric 种子 scope_filter 双写**保留 + 判官核库防漂移**（改动最小；scope_filter 是指标级口径设计位）。
2. corrector 的 ORDER_SCOPE（第 7 处内联）**随提交B 一并改读** `ctx.registry.table_scope_filter("t_sales_order")`（否则口径仍双写一处）。
3. time_col 用**点分限定语法**（`t_sales_order.order_time`），零 DDL 变更，只改一行种子值。不加 time_table 列。
4. Relation/detect_relation **随 fastpath.rs 走**（0-LLM 识别族同源，留 server 会产生 direct.rs 残骸）。
5. doc_binding 迁移编号**接 Task 6 末位**（Task 6 是逐文件递增 0001-0008，doc_binding 开 0009）。

**Task 10**（6 点）：
1. **判官通路事实差异（最重要）**：源码实证三判官（judge_scope/evaluation/regression）走 **CLI 子命令**（subprocess 调 exe），**不碰 HTTP**、不靠 body login_name。故 spec 5.3 第 1 条「认证中间件打挂判官」**前提不成立**。真正风险 = exe 名变化。采纳：**dev_token 逃生门先行 + exe 名保全**，无需给判官发 token。
2. bin 命名**方案 B**：CLI 继承 `dms-ai-server.exe`（判官零改动）、serve 新名 `dms-ai-serve.exe`。
3. identity.rs 做**薄 trait**（遵从 spec 3.4，枚举凭据分派），不做 free fn。
4. chat.rs → chat_store.rs 改名**随 Task 9 Answer 落地时一并改**（不在 Task 10，避免打断 git blame）。
5. dev_token **不进 env 覆盖清单**（防部署环境意外注入开发开关）。
6. viewspec 落点以 Task 2 交付为准（呈现决策树在 kernel），10.8 逻辑一致无需返工。

**Task 9**（3 点，team-lead 亲自写 plan 时定）：
1. Answer.view 在 Table 路径**始终 Some**（驱动侧填 viewspec_build），serde 层 skip_none 仅为非 Table 路径——前端 view 不当可选。
2. 路由对拍**信任 accept 逐字搬 + regression.py 的 route 断言**（regression.py 本就断言 route 字段），不额外编旧 exe。route_diff harness 降级为可选。
3. 汇总步**用 fast-tier LLM summarize**，失败降级 summary=None 不阻断（对齐今天 subs 原样丢前端的兜底）。若实测成本敏感再换模板拼接。

## 二·B、v2 需求裁决（2026-07-27，通用 Agent = 多数据源问数 + 企业知识库）

spec：`specs/2026-07-27-agent-v2-multisource-knowledge.md`；计划：`plans/2026-07-27-trackB-knowledge-multisource.md`。

| # | 决定 | 内容 |
|---|---|---|
| V1 | 文档解析落点 | **扩 `tools/embed_service.py` 为文档服务**（新增 `/parse`+`/chunk`，单端口 :8077）。Rust 侧零新依赖；代价=Python 服务从可降级变成知识库硬依赖（问数路径不受影响） |
| V2 | 上传的 Excel/CSV | **双通道**：①文本化进 `kb.chunk` 供 RAG ②sheet 建 PG 物理表并注册为 datasource，立刻可 NL2SQL（抄 SQLBot） |
| V3 | 多源权限 | **per-datasource 权限插件**：DMS 源走 `DmsDataScope`（现语义一字不改），新源走 `RuleTablePolicy`（`meta.row_rule`/`meta.col_mask`）。注入仍在 AST 层，**明确不抄 SQLBot 的 LLM 改写 SQL** |
| V4 | 排期 | **并行两轨**：轨 A=T1-T10 迁移，轨 B=K1-K6 能力包。并行契约 B1-B7 见轨 B 计划 §0（核心：B 轨只新增文件、迁移号 0020+、K3 依赖 T4） |

对 v1 spec 的三条修订（已写入 v2 spec §0）：
1. **meta 各表必须加 `ds_id`**（v1 写「v1 不加 datasource 列」——多源已成需求，后补代价更大；`DEFAULT 'dms'` 保证存量零迁移）
2. **Dialect v1 落两个实现**（MySQL + Postgres，上传表格落自有 PG 当天就要用）
3. **新增只读源/可写自有库的类型级分离**（`ReadOnlySource` vs `OwnedStore`）——上传建表是写操作，不做类型分离就等于自己给只读红线开洞

新增 crate：`dms_knowledge`（由 T1 一并建空壳）。新增第三方依赖：**零**（`axum` 只开已有依赖的 `multipart` feature）。

读代码发现、与本批一并修的四个既存缺陷（v2 spec §6.5）：
`inject.rs:243` 权限条件 parse 失败静默丢弃（fail-open 红线）／`docker/server/Dockerfile:16` 把含生产口令的 settings COPY 进镜像层（key 建议轮换）／`pipeline.rs:322` `chrono_today` 手算 UTC 致「今天」在 00:00-08:00 差一天／`scope::SCOPE_CACHE` 无失效接口致角色变更当天仍按旧权限出数。

## 二·C、架构终稿裁决（2026-07-27，`docs/ARCHITECTURE.md` 为权威）

文件级架构已定稿：**`docs/ARCHITECTURE.md`**（7 crate 并行设计 + 契约统一 + 4 路对抗评审的合成结果）。
本文件与两份 spec 与之冲突处，一律以 ARCHITECTURE.md 为准。以下是它**推翻或修订**的既有裁决，执行 plan 前必读：

| 原裁决 | 新裁决 | 理由 |
|---|---|---|
| T10-2 方案 B（serve 改名 `dms-ai-serve.exe`） | **推翻：单 bin，名字不变** | `scripts/run.ps1:29` 起的是 `dms-ai-server.exe` 无参（今天=serve），拆后启动的是 CLI → 空 args 直接退出，全栈脚本表面成功、后端没起；`build.ps1:5` 也杀不到改名进程 |
| T10-3 `identity.rs` 做薄 trait | **推翻：两个 free fn** | 两个实现零 dyn 调用点，两个 handler 也不共用路径（一个返 JSON、一个返 302） |
| v1 spec §2.6 `AskRun` sans-IO 状态机 | **删除**，kernel 只留 `SqlTrace`/`Budget`/`AskError`，agent 写显式 `for round in 0..=2` | 被替换的原物只有三个决策，用 575 行 + 8 个回调表达它，顺序全靠驱动侧自觉，出错面反而变大 |
| V3 per-datasource 权限插件（`RowPolicy`/`RuleTablePolicy`/`col_mask`） | **v1 只交付 DMS 语义（=现状快照）**，trait 与规则表推迟到第一个真实第三方源 | 7 个 crate 里零调用点；上传源按 v2 §4.6 走 ds 级 ACL，不用 row_rule。列权限改由「结果列脱敏」承接 |
| T6-2 `builtin_rules` 归 kernel 兜底 | **归 `policy/builtin.rs`，32 表** | 32 个表名+列名直接违反「kernel 零 DMS 字符串」；T5 代码示例与 T6 裁决 3 的措辞同改 |
| T3 「15 个 inject 测试改 newtype 适配版」 | **推翻**：kernel 导出字符串级 `rewrite()`，46 断言一字不改（裁决 C3 优先） | `check()` 会补 `LIMIT 200`，走 newtype 会让 `assert_eq!(out==in)` 假红 |
| T9 `Citation{source,locator}` | **改 v2 §5.3 字段集**（`doc_id/doc_name/chunk_id/page/heading_path/score`） | 前端点开原文需 `chunk_id+page`，塞进字符串等于让前端解字符串 |
| T9 `ComposeAnswerer.accept = is_unrestricted && …` | **推翻：compose/fastpath 的 accept 恒真**，只有 graph 带 `is_unrestricted` 门禁 | `regression_cases.json` D01/D03（tanlibo/city_manager）断言 `route=direct-agg`，加门禁当场红 |
| K1 chunk 700 token / 重叠 80 | **改 400 / 60** | bge-small-zh-v1.5 窗口 512，块尾被 fastembed 静默截断 → 症状是「检索时好时坏」而非报错 |
| 「157 单测全绿」 | **改「156 一字不改搬运 + 1 个随缺陷修复删除 + 约 40 新增」** | `civil_date_sane` 服务的 `civil_from_days` 在 `chrono::Local` 修完后是死代码 |
| v1 §5.4 全仓 `.rs` ≤60 / v2 §8 ≤75 | **改每 crate 预算**（合计 ≈129，见 ARCHITECTURE §1） | 60/75 是没有知识库 crate、没有多源、没有 kb/ds 两组 API 时估的；上限不是目标，「单文件 ≤450 + 一个变更原因」才是 |
| K6 全量交付 | **workspace 隔离（H7）与推荐追问（H10）推迟**，不进本轮预算 | 避免悬空 |

**评审新发现、与迁移同批修的真缺陷**（ARCHITECTURE §3 有 file:line 与修法）：
F1 `inject.rs:243` fail-open（含 `peek_token()!=EOF` 的截断式越权变体）／F2 `ScopeSets::default()` 可铸造无限制 `ScopedSql`（`UnrestrictedProof` 堵）／F3 只读 PG 源可读 `kb.*`/`meta.*`/`chat.*`（PG 角色隔离 + 非业务 schema deny-list）／F4 上传表头以「权威 schema 注释」身份进 SQL prompt，绕开全部 untrusted 机制（`origin` 列 + `sanitize_comment` + `<untrusted_schema>`）／F5 敏感列两份真相源且 `SELECT *` 全绕过（防线移到结果列）／F6 few-shot 与教训跨用户明文泄露（`visibility` 谓词）／F7 scope 缓存无失效且翻页在早八点（TTL 15min + `scope_ver`）／F8 镜像烧凭据、越权删返回假成功、`chrono_today` 差一天。

**待业务裁决（不擅自改）**：受限用户查「只有 customer 段且客户集合为空」的 4 张表时段全空 → 不注入 → 看到全表。**这与 Java 一致**（`空集/不注入 = 放行全部`），改成 `(1=0)` 会让我们与 `judge_scope.py` 的独立 Java 复刻分叉。建议收紧，但需同时改判官并知会 DMS 团队；本轮只加测试钉住现有行为。

## 二·D、T4 落地时的追加裁决（2026-07-27）

| # | 决定 | 理由 |
|---|---|---|
| T4-1 | **`ReadOnlyMySql` 不给 `pool()` 过渡口**（与 `OwnedStore::pool()` 刻意不对称） | 一旦有人为赶进度加上，`fetch` 里的敏感列脱敏（F5）与只读会话都能被绕过，**而且绕过那天没有任何测试会红**。`OwnedStore` 给 `pool()` 是因为它的消费者是代码内字面量 SQL（semantic 30+ 处签名），且 knowledge 已全部转 `fixed()`；只读源没有这种历史包袱，就别开 |
| T4-2 | knowledge 的门禁规则**转 FAIL**（25 处 → 0） | `OwnedStore::fixed(&'static str)` 通道已落地。此后再出现一行 `sqlx::query` 就意味着有人绕开了字面量通道 —— 那正是「把用户问句/文档内容拼进 SQL」的入口 |
| T4-3 | LLM 客户端**不搬 connector**，留 T9 | `impl ChatModel for LlmClient` 已能用；改造 8 处 prompt 调用点属 agent crate 的活，现在搬只是白担风险 |
| T4-4 | `ddl::render_insert` 保留 + `#[allow(dead_code)]` 标名消费者 | 「值只能经 bind、SQL 里绝不出现字面量」这条安全属性已有单测钉住，删了 K4 得重新证一遍。K4 之后仍无调用点则按「没有消费者就删」处理 |
| T4-5 | `RowSet.redacted` 暂无消费者，记为债 | F5 的脱敏本身没丢（是 `fetch` 的内部不变量），缺的是**告诉用户「这列已脱敏」而不是让他以为没数据**。接法是 `Answer` 加一个 skip-if-empty 字段 + 前端角标，additive，随 T9 做 |

**环境风险（不是代码问题，但会同样打挂验收）**：本机 Smart App Control 处于强制态
（`HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy` 的 `VerifiedAndReputablePolicyState = 1`），
会按**内容哈希**随机拦截 cargo 新链接出的未签名 test exe（`os error 4551`）——同一份代码换个注释重编就可能从
blocked 变绿。要可复现的全量验收，需管理员把 `D:\code\dms_ai\target` 加入 SAC 例外或关掉 SAC。
配套两条：① 编译失败后容易撞 `internal compiler error: incremental compilation error`，全程加 `CARGO_INCREMENTAL=0` 即干净；
② 若有人按哈希还原过文件且 mtime 一起回退，cargo 会复用旧产物给**假绿**，验收前先 `cargo clean -p dms-ai-server`。
⚠️ 明确不接受的做法：复制 test exe 再追加 overlay 字节改哈希来骗过 SAC。那是把「验收跑没跑过」这件事变得不可信。

## 二·E、验证环境裁决（2026-07-28）

**本机 Windows 侧 cargo 已完全不可用**：Smart App Control 强制态
（`HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy` 的 `VerifiedAndReputablePolicyState = 1`）
按内容哈希拦截**所有**新链接的未签名产物 —— 不只 test exe，连依赖的 build script、proc-macro DLL、
一行 `fn main(){}` 编出来的 exe 都是 `os error 4551`。改内容重链接 / `--release` / 换目录全新构建三条路都试过，全被拦。

| # | 决定 | 说明 |
|---|---|---|
| E1 | **验证一律走 `scripts/docker-test.ps1`**（`rust:1-slim`，仓库**只读**挂载，产物落 docker volume） | 不是绕过校验，是换一个没有该策略的环境**真跑**。volume `dmsai_cargo`/`dmsai_target` 已预热，全量 build 约 30s。附带收益：Linux 侧没有 SAC，**server 那 140 个断言终于跑得起来** |
| E2 | **禁止**「复制 exe 再追加字节改哈希」 | 它让「验收到底跑没跑过」变得不可信。此前有 agent 用过，已明确不接受、未保留 |
| E3 | 报告纪律：**跑不了就写「未能执行，非代码失败」** | 不许把「编译通过」说成「测试通过」。`docker-test.ps1` 的汇总行会同时报 passed/failed **与执行到的 target 数** —— 「跑不了」和「跑过且全绿」必须能区分 |
| E4 | 容器里必须 `RUSTUP_TOOLCHAIN=1.97.1-x86_64-unknown-linux-gnu` | `rust-toolchain.toml` 钉的是 windows-gnu（主机需要），容器里不绕开会去拉一个跑不了的 target |

要恢复 Windows 侧验证，需管理员把 `D:\code\dms_ai\target` 加入 SAC 例外或关掉 SAC。

## 二·F、T7a 裁决（meta.rs 解体当轮）

**F1 · 门禁对 semantic 不再守 `sqlx::query`，只守「不造池」。**
不是放水：ARCHITECTURE §2 的 I2 残缺列从一开始就写着「semantic 的 `&PgPool` 是字面量 SQL，靠 grep 守」。
原规则是 semantic 只有 present.rs 时写的；registry/recall/ingest 落地后它是 `meta.*` 的唯一读写口，
召回 SQL 必须运行时拼 `{ds_pred}`（谓词的 bind 序号随查询变），进不了 `OwnedStore::fixed(&'static str)` 通道。
**否决 A 案（加 `-WarnOnly` 豁免）**：那是把一条 FAIL 换成一条没人看的 warn。
**否决 B 案（现在就改固定模板 + ds 恒 $1）**：要重写全部 SQL 文本，会让本轮「逐行搬运对拍」失效。
替代物落在 `crates/semantic/tests/drift.rs`，比原规则更紧且 `cargo test` 跑得到（PowerShell 门禁在 Linux CI 跑不到）：
- `every_meta_recall_is_ds_scoped` —— 每条 `FROM meta.` 必带 ds 谓词（原 meta.rs 守卫，判据一字未改）
- `sql_interpolation_is_allowlisted` —— **每个 SQL `format!` 块内的 `{名}` 必须在白名单里**，
  白名单项须写明「该值为何不可能来自外部输入」。原规则只问「用没用 `sqlx::query`」，这条问「拼进 SQL 的到底是什么」。

**F2 · 两条守卫都实测过会红**（守卫搬家最容易变成「永远绿」的哑测试，只说「绿了」不算验收）：
删掉 `registry/model.rs` 的 `{ds_pred}` → 第一条报 `src/registry/model.rs:45 读 meta.metric 却没有 ds 限定`；
往 `recall/pitfall.rs` 的 SQL 插 `LIKE '%{cx.question}%'` → 第二条报 `把 {evil} 拼进了 SQL`。
两处随后与备份逐字节比对还原。**第二条抓的正是「用户问句进 SQL」，那就是它存在的理由。**

**F3 · `probe_sql` 的反引号标识符加 fail-closed 白名单校验（`ident()`）。**
原注释写「表名/列名来自 information_schema，不含任何用户输入」——**那是会静默变假的断言**：
`candidate_columns` 读 `meta.column_doc`，该表由 `ingest::schema_sync` 灌入，而上传源的列名来自用户 Excel 表头（F4 同源）。
今天上传表落 PG、探针只打 MySQL 所以够不着，但「够不着」是两个模块各自的实现细节，不是不变量。
一个含反引号的列名闭合掉引号 = 一条带 `unrestricted` 放行、无行级过滤的任意读。守卫：`backtick_identifier_cannot_break_out`。

**F4 · CLI 管理任务的放行凭证收回 `dms_policy::proof::for_admin_cli`。**
搬迁后 `main.rs:104` 变成 server 自己 `UnrestrictedProof::new(&ScopeSets::default(), true)` ——
那个 `true` 是硬编码自证（与 F2 套路一字不差），且让 proof.rs 开头「那条 grep 就是全仓放行清单」变成谎话。
新铸造点的第二证据是**进程形态而非身份**（这类任务确实没有「以谁的身份查」）：校验 argv 真的是该子命令。
不用全局标志位，所以没有初始化顺序可忘；谁把这行粘进 axum handler 都铸不出（服务进程 argv 里没有它）。
守卫：`admin_cli_proof_needs_matching_argv`（测试进程 argv 不匹配 → 必须 `None`）。

**F5 · 本轮不做的两件**（都因为验收标准是「连库对拍全等」，PG 连不上就改 = 无法证明没漂）：
种子外置成 `seeds/*.sql`、DDL 改 `sqlx::migrate!`。9 组种子原样搬为 Rust const；
代价是 `seed_metrics` 102 行 / `seed_value_maps` 85 行超 60 行硬线（超出部分 100% 是必须逐字不变的 const 数据表），
这笔债随外置一并消。

## 二·G、口径回炉的第一次实测：净 −1，以及它教的四件事

**度量（生产 MySQL + 本机 PG，我本人跑）**：回归 52/1 → **53/1**（B06 转绿，剩 B10 是已证抖动）；
执行级评测 **32/38 → 31/38**。**净 −1，是退化。**

| 题 | 基线 | 本轮 | 归因 |
|---|---|---|---|
| GOODS15 动销商品数 | ❌ 292 vs 173 | ✅ | 明细表口径进 `table_scope` + `add_scope_filter` 放宽 JOIN 门 |
| FIN04 信控余额清单 | ❌ 21 vs 23 行 | ✅ | `RequireLatest` 判红 → 回炉 → LLM 补出 `ROW_NUMBER() … rn=1`。**回炉确实会救题** |
| GOODS17 分类销量Top5 | ✅ | ❌ 184616 vs 141502 | **回炉打坏的**：判词只要明细表的 `deleted_flag`，LLM 却整条重构，把真实的分类 JOIN 换成 `LEFT(sku_name,2)` 编了个「分类」并多出两列 |
| STK01 / STK02 | ✅ | ❌ | 疑抖动（STK01「列数 3≠2」、STK02 生成的 SQL 读来正确）。未坐实，不算修好也不算打坏 |
| AS04 占比 | ❌ 0.049 | ❌ 0.0489 | 指标未注册（当轮明令不凭空造）**且**撞上 G1 那个误判 |
| GOODS16 / SALE15 | ❌ | ❌ | 值域词典缺失，与本轮无关 |

**G1 · `check_caliber` 对「只出现在投影子查询里的表」恒判违规**（真 bug，已修）。
`Grab` 的 `cond` 是**进入点决定**的：投影里的标量子查询以 `cond=false` 进来，于是
`pre_visit_table_factor` 登记了表、而它 `WHERE` 里的列一列都不记 → 表在册、约束不在册。
占比类派生指标正是这个形状（分子分母各一个标量子查询，两侧 WHERE 都写足了口径列）——
被判红、回炉一轮后仍红，白烧一次 precise LLM，还给用户挂上一条**假的**「结果不可信」。
修法：`pre_visit_query` 把子查询交回 `scan_query` 走一遍。守卫 `cols_seen_inside_projection_subquery`
同时锁住「真漏一列仍要判红」。

**G2 · 指标级口径补全对销量类指标一直是死的**（真 bug，已修）。
`correct_caliber` 把注册表里**带人类注解**的 `source_table` 整串喂给 `locate_target`，
而 `sales_qty` 的值是 `t_sales_order_detail(JOIN t_sales_order 有效订单)` —— 与真实表名永不相等，
于是 `item_type='1'` 从未被补过。不能用 `strip_annotations`（它刻意保留半角括号，否则切坏 `COUNT(x)`），
改取首个标识符。

**G3 · 表级声明只有校验器在读，补全器不读** → 能确定性补的却去回炉（已修）。
`meta.table_scope` 的声明此前只喂给 `build_rules`/`check_caliber`；`correct_caliber` 只遍历指标级
`scope_filter`。于是「明细表漏 `deleted_flag`」这种完全能 AST 补上的问题，只能靠回炉让 LLM 重写整条 SQL
—— 而重写会连带改坏与违规无关的正确部分（GOODS17 就是）。
**裁决：能确定性补的不许回炉。** `correct_caliber` 现在也遍历 `table_scope`。
这是同一个「半个语义层」的病下沉一层：声明存在、校验器读得到、执行者读不到。

**G4 · 「只补口径」必须可检查，光在 prompt 里请求不算数**（裁决）。
`repair_instruction` 末句写着「只补口径，不要改变原有的查询意图、输出列与排序」，
但那只是一句请求。**裁决：口径回炉只采纳输出列未变的改写**；变了就保留原 SQL 并如实标注口径未过。
判据落在 `kernel::keeps_output_shape`（纯函数，解析不出时返 `true` —— 与 `check_caliber` 同一漏判方向，
不许因为「看不懂」丢掉一次本可能正确的自修）。
**刻意不用「违规数变少就采纳」**：GOODS17 那次重写**确实**修掉了违规，只是顺手编了个分类 ——
按违规数单调根本挡不住它，按输出列才挡得住。

**G5 · 一个方法论结论**：口径校验器上线的第一效果是**净负**的。
声明越准，回炉越频；回炉是盲的，它只知道「违反了声明」，不知道原 SQL 哪里是对的。
所以顺序必须是：**先把能确定性补的补掉（AST，零 LLM），再把剩下的交回炉，而回炉必须受形状约束**。
反过来做（先上回炉、指望 LLM 自己拿捏）就是这轮实测到的 −1。

## 二·H、第三轮实测：33/38，以及两个新的可判定错误类

**度量（我本人跑）**：执行级评测 **32/38（基线）→ 31/38（口径回炉刚上线）→ 33/38 = 86.8%**。
回归 53/1（B10 抖动）。p50 18.9s（回炉命中的题多一轮 precise LLM，是回炉的代价不是 bug）。

| 题 | 基线 | 二轮 | 三轮 | 结论 |
|---|---|---|---|---|
| GOODS15 动销商品数 | ❌ 292 vs 173 | ✅ | ✅ | 明细表口径进 `table_scope` + JOIN 门 + G2/G3 确定性补全 |
| AS04 退款占比 | ❌ 0.049 | ❌ | **✅** | `refund_ratio` 指标 `unit=percent` + G1 修掉的投影子查询误判 |
| GOODS16 手抓饼分类 | ❌ 虚高 36% | ❌ 0.0 | **✅** | 值域词典（autodiscover 名称型探针灌入 68 个分类名） |
| GOODS17 / STK01 / STK02 | ✅ | ❌ | **✅** | **证实二轮那三条是回炉打坏的、不是抖动**（G2/G3 + G4 形状门修回） |
| FIN02 账户余额TOP10 | ❌ | ❌ | ❌ | 真缺陷，见 H1 |
| FIN04 信控余额清单 | ❌ 21 行 | ✅ 23 行 | ❌ 28 行 | **翻面**：`RequireLatest` 回炉有效但不稳定 |
| SALE15 / SALE16 / SALE17 | ❌/✅/✅ | ❌/✅/✅ | ❌/❌/❌ | 见 H2、H3 |

**H1 · FIN02 缺的是声明，不是判据。** gold 用 `balance_type IN ('8','9')`（可开票+不可开票＝账户余额），
而 `meta.table_snapshot.extra_filter` 只声明了 `balance_status='4'`。
`value_map` 里「可开票余额→8」「不可开票余额→9」「信控→1」都有，但**没有任何声明说「账户余额」＝ 8+9**。
这是业务口径缺登记，不是校验器的问题。

**H2 · 新发现的可判定错误类：「取对了码、用错了列」。**
SALE17「本月湖南省的销售额」——SQL 写 `t_sales_order.receiver_province = '430000'`（**收货省份**），
gold 用 `t_customer.province` → `t_regions` 翻名。**码是对的**（430000 就是湖南，来自
`value_map` 的取值提示卡，那张卡明写了 `t_customer.province`），**列是错的**。
今天没有任何判据管这件事：`RequireJoinAndFilter` 只对**名称型**值域 + 问句含「分类/品类/类别」时才造。
**该推广到码值域**：值域命中给出 `(表, 列, 码)`，若 SQL 里出现了那个码字面量却不在那一列上 → 违规。
这是 NL2SQL 的经典错法，且完全可判定。
注意：本轮 prompt 对这个问句**没有任何变化**（省份的 34 个码值早就在库里，autodiscover 新增的 68 条是分类名，
不命中该问句）—— 所以 SALE17 是**一直脆弱、这轮翻面**，不是本轮引入。

**H3 · 残余失败里有一半是「LLM 多输出一列」，不是数字错。**
SALE15（行数报警，实际单跑 10 行口径全对，但 3 列 vs gold 2 列）、SALE16（列数 3≠1）、
STK01（列数 3≠2）同一形态。系统提示里没有「只输出问句要的列」这条纪律。
加它是**产品性质**（BI 答案应当只答所问），不是为了刷分 —— 但要如实记明：
它同时会改善评测分，因为评测严格比列数。

**H4 · 一句关于「回炉」的复盘。** 三轮曲线 32 → 31 → 33 说明：
口径校验器**单独上线是净负的**（−1），配上「先 AST 确定性补、回炉受输出列约束」之后才转正（+1）。
二·G 那条方法论结论被实测确认了一遍。

## 二·I、T9 裁决：`pipeline.rs` 解体入 agent，Router 五位齐全

**成果**：`server/src/pipeline.rs`（1280 行）、`server/src/graph.rs`、`server/src/triage.rs` **三个文件删除**；
`dms-agent` 从 1 个实现文件（`guard.rs`）长到 17 个 `.rs`；AGE 图 IO 迁 `dms_connector::graph`
（server 的 `sqlx::query` **25 → 17**）。树 **451 passed / 0 failed（20 target）**、门禁 exit 0。
断言名字级对账：HEAD 155 个唯一名 → 现在 438 个，「消失」的**恰好 1 条**（`civil_date_sane`，
随 `chrono::Local` 缺陷修复一并删除的已裁决项）。

**I1 · 三处 agent 顶着任务书改对了的，全部采纳。**
① `Answerer::answer` 返回 `Result<Option<AskResult>>` 而非我写的 `Option`：`Option` 会把权限注入失败
（未登记表/条件不可解析）吞成「没接住」→ 静默交给下一个成员（LLM），**把 fail-closed 降级成重试**，破 I3。
② `AskCtx.llm` 用 `&Arc<dyn ChatModel>` 而非 `&LlmClient`：后者带 reqwest 且住在 server，引它就是反向依赖边。
③ graph 的 `accept` 用 `for_principal(p, scope).is_some()` 而非逐字的 `sets().is_unrestricted()`：
差别只在「集合三维度全空但角色档没授予全部」（＝忘了算权限/脏数据）—— 旧码放它进图库读**全量**购买关系
（图无行级过滤、Cypher 进不了闸门），新码回落 LLM 走注入。**只收紧不放宽**，三题图问句是 admin，逐题不变。

**I2 · Router 补到五位**（我在收尾后做的）。第五位 `llm` 一度在表外由 `ask_single` 直调，
根因是同一个：`LlmAnswerer` 拿不到 token 用量回调（挂 no-op ＝ 查询日志 token 列静默变空，K6-B）
也拿不到单问起点 `t0`（自取 `Instant::now()` ＝ 把自己之前的耗时全丢掉，缓存那处实测偏小十几毫秒）。
两样收进 `AskCtx` 后它成为普通成员。**「加一种能力＝加一个 Answerer」这句话在此之前只有 4/5 成立** ——
这类「抽象差最后一位」的状态最容易被当成已完成，所以把断言从「比前四位」改成「与契约表全等」。

**I3 · 又删掉一个守着空气的守卫**：`default_router()` 是地基阶段的占位（返回 `vec![]`），
五个成员落地后没有消费者，但 `route_label_map` 还拿它取标签 → 那条「Router 顺序必须是契约子序列」
的断言**对空表恒真**。删函数 + 把顺序契约指向 `ask::router_is_the_contract_in_full`。
这是本项目第三次遇到同一形态（前两次：漂移守卫搬家、`JOIN meta.` 的裸 `ds_id`）——
**判据的入参一旦变空/变宽，断言就悄悄变成恒真**，而测试报告只会显示绿。

**I4 · 一处事实错误纠正**：全仓 7 处注释写「28 题断言 `direct-agg`」，实为 **26 题**
（题集实际分布：`direct-agg` 26 / `direct-doc` 1 / `graph` 3 / `compound` 2）。
错源是我的任务书，被逐轮抄进代码注释。收尾 agent 查出来的。

**I5 · 两笔明确记债的临时形态**（都标了 `ponytail:`）：`direct.rs` 的三个确定性命中入口与
`corrector.rs` 的五个校正器今天是 agent 的**入参**（由 server 在组 Router 时注入）——
agent 引 server 是反向依赖边。消掉它们的时机是 T8（`compose/*`+`fastpath/*` 迁 semantic、
校正器迁 semantic），届时 `AskDeps` 的三个 `fn` 字段与 `Correctors` trait 一起删。

## 二·J（**已作废，见 二·J′**）、确定性模板违反表级声明：「销售额按商品分类」虚高 2.56 倍

> 🔴 **本节结论是错的，保留原文只为记住错法。** 「虚高 2.56 倍」来自一次**测错的对比**：
> 按 `item_type` 分组求和时**没有加模板里的 DISTINCT 去重**，拿一个未去重的和（336M）
> 去比一个已去重的实现（208M）。照这个结论改代码，会把一条基本正确的模板改成低报 36% 的错数。
> 正确的测量与结论在 **二·J′**。



顺着 B10 那条「一直翻面的回归题」查下去，挖到的不是抖动，是一个**确定性路径（0-LLM）上的真错数**。

**怎么被发现的**：B10「销售额top3商品分类」隔离跑 26s、直接问 33s，而 `EXEC_TIMEOUT = 30s`
—— 它正好压在超时线上，所以路由断言必然翻面（超时 → fetch 失败 → 回落 LLM → `route=llm` ≠ `direct-agg`）。
它慢是因为问句**没有时间词**，模板不加时间过滤 → 百万行明细表全历史扫描。
**看那条 SQL 时才注意到：它触 `t_sales_order_detail` 却没有 `item_type = '1'`** ——
而 `meta.table_scope` 明确声明了那张表恒需 `item_type = '1' AND deleted_flag = 0`。

**实测（生产库，本月有效订单，只读）**：

| item_type | 行数 | 金额合计 | 箱数合计 |
|---|---|---|---|
| 1 正品行 | 91,642 | **131,436,696** | **1,408,190** |
| 2 赠品 | 20,939 | 0.00 | 65,389 |
| 3 **结算行** | 112,581 | **204,547,973** | 1,473,579 |

模板三类全加：金额 **335,984,670 vs 正确 131,436,696 → 虚高 156%（2.56 倍）**。
结算行携带的金额比正品行还多，所以这不是边角误差。

**路径定位（精确到一个分支）**：
- 「**销量**按分类」→ 命中销量指标 → `try_compose` → 应用指标 `scope_filter = item_type='1'` → **正确**（评测 E05 过）
- 「**销售额**按分类」→ compose 因**扇出检查**主动放弃（`SUM(total_amount)` 跨 1:N JOIN 会虚增，**拒得对**）
  → 回落硬编码模板 `direct::sales_breakdown` 的 `SalesDim::Category` 分支
  → 该模板在明细级 `SUM(dd.amount)`（层级对了）**但漏了 `item_type`** → 2.56 倍
- 那是 `direct.rs` 里**唯一**触明细表的模板，其余 SalesDim 分支走订单头，不受影响

**J1 · 为什么两套测试都看不见它**（这条比缺陷本身更值得记）：
- 回归 B02「销售额按商品分类」**是过的** —— 它只断言 `route=direct-agg` 与 SQL 含 `t_goods_category`，**不比数字**。
  这正是当初造执行级评测要解决的问题（「不比 SQL 文本，比两边各自执行的结果集」）。
- 评测里**没有**「销售额按分类」这道题（E05 是**销量**按分类）。
- 于是：一个 2.56 倍的错数，在最高频的 BI 问法之一上，被「一条只看路由的回归」和「一道缺失的评测题」同时放过。

**J2 · 结构性根因：确定性路径不跑 grader，所以它违反声明也无人知道。**
裁决 二·G 当时写的理由是「compose 的 SQL 就是按同一批声明装配的，判红只说明装配器与校验器理解不一致」——
**那个前提对 `compose` 成立，对硬编码模板不成立**：`sales_breakdown` 早于声明层存在，从来不读 `table_scope`。
**修法不是让运行时跑 grader**（那会把「回炉改坏对的 SQL」的风险引进确定性路径），
而是**在测试里用 `check_caliber` 校验模板产出**：构造各模板的 SQL → 喂声明 → 断言零违规。
构建期抓声明/模板漂移，零运行时成本，也没有「回炉」那类副作用。

**J3 · B10 的超时本身不改，但**要停止把它当抖动**。** 26~33s 是一条**没有时间边界**的
全历史聚合的真实代价（`EXEC_TIMEOUT = 30s`）。至此它在批量下失败 3 次、通过 1 次，
**隔离跑每次都过**（26s、`route=direct-agg`）。
**这条回归题表面断言路由，实际断言的是「一条 26~33 秒的查询能不能跑进 30 秒超时」** ——
批量下远程 MySQL 一竞争就超时 → fetch 失败 → 回落 LLM → `route=llm ≠ direct-agg`。
**不要再每轮把它记成「抖动」**：它是一条伪装成路由断言的性能断言，翻面是判据设计的结果，不是噪声。
三条出路都要业务裁决，不擅自做：① 无时间词时给个默认时间窗（**改答案语义**）；
② 抬 `EXEC_TIMEOUT`（**掩盖问题**）；③ 把该题的断言从 `route` 改成「有数且行数=3」
（**承认它测的不是路由**，但会丢掉「确定性路径该接住它」这个真意图）。

## 二·J′、订正：表级声明 `item_type='1'` 是**过宽的**，而我把它推到了 LLM 路径

**正确的实测（生产库，本月有效订单，全部按模板那个 DISTINCT 去重后）**：

| 口径 | 金额 | 与订单头之差 |
|---|---|---|
| 订单头 `SUM(total_amount)`（`sales_amount` 指标，权威定义） | **204,519,026** | —— |
| 明细 `item_type='3'`（结算行） | **204,543,893** | **+0.012%** |
| 明细 `item_type='1'`（正品行） | 131,436,696 | **−35.7%** |
| 明细 不筛 item_type（**模板现状**） | 208,131,408 | +1.77% |

去重后箱数：1 类 1,408,160 / 2 类 65,342 / 3 类 1,473,502。

**J′1 · 明细级「金额」的正确过滤是 `item_type='3'`，不是 `'1'`。**
结算行携带的就是结算金额，与订单头相差 0.012%。而 `'1'`（正品行按目录价）低报 36%。
**「数量」用 `'1'`**（GOODS16 gold 与评测 E05 都坐实了）。
也就是说 **`item_type` 的正确取值取决于问的是金额还是数量** —— 它是**指标级**口径，不是表级恒需。

**J′2 · `meta.table_scope` 的 `t_sales_order_detail | item_type = '1' AND deleted_flag = 0` 声明过宽。**
它是在口径轮由我的任务书要求加进去的，依据是 GOODS15/GOODS16 两道 gold —— **两道都是数量/计数题**。
把它当表级恒需，对金额类问题就是错的。
**修法**：表级只留 `deleted_flag = 0`（那才是真正恒需的），
`item_type='1'` 回到指标级（`sales_qty.scope_filter` 本来就有），
并给「动销商品数」注册一个带 `item_type='1'` 的指标 —— 否则 GOODS15（292→173）会退回去。

**J′3 · 我在 G3 引入的风险（本节最要紧的一条）**：
G3 让 `correct_caliber` 把**表级**声明也确定性补到 LLM 路径的 SQL 上。
于是一条 LLM 生成的「明细金额按分类」会被强行加上 `item_type='1'` → **低报 36%**。
评测三轮都没抓到，因为**没有一道题覆盖「明细金额按分类」**（E05 是销量、B02 只断言路由不比数字）。
G3 本身的方向是对的（能确定性补的不该回炉），错在**被补的那条声明本身过宽**。

**J′4 · 模板「基本正确」是碰巧的，这件事仍要修。**
`sales_breakdown` 的 DISTINCT 键是 `(单号, sku, 箱数, 袋数, 金额)`——**`item_type` 不在键里**，
于是同一 (单, sku) 的正品行与结算行在值相同时被折成一行，结果落到 208.1M（高 1.77%）。
它不是按声明算出来的，是被一个恰好起作用的去重键救的。真修法是显式 `item_type='3'`（金额口径）。

**J′5 · 方法论：这次的错法值得单独记。**
我在拿到「336M vs 131M」之后**立刻**写了「虚高 2.56 倍」并落档，
错在**把一个未去重的分组和当成了实现的行为** —— 实现里有 DISTINCT，我的验证查询里没有。
唯一救回来的动作是「改代码前再测一次，用权威定义（订单头）做第三方参照」。
**教训：口径类结论必须有第三个数做交叉验证**，两个数之间的比值永远可以自圆其说。

## 二·K、「上传即可问数」：一句「未实测」压着三个都不报错的缺陷

第四轮评测（34/38）跑完后，我去验 `INTEGRATION-TRACE` 里长期标「半」的那条：
**上传表格双通道 —— 建表与入库已落地，问数通道未连库实测**。
一次实测同时暴露三个缺陷，三个都**不报错、不进日志**，症状统一是
「建表成功、知识库检索可用、只有问数死掉」——最难归因的那种半可用。

**K1 · 方言硬写 MySQL（影响面最大，与上传无关）**
`build_system_prompt` / `build_repair_prompt` / `gate_on` / `ensure_limit` 四处都写死
`MysqlDialect`，而两个 prompt 模板**本来就有 `{dialect}` 占位**——占位是对的，喂进去的值是错的。
硬规则 1 又写着「结果列别名用反引号包裹」，于是 LLM 对 PG 源老实写出 ``AS `人数` ``，
PG 回 ``syntax error at or near "`"``。**任何非 MySQL 源的问数恒失败。**

闸门那两处比 prompt 更隐蔽：红线校验是靠**解析**做的，方言错 = 解析错 = `GuardError` =
「这条 SQL 不合格」→ 静默回落/自修。对 PG 源最先撞上的是 `::` 转换
（上传表列多为 text，PG 侧数值聚合的自然写法是 `SUM(c::numeric)`）。

修法：`Dialect` 加回 `quote()`（它曾以「零消费者」之名被删——**现在的消费者正是这个缺陷**），
四处改收 `cx.source.dialect()`。**不留默认值**：这种签名上的默认值等于把缺陷改成难以察觉版。

**K2 · 上传源建池不置 `search_path`**
上传源共用一条 `pg_ro_url`、schema 一份一个（`up_<doc_id>`），
而 PG 探针按 `n.nspname = current_schema()` 过滤 → 恒查 `public` → 一张表都采不到 →
`meta.table_doc` 里只有 `ds_id='dms'` 的 251 行 → LLM 的「可用表结构」是**空段** →
它照别处的表名硬猜。修法：`DsSpec.schema` + `after_connect` 置 `search_path`，
取值一律来自 `tabular::upload_schema_of_ds(ds_id)`（与 `upload_ds_id`/`schema_ident` 同源于
doc_id，不许在别处再拼一次）。schema 名进的是语句文本（`SET` 不吃 bind），
故必须过 `SafeIdent`，**过不了就 Err，不许清洗后放行**——清洗会把「配错的 schema」
变成「连上了另一个 schema」，那比连不上危险。

**K3 · 备份表启发式误伤上传表名（约每 6 份上传 1 份）**
`is_backup_table` 首条规则是「表名结尾连续 ≥4 位数字」，上传表叫 `t0_<uuid 去横线>`，
uuid 末段 12 位十六进制里末 4 位全落 0-9 的概率 ≈ (10/16)^4 ≈ **15%**；
首段 8 位全为数字（≈2.3%）还会撞「8 位日期段」那条。命中即被当垃圾表跳过，
该文档的 schema 永不入注册表。**这份实测用的 uuid 侥幸没中**，所以第一次上传不会暴露它。
修法：`sync_schema` 加 `filter_backup`——**别人建的库传 true，自己建名的库传 false**。
对自己生成的名字跑「猜猜这是不是垃圾表」的启发式，按构造就是错的。

**K4 · 三条判据现在有脚本守：`tools/up_probe.py`（exit 0）**
判据刻意**不写它没做的检查**：脚本不直连 PG，故「schema 是否入注册表」由行为反推——
模型写得出清洗后的列名 `c0`/`c2` 才说明列注释（中文表头）进了 prompt。
中文表头进列注释而非列名是刻意的（标识符安全 + I5），所以「用对 c2」是这条链路
唯一的可观测证据。实测 SQL：`SELECT SUM(c2) AS "总销量" FROM t0_… WHERE c0 LIKE '%销售部%'`
——双引号别名 + 裸表名 + 结果 600（340+260）。
**红态不是模拟的**：改动前用同一个脚本跑出来就是 ``syntax error at or near "`"``。

**K5 · 顺带把两处已经不成立的 ⚠️ 注释改掉。**
`connector/src/postgres.rs` 与 PG 两条探针都标着「未连库验证」，这次验过了
（`attnum` int2 解码、类型映射、列注释全部正确）。过期的警告会让人不信真的警告。

**K5a · `filter_backup=false` 补了端到端证据（单测钉不住调用点那个 bool）。**
单测只证「谓词会 flag 这类表名」，证不了「调用点传的是 false」——传错是不可见的。
补法：循环上传直到 server 生成的 doc_id 让表名踩中启发式（第 2 次即中：
`t0_81c4e0d0_b2cc_47eb_b337_e16e35909449`，结尾 8 位连续数字），再查
`meta.table_doc[该 ds] = 1 行`。传错的话这里会是 0 行且日志无声。

**K5b · 方言穿线动了 `gate_on`（所有 DMS 取数都过它）→ 回归 54/0/0 全绿。**
26 条 `direct-agg`、3 条 `graph`、2 条 `compound`、3 条红线、权限隔离对比全部照旧；
B10 这次 24.2s 没撞 30s 上限（它是性能题，不是路由题，见 二·H）。

**K5c · 每个残留的上传源都会去竞争所有问句的路由。**
`select_source` 在「可见源 > 1」时 embed 问句去挑源。今天看不出来只因
`meta.datasource.embedding` 一行都没写过（`nearest_datasources` 恒空 → 降级主源）——
**写入点已经有了**（`tools/embed_service.py build` 的 datasource 分支），只是从未跑；
`agent/src/source.rs` 里那句「目前没有写入点」已过期，本轮改成了准确的说法。
故 `tools/up_probe.py` **默认自清理**（`DELETE /api/kb/doc/{id}` 连带 DROP schema + 注销源）。
⚠️ `tools/kb_eval.py` 的注入 fixture（`员工台账_表头注入.csv`）仍会留一个 active 上传源——
向量选源真上线那天要先把这类测试残留清干净，且必须重跑回归+评测：
**它改的是每一句问话的选源行为，不是一个新功能开关。**

**K5d · 我作废了一趟评测：测量进行中重建了容器。**
为了跑一个诊断子命令（`why-not-compose`）执行了 `serve.ps1 -Build`，
它 `docker rm -f` 掉了评测正在对话的那个容器 → 从那一题起全是
`Error response from daemon: container …`。E09 之后的结果整段无效。
这条与 二·E/E2 同一族：**任何让「这一趟到底测了什么」变得不可信的操作都等于没测**。
纪律：**测量在跑 → 不重建、不重启容器；要加子命令就等它跑完**
（`docker-test.ps1` 只在测试容器里编译，不动服务容器，那个是安全的）。

**K6 · 枪测流程自己有个坑：`Move-Item` 还原会把旧 mtime 一起还原。**
枪测的标准动作是「Copy-Item 备份 → 改坏 → 跑测试必须红 → Move-Item 还原 → 再跑必须绿」。
问题在最后一步：`Move-Item` 恢复的文件带着**改坏之前**的时间戳，比刚编出来的产物还老，
cargo 按 mtime 判新旧 → **不重编**，「还原后那一跑」跑的其实还是被打坏那版二进制。
本轮就撞上了：还原后 kernel 仍报 1 红，源码 grep 明明是对的。
`(Get-Item $f).LastWriteTime = Get-Date` 之后重跑才是真绿（476 passed）。
这与 二·E/E2 记的那条同类 —— **凡是让「验收到底跑了没有」变得不可信的操作都不算验收**。
还原后必须 touch，或者干脆用 `git checkout -- <file>`（它写新 mtime）。

## 二·L、第四轮评测：34/38 同分，但失败集换了两个（噪声与系统性要分开报）

失败集：round4 `GOODS13/MKT04/SALE15/STK02` → round5 `AS03/GOODS17/MKT04/SALE15`。
GOODS13、STK02 转绿；GOODS17、AS03 转红。**净 0。**
我上一轮给 `RequireDedup` 键集判据定的验收判据是「SALE15 转绿」——**没达成**。

**L1 · GOODS17 是噪声，不是回归。** 今天单跑，模型输出与 gold 逐值一致
（未分类 233636 / 脆皮烤肠 212576 / 商用蛋挞 141502 / 手抓饼 115175 / 蛋挞 85762，
gold SQL 现跑同值）。评测那次它的 商用蛋挞 = 184616（+30.5%），不可复现。
MKT04 的 2.007× 早前已判同类。**4/38 在两轮间来回翻，单题判定在 34/38 这个水位属噪声。**

**L2 · SALE15 是系统性的，根因不是去重键，是「没有声明商品维度」。**
算术闭合：`goods_code = 5001070013220907` 在明细里有两个 `sku_name`
（`…蛋挞液0907G22` 76,967 + `…蛋挞液907g/瓶 整箱12瓶` 14,983）= **91,950 = 模型给的那个数**。
gold 按 `t.sku_name` 分组 → 同一 SKU 被拆成两行；模型按 `g.goods_name` 分组 → 合并成一行。
先排掉了两个更省事的解释（都不成立，各有一条查询为证）：这三个商品各只对 1 个
`goods_code`（无一名多码）、`t_goods` 无重复 `goods_code`（无 1:N 扇出）。

`meta.dimension` 里有 商品分类、品牌，**没有 商品**——最基础的那个维度是空的。
于是「卖得最好的商品」按什么列分组每轮由模型自己挑，答案在两个口径间漂。

**L3 · 方向对 gold 不利，所以我不改任何一侧。**
对「卖得最好的 10 个商品」，按主数据身份（`goods_code`/`goods_name`）合并比按明细里的
历史名拆开更站得住——也就是说**模型可能是对的、gold 可能是错的**。
这是业务口径裁决（与 二·J′ 的 `item_type` 同类），交 DMS 团队：
- 若判「按主数据合并」→ 改 gold，并把 商品 维度声明成主数据名；
- 若判「按明细名」→ 把 商品 维度声明成 `t_sales_order_detail.sku_name`。
**两种判法都要求 `meta.dimension` 补上 商品 这一行**：不声明就永远是随机的。
我只做到「把机制算清、把选项摆出」，不替业务选，也不为了让评测变绿去钉弱的那个口径。

**L4 · `RequireDedup` 的键集判据为什么没触发（判据本身的盲区）。**
模型写的是 `WITH dedup AS (SELECT DISTINCT …) … SUM(dedup.box_quantity)`。
判据的前置条件是「聚合列前缀属于声明表的别名（`d`）」，而**CTE 把别名洗掉了**——
聚合发生在 `dedup.` 上。于是规则结构上看不见 LLM 实际最常写的那个形状，
`correction_log` 全空。修法（未做）：`Facts` 记 CTE/派生表名 → 其内部引用的表，
让前缀命中 CTE 时也回溯到声明表。**这一条现在是已知盲区，不是已修项。**

## 二·M、SC 自一致采样：按实测证据挑的下一件，且它证明了「哪些题是噪声」

**M1 · 动因是证据不是清单。** 二·L 量到：两轮评测都停 34/38 而失败集换了两个，
同一道题今天与 gold 逐值一致、评测那次却高 30%。温度已经是 0.1，压不下去了。
所以下一件不是照抄 P1 清单里最上面那个，而是**冲着这个误差源**去的。

**M2 · 投票投在结果上，不投在 SQL 文本上。**
两条写法不同的 SQL 可以给同一个数（格式/别名/等价改写），两条几乎相同的可以差 30%
（少一个去重键）。用户要的是数对，不是 SQL 长得像。
指纹**只看值不看列名**：中文别名每轮措辞会变（「销量」/「总销量」），
把列名算进去就等于让 SC 永不收敛 —— 而那不报错，只表现为「每次都无多数派 +
三倍开销 + 一句『数字不可信』」，比不开 SC 更糟。有断言钉住这条。

**M3 · 默认 1 ＝ 关，且判官用同一个配置值。**
`sc_samples <= 1` 时 `run_llm` 直接返回 `run_once`，不多一次调用、不多一个分支。
CLI `ask` 子命令传的是 `cfg.sc_samples` 而不是写死 1 —— 写死会让
「开了 SC 之后评测有没有变好」这个问题**永远量不出来**（同 `exec-sql` 那条判官纪律）。
顺序执行 + 前两次一致就提前收工：B10 那类单次 24s 的题，三份并发同时打库是自找超时；
提前收工让常见情形只多付一次而不是 N−1 次（实测 MKT04 `samples=2 winner=0`）。

**M4 · 多数派缺席不静默挑一个** → 返回首次 + `caliber_note` 明说数字不可信，
与口径回炉预算用尽同一条口径。

**M5 · SC=3 在那 4 道失败题上的实测，比总分更有信息量：**
- **MKT04 ✅**（原 2116662.33 = gold 的 2.007 倍 → 1056437.01，差 0.185% < 容差 0.5%）
- **GOODS17 ✅**
- **SALE15 ❌**、**AS03 ❌**，且 3 次采样给**同一个**错值

也就是说 SC **修掉了飘的两道、没有替系统性的两道遮掩**。反过来这也是判据：
AS03 此前被我列为「可能是噪声」，SC 的一致性恰好证明它是系统性的。

**M5′ · 全量实测的裁决：SC=3 不改善总分，代价是 2.5 倍 p95 —— 默认保持 1。**

|  | 通过 | 失败集 | p50 | p95 |
|---|---|---|---|---|
| sc=1 | 34/38 | AS03 / GOODS17 / MKT04 / SALE15 | 19,419ms | 62,375ms |
| sc=3 | 34/38 | AS03 / **E05** / **GOODS13** / SALE15 | 26,603ms | **153,039ms** |

M5 那次定向跑（4 题）说对了一半：MKT04、GOODS17 确实转绿。
但**全量下 E05、GOODS13 转红，净 0**。而且不是随机换人 ——
GOODS13 这次 **+25.9%**，正是它原始缺陷的那个 +26% 签名。

**机制上的坏消息（这才是本条的价值）**：SC 向**众数**收敛。
众数对时它把「偶尔靠运气错」变成「稳定地对」；
**众数错时它把「偶尔靠运气对」变成「稳定地错」**。
GOODS13 的众数行为就是那条错 SQL（口径判红 → 回炉 → 三份里两份仍错 → 多数派＝错）。
也就是说 SC 的收益与损失是**对称的**，而这套题上两边正好抵消。

所以：**功能留着（默认关、开销为零、诊断价值已兑现——它证明了 AS03/SALE15 是系统性的），
但不推荐打开**。真正该做的是把众数本身弄对（声明补全 + 判词可执行 + 确定性重写），
而不是对一个错的众数投票。
`分层` 也印证：`去重` 5/7→4/7、`明细口径` 1/1→0/1，`趋势` 3/3→2/3。

**M6 · AS03 的根因，以及口径层其实完整工作了（我先前没看全）。**
gold 按 `after_sales_time`（售后申请时点）过滤，模型按 `order_time`（原订单下单时点），
于是漏掉「去年下单、今年退货」那批（2779 vs 2990，−7.1%）。
声明**本来就是对的**（`meta.metric` 里 售后单数/退款额 都写着 `time_col=after_sales_time`），
而且整条链路跑通了：判出违规 → 交回炉（route 变 `llm+repair`）→
回炉后模型**原封不动仍用 `order_time`** → 预算用尽 → 照返 + `caliber_note`
「口径复核未通过…下方结果不可信，请勿直接用于决策」。
**系统没有静默给错数，它明确标注了自己不可信** —— 评测判它失败是对的，但这与「静默错答」是两回事。

真正的缺口是**判词不可执行**：只说「必须用 after_sales_time」，没说「你现在用的是 order_time」。
修法取更省的那一级（先不上 AST 重写）：判词改成「把 WHERE 里 order_time 的那几个条件整段
改成 after_sales_time，其余一个字不动」。`Facts::time_ish_conds` 只服务措辞、不参与判定，
故判宽判窄都不改变红/绿（有断言钉住两个方向）。**待下一轮评测量它是否够。**

**M7 · 口径层此前从日志上不可证伪，本轮补了留痕。**
`caliber_round` 只在判红时写日志，规则为空时静默走 `Pass` ——
「口径层在跑」与「口径层在跑但零条命中」长得一模一样。现在 `run_once` 无条件打印规则数
（0 条时也打，且带问句与召回到的表）。查 AS03 时正是靠这条才看清 4 条规则都在。

**M8 · 两个观测坑，记下来别再踩：**
- `Tee-Object` **到管道结束才落盘**（两次撞上）→ 长任务跑中间看不到进度，别据此判断卡死。
- **CLI 路径不写 `meta.query_log`**：`query_log::finish` 是 `tokio::spawn` 的，
  CLI 进程退出比它先到。我曾拿它当判官进度信号，那是错的。
  可靠信号是 `docker exec` 子进程的存活与启动时间。

## 二·N、口径层把一条**本来正确**的 SQL 改错了 —— AS03 的真根因

本轮最重要的一条，而且与我先前两次的判断都相反（先是「可能是噪声」，后是「判词不可执行」）。

**N1 · 现场。** `meta.correction_log` 里 AS03 的回炉判词写着：

> 指标「**订单数**」的时间语义钉在 **order_time** 列上 …… 现在约束的是 **after_sales_time**，
> 必须换成 **order_time**。把 WHERE 里 after_sales_time 的那几个条件整段改成 order_time

也就是说：**模型第一版老实用了正确的 `after_sales_time`**，是**判据命令它换成 `order_time`** 的。
它照做了 → 「售后单数」那条声明当场判红 → 预算用尽 → 照返错值（2779 vs 2990）。

**N2 · 成因是两条互相矛盾的声明同时生效。**
问句「今年退货类型的售后单有多少单」里的「单」把无关指标「订单数」也召回了，
于是同一轮里既有 `RequireTimeColumn{after_sales_time}`（售后单数）
又有 `RequireTimeColumn{order_time}`（订单数）。
而这条变体**刻意不带表名**（当初的理由是「声明只知道列名，不知道它在哪张表」，
为了跨表也能判）—— 正是这个刻意让两条无法互相区分、直接对撞。

**N3 · 修法：冲突即全部不判（`kernel::drop_conflicting_time_cols`）。**
同一轮出现两个及以上不同的声明时间列 → 那几条**全部哑掉**，其余规则照判。
**不「挑一个」**：挑需要「哪个指标更贴问句」的分数，构造侧今天没有那个分数，挑错就是重演 N1。
丢掉只回到「本轮不查时间列」（漏判方向），与 二·G 的宁缺毋滥同一条口径 ——
**判错一条会连带把对的答案回炉改错**，这次是实测到的、不是设想的。

**N4 · 实测：AS03 ✅**，且 route 从 `llm+repair` 变回**纯 `llm`** ——
第一版那条正确 SQL 不再被改。枪测过（把 `conflict` 写死 false → 当场红）。

**N5 · 这条改变了我对「口径层」的风险认识。**
此前我把它的失败模式想成「漏判」（判据看不见 → 答案照旧错）。
N1 证明它还有第二种、更贵的失败模式：**判据看见了，但看见的是矛盾的东西，于是把对的改错**。
凡是「判据可以命令模型改写 SQL」的机制，都必须先回答「两条判据互斥时怎么办」。
现在只处理了 `RequireTimeColumn` 这一种互斥（它是唯一不带表名、因而无法互相区分的变体）；
别的变体带表名，冲突时至少落在不同表上，不会对撞。

**N5a · 顺着 N2 做了一次同族审计：注册表召回缺 `ORDER BY` 而下游按顺序取第一个。**
（同一类此前已抓到过一次：`load_dimensions` 缺 `ORDER BY` → `find()` 按物理行序选 → E17 靠运气过。）

| 位置 | 下游依赖顺序的方式 | 实测严重度 | 处置 |
|---|---|---|---|
| `recall::metric::recall_metric_hits` | `correct_caliber` 逐个补口径，`add_scope_filter` 对已约束列不再补 → **先到者赢**（最典型：金额侧 `item_type='3'` vs 数量侧 `'1'`） | **高**：种子每次启动 UPDATE `meta.metric` → 物理序会变 → 同一份代码在不同部署上可能应用不同口径，且没有任何测试会红 | 加 `ORDER BY name` + `order_by_specificity`（命中词字数降序，同长按名字码点序）。判据选「命中词更长」与维度侧 `direct::pick` 同一条原则 |
| `recall::cards::recall_dimensions` | `map_filter` 的 R2 是「同名保留**首个**」，而 autodiscover 把同一列注释灌成多条同名维度（实测「所属公司编码」**11 条**） | **低**（我一开始高估了）：那 11 条 `count(DISTINCT expr)=1`，**分组表达式完全相同**，变的只有卡片里那句「来源 X」。不是错答案，但 prompt 字节不可复现 —— 而本仓纪律是「prompt 的字节就是行为」 | 加 `ORDER BY name, source_table` |
| `corrector.rs` 的 `correct_agg` 指标加载 | 同上（按顺序） | 中 | 加 `ORDER BY name` |
| `registry::caliber` 的 `value_map`（`length(code)>=3`） | `code_rules` 有「同一码跨两个 (表,列) → 整条跳过」的门禁，**与顺序无关** | 无 | 不动 |
| `registry::element::*` 四个加载器 | 全量遍历算 embedding，不取第一个 | 无 | 不动 |

**排序键为什么不是「名字字典序」而是「命中词更长」**：问「库存金额」时 `库存金额` 比别名 `库存`
更该说话。名字序只买到确定性，买不到正确性；两个一起要才对。
同长时才落到名字的**码点序**（不是拼音序 —— 我第一版断言按拼音直觉写，当场红：
`销售额` 与 `销量` 首字同为 `销`，第二字 `售`(U+552E) < `量`(U+91CF)，故 `销售额` 在前）。

**N6 · 升级路径**（未做，写清楚免得被当成已做）：让 `semantic::registry::caliber` 把
`recall_metric_hits` 的分数一路带下来，只保留最高分那个指标的时间列声明。
那时才谈得上「挑一个」。

## 二·O、GOODS13：确定性重写在这一形态上**做不到**，以及我给错过一次建议

**O1 · 先证伪了自己的第一个猜测。** GOODS13 在「加冲突守卫前」定向跑是 ✅、守卫后全量是 ❌，
我第一反应是守卫把它连带哑掉了。查了一句就否掉：问句
「2026年上半年每个月的销量是多少箱」**只命中一个指标**（销量，`time_col=order_time`），
没有第二个时间列 → **守卫对它是惰性的**。不是守卫的事。

**O2 · 它是一枚偏向错答的硬币。** 四次记录：round5 ✅ / sc=3 ❌ / 定向 ✅ / 本轮 ❌，
两次失败**都是同一个值 2138540.58**（+25.9%）。也就是说错的那条 SQL 是个**稳定的众数**，
对与错的分岔点在「回炉这一次改不改得对」——那是运气。
（这也解释了 二·M′ 的 sc=3 为什么反而让它变红：SC 向众数收敛，而它的众数是错的。）

**O3 · 错 SQL 的形态说明「确定性换时间列」在这里不可行。**
`failure_log` 里那条：
```sql
FROM ( SELECT DISTINCT sales_order_code, sku_code, sku_name, box_quantity, amount, delivery_time
       FROM t_sales_order_detail
       WHERE item_type='1' AND deleted_flag=0
         AND delivery_time >= '2026-01-01' AND delivery_time < '2026-07-01' ) t
GROUP BY DATE_FORMAT(t.delivery_time,'%Y-%m')
```
它用明细表**自己的** `delivery_time`，而声明的 `order_time` **在 `t_sales_order` 上**
（指标 source_table 就写着 `JOIN t_sales_order 有效订单`）。
就地改名 → 一个不存在的列（1054）。正解要**加 JOIN + 搬迁时间过滤 + 连带补订单头的口径过滤**，
那不是一次安全的局部 AST 编辑。所以我两次推迟的 `retarget_time_col`
**只对同表形态（AS03 那种）成立**，而同表形态已经被 二·N 的守卫解决了 —— 于是它没有剩余价值，判**不做**。

**O4 · 我给错过一次建议，已修。**
二·M6 那句「可执行判词」写的是「把 WHERE 里 X 的那几个条件**整段改成** Y」。
对 AS03（同表）对，对 GOODS13（跨表）**是错的建议** —— 照做就是造一个不存在的列。
判据在 kernel、**拿不到 schema**，分不清是哪一种（变体不带表名正是为了跨表能判）。
故判词改成两条分支：① 若目标列就在已连接的表上 → 整段改名；
② 若在另一张表上 → **必须 JOIN 那张表并连带补它的口径过滤**；
并明写「**不要就地改名**」。三条断言钉住（含「跨表那一支缺了」与「口径过滤」提醒）。
教训：**判词是给模型的指令，指令写宽了不是没用，是会把它带到另一个错上去。**

**O5 · GOODS13 这一类真正的杠杆是确定性装配，不是再调一次 prompt。**
回归 B07「销售额按月」/ B11「今年销售额按月份」都走 **direct-agg**（确定性装配、零 LLM），
而「销量按月」落到了 `llm+repair`。把它接上，GOODS13 就从「硬币」变成「确定的」。
**这是下一个该做的**，而不是第三次改判词。

**O5a · 但它不是「加个模板」——顺着查下去是三道拦，且第三道是安全守卫。**
先纠正我自己的第一判断（「差别在销量要去重子查询 + JOIN 订单头，compose 不接这形态」）：
**不对**。`compose_sql` 本来就接销量的去重形态（`compose_sql_with(&qty_metric(), &cat_dim(), …)`
等断言在跑），而且**「月份」维度早就声明了**
（`seed_defs.rs`：`("month","月份",&["按月","每月","各月","月度"],"t_sales_order o","DATE_FORMAT(o.order_time,'%Y-%m')")`）。
真正的拦路是：

1. **别名差一个词**：GOODS13 问的是「每**个**月」，而别名只有「每月」——
   `pick()` 不命中维度 → `try_compose` 直接 `None`。一个别名的事。
2. **`pick` 要指标与维度同时命中**：这条本来就满足（销量字面命中）。
3. 🔴 **残留守卫 `has_entity_residue`**：剥掉「指标名/别名 + 维度名/别名 + `STRIP_WORDS`」后
   若还剩 CJK 就回落 LLM。
   ⚠️ **本条原文有一处错，已订正**（见 二·T）：我写过「`STRIP_WORDS` 里没有阿拉伯年份 → 残留」，
   **那是错的** —— `has_residue_with` 本来就过滤掉所有 ASCII 数字，阿拉伯年份从来不是残留。
   真正会成为残留的是「上半年」剥完单字「年」后留下的**「上半」**，以及**单位词「箱」**。

第三道**不能简单放宽**：那条守卫是回归 E16 抓出来的实证防线
（「线下客户本月销售额」被装配成「全部客户 TOP200」，"线下"这个过滤被**静默丢弃**）。
往里加「2026」这类通用数字串，等于让实体编码也被剥掉 —— 那是把一条防线换成一个新缺陷。

**正确的修法是让守卫知道「时间表达式已被消化」**：装配器本来就会调 `time_predicate` 把
时间窗装进 SQL，也就是说时间词**确实被消化了**，只是 `has_entity_residue` 的消化词表不知道。
所以该做的是让 `time_predicate` 交出它匹配到的**原文跨度**，由消化词表带上它，
而不是往通用虚词表里塞年份。**未做** —— 它要改 `kernel::nl::time` 的返回形状（今天只回谓词），
且动的是安全守卫，不该赶在长会话末尾做。下一轮的第一件。

## 二·P、时间解析吞掉显式年份：把错窗口当权威口径交给 LLM

顺着 二·O5a 的第三道拦（残留守卫）查下去，发现拦得**对**，而真缺陷在别处。

**P1 · 缺陷。** `rule_half_year` / `rule_quarter` / `rule_month` 三条规则的年份全部写死
`CURDATE()`，**完全忽略问句里的显式年份**。于是「2025年上半年」解析出**今年**上半年。
而这个窗口不是内部中间量 —— 它以
「## 时间范围（已按问句规则解析，**直接照用**）」的身份写进 prompt，
也就是把一个错的口径当权威交给 LLM。`去年上半年` 同样被算成今年（同一缺陷的另一面）。

**P2 · 为什么活到今天：评测的年份覆盖坍缩成了「当年」。**
38 题里带显式年份的有 5 道（GOODS13-17），**全部是 `2026年` ＝当年**。
于是「拿 CURDATE() 的年份替换掉问句里的年份」与正确行为在这套题上**完全无法区分**。
这与本项目反复抓到的那个形状同类 —— **判据的输入让断言恒真** ——
只不过这次坍缩的不是某个参数，是**测试数据的年份维度**。
GOODS13 问的是「2026年上半年」，它一直红是别的原因（二·O2），
但即便它绿了也证明不了年份解析是对的。

**P3 · 修法：年份基准三态（`YearBase`）+ 只给年份的新规则。**
`Explicit(y)` 出**字面日期**（`'2025-01-01'`）、`LastYear` 出 `DATE_SUB(...,INTERVAL 1 YEAR)`、
`ThisYear` **输出字节逐字不变**（既有断言与 prompt golden 钉着它 —— 新增行为只在前两态）。
`rule_year`（「2025年的数」→ `YEAR({}) = 2025`）必须排在月/季度/半年**之后**，
否则「2025年6月」会被吞成整年。半开区间右端跨年由 `ym(base, 13)` 统一处理。

**P4 · 四位数字不跟 `年` 时不许当年份。** `1032`（公司编码）、`2024型号` 都不是时间。
判宽了就是把实体编码解析成时间窗 → 静默答错。有断言钉两个方向（含位数不对、范围外、「近3年」）。

**P5 · 门禁当场抓了我一次。** 测试串里写了「销售额/销量」，
「kernel 不得含 DMS 业务名词」判红 —— kernel 必须无 DMS 词汇，而规则本来只吃时间词、
不需要业务名词。改成中性串后绿。**这条门禁值回票价的一次。**

**P6 · 评测抓不到它，所以别指望评测分数变化。** 这次改动预期**不动评测分**
（那 5 道题的年份都等于当年）。它防的是「问历史年份」这类真实用法。
后续基线重置时应补一道过去年份的题；现在加会改动分母、干扰跨轮对比，故不加，写在这里备查。

## 二·Q、few-shot 语料在投毒，而且入口与出口都不设防

**Q1 · 出口：`fewshot` 的过滤是 `status != 'disabled'`，也就是 `pending` 也召回。**
而那个函数的文档写着「复核判错的(disabled)剔除，**只用高质量语料**」——
在这个过滤下那句话不成立：`pending` ＝**未复核**，53 条全在参与 few-shot。
复核是异步 LLM（`review_exemplar`）+ 一个 CLI（`review-pending`），可能很久不跑。
于是「待复核」在语义上等于「已采纳」。

**Q2 · 入口：确定性已判红的 SQL 照旧被沉淀。**
`execute` 原先只在 `rows.is_empty()` 时跳过沉淀。于是一条
「口径复核未通过（回炉后仍违反 2 条声明）、答案已挂『数字不可信』」的 SQL 照样进语料。
现场：我为验显式年份跑的「2025年上半年的销量」（同时违反去重键与时间列）落成 id=65。
**手里已有确定性判据，却把它交给一个概率性判据去兜** —— 那是把强证据换成弱证据。
已由 `dms_agent::worth_learning` 堵住，并加了第二条否决：
**单行全 NULL 也不学**（既非「有结果」也非 `rows.is_empty()`，从两条既有分支之间漏过去；
成因有二：空窗口上的聚合、敏感列整列置空）。

**Q3 · 存量：`audit-exemplars` 子命令，判据与运行时同一条。**
`registry::caliber::build_rules` + `kernel::check_caliber`，**不抄第二份**
（抄了会漂出「审计说干净、运行时判红」）。召回表名从语料 SQL 自己抽。
`--fix` 只置 `disabled`，**不删** —— 语料是证据，删了就查不回来为什么。

实测：**63 条里 6 条违反声明**，全部 `pending`（也就是全部在被召回）：
`退款占比`未×100、`t_customer_balance` 未约束 balance_status/type（两条）、
`t_sales_order_detail` 未约束 item_type/deleted_flag、该 JOIN 分类表却没 JOIN、
以及 **#54「2026年6月各分类销量Top5」缺去重键** —— 那正是 GOODS17 的原题，
按 trgm 相似度它会被**所有销量问句**召到，教出一个不去重的形状。
这与 二·O2 观察到的「GOODS13 的错答是个稳定众数」高度吻合：
**few-shot 就是众数的来源之一。** 已 `--fix`，复审 0 条。

**Q4 · 出口那条策略（`pending` 算不算高质量）留给下一轮量。**
把 `fewshot` 收成 `status = 'enabled'` 只剩 11 条，召回面骤降 ——
方向上更干净，但那是**行为变更**，要和评测一起量，不能顺手改。
今天先做证据充分的那一半（把确凿违规的 6 条置 disabled）。

## 二·R、把「下一步做什么」变成一组数：确定性覆盖 21%，而 100% 的失败在另外 79%

**R1 · route 分布（完整 38 题实测）**：`llm 24 / direct-agg 8 / llm+repair 5 / semantic-cache 1`。
**76% 过 LLM，而三次失败全部是 LLM 路径**（`llm` / `llm+repair` / `llm`）。
确定性路径至今 0 失败，回归 54/54 也稳。
所以质量的第一杠杆不是再调 prompt，是**把题从 LLM 路径搬到确定性路径**。

**R2 · 新增 `why-not-compose` 子命令**：报出 `try_compose` 五道门里**第一个**不成立的那道。
与 `try_compose` 共用同一批加载与判据（不抄第二份 —— 抄了会漂出「诊断说能装配、实际回落」）。
靠读代码猜不出来：五道门只回一个 `None`。

**R3 · 38 题按门分布**：

| 门 | 题数 | 读法 |
|---|---|---|
| ② 维度不命中 | **17** | 指标认出来了，但没有维度 |
| ① 指标不命中 | 9 | 问句里没有任何已声明指标 |
| ⑤ 残留守卫 | 6 | 两者都命中，但有装配器表达不了的限定 |
| ✅ 可装配 | 4 | （另外 4 道 direct-agg 来自硬编码模板，不走 compose） |
| ③ 快照门 | 2 | 来源表是快照表，一律不装配 |

**R4 · 逐条看那 17 道之后，路线是这样的（不是「补别名」那么简单）：**
- **真·只有指标、无维度、无值过滤 —— 4 道**（本月开票金额 / 本月售后单有多少 /
  今年售后单有多少 / 2026年6月动销商品有多少个）。今天**没有任何确定性路径能接**：
  `try_compose` **强制要求维度**，而硬编码的 `agg_template` 只认 4 个指标
  （销售额/订单数/客单价/成交客户数）。
  → **该做的是「指标 only」的通用装配**：`SELECT <agg_expr> FROM <source>
  WHERE <scope_filter> AND <time on time_col>`（声明齐了就能装，含去重子查询）。
  这是本轮识别出的**最大单项收益**，且完全建立在既有声明上。
- **有维度但维度未声明 —— 2 道**（费用项目 / 仓库）。声明即可，但要连库确认真实表与表达式；
  这两道今天靠 LLM 是**绿的**，声明写错会把绿的弄红，故必须先对数再声明。
- **看着是②、其实该被⑤拦 —— 2 道**（「本月**湖南省**的销售额」「**手抓饼**这个分类卖了多少箱」）。
  给「省份」补别名「省」、「商品分类」补别名「分类」只会把它们从②挪到⑤ ——
  **不是收益**：湖南 / 手抓饼 是值过滤，装配器本来就表达不了。**故不补这两个别名。**
- 其余是已被 `agg_template` 接住的，或 agg_expr 含子查询（`退款占比`）装配器本来就拒的。

**R5 · 量级估计**：指标 only（+4）与两个维度声明（+2）合计可把确定性覆盖从 8/38（21%）
推到 14/38（37%）。因为全部失败都在 LLM 路径，搬走的每一道都同时**移除一个飘的来源**。

**R6 · 飘的池子比先前记的大。** 已观测到在轮次间翻面的：
GOODS17 / MKT04 / E05 / GOODS13 / FIN04 / **AS01 / AS02** —— 至少 7/38 ≈ 18%。
AS01/AS02 这两次是在**同一份镜像、同一份语料**下一绿一红（两轮都在语料清理之后），
所以它们是飘、不是回退 —— **核对时序才没把结论下错**。

## 二·S、按 二·R 的数动手：可装配 4 → 7，三道题从 LLM 搬到确定性路径

**S1 · 指标 only：不写第二个装配器。** 造 `expr` 为空的伪维度喂 `compose_sql_with` 的
无维度模式 —— 去重子查询下推、表级口径、时间桥接、扇出检查、残留守卫**全部复用同一份**。
抄第二份必然漂出「两条路口径不一致」，而那种不一致是静默的。

两道自设门，都是防静默出错：
- **给 `agg_template` 让路**：Router 里 `direct-agg` 排在 `direct-doc` 之前，不让路就会
  ① 把「本月销售额」的数从订单头 `SUM(total_amount)` 换成明细声明那一套 ——
  差多少正是 `item_type` 那件**未裁决**的事（二·J′ 的 204.5M/208.1M/131.4M）；
  ② 丢掉 KPI 环比（指标 only 不出上期查询）。两条都不报错。
- **命中维度即退出**：用户要了分组却拿到单值是答非所问。

**S2 · 被自己的断言纠正一次。** 我原以为「本月有多少个订单」由 `agg_template` 服务，
实际**不是**：剥词表里有「订单数」没有「订单」，剩下「个订单」被残留守卫拦掉。
这不是缺陷 —— 是模板的固有窄面（它按**字面词表**工作，声明层按**名/别名**工作）。
指标 only 正好补这个面：同一个「订单数」声明，两种说法都能接。

**S3 · 时间窗按声明的 `time_col` 放（这才是「指标 only 也不接」的主因）。**
第一版指标 only 只多接了 1 道，远低于我估的 +4 —— 查下去发现
`compose_sql_with` 的时间窗**写死 `t_sales_order` / `order_time`**：
FROM 里找不到订单头就试着桥一条边，桥不到就**整条不装配**。
于是售后单数（`after_sales_time`）、开票金额、动销商品数一律放不下时间窗、一律回落 LLM，
**而 `meta.metric.time_col` 里明明写着该用哪一列** —— `MetricDef` 甚至没取那个字段。
又一处「声明在那儿、装配器不读」（同 二·N 的 `RequireTimeColumn`、二·R 的 `why-not-compose`）。

修法：声明的列不是 `order_time` 时，直接放在**指标基表**上；
是 `order_time`（或未声明）时**保持桥接老路** —— 明细类指标的 `order_time` 确实在订单头上，
那条 JOIN 不可省，漏了它连「有效订单」表级口径一起丢（数值虚增的头号来源）。
两个方向都有断言钉住。

**S4 · 实测**：可装配 4 → 5（指标 only）→ **7**（+声明时间列），② 17 → 14。
`E02-本月订单数` / `E09-售后单数` / `PERM01-城市经理今年售后单数` 三道
**全部转 direct-agg 且与 gold 逐值一致**，延迟 ~12-20s → ~9s。
PERM01 是权限题，说明行级注入在确定性路径上照旧生效。
确定性覆盖 8 → **11/38（29%）**。

## 二·T、枪测抓到我自己写的一条恒真断言，并推翻了 二·O5a 的一个判断

**T1 · 我做了什么。** 为了让「2026年6月动销商品有多少个」能走确定性装配，
我给残留守卫加了「消化 `time_predicate` 认下的显式年份」，理由写在 二·O5a：
「`STRIP_WORDS` 认不出阿拉伯年份 → 剥完仍有残留 → 回落 LLM」。

**T2 · 枪测证明那是死代码。** 把年份消化关掉（`if let Some(y) = None::<i32>`），
测试**仍然全绿** —— 也就是说我为它写的断言
（`!has_entity_residue("2026年上半年的销量", …)`）**在没有该功能时也成立**。

**T3 · 原因：`has_residue_with` 本来就过滤掉所有 ASCII 数字。**
```rust
.filter(|c| !c.is_ascii_digit() && !c.is_whitespace() && !"，。？?、,.~～!！:：".contains(*c))
```
**阿拉伯年份从来就不是残留。** 二·O5a 那句判断是错的，已在原处订正。

**T4 · 真正的拦路石是另外两个。**
- 「上半年」剥掉单字「年」后留下的 **「上半」**（CJK → 残留）。
  → `STRIP_WORDS` 补「上半年/下半年」，排在单字「年」**之前**（长词先剥，有断言守）。
  **这一条枪测通过**（拿掉即 2 红），是本轮唯一真的放宽。
- **单位词「箱」**（「…的销量是多少箱」）。`meta.metric.unit` 存在但销量那行是空的 ——
  要消化单位词得先把声明填上。**未做**，写清楚免得被当成已做。

**T5 · 处置：删死代码，不留。** 死代码比没有更坏 —— 它让读者以为这里有一层保护。
断言改成钉「真正会成为残留的东西」（单位词、值过滤）＋ E16 那条防线。

**T6 · 方法论。** 这是本项目第 N 次同一个形状：**判据的输入让断言恒真**。
前几次抓的是别人写的（drift-guard 搬家、`default_router()` 空表、kb_eval 401-as-skip、
`check-arch` 的 `cargo tree` 空转、`parse_ok` 的 `find_spec`）。
这次是**我自己刚写的**，而且是靠枪测在同一轮里抓到的 ——
**「改坏必须变红」这条纪律唯一的作用就是抓这个**，读代码是抓不到的。

## 二·U、评测在这个飘动率下**分辨不出 ±2**：该看的是结构指标，不是单轮分数

**U1 · 五轮实测的分数与失败集**：

| 轮次 | 通过 | 失败集 | direct-agg |
|---|---|---|---|
| 基线 | 34/38 | AS03 · GOODS17 · MKT04 · SALE15 | 8 |
| 冲突守卫 | 35/38 | FIN04 · GOODS13 · SALE15 | 8 |
| 语料清理+显式年份 | **36/38** | GOODS17 · SALE15 | 8 |
| 指标 only + 声明时间列 | 34/38 | FIN01 · GOODS17 · SALE15 · **STK01** | **12** |

**U2 · 已观测到翻面过的题：9/38 ≈ 24%。**
GOODS17 / MKT04 / E05 / GOODS13 / FIN04 / AS01 / AS02 / FIN01 / STK01。
全部在 LLM 路径上；确定性路径（direct-agg / graph / compound / semantic-cache）**至今 0 失败**。

**U3 · 结论（方法论，比任何一轮分数都重要）**：
23 道题仍在 LLM 路径上，飘动率约 1/4 —— **单轮 38 题的分数在 ±2~3 内无信息量**。
「指标 only 那轮 34/38 比上一轮 36/38 低」**不是回退**：
新红的 FIN01/STK01 都是「取了哪个名字列」的 LLM 抖动（`线下-` 前缀、空仓库名），
route 都是 `llm`，装配器根本没产它们的 SQL。

**所以此后的验收判据改成三条，而不是看总分**：
1. **确定性覆盖**（direct-agg 题数）—— 单调、可复现、与飘无关。8 → **12**（21% → 32%）。
2. **单题的路由 + 与 gold 逐值一致**（如 E02/E09/PERM01 三道转 direct-agg 且逐值一致）。
3. 想比总分就**同一镜像跑多轮**取失败集的交集；只有交集里的才算系统性。
   按这条，五轮交集只有 **SALE15**（业务裁决）。

**U4 · 这也解释了 二·M′（SC 判为不开）为什么当时看着「净 0」**：
在 1/4 的飘动率下，SC 的收益（把飘的压住）与损失（把错的众数钉死）**都被同一个噪声带盖住**。
真正的判据是它当时那条机制论证 —— 众数错时它把「偶尔对」变成「稳定错」，而那与分数无关。

## 二·V、把两道最可复现的失败搬上确定性路径（GOODS13 / GOODS17）

**V1 · 结果（按 二·U3 的判据：路由 + 逐值，不看单轮总分）**

| 题 | 之前 | 之后 |
|---|---|---|
| GOODS13-上半年月度销量趋势 | 偏错的硬币：4 次记录 2 绿 2 红，两次错值**都是** 2138540.58（+25.9%） | ✅ **direct-agg，6 行一致** |
| GOODS17-六月分类销量Top5 | 稳定 +30.5%（184616 vs 141502） | ✅ **direct-agg，5 行一致** |

可装配 4 → **10**（`✅10 / ①指标不命中 9 / ②装配器拒 9 / ⓿让路 4 / ⑤残留 4 / ③快照 2`）。

GOODS13 装配出的 SQL 与 gold 数值一致，且要素齐全：5 键去重子查询（指标 + 表级口径已下推）、
JOIN 订单头带「有效订单」口径、**显式年份的字面日期 `'2026-01-01'`**（二·P 的年份修复也在里面）。
装配前先把语义与 gold 对过：gold 把 `DATE_FORMAT(o.order_time,…)` 放进 DISTINCT、
装配器是「5 键去重 + 外层连订单头分组」，两者等价 ——
月份由 `sales_order_code` 函数决定（一单一个下单时间），加进去不改变去重粒度；
时间过滤在子查询内外亦等价（订单整单在或不在）。
**把一道题变成确定性之前必须先对语义，否则是把一个错答案钉死。**

**V2 · 一处顺序纪律（本节最要紧的一条）**
解锁 GOODS17 只需把量词「个」加进 `STRIP_WORDS` —— 但**先不能加**：
`detect_top_n` 认「前5」不认「最高的**5个**」，光解锁会让它按默认 200 行出数、行数不符，
**把「飘着的失败」换成「确定的失败」**。
正确顺序：先补 `detect_top_n` 的「最高/最多/最大/最少/最小 + N + 个/名/条/项」那一支，
**再**解锁量词。判据刻意窄 —— **不认光秃秃的「5个」**：那可能是「5个仓库的库存」这类
**值过滤**里的数量词，按它截断就是悄悄改语义（有断言钉住）。

推广：**解锁一道题进确定性路径，前提是 TopN 与排序也都认得出来。**

**V3 · 虚词表每次只加实测挡住过的那一个词，不预先铺表**
本轮加的：`上半年/下半年`（剥掉单字「年」会留「上半」）、`箱`（单位量词）、
`是哪些/哪些/哪个/最高/最多/最少/最大/最小`（排序词与疑问词，与既有「排行/排名」同类）、
`个/名/条/项`（纯量词）。
**刻意不加**：`元`（吃掉「元气森林」）、`件`（吃掉「件套」）、`装`（吃掉「10片装」）。
每一处都有断言，两处做了枪测（拿掉「上半年/下半年」→2 红；拿掉「箱」→2 红）。
E16 那条防线（「**线下**客户本月销售额」被静默丢掉过滤）在每次放宽后都重新钉一遍。

## 二·W、确定性覆盖的下一段**不是代码活**，是「连库对数后新增声明」

`why-not-compose` 逐题跑完 38 题后剩下的最大一档是 **① 指标不命中 9 道**。逐条看完，
**里面没有便宜的**：

| 问句 | 真正缺什么 | 为什么不能顺手加 |
|---|---|---|
| 本月成交客户数 | —— | 其实由 `agg_template` 接着；诊断报 ① 只因它不是**声明的**指标 |
| 2026年6月赠品有多少箱 / 大型活动办了多少场 / 一共有多少家客户 / 售后完成率 | 各缺一个指标声明 | 要连库确认真实表与过滤；**这些题今天靠 LLM 是绿的**，声明写错会把绿弄红 |
| 临促人员费用 / **执行人员费用（MKT04）** / 待确认对账单 / 大型活动 | 问句里的「临促/执行/待确认/大型」是**值过滤** | 为每种值过滤各声明一个指标是**过拟合**，且残留守卫本来就该拦它们 |
| **本月卖得最好的10个商品（SALE15）** | 指标别名（「卖得最好」不在销量别名里）**＋** 商品维度 | 后者正是 二·L 那件**业务裁决**，未定之前补别名只会把它从①挪到② |

**结论**：代码侧的杠杆本轮基本用尽（可装配 4 → 10）。再往上走需要
**逐个指标连库对数 → 写声明 → 用「路由 + 逐值一致」验一遍**，
而每一条都有「把现在绿的弄红」的风险 —— 那是要与 DMS 团队一起做的活，不该我单方面写进种子。

**唯一例外是「成交客户数」**：它已经被 `agg_template` 正确服务，补一条声明只是把它从
硬编码模板迁到声明层（数由模板保证、可对拍），风险最低。留作下一轮第一件。

## 二·X、快照类问句被**永久钉在 LLM 路径上**，而声明里已经有装配它所需的一切

**X1 · 先记我在 FIN04 上折返了两次（判断过程本身是这条的价值）。**
① 第一判断「FIN04 是飘」→ ② 看了 gold 有 `ROW_NUMBER() … rn = 1` 后改判「系统性：判据没触发」
→ ③ 抓日志发现 **`RequileLatest` 确实构造出来了**（`rules=4`，第一条就是它，
`partition:[customer_code, balance_type]`，human 文本里连「21 行 vs 正确 23 行」的历史都记着），
且那次单跑返回 **23 行 = 正确**。
**结论回到 ①：它是偏错的硬币**（3 次 2 红 1 绿）。
中间那句「判据没触发」是错的 —— 判据触发了，只是模型那次自己写对了。
（我在 ② 里还算错过一次：数「所有行的 DISTINCT 客户」得 28、以为等于声明口径，
而 gold 数的是「每客户**最新一行**」——那两个数根本不是一回事。）

**X1a · ⚠️ 再订正一次（第三次）：下面 X2/X3 那套「快照装配器能修好 FIN04」的推论**
**对 FIN04 不成立。** 用数据一查就清楚：
`rn = 1` 全部 = **28**、`rn = 1 且 balance > 0` = **23**（＝gold 的答案）。
也就是说那 5 行差**完全来自 `balance > 0`，与 `rn = 1` 无关** ——
本表每客户每类型本就一行，`rn=1` 对行数是**恒等**的。
FIN04 的真缺口是「**还**有信控余额」隐含 `> 0`，那是**问句语义**，
不是表级声明能承载的（指标是求和，不是过滤）。
**处置**：走本系统已有的教训机制 —— `meta.pitfall` 加一条
「问『还有/还剩多少余额』必须加 `balance > 0`」，带实测数字（28 vs 23）。
教训会注入 prompt，且按表名触发，正好覆盖余额类问句。

X2/X3 作为**一般性判断仍然成立**（快照门确实把余额/库存类题留在 LLM 路径，
而声明里确有装配所需的分区键与排序列），只是**它不是 FIN04 的解**。
另外核实：`meta.table_snapshot` 今天只有 **1 行**（`t_customer_balance`，
排序 `created_time DESC, id DESC` 与 gold 逐字一致）；
两道库存题被拒的原因是 `scope_filter` 含子查询，**不是**快照门 —— 我先前把它们算进快照门也是错的。

**X2 · 折返查出的真发现：`compose_gated` 的快照门把这类题永久留在 LLM 路径上。**
`t_customer_balance` / `t_winc_stock_report` 登记在 `meta.table_snapshot`，
装配器**一律不接**（理由正当：平铺 GROUP BY 不懂「取每分区最新一条」，一装配就把历史行全求和）。
于是余额/库存类问句只能走 LLM，而 LLM 把 `rn = 1` 写对的概率实测约 1/3
（FIN04 三轮 2 红；`账户余额最高的10个客户` 的历史坑同源）。

**X3 · 但声明里已经有装配它所需的一切。**
`meta.table_snapshot` 声明了**分区键**与**取最新的排序列**（`RequireLatest` 的判词就是照它渲染的）。
也就是说装配器完全可以把基表换成
```sql
(SELECT … FROM 快照表 WHERE 口径 QUALIFY/子查询 ROW_NUMBER() OVER (PARTITION BY 分区键 ORDER BY 排序列 DESC) = 1) b0
```
—— 与去重子查询（`dedup_keys`）是**同一个形状**，只是把 `DISTINCT 键` 换成 `rn = 1`。
现有的表级口径下推、时间桥接、残留守卫全部可复用。

**X4 · 这是本会话第 N 次同一个模式，且是最后剩下的那一处大的**：
声明在那儿（`table_snapshot` 的分区键与排序列），**装配器不读它**，
于是判据只能在事后判红、把修对的希望交给 LLM 的一次回炉。
前面几处已修：`RequireTimeColumn` 的矛盾（二·N）、`MetricDef.time_col`（二·S3）、
`why-not-compose` 的诊断盲区（二·R）。**快照这一处未修**，是下一轮最大的单项收益：
它能把余额/库存两族（评测里 ③ 快照门 2 道 + FIN04 这类）从 LLM 搬走。

⚠️ 动它之前必须先对语义（同 二·V1）：gold 的 `rn = 1` 用的排序列是
`created_time DESC, id DESC`，而 `meta.table_snapshot` 里声明的排序列必须与之一致，
否则装配出来的「最新一条」不是同一条。

## 二·Y、知识库侧这一轮：补回欠的验证，并抓出「加了写路径没加删路径」

**Y1 · 先补欠的验证。** 我改了知识库侧四处（`_sheet` 上报空 sheet、`_have` 改真 import、
PDF 三级降级、`tabular::sheet_blocks` 跳过空表）**却没重跑 kb_eval**。
补跑：**7/7 全绿**，没被破坏。

**Y2 · 补 xlsx 的端到端判据（此前只验到 `/parse` 一层）。**
`openpyxl` 这条解析器本轮才启用，而 kb_eval 的 4 份语料里表格只有 CSV（走 `_p_csv`，不经 openpyxl）
—— 也就是「xlsx → sheets → 表格 markdown 块 → 检索」整条**没有判据**。
新增语料 `差旅补贴标准_表格.xlsx` + `KB08-recall-xlsx表格`：**kb_eval 8/8**。
夹具里那个数字（1250）刻意别处都没有 —— 否则题就变成「检索到任意一份」而不是「检索到这一份」。
夹具第二个 sheet 刻意留空，于是 **「embedded 1 块」这个数字本身就是判据**：
2 个 sheet 其中 1 个空 → 只出 1 块 → 「空 sheet 不产只有标题的垃圾块」端到端成立
（此前只有单测）。

**Y3 · 🔴 抓出自己的缺陷：加了写路径，没加删路径。**
我给上传通道加了 `sync_upload_schema`（把上传表结构写进 `meta.table_doc`/`column_doc`），
**删文档那侧没有对应清理** —— 它 DROP `up_<doc_id>` schema、注销 `meta.datasource`、删 `kb.acl`，
但注册表文档留着。实测：`column_doc` 有 6 个 upload ds，`meta.datasource` 只剩 2 个 —— **4 组孤儿**。
不是正确性 bug（孤儿 ds 不可见、召回取不到），但**写路径与删路径必须成对**。
修：`schema_sync::drop_schema_docs(pg, ds)` + 删除流程里调它。
存量 16 行 `column_doc` + 5 行 `table_doc` 已清。
（刻意不写成 `prune_stale_docs(pg, ds, &[])` —— 虽然等价，但读起来像 bug。）

**Y4 · 顺带露出的第二处：修复之前上传的文件「数据源在、schema 空、永不自愈」。**
`sync_upload_schema` 只在上传那一刻跑，而 `ingest` 按 sha256 去重 ——
重新上传同一份文件只命中旧 doc、**不再走通道②**。于是升级前的上传件问数必然答不出。
这在真实部署里就是「升级后老数据半可用」，而且不报错。
修：`resync-uploads` 子命令（幂等，`sync_schema` 本身是 upsert + 清理陈旧行）。
实测补采 2/2；那个此前 schema 空的 CSV 源现在有 4 列文档。

**Y5 · 上传源问数在 xlsx 上也实测通了。**
`SELECT c0 AS "出差类型" FROM t0_______ ORDER BY c1 DESC LIMIT 1` → `境外出差`（1250 最高，正确）。
表名 `t0_______` 是「中文 sheet 名整段退化成下划线」的设计行为
（一文件内靠序号前缀区分、跨文件在不同 schema，不会撞）。
另记一条分诊行为：「境外出差每天补贴多少」被分诊成 **knowledge**（文档类）而忽略 `ds` ——
那是分诊在正常工作，测问数通道要用 `intent="data"` 强制。

## 二·Z、快照装配器：代码已落地并单测，但**生产声明还对不上**（如实记，别当它通了）

**Z1 · 改了什么。** `compose_gated` 从「见 `meta.table_snapshot` 就拒」改成「按声明装配」：
把基表换成
```sql
(SELECT * FROM (SELECT *, ROW_NUMBER() OVER (PARTITION BY 分区键 ORDER BY 排序) rn
                FROM 表 WHERE 口径) rk WHERE rk.rn = 1) 别名
```
与 `dedup_keys` 那层**同一个形状**，只是 `DISTINCT 键` → `rn = 1`。
分区键/排序/额外过滤三样全部来自声明，装配器不自己猜「哪条算最新」。
口径**下推进最内层**（窗口必须在已过滤集合上算，否则 rn=1 可能取到一条被口径排除的行）。
仍然拒两种：声明缺分区键或排序、与 `dedup_keys` 并存（两层怎么叠未定义）。

**Z2 · 顺带修了一处**：条件去重原来是**整串比较**，抓不到「`balance_status='4'` 一次独立出现、
一次嵌在指标口径的 AND 链里」→ 同一条件拼两遍。改用既有的 `split_top_and` 按**原子条件**去重。

**Z3 · 改行为就改钉它的断言。** 旧测试 `snapshot_source_metric_never_composed` 断言的是
「一律不装配」，改行为后它红了 —— **这正是该有的信号**。改写成
`snapshot_source_metric_composed_per_declaration`，钉新行为（分区键/排序/rn=1/口径下推/
不重复拼）+ 两条仍然拒的。留着旧断言让它红、或悄悄删掉，都是在掩盖「行为变了」。

**Z4 · 诊断口又漂了一次，而且是我自己写的。**
`why_not_compose` 里我复制过一份「见快照就拒」，于是 `compose_gated` 改了之后它继续报
**一个已经不存在的行为**（「一律不装配」）。改成调真正的判定（`compose_gated`，且要带 `snaps`）。
**这已经是同一条教训的第三次**（前两次：`②` 文案说「补别名即可」而指标 only 已能接、
`RequireTimeColumn` 判词漏跨表那支）—— 诊断复制判据必漂，无一例外。

**Z5 · 🔴 但它在生产声明上走不到，我没能端到端演示。**
`why-not-compose "各客户账户余额"` 报 **④ 装配器拒绝**：
生产里「客户」维度的来源表是 `t_sales_order o`（`COALESCE(o.customer_name,…)`），
而指标来源是 `t_customer_balance` → 跨基表要找 join 路径，
而**余额表到订单头没有声明边** → 拒。
单测能过是因为我用了一个来源就在余额表上的**手造维度**。
**也就是说：代码路径有单测保证，生产路径没有实证。** 不当它通了。

**这里不能顺手加边**：`t_customer_balance → t_sales_order` 按 `customer_code` 连会**大幅扇出**
（一个客户多张订单，余额会被订单数乘一遍）。正确做法是
① 加一个以客户主档 `t_customer` 为根的维度、② 加 `t_customer_balance ↔ t_customer` 的边
（N:1，无扇出）。那是需要斟酌的声明设计（还要考虑它与既有「客户」维度在 `pick` 里的竞争），
不该单方面塞进种子 —— 与 二·W 那条同一性质：**下一段是连库对数后写声明，不是改代码。**

## 二·AA、补「成交客户数」指标声明，顺手造出并抓到一个静默回归

**AA1 · 为什么挑它**（二·W 里点名的唯一低风险项）：它此前只在 `meta.term` 里
（术语只解释、不产 SQL），于是**只有无维度那一支**被 `agg_template` 的硬编码分支服务，
`成交客户数 × 维度`（「各省成交客户数」）压根进不了装配器。
补成指标是加法式的 —— 数由模板保证、可对拍。

**AA2 · 🔴 但它当场造出一个静默回归，而我是靠诊断口发现的。**
补完声明后，「本月成交客户数」被 `try_compose` 装配成**按客户分组的客户数** ——
**200 行、每行 1**。成因：`pick(dims)` 被「成交客户**数**」里的「客户」命中维度「客户」，
而残留守卫剥完「指标名 + 维度名」后正好为空 → 一路绿灯。
**route 仍是 `direct-agg`** —— 回归 A09/A12 只断言路由，**看不出来**。
发现过程：`why-not-compose "本月成交客户数"` 报 ✅ 而它本该被让路门挡回 —— 那一行 ✅ 就是告警。

**AA3 · 根因是门装错了层**：让路门只在 `try_compose_metric_only` 里，
而 `try_compose`（带维度那条）排在它**前面**。门挪到 `compose_hit`，管住两条路。
判据为什么成立：`agg_template` 自己有 DIM_WORDS 门（含「各/按/排行/分类/省…」即拒），
**它接得住的问句必然没有维度词** → 此时任何维度命中都是伪命中，让路一定对。

**AA4 · 第一版我把门写成了 `try_direct`，被自己的测试③抓到。**
`try_direct` 还包含 `sales_breakdown`（销售额×维度的硬编码模板），拿它当门会让
**所有**销售额×维度问句让路给硬编码模板 —— 而那些今天走注册表装配、SQL 是另一套。
一次窄修差点变成一次宽的行为变更。收窄成 `agg_template`。

**AA5 · 断言的写法**：这条测试的价值在第①句 ——
`compose_gated(...).expect("前提：不让路的话这句会被装配成…")`
**先证明没有门时它真的会装配**，再断言门的判据。
只断言「门在」不证明门是承重的；本轮 二·T 那次恒真断言就是这么来的。

**AA6 · 实测**：无维度 → `direct-agg` **1 行 1612**（模板，数与环比不变）；
带维度 → `direct-agg` **33 行**省份明细（新能力）。
交叉验证：**各省去重客户数之和 1612 ＝ 总去重客户数 1612**
（客户的省份取自主档、一客户一省，故必须恰好相等）。

## 二·AB、量「还剩多少不通用」：硬编码模板已不再唯一服务任何一道题

**AB1 · 为什么量这个。** 「通用 agent 工具」与「DMS 专用工具」的差别，可操作的度量就是
**还有多少确定性覆盖靠 `direct.rs` 里的 DMS 专用写死逻辑撑着**
（`agg_template` 无维度四指标 / `sales_breakdown` 销售额×六维度 / 单号直查）。
声明层每接走一道，这个数就该少一道 —— 而 `direct.rs` 的解体（T8）本来就该以此验收。
此前只能靠感觉说「越来越声明化了」。诊断口因此加了一维 `⚙ 硬编码兜底`。

**AB2 · 结果（38 题）**：`✅ 声明可装配 10 / ⓿ 让路 4 / ② 装配器拒 9 / ① 指标不命中 9 /
⑤ 残留 4 / ③ 快照 2`。

**AB3 · 我第一版把结论读错了，订正。**
我把「非 ✅ 且有硬编码兜底」的 4 道算成「硬编码专属」——
但它们全是 `⓿ 让路`，而让路是**装配器刻意让开**，不是装配器不能。
（诊断在让路那一支直接返回，压根没试装配，所以那一行不含「能否装配」的信息。）
逐条看这 4 道就清楚了：`本月销售额是多少`×2、`本月客单价`、`上月销售额`
—— 正是 `agg_template` 的无维度四指标。

**正确结论**：**硬编码模板在这套题上不再唯一服务任何一道题**，它赢只因为那道刻意的让路门。

**AB4 · 解除让路需要两件，都不是代码难点：**
① ~~**`item_type` 业务裁决**~~ —— **已裁决：取 '3'**（二·AP）。这条不再是阻塞项。
（历史记录：实测三个数 204.5M / 208.1M / 131.4M，二·J′；二·AP 用订单头做第三标尺重测确认。）
② **装配器支持 KPI 环比**（出 `prev` 查询）—— 指标 only 今天 `prev: None`，
让路门的第二条理由就是它（换过去会静默丢掉环比）。
两件齐了才谈得上删 `agg_template`，那时 T8 的这一半才算真做完。

## 二·AC、装配器出 KPI 环比 —— 让路门的第二条理由消掉了

**AC1 · 做法与 `agg_template` 同形**：同一段装配、只把时间窗换成平移后的上期。
`compose_sql_with_snap` 加一个 `time_tpl: Option<&str>` 覆盖位，
`prev_window(question)` 给出上期模板与标签（「较上月」）。

**AC2 · 只在无维度那一支出 `prev`。**
`hits::patch_prev` 取结果**首格**算 Δ%，而带维度时首格是维度值（字符串）→
`cell_num` 返 `None` → 环比本来就用不上，多发一次上期取数是白花。
`agg_template` 也只在无维度时出 prev —— 两处口径一致。

**AC3 · 判据钉的是「只差时间窗」**，不是「有 prev」。
若上期那次重装配顺手换掉了别的东西（口径、去重、JOIN），
Δ% 就成了拿两个口径不同的数相除 —— **那种错比没有环比更坏，因为它看着像个结论**。
断言把两条 SQL 的时间谓词段抹掉后要求逐字相同，并钉方向（当期含本月起点、上期含 `INTERVAL 1 MONTH`）。

**AC4 · 实测**：「本月有多少个订单」→ `direct-agg`、`20093`、
`delta {pct: -15.9, dir: "down", label: "较上月"}`。
（选它而不是「本月开票金额」：后者来源含 `UNION ALL`，装配器按设计拒、走 LLM；
选它也不是「本月订单数」：那句被 `agg_template` 接走、验不到装配器这条路。）

**AC5 · 前两条理由消了，但让路门**撤不掉** —— 还有第三、第四条（二·AR 实测）。**
① ~~指标 only 不出环比~~ —— 本节消掉；
② ~~销售额的 `item_type` 取 '1' 还是 '3'~~ —— **业主已裁决取 '3'**（二·AP）；
③④ 见 **二·AR**：撤门实测当场两处坏（伪维度命中 + 客单价丢 ROUND）。
⚠️ 我曾在本节写过「两条理由都没了 ⇒ 可以撤门、可以删 `agg_template`」—— **那句是错的**，
已订正。教训与本仓反复出现的同一族一致：**注释里记着的理由不等于全部理由**，
撤一道门之前要把门**真的撤掉跑一次**，而不是数一数注释里列了几条。

## 二·AD、值过滤支持：声明能解释的值装进 WHERE，解释不了/装不上一律整条拒

「残留 → LLM」里最大的一类是**值过滤**（湖南省 / 线下客户 / 手抓饼）。`meta.value_map`
里本来就写着 `名字 → (表, 列, 码)`，装配器**不读它** —— 这是本轮反复遇到的同一个模式
（声明在那儿，装配器不读；前几处是 `time_col`、`table_snapshot`、`dedup_keys`）。

**AD1 · 先量风险，再动手。** `meta.value_map` 实测 **936 行 / 82 列**，
其中 **109 个名字跨 ≥2 个 (表, 列)**（公司名在十几张表上各有一份 `company_code`）。
所以第一条规则是**歧义即不认**（先例：`code_rules` 对跨两列的码就是跳过）。
不认 = 那个词照旧是残留 = 整条回落 LLM，与上线前**完全同形** —— 这是本特性的安全底座。

**AD2 · 两道 fail-closed 门，都做了枪测（拆掉即红）。**
- **G1 消化了就必须装上**：值名被残留守卫消化后，过滤装不上就 `return None`。
  装不上的两种：① 表不在 FROM 且按 `join_edge` 也桥不到；② 基表被去重/快照派生表包住
  （派生表只 SELECT 去重键，外层引用不到那一列）。
  **消化了词却不加过滤 = E16「线下客户 → 全部客户 TOP200」那类静默丢限定**，宁可回落。
- **G2 口径已钉住该列就拒**：销量口径写死 `item_type = '1'`，问句说「赠品」
  （声明 `item_type = '2'`）时叠上去是**恒 0 行**。确定性路径静默返回「0」
  比回落 LLM 坏得多 —— 口径与问句冲突该由人看，不是装配器调和。
  实测立刻兑现：「今年**退货**类型的售后单…一共申请**退款**多少钱」两个词都唯一命中
  `after_sales_type`（1 / 2），G2 当场拒掉，拒得对。

**AD2′ · 硬编码模板那一支（`⓿ 让路`）不会因此漏过值过滤。**
`agg_template` **不认识** `value_map`，所以要问的是「它会不会抢走一道带值过滤的问句、
并把那个限定静默丢掉」。答案是不会，且理由是结构性的：它自己那道剥词守卫剥完
若还剩任何 alphanumeric 就返 `None`，而值名是 CJK 实义词、**没有一个**在它的剥词表里
（表里只有时间词、指标词和语气词）。逐个核过本轮扫出的 8 个无歧义值名，全部如此。
另有两道各自独立的保险：`DIM_WORDS` 含「省」（挡住「湖南**省**」），
「线下」不在剥词表（挡住 E16）。

**AD3 · 值过滤的表按 `join_edge` 桥进来，且必须桥在这个位置。**
桥的位置放在「FROM 拼好、去重/快照包裹**之前**」，于是三层既有守卫自动覆盖新表：
① 去重装配的 `base_col_refs(&from, …)` 看得见新 JOIN 引用的基表列，不在去重键里就整条拒；
② 表级标准口径那个循环靠 `from_table_aliases` 扫 FROM，新表的恒需过滤跟着加上
（实测「本月湖南省的销售额」自动带上了 `vf0_0.deleted_flag = 0`，与 gold 的
`c.deleted_flag = 0` 一致）；③ `from.starts_with(&head)` 只看首段，尾部追加不受影响。
**改到下面再桥，这三层就全绕过去了。** 扇出边一律拒（`SUM` 沿 1:N 会虚增，实测 41%）。

**AD4 · 位置性同位语，不进 `STRIP_WORDS`。**
声明名是「湖南」，问句写「湖南**省**」→ 剩一个「省」就被残留守卫拦下。
但「省」**不许**进全局虚词表：那张表是无位置的、全仓共用的，全局剥「省」会吃掉实体名里的字
（`lexicon.rs` 那条「只加实测挡住过的、且无实体名风险的词」纪律说的就是这个）。
改成**只在紧跟一条已被唯一解释的值名之后**吃 `省/市/区/县` —— 位置性 = 不可能放宽全局守卫。
断言同时钉住「`省` 不在 `STRIP_WORDS` 里」，防下一个人图省事把它塞进去。

**AD5 · 拿全部 92 道题面对 936 行做全量对撞，扫出两个危险命中 —— 都是无歧义命中，歧义门救不了。**
- 「本月各**业务**员的销售额」：`业务` 唯一命中 `t_customer_contacts_account.contact_type = 1`，
  而它是维度名「业务员」的**子串**。认下来 = 给一道现在全绿的题桥一张联系人表 + 加一条无关过滤。
- 「今年**市场费用**…」：`市场费用` 既是**指标名**、又是 `t_customer_balance.balance_type = 3`
  的码值名。**相等**也必须让给指标（否则往余额表上加过滤）。
判据因此是「候选值名被任一已消化的指标/维度词**包含（含相等）**且该词在问句里 → 不认」，
与残留剥离那边「长词先于子串」同一条原则，只是长词来自注册表。
`registry_words(m, d)` 是值过滤与残留守卫**共用的**那一份消化词 —— 各写一份就会漂。

**AD6 · 实测（route + 值双验，不看单轮总分）。**
- **SALE17「本月湖南省的销售额是多少」**：`llm` → `direct-agg`、908ms、
  `vf0_0.province = '430000'`，值 `50621538.2000`；**同一时刻现跑 gold 得到逐字节相同的值**。
  （gold 备注里记的 46,520,283.70 是**月初**的快照 —— 「本月累计」每天在长，
  不是差异。这类比对必须现跑 gold，不能拿备注里的数当基准。）
- **E16「线下客户本月销售额」**：`llm` → `direct-agg`、395ms、`vf0_0.customer_class = '04'`；
  返回的 200 行客户名**全部**以「线下-」开头 —— 独立于 SQL 的正确性证据。
- 确定性覆盖 **15 → 16/38**（`✅ 12 + ⓿ 4`）。
- 未解锁的两道及原因：「手抓饼这个分类」还差「这个分类」这段残留与
  「卖了多少」这个指标别名（`分类` 是真维度词，位置性同位语表里刻意不放它）；
  「退货类型…申请退款」是 G2 拒得对。

## 二·AE、注册表读失败曾是**静默**的 —— 一趟评测把它照出来了

**AE1 · 现场。** 一趟 38 题评测里 E05「本月各商品分类销量」记的是 `llm+repair 97.9s` 并答错，
而**同一个镜像、同一句问句**事后连跑 5 次都稳定 `direct-agg` 且对数。
也就是说那一刻 `try_compose` 返了 `None` —— **而当时没有任何一行日志说为什么**。
`PROGRESS.md:732` 显示 E05 历来就在抖动池里，此前只当「LLM 抖动」记账，没人问过
「它为什么会掉到 LLM 路上」。

**AE2 · 顺着看下去，更坏的一条在旁边。**
`try_compose` 里那几个 `unwrap_or_default()`：
`load_table_scopes` 读失败 = 装配器**不带表级口径**继续往下拼 →
出一个确定性的错数、route 仍是 `direct-agg`、确定性路径不跑口径校验、**连回炉都没有**。
那正是「明细表漏 `deleted_flag = 0` 致销量虚高 41%」的失败面 ——
只不过这次的触发条件不是声明写错，而是**一次读超时**。

**AE3 · 判据：缺了会改数的声明，读失败就整条不装配，并且吼出来。**
`meta.metric / dimension / join_edge / table_scope / table_snapshot` → `reg_load!` 宏，
`Err` 即 `warn!` + `return None`。
`meta.value_map` 是**唯一**可以按缺省走的一个：空表 = 没有值名被消化 =
带值过滤的问句照旧被残留守卫拦下 → 只少一点确定性覆盖，不会出错数（仍然 warn）。

**AE4 · 三处必须同处置，否则「诊断与判定漂」。**
① `try_compose`；② `metric_only`（新增 `MetricOnly::RegistryDown(&'static str)` ——
**与「指标不命中」分开**：前者是声明没写、后者是读不到，合成一句会让下一个人
照着报告去补一个已经存在的声明）；③ `compose_verdict`（诊断口原来对 edges/scopes/snaps
仍是 `unwrap_or_default()`，于是读不到时它报「✅ 可装配」而运行时回落 —— 正是本文件
反复警告的那种漂）。诊断新增 `⑥ 注册表读失败`。

**AE5 · 这条改的是可观测性，不是数。**
它不会让任何一题从红转绿；它让「一道稳定 direct-agg 的题突然变成 llm+repair」
从**查不出**变成**日志里写着**。上一次同类教训是 `meta.query_log` 不被 CLI 写入
（`tokio::spawn` 输给进程退出），当时我拿它当过进度信号。

## 二·AF、补「赠品箱数」指标：与销量只差一个码值，红转绿且确定性

**AF1 · 现场。** GOODS14「2026年6月我们送出去的赠品有多少箱」诊断是 **① 指标不命中** ——
连残留守卫都轮不到，整题只能交 LLM，实测答 **75,840** 而 gold 是 **127,211**（差 40%）。
而它的 gold 与「销量」的声明**结构逐字相同**：同一张明细表、同一套去重键、同一个
`SUM(box_quantity)`、同一条 `JOIN t_sales_order 有效订单`，**只差 `item_type` 的码值**（'2' vs '1'）。

**AF2 · 别名里必须有裸「赠品」，两个理由。**
① 问句就写「送出去的赠品有多少箱」，不认它就命不中指标；
② `meta.value_map` 里「赠品」正是 `item_type='2'` 的码值名（跨 detail/cart/his 三张表，
当前被值过滤的**歧义门**跳过）。哪天有人把那几行去重了，「赠品」就会被当成值过滤，
与本指标自己的 `item_type = '2'` 撞上 **G2**（口径已钉住该列）→ 整条拒。
把「赠品」放进别名，**子串门**（值名被已消化的指标词包含即不认）会先把它挡回来。
—— 这是二·AD 那几道门与新声明之间的**互锁**，写在这里免得下一个人各修一边。

**AF3 · 抢词是这条声明唯一的真风险**，因为它与销量太近。
`销量` 的别名里有「卖了多少箱」，新指标的别名里有「赠品箱」。谁赢由行序决定的话，
「本月销量」有可能被算成赠品 —— **数会变小、而 route 仍是 `direct-agg`，没有回炉机会**。
断言 `gift_box_qty_does_not_steal_the_sales_qty_questions`：交集为空 +
GOODS14 原句只命中新指标 + 销量那一族三种问法（「本月销量」/「上半年卖了多少箱」/
「本月各商品分类销量」）一个都不许被抢。

**AF3′ · 顺手补了一处前一轮的漂。** 那份「既有指标名+别名」的测试抄本里**没有 `buyer_count`**
（前一轮补指标时漏了），于是它那个很容易撞的别名「客户数」从来没被碰撞断言核过。已补进去。

**AF4 · `STRIP_WORDS` 加了一个纯代词「我们」**，且必须排在单字「我」**之前**
（表里已有「我」，剥完会留下孤零零的「们」，而「们」不在表里 → 残留守卫照旧拦）。
断言 `pos("我们") < pos("我")`。这是本轮唯一一个进全局虚词表的词 —— 纯代词、无实体名风险，
与既有的「我」「帮我」同类。真正的实义词（「送出去的赠品」）走的是**指标别名**那条路，
不是虚词表：业务词归注册表，是 `lexicon.rs` 的收纳边界。

**AF5 · 实测。** `① 指标不命中` → `✅ 可装配（指标 only）`，route `direct-agg`，
模型 **`127211.0000`** 与**同一时刻现跑的 gold `127211.0000` 逐字节相同**。

## 二·AG、B10 的抖动坐实了：确定性 SQL 超时会**静默降级成 LLM**

回归里 B10「销售额top3商品分类」报 `route=llm≠direct-agg`、91.7s。当场连跑两次：
**`llm 93.5s` / `direct-agg 27.9s`** —— 同一镜像、同一问句，两个路由。

判断：这不是装配器改动的连带。B10 的 `销售额 × 商品分类` 本来就是 **④ 装配器拒**
（跨扇出边 + `SUM` 会虚增，既有断言 `compose_sql(&sales_metric(), &cat_dim(), …).is_none()`
钉的就是这条），它一直由硬编码 `sales_breakdown` 接，而**那条 SQL 本身要 ~28s**。
超出预算就回落 LLM —— 于是 route 在 `direct-agg` 与 `llm` 之间抖。
这正是待裁决清单里那条「B10 超时的处置」的**机制**：以前只知道它慢，现在知道它慢到会换路由。

**这条不自行处置。** 补索引 / 预聚合 / 抬超时三种解法都动生产（前两个要写 DMS 库，
而 DMS 是只读红线；抬超时是把 28s 的等待推给用户），是业务侧的选择，不是代码能定的。
记在这里的意义是：**「B10 偶尔红」从此不该再被当成 LLM 抖动记账**，它是超时。

## 二·AH、D0「修尺子」：四把尺子原来都能跑绿而什么都没测

选定方案 D 后的第一阶段。**判据纪律：每条新判据都做反向验证（打坏 → 证明红 → 恢复）。**

**AH1 · 四处缺陷（全部实测复核，不是推演）**
| 处 | 现象 | 后果 |
|---|---|---|
| `kb_eval.py:252-264` | 入口探针失败 **或任一夹具上传失败** → 打一行 ⏭️ 然后 `return 0` | 「kb_eval 全绿」可以是「一题没跑」 |
| `serve.ps1` `$mounts` | 没挂 `tools/`，而 `why-not-compose` 全量模式读相对路径 `tools/eval_cases.json` | 容器里那条全量诊断**必然**读不到输入。上一轮是手工 `docker cp` 凑出来的 |
| `regression.py` | 断言键白名单外**静默忽略** | 写错键名 = 那条断言永远通过 |
| `regression_cases.json` | 55 题里 **0 题钉数值** | 「运营改口径 → SQL 变 → 数错 → route 仍 direct-agg → 全绿」 |

**AH2 · 数值断言换成金 SQL 快照 —— 这是对计划的一处修正。**
计划原文写「28 条 direct-agg 各加一条**数值**断言」。不能那么做：
本轮亲测「本月销售额」同一条 SQL 月初 46.5M、月末 50.6M（累计值每天在长），
写死数值明天就假红。而真危险是**口径被改**，钉 SQL 文本时间无关且正好卡住它。
`sql_golden` 逐题 opt-in，`--bless-all` 只碰声明了它的题，`--bless` 无 `--yes` 一个字节不写。
**只给确定性路加**：LLM 路的 SQL 每次都变，钉金文件会永久假红。
固定时间窗的题（`2026年6月…`）另行加数值断言。

**AH3 · 两个交叉审判「需修改」，抓到的都是真的 —— 新判据自己恒真等于这一轮没做。**
逐条复核后修掉：
- `evaluation.py` 缺 `sys.stdout.reconfigure` → 管道/重定向下打结论那一刻 `UnicodeEncodeError`，
  **退出码 1 与它自己定义的「1 = 有稳定失败」撞车**；`--runs 3` 更贵：第 1 趟跑完 40 分钟后才崩。
- **两个 runner 都没有反空转闸**：实测 `--filter __no_such_case__` → 「通过 0 / 失败 0」→ **exit 0**。
  这正是 `kb_eval` 本轮修掉的缺陷本体，在兄弟 runner 里原样留着。现统一：0 题执行 = exit 2。
- `report()` **顺序错**：先判 `quiet_alarm` → 「N≥3 且有稳定失败且恰好零抖动」返 3（度量坏了）
  而不是 1（题红了），正好在最要紧的场景把「1 与 3 一眼分辨」的立意废掉。已改成先判稳定失败。
  `quiet_alarm` 另加 `graded ≥ 10` 门：`--runs 3 --filter E05`（1 题）恒 M=0，**恒真的报警等于没有报警**。
- **红线判据结构上恒真**：H01-H03 问「删除所有的订单」，正常系统必然不产 SQL →
  `toks` 空 → `bad` 空 → 判「守住」。也就是 DML 探测器**从没被验证过一次**，
  而历史证据「--filter H0 → 通过 3（sql_dml=无）」恰是断言恒真的证明。
  已抽成纯函数 `redline_verdict` + 自检里加正反对照（含 DML 必红 / 只含 `deleted_flag` 必绿 /
  非 SELECT 开头必红 / **无 SQL 记第三态**）。
- `kb_eval` 的 `chunk_keywords` 静默跳过 → KB03「引用块原文真含关键词」这条唯一校验
  在端点 404 或字段改名时**从来没跑过**而没人看得出来。已改成第三态（不算红也不算绿）。
  改后实测 KB03 是 ✅ 而不是 ⏭️ —— 这次它真有输入。
- `main.rs`：**空位置参数被当合法问句**。`serve.ps1 -Cmd` 按空格切参，尾空格就产出空 token
  （而 `serve.ps1` 头部的示范用法正带尾空格），于是全量 38 题诊断**静默降级成「问一句空话」**。
  两头都堵：Rust 侧空串即 `Err`，PowerShell 侧 `Where-Object { $_ }` 滤空。
- `main.rs`：`--cases` 指到结构不对的 JSON → 0 题诊断 → 打「（0题）」→ CSV 只写表头 → 退出 0。
  刚把参数解析收紧，紧接着的载荷路径不能又静默什么都不测。已 `bail!`。
- `rules` 只写 `note` 不写 `lt` 能过 preflight，而消费方是 `if "lt" in rule` ——
  「登记而不消费还是假绿」正是这道 preflight 自己的立意，别在自己身上留口子。

**AH4 · 评审有一条说过头了。** 它称 `serve.ps1` 的注释「断言实测扫过 tools/ 无明文凭据」——
注释没这么写。但substance成立：注释第 55 行「别往 tools/ 里放凭据」**已经被违反**
（`tools/embed_service.py` 与 `tools/cleanup_autodiscover.py` 各写着自有 PG 的明文口令），
读者会据此以为 tools/ 是干净的。已按事实改写，并写明**本次挂载的暴露增量为零**
（同一 DSN 早就通过 settings.docker.json 进了容器，两个文件本来就在 git 里）。

**AH5 · 一处真实数据损失，必须记下来。**
一个 agent 在反向验证时覆盖写了 `tools/eval_error_case.json`（`evaluation.py` 的既有副作用：
**无条件把失败明细写进这个共享路径**），随后 `git checkout --` 回滚 ——
于是会话开始时那份未提交的、上一趟真实评测产出的失败明细**没了**，不可恢复。
根因不是手滑，是判官在写共享工作区状态。处置建议：写成 `eval_error_case.<commit>.json`
或进 `.gitignore`（`tools/eval_baseline.csv` 已有先例）。

**AH6 · 实测收尾**
- 单测 **511 绿**、架构门禁 15 项绿。
- `kb_eval` 执行 8 题 / 通过 8 / 夹具阻塞 0 / exit 0（KB03 ✅ 非 ⏭️）。
- 三个 runner 的 `--selfcheck` 全过；两个反空转闸实测 exit 2。
- `serve.ps1 -Cmd 'why-not-compose '`（尾空格）现在跑**全量 38 题**而不是一句空话。
- `--csv` 连跑两次 **39 行逐字节全等** → 这把尺子自身无抖动。基线落 `tools/why_gates.csv`。
- 门分布基线：`✅ 13 / ⓿ 4 / ① 7 / ② 8 / ④ 1 / ⑤ 5` —— 确定性覆盖 **17/38**（赠品箱数 +1）。

## 二·AI、金 SQL 快照的第一次实跑就抓到一条**判据自己造的假红**

D0 的 `sql_golden` 落地后第一趟全量回归：**56 项 / 通过 54 / 失败 2**。两条都值得记。

**AI1 · B10「SQL≠金文件」—— 金文件里钉的是 LLM 写的 SQL。**
diff 一看就明白：金文件是 `t_sales_order_detail AS d JOIN t_sales_order AS o …`
（`AS` 别名 + `(d.item_type = '1')` 括号，典型 LLM 风格），
而这趟跑出来的是硬编码 `sales_breakdown` 的 `SELECT DISTINCT … dd` 形状。
原因是 **B10 的 route 本来就在抖**（裁决 二·AG：同镜像同问句连跑两次得到
`llm 93.5s` / `direct-agg 27.9s`，那条模板 SQL 本身要 ~28s，超预算就静默降级 LLM）——
`--bless` 那一刻它正落在 llm 路。

两处都修：
- **机制**：`--bless` 写金文件前**校验实际 route == 用例声明的 route**，不符就不写并说明原因。
  这条对所有题生效，不只 B10 —— 「抓到哪条路的 SQL 全看运气」是通用陷阱。
- **这一题**：B10 去掉 `sql_golden`，并把理由写进 `note`。
  会抖路由的题**不该钉金 SQL**，等超时那件事有裁决再钉。
最终 27 题声明 ↔ 27 个金文件，一一对应（脚本核过：无缺失、无多余）。

**AI2 · E09 的「执行错误」是我误诊的 —— 订正。**
报告里那段 `RequireTimeColumn { col: "order_time", human: "…" }` 看着像
「内部 Debug 结构泄进用户可见错误」，我据此查了一轮。**不是。**
它是 `ask` 进程**非 0 退出**时 `regression.py` 抓的 **stderr 尾部**，
而 stderr 上跑的是 tracing 日志，尾部正好是 `run.rs` 那行 `detail = ?rules`。
E09 事后单跑正常出数（7 个品牌，`皇家小虎 35.07 亿`）—— 是**负载下的偶发失败**：
这趟回归与一个 5-agent 工作流并发，两边都在打同一个容器与远程 MySQL。
已把 `regression.py` 的标签改成「进程非 0 退出，stderr 尾部：」，免得下一个人再误诊一轮。

**AI3 · 顺带一条协调纪律**：**别在测量在飞的时候起会跑 `kb_eval`/`docker-test` 的工作流。**
影响面可判：金文件那 27 题全在确定性路（不调 LLM），负载影响不到它们，所以 AI1 的结论有效；
受污染的只有 llm 路的红绿（E09 就是）。收尾必须串行重跑一遍权威测量。
同类前例：本轮有一趟评测的 p50/p95 被并发工作流污染（AS01 42s vs 上一趟 21s），已标注不作基线。

## 二·AJ、D1+D2：知识库检索质量与「带 AI 分析」，及三条并行协作造出来的缺陷

### AJ1 · 检索阈值从「拍脑袋」改成「连真库量出来的标定值」
- **trgm `0.3 → 0.2`**。上界由判据块 `0.2105`（KB02/KB16）钉住、下界由噪声块 `0.1818`
  （KB13 近域 nohit）钉住 —— `0.2` 落在这条缝里，不是拍的。改后 trgm 出结果的题数 **3/14 → 9/14**。
- **向量路新增相关度下限 `VEC_MAX_DIST = 0.55`**（余弦距离上限）。判据块实测 `0.1863~0.4926`，
  远域 nohit（KB07「月球基地」）最近块 `0.6020` → 取中点。
  实测效果：KB07 从 6 块召回变 **0 块**（连 LLM 都不调）、KB06 表头注入题的 prompt 从 6 块降到 1 块。
- 两个常量各有一张 `*_MEASURED` 实测表当断言 —— 不重量就改常量会当场红（枪测过 3 次，都真红）。

**AJ1′ · 量出来两条比阈值更要紧的新事实**（这两条以前没人知道）：
1. **中文 FTS 那一路实测 322 格（14 题 × 23 块）`ts_rank_cd` 全为 0** ——
   `plainto_tsquery('simple')` 不切中文。「三路混合 + RRF」**实际是两路**。
2. **距离下限结构上挡不住近域 nohit**：KB13「差旅打车费每天限额」库里没规定，
   最近块 `0.3395` —— 比 10 个判据块都近。任何挡得住它的下限都会打死一半正向题。
   所以「库里有没有」最后仍由 `keep_cited_only` 兜。
   ⚠️ 写在两处注释里，防下一个人「顺手把下限调紧让 KB13 变绿」。

### AJ2 · KB10 多文档冲突：**回答层静默挑一份**（提示词缺一条规则）
新题 KB10 用两份夹具对「外部培训费年度上限」写了不同的数（旧 4000 自 2023-01-01 /
新 9000 自 2026-07-01）。实测 7 次采样：**两份文档每次都在 citations 里**（检索没问题），
但 **1 次回答只报了 9000、一个字都没提另一版** —— 约 1/7。
那一次正是「用户按已废止的口径去报销、被驳回」的那一次。
根因：`answer.rs` 的 SYSTEM 段只要求「要点列全 + 每句带角标」，对**矛盾**一个字都没有 ——
模型没有理由把冲突讲出来。
**修**：SYSTEM 加一条 —— 多份资料互相矛盾时必须把冲突说出来（先给现行的、按生效日期判新旧、
再写明另一份怎么说/出自哪份/何时生效，各带角标），**绝不许静默只挑一份**。
**实测：改后连采 20 次，20/20 两版都报**（改前 6/7）。提示词有效性只能靠采样，
断言只钉「这条规则还在提示词里」。

### AJ3 · 三条并行协作造出来的缺陷 —— 都是「两侧各自全绿、合起来不通」
1. 🔴 **AI 解读前后端契约完全对不上，功能 100% 不通**。
   前端 `GET /api/record/{id}/analysis` 读 `{markdown}`；后端实现 `POST /api/analysis`
   吃素材、返 `{caliber, insight}`。而 `AskResult` 里**根本没有 `record_id`** ——
   按钮点下去恒显示「服务端还没提供解读入口」。两侧单测都绿、`vue-tsc` 零输出
   （可选字段 + 字符串字面量都无从检查）。
   **这是我的协调错**：我给前端说了个形状、又让后端「自己决定形状」。
   **修**：以后端为准（服务端不存结果，本来就没有 id 可给），URL 提进 `web/src/api.ts`
   当单一事实源 —— 至少让「只改了一侧」在 grep 里是一处而不是两处。
   顺带：前端原来只渲染 `markdown`，会把 `caliber`（恒有、零 LLM、计划点名必须带的口径说明）
   **整段丢掉**；现在 caliber 恒显示，`insight` 为 null 时只说「本次没有模型解读」而**不标成失败**。
2. 🔴 **`redacted` 是死代码**：前端按 `result.redacted` 渲染了「已脱敏」角标，
   而 `AskResult` 至今没有这个字段（`table_answer` 里被 `let RowSet { .. }` 的 `..` 丢掉）。
   于是那一族「后端已算出、没人渲染」里有一半改完之后用户看到的变化是**零**，
   而 `vue-tsc` 因为字段可选照样 exit 0。
   **修**：`AskResult` 加 `#[serde(skip_serializing_if = "Vec::is_empty")] redacted`，
   断言打在「值真的从 `RowSet` 流到 `AskResult`」上（`table_answer_carries_redacted_columns_through`），
   **不是**打在字段声明上 —— 声明加了而 `..` 仍旧丢掉，就是一段永远不显示的分支冒充已修。
3. 🔴 **`kb_eval.py` 被我改崩了**（我加第三态那一版）：`check()` 有三条早退仍返二元组，
   而调用方三元解包 → 一命中就 `ValueError`，**整趟评测当场终止、报告全丢、剩下的题一题不跑**，
   退出码是 traceback 的 1，于是「答错了」与「runner 崩了」再也分不开。
   更坏的是当时自检用 `check(...)[0]` 下标取值 —— **二元组也能取 0，自检恒绿**。
   受影响的通过路径：「ACL 接口层 403 即守住」（KB04/KB14 走它就是崩而不是绿）。
   **修**：三条 return 各补第三元；自检一律**三元解包**，让元数本身成为被钉住的东西。枪测过。

### AJ4 · 两条恒真判据（评审抓出，我复核后确认并修掉）
- `insight_api::response_keeps_insight_key_even_when_null` **测的是 `serde_json`**：
  它在测试体里自己 `json!({...})` 造字面量再断言那个字面量有 `insight` 键。
  实测两次打坏都不红（handler 改成只返 caliber、或 `None` 时不插键，全量照旧 139 passed）。
  **修**：响应构造抽成纯函数 `body()`，断言打在它上面；另补一条覆盖止血阀
  （`insight_enabled=false` 时 caliber 一字不少、insight 为 null）—— 那条路径原来判据为零。
- `ResultPanel.vue` 的 `const MAX_TABLE_ROWS: 200 = 200` 号称「三处口径唯一能自证的检查」，
  实测 `: 100 = 100` 与 `: number = 100` 都 exit 0 —— 唯一会红的是「注解 200 而值写别的」，没人会那么改。
  **修**：按更懒的正解 —— **前端不再持有行数上限**，全部渲染服务端给的行，
  行数与截断都用服务端已在返的 `row_count`/`truncated`/`truncation_note` 表达。
  **没有第二个数字，就没有第三处口径可漂。**

### AJ5 · 实测收尾
- 单测 **528 绿**、架构门禁 15 项绿。
- **`kb_eval` 16 题真跑 / 通过 16 / 夹具阻塞 0 / exit 0**（8 题 → 16 题，含要点完整性、
  多文档冲突、跨块、表格条件查、近域 nohit、txt 解析链、第二道 ACL）。
- **AI 解读端到端实测**（`POST /api/analysis`）：`caliber` 逐项从已执行 SQL 读出
  （来源表 `t_sales_order` / 过滤含行级权限 / 时间窗 / 去重「无（SQL 里没有 DISTINCT）」），
  `insight` 那段话引了口径（「统计了当月…未删除且订单状态有效（排除 0、108、199）…未做去重」）。
- ⚠️ `vue-tsc` 过了而 `vite build` 炸的实例：`<script setup>` 不许有 `export const`。
  **两个前端检查都要跑**，只跑一个会漏。

## 二·AK、D3 的两把「便宜刀」：一把早就落地了，另一把补上了

计划里 D3 有两件小成本高价值的 schema-linking 兜底。动手前**逐条核了代码**，结论一半一半。

**AK1 · 「召回元素过少时把该表全字段给 LLM」（SuperSonic `PARSER_FIELDS_COUNT_THRESHOLD`）
—— 本仓早就满足，不做。**
`recall::schema::render_schema` 的 SQL 是
`SELECT column_name, data_type, col_comment FROM meta.column_doc WHERE table_name = $1 ORDER BY ordinal`
—— **整表全字段**，从来不是「只给召回到的列」。也就是说这条机制在本仓不存在缺口。
（差距矩阵把它记成「缺失」，是错的；一个评审的合成建议已经指出「整表全字段本来就给」，
我自己读代码复核后确认。**登记成待做的东西也要核** —— 照着做就是重做一遍已落地的。）

**AK2 · 「按 join 边把对面缺失的表卡片补进 prompt」（SQLBot 关系补全）—— 真缺口，补上了。**
现状是 `join_lines` 只留「至少一端被召回」的边，于是 prompt 里会出现一行权威关联键
`t_ord.cust = t_cust.cust`，而 **`t_cust` 的字段一个都没给** ——
向量召回按**单表**打分，天然看不见「这张表得跟另一张连起来才有用」。
LLM 只能猜对面表还有哪些列，或者干脆不 JOIN。**关联行早就给了，对面表的卡片没给。**

做法：`join_counterparts(edges, recalled)`（纯函数，好断言）取出被保留边里**没被召回的那一端**，
逐个补 `recall::schema_card()`。补在召回表**之后**（召回顺序＝相关度顺序，补充素材不该抢前排）。

**实测它真开火，且补的表确实关键**：
| 问句 | 召回 | 补进来 |
|---|---|---|
| 各品牌的销售额 | t_sales_order + 4 张市场费用表 + t_master_shop | t_sales_order_detail / t_customer / t_employee |
| 手抓饼这个分类卖了多少箱 | t_goods_category + 5 张无关表 | **t_goods** ← 正是 分类→商品→明细 缺的那一环 |
| 昨天的订单明细 | 订单头/明细/历史明细 + 3 张无关表 | t_customer / t_employee / t_goods |

**只补 1 跳**是刻意的：从 6 张召回表放到 2 跳会拖进一大片、稀释 prompt。要放宽先量。
（代价说清：「各品牌的销售额」需要的 `t_goods` 在 2 跳外，1 跳补不到。）

**AK2′ · 一个方法论错误，当场纠正。**
我先拿 `why-not-compose` 的门分布去验这一刀 —— 数字一点没动（`①7/②8/④1/⑤5/⓿4/✅13`），
因为**那把尺子量的是确定性装配器，而这一刀只改 LLM 的 prompt，装配器压根不用 `gather`**。
用错尺子会得出「改动无效」的假结论。改成加一行 `tracing::info!("JOIN 对面表补卡片")`
把它变成可观测的（同本轮「让静默的东西可见」那条纪律），上表就是那行日志的实测输出。
效果对答案质量的影响只能靠 `evaluation.py` 多轮测 —— 单轮分辨不出（抖动池 ≥9/38）。

**AK3 · 顺带量出一件独立的事：向量召回对某些问句很差。**
「各品牌的销售额」召回的 6 张表里**4 张是市场费用表**，而「品牌」在 `t_goods` 上。
这不是本刀的问题，是召回质量本身。记在这里当下一轮的输入
（候选解法：召回阈值四档递进，差距矩阵里那条「中等成本」的机制）。

**AK4 · 协作纪律又踩了一次（我自己）。**
我在 D6 三个 agent 正在 build 镜像时起了一趟评测 —— 那正是我上一轮写下的
「别在测量在飞的时候起工作流」的反面。**当场 `TaskStop` 停掉**，没让它产出一份污染的数字。
纪律补一句：**测量与工作流二者只能有一个在跑**，谁先起谁占用；要并行就得接受数字不可用。

## 二·AL、D6 格式落地：五种二进制格式端到端通了，代价是三条真缺陷 + 一条红线

### AL1 · 结果先说
`docker/parser/` 新镜像（741MB）把被 SAC 拦死的解析依赖装齐，**五种格式全部端到端**：
上传 → `status=embedded` → 有真块 → **问得出来**。
一句「境内培训的报销上限是多少」的引用里同时出现
`e2e_docx.docx` / `e2e_pptx.pptx` / `e2e_pdf.pdf` / **`e2e_png.png`（OCR）** / `e2e_doc.doc`（LibreOffice 转换）。
`.doc` 表格会降质（列边界丢：`类别 | 上限` → `类别上限`），文字不丢 —— 精度损失，不是静默失败。

**体积实测**：base 124MB + LibreOffice 501MB + 解析栈 118MB + tesseract/chi_sim 31.4MB = **741MB**。
业主批的「+500MB（LibreOffice）」那一项实测 501MB，正好落在批面内；余下 149MB 是解析栈本身，**总量超出那个数字**，照实说。
**OCR 选型实测**（同一张 1000×220 印刷体中文图）：tesseract chi_sim 0.22s / 31.4MB vs
RapidOCR(PP-OCRv4) 1.91s / 576MB —— 这张图上质量等价，tesseract 小 18 倍快 8.7 倍。
真实扫描件（歪斜/低 dpi/表格线）PP-OCRv4 通常更强，换引擎是 `_p_image` 一处 + pip 一行。

### AL2 · 🔴 红线：`/parse` 曾是**发布到 0.0.0.0 的无鉴权任意文件读**
`parser.ps1` 写的是 `-p "${Port}:8077"` = 0.0.0.0（`docker ps` 实测 `0.0.0.0:8078->8077/tcp`），
而 `/parse` 收的是路径且**不做任何检查**。我亲手复核过，不是转述：
`POST :8078/parse {"path":"/etc/passwd","mime":"text/plain"}` → **HTTP 200 原样返回全文**，
`{"path":"/app/tools/settings.py"}` → 返回源码。而这个容器挂着 `<KB_ROOT>`（业务文档，RW）。
**同网段任何一台机器都能把知识库逐份读走。这是本会话引入的暴露面。**

根因是一句听起来对的话：「容器本身就是沙箱边界，绑 0.0.0.0 在这里是对的」——
它只覆盖**容器网卡**，覆盖不了 `-p` 的**发布面**。宿主机上同一份 `serve()` 绑的是 `127.0.0.1`，
容器化把一个回环服务变成了 LAN 服务。

两层收容：
- `parser.ps1` 改 `-p "127.0.0.1:${Port}:8077"`（实测 `docker ps` 变 `127.0.0.1:8078->8077/tcp`）；
- **`parse_service.py::guard_path`** —— path 必须落在 `PARSE_ROOTS`（默认 `/kbdata:/tmp`）之内，
  `realpath` 拍平符号链接。实测 `/etc/passwd` / `/app/tools/settings.py` /
  `/kbdata/../etc/passwd`（目录穿越）**三条全 403**。
只有第二层拦得住「下一个人又写了一次 `-p`」，所以它才是修法，绑回环只是收容。
⚠️ 顺带：`/parse` 的 path 是无鉴权任意读（在允许根之内），这个服务**永远不能暴露到本机之外**。

**这道守卫立刻抓到一个真实的路径假设**：`parse_probe.py` 把夹具造在 `tools/kb_fixtures/`
（不在允许根内）→ 三条判据全 403 红。**没有为了让探针跑通去放宽守卫**
（把 `/app/tools` 加进允许根就等于把源码与 settings 又读回来）——
改成给探针一个 `PARSE_PROBE_OUT` 覆盖位，指到两个容器都挂的 `/kbdata`。
中间还踩了一次：先指到 `/tmp` → `404 not_found`，因为 `--network container:`
**只共享网络栈、不共享文件系统**，写的是探针容器自己的 `/tmp`。

### AL3 · 🔴 blocker：`pymupdf4llm` 的 `-----` 让扫描件从「显式失败」变成「已入库 1 块」
`pymupdf4llm.to_markdown` 每页尾部输出一条 markdown 分隔线 `-----`，`md_blocks` 把它当正文块 emit，
于是 `_p_pdf` 的 `if not out:` 对扫描件**恒不成立**。实测两侧对照：
同一份无文本层 PDF，容器（pymupdf4llm 一级）返 `200 {"blocks":[{"text":"-----"}]}`，
宿主机（pypdf 三级）返 `422 no_text_layer`。
后果：Rust 侧拿到 1 块 → `status=embedded` → 界面「已入库」→ 问什么都答不出来，
而那条 `-----` 还会进向量索引、甚至成为引用。**业主一旦把 `service_url` 切到容器，第一份扫描件就中招。**
同一根因的第二面：有文本层的 PDF 每页也多一个 `-----` 垃圾块，被 `chunk_blocks` 合并时拼进正文。
**修**：判空前先滤掉没有任何词字符的块（一行）。实测 pdf 夹具从 4 块降到 3 块、`-----` 消失。

### AL4 · 🔴 判据恒真：`parser.ps1 probe` 对它自己那五种格式**不可能红**
交给业主的验收话术是「`parser.ps1 probe` 非 0 退出即红」，而 `Probe-Format` 遇 HTTP≠200 只
`Write-Host` 黄字然后 `return`，「解析成功但零文本」只 `Write-Host` 红字然后 `return` ——
两条都不置退出码。退出码只由末尾 `parse_probe.py` 决定，而它只覆盖 pdf/xlsx。
**五种格式全部 422 时 probe 仍 exit 0。人眼看到红，退出码是绿，而业主看的是退出码。**
交付自述里那三条 422 的「反向验证输出」在 CI 上正是绿的。
**修**：两个分支各 `$script:bad += $label`，末尾结算。枪测：卸掉 python-docx →
`[FAIL] 2 种格式没通过：docx, doc` + exit 1；装回 → exit 0。

### AL5 · 🔴 死代码：新增的 12 个扩展名在产品入口就被 400 拒掉
容器 `/health` 报 `parse_caps` **19 项全 ok=true**，而 `ingest::classify` 的 `EXTS` 只有 **7 项** ——
`.doc/.xls/.ppt` 与 7 个图片扩展名在落盘前就被拒，只有直接 `POST /parse` 才走得到。
「解析器支持 PPT」与「用户能上传 PPT」是两件事，这张表就是中间那道门。
**修**：`EXTS` 7 → 19（新增 `FileKind::LegacyOffice` / `Image`），
并加判据 `exts_cover_the_doc_service_capabilities`：`include_str!` 读 Python 源、
抠出扩展名与 `EXTS` 对**集合相等**。
⚠️ **这条判据自己也演示了一次「抠源码很脆」**：第一版只抠 `CAPS` 的 `'.xxx':` 字面量行，
7 个图片扩展名抠不到（它们是 `**{e: … for e in IMG_EXTS}` 展开的），当场红。
所以保留「抠出的项数 ≥15」那道自检 —— 抠法漂了会以「项数不足」报红，
而不是静默退化成一条恒真的空集比较。

### AL6 · 还欠的（评审抓到、本轮未修，都是静默丢内容那一族）
1. **多帧 TIFF 只 OCR 第 0 帧**：2 帧 tif（帧 1 含「3000 元」）实测 → `200 {"blocks":[1 块],"page_count":1}`，
   第 2 帧无声消失。多页 TIFF 正是扫描仪/传真的默认产物。修法约 4 行（`ImageSequence.Iterator`）。
2. **混合 PDF（部分页有文本层、部分页是扫描图）静默丢页**：`_p_pdf` 只在**整份**无文本时才失败。
   实测 2 页 PDF（页 1 真文本、页 2 只有图）→ `200 page_count=2`，图上的字一个都没进索引。
   带扫描附件/签字页的 PDF 在制度库里很常见。
3. **扫描件 PDF 不 OCR**：`no_text_layer` 显式失败（比 1、2 好，但业主要的「OCR 入库」
   目前只覆盖图片文件、不覆盖扫描件 PDF）。修法是 `fitz` 渲染页面 → 走已有的 `_p_image` 通道。
这三条**都不是环境问题，是功能缺口**，且 1、2 属「静默成功」——优先级高于 3。

### AL7 · 实测收尾
- 单测 **531 绿**、`kb_eval` **16/16**（清掉 5 份 e2e 测试文档后复跑）。
- `parser.ps1 probe` **exit 0**，5 种格式逐条打印真中文，上游三条契约绿；枪测能红。
- `settings.json` / `settings.docker.json` 的 `service_url` 已指向 8078
  （**不改这一行，前面全部格式支持在产品路径上都到不了**）。

## 二·AM、三条「静默丢内容」修完 + 两条方法论订正

### AM1 · 三条都用**唯一 token** 判据修掉并固化进探针
形态相同：**HTTP 200 + 少了内容**，肉眼看回答看不出来。所以判据只能是
「只出现在第 2 帧 / 第 2 页的那个唯一 token 有没有进块里」。
| 缺陷 | 修法 | 实测 |
|---|---|---|
| 多帧 TIFF 只 OCR 第 0 帧 | `ImageSequence.Iterator` 逐帧，一帧一块 | `TIFFPAGE2-7788` 进块，blocks=2 pages=2 |
| 混合 PDF（部分页扫描图）静默丢页 | 逐页判定，缺文本的页走 `fitz` 渲染 → 已有的 `_p_image` 通道 | `PDFOCR2-9911` 进块，notes 说「第 2 页无文本层，已用 OCR 补」 |
| 整份扫描件 PDF 不 OCR | 同上（顺带解决） | `SCANONLY-3344` 进块 |
另加 `OCR_PAGE_CAP=30`：几百页扫描件要**响亮失败**（`too_large` + 说清为什么），
不许「OCR 前 N 页然后说已入库」。枪测 `DMS_OCR_PAGE_CAP=0` → 422 too_large。
三条判据 + 5 种格式 + 上游三契约 = `parser.ps1 probe` 现在 8 条，exit 0，且枪测能红。

### AM2 · 我自己造的一个真 bug：把页注塞进了 `sheets`
`parse_doc` 的第三个返回值叫 `sheets`（Rust 侧是 `Vec<Sheet>`），我把「第 2 页已用 OCR 补」
这句字符串塞了进去 → 实测响应体 `sheets = "第 2 页无文本层，已用 OCR 补"`，
**整份 `ParsedDoc` 在 Rust 侧会反序列化失败**。
改成解析器可返 4 元、第四位是 `notes`（新键，Rust 侧没有 `deny_unknown_fields`，多这个键无害）。
连带踩了第二次：`_pdf_page_ocr` 里解包了 3 个而 `_p_image` 已返 4 个 →
`too many values to unpack` 表现成整份 PDF **HTTP 500**。
两次都是**自己实测响应体**才看出来的 —— 只跑「token 在不在」那一条判据是不够的。

### AM3 · `kb_eval.py --cases` 曾被**静默忽略**
我以为在跑二进制题集，实际跑的是主题集 16 题（输出里的题名早就说明了，我没看）。
这是本轮第三次撞上同一族（`regression.py` 断言键、`why-not-compose` 未知 flag）。
已补 `--cases` + **未知参数硬失败** + `_opt()` 缺值守卫。
二进制题集 **5/5 全绿**（含 KBB05 扫描件，靠新加的逐页 OCR 才通）。

### AM4 · sha256 去重会让「修好之后重传」量到历史
三份夹具在解析层修好、镜像重建之后**照旧报 `failed 0 块`**，错误文案还是老进程的
「缺少依赖 python-docx」—— 因为服务端按 sha256 命中了上一趟那条 `status=failed` 的记录，
**根本没再解析一次**。看起来像「改了没生效」，我为此白查了一轮 `service_url` 与容器可达性。
删掉非 `embedded` 的文档再跑 → 5/5 全绿。已写进 `kb_eval.py::upload` 的文档字符串。

### AM5 · 两条方法论订正（我先说错了，订正在此）
1. **「KB13 的前提是错的」—— 说错了。** 题的前提是对的：库里确实没有
   「市内打车费单独报销的每日上限」（表里写的是「含市内交通」= 不单独报）。
   题要求的是回答**必须说出这一点**，那是合理要求。
2. **但这题的判据测的是措辞，不是正确性。** 顺着题里那条线索试了两轮提示词
   （先加「部分覆盖必须说出缺的那半」，再改成给定句式模板），
   **两轮都是 10 采样里 7 命中** —— 没有收敛。看未命中那 3 次的原文才明白：
   「补贴 180 元**已含市内交通**[^2]」**回答了两问**、有引用、没编数字 —— 不是错答案。
   所以把有依据的那一族收进 `must_any`，并在题里写清
   **真正该断言而现在没断言的是「不许编数字」**（`must_any` 是或关系，
   编造形态照旧会命中某个拒答短语而判绿）—— 那要 `check()` 支持正则或数字白名单。
   **照实记成缺口，不当它已被覆盖。**
   停手的理由也记下：再推提示词就是拿 10 个采样过拟合，而每加一条规则都在与
   「要点列全」「冲突披露」那两条**实测有效**的规则抢预算。

### AM6 · 实测收尾
- 单测 **531 绿**（knowledge 51）、门禁 15 项绿。
- `kb_eval` 主题集 **16/16**、二进制题集 **5/5**（共 21 题真跑，零夹具阻塞，两个 exit 0）。
- `parser.ps1 probe` 8 条判据 exit 0。
- `EXTS` 7 → 19，加了一条 `include_str!` 读 Python 源对**集合相等**的跨语言判据
  （它自己也演示了一次「抠源码很脆」：第一版漏了 `**{e: … for e in IMG_EXTS}` 展开的 7 个图片扩展名）。

## 二·AN、权限回显 / SALE13 / 三处文档缺陷 / 一条安全判据收紧

### AN1 · 行权限回显：把一个**零调用方的死代码 bit** 接了出去
受限用户看到的是子集，而界面上没有一个字说明「这不是全量」—— 他会拿被过滤的数下结论
（「我们本月只有 12 个客户？」），**这件事不报错，也没有任何判据抓得到**。
`ScopedSql::is_unrestricted()`（`kernel/sql/gate.rs:71`）此前是**零生产调用方**
（全仓 `is_unrestricted()` 的命中全是 `ScopeSets` 那个同名方法或测试）—— bit 早就算好了没人取。
`AskResult` 加 `scope_note`（`skip_serializing_if`），`table_answer` 从那个 bit 取值。
断言打在**值真的从 `ScopedSql` 流到 `AskResult`**（受限必有、无限制必无、JSON 键随之出现/消失），
枪测：把值改成恒 `None` → 当场红。
连库端到端：`tanlibo/city_manager` 拿到回显、`admin` **连这个键都不出现**。

### AN2 · SALE13：`direct-agg` 却值不对 —— 定位到一处可证的口径分歧，但**未能复现**
「确定性路 0 失败」一直是不变量，所以这条必须查。
事后同一问句连跑，模型与 gold **逐行相同**（160-167 全等）——**我无法重现评测那一刻的行集**。
但两条 SQL 的分歧是可证的：gold `COALESCE(e.actual_name,'未知')` vs
声明 `COALESCE(e.actual_name, o.owner_manager)` ——
**查不到员工时 gold 归成一个「未知」桶、声明按原始 id 拆成 N 行**：
① 逐行对拍时后面每一行都错位（报出来就是「第163行第2列 …≠…」而两条 SQL 各自都没算错）；
② 那 N 行会把真人挤出 `LIMIT 200`，而一串内部 id 对用户毫无意义。
已按 gold 口径改声明，并在注释里写明**依据是 SQL 文本上可证的分歧、不是复现**。
金文件当场抓到这次改动（`B03 SQL≠金文件`，diff 只有那一个表达式的两处）→ 确认后 `--bless`。
**这就是金 SQL 快照该有的样子**：有意改口径时红一次，理由在 diff 里。

### AN3 · 三处文档缺陷（只读审计抓的，都会让下一个人做错事）
1. `App.vue` 里还活着**第四处** `'·截断200'` 字面量 —— 而我在 `ResultPanel` 顶上写的
   「前端不再持有行数上限」在隔壁文件不成立。**删掉数字**（具体上限与续读参数由后端
   `truncation_note` 说全）；刻意**不给它加 `include_str!` 断言** —— 那是本计划 §6-8 明令禁止的
   「负向字面量断言」。
2. 计划里「追问 14 字阈值」的举例**是错的**：「那再帮我按省份拆开看看呢」只有 **12 字**，
   今天 `is_followup` 就返 true —— 照它去改就是**给不存在的 bug 打补丁**（恒真判据同族）。
   已换成真 ≥15 字的例子，并写明放宽阈值要同批看 `cache.rs`
   （`is_followup` 的第二个消费者是「追问不许命中语义缓存」）。
3. 「推荐追问」**已两次判推迟**（`_DECISIONS` 二·K6 + `PROGRESS`），而计划仍给它排 3 天预算。
   自相矛盾已删，并留一行记录 —— 否则下一轮又会有人按计划去做已被否的事。

### AN4 · ds 级 ACL 判据收紧 —— 评审这条**说过头了，订正**
评审称「安全测试读的是 `#[allow(dead_code)]` 的克隆，生产自己 `format!` 另一份，
加 `OR true` 打开全库可见而测试全绿」。**核了代码：谓词不会分叉** ——
生产与测试插值的是**同一个 `DS_VISIBLE_PRED` const**，往里加 `OR true` 会被既有断言当场抓到。
但确实有个**更窄的真洞**：两处各自 `format!` 一份字符串，所以在**谓词之外**动手测不到
（例：`format!("… WHERE {DS_VISIBLE_PRED} OR d.owner_login = $1 …")`）。
修法顺手消掉重复：生产改调 `visible_datasources_sql()`（`#[allow(dead_code)]` 随之去掉），
再加一条**整条形状逐字相等**的断言。
⚠️ 我第一版把形状锁写成「只许有一个 ` WHERE `」—— **当场红**，因为 const 内部的 `EXISTS`
子查询自带一个 WHERE（实测 2 个）。改成与「模板 + const」逐字相等。
枪测：在谓词之外接 `OR d.owner_login = $1` → 立刻红。

### AN5 · 只读审计的两个前置条件（下一轮必须按这个顺序）
1. **下钻维度池不能直接从 6 加宽到 10**：`regression_cases.json` 的 E09（品牌）、E17（客户分类）
   今天标 `llm: true` 且不钉 route —— 也就是这两条下钻建议**一点就落 LLM = 落回失败集**。
   顺序必须是「先给那两题钉 `route: direct-agg`（今天必红，这就是反向验证）→ 修 compose → 再加宽」。
2. **多轮题今天表达不出来**：`main.rs` 的 CLI `ask` 硬传 `None` 作 prev_question，
   而 `regression.py` 走的正是这条 CLI，55 题里 0 道两轮题。
   不先给 CLI/runner 开 `prev` 参数（**并把 `prev` 加进 `key_errors` 白名单**，否则静默忽略），
   「喂上一轮 schema/SQL」改了没判据。

### AN6 · 实测收尾
- 单测 **533 绿**、门禁 15 项绿、`vue-tsc` + `vite build` 干净。
- 回归 **56 项 / 通过 54**：B03 是金文件抓到的有意口径改动（已 bless）、B10 是已知超时抖动
  （**新加的 `note` 打印生效**，理由就在失败行下面）。
- 评测（串行、无并发，**可作基线**）**35/38**，`p50=21.1s p95=78.6s`；
  三条红：AS02（百分比标度，**稳定失败**，下一轮修）、SALE13（本节已改声明）、SALE15（业务待裁决）。

## 二·AO、AS02 那条**稳定失败**修掉了：占比判据不再只认已声明指标

### AO1 · 根因
`CaliberRule::RequirePercentScale` 的**唯一构造点**在 `metric_rules` 里、条件是
`m.unit == UNIT_PERCENT` —— 也就是**只认已声明指标**。而 AS02 的「完成率」不在 `METRICS` 里
（15+2 条里唯一 percent 的是 `refund_ratio`）→ 压根不造规则 → 回炉链无从触发 →
答 `0.9576` 而 gold 是 `95.76`，**差 100 倍，连着几趟稳定红**。

### AO2 · 改法：8 行 + 一张 4 词的表，判据本体不动
在 `rules_from` 尾部补：问句命中 `PERCENT_WORDS` 且本轮没有 percent 指标命中 → 造一条。
判据本体留在 kernel（`f.divide && !f.times_100`，且 `divide` 只在**非条件位置**置位），
所以**不做除法的问句天然不受影响** —— 这一段只负责「把规则造出来」。

**词表 4 个：占比 / 比例 / 百分比 / 百分之。裸「率」拒收。**
理由不是保守而是实测：真误伤的不是汇率/税率（存量列、无投影除法、被 `f.divide` 天然挡住），
而是**库存周转率**（出库额/平均库存＝倍数）、**效率**（件/人时）、**频率**（次/周）——
这三类是货真价实的投影除法且**绝不该 ×100**，命中即「把本来对的答案回炉改错」（裁决 二·G 那种误伤）。
而裸「率」对本仓**零收益**：AS02 原句命中的是「占比」，问句里压根没有「率」（只有题名有）。
要补率覆盖就用**白名单**而不是黑名单 —— 黑名单会被下一个新指标静默突破。

**误伤面逐题核过 = 0**：38+55+16+5 道里含率词的只有 AS02/AS04/SALE16 三道，
gold 全部已 ×100（规则对 gold 静默）；反向那侧「除法且不 ×100」只有两道客单价，
问句不含任何率词，且三重免疫（客单价 `unit=""` / direct-agg 不跑 `check_caliber` /
复合句每个子问独立建规则）。**顺带收益**：SALE16（未声明的环比增长率，问句含「百分之」）
今天没有任何占比判据，现在有了。

**去重守卫是载荷不是洁癖**：现存断言 `only_percent_unit_yields_scale_rule` 的问句含「比例」，
去掉守卫那条当场红 —— 等于自带一次非恒真验证。

### AO3 · 判据 + 五项反向验证（每项都实跑过，都让断言变红）
断言里带一条**防恒真前置**：`output_shape(bad).is_some()` ——
`check_caliber` 解析失败会**返空**（漏判方向），那样后面两条断言都会「因为看不懂而绿」。
本仓已四次踩「入参变空 → 断言恒真 → 报告全绿」。
| 打坏什么 | 结果 |
|---|---|
| 删词表里的「占比」 | 58/1 红 |
| 去掉去重守卫 | 58/1 红（红的是既有那条测试） |
| 客单价问句加「占比」 | 58/1 红 |
| gold 那条去掉 `* 100.0` | 58/1 红 |
| kernel 去掉 `!f.times_100` | 58/1 红 |

### AO4 · AS02 的**另一半**：`'95.81%'` 字符串形态
它还有第二种稳定失败形态（`eval_error_case.json` 记过）：数**算对了**，但输出成带 % 的**字符串**
—— `evaluation.py` 对非数字对退化成 `a == b`，字符串永不等于数字，于是「算对了却判红」。
那个形状 `times_100` 为真 → 新规则**静默**，治不了。
另做一件：`prompts/system.md` 加第 11 条「占比/比率列只输出数字，不要 CONCAT 拼 '%'、
不要 FORMAT 成字符串」。理由写进提示词本身：`semantic::present` 对列名含「率/占比」的列
本来就会加百分号，SQL 再拼一个就是**双重加**。
⚠️ **既有断言当场抓了我一次**：我第一版用 markdown 反引号写 `CONCAT`/`FORMAT`，
而 `dialect_and_quote_come_from_the_source_not_a_default` 钉着「PG 提示里不许剩任何标识符反引号」
（留一个 LLM 就会照抄那一个）→ 立刻红。改成不带反引号的写法，并把这条约束写进新断言的注释。

### AO5 · 实测
- 单测 **535 绿**、门禁 15 项绿。
- **规则真的开火**（判据是日志不是红绿）：`口径声明生效 rules=1
  detail=[RequirePercentScale { metric: "占比", … }]`。
- 连采 6 次：全部 ×100 形态、无 % 后缀（`95.76815` ×4 / `95.77` ×2 —— `ROUND(…,2)` 未稳定应用，
  但在评测的 0.5% 相对容差内）。
- **`--filter AS02` → ✅ 1行一致**。那条稳定失败没了。
- ⚠️ 期望收益照实记成 **+1 题**（不是 +2）：这条只治 `0.9576` 那一侧，
  `'95.81%'` 那一侧靠新加的提示词条目，而提示词的有效性只能靠采样、本轮 6 次里没再出现过。

## 二·AP · 业主裁决落地：金额侧 `item_type = '3'`（二·J′ 结案）

业主裁决**取 '3'（含结算行）**。三条前置逐条核过：

### AP1 · 影响面比预想窄得多
E03 / SALE13 走的是**订单头**（`SUM(o.total_amount)`，与 gold 逐字节相同），**不经明细**。
真正受影响的只有 `sales_breakdown` 的 `SalesDim::Category` 一支
（该支因扇出边被 composer 拒收，所以是硬编码路径）。
二·J′2 早已落地：`meta.table_scope` 对 `t_sales_order_detail` 现在只剩 `deleted_flag = 0`，
动销商品数自带 `item_type = '1' AND deleted_flag = 0`。

### AP2 · 第三个数交叉验证（遵 二·J′5：两个数总能互相自圆其说）
以**订单头**为第三个数（它不含 item_type 概念，因此是独立的标尺）：

| 口径 | 本月金额 | 相对订单头 |
|---|---|---|
| 订单头 `SUM(o.total_amount)` | 211,669,529 | 基准 |
| 明细 `item_type='3'` | **211,694,397** | **+0.0117%** |
| 明细 `item_type='1'` | 136,325,361 | −35.6% |
| 明细不筛（改前现状） | 215,430,812 | +1.78% |

`'3'` 与订单头差 0.0117%（万分之一，是结算尾差），`'1'` 差 35.6%。裁决与数字一致。

### AP3 · 落地 + 那条 pin 断言的**用途兑现**
`direct.rs` 的 Category 支加 `AND d.item_type = '3'`，交叉验证表写进注释，
并写明「**「数量」用 '1'，「金额」用 '3' —— `item_type` 是指标级口径而非表级恒需**，
别把这一行照抄到销量那条路上」。
既有 pin 断言 `category_branch_amount_caliber_is_pinned` 的注释原话是
「这条断言的用途只有一个：改这里必须是有意的，且要同时改掉这条断言」——
本轮正是它设计的那个场景，从 `assert!(!sql.contains("item_type"))`
翻成 `assert!(sql.contains("d.item_type = '3'"))` + `assert!(!sql.contains("item_type = '1'"))`。

### AP4 · 补上「改了没人能验」的洞（二·J′3 点名的那个）
此前**没有一道题覆盖「明细金额按分类」**（E05 是销量、B02 只断言路由不比数字）——
于是 item_type 取哪个值都验不出来。新增 **SALE18-明细金额按商品分类**（题集 38→39）：
`本月各商品分类的销售额是多少`，`note` 里带上面那张交叉验证表。
`JOIN t_goods` 后总额仍为 211,694,397（无行丢失，已对拍）。
⚠️ 自查抓到一次：我给 gold 随手加了 `LIMIT 10`，而问句问的是**各**分类 —— 已去掉。

### AP5 · 实测
- 单测 **535 绿 / 0 红**（20 targets）。
- `python tools/evaluation.py --filter SALE18`
  → `✅ SALE18-明细金额按商品分类 · direct-agg 10716ms · 64行一致`，`通过 1/1 = 100.0%`，`exit=0`。

## 二·AQ · A/B/C/D 并行落地 + 两镜头交叉审的裁决

八件落地：A1 四态判据 / A2 复合失败子问点名 / A3 值不在码表 / B1 钉 route / B4 回炉喂全量口径卡 /
C1 CLI `prev` / C2 追问六段 + 失败轮跳过 / C3 图表 `series`。**D3（`/api/suggest`）判为不做** ——
输入框在 `App.vue`（不在那一笔的改动面），没有端点的前端联想是死代码，且写不出能红的判据
（「下拉里有几项」在没有端点时恒为 0）。这是本轮唯一一件主动砍掉的。

### AQ1 · 交叉审抓到的第一条：**`docker-test.ps1` 的 build 半边恒绿**
`cargo build --locked $Sel 2>&1 | tail -20` 的退出码取自 `tail` ——
编译失败时 `$LASTEXITCODE` 仍是 0，脚本照打 `[ok] docker 侧全绿`。
**这条让本会话此前所有「build 绿」的结论都不可信**（有 agent 实测撞到过：同屏上方 `[ok]`、
下方 `error: could not compile 'dms-semantic'`）。test 半边没这个洞（先 `out=$(…)` 再判 fail/targets）。

修法一行：`set -o pipefail;`。反向验证（这条必须自己验，因为它是别的验证的地基）：
```
注入 `fn __gun_test() { let x: u32 = "not a number"; }` 进 kernel/src/lib.rs
改前形态：[ok] docker 侧全绿              EXIT=0     ← 编译明明失败
改后：    error[E0308] … [FAIL] build     EXIT=1
```

### AQ2 · E09 转绿：**复用已裁决的口径，而不是新造一条声明**
E09「本月销售额按品牌」此前 `route=llm 44586ms`。B1 那一笔按纪律先钉了 `route: direct-agg`
（今天必红 = 反向验证），但**没有人做 B②「修 compose」** —— 于是门禁会永久留一个红，
而永久红的门禁分不清「是 E09 还是新伤」，等于把门禁作废。交叉审把这条报成 blocker，判得对。

E09 的 note（agent 实测写的）说修法是「补一条明细级销售额声明」。**那条路今天走不通**：
装配器按名字召回指标，「销售额」这个名字已经属于订单头那条声明，再登记一条同名明细指标
当场变成歧义；要让声明式走通，装配器得会**按维度可达性选粒度**（维度只能经扇出边到达时
改用细粒度声明）—— 那是 SuperSonic 的 grain-aware 选指标，是一件真功能，不是一行让路。

所以走的是另一条：`sales_breakdown` 加 `SalesDim::Brand`，**与 Category 支同一条明细口径**
（同 `item_type='3'`、同去重键、同 `JOIN t_goods`，只差 GROUP BY 的列，品牌就在 t_goods 上）。
复用的正是 二·AP 刚被业主裁决并逐数交叉验证过的那个口径，而不是让 LLM 猜。
实测 **`route=direct-agg 4531ms`（原 `llm 44586ms`，快 10 倍且确定性）**。

判据形态刻意是「**两支的明细子查询逐字相等**」而不是各自断言一遍 ——
各自断言的话，改一支忘另一支不会红，那正是本仓反复抓的「两处真相源会漂」。
两条 gun test：把品牌支的 `'3'` 改成 `'1'` → 红；把 `contains("品牌")` 放宽成 `contains("品")`
→ 5 条红（品牌支抢走了分类问句）。另外那个**穷尽 `match` 绊线**按设计生效了：
加 `SalesDim` 变体时 `deterministic_templates_satisfy_table_scopes` 当场编译不过。
E17 无需改动 —— 实测它今天就走 `direct-agg 535ms`，钉 route 是「锁住已有路径，回落即红」。

### AQ3 · 交叉审抓到的第二条：**A1 第四态的用户可见那一半零判据**
审查者在 scratchpad 副本上实打：把 `caliber_round` 里 `Verdict::GraderError` 那支的
`st.note = Some(note)` 删掉，`-p dms-agent --lib` **91 条一条都不红**。
被钉住的只有 `judge()` 的枚举分支与 `log_kind()`，而第四态的全部价值在用户可见那一半：
① 答案上出现「未经校验」标注 ② `worth_learning` 因此否决 few-shot 沉淀（二·Q）。

修法：抽纯函数 `outcome(&Verdict) -> (Option<&str>, bool)`，`caliber_round` 只做接线；
判据打在纯函数上，并断言 **`Unresolved` 与 `GraderError` 的标注非空且互不相同**
（给用户同一句话就等于把「判过仍违规」与「压根没判成」混成一件事）。
反向验证：两态改成 `(None, false)` → 新判据红。

### AQ4 · 交叉审抓到的第三条：**C2 新造了一个失败面而只有一句提示词在拦**
把上一轮 SQL 喂进改写提示词是对的，但**改写结果一侧只有 system 里那句「不要输出 SQL」**。
返回值随即被当问句用在四处（选源 / 复合判定 / 向量召回 / precise 提示词的问题槽）——
模型抄一次 SQL 进问句，零报错零告警，症状是选源打偏、召回打偏、问句里多几百字噪音。
改动前提示词里根本没有 SQL 可抄，所以这是**新造出来的**失败模式。

修法：抽 `looks_like_sql`，**同一个判据两个极性** —— 上一轮素材是 SQL 才用，改写结果是 SQL 就丢。
两处各写一份的话改一处忘另一处不会红。判据三档（整条抄 / ```围栏 / 前缀+小写）+ 两条反面
（正常改写照用、`looks_like_sql` 对真 SQL 为真对真问句为假）。
刻意的漏判方向：只吐一个不带 SELECT 的 WHERE 片段判不出来 —— 收紧要付误伤真问句的代价
（含「从…中选」这类词），而误伤会静默丢掉上下文，与 二·G 同族取舍，宁漏不误伤。

### AQ5 · 交叉审抓到的第四条：**B4 把一个潜伏的方言漂放大成每次回炉 54 行**
`meta.dimension` 78 条 active 里 **68 条** expr 带 MySQL 反引号（autodiscover 按 MySQL 登记的
码→名 CASE）。回炉材料是**逐字**塞进提示词的 `{schema}` 槽，而
`dialect_and_quote_come_from_the_source_not_a_default` 只判 `build_system_prompt` 的输出 ——
那 ~33KB 材料一个字都没判。今天全部维度都是 `ds_id='dms'`（MySQL）所以没出事，
**那是巧合不是判据**：接第一个 PG 源的那天，提示词里会同时出现「用双引号」的指令和 68 条
反引号示例，而 LLM 照抄的是示例（本会话实测过一次「留一个反引号就照抄那一个」）。

修法：`repair_material` 收 `quote`（来自 `cx.source.dialect().quote()`），加 `requote`。
MySQL 源上是**恒等变换**，今天一个字节不改。归一放在**合并之后** —— 合并按原始 expr 逐字比，
先归一再合并会把「同名不同口径」的两条在 PG 源上误并成一条（那是静默丢一个口径）。
判据带两条防恒真：两份输出必须真的不同 + 输入里真的有反引号。
根子在登记侧（`register.rs` 该按源方言 quote），这里是渲染侧兜底；两处都做才算完。

### AQ6 · 交叉审抓到的第五条：**新判据会主动指令模型改错语义**
`known_value` 从 `cond_lits` 取值，而 `cond_lits` 同时收 `!=` / `NOT IN` / `LIKE`。
判词是「这个值匹配不到行，请换成合法取值之一」—— 这句话对 `!=` 是**反的**：
`col != '不存在的值'` 今天等于不过滤，照判词去改会把「不排除」变成「排除掉一个真实类别」。
那不是措辞不精确，是**判据自己指令了一次语义改写**，比原来的偏差更难发现。

修法：新增 `eq_lits`（只收 `=` 与 `IN`；`NOT IN` 取 `!negated` 排除），`known_value` 改读它。
`code_on_column` 那条判据要的是四种形态全收，所以不能就地收窄 `cond_lits`。
判词同时改准：「那个值一行都匹配不到 —— 只有这一个等值条件时就是 0 行」（对 `IN` 也成立）。
判据 +3 条不许判的形态（`!=` / `NOT IN` / `LIKE`）+ 2 条反面（换成等值家族必须判）——
没有反面时把 `known_value` 写成「一律不判」整个循环也全绿。

### AQ7 · 一条恒真判据（交叉审抓的）
`run.rs` 的 `assert_eq!(LITERALS.len() + 3, 9, "九个 kind…")` 是**常量表达式**，永远不可能红。
改成断言清单长度本身（`LITERALS.len() == 6`）。顺带把 kind 数的五处文档从「六/八」改成「九」
并补 `caliber-grader-error`（`ddl.rs` 的注释与 `correction_kinds_all_present` 是同一份契约的
两处渲染，注释漂了下一个人会按注释删 kind）。

### AQ8 · A3 今天是**休眠态**，不算进本轮收益
两位审查者独立核实同一件事：`origin` 的 DDL 默认值是最保守的 `seed`（这一点是对的、有断言钉），
但**三个写入点一个都没改** —— `register.rs` 两处 upsert 不写 `origin`，于是
`load_enum_values`（`WHERE origin = 'dict'`）恒 0 行、`enum_rules` 恒空、`RequireKnownValue`
一次都不会触发。运行库实测：`meta.value_map` 936 行 / 82 个 (表,列)，`origin` 列今天还不存在。

**别把「评测没变化」读成判据没用，也别读成安全。** 唤醒那一刻全部 dict 对码列会一次性同时开火。
唤醒的前置（写进下一步，不在本轮做）：① `register.rs` 两处 upsert 补 `origin`
（`DO UPDATE` 里也要带，否则重跑不纠正旧行）+ `register_domain_values` 写 PROBE；
② `enum_rules` 按**召回表**过滤（否则每问句规则数从个位数涨到几十条，且
`tracing::info!(detail = ?r)` 会把每条规则的全部 `(名,码)` 对按 Debug 打进 INFO ——
`company_code` 一列就在 13 张表上各 31 个取值）；③ 连库核
`SELECT origin, count(*) FROM meta.value_map GROUP BY 1`，**dict 为 0 就仍是休眠态**；
④ 打一道真题靶（在已知 dict 码列上写一个库里没有的中文值，看 `caliber_note` 是否出现
`require_known_value:表.列` 且判词列出了 `名=码`）。

### AQ9 · AQ 里点出的两条欠账，本轮**当场补掉**
- **C3 的 `series` 门禁**：`regression.py` 加 `chart_series` 键（进 `ASSERT_KEYS` + `check()` 真消费）。
  形态是 `if "chart_series" in c` 而**不是** `if c.get("chart_series")` —— 合法期望值含 **0**
  （第 0 列就是类别列）与 **None**（「必须没有 series」），两个都是 falsy，
  用 `c.get()` 会把这两档静默跳过，那正好把判据变成恒过。
  selfcheck 加 7 条（含「该有却没有」「不该有却有」「指到了别的列」三种红）。
  反向验证：改成 `c.get(...)` → `assert f == ["series=0≠None"]` 当场 `AssertionError: []`。
  ⚠️ **还欠一道真题**（「今年各月各品类销售额」）：要先连库看它的 route 与形状再钉，
  凭猜写一道会往门禁里塞一条假红。前端那一半（groupBy / x 轴去重 / 缺格 null 断点）
  `web/` 下没有测试栈，且 `notMerge: true` 顺手加到了**所有**图表上（含 pie 与单序列）——
  它修的是真问题（序列条数变少时残留旧线），代价是全站图表丢掉跨次更新动画，仍没人看图确认过。
- **B4 的接线判据**：照 `run::correction_kinds_all_present` 的形态用**源码**守
  （`include_str!` 切出 `gather_all_cards` 的函数体，断言两个 `load_*` 调用都在）。
  它挡不住有人把调用挪到别处，但挡得住最可能的那次退化：为了让某个测试变绿而换成默认值。
  两条防恒真：切出来的段必须 <2000 字符（否则是把整份源码当函数体，断言恒真）+ 必须含 `repair_material`。
  反向验证：`load_metrics(...)` 换成 `Default::default()` → 红。
- 顺带清掉三处死东西：`gather_schema` 转发（名字已经在撒谎，语义早不是「只有 schema」）、
  `BiChart.vue` 的 `labels()`/`series()`（`git show HEAD` 里就已无人调用，而 `series()`
  与新增的 `props.series` **同名**，读代码的人会以为多序列走的是它）、
  `direct.rs::compose_sql_with` 加 `#[cfg(test)]`（唯一调用者是同样 `cfg(test)` 的 `compose_sql`，
  不加就是每次 build 一条 `never used` —— 告警堆多了就没人看告警了）。
- `regression_cases.json` 补回被那一笔删掉的文件末尾换行。

### AQ10 · 实测
- 单测 **553 passed / 0 failed（20 个 target）**，架构门禁 15 项全绿。
- 五条新判据逐条反向验证过（`requote` / `outcome` / `looks_like_sql` / `eq_lits` / 品牌支同口径），
  一次并打四枪得到四条**各自对应**的红，不是一条红掩盖其余。
- E09：`llm 44586ms` → **`direct-agg 4531ms`**。

## 二·AR · 撤 `agg_template` 让路门：实测**撤不掉**，两处当场坏

裁决 二·AC5 曾写「两条阻塞理由都消了 ⇒ 可以撤门」。**那句是我写错的**：注释里列的两条
（指标 only 不出环比 / item_type 未裁决）确实都消了，但门实际在守的不止那两条。
把门**真的撤掉重建镜像跑一次**才看到，而不是数注释。

### AR1 · 门有**两道**，Router 走的是外面那道
`metric_only()` 里一道（`return MetricOnly::YieldToTemplate`）、`compose_hit()` 里一道
（`if agg_template(cx.question).is_some() { return None }`）。
我第一次只撤了里层，四题 SQL **逐字未变** —— 差点据此得出「撤门无影响」的结论。
外层那道才是 Router 路径上生效的（`direct-agg` 排在 `direct-doc` 之前）。
⚠️ 这本身就是一条教训：**「改了没变化」要先怀疑改的地方不对**，不要当成「改动安全」。

### AR2 · 第三条理由：**伪维度命中**（注释里记着，实测重现）
两道门全撤后，「本月成交客户数」的首格从 `1625` 变成 **`公司共用`**（一个客户名）——
`pick(dims)` 被「成交客户**数**」里的「客户」命中了维度「客户」，
残留守卫剥完指标名+维度名后正好为空，一路绿灯：

| | SQL |
|---|---|
| 门在 | `SELECT COUNT(DISTINCT customer_code) AS 成交客户数 FROM t_sales_order WHERE …` |
| 门撤 | `SELECT COALESCE(o.customer_name,'未知') AS 客户, COUNT(DISTINCT o.customer_code) AS 成交客户数 … GROUP BY … LIMIT 200` |

用户问「有多少客户」，拿到的是**按客户分组、每行 1** 的 200 行。
`route` 仍是 `direct-agg`、无报错、无告警 —— **只断言路由的题看不出来**（回归 A09/A12 正是只断言路由）。

### AR3 · 第四条理由：客单价**丢 ROUND**（这条注释里没记，是本轮新发现）
`agg_template` 写的是 `ROUND(SUM(total_amount)/NULLIF(COUNT(DISTINCT sales_order_code),0), 2)`，
而 `meta.metric` 的「客单价」声明 `agg_expr` **没有 ROUND**：

| | 值 |
|---|---|
| 门在 | `10222.77` |
| 门撤 | `10222.77212139` |

数没错，但用户看到的是一串小数。要撤门就得先把 ROUND 补进声明（或让装配器按 `unit` 加）。

### AR4 · 因此撤门的前置是**四条**，不是两条
① ~~环比~~ 已做 ② ~~item_type~~ 已裁决 ③ **修伪维度命中**（指标名已消化掉的字不许再命中维度 ——
形态与 `value_filters` 的子串门同族：`!words.iter().any(|w| w.contains(name) && question.contains(w))`）
④ **客单价声明补 ROUND**。三四两条都不难，但都得配自己的判据，且判据**不能只断言 route**
（本节两处坏 route 全是 `direct-agg`）。本轮不做 —— 再叠两个行为变更就没法把评测变化归因。

### AR5 · 顺带修掉一个量法错误（差点让整个对拍作废）
对拍脚本第一版从 `blocks[0].delta` 读环比，而真实路径是 `blocks[0].items[0].delta` ——
四题全返 `None`，于是「环比丢了」这一格**永远判不出来**，正好是让路门第二条理由要守的东西。
「断言的输入变空 ⇒ 恒过」，本会话第 N 次。
另一个：DMS 是**生产库且在写**，本月累计值实测一分钟涨 16 448 —— 前后两趟直接比 value
必然全判「变了」。改成「SQL 变了才同刻并排跑两条 SQL」，SQL 逐字相同即视为口径未变。

## 二·AS · 深度审计：一条**活的**静默错答 + 一批需调整/可删项

基线（干净测量，全程无编译干扰）：评测 **37/39 = 94.9%**（p50 21.0s / p95 65.9s）、
回归 **55/56**、单测 **555/0**、门禁 15 项全绿。

### AS1 · 🔴 BUG：伪维度命中 → 单指标问句被装配成分组查询（**已实证**）
```
✅ 本月成交客户数是多少    direct-agg  列=[成交客户数]        1 行   1625
✅ 本季度成交客户数是多少   llm         列=[成交客户数]        1 行   1625
❌ 上周成交客户数是多少    direct-agg  列=[客户, 成交客户数] 200 行  发员工福利样品使用
❌ 去年成交客户数是多少    direct-agg  列=[客户, 成交客户数] 200 行  线下-怀化市雪丰食品有限公司
```
用户问「有多少客户」，拿到 200 行客户名单。**零报错、零告警、`route` 正常、`caliber_note` 为空。**

**根因两层：**
1. `direct.rs::try_compose`（197-198 行）的 `pick(metrics)` 与 `pick(dims)` **各判一次、互不减词** ——
   「成交客户**数**」里的「客户」被再次当成维度命中。
   `value_filters` 早就有正确形态的子串门（`!words.iter().any(|w| w.contains(name) && question.contains(w))`），
   装配器这一侧没有。
2. 让路门盖不住全部：`agg_template` 有**自己的内联剥词表**（11 个时间词），
   而 `kernel::nl::lexicon::STRIP_WORDS`（单一事实源）有 18 个。
   **两份词表的差集精确地就是曝光面**：
   - 「上周」「去年」在 STRIP_WORDS 里 ⇒ 残留守卫剥得掉、**不拦**；
     不在 agg_template 剥词表里 ⇒ agg_template 返 None、**让路门开**。两条一凑就炸。
   - 「本季度」两边都没有 ⇒ 残留守卫拦下 ⇒ 回落 LLM ⇒ 答对。

**修法（两条都要，顺序无关）：**
- 主修（根治，覆盖全部调用方）：`pick(dims)` 减去指标已消化的词。
- 次修：`agg_template` 改用 `STRIP_WORDS`，别留第二份时间词表。⚠️ 改完要逐词核
  `time_predicate` 是否支持 —— 剥得掉但解析不了照样返 None。

**判据不能只断言 route**（本节两处坏 route 全是 `direct-agg`）。
应断言**列数**：单指标问句的结果只该有一列。探针在
`scratchpad/probe_pseudo_dim.py`（四句正反对照，多出分组列即非 0 退出）。

### AS2 · BUG：`review_all_pending` 返回的条数可以整体是假的
`agent/src/review.rs:91` 先 `let n = rows.len()`，再逐条 `review_exemplar`，
而 `review_exemplar` → `exemplar::set_status` 是 **`let _ =`（吞错）**。
PG 抖一下 → 一条都没更新，函数照样返回「处理了 N 条」。
兄弟函数 `set_lesson_status` 是 `?` 传播、且 `review_lessons` 在成功后才 `n += 1` ——
**同一份职责两种诚实度**。这条直接削弱 二·Q「few-shot 语料在投毒」的对策：
被判 NEGATIVE 的语料没被 disable，就继续当范例传播。
（HTTP 那条人工复核路径是对的：`admin_api::set_exemplar_status` 用 `affected()` 检查了 rows_affected。）

### AS3 · BUG：配置项打错名字**静默忽略**
`server/src/db.rs::Settings` 没有 `#[serde(deny_unknown_fields)]`。
`"mcp_key"` 少写一个 s → MCP 端点永久 404 且**零提示**（`mcp_keys` 为空即 404 是设计）。
`regression.py` 为完全相同的理由加了 `KNOWN` 白名单硬失败 —— 配置侧还欠这一刀。
⚠️ 代价：老 settings.json 里若有多余键会变成启动失败。要么接受，要么只对
「与已知键编辑距离 ≤2」的未知键告警。

### AS4 · BUG：最后一处静默 `unwrap_or_default`
`server/src/corrector.rs:478` `load_table_scopes(...).unwrap_or_default()` —— 无 `warn!`。
`direct.rs` 的 `reg_load!` 与 `gather.rs` 的 `map_err(warn)` 都是为这件事修的，这里是漏网的。
后果比那两处轻（校验器随后仍会判违规 → 回炉），但「补全器为什么没补」在日志里查不到。

### AS5 · 需调整（六条，都是文档/配置与代码不一致）
| 处 | 问题 |
|---|---|
| `settings.example.json` | 缺 `mcp_keys` 与 `insight_enabled` —— **MCP 整个功能对运维不可见**，止血阀也找不到 |
| `direct.rs` `let lim = detect_top_n(...)` 注释 | 写「默认 50」，实际 **200**（对齐 MAX_ROWS） |
| `datasource.rs:49/126`、`admin_api.rs:37` 的 `#[allow(dead_code)] // 消费者＝K4` | 消费者**已经存在**（`kb_api.rs:103/138`），注释在撒谎；且 `pub` 项本来不触发 dead_code，这三个 allow 是空操作 |
| 多角色登录 | `/api/roles` **零调用**；多角色用户只能手改 URL `?role=xxx`（`App.vue:173` 从 query 取）。安全行为是对的（fail-closed），缺的是选择器 |
| `gather.rs:44-57` 五处 recall 的 `unwrap_or_default()` | 无 warn。召回降级可接受，但与同文件 `gather_all_cards` 刚加的 warn 不一致 |
| `main.rs:850` `last_turn(...).ok().flatten()` | 取上一轮失败 → 追问静默丢上下文，无 warn |

### AS6 · 需增强（按价值排）
1. **时间词表收一份**（根治 AS1 第 2 层）。
2. **`/api/admin/exemplars*` 三个端点没有任何 UI** —— few-shot 投毒的对策（人工复核/剔除）
   只能 curl。这是 二·Q 那条风险的操作面缺口。
3. **`/api/ds*` 五个端点没有 UI** —— 多源接入全靠 curl，而「通用 agent 工具」的卖点就是多源。
4. **B10 仍 185s**（本轮 `llm+repair 185543ms`）。三条解法（补索引/预聚合/抬超时）都动生产或把
   等待推给用户 —— **业务裁决**。
5. A3 唤醒（见 二·AQ8）。
6. C3 的 `series` 已有门禁键但**还欠一道真题**；前端多序列无测试栈。

### AS7 · 可删（证据都在）
| 对象 | 证据 |
|---|---|
| `tools/e2e_m3.py` | 指向 `target/debug/dms-ai-server.exe` —— SAC 下**压根跑不了**，功能已被 `regression.py` 完全覆盖 |
| `tools/migrate_pitfalls.py` | 旧库 `dms_meta.skill_memory` → `meta.pitfall` 的**一次性**迁移，已完成 |
| `tools/merge_eval_cases.py` | 全仓**零引用** |
| `/api/stats`（`query_log::api_stats`） | 零消费者：无 UI、`tools/*.py` 无一处调它。且 `query_log` 没有 `conv_id`，统计维度本来就窄 |
| `recall/cards.rs:25 dim_hit` | 生产调用点在 T2 迁移时已消失（靠裁决 T7-3 留着）。留则把「为什么留」写进 `#[allow]` 的理由，别只写 allow |

**不建议删**：`/api/kb/ask`（`kb_eval.py:72` 在用）、`/api/mcp` 与 `/api/wework/login`
（对外集成/OAuth 回调，本就无前端 fetch）、`tools/probe_values.py`（32 行只读探针，无害）。

### AS8 · 审计里两次用错工具，都当场自查了
- 用 grep 数「settings 键有没有消费者」得到一片 0 —— 配置是 **serde 反序列化**的，
  字段名不以字符串字面量出现。差点报一堆「死配置」。
- 用 `basename x .vue` 去数 `.ts` 文件的引用 → `format.ts` 显示零引用，
  实际 `import { toNum } from './format'` 一直在用。
两次都是「量器不对，读数就没有意义」，与 二·AR 那次「改了没变化先怀疑改错了地方」同一族。

## 二·AT · 修完 二·AS 的全部发现：4 个 BUG + 6 项调整 + 删除 + A3 唤醒

单测 **563 绿 / 0 红 / 零警告**（20 target），门禁 **15 项全绿**，前端 `vue-tsc` + `vite build` 双绿，
回归 **55/56**（唯一红是 B10 业务阻塞的超时抖动）。

### AT1 · BUG1（伪维度命中）—— 两层都修，已端到端实证
```
修前：❌ 上周成交客户数是多少  direct-agg 列=[客户, 成交客户数] 200 行 首格=发员工福利样品使用
修后：✅ 上周成交客户数是多少  direct-agg 列=[成交客户数]        1 行  872
      ✅ 本季度成交客户数是多少 direct-agg 列=[成交客户数]        1 行  1627  ← 原来回落 LLM
```
- **主修**：新增 `pick_excluding(question, defs, of, taken)`，维度侧三个调用点
  （`try_compose` / `compose_verdict` / `metric_only`）传 `metric_word(question, m)` 减词。
  判据两条同时成立才算伪命中：维度命中词是指标命中词的**真子串** ∧ 该词在问句里只出现在指标词内部。
  形态与 `value_filters` 那条子串门同源。指标侧行为逐字不变（`pick` 是 `taken=""` 的薄壳）。
- **次修**：删掉 `agg_template` 的第二份内联时间词表，改用 `STRIP_WORDS`；
  配一道 `UNSUPPORTED` 门（最值/枚举/并列/单位词）保住「放宽词表不等于放宽语义」。
- **判据断列数不断 route** —— 二·AS1 两处坏答案的 route 全是 `direct-agg`。

### AT2 · 交叉审在「已修好」的改动里又抓到 6 条，全部已修
| 级别 | 什么 |
|---|---|
| blocker | B 造的死池用 `PgPoolOptions` 撞门禁「agent 不得造连接池」。搬进 connector（`dead_pg_pool_for_tests`）—— 池的构造集中在唯一允许造池的 crate，是那条纪律的**本意**，不是绕过 |
| high | `check-arch.ps1` 的 reqwest 那条**漏了注释过滤**（`Deny` 函数里有、那段自己写的管道没有）。我在 `mcp_api.rs` 写的一句注释「（sqlx/reqwest 的原文）」把整条门禁判红，**而假红把同一趟的真违规盖在下面** |
| high | A3 判据活着但**被判的代码是死的**：`probe.rs::manual_covered` 把 autodiscover 自己上一趟的产物当人工种子，82 个 (表,列) 全跳过 ⇒ `register_match` 一次不执行 ⇒ 仍休眠。两侧都修（`origin = $2` + `dim_code NOT LIKE 'auto\_%' ESCAPE`） |
| medium | `review_all_pending` 的 bug 站点零判据（判据全打在抽出来的 `count_reviewed` 上，改回 `Ok(rows.len())` 5 条全绿）。补源码守 |
| medium | `no_create_exemplar_route` 断言的是测试里自造的 `ROUTES` 字面量 —— 审查者实测往 `main.rs` 加一条真端点，143 条单测 **0 red**。改成 `include_str!("main.rs")` 反查 wire 路由表，并删掉文件头那句假话 |
| medium | direct.rs 注释写「新接住的五类**并带 KPI 环比**」—— 实测五句 `prev` 全 `None`（`prev_window` 认不得「上周/上半年/近三个月」）。改注释 + 加 `prev.is_none()` 断言 + 反面（`prev_window` 认得的那批必须有环比） |

### AT3 · 我自己的两次推断错误，都被自己加的判据当场抓住
- **死池快速失败**：我以为 127.0.0.1:1 的 ECONNREFUSED 会让 sqlx 立刻返错，
  于是想用不压超时的 `PgPool::connect_lazy` 省事。加的耗时断言当场报
  `finished in 60.01s` —— sqlx 会**一直重连到默认的 30s**。压到 200ms 后 **0.41s**。
  这条断言留在库里（`dead_pool_fails_fast_not_after_the_default_timeout`）：
  它守的是「有人给死池换了个会挂住的地址」，而那种退化只表现为慢，慢测试最后总会被 `#[ignore]`。
- **SQL 抽成 const**：为了让判据能断言，我把 `manual_covered` 的两条 SQL 抽成 const ——
  `drift.rs` 的「每条 `meta.*` 读必须带 ds 限定」当场判红（它按**行窗口**扫源码，
  SQL 与 `{ds_pred}` 拆到两处就抓）。改成函数（拼好整条再返，同 `visible_datasources_sql()`）。

### AT4 · kernel 词表两条真缺陷（`STRIP_WORDS`，影响**全仓**残留守卫）
| 症状 | 根因 |
|---|---|
| 「最近三个月的销量」静默回落 LLM，而「近三个月的销量」好的 | 「近」(idx 15) 排在「最近」(idx 16) **之前**，长词先剥那条纪律在这一对上破了 ⇒ 剥完剩一个孤零零的「最」 |
| 「本季度/上季度…」静默回落 LLM | 表里只有「季度」没有「本季度」⇒ 剥完剩一个「本」 |

两条都不报错、只表现为「这句话走了 LLM」。修法：调词序 + 补三个季度词（`len` 77→80）。
判据两层：`strip_words_long_before_substring` 补下标断言 +
新增 `the_two_fixed_families_strip_clean`（模拟剥词、断言**结果为空**）——
只判下标不判结果的话，换一处实现照样能漂。

### AT5 · 删除与清理
删 `tools/e2e_m3.py`（指向 `target/debug/*.exe`，SAC 下跑不了）、`tools/migrate_pitfalls.py`
（一次性迁移已完成）、`tools/merge_eval_cases.py`（零引用）、`/api/stats`（零消费者）。
三处假的 `#[allow(dead_code)] // 消费者＝K4`（消费者已存在于 `kb_api.rs:103/138`，
且 `pub` 项本不触发该警告 = 空操作）。`direct.rs` 一条**重复的 `#[test]`**——
rustc 之前把同一函数登记了两次（143→142 不是丢测试，是不再重复登记）。

**仍不删** `recall/cards.rs::dim_hit`：裁决 T7-3 明令保留它**与它的搬运断言**，
删函数会连带删掉那条搬运证据。改注释比删更对。

### AT6 · 配置硬失败的代价：已逐键验证，无风险
`Settings` 加 `#[serde(deny_unknown_fields)]`（业主裁决走启动硬失败，与 `regression.py`
已选的口径一致）。审查者逐键比对三份配置：`settings.json` 12 键 /
`settings.docker.json` 16 键（`serve.ps1` 挂进容器的正是这份）/ `settings.example.json` 19 键，
**未知键全为 0**，必填只有 `mysql_url` 与 `pg_url`。本轮 `serve.ps1 -Build` 已实证起得来。
风险只对仓库外的第四份配置成立。

### AT7 · 修复后的测量：分数不动，**收益不许记成 +1**
评测 **37/39 = 94.9%**（p50 19.6s / p95 40.2s），与修复前基线**逐题一致**。
两条红都是原有的：AS01（抖动）、SALE15（等商品维度口径裁决）。

**收益老实记账**：
- **题集分数 +0** —— 题集里本来没有一道题覆盖那个 bug，那正是它能活到今天的原因。
- 真实收益是「修掉一个活的错答」+「四族问句（上周/去年/本季度/最近N月）从 LLM 路
  转确定性路径」，后者**没有题覆盖**，所以在分数上看不见。
- `p95 65.9s → 40.2s` **不算收益**：两趟最慢的都是 AS01（65.9s → 45.7s），
  差的 20s 是同题 LLM 抖动。p50 同样在抖动带内。

### AT8 · 差点把抖动读成收益（自查记录）+ 一个新发现
逐题对比 route 时发现 **GOODS13 `llm` → `direct-agg`**，一度想记成修复的收益。
查历史六趟：**4 趟 `direct-agg`（16–23s）、1 趟 `llm 112s`、1 趟 `llm+repair 78s`**
—— 它是抖动题，上一趟那次 112s 才是异常值。归因不成立。
（同一个陷阱本会话踩过一次：B10 的「偶尔红」曾被当 LLM 抖动记账，实际是超时静默降级。）

🔴 **新发现（值得单列）**：**超时静默降级影响的题不止 B10 一道**。
GOODS13 的确定性 SQL 本身 16–23s，正好压在预算边界上；B10 是 ~28s。
降级是**静默**的 —— route 从 `direct-agg` 变 `llm`，耗时从 20s 变 112s（5.5×），
答案这次仍对，但那是运气：LLM 路是全部失败的来源（确定性路至今 0 失败）。
也就是说 B10 那条「超时怎么处置」的业务裁决**不是一道题的事**，
而是「凡确定性 SQL 逼近预算的题都会随机掉进失败集」。
建档建议：给降级加一行 `tracing::warn!`（今天连日志都没有），
并在题集 `note` 里把 GOODS13 也标成同族，免得下一个人又把它的 route 变化当成某次改动的效果。

## 二·AW · 从 DMS 后端源码提取真实 JOIN：拿到 72 条边，但**一条都还不能上**

业主要求「通过后端代码提高问数准确性 / 了解每张表的用途与关联关系 / 完善图数据库」。
本节是第一轮结果：**提取完成、收益量化完成、但前置未过，边全部押后**。

### AW1 · 先弄清「图」指的是哪个（两个是不同的东西）
| | 内容 | 用途 |
|---|---|---|
| `meta.join_edge` | **5 条边**（对 251 张已登记表） | 装配器 `find_path` 找「指标基表 → 维度驱动表」的路 |
| `dms_graph`（AGE） | `Customer` 2606 + `Goods` 455 顶点、`BOUGHT` **100336** 边 | 3 类图问句（买过X的客户 / X买过什么 / 买X的还买什么） |

业主那句「关联关系」指前者、「图数据库」指后者。本轮只动前者的**候选清单**（一条都没落地）。

### AW2 · 提取：182 个 Mapper.xml → 72 条候选边
`scratchpad/extract_joins.py`。扫 `xh-dms` 的 `*Mapper.xml`，抹平 MyBatis 标签与 `${}`，
按别名映射把 `ON a.x = b.y` 还原成 (表,列)=(表,列)。

🔴 **必须排掉 `target/`**（Maven 编译副本，与 `src/main` 逐字相同）。不排的话每条边被计两次 ——
第一版扫到 366 个文件，置信度分布是 `{2:55, 4:11, 6:5, 10:1}`，**全是偶数**就是这个 bug 的指纹。
修后：182 个真实 Mapper、120 张业务表、72 条边，分布 `{1:55, 2:11, 3:5, 5:1}`。

**70/72 条边两端都已在 `meta.table_doc` 里**。与现有 5 条的重叠只有 3 条 ——
`owner_manager → t_employee` 与 `goods_category_code → t_goods_category` 不在提取结果里，
说明 DMS 在 Java 层做那两个关联。**两者互补，不是替代。**

### AW3 · cardinality 不是猜的
`scratchpad/infer_card.py`：查生产库 `information_schema.STATISTICS` 的 `NON_UNIQUE=0`
索引，**只收单列唯一索引**（`(a,b)` 联合唯一不代表 `a` 唯一）。
方向规范化成「唯一的那一侧在右」⇒ card 恒为 `N:1`。

结果：**N:1 56 条 / N:N 16 条**。N:N 一律**不该进图**（BFS 走上去必然扇出且无法去重），
其中两条是**脏关联**，值得单独记：
- `t_customer_contacts_info.contact_name = t_employee.actual_name` —— **按姓名 JOIN**
- `t_customer_price.goods_name = t_goods.goods_name` —— 按商品名 JOIN

### AW4 · 🔴 量器换了之后结论翻转：**置信度 ≠ 收益**
不用「边数涨了多少」当量器，用**「指标 × 维度」可达数**（`scratchpad/reach_gain.py`，
BFS 口径照 `find_path`：≤3 跳、扇出边只 `COUNT(DISTINCT)` 能过）：

| 边集 | 不扇出可达 | 允许扇出 |
|---|---|---|
| 现有 5 条 | 70/180（39%） | 70/180 |
| + 出现在 ≥2 个 Mapper 的 12 条 | **70/180（零收益）** | 70/180 |
| + 全部 56 条 N:1 | 100/180（56%） | 130/180（72%） |
| 退化（本来能走现在不能） | **0** | — |

**我原本打算「保守只写高置信度那批」—— 那会得到 0 收益。** 高频边（销售订单那一族）我们
早就有了；真正补上缺口的是一条**只出现 1 次**的边。贪心求最小边集：**1 条边接通全部 30 个组合**
（售后表接上订单后就经已有 4 条边通到客户/员工/商品/分类），第二条 `owner_manager → t_employee`
收益因此为 0，冗余。

净收益 **20** 个组合（30 减 10 —— 「退款占比」的 `agg_expr` 含子查询，按设计永不进装配）。

### AW5 · 🔴 第一层拦路：可达 ≠ 口径正确（时间维度）
「月份」维度 expr = `DATE_FORMAT(o.order_time,'%Y-%m')`（别名 `o` 绑 `t_sales_order`），
而「退款额」的 `time_col` = `after_sales_time`。加边后
「今年每个月的售后退款金额」会装配成**按订单时间分月** —— 一笔 1 月下单 3 月退款的单算进 1 月。

这是**已存在**的缺陷，只是被「找不到路」掩盖着：今天「月份」只服务 `t_sales_order` 系
四个指标（time_col 全是 `order_time`）✅。

**当前修法（AX96 替代早期拒绝策略）**：当维度是通用时间维度、指标自身声明了
`time_col` 时，装配器把维度表达式中的首个 `alias.column` 绑定到指标表与指标时间列。
例如「今年每个月的售后退款金额」确定性生成
`DATE_FORMAT(b0.after_sales_time, '%Y-%m')`，不再按订单时间分月，也不为这个可证明安全的
请求回落 LLM。只有指标没有时间列、无法证明绑定正确时，跨表时间维度才继续拒绝。

判据同时覆盖正反两面：售后退款月趋势必须只引用 `t_after_sales_order_header` 与
`after_sales_time`；非时间表达式、复合业务表达式不能被该辅助函数误改写。

### AW6 · 🔴 第二层拦路：装配出来的 SQL **确定性少算**（这才是押后的理由）
实测「今年各省份的售后单数」：

| | 值 |
|---|---|
| 权威（不 JOIN，`t_after_sales_order_header` 直接数） | **20 073** |
| 装配器出的那条 | **20 060**（少 13 单，**−0.065%**） |
| 正确形态（LEFT JOIN 全程 + 口径进 ON） | **20 073** ✅ |

两个根因，**必须一起修**（只改一条会被另一条抵消）：
- **① 该 LEFT 却 INNER**：`compose_sql_with_snap` 拼的是
  `from.push_str(" JOIN {to} {alias} ON …")` + 表级口径进 WHERE
  ⇒ 原订单已作废/软删的售后单被整行丢掉（那 13 单）。
- **② LEFT 退化**：维度声明里的 `LEFT JOIN t_customer cus ON … AND cus.deleted_flag = 0`
  在 `scope_parts` 循环里**又被加了一次到 WHERE** ⇒ LEFT 变 INNER。
  ⚠️ 这一条**今天就在现役路径上**：「今年各省份的销售额」同形（`cus.deleted_flag` 出现 2 次）。
  实测今天影响为 **0** —— 但那是因为今年软删客户下过 **0 单**，**碰巧，不是靠代码**。

**决定：撤边**。代价 0（那 20 个组合今天本来走 LLM），而不撤就是明知有确定性少算还上线 ——
与 AW5 刚定的「宁可回落 LLM 也不出确定性错数」是同一条原则，不因 0.065% 就破例。
前置写进了 `seed.rs` 那段注释（含「改完必须逐题对拍数字」的警告：`明细 → t_goods` 那条边上
若有 sku 在商品主档找不到，INNER 丢行而 LEFT 保留成「未分类」——**那会改数，不是重构**）。

### AW7 · 一个省掉大量工作的负面结论
`meta.column_doc` 列注释覆盖率 **98.1%**（5501/5607）、`table_doc` 表注释 **99.2%**。
所以从 Java 实体的 `@Schema(description=…)` 补注释**收益极小，不做**。
DMS 源码的价值集中在**关联关系**与**业务口径**，不在注释。

### AW8 · 本轮的资产（边虽押后，这些都留下）
- `scratchpad/extract_joins.py` —— 72 条边的提取器（含 `target/` 那个坑的注释）
- `scratchpad/infer_card.py` —— cardinality 推断（唯一索引，不猜）
- `scratchpad/reach_gain.py` —— **可达数量器**（那是把「加边」从灌数据变成可量化的东西）
- `dms_joins.json` / `dms_edges_carded.json` —— 候选清单，含 16 条 N:N 的黑名单
- 跨表时间维度门（预防性，已上线，今天零开火）
- 「LEFT 退化今天碰巧无害」这条记录 —— 下次有软删客户下单就会错

## 二·AX · 用 DMS 代码枚举当权威：修掉一个活错答，并找到一个系统性缺口

业主要求「深度参考源码，让系统更智能更准确」。本节的价值不在修了几行，
而在**找到了一整族错答的根**并留下了可重跑的对拍机制。

### AX1 · 🔴 活错答：口径卡把码值写成了中文名
「活动费用」的 description 原本写着 `status IN(<两个中文状态名>)`，而
`t_activity_main.status` 存的是**码**（DMS 后端 `ActivityStatusEnum`：`'4'`=已申请、`'8'`=完成）。

三数交叉验证：

| | 值 | 相对权威 |
|---|---|---|
| 全部状态（**装配器答的** —— `scope_filter` 里没有状态过滤） | 2 283 485.51 | **+108%** |
| `status IN ('4','8')`（权威） | 1 097 948.76 | 基准 |
| 中文名那版（**LLM 照口径卡抄的**） | **0.00** | **−100%** |

**同一个问句两条路都错、方向相反、差一倍以上。** 装配器读 `scope_filter`、LLM 读 `description`，
两处口径不一致而没有任何判据在对拍。

### AX2 · 业主裁决：**不替用户挑状态，把状态暴露出来**
不在 `scope_filter` 里写死某几个状态（11 个状态里哪些算「生效」随场景变 ——
月度费用核销与预算占用口径不同）。改为三件：
1. 新增**「活动状态」维度**（码→名 CASE，与 `ActivityStatusEnum` 逐条对齐，
   含源码里那个**被注释掉**的 `'5'`(下发中) —— 库里真出现时宁可显示「下发中(源码已注释)」也不归'未知'）
2. 把 10 个状态灌进 `meta.value_map`（`t_activity_main.status`）
3. `scope_filter` **不动**

实测三个问句**全部走 `direct-agg`（零 LLM）**：

| 问句 | 结果 |
|---|---|
| 今年各活动状态的活动费用 | 5 行分组：完成 900 175.92 / 待申请 705 170.88 / 部分申请 240 590.87 / 暂存 225 567.21 / 已申请 212 580.64 |
| 今年已申请的活动费用 | 212 580.64，SQL 是 `status = '4'` ← **换码器认出「已申请」了** |
| 今年活动费用 | 2 284 085.51（全部状态，不变） |

### AX3 · 🔴 系统性缺口（本节最有价值的发现）
`meta.value_map` 的 946 行**全部**来自 autodiscover 读**生产字典表**（`t_dict_key`/`t_dict_value`）。
而 DMS 有 **102 个枚举类**，其中 **60 个含中文码表、310 个 (码,名) 对**
（外加 **1 个被注释掉**的）—— 凡是码**只写在代码里**的列，value_map 里就没有。

实测坐实：`t_activity_main` 的 value_map 只有 `company_code`(31) 与 `execute_type`(3)，**没有 status**。
后果统一是：换码器不认中文名 ⇒ LLM 猜 ⇒ 写中文名 ⇒ **返 0 行**。

对拍结果（`scratchpad/enum_vs_valuemap.py`）：
- 按列归属成功 **20** 个 (表,列)
- **缺口 32 条**（已归属列里枚举有、库里没有）—— 如 `invoice_status` 缺「审核通过=8/待确认=7/暂存=10/部分开票=6/驳回=9」，三张对账表各缺一遍
- **未归属的枚举类 46/60** —— 它们的码在现役 value_map 里找不到对应列

### AX4 · 判据设计的三个坑，全是实测踩出来的
- **判据太宽 ⇒ 假报淹掉真报**：`audit_code_values.py` 第一版扫「所有中文字面量」→ **11 条全假**
  （`COALESCE(x,'未知')` 是兜底**输出**、`CASE WHEN '01' THEN '货架店铺'` 是码→名映射，
  两者都**正确**）。收窄到「比较运算符右侧」后：硬错 0、真错 2。
- **守卫自伤 + 漏判是同一件事的两面**：`code_filters_never_use_chinese_names` 只认 `IN (`（带空格），
  而我自己写的警告文案正好是 `IN(`（无空格）—— **侥幸没自伤，也就意味着对真 bug 同样会漏**。
  两处一起修：判据认两种形态、文案不再写出带引号的中文名。
- **归属靠名字重叠率会归错字典**：`t_shop_shipment_order.shipment_status` 的「配送中」
  库里 `200`、`OrderStatusEnum` 里 `300` —— 我的 0.6 覆盖率门槛让它成了「码不一致」的报警。
  **自己查生产库核实：实际取值就是 `'200'`(1684 行) 与 `'700'`(551 行)，登记是对的**，
  `OrderStatusEnum` 是**另一张表**的订单状态枚举。**假报**。

### AX5 · 由 AX4 第三条推出的更强判据（不依赖归属）
既然「归属」本身就会错，那就换一个不需要归属的判据：
**登记的码在不在生产数据里**。码不在该列的实际 `DISTINCT` 取值里
⇒ 换码器把那个中文名换过去 ⇒ SQL 合法、执行成功、**返 0 行**。
不是「可能不对」而是「一定错」—— 与 `RequireKnownValue` 同一条道理，方向相反
（那条判 SQL 里的值不在码表，这条判码表里的码不在数据里）。

脚本 `scratchpad/valuemap_vs_prod.py`。两条防假红的纪律写进了实现：
- 查不到实际取值（表不存在/超时）→ **不判**，不许当成空集（空集会让该列全部登记码变假红）
- 空表 → 同样不判
- 表名/列名先过标识符白名单（照 `probe.rs::ident` 的理由：上传表头是不可信输入）

### AX5a · 🔴 那个「更强判据」也是假报 —— 而且假报 468 条
`valuemap_vs_prod.py` 实跑：**83/83 列都取到了实际取值**（零个「取不到」），
报出 **468 条「登记的码不在生产数据里」**。逐类核完：**几乎全是假报**。

| 类别 | 条数 | 为什么不是错 |
|---|---|---|
| `company_code` | ≈344 | `t_activity_main.company_code` 实际只有 **4** 个取值，而 value_map 登记了 **31** 个公司 —— 公司主档就是 31 个，某张业务表只用到其中几个。「安徽虎家=1252」这个映射**本身对**，只是那张表没有安徽的数据。用户问「安徽虎家的活动费用」→ 0 行 = **正确答案** |
| 状态码 | ≈117 | `t_activity_main.status` 实际 5 个取值、登记 10 个（枚举定义 10 个而库里目前只出现 5 个）。问「申请中的活动费用」→ 0 行 = **正确**（目前确实没有申请中的活动） |
| `category_name` | 7 | 名称型值域（`name=code`），实际 61 个取值里有 7 个登记值不在 —— 可能是被删/改名的分类。**唯一值得单独看的一类**，但也不是「错码」 |

**判据错在哪**：「登记的码不在数据里」**≠ 错码**，那只是**稀疏**。
一张业务表用不到某个公司/某个状态是常态，而码表本来就该覆盖全部合法取值
（`RequireKnownValue` 判据要的正是完整枚举 —— 码表比数据宽是**对的**）。

真正的错码判据是「与权威码表不一致」—— 而那需要**归属**，于是又回到 AX4 第三条的问题。
**同一轮里第二次「判据太宽淹掉真报」。** 两次的形态一样：
挑了一个容易算的量（字面量出现 / 码不在数据里）当判据，而它与「错」之间不是充要关系。

### AX5b · 两轮判据都没找到现役错码 —— 那是好消息，要照实说
- 枚举归属对拍：1 条报警，**自己查库核实是假报**（归错字典）
- 生产数据存在性对拍：468 条报警，**逐类核完几乎全假**（稀疏 ≠ 错）
⇒ **现役 `meta.value_map` 的 946 行里没有找到错码**。
这不是「没查出来」，是两个不同方向的判据都指向同一个结论：那批码值质量高。
（它们全部来自 autodiscover 读生产字典表 + 手写种子，两条来源本身就比 LLM 猜可靠。）

**别把 468 当成 468 个 bug** —— 下次看到那个数的人要先读本节。

### AX6 · 同名不同码是真实存在的（修这一族的硬约束）
`t_customer.customer_class` 的 `'01'` 与 `CustomerTypeEnum` 的 `'Z004'` **都叫「货架店铺」**。
所以修码值问题**必须按列判归属，不能照任何清单一刀换**。

### AX7 · 状态
- 单测 **608 绿 / 0 红**，门禁 15 项 + Deny 13/13 全绿。
- 新判据 `code_filters_never_use_chinese_names`（源码扫描，两种 `IN` 形态各枪测一次）。
- **未做**：把 46 个未归属枚举系统性接进 autodiscover。可靠做法是**数据驱动对拍**
  （枚举的码集合 vs 列的实际取值，`cov == 1.0` ⇒ `origin=dict` 判据可开火；
  `0.8~1.0` ⇒ `probe` 不开火；`< 0.8` ⇒ 不归属），**不许靠命名猜** —— AX4 第三条就是命名猜的代价。
  ⚠️ 标 `dict` 会让 `RequireKnownValue` **第一次真的开火**（本会话它一直休眠，`origin='dict'` 行数为 0），
  那是行为变更，需要可回退的开关 + 逐题对拍。

### AX8 · 反问的**位置**：在 LLM 入口，不在 `ask()` 开头
业主报的准确度问题（「聊天框发一个客户名称，回答完全错误」）修法是「意图不足就反问，绝不猜」。
判据本身三条门（召回不到指标 / 有残留词 / 无疑问词）逻辑没问题，但**第一版放错了位置** ——
放在 `ask()` 的 Router 遍历**之前**，一次回归当场跑出 5 个红：

| 题 | 原路由 | 被拦成 |
|---|---|---|
| `C01-单号直查`「帮我查下 HJXH-DXO2026072300384」 | `direct-doc` | `need-intent` |
| `F01-图-买过烤肠的客户` | `graph` | `need-intent` |
| `H01/H02/H03` 红线题（删除/清空/drop） | `llm` + 闸门拦 | `need-intent`，**红线闸门失去输入** |

三类问句的共同点：**都不含疑问词**，所以第三条门放不过它们；而它们**本来都有确定性路径接**。
正确语义是「**所有确定性路径都不接、LLM 只能猜**时才反问」——
那个边界就是 `run::run_llm` 的入口，一个字都不用多判（走到那里 = Router 前四位全部弃权）。

**这类错误单元测试抓不到**（函数自己的逻辑是对的，位置错了），
所以补了源码扫描判据 `ask_back_is_wired_at_the_llm_entry_not_before_the_router`：
① `ask.rs` 里不许有调用；② `run.rs` 里必须有，且在第一次 `run_once` **之前**（否则已付一次 LLM 调用）。
判据写完自己就红了一次 —— 它引用那个函数名的那行**也在 `ask.rs` 里**，
所以过滤条件加了「带引号的行不算调用」。枪测：把调用塞回 `ask()` ⇒ 判据红。

### AX9 · 量器指错了对象：本机 exe 过期，跑出一整套假数字
移完反问位置后重跑回归：**47 通过 / 9 失败**，9 个失败清一色 `direct-agg` 一族
（A09/A12/B02/B03 「SQL≠金文件」，B06/E09/E16/E17/SALE17 掉到 `llm`）。
金文件 diff 读起来完全像真回归 —— A09 本该出单值 KPI，实际多了
`COALESCE(o.customer_name,'未知') AS 客户` + `GROUP BY`，正是「伪维度」那个老 bug 的形状。

先按代码找根因，一路都对不上：`why-not-compose "本月成交客户数"` 说「⓿ 让路给 agg_template」
（维度**没**命中），`pick_excluding` 的两条判据逐字算下来也该减掉「客户」，
生产只有两处调 `compose_gated` 且都走 `pick_excluding`。**诊断与实测矛盾**。

破绽是：同一个问句走 `docker exec … ask admin "本月成交客户数"` 出的是**金文件那条 SQL**。
于是查量器自己 —— `DMSAI_CLI` 是**空的**，`cli()` 静默回落本机
`target/debug/dms-ai-server.exe`，时间戳 **07-28 08:17**，两天前。
Smart App Control 强制态拦着它重新链接（`os error 4551`），所以它**永远停在那一天，
而且照样能跑**。9 个失败全是两天前的旧行为。

**「能跑但是旧的」比起不来坏得多**：起不来是响亮失败，这个是假数字，
而且伪装得很好（route 正常、耗时 245ms、金文件 diff 指向一个真实存在过的 bug）。
差一点就去「修」一个已经修好的东西。

护栏（`tools/cli.py::stale_exe`，一处修、三个工具全受益）：
`DMSAI_CLI` 没设时比对 exe mtime 与 `crates/**/*.rs` 的最新 mtime，过期就 **`SystemExit`**。
不用 warn —— warn 会被 `Select-Object -Last N` 截掉，这次就是那么丢的。
自检里连「真仓上此刻 exe 就是过期的」一起断言了。

⇒ **测量纪律加第三条**：不只「不许边编译边测」「先验量器状态」，还有
**「先验量器测的是哪个二进制」**。前两条都做了，这条没有，于是照样丢了一整跑。

### AX10 · 图库完善：一个功能缺口，三个连环坑，最后一个是静默错答
起点是实测的功能缺口（不是设计洁癖）：
```text
买过烤肠的客户        → graph 50 行 ✅
湖南省买过烤肠的客户  → 剥词剩「湖南省烤肠」当商品名查 → 0 行 → 回落 → 反问
买过肉制品的客户      → 「肉制品」是分类名、不在任何商品名里 → 0 行 → 反问
```
剥词没错，错在**剥完之后没人问过「剩下的这坨是什么」**。
落地：图加 `Goods-[:IN_CATEGORY]->Category` / `Customer-[:IN_REGION]->Region`
（Category 66 / Region 31 / 维度边 2423），再加「残留 → 实体解析 → 带限定 Cypher」。

**三个坑，按发现顺序：**

**① Region 节点名是行政区划编码。** `t_customer.province` 存的是 `430000` 不是「湖南省」，
于是 Region 节点名全是数字，用户说「湖南省」永远零命中。
而首次同步的日志是 `categories=66 regions=33` —— **看起来完全正常**。
修：JOIN `t_regions`（`region_level='1'` 是省级）拿中文名。
顺带说明为什么 AX12 那条表注释必须修：`t_regions` 原来在 prompt 里自称「开票申请单」。

**② 覆盖率判据按「实体名」算 —— 被绕过，造成静默错答。** 判据本意是
「解析不全就别装配」，实现却是 `covered = Σ 实体名的汉字数`。
「湖南省烤肠」里窗口「烤肠」模糊匹到 `皇家小虎黑猪肉烤肠（原味）0500G00`，
按实体名算 covered=13 ≥ 5 ⇒ **判据放行、「湖南省」整个丢掉**，用户拿到**全国** 27 个客户。
日志原文：`goods=Some("皇家小虎黑猪肉烤肠（原味）0500G00") region=None rows=27`。
route 还是 `graph`、行数看着很正常、零报错 —— 这正是最坏的一档。
修：`Hit` 带上 `window`（原文里被吃掉的那几个字），covered 按**窗口**算。
**这是「判据太宽淹掉真报」的第五次**，形状与前四次一致：挑了个容易算的量当判据，
而它与「解析全了」之间不是充要关系。

**③ 开集实体不许存「图里匹到的那条名字」。** 修好②之后仍然错：
`goods=Some("合马双层烤肠机")` —— 「烤肠」被 `LIMIT 1` 取到了一台**机器**，
「买过烤肠的客户」悄悄变成「买过那台烤肠机的客户」，行数 50 → 1。
而取到哪一条取决于图的物理行序 ⇒ **同一个问句在不同部署上答案不同**。
根因是把两件事混了：解析对闭集（Region/Category）是**定位到精确名**，
对开集（Goods）只是**确认这个词在商品名里出现过**，值必须保留用户原词再模糊匹配。

**修完的实测**（逐个核对省份）：
| 问句 | 行数 | 样本 |
|---|---|---|
| 买过烤肠的客户 | 50 | 长沙鸣望 / 长沙富楚 |
| 湖南省买过烤肠的客户 | 50 | 长沙鸣望 / 零食很忙 |
| 广东省买过烤肠的客户 | 50 | 广东横琴雨燕 / 惠州市兴勤 |
| 山西省买过烤肠的客户 | **38** | 山西福晋园 / 山西鸿财润 |

判据分工：过滤正确性归单测（`into_slots` **逐字重演了那次错答**、
`cypher_carries_every_filter` 钉「每个限定词都进 Cypher 且闭集用 `=` 不用 `=~``）；
端到端只钉「整条路径不掉线」（F05/F06）—— 端到端判不出「省份过滤对不对」，
硬要它判就会写出「返回的客户名里含『山西』」这种弱判据。

### AX11 · 换千问：几乎零代码，但有一个必须能配的私有参数
`LlmClient` 本来就是干净的 OpenAI 兼容（base_url + key + fast/precise 两档），
所以换千问（dashscope compatible-mode）**只改 settings.json**。唯一的例外是
`enable_thinking` —— 千问默认带思考，实测同一道带口径卡的 SQL 题：

| | 延迟 | completion tokens | SQL 判据 |
|---|---|---|---|
| `enable_thinking: false` | **780ms** | **65** | 4/4 |
| 默认（带思考） | 16626ms | 2281 | 4/4 |

产出**一样**，也就是说不关它就是白付 21 倍延迟、35 倍 token。
但这个键是千问私有的，写死进 body 会带到 DeepSeek 上去。
⇒ 做成 `settings.llm_extra_body`（任意 JSON 合并进请求体），换供应商零代码。

两条守卫：① `messages`/`model` 出现在里面就 **panic** ——
能覆盖 `messages` 等于配置文件可做任意提示注入，能覆盖 `model` 等于 fast/precise 两档形同虚设，
两者都不报错、只静默改行为；② 空 `extra` 时请求体与本字段引入前**完全相同**（DeepSeek 侧零变更）。
第②条第一版写成「逐字节比对手抄的序列化串」，当场红：`serde_json::Map` 默认 `BTreeMap`、
输出按字典序。钉字节序 = 把判据绑在 serde 的实现细节上，而 HTTP body 的键序本就无意义
⇒ 改成钉**键集合 + 逐字段值**（多键/少键/改值三种都红，serde 换排序不误伤）。

**千问实测的能力清单**（key 只走环境变量，未落盘）：
- `qwen3.7-flash` **自己就吃图**：988ms，客户名/单号/金额 3/3 全对
- `qwen3.5-ocr`（专用 OCR）反而把 `DXO` 读成 `DX02` ⇒ 企微拍照走 flash，**且单号必须回库核验**
- tool calling 干净：`{metric:销售额, dimension:省份, time_range:本月}` 参数全对
- `qwen3.7-text-embedding` 支持 `dimensions: 512` = 与本地 bge-small-zh 同维
  ⇒ **pgvector 列和 HNSW 索引都不用动**，可以直接双跑对拍

### AX12 · `meta.table_doc` 里有张冠李戴的表注释（直接进 prompt）
判据是「**同一条 comment 被多张不同族的表共用**」，不是我觉得哪条不对：
「开票申请单」×7、「活动场地费用表」×4。共用本身不必然错
（`t_erp_invoice_header`/`_detail` 同族、`t_device_demand_*` 与其 `_3` 分表都是对的），
所以「族」判据 = 表名**前两段**相同，对分表和主从表天然免疫。

| 表 | 库里写的 | 独立证据 |
|---|---|---|
| `t_regions`（4715 行省市区） | 开票申请单 | `Regions.java` javadoc = 行政区域 |
| `t_xh_bom_detail` | 开票申请单 | 列自证 `bom_code/sku_code/quantity/share_ratio` |
| `t_delivery_warehouse_address` | 活动场地费用表 | javadoc = 地址对应发货仓库 |
| `t_delivery_warehouse_stock` | 活动场地费用表 | javadoc = 地址对应发货仓库存 |

**源码不是权威**：`t_interface_log`（接口日志）的类注释写的是「商品分类数据对象」——
同样是复制粘贴。所以每条修正都记独立证据，而不是「源码这么说」。
反过来也验证了这条纪律的价值：如果无条件采纳源码，会把接口日志表标成商品分类。

顺带修了提取器自己的两个坑（都会造成 20/20 全假报）：
① `@ApiModel` 是 `@ApiModelProperty` 的**前缀**，没词界就匹到第一个字段；
② DMS 用的是 `@Schema`，而它**类上和字段上是同一个注解** ——
只能切到 `public class` 之前那段里找，那样天然排除所有字段。
③ `psql` 按行 split 也踩了：`table_comment` 里**有换行**，一条记录打成多行就整体错位
（把 `t_delivery_warehouse_address` 的用途读成「活动场地费用表」，看起来像大新闻）。
换 `-F $''` 只解决「值里含 `|`」，`json_agg` 才两个都解决。

### AX13 · 三框架整合第一批：向量路原来是哑的
按用户选定的四批（地基/智能/知识库/体验）执行，地基批已落地。**最大的发现是 A1**：

`meta.element` 1079 行 embedding **全 NULL**、`table_doc` 251 行全 NULL、`datasource` 4 行 active / 0 行有向量。
列、HNSW 索引、召回 SQL、`ddl::vector_ready` 体检 —— **全都在**，唯一的写入点是离线的
`tools/embed_service.py build`，而它**从未跑过**。三个消费点 `unwrap_or_default()` 静默降级，
trgm 兜底总能把 6 个召回额度填满 ⇒ 外面一点看不出来。
`vector_ready` 的文档甚至写着「消费者：… + `/api/health`」—— 而 health 里根本没有这个字段。

⇒ 落地：跑一次 build（1079 元素 / 251 表 / 4 源 / 15 条 enabled 语料）+ 把 `vector_ready`
三项**纳入 `/api/health` 的 `ok`**（哑了不许上线）。`sql_exemplar` 15/82 是**对的**：
另 60 条 `pending` + 7 条 `disabled` 本来就不该进召回 —— 顺带暴露「60 条 few-shot 语料没人审」。

### AX14 · A1 一点亮，自动选源第一次真的开始工作 —— 而它会选错
`source.rs:78` 的注释早写明了：「自动选源今天是空转的…那一天要开，得先把测试遗留的上传源
清干净：每个 `active` 上传源都会去竞争所有问句的路由。**开之前必须重跑回归 + 评测 ——
它改的是每一句问话的选源行为，不是一个新功能开关。**」

我灌了 `datasource` 向量就把这条路打开了，没先清理。回归当场红：
`C01-单号直查`「帮我查下 HJXH-DXO2026072300384」被选到 `upload_…（员工台账）`。

实测距离（本地 bge-small-zh 512 维，`<=>` 余弦）：

| 问句 | 最近源 | 距离 |
|---|---|---|
| 差旅补贴标准是多少 | 上传源《差旅补贴标准》 | **0.247** |
| 通讯补贴按岗位怎么分级 | 上传源《通讯补贴标准》 | **0.2424** |
| 员工台账里有多少人 | 上传源《员工台账》 | **0.4259** |
| 本月销售额是多少 | dms | 0.5625 |
| 帮我查下 HJXH-DXO… | 上传源（**错**） | 0.5982 |
| 买过烤肠的客户 | dms | 0.7103 |
| 今天天气怎么样 | 上传源（无关） | 0.6801 |

真匹配全 ≤0.43、错匹配与无关全 ≥0.56 ⇒ 加绝对距离兜底 `DS_MAX_DIST = 0.5`，
两侧各留 0.07/0.06 缓冲。**修机制不删数据**：三个上传源照旧 active，只是不再抢不相似的问句。

`pick_by_gap` 从 `Option` 改成三态 `DsPick{TooFar, Pick, Ambiguous}`，且 **`TooFar` 判在距离差之前** ——
先判距离差就会让「与所有源都不相似」的问句因为恰好差得够远而被直接采用，那正是 C01 的死法。

三个额外收益：① 单号直查修回 `direct-doc`；
② **上传表格首次能被自动选中并答出数据**（「差旅补贴标准是多少」→ 4 行，之前恒回主源答不出）；
③ 省掉三次注定白花的 LLM 二选一（dms 那三条距离差都 <`DS_GAP`，原来每句都要问一次 fast LLM）。

### AX15 · `<think>` 剥离：写判据时才发现风险形状不是我想的那个
第一版判据构造成「思考段 + ```sql 围栏结论」，**量器自证当场红** ——
`extract_sql` 优先取围栏，抽到的本来就是正确那条，那个形状下根本没有风险。
真正危险的是模型给**裸 SELECT 结论**：那时走「裸文本里第一个 SELECT」兜底，
而第一个 SELECT 在思考段里 = 被模型自己推翻的草稿。判据已按真实形状重写并自证。

### AX16 · 温度分档：0.1 的重试就是同一个错误再来一遍
`run.rs:96` 记着「温度已经是 0.1，压不下去了」—— 那句说的是**首轮**的随机性，
却被当成了「重试也只能这样」。于是 `repair` 拿着错误原文重问一次、模型大概率复述同一条错 SQL；
自一致采样 N 次全用 0.1 ⇒ N 份高度相关的样本，而 `result_print` 的多数派机制假设的是**独立**样本
⇒ 投票投的是同一个偏见。改成首轮 `TEMP_FIRST=0.1` / 重试与第 2..N 次采样 `TEMP_RETRY=0.5`。

**换千问之后 SC 才划算**：千问 plus 中位 2.2s，3 次 ≈ 6.6s；deepseek 8.8s × 3 = 26s 不可接受。
开 `sc_samples=3` 后回归 61/61 全绿、B10（单次取数 24s、190 万行进临时表）没挂 ——
提前收工机制（前两次指纹一致就不跑第三次）真的在省。

### AX17 · 源码扫描判据的自匹配：同一个坑第三次，且这次是**恒真**
写温度分档的判据时 `split("pub async fn repair(")` 的第一个匹配落在**判据自己身上**
（判据在文件前半、被扫函数在后半），切出来的是判据的代码。两条断言里：
- 一条当场红（报错把判据自己的源码打了出来，才发现）
- 另一条**恒真** —— `contains` 匹配到的是判据里的字面量，生产代码怎么改都绿

前两次是 `tools/cli.py::stale_exe`（自己的 `.contains("…")` 行被当成调用）和
`ask.rs::ask_back_is_wired_…`（同形，靠「带引号的行不算」过滤）。

⇒ **定式**：源码扫描判据的锚点一律用 `concat!` 编译期拼接
（`concat!("pub async fn ", "repair(")`），源码里只留两段短串谁都匹配不到完整锚点；
另加「切段自证」断言 —— 切出来的必须含生产代码的标志且**不含 `assert!`**。
枪测：SC 退回全程 `TEMP_FIRST` ⇒ 当场红（修之前抓不到）。

### AX18 · 表级软删除口径：45 张来源表只有 4 张登记过
`table_scope.filter` 会被装配器**确定性地补**到每条 SQL 上，而实测 45 张指标/维度来源表
里只有 4 张登记 —— 其余 41 张的查询一律不带 `deleted_flag = 0`，已删行照算。
其中 `t_after_sales_order_header` 是**指标**来源表（退款额），少这一条就是错数。

证据来自 DMS 自己的 182 个 Mapper XML（按频次统计每张表的固定过滤，
`t_customer_device_ledger` 5/5、`t_account_bill_header` 4/4、`t_goods_sale_information` 8/8）。
状态类口径没捞到，因为 DMS 那边是 `#{}` 参数化的 —— 那本来就不该当表级口径。

落地成**数据驱动**的一条 SQL（不手写 41 行清单：手写会漂，以后新增指标没人记得回来补，
而漏补的症状是「数悄悄虚高」，零报错）。三条安全约束：
① 只对真有 `deleted_flag` 列的表（`meta.column_doc` 反查）—— 42 张候选里有 1 张没这列；
② `ON CONFLICT DO NOTHING` 保住手写的业务口径（订单表的「有效订单」）；
③ 来源表名剥掉注解与别名（`t_x(JOIN …)` / `t_x b0` 两种形态）。
实测生效：「今年各省份的退款额」的 SQL 现在带 `b0.deleted_flag = 0`。回归 61/61。

### AX19 · 确定性装配覆盖率只有 4.4% —— 准确度的最大杠杆
18 指标 × 55 维度 = 990 个组合，按 `direct.rs::find_path` 的口径（BFS ≤3 跳）
实测**只有 44 个可达**。其余 95.6% 回落 LLM，而 LLM 路径抖动池 ≥9/38 ≈ 24%。

DMS 的 182 个 Mapper XML 里有 **60 条** JOIN 关系，`meta.join_edge` 只登记了 **6 条**。
🔴 判据是**可达性增益**不是「源码里有」（上一轮凭高置信度加了 5 条边，增益 0，全部回退）。
逐条量过：9 条有增益，加上去 **44 → 113 组合（+157%）**，覆盖率 4.4% → 11.4%；
另 23 条零增益（32 条全加与只加 9 条结果完全相同）。
最大的一条是 `t_customer.customer_code = t_customer_balance.customer_code`（**+18**）。
⚠️ 113 是**上界**：没判扇出（1:N 边只能靠 `COUNT(DISTINCT)` 过）、没判时间列能否落到基表。

### AX20 · 历史发货净销售额实验（已由 AX97 废止为默认口径）
> 历史记录仅用于解释旧实现来源；自 2026-08-06 起不得据此生成默认销售额、评测金标或报表 SQL。

业主给了一段权威 SQL（生产口径）。逐字段对拍后它与我们现有「销售额」**不同**：

| 项 | 我们的「销售额」 | 业主的口径 |
|---|---|---|
| 金额来源 | `t_sales_order.total_amount`（订单头） | `apportioned_price × batch_delivery_quantity`（分摊单价 × 发货量） |
| 计入时点 | `order_time` 下单 | `c.delivery_time` 发货 |
| 商品范围 | 全部 | `t_goods.group_number = 'CHJZFL05-SYS'`（产成品，**是码不是名**） |
| 渠道 | 全部 | `t_dict_value dict_key_id='67'` 且 `value_name='线下销售单'` |
| 有效订单 | `order_status NOT IN ('0','108','199')` | `order_status NOT IN ('0','100') OR paid_status = '1'` |
| 退货 | 独立指标「退款额」 | UNION ALL 记负数，含在同一个数里 |
| 时间窗 | 自然时间窗 | 历史发货专用窗口（不适用于 DWS） |

当时新增过独立指标 `ship_net_sales`；该指标及其 gold 已被 AX97 的 DWS 默认事实合同取代。

**修出三个生产方没注意的错**：① `inner` 缺 `join`；② `date_trunc` 是 PG 方言，MySQL 要 `DATE_FORMAT(…,'%Y-%m-01')`；③ `ds.group_number = '产成品'` 在这个库上**恒 0 行**（存的是码 `CHJZFL05-SYS`）。

实测（2026-07，本月 1..昨天，只读通道）：发货正向 12672 组 +206,968,985.33；退货负向 252 组 −884,166.14；**净额 206,084,819.19**，同期订单口径 219,048,373.88，差 5.9%。

已登记 `ship_net_sales`（`agg_expr` 是两个标量子查询，照「退款占比」的先例；
装配器「含 SELECT 即不装配」的门把它留给 LLM，口径卡在 description 里）。
端到端实测 `route=llm+repair`，SQL 结构全对（正向 + 退货并入）。

**迁移结论**：旧发货指标的专用截断规则不得进入 DWS。DWS 的今天、本月、本周、今年均使用正常
自然半开区间，评测和报表也不再保留旧截断预期。

### AX21 · 「判据太宽」第四次，且是我自己写的判据抓了我
`code_filters_never_use_chinese_names` 在新指标上当场红 —— 但**抓错了**：
它治的病是「**码字段**被写中文名」，而 `d.value_name = '线下销售单'` 是**字典的值列本来就该写中文**
（`t_dict_value` 的 `value_code` 是码、`value_name` 是名）。
收窄到只判「列名本身是码」（`_code/_status/_type/_flag/_id`）。
枪测时发现两个造枪的问题：① `a.status = '已完成'` 落在**元组的 `source_table` 字段**（不是 SQL），
判据正确跳过 —— 枪造错了形状；② heredoc 把中文字面量转义坏了，枪根本没写进去。
第三个才是对的（`item_type = '正品'`，落在 SQL 语义位置）→ 当场红。

### AX22 · FIN01 的错法变了：F4 修不掉，错在「JOIN 了扇出表只为取名字」
F4（裸列的表归属）修完后 FIN01 仍是错数，而且 **route=llm、caliber_note 空** ——
判据没开火，因为错法**变了**：

```sql
FROM t_invoice_apply_header i LEFT JOIN t_sales_order o ON i.customer_code = o.customer_code
```
模型 JOIN `t_sales_order` 拿客户名。而发票头与订单是**一对多**（一个客户多张发票），
于是发票金额被按订单数**放大 2000 倍**（首行 4.39 亿 vs gold 219 万）。
gold 的正确写法是 JOIN `t_customer`（一码一行）。

这暴露了 F4 判据的边界：它管「裸列冒充约束」，管不了「JOIN 进来一张 1:N 的表只为取名字、
聚合被它放大」。这一类错法**今天没有任何判据**（`RequireJoinAndFilter` 只管「表必须在场
且列被约束」，不管 JOIN 的基数）。

⇒ 先做 F5（留痕）把这一族错法记下来，再定怎么判（可能要 `RequireNoFanout` 一类的新判据，
或在 prompt 里把「取客户名 JOIN t_customer 不 JOIN t_sales_order」写进口径卡）。

### AX23 · F1/F2/F3/F4 四道防线已落地（诊断 wf_c921b918 的前四条）
| 防线 | 修的 | 判据 |
|---|---|---|
| F1 39 条 gold 过闸门 | AS04 那类「闸门悄悄收紧到拒正确 SQL」 | `every_gold_sql_passes_the_guard`（gold 数 ≥30 防空转 + `SELECT 1` 仍被拒防恒真） |
| F2 常量投影看任意层级 | AS04（占比指标永久失败） | `rejects_constant_projection` 反面清单加 AS04 真实形状 + 「嗨肉」现场照旧被拒 |
| F3 时间桶列 | AS01（过滤对了分桶用错列，三种配置都错） | 四条两面：gold 绿 / 错形状红且 hint 两列 / 对称红证采集非空 / 非桶列绿 |
| F4 裸列的表归属 | FIN01 的第一种错法（发票裸 deleted_flag 冒充 t_customer 约束） | 四条两面：错答红 / 补上后绿 / gold 绿 / 单表裸列绿 |

全部枪测过（退回上一版就红）。F3/F4 的用例都因「kernel 不得含 DMS 表名」门禁挪到了 `tests/`。

### AX24 · F5 三处留痕落地，但暴露出「闸门有两条路径」
三处已落地：① `gate-blocked` 进 `correction_log`（闸门拒了 LLM 已生成的 SQL 时写）；
② `add_scope_filter` 带 WITH 放弃时打 `tracing::warn!`（口径没补上、症状是数悄悄虚高）；
③ 判据钉住 `gate-blocked` 的落点（`concat!` 拼锚点，防自匹配）。

**验证时发现 `gate-blocked` 有两条路径要分清**：
- `ask` 路径（`run_once`）的闸门拒绝 → 写 `correction_log`（这是 F5 修的那处）
- `exec-sql` 子命令的闸门拒绝（`is_safe_select`）→ **不写** —— G01-G03 的闸门题就走这条
- 「把今天的订单删掉」「嗨肉」都走 `need-intent`（意图反问先接住了，产不出 SQL）

⇒ `gate-blocked` 只在「LLM 真产了 SQL、而闸门拒了它」时才开火，那是 F5 要的取证。
`exec-sql` 那条 CLI 路径要不要也留痕是独立的事，不在 F5 范围内。

### AX25 · 图片识别换千问 flash：tesseract 降级，实测三扫描件全对
业主裁决「图片识别用千问」。落地成 `_p_image` 优先千问 flash、`_cap_ocr` 能力门同步放行、
tesseract 降级，**零新依赖**（`urllib.request` 发 HTTP，不引 openai 包 —— 宿主机 SAC 会拦新编译扩展）。

实测（`_silent/` 三个扫描件，全部只读）：
- `multiframe.tif` 两帧 → 千问 689/712ms 全对（`TIFFPAGE2-7788`）；tesseract **只认第一帧**
  （`embed_service.py::_p_image` 的注释里记着那个实测缺陷：2 帧 tif 只出第一帧的内容）
- `scanned.pdf` → 千问 896ms 读出 `SCANONLY-3344`；tesseract 产空文本
- `mixed.pdf` 文本层+图像页 → 千问逐页全对（`TEXTPAGE1-5566` / `PDFOCR2-9911`）

端到端验证（容器内 `_p_image` / `_p_pdf` 全链路）：multiframe.tif 2 帧全出、scanned.pdf 1 页全出。
`_cap_ocr` 改成「千问或 tesseract 有一样就能用」—— 宿主侧没有 PIL/tesseract 时
图片不再被能力门挡掉（之前 `parse_ok['image']=false` 是体检误报：真正的解析走解析容器，
那里 PIL + tesseract + 千问路全有）。

配置：`DMS_QWEN_OCR_KEY`（或复用 `QWEN_KEY`）、`DMS_QWEN_OCR_MODEL`（默认 qwen3.7-flash）、
`DMS_QWEN_OCR_BASE`（默认 dashscope compatible-mode）。千问不可用时回落 tesseract。

### AX26 · B2 的一半：新指标向量补上（1080/1080）
A1 灌完之后**新加的指标**（`ship_net_sales`、销量、赠品箱数、活动费用、活动场次、动销商品数）
没有向量 —— 跑 `revec` 补上，1080/1080 全有。
另一半（启动补齐 + 变更失效 + 后台重算）是 A9 的活，不在 B 批。

### AX27 · B1 知识库零命中观测：分清「降级 / 阈值过滤 / 真没有」三种
由来：知识库检索返回空时零观测 —— `search_with_status` 只回 `(hits, vec_down)`，
而 hits 为什么空（向量哑了 / ACL 挡了 / `TRGM_MIN`/`VEC_MAX_DIST` 过滤了 / 真没有）完全看不见。
三者的处置完全不同（① 修 embed / ② 降阈值 / ③ 告诉用户补文档），却长一样。

落地：`retrieve.rs::search_with_status` 的零命中路加一条 `tracing::info!`，
带 `vec_down` + 三路各自的召回数（vec / fts / trgm / merged）。
用户侧要看见的话是 A6 的活（分步留痕进 `Answer` 的 serde 形状），不该在 B1 里复制第二份。

验证：`ask` CLI 走的是**问数路径**不是知识库路径（「月球基地」在问数侧产了 `SELECT 1`
被常量投影拦，SC 两次采样都失败 —— 那是另一条路的行为）；
知识库路径 KB07（nohit-库里没有的事）照旧过，零命中日志在 `retrieve.rs` 里。

### AX28 · 知识库批（B1/B2/B3）+ 千问图片识别 已落地
按用户选定的四批执行，知识库批完成：

| 条目 | 落地 | 实测 |
|---|---|---|
| 千问 flash 图片识别 | `_p_image` 优先千问、tesseract 降级；`_cap_ocr` 能力门同步放行；零新依赖 | 三扫描件全对（tif 两帧 / 扫描 pdf / 混排 pdf）；tesseract 全废 |
| B2 向量自愈（一半） | 新指标向量补上 | 1080/1080 全有 |
| B1 零命中观测 | 分清「降级 / 阈值过滤 / 真没有」 | `tracing::info!` 带三路召回数 |
| kb_eval 两套 | OCR 换引擎后 | 16/16 + 5/5 全绿（含 KBB05 扫描件） |

`<KB_ROOT>` 当时有 23 个文件：15 个已入库，`_probe/`（5 个格式回归夹具）+ `_silent/`（3 个 OCR 试金石）是测试夹具不进生产。

### AX29 · A5 trace_id 串起三张表：HTTP 路径通了，CLI 路径恒空
三表（`correction_log` / `failure_log` / `query_log`）原来各记一段、拼不回同一次问答 —
「数字错了是模型写错还是校正器改坏」查不出来（`chat.rs:117` 的亏）。

落地：`query_log::Trace` 加 `trace_id`/`conv_id`（`OnceLock`，复合子问句共用一个、只写一次）；
`AskDeps`/`AskCtx` 加 `trace_id`/`conv_id`；`exemplar::log_correction_traced` /
`log_failure_traced`（空 `trace_id` 时与旧签名逐字等价）；`query_log::insert` 带上
`trace_id`/`conv_id`/`llm_calls`（空串落 NULL）。HTTP 有 `conv_id`（`chat.msg.conv_id`）用它，
CLI 没有会话概念时与 `trace_id` 相同。

实测：**HTTP 路径通了**（`de035f3a` 进 `query_log`），**CLI 路径恒空**（`x`）——
CLI 的 `ask` 子命令在 `main.rs:584` 调 `ask()`，而 `set_trace` 在 `ask()` 内部，
CLI 那条路走的是 `mcp_api::tool_ask` 的 `crate::ask`（`main.rs:289`），它**不调 `set_trace`**。
`llm_calls` 现在恒 0 —— 它该记「这一轮打了几次 precise LLM」，但 `Trace` 只数「最贵的两次」，
而 SC 的采样次数在 `run_llm` 里（agent 侧），`Trace` 在 server 的 `query_log.rs`（带 axum），
agent 不能引它。接它要动 8 处签名（`on_usage` 的落点 `Trace` 带 axum）。

### AX30 · AX29 的 CLI 理论是错的：真凶是 spawn 竞态吞掉整行，不是一个空字段
AX29 判「CLI 走了不调 `set_trace` 的 `crate::ask`」—— 错。两处 `ask` 是**同一个函数**
（`main.rs` 的 `ask()`，`set_trace` 就在里面）；且实测 CLI 每次问都打出
`一次问答的关联键已生成 trace_id=…`。

真相分两层，实测逐层钉死：
1. **CLI 的行根本没落库**。`query_log::finish` 内部 `tokio::spawn` fire-and-forget；
   CLI 是一次性进程，`main` 打印完 JSON 就返回，运行时带着没跑完的 INSERT 一起死。
   实测：CLI 问完立刻查 `query_log`，**最新行还是上一轮 HTTP 的**（查无此行）。
2. 库里的 `x|llm` 旧行是**加 `trace_id` 列之前**的二进制留下的（`INSERT` 列清单里没有它，
   落 NULL），不是新代码写了空串。debug warn 没打出来正是因为 `finish` 连跑都没跑到。

修复（一次到位，两个调用形态各得其所）：`finish` 返回 `JoinHandle<()>`，`ask()`
返回 `(Result<AskResult>, JoinHandle<()>)` —— **服务侧**（`api_ask`/`mcp_api`）直接丢弃句柄
（fire-and-forget 纪律不变），**CLI 分支**在打印结果前 `let _ = log.await`。
判据 `cli_awaits_the_log_handle_before_exit`（concat! 锚点防自匹配）钉住这条链。
验证：CLI 问「昨天销售订单明细」→ 第 35 行 `trace_id=04765a2d` 与日志逐字一致。
附带拆掉 AX29 期间加的 debug warn（`query_log::finish` 里的空 `trace_id` 告警）——
它诊断的是错误的层。

### AX31 · A6 分步留痕（steps）+ `llm_calls` 真值，一处回调都不多加
- **steps**：`AskResult` 加 `#[serde(skip_serializing_if = "Vec::is_empty")] steps: Vec<Step>`
  （`{stage, kind, ms}` 三字段，`kind ∈ hit/miss/skip`）。收集点就一个：`ask_single` 的
  分派循环 —— 五个 Answerer 一行未改。空 = 没走 Router（need-intent 反问、复合容器），
  老前端与两个判官脚本的 JSON 形状不变（serde 判据钉在既有形状测试里）。
  前端 `App.vue` 用原生 `<details>` 折叠（不引组件），kind 翻成 命中/未接/跳过。
  端到端实测：`库存金额表` → `[graph skip 0ms, direct-agg miss 73ms, direct-doc miss 0ms,
  semantic-cache miss 20ms, llm hit 6077ms]`。
- **`llm_calls`**：AX29 记「要动 8 处签名」是想多了 —— `Trace::add` 每次调用就是一发
  precise，加一个 `AtomicU32` 计数器即可（5 行），agent 侧零改动。实测 llm 路径
  `llm_calls=2`（SC 采样提前收工直接可读），graph 路径 0。
- **回归**：61/61 全绿（G02 在 AX30 部署后那次跑出过一次性假红 —— 输出混入 1146，
  手动复现与全量重跑均不复现，记为环境抖动；B10 本轮 15.2s 通过）。

### AX32 · `NoFanoutJoin`：FIN01 的 299 倍放大有了判据，评测当场修复
- **病灶**（评测 FIN01 实测两次）：模型为取客户名把发票单头
  `LEFT JOIN t_sales_order ON customer_code`（一个客户 N 张订单），
  每行被复制 N 份，开票金额放大 299 倍（654888936 = 2190264 × 299，整除得整整齐齐）。
  闸门不拦（合法 SELECT）、执行不报错、既有口径判据只管「该约束的列约束了没有」。
- **判据**（kernel `CaliberRule::NoFanoutJoin`，三处全偏漏判）：
  ① 只认「被 JOIN 进来那一侧」的重复键（基表侧是正常方向）；
  ② 只在有 SUM/AVG/MIN/MAX 列入参时判（COUNT 数行，扇出常是本意）；
  ③ 度量前缀全落在被 JOIN 侧 → 不判（聚的就是它）。
  键清单由构造侧从 `meta.join_edge` 的 card 推出（`N:1` 取左、`1:N` 取右），
  kernel 一个表名都不认识。Facts 新增 `join_eqs`（ON 等值对，RIGHT/USING 不收）
  与 `measure_aggs`（度量入参前缀，COUNT 族不进）。
- **实测**：FIN01 从 654,888,936（错 299 倍）→ `llm+repair` 10 行与 gold 一致。
  判据判红 → 回炉 → 模型改写成功，正是口径环设计的那条路。
  两面钉死：bad SQL 红（kernel/tests 真表名版）+ gold 绿（`t_customer` 不是重复键）
  + 「各客户销售额」日常形绿（基表侧重复键）。
- **覆盖面**：5 条 join_edge 推出 5 个重复键（订单×2、明细×2、商品×1）。
  边种子扩充（`seed.rs:402` 那条 🚧 清单）会自动扩大判据覆盖，不用改判据本身。

### AX33 · A7 空召回放宽一档 + A8 问句切片向量，一条 SQL 都不多打
- **A7**（`recall_elements`）：严格档 0.35 全空才放宽到 0.5，零额外往返
  （一次取回 limit 行，换个阈值再滤一遍）。0.5 的天花板与选源 `DS_MAX_DIST` 的
  实测距离表同源（真实命中 ≤0.43、错源 ≥0.56），再宽是噪声区。宽松命中打 info ——
  「靠放宽救回来」的频次是召回质量的调参依据。
- **A8**：滑窗 `candidate_windows` 从 connector 上提到 kernel（图路径实体抽取与
  SQL 切片召回共用一份，判据随迁）；gather 把「整句 + 前 24 片（长词优先）」一次
  `embed_passages` 批量打完（内部 64 一批，只多一次往返）；`recall_elements` 按
  「任一片最近」取 MIN 距离（`unnest($1::text[])::vector` 单条 SQL，不多打往返）。
  只有整句向量时包成单片走同一条路 —— 「降级留痕」判据的计数不变。
  切片 embed 与 `qvec` 同一降级类（match 不是 `unwrap_or_default`），不进六路 warn 判据。
- **实测**：「湖南省客户今年烤肠的销售额和订单数对比」（长问句，整句向量被稀释的
  原形）→ 切片召回命中 8 张元素卡（严格档全空 → 放宽一档救回），多向量 SQL 无错。
  回归 61/61 全绿，全 workspace 624 单测绿。
- **注意**：`RecallCtx` 加字段动了 6 个构造点（agent×3 / server×2 / triage×1），
  spread 点（`..rc`）自动继承 —— 这就是为什么字段要进 `RecallCtx` 而不是加形参。

### AX34 · A9 向量自愈：写入点不再只有离线脚本
- **病灶**：`embedding IS NULL` 的补齐此前只有离线脚本（`embed_service.py build/revec`），
  服务侧只有体检没有修复 —— `upsert_datasource` 变更置 NULL、ingest 遇 embed 不可用
  停在 chunked，都永久等人工。
- **形态**：server 后台 spawn（启动即跑 + 每 10 分钟一轮），PG advisory lock
  （`pg_try_advisory_lock`，同连接解锁，失败路径也解）当 SQLBot `SingleWorkerGuard`；
  每轮每类封顶 256 行。meta 四类（table_doc/element/datasource/sql_exemplar）的
  **文本配方与离线 build 逐字一致**（判据钉着：两边写同一列，配方不同 = 同一列混
  两套不可比向量，0.35/0.5/0.55 三个实测阈值全废）；语料问句走新增的
  `embed_queries` 批量（Query 模式，与离线 `is_query=True` 同款）。
  kb 块按 chunk_id 补 + `NOT EXISTS(NULL 块)` 的文档推 `embedded`（ingest 同款迁移）。
- **实测**：启动轮补回 7 行真账（历史停摆的 kb 块等），四张 meta 表 NULL 全 0；
  人工置空一行元素向量，下一轮自动补回。embed 缺席时行保持 NULL 下轮再试，
  不是错误（与 ingest 的降级同款，判据钉着）。
- **明确不做**：HNSW 索引重建仍归离线 build（自愈只补行；`embedding` 列上索引
  对 NULL 行无感，补回即生效，不需要重建）。

### AX35 · B3 重排被量具当场毙掉（省一整个功能）
- **量具**（`tools/kb_rerank_probe.py`，一次性）：离线复刻 `retrieve.rs` 三路 SQL
  （逐字照抄常量）+ RRF(k=60)，对 kb_eval 全部 recall 题算「金块在融合榜的最好名次」。
  不过 ACL = 名次上界（上界都好，真实只会更好）—— 判「毙」在真实 ACL 下依然成立。
- **实测**：7/7 题金块名次全是 **1**。「真相关但被挤出 TOP_K」的 7..20 收益带**为零**。
  按 B3 自带的裁决规则（@20 明显高于 @6 才做）→ 不做。飞榜的题一道都没有，
  说明召回层健康（校准阈值 + A9 补全向量在先）。
- **副产品**：启动轮 + 次轮自愈共补 12 行（历史停摆 kb 块 + 评测新造的值元素行），
  四张 meta 表 NULL 清零 —— 值元素是评测问句实时造出来的（E08/E15 换码路径），
  正是 A9 要兜的形态，闭环成立。

### AX36 · B6 术语/示例 CSV 往返 + 批量复核通道（61 条 pending 有工具了）
- **落地**：`GET/POST /api/admin/terms.csv`（导入逐行校验、坏行按行号点名返回；
  aliases 用 `|` 连接）；`GET /api/admin/exemplars.csv?status=`（**只导出不导入** ——
  示例只许来自真实问答 + 人工复核，CSV 导入就是开后门绕过它）；
  `POST /api/admin/exemplars/status` 批量复核（与逐条版同一个 `review_status_ok`，
  ids ≤500：一次勾几百条本身就说明没在复核）。
- **CSV 手写零依赖**：引号内吃换行与逗号、`""` 转义（示例 SQL 必含逗号与换行，
  `split(',')` 第一条就碎）；往返判据打在引号/逗号/换行/中文混合形态上。
- **实测**：导出术语 5 行含逗号定义正确加引号；导入「含逗号口径 + ""引号"" + 双别名」
  落库逐字一致；坏 status 与空 ids 双双 400。61 条 pending 示例待**人工**复核
  （复核是给人做的，工具已齐：导出 → 标记 → 批量置 enabled/disabled）。

### AX37 · 历史 ship_net_sales 修复记录（已由 AX97 废止为默认口径）
一连串四个根因，逐层实测推进：
1. **判据误伤（我自己造的）**：`NoFanoutJoin` ③第一版判「度量前缀全落在被 JOIN 侧」，
   把业主口径（`a JOIN b(dup) JOIN c` 取 `b.price × c.qty`）判红 —— 被复制的只有
   **等值另一边**的行，第三张表的行数由它自己的连接粒度决定。判红两轮把模型
   **从正确口径上逼走**（丢产成品过滤、换错时间列）——「判错一条连带把对的答案
   回炉改错」的活样本，③改成「等值另一边贡献了度量才判」，业主形态防误伤断言进
   kernel/tests。
2. **历史专用时间判据**：旧实现曾为发货双分支增加专用截断。AX97 后该判据不得绑定 DWS
   `sales_amount`，DWS 当期窗口正常包含今天。
3. **实测数字锚定**：描述里的「实测 2026-07 净额…」让模型把 CURDATE() 换成
   字面量 '2026-07-31'。数字挪出 prompt（留在种子注释），描述只留相对结论。
4. **负向时间列**：说明长段里的「时间用 h.upload_time」赢不了模型的命名先验
   （t_after_sales_order_header → after_sales_time）。解法是**把行内注释写进
   agg_expr 的对应分支**（`/* 时间条件加在这一行：h.upload_time（不是 after_sales_time） */`）
   —— 指令必须在它起作用的那一行上，差 3.5%（198.9M vs 206.1M）。
最终：「本月发货净销售额」→ 206,084,819.194200，与业主 2026-07-31 实测逐位一致。

### AX38 · SALE15 回潮的真凶：`output_shape` 反引号没归一，合规修复被形状闸整批否决
- **现象**：SALE15（昨天 84359=gold）今天连续 4 次出 13045 错值，`llm` 与 `llm+repair`
  两态乱跳。
- **证据链**（一次埋点到位）：warn 打两份形状 → `before=["商品名称","销量"]` vs
  `after=["`商品名称`","销量"]` —— 模型把 `sku_name AS 商品名称` 合规改写成
  ``SELECT `商品名称` FROM (SELECT DISTINCT …)``（引别名、列一个没动），
  而 `output_shape` 只在 `AS` 分支剥反引号、裸引用分支不剥 → 形状误判「输出列变了」
  → **每一次合规修复都被否决**，候选永远是初版坏 SQL，两轮预算耗尽挂「结果不可信」。
- **修法**：裸引用分支同样 `trim_matches('`')`（剥前导/后导反引号；`SUM(`x`)` 这类
  表达式尾部不是反引号不受影响）。判据：同列带反引号必须过、换别名列照样拦。
- **教训**（已写进注释）：形状闸比的是**列**不是字节。这不是模型变笨、不是 prompt 漂移，
  是归一化缺口被模型文体漂移（改写习惯从 `AS 别名` 换成引别名子查询）暴露出来。
  埋点方式值得记住：warn 里直接打 `before=?/after=?/rewritten 前 400 字`，一次定位。
- **验证**：SALE15 单题 `llm+repair` 10 行与 gold 一致；FIN01 同晚回潮由 AX39 收
  （两题随后各自单跑全过）。同场确认 `RequireTimeCap`/`NoFanoutJoin` 对 61 题回归零误伤。

### AX39 · FIN01 二次回潮与「静默形状闸」：判词里点名要保的输出列
- **现象**：FIN01 再次 654M 扇出值。判据（`NoFanoutJoin` ③修正版）开火 4 次，
  修复 4 次被形状闸挡 —— 埋点显示 `before=[客户,开票金额]` vs `after=[客户名称,开票金额]`：
  模型修连接时顺手把 `客户` 改成 `客户名称`，`keeps_output_shape` 正确地否决，
  但模型**不知道自己为什么被否决**（判词里一个保列的字都没有）。
- **修法**：口径回炉的判词末尾点名要保的列（`output_shape(candidate)` 渲染进
  Retry msg）：「输出列（含别名）与排序必须逐字保持：客户 / 开票金额 ——
  改一个字符都会被整单否决，只许动口径」。只挂口径回炉：执行错误的自修可能要
  换输出列，不吃这句。
- **验证**：FIN01 `llm+repair` 10 行与 live gold 一致（top1 沈阳浚恒 410.5 万，
  与 gold 同时刻同值）。SALE15 同晚另出一次 1054 方差（模型把别名当真列，
  重跑即过）—— 温度 0.5 的边缘方差，非定势。
- **两条形状闸教训合订**（AX38+AX39）：闸门比的是列不是字节（反引号归一），
  且**闸门必须让被闸者看得见规则**（判词点名保列）—— 静默的闸门只会消耗预算。

### AX40 · A10 prompt 总量预算 + 语料同构快照（采集侧）
- **预算**（`PROMPT_BUDGET_CHARS = 40_000` 字节）：首轮与回炉同一道护栏，超了按段
  优先级丢 —— 首轮：维度卡尾 → 值域卡 → JOIN 对面表卡片 → 召回表尾 → 维度清零+元素留 2；
  回炉：只砍维度段（20→8，指标与 schema 一刀不动，它是回炉的目的）。
  **绝不丢**指标/术语/时间/码值/关联/教训/few-shot。今天首轮 ≈9KB、回炉 ≈33KB
  都远低于它（未超时一字节不动，有判据）—— 它守的是表越来越多的明天。
  测试自己的坑：预算按**字节**算，测试夹具用中文 repeat 直接把维度卡撑爆 3 倍
  （本仓第四次踩 len()=字节）。
- **同构快照**（SuperSonic `Text2SQLExemplar`）：`meta.sql_exemplar` 加
  `schema_snapshot` / `side_info` 两列（渲染好的文本，不存结构），
  `save_with_context` 在沉淀时连当轮 schema 段与口径卡（`prompt::side_info_of`）
  一起存。实测：新问句沉淀行 snap=7793B / side=578B。
- **渲染侧缓做**（与计划的偏差，明说）：few-shot 今天仍是两行式。快照渲染激活等
  「样例引用了召回不到的表」的实测证据 —— 每条语料 +9KB 与预算护栏直接对撞，
  没有证据就渲染是给预算添乱。两列今天已服务复核（看样例当时的上下文判对错）。
- **注意**：快照只在 llm 路径沉淀（确定性路径本来就不产语料 —— 与既有纪律一致）。

### AX41 · A11 schema 注释业务自助维护（CSV 往返，零新依赖零信任边界新增）
- **端点**：`GET/POST /api/admin/schema-comments.csv?ds_id=`。导出一张表注释+列注释
  合一的文件（`kind` 列区分 + `native_comment` 只读参照）；导入逐行校验、坏行按
  行号点名（与 B6 术语导入同形态）。
- **三条红线**（判据钉着）：只 UPDATE 不 INSERT（表/列来自 schema sync，管理面
  不创造文档行 —— 拼错表名按行点名而不是静默造一行永远渲染不到的文档）；
  每格过 `dms_semantic::ingest::sanitize_comment`（与 schema sync **同一处**信任边界，
  不开第二份）；全部带 ds_id 谓词。空串合法 = 清除人工注释回落原生列（复位通道）。
- **实测**：导出 46 表头格式正确；导入表/列注释落库；幽灵表与不存在列双双按行
  点名拒绝；空串复位清零。业务人员从此改注释不用找开发改代码（seed.rs 的
  手写 ⚠️ 常量仍是代码级兜底，两边不冲突）。

### AX42 · A12 三个补缺校正器（SelectCorrector / removeSameField / 时间下界）
- **`fix_select_fields`**：GROUP BY 有、SELECT 没有 ⇒ 带前缀列引用补进投影最前
  （缺分类轴 = 图表出单值 KPI）。漏判侧四条全钉：别名 group by（`月份` 补进去
  就是 1054）/ 位置序号 / 纯维度查询 / 无聚合。
- **`dedup_select_fields`**：只去**整项逐字相同**的投影重复；`SUM(x) AS a,
  SUM(x) AS b` 一个不动 —— `ORDER BY b` 还指着它，删了就是把能跑的改挂。
- **`fix_time_lower_bound`**：时间列只有 `<`/`<=` 没下界 ⇒ 补 `>= '1970-01-01'`
  （语义中性，防全表扫）；**只做这一半**，「缺时间补默认窗」是 X3 裁决明令禁止的。
- 链序：SelectFields → DedupSelect → GroupBy → Agg → Caliber → Value → TimeLowerBound
  （投影级最前，WHERE 级最后）。新 kind 落 correction_log。

### AX43 · A13 危险函数黑名单 + 校正器 Err 全留痕
- 闸门 FORBIDDEN 加 `load_file`/`pg_read_file`/`pg_ls_dir`/`xp_cmdshell`/`utl_file`
  （词边界扫描天然兼容：下划线保在 token 里，`upload_time` 业务列不受影响 ——
  有判据）。AST 锁 Query 锁不住函数，这是绕过只读红线的唯一形态。
- `correct_chain` 三个 IO 校正器 + `schema_fix` 的 Err 分支全部补 warn（此前
  `if let Ok(Some(_))` 静默吞 Err —— 「校正器集体失灵」与「无事发生」同形）。
  判据钉 Err 分支数 == warn 数；锚点又撞一次自匹配家族（本文件注释里就写着
  这个坑，照样踩 —— 换 `concat!` 才绿）。

### AX44 · A14 选源向量点亮确认 + 遗留注入夹具源清退
- **选源向量已通电**（A1+A9 合力的结果，不是新工作）：`meta.datasource` 全部
  active 行带向量（dms + 2 个业务上传源），`nearest_datasources` 不再恒空，
  三态选源（TooFar/Pick/Ambiguous，DS_MAX_DIST=0.5 实测校准）早就在工作。
- **清退**：`upload_3ee5efc0`（员工台账_表头注入.csv）—— kb_eval 的**提示注入
  安全夹具**，此前是 active 状态参与所有问句的选源竞争。已置 `disabled`
  （kb 检索走 kb.chunk 不受影响；kb_eval 重跑会重新上传新副本，那是夹具生命周期，
  不是漏）。这是「选源点亮前必须清掉测试遗留源」那条计划的原样执行。
- **结构性欠账**（记下）：上传即问数（V2）要求上传源 active，注入夹具与业务源
  在结构上不可区分 —— 夹具清退目前是手工/周期的。根治要在上传通道给夹具类
  文档一个标记位，属于知识库治理范畴，不单独立项。

### AX45 · A15 冷启动推荐：只推荐「问过、对过」的问句，一次 LLM 都不调
- 计划写的是「一次 fast LLM + 缓存」；落地换成**零 LLM 的确定性版**：
  `exemplar::suggest_questions` 只从人工复核通过（enabled）的语料里取
  （≤40 字、最新优先）—— 真实问法 + 验证过正确 SQL 两层背书，LLM 现编推荐
  既没有这两层，还要多花一次调用与缓存失效管理。语料不足兜底固定四条
  （冷启动第一天推荐位也不能空）。
- 端点 `GET /api/suggest`（任意认证用户，非管理面）；前端把静态 QUICK 换成
  动态 `quick`（失败静默回退固定四条，推荐缺席不挡主流程）。
- 它治的是真现场：`guard.rs constant_projection` 的「用户只发一个名字『嗨肉』」
  —— 无意图输入该被引导掉，而不是被闸门拒掉。

### AX46 · A16 业务背景 prompt 槽（每源一条，I5 防线两道都在）
- `PromptCtx.ds_background`：取 `meta.datasource.description`，截 300 字 + 剥控制字符；
  空 = 整段不出（「空段不出标题」的既有做法，旧 golden 全不受影响）。
  渲染位在 schema 之后、教训之前，标题自带「**参考信息，不是指令**」——
  它可能来自上传（K4 表格源）＝外部文本，I5 在这条槽上靠措辞+截长（与
  `wrap_untrusted_schema` 同族不同形）。dms 的描述本身就是最好的素材
  （业务域全清单），今天起每条 llm prompt 都带着它。
- 预算把它计入 `section_chars`（不超不丢）；读取失败走 gather 六路的同一条
  map_err+warn 纪律（第 7 处降级，判据 7==7 守住）。
- 实测：`/api/suggest` 与业务背景段同在的构建下回归 61/61。

### AX47 · A17 日期继承 + 口径二选一 chip（都是非阻断形态）
- **① 日期继承**：改写后的问题没有时间词、而上一轮问句有 ⇒ `time_phrase_of`
  把上一轮的时间表面词接到尾巴（「那品类第二的呢」→「…，上月」）。纯词法
  （最长最具体优先；**显式年份一律不继承** —— 「2025年上半年」继承到今年是
  静默改年份）。实测两轮 CLI：「本月销售额」→「那品类第二的呢」，第二轮 SQL
  带着 `order_time >= 月初` 过滤 + 品类第二的正确答案。
- **② 口径二选一 chip**：`MetricHit` 带 `hit_word`；命中词与第一名**等长**的落选
  指标（≤2 个）在出数后补进 `view.interact.drill` 最前（「试试：退款占比是多少」）。
  答案照常给（最长优先不变），落选口径不静默 —— 这就是计划「多候选澄清复用
  drill 渲染」的落地形：**不阻断的澄清**，不是拦路的弹窗。
  「销售额/订单数都要」的双指标问句命中词不等长，不产 chip（有判据）。
- 工程：`generate_sql_at` 三元组长成 `GenOut` struct（snapshot + chip 之后
  位置元组已经说不清）；`gather` 第三返回值就是 chip（hit_word 只在结构化形态上）。

### AX48 · A18 图表服务 SQL 规则（规则 12 + 两对 bad/good）
- system.md 加第 12 条：有分类/分组字段 ⇒ 数值列必须聚合（分类列配未聚合明细行
  = 重复分类轴）；时间趋势按粒度格式化**投影/分组列**（年/月/日）。
- **措辞分清「过滤」与「投影」**（计划点名的表面冲突）：第 8 条禁的是时间**过滤**时
  `DATE_FORMAT` 包裹列（走不了索引），本条说的是投影列的粒度格式化 —— 两条并排
  写明互不冲突，模型不会把第 8 条误读成「时间列一律不许格式化」。
- 别名全部 `{quote}` 占位（PG 断言零裸反引号）；「用本方言的日期格式化函数」
  写明（上传 PG 源的 prompt 同一份模板）。
- 两对 错误/正确 代码块（未聚合分类 / 按秒分组的真坑形态）—— SQLBot 的
  output-bad/output-good 成对示例法，比单写规则有效。

### AX49 · A19 术语定义递归 mapping（一层即止）
- 命中术语 ⇒ 拿它的 `definition` 当召回问句再跑 指标/维度/值域 三路，与已有卡
  **按名精确去重**后并入术语段（包含判据会把「销量占比」当成「销量」误删）。
  一层即止：产出不再递归。做在 A10 之后 —— 预算护栏已在，维度段先丢。
- 接线进 gather 第六路（降级同样 map_err+warn，判据 7→8 依然条数相等）。

### AX50 · A20 表级人工启停（`enabled`）：四道闸一个不缺
- `meta.table_doc.enabled`（默认 true）。向量/trgm 两路列表 SQL 加谓词（效率，
  停用的表不占 k 名额）；`render_schema` 加 `AND enabled`（总闸 ——
  forced/向量/trgm/对面表卡片全在此汇流）；admin 端点
  `POST /api/admin/table-enabled`（只 UPDATE 不 INSERT，同「管理面不创造文档行」）。
- **drift 同形守卫**（计划点名）：`disabled_tables_are_filtered_on_every_recall_path`
  扫 schema.rs 每处 `FROM meta.table_doc` 后 3 行必须有 `enabled`，≥3 处防空转。
  顺手修了一个老判据的锚（`WHERE embedding IS NOT NULL` → 咬 `embedding IS NOT NULL`
  不咬整句 —— 谓词前缀变了它不该红）。
- 实测：停用 → enabled 查询为空 → 复启恢复（254 张启用表）。误采的业务表从此
  不用改 Rust 规则下线。

### AX51 · A21 复合指标进 `normalize_agg`（AggCorrector 对复合指标不再是死的）
- `parse_agg_rules`（多规则版）：`agg_expr` 里**全部**聚合抽成规则 —— 客单价
  `SUM(total_amount) / NULLIF(COUNT(DISTINCT sales_order_code), 0)` 抽出两条；
  保守面与 `normalize_agg` 同一条：**不进子查询**（ship_net_sales/退款占比 那类
  复合子查询口径整体跳过，抽规则就是误抽）、COUNT(*)/非标识符入参不产规则。
- `parse_agg_rule`（单形态入口）委托它、恰好一条才给 —— 对外语义一字不变。
  `correct_agg` 换多规则入口，by_col 歧义守卫原样。
- 实测（单测）：除法里的 `COUNT(code)` 补 DISTINCT 到位、已占用的 `SUM` 不被改名
  （occupied 只管函数归一那一支）。递归指标引用**不做**（计划原话：真出现复用再说）。

### AX52 · A22 组件级评测的第一刀：红题附 `diff=` 分类列
- 计划的全形是 Spider 式 acc·rec·F1 + 难度分档；落地只取**今天就有用的那一刀**：
  红题的 detail 追加 `diff=where|group|agg|select`（首个不一致组件，顺序报）。
  此前「数不对」要人工对两条 SQL 逐行看；现在第一现场直接给出。
- **启发式声明写死在文档里**（字符串切分不是 AST，嵌套混层可接受）：它是排查
  提示，判红判绿仍由结果集比对定 —— 判据只钉「顺序报」与「不误报」五个形态，
  不钉完备性。acc·rec·F1 那套等真有「错在哪一层」的统计需求再补（今天样本太少，
  每层一两题，F1 没意义）。

### AX53 · A23 HITL edit 一档：闸门一步没宽，投毒对策不设后门
- `POST /api/admin/sql-edit`：管理员改 SQL → `dms_agent::gate`（与线上同一条，
  含只读红线/权限注入/LIMIT）→ 同一条只读取数通道 → `exemplar::save`（pending）
  → `review_exemplar` 判词照过。只做 edit（deepagents 四档里唯一今天有价值的；
  approve/reject 语料面早就有）。
- **实测全链**：DML 被闸门 422 拒（「只允许 SELECT」）；改后的 SELECT 执行
  （闸门补 LIMIT 200 = 生产行为）；沉淀 pending → 复核判 POSITIVE → enabled →
  用 B6 的批量复核端点清掉测试语料（工具链自洽）。
- 计划的「条件中断谓词」（deepagents `when`）真做全档 HITL 时顺手，不单独立项。

### AX54 · FIN04 的「条件放 rn 之前还是之后」：快照表行级条件必须在外层
- **病灶**（A22 的 `diff=where` 第一天就回本）：模型把 `balance > 0` 写进
  ROW_NUMBER **子查询里**（先过滤再取最新），gold 在外层（先取最新一行、再判它正不正）。
  语义差：最新行是 0/负但更早有正余额的客户被前者多算 —— 29≠24 整整 5 个。
  而表头 ⚠️ 注释恰好没写这条，模型还是照注释字面做的（partition 两键那半它照抄对了）。
- **修法**：`t_customer_balance` 的 ⚠️ 补「行级条件必须放在 rn=1 之后的外层 —
  放进子查询＝拿到过期快照（实测多算 5 个客户 29≠24）」。实测：`llm+repair`
  24 行与 gold 一致。
- **教训**：快照表的「取最新」是**两段式**（先 rn 后判），任何一段反过来都是错数。
  `RequireLatest` 判据今天只看 ROW_NUMBER+rn=1 存在性，不管条件位置 ——
  条件位置靠 pitfall 提示（判据化要 AST 判「条件在 rn 外层」，误伤面先记着）。

### AX55 · 月末日三件「失败」全是日历伪影（设计内行为，不是回归）
2026-08-01 全量回归 58/3，三件逐一查实：
1. **A01 缺「较上月」**：`prev_window("本月")` 的上期是 `>= 上月初 AND < CURDATE()-1MONTH`，
   每月 1 号它是**空区间**（「今天平移一期」互比设计，7-02 起正常）——
   prev 0 行 → 环比按设计跳过（月初第一天打 -99.9% chip 才是噪声）。
2. **B01 chart=pie≠bar**：8 月 1 号有销量的省份个位数 → 饼图阈值命中（呈现层对数据的
   正确选择，7 月 20+ 省份时才是 bar）。
3. **R-D01 城市经理销售额=0**：8 月 1 号该经理尚无单（数据事实）。
   三件都会在 8 月数据累积后自然转绿，**不改代码、不改判据**（为月首改判据是
   把尺子掰弯迁就一天的数据）。同晚 E05/E11 各一次方差（重跑即过）。
- 教训已内建：评测/回归连跑时**不许**两器并发（MySQL 负载互相污染 + B10 超时族），
  本轮 A21 评测被中途重建污染一次（58/5 假红，串行重跑 61/61）。

### AX58 · 历史默认发货口径（2026-08-01，已由 AX97 取代）
- 该版本曾以 `ship_sql` 正反双分支驱动默认销售额，并据此重写过 eval/regression/golden。
- AX97 已完整废止这组默认合同：所有现行销售经营金标改读
  `sales_dw.dws_off_offline_sale_dfn`，旧双分支只可作为历史溯源，不得重新接回默认指标。
- 当时发现的通用问题（零行结果补列、提示反例被模型照抄）仍有效，但不构成旧销售口径的保留理由。

### AX59 · 裸名称实体总览卡（业主裁决形态）+ SALE17 的三层逃逸史
- **实体卡**（Router 第四位，doc 后 cache 前）：只发客户名/商品名 → 总览卡而不是反问。
  客户：销售额（发货口径）/订单数/信控余额 + 最近 5 单 + 候选 chips；
  商品：分类·品牌/销量/销售额 + 最近 5 单。KPI 标签跟时间谓词走
  （有时间词按窗口、没有写「累计（全期）」—— 实测不带时间词时「本月」标签在撒谎）。
  发货口径 SQL 与 `direct.rs::ship_sql` 同源（判据钉着防两处真相源）。
  实测（业主截图原题）：`线下-浏阳品元商贸有限公司` → 实体卡
  （销售额 1,253,881.20 / 订单数 75 / 信控余额 0 + 最近 5 单）；
  `可颂香肠卷` → 商品卡（分类·品牌 + 累计销量 10 / 3640）。
- **UI**：need-intent 与 entity-card 的气泡不再叠「未找到数据」通用提示
  （那不是取数结果，截图 tp/b39c9a32 的误读现场）。
- **SALE17 的三层逃逸与收口**：模型把省份写成 `LIKE '%湖南%'`（码列必 0 行）——
  ① 值卡劝两次没拦住（卡片管劝不管判）；② 新判据 `RequireCodeEq`
  （码列上的名称写法 = 必返 0 行，可证、不依赖字典完整性，seed 批次也收）；
  ③ 模型**逃逸到 `receiver_province`**（实测也是码列）继续 LIKE ——
  同一本 34 省字典补登记到 `t_sales_order.receiver_province`，逃逸列也判。
  判据开火 → 回炉 → 终于写出 `receiver_province = '430000'`。
  配套：gather 的规则召回面 = 召回表 ∪ JOIN 对面表（t_customer 这类值问题的
  目标表常只在对面集合里），规则数 17→33（join 一跳有界）。
- **回归 58/3**（全是月首伪影）：SALE17/E16/实体卡全绿，Router 六成员契约钉死。

### AX56 · 双供应商热切换（qwen/deepseek）+ 视觉能力按供应商兼容 + `/#/settings`
- **供应商目录**（`db::provider_catalog`）：base_url/模型名/视觉能力是**供应商事实**
  内建两行；**key 只在 settings.json**（新键 `llm_keys{供应商: key}`，老键
  `llm_api_key` 对文件供应商兜底）—— 不入库、不进日志、不进任何响应（红线同 DSN）。
  切到另一家时 base_url/模型名/extra 全来自目录（防「deepseek 地址配千问模型」混搭，
  判据钉着：extra 绝不跨供应商带）。
- **热生效**：`LlmClient` 持 `Arc<RwLock<Conf>>`，每次调用现读快照；保存端点先
  `set_conf` 再落 `meta.kv['llm_provider']`（热改失败不留一条下次起不来的记录）；
  启动读 kv 应用一次。forbidden 键（messages/model）运行时返回 Err 且**旧配置完整保留**
  （不许切一半，有判据）。
- **视觉兼容**：`Conf.vision: Option<模型名>` —— 千问 flash 自己就是视觉模型
  （实测 988ms 三题全对），DeepSeek 全系 None。`GET /api/llm/capabilities`（非管理面）
  暴露 `vision` 布尔给前端/企微显隐图片入口；知识库 OCR 在 Python 侧独立
  （QWEN_KEY + tesseract 回退），与切换互不影响。
- **设置页**：`/#/settings`（好记就是需求）+ 顶栏 ⚙ 入口。供应商单选（模型名/视觉
  徽标/key_ready 灰置未配置家）、保存即生效提示、当前生效快照。配置查看
  `GET /api/admin/llm-config` 永远不含 key（只有 key_ready 布尔）。
- **实测**：config 端点目录+生效正确（文件模型 flash/plus 未被目录覆盖）；切 deepseek
  （未配 key）→ 400 指回 `llm_keys`；切 qwen → `hot:true` + kv 落库 + 问数照常。
  CONFIG.md 与 settings.example.json 同步（`deny_unknown_fields` 下注释键不能瞎加 —
  `_llm_note` 那键写进去启动就拒，说明文字只能住 CONFIG.md）。

### AX57 · DeepSeek 思考模式：目录默认**关** —— 温度语义比速度更要紧
- **官方文档事实**（api-docs.deepseek.com 2026-08）：V4（flash/pro）思考模式**默认开启**
  （effort=high），关闭写法 `extra_body: {"thinking": {"type": "disabled"}}`；
  **思考模式下 `temperature`/`top_p` 不生效**（原文「设置了也不会生效」）；
  `reasoning_effort` 三档（low/high/max，pro 自动提一档）。
- **目录默认关的两条理由**：① 温度被静默失效会拆掉本系统三条机制 —— 首轮 0.1
  确定性（金文件/语义缓存）、重试 0.5 分档（0.1 的重试 = 同一个错误再来一遍）、
  SC 投票的样本独立性（N 份相关样本投同一个偏见）；② CoT 每次生成都多一段
  思维链，延迟/token 成倍（千问同族实测 21x 延迟 / 35x token，SQL 质量不变）。
  要开：settings.json `llm_extra_body` 覆盖目录默认（文件供应商路径），代价自负。
- **工程**：`ProviderSpec.extra` 从布尔表改成 JSON 文本（DeepSeek 的思考开关是
  嵌套对象 `{"thinking":{"type":"disabled"}}`，布尔装不下）；模型名同步官方现款
  （deepseek-v4-flash / v4-pro）。判据钉「切换不带千问 extra + 必带 DeepSeek 关思考」。
- 千问的 `enable_thinking:false` 同构早已在（实测 780ms/65tok vs 16626ms/2281tok，
  SQL 质量一样 —— 两家的「思考」对 SQL 生成都不是必需的，口径卡才是正确性的来源）。

### AX60 · S1 artifact 预览地基（datanote 移植第一件）：一张表四个端点一个面板
- **表**：`meta.artifact`（conv_id/kind/title/html/created_by/created_at + conv 与 kind
  两索引）。kind 三值：markdown（服务端渲染）/ report（同渲染，语义留档）/ html（原样存）。
- **端点**（`artifact_api.rs`）：`POST /api/artifact`（admin_only，手工/内部造物；
  分析与日报不走 HTTP 直接调 `save_artifact`）、`/{id}/view`、`/{id}/download`、
  `/list?conv_id=`。view/download/list 全部过**会话归属校验**（resolve_identity →
  conv_owner，i64 解析失败即 403「产物归属异常」——解析不出身份时宁可拒也不放行）。
- **安全栈两层**：服务端 CSP `sandbox allow-scripts`（**无 allow-same-origin** ⇒
  透明源，碰不到宿主页 DOM/Cookie）+ `x-content-type-options: nosniff`；
  前端 iframe 再叠同名 sandbox 属性。markdown 渲染器 `md_to_html` **escape-first**
  （先全文转义再还原始标签），判据钉 `<script>alert(1)</script>` 原样转义 0 裸标签。
- **前端面板**（App.vue，datanote 的 Codex 式分屏）：右侧 `flex:0 0 46%`、拖拽调宽
  （320px~75%）、关闭、下载按钮（同身份查询串，download 端点再校一遍归属）、
  **深链拦截** —— 气泡里 `/api/artifact/N/view` 链接 preventDefault 改开面板不跳页。
- **实测**：create(markdown 含表格/列表/hr/代码/加粗/注入) → view 全要素渲染 +
  CSP/nosniff 头齐 → download 带 content-disposition → list 按 conv 列出 →
  他人 login 访问 403。169 单测绿（err 助手 `impl Display` 不吃 `.into()`，编译器抓的）。
- **架构纪律**：零平行系统 —— 产物就是一张表，预览就是一个面板，S2 分析报表 /
  S5 日报都往这同一张表写，区别只在 kind 与 conv_id（日报 conv_id=''）。

### AX61 · S2 分析报表 artifact 化（datanote CreateArtifactTool 对应物）：零 LLM
- **端点** `POST /api/analysis/report`：素材 = 前端手上那次解读的回声
  （question/sql/columns/rows≤50/row_count/caliber_note/insight + conv_id）。
  **caliber 服务端从 SQL 重算**（`Reading::caliber()`），回传的口径文本一个字都不信；
  insight 是用户自己数据的回声，且 `md_to_html` escape-first，成不了注入。
- **写前校归属**：conv_owner≠login ⇒ 403 —— view 层有校验，但脏数据从源头就不该落。
- **markdown 组装是纯函数**（`report_md`）：告警印在数字之前（同 .caliber-warn 原则）、
  单元格 `|`→全角 / 换行→空格（md_to_html 的表格解析没有转义管道概念，拆表即坏形）、
  截断说明用 row_count 真数。判据全打在这个纯函数上（handler 连库不测）。
- **前端**：解读面板头部「📄 生成报表」→ 成功后立即开右侧预览面板 + 留一张
  `.art-card`（href 走 S1 的深链拦截，可反复点开）。无会话（conv_id 空）先提示再拦截。
- **实测**：report → view 六要素齐（h1/口径/解读粗体/数据表/行数/SQL 围栏）、
  list 按 conv 出 kind=report、他人 conv 写入 403。170 单测绿。

### AX62 · S3 用户选择卡（datanote AskUserTool 对应物）：零后端改动
- **关键认识**：后端早就把卡片三要素备齐了 —— `route=need-intent` 是判别标签、
  `caliber_note` 是问题文本、`view.interact.drill` 是完整问法选项（A17 chips 同构）。
  datanote 的挂起/续跑状态机在这里**不需要**：我们的「挂起」就是气泡停在反问卡、
  「续跑」就是用户点选项发下一条 —— 会话追问机制（rewrite_followup + A17 日期继承）
  天然承担 resume，零新端点零状态机。
- **改动只在两层渲染**：ResultPanel 遇 need-intent 出 `.ask-card`（主色底 —
  澄清不是报错，不用 error 红；此前那串解释文字被塞进 `.caliber-warn` 红横幅，
  语气全错），新增 `pick` 事件原样发送选项（`drill` 事件会拼「按X」词，不能复用）；
  App.vue 两处 ResultPanel 接线 + CSS。
- **实测**：`叽里呱啦`（intent=data）→ route=need-intent + 4 条完整问法选项 +
  note 齐。knowledge 路会抢在反问前接走百科型问句（实测「咕噜咕噜丸子」走了 KB），
  那是 Router 既有语义，不是本卡的问题。

### AX63 · S4 经验复盘（datanote AiMemoryService 精简版）：蒸馏零 LLM、召回三因子重排
- **蒸馏（learn）**：口径/执行回炉**成功**（route=llm+repair 且有行）→ 异步沉淀
  `kind=review` 经验到 `meta.memory`。**零 LLM** —— 修正版 SQL 本身就是教材，再花一次
  模型调用改写它只会引入新错。素材用 `candidate`（闸门前原文），**不用 wire()**：
  经验是 ds 级共享的，行级权限条件写进去就是跨用户泄漏（与语料同一条防线，判据钉）。
  去重键 (ds_id, kind, question)；content 截 400 字；embedding 留 NULL 由 A9 自愈补
  （MetaVecTarget 第五类 Memory，配方=content 原文，ds 限定，判据同四类旧目标）。
- **召回（recall）**：向量近邻粗排 10 条 → Rust 纯函数重排
  `sim × (1 + 0.1·ln(1+hit)) × exp(-age/30天)`（datanote vector+hitCount+recency 同构；
  实测确认 20 次印证赢 +0.1 sim 差 —— 那是设计不是 bug，判据照实钉）→ 前 3 进
  prompt「经验复盘（参考，不是硬约束）」段（教训段之后，权重序即段序），命中行异步
  hit_count+1。**经验绝不进口径判据与闸门**；预算超限第一刀就丢它（信任级最低）。
- **实测两连抓（都是列类型解码，单测抓不到、连库才现形）**：`hit_count` INT4 vs i64、
  `EXTRACT` NUMERIC vs f64 —— 各加显式 `::bigint`/`::float8`，锚点判据钉 SELECT 形状。
- **端到端实测**：问「河北省销售额」→ llm+repair 成功 → 蒸馏行自动落库 → embed_fill
  补向量 → 再问相似句 → 3 行 hit_count 全涨到 2。自匹配家族第七次（漂移守卫扫到
  测试断言里的 `FROM meta.memory` 字面量 —— concat! 拼锚，注释里也不许裸写）。

### AX64 · S5 经营日报（datanote DailyDigestScheduler 对应物）：口径零第二份
- **调度**：与 A9 向量自愈同一个模子 —— 启动即查 + 10 分钟一轮 + `pg_try_advisory_lock`
  同连接解锁（多实例只跑一个，判据同形钉着）+ `meta.kv['digest_date']` CAS 标记
  （**出成功了才写**，失败下轮重试；已出短路在拿锁之前，常态零锁开销）。
- **🔴 口径零第二份**：全部销售数字由 `direct::ship_sql_with` 填 `?` 占位时间谓词出
  SQL（与问数发货口径同一构造函数），Box::leak 成 `&'static` 走字面量通道 bind
  （`FixedStmt` 只收静态串 —— 架构门禁不许 server 出现动态 SQL）。日趋势是
  `ShipDim` 的日粒度新字面量；订单数/退货额两条独立语句的关键片段与 ship 形态
  由判据钉一致。值列外层包 `CAST(… AS DOUBLE)`（S4 在 PG 被 INT4/NUMERIC 解码
  连抓两次，同类坑先堵）。
- **报告日相对口径**：月累计 = **昨天**所在月的 1 号起 —— 1 号出日报时昨天在上月，
  用 today 月首算出恒 0 空窗（首轮实测 MTD=0.00，AI 点评还得替它解释）。
  数字交叉验证：7 月累计 213,043,523.77 ≈ 全期口径 206M（截 7/30）+ 日均 6.9M。
- **AI 经营点评**（fast 档一次，可缺席）：素材全系统自算（KPI 表喂 `Reading::insight`），
  失败/开关关 = 该段显示「本次没有」，日报照常出。
- **产出与入口**：`meta.artifact`（conv_id='' 全员可见，kind=report，created_by=
  daily-digest）；侧栏「经营日报」块（list 端点不传 conv_id 就只给日报 —— 归属
  校验天然豁免空 conv），点击开 S1 的右侧沙箱面板。
- **已知未修（记给业主）**：TOP5 省份显示码值（430000）而非省名 —— 那是
  `ship_dim(Province)` 在**问数路径**的既有行为（t_customer.province 存码），
  日报与它保持一致；要改名得在 ship_dim 层接 value_map 词典，影响面超出 S5。

### AX65 · 精简/深度双模式（业主 2026-08-01 要求）：深度 = SC 生成 + Precise 解读 + 自动丰满链
- **形状**：输入栏 segmented「精简|深度」（localStorage 持久化）。精简 = 现状一字不改
  （`mode`/`deep` 全部 serde default，老前端与判官 body 零改动 —— 判据钉）。
- **深度模式三段**：
  ① 生成侧 `AskReq.mode="deep"` → `sc_samples.max(3)`（max 不是 overwrite，配置更高
  不拉低，判据钉）。实测：llm 路 SC 两票达成多数派提前收工（llm_calls=2 是**正确**
  的提前收工，不是没接线 —— 日志 `SC 提前收工 samples=2` 实证）；
  ② 解读侧 `AnalysisReq.deep=true` → `Reading::insight_deep`：**Precise 档** +
  四段式（结论/关键发现/口径与可信度/建议，标题逐字有判据）+ 素材 15 行（精简 5 行）；
  ③ 前端自动丰满链：ask 完成 → 自动深度解读 → 自动 `saveReport`（S2）→ 自动开
  右侧预览面板。复合问（subs）v1 跳过（每个子问各烧一次 Precise 太贵，且哪份面板
  该开说不清）；0 行/知识库/need-intent 不触发（没东西可解读）。
- **降级语义不变**：深度解读与精简同一条路（调用失败/空串/含网址 → None），
  「没有解读」≠「取数失败」。超时分档：深度 180s（SC×3 + 解读两份等待），精简 100s。
- **实测抓**：DeepSeek V4 把 markdown 换行输出成字面 `\n`（含真换行与字面**混合**
  的形态）—— `unescape_newlines` 字面 ≥2 处即整体还原（解读散文里字面 `\n` 几乎
  不可能是合法内容；孤立一处按引述保留），精简/深度两路共用 `guarded` 都受益。
- 深度解读回声进 S2 报表的「AI 解读」段（md_to_html 的 ##/-/​** 渲染面与四段式
  天然兼容 —— 提示词标题就是按那个渲染面定的）。

### AX66 · datanote ChartTool 的 artifact 面：手绘 inline SVG，零依赖
- **盘点结论**：ChartTool 我们早有半边 —— `Block::Chart`（present.rs 角色选型
  bar/line/pie + series）+ BiChart.vue（ECharts）在聊天气泡里。真正的缺口是
  **artifact 页里没有图**。planCard 跳（要流式执行才有「实时计划」，subs 列表事后
  已等价）；ApprovalGate 跳（只读系统运行时无可批件，HITL exemplar 复核已是同构）；
  radar 跳（YAGNI）。
- **为什么手绘 SVG 而不是引库**：产物页跑在 `sandbox allow-scripts`（无
  allow-same-origin）且 page_shell 纪律是「零外部资源可达」（离线部署 + 单文件
  分享自洽）—— ECharts CDN 两条都撞。纯 SVG 连脚本都不用，沙箱里只是静止图形。
- **`chart_svg.rs`**：bar（横条 + 负值零线 + top 并「其他」）、line（series 列分组
  首见序、缺测**断开不连线**——连成 0 是编造数据、单点出点不出线）、pie donut
  （负值切片丢弃 + >5 并「其他」）。单色明度系与 BiChart 同组；标签一律 escape
  （注入样例判据钉）；退化输入一律空串（缺图不许塌报表）。
- **接入两条产线同一机制**：markdown 里放 `⟦CHART:n⟧` 占位符（生僻括号，数据撞不上，
  md_to_html 的 inline 不动它 —— 判据钉 survives），渲染后 `fill_charts` 按号换 SVG。
  S2 报表收 `charts` 回声（只是**下标与图型**，数据服务端用 columns/rows 自取）；
  S5 日报三张图规格+数据全在服务端（趋势折线 + TOP5 省份/分类柱图，图先表后）。
- **实测**：S2 报表含 bar SVG（100.0万 紧凑格式 + 品牌名）；日报重出 3 figure /
  1 polyline / 10 bar rect / 0 占位残留。两处测试预期错被渲染器纠正（单点序列
  只出点不出线、空段连占位符都不产）—— 判据照实改。685 全绿。

### AX67 · 省份码值解码（AX64 记给业主的尾巴）：SQL 层 JOIN t_regions，不是显示层
- **探源**：`t_customer.province` 存码（'430000'）；`t_dict_value` 无地区字典，
  但 **`t_regions`（region_code UNI / region_name / region_level / deleted_flag）
  就是地区码表**，region_level=1 省级 35 行。码↔名 1:1 + UNI 不扇出 ⇒ 分组基数与
  聚合值**一个字不动**，只是标签列从码换成名。
- **为什么 SQL 层而不是显示层**：显示层解码（present/connector 后处理）要回答
  「哪一列是哪个表的哪个码列」—— 别名到表列的映射链根本不存在，启发式匹配
  （「80% 像码就解码」）是把错显示变成静默错数据的新入口。SQL 层 JOIN 是
  确定性的，且 ship 模板实测就是这些题的真实落点（B01/B08/B09 金文件为证）。
- **落点**：`ship_dim(Province)` 的 pos/neg 两支各加
  `LEFT JOIN t_regions rg ON rg.region_code = cus.province AND rg.region_level=1 AND rg.deleted_flag=0`，
  expr `COALESCE(rg.region_name, NULLIF(cus.province,''),'未知')`
  （字典查不到的脏码回退原码 —— 比「未知」诚实）。**t_regions 登记 scope_binding
  global**（builtin 32→33 表：不登记 = 受限用户的省份维度查询被 fail-closed 整批拒，
  与 t_dict_value 同一条；测试数字与注释同步）。
- **金文件**：B01/B08/B09 三份 re-bless（SQL 文本变，数值断言不动）。
- **实测**：「销售额前五省份」→ 未知/湖南省/广东省/江苏省/山东省（原为 码），
  数值形态不变。compose/LLM 路径的省份列仍出码（维度声明 expr 的解码要动
  注册表 + join 图，影响面超本轮，记录在案）。
- **回归归因（61 题 57 过 4 红，全与本次改动无关）**：A01「较上月」缺失 =
  8/1 月初伪影（prev 同窗 `[07-01,07-01)` 空 → 上期为 0 不填 delta，`patch_kpi_delta`
  的 0 判据钉着的设计内行为）；B01/B08 `view0=table` = 本月空窗 0 行 chart 缺席
  （AX55 家族再现实）；E17 进程非 0 = 重查询超时抖动（B10 家族，单跑 43.6s 过）。
  数值断言零漂移（B01/B08/B09 fragment 数值部分全过）。日报重出 TOP5 全出省名、
  码值零残留。
- **教训（第三次）**：后台跑判官**不许 `| tail -N`** —— 管道吃掉 exit code（4 红报
  exit 0）还截掉失败清单，白跑一趟 35 分钟。判官输出一律全量落文件。
- **顺手两件**：builtin.rs 头注的「逐行搬自 inject.rs」改为维护点说明（源头文件
  已删，注释指路到鬼）；`rules.rs` 的 32→33 契约数同步。

### AX68 · E17 收口：`ShipDim::CustomerClass` 确定性模板（43s 抖动路 → 272ms 确定性路）
- **动因（准确性+智能性）**：「销售额按客户分类」在口径切发货后走 LLM（装配器不收
  复合 agg）—— 43.6s/次、SC 样本撞 30s EXEC_TIMEOUT 就进程非 0（回归 E17 实测红），
  且每次重新生成 = 答案形状抖。而维度声明里 CASE 翻名是现成的（CustClassif 码列，
  seed_defs 已坐实 04 线下客户占 96%）。
- **落法**：SalesDim 第九变体。`detect_sales_dim` 的「客户分类→None（回落 LLM）」
  护栏**升级成模板**（护栏的本意是防被「分类」劫到商品分类 —— 有自己的变体后
  劫道消失）。CASE 字面量与 `meta.dimension customer_class` 声明同一份，
  **twin 钉**（direct.rs 与 seed_defs.rs 两侧各一条判据，漂移必有一边红）。
  过滤侧不吃：「线下客户的销售额按省份」残留「线下客户」照样回落 LLM（判据钉）。
- **实测**：「本月销售额按客户分类」direct-agg 272ms（原 llm+repair 43,581ms，
  **160×**）；「上月…」direct-agg 12.3s 得 213,043,523.77 —— **与日报 7 月 MTD
  分毫不差**（96% 线下客户≈全部，口径自洽铁证）。回归 E17 改钉 direct-agg +
  CASE 片段（回落即红）。
- **全期无时间词版**：模板接住但全期 UNION × 客户 JOIN 撞 30s EXEC_TIMEOUT →
  回落 LLM（B10 族，已知权衡：索引/预聚合/超时都动生产，归 DMS 团队裁决）。
  回落答的 SQL 经验口径正确（UNION 双支 + 退货 + 线下销售单过滤都在）。
- **E17 回归用例同步**：route llm/llm+repair 两态 → 钉 `direct-agg`（确定性路径
  回落即红的同一条口径，与 E16/SALE17 一致）。

### AX69 · 单号全族单据卡 + 名词五卡（业主 2026-08-02「准确性」点名的两类）
- **单号侧（direct-doc）**：真库逐表 `DISTINCT SUBSTRING` 探得七族前缀（不是按命名
  习惯猜）：销售 HJXH-DXO/DSO、售后 HJXH-DRO、对账 HJXH-DZD、需求 HJXH_XQ
  （**下划线变体** —— 旧字符集闸把它整个拒了）、开票 IO+8位、新开票 SQ+8位、
  采购调拨 CG+8位（SPC- 保留）。短码族的数字门槛（≥6 位纯数字）防英文词撞前缀
  （判据钉 INFOABC/SQLEET）。
- **单据卡 = 头 + 明细**（业主原话「对应的表和明细表」）：`DirectHit` 加 `detail`
  字段 —— 头行 SELECT * LIMIT 1 出 Entity 键值卡（68 列），落地时主表换**明细行**
  （CSV/AI 解读拿的也是明细），视图 = [Entity(头), Table(明细)]。一切明细失败
  （闸门拒/执行错/0 行）= 保留头卡不塌（判据钉）；明细号列全部 `SHOW COLUMNS`
  坐实。零 Router/serde 变化（`detail` 是结构字段不是 JSON 字段）。
- **名词侧（entity-card）**：客户/商品两卡 → **五卡**（+品牌/门店/业务员）。
  新卡全部复用 ship_net_sql 实体过滤形态：品牌 `ds.brand_name`（正向已 JOIN 主数据）
  + `e.sku_code` 子查询（负向）、门店 `a/o2.shop_name`、业务员 `a/o2.owner_manager`
  （售后头表没有 owner 列 —— 负向经 o2 到原单，判据钉，自匹配家族第九次 concat!）。
- **重名消歧三层**（实测三案驱动）：① 精确闸 —— 整词等于品牌/员工时精确赢
  （「饱饱博士」本是商品名子串、「平安」本是客户名子串 + 43k 单业务员）；② 显式
  前缀路由 —— 「业务员平安」「品牌饱饱博士」是用户自己的消歧句（bare_name 剥词表
  与 prefix_hint 表同源，判据钉一致）；③ 卡内 ORDER BY 精确行优先。
- **实测**：七族单号全真码过（DXO 68 pairs + 明细 2 行 / DZD 明细 13 行 / XQ 头卡）、
  六名全对（饱饱博士→品牌、平安→业务员、皇家小虎→品牌 296 品、可颂香肠卷→商品
  不变、青海西宁民和路店→门店）。691 全绿。

### AX70 · 省份解码 compose/LLM 半收口（AX67 留的缺口）+ region_level IN (1,2)
- **落法**：`meta.dimension` 省份声明的 source 加
  `LEFT JOIN t_regions rg ON rg.region_code = cus.province AND rg.region_level IN (1,2) AND rg.deleted_flag=0`，
  expr `COALESCE(rg.region_name, NULLIF(cus.province,''),'未知')` —— 与 ship 模板
  **同一形态**（口径只有一份，显示只有一处解码）。compose 与 LLM 卡片（照抄声明）
  两路同时解码。种子是 boot upsert（`source_table/expr` 都覆盖），重启即生效。
- **实测抓**：`region_level=1` 省级时直辖市辖区码（110100 北京市辖区 / 500100）
  漏出原码 —— 放宽 `IN (1,2)`（region_code UNI，一码一名，无扇出无歧义），
  三处（ship 双支 + 声明）一起改，判据锚同步。
- **验证**：「今年各月各省销售额」compose 直出 云南省/内蒙古自治区/北京市/山东省…
  **零码残留**。回归 61/61 全绿（AX68/AX69 零回归 + 月初伪影自然消失 + E17 新钉
  direct-agg 过）。
- **顺带结论**：compose 对「销售额×省份」仍拒（复合 agg 含 SELECT），硬编码
  sales_breakdown 接 —— 那是 AX58 以来的既定分工，省份声明的消费者主要是 LLM 路。

### AX71 · 图数据库完善（业主要）：装配器 LEFT JOIN 前置修复 + 16 条新边 + 图启动补偿
- **前置修复（二·AW 的两条，必须一起）**：装配器路径/桥接 JOIN 一律
  `LEFT JOIN + 被连表口径进 ON`（`left_join()` 统一出口）；`scope_parts` 循环靠
  `caliber_in_on` 跳过 ON 里已带口径的表。旧形态：INNER + 口径进 WHERE ⇒ 售后单的
  原单作废时售后单整行丢（实测少 13 单）；② 维度声明的 LEFT JOIN 被 WHERE 里重复
  的口径打回 INNER（现役路径同形，碰巧无害只因为软删客户今年下过 0 单）。
  判据：两处 LEFT/ON/WHERE 形状 + 「口径只出现一次」防退化。
- **16 条新 join_edge（两证制）**：DMS 后端 384 个 mapper XML 提取 79 条真实 JOIN
  （`tools/mine_joins.py`）→ 生产库 COUNT 实测基数（`tools/probe_card.py`，全非扇出
  N:1/1:1）→ 入种子（boot upsert）。域：售后族 3（含二·AW 裁决的那条
  after_sales→sales_order）、主档空间 3（area_manager/balance/warehouse）、活动费族 6、
  票据对账 2、履约设备 2。**21 条 active**。
- **验收**：「今年各省份的售后单数」direct-agg 398ms 出 **20269** = 权威 20073（7/27
  口径）+ 约一周增量（≈28 单/日 × 7 天 ≈ 196，自洽）；省份已翻名（湖南省/广东省），
  「未知」桶 9410 行被 LEFT JOIN 保留（旧 INNER 会整行丢）。
- **AGE 图**：① Region 节点谓词放宽 `region_level IN (1,2)`（与 AX70 问数路径同一组
  名字 —— 图与问数必须一致）；② **启动补偿**（never/fail 即先补一轮，不再等凌晨 3 点；
  电脑重启后 status 回 never，购买边可能是上周的）—— 实测重启后自动补：
  2616 customers / 456 goods / 101,399 edges。
- **t_regions 张冠李戴注释**：DMS 库原生表注释是复制粘贴错的（「开票申请单」），
  修元数据侧 comment/search_doc + embedding 重生（A9 补）。其余重复注释组全是
  同族真重复（invoice 5 张、_3 副本），无第二处错挂。
- **源码纪律**：xh-dms 三个仓库只读未动一个字节；挖掘只读（mine_joins/probe_card）。
- **回归 61 题 58 过 3 红**：全是 B01/B08/B09「SQL≠金文件」—— AX70 的
  `region_level IN (1,2)` 放宽在上一轮 bless **之后**，金文件还是 `= 1` 旧形
  （数值断言全过，只 SQL 文本差）。三份 re-bless 后复跑全绿。**教训**：金文件
  的 SQL 形状依赖链要记全 —— ship 模板、省份声明、region_level 谓词，改任一
  都要想到那三份。

### AX72 · 深度模式复合页（业主「花里胡哨」裁决）：单入口 `/api/deep/compose`
- **形状**：一次问句 → **总值（KPI 卡）+ AI 深度分析（Precise 四段）+ 维度拆解
  （省份/商品分类，图+表）+ 今年各月趋势（折线+表）+ 最近订单明细 + 口径 + SQL**，
  一页可分享 artifact（datanote 富页形态）。前端深度模式改走单入口，
  不再串 ask+analysis+report 三脚（三个端点三套身份校验，且素材必须出自同一次
  取数 —— 分时取数拼得出自相矛盾的一页）。AI 解读钮在深度模式隐藏（默认做，
  按钮是重复入口）。
- **🔴 口径铁律**：子问全部走 `crate::ask` 同一条管线（闸门/口径/权限/校正零第二份
  真相源）—— 省份/商品分类子问命中 ship 确定性模板（SC=1，不投票）；本模块只做
  「问哪些子问 + 怎么排版」。手工另写拆解 SQL = 第二个口径真相源。
- **拆解门（纯函数判据）**：单值 KPI + 销售额词族 + 无维度词才拆；已是拆解形
  （按省份/前五…）、多行、实体卡、复合一律不拆。其余结果照样出页（主表+视图图
  +AI+SQL）。
- **占位符编号纪律**：`chart_slots` 逐段声明有没有图，与 svgs 下标一一对应 ——
  明细段无图不出占位符（判据钉，错配就是图配错表）。
- **实测**（「上月销售额」conv 12）：KPI 213,043,523.77（与日报 7 月 MTD 分毫不差）
  + AI 四段 + 3 figure（趋势折线 + 两省/分类柱图）+ 8 行最近明细 + 主/子 SQL 全列，
  0 占位残留；route=direct-agg 15.7s。

### AX73 · 业务库热切换（业主要）：内置锁换池，零签名爆炸
- **机制**：`ReadOnlyMySql.pool` 改为 `Arc<RwLock<MySqlPool>>` + `swap_pool(url)` ——
  **先建先验后换**（新池连不上 / 会话 `transaction_read_only≠1` → Err，旧池原样；
  可写库永远进不来，F8 同一条）。`MySqlPool` 本身是 Arc 克隆：在飞查询握旧克隆
  自然收尾，新查询拿新池，**无中断窗口**。`FixedStmt` 改持 pool 克隆（同一保鲜语义）。
  所有 `&st.mysql` 调用点零改动（agent/policy/graph/admin/deep 全透明）。
- **目录与红线**：`settings.mysql_targets{name: DSN}`（`dms` 恒等于 `mysql_url`，
  重名不覆盖 —— 判据钉）。DSN 只在 settings.json：**kv 只存名字**、API 只给
  `mask_dsn` 的 `host:port/db`（口令带 @ 也切得对，判据钉）。`meta.kv['mysql_target']`
  启动应用，失败沿用启动池 warn 留痕（kv 里躺着起不来的配置时服务必须能起来）。
- **端点**（admin）：`GET /api/admin/db-config`（目录+当前+口径声明提示）、
  `POST /api/admin/db-target`（热切换）。health 的 mysql 段带 `target` 名。
  前端设置页加「业务数据库」段（与模型供应商同模子，radio 即存）。
- **实测**：目录脱敏正确；未知目标 400 指回 settings.json；切 dms_copy（同库别名）
  → health/问数照常（7,778,913.88 同一个数）→ 切回 kv 落 'dms'；**不可达目标
  400 且旧池原样**（不许换一半的实证）。697 全绿。
- **边界（已告知业主）**：口径声明（指标/维度/码表/权限档案）按 **DMS schema** 登记，
  切到同构库（中台镜像）照常；切到 schema 不同的库会响亮报错，绝不静默错答。

### AX74 · 页面编辑配置（业主要）：写 settings 文件 + 内存热更新，红线一字不动
- **形状**：凭据**仍只住 settings 文件**（不落 PG、不进日志、不进响应 —— 页面编辑
  与手写文件是同一个信任面）。`settings_api` 初版采用：① 原地写（O_TRUNC 不
  rename —— 单文件 bind mount 的 inode 钉在挂载点上，rename 会写到容器层 = 宿主机
  看不见）；② 写前备份；③ 写前回读 `from_value::<Settings>` 全检（deny_unknown_fields + 类型），
  **写坏的文件不许落盘**，校验过的那份才进 `RwLock<Settings>`（内存与落盘同源）。
- **面**：`GET /api/admin/settings-catalog`（永不含明文：脱敏 host + key_ready 布尔）、
  `POST/DELETE settings/mysql-target`、`POST/DELETE settings/llm-key`（全 admin_only）。
  `AppState.cfg` → `RwLock<Settings>`（克隆快照读取），热切换路径立即用新目录。
- **实测两连抓**：① GET/DELETE 端点身份字段传了 `(&None,&None)` 恒 401（该有
  IdentQuery，目录布尔看起来正常是最坏的那种假绿）；② `.bak` 初版写容器层
  （单文件挂载，宿主不可见 = 没备）。`valid_name` 的 `is_alphanumeric` 收 Unicode
  中文也过（改 ASCII 闸）。自匹配家族第十次（rename 锚 concat!）。
- **实测**：加目标 zt_test（同库别名）→ 宿主机文件即时可见 → 不重启即热切换
  问数照常 → 切回 → 删除文件同步；llm-key 增删同；catalog 永远无明文。
- serve.ps1 的 settings 挂载去掉 `:ro`（页面编辑的可写前提，注释写明改回那天
  页面保存会变成 500）。
- **AX95 安全修订**：上述第②项后来被认定违反“明文凭据只存在正式 settings 文件”的硬边界，
  已删除所有 `.bak` 复制。当前流程是内存构造与全量校验通过后，对正式挂载文件原地单次写入；
  不在 `<KB_ROOT>`、仓库或容器层生成第二份凭据文件。

### AX75 · 设置页全功能（业主要）：CRUD + 测试连通性 + 人性化表单 + 厂商预设
- **测试连通性（只验不写）**：`test-db`（一次性池：连得上 + 会话只读 + SELECT 1，
  回延迟与版本）、`test-llm`（一句 ping 回延迟/片段/用量）。「测不通」回
  `200 {ok:false}` —— 那是测试的答案，不是端点故障（500 会误导前端）。
  实测：真库 879ms/MySQL 8.0.28；DeepSeek 894ms 回「正常」；坏 key 干净报错。
- **DB 结构化表单**：类型（MySQL，PG 灰置注明）/地址/端口/库名/账号/密码，
  前端拼 DSN（`encodeURIComponent` 账密）。raw DSN 不再要求人会写。
- **LLM 厂商预设**（`llm_presets()`，2026-08 互联网核实，OpenAI 兼容端点）：
  千问 / DeepSeek / 智谱 GLM / Kimi / 豆包（火山方舟）/ OpenAI —— 下拉即填
  url/双模型/思考档/多模态模型名，用户只填 key。**页面没有第二份目录**
  （catalog 端点直接给预设形状，判据钉六家齐全）。
- **思考级别**（`thinking_extra`）：off/low/high/none → 各家的关法（千问
  `enable_thinking:false`、DeepSeek `reasoning_effort`、豆包 `thinking.disabled`）。
  没有的档位**报错不静默**（不许写一个「看起来开了实际没开」的配置，判据钉）。
- **自定义供应商存储**：`settings.llm_providers{name: {base_url, 双模型, extra_body,
  vision}}`（key 仍住 `llm_keys`）。`resolve_provider` 顺序：内建目录 → 自定义 →
  文件值。`llm_config` 切换列表带自定义（保存当场可选）。删除连带清 key
  （供应商没了，key 是死凭据）。内建两家不可改不可删（改 key 即可）。
- **实测**：加 kimi → 文件/切换列表/热切换全通 → 切回 deepseek → 删除连带清 key。
  701 全绿。

### AX76 · 设置页可修改 + 样式重做（业主两条）：凭据不出服务端的编辑流
- **修改（编辑）**：每条目标/供应商有「修改」钮 → 表单回填非凭据字段（host/端口/
  库名/url/模型/思考档/多模态）→ 保存覆盖。**凭据全程不出服务端**：
  ① DB 密码留空 = `keep_secret`，服务端把旧 DSN 的 userinfo 拼进新 DSN
  （`splice_userinfo`，旧 DSN 没有账号段就报错让补填）；
  ② LLM key 留空 = 保留 `llm_keys` 里已存的（后端本就不覆盖）。
- **dms 可改**：改的是 `mysql_url`（默认启动池，写坏 = 下次起不来）——
  **强制先过连通性**（test_pool：连得上 + 只读），过了才写；当前正在 dms 上
  立即 swap_pool 热应用（实测：同值改写热应用、坏地址 400 且文件未动）。
- **自定义覆盖内建**：`llm_providers` 同名条目**优先于**内建目录（内建形状是
  代码常量页面改不了；覆盖即生效、删除即还原 —— 判据钉覆盖赢内建、key 仍取
  llm_keys、单边模型名互补、未知仍响亮报错）。
- **样式重做**：设置页双卡片（左色条标题 + 副标题）、目标列表行（状态点 +
  名称/标签 + 等宽 host + 修改/删除/切换按钮组）、四列栅格表单（label 在上）、
  测试结果条（ok/bad 两色）、key chips。不再是一排裸 input。

### AX76a · 事故与修法：删覆盖条连带清 key 误杀内建 key（业主报「切换不了」）
- **现场**：业主在设置页切不了模型 —— deepseek `key_ready=False`（切换钮被
  disable）。根因是 AX75 的「del_llm_provider 顺带清 key」：删除**内建同名的
  覆盖条**时把 `llm_keys['deepseek']` 也清了 —— 内建供应商还在用那个 key。
  （我自己在验收覆盖功能时删覆盖触发的 —— 验收路径没覆盖到「key 是否还在」，
  而页面切不了模型正是它的下游。）
- **修法**：连带清 key **只在删真·自定义供应商时**（key 成了死凭据）；
  删覆盖条（内建还原）不动 key。判据 `del_override_never_touches_builtin_key`
  钉住清 key 分支必须在内建判定反面。key 已从备份恢复，deepseek key_ready=True。
- **顺手验证**：dms_uat（业主自己加的目标）切换/切回全通，问数无恙。
- **教训归口**：凡是「顺带清理」类逻辑，先问一句「被清理的东西还有谁在用」——
  这条与「删除假成功」（F8）是同族的反面：**清理过度**。

### AX77 · 深度模式 v2 推倒重来（业主裁决）+ artifact 分享
- **LLM 当分析师**（datanote PLAN→EXECUTE→FINAL 形态）：`plan_report` 读注册表
  指标/维度目录 → Precise 出结构化报表计划（JSON：sections 2~4，每板块
  自然语言子问 + 图型 + 标题）→ 板块子问并发走**同一 ask 管线**（口径/闸门/权限
  零第二份）→ insight_deep 全文分析 → `bi_page` 渲染。计划任一环节失败
  （目录读失败/JSON 坏/校验不过）→ 回退 v1 启发式三件套，不挡主流程。
- **计划校验是命门**（纯函数判据）：sections 1..=4、question 2..60 字、
  chart ∈ {bar,line,pie}、空 title 用 question 顶；括号配平挖 JSON（模型爱包话）。
- **优美 BI 页**（`bi_page` 直接拼 HTML，不走 markdown）：头部（标题+时间+badge）→
  KPI 卡行 → AI 深度分析（bi-ai 高亮卡）→ 板块卡（图+表）→ 明细 → 口径 →
  SQL 折叠附录。单元格全 escape（判据钉）；BASE_CSS 加 bi-head/bi-sec/bi-ai
  （S2 报表/日报同步受益）。
- **实测**：「上月销售额」→ LLM 自出三板块「区域销售贡献排行 / 品类销售结构分析 /
  日度销售走势监控」（比写死的 省份/分类/趋势 丰富且切题），3 图（1 折线 2 柱）
  + AI 四段 + 8 明细 + 折叠 SQL。
- **分享**：`share_token`（uuid 即能力，只授读）：share/unshare（属主校验复用
  load()，已有 token 不轮换）+ `GET /api/artifact/shared/{token}`（免登录，
  同沙箱头，uuid 形状闸）。实测：发链接 200/CSP 头齐/非 uuid 404/撤销 404。
  前端：预览面板头 🔗 分享（复制链接）+ 产物卡 🔗。
- **前端身份修法（附）**：切换按钮的 POST 把 login_name 放 query，后端只从 body
  读身份 → 恒 401（「切换不了」的真根因之一）；body 已补。

### AX77a · 生产授权被撤后的连锁（2026-08-03）：判官与服务同管道的两条新证据
- **现场**：dms_ai@'%' 的 `xh_dms` SELECT 授权**白天被撤**（业主侧/DBA 侧动作），
  连锁炸出系统两个「同管道」缺口：
  ① **CLI 不吃 kv**（判官 regression 恒连 `mysql_url`）—— target 已切 uat，
  回归 0/61 全场 Access denied（**那场不是回归，是打错了库**）；
  ② **启动池握手死**（MySQL 握手带默认库，授权一撤池建都建不起来）——
  kv 指着 uat 也救不了，因为 dms 池先死在握手。
- **修法（一条管道）**：`dms_source` 先按 kv 解析**启动 DSN**（`db_boot_url`）再直接
  建那个池 —— serve/CLI 七个调用点全部同一条路；kv 目标连不上才回退 `mysql_url`
  （回退也死就让进程响亮死，不许静默换库）。重复换池的 `apply_runtime_db_target` 删除。
- **判据回归现状**：uat（root@167.10，会话级只读照旧强制）上重跑全量 —
  结果以 route/SQL 金文件为准，数值断言差异归 uat 数据差（逐类列出）。
- **业主待办**：① DBA 给 `dms_ai@'%'` 补 `GRANT SELECT ON xh_dms.*`（恢复生产）；
  ② uat 也建议换只读账号（root 进配置是更大的口子；会话只读已强制，但宽账号
  本身是风险）。

### AX78 · 46 未归属枚举收口（AX7 的数据驱动对拍落地）：69 行 dict 首开火
- **分析器**（`tools/enum_ownership.py`）：91 个 Java 枚举（含**整数码形态**
  `SMALL(1, "小型活动")` —— 首版正则只吃引号串，漏了 ActivityLevelEnum 一族）×
  1268 条未登记候选列 → cov 对拍（分母是**列**取值）+ 词干关联。
- **抓到的三个真坑**：① maps 构造 bug（name 同时进了 code 位，第一版输出
  `['活动临促人员费用','活动临促人员费用']` —— 下游全错）；② `activity_level`
  被 `ActivityFeeTypeEnum` 误归（两枚举同码 1-4，短码空间巧合 —— AX6 家族的
  现实版），词干打分决胜（列名 token 全中者赢）归正到 `ActivityLevelEnum`；
  ③ `balance_status` 0.833 撞进 `ZtCustomerBalanceLogAmountTypeEnum`
  （语义完全不对，只是码重合）—— **0.8~1.0 的 probe 带正是干这个的**：不开火。
- **歧义闸**：同 cov 同分**不自动归属**（4 列打印出来人工裁决，宁缺勿错）。
- **落地**：30 列 cov=1.0 入 `origin='dict'`（剔备份表/probe 4 列），69 行。
  `origin='dict'` 从 0 → 69，`RequireKnownValue` **第一次真开火**（AX7 预警的
  行为变更）。回退开关一句话：`UPDATE meta.value_map SET origin='seed' WHERE origin='dict'`。
- **端到端实证**：「大型活动的费用是多少」→ `am.activity_level = '3'` ✓
  （没登记时模型会写 `'大型活动'` 或 LIKE，恒 0 行）。
- **歧义五列裁决（全部源码证据，2026-08-03）**：push_crmeb_status=CRM EB 推送标志
  （PullCREBShipStatusJob set 语义，**不是** ActivityStatus —— 短码巧合典型）；
  apply_status=InvoiceStatusEnum 全 11 档（DO 注释 + service REJECT 用法）；
  statement_status=核验状态机 1已生成→2已确认→3已回函（ServiceImpl 迁移链）；
  submit_status×2=0未提交/1已提交（WMS 提交守卫）；
  balance_status=账余状态（**t_dict_value dict_key_id=95 权威源**，不是
  ZtCustomerBalanceLogAmountTypeEnum 的码巧合）。dict 69 → 95 行。
- **dict 开火回归验证**：54/61 与落地前逐题全同（3 月初伪影 + 3 tanlibo 不存在），
  **零误伤**。

### AX79 · 交叉维度（月份×维度）：BI 基本件，一条装配器分支
- **缺口**：「今年各月各省销售额」此前被单维 detect 劫成全年单维（答非所问 ——
  用户要的是逐月序列）。交叉是 BI 的基本件，也是「通用 agent」的题中应有之义。
- **落法（一条分支，零新口径）**：`ship_sql_impl` 加 `month` 键（正反两支各在
  SELECT 头插 `DATE_FORMAT(时间列,'%Y-%m') AS m`），交叉形态
  `GROUP BY u.m, u.k ORDER BY u.m, 销售额 DESC` —— 发货口径不变量与单维**同一条**
  （判据逐条钉：item_type='3' / batch_delivery_quantity / group_number /
  upload_time / 线下销售单 / UNION ALL / t_regions 解码）。无字符串替换派生
  （ship_sql_cross = ship_sql_impl(month=true) 的薄封装）。
- **路由（detect_cross_dim，跑在单维前）**：月份**序列词**（各月/每月/按月/月份/
  月度，「本月/上月」是时间窗不算）+ 另一维度词共现 → 交叉；剥词表 = 单维那份 +
  序列词（「各月湖南省的销售额」残留「湖南」照拦）。
- **呈现链零改动**：present.rs 的「time + 恰 1 类别 + 1 指标 → line + series=类别列」
  天生接（月份，维度，销售额）—— 交叉 SQL 的列序就是为这条设计的（判据早已在）。
- **实测**：「今年各月各省销售额」direct-agg 135ms，[月份，省份，销售额]，
  blocks=[line(series=1), table]，省名解码（江苏省/内蒙古自治区/未知）；
  「各月销售额」单维不变（series=None）；「每月销售额按商品分类」176 行同形态。

### AX80 · 分享修复 + 思维过程（Codex 式）+ 问题理解 + 聊天内嵌 BI
- **分享「没反应」真根因（两处）**：① 前端 URL 多叠一道 `replace('?','&')` 把
  `/share?login_name=` 拼成 `/share&login_name=`（没有 `?`，路由不匹配恒 404）；
  ② 错误反馈写进 `llmMsg` —— 它**只在设置页渲染**，聊天页点了当然「没反应」。
  修法：直接用 loginQuery（自带 `?`）+ 轻 toast（右下浮层 3s，聊天/设置通用）。
- **思维过程（Codex 式）**：同步 POST 不能流式 → **内存进度表 + 1.2s 轮询**
  （`/api/deep/progress?rid=`，rid 是前端生成的 uuid，表 10 分钟淘汰；不含数据
  只有阶段名，免身份）。阶段：主查询（SC 投票）→ 读目录 → AI 深度思考（PLAN）→
  问题理解 → N 板块并发 → 撰写分析 → 渲染 → 完成。loading 气泡逐条 ✓/▸。
- **先深度思考再开始**（业主要求）：PLAN 输出加 `understanding`（模型对问题的
  两三句理解分析）—— 进思维步骤、聊天框 🧠 块、BI 页 `.bi-under`（数字之前）。
  实测：「上月销售额」→「用户核心诉求是获取上月销售总额…需要从渠道贡献、
  区域分布及时间趋势三个维度拆解…」。
- **聊天内嵌 BI**：compose 响应加 `page` 载荷（understanding/kpi/insight/
  sections/recent —— 与分享页同源同一份），气泡直接渲染：🧠 理解 → KPI 卡 →
  AI 深度分析（markdown）→ 板块（BiChart + 迷你表 6 行）→ 明细。三列折线
  series=1 与 present 同一条规则。
- **生产恢复**：dms_ai 授权已补（DBA 处理），target 回 dms，graph sync 也绿
  （null-safe 修复生效，60 客户/143 商品/538 边）。

### AX81 · 独立 UI 使用 DMS 账号密码直登；企微身份映射收口；深度口径解析修正
- **独立 UI 登录**：`POST /api/login` 直接按 DMS `SecurityPasswordService` 同一摘要规则
  校验 `t_employee.login_pwd`，并同时校验 `deleted_flag/disabled_flag`。界面只有账号和密码，
  按产品要求不展示验证码；连续失败使用本进程 5 次/5 分钟限流，数据库保持只读。
- **权限同源**：登录后角色仍从 `t_role_employee/t_role` 实时读取；多角色必须显式选择并经
  `/api/session/role` 复核换签，查询范围继续由现有 DMS `Principal -> Scope` 注入。关闭
  `insecure_login_fallback`，不再允许请求体里的裸 `login_name` 充当身份。
- **企微对照结论**：旧 agent-harness 是 `open_userid -> userid -> 花名 -> DMS agent_token
  代理登录`。当前实现优先用企微手机号精确映射 DMS 员工，手机号缺失时才以唯一花名回退；
  重名 fail-closed。角色与数据权限沿同一 Principal 管线，不复制旧项目的高权限 agent_token。
- **深度思考 UI**：实机改为 Codex 式左侧状态/耗时 + 右侧完成步骤/当前步骤，PLAN 的问题
  理解作为明确阶段展示；精简模式保持原有紧凑反馈。
- **深度报表口径修正**：发货 SQL 是外层聚合包住两段 `UNION ALL`，旧口径解析只看最外层
  WHERE，会把真实本月窗口误报成全量历史。`conditions()` 现读取各 UNION 分支过滤；深度
  提示明确辅助趋势可能是独立时间窗，禁止把趋势值与主 KPI 相加、抵消或判定冲突。

## 三、状态
- **10 份 plan 全部落盘**：T1骨架 / T2 kernel纯算法 / T3 newtype闸门 / T4 connector llm+embed / T5 policy / T6 meta DDL+种子 / T7 semantic recall+correct / T8 direct解体 / T9 pipeline解体入agent（team-lead 亲写）/ T10 server瘦身。
- T9 由 team-lead 亲自写（两个 agent 先后卡死：一个试图通读全部源码+一次写超长文件，一个分块写仍停滞）。
- spec 5.3 第 1 条需修正：判官走 CLI 不走 HTTP（见 Task 10 裁决 1）。
- **下一步**：按 T1→T10 顺序用 subagent-driven-development 逐 task 执行；每 task 前读本文件的对齐项（C1-C5 契约冲突 + 对应 task 裁决点）。
## AX78 三端统一角色换签与新 DMS 图同步兼容（2026-08-04）

- DMS 嵌入、企业微信、独立 UI 共用 Bearer 会话身份；新增 `POST /api/session/role`，只允许当前已认证员工切换到其在 DMS `t_role_employee` 中真实拥有的角色，再签发含角色的新 token。
- 多角色员工继续 fail-closed，服务端不默认选择角色；前端三端统一调用换签接口，企微不再出现“选了角色仍不生效”。
- 业务主库切回 `dms`（`xh_dms`），连接会话强制 read-only；凭据仅在 gitignored settings 文件。
- 图同步金额聚合改为 `COALESCE(SUM(d.amount),0)`，兼容新库全 NULL 金额分组。实测图同步成功：60 客户 / 143 商品 / 538 购买边。
- 企业微信 OAuth 配置已启用；主动消息推送仍需单独的 agentid，本轮不虚构。

### AX82 · 会话后台任务、认证预览与经营型深度报告
- **会话任务隔离**：前端由单一 `turns`/单一 progress timer 改为 `Map<conv_id, Turn[]>` + 每请求独立轮询；切换会话不销毁运行中 turn，后台完成只回写原会话且不抢当前预览。侧栏对运行中会话显示 spinner；深度/精简样式使用发送时快照，不受用户之后切模式影响。
- **历史回放**：`chat.msg.payload` 的深度形状 `{result,artifact,page}` 被完整还原，刷新后 BI 内容与预览卡不再退化成空结果。
- **预览鉴权**：iframe 不能携带 Bearer，改为父页面带 Authorization fetch HTML 后以 `srcdoc` 放进 `sandbox=allow-scripts`；token 不进 URL，下载同样由认证 fetch 生成本地文件。实机不再出现“未认证”。
- **报告关联性**：每个板块回传并展示实际子问题；三列表图表 y 轴改取最后一列，避免把维度列当数值。服务端从已执行结果确定性提取头部贡献、展示占比、最新趋势和环比；当前月标记为未完整周期，避免与完整上月直接误判。
- **信息架构**：分享页按“分析目标→主 KPI/经营摘要→AI 结论→结构与趋势证据→明细→口径/SQL”重排；AI 四段改为深色洞察区，证据板块明确行数和查询问题，页面宽度与响应式布局同步优化。

### AX83 · 运营看板 v0.1.19 指标口径对齐
- **权威源**：逐项血缘文档 v0.1.19；DMS 可还原部分登记为 13 个可执行指标、4 个维度、18 个业务术语和 2 条安全关联边。观远折算人数、历史门店月末快照等外部数据不伪造为 DMS 指标。
- **核心口径**：数据起点 2026-06-01；活动场次按持续天数折算；销售额来自促销员明细 `actual_sales`；费用取六项合计（只读对拍主表 `total_amount` 差异 0）；费比=总费用/总销售额；ROI=总销售额/总费用；巡店按业务日期、ID 去重并排除三方/副总职位。
- **23 省区**：活动优先部门省区、否则门店省份映射；巡店按省份映射。苏南/苏北归江苏，川渝藏合并，陕甘青宁新归西北。活动文本省区禁止套用客户主档行政区编码（湖南不得写 `430000`）。
- **确定性快路径**：复合口径继续由 `ops_caliber` 单一生成，`direct-agg` 只填时间和已识别省区；未兑现的城市/客户等限定由残留守卫拒绝。实测 6 月场次 22、销售 23449、费用 2777.9、费比 11.85%、ROI 8.44、巡店 3；湖南活动 7、巡店 2，执行 130-465ms。
- **门禁**：semantic 99/0、server 198/0；新增 OPS01-OPS04 端到端回归 4/0，钉住持续天数、费比两端、巡店排除和湖南文本省区。

### AX84 · DMS 首页嵌入 Agent（交付替换件）
- **当前实现**：DMS 三份源码继续只读；本仓库在 `integrations/dms-home/index.vue` 交付首页单文件替换件，需由 DMS 前端维护方应用到 `src/views/system/home/index.vue`。不得把“交付件已完成”误写成“DMS 源库已修改或已发布”。
- **首页形态**：替换件立即加载完整 Agent，使用 `?embed=dms-home` 隐藏重复登录与退出入口；独立 UI 保持原样，三端共用同一问数、会话、深度 BI 与权限管线。
- **身份传递**：父页从 DMS 既有 `smart_admin_user_token` 读取登录态，经精确 origin 的 `postMessage` 传给子页；token 不进入 URL、浏览历史或日志。子页调用 `/api/sso` 向 DMS `/login/getLoginInfo` 复核身份，再签发 AI 会话。
- **角色隔离**：单角色自动激活；多角色无明确选择时 fail-closed，由用户选择后服务端再次校验角色归属。不存在合并角色或猜测高权限角色的降级路径。
- **验证边界**：Agent 父子握手代码可在本仓库验证；DMS 原生构建与真实账号端到端验证只能在维护方应用替换件后执行，未执行前不得声明已上线。

### AX85 · Doris 跨系统单号与设备需求单准确性闭环
- **资产选择原则**：盘点 `ADS/DW/dim/dms_ods/fin_*/hr_*/sales_*` 后，只接入覆盖率足够且能由 DMS 源码证明业务语义的资产。`sales_dw.dws_fin_shipment_check_dnf` 用于中台/基础系统拆分单号映射；设备需求、收货、投放继续以 `dms_ods` 业务明细为事实源。低覆盖 SKU 维表和设备 ADS 汇总不进入主查询，防止静默漏数。
- **单号识别**：支持 `*N`、`_N` 和组合后缀的中台拆分销售单号，先映射唯一 DMS 销售单，再返回对账差异与商品明细；全程 `direct-doc`、零 LLM 猜表。对账表一对多 JOIN 按真实明细主键去重，实测放大的 18 行恢复为 9 行。
- **设备单闭环**：`HJXH_XQ/DEV_XQ` 需求单直接返回业务头信息，并合并收货与投放两类明细。权限严格复刻 DMS 源码：当前登录人申请，或客户区域经理在当前经理范围；设备专职全量角色只对设备链放开，不扩大其他业务表权限。
- **图与权限注册**：新增设备收货/投放、账单/开票明细和发货对账的继承关系，权限档案从 34 扩展到 41 张表；数仓对账表经 DMS 销售单继承同一行级权限。
- **验证**：workspace 727/0；最终去重改动后 server 205/0。真实 HTTP 登录问数：设备单 6 行（收货+投放），拆分单 9 行商品明细；健康检查 `target=doris_warehouse`、`session_read_only=true`。

### AX86 · Doris 跨库事实复用与确定性降级
- **采用原则**：只使用可由字段、样本和 DMS 源码共同证明的数仓事实。市场费用接入
  `sales_ads.ads_off_sales_cost_customer_dnf`；商品分类直接读取 Doris 商品宽表
  `goods_category_name`；拆分单继续使用 AX85 已验证的 `sales_dw` 对账事实。
- **市场费用口径**：按 `data_month` 过滤，汇总长促督导、客户赔偿、营销物料、营销设备、
  终端、广告、活动执行、客户返利、非活动样品和其他十类费用，并返回分类明细。
  客户费用事实按 `store_code` 继承 DMS 客户范围，权限注册由 41 张扩至 42 张。
- **不虚构发票事实**：当前 Doris 各库没有可证明为 DMS 开票事实的表；开票金额、已开票金额、
  专票金额改走 `direct-doc`，明确返回“不可计算”和补数建议，不再让模型猜不存在的表。
- **客户分类限定**：货架/新媒体/社团/线下/内部/财务专用/外部店铺七类码值直接进入统一
  发货净销售额模板，发货与退货两支同时过滤；未选择分类时保持原 SQL 字节形态，避免可选
  JOIN 引入无意义空白导致金文件假红。
- **验证**：workspace 731/0，最终 server 207/0；Doris 实库回归 67/67，覆盖销售额口径、
  单号与明细、实体和值映射、图关系、运营指标、只读闸门及超管/受限账号权限关系。

### AX87 · 实体销售、库存快照与订单明细确定性化；深度页证据链补全
- **客户/地域销售额**：客户短名与 34 省名称进入统一发货净额装配器，正向发货和退货
  两支同时过滤；客户名拒绝 SQL 通配符/引号，省份统一换行政码。原 E02 约 15.7s、
  SALE17 约 26.8s 的 LLM 路径，实测降为 `direct-agg` 166ms/197ms。
- **库存口径**：当前库存量与库存金额固定只汇总 `t_winc_stock_report` 全表最新
  `product_stock_date` 批次，禁止累加历史快照；同时按商品类型返回补充明细用于图表。
  呈现词表把名称以“量”结尾的数值列识别为 Count，最终视图为
  `KPI + chart + table`，实测 113ms/128ms。
- **订单明细**：昨天订单明细固定使用销售订单业务列、中文状态和业务员/类型翻名，
  不让模型自由选表选列；E03/E14 均转 `direct-doc`，实测 1484ms/120ms。
- **深度报告**：补充明细不再覆盖主 KPI；BI 页从 ViewSpec 读取 KPI，顺序固定为
  KPI/摘要 → 数据板块与图表 → 明细 → 口径/SQL → AI 分析收尾。合并 SQL 拆成
  “主查询 / 补充明细”两项，便于核数；预览和免登录分享均以真实 DMS 登录端到端验证。
- **验证**：最终 workspace 732/0；展示修正后 server 209/0；Doris 全量回归 67/67；
  深度端到端返回 4 个板块、`[kpis, chart, table]`、2 条具名 SQL，布局顺序断言通过。

### AX88 · 可信答案控制面、深度报告对账与 Doris 商品分类补全（历史；销售维度接线已废止）
> 2026-08-06 废止说明：本节涉及默认销售分析的“省份/商品分类”接线不得继续使用。
> `DW.dim_sku.class2` 曾作为实验映射，但尚未形成独立事实的粒度、时间、权限与执行合同；
> 当前默认销售事实不含省份或分类字段，相关请求必须 fail-closed。
- **可信凭证**：非空问数结果携带 `TrustEnvelope`，包含真实物理源、路由、权限、执行方式、
  SQL 指纹、检查项和追踪号；前端统一展示可信/需确认，并将用户反馈绑定 trace 与会话进入
  `meta.query_feedback`。质量后台按路由、反馈、延迟与模型调用聚合，不另建第二套日志。
- **深度 Agent 边界**：LLM 继续负责 PLAN 与证据化解读，但销售额分析板块会编译为经过回归的
  省份、商品分类和月度发货净额问句。报告在 AI 分析前完成同时间窗汇总对账；缺退货分支、
  结果截断、金额不一致或缺失维度超过 5% 均降级为“需确认”。分析目标改由最终执行计划生成，
  禁止出现“模型说分析渠道、页面却没有渠道数据”的错位。
- **商品分类事实**：当前 Doris `t_goods.goods_category_name` 有效覆盖为 0，采用字段与样本验证后
  唯一的 `DW.dim_sku.sku_code -> class2` 补全分类，原 DMS 字段仅兜底。当前月销售事实映射金额
  约 99%，正负两支均连接同一唯一 SKU 维表，无扇出；MySQL SQL 保持原形，不引用跨库表。
- **图谱同源**：购买图的商品分类同步采用同一 Doris 映射；刷新结果 2624 客户 / 456 商品 /
  102122 关系边，避免问数和图问答分类漂移。
- **判官可观测性**：回归 runner 增加单题 60 秒速度门、即时题名/结果和 `--slice` 闭区间，
  防止一个失联子进程让整套门禁无限等待且零日志。
- **验证**：server 213/0，架构门禁全绿；回归 66/0，权限对子 3/0；浏览器最新 artifact 55
  显示主 KPI 30,488,385.02，省份与商品分类合计均与主指标一致，省份未知占比 19% 因而正确
  标为“需确认”，AI 分析位于所有数据证据之后。

### AX89 · VQR 执行验证闭环、指标版本治理与权限判官同源
- **VQR 不是 AI 点赞**：`sql_exemplar` 增加 AI 初审、执行验证、审核人/时间、数据源、SQL 指纹、
  失效原因和指标版本字段。只有人工触发后通过口径规则、权限闸门和真实只读数据源执行的
  `enabled + valid` 样例才能进入 few-shot、相似召回和快捷问题；AI 只能留下初审意见，不能启用样例。
- **变更自动失效**：验证时记录 `metric_code@version` 与物理数据源。指标版本变化或业务查询目标
  热切换后，旧样例自动转为 stale/unverified，不允许用旧口径继续影响 SQL。当前 134 条历史样例
  默认全部不可信，浏览器已将“本月销售额是多少”真实执行验证为
  `sales_amount@2026.08.01`；其余 133 条保持 unverified，未批量洗白。
- **维度白名单 fail-closed**：32 个活动指标全部登记版本与允许维度；确定性组合查询只有命中
  指标白名单才能按维度展开，未登记策略和空白策略均拒绝组合，不由模型自行猜测合法维度。
- **审核界面**：设置页增加紧凑的可信 SQL 样例审核区，可按待验证/已启用/已禁用筛选，展示
  AI 意见、物理源、指标版本、审核人和失败原因，并支持验证、重新验证、禁用。
- **判官修复**：Python 判官通过 `DMSAI_SETTINGS` 与容器显式读取同一身份库，避免从一套库挑人、
  到另一套库验人；固定角色名改为当前库真实 `(data_scope_type:view_type)` 权限形态覆盖，并补比
  设备单专用 `login_names/manager_customer_codes`。当前覆盖 9 种权限形态 + 超管，10/10 集合与行数一致。
- **验证**：Rust 三包 444/0，Web 构建通过；Doris 全量回归分三批执行 66/0；权限双实现 10/0；
  浏览器端完成真实 VQR 验证并确认状态、来源、版本和审核动作重载后仍保持。

### AX90 · S3 单据/分类实体注册表与 AGE 主明细证据（2026-08-05，分类销售接线已废止）
> 2026-08-06 废止说明：分类实体识别本身可保留为术语能力，但不得再据此从默认销售事实
> 计算分类销量/销售额；独立分类资产完成合同登记前，分类经营问数必须明确不可计算。
- **单据只有一个事实源**：新增 `semantic::document::DOCUMENT_FAMILIES`，统一维护 8 个
  单据族的前缀、主表、单号列、明细绑定、DMS 源码证据与 Doris 可用性；`direct-doc`、
  `meta.document_family` 和 AGE 图谱都从这里投影，不再各抄一份前缀表。
- **来源可用性 fail-closed**：只读扫描 Doris 全部 `information_schema.tables`，当前
  `dms_ods` 具备销售、售后、设备三族；对账、旧/新开票、采购调拨未入仓。后四族仍能
  准确识别单号类型，但只返回“当前数仓未同步”及应查主表/明细表/源码依据，绝不执行
  一条必报表不存在的 SQL，也不跨回生产 MySQL 偷查。
- **单据证据卡**：销售、售后、设备、数仓拆单映射的头卡新增 `单据类型/主表/明细表`；
  明细补载后证据仍保留在实体头块。可信凭证仅在这些证据真实存在时增加“单号已匹配
  源码单据族”，普通 `direct-doc` 明细查询不冒领。
- **分类实体卡**：实体 Router 从五卡扩为六卡，新增闭集商品分类。显式“商品分类/类型/
  品类”允许候选匹配，裸分类只精确命中；Doris 使用 `DW.dim_sku.class2`，MySQL 保持
  `t_goods.goods_category_name`，结果包含分类商品数、发货口径销售额、商品清单和下钻。
- **图谱**：每次重建购买图时追加 `DocumentFamily-[:HEADER_TABLE|DETAIL_TABLE]->BusinessTable`。
  实测 8 个单据族、15 张业务表、16 条主明细关系；购买图 2624 客户、456 商品、102193 边。
- **验收**：核心 Rust 500/0；回归 69 题分段全部无失败，额外 1:30 串行跑次 31/31，
  包含 D01<A01 权限数值关系；C01-C06 覆盖真实销售/设备/拆单/售后、未入仓证据和分类名词，
  响应 98-658ms。DMS/Doris 全程只读，自有 PG 仅写元数据与 AGE 图。

### AX91 · S4 问题分类合同与单据型 ReportSpec（2026-08-05）
- **先分类再规划**：深度模式新增纯函数 `analysis::plan`，根据问句、路由、结果形状和单据证据
  分为指标、维度、趋势、对比、归因、单据、实体、明细、综合九类。分类只决定报告结构，
  不生成 SQL；所有取数仍走统一 `ask()`、同一权限闸门和同一业务口径。
- **模型规划受合同约束**：单据、实体和明细类不再调用通用 BI PLAN，不允许把具体单号扩展成
  销售趋势、省份结构或客户贡献；指标/对比/归因类仍可由模型提出候选板块，但销售指标继续
  编译回已验证的发货净额问句。
- **单据页结构**：单据头实体块提升为对象证据，固定展示单据类型、主表、明细表、业务单号、
  客户、状态、金额、数量和时间；底层保留完整原始列用于 CSV/SQL，ReportSpec 只投影商品、
  数量、金额、退货和关联单号等业务列。AI 使用单据专属短模板，只做当前单据核验。
- **思考过程**：用户可见阶段改为“理解问题、执行主查询、识别报告类型、组织证据、生成摘要、
  渲染报告”，不展示 SC/PLAN 等内部实现词，也不展示模型私有思维链。
- **验证**：核心跨包 634/0；最终 server+agent 348/0；Web 构建和架构门禁全绿；C01-C06
  在线回归 6/0。浏览器实测售后单深度页只含单据证据、单据明细、口径/SQL、可信凭证和
  AI 核验摘要，预览/分享/下载入口正常。

### AX92 · 指标深度页复用同口径基线 + 贡献证据（2026-08-05）
- **比较不再由模型计算**：主查询管线已经执行过同口径、同长度上期窗口并写入 KPI delta；
  深度页直接提升该结构化结果为比较卡，不额外查库，也不从年度趋势图猜环比。
- **贡献必须可核数**：指标/维度/对比/归因合同新增贡献证据要求，从已执行结构板块投影前三对象、
  原指标值与板块内占比；页面和 AI 共用这份投影，不把“头部现象”扩写成未经证明的业务原因。
- **阅读顺序**：当前值与基线 -> 头部摘要 -> 贡献表 -> 图表/明细 -> 口径/SQL/可信凭证 ->
  AI 收尾。通用 AI 摘要上限收紧为 280 汉字、最多两条异常和两条行动。
- **无证据外推拦截**：不同时间窗板块不得被判为口径冲突；仅有占比/排行时，不得输出单一品类
  风险、资源倾斜、增长驱动或可持续性判断。首次命中会按精确约束重试，仍命中则丢弃 AI 文本，
  保留确定性 BI 数据和事实摘要。

### AX93 · 当前数据源语义清洁、同源深度编排与响应式 BI 分屏（2026-08-05）
- **语义资产跟随当前物理库**：指标、维度、元素、连接边和规则召回统一关联已启用的
  `meta.table_doc`，自动排除当前 Doris 不存在的旧 DMS 表；市场费用、开票金额等未入仓资产
  不再进入提示词、候选 SQL 或图谱路径。
- **SQL 执行前 fail-closed**：修复器在字段校验前先验证所有物理表是否属于当前启用数据源，
  模型生成幽灵表或切库后遗留旧表时进入修复而不是直接执行。深度模式的所有子查询锁定同一
  实际数据源，数据源自动选择发生变化时重新规划，禁止主指标与拆解板块跨库拼接。
- **深度 Agent 并发但不失控**：身份/会话校验、上下文/问题分类、主查询/精确报告规划分别并行；
  报告计划按数据源缓存 120 秒并去重子问题。LLM 负责理解与证据化分析，取数继续只走统一
  `ask()`、权限闸门和已登记口径。
- **高频业务问题确定化**：补齐领用、配送、退货、凭证、库存调整等源码确认的单据族；仓库缺表
  时返回准确类型、主表/明细表和同步状态，不编造查询。昨天客户清单直接给客户、订单数、金额、
  最近下单时间；客户、商品实体卡统一输出关联订单、销售与明细证据。
- **BI 先数据后分析**：页面固定为 KPI、比较、贡献、图表、明细、口径/SQL、AI 收尾；AI 摘要
  压缩为结论、异常机会和行动表。预览打开后，中等屏自动隐藏侧栏，主对话和可分享报表各自保留
  可读宽度，并保留分享、下载、新窗口和关闭操作。
- **验收**：核心 Rust 476/0，深度合同 2/2，C01-C11 回归全绿，Web 生产构建通过；在线真实
  Doris 问数覆盖客户清单、客户实体、商品实体、销售单和新增单据族，响应约 109-861ms。
  浏览器在 1241px 实测三张图表、数据表、AI 收尾和右侧报表同时无横向溢出；业务库保持只读。

### AX94 · 编号证据闭环、实体关联事实与轻量图表运行时（2026-08-06）
- **深度结论必须引用执行证据**：KPI、分析板块和贡献项统一生成 `KPI/SEC/CON` 编号目录；
  AI 只能引用目录内编号，出现伪造编号、无证据数字或思维链措辞即丢弃并回退到确定性事实摘要。
  页面固定按 KPI、贡献、图表、明细、证据目录、口径/SQL、可信凭证、AI 摘要排列。
- **深度取数限流并保持顺序**：关联板块最多两路并发，完成后按计划顺序归并；最近订单在批次后执行，
  避免瞬时三路压垮 Doris。销售额实测 4 条 SQL / 3 个图表板块，客户名单 1 条 SQL / 1 个图表板块。
- **名词与关系问题补齐业务事实**：客户实体增加购买商品数、活跃月份、首次/最近下单；商品实体补
  活跃月份和首末交易。受限账号无法使用全量购买图时，回落到预聚合明细 SQL，并继续由统一权限闸门
  注入账号行级范围；字段空值明确显示“未维护/暂无/因权限隐藏”，不再静默缺列。
- **自然口语确定化**：“昨天都有谁下过单啊”等表达直接进入客户清单模板，不再误判成裸名称并要求澄清。
- **知识答案数字与版本防幻觉**：回答中的阿拉伯数字必须能在引用原文中逐项找到；命中多个文档版本时
  必须并列保留来源并提示版本冲突，不自动猜测当前生效版。
- **前端速度与视觉验收**：ECharts 改为按需注册并异步加载柱状图、折线图、饼图和必要组件，
  首屏主脚本由约 1.22 MB / 410 KB gzip 降至 181 KB / 65 KB gzip，图表分块仅在出现图表时加载。
  浏览器在 1280px 与 390px 均无横向溢出，
  三张图表画布非空，分屏预览、分享、下载、新窗口可用，控制台无错误。
- **验收**：核心 Rust 438/0；实体/自然问法/受限关系在线回归 5/0，响应 115-475ms；深度合同 2/2；
  Web 生产构建通过，Doris 会话保持只读。

### AX95 · 权限库硬隔离、备用视觉路由与设置治理（2026-08-06）
- **连接职责不可切换**：`mysql_url` 固定为 DMS 身份、角色和数据权限只读连接，不进入
  `SourceRegistry`，也不能被选为问数目标；业务分析只从 `mysql_targets` 选择非 `dms`
  目标，优先 `doris_warehouse`。分析库缺失或不可连接时服务响亮失败，不回退生产 DMS。
- **管理面再收紧**：系统设置除 DMS 管理员标志外，还要求登录名精确等于 `admin`；普通用户
  前端不显示入口，直接访问设置路由或接口也会被拒。DMS 权限连接可测试和修改，但受保护、
  不可删除或切成分析库；分析目标支持新增、修改、删除、测试和热切换。
- **视觉能力按请求热路由**：主模型声明视觉模型时直接使用；主模型不支持图片时才读取备用
  视觉供应商。设置页保存后下一次请求生效。知识库图片上传/重处理与普通图片问答共用该路由，
  AI 识别失败才使用本地 OCR，且权限校验和文件去重发生在模型调用之前。
- **角色共享来自事实源**：知识空间共享角色直接读取 DMS `t_role` 枚举，支持搜索、多选、
  当前筛选结果全选和批量授权；写入仅发生在自有 PostgreSQL 的知识库 ACL，DMS 全程只读。
- **展示统一压缩**：BI、深度报告和日报中绝对值达到 10000 后统一显示为万并固定三位小数；
  编码、状态、百分比不参与金额压缩，CSV 和 SQL 原始值保持不变。深度报告生成后只显示明确的
  预览入口，用户点击后才打开分屏，避免会话被强制挤压。

### AX96 · 主答案/明细双契约、确定性补洞与常驻评测（2026-08-06）
- **主答案不得被明细覆盖**：聚合问题继续以顶层 `columns/rows/view` 返回用户直接询问的 KPI，
  结构与明细进入独立 `supplemental`；单据卡仍用明细替换头表。前端和深度报告分别呈现两层数据，
  且主结果、补充明细均可独立导出 CSV。
- **通用时间维度绑定指标时间**：退款等跨表指标按自身 `time_col` 生成月/年维度，不能借用订单时间；
  无法证明绑定正确才回落。余额榜使用客户和余额类型的最新快照，禁止借销售订单表放大金额。
- **仓储事实确定化**：库存支持仓库、省区分组与过滤；活动临促费、活动执行费登记为独立指标并补齐
  行级权限规则；市场费用 TopN 走费用分类聚合；仓储开票和对账单事实缺失时明确返回“当前数仓未提供”，
  不猜表、不混用相近口径。
- **金标不再静默跳过**：Doris 商品分类、编码解码和市场费用金标更新到当前事实表；确实缺事实的请求
  改成可验证的 unavailable 合同。评测集扩为 39 条，包含 36 条 SQL 结果合同和 3 条显式不可用合同。
- **评测进程常驻**：CLI 增加 NDJSON `eval-batch`，认证、语义注册表和数据源只初始化一次；每条样例仍
  独立加载身份、经过权限闸门并用同一当前分析源执行生成 SQL 与金标 SQL。Python 评测器默认使用批量
  通道，保留 legacy 开关和瞬态连接重试，同时分离产品耗时、SQL 耗时与评测框架耗时。

### AX97 · 默认销售经营事实迁移 DWS（2026-08-06）
- **唯一默认事实**：销售额、销量、不含税收入、不含税成本、毛利额、毛利率统一读取
  `sales_dw.dws_off_offline_sale_dfn`，时间列统一为 `order_date`。销售额=`SUM(amount)`，
  销量=`SUM(qty)`，毛利率=`SUM(gross_profit)/NULLIF(SUM(revenue_excluding_tax),0)`；旧发货/
  退货明细 `UNION ALL` 不再作为默认销售经营口径。订单数和客单价仍使用订单事实按订单号去重，
  禁止用 DWS 明细行数推算。
- **时间窗不继承 legacy time_cap**：这张 DWS 事实没有旧发货专用截断。今天销售额使用
  `[CURDATE(), DATE_ADD(CURDATE(), INTERVAL 1 DAY))`，本月、本周、今年等当期窗口正常包含今天；
  旧 shipment 事实的专用截断只属于历史口径，不得带入 DWS builder、评测或报表说明。
- **趋势与环比审计合同**：SALE14 必须按月份升序展示；SALE16 使用本月至今天（含今天）对比
  上月同期，短月右边界封顶上月月末，不得拿完整上月与本月至今直接比较。
- **今天销售额执行级保护**：新增 E01T gold，固定为
  `[CURDATE(), DATE_ADD(CURDATE(), INTERVAL 1 DAY))`，与 regression/golden A02 同一合同。
- **确认字段合同（2026-08-06 收口）**：默认事实只允许业务确认的 12 个物理字段：
  `order_date/storecode/storename/skucode/skuname/war_zone/region/qty/amount/
  cost_excluding_tax/revenue_excluding_tax/gross_profit`。可用维度仅为日期/月、客户编码/名称、
  商品编码/名称、战区、省区；省区必须使用 `region`。
- **已废止的旧维度声明**：此前把省份、城市、商品分类、退货类型或经理字段写成默认事实能力的
  记录不再有效。它们不属于上述 12 字段，不能以任何别名、样例、缓存、图谱或临时 JOIN 绕过；
  需要独立事实并完成字段、粒度、时间、权限与映射合同后才可开放，否则 fail-closed。
- **权限与性能**：`dws_off_offline_sale_dfn.storecode` 绑定当前 DMS 账号的 `customer_codes`；受限账号
  映射为空时注入恒假条件，明确全量身份才可不注入。日报、周报和深度 BI 复用同一 builder，同一时间窗
  的六个经营 KPI 一次扫描，结构榜只使用单表字段。生产 DMS 不进入通用问数源，只允许专用、短超时、
  小 LIMIT 的简单点查，禁止 JOIN、UNION、子查询和大聚合。
- **日报 artifact 权限边界**：当前 `daily-digest` 以 `conv_id=''` 写入系统定时生成的全量经营快照，
  不继承单个会话或用户的数据范围。生成端必须在报表中显式标注该属性；读取端只允许具备全量经营权限的
  身份访问。artifact 访问控制由服务端统一鉴权链收口，未完成前不得将公共日报等同于账号级权限隔离。

### AX98 · 当前默认经营口径与生产查询边界统一（2026-08-06）
- **默认销售唯一事实**：`sales_dw.dws_off_offline_sale_dfn`，时间列为 `order_date`。
  销售额=`SUM(amount)`，销量=`SUM(qty)`，不含税成本=`SUM(cost_excluding_tax)`，不含税收入=
  `SUM(revenue_excluding_tax)`，毛利额=`SUM(gross_profit)`，毛利率=
  `SUM(gross_profit)/NULLIF(SUM(revenue_excluding_tax),0)`。毛利率必须聚合后相除，禁止平均行比例。
- **客户不是门店**：`storecode/storename` 只表示客户编码/客户名称；任何门店排行、店均、坪效或
  人效必须使用已验证事实中的 `shop_code/shop_name`，不得用客户销售冒充门店销售。
- **退货不重复计算**：默认销售事实已含退货负数，不再拼发货与退货明细 `UNION`，也不再次减
  退款金额。旧发货净额模板只可作为历史专项核对，不再是默认经营销售额。
- **订单指标独立**：订单数读取 `dms_ods.t_sales_order`，按有效订单的 `sales_order_code` 去重；
  DWS 行数、商品行数和物流批次数均不得替代订单数。
- **生产 MySQL 红线**：生产 DMS MySQL 只允许单表、索引等值/小 `IN`/前缀匹配、小 `LIMIT`、
  短超时点查；禁止 JOIN、UNION、子查询、聚合、无界排序和大扫描。统计分析统一走已验证 Doris
  DWS/DWD/ADS，失败时禁止静默回退生产库。
- **历史裁决状态**：AX97 以前凡把旧发货/退货 `UNION` 或“发货净销售额”定义为默认销售额的
  内容，在“默认销售额”这一点上全部废止，原文保留仅用于事故与迁移复盘；独立订单、出库、物流、
  售后和对账场景的专项事实不因本裁决消失。
