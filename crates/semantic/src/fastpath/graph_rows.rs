//! T8 搬运：逐行迁自 `server/src/direct.rs`（**只搬不改**，一个字节的行为改动都会让
//! `evaluation.py` 的逐题结果集对拍失去意义）。顺序即行为，只提取不重排。

#![allow(clippy::too_many_arguments)]

use std::collections::{HashMap, HashSet};

use sqlx::PgPool;

use dms_kernel::nl::text::strip_annotations;
use dms_kernel::nl::time::{detect_top_n, fill_time_col, prev_window, time_predicate, yoy_window};
use dms_kernel::sql::lex::{base_col_refs, from_table_aliases, qualify_cols};

use crate::fastpath::*;
use crate::compose::*;
use crate::registry::model::{DimensionDef as DimDef, JoinEdge, MetricDef, TableSnapshot, ValueRef};
use crate::{DirectHit, DirectOutcome, ExecutionEvidence, IntentSlotKind, Relation};

// 同批搬来的兄弟模块（原文件里是同一个作用域，拆文件后要显式引）
#[allow(unused_imports)]
use crate::compose::{assemble::*, metric::*, path::*, values::*};
#[allow(unused_imports)]
use crate::fastpath::{derive::*, finance::*, ops::*, relation::*, sales::*, stock::*, template::*};

use crate::sales_fact;

/// 关系 SQL 的实体名字面量转义：与 `sales_fact::quote` 同规格（`\` 与 `'` 都处理）。
/// 只转 `'` 时，实体名以 `\` 结尾会吃掉闭引号 → 兜底 SQL 自己语法错误。
/// LIKE 通配符（`%`/`_`）不剥 —— 与合同侧 `Predicate::contains` 同一口径（已知的语义放宽）。
pub fn rel_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "''")
}


