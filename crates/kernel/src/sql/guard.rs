//! 只读红线判定 + LIMIT 护栏。变更原因＝红线判据。
//! 搬运源：`pipeline.rs:61-138`（`is_safe_select` 与四个 helper）、`pipeline.rs:204-218`（`ensure_limit`）。
//!
//! 全部**参数化**：敏感列词表与行上限由 server/semantic 传入（kernel 零 DMS 语料）。

use std::ops::ControlFlow;

use sqlparser::ast::{SetExpr, Statement, TableFactor};
use sqlparser::parser::Parser;

use crate::errors::GuardError;
use crate::sql::dialect::Dialect;
use crate::sql::lex::strip_literals_and_comments;

/// 一个源的护栏配置。**故意不实现 `Default`**（I3）：漏传敏感列词表是编译错误，
/// 不是「默认空词表 = 全放行」的静默降级。实例由业务侧提供（`dms_semantic::DMS_GUARD`）。
pub struct GuardConfig {
    pub max_rows: usize,
    pub sensitive_cols: &'static [&'static str],
}

impl GuardConfig {
    pub const fn new(max_rows: usize, sensitive_cols: &'static [&'static str]) -> Self {
        Self { max_rows, sensitive_cols }
    }
}

/// 安全校验：单条 SELECT、无敏感列、无占位符幻觉。
pub fn is_safe_select_with(sql: &str, d: &dyn Dialect, sensitive: &[&str]) -> Result<(), GuardError> {
    let stmts = match Parser::parse_sql(d.parser(), sql) {
        Ok(stmts) => stmts,
        Err(e) => {
            // parser 拒收的方言形态（如 `INTO OUTFILE`）：仍要落到红线原因，
            // 不能只剩一句含糊的「语法不合法」。剥字面量后按词边界扫写操作词。
            let stripped = strip_literals_and_comments(sql).to_lowercase();
            if let Some(tok) = forbidden_token(&stripped) {
                return Err(GuardError::WriteToken(tok.to_string()));
            }
            return Err(GuardError::Parse(e.to_string()));
        }
    };
    if stmts.len() != 1 {
        return Err(GuardError::MultiStatement);
    }
    if !matches!(stmts[0], Statement::Query(_)) {
        return Err(GuardError::NotSelect);
    }
    // MySQL 可执行注释（/*! */、/*+ */）会被 sqlparser 当注释忽略但 MySQL 照跑——直接拒
    if sql.contains("/*!") || sql.contains("/*+") {
        return Err(GuardError::ExecutableComment);
    }
    // 🔴 只读红线（移植 deepagents text-to-sql-agent 硬拦范式）：AST 已锁 Query，
    // 此处是防 parser 盲区的第二道防线。剥掉字符串字面量与注释后按词边界扫——
    // 不误伤 remark LIKE '%update %' 类字面量，也不漏 "delete\nfrom" 换行形态；
    // update_time/deleted_flag 等带下划线列名是独立 token 不受影响。
    // 注：REPLACE 不入列（REPLACE() 是合法字符串函数；REPLACE INTO 语句已被 AST 层拒）。
    // `FOR UPDATE`/`FOR SHARE` 是合法 SELECT 的行锁子句：先从扫描文本剔除，
    // 由调用方在 AST 层按「行锁」拒绝（锁语义比「写操作词」准确，测试钉的正是前者）。
    let stripped = strip_literals_and_comments(sql).to_lowercase();
    let stripped = stripped.replace("for update", " ").replace("for share", " ");
    if let Some(tok) = forbidden_token(&stripped) {
        return Err(GuardError::WriteToken(tok.to_string()));
    }
    if let Some(schema) = system_schema_ref(&stripped) {
        return Err(GuardError::SystemSchema(schema.to_string()));
    }
    if let Some(col) = sensitive_ref(&stripped, sensitive) {
        return Err(GuardError::SensitiveColumn(col));
    }
    if constant_projection(&stmts[0]) {
        return Err(GuardError::ConstantProjection);
    }
    if let Some(e) = placeholder_issue(sql) {
        return Err(e);
    }
    Ok(())
}

/// 写操作词（按词边界扫剥掉字面量后的文本）。REPLACE 不入列：REPLACE() 是合法字符串函数，
/// REPLACE INTO 语句已被 AST 层拒。
///
/// 【A13】函数名黑名单（SQLBot 的分库危险函数清单）：`load_file` / `pg_read_file` 读服务器
/// 文件系统，`xp_cmdshell` / `utl_file` 是另外两家的同类。它们都是合法 SELECT 里的合法函数
/// （AST 锁 Query 锁不住函数），绕过只读红线的唯一通道就是这种函数 —— 只读库上它们
/// 要么报权限错要么真的读到文件。词边界扫描天然兼容（下划线保在 token 里，
/// `upload_time` 这类业务列不受影响）。
fn forbidden_token(stripped: &str) -> Option<&'static str> {
    const FORBIDDEN: &[&str] = &[
        "insert", "update", "delete", "drop", "alter", "truncate",
        "create", "merge", "grant", "revoke", "outfile", "dumpfile",
        "load_file", "pg_read_file", "pg_ls_dir", "xp_cmdshell", "utl_file",
    ];
    stripped
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .find_map(|tok| FORBIDDEN.iter().find(|f| **f == tok).copied())
}

/// 系统库/元数据库引用：业务问答绝无必要，而它们是「读别人的东西」的通道
/// （information_schema 拿全库结构、mysql.user 拿账号）。按 `库名.` 限定形态匹配，
/// 避免误伤 sys_no / meta_flag 这类列名。
/// 自有 PG 的 `meta.` / `kb.` / `chat.` 也在名单里（`ARCHITECTURE` §3 的 F3 修法②）。
///
/// ⚠️ **性质说清：这是纵深防御，不是在补一个可达漏洞。** LLM 产的 SQL 只会打到两处 ——
/// ① DMS MySQL（没有这三个 schema）；② 上传表格建出的 PG 源，而那里的角色**读不到**
/// 自有库（`PostgresSource::connect` 的 F3 启动期自检，看得见就 `Err`、服务起不来）。
/// 加这三项的理由是「零成本 + 误伤方向是多拒 + 让代码与 ARCHITECTURE 一致」，
/// 而不是「今天有人能读到 kb.chunk」。原注释把这件事记成「K3 上线后按 AST 判定」——
/// K3 已上线而那个判定从没做，于是那句注释成了一张空头承诺。
///
/// 业务表不会误伤：判据是 `库名.` 限定形态（`meta.`），而 DMS 的表都叫 `t_*`，
/// 列名 `meta_flag` / `sys_no` 这类不含点号。
fn system_schema_ref(stripped: &str) -> Option<&'static str> {
    const DENY: &[&str] = &[
        "information_schema.", "performance_schema.", "mysql.", "sys.", "pg_catalog.",
        "meta.", "kb.", "chat.",
    ];
    DENY.iter().find(|d| stripped.contains(**d)).copied()
}

/// 敏感列显式点名（词表由调用方传入，单一事实源在业务侧）。
/// 这一层只挡「写出列名」；SELECT * 由结果列脱敏兜底。
fn sensitive_ref(stripped: &str, sensitive: &[&str]) -> Option<String> {
    sensitive.iter().find(|c| stripped.contains(**c)).map(|c| c.to_string())
}

/// 占位符幻觉防线（旧项目实证：LLM 会编 '__ORDER_CODE__'/'xxx_PLACEHOLDER' 恒空自信答 0）。
/// 占位符藏在字面量里，须查原文而非 stripped。
fn placeholder_issue(sql: &str) -> Option<GuardError> {
    let lower = sql.to_lowercase();
    if lower.contains("__") && lower.contains('\'') {
        for frag in sql.split('\'') {
            if frag.starts_with("__") && frag.ends_with("__") {
                return Some(GuardError::UnfilledPlaceholder(frag.to_string()));
            }
        }
    }
    if lower.contains("_placeholder") {
        return Some(GuardError::PlaceholderHallucination);
    }
    None
}

/// **常量投影**：这条 SQL 一张业务表都没碰。
///
/// 🔴 由来（业主报的准确度问题，已实证）：聊天框只发一个客户名「嗨肉」——
/// 那个客户有 31567 单 / 144.6 万 —— LLM 却输出 `SELECT 1 AS \`探针结果\``，
/// 而我们执行它、把「探针结果 = 1」当答案给了用户。
/// 模型在**没有意图可循**时会编一个恒能执行的空壳，而空壳的结果看起来像正常答案
/// （有列名、有值、零报错、零告警）—— 与 `placeholder_issue` 是同一族缺陷。
///
/// 判据是**「有没有引用真表」**，不是「投影是不是字面量」：
/// `SELECT COUNT(*) FROM t_x` 的投影也不含列引用，但它查了表、是合法答案。
/// 所以只在**完全没有 FROM**（或 FROM 里只有派生表/常量表）时判 —— 宁漏不误伤。
///
/// 已知漏判方向（刻意）：`SELECT 1 FROM t_sales_order LIMIT 1` 查了表，判不出来。
/// 那种形态今天没见过，且真要挡得靠「投影有没有业务语义」——那是判不动的东西。
///
/// 🔴 **「有没有 FROM」必须看任意层级，不只看顶层** —— 否则它会把正确 SQL 判成试探。
///
/// 实证（评测 AS04「今年售后退款金额占销售额的比例」，**3/3 确定性失败**）：
/// `meta.metric` 里 `refund_ratio` 的 `agg_expr` 就是**两个标量子查询相除**，
/// 它的 `description` 还明写「必须各写成独立子查询再相除，不许 JOIN 后聚合」，
/// 而 `metric_card` 把这句原样渲进 prompt ⇒ 模型照口径卡写出来的必然是
/// `SELECT (SELECT SUM(a) FROM t_x …) / (SELECT SUM(b) FROM t_y …) AS 占比`
/// —— **顶层没有 FROM，两张真表都在投影的子查询里**。
/// 于是闸门把一条查了两张真表的正确 SQL 判成「模型的试探不是答案」，
/// repair 的错误文案对这个形状是假话，次轮 `bail!` 硬失败。
/// 连带后果：`direct.rs` 的装配器又因 `agg_expr` 含 `SELECT` 注定装不出 ⇒ 这一族占比派生指标
/// **永久失败**。
///
/// 修法是**放宽**：顶层短路保留（`SELECT * FROM (SELECT 1) t` 那条既有断言不许打坏），
/// 再补一个「任意层级出现过真表引用吗」的 Visitor。业主报的那个现场
/// （`SELECT 1 AS 探针结果`，一张表都没有）照旧被拒。
fn constant_projection(stmt: &Statement) -> bool {
    fn body_has_from(body: &SetExpr) -> bool {
        match body {
            SetExpr::Select(s) => !s.from.is_empty(),
            SetExpr::Query(q) => body_has_from(&q.body),
            // 集合运算：任一侧查了表就算查了（`SELECT 1 UNION SELECT * FROM t` 不该判）
            SetExpr::SetOperation { left, right, .. } => body_has_from(left) || body_has_from(right),
            _ => true, // 认不出的形态一律当「查了表」——宁漏不误伤
        }
    }
    match stmt {
        // 顶层有 FROM → 立刻放行（快路径，且保住既有语义）；
        // 顶层没有 → 再问「任意层级引了真表吗」，引了就放行
        Statement::Query(q) => !body_has_from(&q.body) && !references_any_table(stmt),
        _ => false,
    }
}

/// 整条语句里**任意层级**有没有引用一张具名表（`TableFactor::Table`）。
/// 派生表/常量表不算（它们是 `Derived`/`TableFunction` 等别的变体）。
///
/// 用 sqlparser 自己的 `Visit` 遍历，写法照 `caliber.rs` 的 `pre_visit_table_factor` ——
/// 手写递归会漏变体，而漏一个变体在这里的后果是把正确 SQL 判成试探（AS04 那种硬失败）。
fn references_any_table(stmt: &Statement) -> bool {
    use sqlparser::ast::{Visit, Visitor};
    struct Seek(bool);
    impl Visitor for Seek {
        type Break = ();
        fn pre_visit_table_factor(&mut self, tf: &TableFactor) -> ControlFlow<()> {
            if matches!(tf, TableFactor::Table { .. }) {
                self.0 = true;
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
    let mut s = Seek(false);
    let _ = stmt.visit(&mut s);
    s.0
}

/// LIMIT 护栏：非纯聚合且无 LIMIT → 追加 LIMIT max_rows
pub fn ensure_limit_with(sql: &str, d: &dyn Dialect, max_rows: usize) -> String {
    // AST 判定是否已有 LIMIT/FETCH——字面量含 "limit" 不再误判为已限流（漏判=无界扫描）
    let has_limit = Parser::parse_sql(d.parser(), sql)
        .ok()
        .and_then(|stmts| stmts.into_iter().next())
        .map(|s| match s {
            Statement::Query(q) => q.limit.is_some() || q.fetch.is_some(),
            _ => true,
        })
        .unwrap_or_else(|| sql.to_uppercase().contains("LIMIT"));
    if has_limit {
        return sql.to_string();
    }
    format!("{} LIMIT {}", sql.trim().trim_end_matches(';'), max_rows)
}
