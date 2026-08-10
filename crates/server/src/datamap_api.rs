//! 【数据地图 API + SQL 审计查看】表/列目录节点、JOIN 边（合同 + 推断）、两级路径、
//! 推断边人工复核门、`query_log` 全状态审计。变更原因＝数据地图协议。
//!
//! 四条纪律：
//! 1. **权限 fail-closed 且内联**：①②③ 先 `resolve_identity`（401）再过 ds 级可见性
//!    （`ds_reg::visible_datasources` 的 SQL 是判据唯一实现，这里只做集合判定，不可见 403）；
//!    ④ 只认 `administrator_flag`（`admin_api::admin_only`，前端传的 `role_code` 不算）；
//!    ⑤ admin 看全量、非 admin 由 SQL 谓词强制本人（`($1 = '' OR login_name = $1)`，
//!    绑定值在 Rust 侧只能是 `""` 或**本人**登录名，请求里根本没有用户筛选参数）。
//! 2. **人工确认是推断边进合同的唯一门**：`meta.join_edge` 在本模块只有一条 INSERT
//!    （`ACCEPT_JOIN_SQL` 的 CTE 里），且它与「pending → accepted」的 CAS 是**同一条语句**
//!    （原子：边进合同 ⇔ 复核落账，没有「状态已改、注册表没写」的半截窗口）。reject 与
//!    其余路径在代码结构上够不到那条 INSERT（`join_edge_has_exactly_one_write_path` 钉着）。
//! 3. **全部只读除 ④**：①②③⑤ 全是 SELECT；④ 只写 `meta.datamap_edge` 与 `meta.join_edge`。
//! 4. **字面量通道**：所有 SQL 走 `OwnedStore::fixed(&'static str)`，值全走 bind；
//!    ds 谓词 `ds_id IN ($1, '*')` 逐字内联（与 `registry::DS_PRED` 同一形状 —— 那是运行期
//!    函数，拼不进 `'static` 字面量，故在这里钉同款文本，`ds_predicates_are_inlined` 守着）。
//!
//! ## 接线（已在 `main.rs` 落地）
//! 1. `mod datamap_api;`（无 allow）；2. `bootstrap_meta` 调 `datamap_api::migrate(pg)`
//!    （建 `meta.datamap_edge` + 老库 kind CHECK 拓值 `KIND_CHECK_WIDEN`，幂等）；3. Router 七条：`/api/datamap/{nodes,edges,paths,relations}`、
//!    `/api/datamap/edges/{id}/{accept,reject}`、`/api/audit/sql`。
//! 推断边生产侧：CLI 子命令 `meta datamap-build [ds]`（静态画像推断）与
//! `meta datamap-calibrate [days]`（使用轨迹校准），同 `meta autodiscover` 的管理员上下文。
//!
//! ## 端点契约
//! ### ① `GET /api/datamap/nodes?ds=&login_name=&role_code=`
//! 目录内表/列节点（`meta.table_doc` / `meta.column_doc`，ds 谓词内联）。敏感列
//! （`registry::is_sensitive_col`，与「不进 LLM schema」同一份词表）不进响应。
//! 注释人工优先（`custom_comment` 非空覆盖原生注释，与 `recall::schema` 同款口径）。
//! 响应：`{"ds", "nodes": [...]}`，节点两种：
//! `{"id":"table:t_a","kind":"table","table","comment","domain","row_estimate","enabled"}`
//! `{"id":"column:t_a.c1","kind":"column","table","column","data_type","comment"}`
//!
//! ### ② `GET /api/datamap/edges?ds=&kind=&status=&login_name=&role_code=`
//! 统一边列表，两种来源：注册表边（`meta.join_edge` 的 active 行，kind 恒 `join`，
//! `source="registry"`）与推断边（`meta.datamap_edge`，带 evidence/confidence 与复核轨迹，
//! `source="inferred"`）。status 过滤语义：缺省 = active 注册表边 ∪ pending 推断边
//! （地图 + 待审队列，accepted 边已从注册表侧可见，不重复出列）；`active` = 仅注册表边；
//! `pending/accepted/rejected` = 仅该状态推断边（复核账）。kind 七值闭集 {join, lineage,
//! joinable, synonym, distribution_similar, co_occurs, correlated}，缺省全部。
//! 其余取值一律 400（闭集，不静默忽略）。推断边最多回 500 条（LIMIT 内联），按 confidence 降序
//! —— 静态推断一轮上万条，复核队列必须让最强候选浮头，不能按入库先后淹掉。
//!
//! ### ③ `GET /api/datamap/paths?from=&to=&ds=&login_name=&role_code=`
//! 两级内最短路径：BFS 在内存图上，深度 ≤2 跳（`PATH_MAX_DEPTH`）；边取
//! `registry::model::load_join_edges`（与组合器同一加载口 —— 路径面就是可通行面，
//! pending/rejected 推断边天然不在其中）。边数 >500（`PATH_MAX_EDGES`）直接 422 护栏，
//! 不静默截断（截断会把「两级内不连通」与「没找全」搅在一起）。ds 缺省 `dms`。
//! from/to 允许带库名前缀（剥到裸表名、大小写不敏感）。`found=false` 是正常答案不是错误。
//!
//! ④ `POST /api/datamap/edges/{id}/accept` 与 `POST /api/datamap/edges/{id}/reject`（仅 admin）
//! body：`{"login_name"?, "role_code"?, "card"?, "note"?}`（reject 忽略 card/note）。
//! 状态机：只允许 pending → accepted|rejected，终态不回迁（409）。`reviewed_by` 取服务端
//! 认定身份（principal.login_name），不信请求自报。accept 且 kind='join'：边缺列信息 → 422
//! （进不了注册表的边不许落 accepted 假账）；否则走 `ACCEPT_JOIN_SQL`，ds 取自行内
//! （按 ds 作用域写注册表），ON CONFLICT 只补空白、不覆盖人工/种子已填的 card/note。
//! card ∈ {"", "1:N", "N:1", "1:1"}（"" = 基数未断言 —— 确认「可连」与断言「基数」是两件事）。
//! kind ∈ {join, joinable} 且双列齐 → 进合同；lineage/synonym/distribution_similar/co_occurs/
//! correlated 只落复核态，绝不进 `join_edge`（合同只有 join 一个入口）。
//!
//! ### ⑤ `GET /api/audit/sql?status=&limit=&login_name=&role_code=`
//! `query_log` 全状态审计：admin 全量、非 admin 强制本人（内联谓词，见纪律 1）。
//! status ∈ {succeeded, blocked, failed, timeout}（取值与 `dms_kernel::qalog::STATUS_*` 对齐，
//! 跨文件单测钉着；空 = 全部，含本列上线前的老行）。limit 缺省 100、clamp [1, 500]。
//! 响应行含 question/sql 全文（入库时已各截 2000 字，见 `query_log.rs`）。
//!
//! ### ⑥ `GET /api/datamap/relations?ds=&login_name=&role_code=`
//! 按表聚合的一站式关系卡：合同边（join_edge active）+ 血缘（lineage）+ 统计边 topN +
//! 共现边 topN。组装在 `semantic::lineage::table_relations`（纯 SELECT，窗口函数按表分桶），
//! 本端点只做身份与 ds 可见性（同 ①②③ 纪律）。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use crate::dms_policy::principal;
use dms_semantic::registry::datasource as ds_reg;
use dms_semantic::registry::model::{load_join_edges, JoinEdge};

use crate::AppState;

type ApiErr = (StatusCode, Json<serde_json::Value>);
type ApiOk = Json<serde_json::Value>;

/// 沿用现有 `{"error": msg}` 形状（前端只认这一种）
fn err(code: StatusCode, msg: impl std::fmt::Display) -> ApiErr {
    (code, Json(serde_json::json!({ "error": msg.to_string() })))
}

// ─────────────────────────── 推断边注册表（meta.datamap_edge）───────────────────────────
// 推断边的**唯一**生命周期表：pending（待人工复核）→ accepted / rejected（终态，不回迁）。
// 🔴 本表三处共用（本模块 = 正本 / semantic::datamap 静态推断 / semantic::datamap_usage
// 使用轨迹），DDL 文本三处逐字一致：CREATE IF NOT EXISTS 先跑者赢，不同构就是 race。
// 老库（六值 CHECK 时代建表）由 `KIND_CHECK_WIDEN` 在 migrate 里 DROP+ADD 拓成七值；
// 新库 CREATE 即七值，WIDEN 跑一轮等价重写（幂等无害）。
// idx_datamap_edge_uniq 是两个写入侧 ON CONFLICT 的仲裁唯一索引。
// join_edge 是「已确认合同」，本表是「待审队列 + 复核账」—— 两张表不合并的理由：
// 合同表的消费者是组合器/口径判据（要求干净、active 语义稳定），推断边带着 evidence/
// confidence/复核轨迹，混进一张表会让运行时加载口不得不认识复核字段。
// ⚠️ 与 ddl.rs 同款约束：按分号逐句切执行，故 `DO $$` 块与「注释里带半角分号」一律不许出现。
const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS meta.datamap_edge(
  id bigserial PRIMARY KEY,
  ds_id text NOT NULL DEFAULT 'dms',
  kind text NOT NULL DEFAULT 'join' CHECK (kind IN ('join','lineage','joinable','synonym','distribution_similar','co_occurs','correlated')),
  left_table text NOT NULL,
  left_col text NOT NULL DEFAULT '',
  right_table text NOT NULL,
  right_col text NOT NULL DEFAULT '',
  confidence real NOT NULL DEFAULT 0,
  evidence text NOT NULL DEFAULT '',
  status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','accepted','rejected')),
  reviewed_by text NOT NULL DEFAULT '',
  reviewed_at timestamptz,
  seen_count bigint NOT NULL DEFAULT 0,
  first_seen timestamptz NOT NULL DEFAULT now(),
  last_seen timestamptz NOT NULL DEFAULT now(),
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_datamap_edge_ds ON meta.datamap_edge(ds_id, status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_datamap_edge_uniq ON meta.datamap_edge(ds_id, kind, left_table, left_col, right_table, right_col);
"#;

/// 老库 kind CHECK 拓值（六值 → 七值，加 'correlated'）。`CREATE TABLE` 的内联 CHECK 只对
/// 新库生效；老库的约束文本是建表时落盘的六值，必须 DROP + ADD 重写。约束名 = PG 默认命名
/// `datamap_edge_kind_check`（dev 库 `pg_constraint` 实测，2026-08）。幂等：DROP IF EXISTS +
/// ADD 成对，每次启动都跑；只拓不收（新集合是旧集合的超集，既有行不可能违反）。
/// ⚠️ 与 DDL 同款执行形态（按分号逐句切）：两句顺序不可颠倒（先 DROP 后 ADD），
/// 不许出现 `DO $$` 块与带半角分号的注释。
const KIND_CHECK_WIDEN: &str = r#"
ALTER TABLE meta.datamap_edge DROP CONSTRAINT IF EXISTS datamap_edge_kind_check;
ALTER TABLE meta.datamap_edge ADD CONSTRAINT datamap_edge_kind_check CHECK (kind IN ('join','lineage','joinable','synonym','distribution_similar','co_occurs','correlated'));
"#;

/// 建表 + 老库 CHECK 拓值。与 `quality_api::migrate` 同风格（按分号逐句切，幂等）。
/// 接线：`bootstrap_meta` 补一行调用；`meta datamap-build` CLI 分支也补了一行 ——
/// CLI 不经过 bootstrap，老库上 correlated 边会被建表时的六值 CHECK 拒掉。
pub async fn migrate(pg: &sqlx::PgPool) -> anyhow::Result<()> {
    for stmt in DDL.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(pg).await?;
    }
    for stmt in KIND_CHECK_WIDEN.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(pg).await?;
    }
    Ok(())
}

// ─────────────────────────── SQL 字面量（ds 谓词内联）───────────────────────────
// ds 谓词形状 = `registry::DS_PRED`（`ds_id IN ($1, '*')`）：注册表行按源隔离，
// `'*'` 是跨源共享条目。这里是 server 侧 `'static` 字面量通道，拼不了那个运行期函数，
// 逐字内联同款文本（`ds_predicates_are_inlined` 盯着，漏一条即红）。

const TABLES_SQL: &str = "SELECT table_name, table_comment, custom_comment, domain, row_estimate, enabled \
     FROM meta.table_doc WHERE ds_id IN ($1, '*') ORDER BY table_name";

const COLUMNS_SQL: &str = "SELECT table_name, column_name, data_type, col_comment, custom_comment \
     FROM meta.column_doc WHERE ds_id IN ($1, '*') ORDER BY table_name, ordinal";

const REGISTRY_EDGES_SQL: &str = "SELECT left_table, left_col, right_table, right_col, card, note \
     FROM meta.join_edge WHERE ds_id IN ($1, '*') AND status = 'active' ORDER BY left_table, right_table";

const INFERRED_EDGES_SQL: &str = "SELECT id, kind, left_table, left_col, right_table, right_col, \
     confidence, evidence, status, reviewed_by, reviewed_at, created_at \
     FROM meta.datamap_edge WHERE ds_id IN ($1, '*') \
     AND status = ANY($2::text[]) AND kind = ANY($3::text[]) ORDER BY confidence DESC, id DESC LIMIT 500";

/// 按 bigserial id 取边（不带 ds 谓词：id 全局唯一；accept 写注册表时 ds 由行自己带出，
/// 不存在跨源写错 —— ds 取自 CTE 的 `RETURNING ds_id`，不是取自请求）。
const EDGE_BY_ID_SQL: &str =
    "SELECT kind, left_col, right_col, evidence, status FROM meta.datamap_edge WHERE id = $1";

/// accept(kind='join') 的**唯一**语句：复核 CAS 与进注册表在同一条 CTE 里（原子）。
/// $1=id, $2=reviewed_by, $3=card, $4=note。
/// ON CONFLICT 只补空白：种子/人工已填的 card/note 不被推断边确认覆盖（确认「可连」
/// 不等于改写既有基数断言）。RETURNING 一行 = CAS 成功且注册表已写；空 = 间隙里被并发改走。
const ACCEPT_JOIN_SQL: &str = "\
WITH upd AS (
  UPDATE meta.datamap_edge SET status = 'accepted', reviewed_by = $2, reviewed_at = now()
  WHERE id = $1 AND status = 'pending'
  RETURNING ds_id, left_table, left_col, right_table, right_col
)
INSERT INTO meta.join_edge(ds_id, left_table, left_col, right_table, right_col, card, note, status)
SELECT ds_id, left_table, left_col, right_table, right_col, $3, $4, 'active' FROM upd
ON CONFLICT (ds_id, left_table, left_col, right_table, right_col) DO UPDATE SET
  status = 'active',
  card = CASE WHEN meta.join_edge.card = '' THEN EXCLUDED.card ELSE meta.join_edge.card END,
  note = CASE WHEN meta.join_edge.note = '' THEN EXCLUDED.note ELSE meta.join_edge.note END
RETURNING ds_id";

/// accept(非 join 类型)：只落复核态，绝不进注册表（合同只有 join 一个入口）
const ACCEPT_PLAIN_SQL: &str = "UPDATE meta.datamap_edge \
     SET status = 'accepted', reviewed_by = $2, reviewed_at = now() \
     WHERE id = $1 AND status = 'pending'";

const REJECT_SQL: &str = "UPDATE meta.datamap_edge \
     SET status = 'rejected', reviewed_by = $2, reviewed_at = now() \
     WHERE id = $1 AND status = 'pending'";

/// ⑤ 的审计 SQL。`$1` 是 login 过滤：admin 绑 `''`（全量）、非 admin 绑**本人**登录名 ——
/// 「只能看自己」靠谓词内联保证，不靠调用方自觉。`$2` = status（'' = 全部），`$3` = limit。
const AUDIT_SQL: &str = "SELECT id, at, login_name, ds_id, route, status, question, sql, \
     row_count, elapsed_ms, cache_hit, prompt_tokens, completion_tokens, llm_calls, trace_id, error, \
     context_summary \
     FROM meta.query_log \
     WHERE ($1 = '' OR login_name = $1) AND ($2 = '' OR status = $2) \
     ORDER BY id DESC LIMIT $3";

// ─────────────────────────── 纯函数（判据核心，全部不连库可测）───────────────────────────

/// 路径护栏：图边数上限。超过即 422（MCP 侧映 -32000）—— 静默截断会把「找不到」与「没找全」搅在一起。
pub(crate) const PATH_MAX_EDGES: usize = 500;
/// 「两级内」= 最多 2 跳
const PATH_MAX_DEPTH: usize = 2;

pub(crate) fn within_edge_budget(n: usize) -> bool {
    n <= PATH_MAX_EDGES
}

/// 表名归一：剥库名前缀与引号、小写（`join_edge` 种子是裸小写表名，路径参数允许带库名）
fn bare_table(t: &str) -> String {
    t.trim()
        .trim_matches(|c| matches!(c, '`' | '"'))
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .trim_matches(|c| matches!(c, '`' | '"'))
        .to_lowercase()
}

/// 一跳：边按**存储方向**原样透出（card 是 left→right 的基数断言），`forward` 记本跳走向。
#[derive(Debug, Clone, PartialEq)]
struct PathHop {
    left_table: String,
    left_col: String,
    right_table: String,
    right_col: String,
    card: String,
    forward: bool,
}

/// BFS 最短路径（≤`PATH_MAX_DEPTH` 跳，无向走边）。`from == to`（归一后）→ `Some(空)`。
/// 纯函数：不连库，单测直接喂边。两级深度让复杂度天然有界（O(E)），500 边护栏管的是响应体积。
fn shortest_path(from: &str, to: &str, edges: &[JoinEdge]) -> Option<Vec<PathHop>> {
    let from = bare_table(from);
    let to = bare_table(to);
    if from.is_empty() || to.is_empty() {
        return None;
    }
    if from == to {
        return Some(vec![]);
    }
    // 邻接：裸名 → [(对侧裸名, 边下标, 是否沿 left→right)]
    let mut adj: std::collections::HashMap<String, Vec<(String, usize, bool)>> =
        std::collections::HashMap::new();
    for (i, e) in edges.iter().enumerate() {
        let l = bare_table(&e.lt);
        let r = bare_table(&e.rt);
        if l.is_empty() || r.is_empty() {
            continue;
        }
        adj.entry(l.clone()).or_default().push((r.clone(), i, true));
        adj.entry(r).or_default().push((l, i, false));
    }
    let mut visited = std::collections::HashSet::from([from.clone()]);
    let mut queue =
        std::collections::VecDeque::from([(from, Vec::<(usize, bool)>::new())]);
    while let Some((cur, path)) = queue.pop_front() {
        if path.len() >= PATH_MAX_DEPTH {
            continue;
        }
        for (next, ei, fwd) in adj.get(&cur).into_iter().flatten() {
            if visited.contains(next) {
                continue;
            }
            let mut p = path.clone();
            p.push((*ei, *fwd));
            if *next == to {
                return Some(
                    p.into_iter()
                        .map(|(i, f)| {
                            let e = &edges[i];
                            PathHop {
                                left_table: e.lt.clone(),
                                left_col: e.lc.clone(),
                                right_table: e.rt.clone(),
                                right_col: e.rc.clone(),
                                card: e.card.clone(),
                                forward: f,
                            }
                        })
                        .collect(),
                );
            }
            visited.insert(next.clone());
            queue.push_back((next.clone(), p));
        }
    }
    None
}

/// 路径经过的节点序列（从 from 起，逐跳取对侧裸名）
fn path_nodes(from: &str, hops: &[PathHop]) -> Vec<String> {
    let mut out = vec![bare_table(from)];
    for h in hops {
        out.push(bare_table(if h.forward { &h.right_table } else { &h.left_table }));
    }
    out
}

/// 复核动作（路由两条：accept / reject）
#[derive(Debug, Clone, Copy, PartialEq)]
enum ReviewAction {
    Accept,
    Reject,
}

impl ReviewAction {
    fn target(self) -> &'static str {
        match self {
            Self::Accept => "accepted",
            Self::Reject => "rejected",
        }
    }
}

/// 复核状态机：只允许 pending → accepted|rejected，终态不回迁。
/// 人工确认是推断边进合同的唯一门 —— accepted 边已从 accept 那条 CTE 进注册表，
/// 若再允许 accepted → rejected，注册表里的行就成了「被否决却仍在合同里」的账外状态。
fn review_transition(current: &str, action: ReviewAction) -> Result<&'static str, String> {
    match current {
        "pending" => Ok(action.target()),
        s => Err(format!("推断边当前状态是 {s}，只有 pending 能复核（终态不回迁）")),
    }
}

/// joinable = 能进 `meta.join_edge`（注册表主键含双列，缺列的「join 边」写不进去）。
/// 进合同的边：kind ∈ {join, joinable}（人工登记的合同边 + DataLink 静态推断的可连边）
/// 且双列齐。lineage/synonym/distribution_similar/co_occurs/correlated 恒 false：
/// 只落复核态，绝不进合同。
fn joinable(kind: &str, left_col: &str, right_col: &str) -> bool {
    matches!(kind, "join" | "joinable")
        && !left_col.trim().is_empty()
        && !right_col.trim().is_empty()
}

/// accept 的 card 入参："" = 基数未断言（确认「可连」不断言「基数」）
fn valid_card(s: &str) -> bool {
    matches!(s, "" | "1:N" | "N:1" | "1:1")
}

/// ② 的 status 过滤 → (是否含注册表边, 推断边状态数组)。
/// 缺省 = active 注册表边 ∪ pending 推断边（accepted 边已从注册表侧可见，不重复出列）。
/// 注册表边恒 active：status=pending/accepted/rejected 时注册表侧恒不匹配，直接不查。
fn edge_status_filter(s: &str) -> Option<(bool, Vec<String>)> {
    match s.trim() {
        "" => Some((true, vec!["pending".to_string()])),
        "active" => Some((true, vec![])),
        x @ ("pending" | "accepted" | "rejected") => Some((false, vec![x.to_string()])),
        _ => None,
    }
}

/// ② 的 kind 过滤 → (是否含注册表边, 推断边 kind 数组)。注册表边恒 'join'。
/// 七值闭集与 DDL CHECK 逐字同源（`edge_filters_are_closed_sets` 钉着，拓值要四处同步：
/// 这里 / DDL 三处 / `KIND_CHECK_WIDEN` / 头注与错误文案）。
pub(crate) fn edge_kind_filter(s: &str) -> Option<(bool, Vec<String>)> {
    match s.trim() {
        "" => Some((
            true,
            ["join", "lineage", "joinable", "synonym", "distribution_similar", "co_occurs", "correlated"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )),
        "join" => Some((true, vec!["join".to_string()])),
        "lineage" => Some((false, vec!["lineage".to_string()])),
        "joinable" => Some((false, vec!["joinable".to_string()])),
        "synonym" => Some((false, vec!["synonym".to_string()])),
        "distribution_similar" => Some((false, vec!["distribution_similar".to_string()])),
        "co_occurs" => Some((false, vec!["co_occurs".to_string()])),
        "correlated" => Some((false, vec!["correlated".to_string()])),
        _ => None,
    }
}

/// ⑤ 的 login 过滤绑定值：admin → `""`（全量）；非 admin → **本人**登录名。
/// 请求里没有用户筛选参数 —— 非 admin 连「填别人的 login_name」的缝都没有。
fn audit_login_filter(admin: bool, login: &str) -> &str {
    if admin {
        ""
    } else {
        login
    }
}

/// ⑤ 的 status 白名单：取值与 `dms_kernel::qalog::STATUS_*` 对齐（跨文件单测钉着）。
/// 空 = 全部（含本列上线前的老行）。
fn valid_audit_status(s: &str) -> bool {
    s.is_empty() || matches!(s, "succeeded" | "blocked" | "failed" | "timeout")
}

fn audit_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(100).clamp(1, 500)
}

/// ds 入参白名单（与 `ds_api::valid_ds_id` 同一形状：它会成为错误文案与日志的一部分；
/// `pub(crate)` 是 MCP `datamap_*` 工具在用 —— 两侧同一个函数，不许各抄一份）
pub(crate) fn valid_ds(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 列节点可见性：敏感列不进响应（与「不进 LLM schema」同一份词表，单一事实源在 kernel）
fn column_node_visible(name: &str) -> bool {
    !dms_semantic::registry::is_sensitive_col(name)
}

/// 注释人工优先（`custom_comment` 非空覆盖原生注释，与 `recall::schema` 同款口径）
fn display_comment<'a>(custom: &'a str, native: &'a str) -> &'a str {
    let c = custom.trim();
    if c.is_empty() {
        native.trim()
    } else {
        c
    }
}

/// note 入库上限（字符，非字节）—— 与 `query_log` 截断同一纪律
fn clip_note(s: &str) -> String {
    s.chars().take(500).collect()
}

// ─────────────────────────── 身份与 ds 级 ACL（内联 fail-closed）───────────────────────────

/// 身份换算：Bearer 会话 token 优先，回退 login_name（与 `/api/ds` 同一个 `resolve_identity`）
async fn caller(
    st: &AppState,
    h: &HeaderMap,
    id: (&Option<String>, &Option<String>),
) -> Result<principal::Principal, ApiErr> {
    let (login, role) = crate::resolve_identity(st, h, id.0, id.1)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    principal::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(|e| err(StatusCode::FORBIDDEN, e))
}

/// ds 级可见性判据的唯一实现（REST `require_ds_visible` 与 MCP `datamap_*` 工具共用）：
/// 可见集合由 `visible_datasources` 在 SQL 内算（含 `kb.acl` 的 ds 级授权），
/// 这里只做集合判定，**不可能放宽**。
pub(crate) async fn ds_visible(
    st: &AppState,
    p: &principal::Principal,
    ds: &str,
) -> anyhow::Result<bool> {
    let visible = ds_reg::visible_datasources(st.owned.pool(), &p.login_name, &[p.role_code.clone()]).await?;
    Ok(visible.iter().any(|v| v == ds))
}

/// ds 级 ACL（REST 侧包装）：与问数链路同一句拒绝文案（HTTP 侧映 403；MCP 侧映 -32000，
/// 见 `mcp_api::require_ds` —— 两侧调的都是上面这一个 `ds_visible`）。
async fn require_ds_visible(
    st: &AppState,
    p: &principal::Principal,
    ds: &str,
) -> Result<(), ApiErr> {
    if !ds_visible(st, p, ds)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?
    {
        return Err(err(StatusCode::FORBIDDEN, format!("无权访问数据源 {ds}")));
    }
    Ok(())
}

// ─────────────────────────── ① 目录节点 ───────────────────────────

#[derive(serde::Deserialize, Default)]
pub struct NodesQuery {
    ds: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

type TableRow = (String, String, String, String, i64, bool);
type ColumnRow = (String, String, String, String, String);

/// ① 的取数层（REST handler 与 MCP `datamap_search_nodes` 共用，**两侧不许各抄一份 SQL**）：
/// 只取数 + 组装节点 JSON；身份与 ds 可见性判定是调用侧各自的职责（REST=Bearer/login，MCP=API key）。
pub(crate) async fn load_nodes(st: &AppState, ds: &str) -> anyhow::Result<Vec<serde_json::Value>> {
    let tables = st.owned.fixed(TABLES_SQL).bind(ds).fetch_all::<TableRow>().await?;
    let columns = st.owned.fixed(COLUMNS_SQL).bind(ds).fetch_all::<ColumnRow>().await?;
    let mut out = Vec::with_capacity(tables.len() + columns.len());
    for (name, comment, custom, domain, rows, enabled) in &tables {
        out.push(serde_json::json!({
            "id": format!("table:{name}"),
            "kind": "table",
            "table": name,
            "comment": display_comment(custom, comment),
            "domain": domain,
            "row_estimate": rows,
            "enabled": enabled,
        }));
    }
    for (table, column, data_type, comment, custom) in &columns {
        if !column_node_visible(column) {
            continue;
        }
        out.push(serde_json::json!({
            "id": format!("column:{table}.{column}"),
            "kind": "column",
            "table": table,
            "column": column,
            "data_type": data_type,
            "comment": display_comment(custom, comment),
        }));
    }
    Ok(out)
}

/// `GET /api/datamap/nodes` —— 目录内表/列节点（契约见文件头）
pub async fn nodes(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<NodesQuery>,
) -> Result<ApiOk, ApiErr> {
    let ds = q.ds.as_deref().map(str::trim).unwrap_or("");
    if !valid_ds(ds) {
        return Err(err(StatusCode::BAD_REQUEST, "ds 必填（字母数字与 _-，≤64 字符）"));
    }
    let p = caller(&st, &headers, (&q.login_name, &q.role_code)).await?;
    require_ds_visible(&st, &p, ds).await?;
    let out = load_nodes(&st, ds)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(serde_json::json!({ "ds": ds, "nodes": out })))
}

// ─────────────────────────── ② 边列表 ───────────────────────────

#[derive(serde::Deserialize, Default)]
pub struct EdgesQuery {
    ds: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

type RegistryEdgeRow = (String, String, String, String, String, String);
#[allow(clippy::type_complexity)]
type InferredEdgeRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    f32,
    String,
    String,
    String,
    Option<chrono::DateTime<chrono::Utc>>,
    chrono::DateTime<chrono::Utc>,
);

/// ② 推断边的取数层（REST 边列表与 MCP `datamap_list_pending_edges` 共用）：SQL 的
/// ORDER BY confidence DESC + LIMIT 500 内联是复核队列契约（最强候选浮头）；
/// 调用侧的更小限量在 Rust 侧截（语义 = 从最强候选里再截一段）。
pub(crate) async fn load_inferred_edges(
    st: &AppState,
    ds: &str,
    statuses: &[String],
    kinds: &[String],
) -> anyhow::Result<Vec<serde_json::Value>> {
    let rows = st
        .owned
        .fixed(INFERRED_EDGES_SQL)
        .bind(ds)
        .bind(statuses)
        .bind(kinds)
        .fetch_all::<InferredEdgeRow>()
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, kind, lt, lc, rt, rc, confidence, evidence, status, by, at, created) in rows {
        out.push(serde_json::json!({
            "source": "inferred", "id": id, "kind": kind, "status": status,
            "left_table": lt, "left_col": lc, "right_table": rt, "right_col": rc,
            "card": "", "note": "",
            "confidence": confidence as f64, "evidence": evidence,
            "reviewed_by": by,
            "reviewed_at": at.map(|t| t.to_rfc3339()),
            "created_at": created.to_rfc3339(),
        }));
    }
    Ok(out)
}

/// `GET /api/datamap/edges` —— 统一边列表（注册表合同边 + 推断边，契约见文件头）
pub async fn edges(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<EdgesQuery>,
) -> Result<ApiOk, ApiErr> {
    let ds = q.ds.as_deref().map(str::trim).unwrap_or("");
    if !valid_ds(ds) {
        return Err(err(StatusCode::BAD_REQUEST, "ds 必填（字母数字与 _-，≤64 字符）"));
    }
    let (reg_by_status, statuses) = edge_status_filter(q.status.as_deref().unwrap_or(""))
        .ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "status 只能是 active | pending | accepted | rejected（缺省 = active 合同边 ∪ pending 推断边）",
            )
        })?;
    let (reg_by_kind, kinds) = edge_kind_filter(q.kind.as_deref().unwrap_or(""))
        .ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "kind 只能是 join | lineage | joinable | synonym | distribution_similar | co_occurs | correlated",
            )
        })?;
    let p = caller(&st, &headers, (&q.login_name, &q.role_code)).await?;
    require_ds_visible(&st, &p, ds).await?;
    let mut out: Vec<serde_json::Value> = vec![];
    // 注册表合同边（active）：kind 恒 'join'，两个过滤器都可能把它排掉
    if reg_by_status && reg_by_kind {
        let rows = st
            .owned
            .fixed(REGISTRY_EDGES_SQL)
            .bind(ds)
            .fetch_all::<RegistryEdgeRow>()
            .await
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        for (lt, lc, rt, rc, card, note) in rows {
            out.push(serde_json::json!({
                "source": "registry", "id": null, "kind": "join", "status": "active",
                "left_table": lt, "left_col": lc, "right_table": rt, "right_col": rc,
                "card": card, "note": note,
                "confidence": null, "evidence": "",
                "reviewed_by": "", "reviewed_at": null, "created_at": null,
            }));
        }
    }
    // 推断边（带证据与置信度；accepted 的有复核轨迹）
    if !statuses.is_empty() {
        out.extend(
            load_inferred_edges(&st, ds, &statuses, &kinds)
                .await
                .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?,
        );
    }
    Ok(Json(serde_json::json!({ "ds": ds, "edges": out })))
}

// ─────────────────────────── ③ 两级路径 ───────────────────────────

#[derive(serde::Deserialize, Default)]
pub struct PathsQuery {
    from: Option<String>,
    to: Option<String>,
    ds: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// ③ 的判定 + 组装（纯函数，REST 与 MCP `datamap_find_paths` 共用，**两侧不许各抄一份**）：
/// BFS ≤2 跳在同一份合同边上跑，`found=false` 是正常答案。边数护栏在调用侧
/// （REST 映 422、MCP 映 -32000，文案逐字一致）。
pub(crate) fn paths_result_json(ds: &str, from: &str, to: &str, edges: &[JoinEdge]) -> serde_json::Value {
    let found = shortest_path(from, to, edges);
    let (hops, node_list, hop_json) = match &found {
        Some(hops) => {
            let nodes = path_nodes(from, hops);
            let js: Vec<serde_json::Value> = hops
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "left_table": h.left_table, "left_col": h.left_col,
                        "right_table": h.right_table, "right_col": h.right_col,
                        "card": h.card, "forward": h.forward,
                    })
                })
                .collect();
            (hops.len(), nodes, js)
        }
        None => (0, vec![], vec![]),
    };
    serde_json::json!({
        "ds": ds,
        "from": bare_table(from),
        "to": bare_table(to),
        "found": found.is_some(),
        "hops": hops,
        "nodes": node_list,
        "path": hop_json,
        "edges_considered": edges.len(),
    })
}

/// `GET /api/datamap/paths` —— 两级内最短路径（契约见文件头）
pub async fn paths(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<PathsQuery>,
) -> Result<ApiOk, ApiErr> {
    let from = q.from.as_deref().map(str::trim).unwrap_or("");
    let to = q.to.as_deref().map(str::trim).unwrap_or("");
    if from.is_empty() || to.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "from 与 to 均必填（表名，可带库名前缀）"));
    }
    let ds = q
        .ds
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(ds_reg::DMS_DS_ID);
    if !valid_ds(ds) {
        return Err(err(StatusCode::BAD_REQUEST, format!("ds 非法：{ds}")));
    }
    let p = caller(&st, &headers, (&q.login_name, &q.role_code)).await?;
    require_ds_visible(&st, &p, ds).await?;
    // 边取组合器同一加载口：路径面就是可通行面（liveness 谓词与 ds 作用域在那一处）
    let edges = load_join_edges(st.owned.pool(), ds)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if !within_edge_budget(edges.len()) {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("关联边 {} 条超过护栏 {}，路径查询被拒（先收窄目录再查）", edges.len(), PATH_MAX_EDGES),
        ));
    }
    Ok(Json(paths_result_json(ds, from, to, &edges)))
}

// ─────────────────────────── ④ 推断边复核（仅 admin，唯一写口）───────────────────────────

#[derive(serde::Deserialize)]
pub struct ReviewReq {
    login_name: Option<String>,
    role_code: Option<String>,
    card: Option<String>,
    note: Option<String>,
}

type EdgeRow = (String, String, String, String, String);

async fn load_edge(st: &AppState, id: i64) -> Result<Option<EdgeRow>, ApiErr> {
    st.owned
        .fixed(EDGE_BY_ID_SQL)
        .bind(id)
        .fetch_optional::<EdgeRow>()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// `POST /api/datamap/edges/{id}/accept` —— 人工确认（**推断边进合同的唯一门**）
pub async fn accept(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<ReviewReq>,
) -> Result<ApiOk, ApiErr> {
    let p = crate::admin_api::admin_only(&st, &headers, (&req.login_name, &req.role_code)).await?;
    let card = req.card.as_deref().map(str::trim).unwrap_or("");
    if !valid_card(card) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("card 只能是 1:N | N:1 | 1:1（留空 = 基数未断言）：{card}"),
        ));
    }
    let (kind, lc, rc, evidence, status) = load_edge(&st, id)
        .await?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("推断边 #{id} 不存在")))?;
    if let Err(msg) = review_transition(&status, ReviewAction::Accept) {
        return Err(err(StatusCode::CONFLICT, msg));
    }
    if matches!(kind.as_str(), "join" | "joinable") {
        if !joinable(&kind, &lc, &rc) {
            return Err(err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "join/joinable 边缺 left_col/right_col，进不了 join_edge 注册表 —— 不许落 accepted 假账",
            ));
        }
        let note = match req.note.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(n) => clip_note(n),
            None => clip_note(&format!(
                "人工确认自推断边 #{id}（复核人 {}）；证据：{evidence}",
                p.login_name
            )),
        };
        // CAS 与写注册表同一条 CTE（原子）：空返回 = 间隙里被并发复核改走
        let written = st
            .owned
            .fixed(ACCEPT_JOIN_SQL)
            .bind(id)
            .bind(&p.login_name)
            .bind(card)
            .bind(&note)
            .fetch_optional::<(String,)>()
            .await
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        if written.is_none() {
            return Err(err(
                StatusCode::CONFLICT,
                format!("推断边 #{id} 的状态刚被并发变更，请刷新后重试"),
            ));
        }
        return Ok(Json(serde_json::json!({
            "ok": true, "id": id, "status": "accepted",
            "join_edge_written": true, "reviewed_by": p.login_name,
        })));
    }
    // lineage/synonym/distribution_similar/co_occurs/correlated：只落复核态，绝不进 join_edge —— 合同只有 join 一个入口
    let n = st
        .owned
        .fixed(ACCEPT_PLAIN_SQL)
        .bind(id)
        .bind(&p.login_name)
        .execute()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if n == 0 {
        return Err(err(
            StatusCode::CONFLICT,
            format!("推断边 #{id} 的状态刚被并发变更，请刷新后重试"),
        ));
    }
    Ok(Json(serde_json::json!({
        "ok": true, "id": id, "status": "accepted",
        "join_edge_written": false, "reviewed_by": p.login_name,
    })))
}

/// `POST /api/datamap/edges/{id}/reject` —— 人工否决（只落复核态；不碰注册表）
pub async fn reject(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<ReviewReq>,
) -> Result<ApiOk, ApiErr> {
    let p = crate::admin_api::admin_only(&st, &headers, (&req.login_name, &req.role_code)).await?;
    let (_, _, _, _, status) = load_edge(&st, id)
        .await?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("推断边 #{id} 不存在")))?;
    if let Err(msg) = review_transition(&status, ReviewAction::Reject) {
        return Err(err(StatusCode::CONFLICT, msg));
    }
    let n = st
        .owned
        .fixed(REJECT_SQL)
        .bind(id)
        .bind(&p.login_name)
        .execute()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if n == 0 {
        return Err(err(
            StatusCode::CONFLICT,
            format!("推断边 #{id} 的状态刚被并发变更，请刷新后重试"),
        ));
    }
    Ok(Json(serde_json::json!({
        "ok": true, "id": id, "status": "rejected", "reviewed_by": p.login_name,
    })))
}

// ─────────────────────────── ⑤ SQL 全状态审计 ───────────────────────────

#[derive(serde::Deserialize, Default)]
pub struct AuditQuery {
    status: Option<String>,
    limit: Option<i64>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `audit_sql` 的行型。手写 `FromRow`（workspace 的 sqlx 没开 derive feature，不改 Cargo.toml
/// 是硬规则）：sqlx 的元组 `FromRow` 上限 16 元，本行 17 列（D7 加了 `context_summary`）。
/// 按名取列，与 `AUDIT_SQL` 的列清单一一对应。
struct AuditRow {
    id: i64,
    at: chrono::DateTime<chrono::Utc>,
    login_name: String,
    ds_id: String,
    route: String,
    status: String,
    question: String,
    sql: String,
    row_count: i32,
    elapsed_ms: i64,
    cache_hit: bool,
    prompt_tokens: i32,
    completion_tokens: i32,
    llm_calls: i32,
    trace_id: Option<String>,
    error: String,
    /// 【D7】AUDIT_SQL 末列：本轮上下文摘要的 JSON 文本（老行 = 默认空串）
    context_summary: String,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for AuditRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            at: row.try_get("at")?,
            login_name: row.try_get("login_name")?,
            ds_id: row.try_get("ds_id")?,
            route: row.try_get("route")?,
            status: row.try_get("status")?,
            question: row.try_get("question")?,
            sql: row.try_get("sql")?,
            row_count: row.try_get("row_count")?,
            elapsed_ms: row.try_get("elapsed_ms")?,
            cache_hit: row.try_get("cache_hit")?,
            prompt_tokens: row.try_get("prompt_tokens")?,
            completion_tokens: row.try_get("completion_tokens")?,
            llm_calls: row.try_get("llm_calls")?,
            trace_id: row.try_get("trace_id")?,
            error: row.try_get("error")?,
            context_summary: row.try_get("context_summary")?,
        })
    }
}

/// `GET /api/audit/sql` —— query_log 全状态审计（admin 全量 / 非 admin 强制本人）
pub async fn audit_sql(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> Result<ApiOk, ApiErr> {
    let p = caller(&st, &headers, (&q.login_name, &q.role_code)).await?;
    let admin = p.administrator_flag;
    let status = q.status.as_deref().map(str::trim).unwrap_or("");
    if !valid_audit_status(status) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("status 只能是 succeeded | blocked | failed | timeout：{status}"),
        ));
    }
    let limit = audit_limit(q.limit);
    let rows = st
        .owned
        .fixed(AUDIT_SQL)
        .bind(audit_login_filter(admin, &p.login_name))
        .bind(status)
        .bind(limit)
        .fetch_all::<AuditRow>()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let out: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            // 【D7】本轮实际进 prompt 的上下文摘要：列里是 JSON 文本，解析成对象透出；
            // 老行/无摘要 = 空串 → null（前端按空隐藏，不渲染那块）。
            let ctx_summary = serde_json::from_str::<serde_json::Value>(&r.context_summary)
                .unwrap_or(serde_json::Value::Null);
            serde_json::json!({
                "id": r.id, "at": r.at.to_rfc3339(), "login_name": r.login_name, "ds": r.ds_id,
                "route": r.route, "status": r.status, "question": r.question, "sql": r.sql,
                "row_count": r.row_count, "elapsed_ms": r.elapsed_ms, "cache_hit": r.cache_hit,
                "prompt_tokens": r.prompt_tokens, "completion_tokens": r.completion_tokens,
                "llm_calls": r.llm_calls, "trace_id": r.trace_id, "error": r.error,
                "context_summary": ctx_summary,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "rows": out,
        "count": out.len(),
        "limit": limit,
        "status": status,
        "viewer": p.login_name,
        "admin": admin,
    })))
}

// ─────────────────────────── ⑥ 表关系卡 ───────────────────────────

#[derive(serde::Deserialize, Default)]
pub struct RelationsQuery {
    ds: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `GET /api/datamap/relations?ds=&login_name=&role_code=` —— 按表聚合的一站式关系卡
/// （合同边 + 血缘 + 统计边 + 共现边）。组装全部在 `semantic::lineage::table_relations`
/// （纯 SELECT，列只到表/列名与证据元数据，不出任何行值），这里只做身份与 ds 可见性
/// 判定（与 nodes 同一纪律：401/403 fail-closed）。
pub async fn relations(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<RelationsQuery>,
) -> Result<ApiOk, ApiErr> {
    let ds = q.ds.as_deref().map(str::trim).unwrap_or("");
    if !valid_ds(ds) {
        return Err(err(StatusCode::BAD_REQUEST, "ds 必填（字母数字与 _-，≤64 字符）"));
    }
    let p = caller(&st, &headers, (&q.login_name, &q.role_code)).await?;
    require_ds_visible(&st, &p, ds).await?;
    let cards = dms_semantic::lineage::table_relations(st.owned.pool(), ds)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(cards))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(lt: &str, lc: &str, rt: &str, rc: &str) -> JoinEdge {
        JoinEdge {
            lt: lt.into(),
            lc: lc.into(),
            rt: rt.into(),
            rc: rc.into(),
            card: "N:1".into(),
        }
    }

    // ── ③ BFS ──

    /// 直连边 1 跳；路径节点序列与走向正确
    #[test]
    fn bfs_direct_edge_is_one_hop() {
        let es = vec![edge("t_a", "b_id", "t_b", "id")];
        let p = shortest_path("t_a", "t_b", &es).expect("直连必须可达");
        assert_eq!(p.len(), 1);
        assert!(p[0].forward);
        assert_eq!(p[0].left_table, "t_a");
        assert_eq!(path_nodes("t_a", &p), vec!["t_a".to_string(), "t_b".to_string()]);
    }

    /// 两级内：2 跳可达；边可逆走（右→左时 forward=false）
    #[test]
    fn bfs_two_hops_and_reverse_traversal() {
        let es = vec![edge("t_a", "b_id", "t_b", "id"), edge("t_c", "b_id", "t_b", "id")];
        let p = shortest_path("t_a", "t_c", &es).expect("2 跳必须可达");
        assert_eq!(p.len(), 2);
        assert!(p[0].forward, "第一跳沿 left→right");
        assert!(!p[1].forward, "第二跳逆着边走（t_b → t_c）");
        assert_eq!(
            path_nodes("t_a", &p),
            vec!["t_a".to_string(), "t_b".to_string(), "t_c".to_string()]
        );
    }

    /// 两级护栏：3 跳链不可达；2 跳部分仍可达
    #[test]
    fn bfs_respects_the_two_hop_limit() {
        let es = vec![
            edge("t_a", "x", "t_b", "x"),
            edge("t_b", "x", "t_c", "x"),
            edge("t_c", "x", "t_d", "x"),
        ];
        assert_eq!(PATH_MAX_DEPTH, 2, "契约是「两级内」");
        assert!(shortest_path("t_a", "t_c", &es).is_some(), "2 跳可达");
        assert!(shortest_path("t_a", "t_d", &es).is_none(), "3 跳超出两级必须拒");
    }

    /// BFS 必须是最短：直连（1 跳）优先于绕行（2 跳）
    #[test]
    fn bfs_returns_the_shortest_path() {
        let es = vec![
            edge("t_a", "x", "t_b", "x"),
            edge("t_b", "x", "t_c", "x"),
            edge("t_a", "y", "t_c", "y"),
        ];
        let p = shortest_path("t_a", "t_c", &es).unwrap();
        assert_eq!(p.len(), 1, "直连边必须赢过绕行");
        assert_eq!((p[0].left_col.as_str(), p[0].right_col.as_str()), ("y", "y"));
    }

    /// from==to（归一后）是 0 跳；库名前缀与大小写不影响归一
    #[test]
    fn bfs_same_table_is_zero_hops_and_names_normalize() {
        let es = vec![edge("t_a", "x", "t_b", "x")];
        assert_eq!(shortest_path("t_a", "t_a", &es), Some(vec![]));
        assert_eq!(shortest_path("sales_dw.t_a", "T_A", &es), Some(vec![]));
        assert_eq!(bare_table("sales_dw.t_b"), "t_b");
        assert_eq!(bare_table("`t_c`"), "t_c");
        assert_eq!(bare_table("sales_dw.`t_b`"), "t_b");
        assert_eq!(bare_table("  T_Order  "), "t_order");
        assert!(shortest_path("", "t_a", &es).is_none(), "空 from 拒");
        assert!(shortest_path("t_a", "t_z", &es).is_none(), "不连通报 None（found=false）");
    }

    /// 边数护栏：500 以内放行，超过即拒（不静默截断）
    #[test]
    fn edge_budget_guard_trips_above_500() {
        assert_eq!(PATH_MAX_EDGES, 500);
        assert!(within_edge_budget(500));
        assert!(!within_edge_budget(501));
    }

    // ── ④ 复核状态机与 join_edge 写入形状 ──

    /// 状态机：只允许 pending → accepted|rejected，终态不回迁；
    /// SQL 侧的 CAS 谓词必须与状态机同一形状（两处各写一份就会漂，故钉死）
    #[test]
    fn review_state_machine_matches_sql_cas() {
        assert_eq!(review_transition("pending", ReviewAction::Accept).unwrap(), "accepted");
        assert_eq!(review_transition("pending", ReviewAction::Reject).unwrap(), "rejected");
        for terminal in ["accepted", "rejected"] {
            assert!(review_transition(terminal, ReviewAction::Accept).is_err(), "{terminal} 不许再 accept");
            assert!(review_transition(terminal, ReviewAction::Reject).is_err(), "{terminal} 不许再 reject");
        }
        assert!(review_transition("deleted", ReviewAction::Accept).is_err(), "未知态一律拒");
        for sql in [ACCEPT_PLAIN_SQL, REJECT_SQL, ACCEPT_JOIN_SQL] {
            assert!(sql.contains("WHERE id = $1 AND status = 'pending'"), "CAS 起点必须是 pending：{sql}");
        }
        assert!(ACCEPT_PLAIN_SQL.contains("SET status = 'accepted'"));
        assert!(REJECT_SQL.contains("SET status = 'rejected'"));
    }

    /// join_edge 写入形状：列清单 = 注册表既有结构（ds 作用域在行内），冲突键与
    /// ddl.rs 的主键对得上（跨文件漂移守卫）；人工/种子已填的 card/note 不被覆盖
    #[test]
    fn accept_join_upsert_shape_matches_registry_pk() {
        let conflict = "ON CONFLICT (ds_id, left_table, left_col, right_table, right_col)";
        assert!(ACCEPT_JOIN_SQL.contains(conflict), "冲突键必须是注册表主键");
        let ddl = include_str!("../../semantic/src/ddl.rs");
        assert!(
            ddl.contains("(\"join_edge\", \"ds_id, left_table, left_col, right_table, right_col\")"),
            "join_edge 主键变了 —— upsert 的 ON CONFLICT 键要跟着改"
        );
        let insert_head = [
            "INSERT INTO meta.",
            "join_edge(ds_id, left_table, left_col, right_table, right_col, card, note, status)",
        ]
        .concat();
        assert!(ACCEPT_JOIN_SQL.contains(&insert_head), "写入列清单必须覆盖注册表全部业务列");
        assert!(
            ACCEPT_JOIN_SQL
                .contains("card = CASE WHEN meta.join_edge.card = '' THEN EXCLUDED.card ELSE meta.join_edge.card END"),
            "已填的 card 不许被推断边确认覆盖"
        );
        assert!(
            ACCEPT_JOIN_SQL
                .contains("note = CASE WHEN meta.join_edge.note = '' THEN EXCLUDED.note ELSE meta.join_edge.note END"),
            "已填的 note 不许被推断边确认覆盖"
        );
        // CAS 与写入同一条 CTE：accepted 落账 ⇔ 边进合同，无半截窗口
        assert!(ACCEPT_JOIN_SQL.contains("WITH upd AS ("));
        assert!(ACCEPT_JOIN_SQL.contains("SET status = 'accepted'"));
    }

    /// 🔴 人工确认是推断边进合同的**唯一门**：整个模块只有一条 join_edge INSERT。
    /// 针尖用拼接拼出来 —— 断言文本里再写一份完整字面量，这条计数就会数到自己（恒真坑）。
    #[test]
    fn join_edge_has_exactly_one_write_path() {
        let src = include_str!("datamap_api.rs");
        let needle = ["INSERT INTO meta.", "join_edge"].concat();
        assert_eq!(
            src.matches(&needle).count(),
            1,
            "join_edge 出现了第二个写入口 —— 人工确认门被绕过"
        );
    }

    /// joinable：kind ∈ {join, joinable} 且双列非空；缺列边与其余四类一律进不了注册表
    #[test]
    fn joinable_requires_join_kind_and_both_columns() {
        assert!(joinable("join", "a", "b"));
        assert!(joinable("joinable", "a", "b"), "静态推断的可连边验收后进合同");
        assert!(!joinable("join", "", "b"));
        assert!(!joinable("joinable", "a", " "));
        assert!(!joinable("lineage", "a", "b"), "血缘绝不进合同");
        assert!(!joinable("synonym", "a", "b"), "同义词只落复核态");
        assert!(!joinable("distribution_similar", "a", "b"));
        assert!(!joinable("co_occurs", "a", "b"), "使用轨迹只落复核态");
        assert!(!joinable("correlated", "a", "b"), "相关边只落复核态，绝不进合同");
    }

    /// card 白名单："" = 基数未断言；N:M 之类一律 400
    #[test]
    fn card_values_are_closed() {
        for ok in ["", "1:N", "N:1", "1:1"] {
            assert!(valid_card(ok), "{ok}");
        }
        for bad in ["N:M", "n:1", "1:2"] {
            assert!(!valid_card(bad), "{bad}");
        }
    }

    /// note 按字符截断（中文不被切成半个字）
    #[test]
    fn note_is_clipped_by_chars() {
        let long = "证".repeat(600);
        assert_eq!(clip_note(&long).chars().count(), 500);
    }

    // ── ② 边列表过滤语义 ──

    /// status/kind 过滤是闭集：合法映射 + 非法一律 None（400），不静默忽略
    #[test]
    fn edge_filters_are_closed_sets() {
        assert_eq!(edge_status_filter(""), Some((true, vec!["pending".to_string()])));
        assert_eq!(edge_status_filter("active"), Some((true, vec![])));
        assert_eq!(edge_status_filter("pending"), Some((false, vec!["pending".to_string()])));
        assert_eq!(edge_status_filter("accepted"), Some((false, vec!["accepted".to_string()])));
        assert_eq!(edge_status_filter("rejected"), Some((false, vec!["rejected".to_string()])));
        assert_eq!(edge_status_filter("deleted"), None);
        assert_eq!(
            edge_kind_filter(""),
            Some((
                true,
                ["join", "lineage", "joinable", "synonym", "distribution_similar", "co_occurs", "correlated"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
            ))
        );
        assert_eq!(edge_kind_filter("join"), Some((true, vec!["join".to_string()])));
        assert_eq!(edge_kind_filter("lineage"), Some((false, vec!["lineage".to_string()])));
        assert_eq!(edge_kind_filter("joinable"), Some((false, vec!["joinable".to_string()])));
        assert_eq!(edge_kind_filter("co_occurs"), Some((false, vec!["co_occurs".to_string()])));
        assert_eq!(edge_kind_filter("correlated"), Some((false, vec!["correlated".to_string()])));
        assert_eq!(edge_kind_filter("foreign_key"), None);
    }

    // ── ⑤ 审计权限分支 ──

    /// 非 admin 强制本人：绑定值只能是 ""（admin 全量）或本人登录名。
    /// 就算非 admin 的登录名恰好叫 admin，过滤也照常收紧（身份位来自 principal，不是串）。
    #[test]
    fn audit_login_filter_forces_self_for_non_admin() {
        assert_eq!(audit_login_filter(true, "zhangsan"), "");
        assert_eq!(audit_login_filter(false, "zhangsan"), "zhangsan");
        assert_eq!(audit_login_filter(false, "admin"), "admin");
    }

    /// status 白名单与 `qalog::STATUS_*` 常量对齐（跨文件漂移守卫；Y2 起常量迁 kernel，
    /// KB 落账与问数落账共用同一份），其余一律 400
    #[test]
    fn audit_status_whitelist_matches_query_log() {
        let ql = include_str!("../../kernel/src/qalog.rs");
        for s in ["succeeded", "blocked", "failed", "timeout"] {
            assert!(valid_audit_status(s), "{s}");
            assert!(ql.contains(&format!("&str = \"{s}\"")), "qalog 的 STATUS_* 取值变了：{s}");
        }
        assert!(valid_audit_status(""), "空 = 全部（含老行）");
        for bad in ["running", "SUCCESS", " pending "] {
            assert!(!valid_audit_status(bad), "{bad}");
        }
    }

    /// 「非 admin 强制本人」的谓词必须内联在 SQL 里（不靠调用方自觉）；limit 走 bind
    #[test]
    fn audit_sql_forces_self_filter_inline() {
        assert!(AUDIT_SQL.contains("($1 = '' OR login_name = $1)"), "强制本人谓词必须内联");
        assert!(AUDIT_SQL.contains("($2 = '' OR status = $2)"));
        assert!(AUDIT_SQL.contains("LIMIT $3"));
        assert!(AUDIT_SQL.contains("FROM meta.query_log"));
    }

    /// limit 缺省 100、clamp [1, 500]
    #[test]
    fn audit_limit_clamps_and_defaults() {
        assert_eq!(audit_limit(None), 100);
        assert_eq!(audit_limit(Some(0)), 1);
        assert_eq!(audit_limit(Some(-5)), 1);
        assert_eq!(audit_limit(Some(99999)), 500);
    }

    // ── 公共判据 ──

    /// ds 谓词必须逐字内联在每一条目录/边 SQL 里（与 `registry::DS_PRED` 同一形状；
    /// semantic 的漂移守卫扫不到 server 侧，这里守本模块自己的）
    #[test]
    fn ds_predicates_are_inlined() {
        for (name, sql) in [
            ("TABLES_SQL", TABLES_SQL),
            ("COLUMNS_SQL", COLUMNS_SQL),
            ("REGISTRY_EDGES_SQL", REGISTRY_EDGES_SQL),
            ("INFERRED_EDGES_SQL", INFERRED_EDGES_SQL),
        ] {
            assert!(sql.contains("ds_id IN ($1, '*')"), "{name} 丢了 ds 谓词");
        }
    }

    /// 建表 DDL 幂等（启动路径每次全跑）+ status/kind 取值闭集
    #[test]
    fn ddl_is_idempotent_and_domains_are_closed() {
        for stmt in DDL.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            assert!(stmt.contains("IF NOT EXISTS"), "非幂等语句: {stmt}");
        }
        assert!(DDL.contains("CHECK (status IN ('pending','accepted','rejected'))"));
        assert!(DDL.contains(
            "CHECK (kind IN ('join','lineage','joinable','synonym','distribution_similar','co_occurs','correlated'))"
        ));
        assert!(DDL.contains(
            "idx_datamap_edge_uniq ON meta.datamap_edge(ds_id, kind, left_table, left_col, right_table, right_col)"
        ), "ON CONFLICT 仲裁唯一索引丢了");
        assert!(DDL.contains("DEFAULT 'pending'"), "推断边落地即待审");
    }

    /// 老库 CHECK 拓值（`KIND_CHECK_WIDEN`）：幂等形态 + 与 DDL 内联 CHECK 逐字同源。
    /// 两条防漂：① DROP 必须在 ADD 前（顺序反了老约束删不掉，ADD 撞名直接失败）；
    /// ② ADD 的取值集合必须就是 DDL 那份 —— 两处各写一份七值清单，拓下一个值时只改一处就漂。
    #[test]
    fn kind_check_widen_is_idempotent_and_matches_ddl() {
        let stmts: Vec<&str> = KIND_CHECK_WIDEN.split(';').map(str::trim).filter(|s| !s.is_empty()).collect();
        assert_eq!(stmts.len(), 2, "DROP + ADD 恰好两句：{KIND_CHECK_WIDEN}");
        assert_eq!(
            stmts[0],
            "ALTER TABLE meta.datamap_edge DROP CONSTRAINT IF EXISTS datamap_edge_kind_check",
            "先 DROP（IF EXISTS 幂等）；约束名是 PG 默认命名，dev 库 pg_constraint 实测过"
        );
        assert!(stmts[1].starts_with("ALTER TABLE meta.datamap_edge ADD CONSTRAINT datamap_edge_kind_check"), "后 ADD：{}", stmts[1]);
        // ADD 的 CHECK 文本与 DDL 内联 CHECK 逐字一致（拓值只许同步扩，不许两处各写一份）
        let extract = |ddl: &str| {
            ddl.split("CHECK (kind IN (").nth(1)
                .and_then(|rest| rest.split("))").next())
                .map(|s| s.to_string())
        };
        assert_eq!(extract(DDL), extract(stmts[1]), "DDL 与 KIND_CHECK_WIDEN 的 kind 取值集合漂了");
        assert!(stmts[1].contains("'correlated'"), "拓值就是为了 correlated：{}", stmts[1]);
    }

    /// 敏感列不进节点响应（与「不进 LLM schema」同一份词表）
    #[test]
    fn sensitive_columns_never_become_nodes() {
        assert!(!column_node_visible("login_pwd"));
        assert!(!column_node_visible("api_token"));
        assert!(column_node_visible("customer_code"));
    }

    /// 注释人工优先：custom 非空覆盖原生，空则回落原生（与召回渲染同款口径）
    #[test]
    fn display_comment_prefers_custom_over_native() {
        assert_eq!(display_comment("人工注释", "原生注释"), "人工注释");
        assert_eq!(display_comment("", "原生注释"), "原生注释");
        assert_eq!(display_comment("  ", "原生注释"), "原生注释", "空白 custom 不算数");
    }

    /// ds 入参白名单（与 ds_api 同一形状）
    #[test]
    fn ds_param_is_allowlisted() {
        assert!(valid_ds("dms") && valid_ds("upload_1") && valid_ds("crm-pg"));
        let long = "x".repeat(65);
        for bad in ["", "a b", "a;DROP", long.as_str()] {
            assert!(!valid_ds(bad), "{bad}");
        }
    }
}
