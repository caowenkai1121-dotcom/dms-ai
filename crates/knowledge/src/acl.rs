//! **本 crate 唯一越权面**——独立成文件就是为了能被单独 review。变更原因＝谁能看/写哪篇。
//!
//! 两条口径：
//! 1. **可见性 ACL 先行**：内联进检索 SQL 的是宏 `visible_docs!()`（编译期文本）；
//!    `visible_docs_sql()` 只是同一份文本的运行时视图，只服务单测与文档。
//!    不做「查完再过滤」（ARCHITECTURE §2 I4）。
//! 2. **写权限 v1 最小口径**：空间 owner 恒可写；其余一律要显式
//!    `kb.acl(scope='space', target_id=space_id, grantee=login|role, perm='write')`
//!    （fail-closed）。**不按 `space_id == viewer.login` 放行**：管理员可创建自定义空间，
//!    字符串碰撞不能变成权限。
//!    不设 `perm` 就无法表达「可读不可写」→ 任何认证用户都能往别人空间投毒写，
//!    而带引用的回答会让同事读到伪造的「制度原文」。

use crate::store::DocRow;
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

/// 授予对象：登录名或角色码（对应 `kb.acl` 的 `grantee_kind` + `grantee` 两列）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grantee {
    Login(String),
    Role(String),
}

impl Grantee {
    pub fn kind(&self) -> &'static str {
        match self {
            Grantee::Login(_) => "login",
            Grantee::Role(_) => "role",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Grantee::Login(s) | Grantee::Role(s) => s,
        }
    }

    /// id 先 trim；trim 后为空返回 `None`——不接受会落出 `grantee=''` 废授权行的输入。
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

/// 空间读/写判据的公共 SQL：两条语句只差 perm 谓词，grantee/role 谓词逐字共享——
/// 日后改谓词只能改这一处，改出两份就是越权洞。`$1`=space_id、`$2`=login、`$3`=角色码。
/// （判据条件本身一字不动，锚点测试 `readable_and_writable_are_separate_contracts` 钉住。）
macro_rules! space_acl_sql {
    ($perm:literal) => {
        concat!(
            "SELECT count(*) FROM kb.space s WHERE s.space_id=$1 AND (s.owner=$2 OR EXISTS ( \
               SELECT 1 FROM kb.acl a WHERE a.scope='space' AND a.target_id=s.space_id \
                 AND ",
            $perm,
            " AND ((a.grantee_kind='login' AND a.grantee=$2) \
                   OR (a.grantee_kind='role' AND a.grantee = ANY($3::text[])))))"
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
                            OR (a.grantee_kind='role'  AND a.grantee = ANY($2::text[]))))"
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
        assert_eq!(Grantee::parse("dept", "1"), None);
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
