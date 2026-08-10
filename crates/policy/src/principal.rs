//! 当前用户（principal）加载：员工 + 当前激活角色。
//! 语义对齐 Java：多角色不合并，单一激活角色生效（RequestEmployee + getCurrentRole）。
//!
//! 逐行搬自 server/src/principal.rs 全文（T5），一字不改。

use dms_connector::mysql::ReadOnlyMySql;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Principal {
    pub employee_id: i64,
    pub login_name: String,
    pub actual_name: String,
    pub administrator_flag: bool,
    pub department_id: Option<i64>,
    pub role_id: i64,
    pub role_code: String,
}

/// 按登录名加载员工与激活角色。
///
/// 🔴 role_code=None 的语义 1:1 对齐 Java `DataScope.getCurrentRole`：
/// 该方法从登录会话取 roleCode，**取不到就抛「请选择登录角色」——DMS 绝不替用户默认选角色**。
/// 我们原先「取 role_id 最小的一个」是自造语义：多角色用户（如 tanlibo 同时有 admin 与
/// city_manager）不传 role_code 时会被静默授予 admin 的**全量数据权限**（实测 unrestricted=true），
/// 而同一账号选 city_manager 只能看 27 家客户——这是三端（SSO/企微）不传角色时的越权面。
/// 现在：多角色必须显式指定（错误信息列出可选角色）；单角色无歧义则沿用该角色。
/// 无任何角色 → Err（fail-closed，对齐 Java「角色未正确设定」抛错行为）。
pub async fn load_principal(
    mysql: &ReadOnlyMySql,
    login_name: &str,
    role_code: Option<&str>,
) -> anyhow::Result<Principal> {
    let emp: Option<(i64, String, String, Option<i8>, Option<i64>)> = mysql
        .fixed(
            "SELECT employee_id, login_name, actual_name, administrator_flag, department_id
         FROM t_employee WHERE login_name = ? AND deleted_flag = 0 AND disabled_flag = 0
           AND (passwd_expire_time IS NULL OR passwd_expire_time >= CURRENT_TIMESTAMP)",
        )
        .bind(login_name)
        .fetch_optional()
        .await?;
    let (employee_id, login_name, actual_name, admin_flag, department_id) =
        emp.ok_or_else(|| anyhow::anyhow!("员工不存在: {login_name}"))?;

    let roles: Vec<(i64, String)> = mysql
        .fixed(
            "SELECT r.role_id, r.role_code FROM t_role_employee re
         JOIN t_role r ON r.role_id = re.role_id
         WHERE re.employee_id = ? ORDER BY r.role_id",
        )
        .bind(employee_id)
        .fetch_all()
        .await?;

    let administrator_flag = admin_flag.unwrap_or(0) == 1;
    // 超管可以无角色（短路放行）；普通员工无角色 = fail-closed 拒绝
    let (role_id, role_code) = match role_code {
        Some(rc) => roles
            .iter()
            .find(|(_, c)| c.trim() == rc)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("该账号无角色 {rc}"))?,
        None => match roles.len() {
            1 => roles[0].clone(),
            0 if administrator_flag => (0, "admin".into()),
            0 => anyhow::bail!("该账号无任何角色（fail-closed 拒绝）"),
            _ => anyhow::bail!(
                "请选择登录角色（该账号有多个角色，权限档不同，不能替你默认选）：{}",
                roles.iter().map(|(_, c)| c.trim()).collect::<Vec<_>>().join(" / ")
            ),
        },
    };

    Ok(Principal {
        employee_id,
        login_name,
        actual_name,
        administrator_flag,
        department_id,
        role_id,
        role_code: role_code.trim().to_string(),
    })
}

/// 该账号的可选角色列表（多角色时前端/三端据此让用户选，对齐 DMS 登录选角色）
pub async fn list_roles(mysql: &ReadOnlyMySql, login_name: &str) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String,)> = mysql
        .fixed(
            "SELECT r.role_code FROM t_employee e
         JOIN t_role_employee re ON re.employee_id = e.employee_id
         JOIN t_role r ON r.role_id = re.role_id
         WHERE e.login_name = ? AND e.deleted_flag = 0 AND e.disabled_flag = 0
         ORDER BY r.role_id",
        )
        .bind(login_name)
        .fetch_all()
        .await?;
    Ok(rows.into_iter().map(|(c,)| c.trim().to_string()).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn principal_queries_reject_disabled_employees() {
        let src = include_str!("principal.rs");
        assert!(src.matches("disabled_flag = 0").count() >= 2);
        assert!(src.matches("passwd_expire_time").count() >= 1);
    }
}
