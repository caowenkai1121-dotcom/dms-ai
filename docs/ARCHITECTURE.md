# dms-ai 架构（权威文件级设计）

> 版本：2026-07-27 ｜ 状态：可开工 ｜ 产出方式：7 crate 并行文件级设计 + 跨 crate 契约裁决 + 4 路对抗评审（过度设计 / 上帝文件与函数肥胖 / 功能不许丢 / 红线不破）
> 关系：本文是 v1 spec（6-crate 受控内核）与 v2 spec（多数据源 + 知识库）的**合并终稿**。三者冲突处以本文为准；两份 spec 保留为设计过程记录，`plans/_DECISIONS.md` 的裁决除本文 §8 明确推翻的以外继续有效。
> 目标形态：通用 Agent 运行时。NL2SQL 与企业知识库是两个能力包，多数据源是取数能力包的地基。
> 2026-08-12 修订：业务已明确要求模型主导的结构化意图与受控工具循环，原 §8 对 ReAct 的延期条件已经满足；迁移边界、pi 对照和验收见 [`AGENT-ARCHITECTURE.md`](AGENT-ARCHITECTURE.md)。在 typed tool 与覆盖闸完成前，不以新增平行状态机冒充完整 Agent。

---

## 0. 设计纪律（本文的验收标准）

| # | 硬线 | 判据 |
|---|---|---|
| D1 | 单函数目标 ≤40 行、上限 60 行 | 现状 8 个超线函数（`compose_sql_with` 154 / `ask_single` 189 / `autodiscover_dict_columns` 160 / `compute_scope` 97 / `viewspec::build` 92 / `time_predicate` 84 / `rewrite_agg` 78 / `sales_breakdown` 70）逐个给了拆法，见 §4 各表 |
| D2 | 单文件甜区 150-450 行，>500 必拆，<80 且无独立测试则并回 | 终态无一 `.rs` 超 450 |
| D3 | 每个文件只能有一个「变更原因」 | §4 每行的「职责」列就是它的变更原因；两个原因即拆 |
| D4 | 拆函数必须同时拆参数 | 子函数 ≤3 参。共享状态与命中结果一律命名（`Plan` / `DictHit` / `BindingRow` / `RoleIdx` / `AclEntry`），禁止把 6-8 个同型 `&str`/`Option<String>` 连排 |
| D5 | 纯算法与 IO 分离 | 能纯函数化的必须纯函数化（无库无网可单测）；kernel 零 IO、零 DMS 业务语料 |
| D6 | 零新增第三方依赖 | 只允许开已有依赖的 feature（`axum` 的 `multipart`、`sqlx` 的 `migrate`）；异步 trait 手写 `BoxFut`，不引 `async-trait` |
| D7 | 只有 ≥2 个实现或已被裁决的 trait 才存在 | 单实现 trait 一律推迟到第二个实现出现那天（§8 列了本轮因此删掉的 5 个） |
| D8 | 既有资产不许丢 | 157 单测 → **156 一字不改搬运 + 1 个随缺陷修复删除**（`civil_date_sane`，见 §7）；M0-M9i 全部功能有落点（§4 的搬运列逐条对账过） |
| D9 | 顺序即行为 | `infer_semantic` 的 Count 先于 Order、`STRIP_WORDS` 的「上月」先于「上个月」、`time_predicate` 五条规则的先后、customer 段序 base→common→102→103→下属：重排即行为变化，拆分只许提取不许重排 |

**膨胀哨兵**：现状 8347 行（含 157 测试）→ 终态约 17k-19k 行。其中知识库能力包 ≈1400、多源 ≈500、种子与迁移外置 ≈490（SQL）、新增测试 ≈1500，其余为解体后的模块声明与契约声明。**判据：任一 crate 的非测试行数超过其搬运量 20% 且说不出对应的具名功能，就是过度设计又爬回来了。**

---

## 1. crate 骨架与依赖规则

```
kernel ──► connector ──► policy ──┐
   │          ├────► semantic ────┼──► agent ──► server
   │          └────► knowledge ───┘
   └──────────────►（semantic / knowledge 都不依赖 policy）
```

| crate | 唯一职责 | 允许的依赖 | 硬规则 | 预算 |
|---|---|---|---|---|
| **kernel** | 纯契约 + 纯算法底座 | serde / serde_json / sqlparser | 零 IO、零 DMS 业务语料（表名/列名/中文业务词/口径魔数）、`cargo test -p dms-kernel` 无库无网跑绿；**不引 chrono**（时钟一律参数注入） | ≤21 `.rs` / ≈3600 行 |
| **connector** | 全部对外 IO 的唯一出口 | kernel + sqlx / reqwest | 全仓唯一能造连接池；不导出裸池；只读源只收 `ScopedSql`；`OwnedStore` 永不接受 LLM 产物 | ≤14 / ≈2250 |
| **policy** | 行级权限的 IO 侧 | kernel + connector | 语义 1:1 复刻 Java；一切失败 fail-closed；46 权限单测断言一字不改 | ≤12（8 src + 4 tests）/ ≈1560 |
| **semantic** | DMS 业务知识全部落点 | kernel + connector | 不依赖 policy（改口径永远碰不到权限内核）；`meta.*` 的唯一读写口 | ≤38 / ≈6100 |
| **knowledge** | 企业知识库能力包 | kernel + connector | 不产 SQL、不碰 `meta.*` 领域表（唯一例外：`qa_log` 落 `meta.query_log` 观测行）、不依赖 policy（用 `Viewer` 而非 `Principal`） | ≤8 / ≈1700 |
| **agent** | 一次问答的循环语义与路由分诊 | 全部业务 crate | 不配 axum；全仓唯一 loop；HTTP/CLI/定时三入口共用 `ask()` | ≤15 / ≈1900（T9 实测 17 / 3621 含测试，理由见 §4.6 头） |
| **server** | 装配 + 协议 + 认证收口 | 全部 | 零业务算法、零 SQL 拼装；`reqwest` 只许出现在身份面三文件 | ≤22 / ≈2200 |

**门禁脚本** `scripts/check-arch.ps1`（三条 grep，进 CI）：
1. `MySqlPool|PgPool|PgPoolOptions|MySqlPoolOptions|sqlx::query` 命中 server/agent/policy/semantic/knowledge 任一 → exit 1（server 保留 sqlx 仅为 `FromRow` 派生）
2. `cargo tree -p dms-agent` 出现 axum → exit 1；`cargo tree -p dms-kernel` 出现 sqlx/reqwest/axum/chrono → exit 1
3. kernel 源码出现 `t_[a-z_]+` 或「销售额|客单价|客户|门店」→ exit 1

**文件数上限的修订**：v1 §5.4 的 60 与 v2 §8 的 75 都作废 —— 它们是在没有知识库 crate、没有多源、没有 kb/ds 两组 API 时估的。改为上表的**每 crate 预算**（合计 ≈129）。超预算时的合并顺序固定：semantic 的 `fastpath/{agg,breakdown}→template.rs`、`recall/metric→cards.rs`；kernel 的 `nl/lexicon→nl/text`。**上限本身不是目标，D2/D3 才是。**

---

## 2. 五条不变量与它们的结构性保证

| # | 不变量 | 结构性保证（编译器管的） | 残缺（脚本+review 管的） |
|---|---|---|---|
| I1 | 到达生产 MySQL 的 SQL 必是 `ScopedSql` | `SqlSource::fetch(&ScopedSql)`；`ScopedSql` 字段私有，产出点只有 `inject()` 与 `ScopedSql::unrestricted(_, &UnrestrictedProof)`；`UnrestrictedProof` 字段私有、只能由 `dms_policy::proof::for_principal()` 铸造 | `ScopedSql::wire()` 必须 pub（connector 要读串）；`grep 'unrestricted('` = 全仓无权限出口清单 |
| I2 | 自有可写库与只读源类型分离 | `OwnedStore` 无任何吃 `RawSql/ScopedSql/String` SQL 的方法；写入只走 `fixed(&'static str)+bind` 与 `create_upload_table(&UploadTableSpec)` | semantic 的 60+ 处 `&PgPool` 由 `semantic/tests/drift.rs` 两条测试守（T7a 裁决 F1）：`every_meta_recall_is_ds_scoped`（每条 `FROM meta.` 必带 ds 谓词）+ `sql_interpolation_is_allowlisted`（SQL 里只许插白名单，白名单项必须写明「该值为何不可能来自外部输入」）。门禁对 semantic **只**守「不造池」—— 召回 SQL 必须运行时拼 `{ds_pred}`，进不了 `&'static str` 通道 |
| I3 | fail-closed 优先于可用性 | 受限用户遇未登记表 → `PolicyError::UnregisteredTable`；无角色 → `Err`；注入条件不可解析 → `PolicyError::ConditionParse`（本轮修的 fail-open）；`GuardConfig` 无 `Default`（漏传敏感列词表 = 编译错误） | —— |
| I4 | 缓存不跨用户/不跨源 | scope 缓存 key = `(login, role, ds, scope_ver)`；知识库检索 ACL 在 SQL 内 JOIN 而非后过滤；few-shot/教训召回带 `visibility` 谓词 | 语义缓存复用 SQL 时按当轮用户重新注入（行为不变） |
| I5 | 外部文本永不成为指令 | knowledge **源码**不 import `sqlparser`、不出现 `RawSql`/`CheckedSql`/`ScopedSql` —— 知识库路径**结构上产不出 SQL**。门禁那条 `knowledge 结构上不得产 SQL` 守它（**开枪验过会红**）。⚠️ 原措辞「依赖树里没有 sqlparser」是错的：`dms-kernel` 就依赖 sqlparser，传递必然在树里；可检查的是源码面。文档块与上传表头都经 `wrap_untrusted` | 系统提示的禁令措辞、`summarize` 不得输出 URL |

### 2.1 当前默认经营口径与查询边界（2026-08-06）

`crates/semantic/src/sales_fact.rs` 是默认线下销售口径的代码事实源，`docs/warehouse_catalog.md`
是数仓表粒度、血缘和风险的静态审计依据。当前合同固定如下：

| 对象 | 当前有效合同 |
|---|---|
| 默认销售事实 | `sales_dw.dws_off_offline_sale_dfn`，时间列只使用 `order_date` |
| 销售额 / 销量 | `SUM(amount)` / `SUM(qty)` |
| 成本 / 收入 / 毛利 | `SUM(cost_excluding_tax)` / `SUM(revenue_excluding_tax)` / `SUM(gross_profit)` |
| 毛利率 | `SUM(gross_profit) / NULLIF(SUM(revenue_excluding_tax), 0)`；禁止平均行毛利率 |
| 客户维度 | `storecode/storename` 是客户编码/名称，不是门店；真实门店必须来自已验证的 `shop_code/shop_name` |
| 退货 | 销售事实已含退货负数，不再拼旧发货/退货 `UNION`，也不再次冲减退款 |
| 订单数 | 来自 `dms_ods.t_sales_order`，按有效订单的 `sales_order_code` 去重；禁止用销售事实行数推算 |

生产 DMS MySQL 只承担身份权限和受控业务点查。业务点查必须是**单表、索引等值/小 `IN`/
前缀匹配、小 `LIMIT`、短超时**；禁止 JOIN、UNION、子查询、聚合、无界排序和大范围扫描。
统计分析统一走已验证的 Doris DWS/DWD/ADS，不得为了补维度回生产库做多表查询。

---

## 3. 本轮一并修的真缺陷（全部有 file:line 依据）

| # | 缺陷 | 现状 | 修法 | 归属 |
|---|---|---|---|---|
| F1 | **权限注入 fail-open** | `inject.rs:243` 条件字符串 parse 失败被 `if let Ok` 静默丢弃 → 条件消失查询照跑 | `Err(PolicyError::ConditionParse)`；**且 parse 成功但 `peek_token() != EOF` 同样阻断**（`x.owner manager in (1)` 会前缀解析成功只吃 `x.owner`，是更隐蔽的截断式越权） | kernel/policy |
| F2 | **`ScopeSets::default()` 可铸造无限制 ScopedSql** | `inject.rs:182-185` unrestricted 原样返回；裁决 T3-2 还把这条路径写成 autodiscover 的官方写法 | 放行改独立入口 `ScopedSql::unrestricted(CheckedSql, &UnrestrictedProof)`，proof 只能由 policy 从 `Principal.administrator_flag` / `BaseDecision::Unrestricted` 铸造 | kernel/policy |
| F3 | **自有库对只读源可见** | `PostgresSource` 与 `OwnedStore` 同库同角色 → 一条合法 SELECT 可读 `kb.chunk`（全员文档原文）、`meta.sql_exemplar`（他人问句与 SQL）、`chat.msg`（他人问答） | ①只读源用无写权限的 PG 角色 + `REVOKE USAGE ON SCHEMA meta,kb,chat`；`PostgresSource::connect()` 自检 `has_schema_privilege` 为真即拒绝启动，布尔进 `/api/health`；②`check()` 无条件拒绝非业务 schema（`meta.`/`kb.`/`chat.`/`information_schema`/`pg_catalog`/`mysql`/`sys`）—— **注意这是 deny-list 不是 whitelist**：无条件白名单会让超管在约 200 张未登记业务表上被拒，回归当场红 | connector/kernel |
| F4 | **上传表头以「权威 schema 注释」身份进 SQL prompt** | K4 把 Excel 中文表头写进 PG 列注释 → `meta.column_doc` → `render_schema` 拼进 schema 段，而系统提示第 3 条明令「表头注释里的【⚠️】必须逐条遵守」= 一条被文档背书、绕开全部 untrusted 机制的指令通道 | ①`column_doc` 加 `origin`（`information_schema` / `upload`）；②入库前过 `sanitize_comment`（剥控制字符/`【⚠️`/`##`/`<`/`>`/换行，截 120 字）；③`origin='upload'` 的表整体包 `<untrusted_schema>`；④系统提示第 3 条限定为 `origin='information_schema'` | semantic |
| F5 | **敏感列两份真相源 + `SELECT *` 全绕过** | 执行侧只有 `login_pwd`/`password` 两词（`pipeline.rs:88`），给 LLM 剔 schema 的有 9 词（`meta.rs:183`）→ `SELECT id_card FROM t_employee` 全程绿灯（该表还是 Global 免注入）；单号直查恒 `SELECT *` | 防线移到**结果列**这个唯一收口：`fetch` 组装 `RowSet` 时按连接期注入的词表逐列比对，命中即整列置 `Null` 并回 `RowSet.redacted`；9 词表提为 `kernel::nl::lexicon::SENSITIVE_COLS` 单一事实源 + 漂移单测 | connector/kernel |
| F6 | **few-shot 与教训跨用户明文泄露** | `fewshot_block`（`pipeline.rs:206-218`）无条件把最相似两条历史 `question`+`sql` 塞进他人 prompt —— 那是别人打的字与 SQL 里的客户编码/人名/金额 | `meta.sql_exemplar`/`meta.pitfall` 加 `visibility`+`owner_login`，召回统一加 `VIS_PRED`（与 `DS_PRED` 同一拼接点）；晋升 public 只在复核通道发生，复核提示加「含具体客户名/人名/金额一律判 disabled」 | semantic |
| F7 | **scope 缓存无失效 + 翻页在早八点** | 当日过期且 `epoch_day()` 用 UTC → DMS 侧收紧权限后最长 24h 仍按旧权限出数，翻页发生在北京时间 08:00（上班第一波查询） | TTL 15 分钟 + key 带 `scope_ver`（`compute_scope` 本来就要查 `t_role_data_scope`，顺手把行数与 `max(view_type)` 拼成版本号，改配置后第一次查询即自愈）+ key 加 `DsId` | policy |
| F8 | **镜像烧凭据 / 删除假成功 / 今天差一天** | `Dockerfile:16` 把含生产口令与 DeepSeek key 的 settings COPY 进镜像层（该文件从未进 git，只在镜像层，是否轮换取决于镜像是否分发过）；`main.rs:434-444` 越权删返回 `{ok:true}`；`pipeline.rs:322` 手算 UTC | 配置运行时挂载/env + CI 断言镜像里没有 `/app/settings.json`；删除改查 `rows_affected==0 → 403`；`chrono::Local` | server/agent |

**待业务裁决（不擅自改）**：受限用户查「只有 customer 段、且客户集合为空」的 4 张表（`t_customer_balance`/`t_customer_device_ledger`/`t_device_disposal_order`/`t_shop_inspection_records`）时，段全空 → 不注入 → **看到全表**。这与 Java 一致（`空集/不注入 = 放行全部`），所以不是搬运错误；但对「本人视图 + 无管辖客户的新员工」实际是越权观感。收紧成 `(1=0)` 会让我们与 `judge_scope.py` 的独立 Java 复刻分叉。**建议收紧，但需同时改判官并知会 DMS 团队。** 本轮只加两条测试把现有行为钉住（`empty_segments_allows_today`），不改行为。

---

## 4. 文件级模块树

### 4.1 kernel（≤21 `.rs` / ≈3600 行）

| 文件 | 行 | 职责（唯一变更原因） | 搬运源 |
|---|---|---|---|
| `lib.rs` | 60 | 模块树 + **路径一次性钉死的 re-export** + 「什么算 kernel」四条收纳判据写在 crate 文档 | —— |
| `errors.rs` | 130 | `GuardError`/`PolicyError`/`AskError` 与它们的 Display 文案（文案是对 LLM 与测试的外部契约，逐字等于今天的 anyhow 消息） | `pipeline.rs:64-103`、`inject.rs:191/310/319`、`scope.rs:78` |
| `ds.rs` | 20 | `DsId` newtype（`GLOBAL = "*"`）；不含 `dms()` 构造器（`'dms'` 是业务默认值） | 新增（契约缺口） |
| `sql/lex.rs` | 340 | SQL 纯文本词法：剥字面量与注释、切顶层 AND、取首标识符、解析 FROM 别名、收别名列引用、裸列限定 | `pipeline.rs:119-169`、`corrector.rs:385-429`、`direct.rs:362-424/467-523` |
| `sql/ast.rs` | 210 | sqlparser 只读遍历：别名→真表、带前缀列引用、WHERE 列名集、语句实表名（排除 CTE） | `corrector.rs:14-59/432-474` |
| `sql/guard.rs` | 240 | 只读红线判定 + LIMIT 护栏 + **非业务 schema deny-list**（F3）；敏感列词表参数化 | `pipeline.rs:61-105/171-185` |
| `sql/gate.rs` | 250 | 三段闸门：`RawSql`/`CheckedSql`/`ScopedSql` 字段私有，`check()` 唯一入口、`inject()` 与 `unrestricted(_, &Proof)` 是 `ScopedSql` 唯二产出点（F2） | 新增 + `pipeline.rs` 编排 |
| `sql/dialect.rs` | 180 | 方言层，**四方法**：`name/parser/table_probe/column_probe`；MySQL + Postgres 两实现 | `meta.rs:209-220` 两条探针 |
| `policy/scope.rs` | 250 | 权限集合纯裁决：`decide_base`（max view_type）、员工/客户合并的哨兵语义、部门树展开 | `scope.rs:61-153` |
| `policy/rules.rs` | 90 | 档案类型与查表容器 `Binding/OwnerKind/TableRule{Scoped,Global,Via}/RuleSet`（**三臂，`Cond` 推迟到 K3**） | `inject.rs:19-44` |
| `policy/inject.rs` | 350 | 注入算法本体：逐表取档案、or 段生成、CTE 豁免、明细 via EXISTS、未登记表 fail-closed、**条件解析失败阻断**（F1）；同时导出字符串级 `rewrite()`（46 断言零改的硬前提） | `inject.rs:182-369` |
| `nl/time.rs` | 360 | 中文时间与数量规则解析 → 列名占位 `{}` 的谓词模板；中文数字、近 N 单位、TopN | `direct.rs:786-990` |
| `nl/lexicon.rs` | 130 | 通用中文虚词表 + `SENSITIVE_COLS` 9 词单一事实源（F5）；业务名词一律留注册表 | `pipeline.rs:445-448/789`、`meta.rs:183-188` |
| `nl/text.rs` | 280 | 最长别名命中、MapFilter 四规则、列注释洗成维度名、全角括注剥离、剥词残留守卫 | `meta.rs:864-918/1075-1087`、`direct.rs:110-141/432-455` |
| `present.rs` | 130 | **只有类型**：`ViewSpec/ColumnSpec/Block/Kpi/Role/Semantic/Delta/Interact`（`Answer.view` 需要它们）；算法整块在 semantic | `viewspec.rs:8-124` 的类型段 |
| `llm.rs` | 230 | `ChatModel` 契约：`messages + tools` 的请求/回复形状（v1 tools 恒空，形状先摆正）；`BoxFut` | 替换 `llm.rs:34` |
| `answer.rs` | 190 | 统一回答协议 `Answer + AnswerBody{Table,Text,Composite}`（**`Steps` 推迟**）+ `Citation` + `SqlTrace` + 三个构造器 | 与 `pipeline.rs:16-53` 字节兼容 |
| `qalog.rs` | 150 | 【Y2】问答落账共享件：`meta.query_log` 的 INSERT 列清单、`route/status` 取值域、脱敏/截断、超时文案判据 —— server `query_log.rs` 与 knowledge `qa_log.rs` 两个写口吃同一份（编译方向决定只能落这里；观测表非 DMS 业务语料） | 新增（原 `query_log.rs` 私有件下沉） |
| `run.rs` | 90 | **只有纯数据**：`SqlTrace` 四态留痕 + `Budget{max_repair_rounds:2}` + `AskError`（**`AskRun` 状态机推迟**，见 §8） | `pipeline.rs:626-716` 的常量部分 |
| `sql/mod.rs` `policy/mod.rs` `nl/mod.rs` | 40 | 三个模块声明 | —— |

**函数拆法（D1）**：`is_safe_select` 45 → 28 + `forbidden_token` + `placeholder_issue`；`collect_table_conds` 58 → 38 + `via_exists_cond`；`time_predicate` 84 → 10 + 五段规则各一函数。

### 4.2 connector（≤14 / ≈2250）

| 文件 | 行 | 职责 | 搬运源 |
|---|---|---|---|
| `lib.rs` | 45 | 模块声明 + 对外 re-export 白名单 + 两条红线写进 crate 文档 | —— |
| `error.rs` | 90 | `ConnectorError`（**删掉 `is_transient`**，全仓没有退避重试的调用点） | 新增 |
| `source.rs` | 115 | `trait SqlSource` + `RowSet`（含 `redacted`）+ `SchemaSnapshot/TableInfo/ColumnInfo/SourceKind` | 新增 |
| `fixed.rs` | 215 | 字面量语句通道 `FixedStmt`/`PgStmt` + `{in}` 展开（裁决 C1）；只保留实际有调用点的方法 | `scope.rs` 的 `placeholders(n)` 拼串 |
| `mysql.rs` | 240 | `ReadOnlyMySql`：池私有 + 构造即 `SET SESSION TRANSACTION READ ONLY` + `cell_to_json` 类型映射（DECIMAL→字符串保精度）+ **敏感列脱敏**（F5） | `db.rs:50-62`、`pipeline.rs:354-436` |
| `postgres.rs` | 215 | `PostgresSource`：只读 PG 源 + **启动期自检不可见 meta/kb/chat**（F3） | 新增 |
| `registry.rs` | 160 | 多源连接管理：per-ds 池懒建 + `dsn_ref` 解析（明文只在配置）+ `probe()` 连通性测试；**删掉 cap** | 新增 |
| `owned.rs` | 155 | `OwnedStore`：自有 PG 唯一可写通道 + `pool()` 访问器 + `run_migrator` + `create_upload_table(&UploadTableSpec)`/`drop_upload_schema` | `db.rs:64-66` |
| `ddl.rs` | 200 | **全仓唯一**的上传建表安全面：`SafeIdent` 白名单 + `ColType` 推断 + DDL 渲染 + `quote_literal`（knowledge 不得再有第二份） | 新增 |
| `http.rs` | 70 | 出站 HTTP 共享底座：进程级 `reqwest::Client` + 最小熔断器（不放任何策略） | 新增 |
| `llm.rs` | 205 | OpenAI 兼容 `ChatModel` 实现；参数全部请求级（升温重试不写回共享配置） | `llm.rs` 全文 |
| `embed.rs` | 150 | 向量客户端：批量 + query/passage 双模式 + 3s 超时 + 300s 熔断；**实例而非全局单例**。它不认识任何供应商 —— 模型在 `tools/embed_service.py` 那一层（2026-08-16 起是千问 `text-embedding-v4`@512） | `embed.rs` 全文 |
| `doc.rs` | 190 | Python 文档服务客户端 `/parse`（返 `blocks` + **`sheets`**）`/chunk` `/health`；大文件 120s | 新增（K1） |
| `graph.rs` | 200 | AGE 协议面：cypher 通道 + `esc/unquote` + 三个图查询 + `rebuild(nodes,edges)`；**查询要求 `&UnrestrictedProof`**（F2 同源） | `graph.rs:1-160` |

### 4.3 policy（≤12 / ≈1560）

| 文件 | 行 | 职责 | 搬运源 |
|---|---|---|---|
| `lib.rs` | 70 | 对外单点 re-export + 两参 `inject(sql,&sets)` 兼容门面（15 个断言原样调用） | —— |
| `principal.rs` | 105 | 员工 + 激活角色加载（多角色必须显式选、无角色 fail-closed、超管可无角色） | `principal.rs` 全文 |
| `scope.rs` | 155 | `ScopeSets` 计算编排：超管短路 → 基础档裁决 → 101/102/103 → 三维度合并。`compute_scope` 97 行拆 1 编排 + 3 段函数，**段序不许动** | `scope.rs:185-281` |
| `dms_tables.rs` | 215 | 7 张权限来源 DMS 表的固定模板查询，全走 `fixed(&'static str).expand(n)` | `scope.rs:284-436` |
| `cache.rs` | 120 | scope 缓存：**TTL 15min + `scope_ver` + `DsId` 维度 + 显式 `invalidate`**（F7） | `scope.rs:155-183` |
| `rules.rs` | 190 | 档案注册表 `RwLock<Arc<RuleSet>>` 快照热更新 + `meta.scope_binding` 播种/加载；`BindingRow`（`FromRow`）取代 8 参解码函数（D4） | `inject.rs:94-178` |
| `builtin.rs` | 80 | **32 张** DMS 表内置档案（scoped 14 / via 3 / global 15）—— 表名列名在 policy，不进 kernel | `inject.rs:48-92` |
| `proof.rs` | 40 | `UnrestrictedProof` 铸造：`for_principal(&Principal,&ScopeSets) -> Option<Proof>`（F2 的唯一铸造点） | 新增 |
| `tests/scope_tests.rs` | 200 | 28 个纯裁决断言，一字不改 | `scope.rs:454-692` |
| `tests/inject_tests.rs` | 175 | 15 个注入断言，一字不改 | `inject.rs:379-546` |
| `tests/inject_e2e.rs` | 55 | 3 个跨模块语义锁，一字不改 | `scope.rs:651-692` |
| `tests/fail_closed_tests.rs` | 120 | 本轮新增回归锁（不计入 46）：条件不可解析必阻断、截断式条件必阻断、坏列档案必拒表、`empty_segments_allows_today`（钉住待裁决行为） | 新增 |

### 4.4 semantic（≤38 / ≈6100）

```
migrations/0001..0009_*.sql        190   16 张 meta.* 的版本化 DDL（含 doc_binding）；轨 A 占 0001-0019
migrations/0020_kb_init.sql         75   kb schema（owner=knowledge，单 migrator 避免 VersionMissing）
seeds/*.sql (11 个)                330   9 组 const 种子 + 32 表权限档案 + 9 行 doc_binding，幂等形态逐字保留
```

| 文件 | 行 | 职责 | 搬运源 |
|---|---|---|---|
| `lib.rs` | 65 | 模块声明 + `DirectHit` + **`DMS_GUARD` 常量**（`GuardConfig::new(200,&SENSITIVE_COLS)`，契约缺口） | —— |
| `bootstrap.rs` | 130 | 启动引导：迁移 + 11 个种子按固定顺序整文件灌入 | `meta.rs:157` `split(';')` |
| `registry/mod.rs` | 140 | `Registry`（持 `PgPool`+`DsId`+`EmbedClient`）+ `with_ds` + `DS_PRED`/`VIS_PRED` 两个谓词常量 | 新增 |
| `registry/model.rs` | 150 | 装配侧行类型与读取：`MetricDef/DimensionDef/JoinEdge/TableScope` | `direct.rs:24-51`、`meta.rs` 各 load |
| `registry/lexicon.rs` | 130 | 文本命中侧：`ValueMap/TermDef/DocBinding` | 同上 |
| `registry/exemplar.rs` | 497 | **语料表 `meta.sql_exemplar` 的唯一读写口**（few-shot trgm 召回 / 向量最近邻 / pending 复核 / 状态更新），带 `VIS_PRED`（F6）—— 消灭 agent 直写 `meta.*`。三个状态写口共用 `ledger_status_change`（读前值→落账→再改） | `pipeline.rs:206-218/653-679/815-869` |
| `registry/pitfall.rs` | 103 | **教训表 `meta.pitfall` 的唯一读写口**（候选落库 / 待复核清单 / 复核结论）。2026-08-14 从 exemplar 拆出：D2（>500 必拆）+ D3（语料与教训是两条独立学习链） | 拆自 `registry/exemplar.rs` |
| `registry/learn.rs` | 300 | **学习事件账本**：`log_event`（前值/后值/批次号）+ `recent_batches`（带 first_at/last_at/rolled_back）+ `rollback_batch`（三态 `Undone`）+ 纯函数 `undo_stmt`。形态借 prime-agent 的 refinement | 新增（2026-08-14） |
| `registry/user_pref.rs` | 170 | **用户习惯层**：从 `meta.query_log` 现算高频时间/分组说法（`MIN_SUPPORT=3`），只进 prompt 参考段、不改 SQL、不覆盖用户显式表达 | 新增（2026-08-14） |
| `registry/failure.rs` | 66 | 失败经验的**读回**半：`failure_streak`（同 kind + 错误前缀 60 字），第 2 次起才惊动模型复盘 | 新增（2026-08-14） |
| `registry/element.rs` | 120 | 四注册表 → `meta.element` 幂等派生 | `meta.rs:414-494` |
| `registry/datasource.rs` | 150 | 【K3】`meta.datasource` CRUD + `visible(&Viewer)`（ACL 内联 SQL）+ `authorize→DsGrant` + `nearest(q,k)` 向量选源候选 + `register_datasource(&TabularSource)` | 新增 |
| `ingest/mod.rs` | 110 | 准入规则：备份表识别、敏感列黑名单、按名前缀分域、**`sanitize_comment` + `origin`**（F4） | `meta.rs:164-205` |
| `ingest/schema_sync.rs` | 165 | information_schema ETL（吃 `SchemaSnapshot`）+ 陈旧行清理 | `meta.rs:208-282` |
| `ingest/autodiscover/{mod,probe,match_dict,register}.rs` | 560 | A1 三段：编排 / 探测（10s 单探针超时）/ 三闸防误配 / 注册。`DictHit` 命名命中结果，`register_match` 9 参 → 3 参（D4） | `meta.rs:1226-1457` |
| `recall/mod.rs` | 60 | 六种召回的统一入参 `RecallCtx`（问句 + 已召回表 + 上限 + `ds` + `embed`） | 新增 |
| `recall/metric.rs` | 170 | 指标命中的结构化召回 + 口径卡（口径/时间列/去重键/说明四段）+ MapFilter 净化的 7 个搬运断言 | `meta.rs:920-976` |
| `recall/cards.rs` | 200 | 维度卡 / 术语 / 取值编码提示 / 元素向量近邻卡 | `meta.rs:738-1119` |
| `recall/pitfall.rs` | 150 | 教训召回（`表名.列名` 触发形态）+ 候选沉淀 + 抽表名 + 失败日志 | `meta.rs:495-595/1194-1220` |
| `recall/schema.rs` | 210 | 三路表召回（kw_force 强制 → 向量 → trgm）+ schema 渲染（⚠️ 进表头、敏感列剔除、**`origin='upload'` 包 `<untrusted_schema>`**） | `meta.rs:1128-1190/1458-1484` |
| `compose/mod.rs` | 90 | 组合器入口：加载注册表 → 命中一对即委托装配 → 装不出回落 | `direct.rs:57-107` |
| `compose/assemble.rs` | 300 | 装配规则。**`compose_sql_with` 154 行 → `Plan` 结构 + 6 个方法各 ≤35 行**（`new/from_clause/time_and/push_dedup/where_sql/render`），子函数 ≤2 参（D4）；含去括注与裸列限定的薄包装 | `direct.rs:206-358` |
| `compose/timebridge.rs` | 120 | 时间窗宿主解析 —— 修「写死 `t_sales_order/order_time` 而 `metric.time_col` 无人读」的自相矛盾 | `direct.rs:274-291` |
| `compose/path.rs` | 130 | `join_edge` 图算法：BFS ≤3 跳（带扇出方向）、直接边查找 | `direct.rs:144-197` |
| `compose/tests.rs` | 220 | 装配端到端断言与四组 fixtures | `direct.rs` tests |
| `fastpath/{doc,breakdown,agg,relation}.rs` | 520 | 单号直查（读 `doc_binding`）/ 6 维度手工模板 / 高频聚合含上期环比 / 图关系识别。**有效订单状态码 6 处内联全改读 `table_scope`** | `direct.rs:526-784` |
| `correct/mod.rs` | 150 | `Corrector` trait + `CorrectCtx` + `run_chain` + `default_chain()` 四件（裁决 T7-5） | `pipeline.rs:604-624` |
| `correct/groupby.rs` | 120 | 漏 GROUP BY 补全 | `corrector.rs:498-560` |
| `correct/agg.rs` | 165 | 聚合命中与 `AggRule` 解析（变更原因＝口径与命中） | `corrector.rs:276-317` |
| `correct/agg_rewrite.rs` | 200 | AST 改写（变更原因＝sqlparser 与 SELECT 形态）：`rewrite_agg` 78 行拆 3 段 | `corrector.rs:688-765` |
| `correct/caliber.rs` | 215 | 口径过滤补全（漏则数值虚高 17%）；`add_scope_filter` 64 行拆 2 段 | `corrector.rs:319-493` |
| `correct/value.rs` | 270 | 值链接换码；`Linker::post_visit_expr` 70 行拆 `eq_case`/`in_case` | `corrector.rs:118-273` |
| `correct/schema.rs` | 145 | 字段白名单校验 → hint（独立 validator，不进链） | `corrector.rs:62-116` |
| `present.rs` | 330 | **呈现算法整块**：`build` 两参门面 + `infer_role/infer_semantic/infer_drill/compute_insight/compress/patch_kpi_delta` + 全部中文词表 + `PROVINCE_LABELS` 34 行 + **`viewspec.rs` 10 个断言原样落位**（`build` 92 行拆 `index_roles`+`blocks_of`+6 个分支构造器，`RoleIdx` 命名索引） | `viewspec.rs` 全文 |
| `graph.rs` | 90 | 图 ETL 业务面：`sync(mysql, store, registry)`，有效订单口径读 `table_scope`（消灭第 8 处内联） | `graph.rs:161-196` |
| `tests/drift.rs` | 160 | 把「单一事实源」变成会红的测试：每条召回 SQL 含 `DS_PRED` 与 `VIS_PRED` / 种子条数（WARNS 23 / KW_FORCE 36 / METRICS 12 / DIMENSIONS 9 / scope_binding 32）/ 省码表 ↔ `seeds/value_maps.sql` ↔ `web/src/format.ts`（`include_str!` 断言 34 码全在）/ 校正链顺序 / `correction_log` 九个 kind | 新增 |

### 4.5 knowledge（≤8 / ≈1700）

| 文件 | 行 | 职责 | 备注 |
|---|---|---|---|
| `lib.rs` | 95 | `Viewer/KbCtx/KbError` + `From<sqlx::Error>/<ConnectorError>/<DocError>` | 用 `Viewer{login,roles}` 而非 `Principal` —— 这就是「不依赖 policy」的载体 |
| `store.rs` | 200 | kb 表结构与状态机：`DocStatus`（`as_str/parse`）+ doc/space/chunk 的读写 | 变更原因＝表结构 |
| `acl.rs` | 170 | **唯一越权面独立成文件**：`visible_docs` 片段、`doc_for_viewer`、`chunk_window`、`delete_doc` 归属判定、`grant/revoke`。`AclEntry{scope,target_id,grantee}` + `AclScope`/`Grantee` 枚举取代 5 个 `&str` 连排（D4）；`kb.acl` 加 `perm`（read/write），**上传只接受 `space_id == viewer.login` 或 `perm='write'`**（防对他人知识库投毒写） | 变更原因＝谁能看/写哪篇 |
| `ingest.rs` | 300 | 上传入库编排 + **唯一**的类型白名单与大小上限（server 侧不得有第二份）+ sha256 去重（PG `encode(sha256(...))`，零新依赖）+ uuid 落盘 + parse → chunk（**400 token / 重叠 60**，贴 bge 512 窗口）→ embed，每步落库 | 状态机失败可查 |
| `retrieve.rs` | 280 | ACL 先行（SQL 内 JOIN，不做后过滤）+ 三路混合（向量 HNSW 20 / tsvector 20 / trgm 10）+ RRF `1/(60+rank)` + 同文档相邻块合并；参数为 `const` 不做配置 | 可见 doc 数 <50 走精确扫描（HNSW+ACL 的召回坑） |
| `answer.rs` | 250 | 引用式回答：`wrap_untrusted` → LLM → `keep_cited_only` → `Answer::Text{markdown,citations}`。三条纪律全在这一个文件 | 无命中必答「知识库里没有相关内容」，禁止用模型自身知识补 |
| `tabular.rs` | 180 | 表格双通道：`sheet_blocks`（→markdown 进 chunk）+ 组装 `UploadTableSpec`（DDL 与 `SafeIdent` 用 connector 的，不重复实现）+ `materialize/drop_source` 编排 + `TabularSource` 描述符 | 单 sheet 上限 20 万行 / 200 列，超出 `BadInput` |
| `qa_log.rs` | 170 | 【Y2】KB 问答落账：一次知识问答一行 `meta.query_log`（`route='knowledge'`，答案落定后写，成功与失败同写，spawn fire-and-forget）；INSERT/脱敏/状态常量与问数侧共用 `kernel::qalog`，本文件不复述 | 「不碰 `meta.*`」的唯一例外（观测表，非注册表域） |

### 4.6 agent（预算 ≤15 `.rs` + 2 `.md` / ≈1900；**T9 实测 17 `.rs` / 3621 行含测试**）

> **落地后的口径修订**（T9 完工实测，超预算的两项都说明理由）：
> - **17 个 `.rs`，比预算多 2**：多出的是 `gate.rs`（三段闸门从 `pipeline.rs` 整块搬来，
>   原表没给它单独一行）与 `answerers/mod.rs`（`Answerer` trait + 路由契约常量，
>   原表把它算进「answerers 一族」）。两者都是 D3 意义上的独立变更原因，不并回。
> - **`ask.rs` 行预算 130 → 实测 234 非测试行**：多出的是 `AskDeps`（9 个形参收成 struct，D4）、
>   `open_source`（多源懒建池）与 `rewrite_followup`。原表估的是「只有分派骨架」那一版。
> - **Router 五位齐全**（`ask::router()`）：`graph → direct-agg → direct-doc → semantic-cache → llm`。
>   第五位一度在表外由 `ask_single` 直调，因为 `LlmAnswerer` 拿不到 token 用量回调与单问起点；
>   两样收进 `AskCtx`（`t0` / `on_usage`）后它成为普通成员 ——
>   「**加一种能力＝加一个 Answerer**」由此 5/5 成立。契约由 `router_is_the_contract_in_full` 逐字守。
> - 原表里的 `default_router()` **已删**：五个成员落地后没有消费者，而 `route_label_map` 拿它取标签
>   让那条子序列断言恒真（守着空气比没有守卫更坏）。

| 文件 | 行 | 职责 | 搬运源 |
|---|---|---|---|
| `lib.rs` | 45 | 模块声明 + 问答参数常量 | —— |
| `ctx.rs` | 120 | `AskCtx`（**`source: &dyn SqlSource`** 而非具名 MySQL —— ds_id 断链的头号修法；`llm: &Arc<dyn ChatModel>` 供 spawn）+ `table_answer`（`view` 恒 Some） | 新增 |
| `answerers/mod.rs` | 110 | `Answerer` trait + `Router` 有序表 `[graph, compose, fastpath, cache, llm]`（逐条转写 `pipeline.rs:537/546/586/593`）+ `route_label_map` 断言 | `pipeline.rs:527-624` |
| `answerers/hits.rs` | 160 | 组合器/模板两成员的共同落地：check→inject→fetch→view→KPI 环比 | `pipeline.rs:551-584` |
| `answerers/graph.rs` | 80 | 图成员：`Relation` → 三列表格。准入判据 `skip_reason()` 六项**只看问句、不读合同**（读 fast 产物会让同题两次进程两条路由，2026-08-14 实测）；六个不接理由各有名字并进 `info!` | `pipeline.rs:878-920` |
| `answerers/cache.rs` | 100 | 语义缓存：调 `registry::exemplar::nearest` + 时间/数字词护栏 + 回放三关（注入失败仍回落但必须 warn，且不吞 `ConditionParse`/`UnregisteredTable`） | `pipeline.rs:812-860` |
| `answerers/knowledge.rs` | 65 | 【K5】知识库适配器（`Answerer` 在 agent，故适配器只能在 agent）；**不进 `default_router()`**，由 triage 直接分派（进链会让文档问句回落到 SQL 生成，破 I5） | 新增 |
| `run.rs` | 200 | `route="llm"` 的 IO 落地：**显式 `for round in 0..=budget.max_repair_rounds` 循环**（不是状态机回调）+ 五个 async 步骤 + schema-fix 在循环外（不占预算）+ **`correction_log` 九个 kind 一个不少**（含 `schema-fix`/`explain-fail` 与 guard 的三个 caliber-*） | `pipeline.rs:593-716` |
| `prompt.rs` | 210 | 全部 prompt 纯渲染。`user()` 63 行 → `section(out,title,items)` 4 行 helper + 8 个标题常量 → 22 行（D1/D4）；`today_cn()` 用 `chrono::Local`（F8） | `pipeline.rs:187-320` |
| `gather.rs` | 115 | prompt 素材的 IO 装配：六路召回 + 语料 + 规则时间段 → `PromptCtx` | `pipeline.rs:233-251` |
| `ask.rs` | 130 | 顶层编排：多轮改写 → 分诊 → 单问/复合/hybrid 分派 | `pipeline.rs:495-525` |
| `compound.rs` | 150 | `Composite` 生产：并行拆解 + 汇总步（fast LLM，失败降级 `None`）+ hybrid 合并。**`SubBrief` 的文本段必须过 `wrap_untrusted`，summary 不许含 URL** | `pipeline.rs:507-521` + 补今天空的汇总 |
| `review.rs` | 95 | 自评闭环：三类复核的 prompt + 三个 parse 纯函数 + 四个 10 行编排（SQL 全走 `registry::exemplar`） | `pipeline.rs:718-875` |
| `triage.rs` | 135 | 【K5】意图分诊 data/knowledge/hybrid：规则优先 0-LLM，fast 兜底，失败默认 data | 新增 |
| `hybrid.rs` | 180 | 【2026-08-14】混合问句的**唯一编排点**（原在 `server/main.rs`，两条链路对同一合同行为相反）：`split()` = N 条问数 + 恰好 1 条资料、两路并行、一路挂了不拖死另一路；`knowledge_only()` 供纯资料问句 | 收自 `server/src/main.rs::hybrid_payload` |
| `source.rs` | 110 | 【K3】向量选源：显式选源优先 → `registry::datasource::visible` 候选 → 距离差 >0.08 直接用 → 否则 fast LLM 选一次 | 新增 |
| `prompts/system.md` `prompts/repair.md` | 32 | **只外置这两个**（需要 golden 逐字守）；其余 2-8 行 fast 提示保持 `format!` 字面量；方言段用 `Dialect::name()` 插值 | `pipeline.rs:189-201` |

### 4.7 server（≤22 / ≈2200）

| 文件 | 行 | 职责 | 搬运源 |
|---|---|---|---|
| `lib.rs` | 70 | 启动装配顺序的唯一事实源：日志(stderr) → 配置 → 建库句柄 → `bootstrap_meta`（三条路径共用，别退化） | `main.rs:42-49/56-66` |
| `config.rs` | 130 | 分组配置 + `DMSAI_` env 覆盖 + `dsn_ref` → 明文 DSN；**删掉硬编码生产地址**；单一 `service_url`（embed 与 doc 同端口） | `db.rs:1-46` |
| `state.rs` | 115 | `AppState`；`rules()` 是访问器（委托 `policy::rules::snapshot()`，避免双真相源）；`source(&DsId, &DsGrant)` —— 拿不到 grant 编译不过 | `main.rs:29-37` |
| `session.rs` | 105 | 会话 token 颁发/校验（进程内）+ dev 放行判定 | `auth.rs:1-57` |
| `identity.rs` | 45 | 两个 free fn（`verify_dms_token` / `login_by_code`）—— **不做 trait**（两个具体实现零 dyn 调用点） | `auth.rs:59-79` |
| `wework.rs` | 115 | 企微客户端：token 2h 缓存 + OAuth 三段 + 手机号→员工 | `wework.rs` 全文 |
| `jobs.rs` | 40 | `spawn_graph_sync` + `secs_until_next_3am` —— **不做注册表**（只有一个任务） | `main.rs:230-257/446-457` |
| `chat_store.rs` | 145 | 会话/消息持久化（payload 存 `Answer`）+ 归属校验；**删除按 `rows_affected` 判 403**（F8） | `chat.rs` 全文 |
| `mw/error.rs` | 80 | `AppError` 唯一收口；`PolicyError` → **403**，其余维持 422 | `main.rs` 各 handler 的 err 闭包 |
| `mw/auth.rs` | 110 | 统一认证中间件，**封死 body/query 带 `login_name` 冒充**；`dev_token` 逃生门（生产留空 + warn 留痕） | `main.rs:332-337` |
| `api/mod.rs` | 50 | 路由表（公开组/受保护组）+ `/api/kb/upload` 单挂 `DefaultBodyLimit` + 上传 `Semaphore(4)` | `main.rs:258-267` |
| `api/{ask,conv,auth,health}.rs` | 400 | 8 个现存 handler 分文件；**health 修恒真判定** + 加 `ro_source_isolated` 布尔（F3） | `main.rs:275-488` |
| `api/kb.rs` | 165 | 【K1/K2】上传 multipart / 文档列表与状态 / 删除 / 引用原文回查（白名单校验不在这，只透传给 `knowledge::ingest`） | 新增 |
| `api/ds.rs` | 115 | 【K3】数据源 CRUD + 连通性测试 + 采集触发（只收 `dsn_ref`；写操作 `administrator_flag`） | 新增 |
| `cli/{mod,judge,admin}.rs` | 280 | **10 个**子命令分派（名字与参数位是判官门禁）；`exec-sql`/`scope` 与服务同一条 check→inject→fetch 管道；新增 `scope invalidate <login> [role]` | `main.rs:71-209` |
| `bin/main.rs` | 45 | **单一 bin，名字不变 `dms-ai-server`**：`if args.len()>1 { cli } else { serve }` —— 不拆 bin（见 §8） | `main.rs:68-273` |

前端（K1/K2 同批，否则知识库回答会白屏）：`App.vue` 加 `v-else-if="t.result.kind === 'text'"` → 新组件 `KbAnswer.vue`（markdown + 角标）；`ResultPanel.vue` 的 `view` 改可选并加早退；新增 `KbPanel.vue`（上传/状态/删除/授权）；`format.ts` 的省码表保留并加同源注释（drift 测已覆盖）。

---

## 5. 跨 crate 权威契约（签名漂移的唯一裁决处）

| 类型 | 落点 | 关键签名 / 裁决 |
|---|---|---|
| 三段闸门 | kernel `sql/gate.rs` | `check(RawSql, &'static dyn Dialect, &GuardConfig) -> Result<CheckedSql, GuardError>`；`inject(CheckedSql, &ScopeSets, &RuleSet) -> Result<ScopedSql, PolicyError>`（三参，裁决 C2）；`ScopedSql::unrestricted(CheckedSql, &UnrestrictedProof)` |
| 字符串级注入 | kernel `policy/inject.rs` | `rewrite(&str,&ScopeSets,&RuleSet,&dyn Dialect) -> Result<String,PolicyError>` 必须 pub —— 46 断言零改的硬前提（`check()` 会补 `LIMIT 200`，走 newtype 会让 `assert_eq!(out==in)` 假红） |
| `GuardConfig` | kernel 定义 / semantic 提供实例 | 故意无 `Default`；`dms_semantic::DMS_GUARD = GuardConfig::new(200, SENSITIVE_COLS)` |
| `DsId` | kernel `ds.rs` | `GLOBAL="*"`；无 `dms()` 构造器 |
| 权限纯算法 | 算法 kernel / 断言 policy/tests | 「数值 vs 字符串」切法：kernel 只有集合运算与 `SENTINEL=-1`（101/102/103 是数值不是字符串，`kernel 零 DMS 字符串` 仍可 grep 校验）；28+3 个含 DMS 列名的断言必须与 `builtin_rules` 同 crate |
| `builtin_rules` | policy `builtin.rs` | **32 张**（scoped 14 / via 3 / global 15）；漂移测在 `policy/tests/seed_drift.rs` 用 `include_str!` 读 semantic 种子（不建依赖边） |
| `Dialect` | kernel | 四方法 `name/parser/table_probe/column_probe`；`time_fn` 等到 K3 做 PG 规则时间解析时再加 |
| `RowSet` | connector `source.rs` | 含 `redacted: Vec<String>`（F5） |
| `Answer` | kernel `answer.rs` | `#[serde(tag="kind", rename_all="snake_case")]` **必须写死**（默认 externally tagged + `flatten` 运行时报错 = `/api/ask` 500 + 三个判官 JSONDecodeError）；Table 路径 `view` 恒 Some；`subs` 只在顶层且元素恒 `{question,result}`；Composite 继续输出 `sql/row_count/truncated` 占位键 |
| `Citation` | kernel | `{doc_id,doc_name,chunk_id,page,heading_path,score}`（角标 = 数组下标+1，不存字段） |
| `SqlSource::explain` | connector | `Result<Option<String>>` —— `Some`=DB 明确判定 SQL 有问题；`None`=超时/抖动**不触发**改写（现状语义，丢 Option 会把抖动当 SQL 错误进 repair 轮） |
| schema 采集 | connector `probe_schema()` 唯一入口 | 删掉 semantic 想要的三个专用方法；字典查询走 `mysql.fixed(DICT_SQL)`（DMS 表名留 semantic）；autodiscover 动态探针走 `RawSql→check→inject(unrestricted proof)→fetch` 全管道，不开后门 |
| `EmbedClient` | connector | 实例 + Clone 共享熔断；**无全局单例**。传递链 `AppState → Registry::new → RecallCtx/KbCtx/AskCtx` |
| `Registry` | semantic | `new(pg, ds, embed)` + `with_ds`；召回统一入参 `RecallCtx`（含 `ds`），`retrieve` 改 `(pg,&RecallCtx,k)` —— 不给召回族开例外 |
| `Corrector` | semantic `correct/mod.rs` | trait 在 semantic（四个实现都在这，放 agent 会让 semantic 反向依赖 agent）；链＝GroupBy→Agg→Caliber→Value 四件，SchemaCorrector 是独立 validator |
| `Answerer`/`AskCtx` | agent | `AskCtx.source: &dyn SqlSource`；`llm: &Arc<dyn ChatModel>`；`Answerer::route()` 是表标签，**`Answer.route` 取 `hit.route`**（混用即 26 题 direct-agg + 3 题 graph 全红） |
| knowledge 入口 | knowledge | `answer::answer(&KbCtx, question) -> Result<Answer>`；`Answerer` 适配器在 agent（反向边） |
| `OwnedStore::pool()` | connector | 开放访问器，semantic 的 30+ 个 `&PgPool` 签名不动（`ponytail:` 标注这条靠 grep 而非类型守） |
| 迁移 | 单 migrator | `0020_kb_init.sql` 放 `crates/semantic/migrations/`（两个 migrator 同表会 `VersionMissing` 启动失败；靠 `set_ignore_missing(true)` 绕开等于关掉「迁移被删」检测） |
| chunk 尺寸 | knowledge | **400 token / 重叠 60**（原因曾是 bge 的 512 窗口；2026-08-16 换千问后窗口远大于此，**这个数不跟着放大** —— 块大小是检索粒度，放大块会让引用定位变粗，要改另开一票并重算全部向量） |

---

## 6. 一次问答的数据流

```
POST /api/ask ─ mw::auth（只认会话 token / dev_token）─ api::ask（会话归属校验）
 └─ agent::ask(question, principal, forced_intent?)
     ├─ rewrite_followup（短追问 + 上一轮 → 完整问题，fast）
     ├─ triage → Data | Knowledge | Hybrid（规则优先 0-LLM，fast 兜底，失败默认 Data）
     │
     ├── Data ─ source::select（显式选源 > 向量最近邻 > fast 选一次）→ DsId
     │    ├ policy::compute_scope_cached（TTL15m + scope_ver + ds）→ ScopeSets
     │    ├ policy::proof::for_principal → Option<UnrestrictedProof>
     │    ├ registry.with_ds(ds) → 召回全部带 ds_id IN ($ds,'*') 与 visibility 谓词
     │    └ Router 按序：graph → compose → fastpath → cache → llm
     │        llm 路径（agent/run.rs 的显式循环，≤2 轮 repair）：
     │          gather → prompt → ChatModel
     │          → schema_check（有 hint 则 repair 一次，不占预算）
     │          → run_chain（GroupBy→Agg→Caliber→Value，全确定性 0-LLM）
     │          → check(RawSql)          ← 只读红线 + 非业务 schema deny-list + LIMIT 护栏
     │          → inject(CheckedSql, sets, rules) | unrestricted(proof)   ← ScopedSql 唯二产出点
     │          → explain 预检（Some 才改写；超时/抖动不改写）
     │          → source.fetch(&ScopedSql)  ← 敏感列在这里整列置 Null 并回 redacted
     │          → present::build → Answer{kind:"table", view:Some, trace}
     │          → 语料沉淀 + 异步复核（visibility 默认 private）
     │
     ├── Knowledge ─ knowledge::retrieve（ACL 内联 SQL + 三路 + RRF + 相邻合并）
     │                → answer（wrap_untrusted → LLM → 校角标）→ Answer{kind:"text", citations}
     │
     └── Hybrid ─ 两路并行 → Answer{kind:"composite", subs, summary}
                   （文档只影响措辞：文本段过 wrap_untrusted，绝不进 SQL 生成 prompt）
```

上传链路（K1/K4）：`POST /api/kb/upload` → 白名单/大小/sha256（`knowledge::ingest` 唯一实现）→ uuid 落盘 → `DocService.parse` → 若 `sheets` 非空则双通道：①`sheet_blocks` → chunk → embed；②`UploadTableSpec`（`SafeIdent` 清洗、代码生成 DDL）→ `OwnedStore::create_upload_table` → `registry::register_datasource` + **同一函数里写 `kb.acl(scope='ds', grantee=上传者)`** → schema ingest（`origin='upload'`）。

---

## 7. 测试基线与验收门禁

| 层 | 锁什么 | 通过标准 |
|---|---|---|
| 纯单测 | 权限裁决 / 注入 / 词法 / 时间解析 / 呈现决策 / 校正器 | **156 个一字不改搬运 + 1 个随缺陷修复删除**（`civil_date_sane` 服务的 `civil_from_days` 只为手算 UTC 存在，F8 修掉后是死代码）+ 新增约 40（fail-closed 四条、DDL 清洗、RRF、wrap_untrusted、drift 五条…）。**验收口径写「156+1删+40新」，否则「157 全绿」会被判假红** |
| 权限对拍 | Java 语义 1:1 | `judge_scope.py` 6/6；`compute_scope` 拆分前后 `scope <login> [role]` 的 stdout JSON **字节级 diff 全等** |
| 路由对拍 | 同批问句 route 逐条同 | `regression.py` 55 题（其中 28 题断言 `direct-agg`、3 题 `graph`、11 题断言 `view0/chart_kind`）。⚠️ **穷举式 `sql_contains_any` 会随路由变化假红**：E13 枚举了 `after_sales_code` 的三种别名前缀，该题从 `llm` 转 `direct-agg` 后装配器用的是 `b0.`，断言当场红 —— 断言的**意图**是「必须 DISTINCT、不许 `COUNT(*)`」，补一支即可，不是回退功能 |
| 值过滤装配 | 声明能解释的值真的进 WHERE，解释不了一律回落 | 四道门各有断言且**全部枪测过**（拆掉即红）：歧义（936 行里 109 个同名跨列）／消化了必须装上（G1）／口径已钉住该列就拒（G2）／只认 `match_kind='eq'`（5 行 `like` 在多值列 `paid_way`，写 `=` 是确定性取错集合）。外加一条**子串门**：值名被指标/维度词包含（含相等）就不认 —— 拿全部 92 道题面对 936 行全量对撞得到的两个危险命中都是**无歧义**命中，歧义门救不了。验收看 **route + 值双验**：SALE17 的值须与**同一时刻现跑的 gold** 逐字节相同（gold 备注里的数是快照，「本月累计」每天在长，拿备注当基准会得出假差异） |
| serde golden | 前端字节兼容 | 单结果响应必含 `kind/sql/columns/rows/row_count/truncated/route/elapsed_ms` 八个顶层键；复合必含 `kind/sql/row_count/truncated/route/elapsed_ms/subs` |
| 结果集 | 数字对不对 | `evaluation.py` 38 题 exec-only。长跑加 `--progress <文件>`（逐题立刻落盘）——逐题结果本来只在全部跑完后打印，而全量一趟 40 分钟起，中途「在跑」与「卡死」长得一样（实测误判两次、还杀掉过一趟快跑完的）。**别在外面用管道解决**：`Tee-Object` 与 `>` 都到结束才落盘。**`ds_id` 化那一步的硬门禁是「逐题结果集不变」** |
| 整合溯源 | 声明的整合有没有代码支撑 | `tools/audit_trace.py` exit 0：把 `INTEGRATION-TRACE.md` 里**每一处符号/路径引用**回查代码（实测 53 行表格 / 40 处引用 / 0 失效）。这份矩阵是回答「SuperSonic / deepagents / SQLBot 哪些机制进来了、落在哪个符号、由哪条判据证明」的唯一凭据，而它最容易**静默腐烂**——声明还在、引用的函数早改名了，读者无从分辨「已落地」是真的还是过期的。与本仓反复抓到的那类缺陷同形（判据的输入没了、断言恒真），故用同一条纪律核它。枪测过（改坏一处引用即 exit 1）|
| 文档解析器 | 解析出来的东西对不对 | `tools/parse_probe.py` exit 0：①pdf 出正文（依赖三级：`pymupdf4llm`/`fitz` AGPL-3.0 → `pypdf` BSD-3，判据不要求装到哪一级）②**xlsx 数据在 `sheets` 不在 `blocks`**（拿 blocks 判 xlsx 永远红，那是判据写错）③**空 sheet 必须出现在 `sheets` 里** —— 它曾在 Python 侧被 `return None` 丢掉，于是 Rust 的 `TabularSource.skipped` 永远看不到它，而那条契约写的是「不建零列表但**不能静默**」。`parse_ok` 只说依赖能不能 import，说不了解析对不对（而且它本来用 `find_spec`，装了 python-docx 但 lxml 的 DLL 被 SAC 拦时会假报 true）|
| 上传即可问数 | 双通道端到端 | `tools/up_probe.py` exit 0：①响应带 `datasource` ②SQL 用清洗后列名 `c0/c2` 且不带反引号（＝列注释进了 prompt、方言跟着源走）③销售部合计 600。**这条通道曾长期只标「建表已落地」，底下压着三个都不报错的缺陷**（方言硬写 MySQL / 缺 `search_path` 致 schema 采集为空 / 备份表启发式误伤 ~1/6 的 uuid 表名），2026-07-28 一次实测同时暴露 |
| 知识库 | 检索与注入 | `tools/kb_eval.py`（≈180 行）：recall@6 / 引用正确性 / ACL 越权必拒 / **注入必拒**（文档与 Excel 表头两种载体）/ 无命中必说「没有」。注入题 0 通过 = 合并门禁 |
| 种子 | 外置不漏字 | 逐表全量 SELECT 对拍全等（含 NULL vs 空串）；`status='opt-out'` 行豁免 |

**基线迁移注意**：`exec-sql` 的 gold SQL 现在不带 LIMIT（`main.rs:159-181`），新架构走 `check()` 会补 `LIMIT 200` → 凡 gold 返回 >200 行的题对拍结果都会变。切换当天先用旧 exe 跑一遍存 `eval_baseline.pre.csv`，切换后逐题 diff，把改判题号写进提交信息；确需全量 gold 的题在 `eval_cases.json` 里显式写 `LIMIT 100000`。

联调账号从本机运行时配置或受控密码库获取；仓库不记录账号口令。

---

## 8. 明确删掉的（对 v1/v2 的减法）

砍掉的都是「为分层纪律造、不为需求造」的抽象。合计约 1800 行、13 个文件。

| 删掉 | 为什么 | 什么时候加回来 |
|---|---|---|
| `AskRun` 状态机（`Step/Stage/ExecFailure` + 8 个回调，575 行） | 被替换的原物是 `pipeline.rs:626-716` 的 `for attempt in 0..2`，全部决策只有三件（最多 2 轮 / 首轮才 EXPLAIN / route 标签）。用两个平行枚举 + 8 个回调表达它，顺序全靠驱动侧自觉，编译器一条也保证不了 —— 出错面反而变大 | 真做 ReAct 工具循环时（那时轮次里有工具调用与并发） |
| `RowPolicy` / `RuleTablePolicy` / `TableRule::Cond` / `meta.row_rule` / `meta.col_mask` | 7 个 crate 里零调用点：agent 持 `&RuleSet`、server 直接 `rules::snapshot()`。v1 只有 dms 与上传两类源，而 v2 §4.6 自己写了上传源走 ds 级 ACL 不用 row_rule | 第一个真实第三方源需要行级规则时（届时两个实现同时存在，trait 才不是空壳） |
| `policy/colmask.rs` + `RowPolicy::column_mask` | 唯一实现恒返 `vec![]`，且全仓无调用点 | 列权限的真实需求由 F5 的「结果列脱敏」承接；要 mask 模式（末 4 位可见）时再加 |
| `kernel::present` 的 `PresentLexicon`(11 字段) / `WordRule` / `code_labels` 形参 | 为了「零 DMS 语料」凭空造注入结构，被参数化的正是中文业务词；其中 `bare_metric_excludes` 参数化的是 `viewspec.rs:190-192` 的**空 if**（死代码）。546 行的 viewspec 会变成 895 行跨两个 crate | 不加回来：类型留 kernel、算法与词表整块在 semantic 就够 |
| `Answer::ColumnMeta`（含 `code_map`） | 零生产者：`annotate` 与词表推断对同一字段给两个来源（双源打架且无测试能抓），前端今天读的是 `view.columns[].semantic` | 前端要读服务端码表时（K6 之后），那时只有一个来源 |
| `AnswerBody::Steps` + `StepRecord` | 零生产者（ReAct 是两份 YAGNI 清单都明写不做的） | ReAct，5 行 |
| `is_transient` ×4（+ 文本匹配纯函数 + 8 单测） | 全仓没有退避重试的调用点，现状代码也零重试 | 真要重试时，在唯一需要的那个 await 点写 3 行循环 |
| `IdentityProvider` trait + `IdentityCred` | 两个实现零 dyn 调用点，两个 handler 也不共用路径（一个返 JSON 一个返 302）。推翻裁决 T10-3 | 第三个身份提供方 |
| `jobs::Job` fn 指针注册表 | 只有一个任务（graph nightly）。v1 §6 YAGNI 清单第一条就是「不做 SPI/插件注册」 | 第二个定时任务 |
| `notify.rs` | 唯一消费者是「jobs 失败告警」这个没人提过的需求，且默认 `no-op`。先 `tracing::warn!` | 真要企微推送且有第二个消费者时 |
| `Dialect` 的 `time_fn/TimeFn/quote_ident/classify_column/ColumnKind/limit_clause` | v1 零消费者或零方言差异（MySQL 与 PG 的 `LIMIT n` 相同） | `time_fn` 在 K3（PG 规则时间解析） |
| 拆两个 bin（`dms-ai-serve.exe`） | 会静默打死唯一启动链路：`scripts/run.ps1:29` 起的是 `dms-ai-server.exe` 无参（今天=serve），拆后启动的是 CLI → 空 args 直接退出，M7g 全栈脚本表面成功、后端根本没起；`build.ps1:5` 也杀不到改名进程（撞回 M8a 记的「旧进程锁 exe」坑）。推翻裁决 T10-2 方案 B，改名收益为零 | 不加回来 |
| `RetrieveCfg` 六个可调参 / `IngestCfg.tabular_channel` / 连接池 `cap` / `DocStatus::can_advance_to` / `FixedStmt::fetch_scalar` / 12 个 prompt `.md` 里的 10 个 | 没有运维入口会改的「配置」、没人要求的开关、没有调用点的方法、只为写一句方言名的模板文件 | 改常量重编译比加一层配置便宜 |

**三处「同一信任边界两份实现」全部合并**（漂出来的宽松那份就是入口）：敏感列 2 词 vs 9 词 → `kernel::nl::lexicon::SENSITIVE_COLS`；`SafeIdent` 三套字符集 → `connector::ddl::SafeIdent`；上传白名单 server + knowledge 两份 → `knowledge::ingest` 一份。

---

## 9. 与执行计划的对应

轨 A（T1-T10 架构迁移）与轨 B（K1-K6 能力包）**并行**，契约 B1-B7 见 `plans/2026-07-27-trackB-knowledge-multisource.md` §0。本文对既有 plan 的修订点：

| 步骤 | 落本文哪些文件 | 本文改了 plan 什么 |
|---|---|---|
| T1 骨架 | 6 个空 crate（含 `knowledge`） | —— |
| T2 kernel 纯算法 | §4.1 除 `gate/run/answer` | 词表验收口径改「通用段收敛 kernel + 业务段收敛注册表」；`civil_from_days` 删除 |
| T3 newtype 闸门 | `sql/gate.rs`、`connector/{source,fixed,mysql}` | 加 `UnrestrictedProof`（F2）与 schema deny-list（F3）；15 个 inject 测试**不改**（走 `rewrite()`） |
| T4 connector | §4.2 | 删 `is_transient`/`cap`；`EmbedClient` 实例化；`connect(ds,…)` |
| T5 policy | §4.3 | 删 `rowpolicy/rule_table/colmask`；`builtin_rules` 归 policy（32 表）；缓存改 TTL+版本 |
| T6 DDL+种子 | `migrations/` `seeds/` `bootstrap.rs` | 种子条数以实测为准；`0020` 同一 migrator |
| T7 recall+correct | `registry/*` `recall/*` `correct/*` | 新增 `registry/exemplar.rs`（agent 不许直写 `meta.*`）；`retrieve` 改吃 `RecallCtx`；`correct/agg` 拆两文件 |
| T8 direct 解体 | `compose/*` `fastpath/*` | `compose_sql_with` → `Plan` 六方法；时间桥读 `metric.time_col` 单独提交并出数值对照清单 |
| T9 pipeline 解体 | §4.6 + `kernel/{answer,run}` | 删 `AskRun`（显式循环）；Router `[graph,compose,fastpath,cache,llm]` 且 compose/fastpath accept 恒真；`Answer` serde tag 写死 |
| T10 server 瘦身 | §4.7 | 单 bin；`identity` 去 trait；`jobs` 去注册表；删 `notify` |
| K1/K2 知识库 | §4.5 + `api/kb.rs` + 前端三处 | chunk 400/60；`acl.rs` 独立 + `perm` 列；`KbAnswer.vue` 与 `kb_eval.py` 进交付清单 |
| K3 多源 | `registry/datasource.rs`、`Dialect::time_fn`、`agent/source.rs` | `ds_id` 化拆三个提交（加列 → 开谓词 → 上线选源）；`cleanup_autodiscover.py` 两条 DELETE 加 `ds_id` |
| K4 表格双通道 | `knowledge/tabular.rs` + `connector/ddl.rs` | DDL 与 `SafeIdent` 只在 connector；注册数据源与写 ACL 在同一函数 |
| K5 分诊 | `agent/{triage,ask,compound,answerers/knowledge}.rs` | 知识库 answerer 不进 `default_router()` |
| K6 对外与运营 | `api/{mcp,stats,registry_admin}.rs`、`registry/admin.rs`、迁移 `0023_query_log.sql` | server 预算提到 28 文件 / 2600 行；**workspace 隔离（H7）与推荐追问（H10）明确推迟，不进本轮预算** |

## 10. 收敛条件

- 单文件硬线 §0 D2；每 crate 预算 §1；超预算时的合并顺序已写死。
- `knowledge` 若 6 个月内只剩「上传→检索→引用」一条链（删掉表格双通道），回落 4 文件。
- `agent` 若在 M+3 时仍只有 axum 一个消费者，执行 agent/server 合并回 5 crate。
- 数据源实现 ≤3 时不引注册中心（`match kind` 显式可 grep）。
- 每个 `ponytail:` 标注（`OwnedStore::pool` 的 grep 天花板、AGE `esc()` 弱转义、sha256 走 PG 往返、`RuleTablePolicy` 缺席）都要进 `/ponytail-debt` 清单，别烂成「later means never」。
