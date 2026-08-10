//! 权限来源 DMS 表的固定模板查询 —— policy 的**全部** IO 面。
//!
//! 表：`t_role_data_scope` / `t_employee_department` / `t_department` / `t_employee` /
//! `t_customer` / `t_dict_key`+`t_dict_value` / `t_employee_customer_group` /
//! `t_customer_contacts_info` / `t_customer_contacts_account` / `t_master_shop` / `t_config`。
//!
//! 每条 SQL 都是 `&'static str` 字面量，动态 `IN` 只有 `fixed(tpl).expand(n)` 一条路
//! （裁决 C1）：拼串的入口在编译期就不存在。逐行搬自 server/src/scope.rs:186-355。

use std::collections::HashSet;

use dms_connector::mysql::ReadOnlyMySql;

use crate::principal::Principal;

const GUEST_DISTRIBUTOR: &str =
    "SELECT config_value FROM t_config WHERE config_key = 'guest_distributor'";
const CUSTOMER_CONTACT_ACCOUNTS: &str =
    "SELECT contact_id, customer_code FROM t_customer_contacts_account
     WHERE customer_code IN (
       SELECT DISTINCT t.customer_code FROM t_customer_contacts_account t WHERE t.contact_id = ?
     )";
const SHOP_CONTACT_ACCOUNTS: &str =
    "SELECT customer_code, shop_code FROM t_customer_contacts_account
     WHERE contact_id = ? AND deleted_flag = 0";
const CONTACT_LOGINS_BY_CUSTOMERS: &str =
    "SELECT DISTINCT login_name FROM t_customer_contacts_account
     WHERE customer_code IN ({in}) AND deleted_flag = 0";
const SHOPS_BY_CUSTOMERS: &str =
    "SELECT DISTINCT shop_code FROM t_master_shop
     WHERE customer_code IN ({in}) AND status = 0 AND deleted_flag = 0";
const SHOPS_BY_CODES: &str =
    "SELECT DISTINCT shop_code FROM t_master_shop
     WHERE shop_code IN ({in}) AND status = 0 AND deleted_flag = 0";

/// 角色数据范围原始行 `(data_scope_type, view_type)` —— 基础档/定制档与缓存版本号的共同来源
pub async fn role_data_scope(mysql: &ReadOnlyMySql, role_id: i64) -> anyhow::Result<Vec<(i32, i32)>> {
    Ok(mysql
        .fixed("SELECT data_scope_type, view_type FROM t_role_data_scope WHERE role_id = ?")
        .bind(role_id)
        .fetch_all()
        .await?)
}

/// 游客默认经销商。DMS `ConfigService.selectByKey` 无 deleted/disabled 条件；重复或空值拒绝。
pub async fn guest_distributor_code(mysql: &ReadOnlyMySql) -> anyhow::Result<Option<String>> {
    let rows: Vec<(Option<String>,)> = mysql.fixed(GUEST_DISTRIBUTOR).fetch_all().await?;
    if rows.len() != 1 {
        return Ok(None);
    }
    Ok(rows.into_iter().next().and_then(|(v,)| v).filter(|v| !v.trim().is_empty()))
}

/// CustomerContacts.listByContactId：内外层都刻意不加 deleted/status，逐字保持 DMS 语义。
pub async fn customer_contact_accounts(
    mysql: &ReadOnlyMySql,
    contact_id: i64,
) -> anyhow::Result<Vec<(Option<i64>, Option<String>)>> {
    Ok(mysql
        .fixed(CUSTOMER_CONTACT_ACCOUNTS)
        .bind(contact_id)
        .fetch_all()
        .await?)
}

/// CustomerContacts.getEmployeeCodesByCurrentUser：同客户、未删除账号的登录名。
pub async fn contact_login_names_by_customers(
    mysql: &ReadOnlyMySql,
    customer_codes: &[String],
) -> anyhow::Result<Vec<String>> {
    fetch_str_by_str_in(mysql, CONTACT_LOGINS_BY_CUSTOMERS, customer_codes).await
}

/// ShopContacts：当前联系人未删除的客户/门店绑定；源码不检查 account.status。
pub async fn shop_contact_accounts(
    mysql: &ReadOnlyMySql,
    contact_id: i64,
) -> anyhow::Result<Vec<(Option<String>, Option<String>)>> {
    Ok(mysql.fixed(SHOP_CONTACT_ACCOUNTS).bind(contact_id).fetch_all().await?)
}

/// DefaultEmployee/CustomerContacts：客户范围内 status=0 且未删除的门店。
pub async fn active_shop_codes_by_customers(
    mysql: &ReadOnlyMySql,
    customer_codes: &[String],
) -> anyhow::Result<Vec<String>> {
    fetch_str_by_str_in(mysql, SHOPS_BY_CUSTOMERS, customer_codes).await
}

/// ShopContacts：显式绑定门店与有效门店主档取交集。
pub async fn active_shop_codes_by_codes(
    mysql: &ReadOnlyMySql,
    shop_codes: &[String],
) -> anyhow::Result<Vec<String>> {
    fetch_str_by_str_in(mysql, SHOPS_BY_CODES, shop_codes).await
}

/// 用户所属部门：任职表未删除行；无 → 主档部门 + 兼职部门（Java getDepartmentByUser L487-498）
pub async fn user_departments(mysql: &ReadOnlyMySql, p: &Principal) -> anyhow::Result<Vec<i64>> {
    let ids: Vec<(i64,)> = mysql
        .fixed(
            "SELECT department_id FROM t_employee_department WHERE employee_id = ? AND deleted_flag = 0",
        )
        .bind(p.employee_id)
        .fetch_all()
        .await?;
    let mut v: Vec<i64> = ids.into_iter().map(|(d,)| d).collect();
    if v.is_empty() {
        if let Some(d) = p.department_id {
            v.push(d);
        }
        let part_time: Option<(Option<i64>,)> = mysql
            .fixed(
                "SELECT part_time_department_id FROM t_employee
                 WHERE employee_id = ? AND deleted_flag = 0 AND disabled_flag = 0 LIMIT 1",
            )
            .bind(p.employee_id)
            .fetch_optional()
            .await?;
        if let Some((Some(d),)) = part_time {
            if !v.contains(&d) {
                v.push(d);
            }
        }
    }
    Ok(v)
}

/// 部门自身+全部子部门（Java DepartmentCacheManager：status=1 且 deleted_flag=0，按 parent_id 递归）
pub async fn self_and_children_departments(
    mysql: &ReadOnlyMySql,
    roots: &[i64],
) -> anyhow::Result<Vec<i64>> {
    let all: Vec<(i64, Option<i64>)> = mysql
        .fixed(
            "SELECT department_id, parent_id FROM t_department WHERE status = 1 AND deleted_flag = 0",
        )
        .fetch_all()
        .await?;
    Ok(dms_kernel::policy::scope::expand_department_tree(&all, roots))
}

/// 部门员工：主部门 OR 任职部门（任职行须 deleted=0 且 service_status=0，Java EmployeeMapper.xml L179-191）
pub async fn department_employee_ids(
    mysql: &ReadOnlyMySql,
    dept_ids: &[i64],
) -> anyhow::Result<Vec<i64>> {
    if dept_ids.is_empty() {
        return Ok(vec![]);
    }
    // 两个 `{in}` 用同一个 n（`expand` 展开模板里的每个标记），bind 顺序 = 占位符顺序
    let mut q = mysql
        .fixed(
            "SELECT DISTINCT t.employee_id FROM t_employee t
         INNER JOIN t_employee_department td
            ON td.employee_id = t.employee_id AND td.deleted_flag = 0 AND td.service_status = 0
         WHERE t.department_id IN ({in}) OR td.department_id IN ({in})",
        )
        .expand(dept_ids.len());
    for _ in 0..2 {
        for d in dept_ids {
            q = q.bind(d);
        }
    }
    Ok(q.fetch_all::<(i64,)>().await?.into_iter().map(|(i,)| i).collect())
}

/// 下属递归（含本人；任职行 deleted=0 且 service_status=0 按 manager_id 下钻）。
/// DMS 一旦在当前层发现任一环边，就保留本层新节点但停止继续下钻，避免异常关系扩大权限。
pub async fn subordinate_ids(mysql: &ReadOnlyMySql, user_id: i64) -> anyhow::Result<Vec<i64>> {
    let mut result: HashSet<i64> = HashSet::from([user_id]);
    let mut frontier: Vec<i64> = vec![user_id];
    while !frontier.is_empty() {
        let mut q = mysql
            .fixed(
                "SELECT DISTINCT employee_id FROM t_employee_department
             WHERE deleted_flag = 0 AND service_status = 0 AND manager_id IN ({in})",
            )
            .expand(frontier.len());
        for id in &frontier {
            q = q.bind(id);
        }
        let found: Vec<i64> = q.fetch_all::<(i64,)>().await?.into_iter().map(|(i,)| i).collect();
        let has_cycle = found.iter().any(|id| result.contains(id));
        result.extend(found.iter().copied());
        if has_cycle {
            break;
        }
        frontier = found;
    }
    Ok(result.into_iter().collect())
}

pub async fn login_names_by_ids(mysql: &ReadOnlyMySql, ids: &[i64]) -> anyhow::Result<Vec<String>> {
    fetch_str_in(mysql, "SELECT login_name FROM t_employee WHERE employee_id IN ({in})", ids).await
}

pub async fn actual_names_by_ids(mysql: &ReadOnlyMySql, ids: &[i64]) -> anyhow::Result<Vec<String>> {
    fetch_str_in(mysql, "SELECT actual_name FROM t_employee WHERE employee_id IN ({in})", ids).await
}

pub async fn customers_by_area_manager(
    mysql: &ReadOnlyMySql,
    ids: &[i64],
) -> anyhow::Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    fetch_str_in(
        mysql,
        "SELECT customer_code FROM t_customer WHERE deleted_flag = 0 AND area_manager_id IN ({in})",
        ids,
    )
    .await
}

/// 公用客户：字典三 key 的 value_code（Java getGeneralCustomerCodes L173-190）
pub async fn common_customer_codes(mysql: &ReadOnlyMySql) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String,)> = mysql
        .fixed(
            "SELECT DISTINCT v.value_code FROM t_dict_value v
         JOIN t_dict_key k ON k.dict_key_id = v.dict_key_id
         WHERE k.key_code IN ('payment_customer_for_inside','payment_customer_for_all','payment_customer_for_yiming')
           AND k.deleted_flag = 0 AND v.deleted_flag = 0",
        )
        .fetch_all()
        .await?;
    Ok(rows.into_iter().map(|(s,)| s).collect())
}

/// 102 客户分组：FIND_IN_SET(员工组码, 客户.customer_group)（Java EmployeeCustomerGroupMapper.xml L80-93）
pub async fn group_customer_codes(
    mysql: &ReadOnlyMySql,
    employee_ids: &[i64],
) -> anyhow::Result<Vec<String>> {
    if employee_ids.is_empty() {
        return Ok(vec![]);
    }
    let mut q = mysql
        .fixed(
            "SELECT DISTINCT tc.customer_code FROM t_customer tc
         WHERE EXISTS (SELECT 1 FROM t_employee_customer_group t
                       WHERE t.employee_id IN ({in})
                         AND FIND_IN_SET(t.customer_group, tc.customer_group) > 0)",
        )
        .expand(employee_ids.len());
    for id in employee_ids {
        q = q.bind(id);
    }
    Ok(q.fetch_all::<(String,)>().await?.into_iter().map(|(s,)| s).collect())
}

/// 103 客户团队：contact_name IN 姓名 且 contact_type IN ('Y1'负责人,'Y3'团队成员)（Java L137-151）
pub async fn manager_customer_codes(
    mysql: &ReadOnlyMySql,
    names: &[String],
) -> anyhow::Result<Vec<String>> {
    if names.is_empty() {
        return Ok(vec![]);
    }
    let mut q = mysql
        .fixed(
            "SELECT DISTINCT customer_code FROM t_customer_contacts_info
         WHERE deleted_flag = 0 AND contact_type IN ('Y1','Y3') AND contact_name IN ({in})",
        )
        .expand(names.len());
    for n in names {
        q = q.bind(n.as_str());
    }
    Ok(q
        .fetch_all::<(String,)>()
        .await?
        .into_iter()
        .map(|(s,)| s)
        .filter(|s| !s.trim().is_empty())
        .collect())
}

/// 单列字符串取数的公用体：`sql` 必是 `&'static str` 模板（含 `{in}`），值全走 bind。
async fn fetch_str_in(
    mysql: &ReadOnlyMySql,
    sql: &'static str,
    ids: &[i64],
) -> anyhow::Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let mut q = mysql.fixed(sql).expand(ids.len());
    for id in ids {
        q = q.bind(id);
    }
    Ok(q.fetch_all::<(String,)>().await?.into_iter().map(|(s,)| s).collect())
}

/// 字符串 `IN` 的固定模板版本；空白结果不能形成可用权限标识，直接丢弃。
async fn fetch_str_by_str_in(
    mysql: &ReadOnlyMySql,
    sql: &'static str,
    values: &[String],
) -> anyhow::Result<Vec<String>> {
    if values.is_empty() {
        return Ok(vec![]);
    }
    let mut q = mysql.fixed(sql).expand(values.len());
    for value in values {
        q = q.bind(value.as_str());
    }
    Ok(q
        .fetch_all::<(Option<String>,)>()
        .await?
        .into_iter()
        .filter_map(|(s,)| s)
        .filter(|s| !s.trim().is_empty())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_role_queries_preserve_dms_filters() {
        assert!(!CUSTOMER_CONTACT_ACCOUNTS.contains("deleted_flag"));
        assert!(!CUSTOMER_CONTACT_ACCOUNTS.contains("status"));
        assert!(SHOP_CONTACT_ACCOUNTS.contains("deleted_flag = 0"));
        assert!(!SHOP_CONTACT_ACCOUNTS.contains("status"));
        for sql in [SHOPS_BY_CUSTOMERS, SHOPS_BY_CODES] {
            assert!(sql.contains("status = 0") && sql.contains("deleted_flag = 0"));
        }
        assert!(GUEST_DISTRIBUTOR.contains("config_key = 'guest_distributor'"));
    }
}
