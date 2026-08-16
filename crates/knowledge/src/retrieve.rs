//! 混合检索：**ACL 先行**（可见 doc 子查询内联进检索 SQL）→ 向量/精确 token/正文 trgm/词级词命中/标题/元数据各自 top
//! → 目录/文档关系扩展 + 图谱召回（Yuxi B6：实体种子 → 1~2 hop 扩散 → 幂迭代 PPR，
//! `DMS_KG_RETRIEVAL=off` 整体关闭）+ 外部只读 KB（Yuxi B9：Dify 数据集检索，
//! `DMS_EXT_KB_*` 未配齐即关闭，关闭时零 IO）→ RRF 融合 → 同文档相邻块合并、去重 → 来源多样化截断。
//! 变更原因＝召回口径。
//! 可选精排：`DMS_RERANK_*` 配置存在时，合并去重后的头部候选（约 2×`TOP_K`）先送 rerank 重排再截
//! （`docs/research/yuxi.json` B5：recall_top_k → 精排 → final_top_k）；未配置/打分失败一律走原路。
//!
//! 两条不许退让的：
//! 1. **绝不「查完再过滤」**：ACL 条件必须参与检索查询本身。后过滤在 HNSW 上等于自杀 ——
//!    先取到的全局最近邻可能整批属于别人，滤完剩零条，症状是「明明上传了文档却搜不到」。
//! 2. **embed 挂只跳这一路**：精确匹配与 trgm 仍要出结果（`ingest` 也允许停在 `chunked`）。
//!
//! 两个阈值（`TRGM_MIN` / `VEC_MAX_DIST`）是**标定值**，都由连库实测的分数分布定的，
//! 各自钉着一张 `*_MEASURED` 表。改它们之前先重量一遍分布，别拍脑袋调。
//!
//! 阈值与名额仍是 `const`（标定值，改动要重量分布）；**RRF 四路辅助召回的权重**自 Y3 起
//! 从 settings.json `kb_rrf_weights` 读（缺省 = 原编译期常量，逐路字节级等价），由调用方把
//! 快照以 `&RrfWeights` 传进来 —— 本文件**不许**全局单例读配置/文件（检索是纯函数式的：
//! 同样的输入与权重，同样的结果）。

use crate::{KbError, Viewer};
#[cfg(test)]
use crate::acl;
use dms_connector::doc_graph::{self, RelEdge};
use dms_connector::embed::{to_pgvector, EmbedClient};
use dms_connector::external_kb::{ExtKbClient, ExtKbRecord};
use dms_connector::owned::OwnedStore;
use dms_connector::rerank::RerankClient;
use std::collections::HashMap;

/// 各召回路的上限
const VEC_TOP: i64 = 20;
const EXACT_TOP: i64 = 20;
const TRGM_TOP: i64 = 10;
const TITLE_TOP: i64 = 10;
const METADATA_TOP: i64 = 10;
const RELATION_TOP: i64 = 10;
/// 目录、文档族/版本、业务域和标签只是弱关联，必须达到正文/标题相似度门才可进入上下文。
/// 显式 doc_link 可直接扩展，但以低权重进入，回答层仍只能引用正文直接支持的事实。
const RELATION_CONTEXT_MIN: f32 = TRGM_MIN;
const METADATA_WEIGHT: f32 = 0.2;
const RELATION_WEIGHT: f32 = 0.25;
/// 图谱召回路（第 7 路，Yuxi B6）进 RRF 的权重：与 RELATION_WEIGHT 同级的辅助加分项，
/// 不许压过正文直接命中（1.0 路）。
const KG_WEIGHT: f32 = 0.3;
/// 图谱路送进 RRF 的 chunk 名额（PPR 分降序）。
const KG_TOP: usize = 10;
/// 外部只读 KB 路（第 8 路，Yuxi B9）进 RRF 的权重：与 METADATA_WEIGHT 同级的辅助加分项，
/// 不许压过正文直接命中（1.0 路）—— 远程块只过「配置即授权」，没有本地 ACL/阈值复核。
const EXT_KB_WEIGHT: f32 = 0.2;
/// 外部路从远程取回的块数：远程排序质量不透明，宁少勿滥。
const EXT_KB_TOP: usize = 4;
/// 词级稀疏召回路（第 9 路，对照 Yuxi 稠密+BM25 混合检索的 BM25 半）：jieba 词命中。
/// trgm 是**模糊相似**不是「词命中」，短口语化问句（小程序场景）排序粗；词级路把问句与
/// 块正文过**同一个分词器**（`store::terms_of` 单一事实源），按命中的不同问句词数排序。
/// 权重恒 1.0：它是正文直接命中路，与四路正文同一条「字面量钉死、配置够不着」的纪律。
const TERMS_WEIGHT: f32 = 1.0;
/// 词级路的候选上限：与 TRGM_TOP 同档（同是正文路，名额不膨胀）。
const TERMS_TOP: i64 = 10;
/// 问句参与词级路的词数上限（防爆闸，与 `EXACT_MAX_TOKENS` 同族；正常问句 ≤10 个词，
/// 评测集最大 9—— 闸对语料零影响，防的是「把整篇文档粘进问句」的病态输入）。
/// 🔴 只能用在**查询侧**：块正文的词表必须全量落库（写侧截断 = 词级路召回缺臂）。
const TERMS_MAX_QUERY_TERMS: usize = 32;
/// 词级路入选门：至少命中几个**不同**问句词。标定值 —— 2026-08-11 连真库量的分布
/// （kb_eval 夹具 14 篇 / 每题「问句词 × 各块命中数」全表，钉下来的几个值见 `TERMS_MEASURED`）：
/// 判据块全部 ≥2（最低的是 KB02 的 2/4），而远域 nohit（KB07「月球基地」）的最高噪声只有 1 ——
/// 门取 2 把远域 nohit 整路清空，判据块一个不掉。
/// 🔴 它做不到的事（与 `VEC_MAX_DIST` 同一族）：**近域** nohit 分不出来 —— KB13「差旅打车费」
/// 的最高噪声块（体检尾块）命中 4/7 个问句词，比一半判据块都高；任何能挡住它的门都会先打死
/// 正向题。那道题由 `answer::keep_cited_only` 兜，不是由命中数门兜。
/// 问句词数不足时按问句词数收口（`terms_min_hits`），单词问句不变哑。
const TERMS_MIN_HITS: i64 = 2;

/// 【Y3】RRF 四路**辅助**召回的权重（settings.json `kb_rrf_weights`）。缺省 = 上面四个
/// 编译期常量，与引入本项前**逐路字节级等价**（有单测钉着）。正文直接命中的四路恒 1.0，
/// 不在可调范围内 —— 「辅助路不许压过正文路」的那条 1.0 由调用点字面量钉死，配置够不着。
///
/// 反序列化语义：整个键缺席 → `Default`（全旧值）；键在但缺某路 → 该路仍取旧值
/// （部分覆盖）；键名打错 → `deny_unknown_fields` 硬失败（与 `Settings` 同一条纪律）。
/// 运行时改值走 settings_api 页面（保存即生效，调用方每次检索重新取快照）。
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RrfWeights {
    /// 元数据路（标签/业务域/来源）
    pub metadata: f32,
    /// 关系扩展路（目录/文档族/版本/显式关联）
    pub relation: f32,
    /// 图谱召回路（第 7 路，Yuxi B6）
    pub kg: f32,
    /// 外部只读 KB 路（第 8 路，Yuxi B9）
    pub ext_kb: f32,
}

impl Default for RrfWeights {
    /// 缺省 = 原编译期常量。改上面四个 const 就是改默认行为，等价性单测会跟着红。
    fn default() -> Self {
        Self {
            metadata: METADATA_WEIGHT,
            relation: RELATION_WEIGHT,
            kg: KG_WEIGHT,
            ext_kb: EXT_KB_WEIGHT,
        }
    }
}

impl RrfWeights {
    /// 配置闸：负值与非有限值（NaN/±Inf）一律**拒绝**（报错，不 clamp —— 写错配置的
    /// 人必须在保存/启动时当场看见，而不是拿一份被静默改过的权重跑出没法复现的排序）。
    /// `0.0` 合法：该路候选仍进 RRF 但不加权（加分恒 0）。
    /// `rrf_weighted` 里的 `.max(0.0)` 是最后一道保险丝；两层是「拒绝坏配置」与
    /// 「防御性钳制」，不互相替代。
    pub fn validate(&self) -> Result<(), String> {
        for (name, w) in [
            ("metadata", self.metadata),
            ("relation", self.relation),
            ("kg", self.kg),
            ("ext_kb", self.ext_kb),
        ] {
            if !w.is_finite() {
                return Err(format!("kb_rrf_weights.{name} 必须是有限数（不许 NaN/Inf）"));
            }
            if w < 0.0 {
                return Err(format!("kb_rrf_weights.{name} 不许为负（{w}）"));
            }
        }
        Ok(())
    }

    /// 八路权重数组：前四路正文恒 1.0（字面量钉死，见 struct 文档），后四路取配置。
    /// 第一遍融合只用前 5 路（直接命中），第二遍用全 8 路 —— 两个调用点共享这一份顺序。
    pub fn route_array(&self) -> [f32; 8] {
        [1.0, 1.0, 1.0, 1.0, self.metadata, self.relation, self.kg, self.ext_kb]
    }
}
/// 扩散子图节点上限（实体 + chunk 合计）：B6 `graph_max_nodes` 的落法，是防爆闸不是相关性工具。
const KG_MAX_NODES: usize = 200;
/// 种子个性化权重：直接命中（向量路 chunk 提及 / 实体名 trgm·包含）1.0，命中实体的邻边实体 0.8；
/// 二跳实体 0.0 —— 参与拓扑但不回传个性化质量，质量只压在「问句直接相关的实体及其紧邻」上。
const KG_SEED_DIRECT: f64 = 1.0;
const KG_SEED_NEIGHBOR: f64 = 0.8;
const KG_SEED_HOP2: f64 = 0.0;
/// 实体名 trgm 种子的相似度下限（问句包含命中不看这个分）：种子阶段宁宽勿严（扩散与 PPR 会
/// 重排），但「毫不相干」的泛名种子会把个性化质量洒进无关邻域。与 TITLE/METADATA 辅助路同档。
const KG_SEED_SIM_MIN: f32 = 0.3;
/// 种子实体数上限：个性化压在少数实体上才是「个性化」。
const KG_SEED_MAX: usize = 20;
/// 名称命中种子的候选池：可见文档里提及次数最多的前 N 个实体参与匹配。
const KG_SEED_CANDIDATES: usize = 500;
/// 邻边 / MENTIONS 明细查询的行数闸（防枢纽实体爆内存；语义上限由 Rust 组装侧的
/// `KG_MAX_NODES` 收口，行数闸只是搬运量的保险丝）。
const KG_EDGE_ROWS: usize = 1000;
const KG_PAIR_ROWS: usize = 5000;
/// 幂迭代 PPR：阻尼 0.85（标准值）；≤200 节点、数千边的子图上 100 轮迭代成本可忽略。
const KG_PPR_DAMPING: f64 = 0.85;
const KG_PPR_MAX_ITER: usize = 100;
const KG_PPR_TOL: f64 = 1e-6;
/// 融合后进 prompt 的块数
const TOP_K: usize = 6;
/// 先多取候选再去重/多样化；直接在 RRF 后截 6 条会让同一文档的相邻块挤掉其他来源。
const CANDIDATE_K: usize = TOP_K * 4;
/// rerank 精排的窗口：合并去重后的头部送精排，排完仍截回 `TOP_K`
/// （`docs/research/yuxi.json` B5：recall_top_k → 精排 → final_top_k）。窗口外候选保持 RRF 原序。
///
/// 🔴 = `CANDIDATE_K`，**不是 `TOP_K * 2`**（2026-08-16）。旧值 12 够不到候选集的
/// 第 13-24 名，而生产度量是 `recall@6 = 0.95` / `recall@1 = 0.15` —— 精排要救的正是
/// 「召回到了但排在后面」。拿一个够不到后半段的窗口去测「精排有没有收益」，
/// 测出来必然是「没收益」，然后有人会把这个能力删掉。
/// 代价：一次 rerank 多 12 条文本，上游 `MAX_DOCS = 500`，富余。
const RERANK_WINDOW: usize = CANDIDATE_K;
/// 多文档时先保证来源覆盖；候选不足时第二轮仍会用同文档结果补满。
const DOC_FIRST_PASS: usize = 2;
/// `span()` 的硬上限也是 16，合并超过它会让引用无法完整回查。
const MAX_MERGE_SPAN: u32 = 16;
/// RRF 常数（业界默认 60）
const RRF_K: f32 = 60.0;
/// 可见 doc 少于这个数时向量路走精确扫描（理由见 `scan_mode`）
const EXACT_SCAN_DOCS: usize = 50;

/// trgm 路的词相似度下限。**按实测分布选的**（2026-07-29 连真库量：kb_eval 10 篇夹具 / 23 块 /
/// 14 道 A 用户题，每题三路原始分数全表；钉下来的那几个值见 `TRGM_MEASURED`）。
///
/// 🔴 原值 0.3 把 KB01 的判据块（**0.2667**）挡在门外，而 14 题里只有 3 题能让 trgm 出结果
/// （其中一次还是同文档标题块）—— 「三路混合 + RRF」于是退化成**单路**：
/// 当时的 FTS 那一路 `plainto_tsquery('simple', …)` 对中文不分词，实测 14 题 × 23 块
/// **322 格全部 `ts_rank_cd = 0`**（恒空；KB 审查③已把它换成单号/型号精确匹配路）。
/// trgm 再被挡住，RRF 就只剩向量一路可融。
/// 降到 0.2 后 trgm 在 **9/14** 题出结果（每题 1-3 条）。
///
/// 上界钉在 0.2105（KB02「发票 15 个工作日」与 KB16「通讯补贴四档」的判据块），
/// 下界钉在 **0.1818** —— KB13「差旅打车费」（**近域** nohit，库里没规定）的最高噪声块。
/// 再往下调就把它放进 trgm 的 rank1（1/61，与向量路 rank1 等权），
/// 等于给一道必须回答「没有」的题递一块看着很像的正文。
///
/// 判据块与噪声块的两条带在这份语料上**是重叠的**（判据低到 0.1481，噪声高到 0.3333），
/// 所以别指望存在一个「完美阈值」：trgm 是 RRF 的加分项，不是承重路。
/// KB10 的判据块恰好 0.2000（`>` 是严格的，差一点点被排除）—— 无害，它的向量路排第 1。
const TRGM_MIN: f32 = 0.2;

/// 标题/文件名是一条高精度辅助路，阈值刻意高于正文 trgm，避免“制度/说明”等泛文件名抢占结果。
const TITLE_MIN: f32 = 0.35;
/// 标签/业务域/来源元数据只做高精度加分，不能替代正文证据。
const METADATA_MIN: f32 = 0.35;

/// 向量路的**相关度下限**：余弦距离上限（`<=>` 值域 [0,2]，越小越像）。
///
/// 🔴 为什么必须有：没有它 HNSW 恒返 `VEC_TOP` 条，问「库里根本没有的事」照样有 6 块进 prompt，
/// 「会不会编」全押在模型肯不肯说「没有」上。知识库**答错比答「没有」坏得多**，
/// 无命中时宁可少给也不给噪声。
///
/// 按实测选（同一趟，数值见 `VEC_MEASURED`）：判据块的距离 0.1863～**0.4926**
/// （最远那个是 KB06 员工台账 CSV 的表格块，次远是 KB15 的 txt 那份 0.4201），
/// 而 KB07「月球基地建设进度」这道**远域** nohit 最近的块也要 **0.6020**。
/// 0.55 取 [0.4926, 0.6020] 的中点，两边各留 ~0.05。
///
/// 实测效果（同一份语料，改前 → 改后）：KB07 **6 块 → 0 块** ——
/// `search` 返空 → `respond` 不调 LLM 直接回「没有」（省一次 Precise 调用，且不再赌模型的判断）；
/// KB06 6 块 → 1 块（改前那 6 块里 5 块来自另外 3 篇无关文档）；KB08 6 → 3；
/// 而 14 道题的判据载体块**一个都没掉出** prompt。
///
/// 🔴 它做不到的事，别再试：**近域** nohit 分不出来。KB13「差旅打车费每天限额多少」
/// 最近的块只有 **0.3395** —— 比上面 10 个判据块都近。任何能挡住它的下限都会打死一半正向题。
/// 那道题该由 `answer::keep_cited_only`（无角标即无结论）兜，不是由距离兜。
///
/// 这是**标定值不是常数**：换 embedding 模型或换语料域必须重量一遍再改
/// （量法：逐题取 `embedding <=> 问句向量` 全表分布，找「判据块最远的那个」与
/// 「远域 nohit 最近的那个」之间的缝）。
///
/// 🔴 **2026-08-17 重量**（向量层已换千问 `text-embedding-v4` @1024，上面那组数字是
/// bge-small-zh/512 时代、14 篇本地夹具上量的，已作废但留作对照）。
/// 这次用 56 题**生产语料**基准（`tools/kb_fixtures/prod_cases.json`）：
///
/// | | 距离 |
/// |---|---|
/// | 判据块 最近 / 中位 / P90 / **最远** | 0.1322 / 0.2528 / 0.3618 / **0.5004** |
/// | 远域 nohit 最近块（月球基地/给猫绝育/量子纠错/世界杯比分/光伏 MPPT 五题） | **0.6149** |
///
/// 缝是 [0.5004, 0.6149]，中点 0.5577 —— **0.55 仍然落在缝里**，
/// 判据块一个不掉、远域一个不进。换了模型换了维度它还成立，是运气也是结论：
/// 这个数不用改，但**必须重量过才知道不用改**。
///
/// `VEC_CALIBRATED_ON` 把这次标定的上下文钉住：模型或维度一变，
/// `the_vector_threshold_records_what_it_was_calibrated_on` 当场红 ——
/// 逼下一手重量一遍，而不是让一个作废的阈值继续无声地当门。
const VEC_MAX_DIST: f64 = 0.55;

/// `VEC_MAX_DIST` 是在**哪个向量空间**上量出来的。形状 `模型@维度`。
/// 阈值本身是个数，看不出它已经作废 —— 这一行就是它的保质期标签。
const VEC_CALIBRATED_ON: &str = "text-embedding-v4@1024";
/// 相邻块合并时的正文分隔
const JOIN: &str = "\n\n";

/// 检索前的保守归一化：统一全角 ASCII/空白/大小写，去掉纯排版标点与常见礼貌前缀。
/// 型号里的 `-_.` 会保留；不做同义词改写，避免把用户原意改坏。
fn normalize_query(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut gap = false;
    for raw in input.trim().chars() {
        let ch = match raw {
            '\u{3000}' => ' ',
            '\u{ff01}'..='\u{ff5e}' => char::from_u32(raw as u32 - 0xfee0).unwrap_or(raw),
            _ => raw,
        };
        if ch.is_control() {
            continue;
        }
        if ch.is_whitespace()
            || matches!(
                ch,
                ',' | ';' | ':' | '!' | '?' | '/' | '\\' | '|' | '，' | '。' | '；' | '：'
                    | '！' | '？' | '、' | '“' | '”' | '‘' | '’' | '《' | '》' | '〈' | '〉'
                    | '【' | '】' | '[' | ']' | '(' | ')' | '（' | '）' | '{' | '}'
            )
        {
            gap = !out.is_empty();
            continue;
        }
        if gap {
            out.push(' ');
            gap = false;
        }
        out.extend(ch.to_lowercase());
    }
    // gap 逻辑保证 out 无首尾空白（前导分隔符不会写入、尾随分隔符只置旗不推字符），
    // 不需要再 trim（白分配一次 String）
    let mut q = out;
    for prefix in ["麻烦帮我查询一下", "麻烦帮我查一下", "帮我查询一下", "帮我查一下", "请问一下", "请问"] {
        if let Some(rest) = q.strip_prefix(prefix) {
            let rest = rest.trim();
            if !rest.is_empty() {
                q = rest.to_string();
                break;
            }
        }
    }
    q
}

/// 一条命中。`chunk_id` 是引用回查的锚点（相邻合并后取首块，见 `merge_adjacent`）。
#[derive(Debug, Clone)]
pub struct Hit {
    pub chunk_id: i64,
    pub doc_id: String,
    pub doc_name: String,
    pub folder_id: Option<String>,
    pub folder_path: String,
    pub ord: i32,
    pub text: String,
    pub heading_path: String,
    pub page: Option<i32>,
    pub tags: Vec<String>,
    pub business_domain: Option<String>,
    pub effective_from: Option<String>,
    pub effective_to: Option<String>,
    pub source_uri: Option<String>,
    pub document_family: Option<String>,
    pub document_revision: Option<String>,
    pub source_hash: String,
    pub doc_updated_at: String,
    /// 本条候选被哪些召回通道命中；只用于解释，不参与二次打分。
    pub channels: Vec<String>,
    /// 与直接命中文档的结构关系；稳定代码供前端分组，不参与事实引用授权。
    pub relations: Vec<String>,
    pub score: f32,
    /// 本条命中由多少个连续块合并而成（`merge_adjacent` 填；SQL 取出来时恒 1）。
    /// 引用要靠它才能被忠实回查 —— 见 `dms_kernel::Citation::span`。
    pub merged: u32,
    /// 与问句的向量距离（`embedding <=> query`，越小越近）。**只做并列时的次序键**，
    /// 不参与任何阈值判定、不进 RRF 主分、不出现在 wire 上。
    ///
    /// 🔴 为什么需要它（2026-08-16 度量）：`recall@6 = 0.95` 而 `recall@1 = 0.15` ——
    /// 对的块召回得到、就是排不到第一。0.80 的缺口 100% 在排序层。
    /// 而 `merge_adjacent` 的收尾排序此前是 `score desc → chunk_id asc`：
    /// RRF 同分时**由入库顺序决胜**，用户看到的第一条引用与相关性无关。
    /// 典型同分形态：A 只被向量路 rank1 命中、B 只被词级路 rank1 命中，两者都是 `w/61`。
    ///
    /// 为什么用向量距离而不是「每路各自归一分取最大」：后者是**跨路比较**，
    /// 而各路分的量纲完全不同（元数据路命中标签时 sim 恒等于 1.0、向量强命中只有 0.45），
    /// 得出的偏好序会与本文件用字面量钉死的权重序**相反** —— 那是「同一件事两份口径
    /// 各自演化」的同构体。向量距离是**单一量纲、零标定**：对每个候选块都可算，
    /// 而且候选集加载时那张表就在手上。
    ///
    /// `None` = 这块没有向量（外部 KB 的合成块、或 embed 服务降级）：排在有距离的之后，
    /// 再按 `chunk_id` 兜底 —— 可复现性一个字没松。
    pub vec_dist: Option<f32>,
}

/// 一次检索的可观测数据。数值直接来自六路内容召回、一路结构扩展、一路图谱召回和一路外部 KB 召回，不参与阈值判定。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchStats {
    pub visible_docs: usize,
    pub vector_candidates: usize,
    /// 精确 token 路的候选数（KB 审查③起是单号/型号 ILIKE 那一路；字段名保留是
    /// server `kb_api` 诊断端点的既有 JSON 键，改名即破约）。
    pub fts_candidates: usize,
    pub trgm_candidates: usize,
    pub title_candidates: usize,
    pub metadata_candidates: usize,
    pub relation_candidates: usize,
    pub kg_candidates: usize,
    pub ext_kb_candidates: usize,
    /// 词级稀疏召回路（第 9 路）的候选数。kb_api 诊断 JSON 的键集是既有契约，
    /// 本字段只进服务端日志/评测，不加那边（那个文件本轮不在改动范围）。
    pub terms_candidates: usize,
    pub fused_candidates: usize,
}

/// 检索结果与诊断信息。管理端用它解释“为什么命中/为什么没命中”；问答链仍走兼容入口。
#[derive(Debug, Clone)]
pub struct SearchReport {
    pub normalized_query: String,
    pub hits: Vec<Hit>,
    pub vector_degraded: bool,
    pub stats: SearchStats,
}

/// 手写 `FromRow`（workspace 的 sqlx 没开 `derive` feature）。
/// `score` 不来自 SQL——它是 RRF 融合分，由 `search` 回填。
impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for Hit {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            chunk_id: row.try_get("chunk_id")?,
            doc_id: row.try_get("doc_id")?,
            doc_name: row.try_get("doc_name")?,
            folder_id: row.try_get("folder_id")?,
            folder_path: row.try_get("folder_path")?,
            ord: row.try_get("ord")?,
            text: row.try_get("text")?,
            heading_path: row.try_get("heading_path")?,
            page: row.try_get("page")?,
            tags: row.try_get("tags")?,
            business_domain: row.try_get("business_domain")?,
            effective_from: row.try_get("effective_from")?,
            effective_to: row.try_get("effective_to")?,
            source_uri: row.try_get("source_uri")?,
            document_family: row.try_get("document_family")?,
            document_revision: row.try_get("document_revision")?,
            source_hash: row.try_get("source_hash")?,
            doc_updated_at: row.try_get("doc_updated_at")?,
            channels: Vec::new(),
            relations: Vec::new(),
            score: 0.0,
            merged: 1,
            // SQL 不取向量距离：候选集定下来之后统一回填一次（见 `fill_vec_dist`）
            vec_dist: None,
        })
    }
}

/// 块正文 + 所属文档名的取列口径（`Hit::from_row` 按名取列，两处必须一致）。
/// 是宏不是 `const`：两个调用点要在编译期 `concat!` 出各自的 `&'static str`。
macro_rules! hit_select {
    () => {
        "SELECT c.chunk_id, c.doc_id, d.name AS doc_name,d.folder_id,d.folder_path,c.ord,c.text, \
                c.heading_path, c.page, d.tags, d.business_domain, \
                d.effective_from::text AS effective_from, \
                d.effective_to::text AS effective_to, d.source_uri, \
                d.document_family, d.document_revision, \
                d.sha256 AS source_hash, d.updated_at::text AS doc_updated_at \
         FROM kb.chunk c JOIN kb.doc d ON d.doc_id = c.doc_id"
    };
}

/// 混合检索。`space = None` 表示不限空间（全部可见文档）。
/// 丢掉降级旗的版本：MCP 的 `kb_search` 只列命中，不需要那面旗。
/// `weights` = RRF 四路辅助召回权重（settings 快照；调用方负责取，默认用 `RrfWeights::default()`）。
pub async fn search(
    store: &OwnedStore,
    embed: &EmbedClient,
    v: &Viewer,
    space: Option<&str>,
    question: &str,
    weights: &RrfWeights,
) -> Result<Vec<Hit>, KbError> {
    Ok(search_with_status(store, embed, v, space, question, weights).await?.0)
}

/// `search` + **向量路是否缺席**（第二项 true = 缺席）。
/// 分两个入口只为不动 `mcp_api::kb_search` 的签名；回答层只把降级记入服务端诊断。
pub async fn search_with_status(
    store: &OwnedStore,
    embed: &EmbedClient,
    v: &Viewer,
    space: Option<&str>,
    question: &str,
    weights: &RrfWeights,
) -> Result<(Vec<Hit>, bool), KbError> {
    let report = search_report(store, embed, v, space, question, weights).await?;
    Ok((report.hits, report.vector_degraded))
}

/// 与 `search_with_status` 相同的检索，额外返回 `normalized_query`、`vector_degraded`
/// 与各路召回的候选数量（`SearchStats`，只观测、不参与阈值判定）。
pub async fn search_report(
    store: &OwnedStore,
    embed: &EmbedClient,
    v: &Viewer,
    space: Option<&str>,
    question: &str,
    weights: &RrfWeights,
) -> Result<SearchReport, KbError> {
    let query = normalize_query(question);
    if query.is_empty() {
        return Ok(SearchReport {
            normalized_query: query,
            hits: Vec::new(),
            vector_degraded: false,
            stats: SearchStats::default(),
        });
    }
    let docs = visible_docs(store, v, space).await?;
    let visible_n = docs.len();
    // 可见集合为空 → 一条检索查询都不发（`scan_mode` 返 None）。
    // 这里不算降级：一篇可见文档都没有时「没有相关内容」是实话。
    let Some(scan) = scan_mode(visible_n) else {
        // 🔴 这条早退**必须吼一声**（2026-08-14）：它在九路召回之前，
        // 于是下面那句「检索零命中：各路召回数」永远走不到 —— 一个可见文档都没有时，
        // 服务端日志里**一行痕迹都没有**，而用户侧看到的是每一问都「没有相关内容」。
        // 排查时最贵的正是这一步：分不清「库里没有」「权限看不到」「状态没就绪」。
        // 三个变量一起打：谁在问、问的哪个空间、可见集为什么是 0 只能靠这三样定位。
        tracing::warn!(
            login = %v.login,
            roles = ?v.roles,
            space = space.unwrap_or("*"),
            "知识库可见文档为 0 → 不发任何召回查询，本次必然「没有相关内容」             （查 kb.doc 的 enabled/status/生效期，以及 kb.acl / kb.user_dept 的授权行）"
        );
        return Ok(SearchReport {
            normalized_query: query,
            hits: Vec::new(),
            vector_degraded: false,
            stats: SearchStats { visible_docs: visible_n, ..SearchStats::default() },
        });
    };
    // 权重数组取一次（两个融合点共享同一份快照，不重复构造）
    let routes = weights.route_array();
    // 五路彼此独立：向量 HTTP 与四条 PG 查询并行，保持原阈值/排序不变，只去掉固定串行等待。
    let vector = async {
        match embed.embed_query(&query).await {
            // 🔴 问句向量要**留下来**（2026-08-16）：候选集定下来之后拿它回填每块的语义距离，
            // 当 RRF 同分时的次序键（见 `Hit::vec_dist`）。此前它算完就丢，
            // 于是 rank-1 并列只能按 `chunk_id`（入库顺序）决胜。
            Some(qv) => {
                let pg = to_pgvector(&qv);
                Ok((vector_ids(store, &docs, &pg, scan).await?, false, Some(pg)))
            }
            // 🔴 静默跳过这一路是本文件最贵的一处沉默。剩下路线仍可答，但必须显式降级。
            None => {
                tracing::warn!(
                    space = space.unwrap_or("<all>"),
                    "向量检索不可用（embed 服务挂或熔断中）→ 本次只剩精确匹配/正文相似/标题/元数据路，可能漏检"
                );
                Ok::<_, KbError>((Vec::new(), true, None))
            }
        }
    };
    // 第 8 路外部只读 KB（B9）也是一条独立 HTTP，与上面六路同批发出（不占额外尾延迟）；
    // 未配置时 `from_env` 为 None → 这一路恒空、零 IO，与接入前逐字节一致。
    let ext_client = ExtKbClient::from_env();
    let (vector, exact, trgm, title, metadata, terms, ext_kb) = tokio::join!(
        vector,
        exact_ids(store, &docs, &query),
        trgm_ids(store, &docs, &query),
        title_ids(store, &docs, &query),
        metadata_ids(store, &docs, &query),
        terms_ids(store, &docs, &query, terms_route_enabled()),
        ext_kb_route(ext_client.as_ref(), &query),
    );
    let (vec_ids, vec_down, query_vector) = vector?;
    let terms = terms?;
    // 固定五个槽位，向量降级时也保留空 vec：诊断日志不会把精确路数量误记成向量数量。
    let mut lists = vec![vec_ids, exact?, trgm?, title?, metadata?];
    // 槽位序 = 「向量/精确/正文/标题 | 元数据」：push 序就是语义序，`auxiliary[0]` 恒为
    // 元数据路——在 metadata 前插一路会静默指错，先钉死。
    // 词级路（第 9 路）的槽位钉死在**末尾**（见下方 push 序）：CHANNEL_NAMES 前八槽被
    // 既有测试逐字钉住，插位等于改旧路语义；它参与「直接命中」靠下方显式并行数组。
    debug_assert_eq!(lists.len(), 5, "lists 槽位序变了，auxiliary[0] 不再是元数据路");
    let (direct, auxiliary) = lists.split_at_mut(4);
    // 直接命中 = 前四路 + 词级路：元数据票只许落在直接命中上（词级是正文词命中，同属直接命中）
    keep_auxiliary_votes_on_direct_hits(
        &[&direct[0], &direct[1], &direct[2], &direct[3], &terms],
        &mut auxiliary[0],
    );
    // 第一遍融合：五路直接命中 + 元数据票。词级路的 lists 槽位在末尾，这里按显式并行
    // 数组喂（first_pass 路序与 first_weights 一一对应）；词级路为空时与接入前逐字节等价。
    let first_weights = [routes[0], routes[1], routes[2], routes[3], routes[4], TERMS_WEIGHT];
    let mut first_pass = lists.clone();
    first_pass.push(terms.clone());
    let direct_ranked = rrf_weighted(&first_pass, &first_weights);
    let direct_ids: Vec<i64> = direct_ranked.iter().take(CANDIDATE_K).map(|(id, _)| *id).collect();
    // 第 6/7 路互不依赖（关系扩展吃直接命中，图谱吃向量路种子），并行发出。
    // 图谱路永不返 Err：图没建 / 查询失败 / env 关闭都是「这一路缺席」—— 降级 warn 留痕走原路，
    // 绝不允许它挂掉检索（与 rerank 同一条纪律）。
    let (related, kg) = tokio::join!(
        async {
            if direct_ids.is_empty() {
                Ok(Vec::new())
            } else {
                relation_candidates(store, v, space, &direct_ids, &query).await
            }
        },
        kg_route(store.pool(), space, kg_retrieval_enabled(), &docs, &query, &lists[0]),
    );
    let related = related?;
    lists.push(related.iter().map(|r| r.chunk_id).collect());
    // 🔴 图谱是加分项，不是承重路（2026-08-16）：一条正文直接命中都没有时，
    // 它的 PPR top-10 就是无锚噪声 —— `kg_top_chunks` 没有任何相关度下限，
    // 只按图上的邻近度取前 K。与上面关系路的锚**逐字同一条**。
    //
    // 由来：纯数据问句下面挂一段「知识库里没有关于…」外加一篇无关手册。
    // 五条正文路（向量/全文/标题/元数据/词级）全空 ⇒ 按定义没有任何正文证据，
    // 而这两路仍往 `ids` 里塞候选，于是零命中早退（本函数下方那条不变量）走不到。
    lists.push(if direct_ids.is_empty() { Vec::new() } else { kg });
    // 第 8 路（Yuxi B9）：远程文本块没有本地 chunk 行，合成**负 id** 进 RRF
    // （本地 chunk_id 是 bigserial 恒正，负数永不撞；id 序即路内名次）。
    // 记录留在旁表 `ext_kb_map`，加载阶段按它构造只读 Hit。
    // 同上：合成负 id 永远进不了任何正文路，结构上不受直接命中约束 ——
    // 没有正文锚时这一路等于「拿远程库的 top-k 当答案」，正是本次要治的噪声源。
    let ext_kb_map: Vec<(i64, ExtKbRecord)> = if direct_ids.is_empty() {
        Vec::new()
    } else {
        ext_kb
            .into_iter()
            .enumerate()
            .map(|(i, record)| (ext_kb_synthetic_id(i), record))
            .collect()
    };
    lists.push(ext_kb_map.iter().map(|(id, _)| *id).collect());
    // 第 9 路词级召回：槽位**钉死在末尾**（前八槽的通道名被既有测试逐字钉住）。
    lists.push(terms);

    // 词级路权重是编译期字面量（TERMS_WEIGHT，与四路正文同钉死），不进 settings：
    // 「辅助路不许压过正文路」与「正文直接命中恒 1.0」都由字面量守，配置够不着。
    let mut weights9 = [0.0f32; 9];
    weights9[..8].copy_from_slice(&routes);
    weights9[8] = TERMS_WEIGHT;
    let ranked = rrf_weighted(&lists, &weights9);
    debug_assert_eq!(lists.len(), 9, "lists 槽位序变了，下方 stats/通道名按硬下标取数会错位");
    let stats = SearchStats {
        visible_docs: visible_n,
        vector_candidates: lists[0].len(),
        fts_candidates: lists[1].len(),
        trgm_candidates: lists[2].len(),
        title_candidates: lists[3].len(),
        metadata_candidates: lists[4].len(),
        relation_candidates: lists[5].len(),
        kg_candidates: lists[6].len(),
        ext_kb_candidates: lists[7].len(),
        terms_candidates: lists[8].len(),
        fused_candidates: ranked.len(),
    };
    let mut ids: Vec<i64> = ranked.iter().take(CANDIDATE_K).map(|(id, _)| *id).collect();
    let review_ids: Vec<i64> = related
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.relation.as_str(),
                "document_family" | "document_revision" | "references" | "referenced_by"
            )
        })
        .map(|candidate| candidate.chunk_id)
        .collect();
    // 同族版本与显式文档关系即使分数较低也要加载；它们仍只有低权重，不能压过正文命中。
    for chunk_id in &review_ids {
        if !ids.contains(chunk_id) {
            ids.push(*chunk_id);
        }
    }
    if ids.is_empty() {
        // 🔴 **零命中必须说清是哪一种**：三种原因长一样（`hits` 空），处置完全不同 —
        //   ① 向量路缺席（`vec_down`）：embed 挂了/熔断中，用户看到的是「没有」其实是降级；
        //   ② 各路都执行但 RRF 后一条不剩（阈值过滤掉的）：相关度下限把它们挡住了，
        //      处置是**降阈值**（而①不该动阈值）；
        //   ③ 各路都空（`lists` 里每条都是空）：真没有，处置是告诉用户补文档。
        // ⚠️ 本分支里②与③同形态、无法区分：能进这里 ⇒ ranked 空 ⇒ 八路 list 全空
        // （RRF 不过滤候选，阈值过滤发生在各路 SQL 内），所以②在这份日志里永远观测不到，
        // 各路计数结构性恒零——stats 字段照记（含 relation 路），只为形状完整。
        tracing::info!(
            space = space.unwrap_or("<all>"),
            vec_down,
            vec = stats.vector_candidates,
            exact = stats.fts_candidates,
            trgm = stats.trgm_candidates,
            title = stats.title_candidates,
            metadata = stats.metadata_candidates,
            relation = stats.relation_candidates,
            kg = stats.kg_candidates,
            ext_kb = stats.ext_kb_candidates,
            terms = stats.terms_candidates,
            merged = stats.fused_candidates,
            "检索零命中：各路召回数（vec=向量 exact=单号/型号精确 trgm=正文相似 title=标题/文件名 metadata=元数据 relation=结构关联 kg=图谱 ext_kb=外部知识库 terms=词级 merged=RRF 后）"
        );
        return Ok(SearchReport {
            normalized_query: query,
            hits: Vec::new(),
            vector_degraded: vec_down,
            stats,
        });
    }
    let mut hits = load_hits(store, v, space, &ids, &review_ids).await?;
    // 外部路候选无本地行可载：只为进了候选名单（ids）的合成 id 构造只读 Hit
    // （`source_uri` 标注来源 —— 外部 KB 是独立授权源，配置即授权，来源必须能被用户看穿）。
    for (ord, (synthetic_id, record)) in ext_kb_map.iter().enumerate() {
        if ids.contains(synthetic_id) {
            hits.push(ext_kb_hit(*synthetic_id, ord as i32, record));
        }
    }
    // 每 hit 一次 `ranked.iter().find` / 8 路 `contains` 是 O(hits×fused) 的重复扫：
    // 融合分与通道名各离线建一次映射，循环内查表
    let score_of: HashMap<i64, f32> = ranked.iter().map(|(id, s)| (*id, *s)).collect();
    let mut channel_map: HashMap<i64, Vec<&'static str>> = HashMap::new();
    for (route, name) in lists.iter().zip(CHANNEL_NAMES) {
        for id in route {
            channel_map.entry(*id).or_default().push(name);
        }
    }
    // RRF 同分时的次序键：一条 SQL、只打本次候选集（向量降级时整步跳过，行为回到旧的
    // 「按 chunk_id 兜底」，不额外报错）。
    let dist_of = match &query_vector {
        Some(pg) => chunk_distances(store, pg, &ids).await,
        None => HashMap::new(),
    };
    for h in &mut hits {
        h.score = score_of.get(&h.chunk_id).copied().unwrap_or(0.0);
        h.vec_dist = dist_of.get(&h.chunk_id).copied();
        h.channels = channel_map
            .get(&h.chunk_id)
            .map(|names| names.iter().map(|n| n.to_string()).collect())
            .unwrap_or_default();
        h.relations = related
            .iter()
            .filter(|r| r.chunk_id == h.chunk_id)
            .map(|r| r.relation.clone())
            .collect();
    }
    // rerank（B5）只在 `DMS_RERANK_*` 配齐时插入；未配置时走原来的 `finalize_hits`，一字不差。
    // 每次检索读一次 env：本函数签名被 server 各调用点冻结，配置只能走环境变量（3 次 getenv 可忽略）。
    let hits = match RerankClient::from_env() {
        None => {
            // 🔴 关着必须留痕（2026-08-16）：精排是唯一专治「召回到了但排不到第一」的组件，
            // 而它在生产上**一次都没跑过**（`DMS_RERANK_BASE_URL` / `DMS_RERANK_MODEL`
            // 从未进过 docker run 的 --env，全仓也没有任何 rerank 服务）。
            // 不留痕的后果不是少一点效果，是**结论被污染**：任何「精排有没有收益」的
            // 实验测出来必然是「没收益」，然后这条链会被当成无用能力删掉。
            tracing::debug!(
                "rerank 未接线（DMS_RERANK_BASE_URL / DMS_RERANK_MODEL 未配）→ 本次只有 RRF 排序；                 recall@1 的头寸全在这条链上，别拿本次结果去判「精排无收益」"
            );
            finalize_hits(hits)
        }
        Some(client) => finalize_ranked(rerank_candidates(&client, &query, rank_hits(hits)).await),
    };
    Ok(SearchReport {
        normalized_query: query,
        hits,
        vector_degraded: vec_down,
        stats,
    })
}

/// `window()` 上下文窗口的硬上限（±块）：人工浏览用，±3 够了
const CITATION_WINDOW_MAX: i32 = 3;

/// 引用原文回查：`chunk_id` 前后各 `w` 块（同文档内）。
/// 锚点查询本身内联 ACL、启用状态，正文加载时再重放一次；历史版本也必须可核对。
/// 不存在与不可见统一报 `Forbidden`，不给他人文档的存在性做探针。
pub async fn window(
    store: &OwnedStore,
    v: &Viewer,
    chunk_id: i64,
    w: i32,
) -> Result<Vec<Hit>, KbError> {
    let (doc_id, ord) = citation_anchor(store, v, chunk_id).await?;
    let w = w.clamp(0, CITATION_WINDOW_MAX);
    citation_hits_for_anchor(store, v, chunk_id, &doc_id, ord - w, ord + w).await
}

/// 引用回查（按**合并跨度**）：从 `chunk_id` 起连续 `span` 块，正是 `Citation.span` 那一段。
///
/// 与 `window` 并存而不是替换它：`window` 是「上下文各看几块」（人工浏览用，±3 够了），
/// 本函数是「把模型看到的那一条命中原样取回」（核对用，长度由检索时的合并决定）。
/// 用 `window` 冒充它会漏 —— 合并跨度可以是 5 块，而 `window` 被 `clamp(0,3)` 钉死。
///
/// `span` 上限 = `MAX_MERGE_SPAN`：检索合并同样受它约束，而无上限的取数口是个 DoS 面。
pub async fn span(
    store: &OwnedStore,
    v: &Viewer,
    chunk_id: i64,
    span: u32,
) -> Result<Vec<Hit>, KbError> {
    let (doc_id, ord) = citation_anchor(store, v, chunk_id).await?;
    let n = span.clamp(1, MAX_MERGE_SPAN) as i32;
    citation_hits_for_anchor(store, v, chunk_id, &doc_id, ord, ord + n - 1).await
}

/// 锚点查询（`window`/`span` 共用）：返回 (doc_id, ord)；不存在与不可见统一 `Forbidden`。
async fn citation_anchor(store: &OwnedStore, v: &Viewer, chunk_id: i64) -> Result<(String, i32), KbError> {
    store
        .fixed(citation_anchor_sql())
        .bind(&v.login)
        .bind(&v.roles)
        .bind(chunk_id)
        .fetch_optional::<(String, i32)>()
        .await?
        .ok_or_else(|| KbError::Forbidden("引用当前不可见".into()))
}

fn citation_anchor_sql() -> &'static str {
    concat!(
        "SELECT c.doc_id,c.ord FROM kb.chunk c JOIN kb.doc d ON d.doc_id=c.doc_id \
         WHERE c.chunk_id=$3 AND d.enabled=true AND d.status IN ('chunked','embedded') \
           AND d.doc_id IN (",
        crate::acl::visible_docs!(),
        ")"
    )
}

fn citation_hits_sql() -> &'static str {
    concat!(
        hit_select!(),
        " WHERE c.doc_id=$3 AND c.ord BETWEEN $4 AND $5 \
           AND d.enabled=true AND d.status IN ('chunked','embedded') \
           AND d.doc_id IN (",
        crate::acl::visible_docs!(),
        ") ORDER BY c.ord"
    )
}

async fn citation_hits(
    store: &OwnedStore,
    v: &Viewer,
    doc_id: &str,
    from: i32,
    to: i32,
) -> Result<Vec<Hit>, KbError> {
    let hits = store
        .fixed(citation_hits_sql())
        .bind(&v.login)
        .bind(&v.roles)
        .bind(doc_id)
        .bind(from)
        .bind(to)
        .fetch_all()
        .await?;
    if hits.is_empty() {
        return Err(KbError::Forbidden("引用当前不可见".into()));
    }
    Ok(hits)
}

/// 锚点与正文分两次读取；若两次之间文档被重建，范围内可能仍残留其他块。
/// 此时不能拿邻块冒充原引用，统一按“当前不可见/已失效”处理。
async fn citation_hits_for_anchor(
    store: &OwnedStore,
    v: &Viewer,
    chunk_id: i64,
    doc_id: &str,
    from: i32,
    to: i32,
) -> Result<Vec<Hit>, KbError> {
    let hits = citation_hits(store, v, doc_id, from, to).await?;
    if !hits.iter().any(|hit| hit.chunk_id == chunk_id) {
        return Err(KbError::Forbidden("引用当前不可见".into()));
    }
    Ok(hits)
}

/// 可见 doc 清单。ACL 片段（`$1` login / `$2` 角色码）原样内联，空间过滤走 `$3`（NULL = 不限）。
/// 编译期 `concat!`：拼进去的只有本 crate 的两条字面量，运行时输入一个字都没有。
fn visible_sql() -> &'static str {
    concat!(
        "SELECT x.doc_id FROM kb.doc x WHERE x.enabled=true \
         AND x.status IN ('chunked','embedded') \
         AND EXISTS (SELECT 1 FROM kb.chunk xc WHERE xc.doc_id=x.doc_id) \
         AND (x.effective_from IS NULL OR x.effective_from <= CURRENT_DATE) \
         AND (x.effective_to IS NULL OR x.effective_to >= CURRENT_DATE) \
         AND x.doc_id IN (",
        crate::acl::visible_docs!(),
        ") AND ($3::text IS NULL OR x.space_id = $3)"
    )
}

async fn visible_docs(
    store: &OwnedStore,
    v: &Viewer,
    space: Option<&str>,
) -> Result<Vec<String>, KbError> {
    Ok(store
        .fixed(visible_sql())
        .bind(&v.login)
        .bind(&v.roles)
        .bind(space)
        .fetch_all::<(String,)>()
        .await?
        .into_iter()
        .map(|(id,)| id)
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    /// 关掉本事务的 index scan：规划器改走「过滤 + 排序」，召回的是可见集合内的真最近邻
    Exact,
    /// 让 HNSW 索引干活（可见集合大到精确扫描不划算时）
    Hnsw,
}

/// 可见集合为空 → `None`（调用方早退）。少于 50 篇走精确扫描：
/// HNSW 先取全局最近邻、再按 ACL 过滤，小可见集合下前 20 个邻居可能整批属于别人 → 滤完剩零条。
fn scan_mode(visible: usize) -> Option<Scan> {
    match visible {
        0 => None,
        n if n < EXACT_SCAN_DOCS => Some(Scan::Exact),
        _ => Some(Scan::Hnsw),
    }
}

/// 相关度下限（`$4`）写在 WHERE 里而不是取回来再滤：滤掉的行本来就不该占 `LIMIT` 的名额。
/// 它**不会**破坏下面 `Scan::Exact` 那个 `+ 0` 的用意 —— HNSW 只能服务 `ORDER BY`，
/// 距离上的范围条件任何时候都是过滤，不是索引扫描。
const VEC_SQL: &str = "SELECT chunk_id FROM kb.chunk \
                       WHERE doc_id = ANY($1::text[]) AND embedding IS NOT NULL \
                         AND (embedding <=> $2::vector) < $4 \
                       ORDER BY embedding <=> $2::vector, chunk_id LIMIT $3";

/// `Scan::Exact` 版：`+ 0` 让排序表达式不再匹配 HNSW 的索引形态，规划器只能「过滤 + 排序」，
/// 于是召回的是**可见集合内**的真最近邻（这正是 `scan_mode` 要的东西）。
///
/// `ponytail:` 原实现是 `BEGIN; SET LOCAL enable_indexscan = off; …` —— GUC 只在事务里生效，
/// 而字面量通道刻意不导出裸池、也没有事务句柄。天花板：这依赖「PG 不折叠 `x + 0`」
/// （它确实不折叠 float8 加法，那会改变溢出/精度语义）。哪天要更硬的保证，
/// 就给 `OwnedStore` 加一个只吃 `&'static str` 的事务版 `fixed`，再换回 `SET LOCAL`。
const VEC_SQL_EXACT: &str = "SELECT chunk_id FROM kb.chunk \
                             WHERE doc_id = ANY($1::text[]) AND embedding IS NOT NULL \
                               AND (embedding <=> $2::vector) < $4 \
                             ORDER BY (embedding <=> $2::vector) + 0, chunk_id LIMIT $3";

async fn vector_ids(
    store: &OwnedStore,
    docs: &[String],
    vlit: &str,
    scan: Scan,
) -> Result<Vec<i64>, KbError> {
    let sql = match scan {
        Scan::Hnsw => VEC_SQL,
        Scan::Exact => VEC_SQL_EXACT,
    };
    ids(store.fixed(sql).bind(docs).bind(vlit).bind(VEC_TOP).bind(VEC_MAX_DIST)).await
}

/// 无编号精确匹配路（KB 审查③，替换原中文 FTS 路）：`plainto_tsquery('simple', …)` 对连写中文
/// 切不出词，实测 14 题 × 23 块 322 格 `ts_rank_cd` 恒 0（见 `TRGM_MIN` 的实测注释）——
/// 那一路从没产出过候选，名额让给「单号/型号精确包含」：从归一化问句抽 `[A-Za-z0-9-]{6,}`
/// 的 token 做正文 ILIKE。权重与标题路同级（1.0，RRF 第二槽）；问单号时一路直达。
/// 路内排序：命中 token 数多的块在前（多号并问时兼中者优先），同数按 chunk_id（可复现）。
const EXACT_SQL: &str = "SELECT chunk_id FROM kb.chunk c \
                         WHERE c.doc_id = ANY($1::text[]) AND c.text ILIKE ANY($2::text[]) \
                         ORDER BY (SELECT count(*) FROM unnest($2::text[]) AS t(p) \
                                   WHERE c.text ILIKE t.p) DESC, c.chunk_id \
                         LIMIT $3";

/// 精确路一次参与的 token 上限（防爆闸；正常问句一两个）。
const EXACT_MAX_TOKENS: usize = 8;
/// 精确路 token 的长度门槛（`{6,}`）：短于它的 ASCII 串不是单号/型号，扫了也是噪声
const EXACT_TOKEN_MIN: usize = 6;

/// 精确路的 token 抽取：`[A-Za-z0-9-]{6,}`（单号/型号/编号）。全仓无 regex 依赖，手写扫描。
/// 调用方传入的是归一化问句（已小写化、按空白/切段）；token 字符集不含 `%`/`_`，
/// 拼 ILIKE 模式串无需转义。去重保序，超闸截断。
fn exact_tokens(q: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if cur.len() >= EXACT_TOKEN_MIN && !out.contains(cur) {
            out.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    for ch in q.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            cur.push(ch);
        } else {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out.truncate(EXACT_MAX_TOKENS);
    out
}

async fn exact_ids(store: &OwnedStore, docs: &[String], q: &str) -> Result<Vec<i64>, KbError> {
    let tokens = exact_tokens(q);
    if tokens.is_empty() {
        return Ok(Vec::new()); // 没有 ≥6 位 ASCII token：省一次可见块全扫（纯中文问句的常态）
    }
    let pats: Vec<String> = tokens.iter().map(|t| format!("%{t}%")).collect();
    ids(store.fixed(EXACT_SQL).bind(docs).bind(&pats).bind(EXACT_TOP)).await
}

/// 各召回路共用的 `Vec<(i64,)> → Vec<i64>`（`fixed()` 通道只给 `FromRow`，标量要自己拆元组）
async fn ids(stmt: dms_connector::fixed::PgStmt<'_>) -> Result<Vec<i64>, KbError> {
    Ok(stmt.fetch_all::<(i64,)>().await?.into_iter().map(|(id,)| id).collect())
}

/// `ponytail:` 顺序扫描可见块（`word_similarity` 不走 GIN）。
/// 要用 `idx_kb_chunk_trgm` 得改 `$2 <% c.text` 并调 `pg_trgm.word_similarity_threshold`
/// （默认 0.6 对「长问句 vs 长正文」偏严）——单空间上万块时再换。
const TRGM_SQL: &str = "SELECT chunk_id FROM kb.chunk \
                        WHERE doc_id = ANY($1::text[]) AND word_similarity($2, text) > $3 \
                        ORDER BY word_similarity($2, text) DESC, chunk_id LIMIT $4";

async fn trgm_ids(store: &OwnedStore, docs: &[String], q: &str) -> Result<Vec<i64>, KbError> {
    ids(store.fixed(TRGM_SQL).bind(docs).bind(q).bind(TRGM_MIN).bind(TRGM_TOP)).await
}

/// 词级稀疏召回（第 9 路）：`terms && $2` 先由 GIN 收口「至少中一词」，`hits >= $3`
/// 再按**不同问句词命中数**过滤；排序 = 命中词数降序、chunk_id 决胜（可复现）。
/// `terms IS NULL`（回填未完成）的行天然不在 GIN 索引里，`&&` 直接不命中 —— 无特判。
const TERMS_SQL: &str = "SELECT chunk_id, hits FROM ( \
                         SELECT c.chunk_id, (SELECT count(*) FROM unnest($2::text[]) AS q(w) \
                                             WHERE q.w = ANY(c.terms)) AS hits \
                         FROM kb.chunk c \
                         WHERE c.doc_id = ANY($1::text[]) AND c.terms && $2::text[] \
                         ) s WHERE hits >= $3 \
                         ORDER BY hits DESC, chunk_id LIMIT $4";

/// 词级路开关（A/B 对照与应急回退用）：`DMS_KB_TERMS=off` 整体关闭，关闭时零 IO，
/// 与接入前逐字节一致 —— 与图谱路 `DMS_KG_RETRIEVAL` 同一个闸形。
fn terms_route_enabled() -> bool {
    terms_route_enabled_env(std::env::var("DMS_KB_TERMS").ok().as_deref())
}

fn terms_route_enabled_env(v: Option<&str>) -> bool {
    !matches!(v, Some(s) if s.trim().eq_ignore_ascii_case("off"))
}

/// 词级路的有效命中词数门：`min(TERMS_MIN_HITS, 问句词数)` —— 问句词数低于门槛时
/// 按问句词数收口，单词问句（「报销」）不因全局门槛变哑。
fn terms_min_hits(query_terms: usize) -> i64 {
    TERMS_MIN_HITS.min(query_terms.max(1) as i64)
}

async fn terms_ids(
    store: &OwnedStore,
    docs: &[String],
    q: &str,
    enabled: bool,
) -> Result<Vec<i64>, KbError> {
    if !enabled {
        return Ok(Vec::new()); // env 关闭：一条查询都不发（闸形与图谱路一致）
    }
    let qterms = crate::store::terms_of(q);
    if qterms.is_empty() {
        // 纯标点/单字问句分不出词：省一次可见块全扫（与 exact_ids 的早退同约）
        return Ok(Vec::new());
    }
    let mut qterms = qterms;
    qterms.truncate(TERMS_MAX_QUERY_TERMS);
    // 问句词数低于门槛时按问句词数收口：单词问句（「报销」）不因全局门槛变哑
    let min_hits = terms_min_hits(qterms.len());
    let rows = store
        .fixed(TERMS_SQL)
        .bind(docs)
        .bind(&qterms)
        .bind(min_hits)
        .bind(TERMS_TOP)
        .fetch_all::<(i64, i64)>()
        .await?;
    // 词级路是标定数据来源：连库评测时按 debug 日志取「问句词 → 各块命中数」分布
    tracing::debug!(query = %q, terms = ?qterms, min_hits, candidates = ?rows, "词级路候选");
    Ok(rows.into_iter().map(|(id, _)| id).collect())
}

/// 标题/文件名辅助召回：每篇文档只给最高相似块一个名额，防止文件名命中后整篇块铺满候选。
/// `doc_id = ANY($1)` 与其他召回路一致，权限仍在可见文档集合内收口。
const TITLE_SQL: &str = "SELECT chunk_id FROM ( \
                         SELECT DISTINCT ON (c.doc_id) c.chunk_id, c.doc_id, \
                           GREATEST(word_similarity($2, d.name), \
                                    word_similarity($2, c.heading_path)) AS sim \
                         FROM kb.chunk c JOIN kb.doc d ON d.doc_id = c.doc_id \
                         WHERE c.doc_id = ANY($1::text[]) \
                           AND GREATEST(word_similarity($2, d.name), \
                                        word_similarity($2, c.heading_path)) > $3 \
                         ORDER BY c.doc_id, sim DESC, c.ord, c.chunk_id \
                         ) x ORDER BY sim DESC, chunk_id LIMIT $4";

async fn title_ids(store: &OwnedStore, docs: &[String], q: &str) -> Result<Vec<i64>, KbError> {
    ids(store.fixed(TITLE_SQL).bind(docs).bind(q).bind(TITLE_MIN).bind(TITLE_TOP)).await
}

/// 文档治理元数据辅助召回。精确包含的标签/业务域给满分，其他情况走 trgm 相似度；
/// 每篇文档只取首块，避免一条标签把整篇文档的所有块铺满候选。
/// Y7：`d.description`（AI 生成描述）进同一路 metadata 语料——只在此一处挂载，
/// 不改权重、阈值与融合（运营小包纪律：权重区属其他代理）。
const METADATA_SQL: &str = "SELECT chunk_id FROM ( \
                            SELECT DISTINCT ON (d.doc_id) c.chunk_id, d.doc_id, \
                              GREATEST( \
                                CASE WHEN length(coalesce(d.business_domain,'')) >= 2 \
                                  AND position(lower(d.business_domain) in lower($2)) > 0 THEN 1 ELSE 0 END, \
                                CASE WHEN EXISTS (SELECT 1 FROM unnest(d.tags) AS tag(value) \
                                  WHERE length(value) >= 2 AND position(lower(value) in lower($2)) > 0) \
                                  THEN 1 ELSE 0 END, \
                                word_similarity($2, coalesce(d.business_domain,'')), \
                                word_similarity($2, array_to_string(d.tags,' ')), \
                                word_similarity($2, coalesce(d.document_family,'')), \
                                word_similarity($2, coalesce(d.document_revision,'')), \
                                word_similarity($2, coalesce(d.source_uri,'')), \
                                word_similarity($2, coalesce(d.description,'')) \
                              ) AS sim, \
                              GREATEST(word_similarity($2, c.heading_path), \
                                       word_similarity($2, c.text)) AS content_sim \
                            FROM kb.doc d JOIN kb.chunk c ON c.doc_id=d.doc_id \
                            WHERE d.doc_id = ANY($1::text[]) \
                            ORDER BY d.doc_id, content_sim DESC, c.ord, c.chunk_id \
                            ) x WHERE sim > $3 \
                            ORDER BY sim DESC, content_sim DESC, chunk_id LIMIT $4";

async fn metadata_ids(store: &OwnedStore, docs: &[String], q: &str) -> Result<Vec<i64>, KbError> {
    ids(store.fixed(METADATA_SQL).bind(docs).bind(q).bind(METADATA_MIN).bind(METADATA_TOP)).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RelationCandidate {
    chunk_id: i64,
    relation: String,
}

/// 从直接命中的块反查文档，再扩展同目录、祖先目录、同文档族/版本与显式引用。
/// seed 与 candidate 两侧都内联 `visible_docs`，撤权发生在两步之间也不会泄露正文。
fn relation_candidates_sql() -> &'static str {
    concat!(
        "WITH seed_docs AS ( \
           SELECT DISTINCT d.doc_id,d.space_id,d.folder_id,d.folder_path, \
                           d.document_family,d.document_revision,d.business_domain,d.tags \
           FROM kb.chunk c JOIN kb.doc d ON d.doc_id=c.doc_id \
           WHERE c.chunk_id=ANY($3::bigint[]) AND d.enabled=true \
             AND d.status IN ('chunked','embedded') \
             AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE) \
             AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE) \
             AND d.doc_id IN (",
        crate::acl::visible_docs!(),
        ") AND ($4::text IS NULL OR d.space_id=$4) \
         ), candidate_docs AS ( \
           SELECT d.doc_id,d.name,d.space_id,d.folder_id,d.folder_path, \
                  d.document_family,d.document_revision,d.business_domain,d.tags, \
                  d.effective_from,d.effective_to \
            FROM kb.doc d WHERE d.enabled=true \
              AND d.status IN ('chunked','embedded') \
              AND EXISTS (SELECT 1 FROM kb.chunk existing WHERE existing.doc_id=d.doc_id) \
             AND d.doc_id IN (",
        crate::acl::visible_docs!(),
        ") AND ($4::text IS NULL OR d.space_id=$4) \
         ), related_docs AS ( \
           SELECT d.doc_id,'same_folder'::text AS relation,3 AS priority \
           FROM seed_docs s JOIN candidate_docs d ON d.space_id=s.space_id \
             AND s.folder_id IS NOT NULL AND d.folder_id=s.folder_id \
             AND NULLIF(btrim(s.folder_path),'') IS NOT NULL \
             AND NULLIF(btrim(d.folder_path),'') IS NOT NULL \
             AND btrim(s.folder_path)<>'/' AND btrim(d.folder_path)<>'/' \
             AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE) \
             AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE) \
           WHERE d.doc_id<>s.doc_id \
            UNION ALL SELECT d.doc_id,'ancestor_folder',4 \
             FROM seed_docs s JOIN candidate_docs d ON d.space_id=s.space_id \
               AND s.folder_id IS NOT NULL AND d.folder_id IS NOT NULL \
               AND NULLIF(btrim(d.folder_path),'') IS NOT NULL \
              AND NULLIF(btrim(s.folder_path),'') IS NOT NULL \
               AND btrim(d.folder_path)<>'/' AND btrim(s.folder_path)<>'/' \
               AND left(btrim(s.folder_path),length(btrim(d.folder_path))+1)=btrim(d.folder_path)||'/' \
               AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE) \
               AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE) \
             WHERE d.doc_id<>s.doc_id \
            UNION ALL SELECT d.doc_id,'descendant_folder',5 \
             FROM seed_docs s JOIN candidate_docs d ON d.space_id=s.space_id \
            WHERE d.doc_id<>s.doc_id \
              AND s.folder_id IS NOT NULL AND d.folder_id IS NOT NULL \
              AND NULLIF(btrim(s.folder_path),'') IS NOT NULL \
              AND NULLIF(btrim(d.folder_path),'') IS NOT NULL \
              AND btrim(s.folder_path)<>'/' AND btrim(d.folder_path)<>'/' \
               AND left(btrim(d.folder_path),length(btrim(s.folder_path))+1)=btrim(s.folder_path)||'/' \
              AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE) \
              AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE) \
            UNION ALL SELECT d.doc_id,'document_family',2 \
           FROM seed_docs s JOIN candidate_docs d ON d.space_id=s.space_id \
             AND NULLIF(btrim(s.document_family),'') IS NOT NULL \
             AND btrim(d.document_family)=btrim(s.document_family) WHERE d.doc_id<>s.doc_id \
           UNION ALL SELECT d.doc_id,'document_revision',1 \
           FROM seed_docs s JOIN candidate_docs d ON d.space_id=s.space_id \
             AND NULLIF(btrim(s.document_family),'') IS NOT NULL \
             AND NULLIF(btrim(s.document_revision),'') IS NOT NULL \
             AND btrim(d.document_family)=btrim(s.document_family) \
             AND btrim(d.document_revision)=btrim(s.document_revision) WHERE d.doc_id<>s.doc_id \
           UNION ALL SELECT d.doc_id,'same_domain',6 \
            FROM seed_docs s JOIN candidate_docs d ON d.space_id=s.space_id \
              AND NULLIF(btrim(s.business_domain),'') IS NOT NULL \
              AND btrim(d.business_domain)=btrim(s.business_domain) \
            WHERE d.doc_id<>s.doc_id \
              AND s.folder_id IS NOT NULL AND d.folder_id IS NOT NULL \
              AND btrim(s.folder_path)<>'/' AND btrim(d.folder_path)<>'/' \
              AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE) \
              AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE) \
           UNION ALL SELECT d.doc_id,'shared_tag',7 \
            FROM seed_docs s JOIN candidate_docs d ON d.space_id=s.space_id \
            WHERE d.doc_id<>s.doc_id \
              AND s.folder_id IS NOT NULL AND d.folder_id IS NOT NULL \
              AND btrim(s.folder_path)<>'/' AND btrim(d.folder_path)<>'/' \
              AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE) \
              AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE) \
              AND EXISTS ( \
             SELECT 1 FROM unnest(s.tags) AS st(tag) \
             JOIN unnest(d.tags) AS dt(tag) ON btrim(dt.tag)=btrim(st.tag) \
             WHERE NULLIF(btrim(st.tag),'') IS NOT NULL \
           ) \
           UNION ALL SELECT d.doc_id, \
             CASE WHEN l.source_doc_id=s.doc_id THEN 'references' ELSE 'referenced_by' END,0 \
           FROM seed_docs s JOIN kb.doc_link l \
             ON l.source_doc_id=s.doc_id OR l.target_doc_id=s.doc_id \
            JOIN candidate_docs d ON d.doc_id=CASE WHEN l.source_doc_id=s.doc_id \
             THEN l.target_doc_id ELSE l.source_doc_id END AND d.space_id=s.space_id \
          ), scored AS ( \
            SELECT c.chunk_id,r.doc_id,r.relation,r.priority,c.ord, \
              word_similarity($5,c.text) AS content_sim, \
              GREATEST(word_similarity($5,c.text),word_similarity($5,c.heading_path), \
                       word_similarity($5,d.name)) AS support_sim, \
              GREATEST(word_similarity($5,c.text),word_similarity($5,c.heading_path), \
                       word_similarity($5,d.name),word_similarity($5,d.folder_path)) AS rank_sim \
             FROM related_docs r JOIN candidate_docs d ON d.doc_id=r.doc_id \
             JOIN kb.chunk c ON c.doc_id=d.doc_id \
             WHERE d.doc_id NOT IN (SELECT doc_id FROM seed_docs) \
          ), ranked AS ( \
            SELECT DISTINCT ON (doc_id,relation) chunk_id,doc_id,relation,priority,ord, \
              content_sim,support_sim,rank_sim,support_sim AS pick_sim \
            FROM scored ORDER BY doc_id,relation,pick_sim DESC,rank_sim DESC,ord,chunk_id \
          ) SELECT chunk_id,relation FROM ranked \
           WHERE CASE \
             WHEN relation IN ('references','referenced_by') THEN true \
             ELSE support_sim > $6 END \
           ORDER BY priority,support_sim DESC,content_sim DESC,rank_sim DESC,chunk_id,relation LIMIT $7"
    )
}

async fn relation_candidates(
    store: &OwnedStore,
    v: &Viewer,
    space: Option<&str>,
    direct_ids: &[i64],
    q: &str,
) -> Result<Vec<RelationCandidate>, KbError> {
    Ok(store
        .fixed(relation_candidates_sql())
        .bind(&v.login)
        .bind(&v.roles)
        .bind(direct_ids)
        .bind(space)
        .bind(q)
        .bind(RELATION_CONTEXT_MIN)
        .bind(RELATION_TOP)
        .fetch_all::<(i64, String)>()
        .await?
        .into_iter()
        .map(|(chunk_id, relation)| RelationCandidate { chunk_id, relation })
        .collect())
}

// ==================== 图谱召回（Yuxi B6：种子 → 扩散 → PPR → RRF 第 7 路） ====================

/// `DMS_KG_RETRIEVAL=off` 整体关闭图谱召回（默认开）。与 rerank 同款：函数签名被调用点冻结，
/// 配置只能走环境变量，每次检索读一次。
fn kg_retrieval_enabled() -> bool {
    kg_retrieval_enabled_env(std::env::var("DMS_KG_RETRIEVAL").ok().as_deref())
}

fn kg_retrieval_enabled_env(v: Option<&str>) -> bool {
    !matches!(v, Some(s) if s.trim().eq_ignore_ascii_case("off"))
}

/// 图谱增强召回第 7 路：返回 chunk_id 列表（PPR 分降序），**永不返 Err**。
/// 三种「缺席」各留各的痕迹：env 关闭 / 不限空间（静默：不是降级，是功能不适用）；
/// 图无数据（warn：该空间没构建过图谱是部署事实，不许装没看见）；查询失败（warn：
/// 图挂了只跳这一路，与「embed 挂只跳向量路」同一条纪律）。
///
/// ACL：扩散与召回只走 `docs`（调用方现算的可见集合）内联进图查询的节点，`doc_graph` 层
/// 没有任何一条不带 doc 过滤的召回 Cypher。
async fn kg_route(
    pg: &sqlx::PgPool,
    space: Option<&str>,
    enabled: bool,
    docs: &[String],
    query: &str,
    vec_ids: &[i64],
) -> Vec<i64> {
    // env 关闭：一条图查询都不发，行为与接入前逐字节一致。
    if !enabled || docs.is_empty() {
        return Vec::new();
    }
    // 图谱按空间构建（实体 id 带空间前缀）：不限空间的检索没有对应的单张图，跳过。
    let Some(space_id) = space else { return Vec::new() };
    match kg_route_inner(pg, space_id, docs, query, vec_ids).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(space = space_id, "图谱召回查询失败 → 本次回退其余召回路（检索未受影响）：{e}");
            Vec::new()
        }
    }
}

async fn kg_route_inner(
    pg: &sqlx::PgPool,
    space_id: &str,
    docs: &[String],
    query: &str,
    vec_ids: &[i64],
) -> anyhow::Result<Vec<i64>> {
    // ① 种子：向量路命中 chunk 提及的实体（问句向量通道）∪ 实体名 trgm/包含命中（trgm 通道）
    let (by_chunk, by_name) = tokio::try_join!(
        doc_graph::entities_of_chunks(pg, space_id, docs, vec_ids, KG_SEED_MAX),
        doc_graph::entities_named_like(
            pg,
            space_id,
            docs,
            query,
            KG_SEED_SIM_MIN,
            KG_SEED_CANDIDATES,
            KG_SEED_MAX,
        ),
    )?;
    let mut seeds = by_chunk;
    for id in by_name {
        if !seeds.contains(&id) {
            seeds.push(id);
        }
    }
    // 并集再按 KG_SEED_MAX 收口：by_chunk/by_name 各自 ≤20，并集可达 40——
    // 「个性化压在少数实体上」的口径不许被并集突破
    seeds.truncate(KG_SEED_MAX);
    if seeds.is_empty() {
        // 「没命中实体」与「图里没数据」长得一样，处置不同：后者是降级，必须留痕。
        if !doc_graph::space_has_chunks(pg, space_id, docs).await? {
            tracing::warn!(space = space_id, "kb_graph 该空间无图数据（未构建过）→ 图谱召回缺席，本次走其余召回路");
        }
        return Ok(Vec::new());
    }
    // ② 1~2 hop 扩散：邻边查询的 doc 过滤内联在 Cypher 里（可见集合之外一个节点都碰不到）
    let hop1_edges = doc_graph::relation_edges_touching(pg, space_id, docs, &seeds, KG_EDGE_ROWS).await?;
    let hop1 = new_endpoints(&seeds, &hop1_edges);
    let mut frontier: Vec<String> = seeds.iter().chain(hop1.iter()).cloned().collect();
    frontier.truncate(KG_MAX_NODES); // 超出上限的实体进不了子图，它们的邻边也不必查
    let hop2_edges = if hop1.is_empty() {
        Vec::new() // 没有一跳邻居就不存在二跳
    } else {
        doc_graph::relation_edges_touching(pg, space_id, docs, &frontier, KG_EDGE_ROWS).await?
    };
    // ③ 组装（节点上限在 Rust 侧收口，纯函数可单测）→ 幂迭代 PPR → top chunk
    let entities = diffuse_entities(&seeds, &hop1_edges, &hop2_edges, KG_MAX_NODES);
    let entity_ids: Vec<String> = entities.iter().map(|(id, _)| id.clone()).collect();
    let chunks = doc_graph::mentioned_chunks(pg, space_id, docs, &entity_ids, KG_MAX_NODES).await?;
    let chunk_ids: Vec<i64> = chunks.iter().map(|(id, _)| *id).collect();
    let pairs = if chunk_ids.is_empty() {
        Vec::new()
    } else {
        doc_graph::mention_pairs(pg, space_id, docs, &entity_ids, &chunk_ids, KG_PAIR_ROWS).await?
    };
    let mut rel_edges = hop1_edges;
    rel_edges.extend(hop2_edges);
    // ⚠️ hop1 边被计入两次（frontier 含 seeds，hop2 查询覆盖 hop1 的边）：`assemble_subgraph`
    // 的 `+= e.weight` 让 hop1 关系边权重翻倍、hop1-hop1/hop1-hop2 边不翻倍。
    // 这是现有行为，当作「hop1 关系更强」的隐式加权保留——改动会漂 PPR 排序（判官面），
    // 要去重先过评测。
    let sg = assemble_subgraph(&entities, &rel_edges, &chunks, &pairs, KG_MAX_NODES);
    let (rank, _) = personalized_pagerank(sg.teleport.len(), &sg.edges, &sg.teleport);
    Ok(kg_top_chunks(&sg, &rank))
}

/// 邻边查询带回来的新端点（不在已知集合内的），保持边序（支持次数降序，高支持邻域先进）。
fn new_endpoints(known: &[String], edges: &[RelEdge]) -> Vec<String> {
    // 旁挂 HashSet 判重（Vec 保序）：双重线性查找在 known≤200、端点≤2000 时是数十万次比较
    let known: std::collections::HashSet<&str> = known.iter().map(String::as_str).collect();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for edge in edges {
        for endpoint in [&edge.src, &edge.dst] {
            if !known.contains(endpoint.as_str()) && seen.insert(endpoint.as_str()) {
                out.push(endpoint.clone());
            }
        }
    }
    out
}

/// 扩散的实体层组装：hop0 种子（1.0）→ hop1 邻边实体（0.8）→ hop2（0.0），上限截断。
/// hop2 的边查询覆盖了 hop1 的边（frontier 是超集），重复端点按「先到的权重」保留 ——
/// 即 hop1 实体不会被 hop2 结果覆盖成 0.0。
fn diffuse_entities(
    seeds: &[String],
    hop1_edges: &[RelEdge],
    hop2_edges: &[RelEdge],
    max_nodes: usize,
) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = Vec::new();
    // 旁挂判重集（上限 200 时线性 any 约 2 万次比较）
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for seed in seeds {
        if out.len() >= max_nodes {
            return out;
        }
        if seen.insert(seed.as_str()) {
            out.push((seed.clone(), KG_SEED_DIRECT));
        }
    }
    for (edges, weight) in [(hop1_edges, KG_SEED_NEIGHBOR), (hop2_edges, KG_SEED_HOP2)] {
        for edge in edges {
            for endpoint in [&edge.src, &edge.dst] {
                if out.len() >= max_nodes {
                    return out;
                }
                if seen.insert(endpoint.as_str()) {
                    out.push((endpoint.clone(), weight));
                }
            }
        }
    }
    out
}

/// PPR 的完整输入：节点（实体按扩散优先序 + chunk 按 SQL 度数序）、无向加权边、个性化权重。
struct KgSubgraph {
    teleport: Vec<f64>,
    /// (a, b, w)，节点下标对；已按 (a,b) 定序 —— 幂迭代按边序累加浮点，序不定末位会漂
    edges: Vec<(usize, usize, f64)>,
    /// (节点下标, chunk_id)
    chunk_nodes: Vec<(usize, i64)>,
}

/// 子图组装：实体层 + chunk 层合计 ≤ `max_nodes`；两端不全在子图内的边一律丢弃
/// （被上限截断的实体不带边进来 —— 悬空边会把个性化质量漏给没通过可见性/上限闸的节点）。
fn assemble_subgraph(
    entities: &[(String, f64)],
    rel_edges: &[RelEdge],
    chunks: &[(i64, i64)],
    pairs: &[(i64, String)],
    max_nodes: usize,
) -> KgSubgraph {
    let mut index: HashMap<&str, usize> = HashMap::with_capacity(entities.len());
    let mut teleport: Vec<f64> = Vec::with_capacity(entities.len() + chunks.len());
    for (id, w) in entities {
        index.insert(id.as_str(), teleport.len());
        teleport.push(*w);
    }
    // chunk 名额 = 上限扣掉实体（实体层吃满时这一路产不出候选 —— 上限本来就是防爆闸）。
    // chunks 已由 SQL 按度数降序排好，保序截断。
    let mut chunk_index: HashMap<i64, usize> = HashMap::new();
    let mut chunk_nodes: Vec<(usize, i64)> = Vec::new();
    for &(cid, _) in chunks.iter().take(max_nodes.saturating_sub(teleport.len())) {
        chunk_index.insert(cid, teleport.len());
        chunk_nodes.push((teleport.len(), cid));
        teleport.push(0.0);
    }
    let mut agg: HashMap<(usize, usize), f64> = HashMap::new();
    for e in rel_edges {
        if let (Some(&a), Some(&b)) = (index.get(e.src.as_str()), index.get(e.dst.as_str())) {
            if a != b {
                *agg.entry((a.min(b), a.max(b))).or_default() += e.weight as f64;
            }
        }
    }
    for (cid, eid) in pairs {
        if let (Some(&c), Some(&e)) = (chunk_index.get(cid), index.get(eid.as_str())) {
            *agg.entry((c.min(e), c.max(e))).or_default() += 1.0;
        }
    }
    let mut edges: Vec<(usize, usize, f64)> = agg.into_iter().map(|((a, b), w)| (a, b, w)).collect();
    edges.sort_by_key(|&(a, b, _)| (a, b));
    KgSubgraph { teleport, edges, chunk_nodes }
}

/// 幂迭代 Personalized PageRank（自写，零依赖）：`p ← (1-d)·t + d·Mᵀp`，M 按加权出度归一。
/// 边无向化：MENTIONS/RELATION 都是「相关」语义，扩散不该被抽取时的方向卡死；
/// dangling（出度 0）节点的 rank 均摊回全体 —— 子图截断会造出叶子，不摊会漏质量。
/// 返回 (rank, 实际迭代轮数)；轮数只用于单测观察收敛性。
fn personalized_pagerank(
    n: usize,
    edges: &[(usize, usize, f64)],
    teleport: &[f64],
) -> (Vec<f64>, usize) {
    if n == 0 {
        return (Vec::new(), 0);
    }
    let mut out_w = vec![0.0f64; n];
    for &(a, b, w) in edges {
        out_w[a] += w;
        out_w[b] += w;
    }
    let tsum: f64 = teleport.iter().sum();
    if tsum <= 0.0 {
        return (vec![0.0; n], 0);
    }
    let tele: Vec<f64> = teleport.iter().map(|t| t / tsum).collect();
    // dangling 节点集合由 out_w 决定、迭代期间不变——下标集合循环外算一次复用
    let dangling_idx: Vec<usize> = (0..n).filter(|&i| out_w[i] == 0.0).collect();
    let mut rank = vec![1.0 / n as f64; n];
    // 双 buffer 轮换（每轮 collect 重新分配 ×100 轮不值）；逐元素运算保持原样，逐位等价
    let mut next = vec![0.0f64; n];
    let mut iter = 0;
    while iter < KG_PPR_MAX_ITER {
        let dangling: f64 = dangling_idx.iter().map(|&i| rank[i]).sum();
        for i in 0..n {
            next[i] = (1.0 - KG_PPR_DAMPING) * tele[i] + KG_PPR_DAMPING * dangling / n as f64;
        }
        for &(a, b, w) in edges {
            next[b] += KG_PPR_DAMPING * rank[a] * w / out_w[a];
            next[a] += KG_PPR_DAMPING * rank[b] * w / out_w[b];
        }
        let diff: f64 = next.iter().zip(&rank).map(|(x, y)| (x - y).abs()).sum();
        std::mem::swap(&mut rank, &mut next);
        iter += 1;
        if diff < KG_PPR_TOL {
            break;
        }
    }
    (rank, iter)
}

/// PPR 分 → 第 7 路候选：chunk 节点按 rank 降序（同分按 chunk_id 升序，可复现）取 `KG_TOP`。
fn kg_top_chunks(sg: &KgSubgraph, rank: &[f64]) -> Vec<i64> {
    let mut scored: Vec<(i64, f64)> =
        sg.chunk_nodes.iter().map(|&(idx, cid)| (cid, rank[idx])).collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().take(KG_TOP).map(|(cid, _)| cid).collect()
}

// ==================== 外部只读 KB（Yuxi B9：Dify 数据集检索 → RRF 第 8 路） ====================

/// 外部知识库第 8 路：返回远程记录（响应原序即名次序），**永不返 Err**。
/// 未配置（`client = None`）：静默缺席 —— 功能关闭不是降级，不留 warn；
/// 配置后失败/超时/熔断/形状不符：warn 留痕回退原七路 —— 与 rerank/图谱同一条纪律：
/// 外挂加分项绝不许挂掉检索。
async fn ext_kb_route(client: Option<&ExtKbClient>, query: &str) -> Vec<ExtKbRecord> {
    let Some(client) = client else { return Vec::new() };
    match client.retrieve(query, EXT_KB_TOP).await {
        Some(records) => records,
        None => {
            tracing::warn!("外部知识库检索不可用（失败/超时/熔断/响应形状不符）→ 本次回退原七路（检索未受影响）");
            Vec::new()
        }
    }
}

/// 外部路合成 id：-(名次+1)，恒负。本地 chunk_id 是 PG bigserial 恒正 —— 两个命名空间永不撞，
/// 且负 id 在 `load_hits` 的 `ANY($3::bigint[])` 里天然匹配不到任何行（不误载、不泄露）。
fn ext_kb_synthetic_id(rank: usize) -> i64 {
    -(rank as i64) - 1
}

/// 外部记录 → 只读 Hit。无本地 chunk 行：`chunk_id` 是合成负 id（只在本次检索内当 RRF/合并的
/// 临时锚点），引用回查（`window`/`span`）对它统一报 Forbidden —— 与「不存在与不可见」同一形态。
///
/// ACL 面：外部 KB 是**独立授权源，配置即授权**（`DMS_EXT_KB_*` 配齐 = 部署方把这只数据集授给
/// 全体 KB 用户），它走不到 `kb.doc` 的 ACL 子查询；作为交换，`source_uri` 必须标注来源，
/// 让「这条证据来自外部系统」一眼可辨。`source_hash` 置空：`dedup_sources` 对空 hash 原样放行
/// （远程块没有 sha256 可核对），绝不许伪造一个。
fn ext_kb_hit(chunk_id: i64, ord: i32, record: &ExtKbRecord) -> Hit {
    let doc_key = if record.document_id.is_empty() {
        record.segment_id.as_str()
    } else {
        record.document_id.as_str()
    };
    Hit {
        chunk_id,
        // 带冒号的前缀本地 doc_id 产不出（本地是 uuid/文档码）：分组/多样化把它当一篇独立文档。
        doc_id: format!("ext-kb:{doc_key}"),
        doc_name: record.document_name.clone(),
        folder_id: None,
        folder_path: String::new(),
        ord,
        text: record.content.clone(),
        heading_path: String::new(),
        page: None,
        tags: Vec::new(),
        business_domain: None,
        effective_from: None,
        effective_to: None,
        source_uri: Some(record.source_uri.clone()).filter(|s| !s.is_empty()), // 空串不落 Some("")：「来源可辨」不能变成空链接
        // 远程版本/族不可核对：留空 → 不进治理版本与文本版本两套保全逻辑。
        document_family: None,
        document_revision: None,
        source_hash: String::new(),
        doc_updated_at: String::new(),
        channels: Vec::new(),
        relations: Vec::new(),
        score: 0.0,
        // 远程块没有本地向量：并列时排在有距离的之后（与它「宁少勿滥」的定位一致）
        vec_dist: None,
        merged: 1,
    }
}

/// 八路召回的通道名，顺序 = `lists` 槽位序（`search_report` 里 `debug_assert_eq!(lists.len(), 8)`
/// 钉住对齐）。`match_channels` 的 zip 对更短的输入静默截断——测试只给 6 路是刻意的。
const CHANNEL_NAMES: [&str; 9] =
    ["向量", "精确匹配", "正文相似", "标题", "元数据", "结构关联", "图谱", "外部知识库", "词级"];

/// 测试专用的通道名查询（生产路径在 `search_report` 里用离线建好的 `channel_map` 查表）。
/// zip 对更短的输入静默截断——测试只给 6 路是刻意的。
#[cfg(test)]
fn match_channels(chunk_id: i64, lists: &[Vec<i64>]) -> Vec<String> {
    lists
        .iter()
        .zip(CHANNEL_NAMES)
        .filter(|(ids, _)| ids.contains(&chunk_id))
        .map(|(_, name)| name.to_string())
        .collect()
}

/// 元数据只能增强已由直接命中路（向量/精确/正文/标题/词级）召回的块，
/// 不能凭标签或业务域单独制造答案候选。
/// 泛型是为词级路让位：它的 lists 槽位钉死在末尾（通道名表前八槽被既有测试钉住），
/// 「直接命中」集合在调用点按引用并行数组传入，这里不再假设槽位连续。
fn keep_auxiliary_votes_on_direct_hits<S: AsRef<[i64]>>(direct: &[S], auxiliary: &mut Vec<i64>) {
    auxiliary.retain(|id| direct.iter().any(|route| route.as_ref().contains(id)));
}

/// 取最终正文时重放 ACL、启用状态和空间条件。直接命中仍须当前有效；仅同族版本和显式
/// 文档关系候选允许带回历史/未来资料供并列核对。
fn load_hits_sql() -> &'static str {
    concat!(
        hit_select!(),
        " WHERE c.chunk_id = ANY($3::bigint[]) \
           AND d.enabled=true AND d.status IN ('chunked','embedded') \
           AND (((d.effective_from IS NULL OR d.effective_from <= CURRENT_DATE) \
             AND (d.effective_to IS NULL OR d.effective_to >= CURRENT_DATE)) \
             OR c.chunk_id = ANY($5::bigint[])) \
           AND d.doc_id IN (",
        crate::acl::visible_docs!(),
        ") AND ($4::text IS NULL OR d.space_id=$4)"
    )
}

/// 候选块与问句的向量距离（`embedding <=> $1`）。**只做并列次序键**，不是阈值。
///
/// 一切失败 = 空表（次序退回 `chunk_id` 兜底）：它是排序的锦上添花，
/// 不许因为它让一次本来能答的检索失败。
async fn chunk_distances(
    store: &OwnedStore,
    query_vector: &str,
    chunk_ids: &[i64],
) -> HashMap<i64, f32> {
    if chunk_ids.is_empty() {
        return HashMap::new();
    }
    let rows: Result<Vec<(i64, f64)>, _> = store
        .fixed(
            "SELECT chunk_id, (embedding <=> $1::vector) AS dist FROM kb.chunk              WHERE chunk_id = ANY($2::bigint[]) AND embedding IS NOT NULL",
        )
        .bind(query_vector)
        .bind(chunk_ids)
        .fetch_all()
        .await;
    match rows {
        Ok(rows) => rows.into_iter().map(|(id, d)| (id, d as f32)).collect(),
        Err(e) => {
            tracing::warn!(err = %e, "候选块向量距离读取失败 → 并列次序退回 chunk_id 兜底");
            HashMap::new()
        }
    }
}

async fn load_hits(
    store: &OwnedStore,
    v: &Viewer,
    space: Option<&str>,
    chunk_ids: &[i64],
    review_ids: &[i64],
) -> Result<Vec<Hit>, KbError> {
    Ok(store
        .fixed(load_hits_sql())
        .bind(&v.login)
        .bind(&v.roles)
        .bind(chunk_ids)
        .bind(space)
        .bind(review_ids)
        .fetch_all()
        .await?)
}

/// 等权 RRF 的测试壳（`score = Σ 1/(60 + rank)`，权重全 1.0）——生产路径走 `rrf_weighted`，
/// 公式与权重语义见它的文档。
#[cfg(test)]
fn rrf(lists: &[Vec<i64>]) -> Vec<(i64, f32)> {
    rrf_weighted(lists, &[])
}

/// RRF 融合：`score = Σ w_route/(60 + rank)`，rank 从 1 起；`weights` 缺省的路按 1.0、负值钳 0
/// （钳制是防御性保险丝，配置侧的拒绝闸在 `RrfWeights::validate`）。
/// 并列按 `chunk_id` 升序 —— 检索结果必须可复现，否则「同一问句两次不同答案」无法排查。
///
/// `ponytail:` O(路数 × top × 结果集) 的线性查找，n ≤ 50，建 HashMap 不划算。
fn rrf_weighted(lists: &[Vec<i64>], weights: &[f32]) -> Vec<(i64, f32)> {
    let mut acc: Vec<(i64, f32)> = Vec::new();
    for (route, list) in lists.iter().enumerate() {
        let weight = weights.get(route).copied().unwrap_or(1.0).max(0.0);
        let mut seen: Vec<i64> = Vec::new();
        for id in list {
            // 单路 SQL 理论上不重复；这里防御性去重，避免未来 JOIN 改动把同一路的重复行算成多路共识。
            if seen.contains(id) {
                continue;
            }
            seen.push(*id);
            let s = weight / (RRF_K + seen.len() as f32);
            match acc.iter_mut().find(|(x, _)| x == id) {
                Some(e) => e.1 += s,
                None => acc.push((*id, s)),
            }
        }
    }
    acc.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    acc
}

/// 候选后处理：相邻块先合并，再按原文件/正文去重，最后稳定地保证来源多样性。
/// = `finalize_ranked(rank_hits(hits))`。拆两段只为让 rerank 插在中间；本函数行为一字未变。
fn finalize_hits(hits: Vec<Hit>) -> Vec<Hit> {
    finalize_ranked(rank_hits(hits))
}

/// 合并 + 去重后的候选全序（rerank 的输入；未开 rerank 时它原样进 `finalize_ranked`）。
fn rank_hits(hits: Vec<Hit>) -> Vec<Hit> {
    dedup_text(dedup_sources(merge_adjacent(hits)))
}

/// 从候选全序里截最终进 prompt 的 `TOP_K`（来源多样化 + 版本/显式关系保全）。
fn finalize_ranked(ranked: Vec<Hit>) -> Vec<Hit> {
    let selected = diversify(&ranked, TOP_K);
    let selected = preserve_governed_versions(&ranked, selected, TOP_K);
    let selected = preserve_textual_versions(&ranked, selected, TOP_K);
    append_explicit_context(&ranked, selected)
}

/// rerank 精排（`docs/research/yuxi.json` B5）：头部 `RERANK_WINDOW` 条送 rerank 服务重排，
/// 窗口外候选保持 RRF 原序。打分不可用（服务挂/超时/熔断/响应形状不符）→ **原样返回**并
/// `warn` 留痕 —— 回退 RRF 原序是降级，降级不许静默；rerank 只是加分项，绝不允许它挂掉检索。
async fn rerank_candidates(client: &RerankClient, query: &str, ranked: Vec<Hit>) -> Vec<Hit> {
    if ranked.len() < 2 {
        return ranked; // 0/1 条没有可排的，省一次 HTTP
    }
    let window = ranked.len().min(RERANK_WINDOW);
    let docs: Vec<&str> = ranked[..window].iter().map(|h| h.text.as_str()).collect();
    let Some(scores) = client.rerank(query, &docs).await else {
        tracing::warn!(candidates = window, "rerank 打分不可用 → 本次回退 RRF 原序（检索未受影响）");
        return ranked;
    };
    // `scores` 与窗口等长且按下标对齐（client 的形状保证）。稳定排序：同分保持 RRF 原序，
    // 否则「同一问句两次不同顺序」无法排查（与 `rrf_weighted` 并列按 chunk_id 同一条纪律）。
    let mut order: Vec<usize> = (0..window).collect();
    order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]));
    let mut iter = ranked.into_iter();
    let mut head: Vec<Option<Hit>> = iter.by_ref().take(window).map(Some).collect();
    let mut out: Vec<Hit> = Vec::with_capacity(window + iter.len());
    out.extend(order.into_iter().map(|i| head[i].take().expect("每个下标恰好取一次")));
    out.extend(iter);
    out
}

fn governed_version_key(hit: &Hit) -> Option<(String, String)> {
    let family = hit.document_family.as_deref()?.trim();
    if family.is_empty() {
        return None;
    }
    let signature = [
        ("revision", hit.document_revision.as_deref()),
        ("from", hit.effective_from.as_deref()),
        ("to", hit.effective_to.as_deref()),
    ]
    .into_iter()
    .filter_map(|(field, value)| {
        value.map(str::trim).filter(|value| !value.is_empty()).map(|value| format!("{field}={value}"))
    })
    .collect::<Vec<_>>()
    .join("|");
    (!signature.is_empty()).then(|| (family.to_string(), signature))
}

fn governed_versions_conflict(left: &Hit, right: &Hit) -> bool {
    if left.document_family.as_deref().map(str::trim)
        != right.document_family.as_deref().map(str::trim)
    {
        return false;
    }
    [
        (left.document_revision.as_deref(), right.document_revision.as_deref()),
        (left.effective_from.as_deref(), right.effective_from.as_deref()),
        (left.effective_to.as_deref(), right.effective_to.as_deref()),
    ]
    .into_iter()
    .any(|(left, right)| match (left.map(str::trim), right.map(str::trim)) {
        (Some(left), Some(right)) if !left.is_empty() && !right.is_empty() => left != right,
        _ => false,
    })
}

/// 文本版版本标记词表（`textual_version_class` 与 `opposite_version_sections` 共用——
/// 各硬编码一份，改一处漏一处就是行为分叉）
const TEXT_OLD_MARKERS: [&str; 4] = ["旧版", "历史版", "历史口径", "废止"];
const TEXT_CURRENT_MARKERS: [&str; 4] = ["新版", "现行版", "现行口径", "修订版"];

/// 版本保全最多追加的候选条数（两个 `preserve_*` 共用）：新旧两版并排核对足够，
/// 再多就挤掉正文命中了
const PRESERVE_APPEND_MAX: usize = 2;

/// 已召回的同族多版本必须一起进入最终上下文，但它们只是补充核对资料：
/// 先保留正文直接命中的 `selected`，再在末尾追加最多两个版本候选，不能反过来挤掉正文命中。
fn preserve_governed_versions(ranked: &[Hit], selected: Vec<Hit>, limit: usize) -> Vec<Hit> {
    // 预算各 hit 的版本键：去重比较里反复重算（`governed_version_key` 每次比较都分配 String）
    let keys: Vec<Option<(String, String)>> = ranked.iter().map(governed_version_key).collect();
    let mut required: Vec<Hit> = Vec::new();
    let mut required_keys: Vec<&(String, String)> = Vec::new();
    for (i, hit) in ranked.iter().enumerate() {
        let Some(key) = &keys[i] else { continue };
        let conflicts = ranked.iter().any(|other| {
            other.doc_id != hit.doc_id && governed_versions_conflict(hit, other)
        });
        if conflicts && !required_keys.contains(&key) {
            required_keys.push(key);
            required.push(hit.clone());
        }
    }
    if required.is_empty() {
        return selected;
    }
    let mut out = selected;
    let mut added = 0usize;
    for hit in required {
        if added >= PRESERVE_APPEND_MAX || out.len() >= limit + PRESERVE_APPEND_MAX {
            break;
        }
        if !out.iter().any(|existing| existing.chunk_id == hit.chunk_id) {
            out.push(hit);
            added += 1;
        }
    }
    out
}

fn textual_version_class(hit: &Hit) -> Option<i8> {
    let old = TEXT_OLD_MARKERS
        .iter()
        .any(|marker| hit.heading_path.contains(marker) || hit.text.contains(marker));
    let current = TEXT_CURRENT_MARKERS
        .iter()
        .any(|marker| hit.heading_path.contains(marker) || hit.text.contains(marker));
    match (old, current) {
        (true, false) => Some(-1),
        (false, true) => Some(1),
        _ => {
            let old = TEXT_OLD_MARKERS.iter().any(|marker| hit.doc_name.contains(marker));
            let current = TEXT_CURRENT_MARKERS.iter().any(|marker| hit.doc_name.contains(marker));
            match (old, current) {
                (true, false) => Some(-1),
                (false, true) => Some(1),
                _ => None,
            }
        }
    }
}

fn textual_version_group(hit: &Hit) -> String {
    if let Some(family) = hit.document_family.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        return format!("family:{}", family.to_lowercase());
    }
    let stem = hit.doc_name.rsplit_once('.').map_or(hit.doc_name.as_str(), |(stem, _)| stem);
    let mut normalized = stem.to_lowercase();
    for marker in ["现行版", "修订版", "历史版", "新版", "旧版", "废止", "备份", "副本"] {
        normalized = normalized.replace(marker, "");
    }
    let normalized: String = normalized.chars().filter(|ch| ch.is_alphabetic()).collect();
    if normalized.chars().count() < 4 || ["制度", "规定", "办法", "流程", "手册"].contains(&normalized.as_str()) {
        format!("doc:{}", hit.doc_id)
    } else {
        format!("name:{normalized}")
    }
}

fn preserve_textual_versions(ranked: &[Hit], selected: Vec<Hit>, limit: usize) -> Vec<Hit> {
    // 预算 (class, group)：class 要对合并后长正文跑 8 个 marker contains、group 有
    // lowercase+多次 replace 分配——O(n²) 双重循环里反复重算不值
    let meta: Vec<(Option<i8>, String)> =
        ranked.iter().map(|h| (textual_version_class(h), textual_version_group(h))).collect();
    let mut required: Vec<Hit> = Vec::new();
    let mut required_keys: Vec<(&str, i8)> = Vec::new();
    for (i, hit) in ranked.iter().enumerate() {
        let (Some(class), group) = (meta[i].0, &meta[i].1) else { continue };
        let conflicts = meta
            .iter()
            .any(|(other_class, g)| g == group && other_class.is_some_and(|oc| oc != class));
        if conflicts && !required_keys.contains(&(group.as_str(), class)) {
            required_keys.push((group.as_str(), class));
            required.push(hit.clone());
        }
    }
    if required.is_empty() {
        return selected;
    }
    let mut out = selected;
    let mut added = 0usize;
    for hit in required {
        if added >= PRESERVE_APPEND_MAX || out.len() >= limit + PRESERVE_APPEND_MAX {
            break;
        }
        if !out.iter().any(|existing| existing.chunk_id == hit.chunk_id) {
            out.push(hit);
            added += 1;
        }
    }
    out
}

fn append_explicit_context(ranked: &[Hit], mut selected: Vec<Hit>) -> Vec<Hit> {
    let candidate = ranked.iter().find(|hit| {
        !hit.channels.is_empty()
            && hit.channels.iter().all(|channel| channel == "结构关联")
            && hit.relations.iter().any(|relation| matches!(relation.as_str(), "references" | "referenced_by"))
            && !selected.iter().any(|existing| existing.chunk_id == hit.chunk_id)
    });
    if let Some(candidate) = candidate {
        selected.push(candidate.clone());
    }
    selected
}

fn normalized_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}

/// 同一原文件重复上传时 `doc_id` 不同、`source_hash` 相同。只选融合分最高的那次上传，
/// 但保留所选文档里的不同证据段；否则同一个 PDF 会伪装成多份独立佐证。
fn dedup_sources(hits: Vec<Hit>) -> Vec<Hit> {
    let mut selected: Vec<(String, String)> = Vec::new();
    let mut out = Vec::with_capacity(hits.len());
    for hit in hits {
        if hit.source_hash.is_empty() {
            out.push(hit);
            continue;
        }
        match selected.iter().find(|(hash, _)| hash == &hit.source_hash) {
            Some((_, doc_id)) if doc_id != &hit.doc_id => continue,
            Some(_) => out.push(hit),
            None => {
                selected.push((hit.source_hash.clone(), hit.doc_id.clone()));
                out.push(hit);
            }
        }
    }
    out
}

/// 正文级去重：只在**同 doc_id 内**按归一化正文去重（跨文档同正文是「同一原件重复上传」，
/// 归 `dedup_sources` 按 source_hash 管，两份各管一段不重叠）。
/// 归一化后为空的块（纯空白正文）豁免去重：空键互相撞会误杀，原样放行。
fn dedup_text(hits: Vec<Hit>) -> Vec<Hit> {
    let mut out: Vec<Hit> = Vec::with_capacity(hits.len());
    // (doc_id, 归一化键) 旁挂缓存：内层 any 对每个已收条目重算归一化是 O(n²) 的字符串分配
    let mut keys: Vec<(String, String)> = Vec::with_capacity(hits.len());
    for h in hits {
        let key = normalized_text(&h.text);
        if key.is_empty() || !keys.iter().any(|(doc, k)| *doc == h.doc_id && *k == key) {
            keys.push((h.doc_id.clone(), key));
            out.push(h);
        }
    }
    out
}

/// 多文档时先保证来源覆盖（每篇最多 `DOC_FIRST_PASS` 条）；候选不足时第二轮用同文档结果补满。
/// 收 `&[Hit]`（调用方只有一份全序要保给后续 preserve 步骤），入选条目逐条 clone。
fn diversify(hits: &[Hit], limit: usize) -> Vec<Hit> {
    let mut out: Vec<Hit> = Vec::with_capacity(limit.min(hits.len()));
    for h in hits {
        let n = out.iter().filter(|p| p.doc_id == h.doc_id).count();
        if n < DOC_FIRST_PASS {
            out.push(h.clone());
            if out.len() == limit {
                return out;
            }
        }
    }
    for h in hits {
        if out.iter().any(|p| p.chunk_id == h.chunk_id) {
            continue;
        }
        out.push(h.clone());
        if out.len() == limit {
            break;
        }
    }
    out
}

/// 同文档 `ord` 连续的块拼成一条（减少碎片）：`chunk_id`/`ord` 取首块，
/// `heading_path` 取组内第一个非空，分数取组内最大，`page` 取第一个非空。输出按分数降序。
fn merge_adjacent(mut hits: Vec<Hit>) -> Vec<Hit> {
    hits.sort_by(|a, b| a.doc_id.cmp(&b.doc_id).then(a.ord.cmp(&b.ord)));
    let mut out: Vec<Hit> = Vec::new();
    // out 末条已吸收到的最大 ord（合并后 `Hit.ord` 是首块的，不能用它判连续）
    let mut tail = i32::MIN;
    for h in hits {
        let cont = out.last().is_some_and(|p| {
            p.doc_id == h.doc_id
                && h.ord == tail + 1
                && !opposite_version_sections(p, &h)
        });
        tail = h.ord;
        match out.last_mut() {
            Some(p) if cont && p.merged < MAX_MERGE_SPAN => {
                p.text.push_str(JOIN);
                p.text.push_str(&h.text);
                p.score = p.score.max(h.score);
                p.page = p.page.or(h.page);
                if p.heading_path.is_empty() && !h.heading_path.is_empty() {
                    p.heading_path = h.heading_path.clone();
                }
                for channel in h.channels {
                    if !p.channels.contains(&channel) {
                        p.channels.push(channel);
                    }
                }
                for relation in h.relations {
                    if !p.relations.contains(&relation) {
                        p.relations.push(relation);
                    }
                }
                // 记住跨度：引用只有靠它才能被忠实回查（`Citation::span`）
                p.merged += 1;
            }
            _ => out.push(h),
        }
    }
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            // 🔴 RRF 同分时按**与问句的向量距离**决胜，不再按入库顺序（见 `Hit::vec_dist`）
            .then(dist_key(a).total_cmp(&dist_key(b)))
            .then(a.chunk_id.cmp(&b.chunk_id))
    });
    out
}

/// 并列次序键：没有向量的块（外部 KB 合成块 / embed 降级）排在有距离的之后。
fn dist_key(hit: &Hit) -> f32 {
    hit.vec_dist.unwrap_or(f32::INFINITY)
}

fn opposite_version_sections(left: &Hit, right: &Hit) -> bool {
    fn class(hit: &Hit) -> Option<i8> {
        let old = TEXT_OLD_MARKERS
            .iter()
            .any(|marker| hit.heading_path.contains(marker) || hit.text.contains(marker));
        let current = TEXT_CURRENT_MARKERS
            .iter()
            .any(|marker| hit.heading_path.contains(marker) || hit.text.contains(marker));
        match (old, current) {
            (true, false) => Some(-1),
            (false, true) => Some(1),
            _ => None,
        }
    }
    matches!(
        (class(left), class(right)),
        (Some(left), Some(right)) if left != right
    )
}

#[cfg(test)]
mod tests {
    /// 🔴 标定值必须记住自己是在哪个向量空间上量的。
    ///
    /// `VEC_MAX_DIST` 是**标定值不是常数**（它自己的注释里写着）。2026-08-16~17 两天里
    /// 向量层换了模型（bge-small-zh → 千问 text-embedding-v4）又换了维度（512 → 1024），
    /// 而这个阈值一动不动、五条既有判据也全绿 —— 因为它们只钉「值等于 0.55」，
    /// 钉不住「0.55 还成立吗」。阈值作废了也**不会有人知道**：检索只是悄悄少给几块。
    ///
    /// 这条判据把标定上下文与**当前真实配置**绑在一起：模型名取自
    /// `tools/embed_service.py` 的 MODEL，维度取自 `semantic::ddl::EMBED_DIM`。
    /// 任一变化 ⇒ 当场红 ⇒ 逼下一手按注释里的量法重量一遍。
    #[test]
    fn the_vector_threshold_records_what_it_was_calibrated_on() {
        let py = include_str!("../../../tools/embed_service.py");
        let model_line = py
            .lines()
            .find(|l| l.starts_with("MODEL = "))
            .expect("embed_service.py 里找不到 `MODEL = ` 那一行");
        // 形如 MODEL = os.environ.get(...默认模型名在最后一对单引号里)。
        // 用 char::from(39) 取单引号：源码里写单引号字面量要转义，转义在这一行里读着更容易看错
        let model = model_line
            .rsplit(char::from(39))
            .nth(1)
            .expect("MODEL 那一行取不到默认模型名");
        // knowledge 不依赖 semantic（分层纪律），维度从 semantic 的 DDL 源文件里读 ——
        // 那一行本身又被 `the_vector_dimension_is_declared_once` 钉着，链条闭合。
        let ddl = include_str!("../../semantic/src/ddl.rs");
        let dim_line = ddl
            .lines()
            .find(|l| l.starts_with("pub const EMBED_DIM"))
            .expect("semantic/src/ddl.rs 里找不到 EMBED_DIM");
        // 只取等号右边的数字：左边的 `u32` 里也有数字（第一版就被它坑成 321024）
        let dim: String = dim_line
            .rsplit_once('=')
            .expect("EMBED_DIM 那一行没有等号")
            .1
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        let want = format!("{model}@{dim}");
        assert_eq!(
            VEC_CALIBRATED_ON, want,
            "向量空间已从 {VEC_CALIBRATED_ON} 变成 {want}，而 VEC_MAX_DIST={VEC_MAX_DIST} 还是旧空间上量的。             按注释里的量法重量一遍（判据块最远的那个 vs 远域 nohit 最近的那个，取缝的中点），             再把新数字与 VEC_CALIBRATED_ON 一起更新。"
        );
    }


    /// 🔴 图谱路与外部 KB 路必须与关系路共用同一条锚：**没有正文直接命中就不出候选**。
    ///
    /// 由来（2026-08-16）：纯数据问句下面挂一段「知识库里没有关于…」外加一篇无关手册。
    /// 五条正文路（向量/全文/标题/元数据/词级）全空 ⇒ 按定义没有任何正文证据，
    /// 而这两路仍往 `ids` 里塞候选 —— `kg_top_chunks` 没有相关度下限（只按图上邻近度取前 K），
    /// 外部 KB 用的是合成负 id、结构上进不了任何正文路。于是本函数下方那条
    /// 「零命中早退」的不变量在注释里写着、实际却是假的，top-k 无关文档照样进 prompt。
    ///
    /// 这里只能扫源码：`search_report` 要 PgPool + 向量服务，单测跑不起来。
    /// 判据钉的是三路锚**逐字同形**，谁把其中一条改回无条件 push 就当场红。
    #[test]
    fn kg_and_ext_kb_routes_need_a_body_anchor() {
        let src = include_str!("retrieve.rs");
        let body = src
            .split("async fn search_report")
            .nth(1)
            .expect("search_report 没了");
        assert!(
            body.contains("lists.push(if direct_ids.is_empty() { Vec::new() } else { kg });"),
            "图谱路丢了正文锚 —— 无锚 PPR top-10 就是噪声"
        );
        assert!(
            body.contains("let ext_kb_map: Vec<(i64, ExtKbRecord)> = if direct_ids.is_empty() {"),
            "外部 KB 路丢了正文锚 —— 合成负 id 结构上不受直接命中约束"
        );
        // 关系路那条锚是这两条的原型，一起钉住（它先在，别被顺手改掉）
        assert!(
            body.contains("if direct_ids.is_empty() {
                Ok(Vec::new())"),
            "关系路的锚是这两条的原型"
        );
        // 🔴 五道已标定的相关度下限**钉值，不钉名字**（2026-08-16 复核逮到）。
        //
        // 上一版写的是 `for gate in [...] { assert!(src.contains(gate)) }`，而
        // `src = include_str!("retrieve.rs")` 是整份文件 —— **包含这个数组字面量本身**。
        // 五条断言恒真：把五个常量全删了它照样绿。这是本轮反复猎的「护栏自己会哑」，
        // 而它守的恰好是「不许调松阈值」这条纪律（调松会让 nohit 题变坏，
        // 而五个自动指标会一起假涨，没有任何判据会红）。
        //
        // 值闸不会哑：谁想调松，先来改这行数字，改的时候就得读上面那段。
        assert_eq!(VEC_MAX_DIST, 0.55, "向量距离上限被动了");
        assert_eq!(TRGM_MIN, 0.2, "正文相似下限被动了");
        assert_eq!(TITLE_MIN, 0.35, "标题相似下限被动了");
        assert_eq!(METADATA_MIN, 0.35, "元数据相似下限被动了");
        assert_eq!(TERMS_MIN_HITS, 2, "词级最少命中数被动了");
    }
    use super::*;

    fn hit(chunk_id: i64, doc: &str, ord: i32, score: f32) -> Hit {
        Hit {
            chunk_id,
            doc_id: doc.into(),
            doc_name: format!("{doc}.md"),
            folder_id: None,
            folder_path: "/".into(),
            ord,
            text: format!("块{ord}"),
            heading_path: String::new(),
            page: None,
            tags: Vec::new(),
            business_domain: None,
            effective_from: None,
            effective_to: None,
            source_uri: None,
            document_family: None,
            document_revision: None,
            source_hash: format!("hash-{doc}"),
            doc_updated_at: "2026-08-06 00:00:00+00".into(),
            channels: Vec::new(),
            relations: Vec::new(),
            score,
            merged: 1,
            // SQL 不取向量距离：候选集定下来之后统一回填一次（见 `fill_vec_dist`）
            vec_dist: None,
        }
    }

    #[test]
    fn query_normalization_is_conservative_and_stable() {
        assert_eq!(normalize_query("  请问：报销制度？\n"), "报销制度");
        assert_eq!(normalize_query("帮我查一下　DHT１５０－６／昨天"), "dht150-6 昨天");
        assert_eq!(normalize_query("ＡＢＣ_01.2"), "abc_01.2", "型号里的 _-. 必须保留");
        assert_eq!(normalize_query("\n\t？！？"), "");
    }

    // ==================== KB 审查③：无编号精确匹配路 ====================

    /// token 抽取：`[A-Za-z0-9-]{6,}`（输入是归一化后的问句），去重保序、超闸截断。
    #[test]
    fn exact_tokens_picks_order_and_model_numbers() {
        assert_eq!(exact_tokens("单号 so-2026-007 报了多少钱"), vec!["so-2026-007"]);
        assert_eq!(exact_tokens("dht150-6 昨天 dht150-6"), vec!["dht150-6"], "重复 token 去重");
        assert!(exact_tokens("报销上限是多少").is_empty(), "纯中文问句没有精确 token");
        assert!(exact_tokens("abc_01.2xy").is_empty(), "`_` 与 `.` 是切点，各段都不到 6 位");
        assert!(exact_tokens("abcde").is_empty(), "5 位不到门槛");
        assert_eq!(exact_tokens("abcdef"), vec!["abcdef"], "6 位恰好够");
        let many = (0..12).map(|i| format!("token{i:02}")).collect::<Vec<_>>().join(" ");
        assert_eq!(exact_tokens(&many).len(), EXACT_MAX_TOKENS, "超闸截断");
    }

    /// 精确路 SQL 的契约：ACL 内联（`$1`）、正文 ILIKE 任一模式（`$2`）、命中数排序 + LIMIT。
    #[test]
    fn exact_route_sql_matches_tokens_against_chunk_text() {
        assert!(EXACT_SQL.contains("c.doc_id = ANY($1::text[])"), "{EXACT_SQL}");
        assert!(EXACT_SQL.contains("c.text ILIKE ANY($2::text[])"), "{EXACT_SQL}");
        assert!(EXACT_SQL.contains("LIMIT $3") && EXACT_TOP == 20, "{EXACT_SQL}");
        assert!(EXACT_SQL.contains("WHERE c.text ILIKE t.p"), "多 token 时兼中者排前：{EXACT_SQL}");
        assert!(EXACT_SQL.contains("c.chunk_id"), "并列按 chunk_id（可复现）：{EXACT_SQL}");
        // 权重等同标题路：占据 RRF 第二槽（恒 1.0 的字面量区），不进可调权重
        assert_eq!(RrfWeights::default().route_array()[1], 1.0);
    }

    /// 没有 ≥6 位 ASCII token 就不发查询（`exact_ids` 的早退）：纯中文问句是常态，
    /// 空模式扫一遍可见块是纯浪费。早退的纯逻辑输入即 `exact_tokens` 的空 Vec 判据。
    #[test]
    fn exact_route_skips_when_no_token() {
        assert!(exact_tokens("报销上限是多少").is_empty());
        assert!(!exact_tokens("so-2026-007 的审批人").is_empty());
    }

    #[test]
    fn search_stats_default_is_an_explicit_zero_baseline() {
        let s = SearchStats::default();
        assert_eq!(s.visible_docs, 0);
        assert_eq!(s.vector_candidates + s.fts_candidates + s.trgm_candidates, 0);
        assert_eq!(
            s.title_candidates
                + s.metadata_candidates
                + s.relation_candidates
                + s.kg_candidates
                + s.ext_kb_candidates
                + s.fused_candidates,
            0
        );
    }

    /// 可见集合为空必须早退（否则召回 SQL 白发，还会让 HNSW 返回他人邻居再被滤空）
    #[test]
    fn empty_visible_set_short_circuits() {
        assert_eq!(scan_mode(0), None);
        assert_eq!(scan_mode(1), Some(Scan::Exact));
        assert_eq!(scan_mode(EXACT_SCAN_DOCS - 1), Some(Scan::Exact));
        assert_eq!(scan_mode(EXACT_SCAN_DOCS), Some(Scan::Hnsw));
    }

    /// trgm 阈值的定标数据：2026-07-29 连真库量的 `word_similarity(问句, 块正文)`
    /// （kb_eval 10 篇夹具 / 23 块 / 14 道 A 用户题）。`true` = 这一路该收住它。
    ///
    /// 只列**卡边界的**那些：上界由 KB02/KB16 的判据块（0.2105）钉住，
    /// 下界由 KB13（近域 nohit）的最高噪声块（0.1818）钉住。
    const TRGM_MEASURED: &[(&str, f32, bool)] = &[
        ("KB13 近域 nohit「差旅打车费」最高噪声块", 0.1818, false),
        ("KB02 判据块「发票 15 个工作日」", 0.2105, true),
        ("KB16 判据块「通讯补贴四档」", 0.2105, true),
        ("KB12 判据块「销售岗 430」", 0.2308, true),
        ("KB15 判据块「线下签字」", 0.2500, true),
        ("KB01 判据块「一线城市住宿 500 元/晚」", 0.2667, true),
        ("KB09 判据块「五项报销材料」", 0.2727, true),
        ("KB08 判据块「境外补贴 1250」", 0.3333, true),
        ("KB03 判据块「5000 元谁审批」", 0.3478, true),
        ("KB07 远域 nohit 最高噪声块", 0.0833, false),
        ("KB02 跨文档噪声（差旅补贴 xlsx）", 0.1250, false),
        ("KB06 判据块 trgm 达不到（靠向量路兜）", 0.0714, false),
    ];

    /// 🔴 trgm 阈值必须同时满足两头：收住实测判据块、挡住近域 nohit 的噪声。
    ///
    /// 由来：原值 0.3 把 KB01 的判据块（0.2667）挡在门外 → 14 题里只有 3 题能让 trgm 出结果，
    /// 而彼时的中文 FTS 那一路实测 322 格全为 0（`plainto_tsquery('simple')` 不切中文；
    /// 该路后被 KB 审查③换成单号/型号精确匹配），
    /// 于是「三路混合 + RRF」实际只有向量一路。
    #[test]
    fn trgm_threshold_matches_the_measured_distribution() {
        for (what, score, want) in TRGM_MEASURED {
            assert_eq!(
                *score > TRGM_MIN,
                *want,
                "{what}：实测 {score} 与阈值 {TRGM_MIN} 的关系变了（改阈值前先连库重量一遍）"
            );
        }
        // 阈值只能通过 `$3` 进 SQL —— 写死字面量就没法被上面这张表管住了
        assert!(TRGM_SQL.contains("word_similarity($2, text) > $3"), "{TRGM_SQL}");
    }

    /// 向量路相关度下限的定标数据：同一趟实测的余弦距离 `embedding <=> 问句向量`。
    /// `true` = 距离够近、该进 prompt。
    const VEC_MEASURED: &[(&str, f64, bool)] = &[
        ("KB10 判据块「9000」", 0.1863, true),
        ("KB09 判据块「五项报销材料」", 0.2466, true),
        ("KB11 判据块「170」（跨块的前一块）", 0.2550, true),
        ("KB10 判据块「旧版 4000」", 0.2555, true),
        ("KB08 判据块「1250」", 0.2909, true),
        ("KB11 判据块「860」（跨块的后一块）", 0.3037, true),
        ("KB01 判据块「500 元/晚」", 0.3058, true),
        ("KB03 判据块「总经理审批」", 0.3144, true),
        ("KB12 判据块「销售岗 430」", 0.3181, true),
        ("KB16 判据块「四档」", 0.3257, true),
        // 🔴 近域 nohit 的最近块比上面一半判据块都近：距离下限**结构上**分不出它。
        // 谁想靠调这个下限让 KB13 变绿，会先打死这一半正向题。它该由 keep_cited_only 兜。
        ("KB13 近域 nohit 最近块（下限管不到它）", 0.3395, true),
        ("KB02 判据块「15 个工作日」", 0.3544, true),
        ("KB05 判据块「口令 12 位」", 0.3834, true),
        ("KB15 判据块「线下签字」（txt 那份）", 0.4201, true),
        ("KB06 判据块（判据块里最远的：CSV 表格块）", 0.4926, true),
        ("KB08 第 4 近块（无关，改前也进 prompt）", 0.5520, false),
        ("KB06 第 2 近块（无关，改前也进 prompt）", 0.5619, false),
        ("KB07 远域 nohit·最近块", 0.6020, false),
        ("KB07 远域 nohit·次近块", 0.6145, false),
    ];

    /// 🔴 向量路必须有相关度下限，且它必须落在「判据块最远的那个」与「远域 nohit 最近的那个」之间。
    ///
    /// 由来：HNSW 恒返 `VEC_TOP` 条，问「库里根本没有的事」也有 6 块进 prompt ——
    /// 答对答错全押在模型肯不肯说「没有」上。实测改后 KB07 零命中（连 LLM 都不调），
    /// KB06 的 prompt 从 6 块（跨 4 篇文档）收到 1 块，而 14 题的判据载体块一个都没掉出。
    #[test]
    fn vector_floor_matches_the_measured_distribution() {
        for (what, dist, want) in VEC_MEASURED {
            assert_eq!(
                *dist < VEC_MAX_DIST,
                *want,
                "{what}：实测距离 {dist} 与上限 {VEC_MAX_DIST} 的关系变了（改上限前先连库重量一遍）"
            );
        }
        // 下限必须真的进了**两条**向量 SQL：漏掉 EXACT 那条等于「小可见集合下没有下限」，
        // 而那正是本仓最常走的一支（`scan_mode`：可见 doc < 50 篇走 Exact）。
        for sql in [VEC_SQL, VEC_SQL_EXACT] {
            assert!(
                sql.contains("AND (embedding <=> $2::vector) < $4"),
                "缺相关度下限：{sql}"
            );
            assert!(
                sql.contains("chunk_id LIMIT $3"),
                "向量距离并列时必须用 chunk_id 决胜：{sql}"
            );
        }
    }

    /// ACL 必须在检索 SQL 内（片段原样内联 + 空间过滤只占 $3）
    #[test]
    fn acl_fragment_is_inlined_not_post_filtered() {
        let s = visible_sql();
        assert!(s.contains(acl::visible_docs_sql()), "必须内联 acl::visible_docs_sql()");
        assert!(s.contains("x.enabled=true"), "停用文档不得参与检索");
        assert!(s.contains("x.status IN ('chunked','embedded')"), "失败/处理中文档不得参与检索");
        assert!(s.contains("EXISTS (SELECT 1 FROM kb.chunk xc WHERE xc.doc_id=x.doc_id)"));
        assert!(s.contains("x.effective_from IS NULL OR x.effective_from <= CURRENT_DATE"));
        assert!(s.contains("x.effective_to IS NULL OR x.effective_to >= CURRENT_DATE"));
        assert!(s.contains("$3::text IS NULL OR x.space_id = $3"));
        assert!(!s.contains("$4"), "只许用到 $1/$2/$3");
        // 所有召回路都按 doc_id 数组收口，没有任何一路是「查完再过滤」
        for sql in [VEC_SQL, EXACT_SQL, TRGM_SQL, TITLE_SQL, METADATA_SQL] {
            assert!(sql.contains("doc_id = ANY($1::text[])"), "{sql}");
        }
        assert!(TITLE_SQL.contains("d.name") && TITLE_SQL.contains("c.heading_path"), "{TITLE_SQL}");
        assert!(TITLE_SQL.contains("DISTINCT ON (c.doc_id)"), "每篇文档只许一个标题命中：{TITLE_SQL}");
        assert!(METADATA_SQL.contains("d.business_domain") && METADATA_SQL.contains("unnest(d.tags)"));
        assert!(METADATA_SQL.contains("d.source_uri") && METADATA_SQL.contains("DISTINCT ON (d.doc_id)"));
        assert!(METADATA_SQL.contains("d.document_family") && METADATA_SQL.contains("d.document_revision"));
        // Y7：AI 描述只许挂在 metadata 语料这一路，不许另起召回通道
        assert!(METADATA_SQL.contains("word_similarity($2, coalesce(d.description,''))"));
    }

    #[test]
    fn final_hit_load_rechecks_acl_and_keeps_related_versions_reviewable() {
        let s = load_hits_sql();
        assert!(s.contains(acl::visible_docs_sql()), "最终正文加载必须重放 ACL: {s}");
        assert!(s.contains("c.chunk_id = ANY($3::bigint[])"));
        assert!(s.contains("c.chunk_id = ANY($5::bigint[])"));
        assert!(s.contains("d.enabled=true"));
        assert!(s.contains("d.status IN ('chunked','embedded')"));
        assert!(s.contains("d.effective_from IS NULL OR d.effective_from <= CURRENT_DATE"));
        assert!(s.contains("d.effective_to IS NULL OR d.effective_to >= CURRENT_DATE"));
        assert!(s.contains("$4::text IS NULL OR d.space_id=$4"));
    }

    #[test]
    fn relation_expansion_rechecks_acl_and_uses_folder_metadata() {
        let s = relation_candidates_sql();
        assert_eq!(
            s.matches(acl::visible_docs_sql()).count(),
            2,
            "种子和扩展候选必须各自内联 ACL"
        );
        for contract in [
            "s.folder_id IS NOT NULL AND d.folder_id=s.folder_id",
            "btrim(s.folder_path)<>'/' AND btrim(d.folder_path)<>'/'",
            "btrim(d.folder_path)<>'/' AND btrim(s.folder_path)<>'/'",
            "left(btrim(s.folder_path),length(btrim(d.folder_path))+1)=btrim(d.folder_path)||'/'",
            "btrim(s.folder_path)<>'/' AND btrim(d.folder_path)<>'/'",
            "left(btrim(d.folder_path),length(btrim(s.folder_path))+1)=btrim(s.folder_path)||'/'",
            "word_similarity($5,d.folder_path)",
            "word_similarity($5,c.text) AS content_sim",
            "word_similarity($5,d.name)) AS support_sim",
            "btrim(d.document_family)=btrim(s.document_family)",
            "btrim(d.document_revision)=btrim(s.document_revision)",
            "NULLIF(btrim(s.business_domain),'') IS NOT NULL",
            "btrim(d.business_domain)=btrim(s.business_domain)",
            "JOIN unnest(d.tags) AS dt(tag) ON btrim(dt.tag)=btrim(st.tag)",
            "NULLIF(btrim(st.tag),'') IS NOT NULL",
            "JOIN kb.doc_link",
            "d.space_id=s.space_id",
            "EXISTS (SELECT 1 FROM kb.chunk existing WHERE existing.doc_id=d.doc_id)",
        ] {
            assert!(s.contains(contract), "关系扩展缺少合同 {contract}: {s}");
        }
        assert!(
            s.contains("WHEN relation IN ('references','referenced_by') THEN true")
                && s.contains("ELSE support_sim > $6 END")
                && RELATION_CONTEXT_MIN == TRGM_MIN,
            "显式链接可直接扩展；目录、版本族、业务域和标签必须由正文或标题相似度证明: {s}"
        );
        assert!(
            s.contains("content_sim,support_sim,rank_sim,support_sim AS pick_sim")
                && s.contains("ORDER BY doc_id,relation,pick_sim DESC"),
            "所有弱关系都必须按正文/标题支持度选段，目录只能参与同分排序: {s}"
        );
        assert!(
            !s.contains("d.folder_id IS NOT DISTINCT FROM s.folder_id"),
            "未分类文档不得因 folder_id=NULL 自动互相关联"
        );
        assert!(
            !s.contains("d.folder_path='/' AND s.folder_path<>'/'")
                && !s.contains("s.folder_path='/' AND d.folder_path<>'/'"),
            "未分类根路径不得伪装成全空间的祖先或后代: {s}"
        );
        assert!(s.contains("'descendant_folder',5"));
        assert!(s.contains("'same_domain',6") && s.contains("'shared_tag',7"));
        assert!(s.contains("candidate_docs AS") && s.contains("FROM related_docs r JOIN candidate_docs d"));
        assert!(s.contains("LIMIT $7") && RELATION_TOP == 10, "关系扩展候选上限不得膨胀");
        let candidates = s
            .split("candidate_docs AS")
            .nth(1)
            .unwrap()
            .split("related_docs AS")
            .next()
            .unwrap();
        assert!(
            !candidates.contains("CURRENT_DATE"),
            "候选池需保留历史版本；只有目录/域/标签分支继续限制当前有效期: {candidates}"
        );
    }

    #[test]
    fn unclassified_documents_form_no_folder_relation() {
        let sql = relation_candidates_sql();
        let same = sql.split("'same_folder'::text").nth(1).unwrap().split("UNION ALL").next().unwrap();
        let ancestor = sql.split("'ancestor_folder',4").nth(1).unwrap().split("UNION ALL").next().unwrap();
        let descendant = sql.split("'descendant_folder',5").nth(1).unwrap().split("UNION ALL").next().unwrap();
        for relation in [same, ancestor, descendant] {
            assert!(
                relation.contains("NULLIF(btrim(s.folder_path),'') IS NOT NULL")
                    && relation.contains("NULLIF(btrim(d.folder_path),'') IS NOT NULL")
                    && relation.contains("btrim(s.folder_path)<>'/'")
                    && relation.contains("btrim(d.folder_path)<>'/'"),
                "未分类文档不得形成 same/ancestor/descendant 目录关系: {relation}"
            );
        }
        assert!(ancestor.contains("s.folder_id IS NOT NULL AND d.folder_id IS NOT NULL"));
        assert!(descendant.contains("s.folder_id IS NOT NULL AND d.folder_id IS NOT NULL"));
        let domain = sql.split("'same_domain',6").nth(1).unwrap().split("UNION ALL").next().unwrap();
        let tag = sql.split("'shared_tag',7").nth(1).unwrap().split("UNION ALL").next().unwrap();
        for relation in [domain, tag] {
            assert!(
                relation.contains("s.folder_id IS NOT NULL AND d.folder_id IS NOT NULL")
                    && relation.contains("btrim(s.folder_path)<>'/'")
                    && relation.contains("btrim(d.folder_path)<>'/'"),
                "未分类文档不得因业务域或标签形成弱关联: {relation}"
            );
        }
        assert!(!sql.contains("d.folder_path='/' AND s.folder_path<>'/'"));
        assert!(!sql.contains("s.folder_path='/' AND d.folder_path<>'/'"));
    }

    #[test]
    fn citation_load_rechecks_acl_and_keeps_historical_versions_reviewable() {
        let anchor = citation_anchor_sql();
        assert!(anchor.contains(acl::visible_docs_sql()));
        assert!(anchor.contains("c.chunk_id=$3"));
        assert!(anchor.contains("d.enabled=true"));
        assert!(anchor.contains("d.status IN ('chunked','embedded')"));
        assert!(!anchor.contains("CURRENT_DATE"), "历史版本引用仍须可回查: {anchor}");
        let s = citation_hits_sql();
        assert!(s.contains(acl::visible_docs_sql()));
        assert!(s.contains("d.enabled=true"));
        assert!(s.contains("d.status IN ('chunked','embedded')"));
        assert!(!s.contains("CURRENT_DATE"), "历史版本引用正文仍须可回查: {s}");
    }

    #[test]
    fn retrieval_channels_are_explainable_and_stable() {
        let lists = vec![vec![7, 8], vec![], vec![8], vec![9, 7], vec![7], vec![10, 7]];
        assert_eq!(match_channels(7, &lists), ["向量", "标题", "元数据", "结构关联"]);
        assert_eq!(match_channels(8, &lists), ["向量", "正文相似"]);
        assert_eq!(match_channels(10, &lists), ["结构关联"]);
        // 第二槽是精确匹配路（KB 审查③ 起，原「全文」路已替换）
        let lists = vec![vec![], vec![42], vec![], vec![], vec![], vec![]];
        assert_eq!(match_channels(42, &lists), ["精确匹配"]);
    }

    #[test]
    fn relation_route_is_a_context_boost_not_a_direct_hit_replacement() {
        let out = rrf_weighted(
            &[vec![7], vec![], vec![], vec![], vec![], vec![9]],
            &[1.0, 1.0, 1.0, 1.0, METADATA_WEIGHT, RELATION_WEIGHT],
        );
        assert_eq!(out.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![7, 9]);
    }

    #[test]
    fn metadata_cannot_create_a_candidate_without_a_direct_content_or_title_hit() {
        let direct = vec![vec![7], vec![], vec![8], vec![9]];
        let mut metadata = vec![10, 8, 7];
        keep_auxiliary_votes_on_direct_hits(&direct, &mut metadata);
        assert_eq!(metadata, vec![8, 7]);
    }

    #[test]
    fn governed_versions_survive_the_final_limit_side_by_side() {
        let mut current = hit(91, "current", 0, 0.1);
        current.document_family = Some("报销制度".into());
        current.document_revision = Some("v2".into());
        let mut old = hit(92, "old", 0, 0.09);
        old.document_family = Some("报销制度".into());
        old.document_revision = Some("v1".into());
        let mut ranked: Vec<Hit> = (1..=6).map(|id| hit(id, &format!("d{id}"), 0, 1.0)).collect();
        ranked.extend([current, old]);
        let out = preserve_governed_versions(&ranked, ranked[..6].to_vec(), TOP_K);
        assert_eq!(out[..TOP_K].iter().map(|hit| hit.chunk_id).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6]);
        assert!(out.iter().any(|hit| hit.chunk_id == 91));
        assert!(out.iter().any(|hit| hit.chunk_id == 92));
    }

    #[test]
    fn textual_old_and_current_versions_survive_without_governance_metadata() {
        let mut current = hit(91, "current", 0, 0.1);
        current.doc_name = "报销制度新版.md".into();
        current.text = "新版上限 9000 元".into();
        let mut old = hit(92, "old", 0, 0.09);
        old.doc_name = "报销制度旧版.md".into();
        old.text = "旧版上限 4000 元".into();
        let mut ranked: Vec<Hit> = (1..=6).map(|id| hit(id, &format!("d{id}"), 0, 1.0)).collect();
        ranked.extend([current, old]);
        let out = preserve_textual_versions(&ranked, ranked[..6].to_vec(), TOP_K);
        assert_eq!(out[..TOP_K].iter().map(|hit| hit.chunk_id).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6]);
        assert!(out.iter().any(|hit| hit.chunk_id == 91));
        assert!(out.iter().any(|hit| hit.chunk_id == 92));
    }

    #[test]
    fn explicit_document_link_keeps_one_low_weight_context_candidate() {
        let selected: Vec<Hit> = (1..=TOP_K as i64).map(|id| hit(id, &format!("d{id}"), 0, 1.0)).collect();
        let mut linked = hit(91, "linked", 0, 0.01);
        linked.channels = vec!["结构关联".into()];
        linked.relations = vec!["references".into()];
        let mut ranked = selected.clone();
        ranked.push(linked);
        let out = append_explicit_context(&ranked, selected);
        assert_eq!(out.last().map(|hit| hit.chunk_id), Some(91));
    }

    #[test]
    fn rrf_sums_reciprocal_ranks() {
        // 7 两路都命中（rank1 + rank2）→ 1/61+1/62；9 只在一路 rank1 → 1/61；8 只在一路 rank2 → 1/62。
        // 「两路各排第二」因此能压过「一路排第一」——这正是 RRF 的目的（多路共识优先）
        let out = rrf(&[vec![7, 8], vec![9, 7]]);
        assert_eq!(out[0].0, 7);
        assert!((out[0].1 - (1.0 / 61.0 + 1.0 / 62.0)).abs() < 1e-6, "{:?}", out[0]);
        assert_eq!(out[1].0, 9);
        assert!((out[1].1 - 1.0 / 61.0).abs() < 1e-6);
        assert_eq!(out[2].0, 8);
        assert!((out[2].1 - 1.0 / 62.0).abs() < 1e-6);
    }

    /// 🔴 **RRF 同分时按语义距离决胜，不再按入库顺序**（2026-08-16 生产度量逼出来的）。
    ///
    /// 基线（20 题）：`recall@6 = 0.95` 而 `recall@1 = 0.15` —— 对的块召回得到、
    /// 就是排不到第一。0.80 的缺口 100% 落在排序层，不在召回层。
    /// 而 `merge_adjacent` 的收尾排序此前是 `score desc → chunk_id asc`：
    /// RRF 同分时**由入库顺序决胜**，用户看到的第一条引用与相关性无关。
    ///
    /// 典型同分形态（本测试构造的就是它）：A 只被向量路 rank1 命中、B 只被词级路 rank1
    /// 命中，两路权重都是 1.0 ⇒ 两块都是 `w/61`，完全同分。
    ///
    /// 次序键选**向量距离**而不是「每路各自归一分取最大」：后者是跨路比较，
    /// 而各路量纲完全不同（元数据路命中标签时 sim 恒 1.0、向量强命中只有 0.45），
    /// 得出的偏好序会与本文件字面量钉死的权重序**相反**。向量距离单一量纲、零标定。
    #[test]
    fn equal_rrf_scores_break_by_semantic_distance_not_insertion_order() {
        let mut near = hit(9, "d1", 0, 0.5);
        near.vec_dist = Some(0.20); // 语义更近，但 chunk_id 更大
        let mut far = hit(2, "d2", 0, 0.5);
        far.vec_dist = Some(0.50);
        let out = merge_adjacent(vec![far.clone(), near.clone()]);
        assert_eq!(out[0].chunk_id, 9, "同分时该由语义距离决胜，不是入库顺序");

        // 没有向量的块（外部 KB 合成块 / embed 降级）排在有距离的之后
        let mut blind = hit(1, "d3", 0, 0.5);
        blind.vec_dist = None;
        let out = merge_adjacent(vec![blind, near.clone()]);
        assert_eq!(out[0].chunk_id, 9, "无向量的块不许靠 chunk_id 小抢到第一");

        // 距离全等时仍按 chunk_id 升序 —— 可复现性一个字没松
        let mut a = hit(7, "d4", 0, 0.5);
        a.vec_dist = Some(0.30);
        let mut b = hit(3, "d5", 0, 0.5);
        b.vec_dist = Some(0.30);
        let out = merge_adjacent(vec![a, b]);
        assert_eq!(out[0].chunk_id, 3);

        // 🔴 距离**严格次级**：RRF 主分不同时，距离再近也不许翻越
        let mut low_but_near = hit(4, "d6", 0, 0.1);
        low_but_near.vec_dist = Some(0.01);
        let mut high_but_far = hit(6, "d7", 0, 0.9);
        high_but_far.vec_dist = Some(0.54);
        let out = merge_adjacent(vec![low_but_near, high_but_far]);
        assert_eq!(out[0].chunk_id, 6, "次序键不许翻越 RRF 主分");
    }

    /// 并列必须稳定（同分按 chunk_id 升序）：同一问句两次不同顺序无法排查
    #[test]
    fn rrf_ties_break_by_chunk_id() {
        assert_eq!(rrf(&[vec![5], vec![3]]).iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![3, 5]);
        assert!(rrf(&[]).is_empty());
        assert!(rrf(&[vec![]]).is_empty());
    }

    #[test]
    fn rrf_deduplicates_within_one_route() {
        let once = rrf(&[vec![7, 8]]);
        let dup = rrf(&[vec![7, 7, 8]]);
        assert_eq!(dup, once, "同一路重复 id 不许伪装成多路共识");
    }

    // ==================== 【Y3】RRF 权重入 settings ====================

    /// 🔴 等价锚点：默认配置与旧编译期常量**逐路**相等，且同一输入下融合结果逐字节一致。
    /// 这就是「权重入 settings 零行为变化」的证明件 —— 改四个 const 中的任何一个都会红。
    #[test]
    fn rrf_weights_default_is_byte_equivalent_to_legacy_consts() {
        let w = RrfWeights::default();
        assert_eq!(w.metadata, METADATA_WEIGHT);
        assert_eq!(w.relation, RELATION_WEIGHT);
        assert_eq!(w.kg, KG_WEIGHT);
        assert_eq!(w.ext_kb, EXT_KB_WEIGHT);
        assert_eq!(w.route_array(), [1.0, 1.0, 1.0, 1.0, METADATA_WEIGHT, RELATION_WEIGHT, KG_WEIGHT, EXT_KB_WEIGHT]);
        // 同一输入：默认权重数组 == 旧字面量数组（两遍融合各自验证一遍）
        let lists8 = vec![
            vec![1, 2], vec![2, 3], vec![4], vec![5], vec![6], vec![7], vec![8], vec![9],
        ];
        let legacy = [1.0, 1.0, 1.0, 1.0, METADATA_WEIGHT, RELATION_WEIGHT, KG_WEIGHT, EXT_KB_WEIGHT];
        assert_eq!(rrf_weighted(&lists8, &w.route_array()), rrf_weighted(&lists8, &legacy));
        let lists5: Vec<Vec<i64>> = lists8.into_iter().take(5).collect();
        assert_eq!(rrf_weighted(&lists5, &w.route_array()[..5]), rrf_weighted(&lists5, &legacy[..5]));
    }

    /// 配置闸：**拒绝**负值与 NaN/±Inf（报错，不 clamp）；0 与默认值放行。
    #[test]
    fn rrf_weights_validate_rejects_negative_and_non_finite() {
        assert!(RrfWeights::default().validate().is_ok());
        let zero = RrfWeights { metadata: 0.0, relation: 0.0, kg: 0.0, ext_kb: 0.0 };
        assert!(zero.validate().is_ok(), "0 = 该路不加权，合法");
        for bad in [-0.1, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let w = RrfWeights { kg: bad, ..RrfWeights::default() };
            assert!(w.validate().is_err(), "{bad} 必须被拒");
        }
        // 报错要点名是哪一路 —— 运维不该拿着「配置无效」猜是哪个字段
        let w = RrfWeights { ext_kb: -1.0, ..RrfWeights::default() };
        assert!(w.validate().unwrap_err().contains("ext_kb"));
    }

    /// serde 语义：键缺席/部分覆盖都回落旧值；键名打错硬失败（与 Settings 同一条纪律）。
    #[test]
    fn rrf_weights_serde_partial_override_and_unknown_key() {
        let w: RrfWeights = serde_json::from_str(r#"{"kg": 0.5}"#).unwrap();
        assert_eq!(w.kg, 0.5);
        assert_eq!(w.metadata, METADATA_WEIGHT, "没给的路必须仍是旧值");
        let full: RrfWeights = serde_json::from_str(
            r#"{"metadata": 0.1, "relation": 0.2, "kg": 0.3, "ext_kb": 0.4}"#,
        )
        .unwrap();
        assert_eq!(full.route_array()[4..], [0.1, 0.2, 0.3, 0.4]);
        assert!(serde_json::from_str::<RrfWeights>(r#"{"kgg": 0.5}"#).is_err(),
                "键名打错必须硬失败（deny_unknown_fields）");
    }

    #[test]
    fn finalize_deduplicates_text_and_diversifies_documents() {
        let mut duplicate = hit(12, "a", 4, 0.95);
        duplicate.text = " 相同\n正文 ".into();
        let mut original = hit(11, "a", 1, 1.0);
        original.text = "相同正文".into();
        let out = finalize_hits(vec![
            original,
            duplicate,
            hit(13, "a", 6, 0.9),
            hit(14, "a", 8, 0.8),
            hit(21, "b", 0, 0.7),
            hit(31, "c", 0, 0.6),
            hit(41, "d", 0, 0.5),
        ]);
        assert_eq!(out.len(), TOP_K);
        assert!(!out.iter().any(|h| h.chunk_id == 12), "同文档空白差异正文应去重");
        assert_eq!(
            out.iter().take(5).map(|h| h.doc_id.as_str()).collect::<Vec<_>>(),
            vec!["a", "a", "b", "c", "d"],
            "第一轮最多每篇两条，且保持原相关度顺序"
        );
        assert_eq!(out[5].chunk_id, 14, "第二轮用剩余高分候选补满");
    }

    #[test]
    fn duplicate_uploads_of_the_same_source_are_one_piece_of_evidence() {
        let mut first = hit(11, "a", 0, 1.0);
        first.text = "同一原件正文".into();
        first.source_hash = "same-file".into();
        let mut duplicate = hit(21, "b", 0, 0.9);
        duplicate.text = "同一原件正文".into();
        duplicate.source_hash = "same-file".into();
        let out = finalize_hits(vec![first, duplicate]);
        assert_eq!(out.len(), 1, "同一原件的重复上传不得伪装成两份独立佐证");
        assert_eq!(out[0].chunk_id, 11, "应保留融合分更高的命中");
    }

    #[test]
    fn one_source_keeps_multiple_distinct_passages_from_the_selected_upload() {
        let mut first = hit(11, "a", 0, 1.0);
        first.source_hash = "same-file".into();
        first.text = "第一条制度".into();
        let mut second = hit(13, "a", 2, 0.8);
        second.source_hash = "same-file".into();
        second.text = "第二条制度".into();
        let mut duplicate_upload = hit(21, "b", 0, 0.9);
        duplicate_upload.source_hash = "same-file".into();
        duplicate_upload.text = "第一条制度".into();
        let out = finalize_hits(vec![first, duplicate_upload, second]);
        assert_eq!(out.iter().map(|h| h.chunk_id).collect::<Vec<_>>(), vec![11, 13]);
    }

    #[test]
    fn adjacent_chunks_merge_keeping_first_id() {
        let out = merge_adjacent(vec![
            hit(11, "a", 1, 0.1),
            hit(12, "a", 2, 0.9),
            hit(13, "a", 3, 0.2),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].chunk_id, 11, "引用锚点取首块");
        assert_eq!(out[0].ord, 1);
        assert_eq!(out[0].text, "块1\n\n块2\n\n块3");
        assert!((out[0].score - 0.9).abs() < 1e-6, "分数取组内最大");
    }

    #[test]
    fn gap_and_cross_doc_do_not_merge() {
        let out = merge_adjacent(vec![
            hit(11, "a", 1, 0.5),
            hit(13, "a", 3, 0.4), // ord 不连续
            hit(21, "b", 2, 0.9), // 另一篇文档，ord 紧邻也不许拼
            hit(22, "b", 3, 0.1),
        ]);
        assert_eq!(out.len(), 3);
        assert_eq!(out.iter().map(|h| h.chunk_id).collect::<Vec<_>>(), vec![21, 11, 13]);
        assert_eq!(out[0].text, "块2\n\n块3");
        assert_eq!(out[1].text, "块1");
    }

    /// 合并后的 page 取组内第一个非空（首块常是无页码的表头/标题块）
    #[test]
    fn merged_page_falls_back_to_next_block() {
        let mut a = hit(11, "a", 1, 0.5);
        let mut b = hit(12, "a", 2, 0.4);
        a.page = None;
        b.page = Some(7);
        let out = merge_adjacent(vec![a, b]);
        assert_eq!(out[0].page, Some(7));
    }

    #[test]
    fn adjacent_old_and_current_sections_do_not_merge() {
        let mut old = hit(11, "a", 1, 0.8);
        old.heading_path = "历史口径".into();
        old.text = "旧版上限 4000 元".into();
        let mut current = hit(12, "a", 2, 0.9);
        current.heading_path = "现行口径".into();
        current.text = "现行版上限 9000 元".into();
        let out = merge_adjacent(vec![old, current]);
        assert_eq!(out.len(), 2, "同一文档里的冲突版本必须保持为两个可独立引用的来源");
    }

    #[test]
    fn merged_heading_falls_back_and_span_is_bounded() {
        let mut hits: Vec<Hit> = (0..=MAX_MERGE_SPAN)
            .map(|i| hit(100 + i as i64, "a", i as i32, 1.0 - i as f32 / 100.0))
            .collect();
        hits[1].heading_path = "第二章 > 审批".into();
        let out = merge_adjacent(hits);
        assert_eq!(out.len(), 2, "第 17 块必须另起一条，确保 span 可完整回查");
        assert_eq!(out[0].merged, MAX_MERGE_SPAN);
        assert_eq!(out[0].heading_path, "第二章 > 审批");
        assert_eq!(out[1].merged, 1);
    }

    /// 🔴 合并**必须**记跨度，否则引用还原不出模型看到的那一段。
    ///
    /// 实测的翻车形态：一条引用合并了 5 块、支撑答案的那句话在第 5 块，
    /// 而 `/api/kb/chunk/{id}` 的 `window` 被 `clamp(0,3)` 钉死 —— 读者点开引用**看不到那句话**，
    /// 而 `kb_eval` 的「引用块原文必须含关键词」那条校验因此把「引用其实有据」误判成缺关键词。
    /// 引用的全部价值在可核对；`merged` 是 `Citation.span` 的唯一来源。
    #[test]
    fn merge_adjacent_records_the_span() {
        // 连续 3 块 → 一条命中，跨度 3，chunk_id 取首块
        let out = merge_adjacent(vec![
            hit(11, "a", 0, 0.5),
            hit(12, "a", 1, 0.4),
            hit(13, "a", 2, 0.3),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].chunk_id, 11, "chunk_id 必须是首块（回查从它起算）");
        assert_eq!(out[0].merged, 3);
        // 不连续 → 不合并，各自跨度 1
        let split = merge_adjacent(vec![hit(21, "b", 0, 0.5), hit(22, "b", 5, 0.4)]);
        assert_eq!(split.len(), 2);
        assert!(split.iter().all(|h| h.merged == 1), "{split:?}");
        // 跨文档同 ord 也不许合并（否则引用会指到另一份文档）
        let cross = merge_adjacent(vec![hit(31, "c", 0, 0.5), hit(32, "d", 1, 0.4)]);
        assert_eq!(cross.len(), 2);
        assert!(cross.iter().all(|h| h.merged == 1), "{cross:?}");
    }

    // ===== B5 rerank：关闭零变化 / 开启重排 / 失败回退 =====

    /// 关闭态零变化的结构性锁：拆出来的两段拼回去必须与 `finalize_hits` 逐字节一致。
    /// （env 门控本身在 `connector::rerank` 测：base/model 缺一即 `None` → 走的还是 `finalize_hits`。）
    #[test]
    fn finalize_split_is_a_pure_refactor() {
        let fixture = || {
            vec![
                hit(11, "a", 1, 0.9),
                hit(12, "a", 2, 0.8), // 与上一块相邻 → 走合并分支
                hit(21, "b", 0, 0.7),
                hit(31, "c", 0, 0.6),
                hit(41, "d", 0, 0.5),
                hit(51, "e", 0, 0.4),
                hit(61, "f", 0, 0.3),
            ]
        };
        let shape = |v: &[Hit]| {
            v.iter().map(|h| (h.chunk_id, h.score, h.merged, h.text.clone())).collect::<Vec<_>>()
        };
        assert_eq!(shape(&finalize_hits(fixture())), shape(&finalize_ranked(rank_hits(fixture()))));
    }

    /// 最小 rerank HTTP 桩（与 connector/embed.rs 测试同款：不引新依赖）。
    /// 记录每次请求里的文档条数，按 `score_of` 给第 i 篇文档打分。
    async fn rerank_stub(
        score_of: fn(usize) -> f32,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<usize>>>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
        let s0 = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = l.accept().await else { return };
                let s = s0.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = Vec::new();
                    // 必须按 Content-Length 读满（embed.rs 测试桩同款教训）：半个 body 喂给
                    // serde_json 会 panic 在桩里，看起来像「客户端没发全」。
                    let head = loop {
                        if let Some(h) = find_sub(&buf, b"\r\n\r\n") {
                            if buf.len() >= h + 4 + content_len(&buf[..h]) {
                                break h;
                            }
                        }
                        let mut b = [0u8; 8192];
                        match sock.read(&mut b).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&b[..n]),
                        }
                    };
                    let v: serde_json::Value = serde_json::from_slice(&buf[head + 4..]).unwrap();
                    let n = v["documents"].as_array().unwrap().len();
                    s.lock().unwrap().push(n);
                    let results: Vec<serde_json::Value> = (0..n)
                        .map(|i| serde_json::json!({"index": i, "relevance_score": score_of(i)}))
                        .collect();
                    let body = serde_json::json!({"results": results}).to_string();
                    let _ = sock
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                });
            }
        });
        (base, seen)
    }

    fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    fn content_len(head: &[u8]) -> usize {
        String::from_utf8_lossy(head)
            .to_lowercase()
            .split("content-length:")
            .nth(1)
            .and_then(|t| t.split("\r\n").next())
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0)
    }

    /// 开启态：窗口（约 2×`TOP_K`）内按 rerank 分重排，窗口外候选 RRF 原序不动。
    #[tokio::test]
    async fn rerank_reorders_the_window_and_keeps_the_tail() {
        let (base, seen) = rerank_stub(|i| i as f32).await; // 越靠后分越高 → 窗口内应整体倒序
        let client = RerankClient::new(&base, None, "m");
        let n = RERANK_WINDOW + 3;
        let hits: Vec<Hit> = (0..n as i64)
            .map(|id| hit(id, &format!("d{id}"), 0, 1.0 - id as f32 / 100.0))
            .collect();
        let out = rerank_candidates(&client, "q", hits).await;
        let want: Vec<i64> =
            (0..RERANK_WINDOW as i64).rev().chain(RERANK_WINDOW as i64..n as i64).collect();
        assert_eq!(out.iter().map(|h| h.chunk_id).collect::<Vec<_>>(), want);
        assert_eq!(*seen.lock().unwrap(), vec![RERANK_WINDOW], "只有窗口内候选进精排");
    }

    /// 同分不许制造新顺序：全部同分时应与输入逐条一致（RRF 原序）。
    #[tokio::test]
    async fn rerank_ties_keep_the_rrf_order() {
        let (base, _) = rerank_stub(|_| 0.5).await;
        let client = RerankClient::new(&base, None, "m");
        let hits: Vec<Hit> = (0..4).map(|id| hit(id, &format!("d{id}"), 0, 1.0)).collect();
        let out = rerank_candidates(&client, "q", hits).await;
        assert_eq!(out.iter().map(|h| h.chunk_id).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
    }

    /// 失败回退：服务不可达 → 原序原样返回，一条都不许丢（warn 留痕在 `rerank_candidates` 内）。
    #[tokio::test]
    async fn rerank_failure_falls_back_to_rrf_order() {
        let client = RerankClient::new("http://127.0.0.1:1", None, "m");
        let hits: Vec<Hit> = (0..5).map(|id| hit(id, &format!("d{id}"), 0, 1.0)).collect();
        let out = rerank_candidates(&client, "q", hits).await;
        assert_eq!(out.iter().map(|h| h.chunk_id).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
    }

    /// 0/1 条候选没有可排的：不发 HTTP（连不可达地址都不碰），直接原样返回。
    #[tokio::test]
    async fn rerank_skips_trivial_inputs() {
        let client = RerankClient::new("http://127.0.0.1:1", None, "m");
        assert!(rerank_candidates(&client, "q", vec![]).await.is_empty());
        let one = rerank_candidates(&client, "q", vec![hit(7, "a", 0, 1.0)]).await;
        assert_eq!(one.iter().map(|h| h.chunk_id).collect::<Vec<_>>(), vec![7]);
    }

    // ===== B6 图谱召回：种子权重 / 扩散上限 / PPR / 回退 =====

    fn rel_edge(src: &str, dst: &str, w: i64) -> RelEdge {
        RelEdge { src: src.into(), dst: dst.into(), weight: w }
    }

    /// 种子权重契约：直接命中 1.0 / 邻边实体 0.8 / 二跳 0.0；KG_WEIGHT 与 RELATION_WEIGHT 同级单设。
    #[test]
    fn kg_seed_weights_follow_the_b6_contract() {
        assert_eq!(KG_SEED_DIRECT, 1.0);
        assert_eq!(KG_SEED_NEIGHBOR, 0.8);
        assert_eq!(KG_WEIGHT, 0.3);
        assert_eq!(KG_MAX_NODES, 200);
        // hop1 的边会重复出现在 hop2 的查询结果里（frontier 是超集）：先到的权重必须保留
        let out = diffuse_entities(
            &["e_a".to_string()],
            &[rel_edge("e_a", "e_b", 3)],
            &[rel_edge("e_a", "e_b", 3), rel_edge("e_b", "e_c", 1)],
            KG_MAX_NODES,
        );
        assert_eq!(
            out,
            vec![
                ("e_a".to_string(), KG_SEED_DIRECT),
                ("e_b".to_string(), KG_SEED_NEIGHBOR),
                ("e_c".to_string(), 0.0),
            ]
        );
    }

    /// 扩散上限：实体层截断优先保种子；组装侧实体 + chunk 合计不许超过 200。
    #[test]
    fn kg_diffusion_and_assembly_respect_the_node_cap() {
        let seeds = vec!["e_s".to_string()];
        let hop1: Vec<RelEdge> = (0..500).map(|i| rel_edge("e_s", &format!("e_{i}"), 1)).collect();
        let entities = diffuse_entities(&seeds, &hop1, &[], KG_MAX_NODES);
        assert_eq!(entities.len(), KG_MAX_NODES, "实体层必须在上限截断");
        assert_eq!(entities[0], ("e_s".to_string(), KG_SEED_DIRECT), "截断优先保种子");
        // 实体 150 → chunk 恰好补到 200
        let entities = diffuse_entities(&seeds, &hop1[..149], &[], KG_MAX_NODES);
        assert_eq!(entities.len(), 150);
        let chunks: Vec<(i64, i64)> = (0..500).map(|i| (i, 1)).collect();
        let pairs: Vec<(i64, String)> = vec![(0, "e_s".to_string())];
        let sg = assemble_subgraph(&entities, &hop1[..149], &chunks, &pairs, KG_MAX_NODES);
        assert_eq!(sg.teleport.len(), KG_MAX_NODES, "实体+chunk 合计不许超过上限");
        assert_eq!(sg.chunk_nodes.len(), 50);
        assert!(sg.edges.iter().all(|&(a, b, _)| a < KG_MAX_NODES && b < KG_MAX_NODES));
    }

    /// 组装纪律：悬空边（端点不在子图内）丢弃；chunk 名额按 SQL 度数序保序截断。
    #[test]
    fn kg_subgraph_assembly_drops_dangling_edges() {
        let entities =
            vec![("e_a".to_string(), 1.0), ("e_b".to_string(), 0.8), ("e_c".to_string(), 0.0)];
        let rel = vec![rel_edge("e_a", "e_b", 2), rel_edge("e_a", "e_out", 9)];
        let chunks = vec![(11, 2), (12, 1), (13, 1)];
        let pairs = vec![
            (11, "e_a".to_string()),
            (11, "e_b".to_string()),
            (12, "e_a".to_string()),
            (13, "e_c".to_string()),
        ];
        let sg = assemble_subgraph(&entities, &rel, &chunks, &pairs, 4);
        assert_eq!(sg.chunk_nodes.iter().map(|&(_, id)| id).collect::<Vec<_>>(), vec![11]);
        // e_out 不在子图 → (e_a, e_out) 这条边必须消失；(e_a, e_b) 留下
        assert_eq!(sg.edges.len(), 1 + 2, "{:?}", sg.edges); // rel 1 条 + 11 的两条 MENTIONS
        assert!(sg.edges.iter().all(|&(a, b, _)| a < 4 && b < 4));
    }

    /// PPR 收敛性 + 个性化偏向：稀疏小子图远早于上限收敛；rank 保持概率分布；
    /// 种子一侧压过远端；同一张图只翻转 teleport，优劣必须翻转。
    #[test]
    fn ppr_converges_and_personalization_pulls_toward_the_seed() {
        let edges = vec![(0usize, 1usize, 1.0f64), (1, 2, 1.0), (2, 3, 1.0)];
        let (rank, iters) = personalized_pagerank(4, &edges, &[KG_SEED_DIRECT, 0.0, 0.0, 0.0]);
        assert!(iters < KG_PPR_MAX_ITER, "收敛必须远早于迭代上限（实测几十轮）");
        let sum: f64 = rank.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6, "rank 必须保持概率分布：{sum}");
        assert!(rank[0] + rank[1] > rank[2] + rank[3], "种子一侧必须压过远端：{rank:?}");
        assert!(rank[1] > rank[2] && rank[2] > rank[3], "离种子越远分越低：{rank:?}");
        let (flipped, _) = personalized_pagerank(4, &edges, &[0.0, 0.0, 0.0, KG_SEED_DIRECT]);
        assert!(flipped[3] + flipped[2] > flipped[0] + flipped[1], "{flipped:?}");
    }

    /// 种子权重的真实效应：对称图里 1.0 种子压过 0.8 种子；权重互换则胜者互换。
    #[test]
    fn ppr_seed_weights_decide_the_winner() {
        let edges = vec![(0usize, 2usize, 1.0f64), (1, 2, 1.0)];
        let (rank, _) = personalized_pagerank(3, &edges, &[KG_SEED_DIRECT, KG_SEED_NEIGHBOR, 0.0]);
        assert!(rank[0] > rank[1], "1.0 种子必须压过 0.8 种子：{rank:?}");
        let (swapped, _) = personalized_pagerank(3, &edges, &[KG_SEED_NEIGHBOR, KG_SEED_DIRECT, 0.0]);
        assert!(swapped[1] > swapped[0]);
    }

    /// B6 语义全链：种子直提的 chunk 压过仅邻居提及的；双侧提及的压过单侧的。
    #[test]
    fn ppr_ranks_chunks_by_seed_proximity() {
        let entities = vec![("e0".to_string(), KG_SEED_DIRECT), ("e1".to_string(), KG_SEED_NEIGHBOR)];
        let rel = vec![rel_edge("e0", "e1", 2)];
        let chunks = vec![(4, 2), (2, 1), (3, 1)]; // SQL 度数序：双侧提及的 chunk 4 在前
        let pairs = vec![
            (4, "e0".to_string()),
            (4, "e1".to_string()),
            (2, "e0".to_string()),
            (3, "e1".to_string()),
        ];
        let sg = assemble_subgraph(&entities, &rel, &chunks, &pairs, KG_MAX_NODES);
        let (rank, iters) = personalized_pagerank(sg.teleport.len(), &sg.edges, &sg.teleport);
        assert!(iters < KG_PPR_MAX_ITER);
        let score = |cid: i64| {
            sg.chunk_nodes.iter().find(|&&(_, c)| c == cid).map(|&(i, _)| rank[i]).unwrap()
        };
        assert!(score(2) > score(3), "种子直提必须压过仅邻居提及：{:?}", sg.chunk_nodes);
        assert!(score(4) > score(2), "双侧提及必须压过单侧");
        assert_eq!(kg_top_chunks(&sg, &rank)[0], 4, "top1 必须是双侧提及的 chunk");
    }

    /// 全 dangling（零边）子图：PPR 不 NaN 不panic，chunk 同分按 chunk_id 升序（可复现）。
    #[test]
    fn kg_top_chunks_breaks_ties_by_chunk_id() {
        let sg = KgSubgraph {
            teleport: vec![KG_SEED_DIRECT, 0.0, 0.0],
            edges: vec![],
            chunk_nodes: vec![(1, 22), (2, 11)],
        };
        let (rank, _) = personalized_pagerank(sg.teleport.len(), &sg.edges, &sg.teleport);
        assert!(rank.iter().all(|r| r.is_finite()));
        assert_eq!(kg_top_chunks(&sg, &rank), vec![11, 22]);
    }

    /// env 门控：默认开；`off`（任意大小写/带空白）关闭。运行期读 env 的那层只是它的壳。
    #[test]
    fn kg_env_gate_defaults_on_and_honors_off() {
        assert!(kg_retrieval_enabled_env(None));
        assert!(kg_retrieval_enabled_env(Some("on")));
        assert!(kg_retrieval_enabled_env(Some("1")));
        assert!(!kg_retrieval_enabled_env(Some("off")));
        assert!(!kg_retrieval_enabled_env(Some(" OFF ")));
    }

    /// env 关闭 / 不限空间：一条图查询都不发（死池一碰就 Err，能拿到空 Vec 即证明没碰）。
    /// 这就是「关闭零变化」的结构性证明：第 7 路恒空 → RRF 与接入前逐字节一致。
    #[tokio::test]
    async fn kg_route_off_or_unscoped_never_touches_the_graph() {
        let pool = dms_connector::owned::dead_pg_pool_for_tests(std::time::Duration::from_millis(50));
        let docs = vec!["d1".to_string()];
        assert!(kg_route(&pool, Some("sp1"), false, &docs, "问句", &[1]).await.is_empty());
        assert!(kg_route(&pool, None, true, &docs, "问句", &[1]).await.is_empty());
        assert!(kg_route(&pool, Some("sp1"), true, &[], "问句", &[1]).await.is_empty());
    }

    /// 图查询失败（死池）→ 空路回退，绝不把错误传出去挂掉检索（warn 留痕在 kg_route 内）。
    #[tokio::test]
    async fn kg_route_failure_falls_back_to_the_original_routes() {
        let pool = dms_connector::owned::dead_pg_pool_for_tests(std::time::Duration::from_millis(50));
        let docs = vec!["d1".to_string()];
        assert!(kg_route(&pool, Some("sp1"), true, &docs, "问句", &[1]).await.is_empty());
    }

    /// 🔴 缺图回退不许静默：空种子必须区分「图没数据」（降级，warn）与「没命中实体」（正常空路）。
    #[test]
    fn kg_missing_graph_warns_instead_of_silently_degrading() {
        let src = include_str!("retrieve.rs");
        let body = src.split("async fn kg_route_inner").nth(1).expect("kg_route_inner 不见了");
        assert!(body.contains("space_has_chunks"), "空种子必须探测图是否有数据：{body}");
        assert!(body.contains("tracing::warn!"), "缺图降级必须 warn 留痕：{body}");
        let route = src.split("async fn kg_route(").nth(1).expect("kg_route 不见了");
        assert!(route.contains("tracing::warn!"), "查询失败必须 warn 留痕：{route}");
    }

    /// 第 7 路缺席/为空 → 与接入前六路逐字节一致；有候选 → 按 KG_WEIGHT 独立加分。
    #[test]
    fn kg_route_absent_or_empty_leaves_the_original_six_routes_untouched() {
        let lists6 = vec![vec![7, 8], vec![8], vec![], vec![9], vec![], vec![10]];
        let w6 = [1.0, 1.0, 1.0, 1.0, METADATA_WEIGHT, RELATION_WEIGHT];
        let mut lists7 = lists6.clone();
        lists7.push(vec![]);
        let w7 = [1.0, 1.0, 1.0, 1.0, METADATA_WEIGHT, RELATION_WEIGHT, KG_WEIGHT];
        assert_eq!(rrf_weighted(&lists6, &w6), rrf_weighted(&lists7, &w7), "缺席必须零变化");
        let mut lists7 = lists6;
        lists7.push(vec![11]);
        let out = rrf_weighted(&lists7, &w7);
        let entry = out.iter().find(|(id, _)| *id == 11).expect("图谱候选要进融合");
        assert!((entry.1 - KG_WEIGHT / (RRF_K + 1.0)).abs() < 1e-6, "{entry:?}");
    }

    /// 第 7 路的解释性：channels 里能说出「图谱」。
    #[test]
    fn kg_channel_is_explainable() {
        let lists = vec![vec![7], vec![], vec![], vec![], vec![], vec![], vec![7]];
        assert_eq!(match_channels(7, &lists), ["向量", "图谱"]);
        assert_eq!(match_channels(8, &lists), Vec::<String>::new());
    }

    // ===== B9 外部只读 KB：权重 / 关闭零变化 / 回退 / Hit 构造 =====

    fn ext_record(segment: &str, doc: &str, content: &str) -> ExtKbRecord {
        ExtKbRecord {
            segment_id: segment.into(),
            document_id: doc.into(),
            document_name: format!("{doc}.md"),
            content: content.into(),
            score: 0.5,
            source_uri: format!("http://dify.internal/v1/datasets/ds1#segment-{segment}"),
        }
    }

    /// 权重契约：第 8 路是与元数据同级的辅助加分项（0.2），不许压过正文直接命中（1.0 路）。
    #[test]
    fn ext_kb_weight_is_an_auxiliary_vote_not_a_load_bearing_route() {
        assert_eq!(EXT_KB_WEIGHT, 0.2);
        assert_eq!(EXT_KB_WEIGHT, METADATA_WEIGHT);
        assert!(EXT_KB_WEIGHT < 1.0 && EXT_KB_WEIGHT <= KG_WEIGHT);
        assert_eq!(EXT_KB_TOP, 4);
    }

    /// 合成 id 契约：恒负、次序保持；与本地（bigserial 恒正）chunk_id 永不撞。
    #[test]
    fn ext_kb_synthetic_ids_never_collide_with_local_chunk_ids() {
        let ids: Vec<i64> = (0..EXT_KB_TOP).map(ext_kb_synthetic_id).collect();
        assert_eq!(ids, vec![-1, -2, -3, -4]);
        assert!(ids.iter().all(|id| *id < 0), "本地 chunk_id 恒正，负 id 永不撞本地行");
    }

    /// 🔴 关闭零变化的结构性锁：第 8 路缺席/为空 → 与接入前七路逐字节一致；
    /// 有候选 → 按 EXT_KB_WEIGHT 独立加分。（env 门控本身在 `connector::external_kb` 测：
    /// base/dataset 缺一即 None → `ext_kb_route` 恒空 → 这里就是空 Vec 的那一支。）
    #[test]
    fn ext_kb_absent_or_empty_leaves_the_original_seven_routes_untouched() {
        let lists7 = vec![vec![7, 8], vec![8], vec![], vec![9], vec![], vec![10], vec![11]];
        let w7 = [1.0, 1.0, 1.0, 1.0, METADATA_WEIGHT, RELATION_WEIGHT, KG_WEIGHT];
        let mut lists8 = lists7.clone();
        lists8.push(vec![]);
        let w8 = [1.0, 1.0, 1.0, 1.0, METADATA_WEIGHT, RELATION_WEIGHT, KG_WEIGHT, EXT_KB_WEIGHT];
        assert_eq!(rrf_weighted(&lists7, &w7), rrf_weighted(&lists8, &w8), "缺席必须零变化");
        let mut lists8 = lists7;
        lists8.push(vec![ext_kb_synthetic_id(0)]);
        let out = rrf_weighted(&lists8, &w8);
        let entry = out.iter().find(|(id, _)| *id == -1).expect("外部候选要进融合");
        assert!((entry.1 - EXT_KB_WEIGHT / (RRF_K + 1.0)).abs() < 1e-6, "{entry:?}");
    }

    /// Hit 构造：只读、来源可看穿、不伪造本地治理字段。
    #[test]
    fn ext_kb_hit_is_readonly_and_marks_its_source() {
        let h = ext_kb_hit(-1, 0, &ext_record("s1", "d9", "外部正文"));
        assert_eq!(h.chunk_id, -1);
        assert_eq!(h.doc_id, "ext-kb:d9");
        assert_eq!(h.doc_name, "d9.md");
        assert_eq!(h.text, "外部正文");
        assert_eq!(
            h.source_uri.as_deref(),
            Some("http://dify.internal/v1/datasets/ds1#segment-s1"),
            "外部 KB 是独立授权源（配置即授权），source_uri 必须让用户能看穿来源"
        );
        assert!(
            h.source_hash.is_empty(),
            "远程块没有 sha256 可核对，绝不许伪造（dedup_sources 对空 hash 原样放行）"
        );
        assert!(
            h.document_family.is_none() && h.document_revision.is_none(),
            "远程版本/族不可核对，不进治理版本保全"
        );
        assert!(h.channels.is_empty() && h.relations.is_empty() && h.merged == 1);
        // document_id 缺失时退到 segment id：doc_id 仍稳定，多样化仍把它当独立文档分组
        let h = ext_kb_hit(-2, 1, &ext_record("s2", "", "无文档 id"));
        assert_eq!(h.doc_id, "ext-kb:s2");
    }

    /// 外部命中走完合并/去重/多样化仍是它自己：正文、来源、通道标注不丢。
    #[test]
    fn ext_kb_hit_survives_finalize_with_provenance_and_channel() {
        let mut local: Vec<Hit> = (1..=3).map(|id| hit(id, &format!("d{id}"), 0, 1.0)).collect();
        let mut ext = ext_kb_hit(ext_kb_synthetic_id(0), 0, &ext_record("s1", "d9", "外部正文"));
        ext.score = EXT_KB_WEIGHT / (RRF_K + 1.0); // 路内 rank1 的融合分：排在本地直接命中之后
        ext.channels = vec!["外部知识库".into()];
        local.push(ext);
        let out = finalize_hits(local);
        let got = out.iter().find(|h| h.chunk_id == -1).expect("外部命中不该被后处理吃掉");
        assert_eq!(got.text, "外部正文");
        assert_eq!(got.channels, vec!["外部知识库".to_string()]);
        assert!(got.source_uri.is_some() && got.merged == 1);
    }

    /// 回退纪律：未配置（None）零 IO 恒空；配置了但服务挂（连接即拒）→ 空路回退。
    /// 两条都绝不返 Err、绝不 panic（warn 留痕在 `ext_kb_route` 内）。
    #[tokio::test]
    async fn ext_kb_route_disabled_or_failed_falls_back_to_the_original_routes() {
        assert!(ext_kb_route(None, "问句").await.is_empty(), "未配置 = 功能关闭，零 IO");
        let client = ExtKbClient::new("http://127.0.0.1:1", None, "ds1");
        assert!(ext_kb_route(Some(&client), "问句").await.is_empty(), "失败/超时回退空路");
    }

    /// 第 8 路的解释性：channels 里能说出「外部知识库」；合成负 id 不会被本地路误领。
    #[test]
    fn ext_kb_channel_is_explainable() {
        let lists = vec![vec![7], vec![], vec![], vec![], vec![], vec![], vec![7], vec![-1]];
        assert_eq!(match_channels(-1, &lists), ["外部知识库"]);
        assert_eq!(match_channels(7, &lists), ["向量", "图谱"]);
        assert_eq!(match_channels(8, &lists), Vec::<String>::new());
    }

    // ==================== 词级稀疏召回路（第 9 路，对照 Yuxi BM25 半） ====================

    /// 词级路开关：默认开，`DMS_KB_TERMS=off` 关闭（闸形与图谱路逐字同款）。
    #[test]
    fn terms_route_env_gate_defaults_on_and_honors_off() {
        assert!(terms_route_enabled_env(None), "未配置默认开");
        assert!(terms_route_enabled_env(Some("on")));
        assert!(!terms_route_enabled_env(Some("off")));
        assert!(!terms_route_enabled_env(Some(" OFF ")), "大小写/空白不敏感");
        assert!(terms_route_enabled_env(Some("0")), "只认 off（与 DMS_KG_RETRIEVAL 同约）");
    }

    /// 词级路 SQL 的合同：ACL 内联（$1）+ GIN 重叠收口（$2）+ 命中词数门（$3）+ 名额（$4）。
    /// 排序 = 命中词数降序、chunk_id 决胜 —— 检索结果必须可复现。
    #[test]
    fn terms_route_sql_contract() {
        assert!(TERMS_SQL.contains("c.doc_id = ANY($1::text[])"), "ACL 可见集合必须内联：{TERMS_SQL}");
        assert!(TERMS_SQL.contains("c.terms && $2::text[]"), "GIN 重叠收口（至少中一词）：{TERMS_SQL}");
        assert!(TERMS_SQL.contains("WHERE hits >= $3"), "命中词数门走 bind（标定值不写死）：{TERMS_SQL}");
        assert!(TERMS_SQL.contains("ORDER BY hits DESC, chunk_id LIMIT $4"), "排序与决胜键：{TERMS_SQL}");
        assert!(TERMS_SQL.contains("q.w = ANY(c.terms)"), "命中词数按「不同问句词」计：{TERMS_SQL}");
        // 防爆闸只许截查询侧：truncate 必须出现在 terms_ids（查询）而不许碰 terms_of（写读共用）
        let src = include_str!("retrieve.rs");
        let body = src.split("async fn terms_ids").nth(1).unwrap().split("\n}\n").next().unwrap();
        assert!(body.contains("truncate(TERMS_MAX_QUERY_TERMS)"), "查询侧缺防爆闸：{body}");
        let shared = include_str!("store.rs");
        let tof = shared.split("pub fn terms_of").nth(1).unwrap().split("\n}\n").next().unwrap();
        assert!(!tof.contains("truncate"), "写侧词表不许截断（截断 = 召回缺臂）：{tof}");
    }

    /// 有效门槛：全局门槛与问句词数取小（单词问句不变哑）；词数≥门槛时门槛生效。
    #[test]
    fn terms_min_hits_clamps_to_query_term_count() {
        assert_eq!(terms_min_hits(1), 1, "单词问句恒按 1 收口，与全局门槛无关");
        assert_eq!(terms_min_hits(5), TERMS_MIN_HITS.min(5));
        assert_eq!(terms_min_hits(0), 1, "调用方空词表早退，这里只防御不返 0");
    }

    /// 词级路命中数门的定标数据：2026-08-11 连真库量（kb_eval 主题集夹具 + 4 篇既有真实文档，
    /// 逐题取「问句词 × 全语料各块命中数」分布；词级路候选经 `词级路候选` debug 日志全量核对）。
    /// 三元组 = (块, 命中词数, 问句词数)；`true` = 该块该入选（命中数 ≥ 有效门）。
    ///
    /// 分离度一句话：判据块最低 2（KB02），远域 nohit 噪声最高 1（KB07）—— 门 2 落在缝里。
    /// 近域 nohit（KB13）噪声 4 门管不到（同 `VEC_MAX_DIST` 的那条「近域分不出来」），答案层兜。
    const TERMS_MEASURED: &[(&str, i64, usize, bool)] = &[
        ("KB07 远域 nohit 最高噪声块（操作手册/品牌专区/体检/通讯，并列 4 块）", 1, 8, false),
        ("KB13 近域 nohit 最高噪声块（体检尾块 248）—— 近域门管不到，keep_cited_only 兜", 4, 7, true),
        ("KB02 判据块「发票 15 个工作日」（判据块里最低的）", 2, 4, true),
        ("KB03 判据块「5000 元总经理审批」", 3, 5, true),
        ("KB01 判据块「一线城市住宿 500 元/晚」", 4, 5, true),
        ("KB08 判据块「境外补贴 1250」", 3, 4, true),
        ("KB09 判据块「五项报销材料」", 4, 5, true),
        ("KB11 判据块「入职 170 天」（跨块的前一块）", 7, 9, true),
        ("KB11 判据块「自选 860」（跨块的后一块）", 7, 9, true),
        ("KB12 判据块「销售岗 430」", 4, 4, true),
        ("KB15 判据块「线下签字」", 5, 7, true),
        ("KB16 判据块「通讯补贴四档」", 4, 6, true),
        ("KB10 判据块「新版 9000」", 3, 7, true),
        ("KB10 判据块「旧版 4000」", 3, 7, true),
        // KB06 判据块只有 1 个问句词命中（CSV 表格块）—— 它一向靠向量路兜
        //（trgm 也只有 0.0714，见 TRGM_MEASURED），词级路收它进来才是噪声门失守
        ("KB06 判据块（CSV 表格块，词级路本就不该收）", 1, 4, false),
    ];

    /// 🔴 词级路命中数门必须同时满足两头：收住全部实测判据块、挡住远域 nohit 的噪声。
    #[test]
    fn terms_min_hits_matches_the_measured_distribution() {
        for (what, hits, n_terms, want) in TERMS_MEASURED {
            assert_eq!(
                *hits >= terms_min_hits(*n_terms),
                *want,
                "{what}：实测命中 {hits}/{n_terms} 与门 {TERMS_MIN_HITS} 的关系变了（改门前先连库重量一遍）"
            );
        }
        // 门只能通过 `$3` 进 SQL —— 写死字面量就没法被上面这张表管住了
        assert!(TERMS_SQL.contains("WHERE hits >= $3"), "{TERMS_SQL}");
    }

    /// 🔴 槽位纪律：词级路钉死在第 9 槽，前八槽通道名逐字不变（既有测试钉着前八槽）。
    #[test]
    fn terms_route_is_the_ninth_slot_and_keeps_the_first_eight_names() {
        assert_eq!(CHANNEL_NAMES.len(), 9);
        assert_eq!(CHANNEL_NAMES[8], "词级");
        assert_eq!(
            &CHANNEL_NAMES[..8],
            ["向量", "精确匹配", "正文相似", "标题", "元数据", "结构关联", "图谱", "外部知识库"]
        );
        // 词级路权重与四路正文同一个钉死档（1.0）：正文直接命中，不进 settings 可调区
        assert_eq!(TERMS_WEIGHT, 1.0);
        assert_eq!(RrfWeights::default().route_array()[0], TERMS_WEIGHT);
    }

    /// 词级路为空 = 与接入前**逐字节**等价（A/B 对照的干净基线）；
    /// 词级路非空时以 1.0 权重进 RRF —— 与向量路同 rank 同分。
    #[test]
    fn terms_route_empty_is_byte_equivalent_to_the_original_eight_routes() {
        let lists8: Vec<Vec<i64>> =
            vec![vec![1, 2], vec![3], vec![2, 4], vec![], vec![2], vec![5], vec![], vec![-1]];
        let w8 = RrfWeights::default().route_array();
        let base = rrf_weighted(&lists8, &w8);
        let mut lists9 = lists8.clone();
        lists9.push(Vec::new()); // 词级路缺席（env 关 / 问句分不出词）
        let mut w9 = [0.0f32; 9];
        w9[..8].copy_from_slice(&w8);
        w9[8] = TERMS_WEIGHT;
        assert_eq!(rrf_weighted(&lists9, &w9), base, "空词级路不许改变任何旧路的融合结果");

        // 词级路命中：与向量路同 rank 同权 → 同分（1.0 钉死档），两路共识者排前
        let mut lists9 = lists8;
        lists9.push(vec![2, 9]); // 词级路：2 是 rank1（它同时命中向量 rank2/正文 rank1）
        let out = rrf_weighted(&lists9, &w9);
        assert_eq!(out[0].0, 2, "三路共识必须排第一：{out:?}");
        let terms_only = out.iter().find(|(id, _)| *id == 9).expect("词级命中不能丢");
        let vec_rank1 = out.iter().find(|(id, _)| *id == 1).unwrap();
        assert!(
            (terms_only.1 - TERMS_WEIGHT / (RRF_K + 2.0)).abs() < 1e-6,
            "词级 rank2 的分必须是 1.0/(60+2)：{terms_only:?}"
        );
        assert!((vec_rank1.1 - 1.0 / (RRF_K + 1.0)).abs() < 1e-6);
    }

    /// 词级命中算**直接命中**：元数据票可以落在它上面（与四路正文同等待遇）。
    #[test]
    fn terms_hit_counts_as_a_direct_hit_for_metadata_votes() {
        let direct: [&Vec<i64>; 5] = [&vec![7], &vec![], &vec![], &vec![], &vec![42]]; // 42 仅词级命中
        let mut metadata = vec![42, 99];
        keep_auxiliary_votes_on_direct_hits(&direct, &mut metadata);
        assert_eq!(metadata, vec![42], "仅词级命中的块仍应被元数据增强；99 无直接命中必须剔");
    }

    /// 词级路的通道解释性：channels 里能说出「词级」。
    #[test]
    fn terms_channel_is_explainable() {
        let lists = vec![vec![7], vec![], vec![], vec![], vec![], vec![], vec![], vec![], vec![7, 8]];
        assert_eq!(match_channels(7, &lists), ["向量", "词级"]);
        assert_eq!(match_channels(8, &lists), ["词级"]);
    }

    /// SearchStats 的第 9 路计数：default 是显式零基线的一部分
    #[test]
    fn search_stats_carry_terms_candidates() {
        let s = SearchStats::default();
        assert_eq!(s.terms_candidates, 0);
    }
}
