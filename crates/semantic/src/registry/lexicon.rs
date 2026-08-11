//! 文本命中侧的注册表行类型与读取：术语 / 值链接码表。
//! 变更原因＝命中侧读到的形状。召回的命中与卡片渲染在 `recall/*`，这里只管取行。
//!
//! 搬运源 `server/src/meta.rs:871-877`（`recall_terms` 的加载段）与
//! `server/src/meta.rs:1133-1140`（`recall_value_hints` 的加载段）—— SQL 文本与绑定序号原样。
//!
//! `DocBinding` 本轮不落：`meta.doc_binding` 这张表还不存在（DDL 16 张里没有它，
//! 单号直查读的是 `direct.rs` 里的硬编码前缀表），造个空类型只是替将来占位。

use crate::registry::{catalog_allows_column, ds_pred, table_asset_live_pred_at};
use sqlx::PgPool;

/// 业务术语（meta.term 行）
#[derive(Debug)]
pub struct TermDef {
    pub term: String,
    pub definition: String,
    pub aliases: Vec<String>,
}

/// 值链接码表（meta.value_map 行）。`match_kind`：eq=等值换码 / like=组合值列须 LIKE '%码%'
/// （与 `model::ValueRef` 同表同字段的另一份行类型：字段名不同，合并要动 server 侧消费点
/// —— 欠账，两处注释互指。）
#[derive(Debug)]
pub struct ValueMap {
    pub table_name: String,
    pub column_name: String,
    pub name: String,
    pub code: String,
    pub match_kind: String,
}

/// 实体名值域声明（meta.value_domain 行）：这一列的取值是**业务实体名**，不是码值。
/// **取值不在这张表里**：由 `meta autodiscover` 的名称型探针灌进 `meta.value_map`
/// （`name = code = 取值`，复用码值表不新建 —— 重跑即自适应），读取见 `load_domain_values`。
#[derive(Debug)]
pub struct ValueDomain {
    pub table_name: String,
    pub column_name: String,
    /// 人话：该用哪一列过滤、误用哪一列会怎样（LLM 逐字读，渲染进值域命中卡）
    pub note: String,
}

pub async fn load_value_domains(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<ValueDomain>> {
    let ds_pred = format!(
        "{}{}",
        crate::registry::ds_pred(1),
        table_asset_live_pred_at("", 1)
    );
    // ORDER BY 钉死行序：caliber 的 domain_rules 产出规则序不随物理行序漂
    let rows: Vec<(String, String, String)> = sqlx::query_as(&format!(
        "SELECT table_name, column_name, note FROM meta.value_domain WHERE 1 = 1{ds_pred}
         ORDER BY table_name, column_name",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(table_name, column_name, ..)| {
            catalog_allows_column(ds, table_name, column_name)
        })
        .map(|(table_name, column_name, note)| ValueDomain { table_name, column_name, note })
        .collect())
}

/// 名称型值域的**取值** `(表, 列, 取值)`：`meta.value_map` 里 `(表,列)` 在 `meta.value_domain`
/// 登记过的那批（名称型的 `name = code = 取值`）。
/// 交集在 SQL 里 JOIN 完 —— 不查全表再回 Rust 过滤（谓词一律留在 SQL 内是本仓纪律）。
pub async fn load_domain_values(
    pg: &PgPool,
    ds: &str,
) -> anyhow::Result<Vec<(String, String, String)>> {
    let ds_pred = format!(
        "{}{}",
        crate::registry::ds_pred_at("v", 1),
        table_asset_live_pred_at("v", 1)
    );
    let rows: Vec<(String, String, String)> = sqlx::query_as(&format!(
        "SELECT v.table_name, v.column_name, v.name FROM meta.value_map v
         JOIN meta.value_domain d ON d.table_name = v.table_name
           AND d.column_name = v.column_name AND d.ds_id IN (v.ds_id, '*')
         WHERE 1 = 1{ds_pred}
         ORDER BY v.table_name, v.column_name, v.name",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(table_name, column_name, ..)| {
            catalog_allows_column(ds, table_name, column_name)
        })
        .collect())
}

/// 值域命中：问句里出现的最长取值（纯函数，`values` 是该列的取值集）。
///
/// **最长优先**是必须的：实体名值域里「手抓饼」与「手抓饼卷」并存，短名先中会把统计范围放大。
/// 单字取值一律不算（与 `recall_value_hints` 的 `>= 2` 同一门槛，避免「饼」命中一切）。
///
/// ponytail: 按长度倒序 `contains`，O(n·m)。值域规模百级（真库 60 个分类名），真到万级再谈
/// 多模式自动机 —— 本轮明令不引 aho-corasick。
pub fn longest_value_hit<'a>(
    question: &str,
    values: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    // 长度键随收集预算一次（原来 sort_by_key 的 key 在 O(n log n) 次比较里重算 chars().count()）
    let mut vs: Vec<(usize, &str)> = values
        .into_iter()
        .filter_map(|v| {
            let n = v.chars().count();
            (n >= 2).then_some((n, v))
        })
        .collect();
    // 早退判空：空取值集不进 sort/find
    if vs.is_empty() {
        return None;
    }
    vs.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    vs.into_iter().find(|(_, v)| question.contains(*v)).map(|(_, v)| v)
}

/// 术语加载。无 asset-live 谓词的豁免说明：term 不挂物理表（纯文本知识，无表活性可判），
/// 刻意只按 status/ds 过滤 —— 漂移守卫（grep 谓词）读到这里别当漏网。
pub async fn load_terms(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<TermDef>> {
    let rows: Vec<(String, String, Vec<String>)> = sqlx::query_as(&format!(
        "SELECT term, definition, aliases FROM meta.term WHERE status = 'active'{ds_pred}",
        ds_pred = ds_pred(1)
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(term, definition, aliases)| TermDef { term, definition, aliases })
        .collect())
}

/// 命中侧码值全量加载。
/// 🔴 与 `model::load_value_map` 同表两份加载：过滤口径（`catalog_allows_column` vs
/// `catalog_allows_table`）、返回类型都不同 —— 各自服务不同判据，改一边先看另一边。
/// ORDER BY 与 model 侧同序（确定性：同名多列的卡序/码查找不随物理行序漂）。
pub async fn load_value_maps(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<ValueMap>> {
    let ds_pred = format!(
        "{}{}",
        crate::registry::ds_pred(1),
        table_asset_live_pred_at("", 1)
    );
    let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(&format!(
        "SELECT table_name, column_name, name, code, match_kind
         FROM meta.value_map WHERE 1 = 1{ds_pred} ORDER BY name, table_name, column_name",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(table_name, column_name, ..)| {
            catalog_allows_column(ds, table_name, column_name)
        })
        .map(|(table_name, column_name, name, code, match_kind)| ValueMap {
            table_name,
            column_name,
            name,
            code,
            match_kind,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_domain_hit_takes_the_longest() {
        // 真库分类名里「手抓饼」与「手抓饼卷」并存：短名先中会把别的分类算进来
        let cats = ["手抓饼", "手抓饼卷", "烤肠"];
        assert_eq!(
            longest_value_hit("2026年6月手抓饼卷这个分类卖了多少箱", cats),
            Some("手抓饼卷")
        );
        assert_eq!(longest_value_hit("2026年6月手抓饼这个分类卖了多少箱", cats), Some("手抓饼"));
        assert_eq!(longest_value_hit("本月销售额", cats), None);
        // 单字取值不算（否则「饼」命中一切）
        assert_eq!(longest_value_hit("手抓饼卖了多少", ["饼"]), None);
    }
}
