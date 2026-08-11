//! A1 第二段：**三闸防误配**（纯算法，无库无网可单测）。变更原因＝对码判据。
//!
//! 🔴 **一个判据都不许松**：这是两轮实跑血泪的落点。`menu_type` 撞对账单状态、
//! `data_scope_type={1,2}` 撞联系人类型都靠它挡；下面三个断言就是那两次误配的复现。
//!
//! 搬运源 `server/src/meta.rs:1533-1600`（`best_dict_match` / `name_aligns` 的判据与阈值逐字保留；
//! 本轮加了 `DictIndex` 预建视图与同分确定性排序，判据本身没动）。

/// 对码判据阈值（每个都有测试钉点，改动即评审；doc 注释引常量不抄数字）。
/// 值集不同值数下限（单值列证据近零）。
const MIN_DISTINCT: usize = 2;
/// 值集不同值数上限（超过即非码列）。
const MAX_DISTINCT: usize = 60;
/// 覆盖率下限：`列实际取值 ∩ 字典码 / 列实际取值`。
const MIN_COVERAGE: f64 = 0.8;
/// 直通档：覆盖率 100% 且不同值 ≥ 此数（大集合免名称对齐）。
const DIRECT_MIN_DISTINCT: usize = 8;
/// 名称对齐的连续公共子串字数（CJK n-gram 的 n）。
const NAME_GRAM: usize = 3;

/// `best_dict_match` 的命中（dict_key, dict_name, 码名对, 覆盖率）。
pub type DictMatch = (String, String, Vec<(String, String)>, f64);

/// 一轮对码的预建视图（小写键/名 + 各字典码集 + 码集引用）：原来每个候选列都重建一遍，
/// 且 HashMap 迭代序随机 → 同分谁先看谁随机。构建时按 key 排序 = 跨轮可复现。
pub struct DictIndex<'a> {
    entries: Vec<DictEntry<'a>>,
}

struct DictEntry<'a> {
    key: &'a str,
    name: &'a str,
    pairs: &'a [(String, String)],
    key_low: String,
    name_low: String,
    codes: std::collections::HashSet<&'a str>,
}

impl<'a> DictIndex<'a> {
    pub fn build(dicts: &'a std::collections::HashMap<String, (String, Vec<(String, String)>)>) -> Self {
        let mut entries: Vec<DictEntry<'a>> = dicts
            .iter()
            .map(|(kc, (kn, pairs))| DictEntry {
                key: kc,
                name: kn,
                pairs,
                key_low: kc.to_lowercase(),
                name_low: kn.to_lowercase(),
                codes: pairs.iter().map(|(c, _)| c.as_str()).collect(),
            })
            .collect();
        // 按 key 排序：同 cov 同 hit 的两个字典谁中与迭代序无关（跨轮可复现）
        entries.sort_by(|a, b| a.key.cmp(b.key));
        Self { entries }
    }
}

/// 值集对码：找覆盖率最高的 dict key。防误配硬闸（两轮实跑教训）：
///   教训① 数值小码集互相撞车（menu_type 撞对账单状态、wms_type 撞 28 项发票类型）；
///   教训② 含字母码的字典一样是撞车磁铁（data_scope_type={1,2} 撞联系人类型、审批状态撞设备处置状态）——
///          小值集证据本质不足，除名称对齐外无捷径。
/// 规则：A. 注释点名优先：列注释里出现某 dict 的 key_code/key_name（如「数据字典 MARKETING_GOODS_CATEGORY」）→ 只评该字典；
///        B. 直通：覆盖率 100% 且 ≥ `DIRECT_MIN_DISTINCT` 个不同值；
///        C. 名称对齐：列注释与字典名有 ≥`NAME_GRAM` 字连续公共子串。
/// 值集需 `MIN_DISTINCT`~`MAX_DISTINCT` 个不同值，覆盖 ≥ `MIN_COVERAGE`。纯函数可单测。
pub fn best_dict_match(
    values: &[String],
    dicts: &std::collections::HashMap<String, (String, Vec<(String, String)>)>,
    col_comment: &str,
) -> Option<DictMatch> {
    best_dict_match_ix(values, &DictIndex::build(dicts), col_comment)
}

/// `best_dict_match` 的索引版：一轮多候选列共用一份 `DictIndex`。
pub fn best_dict_match_ix(
    values: &[String],
    index: &DictIndex<'_>,
    col_comment: &str,
) -> Option<DictMatch> {
    use std::collections::HashSet;
    let uniq: HashSet<&String> = values.iter().collect();
    if uniq.len() < MIN_DISTINCT || uniq.len() > MAX_DISTINCT {
        return None;
    }
    // A. 注释点名的字典优先（只评点名的；点名了但不匹配也宁缺毋滥）。
    //    key 与 name 统一小写比对（字典名含 ASCII 时大小写不一致不再漏点名）。
    let comment_low = col_comment.to_lowercase();
    let named: Vec<&DictEntry<'_>> = index
        .entries
        .iter()
        .filter(|e| {
            (!e.key.is_empty() && e.key.chars().count() >= 4 && comment_low.contains(&e.key_low))
                || (!e.name.is_empty()
                    && e.name.chars().count() >= 3
                    && comment_low.contains(&e.name_low))
        })
        .collect();
    let candidates: Vec<&DictEntry<'_>> = if !named.is_empty() {
        named
    } else {
        index.entries.iter().collect()
    };
    let mut best: Option<(&DictEntry<'_>, f64, usize)> = None;
    for e in candidates {
        let hit = uniq.iter().filter(|v| e.codes.contains(v.as_str())).count();
        let cov = hit as f64 / uniq.len() as f64;
        if hit < MIN_DISTINCT || cov < MIN_COVERAGE {
            continue;
        }
        let pass = (cov >= 1.0 && uniq.len() >= DIRECT_MIN_DISTINCT) || name_aligns(col_comment, e.name);
        if !pass {
            continue;
        }
        // 严格大于才换：同 cov 同 hit 保持 key 序在前者（确定性）
        let better = match &best {
            Some((_, bcov, bhit)) => (cov, hit) > (*bcov, *bhit),
            None => true,
        };
        if better {
            best = Some((e, cov, hit));
        }
    }
    // 出循环后克隆一次（原来每次刷新 best 都 pairs.clone() 整份码表）
    best.map(|(e, cov, _)| (e.key.to_string(), e.name.to_string(), e.pairs.to_vec(), cov))
}

/// 名称对齐：列注释与字典名存在 ≥`NAME_GRAM` 字连续公共子串（CJK n-gram 包含判定；
/// 「存在公共 n-gram」数学上对称，单向判一次即可 —— 原双向各判一次是冗余）。
/// 统一小写比对（CJK 无大小写；含 ASCII 的字典名与注释大小写不同也照中 —— 与点名闸同口径）。
pub fn name_aligns(comment: &str, dict_name: &str) -> bool {
    let c: Vec<char> = comment.to_lowercase().chars().collect();
    let d: Vec<char> = dict_name.to_lowercase().chars().collect();
    if c.len() < NAME_GRAM || d.len() < NAME_GRAM {
        return false;
    }
    // HashSet 包含判定（原 O(|a|×|b|) 双层 windows 比较）
    let grams: std::collections::HashSet<&[char]> = c.windows(NAME_GRAM).collect();
    d.windows(NAME_GRAM).any(|w| grams.contains(w))
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

    /// 同 cov 同 hit 的两个字典：结果跨轮可复现（按 key 序取前者，不看 HashMap 迭代序）。
    #[test]
    fn tied_dicts_resolve_deterministically() {
        let mut dicts = std::collections::HashMap::new();
        // 两本字典码集相同、名称都与注释对齐 → cov/hit 全平
        for key in ["Zb_dict", "Aa_dict"] {
            dicts.insert(
                key.to_string(),
                ("订单状态".to_string(), vec![("01".into(), "暂存".into()), ("02".into(), "生效".into())]),
            );
        }
        let vals = vec!["01".to_string(), "02".to_string()];
        let first = best_dict_match(&vals, &dicts, "订单状态").unwrap();
        for _ in 0..20 {
            let again = best_dict_match(&vals, &dicts, "订单状态").unwrap();
            assert_eq!(again.0, first.0, "同分结果跨轮必须可复现");
        }
        assert_eq!(first.0, "Aa_dict", "同分按 key 序取前者");
    }

    /// 点名的统一小写口径：字典名含 ASCII 时，注释与名大小写不同也照中。
    #[test]
    fn named_dict_match_is_case_insensitive_on_ascii_names() {
        let mut dicts = std::collections::HashMap::new();
        dicts.insert(
            "WMS_STATUS".to_string(),
            ("WMS状态".to_string(), vec![("01".into(), "启用".into()), ("02".into(), "停用".into())]),
        );
        let vals = vec!["01".to_string(), "02".to_string()];
        let (kc, ..) = best_dict_match(&vals, &dicts, "wms状态说明").unwrap();
        assert_eq!(kc, "WMS_STATUS", "小写注释点大写名字典必须中");
    }
}
