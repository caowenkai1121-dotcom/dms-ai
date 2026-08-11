//! 文档级知识图谱构建编排（Yuxi B6 节的 dms-ai 落法）。变更原因＝抽取/归并/构建流程。
//!
//! 三段职责切分：
//! - **本文件**：固定 JSON schema 的抽取 prompt、容错解析（json_repair 思路：剥围栏 →
//!   定位首个 `{` 到末个 `}` → 直接解析 → 去尾逗号再解析）、实体归并
//!   （规范名 → 确定性 id；label 多数决 + 噪声过滤，KB 审查⑤）、并发 4 + 指数退避重试 2 次的构建流水；
//! - **Cypher 拼接唯一收口**在 `dms_connector::doc_graph`（AGE 图名 `kb_graph`）；
//! - **构建状态表 `meta.kb_graph_build`** 在 server 侧（knowledge 不碰 `meta.*`），
//!   本文件只经 `BuildProgress` 回报口把进度推上去。
//!
//! ACL 先行：构建取 chunk 与查询取可见文档都走 `visible_docs!()` 内联子查询
//! （`$1` login / `$2` 角色码），不做查完再过滤；撤权即不可见。

use crate::{KbError, Viewer};
use dms_connector::doc_graph::{self, ChunkGraph, GraphEntity, GraphRelation};
use dms_connector::owned::OwnedStore;
use dms_kernel::{ChatModel, ChatRequest, ModelTier};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// 抽取并发（Yuxi `concurrency_count` 的固定值形态：4）。
pub const BUILD_CONCURRENCY: usize = 4;
/// 每个 chunk 的失败重试次数（指数退避，共 RETRY_MAX+1 次尝试；
/// 语义由测试 `retry_is_exponential_and_bounded` 钉住，改它先改测试）。
pub const RETRY_MAX: u32 = 2;
/// 退避基数：400ms → 800ms。
pub const RETRY_BASE: std::time::Duration = std::time::Duration::from_millis(400);
/// 单次构建的 chunk 上限（成本闸；超出部分等下一轮 —— 图谱是增量可重建的）。
pub const MAX_BUILD_CHUNKS: i64 = 2000;
/// 进 prompt 的正文截断（字符）：防个别超长块把单次调用打爆。
const MAX_CHUNK_CHARS: usize = 4000;
/// status 契约里的失败样本上限（前 5 条）。
pub const MAX_FAILED_SAMPLES: usize = 5;
/// 单 chunk 抽取结果上限：防模型失控输出撑爆图写批。
/// 真实上界注意：关系端点自动补登（`to_chunk_graph`）可再塞进最多 2×50 个实体，
/// 图写批的实际峰值是 150 实体 + 50 关系。
const MAX_ITEMS_PER_CHUNK: usize = 50;
const MAX_NAME_CHARS: usize = 100;
const MAX_LABEL_CHARS: usize = 30;
/// 失败样本的错误文案截断长度（字符）。
const MAX_SAMPLE_ERR_CHARS: usize = 300;

/// 固定 JSON schema 的抽取 prompt（Yuxi `DEFAULT_TRIPLE_EXTRACTION_PROMPT` 同构）。
/// 不接受自定义 prompt：图谱质量依赖输出形状稳定，开放 prompt 等于开放 schema 漂移。
///
/// KB 审查⑤的硬约束（垃圾实体两族）：单号/日期/金额**实例值**不进图（一张单据一个节点，
/// 图很快长满一次性叶子）；「制度」「规定」这类单独泛词不进图（没有辨识度，全是枢纽）。
/// prompt 是第一道，`is_noise_entity` 是兜底的确定性闸 —— 模型不听话时图也不收。
/// ⚠️ 本 prompt 与归并口径的改动**必须重建图谱才生效**（`build_space` 先清该空间旧图再全量写）。
pub const EXTRACTION_SYSTEM: &str = "请从给定文本中抽取实体和实体关系，返回严格 JSON，不要输出任何解释。\n\
JSON 格式：\n\
{\n\
  \"entities\": [{\"text\": \"实体文本\", \"label\": \"实体类型\"}],\n\
  \"relations\": [{\"source\": {\"text\": \"实体文本\", \"label\": \"实体类型\"}, \
\"target\": {\"text\": \"实体文本\", \"label\": \"实体类型\"}, \"label\": \"关系类型\"}]\n\
}\n\
要求：\n\
- 实体文本必须是原文中出现的片段；实体类型用简短名词（如 人物、组织、产品、制度、地点、概念）。\n\
- 实体只抽有结构价值的业务对象：不抽单号/编号、日期、金额这类实例值\
（如「PO-2024-001」「2025年3月」「5000元」），也不抽「制度」「规定」「公司」这类单独出现的泛词。\n\
- 关系类型只许从受控词表选择：隶属于 / 规定了 / 适用于 / 负责 / 审批 / 包含 / 引用 / 相关；\
都不合适就用「相关」。source 与 target 必须是 entities 里的实体。\n\
- 只抽取文本明确陈述的事实，不推测；没有可抽取内容时返回 {\"entities\": [], \"relations\": []}。";

/// 一次抽取的原始产物（未归并；字段已按上限截断）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Extraction {
    pub entities: Vec<RawEntity>,
    pub relations: Vec<RawRelation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntity {
    pub text: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRelation {
    pub source: RawEntity,
    pub target: RawEntity,
    pub label: String,
}

/// 待抽取的 chunk（构建输入的最小单位）。
#[derive(Debug, Clone)]
pub struct ChunkToExtract {
    pub chunk_id: i64,
    pub doc_id: String,
    pub ord: i32,
    pub text: String,
}

/// 失败样本（status 端点的 `failed_samples`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FailedSample {
    pub doc_id: String,
    pub chunk_id: i64,
    pub error: String,
}

/// 一次构建的终态。
#[derive(Debug, Default, Clone)]
pub struct BuildOutcome {
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub failed_samples: Vec<FailedSample>,
}

/// 构建进度回报口。server 侧实现它把进度落 `meta.kb_graph_build`；
/// knowledge 只认这个 trait（本 crate 不碰 `meta.*` 的纪律不变）。
pub trait BuildProgress: Send + Sync {
    fn report<'a>(
        &'a self,
        total: usize,
        done: usize,
        failed: usize,
        samples: &'a [FailedSample],
    ) -> dms_kernel::BoxFut<'a, ()>;
}

/// Yuxi `normalize_entity_name` 同款：压缩内部连续空白 + 小写化。
/// 归并键用它而不是原文 —— 「差旅  报销」与「差旅 报销」必须是同一个实体。
pub fn normalize_name(s: &str) -> String {
    // fold 直拼省一次中间 Vec；整串 `to_lowercase`（不逐 char）是刻意的——
    // Unicode 语境规则（如词尾 Σ→ς）只有整串版才正确
    let joined = s.split_whitespace().fold(String::with_capacity(s.len()), |mut acc, w| {
        if !acc.is_empty() {
            acc.push(' ');
        }
        acc.push_str(w);
        acc
    });
    joined.to_lowercase()
}

/// 确定性实体 id = hash(space:规范名)（KB 审查⑤：label 退出归并键 —— 「差旅报销制度·制度」与
/// 「差旅报销制度·概念」两个节点各挂一半 MENTIONS，是实测抓到的实体碎裂根因）。
/// 同 (space, 规范名) 恒同 id → 跨 chunk/跨文档的同名实体在 MERGE 时天然归并。
///
/// 散列用 std 的 `DefaultHasher`（固定密钥，同二进制内稳定）—— 零新增依赖（D6）下
/// 没有 sha2/md5 可用。id 只需在一次部署内自洽：重建本就先清空再全量，不跨版本比对。
/// ⚠️ id 口径变了（旧 id 含 label）：**必须重建图谱才生效**，新旧 id 永不匹配。
pub fn entity_id(space_id: &str, normalized_name: &str) -> String {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // 与基线 `format!("{space}:{name}").hash(&mut h)` 逐位等价（`str::hash` = 写字节 + 0xFF），
    // 手写省一次临时 String 分配；等价性由测试 `entity_id_matches_format_baseline` 钉死
    h.write(space_id.as_bytes());
    h.write(b":");
    h.write(normalized_name.as_bytes());
    h.write_u8(0xff);
    format!("e_{:016x}", h.finish())
}

/// 泛词 stoplist（KB 审查⑤）：单独出现没有辨识度的词；组合词不受影响（「差旅报销制度」≠「制度」）。
const GENERIC_ENTITY_NAMES: &[&str] = &[
    "制度", "规定", "办法", "流程", "通知", "文件", "公司", "部门", "员工", "系统", "管理", "标准", "要求",
];

/// 噪声实体判定（`EXTRACTION_SYSTEM` 硬约束的确定性兜底）：
/// - stoplist 泛词（单独出现没有结构价值，全是枢纽节点）；
/// - 纯 ASCII 编号/单号：全 `[a-z0-9-]`、含数字、≥4 位（normalize_name 已小写化）；
/// - 日期/纯数字/金额：剥掉数字与日期/金额修饰字后什么都不剩。
fn is_noise_entity(name: &str) -> bool {
    if GENERIC_ENTITY_NAMES.contains(&name) {
        return true;
    }
    if name.len() >= 4
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && name.bytes().any(|b| b.is_ascii_digit())
    {
        return true;
    }
    if name.chars().all(|c| {
        c.is_ascii_digit()
            || matches!(
                c,
                '.' | '-' | '/' | ':' | ',' | '年' | '月' | '日' | '号' | '元' | '万' | '亿'
                    | '角' | '分' | '块' | '¥' | '￥' | '$' | '％' | '%'
            )
    }) {
        return true;
    }
    // 兜底族：纯符号名（「——」「…」——一个字母/数字/汉字都没有）同样没有结构价值。
    // `is_alphanumeric` 是 Unicode 口径，汉字（Lo）已涵盖
    !name.chars().any(char::is_alphanumeric)
}

/// 关系 label 受控词表（KB 审查⑤）：自由文本 label 让同一语义长出几十种写法
/// （「由发布」「发布了」「出自」），按 label 聚合的图查询随之失效。
/// 词表外（含空）一律归「相关」。
pub const RELATION_VOCAB: &[&str] =
    &["隶属于", "规定了", "适用于", "负责", "审批", "包含", "引用", "相关"];

/// 关系 label 归一到受控词表（`normalize_name` 先压空白小写化）。
fn normalize_relation(label: &str) -> String {
    let l = normalize_name(label);
    if RELATION_VOCAB.contains(&l.as_str()) { l } else { "相关".into() }
}

/// 构建取数 SQL：当前 viewer 可见 + enabled + 已入库（chunked/embedded）+ 生效期内的
/// 文档 chunk。ACL 片段内联（`$1`/`$2`），空间过滤 `$3`，成本闸 `$4`。
fn build_chunks_sql() -> &'static str {
    concat!(
        "SELECT c.chunk_id,c.doc_id,c.ord,c.text FROM kb.chunk c \
         JOIN kb.doc d ON d.doc_id=c.doc_id \
         WHERE d.space_id=$3 AND d.enabled=true AND d.status IN ('chunked','embedded') \
           AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE) \
           AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE) \
           AND d.doc_id IN (",
        crate::acl::visible_docs!(),
        ") ORDER BY c.doc_id,c.ord LIMIT $4"
    )
}

/// 图谱查询端点的可见文档集合（与检索同一份过滤口径，空间在这里是必填）。
fn visible_doc_ids_sql() -> &'static str {
    concat!(
        "SELECT x.doc_id FROM kb.doc x WHERE x.space_id=$3 AND x.enabled=true \
         AND x.status IN ('chunked','embedded') \
         AND (x.effective_from IS NULL OR x.effective_from<=CURRENT_DATE) \
         AND (x.effective_to IS NULL OR x.effective_to>=CURRENT_DATE) \
         AND x.doc_id IN (",
        crate::acl::visible_docs!(),
        ") ORDER BY x.doc_id"
    )
}

/// 构建侧的 chunk 清单（ACL 内联，见 `build_chunks_sql`）。
pub async fn chunks_for_build(
    store: &OwnedStore,
    v: &Viewer,
    space_id: &str,
) -> Result<Vec<ChunkToExtract>, KbError> {
    let rows = store
        .fixed(build_chunks_sql())
        .bind(&v.login)
        .bind(&v.roles)
        .bind(space_id)
        .bind(MAX_BUILD_CHUNKS)
        .fetch_all::<(i64, String, i32, String)>()
        .await?;
    Ok(rows
        .into_iter()
        .map(|(chunk_id, doc_id, ord, text)| ChunkToExtract { chunk_id, doc_id, ord, text })
        .collect())
}

/// 查询侧的可见文档 id 集合（subgraph/stats 的 ACL 过滤输入；撤权即不可见就靠它现算）。
pub async fn visible_doc_ids(
    store: &OwnedStore,
    v: &Viewer,
    space_id: &str,
) -> Result<Vec<String>, KbError> {
    Ok(store
        .fixed(visible_doc_ids_sql())
        .bind(&v.login)
        .bind(&v.roles)
        .bind(space_id)
        .fetch_all::<(String,)>()
        .await?
        .into_iter()
        .map(|(id,)| id)
        .collect())
}

/// 容错解析共用文案（两处判定同一失败形态，只许一份字面量）
const ERR_NO_FULL_JSON: &str = "抽取响应里没有完整 JSON 对象";

/// 抽取响应的容错解析（json_repair 思路的最小实现）：
/// 剥 markdown 围栏 → 取首个 `{` 到末个 `}` → 直接解析 → 失败则去尾逗号再解析。
/// 两步都失败才算这个 chunk 失败（进 failed_samples）；形状残缺的条目跳过不报错。
pub fn parse_extraction(raw: &str) -> Result<Extraction, String> {
    let stripped = strip_fence(raw.trim());
    let start = stripped.find('{').ok_or("抽取响应里没有 JSON 对象")?;
    let end = stripped.rfind('}').ok_or(ERR_NO_FULL_JSON)?;
    if end <= start {
        return Err(ERR_NO_FULL_JSON.into());
    }
    let candidate = &stripped[start..=end];
    let v: serde_json::Value = serde_json::from_str(candidate)
        .or_else(|e| {
            // 多数失败响应根本没有尾逗号——快查不过就不走修复分支（省一次全量分配）；
            // 快查宁可误报（多试一次，结果不变）不许漏报
            if may_have_trailing_comma(candidate) {
                serde_json::from_str(&drop_trailing_commas(candidate))
            } else {
                Err(e)
            }
        })
        .map_err(|e| format!("抽取响应 JSON 解析失败：{e}"))?;
    Ok(extraction_from_value(&v))
}

/// 快查：是否可能有尾逗号（`,` 后仅空白就到 `}`/`]`）。只决定要不要走修复分支。
/// 字节扫描安全：`,`/`}`/`]` 是 ASCII，UTF-8 多字节序列的字节不会与之碰撞。
fn may_have_trailing_comma(s: &str) -> bool {
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if c != b',' {
            continue;
        }
        let mut j = i + 1;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < b.len() && (b[j] == b'}' || b[j] == b']') {
            return true;
        }
    }
    false
}

/// 剥 ```` ```json ```` 围栏：首行是围栏就丢首行，末行是围栏就丢末行（没有围栏时一个字符都不动）。
fn strip_fence(s: &str) -> &str {
    let mut out = s;
    if let Some(rest) = out.strip_prefix("```") {
        out = match rest.split_once('\n') {
            Some((_, body)) => body,
            // 单行围栏（```` ```json{"a":1}``` ````，无换行）：剥语言标记前缀留全文——
            // 直接给空串会把明明有的 JSON 报成「没有 JSON 对象」
            None => rest.strip_prefix("json").unwrap_or(rest),
        };
    }
    let t = out.trim_end();
    if t.ends_with("```") {
        out = t[..t.len() - 3].trim_end();
    }
    out
}

/// 去尾逗号（`,}` / `,]` 一族）：字符串感知的扫描，字符串内容一个字符不动。
/// 按 char 扫而不是按字节 —— 实体名是中文，按字节推 char 会切成乱码。
fn drop_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    let mut in_str = false;
    while let Some(c) = it.next() {
        if in_str {
            out.push(c);
            // 转义字符连下一个一起原样带过（\" 不该开关字符串状态）
            if c == '\\' {
                if let Some(escaped) = it.next() {
                    out.push(escaped);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            continue;
        }
        if c == ',' {
            // 前瞻：逗号后只有空白就到 }/]，则丢逗号（空白原样保留）
            let mut ws = String::new();
            while let Some(&w) = it.peek() {
                if w.is_whitespace() {
                    ws.push(w);
                    it.next();
                } else {
                    break;
                }
            }
            if !matches!(it.peek(), Some('}') | Some(']')) {
                out.push(',');
            }
            out.push_str(&ws);
            continue;
        }
        out.push(c);
    }
    out
}

/// 截断辅助：按 Unicode 字符计（按字节截会把中文切成半个字），分配版留给要 owned 的调用点。
fn truncate_chars(s: &str, max: usize) -> String {
    truncate_str(s, max).to_string()
}

/// 切片版截断：按 Unicode 字符计（与 KB 侧偏移口径一致），只需要 `&str` 的调用点零分配。
fn truncate_str(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Value → Extraction：坏条目跳过（拿不到 text 的实体/端点不全的关系），
/// 数量与字段长度按常量截断。label 缺省 "Entity"（Yuxi 同款缺省）。
fn extraction_from_value(v: &serde_json::Value) -> Extraction {
    let entity = |x: &serde_json::Value| -> Option<RawEntity> {
        let (text, label) = match x {
            serde_json::Value::String(s) => (s.as_str(), "Entity"),
            serde_json::Value::Object(_) => {
                (x["text"].as_str()?, x["label"].as_str().unwrap_or("Entity"))
            }
            _ => return None,
        };
        let text = truncate_chars(text.trim(), MAX_NAME_CHARS);
        if text.is_empty() {
            return None;
        }
        let label = truncate_chars(label.trim(), MAX_LABEL_CHARS);
        Some(RawEntity {
            text,
            label: if label.is_empty() { "Entity".into() } else { label },
        })
    };
    let entities = v["entities"]
        .as_array()
        .map(|a| a.iter().filter_map(entity).take(MAX_ITEMS_PER_CHUNK).collect())
        .unwrap_or_default();
    let relations = v["relations"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    // `r["text"]` 兜底：兼容某些模型把关系名放在 `text` 字段的实测形态
                    // （prompt schema 之外的宽容路径）
                    let label = r["label"].as_str().or_else(|| r["text"].as_str()).unwrap_or("相关");
                    let label = truncate_chars(label.trim(), MAX_LABEL_CHARS);
                    Some(RawRelation {
                        source: entity(&r["source"])?,
                        target: entity(&r["target"])?,
                        label: if label.is_empty() { "相关".into() } else { label },
                    })
                })
                .take(MAX_ITEMS_PER_CHUNK)
                .collect()
        })
        .unwrap_or_default();
    Extraction { entities, relations }
}

/// 抽取结果 → 图投影：实体按**规范名**归并出确定性 id（label 不进归并键，KB 审查⑤）；
/// 关系端点若不在 entities 清单里自动补登（模型偶尔漏 entities 却给了关系，丢关系比补实体可惜）；
/// 噪声实体（泛词/单号/日期/金额实例值）不登记，指向它的关系随之丢弃；
/// 自环丢弃（「X 隶属于 X」是抽取噪声）；关系 label 归一到受控词表；同 chunk 内重复关系去重。
pub fn to_chunk_graph(space_id: &str, chunk_id: i64, doc_id: &str, ex: &Extraction) -> ChunkGraph {
    let mut it = Interner::default();
    for e in &ex.entities {
        it.intern(space_id, &e.text, &e.label);
    }
    let mut seen_rel: HashSet<(String, String, String)> =
        HashSet::with_capacity(MAX_ITEMS_PER_CHUNK);
    let mut relations: Vec<GraphRelation> = Vec::with_capacity(MAX_ITEMS_PER_CHUNK);
    for r in &ex.relations {
        let (Some(src), Some(dst)) = (
            it.intern(space_id, &r.source.text, &r.source.label),
            it.intern(space_id, &r.target.text, &r.target.label),
        ) else {
            continue;
        };
        if src == dst {
            continue;
        }
        let relation = normalize_relation(&r.label);
        if !seen_rel.insert((src.clone(), dst.clone(), relation.clone())) {
            continue;
        }
        relations.push(GraphRelation { src, dst, relation });
    }
    ChunkGraph {
        space_id: space_id.to_string(),
        doc_id: doc_id.to_string(),
        chunk_id,
        entities: it.entities,
        relations,
    }
}

/// 一个 chunk 内的实体归并器：规范名 → 确定性 id（同名实体同 id）。
/// label 不参与归并；同名实体的 label 取**本 chunk 内被提及次数最多**的那个（并列保持先到）
/// —— 跨 chunk 的 label 收敛没有全局票箱（构建是并发写的），与所有图数据改动一样
/// **必须重建图谱才生效**（`build_space` 先清该空间旧图再全量写）。
#[derive(Default)]
struct Interner {
    entities: Vec<GraphEntity>,
    ids: HashMap<String, usize>,
    /// `ballots[i]` = `entities[i]` 各 label 在本 chunk 内的票数（多数决的票箱）
    ballots: Vec<HashMap<String, usize>>,
    /// `cur_votes[i]` = `entities[i]` 当前 label 的票数（换 label 时同步更新），
    /// 省得每次投票都拿当前 label 反查票箱
    cur_votes: Vec<usize>,
}

impl Interner {
    /// 归并登记：返回该实体的确定性 id；噪声实体（空名/泛词/实例值）返 None，
    /// 指向它的关系端点由 `to_chunk_graph` 的 let-else 一并丢弃。
    fn intern(&mut self, space_id: &str, text: &str, label: &str) -> Option<String> {
        let name = normalize_name(text);
        if name.is_empty() || is_noise_entity(&name) {
            return None;
        }
        let label = {
            let l = normalize_name(label);
            if l.is_empty() { "entity".to_string() } else { l }
        };
        if let Some(&idx) = self.ids.get(&name) {
            // 同名再现一票；票数严格更多才换 label（并列保持先到，确定性）
            // 命中已登记 label 时不克隆（多数路径），miss 才 insert
            let n = if let Some(b) = self.ballots[idx].get_mut(&label) {
                *b += 1;
                *b
            } else {
                self.ballots[idx].insert(label.clone(), 1);
                1
            };
            if n > self.cur_votes[idx] {
                self.entities[idx].label = label;
                self.cur_votes[idx] = n;
            }
            return Some(self.entities[idx].id.clone());
        }
        let id = entity_id(space_id, &name);
        self.ids.insert(name.clone(), self.entities.len());
        self.ballots.push(HashMap::from([(label.clone(), 1)]));
        self.cur_votes.push(1);
        self.entities.push(GraphEntity { id: id.clone(), name, label });
        Some(id)
    }
}

/// 单次 LLM 抽取（Fast 档：批量后台任务的性价比档；形状约束在 prompt 里）。
/// `body` 须已按 `MAX_CHUNK_CHARS` 截断（调用方截一次，重试复用，不逐次重截）。
async fn extract_once<L: ChatModel + ?Sized>(llm: &L, body: &str) -> Result<Extraction, String> {
    let req = ChatRequest::text(
        ModelTier::Fast,
        EXTRACTION_SYSTEM,
        &format!("文本：\n{body}"),
        Some(0.1),
    );
    let reply = llm.chat(req).await.map_err(|e| format!("大模型调用失败：{e}"))?;
    let content = reply.content.filter(|c| !c.trim().is_empty()).ok_or("大模型没有返回内容")?;
    parse_extraction(&content)
}

/// 指数退避重试：共 RETRY_MAX+1 次尝试，间隔 base ×2ⁿ。
/// `base` 是参数而不是常量 —— 单测传 0 不等真实退避（慢测试最后总会被人 ignore 掉）。
async fn extract_with_retry<L: ChatModel>(
    llm: &L,
    text: &str,
    base: std::time::Duration,
) -> Result<Extraction, String> {
    // 入口截一次：同一 chunk 重试几遍，正文截断不该跟着做几遍
    let body = truncate_str(text, MAX_CHUNK_CHARS);
    let mut wait = base;
    let mut last = String::new();
    for attempt in 0..=RETRY_MAX {
        if attempt > 0 {
            tokio::time::sleep(wait).await;
            wait *= 2;
        }
        // 解析失败与传输失败同等重试是有意的取舍：温度 0.1 下输出形状仍有随机性，
        // 重试拿到可解析响应的实测概率不低；为省 1-2 发调用把可救的 chunk 打成失败样本
        // （留待人工 failed-chunks 干预）不划算。
        match extract_once(llm, body).await {
            Ok(ex) => return Ok(ex),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// 空间级构建：取可见 chunk → 清该空间旧图 → 并发 4 抽取+写图 → 回报进度。
/// 调用方（server）负责构建权认领与终态落库；本函数返回的 BuildOutcome 是终态的唯一事实源。
///
/// 失败语义：单 chunk 失败（LLM/解析/写图）记 failed + 样本（前 MAX_FAILED_SAMPLES 条），
/// 不拖垮整轮；整轮级失败（取数/建图/清库）直接 Err。
pub async fn build_space<L>(
    store: &OwnedStore,
    pg: &sqlx::PgPool,
    llm: &L,
    v: &Viewer,
    space_id: &str,
    progress: &dyn BuildProgress,
) -> Result<BuildOutcome, KbError>
where
    L: ChatModel + Clone + Send + Sync + 'static,
{
    let chunks = chunks_for_build(store, v, space_id).await?;
    doc_graph::ensure_graph(pg).await.map_err(as_db_err)?;
    doc_graph::clear_space(pg, space_id).await.map_err(as_db_err)?;

    let mut outcome = BuildOutcome { total: chunks.len(), ..BuildOutcome::default() };
    progress.report(outcome.total, 0, 0, &[]).await;

    let gate = Arc::new(tokio::sync::Semaphore::new(BUILD_CONCURRENCY));
    let mut set = tokio::task::JoinSet::new();
    for chunk in chunks {
        let gate = gate.clone();
        let pg = pg.clone();
        let llm = llm.clone();
        let space = space_id.to_string();
        // spawn 前截断：几千个任务各自持有完整正文（库里多长就多长）会一次性常驻内存；
        // 截断是幂等的（`extract_with_retry` 入口的同款截断不受影响）
        let text = truncate_str(&chunk.text, MAX_CHUNK_CHARS).to_string();
        set.spawn(async move {
            let _permit = gate.acquire_owned().await.expect("build gate 从不关闭");
            match extract_with_retry(&llm, &text, RETRY_BASE).await {
                Ok(ex) => {
                    let g = to_chunk_graph(&space, chunk.chunk_id, &chunk.doc_id, &ex);
                    doc_graph::write_chunk(&pg, &g)
                        .await
                        .map_err(|e| (chunk.doc_id.clone(), chunk.chunk_id, e.to_string()))
                }
                Err(e) => Err((chunk.doc_id, chunk.chunk_id, e)),
            }
        });
    }
    let mut since_report = 0usize;
    let mut last_report = std::time::Instant::now();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(())) => outcome.done += 1,
            Ok(Err((doc_id, chunk_id, error))) => {
                outcome.failed += 1;
                push_sample(&mut outcome.failed_samples, doc_id, chunk_id, error);
            }
            Err(e) => {
                outcome.failed += 1;
                // 哨兵（doc_id 空串 / chunk_id -1）= JoinError：任务 panic 拿不到 chunk 身份，
                // 只能以不可能值占位（FailedSample 是 wire 形状，类型不动）
                push_sample(&mut outcome.failed_samples, String::new(), -1, format!("任务中断：{e}"));
            }
        }
        // 进度落库节流：每 25 条或 500ms 报一次（2000 chunk 不该是 2000 次
        // `meta.kb_graph_build` 写），终态必报——判据抽成纯函数有单测钉住
        since_report += 1;
        if should_report(&outcome, since_report, last_report.elapsed()) {
            progress.report(outcome.total, outcome.done, outcome.failed, &outcome.failed_samples).await;
            since_report = 0;
            last_report = std::time::Instant::now();
        }
    }
    Ok(outcome)
}

/// `doc_graph` 错误 → `KbError::Db`（build_space 里连用，只此一份映射）
fn as_db_err(e: impl std::fmt::Display) -> KbError {
    KbError::Db(e.to_string())
}

/// 进度回报节流判据（纯函数）：每 25 条或 500ms 一到就报；**终态（最后一条）必报**。
fn should_report(
    outcome: &BuildOutcome,
    since_report: usize,
    elapsed: std::time::Duration,
) -> bool {
    outcome.done + outcome.failed == outcome.total
        || since_report >= 25
        || elapsed >= std::time::Duration::from_millis(500)
}

fn push_sample(samples: &mut Vec<FailedSample>, doc_id: String, chunk_id: i64, error: String) {
    if samples.len() < MAX_FAILED_SAMPLES {
        samples.push(FailedSample { doc_id, chunk_id, error: truncate_chars(&error, MAX_SAMPLE_ERR_CHARS) });
    }
}

// ==================== 【Y4】图谱运营：failed-chunks 候选与 reconcile 清理计划 ====================

/// failed-chunks 端点的候选清单：构建口径（当前 viewer 可见 + enabled + 已入库 +
/// 生效期内）的 chunk 三元组，与 `build_chunks_sql` 同一过滤族（只差不取正文 ——
/// 清单展示用不上，省搬运）。「未入图」= 本清单 − 图里已有的 Chunk 节点（server 侧做差）。
fn eligible_chunks_sql() -> &'static str {
    concat!(
        "SELECT c.chunk_id,c.doc_id,c.ord FROM kb.chunk c \
         JOIN kb.doc d ON d.doc_id=c.doc_id \
         WHERE d.space_id=$3 AND d.enabled=true AND d.status IN ('chunked','embedded') \
           AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE) \
           AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE) \
           AND d.doc_id IN (",
        crate::acl::visible_docs!(),
        ") ORDER BY c.chunk_id LIMIT $4"
    )
}

/// 构建口径的 chunk 三元组（chunk_id, doc_id, ord）；上限与构建同闸（MAX_BUILD_CHUNKS）。
pub async fn eligible_chunks(
    store: &OwnedStore,
    v: &Viewer,
    space_id: &str,
) -> Result<Vec<(i64, String, i32)>, KbError> {
    store
        .fixed(eligible_chunks_sql())
        .bind(&v.login)
        .bind(&v.roles)
        .bind(space_id)
        .bind(MAX_BUILD_CHUNKS)
        .fetch_all::<(i64, String, i32)>()
        .await
        .map_err(KbError::from)
}

/// reconcile 的孤儿判据输入：空间内「活着」的文档（enabled + 已入库 + 生效期内）。
/// 🔴 **刻意不带 `visible_docs!()`**：孤儿判据是**文档生命周期**（删除/禁用/失效），
/// 不是操作者可见性 —— 拿 viewer 可见集做差，会把「自己无权、别人可见」的文档误判成
/// 孤儿，一次 reconcile 就把别人的图数据清掉。权限闸在调用方（空间写权限，kg_api）。
fn alive_doc_ids_sql() -> &'static str {
    "SELECT doc_id FROM kb.doc WHERE space_id=$1 AND enabled=true \
     AND status IN ('chunked','embedded') \
     AND (effective_from IS NULL OR effective_from<=CURRENT_DATE) \
     AND (effective_to IS NULL OR effective_to>=CURRENT_DATE) ORDER BY doc_id"
}

/// 空间内「活着」的文档 id（运维判据专用，无 ACL —— 见 `alive_doc_ids_sql` 的 🔴 注释）。
pub async fn alive_doc_ids(store: &OwnedStore, space_id: &str) -> Result<Vec<String>, KbError> {
    Ok(store
        .fixed(alive_doc_ids_sql())
        .bind(space_id)
        .fetch_all::<(String,)>()
        .await?
        .into_iter()
        .map(|(id,)| id)
        .collect())
}

/// failed-chunks 的集合差（纯函数）：构建口径里有、图里没有的 chunk。
/// 输入都已按 chunk_id 升序，输出保序 —— 分页切片因此是确定性的。
pub fn missing_from_graph(
    eligible: &[(i64, String, i32)],
    present: &std::collections::HashSet<i64>,
) -> Vec<(i64, String, i32)> {
    eligible.iter().filter(|(cid, _, _)| !present.contains(cid)).cloned().collect()
}

/// reconcile 的清理计划（纯函数）：图里 doc 已不「活着」的 Chunk 节点 = 孤儿。
/// `max_orphans` 是**执行闸**不是清理目标：孤儿数超阈值只置 `over_threshold`，
/// 清单照常给全 —— dry-run 要看的就是全量；执行层（kg_api）撞上它必须拒删并报告。
/// 一次清掉半个空间的图通常意味着判据/数据出了错，宁可让人看一眼再放大闸值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcilePlan {
    /// 图里 Chunk 节点总数（该空间）
    pub graph_chunks: usize,
    /// 孤儿 chunk_id（升序去重，确定性）
    pub orphan_chunk_ids: Vec<i64>,
    pub max_orphans: usize,
    pub over_threshold: bool,
}

pub fn plan_reconcile(
    graph_chunks: &[(String, i64)],
    alive_docs: &std::collections::HashSet<String>,
    max_orphans: usize,
) -> ReconcilePlan {
    let mut orphan_chunk_ids: Vec<i64> = graph_chunks
        .iter()
        .filter(|(doc_id, _)| !alive_docs.contains(doc_id))
        .map(|(_, cid)| *cid)
        .collect();
    orphan_chunk_ids.sort_unstable();
    orphan_chunk_ids.dedup();
    let over_threshold = orphan_chunk_ids.len() > max_orphans;
    ReconcilePlan { graph_chunks: graph_chunks.len(), orphan_chunk_ids, max_orphans, over_threshold }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 干净 JSON 直出。
    #[test]
    fn parse_clean_json() {
        let raw = r#"{"entities":[{"text":"差旅报销制度","label":"制度"}],
            "relations":[{"source":{"text":"差旅报销制度","label":"制度"},
                          "target":{"text":"财务部","label":"组织"},"label":"由发布"}]}"#;
        let ex = parse_extraction(raw).unwrap();
        assert_eq!(ex.entities.len(), 1);
        assert_eq!(ex.relations[0].label, "由发布");
    }

    /// 围栏 + 前后散文 + 尾逗号：三层容错各中一条。
    #[test]
    fn parse_tolerates_fence_prose_and_trailing_commas() {
        let fenced = "好的，抽取结果如下：\n```json\n{\n  \"entities\": [\n    {\"text\": \"烤肠\", \"label\": \"产品\"},\n  ],\n  \"relations\": [],\n}\n```\n以上。";
        let ex = parse_extraction(fenced).unwrap();
        assert_eq!(ex.entities[0].text, "烤肠");
        // 散文不带围栏的形态
        let prose = "结果：{\"entities\": [{\"text\": \"A\"}], } 完毕";
        assert_eq!(parse_extraction(prose).unwrap().entities[0].label, "Entity");
        // 尾逗号修复必须是字符串感知的：字符串里的 `",}` 不许动
        let tricky = r#"{"entities":[{"text":"a\",}b","label":"概念",}],}"#;
        let ex = parse_extraction(tricky).unwrap();
        assert_eq!(ex.entities[0].text, "a\",}b");
    }

    /// 坏条目跳过不报错；完全没有 JSON 才是 chunk 级失败。
    #[test]
    fn parse_skips_bad_items_and_rejects_json_less_text() {
        let raw = r#"{"entities":[{"no_text":1},{"text":"有效实体"},"",{"text":"  "}],
                      "relations":[{"source":{"text":"A"},"label":"x"},"not_an_object"]}"#;
        let ex = parse_extraction(raw).unwrap();
        assert_eq!(ex.entities.len(), 1);
        assert!(ex.relations.is_empty());
        assert!(parse_extraction("我不知道怎么抽").is_err());
        assert!(parse_extraction("{}").unwrap().entities.is_empty());
    }

    /// 抽取上限：实体/关系各 50 条封顶，字段按字符截断。
    #[test]
    fn parse_caps_are_enforced() {
        let many = (0..80).map(|i| format!("{{\"text\":\"e{i}\"}}")).collect::<Vec<_>>().join(",");
        let raw = format!("{{\"entities\":[{many}]}}");
        assert_eq!(parse_extraction(&raw).unwrap().entities.len(), MAX_ITEMS_PER_CHUNK);
        let long = format!("{{\"entities\":[{{\"text\":\"{}\",\"label\":\"{}\"}}]}}", "长".repeat(200), "类".repeat(50));
        let ex = parse_extraction(&long).unwrap();
        assert_eq!(ex.entities[0].text.chars().count(), MAX_NAME_CHARS);
        assert_eq!(ex.entities[0].label.chars().count(), MAX_LABEL_CHARS);
    }

    /// 归并语义：空白压缩 + 小写化后相同 ⇒ 同 id；space 不同 ⇒ 不同 id。
    /// label 不再参与 id（KB 审查⑤：同名不同 label 曾是实体碎裂的根因）。
    #[test]
    fn entity_id_merges_and_separates() {
        assert_eq!(normalize_name("  差旅   报销 制度 "), "差旅 报销 制度");
        assert_eq!(normalize_name("ERP 系统"), "erp 系统");
        let a = entity_id("sp1", &normalize_name("差旅  报销"));
        let b = entity_id("sp1", &normalize_name("差旅 报销"));
        assert_eq!(a, b, "规范名相同必须同 id（归并）");
        assert_ne!(a, entity_id("sp2", &normalize_name("差旅 报销")), "跨空间不许归并");
        assert!(a.starts_with("e_") && a.len() == 18);
    }

    /// 🔴 id 口径回归钉：手写 hash 必须与基线 `format!("{s}:{n}").hash()` 逐位一致——
    /// id 变了新旧图永不匹配（这等价于钉死 `entity_id("sp1","差旅报销")` 的既有字面量）
    #[test]
    fn entity_id_matches_format_baseline() {
        use std::hash::{Hash, Hasher};
        let baseline = |space: &str, name: &str| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            format!("{space}:{name}").hash(&mut h);
            format!("e_{:016x}", h.finish())
        };
        for (s, n) in [("sp1", "差旅报销"), ("sp2", "erp 系统"), ("", ""), ("a:b", "c:d")] {
            assert_eq!(entity_id(s, n), baseline(s, n), "({s},{n}) 的 id 口径漂移");
        }
    }

    /// 单行围栏（无换行）里的 JSON 不许被剥成空串
    #[test]
    fn single_line_fence_keeps_its_json() {
        let ex = parse_extraction("```json{\"entities\":[{\"text\":\"烤肠\"}]}```").unwrap();
        assert_eq!(ex.entities[0].text, "烤肠");
    }

    /// 纯符号名（一个字母/数字/汉字都没有）是噪声兜底族
    #[test]
    fn pure_symbol_names_are_noise() {
        for noise in ["——", "…", "—— ——", "※"] {
            assert!(is_noise_entity(&normalize_name(noise)), "{noise} 必须被拦");
        }
        // 含一个汉字/字母就放行（别误拦正常实体）
        for keep in ["第—章", "a-b", "μ 系数"] {
            assert!(!is_noise_entity(&normalize_name(keep)), "{keep} 不许误拦");
        }
    }

    /// 进度回报节流：计数/时间任一到位即报；终态必报（哪怕刚报过）
    #[test]
    fn progress_report_is_throttled_but_final_is_always_reported() {
        use std::time::Duration;
        let mk = |total: usize, done: usize, failed: usize| BuildOutcome {
            total,
            done,
            failed,
            failed_samples: vec![],
        };
        let o = mk(100, 10, 0);
        assert!(!should_report(&o, 1, Duration::from_millis(10)), "刚报过就不该再报");
        assert!(should_report(&o, 25, Duration::from_millis(10)), "25 条一到即报");
        assert!(should_report(&o, 1, Duration::from_millis(600)), "500ms 一到即报");
        let fin = mk(100, 99, 1);
        assert!(should_report(&fin, 1, Duration::from_millis(1)), "终态必报");
    }

    /// 图投影：大小写/空白变体归并为一个实体；关系端点自动补登；自环与重复关系丢弃。
    #[test]
    fn chunk_graph_merges_and_drops_noise() {
        let ex = Extraction {
            entities: vec![
                RawEntity { text: "ERP 系统".into(), label: "产品".into() },
                RawEntity { text: "erp  系统".into(), label: "产品".into() },
            ],
            relations: vec![
                RawRelation {
                    source: RawEntity { text: "ERP系统".into(), label: "产品".into() },
                    target: RawEntity { text: "财务部".into(), label: "组织".into() },
                    label: " 由 维护 ".into(),
                },
                RawRelation {
                    source: RawEntity { text: "财务部".into(), label: "组织".into() },
                    target: RawEntity { text: "财务部".into(), label: "组织".into() },
                    label: "自环".into(),
                },
            ],
        };
        let g = to_chunk_graph("sp1", 7, "doc-1", &ex);
        // 「ERP 系统」与「erp  系统」归并；「ERP系统」（无空格）是另一个规范名 —— 归并
        // 只保证确定性，不发明语义等价。
        assert_eq!(g.entities.len(), 3, "{:?}", g.entities);
        assert_eq!(g.entities[0].name, "erp 系统");
        assert_eq!(g.relations.len(), 1, "自环必须丢：{:?}", g.relations);
        assert_eq!(g.relations[0].relation, "相关", "词表外的「由 维护」归一到受控词表");
        assert!(g.entities.iter().any(|e| e.name == "财务部"), "关系端点要补登");
        assert!(g.relations[0].src.starts_with("e_") && g.relations[0].dst.starts_with("e_"));
    }

    // ==================== KB 审查⑤：归并键去 label / 噪声过滤 / 关系受控词表 ====================

    /// 同名不同 label 归并成**一个**节点，label 取本 chunk 内提及次数最多的（并列保持先到）。
    /// ⚠️ 跨 chunk 的 label 收敛没有全局票箱 —— 图数据改动必须重建图谱才生效。
    #[test]
    fn same_name_merges_and_label_is_majority_within_the_chunk() {
        let ex = Extraction {
            entities: vec![
                RawEntity { text: "差旅报销制度".into(), label: "制度".into() },
                RawEntity { text: "差旅报销制度".into(), label: "概念".into() },
                RawEntity { text: "差旅报销制度".into(), label: "概念".into() },
            ],
            relations: vec![],
        };
        let g = to_chunk_graph("sp1", 7, "doc-1", &ex);
        assert_eq!(g.entities.len(), 1, "同名不许碎成多节点：{:?}", g.entities);
        assert_eq!(g.entities[0].label, "概念", "多数决：概念 2 票 > 制度 1 票");
        assert_eq!(g.entities[0].id, entity_id("sp1", "差旅报销制度"));
        // 并列保持先到（确定性）
        let ex = Extraction {
            entities: vec![
                RawEntity { text: "财务部".into(), label: "组织".into() },
                RawEntity { text: "财务部".into(), label: "部门".into() },
            ],
            relations: vec![],
        };
        let g = to_chunk_graph("sp1", 7, "doc-1", &ex);
        assert_eq!(g.entities.len(), 1);
        assert_eq!(g.entities[0].label, "组织", "1:1 并列保持先到的 label");
    }

    /// 噪声实体不登记：泛词 stoplist、单号/编号、日期、金额实例值；指向它们的关系一并丢弃。
    #[test]
    fn noise_entities_never_enter_the_graph() {
        for noise in [
            "制度", "规定", "公司", "po-2024-001", "DHT150-6", "2026-08-06", "2025年3月",
            "5000元", "1,000元", "50%",
        ] {
            assert!(is_noise_entity(&normalize_name(noise)), "{noise} 必须被拦");
        }
        for keep in ["差旅报销制度", "erp 系统", "第三章", "v2", "总经理"] {
            assert!(!is_noise_entity(&normalize_name(keep)), "{keep} 不许误拦");
        }
        let ex = Extraction {
            entities: vec![
                RawEntity { text: "PO-2024-001".into(), label: "单号".into() },
                RawEntity { text: "差旅报销制度".into(), label: "制度".into() },
            ],
            relations: vec![RawRelation {
                source: RawEntity { text: "PO-2024-001".into(), label: "单号".into() },
                target: RawEntity { text: "差旅报销制度".into(), label: "制度".into() },
                label: "适用于".into(),
            }],
        };
        let g = to_chunk_graph("sp1", 7, "doc-1", &ex);
        assert_eq!(g.entities.len(), 1, "单号实例值不进图：{:?}", g.entities);
        assert_eq!(g.entities[0].name, "差旅报销制度");
        assert!(g.relations.is_empty(), "端点被拦的关系随之丢弃");
    }

    /// 关系 label 受控词表：词表内保留（先空白/大小写归一），词表外（含空）一律「相关」；
    /// 归一后同一对实体的不同写法按重复关系去重。
    #[test]
    fn relation_labels_are_clamped_to_the_vocabulary() {
        assert_eq!(normalize_relation(" 隶属于 "), "隶属于");
        assert_eq!(normalize_relation("由发布"), "相关");
        assert_eq!(normalize_relation(""), "相关");
        let mk = |label: &str| RawRelation {
            source: RawEntity { text: "员工公寓".into(), label: "地点".into() },
            target: RawEntity { text: "后勤部".into(), label: "组织".into() },
            label: label.into(),
        };
        let ex = Extraction { entities: vec![], relations: vec![mk("由管理"), mk("归口于")] };
        let g = to_chunk_graph("sp1", 7, "doc-1", &ex);
        assert_eq!(g.relations.len(), 1, "词表外 label 都归「相关」→ 同对同 label 去重");
        assert_eq!(g.relations[0].relation, "相关");
    }

    /// 抽取 prompt 的硬约束钉（KB 审查⑤）：不抽实例值/泛词 + 关系受控词表，
    /// 缺一条图就长垃圾（prompt 是第一道，`is_noise_entity`/`normalize_relation` 是兜底）。
    #[test]
    fn extraction_prompt_carries_the_hard_constraints() {
        assert!(EXTRACTION_SYSTEM.contains("不抽单号/编号、日期、金额"), "{EXTRACTION_SYSTEM}");
        assert!(EXTRACTION_SYSTEM.contains("泛词"), "{EXTRACTION_SYSTEM}");
        assert!(EXTRACTION_SYSTEM.contains("受控词表"), "{EXTRACTION_SYSTEM}");
        for l in RELATION_VOCAB {
            assert!(EXTRACTION_SYSTEM.contains(l), "prompt 缺受控词表成员 {l}");
        }
    }

    struct Fake {
        calls: AtomicUsize,
        fail_first: usize,
    }

    impl ChatModel for Fake {
        fn chat<'a>(
            &'a self,
            _req: ChatRequest,
        ) -> dms_kernel::BoxFut<'a, Result<dms_kernel::ChatReply, dms_kernel::LlmError>> {
            let n = self.calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                if n < self.fail_first {
                    return Err(dms_kernel::LlmError::Transport("模拟故障".into()));
                }
                Ok(dms_kernel::ChatReply {
                    content: Some("{\"entities\":[],\"relations\":[]}".into()),
                    usage: Default::default(),
                })
            })
        }
    }

    /// 重试语义：先败后成 → 成功且调用数 = 失败数+1；一直败 → RETRY_MAX+1 次后 Err。
    /// 退避基数传 0，单测不等真实墙钟。
    #[tokio::test]
    async fn retry_is_exponential_and_bounded() {
        let flaky = Fake { calls: AtomicUsize::new(0), fail_first: 2 };
        assert!(extract_with_retry(&flaky, "t", std::time::Duration::ZERO).await.is_ok());
        assert_eq!(flaky.calls.load(Ordering::Relaxed), 3);

        let down = Fake { calls: AtomicUsize::new(0), fail_first: 99 };
        let err = extract_with_retry(&down, "t", std::time::Duration::ZERO).await.unwrap_err();
        assert_eq!(down.calls.load(Ordering::Relaxed), (RETRY_MAX + 1) as usize);
        assert!(err.contains("模拟故障"), "{err}");
    }

    /// 🔴 ACL 锚点：构建取数与可见集合两条 SQL 都必须内联 `visible_docs!()` 且带
    /// enabled/status/生效期过滤 —— 少一条，图谱就把他人文档的实体漏给无权者。
    #[test]
    fn acl_fragment_is_inlined_in_both_read_paths() {
        let src = include_str!("kg.rs");
        for f in ["fn build_chunks_sql", "fn visible_doc_ids_sql"] {
            let body = src
                .split(f)
                .nth(1)
                .unwrap_or_else(|| panic!("{f} 不见了 —— 锚点失效"))
                .split("\n}\n")
                .next()
                .expect("SQL 函数形状变了");
            assert!(body.contains(concat!("crate::acl::visible_", "docs!()")), "{f} 丢了 ACL 内联");
            assert!(body.contains("enabled=true"), "{f} 丢了 enabled 过滤");
            assert!(body.contains("status IN ('chunked','embedded')"), "{f} 丢了状态过滤");
            assert!(body.contains("effective_from"), "{f} 丢了生效期过滤");
            assert!(body.contains("space_id=$3"), "{f} 的空间过滤必须绑参数，不许拼串");
        }
        let build = src
            .split("fn build_chunks_sql")
            .nth(1)
            .unwrap_or_else(|| panic!("fn build_chunks_sql 不见了 —— 锚点失效"));
        assert!(build.contains("LIMIT $4"), "构建成本闸（chunk 上限）没了");
    }

    /// 并发与重试必须是这两个值（契约①）；改它们就是改契约。
    #[test]
    fn build_concurrency_and_retry_are_the_contract() {
        assert_eq!(BUILD_CONCURRENCY, 4);
        assert_eq!(RETRY_MAX, 2);
        assert_eq!(MAX_FAILED_SAMPLES, 5);
    }

    // ==================== 【Y4】failed-chunks 与 reconcile ====================

    /// eligible_chunks 必须与构建同一过滤族（可见 + enabled + 状态 + 生效期 + 空间绑参），
    /// 只是不取正文；alive_doc_ids 刻意**没有** ACL 片段（生命周期判据，见函数 🔴 注释）。
    #[test]
    fn y4_sql_filters_match_their_contracts() {
        let eligible = eligible_chunks_sql();
        assert!(eligible.contains(concat!("crate::acl::visible_", "docs!()")) || eligible.contains("kb.acl a"),
                "eligible 丢了 ACL 内联");
        assert!(eligible.contains("enabled=true") && eligible.contains("status IN ('chunked','embedded')"));
        assert!(eligible.contains("effective_from") && eligible.contains("space_id=$3"));
        assert!(eligible.contains("LIMIT $4"), "与构建同一个成本闸");
        let alive = alive_doc_ids_sql();
        assert!(alive.contains("enabled=true") && alive.contains("effective_from"));
        assert!(alive.contains("space_id=$1"), "alive 的空间过滤必须绑参数");
        assert!(!alive.contains("kb.acl"), "alive 不许带 ACL（生命周期判据，不是可见性）");
    }

    /// 集合差：在图集合之外的全保留、保序；空输入/全命中都是确定性空。
    #[test]
    fn missing_from_graph_is_order_preserving_set_difference() {
        let eligible = vec![
            (1i64, "d1".to_string(), 0i32),
            (2, "d1".into(), 1),
            (3, "d2".into(), 0),
        ];
        let present: std::collections::HashSet<i64> = [1, 3].into_iter().collect();
        let missing = missing_from_graph(&eligible, &present);
        assert_eq!(missing, vec![(2, "d1".to_string(), 1)]);
        assert!(missing_from_graph(&[], &present).is_empty());
        assert_eq!(missing_from_graph(&eligible, &[].into_iter().collect()), eligible,
                   "图全空（未构建）时未入图 = 全部候选");
    }

    /// 清理计划：doc 不活着的 Chunk 才是孤儿；升序去重（确定性）；阈值只置旗不截清单
    /// （dry-run 要报全量，拒删是执行层的事）。
    #[test]
    fn reconcile_plan_marks_orphans_and_threshold() {
        let alive: std::collections::HashSet<String> = ["d1".to_string()].into_iter().collect();
        let graph = vec![
            ("d1".to_string(), 5i64),
            ("d2".to_string(), 2),
            ("d3".to_string(), 9),
            ("d2".to_string(), 2), // 防御重复：MERGE 不该出重，出了也不许双删/双数
        ];
        let plan = plan_reconcile(&graph, &alive, 10);
        assert_eq!(plan.graph_chunks, 4);
        assert_eq!(plan.orphan_chunk_ids, vec![2, 9], "必须升序去重");
        assert!(!plan.over_threshold);
        let plan = plan_reconcile(&graph, &alive, 1);
        assert!(plan.over_threshold, "孤儿 2 > 闸 1");
        assert_eq!(plan.orphan_chunk_ids.len(), 2, "超闸不截清单（dry-run 报全量）");
        // 全活着 / 图全空：两种零操作形态
        let none = plan_reconcile(&[], &alive, 10);
        assert!(none.orphan_chunk_ids.is_empty() && !none.over_threshold);
        let all_alive = plan_reconcile(&[("d1".to_string(), 5)], &alive, 0);
        assert!(all_alive.orphan_chunk_ids.is_empty() && !all_alive.over_threshold);
    }
}
