//! `meta.*` 16 张表的 DDL + `ds_id` 化迁移。变更原因＝表结构。
//!
//! 搬运源 `server/src/meta.rs:13-240`（逐行搬运：DDL 文本、按 `;` 切分执行的实现、
//! `DS_PKS` 的列序与幂等跳过判定全部原样）。
//!
//! **本轮不改写成 `sqlx::migrate!`**：它会动 `_sqlx_migrations`，而 PG 连不上就没法证明没漂。

use sqlx::PgPool;

/// `meta.value_map.origin` 的**取值单一事实源**（与 `ingest::ORIGIN_INFORMATION_SCHEMA` 同一先例：
/// 写入侧与判据侧必须认同一个字面量，各写一份字符串就会漂）。
///
/// 三种来源的差别只有一件事 —— **登记的取值是不是这一列的完整枚举**，而
/// `CaliberRule::RequireKnownValue`（「值不在码表 → 返 0 行」那条判据）只对可证完整的那批开火。
///
/// 🔴 `seed` 是 `ADD COLUMN` 的**默认值，且必须是三者里最保守的那个**：`seed_defs` 手写的码表
/// 只播了会用到的那几个取值（不是完整枚举），既有行全部落它 —— 默认成 `dict` 就等于对手写那批
/// 开火，那是假红，而误伤一条会连带把本来对的答案回炉改错（裁决 二·G）。
pub const VALUE_ORIGIN_SEED: &str = "seed";

/// autodiscover **字典对码**那批写这个（写入点 `ingest::autodiscover::register::register_match`）：
/// 登记的是字典**全码**，且抽样值集 `uniq.len() > 60` 即整列跳过（`match_dict.rs`）
/// ⇒ 这一列的取值可证完整枚举 ⇒ **判据只对这一批开火**。
///
/// 已知天花板（判宽的那一侧，写下来是因为它是唯一的假红来源）：对码只要求抽样覆盖 ≥80%，
/// 故列里可能有 ≤20% 的码不在字典里。判据用「只判非 ASCII 字面量」把这个缺口挡掉了 ——
/// 未覆盖的那些**码**照写不判，见 `dms_kernel::sql::caliber` 的 `known_value`。
pub const VALUE_ORIGIN_DICT: &str = "dict";

/// autodiscover **名称型探针**那批写这个（写入点 `register::register_domain_values`）：
/// 抽样上限 `DOMAIN_LIMIT = 2000` 会**截断**（分类名百级、品牌名可能上千）⇒ 不是完整枚举
/// ⇒ 判据一律不开火（那批由 `RequireJoinAndFilter` 管，两条判据不重叠）。
pub const VALUE_ORIGIN_PROBE: &str = "probe";

/// 16 张 `meta.*` 表的建表/加列 DDL。**按分号逐句切执行**（见下方 `migrate`），
/// 故 `DO $$` 块与「注释里带半角分号」一律不许出现 —— 半角分号会把一条语句劈成
/// 「只有注释的空语句」+「以裸单引号开头的残句」，服务与全部 CLI 启动即语法错误（踩过一次）。
const DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS meta;
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE EXTENSION IF NOT EXISTS vector;
CREATE TABLE IF NOT EXISTS meta.table_doc(
  -- 新库直接带 ds_id 复合主键（老库本句 no-op，走 ALTER + rekey 路径），省一次主键重建
  ds_id text NOT NULL DEFAULT 'dms',
  table_name text NOT NULL,
  table_comment text NOT NULL DEFAULT '',
  domain text NOT NULL DEFAULT '',
  warn text NOT NULL DEFAULT '',
  row_estimate bigint NOT NULL DEFAULT 0,
  search_doc text NOT NULL DEFAULT '',
  updated_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (ds_id, table_name)
);
CREATE INDEX IF NOT EXISTS idx_table_doc_trgm ON meta.table_doc USING gin (search_doc gin_trgm_ops);
-- 表召回的向量半（`recall::schema::vector_tables`）。**2026-07-28 查库：这一列压根不存在** ——
-- 唯一的 ADD COLUMN 在 `tools/embed_service.py build` 里，而 build 从未跑过。于是那条召回 SQL
-- 每次都 42703 `column "embedding" does not exist`，被 `.unwrap_or_default()` 吞成空集、零日志，
-- 而 trgm 兜底总能把 6 个额度填满 ⇒ 三路召回只剩两路，外面一点看不出来。
-- 服务侧必须自己声明：DDL 是**启动**路径，build 是离线路径，不能拿离线脚本当建表工具。
ALTER TABLE meta.table_doc ADD COLUMN IF NOT EXISTS embedding vector(512);
-- 索引名与 `embed_service.py build` 逐字同名（idx_doc_hnsw + vector_cosine_ops）：
-- 换个名字就是两边各建一棵 HNSW，同一列上两份索引各占一份内存还都得维护。
CREATE INDEX IF NOT EXISTS idx_doc_hnsw ON meta.table_doc USING hnsw (embedding vector_cosine_ops);
-- 🔴 **人工注释与原生注释分列**（照 SQLBot 的两列制）。
--
-- 由来：`ingest::schema_sync::upsert_table_doc` / `upsert_column_doc` 的 `ON CONFLICT DO UPDATE`
-- **无条件覆盖** `table_comment` / `col_comment`，而那两列采的是 MySQL 的 `COMMENT` 原文。
-- 于是任何人工修正都活不过下一次 `meta sync`。本轮 `seed_table_comments` 修的 4 条张冠李戴
-- （`t_regions` 在 prompt 里自称「开票申请单」等）**今天靠的是运气**：`meta sync` 子命令里
-- seed 恰好排在 sync 之后，顺序一换就静默失效。
-- `warn` 列侥幸没事（那两个 DO UPDATE 的 SET 列表里没有它）—— 那同样是巧合不是设计。
--
-- 分列之后：sync 只写原生列，人工列谁都不许覆盖；渲染 prompt 时人工列优先（见
-- `recall::schema::render_schema`）。这也是「业务人员自助维护注释」的前提 ——
-- 没有它那件事落地即被下一次 sync 抹掉。
ALTER TABLE meta.table_doc ADD COLUMN IF NOT EXISTS custom_comment text NOT NULL DEFAULT '';
-- 【A20】人工勾选：`enabled=false` 的表不进任何一路召回（误采的业务表不再只能靠改
-- Rust 规则下线）。三路谓词 + render 闸 + drift 同形守卫在 `recall/schema.rs` 与
-- `semantic/tests/drift.rs` —— 漏一路就等于没关（计划原话）。
ALTER TABLE meta.table_doc ADD COLUMN IF NOT EXISTS enabled boolean NOT NULL DEFAULT true;
CREATE TABLE IF NOT EXISTS meta.column_doc(
  table_name text NOT NULL,
  column_name text NOT NULL,
  data_type text NOT NULL DEFAULT '',
  col_comment text NOT NULL DEFAULT '',
  custom_comment text NOT NULL DEFAULT '',
  ordinal int NOT NULL DEFAULT 0,
  PRIMARY KEY(table_name, column_name)
);
-- 老库补列（`CREATE TABLE IF NOT EXISTS` 对已存在的表不生效）
ALTER TABLE meta.column_doc ADD COLUMN IF NOT EXISTS custom_comment text NOT NULL DEFAULT '';
CREATE TABLE IF NOT EXISTS meta.kw_force(
  keyword text PRIMARY KEY,
  table_name text NOT NULL
);
CREATE TABLE IF NOT EXISTS meta.pitfall(
  id bigserial PRIMARY KEY,
  kind text NOT NULL DEFAULT 'pitfall',
  trigger_words text NOT NULL,
  lesson text NOT NULL,
  status text NOT NULL DEFAULT 'active',
  created_at timestamptz NOT NULL DEFAULT now()
);
-- 【S1】Agent 产物与 BI 日报**共用一张表**（预览地基，datanote 预览功能的落点）：
-- `conv_id` 归属会话（日报为空串）；`kind` = markdown/report/…（决定前端图标）；
-- `html` 是渲染好的整页（服务端渲染，前端只负责 iframe 沙箱展示，不跑任何脚本信任）。
CREATE TABLE IF NOT EXISTS meta.artifact(
  id bigserial PRIMARY KEY,
  conv_id text NOT NULL DEFAULT '',
  kind text NOT NULL DEFAULT 'markdown',
  title text NOT NULL DEFAULT '',
  html text NOT NULL DEFAULT '',
  created_by text NOT NULL DEFAULT '',
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_artifact_conv ON meta.artifact(conv_id, id DESC);
CREATE INDEX IF NOT EXISTS idx_artifact_kind ON meta.artifact(kind, id DESC);
-- 【分享】`share_token`（空 = 未分享）：uuid 即能力 —— 持链接者可看，无需登录。
-- 只授读，不授写；撤销 = 清空这一列。查询路径只认 `share_token <> ''`（部分索引同口径，
-- 空串不占索引；老库的全量索引由 migrate 的条件 DROP 升级）。
ALTER TABLE meta.artifact ADD COLUMN IF NOT EXISTS share_token text NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS idx_artifact_share ON meta.artifact(share_token) WHERE share_token <> '';
-- 【D6 版本链】同会话、同 (kind,title) 的产物重生成时版本自增，老版本保留可回看。
-- title 即链键（slug 的最小形态：产物链的身份 = 「这个会话里叫这个名字的这类报告」），
-- 版本自增在 artifact_api::INSERT_SQL 的 MAX(version)+1 子查询里，唯一索引兜底并发撞号。
ALTER TABLE meta.artifact ADD COLUMN IF NOT EXISTS version int NOT NULL DEFAULT 1;
-- 老数据回填：链内多行按 id 序编 1..n（新版本的 id 恒更大 ⇒ id 序与 version 序天然一致；
-- 单行链算出 1 = 默认值，WHERE 条件把它滤成 no-op，故每次启动重跑也幂等）。
UPDATE meta.artifact a SET version = s.v FROM (
  SELECT id, row_number() OVER (PARTITION BY conv_id, kind, title ORDER BY id) AS v
  -- ds:any —— meta.artifact 无 ds_id（产物归属按 conv_id/created_by，不是数据源维度），
  -- 版本回填是迁移期全表批处理：按 every_meta_recall_is_ds_scoped 的跨源豁免约定显式标记。
  FROM meta.artifact
) s WHERE a.id = s.id AND a.version <> s.v;
CREATE UNIQUE INDEX IF NOT EXISTS idx_artifact_chain ON meta.artifact(conv_id, kind, title, version);
-- 【S4】经验复盘（datanote AiMemoryService 对应物）：过往会话蒸馏出的修正经验。
-- `kind` = skill/review/experience（v1 只写 review = 口径/执行回炉成功后的沉淀）；
-- `question` 供去重与排查；`embedding` 由 A9 向量自愈补（MetaVecTarget::Memory）；
-- `hit_count` 是召回侧的「被印证次数」，参与 rerank（datanote 的 hitCount+recency 同构）。
-- 🔴 经验**只进 LLM prompt 的参考段**，绝不进口径判据/闸门 —— 它是未连库验证的二手材料。
CREATE TABLE IF NOT EXISTS meta.memory(
  id bigserial PRIMARY KEY,
  ds_id text NOT NULL DEFAULT '',
  conv_id text NOT NULL DEFAULT '',
  kind text NOT NULL DEFAULT 'review',
  question text NOT NULL DEFAULT '',
  content text NOT NULL,
  embedding vector(512),
  hit_count int NOT NULL DEFAULT 0,
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_memory_ds ON meta.memory(ds_id, id DESC);
CREATE TABLE IF NOT EXISTS meta.sql_exemplar(
  id bigserial PRIMARY KEY,
  question text NOT NULL,
  sql text NOT NULL,
  embedding vector(512),
  created_at timestamptz NOT NULL DEFAULT now()
);
-- 复核态（移植 SuperSonic MemoryReviewTask）：pending 未复核 / enabled 复核通过 / disabled 判错剔除
ALTER TABLE meta.sql_exemplar ADD COLUMN IF NOT EXISTS status text NOT NULL DEFAULT 'pending';
-- 术语注册表（移植 SuperSonic DomainTerms）：业务黑话→标准口径
CREATE TABLE IF NOT EXISTS meta.term(
  term text PRIMARY KEY,
  definition text NOT NULL,
  aliases text[] NOT NULL DEFAULT '{}',
  status text NOT NULL DEFAULT 'active'
);
-- 指标注册表（移植 SuperSonic 语义层 MetricResp 最小可用）：指标名→口径单一事实源
CREATE TABLE IF NOT EXISTS meta.metric(
  metric_code text PRIMARY KEY,
  name text NOT NULL,
  aliases text[] NOT NULL DEFAULT '{}',
  source_table text NOT NULL,
  agg_expr text NOT NULL,
  scope_filter text NOT NULL DEFAULT '',
  description text NOT NULL DEFAULT '',
  status text NOT NULL DEFAULT 'active'
);
-- 默认时间列（SuperSonic 分区时间维度）：同表多时间列语义不同且有全 NULL 坑列，口径必须钉死
ALTER TABLE meta.metric ADD COLUMN IF NOT EXISTS time_col text NOT NULL DEFAULT '';
-- 去重键：来源表含系统级重复行（ETL 双写）时聚合前须按这些列 DISTINCT，否则数值虚增
ALTER TABLE meta.metric ADD COLUMN IF NOT EXISTS dedup_keys text NOT NULL DEFAULT '';
-- 结果单位：''无单位 / percent百分数（必须 * 100.0）/ ratio小数比值（不得 * 100）/ amount金额 / qty数量。
-- 评测「今年退款占比」答 0.049 而正确 4.9 —— 差的就是这一句声明，占比的单位从来没被写下来过。
ALTER TABLE meta.metric ADD COLUMN IF NOT EXISTS unit text NOT NULL DEFAULT '';
-- 表级标准口径（SuperSonic 数据模型 model filter）：无论谁 JOIN 这张表都恒成立的过滤。
-- 解决「明细类指标 JOIN 订单主表却漏掉有效订单过滤 → 数值虚增」（评测抓获销量虚高 41%）。
CREATE TABLE IF NOT EXISTS meta.table_scope(
  table_name text PRIMARY KEY,
  filter text NOT NULL,
  note text NOT NULL DEFAULT ''
);
-- 快照/流水表声明：同一分区键有多条历史行，取数【必须】只留最新一条
-- （ROW_NUMBER() OVER (PARTITION BY 分区列 ORDER BY 排序列) 取 rn = 1）。
-- 漏了就是把历史行重复求和 —— 实测「账户余额TOP10」第 1 名客户答错、
-- 「有信控余额客户清单」21 行 vs 正确 23 行。extra_filter = 该表恒需的额外过滤（如仅生效状态）。
-- ds_id 建表即入主键（不进 rekey_ds_pk 那张补丁清单）。
CREATE TABLE IF NOT EXISTS meta.table_snapshot(
  ds_id text NOT NULL DEFAULT 'dms',
  table_name text NOT NULL,
  partition_cols text NOT NULL DEFAULT '',
  order_cols text NOT NULL DEFAULT '',
  extra_filter text NOT NULL DEFAULT '',
  note text NOT NULL DEFAULT '',
  PRIMARY KEY(ds_id, table_name)
);
-- 实体名值域声明：这一列的取值是**业务实体名**（分类名/品牌名…），不是码值。
-- 与 meta.value_map 的分工：那张是码值字典（枚举 ≤34 值，靠 _code|_type|_status 后缀被
-- autodiscover 发现），而实体名列的后缀不匹配 → 从未被发现 → LLM 只能猜成「名字 LIKE」，
-- 把名字含该词却不属该实体的行算进来（实测「手抓饼这个分类卖了多少箱」虚高 36%）。
-- 这张只登记「哪一列是值域」+ 那句人话；**取值本身落 meta.value_map**（name=code=取值，
-- 由 `meta autodiscover` 的名称型探针灌，重跑即自适应）—— 复用码值表，零 DDL 零迁移。
CREATE TABLE IF NOT EXISTS meta.value_domain(
  ds_id text NOT NULL DEFAULT 'dms',
  table_name text NOT NULL,
  column_name text NOT NULL,
  note text NOT NULL DEFAULT '',
  PRIMARY KEY(ds_id, table_name, column_name)
);
-- DMS 单据族：前缀、主表、明细表与源码证据。运行时解析仍使用同一 Rust 注册表，
-- 本表用于后台审核、图谱同步状态核查和后续自动抽取差异对比。
CREATE TABLE IF NOT EXISTS meta.document_family(
  ds_id text NOT NULL DEFAULT 'dms',
  family_code text NOT NULL,
  name text NOT NULL,
  prefixes text[] NOT NULL DEFAULT '{}',
  header_table text NOT NULL,
  header_code_col text NOT NULL,
  detail_bindings text[] NOT NULL DEFAULT '{}',
  evidence text NOT NULL DEFAULT '',
  warehouse_available boolean NOT NULL DEFAULT false,
  status text NOT NULL DEFAULT 'active',
  updated_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY(ds_id, family_code)
);
ALTER TABLE meta.document_family
  ADD COLUMN IF NOT EXISTS warehouse_available boolean NOT NULL DEFAULT false;
-- 维度注册表（移植 SuperSonic DimensionResp 最小可用）：维度名→分组取数口径单一事实源
CREATE TABLE IF NOT EXISTS meta.dimension(
  dim_code text PRIMARY KEY,
  name text NOT NULL,
  aliases text[] NOT NULL DEFAULT '{}',
  source_table text NOT NULL,
  expr text NOT NULL,
  description text NOT NULL DEFAULT '',
  status text NOT NULL DEFAULT 'active'
);
-- 值链接码表（移植 SuperSonic ValueLinking）：编码列 中文名→码，写中文名必返0行的确定性纠正依据
CREATE TABLE IF NOT EXISTS meta.value_map(
  table_name text NOT NULL,
  column_name text NOT NULL,
  name text NOT NULL,
  code text NOT NULL,
  match_kind text NOT NULL DEFAULT 'eq', -- eq=等值换码 / like=组合值列须 LIKE '%码%'
  PRIMARY KEY(table_name, column_name, name)
);
-- 来源标记 seed/dict/probe（取值与理由见本文件顶部三个常量）。判据「值不在码表 → 返 0 行」
-- 只对可证完整枚举的 dict 那批开火。**默认必须是最保守的 seed**：既有行与手写种子都只播了
-- 部分取值，默认成 dict 就是对它们造假红。
ALTER TABLE meta.value_map ADD COLUMN IF NOT EXISTS origin text NOT NULL DEFAULT 'seed';
-- 元素注册表（移植 SuperSonic SchemaElement）：metric/dimension/value/term 统一为可向量召回的元素
CREATE TABLE IF NOT EXISTS meta.element(
  element_id text PRIMARY KEY,       -- kind:标识
  kind text NOT NULL,                -- metric / dimension / value / term
  name text NOT NULL,
  aliases text[] NOT NULL DEFAULT '{}',
  ref_expr text NOT NULL DEFAULT '', -- agg_expr / 维度取值表达式 / 码值 / 术语定义
  description text NOT NULL DEFAULT '',
  search_text text NOT NULL DEFAULT '', -- 名+别名+描述（向量化文本）
  status text NOT NULL DEFAULT 'active'
);
ALTER TABLE meta.element ADD COLUMN IF NOT EXISTS embedding vector(512);
-- 纠错反哺日志（自进化引擎B+）：确定性校正器每次出手都记录，同错累计→升格 pitfall 教训
CREATE TABLE IF NOT EXISTS meta.correction_log(
  id bigserial PRIMARY KEY,
  -- 自由文本列：取值由产出方（校正器/复盘链，见 agent::guard 的裁决落点）各自登记，
  -- 本表不维护枚举计数（在这里写死「现有 N 种」必漂）。「有多少答案没被校验过」看
  -- caliber-* 族（caliber-grader-error 是判据没跑起来的那档）。
  kind text NOT NULL,
  question text NOT NULL,
  detail text NOT NULL,      -- 纠正要点（幻觉列名/聚合改写/码值换写等）
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_correction_kind ON meta.correction_log(kind, created_at);
-- 失败复盘日志（自进化引擎C）：执行报错/超时/0行 记录，报错类由 LLM 复盘产出候选教训
CREATE TABLE IF NOT EXISTS meta.failure_log(
  id bigserial PRIMARY KEY,
  kind text NOT NULL,        -- exec-error / zero-rows
  question text NOT NULL,
  sql text NOT NULL DEFAULT '',
  error text NOT NULL DEFAULT '',
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_failure_kind ON meta.failure_log(kind, created_at);
-- 🔴 **一次问答的关联键**（照 SuperSonic 四段落库 / SQLBot 分步日志的立意）。
--
-- 由来：三张日志表各记一段，但**拼不回同一次问答** —— `query_log` 一行（最终 SQL）、
-- `correction_log` 记 before→after 的九个 kind、`failure_log` 记失败，三张之间没有任何键。
-- 今天只能按 question 文本对，而 `chat.rs:117` 已经吃过一次这个亏
-- （「query_log 没有 conv_id，从它拿不回本会话上一轮」）。
-- 直接后果：「数字错了，是模型写错还是某个校正器改坏的」这个问题查不出来 ——
-- 那正是本仓最高频的排查题。
--
-- `trace_id` 一次问答一个（子问题共用父的）；`conv_id` 一次会话一个。
-- 都可空：老行没有，且写入点失败不许让问答失败。
ALTER TABLE meta.correction_log ADD COLUMN IF NOT EXISTS trace_id text;
ALTER TABLE meta.failure_log    ADD COLUMN IF NOT EXISTS trace_id text;
ALTER TABLE IF EXISTS meta.query_log      ADD COLUMN IF NOT EXISTS trace_id text;
ALTER TABLE IF EXISTS meta.query_log      ADD COLUMN IF NOT EXISTS conv_id  text;
-- 两次 precise 调用（首轮生成 + 自修）的成本今天分不开：`query_log` 只有 token 总和。
-- `llm_calls` 记这一轮真的打了几次 LLM —— 开了自一致采样之后这个数才有意义
-- （`sc_samples=3` 时它是 1~3，提前收工的效果直接读得出来）。
ALTER TABLE IF EXISTS meta.query_log      ADD COLUMN IF NOT EXISTS llm_calls int NOT NULL DEFAULT 0;
-- 【A10】语料同构快照（SuperSonic `Text2SQLExemplar{question, dbSchema, sideInfo, sql}`）：
-- 沉淀时把当轮的 schema 段与口径卡（side_info）存**渲染好的文本** —— 历史样例从此带得回
-- 当时的表结构与口径上下文。可空：老行没有；今天 few-shot 渲染仍是两行式，
-- 这两列先服务复核与未来渲染（渲染激活等「样例引了召回不到的表」的实测证据）。
ALTER TABLE meta.sql_exemplar ADD COLUMN IF NOT EXISTS schema_snapshot text NOT NULL DEFAULT '';
ALTER TABLE meta.sql_exemplar ADD COLUMN IF NOT EXISTS side_info       text NOT NULL DEFAULT '';
-- 【S2 VQR】SQL 样例必须经过人工确认 + 当前只读业务源真实执行，才可进入 few-shot/语义缓存。
-- AI 复核只记候选意见，不再直接授予 enabled。旧数据默认 unverified，避免历史错误继续自传播。
ALTER TABLE meta.sql_exemplar ADD COLUMN IF NOT EXISTS ai_review text NOT NULL DEFAULT '';
ALTER TABLE meta.sql_exemplar ADD COLUMN IF NOT EXISTS validation_status text NOT NULL DEFAULT 'unverified';
ALTER TABLE meta.sql_exemplar ADD COLUMN IF NOT EXISTS reviewed_by text NOT NULL DEFAULT '';
ALTER TABLE meta.sql_exemplar ADD COLUMN IF NOT EXISTS reviewed_at timestamptz;
ALTER TABLE meta.sql_exemplar ADD COLUMN IF NOT EXISTS validated_at timestamptz;
ALTER TABLE meta.sql_exemplar ADD COLUMN IF NOT EXISTS validated_source text NOT NULL DEFAULT '';
ALTER TABLE meta.sql_exemplar ADD COLUMN IF NOT EXISTS validated_fingerprint text NOT NULL DEFAULT '';
ALTER TABLE meta.sql_exemplar ADD COLUMN IF NOT EXISTS invalid_reason text NOT NULL DEFAULT '';
ALTER TABLE meta.sql_exemplar ADD COLUMN IF NOT EXISTS metric_versions text NOT NULL DEFAULT '';
-- 指标版本与可组合维度白名单。'*'=兼容期允许全部，空数组=只能做无维度指标。
ALTER TABLE meta.metric ADD COLUMN IF NOT EXISTS version text NOT NULL DEFAULT '1';
ALTER TABLE meta.metric ADD COLUMN IF NOT EXISTS allowed_dimensions text[] NOT NULL DEFAULT ARRAY['*']::text[];
-- 【运行时开关】极少量「页面可改、保存即生效」的配置（今天是 `llm_provider`）。
-- 只放**非密钥**（红线：key/DSN 只住 settings.json，永不落库）。写口：
-- `admin_api::set_llm_provider`；读口：启动一次 + 保存时热改（不需要重启）。
CREATE TABLE IF NOT EXISTS meta.kv(
  k text PRIMARY KEY,
  v text NOT NULL DEFAULT ''
);
-- trace_id 可空：老行没有。部分索引不含 NULL 行（查询只认非空 trace_id；老库全量索引
-- 由 migrate 的条件 DROP 升级）。
CREATE INDEX IF NOT EXISTS idx_correction_trace ON meta.correction_log(trace_id) WHERE trace_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_failure_trace    ON meta.failure_log(trace_id) WHERE trace_id IS NOT NULL;
-- idx_query_trace 移交 server/query_log.rs（它是该表的唯一属主：本文件按分号逐句切执行，
-- 在全新空库上跑在本表建表之前，CREATE INDEX 没有 IF EXISTS 表级容错）。
-- JOIN 边注册表（SuperSonic JoinPath 思想）：表间可连接边+基数，组合器跨基表路径推导用
CREATE TABLE IF NOT EXISTS meta.join_edge(
  left_table text NOT NULL,
  left_col text NOT NULL,
  right_table text NOT NULL,
  right_col text NOT NULL,
  card text NOT NULL DEFAULT 'N:1',  -- left→right 基数：1:N(扇出,聚合危险) / N:1(收敛,安全)
  note text NOT NULL DEFAULT '',
  status text NOT NULL DEFAULT 'active',
  PRIMARY KEY(left_table, left_col, right_table, right_col)
);
-- 表权限档案（fail-closed）：scoped=注入条件 / global=Java 无 @DataScope 审定全量可见 / via=独查借头表条件
CREATE TABLE IF NOT EXISTS meta.scope_binding(
  table_name text PRIMARY KEY,
  mode text NOT NULL DEFAULT 'scoped',
  customer_col text,
  customer_kind text NOT NULL DEFAULT 'codes', -- codes | manager_codes | shop_codes
  owner_col text,
  owner_kind text,          -- ids | codes | login
  via_table text,
  via_local_col text,
  via_remote_col text,
  note text NOT NULL DEFAULT ''
);
-- 【K3】数据源注册表。**只存 dsn_ref 键名，明文 DSN 只在 settings.json**——
-- 这一行会进接口响应与日志，口令跟着走一次就再也收不回来。
-- policy_kind v1 只有两种：dms_datascope（行级权限由 inject 兜）/ global（可见性全靠 ds 级 ACL）。
CREATE TABLE IF NOT EXISTS meta.datasource(
  ds_id       text PRIMARY KEY,
  name        text NOT NULL,
  kind        text NOT NULL,
  dialect     text NOT NULL,
  dsn_ref     text NOT NULL,
  policy_kind text NOT NULL DEFAULT 'dms_datascope',
  description text NOT NULL DEFAULT '',
  workspace   text NOT NULL DEFAULT 'default',
  status      text NOT NULL DEFAULT 'active',
  embedding vector(512),
  created_at  timestamptz NOT NULL DEFAULT now()
);
-- ── 【K3-B ①】注册表 ds_id 化：只加列，本步不改任何查询 ──
-- 'dms' = 只在 DMS 源生效（存量全部落这里）；'*' = 全局生效（跨源共享的口径）。
-- 主键前置 ds_id 在 `rekey_ds_pk`（PG 侧幂等）。⚠️ 本 DDL 串按分号逐句切，故 `DO $$` 块
-- 与「注释里带半角分号」一律不许出现（会把一条语句劈成两半，migrate 当场语法错误）。
-- scope_binding 只加列**不动主键**：inject.rs 的 `ON CONFLICT (table_name)` 会当场炸（那不是
-- 本任务的文件），而权限档案只服务 policy_kind='dms_datascope' 的源（唯一 = dms）。
ALTER TABLE meta.table_doc     ADD COLUMN IF NOT EXISTS ds_id text NOT NULL DEFAULT 'dms';
ALTER TABLE meta.column_doc    ADD COLUMN IF NOT EXISTS ds_id text NOT NULL DEFAULT 'dms';
ALTER TABLE meta.kw_force      ADD COLUMN IF NOT EXISTS ds_id text NOT NULL DEFAULT 'dms';
ALTER TABLE meta.metric        ADD COLUMN IF NOT EXISTS ds_id text NOT NULL DEFAULT 'dms';
ALTER TABLE meta.dimension     ADD COLUMN IF NOT EXISTS ds_id text NOT NULL DEFAULT 'dms';
ALTER TABLE meta.term          ADD COLUMN IF NOT EXISTS ds_id text NOT NULL DEFAULT 'dms';
ALTER TABLE meta.value_map     ADD COLUMN IF NOT EXISTS ds_id text NOT NULL DEFAULT 'dms';
ALTER TABLE meta.element       ADD COLUMN IF NOT EXISTS ds_id text NOT NULL DEFAULT 'dms';
ALTER TABLE meta.join_edge     ADD COLUMN IF NOT EXISTS ds_id text NOT NULL DEFAULT 'dms';
ALTER TABLE meta.table_scope   ADD COLUMN IF NOT EXISTS ds_id text NOT NULL DEFAULT 'dms';
ALTER TABLE meta.scope_binding ADD COLUMN IF NOT EXISTS ds_id text NOT NULL DEFAULT 'dms';
ALTER TABLE meta.scope_binding ADD COLUMN IF NOT EXISTS customer_kind text NOT NULL DEFAULT 'codes';
ALTER TABLE meta.pitfall       ADD COLUMN IF NOT EXISTS ds_id text NOT NULL DEFAULT 'dms';
ALTER TABLE meta.sql_exemplar  ADD COLUMN IF NOT EXISTS ds_id text NOT NULL DEFAULT 'dms';
-- 指标级时间窗上限（'' = 无；'yesterday' = 仅适用于事实合同明确要求的延迟确认指标）。
-- 默认 DWS 销售事实按 order_date 聚合，必须保持空值，不继承旧发货口径上限。
ALTER TABLE meta.metric        ADD COLUMN IF NOT EXISTS time_cap text NOT NULL DEFAULT '';
"#;

pub async fn migrate(pg: &PgPool) -> anyhow::Result<()> {
    // 版本回填只做一次（meta.kv 哨兵）：已收敛的库启动不再全表扫 + window 计算。
    // ds:any —— meta.kv 是全局运行时开关表（无 ds_id 列），回填哨兵是全局状态，不按源切。
    let backfill_done: Option<String> = sqlx::query_scalar(
        "SELECT v FROM meta.kv WHERE k = 'artifact_version_backfill_done'",
    )
    .fetch_optional(pg)
    .await
    .unwrap_or(None); // 全新空库还没有 meta.kv（它在 DDL 后段建）→ 视为未做
    let backfill_done = backfill_done.as_deref() == Some("1");
    let mut n = 0usize;
    for stmt in DDL.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        if backfill_done && stmt.starts_with("UPDATE meta.artifact") {
            continue;
        }
        sqlx::query(stmt).execute(pg).await.map_err(|e| {
            // 逐句执行失败必须带是第几句（sqlx 默认不带 SQL 文本，60+ 句里定位靠猜）
            let head: String = stmt.chars().take(80).collect();
            anyhow::anyhow!("DDL 第 {} 句执行失败：{e}\n语句开头：{head}", n + 1)
        })?;
        n += 1;
    }
    if !backfill_done {
        sqlx::query(
            "INSERT INTO meta.kv(k, v) VALUES ('artifact_version_backfill_done', '1')
             ON CONFLICT (k) DO NOTHING",
        )
        .execute(pg)
        .await?;
    }
    // 老库的全量索引升级成部分索引（CREATE IF NOT EXISTS 不会替换既有索引定义，要 DROP 一次）
    for (idx, create_sql) in [
        ("meta.idx_artifact_share",
         "CREATE INDEX IF NOT EXISTS idx_artifact_share ON meta.artifact(share_token) WHERE share_token <> ''"),
        ("meta.idx_correction_trace",
         "CREATE INDEX IF NOT EXISTS idx_correction_trace ON meta.correction_log(trace_id) WHERE trace_id IS NOT NULL"),
        ("meta.idx_failure_trace",
         "CREATE INDEX IF NOT EXISTS idx_failure_trace ON meta.failure_log(trace_id) WHERE trace_id IS NOT NULL"),
    ] {
        let def: Option<String> = sqlx::query_scalar("SELECT pg_get_indexdef(to_regclass($1))::text")
            .bind(idx)
            .fetch_optional(pg)
            .await?
            .flatten();
        if let Some(def) = def {
            if !def.contains("WHERE") {
                sqlx::query(&format!("DROP INDEX {idx}")).execute(pg).await?;
                sqlx::query(create_sql).execute(pg).await?;
                tracing::info!("{idx} 已升级为部分索引");
            }
        }
    }
    rekey_ds_pk(pg).await?;
    tracing::info!(statements = n, "meta DDL 迁移完成");
    Ok(())
}

/// 主键前置 `ds_id`（【K3-B ①】）：`(表, 新主键列序)`。
/// 不在表里的四张：`pitfall`/`sql_exemplar` 主键是 bigserial id（不需要）、
/// `scope_binding` 见 `migrate` 里的注释、`datasource` 本身就是按 ds_id 建的。
const DS_PKS: &[(&str, &str)] = &[
    ("table_doc", "ds_id, table_name"),
    ("column_doc", "ds_id, table_name, column_name"),
    ("kw_force", "ds_id, keyword"),
    ("metric", "ds_id, metric_code"),
    ("dimension", "ds_id, dim_code"),
    ("term", "ds_id, term"),
    ("value_map", "ds_id, table_name, column_name, name"),
    ("element", "ds_id, element_id"),
    ("join_edge", "ds_id, left_table, left_col, right_table, right_col"),
    ("table_scope", "ds_id, table_name"),
];

/// 现有主键定义已前置 ds_id 才算迁移完成：`contains` 会把 `(table_name, ds_id)` 误判已迁移。
fn pk_prefixed_with_ds(def: &str) -> bool {
    def.starts_with("PRIMARY KEY (ds_id")
}

/// PK 前置 ds_id，**幂等**：现有主键已前置 ds_id 就跳过，否则 DROP + ADD（包一个事务：
/// 进程在两句之间崩溃 = 该表无 PK 直到下次启动成功）。
/// 跳过判定不可省——DROP/ADD 每次启动都跑一遍等于每次重建索引（`bootstrap_meta` 是
/// 服务与每次 `exec-sql` 都走的路径）。表名与列序全是本文件的字面量，无用户输入。
async fn rekey_ds_pk(pg: &PgPool) -> anyhow::Result<()> {
    // 一次查询取回全部现状（原来逐表一趟 pg_constraint）
    let names: Vec<String> = DS_PKS.iter().map(|(t, _)| format!("meta.{t}")).collect();
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT conrelid::regclass::text, conname::text, pg_get_constraintdef(oid)
         FROM pg_constraint WHERE conrelid = ANY($1::regclass[]) AND contype = 'p'",
    )
    .bind(&names)
    .fetch_all(pg)
    .await?;
    let mut defs: std::collections::HashMap<String, (String, String)> = std::collections::HashMap::new();
    for (rel, conname, def) in rows {
        // search_path 不同渲染可能剥掉 schema 前缀，两种形态都认
        let table = rel.strip_prefix("meta.").unwrap_or(&rel).to_string();
        defs.insert(table, (conname, def));
    }
    for (t, cols) in DS_PKS {
        match defs.get(*t) {
            Some((_, def)) if pk_prefixed_with_ds(def) => continue,
            found => {
                let mut tx = pg.begin().await?;
                if let Some((conname, _)) = found {
                    // 用查到的真实约束名 DROP（手工建过的非默认名不再空转后 ADD 报错；
                    // 目录名按标识符转义）
                    sqlx::query(&format!(
                        "ALTER TABLE meta.{t} DROP CONSTRAINT IF EXISTS \"{}\"",
                        conname.replace('"', "\"\"")
                    ))
                    .execute(&mut *tx)
                    .await?;
                }
                sqlx::query(&format!("ALTER TABLE meta.{t} ADD PRIMARY KEY ({cols})"))
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                tracing::info!("meta.{t} 主键前置 ds_id：({cols})");
            }
        }
    }
    Ok(())
}

/// 三条向量路的**就绪位**：那一列到底算出来了没有。`false` = 这条路今天是哑的。
///
/// `Serialize` 是给 `/api/health` 直接吐的（serde 本来就是本 crate 的依赖）——
/// 让调用方手搭一份 json 就等于第二处字段名，改名时那边不会跟着红。
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct VectorReady {
    /// 表召回的向量半（`recall::schema::vector_tables`）
    pub table_doc: bool,
    /// 元素卡的向量召回（`recall::cards::recall_elements`）
    pub element: bool,
    /// 向量选源（`agent::source::select_source`）
    pub datasource: bool,
}

/// 三处谓词与各自的生产 SQL 逐字同款：判宽一格 = 明明选不出候选却照样花那次 embed HTTP，
/// 判窄一格 = 灌好了向量却永远不走向量路。`vector_ready_matches_the_production_predicate` 钉着。
///
/// ⚠️ 下面那句 `ds:any` 必须**紧贴** const 那一行：`drift.rs` 的 ds 守卫按 `[i-2, i+8]` 行窗口
/// 扫，而第二个 `EXISTS` 落在 const 的第二行 ⇒ 两个窗口的交集只剩「紧贴」这一种放法
/// （标记先放高两行、再放高一行，各判红一次 —— 那道守卫确实在看内容）。
/// `ds:any` 的理由：三个位是**全局**体检（「向量路到底通不通」），按源切开就答不了那个问题。
const VECTOR_READY_SQL: &str = "SELECT EXISTS(SELECT 1 FROM meta.table_doc WHERE embedding IS NOT NULL) AS table_doc, \
     EXISTS(SELECT 1 FROM meta.element WHERE status = 'active' AND embedding IS NOT NULL) AS element, \
     EXISTS(SELECT 1 FROM meta.datasource WHERE status = 'active' AND embedding IS NOT NULL) AS datasource";

/// 向量就绪体检：**一句只读 SQL、三个 `EXISTS`**（命中即短路；全 NULL 时三张表全走顺序扫，
/// 也就是今天的最坏情况）。2026-07-30 实测（`EXPLAIN ANALYZE`，254 + 1033 + 4 行全 NULL）：
/// 规划 1.177 ms + 执行 2.094 ms。对照它替掉的那次 embed HTTP（同日实测 :8077 单条 query）：
/// 冷启 311 ms、热身后 14~19 ms。所以省的是 **~7 倍（热）/ ~100 倍（冷）**，
/// 外加单线程 :8077 被占时那条 3s 超时的长尾 —— 不是三个数量级，别把上限当日常。
///
/// 为什么落在 ddl.rs：这三列就是本文件声明的（各一句 `embedding vector(512)`），而
/// **「列建好了」≠「向量灌了」**——数据靠 `python tools/embed_service.py build` 离线算。
/// 2026-07-28 查库：`meta.table_doc` 连列都没有（本轮补的就是它）、`meta.element` 1033 行
/// embedding 全 NULL、`meta.datasource` 4 行 active / 0 行有向量、HNSW 索引 0 个 ——
/// 三条向量路从上线起全哑，而三个调用点都是 `.unwrap_or_default()` 静默降级 ⇒ 谁都看不出来。
/// 这个函数就是「看得出来」的那一眼。
///
/// 消费者：`agent::source::nearest_visible`（省掉一次注定返空的 embed HTTP）+ `/api/health`。
pub async fn vector_ready(pg: &PgPool) -> anyhow::Result<VectorReady> {
    // 按列名解码（三个同型 bool 按位置取的话，列序一调就静默张冠李戴）
    use sqlx::Row;
    let row = sqlx::query(VECTOR_READY_SQL).fetch_one(pg).await?;
    Ok(VectorReady {
        table_doc: row.get("table_doc"),
        element: row.get("element"),
        datasource: row.get("datasource"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 🔴 `value_map.origin` 的默认值必须是**最保守**的那个（`seed`）。
    ///
    /// 这不是洁癖：`RequireKnownValue` 拿 `origin = 'dict'` 当「取值可证完整枚举」的凭据，
    /// 而 `seed_defs` 手写的码表只播了会用到的那几个取值。默认值一改成 `dict`，
    /// 手写那批立刻被开火 —— 每一条写了「未播种但真实存在」的取值的正确 SQL 都会被判红回炉。
    /// 判据按字面量比对，所以这里同时钉住「DDL 里的默认值就是那个常量」。
    #[test]
    fn value_map_origin_defaults_to_the_most_conservative() {
        // 位置参数不是命名插值：`drift.rs::sql_interpolation_is_allowlisted` 扫的是含 SQL 关键字的
        // `format!` 块里的 `{名}`，而这一句里有「ALTER TABLE」。用 `{}` 就不必往那份白名单加例外。
        assert!(
            DDL.contains(&format!(
                "ALTER TABLE meta.value_map ADD COLUMN IF NOT EXISTS origin text \
                 NOT NULL DEFAULT '{}'",
                VALUE_ORIGIN_SEED
            )),
            "value_map.origin 的 DDL 默认值必须是 {VALUE_ORIGIN_SEED}"
        );
        // 三个来源两两不同：撞了就等于把「只对完整枚举开火」这条前提抹掉
        assert_ne!(VALUE_ORIGIN_SEED, VALUE_ORIGIN_DICT);
        assert_ne!(VALUE_ORIGIN_PROBE, VALUE_ORIGIN_DICT);
    }

    /// 🔴 `meta.table_doc.embedding` 必须在**启动路径**的 DDL 里，且加列在建索引之前。
    ///
    /// 由来（2026-07-28 查库坐实）：这一列只在 `tools/embed_service.py build` 里被 ADD，
    /// 而 build 从未跑过 ⇒ 列不存在 ⇒ `recall::schema::vector_tables` 那条 SQL 每次 42703，
    /// 被 `.unwrap_or_default()` 吞成「本来就没命中」⇒ SuperSonic 双召回的向量半从上线起没工作过。
    /// 顺序那条不是洁癖：`migrate` 按分号逐句执行，索引跑在加列之前就是当场 42703 启动失败。
    #[test]
    fn table_doc_has_the_vector_column_before_its_index() {
        let col = "ALTER TABLE meta.table_doc ADD COLUMN IF NOT EXISTS embedding vector(512)";
        // 索引名必须与 embed_service.py 的 build 同名，否则同一列上两棵 HNSW
        let idx = "CREATE INDEX IF NOT EXISTS idx_doc_hnsw ON meta.table_doc \
                   USING hnsw (embedding vector_cosine_ops)";
        let at = |needle: &str| DDL.find(needle).unwrap_or_else(|| panic!("DDL 里没有：{needle}"));
        assert!(at(col) < at(idx), "加列必须在建索引之前，否则 migrate 当场 42703");
    }

    /// 🔴 就绪位的三个谓词必须与**各自的生产 SQL** 同款。
    ///
    /// 这条判的是耦合而不是「函数存在」：`nearest_visible` 现在拿 `datasource` 这一位当
    /// 「要不要花那次 embed HTTP」的闸。谓词漂了（比如这里漏掉 `status = 'active'`）就会
    /// 出现「有一行 inactive 的源带着旧向量 ⇒ 就绪位说通了 ⇒ embed 照发 ⇒ 选源照旧返空」，
    /// 也就是这次修复被静默还原。故直接跨文件比对那两条生产 SQL 的谓词文本。
    #[test]
    fn vector_ready_matches_the_production_predicate() {
        const ACTIVE_PRED: &str = "status = 'active' AND embedding IS NOT NULL";
        // 便宜是这条体检存在的全部理由：退化成 count(*) / SELECT * 就没意义了
        assert_eq!(
            VECTOR_READY_SQL.matches("EXISTS(SELECT 1 FROM meta.").count(),
            3,
            "三个位少了：{VECTOR_READY_SQL}"
        );
        assert!(!VECTOR_READY_SQL.contains("count("), "别退化成 count：{VECTOR_READY_SQL}");
        assert_eq!(
            VECTOR_READY_SQL.matches(ACTIVE_PRED).count(),
            2,
            "element 与 datasource 两位都必须带 status 过滤：{VECTOR_READY_SQL}"
        );
        // 生产那两条 SQL（选源 / 元素召回）必须真的长这样 —— 否则本判据比的是个已经不存在的口径
        for (what, src, pred) in [
            ("registry::datasource::nearest_datasources", include_str!("registry/datasource.rs"), ACTIVE_PRED),
            // recall_elements 的谓词带 `e.` 别名（那条查询 JOIN 了 metric/dimension 副表）
            (
                "recall::cards::recall_elements",
                include_str!("recall/cards.rs"),
                "e.status = 'active' AND e.embedding IS NOT NULL",
            ),
        ] {
            assert!(src.contains(pred), "{what} 的谓词变了 —— 就绪位要跟着改");
        }
        // table_doc 那条没有 status 列，只判非空（`vector_tables` 的谓词逐字同款；
        // A20 之后前面多了 `enabled AND` —— 锚只咬 `embedding IS NOT NULL`，别咬整句）
        assert!(include_str!("recall/schema.rs").contains("embedding IS NOT NULL"));
    }

    /// 主键必须**前置** ds_id：写在后面等于「同名表在两个源里撞车」照样发生
    #[test]
    fn ds_pk_is_prefixed() {
        assert_eq!(DS_PKS.len(), 10);
        for (t, cols) in DS_PKS {
            assert!(cols.starts_with("ds_id"), "{t} 的主键没有前置 ds_id: {cols}");
        }
        // 「已迁移」判定必须验前置：contains 会把 (table_name, ds_id) 误判已迁移
        assert!(pk_prefixed_with_ds("PRIMARY KEY (ds_id, table_name)"));
        assert!(!pk_prefixed_with_ds("PRIMARY KEY (table_name, ds_id)"));
        // 建表即带 ds_id 主键的三张（不走 rekey）：内联 PRIMARY KEY 必须以 ds_id 开头
        for t in ["value_domain", "table_snapshot", "document_family"] {
            let needle = format!("meta.{t}(");
            let at = DDL.find(&needle).unwrap_or_else(|| panic!("{t} 建表语句不见了"));
            let body = &DDL[at..];
            let head = &body[..body.find(");").expect("{t} 建表语句缺收尾")];
            assert!(head.contains("PRIMARY KEY(ds_id"), "{t} 内联主键未前置 ds_id");
        }
        // 新库路径：table_doc 建表直接带 ds_id 复合主键（不再先 PK(table_name) 再 rekey）
        let at = DDL
            .find("CREATE TABLE IF NOT EXISTS meta.table_doc(")
            .expect("table_doc 建表语句不见了");
        let body = &DDL[at..];
        let head = &body[..body.find(");").expect("table_doc 建表语句缺收尾")];
        assert!(head.contains("PRIMARY KEY (ds_id, table_name)"), "table_doc 内联主键变了");
    }

    /// 🔴 DDL 注释行不许带半角分号（按分号逐句切执行：注释里的分号会把语句劈成两半，
    /// 服务与全部 CLI 启动即语法错误 —— 踩过一次）。`DO $$` 块同理不许出现。
    #[test]
    fn ddl_comment_lines_have_no_semicolons() {
        for (n, line) in DDL.lines().enumerate() {
            if let Some(pos) = line.find("--") {
                assert!(
                    !line[pos..].contains(';'),
                    "DDL 第 {} 行注释带半角分号：{line}",
                    n + 1
                );
            }
        }
        // DO 块同理不许出现（判的是行首语句形态；注释里的字面提及不算）
        assert!(
            !DDL.lines().any(|l| l.trim_start().starts_with("DO $$")),
            "DO 块会把切句劈碎"
        );
        // 版本回填哨兵锚点：migrate 按这个语句头跳过已收敛的回填
        assert!(
            DDL.contains("UPDATE meta.artifact a SET version"),
            "回填语句形态变了 —— migrate 的哨兵跳过锚点要同步"
        );
    }
}
