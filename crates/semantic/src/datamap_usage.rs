//! 使用轨迹校准（DataLink 思想：静态推断打底图，真实使用轨迹校准置信度）。
//!
//! 数据源是 `meta.query_log` 近 N 天 `status='succeeded'` 的行：从 SQL 提取
//! **表级 JOIN 对**（`JOIN ... ON a.x=b.y` 两侧表；逗号 FROM 各项两两）与
//! **同现列对**（同一句 SELECT/WHERE/HAVING 共同出现的列，别名解析成 `table.col`），
//! 聚合成 `co_occurs` 证据 upsert 进 `meta.datamap_edge`。
//!
//! ## 表形约定（🔴 裁决已落地：三处 DDL 逐字一致）
//! `meta.datamap_edge` 由三处共用：server `datamap_api`（复核域，DDL 正本）、
//! `datamap.rs`（静态推断）、本模块（使用轨迹）。唯一键 =
//! **(ds_id, kind, left_table, left_col, right_table, right_col)**（`idx_datamap_edge_uniq`
//! 唯一索引，ON CONFLICT 的仲裁）。usage 来源的边一律 `kind='co_occurs'`：JOIN 表对 →
//! (left_table, '', right_table, '')；同现列对 → 两侧 `table.col` 拆开（未带表前缀的裸列
//! 无法归属，跳过不落地）。建表幂等、入口自确保，不依赖 `ddl::migrate` 顺序。
//! status 不在 upsert 的 SET 列表里 —— 人工复核结论不被校准轮冲掉。
//!
//! ## 合并公式
//! `merged = 0.6 × 既有 confidence + 0.4 × 归一化频次`（`UPSERT_SQL` 字面量是
//! `merged_confidence` 的 SQL 镜像，单测两边都钉）。归一化频次 = 该对命中的成功查询数
//! ÷ 本轮最大命中数；新边（无静态部分）= `0.4 × 归一化频次`。重跑 = 向最新观测做指数
//! 衰减校准，天然幂等可重入。
//!
//! ## 边界（刻意从简：校准信号，不是事实判定）
//! - 语句级作用域：子查询的表/列并入所属语句；同名别名跨子查询后者覆盖前者。
//! - 列提取覆盖 SELECT/WHERE/HAVING 与 CASE 表达式；GROUP BY/ORDER BY 不收
//!   （分组/排序键不是取值共现信号，收了只会稀释列对）。
//! - 方言双试：先 MySQL 后 PG（kernel 方言实例，单一事实源）；都失败记 `ParseFailure`
//!   进报告（失败留痕），一条脏 SQL 不许炸掉整轮。
//! - 单语句列数 > `MAX_COLS_PER_STATEMENT` 只记 JOIN 对（宽 SELECT 的列对是 O(n²) 噪声）。
//! - 非 `Statement::Query` 跳过（query_log 过了只读红线，正常不会有）。
//!
//! 调用点：CLI 子命令 `meta datamap-calibrate [days]`（main.rs 已接线）：`calibrate_from_query_log(&pg, 30).await`。

use std::collections::{HashMap, HashSet};

use serde_json::json;
use sqlparser::ast::{
    BinaryOperator, Expr, JoinConstraint, JoinOperator, Query, Select, SelectItem, SetExpr,
    Statement, TableFactor,
};
use sqlparser::parser::Parser;
use sqlx::PgPool;

/// 合并公式权重（`UPSERT_SQL` 字面量必须与之同步，单测钉着）
const STATIC_WEIGHT: f64 = 0.6;
const USAGE_WEIGHT: f64 = 0.4;
const MAX_EDGES_PER_RUN: usize = 500; // 每数据源回写上限（命中数降序截断，防爆；per-ds 各自截断）
const MAX_COLS_PER_STATEMENT: usize = 24; // 单语句参与配对的列数上限
const ROW_SCAN_CAP: i64 = 20_000; // 单轮扫描行上限（取最近 —— 频次偏向近期，刻意的）
const FAILURE_KEEP: usize = 50; // 报告留存的失败样本数（total 照计全量）
const ERR_CLIP: usize = 200; // 失败原因截断上限（字符，非字节）

/// 边表 DDL（唯一键约定见头注）。幂等：`ensure_edge_table` 每次入口都跑。
/// pub(crate)：`datamap.rs` 的 `datamap_ddl_matches_usage_ddl` 测试钉两份逐字一致。
pub(crate) const DDL: &str = r#"
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

/// `merged_confidence` 的 SQL 镜像：$6 = `0.4 × 归一化频次`（= 新边值）。命中既有行
/// （上轮校准的同对边）→ `0.6×旧 + $6`；evidence 记本轮观测，seen_count 累加历史命中。
/// status 不进 SET：人工复核（accepted/rejected）的结论不被下一轮校准冲掉。
/// `last_seen`/`seen_count` 是「被真实查询观测到」的口径，独归本写口维护（静态推断与
/// 血缘写口都不刷 —— 三处写口的分工钉在这里）；`updated_at` 与另两处写口同款照刷。
const UPSERT_SQL: &str = "INSERT INTO meta.datamap_edge(ds_id, kind, left_table, left_col, right_table, right_col, confidence, evidence, seen_count, status)
  VALUES ($1, 'co_occurs', $2, $3, $4, $5, $6, $7, $8, 'pending')
  ON CONFLICT (ds_id, kind, left_table, left_col, right_table, right_col) DO UPDATE SET
    confidence = LEAST(1.0, GREATEST(0.0, 0.6 * meta.datamap_edge.confidence + EXCLUDED.confidence)),
    evidence = EXCLUDED.evidence,
    seen_count = meta.datamap_edge.seen_count + EXCLUDED.seen_count,
    last_seen = now(),
    updated_at = now()";

/// 一轮校准的落点报告（失败留痕在这里；调用方打日志/落审计自取）。
#[derive(Debug)]
pub struct UsageReport {
    pub window_days: u32,
    pub rows_scanned: usize,
    pub rows_parsed: usize,
    /// 解析失败总行数（样本只留 `FAILURE_KEEP` 条）
    pub parse_failure_total: usize,
    pub parse_failures: Vec<ParseFailure>,
    /// distinct JOIN 表对 / 同现列对数（截断前）
    pub join_edges: usize,
    pub col_edges: usize,
    pub edges_upserted: usize,
}

/// 一条解析失败的留痕（哪行日志、什么原因）。
#[derive(Debug, Clone)]
pub struct ParseFailure {
    pub log_id: i64,
    pub error: String,
}

/// 近 `days` 天成功行 → 聚合 → upsert `meta.datamap_edge`，返回本轮报告。
pub async fn calibrate_from_query_log(pg: &PgPool, days: u32) -> anyhow::Result<UsageReport> {
    let days = days.max(1);
    ensure_edge_table(pg).await?;
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id, sql, ds_id FROM meta.query_log
         WHERE status = 'succeeded' AND sql <> ''
           AND at >= now() - make_interval(days => $1)
         ORDER BY id DESC LIMIT $2",
    )
    .bind(days.min(i32::MAX as u32) as i32)
    .bind(ROW_SCAN_CAP)
    .fetch_all(pg)
    .await?;
    let by_ds = aggregate(&rows);
    let failure_total: usize = by_ds.values().map(|a| a.failure_total).sum();
    if failure_total > 0 {
        // per-ds 分布一并透出：多数据源部署时定位是哪条源的日志脏
        let mut per_ds: Vec<(&str, usize)> = by_ds
            .iter()
            .filter(|(_, a)| a.failure_total > 0)
            .map(|(ds, a)| (ds.as_str(), a.failure_total))
            .collect();
        per_ds.sort();
        tracing::warn!(total = failure_total, by_ds = ?per_ds, "使用轨迹校准：有成功行解析失败（样本已留痕于报告）");
    }
    let edges = edges_of(&by_ds, days);
    // 整轮回写包一个事务：中途失败整体回滚，重跑即收敛（与静态推断写口同形态）。
    let mut tx = pg.begin().await?;
    for e in &edges {
        sqlx::query(UPSERT_SQL)
            .bind(&e.ds)
            .bind(&e.left_table)
            .bind(&e.left_col)
            .bind(&e.right_table)
            .bind(&e.right_col)
            .bind(e.usage)
            .bind(&e.evidence)
            .bind(i64::from(e.freq))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    tracing::info!(rows = rows.len(), edges = edges.len(), "使用轨迹校准完成");
    Ok(UsageReport {
        window_days: days,
        rows_scanned: rows.len(),
        rows_parsed: by_ds.values().map(|a| a.parsed).sum(),
        parse_failure_total: failure_total,
        parse_failures: by_ds
            .values()
            .flat_map(|a| a.failures.iter().cloned())
            .take(FAILURE_KEEP)
            .collect(),
        join_edges: by_ds.values().map(|a| a.join_counts.len()).sum(),
        col_edges: by_ds.values().map(|a| a.col_counts.len()).sum(),
        edges_upserted: edges.len(),
    })
}

/// 幂等建表（warehouse_catalog `ensure_snapshot_table` 同款纪律）。
async fn ensure_edge_table(pg: &PgPool) -> anyhow::Result<()> {
    for stmt in DDL.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(pg).await?;
    }
    Ok(())
}

/// 方言双试（kernel 的方言实例）：先 MySQL 后 PG，都挂 → Err（调用方留痕）。
fn parse(sql: &str) -> Result<Vec<Statement>, String> {
    // 方言对象在函数入口各取一次（注册表查找 + expect 不随每行日志重复）
    let mysql = dms_kernel::by_name("mysql").expect("mysql 方言常驻");
    let pg = dms_kernel::by_name("pg").expect("pg 方言常驻");
    match Parser::parse_sql(mysql.parser(), sql) {
        Ok(stmts) => Ok(stmts),
        Err(_) => Parser::parse_sql(pg.parser(), sql).map_err(|e| e.to_string()),
    }
}

/// 一轮校准的内存聚合（纯函数，单测全在这里打）。
#[derive(Default, Debug)]
struct Aggregate {
    join_counts: HashMap<(String, String), u32>,
    col_counts: HashMap<(String, String), u32>,
    parsed: usize,
    failures: Vec<ParseFailure>,
    failure_total: usize,
}

/// 按行自带的 ds_id 分源聚合：不同数据源的查询日志不混进同一张地图。
/// ds 先 trim + 小写归一（'DMS' 与 'dms' 不裂成两组），空串归 'dms'（与 DDL 默认值同口径）；
/// 全空白 SQL 进 parse 必然失败，只会虚增 parse_failure_total —— 直接跳过不计。
fn aggregate(rows: &[(i64, String, String)]) -> HashMap<String, Aggregate> {
    let mut by_ds: HashMap<String, Aggregate> = HashMap::new();
    for (id, sql, ds) in rows {
        if sql.trim().is_empty() {
            continue;
        }
        let ds = ds.trim().to_ascii_lowercase();
        let key = if ds.is_empty() { "dms".to_string() } else { ds };
        let agg = by_ds.entry(key).or_default();
        match parse(sql) {
            Ok(stmts) => {
                let (joins, cols) = extract(&stmts);
                agg.parsed += 1;
                for p in joins {
                    *agg.join_counts.entry(p).or_insert(0) += 1;
                }
                for p in cols {
                    *agg.col_counts.entry(p).or_insert(0) += 1;
                }
            }
            Err(e) => {
                agg.failure_total += 1;
                if agg.failures.len() < FAILURE_KEEP {
                    agg.failures.push(ParseFailure { log_id: *id, error: clip(&e) });
                }
            }
        }
    }
    by_ds
}

/// 一条待回写的边（usage 已按合并公式的新边形态取值）。裸表名+列名拆开落库
/// （JOIN 表对的列留空）；ds 取自产生它的 query_log 行。
#[derive(Debug)]
struct Edge {
    ds: String,
    left_table: String,
    left_col: String,
    right_table: String,
    right_col: String,
    usage: f64,
    freq: u32,
    evidence: String,
}

/// `table.col` → (table, col)。未带表前缀的裸列（单表查询没写前缀）无法归属 → None 跳过。
fn split_col_ref(s: &str) -> Option<(String, String)> {
    let (t, c) = s.rsplit_once('.')?;
    (!t.is_empty() && !c.is_empty()).then(|| (t.to_string(), c.to_string()))
}

fn edges_of(by_ds: &HashMap<String, Aggregate>, days: u32) -> Vec<Edge> {
    let mut out = Vec::new();
    // ds 名升序：输出顺序确定（截断边界同理），不因 HashMap 迭代序漂移
    let mut dss: Vec<&String> = by_ds.keys().collect();
    dss.sort();
    for ds in dss {
        let agg = &by_ds[ds];
        // 先收敛「可落库」集合：带裸列的列对无法归属到表，丢弃；被丢弃对的频次也不再
        // 计入归一化分母（原来 max 在过滤前算，会把可落库边的 norm 系统性压小）。
        let mut all: Vec<(u32, String, String)> = Vec::new();
        for ((a, b), &freq) in &agg.join_counts {
            all.push((freq, a.clone(), b.clone()));
        }
        for ((a, b), &freq) in &agg.col_counts {
            if split_col_ref(a).is_some() && split_col_ref(b).is_some() {
                all.push((freq, a.clone(), b.clone()));
            }
        }
        let max = all.iter().map(|e| e.0).max().unwrap_or(0);
        // 命中数降序、对名升序：截断边界确定性（同分不因迭代序漂移）
        all.sort_by(|x, y| y.0.cmp(&x.0).then_with(|| (&x.1, &x.2).cmp(&(&y.1, &y.2))));
        all.truncate(MAX_EDGES_PER_RUN);
        out.extend(all.into_iter().map(|(freq, a, b)| {
            let norm = normalized(freq, max);
            // 落库四元组在收敛后拆解：列对拆 table.col，JOIN 表对（键里无点）列留空
            let (lt, lc, rt, rc) = match (split_col_ref(&a), split_col_ref(&b)) {
                (Some((ta, ca)), Some((tb, cb))) => (ta, ca, tb, cb),
                _ => (a, String::new(), b, String::new()),
            };
            Edge {
                ds: ds.clone(),
                left_table: lt,
                left_col: lc,
                right_table: rt,
                right_col: rc,
                usage: merged_confidence(None, norm),
                freq,
                evidence: json!({
                    "source": "query_log",
                    "window_days": days,
                    "freq": freq,
                    "norm": (norm * 1e4).round() / 1e4,
                })
                .to_string(),
            }
        }));
    }
    out
}

/// 归一化频次：命中数 ÷ 本轮最大命中数（最大者为 1.0）。空轮 → 0。
fn normalized(count: u32, max: u32) -> f64 {
    if max == 0 { 0.0 } else { f64::from(count) / f64::from(max) }
}

/// 合并公式（`UPSERT_SQL` 的 Rust 镜像）：既有边 = 静态 0.6 + 使用 0.4×归一化频次；
/// 新边（existing=None）静态部分为 0。钳到 [0,1]。
fn merged_confidence(existing: Option<f64>, norm: f64) -> f64 {
    (STATIC_WEIGHT * existing.unwrap_or(0.0) + USAGE_WEIGHT * norm).clamp(0.0, 1.0)
}

/// 按字符截断（query_log::clip 同款理由：按字节截会把中文切成半个字）
fn clip(s: &str) -> String {
    s.chars().take(ERR_CLIP).collect()
}

/// 一条 SQL（可多语句）→（JOIN 表对集，同现列对集）。对按字典序规整 = 无向边。
fn extract(stmts: &[Statement]) -> (HashSet<(String, String)>, HashSet<(String, String)>) {
    let mut raw = Raw::default();
    for s in stmts {
        if let Statement::Query(q) = s {
            raw.walk_query(q);
        }
    }
    let resolve = |p: &str| raw.aliases.get(p).cloned().unwrap_or_else(|| p.to_string());
    let live = |t: &str| !t.is_empty() && !raw.ctes.contains(t);
    let mut joins = HashSet::new();
    for (a, b) in raw.join_pairs.iter().map(|(a, b)| (resolve(a), resolve(b))) {
        if live(&a) && live(&b) && a != b {
            joins.insert(if a < b { (a, b) } else { (b, a) });
        }
    }
    let mut keys = HashSet::new();
    for (prefix, col) in &raw.cols {
        if col.is_empty() {
            continue;
        }
        match prefix {
            Some(p) => {
                let t = resolve(p);
                if live(&t) {
                    keys.insert(format!("{t}.{col}"));
                }
            }
            None => {
                keys.insert(col.clone());
            }
        }
    }
    let mut cols = HashSet::new();
    if keys.len() <= MAX_COLS_PER_STATEMENT {
        let mut sorted: Vec<&String> = keys.iter().collect();
        sorted.sort();
        for (i, a) in sorted.iter().enumerate() {
            for b in &sorted[i + 1..] {
                cols.insert(((*a).clone(), (*b).clone()));
            }
        }
    }
    (joins, cols)
}

/// 单条 SQL 的原始提取物（前缀留到 extract 末尾统一解析别名，CTE 同理末尾排除）。
#[derive(Default)]
struct Raw {
    /// 全部 CTE 名（语句级近似，见头注）
    ctes: HashSet<String>,
    /// 前缀（别名或表名 lower）→ 真实表名 lower
    aliases: HashMap<String, String>,
    /// JOIN 对：ON 等值推出的是（前缀A, 前缀B)，结构性兜底/逗号 FROM 推出的是（表名, 表名）——
    /// 末尾统一过 resolve（别名表查不到就原样返回，表名因此不受影响）
    join_pairs: Vec<(String, String)>,
    /// (前缀, 列) 原始引用
    cols: Vec<(Option<String>, String)>,
}

impl Raw {
    /// 登记 FROM 一项：实表记别名并返回表名；派生表递归走进去；其余返回 None。
    fn register(&mut self, tf: &TableFactor) -> Option<String> {
        match tf {
            TableFactor::Table { name, alias, .. } => {
                let table = name.0.last().map(|p| p.value.to_lowercase()).unwrap_or_default();
                if table.is_empty() {
                    return None;
                }
                let key = alias
                    .as_ref()
                    .map(|a| a.name.value.to_lowercase())
                    .unwrap_or_else(|| table.clone());
                self.aliases.insert(key, table.clone());
                Some(table)
            }
            TableFactor::Derived { subquery, .. } => {
                self.walk_query(subquery);
                None
            }
            _ => None,
        }
    }

    fn walk_query(&mut self, q: &Query) {
        if let Some(with) = &q.with {
            for cte in &with.cte_tables {
                self.ctes.insert(cte.alias.name.value.to_lowercase());
                self.walk_query(&cte.query);
            }
        }
        self.walk_set_expr(&q.body);
    }

    fn walk_set_expr(&mut self, body: &SetExpr) {
        match body {
            SetExpr::Select(s) => self.walk_select(s),
            SetExpr::Query(q) => self.walk_query(q),
            SetExpr::SetOperation { left, right, .. } => {
                self.walk_set_expr(left);
                self.walk_set_expr(right);
            }
            _ => {}
        }
    }

    fn walk_select(&mut self, s: &Select) {
        let mut item_tables = Vec::new();
        for twj in &s.from {
            let base = self.register(&twj.relation);
            if let Some(t) = &base {
                item_tables.push(t.clone());
            }
            let mut last = base;
            for j in &twj.joins {
                let right = self.register(&j.relation);
                let before = self.join_pairs.len();
                // 只认 Inner/Left/Right/Full 的 ON 等值；Semi/Anti/CrossApply 等其余 JOIN
                // 类型一律 None → 走下面的结构性兜底（刻意的近似，见头注边界节）
                let on = match &j.join_operator {
                    JoinOperator::Inner(JoinConstraint::On(e))
                    | JoinOperator::LeftOuter(JoinConstraint::On(e))
                    | JoinOperator::RightOuter(JoinConstraint::On(e))
                    | JoinOperator::FullOuter(JoinConstraint::On(e)) => Some(e),
                    _ => None,
                };
                if let Some(e) = on {
                    on_eq_pairs(e, &mut self.join_pairs);
                }
                if self.join_pairs.len() == before {
                    if let (Some(l), Some(r)) = (&last, &right) {
                        if l != r {
                            self.join_pairs.push((l.clone(), r.clone()));
                        }
                    }
                }
                last = right.or(last);
            }
        }
        // 逗号 FROM（隐式交叉连接）：各项两两共现
        for (i, a) in item_tables.iter().enumerate() {
            for b in &item_tables[i + 1..] {
                if a != b {
                    self.join_pairs.push((a.clone(), b.clone()));
                }
            }
        }
        for item in &s.projection {
            match item {
                SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                    collect_cols(e, &mut self.cols);
                }
                _ => {}
            }
        }
        if let Some(sel) = &s.selection {
            collect_cols(sel, &mut self.cols);
        }
        // HAVING 与 SELECT/WHERE 同一口径照收（GROUP BY/ORDER BY 刻意不收，见头注边界节）
        if let Some(having) = &s.having {
            collect_cols(having, &mut self.cols);
        }
    }
}

/// JOIN ON 里的等值对：两侧都是带前缀列引用 → 记 (前缀A, 前缀B)。同前缀（表内条件）不算。
fn on_eq_pairs(e: &Expr, out: &mut Vec<(String, String)>) {
    if let Expr::BinaryOp { left, op, right } = e {
        if *op == BinaryOperator::Eq {
            if let (Expr::CompoundIdentifier(a), Expr::CompoundIdentifier(b)) =
                (left.as_ref(), right.as_ref())
            {
                if a.len() >= 2 && b.len() >= 2 {
                    let pa = a[a.len() - 2].value.to_lowercase();
                    let pb = b[b.len() - 2].value.to_lowercase();
                    if pa != pb {
                        out.push((pa, pb));
                    }
                }
            }
        }
        on_eq_pairs(left, out);
        on_eq_pairs(right, out);
    } else if let Expr::Nested(inner) = e {
        on_eq_pairs(inner, out);
    }
}

/// 收集表达式里的列引用（末段小写；带前缀的记前缀供别名解析）。
/// 覆盖面与 kernel `collect_where_cols` 同款（只读遍历，不进子查询内部），输出多带前缀。
fn collect_cols(e: &Expr, out: &mut Vec<(Option<String>, String)>) {
    match e {
        Expr::Identifier(i) => out.push((None, i.value.to_lowercase())),
        Expr::CompoundIdentifier(p) => {
            if let Some(col) = p.last() {
                let prefix = p.len().checked_sub(2).map(|i| p[i].value.to_lowercase());
                out.push((prefix, col.value.to_lowercase()));
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_cols(left, out);
            collect_cols(right, out);
        }
        Expr::UnaryOp { expr, .. } | Expr::Nested(expr) | Expr::Cast { expr, .. } => {
            collect_cols(expr, out);
        }
        // CASE WHEN 在 SELECT 里极常见：操作数/条件/结果/ELSE 全遍历
        Expr::Case { operand, conditions, results, else_result } => {
            if let Some(operand) = operand {
                collect_cols(operand, out);
            }
            for e in conditions.iter().chain(results) {
                collect_cols(e, out);
            }
            if let Some(else_result) = else_result {
                collect_cols(else_result, out);
            }
        }
        Expr::InList { expr, list, .. } => {
            collect_cols(expr, out);
            for item in list {
                collect_cols(item, out);
            }
        }
        Expr::InSubquery { expr, .. } => collect_cols(expr, out),
        Expr::Between { expr, low, high, .. } => {
            collect_cols(expr, out);
            collect_cols(low, out);
            collect_cols(high, out);
        }
        Expr::IsNull(x) | Expr::IsNotNull(x) => collect_cols(x, out),
        Expr::Like { expr, pattern, .. } | Expr::ILike { expr, pattern, .. } => {
            collect_cols(expr, out);
            collect_cols(pattern, out);
        }
        Expr::Function(f) => {
            if let sqlparser::ast::FunctionArguments::List(l) = &f.args {
                for a in &l.args {
                    if let sqlparser::ast::FunctionArg::Unnamed(
                        sqlparser::ast::FunctionArgExpr::Expr(e),
                    ) = a
                    {
                        collect_cols(e, out);
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(sql: &str) -> (Vec<(String, String)>, Vec<(String, String)>) {
        let (j, c) = extract(&parse(sql).expect("测试 SQL 必须能解析"));
        (j.into_iter().collect(), c.into_iter().collect())
    }

    /// JOIN 对提取：ON 等值两侧解析别名成表；链式逐段出边；逗号 FROM 两两共现；
    /// 自连与 CTE 引用不出边；无 ON 等值走结构性兜底。
    #[test]
    fn join_pairs_resolve_aliases_and_skip_self_and_cte() {
        let (j, _) = pairs(
            "SELECT * FROM t_order o JOIN t_customer c ON o.customer_code = c.customer_code
             JOIN t_shop s ON s.shop_code = o.shop_code",
        );
        assert_eq!(j.len(), 2, "ON 列上的第三张表不许拉郎配: {j:?}");
        assert!(
            j.contains(&("t_customer".into(), "t_order".into()))
                && j.contains(&("t_order".into(), "t_shop".into())),
            "{j:?}"
        );
        let (j2, _) = pairs("SELECT * FROM t_x x, t_y y WHERE x.id = y.id");
        assert_eq!(j2, vec![("t_x".to_string(), "t_y".to_string())], "逗号 FROM 两两共现");
        let (j3, _) = pairs("SELECT * FROM t_a x JOIN t_a y ON x.id = y.pid");
        assert!(j3.is_empty(), "自连不出边: {j3:?}");
        let (j4, _) =
            pairs("WITH w AS (SELECT b.id FROM t_b b) SELECT * FROM w JOIN t_c ON w.id = t_c.id");
        assert!(j4.is_empty(), "CTE 引用不许出边: {j4:?}");
        let (j5, _) = pairs("SELECT * FROM t_p NATURAL JOIN t_q");
        assert_eq!(j5, vec![("t_p".to_string(), "t_q".to_string())], "无 ON 等值 → 结构性兜底");
    }

    /// 同现列对：SELECT/WHERE 同句共现；别名解析成 table.col；JOIN ON 的列不算
    /// （规格只认 WHERE/SELECT）；超宽语句不配列对。
    #[test]
    fn column_pairs_come_from_select_and_where_only() {
        let (_, c) = pairs(
            "SELECT o.order_date, c.customer_name FROM t_order o JOIN t_customer c
             ON o.customer_code = c.customer_code
             WHERE o.status NOT IN ('0') AND o.amount > 0",
        );
        assert_eq!(c.len(), 6, "4 列两两配对: {c:?}");
        assert!(c.contains(&("t_customer.customer_name".into(), "t_order.order_date".into())), "{c:?}");
        assert!(c.contains(&("t_order.amount".into(), "t_order.status".into())), "{c:?}");
        assert!(
            c.iter().all(|(a, b)| !a.contains("customer_code") && !b.contains("customer_code")),
            "JOIN ON 的列不许进同现列对: {c:?}"
        );
        let wide = format!(
            "SELECT {} FROM t_w",
            (1..=30).map(|i| format!("c{i}")).collect::<Vec<_>>().join(", ")
        );
        assert!(pairs(&wide).1.is_empty(), "超过 {MAX_COLS_PER_STATEMENT} 列不配列对");
    }

    /// 频次归一与合并公式：最大者归一到 1.0；静态 0.6 + 使用 0.4×归一；新边静态部分为 0；
    /// 钳到 [0,1]；SQL 镜像与常量不许漂移。
    #[test]
    fn normalization_and_merge_formula_are_pinned() {
        assert_eq!((normalized(3, 6), normalized(0, 0)), (0.5, 0.0), "空轮不许除零");
        assert_eq!((STATIC_WEIGHT, USAGE_WEIGHT), (0.6, 0.4));
        assert!((merged_confidence(Some(0.9), 0.5) - 0.74).abs() < 1e-12, "0.6×0.9 + 0.4×0.5");
        assert!((merged_confidence(None, 1.0) - 0.4).abs() < 1e-12, "新边没有静态部分");
        assert_eq!(merged_confidence(Some(1.0), 1.0), 1.0, "钳到 1.0");
        assert!(
            UPSERT_SQL.contains("0.6 * meta.datamap_edge.confidence"),
            "UPSERT_SQL 与 STATIC_WEIGHT 漂移了"
        );
    }

    /// 失败留痕：坏行记 ParseFailure（log_id + 原因），好行照常聚合 —— 一条脏 SQL
    /// 不许炸掉整轮校准。
    #[test]
    fn parse_failure_leaves_trace_and_good_rows_still_count() {
        let good = "SELECT * FROM t_a a JOIN t_b b ON a.id = b.aid".to_string();
        let rows = vec![
            (1i64, good.clone(), "dms".to_string()),
            (2, "SELCT ** FROM ((( ".to_string(), "dms".to_string()),
            (3, good, "dms".to_string()),
        ];
        let by_ds = aggregate(&rows);
        let agg = by_ds.get("dms").expect("dms 聚合必须在");
        assert_eq!((agg.parsed, agg.failure_total, agg.failures.len()), (2, 1, 1));
        assert_eq!(agg.failures[0].log_id, 2);
        assert!(!agg.failures[0].error.is_empty(), "失败原因不许为空");
        assert_eq!(
            agg.join_counts.get(&("t_a".into(), "t_b".into())),
            Some(&2),
            "两条成功行各贡献一次命中"
        );
        let edges = edges_of(&by_ds, 30);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].ds, "dms", "ds 取自 query_log 行");
        assert_eq!(
            (edges[0].left_table.as_str(), edges[0].left_col.as_str(), edges[0].right_table.as_str(), edges[0].right_col.as_str()),
            ("t_a", "", "t_b", ""),
            "JOIN 表对：列留空"
        );
        assert!((edges[0].usage - 0.4).abs() < 1e-12, "唯一一对归一到 1.0，新边 = 0.4");
        assert!(edges[0].evidence.contains("\"freq\":2"), "evidence 记本轮频次: {}", edges[0].evidence);
    }

    /// 同现列对拆 table.col 落库；未带表前缀的裸列无法归属 → 跳过不落地
    #[test]
    fn column_pairs_split_and_bare_columns_are_skipped() {
        let rows = vec![(
            1i64,
            "SELECT o.order_date, o.status, remark FROM t_order o WHERE o.amount > 0".to_string(),
            "dms".to_string(),
        )];
        let by_ds = aggregate(&rows);
        let edges = edges_of(&by_ds, 30);
        assert!(edges.iter().all(|e| e.left_table == "t_order"), "带前缀列都归属 t_order: {edges:?}");
        assert!(
            edges.iter().all(|e| !e.left_col.is_empty() && !e.right_col.is_empty()),
            "裸列 remark 参与的列对不许落地: {edges:?}"
        );
        assert_eq!(edges.len(), 3, "3 个带前缀列两两配对: {edges:?}");
    }

    /// ds 归一与空白行：' DMS '/'dms' 不裂成两组；全空白 SQL 跳过不计（不虚增失败数）。
    #[test]
    fn aggregate_normalizes_ds_and_skips_blank_sql() {
        let rows = vec![
            (1i64, "SELECT * FROM t_a".to_string(), " DMS ".to_string()),
            (2, "SELECT * FROM t_b".to_string(), "dms".to_string()),
            (3, "   ".to_string(), "upload_9".to_string()),
        ];
        let by_ds = aggregate(&rows);
        assert_eq!(by_ds.len(), 1, "大小写/空白 ds 必须归并：{by_ds:?}");
        let agg = by_ds.get("dms").expect("归一到 dms");
        assert_eq!(agg.parsed, 2, "两条有效行都解析：{agg:?}");
        assert_eq!(agg.failure_total, 0, "全空白行不计入解析失败");
    }

    /// 列提取覆盖：CASE WHEN 里的列、HAVING 里的列都进同现列对；max 在过滤裸列后算。
    #[test]
    fn case_and_having_columns_are_collected() {
        let rows = vec![(
            1i64,
            "SELECT CASE WHEN o.status = '1' THEN o.amount ELSE 0 END AS amt FROM t_order o \
             GROUP BY o.status, o.amount HAVING sum(o.qty) > 0"
                .to_string(),
            "dms".to_string(),
        )];
        let by_ds = aggregate(&rows);
        let agg = by_ds.get("dms").unwrap();
        let cols: Vec<&String> = agg.col_counts.keys().flat_map(|(a, b)| [a, b]).collect();
        for want in ["t_order.status", "t_order.amount", "t_order.qty"] {
            assert!(cols.iter().any(|c| c.as_str() == want), "CASE/HAVING 列 {want} 必须收进：{cols:?}");
        }
    }

    /// 建表 DDL 幂等纪律（与 query_log::migrate 同一判据）：每句都可重复执行；
    /// 唯一键六元组 (ds_id,kind,left_table,left_col,right_table,right_col) 约定钉死、upsert 与之一致。
    #[test]
    fn ddl_statements_are_idempotent() {
        for stmt in DDL.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            assert!(stmt.contains("IF NOT EXISTS"), "非幂等语句: {stmt}");
        }
        assert!(
            DDL.contains("idx_datamap_edge_uniq ON meta.datamap_edge(ds_id, kind, left_table, left_col, right_table, right_col)"),
            "唯一键约定丢了"
        );
        assert!(
            UPSERT_SQL.contains("ON CONFLICT (ds_id, kind, left_table, left_col, right_table, right_col)"),
            "upsert 与唯一键不一致"
        );
        assert!(UPSERT_SQL.contains("'co_occurs'"), "usage 来源的边 kind='co_occurs' 钉死");
        assert!(
            UPSERT_SQL.contains("updated_at = now()"),
            "updated_at 与另两处写口同款照刷（last_seen/seen_count 独归本写口）"
        );
    }
}
