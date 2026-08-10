//! dms-ai 服务端：M0 骨架（/api/health）+ M1 权限内核（principal/scope/inject + scope 判官子命令）。

mod admin_api;
mod artifact_api;
mod auth;
mod chat;
mod corrector;
mod db;
mod embed;
mod direct;
mod ds_api;
mod embed_fill;
mod daily_digest;
mod chart_svg;
// 【数据地图】路由与 migrate 已接线（契约见 datamap_api.rs 文件头）。
mod datamap_api;
mod deep_api;
mod settings_api;
mod insight_api;
mod kb_api;
mod kb_eval_api;
mod kg_api;
mod kb_mindmap_api;
mod llm;
mod mcp_api;
mod query_log;
mod quality_api;
mod skills_api;
mod trace_api;
// 路由注册不在本轮（统一由集成方在下方 Router 处补 .route 两行）。
// 集成方接线后这个 allow 一行删掉：两个 handler 一经 .route 引用，全模块即活。
mod usage_api;
mod vision_api;
mod wework;
// 【小程序接入】路由已接线（契约见 xcx_api.rs 文件头）。
mod xcx_api;

// Server 内所有 HTTP 身份加载都从 `auth::load_principal` 收口。它保持 policy 的字段与
// scope 契约不变，只区分独立密码会话和已由 DMS/企微认证的会话是否复查密码过期。
extern crate dms_policy as dms_policy_core;
mod dms_policy {
    pub use crate::dms_policy_core::{inject, load_rules, seed_rules};

    pub mod principal {
        pub use crate::dms_policy_core::Principal;
        pub use crate::dms_policy_core::principal::list_roles;

        pub async fn load_principal(
            mysql: &dms_connector::mysql::ReadOnlyMySql,
            login_name: &str,
            role_code: Option<&str>,
        ) -> anyhow::Result<crate::dms_policy_core::Principal> {
            crate::auth::load_principal(mysql, login_name, role_code).await
        }
    }

    pub mod proof {
        pub use crate::dms_policy_core::proof::*;
    }

    pub mod scope {
        pub use crate::dms_policy_core::scope::*;
    }
}

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{delete, get, post, put},
    Json, Router,
};
use dms_connector::mysql::ReadOnlyMySql;
use dms_connector::owned::OwnedStore;
use dms_connector::registry::SourceRegistry;
use dms_connector::source::SqlSource;
use dms_kernel::DsId;
// 权限内核（principal/scope/inject）已整块迁 dms-policy：这里只保留模块别名，
// 让 `principal::load_principal` / `scope::compute_scope` 这些调用点一个字不用改。
use dms_policy::{principal, scope};
// 元数据（DDL/种子/注册表/召回）已整块迁 dms-semantic（`server/src/meta.rs` 随之删除）。
// 数据源注册表的调用点最密，只给它一个 use 别名；其余按族全路径写，不造第二层门面（§8）。
use dms_semantic::registry::datasource as ds_reg;
// 问答循环整块迁 dms-agent（`server/src/pipeline.rs` 与 `triage.rs` 随之删除，
// AGE 图 IO 迁 `dms_connector::graph`）。server 侧只剩下面那层薄包装：观测 + 依赖注入。
use dms_agent::triage;

struct AppState {
    /// DMS 身份与权限源。分析库切换后，本池仍固定读取 DMS 权限表。
    auth_mysql: Arc<ReadOnlyMySql>,
    /// 当前分析查询源。`Arc` 是为了与 `SourceRegistry` 里预置的那一个**是同一个池**
    mysql: Arc<ReadOnlyMySql>,
    /// 自有 PG 唯一可写通道。迁移期未字面量化的查询走 `owned.pool()`（T10 收口后那个过渡口删掉）
    owned: OwnedStore,
    /// 多源注册中心：DMS 主源已 `preload`，第二个源从这里懒建。
    /// 消费者：`ds_api` 的 probe/注销（K3-A）；选源取数是 K3-B。
    sources: Arc<SourceRegistry>,
    llm: llm::LlmClient,
    dms_base_url: String,
    wework: wework::WeworkCfg,
    /// 【K1】Python 文档服务（/parse、/chunk）与向量服务（/embed）：同一个 `service_url`（裁决 V1）
    doc: dms_connector::doc::DocService,
    embed: dms_connector::embed::EmbedClient,
    kb_cfg: dms_knowledge::ingest::IngestCfg,
    /// 【K6-A】对外 MCP 的 key → login_name。空 = 功能关闭（`/api/mcp` 恒 404）。
    /// 明文 key 只从 `settings.json` 来，只在 `mcp_api::authorize` 里被查表，不许外流
    /// （不进日志、不进响应）——与 dsn 同级敏感。
    mcp_keys: std::collections::HashMap<String, String>,
    /// AGE 图最近定时刷新结果（健康检查可见）
    graph_status: Arc<std::sync::Mutex<String>>,
    /// 【SC】自一致采样数（配置 `sc_samples`，默认 1 = 关）
    sc_samples: usize,
    /// 【AI 解读】`/api/analysis` 是否真的调 fast 模型（配置 `insight_enabled`，默认 true）。
    /// false = 只返确定性口径说明，零 LLM 花费（止血阀，见 `db.rs` 那个字段的文档）
    insight_enabled: bool,
    /// 🔴 **安全**：无会话 token 时是否采信请求自报的 `login_name`。默认 false。
    /// 开着等于没有认证 —— 见 `db.rs::Settings::insecure_login_fallback` 的文档。
    insecure_login_fallback: bool,
    /// settings.json 原样持有（`db::resolve_provider` 的 key 解析与 `admin_api` 的
    /// llm-config 需要它）。**只在进程内**，字段里的 key 永远不进日志/响应（红线同 DSN）。
    /// 运行时配置（`settings_api` 页面编辑会写它 —— RwLock；读取一律 `st.cfg()` 克隆快照）
    cfg: std::sync::RwLock<db::Settings>,
    /// 设置文件、运行时池与 meta.kv 的跨介质提交锁；只串行化配置写，不阻塞问数与配置读取。
    settings_write: tokio::sync::Mutex<()>,
}

impl AppState {
    /// 配置快照（克隆 —— 读者拿的是当时那份，写者整体替换；`Settings: Clone` 是小 struct）
    fn cfg(&self) -> db::Settings {
        self.cfg.read().expect("cfg 锁中毒").clone()
    }
}

/// 元数据启动引导：建表 → 首装/目录升级时采集数仓 → 种子 upsert → 权限档案灌表+加载。
/// 服务与 CLI 统一走这里；目录版本未变且完整时跳过 information_schema 采集。
/// 入参从裸 `&PgPool` 改 `&OwnedStore`：权限档案的两条 SQL 迁 policy 后走
/// `OwnedStore::fixed(&'static str)` 字面量通道（门禁「policy 不得 sqlx::query」）。
async fn bootstrap_meta(owned: &OwnedStore, mysql: &ReadOnlyMySql) -> anyhow::Result<()> {
    let pg = owned.pool();
    dms_semantic::ddl::migrate(pg).await?;
    // 【K6-B】meta.query_log。放这里而不是只放服务启动：判官的 `ask` 子命令也走 `pipeline::ask`，
    // 表不存在时每问一句 warn 一行「表不存在」（写入是 spawn 的，不影响 stdout JSON）。
    query_log::migrate(pg).await?;
    kb_eval_api::migrate(&owned).await?;
    skills_api::migrate(&owned).await?;
    quality_api::migrate(pg).await?;
    datamap_api::migrate(pg).await?;
    let target = mysql.target_name();
    let refresh_catalog = if mysql.is_warehouse() {
        dms_semantic::warehouse_catalog::needs_sync(pg, ds_reg::DMS_DS_ID, &target).await?
    } else {
        tracing::info!(target = %target, "production_lookup 启动不执行数仓目录探针");
        false
    };
    let catalog_stats = if refresh_catalog {
        anyhow::ensure!(
            mysql.is_warehouse(),
            "分析目标 {target} 不是数仓，拒绝用生产 DMS 或旧目录回退启动"
        );
        let assets = dms_semantic::warehouse_catalog::metadata_assets();
        // 探针失败不再直接拒绝启动：有历史快照则降级沿用（trust=degraded 透出），
        // 无任何快照才硬失败（fail-closed 不变）。公网链路实测探针 ~27s 且会抖。
        let probed = dms_semantic::warehouse_catalog::probe_with_fallback(
            pg,
            &target,
            mysql
                .probe_schema_with_warehouse_catalog(&assets)
                .await
                .map_err(|e| anyhow::anyhow!("数仓目录探针失败：{e}")),
        )
        .await?;
        match probed.snapshot {
            Some(mut snapshot) => {
                dms_semantic::warehouse_catalog::validate_required_snapshot(&snapshot)?;
                let warehouse_comments = mysql.enrich_dms_snapshot(&mut snapshot).await?;
                let _ = dms_semantic::sales_fact::enrich_schema_snapshot(&mut snapshot);
                let (tables, columns) = dms_semantic::ingest::schema_sync::sync_schema(
                    pg,
                    ds_reg::DMS_DS_ID,
                    &snapshot,
                    true,
                )
                .await
                .map_err(|e| anyhow::anyhow!("数仓目录同步失败，拒绝使用空/旧语义启动：{e}"))?;
                Some((probed.stats, tables, columns, warehouse_comments))
            }
            None => {
                // degraded：跳过 sync_schema 与 mark_synced（版本标记不动，下次启动自动重试）
                tracing::warn!(target = %target, trust = probed.trust.as_str(), "数仓目录探针失败，按最近快照降级启动");
                None
            }
        }
    } else {
        None
    };
    dms_semantic::seed::seed(pg).await?;
    if let Some((stats, tables, columns, warehouse_comments)) = catalog_stats {
        dms_semantic::warehouse_catalog::mark_synced(
            pg,
            &target,
            stats.requested,
            stats.tables,
            stats.missing,
        )
        .await?;
        if stats.missing > 0 {
            tracing::warn!(
                target = %target,
                missing = stats.missing,
                "部分可选数仓资产物理缺失，已保持 fail-closed"
            );
        }
        tracing::info!(
            target = %target,
            version = dms_semantic::warehouse_catalog::VERSION,
            tables,
            columns,
            warehouse_comments,
            catalog_tables = stats.tables,
            catalog_missing = stats.missing,
            "数仓目录已按版本自动同步"
        );
    }
    // 【K3-A】'dms' 那一行是**存量行为的显式表达**：让「只有一个源」与「多源」走同一套代码
    dms_semantic::seed::seed_datasources(pg).await?;
    dms_policy::seed_rules(owned).await?;
    let n = dms_policy::load_rules(owned, ds_reg::DMS_DS_ID).await?;
    tracing::info!("元数据引导完成：scope_binding 权限档案 {n} 张表");
    Ok(())
}

fn llm_client(cfg: &db::Settings) -> llm::LlmClient {
    // 经供应商目录解析（文件供应商 = `llm_provider` 或按 base_url 推断）：文件值覆盖
    // 目录默认（老配置逐字等价），目录补文件缺省字段（只写 key 的最小配置也能起）。
    let name = if cfg.llm_provider.is_empty() {
        db::infer_provider(&cfg.llm_base_url).unwrap_or("custom")
    } else {
        cfg.llm_provider.as_str()
    };
    let conf = db::resolve_provider(name, cfg).unwrap_or_else(|e| panic!("settings.json 的 LLM 配置无效: {e}"));
    let fallback = db::resolve_fallback_vision(cfg)
        .unwrap_or_else(|e| panic!("settings.json 的备用多模态配置无效: {e}"))
        .map(|(_, conf)| conf);
    llm::LlmClient::with_conf_and_fallback(conf, fallback)
}

/// 问数分析源（只读 MySQL 协议，当前为 Doris）。`DsId::new("dms")` 是历史固定标识：
/// registry、权限注入与缓存键仍沿用它；连接地址只允许来自非 `dms` 的 `mysql_targets`。
/// 分析库连接失败直接启动失败，绝不回退到 DMS 身份/权限库。
async fn dms_source(cfg: &db::Settings, owned: &OwnedStore) -> anyhow::Result<Arc<ReadOnlyMySql>> {
    let (target, url) = admin_api::db_boot_target(owned, cfg).await?;
    let m = ReadOnlyMySql::connect(
        DsId::new("dms"),
        &url,
        10,
        dms_semantic::registry::SENSITIVE_COLS,
        db::db_target_capability(cfg, &target),
    )
    .await
    .map_err(|_| anyhow::anyhow!(
        "分析库目标 {target} 连接失败：请检查目标配置、网络与只读权限"
    ))?;
    m.set_target_name(&target);
    // A8：数据源级查询策略（settings.json 的 mysql_targets.<name>.max_rows/timeout_ms），
    // 与全局两档取 min —— 只可能更紧
    if let Some(t) = cfg.mysql_targets.get(&target) {
        m.set_ds_policy(t.policy());
    }
    Ok(Arc::new(m))
}

/// 身份、角色与数据权限固定读取 DMS 默认库，不跟业务查询目标切换。
async fn auth_source(cfg: &db::Settings) -> anyhow::Result<Arc<ReadOnlyMySql>> {
    Ok(Arc::new(
        ReadOnlyMySql::connect(
            DsId::new("dms-auth"),
            &cfg.mysql_url,
            5,
            dms_semantic::registry::SENSITIVE_COLS,
            dms_connector::mysql::MysqlCapability::IdentityPermission,
        )
        .await
        .map_err(|_| anyhow::anyhow!(
            "DMS 身份/权限库连接失败：请检查正式 settings 配置、网络与只读权限"
        ))?,
    ))
}

/// A1 自动发现的探针闸门 + 调用（裁决 T3-2 / C5：动态 SQL 走同一条全管道，不开专用后门）。
/// **有资格 unrestricted 放行**：这是 CLI 管理任务（`meta autodiscover`），SQL 由
/// `autodiscover_dict_columns` 用 information_schema 的表名/列名拼装，不含任何用户输入，
/// 也没有「以谁的身份查」这回事。即便如此只读红线与 LIMIT 护栏一条不少（`check()` 照走）。
/// 凭证只能由 `dms_policy::proof` 铸造 —— server 自己调 kernel 的构造函数，会让
/// 「那条 grep 就是全仓放行清单」失效（连这句注释都刻意不写出那个符号名，否则它自己就是噪声）。
/// 第二证据是 argv：服务进程铸不出这张凭证。
async fn autodiscover(
    mysql: &ReadOnlyMySql,
    pg: &sqlx::PgPool,
) -> anyhow::Result<serde_json::Value> {
    let proof = dms_policy::proof::for_admin_cli("meta autodiscover")
        .ok_or_else(|| anyhow::anyhow!("探针放行凭证铸造失败"))?;
    let gate = dms_semantic::ingest::autodiscover::probe::ProbeGate {
        proof: &proof,
        guard: &dms_agent::GUARD,
    };
    dms_semantic::ingest::autodiscover::autodiscover_dict_columns(mysql, pg, &gate).await
}

/// 自有 PG（meta/kb/chat）。全仓唯一可写通道。
async fn owned_store(cfg: &db::Settings) -> anyhow::Result<OwnedStore> {
    Ok(OwnedStore::connect(&cfg.pg_url, 10)
        .await
        .map_err(|_| anyhow::anyhow!(
            "自有元数据库连接失败：请检查正式 settings 配置、网络与数据库权限"
        ))?)
}

/// `why-not-compose` 的题库默认路径。**相对路径**，解析基准是进程 cwd ——
/// 容器里那是 `WORKDIR /app`，故要求 `scripts/serve.ps1` 把仓库 `tools/` 挂到 `/app/tools`。
const WHY_CASES_DEFAULT: &str = "tools/eval_cases.json";

/// `why-not-compose` 的参数。**独立成纯函数是为了能断言**：这里的失败模式全是
/// 「静默把参数当成别的东西」，而那种失败不连库也能测出来（见 `mod tests`）。
#[derive(Debug, Default, PartialEq)]
struct WhyArgs {
    /// 单问模式的问句；`None` = 扫全量题库
    question: Option<String>,
    /// 题库路径覆盖位；`None` = `WHY_CASES_DEFAULT`
    cases: Option<String>,
    /// 逐题门分布 CSV 输出路径
    csv: Option<String>,
}

/// 🔴 未知 flag 与多余位置参数一律**报错**，不许当问句吞掉。
///
/// 这道判据是踩出来的：上一版用 `args.get(2)` 当问句，于是
/// ① 任何 `--xxx` 都变成「问一句叫 --xxx 的话」，报告照样打印、看不出跑错了；
/// ② `why-not-compose 本月销售额 按品牌`（`serve.ps1` 的 `-Cmd` 按空格切参）静默丢掉
///    「按品牌」，那道题于是恒报 ✅ —— 判据恒绿。两种都是同一个形状：**宽容解析 = 假绿**。
fn parse_why_args(argv: &[String]) -> Result<WhyArgs, String> {
    let mut a = WhyArgs::default();
    let mut it = argv.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--csv" | "--cases" => {
                // 后面跟着另一个 flag 也算缺值（`--csv --cases x` 不该把 `--cases` 当路径）
                let v = it
                    .next()
                    .filter(|v| !v.starts_with('-'))
                    .ok_or_else(|| format!("{x} 缺路径参数（用法：{x} <path>）"))?
                    .clone();
                if x == "--csv" {
                    a.csv = Some(v);
                } else {
                    a.cases = Some(v);
                }
            }
            f if f.starts_with('-') => {
                return Err(format!(
                    "未知参数「{f}」。用法：why-not-compose [\"<问句>\"] [--cases <path>] [--csv <path>]\n\
                     （`--flag=value` 也不支持，用空格分开写）"
                ))
            }
            // 🔴 空位置参数**不是**问句。`serve.ps1 -Cmd` 用 `$Cmd -split ' '` 切参，
            // 多打一个空格或留个尾空格就会产出空 token（实测
            // `'why-not-compose ' -split ' '` → [why-not-compose][""]）。
            // 落进问句臂的话，「扫全量 38 题」会**静默降级成「问一句空话」**，
            // 打印「按门分布（1题）」并退出 0 —— 判据从 38 题缩到 1 题，没人会红。
            // 这正是本函数要关的那扇门的另一条缝（评审实测抓到）。
            q if q.trim().is_empty() => {
                return Err(
                    "位置参数是空串（`-Cmd` 里多了一个空格？）—— 拒绝把它当问句：\
                     那会让全量诊断静默降级成「问一句空话」"
                        .to_string(),
                )
            }
            q if a.question.is_none() => a.question = Some(q.to_string()),
            extra => {
                return Err(format!(
                    "多余的位置参数「{extra}」—— 问句只能有一个。问句含空格时整体加引号；\
                     注意 `serve.ps1 -Cmd` 是按空格切参的，带空格的问句请直接用 docker exec"
                ))
            }
        }
    }
    Ok(a)
}

/// `audit-exemplars` 喂给 `build_rules` 的召回表名。
///
/// 🔴 `from_table_aliases` 返回 `(表名, 别名)` —— 这里要的是**表名**。原来写的是 `(_, t)`，
/// 取的是别名，于是 `FROM t_sales_order so` 交上去的是 `["so"]`：`build_rules` 的**表级**判据
/// （`table_scope` 的 `RequireCols` / 快照那两条）拿别名去匹配表名，一条都匹配不上 ——
/// 审计于是对「缺表级口径过滤」这一整类**恒报干净**，而那正是它唯一的存在理由
/// （问句级判据不靠表名，所以它看着仍在报东西，最难发现的那种假绿）。
/// 判据 `audit_tables_are_table_names_not_aliases`。
fn audit_tables(sql: &str) -> Vec<String> {
    dms_kernel::sql::lex::from_table_aliases(sql).into_iter().map(|(t, _)| t).collect()
}

/// 最小 CSV 转义：字段一律加引号、内部 `"` 翻倍。
/// 不引 csv crate：这里只写不读；但**不能不转义** —— 题库问句里有半角逗号和引号，
/// 不转义会把列串位，而串位后的 CSV 看起来仍然「有八列」，正是最难发现的那种坏。
fn csv_row(cells: &[&str]) -> String {
    cells.iter().map(|c| format!("\"{}\"", c.replace('"', "\"\""))).collect::<Vec<_>>().join(",")
}

/// `eval-batch` 的 stdin NDJSON 协议。`id` 原样透传，方便调用方用字符串或数字关联题目；
/// role 必须出现但可为 null，gold_sql 可省略；两者的空串都按缺省处理。
#[derive(serde::Deserialize)]
struct EvalBatchReq {
    id: serde_json::Value,
    login: String,
    role: serde_json::Value,
    q: String,
    #[serde(default)]
    gold_sql: Option<String>,
}

impl EvalBatchReq {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.id.is_null(), "id 不能为空");
        anyhow::ensure!(!self.login.trim().is_empty(), "login 不能为空");
        anyhow::ensure!(self.role.is_null() || self.role.is_string(), "role 只能是字符串或 null");
        anyhow::ensure!(!self.q.trim().is_empty(), "q 不能为空");
        Ok(())
    }
}

fn eval_batch_output(
    id: serde_json::Value,
    got: Option<serde_json::Value>,
    gold: Option<serde_json::Value>,
    ask_wall_ms: u64,
    gold_ms: u64,
    errors: Vec<String>,
) -> serde_json::Value {
    let mut out = serde_json::Map::<String, serde_json::Value>::new();
    out.insert("id".into(), id);
    out.insert("ask_wall_ms".into(), ask_wall_ms.into());
    out.insert("gold_ms".into(), gold_ms.into());
    if let Some(v) = got {
        out.insert("got".into(), v);
    }
    if let Some(v) = gold {
        out.insert("gold".into(), v);
    }
    if !errors.is_empty() {
        out.insert("error".into(), errors.join(" | ").into());
    }
    out.into()
}

/// 单题失败只落到该题的 `error`，绝不终止驻留进程。身份每题重新从 DMS 认证源加载；
/// 连接池、元数据、LLM、向量客户端和分析源则由 `eval-batch` 分支一次初始化后复用。
#[allow(clippy::too_many_arguments)]
async fn eval_batch_one(
    req: EvalBatchReq,
    client: &llm::LlmClient,
    auth_mysql: &ReadOnlyMySql,
    mysql: &ReadOnlyMySql,
    sources: &SourceRegistry,
    pg: &sqlx::PgPool,
    embed: &dms_connector::embed::EmbedClient,
    sc_samples: usize,
) -> serde_json::Value {
    let EvalBatchReq { id, login, role, q, gold_sql } = req;
    let role = role.as_str().map(str::trim).filter(|s| !s.is_empty());
    let gold_sql = gold_sql.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let mut got = None;
    let mut gold = None;
    let mut errors = Vec::new();
    let ask_t0 = std::time::Instant::now();

    let principal = match principal::load_principal(auth_mysql, login.trim(), role).await {
        Ok(p) => Some(p),
        Err(e) => {
            errors.push(format!("identity: {e}"));
            None
        }
    };

    if let Some(p) = principal.as_ref() {
        // 评测只验证当前热切换分析目标：它在运行时始终以逻辑主源 `dms` 注册。
        // 显式锁源可阻止多源自动路由把 generated 送到上传源，而 gold 仍在主分析池执行。
        let eval_ds = Some(ds_reg::DMS_DS_ID);
        let (answer, log) = ask(
            client,
            auth_mysql,
            mysql,
            sources,
            pg,
            embed,
            p,
            q.trim(),
            None,
            eval_ds,
            None,
            sc_samples,
        )
        .await;
        // 与一次性 ask CLI 一样，在输出前等观测写入完成；写日志失败不改变业务结果。
        let _ = log.await;
        match answer {
            Ok(answer) => match serde_json::to_value(answer) {
                Ok(v) => got = Some(v),
                Err(e) => errors.push(format!("ask serialize: {e}")),
            },
            Err(e) => errors.push(format!("ask: {e}")),
        }
    }
    let ask_wall_ms = ask_t0.elapsed().as_millis() as u64;

    let mut gold_ms = 0;
    if let (Some(p), Some(sql)) = (principal.as_ref(), gold_sql) {
        let gold_t0 = std::time::Instant::now();
        let gold_result = async {
            // 与 exec-sql 完全相同：同身份 scope → 生产 gate → 当前只读分析源。
            let user_scope = scope::compute_scope_cached(auth_mysql, p).await?;
            let scoped = dms_agent::gate(p, sql, &user_scope, &dms_kernel::MysqlDialect)?;
            let fetch_t0 = std::time::Instant::now();
            let rs = mysql
                .fetch(&scoped, dms_agent::MAX_ROWS, dms_agent::EXEC_TIMEOUT)
                .await?;
            Ok::<_, anyhow::Error>(serde_json::json!({
                "sql": scoped.wire(),
                "columns": rs.columns,
                "row_count": rs.rows.len(),
                "rows": rs.rows,
                "elapsed_ms": fetch_t0.elapsed().as_millis() as u64,
            }))
        }
        .await;
        gold_ms = gold_t0.elapsed().as_millis() as u64;
        match gold_result {
            Ok(v) => gold = Some(v),
            Err(e) => errors.push(format!("gold: {e}")),
        }
    }

    eval_batch_output(id, got, gold, ask_wall_ms, gold_ms, errors)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 日志一律走 stderr：stdout 留给子命令的 JSON 输出（判官脚本要解析）
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = db::load_settings()?;

    let args: Vec<String> = std::env::args().collect();

    // M2 子命令：meta sync —— 采集 schema 入 PG 并播种警告/强制补表
    if args.len() >= 3 && args[1] == "meta" && args[2] == "sync" {
        let owned = owned_store(&cfg).await?;
        let mysql = dms_source(&cfg, &owned).await?;
        let pg = owned.pool();
        dms_semantic::ddl::migrate(pg).await?;
        // 采集（IO）在 server，入库（PG）在 semantic：`ds` 传主源即今天的语义（零行为变化）
        let assets = dms_semantic::warehouse_catalog::metadata_assets();
        let (mut snap, warehouse_catalog) =
            mysql.probe_schema_with_warehouse_catalog(&assets).await?;
        dms_semantic::warehouse_catalog::validate_required_snapshot(&snap)?;
        let enriched = mysql.enrich_dms_snapshot(&mut snap).await?;
        let _ = dms_semantic::sales_fact::enrich_schema_snapshot(&mut snap);
        let (nt, nc) =
            // `true`＝过滤备份表（DMS 是别人建的库，含 bak_*/日期后缀的垃圾表）
            dms_semantic::ingest::schema_sync::sync_schema(pg, ds_reg::DMS_DS_ID, &snap, true)
                .await?;
        dms_semantic::seed::seed(pg).await?;
        dms_semantic::warehouse_catalog::mark_synced(
            pg,
            &mysql.target_name(),
            warehouse_catalog.requested,
            warehouse_catalog.tables,
            warehouse_catalog.missing,
        )
        .await?;
        println!("{}", serde_json::json!({
            "tables": nt,
            "columns": nc,
            "warehouse_comments": enriched,
            "warehouse_catalog_requested": warehouse_catalog.requested,
            "warehouse_catalog_tables": warehouse_catalog.tables,
            "warehouse_catalog_columns": warehouse_catalog.columns,
            "warehouse_catalog_missing": warehouse_catalog.missing
        }));
        return Ok(());
    }

    // 引擎 A1 子命令：meta autodiscover —— 字典码列自动对码注册（数据驱动，字典变了重跑即自适应）
    if args.len() >= 3 && args[1] == "meta" && args[2] == "autodiscover" {
        let owned = owned_store(&cfg).await?;
        let mysql = dms_source(&cfg, &owned).await?;
        let pg = owned.pool();
        dms_semantic::ddl::migrate(pg).await?;
        let r = autodiscover(&mysql, pg).await?;
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }

    // 引擎 A2 子命令：meta datamap-build [ds] —— 数据地图静态推断（只读小样画像 →
    // 四类列间推断 → 全部按 pending upsert 进 meta.datamap_edge）。
    // proof 铸造点在这里：CLI 管理任务、没有「以谁的身份查」，与 `meta autodiscover` 同一先例。
    if args.len() >= 3 && args[1] == "meta" && args[2] == "datamap-build" {
        let owned = owned_store(&cfg).await?;
        let mysql = dms_source(&cfg, &owned).await?;
        let pg = owned.pool();
        dms_semantic::ddl::migrate(pg).await?;
        // 老库 kind CHECK 拓值（六值 → 七值含 correlated）：服务端启动走 bootstrap_meta 已含
        // 这一步，但 CLI 直跑本分支不经过 bootstrap —— 幂等，补上与服务同一序，否则老库上
        // correlated 边会被建表时的六值 CHECK 拒掉。
        datamap_api::migrate(pg).await?;
        let ds = args.get(3).map(String::as_str).unwrap_or(ds_reg::DMS_DS_ID);
        anyhow::ensure!(mysql.is_warehouse(), "datamap-build 只许打数仓目标");
        let assets = dms_semantic::warehouse_catalog::metadata_assets();
        let (snapshot, _) = mysql
            .probe_schema_with_warehouse_catalog(&assets)
            .await
            .map_err(|e| anyhow::anyhow!("数仓目录探针失败：{e}"))?;
        let proof = dms_policy::proof::for_admin_cli("meta datamap-build")
            .ok_or_else(|| anyhow::anyhow!("画像放行凭证铸造失败"))?;
        let gate = dms_semantic::datamap::MapGate { proof: &proof, guard: &dms_agent::GUARD };
        let r = dms_semantic::datamap::build(pg, &mysql, &gate, ds, &snapshot).await?;
        println!("{}", serde_json::json!({
            "ds": r.ds_id,
            "tables_profiled": r.tables_profiled,
            "columns_profiled": r.columns_profiled,
            "tables_skipped": r.tables_skipped.len(),
            "edges": r.edges,
            "joinable": r.joinable,
            "synonym": r.synonym,
            "distribution_similar": r.distribution_similar,
            "correlated": r.correlated,
        }));
        return Ok(());
    }

    // 引擎 A2 子命令：meta datamap-calibrate [days] —— 使用轨迹校准
    // （query_log 近 N 天成功行 → JOIN 表对/同现列对 → co_occurs 边 upsert）
    if args.len() >= 3 && args[1] == "meta" && args[2] == "datamap-calibrate" {
        let owned = owned_store(&cfg).await?;
        let pg = owned.pool();
        let days: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
        let r = dms_semantic::datamap_usage::calibrate_from_query_log(pg, days).await?;
        println!("{}", serde_json::json!({
            "window_days": r.window_days,
            "rows_scanned": r.rows_scanned,
            "rows_parsed": r.rows_parsed,
            "parse_failure_total": r.parse_failure_total,
            "join_edges": r.join_edges,
            "col_edges": r.col_edges,
            "edges_upserted": r.edges_upserted,
        }));
        return Ok(());
    }

    // 引擎 A2 子命令：meta lineage-build [ds] —— 血缘反推（DWS/ADS ← ODS）。
    // 纯 PG 元数据（table_doc/column_doc + 目录 + 统计边佐证），不打 Doris，秒级可重跑。
    if args.len() >= 3 && args[1] == "meta" && args[2] == "lineage-build" {
        let owned = owned_store(&cfg).await?;
        let pg = owned.pool();
        let ds = args.get(3).map(String::as_str).unwrap_or(ds_reg::DMS_DS_ID);
        let r = dms_semantic::lineage::build(pg, ds).await?;
        println!("{r:?}");
        return Ok(());
    }

    // 子命令：review-pending —— 批量复核 pending 语料（SuperSonic MemoryReviewTask）
    if args.len() >= 2 && args[1] == "review-pending" {
        let owned = owned_store(&cfg).await?;
        let client = llm_client(&cfg);
        let n = dms_agent::review::review_all_pending(&client, owned.pool(), 100).await?;
        println!("复核处理 {n} 条 pending 语料");
        return Ok(());
    }

    // 引擎 C 子命令：review-lessons —— 批量复核失败复盘产出的候选教训（candidate → active/disabled）
    if args.len() >= 2 && args[1] == "review-lessons" {
        let owned = owned_store(&cfg).await?;
        let client = llm_client(&cfg);
        let n = dms_agent::review::review_lessons(&client, owned.pool(), 100).await?;
        println!("复核处理 {n} 条候选教训");
        return Ok(());
    }

    // 子命令：check-sql "<sql>" —— SchemaCorrector 字段校验冒烟
    if args.len() >= 3 && args[1] == "check-sql" {
        let owned = owned_store(&cfg).await?;
        match corrector::schema_check(owned.pool(), ds_reg::DMS_DS_ID, &args[2]).await? {
            Some(hint) => println!("发现幻觉列:\n{hint}"),
            None => println!("OK 字段全部合法"),
        }
        return Ok(());
    }

    // 子命令：resync-uploads —— 给已登记的上传源**补采** schema（幂等）
    //
    // 🔴 为什么需要它：`sync_upload_schema` 是在上传那一刻跑的，
    // 于是**在它落地之前上传的文件**留下「数据源在、`meta.table_doc`/`column_doc` 空」的状态 ——
    // 问数必然答不出（LLM 拿到空 schema），而且**不会自愈**：`ingest` 按 sha256 去重，
    // 重新上传同一份文件只会命中旧 doc、不再走通道②。
    // 实测本机就有一个这样的源（CSV 那份）。真实部署里这就是「升级后老数据半可用」。
    //
    // 幂等：`sync_schema` 本身是 upsert + 清理陈旧行，重复跑无副作用。
    if args.len() >= 2 && args[1] == "resync-uploads" {
        let owned = owned_store(&cfg).await?;
        let pg = owned.pool();
        let sources = SourceRegistry::new(cfg.dsn_map());
        for (name, target) in &cfg.mysql_targets {
            sources.set_policy(&DsId::new(name), target.policy());
        }
        let rows = ds_reg::list_datasources(pg).await?;
        let ups: Vec<_> = rows.iter().filter(|r| r.ds_id.starts_with("upload_")).collect();
        let mut ok = 0usize;
        for r in &ups {
            let spec = dms_connector::registry::DsSpec {
                ds_id: DsId::new(&r.ds_id),
                kind: match ds_reg::source_kind(&r.kind) {
                    Some(k) => k,
                    None => {
                        println!("跳过 {}（kind={} 不支持）", r.ds_id, r.kind);
                        continue;
                    }
                },
                dsn_ref: r.dsn_ref.clone(),
                max_conn: 2,
                schema: dms_knowledge::tabular::upload_schema_of_ds(&r.ds_id),
            };
            let res = async {
                let src = sources.get(&spec).await?;
                let snap = src.probe_schema().await?;
                // `false` = 不过滤备份表：表名是我们自己生成的（同 kb_api 那处的理由）
                dms_semantic::ingest::schema_sync::sync_schema(pg, &r.ds_id, &snap, false).await
            }
            .await;
            match res {
                Ok((t, c)) => {
                    ok += 1;
                    println!("✅ {} · {} · 表 {t} 列 {c}", r.ds_id, r.name);
                }
                Err(e) => println!("✗ {} · {} · {e}", r.ds_id, r.name),
            }
        }
        println!("上传源 {} 个，成功补采 {ok} 个", ups.len());
        return Ok(());
    }

    // 子命令：why-not-compose ["<问句>"] [--cases <path>] [--csv <path>]
    //   无参        = 扫全量题库（默认 `WHY_CASES_DEFAULT`）
    //   "<问句>"    = 单问
    //   --csv <p>   = 扫完再把**逐题**门分布写成 CSV（stdout 只有汇总，逐题对比要文件）
    //
    // 参数解析在 `parse_why_args`（未知 flag/多余位置参数一律报错，理由写在那儿）。
    if args.len() >= 2 && args[1] == "why-not-compose" {
        let wa = match parse_why_args(&args[2..]) {
            Ok(v) => v,
            // 退 2 而不是 1：判官脚本要能把「参数写错」与「诊断跑完有结论」分开
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        };
        // (题名, 问句, tags) —— 题名/tags 只为 CSV 分层统计；单问模式两者为空
        let cases: Vec<(String, String, String)> = match &wa.question {
            Some(q) => vec![(String::new(), q.clone(), String::new())],
            None => {
                let p = wa.cases.as_deref().unwrap_or(WHY_CASES_DEFAULT);
                // 🔴 报错必须说清怎么修。上一轮容器里这条只吐 `No such file`，
                // 而真正的原因是「宿主机有文件、容器没挂」——看着像题库丢了，其实是挂载缺。
                let txt = std::fs::read_to_string(p).map_err(|e| {
                    anyhow::anyhow!(
                        "读不到题库 {p}（{e}）。容器里跑要先把仓库的 tools/ 挂进去：\
                         `scripts/serve.ps1` 的 $mounts 里有 `${{repo}}\\tools:/app/tools` 那一行，\
                         容器 cwd 是 /app，故相对路径落在 /app/tools/。\
                         也可以 `--cases <path>` 指到别处，或直接带问句参数单问。"
                    )
                })?;
                let v: serde_json::Value = serde_json::from_str(&txt)?;
                v["cases"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| {
                                let q = c["q"].as_str()?;
                                let tags = c["tags"]
                                    .as_array()
                                    .map(|t| {
                                        t.iter()
                                            .filter_map(|x| x.as_str())
                                            .collect::<Vec<_>>()
                                            .join(";")
                                    })
                                    .unwrap_or_default();
                                Some((
                                    c["name"].as_str().unwrap_or_default().to_string(),
                                    q.to_string(),
                                    tags,
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
        };
        // 🔴 反空转闸：题库读到了但一题都没解析出来（没有 `cases` 数组、或条目缺 `q`）→
        // 循环 0 次 → 打印「按门分布（0题）」→ CSV 只写表头 → **退出 0**。
        // 刚把参数解析收紧到「未知 flag 即 Err」，紧接着的载荷路径不能又静默什么都不测；
        // `--cases` 正是新加的入口，这是它自带的假绿（评审抓到）。
        if cases.is_empty() {
            anyhow::bail!(
                "题库里没有可用的 cases[].q（是不是 --cases 指错了文件、或容器没挂 tools/）"
            );
        }
        let owned = owned_store(&cfg).await?;
        let pg = owned.pool();
        let mut tally: std::collections::BTreeMap<String, usize> = Default::default();
        // CSV 八列：idx,case,question,gate,composable,reason,hardcoded,tags
        //   gate       = 门标记（`✅`/`⓿`/`①`…`⑥`/`⚠️`）——与 stdout 汇总**同一把尺子**（都取前两字符）
        //   composable = gate 是否 ✅，1/0。单独一列是为了 `awk -F, '{s+=$5}'` 直接数覆盖率
        //   reason     = 诊断首行全文（含具名指标/维度），门相同而理由不同的题靠它区分
        //   hardcoded  = 「⚙ 硬编码兜底」那一行（空 = 无）。度量「还剩多少不通用」
        //   tags       = 题库 tags，`;` 连接（分层统计）
        let mut rows: Vec<String> = vec![];
        for (i, (name, q, tags)) in cases.iter().enumerate() {
            let why = direct::why_not_compose(pg, ds_reg::DMS_DS_ID, q).await;
            println!("{q}\n    {why}");
            let gate = why.chars().take(2).collect::<String>();
            *tally.entry(gate.clone()).or_default() += 1;
            if wa.csv.is_some() {
                // 首行是判定，第二行（若有）是硬编码兜底 —— 这是 `why_not_compose` 的输出形状
                let mut ls = why.lines();
                let reason = ls.next().unwrap_or_default();
                let hard = ls.next().unwrap_or_default().trim();
                rows.push(csv_row(&[
                    &(i + 1).to_string(),
                    name,
                    q,
                    &gate,
                    if gate.starts_with('✅') { "1" } else { "0" },
                    reason,
                    hard,
                    tags,
                ]));
            }
        }
        println!("\n=== 按门分布（{}题）===", cases.len());
        for (k, n) in &tally {
            println!("  {k}  {n}");
        }
        if let Some(p) = &wa.csv {
            // 不写 BOM：这份文件是给「连跑两次逐列全等」当基线的，Python 侧
            // `open(encoding='utf-8')` 遇 BOM 会把首列名读成 `﻿idx`，比 Excel 乱码更坑
            let out = std::iter::once("idx,case,question,gate,composable,reason,hardcoded,tags")
                .chain(rows.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(p, out + "\n")
                .map_err(|e| anyhow::anyhow!("写不了 CSV {p}（{e}）——容器里 tools/ 要挂成可写（无 :ro）"))?;
            println!("CSV 已写：{p}（{} 题）", rows.len());
        }
        return Ok(());
    }

    // 子命令：audit-exemplars [--fix] —— 对 few-shot 语料逐条跑口径复核
    //
    // 🔴 为什么需要它：few-shot **直接塑造模型的众数**。一条违反声明的 `enabled` 语料会
    // 静默拖坏所有相似问句，而它不会让任何测试变红 —— 症状是「某类问句总是错同一种法」。
    // 实测撞到过投毒的入口：`execute` 原先只在 `rows.is_empty()` 时跳过沉淀，
    // 于是一条「口径复核未通过（回炉后仍违反 2 条声明）」的 SQL 照旧被沉淀
    //（那个入口已由 `dms_agent::worth_learning` 堵住，但**存量语料要能查**）。
    //
    // 判据与运行时**同一条**（`registry::caliber::build_rules` + `kernel::check_caliber`）：
    // 抄第二份就会漂出「审计说干净、运行时判红」。
    // `--fix` 只把命中的置成 `disabled`，**不删**：语料是证据，删了就查不回来为什么。
    if args.len() >= 2 && args[1] == "audit-exemplars" {
        let owned = owned_store(&cfg).await?;
        let pg = owned.pool();
        let fix = args.iter().any(|a| a == "--fix");
        let rows: Vec<(i64, String, String, String)> = sqlx::query_as(
            "SELECT id, question, sql, status FROM meta.sql_exemplar \
             WHERE status <> 'disabled' ORDER BY id",
        )
        .fetch_all(pg)
        .await?;
        let mut bad = 0usize;
        for (id, q, sql, status) in &rows {
            // 召回表名从 SQL 自己抽（语料没存召回结果）：`build_rules` 的表级判据要它
            let tables = audit_tables(sql);
            let rules =
                dms_semantic::registry::caliber::build_rules(pg, ds_reg::DMS_DS_ID, q, &tables)
                    .await
                    .unwrap_or_default();
            let v = dms_kernel::check_caliber(sql, &rules);
            if v.is_empty() {
                continue;
            }
            bad += 1;
            println!("[{status}] #{id} {q}");
            for x in &v {
                println!("    {} · {}", x.rule, x.hint);
            }
            if fix {
                sqlx::query("UPDATE meta.sql_exemplar SET status = 'disabled' WHERE id = $1")
                    .bind(id)
                    .execute(pg)
                    .await?;
            }
        }
        println!(
            "{} 条语料，{bad} 条违反声明{}",
            rows.len(),
            if fix { "（已置 disabled）" } else { "（加 --fix 置 disabled）" }
        );
        return Ok(());
    }

    // M6b 子命令：graph sync —— 聚合客户-商品购买边入 AGE 图
    if args.len() >= 3 && args[1] == "graph" && args[2] == "sync" {
        let owned = owned_store(&cfg).await?;
        let mysql = dms_source(&cfg, &owned).await?;
        let docs = document_graph_specs();
        let assets = warehouse_graph_specs();
        let (nc, ng, ne) =
            dms_connector::graph::sync(&mysql, owned.pool(), &docs, &assets).await?;
        println!("{}", serde_json::json!({ "customers": nc, "goods": ng, "edges": ne }));
        return Ok(());
    }

    // M2 子命令：retrieve "<问题>" —— 三路召回冒烟
    if args.len() >= 3 && args[1] == "retrieve" {
        let owned = owned_store(&cfg).await?;
        let pg = owned.pool();
        // 问句向量在调用侧算一次（semantic 不持 HTTP 客户端）；embed 缺席则向量路降级跳过
        let qvec = embed::embed_query(&args[2]).await.map(|v| embed::to_pgvector(&v));
        let cx = dms_semantic::recall::RecallCtx {
            question: &args[2],
            tables: &[],
            limit: 6,
            ds: ds_reg::DMS_DS_ID,
            embed: qvec.as_deref(),
            embed_slices: &[],
        };
        let ctxs = dms_semantic::recall::retrieve(pg, &cx).await?;
        let table_names: Vec<String> = ctxs.iter().map(|c| c.table_name.clone()).collect();
        let pitfalls = dms_semantic::recall::recall_pitfalls(
            pg,
            &dms_semantic::recall::RecallCtx { tables: &table_names, limit: 5, ..cx },
        )
        .await?;
        println!(
            "{}",
            serde_json::json!({
                "tables": ctxs.iter().map(|c| serde_json::json!({
                    "table": c.table_name, "score": c.score, "forced": c.forced,
                })).collect::<Vec<_>>(),
                "pitfalls": pitfalls,
                "schema_chars": ctxs.iter().map(|c| c.schema_text.len()).sum::<usize>(),
            })
        );
        return Ok(());
    }

    // 驻留评测子命令：eval-batch —— stdin NDJSON / stdout NDJSON。
    // 只复用一次启动成本；每题仍重新加载 DMS 身份并走生产权限闸门，不缓存 Principal/Scope。
    if args.len() >= 2 && args[1] == "eval-batch" {
        anyhow::ensure!(args.len() == 2, "用法：dms-ai-server eval-batch（请求从 stdin NDJSON 读取）");
        let owned = owned_store(&cfg).await?;
        let mysql = dms_source(&cfg, &owned).await?;
        let auth_mysql = auth_source(&cfg).await?;
        let pg = owned.pool();
        bootstrap_meta(&owned, &mysql).await?;
        let client = llm_client(&cfg);
        let sources = SourceRegistry::new(cfg.dsn_map());
        for (name, target) in &cfg.mysql_targets {
            sources.set_policy(&DsId::new(name), target.policy());
        }
        sources.preload(mysql.clone());
        let embed = dms_connector::embed::EmbedClient::new(&cfg.service_url);

        use std::io::{BufRead, Write};
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut output = std::io::BufWriter::new(stdout.lock());
        for line in stdin.lock().lines() {
            let response = match line {
                Ok(line) => match serde_json::from_str::<EvalBatchReq>(&line) {
                    Ok(req) => match req.validate() {
                        Ok(()) => {
                            eval_batch_one(
                                req,
                                &client,
                                &auth_mysql,
                                &mysql,
                                &sources,
                                pg,
                                &embed,
                                cfg.sc_samples,
                            )
                            .await
                        }
                        Err(e) => eval_batch_output(
                            req.id,
                            None,
                            None,
                            0,
                            0,
                            vec![format!("protocol: {e}")],
                        ),
                    },
                    Err(e) => eval_batch_output(
                        serde_json::Value::Null,
                        None,
                        None,
                        0,
                        0,
                        vec![format!("protocol: {e}")],
                    ),
                },
                Err(e) => eval_batch_output(
                    serde_json::Value::Null,
                    None,
                    None,
                    0,
                    0,
                    vec![format!("stdin: {e}")],
                ),
            };
            serde_json::to_writer(&mut output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
        return Ok(());
    }

    // M3 子命令：ask <login_name> "<问题>" [role_code] [上一轮问句] [上一轮SQL] —— 完整问答链
    //
    // 后两位是**多轮题的唯一表达方式**：判官（`tools/regression.py`）走的正是这条 CLI，
    // 而这里此前硬传 `None` 作 prev —— 于是 55 题里 **0 道两轮题**，
    // 「喂上一轮 schema/SQL」改了没有任何判据能证明（裁决 二·AN5-2）。
    // 位置式而不是 flag：这个子命令全是位置参数，加一套 flag 解析要给判官改三处调用点。
    if args.len() >= 4 && args[1] == "ask" {
        let owned = owned_store(&cfg).await?;
        let mysql = dms_source(&cfg, &owned).await?;
        let auth_mysql = auth_source(&cfg).await?;
        let pg = owned.pool();
        bootstrap_meta(&owned, &mysql).await?;
        let client = llm_client(&cfg);
        // 空串 = 该位缺省。位置式参数的必然代价：只传 prev 不传 role 时得给 role 占一个空位，
        // 而 `Some("")` 当 role_code 会让 `load_principal` 去查一个不存在的角色 —— 空串必须过滤掉。
        let slot = |i: usize| args.get(i).map(|s| s.as_str()).filter(|s| !s.is_empty());
        let p = principal::load_principal(&auth_mysql, &args[2], slot(4)).await?;
        // 判官链路（regression.py / kb_eval.py 走这条）与服务共用同一条选源+闸门管道
        let sources = SourceRegistry::new(cfg.dsn_map());
        for (name, target) in &cfg.mysql_targets {
            sources.set_policy(&DsId::new(name), target.policy());
        }
        sources.preload(mysql.clone());
        // CLI 是短生命周期进程：图就绪状态不在内存里，靠持久化标记接管
        // （仅数仓目标可能有图；标记缺席/不是当前目标 = 回落非图路径，与未同步一致）。
        if mysql.is_warehouse() {
            let _ = dms_connector::graph::adopt_if_current(owned.pool(), &mysql.target_name()).await;
        }
        // 判官侧的 embed 客户端与服务侧同一份配置（`service_url`）：选源、语义缓存、召回三处
        // 共用一个实例，熔断状态随 `Clone` 共享 —— 各自 `new()` 会退化成三个独立熔断器。
        let embed = dms_connector::embed::EmbedClient::new(&cfg.service_url);
        // `cfg.sc_samples` 而不是写死 1：**判官必须与服务同一组参数**（同 `exec-sql` 那条纪律）。
        // 写死会让「开了 SC 之后评测有没有变好」这个问题永远量不出来。
        // 只给 prev 问句、不给 prev SQL 也是**有意义的一档**（= 上一轮失败/走了知识库）：
        // 那一档 `rewrite_followup` 必须一次 LLM 都不调，两轮题正好用它当反面用例。
        // 第三位【证据引用】恒空：CLI 没有「圈选上轮结果」的输入面（与 conv_id=None 同一个约定）。
        let prev = slot(5).map(|q| (q, slot(6), &[] as &[&str]));
        let (r, log) =
            ask(&client, &auth_mysql, &mysql, &sources, pg, &embed, &p, &args[3], prev, None, None, cfg.sc_samples)
                .await;
        // CLI 是一次性进程：不 await 写入句柄，`main` 返回时 spawn 出的 INSERT 还没跑，
        // 进程退出连任务一起带走 —— `query_log` 整行丢失（实测）。服务侧则直接丢弃句柄。
        let _ = log.await;
        let r = r?;
        println!("{}", serde_json::to_string(&r)?);
        return Ok(());
    }

    // 评测子命令：exec-sql <login_name> "<sql>" [role_code] —— 以该用户身份执行给定 SQL。
    // 三道防线一个不少（只读红线 → 权限注入 → 只读连接），供 tools/evaluation.py 跑 gold SQL 对拍。
    if args.len() >= 4 && args[1] == "exec-sql" {
        let owned = owned_store(&cfg).await?;
        let mysql = dms_source(&cfg, &owned).await?;
        let auth_mysql = auth_source(&cfg).await?;
        bootstrap_meta(&owned, &mysql).await?;
        let p = principal::load_principal(&auth_mysql, &args[2], args.get(4).map(|s| s.as_str())).await?;
        let scope = scope::compute_scope_cached(&auth_mysql, &p).await?;
        // 判官与服务共用同一条闸门（`dms_agent::gate`）——评测可信的前提是走的就是生产那条管道。
        // ⚠️ gold SQL 不带 LIMIT 时会被 `check()` 追加 `LIMIT 200`：这是**已知且已记录**的行为
        // 变化（docs/ARCHITECTURE.md §7「基线迁移注意」），凡 gold 返回 >200 行的题对拍结果会变，
        // 不是 SQL 生成变好了。要全量的题在 eval_cases.json 里显式写 LIMIT。
        let scoped = dms_agent::gate(&p, &args[3], &scope, &dms_kernel::MysqlDialect)?;
        let t0 = std::time::Instant::now();
        // 与服务同一条取数路径（含敏感列脱敏与行上限）——判官可信的前提是走的就是生产那条管道
        let rs = mysql.fetch(&scoped, dms_agent::MAX_ROWS, dms_agent::EXEC_TIMEOUT).await?;
        println!(
            "{}",
            serde_json::json!({
                "sql": scoped.wire(),
                "columns": rs.columns,
                "rows": rs.rows,
                "row_count": rs.rows.len(),
                "elapsed_ms": t0.elapsed().as_millis() as u64,
            })
        );
        return Ok(());
    }

    // 判官子命令：scope <login_name> [role_code] —— 输出权限集合 JSON + t_sales_order 注入示例
    if args.len() >= 3 && args[1] == "scope" {
        let auth_mysql = auth_source(&cfg).await?;
        let login = &args[2];
        let role = args.get(3).map(|s| s.as_str());
        let p = principal::load_principal(&auth_mysql, login, role).await?;
        let scope = scope::compute_scope(&auth_mysql, &p).await?;
        let sets = scope.sets();
        let demo = dms_policy::inject(
            "SELECT COUNT(*) AS cnt FROM t_sales_order so WHERE so.deleted_flag = 0",
            &sets,
        )?;
        println!(
            "{}",
            serde_json::json!({
                "principal": p,
                "sets": {
                    "employee_ids": sets.employee_ids,
                    "employee_codes": sets.employee_codes,
                    "customer_codes": sets.customer_codes,
                    "login_names": sets.login_names,
                    "manager_customer_codes": sets.manager_customer_codes,
                    "shop_codes": sets.shop_codes,
                    "unrestricted": sets.is_unrestricted(),
                },
                "demo_sql": demo,
            })
        );
        return Ok(());
    }

    let owned = owned_store(&cfg).await?;
    let auth_mysql = auth_source(&cfg).await?;
    let mysql = dms_source(&cfg, &owned).await?;
    bootstrap_meta(&owned, &mysql).await?;
    chat::migrate(owned.pool()).await?;
    // 顺序有依赖：kb.chunk 用 vector(512) 与 gin_trgm_ops，两个扩展由 meta 迁移的
    // CREATE EXTENSION 建（0020 自己不建）——必须排在 bootstrap_meta 之后。
    dms_knowledge::store::migrate(&owned).await?;
    // 重启收割：上次进程遗留的「进行中」评估/图谱构建永远等不到终态（后台任务随进程死了），
    // 启动时统一标 failed（error='服务重启中断'）。排在各 migrate 之后：表已就绪。
    kb_eval_api::reap_interrupted(&owned).await?;
    kg_api::reap_interrupted(owned.pool()).await?;

    // 多源注册中心：映射的键是 **dsn_ref 键名**（不是 ds_id）——`registry.dsn(spec)` 按
    // `spec.dsn_ref` 查表。主分析源只允许通过 preload 命中当前非 dms 连接池；即便元数据里
    // 留有历史 `dsn_ref="mysql_url"`，`Settings::dsn_map` 也刻意不暴露 DMS 权限 DSN，避免
    // preload 缺失时懒连接回权限库。
    let sources = Arc::new(SourceRegistry::new(cfg.dsn_map()));
    for (name, target) in &cfg.mysql_targets {
        sources.set_policy(&DsId::new(name), target.policy());
    }
    sources.preload(mysql.clone());

    let state = Arc::new(AppState {
        auth_mysql,
        mysql,
        owned,
        sources,
        llm: llm_client(&cfg),
        dms_base_url: cfg.dms_base_url.clone(),
        wework: wework::WeworkCfg {
            corpid: cfg.wework_corpid.clone(),
            secret: cfg.wework_secret.clone(),
            agentid: cfg.wework_agentid.clone(),
            redirect_url: cfg.wework_redirect_url.clone(),
        },
        doc: dms_connector::doc::DocService::new(&cfg.service_url),
        embed: dms_connector::embed::EmbedClient::new(&cfg.service_url),
        kb_cfg: dms_knowledge::ingest::IngestCfg {
            root: std::path::PathBuf::from(&cfg.kb_root),
            max_bytes: cfg.kb_max_mb * 1024 * 1024,
        },
        mcp_keys: cfg.mcp_keys.clone(),
        graph_status: Arc::new(std::sync::Mutex::new(String::from("never"))),
        sc_samples: cfg.sc_samples,
        insight_enabled: cfg.insight_enabled,
        insecure_login_fallback: cfg.insecure_login_fallback,
        cfg: std::sync::RwLock::new(cfg.clone()),
        settings_write: tokio::sync::Mutex::new(()),
    });

    // 【双供应商】运行时开关：`meta.kv['llm_provider']` 有记录就覆盖文件配置（保存即生效）
    admin_api::apply_runtime_llm_provider(&state).await;
    // kv['mysql_target'] 已在 `dms_source` 连上时应用（serve 与 CLI 同一条管道）——
    // 这里不再重复换（换一次和换两次终态相同，但白白建两轮池）。

    // M6c：AGE 图 nightly 定时刷新（本地 03:00 低谷期，一次性全量重建 ~4min；
    // 失败记 warn 次日重试，不影响服务）。图数据当日增量靠次日刷新补齐。
    {
        // 整个 `AppState` 进 spawn：`OwnedStore` 不是 Clone（刻意的——池只有一份）
        let st = state.clone();
        tokio::spawn(async move {
            // 启动补偿：从未同步（never）或上次失败（fail）先补一轮 —— 图问句不该等凌晨 3 点
            // （电脑重启后 status 回 never，今天的购买边可能已经是上周的）
            let stale = {
                let s = st.graph_status.lock().unwrap().clone();
                s.starts_with("never") || s.starts_with("fail")
            };
            if stale {
                tracing::info!("graph sync 启动补偿：{}", st.graph_status.lock().unwrap());
                graph_sync_and_record(&st).await;
            }
            loop {
                let wait = secs_until_next_3am();
                tracing::info!("graph sync 定时刷新：{wait}s 后（下个本地 03:00）执行");
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                tracing::info!("graph sync 定时刷新开始");
                graph_sync_and_record(&st).await;
            }
        });
    }
    // 【A9】向量自愈：启动即跑一轮 + 每 10 分钟扫 `embedding IS NULL` 补齐（启动批量 embed
    // 会拖慢启动 ⇒ 后台 spawn + 失败只 warn；多实例由 PG advisory lock 选一个跑）
    embed_fill::spawn(state.clone());
    // 【S5】经营日报：同 A9 的调度模子（lock + 10min + kv 标记），产物写 meta.artifact
    daily_digest::spawn(state.clone());
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/login", post(api_login))
        .route("/api/sso", post(api_sso))
        .route("/api/wework/start", get(api_wework_start))
        .route("/api/wework/login", get(api_wework_login))
        .route("/api/session/role", post(api_session_role))
        .route("/api/ask", post(api_ask))
        // 【S1】可信结果反馈 + 管理员质量控制面。反馈只绑定本人 trace，统计在 PG 内聚合。
        .route("/api/feedback", post(quality_api::feedback))
        .route("/api/admin/quality", get(quality_api::quality))
        .route("/api/admin/feedback/{id}/status", post(quality_api::set_feedback_status))
        // 【A15】冷启动推荐问句（enabled 语料 + 兜底四条）
        .route("/api/suggest", get(api_suggest))
        // 【S1】artifact 预览地基（产物与日报共用一张表；/view 是 CSP 沙箱页）
        .route("/api/artifact", post(artifact_api::create))
        .route("/api/artifact/list", get(artifact_api::list))
        .route("/api/artifact/{id}/view", get(artifact_api::view))
        .route("/api/artifact/{id}/download", get(artifact_api::download))
        // 【分享】发链接/撤销（属主）与免登录分享视图（token 即能力）
        .route("/api/artifact/{id}/share", post(artifact_api::share))
        .route("/api/artifact/{id}/unshare", post(artifact_api::unshare))
        // 【D6】产物层：版本链回看 / 表格导出 / 跨会话引用（权限全部单点收口在 handler 内）
        .route("/api/artifact/{id}/versions", get(artifact_api::versions))
        .route("/api/artifact/{id}/export", get(artifact_api::export))
        .route("/api/artifact/{id}/promote", post(artifact_api::promote))
        .route("/api/artifact/shared/{token}", get(artifact_api::shared))
        // 【双供应商】能力查询（前端按它显隐图片入口；**非管理面**，不含 key）
        .route("/api/llm/capabilities", get(api_llm_capabilities))
        .route("/api/vision/chat", post(vision_api::chat))
        // 【双供应商】配置查看与热切换（保存即生效，不需要重启）
        .route("/api/admin/llm-config", get(admin_api::llm_config))
        .route("/api/admin/llm-provider", post(admin_api::set_llm_provider))
        // 【业务库热切换】目标目录（脱敏 host）与热切换（保存即生效；只读校验不过不换）
        .route("/api/admin/db-config", get(admin_api::db_config))
        .route("/api/admin/db-target", post(admin_api::set_db_target))
        // 【页面编辑配置】分析目标 / DMS 权限连接 / LLM 的可写面（写文件 + 内存热更新）
        .route("/api/admin/settings-catalog", get(settings_api::catalog))
        .route("/api/admin/settings/mysql-target", post(settings_api::put_mysql_target))
        .route("/api/admin/settings/mysql-target/{name}", delete(settings_api::del_mysql_target))
        .route("/api/admin/settings/llm-key", post(settings_api::put_llm_key))
        .route("/api/admin/settings/llm-key/{name}", delete(settings_api::del_llm_key))
        // 【测试连通性】DB / LLM（只验不写）+ 自定义供应商 CRUD
        .route("/api/admin/settings/test-db", post(settings_api::test_db))
        .route("/api/admin/settings/test-llm", post(settings_api::test_llm))
        .route("/api/admin/settings/llm-provider", post(settings_api::put_llm_provider))
        .route("/api/admin/settings/llm-provider/{name}", delete(settings_api::del_llm_provider))
        .route("/api/admin/settings/fallback-vision", post(settings_api::set_fallback_vision))
        // 【Y3】RRF 四路辅助召回权重页内编辑（admin 门禁，保存即热生效）
        .route("/api/admin/settings/kb-rrf-weights", post(settings_api::put_kb_rrf_weights))
        // 【AI 解读】按需拉取：前端点了才调 fast 模型。**刻意不并进 `/api/ask`** ——
        // 评测/回归的 p95 基线不许为一笔与判分无关的 LLM 调用买单（理由在 `insight_api.rs` 文件头）。
        .route("/api/analysis", post(insight_api::analysis))
        // 【S2】解读固化成报表 artifact（零 LLM：caliber 重算、insight 是回声）
        .route("/api/analysis/report", post(insight_api::report))
        // 【深度模式】复合产出单入口：总值+维度拆解+趋势+明细+图表+AI 分析 → artifact 富页
        .route("/api/deep/compose", post(deep_api::compose))
        // 【D4】深度报告手动续跑（已完成板块零重跑；双保险认领防并发执行器）
        .route("/api/deep/resume", post(deep_api::resume))
        // 【思维过程】进度轮询（Codex 式：阶段清单，无数据只阶段名）
        .route("/api/deep/progress", get(deep_api::progress))
        .route("/api/roles", get(api_roles))
        .route("/api/convs", get(api_convs))
        .route("/api/conv/new", post(api_conv_new))
        .route("/api/conv/{id}", get(api_conv_msgs).delete(api_conv_delete))
        // 【K1】知识库。上传单挂 body limit：axum 默认 2MB 会先于配置触发
        //（症状是「配置写着 50MB 却报 413」）。并发闸在 `kb_api::UPLOAD_GATE`（4 许可 → 429）。
        .route(
            "/api/kb/upload",
            post(kb_api::upload).layer(axum::extract::DefaultBodyLimit::max(
                (cfg.kb_max_mb * 1024 * 1024) as usize,
            )),
        )
        .route("/api/kb/docs", get(kb_api::docs))
        .route("/api/kb/spaces", get(kb_api::spaces).post(kb_api::create_space))
        .route("/api/kb/folders", get(kb_api::folders).post(kb_api::create_folder))
        .route(
            "/api/kb/folder/{id}",
            post(kb_api::update_folder).delete(kb_api::delete_folder),
        )
        .route(
            "/api/kb/space/{id}/grant",
            get(kb_api::space_grants).post(kb_api::grant_space).delete(kb_api::revoke_space),
        )
        .route("/api/kb/doc/{id}", get(kb_api::doc).delete(kb_api::delete))
        .route("/api/kb/doc/{id}/folder", post(kb_api::move_doc))
        .route("/api/kb/doc/{id}/download", get(kb_api::download_doc))
        .route("/api/kb/doc/{id}/reprocess", post(kb_api::reprocess))
        // 【Y12/Y7】KB 运营：URL 抓取入库（SSRF 护栏）/ 空间导出 / AI 生成文档描述
        .route("/api/kb/ingest-url", post(kb_api::ingest_url))
        .route("/api/kb/space/{id}/export", get(kb_api::export_space))
        .route("/api/kb/doc/{id}/description", post(kb_api::generate_description))
        .route("/api/kb/doc/{id}/state", post(kb_api::set_doc_state))
        .route("/api/kb/doc/{id}/metadata", post(kb_api::update_doc_metadata))
        // 【K2】问答 + 引用原文回查。ACL 都在 knowledge 侧的 SQL 里，这里只接线。
        .route("/api/kb/ask", post(kb_api::ask))
        .route("/api/kb/search", post(kb_api::search))
        .route("/api/kb/chunk/{id}", get(kb_api::chunk))
        .route("/api/kb/sample-questions", get(usage_api::sample_questions))
        .route("/api/skills", get(skills_api::list).post(skills_api::create))
        .route("/api/skills/{id}", put(skills_api::update).delete(skills_api::remove))
        .route("/api/skills/{id}/toggle", post(skills_api::toggle))
        .route("/api/chat/conv/{id}/trace", get(trace_api::conv_trace))
        .route("/api/chat/conv/{id}/branch", post(api_conv_branch))
        // 【Y5】运行中插话（steer 并入当前问题上下文重走一次组装，仅一次防循环）
        .route("/api/chat/conv/{id}/steer", post(chat::api_conv_steer))
        .route("/api/usage/summary", get(usage_api::usage_summary))
        // 【数据地图】目录节点 / 统一边列表 / 两级路径 + 推断边人工复核门 + SQL 全状态审计
        .route("/api/datamap/nodes", get(datamap_api::nodes))
        .route("/api/datamap/edges", get(datamap_api::edges))
        .route("/api/datamap/paths", get(datamap_api::paths))
        .route("/api/datamap/relations", get(datamap_api::relations))
        .route("/api/datamap/edges/{id}/accept", post(datamap_api::accept))
        .route("/api/datamap/edges/{id}/reject", post(datamap_api::reject))
        .route("/api/audit/sql", get(datamap_api::audit_sql))
        .route("/api/kb/doc/{id}/markdown", get(kb_mindmap_api::doc_markdown))
        .route("/api/kb/doc/{id}/chunks", get(kb_mindmap_api::doc_chunks))
        .route("/api/kb/mindmap", get(kb_mindmap_api::mindmap))
        .route("/api/kb/mindmap/regenerate", post(kb_mindmap_api::regenerate_mindmap))
        .route("/api/kb/eval/runs", get(kb_eval_api::list_runs).post(kb_eval_api::create_run))
        .route("/api/kb/eval/runs/{id}", get(kb_eval_api::get_run))
        .route("/api/kb/graph/build", post(kg_api::build))
        .route("/api/kb/graph/status", get(kg_api::status))
        .route("/api/kb/graph/subgraph", get(kg_api::subgraph))
        .route("/api/kb/graph/stats", get(kg_api::stats))
        // 【Y4】图谱运营三件套：失败块清单（读）/ 按空间清图（写）/ 删改后修复（写，dry-run 默认）
        .route("/api/kb/graph/failed-chunks", get(kg_api::failed_chunks))
        .route("/api/kb/graph/reset", post(kg_api::reset))
        .route("/api/kb/graph/reconcile", post(kg_api::reconcile))
        // 【K3-A】数据源管理。读按 ds 级可见性，写/probe/sync 一律 administrator_flag。
        .route("/api/ds", get(ds_api::list).post(ds_api::upsert))
        .route("/api/ds/{id}", delete(ds_api::remove))
        .route("/api/ds/{id}/probe", post(ds_api::probe))
        .route("/api/ds/{id}/sync", post(ds_api::sync))
        // 【K6-C】数据源授权收发。第三个静态段（probe/sync/grant 互不重叠）
        .route("/api/ds/{id}/grant", post(admin_api::grant).delete(admin_api::revoke))
        // 【K6-C】管理面：术语 / SQL 示例复核。示例只有「复核状态」入口，没有 POST 新建
        // （人写的示例不进链，理由在 `admin_api` 文件头）。
        // ⚠️ `admin_api::ROUTES` **不守这张路由表**：实测在这里加一条
        // `.route("/api/bogus_reverse_check", get(health))`，`-p dms-ai-server` 全套单测
        // 141 条一条不红（ROUTES 唯一消费者是它自己那条单测，wire 侧根本不读它）。
        // 它只保证「admin_api 自己没长出新增示例的端点」，别把它当端点清单的事实源。
        .route(
            "/api/admin/terms",
            get(admin_api::terms).post(admin_api::upsert_term).delete(admin_api::delete_term),
        )
        .route("/api/admin/exemplars", get(admin_api::exemplars))
        // 【B6】术语/示例的 CSV 往返与批量复核。示例库**只导出不导入**（纪律见 admin_api 头）
        .route("/api/admin/terms.csv", get(admin_api::terms_csv).post(admin_api::import_terms_csv))
        // 【A11】schema 注释业务自助维护（导出→填→回传，只写 custom_comment）
        .route("/api/admin/schema-comments.csv",
               get(admin_api::schema_comments_csv).post(admin_api::import_schema_comments_csv))
        // 【A20】表级人工启停（enabled=false 不进任何召回）
        .route("/api/admin/table-enabled", post(admin_api::set_table_enabled))
        // 【A23】HITL edit：人改 SQL → 闸门 → 执行 → 沉淀待复核
        .route("/api/admin/sql-edit", post(admin_api::sql_edit_exec))
        .route("/api/admin/exemplars.csv", get(admin_api::exemplars_csv))
        .route("/api/admin/exemplars/{id}/status", post(admin_api::set_exemplar_status))
        .route("/api/admin/exemplars/status", post(admin_api::set_exemplars_status))
        .route("/api/admin/exemplars/{id}", delete(admin_api::delete_exemplar))
        // 【K6-A】对外 MCP（JSON-RPC 2.0）。**刻意不挂会话鉴权**：它自带 X-API-Key
        //（`mcp_keys` 为空时恒 404 = 默认关）。套会话中间件会让所有 MCP 调用 401。
        .route("/api/mcp", post(mcp_api::mcp))
        // 【小程序接入】x-access-token 桥接校验 + 问答（契约见 xcx_api.rs 文件头）
        .route("/api/xcx/ask", post(xcx_api::ask))
        .route("/api/xcx/me", get(xcx_api::me))
        .with_state(state);

    // 🔴 认证回退开着必须**每次启动都吼一声**：它等于「任何能到达端口的人可冒充任何 login_name」。
    // 只在文档里写一句不够 —— 配置是会被人抄走的，而抄的人不读文档。
    if cfg.insecure_login_fallback {
        tracing::warn!(
            listen = %cfg.listen,
            "⚠️ insecure_login_fallback=true：无会话 token 时采信请求自报的 login_name              —— 任何能连到上面这个地址的人都能冒充任意用户（含管理员）。             仅供本机判官脚本使用；生产请删掉该键，并把 docker 端口映射收到 127.0.0.1。"
        );
    }
    tracing::info!("dms-ai server listening on {}", cfg.listen);
    let listener = tokio::net::TcpListener::bind(&cfg.listen).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct SsoReq {
    /// DMS 的 x-access-token（iframe 嵌入时由 DMS 前端透传）
    dms_token: String,
    /// DMS 当前激活角色（可选，前端知道）
    role_code: Option<String>,
}

fn sso_role(
    roles: &[String],
    requested: Option<&str>,
    administrator: bool,
) -> anyhow::Result<Option<String>> {
    let requested = requested
        .map(|role| auth::normalized_role(role).ok_or_else(|| anyhow::anyhow!("角色无效")))
        .transpose()?;
    match requested {
        Some(role) if roles.iter().any(|r| r == role) => Ok(Some(role.to_string())),
        Some("admin") if administrator && roles.is_empty() => Ok(Some("admin".into())),
        Some(role) => anyhow::bail!("该账号无角色 {role}"),
        None if roles.len() == 1 => Ok(Some(roles[0].clone())),
        None if roles.is_empty() && administrator => Ok(Some("admin".into())),
        None => Ok(None),
    }
}

/// SSO 换签：验真 DMS token → 颁自有会话 token
async fn api_sso(
    State(st): State<Arc<AppState>>,
    Json(req): Json<SsoReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let err = |code: StatusCode, msg: String| (code, Json(serde_json::json!({ "error": msg })));
    if st.dms_base_url.trim().is_empty() {
        return Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            "DMS SSO 地址未配置，请联系管理员".into(),
        ));
    }
    let login_name = auth::verify_dms_token(&st.dms_base_url, &req.dms_token)
        .await
        .map_err(|_| err(StatusCode::UNAUTHORIZED, "DMS 身份认证失败，请重新登录".into()))?;
    let (identity, roles) = tokio::join!(
        auth::active_identity(&st.auth_mysql, &login_name),
        principal::list_roles(&st.auth_mysql, &login_name),
    );
    let administrator = identity
        .map_err(|_| err(StatusCode::SERVICE_UNAVAILABLE, "DMS 账号状态校验暂不可用".into()))?
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "DMS 账号已禁用或已删除".into()))?;
    let roles = roles
        .map_err(|_| err(StatusCode::SERVICE_UNAVAILABLE, "读取 DMS 角色失败".into()))?;
    let roles: Vec<String> = roles
        .into_iter()
        .filter(|role| administrator || role != "admin")
        .collect();
    if roles.is_empty() && !administrator {
        return Err(err(StatusCode::FORBIDDEN, "该账号无可用角色".into()));
    }
    let role = sso_role(&roles, req.role_code.as_deref(), administrator)
        .map_err(|_| err(StatusCode::FORBIDDEN, "所选角色不可用".into()))?;
    let token = auth::issue_from(login_name.clone(), role.clone(), auth::SessionSource::DmsSso)
        .map_err(|_| err(StatusCode::SERVICE_UNAVAILABLE, "会话服务暂不可用".into()))?;
    Ok(Json(serde_json::json!({
        "token": token,
        "login_name": login_name,
        "roles": roles,
        "active": role,
    })))
}

#[derive(serde::Deserialize)]
struct WeworkQuery {
    code: String,
    state: String,
}

fn cookie_value<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

async fn api_wework_start(State(st): State<Arc<AppState>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    match wework::oauth_start(&st.wework) {
        Ok((url, state)) => {
            let secure = st.wework.redirect_url.starts_with("https://");
            let mut headers = axum::http::HeaderMap::new();
            let cookie = wework::oauth_cookie(&state, secure);
            let Ok(cookie) = axum::http::HeaderValue::from_str(&cookie) else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            headers.insert(axum::http::header::SET_COOKIE, cookie);
            (headers, axum::response::Redirect::temporary(&url)).into_response()
        }
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "企业微信登录未配置" })),
        )
            .into_response(),
    }
}

/// 企微 OAuth 回调：code → 员工 → 会话 token，302 重定向前端带 token
async fn api_wework_login(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<WeworkQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let secure = st.wework.redirect_url.starts_with("https://");
    let clear_cookie = wework::clear_oauth_cookie(secure);
    let clear = |mut response: axum::response::Response| {
        if let Ok(cookie) = axum::http::HeaderValue::from_str(&clear_cookie) {
            response.headers_mut().insert(axum::http::header::SET_COOKIE, cookie);
        }
        response
    };
    if !wework::consume_oauth_state(
        &q.state,
        cookie_value(&headers, wework::OAUTH_STATE_COOKIE),
    ) {
        return clear((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "企业微信登录状态无效，请重新打开应用" })),
        ).into_response());
    }
    match wework::login_by_code(&st.wework, &st.auth_mysql, &q.code).await {
        Ok(login_name) => {
            let administrator = match auth::active_identity(&st.auth_mysql, &login_name).await {
                Ok(Some(flag)) => flag,
                Ok(None) => return clear((
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({ "error": "企业微信账号在 DMS 中不可用" })),
                ).into_response()),
                Err(_) => return clear((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({ "error": "DMS 账号状态校验暂不可用" })),
                ).into_response()),
            };
            let roles = match principal::list_roles(&st.auth_mysql, &login_name).await {
                Ok(roles) => roles,
                Err(_) => return clear((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({ "error": "DMS 角色读取暂不可用" })),
                ).into_response()),
            };
            let roles: Vec<String> = roles
                .into_iter()
                .filter(|role| administrator || role != "admin")
                .collect();
            if roles.is_empty() && !administrator {
                return clear((
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({ "error": "该账号无可用角色" })),
                ).into_response());
            }
            let active = match roles.len() {
                1 => Some(roles[0].clone()),
                0 if administrator => Some("admin".into()),
                _ => None,
            };
            let token = match auth::issue_from(login_name, active, auth::SessionSource::Wework) {
                Ok(token) => token,
                Err(_) => return clear((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({ "error": "会话服务暂不可用" })),
                ).into_response()),
            };
            // 重定向前端，会话 token 走 fragment（不进服务端日志）
            clear(axum::response::Redirect::to(&format!("/#token={token}")).into_response())
        }
        Err(_) => clear((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "企业微信登录失败，请重试" })),
        ).into_response()),
    }
}

#[derive(serde::Deserialize)]
struct LoginReq {
    login_name: String,
    password: String,
}

/// 独立 UI 登录：账号密码与 DMS `t_employee` 同源，角色与后续数据权限仍实时读取 DMS。
/// 本端没有 DMS 强制修改密码页，因此仅此密码入口拒绝过期密码；SSO/企微不应用该限制。
async fn api_login(
    State(st): State<Arc<AppState>>,
    Json(req): Json<LoginReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let err = |code: StatusCode, msg: &str| (code, Json(serde_json::json!({ "error": msg })));
    if auth::normalized_login(&req.login_name).is_none() || req.password.is_empty() || req.password.len() > 256 {
        return Err(err(StatusCode::BAD_REQUEST, "请输入账号和密码"));
    }
    if !auth::login_allowed(&req.login_name) {
        return Err(err(StatusCode::TOO_MANY_REQUESTS, "登录失败次数过多，请 5 分钟后重试"));
    }
    let verified = auth::verify_password(&st.auth_mysql, &req.login_name, &req.password)
        .await
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "登录服务暂不可用"))?;
    auth::record_login(&req.login_name, verified.is_some());
    let Some((login_name, administrator)) = verified else {
        return Err(err(StatusCode::UNAUTHORIZED, "账号或密码错误，账号已禁用或密码已过期"));
    };
    let roles = principal::list_roles(&st.auth_mysql, &login_name)
        .await
        .map_err(|_| err(StatusCode::SERVICE_UNAVAILABLE, "读取 DMS 角色失败"))?;
    let roles: Vec<String> = roles
        .into_iter()
        .filter(|role| administrator || role != "admin")
        .collect();
    if roles.is_empty() && !administrator {
        return Err(err(StatusCode::FORBIDDEN, "该账号无可用角色"));
    }
    let active = match roles.len() {
        1 => Some(roles[0].clone()),
        0 if administrator => Some("admin".into()),
        _ => None,
    };
    let token = auth::issue(login_name.clone(), active.clone())
        .map_err(|_| err(StatusCode::SERVICE_UNAVAILABLE, "会话服务暂不可用"))?;
    Ok(Json(serde_json::json!({
        "token": token, "login_name": login_name, "roles": roles, "active": active,
    })))
}

#[derive(serde::Deserialize)]
struct SessionRoleReq {
    role_code: String,
}

/// 三端统一角色换签：只能修改当前已认证员工的激活角色，角色必须真实归属于该员工。
async fn api_session_role(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<SessionRoleReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let err = |code: StatusCode, msg: String| (code, Json(serde_json::json!({ "error": msg })));
    let old_token = bearer(&headers)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token".into()))?;
    let session = auth::resolve_session(&old_token)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token".into()))?;
    let login_name = session.login_name.clone();
    let role = auth::normalized_role(&req.role_code)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "角色不能为空".into()))?;
    let principal_role = session.principal_role_for(role);
    let p = principal::load_principal(&st.auth_mysql, &login_name, Some(&principal_role))
        .await
        .map_err(|_| err(StatusCode::FORBIDDEN, "角色不可用或不属于当前账号".into()))?;
    let token = auth::issue_from(login_name.clone(), Some(p.role_code.clone()), session.source)
        .map_err(|_| err(StatusCode::SERVICE_UNAVAILABLE, "会话服务暂不可用".into()))?;
    auth::revoke(&old_token);
    Ok(Json(serde_json::json!({
        "token": token,
        "login_name": login_name,
        "role_code": p.role_code,
    })))
}

#[derive(serde::Deserialize)]
struct AskReq {
    question: String,
    /// 报表模板可把完整分析指令放在 `question`，同时用短标题写入会话和产物页。
    /// 目前仅深度报表消费；普通问数忽略它，保持原有协议兼容。
    #[serde(default)]
    display_question: Option<String>,
    /// 开发/内网模式的直接身份传递；生产走 Authorization Bearer 会话 token
    login_name: Option<String>,
    role_code: Option<String>,
    /// 归属会话 id（多轮问答存进同一会话）
    conv_id: Option<i64>,
    /// 【K5】前端能力 chip：`自动` 传 null（后端分诊），`data`/`knowledge` 强制走一条路。
    /// K5 之前 serde 静默忽略这个字段，所以老前端与 `tools/kb_eval.py` 的 body 都照旧能用。
    intent: Option<String>,
    /// 【KB】显式知识空间；只在 Knowledge 分支消费，ACL 仍在检索 SQL 内生效。
    space_id: Option<String>,
    /// 【K3】显式选源（前端数据源下拉）。不传 = 后端选源（可见源只有一个时直通主源）。
    /// 传了但对本人不可见 → 403（`select_source` 的 ① 分支不吃降级：那会把「无权」变成「换个源给你查」）。
    ds: Option<String>,
    /// 【深度模式】`"deep"` = AI 深度参与：生成侧 SC 采样抬到 ≥3（多数派投票），
    /// 分析侧由前端串 `/api/analysis?deep`（Precise 档四段式）。其余值/缺省 = 精简模式，
    /// 老前端与判官脚本的 body 一字不用改。
    mode: Option<String>,
    /// 【思维过程】进度轮询 id（`/api/deep/progress?rid=`）：前端生成 uuid 带上，
    /// 服务端逐阶段登记，前端轮询渲染。缺省 = 不登记（老前端零变化）。
    #[serde(default)]
    rid: Option<String>,
    /// 【证据引用】追问携带的上轮结果片段（EvidenceRef 简化形，`docs/research/datafoundry.json` A3）。
    /// 只在多轮改写里当指代消解素材 —— agent 侧收口（剥控制字符、截 500 字、最多 3 段），
    /// 不进 SQL 生成链、不参与权限判定。缺省/null/空数组 = 与引入前逐字等价，
    /// 老前端与判官脚本的 body 一字不用改。
    #[serde(default)]
    refs: Option<Vec<String>>,
}

/// 从 header/body 解析身份（Bearer 会话 token 优先，回退 login_name）
/// 请求身份：**Bearer 会话 token 是唯一可信来源**。
///
/// 🔴 `login_name` 回退默认**关**（`Settings::insecure_login_fallback`）。开着等于没有认证：
/// 能连到端口的人写一个 `?login_name=<别人>` 就以那个人的身份跑。见那个字段的文档。
/// 这里是全仓 13 个调用点的**唯一收口** —— 改这一处，`/api/ask`、`/api/kb/*`、`/api/ds/*`、
/// `/api/admin/*`、`/api/analysis`、会话那几个端点全部跟着变。
fn resolve_identity(
    st: &AppState,
    headers: &axum::http::HeaderMap,
    ln: &Option<String>,
    rc: &Option<String>,
) -> Option<(String, Option<String>)> {
    // 【D10】双通道身份（契约见 `auth::resolve_identity_dual` 文档）：
    // X-API-Key / Bearer <API key> 命中 mcp_keys → 该 login（role 恒 None 由 load_principal
    // 现算，与 MCP 同语义）；显式错 key = fail-closed（**不**落 login_name 自报回退）；
    // Bearer 会话 token 语义一字不变。
    let key_hdr = headers.get("X-API-Key").and_then(|v| v.to_str().ok());
    let bearer_tok = bearer(headers);
    match auth::resolve_identity_dual(&st.cfg().mcp_keys, key_hdr, bearer_tok.as_deref()) {
        auth::IdentityChannel::ApiKey(login) => Some((login, None)),
        auth::IdentityChannel::Session(session) if session.role_code.is_some() => {
            let role = session.principal_role();
            Some((session.login_name, role))
        }
        auth::IdentityChannel::Session(_) | auth::IdentityChannel::BadKey => None,
        // 回退**必须**由显式开关放行：默认 None ⇒ 调用方一律返 401
        auth::IdentityChannel::Absent if st.insecure_login_fallback => ln
            .as_deref()
            .and_then(auth::normalized_login)
            .map(|login| {
                let role = rc.as_deref().and_then(auth::normalized_role).map(str::to_string);
                (login.to_string(), role)
            }),
        auth::IdentityChannel::Absent => None,
    }
}

/// 【双供应商】能力查询（任意认证用户）：当前供应商 + 视觉能力有无。
/// 前端按它显隐图片入口；**不含 key 与 base_url**（能力布尔，不是配置面）。
async fn api_llm_capabilities(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let err = |code: StatusCode, msg: String| (code, Json(serde_json::json!({ "error": msg })));
    let _ = resolve_identity(&st, &headers, &q.get("login_name").cloned(), &q.get("role_code").cloned())
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name".into()))?;
    let (_, fast, precise, primary_vision, _) = st.llm.public_conf();
    let primary_provider = st.llm.primary_provider();
    let fallback_provider = st.llm.fallback_vision_provider();
    let effective_vision = st.llm.vision_capability();
    Ok(Json(serde_json::json!({
        "provider": primary_provider,
        "model_fast": fast,
        "model_precise": precise,
        "primary_supports_vision": primary_vision.is_some(),
        "fallback_vision_provider": fallback_provider,
        "vision": effective_vision.is_some(),
        "vision_model": effective_vision.as_ref().map(|v| v.model.clone()),
        "vision_provider": effective_vision.as_ref().map(|v| v.provider.clone()),
        "vision_fallback": effective_vision.as_ref().map(|v| v.fallback).unwrap_or(false),
    })))
}

/// 【A15】冷启动推荐：人工复核过（enabled）的真实问句，不足时兜底固定四条。
/// 不需要 LLM：它们既是真实问法又背着「问过、对过」两层验证（`suggest_questions` 的注释）。
/// 治的是 `guard.rs` `constant_projection` 那个实测现场：用户只发一个名字（「嗨肉」），
/// 无意图输入本来就该被引导掉，而不是被闸门拒掉。
async fn api_suggest(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let err = |code: StatusCode, msg: String| (code, Json(serde_json::json!({ "error": msg })));
    let (login_name, role_code) =
        resolve_identity(&st, &headers, &q.get("login_name").cloned(), &q.get("role_code").cloned())
            .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name".into()))?;
    let _ = principal::load_principal(&st.auth_mysql, &login_name, role_code.as_deref())
        .await
        .map_err(|_| err(StatusCode::FORBIDDEN, "当前账号或角色不可用".into()))?;
    let mut qs =
        dms_semantic::registry::exemplar::suggest_questions(st.owned.pool(), ds_reg::DMS_DS_ID, 6)
            .await;
    // 语料不足时兜底（冷启动第一天语料库是空的，推荐位不能也是空的）
    const FALLBACK: &[&str] =
        &["本月销售额是多少", "销售额按省份", "有多少订单", "今年退款额是多少"];
    for f in FALLBACK {
        if qs.len() >= 6 {
            break;
        }
        if !qs.iter().any(|x| x == f) {
            qs.push(f.to_string());
        }
    }
    Ok(Json(serde_json::json!({ "suggestions": qs })))
}

async fn api_ask(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<AskReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let err = |code: StatusCode, msg: String| (code, Json(serde_json::json!({ "error": msg })));
    let (login_name, role_code) = resolve_identity(&st, &headers, &req.login_name, &req.role_code)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "未认证：缺会话 token 或 login_name".into()))?;
    let p = principal::load_principal(&st.auth_mysql, &login_name, role_code.as_deref())
        .await
        .map_err(|_| err(StatusCode::FORBIDDEN, "当前账号或角色不可用".into()))?;
    // 会话归属校验：非属主禁止读写（防越权借他人 conv_id 泄露上一问/写入消息）
    if let Some(cid) = req.conv_id {
        match chat::conv_owner(st.owned.pool(), cid).await {
            Ok(Some(owner)) if owner == login_name => {}
            Ok(_) => return Err(err(StatusCode::FORBIDDEN, "无权访问该会话".into())),
            Err(_) => return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "会话状态读取失败，请稍后重试".into(),
            )),
        }
    }
    // 多轮追问改写用的上一轮 (问句, 那一轮执行的 SQL)（同会话）。
    // SQL 那一半是新加的载荷：上一轮的口径（哪张表/哪个时间列/哪个过滤）只在 SQL 里。
    // 取不到就当首问（不失败）；但**必须留痕**：静默丢上下文的症状是「追问答得像换了个问题」，
    // 用户看不出、日志里也查不到（AS5 审计条目）。降级可接受，不可见不行。
    let prev = match req.conv_id {
        Some(cid) => chat::last_turn(st.owned.pool(), cid)
            .await
            .inspect_err(|_| {
                tracing::warn!(conv_id = cid, reason = "chat_context_load_failed", "取上一轮失败，本轮按首问处理")
            })
            .ok()
            .flatten(),
        None => None,
    };
    // 【证据引用】追问携带的上轮结果片段，打包进 `PrevTurn` 第三位随改写透传。
    // 没有上一轮（首问/新会话）时引用无处附着，随 `prev = None` 一起不落 ——
    // 「上轮结果引用」本就以上轮为前提，这是刻意而不是丢数据。
    // 截断/剥控制字符/段数上限都在 agent 侧收口（`refs_section_of`），这里只透传原文。
    let refs: Vec<&str> = req.refs.as_deref().unwrap_or(&[]).iter().map(String::as_str).collect();
    // 【K5】意图分诊。`ds` 用主源：判据只读 `meta.metric/dimension/term`（谓词 `IN (ds,'*')`），
    // 而选源发生在 `dms_agent::ask` 内部——分诊只决定「问数还是查文档」，不决定问哪个源。
    // 分诊不会失败（内部一律降级 Data），故这里没有 `?`：存量问数链路一步不变。
    let intent =
        triage::triage(&st.llm, st.owned.pool(), ds_reg::DMS_DS_ID, &req.question, req.intent.as_deref())
            .await;
    let payload = match intent {
        triage::Intent::Data => {
            let (r, _log) = ask(
                &st.llm,
                &st.auth_mysql,
                &st.mysql,
                &st.sources,
                st.owned.pool(),
                &st.embed,
                &p,
                &req.question,
                prev.as_ref().map(|(q, s)| (q.as_str(), s.as_deref(), refs.as_slice())),
                req.ds.as_deref(),
                // 会话 id 透传到 `query_log` 与三张日志表 —— `chat.rs:117` 的亏就是
                // 「query_log 没有 conv_id，从它拿不回本会话上一轮」
                req.conv_id.map(|c| c.to_string()).as_deref(),
                // 【深度模式】SC 抬到 ≥3：多数派投票是现成的「AI 深度参与生成」件
                //（配置 sc_samples 已 ≥3 时不降 —— max 不是 overwrite）
                if req.mode.as_deref() == Some("deep") { st.sc_samples.max(3) } else { st.sc_samples },
            )
            .await;
            // 服务侧 fire-and-forget：`_log` 句柄直接丢弃，HTTP 主链路一个 `.await` 都不多
            let r = r
            // 「无权访问数据源」是权限拒绝，必须 403 而不是 422：前者前端提示「联系管理员授权」，
            // 后者会被当成「这个问题问不出来」，用户永远不知道是权限问题。
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("无权访问数据源") {
                    err(StatusCode::FORBIDDEN, "当前账号无权访问该数据源".into())
                } else {
                    err(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "暂时无法完成本次问数，请调整问题后重试".into(),
                    )
                }
            })?;
            serde_json::to_value(&r).unwrap()
        }
        // `Answer` 带 `kind:"text"`，前端 K2 的 `KbAnswer` 分支按它分派；`route` 是 `"knowledge"`
        triage::Intent::Knowledge => {
            let a = kb_answer(&st, &p, req.space_id.as_deref(), &req.question)
                .await
                .map_err(|_| err(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "暂时无法完成知识检索，请稍后重试".into(),
                ))?;
            serde_json::to_value(&a).unwrap()
        }
    };
    // 存会话消息（用户问 + AI 结果），首问顺手设标题
    if let Some(cid) = req.conv_id {
        let _ = chat::save_msg(st.owned.pool(), cid, "user", &req.question, None).await;
        let _ = chat::save_msg(st.owned.pool(), cid, "ai", "", Some(&payload)).await;
    }
    Ok(Json(payload))
}

/// 一次问答的**服务端唯一入口**：观测出口（`Trace` + 查询日志）+ 依赖注入 → `dms_agent::ask`。
/// HTTP（`api_ask`）/ MCP（`mcp_api::tool_ask`）/ CLI（`ask` 子命令，判官链路）三处共用它 ——
/// 判官与服务走同一条管道，是评测数字可信的前提。
///
/// 为什么这一层留在 server：`Trace` 与 `query_log` 带 axum，落不进 agent（门禁第 3 条），
/// 所以用量（K6-B）与选源两个观测出口都以回调传下去；
/// 五个校正器与三个快路径判据的实现也仍在 server（`corrector.rs` / `direct.rs`，那两处各有一行
/// ponytail 记账，T8/T10 消掉）。
#[allow(clippy::too_many_arguments)] // 形参 = 拆分前 `pipeline::ask` 那 8 个 + embed 客户端
async fn ask(
    llm: &llm::LlmClient,
    auth: &ReadOnlyMySql,
    dms: &ReadOnlyMySql,
    registry: &SourceRegistry,
    pg: &sqlx::PgPool,
    embed: &dms_connector::embed::EmbedClient,
    p: &principal::Principal,
    question: &str,
    // 上一轮 (问句, 那一轮执行的 SQL, 用户引用的上轮结果片段)。刻意**改类型而不加形参**：
    // `mcp_api::tool_ask` 也调这个函数（无会话状态、恒传 `None`），裸 `None` 换个内层类型
    // 照样编得过，多一个形参它就红。第三位【证据引用】只有 HTTP 聊天有输入面，
    // MCP / CLI / 深度子问恒空 —— 空 refs 时改写提示词与引入前逐字相同。
    // 走模块路径（`ask::`）而不是 crate 根：本轮不改 `agent/src/lib.rs` 的 re-export 表，
    // 那张表是「一个符号一条 use 路径」的真相源，进它要与该文件的改动面一起走。
    prev: Option<dms_agent::ask::PrevTurn<'_>>,
    explicit_ds: Option<&str>,
    // 会话 id（`chat.msg.conv_id`）。HTTP 聊天有它时透传到 `query_log` 与三张日志表 ——
    // `chat.rs:117` 的亏就是「query_log 没有 conv_id，从它拿不回本会话上一轮」。
    // CLI / `mcp_api::tool_ask` 无会话概念恒传 `None`（与 `explicit_ds` 同一个约定）。
    conv_id: Option<&str>,
    sc_samples: usize,
    // 返回值带观测写入句柄：服务侧调用方丢弃它（fire-and-forget，主链路不多一次往返），
    // CLI 一次性进程必须 await —— 否则进程退出时 spawn 出的 INSERT 还没跑（实测整行丢失）。
) -> (anyhow::Result<dms_agent::AskResult>, tokio::task::JoinHandle<()>) {
    let t0 = std::time::Instant::now();
    // 【K6-B】观测出口：一次问答一行，成功与失败都写。`finish` 内部 `tokio::spawn` 异步写、
    // 失败只 warn —— 主链路一个 `.await` 都不多（多一次往返就多一次超时面）。
    let trace = query_log::Trace::default();
    // 🔴 一次问答一个 `trace_id`（三表共用）+ 一次会话一个 `conv_id`。
    // `correction_log` / `failure_log` / `query_log` 三张表原来各记一段、拼不回同一次问答 —
    // 「数字错了是模型写错还是校正器改坏」查不出来（`chat.rs:117` 已吃过一次这个亏）。
    let trace_id = uuid::Uuid::new_v4().to_string();
    // HTTP 有 `conv_id` 用它；CLI 没有会话概念时与 `trace_id` 相同（单轮即单会话）。
    let conv_id = conv_id.map(str::to_string).unwrap_or_else(|| trace_id.clone());
    trace.set_trace(&trace_id, &conv_id);
    tracing::info!(trace_id = %trace_id, conv_id = %conv_id, "一次问答的关联键已生成");
    // `AskCtx.llm` 要 `&Arc<dyn ChatModel>`（语料复核与失败复盘要 `tokio::spawn`）。
    // `LlmClient::clone` 共享同一个底层 HTTP 连接池（见 `llm.rs`），这次装箱不多出第二个客户端。
    // 门禁 ④「server 的 HTTP 客户端只许出现在身份面文件」不过滤注释行 —— 那个库名连写在
    // 注释里都会判红，本文件刻意不提它（实测撞过一次）。
    let llm: Arc<dyn dms_kernel::ChatModel> = Arc::new(llm.clone());
    let correctors = corrector::DmsCorrectors;
    // 两个观测回调必须先绑定成局部：直接写 `&|u| ...` 是临时值，`deps` 借它会活不过这条语句
    let on_usage = |u: &dms_kernel::llm::Usage| trace.add(u);
    let on_ds = |ds: &str| trace.set_ds(ds);
    let main_source_name = dms.target_name();
    let deps = dms_agent::AskDeps {
        llm: &llm,
        auth,
        dms,
        registry,
        pg,
        embed,
        correctors: &correctors,
        // Router 三个成员的产出方（顺序由 agent 的 `router()` 定，这里只给实现）
        detect: direct::detect_relation,
        compose_hit: direct::compose_hit,
        direct_hit: direct::direct_hit,
        main_source_name: &main_source_name,
        on_usage: &on_usage,
        on_ds: &on_ds,
        trace_id,
        conv_id,
        sc_samples,
    };
    let out = dms_agent::ask(&deps, p, question, prev, explicit_ds).await;
    let log = query_log::finish(pg, &trace, &p.login_name, question, &out, t0.elapsed().as_millis());
    (out, log)
}

/// 【K5】分诊到 Knowledge 的落点。`space_id` 缺省＝**不限空间**（与 `/api/kb/ask` 同口径）：
/// 被授权看别人空间的人也得能检索到，ACL 由 `retrieve` 在 SQL 内把关，server 不拼第二份。
///
/// 错误由调用方一律映 422：`/api/ask` 今天对全部失败就是 422，不给同一个端点造第二套状态码表
/// （`kb_api::kb_err` 那张 400/403/404 表服务的是 `/api/kb/*`，在这里复述一份必漂）。
///
/// `Principal` → `Viewer` 的映射（含「`roles` 必须用**解出来的** `role_code`」那条依据）
/// 已迁 `dms_agent::answerers::knowledge::viewer`：那是分诊的另一条分支，两处口径必须同源，
/// 在这里再写一份 `Viewer::new` 就是埋一处会漂的身份映射。
async fn kb_answer(
    st: &AppState,
    p: &principal::Principal,
    space: Option<&str>,
    question: &str,
) -> Result<dms_kernel::Answer, dms_knowledge::KbError> {
    dms_agent::answerers::knowledge::answer(&st.owned, &st.embed, &st.llm, p, space, question, &st.cfg().kb_rrf_weights).await
}

#[derive(serde::Deserialize)]
struct ConvQuery {
    login_name: Option<String>,
}

/// 可选角色列表：多角色账号必须显式选角色（1:1 对齐 DMS「请选择登录角色」，
/// 不替用户默认选——不同角色数据权限档差异巨大）
async fn api_roles(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(_q): axum::extract::Query<ConvQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let session = bearer(&headers)
        .and_then(|token| auth::resolve_session(&token))
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "未认证" }))))?;
    let login = session.login_name;
    let active = session.role_code;
    let administrator = auth::active_identity(&st.auth_mysql, &login)
        .await
        .map_err(|_| (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "DMS 账号状态校验暂不可用" })),
        ))?
        .ok_or_else(|| (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "DMS 账号已禁用或已删除" })),
        ))?;
    let roles = principal::list_roles(&st.auth_mysql, &login)
        .await
        .map_err(|_| (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "DMS 角色读取暂不可用" })),
        ))?;
    let roles: Vec<String> = roles
        .into_iter()
        .filter(|role| administrator || role != "admin")
        .collect();
    if roles.is_empty() && !administrator {
        return Err((StatusCode::FORBIDDEN, Json(serde_json::json!({ "error": "该账号无可用角色" }))));
    }
    if let Some(role) = active.as_deref() {
        if !(administrator && role == "admin") && !roles.iter().any(|r| r == role) {
            return Err((StatusCode::FORBIDDEN, Json(serde_json::json!({ "error": "当前角色已失效，请重新选择" }))));
        }
    }
    Ok(Json(serde_json::json!({
        "login_name": login,
        "roles": roles,
        "active": active,
    })))
}

async fn api_convs(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ConvQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (login, _) = resolve_identity(&st, &headers, &q.login_name, &None)
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "未认证" }))))?;
    let convs = chat::list_convs(st.owned.pool(), &login).await.unwrap_or_default();
    Ok(Json(serde_json::json!({ "convs": convs })))
}

async fn api_conv_new(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(q): Json<ConvQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (login, _) = resolve_identity(&st, &headers, &q.login_name, &None)
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "未认证" }))))?;
    let id = chat::new_conv(st.owned.pool(), &login)
        .await
        .map_err(|_| (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "新建会话失败，请稍后重试" })),
        ))?;
    Ok(Json(serde_json::json!({ "id": id })))
}

async fn api_conv_msgs(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::extract::Query(q): axum::extract::Query<ConvQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (login, _) = resolve_identity(&st, &headers, &q.login_name, &None)
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "未认证" }))))?;
    // 会话归属校验（防越权读他人会话）
    match chat::conv_owner(st.owned.pool(), id).await {
        Ok(Some(owner)) if owner == login => {}
        _ => return Err((StatusCode::FORBIDDEN, Json(serde_json::json!({ "error": "无权访问该会话" })))),
    }
    let msgs = chat::conv_msgs(st.owned.pool(), id).await.unwrap_or_default();
    Ok(Json(serde_json::json!({ "msgs": msgs })))
}

async fn api_conv_delete(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::extract::Query(q): axum::extract::Query<ConvQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (login, _) = resolve_identity(&st, &headers, &q.login_name, &None)
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "未认证" }))))?;
    let _ = chat::delete_conv(st.owned.pool(), id, &login).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 分支会话：从 `from_seq`（1 基消息序号，缺省=整条）处 fork 出新会话并复制前缀消息。
/// 属主校验与复制在同一事务里（chat::branch_conv），非属主/不存在统一 403 不泄存在性。
async fn api_conv_branch(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(req): Json<BranchReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (login, _) = resolve_identity(&st, &headers, &req.login_name, &None)
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "未认证" }))))?;
    match chat::branch_conv(st.owned.pool(), id, &login, req.from_seq).await {
        Ok(Some((conv_id, copied))) => Ok(Json(serde_json::json!({ "conv_id": conv_id, "copied": copied }))),
        Ok(None) => Err((StatusCode::FORBIDDEN, Json(serde_json::json!({ "error": "无权访问该会话" })))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() })))),
    }
}

#[derive(serde::Deserialize)]
struct BranchReq {
    login_name: Option<String>,
    from_seq: Option<i64>,
}

/// 距下一个本地 03:00 的秒数（AGE 图 nightly 刷新，对齐业务低谷）
fn secs_until_next_3am() -> u64 {
    let now = chrono::Local::now();
    let Some(t3) = now.date_naive().and_hms_opt(3, 0, 0) else {
        return 3600;
    };
    let Some(today3) = t3.and_local_timezone(chrono::Local).single() else {
        return 3600;
    };
    let target = if now < today3 { today3 } else { today3 + chrono::Duration::days(1) };
    (target - now).num_seconds().max(60) as u64
}

/// 跑一轮图同步并记录 status（启动补偿与 03:00 定时共用 —— 同一处失败文案与日志）
async fn graph_sync_and_record(st: &AppState) {
    if !st.mysql.is_warehouse() {
        let msg = format!(
            "skip {} production_lookup_target={}",
            chrono::Local::now().format("%F %T"),
            st.mysql.target_name()
        );
        tracing::info!("graph sync 已跳过：当前目标不是数仓");
        *st.graph_status.lock().unwrap() = msg;
        return;
    }
    let docs = document_graph_specs();
    let assets = warehouse_graph_specs();
    let msg = match dms_connector::graph::sync(&st.mysql, st.owned.pool(), &docs, &assets).await {
        Ok((c, g, e)) => {
            format!("ok {} customers={c} goods={g} edges={e}", chrono::Local::now().format("%F %T"))
        }
        Err(_) => format!("fail {} graph_sync_failed", chrono::Local::now().format("%F %T")),
    };
    if msg.starts_with("ok") {
        tracing::info!("graph sync 完成：{msg}");
    } else {
        tracing::warn!("graph sync 失败（次日重试）：{msg}");
    }
    *st.graph_status.lock().unwrap() = msg;
}

fn document_graph_specs() -> Vec<dms_connector::graph::DocumentGraphSpec> {
    dms_semantic::document::DOCUMENT_FAMILIES
        .iter()
        .map(|f| dms_connector::graph::DocumentGraphSpec {
            code: f.code.to_string(),
            name: f.name.to_string(),
            header_table: f.header_table.to_string(),
            detail_tables: f.details.iter().map(|(table, _)| (*table).to_string()).collect(),
        })
        .collect()
}

fn warehouse_graph_specs() -> Vec<dms_connector::graph::WarehouseAssetGraphSpec> {
    dms_semantic::warehouse_catalog::ASSETS
        .iter()
        .map(|asset| {
            let database = dms_semantic::warehouse_catalog::database_of(asset);
            let code = format!("{database}.{}", asset.table);
            let layer = match asset.layer {
                "ODS" | "DWD" | "DWS" | "ADS" => asset.layer,
                _ => "OTHER",
            };
            dms_connector::graph::WarehouseAssetGraphSpec {
                default_sales: code == dms_semantic::sales_fact::TABLE,
                code,
                table: asset.table.to_string(),
                database: database.to_string(),
                name: format!("{} · {}", asset.domain, asset.table),
                layer: layer.to_string(),
                domain: asset.domain.to_string(),
                grain: asset.grain.to_string(),
                time_rule: asset.time_rule.to_string(),
                metrics: asset.metrics.to_string(),
                forbidden: asset.forbidden.to_string(),
                comparison: asset.comparison.to_string(),
            }
        })
        .collect()
}

fn bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|s| s.to_string())
}

/// 自有 PG 必须有的三个扩展：`vector`（kb.chunk 的向量列）/ `pg_trgm`（trgm 召回）/ `age`（图查询）。
/// 少一个就有一整条链静默失效，故进 `ok`。
const REQUIRED_PG_EXTS: [&str; 3] = ["vector", "pg_trgm", "age"];

fn business_health_status(connected: bool, read_only: bool, timed_out: bool) -> &'static str {
    if timed_out {
        "busy"
    } else if connected && read_only {
        "ok"
    } else {
        "unavailable"
    }
}

async fn health(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // 业务池拥塞时旧实现会让 `/api/health` 一起排队，前端最后误报“后端未连接”。
    // 两秒只用于体检止损，不改变业务查询超时；无法确认只读仍 fail-closed。
    let mysql_probe = async {
        match tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let connected = st.mysql.ping().await;
            let read_only = connected && st.mysql.session_read_only().await;
            (connected, read_only)
        })
        .await
        {
            Ok((connected, read_only)) => (connected, read_only, false),
            Err(_) => (false, false, true),
        }
    };
    let pg_exts_probe = async {
        st.owned
            .fixed("SELECT extname FROM pg_extension ORDER BY 1")
            .fetch_all::<(String,)>()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(e,)| e)
            .collect::<Vec<String>>()
    };
    let vec_probe = dms_semantic::ddl::vector_ready(st.owned.pool());
    let ((mysql_ok, mysql_readonly, mysql_busy), pg_exts, vec_ready) =
        tokio::join!(mysql_probe, pg_exts_probe, vec_probe);
    // 🔴 修恒真判定：原来第三项是「扩展列表非空」——任何 PG 都非空（`plpgsql` 恒在），
    // 这一项从来没有为假过，等于 `ok` 只在报 MySQL 的状态。改成三个扩展逐个校验。
    let pg_exts_ok = REQUIRED_PG_EXTS.iter().all(|n| pg_exts.iter().any(|e| e == n));
    // 🔴 向量就绪必须**在 `ok` 里**，不是旁边挂个字段。
    //
    // `ddl::vector_ready` 的文档写着消费者含 `/api/health`，实际从来没接上 —— 于是
    // 「三条向量路全哑」这件事在健康检查里完全看不见：三个调用点都 `.unwrap_or_default()`
    // 静默降级，trgm 兜底把召回额度填满，外面一切正常。
    // 2026-07-31 实测点亮前：`meta.element` 1079 行 embedding **全 NULL**、`table_doc` 251 行全 NULL、
    // `datasource` 4 行 active / 0 行有向量 —— 而系统从上线起一直这样跑。
    // 体检取不到（PG 挂了）时报 `null` 并让 `ok` 为假：「查不出来」不许算通过。
    let vec_ready = vec_ready.ok();
    let vec_ok = vec_ready.as_ref().is_some_and(|v| v.table_doc && v.element && v.datasource);

    Json(serde_json::json!({
        "ok": mysql_ok && mysql_readonly && pg_exts_ok && vec_ok,
        "mysql": {
            "connected": mysql_ok,
            "session_read_only": mysql_readonly,
            "status": business_health_status(mysql_ok, mysql_readonly, mysql_busy),
            // 当前分析库目标名（热切换后与连接池同锁更新；名字不是凭据，可上报）
            "target": crate::admin_api::current_db_target_pub(&st).await,
        },
        "pg": { "extensions": pg_exts },
        // 三条向量路各自的通断。哪一条为 false 就跑 `python tools/embed_service.py build`
        "vector_ready": match &vec_ready {
            Some(v) => serde_json::json!({
                "table_doc": v.table_doc, "element": v.element, "datasource": v.datasource,
            }),
            None => serde_json::Value::Null,
        },
        // F3 自检（只读源角色看不见 meta/kb/chat）。本轮**还没有**建 PG 只读源，
        // 故报 null 而不是 true —— 没做过的检查不许报成通过。K3 接上 `PostgresSource` 后
        // 这里改报 `!st.ro_source.owned_schema_visible().await?`（那个方法每次重查，
        // 因为授权可以在启动之后被 GRANT 改坏）。
        "ro_source_isolated": serde_json::Value::Null,
        "graph_sync": st.graph_status.lock().map(|s| s.clone()).unwrap_or_default(),
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn eval_batch_ndjson_protocol_is_strict_and_sparse() {
        let req: super::EvalBatchReq = serde_json::from_str(
            r#"{"id":"E01","login":"admin","role":null,"q":"本月销售额"}"#,
        )
        .unwrap();
        req.validate().unwrap();
        assert_eq!(req.id, serde_json::json!("E01"));
        assert!(req.role.is_null() && req.gold_sql.is_none());

        // role 是协议必填位（值可为 null）；gold_sql 才是可省略位。
        assert!(serde_json::from_str::<super::EvalBatchReq>(
            r#"{"id":"E01","login":"admin","q":"本月销售额"}"#
        )
        .is_err());
        let out = super::eval_batch_output(req.id, None, None, 12, 0, vec![]);
        assert_eq!(out["ask_wall_ms"], 12);
        assert!(out.get("got").is_none() && out.get("gold").is_none() && out.get("error").is_none());
    }

    #[test]
    fn eval_batch_reuses_startup_but_not_identity_or_gate() {
        let src = include_str!("main.rs");
        assert!(src.contains(".with_writer(std::io::stderr)"),
                "运行日志没有统一写到 stderr，可能污染 stdout NDJSON");
        let source = src
            .split(concat!("async fn dms_", "source("))
            .nth(1)
            .expect("分析源初始化函数不见了")
            .split("/// 身份、角色与数据权限固定读取")
            .next()
            .unwrap();
        assert!(
            source.contains("admin_api::db_boot_target(owned, cfg).await?"),
            "eval-batch 的分析池没有读取热切换后的当前目标"
        );
        assert!(!source.contains("cfg.mysql_url"), "分析池回退到了 DMS 身份/权限库");

        let branch = src
            .split(concat!("if args.len() >= 2 && args[1] == ", "\"eval-batch\""))
            .nth(1)
            .expect("eval-batch CLI 分支不见了")
            .split("// M3 子命令：ask")
            .next()
            .unwrap();
        let loop_at = branch.find("for line in stdin.lock().lines()").expect("NDJSON 驻留循环不见了");
        for once in [
            "owned_store(&cfg)",
            "dms_source(&cfg, &owned)",
            "bootstrap_meta(&owned, &mysql)",
        ] {
            let at = branch.find(once).unwrap_or_else(|| panic!("一次初始化缺失：{once}"));
            assert!(at < loop_at, "{once} 被放进逐题循环，冷启动优化失效");
        }
        assert!(!branch.contains("println!(") && !branch.contains("eprintln!("),
                "eval-batch 分支向 stdout/stderr 写了非协议文本");
        assert!(branch.contains("serde_json::to_writer(&mut output, &response)")
                && branch.contains("output.write_all(b\"\\n\")")
                && branch.contains("output.flush()?"),
                "eval-batch 不是逐行 JSON 并立即 flush");

        let one = src
            .split(concat!("async fn eval_batch_", "one("))
            .nth(1)
            .expect("逐题处理函数不见了")
            .split("#[tokio::main]")
            .next()
            .unwrap();
        assert!(one.contains(") -> serde_json::Value"), "单题处理仍会用 Result 把错误冒泡并杀死驻留进程");
        assert!(one.contains("principal::load_principal(auth_mysql"), "逐题没有重载 DMS 身份");
        assert!(one.contains("ask(") && one.contains("mysql,") && one.contains("sources,"),
                "问答没有使用驻留进程当前分析池/注册表");
        assert!(one.contains("let eval_ds = Some(ds_reg::DMS_DS_ID)")
                && one.contains("q.trim(),") && one.contains("eval_ds,"),
                "generated 没有锁定当前逻辑主分析源，可能与 gold 跨库对拍");
        assert!(one.contains("scope::compute_scope_cached(auth_mysql, p)"), "gold 没按同身份计算权限");
        assert!(one.contains("dms_agent::gate(p, sql, &user_scope"), "gold 绕过了生产 SQL gate");
        assert!(one.contains(".fetch(&scoped"), "gold 没走当前只读分析源");
        for isolated in ["identity: {e}", "ask: {e}", "gold: {e}"] {
            assert!(one.contains(isolated), "单题错误没有收敛到该行 error：{isolated}");
        }
    }

    #[test]
    fn sso_role_is_single_role_automatic_and_multi_role_fail_closed() {
        let one = vec!["city_manager".to_string()];
        assert_eq!(super::sso_role(&one, None, false).unwrap().as_deref(), Some("city_manager"));
        let many = vec!["admin".to_string(), "city_manager".to_string()];
        assert_eq!(super::sso_role(&many, None, false).unwrap(), None);
        assert_eq!(super::sso_role(&many, Some("city_manager"), false).unwrap().as_deref(), Some("city_manager"));
        assert!(super::sso_role(&many, Some("unknown"), false).is_err());
        assert!(super::sso_role(&one, Some("\n"), false).is_err());
        assert_eq!(super::sso_role(&[], None, true).unwrap().as_deref(), Some("admin"));
    }

    #[test]
    fn next_3am_within_a_day() {
        // 下次 03:00 必在 (60s, 24h] 内
        let s = super::secs_until_next_3am();
        assert!((60..=24 * 3600).contains(&s), "{s}");
    }

    /// 🔴 **三表必须共用同一个 `trace_id`** —— `correction_log` / `failure_log` / `query_log`
    /// 原来各记一段、拼不回同一次问答（`chat.rs:117` 的亏：「数字错了是模型写错还是
    /// 校正器改坏」查不出来）。这条钉的是「`trace_id` 真的透传到了三张表」：
    /// `AskCtx`（agent 侧）、`log_correction_traced` / `log_failure_traced`（semantic 侧）、
    /// `query_log::insert`（server 侧）三处各一处引用，缺一处就拼不回。
    ///
    /// 源码扫描判据（这是「位置/透传」类事实，无库单测碰不到）。
    /// 锚点用 `concat!` 拼（AX17 的恒真坑：`split` 的第一个匹配会落在判据自己身上）。
    #[test]
    fn trace_id_reaches_all_three_log_tables() {
        let src = include_str!("main.rs");
        // ① server 侧生成 + 传给 AskDeps（`AskDeps` 里的字段名，与缩进无关）
        assert!(src.contains(concat!("let trace_id = uuid::Uuid::new_v4", "()")),
                "trace_id 的生成点没了");
        let deps = src.split(concat!("let deps = dms_agent::AskDeps", " {"))
            .nth(1)
            .expect("AskDeps 不见了 —— 判据锚点失效")
            .split("};")
            .next()
            .unwrap();
        assert!(deps.contains("trace_id,") && deps.contains("conv_id,"),
                "trace_id/conv_id 没进 AskDeps —— agent 拿不到：{deps}");
        // ② AskCtx 在 agent 里有这两个字段（透传的落点）
        let ctx = include_str!("../../agent/src/ctx.rs");
        assert!(ctx.contains("pub trace_id: String"), "AskCtx 的 trace_id 字段没了");
        assert!(ctx.contains("pub conv_id: String"), "AskCtx 的 conv_id 字段没了");
        // ③ 两张日志表各自收了它（不依赖完整签名 —— 参数类型会变，字段名不会）
        let ex = include_str!("../../semantic/src/registry/exemplar.rs");
        assert!(ex.contains("log_correction_traced") && ex.contains("trace_id"),
                "correction_log 没吃 trace_id");
        assert!(ex.contains("log_failure_traced") && ex.contains("trace_id"),
                "failure_log 没吃 trace_id");
        // ④ query_log 的 INSERT 里有这两列（空串落 NULL 的那两个 bind）
        let ql = include_str!("query_log.rs");
        assert!(ql.contains("trace_id") && ql.contains("conv_id") && ql.contains("llm_calls"),
                "query_log 的 INSERT 没带 trace_id/conv_id/llm_calls");
    }

    /// 🔴 **CLI 必须在退出前 await 观测写入句柄** —— `finish` 的 INSERT 走 `tokio::spawn`，
    /// CLI 是一次性进程，`main` 一返回运行时就带着没跑完的任务一起死（实测：CLI 问完
    /// `query_log` 查无新行，一度被误判成「`set_trace` 没写进去」查了一整轮）。
    /// 服务侧相反：句柄直接丢弃，主链路一个 `.await` 都不多。
    /// 这条钉的是「句柄从 `ask()` 一路返回到 CLI 调用点并被 await」这条链。
    #[test]
    fn cli_awaits_the_log_handle_before_exit() {
        let src = include_str!("main.rs");
        assert!(src.contains(concat!("let (r, ", "log) =")),
                "CLI 分支没接住 ask 返回的写入句柄");
        assert!(src.contains(concat!("let _ = log", ".await")),
                "CLI 分支没 await 写入句柄 —— 进程退出会带走 spawn 出的 INSERT");
        // ask 的返回类型必须是「结果 + 句柄」二元组，否则上面两句凑得出但链不通
        assert!(src.contains(concat!("JoinHandle", "<()>")),
                "ask 的返回类型没带写入句柄");
    }

    #[test]
    fn health_distinguishes_busy_from_unavailable() {
        assert_eq!(super::business_health_status(true, true, false), "ok");
        assert_eq!(super::business_health_status(false, false, true), "busy");
        assert_eq!(super::business_health_status(false, false, false), "unavailable");
        assert_eq!(super::business_health_status(true, false, false), "unavailable");
    }

    #[test]
    fn cli_keeps_auth_source_separate_from_analysis_source() {
        let src = include_str!("main.rs");
        let cli = src
            .split("if args.len() >= 4 && args[1] == \"ask\"")
            .nth(1)
            .expect("ask CLI 分支不见了")
            .split("// 评测子命令：exec-sql")
            .next()
            .unwrap();
        assert!(cli.contains("let auth_mysql = auth_source(&cfg).await?"), "CLI 没建立固定 DMS 认证源");
        assert!(
            cli.contains("principal::load_principal(&auth_mysql")
                && cli.contains("ask(&client, &auth_mysql, &mysql"),
            "CLI 又把热切换后的分析库当成身份/权限库了"
        );
    }

    /// 主源只能命中 preload 的分析池；历史 dsn_ref=mysql_url 也不能懒连回 DMS 权限库。
    #[test]
    fn source_registry_never_receives_dms_permission_dsn() {
        let db = include_str!("db.rs");
        let body = db
            .split("pub fn dsn_map(&self)")
            .nth(1)
            .expect("dsn_map 不见了")
            .split("\n    }\n")
            .next()
            .unwrap();
        assert!(body.contains("eq_ignore_ascii_case(\"mysql_url\")")
                && body.contains("eq_ignore_ascii_case(\"dms\")"),
                "通用注册表没有移除权限源保留键：{body}");
        assert!(body.contains("m.retain") && body.contains("endpoint_key(&self.mysql_url)"),
                "通用注册表没有过滤改名后的同库端点：{body}");
        assert!(!body.contains(concat!("m.insert(\"mysql_", "url\"")),
                "DMS 权限 DSN 被重新插入通用注册表：{body}");
        let main = include_str!("main.rs");
        assert!(main.contains("主分析源只允许通过 preload 命中当前非 dms 连接池"));
        assert!(main.contains("sources.preload(mysql.clone())"), "主分析池没有预载进注册表");
    }

    /// 【深度模式】接线判据：AskReq 有 mode 字段 + deep 时 SC 抬到 ≥3（max 不是 overwrite —
    /// 配置已更高时不许被拉低）+ 缺省路径一字不变。锚点 concat! 拼（自匹配家族，本仓第八次）。
    #[test]
    fn deep_mode_raises_sc_and_keeps_default_path_untouched() {
        let src = include_str!("main.rs");
        assert!(src.contains(concat!("mode: ", "Option<String>")), "AskReq 缺 mode 字段");
        assert!(
            src.contains(concat!("Some(\"deep\") { st.sc_samples.", "max(3) }")),
            "deep 时 SC 必须 max(3) —— 直接写成 3 会把配置里的更高值拉低"
        );
    }

    fn why(argv: &[&str]) -> Result<super::WhyArgs, String> {
        super::parse_why_args(&argv.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    /// `why-not-compose` 的参数解析：**flag 不许被当成问句**。
    /// 上一版是 `args.get(2)`，`--csv x` 会变成「问一句叫 --csv 的话」+ 多余参数被丢，
    /// 而那种失败**不会让任何东西变红** —— 报告照样打印。所以判据钉在这里。
    #[test]
    fn why_args_flags_are_not_questions() {
        // 无参 = 扫全量、题库走默认路径
        assert_eq!(why(&[]).unwrap(), super::WhyArgs::default());
        // --csv / --cases 各自落到自己的位子上，且**没有**变成问句
        let c = why(&["--csv", "tools/why_gates.csv"]).unwrap();
        assert_eq!(c.csv.as_deref(), Some("tools/why_gates.csv"));
        assert_eq!(c.question, None, "--csv 被当成问句了");
        let k = why(&["--cases", "tools/other.json"]).unwrap();
        assert_eq!(k.cases.as_deref(), Some("tools/other.json"));
        assert_eq!(k.question, None, "--cases 被当成问句了");
        // 单问 + flag 混用，顺序无关
        let m = why(&["本月各品牌销售额", "--csv", "o.csv", "--cases", "c.json"]).unwrap();
        assert_eq!(m.question.as_deref(), Some("本月各品牌销售额"));
        assert_eq!(m.csv.as_deref(), Some("o.csv"));
        assert_eq!(m.cases.as_deref(), Some("c.json"));
        // flag 在前、问句在后也要认（问句不许被 flag 的值位吃掉）
        let r = why(&["--csv", "o.csv", "本月各品牌销售额"]).unwrap();
        assert_eq!(r.question.as_deref(), Some("本月各品牌销售额"));
        assert_eq!(r.csv.as_deref(), Some("o.csv"));
        // 🔴 空位置参数必须被拒。`serve.ps1 -Cmd` 按空格切参，多一个空格就产出空 token
        // （`'why-not-compose ' -split ' '` → [why-not-compose][""]）。
        // 放它进问句臂 = 全量 38 题诊断静默降级成「问一句空话」、打印「（1题）」、退出 0。
        for bad in [vec![""], vec![" "], vec!["", "--csv", "x"], vec!["--csv", "x", ""]] {
            assert!(why(&bad).is_err(), "空位置参数被当成问句了：{bad:?}");
        }
    }

    /// 未知 flag / 缺值 / 多余位置参数：**必须报错**，不许静默吞掉
    #[test]
    fn why_args_reject_unknown_and_extra() {
        for bad in [
            vec!["--wat"],                    // 未知 flag
            vec!["--wat", "x"],               //   带值也一样
            vec!["--csv=x.csv"],              // `=` 形式没实现 → 报错，不许当问句
            vec!["--csv"],                    // 缺值
            vec!["--cases"],                  // 缺值
            vec!["--csv", "--cases", "x"],    // 值位上是另一个 flag = 缺值
            vec!["本月销售额", "按品牌"],       // serve.ps1 按空格切参丢维度的那个形状
        ] {
            assert!(why(&bad).is_err(), "{bad:?} 该报错却被接受了");
        }
    }

    /// 🔴 语料审计喂给 `build_rules` 的必须是**表名**，不是别名。
    /// 取错那一位（原实现 `(_, t)`）会让表级口径判据一条都匹配不上 →
    /// 「缺口径过滤」这一整类恒报干净，而那是这个子命令唯一的存在理由。
    #[test]
    fn audit_tables_are_table_names_not_aliases() {
        let t = super::audit_tables(
            "SELECT 1 FROM t_sales_order so JOIN t_sales_order_detail d ON d.order_id = so.id",
        );
        assert_eq!(t, vec!["t_sales_order", "t_sales_order_detail"]);
        for alias in ["so", "d"] {
            assert!(!t.iter().any(|x| x == alias), "别名混进表名清单：{t:?}");
        }
        // 无别名写法照样是表名
        assert_eq!(super::audit_tables("SELECT 1 FROM t_sales_order"), vec!["t_sales_order"]);
    }

    #[test]
    fn csv_row_quotes_commas_and_quotes() {
        // 不转义会串位，而串位后的 CSV 仍然「有八列」，是最难发现的那种坏
        assert_eq!(
            super::csv_row(&["1", "a,b", "他说\"嗨\""]),
            "\"1\",\"a,b\",\"他说\"\"嗨\"\"\""
        );
    }

    /// 重启收割接线：启动段在各 migrate 之后收割「进行中」的评估 run 与图谱构建，
    /// 否则它们永远卡在 running/building（后台任务已随上次进程死了）。
    #[test]
    fn startup_reaps_interrupted_eval_runs_and_graph_builds() {
        let src = include_str!("main.rs");
        let boot = src
            .split("dms_knowledge::store::migrate(&owned).await?;")
            .nth(1)
            .expect("启动段的 store::migrate 不见了 —— 判据锚点失效")
            .split("// 多源注册中心")
            .next()
            .unwrap();
        assert!(boot.contains("kb_eval_api::reap_interrupted(&owned).await?"),
                "启动段没收割被重启中断的评估 run");
        assert!(boot.contains("kg_api::reap_interrupted(owned.pool()).await?"),
                "启动段没收割被重启中断的图谱构建");
    }
}
