//! 【知识导图 + 文档预览】知识库 HTTP 面的两个端点组。变更原因＝导图/预览协议。
//!
//! # 端点契约（路由注册在 `main.rs`，形状如下）
//!
//! ```text
//! .route("/api/kb/mindmap", get(kb_mindmap_api::mindmap))
//! .route("/api/kb/mindmap/regenerate", post(kb_mindmap_api::regenerate_mindmap))
//! .route("/api/kb/doc/{id}/markdown", get(kb_mindmap_api::doc_markdown))
//! .route("/api/kb/doc/{id}/chunks", get(kb_mindmap_api::doc_chunks))
//! // ③ 内容级导图（已在 `main.rs` 接线，注册行如下）：
//! .route("/api/kb/doc/{id}/sections", get(kb_mindmap_api::doc_sections))
//! ```
//!
//! ## ① 知识导图
//! - `GET /api/kb/mindmap?space_id=`（`space_id` 缺省＝登录名＝个人空间，同 `/api/kb/docs`）
//!   → `{ "space_id": "...", "root": { "label": "...", "children": [
//!       { "label": "...", "children": [ { "label": "...", "children": [
//!         { "label": "...", "doc_id": "..." } ] } ] } ] } }`
//!   不限层级：根＝空间，中间节点＝`folder_path` 的每一段，根目录文档归「未分类」，
//!   叶子＝文档。骨架按 `folder_path`/文档名**确定性**聚合（排序只依赖数据，不依赖 LLM），
//!   再让 fast 模型给一级分支改写主题标签；**LLM 失败/超时/答非所问一律回退纯结构骨架，
//!   不报错**——导图是导航面，目录名本身就是够用的标签。
//!   同一目录直接文档超过 12 篇时增加“目录→主题→文档”层；主题优先由 fast 模型严格分类，
//!   模型输出若有漏项、重复或越界则整目录回退到确定性的标题族分组。
//!   结果缓存 `meta.kv['kb_mindmap:v3:{space_id}']`（读写在 server 侧：knowledge 的纪律是不碰
//!   `meta.*`，SQL 复用 `admin_api::KV_GET_SQL/KV_SET_SQL` 那对口，不复述第三份）。缓存值内带
//!   当前空间名 + 文档 id/名称/目录路径的稳定指纹；GET 比较当前指纹后才命中，
//!   因此上传/删除/移动/改名无需在每条 mutation 路径手工删缓存。
//!   缓存是**空间级**的：`list_docs` 的可见性过滤内联在 SQL 里，任何通过 `space_readable`
//!   的 viewer 看到的文档集相同，故缓存对不同读者不泄露差异。
//! - `POST /api/kb/mindmap/regenerate` `{ "space_id": "...", "login_name"?, "role_code"? }`
//!   → 同上形状。强制重生成并覆盖缓存。**要 `space_writable`**：它改写共享缓存且每次
//!   都烧 fast LLM 额度，只读授权者不许反复触发（`insight_api` 那条「别让烧 LLM 额度
//!   比问数更便宜」的同款闸）。
//!
//! ## ② 文档预览（`kb.doc` 的解析产物＝`kb.chunk` 行；库表不存整篇文本）
//! - `GET /api/kb/doc/{id}/markdown` → `{ "name": "...", "markdown": "..." }`。
//!   正文按 `ord` 序从块文本重建，重叠尾巴用 `start_char_pos/end_char_pos` 去重
//!   （偏移是「trim 后文本在分块输入流中的区间」，见 `ingest::locate_offsets`）。
//!   文档无解析文本（pending/parsing/failed 或零块）→ **404 + 说明文案**。
//! - `GET /api/kb/doc/{id}/chunks` → `{ "chunks": [{ "ord", "text", "heading_path",
//!   "page", "start_char_pos", "end_char_pos" }] }`（空文档给空数组，不 404）。
//! - 两个都过 `acl::doc_for_viewer`：**不存在与不可见统一 403**（现有 kb_api 惯例，
//!   不泄露他人文档的存在性）。
//!
//! ## ③ 内容级导图（多级章节树；已在 `main.rs` 注册，见上）
//! - `GET /api/kb/doc/{id}/sections` → `{ "doc_id", "doc_name", "sections": [
//!   { "section", "chunk_count", "first_ord", "page", "excerpt", "children": [...同形...] } ] }`。
//!   章节树＝`kb.chunk.heading_path` 按 `" > "` 逐段建树（与 ingest 写入同口径），同路径段
//!   跨位置归并、块数子树累计；`excerpt`＝该节子树首块文本的空白压缩摘录（≤160 字），
//!   `page`/`first_ord` 同取首块；节序＝首块出现顺序（即 `ord` 序，确定性，不依赖 LLM）。
//!   空文档/无章节结构给 `sections: []` 或单个「未分节」节，不 404。
//! - 权限＝文档读：`doc_for_viewer` 闸 + 语句内联空间级读谓词双保险（与 `kb_api`
//!   摘录 SQL 同一模子：撤权若发生在两步之间，内联谓词让结果一行都返不出）。
//!
//! 身份解析与错误形状（`{"error": msg}`、400/403/404 映射）与 `kb_api` 逐字同口径——
//! 那几个 helper 是 kb_api 的私有函数，本文件按 `mcp_api`/`ds_api` 的既有做法各写一份
//! 薄壳，映射表一个字不许改。

use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use dms_kernel::{ChatModel, ChatRequest, ModelTier};
use dms_knowledge::{acl, store, Viewer};
use std::sync::Arc;

type ApiErr = (StatusCode, Json<serde_json::Value>);
type ApiOk = Json<serde_json::Value>;

/// 导图标签的 LLM 闸：超过它就算模型活着也当失败处理（回退骨架，不拖住导航页）。
const LLM_LABEL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// 一次最多请模型写多少个分支的标签（提示词与花费的有界闸）。
const MAX_LLM_BRANCHES: usize = 50;
/// 每个分支喂给模型的文档名条数（主题归纳只看苗头是够的）。
const MAX_NAMES_PER_BRANCH: usize = 12;
/// 模型标签的字符上限（导图节点要短；超长截断而不是拒收）。
const MAX_LABEL_CHARS: usize = 24;
/// 喂给模型的文档名截断长度（提示词体量闸，与 MAX_NAMES_PER_BRANCH 一族）。
const MAX_PROMPT_NAME_CHARS: usize = 40;
/// 单目录超过该值才增加主题层；小目录直接展开更省一次点击。
const DENSE_DOC_THRESHOLD: usize = 12;
/// 每个主题的目标体量与硬上限：119 篇会收敛为约 10 个主题，不再形成一条超长文档瀑布。
const DENSE_GROUP_TARGET: usize = 12;
const MAX_DENSE_GROUP_DOCS: usize = 16;
const MAX_DENSE_GROUPS: usize = 12;
/// 一次分类调用的输入闸；超出预算的目录直接走确定性标题族回退，不拖慢导图。
const MAX_DENSE_LLM_FOLDERS: usize = 4;
const MAX_DENSE_LLM_DOCS: usize = 160;
const MAX_DENSE_PROMPT_NAME_CHARS: usize = 64;

const LABEL_SYSTEM: &str = "你是企业知识库的主题归纳助手。给定若干分组（组名＝目录名，附组内文档名），\
为每个分组写一个不超过 12 字的主题标签。只输出 JSON 字符串数组，长度与分组数一致、顺序一致，\
不输出任何其他内容。";

const DENSE_GROUP_SYSTEM: &str =
    "你是企业知识库的文档分类器。输入包含若干目录及目录内带编号的文档。\
请按业务主题或文档族分类。只输出 JSON 数组，元素形如\
{\"folder\":1,\"groups\":[{\"label\":\"主题名\",\"docs\":[1,2]}]}。\
每个目录输出 2 到 12 组；每个文档编号必须且只能出现一次；不得创造、遗漏或重复编号；\
每组最多 16 篇，主题名不超过 12 字且同目录内不得重名；不要输出解释、Markdown 或其他字段。";

/// 响应体沿用 `{"error": msg}` 形状（前端只认这一种，同 kb_api）
fn err(code: StatusCode, msg: impl std::fmt::Display) -> ApiErr {
    (code, Json(serde_json::json!({ "error": msg.to_string() })))
}

/// `KbError` → HTTP：与 `kb_api::kb_err` 同一张 400/403/404 表（本端点族就是 `/api/kb/*`）。
fn kb_err(e: dms_knowledge::KbError) -> ApiErr {
    use dms_knowledge::KbError;
    let code = match &e {
        KbError::BadInput(_) => StatusCode::BAD_REQUEST,
        KbError::Forbidden(_) => StatusCode::FORBIDDEN,
        KbError::NotFound(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let message = match &e {
        KbError::BadInput(_) | KbError::Forbidden(_) | KbError::NotFound(_) => e.to_string(),
        // 5xx 两臂：细节全丢 + 无日志 = 排障零线索；原文进 warn，客户端仍只见固定文案
        KbError::Upstream(_) => {
            tracing::warn!(err = %e, "文档处理服务故障（对客户端收敛为固定文案）");
            "文档处理服务暂时不可用，请稍后重试".to_string()
        }
        KbError::Db(_) => {
            tracing::warn!(err = %e, "知识库 DB 故障（对客户端收敛为固定文案）");
            "知识库服务暂时不可用，请稍后重试".to_string()
        }
    };
    err(code, message)
}

/// 身份：与 `kb_api::viewer` 同口径（Bearer 会话优先，`login_name` 回退由同一个开关把守）。
async fn viewer(
    st: &AppState,
    headers: &HeaderMap,
    login_name: &Option<String>,
    role_code: &Option<String>,
) -> Result<Viewer, ApiErr> {
    let (login, role) = crate::resolve_identity(st, headers, login_name, role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name"))?;
    let p = crate::auth::load_principal(&st.auth_mysql, &login, role.as_deref())
        .await
        .map_err(|e| {
            // 底层错误原文只进日志：DB 故障不该被误判成身份问题（403 文案不透出是刻意的）
            tracing::warn!(login = %login, err = %e, "DMS 身份/角色查询失败");
            err(StatusCode::FORBIDDEN, "当前 DMS 身份或角色不可用")
        })?;
    Ok(Viewer::new(p.login_name, vec![p.role_code]))
}

/// 导图/重生成共用的 query/body 身份段（`space_id` 缺省＝个人空间，同 `docs`）。
#[derive(serde::Deserialize, Default)]
pub struct MindmapQuery {
    space_id: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct RegenerateReq {
    space_id: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// 导图节点：分支/根带 `children`，叶子带 `doc_id`（两种键互斥，由 skip 规则保证）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
struct MindNode {
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    doc_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    children: Vec<MindNode>,
}

/// 骨架聚合的输入（从 `DocRow` 抽出的最小三元组，纯函数好测）
struct SkelDoc {
    doc_id: String,
    name: String,
    folder_path: String,
}

/// `meta.kv` 的缓存键：v3 引入密集目录主题层，隔离旧的 v2 平铺缓存。
fn cache_key(space_id: &str) -> String {
    format!("kb_mindmap:v3:{space_id}")
}

/// 导图只依赖空间显示名和文档的这三个结构字段；状态/分块数的后台推进不应让
/// 导图重烧 LLM。先按内容排序再摘要，使 `list_docs` 返回顺序变化不会伪造失效。
fn source_fingerprint(space_label: &str, docs: &[SkelDoc]) -> String {
    fn field(ctx: &mut ring::digest::Context, value: &str) {
        ctx.update(&(value.len() as u64).to_be_bytes());
        ctx.update(value.as_bytes());
    }

    let mut ordered = docs.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| {
        (&a.doc_id, &a.name, &a.folder_path).cmp(&(&b.doc_id, &b.name, &b.folder_path))
    });
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    field(&mut ctx, "kb-mindmap-source-v1");
    field(&mut ctx, space_label);
    for doc in ordered {
        field(&mut ctx, &doc.doc_id);
        field(&mut ctx, &doc.name);
        field(&mut ctx, &doc.folder_path);
    }
    let digest = ctx.finish();
    let mut out = String::with_capacity(digest.as_ref().len() * 2);
    for byte in digest.as_ref() {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn cache_envelope(fingerprint: &str, body: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "fingerprint": fingerprint, "body": body })
}

fn cached_body(text: &str, fingerprint: &str) -> Option<serde_json::Value> {
    let cached: serde_json::Value = serde_json::from_str(text).ok()?;
    if cached.get("fingerprint")?.as_str()? != fingerprint {
        return None;
    }
    let body = cached.get("body")?.clone();
    body.get("root").is_some().then_some(body)
}

#[derive(Default)]
struct FolderBucket<'a> {
    docs: Vec<&'a SkelDoc>,
    children: std::collections::BTreeMap<&'a str, FolderBucket<'a>>,
}

fn folder_node(label: &str, mut bucket: FolderBucket<'_>) -> MindNode {
    let mut children = bucket
        .children
        .into_iter()
        .map(|(name, child)| folder_node(name, child))
        .collect::<Vec<_>>();
    bucket
        .docs
        .sort_by(|a, b| (&a.name, &a.doc_id).cmp(&(&b.name, &b.doc_id)));
    children.extend(bucket.docs.into_iter().map(|d| MindNode {
        label: d.name.clone(),
        doc_id: Some(d.doc_id.clone()),
        children: Vec::new(),
    }));
    MindNode {
        label: label.to_string(),
        doc_id: None,
        children,
    }
}

/// 标题族键：去扩展名、版本/日期/编号/括号尾巴后取稳定前缀；无可用前缀时退化为首字符。
/// 它只负责模型失败时的安全分桶，不假装理解具体业务词汇。
fn title_family(name: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem).trim();
    let mut out = String::new();
    for ch in stem.chars() {
        if ch.is_ascii_digit()
            || matches!(ch, '(' | '（' | '[' | '【' | '-' | '_' | ' ' | '·' | '—')
        {
            break;
        }
        if !ch.is_ascii_punctuation() {
            out.push(ch);
        }
        if out.chars().count() >= 8 {
            break;
        }
    }
    if out.chars().count() < 2 {
        out = stem
            .chars()
            .filter(|c| c.is_alphanumeric())
            .take(2)
            .collect();
    }
    if out.is_empty() {
        "其他".to_string()
    } else {
        out
    }
}

fn fallback_dense_groups(docs: Vec<MindNode>) -> Vec<MindNode> {
    let mut families: std::collections::BTreeMap<String, Vec<MindNode>> =
        std::collections::BTreeMap::new();
    for doc in docs {
        families
            .entry(title_family(&doc.label))
            .or_default()
            .push(doc);
    }
    // 标题族按稳定顺序装箱；一个族过大就切片，小族尽量相邻合并，每组硬封顶 16 篇。
    let mut groups: Vec<(Vec<String>, Vec<MindNode>)> = Vec::new();
    for (family, members) in families {
        for chunk in members.chunks(DENSE_GROUP_TARGET) {
            if let Some((labels, docs)) = groups.last_mut() {
                if docs.len() + chunk.len() <= MAX_DENSE_GROUP_DOCS {
                    if labels.last() != Some(&family) {
                        labels.push(family.clone());
                    }
                    docs.extend_from_slice(chunk);
                    continue;
                }
            }
            groups.push((vec![family.clone()], chunk.to_vec()));
        }
    }
    groups
        .into_iter()
        .enumerate()
        .map(|(i, (labels, children))| MindNode {
            label: match labels.as_slice() {
                [only] if only == "其他" => format!("其他资料 {}", i + 1),
                [only] => only.clone(),
                [first, ..] => format!("{}等", first),
                [] => format!("资料组 {}", i + 1),
            },
            doc_id: None,
            children,
        })
        .collect()
}

fn dense_fallback(root: &mut MindNode) {
    fn visit(node: &mut MindNode) {
        for child in &mut node.children {
            visit(child);
        }
        let direct_docs = node
            .children
            .iter()
            .filter(|child| child.doc_id.is_some())
            .count();
        if direct_docs <= DENSE_DOC_THRESHOLD {
            return;
        }
        let mut docs = Vec::with_capacity(direct_docs);
        let mut branches = Vec::new();
        for child in std::mem::take(&mut node.children) {
            if child.doc_id.is_some() {
                docs.push(child);
            } else {
                branches.push(child);
            }
        }
        branches.extend(fallback_dense_groups(docs));
        node.children = branches;
    }
    visit(root);
}

#[derive(serde::Deserialize)]
struct DenseReplyFolder {
    folder: usize,
    groups: Vec<DenseReplyGroup>,
}
#[derive(serde::Deserialize)]
struct DenseReplyGroup {
    label: String,
    docs: Vec<usize>,
}

fn dense_prompt(root: &MindNode) -> Option<(String, Vec<Vec<MindNode>>)> {
    fn collect(node: &MindNode, path: &str, out: &mut Vec<(String, Vec<MindNode>)>) {
        let next = if path.is_empty() {
            node.label.clone()
        } else {
            format!("{path}/{}", node.label)
        };
        let docs: Vec<MindNode> = node
            .children
            .iter()
            .filter(|c| c.doc_id.is_some())
            .cloned()
            .collect();
        if docs.len() > DENSE_DOC_THRESHOLD {
            out.push((next.clone(), docs));
        }
        for child in node.children.iter().filter(|c| c.doc_id.is_none()) {
            collect(child, &next, out);
        }
    }
    let mut folders = Vec::new();
    collect(root, "", &mut folders);
    if folders.is_empty()
        || folders.len() > MAX_DENSE_LLM_FOLDERS
        || folders
            .iter()
            .any(|(_, docs)| docs.len() > MAX_DENSE_LLM_DOCS)
    {
        return None;
    }
    let mut prompt = String::from("目录清单：\n");
    let mut docs_by_folder = Vec::new();
    for (fi, (path, docs)) in folders.into_iter().enumerate() {
        use std::fmt::Write as _;
        let _ = writeln!(prompt, "目录 {}「{}」：", fi + 1, path);
        for (di, doc) in docs.iter().enumerate() {
            let name: String = doc
                .label
                .chars()
                .take(MAX_DENSE_PROMPT_NAME_CHARS)
                .collect();
            let _ = writeln!(prompt, "{}. {}", di + 1, name);
        }
        docs_by_folder.push(docs);
    }
    Some((prompt, docs_by_folder))
}

fn parse_dense_groups(reply: &str, docs_by_folder: &[Vec<MindNode>]) -> Option<Vec<Vec<MindNode>>> {
    let start = reply.find('[')?;
    let end = reply.rfind(']')?;
    let raw: Vec<DenseReplyFolder> = serde_json::from_str(&reply[start..=end]).ok()?;
    if raw.len() != docs_by_folder.len() {
        return None;
    }
    let mut out = Vec::with_capacity(raw.len());
    for (expected, folder) in raw.into_iter().enumerate() {
        let docs = &docs_by_folder[expected];
        if folder.folder != expected + 1 || !(2..=MAX_DENSE_GROUPS).contains(&folder.groups.len()) {
            return None;
        }
        let mut seen = vec![false; docs.len()];
        let mut labels = std::collections::HashSet::new();
        let mut groups = Vec::with_capacity(folder.groups.len());
        for group in folder.groups {
            let label = group.label.trim();
            if label.is_empty()
                || label.chars().count() > 12
                || group.docs.is_empty()
                || group.docs.len() > MAX_DENSE_GROUP_DOCS
                || !labels.insert(label.to_string())
            {
                return None;
            }
            let mut children = Vec::with_capacity(group.docs.len());
            for index in group.docs {
                let slot = index.checked_sub(1)?;
                if slot >= docs.len() || std::mem::replace(&mut seen[slot], true) {
                    return None;
                }
                children.push(docs[slot].clone());
            }
            groups.push(MindNode {
                label: label.to_string(),
                doc_id: None,
                children,
            });
        }
        if seen.iter().any(|covered| !covered) {
            return None;
        }
        out.push(groups);
    }
    Some(out)
}

fn apply_dense_groups(root: &mut MindNode, grouped: Vec<Vec<MindNode>>) {
    fn visit(node: &mut MindNode, grouped: &mut std::vec::IntoIter<Vec<MindNode>>) {
        let dense =
            node.children.iter().filter(|c| c.doc_id.is_some()).count() > DENSE_DOC_THRESHOLD;
        // prompt/解析是前序遍历：先取本目录结果，再递归原有子目录；主题节点在递归后才嫁接，
        // 否则一个主题自己含 13 篇文档时会被误当成“待二次分层目录”。
        let current = dense.then(|| grouped.next().expect("已按同一棵树校验目录数量"));
        for child in node.children.iter_mut().filter(|c| c.doc_id.is_none()) {
            visit(child, grouped);
        }
        if dense {
            let mut branches: Vec<MindNode> = std::mem::take(&mut node.children)
                .into_iter()
                .filter(|child| child.doc_id.is_none())
                .collect();
            branches.extend(current.expect("dense 时已有本目录分组"));
            node.children = branches;
        }
    }
    let mut grouped = grouped.into_iter();
    visit(root, &mut grouped);
    debug_assert!(grouped.next().is_none(), "分类结果目录数应已完全消费");
}

/// 确定性骨架：完整保留目录树（根目录文档归「未分类」），同层目录按名称排序，
/// 目录内文档按 `(name, doc_id)` 排序。LLM 之后只许改写根下分支 `label`，
/// 结构与顺序与模型输出无关——模型挂了，导图只是标签朴素一点，形状不变。
fn build_skeleton(space_label: &str, docs: &[SkelDoc]) -> MindNode {
    let mut root = FolderBucket::default();
    for d in docs {
        let mut cursor = &mut root;
        for segment in d
            .folder_path
            .split('/')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            cursor = cursor.children.entry(segment).or_default();
        }
        cursor.docs.push(d);
    }
    let mut children = root
        .children
        .into_iter()
        .map(|(name, bucket)| folder_node(name, bucket))
        .collect::<Vec<_>>();
    if !root.docs.is_empty() {
        children.insert(
            0,
            folder_node(
                "未分类",
                FolderBucket {
                    docs: root.docs,
                    ..Default::default()
                },
            ),
        );
    }
    MindNode {
        label: space_label.to_string(),
        doc_id: None,
        children,
    }
}

fn collect_doc_names(node: &MindNode, names: &mut Vec<String>) {
    if node.doc_id.is_some() {
        names.push(node.label.chars().take(MAX_PROMPT_NAME_CHARS).collect());
    } else {
        for child in &node.children {
            if names.len() >= MAX_NAMES_PER_BRANCH {
                break;
            }
            collect_doc_names(child, names);
        }
    }
}

/// 给模型的素材：分支序号 + 目录名（即骨架标签）+ 前几条文档名。只取材于标签所需的最小集。
fn label_prompt(branches: &[MindNode]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("分组清单：\n");
    for (i, b) in branches.iter().enumerate() {
        let mut names = Vec::new();
        collect_doc_names(b, &mut names);
        let names = names.join("、");
        let _ = writeln!(out, "{}. 目录「{}」：{}", i + 1, b.label, names);
    }
    let _ = write!(out, "共 {} 个分组，请输出 {} 个主题标签的 JSON 数组。", branches.len(), branches.len());
    out
}

/// 解析模型回的标签数组：**全有或全无**——数量对不上、有空标签、JSON 不合法都整体回退
/// （半对半错的标签比目录名更误导）。容忍模型在数组外套 ```json 围栏或前后缀废话。
fn parse_labels(reply: &str, n: usize) -> Option<Vec<String>> {
    let start = reply.find('[')?;
    let end = reply.rfind(']')?;
    if end <= start {
        return None;
    }
    let raw: Vec<String> = serde_json::from_str(&reply[start..=end]).ok()?;
    if raw.len() != n {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for s in raw {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        out.push(t.chars().take(MAX_LABEL_CHARS).collect());
    }
    Some(out)
}

/// 把标签写回骨架（只动前 `labels.len()` 个分支的 `label`）；`None`＝保留纯结构骨架。
fn apply_labels(root: &mut MindNode, labels: Option<&[String]>) {
    if let Some(labels) = labels {
        for (branch, label) in root.children.iter_mut().zip(labels.iter()) {
            branch.label = label.clone();
        }
    }
}

/// 问 fast 模型要分支主题标签。任何失败（超时/传输/缺 content/解析不过）都是 `None`，
/// 由 `apply_labels` 落纯结构骨架——本函数**永不报错**，导图端点没有「LLM 失败」这个状态码。
async fn llm_labels(st: &AppState, root: &MindNode) -> Option<Vec<String>> {
    let branches = &root.children;
    if branches.is_empty() {
        return None;
    }
    let sent = &branches[..branches.len().min(MAX_LLM_BRANCHES)];
    let user = label_prompt(sent);
    let req = ChatRequest::text(ModelTier::Fast, LABEL_SYSTEM, &user, Some(0.1));
    let reply = match tokio::time::timeout(LLM_LABEL_TIMEOUT, st.llm.chat(req)).await {
        Ok(Ok(reply)) => reply,
        Ok(Err(e)) => {
            tracing::warn!(err = %e, "导图分支标签 fast 调用失败 → 回退目录名骨架");
            return None;
        }
        Err(_) => {
            tracing::warn!("导图分支标签 fast 调用超时 → 回退目录名骨架");
            return None;
        }
    };
    let Some(content) = reply.content.as_deref() else {
        // 传输与超时分支上面都有 warn，这条缺 content 的路径不能无声
        tracing::warn!("导图分支标签 fast 调用无 content → 回退目录名骨架");
        return None;
    };
    parse_labels(content, sent.len())
}

/// 密集目录主题分类：严格 JSON 全量校验，任一目录漏项/重复/越界都整体返回 None。
/// 调用方随后走 `dense_fallback`，所以模型故障永远不会丢文档或打坏导图。
async fn llm_dense_groups(st: &AppState, root: &MindNode) -> Option<Vec<Vec<MindNode>>> {
    let (user, docs_by_folder) = dense_prompt(root)?;
    let req = ChatRequest::text(ModelTier::Fast, DENSE_GROUP_SYSTEM, &user, Some(0.0));
    let reply = match tokio::time::timeout(LLM_LABEL_TIMEOUT, st.llm.chat(req)).await {
        Ok(Ok(reply)) => reply,
        Ok(Err(e)) => {
            tracing::warn!(err = %e, "导图密集目录分类失败 → 回退确定性标题族");
            return None;
        }
        Err(_) => {
            tracing::warn!("导图密集目录分类超时 → 回退确定性标题族");
            return None;
        }
    };
    let content = reply.content.as_deref()?;
    parse_dense_groups(content, &docs_by_folder)
}

/// 空间显示名（根节点标签）；空名回退 `space_id` 本身。
async fn space_label(st: &AppState, space_id: &str) -> Result<String, ApiErr> {
    let row: Option<(String,)> = st
        .owned
        .fixed("SELECT name FROM kb.space WHERE space_id=$1")
        .bind(space_id)
        .fetch_optional()
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用，请稍后重试"))?;
    Ok(row
        .map(|(name,)| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| space_id.to_string()))
}

/// 一次取出导图的完整结构输入；缓存比对与未命中后生成共用同一份快照。
/// `list_docs` 的 viewer 可见性过滤内联在它自己的 SQL 里，本函数不拼第二份 ACL。
async fn load_mindmap_source(
    st: &AppState,
    v: &Viewer,
    space_id: &str,
) -> Result<(String, Vec<SkelDoc>, String), ApiErr> {
    let rows = store::list_docs(&st.owned, v, space_id)
        .await
        .map_err(kb_err)?;
    let docs: Vec<SkelDoc> = rows
        .into_iter()
        .map(|r| SkelDoc { doc_id: r.doc_id, name: r.name, folder_path: r.folder_path })
        .collect();
    let label = space_label(st, space_id).await?;
    let fingerprint = source_fingerprint(&label, &docs);
    Ok((label, docs, fingerprint))
}

/// 从已取快照生成一棵新导图（不含缓存读写）。
async fn generate_mindmap(
    st: &AppState,
    space_id: &str,
    label: &str,
    docs: &[SkelDoc],
) -> serde_json::Value {
    let mut root = build_skeleton(label, docs);
    // 两个 fast 增强互不依赖，并行后仍由纯函数依序落树；最坏等待保持单个 20s 闸而非叠成 40s。
    let (groups, labels) = tokio::join!(llm_dense_groups(st, &root), llm_labels(st, &root));
    if let Some(groups) = groups {
        apply_dense_groups(&mut root, groups);
    } else {
        dense_fallback(&mut root);
    }
    apply_labels(&mut root, labels.as_deref());
    serde_json::json!({ "space_id": space_id, "root": root })
}

/// 写缓存失败只记 warn 不报错：导图已经生成出来，没有理由因为缓存写不进让用户看见 500
/// （代价只是下次 GET 重新生成一次）。
async fn write_cache(st: &AppState, space_id: &str, fingerprint: &str, body: &serde_json::Value) {
    // Value 的 Display 即紧凑 JSON（序列化不可失败，unwrap_or_else 的 fallback 是死代码）
    let text = cache_envelope(fingerprint, body).to_string();
    if let Err(e) = st
        .owned
        .fixed(crate::admin_api::KV_SET_SQL)
        .bind(cache_key(space_id))
        .bind(text)
        .execute()
        .await
    {
        tracing::warn!(space_id = %space_id, err = %e, "导图缓存写入失败（本次仍返回新结果）");
    }
}

/// 缓存写放后台：响应路径不白等一个 RTT（写失败本来就只 warn）。
fn spawn_write_cache(
    st: &Arc<AppState>,
    space_id: &str,
    fingerprint: &str,
    body: &serde_json::Value,
) {
    let st = st.clone();
    let space_id = space_id.to_string();
    let fingerprint = fingerprint.to_string();
    let body = body.clone();
    tokio::spawn(async move { write_cache(&st, &space_id, &fingerprint, &body).await });
}

/// `space_id` 缺省＝个人空间；trim 后空串按缺省、超长拒——与 `kb_eval_api::normalize_space`
/// 同口径（那份是它模块私有的拿不出来，同族端点保持同一闸形）。
fn normalize_space(v: &Viewer, space_id: Option<&str>) -> Result<String, ApiErr> {
    let s = space_id.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&v.login);
    if s.chars().count() > 64 {
        return Err(err(StatusCode::BAD_REQUEST, "space_id 不能超过 64 字符"));
    }
    Ok(s.to_string())
}

/// `GET /api/kb/mindmap?space_id=`：当前结构指纹与缓存一致才命中；
/// 未命中/过期/缓存损坏则生成后落缓存。
pub async fn mindmap(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<MindmapQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = viewer(&st, &headers, &q.login_name, &q.role_code).await?;
    let space_id = normalize_space(&v, q.space_id.as_deref())?;
    if !acl::space_readable(&st.owned, &v, &space_id).await.map_err(kb_err)? {
        return Err(err(StatusCode::FORBIDDEN, format!("无权访问知识空间 {space_id}")));
    }
    let (label, docs, fingerprint) = load_mindmap_source(&st, &v, &space_id).await?;
    // 缓存只是加速器：读失败不该 500，按未命中重新生成（与缓存损坏同口径）
    let cached: Option<(String,)> = match st
        .owned
        .fixed(crate::admin_api::KV_GET_SQL)
        .bind(cache_key(&space_id))
        .fetch_optional()
        .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::warn!(space_id = %space_id, err = %e, "导图缓存读取失败，按未命中重新生成");
            None
        }
    };
    if let Some((text,)) = cached {
        // 缓存过期/损坏（手改/旧版形状）按未命中处理：覆盖写回，不报错。
        if let Some(body) = cached_body(&text, &fingerprint) {
            return Ok(Json(body));
        }
    }
    let body = generate_mindmap(&st, &space_id, &label, &docs).await;
    spawn_write_cache(&st, &space_id, &fingerprint, &body);
    Ok(Json(body))
}

/// `POST /api/kb/mindmap/regenerate`：强制重生成并覆盖缓存。改写共享缓存 + 每次都烧
/// LLM 额度，故要 `space_writable`（fail-closed），不是只读可触发。
pub async fn regenerate_mindmap(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RegenerateReq>,
) -> Result<ApiOk, ApiErr> {
    let v = viewer(&st, &headers, &req.login_name, &req.role_code).await?;
    let space_id = normalize_space(&v, req.space_id.as_deref())?;
    if !acl::space_writable(&st.owned, &v, &space_id).await.map_err(kb_err)? {
        return Err(err(StatusCode::FORBIDDEN, format!("无权修改知识空间 {space_id} 的导图")));
    }
    let (label, docs, fingerprint) = load_mindmap_source(&st, &v, &space_id).await?;
    let body = generate_mindmap(&st, &space_id, &label, &docs).await;
    spawn_write_cache(&st, &space_id, &fingerprint, &body);
    Ok(Json(body))
}

/// 一块解析产物（`kb.chunk` 行的预览投影；偏移可能为 NULL＝未能可靠定位）
struct PreviewChunk {
    ord: i32,
    text: String,
    heading_path: String,
    page: Option<i32>,
    start_char_pos: Option<i32>,
    end_char_pos: Option<i32>,
}

/// 预览是整篇取回：块数上界就是 `kb.doc.chunk_count`（上传 50MB 封顶），调用方已先过
/// `doc_for_viewer`，与下载原文同级的授权面，故不设额外 LIMIT。
const PREVIEW_CHUNKS_SQL: &str = "SELECT ord,text,heading_path,page,start_char_pos,end_char_pos \
     FROM kb.chunk WHERE doc_id=$1 ORDER BY ord";

async fn load_chunks(st: &AppState, doc_id: &str) -> Result<Vec<PreviewChunk>, ApiErr> {
    let rows: Vec<(i32, String, String, Option<i32>, Option<i32>, Option<i32>)> = st
        .owned
        .fixed(PREVIEW_CHUNKS_SQL)
        .bind(doc_id)
        .fetch_all()
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用，请稍后重试"))?;
    Ok(rows
        .into_iter()
        .map(|(ord, text, heading_path, page, start_char_pos, end_char_pos)| PreviewChunk {
            ord,
            text,
            heading_path,
            page,
            start_char_pos,
            end_char_pos,
        })
        .collect())
}

/// 从块重建整篇文本：general 分块带重叠尾巴，用字符偏移把已覆盖的头部跳掉
/// （`start/end_char_pos` 覆盖的是 **trim 后**的文本，见 `ingest::locate_offsets`）；
/// 偏移缺失的块全文保留——预览宁可是带重复的原文，也不做猜出来的删减。
/// 退化跨度（`e <= s`）同按缺失处理：落入全文保留分支。
fn markdown_from_chunks(chunks: &[PreviewChunk]) -> String {
    let mut out = String::new();
    let mut covered_end: i64 = 0; // 已拼进 out 的流区间右端（字符计）
    for c in chunks {
        let text = c.text.trim();
        if text.is_empty() {
            continue;
        }
        let piece = match (c.start_char_pos, c.end_char_pos) {
            (Some(s), Some(e)) if e > s && (s as i64) < covered_end => {
                let skip = (covered_end - s as i64) as usize;
                text.chars().skip(skip).collect::<String>().trim_start().to_string()
            }
            _ => text.to_string(),
        };
        if piece.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&piece);
        if let (Some(_), Some(e)) = (c.start_char_pos, c.end_char_pos) {
            covered_end = covered_end.max(e as i64);
        }
    }
    out
}

/// `GET /api/kb/doc/{id}/markdown` → `{name, markdown}`。无解析文本 404（文案说明原因）；
/// 不存在与不可见统一 403（`doc_for_viewer` 惯例）。
pub async fn doc_markdown(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<MindmapQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = viewer(&st, &headers, &q.login_name, &q.role_code).await?;
    let row = acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
    let chunks = load_chunks(&st, &id).await?;
    let markdown = markdown_from_chunks(&chunks);
    if markdown.is_empty() {
        return Err(err(
            StatusCode::NOT_FOUND,
            "文档暂无解析文本（可能仍在解析中、解析失败或已被删除），请稍后重试或重新上传",
        ));
    }
    Ok(Json(serde_json::json!({ "name": row.name, "markdown": markdown })))
}

/// `GET /api/kb/doc/{id}/chunks` → `{chunks:[...]}`（原样透出块的全部定位字段；
/// 空文档给空数组）。可见性闸同 `doc_markdown`。
pub async fn doc_chunks(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<MindmapQuery>,
) -> Result<ApiOk, ApiErr> {
    let v = viewer(&st, &headers, &q.login_name, &q.role_code).await?;
    let _ = acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
    let chunks = load_chunks(&st, &id).await?;
    let mut out = Vec::with_capacity(chunks.len());
    for c in &chunks {
        out.push(serde_json::json!({
            "ord": c.ord,
            "text": c.text,
            "heading_path": c.heading_path,
            "page": c.page,
            "start_char_pos": c.start_char_pos,
            "end_char_pos": c.end_char_pos,
        }));
    }
    Ok(Json(serde_json::json!({ "chunks": out })))
}

// ════════════════════════ ③ 内容级导图（章节分桶）════════════════════════
//
// 已在 `main.rs` 接线（注册行如下，路由变动时两边同步改）：
//   .route("/api/kb/doc/{id}/sections", get(kb_mindmap_api::doc_sections))
// 用途：导图文档节点展开到章节级（顶层 heading 分桶 + 块数徽标），点章节出摘要卡（首块摘录）。
// 形态与 `kb_api::ops_pack` 同一模子（子模块 + glob re-export，接线方写
// `kb_mindmap_api::doc_sections` 即可）。
mod content_pack {
    use super::*;

    /// 摘要卡摘录上限（字符）：只给苗头，全文走 `/chunks` 或 `/markdown`
    const SECTION_EXCERPT_CHARS: usize = 160;
    /// 单文档顶层章节数上限（导图节点有界闸；超出截断，不报错）
    const MAX_SECTIONS: usize = 100;
    /// 空 heading_path（扫描件/图片 OCR 等无标题结构）的桶名
    const NO_SECTION: &str = "未分节";

    /// 一节的内容级投影：标题 + 块数徽标（**子树累计**）+ 首块定位（ord/页码）+ 首块摘录 + 子章节
    struct SectionNode {
        section: String,
        chunk_count: usize,
        first_ord: i32,
        page: Option<i32>,
        excerpt: String,
        children: Vec<SectionNode>,
    }

    /// heading_path **多级章节树**（纯函数，单测钉住）：`"A > B > C"` 逐段建树；同路径段
    /// 跨位置归并（按全路径前缀匹配，如 `A>x` 与 `A>y` 合挂 A 下）；块数 = 子树累计；
    /// 摘录/定位取子树内按 ord 的首块（输入必须按 ord 序，SQL 保证）。总节点数达
    /// MAX_SECTIONS 后不再新建（已存在的路径继续累计块数）——导图节点有界闸，截断不报错。
    fn section_tree(chunks: &[PreviewChunk]) -> Vec<SectionNode> {
        let mut total = 0usize;
        let mut roots: Vec<SectionNode> = Vec::new();
        'chunks: for c in chunks {
            let mut segs: Vec<&str> = c
                .heading_path
                .split(" > ")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if segs.is_empty() {
                segs.push(NO_SECTION);
            }
            let mut level = &mut roots;
            for seg in segs {
                let node = match level.iter().position(|n| n.section == seg) {
                    Some(i) => &mut level[i],
                    None => {
                        if total >= MAX_SECTIONS {
                            // 满闸后只往已存在的路径累计（父级在下行前已 +=1），不再开新节点
                            continue 'chunks;
                        }
                        total += 1;
                        level.push(SectionNode {
                            section: seg.to_string(),
                            chunk_count: 0,
                            first_ord: c.ord,
                            page: c.page,
                            excerpt: clip_excerpt(&c.text),
                            children: Vec::new(),
                        });
                        level.last_mut().expect("刚 push 的节点必在")
                    }
                };
                node.chunk_count += 1;
                level = &mut node.children;
            }
        }
        roots
    }

    /// 递归序列化：每节带 `children`（叶子为空数组），前端按同形递归嫁接进导图
    fn section_json(node: &SectionNode) -> serde_json::Value {
        serde_json::json!({
            "section": display_section_name(&node.section, &node.excerpt),
            "chunk_count": node.chunk_count,
            "first_ord": node.first_ord,
            "page": node.page,
            "excerpt": node.excerpt,
            "children": node.children.iter().map(section_json).collect::<Vec<_>>(),
        })
    }

    /// OCR 页桶的展示名：「第 N 页（OCR）」对用户零信息（2026-08-12 用户：不能显示为 OCR）。
    /// 用首块摘录的开头当内容标签，页码收进后缀（「市场费用申请-费用项目明细…（第 11 页）」）。
    /// 摘录空时保留原名（总比空标签强）。
    fn display_section_name(section: &str, excerpt: &str) -> String {
        if !(section.starts_with("第 ") && section.contains("页（OCR")) {
            return section.to_string();
        }
        let hint: String = excerpt.chars().take(16).collect();
        let hint = hint.trim();
        if hint.is_empty() {
            return section.to_string();
        }
        let page = section.replace("（OCR）", "");
        format!("{hint}…（{page}）")
    }

    /// 章节 SQL。调用方已过 `doc_for_viewer`，这里仍内联一次空间级读谓词——撤权若发生在
    /// 两步之间，本语句一行都返不出（同 `kb_api` 描述摘录 SQL 的两步内联理由）。
    const SECTIONS_SQL: &str =
        "SELECT c.ord,c.heading_path,c.page,c.text FROM kb.chunk c JOIN kb.doc d ON d.doc_id = c.doc_id
         WHERE c.doc_id = $1
           AND EXISTS (SELECT 1 FROM kb.space s WHERE s.space_id = d.space_id
             AND (s.owner = $2 OR EXISTS (SELECT 1 FROM kb.acl a
               WHERE a.scope = 'space' AND a.target_id = s.space_id
                 AND a.perm IN ('read','write')
                 AND ((a.grantee_kind = 'login' AND a.grantee = $2)
                   OR (a.grantee_kind = 'role' AND a.grantee = ANY($3::text[]))))))
         ORDER BY c.ord";

    /// 首块摘录：压缩一切空白串为单个空格（块文本里的换行/缩进对摘要卡只是噪音），
    /// 封顶 `SECTION_EXCERPT_CHARS` 字符。
    fn clip_excerpt(text: &str) -> String {
        let mut out = String::new();
        let mut len = 0usize; // 字符计数器：chars().count() 每字符重扫是 O(n²)
        let mut pending_space = false;
        for c in text.trim().chars() {
            if c.is_whitespace() {
                pending_space = true;
                continue;
            }
            if pending_space && !out.is_empty() {
                out.push(' ');
                len += 1;
            }
            pending_space = false;
            out.push(c);
            len += 1;
            if len >= SECTION_EXCERPT_CHARS {
                break;
            }
        }
        out
    }

    /// `GET /api/kb/doc/{id}/sections` → `{doc_id, doc_name, sections:[...]}`。
    /// 空文档给空数组（与 `doc_chunks` 同口径）；不存在与不可见统一 403。
    pub async fn doc_sections(
        State(st): State<Arc<AppState>>,
        headers: HeaderMap,
        Path(id): Path<String>,
        Query(q): Query<MindmapQuery>,
    ) -> Result<ApiOk, ApiErr> {
        let v = viewer(&st, &headers, &q.login_name, &q.role_code).await?;
        // 先过可见性闸再取块（与 doc_markdown/doc_chunks 同序，守卫测试钉住）
        let row = acl::doc_for_viewer(&st.owned, &v, &id).await.map_err(kb_err)?;
        let rows: Vec<(i32, String, Option<i32>, String)> = st
            .owned
            .fixed(SECTIONS_SQL)
            .bind(&id)
            .bind(&v.login)
            .bind(&v.roles)
            .fetch_all()
            .await
            .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "知识库服务暂时不可用，请稍后重试"))?;
        let chunks: Vec<PreviewChunk> = rows
            .into_iter()
            .map(|(ord, heading_path, page, text)| PreviewChunk {
                ord,
                text,
                heading_path,
                page,
                start_char_pos: None,
                end_char_pos: None,
            })
            .collect();
        let sections = section_tree(&chunks);
        Ok(Json(serde_json::json!({
            "doc_id": row.doc_id,
            "doc_name": row.name,
            "sections": sections.iter().map(section_json).collect::<Vec<_>>(),
        })))
    }

    #[cfg(test)]
    mod content_tests {
        use super::*;

        fn hchunk(ord: i32, heading_path: &str, text: &str, page: Option<i32>) -> PreviewChunk {
            PreviewChunk {
                ord,
                text: text.into(),
                heading_path: heading_path.into(),
                page,
                start_char_pos: None,
                end_char_pos: None,
            }
        }

        /// 章节树：按路径段分层、节序＝首块出现序、同路径跨位置归并、块数子树累计、定位取首块
        #[test]
        fn sections_build_nested_tree_in_first_seen_order() {
            let chunks = vec![
                hchunk(0, "总则 > 目的", "总则首块。", Some(1)),
                hchunk(1, "总则 > 适用范围", "总则第二块。", Some(1)),
                hchunk(2, "报销流程 > 申请", "报销首块。", Some(3)),
                hchunk(3, "总则 > 附则", "总则第三块（跨位置仍归总则）。", Some(9)),
                hchunk(4, "报销流程 > 审批 > 店长", "审批首块。", Some(4)),
                hchunk(5, "报销流程 > 审批 > 区经", "审批第二块。", Some(5)),
            ];
            let secs = section_tree(&chunks);
            assert_eq!(secs.len(), 2);
            assert_eq!(secs[0].section, "总则");
            assert_eq!(secs[0].chunk_count, 3, "父节块数 = 子树累计");
            assert_eq!(secs[0].first_ord, 0);
            assert_eq!(secs[0].page, Some(1));
            assert_eq!(secs[0].excerpt, "总则首块。", "摘录必须取自子树首块");
            let names: Vec<&str> = secs[0].children.iter().map(|c| c.section.as_str()).collect();
            assert_eq!(names, ["目的", "适用范围", "附则"], "二级节按首块序、跨位置归并");
            assert_eq!(secs[1].section, "报销流程");
            assert_eq!(secs[1].chunk_count, 3);
            assert_eq!(secs[1].children.len(), 2, "申请 + 审批");
            let approve = &secs[1].children[1];
            assert_eq!(approve.section, "审批");
            assert_eq!(approve.chunk_count, 2);
            let deep: Vec<&str> = approve.children.iter().map(|c| c.section.as_str()).collect();
            assert_eq!(deep, ["店长", "区经"], "三级节同样展开（用户核心诉求：有节点就能展）");
            assert!(section_tree(&[]).is_empty());
        }

        /// 空 heading（扫描件/OCR）归「未分节」；摘录压缩空白且按字符封顶
        #[test]
        fn sections_fallback_and_excerpt_clipping() {
            let long = format!("{}　\n\n  {}", "甲".repeat(200), "乙".repeat(50));
            let chunks = vec![hchunk(0, "", &long, None), hchunk(1, "  ", "第二块", None)];
            let secs = section_tree(&chunks);
            assert_eq!(secs.len(), 1);
            assert_eq!(secs[0].section, NO_SECTION);
            assert_eq!(secs[0].chunk_count, 2);
            assert!(secs[0].children.is_empty());
            assert_eq!(secs[0].excerpt.chars().count(), SECTION_EXCERPT_CHARS);
            assert!(!secs[0].excerpt.contains(char::is_whitespace), "摘录不许带空白: {}", secs[0].excerpt);
        }

        /// OCR 页桶在导图上显示内容含义（摘录开头），页码收后缀；普通章节名一字不动
        #[test]
        fn ocr_page_nodes_display_content_hint_not_the_ocr_label() {
            assert_eq!(
                display_section_name("第 11 页（OCR）", "市场费用申请-费用项目明细表单 报销需…"),
                "市场费用申请-费用项目明细表单…（第 11 页）"
            );
            assert_eq!(display_section_name("第 11 页（OCR）", ""), "第 11 页（OCR）", "空摘录保留原名");
            assert_eq!(display_section_name("04 市场费用申请-合计费用", "随便"), "04 市场费用申请-合计费用");
        }

        /// 有界闸：总节点数封顶 `MAX_SECTIONS`（截断不报错；满闸后已有路径仍累计块数）
        #[test]
        fn sections_are_capped() {
            fn node_count(nodes: &[SectionNode]) -> usize {
                nodes.iter().map(|n| 1 + node_count(&n.children)).sum()
            }
            let chunks: Vec<PreviewChunk> = (0..(MAX_SECTIONS + 20))
                .map(|i| hchunk(i as i32, &format!("第{i}章"), "文", None))
                .collect();
            assert_eq!(section_tree(&chunks).len(), MAX_SECTIONS);
            let mut chunks: Vec<PreviewChunk> = (0..MAX_SECTIONS)
                .map(|i| hchunk(i as i32, &format!("第{i}章 > 节"), "文", None))
                .collect();
            chunks.push(hchunk(999, "第0章 > 节", "回头块", None));
            let secs = section_tree(&chunks);
            assert_eq!(node_count(&secs), MAX_SECTIONS, "多级路径下封顶的是总节点数");
            assert_eq!(secs[0].chunk_count, 2, "满闸后已有路径继续累计");
            assert_eq!(secs[0].children[0].chunk_count, 2, "子节同样累计");
        }

        /// 端点顺序闸（源码级）：doc_sections 必须先过 `doc_for_viewer` 再执行章节 SQL
        #[test]
        fn sections_endpoint_gates_visibility_before_query() {
            let src = include_str!("kb_mindmap_api.rs");
            let body = src.split("pub async fn doc_sections").nth(1).unwrap();
            // handler 在子模块内（缩进 4），函数体到第一个 4 空格缩进的 `}` 为止
            let body = body.split("\n    }\n").next().unwrap();
            let gate = body.find("doc_for_viewer").unwrap();
            let query = body.find("SECTIONS_SQL").unwrap();
            assert!(gate < query, "doc_sections 必须先过可见性再取块: {body}");
        }
    }
}

// 与 `kb_api::ops_pack` 同一模子：glob re-export 让接线方写 `kb_mindmap_api::doc_sections` 即可。
// 已在 main.rs 接线，re-export 被真正使用（`unused_imports` 的 allow 已随接线删除）。
pub(crate) use content_pack::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn skel(doc_id: &str, name: &str, folder_path: &str) -> SkelDoc {
        SkelDoc {
            doc_id: doc_id.into(),
            name: name.into(),
            folder_path: folder_path.into(),
        }
    }

    fn chunk(ord: i32, text: &str, span: Option<(i32, i32)>) -> PreviewChunk {
        PreviewChunk {
            ord,
            text: text.into(),
            heading_path: String::new(),
            page: None,
            start_char_pos: span.map(|(s, _)| s),
            end_char_pos: span.map(|(_, e)| e),
        }
    }

    /// 骨架聚合：完整目录分层、根目录归「未分类」、排序确定（同样的输入乱序给同一棵树）
    #[test]
    fn skeleton_preserves_full_folder_hierarchy_deterministically() {
        let docs = vec![
            skel("d4", "补贴办法.pdf", "/财务/报销"),
            skel("d2", "报销制度 v2.pdf", "/财务"),
            skel("d1", "招聘流程.docx", "/人事"),
            skel("d3", "随手记.txt", "/"),
        ];
        let tree = build_skeleton("张三的知识库", &docs);
        assert_eq!(tree.label, "张三的知识库");
        assert!(tree.doc_id.is_none());
        let labels: Vec<&str> = tree.children.iter().map(|b| b.label.as_str()).collect();
        // BTreeMap 按段名排：""（未分类）< "人事" < "财务"（字节序）
        assert_eq!(labels, vec!["未分类", "人事", "财务"], "{labels:?}");
        let finance = &tree.children[2];
        let leaves: Vec<(&str, Option<&str>)> = finance
            .children
            .iter()
            .map(|l| (l.label.as_str(), l.doc_id.as_deref()))
            .collect();
        assert_eq!(
            leaves,
            vec![("报销", None), ("报销制度 v2.pdf", Some("d2"))]
        );
        assert_eq!(finance.children[0].children[0].label, "补贴办法.pdf");
        assert_eq!(
            finance.children[0].children[0].doc_id.as_deref(),
            Some("d4")
        );
        // 乱序输入同一棵树
        let shuffled: Vec<SkelDoc> = docs
            .iter()
            .rev()
            .map(|d| skel(&d.doc_id, &d.name, &d.folder_path))
            .collect();
        assert_eq!(build_skeleton("张三的知识库", &shuffled), tree);
    }

    #[test]
    fn skeleton_handles_edge_folder_paths_and_empty_space() {
        // "//" 与 "/" 都归根目录；无文档＝空 children（端点侧会跳过 LLM）
        let docs = vec![skel("d1", "a.pdf", "//")];
        let tree = build_skeleton("s", &docs);
        assert_eq!(tree.children.len(), 1);
        assert_eq!(tree.children[0].label, "未分类");
        assert!(build_skeleton("s", &[]).children.is_empty());
    }

    fn doc_ids(node: &MindNode, out: &mut Vec<String>) {
        if let Some(id) = &node.doc_id {
            out.push(id.clone());
        }
        for child in &node.children {
            doc_ids(child, out);
        }
    }

    #[test]
    fn dense_folder_fallback_adds_topic_layer_without_losing_docs() {
        let docs: Vec<SkelDoc> = (0..24)
            .map(|i| skel(&format!("d{i}"), &format!("业务资料{i:02}.pdf"), "/运营"))
            .collect();
        let mut tree = build_skeleton("s", &docs);
        dense_fallback(&mut tree);
        let folder = &tree.children[0];
        assert!(folder.children.len() >= 2 && folder.children.len() <= MAX_DENSE_GROUPS);
        assert!(
            folder.children.iter().all(|node| node.doc_id.is_none()),
            "密集目录下必须先到主题节点"
        );
        let mut ids = Vec::new();
        doc_ids(folder, &mut ids);
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), docs.len(), "每篇文档必须且只能出现一次");
    }

    #[test]
    fn small_folder_does_not_gain_an_extra_topic_layer() {
        let docs: Vec<SkelDoc> = (0..DENSE_DOC_THRESHOLD)
            .map(|i| skel(&format!("d{i}"), &format!("资料{i}.pdf"), "/运营"))
            .collect();
        let mut tree = build_skeleton("s", &docs);
        dense_fallback(&mut tree);
        assert!(tree.children[0]
            .children
            .iter()
            .all(|node| node.doc_id.is_some()));
    }

    #[test]
    fn dense_model_output_must_cover_each_doc_exactly_once_or_fall_back() {
        let docs: Vec<SkelDoc> = (0..20)
            .map(|i| skel(&format!("d{i}"), &format!("资料{i}.pdf"), "/运营"))
            .collect();
        let tree = build_skeleton("s", &docs);
        let (_, folders) = dense_prompt(&tree).expect("20 篇应进入模型分类预算");
        let good = r#"[{"folder":1,"groups":[{"label":"制度","docs":[1,2,3,4,5,6,7,8,9,10]},{"label":"流程","docs":[11,12,13,14,15,16,17,18,19,20]}]}]"#;
        let groups = parse_dense_groups(good, &folders).expect("合法全覆盖输出");
        let mut grouped = tree.clone();
        apply_dense_groups(&mut grouped, groups);
        assert!(grouped.children[0]
            .children
            .iter()
            .all(|node| node.doc_id.is_none()));
        let mut ids = Vec::new();
        doc_ids(&grouped, &mut ids);
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!((before, ids.len()), (20, 20));

        let duplicate = r#"[{"folder":1,"groups":[{"label":"甲","docs":[1,2,3,4,5,6,7,8,9,10]},{"label":"乙","docs":[10,11,12,13,14,15,16,17,18,19,20]}]}]"#;
        assert!(
            parse_dense_groups(duplicate, &folders).is_none(),
            "重复编号必须整份拒绝"
        );
        let missing = r#"[{"folder":1,"groups":[{"label":"甲","docs":[1,2,3]},{"label":"乙","docs":[4,5]}]}]"#;
        assert!(
            parse_dense_groups(missing, &folders).is_none(),
            "漏文档必须整份拒绝"
        );

        let mut fallback = tree;
        dense_fallback(&mut fallback);
        let mut fallback_ids = Vec::new();
        doc_ids(&fallback, &mut fallback_ids);
        assert_eq!(
            fallback_ids.len(),
            20,
            "坏模型输出的安全回退仍须保留全部文档"
        );
    }

    /// 节点序列化形状即契约：叶子只有 label+doc_id，分支只有 label+children
    #[test]
    fn node_json_shape_is_leaf_or_branch() {
        let tree = build_skeleton("s", &[skel("d1", "a.pdf", "/财务")]);
        let v = serde_json::to_value(&tree).unwrap();
        let branch = &v["children"][0];
        assert!(branch.get("doc_id").is_none(), "分支不得带 doc_id: {branch}");
        let leaf = &branch["children"][0];
        assert_eq!(leaf["doc_id"], "d1");
        assert!(leaf.get("children").is_none(), "叶子不得带 children: {leaf}");
    }

    /// LLM 标签解析：围栏/废话容忍，数量不符/空标签/非 JSON 一律 None（整体回退）
    #[test]
    fn label_parsing_is_all_or_nothing() {
        assert_eq!(
            parse_labels(r#"["费用管理","人事行政"]"#, 2).unwrap(),
            vec!["费用管理", "人事行政"]
        );
        // 围栏 + 前后缀
        assert_eq!(
            parse_labels("好的：\n```json\n[\"甲\",\"乙\"]\n```\n以上。", 2).unwrap(),
            vec!["甲", "乙"]
        );
        assert!(parse_labels(r#"["只有一个"]"#, 2).is_none(), "数量不符必须整体回退");
        assert!(parse_labels(r#"["甲","  "]"#, 2).is_none(), "空标签必须整体回退");
        assert!(parse_labels("不是 JSON", 1).is_none());
        assert!(parse_labels(r#"{"a":1}"#, 1).is_none());
        // 超长标签截断而不是拒收
        let long = "一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十";
        let got = parse_labels(&format!(r#"["{long}"]"#), 1).unwrap();
        assert_eq!(got[0].chars().count(), MAX_LABEL_CHARS);
    }

    /// LLM 失败回退：apply_labels(None) 不动骨架；Some 只改写分支 label、不动结构
    #[test]
    fn label_fallback_keeps_structural_skeleton() {
        let docs = vec![skel("d1", "报销制度.pdf", "/财务"), skel("d2", "招聘.pdf", "/人事")];
        let mut tree = build_skeleton("s", &docs);
        let before = tree.clone();
        apply_labels(&mut tree, None);
        assert_eq!(tree, before, "None 必须原样保留骨架");

        let labels = vec!["人事行政".to_string(), "费用管理".to_string()];
        apply_labels(&mut tree, Some(&labels));
        // 分支顺序是骨架的（"人事" < "财务" 字节序），标签按序贴
        assert_eq!(tree.children[0].label, "人事行政");
        assert_eq!(tree.children[1].label, "费用管理");
        assert_eq!(tree.children[0].children[0].doc_id.as_deref(), Some("d2"));
        assert_eq!(tree.children[1].children[0].label, "报销制度.pdf", "叶子不许被改名");
    }

    /// 提示词有界：分支数与每支文档名条数都被钉死（LLM 花费闸）
    #[test]
    fn label_prompt_is_bounded() {
        let docs: Vec<SkelDoc> = (0..30)
            .map(|i| skel(&format!("d{i}"), &format!("文档{i}.pdf"), "/财务"))
            .collect();
        let tree = build_skeleton("s", &docs);
        let prompt = label_prompt(&tree.children);
        assert!(prompt.matches("文档").count() == MAX_NAMES_PER_BRANCH, "{prompt}");
        assert!(prompt.contains("共 1 个分组"));
    }

    /// 缓存键形状是契约：版本前缀隔离旧扁平树，与 `admin_api` 的 kv 口配套
    #[test]
    fn cache_key_is_namespaced_by_space() {
        assert_eq!(cache_key("zhangsan"), "kb_mindmap:v3:zhangsan");
        assert_eq!(cache_key("enterprise-hr"), "kb_mindmap:v3:enterprise-hr");
        // 键带前缀：kv 表是全 runtime 开关共用的（llm_provider/mysql_target/digest_date），
        // 裸 space_id 会撞上同名登录名以外的键族。
        assert!(cache_key("llm_provider").ends_with(":llm_provider"));
    }

    #[test]
    fn source_fingerprint_tracks_only_mindmap_structure() {
        let docs = vec![
            skel("d2", "招聘.pdf", "/人事"),
            skel("d1", "报销.pdf", "/财务"),
        ];
        let same_reordered = vec![
            skel("d1", "报销.pdf", "/财务"),
            skel("d2", "招聘.pdf", "/人事"),
        ];
        let base = source_fingerprint("企业知识库", &docs);
        assert_eq!(base, source_fingerprint("企业知识库", &same_reordered));
        assert_ne!(
            base,
            source_fingerprint("新空间名", &docs),
            "空间改名必须失效"
        );
        assert_ne!(
            base,
            source_fingerprint("企业知识库", &[skel("d1", "报销.pdf", "/财务")]),
            "文档删除必须失效"
        );
        assert_ne!(
            base,
            source_fingerprint(
                "企业知识库",
                &[
                    skel("d2", "招聘.pdf", "/人事"),
                    skel("d1", "报销.pdf", "/财务/制度")
                ],
            ),
            "文档移动/目录改名必须失效"
        );
    }

    #[test]
    fn cache_envelope_requires_matching_current_fingerprint() {
        let body = serde_json::json!({ "space_id": "s", "root": { "label": "s" } });
        let text = cache_envelope("rev-1", &body).to_string();
        assert_eq!(cached_body(&text, "rev-1"), Some(body.clone()));
        assert!(cached_body(&text, "rev-2").is_none(), "旧指纹缓存必须失效");
        assert!(
            cached_body(&body.to_string(), "rev-1").is_none(),
            "v3 旧裸 body 要自动重建"
        );
        assert!(cached_body(r#"{"fingerprint":"rev-1","body":{}}"#, "rev-1").is_none());
    }

    /// markdown 重建：重叠尾巴按偏移去重；缺偏移的块全文保留；空块跳过
    #[test]
    fn markdown_reconstruction_dedups_overlap_by_span() {
        let chunks = vec![
            chunk(0, "甲乙丙丁。", Some((0, 5))),
            chunk(1, "丙丁。戊己", Some((2, 7))), // 与上一块重叠「丙丁。」
            chunk(2, "庚辛。", None),            // 缺偏移：全文保留
            chunk(3, "   ", Some((7, 7))),       // 空块跳过
        ];
        let md = markdown_from_chunks(&chunks);
        assert_eq!(md, "甲乙丙丁。\n\n戊己\n\n庚辛。");
        assert_eq!(markdown_from_chunks(&[]), "");
    }

    /// 端点顺序闸（源码级）：regenerate 必须先过写权限再生成（只读者不许烧 LLM）
    #[test]
    fn regenerate_checks_write_permission_before_generating() {
        let src = include_str!("kb_mindmap_api.rs");
        let body = src
            .split("pub async fn regenerate_mindmap")
            .nth(1)
            .unwrap()
            .split("pub async fn doc_markdown")
            .next()
            .unwrap();
        let auth = body.find("viewer(").unwrap();
        let writable = body.find("space_writable").unwrap();
        let generate = body.find("generate_mindmap").unwrap();
        assert!(auth < writable && writable < generate, "顺序必须是 认证→写权限→生成: {body}");
    }

    /// 预览两个端点都必须先过 `doc_for_viewer` 再取块（不可见即 403，不泄露存在性）
    #[test]
    fn preview_endpoints_gate_visibility_before_loading_chunks() {
        let src = include_str!("kb_mindmap_api.rs");
        for name in ["pub async fn doc_markdown", "pub async fn doc_chunks"] {
            let body = src.split(name).nth(1).unwrap();
            let body = body.split("\n}\n").next().unwrap();
            let gate = body.find("doc_for_viewer").unwrap();
            let load = body.find("load_chunks").unwrap();
            assert!(gate < load, "{name} 必须先过可见性再取块: {body}");
        }
    }

    /// space_id 闸：缺省个人空间、trim、全空白按缺省、超长 400（与 kb_eval_api 同口径）
    #[test]
    fn normalize_space_trims_defaults_and_caps() {
        let v = Viewer::new("zhangsan", vec![]);
        assert_eq!(normalize_space(&v, None).unwrap(), "zhangsan");
        assert_eq!(normalize_space(&v, Some("  ")).unwrap(), "zhangsan", "全空白按缺省");
        assert_eq!(normalize_space(&v, Some("  kb-hr  ")).unwrap(), "kb-hr", "文本值要 trim");
        let long = "a".repeat(65);
        assert_eq!(normalize_space(&v, Some(&long)).unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    /// 缓存读失败必须降级为重新生成（缓存只是加速器，不该 500）；
    /// 两个端点的缓存写都必须放后台（响应路径不白等一个 RTT）
    #[test]
    fn cache_read_degrades_and_write_is_off_response_path() {
        let src = include_str!("kb_mindmap_api.rs");
        let mindmap = src.split("pub async fn mindmap").nth(1).unwrap();
        let mindmap = mindmap.split("pub async fn regenerate_mindmap").next().unwrap();
        assert!(mindmap.contains("按未命中重新生成"), "缓存读失败必须降级为重新生成: {mindmap}");
        let regen = src.split("pub async fn regenerate_mindmap").nth(1).unwrap();
        let regen = regen.split("pub async fn doc_markdown").next().unwrap();
        for (name, body) in [("mindmap", mindmap), ("regenerate_mindmap", regen)] {
            assert!(body.contains("spawn_write_cache"), "{name} 必须后台写缓存: {body}");
        }
    }
}
