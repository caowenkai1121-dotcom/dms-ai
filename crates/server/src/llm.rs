//! LLM 客户端：OpenAI 兼容 HTTP（DeepSeek/千问），无框架依赖。

use serde::Serialize;

/// 一次切换要换的全部字段（供应商目录里的一条）。
/// `api_key` 只经 settings.json 进来 —— 不入库、不进日志、不进任何响应（红线同 DSN）。
#[derive(Clone)]
pub struct Conf {
    /// 供应商目录名。仅用于能力路由与脱敏响应，不是凭据。
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub model_fast: String,
    pub model_precise: String,
    /// 供应商特有参数，合并进每次请求体（见 `Settings::llm_extra_body`）
    pub extra: serde_json::Map<String, serde_json::Value>,
    /// 视觉模型名（`None` = 该供应商没有图片识别能力 —— DeepSeek 全系。
    /// 千问 flash 自己就是视觉模型，实测 988ms 三题全对，不需要单独的 vision 型号）。
    pub vision: Option<String>,
}

/// 【热切换】配置住在 `RwLock` 里：每次调用现读（`POST /api/admin/llm-provider`
/// 保存即生效，不需要重启 —— 实测诉求）。读锁一次调用一次，纳秒级，不是性能问题。
#[derive(Clone)]
pub struct LlmClient {
    /// 主配置与备用视觉配置必须来自同一个快照；否则热切换并发时可能混出
    /// “旧主模型 + 新备用模型”的不存在组合。
    runtime: std::sync::Arc<std::sync::RwLock<RuntimeConf>>,
    http: reqwest::Client,
}

#[derive(Clone)]
struct RuntimeConf {
    /// Arc 化：每次调用只付一次引用计数；整份克隆会把 `extra` 整个 Map 带上。
    primary: std::sync::Arc<Conf>,
    fallback_vision: Option<std::sync::Arc<Conf>>,
}

/// 锁毒化恢复：锁只会被「持锁期间 panic」（如 persist 闭包）毒化，那时运行时配置仍是
/// 完整快照。恢复警卫继续服务，而不是让此后**所有** LLM 调用连锁 panic。
fn unpoison<T>(r: std::sync::LockResult<T>) -> T {
    r.unwrap_or_else(|e| e.into_inner())
}

/// 写入前的归一化：剥掉 `base_url` 首尾空白与尾斜杠。校验（`validate_base_url`）与使用
/// （拼 `{base_url}/chat/completions`）从此看同一份值 —— settings.json 里带尾斜杠
/// 不会再打出 `//chat/completions`，首尾空白也不会校验过了却原样存进去。
fn normalized(mut conf: Conf) -> Conf {
    conf.base_url = conf.base_url.trim().trim_end_matches('/').to_string();
    conf
}

#[derive(Clone, Debug)]
pub struct VisionCapability {
    pub provider: String,
    pub model: String,
    pub fallback: bool,
}

pub(crate) const MAX_VISION_IMAGE_URL_BYTES: usize = 16 * 1024 * 1024;

/// 单次 LLM 请求总超时。Precise 档长生成实测十几秒（思考全开 ~17s），
/// 90s 给长生成留足余量，又不让挂死的连接无限占着。
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// LLM 响应体上限。正常补全响应是 KB 级，这只为拦「异常上游/代理回超大 body」。
const MAX_LLM_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// 视觉调用只向协议层暴露固定错误类别。上游 URL、响应正文和供应商名称都不能进入错误链，
/// 避免未来新增日志或调用方直接返回错误时泄漏配置细节。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisionError {
    InvalidImage,
    ImageTooLarge,
    Unavailable,
    Upstream,
}

impl std::fmt::Display for VisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidImage => "图片仅支持 HTTPS 地址或受支持的 data:image Base64 数据",
            Self::ImageTooLarge => "图片数据（base64 后）不能超过 16MB",
            Self::Unavailable => "当前未配置可用的多模态模型",
            Self::Upstream => "图片解析服务暂时不可用",
        };
        f.write_str(message)
    }
}

impl std::error::Error for VisionError {}

/// `llm_extra_body` **不许**出现的键：能覆盖它们的配置项等于静默改行为。
/// `messages` = 配置文件可做任意提示注入；`model` = fast/precise 两档形同虚设。
/// 两个都不会报错，所以只能在构造时硬拦。
const EXTRA_FORBIDDEN: &[&str] = &[
    "messages",
    "model",
    "temperature",
    // 用户明确要求全系统不设置应用层输出 token 上限；同时禁止配置 extra 偷偷加回。
    "max_tokens",
    "max_completion_tokens",
    "stream",
    "authorization",
    "api_key",
    "apikey",
    "access_token",
    "token",
];

#[derive(Serialize)]
struct Msg<'a> {
    role: &'a str,
    content: &'a str,
}

impl LlmClient {
    /// 带供应商特有参数的构造。**空 map 时与 `new` 逐字节等价**（判据 `empty_extra_is_byte_identical`）。
    ///
    /// # Panics
    /// `extra` 含 `messages`/`model` 时 panic —— 那是启动期的配置错误，必须响亮失败。
    /// 静默忽略它会让人以为配置生效了，静默接受它是提示注入通道。
    #[cfg(test)]
    pub fn with_extra(
        base_url: &str,
        api_key: &str,
        fast: &str,
        precise: &str,
        extra: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self::with_conf(Conf {
            provider: "custom".to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model_fast: fast.to_string(),
            model_precise: precise.to_string(),
            extra,
            vision: None,
        })
    }

    /// 整份 Conf 的构造（供应商目录的落点）。forbidden 键检查与 `with_extra` 同一处。
    pub fn with_conf(conf: Conf) -> Self {
        Self::with_conf_and_fallback(conf, None)
    }

    /// 启动时一次装入主供应商与备用视觉供应商。备用只在主供应商无 vision 时消费。
    pub fn with_conf_and_fallback(conf: Conf, fallback: Option<Conf>) -> Self {
        let conf = normalized(conf);
        let fallback = fallback.map(normalized);
        validate_conf(&conf, false).unwrap_or_else(|e| panic!("settings 的 LLM 配置无效: {e}"));
        if let Some(c) = fallback.as_ref() {
            validate_conf(c, true)
                .unwrap_or_else(|e| panic!("settings 的备用多模态配置无效: {e}"));
        }
        Self {
            runtime: std::sync::Arc::new(std::sync::RwLock::new(RuntimeConf {
                primary: std::sync::Arc::new(conf),
                fallback_vision: fallback.map(std::sync::Arc::new),
            })),
            http: reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .build()
                .expect("http client"),
        }
    }

    /// 热切换（保存即生效）：换整份 Conf。forbidden 键在这里**返回错误**而不是 panic ——
    /// 这是运行时路径，不是启动路径，拒绝切换就是了。
    #[cfg(test)]
    pub fn set_conf(&self, conf: Conf) -> anyhow::Result<()> {
        let conf = normalized(conf);
        validate_conf(&conf, false)?;
        unpoison(self.runtime.write()).primary = std::sync::Arc::new(conf);
        Ok(())
    }

    /// 保存/清除备用视觉供应商。先完整校验再换锁，拒绝时旧配置原样保留。
    #[cfg(test)]
    pub fn set_fallback_vision(&self, conf: Option<Conf>) -> anyhow::Result<()> {
        let conf = conf.map(normalized);
        if let Some(c) = conf.as_ref() {
            validate_conf(c, true)?;
        }
        unpoison(self.runtime.write()).fallback_vision = conf.map(std::sync::Arc::new);
        Ok(())
    }

    /// 设置页同时改供应商形状/key/备用模型时，用一次校验后的临界区提交整份运行时快照。
    /// 任一配置不合法时快照不动；调用期间不会出现主配置更新而备用仍是旧值。
    pub fn set_runtime_configs(&self, primary: Conf, fallback: Option<Conf>) -> anyhow::Result<()> {
        let primary = normalized(primary);
        let fallback = fallback.map(normalized);
        validate_conf(&primary, false)?;
        if let Some(c) = fallback.as_ref() {
            validate_conf(c, true)?;
        }
        *unpoison(self.runtime.write()) = RuntimeConf {
            primary: std::sync::Arc::new(primary),
            fallback_vision: fallback.map(std::sync::Arc::new),
        };
        Ok(())
    }

    /// 设置文件与运行时主备模型的一次提交。写锁覆盖持久化窗口，因此并发调用只能看到
    /// 完整旧快照或完整新快照；持久化失败时在释放锁前恢复旧快照。
    pub fn commit_runtime_configs(
        &self,
        primary: Conf,
        fallback: Option<Conf>,
        persist: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let primary = normalized(primary);
        let fallback = fallback.map(normalized);
        validate_conf(&primary, false)?;
        if let Some(c) = fallback.as_ref() {
            validate_conf(c, true)?;
        }
        let mut runtime = unpoison(self.runtime.write());
        let old = runtime.clone();
        *runtime = RuntimeConf {
            primary: std::sync::Arc::new(primary),
            fallback_vision: fallback.map(std::sync::Arc::new),
        };
        if let Err(error) = persist() {
            *runtime = old;
            return Err(error);
        }
        Ok(())
    }

    /// 当前 Conf 的快照（调用一次的读取点：base_url/key/模型/extra 全从快照出，
    /// 切换半途不会出现「base_url 是新的、模型是旧的」的混搭）。
    fn conf(&self) -> std::sync::Arc<Conf> {
        unpoison(self.runtime.read()).primary.clone()
    }

    /// 当前供应商的视觉能力：`Some(模型名)` = 支持图片识别，`None` = 没有（DeepSeek）。
    /// 调用方（图片问答/企微拍照）按它降级，而不是猜供应商名。
    #[cfg(test)]
    pub fn vision_model(&self) -> Option<String> {
        self.vision_capability().map(|c| c.model)
    }

    pub fn primary_provider(&self) -> String {
        unpoison(self.runtime.read()).primary.provider.clone()
    }

    pub fn fallback_vision_provider(&self) -> Option<String> {
        unpoison(self.runtime.read())
            .fallback_vision
            .as_ref()
            .map(|c| c.provider.clone())
    }

    /// 最终视觉能力：主供应商优先；主供应商没有 vision 才看备用。
    pub fn vision_capability(&self) -> Option<VisionCapability> {
        self.vision_route().ok().map(|(_, c)| c)
    }

    /// 当前生效的快照（`/api/admin/llm-config` 的 effective 段；**不含 api_key** ——
    /// key 只经 settings.json 进来，永不出现在任何响应里）。
    pub fn public_conf(&self) -> (String, String, String, Option<String>, serde_json::Map<String, serde_json::Value>) {
        let c = self.conf();
        (
            c.base_url.clone(),
            c.model_fast.clone(),
            c.model_precise.clone(),
            c.vision.clone(),
            c.extra.clone(),
        )
    }

    /// 【K6-B】带 token 用量的一次调用。**全仓唯一的 LLM 出口**：拆分前那个丢弃 usage 的
    /// `chat(model, system, user)` 薄包装随 `pipeline.rs`/`triage.rs` 一起没了调用点（T9），
    /// 留着它就是留一条「用量静默变 0」的路 —— 而那条路正好是下面 `ChatModel` 曾经踩的坑。
    pub async fn chat_with_usage(
        &self,
        model: &str,
        system: &str,
        user: &str,
    ) -> anyhow::Result<(String, dms_kernel::llm::Usage)> {
        let c = self.conf();
        self.chat_with_conf(&c, model, system, user, Some(0.1))
            .await
    }

    async fn chat_with_conf(
        &self,
        c: &Conf,
        model: &str,
        system: &str,
        user: &str,
        temperature: Option<f32>,
    ) -> anyhow::Result<(String, dms_kernel::llm::Usage)> {
        let body = build_body(model, system, user, temperature, &c.extra);
        // 吞错纪律：reqwest/serde 真因只进服务端 tracing 日志，错误链保持笼统文案
        // （上游细节不回浏览器 —— 红线），排障去日志里查。
        let resp = self
            .http
            .post(format!("{}/chat/completions", c.base_url))
            .bearer_auth(&c.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(err = %e, "LLM 请求发送失败");
                anyhow::anyhow!("LLM 请求失败")
            })?;
        let status = resp.status();
        if !status.is_success() {
            // 供应商侧错误详情（限流原因、模型名下线）截断留痕，不进错误链
            let detail = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %detail.chars().take(512).collect::<String>(), "LLM 非 2xx 响应");
            anyhow::bail!("LLM 请求失败（HTTP {status}）");
        }
        if resp.content_length().is_some_and(|n| n > MAX_LLM_RESPONSE_BYTES as u64) {
            anyhow::bail!("LLM 响应格式无效");
        }
        let raw = resp.bytes().await.map_err(|e| {
            tracing::warn!(err = %e, "LLM 响应读取失败");
            anyhow::anyhow!("LLM 响应格式无效")
        })?;
        if raw.len() > MAX_LLM_RESPONSE_BYTES {
            anyhow::bail!("LLM 响应格式无效");
        }
        let v: serde_json::Value = serde_json::from_slice(&raw).map_err(|e| {
            tracing::warn!(err = %e, "LLM 响应解析失败");
            anyhow::anyhow!("LLM 响应格式无效")
        })?;
        let text = content_text(&v).ok_or_else(|| anyhow::anyhow!("LLM 响应缺 content"))?;
        Ok((text, read_usage(&v["usage"])))
    }

    /// `chat_with_conf` 的流式变体（KB 流式问答）：同一条 `/chat/completions`，body 只多
    /// `stream`/`stream_options` 两键。增量（剥思考段**之前**的原文）边收边推 `on_delta` ——
    /// 仅供预览；返回的累计全文仍过 `strip_thinking`，content 口径与非流式逐字一致。
    /// 吞错纪律同款：reqwest/serde 真因只进 tracing，错误链保持笼统文案。
    async fn chat_stream_with_conf(
        &self,
        c: &Conf,
        model: &str,
        system: &str,
        user: &str,
        temperature: Option<f32>,
        on_delta: &mut (dyn FnMut(&str) + Send),
    ) -> anyhow::Result<(String, dms_kernel::llm::Usage)> {
        let body = build_stream_body(model, system, user, temperature, &c.extra);
        let mut resp = self
            .http
            .post(format!("{}/chat/completions", c.base_url))
            .bearer_auth(&c.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(err = %e, "LLM 流式请求发送失败");
                anyhow::anyhow!("LLM 请求失败")
            })?;
        let status = resp.status();
        if !status.is_success() {
            let detail = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %detail.chars().take(512).collect::<String>(), "LLM 流式非 2xx 响应");
            anyhow::bail!("LLM 请求失败（HTTP {status}）");
        }
        let mut lines = SseLines::default();
        let mut full = String::new();
        let mut usage = dms_kernel::llm::Usage::default();
        let mut take = |line: String,
                        full: &mut String,
                        usage: &mut dms_kernel::llm::Usage|
         -> anyhow::Result<()> {
            match parse_stream_line(&line) {
                StreamLine::Delta(piece) => {
                    full.push_str(&piece);
                    if full.len() > MAX_LLM_RESPONSE_BYTES {
                        anyhow::bail!("LLM 响应格式无效");
                    }
                    on_delta(&piece);
                }
                StreamLine::Usage(u) => *usage = u,
                StreamLine::Done | StreamLine::Ignore => {}
            }
            Ok(())
        };
        loop {
            let Some(chunk) = resp.chunk().await.map_err(|e| {
                tracing::warn!(err = %e, "LLM 流式响应读取失败");
                anyhow::anyhow!("LLM 响应格式无效")
            })?
            else {
                break;
            };
            for line in lines.feed(&chunk)? {
                take(line, &mut full, &mut usage)?;
            }
        }
        // 流尾残余（无换行收尾的最后一行）同样过一解析
        if let Some(line) = lines.finish() {
            take(line, &mut full, &mut usage)?;
        }
        // 与 `content_text` 同一条判据：剥完思考段只剩空白 = 无内容
        let text = strip_thinking(&full);
        if text.is_empty() {
            anyhow::bail!("LLM 响应缺 content");
        }
        Ok((text, usage))
    }

    /// OpenAI-compatible 多模态调用的统一出口。图片/OCR/企微拍照都应调用本方法，
    /// 不能自己猜 qwen 或复制 key。`image_url` 支持 data URL 与 https URL。
    pub async fn vision_chat(
        &self,
        prompt: &str,
        image_url: &str,
    ) -> Result<(String, dms_kernel::llm::Usage, VisionCapability), VisionError> {
        let image_url = image_url.trim();
        validate_vision_image(image_url)?;
        let (c, route) = self.vision_route()?;
        let mut body = serde_json::json!({
            "model": route.model.clone(),
            "messages": [
                {"role": "system", "content": "准确识别图片中的文字、表格、对象与业务信息；不要猜测不可见内容。"},
                {"role": "user", "content": [
                    {"type": "text", "text": if prompt.trim().is_empty() { "请识别并说明图片内容。" } else { prompt.trim() }},
                    {"type": "image_url", "image_url": {"url": image_url}}
                ]}
            ],
            "temperature": 0.1
        });
        if let Some(o) = body.as_object_mut() {
            for (k, v) in &c.extra {
                o.insert(k.clone(), v.clone());
            }
        }
        let resp = self
            .http
            .post(format!("{}/chat/completions", c.base_url))
            .bearer_auth(&c.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|_| VisionError::Upstream)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(VisionError::Upstream);
        }
        let v: serde_json::Value = resp.json().await.map_err(|_| VisionError::Upstream)?;
        let text = content_text(&v).ok_or(VisionError::Upstream)?;
        Ok((text, read_usage(&v["usage"]), route))
    }

    fn vision_route(&self) -> Result<(std::sync::Arc<Conf>, VisionCapability), VisionError> {
        // 快照是 Arc 克隆（两次引用计数），不再整份克隆两份 Conf
        let runtime = unpoison(self.runtime.read()).clone();
        let primary = runtime.primary;
        if let Some(model) = primary.vision.clone().filter(|m| !m.trim().is_empty()) {
            let route = VisionCapability {
                provider: primary.provider.clone(),
                model,
                fallback: false,
            };
            return Ok((primary, route));
        }
        let fallback = runtime.fallback_vision.ok_or(VisionError::Unavailable)?;
        let model = fallback
            .vision
            .clone()
            .filter(|m| !m.trim().is_empty())
            .ok_or(VisionError::Unavailable)?;
        let route = VisionCapability {
            provider: fallback.provider.clone(),
            model,
            fallback: true,
        };
        Ok((fallback, route))
    }
}

fn validate_vision_image(image_url: &str) -> Result<(), VisionError> {
    validate_vision_image_len(image_url.len())?;
    // `Url::parse` 把 scheme 归一小写（WHATWG URL）：`HTTPS://…` 这类合法写法不再被误拒
    if let Ok(url) = reqwest::Url::parse(image_url) {
        if url.scheme() == "https" {
            if url.host().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(VisionError::InvalidImage);
            }
            return Ok(());
        }
    } else if image_url
        .get(.."https://".len())
        .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
    {
        // 形如 https 但不是合法 URL（如无 host）：直接拒，别掉进下面的 data URL 分支
        //（用 `get` 而不是直接切片：多字节字符的位边界处切片会 panic）
        return Err(VisionError::InvalidImage);
    }
    let (header, payload) = image_url.split_once(',').ok_or(VisionError::InvalidImage)?;
    // RFC 2397 mediatype 大小写不敏感：统一转小写再比
    let header = header.to_ascii_lowercase();
    if !matches!(
        header.as_str(),
        "data:image/png;base64"
            | "data:image/jpeg;base64"
            | "data:image/jpg;base64"
            | "data:image/bmp;base64"
            | "data:image/tiff;base64"
            | "data:image/webp;base64"
    ) || payload.is_empty()
        || payload.len() % 4 != 0
    {
        return Err(VisionError::InvalidImage);
    }
    let padding = payload.as_bytes().iter().rev().take_while(|&&b| b == b'=').count();
    if padding > 2
        || !payload.as_bytes()[..payload.len() - padding]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'+' | b'/'))
        || !valid_base64_padding_bits(payload.as_bytes(), padding)
    {
        return Err(VisionError::InvalidImage);
    }
    Ok(())
}

fn valid_base64_padding_bits(payload: &[u8], padding: usize) -> bool {
    let value = |b: u8| -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    match padding {
        0 => true,
        1 => payload.len() >= 4 && value(payload[payload.len() - 2]).is_some_and(|v| v & 0b11 == 0),
        2 => payload.len() >= 4 && value(payload[payload.len() - 3]).is_some_and(|v| v & 0b1111 == 0),
        _ => false,
    }
}

fn validate_vision_image_len(len: usize) -> Result<(), VisionError> {
    if len > MAX_VISION_IMAGE_URL_BYTES {
        Err(VisionError::ImageTooLarge)
    } else {
        Ok(())
    }
}

/// 配置校验只读，供“先校验、后持久化、最后热换”使用。校验失败不碰任何锁。
pub fn validate_conf(conf: &Conf, require_vision: bool) -> anyhow::Result<()> {
    validate_provider_shape(conf)?;
    if conf.api_key.trim().is_empty() {
        anyhow::bail!("模型配置没有 key");
    }
    if conf.api_key.len() > 4096 || conf.api_key.chars().any(char::is_control) {
        anyhow::bail!("模型 key 过长或含控制字符");
    }
    if require_vision && conf.vision.as_deref().map(str::trim).filter(|m| !m.is_empty()).is_none() {
        anyhow::bail!("备用多模态配置没有 vision 模型");
    }
    Ok(())
}

/// 校验可保存但尚未启用的供应商形状；Key 可稍后填写，其他字段不能先存坏再等切换时报错。
pub fn validate_provider_shape(conf: &Conf) -> anyhow::Result<()> {
    // 报错带上命中的键名（键名本身不敏感，值才是）——配置排障不该靠猜
    let offender = conf.extra.iter().find_map(|(key, value)| {
        if forbidden_extra_key(key) {
            Some(key.as_str())
        } else {
            forbidden_extra_field(value)
        }
    });
    if let Some(key) = offender {
        anyhow::bail!("extra_body 不许含保留或敏感字段（命中键 `{key}`）");
    }
    validate_base_url(&conf.base_url)?;
    if conf.model_fast.trim().is_empty() || conf.model_precise.trim().is_empty() {
        // 地址问题已在上面 `validate_base_url` 返回，走到这里只是模型为空
        anyhow::bail!("fast/precise 模型不能为空");
    }
    for value in [Some(conf.model_fast.as_str()), Some(conf.model_precise.as_str()), conf.vision.as_deref()]
        .into_iter()
        .flatten()
    {
        if value.len() > 160 || value.chars().any(char::is_control) {
            anyhow::bail!("模型名称过长或含控制字符");
        }
    }
    Ok(())
}

fn forbidden_extra_key(key: &str) -> bool {
    let normalized = key.trim().to_ascii_lowercase().replace('-', "_");
    EXTRA_FORBIDDEN.iter().any(|blocked| normalized == *blocked)
        || normalized.contains("api_key")
        || normalized.contains("access_token")
        || normalized.ends_with("_token")
        || normalized.contains("secret")
        || normalized.contains("password")
}

fn forbidden_extra_field(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::Object(map) => map.iter().find_map(|(key, value)| {
            forbidden_extra_key(key)
                .then_some(key.as_str())
                .or_else(|| forbidden_extra_field(value))
        }),
        serde_json::Value::Array(values) => values.iter().find_map(forbidden_extra_field),
        _ => None,
    }
}

fn validate_base_url(base_url: &str) -> anyhow::Result<()> {
    let url = reqwest::Url::parse(base_url.trim())
        .map_err(|_| anyhow::anyhow!("供应商地址不是有效 URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        anyhow::bail!("供应商地址只支持 HTTP(S)");
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        anyhow::bail!("供应商地址不能携带认证信息、查询参数或片段");
    }
    Ok(())
}

/// 剥掉推理模型混进 `content` 的思考段（`<think>…</think>` 一族）。
///
/// 🔴 这不是「缺一个功能」，是**埋着的错答**：
/// `prompt::extract_sql` 的兜底是「裸文本里第一个 SELECT」，而思考段里恰好总有几条
/// **被模型自己推翻的** SQL 草稿。一旦某个供应商把思考混进 content（现用千问系是推理模型，
/// `enable_thinking` 只是个请求参数，供应商侧默认值随时可能变），我们就会执行草稿而不是结论——
/// 而它 EXPLAIN 能过、三段闸门能过、口径判据也可能过，**没有任何断言会红**。
/// SQLBot 显式解析这一段并单独推给前端，我们至少要保证它不进 `extract_sql`。
///
/// 只剥**成对**的标签：单独一个 `</think>` 说明思考段被截断了，那时 content 里
/// 剩下的是结论那一半，剥掉开头反而对（见判据 `strip_thinking_handles_truncation`）。
fn strip_thinking(s: &str) -> String {
    // 只钉小写形态：`<THINK>`/`<Think>` 变体剥不掉是已知缝隙（现用供应商实测都是小写输出；
    // 真要堵需小写化扫描后按原串切片，未见到真实样本前不为假设付扫描成本）。
    const PAIRS: &[(&str, &str)] = &[
        ("<think>", "</think>"),
        ("<thinking>", "</thinking>"),
        ("<reasoning>", "</reasoning>"),
    ];
    // 快速路径：没有 '<' 就不可能命中任何标签，直接省掉两次全量克隆
    if !s.contains('<') {
        return s.trim().to_string();
    }
    let mut out = s.to_string();
    for (open, close) in PAIRS {
        loop {
            let Some(i) = out.find(open) else {
                // 只有闭标签：思考段在传输里被截了头，闭标签**之前**的全是思考
                if let Some(j) = out.find(close) {
                    out = out[j + close.len()..].to_string();
                }
                break;
            };
            let Some(j) = out[i..].find(close) else {
                // 只有开标签：后面全是没写完的思考，整段丢掉
                out.truncate(i);
                break;
            };
            out.replace_range(i..i + j + close.len(), "");
        }
    }
    out.trim().to_string()
}

/// 请求体构造。抽成纯函数是为了让「空 `extra` 与本功能引入前逐字节相同」成为**可测的断言**
/// 而不是一句注释 —— 那条等价性是本改动的全部安全论证（DeepSeek 那边一个字节都不该变）。
fn build_body(
    model: &str,
    system: &str,
    user: &str,
    temperature: Option<f32>,
    extra: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            Msg { role: "system", content: system },
            Msg { role: "user", content: user },
        ],
        "temperature": f64::from(temperature.unwrap_or(0.1)),
    });
    // `extra` 空时这个循环一次都不转。保留键已在统一配置校验入口拦掉，
    // 所以这里的 insert 不可能覆盖基础请求字段 —— 那条不变量由 `forbidden_keys_panic` 守。
    if let Some(o) = body.as_object_mut() {
        for (k, v) in extra {
            o.insert(k.clone(), v.clone());
        }
    }
    body
}

/// 流式请求体 = `build_body` + 两个流式键（`stream` 是 extra 的 forbidden 键，
/// `stream_options` 在 extra 之后写死 —— 配置面无论如何关不掉/抢不走流式）。
/// `include_usage`：OpenAI/DeepSeek/千问兼容端点都在末块回 usage，拿不到时记 0 不报错
/// （与 `read_usage` 的缺段纪律同款；观测缺一格不把问答变失败）。
fn build_stream_body(
    model: &str,
    system: &str,
    user: &str,
    temperature: Option<f32>,
    extra: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    let mut body = build_body(model, system, user, temperature, extra);
    if let Some(o) = body.as_object_mut() {
        o.insert("stream".to_string(), serde_json::Value::Bool(true));
        o.insert(
            "stream_options".to_string(),
            serde_json::json!({ "include_usage": true }),
        );
    }
    body
}

/// OpenAI 兼容流式响应的一行解析结果（纯函数，形态由判据测试钉死）。
#[derive(Debug, PartialEq, Eq)]
enum StreamLine {
    /// `data: [DONE]` —— 流正常收尾
    Done,
    /// `data: {...}` 里的正文增量（`choices[0].delta.content`）
    Delta(String),
    /// 末块携带的用量（`usage` 对象）；与正文增量同块时**增量优先**（见 parse_stream_line）
    Usage(dms_kernel::llm::Usage),
    /// 空行 / `event:` / 注释 / 无增量块 / 半行坏 JSON：跳过（流式解析纪律 = 容错向前）
    Ignore,
}

/// 解析单行。只认 `data:` 前缀；`data.trim()` 一并吃掉 `\r\n` 尾巴与 OWS。
fn parse_stream_line(line: &str) -> StreamLine {
    let Some(data) = line.strip_prefix("data:") else {
        return StreamLine::Ignore;
    };
    let data = data.trim();
    if data.is_empty() {
        return StreamLine::Ignore;
    }
    if data == "[DONE]" {
        return StreamLine::Done;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return StreamLine::Ignore;
    };
    // 增量优先于 usage：正常供应商不同块，真同块时丢用量只是观测缺格，丢正文是答案缺字
    if let Some(piece) = v["choices"][0]["delta"]["content"].as_str() {
        if !piece.is_empty() {
            return StreamLine::Delta(piece.to_string());
        }
    }
    if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
        return StreamLine::Usage(read_usage(u));
    }
    StreamLine::Ignore
}

/// SSE 行装配器：喂字节块，吐出完整行（跨块半行留缓冲）。
/// 按 `\n`（0x0A）切安全：UTF-8 多字节序列的字节都 ≥0x80，绝不等于 0x0A ——
/// 中文跨块切开不会污染行边界（判据测试钉这条）。
#[derive(Default)]
struct SseLines {
    buf: Vec<u8>,
}

impl SseLines {
    fn feed(&mut self, chunk: &[u8]) -> anyhow::Result<Vec<String>> {
        self.buf.extend_from_slice(chunk);
        // 无换行洪泛闸：上游/代理回一条永不换行的流时，缓冲不许无界涨
        if self.buf.len() > MAX_LLM_RESPONSE_BYTES {
            anyhow::bail!("LLM 响应格式无效");
        }
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..pos).collect();
            self.buf.drain(..1); // 吃掉 \n 本身
            // 完整行必是完整 UTF-8（见上）；真收到非法字节跳过该行，不炸整条流
            if let Ok(s) = std::str::from_utf8(&line) {
                out.push(s.strip_suffix('\r').unwrap_or(s).to_string());
            }
        }
        Ok(out)
    }

    /// 流尾残余（供应商没以换行收尾的最后一行）；空缓冲 = None
    fn finish(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            return None;
        }
        let rest = std::mem::take(&mut self.buf);
        std::str::from_utf8(&rest)
            .ok()
            .map(|s| s.strip_suffix('\r').unwrap_or(s).to_string())
    }
}

/// 从 OpenAI 兼容响应里取正文：缺 content、或剥完思考段只剩空白，都按「无内容」处理。
/// 文本与视觉两条出口同一判据 —— 空串不该一路流到 `extract_sql` 才变成「无 SQL」。
fn content_text(v: &serde_json::Value) -> Option<String> {
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(strip_thinking)
        .filter(|text| !text.trim().is_empty())
}

/// OpenAI 兼容响应的 `usage` 段 → `Usage`。缺段/缺字段（供应商不回用量）一律记 0，
/// **绝不报错**：观测缺一格不能把这次问答变成失败。
fn read_usage(v: &serde_json::Value) -> dms_kernel::llm::Usage {
    let n = |k: &str| v[k].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32;
    dms_kernel::llm::Usage {
        prompt_tokens: n("prompt_tokens"),
        completion_tokens: n("completion_tokens"),
    }
}

/// 【K2】给现有客户端戴上 kernel 的 `ChatModel` 帽子——**不新建第二个 HTTP 客户端**
/// （T4 才把实现整体搬进 `connector/llm.rs`，那之前两份客户端＝两份超时/重试语义）。
///
/// 请求级 `temperature` 必须原样进入 HTTP body：意图解析与知识问答用 0 温度追求稳定，
/// SQL 自修/自一致性采样会显式升温。应用层不发送模型输出 token/费用上限。
///
/// 🔴 **usage 必须原样填回**：这里曾经写 `usage: Default::default()`（全 0）。T9 之后**所有**
/// LLM 调用都走本 trait，那个 0 就等于把 `meta.query_log` 的 token 列静默清空 ——
/// 没有任何测试会红，只有对账时发现成本统计恒 0。故走 `chat_with_usage` 并把真用量带出去。
impl dms_kernel::ChatModel for LlmClient {
    fn chat<'a>(
        &'a self,
        req: dms_kernel::ChatRequest,
    ) -> dms_kernel::BoxFut<'a, Result<dms_kernel::ChatReply, dms_kernel::LlmError>> {
        Box::pin(async move {
            // 模型名从**当前快照**出（热切换后下一次调用立即用新模型）
            let c = self.conf();
            let model = match req.tier {
                dms_kernel::ModelTier::Fast => c.model_fast.clone(),
                dms_kernel::ModelTier::Precise => c.model_precise.clone(),
            };
            let (system, user) = split_roles(&req.messages);
            let temperature = req.temperature;
            let (content, usage) = self
                .chat_with_conf(&c, &model, &system, &user, temperature)
                .await
                .map_err(|e| dms_kernel::LlmError::Transport(e.to_string()))?;
            Ok(dms_kernel::ChatReply { content: Some(content), usage })
        })
    }

    /// 流式覆盖（KB 流式问答）：真 SSE 边收边推 `on_delta`（剥思考段前的原文预览）。
    /// content/usage 语义与 `chat` 逐字一致 —— 最终全文过同一条 `strip_thinking`，
    /// 供应商末块不回 usage 时记 0（`stream_options.include_usage` 已请求，不回不报错）。
    fn chat_stream<'a>(
        &'a self,
        req: dms_kernel::ChatRequest,
        mut on_delta: Box<dyn FnMut(&str) + Send + 'a>,
    ) -> dms_kernel::BoxFut<'a, Result<dms_kernel::ChatReply, dms_kernel::LlmError>> {
        Box::pin(async move {
            let c = self.conf();
            let model = match req.tier {
                dms_kernel::ModelTier::Fast => c.model_fast.clone(),
                dms_kernel::ModelTier::Precise => c.model_precise.clone(),
            };
            let (system, user) = split_roles(&req.messages);
            let temperature = req.temperature;
            let (content, usage) = self
                .chat_stream_with_conf(
                    &c,
                    &model,
                    &system,
                    &user,
                    temperature,
                    &mut *on_delta,
                )
                .await
                .map_err(|e| dms_kernel::LlmError::Transport(e.to_string()))?;
            Ok(dms_kernel::ChatReply { content: Some(content), usage })
        })
    }
}

/// `messages` → 现有 `chat()` 的两个字符串：system 取第一条 system 角色，其余按序拼成 user。
/// `ChatRequest::text` 产的正是「一条 system + 一条 user」，多轮形态先原样落平（v1 无多轮 LLM 调用）。
fn split_roles(msgs: &[dms_kernel::Message]) -> (String, String) {
    let mut system = String::new();
    // 显式标志而不是 `system.is_empty()`：首条 system 内容为空串时，
    // 第二条 system 不许静默顶位（顶位等于把一条系统提示混进 user）
    let mut seen_system = false;
    let mut user: Vec<&str> = Vec::new();
    for m in msgs {
        if m.role == "system" && !seen_system {
            seen_system = true;
            system = m.content.clone();
        } else {
            user.push(&m.content);
        }
    }
    (system, user.join("\n\n"))
}

// `extract_sql` 随 prompt 渲染整块迁 `dms_agent::prompt`（T9，逐字搬运）：唯一的调用点
// （`pipeline.rs` 的生成与自修）已在那边，server 侧留一份实现就是两份会漂的解析器。
// 三个断言仍在本文件跑（见下面 `mod tests` 的 `use`）——它们守的是「围栏 / 裸 SELECT / 无 SQL」
// 三种解析形态，与它住哪个 crate 无关，而断言只增不减。

#[cfg(test)]
mod tests {
    use super::*;

    // 实现在 agent（见上）；断言体一字未改，靠这一行把符号带回作用域。
    use dms_agent::extract_sql;

    /// 🔴 本改动的全部安全论证：`extra` 空时请求体与**引入本字段之前**完全相同 ——
    /// DeepSeek 那边不该因为千问的需求多一个键、少一个键或改一个值。
    ///
    /// 第一版把期望写成手抄的序列化字面量，当场红：`serde_json::Map` 默认是 `BTreeMap`
    /// （没开 `preserve_order`），输出按**字典序**而不是书写序。
    /// 钉字节序会让这条判据绑在 serde 的一个实现细节上，而 HTTP body 的键序本就无意义。
    /// 所以改成钉**键集合 + 逐字段值**：多一个键、少一个键、值漂了，三种都会红，
    /// 而 serde 换排序策略不会误伤。
    /// 🔴 思考段里的 SQL 草稿**绝不能**流到 `extract_sql`。
    /// 判据直接用 `extract_sql` 对拍：那才是真正的下游，只断言「字符串里没有 think」
    /// 挡不住「草稿被抽走」这件事本身。
    #[test]
    fn thinking_draft_never_reaches_extract_sql() {
        // 🔴 风险形状是**结论不带围栏**。写这条判据时先踩了一次：结论带 ```sql 围栏时
        // `extract_sql` 优先取围栏，抽到的本来就是正确那条 —— 那个形状下根本没有风险，
        // 我的「量器自证」当场红，红得对。真正危险的是模型给裸 SELECT 结论：
        // 那时走「裸文本里第一个 SELECT」兜底，第一个 SELECT 在思考段里 = 被自己推翻的草稿。
        let raw = "<think>先试 SELECT SUM(amount) FROM t_wrong；\n\
                   不对，amount 在明细表且有 2x 重复行，应该去重。</think>\n\
                   SELECT SUM(x) FROM t_right";
        // 量器自证：不剥的话真的会抽到草稿（这一条红了就说明本判据是空转的）
        let unstripped = extract_sql(raw).expect("不剥时该抽到东西");
        assert!(
            unstripped.contains("t_wrong"),
            "不剥时抽不到草稿？本判据没守住任何东西 —— 先核 extract_sql 改了什么：{unstripped}"
        );
        // 剥完之后抽到的是结论
        let got = strip_thinking(raw);
        assert!(!got.contains("t_wrong"), "草稿没剥掉：{got}");
        assert_eq!(extract_sql(&got).unwrap(), "SELECT SUM(x) FROM t_right");
        // 带围栏的形状也不许被剥坏（那个形状本来就安全，但不能因为剥而变坏）
        let fenced = "<think>SELECT 1 FROM t_wrong</think>\n```sql\nSELECT SUM(x) FROM t_right\n```";
        assert_eq!(extract_sql(&strip_thinking(fenced)).unwrap(), "SELECT SUM(x) FROM t_right");
    }

    /// 三种残缺形态：只有闭标签（头被截）、只有开标签（尾没写完）、多段。
    #[test]
    fn strip_thinking_handles_truncation() {
        // 只有闭标签 ⇒ 之前的都是思考，剥掉开头是对的
        assert_eq!(strip_thinking("想了半天</think>SELECT 1"), "SELECT 1");
        // 只有开标签 ⇒ 后面是没写完的思考，整段丢
        assert_eq!(strip_thinking("SELECT 1<think>还在想"), "SELECT 1");
        // 多段 + 另一族标签名
        assert_eq!(strip_thinking("<think>a</think>X<thinking>b</thinking>Y"), "XY");
        // 没有思考段时**一个字符都不许动**（这是 DeepSeek 侧零变更的论证）
        let plain = "```sql\nSELECT 1 FROM t\n```";
        assert_eq!(strip_thinking(plain), plain);
        // 只有首尾空白被 trim（`extract_sql` 本来就 trim，不改变行为）
        assert_eq!(strip_thinking("  SELECT 1  "), "SELECT 1");
    }

    #[test]
    fn empty_extra_changes_nothing() {
        let got = build_body("m1", "sys", "usr", Some(0.1), &serde_json::Map::new());
        let o = got.as_object().expect("body 必须是对象");
        let mut keys: Vec<&str> = o.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["messages", "model", "temperature"], "键集合变了：{got}");
        assert_eq!(o["model"], "m1");
        assert!((o["temperature"].as_f64().unwrap() - 0.1).abs() < 1e-6);
        assert_eq!(
            o["messages"],
            serde_json::json!([
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "usr"},
            ]),
            "messages 的形状/顺序变了：{got}"
        );
    }

    #[test]
    fn request_sampling_reaches_the_wire_without_output_limit() {
        let deterministic = build_body(
            "m1",
            "sys",
            "usr",
            Some(0.0),
            &serde_json::Map::new(),
        );
        assert_eq!(deterministic["temperature"], serde_json::json!(0.0));
        assert!(deterministic.get("max_tokens").is_none());

        let retry = build_body("m1", "sys", "usr", Some(0.5), &serde_json::Map::new());
        assert_eq!(retry["temperature"], serde_json::json!(0.5));
        assert!(retry.get("max_tokens").is_none());
    }

    /// 千问要的那个键真的进得去，且**不动**原有三个键。
    #[test]
    fn extra_merges_without_touching_the_base() {
        let mut e = serde_json::Map::new();
        e.insert("enable_thinking".into(), serde_json::Value::Bool(false));
        let got = build_body("m1", "sys", "usr", Some(0.1), &e);
        assert_eq!(got["enable_thinking"], serde_json::json!(false));
        assert_eq!(got["model"], "m1");
        assert!((got["temperature"].as_f64().unwrap() - 0.1).abs() < 1e-6);
        assert_eq!(got["messages"][1]["content"], "usr");
        // 实测账：这个键关掉思考后同一道 SQL 题 780ms/65tok，不关 16626ms/2281tok，产出一样。
        assert_eq!(got.as_object().unwrap().len(), 4, "只该多出一个键：{got}");
    }

    /// 覆盖 `messages`/`model` 必须 panic。两个各测一次 —— 只测一个的话
    /// 另一个从白名单里掉出去也不会有人知道。
    #[test]
    fn forbidden_keys_panic() {
        for k in EXTRA_FORBIDDEN {
            let mut e = serde_json::Map::new();
            e.insert((*k).to_string(), serde_json::json!("恶意值"));
            let r = std::panic::catch_unwind(|| {
                LlmClient::with_extra("http://x", "k", "f", "p", e.clone())
            });
            assert!(r.is_err(), "`{k}` 出现在 llm_extra_body 里必须 panic，不许静默接受");
        }
        // 正常键不许被误拦（否则上面那条可以靠「一律 panic」通过）
        let mut ok = serde_json::Map::new();
        ok.insert("enable_thinking".into(), serde_json::Value::Bool(false));
        let _ = LlmClient::with_extra("http://x", "k", "f", "p", ok);
    }

    // ---------------- 流式（K2 流式问答）----------------

    /// 流式 body = 非流式三键 + stream/stream_options；extra 照常合并，
    /// 且 extra 抢不走流式开关（`stream` 本就在 forbidden 名单，`stream_options` 后写死）。
    #[test]
    fn stream_body_adds_stream_keys_on_top_of_build_body() {
        let mut e = serde_json::Map::new();
        e.insert("enable_thinking".into(), serde_json::Value::Bool(false));
        let got = build_stream_body("m1", "sys", "usr", Some(0.1), &e);
        assert_eq!(got["stream"], serde_json::json!(true));
        assert_eq!(got["stream_options"], serde_json::json!({ "include_usage": true }));
        assert_eq!(got["model"], "m1");
        assert_eq!(got["enable_thinking"], serde_json::json!(false));
        assert_eq!(got["messages"][0]["content"], "sys");
    }

    /// 行解析形态钉死：增量 / [DONE] / usage / 该跳过的（空行、event:、注释、半行坏 JSON、
    /// 无 content 的角色块）。增量与 usage 同块时增量优先（丢用量只是观测缺格，丢正文是答案缺字）。
    #[test]
    fn parse_stream_line_shapes() {
        assert_eq!(
            parse_stream_line("data: {\"choices\":[{\"delta\":{\"content\":\"报销\"}}]}"),
            StreamLine::Delta("报销".into())
        );
        assert_eq!(parse_stream_line("data: [DONE]"), StreamLine::Done);
        // CRLF 尾巴 / OWS 都吃掉
        assert_eq!(parse_stream_line("data:[DONE]\r"), StreamLine::Done);
        assert_eq!(
            parse_stream_line("data: {\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3},\"choices\":[]}"),
            StreamLine::Usage(dms_kernel::llm::Usage { prompt_tokens: 7, completion_tokens: 3 })
        );
        // 增量与 usage 同块：增量赢
        assert_eq!(
            parse_stream_line(
                "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}],\"usage\":{\"prompt_tokens\":1}}"
            ),
            StreamLine::Delta("x".into())
        );
        for ignored in [
            "",
            "event: message",
            ": ping",
            "data:",
            "data: {not json",
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}",
            "data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}",
        ] {
            assert_eq!(parse_stream_line(ignored), StreamLine::Ignore, "该行该跳过：{ignored:?}");
        }
    }

    /// 行装配边界：一条事件跨多块、一块多行、CRLF、流尾无换行、
    /// 中文字符跨块切开（\n 切分不受多字节影响）。
    #[test]
    fn sse_lines_split_across_chunks() {
        let mut lines = SseLines::default();
        // 中文「报」= E6 8A A5，故意从中间切开喂
        assert_eq!(lines.feed(b"data: {\"a\":\"\xE6").unwrap(), Vec::<String>::new());
        assert_eq!(lines.feed(b"\x8A\xA5\"}\r\nda").unwrap(), vec!["data: {\"a\":\"报\"}".to_string()]);
        assert_eq!(lines.feed(b"ta: 1\ndata: 2\n").unwrap(), vec!["data: 1".to_string(), "data: 2".to_string()]);
        assert_eq!(lines.finish(), None);
        // 流尾残余（无换行收尾）
        assert_eq!(lines.feed(b"data: tail").unwrap(), Vec::<String>::new());
        assert_eq!(lines.finish(), Some("data: tail".to_string()));
        assert_eq!(lines.finish(), None, "残余只吐一次");
    }

    /// 【双供应商】热切换：下一次调用用新配置（保存即生效）；forbidden 键在运行时
    /// 也拒（`Err` 不是 panic —— 那是运行时路径）；**拒绝后旧配置还在**（不许切一半）。
    #[test]
    fn hot_swap_takes_effect_and_never_halves() {
        let conf = |url: &str, key: &str, f: &str, p: &str, v: Option<&str>| Conf {
            provider: "test".into(),
            base_url: url.into(),
            api_key: key.into(),
            model_fast: f.into(),
            model_precise: p.into(),
            extra: serde_json::Map::new(),
            vision: v.map(str::to_string),
        };
        let c = LlmClient::with_conf(conf("https://a", "k1", "f1", "p1", None));
        assert_eq!(c.public_conf().0, "https://a");
        c.set_conf(conf("https://b", "k2", "f2", "p2", Some("v2"))).unwrap();
        let (url, f, p, v, _) = c.public_conf();
        assert_eq!((url.as_str(), f.as_str(), p.as_str(), v.as_deref()), ("https://b", "f2", "p2", Some("v2")));
        assert_eq!(c.vision_model().as_deref(), Some("v2"));
        // forbidden 键：运行时返回 Err，且旧配置一个字段都没动
        let mut bad = serde_json::Map::new();
        bad.insert("model".into(), serde_json::Value::Null);
        let mut bad_conf = conf("https://c", "k3", "f3", "p3", None);
        bad_conf.extra = bad;
        assert!(c.set_conf(bad_conf).is_err());
        assert_eq!(c.public_conf().0, "https://b", "拒了一半也算事故：旧配置必须完整在");
    }

    #[test]
    fn vision_prefers_primary_and_uses_one_runtime_snapshot() {
        let conf = |provider: &str, vision: Option<&str>| Conf {
            provider: provider.into(),
            base_url: "https://example.invalid/v1".into(),
            api_key: "key".into(),
            model_fast: "fast".into(),
            model_precise: "precise".into(),
            extra: serde_json::Map::new(),
            vision: vision.map(str::to_string),
        };
        let client = LlmClient::with_conf_and_fallback(
            conf("deepseek", None),
            Some(conf("qwen", Some("qwen-vl"))),
        );
        let (_, fallback) = client.vision_route().unwrap();
        assert_eq!((fallback.provider.as_str(), fallback.model.as_str(), fallback.fallback), ("qwen", "qwen-vl", true));

        client
            .set_runtime_configs(
                conf("qwen-primary", Some("qwen-primary-vl")),
                Some(conf("qwen-backup", Some("qwen-backup-vl"))),
            )
            .unwrap();
        let (_, primary) = client.vision_route().unwrap();
        assert_eq!(
            (primary.provider.as_str(), primary.model.as_str(), primary.fallback),
            ("qwen-primary", "qwen-primary-vl", false)
        );
    }

    #[test]
    fn primary_and_fallback_vision_hot_switch_independently() {
        let conf = |provider: &str, vision: Option<&str>| Conf {
            provider: provider.into(),
            base_url: "https://example.invalid/v1".into(),
            api_key: "key".into(),
            model_fast: "fast".into(),
            model_precise: "precise".into(),
            extra: serde_json::Map::new(),
            vision: vision.map(str::to_string),
        };
        let client = LlmClient::with_conf_and_fallback(
            conf("deepseek", None),
            Some(conf("qwen", Some("qwen-vl"))),
        );

        // 默认文本模型没有 vision：使用独立配置的备用千问。
        let (_, route) = client.vision_route().unwrap();
        assert_eq!(
            (route.provider.as_str(), route.model.as_str(), route.fallback),
            ("qwen", "qwen-vl", true)
        );

        // 只热切默认模型；备用配置不动。主模型有 vision 后必须直接走主模型。
        client
            .set_conf(conf("qwen-primary", Some("qwen-primary-vl")))
            .unwrap();
        let (_, route) = client.vision_route().unwrap();
        assert_eq!(
            (route.provider.as_str(), route.model.as_str(), route.fallback),
            ("qwen-primary", "qwen-primary-vl", false)
        );

        // 再切回纯文本模型，备用千问应立即恢复接管；不需要重启或重设备用项。
        client.set_conf(conf("deepseek-next", None)).unwrap();
        let (_, route) = client.vision_route().unwrap();
        assert_eq!(
            (route.provider.as_str(), route.model.as_str(), route.fallback),
            ("qwen", "qwen-vl", true)
        );

        // 备用模型也可单独热换与清除，且不改变当前文本模型。
        client
            .set_fallback_vision(Some(conf("qwen-backup", Some("qwen-vl-plus"))))
            .unwrap();
        let (_, route) = client.vision_route().unwrap();
        assert_eq!(
            (route.provider.as_str(), route.model.as_str(), route.fallback),
            ("qwen-backup", "qwen-vl-plus", true)
        );
        assert_eq!(client.primary_provider(), "deepseek-next");
        client.set_fallback_vision(None).unwrap();
        assert!(matches!(
            client.vision_route(),
            Err(VisionError::Unavailable)
        ));
        assert_eq!(client.primary_provider(), "deepseek-next");
    }

    #[test]
    fn rejected_fallback_hot_swap_keeps_the_previous_route() {
        let conf = |provider: &str, vision: Option<&str>| Conf {
            provider: provider.into(),
            base_url: "https://example.invalid/v1".into(),
            api_key: "key".into(),
            model_fast: "fast".into(),
            model_precise: "precise".into(),
            extra: serde_json::Map::new(),
            vision: vision.map(str::to_string),
        };
        let client = LlmClient::with_conf_and_fallback(
            conf("deepseek", None),
            Some(conf("qwen", Some("qwen-vl"))),
        );
        assert!(client
            .set_fallback_vision(Some(conf("not-vision", None)))
            .is_err());
        let (_, route) = client.vision_route().unwrap();
        assert_eq!(
            (route.provider.as_str(), route.model.as_str(), route.fallback),
            ("qwen", "qwen-vl", true),
            "校验失败时旧备用路由必须完整保留"
        );
    }

    #[test]
    fn failed_settings_commit_restores_the_whole_runtime_snapshot() {
        let conf = |provider: &str, vision: Option<&str>| Conf {
            provider: provider.into(),
            base_url: "https://example.invalid/v1".into(),
            api_key: "key".into(),
            model_fast: "fast".into(),
            model_precise: "precise".into(),
            extra: serde_json::Map::new(),
            vision: vision.map(str::to_string),
        };
        let client = LlmClient::with_conf_and_fallback(
            conf("old-primary", None),
            Some(conf("old-vision", Some("old-vl"))),
        );
        assert!(client
            .commit_runtime_configs(
                conf("new-primary", Some("new-vl")),
                None,
                || anyhow::bail!("simulated settings write failure"),
            )
            .is_err());
        assert_eq!(client.primary_provider(), "old-primary");
        assert_eq!(client.fallback_vision_provider().as_deref(), Some("old-vision"));
        let (_, route) = client.vision_route().unwrap();
        assert_eq!((route.provider.as_str(), route.model.as_str(), route.fallback), ("old-vision", "old-vl", true));
    }

    #[test]
    fn data_image_validation_checks_shape_and_url_size() {
        assert_eq!(validate_vision_image("data:image/png;base64,TQ=="), Ok(()));
        assert_eq!(validate_vision_image("https://images.example.com/a.png?sig=x"), Ok(()));
        assert_eq!(validate_vision_image("https://user:secret@images.example.com/a.png"), Err(VisionError::InvalidImage));
        assert_eq!(validate_vision_image("https://"), Err(VisionError::InvalidImage));
        assert_eq!(
            validate_vision_image("data:text/plain;base64,TQ=="),
            Err(VisionError::InvalidImage)
        );
        assert_eq!(
            validate_vision_image("data:image/png;base64,@@=="),
            Err(VisionError::InvalidImage)
        );
        assert_eq!(
            validate_vision_image("data:image/png;base64,TR=="),
            Err(VisionError::InvalidImage),
            "字符合法但填充位非零的 Base64 也必须拒绝"
        );
        assert_eq!(validate_vision_image_len(MAX_VISION_IMAGE_URL_BYTES), Ok(()));
        assert_eq!(
            validate_vision_image_len(MAX_VISION_IMAGE_URL_BYTES + 1),
            Err(VisionError::ImageTooLarge)
        );
    }

    #[test]
    fn config_rejects_nested_credentials_and_authenticated_urls() {
        let conf = |base_url: &str, extra: serde_json::Map<String, serde_json::Value>| Conf {
            provider: "test".into(),
            base_url: base_url.into(),
            api_key: "key".into(),
            model_fast: "fast".into(),
            model_precise: "precise".into(),
            extra,
            vision: None,
        };
        let mut nested = serde_json::Map::new();
        nested.insert("headers".into(), serde_json::json!({ "Authorization": "secret" }));
        assert!(validate_conf(&conf("https://example.invalid/v1", nested), false).is_err());
        assert!(validate_conf(&conf("https://user:secret@example.invalid/v1", serde_json::Map::new()), false).is_err());
        assert!(validate_conf(&conf("https://example.invalid/v1?token=secret", serde_json::Map::new()), false).is_err());
        let mut reserved = serde_json::Map::new();
        reserved.insert("stream".into(), serde_json::json!(true));
        assert!(validate_provider_shape(&conf("https://example.invalid/v1", reserved)).is_err());
    }

    #[test]
    fn extracts_fenced_sql() {
        let s = "好的：\n```sql\nSELECT 1 FROM t\n```\n说明";
        assert_eq!(extract_sql(s).unwrap(), "SELECT 1 FROM t");
    }

    #[test]
    fn extracts_bare_select() {
        assert_eq!(extract_sql("SELECT a FROM b;").unwrap(), "SELECT a FROM b");
    }

    #[test]
    fn none_when_no_sql() {
        assert!(extract_sql("我不知道").is_none());
    }

    /// 用量解析：缺段一律 0（供应商不回 usage 时不能把这次问答判失败）
    #[test]
    fn usage_missing_fields_are_zero() {
        let u = read_usage(&serde_json::json!({ "prompt_tokens": 1200, "completion_tokens": 80 }));
        assert_eq!((u.prompt_tokens, u.completion_tokens), (1200, 80));
        let z = read_usage(&serde_json::Value::Null);
        assert_eq!((z.prompt_tokens, z.completion_tokens), (0, 0));
    }

    /// 🔴 用量不许被丢。这里曾经是 `usage: Default::default()`（全 0）：T9 之后**所有** LLM 调用
    /// 都走 `ChatModel`，那个 0 就是 `meta.query_log` 的 token 列恒空 —— 而它不报错、不返回错误、
    /// 也没有任何运行时断言能碰到（真 usage 只有连上供应商才拿得到）。故用源码守。
    #[test]
    fn chat_model_never_drops_usage() {
        // CRLF 检出的工作树上 `include_str!` 带 \r：先剥掉再切分，判据不对行尾敏感
        let src = include_str!("llm.rs").replace('\r', "");
        // 只扫非测试段（否则本测试写的 needle 会让自己恒绿——哑测试，裁决 二·F F2）。
        // 锚点是测试模块声明而不是第一个 #[cfg(test)]：单测辅助构造器也带这个属性，
        // 咬属性会把生产段切没（实测：usage 断言因此假红）
        let body = src.split("#[cfg(test)]\nmod tests").next().expect("测试模块必然存在");
        let code: Vec<&str> =
            body.lines().filter(|l| !l.trim_start().starts_with("//")).collect();
        assert!(
            !code.iter().any(|l| l.contains("Default::default()")),
            "usage 被丢成全 0 了：查询日志的 token 列会静默变空"
        );
        // 分两个锚：单行 `"usage }"` 绑死在 rustfmt 输出形状上，字段一重排/换行就假红
        assert!(
            code.iter().any(|l| l.contains("ChatReply {")),
            "ChatReply 构造点没了 —— 锚要跟着实现一起改"
        );
        assert!(code.iter().any(|l| l.contains("usage")), "真 usage 必须原样进 ChatReply");
    }

    /// ChatModel 适配：system 必须落到 system 位，别把它拼进 user（提示词顺序是契约）
    #[test]
    fn chat_model_splits_system_and_user() {
        let r = dms_kernel::ChatRequest::text(dms_kernel::ModelTier::Fast, "你是内核", "问句", None);
        assert_eq!(split_roles(&r.messages), ("你是内核".into(), "问句".into()));
        assert_eq!(split_roles(&[]), (String::new(), String::new()));
    }

    /// 首条 system 内容为空串时，第二条 system 不许静默顶位（显式 `seen_system` 标志）
    #[test]
    fn split_roles_first_system_wins_even_when_empty() {
        let msgs = vec![
            dms_kernel::Message { role: "system".into(), content: String::new() },
            dms_kernel::Message { role: "system".into(), content: "S2".into() },
            dms_kernel::Message { role: "user".into(), content: "U".into() },
        ];
        let (system, user) = split_roles(&msgs);
        assert_eq!(system, "");
        assert!(user.contains("S2") && user.contains("U"), "{user}");
    }

    /// base_url 带尾斜杠/首尾空白：写入时归一化（请求不会打到 `//chat/completions`），
    /// 且校验看到的就是使用的那份（校验与使用同源）。
    #[test]
    fn base_url_is_normalized_on_store() {
        let conf = |url: &str| Conf {
            provider: "test".into(),
            base_url: url.into(),
            api_key: "k".into(),
            model_fast: "f".into(),
            model_precise: "p".into(),
            extra: serde_json::Map::new(),
            vision: None,
        };
        let c = LlmClient::with_conf(conf("https://example.invalid/v1/"));
        assert_eq!(c.public_conf().0, "https://example.invalid/v1");
        c.set_conf(conf(" https://example.invalid/v2// ")).unwrap();
        assert_eq!(c.public_conf().0, "https://example.invalid/v2");
    }

    /// persist 闭包 panic 会毒化锁：恢复警卫继续服务，不许此后所有 LLM 调用连锁 panic。
    #[test]
    fn poisoned_runtime_lock_recovers() {
        let conf = |p: &str| Conf {
            provider: p.into(),
            base_url: "https://example.invalid/v1".into(),
            api_key: "k".into(),
            model_fast: "f".into(),
            model_precise: "p".into(),
            extra: serde_json::Map::new(),
            vision: None,
        };
        let client = LlmClient::with_conf(conf("old"));
        let c2 = client.clone();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = c2.commit_runtime_configs(conf("new"), None, || panic!("persist boom"));
        }));
        assert!(r.is_err());
        // panic 发生在回滚之前：接受「新配置在内存、落库未成」的半截状态，换服务不挂
        assert_eq!(client.primary_provider(), "new");
        client.set_conf(conf("next")).unwrap();
        assert_eq!(client.primary_provider(), "next");
    }

    /// 空 content（或剥完思考段只剩空白）按「无内容」：不再以空串流到 `extract_sql`。
    #[test]
    fn empty_content_counts_as_missing() {
        let empty = serde_json::json!({"choices": [{"message": {"content": ""}}]});
        assert!(content_text(&empty).is_none());
        let thinking_only = serde_json::json!({"choices": [{"message": {"content": "<think>想</think>  "}}]});
        assert!(content_text(&thinking_only).is_none());
        let ok = serde_json::json!({"choices": [{"message": {"content": " SELECT 1 "}}]});
        assert_eq!(content_text(&ok).as_deref(), Some("SELECT 1"));
    }

    /// scheme 与 data mediatype 都大小写不敏感（WHATWG URL / RFC 2397）
    #[test]
    fn vision_image_schemes_are_case_insensitive() {
        assert_eq!(validate_vision_image("HTTPS://images.example.com/a.png"), Ok(()));
        assert_eq!(validate_vision_image("DATA:IMAGE/PNG;BASE64,TQ=="), Ok(()));
        // 大写 scheme 同样不放行带认证的 URL
        assert_eq!(
            validate_vision_image("HTTPS://user:secret@images.example.com/a.png"),
            Err(VisionError::InvalidImage)
        );
    }

    /// 配置形状报错必须带命中键名（键名不敏感，排障不该靠猜）
    #[test]
    fn provider_shape_error_names_the_offending_key() {
        let mut extra = serde_json::Map::new();
        extra.insert("api_key".into(), serde_json::json!("x"));
        let conf = Conf {
            provider: "t".into(),
            base_url: "https://example.invalid".into(),
            api_key: "k".into(),
            model_fast: "f".into(),
            model_precise: "p".into(),
            extra,
            vision: None,
        };
        let err = validate_provider_shape(&conf).unwrap_err().to_string();
        assert!(err.contains("api_key"), "报错没带命中键名：{err}");
    }
}
