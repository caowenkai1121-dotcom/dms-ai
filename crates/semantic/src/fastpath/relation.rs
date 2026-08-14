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
use crate::fastpath::{derive::*, finance::*, graph_rows::*, ops::*, sales::*, stock::*, template::*};

use crate::sales_fact;

/// 识别图关系问题并抽实体名。顺序敏感：共购(还买)先于买过，买过先于"X买了"。
pub fn detect_relation(q: &str) -> Option<Relation> {
    // 共购：买X还买 / 买了X还买什么
    // （四个析取项字字都含「买」，原来再合取 `q.contains("买")` 恒真 —— 死条件，已删）
    if q.contains("还买") || q.contains("还购买") || q.contains("关联购买") || q.contains("一起买") {
        let name = strip_relation_words(q);
        if !name.is_empty() {
            return Some(Relation::Copurchase(name));
        }
    }
    // 买过 X 的客户 / 哪些客户买过 X
    if (q.contains("买过") || q.contains("购买过") || q.contains("买了")) && (q.contains("客户") || q.contains("哪些") || q.contains("门店")) {
        let name = strip_relation_words(q);
        if !name.is_empty() {
            return Some(Relation::BuyersOfGoods(name));
        }
    }
    // X 买过什么 / X 买了哪些商品
    if q.contains("买过什么") || q.contains("买了什么") || q.contains("买过哪些") || q.contains("买了哪些") || q.contains("购买清单") {
        let name = strip_relation_words(q);
        if !name.is_empty() {
            return Some(Relation::GoodsOfCustomer(name));
        }
    }
    None
}


/// 剥关系词/疑问词，剩下实体名
pub fn strip_relation_words(q: &str) -> String {
    let mut s = q.to_string();
    for w in [
        "还买过什么", "还买什么", "还买了什么", "还购买", "还买", "关联购买", "一起买",
        "买过什么", "买了什么", "买过哪些", "买了哪些", "购买清单", "购买过", "买过", "买了",
        "的客户", "哪些客户", "哪些门店", "哪些", "客户", "门店", "商品", "什么",
    ] {
        s = s.replace(w, "");
    }
    // 单字词只在**边界**剥：实体名里可能含这些字（「美的」的「的」），
    // 全局 replace 会把实体名吃掉（「买过美的冰箱的客户」剥完剩「美冰箱」，探库/过滤全错）
    for w in ["有", "的", "是", "都", "买"] {
        if let Some(rest) = s.strip_prefix(w) {
            s = rest.to_string();
        }
        if let Some(rest) = s.strip_suffix(w) {
            s = rest.to_string();
        }
    }
    s.trim().to_string()
}

