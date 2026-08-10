//! 指标命中的结构化召回 + 口径卡渲染（口径 / 时间列 / 去重键 / 说明四段）。
//! 变更原因＝指标口径卡的形状与命中净化。
//!
//! 搬运源 `server/src/meta.rs:996-1062`（`MetricHit` / `recall_metric_hits_ds` / `metric_card` /
//! `recall_metrics`，SQL 文本、绑定序号、四段拼串顺序逐字保留）与 `server/src/meta.rs:2036-2104`
//! （MapFilter 净化的 7 个断言，断言体一字不改）。
//!
//! `MetricHit` 是**结构化**的（不是渲染好的字符串）：口径卡渲染与口径校正器
//! （`correct_caliber` 要拿 `scope_filter`/`dedup_keys`）吃同一份，避免口径出现第二处真相。

use crate::recall::RecallCtx;
use crate::registry::caliber::{UNIT_PERCENT, UNIT_RATIO};
use crate::registry::{
    catalog_allows_metric_dimension, catalog_allows_metric_record, source_asset_live_pred_at,
    warehouse_qualified_source,
};
use sqlx::PgPool;

/// 最长别名命中 + MapFilter 命中净化四规则：纯文本基元，已收进 kernel（`nl::text`）。
/// 净化规则的行为契约与 7 个断言原地留在本文件（裁决 T7-3）。
pub use dms_kernel::nl::text::{map_filter, match_word};

/// 命中的指标（结构化，供口径卡渲染与口径校正器共用——单一事实源）
#[derive(Clone)]
pub struct MetricHit {
    pub name: String,
    pub source_table: String,
    pub agg_expr: String,
    pub scope_filter: String,
    pub time_col: String,
    pub dedup_keys: String,
    pub description: String,
    /// `meta.metric.unit`；百分数与小数比值约定见 `registry::caliber::{UNIT_PERCENT, UNIT_RATIO}`。
    pub unit: String,
    /// `meta.metric.time_cap`：指标级时间窗上限（'' = 无；'yesterday' = 算到昨天）。
    /// 指标级数据新鲜度上限；默认 DWS 销售事实当前不设置该上限。
    /// 只进口径卡提示（`metric_card`），不进判据 —— 时间窗的合法写法太多
    /// （`< CURDATE()` / `<= CURDATE()-1` / `DATE() = 昨天`），AST 判「排除了今天」误伤面大。
    pub time_cap: String,
    /// 口径版本与允许分析维度：深度模式和 LLM 都能看到治理边界。
    pub version: String,
    pub allowed_dimensions: Vec<String>,
    /// 命中词（问句里逐字出现的那一段）。【A17 ②】口径二选一 chip 的判据：
    /// 两个指标的命中词一样长 = 问句没说清是哪个 —— 答题照常（最长优先），
    /// 但把落选那个挂成可点 chip，而不是静默替用户挑完不说。
    pub hit_word: String,
}

/// 命中的先后＝**最具体的匹配在前**（命中词字数降序，同长按名字升序保证确定）。
///
/// 🔴 为什么顺序是行为而不是审美：`corrector::correct_caliber` 按这个顺序逐个补口径，
/// 而 `add_scope_filter` 对**已被约束的列不再补** —— 于是同一张表上两个指标口径不同时
/// （最典型：金额侧 `item_type='3'` vs 数量侧 `item_type='1'`，见 二·J′），
/// **先到者赢**。此前这个「先」是 PG 的物理行序：没有 `ORDER BY`，
/// 而种子每次启动都 UPDATE 一遍 `meta.metric` → 物理序会变 →
/// **同一份代码在不同部署上可能应用不同口径，且没有任何测试会红**。
/// 这与 `load_dimensions` 那次（缺 `ORDER BY` → `find()` 按物理行序选 → E17 靠运气过）同一类。
///
/// 排序判据选「命中词更长」而不是「名字字典序」：同一条原则已经在维度侧用过
/// （`direct::pick` 取最长命中而非行序）。问「库存金额」时 `库存金额` 比 `库存` 更该说话。
fn order_by_specificity(matched: &mut [(usize, String)], name_of: impl Fn(usize) -> String) {
    matched.sort_by(|a, b| {
        b.1.chars().count().cmp(&a.1.chars().count()).then_with(|| name_of(a.0).cmp(&name_of(b.0)))
    });
}

/// 召回命中的指标（问句含指标名或别名）
pub async fn recall_metric_hits(pg: &PgPool, cx: &RecallCtx<'_>) -> anyhow::Result<Vec<MetricHit>> {
    let ds_pred = format!(
        "{}{}",
        crate::registry::ds_pred(1),
        source_asset_live_pred_at("", 1)
    );
    let rows: Vec<(String, Vec<String>, String, String, String, String, String, String, String, String, String, Vec<String>)> = sqlx::query_as::<
        _,
        (String, Vec<String>, String, String, String, String, String, String, String, String, String, Vec<String>),
    >(&format!(
        // `ORDER BY name` 是**确定性的底座**（见 `order_by_specificity` 的红字）：
        // 少了它，`rows` 的顺序就是物理行序，而下游有多处按顺序取第一个。
        "SELECT name, aliases, source_table, agg_expr, scope_filter, time_col, dedup_keys, description, unit, time_cap, version, allowed_dimensions
         FROM meta.metric WHERE status = 'active'{ds_pred} ORDER BY name",
    ))
    .bind(cx.ds)
    .fetch_all(pg)
    .await?
    .into_iter()
    .filter(|(name, _, source, expr, scope, time_col, dedup, description, unit, time_cap, version, _)| {
        catalog_allows_metric_record(
            cx.ds, name, source, expr, scope, time_col, dedup, description, unit, time_cap,
            version,
        )
    })
    .collect();
    // 命中 + MapFilter 净化（"库存金额" 不该同时拖出 "库存量"）
    let mut matched: Vec<(usize, String)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, (name, aliases, ..))| match_word(cx.question, name, aliases).map(|w| (i, w)))
        .collect();
    order_by_specificity(&mut matched, |i| rows[i].0.clone());
    let pairs: Vec<(String, String)> =
        matched.iter().map(|(i, w)| (rows[*i].0.clone(), w.clone())).collect();
    Ok(map_filter(&pairs)
        .into_iter()
        .map(|k| {
            let (name, _a, source_table, agg_expr, scope_filter, time_col, dedup_keys, description, unit, time_cap, version, allowed_dimensions) =
                rows[matched[k].0].clone();
            let allowed_dimensions = allowed_dimensions
                .into_iter()
                .filter(|dimension| {
                    catalog_allows_metric_dimension(cx.ds, &source_table, dimension)
                })
                .collect();
            MetricHit {
                name,
                source_table,
                agg_expr,
                scope_filter,
                time_col,
                dedup_keys,
                description,
                unit,
                time_cap,
                version,
                allowed_dimensions,
                hit_word: matched[k].1.clone(),
            }
        })
        .collect())
}

/// 指标口径卡文本（注入 prompt 让 LLM 严格按口径）
pub fn metric_card(m: &MetricHit) -> String {
    metric_card_for("", m)
}

/// 数据源感知的提示卡：DMS 目录来源补全为 `库.表`，结构化口径仍保留原值。
pub fn metric_card_for(ds: &str, m: &MetricHit) -> String {
    let filter = if m.scope_filter.is_empty() { String::new() } else { format!("；口径过滤：{}", m.scope_filter) };
    // 时间列是最高频错误源（同表多个时间列语义不同，且有全 NULL 的坑列）——口径卡必须钉死
    let tcol = if m.time_col.is_empty() {
        String::new()
    } else {
        format!("；时间过滤【必须】用 {} 列", m.time_col)
    };
    let dedup = if m.dedup_keys.is_empty() {
        String::new()
    } else {
        format!("；⚠️该表含系统级重复行，聚合前【必须】先按 ({}) DISTINCT 去重再算，否则数值虚增", m.dedup_keys)
    };
    // 占比类指标漏 * 100 是「数字看着像小数、其实差 100 倍」的静默错答（评测 AS04：0.049 vs 4.9）。
    // 同一件事另有 kernel 的 RequirePercentScale 硬判据回炉；这句只是把它提前告诉 LLM，省一轮。
    let pct = if m.unit == UNIT_PERCENT {
        "；占比口径：结果为百分数，SQL 必须 * 100.0 并 ROUND(…, 2)"
    } else if m.unit == UNIT_RATIO {
        "；比率口径：结果为小数比值，严格复用声明公式，不得乘 100"
    } else {
        ""
    };
    // 指标级时间窗上限放卡片**末尾**独立成句，避免埋在说明长段中被忽略。
    let cap = match m.time_cap.as_str() {
        "yesterday" => "。⚠️时间窗【不要含今天】：该指标算到**昨天**，时间上限写 `< CURDATE()` \
                        （不是期月末日、不是今天）—— 今天的单大多还没发生这个动作",
        _ => "",
    };
    // 复合子查询口径（agg_expr 含 SELECT）：模型容易改写内部连接或过滤，因此明确要求照抄。
    let composite = if m.agg_expr.to_uppercase().contains("SELECT") {
        "。⚠️该指标是复合子查询口径：【严格照抄】上面表达式的每个子查询，\
         只在各子查询的 WHERE 末尾各加一行时间条件（别重写连接、别改过滤、别换时间列）"
    } else {
        ""
    };
    let dims = if m.allowed_dimensions.is_empty() {
        "；允许维度：无（只允许总量）".to_string()
    } else {
        format!("；允许维度：{}", m.allowed_dimensions.join("、"))
    };
    let source = warehouse_qualified_source(ds, &m.source_table);
    format!("【{}·v{}】= {}，来源表 {}{}{}{}{}。说明：{}{}{}{}", m.name, m.version, m.agg_expr, source, filter, tcol, dedup, dims, m.description, pct, cap, composite)
}

pub async fn recall_metrics(pg: &PgPool, cx: &RecallCtx<'_>) -> anyhow::Result<Vec<String>> {
    Ok(recall_metric_hits(pg, cx)
        .await?
        .iter()
        .map(|hit| metric_card_for(cx.ds, hit))
        .collect())
}

/// 【A17 ②】口径二选一 chip：命中词与第一名**一样长**的落选指标（cap 2 条）。
/// 命中词等长 = 问句没区分出是哪个（「退款金额」同时像「退款额」与「退款占比」）；
/// 答题仍按最长优先（`order_by_specificity`），但落选者挂出来让人一键改问。
/// 单命中 / 命中词明显更短的落选者（问句已经说清了）不出 chip。
pub fn alt_questions(hits: &[MetricHit]) -> Vec<String> {
    let Some(top) = hits.first() else { return vec![] };
    let top_len = top.hit_word.chars().count();
    hits[1..]
        .iter()
        .filter(|h| h.hit_word.chars().count() == top_len)
        .take(2)
        .map(|h| format!("试试：{}是多少", h.name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    /// 列注释 → 干净维度名：单一事实源在 `registry`（那边 `pub use` 的就是 kernel 那一份）
    use crate::registry::clean_dim_name;

    // ── 占比单位（评测 AS04：0.049 vs 4.9）──
    fn pct_hit(unit: &str) -> MetricHit {
        MetricHit {
            name: "确认金额占比".into(),
            source_table: "fact_confirmed / fact_total".into(),
            agg_expr: "ROUND(分子 * 100.0 / 分母, 2)".into(),
            scope_filter: String::new(),
            time_col: "confirmed_at".into(),
            dedup_keys: String::new(),
            description: "分子 confirmed_amount，分母 eligible_amount".into(),
            unit: unit.into(),
            time_cap: String::new(),
            version: "1".into(),
            allowed_dimensions: vec!["月份".into()],
            hit_word: "退款占比".into(),
        }
    }

    /// 【A17 ②】chip 只在命中词等长时出：那是「问句没分清」的唯一可靠信号。
    #[test]
    fn alt_questions_only_on_tied_hit_words() {
        let mut a = pct_hit("");
        a.name = "退款额".into();
        a.hit_word = "退款金额".into();
        let mut b = pct_hit("");
        b.name = "退款占比".into();
        b.hit_word = "退款金额".into();
        assert_eq!(alt_questions(&[a.clone(), b]), ["试试：退款占比是多少"]);
        // 命中词更短的落选者：问句已经说清了，不出
        let mut c = pct_hit("");
        c.hit_word = "退款".into();
        assert!(alt_questions(&[a.clone(), c]).is_empty());
        // 单命中：不出
        assert!(alt_questions(&[a]).is_empty());
        assert!(alt_questions(&[]).is_empty());
    }

    #[test]
    fn metric_card_flags_percent_unit() {
        const HINT: &str = "占比口径：结果为百分数，SQL 必须 * 100.0 并 ROUND(…, 2)";
        assert!(metric_card(&pct_hit(UNIT_PERCENT)).ends_with(HINT), "{}", metric_card(&pct_hit(UNIT_PERCENT)));
        // 非 percent 一律不加：金额/数量/小数比值挂上这句都会改变指标语义。
        for u in ["", "amount", "qty", UNIT_RATIO] {
            assert!(!metric_card(&pct_hit(u)).contains(HINT), "unit={u}");
        }
        let ratio_card = metric_card(&pct_hit(UNIT_RATIO));
        assert!(ratio_card.contains("比率口径：结果为小数比值"), "{ratio_card}");
        assert!(ratio_card.contains("不得乘 100"), "{ratio_card}");
    }

    /// 延迟确认指标的时间窗上限 —— 'yesterday' 必须渲出
    /// 独立的 ⚠️ 句（埋在长段中间就是它上次被无视的原因）；空值一个字都不多。
    #[test]
    fn metric_card_flags_time_cap() {
        let mut h = pct_hit("");
        assert!(!metric_card(&h).contains("不要含今天"), "空 time_cap 不该渲这句");
        h.time_cap = "yesterday".into();
        let card = metric_card(&h);
        assert!(card.contains("⚠️时间窗【不要含今天】") && card.contains("< CURDATE()"), "{card}");
    }

    /// 复合子查询口径必须带「严格照抄」指令，避免模型改写时丢掉分支过滤；
    /// 普通表达式（SUM/CASE）不带 —— 那句对它们没有含义还稀释注意力。
    #[test]
    fn metric_card_flags_composite_agg_expr() {
        let mut h = pct_hit("");
        h.agg_expr = "(SELECT SUM(x) FROM a) + (SELECT SUM(y) FROM b)".into();
        assert!(metric_card(&h).contains("【严格照抄】"), "{}", metric_card(&h));
        h.agg_expr = "SUM(order_total)".into();
        assert!(!metric_card(&h).contains("【严格照抄】"));
    }

    /// 名+别名 → 命中并净化后保留的指标名（与 `recall_metric_hits` 同一套判据，只是省掉 DB）
    fn recalled(question: &str, defs: &[(&str, &[&str])]) -> Vec<String> {
        let hits: Vec<(String, String)> = defs
            .iter()
            .filter_map(|(n, al)| {
                let al: Vec<String> = al.iter().map(|s| s.to_string()).collect();
                match_word(question, n, &al).map(|w| (n.to_string(), w))
            })
            .collect();
        map_filter(&hits).into_iter().map(|i| hits[i].0.clone()).collect()
    }

    /// 🔴 派生指标「退款占比」的别名**不许遮蔽**既有「退款额」：
    /// 「今年退款额是多少」被答成百分数比 AS04 本身更糟（那是既有正确行为被打坏）。
    /// 三条别名清单与 `seed_defs.rs` 的 METRICS 同步 —— 改那边的别名必须改这里。
    #[test]
    fn refund_ratio_aliases_do_not_shadow_refund_amount() {
        const RATIO: (&str, &[&str]) =
            ("退款占比", &["退款率", "售后退款占比", "退款金额占比", "退款占销售额比例", "售后退款金额占销售额"]);
        const AMOUNT: (&str, &[&str]) = ("退款额", &["售后退款", "退款金额", "售后金额"]);
        const SALES: (&str, &[&str]) = ("销售额", &["销售总额", "营业额", "销售业绩", "业绩", "卖了多少",
            "线下销售额", "经营销售额", "DWS销售额"]);
        let all = [RATIO, AMOUNT, SALES];
        // 评测 AS04 原句：只留派生指标（长命中词把「售后退款」与「销售额」两条压掉，三张卡不打架）
        assert_eq!(recalled("今年售后退款金额占销售额的比例是多少？", &all), ["退款占比"]);
        // 反向两句：退款额/退款金额一如既往只命中 refund_amount
        assert_eq!(recalled("今年退款额是多少", &all), ["退款额"]);
        assert_eq!(recalled("今年退款金额是多少", &all), ["退款额"]);
        // 两条指标的名与别名交集为空（同一命中词永不同时属于两者）
        let inter: Vec<&str> =
            RATIO.1.iter().chain([&RATIO.0]).filter(|a| AMOUNT.1.contains(a) || **a == AMOUNT.0).cloned().collect();
        assert!(inter.is_empty(), "{inter:?}");
    }

    // ── MapFilter 召回净化（SuperSonic SchemaMapper 五规则中文适配）──
    fn hits(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(n, w)| (n.to_string(), w.to_string())).collect()
    }
    fn kept(v: &[(&str, &str)]) -> Vec<String> {
        let h = hits(v);
        map_filter(&h).into_iter().map(|i| h[i].0.clone()).collect()
    }

    /// 🔴 命中顺序＝行为：`correct_caliber` 按它逐个补口径，同表口径不同时**先到者赢**。
    /// 此前那个「先」是 PG 物理行序（SQL 没 `ORDER BY`），种子每次启动 UPDATE 一遍就可能变 ——
    /// 同一份代码在不同部署上应用不同口径，且没有任何测试会红。
    #[test]
    fn order_by_specificity_is_deterministic_and_longest_first() {
        let names = ["销量", "销售额", "库存", "库存金额"];
        let name_of = |i: usize| names[i].to_string();
        // 长命中词在前；输入顺序完全颠倒也得到同一个结果（确定性）
        let mut a = vec![(2usize, "库存".to_string()), (3, "库存金额".to_string())];
        let mut b = vec![(3usize, "库存金额".to_string()), (2, "库存".to_string())];
        order_by_specificity(&mut a, name_of);
        order_by_specificity(&mut b, name_of);
        assert_eq!(a, b);
        assert_eq!(a[0].1, "库存金额", "最具体的匹配必须在前");
        // 同长度 → 按名字升序。`销售额` 与 `销量` 首字同为 `销`，第二字 `售`(U+552E) < `量`(U+91CF)，
        // 故 `销售额` 在前 —— 排序键是 Unicode 码点序，不是拼音序，别按拼音直觉写预期。
        let mut c = vec![(0usize, "aa".to_string()), (1, "bb".to_string())];
        let mut d = vec![(1usize, "bb".to_string()), (0, "aa".to_string())];
        order_by_specificity(&mut c, name_of);
        order_by_specificity(&mut d, name_of);
        assert_eq!(c, d, "同长度也必须确定");
        assert_eq!(
            c.iter().map(|(i, _)| name_of(*i)).collect::<Vec<_>>(),
            ["销售额", "销量"],
            "同长按名字码点序"
        );
    }

    #[test]
    fn map_filter_longest_wins() {
        // 问「库存金额」不该同时拖出「库存量」(别名"库存")——两张口径卡打架
        assert_eq!(kept(&[("库存量", "库存"), ("库存金额", "库存金额")]), vec!["库存金额"]);
        assert_eq!(kept(&[("客户", "客户"), ("客户分类", "客户分类")]), vec!["客户分类"]);
    }

    #[test]
    fn map_filter_dedups_same_name() {
        // autodiscover 把同名列注册成多条维度 → 只留一条
        assert_eq!(kept(&[("所属公司编码", "公司编码"), ("所属公司编码", "公司编码")]), vec!["所属公司编码"]);
    }

    #[test]
    fn map_filter_drops_single_char() {
        assert!(kept(&[("费用", "费")]).is_empty());
    }

    #[test]
    fn map_filter_exact_beats_partial() {
        // 同一命中词下，名字与命中词完全相等的（满分）胜出
        assert_eq!(
            kept(&[("订单状态(0:暂存 108:无效)", "订单状态"), ("订单状态", "订单状态")]),
            vec!["订单状态"]
        );
    }

    #[test]
    fn map_filter_keeps_unrelated() {
        // 不同概念互不影响
        assert_eq!(kept(&[("销售额", "销售额"), ("省份", "各省")]), vec!["销售额", "省份"]);
    }

    #[test]
    fn match_word_takes_longest_alias() {
        let al: Vec<String> = ["多少单", "多少个订单"].iter().map(|s| s.to_string()).collect();
        assert_eq!(match_word("本月有多少个订单", "订单数", &al).as_deref(), Some("多少个订单"));
        assert_eq!(match_word("本月销售额", "订单数", &al), None);
    }

    /// 与 kernel 里同判据的算法测试互补：这里喂的是**生产库真实的列注释**。
    /// （名字不叫 `clean_dim_name_cuts_at_separator` —— 那个名字 kernel 已经占了，
    /// 两个 crate 同名测试会让「按名字对账断言有没有丢」这件事失真。）
    #[test]
    fn clean_dim_name_on_real_dms_comments() {
        assert_eq!(clean_dim_name("配送状态：100:待配送, 200:配送中").as_deref(), Some("配送状态"));
        assert_eq!(clean_dim_name("行类型（赠品，正品，结算）").as_deref(), Some("行类型"));
        assert_eq!(clean_dim_name("所属公司编码").as_deref(), Some("所属公司编码"));
        // 非中文/超长/过短 → None（退回字典名）
        assert_eq!(clean_dim_name("status"), None);
        assert_eq!(clean_dim_name("云之家附件上传状态说明补充文字"), None);
        assert_eq!(clean_dim_name("是"), None);
    }
}
