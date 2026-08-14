//! 文档诉求信号：用户要的是**一份文件**，还是一段数据？
//!
//! ## 🔴 为什么需要这一维（2026-08-14 架构级体检的第一根因）
//!
//! 意图合同（`dms_agent::intent::IntentV1`）13 个槽位全是**取数面**，`IntentMode` 只有
//! `data|knowledge|hybrid|unknown` —— 「下载 / 发我一份 / 打印」在合同层**无处安放**。
//! 模型只能把它塞进 `data`（或塞进 `filters` 求生，反被覆盖闸判成冲突），于是：
//!
//! ```text
//! 「下载 押金转货款申请书」
//!   → 合同 mode=data
//!   → recall::schema 的 kw_force 种子 ("押金" → t_customer_balance) 把账余表钉成第一张卡
//!   → 返回 38 行「账余充值明细」+ 一整页深度 BI
//! ```
//!
//! 用户要一份**文档**，系统给了一堆**数据行**，而且很自信。
//!
//! ## 这个模块的定位
//!
//! 它是路由裁决里**确定性**的那一维：零 IO、纯函数、与模型这次吐了哪个 token 无关。
//! 同一句话问一百遍走同一条路 —— 这正是「CLI 返 knowledge、HTTP 深度返数据表」那类
//! 同题不同答的解药（路由此前唯一的输入是一次 fast LLM 采样的 `mode` 字段）。
//!
//! ## 为什么放在 kernel 而不是 agent
//!
//! `semantic` 的召回守卫（`recall/schema.rs` 的 `kw_force`）也要用它兜底，而 `semantic`
//! 不依赖 `agent`。放这里两边都够得着，且它本来就只是词表 + `contains`，无业务语义。
//!
//! ## 判据纪律：**共现**，不是单词命中
//!
//! `is_document_request` 要求 `动词 × (名词 | 扩展名)` 同时出现。单个名词命中不算 ——
//! 「本月合同金额」有「合同」但那是限定词；单个动词命中也不算 ——「下载量多少」是指标。
//! 这条纪律是从 `agent::triage::strong_doc_intent` 的实测教训继承来的。

/// 三个信号各自是否命中。分开返回而不是只给一个 bool：调用方要区分
/// 「有文档名词但没动词」（→ 资料问句）与「动词+名词」（→ 要文件）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocSignals {
    /// 文档类名词：制度/办法/申请书/…
    pub noun: bool,
    /// 取件动词：下载/发我一份/打印/…
    pub verb: bool,
    /// 文件扩展名：.pdf/.docx/…（用户常直接贴文件名）
    pub ext: bool,
}

/// 文档类名词。前 13 个继承自 `agent::triage` 的 `strong_doc_intent`（那份词表是
/// 2026-08-11「报销政策是什么」被指标词抢去问数之后实测定下来的）；后 10 个是本轮新增，
/// 直接来自业主实测的失败样本「押金转货款**申请书**」。
///
/// 「资料」刻意不收：「客户资料」是实体/问数语境。
/// 「报告」刻意不收：「销售报告」几乎恒为问数诉求。
const DOC_NOUNS: &[&str] = &[
    "制度", "规定", "流程", "标准", "模板", "合同", "办法", "须知", "政策", "规范", "手册",
    "指南", "sop", // ↑ 继承 ↓ 新增
    "申请书", "申请表", "表单", "协议", "通知", "方案", "附件", "原件", "扫描件", "文件",
    // 「指引」：生产知识库的一级目录就叫「指引合集」，104 份文档里大量以它结尾
    // （客户打款退款指引 / 客户退出申请流程填写详细指引 / 线下设备物资处置申请单流程指引）。
    "指引", "细则",
];

/// 文件扩展名。全小写匹配（`signals` 先把问句 ASCII 小写化）。
const DOC_EXTS: &[&str] =
    &[".pdf", ".docx", ".xlsx", ".doc", ".xls", ".ppt", ".pptx", ".wps", ".txt"];

/// 取件动词。
///
/// 🔴 **「导出」刻意不收**：它在本系统里是**问数**的既有功能（结果集导出 Excel），
/// 「导出标准成本明细」「导出上月合同金额」都会被 `标准`/`合同` 撞成文档诉求 ——
/// 一个词换来一整类问数问句误路由，不划算。要文件的人极少只说「导出」而不说
/// 「下载 / 发我 / 打印」。
const DOC_VERBS: &[&str] = &[
    "下载", "发我", "发一份", "给我一份", "给我发", "打印", "找一份", "要一份", "发个",
    "来一份", "调出", "原件", "拿一份", "看看原文",
];

/// 三个信号一次算完。ASCII 词（`sop`、扩展名）需要小写化，中文词不受影响。
pub fn signals(q: &str) -> DocSignals {
    let lower = q.to_ascii_lowercase();
    DocSignals {
        noun: DOC_NOUNS.iter().any(|w| lower.contains(w)),
        verb: DOC_VERBS.iter().any(|w| lower.contains(w)),
        ext: DOC_EXTS.iter().any(|w| lower.contains(w)),
    }
}

/// 「用户要的是一份文件」。**共现**判据，见文件头的纪律段。
pub fn is_document_request(q: &str) -> bool {
    let s = signals(q);
    s.verb && (s.noun || s.ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 正例：业主实测的那句 + 扩展名直贴 + 常见取件说法。
    #[test]
    fn document_requests_are_recognized() {
        for q in [
            "下载 押金转货款申请书",
            "押金转货款申请书 下载",
            "把设备管理办法.pdf发我",
            "打印一下押金转货款申请表",
            "线下设备申请政策 给我一份",
            "客户合同模板发我一份",
            "SOP原件调出来看看",
        ] {
            assert!(is_document_request(q), "{q} 应判为要文件");
        }
    }

    /// 反例：**问数**问句一句都不许被抢走。
    ///
    /// 「导出上月合同金额」是这条判据存在的理由 —— 早一版把「导出」收进动词表，
    /// 它就会带走一整类问数问句（见 `DOC_VERBS` 的红字）。
    #[test]
    fn data_questions_are_not_stolen() {
        for q in [
            "本月合同金额多少",
            "导出上月合同金额",
            "导出标准成本明细",
            "本月各品牌销售额",
            "设备订单明细",
            "查一下潍坊程祥商贸有限公司的销售数据",
            "下载量最高的商品是哪个",
        ] {
            assert!(!is_document_request(q), "{q} 是问数，不许判成要文件");
        }
    }

    /// 共现纪律：单个信号不触发。「本月报销制度」有名词无动词 —— 它是**资料问句**
    /// （由上层的 R2 接走），不是**要文件**。
    #[test]
    fn a_single_signal_never_fires() {
        let s = signals("本月报销制度");
        assert!(s.noun && !s.verb && !s.ext);
        assert!(!is_document_request("本月报销制度"));

        let s = signals("帮我下载一下");
        assert!(s.verb && !s.noun && !s.ext);
        assert!(!is_document_request("帮我下载一下"));
    }
}
