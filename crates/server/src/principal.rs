//! 当前用户（principal）加载：员工 + 当前激活角色。
//! 语义对齐 Java：多角色不合并，单一激活角色生效（RequestEmployee + getCurrentRole）。

use serde::Serialize;
use sqlx::MySqlPool;

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

/// 按登录名加载员工与激活角色。role_code=None 时取该员工第一个角色（role_id 升序）。
/// 无任何角色 → Err（fail-closed，对齐 Java「角色未正确设定」抛错行为）。
pub async fn load_principal(
    mysql: &MySqlPool,
    login_name: &str,
    role_code: Option<&str>,
) -> anyhow::Result<Principal> {
    let emp: Option<(i64, String, String, Option<i8>, Option<i64>)> = sqlx::query_as(
        "SELECT employee_id, login_name, actual_name, administrator_flag, department_id
         FROM t_employee WHERE login_name = ? AND deleted_flag = 0",
    )
    .bind(login_name)
    .fetch_optional(mysql)
    .await?;
    let (employee_id, login_name, actual_name, admin_flag, department_id) =
        emp.ok_or_else(|| anyhow::anyhow!("员工不存在: {login_name}"))?;

    let roles: Vec<(i64, String)> = sqlx::query_as(
        "SELECT r.role_id, r.role_code FROM t_role_employee re
         JOIN t_role r ON r.role_id = re.role_id
         WHERE re.employee_id = ? ORDER BY r.role_id",
    )
    .bind(employee_id)
    .fetch_all(mysql)
    .await?;

    let administrator_flag = admin_flag.unwrap_or(0) == 1;
    // 超管可以无角色（短路放行）；普通员工无角色 = fail-closed 拒绝
    let (role_id, role_code) = match role_code {
        Some(rc) => roles
            .iter()
            .find(|(_, c)| c.trim() == rc)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("该账号无角色 {rc}"))?,
        None => match roles.first().cloned() {
            Some(r) => r,
            None if administrator_flag => (0, "admin".into()),
            None => anyhow::bail!("该账号无任何角色（fail-closed 拒绝）"),
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
