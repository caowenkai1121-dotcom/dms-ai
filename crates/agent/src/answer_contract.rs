//! AI 文案的可执行事实合同。
//!
//! 结果表、复合子问和知识正文先被拆成带命名空间的原子事实；模型只可用 `[Q:F001]`
//! 这类内部引用支撑紧邻的事实子句。引用在返回 UI 前移除。这样校验的不是“整段里是否碰巧
//! 出现过同一个数”，而是同一子句引用的事实是否同时支持它的主体、指标和值。

use serde_json::Value;

const MAX_FACTS: usize = 128;
const MAX_CONTEXT_CHARS: usize = 180;

#[derive(Clone, Copy, Debug, PartialEq)]
enum NumberUnit {
    Plain,
    Wan,
    Yi,
    Percent,
    Count(char),
    Multiplier(char),
    Date,
}

#[derive(Clone, Debug)]
struct NumericClaim {
    value: f64,
    unit: NumberUnit,
    raw: String,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QualitativeKind {
    Highest,
    Lowest,
    Growth,
    Decline,
    Lead,
    Lag,
    Support,
    Forbid,
    Require,
    Permit,
    Disallow,
}

#[derive(Clone, Debug)]
struct QualitativeClaim {
    kind: QualitativeKind,
    raw: &'static str,
    start: usize,
    end: usize,
}

#[derive(Clone, Debug)]
struct ChineseNumberClaim {
    raw: String,
}

/// 一条只能在其自身作用域内使用的已验证事实。表格事实按“单元格”原子化，避免同一行的
/// 销售额、订单数互相借值；复合问句再用 namespace 隔开各子问。
#[derive(Clone, Debug)]
pub(crate) struct VerifiedFact {
    id: String,
    namespace: String,
    source: String,
    subjects: Vec<String>,
    metric: String,
    value: String,
    numbers: Vec<NumericClaim>,
}

/// 一次模型生成所能引用的全部事实。它不修改原数据，只裁决附加的 AI 文案能否展示。
#[derive(Default, Debug)]
pub struct AnswerContract {
    facts: Vec<VerifiedFact>,
}

#[derive(Clone, Debug)]
pub struct ContractFactInput {
    pub namespace: String,
    pub source: String,
    pub subjects: Vec<String>,
    pub metric: String,
    pub value: Value,
}

impl AnswerContract {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 服务端已经把比较字段、表格单元格拆成原子事实时，直接接收其
    /// 作用域；不再把一整段 `key=value；...` 压回文本数字池。
    pub fn from_facts(inputs: impl IntoIterator<Item = ContractFactInput>) -> Self {
        let mut contract = Self::new();
        for input in inputs.into_iter().take(MAX_FACTS) {
            let numbers = cell_numbers(&input.value);
            if numbers.is_empty() {
                continue;
            }
            contract.push_fact(
                &input.namespace,
                &input.source,
                input.subjects,
                input.metric,
                value_text(&input.value),
                numbers,
            );
        }
        contract
    }

    pub fn fact_ids(&self) -> Vec<String> {
        self.facts.iter().map(|fact| fact.id.clone()).collect()
    }

    /// 把结果表前 `limit` 行拆成原子数值事实。字符串只有在整格是数值/单位时才算数值；
    /// `小虎500G` 这类商品名仍是主体，不会把问题里的 500 变成库存证据。
    pub(crate) fn push_table(
        &mut self,
        namespace: &str,
        source: &str,
        columns: &[String],
        rows: &[Vec<Value>],
        limit: usize,
    ) {
        for row in rows.iter().take(limit) {
            if self.facts.len() >= MAX_FACTS {
                break;
            }
            let numeric = row.iter().map(cell_numbers).collect::<Vec<_>>();
            let subjects = row
                .iter()
                .zip(&numeric)
                .filter(|(value, numbers)| {
                    (numbers.is_empty()
                        || numbers.iter().all(|number| number.unit == NumberUnit::Date))
                        && !value.is_null()
                })
                .map(|(value, _)| clip(&value_text(value), 72))
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();

            for (column_index, (value, numbers)) in row.iter().zip(numeric).enumerate() {
                if numbers.is_empty()
                    || numbers.iter().all(|number| number.unit == NumberUnit::Date)
                    || self.facts.len() >= MAX_FACTS
                {
                    continue;
                }
                let metric = columns
                    .get(column_index)
                    .filter(|name| !name.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| format!("第 {} 列", column_index + 1));
                let numbers = numbers
                    .into_iter()
                    .map(|mut number| {
                        if number.unit == NumberUnit::Plain {
                            if metric.contains("亿元") || metric.contains("亿件") {
                                number.unit = NumberUnit::Yi;
                            } else if metric.contains("万元") || metric.contains("万件") {
                                number.unit = NumberUnit::Wan;
                            }
                        }
                        number
                    })
                    .collect();
                self.push_fact(
                    namespace,
                    source,
                    subjects.clone(),
                    metric,
                    value_text(value),
                    numbers,
                );
            }
        }
    }

    /// 知识正文按数字所在的局部子句拆成事实。每个数字单独一条事实，避免同一句法规里的
    /// “1 年 / 3 个月”在综合时互换。没有数字的正文仍保留在原知识回答中，不由本合同改写。
    pub(crate) fn push_text(&mut self, namespace: &str, source: &str, text: &str) {
        let claims = extract_numeric_claims(text);
        for claim in claims {
            if self.facts.len() >= MAX_FACTS {
                break;
            }
            let (start, end) = local_clause(text, claim.start, claim.end);
            let context = clip(text[start..end].trim(), MAX_CONTEXT_CHARS);
            let metric = label_before(text, claim.start).unwrap_or_else(|| source.to_string());
            self.push_fact(
                namespace,
                source,
                vec![metric.clone()],
                metric,
                context,
                vec![claim],
            );
        }

        // 纯定性规定也必须形成可引用事实，不能因为没有阿拉伯数字就绕过合同。
        for (start, end) in local_clauses(text) {
            if self.facts.len() >= MAX_FACTS {
                break;
            }
            let context = clip(text[start..end].trim(), MAX_CONTEXT_CHARS);
            let qualitative = extract_qualitative_claims(&context);
            if qualitative.is_empty() {
                continue;
            }
            for claim in qualitative {
                if self.facts.len() >= MAX_FACTS {
                    break;
                }
                let cue_start = start + claim.start;
                let metric = label_before(text, cue_start).unwrap_or_else(|| source.to_string());
                let claim_end_absolute = start + claim.end;
                let claim_end = text[claim_end_absolute..]
                    .char_indices()
                    .find(|(_, ch)| matches!(ch, '，' | ',' | '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n'))
                    .map(|(index, ch)| claim_end_absolute + index + ch.len_utf8())
                    .unwrap_or(end);
                let claim_start = text[..cue_start]
                    .char_indices()
                    .rev()
                    .find(|(_, ch)| matches!(ch, '，' | ',' | '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n'))
                    .map(|(index, ch)| index + ch.len_utf8())
                    .unwrap_or(start);
                let claim_context = clip(text[claim_start..claim_end].trim(), MAX_CONTEXT_CHARS);
                self.push_fact(
                    namespace,
                    source,
                    vec![metric.clone()],
                    metric,
                    claim_context,
                    Vec::new(),
                );
            }
        }
    }

    fn push_fact(
        &mut self,
        namespace: &str,
        source: &str,
        subjects: Vec<String>,
        metric: String,
        value: String,
        numbers: Vec<NumericClaim>,
    ) {
        let ordinal = self.facts.iter().filter(|fact| fact.namespace == namespace).count() + 1;
        self.facts.push(VerifiedFact {
            id: format!("{namespace}:F{ordinal:03}"),
            namespace: namespace.to_string(),
            source: source.to_string(),
            subjects,
            metric,
            value,
            numbers,
        });
    }

    /// 这段会放进既有的不可信资料包裹；ID 是系统生成的，主体和值仍按外部数据处理。
    pub(crate) fn render(&self) -> String {
        if self.facts.is_empty() {
            return "（没有可引用事实；AI 文案不得输出数字、日期、倍数或定性业务断言）"
                .to_string();
        }
        self.facts
            .iter()
            .map(|fact| {
                let subjects = if fact.subjects.is_empty() {
                    "（当前结果）".to_string()
                } else {
                    fact.subjects.join(" / ")
                };
                format!(
                    "[{}] namespace={}；来源={}；主体={}；指标={}；值={}",
                    fact.id,
                    fact.namespace,
                    fact.source,
                    subjects,
                    fact.metric,
                    clip(&fact.value, MAX_CONTEXT_CHARS),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 放在可信提示段的引用纪律；事实正文则必须由调用方放入不可信包裹。
    pub(crate) fn instruction() -> &'static str {
        "事实合同：每个含数字、日期、倍数、金额、数量、比例或定性业务断言的事实子句，必须在句末紧跟支持它的事实 ID，\
         例如‘山东销售额为 100 万元[Q:F001]’。一个 ID 只支持该行写明的 namespace、主体、\
         指标和值；不同主体、指标、行、子问或知识/数据来源之间禁止借数。一个子句若有多个\
         不同作用域的事实，必须拆成多个各自引用的子句。最高/最低、增长/下降、领先/落后及\
         支持/禁止/必须/可以/不可等判断也只能复述对应类型的事实。没有对应事实就省略该断言。引用仅供\
         内部核验，系统会在展示前移除。"
    }

    pub(crate) fn retry_note(errors: &[String]) -> String {
        format!(
            "上一次输出未通过事实合同：{}。请重新生成一次；逐个事实子句使用紧邻的精确 ID，\
             不得把另一个主体、指标、子问或来源里碰巧相同的数字当证据。",
            errors.iter().take(6).cloned().collect::<Vec<_>>().join("；")
        )
    }

    /// 校验成功返回已经移除内部引用的展示文本；失败只返回问题清单，由上层最多重试一次。
    pub fn validate(&self, text: &str) -> Result<String, Vec<String>> {
        let references = reference_tokens(text);
        let mut errors = Vec::new();
        for reference in &references {
            if !self.facts.iter().any(|fact| fact.id == reference.id) {
                errors.push(format!("未知事实引用 [{}]", reference.id));
            }
        }

        let masked = mask_references(text, &references);
        let all_claims = extract_numeric_claims(&masked);
        let all_qualitative = extract_qualitative_claims(&masked);
        let chinese_numbers = extract_chinese_number_claims(&masked);
        let mut associated = Vec::<(usize, usize)>::new();
        let mut associated_qualitative = Vec::<(usize, usize)>::new();
        for claim in chinese_numbers {
            errors.push(format!(
                "中文数字 {} 暂不能精确核验，请改用阿拉伯数字并引用事实",
                claim.raw
            ));
        }
        let groups = reference_groups(&references, text);
        let all_subjects = unique_subjects(&self.facts);
        let all_metrics = unique_metrics(&self.facts);
        let mut previous_group_end = 0usize;
        let mut previous_metric: Option<String> = None;

        for group in groups {
            let errors_before_group = errors.len();
            let mut segment_start = clause_start(&masked, group.start).max(previous_group_end);
            // 引用允许紧跟在句号后：`120。[Q:F001]`。此时引用前的当前
            // “子句”只有空白，应回退到句号前一子句，而不是把事实判成无主张。
            if masked[segment_start..group.start].trim().is_empty()
                && segment_start > previous_group_end
            {
                if let Some((separator, _)) = masked[..segment_start]
                    .char_indices()
                    .rev()
                    .find(|(_, ch)| matches!(ch, '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n'))
                {
                    segment_start = clause_start(&masked, separator).max(previous_group_end);
                }
            }
            let segment = &masked[segment_start..group.start];
            let claims = extract_numeric_claims(segment)
                .into_iter()
                .map(|mut claim| {
                    claim.start += segment_start;
                    claim.end += segment_start;
                    claim
                })
                .collect::<Vec<_>>();
            associated.extend(claims.iter().map(|claim| (claim.start, claim.end)));
            let qualitative = extract_qualitative_claims(segment)
                .into_iter()
                .map(|mut claim| {
                    claim.start += segment_start;
                    claim.end += segment_start;
                    claim
                })
                .collect::<Vec<_>>();
            associated_qualitative
                .extend(qualitative.iter().map(|claim| (claim.start, claim.end)));

            let cited = group
                .ids
                .iter()
                .filter_map(|id| self.facts.iter().find(|fact| &fact.id == id))
                .collect::<Vec<_>>();
            if cited.is_empty() {
                previous_group_end = group.end;
                continue;
            }

            let mentioned_subjects = mentioned(segment, &all_subjects);
            let mentioned_metrics = mentioned(segment, &all_metrics);
            if self.is_hybrid() {
                let says_data = ["取数结果", "查询结果", "数据", "表中"]
                    .iter()
                    .any(|cue| segment.contains(cue));
                let says_knowledge = ["知识库", "资料", "规定", "制度", "文档"]
                    .iter()
                    .any(|cue| segment.contains(cue));
                if says_data && cited.iter().any(|fact| fact.namespace != "DATA") {
                    errors.push("显式的数据结论引用了非 DATA 事实".to_string());
                }
                if says_knowledge && cited.iter().any(|fact| fact.namespace != "KB") {
                    errors.push("显式的知识/规定结论引用了非 KB 事实".to_string());
                }
            }
            let scope_count = cited
                .iter()
                .map(|fact| scope_key(fact))
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            if claims.len() > 1 && scope_count > 1 {
                errors.push(format!(
                    "子句‘{}’混合多个事实作用域，需拆句并分别引用",
                    clip(segment.trim(), 48)
                ));
            }

            for claim in &claims {
                let supported = cited.iter().any(|fact| {
                    (fact.numbers.iter().any(|number| numbers_match(claim, number))
                        || (claim.unit == NumberUnit::Date
                            && date_subject_matches(fact, segment, &claims)))
                        && subjects_match(fact, segment, &mentioned_subjects)
                        && metric_matches(
                            fact,
                            segment,
                            &mentioned_metrics,
                            claim,
                            previous_metric.as_deref(),
                        )
                });
                if !supported {
                    errors.push(format!(
                        "数字 {} 的紧邻引用不支持该主体/指标/值",
                        claim.raw
                    ));
                }
            }

            for claim in &qualitative {
                let supported = cited.iter().any(|fact| {
                    fact_qualitative_kinds(fact).contains(&claim.kind)
                        && subjects_match(fact, segment, &mentioned_subjects)
                        && metric_matches(
                            fact,
                            segment,
                            &mentioned_metrics,
                            &NumericClaim {
                                value: 0.0,
                                unit: NumberUnit::Plain,
                                raw: String::new(),
                                start: 0,
                                end: 0,
                            },
                            previous_metric.as_deref(),
                        )
                });
                if !supported {
                    errors.push(format!(
                        "定性断言 {} 的紧邻引用不支持该主体/指标/类型",
                        claim.raw
                    ));
                }
            }

            if claims.is_empty()
                && (!mentioned_subjects.is_empty() || !mentioned_metrics.is_empty())
                && !cited.iter().any(|fact| {
                    subjects_match(fact, segment, &mentioned_subjects)
                        && metric_matches(
                            fact,
                            segment,
                            &mentioned_metrics,
                            &NumericClaim {
                                value: 0.0,
                                unit: NumberUnit::Plain,
                                raw: String::new(),
                                start: 0,
                                end: 0,
                            },
                            previous_metric.as_deref(),
                        )
                })
            {
                errors.push(format!(
                    "子句‘{}’的引用与主体或指标不一致",
                    clip(segment.trim(), 48)
                ));
            }
            if errors.len() == errors_before_group {
                let metrics = cited
                    .iter()
                    .map(|fact| fact.metric.as_str())
                    .collect::<std::collections::BTreeSet<_>>();
                if metrics.len() == 1 {
                    previous_metric = metrics.into_iter().next().map(str::to_string);
                }
            }
            previous_group_end = group.end;
        }

        for claim in all_claims {
            if !associated.iter().any(|(start, end)| claim.start == *start && claim.end == *end) {
                errors.push(format!("数字 {} 没有紧邻事实引用", claim.raw));
            }
        }
        for claim in all_qualitative {
            if !associated_qualitative
                .iter()
                .any(|(start, end)| claim.start == *start && claim.end == *end)
            {
                errors.push(format!("定性断言 {} 没有紧邻事实引用", claim.raw));
            }
        }
        errors.sort();
        errors.dedup();
        if errors.is_empty() {
            Ok(strip_references(text, &references))
        } else {
            Err(errors)
        }
    }

    fn is_hybrid(&self) -> bool {
        self.facts.iter().any(|fact| fact.namespace == "DATA")
            && self.facts.iter().any(|fact| fact.namespace == "KB")
    }
}

#[derive(Clone, Debug)]
struct ReferenceToken {
    id: String,
    start: usize,
    end: usize,
}

#[derive(Debug)]
struct ReferenceGroup {
    ids: Vec<String>,
    start: usize,
    end: usize,
}

fn reference_tokens(text: &str) -> Vec<ReferenceToken> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(open_rel) = text[cursor..].find('[') {
        let open = cursor + open_rel;
        let Some(close_rel) = text[open + 1..].find(']') else {
            break;
        };
        let close = open + 1 + close_rel;
        let id = &text[open + 1..close];
        if looks_like_fact_id(id) {
            out.push(ReferenceToken {
                id: id.to_string(),
                start: open,
                end: close + 1,
            });
        }
        cursor = close + 1;
    }
    out
}

fn looks_like_fact_id(id: &str) -> bool {
    (id.starts_with('F') || id.contains(":F"))
        && id.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '_' | '-' | '.'))
}

fn reference_groups(references: &[ReferenceToken], text: &str) -> Vec<ReferenceGroup> {
    let mut groups = Vec::<ReferenceGroup>::new();
    for reference in references {
        if let Some(last) = groups.last_mut() {
            if text[last.end..reference.start].trim().is_empty() {
                last.ids.push(reference.id.clone());
                last.end = reference.end;
                continue;
            }
        }
        groups.push(ReferenceGroup {
            ids: vec![reference.id.clone()],
            start: reference.start,
            end: reference.end,
        });
    }
    groups
}

fn mask_references(text: &str, references: &[ReferenceToken]) -> String {
    let mut bytes = text.as_bytes().to_vec();
    for reference in references {
        for byte in &mut bytes[reference.start..reference.end] {
            *byte = b' ';
        }
    }
    // 事实 ID 只含 ASCII，覆盖不会破坏原文本的 UTF-8 边界。
    String::from_utf8(bytes).expect("masking ASCII fact references keeps UTF-8 valid")
}

fn strip_references(text: &str, references: &[ReferenceToken]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for reference in references {
        out.push_str(&text[cursor..reference.start]);
        cursor = reference.end;
    }
    out.push_str(&text[cursor..]);
    let cleaned = out
        .replace(" 。", "。")
        .replace(" ，", "，")
        .replace(" ；", "；")
        .replace(" ：", "：");
    cleaned.trim().to_string()
}

fn clause_start(text: &str, end: usize) -> usize {
    text[..end]
        .char_indices()
        .rev()
        .find(|(_, ch)| matches!(ch, '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n'))
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0)
}

fn local_clause(text: &str, claim_start: usize, claim_end: usize) -> (usize, usize) {
    let start = clause_start(text, claim_start);
    let end = text[claim_end..]
        .char_indices()
        .find(|(_, ch)| matches!(ch, '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n'))
        .map(|(index, ch)| claim_end + index + ch.len_utf8())
        .unwrap_or(text.len());
    (start, end)
}

fn local_clauses(text: &str) -> Vec<(usize, usize)> {
    let mut clauses = Vec::new();
    let mut start = 0usize;
    for (index, ch) in text.char_indices() {
        if matches!(ch, '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n') {
            let end = index + ch.len_utf8();
            if !text[start..end].trim().is_empty() {
                clauses.push((start, end));
            }
            start = end;
        }
    }
    if start < text.len() && !text[start..].trim().is_empty() {
        clauses.push((start, text.len()));
    }
    clauses
}

fn label_before(text: &str, number_start: usize) -> Option<String> {
    let prefix = &text[..number_start];
    let mut label = prefix
        .chars()
        .rev()
        .take_while(|ch| !matches!(ch, '。' | '，' | ',' | '；' | ';' | '：' | ':' | '\n' | '|' | '（' | '('))
        .take(16)
        .collect::<Vec<_>>();
    label.reverse();
    let value = label
        .into_iter()
        .collect::<String>()
        .trim()
        .trim_end_matches(['为', '是', '约', '达', '共', '=', '：', ':'])
        .trim()
        .to_string();
    (!value.is_empty()).then_some(value)
}

fn extract_qualitative_claims(text: &str) -> Vec<QualitativeClaim> {
    const CUES: [(&str, QualitativeKind); 11] = [
        ("最高", QualitativeKind::Highest),
        ("最低", QualitativeKind::Lowest),
        ("增长", QualitativeKind::Growth),
        ("下降", QualitativeKind::Decline),
        ("领先", QualitativeKind::Lead),
        ("落后", QualitativeKind::Lag),
        ("支持", QualitativeKind::Support),
        ("禁止", QualitativeKind::Forbid),
        ("必须", QualitativeKind::Require),
        ("可以", QualitativeKind::Permit),
        ("不可", QualitativeKind::Disallow),
    ];
    let mut out = CUES
        .into_iter()
        .flat_map(|(raw, kind)| {
            text.match_indices(raw).map(move |(start, _)| QualitativeClaim {
                kind,
                raw,
                start,
                end: start + raw.len(),
            })
        })
        .collect::<Vec<_>>();
    out.sort_by_key(|claim| claim.start);
    out
}

fn fact_qualitative_kinds(fact: &VerifiedFact) -> Vec<QualitativeKind> {
    extract_qualitative_claims(&format!("{} {}", fact.metric, fact.value))
        .into_iter()
        .map(|claim| claim.kind)
        .collect()
}

fn extract_chinese_number_claims(text: &str) -> Vec<ChineseNumberClaim> {
    const DIGITS: &str = "零〇一二两三四五六七八九十百千";
    const UNITS: [&str; 18] = [
        "亿元", "万元", "个月", "百分比", "百分点", "年", "月", "日", "天", "倍", "成",
        "元", "个", "单", "家", "件", "台", "人",
    ];
    let chars = text.char_indices().collect::<Vec<_>>();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if !DIGITS.contains(chars[i].1) {
            i += 1;
            continue;
        }
        let start_index = i;
        while i < chars.len() && DIGITS.contains(chars[i].1) {
            i += 1;
        }
        if i == start_index {
            i += 1;
            continue;
        }
        let start = chars[start_index].0;
        let digit_end = chars.get(i).map(|(byte, _)| *byte).unwrap_or(text.len());
        let suffix = &text[digit_end..];
        let Some(unit) = UNITS
            .iter()
            .filter(|unit| suffix.starts_with(**unit))
            .max_by_key(|unit| unit.len())
        else {
            continue;
        };
        let end = digit_end + unit.len();
        if !chinese_number_context(text, start, end, unit) {
            continue;
        }
        out.push(ChineseNumberClaim {
            raw: text[start..end].to_string(),
        });
    }
    out
}

fn chinese_number_context(text: &str, start: usize, end: usize, unit: &str) -> bool {
    let previous = text[..start].chars().next_back();
    let suffix = &text[end..];
    let next = suffix.chars().next();
    let previous_boundary = previous.is_none_or(|ch| {
        !is_han(ch) || matches!(ch, '为' | '是' | '共' | '约' | '达' | '第' | '至' | '需' | '满' | '超' | '逾')
    });
    let next_boundary = next.is_none_or(|ch| !is_han(ch))
        || suffix.starts_with(['的', '内', '外', '起', '止', '期']);
    if next_boundary {
        return true;
    }
    if !previous_boundary {
        return false;
    }
    unit.contains('年')
        || unit.contains('月')
        || unit.contains('日')
        || unit.contains('天')
        || unit.contains('元')
        || ["客户", "门店", "商户", "订单", "商品", "设备", "人员", "员工", "报告", "文件"]
            .iter()
            .any(|noun| suffix.starts_with(noun))
}

fn is_han(ch: char) -> bool {
    matches!(ch, '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}')
}

fn unique_subjects(facts: &[VerifiedFact]) -> Vec<String> {
    let mut labels = facts
        .iter()
        .flat_map(|fact| fact.subjects.iter().cloned().chain(std::iter::once(fact.source.clone())))
        .collect::<Vec<_>>();
    labels.sort_by_key(|label| std::cmp::Reverse(label.chars().count()));
    labels.dedup();
    labels
}

fn unique_metrics(facts: &[VerifiedFact]) -> Vec<String> {
    let mut labels = facts.iter().map(|fact| fact.metric.clone()).collect::<Vec<_>>();
    labels.sort_by_key(|label| std::cmp::Reverse(label.chars().count()));
    labels.dedup();
    labels
}

fn mentioned(segment: &str, labels: &[String]) -> Vec<String> {
    let mut matched = labels
        .iter()
        .filter(|label| aliases(label).iter().any(|alias| alias.chars().count() >= 2 && segment.contains(alias)))
        .cloned()
        .collect::<Vec<_>>();
    let all = matched.clone();
    matched.retain(|label| {
        !all.iter().any(|longer| {
            longer != label
                && longer.chars().count() > label.chars().count()
                && segment.contains(longer)
                && longer.contains(label)
        })
    });
    matched
}

fn aliases(label: &str) -> Vec<String> {
    let normalized = label.trim().to_string();
    let mut out = vec![normalized.clone()];
    for suffix in [
        "维吾尔自治区", "壮族自治区", "回族自治区", "自治区", "特别行政区", "省区", "战区",
        "大区", "销售额", "成交额", "金额", "客户数", "订单数", "门店数", "商品数", "数量",
        "占比", "比例", "省", "市", "数", "额", "率",
        "量",
    ] {
        if let Some(short) = normalized.strip_suffix(suffix) {
            if short.chars().count() >= 2 {
                out.push(short.to_string());
            }
        }
    }
    out
}

fn scope_compatible(left: &str, right: &str) -> bool {
    aliases(left).iter().any(|a| {
        aliases(right).iter().any(|b| {
            let a = a.to_ascii_lowercase();
            let b = b.to_ascii_lowercase();
            a == b || (a.chars().count() >= 2 && b.chars().count() >= 2 && (a.contains(&b) || b.contains(&a)))
        })
    })
}

fn subjects_match(fact: &VerifiedFact, segment: &str, mentioned_subjects: &[String]) -> bool {
    let no_conflict = mentioned_subjects.iter().all(|mentioned| {
        scope_compatible(&fact.source, mentioned)
            || scope_compatible(&fact.metric, mentioned)
            || fact.subjects.iter().any(|subject| scope_compatible(subject, mentioned))
    });
    if !no_conflict {
        return false;
    }
    if !fact.subjects.is_empty() {
        return fact
            .subjects
            .iter()
            .all(|subject| aliases(subject).iter().any(|alias| alias.chars().count() >= 2 && segment.contains(alias)));
    }
    // 复合子问的标量结果没有主体列，必须把子问 source 写进结论，防止 Q01/Q02
    // 恰好同值时互相借事实。单问 Q 与 Hybrid 的 DATA/KB 另有各自作用域纪律。
    !fact.namespace.starts_with('Q')
        || fact.namespace == "Q"
        || aliases(&fact.source)
            .iter()
            .any(|alias| alias.chars().count() >= 2 && segment.contains(alias))
}

fn date_subject_matches(fact: &VerifiedFact, segment: &str, claims: &[NumericClaim]) -> bool {
    let dates = claims
        .iter()
        .filter(|claim| claim.unit == NumberUnit::Date)
        .collect::<Vec<_>>();
    if dates.is_empty() {
        return false;
    }
    fact.subjects.iter().any(|subject| {
        segment.contains(subject)
            && dates.iter().all(|claim| {
                extract_numeric_claims(subject)
                    .iter()
                    .any(|number| number.unit == NumberUnit::Date && numbers_match(claim, number))
            })
    })
}

fn metric_matches(
    fact: &VerifiedFact,
    segment: &str,
    mentioned_metrics: &[String],
    claim: &NumericClaim,
    previous_metric: Option<&str>,
) -> bool {
    if !mentioned_metrics.iter().all(|metric| scope_compatible(&fact.metric, metric)) {
        return false;
    }
    let names_fact_metric = aliases(&fact.metric)
        .iter()
        .any(|alias| alias.chars().count() >= 2 && segment.contains(alias));
    // 无论知识还是数据事实，指标都必须正向出现，不能靠 namespace 或“没识别到其它指标词”
    // 放行。省略指标只允许两种可证明形态：
    // 计数单位本身唯一指向该指标；或紧邻上一条同指标，且本句除主体/数值/语气词外无新词。
    names_fact_metric
        || count_unit_proves_metric(claim.unit, &fact.metric)
        || previous_metric.is_some_and(|metric| {
            scope_compatible(metric, &fact.metric) && only_scope_and_number(segment, fact)
        })
}

fn count_unit_proves_metric(unit: NumberUnit, metric: &str) -> bool {
    match unit {
        NumberUnit::Count('单') => metric.contains("订单") || metric.contains("单量"),
        NumberUnit::Count('家') => metric.contains("客户") || metric.contains("门店") || metric.contains("商户"),
        NumberUnit::Count('户') => metric.contains("客户") || metric.contains("商户"),
        NumberUnit::Count('件' | '个' | '条' | '台' | '份' | '笔' | '款') => {
            metric.contains("数量") || metric.contains("销量") || metric.contains("库存")
        }
        NumberUnit::Count('人' | '位' | '名') => metric.contains("人数") || metric.contains("员工"),
        _ => false,
    }
}

fn only_scope_and_number(segment: &str, fact: &VerifiedFact) -> bool {
    let mut rest = segment.to_string();
    let mut removable = fact
        .subjects
        .iter()
        .chain(std::iter::once(&fact.source))
        .flat_map(|label| aliases(label))
        .filter(|label| label.chars().count() >= 2)
        .collect::<Vec<_>>();
    removable.sort_by_key(|label| std::cmp::Reverse(label.len()));
    for label in removable {
        rest = rest.replace(&label, "");
    }
    for word in ["分别", "其中", "当前", "本期", "合计", "总计", "共有", "形成", "为", "是", "约", "达", "共"] {
        rest = rest.replace(word, "");
    }
    rest.chars().all(|ch| {
        ch.is_ascii_digit()
            || ch.is_whitespace()
            || matches!(
                ch,
                ',' | '.' | '+' | '-' | '−' | '%' | '％' | '¥' | '￥' | '$' | '万' | '亿'
                    | '元' | '块' | '个' | '单' | '家' | '件' | '条' | '次' | '位' | '名'
                    | '行' | '页' | '台' | '份' | '笔' | '户' | '款' | '人' | '天' | '年'
                    | '月' | '，' | '。' | '；' | '：' | '(' | ')' | '（' | '）'
            )
    })
}

fn scope_key(fact: &VerifiedFact) -> String {
    format!("{}|{}|{}", fact.namespace, fact.subjects.join("/"), fact.metric)
}

fn numbers_match(claim: &NumericClaim, evidence: &NumericClaim) -> bool {
    match (claim.unit, evidence.unit) {
        // 百分比字段在真实库中既有 0.256（比值）也有 25.6（百分点）两种存法；
        // 仅这一个单位允许两档对账。金额的万/亿绝不保留 raw 同值兜底。
        (NumberUnit::Percent, NumberUnit::Percent) => close(claim.value, evidence.value),
        (NumberUnit::Percent, NumberUnit::Plain) => {
            close(claim.value / 100.0, evidence.value) || close(claim.value, evidence.value)
        }
        (NumberUnit::Plain, NumberUnit::Percent) => {
            close(claim.value, evidence.value / 100.0) || close(claim.value, evidence.value)
        }
        (NumberUnit::Count(left), NumberUnit::Count(right)) if left != right => false,
        (NumberUnit::Percent, NumberUnit::Count(_)) | (NumberUnit::Count(_), NumberUnit::Percent) => false,
        (NumberUnit::Multiplier(left), NumberUnit::Multiplier(right)) => {
            left == right && close(claim.value, evidence.value)
        }
        (NumberUnit::Date, NumberUnit::Date) => close(claim.value, evidence.value),
        (NumberUnit::Multiplier(_), _) | (_, NumberUnit::Multiplier(_)) => false,
        (NumberUnit::Date, _) | (_, NumberUnit::Date) => false,
        _ => close(base_value(claim), base_value(evidence)),
    }
}

fn base_value(number: &NumericClaim) -> f64 {
    match number.unit {
        NumberUnit::Wan => number.value * 10_000.0,
        NumberUnit::Yi => number.value * 100_000_000.0,
        NumberUnit::Plain
        | NumberUnit::Count(_)
        | NumberUnit::Percent
        | NumberUnit::Multiplier(_)
        | NumberUnit::Date => number.value,
    }
}

fn close(left: f64, right: f64) -> bool {
    let floating_noise = f64::EPSILON * left.abs().max(right.abs()).max(1.0) * 16.0;
    (left - right).abs() <= floating_noise
}

fn cell_numbers(value: &Value) -> Vec<NumericClaim> {
    match value {
        Value::Number(number) => number
            .as_f64()
            .map(|value| {
                vec![NumericClaim {
                    value,
                    unit: NumberUnit::Plain,
                    raw: number.to_string(),
                    start: 0,
                    end: number.to_string().len(),
                }]
            })
            .unwrap_or_default(),
        Value::String(text) if numeric_cell(text) => extract_numeric_claims(text),
        _ => Vec::new(),
    }
}

fn numeric_cell(text: &str) -> bool {
    text.chars().any(|ch| ch.is_ascii_digit())
        && text.chars().all(|ch| {
            ch.is_ascii_digit()
                || ch.is_whitespace()
                || matches!(
                    ch,
                    ',' | '.' | '+' | '-' | '−' | '%' | '％' | '¥' | '￥' | '$' | '万' | '亿'
                        | '元' | '块' | '个' | '单' | '家' | '件' | '条' | '次' | '位' | '名'
                        | '行' | '页' | '台' | '份' | '笔' | '户' | '款' | '人'
                )
        })
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 抽所有可核验事实值（含小整数、日期、倍数、金额/比例/计数单位）；只排除列表序号、引用 ID
/// 和紧跟 ASCII 字母的型号数字。调用前引用 ID 会被 mask，因此 `F001` 不会成为断言。
fn extract_numeric_claims(text: &str) -> Vec<NumericClaim> {
    let chars = text.char_indices().collect::<Vec<_>>();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let (_, ch) = chars[i];
        let signed = matches!(ch, '+' | '-' | '−')
            && chars.get(i + 1).is_some_and(|(_, next)| next.is_ascii_digit())
            && i.checked_sub(1)
                .and_then(|index| chars.get(index))
                .is_none_or(|(_, prev)| !prev.is_ascii_alphanumeric());
        if !ch.is_ascii_digit() && !signed {
            i += 1;
            continue;
        }
        let start_index = i;
        if signed {
            i += 1;
        }
        let digit_start = i;
        let mut seen_dot = false;
        while i < chars.len() {
            let current = chars[i].1;
            if current.is_ascii_digit() || current == ',' {
                i += 1;
            } else if current == '.'
                && !seen_dot
                && chars.get(i + 1).is_some_and(|(_, next)| next.is_ascii_digit())
            {
                seen_dot = true;
                i += 1;
            } else {
                break;
            }
        }
        if digit_start == i {
            i += 1;
            continue;
        }
        let start = chars[start_index].0;
        let end = chars.get(i).map(|(byte, _)| *byte).unwrap_or(text.len());
        let raw = text[start..end].to_string();
        let next = chars.get(i).map(|(_, value)| *value);
        let prev = start_index.checked_sub(1).map(|index| chars[index].1);
        let unit_index = if next.is_some_and(char::is_whitespace) { i + 1 } else { i };
        let unit_char = chars.get(unit_index).map(|(_, value)| *value);

        let year_literal = next == Some('年')
            && raw.trim_start_matches(['+', '-', '−']).replace(',', "").len() == 4;
        let date_like = matches!(next, Some('-' | ':' | '/' | '月' | '日' | '时' | '分'))
            || matches!(prev, Some('-' | ':' | '/'))
            || year_literal;
        let list_marker = matches!(next, Some('.' | '、' | ')' | '）'));
        let model_suffix = next.is_some_and(|value| value.is_ascii_alphabetic())
            || prev.is_some_and(|value| value.is_ascii_alphabetic());
        if list_marker || model_suffix {
            continue;
        }
        let normalized = raw.replace(',', "").replace('−', "-");
        let Ok(value) = normalized.parse::<f64>() else {
            continue;
        };
        let unit = if date_like || matches!(prev, Some('第')) {
            NumberUnit::Date
        } else {
            match unit_char {
                Some('万') => NumberUnit::Wan,
                Some('亿') => NumberUnit::Yi,
                Some('%' | '％') => NumberUnit::Percent,
                Some(value @ ('倍' | '成')) => NumberUnit::Multiplier(value),
                Some('个')
                    if chars.get(unit_index + 1).is_some_and(|(_, value)| *value == '月') =>
                {
                    NumberUnit::Count('月')
                }
                Some(value @ ('元' | '块' | '个' | '单' | '家' | '件' | '条' | '次' | '位' | '名'
                    | '行' | '页' | '台' | '份' | '笔' | '户' | '款' | '人' | '天' | '年')) => NumberUnit::Count(value),
                _ => NumberUnit::Plain,
            }
        };
        out.push(NumericClaim { value, unit, raw, start, end });
    }
    out
}

fn clip(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let head = chars.by_ref().take(max).collect::<String>();
    if chars.next().is_some() { head + "…" } else { head }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn province_contract() -> AnswerContract {
        let mut contract = AnswerContract::new();
        contract.push_table(
            "Q",
            "各省销售额",
            &["省份".into(), "销售额".into()],
            &[
                vec![Value::from("山东"), Value::from(1_000_000)],
                vec![Value::from("江苏"), Value::from(2_000_000)],
            ],
            5,
        );
        contract
    }

    #[test]
    fn province_values_cannot_be_swapped_between_facts() {
        let contract = province_contract();
        assert!(contract.validate("山东销售额为100万元[Q:F001]，江苏为200万元[Q:F002]。").is_ok());
        assert!(
            contract.validate("山东销售额为200万元[Q:F002]，江苏为100万元[Q:F001]。").is_err(),
            "数字存在于整份素材也不够，紧邻事实还必须同时匹配主体"
        );
    }

    #[test]
    fn unknown_subject_or_metric_cannot_hide_behind_a_known_fact() {
        let contract = province_contract();
        assert!(
            contract.validate("河北销售额为100万元[Q:F001]。").is_err(),
            "证据只写山东时，凭空换成河北不能因河北不在 fact labels 中而通过"
        );
        assert!(
            contract.validate("山东毛利额为100万元[Q:F001]。").is_err(),
            "销售额 fact 不能因毛利额不在 fact metrics 中而被借用"
        );
        assert!(contract.validate("山东销售额为100万元[Q:F001]。").is_ok());
    }

    #[test]
    fn every_dimension_subject_must_match_the_cited_row() {
        let mut contract = AnswerContract::new();
        contract.push_table(
            "Q",
            "省份商品销售额",
            &["省份".into(), "商品".into(), "销售额".into()],
            &[vec![Value::from("山东"), Value::from("苹果"), Value::from(1_000_000)]],
            5,
        );
        assert!(contract.validate("山东苹果销售额为100万元[Q:F001]。").is_ok());
        assert!(
            contract.validate("山东苹果税额为100万元[Q:F001]。").is_err(),
            "即使所有维度都正确，新指标也不能借销售额 fact"
        );
        assert!(
            contract.validate("河北苹果销售额为100万元[Q:F001]。").is_err(),
            "命中同一行的苹果不能掩盖省份已被换成河北"
        );
        assert!(
            contract.validate("山东税额为100万元[Q:F001]。").is_err(),
            "任何新指标都不能借销售额 fact"
        );
    }

    #[test]
    fn model_number_in_entity_name_is_not_stock_evidence() {
        let mut contract = AnswerContract::new();
        contract.push_table(
            "Q",
            "库存查询",
            &["商品".into(), "库存量".into()],
            &[vec![Value::from("小虎黑椒味烤肠500G"), Value::from(20)]],
            5,
        );
        assert!(contract.validate("小虎黑椒味烤肠500G库存为20件[Q:F001]。").is_ok());
        assert!(
            contract.validate("小虎黑椒味烤肠500G库存为500件[Q:F001]。").is_err(),
            "型号 500G 是主体文本，不能证明库存是 500"
        );
    }

    #[test]
    fn small_counts_and_signed_percentages_are_verified() {
        let mut contract = AnswerContract::new();
        contract.push_table(
            "Q",
            "经营概览",
            &["客户数".into(), "订单数".into(), "增长率".into()],
            &[vec![Value::from(3), Value::from(2), Value::from(-0.05)]],
            5,
        );
        assert!(contract.validate("共有3家客户[Q:F001]，形成2单[Q:F002]，增长率为-5%[Q:F003]。").is_ok());
        assert!(contract.validate("共有3家客户[Q:F002]。").is_err(), "3 家不能借 2 单的事实");
        assert!(contract.validate("共有3家客户。").is_err(), "小整数同样必须有引用");

        let mut equal_counts = AnswerContract::new();
        equal_counts.push_table(
            "Q",
            "经营概览",
            &["客户数".into(), "订单数".into()],
            &[vec![Value::from(3), Value::from(3)]],
            5,
        );
        assert!(
            equal_counts.validate("共有3家客户[Q:F002]。").is_err(),
            "客户数与订单数即使碰巧相同，也必须由指标匹配的事实支撑"
        );
    }

    #[test]
    fn amount_units_are_normalized_without_raw_value_fallback() {
        let mut good = AnswerContract::new();
        good.push_table("Q", "销售额", &["销售额".into()], &[vec![Value::from(1_000_000)]], 5);
        assert!(good.validate("销售额为100万元[Q:F001]。").is_ok());
        assert!(
            good.validate("销售额为99万元[Q:F001]。").is_err(),
            "证据 100 万元不能使用相对容差支持 99 万元"
        );

        let mut wrong_scale = AnswerContract::new();
        wrong_scale.push_table("Q", "销售额", &["销售额".into()], &[vec![Value::from(100)]], 5);
        assert!(
            wrong_scale.validate("销售额为100万元[Q:F001]。").is_err(),
            "证据 100 元不能因 raw 数相同而支持 100 万元"
        );

        let mut percentage = AnswerContract::new();
        percentage.push_table("Q", "增长率", &["增长率".into()], &[vec![Value::from(0.256)]], 5);
        assert!(percentage.validate("增长率为25.6%[Q:F001]。").is_ok());
    }

    #[test]
    fn compound_namespaces_do_not_lend_values_to_each_other() {
        let mut contract = AnswerContract::new();
        contract.push_table(
            "Q01",
            "山东销售额",
            &["省份".into(), "销售额".into()],
            &[vec![Value::from("山东"), Value::from(1_000_000)]],
            5,
        );
        contract.push_table(
            "Q02",
            "江苏销售额",
            &["省份".into(), "销售额".into()],
            &[vec![Value::from("江苏"), Value::from(1_000_000)]],
            5,
        );
        assert!(contract.validate("山东销售额为100万元[Q01:F001]。").is_ok());
        assert!(
            contract.validate("山东销售额为100万元[Q02:F001]。").is_err(),
            "即便数值相同，另一个子问的 namespace/主体也不能借用"
        );
    }

    #[test]
    fn hybrid_source_words_pin_the_namespace() {
        let mut contract = AnswerContract::new();
        contract.push_table("DATA", "取数结果", &["销售额".into()], &[vec![Value::from(100)]], 5);
        contract.push_text("KB", "知识库资料", "销售额为100元。");
        assert!(contract.validate("取数结果显示销售额为100元[DATA:F001]。").is_ok());
        assert!(contract.validate("知识库资料规定销售额为100元[KB:F001]。").is_ok());
        assert!(
            contract.validate("取数结果显示销售额为100元[KB:F001]。").is_err(),
            "显式说取数/数据时不能借 KB namespace"
        );
        assert!(
            contract.validate("知识库资料规定销售额为100元[DATA:F001]。").is_err(),
            "显式说资料/规定时不能借 DATA namespace"
        );
    }

    #[test]
    fn knowledge_duration_units_cannot_be_exchanged() {
        let mut contract = AnswerContract::new();
        contract.push_text("KB", "保修规定", "整机保修期为1年，易损件保修期为3个月。");
        assert!(contract.validate("整机保修期为1年[KB:F001]。").is_ok());
        assert!(contract.validate("易损件保修期为3个月[KB:F002]。").is_ok());
        assert!(
            contract.validate("易损件保修期为1年[KB:F001]。").is_err(),
            "KB 同值也必须正向绑定整机/易损件主题，不能只因 namespace 相同而借事实"
        );
        assert!(
            contract.validate("整机保修期为3个月[KB:F002]。").is_err(),
            "KB 的另一个局部主题不能反向借值"
        );
        assert!(
            contract.validate("整机保修期为1个月[KB:F001]。").is_err(),
            "数值相同但年/月单位不同，不能通过"
        );
    }

    #[test]
    fn dates_and_multipliers_require_exact_scoped_evidence() {
        let mut date = AnswerContract::new();
        date.push_table(
            "Q",
            "经营日期",
            &["统计日期".into(), "销售额".into()],
            &[vec![Value::from("2026-08-13"), Value::from(100)]],
            5,
        );
        assert!(date.validate("2026-08-13销售额为100元[Q:F001]。").is_ok());
        assert!(
            date.validate("2026-08-12销售额为100元[Q:F001]。").is_err(),
            "错日期不能因销售额正确而通过"
        );
        assert!(
            date.validate("2026-08-13销售额为100元。").is_err(),
            "正确日期和数值也都需要紧邻事实引用"
        );

        let mut multiple = AnswerContract::new();
        multiple.push_text("KB", "返利规定", "会员返利为2倍。");
        assert!(multiple.validate("会员返利为2倍[KB:F001]。").is_ok());
        assert!(multiple.validate("会员返利为3倍[KB:F001]。").is_err());
        assert!(multiple.validate("会员返利为2倍。").is_err());
    }

    #[test]
    fn chinese_numbers_are_rejected_until_they_can_be_exactly_parsed() {
        let mut contract = AnswerContract::new();
        contract.push_table("Q", "销售额", &["销售额".into()], &[vec![Value::from(1_000_000)]], 5);
        assert!(contract.validate("销售额为一百万元[Q:F001]。").is_err());

        let mut duration = AnswerContract::new();
        duration.push_text("KB", "保修规定", "整机保修期为1年，易损件保修期为3个月。");
        assert!(duration.validate("整机保修期为一年[KB:F001]。").is_err());
        assert!(duration.validate("易损件保修期为三个月[KB:F002]。").is_err());
        assert!(
            duration.validate("这是一个可以核验的结论。").is_err(),
            "普通汉字不应被误判为中文数字，但‘可以’属于受控定性业务断言"
        );
        assert!(
            duration.validate("这是一个清晰结论。").is_ok(),
            "没有数字或受控业务断言的普通中文仍可展示"
        );
    }

    #[test]
    fn qualitative_business_assertions_need_matching_typed_evidence() {
        let mut trend = AnswerContract::new();
        trend.push_text("KB", "经营结论", "山东销售额增长，江苏销售额下降。");
        assert!(trend.validate("山东销售额增长[KB:F001]。").is_ok());
        assert!(trend.validate("山东销售额下降[KB:F001]。").is_err());
        assert!(trend.validate("山东销售额最高[KB:F001]。").is_err());
        assert!(trend.validate("山东销售额增长。").is_err());

        let mut policy = AnswerContract::new();
        policy.push_text("KB", "设备政策", "整机必须登记，易损件不可退换。");
        assert!(policy.validate("整机必须登记[KB:F001]。").is_ok());
        assert!(policy.validate("易损件不可退换[KB:F002]。").is_ok());
        assert!(policy.validate("整机可以不登记[KB:F001]。").is_err());
        assert!(policy.validate("易损件支持退换[KB:F002]。").is_err());
        assert!(policy.validate("整机必须登记。").is_err());
    }

    #[test]
    fn subquestion_source_name_pins_equal_metrics() {
        let mut contract = AnswerContract::new();
        contract.push_table(
            "Q01",
            "山东销售额",
            &["销售额".into()],
            &[vec![Value::from(1_000_000)]],
            5,
        );
        contract.push_table(
            "Q02",
            "江苏销售额",
            &["销售额".into()],
            &[vec![Value::from(1_000_000)]],
            5,
        );
        assert!(contract.validate("山东销售额为100万元[Q01:F001]。").is_ok());
        assert!(
            contract.validate("山东销售额为100万元[Q02:F001]。").is_err(),
            "标量子结果没有主体列时，也要用子问 source 锁住作用域"
        );
    }

    #[test]
    fn successful_validation_removes_internal_references() {
        let contract = province_contract();
        assert_eq!(
            contract.validate("山东销售额为100万元[Q:F001]。").unwrap(),
            "山东销售额为100万元。"
        );
    }

}
