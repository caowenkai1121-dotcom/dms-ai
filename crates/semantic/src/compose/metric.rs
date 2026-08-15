//! T8 搬运：逐行迁自 `server/src/direct.rs`（**只搬不改**，一个字节的行为改动都会让
//! `evaluation.py` 的逐题结果集对拍失去意义）。顺序即行为，只提取不重排。

#![allow(clippy::too_many_arguments)]

use std::collections::{HashMap, HashSet};

use sqlx::PgPool;

use dms_kernel::nl::text::strip_annotations;
use dms_kernel::nl::time::{detect_top_n, fill_time_col, prev_window, time_predicate, yoy_window};
use dms_kernel::sql::lex::{base_col_refs, from_table_aliases, qualify_cols};

use crate::compose::*;
use crate::registry::model::{DimensionDef as DimDef, JoinEdge, MetricDef, TableSnapshot, ValueRef};
use crate::{DirectHit, DirectOutcome, ExecutionEvidence, IntentSlotKind, Relation};

// 同批搬来的兄弟模块（原文件里是同一个作用域，拆文件后要显式引）
#[allow(unused_imports)]
use crate::compose::{assemble::*, path::*, values::*};
#[allow(unused_imports)]
use crate::fastpath::{derive::*, finance::*, graph_rows::*, ops::*, relation::*, sales::*, stock::*, template::*};

use crate::sales_fact;

/// 【指标 only】通用装配：**只有指标、没有维度**的问句（「本月开票金额」「今年售后单有多少」）。
///
/// 🔴 为什么必须有它（实测出来的，不是设计洁癖）：38 题的 route 分布是
/// `llm 24 / direct-agg 8 / llm+repair 5 / semantic-cache 1` —— **76% 过 LLM，
/// 而全部失败都在 LLM 路径**（确定性路径至今 0 失败）。
/// 用 `why-not-compose` 逐题诊断后，最大的一档是 **② 维度不命中 17 题** ——
/// 因为 `try_compose` **强制要求维度**，而「无维度」这条路今天只有 `agg_template`
/// 一个硬编码模板，且它只认 4 个指标（销售额/订单数/客单价/成交客户数）。
/// 于是「本月开票金额」这种**声明齐全**的问句照旧被丢给 LLM。
///
/// 实现上**不写第二个装配器**：造一个 `expr` 为空的伪维度喂给 `compose_sql_with`，
/// 由它的「无维度模式」出 SQL。这样去重子查询下推、表级口径、时间桥接、
/// 扇出检查、残留守卫全部复用同一份 —— 抄第二份必然漂出「两条路口径不一致」。
///
/// 三道自己的门（`compose_sql_with` 的那些照旧生效）：
/// ① **维度命中就不接**：用户要了分组却给单值，是答非所问，交给 `try_compose`；
/// ② 快照表不接（同 `compose_gated`）；
/// ③ 残留守卫由伪维度的空名/空别名参与 —— 消化词是指标名/别名**加上能被 `meta.value_map`
///    唯一解释的值过滤名**：「本月湖南省的销售额」的「湖南」如今被声明消化并装成
///    `cus.province = '430000'`；解释不了或装不上（G1/G2）则照旧残留 → 回落 LLM。
///
/// `agg_template_hit`：调用方对同一问句已算过的 `agg_template` 命中结果（让路判定）——
/// `compose_hit` 的让路门刚跑过这一遍，传进来免得 `metric_only` 再全句重扫一次。
pub async fn try_compose_metric_only(
    pg: &sqlx::PgPool,
    ds: &str,
    question: &str,
    agg_template_hit: bool,
) -> Option<DirectHit> {
    if !warehouse_sales_metrics(question).is_empty() {
        return None;
    }
    if ds == crate::registry::datasource::DMS_DS_ID {
        // 🔴 带上**已兑现槽位的证明**（2026-08-14 回归 OPS04）：这条 SQL 是代码写死的，
        // 时间窗是字面日期、省区是 `CASE(...) = '湖南'`，覆盖闸按 LLM SQL 的形状认不出来，
        // 于是判 `missing: time:2026年6月` 把整条运营口径路挡回去 —— 它平时根本不生效。
        if let Some((sql, _name, evidence)) =
            crate::ops_caliber::direct_metric_with_evidence(question)
        {
            let mut h = hit(crate::registry::warehouse_qualified_source(ds, &sql), "direct-agg");
            h.intent_evidence = evidence;
            return Some(h);
        }
    }
    match metric_only(pg, ds, question, agg_template_hit).await {
        MetricOnly::Hit(h) => Some(h),
        _ => None,
    }
}


/// 指标 only 的判定结果。**有名字的拒绝理由**，而不是一个 `None` ——
/// 诊断口（`why_not_compose`）要报「被哪道门挡的」，而它必须与真正的判定
/// **共用同一份实现**：诊断自己重判一遍就会漂出「诊断说能装配、实际回落」。
pub enum MetricOnly {
    Hit(DirectHit),
    /// 硬编码模板能接 → 让路（数与 KPI 环比都以模板为准）
    YieldToTemplate,
    /// 指标不命中
    NoMetric,
    /// 维度命中了 → 那是 `try_compose` 的活
    DimPresent,
    /// 来源是快照表
    Snapshot,
    /// 装配器自己拒（含 SELECT 的口径 / UNION 多流来源 / 去重键不全 / 残留守卫 / 时间窗放不下）
    ComposeRefused,
    /// 注册表读失败。**与「指标不命中」分开**：前者是声明没写，后者是声明读不到 ——
    /// 合成一句会让下一个人照着报告去补一个已经存在的声明。
    RegistryDown(&'static str),
}


pub async fn metric_only(pg: &sqlx::PgPool, ds: &str, question: &str, agg_template_hit: bool) -> MetricOnly {
    use crate::registry::model as reg;
    macro_rules! load {
        ($what:literal, $call:expr) => {
            match $call {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("注册表读失败：{} —— 指标 only 整条放弃，回落 LLM（{e}）", $what);
                    return MetricOnly::RegistryDown($what);
                }
            }
        };
    }
    // 与 `try_compose` 同一条修法：互不依赖的注册表读并发发起，按原顺序判失败。
    let (metrics, dims) = tokio::join!(reg::load_metrics(pg, ds), reg::load_dimensions(pg, ds));
    let metrics = load!("meta.metric", metrics);
    let dims = load!("meta.dimension", dims);
    // ⓿ 订单口径模板能接的，一律让给它。
    //
    // 默认销售额已经由 DWS 事实快路径处理，不再参与此门。这里仍保留订单数、
    // 成交客户数和客单价的让路，避免伪维度命中及客单价精度漂移。
    // Router 里 `direct-agg`（本函数所在的成员）排在 `direct-doc`（`agg_template`）**之前**，
    // 所以不设门的话订单口径标量仍可能被本函数抢走。
    //
    // ① ~~数不一样~~ —— **这条已订正、不成立**。我一度写「模板走订单头、声明走明细表，
    //    那正是 item_type 那件未裁决的事」。实查 `seed_defs.rs:19-48`：
    //    `order_count` / `buyer_count` / `avg_order_value` 三条声明的
    //    `source_table` 是 `t_sales_order`，`agg_expr` 与 `scope_filter` 与模板逐字相同。
    //    ~~唯一真差异是客单价的 ROUND~~ —— **已消（2026-08-15）**：声明补上了 `ROUND(…, 2)`，
    //    两条路逐字同形。此前生产实测同一句「客单价」两条路给 11317.72 与 11318.33052890。
    //    默认销售额已迁出本模板，由 `sales_fact` 单独负责。
    // ② ~~本函数不出上期查询~~ —— 已消（二·AC：装配器出 KPI 环比，与模板同形）。
    // ③ **伪维度命中**：撤门实测「本月成交客户数」首格从 `1625` 变成一个客户名
    //    （200 行每行 1，route 仍 `direct-agg`、无报错）。二·AS1 已在 `pick_excluding` 里根治，
    //    但那是**装配器这一侧**的修法；门撤掉还要逐题对拍数字才算安全（二·AR）。
    // ④ ~~客单价丢 ROUND~~ —— **已消（2026-08-15，声明补上 ROUND）**。
    // 撤门的前置现在只剩 ③ 的逐题对拍。
    // 命中结果由调用方传入（`compose_hit` 的让路门对同一问句刚算过同一函数，别重扫一遍）
    if agg_template_hit {
        return MetricOnly::YieldToTemplate;
    }
    let Some((m, m_word)) = pick(question, &metrics, |x| (&x.name, &x.aliases)) else {
        return MetricOnly::NoMetric;
    };
    // 同样减词：不减的话「上周成交客户数」会被判成「有维度」→ 连指标 only 这条路也走不了
    // （伪维度把两条确定性路径一起堵死，实测那两句只能回落 LLM 或出 200 行名单）
    if pick_excluding(question, &dims, |x| (&x.name, &x.aliases), &m_word).is_some() {
        return MetricOnly::DimPresent; // 有维度 → 那是 `try_compose` 的活
    }
    let (edges, scopes, snaps, vals) = tokio::join!(
        reg::load_join_edges(pg, ds),
        reg::load_table_scopes(pg, ds),
        reg::load_table_snapshots(pg, ds),
        reg::load_value_map(pg, ds),
    );
    let edges = load!("meta.join_edge", edges);
    let scopes = load!("meta.table_scope", scopes);
    let snaps = load!("meta.table_snapshot", snaps);
    // 值域读不到只会少一点确定性覆盖（空表 = 没有值名被消化 = 残留守卫照旧拦），不会出错数
    let vals = match vals {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("注册表读失败：meta.value_map —— 值过滤本轮不生效（{e}）");
            vec![]
        }
    };
    let Some(base) = dms_kernel::sql::lex::first_ident_of(&strip_annotations(&m.source_table)) else {
        return MetricOnly::ComposeRefused; // 来源表声明取不出标识符
    };
    if snaps.iter().any(|s| s.table_name.eq_ignore_ascii_case(&base)) {
        return MetricOnly::Snapshot; // 快照表：平铺聚合会把历史流水全求和
    }
    // 伪维度：来源表 = 指标自己的基表（走 `dim_base == m_src` 那条同基表分支，不必找 join 路径），
    // `expr`/`name`/`aliases` 全空 → 无维度模式 + 残留守卫只消化指标词。
    let pseudo = DimDef {
        name: String::new(),
        aliases: vec![],
        source_table: format!("{base} b0"),
        expr: String::new(),
    };
    let Some(sql) = compose_sql_with_snap(m, &pseudo, question, &edges, &scopes, None, None, &vals)
    else {
        return MetricOnly::ComposeRefused;
    };
    let sql = crate::registry::warehouse_qualified_source(ds, &sql);
    // 🔴 KPI 环比：与 `agg_template` 同形 —— 同一段装配、只把时间窗换成平移后的上期。
    //
    // 为什么只在**无维度**这一支出 `prev`：`hits::patch_prev` 取结果首格算 Δ%，
    // 而带维度时首格是维度值（字符串）→ `cell_num` 返 None → 环比本来就用不上，
    // 多发一次上期取数是白花。`agg_template` 也只在无维度时出 prev。
    //
    // 这一条同时消掉了让路门的**第二条**理由（「指标 only 不出环比，换过去会静默丢功能」）。
    // 剩下的第一条（销售额的 `item_type` 取 '1' 还是 '3'）是业务裁决，代码修不了。
    let prev = prev_window(question).and_then(|(tpl, label)| {
        compose_sql_with_snap(m, &pseudo, question, &edges, &scopes, None, Some(tpl), &vals)
            .map(|s| (crate::registry::warehouse_qualified_source(ds, &s), label.to_string()))
    });
    let comparisons = yoy_window(question)
        .and_then(|(tpl, label)| {
            compose_sql_with_snap(m, &pseudo, question, &edges, &scopes, None, Some(tpl), &vals)
                .map(|s| (crate::registry::warehouse_qualified_source(ds, &s), label.to_string()))
        })
        .into_iter()
        .collect();
    MetricOnly::Hit(DirectHit { prev, comparisons, ..hit(sql, "direct-agg") })
}


/// 残留守卫（纯函数）：把问句里被模板/组合器「消化掉」的词剥光后，
/// 若还剩实义字（CJK/字母数字）→ 说明问句含模板表达不了的限定（实体名、值过滤、
/// 未支持的维度），必须回落 LLM，绝不能装配一条**丢掉限定**的 SQL 静默答错。
///
/// 真实翻车（回归 E16 抓获）：「线下客户本月销售额」被销售额×客户模板装配成
/// 「全部客户 TOP200 销售额」——"线下"这个客户分类过滤被静默丢弃，答非所问。
/// 通用虚词表在 `kernel::nl::lexicon::STRIP_WORDS`（单一事实源），算法在 `kernel::nl::text`。
pub fn has_residue(question: &str, consumed: &[String]) -> bool {
    let stripped = dms_kernel::nl::time::strip_explicit_date_range(question);
    dms_kernel::nl::text::has_residue_with(
        stripped.as_deref().unwrap_or(question),
        consumed,
        dms_kernel::nl::lexicon::STRIP_WORDS,
    )
}

