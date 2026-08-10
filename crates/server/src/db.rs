//! 配置加载。**建池能力已全部搬去 connector**（`ReadOnlyMySql::connect` / `OwnedStore::connect`）：
//! 全仓唯一能造连接池的地方只剩那里，本文件只剩 dsn 明文的来源（`mysql_url` / `pg_url`）。
//!
//! 【D1】凭据引用化 + AES-GCM 密文存储：**落盘态密文、内存态明文**。
//! 敏感字段清单在 `encrypt_sensitive_fields`（新增含凭据的配置键先改那里 + docs/CONFIG.md）；
//! 加解密原语在 `crypto` 子模块；Python 判官链的镜像在 `tools/settings.py`。

pub mod crypto;

use anyhow::Context;
use serde::Deserialize;

// 不派生 Debug：值里是完整 DSN，后续任何 `{:?}` 都可能把账号密码写进日志。
#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub enum MysqlTarget {
    Legacy(String),
    Detailed {
        url: String,
        #[serde(default, rename = "type")]
        kind: String,
        /// 数据源级查询策略（可选）：单次取数行上限/超时，与全局两档取 min（A8）
        #[serde(default)]
        max_rows: Option<usize>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
}

impl MysqlTarget {
    pub fn url(&self) -> &str {
        match self {
            Self::Legacy(url) | Self::Detailed { url, .. } => url,
        }
    }

    /// 数据源级查询策略（缺省 = 不收紧）。只可能更紧：`DsPolicy::clamp` 取 min。
    pub fn policy(&self) -> dms_connector::DsPolicy {
        match self {
            Self::Legacy(_) => dms_connector::DsPolicy::default(),
            Self::Detailed { max_rows, timeout_ms, .. } => dms_connector::DsPolicy {
                max_rows: *max_rows,
                timeout: timeout_ms.map(std::time::Duration::from_millis),
            },
        }
    }

    pub fn capability(&self) -> dms_connector::mysql::MysqlCapability {
        match self {
            Self::Detailed { kind, .. }
                if kind.eq_ignore_ascii_case("doris") || kind.eq_ignore_ascii_case("warehouse") =>
            {
                dms_connector::mysql::MysqlCapability::Warehouse
            }
            _ => dms_connector::mysql::MysqlCapability::ProductionLookup,
        }
    }

    /// 与 DMS 权限源复用同一端点时，只接受结构化配置中明确写出的生产点查能力。
    /// 旧字符串与未知类型虽按最小权限运行，但不能据此推断用户有意复用生产端点。
    pub fn is_explicit_production_lookup(&self) -> bool {
        matches!(
            self,
            Self::Detailed { kind, .. } if kind.eq_ignore_ascii_case("production_lookup")
        )
    }
}

/// `deny_unknown_fields`：**键名打错必须启动失败**（裁决 二·AS3）。
/// 曾经的行为：`"mcp_key"` 少写一个 s → serde 静默丢弃 → `mcp_keys` 为空 → `/api/mcp` 恒 404，
/// 零报错零告警，运维只能看到「对外集成怎么都连不上」。
/// 静默关掉一个功能，比启动时一句「unknown field `mcp_key`, expected one of ...」坏得多；
/// 口径也与 `tools/regression.py` 的 `KNOWN` 白名单硬失败一致。
/// 代价：老 settings.json 里的多余键会变成启动失败 —— 那正是想要的，报文里带全量已知键清单（有断言锁着）。
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub mysql_url: String,
    pub pg_url: String,
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub llm_base_url: String,
    #[serde(default)]
    pub llm_api_key: String,
    #[serde(default)]
    pub llm_model_fast: String,
    #[serde(default)]
    pub llm_model_precise: String,
    /// 合并进每次 `chat/completions` 请求体的**额外字段**（供应商特有参数）。
    ///
    /// 🔴 为什么是配置而不是代码：千问（dashscope compatible-mode）默认**带思考**，
    /// 实测同一道带口径卡的 SQL 题 `enable_thinking:false` 是 780ms / 65 tokens，
    /// 不关是 16626ms / 2281 tokens，而**产出的 SQL 质量一样**（判据 4/4 都命中）。
    /// 也就是说不关它就是白付 21 倍延迟和 35 倍 token。
    /// 但这个字段是千问私有的，写死进 body 会带到 DeepSeek 上去 —— 那边可能 400。
    /// 做成配置项，换供应商只改 settings.json，代码一行不动。
    ///
    /// `messages`/`model` 两个键**禁止**出现在这里（`LlmClient::new` 会 panic）：
    /// 能覆盖 `messages` 等于配置文件可以做任意提示注入，能覆盖 `model` 等于
    /// fast/precise 两档形同虚设，而两者都不会报错、只会静默改变行为。
    #[serde(default)]
    pub llm_extra_body: serde_json::Map<String, serde_json::Value>,
    /// 当前供应商（`provider_catalog` 的键：qwen | deepseek）。缺省按 `llm_base_url` 推断
    /// （dashscope→qwen、deepseek→deepseek、其它→按文件值当自定义供应商）。
    /// **运行时切换不在这里**：`meta.kv['llm_provider']`（页面上保存即生效）覆盖本键。
    #[serde(default)]
    pub llm_provider: String,
    /// 主供应商没有视觉模型时使用的备用供应商。这里只保存供应商名；key 仍只从
    /// `llm_keys[供应商名]` 解析，不复制、不入库、不进响应。
    #[serde(default)]
    pub fallback_vision_provider: String,
    /// 各供应商的 key（`llm_provider` 切到哪家取哪家的）。`llm_api_key` 是**当前文件供应商**
    /// 那家的旧键位，等价于 `llm_keys[当前供应商]`——两个键位共存时以 `llm_keys` 为准。
    /// ⚠️ 红线同 DSN：只在 settings.json，不入库、不进日志、不进任何响应。
    #[serde(default)]
    pub llm_keys: std::collections::HashMap<String, String>,
    /// 【查询库热切换】可切换的 MySQL/Doris 目标目录：name → DSN。
    /// `dms` 是保留名且会被过滤；`mysql_url` 只服务身份、角色和数据权限读取，不会隐式
    /// 进入目录。结构化目标显式写 `type=production_lookup` 时可与 `mysql_url` 复用端点，
    /// 但只能走生产轻点查能力；旧字符串和数仓目标仍会被过滤。
    /// 至少配置一个非 `dms` 目标，启动优先 `meta.kv['mysql_target']`，其次
    /// `doris_warehouse`，再取目录首项；任何失败都不得回退到 `mysql_url`。
    /// ⚠️ 红线同 `mysql_url`：DSN 只在 settings.json，不入库、不进日志、不进任何响应 ——
    /// `meta.kv['mysql_target']` 只存**名字**，API 响应只给脱敏 host。
    #[serde(default)]
    pub mysql_targets: std::collections::HashMap<String, MysqlTarget>,
    /// 【自定义 LLM 供应商】页面/手工加的供应商（内建目录 `provider_catalog` 之外的）：
    /// name → 连接形状。key 仍在 `llm_keys`（红线同一条：明文只住这个文件）。
    #[serde(default)]
    pub llm_providers: std::collections::HashMap<String, CustomProvider>,
    #[serde(default)]
    pub dms_base_url: String,
    #[serde(default)]
    pub wework_corpid: String,
    #[serde(default)]
    pub wework_secret: String,
    #[serde(default)]
    pub wework_agentid: String,
    /// 企业微信 OAuth 的精确回调地址，例如 `https://agent.example.com/api/wework/login`。
    /// 不从 Host/X-Forwarded-* 推导，避免反向代理主机头污染 OAuth 重定向。
    #[serde(default)]
    pub wework_redirect_url: String,
    /// Python 侧服务地址：`/embed` 与 `/parse`、`/chunk` 同进程同端口（裁决 V1）。
    /// **一个键，不许拆两个**——拆了必然出现「一个填 /embed 一个填根」的配置陷阱。
    #[serde(default = "default_service_url")]
    pub service_url: String,
    /// 知识库落盘根目录（原名不进路径，文件名恒 `<doc_id>.<ext>`）
    #[serde(default = "default_kb_root")]
    pub kb_root: String,
    /// 单个上传文件上限（MB）；同时是 `/api/kb/upload` 的 body limit（axum 默认 2MB 会先触发）
    #[serde(default = "default_kb_max_mb")]
    pub kb_max_mb: u64,
    /// 【Y3】RRF 四路辅助召回（元数据/关系扩展/图谱/外部 KB）的融合权重。
    /// **缺省 = retrieve 原编译期常量**（0.2/0.25/0.3/0.2，逐路字节级等价，有单测钉着）。
    /// 负值与 NaN/Inf 在启动加载与页面保存两处都被拒绝（`RrfWeights::validate`，报错不 clamp）；
    /// `0` = 该路不加权。保存即生效：检索/问答链在每次请求取 `st.cfg()` 快照。
    /// ⚠️ 例外：`/api/ask` 主链（`answerers::knowledge`）暂用默认值 —— 它的调用点在
    /// main.rs，本包未接线（见该文件的 Y3 注释）。
    #[serde(default)]
    pub kb_rrf_weights: dms_knowledge::retrieve::RrfWeights,
    /// 上传表格源（K4 的 `up_*` schema）的**只读**连接串。
    /// 必须是一个**读不到 `meta`/`kb`/`chat` 的 PG 角色**——`PostgresSource::connect` 会自检，
    /// 拿 owner 角色填这里会被拒绝启动（F3）：那等于让 LLM 产的 SQL 能读全员文档与他人问答。
    /// 不填则上传源无法问数（建表与知识库检索照常），错误文案会指回这里。
    #[serde(default)]
    pub pg_ro_url: Option<String>,
    /// 额外数据源：`dsn_ref` 键名 → 明文连接串。登记数据源时 `dsn_ref` 填这里的键名。
    #[serde(default)]
    pub datasources: std::collections::HashMap<String, String>,
    /// 【K6-A】对外 MCP 的 key → login_name。**不填 = `/api/mcp` 关闭（恒 404）**，
    /// 老 settings.json 免改。一 key 一员工，轮换＝改配置重启。
    /// ⚠️ 明文 key 与 DSN 同级敏感：不入库、不进日志、不进任何响应
    ///（`mcp_api` 记日志只写前 4 位+长度）。
    #[serde(default)]
    pub mcp_keys: std::collections::HashMap<String, String>,
    /// 【SC】自一致采样数：LLM 路径整条跑几次、按**结果指纹**投票取多数派。
    ///
    /// **默认 1 = 与本项引入前逐字等价**（不多一次 LLM 调用、不多一次取数）。
    /// 3 是常用值：实测两轮评测都停在 34/38 而失败集换了两个 —— 同一道题今天与 gold 逐值一致、
    /// 评测那次却高 30%，误差主要来自模型本身（温度已是 0.1）。
    /// ⚠️ 代价是**线性的**：3 就是最多 3 倍 precise LLM 调用 + 3 倍取数
    /// （前两次指纹一致会提前收工，故常见情形只多付一次）。B10 那类单次 24s 的题要留意。
    #[serde(default = "default_sc_samples")]
    pub sc_samples: usize,
    /// 【AI 解读】`POST /api/analysis` 是否真的调 fast 模型。**默认 true**。
    ///
    /// 为什么敢默认开：解读是**独立端点**（前端点「AI 解读」才调），取数链路一次 LLM 都不多。
    /// 评测与回归走 CLI `ask` 与 `/api/ask`，**结构上根本不经过那个端点** ——
    /// 那条 p95 基线（本轮实测 28~42s）不会被污染，也不依赖任何人记得把开关关掉。
    /// 置 false = 止血阀（模型欠费/被限流时只返确定性口径说明，零 LLM 花费），前端不用改。
    #[serde(default = "default_true")]
    pub insight_enabled: bool,
    /// 🔴 **安全开关：无会话 token 时是否采信请求自报的 `login_name`。默认 `false`。**
    ///
    /// 关掉之前的行为（`resolve_identity` 的 `None` 分支无条件采信 `ln`）等于**没有认证**：
    /// 能连到 `listen` 端口的人写一个 `?login_name=<别人>` 就以那个人的身份跑 ——
    /// 读全公司数据、读他人知识库块原文、往他人空间写文档、冒充管理员改术语与登记数据源
    /// （`is_admin(&p)` 读的是 DMS 库里的**真** flag，冒充成 admin 的 login 后它返回 true）。
    /// 而实测 `settings.docker.json` 的 `listen` 是 `0.0.0.0:8100` 且容器映射到 `0.0.0.0`
    /// —— 那是对整个局域网开放。
    ///
    /// 为什么留这个开关而不是直接删掉回退：三个判官脚本走 HTTP 带 `login_name`
    /// （`judge_scope.py` / `kb_eval.py` / `up_probe.py`），直接删会让它们全部 401。
    /// **默认 `false`** 意味着新部署与忘了配的部署都是安全的；要开必须显式写进 settings.json，
    /// 且启动时 `warn!` 留痕一次。
    ///
    /// ⚠️ 它**不是**「本机才生效」——开了就是对所有能到达端口的来源生效。
    /// 端口收窄（docker 映射改 `127.0.0.1:8100:8100`）是另一道独立防线，两道都该做。
    #[serde(default)]
    pub insecure_login_fallback: bool,
}

/// 1 = 关（见 `sc_samples` 的文档）
fn default_sc_samples() -> usize {
    1
}

/// 见 `insight_enabled` 的文档：默认开，且这个默认对评测是结构性安全的
fn default_true() -> bool {
    true
}

impl Settings {
    /// 这些目录的名称在 API 与运行时都按大小写不敏感匹配；若 JSON 同时出现
    /// `qwen`/`QWEN`，HashMap 迭代顺序会让实际命中项不确定，必须在进入运行时前拒绝。
    pub fn validate_named_catalogs(&self) -> anyhow::Result<()> {
        fn unique<T>(label: &str, map: &std::collections::HashMap<String, T>) -> anyhow::Result<()> {
            let mut seen = std::collections::HashSet::new();
            if map.keys().any(|name| !seen.insert(name.to_ascii_lowercase())) {
                anyhow::bail!("{label} 包含仅大小写不同的重复名称");
            }
            Ok(())
        }
        unique("mysql_targets", &self.mysql_targets)?;
        unique("llm_keys", &self.llm_keys)?;
        unique("llm_providers", &self.llm_providers)?;
        Ok(())
    }

    /// 【D1】内存态解密：`enc:v1:` 前缀的敏感字段就地换回明文（无前缀原样 —— 明文兼容）。
    /// 只对「刚从文件读进来的」Settings 调；此后进程内流转全是明文，红线语义与明文时代一致。
    /// 失败（错钥匙/密文损坏）响亮报出**字段名** —— 永远不带值片段。
    pub fn decrypt_secrets(&mut self) -> anyhow::Result<()> {
        let (key, _) = crypto::default_key();
        self.decrypt_secrets_with(&key)
    }

    /// 指定钥匙版（单测用固定钥匙，不碰环境变量）。
    pub fn decrypt_secrets_with(&mut self, key: &[u8; 32]) -> anyhow::Result<()> {
        fn dec(key: &[u8; 32], field: &str, s: &mut String) -> anyhow::Result<()> {
            if crypto::is_encrypted(s) {
                *s = crypto::decrypt_with(key, s)
                    .map_err(|e| anyhow::anyhow!("settings 敏感字段 {field} 解密失败（{e}）"))?;
            }
            Ok(())
        }
        dec(key, "mysql_url", &mut self.mysql_url)?;
        dec(key, "pg_url", &mut self.pg_url)?;
        dec(key, "llm_api_key", &mut self.llm_api_key)?;
        dec(key, "wework_secret", &mut self.wework_secret)?;
        if let Some(s) = &mut self.pg_ro_url {
            dec(key, "pg_ro_url", s)?;
        }
        for (name, v) in &mut self.llm_keys {
            let name = name.clone();
            dec(key, &format!("llm_keys.{name}"), v)?;
        }
        for (name, v) in &mut self.datasources {
            let name = name.clone();
            dec(key, &format!("datasources.{name}"), v)?;
        }
        for (name, target) in &mut self.mysql_targets {
            let name = name.clone();
            match target {
                MysqlTarget::Legacy(url) => dec(key, &format!("mysql_targets.{name}"), url)?,
                MysqlTarget::Detailed { url, .. } => {
                    dec(key, &format!("mysql_targets.{name}.url"), url)?
                }
            }
        }
        // mcp_keys 的**键名**本身是凭据（值只是 login_name，非密）：整表重建，
        // 解密后撞名说明配置自相矛盾，响亮失败而不是静默丢一个员工的 key。
        let old = std::mem::take(&mut self.mcp_keys);
        for (k, v) in old {
            let dk = if crypto::is_encrypted(&k) {
                crypto::decrypt_with(key, &k)
                    .map_err(|e| anyhow::anyhow!("settings 敏感字段 mcp_keys 的键名解密失败（{e}）"))?
            } else {
                k
            };
            if self.mcp_keys.insert(dk, v).is_some() {
                anyhow::bail!("mcp_keys 解密后出现重复键名（配置冲突）");
            }
        }
        Ok(())
    }

    /// 消费式封装：`settings_api` 的回读校验链需要「校验过的那份再解密」一步。
    pub fn decrypted(mut self) -> anyhow::Result<Self> {
        self.decrypt_secrets()?;
        Ok(self)
    }


    /// `dsn_ref` → 明文 DSN 的映射（`SourceRegistry` 的唯一入参）。
    /// 键名就是 `meta.datasource.dsn_ref` 的取值，两处对不上表现为「测试连接说 dsn_ref 未配置」。
    ///
    /// ⚠️ 本文件里的明文机密有两处：这里的 DSN 与上面的 `mcp_keys`。两者同级——
    /// 都只在进程内被当查找表用，**任何日志/响应/错误文案里都不许出现明文**。
    pub fn dsn_map(&self) -> std::collections::HashMap<String, String> {
        // 【D1】读取侧透明解密：内存 cfg 正常已是明文，前缀闸让这步零成本；
        // 真撞上密文也按进程钥匙解，解不开原样放行 —— 建池时会以 DSN 形状错误响亮失败。
        let decrypt = |s: &str| crypto::decrypt_auto(s).unwrap_or_else(|_| s.to_string());
        let mut m: std::collections::HashMap<String, String> = self
            .datasources
            .iter()
            .map(|(name, url)| (name.clone(), decrypt(url)))
            .collect();
        // `mysql_url` 是 DMS 身份/角色/权限专用连接，绝不进入通用数据源注册表。
        // 主分析源会在 main.rs 启动时从非 dms 的 mysql_targets 建池并 preload；若 preload
        // 缺席，`dsn_ref=mysql_url` 应响亮失败，不能懒连接回权限库。
        m.retain(|name, _| {
            !name.eq_ignore_ascii_case("mysql_url") && !name.eq_ignore_ascii_case("dms")
        });
        let auth_endpoint = endpoint_key(&self.mysql_url);
        if !auth_endpoint.is_empty() {
            m.retain(|_, url| endpoint_key(url) != auth_endpoint);
        }
        // 注意：**不把 `pg_url` 放进去**。它是 OwnedStore 的 owner 角色（可写、能看见 meta/kb/chat），
        // 一旦有人给某个数据源填 `dsn_ref: "pg_url"`，LLM 的 SQL 就能读全员文档——
        // 让它在「dsn_ref 未配置」上失败，比让它连上更安全。
        if let Some(ro) = &self.pg_ro_url {
            m.insert(dms_semantic::registry::datasource::UPLOAD_DSN_REF.into(), decrypt(ro));
        }
        m
    }
}

fn default_service_url() -> String {
    "http://127.0.0.1:8077".into()
}

fn default_kb_root() -> String {
    "data/kb".into()
}

fn default_kb_max_mb() -> u64 {
    // 产品口径：单文件 ≤20MB（对齐上传全清单裁决；前端预校验与服务端 classify 同此值）
    20
}

fn default_listen() -> String {
    "127.0.0.1:8100".into()
}

// ───────────────── 【D1】敏感字段清单与落盘加密 ─────────────────
//
// 哪些键算敏感**只有这一处事实源**（新增含凭据的配置键：改这里 + `Settings::decrypt_secrets`
// + `tools/settings.py` 的镜像 + docs/CONFIG.md，四处一起动）。
/// 顶层字符串字段：值整体是凭据
const SECRET_SCALARS: &[&str] = &["mysql_url", "pg_url", "pg_ro_url", "llm_api_key", "wework_secret"];
/// 顶层 map：每个**值**是凭据（键名是引用名，非密）
const SECRET_MAP_VALUES: &[&str] = &["llm_keys", "datasources"];

/// settings.json 落盘前的幂等加密：只动敏感字段、只动明文
/// （`enc:v1:` 前缀与空串原样，所以重复跑收敛 —— 启动迁移靠这个幂等）。
/// 返回是否有字段被改写（false = 文件已是全密文，无需写盘）。
pub fn encrypt_sensitive_fields(v: &mut serde_json::Value) -> anyhow::Result<bool> {
    let (key, _) = crypto::default_key();
    encrypt_sensitive_fields_with(v, &key)
}

/// 指定钥匙版（单测用固定钥匙，不碰环境变量）。
pub fn encrypt_sensitive_fields_with(
    v: &mut serde_json::Value,
    key: &[u8; 32],
) -> anyhow::Result<bool> {
    fn enc(key: &[u8; 32], s: &mut String) -> anyhow::Result<bool> {
        // 空串/已密文的判定收口在 `encrypt_if_plain_with`（单一事实源）
        let sealed =
            crypto::encrypt_if_plain_with(key, s).map_err(|e| anyhow::anyhow!("敏感字段加密失败（{e}）"))?;
        if sealed == *s {
            return Ok(false);
        }
        *s = sealed;
        Ok(true)
    }
    let mut changed = false;
    for field in SECRET_SCALARS {
        if let Some(serde_json::Value::String(s)) = v.get_mut(*field) {
            changed |= enc(key, s)?;
        }
    }
    for field in SECRET_MAP_VALUES {
        if let Some(serde_json::Value::Object(m)) = v.get_mut(*field) {
            for val in m.values_mut() {
                if let serde_json::Value::String(s) = val {
                    changed |= enc(key, s)?;
                }
            }
        }
    }
    // mysql_targets：旧字符串形态 / 结构化 {url} 形态都要顾
    if let Some(serde_json::Value::Object(m)) = v.get_mut("mysql_targets") {
        for target in m.values_mut() {
            match target {
                serde_json::Value::String(s) => changed |= enc(key, s)?,
                serde_json::Value::Object(o) => {
                    if let Some(serde_json::Value::String(s)) = o.get_mut("url") {
                        changed |= enc(key, s)?;
                    }
                }
                _ => {}
            }
        }
    }
    // mcp_keys 的**键名**本身是凭据（同 `decrypt_secrets` 的镜像）：整表重建
    if let Some(serde_json::Value::Object(m)) = v.get_mut("mcp_keys") {
        let entries: Vec<(String, serde_json::Value)> =
            m.iter().map(|(k, val)| (k.clone(), val.clone())).collect();
        let mut rebuilt = serde_json::Map::with_capacity(entries.len());
        for (k, val) in entries {
            let nk = if k.is_empty() || crypto::is_encrypted(&k) {
                k
            } else {
                changed = true;
                crypto::encrypt_with(key, &k)
                    .map_err(|e| anyhow::anyhow!("敏感字段加密失败（{e}）"))?
            };
            rebuilt.insert(nk, val);
        }
        *m = rebuilt;
    }
    Ok(changed)
}

pub fn load_settings() -> anyhow::Result<Settings> {
    let (p, s) = find_settings_path()
        .ok_or_else(|| anyhow::anyhow!("settings.json 未找到（参考 settings.example.json）"))?;
    // 钥匙来源只在这里留痕一次（key 本体永不进日志）：机器指纹兜底有「跨机不可迁移」的
    // 运维语义（见 crypto::machine_fingerprint），不 warn 一声运维换机时只能猜。
    match crypto::default_key().1 {
        crypto::KeySource::Env => {}
        crypto::KeySource::EnvShort => {
            tracing::warn!("DMS_SECRET_KEY 少于 32 字节：熵不足，请换 ≥32 字节的随机串")
        }
        crypto::KeySource::Machine => tracing::warn!(
            "未配置 DMS_SECRET_KEY：settings 凭据密钥由机器指纹派生，settings.json 跨机/跨用户/容器重建后不可迁移（换机需重填凭据）；生产与 docker 部署请配置 ≥32 字节的 DMS_SECRET_KEY"
        ),
    }
    let mut v: serde_json::Value = serde_json::from_str(&s)
        .with_context(|| format!("{p} 不是合法 JSON"))?;
    // ① 幂等迁移：明文敏感字段 → enc:v1: 密文（已是密文的原样，重复启动收敛）
    let migrated = encrypt_sensitive_fields(&mut v)
        .with_context(|| format!("{p} 敏感字段加密失败"))?;
    // ② 完整校验先于任何写盘（与 settings_api 同一条纪律：写出启动不了的文件比不写更坏）
    let mut settings: Settings = serde_json::from_value(v.clone())
        .with_context(|| format!("{} 解析失败（键名打错会在此硬失败，见 docs/CONFIG.md）", p))?;
    settings
        .validate_named_catalogs()
        .with_context(|| format!("{} 目录名称冲突", p))?;
    // 【Y3】RRF 权重闸：负值/NaN/Inf 在启动加载与页面保存（settings_api）两处同一拒绝口径
    settings
        .kb_rrf_weights
        .validate()
        .map_err(|e| anyhow::anyhow!("{} 的 kb_rrf_weights 无效：{}", p, e))?;
    if migrated {
        // 落盘纪律同 settings_api：正式文件**原地单次写**（bind mount 单文件挂载点不许
        // rename）。此刻是启动路径、尚无并发写者，不需要运行期那道 settings_write 锁。
        let out = serde_json::to_string_pretty(&v)
            .with_context(|| format!("{p} 序列化失败"))?;
        if std::fs::write(&p, &out).is_err() {
            // 文件只读（挂载/ro 权限）：本次以内存态照常运行，下次启动再试迁移 ——
            // 宁可明文多住一晚，不让只读挂载把服务挡在门外。
            tracing::warn!("settings 敏感字段已加密但写回 {p} 失败（只读挂载？）：本次照常运行，下次启动重试迁移");
        } else {
            tracing::info!("settings.json 敏感字段已加密落盘（enc:v1，幂等迁移完成）");
        }
    }
    // ③ 内存态一律明文：读取侧（建池/LLM/企微/MCP）零改动。解不开 = 钥匙变了/密文损坏，
    // 响亮失败指回 DMS_SECRET_KEY —— 不静默拿密文去连库。
    settings
        .decrypt_secrets()
        .with_context(|| format!("{p} 敏感字段解密失败（DMS_SECRET_KEY 是否变更？换机/换用户后机器指纹钥匙会失效，需重填明文凭据）"))?;
    Ok(settings)
}

/// 就近找 settings.json：优先当前目录，其次仓库根（cargo run 时 cwd=仓库根）。
/// 返回（展示路径, 内容）；展示路径就是 `deny_unknown_fields` 报错时第一个要回答的问题
/// （容器里挂载点是 /app/settings.json）。`settings_api` 写回时也走这同一个定位。
pub fn find_settings_path() -> Option<(String, String)> {
    for p in ["settings.json", "../settings.json", "../../settings.json"] {
        if let Ok(s) = std::fs::read_to_string(p) {
            return Some((p.to_string(), s));
        }
    }
    None
}

// ───────────────── 供应商目录（qwen / deepseek 双供应商热切换）─────────────────

/// 目录里的一家。**只有供应商事实**（base_url / 模型名 / 视觉能力 / 额外参数默认值）——
/// key 不在这里（红线：明文 key 只住 settings.json）。
pub struct ProviderSpec {
    pub base_url: &'static str,
    pub model_fast: &'static str,
    pub model_precise: &'static str,
    /// 目录默认的 extra_body（**JSON 文本**，解析进请求体）。千问的 `enable_thinking`
    /// 是布尔；DeepSeek 的思考开关是**嵌套对象** `{"thinking":{"type":"disabled"}}` ——
    /// 布尔装不下，所以这一位是 JSON 文本而不是值（非法 JSON 在 `resolve_provider` 里 panic，
    /// 那是常量写错，不是运行时错误）。
    pub extra: &'static str,
    /// 视觉模型名（`None` = 没有图片识别能力）。千问 flash 自己就是视觉模型
    /// （实测 988ms 三题全对）；DeepSeek 全系没有视觉接口。
    pub vision: Option<&'static str>,
}

/// 内建供应商目录。**加供应商 = 加一行 + settings.json 的 `llm_keys` 给 key**，代码不动第二处。
pub fn provider_catalog() -> &'static [(&'static str, ProviderSpec)] {
    &[
        ("qwen", ProviderSpec {
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
            model_fast: "qwen3.7-flash",
            model_precise: "qwen3.7-flash",
            // 实测（`Settings::llm_extra_body` 注释里的账）：不关思考 = 21 倍延迟 35 倍 token，SQL 质量一样
            extra: r#"{"enable_thinking": false}"#,
            vision: Some("qwen3.7-flash"),
        }),
        ("deepseek", ProviderSpec {
            base_url: "https://api.deepseek.com",
            model_fast: "deepseek-v4-flash",
            model_precise: "deepseek-v4-pro",
            // 🔴 DeepSeek 思考模式**默认开**（官方文档：`thinking` 默认 enabled、effort=high）。
            // 这里默认关，两条理由，速度只是其次：
            // ① 思考模式下 `temperature`/`top_p` **不生效**（官方原文「设置了也不会生效」）——
            //    它会静默拆掉本系统的三条机制：首轮 0.1 确定性（金文件/语义缓存）、
            //    重试 0.5 分档（温度 0.1 的重试 = 同一个错误再来一遍）、SC 投票的样本独立性；
            // ② CoT 每次生成都多一段思维链，延迟与 token 成倍（千问同族实测 21x/35x）。
            // 想开思考：settings.json 的 `llm_extra_body` 覆盖目录默认（文件供应商路径）。
            extra: r#"{"thinking": {"type": "disabled"}}"#,
            vision: None,
        }),
    ]
}

/// 自定义供应商的连接形状（`llm_providers` 的值）。厂商参数统一放 `extra_body`；
/// 连接字段拼错必须硬失败，不能在页面看似保存成功后直到切换才暴露。
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct CustomProvider {
    pub base_url: String,
    #[serde(default)]
    pub model_fast: String,
    #[serde(default)]
    pub model_precise: String,
    /// 思考级别固化产物（「关」= 各家的关法，见 `llm_presets` 的 thinking_off）
    #[serde(default)]
    pub extra_body: serde_json::Map<String, serde_json::Value>,
    /// 多模态模型名（`None` = 无视觉能力）
    #[serde(default)]
    pub vision: Option<String>,
}

/// 预设厂商目录（2026-08 互联网核实，OpenAI 兼容端点）：用户下拉即填好
/// url/模型/思考关法/多模态，只剩 key 要手填。**改预设只许改这里**（页面读的
/// 也是这份 —— 前端没有第二份目录）。
#[derive(Debug)]
pub struct PresetProvider {
    pub label: &'static str,
    pub base_url: &'static str,
    pub model_fast: &'static str,
    pub model_precise: &'static str,
    /// 「思考关」的 extra_body（SQL 生成的默认值 —— AX57 的账：思考对 SQL 不提质，
    /// 只加延迟与 token，还静默废掉 temperature）
    pub thinking_off: Option<&'static str>,
    /// 「思考低/高」的 extra_body（没有 = 这家没有思考档位）
    pub thinking_low: Option<&'static str>,
    pub thinking_high: Option<&'static str>,
    /// 多模态模型名（`None` = 无视觉能力）
    pub vision: Option<&'static str>,
}

pub fn llm_presets() -> &'static [(&'static str, PresetProvider)] {
    &[
        ("qwen", PresetProvider {
            label: "阿里·千问（dashscope）",
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
            model_fast: "qwen3.7-flash",
            model_precise: "qwen3.7-plus",
            thinking_off: Some(r#"{"enable_thinking": false}"#),
            thinking_low: None,
            thinking_high: Some(r#"{"enable_thinking": true}"#),
            vision: Some("qwen3.7-flash"),
        }),
        ("deepseek", PresetProvider {
            label: "DeepSeek",
            base_url: "https://api.deepseek.com",
            model_fast: "deepseek-v4-flash",
            model_precise: "deepseek-v4-pro",
            thinking_off: Some(r#"{"thinking": {"type": "disabled"}}"#),
            thinking_low: Some(r#"{"reasoning_effort": "low"}"#),
            thinking_high: Some(r#"{"reasoning_effort": "high"}"#),
            vision: None,
        }),
        ("glm", PresetProvider {
            label: "智谱 GLM（bigmodel）",
            base_url: "https://open.bigmodel.cn/api/paas/v4",
            model_fast: "glm-4-flash",
            model_precise: "glm-4-plus",
            thinking_off: None,
            thinking_low: None,
            thinking_high: None,
            vision: Some("glm-4v-plus"),
        }),
        ("kimi", PresetProvider {
            label: "Kimi（moonshot）",
            base_url: "https://api.moonshot.cn/v1",
            model_fast: "kimi-k2.6",
            model_precise: "kimi-k3",
            thinking_off: None,
            thinking_low: None,
            thinking_high: None,
            vision: Some("kimi-k2.6"),
        }),
        ("doubao", PresetProvider {
            label: "豆包（火山方舟）",
            base_url: "https://ark.cn-beijing.volces.com/api/v3",
            model_fast: "doubao-seed-1-6-flash-250828",
            model_precise: "doubao-seed-1-6-250615",
            thinking_off: Some(r#"{"thinking": {"type": "disabled"}}"#),
            thinking_low: None,
            thinking_high: Some(r#"{"thinking": {"type": "enabled"}}"#),
            vision: Some("doubao-seed-1-6-250615"),
        }),
        ("openai", PresetProvider {
            label: "OpenAI",
            base_url: "https://api.openai.com/v1",
            model_fast: "gpt-4o-mini",
            model_precise: "gpt-4o",
            thinking_off: None,
            thinking_low: None,
            thinking_high: None,
            vision: Some("gpt-4o"),
        }),
    ]
}

/// `llm_base_url` → 供应商名（settings.json 老配置不带 `llm_provider` 键时的兼容推断）。
pub fn infer_provider(base_url: &str) -> Option<&'static str> {
    let u = base_url.to_lowercase();
    if u.contains("dashscope.aliyuncs.com") {
        return Some("qwen");
    }
    if u.contains("api.deepseek.com") {
        return Some("deepseek");
    }
    None
}

/// settings 文件里的基础供应商。运行时可由 `meta.kv` 切到别家，但这一项仍是旧式
/// `llm_base_url` / `llm_api_key` 字段的归属，删除保护与 key 清理必须识别它。
pub fn file_provider_name(cfg: &Settings) -> String {
    if cfg.llm_provider.is_empty() {
        infer_provider(&cfg.llm_base_url).unwrap_or("custom").to_string()
    } else {
        cfg.llm_provider.clone()
    }
}

/// 只返回“该供应商是否具备非空凭据”，不返回凭据本身。名称比较大小写不敏感，避免
/// settings 文件历史大小写差异导致运行时可解析、设置页却错误禁用切换按钮。
pub fn provider_key_ready(cfg: &Settings, provider: &str) -> bool {
    cfg.llm_keys
        .iter()
        .any(|(name, key)| name.eq_ignore_ascii_case(provider) && !key.trim().is_empty())
        || (file_provider_name(cfg).eq_ignore_ascii_case(provider)
            && !cfg.llm_api_key.trim().is_empty())
}

/// 【查询库热切换】目标目录只包含 `mysql_targets` 的非 dms 项。
/// `mysql_url` 不会被隐式加入目录；显式声明为 `production_lookup` 的目标可复用其端点，
/// 但仍由生产点查的单表/索引/2s/50 行闸门执行。数仓或旧字符串不能复用该端点。
/// 返回 (名字, DSN)。**DSN 不出本函数** —— 任何响应/日志只许给 `mask_dsn` 的产物。
pub fn db_targets(cfg: &Settings) -> Vec<(String, String)> {
    let mut v = Vec::new();
    // 【D1】读取侧透明解密（同 `dsn_map` 的注释：正常零成本，密文兜底解）。
    // 只解**出参**的 DSN；端点比较用的 `mysql_url` 不出目录，保持原文读取。
    let decrypt = |s: &str| crypto::decrypt_auto(s).unwrap_or_else(|_| s.to_string());
    let auth_endpoint = endpoint_key(&cfg.mysql_url);
    for (k, target) in &cfg.mysql_targets {
        let url = target.url();
        let same_as_permission_source =
            !auth_endpoint.is_empty() && endpoint_key(url) == auth_endpoint;
        let allowed_same_endpoint =
            same_as_permission_source && target.is_explicit_production_lookup();
        if !k.eq_ignore_ascii_case("dms")
            && (!same_as_permission_source || allowed_same_endpoint)
        {
            v.push((k.clone(), decrypt(url)));
        }
    }
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

pub fn db_target_capability(
    cfg: &Settings,
    name: &str,
) -> dms_connector::mysql::MysqlCapability {
    cfg.mysql_targets
        .iter()
        .find(|(target, _)| target.eq_ignore_ascii_case(name))
        .map(|(_, target)| target)
        .map(MysqlTarget::capability)
        .unwrap_or(dms_connector::mysql::MysqlCapability::ProductionLookup)
}

/// DSN → 脱敏展示（`host:port/db`，**绝不带用户/口令** —— 红线同 settings.json 本体）。
/// 解析失败给空串：宁可什么都不显示，不把疑似口令的片段漏出去。
pub fn mask_dsn(url: &str) -> String {
    // mysql://user:pass@host:3306/db?params
    let after_scheme = match url.split_once("://") {
        Some((_, r)) => r,
        None => return String::new(),
    };
    let host_part = match after_scheme.rsplit_once('@') {
        Some((_, h)) => h,
        None => return String::new(),
    };
    let host_db: String = host_part.split(&['?', '#', ' '][..]).next().unwrap_or("").to_string();
    if host_db.is_empty() {
        return String::new();
    }
    host_db
}

/// 管理页展示模型地址时去掉 userinfo/query/fragment，防止把误写在 URL 里的 token 返回浏览器。
pub fn public_service_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return String::new();
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return String::new();
    }
    let host_path = rest.rsplit_once('@').map(|(_, tail)| tail).unwrap_or(rest);
    let clean = host_path.split(&['?', '#', ' '][..]).next().unwrap_or("");
    if clean.is_empty() {
        String::new()
    } else {
        format!("{scheme}://{clean}")
    }
}

/// 只用于“是否与 DMS 权限源同一物理服务器”的保守比较；不含凭据，且忽略主机大小写。
/// 数据库名刻意不参与：同一 MySQL 实例换个 schema 仍会共享 CPU/IO，不能借此把生产库
/// 伪装成 warehouse 后执行复杂分析。
fn endpoint_key(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return String::new();
    };
    let host = rest.rsplit_once('@').map(|(_, host)| host).unwrap_or(rest);
    let host_db = host
        .split(&['?', '#', ' '][..])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    let authority = host_db.split_once('/').map(|(authority, _)| authority).unwrap_or(host_db);
    if authority.is_empty() {
        return String::new();
    }
    let authority = if authority.starts_with('[') {
        match authority.find(']') {
            Some(end) if authority.get(end + 1..end + 2) == Some(":") => {
                let Ok(port) = authority[end + 2..].parse::<u16>() else {
                    return String::new();
                };
                format!("{}:{port}", &authority[..=end])
            }
            Some(end) => format!("{}:3306", &authority[..=end]),
            None => return String::new(),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => {
                let Ok(port) = port.parse::<u16>() else {
                    return String::new();
                };
                format!("{host}:{port}")
            }
            _ => format!("{authority}:3306"),
        }
    };
    authority.to_ascii_lowercase()
}

pub fn same_db_endpoint(a: &str, b: &str) -> bool {
    let a = endpoint_key(a);
    !a.is_empty() && a == endpoint_key(b)
}

#[cfg(test)]
mod db_target_tests {
    use dms_connector::mysql::MysqlCapability;

    /// 脱敏：host:port/db 留下，用户/口令一个字符都不许出现
    #[test]
    fn mask_dsn_strips_credentials() {
        let m = super::mask_dsn("mysql://root:p%3Asecret@203.0.113.10:3306/dms?charset=utf8mb4");
        assert_eq!(m, "203.0.113.10:3306/dms");
        assert!(!m.contains("root") && !m.contains("secret") && !m.contains("p%3A"), "{m}");
        // 口令里带 @ 的（rsplit_once 从右切）
        let m2 = super::mask_dsn("mysql://u:p@ss@10.0.0.1/db2");
        assert_eq!(m2, "10.0.0.1/db2");
        assert!(!m2.contains("p@ss"), "{m2}");
        // 畸形输入给空串（不给疑似口令的片段）
        assert_eq!(super::mask_dsn("不是DSN"), "");
        assert_eq!(super::mask_dsn("mysql://noat"), "");
        assert!(super::same_db_endpoint(
            "mysql://auth:a@DB.EXAMPLE/xh_dms",
            "mysql://other:b@db.example:03306/XH_DMS?charset=utf8mb4",
        ));
        assert!(super::same_db_endpoint(
            "mysql://auth:a@db.example:3306/xh_dms",
            "mysql://warehouse:b@DB.EXAMPLE/another_schema",
        ), "同一实例换 schema 仍是同一生产服务器");
        assert_eq!(
            super::public_service_url("https://user:token@api.example/v1?key=hidden#x"),
            "https://api.example/v1",
        );
    }

    /// 权限源不隐式进入查询目录；只有显式 production_lookup 可复用同端点。
    #[test]
    fn db_targets_only_allow_explicit_production_lookup_on_auth_endpoint() {
        let cfg: super::Settings = serde_json::from_str(
            r#"{"mysql_url":"mysql://u:p@1.2.3.4/dms","pg_url":"postgres://x",
               "mysql_targets":{"zhongtai":"mysql://u:p@10.0.0.2/zt","DMS":"mysql://u:p@10.0.0.3/dup",
               "renamed_dms":"mysql://readonly:other@1.2.3.4:3306/dms",
               "business_lookup":{"url":"mysql://point:readonly@1.2.3.4:3306/dms","type":"production_lookup"},
               "fake_warehouse":{"url":"mysql://point:readonly@1.2.3.4:3306/dms","type":"warehouse"}},
               "datasources":{"MYSQL_URL":"mysql://u:p@9.9.9.9/other","dms":"mysql://u:p@10.0.0.3/dup",
               "renamed_dms":"mysql://readonly:other@1.2.3.4/dms","other":"mysql://u:p@10.0.0.4/other"}}"#,
        )
        .unwrap();
        let v = super::db_targets(&cfg);
        assert_eq!(
            v.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
            vec!["business_lookup", "zhongtai"],
            "只有显式 production_lookup 可复用权限源端点：{v:?}",
        );
        assert!(v.iter().all(|(name, _)| !name.eq_ignore_ascii_case("dms")));
        let dsn = cfg.dsn_map();
        assert_eq!(dsn.len(), 1, "通用注册表也必须过滤权限源别名：{dsn:?}");
        assert!(dsn.contains_key("other"));
    }

    #[test]
    fn warehouse_capability_requires_explicit_catalog_type() {
        let cfg: super::Settings = serde_json::from_str(
            r#"{"mysql_url":"mysql://u:p@1.2.3.4/dms","pg_url":"postgres://x",
               "mysql_targets":{
                 "legacy_9030":"mysql://u:p@10.0.0.2:9030/legacy",
                 "plain":{"url":"mysql://u:p@10.0.0.3:3306/plain"},
                 "production":{"url":"mysql://u:p@10.0.0.6:3306/dms","type":"production_lookup"},
                 "doris":{"url":"mysql://u:p@10.0.0.4:9030/warehouse","type":"doris"},
                 "warehouse":{"url":"mysql://u:p@10.0.0.5:9030/warehouse","type":"warehouse"}
               }}"#,
        )
        .unwrap();
        assert_eq!(super::db_target_capability(&cfg, "legacy_9030"), MysqlCapability::ProductionLookup);
        assert_eq!(super::db_target_capability(&cfg, "plain"), MysqlCapability::ProductionLookup);
        assert_eq!(super::db_target_capability(&cfg, "production"), MysqlCapability::ProductionLookup);
        assert_eq!(super::db_target_capability(&cfg, "doris"), MysqlCapability::Warehouse);
        assert_eq!(super::db_target_capability(&cfg, "warehouse"), MysqlCapability::Warehouse);
        assert_eq!(super::db_target_capability(&cfg, "DORIS"), MysqlCapability::Warehouse);
        assert_eq!(super::db_target_capability(&cfg, "missing"), MysqlCapability::ProductionLookup);
        assert!(cfg.mysql_targets["production"].is_explicit_production_lookup());
        assert!(!cfg.mysql_targets["legacy_9030"].is_explicit_production_lookup());
        assert!(!cfg.mysql_targets["plain"].is_explicit_production_lookup());
        assert!(!cfg.mysql_targets["doris"].is_explicit_production_lookup());
    }
}

/// 解析**当前该生效的** LLM Conf：目录条目打底；`name` 就是文件供应商（或 custom）时
/// settings.json 的文件值覆盖目录 —— 老配置等价于「文件供应商 + 文件自定义参数」。
/// 切到**另一家**时它的 base_url/模型名必须来自目录（否则就是「deepseek 的地址配千问的
/// 模型名」的混搭）。key 取 `llm_keys[name]`，`llm_api_key` 只对文件供应商兜底
/// （那正是老配置的语义）；都没有 → 响亮报错，不静默落空。
pub fn resolve_provider(name: &str, cfg: &Settings) -> anyhow::Result<crate::llm::Conf> {
    let file_provider_name = file_provider_name(cfg);
    let file_provider = file_provider_name.as_str();
    let file_values_apply = name.eq_ignore_ascii_case(file_provider);
    let key_for = |provider: &str| {
        cfg.llm_keys
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(provider))
            .map(|(_, value)| value.clone())
    };
    // 【自定义覆盖内建】`llm_providers` 同名条目**优先于**内建目录 —— 内建形状是代码常量，
    // 页面改不了；想调内建的模型名/地址/思考档，存一条同名自定义即可（删除即还原）。
    if let Some((provider_name, c)) = cfg
        .llm_providers
        .iter()
        .find(|(provider, _)| (*provider).eq_ignore_ascii_case(name))
    {
        let (mf, mp) = (
            if c.model_fast.is_empty() { c.model_precise.clone() } else { c.model_fast.clone() },
            if c.model_precise.is_empty() { c.model_fast.clone() } else { c.model_precise.clone() },
        );
        let api_key = key_for(provider_name)
            .or_else(|| file_values_apply.then(|| cfg.llm_api_key.clone()))
            .unwrap_or_default();
        // 【D1】读取侧透明解密：enc:v1: 密文按进程钥匙解回（内存 cfg 正常已是明文，前缀闸零成本）
        let api_key = crypto::decrypt_auto(&api_key)
            .map_err(|e| anyhow::anyhow!("供应商 {name} 的 key 解密失败（{e}）：DMS_SECRET_KEY 是否变更？"))?;
        if api_key.is_empty() {
            anyhow::bail!("供应商 {name} 的 key 不在 settings.json —— 在 llm_keys 里加（key 只在 settings.json，不入库）");
        }
        return Ok(crate::llm::Conf {
            provider: provider_name.clone(),
            base_url: c.base_url.trim_end_matches('/').to_string(),
            api_key,
            model_fast: mf,
            model_precise: mp,
            extra: c.extra_body.clone(),
            vision: c.vision.clone(),
        });
    }
    let spec = provider_catalog()
        .iter()
        .find(|(provider, _)| (*provider).eq_ignore_ascii_case(name));
    let from_catalog = |field: &str, default: &str| {
        if field.is_empty() { default.to_string() } else { field.to_string() }
    };
    let (provider_name, base_url, fast, precise, extra, vision) = match spec {
        Some((provider, s)) if !file_values_apply => (
            (*provider).to_string(),
            s.base_url.to_string(),
            s.model_fast.to_string(),
            s.model_precise.to_string(),
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(s.extra)
                .unwrap_or_else(|e| panic!("provider_catalog 的 extra 不是合法 JSON 对象（常量写错）: {e}")),
            s.vision.map(str::to_string),
        ),
        Some((provider, s)) => (
            (*provider).to_string(),
            from_catalog(&cfg.llm_base_url.trim_end_matches('/').to_string(), s.base_url),
            from_catalog(&cfg.llm_model_fast, s.model_fast),
            from_catalog(&cfg.llm_model_precise, s.model_precise),
            cfg.llm_extra_body.clone(),
            s.vision.map(str::to_string),
        ),
        None if name.eq_ignore_ascii_case("custom") || file_values_apply => (
            if file_values_apply { file_provider.to_string() } else { "custom".to_string() },
            cfg.llm_base_url.trim_end_matches('/').to_string(),
            cfg.llm_model_fast.clone(),
            cfg.llm_model_precise.clone(),
            cfg.llm_extra_body.clone(),
            None,
        ),
        None => anyhow::bail!("未知供应商 {name}（目录：内建 qwen | deepseek、`llm_providers` 自定义、custom=settings.json 文件值）"),
    };
    let api_key = key_for(&provider_name)
        .or_else(|| file_values_apply.then(|| cfg.llm_api_key.clone()))
        .unwrap_or_default();
    // 【D1】读取侧透明解密（同上：密文兜底解，明文零成本）
    let api_key = crypto::decrypt_auto(&api_key)
        .map_err(|e| anyhow::anyhow!("供应商 {name} 的 key 解密失败（{e}）：DMS_SECRET_KEY 是否变更？"))?;
    if api_key.is_empty() {
        anyhow::bail!(
            "供应商 {name} 的 key 不在 settings.json —— 在 `llm_keys` 里加 \"{name}\": \"sk-…\"（key 只在 settings.json，不入库）"
        );
    }
    if base_url.is_empty() {
        anyhow::bail!("供应商 {name} 没有 base_url（settings.json 的 llm_base_url 为空，目录里也没有它）");
    }
    Ok(crate::llm::Conf {
        provider: provider_name,
        base_url,
        api_key,
        model_fast: fast,
        model_precise: precise,
        extra,
        vision,
    })
}

/// 解析配置的备用视觉供应商。空字符串表示未配置；非空时必须能解析、具备 key 且声明
/// 视觉模型，否则保存/启动后的视觉能力查询都会给出明确错误，不会复制主供应商凭据。
pub fn resolve_fallback_vision(
    cfg: &Settings,
) -> anyhow::Result<Option<(String, crate::llm::Conf)>> {
    let name = cfg.fallback_vision_provider.trim();
    if name.is_empty() {
        return Ok(None);
    }
    let conf = resolve_provider(name, cfg)?;
    if conf.vision.as_deref().map(str::trim).filter(|m| !m.is_empty()).is_none() {
        anyhow::bail!("备用多模态供应商 {name} 没有配置 vision 模型");
    }
    Ok(Some((name.to_string(), conf)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: &str = r#"{"mysql_url":"mysql://x","pg_url":"postgres://x"}"#;

    /// 上传单文件上限的产品口径是 20MB：默认值钉死，配置缺省时前端预校验与服务端同值
    #[test]
    fn kb_upload_default_cap_is_20mb() {
        assert_eq!(default_kb_max_mb(), 20);
        let cfg: Settings = serde_json::from_str(MIN).unwrap();
        assert_eq!(cfg.kb_max_mb, 20);
    }

    /// 【Y3】kb_rrf_weights：缺省 = retrieve 旧编译期常量（与旧字面量逐路字节级等价的
    /// 证明件在 dms-knowledge 侧 `rrf_weights_default_is_byte_equivalent_to_legacy_consts`，
    /// 这里钉 Settings 层语义）—— 缺键=旧值、部分覆盖只改给了的路、负值 validate 拒、
    /// 键名打错连嵌套结构也硬失败。
    #[test]
    fn kb_rrf_weights_defaults_validate_and_reject_bad_values() {
        let cfg: Settings = serde_json::from_str(MIN).unwrap();
        assert_eq!(cfg.kb_rrf_weights, dms_knowledge::retrieve::RrfWeights::default());
        assert!(cfg.kb_rrf_weights.validate().is_ok());
        let partial: Settings = serde_json::from_str(r#"{"mysql_url":"mysql://x","pg_url":"postgres://x",
            "kb_rrf_weights":{"kg":0.5}}"#)
        .unwrap();
        assert_eq!(partial.kb_rrf_weights.kg, 0.5);
        assert_eq!(partial.kb_rrf_weights.metadata, 0.2, "没给的路必须仍是旧值");
        let negative: Settings = serde_json::from_str(r#"{"mysql_url":"mysql://x","pg_url":"postgres://x",
            "kb_rrf_weights":{"ext_kb":-0.1}}"#)
        .unwrap();
        assert!(negative.kb_rrf_weights.validate().is_err(), "负值必须被 validate 拒");
        assert!(serde_json::from_str::<Settings>(r#"{"mysql_url":"mysql://x","pg_url":"postgres://x",
            "kb_rrf_weights":{"kgg":0.5}}"#)
        .is_err(), "嵌套键名打错也必须硬失败（deny_unknown_fields）");
    }

    /// 【双供应商】目录与解析的六条不变量：推断正确；切到另一家必须用目录的
    /// base_url/模型名（不许混搭文件的）；key 只从 llm_keys/llm_api_key 来；
    /// 视觉能力按供应商分（qwen 有、deepseek 无）；未知供应商与缺 key 都响亮报错。
    #[test]
    fn provider_resolution_keeps_catalog_and_file_apart() {
        assert_eq!(infer_provider("https://dashscope.aliyuncs.com/compatible-mode/v1"), Some("qwen"));
        assert_eq!(infer_provider("https://api.deepseek.com"), Some("deepseek"));
        assert_eq!(infer_provider("https://other.example.com"), None);

        // 文件是千问（老配置形状），切到 deepseek：必须拿目录的地址与模型、llm_keys 的 key，
        // 且**不**带千问的 extra（混搭 = deepseek 收到 enable_thinking 当场 400）
        let file_qwen = r#"{"mysql_url":"mysql://x","pg_url":"postgres://x",
            "llm_base_url":"https://dashscope.aliyuncs.com/compatible-mode/v1",
            "llm_api_key":"sk-file",
            "llm_extra_body":{"enable_thinking":false},
            "llm_keys":{"deepseek":"sk-ds"}}"#;
        let cfg: Settings = serde_json::from_str(file_qwen).unwrap();
        let c = resolve_provider("deepseek", &cfg).unwrap();
        assert_eq!(c.base_url, "https://api.deepseek.com");
        assert_eq!(c.model_fast, "deepseek-v4-flash");
        assert_eq!(c.model_precise, "deepseek-v4-pro");
        assert_eq!(c.api_key, "sk-ds");
        // 切换必须不带文件供应商的 extra；目录给的是 DeepSeek 自己的「关思考」
        // （官方：思考默认开且 temperature 不生效 —— 不关掉它会静默拆掉温度分档机制）
        assert!(!c.extra.contains_key("enable_thinking"), "混搭了千问的参数：{:?}", c.extra);
        assert_eq!(c.extra.get("thinking"), Some(&serde_json::json!({"type": "disabled"})), "{:?}", c.extra);
        assert!(c.vision.is_none(), "deepseek 没有视觉能力 —— 兼容就是靠这个 None 降级");

        // 文件供应商自己：文件值覆盖目录默认（老配置逐字等价），key 允许用 llm_api_key 兜底
        let c2 = resolve_provider("qwen", &cfg).unwrap();
        assert_eq!(c2.base_url, "https://dashscope.aliyuncs.com/compatible-mode/v1");
        assert_eq!(c2.api_key, "sk-file");
        assert_eq!(c2.vision.as_deref(), Some("qwen3.7-flash"), "千问 flash 自己就是视觉模型");
        assert_eq!(c2.extra.get("enable_thinking"), Some(&serde_json::Value::Bool(false)));

        // 未知供应商与「目录里但 key 没配」都必须响亮报错（不静默落空）
        assert!(resolve_provider("openai", &cfg).is_err());
        let no_key = r#"{"mysql_url":"mysql://x","pg_url":"postgres://x",
            "llm_base_url":"https://api.deepseek.com","llm_api_key":"sk-ds"}"#;
        let cfg2: Settings = serde_json::from_str(no_key).unwrap();
        assert!(resolve_provider("qwen", &cfg2).is_err(), "没配 key 的切换必须报错指回 llm_keys");
        // 而 deepseek（文件供应商）用 llm_api_key 兜底能过
        assert!(resolve_provider("deepseek", &cfg2).is_ok());

        // 【自定义覆盖内建】同名 llm_providers 条目优先于内建目录（形状全换；key 仍取 llm_keys）
        let cfg3: Settings = serde_json::from_str(
            r#"{"mysql_url":"mysql://x","pg_url":"postgres://x",
               "llm_keys":{"qwen":"sk-q"},
               "llm_providers":{"qwen":{"base_url":"https://proxy.example.com/v1",
                  "model_fast":"my-fast","model_precise":"my-pro",
                  "extra_body":{"enable_thinking":false},"vision":"my-fast"}}}"#,
        )
        .unwrap();
        let c = resolve_provider("qwen", &cfg3).unwrap();
        assert_eq!(c.base_url, "https://proxy.example.com/v1", "覆盖必须赢内建");
        assert_eq!(c.model_fast, "my-fast");
        assert_eq!(c.api_key, "sk-q", "key 仍取 llm_keys（红线不因为覆盖而变）");
        // 单边模型名互补（fast 空 = 用 precise）；未知供应商仍响亮报错
        let cfg4: Settings = serde_json::from_str(
            r#"{"mysql_url":"mysql://x","pg_url":"postgres://x",
               "llm_keys":{"qwen":"sk-q"},
               "llm_providers":{"qwen":{"base_url":"https://proxy.example.com",
                  "model_fast":"","model_precise":"only-one"}}}"#,
        )
        .unwrap();
        let c = resolve_provider("qwen", &cfg4).unwrap();
        assert_eq!(c.model_fast, "only-one");
        assert_eq!(c.model_precise, "only-one");
        assert!(resolve_provider("不存在", &cfg4).is_err());
    }

    #[test]
    fn custom_provider_rejects_misspelled_connection_fields() {
        let bad = r#"{"mysql_url":"mysql://x","pg_url":"postgres://x",
            "llm_providers":{"custom":{"base_url":"https://api.example/v1",
            "model_fast":"fast","model_precies":"typo"}}}"#;
        assert!(serde_json::from_str::<Settings>(bad).is_err());
    }

    #[test]
    fn case_insensitive_catalog_names_cannot_be_ambiguous() {
        let cfg: Settings = serde_json::from_str(
            r#"{"mysql_url":"mysql://x","pg_url":"postgres://x",
               "llm_keys":{"qwen":"sk-one","QWEN":"sk-two"}}"#,
        )
        .unwrap();
        assert!(cfg.validate_named_catalogs().is_err());

        let ok: Settings = serde_json::from_str(
            r#"{"mysql_url":"mysql://x","pg_url":"postgres://x",
               "llm_keys":{"qwen":"sk-one"},
               "llm_providers":{"DeepSeek":{"base_url":"https://api.example/v1",
                  "model_fast":"fast","model_precise":"precise"}}}"#,
        )
        .unwrap();
        assert!(ok.validate_named_catalogs().is_ok());
    }


    /// 判据①：打错的键必须 Err，且报文能自解（含错键名 + 已知键清单）。
    /// 反面同一条断言里做：正确键集必须 Ok —— 否则「写成恒 Err」也能让上半条绿。
    #[test]
    fn unknown_key_is_hard_failure_with_self_explaining_message() {
        let ok = serde_json::from_str::<Settings>(MIN);
        assert!(ok.is_ok(), "正确的最小键集必须 Ok：{:?}", ok.err());

        let typo = MIN.replace("{", r#"{"mcp_key":{},"#);
        // 用 `.err()` 而不是 `expect_err`：后者要求 `Settings: Debug`，而 Settings 里全是明文机密，
        // **不给它 Debug** —— 一个 `{:?}` 就能把 DSN 与 mcp key 打进日志。
        let e = serde_json::from_str::<Settings>(&typo).err().expect("mcp_key 打错必须 Err");
        let msg = e.to_string();
        assert!(msg.contains("mcp_key"), "报文没说是哪个键错了：{msg}");
        // serde 默认会跟一串 `expected one of ...`；确认它真列了已知键，否则运维看了也不知道该写啥
        assert!(msg.contains("mcp_keys") && msg.contains("mysql_url"), "报文没列已知键清单：{msg}");
    }

    /// 判据②：示例配置的键集 ⊆ Settings 的字段集。
    /// 有了 `deny_unknown_fields`，这一条同时钉住反向的「示例里有个代码不认的键」。
    /// `include_str!` 只在 `#[cfg(test)]` 里出现，`cargo build` 不展开它 ——
    /// 镜像构建只 COPY `Cargo.*` 与 `crates/`，拿不到这个文件（已在容器里实证过）。
    #[test]
    fn example_settings_parses_and_exposes_mcp_keys() {
        let raw = include_str!("../../../settings.example.json");
        // 防恒真：这两个键是 AS5 的全部理由（MCP 对运维不可见 = 永远开不起来；止血阀找不到），
        // 所以断言**原文含键名**而不是断言解析后的值 —— 两者都有 `#[serde(default)]`，
        // 只断言值的话，从示例里删掉这两行照样全绿（实测反向验证：删 mcp_keys / 删 insight_enabled
        // 各让本条 panic 在下面对应那句上）。
        assert!(raw.contains("\"mcp_keys\""), "示例配置缺 mcp_keys —— MCP 功能对运维不可见");
        assert!(raw.contains("\"insight_enabled\""), "示例配置缺 insight_enabled —— 止血阀找不到");
        let s: Settings = serde_json::from_str(raw).expect("settings.example.json 必须能解析成 Settings");
        // 示例里 `mcp_keys` 刻意留空 = 对外默认关。**不许在示例里放一个假 key**：
        // 照抄的人就得到一个可猜的对外凭据（等于 `login_name` 冒充入口）。
        assert!(s.mcp_keys.is_empty(), "示例里的 mcp_keys 必须是空的（对外默认关）");
        // 【Y3】示例里的 RRF 权重必须就是缺省值：照抄示例 = 检索行为零变化（改默认要两边一起想清）
        assert!(raw.contains("\"kb_rrf_weights\""), "示例配置缺 kb_rrf_weights —— 权重口径对运维不可见");
        assert_eq!(
            s.kb_rrf_weights,
            dms_knowledge::retrieve::RrfWeights::default(),
            "示例里的 kb_rrf_weights 必须等于缺省值（照抄示例 = 行为零变化）"
        );
    }

    /// 🔴 **认证回退必须默认关**（二·AU2 的安全修复）。
    ///
    /// 修前 `resolve_identity` 的 `None` 分支无条件采信请求自报的 `login_name` ——
    /// 而实测容器映射是 `0.0.0.0:8100`，也就是局域网内任何人
    /// `curl -d '{"question":"…","login_name":"admin"}'` 就是管理员。
    ///
    /// 判据三层，缺任何一层都能让「写成默认开」溜过去：
    #[test]
    fn login_fallback_defaults_to_closed() {
        // ① 不写这个键时必须是 false（`#[serde(default)]` 对 bool 给 false —— 钉住这件事，
        //    因为把它改成 `default = "default_true"` 是一次 5 个字符的编辑）
        let s: Settings = serde_json::from_str(MIN).unwrap();
        assert!(!s.insecure_login_fallback, "认证回退默认必须是关的");
        // ② 显式写 true 要真的读进来（否则「永远 false」也让 ① 绿，而那会让判官脚本全 401
        //    并被下一个人误判成「服务挂了」）
        let on = MIN.replace("{", r#"{"insecure_login_fallback":true,"#);
        assert!(serde_json::from_str::<Settings>(&on).unwrap().insecure_login_fallback);
        // ③ 示例配置里必须**显式**是 false —— 抄示例的人拿到的是安全的那一档。
        //    断言原文含键名 + 解析值为 false：只断言值的话，示例里删掉这一行照样绿。
        let raw = include_str!("../../../settings.example.json");
        assert!(raw.contains("\"insecure_login_fallback\""), "示例缺这个键 = 运维不知道它存在");
        assert!(
            !serde_json::from_str::<Settings>(raw).unwrap().insecure_login_fallback,
            "示例配置把认证回退设成了开 —— 照抄的人默认无认证"
        );
    }

    /// 🔴 `docker run` 的端口发布面必须收在回环。
    ///
    /// 裸写 `-p 8100:8100` docker 会绑 `0.0.0.0`（实测 `docker ps` 显示
    /// `0.0.0.0:8100->8100/tcp, [::]:8100->8100/tcp`）—— 对整个局域网开放。
    /// 这是与认证开关**独立**的第二道防线：认证开关在本机判官场景下是开着的，
    /// 那时端口收窄就是唯一挡住局域网的东西。
    ///
    /// 用源码守是因为这件事没有运行时判据可打（脚本不在编译单元里）。
    /// 防恒真：同时断言裸写形态**不存在**，否则加一行新的 `-p 8100:8100` 不会红。
    #[test]
    fn docker_publishes_only_on_loopback() {
        let sh = include_str!("../../../scripts/serve.ps1");
        assert!(sh.contains("-p 127.0.0.1:8100:8100"), "serve.ps1 的端口发布面不在回环");
        // 裸写形态只许出现在注释里（上面那段说明提到了它）
        for line in sh.lines() {
            let t = line.trim_start();
            if t.starts_with('#') {
                continue;
            }
            assert!(!t.contains("-p 8100:8100"), "裸写 -p 8100:8100 会绑 0.0.0.0：{line}");
        }
    }
}


/// 【D1】字段级加解密与迁移的判据：清单覆盖、幂等、明文兼容、错钥匙响亮失败、
/// 读取侧（resolve_provider / dsn_map / db_targets）透明解密。
#[cfg(test)]
mod d1_crypto_tests {
    use super::*;

    const K: [u8; 32] = [42u8; 32];

    /// 一份覆盖全部敏感字段形态的样例（含非敏感字段做对照）
    fn sample() -> serde_json::Value {
        serde_json::json!({
            "mysql_url": "mysql://root:p%40ss@10.0.0.1:3306/xh_dms",
            "pg_url": "postgres://postgres:pw@127.0.0.1:15433/dms_ai",
            "pg_ro_url": "postgres://ro:pw@127.0.0.1:15433/dms_ai",
            "listen": "127.0.0.1:8100",
            "llm_base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "llm_api_key": "sk-file-key",
            "llm_keys": {"qwen": "sk-qwen", "deepseek": "sk-ds"},
            "wework_secret": "ww-secret",
            "mysql_targets": {
                "legacy": "mysql://u:p@10.0.0.2:9030/legacy",
                "doris": {"url": "mysql://u:p@10.0.0.3:9030/wh", "type": "doris"}
            },
            "datasources": {"ods": "mysql://u:p@10.0.0.4:3306/ods"},
            "mcp_keys": {"mcp-key-alice": "alice", "mcp-key-bob": "bob"}
        })
    }

    /// 加密后：敏感值全带前缀、非敏感逐字节不动、mcp 的 login_name 不动；
    /// 再跑一遍 changed=false 且逐字节不变（幂等 = 启动重复迁移收敛）
    #[test]
    fn encrypt_sensitive_fields_covers_list_and_is_idempotent() {
        let mut v = sample();
        assert!(encrypt_sensitive_fields_with(&mut v, &K).unwrap());
        for f in ["mysql_url", "pg_url", "pg_ro_url", "llm_api_key", "wework_secret"] {
            assert!(crypto::is_encrypted(v[f].as_str().unwrap()), "{f} 没加密");
        }
        for (n, val) in v["llm_keys"].as_object().unwrap() {
            assert!(crypto::is_encrypted(val.as_str().unwrap()), "llm_keys.{n} 没加密");
        }
        assert!(crypto::is_encrypted(v["datasources"]["ods"].as_str().unwrap()));
        assert!(crypto::is_encrypted(v["mysql_targets"]["legacy"].as_str().unwrap()), "旧字符串目标");
        assert!(crypto::is_encrypted(v["mysql_targets"]["doris"]["url"].as_str().unwrap()), "结构化目标");
        for k in v["mcp_keys"].as_object().unwrap().keys() {
            assert!(crypto::is_encrypted(k), "mcp 键名没加密：{k}");
        }
        // 对照组：非敏感字段与非密部分一个字符都不许动
        assert_eq!(v["listen"], "127.0.0.1:8100");
        assert_eq!(v["llm_base_url"], "https://dashscope.aliyuncs.com/compatible-mode/v1");
        assert_eq!(v["mysql_targets"]["doris"]["type"], "doris");
        assert_eq!(v["llm_keys"].as_object().unwrap().len(), 2, "供应商名（键）不是凭据，不许动");
        assert!(v["llm_keys"].as_object().unwrap().contains_key("qwen"));
        // login_name（值）不是凭据，不许动。键名密文随机 → map 序不定，按值排序后断言
        let mut logins: Vec<String> = v["mcp_keys"]
            .as_object()
            .unwrap()
            .values()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        logins.sort();
        assert_eq!(logins, vec!["alice".to_string(), "bob".to_string()]);
        // 幂等出口：第二遍不改一个字节
        let before = serde_json::to_string(&v).unwrap();
        assert!(!encrypt_sensitive_fields_with(&mut v, &K).unwrap(), "二次加密必须报告无变化");
        assert_eq!(serde_json::to_string(&v).unwrap(), before);
    }

    /// 往返：加密态能过 `deny_unknown_fields` 全检，解密后逐字段还原明文 ——
    /// 这就是 `load_settings` 的 ③（内存态明文）
    #[test]
    fn encrypted_file_parses_and_decrypts_back() {
        let mut v = sample();
        encrypt_sensitive_fields_with(&mut v, &K).unwrap();
        let mut s: Settings = serde_json::from_value(v).unwrap();
        s.decrypt_secrets_with(&K).unwrap();
        assert_eq!(s.mysql_url, "mysql://root:p%40ss@10.0.0.1:3306/xh_dms");
        assert_eq!(s.pg_url, "postgres://postgres:pw@127.0.0.1:15433/dms_ai");
        assert_eq!(s.pg_ro_url.as_deref(), Some("postgres://ro:pw@127.0.0.1:15433/dms_ai"));
        assert_eq!(s.llm_api_key, "sk-file-key");
        assert_eq!(s.llm_keys["qwen"], "sk-qwen");
        assert_eq!(s.wework_secret, "ww-secret");
        assert_eq!(s.mysql_targets["legacy"].url(), "mysql://u:p@10.0.0.2:9030/legacy");
        assert_eq!(s.mysql_targets["doris"].url(), "mysql://u:p@10.0.0.3:9030/wh");
        assert_eq!(s.datasources["ods"], "mysql://u:p@10.0.0.4:3306/ods");
        assert_eq!(s.mcp_keys["mcp-key-alice"], "alice");
        assert_eq!(s.mcp_keys["mcp-key-bob"], "bob");
    }

    /// 明文旧配置解密 = 原样（向后兼容）；错钥匙 = 响亮失败且只报字段名不报值
    #[test]
    fn plaintext_compatible_and_wrong_key_is_loud() {
        let mut plain: Settings = serde_json::from_value(sample()).unwrap();
        plain.decrypt_secrets_with(&K).unwrap();
        assert_eq!(plain.llm_api_key, "sk-file-key", "无前缀必须原样放行");

        let mut v = sample();
        encrypt_sensitive_fields_with(&mut v, &K).unwrap();
        let mut s: Settings = serde_json::from_value(v).unwrap();
        let e = s.decrypt_secrets_with(&[7u8; 32]).err().expect("错钥匙必须失败");
        let msg = e.to_string();
        assert!(msg.contains("mysql_url"), "报错要指到字段：{msg}");
        assert!(!msg.contains("p%40ss") && !msg.contains("10.0.0.1"), "报错不许带值片段：{msg}");
    }

    /// 空敏感字段不加密（「没配」与「配了」一眼可辨）；缺省字段不凭空造出来
    #[test]
    fn empty_and_absent_fields_are_untouched() {
        let mut v = serde_json::json!({
            "mysql_url": "mysql://u:p@h/dms", "pg_url": "postgres://x",
            "llm_api_key": "", "pg_ro_url": null,
        });
        assert!(encrypt_sensitive_fields_with(&mut v, &K).unwrap());
        assert_eq!(v["llm_api_key"], "", "空串保持空串");
        assert!(v["pg_ro_url"].is_null(), "null 不动");
        assert!(v.get("wework_secret").is_none(), "缺省字段不凭空造");
        assert!(v.get("mcp_keys").is_none());
        let mut s: Settings = serde_json::from_value(v).unwrap();
        s.decrypt_secrets_with(&K).unwrap();
        assert_eq!(s.mysql_url, "mysql://u:p@h/dms");
        assert!(s.llm_api_key.is_empty() && s.pg_ro_url.is_none());
    }

    /// 读取侧透明解密：哪怕内存 cfg 真撞上密文（未走 load_settings 的路径），
    /// resolve_provider / dsn_map / db_targets 也按进程钥匙解回明文再交给建池/LLM。
    /// （正常路径里内存已是明文，这层是零成本保险 —— 判据用进程默认钥匙自洽构造。）
    #[test]
    fn read_side_decrypts_transparently() {
        let (key, _) = crypto::default_key();
        let enc = |s: &str| crypto::encrypt_with(&key, s).unwrap();
        let cfg: Settings = serde_json::from_value(serde_json::json!({
            "mysql_url": enc("mysql://auth:pw@10.0.0.1:3306/dms"),
            "pg_url": "postgres://x",
            "llm_base_url": "https://api.deepseek.com",
            "llm_api_key": enc("sk-file"),
            "llm_keys": {"qwen": enc("sk-qwen")},
            "mysql_targets": {"wh": enc("mysql://u:p@10.0.0.9:9030/wh")},
            "datasources": {"ods": enc("mysql://u:p@10.0.0.4:3306/ods")},
        }))
        .unwrap();
        // LLM key：llm_keys 与旧式 llm_api_key 两条路径都解
        assert_eq!(resolve_provider("qwen", &cfg).unwrap().api_key, "sk-qwen");
        assert_eq!(resolve_provider("deepseek", &cfg).unwrap().api_key, "sk-file");
        // DSN 映射：值解回明文，权限源端点比较也按明文算
        let m = cfg.dsn_map();
        assert_eq!(m["ods"], "mysql://u:p@10.0.0.4:3306/ods");
        // db_targets：解回明文才能建池
        let t = db_targets(&cfg);
        assert_eq!(t, vec![("wh".to_string(), "mysql://u:p@10.0.0.9:9030/wh".to_string())]);
        // 错钥匙场景：decrypt_auto 失败原样放行 → 密文留在值里，建池时以 DSN 形状响亮失败
        assert!(crypto::is_encrypted(&enc("x")), "自洽检查");
    }

    /// mcp_keys 解密后撞名 = 配置自相矛盾，响亮失败（不静默丢一个员工的 key）
    #[test]
    fn mcp_key_name_collision_after_decrypt_fails() {
        let (key, _) = crypto::default_key();
        let dup = crypto::encrypt_with(&key, "same-key").unwrap();
        // 同一明文的两份密文（nonce 不同）—— 解密后键名撞车
        let dup2 = crypto::encrypt_with(&key, "same-key").unwrap();
        let mut s: Settings = serde_json::from_value(serde_json::json!({
            "mysql_url": "mysql://x", "pg_url": "postgres://x",
            "mcp_keys": {dup: "alice", dup2: "bob"},
        }))
        .unwrap();
        let e = s.decrypt_secrets().err().expect("撞名必须失败");
        assert!(e.to_string().contains("mcp_keys"), "{e}");
    }
}
