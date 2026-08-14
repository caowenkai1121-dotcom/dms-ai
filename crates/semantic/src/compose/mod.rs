//! T8 搬运：逐行迁自 `server/src/direct.rs`（**只搬不改**，一个字节的行为改动都会让
//! `evaluation.py` 的逐题结果集对拍失去意义）。顺序即行为，只提取不重排。

#![allow(clippy::too_many_arguments)]

use std::collections::{HashMap, HashSet};

use sqlx::PgPool;

pub mod assemble;
pub mod metric;
pub mod path;
pub mod values;

use dms_kernel::nl::text::strip_annotations;
use dms_kernel::nl::time::{detect_top_n, fill_time_col, prev_window, time_predicate, yoy_window};
use dms_kernel::sql::lex::{base_col_refs, from_table_aliases, qualify_cols};

use crate::registry::model::{DimensionDef as DimDef, JoinEdge, MetricDef, TableSnapshot, ValueRef};
use crate::{DirectHit, DirectOutcome, ExecutionEvidence, IntentSlotKind, Relation};

// 同批搬来的兄弟模块（原文件里是同一个作用域，拆文件后要显式引）
#[allow(unused_imports)]
use crate::compose::{assemble::*, metric::*, path::*, values::*};
#[allow(unused_imports)]
use crate::fastpath::{derive::*, finance::*, graph_rows::*, ops::*, relation::*, sales::*, stock::*, template::*};

// 子模块符号在父模块同样可见（搬运前它们本来就是同一个作用域）
pub use assemble::*;
pub use metric::*;
pub use path::*;
pub use values::*;

use crate::sales_fact;

/// `DirectHit` 的单一构造点：只出 sql+route 的命中全走这里（prev/comparisons/detail/
/// sales_context 默认空），要带字段的用结构更新语法覆盖 —— 那五字段字面量曾散写了 19 处。
pub fn hit(sql: String, route: &str) -> DirectHit {
    DirectHit {
        outcome: DirectOutcome::Data,
        sql,
        route: route.into(),
        prev: None,
        comparisons: vec![],
        detail: None,
        sales_context: None,
        intent_evidence: Default::default(),
    }
}




/// 单号直查是否命中（`try_direct` 的第一支）。抽出来只为让 `hardcoded_producer` 分得清三支。
pub fn doc_binding_hit(question: &str) -> bool {
    sniff_doc_code(question, true).is_some()
}




/// 快照门（**通用防线**）：指标来源表登记在 `meta.table_snapshot` 里 → **按声明装配**：
/// 照声明的分区键/排序/额外过滤包一层 `ROW_NUMBER() … rn = 1`（取每分区最新一条）。
/// 仍拒的只有两种：① 声明缺分区键或排序（包不出确定的「最新一条」）；
/// ② 同一张表既声明去重键又声明快照（两层怎么叠是未定义的，宁可回落 LLM）。
///
/// 为什么不能平铺：装配器是「指标 × 维度」GROUP BY，**不懂「取每个分区最新一条」**——
/// 余额类指标一平铺就丢 `rn = 1`，把同一 (客户,账余类型) 的历史流水行全部求和（数字虚高），
/// 而 route 恒为 `direct-agg`，确定性路径不跑口径校验、连回炉都没机会，错数直接出给用户。
/// 声明不全而回落 LLM 时，由口径卡 + `RequireLatest` 判据接管（它们才认识快照语义）。
///
/// 历史注记：本函数曾经是「见快照就一律不装配」。那正确但过度 —— 把余额/库存一族永久
/// 留在 LLM 路径上，而实测 LLM 把 `rn = 1` 写对的概率约 1/3。库存类指标
/// （`stock_qty`/`stock_amount`）彼时没出事是**碰巧**：它们的 `scope_filter` 含
/// `(SELECT MAX(product_stock_date) …)`，撞上了 `compose_sql_with` 的「含 SELECT 即不装配」
/// 那道门。那是偶然，不是防线 —— 声明才是。
pub fn compose_gated(
    m: &MetricDef,
    d: &DimDef,
    question: &str,
    edges: &[JoinEdge],
    table_scopes: &[(String, String)],
    snaps: &[TableSnapshot],
    vals: &[ValueRef],
) -> Option<String> {
    // 来源表声明带人类注解（`t_sales_order_detail(JOIN …)`）或 UNION 串，取首个标识符即基表
    let base = dms_kernel::sql::lex::first_ident_of(&m.source_table)?;
    let snap = snaps.iter().find(|s| s.table_name.eq_ignore_ascii_case(&base));
    // 🔴 从「见快照就拒」改成「按声明装配」。
    //
    // 原来的拒绝是**正确但过度**的：装配器平铺 GROUP BY 确实不懂「取每分区最新一条」，
    // 于是把余额/库存这一族永久留在 LLM 路径上 —— 而实测 LLM 把 `rn = 1` 写对的概率约 1/3。
    // **但 `meta.table_snapshot` 已经声明了分区键、取最新的排序、以及该表恒需的额外过滤**，
    // 装配器完全可以照它包一层（与 `dedup_keys` 那层是同一个形状，只把 `DISTINCT 键` 换成 `rn=1`）。
    // 这是本轮反复遇到的同一个模式的最后一处：**声明在那儿，装配器不读它**。
    //
    // 仍然拒的两种：① 声明缺分区键或排序（包不出确定的「最新一条」）；
    // ② 同一张表既声明去重键又声明快照 —— 两层怎么叠是未定义的，宁可回落 LLM。
    if let Some(s) = snap {
        if s.partition_cols.trim().is_empty() || s.order_cols.trim().is_empty() {
            return None;
        }
        if !m.dedup_keys.trim().is_empty() {
            return None;
        }
    }
    // 跨表时间维度只有在指标没有声明时间列时才拒。声明完整时，下方把通用月份表达式
    // 绑定到指标基表的 `time_col`，避免“按订单时间分退款”的旧错口径。
    let dim_base = dms_kernel::sql::lex::first_ident_of(&d.source_table).unwrap_or_default();
    if !dim_base.is_empty()
        && !dim_base.eq_ignore_ascii_case(&base)
        && is_time_expr(&d.expr)
        && strip_annotations(&m.time_col).is_empty()
    {
        return None;
    }
    compose_sql_with_snap(m, d, question, edges, table_scopes, snap, None, vals)
}



