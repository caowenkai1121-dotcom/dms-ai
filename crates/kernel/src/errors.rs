//! 错误契约。**Display 文案是外部契约，不是给人看的日志**：
//! ① repair 轮把 `e.to_string()` 原样喂给 LLM（`pipeline.rs:687-701` 的 err 分支），
//! ② 注入侧与 guard 侧的断言直接断言其子串。改一个字就是改行为，故本文件配漂移单测。
//!
//! 不引 thiserror（D6 零新增依赖）：Display 与 `std::error::Error` 手写。
//! 文案来源（迁移前的 anyhow 消息，逐字）：
//! - `pipeline.rs:64/67/71/80/83/86/130/135` 八条 —— `GuardError`
//! - `inject.rs:191/246/318/327-329`、`scope.rs:78` —— `PolicyError`
//!
//! `Parse` 两个变体存 `ParserError` 的 `to_string()`：迁移前是 `Parser::parse_sql(..)?`
//! 由 anyhow 包 ParserError，`to_string()` 即 `sql parser error: ...`，透传即文案不变。

use std::error::Error;
use std::fmt;

/// 只读红线与 LIMIT 护栏的判定失败（`sql::guard` / `sql::ast` 产出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardError {
    /// sqlparser 解析失败，原样透传其 Display
    Parse(String),
    MultiStatement,
    NotSelect,
    ExecutableComment,
    /// 写操作词（剥字面量后按词边界扫到）
    WriteToken(String),
    /// 系统库/元数据库引用（`库名.` 限定形态）
    SystemSchema(String),
    SensitiveColumn(String),
    UnfilledPlaceholder(String),
    PlaceholderHallucination,
    /// **常量投影**：SQL 没有引用任何业务表，投影全是常量（`SELECT 1 AS 探针结果`）。
    ///
    /// 🔴 实证（业主报的准确度问题）：用户在聊天框只发一个客户名「嗨肉」，
    /// LLM 输出 `SELECT 1 AS \`探针结果\`` —— 那是它**自言自语的试探**，不是答案，
    /// 而我们照样执行并把「探针结果 = 1」给了用户。
    /// 与 `PlaceholderHallucination` 同族：模型在没有意图时会编一个恒能执行的空壳，
    /// 而空壳的结果**看起来像个正常答案**（有列名、有值、零报错）。
    ConstantProjection,
}

impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(m) => write!(f, "{m}"),
            Self::MultiStatement => write!(f, "只允许单条语句"),
            Self::NotSelect => write!(f, "只允许 SELECT"),
            Self::ExecutableComment => write!(f, "只读红线：禁止可执行注释"),
            Self::WriteToken(tok) => write!(f, "只读红线：禁止写操作 [{tok}]"),
            Self::SystemSchema(schema) => write!(f, "只读红线：禁止访问系统库 [{schema}]"),
            Self::SensitiveColumn(col) => write!(f, "SQL 含敏感列 [{col}]"),
            Self::UnfilledPlaceholder(frag) => write!(f, "SQL 含未填充占位符: {frag}"),
            Self::PlaceholderHallucination => write!(f, "SQL 含占位符幻觉"),
            Self::ConstantProjection => write!(
                f,
                "SQL 没有查任何业务表、投影全是常量（如 SELECT 1）——那是模型的试探不是答案"
            ),
        }
    }
}

impl Error for GuardError {}

/// 行级权限注入与集合裁决的失败。一切失败 fail-closed（I3）：拿到 `Err` 必须拒绝查询，
/// 绝不允许「注入失败就不注入」——那是越权出数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    /// sqlparser 解析失败，原样透传其 Display
    Parse(String),
    NotSelect,
    /// via 表的头表没有 scoped 档案（`{table}` 被查，`{via}` 是头表）
    ViaHeadUnregistered { table: String, via: String },
    UnregisteredTable(String),
    /// 权限条件字符串无法完整解析（F1：含前缀解析成功但未吃到 EOF 的截断式条件）
    ConditionParse(String),
    /// 角色数据权限的 view_type 取值不在已知枚举内
    BadViewType(i32),
    /// 表达式里的子查询数与递归器实际走到的数**不相等** —— 说明 `walk_expr_subqueries`
    /// 的 match 漏了某个 `Expr` 变体，于是那个子查询**一条权限条件都没注入**。
    ///
    /// 🔴 为什么做成错误而不是「尽力而为」：漏注入的后果是**静默越权读**
    /// （SQL 合法、形状正常、零报错，用户拿到全公司数据）。手写 match 不可能保证对
    /// sqlparser 的全部 `Expr` 变体完备，所以不指望它完备 —— 而是**数出来对拍**，
    /// 不相等就拒。误伤方向是「多拒一条查询」，那是可接受的一侧。
    SubqueryNotCovered { counted: usize, walked: usize },
    /// 集合全空（无限制档）却走了 `inject()`。放行必须显式走
    /// `ScopedSql::unrestricted(sql, &UnrestrictedProof)`（F2）——
    /// 否则 `ScopeSets::default()` 就成了万能放行钥匙。
    NeedsProof,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(m) => write!(f, "{m}"),
            Self::NotSelect => write!(f, "只允许 SELECT 语句"),
            Self::ViaHeadUnregistered { table, via } => {
                write!(f, "表 {table} 的 via 头表 {via} 未登记 scoped 档案，fail-closed 拒绝")
            }
            Self::UnregisteredTable(table) => write!(
                f,
                "表 {table} 未在权限档案登记（meta.scope_binding），已按 fail-closed 拒绝；请核实 Java @DataScope 口径后登记 scoped/global/via"
            ),
            Self::SubqueryNotCovered { counted, walked } => write!(
                f,
                "SQL 的表达式里有 {counted} 个子查询，而权限注入只走到 {walked} 个 ——                  递归器漏了某个语法形态，已按 fail-closed 拒绝（漏注入等于越权读）。                 请把这条 SQL 报给维护者：`walk_expr_subqueries` 需要补一个 Expr 变体"
            ),
            Self::ConditionParse(cond) => {
                write!(f, "权限条件无法完整解析，已按 fail-closed 拒绝：{cond}")
            }
            Self::BadViewType(v) => write!(f, "角色数据权限配置错误 view_type={v}"),
            Self::NeedsProof => write!(
                f,
                "无限制档不得经 inject() 放行，必须显式铸造放行凭证（ScopedSql::unrestricted）"
            ),
        }
    }
}

impl Error for PolicyError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 文案漂移守卫：这些字面量是给 LLM 的输入与测试的断言对象，逐字等于迁移前的 anyhow 消息。
    #[test]
    fn guard_error_wording_frozen() {
        assert_eq!(GuardError::Parse("sql parser error: x".into()).to_string(), "sql parser error: x");
        assert_eq!(GuardError::MultiStatement.to_string(), "只允许单条语句");
        assert_eq!(GuardError::NotSelect.to_string(), "只允许 SELECT");
        assert_eq!(GuardError::ExecutableComment.to_string(), "只读红线：禁止可执行注释");
        assert_eq!(GuardError::WriteToken("delete".into()).to_string(), "只读红线：禁止写操作 [delete]");
        assert_eq!(
            GuardError::SystemSchema("mysql.".into()).to_string(),
            "只读红线：禁止访问系统库 [mysql.]"
        );
        assert_eq!(GuardError::SensitiveColumn("login_pwd".into()).to_string(), "SQL 含敏感列 [login_pwd]");
        assert_eq!(
            GuardError::UnfilledPlaceholder("__ORDER_CODE__".into()).to_string(),
            "SQL 含未填充占位符: __ORDER_CODE__"
        );
        assert_eq!(GuardError::PlaceholderHallucination.to_string(), "SQL 含占位符幻觉");
    }

    #[test]
    fn policy_error_wording_frozen() {
        assert_eq!(PolicyError::Parse("sql parser error: x".into()).to_string(), "sql parser error: x");
        assert_eq!(PolicyError::NotSelect.to_string(), "只允许 SELECT 语句");
        assert_eq!(
            PolicyError::ViaHeadUnregistered { table: "d".into(), via: "h".into() }.to_string(),
            "表 d 的 via 头表 h 未登记 scoped 档案，fail-closed 拒绝"
        );
        assert_eq!(
            PolicyError::UnregisteredTable("x".into()).to_string(),
            "表 x 未在权限档案登记（meta.scope_binding），已按 fail-closed 拒绝；请核实 Java @DataScope 口径后登记 scoped/global/via"
        );
        assert_eq!(
            PolicyError::ConditionParse("x.owner manager in (1)".into()).to_string(),
            "权限条件无法完整解析，已按 fail-closed 拒绝：x.owner manager in (1)"
        );
        assert_eq!(PolicyError::BadViewType(7).to_string(), "角色数据权限配置错误 view_type=7");
    }
}
