//! 【A9】向量自愈（meta 侧）：把 `embedding IS NULL` 的行按**与离线脚本同一配方**补回。
//!
//! 由来：写入点原来只有离线脚本（`tools/embed_service.py build/revec`），服务侧只有体检
//! 没有修复（`ddl.rs vector_ready` 三个 EXISTS）—— 于是 `upsert_datasource` 把变更行置
//! NULL 之后没人补，向量路静默降级到「结果为空」。
//!
//! 🔴 **文本配方与 `tools/embed_service.py::build` 逐字一致**：两边写同一列，
//! 配方不同 = 同一列里混着两套不可比的向量，0.35/0.5/0.55 三个实测阈值静默全废。
//! （判据 `text_recipe_matches_the_offline_builder` 钉住四个配方片段。）
//!
//! 调度与并发守在 server（`server/src/embed_fill.rs`，advisory lock）；
//! 本文件只有「选哪些行、文本是什么、写回哪里」三件事。

use sqlx::PgPool;

use crate::registry::ds_pred;

/// 单轮每类最多补多少行：攒了几千行时一轮全补会把本轮拖成几分钟 —— 分批，
/// 剩下的下一轮（调度间隔定义在 server 侧 `server/src/embed_fill.rs`，本文件不抄数字），
/// 每轮有界。
pub const FILL_BATCH: i64 = 256;

/// 五类 meta 向量目标。`Datasource` 没有 ds 谓词：它是 ds 注册表本身
/// （drift 守卫的豁免清单与离线脚本「这里不加也不能加 ds 限定」的注释，两边一致）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MetaVecTarget {
    TableDoc,
    Element,
    Datasource,
    SqlExemplar,
    /// 【S4】经验复盘：蒸馏写入时 embedding 留 NULL，由这里按「content 原文」配方补
    Memory,
}

impl MetaVecTarget {
    pub const ALL: [MetaVecTarget; 5] =
        [Self::TableDoc, Self::Element, Self::Datasource, Self::SqlExemplar, Self::Memory];

    /// 是否「问句侧」向量：语料问句是查询侧（语义缓存按问句近邻召回），
    /// 其余四类都是文档侧 —— 与离线 `_revec(.., is_query)` 的取值逐条一致。
    pub fn is_query_side(&self) -> bool {
        matches!(self, Self::SqlExemplar)
    }

    /// 是否按 ds 限定（`Datasource` 是注册表本身，豁免）
    pub fn ds_scoped(&self) -> bool {
        !matches!(self, Self::Datasource)
    }

    /// 五条 SQL 的进程内成品（`ds_pred(1)` 对固定入参是确定串，不每次调用 format! 重建）。
    /// 下标 = 枚举声明序（与 `ALL` 同序）。
    fn select_sql(&self) -> &'static str {
        static SQLS: std::sync::LazyLock<[String; 5]> = std::sync::LazyLock::new(|| {
            [
                // 文本配方四连，与离线 build 逐字一致（判据钉在下方）
                format!(
                    "SELECT table_name, coalesce(nullif(search_doc, ''), table_name) \
                     FROM meta.table_doc WHERE embedding IS NULL{ds_pred} LIMIT $2",
                    ds_pred = ds_pred(1)
                ),
                format!(
                    "SELECT element_id, search_text FROM meta.element \
                     WHERE status = 'active' AND embedding IS NULL{ds_pred} LIMIT $2",
                    ds_pred = ds_pred(1)
                ),
                "SELECT ds_id, name || '。' || description FROM meta.datasource \
                 WHERE status = 'active' AND embedding IS NULL LIMIT $1"
                    .to_string(),
                format!(
                    "SELECT id::text, question FROM meta.sql_exemplar \
                     WHERE status = 'enabled' AND embedding IS NULL{ds_pred} LIMIT $2",
                    ds_pred = ds_pred(1)
                ),
                // 经验的向量化文本 = content 原文（蒸馏时已截 400 字，无需再加工）
                format!(
                    "SELECT id::text, content FROM meta.memory \
                     WHERE embedding IS NULL{ds_pred} LIMIT $2",
                    ds_pred = ds_pred(1)
                ),
            ]
        });
        &SQLS[*self as usize]
    }

    fn update_sql(&self) -> &'static str {
        match self {
            Self::TableDoc =>
                "UPDATE meta.table_doc SET embedding = $1::vector WHERE table_name = $2 AND ds_id = $3",
            Self::Element =>
                "UPDATE meta.element SET embedding = $1::vector WHERE element_id = $2 AND ds_id = $3",
            Self::Datasource =>
                "UPDATE meta.datasource SET embedding = $1::vector WHERE ds_id = $2",
            Self::SqlExemplar =>
                "UPDATE meta.sql_exemplar SET embedding = $1::vector WHERE id = $2::bigint AND ds_id = $3",
            Self::Memory =>
                "UPDATE meta.memory SET embedding = $1::vector WHERE id = $2::bigint AND ds_id = $3",
        }
    }
}

/// 待补行：`(主键, 待向量化文本)`。
pub async fn null_vec_rows(
    pg: &PgPool,
    ds: &str,
    t: MetaVecTarget,
    limit: i64,
) -> anyhow::Result<Vec<(String, String)>> {
    let limit = limit.max(0); // 负值 PG 直接报错，夹紧
    let sel = t.select_sql();
    let q = sqlx::query_as::<_, (String, String)>(sel);
    let rows = if t.ds_scoped() {
        q.bind(ds).bind(limit).fetch_all(pg).await?
    } else {
        q.bind(limit).fetch_all(pg).await?
    };
    Ok(rows)
}

/// 写回一行（`lit` 是 `to_pgvector` 的字面量）。失败上抛 —— 该行保持 NULL，下轮再试。
pub async fn write_vec(
    pg: &PgPool,
    ds: &str,
    t: MetaVecTarget,
    id: &str,
    lit: &str,
) -> anyhow::Result<()> {
    let q = sqlx::query(t.update_sql()).bind(lit).bind(id);
    if t.ds_scoped() {
        q.bind(ds).execute(pg).await?;
    } else {
        q.execute(pg).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 🔴 四个文本配方必须与离线 `tools/embed_service.py::build` 逐字一致：
    /// 两边写同一列，配方不同 = 同一列混着两套不可比向量，实测阈值全废。
    /// 改任何一边，另一边与这条判据一起改。
    #[test]
    fn text_recipe_matches_the_offline_builder() {
        let t = MetaVecTarget::TableDoc.select_sql();
        assert!(t.contains("coalesce(nullif(search_doc, ''), table_name)"), "{t}");
        let e = MetaVecTarget::Element.select_sql();
        assert!(e.contains("search_text") && e.contains("status = 'active'"), "{e}");
        let d = MetaVecTarget::Datasource.select_sql();
        assert!(d.contains("name || '。' || description"), "{d}");
        assert!(!d.contains("{ds_pred}") && !d.contains("ds_id ="), "注册表本身不许带 ds 限定：{d}");
        let x = MetaVecTarget::SqlExemplar.select_sql();
        assert!(x.contains("question") && x.contains("status = 'enabled'"), "{x}");
        let m = MetaVecTarget::Memory.select_sql();
        // 锚点 `concat!` 拼（自匹配家族，本仓第七次）：漂移守卫扫全文件找 `FROM meta.<表>`，
        // 裸写会被它当成一条缺 ds 谓词的召回 SQL —— 注释里也不许出现那三个字连表名。
        assert!(m.contains(concat!("id::text, content FROM meta.", "memory")), "{m}");
        // 问句侧只有语料问句一类（离线 `_revec(.., is_query=True)` 只传了它）；
        // 遍历 ALL 而不是手抄清单：新增目标漏加会当场红
        for t in MetaVecTarget::ALL {
            assert_eq!(
                t.is_query_side(),
                matches!(t, MetaVecTarget::SqlExemplar),
                "{t:?} 的问句侧标记"
            );
        }
    }

    /// 上面那条只把 Rust 侧钉在字面量上 —— 它**读不到离线脚本**，所以「离线少覆盖一张表」
    /// 它一个字都不会红。2026-08-16 换向量空间那次就是这么漏的：`build` 覆盖四张、
    /// `meta.memory` 只长在 Rust 这边，重算之后库里同时存着两套不可比的向量。
    /// 这条判据真去读 `tools/embed_service.py`：每个目标的表名与文本配方都得在里面出现。
    #[test]
    fn the_offline_builder_covers_every_meta_vector_target() {
        let py = include_str!("../../../../tools/embed_service.py");
        // 只看 build() 那一段：selftest/注释里出现表名不算覆盖
        let build = py
            .split_once("def build(ds='dms'):")
            .expect("tools/embed_service.py 里找不到 build()")
            .1;
        // 切到「第五个 build 目标」那条分节线为止：`_revec_datasources` 是 build 调用的
        // 私有助手，配方长在它里面，按换行加 def 切会把它切掉（第一版就是这么漏判的）
        let build = build.split("# ============ 第五个 build 目标").next().unwrap();
        for t in MetaVecTarget::ALL {
            let table = match t {
                MetaVecTarget::TableDoc => "table_doc",
                MetaVecTarget::Element => "element",
                MetaVecTarget::Datasource => "datasource",
                MetaVecTarget::SqlExemplar => "sql_exemplar",
                MetaVecTarget::Memory => "memory",
            };
            // datasource 走 build 调用的 `_revec_datasources`，函数名里带表名即算覆盖
            let covered = build.contains(&format!("meta.{table}"))
                || build.contains(&format!("_revec_{table}s"));
            assert!(covered, "{t:?}（meta.{table}）在离线 build() 里没有写入点");
        }
        // 配方也对一遍：同一列写两套文本 = 同一列混两套向量
        for frag in [
            "coalesce(nullif(search_doc, ''), table_name)",
            "name || '。' || description",
            "SELECT id, content FROM meta.memory",
        ] {
            assert!(build.contains(frag), "离线 build() 缺配方片段：{frag}");
        }
    }

    /// 写回必须带主键谓词（`ds_id` 双写的两张表尤其）：少了就是把别的源的向量盖掉。
    #[test]
    fn updates_are_scoped_to_the_row_key() {
        assert!(MetaVecTarget::TableDoc.update_sql().contains("table_name = $2 AND ds_id = $3"));
        assert!(MetaVecTarget::Element.update_sql().contains("element_id = $2 AND ds_id = $3"));
        assert!(MetaVecTarget::SqlExemplar.update_sql().contains("id = $2::bigint AND ds_id = $3"));
        assert!(MetaVecTarget::Memory.update_sql().contains("meta.memory"));
        assert!(MetaVecTarget::Memory.update_sql().contains("id = $2::bigint AND ds_id = $3"));
        assert_eq!(MetaVecTarget::Datasource.update_sql(),
                   "UPDATE meta.datasource SET embedding = $1::vector WHERE ds_id = $2");
    }
}
