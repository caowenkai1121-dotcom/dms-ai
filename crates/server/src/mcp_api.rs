//! 【K6-A】对外 MCP 服务端：手写 JSON-RPC 2.0 over HTTP，五个工具 `ask` / `kb_search` / `datamap_search_nodes` / `datamap_find_paths` / `datamap_list_pending_edges`。
//! 变更原因＝对外集成协议（n8n / Dify / DataEase 调我们的问数、知识库与数据地图）。
//!
//! 三条纪律：
//! 1. **零旁路**：`ask` 走的就是 `/api/ask` 那条链（一次 PreparedQuestion → typed route → 对应执行器），
//!    `kb_search` 走的就是 `retrieve::search`；三个 `datamap_*` 走 `datamap_api` 抽出的共用取数层
//!    （与 REST 同一函数，不另抄 SQL）。本文件不含任何判据、不拼一行 SQL。
//! 2. **权限等同该员工登录**：key 只映射到 login_name，随后**必须**过
//!    `principal::load_principal` + 正常 scope 计算（`dms_agent::ask` 内部算）——没有「MCP 就是超管」
//!    这条路，多角色账号在这里同样被 fail-closed 拒（要选角色）。`kb_search` 的 `Viewer` 用映射到的
//!    login + 该 `Principal` 的角色码，**不放宽**：放宽一个字，MCP 就成了绕过 `kb.acl` 的入口。
//! 3. **默认关**：`mcp_keys` 为空时整个端点 404（对外面默认关比默认开重要）。key 不匹配 401，
//!    比较走 `auth::api_key_login` 常量时间版（不泄露前缀时序），且响应与日志都不回显 key
//!    （日志只记 `key_len`，一个前缀位都不给）。
//!
//! HTTP 状态码恒 200（JSON-RPC 的错误在 body 里），只有鉴权两种情况例外（404 / 401）——
//! 否则 MCP 客户端会把「工具执行失败」当成传输层故障重试。
//!
//! `ponytail:` 只受理请求，不受理通知（`notifications/*` 无 id → 一律 -32600）；真客户端要 202
//! 空响应时在 `parse_req` 的缺 id 分支加两行。`inputSchema` 手写 `json!`，不引 schemars（§7 已定）。

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Value};

use crate::AppState;
use dms_agent::intent::IntentRoute;
use dms_policy::{principal, Principal};
use dms_semantic::registry::datasource as ds_reg;

type ApiErr = (StatusCode, Json<Value>);
/// JSON-RPC 失败：(code, message)。**不是 HTTP 错误**——它进 200 响应的 `error` 字段。
type RpcFail = (i64, String);

/// 鉴权头。MCP 客户端（n8n/Dify）都支持自定义 header。
const API_KEY_HEADER: &str = "X-API-Key";
/// MCP 协议版本（n8n/Dify 现役客户端都认这一版）
const PROTOCOL_VERSION: &str = "2024-11-05";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const EXEC_FAILED: i64 = -32000;
/// 可重试的执行失败（超时 / 连接抖动 / 上游限流 / 熔断打开）。
///
/// 🔴 为什么要拆：MCP 对接方（Claude Desktop、n8n 之类）拿到 `-32000` 只能二选一 ——
/// 要么一律重试（把「员工不存在」这种永远不会变的失败重试三遍，白烧三次问答预算），
/// 要么一律不重试（一次网络抖动就把用户的问题判死）。**两种都错**，而错法都不报错。
/// 拆成两个码之后对接方能写「-32001 退避重试，-32000 直接报错」。
///
/// 形状不动：仍是 JSON-RPC 标准的 `error.code` / `error.message`，
/// 不加 `data` 字段、不加 `isError`（`error_response_shape` 与 `tool_result_shape` 两条断言钉着形状）。
/// 取 `-32001` 是因为 `-32000..-32099` 是 JSON-RPC 留给实现自定义的区间。
const EXEC_RETRYABLE: i64 = -32001;

/// 执行失败的错误文本 → 该给哪个码（**纯函数**，故有正反对照单测）。
///
/// 判据只认**瞬时**特征。判不出来一律落 `-32000`（不可重试）—— 偏向哪一侧是有讲究的：
/// 把不可重试的错判成可重试，对接方会拿同一个必然失败的问句反复打库；
/// 反过来只是少了一次自动重试，用户重问一次就是了。宁少不多。
fn fail_code(msg: &str) -> i64 {
    // 中文串按子串匹配（本仓错误文案是中文）；英文串先小写化再匹配（sqlx/reqwest 的原文）。
    // 连接类只收瞬时形态：「连接失败」对应 connector::Connect（建池/握手/被拒，瞬时倾向），
    // 裸「连接」/「connect」会误吞「连接配置缺失」「connection string invalid」这类永久错误
    //（与上方「宁少不多」的偏向一致）。
    const RETRYABLE_CN: &[&str] = &[
        "超时",
        "连接失败",
        "连接重置",
        "连接中断",
        "熔断",
        "限流",
        "请稍后",
        "暂时不可用",
    ];
    const RETRYABLE_EN: &[&str] = &[
        "timeout",
        "timed out",
        "connection refused",
        "connection reset",
        "connection closed",
        "connection terminated",
        "broken pipe",
        "429",
        "502",
        "503",
        "504",
        "too many requests",
        "temporarily",
    ];
    // 先判 CN：中文文案命中时，全文小写化那次分配是白付
    if RETRYABLE_CN.iter().any(|w| msg.contains(w)) {
        return EXEC_RETRYABLE;
    }
    let low = msg.to_ascii_lowercase();
    if RETRYABLE_EN.iter().any(|w| low.contains(w)) {
        EXEC_RETRYABLE
    } else {
        EXEC_FAILED
    }
}

/// 两个工具描述里都写死这句：对接方必须知道自己拿到的是**谁的**数据权限。
const PERM_NOTE: &str = "⚠️ 本工具的数据权限等同于所映射员工的登录权限（行级权限、数据源可见性、\
知识库 ACL 全部按该员工计算），不是管理员权限。";

// ---------------------------------------------------------------- 协议纯函数

#[derive(Debug)]
struct RpcReq {
    id: Value,
    method: String,
    params: Value,
}

/// 请求 id：只认字符串与**整数**（JSON-RPC 明确 SHOULD NOT 带小数部分）。
/// 缺失 / null / 小数 / 布尔等非法类型 → `Null`
/// （JSON-RPC 要求无法判定 id 的错误响应带 `id: null`）。
fn req_id(v: &Value) -> Value {
    match v.get("id") {
        Some(i) if i.is_string() || i.is_i64() || i.is_u64() => i.clone(),
        _ => Value::Null,
    }
}

/// 解析 JSON-RPC 请求。`jsonrpc` 版本字段刻意不校验（客户端漏填不影响语义，
/// 为它拒一个正确请求是纯粹的自伤）。
fn parse_req(v: &Value) -> Result<RpcReq, RpcFail> {
    let id = req_id(v);
    if id.is_null() {
        return Err((
            INVALID_REQUEST,
            "缺 id 或 id 类型非法（仅收字符串/整数；本端点不受理通知式请求）".into(),
        ));
    }
    let method = v
        .get("method")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| (INVALID_REQUEST, "缺 method".to_string()))?;
    Ok(RpcReq {
        id,
        method: method.to_string(),
        params: v.get("params").cloned().unwrap_or_else(|| json!({})),
    })
}

fn ok_resp(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err_resp(id: &Value, (code, message): RpcFail) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn server_info() -> Value {
    // 内容全静态：缓存一份克隆出去（每次请求重建整棵 JSON 是白付）
    static V: OnceLock<Value> = OnceLock::new();
    V.get_or_init(|| {
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "dms-ai", "version": env!("CARGO_PKG_VERSION") },
        })
    })
    .clone()
}

fn tools() -> Value {
    // 同 server_info：全静态清单缓存一份、克隆出去（每次请求重建整棵 JSON 是白付）
    static V: OnceLock<Value> = OnceLock::new();
    V.get_or_init(|| {
        json!({ "tools": [
        {
            "name": "ask",
            "description": format!(
                "自然语言取数与企业知识问答：自动分诊到 NL2SQL（返回表格数据）或知识库（返回带引用的文本）。{PERM_NOTE}"
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "自然语言问题，例如「上月销售额」「差旅报销上限」" },
                    "ds": { "type": "string", "description": "可选：显式指定数据源 ds_id；不传则由后端选源。对该员工不可见的源会被拒绝。" },
                },
                "required": ["question"],
            },
        },
        {
            "name": "kb_search",
            "description": format!(
                "知识库检索：返回命中的文档块与引用信息（chunk_id / doc_id / doc_name / ord / page / heading_path / score / text），不调用大模型。{PERM_NOTE}"
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "检索词或问句" },
                    "space_id": { "type": "string", "description": "可选：限定知识空间；不传＝该员工全部可见文档" },
                },
                "required": ["query"],
            },
        },
        {
            "name": "datamap_search_nodes",
            "description": format!(
                "数据地图节点检索：按关键字搜表/列节点（大小写不敏感包含匹配表名/列名/注释），返回目录元数据（注释、域、估算行数、类型），不含任何行数据。{PERM_NOTE}"
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "keyword": { "type": "string", "description": "检索关键字，例如「门店」「订单」「storecode」" },
                    "ds": { "type": "string", "description": "可选：数据源 ds_id，缺省 dms。对该员工不可见的源会被拒绝。" },
                    "limit": { "type": "integer", "description": "可选：返回上限，缺省 50，范围 1–200" },
                },
                "required": ["keyword"],
            },
        },
        {
            "name": "datamap_find_paths",
            "description": format!(
                "数据地图路径查询：两表间两级内的最短 JOIN 路径（只走已验收的合同边，待审推断边不在其中）；found=false 表示两级内不连通，是正常答案不是错误。{PERM_NOTE}"
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "起点表名（可带库名前缀）" },
                    "to": { "type": "string", "description": "终点表名（可带库名前缀）" },
                    "ds": { "type": "string", "description": "可选：数据源 ds_id，缺省 dms" },
                },
                "required": ["from", "to"],
            },
        },
        {
            "name": "datamap_list_pending_edges",
            "description": format!(
                "数据地图待审推断边：按 confidence 降序列出待人工复核的推断边（joinable/synonym/distribution_similar/co_occurs/correlated 等），含置信度与证据 JSON。{PERM_NOTE}"
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ds": { "type": "string", "description": "可选：数据源 ds_id，缺省 dms" },
                    "kind": { "type": "string", "description": "可选：边类型过滤，闭集 join | lineage | joinable | synonym | distribution_similar | co_occurs | correlated" },
                    "limit": { "type": "integer", "description": "可选：返回上限，缺省 50，范围 1–500" },
                },
                "required": [],
            },
        },
    ] })
    })
    .clone()
}

/// `tools/call` 的 `{name, arguments}`。`arguments` 缺省按空对象（无参工具的合法形态）。
fn call_args(params: &Value) -> Result<(String, Value), RpcFail> {
    let name = req_str(params, "name")?;
    Ok((
        name,
        params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({})),
    ))
}

/// 必填字符串参数：缺失 / 非字符串 / 空白串一律 -32602。
fn req_str(args: &Value, key: &str) -> Result<String, RpcFail> {
    match args.get(key).and_then(Value::as_str).map(str::trim) {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => Err((INVALID_PARAMS, format!("缺参数 {key}（非空字符串）"))),
    }
}

/// 可选字符串参数：空白串按未传（表单与低代码平台常传空串）。
fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 用户输入回显进错误文案的上限（未知 method/工具名可被塞超长串撑大响应）
fn clipped(s: &str) -> String {
    s.chars().take(64).collect()
}

/// 工具返回体。**不产 `isError`**：执行失败一律走 JSON-RPC 的 -32000（零生产者的字段不建）。
fn text_content(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

// ---------------------------------------------------------------- 鉴权

/// key → login_name。两种 HTTP 例外都在这里：未配置任何 key = 功能关闭（404）、key 不匹配（401）。
fn authorize(keys: &HashMap<String, String>, headers: &HeaderMap) -> Result<String, ApiErr> {
    let deny = |code: StatusCode, msg: &str| (code, Json(json!({ "error": msg })));
    if keys.is_empty() {
        // 默认关：没配 key 时连「端点存在」都不该暴露
        return Err(deny(StatusCode::NOT_FOUND, "未找到"));
    }
    let raw = headers
        .get(API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    // 常量时间比较（复用 `auth::api_key_login`）：`HashMap::get` 的哈希早退会泄露时序，
    // 攻击者可逐位探测 key 前缀 —— 与 REST 双通道同一条比较链，不各抄一份。
    match crate::auth::api_key_login(keys, raw) {
        Some(login) => Ok(login.to_string()),
        None => {
            // 日志只记长度与客户端地址，不回显任何前缀（前 4 位也是 key 的一部分）
            tracing::warn!(key_len = raw.len(), ip = %crate::auth::client_ip(headers), "MCP 鉴权失败：X-API-Key 不匹配");
            Err(deny(StatusCode::UNAUTHORIZED, "X-API-Key 无效"))
        }
    }
}

// ---------------------------------------------------------------- 入口

/// `POST /api/mcp`。body 手工解析（不用 `Json` 提取器）：非法 JSON 也必须回 200 + -32700，
/// 走提取器会变成 axum 的 400，MCP 客户端会当传输故障。
pub async fn mcp(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Result<Json<Value>, ApiErr> {
    let login = authorize(&st.mcp_keys, &headers)?;
    let v: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return Ok(Json(err_resp(
                &Value::Null,
                (PARSE_ERROR, format!("JSON 解析失败：{e}")),
            )))
        }
    };
    let id = req_id(&v);
    let req = match parse_req(&v) {
        Ok(r) => r,
        Err(f) => return Ok(Json(err_resp(&id, f))),
    };
    let out = match req.method.as_str() {
        "initialize" => Ok(server_info()),
        "ping" => Ok(json!({})), // MCP 规范的保活探测：空 result（保活型客户端不该拿 -32601）
        "tools/list" => Ok(tools()),
        "tools/call" => call(&st, &login, &req.params).await,
        other => Err((METHOD_NOT_FOUND, format!("未知方法 {}", clipped(other)))),
    };
    Ok(Json(match out {
        Ok(result) => ok_resp(&req.id, result),
        Err(f) => err_resp(&req.id, f),
    }))
}

/// 工具闭集（与 tools() 清单同源 —— dispatch_covers_every_listed_tool 钉着一致性）。
/// 先验名再加载身份：乱填工具名的请求不许白打一次身份库。
const TOOL_NAMES: &[&str] = &[
    "ask",
    "kb_search",
    "datamap_search_nodes",
    "datamap_find_paths",
    "datamap_list_pending_edges",
];

/// 工具分派。**身份换算在这里发生且只发生一次**：login_name → `Principal`
/// （员工不存在 / 无角色 / 多角色未选 一律失败，与该员工登录同一套判据）。
async fn call(st: &AppState, login: &str, params: &Value) -> Result<Value, RpcFail> {
    let (name, args) = call_args(params)?;
    if !TOOL_NAMES.contains(&name.as_str()) {
        // 工具名不认识与方法名不认识同性质，共用 -32601
        return Err((METHOD_NOT_FOUND, format!("未知工具 {}", clipped(&name))));
    }
    let p = principal::load_principal(&st.auth_mysql, login, None)
        .await
        .map_err(|e| {
            // 「员工不存在 / 无角色」是永久失败，但同一个 Err 也可能是身份库连接抖动 ——
            // 所以交给 `fail_code` 按文本判，别在这里写死成不可重试。
            let m = format!("身份加载失败：{e}");
            (fail_code(&m), m)
        })?;
    let text = match name.as_str() {
        "ask" => tool_ask(st, &p, &args).await?,
        "kb_search" => tool_kb_search(st, &p, &args).await?,
        "datamap_search_nodes" => tool_datamap_search_nodes(st, &p, &args).await?,
        "datamap_find_paths" => tool_datamap_find_paths(st, &p, &args).await?,
        "datamap_list_pending_edges" => tool_datamap_list_pending_edges(st, &p, &args).await?,
        // 上方已按 TOOL_NAMES 闭集校验，此臂理论不可达（双保险，不 panic）
        other => return Err((METHOD_NOT_FOUND, format!("未知工具 {}", clipped(other)))),
    };
    Ok(text_content(text))
}

/// 与 `/api/ask` 完全同一条链路：统一准备一次 → typed route → 对应执行器。
/// 无会话状态，故 `prev_question=None`、forced intent=None（MCP 侧不给 chip）。
async fn tool_ask(st: &AppState, p: &Principal, args: &Value) -> Result<String, RpcFail> {
    let question = req_str(args, "question")?;
    let ds = opt_str(args, "ds");
    let prepared = crate::prepare_ask(st, &question, None).await;
    let out: Value = match prepared.question.route() {
        // 合同不可用 ≠ 知识库不能答（同 `/api/ask`，见 `unknown_route_kb_fallback` 的红字）。
        // 只做检索、不生成任何 SQL；查到带引用的内容才顶替卡片。
        IntentRoute::Unknown => {
            match crate::unknown_route_kb_fallback(
                st,
                p,
                None,
                &prepared.question.effective_question,
            )
            .await
            {
                Some(a) => serde_json::to_value(&a)
                    .map_err(|e| internal_fail("知识库结果序列化", &e))?,
                None => serde_json::to_value(prepared.question.clarification_result())
                    .map_err(|e| internal_fail("澄清结果序列化", &e))?,
            }
        }
        IntentRoute::Data => {
            let (r, _log) = crate::ask_prepared(
                &st.llm,
                &st.auth_mysql,
                &st.mysql,
                &st.sources,
                st.owned.pool(),
                &st.embed,
                p,
                &prepared,
                ds.as_deref(),
                None, // conv_id：MCP 无会话概念（`chat.msg.conv_id` 只存在于 HTTP 聊天）
                st.sc_samples,
                None, // space_id：MCP 无空间选择面
                true,
            )
            .await;
            // 长驻进程，写入句柄直接丢弃（fire-and-forget，同 `/api/ask`）
            let r = r.map_err(|e| internal_fail("问数执行", &e))?;
            serde_json::to_value(&r).map_err(|e| internal_fail("问数结果序列化", &e))?
        }
        IntentRoute::Knowledge => {
            let a = crate::kb_answer(st, p, None, &prepared.question.effective_question)
                .await
                .map_err(|e| internal_fail("知识问答", &e))?;
            let mut payload =
                serde_json::to_value(&a).map_err(|e| internal_fail("知识结果序列化", &e))?;
            payload["intent_summary"] = crate::knowledge_summary_value(&prepared, &a);
            if prepared.question.effective_question != question {
                payload["resolved_question"] = json!(prepared.question.effective_question);
            }
            payload
        }
        IntentRoute::Hybrid => {
            let h = crate::HybridAsk {
                question: &question,
                p,
                ds: ds.as_deref(),
                conv_id: None,
                space_id: None,
                sc_samples: st.sc_samples,
            };
            crate::hybrid_payload(st, &h, &prepared)
                .await
                .map_err(|(_, body)| {
                    let msg = body
                        .0
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("混合问答执行失败")
                        .to_string();
                    (EXEC_FAILED, msg)
                })?
        }
    };
    Ok(json_text(&out))
}

/// 检索命中直出（不调 LLM）：对接方要的是块正文 + 引用信息，好自己拼提示词。
async fn tool_kb_search(st: &AppState, p: &Principal, args: &Value) -> Result<String, RpcFail> {
    let query = req_str(args, "query")?;
    let space = opt_str(args, "space_id");
    let hits = dms_knowledge::retrieve::search(
        &st.owned,
        &st.embed,
        &viewer(p),
        space.as_deref(),
        &query,
        &st.cfg().kb_rrf_weights,
    )
    .await
    .map_err(|e| internal_fail("知识库检索", &e))?;
    let hits: Vec<Value> = hits
        .into_iter()
        .map(|h| {
            json!({ "chunk_id": h.chunk_id, "doc_id": h.doc_id, "doc_name": h.doc_name,
                "ord": h.ord, "page": h.page, "heading_path": h.heading_path,
                "score": h.score, "text": h.text })
        })
        .collect();
    Ok(json_text(&json!({ "hits": hits })))
}

// ---------------------------------------------------------------- 数据地图三工具
// 取数全部走 `datamap_api` 抽出的共用层（与 REST 同一函数），本文件只补 MCP 侧的
// 入参解析与错误形状。ds 可见性判据也是同一个 `datamap_api::ds_visible` —— 权限语义
// 与 `/api/datamap/*` 逐字一致（REST 映 403 的那句，这里映 -32000：权限拒绝重试无意义）。

/// 三个 datamap 工具共用的前段：ds 缺省 `DMS_DS_ID`，白名单校验 → 可见性判定。
async fn require_ds(st: &AppState, p: &Principal, args: &Value) -> Result<String, RpcFail> {
    let ds = opt_str(args, "ds").unwrap_or_else(|| ds_reg::DMS_DS_ID.to_string());
    if !crate::datamap_api::valid_ds(&ds) {
        return Err((
            INVALID_PARAMS,
            format!("ds 非法：{ds}（字母数字与 _-，≤64 字符）"),
        ));
    }
    let visible = crate::datamap_api::ds_visible(st, p, &ds)
        .await
        .map_err(|e| {
            let m = format!("ds 可见性判定失败：{e}");
            (fail_code(&m), m)
        })?;
    if !visible {
        // 与 REST 侧同一文案口径：「不存在」与「无权」不分写（分开 = 证实存在性，枚举 oracle）
        return Err((EXEC_FAILED, format!("数据源不存在或无权访问：{ds}")));
    }
    Ok(ds)
}

/// 可选整数限量参数（纯函数）：缺失/非数字 → default，再 clamp 进 [lo, hi]。
fn opt_limit(args: &Value, key: &str, default: i64, lo: i64, hi: i64) -> i64 {
    args.get(key)
        .and_then(Value::as_i64)
        .unwrap_or(default)
        .clamp(lo, hi)
}

/// 节点关键字匹配（纯函数）：表名/列名/注释/域任一包含即命中（大小写不敏感）。
/// 只碰元数据字段 —— 节点 JSON 里本来就没有行数据（`datamap_api::load_nodes` 的组装保证）。
fn node_matches(node: &Value, kw: &str) -> bool {
    // 大小写归一化收在这里：测试钉的「函数本身大小写不敏感」，调用方不必记得先 lower
    let kw = kw.to_lowercase();
    ["table", "column", "comment", "domain"].iter().any(|k| {
        node.get(k)
            .and_then(Value::as_str)
            .is_some_and(|v| v.to_lowercase().contains(&kw))
    })
}

/// ① 节点检索：共用层取全量节点，关键字过滤 + 限量在 Rust 侧（目录体量小，
/// 比多养一份 LIKE SQL 值得 —— 纪律 1「不拼一行 SQL」）。
async fn tool_datamap_search_nodes(
    st: &AppState,
    p: &Principal,
    args: &Value,
) -> Result<String, RpcFail> {
    let keyword = req_str(args, "keyword")?;
    let limit = opt_limit(args, "limit", 50, 1, 200) as usize;
    let ds = require_ds(st, p, args).await?;
    let nodes = crate::datamap_api::load_nodes(st, &ds).await.map_err(|e| {
        let m = format!("数据地图节点加载失败：{e}");
        (fail_code(&m), m)
    })?;
    // 大小写归一化收在 node_matches 内部，调用方不再先 lower 一遍
    let hits: Vec<&Value> = nodes
        .iter()
        .filter(|n| node_matches(n, &keyword))
        .take(limit)
        .collect();
    Ok(json_text(&json!({
        "ds": ds, "keyword": keyword, "count": hits.len(), "nodes": hits,
    })))
}

/// ② 两级路径：合同边加载口、边数护栏、BFS 与组装全部与 REST `/api/datamap/paths` 同款。
async fn tool_datamap_find_paths(
    st: &AppState,
    p: &Principal,
    args: &Value,
) -> Result<String, RpcFail> {
    let from = req_str(args, "from")?;
    let to = req_str(args, "to")?;
    let ds = require_ds(st, p, args).await?;
    // 边取组合器同一加载口（路径面 = 可通行面，pending/rejected 推断边天然不在其中）
    let edges = dms_semantic::registry::model::load_join_edges(st.owned.pool(), &ds)
        .await
        .map_err(|e| {
            let m = format!("合同边加载失败：{e}");
            (fail_code(&m), m)
        })?;
    if !crate::datamap_api::within_edge_budget(edges.len()) {
        // 护栏与 REST 同款：超预算直接拒，不静默截断（文案与 422 那句逐字一致）
        return Err((
            EXEC_FAILED,
            format!(
                "关联边 {} 条超过护栏 {}，路径查询被拒（先收窄目录再查）",
                edges.len(),
                crate::datamap_api::PATH_MAX_EDGES
            ),
        ));
    }
    Ok(json_text(&crate::datamap_api::paths_result_json(
        &ds, &from, &to, &edges,
    )))
}

/// ③ 待审推断边：status 恒 pending、confidence 降序（SQL 内 ORDER BY + LIMIT 500），
/// 调用侧限量是「从最强候选里再截一段」，语义与 REST 复核队列一致。
async fn tool_datamap_list_pending_edges(
    st: &AppState,
    p: &Principal,
    args: &Value,
) -> Result<String, RpcFail> {
    let limit = opt_limit(args, "limit", 50, 1, 500) as usize;
    let ds = require_ds(st, p, args).await?;
    // kind 过滤与 REST ② 同一个闭集函数：非法取值 REST 映 400，这里映 -32602
    let kinds = match opt_str(args, "kind") {
        Some(k) => match crate::datamap_api::edge_kind_filter(&k) {
            Some((_, kinds)) => kinds,
            None => {
                return Err((
                    INVALID_PARAMS,
                    format!("kind 只能是 join | lineage | joinable | synonym | distribution_similar | co_occurs | correlated：{k}"),
                ))
            }
        },
        // 空串 = 「全部 kind」的隐式契约（edge_kind_filter 的 "" 臂，七值全收）
        None => crate::datamap_api::edge_kind_filter("").map(|(_, k)| k).unwrap_or_default(),
    };
    let mut edges = crate::datamap_api::load_inferred_edges(st, &ds, &["pending"], kinds)
        .await
        .map_err(|e| {
            let m = format!("推断边加载失败：{e}");
            (fail_code(&m), m)
        })?;
    edges.truncate(limit);
    Ok(json_text(
        &json!({ "ds": ds, "count": edges.len(), "edges": edges }),
    ))
}

/// 知识库身份：映射到的 login + 该 `Principal` 的**角色码**。多一个字都不许放宽。
fn viewer(p: &Principal) -> dms_knowledge::Viewer {
    let roles = if p.role_code.is_empty() {
        vec![]
    } else {
        vec![p.role_code.clone()]
    };
    dms_knowledge::Viewer::new(&p.login_name, roles)
}

/// 内部错误对外固定文案：anyhow 原文可含库名/SQL 片段，外泄 = 内部结构白送 —— 原文只进
/// 服务端日志（对齐 artifact_api::db_err 的口径）；但可重试码仍按原文分类（-32001/-32000 语义不动）。
fn internal_fail(what: &str, e: impl std::fmt::Display) -> RpcFail {
    let m = e.to_string();
    tracing::warn!(err = %m, "MCP {what}失败");
    (fail_code(&m), format!("{what}失败，请稍后重试"))
}

/// content 只有文本一种载体，故结构化结果整体序列化成 JSON 文本（对端一个 JSON.parse 就能用）。
fn json_text(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| {
        // json! 产物序列化本不可失败；真失败也必须留信号，不能静默兜底 "{}"
        tracing::error!(err = %e, "MCP 结果 JSON 序列化失败（理论不可达）");
        "{}".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(k: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(API_KEY_HEADER, k.parse().unwrap());
        h
    }

    fn keys() -> HashMap<String, String> {
        HashMap::from([("k-abcdef123456".to_string(), "zhangsan".to_string())])
    }

    #[test]
    fn parse_valid_request() {
        let r = parse_req(&json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": "ask", "arguments": { "question": "上月销售额" } }
        }))
        .unwrap();
        assert_eq!(r.id, json!(7));
        assert_eq!(r.method, "tools/call");
        let (name, args) = call_args(&r.params).unwrap();
        assert_eq!(name, "ask");
        assert_eq!(req_str(&args, "question").unwrap(), "上月销售额");
        // 缺省 params 也要能解析（initialize 常不带 params）
        let r2 = parse_req(&json!({ "id": "x", "method": "initialize" })).unwrap();
        assert_eq!(r2.params, json!({}));
    }

    #[test]
    fn parse_missing_method_is_invalid_request() {
        let e = parse_req(&json!({ "jsonrpc": "2.0", "id": 1 })).unwrap_err();
        assert_eq!(e.0, INVALID_REQUEST);
        assert!(e.1.contains("method"), "{}", e.1);
    }

    /// id 类型契约：字符串/整数收；小数（JSON-RPC SHOULD NOT）、布尔、null、缺失一律拒
    #[test]
    fn req_id_accepts_only_strings_and_integers() {
        assert_eq!(req_id(&json!({"id": "abc"})), json!("abc"));
        assert_eq!(req_id(&json!({"id": 7})), json!(7));
        assert_eq!(req_id(&json!({"id": -3})), json!(-3), "负整数是合法 id");
        for bad in [
            json!({"id": 1.5}),
            json!({"id": true}),
            json!({"id": null}),
            json!({}),
        ] {
            assert_eq!(req_id(&bad), Value::Null, "{bad}");
            let e = parse_req(
                &json!({"id": bad.get("id").cloned().unwrap_or(Value::Null), "method": "m"}),
            )
            .unwrap_err();
            assert_eq!(e.0, INVALID_REQUEST, "{bad}");
            assert!(
                e.1.contains("id 类型非法"),
                "文案要分清缺失与类型非法：{}",
                e.1
            );
        }
    }

    /// 缺 id / id=null 都不受理（通知式请求）；且此时错误响应必须带 `id: null`
    #[test]
    fn parse_missing_id_is_invalid_request() {
        for v in [
            json!({ "method": "tools/list" }),
            json!({ "id": null, "method": "tools/list" }),
        ] {
            let e = parse_req(&v).unwrap_err();
            assert_eq!(e.0, INVALID_REQUEST);
            assert_eq!(req_id(&v), Value::Null);
        }
    }

    /// 错误响应形状是外部契约：jsonrpc/id/error{code,message}，且**不许同时带 result**
    #[test]
    fn error_response_shape() {
        let r = err_resp(&json!(3), (METHOD_NOT_FOUND, "未知方法 foo".into()));
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["id"], json!(3));
        assert_eq!(r["error"]["code"], -32601);
        assert_eq!(r["error"]["message"], "未知方法 foo");
        assert!(r.get("result").is_none());
        let ok = ok_resp(&json!("a"), json!({ "x": 1 }));
        assert!(ok.get("error").is_none());
        assert_eq!(ok["result"]["x"], 1);
        // 拆出 -32001 之后**形状不许变**：仍只有 code + message 两个键，没有 data、没有 isError。
        // 对接方按形状解析，多一个键就可能让严格解析器整条报错。
        let rt = err_resp(&json!(4), (EXEC_RETRYABLE, "上游超时".into()));
        assert_eq!(rt["error"].as_object().unwrap().len(), 2, "{rt}");
        assert_eq!(rt["error"]["code"], -32001);
    }

    /// 🔴 可重试 / 不可重试必须**真的分开**。合成一个码时对接方只能二选一：
    /// 一律重试（把「员工不存在」重试三遍，白烧三次问答预算）或一律不重试
    /// （一次网络抖动就把问题判死）。两种都错，且都不报错。
    ///
    /// 正反对照两侧都给足：只断言「超时→可重试」的话，把 `fail_code` 写成恒返
    /// `EXEC_RETRYABLE` 也全绿 —— 而那正是最坏的一档（永久失败被无限重试）。
    #[test]
    fn transient_failures_get_a_different_code_than_permanent_ones() {
        assert_ne!(EXEC_FAILED, EXEC_RETRYABLE, "两个码撞了 = 等于没拆");
        // 瞬时：本仓中文文案 + 上游英文原文（大小写混写照样要认）
        for m in [
            "查询超时（8s）",
            "身份加载失败：pool timed out while waiting for an open connection",
            "connection refused",
            "embed 熔断打开，本轮跳过",
            "HTTP 429 Too Many Requests",
            "upstream returned 503",
            "服务暂时不可用，请稍后再试",
        ] {
            assert_eq!(fail_code(m), EXEC_RETRYABLE, "该判可重试：{m}");
        }
        // 瞬时（连接类收窄后的瞬时形态仍要认）
        assert_eq!(
            fail_code("连接失败 [owned-pg] connection refused"),
            EXEC_RETRYABLE
        );
        assert_eq!(fail_code("连接重置，请重试"), EXEC_RETRYABLE);
        // 永久：重试一万次也还是这个结果，重试就是纯浪费
        for m in [
            "连接配置缺失",
            "connection string invalid",
            "身份加载失败：员工不存在",
            "身份加载失败：该账号有多个角色，请指定 role_code",
            "生成失败（自修后仍不可用）",
            "SQL 未通过只读闸门：检测到 DELETE",
            "Unknown column 'brand_nam' in 'field list'",
            "知识库里没有关于这个问题的内容",
            "",
        ] {
            assert_eq!(fail_code(m), EXEC_FAILED, "该判不可重试：{m}");
        }
    }

    /// `tools/list` 是对接方唯一的契约来源：工具名、必填字段、权限声明一个都不许漂
    #[test]
    fn tools_list_names_and_schema() {
        let t = tools();
        let arr = t["tools"].as_array().unwrap();
        let names: Vec<&str> = arr.iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            vec![
                "ask",
                "kb_search",
                "datamap_search_nodes",
                "datamap_find_paths",
                "datamap_list_pending_edges"
            ]
        );
        assert_eq!(arr[0]["inputSchema"]["required"], json!(["question"]));
        assert_eq!(arr[1]["inputSchema"]["required"], json!(["query"]));
        assert!(arr[0]["inputSchema"]["properties"]["ds"].is_object());
        assert!(arr[1]["inputSchema"]["properties"]["space_id"].is_object());
        // 数据地图三工具的契约钉点
        assert_eq!(arr[2]["inputSchema"]["required"], json!(["keyword"]));
        assert_eq!(arr[3]["inputSchema"]["required"], json!(["from", "to"]));
        assert_eq!(
            arr[4]["inputSchema"]["required"],
            json!([]),
            "待审边全参数可选"
        );
        for i in 2..=4 {
            assert!(
                arr[i]["inputSchema"]["properties"]["ds"].is_object(),
                "工具 {i} 缺 ds 参数"
            );
        }
        assert!(arr[2]["inputSchema"]["properties"]["limit"].is_object());
        assert!(arr[4]["inputSchema"]["properties"]["kind"].is_object());
        assert!(arr[4]["inputSchema"]["properties"]["limit"].is_object());
        for tool in arr {
            let d = tool["description"].as_str().unwrap();
            assert!(d.contains("数据权限等同于所映射员工的登录权限"), "{d}");
        }
    }

    /// 🔴 派发表与工具清单不许漂：`call()` 的 match 臂必须覆盖 `tools()` 里的每一个名字
    /// （清单有、派发没有 = 客户端 tools/list 看到一个一调就 -32601 的工具）。
    #[test]
    fn dispatch_covers_every_listed_tool() {
        let src = include_str!("mcp_api.rs");
        for tool in tools()["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            assert!(
                src.contains(&format!("\"{name}\" =>")),
                "call() 缺 {name} 的派发臂"
            );
        }
        assert!(src.contains("\"ping\" =>"), "MCP 规范的 ping 保活臂不许丢");
    }

    #[test]
    fn call_args_and_required_params() {
        // 缺 name → -32602
        assert_eq!(call_args(&json!({})).unwrap_err().0, INVALID_PARAMS);
        // arguments 缺省 = 空对象，但必填参数照样必须报 -32602
        let (_, args) = call_args(&json!({ "name": "ask" })).unwrap();
        assert_eq!(req_str(&args, "question").unwrap_err().0, INVALID_PARAMS);
        // 空白串不算填了
        assert_eq!(
            req_str(&json!({ "question": "  " }), "question")
                .unwrap_err()
                .0,
            INVALID_PARAMS
        );
        assert_eq!(opt_str(&json!({ "ds": "" }), "ds"), None);
        assert_eq!(
            opt_str(&json!({ "ds": " up_d1 " }), "ds").as_deref(),
            Some("up_d1")
        );
    }

    /// 脱敏已收敛为「日志只记 key_len」：`mask_key` 已删（前 4 位也是 key 的一部分）。
    /// 这条守的是「别把脱敏前缀加回来」：失败日志一个前缀位都不给。
    /// （判据只扫非测试代码：断言文本里自己也写着这个函数名，全文件扫会数到自己）
    #[test]
    fn key_masking_helper_is_gone() {
        let src = include_str!("mcp_api.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or("");
        assert!(
            !code.contains("fn mask_key"),
            "脱敏函数加回来 = 前缀泄露面加回来"
        );
    }

    /// 鉴权的三条分支（本文件唯一的 HTTP 状态码来源）
    #[test]
    fn authorize_closed_by_default_and_never_echoes_key() {
        // 未配置任何 key → 404（功能默认关，连端点存在都不暴露）
        let e = authorize(&HashMap::new(), &hdr("k-abcdef123456")).unwrap_err();
        assert_eq!(e.0, StatusCode::NOT_FOUND);
        // 不匹配 → 401，且响应体不含 key 的任何完整形态
        let (code, Json(body)) = authorize(&keys(), &hdr("k-wrongwrongwrong")).unwrap_err();
        assert_eq!(code, StatusCode::UNAUTHORIZED);
        let body = body.to_string();
        assert!(
            !body.contains("k-wrongwrongwrong") && !body.contains("k-abcdef123456"),
            "{body}"
        );
        // 缺头 → 401（不是 500）
        assert_eq!(
            authorize(&keys(), &HeaderMap::new()).unwrap_err().0,
            StatusCode::UNAUTHORIZED
        );
        // 匹配 → login_name
        assert_eq!(
            authorize(&keys(), &hdr("k-abcdef123456")).unwrap(),
            "zhangsan"
        );
        // 差一位 / 前缀相同都不许命中（常量时间比较的行为由 auth 侧单测钉，这里钉接线没丢）
        assert_eq!(
            authorize(&keys(), &hdr("k-abcdef123457")).unwrap_err().0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            authorize(&keys(), &hdr("k-abcdef12345")).unwrap_err().0,
            StatusCode::UNAUTHORIZED
        );
    }

    /// 🔴 比较链必须复用 `auth::api_key_login`（常量时间）：`HashMap::get` 的哈希早退
    /// 泄露时序，攻击者可逐位探测前缀。失败日志只记 key_len，连脱敏前缀都不回显。
    ///（先归一 CRLF 再切函数体：本文件是混合行尾，直接切 "\n}\n" 会把测试体也切进来）
    #[test]
    fn authorize_uses_constant_time_lookup_and_logs_only_length() {
        let src = include_str!("mcp_api.rs").replace("\r\n", "\n");
        let body = src
            .split("fn authorize(")
            .nth(1)
            .expect("authorize 没了")
            .split("\n}\n")
            .next()
            .unwrap();
        assert!(
            body.contains("crate::auth::api_key_login"),
            "key 比较必须走常量时间版：{body}"
        );
        assert!(
            !body.contains("keys.get("),
            "API key 查找不许用 HashMap::get（哈希早退泄露时序）：{body}"
        );
        assert!(
            !body.contains("mask_key"),
            "失败日志不回显 key 前缀：{body}"
        );
        assert!(body.contains("key_len"), "失败日志只记长度：{body}");
    }

    /// `initialize` 的三个字段是客户端握手的硬要求（缺一个 n8n 直接报 protocol error）
    #[test]
    fn initialize_result_shape() {
        let s = server_info();
        assert_eq!(s["protocolVersion"], PROTOCOL_VERSION);
        assert!(s["capabilities"]["tools"].is_object());
        assert_eq!(s["serverInfo"]["name"], "dms-ai");
        assert!(s["serverInfo"]["version"].is_string());
    }

    /// 工具返回体：`content` 数组 + `type:"text"`（不产 isError，失败走 -32000）
    #[test]
    fn tool_content_is_text_array() {
        let c = text_content(json_text(&json!({ "hits": [] })));
        assert_eq!(c["content"][0]["type"], "text");
        assert_eq!(c["content"][0]["text"], "{\"hits\":[]}");
        assert!(c.get("isError").is_none());
    }

    /// 节点关键字匹配：表名/列名/注释/域任一命中；大小写归一化收在 `node_matches` 内部
    ///（关键字与字段值两侧都不敏感，调用方不必记得先 lower —— 契约钉在这里）
    #[test]
    fn node_matches_any_metadata_field_case_insensitively() {
        let table =
            json!({"kind": "table", "table": "DWS_Sale", "comment": "销售汇总", "domain": "销售"});
        assert!(
            node_matches(&table, "dws_"),
            "表名命中（字段值大小写不敏感）"
        );
        assert!(node_matches(&table, "dws_sale"), "完整表名");
        assert!(node_matches(&table, "销售汇总"), "注释命中");
        assert!(node_matches(&table, "销售"), "域命中");
        assert!(!node_matches(&table, "门店"), "全不中");
        let column = json!({"kind": "column", "table": "dws_sale", "column": "StoreCode", "comment": "客户编码"});
        assert!(
            node_matches(&column, "storecode"),
            "列名命中（字段值大小写不敏感）"
        );
        assert!(node_matches(&column, "客户"), "列注释命中");
        // 缺字段不 panic（节点 JSON 形状由 load_nodes 保证，这里钉容错）
        assert!(!node_matches(&json!({"kind": "table"}), "x"));
    }

    /// 限量参数：缺省/非数字 → default；越界 clamp；零与负数收进下界
    #[test]
    fn opt_limit_defaults_and_clamps() {
        assert_eq!(opt_limit(&json!({}), "limit", 50, 1, 200), 50);
        assert_eq!(
            opt_limit(&json!({"limit": "abc"}), "limit", 50, 1, 200),
            50,
            "非数字按缺省"
        );
        assert_eq!(opt_limit(&json!({"limit": 5}), "limit", 50, 1, 200), 5);
        assert_eq!(opt_limit(&json!({"limit": 0}), "limit", 50, 1, 200), 1);
        assert_eq!(opt_limit(&json!({"limit": -9}), "limit", 50, 1, 200), 1);
        assert_eq!(
            opt_limit(&json!({"limit": 99999}), "limit", 50, 1, 500),
            500
        );
    }
}
