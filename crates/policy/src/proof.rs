//! `UnrestrictedProof` 的**唯一**业务铸造点（F2）。
//!
//! `grep 'UnrestrictedProof::new'` 之后应只剩这里 + kernel 内部：那条 grep 就是全仓
//! 「谁能免注入直连生产库」的清单。放行需要**两个独立证据**：
//! ① 全部权限维度确实为空（`scope.sets()`）；② 角色档确实授予全部（`unrestricted_by_role`，
//! 来自 `compute_scope` 的超管短路或基础档 ALL）。
//!
//! 第二个证据必须来自**身份**，不能把 `sets` 再喂回去自证 —— `ScopeSets::default()`
//! （= 忘了算权限）会把自己证成「有资格看全部」，闸门就白装了。

use dms_kernel::UnrestrictedProof;

use crate::principal::Principal;
use crate::scope::Scope;

/// 该身份这一轮是否可以免注入。不够格返回 `None`，调用方只能走 `kernel::inject`。
pub fn for_principal(p: &Principal, scope: &Scope) -> Option<UnrestrictedProof> {
    let by_role = scope.unrestricted_by_role();
    // 生产路径上超管的 Scope 只能来自 `admin_shortcut`，那里恒 `(default(), true)`。
    // 若有人手搓 `Scope::new(sets, false)` 冒充超管，这条在 debug 构建会当场炸；
    // release 下零成本，且结果仍是 fail-closed（拿不到 proof → 走注入 → 未登记表被拒）。
    debug_assert!(!p.administrator_flag || by_role, "超管的 Scope 只能由 compute_scope 短路产出");
    UnrestrictedProof::new(scope.sets(), by_role)
}

/// 命令行管理任务的免注入凭证（今天只有 `meta autodiscover` 的 A1 探针要它）。
///
/// 这类任务**没有「以谁的身份查」这回事**：SQL 由 information_schema 的表名/列名拼装
/// （标识符还另过 `probe::ident` 白名单）。所以第二个证据不可能来自身份 ——
/// 那就让它来自**进程形态**：argv 必须真的是这个子命令。
///
/// 为什么不是在调用点写 `UnrestrictedProof::new(&ScopeSets::default(), true)`（本轮迁移后
/// `main.rs:104` 就是那样）：① 那个 `true` 是硬编码的自证，与 F2 的套路一字不差；
/// ② 它让本文件开头那句「`grep 'UnrestrictedProof::new'` 就是全仓放行清单」变成谎话。
/// 校验 argv 之后，谁把这一行粘进 axum handler 都铸不出凭证（服务进程的 argv 里没有该子命令）
/// —— 失败方向是 fail-closed，而不是静默放行整库。
pub fn for_admin_cli(task: &str) -> Option<UnrestrictedProof> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if !argv.join(" ").starts_with(task) {
        tracing::error!("非 `{task}` 进程试图铸造管理任务放行凭证（argv={argv:?}）");
        return None;
    }
    UnrestrictedProof::new(&crate::scope::ScopeSets::default(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{ScopeSets, SENTINEL};

    fn user(admin: bool) -> Principal {
        Principal {
            employee_id: 1,
            login_name: "t5proof".into(),
            actual_name: "张三".into(),
            administrator_flag: admin,
            department_id: None,
            role_id: 9,
            role_code: "city_manager".into(),
        }
    }

    #[test]
    fn proof_needs_both_evidences() {
        // 超管 / 基础档 ALL：集合全空 + 角色档授予全部 → 给
        assert!(for_principal(&user(true), &Scope::new(ScopeSets::default(), true)).is_some());
        // 🔴 F2 的核心：集合全空但角色档没授予全部（= 忘了算权限）→ 不给
        assert!(for_principal(&user(false), &Scope::new(ScopeSets::default(), false)).is_none());
        // 角色档授予全部但集合有限制（脏数据/手搓）→ 不给
        let restricted = ScopeSets { employee_ids: vec![SENTINEL], ..Default::default() };
        assert!(for_principal(&user(false), &Scope::new(restricted, true)).is_none());
    }

    /// 🔴 管理任务凭证的第二证据是 argv。测试进程的 argv 是测试二进制 + 过滤器，
    /// 绝不会以 `meta autodiscover` 开头 —— 所以这里必须是 `None`。
    /// 它同时就是「粘进 HTTP handler 会怎样」的实测：服务进程同样铸不出。
    #[test]
    fn admin_cli_proof_needs_matching_argv() {
        assert!(super::for_admin_cli("meta autodiscover").is_none());
    }
}
