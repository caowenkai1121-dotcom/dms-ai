//! 三段 SQL 类型闸门：`RawSql` → `CheckedSql` → `ScopedSql`，字段全私有。
//! 不变量 I1（ARCHITECTURE §2）：到达生产库的 SQL 必是 `ScopedSql`，而 `ScopedSql` 全仓
//! **唯二**产出点是本文件的 `inject()` 与 `ScopedSql::unrestricted(_, &UnrestrictedProof)`。
//!
//! 本文件只做「状态推进 + 编排」：判定算法全在 `sql::guard`/`sql::ast`，注入算法全在
//! `policy::inject`（一行不改，既有字符串级断言套件继续走 `rewrite()`）。
//!
//! F2 修复点：旧 `rewrite()` 遇 `sets.is_unrestricted()` 原样放行 —— 于是 `ScopeSets::default()`
//! （= 忘了算权限）就是一把万能 `ScopedSql` 铸造钥匙。这里改成：`inject()` 拒绝无限制集合，
//! 放行必须显式走 `unrestricted(checked, &proof)`。字符串级 `rewrite()` 的放行分支保持不动
//! （那是那套断言的地基），闸门层负责不让它成为默认后果。

use crate::errors::{GuardError, PolicyError};
use crate::policy::inject::rewrite;
use crate::policy::rules::RuleSet;
use crate::policy::scope::ScopeSets;
use crate::sql::ast::table_names_of;
use crate::sql::dialect::Dialect;
use crate::sql::guard::{ensure_limit_with, is_safe_select_with, GuardConfig};

/// 未经任何校验的 SQL 文本。LLM 输出、模板装配、语义缓存回放、CLI 入参**都必须**经此包装：
/// 它本身不提供任何读取口，唯一出路是 `check()`。
pub struct RawSql(String);

impl RawSql {
    pub fn new(sql: impl Into<String>) -> Self {
        Self(sql.into())
    }
}

/// 过了只读红线与 LIMIT 护栏的 SQL。带着 `dialect`：多源下「用哪个方言 parse」是 SQL 自身的属性，
/// 存在这里既保证 `inject()` 不会拿错方言，也让 `inject` 维持三参签名（裁决 C2）。
pub struct CheckedSql {
    text: String,
    tables: Vec<String>,
    dialect: &'static dyn Dialect,
}

impl CheckedSql {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 语句涉及的实表名（去重、字典序、排除 CTE 名；限定名 `db.t` 取**首段**——
    /// 与 `ast.rs` 的收集语义对齐，见那边的注释）。权限档案登记核对与 trace 用。
    pub fn tables(&self) -> &[String] {
        &self.tables
    }

    pub fn dialect(&self) -> &'static dyn Dialect {
        self.dialect
    }
}

/// 可以执行的 SQL。字段私有 + 无 `Default`/`Clone`/构造字面量：下游 crate 只能读，不能造。
///
/// 结构性保证（下游 crate 视角，字段私有故编译失败）：
/// ```compile_fail
/// let s = dms_kernel::ScopedSql { text: "SELECT 1".into(), unrestricted: true };
/// ```
// 注意：上面这段 compile_fail doctest 硬编码了 crate 名 `dms_kernel` —— 与 Cargo package
// 名耦合，包改名时记得同步（不改则该守卫悄悄失效/报错）。
pub struct ScopedSql {
    text: String,
    unrestricted: bool,
}

impl ScopedSql {
    /// 给 connector 读串用（必须 pub，见 ARCHITECTURE §2 I1 的「残缺」列）。
    pub fn wire(&self) -> &str {
        &self.text
    }

    pub fn is_unrestricted(&self) -> bool {
        self.unrestricted
    }

    /// 免注入放行的**唯一**入口：要一份 `UnrestrictedProof`。
    /// `grep 'ScopedSql::unrestricted('` = 全仓无权限出口清单。
    pub fn unrestricted(sql: CheckedSql, _proof: &UnrestrictedProof) -> Self {
        Self { text: sql.text, unrestricted: true }
    }
}

/// 「这次查询确实不受行级权限限制」的凭证。字段私有、无 `Default`、无 `Clone`。
///
/// ponytail: 这是**检查过的构造器**，不是能力令牌 —— 调用方硬传 `admin_or_all: true`
/// 仍能骗过它（要真令牌得让 policy 持私有构造种子，成本远超收益）。它换来的两件事是实的：
/// ① `grep UnrestrictedProof::new` 就是全仓权限出口清单（今天只应有 `dms_policy::proof`
/// 与 autodiscover 管理员工具两处）；② 放行不再是「忘了算 ScopeSets」的默认后果。
/// 升级路径：真需要不可伪造时，把 `new` 收成 `pub(crate)` 并由 policy 经密封 trait 铸造。
pub struct UnrestrictedProof(());

impl UnrestrictedProof {
    /// 唯一铸造入口：必须同时给出两个证据 —— 集合确实无限制 + 身份确实是超管/ALL 档。
    /// 返回 `Option` 而非直接构造：`ScopeSets::default()` 单独不再足以铸造放行凭证。
    pub fn new(sets: &ScopeSets, admin_or_all: bool) -> Option<Self> {
        (sets.is_unrestricted() && admin_or_all).then_some(Self(()))
    }

    /// 第二条铸造路径：**该数据源整体不做行级过滤**（注册表里 `policy_kind='global'`）。
    ///
    /// 用于上传表格建出的源与纯维表源：它们没有 owner/customer 列可过滤，
    /// 「谁能看」这件事在**选源**那一层就已经由 ds 级 ACL（`kb.acl(scope='ds')`）判完了。
    /// 与 `new` 的关键区别是**不要求集合全空** —— 受限用户查自己上传的台账时
    /// `ScopeSets` 恒非空，若还走 `inject` 会因该表不在 DMS 权限档案里被 fail-closed 拒绝，
    /// 也就是「上传了自己的文件反而查不了」。
    ///
    /// 唯一证据是 `ds_authorized`：调用方必须先用 `visible_datasources` 判定该源对本人可见。
    /// 天花板同 `new`（检查过的构造器，不是不可伪造令牌），故它也在
    /// `grep UnrestrictedProof::` 的审计清单里。
    pub fn for_global_source(ds_authorized: bool) -> Option<Self> {
        ds_authorized.then_some(Self(()))
    }
}

/// 第一段闸门：只读红线（单 SELECT / 写操作词 / 系统库 / 敏感列 / 占位符幻觉）→ 补 LIMIT → 抽实表名。
/// 顺序刻意先校验后补 LIMIT：追加的是我们自己的常量，被校验的必须是调用方原文。
pub fn check(
    raw: RawSql,
    d: &'static dyn Dialect,
    g: &GuardConfig,
) -> Result<CheckedSql, GuardError> {
    is_safe_select_with(&raw.0, d, g.sensitive_cols)?;
    let text = ensure_limit_with(&raw.0, d, g.max_rows);
    // 这个 `?` 实际不可达：text 已在 is_safe_select_with 里 parse 成功过一次。
    // 它一旦可达，就说明「能 parse 的串第二关却 parse 失败」——前两关之间有洞，值得知道。
    let tables = table_names_of(&text, d)?;
    Ok(CheckedSql { text, tables, dialect: d })
}

/// 第二段闸门：注入行级权限条件。受限集合才走这里。
///
/// 无限制集合（超管 / ALL 档 / **也包括「忘了算权限」的 `ScopeSets::default()`**）在这里被拒：
/// 放行必须显式走 `ScopedSql::unrestricted(checked, &proof)`，让每个出口都留下 grep 痕迹。
pub fn inject(
    sql: CheckedSql,
    sets: &ScopeSets,
    rules: &RuleSet,
) -> Result<ScopedSql, PolicyError> {
    if sets.is_unrestricted() {
        return Err(PolicyError::NeedsProof);
    }
    let text = rewrite(&sql.text, sets, rules, sql.dialect)?;
    Ok(ScopedSql { text, unrestricted: false })
}

/// kernel 自守测试：泛化表名（orders/bills/goods），无库无网。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::rules::{Binding, CustomerKind, OwnerKind, TableRule};
    use crate::sql::dialect::MysqlDialect;
    use std::collections::HashMap;

    const G: GuardConfig = GuardConfig::new(200, &["login_pwd", "password"]);

    fn rules() -> RuleSet {
        let mut m: HashMap<String, TableRule> = HashMap::new();
        m.insert(
            "orders".into(),
            TableRule::Scoped(Binding {
                customer_col: Some("cust_code".into()),
                customer_kind: CustomerKind::Codes,
                owner_col: Some("owner_id".into()),
                owner_kind: OwnerKind::Ids,
            }),
        );
        m.insert("bills".into(), TableRule::Global);
        RuleSet::from(m)
    }

    fn sets(ids: &[i64]) -> ScopeSets {
        ScopeSets { employee_ids: ids.to_vec(), ..Default::default() }
    }

    fn checked(sql: &str) -> Result<CheckedSql, GuardError> {
        check(RawSql::new(sql), &MysqlDialect, &G)
    }

    #[test]
    fn check_appends_limit_and_keeps_existing() {
        let c = checked("SELECT * FROM orders o WHERE o.deleted_flag = 0").unwrap();
        assert!(c.text().ends_with("LIMIT 200"), "{}", c.text());
        let kept = checked("SELECT * FROM orders LIMIT 5").unwrap();
        assert_eq!(kept.text(), "SELECT * FROM orders LIMIT 5");
        assert_eq!(c.dialect().name(), "MySQL");
    }

    /// `.err().unwrap()` 而非 `.unwrap_err()`：后者要求 `CheckedSql: Debug`，
    /// 而 Debug 对闸门类型今天零消费者（要看串就 `text()`/`wire()`）。
    #[test]
    fn check_rejects_write_and_sensitive() {
        assert!(matches!(checked("DELETE FROM orders").err().unwrap(), GuardError::NotSelect));
        assert!(matches!(
            checked("SELECT * FROM orders; SELECT 1").err().unwrap(),
            GuardError::MultiStatement
        ));
        assert!(matches!(
            checked("SELECT login_pwd FROM orders").err().unwrap(),
            GuardError::SensitiveColumn(_)
        ));
    }

    #[test]
    fn check_extracts_real_tables_excluding_cte() {
        let c = checked(
            "WITH x AS (SELECT owner_id FROM orders) SELECT COUNT(*) FROM x JOIN bills b ON 1 = 1",
        )
        .unwrap();
        assert_eq!(c.tables(), ["bills".to_string(), "orders".to_string()]);
    }

    #[test]
    fn inject_scoped_user_gets_condition() {
        let s = inject(checked("SELECT * FROM orders o").unwrap(), &sets(&[7]), &rules()).unwrap();
        assert!(s.wire().to_lowercase().replace(' ', "").contains("o.owner_idin(7)"), "{}", s.wire());
        assert!(s.wire().contains("LIMIT 200"), "{}", s.wire());
        assert!(!s.is_unrestricted());
    }

    /// F2：无限制集合不再是「原样放行」，必须显式带 proof（`ScopeSets::default()` 不是钥匙）。
    #[test]
    fn inject_refuses_unrestricted_sets() {
        let err = inject(checked("SELECT * FROM orders").unwrap(), &ScopeSets::default(), &rules())
            .err()
            .unwrap();
        assert!(err.to_string().contains("ScopedSql::unrestricted"), "{err}");
    }

    #[test]
    fn proof_needs_both_evidences() {
        assert!(UnrestrictedProof::new(&ScopeSets::default(), true).is_some());
        // 集合无限制但身份不是超管/ALL → 不给
        assert!(UnrestrictedProof::new(&ScopeSets::default(), false).is_none());
        // 身份是超管但集合有限制（哨兵/具体集合）→ 不给
        assert!(UnrestrictedProof::new(&sets(&[7]), true).is_none());
    }

    #[test]
    fn unrestricted_passes_text_through_and_flags() {
        let c = checked("SELECT * FROM orders").unwrap();
        let text = c.text().to_string();
        let proof = UnrestrictedProof::new(&ScopeSets::default(), true).unwrap();
        let s = ScopedSql::unrestricted(c, &proof);
        assert_eq!(s.wire(), text);
        assert!(s.is_unrestricted());
    }

    /// 未登记表对受限用户 fail-closed（闸门不吞 `rewrite` 的错误）
    #[test]
    fn inject_propagates_fail_closed() {
        let err =
            inject(checked("SELECT * FROM nowhere").unwrap(), &sets(&[1]), &rules()).err().unwrap();
        assert!(err.to_string().contains("未在权限档案登记"), "{err}");
    }
}
