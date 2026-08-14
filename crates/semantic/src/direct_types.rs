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
    pub fn proves(&self, kind: IntentSlotKind, surface: &str) -> bool {
        self.resolved
            .iter()
            .any(|slot| slot.kind == kind && folded_eq(&slot.surface, surface))
    }
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
