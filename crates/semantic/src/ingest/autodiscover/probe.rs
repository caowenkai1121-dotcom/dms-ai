//! A1 第一段：**探测**。生产字典全量 + 码型后缀候选列 + 人工种子已覆盖判定 +
//! 只读 DISTINCT 抽样（码型 ≤61 / 名称型值域 ≤2000，单探针 10s 超时）。变更原因＝候选与抽样口径。
//!
//! 搬运源 `server/src/meta.rs:1329-1463`（四条加载 SQL、探针 SQL、`probe_scoped` 逐字保留）。

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use dms_connector::{mysql::ReadOnlyMySql, source::SqlSource};
use dms_kernel::sql::guard::GuardConfig;
use dms_kernel::UnrestrictedProof;
use sqlx::PgPool;

use crate::registry::{ds_pred, ds_pred_at};

/// 探针闸门的两件凭证。**`UnrestrictedProof` 由调用方铸造**（铸造点必须留在能判定
/// 「这是管理任务、没有『以谁的身份查』这回事」的那一层，semantic 判不了）。
pub struct ProbeGate<'a> {
    pub proof: &'a UnrestrictedProof,
    pub guard: &'a GuardConfig,
}

/// 一个待抽样的码列。`has_del` = 该表有 `deleted_flag` 列（拼 WHERE 用；部分表无此列）。
pub struct ColRef<'a> {
    pub table: &'a str,
    pub col: &'a str,
    pub has_del: bool,
}

/// 人工种子已覆盖的 `(表, 列)`：`value_map` 直接登记的 + `dimension.expr` 提及的。
pub struct Manual {
    vm: HashSet<(String, String)>,
    dims: Vec<(String, String)>,
}

impl Manual {
    /// 人工优先：已覆盖即跳过（自动发现绝不覆盖手工口径）
    pub fn covers(&self, table: &str, col: &str) -> bool {
        let key = (table.to_lowercase(), col.to_lowercase());
        self.vm.contains(&key)
            || self.dims.iter().any(|(src, expr)| src.contains(table) && expr.contains(col))
    }
}

/// 1. 生产字典（t_dict_key/value，全量小表）。字面量通道，不经 LLM。
pub async fn load_dicts(
    mysql: &ReadOnlyMySql,
) -> anyhow::Result<HashMap<String, (String, Vec<(String, String)>)>> {
    let dict_rows: Vec<(String, String, String, String)> = mysql
        .fixed(
            "SELECT CAST(k.key_code AS CHAR), CAST(k.key_name AS CHAR),
                CAST(v.value_code AS CHAR), CAST(v.value_name AS CHAR)
         FROM t_dict_key k
         JOIN t_dict_value v ON v.dict_key_id = k.dict_key_id AND v.deleted_flag = 0
         WHERE k.deleted_flag = 0",
        )
        .fetch_all()
        .await?;
    let mut dicts: HashMap<String, (String, Vec<(String, String)>)> = HashMap::new();
    for (kc, kn, vc, vn) in dict_rows {
        dicts.entry(kc).or_insert_with(|| (kn, vec![])).1.push((vc, vn));
    }
    Ok(dicts)
}

/// 2. 候选列（码型后缀 + 小表）。JOIN 两张表 → `ds_id` 必须带别名（否则歧义）
pub async fn candidate_columns(
    pg: &PgPool,
    ds: &str,
) -> anyhow::Result<Vec<(String, String, String)>> {
    Ok(sqlx::query_as(&format!(
        "SELECT c.table_name, c.column_name, c.col_comment
         FROM meta.column_doc c
         JOIN meta.table_doc t ON t.table_name = c.table_name AND t.ds_id = c.ds_id
         WHERE t.row_estimate < 1000000
           AND c.column_name ~ '(_code|_type|_status|_class|_mode|_way|_level)$'{ds_pred}
         ORDER BY c.table_name, c.ordinal",
        ds_pred = ds_pred_at("c", 1)
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?)
}

/// 3. 人工已覆盖的 (表,列)（value_map + dimension expr 提及）→ 跳过
///
/// 🔴 **「人工」必须真的只算人工**，两侧都要筛，否则 autodiscover 会把**自己上一趟的产物**
/// 当成人工种子而整批跳过 —— 那正是 `RequireKnownValue` 上线后仍然休眠的原因：
/// - `value_map` 不筛 `origin` ⇒ 上一趟 autodiscover 写进去的 775 行也算「人工已覆盖」；
/// - `dimension` 不筛 `dim_code` ⇒ 实测 78 条 active 里 **68 条是 `auto_` 前缀**（它自己造的），
///   而 `covers()` 的判据是「任何 active 维度的 source_table 含表名且 expr 含列名」。
///
/// 两条一凑，`mod.rs` 里那句 `if manual.covers(..) { continue }` 把 **82 个 (表,列) 全部跳过**
/// ⇒ `register_match` 一次都不执行 ⇒ `origin` 永远停在 `seed`
/// ⇒ `load_enum_values`（`WHERE origin = 'dict'`）返 0 行 ⇒ 判据一次都不触发。
/// **判据活着、被判的代码是死的：绿不等于已唤醒。**
///
/// 两条 SQL 抽成**函数**只为能被断言（同 `datasource.rs::visible_datasources_sql()`）：
/// 这一笔改的全部内容就在那两个 WHERE 里，而直接内联 `format!` 出来的串测试拿不到 ——
/// 拿不到就等于没有判据。
pub async fn manual_covered(pg: &PgPool, ds: &str) -> anyhow::Result<Manual> {
    let vm: HashSet<(String, String)> = sqlx::query_as::<_, (String, String)>(&vm_seed_sql())
        .bind(ds)
        .bind(crate::ddl::VALUE_ORIGIN_SEED)
        .fetch_all(pg)
        .await?
        .into_iter()
        .map(|(t, c)| (t.to_lowercase(), c.to_lowercase()))
        .collect();
    let dims: Vec<(String, String)> =
        sqlx::query_as(&dim_manual_sql()).bind(ds).fetch_all(pg).await?;
    Ok(Manual { vm, dims })
}

/// 只认**手工种子**那批码值（`origin = $2`，值由调用方 bind 成 `VALUE_ORIGIN_SEED`）。
/// `origin` **走 bind 不走插值** —— `tests/drift.rs` 的 SQL 插值白名单只放 `ds_pred`。
///
/// 用 `fn` 而不是 `const`：`drift.rs` 的「每条 `meta.*` 读必须带 ds 限定」是按**行窗口**
/// 扫源码的，SQL 与 `{ds_pred}` 拆到两处它就判红（实测 `drift.rs:85` 当场抓住我）。
/// 同 `datasource.rs::visible_datasources_sql()` —— 拼好整条再返，判据断言整串。
fn vm_seed_sql() -> String {
    format!(
        "SELECT DISTINCT table_name, column_name FROM meta.value_map WHERE origin = $2{ds_pred}",
        ds_pred = ds_pred(1)
    )
}

/// 只认**手工声明**的维度：`auto_` 前缀是 autodiscover 自产（实测 78 条 active 里 68 条）。
/// `LIKE` 里的 `_` 是通配符，必须 `ESCAPE`，否则 `auto_` 会连 `autoX` 一起匹配。
fn dim_manual_sql() -> String {
    format!(
        "SELECT source_table, expr FROM meta.dimension \
         WHERE status = 'active' AND dim_code NOT LIKE 'auto/_%' ESCAPE '/'{ds_pred}",
        ds_pred = ds_pred(1)
    )
}

/// 4. 有 deleted_flag 的表集合（拼 WHERE 用；部分表无此列）
pub async fn del_flag_tables(pg: &PgPool, ds: &str) -> anyhow::Result<HashSet<String>> {
    Ok(sqlx::query_as::<_, (String,)>(&format!(
        "SELECT DISTINCT table_name FROM meta.column_doc
         WHERE column_name = 'deleted_flag'{ds_pred}",
        ds_pred = ds_pred(1)
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?
    .into_iter()
    .map(|(t,)| t)
    .collect())
}

/// 反引号里的标识符：只许 `[A-Za-z0-9_$]`、≤64 字符（MySQL 上限）。
///
/// 为什么这条校验不是多余的：`candidate_columns` 读的是 `meta.column_doc`，那张表由
/// `ingest::schema_sync` 灌入，而**上传源的列名来自用户 Excel 表头**（F4 同一条来源）。
/// 今天上传表落 PG、探针只打 MySQL，所以够不着 —— 但「够不着」是两个模块各自的实现细节，
/// 不是不变量。一个含反引号的列名闭合掉引号，得到的是一条带 `unrestricted` 放行、
/// 无行级过滤的任意读。原注释「不含任何用户输入」正是那类会静默变假的断言。
fn ident(s: &str) -> Option<&str> {
    let ok = !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    ok.then_some(s)
}

/// 码型抽样上限（枚举列：真库最多 34 值，61 留一倍余量）
const CODE_LIMIT: usize = 61;
/// 名称型值域抽样上限：分类名百级、品牌名可能上千，61 会把值域静默截断成半份词典。
/// ponytail: 2000 封顶。真到万级取值时该换的是匹配层（`longest_value_hit` 的倒序 contains），
/// 不是这个数 —— 到那天再谈多模式自动机。
const DOMAIN_LIMIT: usize = 2000;

/// 抽样 SQL（纯函数，可单测）：只读 DISTINCT ≤ `limit` 值。
/// `None` = 表名/列名不是合法标识符 → 不探（fail-closed，宁可少发现一个码表）。
/// `LIMIT` 走位置参数：`drift.rs` 的 ALLOW 白名单只放 col/table/where_del，本轮不新增例外。
fn probe_sql_capped(c: &ColRef, limit: usize) -> Option<String> {
    let (col, table) = (ident(c.col)?, ident(c.table)?);
    let where_del = if c.has_del { "WHERE deleted_flag = 0" } else { "" };
    Some(format!(
        "SELECT DISTINCT CAST(`{col}` AS CHAR) FROM `{table}` {where_del} LIMIT {}",
        limit
    ))
}

/// 码型探针的抽样 SQL（≤61）。形态与反引号越狱由下面两条断言守着。
fn probe_sql(c: &ColRef) -> Option<String> {
    probe_sql_capped(c, CODE_LIMIT)
}

/// autodiscover 动态探针的闸门（裁决 T3-2 / C5：动态 SQL 走同一条全管道，不开专用后门）。
/// **有资格 unrestricted 放行**：这是 CLI 管理任务（`meta autodiscover`），SQL 由
/// `probe_sql_capped` 用 information_schema 的表名/列名（名称型那条走 `meta.value_domain` 的
/// 人工声明）拼装 —— 两条来源都先过 `ident()` 白名单，不含任何用户输入，
/// 也没有「以谁的身份查」这回事。即便如此只读红线与 LIMIT 护栏一条不少（`check()` 照走）。
fn probe_scoped(sql: &str, gate: &ProbeGate<'_>) -> anyhow::Result<dms_kernel::ScopedSql> {
    let checked =
        dms_kernel::check(dms_kernel::RawSql::new(sql), &dms_kernel::MysqlDialect, gate.guard)?;
    Ok(dms_kernel::ScopedSql::unrestricted(checked, gate.proof))
}

/// 码型抽样（≤61 值）。`None` = 被闸门拒或抽样失败（**都不计入 probed**）。
pub async fn sample_values(
    mysql: &ReadOnlyMySql,
    gate: &ProbeGate<'_>,
    c: &ColRef<'_>,
) -> Option<Vec<String>> {
    fetch_distinct(mysql, gate, c, probe_sql(c), gate.guard.max_rows).await
}

/// 名称型值域抽样（`meta.value_domain` 登记的列）。与码型走**同一条**通路，差别只有两点：
/// 上限放宽到 2000（`fetch` 的行截断一并放宽 —— 否则 guard.max_rows=200 会把 SQL 的 2000 截回去），
/// 且不做码型三闸（那三闸防的是码列误配，名称型是显式声明的）。
pub async fn sample_domain_values(
    mysql: &ReadOnlyMySql,
    gate: &ProbeGate<'_>,
    c: &ColRef<'_>,
) -> Option<Vec<String>> {
    fetch_distinct(mysql, gate, c, probe_sql_capped(c, DOMAIN_LIMIT), DOMAIN_LIMIT).await
}

/// 只读抽样（生产库连接池会话级 READ ONLY 兜底）。单探针 10s 超时：
/// row_estimate 可能严重失真（29 行的表真实扫描分钟级），悬挂探针跳过不拖全局。
///
/// `None` = 被闸门拒或抽样失败（**都不计入 probed**，与原实现的两处 `continue` 同）。
async fn fetch_distinct(
    mysql: &ReadOnlyMySql,
    gate: &ProbeGate<'_>,
    c: &ColRef<'_>,
    sql: Option<String>,
    max: usize,
) -> Option<Vec<String>> {
    let (table, col) = (c.table, c.col);
    let Some(sql) = sql else {
        tracing::warn!("autodiscover 跳过：标识符含非法字符 {table}.{col}");
        return None;
    };
    let scoped = match probe_scoped(&sql, gate) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("autodiscover 探针被闸门拒绝 {table}.{col}: {e}");
            return None;
        }
    };
    // 超时不再由外层 timeout 包，而是 `fetch` 的入参（超时文案带源标识）
    let probe = mysql.fetch(&scoped, max, Duration::from_secs(10)).await;
    let rows: Vec<Vec<serde_json::Value>> = match probe {
        Ok(rs) => rs.rows,
        Err(e) => {
            tracing::warn!("autodiscover 抽样失败 {table}.{col}: {e}");
            return None;
        }
    };
    Some(
        rows.iter()
            .filter_map(|r| r.first().and_then(|v| v.as_str()))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_sql_is_read_only_and_capped() {
        let c = ColRef { table: "t_menu", col: "menu_type", has_del: true };
        assert_eq!(
            probe_sql(&c).unwrap(),
            "SELECT DISTINCT CAST(`menu_type` AS CHAR) FROM `t_menu` WHERE deleted_flag = 0 LIMIT 61"
        );
        let c2 = ColRef { table: "t_menu", col: "menu_type", has_del: false };
        assert_eq!(
            probe_sql(&c2).unwrap(),
            "SELECT DISTINCT CAST(`menu_type` AS CHAR) FROM `t_menu`  LIMIT 61"
        );
    }

    /// 反引号闭合 = 带 unrestricted 放行的任意读。拼不出 SQL 才是正确结果。
    #[test]
    fn backtick_identifier_cannot_break_out() {
        let evil = ColRef {
            table: "t_menu",
            col: "a` AS CHAR) FROM t_user WHERE 1=1 -- ",
            has_del: false,
        };
        assert!(probe_sql(&evil).is_none());
        // 表名侧同样封（两个标识符都进反引号）
        assert!(probe_sql(&ColRef { table: "t`x", col: "menu_type", has_del: false }).is_none());
        // 空列名、超长标识符也不探
        assert!(probe_sql(&ColRef { table: "t_menu", col: "", has_del: false }).is_none());
        let long = "a".repeat(65);
        assert!(probe_sql(&ColRef { table: "t_menu", col: &long, has_del: false }).is_none());
        // 正常标识符照旧能探（守卫别把功能封死）
        assert!(probe_sql(&ColRef { table: "t_menu", col: "order_status", has_del: false }).is_some());
    }

    /// 名称型值域探针：只读 + DISTINCT 一字不变，**唯一**差别是上限 2000（码型仍 61）。
    /// 反引号白名单同一个闸（名称型不开专用后门）。
    #[test]
    fn domain_probe_widens_only_the_cap() {
        let c = ColRef { table: "t_goods_category", col: "category_name", has_del: true };
        assert_eq!(
            probe_sql_capped(&c, DOMAIN_LIMIT).unwrap(),
            "SELECT DISTINCT CAST(`category_name` AS CHAR) FROM `t_goods_category` \
             WHERE deleted_flag = 0 LIMIT 2000"
        );
        assert!(probe_sql(&c).unwrap().ends_with("LIMIT 61"));
        // ident() 一条不许绕（裁决 二·F F3）：表名/列名侧都封
        assert!(probe_sql_capped(
            &ColRef { table: "t`x", col: "category_name", has_del: false },
            DOMAIN_LIMIT
        )
        .is_none());
        assert!(probe_sql_capped(
            &ColRef { table: "t_goods_category", col: "a` AS CHAR) FROM t_user -- ", has_del: false },
            DOMAIN_LIMIT
        )
        .is_none());
    }

    /// 人工种子优先：大小写无关命中 value_map，或 dimension 表达式提及该列
    #[test]
    fn manual_seeds_take_precedence() {
        let m = Manual {
            vm: [("t_x".to_string(), "a_code".to_string())].into_iter().collect(),
            dims: vec![("t_y".into(), "CASE `b_type` WHEN '1' THEN '甲' END".into())],
        };
        assert!(m.covers("T_X", "A_CODE"));
        assert!(m.covers("t_y", "b_type"));
        assert!(!m.covers("t_z", "c_status"));
    }

    /// 🔴 「人工已覆盖」必须真的只算人工 —— 否则 autodiscover 把**自己上一趟的产物**
    /// 当人工种子整批跳过，`register_match` 一次都不执行，`RequireKnownValue` 永远休眠
    /// （实测症状：`meta.value_map` 里 `origin='dict'` 恒 0 行）。
    ///
    /// 判据打在两条 SQL 的 const 上：这一笔改的全部内容就是那两个 WHERE。
    /// 三条防恒真：① 两个 bind 序号必须**不同**（撞了就是拿 ds 当 origin 用）
    /// ② `ESCAPE` 必须在（少了 `auto_` 会误吞 `autoX`）③ 反面 —— 不许把 status 过滤顺手删掉。
    #[test]
    fn manual_means_manual_only() {
        // value_map 侧：必须按 origin 收窄，且走 bind（插值会撞 drift.rs 的白名单）
        let vm = vm_seed_sql();
        assert!(vm.contains("origin = $2"), "{vm}");
        assert!(!vm.contains(crate::ddl::VALUE_ORIGIN_SEED), "origin 不许插值进 SQL：{vm}");
        // ds 走 $1、origin 走 $2 —— 序号撞上就是把 ds_id 当 origin 查（恒 0 行，静默）
        assert!(vm.contains("$1") && vm.contains("$2"), "两个 bind 序号都要在：{vm}");
        // 这条 SQL 必须**自带** ds 限定（拆到调用处 drift.rs 会判红，也真的是跨源污染）
        assert!(vm.contains("ds_id"), "少了 ds 限定 = 读到别的源的行：{vm}");
        // dimension 侧：排除自产维度，且 ESCAPE 不许少
        let dm = dim_manual_sql();
        assert!(dm.contains("dim_code NOT LIKE 'auto/_%' ESCAPE '/'"), "{dm}");
        // 反面：原有的 active 过滤不许被顺手删掉（删了会把停用维度也算人工覆盖）
        assert!(dm.contains("status = 'active'"), "{dm}");
        assert!(dm.contains("ds_id"), "少了 ds 限定：{dm}");
    }
}
