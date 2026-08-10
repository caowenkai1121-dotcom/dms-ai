//! 问句与注册表文本的匹配基元：最长别名命中、MapFilter 命中净化、列注释洗成维度名、
//! 全角括注剥离、剥词残留守卫。全部纯函数，词表一律参数化（业务词不进 kernel）。
//!
//! 搬运源 `server/src/meta.rs:864-918/1075-1087`、`server/src/direct.rs:110-141/432-455`。

/// 问句对某元素的命中：返回命中词（名或最长命中别名），未命中返回 None。
/// 取最长——同一元素多个别名同时命中时，长词更具体（"多少个订单" 优于 "多少单"）。
pub fn match_word(question: &str, name: &str, aliases: &[String]) -> Option<String> {
    let mut best: Option<String> = None;
    let mut consider = |w: &str| {
        if !w.is_empty() && question.contains(w) {
            let better = best.as_ref().map(|b| w.chars().count() > b.chars().count()).unwrap_or(true);
            if better {
                best = Some(w.to_string());
            }
        }
    };
    consider(name);
    for a in aliases {
        consider(a);
    }
    best
}

/// MapFilter（移植 SuperSonic SchemaMapper 命中净化五规则的中文适配版）：
/// 召回命中往往互相干扰——问「库存金额」会同时命中指标「库存量」(别名"库存")；
/// autodiscover 把列注释当维度名导致同名重复 10 条。不净化则口径卡互相打架且 prompt 膨胀。
///
/// 输入 (元素名, 命中词)，输出保留下标（保持原序）：
/// - R1 命中词 <2 字 剔除（中文单字无区分度）
/// - R2 同名去重（保留首个）
/// - R3 命中词被另一命中词真包含 → 剔除较短者（"库存" vs "库存金额" 取后者）
/// - R4 同一命中词多元素命中时，元素名==命中词（满分）优先，其余剔除
pub fn map_filter(hits: &[(String, String)]) -> Vec<usize> {
    let words: Vec<&str> = hits.iter().map(|(_, w)| w.as_str()).collect();
    // R4 预备：哪些命中词存在满分元素
    let exact_words: std::collections::HashSet<&str> = hits
        .iter()
        .filter(|(n, w)| n == w)
        .map(|(_, w)| w.as_str())
        .collect();
    let mut seen_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out = vec![];
    for (i, (name, word)) in hits.iter().enumerate() {
        if word.chars().count() < 2 {
            continue; // R1
        }
        if !seen_names.insert(name.as_str()) {
            continue; // R2
        }
        // R3：存在更长且真包含本命中词的命中 → 本条让位
        if words.iter().any(|w| w.len() > word.len() && w.contains(word.as_str())) {
            continue;
        }
        // R4：同词有满分命中而本条非满分 → 让位
        if name != word && exact_words.contains(word.as_str()) {
            continue;
        }
        out.push(i);
    }
    out
}

/// 列注释 → 干净维度名：截到首个分隔符（中英文冒号/括号/逗号/斜杠/空格）之前。
/// 结果须是 2~8 字的纯中文词；否则 None（调用方退回字典名）。
pub fn clean_dim_name(comment: &str) -> Option<String> {
    let head: String = comment
        .trim()
        .chars()
        .take_while(|c| !":：(（)）,，、/ \t".contains(*c))
        .collect();
    let n = head.chars().count();
    if (2..=8).contains(&n) && head.chars().all(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)) {
        Some(head)
    } else {
        None
    }
}

/// 去注册表文本里的全角括注（维护给人类看的说明，不是 SQL 的一部分；半角括号是 SQL 语法不动）
pub fn strip_annotations(s: &str) -> String {
    // 全角（）恒为注记；ASCII () 仅当组内含中文才是注记（否则是真 SQL 如 SUM(col)/IN('0','1')）
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '（' || c == '(' {
            let (open, close) = if c == '（' { ('（', '）') } else { ('(', ')') };
            let mut depth = 1;
            let mut j = i + 1;
            while j < chars.len() && depth > 0 {
                if chars[j] == open {
                    depth += 1;
                } else if chars[j] == close {
                    depth -= 1;
                }
                j += 1;
            }
            let group: String = chars[i..j].iter().collect();
            let has_cjk = group.chars().any(|ch| ('\u{4E00}'..='\u{9FFF}').contains(&ch));
            if open == '(' && !has_cjk {
                out.push_str(&group);
            }
            i = j;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out.trim().to_string()
}

/// 残留守卫（纯函数）：把问句里被模板/组合器「消化掉」的词、以及 `strip_words` 里的通用虚词
/// 剥光后，若还剩实义字（CJK/字母数字）→ 说明问句含模板表达不了的限定（实体名、值过滤、
/// 未支持的维度），必须回落 LLM，绝不能装配一条**丢掉限定**的 SQL 静默答错。
///
/// `strip_words` 由调用方给（通用词表见 `nl::lexicon::STRIP_WORDS`），
/// 业务同义词由 `consumed` 传入——kernel 不持有任何业务名词。
pub fn has_residue_with(question: &str, consumed: &[String], strip_words: &[&str]) -> bool {
    let mut s = question.to_string();
    // 先剥业务词（长词优先，防"客户分类"被"客户"拆散后留下"分类"）
    let mut words: Vec<&String> = consumed.iter().collect();
    words.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    for w in words {
        s = s.replace(w.as_str(), "");
    }
    // 再剥通用虚词/时间词/排序词
    for w in strip_words {
        s = s.replace(w, "");
    }
    let s: String = s
        .chars()
        .filter(|c| !c.is_ascii_digit() && !c.is_whitespace() && !"，。？?、,.~～!！:：".contains(*c))
        .collect();
    s.chars().any(|c| c.is_alphanumeric() || (c as u32) > 0x2E7F)
}

/// 候选子串窗口，**长词优先**（8 字 → 2 字），同长度内按位置从左到右。纯函数。
///
/// 两个消费者：图路径实体抽取（`connector::graph::resolve_entities`，命中即占位所以
/// 顺序承重 —— 「肉制品」必须在「肉制」之前被试，否则分类名被自己的前缀抢走）；
/// SQL 路径的切片向量召回（A8：整句向量被长问句稀释，切片后逐片召回再合并）。
/// 上限 8 字：图里最长的省份名/分类名都在 8 字内，再长的窗口只是白查库/白 embed。
pub fn candidate_windows(text: &str) -> Vec<(usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = vec![];
    if n < 2 {
        return out;
    }
    for len in (2..=8usize.min(n)).rev() {
        for start in 0..=(n - len) {
            out.push((start, chars[start..start + len].iter().collect()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(ws: &[&str]) -> Vec<String> {
        ws.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn match_word_takes_longest_hit() {
        let al = owned(&["甲乙", "甲乙丙丁"]);
        assert_eq!(match_word("这是甲乙丙丁啊", "甲", &al).as_deref(), Some("甲乙丙丁"));
        assert_eq!(match_word("这是甲乙啊", "甲", &al).as_deref(), Some("甲乙"));
        assert_eq!(match_word("无关问句", "甲", &al), None);
    }

    #[test]
    fn map_filter_four_rules() {
        let hits = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
        };
        // R1 单字命中剔除
        assert!(map_filter(&hits(&[("甲乙", "甲")])).is_empty());
        // R2 同名保留首个
        assert_eq!(map_filter(&hits(&[("甲乙", "甲乙"), ("甲乙", "甲乙")])), vec![0]);
        // R3 短命中词让位给真包含它的长命中词
        assert_eq!(map_filter(&hits(&[("甲乙", "甲乙"), ("甲乙丙", "甲乙丙")])), vec![1]);
        // R4 同词非满分让位给满分
        assert_eq!(map_filter(&hits(&[("甲乙丙", "甲乙"), ("甲乙", "甲乙")])), vec![1]);
        // 互不相干的两条都留
        assert_eq!(map_filter(&hits(&[("甲乙", "甲乙"), ("丙丁", "丙丁")])), vec![0, 1]);
    }

    #[test]
    fn clean_dim_name_cuts_at_separator() {
        assert_eq!(clean_dim_name("状态说明：100:未开始").as_deref(), Some("状态说明"));
        assert_eq!(clean_dim_name("行类型（甲，乙）").as_deref(), Some("行类型"));
        assert_eq!(clean_dim_name("status"), None, "非中文不采纳");
        assert_eq!(clean_dim_name("是"), None, "<2 字不采纳");
        assert_eq!(clean_dim_name("一二三四五六七八九"), None, ">8 字不采纳");
    }

    #[test]
    fn strip_annotations_keeps_sql_parens() {
        assert_eq!(strip_annotations("SUM(amount)"), "SUM(amount)");
        assert_eq!(strip_annotations("IN('0','1')"), "IN('0','1')");
        assert_eq!(strip_annotations("t_x（人类说明）"), "t_x");
        assert_eq!(strip_annotations("t_x(含中文的注记)"), "t_x");
        // 嵌套注记整组剥掉
        assert_eq!(strip_annotations("t_x（说明（含嵌套））y"), "t_xy");
    }

    /// 🔴 长词优先是承重的：「肉制品」必须排在「肉制」之前被试，
    /// 否则分类名会被自己的前缀抢走（消费侧命中即占位，长词就再也匹配不上）。
    #[test]
    fn windows_try_long_words_first() {
        let ws = candidate_windows("湖南省烤肠");
        let pos = |w: &str| ws.iter().position(|(_, x)| x == w).expect("缺候选 {w}");
        assert!(pos("湖南省烤肠") < pos("湖南省"), "5 字窗必须早于 3 字窗");
        assert!(pos("湖南省") < pos("湖南"), "3 字窗必须早于 2 字窗");
        assert!(pos("烤肠") > pos("湖南省"), "同理，短词最后");
        // 同长度内从左到右（否则「烤肠」可能抢在「湖南」前占位，解析结果的顺序就乱了）
        assert!(pos("湖南") < pos("南省"));
        // 边界：不足两字没有候选；窗口不超过文本长度
        assert!(candidate_windows("肠").is_empty());
        assert!(candidate_windows("").is_empty());
        assert!(ws.iter().all(|(s, w)| s + w.chars().count() <= 5));
        // 长度覆盖：2..=5 每档都有（`8usize.min(n)` 写成 `8` 会 panic，写成 `n` 会漏长窗）
        for len in 2..=5 {
            assert!(ws.iter().any(|(_, w)| w.chars().count() == len), "缺 {len} 字窗");
        }
    }

    #[test]
    fn residue_needs_word_list() {
        let consumed = owned(&["甲乙", "丙"]);
        // 未消化的实义词残留 → true
        assert!(has_residue_with("戊己甲乙丙", &consumed, &["的"]));
        // 全被消化（业务词 + 虚词）→ false
        assert!(!has_residue_with("本月甲乙的丙", &consumed, &["本月", "的"]));
        // 长词优先：不因先剥"丙"而在"丙丁"上留下"丁"
        let long = owned(&["丙", "丙丁"]);
        assert!(!has_residue_with("丙丁", &long, &[]));
        // 纯数字/标点不算实义残留
        assert!(!has_residue_with("甲乙 100，", &consumed, &[]));
    }
}
