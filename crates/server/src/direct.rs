//! 确定性快路径（0-LLM）：单号直查 + 高频销售聚合模板。
//! 命中即秒级零幻觉出结果，跳过 LLM；未命中回落 pipeline 的 LLM 路径。
//! 生成的 SQL 仍过 is_safe_select + 权限注入 + 只读执行（复用 pipeline），权限不旁路。

/// 确定性命中：SQL（未注入）+ 路由标签 + 可选上期查询（KPI 环比）
pub struct DirectHit {
    pub sql: String,
    pub route: String,
    /// (上期 SQL, 环比标签如"较上月")——仅高频聚合单指标时有
    pub prev: Option<(String, String)>,
}

pub fn try_direct(question: &str) -> Option<DirectHit> {
    sniff_doc_code(question).or_else(|| agg_template(question))
}

/// 单据前缀 → (表, 主号列)。后缀字母区分单据类型，区分度足够（免 UNION 探测开销）。
fn doc_binding(code: &str) -> Option<(&'static str, &'static str)> {
    let up = code.to_uppercase();
    if up.starts_with("SPC-") {
        return Some(("t_winc_purchase_transfer", "bill_code"));
    }
    // HJXH-D**xxxx：按第 6-8 位单据类型字母段
    if let Some(rest) = up.strip_prefix("HJXH-") {
        let tag: String = rest.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        return match tag.as_str() {
            "DXO" | "DSO" | "XO" | "SO" => Some(("t_sales_order", "sales_order_code")),
            "DRO" | "RO" => Some(("t_after_sales_order_header", "after_sales_code")),
            "DZD" | "ZD" => Some(("t_account_bill_header", "bill_code")),
            _ => None,
        };
    }
    None
}

/// 从问句抽单号（HJXH-字母+数字 / SPC-日期-序号），命中即出单据卡（SELECT * 单行）。
fn sniff_doc_code(question: &str) -> Option<DirectHit> {
    for token in question.split(|c: char| c.is_whitespace() || matches!(c, '，' | ',' | '。' | '的' | '是')) {
        let t = token.trim();
        if t.len() < 6 {
            continue;
        }
        // 单号字符集：字母数字与连字符
        if !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            continue;
        }
        if let Some((table, col)) = doc_binding(t) {
            // 单号里不含引号，无注入风险；仍转义防御
            let safe = t.replace('\'', "''");
            return Some(DirectHit {
                sql: format!("SELECT * FROM {table} WHERE {col} = '{safe}' LIMIT 1"),
                route: "direct-doc".into(),
                prev: None,
            });
        }
    }
    None
}

/// 高频销售聚合模板：时间窗 + 单指标，无维度、无实体（含则回落 LLM 做 GROUP BY/实体锚定）。
fn agg_template(question: &str) -> Option<DirectHit> {
    const DIM_WORDS: &[&str] = &["排行", "排名", "前", "各", "按", "分类", "省", "市", "区域", "门店", "客户", "商品", "占比", "对比", "趋势", "明细"];
    if DIM_WORDS.iter().any(|w| question.contains(w)) {
        return None;
    }
    // 剥词守卫（旧项目实证）：去掉时间/指标/语气/连接词后仍有残留=实体问句，回落 LLM。
    // 例：「恒众餐饮本月销售额」剥后剩「恒众餐饮」→ 不命中；「本月销售额是多少」剥后空→命中。
    let mut stripped = question.to_string();
    for w in [
        "今天", "今日", "昨天", "昨日", "本月", "这个月", "上月", "上个月", "本周", "这周", "今年",
        "销售额", "销售总额", "营业额", "订单数", "多少单", "几单", "客单价", "卖了多少",
        "是多少", "多少", "有", "的", "呢", "吗", "总共", "一共", "了", "查", "查询", "看看", "帮我",
    ] {
        stripped = stripped.replace(w, "");
    }
    if stripped.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }
    let time_pred = time_window(question)?;
    let metric = if question.contains("订单数") || question.contains("多少单") || question.contains("几单") {
        "COUNT(DISTINCT sales_order_code) AS `订单数`"
    } else if question.contains("客单价") {
        "ROUND(SUM(total_amount)/NULLIF(COUNT(DISTINCT sales_order_code),0), 2) AS `客单价`"
    } else if question.contains("销售额") || question.contains("销售总额") || question.contains("营业额") || question.contains("卖了多少") {
        "SUM(total_amount) AS `销售额`"
    } else {
        return None;
    };
    let base = |pred: &str| {
        format!(
            "SELECT {metric} FROM t_sales_order \
             WHERE deleted_flag = 0 AND order_status NOT IN ('0','108','199') AND {pred}"
        )
    };
    // 上期查询（环比）：平移时间窗
    let prev = prev_window(question).map(|(pred, label)| (base(pred), label.to_string()));
    Some(DirectHit {
        sql: base(time_pred),
        route: "direct-agg".into(),
        prev,
    })
}

/// 时间窗 → 上一期谓词 + 环比标签
fn prev_window(q: &str) -> Option<(&'static str, &'static str)> {
    if q.contains("今天") || q.contains("今日") {
        Some(("DATE(order_time) = CURDATE() - INTERVAL 1 DAY", "较昨天"))
    } else if q.contains("昨天") || q.contains("昨日") {
        Some(("DATE(order_time) = CURDATE() - INTERVAL 2 DAY", "较前天"))
    } else if q.contains("本月") || q.contains("这个月") {
        Some(("order_time >= DATE_FORMAT(CURDATE() - INTERVAL 1 MONTH,'%Y-%m-01') AND order_time < DATE_FORMAT(CURDATE(),'%Y-%m-01')", "较上月"))
    } else if q.contains("上月") || q.contains("上个月") {
        Some(("order_time >= DATE_FORMAT(CURDATE() - INTERVAL 2 MONTH,'%Y-%m-01') AND order_time < DATE_FORMAT(CURDATE() - INTERVAL 1 MONTH,'%Y-%m-01')", "较上上月"))
    } else if q.contains("本周") || q.contains("这周") {
        Some(("YEARWEEK(order_time, 1) = YEARWEEK(CURDATE() - INTERVAL 7 DAY, 1)", "较上周"))
    } else if q.contains("今年") {
        Some(("YEAR(order_time) = YEAR(CURDATE()) - 1", "较去年"))
    } else {
        None
    }
}

/// 相对时间词 → MySQL 谓词（基于 CURDATE()，零硬编码年份）
fn time_window(q: &str) -> Option<&'static str> {
    if q.contains("今天") || q.contains("今日") {
        Some("DATE(order_time) = CURDATE()")
    } else if q.contains("昨天") || q.contains("昨日") {
        Some("DATE(order_time) = CURDATE() - INTERVAL 1 DAY")
    } else if q.contains("本月") || q.contains("这个月") {
        Some("order_time >= DATE_FORMAT(CURDATE(),'%Y-%m-01') AND order_time < DATE_ADD(DATE_FORMAT(CURDATE(),'%Y-%m-01'), INTERVAL 1 MONTH)")
    } else if q.contains("上月") || q.contains("上个月") {
        Some("order_time >= DATE_FORMAT(CURDATE() - INTERVAL 1 MONTH,'%Y-%m-01') AND order_time < DATE_FORMAT(CURDATE(),'%Y-%m-01')")
    } else if q.contains("本周") || q.contains("这周") {
        Some("YEARWEEK(order_time, 1) = YEARWEEK(CURDATE(), 1)")
    } else if q.contains("今年") {
        Some("YEAR(order_time) = YEAR(CURDATE())")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_prefixes() {
        assert_eq!(doc_binding("HJXH-DXO2026072300384").unwrap().0, "t_sales_order");
        assert_eq!(doc_binding("HJXH-DRO2026072300047").unwrap().0, "t_after_sales_order_header");
        assert_eq!(doc_binding("HJXH-DZD20261230000261").unwrap().0, "t_account_bill_header");
        assert_eq!(doc_binding("SPC-20260718-8").unwrap().0, "t_winc_purchase_transfer");
        assert!(doc_binding("HJXH-XXX123").is_none());
    }

    #[test]
    fn sniff_in_sentence() {
        let h = sniff_doc_code("帮我查下 HJXH-DXO2026072300384 这张单").unwrap();
        assert!(h.sql.contains("t_sales_order"));
        assert!(h.sql.contains("HJXH-DXO2026072300384"));
        assert_eq!(h.route, "direct-doc");
    }

    #[test]
    fn agg_hits_month_sales() {
        let h = agg_template("本月销售额是多少").unwrap();
        assert!(h.sql.contains("SUM(total_amount)"));
        assert!(h.sql.contains("NOT IN ('0','108','199')"));
        assert!(h.sql.contains("DATE_FORMAT"));
        assert_eq!(h.route, "direct-agg");
    }

    #[test]
    fn agg_order_count() {
        let h = agg_template("今天有多少订单数").unwrap();
        assert!(h.sql.contains("COUNT(DISTINCT sales_order_code)"));
        assert!(h.sql.contains("DATE(order_time) = CURDATE()"));
    }

    #[test]
    fn agg_skips_dimension() {
        // 带维度词 → 回落 LLM
        assert!(agg_template("本月销售额前五的省份").is_none());
        assert!(agg_template("各商品分类的销量").is_none());
        assert!(agg_template("恒众餐饮本月销售额").is_none()); // 含"客户"实体? 不，含"恒众"但无维度词——靠"客户"词挡不住
    }

    #[test]
    fn agg_needs_time_and_metric() {
        assert!(agg_template("销售额").is_none()); // 无时间窗
        assert!(agg_template("本月天气").is_none()); // 无指标
    }
}
