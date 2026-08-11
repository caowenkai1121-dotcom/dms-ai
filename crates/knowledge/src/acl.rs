//! **本 crate 唯一越权面**——独立成文件就是为了能被单独 review。变更原因＝谁能看/写哪篇。
//!
//! 两条口径：
//! 1. **可见性 ACL 先行**：内联进检索 SQL 的是宏 `visible_docs!()`（编译期文本）；
//!    `visible_docs_sql()` 只是同一份文本的运行时视图，只服务单测与文档。
//!    不做「查完再过滤」（ARCHITECTURE §2 I4）。
//! 2. **写权限 v1 最小口径**：空间 owner 恒可写；其余一律要显式
//!    `kb.acl(scope='space', target_id=space_id, grantee=login|role|dept, perm='write')`
//!    （fail-closed）。**不按 `space_id == viewer.login` 放行**：管理员可创建自定义空间，
//!    字符串碰撞不能变成权限。
//!    不设 `perm` 就无法表达「可读不可写」→ 任何认证用户都能往别人空间投毒写，
//!    而带引用的回答会让同事读到伪造的「制度原文」。
//!
//! 【share_config v2 · 部门支路】`grantee_kind='dept'` 与 login/role **并存取并集**
//! （同一目标的授权行互相独立，命中任意一路即放行）。部门归属的真相在 MySQL
//! `t_employee.department_id`，而本层 SQL 全在 PG 内求值且占位符契约钉死
//! （`$1`=login、`$2`=角色码，内联者从 `$3` 起编号——retrieve/store/kg 的内联点
//! 一个都不能改），因此 dept 支路经 PG 侧映射表 `kb.user_dept` 按 `$1`（login）
//! 相关子查询求值：映射行缺失或 `dept` 对不上即不匹配（fail-closed，不写特例）。
//! 映射随每次 KB 请求按现算的 Principal 幂等刷新（`sync_viewer_dept`）。
//! **不搬 Yuxi 的「min(授予, 角色上限)」**：那套是给「角色即权限天花板」的模型打的
//! 补丁，我方授权本来就显式写 `perm=read|write`，授予几档就是几档，语义已等价。

use crate::store::{DocRow, FolderRow, SpaceRow};
use crate::{KbError, Viewer};
use dms_connector::owned::OwnedStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclScope {
    Space,
    Doc,
    /// K4 上传表格建出的数据源（私有台账不该被别人 NL2SQL 查到）
    Ds,
}

impl AclScope {
    pub fn as_str(self) -> &'static str {
        match self {
            AclScope::Space => "space",
            AclScope::Doc => "doc",
            AclScope::Ds => "ds",
        }
    }

    /// 输入须已 trim + 小写（调用方负责归一）；本函数对大小写/空白不做宽容。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "space" => Some(AclScope::Space),
            "doc" => Some(AclScope::Doc),
            "ds" => Some(AclScope::Ds),
            _ => None,
        }
    }
}

/// 授予对象：登录名、角色码或部门（对应 `kb.acl` 的 `grantee_kind` + `grantee` 两列）。
/// `Dept` 存 `t_department.department_id` 的字符串形（与 `kb.user_dept.dept` 同型比较）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grantee {
    Login(String),
    Role(String),
    Dept(String),
}

impl Grantee {
    pub fn kind(&self) -> &'static str {
        match self {
            Grantee::Login(_) => "login",
            Grantee::Role(_) => "role",
            Grantee::Dept(_) => "dept",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Grantee::Login(s) | Grantee::Role(s) | Grantee::Dept(s) => s,
        }
    }

    /// id 先 trim；trim 后为空返回 `None`——不接受会落出 `grantee=''` 废授权行的输入。
    /// **严格白名单只收 login|role**：ds 授权面（admin_api）拿本函数当唯一闸，
    /// 那里的可见性 SQL 没有部门支路，收进 dept 就是一条永不命中的死授权。
    /// KB 空间/文档共享请走 `parse_shareable`。
    pub fn parse(kind: &str, id: &str) -> Option<Self> {
        let id = id.trim();
        if id.is_empty() {
            return None;
        }
        match kind {
            "login" => Some(Grantee::Login(id.to_string())),
            "role" => Some(Grantee::Role(id.to_string())),
            _ => None,
        }
    }

    /// KB 共享面的解析：`parse` 之上加 `dept` 一支（share_config v2 部门授权）。
    /// 与 `parse` 分叉是有意的：两个授权面的可见性 SQL 支路集不同，白名单必须各自贴合。
    pub fn parse_shareable(kind: &str, id: &str) -> Option<Self> {
        if kind.trim() == "dept" {
            let id = id.trim();
            return (!id.is_empty()).then(|| Grantee::Dept(id.to_string()));
        }
        Grantee::parse(kind, id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Perm {
    Read,
    Write,
}

impl Perm {
    pub fn as_str(self) -> &'static str {
        match self {
            Perm::Read => "read",
            Perm::Write => "write",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Perm::Read),
            "write" => Some(Perm::Write),
            _ => None,
        }
    }
}

/// 一条授权（命名结构体而非 5 个 `&str` 连排，D4）
#[derive(Debug, Clone)]
pub struct AclEntry {
    pub scope: AclScope,
    pub target_id: String,
    pub grantee: Grantee,
    pub perm: Perm,
}

#[derive(Debug, Clone)]
pub struct AclRow {
    pub grantee_kind: String,
    pub grantee: String,
    pub perm: String,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for AclRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            grantee_kind: row.try_get("grantee_kind")?,
            grantee: row.try_get("grantee")?,
            perm: row.try_get("perm")?,
        })
    }
}

/// 目标存在性由调用方保证：target_id 打错字会落一条永远匹配不上的孤儿授权，本层不前置校验。
/// `ON CONFLICT DO NOTHING`：「授权已存在」与「新建成功」同返 Ok，影响行数不向上传。
pub async fn grant(store: &OwnedStore, e: &AclEntry) -> Result<(), KbError> {
    store
        .fixed(
            "INSERT INTO kb.acl(scope,target_id,grantee_kind,grantee,perm) \
             VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",
        )
        .bind(e.scope.as_str())
        .bind(&e.target_id)
        .bind(e.grantee.kind())
        .bind(e.grantee.id())
        .bind(e.perm.as_str())
        .execute()
        .await?;
    Ok(())
}

pub async fn revoke(store: &OwnedStore, e: &AclEntry) -> Result<(), KbError> {
    store
        .fixed(
            "DELETE FROM kb.acl WHERE scope=$1 AND target_id=$2 \
             AND grantee_kind=$3 AND grantee=$4 AND perm=$5",
        )
        .bind(e.scope.as_str())
        .bind(&e.target_id)
        .bind(e.grantee.kind())
        .bind(e.grantee.id())
        .bind(e.perm.as_str())
        .execute()
        .await?;
    Ok(())
}

pub async fn list_target(
    store: &OwnedStore,
    scope: AclScope,
    target_id: &str,
) -> Result<Vec<AclRow>, KbError> {
    // 宽裕上限：单目标授权正常是零星几行，1000 只是防失控的保险丝
    Ok(store
        .fixed(
            "SELECT grantee_kind,grantee,perm FROM kb.acl WHERE scope=$1 AND target_id=$2 \
             ORDER BY grantee_kind,grantee,perm LIMIT 1000",
        )
        .bind(scope.as_str())
        .bind(target_id)
        .fetch_all()
        .await?)
}

// ─────────────────── 【share_config v2 · 部门支路】 ───────────────────

/// login→部门 映射的随请求刷新（dept 授权的求值底座，见文件头注）。
/// `dept` 是 `t_department.department_id` 的字符串形（与 principal 现算同刻同源）；
/// `None` = 当前无部门 → 删行：旧部门授权即刻不再命中（fail-closed）。
/// 未变化时不重写行（`IS DISTINCT FROM` 守卫），上传轮询这类高频路径不白付写放大。
pub async fn sync_viewer_dept(
    store: &OwnedStore,
    login: &str,
    dept: Option<&str>,
) -> Result<(), KbError> {
    match dept.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => {
            store
                .fixed(
                    "INSERT INTO kb.user_dept(login,dept) VALUES($1,$2) \
                     ON CONFLICT(login) DO UPDATE SET dept=EXCLUDED.dept,updated_at=now() \
                     WHERE kb.user_dept.dept IS DISTINCT FROM EXCLUDED.dept",
                )
                .bind(login)
                .bind(d)
                .execute()
                .await?;
        }
        None => {
            store
                .fixed("DELETE FROM kb.user_dept WHERE login=$1")
                .bind(login)
                .execute()
                .await?;
        }
    }
    Ok(())
}

/// 以下三个 `dept_visible_*` 是 **dept 支路带来的可见性增量**（纯并集项）。
/// store.rs 的 `list_spaces`/`list_docs`/`list_folders` 把手写内联判据复制在各自的 SQL 里
/// （本轮属另一路改动窗口，不能动），那里只认 owner/login/role —— 管理面清单因此
/// = store 既有结果 ∪ 本组查询结果（调用方按主键去重，不删减任何既有行）。
/// 本组查询**只含 dept 一条支路**：owner/login/role 的判据真相仍只在 store.rs，
/// 这里不复述——它们语义变更时本组查询不需要跟着动（drift 面最小化）。

/// dept 授权使当前 login 可见的空间（`writable` = 任一行 write 档授权）。
pub async fn dept_visible_spaces(store: &OwnedStore, login: &str) -> Result<Vec<SpaceRow>, KbError> {
    Ok(store
        .fixed(
            "SELECT s.space_id, s.name, s.owner, s.visibility, \
                    BOOL_OR(a.perm='write') AS writable, \
                    (SELECT count(*) FROM kb.doc d WHERE d.space_id=s.space_id) AS doc_count \
             FROM kb.space s JOIN kb.acl a ON a.scope='space' AND a.target_id=s.space_id \
             WHERE a.grantee_kind='dept' AND a.perm IN ('read','write') \
               AND a.grantee = (SELECT m.dept FROM kb.user_dept m WHERE m.login=$1) \
             GROUP BY s.space_id, s.name, s.owner, s.visibility \
             ORDER BY s.name, s.space_id",
        )
        .bind(login)
        .fetch_all()
        .await?)
}

/// 空间内 dept 授权使当前 login 可见的文档（与 `store::list_docs` 同列同序）。
pub async fn dept_visible_docs(
    store: &OwnedStore,
    login: &str,
    space_id: &str,
) -> Result<Vec<DocRow>, KbError> {
    Ok(store
        .fixed(concat!(
            "SELECT ",
            crate::store::doc_cols!(),
            " FROM kb.doc d WHERE d.space_id=$2 AND EXISTS ( \
               SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=d.space_id \
                 AND a.perm IN ('read','write') AND a.grantee_kind='dept' \
                 AND a.grantee = (SELECT m.dept FROM kb.user_dept m WHERE m.login=$1)) \
             ORDER BY created_at DESC, doc_id"
        ))
        .bind(login)
        .bind(space_id)
        .fetch_all()
        .await?)
}

/// 空间内 dept 授权使当前 login 可见的目录（与 `store::list_folders` 同列同序）。
pub async fn dept_visible_folders(
    store: &OwnedStore,
    login: &str,
    space_id: &str,
) -> Result<Vec<FolderRow>, KbError> {
    Ok(store
        .fixed(
            "SELECT f.folder_id,f.space_id,f.parent_id,f.name,f.path, \
                    (length(f.path)-length(replace(f.path,'/','')))::int AS depth, \
                    (SELECT count(*) FROM kb.folder c WHERE c.parent_id=f.folder_id) AS child_count, \
                    (SELECT count(*) FROM kb.doc d WHERE d.folder_id=f.folder_id) AS doc_count, \
                    f.created_at::text AS created_at,f.updated_at::text AS updated_at \
             FROM kb.folder f WHERE f.space_id=$2 AND EXISTS ( \
               SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=f.space_id \
                 AND a.perm IN ('read','write') AND a.grantee_kind='dept' \
                 AND a.grantee = (SELECT m.dept FROM kb.user_dept m WHERE m.login=$1)) \
             ORDER BY f.path,lower(f.name),f.folder_id",
        )
        .bind(login)
        .bind(space_id)
        .fetch_all()
        .await?)
}

/// 空间读/写判据的公共 SQL：两条语句只差 perm 谓词，grantee/role/dept 谓词逐字共享——
/// 日后改谓词只能改这一处，改出两份就是越权洞。`$1`=space_id、`$2`=login、`$3`=角色码。
/// dept 支路：`kb.user_dept` 按 login 相关子查询取部门，映射缺失/对不上即不匹配
/// （fail-closed，不写特例）。
/// （判据条件本身一字不动，锚点测试 `readable_and_writable_are_separate_contracts` 钉住。）
macro_rules! space_acl_sql {
    ($perm:literal) => {
        concat!(
            "SELECT count(*) FROM kb.space s WHERE s.space_id=$1 AND (s.owner=$2 OR EXISTS ( \
               SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=s.space_id \
                 AND ",
            $perm,
            " AND ((a.grantee_kind='login' AND a.grantee=$2) \
                   OR (a.grantee_kind='role' AND a.grantee = ANY($3::text[])) \
                   OR (a.grantee_kind='dept' AND a.grantee = \
                       (SELECT m.dept FROM kb.user_dept m WHERE m.login=$2)))))"
        )
    };
}

/// 能否往这个空间写（上传/删除的唯一判据）。
/// 空间 owner 恒可写；其余一律要显式 `perm='write'`（fail-closed）。
/// 不按 `space_id == login` 放行：管理员可创建自定义空间，字符串碰撞不能变成权限。
pub async fn space_writable(
    store: &OwnedStore,
    v: &Viewer,
    space_id: &str,
) -> Result<bool, KbError> {
    // 空间 owner 恒可写；其他人必须有显式 write 授权。
    let n = store
        .fixed(space_acl_sql!("a.perm='write'"))
        .bind(space_id)
        .bind(&v.login)
        .bind(&v.roles)
        .fetch_optional::<(i64,)>()
        .await?
        .map_or(0, |(n,)| n);
    Ok(n > 0)
}

/// 空间能否读取。所有管理列表和上传前选择空间都复用它，避免 server 自己复述 ACL。
pub async fn space_readable(
    store: &OwnedStore,
    v: &Viewer,
    space_id: &str,
) -> Result<bool, KbError> {
    let n = store
        .fixed(space_acl_sql!("a.perm IN ('read','write')"))
        .bind(space_id)
        .bind(&v.login)
        .bind(&v.roles)
        .fetch_optional::<(i64,)>()
        .await?
        .map_or(0, |(n,)| n);
    Ok(n > 0)
}

/// 取文档并判可见性；不可见即 `Forbidden`（不区分「不存在」——那会泄露他人文档的存在性）
pub async fn doc_for_viewer(
    store: &OwnedStore,
    v: &Viewer,
    doc_id: &str,
) -> Result<DocRow, KbError> {
    store
        .fixed(concat!(
            "SELECT ",
            crate::store::doc_cols!(),
            " FROM kb.doc WHERE doc_id=$3 AND doc_id IN (",
            // 路径式调用：`macro_rules!` 是文本作用域，定义写在本文件下方（挨着它的文档）
            crate::acl::visible_docs!(),
            ")"
        ))
        .bind(&v.login)
        .bind(&v.roles)
        .bind(doc_id)
        .fetch_optional()
        .await?
        .ok_or_else(|| KbError::Forbidden(format!("文档 {doc_id} 不可见")))
}

/// 可见文档子查询片段，用法 `... WHERE doc_id IN ({})`。
/// **占位符固定**：`$1` = login（text）、`$2` = 角色码（text[]）；内联者的其余参数从 `$3` 起编号。
/// 供 K2 检索内联（ACL 先行，不做后过滤）。`perm IN ('read','write')`：write 蕴含 read。
/// dept 支路不占新占位符：`kb.user_dept` 按 `$1`（login）相关子查询取部门再比对——
/// 内联者（retrieve/store/kg）因此零改动获得部门语义；viewer 无部门 = 子查询出 NULL =
/// 天然不匹配（fail-closed，不写特例）。
///
/// **是宏不是函数**：内联者要在**编译期**把它 `concat!` 进自己的 `&'static str`
/// （`fixed()` 通道不接受 `format!` 出来的 `String`）。`visible_docs_sql()` 是同一份文本的
/// 运行时视图，只服务单测与文档 —— 两处永不可能分叉。
///
/// ⚠️ 本片段**只管「谁能看」**：`enabled/status/生效期` 等文档生命周期过滤不在里面，
/// 是每个内联者自己的义务（范例见 `kg.rs` 的 `build_chunks_sql`/`visible_doc_ids_sql`，
/// 均在片段外自行补齐 `d.enabled=true AND d.status IN ('chunked','embedded') AND 生效期`）。
macro_rules! visible_docs {
    () => {
        "SELECT d.doc_id FROM kb.doc d \
         WHERE EXISTS (SELECT 1 FROM kb.space s WHERE s.space_id=d.space_id AND s.owner=$1) \
            OR EXISTS (SELECT 1 FROM kb.acl a \
                        WHERE a.perm IN ('read','write') \
                          AND ((a.scope='space' AND a.target_id = d.space_id) \
                            OR (a.scope='doc'   AND a.target_id = d.doc_id)) \
                          AND ((a.grantee_kind='login' AND a.grantee = $1) \
                            OR (a.grantee_kind='role'  AND a.grantee = ANY($2::text[])) \
                            OR (a.grantee_kind='dept'  AND a.grantee = \
                                (SELECT m.dept FROM kb.user_dept m WHERE m.login = $1))))"
    };
}
pub(crate) use visible_docs;

pub fn visible_docs_sql() -> &'static str {
    visible_docs!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_roundtrip() {
        for s in [AclScope::Space, AclScope::Doc, AclScope::Ds] {
            assert_eq!(AclScope::parse(s.as_str()), Some(s));
        }
        assert_eq!(AclScope::parse("table"), None);
    }

    #[test]
    fn perm_roundtrip() {
        for p in [Perm::Read, Perm::Write] {
            assert_eq!(Perm::parse(p.as_str()), Some(p));
        }
        assert_eq!(Perm::parse("admin"), None);
    }

    #[test]
    fn grantee_roundtrip() {
        let g = Grantee::Login("zhangsan".into());
        assert_eq!(Grantee::parse(g.kind(), g.id()), Some(g));
        let r = Grantee::Role("101".into());
        assert_eq!(r.kind(), "role");
        assert_eq!(Grantee::parse(r.kind(), r.id()), Some(r));
        // 严格 parse 仍拒 dept：ds 授权面（admin_api）拿它当唯一闸，那里没有部门支路
        assert_eq!(Grantee::parse("dept", "1"), None);
    }

    /// dept 一支只经 `parse_shareable` 进 KB 共享面：序列化往返 + 空白拒收
    #[test]
    fn grantee_dept_shareable_roundtrip() {
        let d = Grantee::Dept("42".into());
        assert_eq!(d.kind(), "dept");
        assert_eq!(Grantee::parse_shareable(d.kind(), d.id()), Some(d));
        // login/role 走同一个入口也不许漂
        assert_eq!(
            Grantee::parse_shareable("login", "zhangsan"),
            Some(Grantee::Login("zhangsan".into()))
        );
        assert_eq!(
            Grantee::parse_shareable("role", "101"),
            Some(Grantee::Role("101".into()))
        );
        assert_eq!(Grantee::parse_shareable("dept", ""), None);
        assert_eq!(Grantee::parse_shareable("dept", "   "), None);
        // 合法 id 的外围空白被归一（与 parse 同口径）
        assert_eq!(
            Grantee::parse_shareable("dept", " 42 "),
            Some(Grantee::Dept("42".into()))
        );
        assert_eq!(Grantee::parse_shareable("table", "1"), None);
    }

    /// 空串/纯空白 id 不得落成 `grantee=''` 的废授权行
    #[test]
    fn grantee_rejects_blank_id() {
        assert_eq!(Grantee::parse("login", ""), None);
        assert_eq!(Grantee::parse("role", "   "), None);
        // 合法 id 的外围空白被归一
        assert_eq!(
            Grantee::parse("login", " zhangsan "),
            Some(Grantee::Login("zhangsan".into()))
        );
    }

    /// 片段必须同时按 grantee 与 perm 过滤——少任何一个就是全员可见/可写
    #[test]
    fn visible_fragment_filters_grantee_and_perm() {
        let s = visible_docs_sql();
        assert!(s.contains("a.grantee = $1"));
        assert!(s.contains("a.grantee = ANY($2::text[])"));
        assert!(s.contains("a.perm IN ('read','write')"));
        assert!(!s.contains("d.space_id = $1"), "空间名不得冒充 owner 身份");
        assert!(s.contains("s.owner=$1"));
        assert!(!s.contains("$3"), "片段只许占用 $1/$2，$3 起留给内联者");
    }

    /// dept 支路：两宏（文档可见性 / 空间读写判据）都必须带，且都按 login 相关子查询
    /// 求值——子查询无行（viewer 无部门）即 NULL 比较 = 天然不匹配，不许写特例分支。
    #[test]
    fn dept_branch_in_both_acl_fragments() {
        let s = visible_docs_sql();
        assert!(s.contains("a.grantee_kind='dept'"), "可见性片段丢了部门支路");
        assert!(
            s.contains("(SELECT m.dept FROM kb.user_dept m WHERE m.login = $1)"),
            "dept 支路必须按 $1（login）相关子查询求值，不占新占位符: {s}"
        );
        let src = include_str!("acl.rs");
        let mac = src.split("macro_rules! space_acl_sql").nth(1).unwrap();
        let mac = mac.split("\n}").next().unwrap();
        assert!(mac.contains("a.grantee_kind='dept'"), "空间判据宏丢了部门支路");
        assert!(
            mac.contains("(SELECT m.dept FROM kb.user_dept m WHERE m.login=$2)"),
            "空间判据的 dept 支路必须按 $2（login）求值: {mac}"
        );
    }

    /// 映射刷新：有部门 upsert（未变化不重写行），无部门删行（旧授权即刻不命中）
    #[test]
    fn sync_viewer_dept_shapes() {
        let src = include_str!("acl.rs");
        let body = src.split("pub async fn sync_viewer_dept").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        assert!(body.contains("ON CONFLICT(login) DO UPDATE"), "缺 upsert: {body}");
        assert!(
            body.contains("IS DISTINCT FROM"),
            "未变化必须跳过重写（上传轮询路径的写放大守卫）: {body}"
        );
        assert!(body.contains("DELETE FROM kb.user_dept WHERE login=$1"), "无部门必须删行: {body}");
    }

    /// 三个增量清单只许含 dept 一条支路：owner/login/role 判据真相在 store.rs，
    /// 这里复述一份就是两处漂移面
    #[test]
    fn dept_visible_augment_queries_carry_only_the_dept_arm() {
        let src = include_str!("acl.rs");
        for name in ["dept_visible_spaces", "dept_visible_docs", "dept_visible_folders"] {
            let body = src
                .split(&format!("pub async fn {name}"))
                .nth(1)
                .unwrap_or_else(|| panic!("{name} 没了"));
            let body = body.split("\n}\n").next().unwrap();
            assert!(body.contains("a.grantee_kind='dept'"), "{name} 丢了部门支路");
            assert!(body.contains("kb.user_dept"), "{name} 没走部门映射: {body}");
            assert!(!body.contains("grantee_kind='login'"), "{name} 复述了 login 支路: {body}");
            assert!(!body.contains("grantee_kind='role'"), "{name} 复述了 role 支路: {body}");
            assert!(!body.contains("s.owner="), "{name} 复述了 owner 支路: {body}");
        }
    }

    #[test]
    fn readable_and_writable_are_separate_contracts() {
        let src = include_str!("acl.rs");
        let readable = src.split("pub async fn space_readable").nth(1).unwrap();
        assert!(readable.contains("perm IN ('read','write')"));
        let writable = src.split("pub async fn space_writable").nth(1).unwrap();
        assert!(writable.contains("perm='write'"));
        assert!(!readable.contains("space_id == v.login"));
        assert!(!writable.contains("space_id == v.login"));
    }

    /// 「文档不存在」与「无权看」必须共用同一 `Forbidden` 出口——多一个分支就泄露存在性
    #[test]
    fn doc_for_viewer_has_single_forbidden_exit() {
        let src = include_str!("acl.rs");
        let body = src.split("pub async fn doc_for_viewer").nth(1).unwrap();
        let body = body.split("/// 可见文档子查询片段").next().unwrap();
        assert_eq!(
            body.matches("KbError::Forbidden").count(),
            1,
            "不存在与无权必须走同一 Forbidden 文案出口"
        );
    }
}
