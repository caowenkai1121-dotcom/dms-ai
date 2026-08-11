//! 四注册表 → `meta.element` 的幂等派生（SuperSonic SchemaElement 统一层）。
//! 变更原因＝元素派生规则。搬运源 `server/src/meta.rs:521-608`。

use sqlx::PgPool;

/// 元素注册表同步（SuperSonic SchemaElement 统一层）：
/// metric/dimension/value_map/term 四注册表 → 统一元素（向量化召回的原子单位）。
/// 幂等 upsert；元素变更后重跑即可（search_text 变了会被 `embed_fill` 的 A9 自愈按
/// `MetaVecTarget::Element` 补向量，不再依赖手工重跑 embed build）。
/// 【K3-B ②】不吃 `ds` 形参：**每行元素跟着它的源走**（`ds_id` 从四张注册表原样带出来），
/// 所以这一支天然是全源的，跑一次把每个源的元素各自补齐。
///
/// 四支包在一个事务里：中途失败整体回滚（幂等可重跑），不留混合态元素表被召回读到半成品。
/// 串行执行是刻意的：同一事务就是同一连接，并发不了 —— 启动期任务，正确性优先。
pub async fn sync_elements(pg: &PgPool) -> anyhow::Result<()> {
    let mut tx = pg.begin().await?;
    sync_metrics(&mut tx).await?;
    sync_dimensions(&mut tx).await?;
    sync_values(&mut tx).await?;
    sync_terms(&mut tx).await?;
    sync_disabled(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// 连接别名：四支在同一事务连接上串行执行。
type Conn<'a> = &'a mut sqlx::PgConnection;

/// 以下四支只做「取一张注册表 → 逐行派生元素」，逐段原样搬自 `sync_elements`（顺序即行为）。
async fn sync_metrics(pg: Conn<'_>) -> anyhow::Result<()> {
    // metric（ORDER BY 钉死处理序：失败重试时日志/断点位置稳定）
    let metrics: Vec<(String, String, String, Vec<String>, String, String)> = sqlx::query_as(
        "SELECT ds_id, metric_code, name, aliases, agg_expr, description FROM meta.metric WHERE status = 'active'
         ORDER BY ds_id, metric_code",
    )
    .fetch_all(&mut *pg)
    .await?;
    let mut id = String::with_capacity(48);
    for (ds, code, name, aliases, agg, desc) in metrics {
        // 复用可清空 buffer，不每行一个新 String
        id.clear();
        use std::fmt::Write as _;
        let _ = write!(id, "metric:{code}");
        let e = Element {
            ds: &ds,
            id: &id,
            kind: "metric",
            name: &name,
            aliases: &aliases,
            ref_expr: &agg,
            desc: &desc,
        };
        upsert_element(&mut *pg, e).await?;
    }
    Ok(())
}

async fn sync_dimensions(pg: Conn<'_>) -> anyhow::Result<()> {
    // dimension
    let dims: Vec<(String, String, String, Vec<String>, String, String)> = sqlx::query_as(
        "SELECT ds_id, dim_code, name, aliases, expr, description FROM meta.dimension WHERE status = 'active'
         ORDER BY ds_id, dim_code",
    )
    .fetch_all(&mut *pg)
    .await?;
    let mut id = String::with_capacity(48);
    for (ds, code, name, aliases, expr, desc) in dims {
        id.clear();
        use std::fmt::Write as _;
        let _ = write!(id, "dimension:{code}");
        let e = Element {
            ds: &ds,
            id: &id,
            kind: "dimension",
            name: &name,
            aliases: &aliases,
            ref_expr: &expr,
            desc: &desc,
        };
        upsert_element(&mut *pg, e).await?;
    }
    Ok(())
}

async fn sync_values(pg: Conn<'_>) -> anyhow::Result<()> {
    // value（码值也是元素：「已开票」「线下客户」应能向量命中）
    let vals: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT ds_id, table_name, column_name, name, code FROM meta.value_map
         ORDER BY ds_id, table_name, column_name, name",
    )
    .fetch_all(&mut *pg)
    .await?;
    let mut id = String::with_capacity(64);
    for (ds, table, col, name, code) in vals {
        id.clear();
        use std::fmt::Write as _;
        let _ = write!(id, "value:{table}.{col}:{code}");
        let desc = format!("{table}.{col} 的码值 {code}");
        let e = Element {
            ds: &ds,
            id: &id,
            kind: "value",
            name: &name,
            aliases: &[],
            ref_expr: &code,
            desc: &desc,
        };
        upsert_element(&mut *pg, e).await?;
    }
    Ok(())
}

async fn sync_terms(pg: Conn<'_>) -> anyhow::Result<()> {
    // term
    let terms: Vec<(String, String, String, Vec<String>)> = sqlx::query_as(
        "SELECT ds_id, term, definition, aliases FROM meta.term WHERE status = 'active'
         ORDER BY ds_id, term",
    )
    .fetch_all(&mut *pg)
    .await?;
    let mut id = String::with_capacity(48);
    for (ds, term, def, aliases) in terms {
        id.clear();
        use std::fmt::Write as _;
        let _ = write!(id, "term:{term}");
        let e = Element {
            ds: &ds,
            id: &id,
            kind: "term",
            name: &term,
            aliases: &aliases,
            ref_expr: &def,
            desc: "",
        };
        upsert_element(&mut *pg, e).await?;
    }
    Ok(())
}

/// 停用收敛：源注册表已 disabled/删除的元素同步置 disabled（原来只增不收敛 —— 指标/维度
/// 被禁用后其元素仍滞留 active，照样被向量召回，embed_fill 也按 active 选）。
/// 反向（源复活）由四支 upsert 的 `status='active'` 收敛回来；meta.element 没有人工
/// 状态写口（全仓 grep 只剩 embed_fill 的 embedding 更新），全自动收敛无人工冲突。
async fn sync_disabled(pg: Conn<'_>) -> anyhow::Result<()> {
    let res = sqlx::query(
        "UPDATE meta.element e SET status = 'disabled'
         WHERE e.status = 'active' AND (
           (e.kind = 'metric' AND NOT EXISTS (
             SELECT 1 FROM meta.metric m WHERE m.status = 'active' AND m.ds_id = e.ds_id
               AND e.element_id = 'metric:' || m.metric_code))
        OR (e.kind = 'dimension' AND NOT EXISTS (
             SELECT 1 FROM meta.dimension d WHERE d.status = 'active' AND d.ds_id = e.ds_id
               AND e.element_id = 'dimension:' || d.dim_code))
        OR (e.kind = 'term' AND NOT EXISTS (
             SELECT 1 FROM meta.term t WHERE t.status = 'active' AND t.ds_id = e.ds_id
               AND e.element_id = 'term:' || t.term))
        OR (e.kind = 'value' AND NOT EXISTS (
             SELECT 1 FROM meta.value_map v WHERE v.ds_id = e.ds_id
               AND e.element_id = 'value:' || v.table_name || '.' || v.column_name || ':' || v.code))
         )",
    )
    .execute(&mut *pg)
    .await?;
    if res.rows_affected() > 0 {
        tracing::info!(disabled = res.rows_affected(), "元素表停用收敛");
    }
    Ok(())
}

/// 一个待写入的元素。原先是 `upsert_element` 的 8 个连排形参（挂着
/// `#[allow(clippy::too_many_arguments)]`）——D4「拆函数必须同时拆参数」在此收口，allow 随之删除。
struct Element<'a> {
    ds: &'a str,
    id: &'a str,
    kind: &'a str,
    name: &'a str,
    aliases: &'a [String],
    ref_expr: &'a str,
    desc: &'a str,
}

/// 单元素幂等 upsert（search_text=名+别名+描述 截 500 字；文本变化时清 embedding 待
/// A9 自愈重建）。`status='active'` 进 SET 是收敛的另一半：源注册表复活的行在这里复活
/// （与 `sync_disabled` 配对；元素表没有人工状态写口，无人工结论会被冲掉）。
async fn upsert_element(pg: Conn<'_>, e: Element<'_>) -> anyhow::Result<()> {
    let Element { ds, id, kind, name, aliases, ref_expr, desc } = e;
    let search = {
        use std::fmt::Write as _;
        let mut s = name.to_string();
        if !aliases.is_empty() {
            let _ = write!(s, "（{}）", aliases.join("、"));
        }
        if !desc.is_empty() {
            let _ = write!(s, "：{desc}");
        }
        s.chars().take(500).collect::<String>()
    };
    sqlx::query(
        "INSERT INTO meta.element(element_id, kind, name, aliases, ref_expr, description, search_text, ds_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
         ON CONFLICT (ds_id, element_id) DO UPDATE SET
           kind=$2, name=$3, aliases=$4, ref_expr=$5, description=$6, search_text=$7,
           status='active',
           embedding = CASE WHEN meta.element.search_text = $7 THEN meta.element.embedding ELSE NULL END",
    )
    .bind(id)
    .bind(kind)
    .bind(name)
    .bind(aliases.to_vec())
    .bind(ref_expr)
    .bind(desc)
    .bind(&search)
    .bind(ds)
    .execute(&mut *pg)
    .await?;
    Ok(())
}
