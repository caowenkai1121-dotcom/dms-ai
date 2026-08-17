//! 确定性路径与 agent 之间的**共享类型**（T8-B5 下沉）。
//!
//! 为什么在 semantic：`try_compose`/`try_direct` 这些确定性产出方要迁到本 crate，而它们的
//! 返回类型此前住在 agent（`answerers/hits.rs` 与 `answerers/graph.rs`，两处都写着
//! 「ponytail: 本轮唯一允许的临时重复，T8 时删掉」）。agent 依赖 semantic，反过来不行，
//! 所以类型必须落在这一侧；agent 保留 `pub use` 让调用点一个字都不用改。
//!
//! ARCHITECTURE §4.4 的 `lib.rs` 行早就把 `DirectHit` 写在 semantic 名下 —— 这是把声明兑现。

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentSlotKind {
    Metric,
    Entity,
    Region,
    Time,
    Filter,
    Breakdown,
    Comparison,
    Detail,
}


/// 确定性解析器产生的 typed evidence。只表示已经唯一解析的原文槽位，
/// 不把 SQL 文本、表名或内部 ID 暴露到回归摘要。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecutionEvidence {
    pub resolved: Vec<ResolvedSlot>,
    pub comparison_count: usize,
    pub detail: bool,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSlot {
    pub kind: IntentSlotKind,
    pub surface: String,
}

impl ExecutionEvidence {
    pub fn resolve(mut self, kind: IntentSlotKind, surface: impl Into<String>) -> Self {
        let slot = ResolvedSlot {
            kind,
            surface: surface.into(),
        };
        if !self.resolved.contains(&slot) {
            self.resolved.push(slot);
        }
        self
    }

    pub fn with_detail(mut self) -> Self {
        self.detail = true;
        self
    }

    pub fn with_issue(mut self, issue: impl Into<String>) -> Self {
        push_unique(&mut self.issues, issue.into());
        self
    }

    /// 跨 crate 可见：覆盖闸住在 agent，而本类型下沉到了 semantic（原为同模块私有）。
    ///
    /// 相等之外多认一种形态：**槽位表面 = 量词前缀 + 证据**（`总库存量` ← `库存量`）。
    /// 🔴 2026-08-17 生产实测：「现在总库存量是多少」答出 1.06 亿，收据却标 blocked
    /// —— SQL 别名是 `库存量`、槽位表面是 `总库存量`，精确相等对不上。
    /// **给了数字又说证不出来**，比干脆答不出更糟：用户不知道该不该信。
    ///
    /// 只认「量词/汇总修饰词」这一类前缀，且剥完必须**逐字等于**证据 —— 不是子串包含。
    /// 子串包含会让「额」证明「销售额」，那是把闸门拆了。
    /// 同族一次收口：总销售额/全部订单数/合计毛利/累计退款额……
    pub fn proves(&self, kind: IntentSlotKind, surface: &str) -> bool {
        self.resolved.iter().any(|slot| {
            slot.kind == kind
                && (folded_eq(&slot.surface, surface)
                    || quantifier_stripped_eq(&slot.surface, surface))
        })
    }
}

/// `surface` 去掉开头的量词/汇总修饰词后是否逐字等于 `evidence`。
/// 词表与 `fastpath::stock` 里那份「残余只剩修饰词就不是商品名」同源同义 ——
/// 一个管「别把修饰词当实体」，一个管「别因为修饰词认不出指标」，两面同一件事。
fn quantifier_stripped_eq(evidence: &str, surface: &str) -> bool {
    strip_quantifier_prefix(surface).is_some_and(|rest| folded_eq(rest, evidence))
}

/// 量词/汇总修饰词。**全仓唯一一份**：三个消费者共用 ——
/// ① `quantifier_stripped_eq`（指标证明：`库存量` 要能证明 `总库存量`）；
/// ② `agent::ctx::metric_has_actual_value`（结果列核对：别名表里没有 `总库存量` 这个键）；
/// ③ `fastpath::stock`（残余只剩修饰词就不是商品名）。
/// 抄第二份的代价立刻可见：2026-08-17 同一个「总」字在三处各绊了一次，
/// 表现分别是 need-intent 反问卡、收据标 blocked、答出数却说证不出来。
pub const QUANTIFIER_PREFIXES: &[&str] = &[
    "总共", "一共", "全部", "所有", "合计", "整体", "累计", "汇总", "总", "全", "共",
];

/// 剥掉开头的量词修饰词；剥完为空（「总」自己）或压根没有前缀时返 `None`。
/// 🔴 调用方拿到的是**剩余部分**，必须再做逐字比对 —— 子串包含会让「额」冒充「销售额」。
pub fn strip_quantifier_prefix(surface: &str) -> Option<&str> {
    let surface = surface.trim();
    // 整串就是修饰词 → 没有指标可言
    if QUANTIFIER_PREFIXES.contains(&surface) {
        return None;
    }
    let stripped = QUANTIFIER_PREFIXES.iter().find_map(|q| {
        surface
            .strip_prefix(q)
            .map(str::trim)
            .filter(|rest| !rest.is_empty())
    })?;
    // 🔴 剥完**仍然**是修饰词（`总共` → `共`）也不算：判据当场逮到的真 bug ——
    // `find_map` 命中最长的「总共」时剥成空被过滤掉，于是退而命中「总」，留下一个「共」。
    // 词表里的叠词（总共/一共）天然会撞上这个形状。
    (!QUANTIFIER_PREFIXES.contains(&stripped)).then_some(stripped)
}


/// 大小写与首尾空白无关的相等（原 `agent::intent::folded_eq`，随 evidence 一起下沉）
fn folded_eq(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

/// 确定性命中的产物形态：出数还是出澄清卡。
#[derive(Debug, Clone)]
pub enum DirectOutcome {
    Data,
    Clarification(String),
}

/// 确定性命中：SQL（未注入）+ 路由标签 + 可选上期查询（KPI 环比）。
pub struct DirectHit {
    pub outcome: DirectOutcome,
    pub sql: String,
    pub route: String,
    /// (上期 SQL, 环比标签如"较上月")——仅高频聚合单指标时有
    pub prev: Option<(String, String)>,
    /// 额外基期查询（销售类通常为同比）。第一基期继续走 `prev`，保证旧调用与精简模式兼容。
    pub comparisons: Vec<(String, String)>,
    /// 补充明细 SQL：单据保留 Entity 头卡，聚合保留 KPI 卡，再追加图表/表格。
    pub detail: Option<String>,
    /// 销售单指标 KPI 的同窗补充 SQL（指标集＝`sales_fact::CONTEXT_METRICS`）。
    pub sales_context: Option<String>,
    /// 确定性解析器兑现但 SQL 因换码而不再保留的原文槽位，例如 `entity:商品原名`。
    pub intent_evidence: ExecutionEvidence,
}

/// 图关系问法的三种形态（识别函数 `detect_relation` 同批迁入 `fastpath`）。
#[derive(Debug, PartialEq, Eq)]
pub enum Relation {
    /// 买过某商品的客户（含实体名）
    BuyersOfGoods(String),
    /// 某客户买过什么
    GoodsOfCustomer(String),
    /// 买某商品还买什么（共购）
    Copurchase(String),
}

#[cfg(test)]
mod quantifier_tests {
    use super::*;

    /// 量词前缀不改变指标本身：`库存量` 应当证明 `总库存量`。
    ///
    /// 2026-08-17 生产实测：「现在总库存量是多少」答出 1.06 亿，收据却标 blocked
    /// （`metric-unverified:总库存量`）—— **给了数字又说证不出来**，比干脆答不出更糟。
    /// 反向那一半同样重要：不许放宽成子串包含，否则「额」能证明「销售额」，闸门就废了。
    #[test]
    fn quantifier_prefix_does_not_break_metric_proof() {
        let e = ExecutionEvidence::default().resolve(IntentSlotKind::Metric, "库存量");
        for surface in ["库存量", "总库存量", "全部库存量", "合计库存量", "累计库存量"] {
            assert!(e.proves(IntentSlotKind::Metric, surface), "{surface} 该被证明");
        }
        // 🔴 不许放宽成子串/后缀包含
        // 🔴「剥完仍**包含**证据但不相等」——这一族专防把 folded_eq 放宽成 contains。
        // 少了它，`总库存量明细`、`全部库存量占比` 都会被当成已证明（2026-08-17 反向验证抓到）。
        for surface in ["总库存量明细", "全部库存量占比", "合计库存量环比"] {
            assert!(!e.proves(IntentSlotKind::Metric, surface), "{surface} 不该被证明（剥完只是包含，不是相等）");
        }
        for surface in ["库存金额", "冻结库存量", "门店库存量", "总额"] {
            assert!(!e.proves(IntentSlotKind::Metric, surface), "{surface} 不该被证明");
        }
        // 「总」自己不是指标：剥完为空不算证明
        let bare = ExecutionEvidence::default().resolve(IntentSlotKind::Metric, "");
        assert!(!bare.proves(IntentSlotKind::Metric, "总"), "空证据不许证明任何东西");
        // 槽位种类不许串：同名不同 kind 不算证明
        assert!(!e.proves(IntentSlotKind::Breakdown, "库存量"), "kind 必须一致");
    }
}

#[cfg(test)]
mod quantifier_prefix_tests {
    use super::*;

    /// 量词前缀的剥法：全仓三个消费者共用这一份，所以判据也钉在这里。
    ///
    /// 2026-08-17 同一个「总」字在三处各绊了一次，表现各不相同却同源：
    /// ① `fastpath::stock` 把「总」当商品名 → 整题落 need-intent 反问卡；
    /// ② `ExecutionEvidence::proves` 精确相等对不上 → 收据标 blocked；
    /// ③ `agent::ctx::metric_has_actual_value` 的别名表没有这个键 → 答出数却说证不出来。
    /// 抄第二份词表的代价，这三条就是账单。
    #[test]
    fn quantifier_prefix_strips_only_when_something_remains() {
        for (surface, rest) in [
            ("总库存量", "库存量"),
            ("全部订单数", "订单数"),
            ("合计毛利额", "毛利额"),
            ("累计退款额", "退款额"),
            ("总共销售额", "销售额"),
        ] {
            assert_eq!(strip_quantifier_prefix(surface), Some(rest), "{surface}");
        }
        // 🔴 剥完为空 = 它本身就只是个修饰词，不是指标名
        for bare in QUANTIFIER_PREFIXES {
            assert_eq!(strip_quantifier_prefix(bare), None, "「{bare}」自己不是指标");
        }
        // 没有前缀就不动它（不许无中生有地剥）
        for plain in ["库存量", "销售额", "毛利率", "订单数"] {
            assert_eq!(strip_quantifier_prefix(plain), None, "{plain} 不该被剥");
        }
        // 前缀在中间不算：「库存总量」是完整指标名，不是「库存」+「总量」
        assert_eq!(strip_quantifier_prefix("库存总量"), None);
    }
}
