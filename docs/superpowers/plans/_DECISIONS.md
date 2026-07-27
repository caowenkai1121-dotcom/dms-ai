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

**Task 7**（4 点）：
1. ingest MySQL 入参以 T3 为准 = `&ReadOnlyMySql`（见 C5），不允许 &MySqlPool 过渡。
2. OPT_OUT 入库致 meta.term +7 行：**对拍脚本豁免 status='opt-out' 行**（Task 6 基线不含它，Task 7 新增）。
3. `dim_hit` 死代码**保留**（守「13 测试一字不改」，随 7 个 filter 测试搬入 recall/filter.rs）。
4. meta.rs 删除前置 = Task 6 清场，降级路径（未清场则不删、留段标注）**可接受**。

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

## 三、状态
- **10 份 plan 全部落盘**：T1骨架 / T2 kernel纯算法 / T3 newtype闸门 / T4 connector llm+embed / T5 policy / T6 meta DDL+种子 / T7 semantic recall+correct / T8 direct解体 / T9 pipeline解体入agent（team-lead 亲写）/ T10 server瘦身。
- T9 由 team-lead 亲自写（两个 agent 先后卡死：一个试图通读全部源码+一次写超长文件，一个分块写仍停滞）。
- spec 5.3 第 1 条需修正：判官走 CLI 不走 HTTP（见 Task 10 裁决 1）。
- **下一步**：按 T1→T10 顺序用 subagent-driven-development 逐 task 执行；每 task 前读本文件的对齐项（C1-C5 契约冲突 + 对应 task 裁决点）。
