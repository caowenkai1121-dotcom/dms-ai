//! AGE 图关系问答：客户-购买-商品图。
//! 价值：MySQL 关系查询要全扫 277 万明细（sku_code 无索引，6~20s），
//! 聚合成 9.8 万边建图后，图查询亚秒。0-LLM 确定性。

use sqlx::{MySqlPool, PgPool, Row};

const GRAPH: &str = "dms_graph";

/// AGE 连接准备：每连接需 LOAD age + search_path（放 fetch 前）
async fn age_conn(pg: &PgPool) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Postgres>> {
    let mut conn = pg.acquire().await?;
    sqlx::query("LOAD 'age'").execute(&mut *conn).await?;
    sqlx::query("SET search_path = ag_catalog, public").execute(&mut *conn).await?;
    Ok(conn)
}

fn esc(s: &str) -> String {
    s.replace('\\', "").replace('\'', "\\'")
}

/// 从 MySQL 聚合客户-商品购买边，重建 AGE 图（幂等：先 drop 再建）。
pub async fn sync(mysql: &MySqlPool, pg: &PgPool) -> anyhow::Result<(usize, usize, usize)> {
    // 聚合边（有效订单口径）——扫明细 JOIN，一次性同步
    let edges: Vec<(String, String, String, String, f64, i64)> = sqlx::query_as(
        "SELECT o.customer_code, COALESCE(MAX(o.customer_name),''),
                d.sku_code, COALESCE(MAX(d.sku_name),''),
                CAST(SUM(d.amount) AS DOUBLE), COUNT(*)
         FROM t_sales_order o
         JOIN t_sales_order_detail d ON d.sales_order_code = o.sales_order_code
         WHERE o.deleted_flag = 0 AND o.order_status NOT IN ('0','108','199')
           AND d.deleted_flag = 0 AND d.sku_code IS NOT NULL AND o.customer_code IS NOT NULL
         GROUP BY o.customer_code, d.sku_code",
    )
    .fetch_all(mysql)
    .await?;

    // 去重节点
    use std::collections::HashMap;
    let mut customers: HashMap<String, String> = HashMap::new();
    let mut goods: HashMap<String, String> = HashMap::new();
    for (cc, cn, gc, gn, _, _) in &edges {
        customers.entry(cc.clone()).or_insert_with(|| cn.clone());
        goods.entry(gc.clone()).or_insert_with(|| gn.clone());
    }

    let mut conn = age_conn(pg).await?;
    // 重建图
    let _ = sqlx::query(&format!("SELECT drop_graph('{GRAPH}', true)")).execute(&mut *conn).await;
    sqlx::query(&format!("SELECT create_graph('{GRAPH}')")).execute(&mut *conn).await?;

    // 批量建节点（UNWIND inline）
    batch_nodes(&mut conn, "Customer", &customers).await?;
    batch_nodes(&mut conn, "Goods", &goods).await?;

    // 节点属性索引（建边 MATCH 提速）
    for label in ["Customer", "Goods"] {
        let _ = sqlx::query(&format!(
            "CREATE INDEX IF NOT EXISTS {}_code_idx ON {GRAPH}.\"{label}\" \
             USING btree (agtype_access_operator(VARIADIC ARRAY[properties, '\"code\"'::agtype]))",
            label.to_lowercase()
        ))
        .execute(&mut *conn)
        .await;
    }

    // 批量建边
    batch_edges(&mut conn, &edges).await?;

    Ok((customers.len(), goods.len(), edges.len()))
}

async fn batch_nodes(
    conn: &mut sqlx::PgConnection,
    label: &str,
    nodes: &std::collections::HashMap<String, String>,
) -> anyhow::Result<()> {
    let items: Vec<(&String, &String)> = nodes.iter().collect();
    for chunk in items.chunks(1000) {
        let list: String = chunk
            .iter()
            .map(|(code, name)| format!("{{code:'{}',name:'{}'}}", esc(code), esc(name)))
            .collect::<Vec<_>>()
            .join(",");
        let cy = format!(
            "SELECT * FROM cypher('{GRAPH}', $$ UNWIND [{list}] AS r CREATE (:{label} {{code:r.code, name:r.name}}) $$) AS (v agtype)"
        );
        sqlx::query(&cy).execute(&mut *conn).await?;
    }
    Ok(())
}

async fn batch_edges(
    conn: &mut sqlx::PgConnection,
    edges: &[(String, String, String, String, f64, i64)],
) -> anyhow::Result<()> {
    for chunk in edges.chunks(500) {
        let list: String = chunk
            .iter()
            .map(|(cc, _, gc, _, amt, cnt)| {
                format!("{{c:'{}',g:'{}',a:{:.2},n:{}}}", esc(cc), esc(gc), amt, cnt)
            })
            .collect::<Vec<_>>()
            .join(",");
        let cy = format!(
            "SELECT * FROM cypher('{GRAPH}', $$ UNWIND [{list}] AS e \
             MATCH (c:Customer {{code:e.c}}), (g:Goods {{code:e.g}}) \
             CREATE (c)-[:BOUGHT {{amount:e.a, cnt:e.n}}]->(g) $$) AS (v agtype)"
        );
        sqlx::query(&cy).execute(&mut *conn).await?;
    }
    Ok(())
}

/// agtype 值去引号（AGE 返回 "xxx" 带引号字符串）
fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

pub struct GraphRow {
    pub code: String,
    pub name: String,
    pub amount: f64,
}

/// 买过某商品（名称模糊）的客户 TOP N，按购买额降序
pub async fn buyers_of_goods(pg: &PgPool, goods_name: &str, limit: usize) -> anyhow::Result<Vec<GraphRow>> {
    let cy = format!(
        "SELECT * FROM cypher('{GRAPH}', $$ \
         MATCH (c:Customer)-[b:BOUGHT]->(g:Goods) WHERE g.name =~ '.*{}.*' \
         RETURN c.code, c.name, sum(b.amount) ORDER BY sum(b.amount) DESC LIMIT {limit} \
         $$) AS (code agtype, name agtype, amount agtype)",
        esc(goods_name)
    );
    fetch_graph_rows(pg, &cy).await
}

/// 某客户（名称模糊）买过的商品 TOP N，按购买额降序
pub async fn goods_of_customer(pg: &PgPool, customer_name: &str, limit: usize) -> anyhow::Result<Vec<GraphRow>> {
    let cy = format!(
        "SELECT * FROM cypher('{GRAPH}', $$ \
         MATCH (c:Customer)-[b:BOUGHT]->(g:Goods) WHERE c.name =~ '.*{}.*' \
         RETURN g.code, g.name, sum(b.amount) ORDER BY sum(b.amount) DESC LIMIT {limit} \
         $$) AS (code agtype, name agtype, amount agtype)",
        esc(customer_name)
    );
    fetch_graph_rows(pg, &cy).await
}

/// 买过 X 商品的客户还买了什么（共购推荐）：两跳
pub async fn copurchase(pg: &PgPool, goods_name: &str, limit: usize) -> anyhow::Result<Vec<GraphRow>> {
    let cy = format!(
        "SELECT * FROM cypher('{GRAPH}', $$ \
         MATCH (c:Customer)-[:BOUGHT]->(g1:Goods) WHERE g1.name =~ '.*{}.*' \
         MATCH (c)-[b2:BOUGHT]->(g2:Goods) WHERE NOT g2.name =~ '.*{}.*' \
         RETURN g2.code, g2.name, sum(b2.amount) ORDER BY sum(b2.amount) DESC LIMIT {limit} \
         $$) AS (code agtype, name agtype, amount agtype)",
        esc(goods_name), esc(goods_name)
    );
    fetch_graph_rows(pg, &cy).await
}

async fn fetch_graph_rows(pg: &PgPool, cypher: &str) -> anyhow::Result<Vec<GraphRow>> {
    let mut conn = age_conn(pg).await?;
    // agtype 类型 sqlx 不识别，外层包一层 ::text（string→带引号JSON、number→裸数字）
    let wrapped = format!("SELECT code::text, name::text, amount::text FROM ({cypher}) AS sub");
    let rows = sqlx::query(&wrapped).fetch_all(&mut *conn).await?;
    Ok(rows
        .iter()
        .map(|r| {
            let code: String = r.try_get::<Option<String>, _>(0).ok().flatten().unwrap_or_default();
            let name: String = r.try_get::<Option<String>, _>(1).ok().flatten().unwrap_or_default();
            let amt_s: String = r.try_get::<Option<String>, _>(2).ok().flatten().unwrap_or_default();
            GraphRow {
                code: unquote(&code),
                name: unquote(&name),
                amount: amt_s.trim().parse().unwrap_or(0.0),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_quotes() {
        assert_eq!(esc("O'Brien"), "O\\'Brien");
    }

    #[test]
    fn unquote_agtype() {
        assert_eq!(unquote("\"恒众\""), "恒众");
        assert_eq!(unquote("恒众"), "恒众");
    }
}
