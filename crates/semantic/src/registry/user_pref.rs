//! 用户习惯层：**同一个问句，不同的人问，默认口径可以不同**。
//!
//! 业主要的「每个用户都是个性化的」在这里落地的那一半（另一半是 `memory` 的
//! `login_name` 作用域 —— 那是「学到的经验按人隔离」，这里是「学到的习惯按人生效」）。
//!
//! ## 为什么不新建学习表
//!
//! `meta.query_log` 本身就是记录：谁、什么时候、问了什么、走了哪条路、出了多少行。
//! 从它现算的习惯**永远是新鲜的**，且不存在「学错了污染语料池」这个面 —— 没有写入，
//! 就没有回滚、没有复核、没有 TTL。这是本轮能找到的最省的一条路（prime-agent 那套
//! 两段式提案 + apply + 账本，是给「写状态」准备的；只读聚合不需要它）。
//!
//! ## 三条硬约束（缺一条这功能就该删掉）
//!
//! 1. **只在用户没明说时用**：问句里已经有时间词/分组词，一律以用户说的为准。
//!    习惯永远不覆盖显式表达 —— 那是「猜」，不是「懂」。
//! 2. **只进 prompt 参考段，不改 SQL**：它是提示不是判据（与 `memory` 同一条纪律，
//!    见 `ddl.rs` 里 meta.memory 的红字）。确定性装配器一个字都不看它。
//! 3. **证据不足就不用**：同一习惯出现 <3 次视为噪声。一次巧合不该变成默认。
//!
//! ## I4/I5
//!
//! 聚合谓词恒带 `login_name = $1`（按人隔离，别人的习惯一条都进不来）；
//! 产出的是**用户自己打过的字**，进 prompt 前照样过 `wrap_untrusted` 同款截长与剥控制字符。

use sqlx::PgPool;

/// 一个习惯至少要被印证这么多次才算数。1 次是巧合，2 次可能还是巧合。
const MIN_SUPPORT: i64 = 3;
/// 只看最近这么多天：三个月前的习惯不代表现在（业务口径会变、岗位会调）。
const WINDOW_DAYS: i32 = 60;
/// 进 prompt 的片段截长（与 `memory` 的 400 字同族纪律）
const CLIP: usize = 40;

/// 用户的高频表达。字段全是**用户自己打过的原文词**，不是我们推断的语义。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UserPref {
    /// 最常用的时间说法（「本月」「今年」…）。`None` = 证据不足。
    pub time_word: Option<String>,
    /// 最常用的分组说法（「按省区」「按客户」…）。`None` = 证据不足。
    pub breakdown_word: Option<String>,
}

impl UserPref {
    pub fn is_empty(&self) -> bool {
        self.time_word.is_none() && self.breakdown_word.is_none()
    }

    /// 渲染成 prompt 的一小段。**空则一个字都不出**（本仓「空段不出标题」的既有做法）。
    ///
    /// 措辞刻意写成「参考」而不是「要求」：它是统计出来的习惯，不是用户这一轮的表达。
    pub fn prompt_section(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut parts = Vec::new();
        if let Some(t) = &self.time_word {
            parts.push(format!("时间口径常用「{t}」"));
        }
        if let Some(b) = &self.breakdown_word {
            parts.push(format!("常按「{b}」看"));
        }
        format!(
            "\n## 该用户的历史习惯（**参考**，不是本轮要求）\n\
             {}。\n\
             仅当本轮问句**没有**给出时间或分组时才可参考；用户这一轮说了什么，一律以他说的为准。\n",
            parts.join("；")
        )
    }
}

/// 高频时间说法与分组说法的候选词（与 `kernel::nl::time` / 维度词表同源的**表层**说法）。
/// 刻意只认这些固定字面量：从问句里自由抽词会把客户名、商品名也当成「习惯」。
const TIME_WORDS: &[&str] =
    &["本月", "上月", "今年", "去年", "本周", "上周", "今天", "昨天", "本季度", "上季度"];
const BREAKDOWN_WORDS: &[&str] =
    &["按省区", "按省份", "按客户", "按商品", "按门店", "按业务员", "按月", "按分类", "按品牌"];

/// 从 `meta.query_log` 现算该用户的习惯。任何失败都返回空习惯（这是增强，不是主路）。
///
/// 只统计**成功出数**的轮次（`error = ''` 且 `row_count > 0`）：答错/答空的那次问法
/// 不该被当成习惯沉淀下来。
pub async fn load(pg: &PgPool, login: &str) -> UserPref {
    if login.is_empty() {
        return UserPref::default();
    }
    // ds:any —— `meta.query_log` 是全局观测表（本文件头「不新建学习表」那一段说明了为什么）
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT question FROM meta.query_log \
         WHERE login_name = $1 AND error = '' AND row_count > 0 \
           AND at > now() - make_interval(days => $2) \
         ORDER BY at DESC LIMIT 300",
    )
    .bind(login)
    .bind(WINDOW_DAYS)
    .fetch_all(pg)
    .await
    .unwrap_or_else(|e| {
        tracing::debug!(err = %e, "用户习惯读取失败 → 本轮不带习惯段");
        Vec::new()
    });
    let questions: Vec<String> = rows.into_iter().map(|(q,)| q).collect();
    UserPref {
        time_word: top_word(&questions, TIME_WORDS),
        breakdown_word: top_word(&questions, BREAKDOWN_WORDS),
    }
}

/// 候选词里出现次数最多的那个；不到 [`MIN_SUPPORT`] 一律返回 `None`。
///
/// 纯函数（判据打这里）：同票时按候选表顺序取先者 —— 顺序即行为，不许按 HashMap 迭代序，
/// 那会让同一批历史在两次进程里给出不同习惯。
pub fn top_word(questions: &[String], candidates: &[&str]) -> Option<String> {
    let mut best: Option<(&str, i64)> = None;
    for word in candidates {
        let n = questions.iter().filter(|q| q.contains(word)).count() as i64;
        if n >= MIN_SUPPORT && best.is_none_or(|(_, b)| n > b) {
            best = Some((word, n));
        }
    }
    best.map(|(w, _)| w.chars().take(CLIP).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// 🔴 证据不足就不用：一次巧合不该变成默认。
    #[test]
    fn below_min_support_is_not_a_habit() {
        assert_eq!(top_word(&qs(&["本月销售额", "今年销售额"]), TIME_WORDS), None);
        assert_eq!(
            top_word(&qs(&["本月销售额", "本月订单数", "本月毛利"]), TIME_WORDS),
            Some("本月".to_string())
        );
    }

    /// 同票按候选表顺序取先者：同一批历史在两次进程里必须给出同一个习惯。
    #[test]
    fn ties_are_deterministic() {
        let history = qs(&["本月A", "本月B", "本月C", "上月A", "上月B", "上月C"]);
        assert_eq!(top_word(&history, TIME_WORDS), Some("本月".to_string()));
        // 反过来放也一样（判据不看输入顺序，只看候选表顺序）
        let flipped = qs(&["上月A", "上月B", "上月C", "本月A", "本月B", "本月C"]);
        assert_eq!(top_word(&flipped, TIME_WORDS), top_word(&history, TIME_WORDS));
    }

    /// 空习惯一个字都不出（本仓「空段不出标题」的既有做法）。
    #[test]
    fn empty_pref_renders_nothing() {
        assert_eq!(UserPref::default().prompt_section(), "");
        let pref = UserPref { time_word: Some("本月".into()), breakdown_word: None };
        let out = pref.prompt_section();
        assert!(out.contains("本月") && out.contains("参考"), "{out}");
        assert!(out.contains("以他说的为准"), "必须写明不覆盖用户显式表达：{out}");
    }

    /// 🔴 只认固定候选词：从问句自由抽词会把客户名/商品名当成「习惯」带进 prompt。
    #[test]
    fn only_fixed_candidates_can_become_habits() {
        let history = qs(&["潍坊程祥商贸有限公司", "潍坊程祥商贸有限公司", "潍坊程祥商贸有限公司"]);
        assert_eq!(top_word(&history, TIME_WORDS), None);
        assert_eq!(top_word(&history, BREAKDOWN_WORDS), None);
    }
}
