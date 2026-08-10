//! 【B6】文档级知识图谱 HTTP 面：构建 / 进度 / 子图 / 统计。变更原因＝图谱协议。
//!
//! **零业务判定**：抽取 prompt、容错解析、实体归并、并发/退避全在 `dms_knowledge::kg`；
//! Cypher 拼接唯一收口在 `dms_connector::doc_graph`。本文件只做协议转换、身份换算，
//! 外加构建状态表 `meta.kb_graph_build`（server 侧已有 meta.query_log 同款先例，
//! knowledge 不碰 `meta.*` 的纪律不变）。
//!
//! ## 路由（父代理统一在 main.rs 注册；登记形态如下，勿直接抄进本文件）
//! ```text
//! mod kg_api;
//! .route("/api/kb/graph/build", post(kg_api::build))
//! .route("/api/kb/graph/status", get(kg_api::status))
//! .route("/api/kb/graph/subgraph", get(kg_api::subgraph))
//! .route("/api/kb/graph/stats", get(kg_api::stats))
//! // 【Y4】运营三件套（本包交付时**未注册**，编排方统一接）：
//! .route("/api/kb/graph/failed-chunks", get(kg_api::failed_chunks))
//! .route("/api/kb/graph/reset", post(kg_api::reset))
//! .route("/api/kb/graph/reconcile", post(kg_api::reconcile))
//! ```
//! 可选：`bootstrap_meta` 里加 `kg_api::migrate(pg).await?` —— 不加也能跑
//! （build/status 处理器自带幂等建表），加了只是启动期把表备好。
//!
//! ## 端点契约
//! - `POST /api/kb/graph/build`  body `{"space_id":"...", "login_name"?, "role_code"?}`
//!   → 200 `{"ok":true,"state":"building","space_id":"..."}`：后台 tokio 任务对空间内
//!   **当前 viewer 可见**且 enabled 的 chunk 做 LLM 抽取（并发 4，指数退避重试 2 次），
//!   写入 AGE 图 `kb_graph`（先清空该空间旧图）。403 无空间写权限；409 该空间正在构建
//!   （认领是单条 UPSERT … WHERE state<>'building'，并发与崩溃遗留都由它兜底）。
//! - `GET  /api/kb/graph/status?space_id=`（需空间读权限）
//!   → `{"state":"idle|building|done|failed","total":n,"done":n,"failed":n,
//!       "failed_samples":[{"doc_id","chunk_id","error"}](≤5 条),"error":"...","updated_at":"..."|null}`
//! - `GET  /api/kb/graph/subgraph?space_id=&limit=200&center=<实体id>`（limit 钳 1..=500）
//!   不带 center → 全空间 TOP 子图（节点按可见 chunk 提及次数降序）；
//!   带 center → 该实体的**一跳邻域**子图（前端双击/按钮「展开邻居」的数据源，
//!   零可见提及的对端实体 weight=0 也返回，防悬空边）。两种形态同一响应形状：
//!   `{"nodes":[{"id","name","label","weight"}],"edges":[{"src","dst","relation","weight"}]}`，
//!   节点 `label` 即抽取出的实体类型（前端类型着色 + 图例的数据源）。
//! - `GET  /api/kb/graph/stats?space_id=` → `{"entities":n,"relations":n,"docs":n}`
//!
//! ## 【Y4】运营三件套契约（均未注册，见上）
//! - `GET  /api/kb/graph/failed-chunks?space_id=&limit=50&offset=0`（需空间读权限，同 status；
//!   limit 钳 1..=200，offset 钳 ≤100000）
//!   → `{"state":"idle|building|done|failed","total":n,"offset":n,"limit":n,
//!       "items":[{"chunk_id","doc_id","ord","kind":"failed|pending","error":"..."|null}]}`
//!   「未入图块」= 构建口径（可见+enabled+已入库+生效期）− 图里已有的 Chunk 节点。
//!   抽取成功但零实体的块也有 Chunk 节点（write_chunk 恒先落节点），所以缺席 ⇔
//!   抽取/写图失败、或该块是最近一轮构建之后新增的。`kind` 区分两者：在最近一轮
//!   `failed_samples`（≤5，build 契约截断）里 = failed 且带 error；否则 = pending
//!   （新增块、或样本被截断的失败 —— 重建即收敛）。从未构建过的空间回 `state:"idle"`
//!   加全量 pending 清单（图全空 ⇔ 全部候选未入图）。
//! - `POST /api/kb/graph/reset`  body 同 build（需空间写权限）
//!   → 200 `{"ok":true,"space_id":"...","state":"idle"}`：`doc_graph::clear_space`
//!   （Chunk/Entity 双标签 DETACH DELETE，标签未建 = 空操作）+ 删除构建状态行
//!   （status 回落 idle 零值行）。**幂等**：连点两次结果一样。409：该空间正在构建
//!   （清图撞上后台构建会让进度计数对不上；要重来请先等构建结束或直接重新 build ——
//!   build 本身就先清图）。
//! - `POST /api/kb/graph/reconcile`  body `{"space_id","dry_run"?,"max_orphans"?,...身份}`
//!   （需空间写权限，同 build/regenerate 一档）
//!   → 200 `{"dry_run":bool,"graph_chunks":n,"alive_docs":n,"orphan_chunks":n,
//!       "orphan_chunk_ids":[...≤50 条样本],"dangling_entities":n,"dangling_entity_ids":[...≤50],
//!       "relations_from_orphans":n,"max_orphans":n,"over_threshold":bool,
//!       "deleted":{"relations":n,"chunks":n,"entities":n}}`
//!   文档删/禁/失效后的图修复：**孤儿 Chunk**（doc 已不「活着」—— 生命周期判据，
//!   刻意不看操作者 ACL，否则会把「自己无权、别人可见」的文档误判孤儿清掉别人的图）、
//!   它们的 MENTIONS、出自它们的 RELATION、**悬空实体**（每条提及都出自孤儿 chunk）。
//!   `dry_run` **默认 true 只算不删**，显式 `dry_run:false` 才真删（删除顺序：
//!   孤儿出处的 RELATION → 孤儿 Chunk（DETACH）→ 悬空实体（DETACH）；三步都幂等，
//!   重跑收敛到全零）。`max_orphans` 是执行闸（默认 1000，钳 1..=10000）：
//!   孤儿数超闸时 dry-run 照常报告（`over_threshold:true`），真删 **409 拒删** ——
//!   大面积孤儿通常意味着判据/数据出错，必须人工核对后放大闸值重跑。409：构建中。
//!
//! ACL：build 要空间写权限（构建改的是全空间共享的图 + 消耗 LLM 配额）；status 要空间
//! 读权限（计数会泄露空间规模）；subgraph/stats 不做空间级预检，可见文档过滤直接内联在
//! `dms_knowledge::kg::visible_doc_ids` 的 SQL 里（`visible_docs!()` 片段，现查现算），
//! 文档级授权的用户因此也能看到自己那部分的图 —— 撤权即不可见。
//! failed-chunks 同 status 走空间读权限；reset/reconcile 同 build 走空间写权限。

use crate::AppState;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use dms_knowledge::{acl, kg, Viewer};
use sqlx::Row;
use std::sync::Arc;

type ApiErr = (StatusCode, Json<serde_json::Value>);
type ApiOk = Json<serde_json::Value>;

/// 响应体沿用现有 `{"error": msg}` 形状（前端只认这一种）。
fn err(code: StatusCode, msg: impl std::fmt::Display) -> ApiErr {
    (code, Json(serde_json::json!({ "error": msg.to_string() })))
}

/// 身份换算与 `kb_api::viewer` 同一条链（resolve_identity → load_principal）。
/// 那份是 kb_api 模块私有的拿不出来；这里的几行只是接线，判定逻辑零复制。
async fn viewer(
    st: &AppState,
    headers: &HeaderMap,
    ln: &Option<String>,
    rc: &Option<String>,
) -> Result<Viewer, ApiErr> {
    let (login, role) = crate::resolve_identity(st, headers, ln, rc)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    let p = crate::auth::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(|_| err(StatusCode::FORBIDDEN, "当前 DMS 身份或角色不可用"))?;
    Ok(Viewer::new(p.login_name, vec![p.role_code]))
}

fn space_param(raw: Option<&str>) -> Result<&str, ApiErr> {
    let Some(id) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(err(StatusCode::BAD_REQUEST, "space_id 不能为空"));
    };
    Ok(id)
}

fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(200).clamp(1, 500)
}

/// 构建状态表。与 `query_log::migrate` 同风格：按分号逐句切，故 DDL 里不许出现
/// `DO $$` 与注释内分号。幂等，可重复执行。
const DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS meta;
CREATE TABLE IF NOT EXISTS meta.kb_graph_build(
  space_id text PRIMARY KEY,
  state text NOT NULL DEFAULT 'idle',
  total int NOT NULL DEFAULT 0,
  done int NOT NULL DEFAULT 0,
  failed int NOT NULL DEFAULT 0,
  failed_samples jsonb NOT NULL DEFAULT '[]'::jsonb,
  error text NOT NULL DEFAULT '',
  updated_at timestamptz NOT NULL DEFAULT now()
);
"#;

pub async fn migrate(pg: &sqlx::PgPool) -> anyhow::Result<()> {
    for stmt in DDL.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(pg).await?;
    }
    Ok(())
}

/// 重启收割：上次进程退出时仍 `building` 的行永远等不到终态落库（后台任务随进程死了），
/// 启动时统一标 `failed` —— 随之落入 CLAIM_SQL 的 `state<>'building'` 分支，可立即
/// 重新构建，不必等 30 分钟过期。只动 `state='building'` 的行；`updated_at` 落钟，
/// status 端点透出的就是收割时刻。
const REAP_SQL: &str =
    "UPDATE meta.kb_graph_build SET state='failed', error='服务重启中断', updated_at=now() \
     WHERE state='building'";

/// 服务启动收割被重启中断的图谱构建。幂等：无 building 行时影响 0 行。返回收割行数。
/// 表可能从没建过（build/status 处理器自带幂等建表），故先跑一次幂等 migrate 再收割。
pub async fn reap_interrupted(pg: &sqlx::PgPool) -> anyhow::Result<u64> {
    migrate(pg).await?;
    let n = sqlx::query(REAP_SQL).execute(pg).await?.rows_affected();
    if n > 0 {
        tracing::info!(reaped = n, "重启收割：被中断的图谱构建已标 failed");
    }
    Ok(n)
}

/// 构建权认领：单条 UPSERT。行在且 state='building' 且未过期 → UPDATE 不命中 →
/// RETURNING 空 → 409。30 分钟未更新的 building 视为崩溃遗留，允许接管
/// （2000 chunk × 并发 4 的正常构建远低于这个阈值）。
const CLAIM_SQL: &str = "\
INSERT INTO meta.kb_graph_build(space_id,state) VALUES($1,'building') \
ON CONFLICT (space_id) DO UPDATE SET state='building',total=0,done=0,failed=0,\
failed_samples='[]'::jsonb,error='',updated_at=now() \
WHERE meta.kb_graph_build.state<>'building' \
   OR meta.kb_graph_build.updated_at < now() - interval '30 minutes' \
RETURNING space_id";

#[derive(serde::Deserialize, Default)]
pub struct BuildReq {
    #[serde(default)]
    space_id: String,
    login_name: Option<String>,
    role_code: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct GraphQuery {
    space_id: Option<String>,
    limit: Option<usize>,
    /// 邻居展开的中心实体 id（仅 subgraph 用；status/stats 忽略）。
    center: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `POST /api/kb/graph/build` —— 认领成功后立即返回，构建在后台 tokio 任务里跑。
pub async fn build(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<BuildReq>,
) -> Result<ApiOk, ApiErr> {
    let v = viewer(&st, &headers, &req.login_name, &req.role_code).await?;
    let space_id = space_param(Some(req.space_id.as_str()))?.to_string();
    if !acl::space_writable(&st.owned, &v, &space_id)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用"))?
    {
        return Err(err(StatusCode::FORBIDDEN, format!("无权构建空间 {space_id} 的图谱")));
    }
    let pool = st.owned.pool().clone();
    migrate(&pool)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱状态表初始化失败"))?;
    let claimed: Option<String> = sqlx::query_scalar(CLAIM_SQL)
        .bind(&space_id)
        .fetch_optional(&pool)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱构建认领失败"))?;
    if claimed.is_none() {
        return Err(err(StatusCode::CONFLICT, "该空间图谱正在构建中，请稍后查询进度"));
    }
    let st2 = st.clone();
    let space = space_id.clone();
    tokio::spawn(async move {
        let progress = PgProgress { pool: st2.owned.pool().clone(), space_id: space.clone() };
        let out =
            kg::build_space(&st2.owned, st2.owned.pool(), &st2.llm, &v, &space, &progress).await;
        finish(st2.owned.pool(), &space, out).await;
    });
    Ok(Json(serde_json::json!({ "ok": true, "state": "building", "space_id": space_id })))
}

/// 构建终态落库。成功：done + 全量计数；失败：failed + error（保留最后一次进度计数，
/// 不归零 —— 「跑到一半挂了」与「一个没跑」是两种不同的运维现场）。
async fn finish(pool: &sqlx::PgPool, space_id: &str, out: Result<kg::BuildOutcome, dms_knowledge::KbError>) {
    let result = match out {
        Ok(o) => {
            let samples = serde_json::to_value(&o.failed_samples)
                .unwrap_or_else(|_| serde_json::json!([]));
            sqlx::query(
                "UPDATE meta.kb_graph_build SET state='done',total=$2,done=$3,failed=$4,\
                 failed_samples=$5,error='',updated_at=now() WHERE space_id=$1",
            )
            .bind(space_id)
            .bind(o.total as i32)
            .bind(o.done as i32)
            .bind(o.failed as i32)
            .bind(samples)
            .execute(pool)
            .await
        }
        Err(e) => {
            let msg: String = e.to_string().chars().take(300).collect();
            sqlx::query(
                "UPDATE meta.kb_graph_build SET state='failed',error=$2,updated_at=now() \
                 WHERE space_id=$1",
            )
            .bind(space_id)
            .bind(&msg)
            .execute(pool)
            .await
        }
    };
    if let Err(e) = result {
        tracing::warn!(space_id, error = %e, "图谱构建终态落库失败");
    }
}

/// 进度回报 → meta.kb_graph_build。落库失败只 warn 不打断构建：
/// 状态观测缺席不该把已经烧掉的 LLM 配额变成整轮失败。
struct PgProgress {
    pool: sqlx::PgPool,
    space_id: String,
}

impl kg::BuildProgress for PgProgress {
    fn report<'a>(
        &'a self,
        total: usize,
        done: usize,
        failed: usize,
        samples: &'a [kg::FailedSample],
    ) -> dms_kernel::BoxFut<'a, ()> {
        Box::pin(async move {
            let samples =
                serde_json::to_value(samples).unwrap_or_else(|_| serde_json::json!([]));
            if let Err(e) = sqlx::query(
                "UPDATE meta.kb_graph_build SET total=$2,done=$3,failed=$4,failed_samples=$5,\
                 updated_at=now() WHERE space_id=$1",
            )
            .bind(&self.space_id)
            .bind(total as i32)
            .bind(done as i32)
            .bind(failed as i32)
            .bind(samples)
            .execute(&self.pool)
            .await
            {
                tracing::warn!(space_id = %self.space_id, error = %e, "图谱构建进度落库失败");
            }
        })
    }
}

/// `GET /api/kb/graph/status?space_id=` —— 从未构建过的空间回 idle 零值行（不是 404）。
pub async fn status(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<GraphQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = viewer(&st, &headers, &q.login_name, &q.role_code).await?;
    let space_id = space_param(q.space_id.as_deref())?;
    if !acl::space_readable(&st.owned, &v, space_id)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用"))?
    {
        return Err(err(StatusCode::FORBIDDEN, format!("空间 {space_id} 不可见")));
    }
    let pool = st.owned.pool().clone();
    migrate(&pool)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱状态表初始化失败"))?;
    let row = sqlx::query(
        "SELECT state,total,done,failed,failed_samples,error,updated_at::text AS updated_at \
         FROM meta.kb_graph_build WHERE space_id=$1",
    )
    .bind(space_id)
    .fetch_optional(&pool)
    .await
    .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱状态查询失败"))?;
    let body = match row {
        Some(r) => serde_json::json!({
            "state": r.try_get::<String, _>("state").unwrap_or_default(),
            "total": r.try_get::<i32, _>("total").unwrap_or(0),
            "done": r.try_get::<i32, _>("done").unwrap_or(0),
            "failed": r.try_get::<i32, _>("failed").unwrap_or(0),
            "failed_samples": r.try_get::<serde_json::Value, _>("failed_samples")
                .unwrap_or_else(|_| serde_json::json!([])),
            "error": r.try_get::<String, _>("error").unwrap_or_default(),
            "updated_at": r.try_get::<String, _>("updated_at").ok(),
        }),
        None => serde_json::json!({
            "state": "idle", "total": 0, "done": 0, "failed": 0,
            "failed_samples": [], "error": "", "updated_at": null,
        }),
    };
    Ok(Json(body))
}

/// `GET /api/kb/graph/subgraph?space_id=&limit=200&center=<实体id>`。
/// 可见文档集合现查现算（ACL 内联在 kg::visible_doc_ids 的 SQL 里），撤权即不可见。
/// 带 center 时返回该实体的一跳邻域（前端双击/按钮展开邻居），不带时返回全空间
/// TOP 子图；两种形态同一响应形状、共用同一份可见集合。
pub async fn subgraph(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<GraphQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = viewer(&st, &headers, &q.login_name, &q.role_code).await?;
    let space_id = space_param(q.space_id.as_deref())?;
    let limit = clamp_limit(q.limit);
    let docs = kg::visible_doc_ids(&st.owned, &v, space_id)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用"))?;
    let center = q.center.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let sg = match center {
        Some(c) => dms_connector::doc_graph::neighborhood(
            st.owned.pool(),
            space_id,
            &docs,
            &[c.to_string()],
            limit,
        )
        .await,
        None => dms_connector::doc_graph::subgraph(st.owned.pool(), space_id, &docs, limit).await,
    }
    .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱查询暂时不可用"))?;
    Ok(Json(serde_json::json!({
        "nodes": sg.nodes.iter().map(|n| serde_json::json!({
            "id": n.id, "name": n.name, "label": n.label, "weight": n.weight,
        })).collect::<Vec<_>>(),
        "edges": sg.edges.iter().map(|e| serde_json::json!({
            "src": e.src, "dst": e.dst, "relation": e.relation, "weight": e.weight,
        })).collect::<Vec<_>>(),
    })))
}

/// `GET /api/kb/graph/stats?space_id=` —— 同样在可见文档集合上计数。
pub async fn stats(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<GraphQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = viewer(&st, &headers, &q.login_name, &q.role_code).await?;
    let space_id = space_param(q.space_id.as_deref())?;
    let docs = kg::visible_doc_ids(&st.owned, &v, space_id)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用"))?;
    let s = dms_connector::doc_graph::stats(st.owned.pool(), space_id, &docs)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱查询暂时不可用"))?;
    Ok(Json(serde_json::json!({
        "entities": s.entities, "relations": s.relations, "docs": s.docs,
    })))
}

// ==================== 【Y4】运营三件套（契约见文件头【Y4】节；均未注册 main.rs） ====================

/// reset/reconcile 共用的构建状态读取。building 撞上运维写一律 409：
/// 清图/修复与后台构建并发，进度计数与图内容都会两边对不上。
async fn build_state(pool: &sqlx::PgPool, space_id: &str) -> Result<Option<String>, ApiErr> {
    sqlx::query_scalar("SELECT state FROM meta.kb_graph_build WHERE space_id=$1")
        .bind(space_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱状态查询失败"))
}

/// failed-chunks 的分页钳制（契约：limit 默认 50、钳 1..=200；offset 默认 0、钳 ≤100000）。
fn clamp_page(limit: Option<usize>, offset: Option<usize>) -> (usize, usize) {
    (limit.unwrap_or(50).clamp(1, 200), offset.unwrap_or(0).min(100_000))
}

#[derive(serde::Deserialize, Default)]
pub struct FailedChunksQuery {
    space_id: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `GET /api/kb/graph/failed-chunks?space_id=&limit=50&offset=0` —— 未入图块清单。
/// 集合差在 Rust 侧做（kg::missing_from_graph，纯函数有单测）；error 文案的唯一来源是
/// 最近一轮构建的 failed_samples（≤5 条，build 契约截断），对不上样本的一律 kind=pending。
pub async fn failed_chunks(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<FailedChunksQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = viewer(&st, &headers, &q.login_name, &q.role_code).await?;
    let space_id = space_param(q.space_id.as_deref())?;
    if !acl::space_readable(&st.owned, &v, space_id)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用"))?
    {
        return Err(err(StatusCode::FORBIDDEN, format!("空间 {space_id} 不可见")));
    }
    let pool = st.owned.pool().clone();
    migrate(&pool)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱状态表初始化失败"))?;
    let row = sqlx::query("SELECT state, failed_samples FROM meta.kb_graph_build WHERE space_id=$1")
        .bind(space_id)
        .fetch_optional(&pool)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱状态查询失败"))?;
    let (state, samples) = match row {
        Some(r) => (
            r.try_get::<String, _>("state").unwrap_or_default(),
            r.try_get::<serde_json::Value, _>("failed_samples")
                .unwrap_or_else(|_| serde_json::json!([])),
        ),
        None => ("idle".to_string(), serde_json::json!([])),
    };
    let mut sample_map: std::collections::HashMap<(String, i64), String> =
        std::collections::HashMap::new();
    for s in samples.as_array().into_iter().flatten() {
        let doc_id = s["doc_id"].as_str().unwrap_or_default().to_string();
        let chunk_id = s["chunk_id"].as_i64().unwrap_or(-1);
        sample_map.insert((doc_id, chunk_id), s["error"].as_str().unwrap_or_default().to_string());
    }
    let eligible = kg::eligible_chunks(&st.owned, &v, space_id)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用"))?;
    let present: std::collections::HashSet<i64> =
        dms_connector::doc_graph::chunk_nodes(&pool, space_id)
            .await
            .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱查询暂时不可用"))?
            .into_iter()
            .map(|(_, chunk_id)| chunk_id)
            .collect();
    let missing = kg::missing_from_graph(&eligible, &present);
    let (limit, offset) = clamp_page(q.limit, q.offset);
    let total = missing.len();
    let items: Vec<serde_json::Value> = missing
        .iter()
        .skip(offset)
        .take(limit)
        .map(|(chunk_id, doc_id, ord)| {
            let error = sample_map.get(&(doc_id.clone(), *chunk_id));
            serde_json::json!({
                "chunk_id": chunk_id,
                "doc_id": doc_id,
                "ord": ord,
                "kind": if error.is_some() { "failed" } else { "pending" },
                "error": error,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "state": state, "total": total, "offset": offset, "limit": limit, "items": items,
    })))
}

/// `POST /api/kb/graph/reset` —— 按空间清空图谱（幂等；构建中 409）。请求体与 build 同形。
pub async fn reset(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<BuildReq>,
) -> Result<ApiOk, ApiErr> {
    let v = viewer(&st, &headers, &req.login_name, &req.role_code).await?;
    let space_id = space_param(Some(req.space_id.as_str()))?.to_string();
    if !acl::space_writable(&st.owned, &v, &space_id)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用"))?
    {
        return Err(err(StatusCode::FORBIDDEN, format!("无权清空空间 {space_id} 的图谱")));
    }
    let pool = st.owned.pool().clone();
    migrate(&pool)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱状态表初始化失败"))?;
    if build_state(&pool, &space_id).await?.as_deref() == Some("building") {
        return Err(err(StatusCode::CONFLICT, "该空间图谱正在构建中，构建结束后再清空"));
    }
    // 清图复用 build 前清库的同一个收口（Chunk/Entity 双标签 DETACH DELETE，标签未建 = 空操作）
    dms_connector::doc_graph::clear_space(&pool, &space_id)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱清空失败"))?;
    // 状态行一并删除：status 对缺行空间回 idle 零值行 —— 清空后不该再透出旧计数
    sqlx::query("DELETE FROM meta.kb_graph_build WHERE space_id=$1")
        .bind(&space_id)
        .execute(&pool)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱状态行清理失败"))?;
    tracing::info!(space_id, "图谱已按空间清空（reset）");
    Ok(Json(serde_json::json!({ "ok": true, "space_id": space_id, "state": "idle" })))
}

/// reconcile 的请求体。`dry_run` 缺省 = true（只算不删）—— 真删必须显式 `dry_run:false`。
#[derive(serde::Deserialize, Default)]
pub struct ReconcileReq {
    #[serde(default)]
    space_id: String,
    dry_run: Option<bool>,
    max_orphans: Option<usize>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// reconcile 执行闸的默认值与硬上限（契约：孤儿数超闸只许 dry-run，真删 409）。
const RECONCILE_MAX_ORPHANS: usize = 1000;
const RECONCILE_ORPHANS_CAP: usize = 10_000;
/// 响应里 id 样本条数（计数另给全量；样本只供人工核对，不是分页面）。
const ID_SAMPLE: usize = 50;

/// `POST /api/kb/graph/reconcile` —— 文档删/禁/失效后的图修复。
/// 编排：存活文档（kg::alive_doc_ids，生命周期判据无 ACL）→ 图 Chunk 全量
/// （doc_graph::chunk_nodes）→ 纯函数计划（kg::plan_reconcile，单测钉着）→
/// 悬空实体/孤儿边统计（doc_graph）→ dry_run=false 时按 ①孤儿边 ②孤儿 Chunk
/// ③悬空实体 的顺序真删（三步都幂等，重跑收敛全零）。
pub async fn reconcile(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ReconcileReq>,
) -> Result<ApiOk, ApiErr> {
    let v = viewer(&st, &headers, &req.login_name, &req.role_code).await?;
    let space_id = space_param(Some(req.space_id.as_str()))?.to_string();
    if !acl::space_writable(&st.owned, &v, &space_id)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用"))?
    {
        return Err(err(StatusCode::FORBIDDEN, format!("无权修复空间 {space_id} 的图谱")));
    }
    let pool = st.owned.pool().clone();
    migrate(&pool)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱状态表初始化失败"))?;
    if build_state(&pool, &space_id).await?.as_deref() == Some("building") {
        return Err(err(StatusCode::CONFLICT, "该空间图谱正在构建中，构建结束后再修复"));
    }
    let dry_run = req.dry_run.unwrap_or(true);
    let max_orphans = req
        .max_orphans
        .unwrap_or(RECONCILE_MAX_ORPHANS)
        .clamp(1, RECONCILE_ORPHANS_CAP);
    let alive: std::collections::HashSet<String> = kg::alive_doc_ids(&st.owned, &space_id)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "存活文档查询失败"))?
        .into_iter()
        .collect();
    let graph_chunks = dms_connector::doc_graph::chunk_nodes(&pool, &space_id)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱读取失败"))?;
    let plan = kg::plan_reconcile(&graph_chunks, &alive, max_orphans);
    let dangling = dms_connector::doc_graph::dangling_entities(&pool, &space_id, &plan.orphan_chunk_ids)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱读取失败"))?;
    let relations =
        dms_connector::doc_graph::relation_count_of_chunks(&pool, &space_id, &plan.orphan_chunk_ids)
            .await
            .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "图谱读取失败"))?;
    let mut deleted = serde_json::json!({ "relations": 0, "chunks": 0, "entities": 0 });
    if !dry_run {
        // 执行闸：超阈值一律拒删（此前一个字节都没动 —— 统计查询不改图）
        if plan.over_threshold {
            return Err(err(
                StatusCode::CONFLICT,
                format!(
                    "孤儿块 {} 超过执行闸 {}：未删任何东西；请核对 dry-run 清单后以更大的 max_orphans 重跑",
                    plan.orphan_chunk_ids.len(),
                    plan.max_orphans
                ),
            ));
        }
        dms_connector::doc_graph::delete_relations_of_chunks(&pool, &space_id, &plan.orphan_chunk_ids)
            .await
            .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "孤儿关系边清理失败"))?;
        dms_connector::doc_graph::delete_chunks(&pool, &space_id, &plan.orphan_chunk_ids)
            .await
            .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "孤儿块清理失败"))?;
        dms_connector::doc_graph::delete_entities(&pool, &space_id, &dangling)
            .await
            .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "悬空实体清理失败"))?;
        deleted = serde_json::json!({
            "relations": relations,
            "chunks": plan.orphan_chunk_ids.len(),
            "entities": dangling.len(),
        });
        tracing::info!(
            space_id,
            orphans = plan.orphan_chunk_ids.len(),
            dangling = dangling.len(),
            "图谱 reconcile 已执行"
        );
    }
    Ok(Json(serde_json::json!({
        "dry_run": dry_run,
        "space_id": space_id,
        "graph_chunks": plan.graph_chunks,
        "alive_docs": alive.len(),
        "orphan_chunks": plan.orphan_chunk_ids.len(),
        "orphan_chunk_ids": plan.orphan_chunk_ids.iter().take(ID_SAMPLE).collect::<Vec<_>>(),
        "dangling_entities": dangling.len(),
        "dangling_entity_ids": dangling.iter().take(ID_SAMPLE).collect::<Vec<_>>(),
        "relations_from_orphans": relations,
        "max_orphans": plan.max_orphans,
        "over_threshold": plan.over_threshold,
        "deleted": deleted,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_is_clamped() {
        assert_eq!(clamp_limit(None), 200);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(9999)), 500);
    }

    /// DDL 契约：幂等 + 按分号可切（migrate 与 query_log 同一个切分约定）。
    #[test]
    fn status_table_ddl_is_idempotent_and_split_safe() {
        let stmts: Vec<&str> = DDL.split(';').map(str::trim).filter(|s| !s.is_empty()).collect();
        assert_eq!(stmts.len(), 2, "{DDL}");
        assert!(DDL.contains("CREATE TABLE IF NOT EXISTS meta.kb_graph_build"));
        assert!(DDL.contains("failed_samples jsonb"), "失败样本列没了");
        assert!(!DDL.contains(concat!("DO ", "$$")), "DO $$ 会把按分号切分打爆");
    }

    /// 认领必须是单条 UPSERT（并发安全）且能接管崩溃遗留的 building 行。
    #[test]
    fn claim_is_atomic_upsert_with_stale_reclaim() {
        assert!(CLAIM_SQL.contains("ON CONFLICT (space_id) DO UPDATE"));
        assert!(CLAIM_SQL.contains("state<>'building'"));
        assert!(CLAIM_SQL.contains("interval '30 minutes'"), "崩溃遗留的 building 行要能接管");
        assert!(CLAIM_SQL.contains("RETURNING space_id"), "靠 RETURNING 空判 409，不能拆两问");
    }

    /// 🔴 ACL 分层锚点：build 走空间写权限、status 走空间读权限、subgraph/stats 的
    /// 可见文档过滤必须经 knowledge 那条内联 `visible_docs!()` 的 SQL（撤权即不可见）。
    #[test]
    fn acl_is_enforced_at_the_right_layers() {
        let src = include_str!("kg_api.rs");
        let build = src
            .split(concat!("pub async fn bu", "ild"))
            .nth(1)
            .expect("build 处理器不见了")
            .split("async fn finish")
            .next()
            .unwrap();
        assert!(build.contains("space_writable"), "build 必须过空间写权限");
        assert!(build.contains("tokio::spawn"), "构建必须是后台任务");
        let status = src
            .split(concat!("pub async fn st", "atus"))
            .nth(1)
            .expect("status 处理器不见了")
            .split(concat!("pub async fn sub", "graph"))
            .next()
            .unwrap();
        assert!(status.contains("space_readable"), "status 必须过空间读权限");
        assert!(status.contains("failed_samples"), "status 必须带失败样本（契约④）");
        for f in [concat!("pub async fn sub", "graph"), concat!("pub async fn st", "ats")] {
            let body = src.split(f).nth(1).unwrap_or_else(|| panic!("{f} 不见了"));
            let end = body.find("\n}\n").expect("处理器形状变了");
            let body = &body[..end];
            assert!(body.contains("kg::visible_doc_ids"), "{f} 丢了内联 ACL 的可见集合查询");
            assert!(body.contains("doc_graph::"), "{f} 的图查询必须走 doc_graph 收口");
        }
    }

    /// 邻居展开：center 参数必须路由到 `doc_graph::neighborhood`，且与全量子图共用同一份
    /// 现算的内联 ACL 可见集合（撤权即不可见对两种形态一致生效）。
    #[test]
    fn subgraph_center_expands_neighborhood_under_same_acl() {
        let src = include_str!("kg_api.rs");
        let body = src
            .split(concat!("pub async fn sub", "graph"))
            .nth(1)
            .expect("subgraph 处理器不见了")
            .split("\n}\n")
            .next()
            .expect("处理器形状变了");
        assert!(body.contains("q.center"), "subgraph 丢了 center 参数");
        assert!(body.contains("doc_graph::neighborhood"), "center 形态必须走 neighborhood 收口");
        assert!(body.contains("doc_graph::subgraph"), "默认形态必须保留全量 TOP 子图");
        assert_eq!(
            body.matches("kg::visible_doc_ids").count(),
            1,
            "可见集合只许现算一次、两种形态共用（多算一次就是多一份漂移面）"
        );
    }

    /// 进度回报必须落 failed_samples（契约④：失败样本可查）。
    #[test]
    fn progress_persists_failed_samples() {
        let src = include_str!("kg_api.rs");
        assert!(src.contains("failed_samples=$5"), "进度 UPDATE 丢了失败样本列");
        assert!(src.contains("impl kg::BuildProgress for PgProgress"));
    }

    /// 收割 SQL 形状锚点：只动 building 行；收割后能被 CLAIM_SQL 立即接管
    #[test]
    fn reap_sql_only_marks_building_rows_failed() {
        assert!(REAP_SQL.contains("UPDATE meta.kb_graph_build SET state='failed'"));
        assert!(REAP_SQL.contains("WHERE state='building'"),
                "收割只许动 building 行：idle/done/failed 不许回碰");
        assert!(REAP_SQL.contains("error='服务重启中断'"), "中断原因文案变了，前端/运维按它辨认重启遗留");
        assert!(REAP_SQL.contains("updated_at=now()"), "status 端点的 updated_at 要透出收割时刻");
        // 标 failed 后落入 CLAIM_SQL 的 state<>'building' 分支：可立即重新构建，不必等 30 分钟过期
        assert!(CLAIM_SQL.contains("state<>'building'"), "认领 SQL 变了，收割后的行可能接管不回来");
    }

    // ==================== 【Y4】运营三件套 ====================

    /// failed-chunks 分页钳制：limit 默认 50 钳 1..=200；offset 钳 ≤100000。
    #[test]
    fn failed_chunks_page_is_clamped() {
        assert_eq!(clamp_page(None, None), (50, 0));
        assert_eq!(clamp_page(Some(0), Some(3)), (1, 3));
        assert_eq!(clamp_page(Some(9999), None), (200, 0));
        assert_eq!(clamp_page(None, Some(999_999)), (50, 100_000));
    }

    /// 🔴 Y4 三件套的 ACL 分层：failed-chunks 走空间读（同 status），
    /// reset/reconcile 走空间写（同 build/regenerate）；两个写端点都必须有构建中 409 闸。
    #[test]
    fn y4_acl_is_enforced_at_the_right_layers() {
        let src = include_str!("kg_api.rs");
        let failed = src
            .split("pub async fn failed_chunks(")
            .nth(1)
            .expect("failed_chunks 处理器不见了")
            .split("pub async fn reset(")
            .next()
            .unwrap();
        assert!(failed.contains("space_readable"), "failed-chunks 必须过空间读权限（同 status）");
        assert!(failed.contains("kg::eligible_chunks"), "候选必须走构建口径那条 SQL（knowledge 收口）");
        assert!(failed.contains("doc_graph::chunk_nodes"), "在图集合必须走 doc_graph 收口");
        assert!(failed.contains("kg::missing_from_graph"), "集合差必须是那个纯函数（单测钉着）");
        for f in ["pub async fn reset(", "pub async fn reconcile("] {
            let body = src.split(f).nth(1).unwrap_or_else(|| panic!("{f} 不见了"));
            let end = body.find("\n}\n").expect("处理器形状变了");
            let body = &body[..end];
            assert!(body.contains("space_writable"), "{f} 必须过空间写权限（同 build/regenerate）");
            assert!(body.contains("Some(\"building\")"), "{f} 丢了构建中 409 闸");
        }
    }

    /// reset：清图必须复用 build 前清库的同一个收口（clear_space），且状态行一并删除
    /// （不删的话 status 会透出一张已被清空的图的旧计数）。
    #[test]
    fn reset_clears_graph_and_status_row() {
        let src = include_str!("kg_api.rs");
        let body = src.split("pub async fn reset(").nth(1).expect("reset 不见了");
        let body = body.split("\n}\n").next().unwrap();
        assert!(body.contains("doc_graph::clear_space"), "清图必须走 doc_graph 收口，不许另拼");
        assert!(body.contains("DELETE FROM meta.kb_graph_build WHERE space_id=$1"), "状态行必须一并删除");
    }

    /// reconcile 的三条铁律锚点：dry-run 默认开；执行闸拒删必须发生在任何 DELETE 之前；
    /// 真删只许走 doc_graph 三件套（孤儿边 → 孤儿 Chunk → 悬空实体，顺序不许换）。
    #[test]
    fn reconcile_is_dry_run_first_and_gated() {
        let src = include_str!("kg_api.rs");
        let body = src
            .split("pub async fn reconcile(")
            .nth(1)
            .expect("reconcile 不见了")
            .split("\n#[cfg(test)]")
            .next()
            .unwrap();
        assert!(body.contains("req.dry_run.unwrap_or(true)"), "dry_run 必须默认 true（只算不删）");
        assert!(body.contains("kg::plan_reconcile"), "清理计划必须是那个纯函数（单测钉着）");
        assert!(body.contains("kg::alive_doc_ids"), "孤儿判据必须是无 ACL 的生命周期口径");
        let gate = body.find("plan.over_threshold").expect("执行闸不见了");
        let first_delete = body.find("doc_graph::delete_").expect("真删路径不见了");
        assert!(gate < first_delete, "执行闸拒删必须先于任何 DELETE：{body}");
        let rel = body.find("delete_relations_of_chunks").unwrap();
        let chunk = body.find("delete_chunks").unwrap();
        let ent = body.find("delete_entities").unwrap();
        assert!(rel < chunk && chunk < ent, "删除顺序必须是 边→Chunk→实体");
    }
}
