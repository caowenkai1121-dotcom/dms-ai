# 三框架深度整合计划（SuperSonic / deepagents / SQLBot → dms_ai）

> 2026-07-31。调研由 7 个 subagent 并行完成（105 万 token，SuperSonic 那路把源码浅克隆到本地逐文件读）。
> 对照数据 104 条，逐条都带**本仓证据**（file:line / meta 表名）。

## 零、本地实测补的三条证据（不在三家清单里，是我们自己的账）

### [D1] 确定性装配覆盖率只有 **4.4%** —— 这是准确度的最大杠杆
`meta.metric` 18 个指标 × `meta.dimension` 55 个维度（去重后）= 990 个「指标 × 维度」组合，
按 `direct.rs::find_path` 的口径（BFS ≤3 跳）实测**只有 44 个可达 = 4.4%**。
其余 95.6% 一律回落 LLM —— 而 `tools/evaluation.py` 头部写着 LLM 路径抖动池 ≥9/38 ≈ **24%**。
也就是说今天绝大多数问句在赌一个 24% 会翻的硬币。

### [D2] DMS 源码里有 60 条 JOIN 关系，我们只登记了 6 条；其中 9 条有真实可达性增益
扫 `D:\code\dms\xh-dms` 的 182 个 Mapper XML 得 60 条不同连接，`meta.join_edge` 只有 6 条。
🔴 **判据是可达性增益，不是「源码里有」** —— 上一轮凭高置信度加了 5 条边，实测增益 **0**，最后全部回退。
这一批逐条量过：

| 增益 | 边 |
|---|---|
| **+18** | `t_customer.customer_code = t_customer_balance.customer_code` |
| +10 | `t_sales_order.delivery_warehouse_code = t_warehouse.wms_code` |
| +10 | `t_customer_device_ledger.sales_order_code = t_sales_order.sales_order_code` |
| +10 | `t_customer.belong_company = t_master_company.company_code` |
| +10 | `t_customer.customer_code = t_device_requisition.customer_code` |
| +10 | `t_after_sales_order_header.refund_warehouse = t_warehouse.wms_code` |
| +7 | `t_customer_device_ledger.sku_code = t_goods.goods_code` |
| +2 | `t_activity_main.id = t_activity_promoter_fee.activity_id` |
| +1 | `t_account_bill_detail.invoice_code = t_invoice_apply_header.invoice_code` |

加这 9 条：**44 → 113 个组合（+157%）**，覆盖率 4.4% → 11.4%。
另外 23 条候选**零增益**（32 条全加与只加 9 条结果完全相同）—— 那 23 条不加。
⚠️ 113 是**上界**：没判扇出（1:N 边只能靠 `COUNT(DISTINCT)` 过）、没判时间列能否落到基表。

### [D3] 已完成（本轮）
- **表级口径 4 张 → 45 张**：45 张指标/维度来源表原来只有 4 张登记 `table_scope`。
  已做成数据驱动补登（从 `meta.column_doc` 反查真有 `deleted_flag` 列的表，
  `ON CONFLICT DO NOTHING` 保住手写的业务口径）。实测生效：「今年各省份的退款额」的 SQL
  现在带 `b0.deleted_flag = 0`，之前会把**已删的售后单**算进退款额。回归 61/61。
- **图库扩到四类节点**：`Customer/Goods` + 新增 `Category`(66) / `Region`(31)，
  边 `BOUGHT`(100515) / `IN_CATEGORY` / `IN_REGION`(2423)。带限定的图问句从「一律答不出」到能答
  （「湖南省买过烤肠的客户」50 行、「山西省…」38 行，逐个核过省份）。
- **表注释张冠李戴修 4 条**：`t_regions`（4715 行省市区）原来在 prompt 里自称「开票申请单」。
- **LLM 换千问**：`fast=qwen3.7-flash` / `precise=qwen3.7-plus` / `enable_thinking:false`。
  对拍见 `tools/model_ab.py`（12 道 SQL 题 + 8 道路由题，各 3 趟取最差）。

---

# 三框架 × dms_ai 差距清单（按落地价值排序）

对照数据 104 条 → 去重合并后 **A 智能问数 22 条 · B 企业知识库 6 条 · 不建议 22 条 · 已有且领先 24 条（附录一行一条）**。
标签口径：`部分有` = 机制在、数据/最后一公里不在（性价比最高的一档）；`没有` = 全新。

**如果只做 6 件：** A1 → A2 → A3 → A4 → A5 → B1。加起来是一次脚本运行 + 一列 DDL + 三处几行改动 + 一个 spawn，全部不动 prompt 字节，不需要重定 golden（除 A1 要重跑评测）。

---

## A 智能问数（准确 / 智能 / 结果美观 / 细节丰富 / 带 AI 分析）

### [A1] 向量召回全面点亮（灌向量 + 体检从静默降级改响亮失败）· 价值高 · 工作量小 · 部分有
- 三家做法：SuperSonic `MetaEmbeddingTask` 启动即灌 + 2h cron 全量补；SQLBot 是改数据源/改注释触发后台重算 + 启动扫 `IS NULL` 补齐；deepagents 无此层。
- 我们现状：列、HNSW 索引、召回 SQL、就绪体检**全都在，数据没灌**。`crates/semantic/src/ddl.rs` 的 `vector_ready` 注释记 2026-07-28 查库结果：`meta.element` 1033 行 embedding 全 NULL、`meta.datasource` 4 行 active / 0 行有向量；写入点只有离线 `tools/embed_service.py:700-755`（table_doc/sql_exemplar/element/datasource 四张）+ `revec`，**从未跑过**；三个消费点 `unwrap_or_default()` 静默降级（`crates/semantic/src/recall/schema.rs:64`、`crates/semantic/src/recall/cards.rs:168`，现各加一条 warn）。trgm 兜底总能把 6 个表额度填满，所以外面看不出向量路是哑的。
- 落地要点：① 跑一次 `python tools/embed_service.py build`；② 重跑 `tools/evaluation.py --runs 3` 重定基线并重定元素阈值 0.35（`cards.rs:190`）；③ 把 `/api/health` 的 `vector_ready` 从"体检"提成发布门（哑了不许上线）。
- 前置/风险：无前置。灌完召回卡数会变多，0.35 是在向量全哑时定的，必须一起重定。**A7 / A8 / A14 / A19 都以它为前提**——在它之前做那几条等于优化一条不通电的线路。

### [A2] 原生 comment 与人工 comment 分列（`custom_comment`）· 价值高 · 工作量小 · 没有
- 三家做法：SQLBot 两列分开存，重新同步元数据只覆盖原生列；SuperSonic/deepagents 无对应物。
- 我们现状：`crates/semantic/src/ingest/schema_sync.rs:138` `upsert_column_doc` 的 `ON CONFLICT DO UPDATE SET data_type=$3, col_comment=$4, ordinal=$5` **无条件覆盖**，`meta.column_doc` 没有人工列。表级侥幸没事：`upsert_table_doc`（同文件 :80-95）的 DO UPDATE 不含 `warn` 列，所以 `seed.rs:35-64 seed_warns` 手写的 ⚠️ 警告不会被冲掉——**那是巧合不是设计**。
- 落地要点：`column_doc` 加 `custom_comment`，`render_schema`（`recall/schema.rs:175-186`）优先取它；`table_doc` 同法保护 `warn`/`domain`。
- 前置/风险：无。**A11（业务人员自助维护注释）没有它就是白干**——落地即被下一次 ds sync 抹掉。

### [A3] 重试与采样的温度分档 + 打开自一致采样 · 价值高 · 工作量小 · 部分有
- 三家做法：SuperSonic 明确"第二轮起 temperature 0→0.5，温度 0 的重试就是同一个错误再来一遍"，且每次采样换 few-shot 子集（10 条召回里挑 3 条：>0.989 必进 + 砍低分一半 + shuffle + 最相似强制回填）；SQLBot 只有 `/regenerate`。
- 我们现状：`crates/agent/src/run.rs:564` `chat_precise` 恒 `Some(0.1)`，首轮生成、`repair`(:551)、SC 的 N 次采样全走同一函数同一温度（:96 注释："温度已经是 0.1，压不下去了"）。投票逻辑本身比上游好：`result_print`(:167) 对**结果集指纹**投票、前两次一致就提前收工、无多数派返首次并标不可信（`clean_pick:199`）。但 `settings.example.json` 默认 `sc_samples=1` ⇒ **生产上整条是关着的**。
- 落地要点：`chat_precise` 加 `temperature` 形参；repair 与第 2..N 次采样传 0.5；生产 `sc_samples=3` 只对失败率高的 route（llm）开。
- 前置/风险：SC 是 N 倍 precise 成本与延迟（`run.rs:96` 记着 B10 单次 24s、190 万行进临时表，所以刻意不并发）。"每次换 few-shot 子集"只在 `sc_samples>1` 时才有意义，且它会让同一问句两次得到不同 prompt——要先跟 `prompt.rs`"prompt 的字节就是行为"的 golden 纪律对齐（做法：shuffle 用问句哈希当种子，可复现）。

### [A4] 剥离 `<think>` / `reasoning_content` · 价值高（隐患兜底）· 工作量小 · 没有
- 三家做法：SQLBot 显式解析 `<think>…</think>` 与 `reasoning_content` 并单独推送前端；另两家无。
- 我们现状：`crates/server/src/llm.rs:83-88` 只取 `choices[0].message.content`，全仓无 `reasoning`/`<think>`。而 `crates/agent/src/prompt.rs` `extract_sql` 的兜底是"裸文本里第一个 SELECT"。
- 落地要点：`llm.rs` 取 content 后一句预处理：剥 `<think>…</think>`、有 `reasoning_content` 就丢弃不拼。四行。
- 前置/风险：无。这不是缺功能是**埋着的错答**：换成把思考混进 content 的推理模型（现用千问系随时可能切），`extract_sql` 会从被模型自己推翻的思路里抽出一条 SQL，而 EXPLAIN 能过、闸门能过、口径判据也可能过——没有任何断言会红。

### [A5] 一次问答的 `trace_id` 串起三张表 · 价值高 · 工作量小 · 部分有 —— ✅ 已落地（AX29/AX30，含 CLI spawn 竞态修复）
- 三家做法：SuperSonic 把 `parsedS2SQL / correctedS2SQL / querySQL / correctedQuerySQL` 四段全落库供人工标注；SQLBot 14 种 operate 各有 start/end log；deepagents 靠 LangSmith。
- 我们现状：三张表各记一段但**拼不回同一次问答**——`crates/server/src/query_log.rs` 一次问答一行（最终 SQL，`CLIP_CHARS=2000` 按字符截）、`meta.correction_log` 记 before→after 九个 kind（`crates/agent/src/run.rs:507` `correct_chain`）、`meta.failure_log` 记失败。没有外键，只能按 question 文本对；`crates/server/src/chat.rs:117-118` 已经吃过这个亏（"query_log 没有 conv_id，从它拿不回本会话上一轮"）。
- 落地要点：三张表加 `trace_id`（+ query_log 顺手加 `conv_id`、加 `tier` 列——今天两次 precise 调用的成本分不开，`query_log.rs:44-45` 只有 token 总和）；`AskCtx` 里透传一个 uuid。
- 前置/风险：无。它直接决定"数字错了，是模型写错还是校正器改坏"查得出来还是查不出来。**A6 的前置。**

### [A6] 全链路分步留痕（steps）· 价值高 · 工作量中 · 部分有 —— ✅ 已落地（AX31，`llm_calls` 一并成真值）
- 三家做法：SQLBot 分步日志带耗时/token/完整 messages，前端可回看执行详情；SuperSonic 落四段中间态；deepagents 是 LangSmith 全托管。
- 我们现状：评测面强（`tools/regression.py` 61 题 route/SQL golden、`tools/evaluation.py` 38 题执行级对 gold、`kb_eval.py`、`model_ab.py`、`judge_scope.py`、`audit_trace.py`），**观测面粗**：`ARCHITECTURE §4.1` 计划的 `SqlTrace` 四态留痕全仓 grep 零命中；"这轮为什么 `llm+repair`、六路召回哪一路空了、五个校正器谁改了什么"只能从 tracing 日志里拼。而本仓最高频的排查题恰好就是"召回为什么是空的"（`crates/agent/src/gather.rs:52-79` 那六行 `map_err(warn)` 与断言 `gather_warns_on_every_recall_degradation` 就是它的补丁）。
- 落地要点：`AskResult` 加一个 `#[serde(skip_serializing_if)]` 的可选 `steps` 数组，每步只记 {阶段, 命中数/kind, 耗时ms}；**不要**把问句与 SQL 原文再落一遍（query_log 已存）。前端 `web/src/App.vue:525-541` 现有 route 徽标旁加一个折叠。
- 前置/风险：依赖 A5。`crates/agent/src/ctx.rs:66/71` 反复写明"serde 形状是前端与两个 runner 的契约，多一个恒在的字段就是一次形状破坏"——必须 optional。messages 全文落库含 schema 与业务数据行 ⇒ 只落统计不落原文；写入必须 spawn（`query_log.rs` 纪律 1）。

### [A7] 首轮召回为空才放宽一档重召 · 价值高 · 工作量中 · 部分有 —— ✅ 已落地（AX33，零额外往返）
- 三家做法：SuperSonic 是 `STRICT/MODERATE/LOOSE/ALL` 四档 + 阈值公式 + 一个都没命中就把阈值折半；SQLBot 无递进。
- 我们现状：只有两档，且**必须先失败一次**：首轮命中筛选 → 失败后回炉喂全量声明（`gather.rs:157 gather_all_cards`，注释明确对照 `AllFieldMapper`/`MapModeEnum.ALL`，材料从 ≈9KB 长到 ≈33KB）。阈值全是编译期 const 且不随轮次变：元素 0.35（`cards.rs:190`）、语义缓存 0.12（`answerers/cache.rs:24`）、选源 0.08（`source.rs:19`）。
- 落地要点：只做"某一路召回结果为空 → 该路阈值放宽一档重召一次"，**不做全局调阈值**。中间那一档（0.35 → 0.5，只在空命中时）就够。
- 前置/风险：依赖 A1（向量哑着时放宽只是放宽 trgm 噪声）。放宽会引噪声卡稀释 prompt，本仓有实测账：autodiscover 灌出的 78 条维度里 68 条是同一条 CASE，把真口径淹没（`gather.rs:190`）。

### [A8] 问句切片多路向量召回（先加批量 embed 接口）· 价值高 · 工作量中 · 部分有 —— ✅ 已落地（AX33，滑窗上提 kernel + MIN 距离单条 SQL）
- 三家做法：SuperSonic 把问句切成大量子串并行探测 + 定长滑窗批量向量召回；SQLBot 整句。
- 我们现状：SQL 路径是**整句一条向量**（`gather.rs:38` `embed.embed_query(cx.question)`，表召回与元素召回共用）+ 整句 substring。唯一的滑窗在图路径实体抽取（`crates/connector/src/graph.rs:314`，长词优先 2..=8 字、命中即占位、后续候选不许与已占区间重叠）——SQL 路径不用它。
- 落地要点：把 graph.rs:314 那个滑窗提到 kernel 复用（纯函数、可单测），切出的片批量送 embed；`connector/src/embed.rs` 已有 `embed_passages`（64 一批），问句侧照抄。
- 前置/风险：一次问答的 embed 调用从 1 涨到 N，而 `tools/embed_service.py` 是单线程 :8077（实测冷启 311ms、热身 14~19ms、有 3s 超时长尾）⇒ **必须先给问句侧也走批量接口**。这是"整句向量被长问句稀释、元素阈值 0.35 常常过不去"的根治项，价值高但排在 A1 之后：先看灌完向量后整句召回还差多少。

### [A9] embedding 自动自愈（启动补齐 + 变更失效 + 后台重算）· 价值高 · 工作量中 · 没有 —— ✅ 已落地（AX34，advisory lock + 配方判据）
- 三家做法：SQLBot 全异步自愈——改数据源/改注释触发后台重算、启动扫 `IS NULL` 补齐、`SingleWorkerGuard.once` 防多实例重复；SuperSonic 是 2h cron 全量。
- 我们现状：写入点只有离线脚本（`tools/embed_service.py` 的 `build`/`revec`，`revec` 已经按 `embedding IS NULL` 补 meta 四张 + `kb.chunk`，键集游标写法在 :775-783），服务侧**只有体检没有修复**（`ddl.rs` `vector_ready` 三个 EXISTS）。后果就是 A1/A14/B2 三条全哑，且三个调用点静默降级。`upsert_datasource` 在 description 变更时置 NULL 的失效机制已经有了（`embed_service.py:749` 注释），只是没人来补。
- 落地要点：server 侧 `tokio::spawn` 一个补齐器（启动跑一次 + 每 N 分钟扫 `IS NULL`），复用 `connector/src/embed.rs::EmbedClient`；单实例守用 PG advisory lock 一行。
- 前置/风险：先做 A1 手工灌一次验证收益，再自动化（顺序反了就是给一条没验证过的链路加调度）。启动时批量 embed 会拖慢启动 ⇒ 必须后台 spawn + 失败只 warn。接线点落在 server：semantic 不持 HTTP 客户端（`gather.rs` 的分工）。

### [A10] exemplar 与在线请求同构 + prompt 总量预算 · 价值高 · 工作量中 · 没有 —— ✅ 已落地（AX40；快照渲染侧缓做有记录）
- 三家做法：SuperSonic 的 `Text2SQLExemplar` 存 `{question, dbSchema, sideInfo, sql}`，与在线 prompt 四槽逐字同源（调研里点名"整个系统最值得抄的一点"）；SQLBot 只存问题+SQL；deepagents 用 `SummarizationMiddleware`（85% 窗口触发、保 10%、原文落盘）解决量的问题。这两件必须一起做——同构会让 few-shot 从两行涨到几 KB。
- 我们现状：`meta.sql_exemplar` 只有 `(question, sql, embedding, status, ds_id)`；沉淀只存 candidate 一条 SQL（`run.rs:384 save_exemplar` → `registry/exemplar.rs` `save`）；召回只能渲染"问：… ```sql …```"两行（`gather.rs:305 fewshot_text`，有 golden 钉着）⇒ **历史样例带不回当时的 schema 与 sideInfo**。另一侧：会话上下文按设计不累积（每轮 `ask.rs:335 rewrite_followup` 改写成独立问句），所以没有爆窗路径、**不需要摘要**；但 prompt 装配也**无总量上限**（`gather.rs:321 schema_text` 无条件拼 6 张表 + join 对面表卡片，`prompt.rs` 全文 grep token/limit/budget 零命中）。
- 落地要点：exemplar 加 `schema_snapshot`/`side_info` 两列（存渲染好的文本，别存结构）；同时给 prompt 加字符预算 + **按段优先级丢**（丢弃序：维度卡 → 值域卡 → 表 → 绝不丢 `T_PITFALLS`）。
- 前置/风险：语料表体积暴涨（schema 段实测 ≈9KB/条）；`prompt.rs` 的 `user_prompt` golden 与 `fewshot_text` 断言都要改（那是刻意的闸，不是障碍）。硬截尾部会先丢 `T_PITFALLS`（`prompt.rs:41` 是最后一段，还是"连库验证过必须遵守"那批）——必须按段优先级，不能截尾。

### [A11] schema 注释业务自助维护（xlsx 导出→填→回传）· 价值高 · 工作量中 · 没有 —— ✅ 已落地（AX41，CSV 复用 B6 通道与同一处 sanitize）
- 三家做法：SQLBot 支持导出 xlsx 人工填备注再批量回传；SuperSonic 靠管理界面逐条编辑。
- 我们现状：注释三个来源**没有一个是业务人员能自助的**：`schema_sync.rs:69-78` 从库原生 comment 同步、`seed.rs:35-64 seed_warns` 把 ⚠️ 警告**手写在 Rust 常量里**（改一句要改代码重编译）、autodiscover 灌列注释。`crates/server/src/admin_api.rs` 只有 terms / exemplars / grant 三组端点，没有一个能改 `table_doc`/`column_doc`。
- 落地要点：两个端点（导出 csv / 上传 csv 批量写 `custom_comment`），csv 不是 xlsx——`web` 侧已有 CSV 导出先例（`App.vue:537`），别为此引 xlsx 库。
- 前置/风险：**硬依赖 A2**，否则下一次 ds sync 抹掉。上传的表头/备注是不可信文本（`recall/schema.rs:198 wrap_untrusted_schema` 正在防它），批量更新必须复用同一处信任边界 + `sanitize_comment`，别在 admin 侧开第二份。

### [A12] 补缺的四个校正器（Select / Having / 去重复列 / 只有上界补下界）· 价值中 · 工作量小 · 部分有 —— ✅ 已落地（AX42，三个；Having 不做有记录）
- 三家做法：SuperSonic 9 个细粒度 AST 纠错器串行重写（含 `SelectCorrector` 把 group by 字段补进 select、`HavingCorrector`、`TimeCorrector`、`removeSameFieldFromSelect`）；SQLBot 用提示词 11 步 checklist 替代。
- 我们现状：五个 + 一个判据层，全部 sqlparser AST 级、复杂 SQL 跳过：`crates/server/src/corrector.rs:67 schema_check` / `:528 fix_group_by` / `:636 normalize_agg` + `:283 correct_agg` / `:405 add_scope_filter` + `:458 correct_caliber`（实测修过"本月订单数虚高 17%"）/ `:258 correct_value`；链在 `run.rs:507`。缺上述四个。
- 落地要点：`SelectCorrector`（group by 有、select 没有 ⇒ 图表拿不到分类轴，正是 present.rs 混轴那族问题的源头）与 `removeSameFieldFromSelect` 最值；`TimeCorrector` **只做"只有上界补下界"这一半**（防全表扫），不做"缺时间就补默认窗"。
- 前置/风险：无。见 X3——自动补默认时间窗与本仓裁决冲突，别顺手一起做。

### [A13] 闸门与留痕的两个小补丁 · 价值中 · 工作量小 · 部分有 —— ✅ 已落地（AX43）
- 三家做法：SQLBot 有分库危险函数黑名单（`LOAD_FILE`/`pg_read_file`/`xp_cmdshell`/`UTL_FILE`/`ADD JAR`）；SuperSonic 的纠错器 try/catch 里会 log。
- 我们现状：① `crates/kernel/src/sql/guard.rs` 的 `forbidden_token` 只有 12 个写操作词，无函数名黑名单（缓冲物是 F3：上传 PG 源的角色读不到自有库，`PostgresSource::connect` 启动期自检）；② `run.rs:507 correct_chain` 每步 `if let Ok(Some(fixed))`，**Err 分支不打日志**——某个校正器炸了，SQL 保持上一版继续走，没人知道。
- 落地要点：往 FORBIDDEN 加一组函数名（按词边界扫，误伤方向是多拒）；Err 分支补一行 warn。两处都是几行。
- 前置/风险：无。补 warn 与本仓"降级必须留痕"的既有纪律一致（`schema.rs:64`/`cards.rs:168` 两条断言）。

### [A14] 选源向量点亮 + 清理遗留 active 上传源 · 价值中 · 工作量小 · 部分有 —— ✅ 已落地（AX44，注入夹具源已清退）
- 三家做法：SuperSonic 是 `HeuristicDataSetResolver` 四级排序（数据集层）；SQLBot 是数据源级向量召回 + LLM 选源。
- 我们现状：判据齐全——`crates/agent/src/source.rs:37 select_source`（显式 > 单源直通 > 向量最近邻，距离差 ≤0.08 交 fast LLM 二选一，`pick_by_gap` 纯函数可测），且已加 `ds_vector_ready` 体检省掉白花的 embed HTTP（断言 `embed_happens_only_after_the_candidate_check`）。但 `source.rs:76` 注释自陈**恒空转**：`meta.datasource.embedding` 一行都没写过 ⇒ `nearest_datasources` 恒空 ⇒ 所有问句都由主源 dms 回答。
- 落地要点：A1 灌完就通电；同时给 `meta.datasource.description` 补上真实业务描述（今天它只被 `pick_by_llm` 用）。
- 前置/风险：依赖 A1。**开之前必须清掉测试遗留的 active 上传源**——每个都会去竞争所有问句的路由（`source.rs:76` 已写明）；它改的是每一句问话的选源行为，回归 + 评测都要重跑。

### [A15] 推荐问题 / 冷启动引导 · 价值中 · 工作量小 · 没有 —— ✅ 已落地（AX45，零 LLM 确定性版）
- 三家做法：SQLBot 基于表结构 + 历史提问猜 N 个后续问题，也支持人工配固定推荐；SuperSonic 有输入联想（trie 补全，见 X 表）。
- 我们现状：grep `recommend` 只命中 `crates/kernel/src/present.rs:99` 的下钻维度声明（`DIM_POOL` 六个维度 `infer_drill`，那是"点这个维度再看一眼"不是"你可以问什么"）；`ask.rs:237 SUGGESTIONS` 是反问时给的四条固定问法。无冷启动端点，`App.vue` 无推荐 chip。
- 落地要点：素材现成（`meta.table_doc` + `meta.query_log` 历史问句），一次 fast LLM，结果缓存按 ds+用户角色；前端复用已有的 drill chips 渲染。
- 前置/风险：无。它治的是真问题：`guard.rs` `constant_projection` 的实测现场正是"用户只发一个客户名『嗨肉』，模型编出 `SELECT 1 AS 探针结果`"——无意图输入本来就该被引导掉，而不是被闸门拒掉。

### [A16] 按数据源的业务背景 prompt 槽 · 价值中 · 工作量小 · 没有 —— ✅ 已落地（AX46，I5 两道防线都在）
- 三家做法：SQLBot 支持按场景/数据源/助手追加背景，作为 `<Other-Infos>` 段；SuperSonic 靠 Agent 配置。
- 我们现状：`prompt.rs` 只 `include_str!` 两个模板，`PromptCtx` 10 个字段里没有"本源业务背景"这一槽；`meta.datasource.description` 列存在但只被 `source.rs pick_by_llm` 用来选源。
- 落地要点：`PromptCtx` 加一槽，空则整段不出（本仓"空段不出标题"已是既有做法）；内容取 `datasource.description`。
- 前置/风险：description 可能来自上传（K4 表格源）＝外部文本，进 prompt 必须走 `prompt.rs:236 render` 的单遍替换（不变量 I5）并截长，别开第二条"外部文本成为指令"的通道。改 prompt 字节 ⇒ 重跑两个 runner。

### [A17] 多候选澄清 + 日期上下文继承 · 价值中 · 工作量小 · 部分有 —— ✅ 已落地（AX47，非阻断 chip + 词法继承）
- 三家做法：SuperSonic 三机制（上下文表继承 + LLM 问题改写 + 候选澄清 `needFeedback`）+ `fillDateConfByInherited`；SQLBot 只有上下文轮数配置。
- 我们现状：改写有且更严（`ask.rs:335 rewrite_followup`：喂上一轮问句 + 上一轮**实际执行的 SQL**、上一轮没有可执行 SQL 就一次 LLM 都不调、`looks_like_sql` 守卫防模型把 SQL 抄进问句；追问词表 `kernel/nl/lexicon.rs FOLLOWUP_MARKS`）。缺：① 日期单独继承（"那上个月呢"依赖整句改写，改写失败就丢时间）；② 澄清只有"意图不足反问"（`ask.rs:160 need_intent_reply`，route=need-intent），不是"多候选让用户挑"。
- 落地要点：多候选澄清复用现有 `view.interact.drill` 渲染（今天反问已经是可点按钮）；日期继承在 rewrite 失败的降级路上补一条"沿用上一轮时间窗"。
- 前置/风险：无。四条降级路今天原样返回原问句（`ask.rs:325`），别把降级改成"猜"。

### [A18] 服务图表的 SQL 规则 + good/bad 示例对 · 价值中 · 工作量小 · 部分有 —— ✅ 已落地（AX48，规则 12 两对示例）
- 三家做法：SQLBot 在提示词里写"维度参与排序 / 有分类字段时数值列默认 SUM / 时间按粒度格式化"，并成对给 `output-bad` / `output-good` 代码块。
- 我们现状：`agent/prompts/system.md` 有第 6 条（明细类 ORDER BY 时间 DESC）与第 10/11 条输出列纪律（带具体反例，`prompt.rs` 有 `system_prompt_keeps_output_column_discipline`/`forbids_percent_suffix_in_sql` 两条断言钉措辞）+ `meta.pitfall`（`prompt.rs:47-49 T_PITFALLS`）。缺"有分类字段→数值列默认 SUM""时间按 yyyy / yyyy-MM / yyyy-MM-dd 粒度格式化"，也没有成对的错误/正确 SQL 代码块。相关坑代码侧已记过一次：`present.rs` trend 注释说 12月×6品类=72 行不切 series 就是混轴折线，而 SQL、口径、行数全对，**没有任何判据会红**。
- 落地要点：加两条规则；措辞必须分清"过滤"与"投影/分组"——system.md 第 8 条禁的是时间**过滤**时 `DATE_FORMAT` 包裹列，粒度格式化说的是投影，表面冲突。
- 前置/风险：改 prompt 字节 ⇒ 重跑 61 题 + 38 题。写 SQL 字面量当示例时注意 dialect 断言禁止 PG 提示里出现任何标识符反引号（`prompt.rs` 注释记了踩过一次）。

### [A19] 术语定义递归 mapping（一层即止）· 价值中 · 工作量小 · 没有 —— ✅ 已落地（AX49，精确名去重）
- 三家做法：SuperSonic 的 `TermDescMapper` 把 `term.definition` 当新问句再跑一遍全部 SchemaMapper（问"复购率"命中术语，定义里的"客户/订单"再去召回真表）；SQLBot 只渲染同义词组。
- 我们现状：`meta.term` 与召回都在（`recall/cards.rs:65 recall_terms` → `T_TERMS` 段），但只把 `term = definition` 渲染成一行，**不拿 definition 再跑一次召回**；全仓无 `descriptionMapped` 类标志。
- 落地要点：命中术语后，用 definition 再跑一遍指标/维度/值域三路（**只一层**），结果去重后合并卡片。
- 前置/风险：要防递归（一层即止）+ 去重。本仓已有"维度卡 78 行淹没真口径"的账（`gather.rs:190`）与"值域/码值卡重复渲染"的实测坑（`cards.rs:97` 那条断言）⇒ 必须过 `map_filter` 并与已有卡去重。做在 A10（prompt 预算）之后更安全。

### [A20] 表/字段人工勾选（`enabled`）· 价值中 · 工作量小 · 部分有 —— ✅ 已落地（AX50，drift 同形守卫）
- 三家做法：SQLBot 未勾选的表/字段根本不进 schema；SuperSonic 靠数据集定义裁剪。
- 我们现状：表侧只有自动规则（`registry/mod.rs:37-52 is_backup_table`：`_bak`/`_history`/6-8 位日期段/尾部 ≥4 位数字 + `prune_stale_docs`），列侧只有 `is_sensitive_col`。误采的业务表**只能靠改 Rust 规则**。
- 落地要点：`table_doc` 加 `enabled` 布尔 + admin 端点开关。
- 前置/风险：加列后**三路召回（forced / vector / trgm）都要加谓词，漏一路就等于没关**；`semantic/tests/drift.rs` 的 `every_meta_recall_is_ds_scoped` 只守 ds 不守它 ⇒ 顺手加一条同形守卫。

### [A21] 复合指标：定义方式分型 + 表达式递归展开 · 价值中 · 工作量中 · 部分有 —— ✅ 已落地（AX51，多规则抽取；递归引用不做）
- 三家做法：SuperSonic 指标三种定义（MEASURE/FIELD/METRIC）+ 指标引用指标递归展开（`visitedMetrics` 防环）+ `defaultAgg`；SQLBot 无指标层。
- 我们现状：`meta.metric` 只有单一 `agg_expr` 字符串 + scope_filter/time_col/dedup_keys/unit，`registry/model.rs:49` 投影 7 列无 define_type；`parse_agg_rule`（`corrector.rs:600`）只接受单聚合形态 `SUM(x)`/`COUNT(DISTINCT x)`，**客单价这类复合表达式保守跳过 ⇒ AggCorrector 对复合指标是死的**。无指标引用指标。
- 落地要点：先只做"复合表达式也能被 `normalize_agg` 识别并归一"（不做递归引用），拿 38 题量收益；递归展开只在真出现指标复用时再做。
- 前置/风险：打开后 `normalize_agg` 会开始改复合指标的 SQL ⇒ 必须重跑 38 题执行级评测；递归要 `visitedMetrics` 防环。

### [A22] 组件级评测（where / group / agg 的 acc·rec·F1 + 难度分档）· 价值中 · 工作量中 · 部分有 —— ✅ 已落地（AX52，diff 分类列；F1 等样本够再补）
- 三家做法：SuperSonic 两层——Spider 式组件级 F1 + hardness 分档，加固定模型批量业务回归 + LLM-as-judge；SQLBot 无。
- 我们现状：三条线且执行级更贴业务（`evaluation.py` 38 题对 gold 结果集、`--runs N` 判据=失败集交集、p50/p95、tags 分层、四态退出码、反空转闸 exit 2；`regression.py` 61 题 + 断言键白名单收口；`kb_eval.py`；`model_ab.py`）。缺组件级 ⇒ **今天只知道"数不对"，不知道错在 where 还是 group by**。
- 落地要点：复用 `kernel` 的 sqlparser AST 与 `ast.rs` 输出列形状比较，在 evaluation.py 侧加一列 diff 分类（where/group/agg/select）。
- 前置/风险：无。做完它，A21 / A12 / A7 的收益才量得清——所以它是这些中价值项的"度量前置"，但不是硬阻塞。

### [A23] HITL：人改 SQL 再放行（edit）· 价值中 · 工作量中 · 没有 —— ✅ 已落地（AX53，edit 一档全链实测）
- 三家做法：deepagents 四种人工决策（approve / edit / reject / respond）+ `interrupt_on` + checkpointer + resume + 条件中断谓词；另两家无执行面人审。
- 我们现状：**语料面**有 approve/reject（`admin_api.rs:259 set_exemplar_status` 三态 + delete、`review.rs:64` 判词、`meta.pitfall` 候选三态），**执行面一个都没有**，尤其没有 edit。`docs/INTEGRATION-TRACE.md` 自己写着"interrupt 人审（HITL）—— 待做"。
- 落地要点：只做 edit 一档：管理员改 SQL → **重走 `crates/agent/src/gate.rs:31 gate`** → 执行 → 改对的 SQL 走 `review_exemplar` 的 POSITIVE 通道回灌成 few-shot 正例（写口现成）。
- 前置/风险：deepagents 拿 HITL 兜的主风险我们用**类型**兜了（只读源 `SET SESSION TRANSACTION READ ONLY`、`SqlSource::fetch` 只收 `&ScopedSql`、OwnedStore 不吃 LLM 产物）⇒ 只剩纠错通道这一点价值。两条硬前置：改后的 SQL 必过闸门（旁路一次 I1 就作废）；人改的 SQL 进语料前仍要过 `review_exemplar` 判词，否则"人工背书"绕过投毒对策。条件中断谓词（deepagents 的 `when`）真做 HITL 时顺手，不单独立项。

---

## B 企业知识库（任意格式上传 / AI 入库 / 智能准确细节丰富）

先说结论：**三个框架在 B 侧几乎没给料**（SQLBot 的知识库能力在对照数据里基本没体现，SuperSonic 无知识库，deepagents 只有 VFS/引用编号）。我们这侧已经比对照数据覆盖到的都强（19 种格式 + 逐页 OCR 补漏 + heading_path 结构化 + RRF 三路融合 + 引用纪律四条断言）。所以 B 组只有 6 条，且一半是跨组基础设施的另一半。

### [B1] 知识库问答零观测 · 价值高 · 工作量小 · 没有
- 三家做法：SQLBot 14 种 operate 全链路 start/end log 覆盖知识库；SuperSonic 三段留痕；deepagents LangSmith。
- 我们现状：`crates/server/src/kb_api.rs:285 ask` 直接调 `dms_knowledge::answer::answer` 并返回，**一行日志都不写**——`query_log` 只挂在问数链上（`crates/server/src/main.rs:997 query_log::finish`）。于是线上看不见：命中了几块、哪几篇文档、有没有走降级（`knowledge/src/answer.rs:32 DEGRADED` 向量不可用时只在答案里提示用户）、token、耗时。只有 `tools/kb_eval.py` 离线能看。
- 落地要点：照抄 `query_log::finish` 的 spawn 模式，记 `{question, hit 数, doc_ids, chunk_ids, degraded, token, elapsed}`；**只记 chunk_id 与统计，不记正文**。
- 前置/风险：写入绝不进主链路（`query_log.rs` 纪律 1：spawn + 失败只 warn）。文档正文含业务内容 ⇒ 走 `CLIP_CHARS` 先例，按字符不按字节截（`query_log.rs:197-198` 那条断言的理由）。

### [B2] 知识库向量自愈（A9 的另一半）· 价值高 · 工作量中 · 部分有 —— ✅ 已落地（AX34，与 A9 同一条链）
- 三家做法：SQLBot embedding 全异步自愈 + `SingleWorkerGuard.once`；另两家无。
- 我们现状：入库时内联 embed（`crates/knowledge/src/ingest.rs:206`，64 一批对齐 `embed_service.py` 的 KB_BATCH），**失败只 warn 并把 chunk 留成 NULL**（注释自陈："doc 推到 embedded 而向量没写 = 界面显示已入库、其实一个字检索不到"，所以刻意不推状态）；补齐只能手跑 `embed_service.py revec`（键集游标 :775-783 已写好）。降级会诚实告知用户（DEGRADED）但仍然漏检。
- 落地要点：与 A9 同一个后台补齐器多扫一张 `kb.chunk`（SQL 现成，直接搬 revec 那两条）。
- 前置/风险：同 A9（后台 spawn + 单实例守 + 失败只 warn）。这条和 A9 是**同一件事**，一起做省一半工。

### [B3] 检索融合后无重排 · 价值中 · 工作量中 · 没有（我们自查，非三家） —— ❌ 毙（AX35 实测：7/7 题金块名次全是 1，7..20 收益带为零）
- 三家做法：对照数据里三家都没有重排；SuperSonic 有"向量召回交 LLM 挑 id"的近亲（见 X5，我判不建议）。
- 我们现状：`crates/knowledge/src/retrieve.rs` 是 RRF 融合三路（`VEC_TOP=20` / `FTS_TOP=20` / `TRGM_TOP=10` → `TOP_K=6`，`RRF_K=60`，`VEC_MAX_DIST=0.55`、`TRGM_MIN=0.2` 都有 `*_MEASURED` 实测断言），TOP_K 之后**直接进 prompt**，没有任何重排；上下文靠 `window`/`span` 扩。
- 落地要点：先用 `tools/kb_eval.py` 量 recall@20 与 precision@6 的差——**只有当 recall@20 明显高于 @6，重排才有收益**，否则这条直接毙掉，省一整个功能。
- 前置/风险：加 LLM 复筛 = 每问一次额外调用与延迟；而 `answer.rs` 已有四条引用纪律（`compact_refs:139` 重编号、`keep_cited_only:208` 剔无角标断言句、`has_valid_ref:315` 越界角标不算引用）在兜"模型引用不实"，重排治的是另一半（真相在第 7 块）。`retrieve.rs:12` 明写"参数全是 const：没有运维入口会去改它们"——重排阈值也照此办，别开配置。

### [B4] 跨子问引用合并 · 价值中 · 工作量中 · 缓做 —— ⏸ 维持缓做（知识库 answerer 不进 Router，不存在多子代理各带来源的形态；真做「知识库复合拆解」时一并做）
- 三家做法：deepagents 多子代理来源去重后全局统一编号。
- 我们现状：单答案侧已有且更严（`answer.rs:139 compact_refs` 只留正文真引用过的来源并重编号——注释说明 `KbAnswer.vue:73` 按 `citations[n-1]` 下标索引，只筛不重编号即角标错位；四条断言锁着）。缺跨子问：`compound.rs:154 summarize` 只吃 `SubResult` 的表格文本，不合并 citations。
- 落地要点：等真要做"知识库复合问题拆解"时再做，那时把 citations 合并 + 全局重编号一次做完。
- 前置/风险：**今天用不上**：knowledge answerer 不进 Router（`crates/agent/src/answerers/mod.rs:11-12`："进链会让文档问句在没命中时回落到 SQL 生成，破不变量 I5"），由 triage 直接分派，所以不存在"多个子代理各自带来源"的形态。

### [B5] 单容器 all-in-one + 内置本地 embedding 模型 · 价值中 · 工作量中 · 部分有 · 只在对外交付时做
- 三家做法：SQLBot 一个镜像起全栈 + 模型打进镜像（离线可跑）；另两家不涉及。
- 我们现状：三个分离镜像（`docker/server/Dockerfile`、`docker/parser/Dockerfile`、`docker/age/`），embedding 是外部 HTTP 服务（`settings.example.json` 的 `http://127.0.0.1:8077`，`tools/embed_service.py serve` 起），模型不在镜像里，无根级 docker-compose 一键起。parser 容器化的动因写在文件头：本机 Smart App Control 拦 lxml 编译扩展 ⇒ word/ppt 恒不可用，容器里全绿；且"镜像里一个凭据都没有，也不 COPY 任何 settings"（F8）。
- 落地要点：先只加**根级 docker-compose**（把已有三个镜像 + embed 服务编排起来），这是 90% 的收益、10% 的工作量。真打成单镜像只在对外交付时做。
- 前置/风险：打进一个镜像会把 PG/AGE/parser/embed 的生命周期绑死，排查变难；F8 的"镜像不含凭据"必须保住。

### [B6] 结果与语料的 xlsx 往返 · 价值低 · 工作量小 —— ✅ 已落地（AX36，CSV 零依赖；示例只导出不导入）
- 三家做法：SQLBot 问数结果/术语库/示例库/用户全能 xlsx 往返。
- 我们现状：只有前端 CSV（`web/src/App.vue:432` 注释 + `:537` 导出入口，`row_count>0` 才出现）；术语/示例库无导入导出。
- 落地要点：术语/示例库的 csv 导入导出顺手（复用 A11 的两个端点形状），别引 xlsx 依赖。
- 前置/风险：无。

---

## 互相依赖（做 X 必须先做 Y）

```
A1 灌向量 ──┬─→ A7 空命中放宽（向量哑时放宽只是放宽 trgm 噪声）
            ├─→ A14 选源向量（+ 必须先清理测试遗留 active 上传源）
            ├─→ A9 自愈（先手灌一次验证收益，再自动化）
            └─→ X5 向量召回 LLM 复筛（前提不满足前做＝空转）
A2 custom_comment ──→ A11 xlsx 注释往返（不做 A2，A11 落地即被 ds sync 抹掉）
A5 trace_id ──→ A6 分步留痕
A10 prompt 预算 ──→ A10 exemplar 同构（同一条，必须一起做：同构会把 few-shot 从两行涨到几 KB）
                └─→ A19 术语递归（会多出重复卡片）
A3 sc_samples>1 ──→ few-shot 子集/shuffle（SC 关着时 shuffle 只是让 prompt 不可复现）
A8 切片召回 ──← 批量 embed 接口（单线程 :8077 撑不住 N 倍调用）
A22 组件级评测 ──→ 度量 A21/A12/A7 的收益（不是硬阻塞，但没有它这三条只能靠体感）
A20 enabled 列 ──→ 三路召回都加谓词（漏一路等于没关）+ 补一条 drift 守卫
A23 HITL edit ──→ 改后 SQL 必过 gate + 进语料必过 review_exemplar
A24（列级权限，见下）──→ 必须落在 policy crate（check-arch.ps1 第 ③ 条门禁：semantic 不依赖 policy）
A9 = B2（同一个补齐器，一起做省一半工）
```

**未列入 A 但需登记的一条**：列级权限按用户/角色（SQLBot 有，我们是全局静态词表 `kernel/nl/lexicon.rs SENSITIVE_COLS` + 结果侧 `RowSet.redacted` 兜底）。价值中·工作量中，**判宽即泄露**，且新增列权限表后必须与 redacted 兜底口径完全一致，否则出现"schema 里没给却在结果里出现"或反之。今天单一敏感列词表够用 ⇒ 建议等真出现"同一列对不同角色不同可见性"的业务诉求再做。

---

## 不建议做

### [X1] S2SQL → 物理 SQL 翻译层 · 工作量大 · 风险极大
- 三家做法：SuperSonic 让 LLM 只写宽表语义 SQL，JOIN / 表达式展开 / 方言 / limit 由 Calcite 语义层补，幻觉列在翻译期就 `status=INVALID`。
- 我们现状：LLM 直接写物理 SQL（`system.md` 第 1 条）；替代物三样：join_edge 卡片喂 LLM（`gather.rs:326`，带一对多扇出警告）+ 执行前字段白名单（`corrector.rs:67 schema_check`）+ EXPLAIN 预检。
- 为什么不做：它改写**每一条** SQL ⇒ route 分布、61 题 golden、38 题评测全部重定。而本仓的确定性装配器 `direct.rs:180 try_compose` 已经吃掉它一半价值（**确定性路径至今 0 失败**）。**先扩 try_compose 的覆盖率比新造一层翻译器划算得多**——这是本清单里唯一值得单独立项的替代方案。

### [X2] 语义层四层模型（主题域树 → 数据模型 → 指标/维度 → 数据集）· 价值低 · 工作量大
- SuperSonic 的组织形式；我们召回单位就是物理表（`recall/schema.rs:27` 返回 `TableCtx`），`domain` 只是 `table_doc` 上一个 text 列，无 parent_id 树、**无数据集层**。
- 为什么不做：加 dataset 层要改召回单位 + 全部 recall SQL + 61 题 route 断言。**组织形式变化，不直接提准确度**。表数量到三位数再谈。

### [X3] 缺时间条件自动补默认时间窗 · 与既有裁决冲突
- SuperSonic 的 `QueryConfig`/`TimeCorrector` 在没时间条件时补数据集默认窗 + `s2_available_date_info` 登记可用日期区间。
- 我们方向**刻意相反**：`prompts/system.md` 第 9 条明令"问题没明确提时间范围时，聚合类不要自行加时间过滤（查全部）"；声明侧只做红线（`kernel/sql/caliber.rs:69 RequireTimeColumn`：问句带时间却没约束声明列 → 判红回炉）。
- 为什么不做：自动补会**静默改变结果集**（用户问"全部"，给"近 30 天"），与"宁可不改也不误伤"的裁决冲突。**只有"只有上界补下界"那一条风险低，已并入 A12。**

### [X4] 无条件 LLM review（"资深工程师再看一遍"）· 价值低 · 实测反向
- SuperSonic 在 S2SQL 层无条件 review + 物理层性能优化两个独立阶段。
- 我们的 LLM 二次出手都**有触发条件**（`run.rs:551 repair` 只在 EXPLAIN 报错/执行报错/红线不过/幻觉列时；`:296 caliber_round` 只在判红时，且 `keeps_output_shape` 只采纳不改输出列的改写）。
- 为什么不做：让 LLM 二次改写一条本来正确的 SQL，本仓有实测账——GOODS17 借回炉整条重构，得 184616 vs 正确 141502（正是 `caliber.rs:107 keeps_output_shape` 的由来）。**但物理层性能优化（谓词下推/日期条件前置）对 30s 超时长尾题有价值、风险小得多，可单独考虑。**

### [X5] 向量召回的 LLM 二次筛选 · 价值低 · 前提未满足
- SuperSonic 的 `use-llm-enhance`：召回结果交 LLM 挑 id，`llmMatched` 豁免全部阈值与短词过滤。
- 我们 `cards.rs:168 recall_elements` 只有硬阈值 `dist < 0.35`，无复筛。
- 为什么不做：每题多一次 fast 调用换召回精度，**而向量路今天还是哑的（A1）⇒ 现在做是空转**。A1 + A7 落地后如果精度仍不够再议。

### [X6] 模型自主决定路由 / 通用子代理 / 虚拟文件系统 / shell 执行 · 工作量大 · 顶不变量
- deepagents 的 `task()` 自主派发、general-purpose 子代理、VFS（ls/read/write/glob/grep）、`LocalShellBackend`。
- 我们现状：父发子只收结论那一半已有（`compound.rs:52 try_compound`，`join_all` 并发 ≤3），但**派发者是代码判据不是模型**；无 tools 通道（`kernel/src/llm.rs:4-5`："tools 字段不建 —— v1 不做 ReAct，真做时加它是 5 行"）。
- 为什么不做：① 我们自己的实测反着——`INTEGRATION-TRACE.md` 记"38 题 route 分布 llm 24 / direct-agg 8…76% 过 LLM，而**全部失败都在 LLM 路径**"，把"派给谁"从 `ROUTER_ORDER` 交给模型 = 把 26 题 direct-agg + 3 题 graph 的回归断言变成不可判定；② 通用子代理与"权限门禁在 `accept` 不在 `answer`"（`answerers/mod.rs:44`，graph 的 accept 要 `has_proof`）直接冲突——能自己选路的成员等于把权限门禁挪回提示层；③ VFS / shell 顶 I2（自有可写库永不接受 LLM 产物）与 I5（外部文本永不成为指令），`scripts/check-arch.ps1` 第 ① 组那条 grep 守的正是这件事；官方自己说 `LocalShellBackend` 是"宿主机、不受限"。

### 其余不建议项（一行一条）

| 能力（来源） | 判定 | 理由 |
|---|---|---|
| 图表两阶段 LLM 选型（SQLBot） | 不做 | 它花两次 LLM 拿到的，`semantic/src/present.rs:243 build` 决策树零 LLM 已算出来 |
| 趋势预测 `/predict`（SQLBot） | 不做 | LLM 外推的点混进结果表/图后，用户/insight/CSV 导出都区分不出它和真实数，与"宁漏不误伤"直接冲突 |
| 内存词典 trie + 中文分词（SuperSonic） | 不做 | 新依赖 + 一份内存索引一致性（kernel 不许 IO ⇒ 词典得住 semantic）；今天元素只 1033 行，全表扫不是瓶颈。真缺的是"问句词→元素"逆向索引与后缀匹配，那是 A8 的一部分 |
| 标签(Tag)画像体系（SuperSonic） | 不做 | 业务上今天没有画像类问句 |
| 多轮伪对话式 prompt 编排（SQLBot） | 不做 | 破 `prompt.rs` 两条逐字节 golden + `build_body` 的 `empty_extra_is_byte_identical`，而伪回执增益在本仓无实测数据；分块效果已由"段标题 + 空段不出标题 + 口径卡排在 schema 前"拿到 |
| M-Schema 紧凑格式（SQLBot） | 不做 | 纯格式差异（我们是 DDL + COMMENT 形态，同样紧凑），改文本＝改 prompt 字节要重跑两个 runner，收益无证据 |
| 提示词 11 步生成 checklist（SQLBot） | 不做 | 提示词自检是我们代码侧确定性判据的弱形式，抄它不如补判据（A12） |
| 标识符零转换硬约束（SQLBot） | 不做 | 那条治繁简/韩语切换，本仓单语言中文不成立；幻觉列已由 `schema_check` 在执行前抓 |
| 多语言与本地化（SQLBot） | 不做 | 内部单语言场景 |
| 工作空间多租户 `oid`（SQLBot） | 不做 | 单租户部署，`meta.datasource.workspace` 今天是纯装饰；真开＝给每张 meta 表再加一维，与 ds_id 那次同等体量 |
| 仪表板/看板（SQLBot） | 不做 | 另一个产品；业主要的是问数准确度 + 知识库 |
| 图表渲 PNG（SQLBot g2-ssr） | 不做 | 要新起 Node 渲图服务 + 改交付形态；MCP 对接方（n8n/Dify）拿 JSON 已够 |
| 输入联想 trie 补全（SuperSonic） | 不做 | 新端点 + 前端改造，对准确度零贡献；A15 推荐问题覆盖了同一场景的实际痛点 |
| 嵌入式"高级应用"+ 动态 CORS + AES 签名（SQLBot） | 不做 | 动态改已构建中间件的 allow_origins 不该抄；外部数据源代理会撬开 connector"全仓唯一能造池"的纪律。iframe/企微两条已有（`auth.rs`/`wework.rs`） |
| 动态数据源/子查询占位替换（SQLBot） | 不做 | 让 LLM 改写已成形 SQL 破 `gate.rs:29-31` 顺序纪律；本仓无"外部系统给 SQL 当表"的场景 |
| 消费外部 MCP 工具（deepagents） | 不做 | 不提升取数准确度，且每个新工具是一条新的不可信输入面，各要一份 `wrap_untrusted` 等价物。我们是 MCP **服务端**（`mcp_api.rs`），方向相反 |
| AsyncSubAgent / 长跑任务轮询（deepagents） | 不做 | 问数是同步交互（前端一次 POST 等结果）；引轮询要动 App.vue + chat.msg 形状 + 任务表，无业务诉求 |
| write_todos 计划工具（deepagents） | 不做 | 上游 v0.7 自己改成 opt-in（基线 token −65%），官方给的三种仍值钱场景（跨很多轮/弱模型/给人看）我们一条不占：一次问答上限 2 轮 LLM |
| Skills 三级进阶披露（deepagents） | 不做 | 加目录层＝多一次 LLM 选择，把确定性命中换成模型判断，与"确定性路径 0 失败"反向；"命中才加载"已由六路召回实现 |
| 阈值/参数运维入口（SuperSonic 25 个可调参） | 不做 | 破"prompt 的字节就是行为"与 `prompt.rs` golden（`ARCHITECTURE §8` 已删过 `RetrieveCfg` 六个可调参，`retrieve.rs:12` 明写理由）。**只有"按环节换模型"值得单独做**（分诊/改写/解读用 fast 已是这个意思） |
| 元数据缓存（SQLBot） | 缓做 | 每次召回打 PG（`render_schema` 每表两条 SQL × 6 表）不是当前瓶颈；缓存要带 ds 维度且能被 ds sync 主动失效，否则重演权限缓存滞后 24h 那族教训 |
| MAX_ROWS / EXEC_TIMEOUT 参数化（SQLBot） | 缓做 | `MAX_ROWS` 不是纯参数：`ctx.rs:172 truncation_note` 用 `row_count == MAX_ROWS` 判截断，GUARD 常量也吃它，改配置要三处一起走 |
| 样例数据注入（每候选表 3 行真实数据进 prompt，SQLBot） | 缓做·风险高 | 价值中·工作量中，但 3 行生产数据是不可信文本 + 越权面：必须走 gate→ScopedSql（否则绕过行级权限注入）、过 `is_sensitive_col`、按 `recall/schema.rs:198 wrap_untrusted_schema` 包裹。三道前置都做完才允许开 |
| 方言规则模板分层（SQLBot 12 份 YAML） | 缓做 | `ARCHITECTURE §8` 已裁决"不做 12 个 .md 里的 10 个"；第三个方言真进来时再动 |
| AK/SK 自助签发（SQLBot） | 缓做 | key 进配置不进库本身是优点（不会随接口响应泄露，同 `datasource` 只存 `dsn_ref` 的裁决）；自助签发要考虑存储与轮转 |

---

## 附录：已有且已领先，一分钱不用花

| 能力 | 我们的位置 | 证据 |
|---|---|---|
| 只读安全闸 / 三段闸门（类型级） | **领先**：`RawSql→CheckedSql→ScopedSql` 字段私有，`fetch` 只收 `&ScopedSql`（想执行 String 编译不过）；对照结论"deepagents 的 text2sql 只读约束全在提示层" | `kernel/src/sql/gate.rs:23/31`、`guard.rs:27-62`、`connector/src/source.rs:16` |
| 行级权限 | **领先**：AST 注入（含 CTE 递归/别名/方言引号），未登记表对受限用户 fail-closed；SQLBot 自承 LLM 缝 where 是妥协 | `kernel/src/policy/inject.rs:44-66`、`rules.rs:46`、`policy/tests/inject_tests.rs` |
| 表白名单二次校验 | 领先：抽实表查 RuleSet，不比对模型自报；解析失败直接拒 | `kernel/src/sql/gate.rs check()`、`sql/ast.rs table_names_of` |
| 系统变量绑定用户 | 已有：全部按 principal 现算，多角色不替用户默认选 | `policy/src/scope.rs`、`principal.rs` |
| 规则解析器（无 LLM 通路） | **领先**：指标×维度数据驱动装配 + join BFS ≤3 跳 + 扇出边闸 + 快照门 + 残留守卫 + `why_not_compose` 逐题报第一道不成立的门；实测 0 失败 | `server/src/direct.rs:180/351/217/70` |
| 规则纠错器族 | 已有五个 + 判据层，全 AST 级、复杂 SQL 跳过 | `server/src/corrector.rs`、`agent/src/run.rs:507` |
| 纠错器吞异常 | 已有（缺一行 warn，见 A13） | `run.rs:507`、`:492` |
| 语料入库过闸 | 已有：pending→enabled/disabled + LLM 复核 + 人工并存 + `worth_learning` 前置，且**刻意不提供"新增示例"端点**并用 `include_str!("main.rs")` 反查路由表钉住 | `admin_api.rs`、`review.rs:75`、`run.rs:481` |
| 记忆/教训自进化 | **领先**：失败复盘 → 候选教训 → LLM 复核 → 注入 prompt，人可否决；system.md 末段三条信任边界逐字＝deepagents Memory 纪律 | `review.rs:44/64`、`recall/pitfall.rs:20` |
| Goal & rubric 自评收敛 | **领先**（上游对照写在代码里）：`check_caliber` + 五条 CaliberRule → 四态 judge → 预算共用 + `keeps_output_shape` 只采纳不改输出列的改写；实测 32→33→34/38 | `agent/src/guard.rs:26-31/80`、`kernel/src/sql/caliber.rs:107` |
| 委派纪律硬上限 | 已有：MAX_SUBS=3 / MIN_SUBS=2 / 拆解轮数=1 由结构保证 / MAX_ATTEMPTS=2 / CALIBER_ROUNDS=1 | `compound.rs:35-36`、`run.rs:33/43` |
| 子代理上下文隔离 + 结构化返回 | 已有：独立 AskCtx，回传只两字段强类型；递归由"compound 不在 ROUTER_ORDER"结构排除 | `compound.rs:74/11-13`、`ctx.rs:102-106` |
| 中间件顺序纪律 | 已有：ROUTER_ORDER 五位是契约，`router_is_the_contract_in_full` 锁死；D9"顺序即行为"是设计硬线 | `answerers/mod.rs:57/69` |
| 工具结果卸载 | 已有：大结果**结构上不进 prompt**（MAX_ROWS=200，进 LLM 只 5 行 + IN 条件截断），超限带原因+范围+续读参数 | `gate.rs:22`、`insight.rs:34/37/39`、`ctx.rs:174` |
| 路径级硬闸（deny-list） | 已有且更狠：无条件拒 information_schema/mysql/meta/kb/chat 等；ACL **内联进检索 SQL** | `guard.rs:91-92`、`knowledge/src/acl.rs:207` |
| 多路召回（6 路）+ 结果融合过滤 | 已有：三路表召回 + 指标/维度/术语/元素/码值/值域 + join 对面表补卡；`map_filter` 四条中文适配规则（7 条断言） | `gather.rs:41-90/326/349`、`kernel/nl/text.rs:34` |
| 维度值字典与别名映射 | 已有：探测 + 三闸防误配 + 生成前提示/生成后换码两处消费 | `ingest/autodiscover/probe.rs`、`match_dict.rs:16`、`corrector.rs:258` |
| 自动建模 | 已有：探测→对码→注册 + schema 同步 + 人工种子优先（两侧都筛掉 autodiscover 自己的产物） | `semantic/src/ingest/` |
| 表关系 + 未召回表自动补齐 | 已有（差 ER 画布 UI，维护走 seed/CLI） | `prompt.rs:43-46`、`recall/schema.rs:139-152` |
| 术语库 / SQL 示例库 | 已有（含管理端点） | `recall/cards.rs:68-85`、`registry/exemplar.rs:16` |
| 错误回灌重试 | 已有且更强：回炉材料是 schema + **全量**指标/维度声明（有断言钉住理由） | `prompt.rs build_repair_prompt` |
| 结果呈现 / 下钻 / 环比 / 确定性洞察 / AI 解读 | **领先**：零 LLM 决策树选图 + 下钻 chips + 环比共用同一"今天"锚点 + CR3 集中度；insight 的口径说明是**确定性从已执行 SQL 读出**（声明是意图、SQL 是事实） | `semantic/src/present.rs:243/17/106/128`、`agent/src/insight.rs:92` |
| Excel/CSV 当数据源 | 已有：解析→建 `up_<doc_id>` schema→注册 global 源→查询侧免注入 + ACL 可见性 | `kb_api.rs:63/136`、`gate.rs gate_on` |
| 上传格式与 AI 入库 | **领先**：19 种扩展名（与文档服务 CAPS 由断言锁死一致）+ PDF **逐页** OCR 补漏（整份失败会 422、丢半份也响亮失败）+ heading_path 结构化 + 400/60 分块 | `knowledge/src/ingest.rs:62/15-16`、`tools/embed_service.py:163-201` |
| 知识库检索与引用纪律 | **领先**：RRF 三路融合 + window/span 原文回查 + 只留真引用的来源并重编号 + 无角标断言句整句剔 + 越界角标不算引用 | `retrieve.rs:19-27`、`answer.rs:139/208/315` |
| 不可信文本包裹 | 已有：`wrap_untrusted` + 转义闭合标签与属性引号（两处开枪断言）+ 上传表头 sanitize | `answer.rs:162/187`、ARCHITECTURE F4 |
| MCP 服务端 | 已有：两个工具、权限等同该员工登录、默认关、错误码拆可否重试（纯函数带正反单测）。差 markdown 表格输出（顺手） | `server/src/mcp_api.rs:135/38/63` |
| 强制行数限制 | 已有且不可关，命中上限带续读参数 | `guard.rs:151-165`、`gate.rs:22`、`ctx.rs:172-190` |
| 池管理与权限缓存 | 已有：按 ds 管池 + 更新即 close + dsn 脱敏；权限缓存 TTL 15 分钟 + key 带 scope_ver（DMS 改配置后第一次查询即自愈） | `connector/src/registry.rs`、`policy/src/cache.rs` |"
  },
  "workflowProgress": [
    {
      "type": "workflow_phase",
      "index": 1,
      "title": "调研"
    },
    {
      "type": "workflow_phase",
      "index": 2,
      "title": "对照"
    },
    {
      "type": "workflow_phase",
      "index": 3,
      "title": "综合"
    },
    {
      "type": "workflow_agent",
      "index": 1,
      "label": "research:supersonic",
      "phaseIndex": 1,
      "phaseTitle": "调研",
      "agentId": "a3b2c7813e80f1de1",
      "model": "claude-opus-5[1m]",
      "state": "done",
      "startedAt": 1785468256371,
      "queuedAt": 1785468256327,
      "attempt": 1,
      "lastToolName": "StructuredOutput",
      "lastToolSummary": "tencentmusic/SuperSonic（Chat BI + Headless BI 一体化，Java…",
      "promptPreview": "深度调研 tencentmusic/SuperSonic（https://github.com/tencentmusic/supersonic）。

用 WebFetch 读这些（读不到就换路径/用 WebSearch 找镜像与文档站）：
- README.md（中英文都看）
- docs/ 目录下的架构文档
- 语义层（semantic layer）相关：headless BI / 指标-维度建模 / schema mapper
- Chat 模块：查询理解、SQL 生成、纠错、多轮
- 关键类名与包结构（headless / chat / common / launchers）

重点回答（这些是委托方最关心的）：
1. **语义层怎么建模**：指标(metric)/维度(dimension)/术语(term)/标签 的表结构与字段，指标怎么声明聚合表达式、时间列、去重键
2. **S…",
      "lastProgressAt": 1785470208108,
      "tokens": 233857,
      "toolCalls": 66,
      "durationMs": 1951736,
      "resultPreview": "{"project":"tencentmusic/SuperSonic（Chat BI + Headless BI 一体化，Java 21 / Spring Boot 3.3 / LangChain4j 0.36 / JSqlParser 4.9 / Calcite 1.38）","architecture":"六个 Maven 模块：auth（鉴权 SPI）、common（Text2SQLExemplar / ExemplarService / 枚举）、headless（语义层，再分 api / chat / core / server）、chat（对话层 server）、launchers（standalone / chat / headless 三种装配，SPI 顺序全部写在 META-INF/spring.factories）、webapp。\
\
数据流两层嵌套。外层是 chat…"
    },
    {
      "type": "workflow_agent",
      "index": 2,
      "label": "research:deepagents",
      "phaseIndex": 1,
      "phaseTitle": "调研",
      "agentId": "aa8fc95f3b93bbc2a",
      "model": "claude-opus-5[1m]",
      "state": "done",
      "startedAt": 1785468256377,
      "queuedAt": 1785468256327,
      "attempt": 1,
      "lastToolName": "StructuredOutput",
      "lastToolSummary": "deepagents (langchain-ai/deepagents) — LangChain 生态的 "de…",
      "promptPreview": "深度调研 deepagents 框架（LangChain 生态，https://github.com/langchain-ai/deepagents）。

用 WebFetch/WebSearch 读 README、docs、examples，尤其 text-to-sql 相关示例。

重点回答：
1. **核心抽象**：deep agent 与普通 ReAct agent 的区别是什么（planning tool / sub-agents / file system / detailed prompt 四要素各自解决什么问题）
2. **子代理（sub-agent）机制**：怎么派、怎么隔离上下文、结果怎么回收
3. **计划工具（todo/planning）**：为什么把计划显式化能提升长任务质量
4. **虚拟文件系统**：agent 之间怎么共享中间产物
5. **text-to-…",
      "lastProgressAt": 1785468968394,
      "tokens": 56232,
      "toolCalls": 37,
      "durationMs": 712017,
      "resultPreview": "{"project":"deepagents (langchain-ai/deepagents) — LangChain 生态的 \"deep agent\" 框架，v0.7 时代","architecture":"deepagents 不是新 agent 循环，而是在 langchain `create_agent`（普通 ReAct: think→act→observe）之上的一层**中间件装配器**，底座仍是 LangGraph 状态机（含 streaming/checkpointer/持久化）。`create_deep_agent(model, tools, system_prompt, subagents, middleware, memory, skills, backend, permissions, interrupt_on, ...)` 一次性把一串中间件按固定顺序装成管…"
    },
    {
      "type": "workflow_agent",
      "index": 3,
      "label": "research:sqlbot",
      "phaseIndex": 1,
      "phaseTitle": "调研",
      "agentId": "a008c9b82a6f0849e",
      "model": "claude-opus-5[1m]",
      "state": "done",
      "startedAt": 1785468256378,
      "queuedAt": 1785468256327,
      "attempt": 1,
      "lastToolName": "StructuredOutput",
      "lastToolSummary": "【部署形态】单容器 all-in-one（Dockerfile + start.sh）：内…",
      "promptPreview": "深度调研 dataease/SQLBot（https://github.com/dataease/SQLBot）。

用 WebFetch/WebSearch 读 README（中英）、docs、docker-compose、后端目录结构。

重点回答：
1. **整体架构**：前后端技术栈、模块划分、数据流
2. **数据源接入**：支持哪些库，schema 怎么采集与缓存
3. **训练/调优机制**：术语表、schema 注释、SQL 示例（few-shot）怎么维护，有没有「训练数据」概念
4. **权限模型**：行级/列级权限怎么做，多租户怎么隔离
5. **结果呈现**：图表怎么自动选型，有没有仪表盘/大屏，导出能力
6. **AI 分析/解读**：出数之后有没有自动洞察（趋势/异常/归因）
7. **知识库/文档问答**：有没有这块，怎么做的
8. **MCP / API 开…",
      "lastProgressAt": 1785469470411,
      "tokens": 193914,
      "toolCalls": 48,
      "durationMs": 1214033,
      "resultPreview": "{"architecture":"【部署形态】单容器 all-in-one（Dockerfile + start.sh）：内置 PostgreSQL(pgvector) → pm2 起 Node 的 g2-ssr(3000, AntV G2 服务端渲图) → uvicorn main:mcp_app(8001, MCP SSE) → uvicorn main:app(8000, 主 API+静态前端)。镜像还内置了本地 embedding 模型（maxkb-vector-model，shibing624/text2vec-base-chinese），所以离线可跑、不依赖外部向量服务。\
\
【技术栈】后端 Python 3.11 + FastAPI + SQLModel/SQLAlchemy + Alembic（71 个 migration，启动时自动 upgrade head）+ lan…"
    },
    {
      "type": "workflow_agent",
      "index": 4,
      "label": "gap:tencentmusic/SuperSonic（Chat BI + Headless BI 一体化，Java 21 / Spring Boot 3.3 / LangChain4j 0.36 / JSqlParser 4.9 / Calcite 1.38）",
      "phaseIndex": 2,
      "phaseTitle": "对照",
      "agentId": "a09e815f91dbaa5e4",
      "model": "claude-opus-5[1m]",
      "state": "done",
      "startedAt": 1785470208154,
      "queuedAt": 1785470208126,
      "attempt": 1,
      "lastToolName": "StructuredOutput",
      "lastToolSummary": "tencentmusic/SuperSonic（Chat BI + Headless BI 一体化，Java…",
      "promptPreview": "下面是对 tencentmusic/SuperSonic（Chat BI + Headless BI 一体化，Java 21 / Spring Boot 3.3 / LangChain4j 0.36 / JSqlParser 4.9 / Calcite 1.38） 的调研结果。你的任务：**逐条**与本仓现状对照，判断我们「已有/部分有/没有」。

本仓（D:\code\dms_ai）是 Rust 7-crate 架构的智能问数 + 企业知识库系统：
- crates/kernel：纯算法（SQL 闸门/只读红线/NL 时间解析/词表），零 IO
- crates/policy：行级权限注入（inject）+ 权限集合
- crates/connector：MySQL/PG/AGE图/embed/LLM 的 IO
- crates/semantic：语义注册表（meta.metric/di…",
      "lastProgressAt": 1785470969981,
      "tokens": 190178,
      "toolCalls": 42,
      "durationMs": 761827,
      "resultPreview": "{"project":"tencentmusic/SuperSonic（Chat BI + Headless BI 一体化，Java 21 / Spring Boot 3.3 / LangChain4j 0.36 / JSqlParser 4.9 / Calcite 1.38）","gaps":[{"capability":"语义层元数据模型四层（主题域树→数据模型→指标/维度→数据集）","our_status":"部分有","evidence":"crates/semantic/src/ddl.rs 的 16 张 meta.* 表：table_doc/column_doc（模型=物理表本身，PK 是 table_name）、metric/dimension、join_edge、datasource。`domain` 只是 table_doc 上的一个 text 列，没有 parent_…"
    },
    {
      "type": "workflow_agent",
      "index": 5,
      "label": "gap:deepagents (langchain-ai/deepagents) — LangChain 生态的 "deep agent" 框架，v0.7 时代",
      "phaseIndex": 2,
      "phaseTitle": "对照",
      "agentId": "a6b37f6686e6c73c7",
      "model": "claude-opus-5[1m]",
      "state": "done",
      "startedAt": 1785470208155,
      "queuedAt": 1785470208126,
      "attempt": 1,
      "lastToolName": "StructuredOutput",
      "lastToolSummary": "deepagents (langchain-ai/deepagents) — LangChain 生态的 "de…",
      "promptPreview": "下面是对 deepagents (langchain-ai/deepagents) — LangChain 生态的 "deep agent" 框架，v0.7 时代 的调研结果。你的任务：**逐条**与本仓现状对照，判断我们「已有/部分有/没有」。

本仓（D:\code\dms_ai）是 Rust 7-crate 架构的智能问数 + 企业知识库系统：
- crates/kernel：纯算法（SQL 闸门/只读红线/NL 时间解析/词表），零 IO
- crates/policy：行级权限注入（inject）+ 权限集合
- crates/connector：MySQL/PG/AGE图/embed/LLM 的 IO
- crates/semantic：语义注册表（meta.metric/dimension/value_map/term/join_edge/table_scope/
  tab…",
      "lastProgressAt": 1785470846602,
      "tokens": 117283,
      "toolCalls": 41,
      "durationMs": 638446,
      "resultPreview": "{"project":"deepagents (langchain-ai/deepagents) — LangChain 生态的 \"deep agent\" 框架，v0.7 时代","gaps":[{"capability":"write_todos 计划工具（TodoListMiddleware）","our_status":"部分有","evidence":"crates/agent/src/compound.rs:40 is_compound + :116 split_questions —— 一次性拆 2-3 条子问，**没有 todos state、没有跨轮重写计划**。agent 明确无状态机：crates/agent/src/run.rs 文件头「ARCHITECTURE §8 删掉了 AskRun（Step/Stage/ExecFailure + 8 个回调，575 行）…"
    },
    {
      "type": "workflow_agent",
      "index": 6,
      "label": "gap:dataease/SQLBot（v1.10.0，FIT2CLOUD 开源许可 / GPLv3 衍生，~6.5k stars）",
      "phaseIndex": 2,
      "phaseTitle": "对照",
      "agentId": "a15dfdf89c08fff3a",
      "model": "claude-opus-5[1m]",
      "state": "done",
      "startedAt": 1785470208156,
      "queuedAt": 1785470208126,
      "attempt": 1,
      "lastToolName": "StructuredOutput",
      "lastToolSummary": "dataease/SQLBot v1.10.0 逐条对照 dms_ai（Rust 7-crate）",
      "promptPreview": "下面是对 dataease/SQLBot（v1.10.0，FIT2CLOUD 开源许可 / GPLv3 衍生，~6.5k stars） 的调研结果。你的任务：**逐条**与本仓现状对照，判断我们「已有/部分有/没有」。

本仓（D:\code\dms_ai）是 Rust 7-crate 架构的智能问数 + 企业知识库系统：
- crates/kernel：纯算法（SQL 闸门/只读红线/NL 时间解析/词表），零 IO
- crates/policy：行级权限注入（inject）+ 权限集合
- crates/connector：MySQL/PG/AGE图/embed/LLM 的 IO
- crates/semantic：语义注册表（meta.metric/dimension/value_map/term/join_edge/table_scope/
  table_snapshot/va…",
      "lastProgressAt": 1785471241576,
      "tokens": 179828,
      "toolCalls": 49,
      "durationMs": 1033420,
      "resultPreview": "{"project":"dataease/SQLBot v1.10.0 逐条对照 dms_ai（Rust 7-crate）","gaps":[{"capability":"多轮伪对话式提示词编排（rules/schema/术语/示例拆成多条 Human + AI「我已确认」回执）","our_status":"没有","evidence":"kernel/src/llm.rs:36-42 ChatRequest::text 只造 system+user，断言 llm.rs:92-98 钉死；server/src/llm.rs:104-105 build_body 硬写两条 Msg。多轮改在别处：agent/src/ask.rs:335 rewrite_followup 把追问+上一轮SQL改写成独立问句；素材是 agent/src/prompt.rs:96-118 单条 user 里的 1…"
    },
    {
      "type": "workflow_agent",
      "index": 7,
      "label": "synthesize",
      "phaseIndex": 3,
      "phaseTitle": "综合",
      "agentId": "a06a282ec0bf06190",
      "model": "claude-opus-5[1m]",
      "state": "done",
      "startedAt": 1785471241582,
      "queuedAt": 1785471241580,
      "attempt": 1,
      "lastToolName": "Bash",
      "lastToolSummary": "sed -n 285,320p crates/server/src/kb_api.rs && echo ---- &&…",
      "promptPreview": "下面是三个框架的能力与本仓现状的逐条对照（104 条）。

本仓（D:\code\dms_ai）是 Rust 7-crate 架构的智能问数 + 企业知识库系统：
- crates/kernel：纯算法（SQL 闸门/只读红线/NL 时间解析/词表），零 IO
- crates/policy：行级权限注入（inject）+ 权限集合
- crates/connector：MySQL/PG/AGE图/embed/LLM 的 IO
- crates/semantic：语义注册表（meta.metric/dimension/value_map/term/join_edge/table_scope/
  table_snapshot/value_domain/sql_exemplar/pitfall/kw_force）+ 召回 + autodiscover + seed
- crates/know…",
      "lastProgressAt": 1785472517106,
      "tokens": 86861,
      "toolCalls": 11,
      "durationMs": 1275524,
      "resultPreview": "# 三框架 × dms_ai 差距清单（按落地价值排序）

对照数据 104 条 → 去重合并后 **A 智能问数 22 条 · B 企业知识库 6 条 · 不建议 22 条 · 已有且领先 24 条（附录一行一条）**。
标签口径：`部分有` = 机制在、数据/最后一公里不在（性价比最高的一档）；`没有` = 全新。

**如果只做 6 件：** A1 → A2 → A3 → A4 → A5 → B1。加起来是一次脚本运行 + 一列 DDL + 三处几行改动 + 一个 spawn，全部不动 prompt 字节，不需要重定 golden（除 A1 要重跑评测）。

---

## A 智能问数（准确 / 智能 / 结果美观 / 细节丰富 / 带 AI 分析）

### [A1] 向量召回全面点亮（灌向量 + 体检从静默降级改响亮失败）· 价值高 · 工作量小 · 部分有
- 三家做法：Su…"
    }
  ],
  "totalTokens": 1058153,
  "totalToolCalls": 294
