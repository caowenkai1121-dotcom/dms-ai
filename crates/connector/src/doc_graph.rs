//! 文档级知识图谱的 AGE 读写（图名 `kb_graph`）—— `kb_graph` 的 Cypher 拼接**唯一收口**。
//! 变更原因＝图谱 schema（节点/边标签与属性）。
//!
//! 与 `graph.rs` 同一条纪律：Cypher 串带动态量进不去 `fixed(&'static str)` 通道，
//! 故本文件沿用 `sqlx::query` + `esc()` 内联（`esc`/`unquote`/`age_conn` 与 graph.rs
//! 逐字同款 —— 那边是私有函数拿不出来，「转义只有一个实现」靠两侧同文 + 源码判据守住）。
//!
//! 图模型（Yuxi B6 节落到 AGE，不引 Neo4j）：
//! - 节点 `Entity {id, space_id, name, label}`：`id` 是调用方算好的确定性散列
//!   （hash(space:规范名:label)），MERGE 按 id —— 同名同 label 实体天然归并；
//! - 节点 `Chunk {doc_id, chunk_id, space_id}`：chunk 粒度的出处。契约只要求
//!   doc_id/chunk_id 两个属性；`space_id` 是清库与过滤的落点 —— 没有它，重建前清空
//!   一个空间得先回关系库查该空间的全部文档清单，多一次本可省掉的跨层往返；
//! - 边 `MENTIONS`（Chunk→Entity，MERGE）与 `RELATION`（Entity→Entity，CREATE，
//!   属性带 relation/space_id/doc_id/chunk_id 出处）。RELATION 按 chunk CREATE 而不
//!   MERGE：查询侧 `count(*)` 就是「被多少处文本支持」，weight 不需要写侧维护。
//!
//! ACL 不在本层：可见 doc 集合由 knowledge 用 `visible_docs!()` 在**查询那一刻**算好，
//! 作为 `doc_ids` 传进来；本层只负责把它们 esc 后内联进 Cypher（撤权即不可见）。
//!
//! 召回读侧（Yuxi B6 检索半场：实体种子 → 1~2 hop 扩散 → PPR 的取数原语）：
//! `entities_of_chunks` / `entities_named_like` / `relation_edges_touching` /
//! `mentioned_chunks` / `mention_pairs` / `space_has_chunks`，全部沿用同一条 doc 内联纪律。
//! 唯一的例外是 `entities_named_like` 的问句：它走外层 SQL 的**绑定参数**（`$1`/`$2`），
//! 一个字节都不进字符串拼接 —— 比 esc 内联更硬，也是本文件第一个带 bind 的查询。
//!
//! 邻居展开读侧（前端双击/按钮拉下一跳子图）：`neighborhood`，同样 doc 内联；
//! 零可见提及的对端实体 weight=0 也返回，防前端悬空边。
//!
//! 运营侧（Y4：failed-chunks / reset / reconcile）：`chunk_nodes`（空间 Chunk 全量）、
//! `dangling_entities`（提及全出自孤儿 chunk 的实体）、`relation_count_of_chunks`
//! 与 `delete_relations_of_chunks` / `delete_chunks` / `delete_entities` 真删三件套。
//! reset 复用既有的 `clear_space`。这层不做 ACL：权限判定在调用方（server 的空间写权限），
//! 输入清单一律视为已授权事实；空输入短路、删空集 = 空操作（幂等）。

use sqlx::{PgPool, Row};

/// 与 `dms_graph`（销售关系图）物理隔离：两张图的重建/清库互不影响。
const GRAPH: &str = "kb_graph";

/// 子图返回规模的硬上限（双保险；HTTP 侧另有 500 的钳制）。
const MAX_LIMIT: usize = 1000;

/// 一个实体节点。`id` 由调用方给（确定性散列），本层不感知散列算法。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEntity {
    pub id: String,
    pub name: String,
    pub label: String,
}

/// 一条关系边（一次抽取一条；`relation` 应是已规范化的关系类型文本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRelation {
    pub src: String,
    pub dst: String,
    pub relation: String,
}

/// 一个 chunk 抽取结果的图投影 —— 写库的唯一输入单位。
#[derive(Debug, Clone)]
pub struct ChunkGraph {
    pub space_id: String,
    pub doc_id: String,
    pub chunk_id: i64,
    pub entities: Vec<GraphEntity>,
    pub relations: Vec<GraphRelation>,
}

/// 子图节点：weight = 可见 chunk 的 MENTIONS 边数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubNode {
    pub id: String,
    pub name: String,
    pub label: String,
    pub weight: i64,
}

/// 子图边：weight = 可见 chunk 里该 (src, relation, dst) 被抽取出的次数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubEdge {
    pub src: String,
    pub dst: String,
    pub relation: String,
    pub weight: i64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Subgraph {
    pub nodes: Vec<SubNode>,
    pub edges: Vec<SubEdge>,
}

/// 空间级统计（都在「当前可见文档」集合上计数，见各查询的 `doc_id IN [...]`）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GraphStats {
    pub entities: i64,
    pub relations: i64,
    pub docs: i64,
}

/// agtype 文本转义（与 `graph.rs::esc` 逐字同款）：先剥反斜杠，再转义单引号。
fn esc(s: &str) -> String {
    s.replace('\\', "").replace('\'', "\\'")
}

/// agtype 值去引号（AGE 返回 `"xxx"` 带引号字符串）。同 `graph.rs::unquote`。
fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

/// AGE 连接准备：每连接需 LOAD age + search_path（同 `graph.rs::age_conn`）。
async fn age_conn(pg: &PgPool) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Postgres>> {
    let mut conn = pg.acquire().await?;
    sqlx::query("LOAD 'age'").execute(&mut *conn).await?;
    sqlx::query("SET search_path = ag_catalog, public")
        .execute(&mut *conn)
        .await?;
    Ok(conn)
}

/// 图与所需标签是否都已存在。没构建过（图/标签缺席）按空图处理，不是错误 ——
/// 否则「还没点过构建的空间」查子图会 500，而正确答案是空。
async fn labels_ready(
    conn: &mut sqlx::PgConnection,
    labels: &[&str],
) -> anyhow::Result<bool> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ag_catalog.ag_label l \
         JOIN ag_catalog.ag_graph g ON l.graph = g.graphid \
         WHERE g.name = $1 AND l.name = ANY($2::text[])",
    )
    .bind(GRAPH)
    .bind(labels)
    .fetch_one(&mut *conn)
    .await?;
    Ok(n as usize == labels.len())
}

/// 建图（幂等）。`create_graph` 没有 IF NOT EXISTS 形态，先查 ag_catalog 目录。
pub async fn ensure_graph(pg: &PgPool) -> anyhow::Result<()> {
    let mut conn = age_conn(pg).await?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ag_catalog.ag_graph WHERE name = $1)")
            .bind(GRAPH)
            .fetch_one(&mut *conn)
            .await?;
    if !exists {
        sqlx::query(&format!("SELECT create_graph('{GRAPH}')"))
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// 重建前的空间级清库：Chunk 先（DETACH 带走 MENTIONS），Entity 后（带走 RELATION）。
/// 图或标签还没建过时是空操作（见 `labels_ready`）。
pub async fn clear_space(pg: &PgPool, space_id: &str) -> anyhow::Result<()> {
    let mut conn = age_conn(pg).await?;
    for label in ["Chunk", "Entity"] {
        if !labels_ready(&mut conn, &[label]).await? {
            continue;
        }
        let cy = cypher_sql(
            &format!("MATCH (n:{label} {{space_id:'{}'}}) DETACH DELETE n", esc(space_id)),
            "v agtype",
        );
        sqlx::query(&cy).execute(&mut *conn).await?;
    }
    Ok(())
}

/// 写一个 chunk 的图投影。语句顺序：Chunk 节点 → 实体 MERGE → MENTIONS → RELATION
/// （后两条 MATCH 依赖前面的节点，顺序不能换）。
pub async fn write_chunk(pg: &PgPool, chunk: &ChunkGraph) -> anyhow::Result<()> {
    let mut conn = age_conn(pg).await?;
    for stmt in chunk_statements(chunk) {
        sqlx::query(&stmt).execute(&mut *conn).await?;
    }
    Ok(())
}

/// 一个 chunk 的全部写入语句。抽成纯函数是为了让「每个值都过了 esc」可单测。
fn chunk_statements(chunk: &ChunkGraph) -> Vec<String> {
    let mut out = vec![chunk_cypher(chunk)];
    if let Some(cy) = entities_cypher(&chunk.space_id, &chunk.entities) {
        out.push(cy);
    }
    if let Some(cy) = mentions_cypher(chunk) {
        out.push(cy);
    }
    if let Some(cy) = relations_cypher(chunk) {
        out.push(cy);
    }
    out
}

/// cypher 调用的外层包装（写语句无 RETURN，沿用 graph.rs 的 `AS (v agtype)` 形态）。
fn cypher_sql(body: &str, cols: &str) -> String {
    format!("SELECT * FROM cypher('{GRAPH}', $$ {body} $$) AS ({cols})")
}

fn chunk_cypher(chunk: &ChunkGraph) -> String {
    cypher_sql(
        &format!(
            "MERGE (c:Chunk {{doc_id:'{}', chunk_id:{}}}) SET c.space_id='{}'",
            esc(&chunk.doc_id),
            chunk.chunk_id,
            esc(&chunk.space_id)
        ),
        "v agtype",
    )
}

fn entities_cypher(space_id: &str, entities: &[GraphEntity]) -> Option<String> {
    if entities.is_empty() {
        return None;
    }
    let list = entities
        .iter()
        .map(|e| {
            format!("{{id:'{}',name:'{}',label:'{}'}}", esc(&e.id), esc(&e.name), esc(&e.label))
        })
        .collect::<Vec<_>>()
        .join(",");
    Some(cypher_sql(
        &format!(
            "UNWIND [{list}] AS e MERGE (n:Entity {{id:e.id}}) \
             SET n.space_id='{}', n.name=e.name, n.label=e.label",
            esc(space_id)
        ),
        "v agtype",
    ))
}

fn mentions_cypher(chunk: &ChunkGraph) -> Option<String> {
    if chunk.entities.is_empty() {
        return None;
    }
    let list = chunk
        .entities
        .iter()
        .map(|e| format!("'{}'", esc(&e.id)))
        .collect::<Vec<_>>()
        .join(",");
    Some(cypher_sql(
        &format!(
            "UNWIND [{list}] AS eid \
             MATCH (c:Chunk {{doc_id:'{}', chunk_id:{}}}), (n:Entity {{id:eid}}) \
             MERGE (c)-[:MENTIONS]->(n)",
            esc(&chunk.doc_id),
            chunk.chunk_id
        ),
        "v agtype",
    ))
}

fn relations_cypher(chunk: &ChunkGraph) -> Option<String> {
    if chunk.relations.is_empty() {
        return None;
    }
    let list = chunk
        .relations
        .iter()
        .map(|r| format!("{{s:'{}',d:'{}',r:'{}'}}", esc(&r.src), esc(&r.dst), esc(&r.relation)))
        .collect::<Vec<_>>()
        .join(",");
    Some(cypher_sql(
        &format!(
            "UNWIND [{list}] AS e \
             MATCH (a:Entity {{id:e.s}}), (b:Entity {{id:e.d}}) \
             CREATE (a)-[:RELATION {{relation:e.r, space_id:'{}', doc_id:'{}', chunk_id:{}}}]->(b)",
            esc(&chunk.space_id),
            esc(&chunk.doc_id),
            chunk.chunk_id
        ),
        "v agtype",
    ))
}

/// 可见文档 id 列表内联成 Cypher 列表字面量。调用方保证非空（空集在入口处短路）。
fn doc_list(doc_ids: &[String]) -> String {
    doc_ids
        .iter()
        .map(|d| format!("'{}'", esc(d)))
        .collect::<Vec<_>>()
        .join(",")
}

/// 子图节点聚合：只看可见 chunk 的 MENTIONS，weight 按提及次数降序取前 limit。
fn nodes_cypher(space_id: &str, doc_ids: &[String], limit: usize) -> String {
    cypher_sql(
        &format!(
            "MATCH (c:Chunk)-[:MENTIONS]->(e:Entity) \
             WHERE c.space_id='{}' AND c.doc_id IN [{}] \
             RETURN e.id, e.name, e.label, count(*) AS w ORDER BY count(*) DESC LIMIT {limit}",
            esc(space_id),
            doc_list(doc_ids)
        ),
        "id agtype, name agtype, label agtype, w agtype",
    )
}

/// 子图边聚合：两端都必须在已返回的节点集内（否则前端拿到指向缺席节点的悬空边）。
fn edges_cypher(
    space_id: &str,
    doc_ids: &[String],
    node_ids: &[String],
    limit: usize,
) -> String {
    cypher_sql(
        &format!(
            "MATCH (a:Entity)-[r:RELATION]->(b:Entity) \
             WHERE r.space_id='{}' AND r.doc_id IN [{}] AND a.id IN [{}] AND b.id IN [{}] \
             RETURN a.id, b.id, r.relation, count(*) AS w ORDER BY count(*) DESC LIMIT {limit}",
            esc(space_id),
            doc_list(doc_ids),
            doc_list(node_ids),
            doc_list(node_ids)
        ),
        "src agtype, dst agtype, relation agtype, w agtype",
    )
}

/// 读 agtype 四元组的公共形态：`SELECT * FROM cypher(...) AS (...)` 外层再包 `::text`
/// （agtype 类型 sqlx 不识别，同 graph.rs 的读法）。
async fn fetch_text_rows(
    conn: &mut sqlx::PgConnection,
    cypher: &str,
    cols: &str,
) -> anyhow::Result<Vec<Vec<String>>> {
    let wrapped = format!(
        "SELECT {} FROM ({cypher}) AS sub",
        cols.split(',')
            .map(|c| format!("{}::text", c.trim()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let rows = sqlx::query(&wrapped).fetch_all(&mut *conn).await?;
    Ok(rows
        .iter()
        .map(|r| {
            (0..cols.split(',').count())
                .map(|i| r.try_get::<Option<String>, _>(i).ok().flatten().unwrap_or_default())
                .collect()
        })
        .collect())
}

/// 空间子图：节点 = 可见 chunk 提及的实体 TOP limit，边 = 节点集内部的关系聚合。
/// `doc_ids` 是 knowledge 侧用 `visible_docs!()` 现算的可见集合（空集 → 空图，不碰 AGE）。
pub async fn subgraph(
    pg: &PgPool,
    space_id: &str,
    doc_ids: &[String],
    limit: usize,
) -> anyhow::Result<Subgraph> {
    let limit = limit.clamp(1, MAX_LIMIT);
    if doc_ids.is_empty() {
        return Ok(Subgraph::default());
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["Chunk", "Entity", "MENTIONS"]).await? {
        return Ok(Subgraph::default());
    }
    let nodes = fetch_text_rows(&mut conn, &nodes_cypher(space_id, doc_ids, limit), "id,name,label,w")
        .await?
        .into_iter()
        .map(|r| SubNode {
            id: unquote(&r[0]),
            name: unquote(&r[1]),
            label: unquote(&r[2]),
            weight: r[3].trim().parse().unwrap_or(0),
        })
        .collect::<Vec<_>>();
    let node_ids = nodes.iter().map(|n| n.id.clone()).collect::<Vec<_>>();
    if node_ids.is_empty() || !labels_ready(&mut conn, &["RELATION"]).await? {
        return Ok(Subgraph { nodes, edges: vec![] });
    }
    let edges =
        fetch_text_rows(&mut conn, &edges_cypher(space_id, doc_ids, &node_ids, limit), "src,dst,relation,w")
            .await?
            .into_iter()
            .map(|r| SubEdge {
                src: unquote(&r[0]),
                dst: unquote(&r[1]),
                relation: unquote(&r[2]),
                weight: r[3].trim().parse().unwrap_or(0),
            })
            .collect();
    Ok(Subgraph { nodes, edges })
}

/// 空间级统计：docs = 图里有 chunk 的可见文档数；entities/relations 同样在可见集合上数。
pub async fn stats(pg: &PgPool, space_id: &str, doc_ids: &[String]) -> anyhow::Result<GraphStats> {
    if doc_ids.is_empty() {
        return Ok(GraphStats::default());
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["Chunk"]).await? {
        return Ok(GraphStats::default());
    }
    // 去重计数在 Rust 侧做（RETURN DISTINCT 的行数就是答案），不依赖 AGE 对
    // count(DISTINCT ...) 的版本化支持。
    let docs = fetch_text_rows(
        &mut conn,
        &cypher_sql(
            &format!(
                "MATCH (c:Chunk) WHERE c.space_id='{}' AND c.doc_id IN [{}] RETURN DISTINCT c.doc_id",
                esc(space_id),
                doc_list(doc_ids)
            ),
            "doc_id agtype",
        ),
        "doc_id",
    )
    .await?
    .len() as i64;
    let mut out = GraphStats { docs, ..GraphStats::default() };
    if labels_ready(&mut conn, &["Chunk", "Entity", "MENTIONS"]).await? {
        out.entities = fetch_text_rows(
            &mut conn,
            &cypher_sql(
                &format!(
                    "MATCH (c:Chunk)-[:MENTIONS]->(e:Entity) \
                     WHERE c.space_id='{}' AND c.doc_id IN [{}] RETURN DISTINCT e.id",
                    esc(space_id),
                    doc_list(doc_ids)
                ),
                "id agtype",
            ),
            "id",
        )
        .await?
        .len() as i64;
    }
    if labels_ready(&mut conn, &["RELATION"]).await? {
        out.relations = fetch_text_rows(
            &mut conn,
            &cypher_sql(
                &format!(
                    "MATCH (a:Entity)-[r:RELATION]->(b:Entity) \
                     WHERE r.space_id='{}' AND r.doc_id IN [{}] \
                     RETURN DISTINCT a.id, b.id, r.relation",
                    esc(space_id),
                    doc_list(doc_ids)
                ),
                "src agtype, dst agtype, relation agtype",
            ),
            "src,dst,relation",
        )
        .await?
        .len() as i64;
    }
    Ok(out)
}

// ==================== 召回读侧（Yuxi B6 检索半场的取数原语） ====================
//
// 与 subgraph/stats 同一条纪律：可见 doc 集合由 knowledge 现算传入、esc 后内联进 Cypher；
// 任何空输入（doc_ids / frontier / entity_ids / chunk_ids）直接短路 —— `IN []` 是语法错，
// 且空集本来就是正确答案。权重/阈值/上限这些召回策略不在本层（在 knowledge::retrieve）。

/// 一跳邻边：按 (src,dst) 聚合的支持次数（RELATION 按 chunk CREATE，`count(*)` 即证据数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelEdge {
    pub src: String,
    pub dst: String,
    pub weight: i64,
}

/// 整数 id 列表内联成 Cypher 列表字面量（i64 按类型即注入安全，无需 esc）。
fn int_list(ids: &[i64]) -> String {
    ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")
}

/// 种子的「问句向量」来源：向量路召回 chunk 提及的实体，按被子图 chunk 提及的次数降序。
fn entities_of_chunks_cypher(
    space_id: &str,
    doc_ids: &[String],
    chunk_ids: &[i64],
    limit: usize,
) -> String {
    cypher_sql(
        &format!(
            "MATCH (c:Chunk)-[:MENTIONS]->(e:Entity) \
             WHERE c.space_id='{}' AND c.doc_id IN [{}] AND c.chunk_id IN [{}] \
             RETURN e.id, count(*) AS w ORDER BY count(*) DESC, e.id LIMIT {limit}",
            esc(space_id),
            doc_list(doc_ids),
            int_list(chunk_ids)
        ),
        "id agtype, w agtype",
    )
}

pub async fn entities_of_chunks(
    pg: &PgPool,
    space_id: &str,
    doc_ids: &[String],
    chunk_ids: &[i64],
    limit: usize,
) -> anyhow::Result<Vec<String>> {
    if doc_ids.is_empty() || chunk_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["Chunk", "Entity", "MENTIONS"]).await? {
        return Ok(Vec::new());
    }
    let rows = fetch_text_rows(
        &mut conn,
        &entities_of_chunks_cypher(space_id, doc_ids, chunk_ids, limit.clamp(1, MAX_LIMIT)),
        "id,w",
    )
    .await?;
    Ok(rows.into_iter().map(|r| unquote(&r[0])).collect())
}

/// 种子的「trgm」来源：候选池（可见文档里提及次数最多的实体）提出到外层 SQL，
/// 用 `word_similarity` / 问句包含过滤。🔴 问句只走绑定参数 `$1`，本函数签名里**没有**问句
/// 入参 —— 问句文本在类型上就进不了这条 SQL 的字符串拼接。
fn entities_named_like_sql(space_id: &str, doc_ids: &[String], candidate_limit: usize, limit: usize) -> String {
    let inner = cypher_sql(
        &format!(
            "MATCH (c:Chunk)-[:MENTIONS]->(e:Entity) \
             WHERE c.space_id='{}' AND c.doc_id IN [{}] \
             RETURN e.id, e.name, count(*) AS w ORDER BY count(*) DESC LIMIT {candidate_limit}",
            esc(space_id),
            doc_list(doc_ids)
        ),
        "id agtype, name agtype, w agtype",
    );
    // agtype 字符串的 ::text 形态带 JSON 引号，外层 trim 掉再匹配；`$1`=问句、`$2`=相似度下限。
    // 短实体名（2 字）几乎攒不出 trgm 分，包含分支（position）是它们唯一现实的命中形态。
    format!(
        "SELECT id FROM ( \
           SELECT trim(both '\"' from id::text) AS id, \
                  trim(both '\"' from name::text) AS nm, w::text::bigint AS w \
           FROM ({inner}) raw \
         ) sub \
         WHERE length(nm) >= 2 \
           AND (position(lower(nm) in lower($1)) > 0 OR word_similarity(nm, $1) > $2) \
         ORDER BY w DESC, id LIMIT {limit}"
    )
}

pub async fn entities_named_like(
    pg: &PgPool,
    space_id: &str,
    doc_ids: &[String],
    query: &str,
    sim_min: f32,
    candidate_limit: usize,
    limit: usize,
) -> anyhow::Result<Vec<String>> {
    if doc_ids.is_empty() || query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["Chunk", "Entity", "MENTIONS"]).await? {
        return Ok(Vec::new());
    }
    let sql = entities_named_like_sql(
        space_id,
        doc_ids,
        candidate_limit.clamp(1, MAX_LIMIT),
        limit.clamp(1, MAX_LIMIT),
    );
    let rows = sqlx::query(&sql).bind(query).bind(sim_min).fetch_all(&mut *conn).await?;
    Ok(rows.iter().filter_map(|r| r.try_get::<String, _>(0).ok()).collect())
}

/// frontier 的一跳邻边（正向/反向都算邻），按 (src,dst) 聚合、支持次数降序。
/// 边过滤用 `r.doc_id`（写侧每条 RELATION 都带出处）——可见集合之外的边一条都回不来。
fn relation_edges_touching_cypher(
    space_id: &str,
    doc_ids: &[String],
    frontier: &[String],
    limit: usize,
) -> String {
    cypher_sql(
        &format!(
            "MATCH (a:Entity)-[r:RELATION]->(b:Entity) \
             WHERE r.space_id='{}' AND r.doc_id IN [{}] AND (a.id IN [{}] OR b.id IN [{}]) \
             RETURN a.id, b.id, count(*) AS w ORDER BY count(*) DESC, a.id, b.id LIMIT {limit}",
            esc(space_id),
            doc_list(doc_ids),
            doc_list(frontier),
            doc_list(frontier)
        ),
        "src agtype, dst agtype, w agtype",
    )
}

pub async fn relation_edges_touching(
    pg: &PgPool,
    space_id: &str,
    doc_ids: &[String],
    frontier: &[String],
    limit: usize,
) -> anyhow::Result<Vec<RelEdge>> {
    if doc_ids.is_empty() || frontier.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["Entity", "RELATION"]).await? {
        return Ok(Vec::new());
    }
    Ok(fetch_text_rows(
        &mut conn,
        &relation_edges_touching_cypher(space_id, doc_ids, frontier, limit.clamp(1, MAX_LIMIT)),
        "src,dst,w",
    )
    .await?
    .into_iter()
    .map(|r| RelEdge {
        src: unquote(&r[0]),
        dst: unquote(&r[1]),
        weight: r[2].trim().parse().unwrap_or(0),
    })
    .collect())
}

/// 一组实体在可见文档内提及的 chunk，按「提及该 chunk 的子图实体数」降序（度数 = 与种子邻域的
/// 相关度代理）。chunk 遴选在 SQL 侧做完，Rust 组装侧只负责节点上限收口。
fn mentioned_chunks_cypher(space_id: &str, doc_ids: &[String], entity_ids: &[String], limit: usize) -> String {
    cypher_sql(
        &format!(
            "MATCH (c:Chunk)-[:MENTIONS]->(e:Entity) \
             WHERE c.space_id='{}' AND c.doc_id IN [{}] AND e.id IN [{}] \
             RETURN c.chunk_id, count(*) AS w ORDER BY count(*) DESC, c.chunk_id LIMIT {limit}",
            esc(space_id),
            doc_list(doc_ids),
            doc_list(entity_ids)
        ),
        "chunk_id agtype, w agtype",
    )
}

pub async fn mentioned_chunks(
    pg: &PgPool,
    space_id: &str,
    doc_ids: &[String],
    entity_ids: &[String],
    limit: usize,
) -> anyhow::Result<Vec<(i64, i64)>> {
    if doc_ids.is_empty() || entity_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["Chunk", "Entity", "MENTIONS"]).await? {
        return Ok(Vec::new());
    }
    Ok(fetch_text_rows(
        &mut conn,
        &mentioned_chunks_cypher(space_id, doc_ids, entity_ids, limit.clamp(1, MAX_LIMIT)),
        "chunk_id,w",
    )
    .await?
    .into_iter()
    .map(|r| (r[0].trim().parse().unwrap_or(0), r[1].trim().parse().unwrap_or(0)))
    .collect())
}

/// 已遴选 chunk 的 MENTIONS 明细（chunk_id, entity_id）—— PPR 无向化边的原料。
fn mention_pairs_cypher(
    space_id: &str,
    doc_ids: &[String],
    entity_ids: &[String],
    chunk_ids: &[i64],
    limit: usize,
) -> String {
    cypher_sql(
        &format!(
            "MATCH (c:Chunk)-[:MENTIONS]->(e:Entity) \
             WHERE c.space_id='{}' AND c.doc_id IN [{}] AND e.id IN [{}] AND c.chunk_id IN [{}] \
             RETURN c.chunk_id, e.id ORDER BY c.chunk_id, e.id LIMIT {limit}",
            esc(space_id),
            doc_list(doc_ids),
            doc_list(entity_ids),
            int_list(chunk_ids)
        ),
        "chunk_id agtype, entity_id agtype",
    )
}

pub async fn mention_pairs(
    pg: &PgPool,
    space_id: &str,
    doc_ids: &[String],
    entity_ids: &[String],
    chunk_ids: &[i64],
    limit: usize,
) -> anyhow::Result<Vec<(i64, String)>> {
    if doc_ids.is_empty() || entity_ids.is_empty() || chunk_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["Chunk", "Entity", "MENTIONS"]).await? {
        return Ok(Vec::new());
    }
    Ok(fetch_text_rows(
        &mut conn,
        &mention_pairs_cypher(space_id, doc_ids, entity_ids, chunk_ids, limit.clamp(1, MAX_LIMIT)),
        "chunk_id,entity_id",
    )
    .await?
    .into_iter()
    .map(|r| (r[0].trim().parse().unwrap_or(0), unquote(&r[1])))
    .collect())
}

// ==================== 邻居展开读侧（前端双击/按钮拉下一跳子图的取数原语） ====================
//
// 与 subgraph 同一条纪律：可见 doc 集合现算传入、esc 后内联；空输入直接短路。

/// 一跳邻域的边聚合：任一端在 centers 内、出处文档在可见集合内（与全局子图同一份过滤口径）。
fn neighborhood_edges_cypher(space_id: &str, doc_ids: &[String], centers: &[String], limit: usize) -> String {
    cypher_sql(
        &format!(
            "MATCH (a:Entity)-[r:RELATION]->(b:Entity) \
             WHERE r.space_id='{}' AND r.doc_id IN [{}] AND (a.id IN [{}] OR b.id IN [{}]) \
             RETURN a.id, b.id, r.relation, count(*) AS w ORDER BY count(*) DESC LIMIT {limit}",
            esc(space_id),
            doc_list(doc_ids),
            doc_list(centers),
            doc_list(centers)
        ),
        "src agtype, dst agtype, relation agtype, w agtype",
    )
}

/// 按 id 取实体名片（id/name/label）。邻域对端可能零可见提及（它的 chunk 全在不可见
/// 文档里），节点本身仍要返回（weight=0），否则前端会拿到指向缺席节点的悬空边。
fn entities_by_ids_cypher(space_id: &str, ids: &[String]) -> String {
    cypher_sql(
        &format!(
            "MATCH (e:Entity) WHERE e.space_id='{}' AND e.id IN [{}] RETURN e.id, e.name, e.label",
            esc(space_id),
            doc_list(ids)
        ),
        "id agtype, name agtype, label agtype",
    )
}

/// 一组实体在可见文档集合内的提及次数（与全局子图 weight 同口径）。
fn mention_weights_cypher(space_id: &str, doc_ids: &[String], ids: &[String]) -> String {
    cypher_sql(
        &format!(
            "MATCH (c:Chunk)-[:MENTIONS]->(e:Entity) \
             WHERE c.space_id='{}' AND c.doc_id IN [{}] AND e.id IN [{}] \
             RETURN e.id, count(*) AS w",
            esc(space_id),
            doc_list(doc_ids),
            doc_list(ids)
        ),
        "id agtype, w agtype",
    )
}

/// 一跳邻域子图：centers + 与它们有可见 RELATION 的对端实体 + 其间的关系聚合边。
/// weight 与全局子图同口径；零可见提及的对端实体 weight=0 也返回（防悬空边）。
pub async fn neighborhood(
    pg: &PgPool,
    space_id: &str,
    doc_ids: &[String],
    centers: &[String],
    limit: usize,
) -> anyhow::Result<Subgraph> {
    if doc_ids.is_empty() || centers.is_empty() {
        return Ok(Subgraph::default());
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["Entity", "RELATION"]).await? {
        return Ok(Subgraph::default());
    }
    let edges = fetch_text_rows(
        &mut conn,
        &neighborhood_edges_cypher(space_id, doc_ids, centers, limit.clamp(1, MAX_LIMIT)),
        "src,dst,relation,w",
    )
    .await?
    .into_iter()
    .map(|r| SubEdge {
        src: unquote(&r[0]),
        dst: unquote(&r[1]),
        relation: unquote(&r[2]),
        weight: r[3].trim().parse().unwrap_or(0),
    })
    .collect::<Vec<_>>();
    // 节点集 = centers ∪ 边两端（保序去重）；孤立的 center 也能回（名片在就有节点）
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut ids: Vec<String> = Vec::new();
    for id in centers.iter().chain(edges.iter().flat_map(|e| [&e.src, &e.dst])) {
        if seen.insert(id.as_str()) {
            ids.push(id.clone());
        }
    }
    if ids.is_empty() {
        return Ok(Subgraph { nodes: vec![], edges });
    }
    let mut nodes = fetch_text_rows(&mut conn, &entities_by_ids_cypher(space_id, &ids), "id,name,label")
        .await?
        .into_iter()
        .map(|r| SubNode {
            id: unquote(&r[0]),
            name: unquote(&r[1]),
            label: unquote(&r[2]),
            weight: 0,
        })
        .collect::<Vec<_>>();
    if !nodes.is_empty() && labels_ready(&mut conn, &["Chunk", "MENTIONS"]).await? {
        for row in fetch_text_rows(&mut conn, &mention_weights_cypher(space_id, doc_ids, &ids), "id,w").await? {
            let id = unquote(&row[0]);
            let w = row[1].trim().parse().unwrap_or(0);
            if let Some(n) = nodes.iter_mut().find(|n| n.id == id) {
                n.weight = w;
            }
        }
    }
    Ok(Subgraph { nodes, edges })
}

/// 该空间在可见文档内是否已有图数据（一条探测）——「没命中实体」与「图里没数据」的判别件：
/// 前者是正常空路，后者是降级（调用方 warn 留痕）。
fn space_chunks_probe_cypher(space_id: &str, doc_ids: &[String]) -> String {
    cypher_sql(
        &format!(
            "MATCH (c:Chunk) WHERE c.space_id='{}' AND c.doc_id IN [{}] RETURN c.chunk_id LIMIT 1",
            esc(space_id),
            doc_list(doc_ids)
        ),
        "chunk_id agtype",
    )
}

pub async fn space_has_chunks(pg: &PgPool, space_id: &str, doc_ids: &[String]) -> anyhow::Result<bool> {
    if doc_ids.is_empty() {
        return Ok(false);
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["Chunk"]).await? {
        return Ok(false);
    }
    Ok(!fetch_text_rows(&mut conn, &space_chunks_probe_cypher(space_id, doc_ids), "chunk_id")
        .await?
        .is_empty())
}

// ==================== 运营侧（Y4：failed-chunks / reset / reconcile 的取数与删除原语） ====================
//
// 与读侧同一条纪律：值全过 esc/int_list 内联进本文件唯一的 `cypher_sql` 包装、空输入短路、
// 标签未建 = 空图（不是错误）。🔴 本层**不做 ACL**：孤儿判据（文档生命周期）与空间写权限
// 判定都在调用方（knowledge/server）—— 这里的输入清单一律视为已授权事实。

/// 运维扫描的行数保险丝：Chunk 节点全量列举的上限。构建本身只扫 ≤2000 chunk/轮，
/// 正常空间远低于它；真撑到上限说明该空间该重建而不是接着运维（调用方拿到的仍是对的答案，
/// 只是可能不全 —— 计数口径会在响应里透出 total，截断看得见）。
const GRAPH_SCAN_ROWS: usize = 100_000;

/// 空间内全部 Chunk 节点（doc_id, chunk_id），按 chunk_id 升序。
/// 两个用途：reconcile 的孤儿检测输入（与「活着的文档」做集合差）；
/// failed-chunks 端点的在图集合（构建口径 − 在图 = 未入图）。图/标签未建 = 空。
pub async fn chunk_nodes(pg: &PgPool, space_id: &str) -> anyhow::Result<Vec<(String, i64)>> {
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["Chunk"]).await? {
        return Ok(Vec::new());
    }
    Ok(fetch_text_rows(
        &mut conn,
        &cypher_sql(
            &format!(
                "MATCH (c:Chunk) WHERE c.space_id='{}' \
                 RETURN c.doc_id, c.chunk_id ORDER BY c.chunk_id LIMIT {GRAPH_SCAN_ROWS}",
                esc(space_id)
            ),
            "doc_id agtype, chunk_id agtype",
        ),
        "doc_id,chunk_id",
    )
    .await?
    .into_iter()
    .map(|r| (unquote(&r[0]), r[1].trim().parse().unwrap_or(0)))
    .collect())
}

/// 悬空实体检测：MENTIONS 全量聚合在图内做完（`om = tm` ⇔ 该实体的每一条提及都来自孤儿
/// chunk），只回传悬空者 id。AGE 子集内写法（WITH / CASE / sum / count）。
/// `orphan_chunk_ids` 为空时调用方必须短路（`IN []` 是语法错，且没有孤儿就不可能有悬空）。
fn dangling_entities_cypher(space_id: &str, orphan_chunk_ids: &[i64]) -> String {
    cypher_sql(
        &format!(
            "MATCH (c:Chunk)-[:MENTIONS]->(e:Entity) WHERE e.space_id='{}' \
             WITH e, sum(CASE WHEN c.chunk_id IN [{}] THEN 1 ELSE 0 END) AS om, count(*) AS tm \
             WHERE om = tm RETURN e.id ORDER BY e.id",
            esc(space_id),
            int_list(orphan_chunk_ids)
        ),
        "id agtype",
    )
}

/// 悬空实体 id 清单（见 `dangling_entities_cypher`）。图/标签未建或清单为空 = 空。
pub async fn dangling_entities(
    pg: &PgPool,
    space_id: &str,
    orphan_chunk_ids: &[i64],
) -> anyhow::Result<Vec<String>> {
    if orphan_chunk_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["Chunk", "Entity", "MENTIONS"]).await? {
        return Ok(Vec::new());
    }
    Ok(fetch_text_rows(&mut conn, &dangling_entities_cypher(space_id, orphan_chunk_ids), "id")
        .await?
        .into_iter()
        .map(|r| unquote(&r[0]))
        .collect())
}

/// 出自一组 chunk 的 RELATION 边数（dry-run 统计与真删前的同一口径；RELATION 按 chunk
/// CREATE，出处就在 `r.chunk_id` 上）。
pub async fn relation_count_of_chunks(
    pg: &PgPool,
    space_id: &str,
    chunk_ids: &[i64],
) -> anyhow::Result<i64> {
    if chunk_ids.is_empty() {
        return Ok(0);
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["RELATION"]).await? {
        return Ok(0);
    }
    let rows = fetch_text_rows(
        &mut conn,
        &cypher_sql(
            &format!(
                "MATCH ()-[r:RELATION]->() WHERE r.space_id='{}' AND r.chunk_id IN [{}] \
                 RETURN count(*) AS w",
                esc(space_id),
                int_list(chunk_ids)
            ),
            "w agtype",
        ),
        "w",
    )
    .await?;
    Ok(rows.first().and_then(|r| r[0].trim().parse().ok()).unwrap_or(0))
}

/// reconcile 真删第 ① 步：按出处删 RELATION 边（先于节点删 —— 统计口径与删除对象一致，
/// 且节点 DETACH DELETE 前把「非悬空实体上出自孤儿 chunk 的边」也带走）。
/// 幂等：清单是调用方现算的，重跑 = 空清单 = 空操作。
pub async fn delete_relations_of_chunks(
    pg: &PgPool,
    space_id: &str,
    chunk_ids: &[i64],
) -> anyhow::Result<()> {
    if chunk_ids.is_empty() {
        return Ok(());
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["RELATION"]).await? {
        return Ok(());
    }
    let cy = cypher_sql(
        &format!(
            "MATCH ()-[r:RELATION]->() WHERE r.space_id='{}' AND r.chunk_id IN [{}] DELETE r",
            esc(space_id),
            int_list(chunk_ids)
        ),
        "v agtype",
    );
    sqlx::query(&cy).execute(&mut *conn).await?;
    Ok(())
}

/// reconcile 真删第 ② 步：DETACH DELETE 孤儿 Chunk（带走它们的 MENTIONS）。
pub async fn delete_chunks(pg: &PgPool, space_id: &str, chunk_ids: &[i64]) -> anyhow::Result<()> {
    if chunk_ids.is_empty() {
        return Ok(());
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["Chunk"]).await? {
        return Ok(());
    }
    let cy = cypher_sql(
        &format!(
            "MATCH (c:Chunk) WHERE c.space_id='{}' AND c.chunk_id IN [{}] DETACH DELETE c",
            esc(space_id),
            int_list(chunk_ids)
        ),
        "v agtype",
    );
    sqlx::query(&cy).execute(&mut *conn).await?;
    Ok(())
}

/// reconcile 真删第 ③ 步：DETACH DELETE 悬空实体。它们残留的边必然全出自孤儿 chunk
/// （写侧纪律：关系端点必登 MENTIONS —— 被活 chunk 的关系指向的实体一定不悬空），
/// 所以 DETACH 带走的边必已被第 ① 步覆盖统计，不会出现「删了没数过的边」。
pub async fn delete_entities(pg: &PgPool, space_id: &str, entity_ids: &[String]) -> anyhow::Result<()> {
    if entity_ids.is_empty() {
        return Ok(());
    }
    let mut conn = age_conn(pg).await?;
    if !labels_ready(&mut conn, &["Entity"]).await? {
        return Ok(());
    }
    let cy = cypher_sql(
        &format!(
            "MATCH (e:Entity) WHERE e.space_id='{}' AND e.id IN [{}] DETACH DELETE e",
            esc(space_id),
            doc_list(entity_ids)
        ),
        "v agtype",
    );
    sqlx::query(&cy).execute(&mut *conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk_graph() -> ChunkGraph {
        ChunkGraph {
            space_id: "sp1".into(),
            doc_id: "doc-1".into(),
            chunk_id: 7,
            entities: vec![
                GraphEntity { id: "e_1".into(), name: "差旅报销制度".into(), label: "制度".into() },
                GraphEntity { id: "e_2".into(), name: "财务部".into(), label: "组织".into() },
            ],
            relations: vec![GraphRelation {
                src: "e_1".into(),
                dst: "e_2".into(),
                relation: "由发布".into(),
            }],
        }
    }

    /// 与 graph.rs 同款的两个基础件，行为必须逐字一致（拼接安全的全部前提）。
    #[test]
    fn esc_and_unquote_match_graph_rs() {
        assert_eq!(esc("O'Brien"), "O\\'Brien");
        assert_eq!(esc("a\\b"), "ab");
        assert_eq!(esc("正常名"), "正常名");
        assert_eq!(unquote("\"恒众\""), "恒众");
        assert_eq!(unquote("恒众"), "恒众");
    }

    /// 写语句的顺序与形状：Chunk 节点永远第一条；空实体/空关系不出语句。
    #[test]
    fn chunk_statements_order_and_short_circuit() {
        let g = chunk_graph();
        let stmts = chunk_statements(&g);
        assert_eq!(stmts.len(), 4);
        assert!(stmts[0].contains("MERGE (c:Chunk {doc_id:'doc-1', chunk_id:7})"), "{}", stmts[0]);
        assert!(stmts[0].contains("SET c.space_id='sp1'"), "{}", stmts[0]);
        assert!(stmts[1].contains("MERGE (n:Entity {id:e.id})"), "{}", stmts[1]);
        assert!(stmts[2].contains("MERGE (c)-[:MENTIONS]->(n)"), "{}", stmts[2]);
        assert!(stmts[3].contains("CREATE (a)-[:RELATION"), "{}", stmts[3]);

        let mut empty = g.clone();
        empty.entities = vec![];
        empty.relations = vec![];
        let stmts = chunk_statements(&empty);
        assert_eq!(stmts.len(), 1, "空抽取只落 Chunk 节点：{stmts:?}");
    }

    /// 🔴 实体名/关系类型来自 LLM 输出（不可信输入）：单引号必须转义进不了 Cypher 语法层。
    #[test]
    fn cypher_escapes_llm_controlled_values() {
        let mut g = chunk_graph();
        g.entities[0].name = "O'Brien\\制度".into();
        g.relations[0].relation = "it's".into();
        let stmts = chunk_statements(&g);
        assert!(stmts[1].contains("O\\'Brien制度"), "{}", stmts[1]);
        assert!(!stmts[1].contains("O'Brien"), "未转义的引号进了 Cypher：{}", stmts[1]);
        assert!(stmts[3].contains("it\\'s"), "{}", stmts[3]);
        // 出处属性必须真的在边上（查询侧按 r.space_id / r.doc_id 过滤全靠它们）
        assert!(stmts[3].contains("space_id:'sp1'") && stmts[3].contains("doc_id:'doc-1'"));
        assert!(stmts[3].contains("chunk_id:7"));
    }

    /// 清库必须先 Chunk 后 Entity，且都是 DETACH DELETE（不留悬空边）。
    #[test]
    fn clear_space_detaches_both_labels() {
        let src = include_str!("doc_graph.rs");
        let body = src
            .split("pub async fn clear_space")
            .nth(1)
            .expect("clear_space 不见了")
            .split("pub async fn write_chunk")
            .next()
            .unwrap();
        assert!(body.contains("[\"Chunk\", \"Entity\"]"), "清库顺序变了：{body}");
        assert!(body.contains("DETACH DELETE"), "{body}");
        assert!(body.contains("space_id"), "清库必须按空间隔离：{body}");
    }

    /// 查询侧的可见性过滤：三个 Cypher 都必须带 doc_id IN [...] 内联列表。
    #[test]
    fn query_cyphers_inline_the_visible_doc_set() {
        let docs = vec!["d1".to_string(), "d'2".to_string()];
        let nodes = nodes_cypher("sp1", &docs, 200);
        assert!(nodes.contains("c.doc_id IN ['d1','d\\'2']"), "{nodes}");
        assert!(nodes.contains("LIMIT 200"), "{nodes}");
        let edges = edges_cypher("sp1", &docs, &["e_1".to_string()], 200);
        assert!(edges.contains("r.doc_id IN ['d1','d\\'2']"), "{edges}");
        assert!(edges.contains("a.id IN ['e_1']") && edges.contains("b.id IN ['e_1']"), "{edges}");
    }

    /// 空可见集合不许碰 AGE（`IN []` 是语法错，且空图本来就是正确答案）。
    #[tokio::test]
    async fn empty_visible_set_never_touches_age() {
        let pool = crate::owned::dead_pg_pool_for_tests(std::time::Duration::from_millis(50));
        let sg = subgraph(&pool, "sp1", &[], 200).await.expect("空集该直接给空图");
        assert!(sg.nodes.is_empty() && sg.edges.is_empty());
        let st = stats(&pool, "sp1", &[]).await.expect("空集该直接给零统计");
        assert_eq!(st, GraphStats::default());
    }

    /// 本文件是 kb_graph 的唯一拼接收口：图名常量 + 全部值过 esc 的纪律锚点。
    #[test]
    fn kb_graph_has_a_single_assembly_point() {
        let src = include_str!("doc_graph.rs");
        assert!(src.contains("const GRAPH: &str = \"kb_graph\""));
        // 防恒真：拼接入口真的存在，且没有第二处裸 cypher( 调用形态
        assert_eq!(src.matches(concat!("fn cyp", "her_sql")).count(), 1, "cypher 包装只能有一个");
        assert!(src.contains(concat!("fn e", "sc(")), "esc 没了");
    }

    // ==================== 召回读侧（B6 检索半场） ====================

    /// 召回读侧的可见性过滤：每条 Cypher 都必须带 doc_id IN [...] 内联列表与 space_id 谓词。
    #[test]
    fn retrieval_cyphers_inline_the_visible_doc_set() {
        let docs = vec!["d1".to_string(), "d'2".to_string()];
        let frontier = vec!["e_1".to_string()];
        let by_chunk = entities_of_chunks_cypher("sp1", &docs, &[7, 9], 20);
        assert!(by_chunk.contains("c.doc_id IN ['d1','d\\'2']"), "{by_chunk}");
        assert!(by_chunk.contains("c.chunk_id IN [7,9]"), "{by_chunk}");
        assert!(by_chunk.contains("c.space_id='sp1'"), "{by_chunk}");
        let edges = relation_edges_touching_cypher("sp1", &docs, &frontier, 1000);
        assert!(edges.contains("r.doc_id IN ['d1','d\\'2']"), "{edges}");
        assert!(edges.contains("a.id IN ['e_1'] OR b.id IN ['e_1']"), "{edges}");
        let chunks = mentioned_chunks_cypher("sp1", &docs, &frontier, 200);
        assert!(chunks.contains("e.id IN ['e_1']"), "{chunks}");
        assert!(chunks.contains("c.doc_id IN ['d1','d\\'2']"), "{chunks}");
        let pairs = mention_pairs_cypher("sp1", &docs, &frontier, &[7], 5000);
        assert!(pairs.contains("c.chunk_id IN [7]"), "{pairs}");
        assert!(pairs.contains("e.id IN ['e_1']"), "{pairs}");
        let probe = space_chunks_probe_cypher("sp1", &docs);
        assert!(probe.contains("c.doc_id IN ['d1','d\\'2']"), "{probe}");
    }

    // ==================== 邻居展开读侧 ====================

    /// 邻域三条 Cypher 都必须内联可见 doc 集合 / center 列表，且全部值过 esc。
    #[test]
    fn neighborhood_cyphers_inline_visible_docs_and_centers() {
        let docs = vec!["d1".to_string(), "d'2".to_string()];
        let centers = vec!["e_1".to_string()];
        let edges = neighborhood_edges_cypher("sp1", &docs, &centers, 120);
        assert!(edges.contains("r.doc_id IN ['d1','d\\'2']"), "{edges}");
        assert!(edges.contains("a.id IN ['e_1'] OR b.id IN ['e_1']"), "{edges}");
        assert!(edges.contains("r.space_id='sp1'") && edges.contains("LIMIT 120"), "{edges}");
        // 名片查询：不过可见 doc（实体本身是空间级归并的），但必须按 space + id 白名单收口
        let cards = entities_by_ids_cypher("sp1", &["e_1".to_string(), "e'2".to_string()]);
        assert!(cards.contains("e.space_id='sp1'"), "{cards}");
        assert!(cards.contains("e.id IN ['e_1','e\\'2']"), "{cards}");
        let weights = mention_weights_cypher("sp1", &docs, &centers);
        assert!(weights.contains("c.doc_id IN ['d1','d\\'2']"), "{weights}");
        assert!(weights.contains("e.id IN ['e_1']"), "{weights}");
    }

    /// 空输入不许碰 AGE（死池一碰就 Err，返 Ok(空) 即证明一条查询都没发）。
    #[tokio::test]
    async fn neighborhood_short_circuits_empty_inputs() {
        let pool = crate::owned::dead_pg_pool_for_tests(std::time::Duration::from_millis(50));
        let sg = neighborhood(&pool, "sp1", &[], &["e".into()], 120).await.expect("空 doc 集该直接给空图");
        assert!(sg.nodes.is_empty() && sg.edges.is_empty());
        let sg = neighborhood(&pool, "sp1", &["d".into()], &[], 120).await.expect("空 center 该直接给空图");
        assert!(sg.nodes.is_empty() && sg.edges.is_empty());
    }

    /// 🔴 问句只许走绑定参数：`entities_named_like_sql` 的签名里压根没有问句入参，
    /// `$1`/`$2` 必须落在外层 SQL，且 Cypher 体（$$..$$）里一个 `$` 都不许有。
    #[test]
    fn named_like_matches_in_outer_sql_with_bound_params() {
        let sql = entities_named_like_sql("sp1", &["d1".to_string()], 500, 20);
        assert!(sql.contains("word_similarity(nm, $1) > $2"), "{sql}");
        assert!(sql.contains("position(lower(nm) in lower($1)) > 0"), "{sql}");
        assert!(sql.contains("length(nm) >= 2"), "单字符实体名不许靠包含命中一切：{sql}");
        let body = sql.split("$$").nth(1).expect("cypher 体不见了");
        assert!(!body.contains('$'), "Cypher 体内不许出现绑定参数：{body}");
    }

    /// 空输入不许碰 AGE（`IN []` 是语法错，且空本来就是正确答案）——死池一碰就 Err，
    /// 全部返 Ok(空) 即证明一条查询都没发。
    #[tokio::test]
    async fn retrieval_reads_short_circuit_empty_inputs() {
        let pool = crate::owned::dead_pg_pool_for_tests(std::time::Duration::from_millis(50));
        assert!(entities_of_chunks(&pool, "sp1", &[], &[1], 20).await.unwrap().is_empty());
        assert!(entities_of_chunks(&pool, "sp1", &["d".into()], &[], 20).await.unwrap().is_empty());
        assert!(entities_named_like(&pool, "sp1", &[], "q", 0.3, 500, 20).await.unwrap().is_empty());
        assert!(entities_named_like(&pool, "sp1", &["d".into()], "  ", 0.3, 500, 20).await.unwrap().is_empty());
        let empty_edges = relation_edges_touching(&pool, "sp1", &[], &["e".into()], 100).await.unwrap();
        assert!(empty_edges.is_empty());
        assert!(relation_edges_touching(&pool, "sp1", &["d".into()], &[], 100).await.unwrap().is_empty());
        assert!(mentioned_chunks(&pool, "sp1", &[], &["e".into()], 100).await.unwrap().is_empty());
        assert!(mention_pairs(&pool, "sp1", &["d".into()], &[], &[1], 100).await.unwrap().is_empty());
        assert!(!space_has_chunks(&pool, "sp1", &[]).await.unwrap());
    }

    // ==================== 运营侧（Y4） ====================

    /// 悬空实体检测的形状：聚合在图内做（WITH + CASE），只回传 id；孤儿清单与 space 过滤
    /// 都必须内联且过 esc/int_list。空清单由调用方短路，本函数不许见到（`IN []` 语法错）。
    #[test]
    fn dangling_entities_cypher_aggregates_inside_graph() {
        let cy = dangling_entities_cypher("sp'1", &[7, 9]);
        assert!(cy.contains("e.space_id='sp\\'1'"), "{cy}");
        assert!(cy.contains("c.chunk_id IN [7,9]"), "{cy}");
        assert!(cy.contains("sum(CASE WHEN"), "{cy}");
        assert!(cy.contains("WHERE om = tm"), "悬空判据（每条提及都出自孤儿）变了：{cy}");
    }

    /// 运营写侧三条 DELETE 的形状锚点：都按 space 过滤、清单内联、幂等（重删=空操作）；
    /// Chunk/Entity 必须 DETACH（不留悬空边），RELATION 按出处 `r.chunk_id` 删。
    #[test]
    fn reconcile_delete_cyphers_are_scoped_and_detached() {
        let src = include_str!("doc_graph.rs");
        for f in ["pub async fn delete_relations_of_chunks", "pub async fn delete_chunks"] {
            let body = src.split(f).nth(1).unwrap_or_else(|| panic!("{f} 不见了"));
            let body = body.split("\n}\n").next().unwrap();
            assert!(body.contains("r.space_id='{}'") || body.contains("c.space_id='{}'"), "{f} 丢了空间过滤");
            assert!(body.contains("int_list(chunk_ids)"), "{f} 的清单没过 int_list");
            assert!(body.contains("if chunk_ids.is_empty()"), "{f} 丢了空清单短路");
        }
        let chunks = src.split("pub async fn delete_chunks").nth(1).unwrap();
        assert!(chunks.contains("DETACH DELETE c"), "Chunk 删除必须 DETACH（带走 MENTIONS）");
        let entities = src.split("pub async fn delete_entities").nth(1).unwrap();
        assert!(entities.contains("DETACH DELETE e"), "Entity 删除必须 DETACH");
        assert!(entities.contains("doc_list(entity_ids)"), "实体 id 清单没过 esc 内联");
        let rel = src.split("pub async fn delete_relations_of_chunks").nth(1).unwrap();
        assert!(rel.contains("r.chunk_id IN"), "RELATION 必须按 chunk 出处删");
        // 统计与删除同一口径：count 也用 r.chunk_id
        let count = src.split("pub async fn relation_count_of_chunks").nth(1).unwrap();
        assert!(count.contains("r.chunk_id IN") && count.contains("count(*)"), "统计口径与删除不一致");
    }

    /// 运营侧空输入同样不许碰 AGE（死池即判据）。
    #[tokio::test]
    async fn ops_writes_short_circuit_empty_inputs() {
        let pool = crate::owned::dead_pg_pool_for_tests(std::time::Duration::from_millis(50));
        assert!(dangling_entities(&pool, "sp1", &[]).await.unwrap().is_empty());
        assert_eq!(relation_count_of_chunks(&pool, "sp1", &[]).await.unwrap(), 0);
        delete_relations_of_chunks(&pool, "sp1", &[]).await.unwrap();
        delete_chunks(&pool, "sp1", &[]).await.unwrap();
        delete_entities(&pool, "sp1", &[]).await.unwrap();
    }
}
