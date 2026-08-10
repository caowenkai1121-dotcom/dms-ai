//! kb 表结构与状态机的唯一读写落点。变更原因＝表结构。
//! ACL 查询真相仍在 `acl.rs`；长耗时上传发布、目录变更与文档关联在本层 SQL 内再次复核
//! 当前操作者的 login + 全部角色，避免只依赖 API 先验检查，也不能拿上传者历史权限冒充当前授权。
//! **不含编排**（那是 `ingest.rs`）。
//! DDL 真相源是 `crates/semantic/migrations/0020_kb_init.sql`，此处只 `include_str!` 不复述。

use crate::KbError;
use dms_connector::doc::Chunk;
use dms_connector::owned::OwnedStore;
use sqlx::Executor;

pub const KB_EMBEDDING_RECIPE: i16 = 1;

/// recipe v1。`tools/embed_service.py::kb_embedding_text` 必须逐字一致。
pub fn chunk_embedding_text(name: &str, folder_path: &str, heading_path: &str, body: &str) -> String {
    format!(
        "文件：{}\n目录：{}\n章节：{}\n\n{}",
        name,
        if folder_path.is_empty() { "/" } else { folder_path },
        if heading_path.is_empty() { "正文" } else { heading_path },
        body
    )
}

#[derive(Debug, Clone)]
pub struct ChunkEmbeddingJob {
    pub chunk_id: i64,
    pub text: String,
    pub recipe: i16,
}

/// chunk 正文在「分块输入流」（逐 block trim 后以 `\n` 连接，见 `ingest`）中的字符区间，
/// 半开 `[start, end)`，按 Unicode 字符计（与 PG 侧 `int` 对齐）。
/// 落库值是 `Option`：`None` ＝ 未能可靠定位（重叠/归一化差异），
/// 回查侧须回退到 ord 邻窗——错位的偏移比没有更糟。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharSpan {
    pub start: i32,
    pub end: i32,
}

/// kb schema 的唯一 DDL 真相源（放在 semantic/migrations 是「单 migrator」的要求，见该文件头注）
const KB_DDL: &str = include_str!("../../semantic/migrations/0020_kb_init.sql");

/// 增量迁移（幂等）：chunk 字符偏移回链（B3，`retrieve` 引用回查按偏移定位的存储底座）。
/// 本轮协作范围只许动 knowledge 的 `ingest.rs`/`store.rs`，增量 DDL 因此内嵌于此；
/// 下个能动 `semantic/migrations` 的窗口应把它回写成正式 00XX 迁移文件并删除本 const。
///
/// Y7 同例追加：`kb.doc.description`（AI 摘要/描述，运营小包）。并发轮次不动共享迁移文件，
/// 待下个迁移窗口与上面两条一并回写正式迁移。
const KB_DDL_DELTA: &str = "\
ALTER TABLE kb.chunk ADD COLUMN IF NOT EXISTS start_char_pos int;
ALTER TABLE kb.chunk ADD COLUMN IF NOT EXISTS end_char_pos int;
ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS description text NOT NULL DEFAULT '';
";

/// `DocRow` 的列清单。`created_at` 取 `::text`——为一个纯展示字段给 knowledge 引 chrono 不值当。
///
/// **是宏不是 `const`**：`OwnedStore::fixed()` 只吃 `&'static str`，列清单必须在**编译期**
/// 拼进 SQL（原来的 `format!("SELECT {DOC_COLS} …")` 产出 `String`，那条路已被类型堵死）。
/// 单一真相源仍是这一处，`DOC_COLS` 与三条 `SELECT` 都从它展开。
macro_rules! doc_cols {
    () => {
        "doc_id,space_id,folder_id,folder_path,name,mime,bytes,sha256,status,enabled,tags,business_domain,\
         effective_from,effective_to,source_uri,document_family,document_revision,error,notice,description,\
         page_count,chunk_count,uploaded_by,created_at::text AS created_at,\
         updated_at::text AS updated_at"
    };
}
pub(crate) use doc_cols;

pub const DOC_COLS: &str = doc_cols!();

/// 建 kb schema（幂等，可重复执行）。旧库规范化、约束重建与向量失效必须原子提交；
/// 任一旧数据不满足新约束时整份回滚，不能留下半升级 schema。
///
/// 按 `;` 切分后在同一 PG 事务逐条执行：切分器跳过 `DO $$`/函数体内的分号，
/// 其余静态 DDL 不含内部语句分隔符（`ddl_splits_without_breaking_statements` 钉住该前提）。
///
/// `pool()` 只在这里用于事务边界；执行文本仍全部来自 `include_str!` 的 `&'static str`，
/// 没有接收请求内容或运行时 SQL 的入口。
///
/// **装配顺序**：依赖 `vector` 与 `pg_trgm` 两个扩展，由 `meta::migrate` 先建 —— 必须在它之后跑。
pub async fn migrate(store: &OwnedStore) -> Result<(), KbError> {
    let mut tx = store.pool().begin().await?;
    for stmt in statements(KB_DDL).chain(statements(KB_DDL_DELTA)) {
        (&mut *tx).execute(stmt).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 入参**必须**是 `&'static str`（唯一调用点是 `include_str!` 的 `KB_DDL`）：
/// 切片继承这个生命周期，才进得去 `fixed()`。
fn statements(ddl: &'static str) -> impl Iterator<Item = &'static str> {
    let bytes = ddl.as_bytes();
    let mut out = Vec::new();
    let (mut start, mut i, mut dollar) = (0usize, 0usize, false);
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'$' && bytes[i + 1] == b'$' {
            dollar = !dollar;
            i += 2;
            continue;
        }
        if bytes[i] == b';' && !dollar {
            let stmt = ddl[start..i].trim();
            if !stmt.is_empty() && !is_comment_only(stmt) {
                out.push(stmt);
            }
            start = i + 1;
        }
        i += 1;
    }
    let stmt = ddl[start..].trim();
    if !stmt.is_empty() && !is_comment_only(stmt) {
        out.push(stmt);
    }
    out.into_iter()
}

/// 纯注释片段（末条语句后面若跟了注释，切出来的就是它）——执行会报空查询，先滤掉
fn is_comment_only(stmt: &str) -> bool {
    stmt.lines().all(|l| {
        let t = l.trim();
        t.is_empty() || t.starts_with("--")
    })
}

/// 入库状态机：`pending → parsing → chunked → embedded`，任一步失败 → `failed`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocStatus {
    Pending,
    Parsing,
    Chunked,
    Embedded,
    Failed,
}

impl DocStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DocStatus::Pending => "pending",
            DocStatus::Parsing => "parsing",
            DocStatus::Chunked => "chunked",
            DocStatus::Embedded => "embedded",
            DocStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(DocStatus::Pending),
            "parsing" => Some(DocStatus::Parsing),
            "chunked" => Some(DocStatus::Chunked),
            "embedded" => Some(DocStatus::Embedded),
            "failed" => Some(DocStatus::Failed),
            _ => None,
        }
    }
}

/// `kb.doc` 一行（`status` 留 `String`：DB 里出现未知值时列表页仍要能展示，不能整表读不出来）
#[derive(Debug, Clone)]
pub struct DocRow {
    pub doc_id: String,
    pub space_id: String,
    pub folder_id: Option<String>,
    pub folder_path: String,
    pub name: String,
    pub mime: String,
    pub bytes: i64,
    pub sha256: String,
    pub status: String,
    pub enabled: bool,
    pub tags: Vec<String>,
    pub business_domain: Option<String>,
    pub effective_from: Option<sqlx::types::chrono::NaiveDate>,
    pub effective_to: Option<sqlx::types::chrono::NaiveDate>,
    pub source_uri: Option<String>,
    pub document_family: Option<String>,
    pub document_revision: Option<String>,
    pub error: String,
    pub notice: String,
    /// AI 生成的摘要/描述（Y7）；空串＝未生成。写回走 `set_doc_description`（写复核内联）。
    pub description: String,
    pub page_count: i32,
    pub chunk_count: i32,
    pub uploaded_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct SpaceRow {
    pub space_id: String,
    pub name: String,
    pub owner: String,
    pub visibility: String,
    pub writable: bool,
    pub doc_count: i64,
}

#[derive(Debug, Clone)]
pub struct FolderRow {
    pub folder_id: String,
    pub space_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub path: String,
    pub depth: i32,
    pub child_count: i64,
    pub doc_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for FolderRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            folder_id: row.try_get("folder_id")?,
            space_id: row.try_get("space_id")?,
            parent_id: row.try_get("parent_id")?,
            name: row.try_get("name")?,
            path: row.try_get("path")?,
            depth: row.try_get("depth")?,
            child_count: row.try_get("child_count")?,
            doc_count: row.try_get("doc_count")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct DocRelationRow {
    pub doc_id: String,
    pub doc_name: String,
    pub folder_id: Option<String>,
    pub folder_path: String,
    pub document_family: Option<String>,
    pub document_revision: Option<String>,
    pub relation: String,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for DocRelationRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            doc_id: row.try_get("doc_id")?,
            doc_name: row.try_get("doc_name")?,
            folder_id: row.try_get("folder_id")?,
            folder_path: row.try_get("folder_path")?,
            document_family: row.try_get("document_family")?,
            document_revision: row.try_get("document_revision")?,
            relation: row.try_get("relation")?,
        })
    }
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for SpaceRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            space_id: row.try_get("space_id")?,
            name: row.try_get("name")?,
            owner: row.try_get("owner")?,
            visibility: row.try_get("visibility")?,
            writable: row.try_get("writable")?,
            doc_count: row.try_get("doc_count")?,
        })
    }
}

/// 手写 `FromRow`：workspace 的 sqlx 没开 `derive` feature（不改 Cargo.toml 是硬规则），
/// 列名与 `DOC_COLS` 一一对应。若以后开启 feature，可换成 `#[derive(sqlx::FromRow)]`。
impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for DocRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            doc_id: row.try_get("doc_id")?,
            space_id: row.try_get("space_id")?,
            folder_id: row.try_get("folder_id")?,
            folder_path: row.try_get("folder_path")?,
            name: row.try_get("name")?,
            mime: row.try_get("mime")?,
            bytes: row.try_get("bytes")?,
            sha256: row.try_get("sha256")?,
            status: row.try_get("status")?,
            enabled: row.try_get("enabled")?,
            tags: row.try_get("tags")?,
            business_domain: row.try_get("business_domain")?,
            effective_from: row.try_get("effective_from")?,
            effective_to: row.try_get("effective_to")?,
            source_uri: row.try_get("source_uri")?,
            document_family: row.try_get("document_family")?,
            document_revision: row.try_get("document_revision")?,
            error: row.try_get("error")?,
            notice: row.try_get("notice")?,
            description: row.try_get("description")?,
            page_count: row.try_get("page_count")?,
            chunk_count: row.try_get("chunk_count")?,
            uploaded_by: row.try_get("uploaded_by")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

/// 新文档入参（命名结构体而非 6 个 `&str` 连排，D4）
pub struct NewDoc<'a> {
    pub space_id: &'a str,
    pub folder_id: Option<&'a str>,
    pub name: &'a str,
    pub mime: &'a str,
    pub bytes: i64,
    pub sha256: &'a str,
    pub uploaded_by: &'a str,
    pub writer_roles: &'a [String],
}

/// 治理元数据与显式文档关联的单次原子更新。
pub struct DocMetadataUpdate<'a> {
    pub tags: &'a [String],
    pub business_domain: Option<&'a str>,
    pub effective_from: Option<sqlx::types::chrono::NaiveDate>,
    pub effective_to: Option<sqlx::types::chrono::NaiveDate>,
    pub source_uri: Option<&'a str>,
    pub document_family: Option<&'a str>,
    pub document_revision: Option<&'a str>,
    pub related_doc_ids: &'a [String],
}

/// 幂等建空间。v1 只有个人空间（`space_id = 登录名`），visibility 恒 private。
pub async fn ensure_space(store: &OwnedStore, space_id: &str, owner: &str) -> Result<(), KbError> {
    if space_id != owner {
        return Err(KbError::Forbidden("个人知识空间只能由同名账号初始化".into()));
    }
    store
        .fixed(
            "INSERT INTO kb.space(space_id,name,owner,visibility) VALUES($1,$1,$2,'private') \
             ON CONFLICT (space_id) DO NOTHING",
        )
        .bind(space_id)
        .bind(owner)
        .execute()
        .await?;
    Ok(())
}

pub async fn create_space(
    store: &OwnedStore,
    space_id: &str,
    name: &str,
    owner: &str,
) -> Result<(), KbError> {
    let n = store
        .fixed(
            "INSERT INTO kb.space(space_id,name,owner,visibility) VALUES($1,$2,$3,'private') \
             ON CONFLICT (space_id) DO UPDATE SET name=EXCLUDED.name \
             WHERE kb.space.owner=EXCLUDED.owner",
        )
        .bind(space_id)
        .bind(name)
        .bind(owner)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("知识空间 {space_id} 已由其他账号持有")));
    }
    Ok(())
}

pub async fn space_exists(store: &OwnedStore, space_id: &str) -> Result<bool, KbError> {
    Ok(store
        .fixed("SELECT EXISTS(SELECT 1 FROM kb.space WHERE space_id=$1)")
        .bind(space_id)
        .fetch_optional::<(bool,)>()
        .await?
        .map(|row| row.0)
        .unwrap_or(false))
}

pub async fn list_spaces(
    store: &OwnedStore,
    login: &str,
    roles: &[String],
) -> Result<Vec<SpaceRow>, KbError> {
    Ok(store
        .fixed(
            "SELECT s.space_id, s.name, s.owner, s.visibility, \
                    (s.owner=$1 OR EXISTS (SELECT 1 FROM kb.acl w WHERE w.scope='space' \
                       AND w.target_id=s.space_id AND w.perm='write' \
                       AND ((w.grantee_kind='login' AND w.grantee=$1) \
                         OR (w.grantee_kind='role' AND w.grantee=ANY($2::text[]))))) AS writable, \
                    (SELECT count(*) FROM kb.doc d WHERE d.space_id=s.space_id) AS doc_count \
             FROM kb.space s WHERE s.owner=$1 OR EXISTS (SELECT 1 FROM kb.acl a \
               WHERE a.scope='space' AND a.target_id=s.space_id AND a.perm IN ('read','write') \
                 AND ((a.grantee_kind='login' AND a.grantee=$1) \
                   OR (a.grantee_kind='role' AND a.grantee=ANY($2::text[])))) \
             ORDER BY (s.owner=$1) DESC, s.name, s.space_id",
        )
        .bind(login)
        .bind(roles)
        .fetch_all()
        .await?)
}

pub fn validate_folder_name(name: &str) -> Result<&str, KbError> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return Err(KbError::BadInput("目录名称不能为空或使用 . / ..".into()));
    }
    if name.chars().count() > 100
        || name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\'))
    {
        return Err(KbError::BadInput("目录名称不能超过 100 字，且不能包含路径分隔符或控制字符".into()));
    }
    Ok(name)
}

pub async fn resolve_folder(
    store: &OwnedStore,
    space_id: &str,
    folder_id: Option<&str>,
) -> Result<(Option<String>, String), KbError> {
    let Some(folder_id) = folder_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok((None, "/".into()));
    };
    store
        .fixed("SELECT folder_id,path FROM kb.folder WHERE folder_id=$1 AND space_id=$2")
        .bind(folder_id)
        .bind(space_id)
        .fetch_optional::<(String, String)>()
        .await?
        .map(|(id, path)| (Some(id), path))
        .ok_or_else(|| KbError::NotFound(format!("空间 {space_id} 中的目录 {folder_id}")))
}

pub async fn list_folders(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    space_id: &str,
) -> Result<Vec<FolderRow>, KbError> {
    Ok(store
        .fixed(
            "SELECT f.folder_id,f.space_id,f.parent_id,f.name,f.path, \
                    (length(f.path)-length(replace(f.path,'/','')))::int AS depth, \
                    (SELECT count(*) FROM kb.folder c WHERE c.parent_id=f.folder_id) AS child_count, \
                    (SELECT count(*) FROM kb.doc d WHERE d.folder_id=f.folder_id) AS doc_count, \
                    f.created_at::text AS created_at,f.updated_at::text AS updated_at \
             FROM kb.folder f JOIN kb.space s ON s.space_id=f.space_id \
             WHERE f.space_id=$3 AND (s.owner=$1 OR EXISTS (SELECT 1 FROM kb.acl a \
               WHERE a.scope='space' AND a.target_id=s.space_id AND a.perm IN ('read','write') \
                 AND ((a.grantee_kind='login' AND a.grantee=$1) \
                   OR (a.grantee_kind='role' AND a.grantee=ANY($2::text[]))))) \
             ORDER BY f.path,lower(f.name),f.folder_id",
        )
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .bind(space_id)
        .fetch_all()
        .await?)
}

pub async fn get_folder(store: &OwnedStore, folder_id: &str) -> Result<Option<FolderRow>, KbError> {
    Ok(store
        .fixed(
            "SELECT f.folder_id,f.space_id,f.parent_id,f.name,f.path, \
                    (length(f.path)-length(replace(f.path,'/','')))::int AS depth, \
                    (SELECT count(*) FROM kb.folder c WHERE c.parent_id=f.folder_id) AS child_count, \
                    (SELECT count(*) FROM kb.doc d WHERE d.folder_id=f.folder_id) AS doc_count, \
                    f.created_at::text AS created_at,f.updated_at::text AS updated_at \
             FROM kb.folder f WHERE f.folder_id=$1",
        )
        .bind(folder_id)
        .fetch_optional()
        .await?)
}

pub async fn create_folder(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    space_id: &str,
    parent_id: Option<&str>,
    name: &str,
) -> Result<FolderRow, KbError> {
    let name = validate_folder_name(name)?;
    let folder_id = uuid::Uuid::new_v4().to_string();
    let inserted = store
        .fixed(
            "WITH guard AS (SELECT pg_advisory_xact_lock(hashtextextended($2,0))), \
             writable AS (SELECT s.space_id FROM kb.space s CROSS JOIN guard g \
               WHERE s.space_id=$2 AND (s.owner=$5 OR EXISTS (SELECT 1 FROM kb.acl a \
                 WHERE a.scope='space' AND a.target_id=s.space_id AND a.perm='write' AND \
                 ((a.grantee_kind='login' AND a.grantee=$5) OR \
                  (a.grantee_kind='role' AND a.grantee=ANY($6::text[])))))), \
             parent AS (SELECT f.folder_id,f.path FROM kb.folder f JOIN writable w ON w.space_id=f.space_id \
                        WHERE f.folder_id=$3) \
             INSERT INTO kb.folder(folder_id,space_id,parent_id,name,path,created_by) \
             SELECT $1,w.space_id,$3,$4,CASE WHEN $3::text IS NULL THEN '/'||$4 ELSE p.path||'/'||$4 END,$5 \
             FROM writable w LEFT JOIN parent p ON true WHERE ($3::text IS NULL OR p.folder_id IS NOT NULL) \
               AND length(CASE WHEN $3::text IS NULL THEN '/'||$4 ELSE p.path||'/'||$4 END) <= 1000 \
             ON CONFLICT DO NOTHING RETURNING folder_id",
        )
        .bind(&folder_id)
        .bind(space_id)
        .bind(parent_id)
        .bind(name)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .fetch_optional::<(String,)>()
        .await?;
    if inserted.is_none() {
        if !crate::acl::space_writable(store, viewer, space_id).await? {
            return Err(KbError::Forbidden(format!("知识空间 {space_id} 的写权限已失效")));
        }
        return Err(KbError::BadInput("父目录无效、同级目录重名或目录路径过长".into()));
    }
    get_folder(store, &folder_id)
        .await?
        .ok_or_else(|| KbError::Db("目录创建后无法读取".into()))
}

pub async fn move_folder(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    folder_id: &str,
    expected_space_id: Option<&str>,
    parent_id: Option<&str>,
    name: &str,
) -> Result<FolderRow, KbError> {
    let name = validate_folder_name(name)?;
    let moved = store
        .fixed(
             "WITH RECURSIVE target AS ( \
               SELECT folder_id,space_id,path FROM kb.folder WHERE folder_id=$1 \
                 AND ($7::text IS NULL OR space_id=$7) \
             ), guard AS ( \
               SELECT pg_advisory_xact_lock(hashtextextended(space_id,0)) FROM target \
             ), writable AS ( \
               SELECT t.space_id FROM target t JOIN kb.space s ON s.space_id=t.space_id \
               WHERE s.owner=$5 OR EXISTS (SELECT 1 FROM kb.acl a WHERE a.scope='space' \
                 AND a.target_id=s.space_id AND a.perm='write' AND \
                 ((a.grantee_kind='login' AND a.grantee=$5) OR \
                  (a.grantee_kind='role' AND a.grantee=ANY($6::text[])))) \
             ), locked_target AS ( \
               SELECT f.folder_id,f.space_id,f.path FROM kb.folder f CROSS JOIN guard g \
               JOIN writable w ON w.space_id=f.space_id WHERE f.folder_id=$1 FOR UPDATE \
             ), descendants AS ( \
               SELECT folder_id,space_id,path,ARRAY[folder_id]::text[] AS seen FROM locked_target UNION ALL \
               SELECT f.folder_id,d.space_id,f.path,d.seen||f.folder_id FROM kb.folder f \
               JOIN descendants d ON f.parent_id=d.folder_id AND f.space_id=d.space_id \
               WHERE NOT f.folder_id=ANY(d.seen) \
             ), parent AS ( \
               SELECT p.folder_id,p.path FROM kb.folder p JOIN locked_target t ON p.space_id=t.space_id \
               WHERE p.folder_id=$2 \
             ), proposed AS ( \
               SELECT t.folder_id,t.path AS old_path, \
                      CASE WHEN $2::text IS NULL THEN '/'||$3 ELSE p.path||'/'||$3 END AS new_path \
               FROM locked_target t LEFT JOIN parent p ON true \
               WHERE ($2::text IS NULL OR p.folder_id IS NOT NULL) \
                 AND NOT EXISTS (SELECT 1 FROM descendants WHERE folder_id=$2) \
                 AND NOT EXISTS (SELECT 1 FROM kb.folder s WHERE s.space_id=t.space_id \
                   AND s.folder_id<>t.folder_id AND s.parent_id IS NOT DISTINCT FROM $2 \
                   AND lower(s.name)=lower($3)) \
             ), valid AS ( \
               SELECT p.* FROM proposed p WHERE NOT EXISTS ( \
                 SELECT 1 FROM descendants d WHERE char_length( \
                   p.new_path||substring(d.path FROM char_length(p.old_path)+1) \
                 )>1000 \
               ) \
             ), moved_root AS ( \
               UPDATE kb.folder f SET path=v.new_path,parent_id=$2,name=$3,updated_at=now() \
               FROM valid v WHERE f.folder_id=v.folder_id \
               RETURNING f.folder_id,f.path \
             ), moved_descendants AS ( \
               UPDATE kb.folder f SET \
                 path=v.new_path||substring(f.path FROM length(v.old_path)+1),updated_at=now() \
               FROM valid v CROSS JOIN moved_root r \
               WHERE f.folder_id IN (SELECT folder_id FROM descendants WHERE folder_id<>v.folder_id) \
               RETURNING f.folder_id,f.path \
             ), moved AS ( \
               SELECT folder_id,path FROM moved_root \
               UNION ALL SELECT folder_id,path FROM moved_descendants \
             ), doc_targets AS ( \
               SELECT d.doc_id,d.name,m.path AS folder_path FROM kb.doc d \
               JOIN moved m ON d.folder_id=m.folder_id \
             ), chunks AS ( \
               UPDATE kb.chunk c SET folder_path=t.folder_path,embedding=NULL, \
                 embedding_text=kb.chunk_embedding_text(t.name,t.folder_path,c.heading_path,c.text), \
                 embedding_recipe=$4 FROM doc_targets t \
               WHERE c.doc_id=t.doc_id AND (c.folder_path IS DISTINCT FROM t.folder_path \
                 OR c.embedding_recipe<>$4 OR c.embedding_text IS DISTINCT FROM \
                   kb.chunk_embedding_text(t.name,t.folder_path,c.heading_path,c.text)) \
               RETURNING c.doc_id \
             ), docs AS ( \
               UPDATE kb.doc d SET folder_path=t.folder_path, \
                 status=CASE WHEN d.status='embedded' AND EXISTS (SELECT 1 FROM chunks c \
                   WHERE c.doc_id=d.doc_id) THEN 'chunked' ELSE d.status END,updated_at=now() \
               FROM doc_targets t WHERE d.doc_id=t.doc_id AND (d.folder_path IS DISTINCT FROM t.folder_path \
                 OR EXISTS (SELECT 1 FROM chunks c WHERE c.doc_id=d.doc_id)) \
               RETURNING d.doc_id \
             ) SELECT count(*) FROM moved WHERE folder_id=$1",
        )
        .bind(folder_id)
        .bind(parent_id)
        .bind(name)
        .bind(KB_EMBEDDING_RECIPE)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .bind(expected_space_id)
        .fetch_optional::<(i64,)>()
        .await?
        .map_or(0, |(n,)| n);
    if moved != 1 {
        let Some(current) = get_folder(store, folder_id).await? else {
            return Err(KbError::Forbidden("目录不存在或无权修改".into()));
        };
        if !crate::acl::space_writable(store, viewer, &current.space_id).await? {
            return Err(KbError::Forbidden("目录不存在或无权修改".into()));
        }
        return Err(KbError::BadInput(
            "目录不存在、目标父目录无效、移动会形成环、同级重名或路径过长".into(),
        ));
    }
    get_folder(store, folder_id)
        .await?
        .ok_or_else(|| KbError::Db("目录移动后无法读取".into()))
}

pub async fn delete_folder(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    folder_id: &str,
    expected_space_id: Option<&str>,
) -> Result<(), KbError> {
    let deleted = store
        .fixed(
            "WITH RECURSIVE target AS (SELECT f.folder_id,f.space_id FROM kb.folder f \
               JOIN kb.space s ON s.space_id=f.space_id WHERE f.folder_id=$1 \
               AND ($4::text IS NULL OR f.space_id=$4) \
               AND (s.owner=$2 OR EXISTS (SELECT 1 FROM kb.acl a WHERE a.scope='space' \
                 AND a.target_id=s.space_id AND a.perm='write' AND \
                 ((a.grantee_kind='login' AND a.grantee=$2) OR \
                   (a.grantee_kind='role' AND a.grantee=ANY($3::text[])))))), \
             guard AS (SELECT pg_advisory_xact_lock(hashtextextended(space_id,0)) FROM target), \
             descendants AS ( \
               SELECT f.folder_id,f.space_id,ARRAY[f.folder_id]::text[] AS seen \
               FROM kb.folder f JOIN target t ON t.folder_id=f.folder_id AND t.space_id=f.space_id \
               CROSS JOIN guard g UNION ALL \
               SELECT c.folder_id,d.space_id,d.seen||c.folder_id FROM kb.folder c \
               JOIN descendants d ON c.parent_id=d.folder_id AND c.space_id=d.space_id \
               WHERE NOT c.folder_id=ANY(d.seen) \
             ), empty AS ( \
               SELECT t.folder_id FROM target t \
               WHERE NOT EXISTS (SELECT 1 FROM descendants d WHERE d.folder_id<>t.folder_id) \
                 AND NOT EXISTS (SELECT 1 FROM kb.doc doc \
                   WHERE doc.folder_id IN (SELECT folder_id FROM descendants)) \
             ) DELETE FROM kb.folder f USING empty e WHERE f.folder_id=e.folder_id \
             RETURNING folder_id",
        )
        .bind(folder_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .bind(expected_space_id)
        .fetch_optional::<(String,)>()
        .await?;
    if deleted.is_none() {
        let Some(current) = get_folder(store, folder_id).await? else {
            return Err(KbError::Forbidden("目录不存在或无权删除".into()));
        };
        if !crate::acl::space_writable(store, viewer, &current.space_id).await? {
            return Err(KbError::Forbidden("目录不存在或无权删除".into()));
        }
        return Err(KbError::BadInput("目录不存在或目录非空，不能删除".into()));
    }
    Ok(())
}

/// 同空间去重（`kb.doc` 有 `UNIQUE(space_id, sha256)`）
pub async fn find_by_sha(
    store: &OwnedStore,
    space_id: &str,
    sha256: &str,
) -> Result<Option<String>, KbError> {
    Ok(store
        .fixed("SELECT doc_id FROM kb.doc WHERE space_id=$1 AND sha256=$2")
        .bind(space_id)
        .bind(sha256)
        .fetch_optional::<(String,)>()
        .await?
        .map(|(id,)| id))
}

/// `insert_doc` 的结果：新行 id，或同 `(space_id, sha256)` 已被并发上传抢占（秒传去重的原子兜底）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocInsert {
    New(String),
    Duplicate(String),
}

/// 落 `status='pending'` 的新行，返回生成的 `doc_id`（uuid v4，同时是磁盘文件名）。
/// `ON CONFLICT (space_id, sha256) DO NOTHING` 把去重判定压进插入这一条语句：
/// 两个并发上传同内容文件时，后到者拿 `Duplicate`（已有 doc_id）而不是唯一约束 500——
/// content_hash 秒传在并发下也成立，且不会重复建行、重复消耗解析与向量。
pub async fn insert_doc(store: &OwnedStore, d: &NewDoc<'_>) -> Result<DocInsert, KbError> {
    let doc_id = uuid::Uuid::new_v4().to_string();
    let inserted = store
        .fixed(
            "WITH guard AS (SELECT pg_advisory_xact_lock(hashtextextended($2,0))), \
             writable AS (SELECT s.space_id FROM kb.space s CROSS JOIN guard WHERE s.space_id=$2 \
               AND (s.owner=$9 OR EXISTS (SELECT 1 FROM kb.acl a WHERE a.scope='space' \
                 AND a.target_id=s.space_id AND a.perm='write' AND \
                 ((a.grantee_kind='login' AND a.grantee=$9) OR \
                  (a.grantee_kind='role' AND a.grantee=ANY($10::text[])))))), \
             folder AS (SELECT f.folder_id,f.path FROM kb.folder f JOIN writable w ON w.space_id=f.space_id \
                        WHERE f.folder_id=$3) \
             INSERT INTO kb.doc(doc_id,space_id,folder_id,folder_path,name,mime,bytes,sha256,status,uploaded_by) \
             SELECT $1,$2,$3,CASE WHEN $3::text IS NULL THEN '/' ELSE f.path END,$4,$5,$6,$7,$8,$9 \
             FROM writable w LEFT JOIN folder f ON true WHERE $3::text IS NULL OR f.folder_id IS NOT NULL \
             ON CONFLICT (space_id, sha256) DO NOTHING RETURNING doc_id",
        )
        .bind(&doc_id)
        .bind(d.space_id)
        .bind(d.folder_id)
        .bind(d.name)
        .bind(d.mime)
        .bind(d.bytes)
        .bind(d.sha256)
        .bind(DocStatus::Pending.as_str())
        .bind(d.uploaded_by)
        .bind(d.writer_roles)
        .fetch_optional::<(String,)>()
        .await?;
    if let Some((id,)) = inserted {
        return Ok(DocInsert::New(id));
    }
    // 0 行只有两种来历：无写权限（writable 空集）或同 hash 并发抢占。先查重——
    // 查得到就是并发秒传；查不到才是权限被拒（上层 ingest 已先做写权限闸门，
    // 走到这里仍查不到时按无权限报，语义与改造前一致）。
    match find_by_sha(store, d.space_id, d.sha256).await? {
        Some(existing) => Ok(DocInsert::Duplicate(existing)),
        None => Err(KbError::Forbidden(format!("无权向知识空间 {} 上传", d.space_id))),
    }
}

/// DMS 角色批量共享：调用方先用 DMS 角色目录完整校验，本层用单条 PG 语句原子落库。
/// 目标空间在写入前消失时拒绝，不能留下悬空 ACL。
/// 同一角色在同一空间只保留一个权限档；read/write 是替换关系，不是叠加关系。
pub async fn grant_space_roles(
    store: &OwnedStore,
    space_id: &str,
    role_codes: &[String],
    perm: &str,
) -> Result<(), KbError> {
    if role_codes.is_empty() || !matches!(perm, "read" | "write") {
        return Err(KbError::BadInput("角色授权参数无效".into()));
    }
    let (exists, _inserted) = store
        .fixed(
            "WITH target AS (SELECT space_id FROM kb.space WHERE space_id=$1), \
             roles AS (SELECT DISTINCT btrim(code) AS code FROM unnest($2::text[]) AS u(code)), \
             removed AS (DELETE FROM kb.acl a USING target t,roles r \
               WHERE a.scope='space' AND a.target_id=t.space_id AND a.grantee_kind='role' \
                 AND a.grantee=r.code AND a.perm<>$3 RETURNING 1), \
             inserted AS (INSERT INTO kb.acl(scope,target_id,grantee_kind,grantee,perm) \
               SELECT 'space',t.space_id,'role',r.code,$3 FROM target t CROSS JOIN roles r \
               ON CONFLICT DO NOTHING RETURNING 1) \
             SELECT EXISTS(SELECT 1 FROM target), \
                    (SELECT count(*) FROM inserted)+(SELECT count(*) FROM removed)",
        )
        .bind(space_id)
        .bind(role_codes)
        .bind(perm)
        .fetch_optional::<(bool, i64)>()
        .await?
        .unwrap_or((false, 0));
    if !exists {
        return Err(KbError::NotFound(format!("知识空间 {space_id}")));
    }
    Ok(())
}

pub async fn grant_space_acl(
    store: &OwnedStore,
    space_id: &str,
    grantee_kind: &str,
    grantee: &str,
    perm: &str,
) -> Result<(), KbError> {
    if !matches!(grantee_kind, "login" | "role") || !matches!(perm, "read" | "write") {
        return Err(KbError::BadInput("空间授权参数无效".into()));
    }
    let (exists, _inserted) = store
        .fixed(
            "WITH target AS (SELECT space_id FROM kb.space WHERE space_id=$1), \
             removed AS (DELETE FROM kb.acl a USING target t WHERE a.scope='space' \
               AND a.target_id=t.space_id AND a.grantee_kind=$2 AND a.grantee=$3 AND a.perm<>$4 \
               RETURNING 1), \
             inserted AS (INSERT INTO kb.acl(scope,target_id,grantee_kind,grantee,perm) \
               SELECT 'space',space_id,$2,$3,$4 FROM target ON CONFLICT DO NOTHING RETURNING 1) \
             SELECT EXISTS(SELECT 1 FROM target), \
                    (SELECT count(*) FROM inserted)+(SELECT count(*) FROM removed)",
        )
        .bind(space_id)
        .bind(grantee_kind)
        .bind(grantee)
        .bind(perm)
        .fetch_optional::<(bool, i64)>()
        .await?
        .unwrap_or((false, 0));
    if !exists {
        return Err(KbError::NotFound(format!("知识空间 {space_id}")));
    }
    Ok(())
}

pub async fn revoke_space_acl(
    store: &OwnedStore,
    space_id: &str,
    grantee_kind: &str,
    grantee: &str,
    perm: &str,
) -> Result<(), KbError> {
    if !matches!(grantee_kind, "login" | "role") || !matches!(perm, "read" | "write") {
        return Err(KbError::BadInput("空间撤权参数无效".into()));
    }
    let (exists, _deleted) = store
        .fixed(
            "WITH target AS (SELECT space_id FROM kb.space WHERE space_id=$1), \
             deleted AS (DELETE FROM kb.acl a USING target t WHERE a.scope='space' \
               AND a.target_id=t.space_id AND a.grantee_kind=$2 AND a.grantee=$3 AND a.perm=$4 \
               RETURNING 1) \
             SELECT EXISTS(SELECT 1 FROM target),(SELECT count(*) FROM deleted)",
        )
        .bind(space_id)
        .bind(grantee_kind)
        .bind(grantee)
        .bind(perm)
        .fetch_optional::<(bool, i64)>()
        .await?
        .unwrap_or((false, 0));
    if !exists {
        return Err(KbError::NotFound(format!("知识空间 {space_id}")));
    }
    Ok(())
}

pub async fn set_status(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    st: DocStatus,
    error: &str,
) -> Result<(), KbError> {
    let n = store
        .fixed(
            "UPDATE kb.doc d SET status=$1,error=$2,updated_at=now() FROM kb.space s \
             WHERE d.doc_id=$3 AND s.space_id=d.space_id AND (s.owner=$4 OR EXISTS ( \
               SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=s.space_id \
                 AND a.perm='write' AND ((a.grantee_kind='login' AND a.grantee=$4) OR \
                   (a.grantee_kind='role' AND a.grantee=ANY($5::text[])))))",
        )
        .bind(st.as_str())
        .bind(error)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 的写权限已失效")));
    }
    Ok(())
}

/// 解析成功但存在 OCR/跳页/表格降级时的可见提示；不滥用 `error` 假装整份失败。
pub async fn set_notice(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    notice: &str,
) -> Result<(), KbError> {
    let n = store
        .fixed(
            "UPDATE kb.doc d SET notice=$1,updated_at=now() FROM kb.space s \
             WHERE d.doc_id=$2 AND s.space_id=d.space_id AND (s.owner=$3 OR EXISTS ( \
               SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=s.space_id \
                 AND a.perm='write' AND ((a.grantee_kind='login' AND a.grantee=$3) OR \
                   (a.grantee_kind='role' AND a.grantee=ANY($4::text[])))))",
        )
        .bind(notice)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 的写权限已失效")));
    }
    Ok(())
}

pub async fn set_enabled(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    enabled: bool,
) -> Result<(), KbError> {
    let n = store
        .fixed(
            "UPDATE kb.doc d SET enabled=$1,updated_at=now() FROM kb.space s \
             WHERE d.doc_id=$2 AND s.space_id=d.space_id AND (s.owner=$3 OR EXISTS ( \
               SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=s.space_id \
                 AND a.perm='write' AND ((a.grantee_kind='login' AND a.grantee=$3) OR \
                   (a.grantee_kind='role' AND a.grantee=ANY($4::text[])))))",
        )
        .bind(enabled)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 的写权限已失效")));
    }
    Ok(())
}

/// 回写文档来源 URL（Y12 URL 抓取入库）。写权限在同一条 UPDATE 里复核（fail-closed）：
/// 撤权若发生在 handler 判定与本语句之间，0 行 → Forbidden，不写别人的文档。
/// 形状与 `set_enabled` 一致；`description`/`source_uri` 分设两函数而不是字段参数——
/// `fixed()` 只吃 `&'static str`，两处都是静态字面量。
pub async fn set_doc_source_uri(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    source_uri: &str,
) -> Result<(), KbError> {
    let n = store
        .fixed(
            "UPDATE kb.doc d SET source_uri=$1,updated_at=now() FROM kb.space s \
             WHERE d.doc_id=$2 AND s.space_id=d.space_id AND (s.owner=$3 OR EXISTS ( \
               SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=s.space_id \
                 AND a.perm='write' AND ((a.grantee_kind='login' AND a.grantee=$3) OR \
                   (a.grantee_kind='role' AND a.grantee=ANY($4::text[])))))",
        )
        .bind(source_uri)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 的写权限已失效")));
    }
    Ok(())
}

/// 回写 AI 生成的文档描述（Y7）。写复核内联（同 `set_enabled` 形状，fail-closed）。
pub async fn set_doc_description(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    description: &str,
) -> Result<(), KbError> {
    let n = store
        .fixed(
            "UPDATE kb.doc d SET description=$1,updated_at=now() FROM kb.space s \
             WHERE d.doc_id=$2 AND s.space_id=d.space_id AND (s.owner=$3 OR EXISTS ( \
               SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=s.space_id \
                 AND a.perm='write' AND ((a.grantee_kind='login' AND a.grantee=$3) OR \
                   (a.grantee_kind='role' AND a.grantee=ANY($4::text[])))))",
        )
        .bind(description)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 的写权限已失效")));
    }
    Ok(())
}

/// 原子更新文档治理元数据与显式关联。
///
/// 源文档必须对当前操作者可见且所在空间当前可写；所有关联目标必须同时满足：可见、
/// 同空间、已启用、已完成切片/向量入库且处于生效期。任一目标失败时元数据和关联均不改变。
/// 实现是一条 `fixed(&'static str)` 数据修改 CTE：校验、metadata UPDATE、reference links 的
/// upsert/删除共享同一快照并原子提交；调用方不应再自行开启事务或复制 SQL。
pub async fn update_doc_metadata_and_links(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    update: &DocMetadataUpdate<'_>,
) -> Result<u64, KbError> {
    if update
        .effective_from
        .zip(update.effective_to)
        .is_some_and(|(from, to)| from > to)
    {
        return Err(KbError::BadInput("生效日期不能晚于失效日期".into()));
    }
    let mut ids = Vec::new();
    for raw in update.related_doc_ids {
        let id = raw.trim();
        if id.is_empty() || ids.iter().any(|seen| seen == id) {
            continue;
        }
        if id == doc_id {
            return Err(KbError::BadInput("文档不能关联自身".into()));
        }
        ids.push(id.to_string());
    }
    if ids.len() > 50 {
        return Err(KbError::BadInput("关联文档最多 50 篇".into()));
    }

    let state = store
        .fixed(concat!(
            "WITH candidate_source AS ( \
               SELECT d.doc_id,d.space_id FROM kb.doc d WHERE d.doc_id=$3 \
                 AND d.doc_id IN (",
            crate::acl::visible_docs!(),
            ") AND EXISTS (SELECT 1 FROM kb.space s WHERE s.space_id=d.space_id AND \
                 (s.owner=$1 OR EXISTS (SELECT 1 FROM kb.acl w WHERE w.scope='space' \
                   AND w.target_id=s.space_id AND w.perm='write' AND \
                   ((w.grantee_kind='login' AND w.grantee=$1) OR \
                    (w.grantee_kind='role' AND w.grantee=ANY($2::text[])))))) \
             ), lock_guard AS ( \
               SELECT pg_advisory_xact_lock(hashtextextended(space_id,0)) FROM candidate_source \
             ), source AS ( \
               SELECT d.doc_id,d.space_id FROM kb.doc d \
               JOIN candidate_source c ON c.doc_id=d.doc_id AND c.space_id=d.space_id \
               CROSS JOIN lock_guard FOR UPDATE OF d \
             ), requested AS ( \
               SELECT DISTINCT btrim(r.doc_id) AS doc_id FROM source \
               CROSS JOIN unnest($11::text[]) AS r(doc_id) WHERE btrim(r.doc_id)<>'' \
             ), valid AS ( \
               SELECT d.doc_id FROM requested r JOIN kb.doc d ON d.doc_id=r.doc_id \
               JOIN source s ON s.space_id=d.space_id WHERE d.doc_id<>s.doc_id \
                 AND d.enabled=true AND d.status IN ('chunked','embedded') \
                 AND EXISTS (SELECT 1 FROM kb.chunk c WHERE c.doc_id=d.doc_id) \
                 AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE) \
                 AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE) \
                 AND d.doc_id IN (",
            crate::acl::visible_docs!(),
            ") ORDER BY d.doc_id FOR UPDATE OF d \
             ), guard AS ( \
               SELECT EXISTS(SELECT 1 FROM source) AS source_ok, \
                 (SELECT count(*) FROM requested)=(SELECT count(*) FROM valid) AS targets_ok \
             ), updated AS ( \
               UPDATE kb.doc d SET tags=$4,business_domain=$5,effective_from=$6,effective_to=$7, \
                 source_uri=$8,document_family=$9,document_revision=$10,updated_at=now() \
               FROM source s,guard g WHERE d.doc_id=s.doc_id AND g.source_ok AND g.targets_ok \
               RETURNING d.doc_id,d.space_id \
             ), upserted AS ( \
               INSERT INTO kb.doc_link(space_id,source_doc_id,target_doc_id,kind,created_by) \
               SELECT u.space_id,u.doc_id,v.doc_id,'reference',$1 FROM updated u CROSS JOIN valid v \
               ON CONFLICT (source_doc_id,target_doc_id,kind) DO UPDATE \
                 SET created_by=EXCLUDED.created_by,created_at=now() RETURNING target_doc_id \
             ), removed AS ( \
               DELETE FROM kb.doc_link l USING updated u \
               WHERE l.source_doc_id=u.doc_id AND l.kind='reference' \
                 AND NOT EXISTS (SELECT 1 FROM valid v WHERE v.doc_id=l.target_doc_id) \
               RETURNING l.target_doc_id \
             ), applied AS ( \
               SELECT (SELECT count(*) FROM upserted)+(SELECT count(*) FROM removed) AS link_changes \
             ) SELECT CASE WHEN NOT (SELECT source_ok FROM guard) THEN -1 \
                    WHEN NOT (SELECT targets_ok FROM guard) THEN -2 \
                    ELSE (SELECT count(*) FROM updated)+(0*applied.link_changes) END FROM applied"
        ))
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .bind(doc_id)
        .bind(update.tags)
        .bind(update.business_domain)
        .bind(update.effective_from)
        .bind(update.effective_to)
        .bind(update.source_uri)
        .bind(update.document_family)
        .bind(update.document_revision)
        .bind(&ids)
        .fetch_optional::<(i64,)>()
        .await?
        .map_or(-1, |(n,)| n);

    match state {
        1 => Ok(1),
        -2 => Err(KbError::Forbidden(
            "关联文档无效、不可见、跨空间或未处于有效可检索状态".into(),
        )),
        _ => Err(KbError::Forbidden(format!("文档 {doc_id} 的写权限已失效"))),
    }
}

/// 上传链只在治理字段为空时补文件名推断值。人工维护优先，重处理不得覆盖用户结论。
pub async fn apply_inferred_doc_version(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    document_family: &str,
    document_revision: &str,
) -> Result<(), KbError> {
    let n = store
        .fixed(
            "UPDATE kb.doc d SET \
             document_family=COALESCE(NULLIF(d.document_family,''),$1), \
             document_revision=COALESCE(NULLIF(d.document_revision,''),$2),updated_at=now() \
             FROM kb.space s WHERE d.doc_id=$3 AND s.space_id=d.space_id AND (s.owner=$4 OR EXISTS ( \
               SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=s.space_id \
                 AND a.perm='write' AND ((a.grantee_kind='login' AND a.grantee=$4) OR \
                   (a.grantee_kind='role' AND a.grantee=ANY($5::text[])))))",
        )
        .bind(document_family)
        .bind(document_revision)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 的写权限已失效")));
    }
    Ok(())
}

pub async fn append_notice(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    notice: &str,
) -> Result<(), KbError> {
    if notice.trim().is_empty() {
        return Ok(());
    }
    let n = store
        .fixed(
            "UPDATE kb.doc d SET notice=concat_ws('；',NULLIF(d.notice,''),$1),updated_at=now() \
             FROM kb.space s WHERE d.doc_id=$2 AND s.space_id=d.space_id AND (s.owner=$3 OR EXISTS ( \
               SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=s.space_id \
                 AND a.perm='write' AND ((a.grantee_kind='login' AND a.grantee=$3) OR \
                   (a.grantee_kind='role' AND a.grantee=ANY($4::text[])))))",
        )
        .bind(notice)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 的写权限已失效")));
    }
    Ok(())
}

/// 重处理前清空旧块与状态。数据修改 CTE 保证两步同一条语句完成，避免半清理。
/// 影子解析完成后一次性替换正文索引与文档状态。任何错误都会让旧 chunks 原样保留。
/// `spans` 与 `chunks` 等长平行（B3 字符偏移），语义同 `insert_chunks`。
#[allow(clippy::too_many_arguments)]
pub async fn replace_chunks(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    chunks: &[Chunk],
    embedding_texts: &[String],
    embeddings: &[Option<String>],
    spans: &[Option<CharSpan>],
    page_count: i32,
    status: DocStatus,
    error: &str,
    notice: &str,
) -> Result<(), KbError> {
    if chunks.is_empty()
        || chunks.len() != embedding_texts.len()
        || chunks.len() != embeddings.len()
        || chunks.len() != spans.len()
    {
        return Err(KbError::BadInput("影子索引的切片与向量数量不一致".into()));
    }
    let ord: Vec<i32> = (0..chunks.len() as i32).collect();
    let text: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
    let heading: Vec<&str> = chunks.iter().map(|c| c.heading_path.as_str()).collect();
    let page: Vec<Option<i32>> = chunks.iter().map(|c| c.page).collect();
    let tokens: Vec<i32> = chunks.iter().map(|c| c.tokens).collect();
    let starts: Vec<Option<i32>> = spans.iter().map(|s| s.map(|x| x.start)).collect();
    let ends: Vec<Option<i32>> = spans.iter().map(|s| s.map(|x| x.end)).collect();
    let written = store
        .fixed(
            "WITH locked AS (SELECT d.doc_id,d.name,d.folder_path FROM kb.doc d JOIN kb.space s ON s.space_id=d.space_id \
               WHERE d.doc_id=$1 AND (s.owner=$15 OR EXISTS (SELECT 1 FROM kb.acl a \
                  WHERE a.scope='space' AND a.target_id=s.space_id AND a.perm='write' \
                    AND ((a.grantee_kind='login' AND a.grantee=$15) OR \
                         (a.grantee_kind='role' AND a.grantee=ANY($16::text[])))))) FOR UPDATE), \
             upserted AS ( \
               INSERT INTO kb.chunk(doc_id,ord,text,heading_path,folder_path,page,tokens,embedding_text,embedding_recipe,embedding,start_char_pos,end_char_pos) \
               SELECT $1,u.ord,u.txt,u.heading,l.folder_path,u.page,u.tokens, \
                      kb.chunk_embedding_text(l.name,l.folder_path,u.heading,u.txt),$14, \
                      CASE WHEN u.expected=kb.chunk_embedding_text(l.name,l.folder_path,u.heading,u.txt) \
                           THEN u.embedding::vector ELSE NULL END,u.cstart,u.cend \
               FROM unnest($2::int[],$3::text[],$4::text[],$5::int[],$6::int[],$7::text[],$8::text[],$17::int[],$18::int[]) \
                    AS u(ord,txt,heading,page,tokens,expected,embedding,cstart,cend) CROSS JOIN locked l \
               ON CONFLICT (doc_id,ord) DO UPDATE SET text=EXCLUDED.text, \
                 heading_path=EXCLUDED.heading_path,folder_path=EXCLUDED.folder_path, \
                 page=EXCLUDED.page,tokens=EXCLUDED.tokens, \
                 embedding_text=EXCLUDED.embedding_text,embedding_recipe=EXCLUDED.embedding_recipe, \
                 embedding=EXCLUDED.embedding,start_char_pos=EXCLUDED.start_char_pos, \
                 end_char_pos=EXCLUDED.end_char_pos RETURNING embedding IS NULL AS missing), trimmed AS ( \
               DELETE FROM kb.chunk WHERE doc_id=$1 AND ord >= $13 \
                 AND EXISTS (SELECT 1 FROM upserted) RETURNING 1) \
             UPDATE kb.doc SET status=CASE WHEN EXISTS(SELECT 1 FROM upserted WHERE missing) \
                                            THEN 'chunked' ELSE $9 END,error=$10,notice=$11,page_count=$12, \
                               chunk_count=(SELECT count(*) FROM upserted),updated_at=now() \
             WHERE doc_id=$1 AND EXISTS (SELECT 1 FROM upserted)",
        )
        .bind(doc_id)
        .bind(&ord)
        .bind(&text)
        .bind(&heading)
        .bind(&page)
        .bind(&tokens)
        .bind(embedding_texts)
        .bind(embeddings)
        .bind(status.as_str())
        .bind(error)
        .bind(notice)
        .bind(page_count)
        .bind(chunks.len() as i32)
        .bind(KB_EMBEDDING_RECIPE)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .bind(&starts)
        .bind(&ends)
        .execute()
        .await?;
    if written == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 的写权限已失效")));
    }
    Ok(())
}

pub async fn delete_doc(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
) -> Result<u64, KbError> {
    let n = store
        .fixed(
            "DELETE FROM kb.doc d USING kb.space s WHERE d.doc_id=$1 AND s.space_id=d.space_id \
             AND (s.owner=$2 OR EXISTS (SELECT 1 FROM kb.acl a WHERE a.scope='space' \
               AND a.target_id=s.space_id AND a.perm='write' AND \
               ((a.grantee_kind='login' AND a.grantee=$2) OR \
                (a.grantee_kind='role' AND a.grantee=ANY($3::text[])))))",
        )
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 已不存在或写权限已失效")));
    }
    Ok(n)
}

pub async fn set_counts(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    page_count: i32,
    chunk_count: i32,
) -> Result<(), KbError> {
    let n = store
        .fixed(
            "UPDATE kb.doc d SET page_count=$1,chunk_count=$2,updated_at=now() FROM kb.space s \
             WHERE d.doc_id=$3 AND s.space_id=d.space_id AND (s.owner=$4 OR EXISTS ( \
               SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=s.space_id \
                 AND a.perm='write' AND ((a.grantee_kind='login' AND a.grantee=$4) OR \
                   (a.grantee_kind='role' AND a.grantee=ANY($5::text[])))))",
        )
        .bind(page_count)
        .bind(chunk_count)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 的写权限已失效")));
    }
    Ok(())
}

/// 批量落块，`ord` 从 0。返回真正写入的行数（重跑时 `ON CONFLICT` 会让它小于 `chunks.len()`）。
///
/// 五列各一个数组 + `unnest`，一条语句写完：`QueryBuilder::push_values` 的多行 `VALUES`
/// 是**运行时**拼出来的 SQL，进不了 `fixed()` 的字面量通道。副产物是 bind 数恒为 6
/// 个内容参数 + 3 个授权/配方参数（不再随行数涨），原先「每批 500 行」的分批也随之不必要。
///
/// `spans` 与 `chunks` 等长平行（B3 字符偏移）：元素为 `None` 时落 NULL，
/// 表示该块未能在原文流中可靠定位，回查侧按 ord 邻窗回退。
pub async fn insert_chunks(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    chunks: &[Chunk],
    spans: &[Option<CharSpan>],
) -> Result<usize, KbError> {
    if chunks.is_empty() {
        return Ok(0);
    }
    if chunks.len() != spans.len() {
        return Err(KbError::BadInput("切片与字符偏移数量不一致".into()));
    }
    let ord: Vec<i32> = (0..chunks.len() as i32).collect();
    let text: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
    let heading: Vec<&str> = chunks.iter().map(|c| c.heading_path.as_str()).collect();
    let page: Vec<Option<i32>> = chunks.iter().map(|c| c.page).collect();
    let tokens: Vec<i32> = chunks.iter().map(|c| c.tokens).collect();
    let starts: Vec<Option<i32>> = spans.iter().map(|s| s.map(|x| x.start)).collect();
    let ends: Vec<Option<i32>> = spans.iter().map(|s| s.map(|x| x.end)).collect();
    let written = store
        .fixed(
            "INSERT INTO kb.chunk(doc_id,ord,text,heading_path,folder_path,page,tokens,embedding_text,embedding_recipe,start_char_pos,end_char_pos) \
             SELECT $1,u.ord,u.txt,u.heading,d.folder_path,u.page,u.tokens, \
                    kb.chunk_embedding_text(d.name,d.folder_path,u.heading,u.txt),$7,u.cstart,u.cend \
             FROM unnest($2::int[], $3::text[], $4::text[], $5::int[], $6::int[], $10::int[], $11::int[]) \
                  AS u(ord,txt,heading,page,tokens,cstart,cend) CROSS JOIN kb.doc d \
             JOIN kb.space s ON s.space_id=d.space_id WHERE d.doc_id=$1 AND \
               (s.owner=$8 OR EXISTS (SELECT 1 FROM kb.acl a WHERE a.scope='space' \
                 AND a.target_id=s.space_id AND a.perm='write' AND \
                 ((a.grantee_kind='login' AND a.grantee=$8) OR \
                  (a.grantee_kind='role' AND a.grantee=ANY($9::text[]))))) \
             ON CONFLICT (doc_id,ord) DO NOTHING",
        )
        .bind(doc_id)
        .bind(&ord)
        .bind(&text)
        .bind(&heading)
        .bind(&page)
        .bind(&tokens)
        .bind(KB_EMBEDDING_RECIPE)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .bind(&starts)
        .bind(&ends)
        .execute()
        .await?;
    if written == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 的写权限已失效")));
    }
    Ok(written as usize)
}

/// 按 `ord` 升序取版本化向量任务；回写时以完整快照做 CAS。
pub async fn chunk_embedding_jobs(
    store: &OwnedStore,
    doc_id: &str,
) -> Result<Vec<ChunkEmbeddingJob>, KbError> {
    Ok(store
        .fixed("SELECT chunk_id,embedding_text,embedding_recipe FROM kb.chunk WHERE doc_id=$1 ORDER BY ord")
        .bind(doc_id)
        .fetch_all::<(i64, String, i16)>()
        .await?
        .into_iter()
        .map(|(chunk_id, text, recipe)| ChunkEmbeddingJob { chunk_id, text, recipe })
        .collect())
}

/// 批量回写向量（一条 UNNEST 语句，不做 N 次往返）。`vlit` 是 `to_pgvector` 的字面量。
pub async fn set_embeddings(
    store: &OwnedStore,
    doc_id: &str,
    rows: &[(ChunkEmbeddingJob, String)],
    viewer: &crate::Viewer,
) -> Result<(), KbError> {
    if rows.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = rows.iter().map(|(j, _)| j.chunk_id).collect();
    let texts: Vec<&str> = rows.iter().map(|(j, _)| j.text.as_str()).collect();
    let recipes: Vec<i16> = rows.iter().map(|(j, _)| j.recipe).collect();
    let lits: Vec<&str> = rows.iter().map(|(_, lit)| lit.as_str()).collect();
    store
        .fixed(
            "UPDATE kb.chunk c SET embedding=v.lit::vector FROM kb.doc d, \
             unnest($1::bigint[],$2::text[],$3::smallint[],$4::text[]) v(id,txt,recipe,lit) \
             WHERE c.chunk_id=v.id AND c.doc_id=$5 AND c.embedding_text=v.txt \
               AND c.embedding_recipe=v.recipe AND c.embedding IS NULL AND d.doc_id=c.doc_id \
               AND d.status='chunked' AND d.enabled=true \
               AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE) \
               AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE) \
               AND EXISTS (SELECT 1 FROM kb.space s WHERE s.space_id=d.space_id AND \
                 (s.owner=$6 OR EXISTS (SELECT 1 FROM kb.acl a WHERE a.scope='space' \
                   AND a.target_id=s.space_id AND a.perm='write' AND \
                   ((a.grantee_kind='login' AND a.grantee=$6) OR \
                    (a.grantee_kind='role' AND a.grantee=ANY($7::text[]))))))",
        )
        .bind(&ids)
        .bind(&texts)
        .bind(&recipes)
        .bind(&lits)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    Ok(())
}

/// 【A9】待补向量的块（跨文档，键集游标由调度侧按 LIMIT 分批 —— 与 meta 侧 `FILL_BATCH` 同约）。
/// 用途：`ingest` 在向量服务不可用时把文档停在 `chunked`，原来补它的只有离线脚本
/// `embed_service.py revec`；服务侧自愈（`server/src/embed_fill.rs`）每轮从这里取一批。
pub async fn null_vec_chunks(store: &OwnedStore, limit: i64) -> Result<Vec<ChunkEmbeddingJob>, KbError> {
    Ok(store
        .fixed(
            "SELECT c.chunk_id,c.embedding_text,c.embedding_recipe FROM kb.chunk c JOIN kb.doc d ON d.doc_id=c.doc_id \
             WHERE c.embedding IS NULL AND c.embedding_recipe=$2 AND d.status='chunked' AND d.enabled=true \
               AND (d.effective_from IS NULL OR d.effective_from <= CURRENT_DATE) \
               AND (d.effective_to IS NULL OR d.effective_to >= CURRENT_DATE) \
             ORDER BY c.chunk_id LIMIT $1",
        )
        .bind(limit)
        .bind(KB_EMBEDDING_RECIPE)
        .fetch_all::<(i64, String, i16)>()
        .await?
        .into_iter()
        .map(|(chunk_id, text, recipe)| ChunkEmbeddingJob { chunk_id, text, recipe })
        .collect())
}

/// 【A9】按 `chunk_id + 配方文本 + 配方版本` CAS 写回向量。重建期间任一项变化时影响 0 行，
/// 旧任务绝不能把旧文本向量覆盖到新索引上。
pub async fn set_chunk_embedding(
    store: &OwnedStore,
    chunk_id: i64,
    expected_text: &str,
    expected_recipe: i16,
    lit: &str,
) -> Result<bool, KbError> {
    let n = store
        .fixed(
            "UPDATE kb.chunk c SET embedding=$1::vector FROM kb.doc d \
             WHERE c.chunk_id=$2 AND c.embedding_text=$3 AND c.embedding_recipe=$4 AND c.embedding IS NULL \
               AND d.doc_id=c.doc_id AND d.status='chunked' AND d.enabled=true \
               AND (d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE) \
               AND (d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE)",
        )
        .bind(lit)
        .bind(chunk_id)
        .bind(expected_text)
        .bind(expected_recipe)
        .execute()
        .await?;
    Ok(n > 0)
}

/// 【A9】块全部补完的 `chunked` 文档推到 `embedded`（`ingest` 正常路径的同款状态迁移）。
/// `NOT EXISTS` 是唯一的判据：还有 NULL 块的文档不许提前毕业 ——
/// 「界面显示已入库、其实检索不到」正是这个状态被骗出来的那一族。
pub async fn flip_embedded_docs(store: &OwnedStore) -> Result<u64, KbError> {
    let n = store
        .fixed(
             "UPDATE kb.doc SET status = 'embedded', error = '', updated_at = now() \
             WHERE status = 'chunked' \
             AND enabled=true AND (effective_from IS NULL OR effective_from<=CURRENT_DATE) \
             AND (effective_to IS NULL OR effective_to>=CURRENT_DATE) \
             AND EXISTS (SELECT 1 FROM kb.chunk c WHERE c.doc_id = kb.doc.doc_id) \
             AND NOT EXISTS (SELECT 1 FROM kb.chunk c WHERE c.doc_id = kb.doc.doc_id \
                             AND (c.embedding IS NULL OR c.embedding_recipe<>$1))",
        )
        .bind(KB_EMBEDDING_RECIPE)
        .execute()
        .await?;
    Ok(n)
}

pub async fn promote_doc_if_ready(
    store: &OwnedStore,
    doc_id: &str,
    viewer: &crate::Viewer,
) -> Result<bool, KbError> {
    let n = store
        .fixed(
            "UPDATE kb.doc SET status='embedded',error='',updated_at=now() WHERE doc_id=$1 \
             AND status='chunked' AND enabled=true \
             AND (effective_from IS NULL OR effective_from<=CURRENT_DATE) \
             AND (effective_to IS NULL OR effective_to>=CURRENT_DATE) \
             AND EXISTS (SELECT 1 FROM kb.space s WHERE s.space_id=kb.doc.space_id AND \
               (s.owner=$3 OR EXISTS (SELECT 1 FROM kb.acl a WHERE a.scope='space' \
                 AND a.target_id=s.space_id AND a.perm='write' AND \
                 ((a.grantee_kind='login' AND a.grantee=$3) OR \
                  (a.grantee_kind='role' AND a.grantee=ANY($4::text[])))))) \
             AND EXISTS (SELECT 1 FROM kb.chunk c WHERE c.doc_id=$1) \
             AND NOT EXISTS (SELECT 1 FROM kb.chunk c WHERE c.doc_id=$1 \
               AND (c.embedding IS NULL OR c.embedding_recipe<>$2))",
        )
        .bind(doc_id)
        .bind(KB_EMBEDDING_RECIPE)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    Ok(n > 0)
}

pub async fn list_docs(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    space_id: &str,
) -> Result<Vec<DocRow>, KbError> {
    Ok(store
        .fixed(concat!(
            "SELECT ",
            doc_cols!(),
            " FROM kb.doc WHERE space_id=$3 AND EXISTS (SELECT 1 FROM kb.space s \
               WHERE s.space_id=kb.doc.space_id AND (s.owner=$1 OR EXISTS (SELECT 1 FROM kb.acl a \
                 WHERE a.scope='space' AND a.target_id=s.space_id AND a.perm IN ('read','write') \
                   AND ((a.grantee_kind='login' AND a.grantee=$1) \
                     OR (a.grantee_kind='role' AND a.grantee=ANY($2::text[])))))) \
             ORDER BY created_at DESC"
        ))
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .bind(space_id)
        .fetch_all()
        .await?)
}

/// 【Y7 导出】空间文档总数与分页切片。ACL 谓词与 `list_docs` 同一形状（fail-closed 内联），
/// 分页只加 LIMIT/OFFSET 与稳定次序（created_at DESC, doc_id 决胜），不改可见性语义。
pub async fn count_space_docs(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    space_id: &str,
) -> Result<i64, KbError> {
    Ok(store
        .fixed(
            "SELECT count(*) FROM kb.doc WHERE space_id=$3 AND EXISTS (SELECT 1 FROM kb.space s \
               WHERE s.space_id=kb.doc.space_id AND (s.owner=$1 OR EXISTS (SELECT 1 FROM kb.acl a \
                 WHERE a.scope='space' AND a.target_id=s.space_id AND a.perm IN ('read','write') \
                   AND ((a.grantee_kind='login' AND a.grantee=$1) \
                     OR (a.grantee_kind='role' AND a.grantee=ANY($2::text[]))))))",
        )
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .bind(space_id)
        .fetch_optional::<(i64,)>()
        .await?
        .map_or(0, |(n,)| n))
}

pub async fn list_docs_page(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    space_id: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<DocRow>, KbError> {
    Ok(store
        .fixed(concat!(
            "SELECT ",
            doc_cols!(),
            " FROM kb.doc WHERE space_id=$3 AND EXISTS (SELECT 1 FROM kb.space s \
               WHERE s.space_id=kb.doc.space_id AND (s.owner=$1 OR EXISTS (SELECT 1 FROM kb.acl a \
                 WHERE a.scope='space' AND a.target_id=s.space_id AND a.perm IN ('read','write') \
                   AND ((a.grantee_kind='login' AND a.grantee=$1) \
                     OR (a.grantee_kind='role' AND a.grantee=ANY($2::text[])))))) \
             ORDER BY created_at DESC, doc_id LIMIT $4 OFFSET $5"
        ))
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .bind(space_id)
        .bind(limit)
        .bind(offset)
        .fetch_all()
        .await?)
}

pub async fn move_doc(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    space_id: &str,
    folder_id: Option<&str>,
) -> Result<(), KbError> {
    let moved = store
        .fixed(
            "WITH guard AS (SELECT pg_advisory_xact_lock(hashtextextended($3,0))), \
             writable AS (SELECT s.space_id FROM kb.space s CROSS JOIN guard g \
               WHERE s.space_id=$3 AND (s.owner=$5 OR EXISTS (SELECT 1 FROM kb.acl a \
                 WHERE a.scope='space' AND a.target_id=s.space_id AND a.perm='write' AND \
                 ((a.grantee_kind='login' AND a.grantee=$5) OR \
                  (a.grantee_kind='role' AND a.grantee=ANY($6::text[])))))), \
             folder AS (SELECT f.folder_id,f.path FROM kb.folder f JOIN writable w ON w.space_id=f.space_id \
                        WHERE f.folder_id=$1), \
             eligible AS (SELECT d.doc_id,d.name,$1::text AS folder_id, \
                 CASE WHEN $1::text IS NULL THEN '/' ELSE f.path END AS folder_path \
               FROM kb.doc d JOIN writable w ON d.space_id=w.space_id LEFT JOIN folder f ON true \
               WHERE d.doc_id=$2 AND ($1::text IS NULL OR f.folder_id IS NOT NULL)), \
             chunks AS (UPDATE kb.chunk c SET folder_path=e.folder_path,embedding=NULL, \
               embedding_text=kb.chunk_embedding_text(e.name,e.folder_path,c.heading_path,c.text), \
               embedding_recipe=$4 FROM eligible e WHERE c.doc_id=e.doc_id \
                 AND (c.folder_path IS DISTINCT FROM e.folder_path \
                   OR c.embedding_recipe<>$4 OR c.embedding_text IS DISTINCT FROM \
                     kb.chunk_embedding_text(e.name,e.folder_path,c.heading_path,c.text)) \
               RETURNING c.doc_id), \
             moved AS (UPDATE kb.doc d SET folder_id=e.folder_id,folder_path=e.folder_path, \
               status=CASE WHEN d.status='embedded' AND EXISTS (SELECT 1 FROM chunks c \
                 WHERE c.doc_id=d.doc_id) THEN 'chunked' ELSE d.status END, \
               updated_at=CASE WHEN d.folder_id IS DISTINCT FROM e.folder_id \
                 OR d.folder_path IS DISTINCT FROM e.folder_path \
                 OR EXISTS (SELECT 1 FROM chunks c WHERE c.doc_id=d.doc_id) THEN now() ELSE d.updated_at END \
               FROM eligible e WHERE d.doc_id=e.doc_id RETURNING d.doc_id) \
             SELECT count(*) FROM moved",
        )
        .bind(folder_id)
        .bind(doc_id)
        .bind(space_id)
        .bind(KB_EMBEDDING_RECIPE)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .fetch_optional::<(i64,)>()
        .await?
        .map_or(0, |(n,)| n);
    if moved != 1 {
        if !crate::acl::space_writable(store, viewer, space_id).await? {
            return Err(KbError::Forbidden(format!("知识空间 {space_id} 的写权限已失效")));
        }
        return Err(KbError::BadInput("文档不存在或目标目录无效".into()));
    }
    Ok(())
}

pub async fn related_docs(
    store: &OwnedStore,
    v: &crate::Viewer,
    doc_id: &str,
) -> Result<Vec<DocRelationRow>, KbError> {
    Ok(store
        .fixed(concat!(
            "WITH anchor AS (SELECT doc_id,space_id,folder_id,folder_path,document_family,document_revision, \
                    business_domain,tags \
             FROM kb.doc WHERE doc_id=$3 AND doc_id IN (",
            crate::acl::visible_docs!(),
            ")), candidates AS ( \
             SELECT d.doc_id,d.name AS doc_name,d.folder_id,d.folder_path,d.document_family, \
                    d.document_revision,'same_folder'::text AS relation,2 AS priority \
             FROM kb.doc d JOIN anchor a ON d.space_id=a.space_id \
               AND a.folder_id IS NOT NULL AND d.folder_id=a.folder_id \
             WHERE d.doc_id<>a.doc_id \
             UNION ALL SELECT d.doc_id,d.name,d.folder_id,d.folder_path,d.document_family,d.document_revision, \
                    'ancestor_folder',3 FROM kb.doc d JOIN anchor a ON d.space_id=a.space_id \
             WHERE d.doc_id<>a.doc_id \
               AND NULLIF(btrim(d.folder_path),'') IS NOT NULL \
               AND NULLIF(btrim(a.folder_path),'') IS NOT NULL \
               AND btrim(d.folder_path)<>'/' AND btrim(a.folder_path)<>'/' \
               AND left(a.folder_path,length(d.folder_path)+1)=d.folder_path||'/' \
             UNION ALL SELECT d.doc_id,d.name,d.folder_id,d.folder_path,d.document_family,d.document_revision, \
                    'descendant_folder',4 FROM kb.doc d JOIN anchor a ON d.space_id=a.space_id \
             WHERE d.doc_id<>a.doc_id \
               AND NULLIF(btrim(d.folder_path),'') IS NOT NULL \
               AND NULLIF(btrim(a.folder_path),'') IS NOT NULL \
               AND btrim(d.folder_path)<>'/' AND btrim(a.folder_path)<>'/' \
               AND left(d.folder_path,length(a.folder_path)+1)=a.folder_path||'/' \
             UNION ALL SELECT d.doc_id,d.name,d.folder_id,d.folder_path,d.document_family,d.document_revision, \
                    'document_revision',1 FROM kb.doc d JOIN anchor a ON d.space_id=a.space_id \
             WHERE d.doc_id<>a.doc_id AND NULLIF(a.document_family,'') IS NOT NULL \
               AND NULLIF(a.document_revision,'') IS NOT NULL \
               AND d.document_family=a.document_family AND d.document_revision=a.document_revision \
             UNION ALL SELECT d.doc_id,d.name,d.folder_id,d.folder_path,d.document_family,d.document_revision, \
                    'document_family',5 FROM kb.doc d JOIN anchor a ON d.space_id=a.space_id \
             WHERE d.doc_id<>a.doc_id AND NULLIF(a.document_family,'') IS NOT NULL \
               AND d.document_family=a.document_family \
             UNION ALL SELECT d.doc_id,d.name,d.folder_id,d.folder_path,d.document_family,d.document_revision, \
                    'same_domain',6 FROM kb.doc d JOIN anchor a ON d.space_id=a.space_id \
             WHERE d.doc_id<>a.doc_id AND NULLIF(btrim(a.business_domain),'') IS NOT NULL \
               AND d.business_domain=a.business_domain \
             UNION ALL SELECT d.doc_id,d.name,d.folder_id,d.folder_path,d.document_family,d.document_revision, \
                    'shared_tag',7 FROM kb.doc d JOIN anchor a ON d.space_id=a.space_id \
             WHERE d.doc_id<>a.doc_id AND EXISTS ( \
               SELECT 1 FROM unnest(a.tags) AS at(tag) \
               JOIN unnest(d.tags) AS dt(tag) ON dt.tag=at.tag \
               WHERE NULLIF(btrim(at.tag),'') IS NOT NULL \
             ) \
             UNION ALL SELECT d.doc_id,d.name,d.folder_id,d.folder_path,d.document_family,d.document_revision, \
                    CASE WHEN l.source_doc_id=$3 THEN 'references' ELSE 'referenced_by' END,0 \
             FROM kb.doc_link l JOIN anchor a ON true \
             JOIN kb.doc d ON d.doc_id=CASE WHEN l.source_doc_id=$3 \
                    THEN l.target_doc_id ELSE l.source_doc_id END AND d.space_id=a.space_id \
             WHERE l.source_doc_id=$3 OR l.target_doc_id=$3 \
             ) SELECT DISTINCT ON (c.doc_id) c.doc_id,c.doc_name,c.folder_id,c.folder_path, \
                      c.document_family,c.document_revision,c.relation \
               FROM candidates c JOIN kb.doc live ON live.doc_id=c.doc_id \
               WHERE live.enabled=true AND live.status IN ('chunked','embedded') \
                 AND (live.effective_from IS NULL OR live.effective_from<=CURRENT_DATE) \
                 AND (live.effective_to IS NULL OR live.effective_to>=CURRENT_DATE) \
                 AND c.doc_id IN (",
            crate::acl::visible_docs!(),
            ") ORDER BY c.doc_id,c.priority,c.relation LIMIT 20"
        ))
        .bind(&v.login)
        .bind(&v.roles)
        .bind(doc_id)
        .fetch_all()
        .await?)
}

/// 按 id 取文档，**不做可见性判定**（要判定用 `acl::doc_for_viewer`）
pub async fn get_doc(store: &OwnedStore, doc_id: &str) -> Result<Option<DocRow>, KbError> {
    Ok(store
        .fixed(concat!("SELECT ", doc_cols!(), " FROM kb.doc WHERE doc_id=$1"))
        .bind(doc_id)
        .fetch_optional()
        .await?)
}

/// sha256 十六进制。
///
/// `ponytail:` 走 PG 内置 `encode(sha256($1::bytea),'hex')`——全仓无 sha2 依赖，
/// 零新增依赖的前提下只有这条路。代价是几十 MB 的 bytea 多一次本地往返；
/// 真成瓶颈就让 `/parse` 顺带返 sha256，本函数随之删除。
///
/// 用 `fetch_optional` 是因为 `PgStmt` 没有 `fetch_one`（`fixed()` 通道只给三个终结子）；
/// 单行聚合查询的「没有行」是不可能事件，落 `Db` 而不是静默给空串。
pub async fn sha256_hex(store: &OwnedStore, bytes: &[u8]) -> Result<String, KbError> {
    store
        .fixed("SELECT encode(sha256($1::bytea),'hex')")
        .bind(bytes)
        .fetch_optional::<(String,)>()
        .await?
        .map(|(hex,)| hex)
        .ok_or_else(|| KbError::Db("sha256 计算没有返回结果".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hierarchy_embedding_recipe_is_stable() {
        assert_eq!(
            chunk_embedding_text("制度.md", "/财务/报销", "第一章 > 范围", "正文"),
            "文件：制度.md\n目录：/财务/报销\n章节：第一章 > 范围\n\n正文"
        );
        assert_eq!(KB_EMBEDDING_RECIPE, 1);
    }

    #[test]
    fn status_roundtrip() {
        for s in [
            DocStatus::Pending,
            DocStatus::Parsing,
            DocStatus::Chunked,
            DocStatus::Embedded,
            DocStatus::Failed,
        ] {
            assert_eq!(DocStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(DocStatus::parse("PENDING"), None);
        assert_eq!(DocStatus::parse(""), None);
    }

    /// 切分前提的漂移守卫：改 0020 时若引入含分号的语句体，这条会红
    #[test]
    fn ddl_splits_without_breaking_statements() {
        let stmts: Vec<&str> = statements(KB_DDL).collect();
        assert!(stmts.len() >= 30, "0020 应包含目录、文档关系与既有知识库 DDL");
        assert!(stmts.iter().all(|s| {
            let u = s.to_uppercase();
            u.contains("CREATE") || u.contains("ALTER TABLE") || u.contains("UPDATE")
                || u.contains("DELETE") || u.contains("DROP TRIGGER") || u.contains("DO $$")
        }));
        assert!(stmts.iter().any(|s| s.contains("guard_folder_tree") && s.contains("folder cycle")));
        let chunk = stmts.iter().find(|s| s.contains("kb.chunk(")).unwrap();
        // 生成列与表尾约束必须落在同一条语句里（切坏了这两个断言会红）
        assert!(chunk.contains("GENERATED ALWAYS AS") && chunk.contains("STORED"));
        assert!(chunk.contains("UNIQUE(doc_id, ord)"));
    }

    #[test]
    fn comment_only_fragments_dropped() {
        assert!(is_comment_only("-- a\n\n-- b"));
        assert!(!is_comment_only("-- a\nCREATE TABLE x()"));
    }

    #[test]
    fn doc_cols_match_row_fields() {
        // 列清单与 DocRow 字段一一对应（FromRow 靠名字取列，漏一列是运行时错，钉在这里）
        assert_eq!(DOC_COLS.split(',').count(), 25);
        for col in [
            "folder_id",
            "folder_path",
            "tags",
            "business_domain",
            "effective_from",
            "effective_to",
            "source_uri",
            "document_family",
            "document_revision",
            "description",
        ] {
            assert!(DOC_COLS.split(',').any(|item| item.trim() == col));
        }
        assert!(DOC_COLS.contains("created_at::text AS created_at"));
        assert!(DOC_COLS.contains("updated_at::text AS updated_at"));
    }

    /// 【Y7/Y12】description 增量迁移幂等；两个回写函数与导出分页都保持 ACL 内联（fail-closed）
    #[test]
    fn description_column_and_writebacks_keep_acl_inline() {
        assert!(KB_DDL_DELTA.contains("ADD COLUMN IF NOT EXISTS description text NOT NULL DEFAULT ''"));
        let src = include_str!("store.rs");
        for f in ["pub async fn set_doc_source_uri", "pub async fn set_doc_description"] {
            let body = src.split(f).nth(1).unwrap();
            let body = body.split("\n}\n").next().unwrap();
            assert!(body.contains("a.perm='write'"), "{f} 写复核丢了");
            assert!(body.contains("a.grantee=ANY($4::text[])"), "{f} 角色谓词丢了");
        }
        let page = src.split("pub async fn list_docs_page").nth(1).unwrap();
        let page = page.split("\n}\n").next().unwrap();
        assert!(page.contains("LIMIT $4 OFFSET $5"));
        assert!(page.contains("a.perm IN ('read','write')"), "导出分页的读谓词丢了");
    }

    #[test]
    fn folder_and_relation_schema_contracts_are_migrated() {
        for ddl in [
            "CREATE TABLE IF NOT EXISTS kb.folder",
            "uq_kb_folder_sibling_name",
            "uq_kb_folder_path",
            "ADD COLUMN IF NOT EXISTS folder_id",
            "ADD COLUMN IF NOT EXISTS folder_path",
            "CREATE TABLE IF NOT EXISTS kb.doc_link",
        ] {
            assert!(KB_DDL.contains(ddl), "缺少目录/关系迁移合同: {ddl}");
        }
        let src = include_str!("store.rs");
        let links = src.split("pub async fn update_doc_metadata_and_links").nth(1).unwrap();
        let links = links.split("pub async fn related_docs").next().unwrap();
        assert!(links.contains(".fixed(concat!("));
        assert_eq!(links.matches("crate::acl::visible_docs!()").count(), 2);
        assert!(links.contains("ON CONFLICT (source_doc_id,target_doc_id,kind) DO UPDATE"));
        assert!(links.contains("l.kind='reference'"));
        assert!(links.contains("requested AS ("));
        assert!(links.contains("FROM candidate_source"));
        assert!(links.contains("CROSS JOIN lock_guard FOR UPDATE OF d"));
        assert!(links.contains("ORDER BY d.doc_id FOR UPDATE OF d"));
        assert!(links.contains("count(*) FROM upserted"));
        assert!(links.contains("count(*) FROM removed"));
        assert!(links.contains("FROM source s,guard g"));
        assert!(links.contains("g.source_ok AND g.targets_ok"));
        assert!(links.contains("FROM updated u CROSS JOIN valid v"));
        assert!(links.contains("DELETE FROM kb.doc_link l USING updated u"));
        assert!(
            links.contains("(SELECT count(*) FROM requested)=(SELECT count(*) FROM valid)"),
            "空数组时 requested=valid=0，guard 必须允许清空旧引用"
        );
        for contract in [
            "d.enabled=true",
            "d.status IN ('chunked','embedded')",
            "EXISTS (SELECT 1 FROM kb.chunk c WHERE c.doc_id=d.doc_id)",
            "d.effective_from IS NULL OR d.effective_from<=CURRENT_DATE",
            "d.effective_to IS NULL OR d.effective_to>=CURRENT_DATE",
        ] {
            assert!(links.contains(contract), "关联目标缺少生命周期约束: {contract}");
        }
    }

    #[test]
    fn unclassified_documents_are_not_implicitly_same_folder() {
        let src = include_str!("store.rs");
        let related = src.split("pub async fn related_docs").nth(1).unwrap();
        assert!(related.contains("a.folder_id IS NOT NULL AND d.folder_id=a.folder_id"));
        assert!(!related.contains("d.folder_id IS NOT DISTINCT FROM a.folder_id"));
        assert!(!related.contains("d.folder_path='/' AND a.folder_path<>'/'"));
        assert!(!related.contains("a.folder_path='/' AND d.folder_path<>'/'"));
        assert!(related.matches("btrim(d.folder_path)<>'/' AND btrim(a.folder_path)<>'/'").count() >= 2);
        assert!(related.matches("NULLIF(btrim(d.folder_path),'') IS NOT NULL").count() >= 2);
        assert!(related.matches("NULLIF(btrim(a.folder_path),'') IS NOT NULL").count() >= 2);
    }

    #[test]
    fn metadata_relations_are_low_priority_acl_safe_context() {
        let src = include_str!("store.rs");
        let related = src.split("pub async fn related_docs").nth(1).unwrap();
        for contract in [
            "NULLIF(btrim(a.business_domain),'') IS NOT NULL",
            "d.business_domain=a.business_domain",
            "JOIN unnest(d.tags) AS dt(tag) ON dt.tag=at.tag",
            "NULLIF(btrim(at.tag),'') IS NOT NULL",
            "'descendant_folder',4",
            "'same_domain',6",
            "'shared_tag',7",
            "live.enabled=true",
            "live.status IN ('chunked','embedded')",
            "live.effective_from IS NULL OR live.effective_from<=CURRENT_DATE",
            "live.effective_to IS NULL OR live.effective_to>=CURRENT_DATE",
            "LIMIT 20",
        ] {
            assert!(related.contains(contract), "元数据关系缺少合同 {contract}");
        }
        assert!(related.matches("crate::acl::visible_docs!()").count() >= 2);
    }

    #[test]
    fn governance_metadata_is_migrated_and_updated_together() {
        for ddl in [
            "ADD COLUMN IF NOT EXISTS tags text[] NOT NULL DEFAULT '{}'",
            "ADD COLUMN IF NOT EXISTS business_domain text",
            "ADD COLUMN IF NOT EXISTS effective_from date",
            "ADD COLUMN IF NOT EXISTS effective_to date",
            "ADD COLUMN IF NOT EXISTS source_uri text",
            "ADD COLUMN IF NOT EXISTS document_family text",
            "ADD COLUMN IF NOT EXISTS document_revision text",
        ] {
            assert!(KB_DDL.contains(ddl), "缺少治理字段迁移: {ddl}");
        }

        let src = include_str!("store.rs");
        let body = src
            .split("pub async fn update_doc_metadata_and_links")
            .nth(1)
            .unwrap()
            .split("pub async fn append_notice")
            .next()
            .unwrap();
        assert!(body.contains(
            "UPDATE kb.doc d SET tags=$4,business_domain=$5,effective_from=$6,effective_to=$7"
        ));
        assert!(body.contains("source_uri=$8,document_family=$9,document_revision=$10"));
        assert!(body.contains("w.perm='write'"));
        assert!(body.contains("w.grantee=ANY($2::text[])"));
    }

    #[test]
    fn background_embedding_write_is_compare_and_set() {
        let src = include_str!("store.rs");
        let body = src.split("pub async fn set_chunk_embedding").nth(1).unwrap();
        assert!(body.contains("c.embedding_text=$3"));
        assert!(body.contains("c.embedding_recipe=$4"));
        assert!(body.contains("c.embedding IS NULL"));
        assert!(body.contains("d.status='chunked'"));
        let pending = src.split("pub async fn null_vec_chunks").nth(1).unwrap();
        assert!(pending.contains("d.status='chunked'"));
        assert!(pending.contains("d.enabled=true"));
        let flip = src.split("pub async fn flip_embedded_docs").nth(1).unwrap();
        assert!(flip.contains("AND EXISTS (SELECT 1 FROM kb.chunk"));
    }

    #[test]
    fn shadow_replace_is_one_statement_and_locks_the_doc() {
        let src = include_str!("store.rs");
        let body = src.split("pub async fn replace_chunks").nth(1).unwrap();
        assert!(body.contains("SELECT d.doc_id,d.name,d.folder_path FROM kb.doc d JOIN kb.space s"));
        assert!(body.contains("s.owner=$15"));
        assert!(body.contains("a.grantee=ANY($16::text[])"));
        assert!(!body.contains("a.grantee=d.uploaded_by"));
        assert!(body.contains("ON CONFLICT (doc_id,ord) DO UPDATE"));
        assert!(body.contains("DELETE FROM kb.chunk WHERE doc_id=$1 AND ord >= $13"));
        assert!(body.contains("INSERT INTO kb.chunk"));
        assert!(body.contains("UPDATE kb.doc SET status=CASE WHEN EXISTS(SELECT 1 FROM upserted WHERE missing)"));
    }

    #[test]
    fn hierarchy_writes_recheck_current_actor_and_invalidate_vectors() {
        let src = include_str!("store.rs");
        for name in ["create_folder", "move_folder", "delete_folder", "move_doc"] {
            let marker = format!("pub async fn {name}");
            let body = src.split(marker.as_str()).nth(1).unwrap();
            let body = body.split("pub async fn ").next().unwrap();
            assert!(body.contains("perm='write'"), "{name} 未在写语句内复核权限");
            assert!(body.contains("grantee_kind='role'"), "{name} 未复核角色授权");
        }
        for name in ["move_folder", "move_doc"] {
            let marker = format!("pub async fn {name}");
            let body = src.split(marker.as_str()).nth(1).unwrap();
            let body = body.split("pub async fn ").next().unwrap();
            assert!(body.contains("embedding=NULL"), "{name} 未让旧目录向量失效");
            assert!(body.contains("kb.chunk_embedding_text"), "{name} 未按新层级重建语义文本");
        }
        let insert = src.split("pub async fn insert_chunks").nth(1).unwrap();
        assert!(insert.contains("a.grantee=ANY($9::text[])"));
    }

    #[test]
    fn hierarchy_reads_and_role_grants_are_fail_closed_in_sql() {
        let src = include_str!("store.rs");
        for name in ["list_folders", "list_docs"] {
            let marker = format!("pub async fn {name}");
            let body = src.split(marker.as_str()).nth(1).unwrap();
            let body = body.split("pub async fn ").next().unwrap();
            assert!(body.contains("s.owner=$1"), "{name} 未在查询内核验空间 owner");
            assert!(body.contains("grantee=ANY($2::text[])"), "{name} 未在查询内核验角色 ACL");
            assert!(body.contains("perm IN ('read','write')"), "{name} 未限定可读权限");
        }
        for name in ["grant_space_roles", "grant_space_acl", "revoke_space_acl"] {
            let marker = format!("pub async fn {name}");
            let body = src.split(marker.as_str()).nth(1).unwrap();
            let body = body.split("pub async fn ").next().unwrap();
            assert!(body.contains("WITH target AS (SELECT space_id FROM kb.space WHERE space_id=$1)"));
            assert!(body.contains("EXISTS(SELECT 1 FROM target)"));
        }
        for name in ["grant_space_roles", "grant_space_acl"] {
            let marker = format!("pub async fn {name}");
            let body = src.split(marker.as_str()).nth(1).unwrap();
            let body = body.split("pub async fn ").next().unwrap();
            assert!(body.contains("removed AS (DELETE FROM kb.acl"), "{name} 未原子替换旧权限档");
            assert!(body.contains("a.perm<>"), "{name} 未保证 read/write 互斥");
        }
    }

    #[test]
    fn document_state_writes_recheck_current_actor() {
        let src = include_str!("store.rs");
        for name in [
            "set_status",
            "set_notice",
            "set_enabled",
            "update_doc_metadata_and_links",
            "apply_inferred_doc_version",
            "append_notice",
            "delete_doc",
            "set_counts",
        ] {
            let marker = format!("pub async fn {name}");
            let body = src.split(marker.as_str()).nth(1).unwrap();
            let body = body.split("pub async fn ").next().unwrap();
            assert!(body.contains("perm='write'"), "{name} 未在写语句内复核权限");
            assert!(body.contains("grantee_kind='role'"), "{name} 未复核角色授权");
            assert!(body.contains("viewer"), "{name} 未接收当前操作者");
        }
    }

    /// B3：chunk 字符偏移列以幂等 ALTER 增量迁移落地，且必须挂在 migrate 执行链上。
    #[test]
    fn chunk_char_pos_migration_is_idempotent_and_wired() {
        let stmts: Vec<&str> = statements(KB_DDL_DELTA).collect();
        assert_eq!(stmts.len(), 3);
        assert!(stmts[..2].iter().all(|s| s.starts_with("ALTER TABLE kb.chunk ADD COLUMN IF NOT EXISTS")));
        assert!(KB_DDL_DELTA.contains("start_char_pos int"));
        assert!(KB_DDL_DELTA.contains("end_char_pos int"));
        // Y7：第三条是 doc.description（AI 描述列），同样幂等
        assert!(stmts[2].starts_with("ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS description text NOT NULL DEFAULT ''"));
        let src = include_str!("store.rs");
        let migrate = src.split("pub async fn migrate").nth(1).unwrap();
        let migrate = migrate.split("pub async fn ").next().unwrap();
        assert!(migrate.contains("statements(KB_DDL).chain(statements(KB_DDL_DELTA))"));
    }

    /// B3：两条落块语句都必须写偏移列；`None` 元素经 `Vec<Option<i32>>` 落 NULL。
    #[test]
    fn chunk_writes_carry_char_spans() {
        let src = include_str!("store.rs");
        let insert = src.split("pub async fn insert_chunks").nth(1).unwrap();
        let insert = insert.split("pub async fn ").next().unwrap();
        assert!(insert.contains("start_char_pos,end_char_pos"));
        assert!(insert.contains("$10::int[], $11::int[]"));
        assert!(insert.contains("u.cstart,u.cend"));
        // $9 仍是角色数组（既有判据钉住），偏移数组只能追加其后
        assert!(insert.contains("a.grantee=ANY($9::text[])"));
        assert!(insert.contains("spans: &[Option<CharSpan>]"));

        let replace = src.split("pub async fn replace_chunks").nth(1).unwrap();
        let replace = replace.split("pub async fn ").next().unwrap();
        assert!(replace.contains("start_char_pos,end_char_pos"));
        assert!(replace.contains("$17::int[],$18::int[]"));
        assert!(replace.contains("start_char_pos=EXCLUDED.start_char_pos"));
        assert!(replace.contains("end_char_pos=EXCLUDED.end_char_pos"));
        // $15/$16 仍是 login/roles（既有判据钉住）
        assert!(replace.contains("s.owner=$15"));
        assert!(replace.contains("a.grantee=ANY($16::text[])"));
        assert!(replace.contains("chunks.len() != spans.len()"));
    }

    /// B7：同 `(space_id, sha256)` 的并发插入必须原子让位给先到者，而不是唯一约束 500。
    #[test]
    fn insert_doc_conflict_yields_duplicate_not_error() {
        let src = include_str!("store.rs");
        let body = src.split("pub async fn insert_doc").nth(1).unwrap();
        let body = body.split("pub async fn ").next().unwrap();
        assert!(body.contains("ON CONFLICT (space_id, sha256) DO NOTHING RETURNING doc_id"));
        assert!(body.contains("DocInsert::Duplicate"));
        assert!(body.contains("find_by_sha"));
        // 无权限且无人抢占时仍然 fail-closed
        assert!(body.contains("KbError::Forbidden"));
    }
}
