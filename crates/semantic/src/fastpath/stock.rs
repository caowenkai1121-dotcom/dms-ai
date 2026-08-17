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
use crate::fastpath::{derive::*, finance::*, graph_rows::*, ops::*, relation::*, sales::*, template::*};

use crate::sales_fact;

/// 当前库存是快照量，必须只取 `product_stock_date` 的全表最大批次。
/// DMS `WincReportServiceImpl` 也按该列倒序展示；直接 SUM 全历史会把每日快照累加。
pub fn stock_province_predicate(question: &str) -> Result<Option<String>, ()> {
    let mut hit = None;
    for &(code, name) in crate::present::PROVINCE_LABELS {
        let code_hit = question.match_indices(code).any(|(at, _)| {
            let before = question[..at].chars().next_back();
            let after = question[at + code.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_digit())
                && !after.is_some_and(|c| c.is_ascii_digit())
        });
        // 先粗筛再拼短语：每个候选短语都含省名本身，问句不含该省时 7 个 format! 全白做
        if !code_hit && !question.contains(name) {
            continue;
        }
        let phrases = [
            format!("{name}壮族自治区"), format!("{name}回族自治区"),
            format!("{name}维吾尔自治区"), format!("{name}自治区"),
            format!("{name}省"), format!("{name}市"), name.to_string(),
        ];
        let phrase = phrases.into_iter().find(|p| question.contains(p));
        let phrase = phrase.unwrap_or_else(|| code.to_string());
        let consumed = [
            "库存金额", "库存货值", "库存数量", "库存总额", "库存总量", "库存额", "库存量",
            "总库存", "总存货", "库存", "存货", "金额", "货值", "数量", "总额", "总量", "合计",
            "省区", "省份", "地区", "区域", phrase.as_str(),
        ];
        if !residual_text(question, &consumed).is_empty() {
            return Err(()); // 省名只是商品/客户等实体的一部分，不能静默当省区过滤。
        }
        if hit.is_some() {
            return Err(()); // 多省问法不是单省快照，回落给能表达多值过滤的路径。
        }
        hit = Some((code, name));
    }
    Ok(hit.map(|(code, name)| format!(
        "province IN ('{name}','{name}省','{name}市','{name}自治区',\
         '{name}壮族自治区','{name}回族自治区','{name}维吾尔自治区','{code}')"
    )))
}




/// 仅提取库存问句里的商品限定；空串表示用户问的是通用库存总量。
/// 这里不猜 SKU：返回的片段还必须经过 WMS 实表唯一性探针才能进入查询谓词。
pub fn stock_product_fragment(question: &str) -> Option<String> {
    // 只剥问句边界，绝不在实体内部 `replace`。旧实现会把「美的冰箱」里的「的」删成
    // 「美冰箱」，随后唯一性探针零命中；同理「有友」等合法品牌也不能被单字虚词破坏。
    const PREFIXES: &[&str] = &[
        "请帮我查询一下",
        "请帮我查一下",
        "帮我查询一下",
        "帮我查一下",
        "请查询一下",
        "请查一下",
        "查询一下",
        "查一下",
        "帮我查询",
        "帮我查",
        "请查询",
        "请查",
        "帮我看看",
        "请问",
        "麻烦查询",
        "麻烦查",
        "麻烦",
        "查询",
        "查查",
        "帮我",
        "看看",
        "看一下",
        "看下",
        "查",
        "目前",
        "现在",
        "当前",
        "商品",
        "产品",
        "SKU",
        "sku",
        "Sku",
    ];
    const STOCK_WORDS: &[&str] = &[
        "的库存信息",
        "的库存情况",
        "的库存数量",
        "的库存总量",
        "的库存总数",
        "的库存量",
        "的总库存",
        "的存货信息",
        "的存货情况",
        "的存货数量",
        "的存货总量",
        "的存货量",
        "的总存货",
        "的库存",
        "的存货",
        "库存信息",
        "库存情况",
        "库存数量",
        "库存总量",
        "库存总数",
        "库存量",
        "总库存",
        "库存",
        "存货信息",
        "存货情况",
        "存货数量",
        "存货总量",
        "存货量",
        "总存货",
        "存货",
    ];
    const TAILS: &[&str] = &[
        "分别是多少",
        "一共有多少",
        "总共有多少",
        "还有多少",
        "还剩多少",
        "剩余多少",
        "数量是多少",
        "总量是多少",
        "总数是多少",
        "有多少",
        "是多少",
        "怎么样",
        "如何",
        "多少",
        "一下",
        "吗",
        "呢",
        "啊",
        "呀",
    ];

    let trim = |value: &str| {
        value
            .trim_matches(|c: char| c.is_whitespace() || RESIDUAL_PUNCT.contains(c))
            .to_string()
    };
    let mut fragment = trim(question);
    loop {
        let before = fragment.clone();
        if let Some(rest) = PREFIXES.iter().find_map(|word| fragment.strip_prefix(word)) {
            fragment = trim(rest);
        } else if let Some(rest) = TAILS.iter().find_map(|word| fragment.strip_suffix(word)) {
            fragment = trim(rest);
        } else if let Some(rest) = STOCK_WORDS
            .iter()
            .find_map(|word| fragment.strip_suffix(word))
        {
            fragment = trim(rest);
        } else if let Some(rest) = STOCK_WORDS
            .iter()
            .find_map(|word| fragment.strip_prefix(word))
        {
            fragment = trim(rest);
        }
        if fragment == before {
            break;
        }
    }
    (!fragment.is_empty()).then_some(fragment)
}




pub fn stock_sku_predicate(code: &str) -> String {
    format!("sku_code = '{}'", rel_quote(code))
}




/// 商品库存不能唯一落到一个 WMS SKU 时的终止卡：只读字典占位行，不扫任何业务事实。
pub fn stock_product_unavailable(fragment: &str, reason: &str) -> DirectHit {
    let fragment: String = fragment.chars().take(40).collect();
    DirectHit {
        outcome: DirectOutcome::Clarification(format!(
            "商品限定「{fragment}」还不能唯一匹配到一个库存商品：{reason}。请补充准确商品编码或完整商品名称后重试。"
        )),
        ..hit(String::new(), "direct-doc")
    }
}




/// 模板兑现了哪个指标，模板自己说 —— `compose::hit` 造出来的 `intent_evidence` 是空的，
/// 于是收据里每个槽位都只敢标「grounded（抽到了但没证明落进 SQL）」，
/// 而这些 SQL 是代码写死的、指标名就是它自己写的列别名（生产回归 E10 的最后一格）。
///
/// 六个 return 各写一遍必漂：证据取自**产出的 SQL 别名本身**，一处收口。
/// 兜底桶的字面量。**只许写这一处**：`COALESCE(NULLIF(x,''),'未知')` 造出来的不是成员，
/// 是「这些行没登记」。排行要排掉它、分布要留着它，两处判据都得引同一个字面量，
/// 抄第二遍就会漂（2026-08-16 城市那条就是只改了一边）。
const UNREGISTERED_BUCKET: &str = "未知";

pub fn stock_snapshot(question: &str) -> Option<DirectHit> {
    let mut hit = stock_snapshot_sql(question)?;
    let metric = if hit.sql.contains("`库存金额`") { "库存金额" } else { "库存量" };
    hit.intent_evidence = std::mem::take(&mut hit.intent_evidence)
        .resolve(IntentSlotKind::Metric, metric);
    Some(hit)
}

fn stock_snapshot_sql(question: &str) -> Option<DirectHit> {
    if !["库存", "存货"].iter().any(|w| question.contains(w)) {
        return None;
    }
    let wants_amount = ["金额", "货值", "库存额"].iter().any(|w| question.contains(w));
    if !wants_amount && !["量", "数量", "多少", "库存"].iter().any(|w| question.contains(w)) {
        return None;
    }
    let grouped_province = ["各省", "各省份", "按省", "按省份", "省份分别", "省区分别"]
        .iter()
        .any(|word| question.contains(word));
    // 裸「前」不是触发词（「目前的库存」会误中）：「前+N/前十」由 `detect_top_n`
    // 带时间单位黑名单判；「top」归一小写后判一次
    // 🔴 「各仓库/按仓库/每个仓库」也是分组诉求（2026-08-15 生产直打 + 复验 3/3）：
    // 此前触发词只有排行词，于是「各仓库库存量」「按仓库看库存量」全落 need-intent ——
    // 而它要的正是这条分支已经会写的那句 GROUP BY。
    let grouped_warehouse = question.contains("仓库")
        && (["哪个", "最高", "最多", "最大", "最少", "最小", "最低", "排行", "排名",
             "各仓库", "按仓库", "每个仓库", "各个仓库", "分仓库"]
            .iter()
            .any(|word| question.contains(word))
            || question.to_lowercase().contains("top")
            || detect_top_n(question) < 200);
    let province = if grouped_province { None } else { stock_province_predicate(question).ok()? };

    // 默认库存源 = 业务中台 WMS 现行库存（ywzt_ods.scm_warehous_manage，2026-08-11 用户指定：
    // 「库存表用中台的」）。但它**无金额列、无省份列**——金额与省份两类问法仍走营销通
    // 门店进销存快照（那两类语义本属门店/经销商侧，总行与合计不许跨源混加）。
    if !wants_amount && !grouped_province && province.is_none() {
        const ZT_FROM: &str = "ywzt_ods.scm_warehous_manage";
        // 只计正品在库；残损/临期等其余 inventory_status 须点名才计（合同卡同口径）
        const ZT_WHERE: &str = "inventory_status = 'ZP'";
        // 🔴 冻结/锁定是**同一张表上的另外两列**，不是商品名（2026-08-15 生产直打 + 复验 4/4）：
        // 「冻结库存量」此前落 need-intent，卡面说「商品限定「冻结」还不能唯一匹配到一个
        // 库存商品」—— 而 `freeze_quantity`（97.6 万）与 `lock_quantity`（3.3 万）就在这张表上。
        // 把度量词落进「商品限定」槽位，唯一匹配必然失败。
        if let Some((col, label)) = [
            ("freeze_quantity", "冻结库存量", ["冻结"].as_slice()),
            ("lock_quantity", "锁定库存量", ["锁定", "锁库"].as_slice()),
        ]
        .iter()
        .find_map(|(col, label, words)| {
            words.iter().any(|w| question.contains(w)).then_some((*col, *label))
        }) {
            let mut freeze = hit(
                format!("SELECT COALESCE(SUM({col}),0) AS `{label}` FROM {ZT_FROM} WHERE {ZT_WHERE}"),
                "direct-agg",
            );
            // 模板自报兑现了哪个指标（与外层出口同一条纪律）
            freeze.intent_evidence = std::mem::take(&mut freeze.intent_evidence)
                .resolve(IntentSlotKind::Metric, label);
            return Some(freeze);
        }
        // 🔴 仓库分组的判断**排在商品残留守卫之前**（2026-08-15）：
        // 「各仓库库存量」里的「各仓库」会被 `stock_product_fragment` 当成商品名残留，
        // 于是整条被那道守卫拒掉 —— 而它要的正是下面这句 GROUP BY，与商品无关。
        // 顺序即行为：先判「这是不是按仓库分组」，再判「有没有商品限定」。
        if grouped_warehouse {
            // 🔴 出**仓库名**而不是 `wms_code`（2026-08-17）：此前这里的注释写着
            // 「仓库码表未镜像进数仓，按码与库位出（名称接入后再换名）」—— 那一天到了。
            // 仓库名唯一活在 `ywzt_ods.master_wms.wms_desc` 上（539 行，主键唯一），
            // 今天已随业主指路（「一般 master 开头的都是主数据」）进目录并登记 JOIN 边。
            // 用户看到的是「京东常温酱料虚拟仓」而不是 `GZHYC-777` —— 后者他没法据以行动。
            // LEFT JOIN + COALESCE：实测 31258 行库存里有 12 行没有仓档，
            // 内连会把它们**静默丢掉**（合计对不上），左连回落到编码，一行不丢。
            return Some(hit(
                format!(
                    "SELECT COALESCE(NULLIF(w.wms_desc,''), s.wms_code) AS `仓库`, \
                            s.location AS `库位`, \
                            SUM(s.in_stock_quantity) AS `库存量` \
                     FROM {ZT_FROM} s \
                     LEFT JOIN ywzt_ods.master_wms w ON w.wms_code = s.wms_code \
                     WHERE s.{ZT_WHERE} \
                     GROUP BY 1, 2 \
                     ORDER BY `库存量` {} LIMIT {}",
                    rank_direction(question),
                    ranking_limit(question)
                ),
                "direct-agg",
            ));
        }
        // 带商品残留的库存题必须走 `stock_product_filtered` 实表探针；
        // 同步模板只接通用总量（仓库分组已在上面接走）。
        if stock_product_fragment(question).is_some() {
            return None;
        }
        return Some(DirectHit {
            detail: Some(format!(
                "SELECT sku_code AS `商品编码`, sku_name AS `商品`, \
                        SUM(in_stock_quantity) AS `库存量` \
                 FROM {ZT_FROM} WHERE {ZT_WHERE} \
                 GROUP BY sku_code, sku_name \
                 ORDER BY `库存量` DESC LIMIT 20"
            )),
            ..hit(
                format!(
                    "SELECT COALESCE(SUM(in_stock_quantity),0) AS `库存量` \
                     FROM {ZT_FROM} WHERE {ZT_WHERE}"
                ),
                "direct-agg",
            )
        });
    }

    // ── 营销通门店进销存快照路径（金额/省份限定专用；库存量的默认源已是中台表）──
    //
    // 🔴 口径要写在**列名**上（2026-08-15 生产直打 + 复验 4/4）：
    // 「福建库存量」这类带省份的问法会从中台 WMS 掉到这张门店/经销商进销存快照表 ——
    // 那是**另一个口径**（门店/经销商在库，不是公司仓库存），而此前答案里没有任何提示，
    // 用户拿到 200048 会以为那是「福建的公司库存」。
    // 中台表无省份列，这条掉落本身是有意的（注释在上面那段）；缺的只是**说清楚**。
    // 写进列别名是零管道成本的披露：`DirectHit` 没有备注字段，而列名一定会上屏。
    // 用户自己点名 WinC/营销通/门店时不加后缀 —— 那时他知道自己在问什么。
    // 只有**库存量**需要这个后缀：它的默认源是中台 WMS，掉到这里是口径被换掉了。
    // 库存金额本来就只有这一张表（中台无金额列），加后缀是纯噪声。
    // 用户自己点名 WinC/营销通/门店/进销存时也不加 —— 那时他知道自己在问什么。
    let names_winc = ["winc", "WinC", "WINC", "营销通", "经销商", "门店", "进销存"]
        .iter()
        .any(|word| question.contains(word));
    let (column, label) = if wants_amount {
        ("stock_amount", "库存金额".to_string())
    } else if names_winc {
        ("stock_quantity", "库存量".to_string())
    } else {
        ("stock_quantity", "库存量（门店进销存口径）".to_string())
    };
    let label = label.as_str();
    let latest = "product_stock_date = (SELECT MAX(product_stock_date) \
                  FROM t_winc_stock_report WHERE deleted_flag = 0)";
    let where_sql = match province {
        Some(p) => format!("deleted_flag = 0 AND {latest} AND {p}"),
        None => format!("deleted_flag = 0 AND {latest}"),
    };
    if grouped_warehouse {
        // 🔴 **合成兜底桶不许占名次**（2026-08-17 审计；与 2026-08-16「最高的城市答『未知』」
        // 是同一个病，那次只修了 sales_fact 一边）。
        //
        // `COALESCE(…,'未知')` 造出来的不是一个成员，是「这些行没登记仓库名」。
        // 而 grouped_warehouse 的触发词里就有 哪个/最高/最多/排行/top/前N ——
        // 这条 SQL 确确实实在发名次。榜首返回「未知」，用户读成「有个仓库叫未知」
        // 或者「系统坏了」，真实的第一名被挤到第二。
        //
        // 排掉它，同时把「被排掉的那一桶有多少」用同窗同谓词的单值查询挂进 detail ——
        // **排除必须说出来**，否则合计对不上又成了另一个谜。
        let bucket = format!("COALESCE(NULLIF(warehouse_name,''),'{UNREGISTERED_BUCKET}')");
        let ranked = format!(
            "SELECT {bucket} AS `仓库`, SUM({column}) AS `{label}`              FROM t_winc_stock_report WHERE {where_sql} AND {bucket} <> '{UNREGISTERED_BUCKET}'              GROUP BY {bucket} ORDER BY `{label}` {} LIMIT {}",
            rank_direction(question),
            ranking_limit(question)
        );
        let excluded = format!(
            "SELECT '{UNREGISTERED_BUCKET}' AS `仓库`, SUM({column}) AS `{label}`              FROM t_winc_stock_report WHERE {where_sql} AND {bucket} = '{UNREGISTERED_BUCKET}'"
        );
        return Some(DirectHit { detail: Some(excluded), ..hit(ranked, "direct-agg") });
    }
    if grouped_province {
        // 省份那支是**分布**不是名次（无 LIMIT），按同一条裁决可以留桶 ——
        // 排行发名次才必须排掉；分布排掉反而把「有多少没登记」这件事藏起来。
        return Some(hit(
            format!(
                "SELECT COALESCE(NULLIF(province,''),'未知') AS `省份`, \
                        SUM({column}) AS `{label}` \
                 FROM t_winc_stock_report WHERE {where_sql} \
                 GROUP BY COALESCE(NULLIF(province,''),'未知') \
                 ORDER BY `{label}` DESC"
            ),
            "direct-agg",
        ));
    }
    Some(DirectHit {
        detail: Some(format!(
            "SELECT COALESCE(NULLIF(product_type,''),'未分类') AS `商品类型`, \
                    COALESCE(SUM({column}),0) AS `{label}` \
             FROM t_winc_stock_report WHERE {where_sql} \
             GROUP BY COALESCE(NULLIF(product_type,''),'未分类') \
             ORDER BY `{label}` DESC LIMIT 20"
        )),
        ..hit(
            format!(
                "SELECT COALESCE(SUM({column}),0) AS `{label}` \
                 FROM t_winc_stock_report WHERE {where_sql}"
            ),
            "direct-agg",
        )
    })
}




pub fn stock_product_snapshot(sku_predicate: &str, surface: &str) -> DirectHit {
    const ZT_FROM: &str = "ywzt_ods.scm_warehous_manage";
    let where_sql = format!("inventory_status = 'ZP' AND {sku_predicate}");
    DirectHit {
        intent_evidence: crate::ExecutionEvidence::default()
            .resolve(crate::IntentSlotKind::Entity, surface)
            .resolve(crate::IntentSlotKind::Metric, "库存量"),
        detail: Some(format!(
            "SELECT wms_code AS `仓库编码`, location AS `库位`, batch AS `批次`, \
                    SUM(in_stock_quantity) AS `库存量`, SUM(lock_quantity) AS `锁定量`, \
                    SUM(freeze_quantity) AS `冻结量`, MAX(invalid_date) AS `效期` \
             FROM {ZT_FROM} WHERE {where_sql} \
             GROUP BY wms_code, location, batch ORDER BY `库存量` DESC LIMIT 200"
        )),
        ..hit(
            format!(
                "SELECT MAX(sku_code) AS `商品编码`, MAX(sku_name) AS `商品`, \
                        COALESCE(SUM(in_stock_quantity),0) AS `库存量`, \
                        COALESCE(SUM(lock_quantity),0) AS `锁定量`, \
                        COALESCE(SUM(freeze_quantity),0) AS `冻结量` \
                 FROM {ZT_FROM} WHERE {where_sql}"
            ),
            "direct-agg",
        )
    }
}





#[cfg(test)]
mod warehouse_group_tests {
    /// 🔴 排行不许把合成兜底桶发成名次（2026-08-17 审计）。
    ///
    /// 与 2026-08-16「本月销售额最高的城市：未知」是同一个病 —— 那次只修了 sales_fact 一边，
    /// 库存这边照旧。`COALESCE(…,'未知')` 造出来的不是一个仓库，是「这些行没登记仓库名」。
    /// 榜首返回「未知」，用户读成「有个仓库叫未知」，真实第一名被挤到第二。
    #[test]
    fn a_warehouse_ranking_never_awards_a_place_to_the_synthetic_bucket() {
        let hit = stock_snapshot("库存金额最高的10个仓库").expect("该命中仓库排行");
        assert!(
            hit.sql.contains("<> '未知'"),
            "合成桶不许占名次：{}",
            hit.sql
        );
        assert!(
            hit.detail.as_deref().is_some_and(|d| d.contains("= '未知'")),
            "被排掉的那一桶必须单独说出来，否则合计对不上又成了一个谜：{:?}",
            hit.detail
        );
        // 省份那支是分布不是名次：留桶（排掉反而把「有多少没登记」藏起来）
        let dist = stock_snapshot("各省份库存量分布").expect("该命中省份分布");
        assert!(!dist.sql.contains("<> '未知'"), "分布不该排掉兜底桶：{}", dist.sql);
    }

    use super::*;

    /// 「各仓库/按仓库」是分组诉求，不是排行诉求。
    ///
    /// 🔴 由来（2026-08-15 生产直打 + 复验 3/3）：触发词此前只有排行词
    /// （哪个/最多/最高/排行/top/前N），于是「各仓库库存量」「按仓库看库存量」
    /// 全落 need-intent —— 而它要的正是这条分支已经会写的那句 GROUP BY。
    /// 冻结/锁定是同一张表上的另外两列，不是商品名。
    ///
    /// 🔴 由来（2026-08-15 生产直打 + 复验 4/4）：「冻结库存量」落 need-intent，
    /// 卡面说「商品限定「冻结」还不能唯一匹配到一个库存商品」——
    /// 而 `freeze_quantity`（97.6 万）与 `lock_quantity`（3.3 万）就在这张表上。
    #[test]
    fn frozen_and_locked_are_columns_not_product_names() {
        let frozen = stock_snapshot("冻结库存量").expect("冻结库存量该命中");
        assert!(frozen.sql.contains("SUM(freeze_quantity)"), "{}", frozen.sql);
        assert!(frozen.sql.contains("`冻结库存量`"), "{}", frozen.sql);
        let locked = stock_snapshot("锁定库存量是多少").expect("锁定库存量该命中");
        assert!(locked.sql.contains("SUM(lock_quantity)"), "{}", locked.sql);
        // 通用总量照旧走在库数量，不许被这条带偏
        let total = stock_snapshot("现在库存量是多少").expect("总量照旧");
        assert!(total.sql.contains("SUM(in_stock_quantity)"), "{}", total.sql);
    }

    #[test]
    fn grouping_by_warehouse_is_a_grouping_ask_not_only_a_ranking_one() {
        for q in ["各仓库库存量", "按仓库看库存量", "每个仓库的库存量", "分仓库库存量"] {
            let hit = stock_snapshot(q).unwrap_or_else(|| panic!("{q} 该命中"));
            assert!(hit.sql.contains("GROUP BY 1, 2"), "{q}：{}", hit.sql);
            // 🔴 2026-08-17 起必须出**仓库名**：`wms_code` 是 GZHYC-777 这种不透明编码，
            // 用户没法据以行动。名字唯一活在 master_wms.wms_desc 上。
            assert!(
                hit.sql.contains("master_wms") && hit.sql.contains("wms_desc"),
                "{q}：仓库分组必须 JOIN 主数据出名字，不能只给编码：{}",
                hit.sql
            );
            // 左连不丢行：实测 31258 行库存里有 12 行没有仓档，内连会把它们静默丢掉，合计对不上
            assert!(
                hit.sql.contains("LEFT JOIN ywzt_ods.master_wms"),
                "{q}：必须 LEFT JOIN —— 内连会把无仓档的行静默丢掉：{}",
                hit.sql
            );
        }
        // 排行诉求照旧（不许被这条改动带偏）
        let rank = stock_snapshot("库存量最多的仓库").expect("排行照旧");
        assert!(rank.sql.contains("ORDER BY"), "{}", rank.sql);
        // 不含「仓库」的通用总量照旧走单值
        let total = stock_snapshot("现在库存量是多少").expect("总量照旧");
        assert!(!total.sql.contains("GROUP BY"), "{}", total.sql);
    }
}
