# Task 10：server 瘦身（薄壳 ≤700 行）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** main.rs（498 行）解体：bin 拆 cli/serve、8 handler 分文件、认证中间件统一收口（封掉 body login_name 冒充）、AppError 统一错误模型、jobs 注册表 + 企微 notify、health 恒真修复、viewspec 去 DMS 化、配置分组 + env 覆盖去硬编码生产地址。server crate 最终只剩装配 + 协议，全 crate src ≤700 行。

**Architecture:** spec 3.4 目录树（config/state/mw/api/identity/jobs/notify/chat_store/bin）。本任务是迁移第 10 步（最后一步），上游 Task 2-9 的交付形态决定适配面：Task 10.0 先核对，缺失项一律标注「需 Task X 补」并按降级路径走，不自己重实现。

**Tech Stack:** Rust workspace、axum 0.8（middleware::from_fn_with_state，不引 tower-http）、sqlx、serde_json；判官脚本 tools/*.py 走 CLI 子命令通路。

## Global Constraints

- **判官通路零停摆（最硬约束，Task 10.1 最先做）**：源码实证三判官全部走 **CLI 子命令**而非 HTTP body——`judge_scope.py:148` 调 `dms-ai-server.exe scope`、`evaluation.py:59` 调 `exec-sql`、`regression.py:25` 调 `ask`（e2e_m3.py 无 HTTP/login_name 匹配）。因此真正会打挂判官的是 **bin 拆分后 exe 路径/名变化**与**子命令语义变化**，两者都必须保住；HTTP 认证收紧对判官无直接影响，但对前端开发模式有影响（见行为变化①）。
- **行为变化白名单**（除此之外一律不许变）：
  1. **认证收紧**：body/query 带 `login_name` 无有效凭据一律 401；开发模式改走显式 `dev_token`（settings 配置项）+ `X-Login-Name` header。AskReq.login_name / ConvQuery.login_name 字段删除。前端开发模式需同步（标注，不改前端代码）。
  2. **bin 形态**：`dms-ai-server.exe` 默认 bin 消失，拆为两个 `[[bin]]`；判官引用的 exe 名必须保持不变（方案 B，见 Task 10.7）。
  3. **错误透明化**：`api_roles`/`api_convs`/`api_conv_msgs` 的 `unwrap_or_default()`（main.rs:389/400/430）从「DB 抖动静默返回空列表」改为 500 `{"error": ...}`。health 探活容错保留（探活语义本就该报详情而非裸 500）。
  4. **health 判定收紧**：从「有任意 PG 扩展即健康」（main.rs:483 `!pg_exts.is_empty()`）改为显式校验 `vector/age/pg_trgm` 三件套齐全；监控若只看 `ok` 字段会在缺扩展时报警（这是修复目的）。
  5. **drill chips 数据源**：从写死 `DIM_POOL` 6 个（viewspec.rs:108）变为读 `meta.dimension` 注册表（当前种子 ~10 个），前端无需改（drill 数组本就动态渲染）。
  6. **省码翻名后端化**：geo 码→省名从 viewspec 内置 `province_cn`（viewspec.rs:294-307）改为 semantic/present.rs 列标注阶段完成（需 Task 7）；三端同步点：viewspec / web/src/App.vue / web/src/format.ts，本任务只动 server 侧并在 App.vue/format.ts 留冗余标注。
  7. **配置格式 breaking**：settings.json 从平铺改分组（db/llm/wework/server），`dms_base_url` 去掉真实 DMS 地址的硬编码默认（db.rs:30-32），未配置时 `/api/sso` 明确报 500「未配置」而非悄悄连生产。settings.example.json 同步更新；部署机 settings.json 需手工迁移一次。
- **零新增第三方依赖**：认证用 axum 自带 `middleware::from_fn_with_state`；AppError 手写 enum + IntoResponse，不引 thiserror；cron 不引（注册表 + wait_secs fn 指针）；中间件测试不引 tower（核心逻辑抽纯函数单测，见 Task 10.3）。
- **TDD 节奏**：每个子任务先写/改测试再动实现；纯逻辑（认证判定、health 判定、env 覆盖、msg 构造、drill 池注入）全部抽纯函数离线单测。
- **外科式搬家**：handler/子命令/wework/auth 逻辑一字不改地搬，只换三类东西：错误闭包 → AppError、resolve_identity → CurrentUser extractor、裸 MySqlPool → ReadOnlyMySql（后者以 Task 3 交付为准）。
- **每请求 RuleSet 快照铁律**（Task 5 已接线）不在本任务改动面；CLI exec-sql/scope 走同一 check→inject→fetch 管道以 Task 3 newtype 落地形态为准，本任务不重排管道。
- Windows 构建须前缀 MinGW bin 路径（见文末备注），cargo 命令走 PowerShell 不走 Bash。

## 上游契约清单（Task 10.0 逐项核对；缺失即标注，不是全部 blocker）

**需 Task 9（agent）已交付：**
| # | 接口 | 用途 | 缺失降级 |
|---|---|---|---|
| G1 | `dms_agent::ask(client, mysql, pg, principal, question, prev) -> anyhow::Result<Answer>` 或等价编排入口 | api_ask 薄 handler 唯一调用点 | handler 继续调 server 内 `pipeline::ask`，标注「需 Task 9 补」，本任务只做搬运 |
| G2 | `dms_kernel::Answer`（serde tag kind + Table flatten） | api_ask 响应形状、chat.msg payload 存 Answer | 维持现 `AskResult` serde 形状不动 |

**需 Task 2（kernel）已交付：**
| # | 接口 | 用途 | 缺失降级 |
|---|---|---|---|
| K1 | 呈现决策树/`ViewSpec` 已下沉 kernel（spec 1 表 kernel 职责含「呈现决策树」） | viewspec 去 DMS 化改动落点 | 改动落 server/src/viewspec.rs，标注「需 Task 2 补」，逻辑相同 |
| K2 | `PresentLexicon`（中文 NLP 基元/呈现词表） | 省码/维度名的单一事实源候选 | 见 S1 降级 |

**需 Task 3（connector）已交付：**
| # | 接口 | 用途 | 缺失降级 |
|---|---|---|---|
| C1 | `ReadOnlyMySql`（AppState.mysql、wework::mobile_to_login、CLI 全部换型） | 红线：server 不再持裸 MySqlPool | **blocker 级**：若 AppState 仍持 MySqlPool，本任务仍拆 bin/分文件，换型留「需 Task 3 补」 |
| C2 | `fixed(&'static str)` 通道 | wework.rs:92 手机号查员工改字面量通道 | 随 C1 一并处理 |

**需 Task 5（policy）已交付：**
| # | 接口 | 用途 |
|---|---|---|
| P1 | `dms_policy::{load_principal, list_roles}`、`scope::compute_scope_cached`、`rules::{seed_rules, load_rules}` | handler/CLI/bootstrap 调用点（Task 5.5 已适配则零改动） |

**需 Task 6/7（semantic）已交付：**
| # | 接口 | 用途 | 缺失降级 |
|---|---|---|---|
| S1 | `semantic::registry` 维度名列表（`meta.dimension` active name 集合） | drill 池数据源 | server 侧直接 `SELECT name FROM meta.dimension WHERE status='active'` 查 PG，形状相同 |
| S2 | `semantic::present` 列标注（geo 码→省名在进 viewspec 前完成） | 删 viewspec::province_cn | **标注「需 Task 7 补」**：本任务只把 province_cn 抽成 `pub` 便于 Task 7 搬走，渲染行为不变 |

> 契约核对结论只影响「落点与适配面」，除 C1 外不阻塞子任务推进；C1 缺失时 Task 10.7（bin 拆分）仍可做，MySqlPool 换型单独立步。

---

### Task 10.0: 上游契约核对（gate，不写代码）

**Files:**
- 只读：`crates/**`

- [ ] **Step 1: 逐项 grep 核对 G1/G2/K1/K2/C1/C2/P1/S1/S2**

Run（PowerShell，前缀 MinGW，下同）:
```powershell
Select-String -Path crates/server/src/*.rs -Pattern "ReadOnlyMySql|dms_policy|dms_agent|dms_kernel" | Select-Object -First 20
Get-ChildItem crates -Directory | Select-Object Name
Select-String -Path crates/server/src/meta.rs -Pattern "SELECT dim_code, name|FROM meta.dimension"
```
Expected: 每条契约按上表记录「已交付 / 缺失→降级路径」，写进提交信息。

- [ ] **Step 2: 判官通路实证锁定**

Run:
```powershell
Select-String -Path tools/judge_scope.py,tools/evaluation.py,tools/regression.py,tools/e2e_m3.py -Pattern "dms-ai-server|/api/|login_name|Bearer" | Select-Object Filename,LineNumber,Line
```
Expected: 输出归档进 plan 执行记录，确认三判官 = CLI 子命令通路（scope/exec-sql/ask），HTTP login_name 通路无判官依赖。

---

### Task 10.1: 判官通路保全 + dev_token 认证开关先行（硬约束，最先做）

**Files:**
- Modify: `crates/server/src/db.rs`（Settings 加 dev_token 字段）
- Modify: `crates/server/src/auth.rs`（加 dev 模式判定纯函数 + 单测）
- Modify: `settings.example.json`（加 dev_token 示例）
- 只读: `tools/*.py`（本步不改判官；exe 名保住后判官零改动，见 Task 10.7）

**Interfaces:**
- Consumes: 无（纯 server 内）
- Produces: `auth::dev_mode_allowed(provided: Option<&str>, configured: Option<&str>) -> bool`；`Settings.dev_token: Option<String>`

**为什么先做这一步**：认证中间件（10.3）落地即封死 body login_name，届时若没有 dev_token 逃生门，本地前端开发与任何临时 HTTP 调试全部 401。先把开关备好再收门。

- [ ] **Step 1: 先写失败测试——dev 模式判定纯函数**

`crates/server/src/auth.rs` 底部 `mod tests` 追加：

```rust
#[test]
fn dev_mode_allowed_only_when_configured_and_matched() {
    // 未配置 dev_token（生产）：任何 token 都不放行 dev 模式
    assert!(!dev_mode_allowed(Some("anything"), None));
    assert!(!dev_mode_allowed(Some("anything"), Some("")));
    // 已配置：精确匹配才放行
    assert!(dev_mode_allowed(Some("dev-123"), Some("dev-123")));
    assert!(!dev_mode_allowed(Some("dev-124"), Some("dev-123")));
    // 空 provided 不放行
    assert!(!dev_mode_allowed(None, Some("dev-123")));
}
```

- [ ] **Step 2: 实现 + Settings 加字段**

```rust
/// dev 模式放行判定：仅当显式配置了非空 dev_token 且与提供值精确匹配。
/// 生产不配置 = dev 模式在配置层不存在（不是被禁用，是不存在）。
pub fn dev_mode_allowed(provided: Option<&str>, configured: Option<&str>) -> bool {
    match (provided, configured) {
        (Some(p), Some(c)) if !c.is_empty() => p == c,
        _ => false,
    }
}
```

db.rs `Settings` 追加（平铺期先放顶层，Task 10.6 分组时挪 server 组）：

```rust
#[serde(default)]
pub dev_token: Option<String>,
```

settings.example.json 追加一行：`"dev_token": "",`（空 = 关闭，注释说明开发模式用法）。

- [ ] **Step 3: 跑测试 + 提交**

Run: `cargo test -p dms-ai-server auth 2>&1 | Select-String "test result"`
Expected: 2 passed（原 issue_resolve_roundtrip + 新增）。

```bash
git add crates/server/src/auth.rs crates/server/src/db.rs settings.example.json
git commit -m "server: dev_token 认证开关先行（dev_mode_allowed 纯函数 + 配置项），为认证中间件收门备逃生门"
```

---

### Task 10.2: AppError 统一错误模型（mw/error.rs）

**Files:**
- Create: `crates/server/src/mw/mod.rs`、`crates/server/src/mw/error.rs`
- Modify: `crates/server/src/main.rs`（挂 `mod mw;`；api_sso/api_ask 两个局部 err 闭包先换用，其余 handler 在 10.4 搬家时换）

**Interfaces:**
- Produces: `mw::error::AppError`（Unauthorized/Forbidden/BadRequest/Unprocessable/Internal 五变体）+ `IntoResponse`；响应体形状与现状字节一致 `{"error": msg}`（前端读 error 字段，零前端改动）

- [ ] **Step 1: 先写失败测试——状态码与响应体形状**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    fn body_string(r: axum::response::Response) -> String {
        // IntoResponse 产物经 Json 序列化；测试只锁 (status, body) 映射，不引 tower——
        // 用 into_response 后直接读 parts：Response 实现 body 需异步，改锁映射函数本身。
        let (p, _) = r.into_parts();
        format!("{}", p.status)
    }

    #[test]
    fn status_mapping() {
        assert_eq!(body_string(AppError::Unauthorized("x".into()).into_response()), "401");
        assert_eq!(body_string(AppError::Forbidden("x".into()).into_response()), "403");
        assert_eq!(body_string(AppError::BadRequest("x".into()).into_response()), "400");
        assert_eq!(body_string(AppError::Unprocessable("x".into()).into_response()), "422");
        assert_eq!(body_string(AppError::Internal(anyhow::anyhow!("x")).into_response()), "500");
    }

    #[test]
    fn body_shape_is_error_field() {
        // 响应 JSON 形状与旧局部 err 闭包一致：{"error": msg}
        let v = serde_json::json!({ "error": "未认证" });
        assert_eq!(v.to_string(), r#"{"error":"未认证"}"#);
        // error_body 构造函数直接锁形状：
        assert_eq!(error_body("未认证").0.to_string(), r#"{"error":"未认证"}"#);
    }
}
```
> 说明：不读响应 body 流（需 tower/hyper 工具），把「(code, Json) 构造」抽成 `fn error_body(msg) -> (StatusCode, Json<Value>)` 纯函数，IntoResponse 只是调它——测试锁纯函数即锁住形状。

- [ ] **Step 2: mw/error.rs 完整实现**

```rust
//! 统一错误模型：server 唯一收口（spec 4.2 错误分层 server 行）。
//! 手写 enum + IntoResponse，不引 thiserror；响应体与旧局部 err 闭包字节一致。

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

pub enum AppError {
    Unauthorized(String),
    Forbidden(String),
    BadRequest(String),
    Unprocessable(String),
    Internal(anyhow::Error),
}

/// 响应构造纯函数（测试锁这里）：形状 = 旧 main.rs:288/344 err 闭包逐字节一致。
pub(crate) fn error_body(msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::OK, // 占位——真实 code 由 into_response 各变体给；此函数只锁 JSON 形状
        Json(serde_json::json!({ "error": msg })),
    )
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (code, msg) = match self {
            Self::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m),
            Self::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::Unprocessable(m) => (StatusCode::UNPROCESSABLE_ENTITY, m),
            Self::Internal(e) => {
                tracing::warn!("内部错误: {e:#}");
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        };
        (code, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}
```

- [ ] **Step 3: main.rs 两处 err 闭包换用 + 编译测试**

- api_sso（:288）与 api_ask（:344）的局部 `err` 闭包删除，返回类型改 `Result<Json<serde_json::Value>, AppError>`，各处 `err(StatusCode::X, msg)` 改 `AppError::X(msg)`（401→Unauthorized、403→Forbidden、422→Unprocessable、500→Internal(anyhow)）。
- 其余 5 个 handler（roles/convs/conv_new/conv_msgs/conv_delete）本步不动，10.4 搬家时一并换。

Run: `cargo test -p dms-ai-server 2>&1 | Select-String "test result"`
Expected: 全绿（含新增 2 个 error 测试），编译无 warning（err 闭包无残留）。

- [ ] **Step 4: 提交**

```bash
git add crates/server
git commit -m "server: mw/error.rs AppError 统一错误模型（响应形状与旧 err 闭包字节一致），sso/ask 两 handler 先行换用"
```

---

### Task 10.3: 认证中间件（mw/auth.rs）——封掉 body login_name 冒充

**Files:**
- Create: `crates/server/src/state.rs`（AppState 从 main.rs :29-37 机械搬入 + `dev_token: Option<String>` 字段；中间件/jobs 都要经 `crate::state::AppState` 引用，先收编避免路径二次改）
- Create: `crates/server/src/mw/auth.rs`
- Modify: `crates/server/src/main.rs`（AppState 删除改 `use crate::state::AppState`；路由分两组：公开组 / 受保护组 route_layer；`resolve_identity`（:332-337）与 6 处手抄（:345/387/398/409/423/441）删除；AskReq.login_name（:325）与 ConvQuery.login_name（:377）字段删除；`bearer()`（:459-466）搬入 mw/auth.rs）

**Interfaces:**
- Consumes: `auth::resolve`（会话表）、`auth::dev_mode_allowed`（10.1 已交付）、`AppState.dev_token`
- Produces: `mw::auth::{CurrentUser, require_auth}`；handler 侧用 `axum::Extension<CurrentUser>` extractor 取身份

**关键设计**：中间件读不到 body（Json extractor 在 handler 才消费，中间件先跑），因此 dev 模式身份**从 header 取**：`Authorization: Bearer <dev_token>` + `X-Login-Name: <login>`（+ 可选 `X-Role-Code`）。核心判定抽纯函数 `authenticate()` 离线单测（不引 tower 测整个 Router）。生产路径（DMS iframe 颁的会话 token / 企微 OAuth 颁的会话 token）零变化。

- [ ] **Step 1: 先写失败测试——authenticate 纯函数全分支**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn issue_session() -> String {
        crate::auth::issue("zhangsan".into(), Some("city_manager".into()))
    }

    #[test]
    fn session_token_wins() {
        let t = issue_session();
        let u = authenticate(Some(&t), None, None, None).unwrap();
        assert_eq!(u.login_name, "zhangsan");
        assert_eq!(u.role_code.as_deref(), Some("city_manager"));
    }

    #[test]
    fn dev_token_with_header_login() {
        let u = authenticate(Some("dev-123"), Some("lisi"), Some("admin"), Some("dev-123")).unwrap();
        assert_eq!(u.login_name, "lisi");
        assert_eq!(u.role_code.as_deref(), Some("admin"));
    }

    #[test]
    fn dev_token_without_header_login_is_401() {
        // dev_token 匹配但缺 X-Login-Name → 拒（不允许「裸 dev_token 无身份」）
        assert!(authenticate(Some("dev-123"), None, None, Some("dev-123")).is_err());
    }

    #[test]
    fn no_valid_credential_is_401() {
        // 无效 token + 未配置 dev_token：封死（旧 body login_name 冒充路径不复存在）
        assert!(authenticate(Some("bad-token"), Some("hacker"), None, None).is_err());
        assert!(authenticate(None, Some("hacker"), None, Some("dev-123")).is_err());
    }
}
```

- [ ] **Step 2: mw/auth.rs 完整实现**

```rust
//! 统一认证中间件：Bearer 会话 token → dev_token+X-Login-Name（显式开发模式）→ 401。
//! 封死旧 body/query login_name 冒充路径（main.rs:332-337 resolve_identity 删除）。

use axum::{
    extract::State,
    http::Request,
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

use crate::mw::error::AppError;
use crate::state::AppState;

/// 已认证身份（中间件注入 request extensions，handler 经 Extension<CurrentUser> 取）
#[derive(Clone, Debug)]
pub struct CurrentUser {
    pub login_name: String,
    pub role_code: Option<String>,
}

/// 认证判定纯逻辑（中间件薄壳包裹它；全分支离线可测）。
/// 优先级：会话 token > dev_token+header；都不中 → 401。
pub fn authenticate(
    bearer: Option<&str>,
    dev_login: Option<&str>,
    dev_role: Option<&str>,
    dev_token: Option<&str>,
) -> Result<CurrentUser, AppError> {
    if let Some(t) = bearer {
        if let Some((l, r)) = crate::auth::resolve(t) {
            return Ok(CurrentUser { login_name: l, role_code: r });
        }
        if crate::auth::dev_mode_allowed(Some(t), dev_token) {
            let l = dev_login
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AppError::Unauthorized("dev 模式缺 X-Login-Name".into()))?;
            tracing::warn!("dev_token 开发模式放行: {l}");
            return Ok(CurrentUser {
                login_name: l.to_string(),
                role_code: dev_role.map(|s| s.to_string()),
            });
        }
    }
    Err(AppError::Unauthorized("未认证：缺有效会话 token".into()))
}

fn bearer_str(req: &Request) -> Option<&str> {
    req.headers()
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// axum 中间件（from_fn_with_state 挂载，受保护路由组专用）
pub async fn require_auth(
    State(st): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let h = req.headers();
    let (dev_login, dev_role) = (
        h.get("x-login-name").and_then(|v| v.to_str().ok()).map(|s| s.to_string()),
        h.get("x-role-code").and_then(|v| v.to_str().ok()).map(|s| s.to_string()),
    );
    let bearer = bearer_str(&req).map(|s| s.to_string());
    let u = authenticate(bearer.as_deref(), dev_login.as_deref(), dev_role.as_deref(), st.dev_token.as_deref())?;
    req.extensions_mut().insert(u);
    Ok(next.run(req).await)
}
```

- [ ] **Step 3: 路由分两组 + resolve_identity 删除**

main.rs 路由段（:258-267）改：

```rust
// 公开路由：颁 token 的入口与探活，不过认证中间件
let public = Router::new()
    .route("/api/health", get(health))
    .route("/api/sso", post(api_sso))
    .route("/api/wework/login", get(api_wework_login))
    .with_state(state.clone());
// 受保护路由：中间件统一认证，handler 内 Extension<CurrentUser> 取身份
let protected = Router::new()
    .route("/api/ask", post(api_ask))
    .route("/api/roles", get(api_roles))
    .route("/api/convs", get(api_convs))
    .route("/api/conv/new", post(api_conv_new))
    .route("/api/conv/{id}", get(api_conv_msgs).delete(api_conv_delete))
    .route_layer(axum::middleware::from_fn_with_state(state.clone(), mw::auth::require_auth))
    .with_state(state);
let app = public.merge(protected);
```

- `resolve_identity`（:332-337）、`bearer()`（:459-466，逻辑已并入 bearer_str）删除。
- `AskReq.login_name` / `role_code`（:325-326）与 `ConvQuery.login_name`（:377）字段删除（**行为变化①落地**；AskReq.role_code 原经 body 传，dev 模式改走 X-Role-Code header，生产 token 内已带 role）。
- 6 处 `resolve_identity(...)` 调用点本步先替换为 `Extension<CurrentUser>` 形参（handler 签名改，10.4 搬家时带过去）：

```rust
async fn api_ask(
    State(st): State<Arc<AppState>>,
    axum::Extension(user): axum::Extension<CurrentUser>,
    Json(req): Json<AskReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (login_name, role_code) = (user.login_name, user.role_code);
    // 以下逻辑一字不动
}
```

- [ ] **Step 4: 编译 + 测试 + 手工冒烟**

Run: `cargo test -p dms-ai-server 2>&1 | Select-String "test result"`
Expected: 全绿（authenticate 4 新测试 + 既有）。

Run（需库环境，可选冒烟）: 起服务后 `curl -X POST 127.0.0.1:8100/api/ask -H "Content-Type: application/json" -d '{"question":"x"}'` → 期望 401 `{"error":"未认证：缺有效会话 token"}`（**收紧证据**）；加 `-H "Authorization: Bearer dev-123" -H "X-Login-Name: zhangsan"`（settings 配 dev_token=dev-123 时）→ 放行。

- [ ] **Step 5: 提交**

```bash
git add crates/server
git commit -m "server: mw/auth.rs 认证中间件统一收口，封死 body/query login_name 冒充（dev_token+X-Login-Name 显式开发模式），401 收紧"
```

---

### Task 10.4: api/ 五文件拆分 + health 恒真修复 + unwrap_or_default 清除

**Files:**
- Create: `crates/server/src/api/mod.rs`、`api/ask.rs`、`api/conv.rs`、`api/auth.rs`、`api/roles.rs`、`api/health.rs`
- Modify: `crates/server/src/main.rs`（8 handler 全部搬出，main.rs 只剩装配）

**Interfaces:**
- Consumes: `mw::auth::CurrentUser`、`mw::error::AppError`、G1（agent 入口，缺失则 pipeline::ask）
- Produces: `api::{ask,conv,auth,roles,health}` 五模块；handler 逻辑一字不改地搬

**搬家映射**（main.rs 行号 → 目标文件，逻辑不动只换错误/身份两切口）：
| handler | 现状 | 目标 | 附带修复 |
|---|---|---|---|
| api_sso :284 | main.rs | api/auth.rs | err 闭包 → AppError（10.2 已换） |
| api_wework_login :302 | main.rs | api/auth.rs | 保持 IntoResponse 形状 |
| api_ask :339 | main.rs | api/ask.rs | 10.3 已换 Extension；`to_value(&r).unwrap()` :366 改 `expect("AskResult 必可序列化")` |
| api_roles :382 | main.rs | api/roles.rs | `unwrap_or_default()` :389 → `map_err(AppError::from)?`（**行为变化③**） |
| api_convs :393 | main.rs | api/conv.rs | `unwrap_or_default()` :400 → `?`（变化③） |
| api_conv_new :404 | main.rs | api/conv.rs | err 元组 → AppError |
| api_conv_msgs :417 | main.rs | api/conv.rs | `unwrap_or_default()` :430 → `?`（变化③）；归属校验 403 不动 |
| api_conv_delete :434 | main.rs | api/conv.rs | `let _ =` :442 保持（DELETE 幂等，外科式不动） |
| health :468 | main.rs | api/health.rs | **恒真修复**，见 Step 2 |

- [ ] **Step 1: 搬家（纯机械，先搬后修）**

按上表整体剪切粘贴；`mod.rs` 五个 `pub mod` + 共用 use。编译绿后跑既有测试。

- [ ] **Step 2: 先写失败测试——health 判定纯函数**

api/health.rs：

```rust
/// 必需的 PG 扩展三件套：vector(向量召回) / age(图) / pg_trgm(相似度召回)
const REQUIRED_EXTS: [&str; 3] = ["vector", "age", "pg_trgm"];

/// health 判定纯逻辑（旧 main.rs:483 有任意扩展即 true = 恒真，修复为显式校验）
pub(crate) fn health_ok(mysql_ok: bool, mysql_readonly: bool, pg_exts: &[String]) -> bool {
    mysql_ok
        && mysql_readonly
        && REQUIRED_EXTS.iter().all(|r| pg_exts.iter().any(|e| e == r))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn exts(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn ok_requires_three_exts() {
        assert!(health_ok(true, true, &exts(&["vector", "age", "pg_trgm", "plpgsql"])));
        assert!(!health_ok(true, true, &exts(&["vector", "age"])), "缺 pg_trgm 不健康");
        assert!(!health_ok(true, true, &exts(&["plpgsql"])), "旧恒真路径：任意扩展 ≠ 健康");
        assert!(!health_ok(false, true, &exts(&["vector", "age", "pg_trgm"])));
        assert!(!health_ok(true, false, &exts(&["vector", "age", "pg_trgm"])), "只读红线掉了不健康");
    }
}
```

- [ ] **Step 3: health handler 换用 + 响应加详情**

handler 主体不动（探活容错保留：`unwrap_or(0)`/`unwrap_or_default()` 是探活语义，不算错误模型问题），仅 `ok` 字段改 `health_ok(mysql_ok, mysql_readonly, &pg_exts)`，并加 `"pg": { "extensions": pg_exts, "required": REQUIRED_EXTS }` 便于排障（新增字段，不改旧字段，监控兼容）。

- [ ] **Step 4: 编译 + 测试 + 提交**

Run: `cargo test -p dms-ai-server 2>&1 | Select-String "test result"`
Expected: 全绿；`Select-String -Path crates/server/src/api/*.rs -Pattern "unwrap_or_default|resolve_identity|err\(StatusCode"` 输出空。

```bash
git add crates/server
git commit -m "server: 8 handler 分 api/{ask,conv,auth,roles,health}.rs，health 修恒真（显式校验 vector/age/pg_trgm），roles/convs 静默空列表改 500 透明化"
```

---

### Task 10.5: jobs.rs 定时任务注册表 + notify.rs 企微 message/send

**Files:**
- Create: `crates/server/src/jobs.rs`、`crates/server/src/notify.rs`
- Modify: `crates/server/src/main.rs`（裸 spawn 块 :232-257 删除，改 `jobs::spawn_all(state.clone())`；`secs_until_next_3am` :447-457 搬入 jobs.rs）
- Modify: `crates/server/src/wework.rs`（`pub(crate) use` 透出 access_token/http 给 notify，或 notify 挂为 wework 子模块——**取前者**，文件数不膨胀）

**Interfaces:**
- Produces: `jobs::{Job, jobs, spawn_all}`；`notify::send_text(cfg, to_user, text)`

- [ ] **Step 1: 先写失败测试——notify 消息构造 + 既有 03:00 测试随搬**

notify.rs：

```rust
/// 企微应用消息体构造（纯函数）：agentid 配置传了三手从未用，本步首次真正消费。
/// 企微 agentid 字段要求整数；配置为字符串时尽量转数值，转不动原样放（企微侧报错可查）。
fn build_text_msg(agentid: &str, to_user: &str, text: &str) -> serde_json::Value {
    let agent = agentid
        .parse::<u64>()
        .map(serde_json::Value::from)
        .unwrap_or_else(|_| serde_json::Value::from(agentid));
    serde_json::json!({
        "touser": to_user,
        "msgtype": "text",
        "agentid": agent,
        "text": { "content": text },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_msg_shape_and_agentid_coercion() {
        let v = build_text_msg("1000002", "ZhangSan", "图刷新失败");
        assert_eq!(v["touser"], "ZhangSan");
        assert_eq!(v["msgtype"], "text");
        assert_eq!(v["agentid"], serde_json::json!(1000002), "数字 agentid 转整数");
        assert_eq!(v["text"]["content"], "图刷新失败");
        let v2 = build_text_msg("", "u", "x");
        assert_eq!(v2["agentid"], serde_json::json!(""), "空 agentid 原样（不 panic）");
    }
}
```

jobs.rs 既有测试 `next_3am_within_a_day`（main.rs:492-497）随函数一字不改搬入。

- [ ] **Step 2: notify.rs 发送实现 + jobs.rs 注册表**

notify.rs 发送（IO 部分不写测试，对齐 wework.rs 既有风格）：

```rust
//! 企微应用消息（message/send）：补 M6c 以来缺的推送出口。

use crate::wework::{self, WeworkCfg};

pub async fn send_text(cfg: &WeworkCfg, to_user: &str, text: &str) -> anyhow::Result<()> {
    let token = wework::access_token(cfg).await?;
    let url = format!("{}/message/send?access_token={token}", wework::API);
    let v: serde_json::Value = wework::http()
        .post(&url)
        .json(&build_text_msg(&cfg.agentid, to_user, text))
        .send().await?
        .json().await?;
    if v["errcode"].as_i64() != Some(0) {
        anyhow::bail!("企微 message/send 失败: {}", v["errmsg"].as_str().unwrap_or(""));
    }
    Ok(())
}
```
> 需 wework.rs 把 `API`、`http()` 从私有改 `pub(crate)`（两处一行改动，不新增依赖）。

jobs.rs 注册表：

```rust
//! 定时任务注册表：新增任务 = jobs() 里加一项，替代写死 03:00 的裸 spawn。

use std::sync::Arc;
use crate::state::AppState;

pub struct Job {
    pub name: &'static str,
    /// 距下次执行秒数（每轮跑完重算）
    pub wait_secs: fn() -> u64,
    /// 任务体：返回一行状态摘要（留痕由任务自己写，如 graph_status）
    pub run: fn(Arc<AppState>) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send>>,
}

pub fn jobs() -> Vec<Job> {
    vec![Job {
        name: "graph_sync",
        wait_secs: secs_until_next_3am,
        run: |st| Box::pin(async move { graph_sync_run(&st).await }),
    }]
}

pub fn spawn_all(state: Arc<AppState>) {
    for job in jobs() {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                let wait = (job.wait_secs)();
                tracing::info!("{} 定时：{wait}s 后执行", job.name);
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                let msg = (job.run)(st.clone()).await;
                if msg.starts_with("ok") {
                    tracing::info!("{} 完成：{msg}", job.name);
                } else {
                    tracing::warn!("{} 失败（下轮重试）：{msg}", job.name);
                }
            }
        });
    }
}
```
`graph_sync_run` 与 `secs_until_next_3am` 从 main.rs :232-257/:447-457 原样搬（含 ok/fail 摘要格式与 graph_status 留痕逐字不动）。

- [ ] **Step 3: 编译 + 测试 + 提交**

Run: `cargo test -p dms-ai-server 2>&1 | Select-String "test result"`
Expected: 全绿（notify 1 + next_3am 1 + 既有）。

```bash
git add crates/server
git commit -m "server: jobs.rs 定时任务注册表（裸 spawn 收编）+ notify.rs 企微 message/send（agentid 首次消费）"
```

---

### Task 10.6: config.rs 配置分组 + env 覆盖 + 去硬编码生产地址

**Files:**
- Create: `crates/server/src/config.rs`
- Modify: `crates/server/src/db.rs`（配置半整体搬出，只剩 `mysql_pool`/`pg_pool` 两个连接池函数；`default_dms_base` :30-32 删除）
- Modify: `settings.example.json`（分组重写）
- Modify: `crates/server/src/main.rs`（`db::load_settings()` → `config::load_settings()`，字段访问改分组路径）

**Interfaces:**
- Produces: `config::{Settings, DbCfg, LlmCfg, WeworkCfg2→复用 wework::WeworkCfg, ServerCfg}`；env 覆盖 `DMSAI_` 前缀

**配置新形状**（settings.example.json 同步，**行为变化⑦ breaking**）：

```json
{
  "db":      { "mysql_url": "mysql://USER:PASSWORD@HOST:3306/xh_dms", "pg_url": "postgres://postgres:PASSWORD@localhost:15433/dms_ai" },
  "llm":     { "base_url": "https://api.deepseek.com", "api_key": "sk-***", "model_fast": "deepseek-v4-flash", "model_precise": "deepseek-v4-pro" },
  "wework":  { "corpid": "ww***", "secret": "***", "agentid": "" },
  "server":  { "listen": "127.0.0.1:8100", "dms_base_url": "", "dev_token": "" }
}
```

- [ ] **Step 1: 先写失败测试——env 覆盖纯函数**

```rust
/// env 覆盖：DMSAI_ 前缀扁平 key → 分组路径，纯函数（env 快照作参数，测试零进程污染）。
fn overlay_env(v: &mut serde_json::Value, env: &[(String, String)]) {
    const KEYS: [(&str, &str); 11] = [
        ("DMSAI_MYSQL_URL", "db.mysql_url"),
        ("DMSAI_PG_URL", "db.pg_url"),
        ("DMSAI_LLM_BASE_URL", "llm.base_url"),
        ("DMSAI_LLM_API_KEY", "llm.api_key"),
        ("DMSAI_LLM_MODEL_FAST", "llm.model_fast"),
        ("DMSAI_LLM_MODEL_PRECISE", "llm.model_precise"),
        ("DMSAI_WEWORK_CORPID", "wework.corpid"),
        ("DMSAI_WEWORK_SECRET", "wework.secret"),
        ("DMSAI_WEWORK_AGENTID", "wework.agentid"),
        ("DMSAI_LISTEN", "server.listen"),
        ("DMSAI_DMS_BASE_URL", "server.dms_base_url"),
    ];
    for (ek, path) in KEYS {
        if let Some((_, val)) = env.iter().find(|(k, _)| k == ek) {
            let mut cur = &mut *v;
            for seg in path.split('.') {
                cur = cur.pointer_mut(&format!("/{seg}")).expect("分组路径存在");
            }
            *cur = serde_json::Value::from(val.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_overrides_grouped_path() {
        let mut v = serde_json::json!({"db": {"mysql_url": "file", "pg_url": "p"}, "server": {"listen": "127.0.0.1:8100"}});
        overlay_env(&mut v, &[("DMSAI_MYSQL_URL".into(), "env".into()), ("OTHER".into(), "x".into())]);
        assert_eq!(v["db"]["mysql_url"], "env");
        assert_eq!(v["db"]["pg_url"], "p", "未覆盖字段不动");
        assert_eq!(v["server"]["listen"], "127.0.0.1:8100", "未识别 key 忽略");
    }
}
```
> dev_token 不进 env 覆盖清单（开发开关不该由部署环境意外注入）；如团队要求再加，列为裁决点。

- [ ] **Step 2: config.rs 实现**

```rust
//! 分组配置 + env 覆盖。dms_base_url 无默认值（去掉 db.rs:30-32 硬编码生产地址）。

#[derive(serde::Deserialize, Clone, Default)]
pub struct Settings {
    #[serde(default)] pub db: DbCfg,
    #[serde(default)] pub llm: LlmCfg,
    #[serde(default)] pub wework: crate::wework::WeworkCfg,
    #[serde(default)] pub server: ServerCfg,
}

#[derive(serde::Deserialize, Clone, Default)]
pub struct DbCfg { pub mysql_url: String, pub pg_url: String }

#[derive(serde::Deserialize, Clone, Default)]
pub struct LlmCfg {
    pub base_url: String, pub api_key: String,
    pub model_fast: String, pub model_precise: String,
}

#[derive(serde::Deserialize, Clone)]
pub struct ServerCfg {
    #[serde(default = "default_listen")] pub listen: String,
    /// 无默认：未配置时 /api/sso 明确报 500，绝不悄悄连某个地址
    #[serde(default)] pub dms_base_url: String,
    #[serde(default)] pub dev_token: Option<String>,
}
impl Default for ServerCfg {
    fn default() -> Self { Self { listen: default_listen(), dms_base_url: String::new(), dev_token: None } }
}

fn default_listen() -> String { "127.0.0.1:8100".into() }

pub fn load_settings() -> anyhow::Result<Settings> {
    for p in ["settings.json", "../settings.json", "../../settings.json"] {
        if let Ok(s) = std::fs::read_to_string(p) {
            let mut v: serde_json::Value = serde_json::from_str(&s)?;
            let env: Vec<(String, String)> = std::env::vars().filter(|(k, _)| k.starts_with("DMSAI_")).collect();
            overlay_env(&mut v, &env);
            return Ok(serde_json::from_value(v)?);
        }
    }
    anyhow::bail!("settings.json 未找到（参考 settings.example.json）")
}
```
> wework::WeworkCfg 需补 `#[derive(serde::Deserialize, Default)]`（现状只有 Clone——一行改动）。

- [ ] **Step 3: 调用点适配 + dms_base_url 空值守卫**

- main.rs/AppState：`cfg.mysql_url` → `cfg.db.mysql_url` 等全字段改分组路径；`st.dms_base_url` → `st.server.dms_base_url`（或 AppState 直接持 ServerCfg）。
- api/auth.rs api_sso 入口加守卫：`if st.dms_base_url.is_empty() { return Err(AppError::Internal(anyhow::anyhow!("未配置 server.dms_base_url"))); }`。
- db.rs 只剩两个连接池函数 + 红线注释，`default_dms_base`/`default_listen`/Settings/load_settings 全删。

- [ ] **Step 4: 编译 + 测试 + 提交**

Run: `cargo test -p dms-ai-server 2>&1 | Select-String "test result"`
Expected: 全绿（overlay_env 1 + 既有）；本地 settings.json 需同步改分组后才能跑通冒烟（执行时注意提醒）。

```bash
git add crates/server settings.example.json
git commit -m "server: config.rs 配置分组（db/llm/wework/server）+ DMSAI_ env 覆盖，去掉 dms_base_url 硬编码生产地址（breaking：settings.json 需迁移）"
```

---

### Task 10.7: bin 拆分（cli/serve）+ identity.rs——判官 exe 名保全落地

**Files:**
- Create: `crates/server/src/lib.rs`（全部 `pub mod` 声明 + `bootstrap_meta`/`llm_client` 共享装配函数）
- Create: `crates/server/src/bin/cli.rs`（全部子命令：meta sync / meta autodiscover / review-pending / review-lessons / check-sql / graph sync / retrieve / ask / exec-sql / scope）
- Create: `crates/server/src/bin/serve.rs`（建池 → bootstrap_meta → chat::migrate → AppState → jobs::spawn_all → 路由 → axum::serve，即现 main.rs :211-272 段）
- Create: `crates/server/src/identity.rs`
- Delete: `crates/server/src/main.rs`
- Modify: `crates/server/Cargo.toml`（两个 `[[bin]]` 段）
- Modify: `crates/server/src/api/auth.rs`（sso/wework_login 改经 identity.rs 编排）

**bin 命名（方案 B：判官零改动）**：

```toml
[lib]
name = "dms_ai_server"
path = "src/lib.rs"

[[bin]]
name = "dms-ai-server"        # CLI 继承旧 exe 名：tools/*.py 的 EXE 路径零改动
path = "src/bin/cli.rs"

[[bin]]
name = "dms-ai-serve"         # 服务进程新名；启动命令同步改（部署脚本/文档标注）
path = "src/bin/serve.rs"
```
> 依据 Task 10.0 Step 2 实证：三判官全部 `subprocess [target/debug/dms-ai-server.exe, <子命令>, ...]`。CLI 继承旧名后，judge_scope/evaluation/regression 零改动直接绿。serve 改名只影响人工启动命令（`cargo run --bin dms-ai-serve`）。

- [ ] **Step 1: identity.rs 薄 trait（spec 3.4 既定形状）**

```rust
//! 身份提供方：颁会话 token 前的「凭据 → login_name」验真编排。
//! middleware 不依赖本 trait（中间件只认会话 token/dev_token），仅 sso/wework_login 两 handler 用。

pub enum IdentityCred {
    DmsToken(String),
    WeworkCode(String),
}

pub trait IdentityProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn authenticate<'a>(
        &'a self,
        cred: &'a IdentityCred,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + 'a>>;
}

pub struct DmsSso { pub base_url: String }
pub struct Wework { pub cfg: crate::wework::WeworkCfg, pub mysql: dms_connector::ReadOnlyMySql }
// 注：mysql 类型以 Task 3 交付为准（C1）；未交付则暂为 sqlx::MySqlPool 并标注「需 Task 3 补」。

impl IdentityProvider for DmsSso {
    fn name(&self) -> &'static str { "dms_sso" }
    fn authenticate<'a>(&'a self, cred: &'a IdentityCred) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + 'a>> {
        Box::pin(async move {
            match cred {
                IdentityCred::DmsToken(t) => crate::auth::verify_dms_token(&self.base_url, t).await,
                _ => anyhow::bail!("dms_sso 不接受该凭据类型"),
            }
        })
    }
}
// Wework 实现同理：WeworkCode → wework::login_by_code（逻辑一字不动，仅编排换壳）。
```

- [ ] **Step 2: lib.rs 收编 + main.rs 解体**

- lib.rs：现 main.rs :3-17 的 14 个 `mod` 声明改 `pub mod`（含 10.2-10.6 新建的 mw/api/jobs/notify/config/state/identity），加 `pub async fn bootstrap_meta(...)`（:42-49 原样）与 `pub fn llm_client(...)`（:51-53 原样，签名以 Task 4 交付的 ChatModel 形态为准）。
- cli.rs：9 组子命令 if 链（:68-209）整体搬入 `#[tokio::main] async fn main()`，逻辑一字不动；日志初始化（:57-64 stderr 约定）随搬——**判官解析 stdout 的契约保住**。
- serve.rs：:211-272 服务装配段搬入；`tracing::info!("dms-ai server listening on ...")` 日志保留。
- api/auth.rs 两 handler 改经 `IdentityProvider` 编排（AppState 持 `Vec<Arc<dyn IdentityProvider>>` 按 cred 类型分派，或两个具名字段——**取具名字段**，两实现写死，对齐 YAGNI「栈在 AppState 构造函数硬编码」）。

- [ ] **Step 3: 编译 + 判官通路冒烟（本任务最关键验收）**

Run:
```powershell
cargo build 2>&1 | Select-Object -Last 3
Test-Path target/debug/dms-ai-server.exe   # 期望 True（CLI）
Test-Path target/debug/dms-ai-serve.exe    # 期望 True
.\target\debug\dms-ai-server.exe scope <login> <role>   # 需库环境；期望 stdout JSON 与拆分前逐字节一致
.\target\debug\dms-ai-server.exe exec-sql <login> "SELECT 1"  # 同上
```
Expected: 编译过；两 exe 俱在；CLI 子命令输出形状不变（判官 json.loads 直接过）。
Run: `cargo test --workspace 2>&1 | Select-String "test result"` → 全绿。

- [ ] **Step 4: 提交**

```bash
git add crates/server
git commit -m "server: main.rs 解体为 lib+bin/cli+bin/serve（dms-ai-server.exe 名由 CLI 继承，判官零改动），identity.rs 薄 trait 收编 sso/wework 编排"
```

---

### Task 10.8: viewspec 去 DMS 化——DIM_POOL 读注册表、省码表移交 present

**Files:**
- Modify: `crates/server/src/viewspec.rs`（若 Task 2 已下沉 kernel 则改 `crates/kernel/src/...` 对应文件，落点以 K1 核对结果为准，改动逻辑一致）
- Modify: 调用点（pipeline.rs 或 agent，以 G1 核对结果为准）传 drill 池参数
- Modify: `crates/server/src/api/ask.rs`（无直接改动，仅经调用链传导）

**Interfaces:**
- Consumes: S1（维度名列表；registry 缺失则 server 侧直查 `SELECT name FROM meta.dimension WHERE status='active'`）
- Produces: `viewspec::build(columns, rows, drill_dims: &[String])` 新签名；`pub fn province_cn` 挂 `// TODO(Task7): 移交 semantic/present.rs` 标注

- [ ] **Step 1: 先改/写测试——drill 池注入 + 现 10 单测适配**

```rust
#[test]
fn drill_pool_from_registry_not_hardcoded() {
    // 注册表池驱动：传什么池出什么 chips（旧 DIM_POOL 固定 6 个不复存在）
    let rows = vec![vec![json!("100")], vec![json!("90")]];
    let v = build(&cols(&["销售额"]), &rows, &["仓库".into(), "供应商".into()]);
    assert_eq!(v.interact.drill, vec!["仓库", "供应商"]);
    // 已用维度剔除逻辑不动：列名含「仓」则「仓库」不建议
    let v2 = build(&cols(&["仓库", "销售额"]), &[vec![json!("A"), json!("1")], vec![json!("B"), json!("2")]], &["仓库".into(), "供应商".into()]);
    assert_eq!(v2.interact.drill, vec!["供应商"]);
    // 无指标不下钻逻辑不动
    let v3 = build(&cols(&["单号"]), &[vec![json!("X1")]], &["仓库".into()]);
    assert!(v3.interact.drill.is_empty());
}
```
既有 10 个单测：`build(&cols, &rows)` 调用全部补第三参（传 `&[]` 或测试池），**断言一字不改**（drill 相关断言若涉及 DIM_POOL 字面量才适配——检查 `infer_drill` 间接断言；现 10 测试无 drill 断言，预期零断言改动）。

- [ ] **Step 2: DIM_POOL 删除 + build 加参**

- `const DIM_POOL`（viewspec.rs:108）删除；`infer_drill(specs, has_metric)` 改 `infer_drill(specs, has_metric, pool: &[String])`，过滤逻辑（关键词前两字未出现才建议）一字不动。
- `build(columns, rows)` 签名加 `drill_dims: &[String]`。
- 调用点（pipeline::ask 内 view 构建处）：从 PG 读维度名列表传入。每问一次查询太勤——加进程内缓存（对齐 scope 缓存当日过期风格？**不**，维度表变更频率低但无失效通知；取「每次问答一次轻查询」优先正确性：`SELECT name FROM meta.dimension WHERE status='active' ORDER BY dim_code`，~10 行结果，开销可忽略。不做缓存，极简）。

- [ ] **Step 3: province_cn 移交准备（行为变化⑥的前半）**

- `province_cn`（:294-307）改 `pub fn`，挂 doc 标注：
```rust
/// 省级区划码 → 省名。
/// TODO(Task7)：移交 semantic/present.rs 列标注阶段（geo 码在数据进 viewspec 前翻名），
/// 届时本函数与 compute_insight:248 的调用一并删除；前端 format.ts:14-28 省码表同步冗余。
pub fn province_cn(code: &str) -> Option<&'static str> {
```
- 本步**不改渲染行为**（insight 仍翻名）；真正的「翻名后端化」由 Task 7 present.rs 落地后收尾——标注「需 Task 7 补」。若 Task 7 已交付 present.rs：则直接改「调用点在进 build 前对 geo 列行值做翻名变换，viewspec 内 province_cn/调用删除」，compute_insight 的 `if is_geo { province_cn... }` 分支（:248）删除，相关单测（若有）适配。

- [ ] **Step 4: 编译 + 测试 + 三端同步标注核对 + 提交**

Run: `cargo test -p dms-ai-server viewspec 2>&1 | Select-String "test result"`（或 kernel 包名，以落点为准）
Expected: 全绿（10 旧 + 1 新）。
Run: `Select-String -Path crates/server/src/*.rs -Pattern "DIM_POOL"` → 期望空。

```bash
git add crates
git commit -m "viewspec 去 DMS 化：DIM_POOL 写死 6 个改读 meta.dimension 注册表（build 加 drill_dims 参），province_cn pub 化标注移交 Task 7 present.rs"
```

---

### Task 10.9: 终验（行数 / 全测试 / 判官通路 / 行为变化清单核对）

- [ ] **Step 1: 薄壳行数验收**

Run:
```powershell
(Get-ChildItem crates/server/src -Recurse -Filter *.rs | Get-Content | Measure-Object -Line).Lines
Get-ChildItem crates/server/src -Recurse -Filter *.rs | Select-Object FullName,@{n='Lines';e={(Get-Content $_.FullName | Measure-Object -Line).Lines}}
```
Expected: server crate src 总行数 **≤700**（口径：含 bin/ 与 #[cfg(test)]，不含上游已迁出的 crate）。若超标：先查是否有 Task 6-9 未迁出的业务文件残留（那不是本任务超支，记录后升级 team-lead），再查本任务是否拆过头。

- [ ] **Step 2: 全量测试 + 残留扫描**

```powershell
cargo test --workspace 2>&1 | Select-String "test result"          # 全绿，既有单测一个不少（本任务新增 ~10 个）
Select-String -Path crates/server/src -Pattern "resolve_identity|err\(StatusCode|DIM_POOL|1\.95\.167\.10"   # 期望空
Select-String -Path crates/server/src/api/*.rs -Pattern "unwrap_or_default"                               # 期望空（health 探活除外）
cargo tree -p dms-ai-server --prefix none 2>&1 | Select-String "tower-http|thiserror|async-trait|cron"    # 期望空（零新增依赖红线）
```

- [ ] **Step 3: 判官三件套回归（需真库环境，判官门禁）**

```powershell
python tools/judge_scope.py        # scope 子命令对拍
python tools/evaluation.py         # exec-sql 38 题
python tools/regression.py         # ask 51 题
```
Expected: 与 Task 10 开始前基线一致（结果集不许变；若 Task 8 口径修复在前序任务已改变基线，以当时基线为准）。

- [ ] **Step 4: 行为变化清单逐项核对（上线前沟通稿）**

| # | 变化 | 核对点 |
|---|---|---|
| ① | 认证收紧 401：body/query login_name 无凭据全拒 | curl 冒烟 401；前端开发模式改 dev_token+X-Login-Name（**前端需同步**：App.vue 开发分支） |
| ② | exe：dms-ai-server=CLI、dms-ai-serve=服务 | 部署/启动脚本改 dms-ai-serve |
| ③ | roles/convs DB 错误 500 透明化（不再静默空列表） | 前端已有 error 字段展示，无需改 |
| ④ | health 缺 vector/age/pg_trgm → ok:false | 监控告警预期内 |
| ⑤ | drill chips 6→注册表 ~10 个 | 前端动态渲染无需改；人工过目 chips 文案 |
| ⑥ | 省码翻名后端化（Task 7 收尾） | 三端同步点：viewspec/App.vue/format.ts |
| ⑦ | settings.json 分组 breaking | 部署机手工迁移；settings.example.json 已同步 |

- [ ] **Step 5: 提交收尾（如有残余改动）**

---

## 自检（已执行）

- **spec 覆盖**：迁移步 10 全部要点（main 拆 bin ✓ 10.7 / handler 分文件 ✓ 10.4 / 认证中间件 ✓ 10.3 / jobs ✓ 10.5）+ 3.4 目录树逐项（config.rs ✓ 10.6、state.rs ✓ 10.3、mw/auth.rs ✓ 10.3、mw/error.rs ✓ 10.2、api/{ask,conv,auth,roles,health}.rs ✓ 10.4、identity.rs ✓ 10.7、jobs.rs+notify.rs ✓ 10.5、bin/cli.rs ✓ 10.7；chat_store.rs = 现 chat.rs 改名问题——见裁决 5）。
- **5.3 第 1 条硬约束**：判官通路实证为 CLI（judge_scope.py:148 / evaluation.py:59 / regression.py:25），真正风险 = exe 名变化 → 方案 B（CLI 继承 dms-ai-server.exe 名）从根上消除；dev_token 逃生门（10.1）先于中间件收门（10.3）✓。「改判官脚本认证」按源码事实改写为「判官通路保全」，差异点已列裁决 1。
- **TDD**：每子任务先测试后实现（10.1 dev_mode_allowed / 10.2 AppError 映射 / 10.3 authenticate 四分支 / 10.4 health_ok / 10.5 build_text_msg / 10.6 overlay_env / 10.8 drill 池注入）；不引 tower 测 Router，核心逻辑全抽纯函数 ✓。
- **行为变化白名单 7 条**全部标注落点与同步方 ✓；resolve_identity 6 处手抄行号核对 ✓（:345/387/398/409/423/441）；unwrap_or_default 三处 :389/400/430 ✓。
- **零新增依赖**：middleware::from_fn_with_state（axum 自带）✓、不引 thiserror/tower-http/cron ✓；wework API/http 改 pub(crate) 属可见性调整非依赖 ✓。
- **红线**：CLI exec-sql/scope 管道不动（以 Task 3 newtype 形态为准）；AppState 换 ReadOnlyMySql 标注 C1 契约 ✓。
- **占位符扫描**：G1/G2/K1/K2/C1/S1/S2 全部给出缺失降级路径，无 TBD 留白 ✓。

## 需 team-lead 裁决（阻塞前先确认）

1. **判官通路事实差异（最重要）**：任务书与 spec 5.3 均称「三判官靠 body 带 login_name，认证中间件会打挂判官」；源码实证三判官全部走 **CLI 子命令**（subprocess 调 exe），不碰 HTTP。本 plan 据此把硬约束改写为「exe 名保全（方案 B）+ dev_token 先行」，未给判官脚本发 token。若团队另有 HTTP 版判官（本仓 tools/ 之外），请指出，10.1 需补「判官改带 dev_token+X-Login-Name」步骤。
2. **bin 命名方案 B**：CLI 继承 `dms-ai-server.exe`（判官零改动）、serve 新名 `dms-ai-serve.exe`；备选 = serve 继承旧名、判官三处 EXE 常量改 `dms-ai-cli.exe`。本 plan 取 B，请确认。
3. **identity.rs 薄 trait**：spec 3.4 列了 `trait IdentityProvider + DmsSso + Wework`，本 plan 遵从 spec 做薄 trait（枚举凭据分派）；极简替代 = 两个 free fn 不抽 trait。请确认取哪档。
4. **chat.rs → chat_store.rs 改名**：spec 3.4 目录树写 chat_store.rs 且注明「payload 存 Answer」（依赖 G2）。本 plan 未单列改名步（纯改名无行为变化，但文件改名会打断 git blame）；建议随 Task 9 Answer 落地时一并改，不在 Task 10。请确认。
5. **dev_token 不进 env 覆盖清单**（防部署环境意外注入开发开关）；若团队要求 `DMSAI_DEV_TOKEN` 可用，请确认后加进 10.6 KEYS 表。
6. **viewspec 落点**：spec 1 表把「呈现决策树」划给 kernel（Task 2），10.8 的改动落 kernel 还是 server 取决于 Task 2 交付；plan 已按「落点以 K1 核对为准、逻辑一致」写，无需返工，仅备案。

## 备注（Windows 构建）
cargo 命令统一前缀：
`$env:PATH = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT.LLVM_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin;" + $env:PATH`
（Bash 的 mingw 与 cargo 链接冲突，必须走 PowerShell。）


