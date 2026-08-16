//! kb 表结构与状态机的唯一读写落点。变更原因＝表结构。
//! ACL 查询真相仍在 `acl.rs`；长耗时上传发布、目录变更与文档关联在本层 SQL 内再次复核
//! 当前操作者的 login + 全部角色，避免只依赖 API 先验检查，也不能拿上传者历史权限冒充当前授权。
//! **不含编排**（那是 `ingest.rs`）。
//! DDL 真相源是 `crates/semantic/migrations/0020_kb_init.sql`，此处只 `include_str!` 不复述。

use crate::KbError;
use dms_connector::doc::Chunk;
use dms_connector::owned::OwnedStore;

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

/// jieba 实例全局只建一次（词典编译进二进制，加载是一次性成本；实例本身无状态）。
/// 这是纯函数备忘表，不是配置/文件单例 —— 同一输入恒同一输出，不破坏检索的纯函数式纪律。
static JIEBA: std::sync::OnceLock<jieba_rs::Jieba> = std::sync::OnceLock::new();

fn jieba() -> &'static jieba_rs::Jieba {
    JIEBA.get_or_init(jieba_rs::Jieba::new)
}

/// 问句语法词（纯疑问/客套词）：匹配价值为零，**对称**剔除（存/查过同一个 `terms_of`，
/// 两侧都见不到这些词，匹配结果不变，只省噪声）。任何带实体语义的词进这张表就是丢召回。
const TERMS_STOPWORDS: &[&str] = &[
    "请问", "麻烦", "一下", "什么", "怎么", "怎样", "怎么样", "如何", "多少", "哪些", "哪个",
    "哪几", "为什么", "为啥", "有没有", "是不是", "能不能",
];

/// 词级稀疏召回（第 9 路）的分词**单一事实源**：写入（`insert_chunks`/`replace_chunks`）、
/// 查询（`retrieve` 词级路）、存量回填（`terms_backfill`）三处共用 —— 两侧各写一份，
/// 存储列与查询词就对不齐。
///
/// 口径（与 `retrieve::normalize_query` 对齐的那半必须逐字一致）：
/// - 先做全角折叠（\u{3000}→空格、FF01-FF5E→半角）：块正文没归一化，不折会让
///   全角型号「ＤＨＴ１５０」配不上问句里的半角「dht150」；
/// - jieba 精确模式 + HMM（未登录词发现：公司黑话/新词不在词典也切得出）；
/// - 词内去控制字符（`\x01` 是落库线格式的分隔符，见 `terms_blob`）、小写化；
/// - 只留 **≥2 个字符且含字母/数字/汉字**的词：单字词几乎全是语法字（的/了/吗/呢），
///   判别力极弱，留着只会把词级路变成噪声放大器；纯标点词同理丢弃；
/// - 去重保序（与 `retrieve::exact_tokens` 同约）。
pub fn terms_of(text: &str) -> Vec<String> {
    let folded: String = text
        .chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            '\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xfee0).unwrap_or(c),
            _ => c,
        })
        .collect();
    let mut out: Vec<String> = Vec::new();
    for tok in jieba().cut(&folded, true) {
        let w: String =
            tok.word.chars().filter(|c| !c.is_control()).flat_map(char::to_lowercase).collect();
        let w = w.trim();
        if w.chars().count() < 2 {
            continue;
        }
        if !w.chars().any(char::is_alphanumeric) {
            continue;
        }
        if TERMS_STOPWORDS.contains(&w) {
            continue;
        }
        if !out.iter().any(|x| x == w) {
            out.push(w.to_string());
        }
    }
    out
}

/// terms 的落库线格式：`\x01` 分隔的单串（`terms_of` 已保证词内无控制字符），
/// SQL 侧 `string_to_array(?, chr(1))` 还原。为什么不绑 `text[][]`：PG 的 `unnest`
/// 会把多维数组**拍平**成一维（各块的词界全丢），这是 PG 数组的著名坑；
/// 分隔符串让两条落块语句的 `unnest` 平行数组模式继续成立。空词表落 `''` → `{}`。
fn terms_blob(terms: &[String]) -> String {
    terms.join("\u{1}")
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
ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS last_ingest_error text NOT NULL DEFAULT '';
-- 🔴 向量维度幂等改型（2026-08-17，512 → 1024）。与 `semantic::ddl` 里 meta 那五张表
-- 同一段逻辑：`0020_kb_init.sql` 的 `vector(1024)` 只对新库成立，已有库的列还停在旧维度，
-- 而维度对不上是**当场报错**（写 `expected 512 dimensions`、读 `<=>` 同样报），
-- 三条向量路一起被 `.unwrap_or_default()` 吞成空集。
-- 改型顺带清 NULL（不同维度的向量不可比，留着只会让检索悄悄变差），
-- 并把受影响文档退回 `chunked` —— 否则 `revec` 的 `KB_SEL` 只扫 chunked，会扫到 0 行还退 0。
DO $$
DECLARE current_dim int;
BEGIN
  SELECT a.atttypmod INTO current_dim
    FROM pg_attribute a
   WHERE a.attrelid = to_regclass('kb.chunk') AND a.attname = 'embedding' AND NOT a.attisdropped;
  IF current_dim IS NOT NULL AND current_dim <> 1024 THEN
    RAISE NOTICE '向量维度改型：kb.chunk 从 % → 1024（旧向量作废置 NULL，文档退回 chunked）', current_dim;
    EXECUTE 'ALTER TABLE kb.chunk ALTER COLUMN embedding TYPE vector(1024) USING NULL';
    UPDATE kb.doc SET status = 'chunked' WHERE status = 'embedded';
  END IF;
END $$;
";

/// `DocRow` 的列清单。`created_at` 取 `::text`——为一个纯展示字段给 knowledge 引 chrono 不值当。
///
/// **是宏不是 `const`**：`OwnedStore::fixed()` 只吃 `&'static str`，列清单必须在**编译期**
/// 拼进 SQL（原来的 `format!("SELECT {DOC_COLS} …")` 产出 `String`，那条路已被类型堵死）。
/// 单一真相源仍是这一处，`DOC_COLS` 与三条 `SELECT` 都从它展开。
macro_rules! doc_cols {
    () => {
        "doc_id,space_id,folder_id,folder_path,name,mime,bytes,sha256,status,enabled,tags,business_domain,\
         effective_from,effective_to,source_uri,document_family,document_revision,error,notice,description,last_ingest_error,\
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
/// 事务只经 `OwnedStore::begin_fixed` 暴露的字面量语句通道执行；迁移文本全部来自
/// `include_str!` 的 `&'static str`，没有接收请求内容或运行时 SQL 的入口。
///
/// **装配顺序**：依赖 `vector` 与 `pg_trgm` 两个扩展，由 `meta::migrate` 先建 —— 必须在它之后跑。
pub async fn migrate(store: &OwnedStore) -> Result<(), KbError> {
    // 多实例同时启动会并发跑这份 DDL：事务内顾问锁收口，避免锁等/约束名撞车
    let mut tx = store.begin_fixed().await?;
    tx.fixed("SELECT pg_advisory_xact_lock(hashtext('kb.migrate'))")
        .execute()
        .await?;
    for ddl in [KB_DDL, KB_DDL_DELTA] {
        // 防御：切分器只认裸 `$$`；迁移若引入 `$tag$` 形式会被从函数体内切坏，
        // 在这里明确报错，而不是带着切坏的语句执行
        if has_tagged_dollar_quote(ddl) {
            return Err(KbError::Db("迁移 DDL 含 $tag$ dollar-quote，切分器不支持".into()));
        }
        for stmt in statements(ddl) {
            tx.fixed(stmt).execute().await?;
        }
    }
    tx.commit().await?;
    // 词级路存量回填：列建好后挂后台任务（terms IS NULL 游标，幂等可续跑）。
    // 必须排在 commit 之后 —— 任务另起连接读表，事务没提交它看不到新列。
    spawn_terms_backfill(store.clone());
    Ok(())
}

/// 检测 `$tag$` 形式的 dollar-quote（`statements` 只认裸 `$$`，不认识带 tag 的）。
/// tag 以字母/下划线开头：`$1$` 这类参数占位不会被误判。
fn has_tagged_dollar_quote(ddl: &str) -> bool {
    let b = ddl.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'$' && i + 1 < b.len() && (b[i + 1].is_ascii_alphabetic() || b[i + 1] == b'_') {
            let mut j = i + 2;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if j < b.len() && b[j] == b'$' {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// 入参**必须**是 `&'static str`（唯一调用点是 `include_str!` 的 `KB_DDL`）：
/// 切片继承这个生命周期，才进得去 `fixed()`。
///
/// ⚠️ 只识别裸 `$$` 的 dollar-quote，**不支持 `$tag$` 形式**——迁移若引入 `$func$`
/// 之类会被从函数体内切坏（`migrate` 入口有 `has_tagged_dollar_quote` 防御断言挡这种形态）。
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
    /// 最近一次影子重建/覆盖失败原因；不改 `status`，因此旧线上版本仍可检索。
    pub last_ingest_error: String,
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
            last_ingest_error: row.try_get("last_ingest_error")?,
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
    // 父目录 FK 是 RESTRICT，空树需在同一事务里按最深层优先删除；空间锁与权限复核同语句。
    let mut tx = store.begin_fixed().await?;
    let target: Option<(String,)> = tx
        .fixed(
        "WITH target AS (SELECT f.space_id FROM kb.folder f \
           WHERE f.folder_id=$1 AND ($4::text IS NULL OR f.space_id=$4)), \
         guard AS (SELECT pg_advisory_xact_lock(hashtextextended(space_id,0)) FROM target) \
         SELECT f.space_id FROM kb.folder f JOIN kb.space s ON s.space_id=f.space_id \
         CROSS JOIN guard g WHERE f.folder_id=$1 \
         AND (s.owner=$2 OR EXISTS (SELECT 1 FROM kb.acl a WHERE a.scope='space' \
           AND a.target_id=s.space_id AND a.perm='write' AND \
           ((a.grantee_kind='login' AND a.grantee=$2) OR \
             (a.grantee_kind='role' AND a.grantee=ANY($3::text[])))))",
        )
        .bind(folder_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .bind(expected_space_id)
        .fetch_optional()
        .await?;
    let Some((space_id,)) = target else {
        tx.rollback().await?;
        let Some(current) = get_folder(store, folder_id).await? else {
            return Err(KbError::Forbidden("目录不存在或无权删除".into()));
        };
        if !crate::acl::space_writable(store, viewer, &current.space_id).await? {
            return Err(KbError::Forbidden("目录不存在或无权删除".into()));
        }
        return Err(KbError::BadInput("目录不存在或目录非空，不能删除".into()));
    };
    let descendants: Vec<(String, i32)> = tx
        .fixed(
            "WITH RECURSIVE tree(folder_id,depth,seen) AS ( \
           SELECT folder_id,0,ARRAY[folder_id]::text[] FROM kb.folder WHERE folder_id=$1 AND space_id=$2 \
           UNION ALL SELECT c.folder_id,t.depth+1,t.seen||c.folder_id FROM kb.folder c \
             JOIN tree t ON c.parent_id=t.folder_id AND c.space_id=$2 \
             WHERE NOT c.folder_id=ANY(t.seen) \
         ) SELECT folder_id,depth FROM tree ORDER BY depth DESC,folder_id",
        )
        .bind(folder_id)
        .bind(&space_id)
        .fetch_all()
        .await?;
    let ids: Vec<String> = descendants.iter().map(|(id, _)| id.clone()).collect();
    let (has_docs,): (bool,) = tx
        .fixed("SELECT EXISTS(SELECT 1 FROM kb.doc WHERE space_id=$1 AND folder_id=ANY($2::text[]))")
        .bind(&space_id)
        .bind(&ids)
        .fetch_one()
        .await?;
    if has_docs {
        tx.rollback().await?;
        return Err(KbError::BadInput(
            "目录树中还有文档，请先移动或删除文档".into(),
        ));
    }
    for (id, _) in descendants {
        tx.fixed("DELETE FROM kb.folder WHERE folder_id=$1 AND space_id=$2")
            .bind(id)
            .bind(&space_id)
            .execute()
            .await?;
    }
    tx.commit().await?;
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

/// 同名覆盖命中：同空间、同目录下 `name` 精确匹配（`=`，大小写敏感）的文档 id。
/// `folder_id = None` ＝根目录（`IS NOT DISTINCT FROM` 让 NULL ↔ NULL 成立，绑定侧不用分叉）。
/// 历史数据可能多篇同名并存——只取**最近更新**的那篇覆盖，其余原样保留（不替用户删数据）。
/// 内部查找不带 ACL：调用方（ingest 快路径）已过空间写闸，真正的写入各自在写语句内联复核。
pub async fn find_by_name_in_folder(
    store: &OwnedStore,
    space_id: &str,
    folder_id: Option<&str>,
    name: &str,
) -> Result<Option<String>, KbError> {
    Ok(store
        .fixed(
            "SELECT doc_id FROM kb.doc WHERE space_id=$1 AND name=$2 \
             AND folder_id IS NOT DISTINCT FROM $3 ORDER BY updated_at DESC, doc_id LIMIT 1",
        )
        .bind(space_id)
        .bind(name)
        .bind(folder_id)
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
             roles AS (SELECT DISTINCT btrim(code) AS code FROM unnest($2::text[]) AS u(code) \
               WHERE btrim(code)<>''), \
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
    // grantee 先 trim 再判空：空 grantee 落库就是一条永远匹配不上的废授权行
    let grantee = grantee.trim();
    if grantee.is_empty() || !matches!(grantee_kind, "login" | "role") || !matches!(perm, "read" | "write")
    {
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

/// 六份文档写语句的公共 ACL 尾部（owner 或空间 write 授权复核，fail-closed）——
/// 谓词只此一份，各写一遍就是六处漂移面。占位符编号随各语句 SET 参数个数平移，
/// 由宏参数显式给足（判据条件一字不动；锚点测试钉住宏定义与各处调用的编号）。
macro_rules! doc_write_acl_tail {
    ($doc:literal, $login:literal, $roles:literal) => {
        concat!(
            " FROM kb.space s WHERE d.doc_id=", $doc,
            " AND s.space_id=d.space_id AND (s.owner=", $login,
            " OR EXISTS (SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=s.space_id \
               AND a.perm='write' AND ((a.grantee_kind='login' AND a.grantee=", $login, ") OR \
                 (a.grantee_kind='role' AND a.grantee=ANY(", $roles, "::text[])))))"
        )
    };
}

pub async fn set_status(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    st: DocStatus,
    error: &str,
) -> Result<(), KbError> {
    let n = store
        .fixed(concat!(
            "UPDATE kb.doc d SET status=$1,error=$2,updated_at=now()",
            doc_write_acl_tail!("$3", "$4", "$5")
        ))
        .bind(st.as_str())
        .bind(error)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 不存在或写权限已失效")));
    }
    Ok(())
}

/// 影子重建/覆盖的失败状态与线上版本状态分开：只记录本次失败，
/// 不改 `status/error/notice`，因此旧 chunks 仍可检索且旧版本降级信息不丢。
pub async fn set_last_ingest_error(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    error: &str,
) -> Result<(), KbError> {
    let n = store
        .fixed(concat!(
            "UPDATE kb.doc d SET last_ingest_error=$1,updated_at=now()",
            doc_write_acl_tail!("$2", "$3", "$4")
        ))
        .bind(error)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!(
            "文档 {doc_id} 不存在或写权限已失效"
        )));
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
        .fixed(concat!(
            "UPDATE kb.doc d SET notice=$1,updated_at=now()",
            doc_write_acl_tail!("$2", "$3", "$4")
        ))
        .bind(notice)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 不存在或写权限已失效")));
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
        .fixed(concat!(
            "UPDATE kb.doc d SET enabled=$1,updated_at=now()",
            doc_write_acl_tail!("$2", "$3", "$4")
        ))
        .bind(enabled)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 不存在或写权限已失效")));
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
        .fixed(concat!(
            "UPDATE kb.doc d SET source_uri=$1,updated_at=now()",
            doc_write_acl_tail!("$2", "$3", "$4")
        ))
        .bind(source_uri)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 不存在或写权限已失效")));
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
        .fixed(concat!(
            "UPDATE kb.doc d SET description=$1,updated_at=now()",
            doc_write_acl_tail!("$2", "$3", "$4")
        ))
        .bind(description)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 不存在或写权限已失效")));
    }
    Ok(())
}

/// 单文档显式关联上限（篇）：关联列表是人工维护面，50 已宽裕；超限直接 BadInput
const MAX_RELATED_DOCS: usize = 50;

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
    if ids.len() > MAX_RELATED_DOCS {
        return Err(KbError::BadInput(format!("关联文档最多 {MAX_RELATED_DOCS} 篇")));
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
                    /* (0*applied.link_changes)：引用 applied 是强制它求值——PG 不保证未引用的 \
                       CTE 被执行，去掉它 upsert/删除就可能不落地；乘 0 不影响计数 */ \
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
        -1 => Err(KbError::Forbidden(format!("文档 {doc_id} 不存在或写权限已失效"))),
        -2 => Err(KbError::Forbidden(
            "关联文档无效、不可见、跨空间或未处于有效可检索状态".into(),
        )),
        // 契约只有 1/-1/-2 三个状态；出现别的值是 SQL 被改坏，按内部错误报而不是误报权限
        other => {
            debug_assert!(false, "update_doc_metadata_and_links 返回了契约外状态 {other}");
            Err(KbError::Db(format!("文档元数据更新返回了意外状态 {other}")))
        }
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
        return Err(KbError::Forbidden(format!("文档 {doc_id} 不存在或写权限已失效")));
    }
    Ok(())
}

/// notice 累加上限（字符）：反复重建失败 notice 会无限增长——超限截断，保留最新尾部
const NOTICE_MAX_CHARS: usize = 2000;

pub async fn append_notice(
    store: &OwnedStore,
    viewer: &crate::Viewer,
    doc_id: &str,
    notice: &str,
) -> Result<(), KbError> {
    if notice.trim().is_empty() {
        return Ok(());
    }
    // 长度上限只能是 SQL 字面量（`fixed()` 不吃 bind 进 DDL 位置的值），
    // debug 构建下断言它与 NOTICE_MAX_CHARS 同值（防漂移）
    const SQL: &str = concat!(
        "UPDATE kb.doc d SET notice=right(concat_ws('；',NULLIF(d.notice,''),$1),2000),updated_at=now() \
         FROM kb.space s WHERE d.doc_id=$2 AND s.space_id=d.space_id AND (s.owner=$3 OR EXISTS ( \
           SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=s.space_id \
             AND a.perm='write' AND ((a.grantee_kind='login' AND a.grantee=$3) OR \
               (a.grantee_kind='role' AND a.grantee=ANY($4::text[])))))"
    );
    debug_assert!(
        SQL.contains(&NOTICE_MAX_CHARS.to_string()),
        "append_notice 的截断字面量须与 NOTICE_MAX_CHARS 同值"
    );
    let n = store
        .fixed(SQL)
        .bind(notice)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 不存在或写权限已失效")));
    }
    Ok(())
}

// ==================== 分块收口（KB 审查②）：标题-only / 过短块并入下一正文块 ====================

/// 「标题-only / 过短」块的字符下限：正文不足 50 字（实测一轮 KB 审查 74 块里 25 块只有标题
/// 没正文）或正文==叶子标题的块不独立成块。标题信号不丢：embedding_text 的「章节」行本就带
/// 完整 heading_path（`chunk_embedding_text` 配方 v1）。
const TITLE_ONLY_MAX_CHARS: usize = 50;
/// 合并后块的字符合顶：bge 512 token 窗口 × 1.6 字符/token（embed_service.py `MAX_TOKENS` 口径）。
/// 超过就不并 —— 并出一个超窗块会被 fastembed 静默截断，比标题块单独留着更坏。
const MERGED_MAX_CHARS: usize = 480 * 8 / 5; // 768

/// 与 `ingest::est_tokens` / `embed_service.py::est_tokens` 同口径：ceil(chars/1.6) 的整数写法。
/// 实现在 `ingest`（全 crate 一份，这里不再养第二份）。
use crate::ingest::est_tokens;

fn leaf_heading(heading_path: &str) -> &str {
    // `rsplit` 对任意输入恒产一项（空串也是），`next()` 没有不可达兜底
    heading_path.rsplit(" > ").next().expect("rsplit 恒产一项").trim()
}

/// 标题-only 判定：正文==叶子标题（标题块自成一个分块组时的形态），或正文不足 50 字。
fn is_title_only(c: &Chunk) -> bool {
    let t = c.text.trim();
    let leaf = leaf_heading(&c.heading_path);
    (!leaf.is_empty() && t == leaf) || t.chars().count() < TITLE_ONLY_MAX_CHARS
}

/// 收口合并的产物：`sources[i]` = 输出第 i 块由哪些输入块（下标，保序）合并而成 ——
/// 影子构建的向量重挂靠它区分「没动过的块」与「合并块」。
struct MergedChunks {
    chunks: Vec<Chunk>,
    spans: Vec<Option<CharSpan>>,
    sources: Vec<Vec<usize>>,
}

/// 与 `ingest::one_page` 同一条纪律：贡献页集合去重后只剩一个真实页才显示；跨页宁可 None。
/// 实现在 `ingest`（全 crate 一份）。
use crate::ingest::one_page as merged_page;

/// 把 srcs 指向的输入块按序拼成一块（文本以 `\n` 连接）：span 取全体联集（任一缺失即 None，
/// 错位的偏移比没有更糟 —— CharSpan 契约），page 取贡献页唯一值，tokens 按合并后文本重估，
/// heading_path 取最后一个**正文块**的（尾随标题并入时不许反过来盖住正文的章节归属）。
fn combine_chunks(chunks: &[Chunk], spans: &[Option<CharSpan>], srcs: &[usize]) -> (Chunk, Option<CharSpan>) {
    let text = srcs
        .iter()
        .map(|&i| chunks[i].text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    // `collect::<Option<Vec<_>>>` 一趟完成「全 Some 才联集」（原 all(is_some)+unwrap 两段式）
    let span = srcs.iter().map(|&i| spans[i]).collect::<Option<Vec<_>>>().map(|sp| {
        let mut it = sp.into_iter();
        let first = it.next().expect("srcs 非空");
        it.fold(first, |a, b| CharSpan { start: a.start.min(b.start), end: a.end.max(b.end) })
    });
    let heading_path = srcs
        .iter()
        .rev()
        .find(|&&i| !is_title_only(&chunks[i]))
        .map(|&i| chunks[i].heading_path.clone())
        .unwrap_or_else(|| chunks[*srcs.last().expect("srcs 非空")].heading_path.clone());
    let page = merged_page(srcs.iter().map(|&i| chunks[i].page));
    let tokens = est_tokens(text.chars().count());
    (Chunk { text, heading_path, page, tokens }, span)
}

/// 分块收口：「text==叶子标题 或不足 50 字」的块并入**下一正文块**（接在它正文前面）；
/// 尾随无处可并的并回上一块；全是标题块的退化文档原样保留（并掉就没有正文了）。
/// 并块以不破 `MERGED_MAX_CHARS` 为限，装不下的块原样落下 —— 合并不是丢弃的理由。
/// 两条入库写路径（`insert_chunks` / `replace_chunks`）都从这里过，各 preset 分块链统一受益。
fn merge_title_only_chunks(chunks: &[Chunk], spans: &[Option<CharSpan>]) -> MergedChunks {
    debug_assert_eq!(chunks.len(), spans.len());
    // 各块「trim 后字符数」预算环外一次算好：主循环与尾随合并都要用，不逐块重扫
    let char_lens: Vec<usize> = chunks.iter().map(|c| c.text.trim().chars().count()).collect();
    let mut out = MergedChunks {
        chunks: Vec::with_capacity(chunks.len()),
        spans: Vec::with_capacity(chunks.len()),
        sources: Vec::with_capacity(chunks.len()),
    };
    let push = |out: &mut MergedChunks, srcs: &[usize]| {
        let (c, sp) = combine_chunks(chunks, spans, srcs);
        out.chunks.push(c);
        out.spans.push(sp);
        out.sources.push(srcs.to_vec());
    };
    let mut pend: Vec<usize> = Vec::new(); // 待并入的标题-only 块（保持原序）
    for (i, c) in chunks.iter().enumerate() {
        if is_title_only(c) {
            pend.push(i);
            continue;
        }
        // 正文块：能装下的积压标题块并到它前面；装不下的原样先落（不留空窗）
        let mut budget = MERGED_MAX_CHARS.saturating_sub(char_lens[i]);
        let mut srcs: Vec<usize> = Vec::with_capacity(pend.len() + 1);
        for &p in &pend {
            let n = char_lens[p] + 1; // +1 = 拼接的 \n
            if n <= budget {
                srcs.push(p);
                budget -= n;
            } else {
                push(&mut out, &[p]);
            }
        }
        srcs.push(i);
        push(&mut out, &srcs);
        pend.clear();
    }
    // 尾随标题块：并回上一块（装不下则原样落）
    if !pend.is_empty() {
        let mut absorbed = 0usize;
        if let Some(last) = out.chunks.last() {
            let mut budget = MERGED_MAX_CHARS.saturating_sub(last.text.chars().count());
            let mut srcs = out.sources.last().expect("chunks 与 sources 平行").clone();
            for &p in &pend {
                let n = char_lens[p] + 1;
                if n > budget {
                    break;
                }
                srcs.push(p);
                budget -= n;
                absorbed += 1;
            }
            if absorbed > 0 {
                out.chunks.pop();
                out.spans.pop();
                out.sources.pop();
                push(&mut out, &srcs);
            }
        }
        for &p in &pend[absorbed..] {
            push(&mut out, &[p]);
        }
    }
    out
}

/// 影子构建的向量随合并重挂：合并块的预计算向量是按旧文本算的，贴到新文本上是错向量 ——
/// expected 置空串（恒不等于 SQL 侧重算的 `kb.chunk_embedding_text`）→ 该块落 NULL →
/// 文档按 `missing` 停 `chunked`，由 A9/embed_fill（或 `embed_service.py revec`）补算；
/// 单源块（没动过）的向量原样保留。
/// 返回借用视图：单源块不再整串 clone embedding_text/向量字面量（两者都可能很大）。
fn remap_shadow_embeddings<'a>(
    sources: &[Vec<usize>],
    embedding_texts: &'a [String],
    embeddings: &'a [Option<String>],
) -> (Vec<&'a str>, Vec<Option<&'a str>>) {
    sources
        .iter()
        .map(|s| {
            if s.len() == 1 {
                (embedding_texts[s[0]].as_str(), embeddings[s[0]].as_deref())
            } else {
                ("", None) // 空串哨兵（CAS 恒失配 → 落 NULL 走补算）
            }
        })
        .collect()
}

/// 同名覆盖切换时随索引**同一条语句**替换的文件元数据（`None` ＝ 纯重建，三列原样保留）。
/// 必须与分块切换同语句：拆成两条会在「sha 已新、块仍旧」的两半状态里毒化秒传去重
/// （同内容重传被 `find_by_sha` 命中弹回，旧块永无自愈）。
pub struct DocFileMeta<'a> {
    pub sha256: &'a str,
    pub bytes: i64,
    pub mime: &'a str,
}

/// 重处理前清空旧块与状态。数据修改 CTE 保证两步同一条语句完成，避免半清理。
/// 影子解析完成后一次性替换正文索引与文档状态。任何错误都会让旧 chunks 原样保留。
/// `spans` 与 `chunks` 等长平行（B3 字符偏移），语义同 `insert_chunks`。
/// `file_meta` 仅同名覆盖链传入：sha256/bytes/mime 随索引同语句切换（`COALESCE($2x,…)` 形，
/// `None` 时三列原样保留，重建链语义不变）。
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
    file_meta: Option<&DocFileMeta<'_>>,
) -> Result<(), KbError> {
    if chunks.is_empty()
        || chunks.len() != embedding_texts.len()
        || chunks.len() != embeddings.len()
        || chunks.len() != spans.len()
    {
        return Err(KbError::BadInput("影子索引的切片与向量数量不一致".into()));
    }
    // 分块收口：标题-only/过短块并入下一正文块（KB 审查②，见 `merge_title_only_chunks`）。
    let merged = merge_title_only_chunks(chunks, spans);
    let (embedding_texts, embeddings) =
        remap_shadow_embeddings(&merged.sources, embedding_texts, embeddings);
    let chunks = &merged.chunks;
    let spans = &merged.spans;
    let embedding_texts = &embedding_texts;
    let embeddings = &embeddings;
    let ord: Vec<i32> = (0..chunks.len() as i32).collect();
    let text: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
    let heading: Vec<&str> = chunks.iter().map(|c| c.heading_path.as_str()).collect();
    let page: Vec<Option<i32>> = chunks.iter().map(|c| c.page).collect();
    let tokens: Vec<i32> = chunks.iter().map(|c| c.tokens).collect();
    let starts: Vec<Option<i32>> = spans.iter().map(|s| s.map(|x| x.start)).collect();
    let ends: Vec<Option<i32>> = spans.iter().map(|s| s.map(|x| x.end)).collect();
    // 词列与正文同一条语句重写（影子索引重建 = 正文变了，词列必须同语句跟着变）
    let term_blobs: Vec<String> = chunks.iter().map(|c| terms_blob(&terms_of(&c.text))).collect();
    let written = store
        .fixed(
            "WITH locked AS (SELECT d.doc_id,d.name,d.folder_path FROM kb.doc d JOIN kb.space s ON s.space_id=d.space_id \
               WHERE d.doc_id=$1 AND (s.owner=$15 OR EXISTS (SELECT 1 FROM kb.acl a \
                  WHERE a.scope='space' AND a.target_id=s.space_id AND a.perm='write' \
                    AND ((a.grantee_kind='login' AND a.grantee=$15) OR \
                         (a.grantee_kind='role' AND a.grantee=ANY($16::text[]))))) FOR UPDATE), \
             upserted AS ( \
               INSERT INTO kb.chunk(doc_id,ord,text,heading_path,folder_path,page,tokens,embedding_text,embedding_recipe,embedding,start_char_pos,end_char_pos,terms) \
               SELECT $1,u.ord,u.txt,u.heading,l.folder_path,u.page,u.tokens, \
                      kb.chunk_embedding_text(l.name,l.folder_path,u.heading,u.txt),$14, \
                      CASE WHEN u.expected=kb.chunk_embedding_text(l.name,l.folder_path,u.heading,u.txt) \
                           THEN u.embedding::vector ELSE NULL END,u.cstart,u.cend, \
                      string_to_array(u.tblob, chr(1)) \
               FROM unnest($2::int[],$3::text[],$4::text[],$5::int[],$6::int[],$7::text[],$8::text[],$17::int[],$18::int[],$19::text[]) \
                    AS u(ord,txt,heading,page,tokens,expected,embedding,cstart,cend,tblob) CROSS JOIN locked l \
               ON CONFLICT (doc_id,ord) DO UPDATE SET text=EXCLUDED.text, \
                 heading_path=EXCLUDED.heading_path,folder_path=EXCLUDED.folder_path, \
                 page=EXCLUDED.page,tokens=EXCLUDED.tokens, \
                 embedding_text=EXCLUDED.embedding_text,embedding_recipe=EXCLUDED.embedding_recipe, \
                 embedding=EXCLUDED.embedding,start_char_pos=EXCLUDED.start_char_pos, \
                 end_char_pos=EXCLUDED.end_char_pos,terms=EXCLUDED.terms RETURNING embedding IS NULL AS missing), trimmed AS ( \
               DELETE FROM kb.chunk WHERE doc_id=$1 AND ord >= $13 \
                 AND EXISTS (SELECT 1 FROM upserted) RETURNING 1) \
              UPDATE kb.doc SET status=CASE WHEN EXISTS(SELECT 1 FROM upserted WHERE missing) \
                                             THEN 'chunked' ELSE $9 END,error=$10,notice=$11,last_ingest_error='',page_count=$12, \
                               sha256=COALESCE($20::text,sha256),bytes=COALESCE($21::bigint,bytes), \
                               mime=COALESCE($22::text,mime), \
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
        .bind(&term_blobs)
        .bind(file_meta.map(|m| m.sha256))
        .bind(file_meta.map(|m| m.bytes))
        .bind(file_meta.map(|m| m.mime))
        .execute()
        .await?;
    if written == 0 {
        // 0 行 = locked 空（DO UPDATE 下全量冲突仍会写）：只剩「文档没了」与「权限没了」两种，
        // 复核后再定性——别把「不存在」报成权限错
        return Err(match get_doc(store, doc_id).await? {
            None => KbError::NotFound(format!("文档 {doc_id} 已不存在")),
            Some(_) => KbError::Forbidden(format!("文档 {doc_id} 的写权限已失效")),
        });
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
        .fixed(concat!(
            "UPDATE kb.doc d SET page_count=$1,chunk_count=$2,updated_at=now()",
            doc_write_acl_tail!("$3", "$4", "$5")
        ))
        .bind(page_count)
        .bind(chunk_count)
        .bind(doc_id)
        .bind(&viewer.login)
        .bind(&viewer.roles)
        .execute()
        .await?;
    if n == 0 {
        return Err(KbError::Forbidden(format!("文档 {doc_id} 不存在或写权限已失效")));
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
    // 分块收口：标题-only/过短块并入下一正文块（KB 审查②，见 `merge_title_only_chunks`）。
    let merged = merge_title_only_chunks(chunks, spans);
    let chunks = &merged.chunks;
    let spans = &merged.spans;
    let ord: Vec<i32> = (0..chunks.len() as i32).collect();
    let text: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
    let heading: Vec<&str> = chunks.iter().map(|c| c.heading_path.as_str()).collect();
    let page: Vec<Option<i32>> = chunks.iter().map(|c| c.page).collect();
    let tokens: Vec<i32> = chunks.iter().map(|c| c.tokens).collect();
    let starts: Vec<Option<i32>> = spans.iter().map(|s| s.map(|x| x.start)).collect();
    let ends: Vec<Option<i32>> = spans.iter().map(|s| s.map(|x| x.end)).collect();
    // 词列与正文同一条语句落库：terms 是 text 的纯函数（`terms_of` 单一事实源），
    // 不许出现「正文已换、词列还是旧文算的」的中间态。
    let term_blobs: Vec<String> = chunks.iter().map(|c| terms_blob(&terms_of(&c.text))).collect();
    let written = store
        .fixed(
            "INSERT INTO kb.chunk(doc_id,ord,text,heading_path,folder_path,page,tokens,embedding_text,embedding_recipe,start_char_pos,end_char_pos,terms) \
             SELECT $1,u.ord,u.txt,u.heading,d.folder_path,u.page,u.tokens, \
                    kb.chunk_embedding_text(d.name,d.folder_path,u.heading,u.txt),$7,u.cstart,u.cend, \
                    string_to_array(u.tblob, chr(1)) \
             FROM unnest($2::int[], $3::text[], $4::text[], $5::int[], $6::int[], $10::int[], $11::int[], $12::text[]) \
                  AS u(ord,txt,heading,page,tokens,cstart,cend,tblob) CROSS JOIN kb.doc d \
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
        .bind(&term_blobs)
        .execute()
        .await?;
    if written == 0 {
        // 0 行有两种成因：权限没了/文档没了，或并发下全量 ON CONFLICT 冲突的合法重跑。
        // 先复核文档在不在再定性——别把「不存在」报成权限错（全冲突仍归权限侧，是已知近似：
        // 放行 Ok(0) 会让上游把 chunk_count 写成 0，那是更坏的误报）
        return Err(match get_doc(store, doc_id).await? {
            None => KbError::NotFound(format!("文档 {doc_id} 已不存在")),
            Some(_) => KbError::Forbidden(format!("文档 {doc_id} 的写权限已失效")),
        });
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
/// 返回实际写入行数：0 行 = CAS 全失配（并发重建把文本/配方改了），调用方应留一声 warn。
pub async fn set_embeddings(
    store: &OwnedStore,
    doc_id: &str,
    rows: &[(ChunkEmbeddingJob, String)],
    viewer: &crate::Viewer,
) -> Result<u64, KbError> {
    if rows.is_empty() {
        return Ok(0);
    }
    let ids: Vec<i64> = rows.iter().map(|(j, _)| j.chunk_id).collect();
    let texts: Vec<&str> = rows.iter().map(|(j, _)| j.text.as_str()).collect();
    let recipes: Vec<i16> = rows.iter().map(|(j, _)| j.recipe).collect();
    let lits: Vec<&str> = rows.iter().map(|(_, lit)| lit.as_str()).collect();
    let n = store
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
    Ok(n)
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

/// 【词级路回填】待补词列的块。`terms IS NULL` = 还没过分词器（待回填）；
/// `{}` = 分过了但没留下词（纯标点块等）—— 两者必须可分，否则回填游标无处可钉。
/// 不按 doc 状态过滤：terms 不进状态机，检索侧的 doc 谓词（enabled/status/有效期）
/// 已经把不可见块挡在召回外，这里多抄一份谓词只会跟着漂。
pub async fn null_terms_chunks(store: &OwnedStore, limit: i64) -> Result<Vec<(i64, String)>, KbError> {
    Ok(store
        .fixed("SELECT chunk_id,text FROM kb.chunk WHERE terms IS NULL ORDER BY chunk_id LIMIT $1")
        .bind(limit)
        .fetch_all::<(i64, String)>()
        .await?)
}

/// 词列回填写回：`chunk_id + 正文 CAS + terms IS NULL` 三重收口。并发重建（影子索引）
/// 把正文改写过时 CAS 失配 = 0 行，下轮按新正文重算 —— 与 `set_chunk_embedding` 同一条纪律，
/// 旧任务绝不能把旧正文的词覆盖到新块上。一条 UNNEST 语句写一批，不做 N 次往返。
pub async fn set_chunk_terms(store: &OwnedStore, rows: &[(i64, String, String)]) -> Result<u64, KbError> {
    if rows.is_empty() {
        return Ok(0);
    }
    let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let blobs: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();
    let texts: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
    let n = store
        .fixed(
            "UPDATE kb.chunk c SET terms=string_to_array(v.blob, chr(1)) \
             FROM unnest($1::bigint[],$2::text[],$3::text[]) AS v(id,blob,txt) \
             WHERE c.chunk_id=v.id AND c.text=v.txt AND c.terms IS NULL",
        )
        .bind(&ids)
        .bind(&blobs)
        .bind(&texts)
        .execute()
        .await?;
    Ok(n)
}

/// 词级路存量回填的调度参数（对齐 `server::embed_fill` 的模式：启动即跑一轮 + 周期补漏）。
/// 与向量回填的不对称：terms 是 `text` 的**纯函数**、不依赖向量/解析服务，读库内现有
/// text 直接重算 —— 存量**不许要求重传**。
const TERMS_FILL_BATCH: i64 = 500;
/// advisory lock 键（多实例只跑一个；与 embed_fill 的 7_720_031 不撞即可）
const TERMS_FILL_LOCK: i64 = 7_720_057;
/// 两轮间隔：收敛后每轮只是一句「还有没有 NULL」的计数，成本可忽略
const TERMS_FILL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(600);

/// 启动任务挂点（`migrate` 末尾调用）。幂等：进程半途死了，下次启动按 `terms IS NULL`
/// 续跑；一次性 CLI（exec-sql 之类）进程随即退出、任务跟着死，也无碍 —— 服务启动会补完。
pub fn spawn_terms_backfill(store: OwnedStore) {
    tokio::spawn(async move {
        loop {
            match terms_backfill_round(&store).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("词级路回填：本轮补回 {n} 块"),
                Err(e) => tracing::warn!("词级路回填本轮失败（下轮重试）: {e:#}"),
            }
            tokio::time::sleep(TERMS_FILL_INTERVAL).await;
        }
    });
}

async fn terms_backfill_round(store: &OwnedStore) -> Result<u64, KbError> {
    // guard 在 connector 内绑定锁与解锁的物理连接；任务取消/崩溃也会关闭专用会话释放锁。
    let Some(lock) = store.try_advisory_lock(TERMS_FILL_LOCK).await? else {
        tracing::debug!("词级路回填：advisory 锁由其他实例持有，本轮跳过");
        return Ok(0);
    };
    let r = terms_backfill_all(store).await;
    if let Err(e) = lock.release().await {
        tracing::warn!("词级路回填：advisory 解锁失败，专用连接将关闭: {e:#}");
    }
    r
}

async fn terms_backfill_all(store: &OwnedStore) -> Result<u64, KbError> {
    let mut filled = 0u64;
    loop {
        let rows = null_terms_chunks(store, TERMS_FILL_BATCH).await?;
        if rows.is_empty() {
            return Ok(filled);
        }
        // 分词是 CPU 活（一批 ≈ 几十万字符内）：在 web worker 上连续算会卡住请求线程
        let jobs: Vec<(i64, String, String)> = rows
            .into_iter()
            .map(|(id, text)| {
                let blob = terms_blob(&terms_of(&text));
                (id, blob, text)
            })
            .collect();
        let n = set_chunk_terms(store, &jobs).await?;
        if n == 0 {
            // 整批 CAS 失配 = 并发重建正在重写同一批行；原地空转只会互踩，本轮收工，
            // 下一轮（或重建完成后的轮次）按新正文重算
            tracing::warn!("词级路回填：整批 CAS 失配（疑似并发重建），本轮提前收工");
            return Ok(filled);
        }
        filled += n;
    }
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

/// 【启动自愈】卡死文档扫描行：重跑入库重活所需的最小上下文。
/// `owner` 是恢复动作的执行身份——系统任务没有会话 Viewer，空间 owner 恒过写门禁（不开旁路）。
#[derive(Debug, Clone)]
pub struct StuckDoc {
    pub doc_id: String,
    pub space_id: String,
    pub folder_id: Option<String>,
    pub name: String,
    pub mime: String,
    pub owner: String,
    /// 是否已有分块（`insert_chunks` 单语句落库，块全在或全不在）：决定自愈走首入链
    /// （无块，零冲突）还是影子重建链（有块，首入链的全量冲突会报 0 行，不许重入）。
    pub has_chunks: bool,
}

/// 进程死亡留下的「进行中」文档：status ∈ pending/parsing/chunked 且 `stale_mins` 分钟没动过。
/// `pending` 也在扫描集里：它只该存在于「建行 → 推进 parsing」的瞬态窗口，超龄即僵尸。
/// `chunked` 含「向量服务不可用」的刻意降级：重跑幂等（顺带补欠下的向量），与 A9 同向。
pub async fn stuck_docs(
    store: &OwnedStore,
    stale_mins: i32,
    limit: i64,
) -> Result<Vec<StuckDoc>, KbError> {
    Ok(store
        .fixed(
            "SELECT d.doc_id,d.space_id,d.folder_id,d.name,d.mime,s.owner,\
                    EXISTS(SELECT 1 FROM kb.chunk c WHERE c.doc_id=d.doc_id) \
             FROM kb.doc d JOIN kb.space s ON s.space_id=d.space_id \
             WHERE d.status IN ('pending','parsing','chunked') \
               AND d.updated_at < now() - make_interval(mins => $1) \
             ORDER BY d.updated_at LIMIT $2",
        )
        .bind(stale_mins)
        .bind(limit)
        .fetch_all::<(String, String, Option<String>, String, String, String, bool)>()
        .await?
        .into_iter()
        .map(|(doc_id, space_id, folder_id, name, mime, owner, has_chunks)| StuckDoc {
            doc_id, space_id, folder_id, name, mime, owner, has_chunks,
        })
        .collect())
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
             ORDER BY created_at DESC, doc_id"
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

    /// 角色码/授权对象的空串防御：btrim 后为空不得落成 `grantee=''` 的废授权行
    #[test]
    fn blank_grantees_are_filtered_or_rejected() {
        let src = include_str!("store.rs");
        let roles = src.split("pub async fn grant_space_roles").nth(1).unwrap();
        let roles = roles.split("pub async fn ").next().unwrap();
        assert!(roles.contains("WHERE btrim(code)<>''"), "角色 CTE 必须滤掉空白角色码");
        let acl = src.split("pub async fn grant_space_acl").nth(1).unwrap();
        let acl = acl.split("pub async fn ").next().unwrap();
        assert!(acl.contains("grantee.trim()") && acl.contains("grantee.is_empty()"),
                "grantee 必须 trim 并拒空");
    }

    /// list_docs 的稳定次序：created_at 同秒时按 doc_id 决胜（与 list_docs_page 同口径）
    #[test]
    fn list_docs_has_a_deterministic_tiebreak() {
        let src = include_str!("store.rs");
        let body = src.split("pub async fn list_docs").nth(1).unwrap();
        let body = body.split("pub async fn ").next().unwrap();
        assert!(body.contains("ORDER BY created_at DESC, doc_id"), "list_docs 缺决胜键");
    }

    /// append_notice 的长度上限：right(...) 截断保留最新尾部；字面量与常量同值
    #[test]
    fn append_notice_is_capped_at_the_constant() {
        let src = include_str!("store.rs");
        let body = src.split("pub async fn append_notice").nth(1).unwrap();
        let body = body.split("pub async fn ").next().unwrap();
        assert!(body.contains("right(concat_ws("), "notice 累加必须带截断");
        assert!(body.contains(&NOTICE_MAX_CHARS.to_string()), "SQL 字面量须与常量同值");
        assert_eq!(NOTICE_MAX_CHARS, 2000);
    }

    #[test]
    fn doc_cols_match_row_fields() {
        // 列清单与 DocRow 字段一一对应（FromRow 靠名字取列，漏一列是运行时错，钉在这里）
        assert_eq!(DOC_COLS.split(',').count(), 26);
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
            "last_ingest_error",
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
            // 写复核谓词收在 `doc_write_acl_tail!` 宏里（定义处由钉扎测试守着）；
            // 这里钉调用点的占位符编号——$1 是字段值，$2/$3/$4 = doc_id/login/roles
            assert!(body.contains("doc_write_acl_tail!(\"$2\", \"$3\", \"$4\")"), "{f} 写复核调用变了");
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

    /// 启动自愈扫描（锚点）：进行态三状态 + 超龄判定 + owner 身份 + 分块存在性一个都不能少——
    /// 漏掉 owner join 自愈就没有合法执行身份，漏掉 has_chunks 就无法分派首入/重建两条链。
    #[test]
    fn stuck_docs_scan_pins_statuses_staleness_and_identity() {
        let src = include_str!("store.rs");
        let body = src.split("pub async fn stuck_docs").nth(1).unwrap();
        let body = body.split("pub async fn ").next().unwrap();
        assert!(body.contains("'pending','parsing','chunked'"), "扫描集必须是三个进行态: {body}");
        assert!(body.contains("make_interval(mins => $1)"), "超龄窗口必须参数化: {body}");
        assert!(body.contains("JOIN kb.space s ON s.space_id=d.space_id"), "必须带出空间 owner: {body}");
        assert!(body.contains("EXISTS(SELECT 1 FROM kb.chunk c WHERE c.doc_id=d.doc_id)"), "必须带出分块存在性: {body}");
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
        assert!(body.contains(
            "UPDATE kb.doc SET status=CASE WHEN EXISTS(SELECT 1 FROM upserted WHERE missing)"
        ));
        assert!(
            body.contains("last_ingest_error=''"),
            "影子切换成功必须清除上次失败"
        );
    }

    /// 同名覆盖的文件元数据（sha256/bytes/mime）必须与分块**同一条语句**切换——拆两条语句
    /// 会在「sha 已新、块仍旧」的两半状态里毒化秒传去重。`COALESCE($2x,列)` 形让重建链
    /// （`file_meta = None`）三列原样保留。既有 bind 编号（$15..$19）不许漂。
    #[test]
    fn shadow_replace_switches_file_meta_in_the_same_statement() {
        let src = include_str!("store.rs");
        let body = src.split("pub async fn replace_chunks").nth(1).unwrap();
        let body = body.split("pub async fn ").next().unwrap();
        assert!(body.contains("file_meta: Option<&DocFileMeta"), "replace_chunks 必须接收可选文件元数据: {body}");
        assert!(body.contains("sha256=COALESCE($20::text,sha256)"), "sha256 同语句 COALESCE 切换: {body}");
        assert!(body.contains("bytes=COALESCE($21::bigint,bytes)"), "bytes 同语句 COALESCE 切换: {body}");
        assert!(body.contains("mime=COALESCE($22::text,mime)"), "mime 同语句 COALESCE 切换: {body}");
        assert!(body.contains("s.owner=$15"), "既有写复核 bind 编号不许漂: {body}");
        assert!(body.contains("$19::text[]"), "既有词列 bind 编号不许漂: {body}");
    }

    /// 同名覆盖命中的 SQL 合同（连库前的源码钉住）：同空间 + `name` 精确匹配 +
    /// 目录限定（`folder_id` NULL 根目录经 `IS NOT DISTINCT FROM` 命中）+
    /// 历史多同名只取最近更新的一篇（其余原样保留，不替用户删数据）。
    #[test]
    fn find_by_name_in_folder_sql_contract() {
        let src = include_str!("store.rs");
        let body = src.split("pub async fn find_by_name_in_folder").nth(1).unwrap();
        let body = body.split("pub enum ").next().unwrap();
        assert!(body.contains("space_id=$1 AND name=$2"), "必须同空间 + name 精确匹配: {body}");
        assert!(body.contains("folder_id IS NOT DISTINCT FROM $3"), "目录限定必须覆盖根目录 NULL: {body}");
        assert!(body.contains("ORDER BY updated_at DESC"), "多同名必须取最近更新: {body}");
        assert!(body.contains("LIMIT 1"), "只取一篇覆盖，其余不动: {body}");
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
    fn delete_folder_accepts_only_document_free_subtrees() {
        let src = include_str!("store.rs");
        let body = src.split("pub async fn delete_folder").nth(1).unwrap();
        let body = body.split("pub async fn ").next().unwrap();
        assert!(body.contains("pg_advisory_xact_lock(hashtextextended(space_id,0))"));
        assert!(body.contains("folder_id=ANY($2::text[])"));
        assert!(body.contains("ORDER BY depth DESC"));
        assert!(body.contains("tx.commit()"));
        assert!(body.contains("store.begin_fixed()"), "事务必须经 OwnedStore 固定 SQL 边界");
        assert!(!body.contains(concat!("sqlx", "::", "query")), "knowledge 不得重新拿裸数据库语句");
        assert!(!body.contains("d.folder_id<>t.folder_id"));
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
        // 宏化六份：谓词在宏定义里（下方单独钉定义），这里钉调用点的占位符编号
        for (name, tail) in [
            ("set_status", "doc_write_acl_tail!(\"$3\", \"$4\", \"$5\")"),
            (
                "set_last_ingest_error",
                "doc_write_acl_tail!(\"$2\", \"$3\", \"$4\")",
            ),
            ("set_notice", "doc_write_acl_tail!(\"$2\", \"$3\", \"$4\")"),
            ("set_enabled", "doc_write_acl_tail!(\"$2\", \"$3\", \"$4\")"),
            ("set_doc_source_uri", "doc_write_acl_tail!(\"$2\", \"$3\", \"$4\")"),
            ("set_doc_description", "doc_write_acl_tail!(\"$2\", \"$3\", \"$4\")"),
            ("set_counts", "doc_write_acl_tail!(\"$3\", \"$4\", \"$5\")"),
        ] {
            let marker = format!("pub async fn {name}");
            let body = src.split(marker.as_str()).nth(1).unwrap();
            let body = body.split("pub async fn ").next().unwrap();
            assert!(body.contains(tail), "{name} 的写复核调用变了（编号漂移 = 绑错参数）");
            assert!(body.contains("viewer"), "{name} 未接收当前操作者");
        }
        // 宏定义本体：谓词只能有这一份
        let mac = src.split("macro_rules! doc_write_acl_tail").nth(1).unwrap();
        assert!(mac.contains("a.perm='write'"), "写复核宏丢了 perm 谓词");
        assert!(mac.contains("grantee_kind='role'"), "写复核宏丢了角色授权");
        assert!(mac.contains("s.owner="), "写复核宏丢了 owner 放行");
        // 形状各异的其余写函数：谓词仍内联在各自语句里
        for name in [
            "update_doc_metadata_and_links",
            "apply_inferred_doc_version",
            "append_notice",
            "delete_doc",
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
        // 条数写死是「不许悄悄多一条」的钉。2026-08-17 从 4 → 5：第五条是向量维度的幂等改型。
        assert_eq!(stmts.len(), 5, "KB_DDL_DELTA 条数变了，确认是有意加的再改这个数");
        assert!(stmts[..2]
            .iter()
            .all(|s| s.starts_with("ALTER TABLE kb.chunk ADD COLUMN IF NOT EXISTS")));
        assert!(KB_DDL_DELTA.contains("start_char_pos int"));
        assert!(KB_DDL_DELTA.contains("end_char_pos int"));
        // Y7：第三条是 doc.description（AI 描述列），同样幂等
        assert!(stmts[2].starts_with(
            "ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS description text NOT NULL DEFAULT ''"
        ));
        // 影子入库失败不能改线上 status：独立字段保留旧版可检索性。
        assert!(stmts[3].starts_with("ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS last_ingest_error text NOT NULL DEFAULT ''"));
        // 第五条：向量维度幂等改型。三件事缺一不可 —— 读 atttypmod 判当前维度（不是无条件 ALTER，
        // 那会每次启动重建一次索引）、改型时清 NULL（不同维度的向量不可比）、
        // 把受影响文档退回 chunked（否则 revec 的 KB_SEL 只扫 chunked，会扫到 0 行还退 0）。
        let retype = stmts[4];
        assert!(retype.contains("atttypmod"), "改型必须先读当前维度：{retype}");
        assert!(retype.contains("ALTER COLUMN embedding TYPE vector(1024) USING NULL"), "{retype}");
        assert!(retype.contains("UPDATE kb.doc SET status = 'chunked'"), "改型必须把文档退回 chunked：{retype}");
        let src = include_str!("store.rs");
        let migrate = src.split("pub async fn migrate").nth(1).unwrap();
        let migrate = migrate.split("pub async fn ").next().unwrap();
        // 两份 DDL 都必须挂在 migrate 执行链上，且迁移有并发顾问锁与 $tag$ 防御
        assert!(migrate.contains("[KB_DDL, KB_DDL_DELTA]"), "migrate 必须覆盖两份 DDL");
        assert!(migrate.contains("pg_advisory_xact_lock"), "migrate 丢了并发互斥锁");
        assert!(migrate.contains("has_tagged_dollar_quote"), "migrate 丢了 $tag$ 防御");
    }

    /// 切分器边界：裸 `$$` 不触发防御，`$func$`/`$body$` 必须被识别（含 `$1` 参数不误报）
    #[test]
    fn tagged_dollar_quotes_are_detected() {
        assert!(!has_tagged_dollar_quote("DO $$ BEGIN RAISE NOTICE; END $$;"));
        assert!(!has_tagged_dollar_quote("SELECT $1, $2"));
        assert!(has_tagged_dollar_quote("CREATE FUNCTION f() RETURNS void AS $func$ ... $func$"));
        assert!(has_tagged_dollar_quote("AS $_$ SELECT 1 $_$"));
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

    // ==================== KB 审查②：标题-only / 过短块的收口合并 ====================

    fn plain_chunk(text: &str, heading: &str, page: Option<i32>) -> Chunk {
        Chunk {
            text: text.into(),
            heading_path: heading.into(),
            page,
            tokens: est_tokens(text.chars().count()),
        }
    }

    /// 正文块（≥50 字、不等于叶子标题）的便捷构造
    fn content_chunk(text: &str, heading: &str) -> Chunk {
        let c = plain_chunk(text, heading, None);
        debug_assert!(!is_title_only(&c), "夹具必须是正文块：{text}");
        c
    }

    /// 标题块并入**下一**正文块：文本接在正文前、章节归属取正文块、span 取联集。
    #[test]
    fn title_only_chunk_merges_into_the_next_content_chunk() {
        let chunks = vec![
            plain_chunk("第三章 报销流程", "制度 > 第三章 报销流程", None),
            content_chunk(&"发".repeat(120), "制度 > 第三章 报销流程 > 3.1 适用范围"),
            content_chunk(&"票".repeat(120), "制度 > 第三章 报销流程 > 3.1 适用范围"),
        ];
        let spans = vec![Some(CharSpan { start: 10, end: 17 }), Some(CharSpan { start: 18, end: 138 }), None];
        let m = merge_title_only_chunks(&chunks, &spans);
        assert_eq!(m.chunks.len(), 2);
        assert_eq!(m.chunks[0].text, format!("第三章 报销流程\n{}", "发".repeat(120)));
        assert_eq!(m.chunks[0].heading_path, "制度 > 第三章 报销流程 > 3.1 适用范围");
        assert_eq!(m.chunks[0].tokens, est_tokens(m.chunks[0].text.chars().count()));
        assert_eq!(m.spans[0], Some(CharSpan { start: 10, end: 138 }), "span 取联集");
        assert_eq!(m.spans[1], None, "任一输入缺偏移即 None（错位的偏移比没有更糟）");
        assert_eq!(m.sources, vec![vec![0, 1], vec![2]]);
    }

    /// 「不足 50 字」同样并入；50 字整是正文块；页码取贡献页唯一值（跨页 None）。
    #[test]
    fn short_chunk_merges_and_page_keeps_the_only_real_page() {
        let chunks = vec![
            plain_chunk(&"短".repeat(49), "H", None),
            plain_chunk(&"正".repeat(60), "H", Some(3)),
            plain_chunk(&"足".repeat(50), "H", Some(3)),
        ];
        let spans = vec![None, None, None];
        let m = merge_title_only_chunks(&chunks, &spans);
        assert_eq!(m.chunks.len(), 2, "49 字并入下一块，50 字整独立成块");
        assert!(m.chunks[0].text.starts_with(&"短".repeat(49)));
        assert_eq!(m.chunks[0].page, Some(3));
        assert_eq!(m.chunks[1].text, "足".repeat(50));
        // 跨页：并进来的块与本块页不同 → None（「不知道」比「说错」好）
        let chunks = vec![
            plain_chunk("小标题", "H > 小标题", Some(2)),
            plain_chunk(&"正".repeat(60), "H > 小标题", Some(3)),
        ];
        let m = merge_title_only_chunks(&chunks, &[None, None]);
        assert_eq!(m.chunks[0].page, None);
    }

    /// 尾随标题块并回上一块；全是标题块的退化文档原样保留（并掉就没有正文了）。
    #[test]
    fn trailing_titles_merge_backwards_and_all_titles_are_kept() {
        let chunks = vec![
            content_chunk(&"正".repeat(100), "H > 一"),
            plain_chunk("附录", "H > 附录", None),
        ];
        let m = merge_title_only_chunks(&chunks, &[None, None]);
        assert_eq!(m.chunks.len(), 1);
        assert_eq!(m.chunks[0].text, format!("{}\n附录", "正".repeat(100)));
        assert_eq!(m.chunks[0].heading_path, "H > 一", "章节归属不许被尾随标题盖住");

        let all_titles = vec![
            plain_chunk("第一章", "第一章", None),
            plain_chunk("第二章", "第二章", None),
        ];
        let m = merge_title_only_chunks(&all_titles, &[None, None]);
        assert_eq!(m.chunks.len(), 2, "全是标题块时一个都不许丢");
    }

    /// 合并以不破 512 token 窗口（768 字符）为限：装不下的标题块原样落下，不硬并。
    #[test]
    fn merge_never_exceeds_the_embedding_window() {
        let chunks = vec![
            plain_chunk("小标题", "H > 小标题", None),
            plain_chunk(&"满".repeat(MERGED_MAX_CHARS), "H > 正文", None),
        ];
        let m = merge_title_only_chunks(&chunks, &[None, None]);
        assert_eq!(m.chunks.len(), 2, "并了就会超窗 → 不并");
        assert_eq!(m.chunks[0].text, "小标题");
        let chunks = vec![
            plain_chunk("小标题", "H > 小标题", None),
            plain_chunk(&"余".repeat(MERGED_MAX_CHARS - 20), "H > 正文", None),
        ];
        let m = merge_title_only_chunks(&chunks, &[None, None]);
        assert_eq!(m.chunks.len(), 1, "装得下就并（6 + 1 + 748 ≤ 768）");
    }

    /// 影子构建的向量重挂：单源块保留原向量，合并块落 (空串哨兵, None) 走补算。
    #[test]
    fn shadow_embeddings_remap_invalidates_only_merged_chunks() {
        let sources = vec![vec![0usize, 1], vec![2]];
        let texts = vec!["t0".to_string(), "t1".to_string(), "t2".to_string()];
        let vecs = vec![Some("v0".to_string()), Some("v1".to_string()), None];
        let (t, v) = remap_shadow_embeddings(&sources, &texts, &vecs);
        assert_eq!(t, vec!["", "t2"], "合并块的 expected 置空串（CAS 恒失配 → NULL）");
        assert_eq!(v, vec![None, None], "合并块不许贴旧向量；单源块原样（含本就 None 的）");
    }

    /// 两条入库写路径都必须过收口（结构性锁：绕过收口 = 标题-only 块回流）。
    #[test]
    fn both_chunk_write_paths_pass_through_the_merge() {
        let src = include_str!("store.rs");
        for f in ["pub async fn insert_chunks", "pub async fn replace_chunks"] {
            let body = src.split(f).nth(1).unwrap();
            let body = body.split("pub async fn ").next().unwrap();
            assert!(body.contains("merge_title_only_chunks"), "{f} 没过标题-only 收口");
        }
        let replace = src.split("pub async fn replace_chunks").nth(1).unwrap();
        let replace = replace.split("pub async fn ").next().unwrap();
        assert!(replace.contains("remap_shadow_embeddings"), "影子构建不许把旧文本的向量贴到合并块上");
    }

    // ==================== 词级稀疏召回（第 9 路）：分词 / 落库 / 回填 ====================

    /// 分词口径钉样例：这些断言是**行为合同**，改 `terms_of` 的过滤规则前先想清召回影响。
    /// （切词结果本身由 jieba 词典决定，断言写的是 0.10 实测输出 —— 升级 jieba 要重量一遍。）
    #[test]
    fn terms_of_segments_chinese_at_word_level() {
        // 词级命中正是这一路存在的理由：「打车费/限额」是**词**，trgm 给不出这种切口
        let t = terms_of("差旅打车费每天限额多少");
        assert!(t.contains(&"限额".to_string()), "{t:?}");
        assert!(t.contains(&"车费".to_string()), "打车费 → 打(单字滤掉)+车费：{t:?}");
        assert!(!t.contains(&"多少".to_string()), "疑问词进停用词表：{t:?}");
        // 单字语法字不进词表
        assert!(t.iter().all(|w| w.chars().count() >= 2), "单字词必须滤净：{t:?}");
        // 型号整体保留且小写化（全角同口径折叠）
        assert!(terms_of("DHT150-6 的报销比例").contains(&"dht150-6".to_string()));
        assert_eq!(terms_of("ＤＨＴ１５０－６"), terms_of("dht150-6"), "全角折叠与 normalize_query 同口径");
        // 数字词保留（「住宿 500 元/晚」的 500 是判据词）
        assert!(terms_of("发票开具后15个工作日内提交").contains(&"15".to_string()));
        // 纯标点/空串 → 空词表（查询侧据此早退，省一次可见块全扫）
        assert!(terms_of("？？？…").is_empty());
        assert!(terms_of("").is_empty());
        // 幂等：归一化问句再过一次 terms_of 结果不变（查询侧输入是 normalize_query 的产物）
        let q = terms_of("一线城市住宿费用标准是多少");
        assert_eq!(terms_of(&q.join(" ")), q);
    }

    /// 去重保序 + 控制字符防御（`\x01` 是落库分隔符，词里绝不允许出现）
    #[test]
    fn terms_of_dedups_in_order_and_strips_control_chars() {
        // jieba 把「报销制度」切成「报销/制度」两个词（词级切口的实测样例）
        let t = terms_of("报销 报销 报销制度");
        assert_eq!(t.iter().filter(|w| *w == "报销").count(), 1, "重复词去重：{t:?}");
        let first = t.iter().position(|w| w == "报销").unwrap();
        let second = t.iter().position(|w| w == "制度").unwrap();
        assert!(first < second, "保序：{t:?}");
        // 控制字符被剥掉，分隔符完整性不受输入影响
        assert!(terms_of("报\u{1}销\u{2}制度").iter().all(|w| !w.contains('\u{1}')));
    }

    /// 写读往返：blob 线格式与 SQL 侧 `string_to_array(?, chr(1))` 互逆。
    /// PG 语义钉两条：`string_to_array('a\x01b', chr(1)) = {a,b}`、`string_to_array('', chr(1)) = {}`
    /// （连库评测时已实测复核 —— 改线格式前先把这两条在真库里再敲一遍）。
    #[test]
    fn terms_blob_round_trips_through_the_wire_format() {
        let terms = terms_of("一线城市住宿费用标准是多少");
        assert!(!terms.is_empty());
        let blob = terms_blob(&terms);
        // 模拟 PG string_to_array：按 \x01 切；空串 → 空数组
        let back: Vec<String> = if blob.is_empty() { Vec::new() } else { blob.split('\u{1}').map(str::to_string).collect() };
        assert_eq!(back, terms, "blob 往返必须无损");
        assert!(terms_blob(&[]).is_empty(), "空词表落 ''（SQL 侧还原成空数组，不是单空串元素）");
    }

    /// terms 列 + GIN 索引在 0020（kb schema 唯一 DDL 真相源），且切句器切得干净
    #[test]
    fn terms_column_and_gin_index_are_migrated() {
        let stmts: Vec<&str> = statements(KB_DDL).collect();
        // 语句碎片带前导注释行（切句器按 `;` 切，注释跟着下一条语句走），故用 contains
        assert!(
            stmts.iter().any(|s| s.contains("ALTER TABLE kb.chunk ADD COLUMN IF NOT EXISTS terms text[]")),
            "0020 缺 terms 列迁移（独立成句，幂等）"
        );
        assert!(
            stmts.iter().any(|s| s.trim_start().starts_with("CREATE INDEX IF NOT EXISTS idx_kb_chunk_terms ON kb.chunk USING gin (terms)")),
            "0020 缺 terms 的 GIN 索引"
        );
        // 列必须在索引之前（migrate 逐句顺序执行，反了当场 42703）
        let at = |needle: &str| KB_DDL.find(needle).unwrap_or_else(|| panic!("0020 里没有：{needle}"));
        assert!(
            at("ADD COLUMN IF NOT EXISTS terms text[]") < at("idx_kb_chunk_terms"),
            "terms 加列必须排在它的 GIN 索引之前"
        );
    }

    /// 两条落块语句都必须同语句写词列（正文与词列不许有中间态），bind 序在既有参数之后追加
    #[test]
    fn chunk_writes_carry_terms() {
        let src = include_str!("store.rs");
        let insert = src.split("pub async fn insert_chunks").nth(1).unwrap();
        let insert = insert.split("pub async fn ").next().unwrap();
        assert!(insert.contains("start_char_pos,end_char_pos,terms"), "insert 列清单缺 terms");
        assert!(insert.contains("string_to_array(u.tblob, chr(1))"), "insert 缺线格式还原");
        assert!(insert.contains("$12::text[]"), "terms blob 数组只能追加在既有 bind 之后");
        assert!(insert.contains("cstart,cend,tblob"), "insert 的 unnest 别名缺 tblob");

        let replace = src.split("pub async fn replace_chunks").nth(1).unwrap();
        let replace = replace.split("pub async fn ").next().unwrap();
        assert!(replace.contains("end_char_pos,terms)"), "replace 列清单缺 terms");
        assert!(replace.contains("terms=EXCLUDED.terms"), "影子重建必须同语句重写词列");
        assert!(replace.contains("$19::text[]"), "terms blob 数组只能追加在既有 bind 之后");
        assert!(replace.contains("cstart,cend,tblob"), "replace 的 unnest 别名缺 tblob");
    }

    /// 回填两条 SQL 的合同：游标谓词（terms IS NULL）+ 三重收口 CAS
    #[test]
    fn terms_backfill_sql_contracts() {
        let src = include_str!("store.rs");
        let nulls = src.split("pub async fn null_terms_chunks").nth(1).unwrap();
        let nulls = nulls.split("pub async fn ").next().unwrap();
        assert!(nulls.contains("WHERE terms IS NULL"), "回填游标就是 terms IS NULL");
        assert!(nulls.contains("ORDER BY chunk_id LIMIT $1"), "键集分批（与 null_vec_chunks 同约）");
        assert!(!nulls.contains("kb.doc"), "不按 doc 状态过滤（检索侧谓词已收口，别抄一份跟着漂）");

        let set = src.split("pub async fn set_chunk_terms").nth(1).unwrap();
        let set = set.split("\n}\n").next().unwrap();
        assert!(set.contains("c.chunk_id=v.id AND c.text=v.txt AND c.terms IS NULL"), "三重收口 CAS 变了");
        assert!(set.contains("string_to_array(v.blob, chr(1))"), "回写缺线格式还原");
    }

    /// 🔴 回填任务的三个坑全在源码层（无库单测碰不到）—— 与 embed_fill 同一族判据：
    /// ① try 锁（阻塞锁会把替补实例睡死）；② 锁与解锁同一条连接；③ 失败路径也解锁。
    /// 外加：spawn 必须排在 migrate 的事务 **commit 之后**（任务另起连接读表，没提交看不到新列）。
    #[test]
    fn terms_backfill_lock_is_try_same_conn_and_always_unlocked() {
        let src = include_str!("store.rs");
        let body = src.split("async fn terms_backfill_round(").nth(1).expect("terms_backfill_round 没了");
        let body = body.split("\nasync fn ").next().unwrap();
        assert!(body.contains("try_advisory_lock"), "必须走 connector 的非阻塞会话锁：{body}");
        assert!(body.contains("lock.release().await"), "没有释放锁 guard：{body}");
        let fill = body.find("terms_backfill_all(store).await").expect("terms_backfill_all 调用没了");
        let unlock = body.find("lock.release().await").unwrap();
        assert!(fill < unlock, "失败路径不解锁会终身占锁：{body}");

        let migrate = src.split("pub async fn migrate").nth(1).unwrap();
        let migrate = migrate.split("\n}\n").next().unwrap();
        let commit = migrate.find("tx.commit().await?").expect("migrate 的 commit 没了");
        let spawn = migrate.find("spawn_terms_backfill(store.clone())").expect("migrate 没挂回填任务");
        assert!(commit < spawn, "回填任务必须排在事务 commit 之后：{migrate}");
    }

    /// 回填循环的防空转闸：整批 CAS 失配必须收工（并发重建重写同一批行时原地空转只会互踩）
    #[test]
    fn terms_backfill_aborts_a_fully_cas_mismatched_batch() {
        let src = include_str!("store.rs");
        let body = src.split("async fn terms_backfill_all(").nth(1).expect("terms_backfill_all 没了");
        let body = body.split("\n}\n").next().unwrap();
        assert!(body.contains("if n == 0"), "整批 0 行要写必须有提前收工闸：{body}");
    }
}
