//! # dms-knowledge —— 企业知识库能力包（RAG）
//!
//! 文件入库（解析/分块/向量，状态可查）→ ACL 先行的三路混合检索 → 带引用的文本回答；
//! 外加上传表格物化成自有 PG 物理表的描述符（双通道的 knowledge 那一半）。
//!
//! ## 纪律
//! - **结构上不产 SQL**：不依赖 sqlparser、不 import `RawSql`/`ScopedSql`。上传文档是不可信输入，
//!   这条依赖边就是「文档内容永不成为可执行指令」的结构性保证。
//! - **不依赖 dms-policy**：管的对象不同——policy 管生产库业务表的行条件注入，
//!   knowledge 管自有 PG 里 `kb.acl` 的文档可见性（SQL 内 JOIN，不做查完再过滤）。
//!   两者唯一交集是 login + 角色码两个字符串，用 `Viewer` 传。
//! - **不碰 `meta.*` 的领域表**（那是 semantic 的地盘）。唯一例外：`qa_log` 向
//!   `meta.query_log` 落观测行 —— 那是 server 的只写不读日志表，不是注册表域；
//!   不落它，KB 问答绑不上反馈也进不了统计（Y2 反馈闭环）。例外只许那一条 INSERT。
//!
//! 预算：≤8 个文件（`qa_log` 是 Y2 落账新增的第 8 个）。落点清单见 `docs/ARCHITECTURE.md` §4.5。
//! 本阶段（K1）落 store/acl/ingest；`retrieve`/`answer`/`tabular` 属 K2/K4。
//!
//! - **全部 PG 访问经 `&OwnedStore`**（T4-C 收口）：SQL 只能是 `&'static str` 字面量，
//!   值全走 `bind`。列清单/ACL 片段这类要拼的东西改成 `macro_rules!` + `concat!`，
//!   **在编译期**拼完 —— 于是「把问句或文档内容拼进 SQL」在类型上不再可能。
//!   本 crate 因此不再出现一行 `sqlx::query*`（架构门禁那条从 warn 转 ok）。

pub mod acl;
pub mod answer;
pub mod ingest;
pub mod kg;
pub mod qa_log;
pub mod retrieve;
pub mod store;
pub mod tabular;

/// 检索/回答的调用者身份：只要 login + 角色码两个字符串。
/// **刻意不用 `dms_policy::Principal`** —— 那会让本 crate 依赖权限内核（见 crate 文档纪律）。
#[derive(Debug, Clone)]
pub struct Viewer {
    pub login: String,
    pub roles: Vec<String>,
}

impl Viewer {
    pub fn new(login: impl Into<String>, roles: Vec<String>) -> Self {
        Self { login: login.into(), roles }
    }
}

/// 知识库错误：`BadInput` 映 400、`Forbidden` 映 403、`NotFound` 映 404，其余 500。
#[derive(Debug)]
pub enum KbError {
    BadInput(String),
    Forbidden(String),
    NotFound(String),
    Upstream(String),
    Db(String),
}

impl std::fmt::Display for KbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KbError::BadInput(m) => write!(f, "入参不合法：{m}"),
            KbError::Forbidden(m) => write!(f, "无权访问：{m}"),
            KbError::NotFound(m) => write!(f, "不存在：{m}"),
            KbError::Upstream(m) => write!(f, "文档服务不可用：{m}"),
            KbError::Db(m) => write!(f, "元数据库错误：{m}"),
        }
    }
}

impl std::error::Error for KbError {}

impl From<sqlx::Error> for KbError {
    fn from(e: sqlx::Error) -> Self {
        KbError::Db(e.to_string())
    }
}

/// `fixed()` 通道的错误。五个变体全归 `Db`（映 500）：`Query`/`Decode` 在这里都意味着
/// **我们自己写的字面量 SQL 与 kb schema 不符**，不是调用方的入参问题，不能映成 400。
impl From<dms_connector::error::ConnectorError> for KbError {
    fn from(e: dms_connector::error::ConnectorError) -> Self {
        KbError::Db(e.to_string())
    }
}

impl From<dms_connector::doc::DocError> for KbError {
    fn from(e: dms_connector::doc::DocError) -> Self {
        use dms_connector::doc::DocError;
        match e {
            // 确定性失败：重试无意义，直接落 kb.doc.error 让用户看见
            DocError::NoTextLayer => KbError::BadInput("该 PDF 没有文本层（扫描版），需先 OCR".into()),
            DocError::Unsupported(m) => KbError::BadInput(format!("不支持的文件类型：{m}")),
            DocError::TooLarge(m) => KbError::BadInput(format!("表格超出上限：{m}")),
            DocError::NotFound(m) => KbError::NotFound(m),
            // 只有「服务不可达/冷却/非预期状态码」才是我们的故障
            other => KbError::Upstream(other.to_string()),
        }
    }
}
