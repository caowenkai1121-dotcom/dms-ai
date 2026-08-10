//! A1 第二段：**三闸防误配**（纯算法，无库无网可单测）。变更原因＝对码判据。
//!
//! 🔴 **一个判据都不许松**：这是两轮实跑血泪的落点。`menu_type` 撞对账单状态、
//! `data_scope_type={1,2}` 撞联系人类型都靠它挡；下面三个断言就是那两次误配的复现。
//!
//! 搬运源 `server/src/meta.rs:1533-1600`（`best_dict_match` / `name_aligns` 一字未改）。

/// 值集对码：找覆盖率最高的 dict key。防误配硬闸（两轮实跑教训）：
///   教训① 数值小码集互相撞车（menu_type 撞对账单状态、wms_type 撞 28 项发票类型）；
///   教训② 含字母码的字典一样是撞车磁铁（data_scope_type={1,2} 撞联系人类型、审批状态撞设备处置状态）——
///          小值集证据本质不足，除名称对齐外无捷径。
/// 规则：A. 注释点名优先：列注释里出现某 dict 的 key_code/key_name（如「数据字典 MARKETING_GOODS_CATEGORY」）→ 只评该字典；
///        B. 直通：覆盖率 100% 且 ≥8 个不同值；
///        C. 名称对齐：列注释与字典名有 ≥3 字连续公共子串。
/// 值集需 2~60 个不同值，覆盖 ≥80%。纯函数可单测。
pub fn best_dict_match(
    values: &[String],
    dicts: &std::collections::HashMap<String, (String, Vec<(String, String)>)>,
    col_comment: &str,
) -> Option<(String, String, Vec<(String, String)>, f64)> {
    use std::collections::HashSet;
    let uniq: HashSet<&String> = values.iter().collect();
    if uniq.len() < 2 || uniq.len() > 60 {
        return None;
    }
    // A. 注释点名的字典优先（只评点名的；点名了但不匹配也宁缺毋滥）
    let comment_low = col_comment.to_lowercase();
    let named: Vec<&String> = dicts
        .keys()
        .filter(|kc| {
            (!kc.is_empty() && kc.len() >= 4 && comment_low.contains(&kc.to_lowercase()))
                || dicts
                    .get(*kc)
                    .map(|(kn, _)| !kn.is_empty() && kn.len() >= 3 && col_comment.contains(kn.as_str()))
                    .unwrap_or(false)
        })
        .collect();
    let candidates: Vec<&String> = if !named.is_empty() {
        named
    } else {
        dicts.keys().collect()
    };
    let mut best: Option<(String, String, Vec<(String, String)>, f64, usize)> = None;
    for kc in candidates {
        let (kn, pairs) = &dicts[kc];
        let codes: HashSet<&String> = pairs.iter().map(|(c, _)| c).collect();
        let hit = uniq.iter().filter(|v| codes.contains(**v)).count();
        let cov = hit as f64 / uniq.len() as f64;
        if hit < 2 || cov < 0.8 {
            continue;
        }
        let pass = (cov >= 1.0 && uniq.len() >= 8) || name_aligns(col_comment, kn);
        if !pass {
            continue;
        }
        let better = match &best {
            Some((_, _, _, bcov, bhit)) => (cov, hit) > (*bcov, *bhit),
            None => true,
        };
        if better {
            best = Some((kc.clone(), kn.clone(), pairs.clone(), cov, hit));
        }
    }
    best.map(|(kc, kn, pairs, cov, _)| (kc, kn, pairs, cov))
}

/// 名称对齐：列注释与字典名存在 ≥3 字连续公共子串（CJK 3-gram 双向包含判定）
pub fn name_aligns(comment: &str, dict_name: &str) -> bool {
    let c: Vec<char> = comment.chars().collect();
    let d: Vec<char> = dict_name.chars().collect();
    let has_common_3gram = |a: &[char], b: &[char]| {
        b.windows(3).any(|w| a.windows(3).any(|x| x == w))
    };
    c.len() >= 3 && d.len() >= 3 && (has_common_3gram(&c, &d) || has_common_3gram(&d, &c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dict_match_basic() {
        let mut dicts = std::collections::HashMap::new();
        dicts.insert(
            "CustClassif".to_string(),
            (
                "客户分类".to_string(),
                vec![
                    ("01".into(), "货架店铺".into()),
                    ("04".into(), "线下客户".into()),
                    ("06".into(), "其他财务专用".into()),
                ],
            ),
        );
        let vals = vec!["04".to_string(), "06".to_string(), "01".to_string()];
        // 小集合（3 值）须名称对齐：注释「客户分类」与字典名「客户分类」对齐 → 过
        let (kc, kn, _, cov) = best_dict_match(&vals, &dicts, "客户分类").unwrap();
        assert_eq!(kc, "CustClassif");
        assert_eq!(kn, "客户分类");
        assert!((cov - 1.0).abs() < 1e-9);
    }

    #[test]
    fn dict_match_rejects() {
        let mut dicts = std::collections::HashMap::new();
        dicts.insert(
            "K".to_string(),
            ("k".to_string(), vec![("01".into(), "a".into()), ("02".into(), "b".into())]),
        );
        // 单值不匹配（<2 个不同值）
        assert!(best_dict_match(&["01".to_string()], &dicts, "任意注释").is_none());
        // 覆盖率不足（2/4=50% < 80%）
        let mixed = vec!["01".to_string(), "02".to_string(), "xx".to_string(), "yy".to_string()];
        assert!(best_dict_match(&mixed, &dicts, "任意注释").is_none());
        // 值过多（非码列）
        let many: Vec<String> = (0..80).map(|i| i.to_string()).collect();
        assert!(best_dict_match(&many, &dicts, "任意注释").is_none());
    }

    #[test]
    fn dict_match_collision_guard() {
        // 实跑误配复现：menu_type 值{0,1,2} ⊆ 对账单状态码 —— 小集合+名称不对齐 → 拒
        let mut dicts = std::collections::HashMap::new();
        dicts.insert(
            "BillStatus".to_string(),
            (
                "对账单状态".to_string(),
                vec![
                    ("0".into(), "待确认".into()),
                    ("1".into(), "已确认".into()),
                    ("2".into(), "部分开票".into()),
                    ("3".into(), "已开票".into()),
                    ("4".into(), "拒绝".into()),
                ],
            ),
        );
        let vals = vec!["0".to_string(), "1".to_string(), "2".to_string()];
        assert!(best_dict_match(&vals, &dicts, "菜单类型").is_none());
        // 含字母码的字典一样是撞车磁铁（data_scope_type={1,2} 撞联系人类型的教训）→ 拒
        let mut dicts2 = std::collections::HashMap::new();
        dicts2.insert(
            "ContactType".to_string(),
            (
                "联系人类型".to_string(),
                vec![("1".into(), "业务".into()), ("2".into(), "财务".into()), ("Y1".into(), "主联系人".into())],
            ),
        );
        assert!(best_dict_match(&vals, &dicts2, "数据范围id").is_none());
        // ≥8 个不同值 cov=1.0 → 大集合直通
        let nine: Vec<String> = (0..9).map(|i| i.to_string()).collect();
        dicts.get_mut("BillStatus").unwrap().1.extend([
            ("5".into(), "x5".into()),
            ("6".into(), "x6".into()),
            ("7".into(), "x7".into()),
            ("8".into(), "x8".into()),
        ]);
        assert!(best_dict_match(&nine, &dicts, "任意列注释").is_some());
        // 注释点名优先：注释写了「数据字典 K」→ 只评 K（值 ⊆ K 即中，不被其他字典抢）
        let mut dicts3 = std::collections::HashMap::new();
        dicts3.insert(
            "GOODS_CAT".to_string(),
            ("商品分类字典".to_string(), vec![("A".into(), "肠类".into()), ("B".into(), "挞类".into())]),
        );
        dicts3.insert(
            "CustClassif".to_string(),
            ("客户分类".to_string(), vec![("A".into(), "货架".into()), ("B".into(), "线下".into())]),
        );
        let ab = vec!["A".to_string(), "B".to_string()];
        let (kc, ..) = best_dict_match(&ab, &dicts3, "商品分类（数据字典 GOODS_CAT）").unwrap();
        assert_eq!(kc, "GOODS_CAT");
        // 名称对齐判据
        assert!(name_aligns("订单状态", "销售订单状态"));
        assert!(name_aligns("所属公司", "所属公司"));
        assert!(!name_aligns("数据范围类型", "合同类型"));
        assert!(!name_aligns("菜单类型", "对账单状态"));
    }
}
