//! 【页面编辑配置】DMS 权限连接、分析目标与 LLM 的前端可写面。
//!
//! 🔴 红线形状（与手写 settings.json **完全同一条**）：凭据**只住 settings 文件** ——
//! 不落 PG（kv 只存名字）、不进日志（下面每个 info 只写名字）、不进响应
//! （catalog 只给 `mask_dsn` 与 `key_ready` 布尔）。页面 POST 体带一次明文，
//! 与手写文件是同一个信任面（只认服务端会话 token，并回查 DMS 的 admin 管理员状态）。
//!
//! 写文件两条硬约束：
//! ① **原地单次写**（O_TRUNC 不 rename）：单文件 bind mount 的 inode 被钉在挂载点上，
//!    rename 会写到容器层、宿主机看不见（配置就「丢了」—— 重启回旧值）。
//! ② **完整校验后再写**：patch 后整份 JSON 过一遍 `serde_json::from_value::<Settings>`
//!    （`deny_unknown_fields` + 类型全检）—— 写出一个启动不了的文件比不答应更坏。
//! 凭据副本一律禁止：明文 DSN/API key 只能存在正式 settings 文件，不能生成旁路文件。
//! 【D1】落盘态是 `enc:v1:` AES-GCM 密文（字段清单见 db.rs）：`prepare_settings` 回读校验后
//! 解密进内存、序列化前加密落盘 —— 页面与运行时的信任面与明文时代一致，只是 at-rest 不再是明文。
//!
//! 内存热更新：改完同步 `AppState.cfg`（RwLock），热切换路径（db-target / llm-provider）
//! 立即用新目录 —— **保存即生效，无需重启**（与手写文件 + 重启的语义一致，只是更快）。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

use crate::admin_api::{err, ApiErr, ApiRes};
use crate::AppState;

// 指引文案常量是设置面唯一口径：admin_api 的 persist_db_target 等路径也引用（防两处漂移）。
pub(crate) const DB_CONNECT_GUIDANCE: &str =
    "数据库连接失败。请检查类型、地址、端口、数据库名、账号密码及只读权限后重试";
pub(crate) const DB_SWITCH_GUIDANCE: &str =
    "数据库切换未生效。请先测试连通性，并确认目标账号具备只读查询权限";
pub(crate) const DB_SECRET_GUIDANCE: &str =
    "无法保留原数据库凭据，请重新填写账号和密码后重试";
pub(crate) const LLM_CONNECT_GUIDANCE: &str =
    "模型连接失败。请检查 URL、模型名称、Key 及 OpenAI 兼容接口后重试";
pub(crate) const LLM_CONFIG_GUIDANCE: &str =
    "模型配置未生效。请检查供应商、模型名称、Key、思考参数及多模态能力配置";
pub(crate) const LLM_THINKING_GUIDANCE: &str =
    "思考级别与该供应商不兼容，请改用“关”或选择供应商支持的档位";
pub(crate) const SETTINGS_WRITE_GUIDANCE: &str =
    "配置保存失败。请检查正式 settings 文件是否可写，修正后重试";

/// DMS 身份/权限连接池大小（热换与回滚同一口径）。
const AUTH_POOL_SIZE: u32 = 5;

/// 改文件 + 同步内存 cfg（本文件全部端点的共用落地）。
/// `patch` 负责改 `serde_json::Value` 与 `Settings`（两个是同一个 JSON 的两种形态 ——
/// 先 patch Value、回读成 Settings 校验，校验过的那份再进锁，**两个永远一致**）。
struct PreparedSettings {
    path: String,
    raw: String,
    out: String,
    checked: crate::db::Settings,
}

fn prepare_settings(
    patch: impl FnOnce(&mut serde_json::Value) -> Result<(), String>,
) -> Result<PreparedSettings, String> {
    let (path, raw) = crate::db::find_settings_path().ok_or("settings.json 未找到")?;
    let mut v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "正式 settings 文件不是合法 JSON".to_string())?;
    patch(&mut v)?;
    // 完整校验：deny_unknown_fields + 类型全检 —— 写坏的文件不许落盘
    // 借用反序列化做校验（与 from_value 同一拒绝口径），不深克隆整份 Value
    let checked: crate::db::Settings =
        <crate::db::Settings as serde::Deserialize>::deserialize(&v)
            .map_err(|_| "配置字段校验失败（未落盘）".to_string())?;
    checked
        .validate_named_catalogs()
        .map_err(|_| "配置目录存在仅大小写不同的重复名称（未落盘）".to_string())?;
    // 【Y3】RRF 权重闸：与启动加载（db::load_settings）同一拒绝口径 —— 负值/NaN/Inf 不许落盘
    checked
        .kb_rrf_weights
        .validate()
        .map_err(|e| format!("kb_rrf_weights 无效（未落盘）：{e}"))?;
    // 【D1】内存态解密：未触碰的密文字段换回明文（patch 进来的新值本就是明文，前缀闸放行）。
    // 解不开 = 钥匙变了/密文损坏 → 不落盘，响亮失败（不静默把密文带进运行时）。
    let checked = checked
        .decrypted()
        .map_err(|_| "已有凭据解密失败（DMS_SECRET_KEY 是否变更？未落盘）".to_string())?;
    // 【D1】落盘态加密：明文敏感字段（含本次 patch 写入的新凭据）加密后才许进文件；
    // 已是密文的字段原样（幂等 —— 与启动迁移同一份字段清单）。
    crate::db::encrypt_sensitive_fields(&mut v)
        .map_err(|_| "敏感字段加密失败（未落盘）".to_string())?;
    let out = serde_json::to_string_pretty(&v)
        .map_err(|_| "配置序列化失败（未落盘）".to_string())?;
    Ok(PreparedSettings { path, raw, out, checked })
}

fn persist_settings(st: &AppState, prepared: &PreparedSettings) -> Result<(), String> {
    // 正式挂载文件原地单次写入；不生成任何含凭据的副本。
    if std::fs::write(&prepared.path, &prepared.out).is_err() {
        if std::fs::write(&prepared.path, &prepared.raw).is_err() {
            tracing::error!(reason = "settings_file_restore_failed", path = %prepared.path, "配置写入失败且正式文件原内容恢复失败");
        }
        return Err("正式 settings 文件写入失败".to_string());
    }
    // 内存热更新（校验过的那份 —— 与落盘内容逐字节同源）；
    // 锁中毒不影响此处语义（整体覆盖写，不读中毒值），取回守卫继续写
    *st.cfg.write().unwrap_or_else(std::sync::PoisonError::into_inner) = prepared.checked.clone();
    Ok(())
}

fn patch_settings(
    st: &AppState,
    patch: impl FnOnce(&mut serde_json::Value) -> Result<(), String>,
) -> Result<(), String> {
    let prepared = prepare_settings(patch)?;
    persist_settings(st, &prepared)
}

fn valid_name(name: &str) -> bool {
    // 单遍同时计长与校验字符。
    // `is_alphanumeric` 收 Unicode —— 中文也过！目标名要进 URL 与 kv，只收 ASCII
    let mut len = 0usize;
    !name.is_empty()
        && name.chars().all(|c| {
            len += 1;
            len <= 32 && (c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        })
}

fn matching_key<V>(map: &std::collections::HashMap<String, V>, name: &str) -> Option<String> {
    map.keys().find(|key| key.eq_ignore_ascii_case(name)).cloned()
}

/// key 形状闸：长度 8..4096 且不含控制字符（put_llm_key 与 put_llm_provider 同一口径）。
fn valid_key(key: &str) -> bool {
    key.len() >= 8 && key.len() <= 4096 && !key.chars().any(char::is_control)
}

/// 预设思考档 extra_body 的解析缓存：预设是编译期静态表，JSON 只解析一次，
/// catalog / llm_config 按供应商逐个调用时不再重复 `serde_json::from_str`。
/// 元素形态：（预设 base_url 去尾斜杠, 级别, 解析后的 extra_body）。
fn preset_thinking_bodies(
) -> &'static [(&'static str, &'static str, serde_json::Map<String, serde_json::Value>)] {
    static CACHE: std::sync::OnceLock<
        Vec<(&'static str, &'static str, serde_json::Map<String, serde_json::Value>)>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        crate::db::llm_presets()
            .iter()
            .flat_map(|(_, preset)| {
                [
                    ("off", preset.thinking_off),
                    ("low", preset.thinking_low),
                    ("high", preset.thinking_high),
                ]
                .into_iter()
                .filter_map(|(level, raw)| {
                    raw.and_then(|raw| serde_json::from_str(raw).ok())
                        .map(|body| (preset.base_url.trim_end_matches('/'), level, body))
                })
                .collect::<Vec<_>>()
            })
            .collect()
    })
}

pub(crate) fn configured_thinking_level(
    base_url: &str,
    extra: &serde_json::Map<String, serde_json::Value>,
) -> &'static str {
    if extra.is_empty() {
        return "none";
    }
    let base_url = base_url.trim_end_matches('/');
    for &(preset_url, level, ref body) in preset_thinking_bodies() {
        if preset_url == base_url && body == extra {
            return level;
        }
    }
    // 页面不懂的手写 extra_body 必须原样保留，不能在一次普通“修改”中静默清空。
    "keep"
}

fn runtime_configs(
    st: &AppState,
    cfg: &crate::db::Settings,
) -> Result<(crate::llm::Conf, Option<crate::llm::Conf>), String> {
    let primary = crate::db::resolve_provider(&st.llm.primary_provider(), cfg)
        .map_err(|_| "主模型配置解析失败".to_string())?;
    let fallback = crate::db::resolve_fallback_vision(cfg)
        .map_err(|_| "备用多模态配置解析失败".to_string())?
        .map(|(_, conf)| conf);
    Ok((primary, fallback))
}

/// LLM 设置的统一提交：运行时写锁覆盖正式文件持久化窗口；写失败时在释放锁前恢复
/// 完整旧快照，任何并发调用都不会观察到未持久化的临时配置。
fn commit_llm_settings(
    st: &AppState,
    patch: impl FnOnce(&mut serde_json::Value) -> Result<(), String>,
) -> Result<(), ApiErr> {
    let prepared = prepare_settings(patch).map_err(|e| {
        // 请求内容导致的校验类失败是 400 且带具体原因；
        // 文件读写/加解密/序列化等服务端故障仍是 500 笼统指引（细节不进响应）
        let validation =
            e.contains("校验失败") || e.contains("重复名称") || e.contains("kb_rrf_weights 无效");
        if validation {
            err(StatusCode::BAD_REQUEST, e)
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, SETTINGS_WRITE_GUIDANCE)
        }
    })?;
    let configs = runtime_configs(st, &prepared.checked)
        .map_err(|_| err(StatusCode::BAD_REQUEST, LLM_CONFIG_GUIDANCE))?;
    crate::llm::validate_conf(&configs.0, false)
        .map_err(|_| err(StatusCode::BAD_REQUEST, LLM_CONFIG_GUIDANCE))?;
    if let Some(fallback) = configs.1.as_ref() {
        crate::llm::validate_conf(fallback, true)
            .map_err(|_| err(StatusCode::BAD_REQUEST, LLM_CONFIG_GUIDANCE))?;
    }
    st.llm
        .commit_runtime_configs(configs.0, configs.1, || {
            persist_settings(st, &prepared).map_err(anyhow::Error::msg)
        })
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, SETTINGS_WRITE_GUIDANCE))
}

/// GET/DELETE 的身份字段（`login_name`/`role_code` 从 query 给 —— DELETE 不带 body）。
/// 注意：这两个字段只为与 POST 端点的签名对称而收，**校验只认 Bearer 会话 token** ——
/// `settings_admin_only` 完全忽略它们（见 admin_api.rs），传与不传都不影响鉴权结果。
#[derive(serde::Deserialize, Default)]
pub struct IdentQuery {
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `GET /api/admin/settings-catalog` —— 可编辑目录（**永不含明文**）。
pub async fn catalog(State(st): State<Arc<AppState>>, h: HeaderMap, Query(q): Query<IdentQuery>) -> ApiRes {
    crate::admin_api::settings_admin_only(&st, &h, (&q.login_name, &q.role_code)).await?;
    let cfg = st.cfg();
    let mut targets = vec![serde_json::json!({
        "name": "dms",
        "host": crate::db::mask_dsn(&cfg.mysql_url),
        "builtin": true,
        "protected": true,
        "query_target": false,
        "purpose": "DMS 身份、角色与数据权限源",
    })];
    let selectable_targets = crate::db::db_targets(&cfg);
    let mut configured_targets: Vec<_> = cfg.mysql_targets.iter().collect();
    configured_targets.sort_by(|a, b| a.0.cmp(b.0));
    targets.extend(configured_targets
        .into_iter()
        .filter(|(name, _)| !name.eq_ignore_ascii_case("dms"))
        .map(|(name, target)| {
            let capability = target.capability();
            let selectable = selectable_targets.iter().any(|(candidate, _)| candidate.eq_ignore_ascii_case(name));
            serde_json::json!({
                "name": name,
                "host": crate::db::mask_dsn(target.url()),
                "type": target_type_name(capability),
                "builtin": false,
                "protected": false,
                "query_target": true,
                "selectable": selectable,
                "purpose": if !selectable {
                    "配置与 DMS 权限源冲突，仅可修改或删除"
                } else { match capability {
                    dms_connector::mysql::MysqlCapability::Warehouse => "只读分析查询源",
                    dms_connector::mysql::MysqlCapability::ProductionLookup
                    | dms_connector::mysql::MysqlCapability::IdentityPermission => "生产业务轻点查",
                }},
            })
        })
    );
    let file_provider = crate::db::file_provider_name(&cfg);
    let mut keys: Vec<String> = cfg.llm_keys.keys().cloned().collect();
    if !cfg.llm_api_key.is_empty() && !keys.iter().any(|name| name.eq_ignore_ascii_case(&file_provider)) {
        keys.push(file_provider.clone());
    }
    keys.sort();
    let keys: Vec<serde_json::Value> = keys
        .iter()
        .map(|n| serde_json::json!({
            "name": n,
            "key_ready": true,
            // 旧式 llm_api_key 仍会在文件供应商上兜底；只删 llm_keys 会造成重启后“复活”。
            "protected": n.eq_ignore_ascii_case(&file_provider) && !cfg.llm_api_key.is_empty(),
        }))
        .collect();
    // 预设目录（页面下拉用 —— url/模型/思考档/多模态全在这，key 才要手填）
    let presets: Vec<serde_json::Value> = crate::db::llm_presets()
        .iter()
        .map(|(n, p)| {
            let levels: Vec<&str> = [
                p.thinking_off.map(|_| "off"),
                p.thinking_low.map(|_| "low"),
                p.thinking_high.map(|_| "high"),
            ]
            .into_iter()
            .flatten()
            .collect();
            serde_json::json!({
                "name": n, "label": p.label, "base_url": p.base_url,
                "model_fast": p.model_fast, "model_precise": p.model_precise,
                "thinking_levels": levels,
                "vision": p.vision,
            })
        })
        .collect();
    // 自定义供应商（页面/手工加的）
    let primary_provider = st.llm.primary_provider();
    let fallback_provider = st.llm.fallback_vision_provider();
    let mut custom: Vec<serde_json::Value> = cfg
        .llm_providers
        .iter()
        .map(|(n, c)| {
            let builtin = crate::db::provider_catalog().iter().any(|(name, _)| name.eq_ignore_ascii_case(n));
            let active = primary_provider.eq_ignore_ascii_case(n);
            let fallback_used = fallback_provider.as_deref().is_some_and(|name| name.eq_ignore_ascii_case(n));
            // 内建供应商本体不能删除；但同名自定义条目只是“覆盖配置”，未被运行时占用时
            // 应允许删除并恢复内建预设。否则页面显示删除按钮，后端却永远 409。
            let protected = active
                || fallback_used
                // 纯自定义文件供应商删掉目录项后仍会由旧式 llm_* 字段“复活”，所以受保护；
                // 内建同名条目只是覆盖层，删除后由文件值/内建预设接管，允许恢复。
                || (file_provider.eq_ignore_ascii_case(n) && !builtin);
            serde_json::json!({
                "name": n, "base_url": crate::db::public_service_url(&c.base_url),
                "model_fast": c.model_fast, "model_precise": c.model_precise,
                "vision": c.vision,
                "thinking": configured_thinking_level(&c.base_url, &c.extra_body),
                "key_ready": crate::db::provider_key_ready(&cfg, n),
                "builtin": builtin, "active": active, "fallback_used": fallback_used,
                "protected": protected, "deletable": !protected,
            })
        })
        .collect();
    // 与 candidate_names 同一口径按小写排序：两个列表的排序规则保持一致
    custom.sort_by_key(|v| v["name"].as_str().unwrap_or_default().to_ascii_lowercase());
    let mut candidate_names: Vec<String> = crate::db::provider_catalog()
        .iter()
        .map(|(name, _)| (*name).to_string())
        .chain(cfg.llm_providers.keys().cloned())
        .collect();
    candidate_names.sort_by_key(|name| name.to_ascii_lowercase());
    candidate_names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    let vision_candidates: Vec<serde_json::Value> = candidate_names
        .into_iter()
        .map(|name| {
            // 自定义同名条目完整覆盖内建能力；`vision: null` 是明确关闭，不能再回退
            // 到内建目录，否则页面会把实际无视觉能力的配置错误标成可选。
            let vision = match cfg
                .llm_providers
                .iter()
                .find(|(provider, _)| provider.eq_ignore_ascii_case(&name))
            {
                Some((_, provider)) => provider.vision.clone(),
                None => {
                    crate::db::provider_catalog()
                        .iter()
                        .find(|(provider, _)| provider.eq_ignore_ascii_case(&name))
                        .and_then(|(_, p)| p.vision.map(str::to_string))
                }
            };
            let supports_vision = vision.is_some();
            let key_ready = crate::db::provider_key_ready(&cfg, &name);
            serde_json::json!({
                "name": name,
                "supports_vision": supports_vision,
                "vision_model": vision,
                "key_ready": key_ready,
                "selectable": supports_vision && key_ready,
            })
        })
        .collect();
    let effective_vision = st.llm.vision_capability();
    Ok(Json(serde_json::json!({
        "mysql_targets": targets,
        "llm_keys": keys,
        "llm_presets": presets,
        "llm_providers": custom,
        "fallback_vision_provider": cfg.fallback_vision_provider,
        // 【Y3】RRF 四路辅助召回权重的当前生效值（设置页展示初值；改它走
        // `put_kb_rrf_weights` —— 路由登记形态见该处理器文档，编排方统一接 main.rs）。
        "kb_rrf_weights": {
            "metadata": cfg.kb_rrf_weights.metadata,
            "relation": cfg.kb_rrf_weights.relation,
            "kg": cfg.kb_rrf_weights.kg,
            "ext_kb": cfg.kb_rrf_weights.ext_kb,
        },
        "vision_candidates": vision_candidates,
        "effective_vision": effective_vision.map(|v| serde_json::json!({
            "provider": v.provider, "model": v.model, "fallback": v.fallback,
        })),
        "note": "明文只在 settings.json：这里只给脱敏 host 与「已配置」布尔",
    })))
}

#[derive(serde::Deserialize)]
pub struct MysqlTargetReq {
    name: String,
    dsn: String,
    #[serde(default)]
    r#type: String,
    /// 密码留空 = 保留原凭据（修改场景）：新 DSN 的 userinfo 用旧 DSN 的替换。
    /// **凭据全程不出服务端** —— 页面只见脱敏 host，编辑只改地址/库名/账号以外的部分。
    #[serde(default)]
    keep_secret: bool,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// 旧 DSN 的 userinfo 拼进新 DSN（keep_secret）。两边都过了形状闸才到这里。
/// `mysql://userinfo@host/db` —— 没有 userinfo 的旧 DSN 不许 keep（空凭据不是凭据）。
fn splice_userinfo(new_dsn: &str, old_dsn: &str) -> Result<String, String> {
    let (nu, ou) = (new_dsn.strip_prefix("mysql://").ok_or("新 DSN 形状不对")?,
                    old_dsn.strip_prefix("mysql://").ok_or("旧 DSN 形状不对")?);
    let (_, after_u) = ou.rsplit_once('@').ok_or("旧 DSN 没有账号段")?;
    let userinfo = &ou[..ou.len() - after_u.len() - 1];
    if userinfo.is_empty() {
        return Err("旧 DSN 没有账号段 —— 密码留空保留不了（这次请把账号密码填上）".into());
    }
    let host_part = nu.rsplit_once('@').map(|(_, h)| h).unwrap_or(nu);
    Ok(format!("mysql://{userinfo}@{host_part}"))
}

fn capability_from_type(
    kind: &str,
) -> Result<dms_connector::mysql::MysqlCapability, ApiErr> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "doris" | "warehouse" => Ok(dms_connector::mysql::MysqlCapability::Warehouse),
        // `mysql` is kept only for old clients. It is the restrictive production mode,
        // never an analytical capability.
        "mysql" | "production_lookup" => {
            Ok(dms_connector::mysql::MysqlCapability::ProductionLookup)
        }
        _ => Err(err(
            StatusCode::BAD_REQUEST,
            "数据库能力类型只允许 warehouse（Doris 分析）或 production_lookup（生产轻点查）",
        )),
    }
}

fn target_type_name(capability: dms_connector::mysql::MysqlCapability) -> &'static str {
    match capability {
        dms_connector::mysql::MysqlCapability::Warehouse => "warehouse",
        dms_connector::mysql::MysqlCapability::ProductionLookup
        | dms_connector::mysql::MysqlCapability::IdentityPermission => "production_lookup",
    }
}

/// `POST /api/admin/settings/mysql-target` —— 修改 DMS 权限连接，或新增/覆盖查询目标。
/// `name == "dms"` 只改 `mysql_url` 与 `auth_mysql`；查询池永不跟随。
pub async fn put_mysql_target(
    State(st): State<Arc<AppState>>, h: HeaderMap, Json(req): Json<MysqlTargetReq>,
) -> ApiRes {
    crate::admin_api::settings_admin_only(&st, &h, (&req.login_name, &req.role_code)).await?;
    let _settings_write = st.settings_write.lock().await;
    // 整函数复用同一份快照（写锁已挡住其他 settings 写端点的并发变更），
    // 不为读几个字段反复克隆整份 Settings（含解密后的明文 DSN/key）
    let cfg = st.cfg();
    let requested_name = req.name.trim().to_string();
    if !valid_name(&requested_name) {
        return Err(err(StatusCode::BAD_REQUEST, "名字只能含 ASCII 字母数字._-（≤32）"));
    }
    let name = if requested_name.eq_ignore_ascii_case("dms") {
        "dms".to_string()
    } else {
        matching_key(&cfg.mysql_targets, &requested_name).unwrap_or(requested_name)
    };
    let mut dsn = req.dsn.trim().to_string();
    // 形状闸；保存前统一做一次只读连通性验证，避免目录出现不可用目标。
    let Some(rest) = dsn.strip_prefix("mysql://") else {
        return Err(err(StatusCode::BAD_REQUEST, "DSN 必须 mysql:// 开头"));
    };
    if !rest.contains('@') || crate::db::mask_dsn(&dsn).is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "DSN 形状不对（需要 用户:口令@主机:端口/库名）"));
    }
    if name.eq_ignore_ascii_case("dms") {
        let old_dsn = cfg.mysql_url.clone();
        if req.keep_secret {
            dsn = splice_userinfo(&dsn, &old_dsn)
                .map_err(|_| err(StatusCode::BAD_REQUEST, DB_SECRET_GUIDANCE))?;
        }
        if cfg.mysql_targets.iter().any(|(target_name, target)| {
            !target_name.eq_ignore_ascii_case("dms")
                && crate::db::same_db_endpoint(&dsn, target.url())
                && !target.is_explicit_production_lookup()
        }) {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "DMS 权限库只能与显式 production_lookup 目标复用端点，不能与数仓目标混用",
            ));
        }
        // DMS 权限连接：先验、热换、再写正式配置；写失败时恢复旧权限池。
        dms_connector::mysql::test_pool(
            &dms_kernel::DsId::new("probe"),
            &dsn,
            dms_connector::mysql::MysqlCapability::IdentityPermission,
        )
            .await
            .map_err(|_| err(StatusCode::BAD_REQUEST, DB_CONNECT_GUIDANCE))?;
        let candidate = dms_connector::mysql::ReadOnlyMySql::connect(
            dms_kernel::DsId::new("dms-settings-probe"),
            &dsn,
            1,
            dms_semantic::registry::SENSITIVE_COLS,
            dms_connector::mysql::MysqlCapability::IdentityPermission,
        )
        .await
        .map_err(|_| err(StatusCode::BAD_REQUEST, DB_CONNECT_GUIDANCE))?;
        let administrator: Option<(Option<i8>,)> = candidate
            .fixed(
                "SELECT administrator_flag FROM t_employee \
                 WHERE login_name = 'admin' AND deleted_flag = 0 AND disabled_flag = 0 \
                 LIMIT 1",
            )
            .fetch_optional()
            .await
            .map_err(|_| err(StatusCode::BAD_REQUEST, DB_CONNECT_GUIDANCE))?;
        if administrator.and_then(|(flag,)| flag).unwrap_or(0) != 1 {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "目标不是可用的 DMS 身份权限库：缺少有效 admin 管理员账号",
            ));
        }
        st.auth_mysql
            .swap_pool(
                &dsn,
                AUTH_POOL_SIZE,
                dms_connector::mysql::MysqlCapability::IdentityPermission,
            )
            .await
            .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, DB_SWITCH_GUIDANCE))?;
        if patch_settings(&st, |v| {
            let obj = v.as_object_mut().ok_or("settings.json 顶层不是对象")?;
            obj.insert("mysql_url".into(), dsn.clone().into());
            Ok(())
        })
        .is_err()
        {
            if st
                .auth_mysql
                .swap_pool(
                    &old_dsn,
                    AUTH_POOL_SIZE,
                    dms_connector::mysql::MysqlCapability::IdentityPermission,
                )
                .await
                .is_err()
            {
                tracing::error!(target = "dms", reason = "runtime_rollback_failed", "DMS 权限源配置保存失败且旧池恢复失败");
            }
            return Err(err(StatusCode::INTERNAL_SERVER_ERROR, SETTINGS_WRITE_GUIDANCE));
        }
        // DMS 连接只服务身份/角色/权限；分析池绝不能跟随它切换。
        tracing::info!("DMS 权限源已更新（连通性已过）并热生效");
        return Ok(Json(serde_json::json!({
            "ok": true, "name": name, "hot": true,
            "protected": true, "query_target": false,
        })));
    }
    if req.keep_secret {
        // 直查 mysql_targets 而不是 db_targets 过滤目录：后者会过滤「与 DMS 同端点但非显式
        // production_lookup」的目标（db.rs:725-731），那些目标会被误报「不存在」。
        // 内存态 cfg 已是解密后的明文（D1 红线不变 —— 明文不出服务端）。
        let Some((_, old_target)) = cfg.mysql_targets.iter().find(|(n, _)| n.eq_ignore_ascii_case(&name)) else {
            return Err(err(StatusCode::BAD_REQUEST, format!("目标 {name} 不存在，密码留空保留不了")));
        };
        dsn = splice_userinfo(&dsn, old_target.url())
            .map_err(|_| err(StatusCode::BAD_REQUEST, DB_SECRET_GUIDANCE))?;
    }
    let capability = capability_from_type(&req.r#type)?;
    if crate::db::same_db_endpoint(&dsn, &cfg.mysql_url)
        && capability != dms_connector::mysql::MysqlCapability::ProductionLookup
    {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "与 DMS 权限库复用端点时，类型必须是 production_lookup（生产轻点查）",
        ));
    }
    dms_connector::mysql::test_pool(&dms_kernel::DsId::new("probe"), &dsn, capability)
        .await
        .map_err(|_| err(StatusCode::BAD_REQUEST, DB_CONNECT_GUIDANCE))?;
    let hot = st.mysql.target_name().eq_ignore_ascii_case(&name);
    let old_hot_url = if hot {
        crate::db::db_targets(&cfg)
            .into_iter()
            .find(|(target, _)| target.eq_ignore_ascii_case(&name))
            .map(|(_, url)| url)
    } else {
        None
    };
    let old_hot_capability = crate::db::db_target_capability(&cfg, &name);
    if hot {
        crate::admin_api::persist_db_target(&st, &name, &dsn, capability).await?;
    }
    if patch_settings(&st, |v| {
        let obj = v.as_object_mut().ok_or("settings.json 顶层不是对象")?;
        let t = obj
            .entry("mysql_targets".to_string())
            .or_insert_with(|| serde_json::json!({}));
        t.as_object_mut().ok_or("mysql_targets 不是对象")?.insert(
            name.clone(),
            serde_json::json!({ "url": dsn, "type": target_type_name(capability) }),
        );
        Ok(())
    })
    .is_err()
    {
        if hot {
            let rollback_failed = match old_hot_url.as_deref() {
                Some(url) => crate::admin_api::persist_db_target(
                    &st,
                    &name,
                    url,
                    old_hot_capability,
                )
                .await
                .is_err(),
                None => true,
            };
            if rollback_failed {
                tracing::error!(target = %name, reason = "runtime_rollback_failed", "分析目标配置保存失败且旧池恢复失败");
            }
        }
        return Err(err(StatusCode::INTERNAL_SERVER_ERROR, SETTINGS_WRITE_GUIDANCE));
    }
    if hot {
        crate::admin_api::after_db_target_switch(&st, &name).await;
    }
    tracing::info!(target = %name, hot, "分析目标已写入 settings.json");
    Ok(Json(serde_json::json!({ "ok": true, "name": name, "hot": hot })))
}

/// `DELETE /api/admin/settings/mysql-target/{name}` —— 删一个未生效的非内建目标。
pub async fn del_mysql_target(
    State(st): State<Arc<AppState>>, h: HeaderMap, Path(name): Path<String>, Query(q): Query<IdentQuery>,
) -> ApiRes {
    crate::admin_api::settings_admin_only(&st, &h, (&q.login_name, &q.role_code)).await?;
    let _settings_write = st.settings_write.lock().await;
    if name.eq_ignore_ascii_case("dms") {
        return Err(err(StatusCode::BAD_REQUEST, "dms 是受保护的身份/角色/权限源，不可删除或作为分析目标"));
    }
    let cfg = st.cfg();
    let name = matching_key(&cfg.mysql_targets, &name)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "数据库目标不存在"))?;
    if crate::admin_api::current_db_target_pub(&st).await.eq_ignore_ascii_case(&name) {
        return Err(err(
            StatusCode::CONFLICT,
            "当前生效数据库不能删除，请先切换到其他目标",
        ));
    }
    if let Err(e) = patch_settings(&st, |v| {
        let obj = v.as_object_mut().ok_or("settings.json 顶层不是对象")?;
        let Some(t) = obj.get_mut("mysql_targets").and_then(|t| t.as_object_mut()) else {
            return Err("settings.json 里没有 mysql_targets".into());
        };
        if t.remove(&name).is_none() {
            return Err(format!("目标 {name} 不存在"));
        }
        Ok(())
    }) {
        // 读写之间目标消失（文件被手工改过）：给 400 + 具体原因；
        // 其他落盘故障仍是 500 笼统指引
        if e.contains("不存在") {
            return Err(err(StatusCode::BAD_REQUEST, e));
        }
        return Err(err(StatusCode::INTERNAL_SERVER_ERROR, SETTINGS_WRITE_GUIDANCE));
    }
    tracing::info!(target = %name, "分析目标已删除");
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
pub struct LlmKeyReq {
    name: String,
    key: String,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `POST /api/admin/settings/llm-key` —— 设置/替换某供应商的 key（保存即生效）。
pub async fn put_llm_key(
    State(st): State<Arc<AppState>>, h: HeaderMap, Json(req): Json<LlmKeyReq>,
) -> ApiRes {
    crate::admin_api::settings_admin_only(&st, &h, (&req.login_name, &req.role_code)).await?;
    let _settings_write = st.settings_write.lock().await;
    let requested_name = req.name.trim().to_string();
    let key = req.key.trim().to_string();
    if !valid_name(&requested_name) {
        return Err(err(StatusCode::BAD_REQUEST, "供应商名只能含 ASCII 字母数字._-（≤32）"));
    }
    if !valid_key(&key) {
        return Err(err(StatusCode::BAD_REQUEST, "key 格式不合法（长度需为 8..4096 且不能含控制字符）"));
    }
    let current_cfg = st.cfg();
    let name = matching_key(&current_cfg.llm_keys, &requested_name)
        .or_else(|| matching_key(&current_cfg.llm_providers, &requested_name))
        .or_else(|| crate::db::provider_catalog().iter()
            .find(|(provider, _)| (*provider).eq_ignore_ascii_case(&requested_name))
            .map(|(provider, _)| (*provider).to_string()))
        .unwrap_or(requested_name);
    commit_llm_settings(&st, |v| {
        let obj = v.as_object_mut().ok_or("settings.json 顶层不是对象")?;
        let t = obj
            .entry("llm_keys".to_string())
            .or_insert_with(|| serde_json::json!({}));
        t.as_object_mut().ok_or("llm_keys 不是对象")?.insert(name.clone(), key.clone().into());
        Ok(())
    })?;
    tracing::info!(provider = %name, "LLM key 已写入 settings.json 并热生效");
    Ok(Json(serde_json::json!({ "ok": true, "name": name, "hot": true })))
}

/// `DELETE /api/admin/settings/llm-key/{name}` —— 移除一个 key（当前供应商正用它时不许删）。
pub async fn del_llm_key(
    State(st): State<Arc<AppState>>, h: HeaderMap, Path(name): Path<String>, Query(q): Query<IdentQuery>,
) -> ApiRes {
    crate::admin_api::settings_admin_only(&st, &h, (&q.login_name, &q.role_code)).await?;
    let _settings_write = st.settings_write.lock().await;
    if st.llm.primary_provider().eq_ignore_ascii_case(&name) {
        return Err(err(
            StatusCode::CONFLICT,
            "当前文本模型正在使用该 Key，请先切换默认模型",
        ));
    }
    if st
        .llm
        .fallback_vision_provider()
        .as_deref()
        .is_some_and(|provider| provider.eq_ignore_ascii_case(&name))
    {
        return Err(err(
            StatusCode::CONFLICT,
            "备用多模态模型正在使用该 Key，请先清除或切换备用模型",
        ));
    }
    let current_cfg = st.cfg();
    if crate::db::file_provider_name(&current_cfg).eq_ignore_ascii_case(&name)
        && !current_cfg.llm_api_key.is_empty()
    {
        return Err(err(
            StatusCode::CONFLICT,
            "该供应商仍由旧式 llm_api_key 提供凭据，不能单独删除；请先迁移基础配置",
        ));
    }
    let name = matching_key(&current_cfg.llm_keys, &name)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("key {name} 不存在")))?;
    commit_llm_settings(&st, |v| {
        let obj = v.as_object_mut().ok_or("settings.json 顶层不是对象")?;
        let Some(llm_keys) = obj.get_mut("llm_keys").and_then(|t| t.as_object_mut()) else {
            return Err("settings.json 里没有 llm_keys".into());
        };
        if llm_keys.remove(&name).is_none() {
            return Err(format!("key {name} 不存在"));
        }
        Ok(())
    })?;
    tracing::info!(provider = %name, "LLM key 已删除");
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ───────────────────── 【测试连通性】DB / LLM（只验不写，admin_only）─────────────────────

#[derive(serde::Deserialize)]
pub struct TestDbReq {
    dsn: String,
    #[serde(default)]
    r#type: String,
    /// 修改已有目标时允许账号/密码留空，服务端从现有配置补回凭据后再测试。
    #[serde(default)]
    keep_secret: bool,
    #[serde(default)]
    name: String,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `POST /api/admin/settings/test-db` —— 建一次性池验证 DSN（连得上 + 会话只读 + SELECT 1）。
/// 失败返回 200 + `{ok:false, error}` —— 「测不通」是测试的答案，不是端点的故障（500 会误导前端）。
pub async fn test_db(
    State(st): State<Arc<AppState>>, h: HeaderMap, Json(req): Json<TestDbReq>,
) -> ApiRes {
    crate::admin_api::settings_admin_only(&st, &h, (&req.login_name, &req.role_code)).await?;
    let mut dsn = req.dsn.trim().to_string();
    let name = req.name.trim();
    if req.keep_secret {
        // keep_secret 两次读取共用一份快照，不各克隆整份 Settings
        let cfg = st.cfg();
        let old = if name.eq_ignore_ascii_case("dms") {
            Some(cfg.mysql_url.clone())
        } else {
            crate::db::db_targets(&cfg)
                .into_iter()
                .find(|(target, _)| target.eq_ignore_ascii_case(name))
                .map(|(_, url)| url)
        }
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "要保留凭据的数据库目标不存在"))?;
        dsn = splice_userinfo(&dsn, &old)
            .map_err(|_| err(StatusCode::BAD_REQUEST, DB_SECRET_GUIDANCE))?;
    }
    let capability = if name.eq_ignore_ascii_case("dms") {
        dms_connector::mysql::MysqlCapability::IdentityPermission
    } else if req.r#type.trim().is_empty() {
        // type 留空且目标已配置：回落到该目标当前的能力类型（页面编辑已有目标不用重复选类型）；
        // 目标未配置则仍走类型闸，报「能力类型只允许…」
        match st.cfg().mysql_targets.iter().find(|(target_name, _)| target_name.eq_ignore_ascii_case(name)) {
            Some((_, target)) => target.capability(),
            None => capability_from_type(&req.r#type)?,
        }
    } else {
        capability_from_type(&req.r#type)?
    };
    match dms_connector::mysql::test_pool(
        &dms_kernel::DsId::new("probe"),
        &dsn,
        capability,
    )
    .await
    {
        Ok((ms, ver)) => Ok(Json(serde_json::json!({ "ok": true, "ms": ms, "version": ver }))),
        Err(_) => Ok(Json(serde_json::json!({
            "ok": false,
            "error": DB_CONNECT_GUIDANCE,
            "suggestion": "确认目标支持 MySQL 协议，并使用只读账号"
        }))),
    }
}

#[derive(serde::Deserialize)]
pub struct TestLlmReq {
    base_url: String,
    model: String,
    key: String,
    #[serde(default)]
    extra_body: serde_json::Map<String, serde_json::Value>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `POST /api/admin/settings/test-llm` —— 一句 ping 验证模型接入（fast 档形状，
/// 只返回延迟 + token 用量）。供应商正文与底层错误均不回传；测不通是答案不是故障。
pub async fn test_llm(
    State(st): State<Arc<AppState>>, h: HeaderMap, Json(req): Json<TestLlmReq>,
) -> ApiRes {
    crate::admin_api::settings_admin_only(&st, &h, (&req.login_name, &req.role_code)).await?;
    let model = req.model.trim().to_string();
    if req.base_url.trim().is_empty() || model.is_empty() || req.key.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "base_url / model / key 都要填"));
    }
    let base_url = req.base_url.trim().trim_end_matches('/').to_string();
    // 与 put_llm_provider 同一道出站闸：非 http(s)、带 userinfo/query 的地址不许探 ——
    // test_llm 同为 admin 触发的出站请求，信任面保持一致
    let public_url = crate::db::public_service_url(&base_url);
    if public_url != base_url || !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err(err(StatusCode::BAD_REQUEST, "base_url 必须 http(s) 开头"));
    }
    let conf = crate::llm::Conf {
        provider: "probe".into(),
        base_url,
        api_key: req.key.trim().to_string(),
        model_fast: model.clone(),
        model_precise: model.clone(),
        extra: req.extra_body,
        vision: None,
    };
    crate::llm::validate_conf(&conf, false)
        .map_err(|_| err(StatusCode::BAD_REQUEST, LLM_CONFIG_GUIDANCE))?;
    let client = crate::llm::LlmClient::with_conf(conf);
    let t0 = std::time::Instant::now();
    match client.chat_with_usage(&model, "你是连通性探针，只回两个字：正常", "ping").await {
        Ok((_text, usage)) => Ok(Json(serde_json::json!({
            "ok": true, "ms": t0.elapsed().as_millis(),
            "usage": { "prompt_tokens": usage.prompt_tokens, "completion_tokens": usage.completion_tokens },
        }))),
        Err(_) => Ok(Json(serde_json::json!({
            "ok": false,
            "error": LLM_CONNECT_GUIDANCE,
            "suggestion": "确认接口支持 chat/completions，模型名与 Key 属于同一供应商"
        }))),
    }
}

// ───────────────────── 【自定义供应商】`llm_providers` 的 CRUD ─────────────────────

#[derive(serde::Deserialize)]
pub struct LlmProviderUpsertReq {
    name: String,
    base_url: String,
    model_fast: Option<String>,
    model_precise: Option<String>,
    /// 思考级别：off | low | high | none（原样 extra_body 不在这里 —— 级别才是人能懂的形态，
    /// raw JSON 是「高级」，页面只发级别；要 raw 走手写文件）。
    /// **缺省 = "off"**：页面外的客户端漏传会把已配置思考档静默重置为关；
    /// 要保留现状必须显式传 "keep"。
    thinking: Option<String>,
    /// 多模态模型名（空 = 无视觉能力）
    vision: Option<String>,
    /// 顺手把 key 也存了（可选 —— 与「保存供应商」分开点也行）
    key: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// 思考级别 → extra_body（查预设目录的映射；目录没有这家 = 该级别无 extra）。
/// 返回 Err 表示「这家没有这个级别」—— 保存时不许静默写一个看起来开了实际没开的配置。
fn thinking_extra(preset_url: Option<&str>, level: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    if level == "none" {
        return Ok(Default::default());
    }
    let Some(url) = preset_url else {
        if level == "off" {
            return Ok(Default::default()); // 未知厂商的「关」= 不传（多数默认不思考）
        }
        return Err("自定义厂商的思考级别请用手写 extra_body（页面只支持预设厂商的档位）".into());
    };
    let p = crate::db::llm_presets()
        .iter()
        .find(|(_, p)| p.base_url.trim_end_matches('/') == url.trim_end_matches('/'))
        .map(|(_, p)| p);
    let raw = match (p, level) {
        (Some(p), "off") => p.thinking_off,
        (Some(p), "low") => p.thinking_low,
        (Some(p), "high") => p.thinking_high,
        _ => None,
    };
    match raw {
        Some(r) => serde_json::from_str(r).map_err(|_| "内建思考参数非法".to_string()),
        None if level == "off" => Ok(Default::default()),
        None => Err(format!("该厂商没有 {level} 思考档（用「关」或手写 extra_body）")),
    }
}

/// `POST /api/admin/settings/llm-provider` —— 新增/覆盖自定义供应商（保存即生效）。
pub async fn put_llm_provider(
    State(st): State<Arc<AppState>>, h: HeaderMap, Json(req): Json<LlmProviderUpsertReq>,
) -> ApiRes {
    crate::admin_api::settings_admin_only(&st, &h, (&req.login_name, &req.role_code)).await?;
    let _settings_write = st.settings_write.lock().await;
    let requested_name = req.name.trim().to_string();
    if !valid_name(&requested_name) {
        return Err(err(StatusCode::BAD_REQUEST, "供应商名只能含 ASCII 字母数字._-（≤32）"));
    }
    let current_cfg = st.cfg();
    let name = matching_key(&current_cfg.llm_providers, &requested_name)
        .or_else(|| crate::db::provider_catalog().iter()
            .find(|(name, _)| (*name).eq_ignore_ascii_case(&requested_name))
            .map(|(name, _)| (*name).to_string()))
        .unwrap_or(requested_name);
    // 内建同名 = **覆盖**（自定义条目优先于内建目录 —— 内建形状是代码常量改不了，
    // 想调模型名/地址/思考档就存一条同名自定义；删除该条即还原内建）
    let overrides_builtin = crate::db::provider_catalog()
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case(&name));
    let base_url = req.base_url.trim().trim_end_matches('/').to_string();
    let public_url = crate::db::public_service_url(&base_url);
    if public_url != base_url || !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err(err(StatusCode::BAD_REQUEST, "base_url 必须 http(s) 开头"));
    }
    let level = req.thinking.as_deref().unwrap_or("off");
    let extra = if level == "keep" {
        current_cfg
            .llm_providers
            .get(&name)
            .map(|provider| provider.extra_body.clone())
            .or_else(|| {
                crate::db::file_provider_name(&current_cfg)
                    .eq_ignore_ascii_case(&name)
                    .then(|| current_cfg.llm_extra_body.clone())
            })
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "没有可保留的现有思考参数"))?
    } else {
        thinking_extra(Some(base_url.as_str()), level)
            .map_err(|_| err(StatusCode::BAD_REQUEST, LLM_THINKING_GUIDANCE))?
    };
    let (mf, mp) = (
        req.model_fast.clone().unwrap_or_default().trim().to_string(),
        req.model_precise.clone().unwrap_or_default().trim().to_string(),
    );
    if mf.is_empty() && mp.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "model_fast / model_precise 至少填一个"));
    }
    // 落盘归一化：只填 precise 时 fast 回填同一模型 —— 下面的形状校验本就按回填口径过，
    // 存空串会让 catalog 显示空、llm_config 再回填一次，两端表示不一致
    let mf = if mf.is_empty() { mp.clone() } else { mf };
    let vision = req.vision.clone().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let key = req.key.clone().map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
    if key.as_ref().is_some_and(|key| !valid_key(key)) {
        return Err(err(StatusCode::BAD_REQUEST, "key 格式不合法（长度需为 8..4096 且不能含控制字符）"));
    }
    let provider = crate::db::CustomProvider {
        base_url: base_url.clone(),
        model_fast: mf.clone(),
        model_precise: mp.clone(),
        extra_body: extra.clone(),
        vision: vision.clone(),
    };
    crate::llm::validate_provider_shape(&crate::llm::Conf {
        provider: name.clone(),
        base_url: provider.base_url.clone(),
        api_key: key.clone().unwrap_or_else(|| "shape-only".into()),
        model_fast: if provider.model_fast.is_empty() {
            provider.model_precise.clone()
        } else {
            provider.model_fast.clone()
        },
        model_precise: if provider.model_precise.is_empty() {
            provider.model_fast.clone()
        } else {
            provider.model_precise.clone()
        },
        extra: provider.extra_body.clone(),
        vision: provider.vision.clone(),
    })
    .map_err(|_| err(StatusCode::BAD_REQUEST, LLM_CONFIG_GUIDANCE))?;
    commit_llm_settings(&st, |v| {
        let obj = v.as_object_mut().ok_or("settings.json 顶层不是对象")?;
        let t = obj
            .entry("llm_providers".to_string())
            .or_insert_with(|| serde_json::json!({}));
        t.as_object_mut().ok_or("llm_providers 不是对象")?.insert(
            name.clone(),
            serde_json::json!({
                "base_url": base_url,
                "model_fast": mf,
                "model_precise": mp,
                "extra_body": extra,
                "vision": vision,
            }),
        );
        if let Some(k) = &key {
            let keys = obj.entry("llm_keys".to_string()).or_insert_with(|| serde_json::json!({}));
            keys.as_object_mut().ok_or("llm_keys 不是对象")?.insert(name.clone(), k.clone().into());
        }
        Ok(())
    })?;
    tracing::info!(provider = %name, overrides_builtin, "供应商已写入 settings.json 并热生效");
    Ok(Json(serde_json::json!({ "ok": true, "name": name, "hot": true, "overrides_builtin": overrides_builtin })))
}

/// `DELETE /api/admin/settings/llm-provider/{name}` —— 删除未生效的自定义供应商；
/// 若它与内建供应商同名，则只删除覆盖项并恢复内建预设，保留该供应商的 key。
pub async fn del_llm_provider(
    State(st): State<Arc<AppState>>, h: HeaderMap, Path(name): Path<String>, Query(q): Query<IdentQuery>,
) -> ApiRes {
    crate::admin_api::settings_admin_only(&st, &h, (&q.login_name, &q.role_code)).await?;
    let _settings_write = st.settings_write.lock().await;
    if st.llm.primary_provider().eq_ignore_ascii_case(&name) {
        return Err(err(
            StatusCode::CONFLICT,
            "当前文本模型正在使用该供应商，请先切换默认模型",
        ));
    }
    if st
        .llm
        .fallback_vision_provider()
        .as_deref()
        .is_some_and(|provider| provider.eq_ignore_ascii_case(&name))
    {
        return Err(err(
            StatusCode::CONFLICT,
            "该供应商正在作为备用多模态模型，请先清除或切换备用模型",
        ));
    }
    let current_cfg = st.cfg();
    let Some(name) = matching_key(&current_cfg.llm_providers, &name) else {
        if crate::db::provider_catalog()
            .iter()
            .any(|(provider, _)| provider.eq_ignore_ascii_case(&name))
        {
            return Err(err(StatusCode::CONFLICT, "内建供应商受保护，不能删除"));
        }
        return Err(err(StatusCode::BAD_REQUEST, format!("自定义供应商 {name} 不存在")));
    };
    let restores_builtin = crate::db::provider_catalog()
        .iter()
        .any(|(provider, _)| provider.eq_ignore_ascii_case(&name));
    if crate::db::file_provider_name(&current_cfg).eq_ignore_ascii_case(&name) && !restores_builtin {
        return Err(err(
            StatusCode::CONFLICT,
            "该供应商仍是 settings 文件的基础供应商，不能从目录删除；请先迁移基础配置",
        ));
    }
    // 内建覆盖删除只还原预设，可复用的 key 留着；纯自定义删除才清理孤立 key
    let related_key = if restores_builtin {
        None
    } else {
        matching_key(&current_cfg.llm_keys, &name)
    };
    commit_llm_settings(&st, |v| {
        let obj = v.as_object_mut().ok_or("settings.json 顶层不是对象")?;
        let removed = obj
            .get_mut("llm_providers")
            .and_then(|t| t.as_object_mut())
            .and_then(|t| t.remove(&name));
        if removed.is_none() {
            return Err(format!("自定义供应商 {name} 不存在"));
        }
        if let Some(keys) = obj.get_mut("llm_keys").and_then(|t| t.as_object_mut()) {
            if let Some(key) = &related_key {
                keys.remove(key);
            }
        }
        Ok(())
    })?;
    tracing::info!(provider = %name, restores_builtin, "自定义供应商配置已删除");
    Ok(Json(serde_json::json!({ "ok": true, "restored_builtin": restores_builtin })))
}

#[derive(serde::Deserialize)]
pub struct FallbackVisionReq {
    /// null / 空字符串 = 清除备用；非空只保存供应商名，key 仍来自 llm_keys。
    provider: Option<String>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `POST /api/admin/settings/fallback-vision` —— 保存或清除备用多模态供应商，立即生效。
pub async fn set_fallback_vision(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Json(req): Json<FallbackVisionReq>,
) -> ApiRes {
    crate::admin_api::settings_admin_only(&st, &h, (&req.login_name, &req.role_code)).await?;
    let _settings_write = st.settings_write.lock().await;
    let requested_provider = req.provider.unwrap_or_default().trim().to_string();
    if !requested_provider.is_empty() && !valid_name(&requested_provider) {
        return Err(err(StatusCode::BAD_REQUEST, "供应商名只能含 ASCII 字母数字._-（≤32）"));
    }
    let current_cfg = st.cfg();
    let provider = if requested_provider.is_empty() {
        String::new()
    } else {
        matching_key(&current_cfg.llm_providers, &requested_provider)
            .or_else(|| crate::db::provider_catalog().iter()
                .find(|(provider, _)| (*provider).eq_ignore_ascii_case(&requested_provider))
                .map(|(provider, _)| (*provider).to_string()))
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "备用多模态供应商不存在"))?
    };
    if !provider.is_empty() {
        // 提交前预检：备用多模态必须有视觉能力且 key 就绪（与 catalog 的 vision_candidates
        // 同一口径 —— 自定义同名条目完整覆盖内建能力，`vision: null` 是明确关闭）。
        // 不预检的话失败会延迟到 commit 里变成笼统的 LLM_CONFIG_GUIDANCE。
        let vision = match current_cfg
            .llm_providers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&provider))
        {
            Some((_, custom)) => custom.vision.clone(),
            None => crate::db::provider_catalog()
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(&provider))
                .and_then(|(_, p)| p.vision.map(str::to_string)),
        };
        if vision.is_none() || !crate::db::provider_key_ready(&current_cfg, &provider) {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "该供应商未配置多模态能力或 Key，不能作为备用多模态模型",
            ));
        }
    }
    commit_llm_settings(&st, |v| {
        let obj = v.as_object_mut().ok_or("settings.json 顶层不是对象")?;
        if provider.is_empty() {
            obj.remove("fallback_vision_provider");
        } else {
            obj.insert("fallback_vision_provider".into(), provider.clone().into());
        }
        Ok(())
    })?;
    tracing::info!(configured = !provider.is_empty(), "备用多模态供应商已热更新");
    Ok(Json(serde_json::json!({
        "ok": true,
        "provider": if provider.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(provider)
        },
        "hot": true,
    })))
}

/// 【Y3】RRF 权重页内编辑的请求体：四路全部可选 —— 缺省的路**保留当前生效值**
/// （不是回落编译期默认；页面每次提交全量四路时两种口径等价）。
#[derive(serde::Deserialize)]
pub struct KbRrfWeightsReq {
    metadata: Option<f32>,
    relation: Option<f32>,
    kg: Option<f32>,
    ext_kb: Option<f32>,
    login_name: Option<String>,
    role_code: Option<String>,
}

/// `POST /api/admin/settings/kb-rrf-weights` —— 改 RRF 四路辅助召回权重，保存即生效。
///
/// 接线契约（本包纪律：**不注册进 main.rs**，编排方统一接；登记形态如下）：
/// ```text
/// .route("/api/admin/settings/kb-rrf-weights", post(settings_api::put_kb_rrf_weights))
/// ```
/// - body：`{"metadata":0.2,"relation":0.25,"kg":0.3,"ext_kb":0.2}`（四路均可缺省，
///   缺省的路保留现值；四路全缺省 = 只回报现值，不落盘不热更）
///   + 身份字段（login_name/role_code 或会话 token，同其他 settings 端点）。
/// - 200 `{"ok":true,"kb_rrf_weights":{...四路生效值...},"hot":true}`：
///   落盘 + `st.cfg()` 热更新，检索/问答链下次请求即取新快照。
/// - 400：任一路为负（`RrfWeights::validate`，与启动加载同一拒绝口径）；
///   403：非 DMS 管理员。
/// 当前生效值的读取面：`GET /api/admin/settings-catalog` 响应的 `kb_rrf_weights` 键。
pub async fn put_kb_rrf_weights(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Json(req): Json<KbRrfWeightsReq>,
) -> ApiRes {
    crate::admin_api::settings_admin_only(&st, &h, (&req.login_name, &req.role_code)).await?;
    let _settings_write = st.settings_write.lock().await;
    // 只读 4 个权重：读锁内 Copy 出来（RrfWeights 是 Copy），不克隆整份 Settings
    let current = st
        .cfg
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .kb_rrf_weights;
    // 四路全 None = 只想读现值：直接按现值回报成功，跳过无操作的文件写与热更
    if [&req.metadata, &req.relation, &req.kg, &req.ext_kb]
        .iter()
        .all(|v| v.is_none())
    {
        return Ok(Json(serde_json::json!({
            "ok": true,
            "kb_rrf_weights": {
                "metadata": current.metadata,
                "relation": current.relation,
                "kg": current.kg,
                "ext_kb": current.ext_kb,
            },
            "hot": true,
        })));
    }
    let next = dms_knowledge::retrieve::RrfWeights {
        metadata: req.metadata.unwrap_or(current.metadata),
        relation: req.relation.unwrap_or(current.relation),
        kg: req.kg.unwrap_or(current.kg),
        ext_kb: req.ext_kb.unwrap_or(current.ext_kb),
    };
    // 先验后写：400 带字段名（NaN/Inf 在 JSON 层就进不来，这里的闸只管负值；
    // prepare_settings 里还有同一道闸兜底 —— 写坏的文件不许落盘）
    next.validate()
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    patch_settings(&st, |v| {
        let obj = v.as_object_mut().ok_or("settings.json 顶层不是对象")?;
        // 四路全量写：文件里始终能看到完整生效面，不留「缺省路到底是多少」的猜测空间
        obj.insert(
            "kb_rrf_weights".into(),
            serde_json::json!({
                "metadata": next.metadata,
                "relation": next.relation,
                "kg": next.kg,
                "ext_kb": next.ext_kb,
            }),
        );
        Ok(())
    })
    .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, SETTINGS_WRITE_GUIDANCE))?;
    tracing::info!(metadata = next.metadata, relation = next.relation, kg = next.kg, ext_kb = next.ext_kb, "RRF 召回权重已热更新");
    Ok(Json(serde_json::json!({
        "ok": true,
        "kb_rrf_weights": {
            "metadata": next.metadata,
            "relation": next.relation,
            "kg": next.kg,
            "ext_kb": next.ext_kb,
        },
        "hot": true,
    })))
}

#[cfg(test)]
mod tests {
    /// patch_settings 的落盘红线：完整校验后原地单次写，绝不生成凭据副本，随后同步 cfg。
    #[test]
    fn patch_settings_keeps_single_file_secret_boundary() {
        let src = include_str!("settings_api.rs");
        let body = src.split("struct PreparedSettings").nth(1).expect("配置准备/持久化链不见了");
        let body = body.split("\nfn valid_name").next().unwrap();
        assert!(body.contains("Settings as serde::Deserialize>::deserialize(&v)"), "回读校验没了：{body}");
        assert!(body.contains("std::fs::write(&prepared.path, &prepared.out)"), "必须原地写（rename 会写到容器层）：{body}");
        // 锚点拆开拼，避免判据自己成为命中项。
        assert!(!body.contains(concat!("re", "name(")), "bind mount 单文件挂载点不许 rename：{body}");
        assert!(!body.contains(concat!(".", "bak")), "不许生成第二份明文配置：{body}");
        assert!(!body.contains(concat!("std::fs::", "copy")), "不许复制含凭据的配置：{body}");
        assert!(body.contains("st.cfg.write()"), "内存热更新没了：{body}");
        assert!(body.contains("unwrap_or_else(std::sync::PoisonError::into_inner)"), "cfg 锁中毒不该 panic 请求任务（整体覆盖写）：{body}");
        assert!(body.contains("std::fs::write(&prepared.path, &prepared.raw)"), "写失败没有在同一正式文件恢复旧内容：{body}");
        assert!(body.contains("path = %prepared.path"), "恢复失败日志必须带文件路径：{body}");
        // 内存进的是校验过的那份（与落盘同源），不是 patch 前的旧 cfg
        let write = body.find("st.cfg.write()").unwrap();
        let check = body.find("let checked").unwrap();
        assert!(check < write, "必须先校验后进锁：{body}");
    }

    /// 整个设置模块都不许产生第二份明文配置；不能只约束当前 patch_settings 实现。
    #[test]
    fn settings_module_never_creates_secret_backups() {
        let src = include_str!("settings_api.rs");
        assert!(!src.contains(concat!(".", "bak")), "settings 模块出现凭据备份后缀");
        assert!(!src.contains(concat!("std::fs::", "copy")), "settings 模块复制了含凭据文件");
        assert!(!src.contains(concat!("fs::", "rename")), "settings 模块不得旁路原地正式文件");
    }

    /// 【Y3】RRF 权重端点的三道闸锚点：管理员门禁 → 负值先验（400）→ patch_settings 落盘热更。
    /// 加载侧（db::load_settings）与保存侧（prepare_settings）的同一道 validate 也要钉住。
    #[test]
    fn kb_rrf_weights_endpoint_validates_before_persist() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("pub async fn put_kb_rrf_weights(")
            .nth(1)
            .expect("put_kb_rrf_weights 没了")
            .split("\n#[cfg(test)]")
            .next()
            .unwrap();
        assert!(body.contains("settings_admin_only"), "权重端点丢了管理员门禁：{body}");
        let validate = body.find("next.validate()").expect("权重端点丢了负值先验");
        let write = body.find("patch_settings(&st").expect("权重端点没有走统一落盘链");
        assert!(validate < write, "必须先验后写：{body}");
        assert!(body.contains("unwrap_or(current."), "缺省的路必须保留现值（不是回落编译期默认）：{body}");
        // 保存侧与加载侧同一道闸（改一边漏另一边 = 手写的坏文件能绕过页面校验启动失败）
        let prepare = src.split("fn prepare_settings").nth(1).unwrap();
        assert!(prepare.contains("kb_rrf_weights") && prepare.contains(".validate()"),
                "prepare_settings 的落盘前校验丢了 RRF 权重闸");
        let db = include_str!("db.rs");
        let load = db.split("pub fn load_settings").nth(1).unwrap();
        assert!(load.contains("kb_rrf_weights") && load.contains(".validate()"),
                "load_settings 的启动校验丢了 RRF 权重闸");
    }

    /// 当前分析目标必须先切走再删；拒绝发生在正式配置变更之前。
    #[test]
    fn deleting_active_db_target_is_rejected_before_mutation() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("pub async fn del_mysql_target(")
            .nth(1)
            .expect("del_mysql_target 没了")
            .split("\n#[derive(serde::Deserialize)]")
            .next()
            .unwrap();
        let current = body
            .find("current_db_target_pub(&st)")
            .expect("删除前没有检查当前生效目标");
        let conflict = body.find("StatusCode::CONFLICT").expect("当前目标未返回 409");
        let write = body.find("patch_settings(&st").expect("删除没有写正式 settings");
        assert!(current < conflict && conflict < write, "当前项保护必须先于配置变更：{body}");
        assert!(!body.contains("persist_db_target"), "删除端点不应隐式切换数据库：{body}");
    }

    /// 连通性端点只给固定建议与数字指标，不回传驱动错误或供应商正文。
    #[test]
    fn connectivity_errors_and_provider_body_are_not_public() {
        let src = include_str!("settings_api.rs");
        let test_db = src
            .split("pub async fn test_db(")
            .nth(1)
            .expect("test_db 没了")
            .split("\n#[derive(serde::Deserialize)]")
            .next()
            .unwrap();
        assert!(test_db.contains("Err(_)" ) && test_db.contains("DB_CONNECT_GUIDANCE"));

        let test_llm = src
            .split("pub async fn test_llm(")
            .nth(1)
            .expect("test_llm 没了")
            .split("\n// ─")
            .next()
            .unwrap();
        assert!(test_llm.contains("Err(_)") && test_llm.contains("LLM_CONNECT_GUIDANCE"));
        assert!(!test_llm.contains(concat!("snip", "pet")), "供应商正文仍被回传");
        assert!(test_llm.contains("validate_conf(&conf, false)"), "模型探针缺统一配置校验");
        for body in [test_db, test_llm] {
            assert!(!body.contains(concat!("e.", "to_string()")), "底层错误被公开：{body}");
            assert!(!body.contains(concat!("error = %", "e")), "底层错误被写日志：{body}");
        }
    }

    /// 内建预设本体受保护；同名自定义覆盖可删除并恢复预设，纯自定义项才清理孤立 key。
    #[test]
    fn provider_delete_restores_builtin_override_and_cleans_only_custom_key() {
        let src = include_str!("settings_api.rs");
        let body = src.split("pub async fn del_llm_provider").nth(1).expect("del_llm_provider 没了");
        let body = body.split("\npub async fn ").next().unwrap_or(body);
        let custom_lookup = body.find("matching_key(&current_cfg.llm_providers, &name)").expect("删除前未区分自定义覆盖项");
        let builtin_guard = body.find("内建供应商受保护，不能删除").expect("内建预设本体没有删除保护");
        let restore = body.find("let restores_builtin =").expect("同名覆盖删除后没有恢复内建预设");
        let mutation = body.find(".get_mut(\"llm_providers\")").expect("自定义删除动作不见了");
        assert!(custom_lookup < builtin_guard && builtin_guard < restore && restore < mutation, "删除分支顺序不对：{body}");
        assert!(
            body.contains("file_provider_name(&current_cfg).eq_ignore_ascii_case(&name) && !restores_builtin"),
            "纯自定义文件供应商要保护，但内建同名覆盖必须允许删除：{body}",
        );
        assert!(body.contains(".and_then(|t| t.remove(&name))"), "没有真正移除自定义覆盖项：{body}");
        assert!(body.contains("let related_key = if restores_builtin"), "内建覆盖删除不应清理可复用 key：{body}");
        assert!(body.contains("keys.remove(key)"), "纯自定义删除后应清理孤立 key：{body}");
        assert!(body.contains("\"restored_builtin\": restores_builtin"), "响应没有说明是否恢复内建预设：{body}");
    }

    /// 设置面所有读写/测试端点必须统一走严格 admin Bearer 闸门，不能靠页面隐藏兜底。
    #[test]
    fn every_settings_handler_uses_the_admin_gate() {
        let src = include_str!("settings_api.rs");
        for name in [
            "catalog",
            "put_mysql_target",
            "del_mysql_target",
            "put_llm_key",
            "del_llm_key",
            "test_db",
            "test_llm",
            "put_llm_provider",
            "del_llm_provider",
            "set_fallback_vision",
        ] {
            let marker = format!("pub async fn {name}(");
            let body = src
                .split(&marker)
                .nth(1)
                .unwrap_or_else(|| panic!("设置端点 {name} 不见了"))
                .split("\npub async fn ")
                .next()
                .unwrap();
            assert!(
                body.contains("settings_admin_only"),
                "设置端点 {name} 没有统一 admin 闸门"
            );
        }
    }

    /// 删除供应商或 Key 时，运行时主模型与备用视觉模型都必须先显式拦截；不能依赖
    /// 后续 resolve 失败间接拒绝，因为内建同名覆盖项删除后仍可解析，曾能绕过该保护。
    #[test]
    fn deleting_llm_config_rejects_runtime_dependencies_before_mutation() {
        let src = include_str!("settings_api.rs");
        for (name, mutation) in [
            ("del_llm_key", "llm_keys.remove"),
            ("del_llm_provider", ".get_mut(\"llm_providers\")"),
        ] {
            let marker = format!("pub async fn {name}(");
            let body = src
                .split(&marker)
                .nth(1)
                .unwrap_or_else(|| panic!("{name} 不见了"))
                .split("\npub async fn ")
                .next()
                .unwrap();
            let primary = body.find("primary_provider()").expect("缺当前文本模型保护");
            let fallback = body
                .find("fallback_vision_provider()")
                .expect("缺备用多模态模型保护");
            let mutation = body.find(mutation).expect("删除动作不见了");
            assert!(primary < mutation && fallback < mutation, "必须先拒绝占用，再改配置：{body}");
            assert!(body.contains("StatusCode::CONFLICT"), "占用冲突必须返回 409：{body}");
            assert!(
                body.contains("eq_ignore_ascii_case(&name)"),
                "主/备供应商占用保护必须大小写无关：{body}",
            );
        }
    }

    /// 与权限源同端点的目标只有显式生产点查能力能保存；写回时必须固化为结构化
    /// `production_lookup`，后续 `db_targets` 才能区分它与旧字符串/数仓目标。
    #[test]
    fn same_endpoint_save_requires_and_persists_production_lookup() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("pub async fn put_mysql_target(")
            .nth(1)
            .expect("put_mysql_target 没了")
            .split("\n/// `DELETE /api/admin/settings/mysql-target")
            .next()
            .unwrap();
        assert!(body.contains("same_db_endpoint(&dsn, &cfg.mysql_url)"));
        assert!(body.contains("capability != dms_connector::mysql::MysqlCapability::ProductionLookup"));
        assert!(body.contains("target_type_name(capability)"), "保存后没有固化能力类型：{body}");
    }

    /// 备用模型只保存供应商名，但提交时必须连同当前文本模型解析成一个运行时快照，
    /// 再通过统一提交函数同时落盘与热切换。
    #[test]
    fn fallback_vision_save_is_atomic_and_hot() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("pub async fn set_fallback_vision(")
            .nth(1)
            .expect("set_fallback_vision 没了")
            .split("\n#[cfg(test)]")
            .next()
            .unwrap();
        let candidate = body.find("let provider =").expect("备用供应商候选没生成");
        let commit = body.find("commit_llm_settings").expect("没有统一热提交");
        assert!(candidate < commit, "候选/提交顺序错误：{body}");
        assert!(body.contains("\"hot\": true"), "响应没有声明保存即生效：{body}");
    }

    #[test]
    fn llm_settings_commit_holds_runtime_snapshot_through_persistence() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("fn commit_llm_settings(")
            .nth(1)
            .expect("统一 LLM 设置提交函数不见了")
            .split("\n}")
            .next()
            .unwrap();
        assert!(body.contains("commit_runtime_configs"));
        assert!(body.contains("prepare_settings(patch)"));
        assert!(body.contains("runtime_configs(st, &prepared.checked)"));
        assert!(body.contains("persist_settings(st, &prepared)"));
        assert!(!body.contains("set_runtime_configs"), "不得在文件写入窗口之外先热切换");
    }

    /// 名字闸：合法字符 + 长度 + dms 保护（判据打纯函数）
    #[test]
    fn name_gate() {
        assert!(super::valid_name("zhongtai"));
        assert!(super::valid_name("zt-1_b.v2"));
        assert!(!super::valid_name(""));
        assert!(!super::valid_name("有中文"));
        assert!(!super::valid_name("a b"));
        assert!(!super::valid_name(&"x".repeat(33)));
    }

    #[test]
    fn database_target_names_reuse_existing_case() {
        let mut map = std::collections::HashMap::new();
        map.insert("Doris_Warehouse".to_string(), "value");
        assert_eq!(
            super::matching_key(&map, "doris_warehouse").as_deref(),
            Some("Doris_Warehouse")
        );
    }

    #[test]
    fn provider_candidates_are_case_insensitive() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("let mut candidate_names")
            .nth(1)
            .expect("备用视觉候选目录不见了")
            .split("let effective_vision")
            .next()
            .unwrap();
        assert!(body.contains("dedup_by(|left, right| left.eq_ignore_ascii_case(right))"));
        assert!(body.contains("provider.eq_ignore_ascii_case(&name)"));
    }

    /// 思考级别映射：预设厂商的档位对齐目录；没有的档位报错不静默；未知厂商「关」= 空。
    #[test]
    fn thinking_level_maps_presets() {
        let qwen = &crate::db::llm_presets().iter().find(|(n, _)| *n == "qwen").unwrap().1;
        let m = super::thinking_extra(Some(qwen.base_url), "off").unwrap();
        assert_eq!(m.get("enable_thinking"), Some(&serde_json::json!(false)));
        let ds = &crate::db::llm_presets().iter().find(|(n, _)| *n == "deepseek").unwrap().1;
        let m = super::thinking_extra(Some(ds.base_url), "high").unwrap();
        assert_eq!(m.get("reasoning_effort"), Some(&serde_json::json!("high")));
        // GLM 没有思考档 → high 报错不静默
        let glm = &crate::db::llm_presets().iter().find(|(n, _)| *n == "glm").unwrap().1;
        assert!(super::thinking_extra(Some(glm.base_url), "high").is_err());
        assert!(super::thinking_extra(Some(glm.base_url), "off").unwrap().is_empty(), "没档的「关」= 空 extra");
        // 未知厂商：「关」= 空，「低/高」拒
        assert!(super::thinking_extra(None, "off").unwrap().is_empty());
        assert!(super::thinking_extra(None, "high").is_err());
        assert!(super::thinking_extra(Some("https://unknown.example.com"), "high").is_err());
        assert!(super::thinking_extra(Some(qwen.base_url), "none").unwrap().is_empty());
    }

    /// 预设目录的形状（页面下拉全靠它）：六家、base_url 带 https、模型名非空、
    /// 视觉名要么全要么没有 —— 加厂商漏一项，页面就出一个半残表单。
    #[test]
    fn presets_are_complete_for_dropdown() {
        let ps = crate::db::llm_presets();
        assert!(ps.len() >= 5, "{ps:?}");
        for (n, p) in ps {
            assert!(p.base_url.starts_with("https://"), "{n}");
            assert!(!p.model_fast.is_empty() && !p.model_precise.is_empty(), "{n}");
            assert!(!p.label.is_empty(), "{n}");
        }
        // 千问/豆包的视觉模型不能没有（页面「是否支持多模态」按它显隐）
        for want in ["qwen", "doubao", "glm", "kimi"] {
            let p = &ps.iter().find(|(n, _)| *n == want).unwrap().1;
            assert!(p.vision.is_some(), "{want} 应有视觉模型名");
        }
        assert!(ps.iter().find(|(n, _)| *n == "deepseek").unwrap().1.vision.is_none(), "DeepSeek 无视觉");
    }

    /// key 形状闸（纯函数）：长度 8..4096 且不含控制字符。
    #[test]
    fn key_gate() {
        assert!(super::valid_key(&"k".repeat(8)));
        assert!(super::valid_key(&"k".repeat(4096)));
        assert!(!super::valid_key(""));
        assert!(!super::valid_key(&"k".repeat(7)));
        assert!(!super::valid_key(&"k".repeat(4097)));
        assert!(!super::valid_key("abc12345\n"));
    }

    /// 思考级别识别（纯函数，走预设缓存）：已知厂商命中档位（尾斜杠差异不影响），
    /// 手写未知 extra_body 标 keep，空 extra 是 none。
    #[test]
    fn configured_thinking_level_matches_cached_presets() {
        let qwen = &crate::db::llm_presets().iter().find(|(n, _)| *n == "qwen").unwrap().1;
        let off: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(qwen.thinking_off.unwrap()).unwrap();
        assert_eq!(super::configured_thinking_level(qwen.base_url, &off), "off");
        let with_slash = format!("{}/", qwen.base_url);
        assert_eq!(super::configured_thinking_level(&with_slash, &off), "off");
        let high: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(qwen.thinking_high.unwrap()).unwrap();
        assert_eq!(super::configured_thinking_level(qwen.base_url, &high), "high");
        let mut handwritten = serde_json::Map::new();
        handwritten.insert("custom_flag".into(), serde_json::json!(true));
        assert_eq!(super::configured_thinking_level(qwen.base_url, &handwritten), "keep");
        assert_eq!(super::configured_thinking_level(qwen.base_url, &serde_json::Map::new()), "none");
    }

    /// put_mysql_target 全程只快照一次 cfg（读多个字段不再反复克隆整份 Settings）；
    /// auth 池大小走常量；keep_secret 直查 mysql_targets（db_targets 的过滤目录会误报不存在）。
    #[test]
    fn put_mysql_target_snapshots_cfg_once_and_looks_up_secret_directly() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("pub async fn put_mysql_target(")
            .nth(1)
            .expect("put_mysql_target 没了")
            .split("\n/// `DELETE /api/admin/settings/mysql-target")
            .next()
            .unwrap();
        assert_eq!(body.matches("st.cfg()").count(), 1, "cfg 快照应只取一次：{body}");
        assert_eq!(body.matches("AUTH_POOL_SIZE").count(), 2, "auth 池大小热换/回滚应共用常量：{body}");
        // 第二个 keep_secret 分支是非 dms 目标的凭据保留
        let non_dms = body
            .split("if req.keep_secret {")
            .nth(2)
            .expect("非 dms 分支 keep_secret 没了");
        let lookup = non_dms.split("splice_userinfo").next().unwrap();
        assert!(lookup.contains("cfg.mysql_targets.iter().find"), "keep_secret 应直查 mysql_targets：{lookup}");
        // 锚点拆开拼，避免命中实现注释里的函数名。
        assert!(!lookup.contains(concat!("db_", "targets(")), "keep_secret 走过滤目录会误报目标不存在：{lookup}");
    }

    /// 删除目标的 patch 失败按内容分派：读写之间目标消失是 400 + 具体原因，落盘故障仍是 500。
    #[test]
    fn del_mysql_target_maps_patch_errors_by_content() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("pub async fn del_mysql_target(")
            .nth(1)
            .expect("del_mysql_target 没了")
            .split("\n#[derive(serde::Deserialize)]")
            .next()
            .unwrap();
        assert!(body.contains("if let Err(e) = patch_settings(&st"), "删除端点吞掉了 patch 具体错误：{body}");
        assert!(body.contains("StatusCode::BAD_REQUEST, e"), "目标消失应给 400 + 具体原因：{body}");
        assert!(body.contains("SETTINGS_WRITE_GUIDANCE"), "落盘故障仍是 500 笼统指引：{body}");
    }

    /// LLM 统一提交链：校验类失败（字段校验/重复名/RRF 权重）透传 400 + 原文案，服务端故障仍是 500。
    #[test]
    fn llm_commit_maps_validation_errors_to_400() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("fn commit_llm_settings(")
            .nth(1)
            .expect("统一 LLM 设置提交函数不见了")
            .split("\n/// GET/DELETE")
            .next()
            .unwrap();
        assert!(body.contains("StatusCode::BAD_REQUEST, e"), "校验类失败必须透传 400 + 原文案：{body}");
        assert!(body.contains("校验失败"), "校验类判据丢了字段校验：{body}");
        assert!(
            body.contains("StatusCode::INTERNAL_SERVER_ERROR, SETTINGS_WRITE_GUIDANCE"),
            "服务端故障仍是 500 笼统指引：{body}"
        );
    }

    /// test_db 的 type 留空且目标已配置时回落到已配置能力；未配置目标仍走类型闸。
    #[test]
    fn test_db_empty_type_falls_back_to_configured_capability() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("pub async fn test_db(")
            .nth(1)
            .expect("test_db 没了")
            .split("\n#[derive(serde::Deserialize)]")
            .next()
            .unwrap();
        let fallback = body
            .find("req.r#type.trim().is_empty()")
            .expect("type 留空回落分支没了");
        let typed = body
            .find("capability_from_type(&req.r#type)?")
            .expect("类型闸没了");
        assert!(fallback < typed, "留空回落必须优先于类型闸：{body}");
        assert!(body.contains("target.capability()"), "没有回落到已配置能力：{body}");
    }

    /// test_llm 与 put_llm_provider 同一道出站闸：非 http(s)/带 userinfo 的地址不许探。
    #[test]
    fn test_llm_shares_the_provider_url_gate() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("pub async fn test_llm(")
            .nth(1)
            .expect("test_llm 没了")
            .split("\n// ─")
            .next()
            .unwrap();
        let gate = body
            .find("public_service_url(&base_url)")
            .expect("test_llm 缺出站地址闸");
        let probe = body.find("chat_with_usage").expect("探针调用不见了");
        assert!(gate < probe, "地址闸必须先于出站请求：{body}");
    }

    /// 供应商落盘归一化：只填 model_precise 时 model_fast 回填同值（catalog 与 llm_config 两端一致）。
    #[test]
    fn llm_provider_persist_normalizes_empty_fast_model() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("pub async fn put_llm_provider(")
            .nth(1)
            .expect("put_llm_provider 没了")
            .split("\n/// `DELETE /api/admin/settings/llm-provider")
            .next()
            .unwrap();
        assert!(body.contains("if mf.is_empty() && mp.is_empty()"), "双空校验没了：{body}");
        let normalize = body
            .find("let mf = if mf.is_empty() { mp.clone() } else { mf };")
            .expect("model_fast 落盘归一化没了");
        let persist = body
            .find("commit_llm_settings(&st")
            .expect("供应商保存没有走统一提交");
        assert!(normalize < persist, "必须先归一化再落盘：{body}");
        assert!(body.contains("!valid_key(key)"), "key 形状闸应共用 valid_key：{body}");
    }

    /// 备用多模态提交前预检视觉能力与 key 就绪，精确 400 而不是笼统的 commit 失败。
    #[test]
    fn fallback_vision_prechecks_capability_and_key() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("pub async fn set_fallback_vision(")
            .nth(1)
            .expect("set_fallback_vision 没了")
            .split("\n#[cfg(test)]")
            .next()
            .unwrap();
        let candidate = body.find("let provider =").expect("备用供应商候选没生成");
        let precheck = body
            .find("provider_key_ready(&current_cfg, &provider)")
            .expect("备用多模态缺 key 就绪/视觉能力预检");
        let commit = body.find("commit_llm_settings(&st").expect("没有统一热提交");
        assert!(candidate < precheck && precheck < commit, "预检必须在候选之后、提交之前：{body}");
        assert!(body.contains("StatusCode::BAD_REQUEST"), "预检失败必须 400：{body}");
    }

    /// 四路全 None 的 RRF 请求只回报现值，不做无操作的落盘与热更；读现值不克隆整份 Settings。
    #[test]
    fn kb_rrf_weights_all_none_short_circuits_before_persist() {
        let src = include_str!("settings_api.rs");
        let body = src
            .split("pub async fn put_kb_rrf_weights(")
            .nth(1)
            .expect("put_kb_rrf_weights 没了")
            .split("\n#[cfg(test)]")
            .next()
            .unwrap();
        let short_circuit = body
            .find(".all(|v| v.is_none())")
            .expect("四路全 None 短路没了");
        let write = body.find("patch_settings(&st").expect("权重端点没有走统一落盘链");
        assert!(short_circuit < write, "全 None 短路必须在落盘之前：{body}");
        assert!(!body.contains("st.cfg().kb_rrf_weights"), "读 4 个权重不该克隆整份 Settings：{body}");
    }
}
