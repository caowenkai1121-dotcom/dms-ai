//! 四注册表 → `meta.element` 的幂等派生（SuperSonic SchemaElement 统一层）。
//! 变更原因＝元素派生规则。搬运源 `server/src/meta.rs:521-608`。

use sqlx::PgPool;

/// 元素注册表同步（SuperSonic SchemaElement 统一层）：
/// metric/dimension/value_map/term 四注册表 → 统一元素（向量化召回的原子单位）。
/// 幂等 upsert；元素变更后重跑即可（search_text 变了需重跑 embed build 补向量）。
/// 【K3-B ②】不吃 `ds` 形参：**每行元素跟着它的源走**（`ds_id` 从四张注册表原样带出来），
/// 所以这一支天然是全源的，跑一次把每个源的元素各自补齐。
pub async fn sync_elements(pg: &PgPool) -> anyhow::Result<()> {
    sync_metrics(pg).await?;
    sync_dimensions(pg).await?;
    sync_values(pg).await?;
    sync_terms(pg).await?;
    Ok(())
}

/// 以下四支只做「取一张注册表 → 逐行派生元素」，逐段原样搬自 `sync_elements`（顺序即行为）。
async fn sync_metrics(pg: &PgPool) -> anyhow::Result<()> {
    // metric
    let metrics: Vec<(String, String, String, Vec<String>, String, String)> = sqlx::query_as(
        "SELECT ds_id, metric_code, name, aliases, agg_expr, description FROM meta.metric WHERE status = 'active'",
    )
    .fetch_all(pg)
    .await?;
    for (ds, code, name, aliases, agg, desc) in metrics {
        let e = Element {
            ds: &ds,
            id: &format!("metric:{code}"),
            kind: "metric",
            name: &name,
            aliases: &aliases,
            ref_expr: &agg,
            desc: &desc,
        };
        upsert_element(pg, e).await?;
    }
    Ok(())
}

async fn sync_dimensions(pg: &PgPool) -> anyhow::Result<()> {
    // dimension
    let dims: Vec<(String, String, String, Vec<String>, String, String)> = sqlx::query_as(
        "SELECT ds_id, dim_code, name, aliases, expr, description FROM meta.dimension WHERE status = 'active'",
    )
    .fetch_all(pg)
    .await?;
    for (ds, code, name, aliases, expr, desc) in dims {
        let e = Element {
            ds: &ds,
            id: &format!("dimension:{code}"),
            kind: "dimension",
            name: &name,
            aliases: &aliases,
            ref_expr: &expr,
            desc: &desc,
        };
        upsert_element(pg, e).await?;
    }
    Ok(())
}

async fn sync_values(pg: &PgPool) -> anyhow::Result<()> {
    // value（码值也是元素：「已开票」「线下客户」应能向量命中）
    let vals: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT ds_id, table_name, column_name, name, code FROM meta.value_map",
    )
    .fetch_all(pg)
    .await?;
    for (ds, table, col, name, code) in vals {
        let id = format!("value:{table}.{col}:{code}");
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
        upsert_element(pg, e).await?;
    }
    Ok(())
}

async fn sync_terms(pg: &PgPool) -> anyhow::Result<()> {
    // term
    let terms: Vec<(String, String, String, Vec<String>)> = sqlx::query_as(
        "SELECT ds_id, term, definition, aliases FROM meta.term WHERE status = 'active'",
    )
    .fetch_all(pg)
    .await?;
    for (ds, term, def, aliases) in terms {
        let e = Element {
            ds: &ds,
            id: &format!("term:{term}"),
            kind: "term",
            name: &term,
            aliases: &aliases,
            ref_expr: &def,
            desc: "",
        };
        upsert_element(pg, e).await?;
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

/// 单元素幂等 upsert（search_text=名+别名+描述 截 500 字；文本变化时清 embedding 待重建）
async fn upsert_element(pg: &PgPool, e: Element<'_>) -> anyhow::Result<()> {
    let Element { ds, id, kind, name, aliases, ref_expr, desc } = e;
    let search = {
        let mut s = name.to_string();
        if !aliases.is_empty() {
            s.push_str(&format!("（{}）", aliases.join("、")));
        }
        if !desc.is_empty() {
            s.push_str(&format!("：{desc}"));
        }
        s.chars().take(500).collect::<String>()
    };
    sqlx::query(
        "INSERT INTO meta.element(element_id, kind, name, aliases, ref_expr, description, search_text, ds_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
         ON CONFLICT (ds_id, element_id) DO UPDATE SET
           kind=$2, name=$3, aliases=$4, ref_expr=$5, description=$6, search_text=$7,
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
    .execute(pg)
    .await?;
    Ok(())
}
