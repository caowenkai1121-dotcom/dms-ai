# 深度整合计划：SuperSonic × deepagents × SQLBot → 通用 Agent 工具

调研方式：12 个 agent 并行（3 个项目源码/文档深挖 + 2 条能力线只读审计 → 差距矩阵 82 行 → 3 条取舍不同的竞争路线图 → 3 个独立镜头评审）。约 200 万 token、54 分钟。
本文件是**给业主选的计划**，不是已完成的工作。

---

## 0. 先说三件比路线图本身更重要的发现

### 0.1 🔴 尺子是坏的 —— 四处「跑绿但什么都没测」，已逐条核实

| 处 | 现象 | 后果 |
|---|---|---|
| `tools/kb_eval.py:252-264` | 入口探针失败、**或任一夹具上传失败** → 打一行 ⏭️ 然后 **`return 0`**（退出码 0，一题不跑） | 「kb_eval 全绿」可能是「一题没跑」。知识库所有后续改动的验收都建在这道门上 |
| `scripts/serve.ps1` 的 `$mounts` | 只挂运行时配置与 `<KB_ROOT>`，**没挂 `tools/`**；而 `main.rs` 的 `why-not-compose` 全量模式读相对路径 `tools/eval_cases.json` | 容器里那条全量诊断**必然读不到文件**。本次会话我是手工 `docker cp` 才跑通的 —— 判据的输入不在容器里 |
| `tools/regression.py` | 断言键白名单外的键**静默忽略**（只认 route/route_not/sql_contains/sql_contains_any/sql_not_contains/min_rows/min_cols/view0/chart_kind/json_contains） | 写错一个键名 = 那条断言永远通过。三条路线图里各有一条判据踩了这个坑 |
| `regression_cases.json` 55 题 | 28 题钉 route、14 题钉 SQL 片段、**0 题钉数值** | 一旦把口径编辑权开给运营，**错数会带着 `route=direct-agg` 和 ✅ 全绿过关** |

**结论：任何方案都必须以「修尺子」开头。** 否则后面每一条「已达标」都是自证。
这与本仓反复抓到的缺陷同族（断言恒真 / 判据的输入没了），`_DECISIONS` 里已有多条同类记录。

### 0.2 用户点名的三项能力，实测状态比预期差

| 用户要求 | 实测状态 | 证据 |
|---|---|---|
| 「带 **AI 大模型分析**」 | **对 99% 的问句完全不存在**。唯一的模型解读 `summarize` 在 `crates/agent/src/compound.rs:153`，只在复合问句分支里被调用；而 route 分布里 `compound` 占比极低 | `compound.rs:153`；`PROGRESS.md` 的 route 分布 |
| 「用户可以**自由上传**任何文件」 | **前端没有任何上传入口**。`web/src/*.vue` 里 `upload` / `input type="file"` / `FormData` **零匹配** —— 后端 `kb_api` 有上传端点，但用户在界面上传不了任何文件 | `web/src/` 全量搜索 0 命中 |
| 「**企业**知识库」 | 今天实际是**每人一个私有空间**：`store.rs:141` 注释写「v1 只有个人空间」，`:145` 恒写 `'private'` | `crates/knowledge/src/store.rs:141,145` |
| 「图片」格式 | 无 OCR / 无 VLM。Word/PPT 解析本机被 Smart App Control 拦（lxml 的 DLL） | `tools/embed_service.py` 解析器分支；`docs/CONFIG.md` |
| 「结果美观」 | 表格**只渲染前 100 行**，而后端文案与导出都是 200 行，界面上没有一个字说明下面还有 100 行 | `web/src/ResultPanel.vue:72` `slice(0, 100)` |
| 「细节丰富」 | 三个已算出来的字段**没人渲染**：`RowSet.redacted`（用户把脱敏列当故障）、子结果 `caliber_note`、子结果 `truncation_note` | `ctx.rs` 无消费者；`ResultPanel.vue` 接口里没这两个字段 |
| 「智能」下钻 | 建议维度是**硬编码 6 个常量**，而注册表里已声明 10 个 —— 品牌/客户分类/大区经理/客户类型 4 个已声明维度**永远不会被建议**（等于白建的声明） | `crates/semantic/src/present.rs:15` |
| 知识库检索 | 三路混合里 trgm 那一路**结构性失效**：阈值 `0.3`，而实测正确块得分 `0.267` 被挡在门外 | `crates/knowledge/src/retrieve.rs:26` |
| 知识库自愈 | 向量服务抖一次，那份文档**永久检索不到** —— `ingest.rs` 承诺的「稍后可重建」没有实现者 | `ingest.rs:176-180` 无 reembed 入口 |

### 0.3 已落地的**不许重做**（评审逐条核对过代码）

方言参数化（`prompt.rs:76,124` + `gate.rs:52-57`，且有 `dialect_and_quote_come_from_the_source_not_a_default` 断言钉着）、强制 LIMIT 注入（`guard.rs:106-120`）、AST 解析失败即拒（`guard.rs:28`）、行权限 AST 注入（`kernel::inject` + 46 条断言 + `judge_scope.py` 6/6 与 Java 语义集合全等）、命中净化五规则（`nl/text.rs::map_filter`）、确定性装配器（`direct.rs`）、图表选型决策树（`present.rs:243-267`）、术语/示例的 **ds 作用域**（差距矩阵曾误记为「缺失」，实际 `registry/lexicon.rs:88-91` 已走 `ds_pred`）。

---

## 1. 差距总览（82 行矩阵的浓缩，按用户七个形容词分组）

### 问数「准确」
- 召回阈值四档递进（STRICT→MODERATE→LOOSE→ALL）— SuperSonic `MapModeEnum`。**缺失**
- schema-linking 兜底：命中元素过少时把该表全字段给 LLM — SuperSonic `PARSER_FIELDS_COUNT_THRESHOLD`。**缺失，几行代码**
- 按 join 边把「对面缺失的表」卡片补进 prompt — SQLBot 关系补全。**缺失，几行代码**
- 值链接**反查修列名**（值对了列错 → 按 value→column 反查换列名）— SuperSonic `SchemaCorrector.updateFieldNameByLinkingValue`。**缺失**，输入 `meta.value_map` 已有
- judge **四态**（`grader_error` 与 `failed` 分离）+ 自相矛盾校验 — deepagents `RubricMiddleware`。**缺失**：`guard.rs:58-68` 只看 `violations.is_empty()`，「判据自己跑挂了」会伪装成 Pass
- 升温重试（首次失败后 temperature 0→0.5）— SuperSonic `LLMSqlParser.tryParse`。**缺失**（温度 0 的重试是确定性重复）
- 元数据查询禁令（SHOW/DESCRIBE 是否被挡**不确定**，需一条断言确认）

### 问数「智能」
- 下钻**确定性重写**（点下钻 → AST 改写 SQL，不再过 LLM）。**缺失**：今天点下钻是拼问句重问，很可能落回 LLM 路 = 落回失败集
- 下钻维度池读注册表（6 常量 → 10 声明，用 `meta.query_log` 现成聚合排序）。**部分**
- ~~推荐追问（人工配置 + 历史高频，0-LLM 两档）~~ —— **已两次判推迟，不进本轮**
  （`_DECISIONS.md` 二·K6「推荐追问（H10）明确推迟，不进本轮预算」+ `PROGRESS.md` 的
  「剩余功能类只有推荐追问与 workspace 隔离，均已判为推迟」）。
  ⚠️ 本文件下方 D5 曾把它算进 3 天预算 —— 那是自相矛盾，已删。
  留一份记录的意义：否则下一轮又会有人按计划去做已被否的事。
- 多轮改写喂上一轮命中 schema + 历史 SQL（`meta.query_log` 里已有上一轮 SQL）。**部分**
- 追问识别 14 字硬阈值。**已落地**（`ask.rs` 有断言钉住 14/15 边界）。
  ⚠️ 本行原来的举例是**错的**：「那再帮我按省份拆开看看呢」只有 **12 字**，
  今天 `is_followup` 就返 true —— 它证明不了漏判，照它去改就是给不存在的 bug 打补丁
  （恒真判据同族）。真要举例得 ≥15 字，例如「那再帮我按省份和商品分类拆开看看呢」（17 字）。
  另：真要放宽阈值，必须同批看 `answerers/cache.rs` —— `is_followup` 的第二个消费者是
  「追问不许命中语义缓存」，放宽阈值等于同时放宽缓存旁路面。

### 问数「AI 分析」
- **单问结果的 LLM 解读**（把 `summarize` 从 `is_compound` 门后解耦 + `/record/{id}/analysis`）。**缺失** ← 用户点名的能力
- 确定性洞察覆盖多指标结果（`present.rs:141-144` 限「恰 1 指标列」，而「销售额+订单数+客单价」恰恰最想要解读）。**部分**

### 问数「美观 / 细节丰富」
- 表格行数三处口径统一 + 行数脚注（**实打实的 bug**）
- `RowSet.redacted` 呈现（「本列已脱敏」而非一列空值）
- 子结果 `caliber_note` / `truncation_note` 渲染
- **权限过滤生效回显**（「结果已被行权限过滤，条件是…」）—— 属正确性：防用户拿被过滤的数当全量下结论
- 图表扩类（地图/堆叠/散点/漏斗）、表格排序分页合计、同比 YoY

### 知识库「任何格式」
- **前端上传入口（零）** ← 最基础的一条，三条路线图全漏了
- 图片/扫描件 OCR 或 VLM；旧二进制 Office（.doc/.xls/.ppt，需 LibreOffice headless）
- Word/PPT 解析线容器化（解 SAC 阻塞）
- PDF 章节层级 `heading_path`（`pymupdf4llm` 是 AGPL，**待业主裁决**）

### 知识库「智能准确细节丰富」
- trgm 阈值 0.3 挡掉 0.267 的正确块（**改一个常量**）
- 向量路无相关度下限（HNSW 恒返 top-20）
- `kb.chunk` embedding 重建入口（**所有 KB 质量改动的地基**）
- 前端引用回传 `span`（引用可核对性是知识库唯一的自证手段）
- rerank / cross-encoder 重排
- 评测覆盖：8 题 → ≥16 题，补 PDF/docx 题、要点覆盖率、多文档冲突、recall@k 基线

---

## 2. 技术方案（按 crate 落点，遵守单向架构）

| 落点 | 内容 |
|---|---|
| `kernel`（纯算法零 IO） | judge 四态与自相矛盾校验；`check_caliber` 解析失败改为**返回 grader_error 而非空**（今天返空是漏判方向）；洞察放开多指标 |
| `connector`（唯一 IO 出口） | 升温重试参数；embedding 重建的批量写口 |
| `semantic` | 召回阈值递进；schema-linking 兜底两件；值链接反查修列名；下钻维度池读注册表 + `query_log` 排序；术语描述二次召回 |
| `knowledge` | trgm 阈值 + 向量路下限；reembed 入口；`span` 透出；rerank（可选）；PDF heading_path（待许可裁决） |
| `agent` | `summarize` 解耦成单问可用；工具错误分型；（可选）Plan/待办状态机与子步隔离 |
| `server` | `/record/{id}/analysis` 端点；权限过滤回显字段；`why-not-compose --csv`（**必须同批加 flag 解析**，现有 `args.get(2)` 会把 `--csv` 当问句）；KB 上传/管理 API 收口 |
| `web` | **上传入口（新建）**；三个未渲染字段；行数口径与脚注；引用 `span`；AI 解读面板；图表扩类与表格交互 |
| `tools`/`scripts` | `kb_eval.py` 逐题记红继续跑（**不许 return 0**）+ 题集扩到 ≥16；`regression.py` 未知断言键**报错而非忽略**；`evaluation.py --runs N` 出交集与抖动池；`serve.ps1` 挂 `tools/`；给 28 条 direct-agg 题各加**数值断言** |

---

## 3. 三条候选路线（工作流产出）与评审结论

| 路线 | 立场 | 工期 | 评审排名 |
|---|---|---|---|
| **A 质量优先** | 先补判据地基 → 检索与格式质量 → 解读与细节 → 图表与下钻。铺面（多工作空间、复杂编排、指标商店 UI）不进本轮 | 18-22 人日 ≈ 日历 5-6 周 | **2 个第一**（用户要求覆盖度、风险红线） |
| **C 声明覆盖优先** | 让已声明的东西全部被读到 + 补声明不再需要改代码重编译。不做 OCR/编排/S2SQL | ≈ 4 周（只做 P0+1+2 约 2 周即可拿到可度量结果） | **1 个第一**（可验证性）+ 2 个第二 |
| **B Agent 内核优先** | 先把 deepagents 四件套做成 agent crate 里可断言的结构，问数与知识库降级成两张 answerer 表。本期不写前端 | 16-18 人日 | **2 个最后** |

评审对 B 的一致意见：Plan 状态机是**给 0/38 触发的路径新建结构**；Reply 信封**爆炸半径最大、尺子最弱**。B 只值得留三样：judge 四态、「新断言必须先被证明会红」的反向验证纪律、prompt 字符预算。

评审抓出的**假绿判据**（已在本计划中改掉）：`why-not-compose 本月销售额 按品牌` 会被 `serve.ps1` 的空格切参 + `args.get(2)` 静默丢掉维度 → 判据恒绿；`json_not_contains` 不是 `regression.py` 支持的键 → 负向断言恒过；`json_contains: ["品牌"]` 是整份 JSON 裸子串 → 列名里出现即通过。

---

## 4. 推荐方案 D（三条路线的合成，按三个评审的一致处拼）

### D0 · 修尺子（2 天，无功能面，无红线面）
1. `kb_eval.py`：夹具/探针失败**不许 `return 0`** —— 逐题记红继续跑；夹具缺失报「夹具缺失」而不是题红（否则先红先绿归因不明）
2. `serve.ps1` 挂 `tools/`（**可写，不能 `:ro`**，`--csv` 要往外写）
3. `regression.py`：未知断言键**报错**
4. `evaluation.py --runs N`：出失败集交集与抖动池 M；**自检**「M ≥ 9 否则判定它没在测抖动」
5. `why-not-compose --csv`：同批加 flag 解析；连跑两次逐列全等才算这把尺子无抖动
6. 给 28 条 `direct-agg` 题各加**一条数值断言**（今天 0 题钉数值）

**验收**：故意改坏一条断言 → 必须红（反向验证，写进 PR）。

### D1 · 知识库能用（3 天）
1. **前端上传入口**（今天为零）：拖拽 + 进度 + 失败原因 + 支持格式清单
2. trgm 阈值 + 向量路相关度下限
3. `kb.chunk` reembed 入口（CLI + API）
4. 引用回传 `span`
5. kb_eval 扩到 ≥16 题（含 PDF/docx/图片题、要点覆盖率、多文档冲突）

### D2 · AI 分析（2 天）← 用户点名
1. `summarize` 解耦：单问也出模型解读；复用 fast 模型 + `wrap_untrusted` + 拦网址，失败降级 `None`
2. `/record/{id}/analysis` 端点 + 前端面板（**开着开关测成本**）
3. 解读里必须写进**口径说明**（这个数是怎么算的），否则「分析」只是形容词
4. 确定性洞察放开多指标

### D3 · 问数准确（3 天，全部小成本高价值）
1. schema-linking 兜底两件（gather.rs，各几行）→ 直打 `①指标不命中 9 题`
2. 值链接反查修列名
3. judge 四态 + `check_caliber` 解析失败改为 grader_error
4. 升温重试
5. seed 的 `ON CONFLICT DO UPDATE` 加 `origin` 守卫（**这是唯一一个「现在就在悄悄吃掉运营改动」的真 bug**）

### D4 · 细节与美观（2.5 天，一次改完前端，不分两批）
三个未渲染字段 + 行数口径与脚注 + 权限过滤回显 + 表格排序/分页/合计 + 图表扩类

### D5 · 智能（3 天）
下钻维度池读注册表（每条建议**过一遍 `try_compose`**，判据落在 `route=direct-agg` 而不是「点击有反应」）+ 多轮喂上一轮 schema/SQL（**先给 regression 加 3 道两轮题**，否则改了没判据）。
**推荐追问已删**（两次判推迟，见上方 D5 缺失清单里的划线条目）。
⚠️ 下钻维度池的**顺序不能反**：直接把 `DIM_POOL` 从 6 个加宽到注册表的 10 个，会建议出
「按品牌」「按客户分类」这类**一点就落 LLM** 的维度（`regression_cases.json` 的 E09/E17 今天标
`llm: true` 且不钉 route）—— 那等于把下钻按钮接到失败集上。正确顺序：
① 先给那两题钉 `route: "direct-agg"`（今天必红，这就是反向验证）→ ② 修 compose 让它们过
→ ③ 才加宽 `DIM_POOL`，且每条建议都要过一遍 `try_compose` 再出。

### D6 · 格式扩展（3.5 天，**需业主先裁两件**）
Python 解析层容器化（解 SAC）→ 容器里造 docx/pptx/图片夹具 → OCR/扫描件/旧 Office
**待裁决**：① `pymupdf4llm` 的 AGPL-3.0 是否可用；② 容器体积（LibreOffice headless ≈ +500MB）

---

## 5. 明确不做（及触发条件）

| 不做 | 理由 | 什么时候再谈 |
|---|---|---|
| Plan/待办状态机、Reply 信封 | 给 0/38 触发的路径新建结构；信封爆炸半径最大而尺子最弱 | 有一条真实红题证明「单候选直出」是失败原因 |
| DataSet 逻辑表、S2SQL 逻辑 SQL 中间层 | 各是「semantic 新增两张表 + gather 重写」级大件，收益在天花板不在当下失败集 | 确定性装配覆盖过半（`direct-agg` ≥19/38） |
| 列权限统一到召回层 | `policy` **没有**列级 ACL（敏感列是 `GuardConfig` 静态表 + connector redact），这是新建 ACL 不是统一边界；且必须同批给 semantic-cache 加 principal 指纹 | 单独一轮，与缓存指纹同批 |
| 多工作空间 / 知识库跨空间授权 | 用户已推迟。**代价要照实说**：今天前端文案不该叫「企业知识库」，应叫「我的知识库」 | 业主要求共享时 |
| AI 预测（趋势外推） | 必须先解决「预测值须标注为模型外推、非口径产物」并进 guard 判据，否则会被当指标读 | 判据先行 |
| SSE 流式解读 | p95 的体验补丁，不是质量项 | D2 的早退参数先解决「机器调用方不为解读付延迟」这一半 |
| 每方言一份规则 YAML（SQLBot 12 份） | 方言名与引号已参数化且有断言钉着；今天只有 MySQL 生产 + PG 上传表两种 | 接入第三种方言那天 |
| SC 多路各采样不同 exemplar | SC 已裁决默认关且实测收益 0；给关着的功能补输入扰动是为不跑的代码干活 | 升温重试证明「打破确定性重复有收益」之后 |
| 同比 YoY / 环比覆盖面 | 要动 `agg_template` 让路那条裁决（二·AB），与 D3/D5 同处代码，同时开两刀会让抖动归因失效 | D5 收尾后 |

---

## 6. 红线与判据纪律（贯穿全程，违反即无效）

1. **DMS 生产 MySQL 只读**，任何写操作禁止（会话级 `SET SESSION TRANSACTION READ ONLY` 兜底）
2. 明文 DSN / API key **只在 `settings.json`** —— 不进库、不进日志、不进响应、**不烤进镜像层**
3. 三个源仓（xh-dms / xh-dms-fornt / xh-xcx）只读
4. 架构单向无环，`scripts/check-arch.ps1` 15 项门禁必须绿
5. 本机 Smart App Control 拦一切新链接的未签名产物 → **所有验证走容器**
6. **判据纪律**：抖动池 ≥9/38，单轮总分分辨不出 ±2 → 看 route + 值**逐题双验**与**多轮失败集交集**
7. **每条新判据必须做一次反向验证**（临时打破 → 证明会红 → 恢复），并写进 PR
8. 禁用四类判据：常见词的 `json_contains`、`include_str!` 的负向字面量断言、`regression.py` 不支持的键、落在噪声带里的延迟判据（p95 ±20%、单题 ≤3s 都不算证据）
9. 观测类计数（GraderError 数、registry-down 行数）明确标「只观测、不设门槛」，不许当验收
10. 迁移一律 `CREATE IF NOT EXISTS` + `ALTER ADD COLUMN IF NOT EXISTS`；`meta.*` **没有 migrator**，只有 `kb.*` 走 `0021+`

---

## 7. 需要业主裁决的事项（不裁则相应条目不动）

1. `pymupdf4llm` 的 **AGPL-3.0** 是否可用（影响 PDF 章节层级质量）
2. 容器体积：加 LibreOffice headless（旧二进制 Office）≈ +500MB，OCR 引擎另计
3. 「企业知识库」是否需要**跨用户共享**（今天是每人私有空间）
4. 三项遗留：SALE15 的商品维度口径、金额侧 `item_type` '1' vs '3'、B10 的 ~28s 超时处置
