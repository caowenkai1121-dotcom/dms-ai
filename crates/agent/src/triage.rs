//! 【K5】意图分诊：一个输入框里既问数又问文档，后端自己判走哪条路。变更原因＝路由判据。
//!
//! 判据顺序敏感（写死并单测）：
//! ① `forced`（前端能力 chip）→ 直接用，**不调 LLM、不查库**
//! ② 规则（0-LLM）：指标名/维度名/术语/时间词/表名/单号 → Data；制度类词/文件扩展名 → Knowledge
//! ③ 规则不决 → fast LLM 一次二分类；**超时/失败/答非所问一律 Data**
//!
//! **失败方向只有一个：Data**。知识库没上传过文档的用户不能因为分诊挂掉就问不了数——
//! 分诊是路由优化，不是闸门，没有 fail-closed 这回事。
//!
//! **hybrid（两路都答）明确不做**：两侧都命中时归 Data 并记一行 info。
//! 两条断言（`both_hit_goes_to_data`）钉着这个方向，且 `Intent` 多一个变体就得有人消费它 ——
//! 攒真实样本再判值不值得做，别先造一个要拆掉的转换层。
//!
//! 搬运源 `server/src/triage.rs` 全文（判据、文案、测试断言逐字保留）。

use std::borrow::Cow;
use std::time::Duration;

use sqlx::PgPool;

use dms_kernel::{ChatModel, ChatRequest, ModelTier};
use dms_semantic::recall::{self, RecallCtx};

/// 【判官实测 2026-08-10·问题 1①】常见错别字归一表（**词表驱动**，加词改表不改逻辑）。
/// 实测现场：「上个月消售额多少」（消售=错别字）→ 规则判据与注册表召回同时失明
/// （「消售额」不是任何指标名/别名），落 LLM 全目录路径打了 ODS 订单表 = 2.29 亿；
/// 而正确问法「上个月销售额多少」走 verified 合同 = 2.03 亿 —— **同题两个答案**，
/// 且两边 trust=high、一个字提示都没有。归一做在分诊/问答的最前面：下游所有判据
/// （规则、注册表召回、语义缓存键、LLM prompt）见到的都是归一后的问句，
/// 错别字问法与正确问法走同一条路。
///
/// 🔴 收词纪律：只收「错形在任何合法业务文本里都不可能成立」的成对词（消售/销受/售销/定单/裤存/对帐）。
/// 单字条目一律不收 —— 「报销」「撤销」里的「销」一个字都不许动；词形不够歧义安全的词也不收
/// （判宽的代价是把正经词改坏，那比不识别更坏）。
const TYPO_PAIRS: &[(&str, &str)] = &[
    ("消售", "销售"),
    ("销受", "销售"),
    ("售销", "销售"),
    ("定单", "订单"),
    ("裤存", "库存"),
    ("对帐", "对账"),
];

/// 错别字归一（纯函数）：命中词表才改写，逐对全量替换（词表内各对互不重叠，顺序无关；
/// 所以「哪些对命中」在原文上过滤与在改写途中过滤等价）。无命中返回 `Cow::Borrowed`
/// （干净问句零分配）。幂等：归一结果再归一一次逐字不变。
///
/// 两个调用点：`triage()` 入口（归一只影响分诊判定，路由出去的原问句不动）与
/// `ask()` 的多轮改写之后（真正送去选源/召回/生成的那份）。
pub fn normalize_typos(q: &str) -> Cow<'_, str> {
    // 命中路径只替换**命中的**那几对，不再 6 对全量各扫一遍
    let hits: Vec<(&str, &str)> =
        TYPO_PAIRS.iter().filter(|(wrong, _)| q.contains(wrong)).copied().collect();
    if hits.is_empty() {
        return Cow::Borrowed(q);
    }
    let mut s = q.to_string();
    for (wrong, right) in hits {
        s = s.replace(wrong, right);
    }
    Cow::Owned(s)
}

/// 相对时间词表（模块级）：`time_tokens` 与 `time_hit` 共用这一份 —— 抄第二份必漂。
const TIME_WORDS: &[&str] =
    &["今天", "昨天", "前天", "本月", "上月", "上个月", "这个月", "本周", "上周", "今年", "去年", "本季度"];

/// 时间词集合（护栏：命中缓存的问题时间词必须与本问全等，"上月"≠"本月"）。
/// 搬运源 `server/src/pipeline.rs:953`，逻辑一字未改。
///
/// 🔴 **单一事实源**：分诊的 `time_hit` 与语义缓存的护栏（`answerers/cache.rs::passes_guards`）
/// 共用这一份 —— 改它会同时影响两处，抄第二份就是埋一处会漂的词表。
/// ponytail: 按 ARCHITECTURE §4.6 的迁移表它的家是 `answerers/cache.rs`；两个文件由并行任务
/// 同时落地，最终收敛到「住在 triage.rs、cache.rs `use` 它」（cache.rs 侧已写明同一句）。
/// 要归位就是两个文件各改一行的收尾动作，不影响行为。
pub fn time_tokens(q: &str) -> std::collections::BTreeSet<&'static str> {
    TIME_WORDS.iter().copied().filter(|t| q.contains(t)).collect()
}

/// 分诊结果。两个变体 = 今天真实存在的两条链路（`ask::ask` / `knowledge::answer::answer`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Data,
    Knowledge,
}

/// fast LLM 兜底的超时。`ChatModel` 的实现自带 90s HTTP 超时，分诊等不了那么久：
/// 分诊判错只是路由差一点（兜底恒 Data），卡 90s 是整条问答都废了。
const LLM_TIMEOUT: Duration = Duration::from_secs(8);

/// 知识库侧关键词：制度/流程类问法 + 三个文件扩展名（用户常直接贴文件名）。
/// 「政策/规范/手册/指南/sop」与制度同类（2026-08-11 实测「报销政策是什么」一个词都不命中、
/// 被指标词抢去问数）。「资料」刻意不收：「客户资料」是实体/问数语境。
const KB_WORDS: &[&str] = &[
    "制度", "规定", "流程", "怎么办", "如何", "标准", "模板", "合同", "办法", "须知",
    "政策", "规范", "手册", "指南", "sop", ".pdf", ".docx", ".xlsx",
];

/// 分诊入口。`ds` = 注册表召回的数据源（今天恒主源，见 `api_ask` 的注释）。
/// 返回值不是 `Result`：**任何异常都降级 Data**，把失败往上抛就等于让分诊能打死问数。
pub async fn triage(
    llm: &dyn ChatModel,
    pg: &PgPool,
    ds: &str,
    question: &str,
    forced: Option<&str>,
) -> Intent {
    // 【判官实测·问题 1①】错别字归一先于一切判据（含 forced 判读）：「消售」这类错形会让
    // 规则判据与注册表召回同时失明。归一只影响分诊**判定** —— 路由出去的原问句不动，
    // 送去分析的那份由 `ask()` 入口再归一一次。
    let normalized = normalize_typos(question);
    let question = normalized.as_ref();
    // ① 前端 chip 显式指定：一次 IO 都不许发生（`auto` / 未知值解析成 None，继续往下）
    if let Some(i) = forced.and_then(parse_forced) {
        return i;
    }
    let kb = kb_hit(question);
    // 裸实体名（公司后缀/渠道前缀/型号规格）是确定性 Data：规则与注册表都不认识客户/商品名，
    // 以前落给 fast-LLM 二分类抛硬币 —— 实测同一句客户名 17 秒内 knowledge×2 / entity-card×1
    // （query_log 2026-08-10 01:18）。`!kb` 前提：名字里撞了制度类词（「标准」「合同」）时维持原判。
    if !kb && crate::answerers::entity::entity_form_hit(question) {
        return Intent::Data;
    }
    // ② 规则。问句内部的四组问数信号（时间词/完整问句/表名/单号）在这里算一次 ——
    // 原来「纯信号快判」与「带注册表结果的裁决」两行各算一遍（纯函数同输入同输出，纯浪费）。
    // kb 侧没命中且纯信号已判 Data → 连注册表都不查：存量问数链路（多数带时间词或
    // 指标名）不该为分诊多付三条查询。kb 命中时必须查库，否则拿不到「两侧都命中 → Data」。
    let own = question_data_hit(question);
    if !kb && own {
        return Intent::Data;
    }
    let data = match registry_hit(pg, ds, question).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(err = %e, "triage: 注册表召回失败 → data");
            return Intent::Data;
        }
    };
    if let Some(i) = rule_decide(own || data, kb, question) {
        return i;
    }
    // ③ fast LLM 一次二分类（见的同样是归一后的问句 —— 与上面所有判据同一份，不是用户原文）
    llm_intent(llm, question).await.unwrap_or(Intent::Data)
}

/// 规则判据（**纯函数**，无库无网可单测）。`data_hit` 由注册表召回给（指标/维度/术语），
/// `kb_hit` 由 `kb_hit()` 给；问句内部的信号（时间词/表名/单号）在 `question_data_hit` 里算。
///
/// 两侧都命中 → Data（v1 不做 hybrid）；都不命中 → `None` 交 LLM。
pub fn rule_intent(question: &str, data_hit: bool, kb_hit: bool) -> Option<Intent> {
    rule_decide(question_data_hit(question) || data_hit, kb_hit, question)
}

/// 问句内部的四组问数信号（时间词/完整业务问句/表名/单号）：`triage()` 预计算与
/// `rule_intent` 共用这一份，判据两份必漂。
fn question_data_hit(question: &str) -> bool {
    time_hit(question)
        || analytical_question_hit(question)
        || table_hit(question)
        || doc_code_hit(question)
}

/// 命中合成 → 路由裁决。`question` 两个用途：both-hit 的强文档意图仲裁（见下）与排障日志。
fn rule_decide(data: bool, kb_hit: bool, question: &str) -> Option<Intent> {
    match (data, kb_hit) {
        (true, true) => {
            // 强文档意图翻 KB（2026-08-11 实测：「市场费用的报销政策是什么」被指标词
            // 「市场费用」抢去聚合费用总额）。除此之外维持 v1 纪律：两侧都命中归 Data。
            if strong_doc_intent(question) {
                tracing::info!(question, "triage: both-hit 但强文档意图 → knowledge");
                Some(Intent::Knowledge)
            } else {
                tracing::info!(question, "triage: both-hit → data（hybrid 待真实样本）");
                Some(Intent::Data)
            }
        }
        (true, false) => Some(Intent::Data),
        (false, true) => Some(Intent::Knowledge),
        (false, false) => None,
    }
}

/// 强文档意图：**文档名词 × 询问词**共现才成立 —— 单个指标词命中不许把制度类问句抢去问数。
/// 词表与钉板的口径：`本月报销制度`（无询问词）与「销售额如何统计」（无文档名词）都仍归 Data。
fn strong_doc_intent(q: &str) -> bool {
    const DOC_NOUNS: &[&str] = &[
        "政策", "制度", "规定", "流程", "规范", "办法", "须知", "手册", "指南", "合同", "模板", "标准", "sop",
    ];
    const ASK_WORDS: &[&str] = &[
        "是什么", "是啥", "什么", "有哪些", "哪些", "怎么", "如何", "内容", "介绍", "讲讲", "说明",
    ];
    let mut lower: Option<String> = None;
    let has_noun = DOC_NOUNS.iter().any(|w| {
        if w.is_ascii() {
            lower.get_or_insert_with(|| q.to_ascii_lowercase()).contains(w)
        } else {
            q.contains(w)
        }
    });
    has_noun && ASK_WORDS.iter().any(|w| q.contains(w))
}

/// 【混合查询】子句级识别（纯函数）：问句切成子句后，至少一条是强文档意图、且至少另有一条
/// 携带问数信号（时间词/完整业务问句/表名/单号）→ `Some((文档子句, 问数子句))`，调用方两路并行。
///
/// 整句级共现**不收**：「合同客户的销售额」的「合同」是限定词不是文档请求（强文档意图的
/// 名词×询问词判据把它挡在 doc 侧之外）；单句不收：切不出两半的句子维持单路裁决一字不变。
/// 返回的两半各自拼回完整问法喂给两路 —— 子句原文保留用户措辞，不做改写。
pub fn hybrid_clauses(question: &str) -> Option<(String, String)> {
    let clauses: Vec<&str> = question
        .split(|c: char| matches!(c, '，' | ',' | '；' | ';' | '。' | '？' | '?' | '、' | '\n'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if clauses.len() < 2 {
        return None;
    }
    let doc: Vec<&str> = clauses
        .iter()
        .copied()
        .filter(|c| strong_doc_intent(c))
        .collect();
    // 文档子句不许同时进问数半（「报销政策是什么」可能被完整问句判据认领）——两半必须互斥
    let data: Vec<&str> = clauses
        .iter()
        .copied()
        .filter(|c| !strong_doc_intent(c) && question_data_hit(c))
        .collect();
    if doc.is_empty() || data.is_empty() {
        return None;
    }
    Some((doc.join("；"), data.join("；")))
}

/// 【混合查询·整句级】意图不明确的 both-hit（2026-08-11 用户裁决：「意图不很明确时问数与
/// 知识库一起查，综合输出」）：文档词与问数信号**共现于同一句**、切不出明确两半
/// （`hybrid_clauses` 不收）、也不是强文档意图单句（`rule_decide` 翻 Knowledge 的那条
/// AX104 裁决不动）——这种句子不再二选一，由入口层整句喂两路。
///
/// 四份判据全部复用本模块既有的（一份都不新造，新造必与路由层漂开）：
/// - `kb_hit`：文档词命中；
/// - `question_data_hit`：问数信号命中（时间词/完整业务问句/表名/单号）；
/// - `!strong_doc_intent`：强文档意图单句仍走纯知识库（「市场费用的报销政策是什么」）；
/// - `!analytical_question_hit`：已构成**完整业务问句**（对象 × 查询目标）的意图已明确
///   是问数 —— 「合同金额最高的客户是谁」的「合同」是限定词不是文档请求，不双查。
///
/// 判定只发生在 HTTP/xcx 入口层（`triage()` 返回值语义不变，`both_hit_goes_to_data`
/// 的钉板不动）；与 `hybrid_clauses` 同层，同样吃用户原问句（不做错别字归一）。
pub fn unclear_both_hit(question: &str) -> bool {
    kb_hit(question)
        && question_data_hit(question)
        && !strong_doc_intent(question)
        && !analytical_question_hit(question)
}

/// 完整业务分析问句的零 IO 判据。它只确认“对象 + 查询目标”已经齐全，不替代指标召回，
/// 也不负责生成 SQL；`ask::need_intent_reply` 与分诊共用，避免两处对同一句话作出相反判断。
pub fn analytical_question_hit(question: &str) -> bool {
    // 词表只留小写：判定前统一 `to_ascii_lowercase` 一次（与 `kb_hit` 同一套大小写策略）——
    // 原来 OBJECTS 收 "SKU"/"sku" 不收 "Sku"、TARGETS 收 "top"/"TOP" 不收 "Top"，两套策略并存。
    const OBJECTS: &[&str] = &[
        "销售额", "销量", "销售量", "毛利", "成本", "收入", "订单", "客户", "商品",
        "产品", "sku", "设备", "单据", "发货", "退款", "售后", "开票", "对账",
        "库存",
    ];
    const TARGETS: &[&str] = &[
        "多少", "几", "哪些", "那些", "哪几", "哪家", "谁", "哪个", "最高", "最低",
        "最多", "最少", "排行", "排名", "趋势", "占比", "比例", "对比", "明细", "清单",
        "分布", "汇总", "合计", "top", "前十", "前20", "前二十",
    ];
    const TIME_SCOPED: &[&str] = &[
        "销售额", "销量", "销售量", "毛利", "成本", "收入", "订单", "单据", "发货",
        "退款", "售后", "开票", "对账", "库存",
    ];

    let q = question.to_ascii_lowercase();
    let has_object = OBJECTS.iter().any(|word| q.contains(word));
    (has_object
        && (TARGETS.iter().any(|word| q.contains(word))
            || RELATION_WORDS.iter().any(|word| q.contains(word))))
        || (time_hit(question) && TIME_SCOPED.iter().any(|word| q.contains(word)))
}

/// 知识库侧命中：纯 substring，零正则依赖（`.pdf` 这类点号在正则里还得转义）。
pub fn kb_hit(question: &str) -> bool {
    // 词表里需要小写化的只有三个 ASCII 扩展名（用户贴的可能是大写 .PDF）；中文词对大小写
    // 不敏感 —— 纯中文问句不为它们付一次整串堆分配（惰性：只在遇到 ASCII 词时小写化一次）。
    let mut lower: Option<String> = None;
    KB_WORDS.iter().any(|w| {
        if w.is_ascii() {
            lower.get_or_insert_with(|| question.to_ascii_lowercase()).contains(w)
        } else {
            question.contains(w)
        }
    })
}

/// 业务关系/事件词表（下单/退货/审核…）。两个消费者：`analytical_question_hit`（完整问句判据）
/// 与 `ask::need_intent_reply` 的覆盖兜底（「本月的退货情况」族：注册表词表不认识「退货」，
/// 但它说的是数仓里有的事件 —— 没有这张表，那句就会被误拦成反问）。
pub(crate) const RELATION_WORDS: &[&str] = &[
    "下单", "购买", "买过", "卖出", "卖给", "成交", "关联", "发生", "新增", "流失",
    "发货", "退货", "申请", "审核", "驳回",
];

/// 注册表命中（指标名/维度名/术语，含各自的别名与 MapFilter 净化）。
/// 三个函数是**召回链已有的那三个**，判据与 `generate_sql` 里喂 prompt 的完全同源——
/// 在这里复述一份「什么算指标名」必然与召回漂开。`||` 短路：命中就不查后面的。
///
/// `pub(crate)` 的第二个消费者是 `ask::need_intent_reply` 的覆盖兜底（fast 判 answer 之后、
/// 放行 SQL 生成之前）——「注册表认不认识这句问句」两处必须用同一份判据，抄第二份必漂。
///
/// ponytail: 表名命中只看问句里的 `t_xxx` 字面（`table_hit`），没查 `meta.kw_force`——
/// 那要多一条 SQL，而问数问句里出现真表名的比例极低（真出现时 trgm 召回照样能找到表）。
pub(crate) async fn registry_hit(pg: &PgPool, ds: &str, q: &str) -> anyhow::Result<bool> {
    // 三条召回都只吃 `(ds, question)`：`tables`/`limit`/`embed` 本组不读（形状见 `RecallCtx`）
    let cx = RecallCtx { question: q, tables: &[], limit: 0, ds, embed: None, embed_slices: &[] };
    Ok(!recall::recall_metric_hits(pg, &cx).await?.is_empty()
        || !recall::recall_dimensions(pg, &cx).await?.is_empty()
        || !recall::recall_terms(pg, &cx).await?.is_empty())
}

/// 时间词命中：相对词表**复用** `TIME_WORDS`（与 `time_tokens` 同一张表，少一处会漂的词表），
/// 外加「数字 + 年/月/日/号/季」的绝对日期形（"2024年1月"）。
/// 要求前面是数字才算：否则「年假制度」「月度须知」会被判成问数。
/// 已知边界（刻意）：数字必须**紧邻**单位，「2024 年」这种带空格的不算 —— 别当 bug 修。
fn time_hit(q: &str) -> bool {
    if TIME_WORDS.iter().any(|t| q.contains(t)) {
        return true;
    }
    let mut prev_digit = false;
    for c in q.chars() {
        if prev_digit && matches!(c, '年' | '月' | '日' | '号' | '季') {
            return true;
        }
        prev_digit = c.is_ascii_digit();
    }
    false
}

/// 表名命中：DMS 业务表一律 `t_` 前缀 + 小写字母（`t_sales_order`）。
/// 要求 `t_` 后跟至少 3 个小写字母，免得「t_」这两个字符本身成为触发器。
/// `pub(crate)` 的第二个消费者：`ask::hold_back_uncovered`（单据/表名形 = 意图明确，不拦）。
pub(crate) fn table_hit(q: &str) -> bool {
    // 判据只看 ASCII（`t_` + 小写字母）：`to_ascii_lowercase` 更便宜且语义等价
    let low = q.to_ascii_lowercase();
    low.match_indices("t_").any(|(i, _)| {
        low[i + 2..].chars().take(3).filter(|c| c.is_ascii_lowercase()).count() == 3
    })
}

/// 单号命中：DMS 单据号形如 `HJXH-DXO2025…` / `SPC-20250101-001`。
/// **只判形不判前缀**——判前缀要复述 `fastpath::doc_binding` 的映射表，那是第二份真相源；
/// 判错的代价也不对称：这里判宽只是多走一次问数（快路径自己会不命中并回落）。
/// 必须含 ASCII 字母：纯数字串（"20250101"）与带杠日期（"2025-01-01"）都不是单号，
/// 那是日期，由 `time_hit` 管 —— 否则含日期的制度类问句会被抢成 Data。
/// `pub(crate)` 的第二个消费者：`ask::hold_back_uncovered`（同上）。
pub(crate) fn doc_code_hit(q: &str) -> bool {
    q.contains("单号")
        || q.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).any(|t| {
            t.len() >= 6
                && t.chars().any(|c| c.is_ascii_digit())
                && t.chars().any(|c| c.is_ascii_alphabetic())
        })
}

/// fast LLM 二分类。答非所问/超时/挂掉 → `None`，调用方兜底 Data。
async fn llm_intent(llm: &dyn ChatModel, question: &str) -> Option<Intent> {
    let system = "你给用户问题做路由二分类。查业务数据库（销售、订单、客户、库存等结构化数据）\
                  答 data；查企业文档（制度、流程、合同、手册等文本）答 knowledge。只输出一个词。";
    let user = format!("问题：{question}\n答：");
    // 温度 0.1：与 ask.rs 三词门的 0.0 不同档 —— 本判定答错的代价只是路由差一点（兜底恒 Data），
    // 与 fast 族其余调用（追问改写/反问候选）同档；三词门的输出是协议单词，温度抖动是纯噪音，压到 0。
    let req = ChatRequest::text(ModelTier::Fast, system, &user, Some(0.1));
    let reply = tokio::time::timeout(LLM_TIMEOUT, llm.chat(req))
        .await
        .inspect_err(|_| tracing::warn!("triage: fast LLM 超时 → data"))
        .ok()?
        // 传输/调用错误同样留痕：「模型挂了」与「超时」在日志里必须分得开
        .inspect_err(|e| tracing::warn!(err = %e, "triage: fast LLM 调用失败 → data"))
        .ok()?;
    parse_intent(&reply.content?)
}

/// forced（前端能力 chip）的解析：**精确等值匹配** —— chip 的合法值就 `data`/`knowledge`
/// 两个（web/src/App.vue 的 CAPS），`auto` 与任何其它值 → `None`（继续往下判，绝不报错）。
/// 与 LLM 回复的容错解析（`parse_intent`）分开：「database」「metadata」这类词含 "data"，
/// 走 chip 通道会被抢成 Data。
fn parse_forced(s: &str) -> Option<Intent> {
    match s.trim().to_ascii_lowercase().as_str() {
        "data" => Some(Intent::Data),
        "knowledge" => Some(Intent::Knowledge),
        _ => None,
    }
}

/// LLM 回复 → Intent（**容错**，只服务 `llm_intent`；chip 通道走 `parse_forced` 的精确匹配）。
/// knowledge 先判：回复常是「knowledge，不是 data」，先判 data 会判反。
/// 只认这两个词 + 两个中文别名（模型偶尔用中文回）：`kb` 之类的缩写没有生产者，
/// 加进来只是让任何含 "kb" 的句子被抢成知识库。
fn parse_intent(s: &str) -> Option<Intent> {
    let low = s.to_lowercase();
    if low.contains("knowledge") || s.contains("知识库") {
        Some(Intent::Knowledge)
    } else if low.contains("data") || s.contains("问数") {
        Some(Intent::Data)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 切出 `triage()` 的函数体（到 `/// 规则判据` 注释为止）—— 两个源码扫描判据共用，
    /// 各自手撕同一串锚点 = 两处会漂的切片逻辑。
    fn triage_body(src: &str) -> &str {
        src.split("pub async fn triage")
            .nth(1)
            .expect("triage 没了")
            .split("/// 规则判据")
            .next()
            .unwrap()
    }

    #[test]
    fn registry_hit_routes_to_data() {
        assert_eq!(rule_intent("销售额", true, false), Some(Intent::Data));
    }

    /// 裸实体名（公司/商品形态）必须被确定性闸门钉死在 Data，**在 LLM 二分类之前** ——
    /// 实测同一句「线下-揭阳市和利食品有限公司」17 秒内 knowledge×2 / entity-card×1
    /// （query_log 2026-08-10 01:18），抛硬币的路由不允许存在。
    /// 闸门带 `!kb` 前提：名字里撞了制度类词（「标准」「合同」）时维持原判，不抢知识库。
    #[test]
    fn bare_entity_forms_never_reach_the_llm_coin_flip() {
        let body = triage_body(include_str!("triage.rs"));
        let gate = body.find("entity_form_hit").expect("triage 缺裸实体名闸门");
        let llm = body.find("llm_intent").expect("triage 缺 LLM 兜底");
        assert!(gate < llm, "裸实体名闸门必须在 LLM 二分类之前：{body}");
        assert!(
            body.contains("!kb && crate::answerers::entity::entity_form_hit"),
            "闸门不得抢知识库词：{body}"
        );
        // 两个真实 case 的行为级断言在 entity 侧（`goods_spec_evidence_pins_bare_goods_names`），
        // 这里钉调用方向：问句原样进闸门，不是改写后的什么中间形态。
        assert!(body.contains("entity_form_hit(question)"), "闸门必须吃原始问句：{body}");
    }

    /// 时间词是问数最强的信号：注册表一条都没命中也算 Data
    #[test]
    fn time_word_routes_to_data() {
        assert_eq!(rule_intent("上月的呢", false, false), Some(Intent::Data));
        assert_eq!(rule_intent("2024年1月怎么样", false, false), Some(Intent::Data));
        // 「年假」「月度」前面没有数字 → 不是时间词（否则制度类问句全被抢成问数）
        assert!(!time_hit("年假规定"));
        assert!(!time_hit("月度须知"));
        // 已知边界（刻意）：数字必须紧邻单位，「2024 年」带空格不算
        assert!(!time_hit("2024 年的制度"));
    }

    #[test]
    fn complete_business_questions_are_data_without_registry_hits() {
        for question in [
            "昨天下单的有哪些客户",
            "昨天有下单的那些客户",
            "昨天的设备订单",
            "本月销量最高的商品",
        ] {
            assert!(analytical_question_hit(question), "完整业务问题未识别：{question}");
            assert_eq!(rule_intent(question, false, false), Some(Intent::Data));
        }
        assert!(!analytical_question_hit("线下-某客户有限公司"), "裸实体名不应伪造查询目标");
        assert!(!analytical_question_hit("可颂香肠卷"), "裸商品名应交实体解析");
        assert!(!analytical_question_hit("客户名称 线下-某客户有限公司，本月"));
        assert!(!analytical_question_hit("设备保温柜 DHT150-6，昨天"));
    }

    /// 大小写策略与 `kb_hit` 统一（词表只留小写，判定前整串小写化一次）：
    /// 「Sku」「Top」这类混排写法不许漏判。
    #[test]
    fn analytical_hit_is_ascii_case_insensitive() {
        assert!(analytical_question_hit("本月Sku销量的Top榜"), "混排写法漏判：Sku/Top");
        assert!(analytical_question_hit("各门店的 SKU 占比"));
        // 反向（防恒真）：混排救不了的句子依旧不命中
        assert!(!analytical_question_hit("可颂香肠卷"));
    }

    #[test]
    fn table_name_routes_to_data() {
        assert_eq!(rule_intent("t_sales_order 有多少行", false, false), Some(Intent::Data));
        assert!(table_hit("T_CUSTOMER 的字段"), "表名大小写不敏感");
        assert!(!table_hit("t_ 是什么"), "光有 t_ 不算表名");
    }

    #[test]
    fn doc_code_routes_to_data() {
        assert_eq!(rule_intent("HJXH-DXO2025010100123", false, false), Some(Intent::Data));
        assert!(doc_code_hit("这个单号查一下"));
        assert!(doc_code_hit("SPC-20250101-001"), "字母前缀 + 带杠数字是单号");
        assert!(!doc_code_hit("报销制度2024版"), "纯数字是日期不是单号");
        assert!(!doc_code_hit("本月销售额"), "无 ASCII 串不算单号");
        // 带杠日期不是单号：含日期的制度类问句不许被抢成 Data
        assert!(!doc_code_hit("2025-01-01 的报销制度"), "带杠日期族漏网会抢知识库的问句");
        assert!(!doc_code_hit("2025-01-01 到 2025-12-31 的情况"));
    }

    #[test]
    fn kb_words_route_to_knowledge() {
        assert_eq!(rule_intent("报销制度是什么", false, kb_hit("报销制度是什么")), Some(Intent::Knowledge));
        assert!(kb_hit("差旅标准"));
        assert!(kb_hit("看看 合同模板.DOCX"), "扩展名大小写不敏感");
        assert!(kb_hit("报销政策"), "政策是制度类词");
        assert!(kb_hit("皇家小虎巡店SOP"), "sop 大小写不敏感");
        assert!(kb_hit("陈列手册") && kb_hit("操作指南"));
        assert!(!kb_hit("客户资料"), "资料不收：实体/问数语境");
        assert!(!kb_hit("本月销售额是多少"));
    }

    /// v1 不做 hybrid：两侧都命中一律归 Data（存量问数不许被知识库抢走）——
    /// 唯一例外是**强文档意图**（文档名词 × 询问词共现），那条翻 Knowledge。
    #[test]
    fn both_hit_goes_to_data() {
        assert_eq!(rule_intent("本月报销制度", false, true), Some(Intent::Data));
        assert_eq!(rule_intent("销售额如何统计", true, true), Some(Intent::Data));
    }

    /// 强文档意图仲裁（2026-08-11「报销政策是什么」被抢去问数的事故钉）：
    /// 文档名词 × 询问词共现 → both-hit 翻 Knowledge；缺任一族维持 Data。
    #[test]
    fn strong_doc_intent_flips_both_hit_to_knowledge() {
        assert_eq!(
            rule_intent("市场费用的报销政策是什么", true, true),
            Some(Intent::Knowledge),
            "政策 × 是什么 必须翻知识库"
        );
        assert_eq!(rule_intent("巡店SOP有哪些内容", true, true), Some(Intent::Knowledge));
        assert_eq!(rule_intent("退货流程怎么走", true, true), Some(Intent::Knowledge));
        // 缺询问词 / 缺文档名词：维持 both-hit → Data 的 v1 纪律
        assert!(!super::strong_doc_intent("本月报销制度"));
        assert!(!super::strong_doc_intent("销售额如何统计"));
        assert!(!super::strong_doc_intent("合同金额最高的客户是谁"), "谁 不是文档询问词");
    }

    /// 都不命中 → None：由 `triage` 交给 fast LLM（规则不许瞎猜）
    #[test]
    fn neither_hit_is_undecided() {
        assert_eq!(rule_intent("你好", false, false), None);
        assert_eq!(rule_intent("帮我看看那个东西", false, false), None);
    }

    /// 混合查询识别（子句级）：文档半 × 问数半都成立才两路并行；
    /// 整句共现/单句/纯问数/纯文档一律不收（维持单路裁决）。
    #[test]
    fn hybrid_clauses_split_only_when_both_halves_present() {
        assert_eq!(
            super::hybrid_clauses("市场费用的报销政策是什么，本月市场费用花了多少"),
            Some(("市场费用的报销政策是什么".to_string(), "本月市场费用花了多少".to_string()))
        );
        assert_eq!(
            super::hybrid_clauses("退货流程怎么走？本月退货金额是多少"),
            Some(("退货流程怎么走".to_string(), "本月退货金额是多少".to_string()))
        );
        // 单句不收（强文档意图单句照旧走 Knowledge 单路）
        assert_eq!(super::hybrid_clauses("市场费用的报销政策是什么"), None);
        // 限定词里的文档名词不算文档半（「合同」是客户的限定，不是要查文档）
        assert_eq!(super::hybrid_clauses("合同客户的销售额是多少，本月订单数"), None);
        // 纯问数多子句不收（那是 compound 的地盘）
        assert_eq!(super::hybrid_clauses("本月销售额，上月销售额"), None);
        // 纯文档多子句不收
        assert_eq!(super::hybrid_clauses("报销政策是什么，差旅标准有哪些"), None);
    }

    /// 整句级 both-hit（2026-08-11 用户裁决：意图不明确时问数 + 知识库双查、综合输出）：
    /// 文档词 × 问数信号共现、且两端都不"明确"（非强文档意图、未构成完整业务问句）才双查。
    /// 判定的落点在入口层 —— `triage()` 与 `rule_decide` 的 both-hit 语义一字不变。
    #[test]
    fn unclear_both_hit_only_when_intent_is_genuinely_ambiguous() {
        // 时间词 × 文档名词、无询问词：意图不明 → 双查
        assert!(super::unclear_both_hit("本月报销制度"));
        // 强文档意图单句：仍走纯知识库（AX104 裁决不动），不双查
        assert!(!super::unclear_both_hit("市场费用的报销政策是什么"));
        // 纯问数：kb 词不命中 → 不双查
        assert!(!super::unclear_both_hit("本月销售额"));
        // 「合同」是限定词不是文档请求：完整业务问句（客户 × 最高）= 意图已明确是问数，不双查
        assert!(!super::unclear_both_hit("合同金额最高的客户是谁"));
    }

    /// forced 覆盖规则：前端 chip 选了知识库，问句再像问数也走知识库。
    /// chip 通道是**精确匹配**：「database」「metadata」这类含 "data" 的词不许被吞成 Data。
    #[test]
    fn forced_overrides_rules() {
        let q = "本月销售额是多少"; // 规则会判 Data
        assert_eq!(rule_intent(q, true, false), Some(Intent::Data));
        assert_eq!(parse_forced("knowledge"), Some(Intent::Knowledge));
        assert_eq!(parse_forced("data"), Some(Intent::Data));
        // `auto`（前端「自动」chip 实际传 null，但传字面量也不许被当成强制）与未知值都不决
        assert_eq!(parse_forced("auto"), None);
        assert_eq!(parse_forced(""), None);
        assert_eq!(parse_forced("hybrid"), None);
        // 精确匹配：含 "data"/"knowledge" 的更长词不是 chip 值（容错是 LLM 通道的事）
        assert_eq!(parse_forced("database"), None);
        assert_eq!(parse_forced("metadata"), None);
        assert_eq!(parse_forced("knowledge base"), None);
    }

    /// LLM 回复容错：带解释、带标点、大小写都要认；knowledge 必须先判
    ///（tolerant 只在这条通道：模型回复常是「knowledge，不是 data」）。
    #[test]
    fn llm_reply_parsing_is_tolerant() {
        assert_eq!(parse_intent("Knowledge"), Some(Intent::Knowledge));
        assert_eq!(parse_intent("knowledge，不是 data"), Some(Intent::Knowledge));
        assert_eq!(parse_intent("答：data。"), Some(Intent::Data));
        assert_eq!(parse_intent("我不确定"), None);
    }

    /// 搬运源 `pipeline.rs` 的 `cache_time_guard`（断言体一字未改）。词表住在本文件，
    /// 语义缓存的护栏 `use` 它 —— 这一条同时守住两个消费者：少一个词就是「上月」能命中
    /// 「本月」的缓存，多给一个月份错的数字，而它长得完全正常。
    #[test]
    fn cache_time_guard() {
        // 本月 ≠ 上月：护栏必须拦
        assert_ne!(time_tokens("本月销售额"), time_tokens("上月销售额"));
        // 同时间词：可命中
        assert_eq!(time_tokens("本月销售额是多少"), time_tokens("查本月销售额"));
    }

    /// 分诊确实读的是那张表（不是另抄一份判据）：表里的相对词**无数字前缀也算** Data。
    #[test]
    fn time_hit_reads_the_token_table() {
        for w in ["今天", "上个月", "本季度", "去年"] {
            assert!(time_hit(w), "词表里的相对词必须命中：{w}");
        }
        // 不在表里、又没有数字前缀 → 不算时间词（判宽会把制度类问句抢成问数）
        assert!(!time_hit("年假规定"));
    }

    /// 🔴 【判官实测·问题 1①】错别字归一表：词表驱动、幂等、不误伤正经词。
    /// 判官原案：「上个月消售额多少」归一后必须与「上个月销售额多少」走同一条路 ——
    /// 同题两答案（2.29 亿 vs 2.03 亿）不允许再现。
    #[test]
    fn typo_normalization_is_table_driven_and_safe() {
        // 判官原案：归一后与正确问法逐字相同
        assert_eq!(normalize_typos("上个月消售额多少"), "上个月销售额多少");
        // 词表每一对都真的生效（表驱动：加词不改逻辑）
        for (wrong, right) in TYPO_PAIRS {
            assert_eq!(normalize_typos(wrong), *right, "词表对 {wrong}→{right} 没生效");
            assert!(
                normalize_typos(&format!("本月{wrong}是多少")) == format!("本月{right}是多少"),
                "语境中的 {wrong} 没被归一"
            );
        }
        // 幂等：归一结果再归一逐字不变
        let once = normalize_typos("上个月消售额多少").into_owned();
        assert_eq!(normalize_typos(&once), once.as_str());
        // 干净问句零改写（Borrowed：一个字符都不动，也不分配）
        assert!(matches!(normalize_typos("本月销售额是多少"), Cow::Borrowed(_)));
        // 🔴 不误伤：含「销」的正经词一个字母都不许动（单字条目永不许进表）
        for legit in ["报销制度是什么", "撤销订单流程", "本月销售额是多少", "对账单有几笔"] {
            assert!(matches!(normalize_typos(legit), Cow::Borrowed(_)), "正经词被误改：{legit}");
        }
        // 归一后的问句必须被完整问句判据识别（判官案的路由层症状：规则判据失明）
        assert!(analytical_question_hit(&normalize_typos("上个月消售额多少")));
        assert!(analytical_question_hit(&normalize_typos("昨天定单有多少")));
    }

    /// 🔴 归一的**位置**判据（源码扫描）：必须在分诊的一切判据之前 —— 在 kb/规则之后
    /// 归一等于没归一（判据读的还是错形）。锚点 `concat!` 拼（自匹配家族，本仓惯例）。
    #[test]
    fn typo_normalization_precedes_every_triage_rule() {
        let body = triage_body(include_str!("triage.rs"));
        let norm = body
            .find(concat!("normalize_", "typos(question)"))
            .expect("triage 入口没做错别字归一");
        let kb = body.find("kb_hit(question)").expect("kb 判据没了");
        let entity = body.find("entity_form_hit(question)").expect("实体闸门没了");
        let registry = body.find("registry_hit(pg, ds, question)").expect("注册表召回没了");
        assert!(norm < kb && norm < entity && norm < registry, "归一必须在一切判据之前：{body}");
        // 归一也先于 forced 判读（统一入口语义：分诊全程只见归一后的问句）
        let forced = body.find("forced.and_then(parse_forced)").expect("forced 判读没了");
        assert!(norm < forced, "{body}");
    }
}
