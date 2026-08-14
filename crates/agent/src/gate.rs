//! 三段闸门（`RawSql → check → inject | unrestricted(proof) → ScopedSql`）在 agent 侧的唯一收口，
//! 外加取数的两个执行参数。变更原因＝「一条 SQL 凭什么获得执行资格」。
//!
//! 搬运源 `server/src/pipeline.rs:85-182`（`MAX_ROWS`/`EXEC_TIMEOUT`/`GUARD`/`gate`/`gate_on`/
//! `is_guard_err`/`ensure_limit`）——**逐行搬运**：分支顺序、错误文案、注释里的依据一字不改。
//!
//! 🔴 本文件不许出现 `UnrestrictedProof::new`：铸造点只有 `dms_policy::proof`（+ kernel 内部），
//! 那条 grep 就是全仓「谁能免注入直连生产库」的清单（裁决 二·F F4）。`gate_on` 收 `&Principal`
//! 也是同一件事的类型级表达：闸门拿不到身份，就只有注入这一条出路。

use std::time::Duration;

use dms_kernel::sql::guard::{ensure_limit_with, GuardConfig};
use dms_kernel::sql::dms_lookup::{DmsLookupPolicy, DmsLookupSql};
use dms_kernel::{check, Dialect, GuardError, RawSql, ScopedSql, UnrestrictedProof};

// 权限内核在 dms-policy：`Scope` 由调用方（`AskCtx`）算好塞进来，闸门自己不查库。
use dms_policy::{scope::Scope, Principal};

/// 行上限与取数超时：`SqlSource::fetch` 的后两个参数。
/// server 的 `exec-sql` 判官子命令改调 `dms_agent::gate` + 这两个常量——判官必须与服务同一组参数。
pub const MAX_ROWS: usize = 200;
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(30);
/// 生产 DMS 业务点查固定 2 秒；与 Doris 分析查询预算分离。

/// 本源（DMS MySQL）的护栏配置：行上限 + 敏感列词表，`check()` 的唯一入参。
/// ponytail: 终态在 semantic（`dms_semantic::DMS_GUARD`，见 ARCHITECTURE §5 契约表），
/// 但那个常量今天还不存在；它落地后删掉本常量，全仓改引 `DMS_GUARD`。
pub const GUARD: GuardConfig = GuardConfig::new(MAX_ROWS, dms_semantic::registry::SENSITIVE_COLS);

/// 新增生产业务点查 answerer 的唯一 SQL 入口。它不做行级权限注入；调用方仍必须先按 DMS
/// 账号算出允许访问的编码，并把该编码作为精确 WHERE 条件。登录鉴权的 `fixed()` 查询不走这里。
pub fn gate_dms_lookup(sql: &str, policy: &DmsLookupPolicy) -> anyhow::Result<DmsLookupSql> {
    dms_kernel::sql::dms_lookup::gate_dms_lookup_registered_with(
        sql,
        &dms_kernel::MysqlDialect,
        GUARD.sensitive_cols,
        policy,
        &dms_connector::dms_lookup::REGISTRY,
    )
    .map_err(anyhow::Error::new)
}

/// 三段闸门唯一收口（ARCHITECTURE §2 I1）：`RawSql → check → ScopedSql`。
/// 顺序 = 拆分前的 `is_safe_select(原文)` → `ensure_limit` → `inject`，一步不换位：
/// 被校验的必须是调用方原文，追加的 LIMIT 是我们自己的常量。
pub fn gate(
    p: &Principal,
    sql: &str,
    scope: &Scope,
    d: &'static dyn Dialect,
) -> anyhow::Result<ScopedSql> {
    gate_on(p, sql, scope, false, d)
}

/// 带数据源策略的闸门。`ds_global = true` 表示该源在注册表里是 `policy_kind='global'`
/// （上传表格源/纯维表源）：**整源不做行级过滤**，「谁能看」由选源那层的 ds 级 ACL 判完。
///
/// 为什么必须有这一支：上传源的表不在 DMS 权限档案（`meta.scope_binding`）里，
/// 受限用户的 `ScopeSets` 又恒非空 —— 走 `inject` 必然 fail-closed 拒绝，
/// 症状就是「用户上传了自己的台账，却查不了自己的数据」。
/// 调用方**必须**已用 `visible_datasources` 判过该源对本人可见（`select_source` 的两条分支都判了）。
///
/// `p` 是免注入放行的入场券：铸造 `UnrestrictedProof` 只能走 `dms_policy::proof::for_principal`
/// （F2 的唯一业务铸造点），而它要一个**身份**。所以「谁能不带行级条件查生产库」这件事
/// 在类型上就必须先有 `Principal`，闸门拿不到身份就只有注入这一条出路。
/// `d` 必须是**该 SQL 要发去的那个源**的方言（`cx.source.dialect()`）。
///
/// 🔴 这里原本硬写 `MysqlDialect`。红线校验是靠**解析**做的，所以方言错＝解析错＝
/// `GuardError`＝「这条 SQL 不合格」→ 静默回落/自修。对 PG 源最先撞上的就是 `::` 转换：
/// 上传表的列全是 text，PG 侧数值聚合的自然写法 `SUM(c3::numeric)` 在 MySQL 方言里根本
/// 解析不出来。症状是「问数一直答不出来」，而日志只说 SQL 不合格 —— 不会有人想到是方言。
pub fn gate_on(
    p: &Principal,
    sql: &str,
    scope: &Scope,
    ds_global: bool,
    d: &'static dyn Dialect,
) -> anyhow::Result<ScopedSql> {
    let checked = check(RawSql::new(sql), d, &GUARD)?;
    if ds_global {
        // 只读红线与 LIMIT 护栏照走（上面的 check），跳过的只有行级注入
        let proof = UnrestrictedProof::for_global_source(true)
            .ok_or_else(|| anyhow::anyhow!("global 源放行凭证铸造失败"))?;
        return Ok(ScopedSql::unrestricted(checked, &proof));
    }
    if !scope.sets().is_unrestricted() {
        // 受限用户唯一出路：行级权限注入。未登记表在这里 fail-closed 被拒（不降级、不放行）。
        return dms_kernel::inject(checked, scope.sets(), &dms_policy::snapshot())
            .map_err(anyhow::Error::from);
    }
    // 放行需**两个独立证据**：集合确实全空 + 角色档确实授予全部（`unrestricted_by_role`，
    // 来自 compute_scope 的超管短路或基础档 ALL）。两个证据的核对在 policy 的 `for_principal`
    // 里做且只做一处——第二个证据必须来自身份而不是把 sets 再喂回去：自证的证据等于没有证据，
    // 那样 `ScopeSets::default()`（= 忘了算权限）会把自己证成放行，F2 的闸门白装。
    // 这就是拆分前 `rewrite()` 原样放行的同一支，语义一字不差；显式 proof 只为让
    // `grep 'unrestricted('` 成为全仓无权限出口的审计清单。
    let proof = dms_policy::for_principal(p, scope)
        .ok_or_else(|| anyhow::anyhow!("无限制放行凭证铸造失败：集合全空但角色档并未授予全部数据"))?;
    Ok(ScopedSql::unrestricted(checked, &proof))
}

/// 闸门失败分类：`GuardError` = SQL 本身不合格（可静默回落快路径 / 可喂 LLM 自修）；
/// 其余（`PolicyError`）= 权限注入失败，fail-closed 绝不降级，原样上抛。
pub fn is_guard_err(e: &anyhow::Error) -> bool {
    e.downcast_ref::<GuardError>().is_some()
}

/// LIMIT 护栏：非纯聚合且无 LIMIT → 追加 LIMIT 200。
/// `d` 同 `gate_on`：判「有没有 LIMIT」也是 AST 判定，方言错则判成「没有」→ 白补一个
/// （补重了 PG 会直接语法错），或把纯聚合误判成需要限流。
pub fn ensure_limit(sql: &str, d: &dyn Dialect) -> String {
    ensure_limit_with(sql, d, MAX_ROWS)
}

/// 闸门测试用的普通身份（非超管）。放行的两个证据都在 `Scope` 里，`p` 只参与
/// `for_principal` 那条「超管的 Scope 只能由 compute_scope 短路产出」的 debug_assert。
///
/// 搬运时从 `mod tests` 提到文件级：`ctx.rs` 的测试要造 `ScopedSql` 也只能经本文件的 `gate()`
/// （它不许自己铸 proof），复制第二份 `Principal` 只会多一处会漂的字面量。
#[cfg(test)]
pub(crate) fn anyone() -> Principal {
    Principal {
        employee_id: 7,
        login_name: "t10gate".into(),
        actual_name: "张三".into(),
        administrator_flag: false,
        department_id: None,
        role_id: 9,
        role_code: "city_manager".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use dms_policy::scope::ScopeSets;

    /// 闸门的两个出口 + 失败分类。无库无网：`snapshot_rules()` 在 PG 未加载时回落内置种子。
    /// 钉住的是「哪种失败可以喂 LLM 自修」——判错就等于把 fail-closed 降级成重试。
    #[test]
    fn gate_exits_and_failure_class() {
        let p = anyone();
        let restricted = Scope::new(ScopeSets { employee_ids: vec![7], ..Default::default() }, false);
        // ① 受限用户 → 必须带上行级条件
        let s = gate(&p, "SELECT * FROM t_sales_order so", &restricted, &dms_kernel::MysqlDialect).unwrap();
        assert!(s.wire().to_lowercase().replace(' ', "").contains("so.owner_managerin(7)"), "{}", s.wire());
        assert!(!s.is_unrestricted());
        // ② 无限制档（集合全空 + 角色档授予全部）→ 原样 + LIMIT 护栏，且标记为放行
        let all = gate(&p, "SELECT * FROM t_sales_order", &Scope::new(ScopeSets::default(), true), &dms_kernel::MysqlDialect).unwrap();
        assert!(all.wire().ends_with("LIMIT 200"), "{}", all.wire());
        assert!(all.is_unrestricted());
        // ③ 只读红线失败 = GuardError → 允许回落/自修
        // （`.err().unwrap()` 而非 `.unwrap_err()`：后者要求 `ScopedSql: Debug`，闸门类型刻意不给）
        let e = gate(&p, "DELETE FROM t_sales_order", &restricted, &dms_kernel::MysqlDialect).err().unwrap();
        assert!(is_guard_err(&e), "{e}");
        // ④ 未登记表对受限用户 = PolicyError → 必须原样上抛，绝不喂 LLM 自修
        let e = gate(&p, "SELECT * FROM t_role_data_scope", &restricted, &dms_kernel::MysqlDialect).err().unwrap();
        assert!(!is_guard_err(&e), "{e}");
        assert!(e.to_string().contains("未在权限档案登记"), "{e}");
    }

    #[test]
    fn dms_lookup_gate_is_independent_and_caps_or_rejects_wire_sql() {
        const SALES_ORDER: DmsLookupPolicy =
            DmsLookupPolicy::new("t_sales_order", &["id", "sales_order_code"]);
        // 未写 LIMIT → 闸门补默认上限
        let sql = gate_dms_lookup(
            "SELECT sales_order_code, order_status FROM t_sales_order WHERE sales_order_code = 'HJXH-1'",
            &SALES_ORDER,
        )
        .unwrap();
        assert!(sql.wire().ends_with("LIMIT 50"), "{}", sql.wire());
        // 显式超限 LIMIT → fail-closed 拒绝（2026-08-06 起 kernel 闸门由钳制改为拒绝）
        let e = gate_dms_lookup(
            "SELECT sales_order_code, order_status FROM t_sales_order WHERE sales_order_code = 'HJXH-1' LIMIT 1000",
            &SALES_ORDER,
        )
        .err()
        .unwrap();
        assert!(e.to_string().contains("不得超过 50"), "{e}");
        assert!(gate_dms_lookup(
            "SELECT * FROM t_sales_order a JOIN t_sales_order_detail b ON a.id=b.sales_order_id WHERE a.id=1",
            &SALES_ORDER,
        )
        .is_err());
    }

    /// 【K4】`policy_kind='global'` 的源（上传表格源）：跳过行级注入，但只读红线与 LIMIT 照走。
    /// 钉住的是「用户上传自己的台账后查得了」——受限用户的 ScopeSets 恒非空，
    /// 若这一支没了就会走 inject → 上传表不在 DMS 权限档案里 → fail-closed 拒绝。
    #[test]
    fn global_source_skips_injection_but_keeps_redline() {
        let p = anyone();
        let restricted = Scope::new(ScopeSets { employee_ids: vec![7], ..Default::default() }, false);
        // ① 受限用户查 global 源：不注入任何行条件，但补了 LIMIT
        let s = gate_on(&p, "SELECT * FROM up_a1b2.sheet1", &restricted, true, &dms_kernel::PostgresDialect).unwrap();
        assert!(s.is_unrestricted(), "global 源不做行级过滤");
        assert!(!s.wire().to_lowercase().contains(" in ("), "不许注入行条件: {}", s.wire());
        assert!(s.wire().ends_with("LIMIT 200"), "{}", s.wire());
        // ② 同一个源上写操作照样被红线拦（跳过的只有注入，不是 check）
        let e = gate_on(&p, "DELETE FROM up_a1b2.sheet1", &restricted, true, &dms_kernel::PostgresDialect).err().unwrap();
        assert!(is_guard_err(&e), "{e}");
        // ③ 同一条 SQL 走 DMS 源（ds_global=false）必须被 fail-closed 拒（表未登记）
        let e = gate_on(&p, "SELECT * FROM up_a1b2.sheet1", &restricted, false, &dms_kernel::PostgresDialect).err().unwrap();
        assert!(!is_guard_err(&e), "{e}");
    }

    /// 🔴 F2 回归锁：空集合**单独**不足以放行。
    /// 这一条钉住的正是「双证据不许自证」——若哪天有人把第二个证据写回
    /// `sets.is_unrestricted()`，`ScopeSets::default()` 就能自己把自己证成放行，本测试当场红。
    #[test]
    fn empty_sets_alone_cannot_mint_release() {
        let p = anyone();
        let forged = Scope::new(ScopeSets::default(), false); // 集合全空，但角色档没授予全部
        let e = gate(&p, "SELECT * FROM t_sales_order", &forged, &dms_kernel::MysqlDialect).err().unwrap();
        assert!(!is_guard_err(&e), "必须是权限类错误而非红线错误: {e}");
        assert!(e.to_string().contains("角色档并未授予全部数据"), "{e}");
    }
}
