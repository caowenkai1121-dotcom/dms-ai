//! 表权限档案类型与查表容器（纯数据，零 IO、零 DMS 语料）。
//! 类型逐行搬自 server/src/inject.rs:19-44；DMS 表的 `builtin_rules()`（表名+列名）
//! 与 PG 注册表加载留在 IO 侧（T5 后归 `dms-policy` 的 `builtin.rs`/`rules.rs`）。
//! `TableRule` 只三臂：`Cond` 变体按 docs/ARCHITECTURE.md §8 推迟到第一个真实第三方源。

use std::collections::HashMap;

/// 表绑定：该表用哪些列吃权限条件（对应 Java @DataScope joinSql 模板，逐条探库核实）
#[derive(Clone)]
pub struct Binding {
    pub customer_col: Option<String>,
    pub customer_kind: CustomerKind,
    pub owner_col: Option<String>,
    pub owner_kind: OwnerKind,
}

#[derive(PartialEq, Clone, Copy)]
pub enum CustomerKind {
    /// DMS 通用 `#customerCodes`。
    Codes,
    /// DMS 通用 `#customerCodes`；受限身份缺少客户集合时必须恒假。
    RequiredCodes,
    /// 仅 `area_manager_id IN #employeeIds` 派生的客户，不含公用/分组/团队客户。
    ManagerCodes,
    /// 独立门店编码集合；用于不能按客户编码放大的门店联系人权限。
    ShopCodes,
}

#[derive(PartialEq, Clone, Copy)]
pub enum OwnerKind {
    /// 数字 employee_id（#employeeIds）
    Ids,
    /// 登录名字符串（#employeeCodes）
    Codes,
    /// 仅当前登录名，不含数据范围中的下属登录名。
    Login,
}

/// 表权限档案（fail-closed 三态）：
/// - Scoped：注入 Java joinSql 等价条件
/// - Global：Java 无 @DataScope，1:1 审定全量可见，免注入
/// - Via：明细/从表独查时借头表条件（EXISTS 半连接）；头表同 SELECT 在场则跳过
/// 未登记的表对受限用户一律拒绝（fail-closed）。
#[derive(Clone)]
pub enum TableRule {
    Scoped(Binding),
    Global,
    Via { table: String, local_col: String, remote_col: String },
}

/// 档案快照：注入算法只认传进来的这一份（裁决 C2 —— 三参 inject，不读全局注册表）。
/// 构造走 `RuleSet::from(HashMap)`，map 私有 = 算法侧无法就地改档案。
#[derive(Clone, Default)]
pub struct RuleSet {
    map: HashMap<String, TableRule>,
}

impl RuleSet {
    /// 查表权限档案（未登记返 None，由调用方 fail-closed 拒绝）
    pub fn rule_of(&self, table: &str) -> Option<TableRule> {
        self.map.get(table).cloned()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl From<HashMap<String, TableRule>> for RuleSet {
    fn from(map: HashMap<String, TableRule>) -> Self {
        Self { map }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scoped(owner: &str, kind: OwnerKind) -> TableRule {
        TableRule::Scoped(Binding {
            customer_col: Some("cust_code".into()),
            customer_kind: CustomerKind::Codes,
            owner_col: Some(owner.into()),
            owner_kind: kind,
        })
    }

    #[test]
    fn ruleset_lookup_and_size() {
        let mut m: HashMap<String, TableRule> = HashMap::new();
        m.insert("orders".into(), scoped("owner_id", OwnerKind::Ids));
        m.insert("goods".into(), TableRule::Global);
        let rs = RuleSet::from(m);
        assert_eq!(rs.len(), 2);
        assert!(!rs.is_empty());
        assert!(matches!(rs.rule_of("goods"), Some(TableRule::Global)));
        assert!(matches!(rs.rule_of("orders"), Some(TableRule::Scoped(_))));
        // 未登记 → None（拒绝与否由注入算法裁决）
        assert!(rs.rule_of("nowhere").is_none());
    }

    #[test]
    fn empty_ruleset() {
        let rs = RuleSet::default();
        assert!(rs.is_empty());
        assert_eq!(rs.len(), 0);
        assert!(rs.rule_of("orders").is_none());
    }
}
