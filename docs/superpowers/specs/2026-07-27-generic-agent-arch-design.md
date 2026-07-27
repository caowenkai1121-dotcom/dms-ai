# dms-ai 通用 Agent 架构设计（6-crate 受控内核）

> 日期：2026-07-27 ｜ 状态：待用户审阅 ｜ 类型：架构重构 spec
> 调研依据：`docs/research/`（6 份 subagent 调研报告）+ 本仓库现状盘点 + 扩展性压测。
> 决策记录：① 目标形态 = 通用 Agent 运行时（NL2SQL 降级为第一个能力包）；② 重构手法 = 每步可编译可跑、随时可停；③ 未来模块 = ReAct 工具调用 / RAG / 定时推送 / 多数据源 四个全要；④ 依赖 = 按需放开但每个新依赖需写明不可替代理由；⑤ 架构基线 = 完整 6-crate（编译期依赖方向 + agent 多入口复用，两者都要）。

---

## 0. 背景与裁决

现系统单 crate、16 文件、~8347 行（含 157 单测）。调研裁决：**现架构不合理，必须一次调到位**。最硬三条证据：

| 病灶 | 证据 | 后果 |
|---|---|---|
| 「回答=一张表+一条SQL」被当成不变量 | `AskResult{sql,columns,rows,view}`（pipeline.rs:16-29），前端 App.vue:20-22 固化进 TS | RAG/报表/ReAct 三者无处安放，是 A/B/C 场景共同的第一道墙 |
| LLM 客户端物理上不支持 tool calling | llm.rs:34 `chat(system,user)->String` 写死两条消息，只取 content，tool_calls 无解析 | ReAct 循环无法成立，必须重写而非增补 |
| 只读/权限靠手工按序调用维持 | execute 收任意字符串（pipeline.rs:369），缓存回放/CLI/图快路径各走各的 | 漏一步即越权 |

业务硬编码进通用路径的典型（全部带行号，详见调研报告）：
- 「有效订单」状态码 `'0','108','199'` 在 6 处各写一份，绕过自建的 `meta.table_scope` 注册表。
- 34 省码表在 meta.rs / viewspec.rs / 前端 format.ts 三份副本。
- 组合器时间桥写死 `t_sales_order/order_time`，而 `meta.metric.time_col` 早已存在却无人读（设计意图与实现自相矛盾）。
- meta.rs(1679)/direct.rs(1385)/corrector.rs(1118)/pipeline.rs(1037) 四个上帝文件；pipeline.rs 依赖全部 9 个同级模块。

四个未来模块在现架构下的代价：ReAct=地基级改造、RAG=破协议、定时推送=缺四件、多数据源=同时污染既有功能（语义缓存串库 + fail-closed 全表拒绝）。

---

## 1. crate 骨架与依赖方向（编译期强制）

```
kernel ──► connector ──► policy ──┐
   │          │                   ├──► agent ──► server
   │          └────► semantic ────┘
   └───────────────►（semantic 不依赖 policy）
```

依赖单向无环，业务永不反向依赖内核——cargo 编译错误强制，不靠自觉。

| crate | 依赖 | 职责 | 硬规则 |
|---|---|---|---|
| **kernel** | serde/sqlparser/chrono，**禁 sqlx/reqwest/axum** | 纯契约+纯算法：三段 SQL newtype、权限注入算法本体、Answer 协议、ChatModel 契约、AskRun 状态机、中文 NLP 基元、呈现决策树 | 零 IO、全部可纯同步单测、**零 DMS 字符串** |
| **connector** | kernel + sqlx/reqwest | 全部对外 IO 唯一出口：ReadOnlyMySql / PgStore / LLM(ChatModel 实现) / embed / AGE | **全仓唯一能造 MySQL 池**，不导出裸 `MySqlPool` |
| **policy** | kernel + connector | 行级权限 IO 侧：算 ScopeSets、加载 RuleSet | 语义 1:1 复刻 Java，唯一「改错=越权」模块，独立测试套件 |
| **semantic** | kernel + connector，**不依赖 policy** | 业务知识全部落点：注册表/召回/组合器/校正器/列标注 | 变更最频繁；不依赖 policy 保证改口径碰不到权限内核 |
| **agent** | kernel+connector+policy+semantic，**不配 axum** | 唯一持有循环语义与路由分诊：Answerer 有序表 + 驱动 AskRun 循环 | HTTP/CLI/定时任务三入口共用 |
| **server** | 全部 | 装配+HTTP+CLI+定时+身份，薄壳只做接线 | 目标 ≤700 行 |

**只读红线的结构性保证**（6-crate 最值钱处）：
- `connector` 不导出裸 `MySqlPool`；`ReadOnlyMySql.pool` 私有，唯一构造入口强制 `SET SESSION TRANSACTION READ ONLY`。
- 业务 SQL 只能经 `fetch(&ScopedSql)`；`ScopedSql` 字段私有、**全仓唯一产出点是 `inject()`**，`inject` 只吃 `check()` 产物。
- CLI exec-sql、语义缓存回放、模板快路径、图快路径全被编译器逼进同一 `check→inject→fetch` 管道。
- 框架自查（policy 查 `t_role_data_scope`）走 `fixed(&'static str)`——`&'static str` 即编译期字面量，LLM 拼接串在类型上进不来。

---

## 2. 核心 trait 与抽象（kernel/connector 真实签名）

> 权限类型（ScopeSets/TableRule/Binding/OwnerKind）语义 1:1 不动，不重复贴。

### 2.1 三段 SQL newtype（只读+权限不可绕过的类型闸门）
```rust
// crates/kernel/src/sql/mod.rs —— 字段全私有，构造入口唯一
pub struct RawSql(String);            // LLM 输出/模板/缓存回放 都必须经此包装
pub struct CheckedSql { text: String, tables: Vec<String> }
pub struct ScopedSql  { text: String, unrestricted: bool }

pub fn check(raw: RawSql, d: &dyn Dialect, g: &GuardConfig) -> Result<CheckedSql, GuardError>;
pub fn inject(sql: CheckedSql, sets: &ScopeSets, rules: &RuleSet) -> Result<ScopedSql, PolicyError>;
```

### 2.2 ReadOnlyMySql（红线结构性载体）
```rust
// crates/connector/src/mysql.rs
pub struct ReadOnlyMySql { pool: sqlx::MySqlPool }   // 私有，全仓唯一造池入口
impl ReadOnlyMySql {
    pub async fn connect(url: &str, max_conn: u32) -> Result<Self, ConnectorError>; // 强制 READ ONLY
    pub async fn fetch(&self, sql: &ScopedSql, max: usize, t: Duration) -> Result<RowSet, ConnectorError>;
    pub async fn explain(&self, sql: &ScopedSql, t: Duration) -> Result<(), ConnectorError>;
    pub fn fixed(&self, sql: &'static str) -> FixedStmt<'_>;   // 框架自查，字面量通道
}
```

### 2.3 ChatModel 契约（形状先摆正，v1 不实现 tool 循环）
```rust
// crates/kernel/src/llm.rs（契约） / crates/connector/src/llm.rs（OpenAI 兼容实现）
pub struct ChatRequest { pub tier: ModelTier, pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>, pub temperature: Option<f32>, pub max_tokens: Option<u32> }
pub struct ChatReply { pub content: Option<String>, pub tool_calls: Vec<ToolCall>, pub usage: Usage }
pub trait ChatModel: Send + Sync {
    fn chat<'a>(&'a self, req: ChatRequest) -> BoxFut<'a, Result<ChatReply, LlmError>>;
}
```
替换 llm.rs:34；6 个调用点一次改到位，将来 ReAct 不必再动。tools 字段 v1 留空。

### 2.4 Dialect（方言/类型/时间函数/schema 采集收敛一处，v1 只 MySQL）
```rust
// crates/kernel/src/sql/dialect.rs
pub trait Dialect: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn parser(&self) -> &'static (dyn sqlparser::dialect::Dialect + Send + Sync);
    fn classify_column(&self, db_type: &str) -> ColumnKind;
    fn time_fn(&self, k: TimeFn) -> &'static str;
    fn schema_probe(&self) -> &'static str;
}
pub struct MysqlDialect;  // v1 唯一实现；多数据源不预造，留 trait 位
```

### 2.5 Answer（统一回答协议，破「回答=一张表」不变量）
```rust
// crates/kernel/src/answer.rs
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnswerBody {
    Table { sql: String, columns: Vec<String>, column_meta: Vec<ColumnMeta>,
            rows: Vec<Vec<serde_json::Value>>, row_count: usize, truncated: bool }, // flatten 保前端字节级不破
    Text { markdown: String, citations: Vec<Citation> },      // RAG
    Steps { steps: Vec<StepRecord> },                          // ReAct 过程
    Composite { subs: Vec<Answer>, summary: Option<String> },  // 报表汇总（补今天缺失）
}
```

### 2.6 AskRun sans-IO 状态机（整条流水线可纯单测）
```rust
// crates/kernel/src/run.rs —— 不做任何 IO，只装「决策」
pub enum Step { Retrieve, Generate{messages:Vec<Message>,tier:ModelTier}, Correct{sql:String},
    Validate{sql:RawSql}, Execute{sql:ScopedSql}, Repair{sql:String,reason:String,round:u8}, Finish(Box<Answer>) }
pub struct AskRun { /* 字段全私有 */ }
impl AskRun {
    pub fn next(&mut self) -> Result<Step, AskError>;
    pub fn on_generated(&mut self, r: ChatReply) -> Result<(), AskError>;
    pub fn on_executed(&mut self, rows: RowSet) -> Result<(), AskError>;
    pub fn on_failed(&mut self, e: ExecFailure) -> Result<(), AskError>;
    // 轮次预算、校正轮、自修重试上限、终止判定全在这；驱动侧剩 ~20 行 match
}
```

**明确不预造**（避免臃肿）：Tool/Host 泛型、ToolMiddleware 洋葱、SPI 文本注册、HITL 可恢复 run（AskRun 不实现 Serialize）、SSE。等 ReAct 真做时再加。

---

## 3. semantic / policy / agent / server 组件边界

### 3.1 semantic（meta.rs 按生命周期切四类）
```
semantic/
├── migrations/0001_init.sql…        版本化 DDL（替代 meta.rs:11-161 按 ';' 朴素切分）
├── seeds/{warns,kw_force,metrics,dimensions,value_maps,terms,pitfalls,table_scopes,join_edges,scope_binding}.sql
├── registry/    六张注册表类型 + PG 读写 + 幂等播种
├── ingest/      information_schema ETL + autodiscover 拆 probe/match/register 三段
├── recall/      六种召回统一签名 + map_filter 净化 + 卡片渲染（算法不动）
├── compose/     注册表驱动组合器【修矛盾：时间桥改读 metric.time_col】+ BFS 路径 + 残留守卫
├── fastpath.rs  单号→下钻→聚合；doc_binding/agg_template 改从 Registry 读，状态码统一读 table_scope
├── correct/     五校正器各一文件 + Corrector trait 有序链
└── present.rs   列语义标注：Registry 打 ColumnMeta，替代 viewspec 渲染层猜中文词
```
这一刀同时修三个自相矛盾：时间桥读 time_col、状态码统一读 table_scope、维度/省码从 3 份副本收敛注册表。

### 3.2 policy（唯一「改错=越权」模块，独立测试套件）
```
policy/
├── principal.rs   原样搬 93 行（多角色 fail-closed）
├── scope.rs       compute_scope + 7 个 DMS 表查询（魔数 101/102/103 不动）
├── cache.rs       RwLock<HashMap> + 显式 invalidate（修现无失效接口）
├── rules.rs       RuleSet 从 OnceLock 改 RwLock<Arc<RuleSet>> 热更新
└── tests/         原 31 scope + 15 inject 单测，一个字不改地通过 = 硬验收
```

### 3.3 agent（唯一持有循环语义与路由，不配 axum）
```
agent/
├── run.rs         驱动 kernel::AskRun 的 IO 循环（全仓唯一 loop）
├── answerers/     trait Answerer + Vec<Arc<dyn Answerer>> 有序表
│                  {compose, fastpath, graph, cache, llm} 顺序与今天生产链路一字不差
├── prompt.rs      8 段上下文组装 + extract_sql
├── prompts/*.md   外置模板（替代 pipeline.rs:189 硬编码）
├── ask.rs         多轮改写 + 复合拆解 + 并行子查询 + 汇总步（补今天主体全空）
└── review.rs      失败复盘 / 候选教训 / 语料复核
```

Corrector 有序链（消灭 pipeline.rs:597-624 五段三种签名样板）：
```rust
pub trait Corrector: Send + Sync {
    fn name(&self) -> &'static str;
    fn correct<'a>(&'a self, ctx: &'a CorrectCtx<'a>, sql: &'a str)
        -> BoxFut<'a, Result<Option<String>, CorrectError>>;
}
pub async fn run_chain(chain: &[Arc<dyn Corrector>], ctx: &CorrectCtx<'_>, sql: String)
    -> (String, Vec<(&'static str, String)>);
```

Answerer 路由分诊（替代写死 if 链）：
```rust
pub trait Answerer: Send + Sync {
    fn route(&self) -> &'static str;
    fn accept(&self, ctx: &AskCtx<'_>) -> bool;                 // 便宜门禁，不做 IO
    fn answer<'a>(&'a self, ctx: &'a AskCtx<'a>)
        -> BoxFut<'a, Result<Option<Answer>, AskError>>;        // Ok(None)=交给下一个
}
pub struct Router { answerers: Vec<Arc<dyn Answerer>> }        // 顺序可 dump、可 trace
```

### 3.4 server（装配+协议，薄壳 ≤700 行）
```
server/
├── config.rs      分组配置 + env 覆盖，去掉硬编码生产地址
├── state.rs       AppState{ReadOnlyMySql, PgStore, Arc<dyn ChatModel>, Registry, RuleSet, IdentityProvider}
├── mw/auth.rs     axum middleware::from_fn 统一认证，封掉 body 带 login_name 冒充
├── mw/error.rs    AppError + IntoResponse
├── api/{ask,conv,auth,roles,health}.rs   8 handler 分文件；health 修恒真判定
├── identity.rs    trait IdentityProvider + DmsSso + Wework
├── jobs.rs        定时任务注册表（替代写死 03:00 裸 spawn）+ notify.rs（补企微 message/send）
├── chat_store.rs  会话/消息，payload 存 Answer
└── bin/cli.rs     9 子命令，exec-sql/scope 判官强制走同一 check→inject→fetch 管道
```

---

## 4. 数据流 + 错误处理模型

### 4.1 一次问答完整流转（NL2SQL 路径）
```
HTTP /api/ask → mw::auth → api::ask 薄 handler
  └─ agent::ask(question, principal)
       ├─ policy::compute_scope(mysql)            算 ScopeSets
       ├─ semantic::recall(pg)                    召回指标卡/维度/术语/码值/教训
       └─ Router.dispatch 按序试 Answerer：
            compose → fastpath → graph → cache → llm(兜底)
            llm 驱动 kernel::AskRun 循环：
              Generate → connector.llm.chat(ChatRequest)
              Correct  → semantic.run_chain(5 校正器)
              Validate → kernel::check(RawSql)→CheckedSql
                         kernel::inject(CheckedSql,ScopeSets)→ScopedSql   ← 唯一产出点
              Execute  → connector.mysql.fetch(&ScopedSql)                ← 只收 ScopedSql
              Repair×≤2 → 失败自修
              Finish   → Answer
  └─ viewspec 构建 → Answer.view → serde 序列化（Table flatten 前端字段字节级不变）
```
**关键不变量**：任何 SQL 要想到达 `mysql.fetch`，类型上必须是 `ScopedSql`，只能由 `inject()` 产出——图快路径/缓存回放/模板路径无一例外被编译器逼进这条管道。

### 4.2 错误处理模型（分层，不滥用 anyhow）
| 层 | 错误类型 | 原则 |
|---|---|---|
| kernel | `GuardError/PolicyError/AskError`（手写 enum + Display + Error，不引 thiserror） | 强类型可匹配，穷尽处理 |
| connector | `ConnectorError/LlmError` | 区分可重试抖动 vs 确定性失败 |
| semantic/policy | kernel 错误 + 各自 domain error | fail-closed：权限/注册表缺失一律拒 |
| agent | `AskError`（含 route、stage） | 错误带路由与阶段 |
| server | `AppError: IntoResponse` | 唯一收口，401/403/400/500 |

两条铁律：
1. **fail-closed 优先于可用性**：未登记权限档案表、无角色账号、权限计算失败——全部拒，绝不降级放行。
2. **抖动才重试，业务错误不重试**：transient 列表（os error 10054/10060/pool timeout/connection reset）退避重试；SQL 语义错误进 AskRun Repair 轮（≤2），超出即返回错误带 trace。

**SqlTrace 四态留痕**（抄 SuperSonic SqlInfo）：`generated / corrected[] / injected / stage_ms[]` 挂 Answer 上，任何问答可回放「LLM 生成什么→校正器改什么→注入哪表哪条件→各阶段耗时」。

---

## 5. 测试策略 + 迁移 10 步执行门禁

### 5.1 测试金字塔
| 层 | 锁什么 | 工具 | 通过标准 |
|---|---|---|---|
| 纯单测 | 权限/注入/状态机/词法 | cargo test（无需起库） | 现有 157 单测一个不改地通过；46 权限单测一个字不动=硬验收 |
| 路由对拍 | 同批问句新旧 route 逐条相同 | 临时 harness | 防 Answerer 顺序错一位改走别路 |
| serde golden | Answer 与旧 AskResult 字节兼容 | golden JSON | Table 变体顶层字段一致（新增 kind/column_meta/trace 除外） |
| 回归题集 | 结果集正确性 | regression.py(51)+evaluation.py(38 exec-only) | 比结果集不比 SQL 文本（SQL 允许因修 bug 变，结果不许变） |
| 种子对拍 | 种子外置不漏字 | 一次性脚本逐表 SELECT 全量比对 | 全等（含 NULL vs 空串） |

### 5.2 迁移 10 步（每步可编译可跑，独立提交）
| # | 动作 | 风险 | 验收门禁 |
|---|---|---|---|
| 1 | workspace 加 5 空 crate，server 加 path 依赖不 use | 零 | cargo build 不变；CI `cargo tree` 无反向边 |
| 2 | 纯算法下沉 kernel + 词表合并 | 低 | 调用点 re-export 不改；词表先并集+开关，跑回归再收 |
| 3 | **三段 newtype + 改执行器签名**（最有价值） | 中高 | 编译器报所有执行点逐个改；fail-closed 先 warn 不 bail，跑一周再收紧 |
| 4 | connector：llm 重写 ChatModel + embed 批量/双模式 | 低 | 6 调用点显式 temperature=0.1；embed 默认 query 模式 |
| 5 | policy crate：principal/scope/inject 迁入 | 低 | **46 权限单测不改通过**；RuleSet 每请求 clone Arc 快照 |
| 6 | meta 解体①：DDL 版本化 + 种子外置 | 中高 | **种子对拍全等**才可合并；先插生产库 baseline |
| 7 | meta 解体②：registry/ingest/recall 拆 + 五校正器 trait | 低中 | 33+13 单测过；CorrectCtx 保 caliber 读 question、value 读 pg |
| 8 | **direct 解体 + 口径单一事实源**（最高危） | 最高 | **拆三个独立提交**，各自跑回归+评测，结果集变化逐题人工判定 |
| 9 | pipeline 解体入 agent：AskRun + 五 Answerer + Answer 协议 | 中高 | **路由对拍** route 逐条同 + **serde golden** + prompt 外置一字不差 |
| 10 | server 瘦身：main 拆 bin、handler 分文件、认证中间件、jobs | 中 | **先把 3 个 python 判官认证改好**（否则评测门禁停摆） |

### 5.3 三条「风险的风险」（前置必做）
1. 步骤 10 认证中间件会打挂 3 个 python 判官（靠 body 带 login_name 的开发模式）→ 步骤 3 之前先发真 token 或加 dev-token。
2. 步骤 8 时间桥修复会改变线上问句数值 → 上线前备「哪些问句的数会变、变成什么、为什么现在的错」对照清单。
3. sqlx migrate 与已有生产库 baseline：16 张 meta.* 已存在 → 先手工插 baseline，否则首启动重跑迁移，部分 ALTER 旧 PG 版本报错。

### 5.4 退出/收敛条件（防过度设计回摆）
- 文件粒度甜区 150-450 行；>500 拆、<80 且无独立测试则并回。
- 全仓 .rs 文件数上限 60，超了说明拆过头。
- M+3 时若 agent 非 axum 消费者 <2 个，执行 agent/server 合并回 5 crate。

---

## 6. 明确不做（YAGNI 清单）
- 不做 Host/Tool 泛型、ToolMiddleware 洋葱（ReAct 真做时再加）。
- 不做 SPI/文本插件注册（栈在 AppState 构造函数硬编码，顺序显式可 grep）。
- 不做 HITL 可恢复 run（AskRun 不实现 Serialize）。
- 不做 SSE 流式、不做中间件洋葱（Corrector/Answerer 都是扁平有序表）。
- meta 七张表 v1 不加 datasource 列（多数据源留 trait 位，不预造）。

## 7. 依赖红线遵守
- 零新增第三方依赖。异步 trait 用 Rust 1.75+ 原生 + 手写 `Pin<Box<dyn Future+Send+'a>>`（BoxFut），不引 async-trait。
- sqlx 仅打开已有 crate 的 `migrate` feature（不是新依赖）。
- 工具 JSON Schema 手写 `serde_json::json!`，不引 schemars（工具数 <30）。
- cron 解析自写，不引 cron crate。
- RAG 文档解析（PDF/Excel）属能力包，到时单独拿方案报批再加依赖。
