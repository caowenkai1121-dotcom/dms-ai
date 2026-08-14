//! 数据权限集合计算的**编排**：1:1 复刻 DMS Java DefaultEmployee.java 语义。
//! 权威源码：infrastructure/.../common/service/impl/DefaultEmployee.java（本文件注释引用其行号）。
//! 纯裁决算法在 `dms_kernel::policy::scope`，连库查询在 `crate::dms_tables`，这里只剩顺序。
//!
//! 关键语义（与 Java 逐条对齐）：
//! - 超管(administrator_flag)/admin 角色 → 全部集合为空 = 不限制（短路）。
//! - visitor/customer_contact/shop_contact 由 DataScopeManager 独立策略接管，先于 t_role_data_scope 分流。
//! - 基础档(type=1)取 view_type 最大的一行：0本人/1本部门/2本部门及下级/3结算客户(哨兵-1)/10全部(空=不限制且整体短路)。
//! - 无 type=1 行 → defaultEmployeeIds 为空 → 整体短路不限制（Java L281-292 else 分支）。
//! - 定制档(type=2)：101下属(递归含本人) / 102客户分组(FIND_IN_SET) / 103客户团队(contact_name=姓名, contact_type IN Y1,Y3)。
//! - 哨兵：集合=[-1] 表示拒绝(0行)；集合为空 = 该维度不注入 = 放行。二者语义相反。
//! - customer_codes = 基础客户 + 公用客户 + 102 + 103 + 下属客户，各段 -1 则跳过并标旗，最终为空且有旗 → ["-1"]。
//!
//! 🔴 **段序 base→common→102→103→下属 不许重排**（D9）：它决定 `merge_customer_codes` 的落旗结果。

use dms_connector::mysql::ReadOnlyMySql;
use dms_connector::SqlSource;

use crate::cache;
use crate::dms_tables as t;
use crate::principal::Principal;

// 纯裁决算法在 kernel：re-export 让 46 断言与下游调用方只有一条 use 路径。
pub use dms_kernel::policy::scope::{
    decide_base, dedup_i64, dedup_str, expand_department_tree, merge_customer_codes, merge_employee_ids,
    BaseDecision, ScopeSets, SENTINEL,
};

/// 字符串形态的拒绝哨兵（kernel 只导出 i64 版 `SENTINEL`）：全文件统一走这个常量
const SENTINEL_STR: &str = "-1";

/// 权限计算结果 = 集合 + **凭什么无限制**的出处。
///
/// 全部集合为空有且只有两个来源，二者都由角色档决定：超管短路（`admin_shortcut`）
/// 或基础档 `BaseDecision::Unrestricted`（view_type=10 / 无 type=1 行）。
/// 把这个来源单独带出来，是为了让 `UnrestrictedProof` 的第二个证据来自**身份**，
/// 而不是把 `sets` 再喂回去自证——自证的证据等于没有证据：
/// `ScopeSets::default()` 会把自己证成「有资格看全部」，F2 的闸门就白装了。
///
/// 字段私有：生产路径只能由 `compute_scope` 产出。`new()` 因跨 crate 消费者（server 的闸门
/// 单测与 CLI 管理任务）必须 pub，`ponytail:` 天花板 —— 谁都能硬写 `new(default(), true)`
/// 撒谎，但那是一行显式的、`grep 'Scope::new'` 就能列全的谎。
#[derive(Clone)]
pub struct Scope {
    sets: ScopeSets,
    unrestricted_by_role: bool,
    device_unrestricted_by_role: bool,
    /// `t_account_bill_header.manager` 兼容历史数据：同一列既可能存员工 ID，
    /// 也可能存员工姓名。姓名只由已授权 employee_ids 单表点查派生，不能由请求输入。
    manager_names: Vec<String>,
}

impl Scope {
    /// 天花板（见 struct 注释）：谁都能硬写 `new(default(), true)` 撒谎，
    /// 但那是一行显式的、`grep 'Scope::new'` 就能列全的谎。
    pub fn new(sets: ScopeSets, unrestricted_by_role: bool) -> Self {
        Self {
            sets,
            unrestricted_by_role,
            device_unrestricted_by_role: false,
            manager_names: vec![],
        }
    }
    pub fn sets(&self) -> &ScopeSets {
        &self.sets
    }
    /// 该身份的角色档是否授予「全部数据」（超管 或 基础档 ALL）
    pub fn unrestricted_by_role(&self) -> bool {
        self.unrestricted_by_role
    }
    /// `t_account_bill_header.manager` 双形态里的姓名侧（由已授权 employee_ids 派生）
    pub fn manager_names(&self) -> &[String] {
        &self.manager_names
    }
    /// 设备订单页的两个专职角色例外（xiaoyunbp/shebeiyy 全量放行证明）
    pub fn device_unrestricted_by_role(&self) -> bool {
        self.device_unrestricted_by_role
    }

    fn with_manager_names(mut self, manager_names: Vec<String>) -> Self {
        self.manager_names = manager_names;
        self
    }

    fn with_device_unrestricted(mut self, unrestricted: bool) -> Self {
        self.device_unrestricted_by_role = unrestricted;
        self
    }
}

/// 放行来源①：身份（Java L93-98, L236-243）。两个入口共用同一条规则，不留第二份判定。
fn admin_shortcut(p: &Principal) -> Option<Scope> {
    (p.administrator_flag || p.role_code == "admin")
        .then(|| Scope::new(ScopeSets::default(), true))
}

/// 带缓存的计算（F7：TTL 15 分钟 + key 带 `scope_ver`/`DsId`）。
/// `t_role_data_scope` 这一条查询是「自愈」的代价：DMS 侧改权限配置 → 版本号变 →
/// 下一次查询即视为未命中重算，不需要任何外部通知源。
pub async fn compute_scope_cached(mysql: &ReadOnlyMySql, p: &Principal) -> anyhow::Result<Scope> {
    if let Some(s) = admin_shortcut(p) {
        return Ok(s);
    }
    // DMS DataScopeManager L109-115：三类特殊角色不读取 t_role_data_scope。
    // 不进 15 分钟通用缓存，账号绑定撤销后下一次请求立即收紧。
    if let Some(s) = compute_special_scope(mysql, p).await? {
        return Ok(s);
    }
    let rows = t::role_data_scope(mysql, p.role_id).await?;
    let key = cache::key(p, mysql.ds_id(), &rows);
    if let Some(hit) = cache::get(&key) {
        return Ok(hit);
    }
    let scope = compute_from_rows(mysql, p, &rows).await?;
    cache::put(key, &scope);
    Ok(scope)
}

pub async fn compute_scope(mysql: &ReadOnlyMySql, p: &Principal) -> anyhow::Result<Scope> {
    if let Some(s) = admin_shortcut(p) {
        return Ok(s);
    }
    if let Some(s) = compute_special_scope(mysql, p).await? {
        return Ok(s);
    }
    let rows = t::role_data_scope(mysql, p.role_id).await?;
    compute_from_rows(mysql, p, &rows).await
}

#[derive(Clone, Copy, Debug, PartialEq)]
/// DMS 三类特殊角色（由 DataScopeManager 独立策略接管，先于 t_role_data_scope 分流）；
/// 空角色归 visitor。语义见 `special_role`。
enum SpecialRole {
    Visitor,
    CustomerContact,
    ShopContact,
}

/// DMS 空角色同样走 visitor；Principal 已 trim，故这里只需处理空串。
fn special_role(role_code: &str) -> Option<SpecialRole> {
    match role_code {
        "" | "visitor" => Some(SpecialRole::Visitor),
        "customer_contact" => Some(SpecialRole::CustomerContact),
        "shop_contact" => Some(SpecialRole::ShopContact),
        _ => None,
    }
}

async fn compute_special_scope(
    mysql: &ReadOnlyMySql,
    p: &Principal,
) -> anyhow::Result<Option<Scope>> {
    let Some(role) = special_role(&p.role_code) else {
        return Ok(None);
    };
    let sets = match role {
        SpecialRole::Visitor => visitor_scope(mysql, p).await?,
        SpecialRole::CustomerContact => customer_contact_scope(mysql, p).await?,
        SpecialRole::ShopContact => shop_contact_scope(mysql, p).await?,
    };
    Ok(Some(scope_with_manager_names(mysql, sets, false, false).await?))
}

async fn scope_with_manager_names(
    mysql: &ReadOnlyMySql,
    sets: ScopeSets,
    unrestricted_by_role: bool,
    device_unrestricted_by_role: bool,
) -> anyhow::Result<Scope> {
    let employee_ids: Vec<i64> = sets
        .employee_ids
        .iter()
        .copied()
        .filter(|id| *id != SENTINEL)
        .collect();
    let manager_names = if employee_ids.is_empty() {
        vec![]
    } else {
        clean_strings(t::actual_names_by_ids(mysql, &employee_ids).await?)
    };
    Ok(Scope::new(sets, unrestricted_by_role)
        .with_manager_names(manager_names)
        .with_device_unrestricted(device_unrestricted_by_role))
}

async fn visitor_scope(mysql: &ReadOnlyMySql, p: &Principal) -> anyhow::Result<ScopeSets> {
    let customer_codes = deny_empty_strings(
        t::guest_distributor_code(mysql).await?.into_iter().collect(),
    );
    Ok(ScopeSets {
        employee_ids: vec![SENTINEL],
        employee_codes: vec![SENTINEL_STR.into()],
        customer_codes,
        // 登录名维度仍供其它已登记策略使用；空登录名改哨兵，不能退化成无条件。
        login_names: deny_empty_strings(vec![p.login_name.clone()]),
        manager_customer_codes: vec![],
        // Visitor.getShopByCurrentUser 明确返回空集合。
        shop_codes: vec![SENTINEL_STR.into()],
    })
}

async fn customer_contact_scope(
    mysql: &ReadOnlyMySql,
    p: &Principal,
) -> anyhow::Result<ScopeSets> {
    let accounts = t::customer_contact_accounts(mysql, p.employee_id).await?;
    let employee_ids = deny_empty_ids(accounts.iter().filter_map(|(id, _)| *id).collect());
    let customer_codes = deny_empty_strings(
        accounts.iter().filter_map(|(_, code)| code.clone()).collect(),
    );
    let employee_codes = if has_real_codes(&customer_codes) {
        deny_empty_strings(t::contact_login_names_by_customers(mysql, &customer_codes).await?)
    } else {
        vec![SENTINEL_STR.into()]
    };
    let manager_customer_codes = clean_strings(
        t::customers_by_area_manager(mysql, &employee_ids).await?,
    );
    let shop_codes = shops_for_customer_scope(mysql, &customer_codes).await?;
    Ok(ScopeSets {
        employee_ids,
        employee_codes,
        customer_codes,
        login_names: deny_empty_strings(vec![p.login_name.clone()]),
        manager_customer_codes,
        shop_codes,
    })
}

async fn shop_contact_scope(
    mysql: &ReadOnlyMySql,
    p: &Principal,
) -> anyhow::Result<ScopeSets> {
    let accounts = t::shop_contact_accounts(mysql, p.employee_id).await?;
    let customer_codes = deny_empty_strings(
        accounts.iter().filter_map(|(customer, _)| customer.clone()).collect(),
    );
    let bound_shop_codes = clean_strings(
        accounts.iter().filter_map(|(_, shop)| shop.clone()).collect(),
    );
    let shop_codes = if bound_shop_codes.is_empty() {
        vec![SENTINEL_STR.into()]
    } else {
        deny_empty_strings(t::active_shop_codes_by_codes(mysql, &bound_shop_codes).await?)
    };
    let employee_ids = vec![p.employee_id];
    let manager_customer_codes = clean_strings(
        t::customers_by_area_manager(mysql, &employee_ids).await?,
    );
    let login_names = deny_empty_strings(vec![p.login_name.clone()]);
    Ok(ScopeSets {
        employee_ids,
        employee_codes: login_names.clone(),
        customer_codes,
        login_names,
        manager_customer_codes,
        shop_codes,
    })
}

/// 普通角色/客户联系人按客户取有效门店。真空客户集合对普通 DMS 策略表示不限制；
/// 哨兵或有客户却查不到门店则必须保留拒绝哨兵，不能让空集合反向变成全量。
///
/// 🔴 **读失败退化成拒绝哨兵，而不是整轮失败**（2026-08-14 回归 F04）：
/// `SELECT DISTINCT shop_code FROM t_master_shop WHERE customer_code IN (27 个) …`
/// 在生产 MySQL 上要 4s，撞 `max_statement_time` 返 3024 —— 于是 `city_manager`
/// **一句话都问不了**（算权限就挂了），而门店只是他众多可查对象里的一个维度。
///
/// 退化方向是 fail-closed 的：哨兵 = 该用户看不到任何门店行（`t_master_shop` 那一族查询返空），
/// 其余表照常按客户/员工集合注入。**不许**改成空集合 —— `ShopCodes` 臂空集不注入 ＝ 整表可见。
/// 这条退化是给「库慢/抖」兜底，不是给「查出来就是空」用：后者本来就走 `deny_empty_strings`。
async fn shops_for_customer_scope(
    mysql: &ReadOnlyMySql,
    customer_codes: &[String],
) -> anyhow::Result<Vec<String>> {
    if customer_codes.is_empty() {
        return Ok(vec![]);
    }
    if !has_real_codes(customer_codes) {
        return Ok(vec![SENTINEL_STR.into()]);
    }
    match t::active_shop_codes_by_customers(mysql, customer_codes).await {
        Ok(codes) => Ok(deny_empty_strings(codes)),
        Err(e) => {
            tracing::warn!(err = %e, "门店集合读取失败 → 该维度按拒绝哨兵处理（其余维度照常）");
            Ok(vec![SENTINEL_STR.into()])
        }
    }
}

fn has_real_codes(codes: &[String]) -> bool {
    codes.iter().any(|code| code != SENTINEL_STR && !code.trim().is_empty())
}

fn deny_empty_ids(ids: Vec<i64>) -> Vec<i64> {
    let ids = dedup_i64(ids);
    if ids.is_empty() { vec![SENTINEL] } else { ids }
}

fn clean_strings(mut values: Vec<String>) -> Vec<String> {
    values.retain(|value| !value.trim().is_empty());
    dedup_str(values)
}

fn deny_empty_strings(values: Vec<String>) -> Vec<String> {
    let values = clean_strings(values);
    if values.is_empty() { vec![SENTINEL_STR.into()] } else { values }
}

fn device_full_scope_role(role_code: &str) -> bool {
    matches!(role_code, "xiaoyunbp" | "shebeiyy")
}

/// 编排：基础档 → 101 下属 → 权限维度合并。查询顺序与 Java DefaultEmployee 逐条等同
/// （原 server/src/scope.rs:85-183，该文件已删除，本文件即迁移后的落点）。
async fn compute_from_rows(
    mysql: &ReadOnlyMySql,
    p: &Principal,
    rows: &[(i32, i32)],
) -> anyhow::Result<Scope> {
    if rows.is_empty() {
        // Java L275: 抛「当前登录用户角色未正确设定[角色-数据范围]」→ fail-closed
        anyhow::bail!("当前登录用户角色未正确设定[角色-数据范围]");
    }
    let mut base_rows: Vec<i32> = Vec::new();
    let mut custom: Vec<i32> = Vec::new();
    for &(t, v) in rows {
        if t == 1 {
            base_rows.push(v);
        } else if t == 2 {
            custom.push(v);
        }
    }

    // 放行来源②：基础档 ALL（view_type=10）或无 type=1 行
    let Some(base) = base_ids(mysql, p, &base_rows).await? else {
        return Ok(Scope::new(ScopeSets::default(), true));
    };
    let sub = sub_ids(mysql, p, &custom).await?;
    let x = Parts { p, base: &base, sub: &sub, custom: &custom };

    // employee_ids 合并（Java getDefaultUserListWithRoleDataScope L382-410）
    // 受限档若异常算不出员工，必须保留恒假哨兵；否则 owner-only 规则会因空段不注入而放大全表。
    let employee_ids = deny_empty_ids(merge_employee_ids(&base, &sub));
    let employee_codes = employee_codes(mysql, &x).await?;
    let customer_codes = customer_codes(mysql, &x).await?;
    let shop_codes = shops_for_customer_scope(mysql, &customer_codes).await?;
    // DMS 设备订单页只对两个专职角色放开全量。例外单独保存在 Scope 布尔证明中；
    // 不能靠清空通用集合表达，否则持久化自定义规则可能因空段不注入而被意外放大。
    let device_full = device_full_scope_role(&p.role_code);
    let manager_customer_codes = t::customers_by_area_manager(mysql, &employee_ids).await?;
    // 走到这里必是受限档（放行的两个来源都已在上面 return），故 unrestricted_by_role = false
    scope_with_manager_names(mysql, ScopeSets {
        employee_ids,
        employee_codes,
        customer_codes,
        // login_name 来自 DB（load_principal 查出），非空 —— 故不似它处再包 deny_empty_strings
        login_names: vec![p.login_name.clone()],
        manager_customer_codes: clean_strings(manager_customer_codes),
        shop_codes,
    }, false, device_full).await
}

/// 基础档（type=1）→ 员工 id 集合。`None` = 整体短路不限制（Java L281-292 / L394-395 / L580-581）。
/// decide_base 返 PolicyError（kernel 错误契约），文案逐字不变，转 anyhow 保持调用侧行为。
async fn base_ids(
    mysql: &ReadOnlyMySql,
    p: &Principal,
    base_rows: &[i32],
) -> anyhow::Result<Option<Vec<i64>>> {
    let ids = match decide_base(base_rows).map_err(anyhow::Error::from)? {
        BaseDecision::Unrestricted => return Ok(None),
        // Me 走占位空集分支：由此处填本人 id（纯函数不碰 principal）
        BaseDecision::Ids(ids) if ids.is_empty() => vec![p.employee_id],
        BaseDecision::Ids(ids) => ids,
        BaseDecision::Departments { with_children } => {
            let depts = t::user_departments(mysql, p).await?;
            let scope_depts = if with_children {
                t::self_and_children_departments(mysql, &depts).await?
            } else {
                depts
            };
            t::department_employee_ids(mysql, &scope_depts).await?
        }
    };
    Ok(Some(ids))
}

/// 定制档 101 下属（递归含本人，Java L458-489）。空 → [-1]；但起点恒含本人，实际不会为空。
async fn sub_ids(mysql: &ReadOnlyMySql, p: &Principal, custom: &[i32]) -> anyhow::Result<Vec<i64>> {
    if !custom.contains(&101) {
        return Ok(vec![]);
    }
    let ids = t::subordinate_ids(mysql, p.employee_id).await?;
    Ok(if ids.is_empty() { vec![SENTINEL] } else { ids })
}

/// 两个合并段的共享状态（D4：拆函数同时拆参数，不把 5 个形参连排）
struct Parts<'a> {
    p: &'a Principal,
    base: &'a [i64],
    sub: &'a [i64],
    custom: &'a [i32],
}

/// employee_codes：基础+下属的 login_name（Java L528-565，-1 语义取净化版）
async fn employee_codes(mysql: &ReadOnlyMySql, x: &Parts<'_>) -> anyhow::Result<Vec<String>> {
    let mut out: Vec<String> = vec![];
    let mut codes_flag = true;
    if x.base.contains(&SENTINEL) {
        codes_flag = false;
    } else if !x.base.is_empty() {
        out.extend(t::login_names_by_ids(mysql, x.base).await?);
    }
    if !x.sub.is_empty() {
        if x.sub.contains(&SENTINEL) {
            codes_flag = false;
        } else {
            out.extend(t::login_names_by_ids(mysql, x.sub).await?);
        }
    }
    if out.is_empty() && !codes_flag {
        out.push(SENTINEL_STR.into());
    }
    Ok(dedup_str(out))
}

/// customer_codes（Java getCustomerCodesByCurrentUser L568-621）。
/// bool = 该段「必须有值否则落旗」（落旗段查空 → 最终 ["-1"] 拒绝）。**段序即行为，不许重排**。
async fn customer_codes(mysql: &ReadOnlyMySql, x: &Parts<'_>) -> anyhow::Result<Vec<String>> {
    let mut segs: Vec<(Vec<String>, bool)> = vec![
        // 1. 基础客户：area_manager_id IN 基础ids（含哨兵时 IN(-1) 自然为空，与 Java 一致）
        (t::customers_by_area_manager(mysql, x.base).await?, false),
        // 2. 公用客户（inside + all + yiming 三组字典）
        (t::common_customer_codes(mysql).await?, false),
    ];
    // 3. 定制 102 客户分组
    if x.custom.contains(&102) {
        segs.push((t::group_customer_codes(mysql, &[x.p.employee_id]).await?, true));
    }
    // 4. 定制 103 客户团队（contact_name = 姓名）
    if x.custom.contains(&103) {
        segs.push((t::manager_customer_codes(mysql, std::slice::from_ref(&x.p.actual_name)).await?, true));
    }
    // 5. 下属为团队成员/分组的客户（Java addSubordinateToCustomerManager L337-359）
    if x.custom.contains(&101) && !x.sub.is_empty() && !x.sub.contains(&SENTINEL) {
        let names = t::actual_names_by_ids(mysql, x.sub).await?;
        let mut sc = t::manager_customer_codes(mysql, &names).await?;
        sc.extend(t::group_customer_codes(mysql, x.sub).await?);
        segs.push((sc, true));
    }
    Ok(merge_customer_codes(&segs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dms_special_roles_are_exact_and_empty_role_is_visitor() {
        assert_eq!(special_role(""), Some(SpecialRole::Visitor));
        assert_eq!(special_role("visitor"), Some(SpecialRole::Visitor));
        assert_eq!(special_role("customer_contact"), Some(SpecialRole::CustomerContact));
        assert_eq!(special_role("shop_contact"), Some(SpecialRole::ShopContact));
        assert_eq!(special_role("city_manager"), None);
    }

    /// 🔴 门店读失败必须退化成**拒绝哨兵**，不能整轮失败、更不能退成空集合。
    ///
    /// 空集合会让 `ShopCodes` 臂一个段都不 push ＝ `t_master_shop` 整表可见（fail-open）；
    /// 整轮失败则是 2026-08-14 回归 F04 的现象：生产库 3024 一次，`city_manager` 一句话都问不了。
    #[test]
    fn shop_read_failure_degrades_to_deny_not_to_full_access() {
        let src = include_str!("scope.rs");
        let body = src
            .split("async fn shops_for_customer_scope(")
            .nth(1)
            .expect("函数改名了")
            .split("
}")
            .next()
            .unwrap();
        assert!(body.contains("Err(e) =>"), "读失败仍在上抛：算权限挂了就等于一句话都问不了");
        let arm = body.split("Err(e) =>").nth(1).unwrap();
        assert!(arm.contains("SENTINEL_STR"), "退化必须是拒绝哨兵：{arm}");
        assert!(!arm.contains("vec![]"), "退成空集合 = ShopCodes 不注入 = 整表可见：{arm}");
    }

    #[test]
    fn missing_special_bindings_become_deny_sentinels() {
        assert_eq!(deny_empty_ids(vec![]), vec![SENTINEL]);
        assert_eq!(deny_empty_strings(vec![" ".into()]), vec!["-1".to_string()]);
        assert!(!has_real_codes(&["-1".into()]));
    }

    #[test]
    fn device_full_scope_exception_is_exactly_two_roles() {
        assert!(device_full_scope_role("xiaoyunbp"));
        assert!(device_full_scope_role("shebeiyy"));
        for role in ["admin", "device", "shebeiyy_admin", "XIAOYUNBP", ""] {
            assert!(!device_full_scope_role(role), "不得扩大设备专属角色例外: {role}");
        }
    }
}
