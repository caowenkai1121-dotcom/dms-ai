//! scope 缓存（F7 修复点）。
//!
//! 旧口径是「当日过期 + `SystemTime / 86400`」，两个后果：权限收紧后最长 24h 仍按旧权限出数，
//! 而且那个除法用的是 **UTC** —— 翻页正好落在北京时间早上 8 点，也就是上班第一波查询。
//!
//! 现口径：**TTL 15 分钟 + key 带 `scope_ver`**。`compute_scope` 本来就要查 `t_role_data_scope`，
//! 把那些行的指纹拼成版本号放进 key：DMS 侧改配置 → 版本不等 → 天然未命中 → **第一次查询即自愈**，
//! 不需要任何外部通知源（dms-ai 对 DMS 库只读，本来也拿不到变更事件）。
//! key 还带 `DsId`：多源下第二个源的 `ScopeSets` 与 DMS 的不是一回事，同 key 会串库（I4）。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use dms_kernel::DsId;

use crate::principal::Principal;
use crate::scope::Scope;

/// 权限收紧后的最长滞后。比这更短会让限权用户的单次 ~10s 计算频繁重跑。
const TTL: Duration = Duration::from_secs(15 * 60);

/// 四维 key：登录名 + 角色 + 数据源 + 权限版本。少任何一维都会串权限（I4）。
#[derive(PartialEq, Eq, Hash, Clone, Debug)]
pub struct Key {
    login: String,
    role: String,
    ds: String,
    ver: u64,
}

type Map = HashMap<Key, (Scope, Instant)>;
static CACHE: OnceLock<Mutex<Map>> = OnceLock::new();

/// 锁中毒不再传染：某次 panic 不该让此后所有权限查询永久 panic。
fn lock() -> MutexGuard<'static, Map> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(PoisonError::into_inner)
}

/// `t_role_data_scope` 该 role_id 全部行的指纹（行数 + 全部 `(data_scope_type, view_type)`）。
/// 先排序再哈希：行序由 MySQL 决定，不排序会让同一份配置算出两个版本号 = 永不命中。
pub fn scope_ver(rows: &[(i32, i32)]) -> u64 {
    let mut sorted = rows.to_vec();
    sorted.sort_unstable();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    sorted.hash(&mut h);
    h.finish()
}

pub fn key(p: &Principal, ds: &DsId, rows: &[(i32, i32)]) -> Key {
    Key {
        login: p.login_name.clone(),
        role: p.role_code.clone(),
        ds: ds.as_str().to_string(),
        ver: scope_ver(rows),
    }
}

/// 命中且未过期才返回；过期条目顺手删掉（惰性清理，没有后台任务）
pub fn get(k: &Key) -> Option<Scope> {
    let mut m = lock();
    if matches!(m.get(k), Some((_, at)) if at.elapsed() < TTL) {
        return m.get(k).map(|(s, _)| s.clone());
    }
    m.remove(k);
    None
}

pub fn put(k: Key, scope: &Scope) {
    lock().insert(k, (scope.clone(), Instant::now()));
}

/// 显式失效（管理面 `scope invalidate <login> [role]`）。`role=None` 清该登录名下全部
/// 角色/数据源/版本的条目。返回清掉的条数，便于管理面回显。
pub fn invalidate(login: &str, role: Option<&str>) -> usize {
    let mut m = lock();
    let before = m.len();
    m.retain(|k, _| !(k.login == login && role.map_or(true, |r| k.role == r)));
    before - m.len()
}

pub fn invalidate_all() -> usize {
    let mut m = lock();
    let n = m.len();
    m.clear();
    n
}

/// 全部断言塞在一个 `#[test]` 里：缓存是**进程级全局**，拆成多个测试会互相清对方的条目。
#[cfg(test)]
mod tests {
    use super::*;

    fn p(login: &str, role: &str) -> Principal {
        Principal {
            employee_id: 1,
            login_name: login.into(),
            actual_name: "张三".into(),
            administrator_flag: false,
            department_id: None,
            role_id: 9,
            role_code: role.into(),
        }
    }

    fn scope() -> Scope {
        Scope::new(crate::scope::ScopeSets { employee_ids: vec![7], ..Default::default() }, false)
    }

    #[test]
    fn versioned_key_self_heals_and_invalidate_works() {
        let ds = DsId::new("dms");
        let other = DsId::new("ds-2");
        let rows = [(1, 0), (2, 101)];

        // 版本号：与行序无关，与内容有关（改配置 → 版本必变 → 天然未命中）
        assert_eq!(scope_ver(&rows), scope_ver(&[(2, 101), (1, 0)]));
        assert_ne!(scope_ver(&rows), scope_ver(&[(1, 10), (2, 101)]), "view_type 变了版本必须变");
        assert_ne!(scope_ver(&rows), scope_ver(&[(1, 0)]), "少一行版本必须变");
        assert_ne!(scope_ver(&rows), scope_ver(&[(1, 101), (2, 0)]), "两列对调必须变");

        let user = p("t5cache", "city_manager");
        let k = key(&user, &ds, &rows);
        assert!(get(&k).is_none(), "空缓存不得命中");
        put(k.clone(), &scope());
        assert_eq!(get(&k).unwrap().sets().employee_ids, vec![7]);

        // 三个维度各自都能造成未命中：版本 / 数据源 / 角色
        assert!(get(&key(&user, &ds, &[(1, 10)])).is_none(), "权限配置改了必须重算");
        assert!(get(&key(&user, &other, &rows)).is_none(), "第二个源不得复用 DMS 的集合");
        assert!(get(&key(&p("t5cache", "admin"), &ds, &rows)).is_none(), "换角色不得复用");

        // 显式失效：指定角色只清该角色，不指定清该登录名全部
        put(key(&p("t5cache", "admin"), &ds, &rows), &scope());
        assert_eq!(invalidate("t5cache", Some("admin")), 1);
        assert!(get(&k).is_some(), "别人的角色不该被连坐");
        assert_eq!(invalidate("t5cache", None), 1);
        assert!(get(&k).is_none());
    }
}
