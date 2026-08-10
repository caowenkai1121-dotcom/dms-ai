# dms-ai「SuperSonic + deepagents」深度整合蓝图

> ⚠️ **本文是动工前的计划，状态列已落后实况。当前实况看 `INTEGRATION-TRACE.md`**
> （逐条映射「来源机制 → 本仓 file:symbol → 状态 → 证明它的那条测试/实测」）。
> 下表里被标成 `missing`/`partial` 的这些已经落地并**连库量过**：
> 规则时间解析、精确词典层、Rubric 自评闭环、截断三件套、Memory 信任边界三条纪律、
> 查询统计日志、派生指标（占比）、快照最新值语义、model filter 表级口径、
> 分区时间维度判据、join 知识接进 LLM 路径。
> 本文保留不改，是因为它记录了**当初的判断依据与优先级理由** —— 那些理由仍然成立，
> 只有状态过期了；直接改状态列会把「为什么当时这么排」一起抹掉。

- 版本：v1.0（2026-07-26）
- 对象：dms-ai（Rust axum + Vue3 + PostgreSQL/pgvector/AGE 的 DMS NL2SQL 智能问数系统）
- 五大目标锚点：**准确、快、智能、漂亮、权限**
- 总原则：尊重现有四大骨架（pipeline 校正链 / meta 注册表 / direct 模板 / viewspec 呈现协议），一切改动走**外科式增量**：新校正器按「纯 AST 函数 + Option 返回 + log_correction」既有形状插入；新知识一律进 meta 注册表而非代码；禁止一次性重写文件。

---

# 一、两框架架构精要

## 1.1 SuperSonic：语义层驱动的确定性 NL2SQL 流水线

**核心思想**：headless「逻辑数据集」语义层。LLM 永远只面对一张虚拟宽表（S2SQL：`select 中文指标 from t_数据集ID`），JOIN 推导、表达式展开、聚合裁决、方言改写全部由确定性代码兜底——LLM 的幻觉面被压缩到最小，同时天然形成「语义 SQL → 物理 SQL」两级审计点。

**数据流文字图**：

```
用户问题
 │
 ├─ chat-server 编排层（SPI 顺序由 spring.factories 决定）
 │    ChatQueryParser → ParseResultProcessor → ChatQueryExecutor → ExecuteResultProcessor
 │    （多轮改写 rewriteMultiTurn / 错误话术改写 / 推荐处理器挂在这层）
 │
 ├─ headless-chat：ChatWorkflowEngine 显式状态机
 │    MAPPING ──6 个 Mapper：向量 / HanLP 双 Trie 词典 / 数据库 LIKE / 过滤器 / 分区时间 / 术语
 │    │        + MapMode 四档递进（STRICT→MODERATE→LOOSE→ALL 兜底）+ MapFilter 五规则净化
 │    PARSING ──双轨：LLM（few-shot 动态采样 + self-consistency 多路投票 + 失败升温重试）
 │    │              ‖ 规则（QueryMatcher 元素共现 + TimeRangeParser + 数据集启发式路由）
 │    S2SQL_CORRECTING ──8 个 Corrector 管道（Schema→Time→Select→Where→GroupBy→Agg→Having→LLM 评审）
 │    │                   全部 isComplexSQL 守卫，单件失败不断链
 │    TRANSLATING ──QueryParser 责任链：名称换码→AggOption 聚合裁决→派生表达式递归展开
 │    │             →同环比模板→本体 SQL（JoinRelation 图 Dijkstra 补桥接 + 主外键 ON 推导）
 │    │             →WITH/子查询合并内外层→QueryOptimizer（方言改写 + 强制 LIMIT + 谓词下推）
 │    PHYSICAL_SQL_CORRECTING ──物理 SQL 兜底修正（解析期即完成 dry-run 翻译验证）
 │
 ├─ 执行层：请求指纹 Caffeine 缓存 → JdbcExecutor
 │    权限以 AOP 切面强制：行权限注入 WHERE + 高敏感列拦截 + 维值别名换码回显 + 「已过滤」提示
 │
 ├─ 呈现层（chat-sdk）：getMsgContentType 决策树选 7 种组件（卡/表/趋势/柱/饼/文本/MD）
 │    + 两级下钻 + 关联指标切换 + 同环比红绿箭头 + 统一数字格式化 + textSummary 伪流式后补
 │
 └─ 记忆闭环：执行成功 → PENDING → LLM/人工复核 → 写入向量库 exemplar → few-shot 复用（越用越准）
```

**对 dms-ai 的定位**：SuperSonic 负责「把单次问答做准、做快」——它的映射/修正/翻译/呈现四层与 dms-ai 的 retrieve/corrector/pipeline/viewspec 一一同构，缺口即补丁清单。

## 1.2 deepagents：中间件洋葱上的上下文经济学与子代理隔离

**核心思想**：不造新运行时，用固定次序的中间件洋葱把「上下文管理、权限、子代理、记忆、自评」全部做成对「本次模型请求」的改写（wrap_model_call / wrap_tool_call），state 保持全量真相不破坏。

**数据流文字图**：

```
create_deep_agent 装配（固定栈序，同名替换保持栈位）
  Skills → Filesystem → SubAgent(task) → Summarization → PatchToolCalls
  → AsyncSubAgent → [用户中间件插入点] → Profile 尾部 → PromptCaching → Memory → HITL

每轮请求循环：
 wrap_model_call ─ 悬空 tool_call 修补 → 85% 阈值摘要压缩（历史 offload 到
 │                /conversation_history/*.md，摘要内嵌回查路径）→ 旧工具大参数预截断
 │                → 超大消息驱逐 → 记忆(AGENTS.md)注入 → Anthropic 缓存断点
 ▼
 模型输出 tool_calls
 ▼
 wrap_tool_call ─ 三态权限（allow / deny / interrupt→人审 approve/edit/reject/respond）
 │               → 工具执行（Backend 协议：State/Store/Filesystem/Composite 前缀路由）
 │               → 大结果 >20k token 驱逐落盘，替换为「路径 + head/tail 预览 + 分页续读指引」
 ▼
 task 子代理 ─ 父 state 剥离私有键 → 独立循环 → 回传最后一条非空 AIMessage 或 structured_response
 ▼
 最终答案 → RubricMiddleware 启动 grader 子代理按清单评审 → needs_revision 回炉迭代 → satisfied
```

**对 dms-ai 的定位**：deepagents 负责「把长会话与复杂任务做稳」——上下文不爆炸、悬空调用不崩、复合拆解不失控、高危操作可人审、产出可自评闭环。dms-ai 的复合拆解/并行子查询已是其简化版，补的是防御细节与闭环件。

## 1.3 整合分工一句话

**SuperSonic 补「一次问答的确定性」（准确/快/漂亮/权限回显），deepagents 补「长期运行的鲁棒性」（上下文经济、权限三态、Rubric 自评、后台任务）**。两者在 dms-ai 中共用同一套 meta 注册表与权限内核，不引入新运行时。

---

# 二、差距矩阵

> 列说明：现状为 partial 时在同格写清**缺什么**。价值列标注对应五大目标。优先级：P0=直接提升准确性/权限安全，P1=智能与速度，P2=呈现体验，P3=锦上添花。

## A. 语义翻译与 SQL 生成（来源：SuperSonic headless）

| 机制 | 来源 | dms 现状（partial 缺口） | 价值 | 量 | 级 |
|---|---|---|---|---|---|
| AggOption 聚合裁决（防内外层双重聚合） | SS-headless | **missing** | 准确（数字对错的硬伤防线） | S | **P0** |
| 执行前预翻译验证（EXPLAIN dry-run + 失败回喂修正） | SS-headless | partial：有只读预检，缺 EXPLAIN 验证列/类型 + 失败信息回喂 repair | 准确 | S | **P0** |
| PG 方言归一 + LIMIT 兜底 | SS-headless | partial：缺 MONTH/DATE_FORMAT/IFNULL→to_char/coalesce 函数映射、反引号→双引号、AST 级 LIMIT 判定（现用 contains） | 准确+快 | S | **P0** |
| MetricDrillDownChecker（必要/允许下钻维度校验） | SS-server | **missing** | 准确（口径护栏，错误可读） | S | **P0** |
| 同环比模板算子（RATIO_ROLL/RATIO_OVER） | SS-headless | **missing**（direct 仅有 prev_window 环比雏形） | 准确+智能（最高频问法） | M | **P1** |
| 派生指标递归展开（指标套指标） | SS-headless | partial：注册表只存单层表达式，缺 define_type、递归展开+环检测、展开底层列并入白名单 | 准确 | M | **P1** |
| JOIN 图 Dijkstra 桥接 + 主外键 ON 推导 | SS-headless | partial：direct::try_compose 有 join_edge BFS≤3 跳，缺雪花桥接、identify 主外键 ON 推导、**LLM 路径完全未接入 join 知识** | 准确+智能 | L | **P1** |
| 时间谓词下推（外层时间条件克隆进内层） | SS-headless | partial：无 | 快（大表聚合数量级提速） | M | P1 |
| 两层 SQL 架构（逻辑数据集虚拟视图） | SS-headless | partial：dms 直面物理表+口径卡，已部分达成同效；完整视图化是架构演进备选 | 准确 | L | P3 |

## B. 映射召回与解析（来源：SuperSonic chat）

| 机制 | 来源 | dms 现状（缺口） | 价值 | 量 | 级 |
|---|---|---|---|---|---|
| MapFilter 五规则净化（短词/包含词/满分优先） | SS-chat | **missing** | 准确（最廉价提升点） | S | **P0** |
| 精确词典层（aho-corasick/fst 多模式匹配，精确>向量合并） | SS-chat | partial：只有向量召回 EmbeddingMapper，缺零延迟专名精确命中与前后缀联想索引 | 准确+快 | M | **P1** |
| MapMode 四档递进（严格命中即停→宽松→全量兜底） | SS-chat | **missing** | 准确+快 | M | **P1** |
| 规则时间解析（三级正则+中文数字栈） | SS-chat | **missing**（时间全靠 LLM） | 准确（时间是 BI 最高频错误源） | M | **P1** |
| SC 多路投票 + few-shot 动态采样 + 升温重试 | SS-chat | **missing**（有 exemplar 记忆，无投票/去重竞争/升温） | 准确（难题稳定性） | M | **P1** |
| Prompt Schema DSL 裁剪 + SideInfo | SS-chat | partial：缺按映射命中裁剪 schema、FORMAT/default_agg 元信息进 DSL、CurrentDate/PG 版本提示 | 准确+快(token) | S | **P1** |
| Embedding 细节（子串豁免阈值 / LLM 二筛开关） | SS-chat | partial：缺两项细节 | 准确 | S | P1 |
| 多轮上下文继承守卫 | SS-chat | partial：有 rewrite_followup，缺 histSQL 为空跳过、规则级 dateInfo/filter 继承（省 LLM 调用）、当前问题先 map 再进改写 prompt | 快+准确 | S | **P1** |
| 记忆闭环完整版 | SS-chat | partial：有 review 闭环，缺创建去重、状态↔向量库双写一致、启动重放（向量库当缓存）、内置系统样例 | 智能（越用越准不腐化） | M | **P1** |
| 执行后处理器族（相似指标/维度推荐、失败话术改写、相似问题） | SS-chat | partial：有 textSummary，缺其余三件 | 智能+漂亮 | M | P2 |
| 规则解析器族 QueryMatcher（元素共现零 LLM 解析） | SS-chat | missing（direct 四层已是等效简化版，边际价值低） | 快（兜底） | L | P3 |
| 输入联想 RetrieveService | SS-chat | **missing** | 漂亮+智能 | M | P3 |

## C. 权限与安全（来源：SuperSonic + deepagents + dms 自审计）

| 机制 | 来源 | dms 现状（缺口） | 价值 | 量 | 级 |
|---|---|---|---|---|---|
| **会话归属越权修复**（conv_owner 已实现从未接线） | dms 自审计 | **缺陷**：任意登录者可读/写他人会话 | 权限（红线） | S | **P0** |
| **binding_of 数据化 + fail-closed** | dms 自审计 | **缺陷**：硬编码 8 表，未绑定表=不注入=放行 | 权限（红线） | S | **P0** |
| is_safe_select / ensure_limit AST 化 | dms 自审计 | 隐患：子串匹配误拦/漏判 | 权限+准确 | S | **P0** |
| 高敏感列拦截 + 行权限生效回显 | SS-server | partial：view_type 行权限内核完备，缺 sensitive_level 列级拦截+「联系管理员」提示、响应 authorization 字段回显 | 权限+漂亮（透明信任） | M | **P0** |
| 权限核心单测面（scope/inject 零单测） | dms 自审计 | 缺陷：403 行权限计算只靠连库判官 | 权限 | S | **P0** |
| 缓存 key 纳入权限上下文（防跨用户越权命中） | SS gotcha | 设计约束（随结果级缓存落地） | 权限 | S | **P0** |
| 三态权限 + interrupt 人审（HITL） | deepagents | partial：只读红线≈单条 deny，缺声明式规则表 first-match、高危查询转审批第三态 | 权限 | M | P2 |
| Memory 信任边界（记忆非指令/凭据禁令/口径注明来源日期） | deepagents | partial：缺三条提示词纪律 | 准确+权限 | S | **P1** |

## D. 性能、上下文与 Agent 基建（来源：SuperSonic + deepagents）

| 机制 | 来源 | dms 现状（缺口） | 价值 | 量 | 级 |
|---|---|---|---|---|---|
| 结果级指纹缓存（moka：规范化 SQL+scope 哈希→结果集） | SS-core | partial：只有「问题→答案」语义缓存层 | 快（多轮改图零查询） | S | **P1** |
| 查询统计日志（sql/耗时/cache_hit/useCnt） | SS-server | **missing** | 快（观测）+智能（热度排序供数） | S | **P1** |
| 大结果落盘/分页 + 截断三件套（原因+范围+续读参数） | deepagents | **missing** | 快+准确（上下文保护） | M | **P1** |
| Rubric 自评迭代（grader 按清单评审→回炉≤2 轮） | deepagents | **missing**（最高杠杆件：把静态校验升级为闭环自修复） | 准确 | M | **P1** |
| HarnessProfile 模型画像（model_profiles.toml） | deepagents | **missing**（sonnet 三段 suffix 可零成本直接抄） | 智能+成本 | S | **P1** |
| 子代理隔离细节对照 | deepagents | ported，需对照补：state 剥离白名单、最后非空消息回溯、非法类型友好错误、并发指令入工具描述 | 准确+权限 | S | P1 |
| 复合拆解编排提示词对照 | deepagents | ported，需补：默认不拆偏置、并发/轮数硬上限、合并阶段指标口径对齐协议 | 快+准确 | S | P1 |
| PatchToolCalls 悬空修补 | deepagents | missing——**dms 当前无 tool-loop，引入 agent 循环时为前置必做件** | 稳定 | S | P2(条件) |
| 摘要压缩 + 会话历史 offload | deepagents | **missing** | 准确（长会话不漂移） | L | P3 |
| AsyncSubAgent 后台任务（发起即返回+任务面板） | deepagents | **missing** | 智能+漂亮 | M | P3 |
| Skills 渐进披露（方法论字典） | deepagents | **missing** | 智能+token | M | P3 |
| Backend 统一存储协议 + 前缀路由 | deepagents | **missing** | 架构地基 | L | P3 |
| 中间件装配 trait 化（洋葱链） | deepagents | partial：pipeline 顺序串行已近似 | 可维护性 | M | P3 |

## E. 呈现与交互（来源：SuperSonic webapp/chat-sdk）

| 机制 | 来源 | dms 现状（缺口） | 价值 | 量 | 级 |
|---|---|---|---|---|---|
| 图表决策树边界护栏 + show_type 列元数据 | SS-exp | partial：viewspec 决策树已有，缺饼图全非负/趋势图日期值须变化/柱图 isFinite/行数分端阈值 | 漂亮+准确 | S | **P2** |
| 指标卡同环比 statistics + 红绿箭头 | SS-exp | **missing**（有 patch_kpi_delta 环比雏形；依赖 P1 同环比算子） | 漂亮+智能 | M | **P2** |
| 两级下钻 + 关联指标/周期切换（query-data 局部刷新） | SS-exp | partial：有 infer_drill+drill 重问，缺不产生新消息的局部刷新、推荐指标切换、周期快捷档 | 智能+漂亮 | M | **P2** |
| 解析结果可编辑再查询（筛选/操作符/时间控件化） | SS-exp | **missing** | 准确+智能（错值原地修正） | M | **P2** |
| 多候选解析选择 + dataCache（歧义交还用户） | SS-exp | **missing** | 准确+反馈语料 | M | P2 |
| textSummary 异步后补（图先出、总结后到） | SS-exp | partial：需确认非同步阻塞；补轮询/SSE 上限 | 快 | S | **P2** |
| 数字格式化统一（列元数据驱动） | SS-exp | partial：format.ts 已有，缺 data_format_type/decimal_places 下发、0 值口径统一 | 漂亮 | S | P2 |
| ApplyAuth/NoPermissionChart 占位 + authorization 提示条 | SS-exp | partial：权限内核有，缺列标 authorized:false 不删列 + 前端占位组件 | 权限+漂亮 | S | **P2** |
| 消息工具条（赞踩反馈→记忆复核 / PNG 导出 / 再试一次） | SS-exp | partial：有 CSV 导出，缺其余三件 | 智能闭环 | S | **P2** |
| SqlItem 六段 SQL 调试面板 + 导出现场 | SS-exp | **missing** | 准确（排障效率） | S | **P2** |
| 相似问题推荐（失败态默认展开） | SS-exp | **missing** | 智能（死胡同变引导） | S | P2 |
| 图表渲染配方（渐变柱/补零趋势/环形饼/tooltip 排序） | SS-exp | partial：BiChart 有单色纪律，可按条目择优吸收 | 漂亮 | M | P2 |

## F. 评测基建（来源：SuperSonic evaluation/benchmark）

| 机制 | 来源 | dms 现状 | 价值 | 量 | 级 |
|---|---|---|---|---|---|
| exec-only 回归评测管线（结果集比对+tags 分层+error_case.json+CI 门禁） | SS-exp | **missing**（regression.py 53 题依赖连库+LLM，无法离线门禁） | 准确（一切改动的验收地基） | M | **P0** |
| benchmark 业务题冒烟（成功率+p50/p95+commit hash 基线） | SS-exp | **missing** | 快+准确 | S | **P0** |

## G. dms 自审计债务（非框架机制，随各期顺带清偿）

| 债务 | 级 | 归属期 |
|---|---|---|
| api_ask 存 AI 消息 role/question 语义错乱；前端 routeLabel 缺映射 | P0/S | 第 1 期顺带 |
| 时间词枚举三处独立（direct×2 + pipeline） | P1/S | 第 3 期（同环比时合一） |
| 种子知识 const 硬编码（WARNS/METRICS/DIMENSIONS/value_map）迁表 | P1/M | 第 3~4 期渐进 |
| correction_log/failure_log 只写不读，S4 升格器未实现 | P1/S | 第 4 期 |
| chrono_today UTC 手算与 chrono::Local 两套日期源 | P1/S | 第 2 期顺带 |
| 进程内状态（SESSIONS/SCOPE_CACHE/TOKEN）不可多实例 | P3/M | 第 6 期 |
| meta.rs 1303 行七种职责，pipeline/direct 逼近巨型 | P2~P3 | 各期只拆不涨，第 6 期收口 |

## H. 已移植且无重大缺口（一行带过）

权限内核 1:1 复刻（scope/inject/principal + 判官对拍 6/6）、三道只读防线、12 张 meta 表幂等体系、三路召回+五路口径卡、direct 四层快路径（单号/模板/组合器/图）、四件 AST 校正器、语义缓存（时间/数字护栏）、多轮改写+复合拆解并行、记忆复核闭环骨架、autodiscover 字典对码、AGE 图问答+nightly 重建、ViewSpec 决策树+洞察+下钻推荐、三端认证、多会话持久化、embed 熔断降级、前端对话流 BI——**均为整合的地基而非改造对象**。

---

# 三、分期开发计划（六期）

> 排期原则：P0=准确硬伤+权限红线优先（第 1~2 期），P1=智能与速度（第 3~4 期），P2=呈现体验（第 5 期），P3=锦上添花（第 6 期）。每期改动精确到文件；每期结束跑回归门禁。

## 第 1 期：权限红线 + 回归门禁（P0，约 2 周）

**目标**：堵死所有已知越权/放行漏洞；建立离线可跑的评测门禁，为后续五期提供验收标尺。

**具体改动**：
1. `crates/server/src/main.rs`：`api_conv_msgs`/`api_ask` 入口调用已实现的 `chat::conv_owner`，非属主返回 403；顺带修复 `save_msg(cid,"ai",&req.question,...)` 把用户问题存进 ai 行的错乱。
2. `crates/server/src/meta.rs`：`migrate` 新增 `scope_binding` 表（table_name/customer_col/owner_col/owner_kind）；把 `inject.rs::binding_of` 的 8 张硬编码表灌为种子行。
3. `crates/server/src/inject.rs`：`binding_of` 改为查表（启动加载+缓存）；新增敏感表审计——SQL 涉及**未登记表**时按配置 fail-closed 拒绝或告警（默认拒绝，白名单豁免非敏感表），杜绝「忘登记即放行」。
4. `crates/server/src/pipeline.rs`：`is_safe_select` 改用 sqlparser AST 语句类型判定（仅单 Query，禁多语句），FORBIDDEN 关键词检查移到 AST 层避免字面量/注释误拦；`ensure_limit` 改 AST 判定 LIMIT 节点。
5. `crates/server/src/scope.rs`/`inject.rs`：将 tools/judge_scope.py 的对拍用例固化为**离线单测**（mock 员工/角色数据，覆盖 view_type 0/1/2/3/10/101/102/103、哨兵 -1、空集、超管短路）。
6. 评测基建：`tools/evaluation.py`（docker compose 固定 PG 测试库+种子数据；题集 YAML `{question, gold_sql, tags}`；两侧 SQL 各自执行后按「列名→排序值列表」比对，浮点容差，忽略别名；按 tags（聚合/明细/时间/同环比/多轮/权限）分层统计；失败落 `error_case.json`）；`tools/benchmark.py`（真实业务题批量 parse/execute，成功率+p50/p95，输出带 git commit hash 的 CSV）。接入 CI：通过率低于基线即红。

**验收标准**：GET/写他人会话返回 403；未登记敏感表查询被拒且日志可查；scope/inject 单测 ≥30 例全绿；evaluation 首版基线报告产出并入 CI。
**回归要求**：regression.py 53 题全绿；judge_scope 对拍 6/6；evaluation 基线归档为后续各期对照点。

## 第 2 期：准确硬伤防线（P0，约 2~3 周）

**目标**：补齐 SuperSonic 修正链/翻译层中直接决定「数字对不对」的缺件。

**具体改动**（全部按 `corrector.rs` 既有「纯 AST 函数 + Option + log_correction」形状插入 `pipeline.rs::ask_single` 校正链）：
1. `corrector.rs` 新增统一守卫 `is_complex_sql`（含子查询/UNION 即跳过修正），现有四件+新件全部前置接入。
2. `corrector.rs::agg_option_guard`：检测外层含 count_distinct/子查询/GROUP BY 时，保证不生成内层预聚合（约 100 行，防双重聚合错数）。
3. `corrector.rs::time_corrector`：读 meta.dimension 分区时间与默认时间配置——SQL 无时间条件时补默认区间（注入前给既有 WHERE 加括号防 OR 优先级）；只有上界补下界；数据集无分区维度时**自行实现**「收集日期字段再删除」（原版 removeDateIfExist 是 no-op bug，勿照抄）。
4. `corrector.rs::select_corrector`（groupby 字段补进 select，WHERE 等值字段免补）、`having_corrector`（HAVING 裸指标补 default_agg）、`unmapped_value_remover`（WHERE 等值维度条件的值未被 correct_value 值链接命中则删除，豁免日期/数值/函数包裹——防幻觉值）。
5. `pipeline.rs`：generate_sql 后加 **PG 方言归一 pass**（MONTH/DAY/YEAR/DATE_FORMAT/IFNULL→to_char/coalesce 函数映射表 + 反引号→双引号）；execute 前对 PG 发 `EXPLAIN` dry-run，失败信息喂 `repair` 重试一次（复用既有 repair 挂点）。
6. `meta.rs`：metric 表加 `necessary_dims`/`allowed_drill_dims` 数组列；`pipeline.rs` 执行前加 `drill_down_check`（缺必要维度/超下钻清单→可读错误返回用户与修正器）。
7. `meta.rs::retrieve` 出口接 `map_filter` 纯函数（五规则：数据集过滤/≤1 字剔除/≤2 字须满分/满分优先/包含关系取最长），逐规则单测对照 Java 语义。
8. 顺带：统一日期源为 `chrono::Local`，删除 `chrono_today` 手写 UTC 算法。

**验收标准**：评测集新增「双重聚合/时间兜底/幻觉值/下钻口径」四类用例且通过；每个新校正器有独立单测；correction_log 可观测各新件出手率。
**回归要求**：第 1 期评测基线不回退；benchmark p95 不劣化。

## 第 3 期：智能与稳定（P1，约 3 周）

**目标**：映射精确化、时间规则化、生成投票化、记忆闭环补强——LLM 依赖度下降，难题稳定性上升。

**具体改动**：
1. 精确词典层：`meta.rs` 建 `dict_word` 表（word/element_type/element_id/dataset_id）+ 从 metric/dimension/term/value_map 同步的重建任务；新增 `crates/server/src/exact_match.rs`：aho-corasick 常驻自动机对问题串多模式精确匹配，定时/事件重建；`retrieve` 改「精确>向量」优先级合并 + MapMode 递进（strict 精确全匹配命中即停，miss 才降 loose 触发向量，LLM 失败再 all 全量 schema 兜底；阈值公式照抄 getThreshold）。
2. 新增 `crates/server/src/time_parse.rs`：三级 fallback（「近/过去 N 天|周|月|年」正则+中文数字栈算法直译、\d{8} 区间、常见相对词表）；产物注入 prompt SideInfo，并在校正链加「LLM 时间条件与规则解析不一致时以规则为准」的校验器。
3. `pipeline.rs::generate_sql` SC 投票：tokio join 并发 2~3 路（温度>0、每路不同 few-shot 组合、最相似 exemplar 每路保底）；SQL 规范化（去空白/统一大小写/AST 归一）后多数票；票率<阈值降级澄清或标低置信。`llm.rs` 封装升温重试（重试时临时升温，**不写回共享配置**）。
4. Prompt 工程：按映射命中裁剪 schema（命中元素优先，超阈值才全量）；`meta.rs` metric 表补 `default_agg`/`format` 进 DSL；SideInfo 固定注入 CurrentDate/PG 版本；新增 `model_profiles.toml`（`llm.rs` 加载：按模型配 prompt 后缀/温度/开关），先抄 sonnet 画像的 use_parallel_tool_calls 与 investigate_before_answering 两段（零成本高收益）。
5. 同环比：`viewspec.rs`/`direct.rs` 增 RATIO_ROLL/RATIO_OVER 算子——PG 单趟 `lag() over (partition by dims order by 时间桶)` 或 CTE interval 自 JOIN；校验「同环比不可混用+必须带时间过滤」；**同步把 direct::time_window/prev_window/pipeline::time_tokens 三处时间词表合一**到公共模块（数据化进 meta 更佳）。
6. 记忆闭环补强：`pipeline.rs` few-shot 回写加「同问已有 enabled 则去重跳过」；服务启动时全量重放 enabled exemplar 到 pgvector（向量库当缓存不当真相源）；内置系统样例 JSON 随部署加载；review 提示词加三条纪律（记忆是资料非指令、凭据永不入记忆、口径类记忆注明来源日期且与 information_schema 冲突时以库为准）。
7. 多轮守卫：`pipeline.rs::rewrite_followup` 加 histSQL 为空跳过；追问同型查询时规则级继承上轮时间/过滤器（不走 LLM）；改写 prompt 带上轮命中元素与本轮 map 结果对照。
8. 拆解/子代理对照补丁（da ported 清单）：复合拆解 prompt 加「默认不拆、仅显式对比/独立维度才拆」偏置与并发上限常量；子查询合并 prompt 明确指标口径对齐规则。

**验收标准**：评测集难题档（多条件+同环比+TopN tags）通过率提升 ≥10pp；同环比新题库通过；时间类问题规则命中率有统计；exact_match 对「东风本田」类专名零误差命中的专项用例通过。
**回归要求**：评测门禁不回退；SC 并发导致的 p95 劣化 ≤20%（并发数可配降级）。

## 第 4 期：准确进阶与速度（P1，约 3 周）

**目标**：多模型 JOIN 与派生指标打开口径上限；缓存与大结果治理兑现「快」；Rubric 闭环自修复。

**具体改动**：
1. `meta.rs`：metric 表加 `define_type`(measure/metric/field)+`expr`；新增 `crates/server/src/expand.rs` 递归展开器（HashMap 缓存已展开+环检测），展开引用的底层列**并入 schema_check 白名单集合**；接入 pipeline 与 direct 两条路径。
2. `direct.rs::try_compose`：join_edge BFS 升级 petgraph dijkstra（覆盖全部查询模型的最短路径，中间桥接模型自动补入——星型一跳先行，雪花二期）；join_edge 表补 identify(primary/foreign) 元数据做 ON 推导；LLM 路径 prompt 注入 join 路径提示段（generate_sql 段落化装配的新段）。
3. 结果级缓存：新增 `crates/server/src/cache.rs`（moka，key=`blake3(规范化SQL + scope 上下文哈希)`，TTL 可配）；**缓存 key 必含权限上下文**并配越权专项测试；`meta.rs` 加 query_log 表（sql/耗时/cache_hit/user/route），供 useCnt 热度与慢查询观测。
4. 大结果治理：`pipeline.rs::execute` 结果超行/字节阈值时只回传前 N 行预览+result_id（结果暂存 PG）；`main.rs` 加 `GET /api/result/{id}?offset&limit` 分页接口；所有截断出口统一附「截断原因+已展示范围+精确续读参数」三件套。
5. Rubric 自评：`pipeline.rs` 出口（SQL+ViewSpec+洞察产出后）调 fast 模型 grader，JSON rubric：字段全在白名单/聚合有 GROUP BY/口径匹配注册表/图表符合决策树/洞察有数字支撑；EXPLAIN 结果作为验证证据输入；needs_revision 携 gaps 回炉 ≤2 轮（防延迟失控）。
6. 时间谓词下推：若 SQL 含 CTE/子查询结构，把外层顶层 AND 的时间列条件克隆注入内层 WHERE（保守规则，失败静默跳过——优化必须纯增益）。
7. S4 升格器：`main.rs` 加 CLI 子命令 `correction-promote`（读 correction_log/failure_log 聚合同错计数 ≥3 → save_lesson_candidate → 走既有 review_lessons 复核链），补齐自进化最后一公里。

**验收标准**：二阶派生指标用例（毛利率=毛利/收入，毛利=收入-成本）通过；多模型 JOIN 用例（订单+客户+门店）通过；缓存命中率与 p50 改善有报表；不同 scope 用户缓存隔离专项测试通过；rubric 回炉率/修复率可观测。
**回归要求**：evaluation+benchmark 双门禁；越权专项（缓存/结果分页接口均校验归属）。

## 第 5 期：呈现体验（P2，约 3 周）

**目标**：从「能出图」到「可探索、可修正、可信任」的报表体验。

**具体改动**：

后端：
1. `viewspec.rs`：决策边界补齐——饼图全非负+行数上限（桌面 10/移动 5）、趋势图要求日期值确有变化否则降级卡/表、柱图 isFinite、表格兜底；QueryColumn 加 `show_type` 由注册表回填（去掉列名 contains('date') 的命名耦合）；`patch_kpi_delta` 扩展为 statistics 口径映射（日粒度=日环比+周同比、周=周环比+月同比、月=月环比+年同比，tokio::join 并行对比期查询，复用第 3 期 RATIO 算子）——**判涨跌用数值符号，勿抄 includes('-')**。
2. `main.rs` 新增接口：`POST /api/chat/query-data`（携 parse_id+改写后 dimensions/metrics/date_info/filters，复用语义上下文重生成 SQL、全程过校正链+权限注入，局部刷新不新增消息）；`GET /api/dimension-values`（注册表+权限过滤+ILIKE+limit）；`GET /api/query/{id}/summary`（textSummary tokio::spawn 异步生成落库，前端轮询/SSE，**设轮询上限与超时**）；`GET /api/query/{id}/similar`（pgvector 召回历史高分问题 top5）；`POST /api/feedback`（query_id+score 落库，score≤1 的坏 case 喂 review_failure）；`DELETE /api/query/{id}`（再试一次）。
3. parse 响应扩展：透传 `sql_info{parsed, corrected, final}`（脱敏）与结构化 `dimension_filters`；歧义度高（多指标同分/多数据域）时返回 `candidate_parses` 数组；列权限裁剪改为**不删列而标 authorized:false**，行权限生效时附 `authorization_message`。

前端（web/）：
4. `ResultPanel.vue`：图卡 footer 两级下钻 chips+关联指标切换+周期快捷（近7/30/90 天、本月/上月），全部走 query-data 局部刷新（loading 包图表）；insight 条下加 authorization 提示。
5. 新增 `FilterEditor.vue`：按值类型分发（字符串→远程搜索多选/数值→操作符+输入/日期→区间选择器带预设），改完重查走同一 query-data。
6. 新增 `SqlDebug.vue`：sql_info 三段 tab+高亮+复制+一键导出调试文本（按开发者角色可见）。
7. `App.vue`：候选解析卡片点选（结果按 parse_id 前端缓存）；工具条补赞踩/PNG 导出（echarts getDataURL pixelRatio:2）/仅尾条「再试一次」；相似问题折叠区（失败态默认展开）；routeLabel 补 semantic-cache/compound/llm+schema-fix 中文映射。
8. `format.ts`/`BiChart.vue`：格式化改列元数据驱动（data_format_type/decimal_places 下发）；0 值正常显示不吞；趋势补零改由 show_type==DATE 触发且缺失日期填 null 而非 0；CSV 导出保留 BOM。

**验收标准**：设计走查通过（明暗双主题、受限用户占位视图）；「一问之后不打字」完成下钻/换指标/换周期/改筛选闭环；赞踩数据入库并被复核任务消费；总结不阻塞首屏（首屏出图 <1s 目标）。
**回归要求**：evaluation 门禁；前端关键交互 e2e 冒烟（下钻/改筛选/候选切换/权限占位）；query-data 接口越权专项。

## 第 6 期：锦上添花与架构还债（P3，按需排布）

**目标**：长会话、后台任务、方法论沉淀与可扩展性。

**具体改动**：
1. 输入联想：`main.rs` 加 `GET /api/suggest`（pg_trgm 前缀索引 over 指标/维度/维度值注册表），前端输入框 debounce 接入。
2. 摘要压缩+历史 offload：chat.msg 保全量真相；conv 表加 summarization_event(cutoff_msg_id, summary_text)；构建 LLM 请求按 event 重建有效消息=摘要+cutoff 之后；淘汰段写 conversation_archive 表，摘要内注明 archive_id 可回查；**token 估算按中文 ≈1.5~2 字符/token 校准，勿照抄 4 字符近似**。
3. 后台任务：tasks 表（status/result）+start/check/cancel 三接口（发起即返回 task_id，提示词写明「启动后不要立即查状态」）；月度归因报告、review_all_pending 批量复核（现串行 await 改后台并发）迁入；前端任务卡片。
4. Skills：meta 加 skills 表（name/description/body/scope），系统提示只注入索引，`load_skill` 按需取正文；首批技能=图表选型决策规范、同环比口径规范、主题域分析套路。
5. 三态权限 interrupt：权限规则表 Vec<Rule{ops,patterns,mode}> first-match；高危查询（超大扫描/敏感域）写 pending_approval 表推前端审批卡片（批准/改写 SQL/拒绝/答复）；若未来引入 tool-loop agent 循环，先落 PatchToolCalls 修补函数（约 30 行）。
6. 还债：auth::SESSIONS/scope::SCOPE_CACHE/wework::TOKEN 外置 PG（多实例部署前提）；meta.rs 拆分为 meta/{ddl,seed,recall,discover,log} 子模块（纯移动不改逻辑，分步小 PR）；种子知识全部迁表+管理页；inject 与 direct 的口径双写收敛到注册表单一来源。

**验收标准**：长会话（>50 轮）追问准确率专项不漂移；后台任务全链路（发起/查询/取消/推送）可用；skills 首个闭环验证。
**回归要求**：全量评测+benchmark；多实例部署冒烟（会话/缓存外置后）。

---

# 四、风险与坑

## 4.1 上游源码 bug——移植时严禁照抄

1. **DataModelNode.findBaseModel**：统计维度计数却按指标计数排序（复制粘贴错误），纯维度查询基模型选择不可靠。
2. **BaseSemanticCorrector.removeDateIfExist**：先 new HashSet 再 removeIf，集合恒空，整函数是 no-op——第 2 期 time_corrector 须自行实现「收集再删」。
3. **MetricRatioCalcProcessor 127-128 行**：判断同比值非空却 set 成环比值，同比被环比覆盖。
4. **PeriodCompareItem 用 includes('-') 判涨跌**：'0%' 判涨、异常值判涨——第 5 期用数值符号判定。
5. **DataInterpretProcessor**：静态非并发 HashMap 有竞态，且问题覆盖条件写反。
6. **PluginManager.getEmbeddingId**：向量清理永远删不掉旧向量（不移植 Plugin 可忽略）。
7. **SqlQueryParser.getAggOption 末尾分支**：注释说返回 NATIVE 实际返回 DEFAULT——以注释意图为准复刻。
8. **HavingCorrector.addHavingToSelect** 是死代码；GroupByCorrector 受环境变量隐藏开关——按语义移植，不按调用链移植。

## 4.2 语义陷阱与默认值陷阱

- **执行顺序藏在 spring.factories**：Mapper/Parser/Processor 序代码里看不出；尤其「RuleSqlParser 开头有 candidateQueries 非空即返回」——实际语义是「LLM 成功规则不跑、LLM 失败规则兜底」，方向极易搞反。
- **默认值≠机制生效**：PARSER_SELF_CONSISTENCY_NUMBER 默认 1（开箱无投票）、PARSER_RULE_CORRECTOR_ENABLE 默认 false——别高估默认流水线，dms 落地时显式配置并在评测中验证开关效果。
- **静默失败泛滥**：BaseMapper/BaseSemanticCorrector 全部 catch 只打日志，带病 SQL 静默放行——dms 侧改为显式错误传播或至少打降级标记入 correction_log。
- **DefaultDimValueParser 语义是兜底非合并**：只在整条 SQL 无任何 WHERE 字段时才注入默认维值，简化版易做成「缺该维度就补」。
- **few-shot 三集合逻辑（same/noSame/mostSimilar）耦合易 off-by-one**：按报告建议重写为「近似同题必选 + top1 最像 + 随机补足」三段式，勿直译。
- **EmbeddingMapper 在 LOOSE 与 LLM_OR_RULE 阶段重复检索**（上游自认浪费）：MapMode 递进实现时传递已映射状态，避免双倍向量调用。
- **升温重试写回共享配置对象**（上游副作用污染后续请求）：llm.rs 封装为请求级参数。
- **翻译快捷路径**：请求已带 querySQL 则直接跳过翻译（多轮改图表复用上次物理 SQL）——query-data 接口实现时别漏，否则改图表也走全链路白白变慢。

## 4.3 权限安全红线（fail-closed 纪律）

- **行权限表达式解析失败必须阻断**：上游 JSQLParserException 仅记日志不阻断=权限静默失效；dms 版任何权限注入失败一律拒绝查询。
- **缓存 key 必含权限上下文**：结果级缓存/语义缓存复用 SQL 时，scope 哈希不进 key 即发生跨用户越权命中——第 4 期专项测试锁死。
- **行权限多角色按 OR 并集**：与 dms view_type MAX 合并规则需显式对齐并写成单测，防止两套语义打架。
- **工具层权限对「能执行任意代码/SQL 的通道」无效**（deepagents 明确 NotImplementedError）：dms 等价约束是——永不提供自由 SQL 执行工具面，一切 SQL 必经校正链+权限注入+只读连接三道防线。
- **子代理权限是整体替换非合并**：复合拆解的子查询必须继承当轮 scope，不允许子路径重新计算或缺省放行。
- **未登记表 fail-open 是 dms 当前最大权限暗坑**：第 1 期 scope_binding 数据化+默认拒绝为最高优先。

## 4.4 中文与阈值校准

- **token 估算**：deepagents 全部阈值按 4 字符/token 近似，中文实际 1.5~2 字符/token——直接照搬会导致落盘/压缩触发过晚一倍以上，所有阈值按中文重标定。
- **中文别名必须反引号/双引号包裹**，ORDER BY 聚合别名换 select 序号——PG 侧统一双引号，规则进方言归一 pass。
- **编辑距离阈值（名称 0.3/维值 0.5）是「宽召回+MapFilter 强净化」的组合拳**：只抄召回不抄净化必然劣化——第 2 期 map_filter 与第 3 期 exact_match 必须成对上线。
- **子串豁免规则**：检索词包含召回词时不受相似度阈值限制，一刀切阈值会漏召回短专名。

## 4.5 前端呈现坑

- 决策树完全依赖列 show_type 标注——后端不回填则全树失效；show_type 回填是第 5 期其余改动的前置。
- 补零趋势：上游按列名 contains('date') 触发且填 0（拉低均值+假谷底）——改为 show_type==DATE 触发、缺失填 null。
- getFormattedValue 把 0 显示为 '-'（0 值被吞）、两函数 0 值口径不一致——统一口径后再移植。
- 趋势图静默丢弃第 20 名之后的分组、多候选只取前 5——阈值行为要在 UI 上明示（"仅展示 TOP20"）。
- summary 轮询无次数上限（后端不返回则前端无限轮询）——第 5 期轮询必须带上限+超时。
- ECharts 实例复用不 clear 会残留旧 series；容器宽 0 时渲染空白需 resize 兜底——BiChart 封装统一处理。

## 4.6 工程纪律（本蓝图执行约束）

- **优化必须纯增益**：谓词下推/Rubric/方言改写全程 try-catch 或 Option 回退原 SQL，任何优化路径不得因异常挂掉主链（上游 FilterToGroupScanRule 的正确示范）。
- **PG 恒走 WITH 合并**，禁用字符串替换嵌入子查询（表名作子串出现在列名/字面量会被误替换）。
- **每期只做该期清单**：meta.rs/pipeline.rs 已逼近巨型，新代码优先新文件（exact_match/time_parse/expand/cache），存量文件只做就近插入；拆分重构留到第 6 期单独小步进行。
- **评测先行**：第 1 期门禁未建立前，第 2 期及以后任何「准确性改进」都无法客观验收——这是排期不可调换的硬依赖。
- **同错三处双写风险**：时间词表、direct 模板口径与注册表口径的双写在第 3 期收敛前，任何口径改动必须三处同步检查（列入 PR checklist）。
