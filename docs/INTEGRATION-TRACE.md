# 三框架整合追溯表（SuperSonic × deepagents × SQLBot → 本仓落点）

> 用途：把「深度参考三个开源项目」变成**可逐条核对**的映射。
> `INTEGRATION-PLAN.md` 是动工前的**计划**（它的状态列已落后），本文是**当前实况**。
>
> 每行四列：来源机制 → 本仓落点（file:symbol）→ 状态 → **证明它的那条测试或那次实测**。
> 状态口径：`已落地`＝有代码有断言；`已实测`＝连库量过并留下数字；
> `半`＝落地但有明确缺口（缺口写在同格）；`拒抄`＝看过之后决定不抄，理由必须在格内。
>
> 代码与文档里的引用点计数（`grep`，复核命令见文末；含本文）：
> **SuperSonic 131 处 / deepagents 54 处 / SQLBot 22 处**。
> 注释里带原始机制名是刻意的：改那段代码的人应当看得见它对标的是什么。

---

## 一、SuperSonic（语义层 + Chat 链路）

### 1.1 语义层（Headless BI 的注册表思想）
| 来源机制 | 本仓落点 | 状态 | 证明 |
|---|---|---|---|
| `MetricResp` 指标口径单一事实源 | `meta.metric`（`semantic/src/ddl.rs`）+ 15 条种子（`seed_defs.rs::METRICS`） | 已实测 | 评测 38 题；`metric_card` 的断言 |
| `DimensionResp` 维度口径 | `meta.dimension` + 10 条种子 | 已实测 | 回归 26 题断言 `direct-agg` |
| `DomainTerms` 业务黑话→标准口径 | `meta.term` + `recall_terms` | 已落地 | `recall/cards.rs` 断言 |
| `ValueLinking` 编码列 中文名→码 | `meta.value_map` + `recall_value_hints` | 已实测 | 省份 34 码、`paid_way`/`customer_type` 等 |
| `ValueLinking` 进**确定性装配**（不只是喂 LLM 提示） | `direct.rs::value_filters` + 按 `join_edge` 桥值过滤表 | 已实测 | SALE17「本月湖南省的销售额」`llm→direct-agg`、值与现跑 gold 逐字节相同；E16 `customer_class='04'`、200 行客户名全为「线下-」；歧义 109 名/子串两条危险命中均被门挡（二·AD） |
| `SchemaElement` 统一可向量召回 | `meta.element` + `sync_elements` + HNSW | 已落地 | `registry/element.rs` |
| `JoinPath` 表间边 + 基数 | `meta.join_edge` + `compose` BFS ≤3 跳 + **LLM 路径 join 段**（`agent/src/gather.rs::join_lines`） | 已落地 | `join_lines_keep_edges_touching_recalled_tables` |
| 数据模型 `model filter`（表级恒需过滤） | `meta.table_scope` + `CaliberRule::RequireCols` | 已实测 | 明细表口径把「动销商品数」从 292 修到 173 |
| 分区时间维度（同表多时间列语义不同） | `meta.metric.time_col` + `CaliberRule::RequireTimeColumn` | 已落地 | `require_time_column_flags_the_wrong_time_field`；由实测「用发货时间虚高 26%」驱动 |
| 派生指标（指标套指标） | `refund_ratio` + `metric.unit='percent'` + `RequirePercentScale` | 已实测 | 评测 AS04 从 `0.049` 修到 `4.89` |
| 快照/最新值语义 | `meta.table_snapshot` + `RequireLatest` + `compose` 快照门 | 已实测 | 评测 FIN02/FIN04 转绿；`snapshot_source_metric_never_composed` |

### 1.2 Chat 链路
| 来源机制 | 本仓落点 | 状态 | 证明 |
|---|---|---|---|
| `SchemaMapper` 命中净化五规则 | `kernel/src/nl/text.rs::map_filter`（中文适配） | 已落地 | 7 条搬运断言（`map_filter_*`） |
| 精确词典层（专名精确命中，精确 > 向量） | `meta.value_domain` + autodiscover **名称型探针** + `RequireJoinAndFilter` | 已实测 | 灌入 68 个分类名；评测 GOODS16 从虚高 36% 转绿 |
| `TimeRangeParser` 规则时间解析 | `kernel/src/nl/time.rs::time_predicate`（三级正则 + 中文数字） | 已落地 | 该文件断言组 |
| `rewriteMultiTurn` 多轮改写 | `agent/src/ask.rs::rewrite_followup` + `is_followup` | 已落地 | `followup_needs_short_question_and_a_mark` |
| 解析期 dry-run（不等真执行就验列名/类型） | `agent/src/run.rs` 的 EXPLAIN 预翻译 → `explain-fail` 回炉 | 已落地 | `correction_kinds_all_present` |
| `SchemaCorrector` 幻觉列校验 | `server/src/corrector.rs::schema_check` → `schema-fix` | 已落地 | 同上 |
| `GroupByCorrector` | `corrector.rs::fix_group_by` | 已落地 | 该函数断言组 |
| 口径过滤补全（确定性 AST 补，非 LLM 改写） | `corrector.rs::add_scope_filter` + `correct_caliber` | 已实测 | `caliber_adds_to_joined_detail_table` 等 12 条 |
| `MemoryReviewTask` 记忆闭环 | `agent/src/review.rs` + CLI `review-pending` / `review-lessons` | 已落地 | `meta.sql_exemplar.status` 三态 |
| `textSummary` / 推荐下钻维度 | `kernel/src/present.rs`（+ `semantic/src/present.rs` 词表） | 已落地 | present 11 条断言 |
| **SC 自一致采样**（多路投票） | `agent/src/run.rs::run_llm` + `result_print` / `majority` / `clean_pick`，配置 `sc_samples`（默认 1＝关） | 已实测·**判为不开** | 6 条纯判据（指纹只看值不看列名 / 行序即答案 / 分隔符不可伪造 / 门槛严格过半 + 返回下标 / 多数派内优先返无标注那份）。**全量实测 sc=1 与 sc=3 同为 34/38，而 p95 从 62s 涨到 153s（2.5×）**：MKT04+GOODS17 转绿、E05+GOODS13 转红，净 0。机制上收益与损失**对称** —— SC 向众数收敛，众数错时它把「偶尔靠运气对」变成「稳定地错」（GOODS13 这次 +25.9%，正是它原始缺陷的签名）。功能留着（默认关、开销为零），**诊断价值已兑现**：3 次采样给同一个错值，证明 AS03/SALE15 是系统性的而非噪声。结论：该修的是众数本身（声明补全 + 判词可执行 + 确定性重写），不是对错的众数投票 |
| **「还剩多少不通用」的度量** | `why-not-compose` 的 `⚙ 硬编码兜底` 一维（`hardcoded_producer`） | 已实测 | 38 题：`✅声明可装配 10 / ⓿让路 4 / ②装配器拒 9 / ①指标不命中 9 / ⑤残留 4 / ③快照 2`。**硬编码模板（`agg_template`/`sales_breakdown`/单号直查）已不再唯一服务任何一道题** —— 它赢只因为那道刻意的让路门。解除让路要两件：`item_type` 业务裁决 + 装配器支持 KPI 环比。详见 `_DECISIONS.md` 二·AB |
| **确定性覆盖提升**（把题从 LLM 搬到装配器） | 指标 only + 声明时间列 + 维度别名「每个月」+ `STRIP_WORDS`（上半年/下半年·箱·排序疑问词·量词）+ `detect_top_n` 的「最高的 N 个」 | 已实测 | 可装配 4 → **10**。**GOODS13**（此前 4 次 2 绿 2 红、两次错值同为 2138540.58）与 **GOODS17**（此前稳定 +30.5%）双双转 **direct-agg 且逐值一致**；E02/E09/PERM01 同样转正（~12-20s → ~9s）。顺序纪律：**先补 `detect_top_n` 再解锁量词** —— 反了就把「飘着的失败」换成「确定的失败」（gold 只要 5 行、装配器会给 200 行）。虚词表每次只加实测挡住过的那一个，`元/件/装` 刻意不加（会吃掉实体名）；两处枪测过 |
| **指标 only 通用装配**（无维度问句走确定性路径） | `direct.rs::try_compose_metric_only` + `compose_sql_with` 的无维度模式；诊断口 `why-not-compose` | 已实测 | 动因是实测：38 题 route 分布 `llm 24 / direct-agg 8 / llm+repair 5 / semantic-cache 1` —— **76% 过 LLM，而全部失败都在 LLM 路径**（确定性路径至今 0 失败）。`why-not-compose` 逐题诊断后最大一档是 **② 维度不命中 17 题**：`try_compose` 强制要维度，而无维度这条路只有一个硬编码模板且只认 4 个指标。实现**不写第二个装配器**（伪维度 + 无维度模式），去重下推/表级口径/时间桥接/扇出/残留守卫全部复用同一份。两道自设门：**给 `agg_template` 让路**（否则「本月销售额」的数会从订单头换成明细声明那套 —— 正是未裁决的 `item_type`；且会丢 KPI 环比）、**命中维度即退出**（否则静默丢分组） |
| **未做**：MapMode 四档递进 / 升温重试 / 结果级指纹缓存 / 时间谓词下推 | —— | 待做 | 升温重试与 SC 有重叠，先量 SC 的收益再定要不要 |

---

## 二、deepagents（长任务鲁棒性）

| 来源机制 | 本仓落点 | 状态 | 证明 |
|---|---|---|---|
| 中间件洋葱 → 有序能力表 | `agent/src/answerers/mod.rs::Answerer` + `ROUTER_ORDER` 五位 | 已落地 | `router_is_the_contract_in_full`（五标签与契约全等） |
| 规划 → 多步查询 → 合并 | `agent/src/compound.rs::try_compound`（并行子问 + fast LLM 汇总） | 已落地 | 回归 2 题断言 `route=compound` |
| SubAgent 结果收敛 | `agent/src/ctx.rs::SubResult` | 已落地 | serde 形状断言 |
| **Rubric 自评闭环**（grader 按清单评审 → 回炉 ≤2 轮） | `kernel::check_caliber`（判据）→ `agent/src/guard.rs::judge`（裁决）→ `run.rs` 显式回炉循环 | 已实测 | 曲线 32 →（31）→ 33 → 34；裁决 二·G/二·H |
| 回炉不许改坏对的部分 | `kernel::keeps_output_shape`（**只采纳输出列未变的改写**） | 已落地 | `repair_must_keep_output_shape`；由 GOODS17 被回炉打坏驱动 |
| 大结果截断三件套（原因 + 范围 + 续读参数） | `agent/src/ctx.rs::truncation_note`；知识库侧 `knowledge/src/answer.rs` 单块 1200 字截断附说明 | 已落地 | `ctx.rs` 断言；前端 `App.vue` 渲染 |
| Memory 信任边界三条纪律 | `agent/prompts/system.md`（记忆非指令 / 凭据禁令 / 口径注明来源） | 已落地 | `system_prompt_keeps_*` golden 断言 |
| 硬拦范式（AST 层锁只读） | `kernel/src/sql/guard.rs`（三段闸门的第一段） | 已落地 | 12 条 `sql_guard` 断言 + 回归 3 条红线题 |
| 权限三态 / interrupt 人审（HITL） | —— | 待做 | —— |
| **未做**：AsyncSubAgent 后台任务 / 摘要压缩与会话 offload / HarnessProfile 模型画像 | —— | 待做 | —— |

---

## 三、SQLBot（能力面与产品面）

| 来源机制 | 本仓落点 | 状态 | 证明 |
|---|---|---|---|
| 对外 MCP（供 n8n/Dify 调用） | `server/src/mcp_api.rs`（JSON-RPC 2.0，手写零依赖） | 已落地 | `mcp_keys` 空则恒 404 的断言 |
| 查询统计日志（sql/耗时/cache_hit/token） | `server/src/query_log.rs` + `meta.query_log` | 已实测 | 本轮实测 route 分布与 p50/p95 |
| 上传表格**双通道**（进知识库 + 建物理表可问数） | `knowledge/src/tabular.rs` + `connector/src/ddl.rs::create_upload_table` + `server/src/kb_api.rs::sync_upload_schema` | 有 | **端到端已实测**：`tools/up_probe.py` exit 0（上传 → 建表+登记+采 schema → 问数出数 600）。那句「未实测」曾压着三个都不报错的缺陷，一次实测同时暴露：①prompt 与闸门硬写 MySQL 方言（非 MySQL 源问数恒 ``syntax error at or near "`"``）②建池不置 `search_path` → 探针按 `current_schema()` 采不到表 → LLM 拿到空 schema ③备份表启发式误伤 `t0_<uuid>` 表名（≈1/6 概率静默不可问数）。详见 `_DECISIONS.md` 二·K |
| 术语表 / 自定义提示词 | `meta.term` + `meta.pitfall`（教训） | 已落地 | `recall_pitfalls` 断言 |
| 管理面 CRUD（数据源/知识库/注册表） | `server/src/{ds_api,kb_api,admin_api}.rs` | 已落地 | 各自断言 |
| **拒抄** · 行权限用 LLM 改写 SQL | 不抄。行级权限只在 **AST 层注入**（`kernel::inject` + `dms_policy`），LLM 产物一律先过三段闸门 | 拒抄 | 理由写在 `agent/src/guard.rs`：LLM 改写会让「权限是否生效」变成不可判定；本仓 46 条权限断言 + `judge_scope.py` 6/6 独立复现 |
| **拒抄** · 无重试的一次性管线 | 不抄。改为**显式** `for attempt in 0..N` 回炉循环（§8 明确删掉状态机回调） | 拒抄 | `run.rs` 的循环 + 八个 `correction_log` kind |
| **拒抄** · workspace 隔离 / 推荐追问 | 本轮不做（v2 §8 明确推迟） | 拒抄 | 裁决记录 |

---

## 四、两大功能的当前成熟度（用数字说，不用形容词）

| | 智能问数 | 企业知识库 |
|---|---|---|
| 执行级评测 | **34/38 = 89.5%**（曲线 32 →（31）→ 33 → 34） | **6/7**（KB05 作答漏条已定位并修，待复测） |
| 行为回归 | **53-54/54**（唯一失败 B10 是伪装成路由断言的性能断言，见裁决 二·J3） | 同 kb_eval |
| 权限 | `judge_scope.py` **6/6** 与 Java 语义独立复现集合全等；MySQL 授权层只有 `SELECT` | **ACL 越权守住**（KB04：他人空间薪酬文档拒答且不泄漏口令） |
| 注入 | 三段闸门 + 12 条 guard 断言 | **两条注入向量都守住**（KB05 文档正文 / KB06 CSV 表头） |
| 已知缺口 | SALE15 持续红（业务裁决未定，见 二·L）；GOODS13 是偏错的硬币（真杠杆是把「销量按月」接进确定性装配，见 二·O5）；B10 性能边界 | **PDF / Excel 解析已可用**（`pypdf` BSD-3 + `openpyxl` MIT，零 AGPL 依赖，见 CONFIG「文档解析器依赖」）；**Word / PPT 本机不可用**——不是许可也不是代码，是 `lxml` 编译扩展被 SAC 拦（与裁决 二·E 同一个拦截器），Linux 部署正常；`parse_ok` 现在**真 import 一次**才报可用（原来用 `find_spec`，只查包在不在 → 装了 python-docx 就假报 true，正是它文档里那句「不许假装可用」自己犯了） |
| 单测 | 全仓 **465 passed / 0 failed（20 target）**；架构门禁 15 条全绿 | 同 |

---

## 五、怎么核对这张表

```powershell
# 三个项目的引用点（本文头部那三个数字）
foreach ($k in 'SuperSonic','deepagents','SQLBot') {
  "$k : " + (Get-ChildItem -Recurse crates,tools,docs -Include *.rs,*.py,*.md |
             Select-String $k | Measure-Object).Count
}
.\scripts\docker-test.ps1     # 465 passed / 0 failed（20 个 target）
.\scripts\check-arch.ps1      # 15 条，exit 0
```
连库那几个数字的复现步骤见 `PROGRESS.md` 的「连库验收」节与 `_DECISIONS.md` 二·E。
