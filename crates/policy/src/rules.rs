//! 表权限档案注册表：进程内快照 + `meta.scope_binding` 播种/加载。
//!
//! 两处相对 server/src/inject.rs（已删除）的改动，都是缺陷修复而非「顺手优化」：
//! ① `OnceLock` → `RwLock<Arc<RuleSet>>`：`OnceLock` 只能设一次，管理面改完档案要重启进程才生效；
//!    每请求 `snapshot()` clone 的是一个 `Arc`，不再 clone 32 行 `HashMap`。
//! ② 多参解码函数 → `BindingRow`（D4）：同型 `Option<String>` 连排传错顺序**不会编译报错**，
//!    效果是把 via 的 local/remote 对调 —— 一条语法合法、静默越权的 EXISTS。
//!
//! SQL 全走 `OwnedStore::fixed(&'static str)` 字面量通道（门禁：policy 不得 `sqlx::query`）。

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use dms_connector::OwnedStore;
use dms_kernel::{Binding, CustomerKind, OwnerKind, RuleSet, TableRule};

use crate::builtin::builtin_rules;

static REGISTRY: OnceLock<RwLock<Arc<RuleSet>>> = OnceLock::new();

/// 初值 = 内置权限种子：PG 不可用 / `load_rules` 未跑时的兜底，绝不是空表
/// （空表对受限用户 = 每张表都 fail-closed 拒绝 = 服务不可用）。
fn cell() -> &'static RwLock<Arc<RuleSet>> {
    REGISTRY.get_or_init(|| RwLock::new(Arc::new(RuleSet::from(builtin_rules()))))
}

/// 档案快照：每请求 clone 一次 `Arc`，全程用同一份（裁决 C2 —— 注入算法只认传进来的 RuleSet）
pub fn snapshot() -> Arc<RuleSet> {
    cell().read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// 热更新的唯一写入口（`load_rules` 与管理面「重载权限档案」共用）
pub fn install(rules: RuleSet) {
    // 档案热更新是运维事件：不留痕就查不了「档案何时被换」
    tracing::info!("权限档案热更新: {} 条", rules.len());
    *cell().write().unwrap_or_else(PoisonError::into_inner) = Arc::new(rules);
}

/// ds 谓词与 `meta.rs::ds_pred(1)` 逐字同形（` AND ds_id IN ($1, '*')`）——
/// **读侧分档、写侧不分**是刻意的（【K6-D】`ds:any`）：行级权限档案的 customer_code/owner_manager
/// 列绑定本质只服务 DMS 源，别的库没有这套列，非 DMS 源在 `meta.datasource` 里走
/// `policy_kind='global'` 根本不进注入。这里只需保证「不把 DMS 的列绑定加载给别的源」。
const LOAD: &str = "SELECT table_name, mode, customer_col, customer_kind, owner_col, owner_kind, via_table, via_local_col, via_remote_col
     FROM meta.scope_binding WHERE ds_id IN ($1, '*')
     ORDER BY CASE WHEN ds_id = '*' THEN 0 ELSE 1 END, table_name";
// ORDER BY 的说明：主键是 table_name，同一表 '*' 行与 ds 专属行今天不可能共存（PK 唯一），
// 排序是给「将来主键加 ds_id 前缀」备的确定性——届时专属行后写胜出（collect 是后写覆盖先写）。

/// `ON CONFLICT (table_name)`：`meta.scope_binding` 的主键没有前置 ds_id（K3-B 只加列不改主键），
/// 灌的就是 DMS 语料，ds_id 由列默认值填 'dms'。真要按源分档案时随主键一起改。
const UPSERT: &str = "INSERT INTO meta.scope_binding(table_name, mode, customer_col, customer_kind, owner_col, owner_kind, via_table, via_local_col, via_remote_col)
     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
     ON CONFLICT (table_name) DO UPDATE SET mode=$2, customer_col=$3, customer_kind=$4, owner_col=$5, owner_kind=$6, via_table=$7, via_local_col=$8, via_remote_col=$9";

/// 曾由内置种子写入、但后来因无法精确表达 DMS 页面权限而退役的档案。
/// 只删明确列出的历史种子；管理员新增的其它自定义档案仍保留。
/// ds:any —— 刻意不带 ds 谓词：退役行在**任何**数据源名下都要清掉（drift 守卫豁免，
/// 判据钉在 `retired_permission_rows_are_deleted_before_upsert`）。
const DELETE_RETIRED: &str = "DELETE FROM meta.scope_binding
     WHERE LOWER(table_name) = ANY($1::text[])";
const RETIRED: &[&str] = &[
    "t_account_bill_header",
    "t_account_bill_detail",
    "t_winc_purchase_transfer",
    "t_device_requisition",
    "t_device_receive_item",
    "t_device_delivery_item",
];

fn is_retired_table(table_name: &str) -> bool {
    RETIRED.iter().any(|retired| table_name.eq_ignore_ascii_case(retired))
}

/// `meta.scope_binding` 的一行。**编解码同一个类型**：`of()` 灌库、`to_rule()` 读回，
/// 列顺序错了立刻是编译错或缺列运行时错，而不是静默换列。
#[derive(Debug, Clone, PartialEq)]
pub struct BindingRow {
    pub table_name: String,
    pub mode: String,
    pub customer_col: Option<String>,
    pub customer_kind: Option<String>,
    pub owner_col: Option<String>,
    pub owner_kind: Option<String>,
    pub via_table: Option<String>,
    pub via_local_col: Option<String>,
    pub via_remote_col: Option<String>,
}

impl BindingRow {
    /// 档案 → 行（播种方向）
    pub fn of(table_name: &str, rule: &TableRule) -> Self {
        let mut r = Self {
            table_name: table_name.to_string(),
            mode: "global".into(),
            customer_col: None,
            customer_kind: Some("codes".into()),
            owner_col: None,
            owner_kind: None,
            via_table: None,
            via_local_col: None,
            via_remote_col: None,
        };
        match rule {
            TableRule::Global => {
                // Global/Via 臂不落 `Some("codes")` 这种误导性列值（读侧忽略，但种子行别撒谎）
                r.customer_kind = None;
            }
            TableRule::Scoped(b) => {
                r.mode = "scoped".into();
                r.customer_kind = Some(match b.customer_kind {
                    CustomerKind::Codes => "codes",
                    CustomerKind::RequiredCodes => "required_codes",
                    CustomerKind::ManagerCodes => "manager_codes",
                    CustomerKind::ShopCodes => "shop_codes",
                }.into());
                r.customer_col = b.customer_col.clone();
                r.owner_col = b.owner_col.clone();
                r.owner_kind = Some(match b.owner_kind {
                    OwnerKind::Ids => "ids",
                    OwnerKind::Codes => "codes",
                    OwnerKind::Login => "login",
                }.into());
            }
            TableRule::Via { table, local_col, remote_col } => {
                r.mode = "via".into();
                r.customer_kind = None;
                r.via_table = Some(table.clone());
                r.via_local_col = Some(local_col.clone());
                r.via_remote_col = Some(remote_col.clone());
            }
        }
        r
    }

    /// 行 → 档案（加载方向，消费本行）。缺列/未知枚举一律 `None` = 跳过该表 = 该表 fail-closed 拒绝。
    pub fn to_rule(self) -> Option<TableRule> {
        // 删除是迁移清理，读侧拒绝才是运行时安全边界：即使旧行位于自定义 ds_id、
        // 清理尚未执行或管理员误恢复，也不能重新进入进程权限快照。
        if is_retired_table(&self.table_name) {
            tracing::warn!("scope_binding {} 已退役，跳过（该表将 fail-closed 拒绝）", self.table_name);
            return None;
        }
        match self.mode.as_str() {
            "global" => Some(TableRule::Global),
            "scoped" => {
                let customer_kind = match self.customer_kind.as_deref() {
                    Some("codes") => CustomerKind::Codes,
                    Some("required_codes") => CustomerKind::RequiredCodes,
                    Some("manager_codes") => CustomerKind::ManagerCodes,
                    Some("shop_codes") => CustomerKind::ShopCodes,
                    other => {
                        tracing::warn!(
                            "scope_binding {} 未知 customer_kind={other:?}，跳过（该表将 fail-closed 拒绝）",
                            self.table_name
                        );
                        return None;
                    }
                };
                let owner_kind = match self.owner_kind.as_deref() {
                    Some("ids") => OwnerKind::Ids,
                    Some("codes") => OwnerKind::Codes,
                    Some("login") => OwnerKind::Login,
                    other => {
                        tracing::warn!(
                            "scope_binding {} 未知 owner_kind={other:?}，跳过（该表将 fail-closed 拒绝）",
                            self.table_name
                        );
                        return None;
                    }
                };
                Some(TableRule::Scoped(Binding {
                    customer_col: self.customer_col,
                    customer_kind,
                    owner_col: self.owner_col,
                    owner_kind,
                }))
            }
            "via" => match (self.via_table, self.via_local_col, self.via_remote_col) {
                (Some(table), Some(local_col), Some(remote_col)) => {
                    Some(TableRule::Via { table, local_col, remote_col })
                }
                _ => {
                    tracing::warn!(
                        "scope_binding {} via 缺列，跳过（该表将 fail-closed 拒绝）",
                        self.table_name
                    );
                    None
                }
            },
            other => {
                tracing::warn!(
                    "scope_binding {} 未知 mode={other:?}，跳过（该表将 fail-closed 拒绝）",
                    self.table_name
                );
                None
            }
        }
    }
}

/// 手写 `FromRow`（workspace 的 sqlx 没开 `derive` feature，不改 Cargo.toml 是硬规则）。
/// 列名与 `COLS` 一一对应，漏一列是运行时错，`cols_match_row_fields` 钉着。
impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for BindingRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            table_name: row.try_get("table_name")?,
            mode: row.try_get("mode")?,
            customer_col: row.try_get("customer_col")?,
            customer_kind: row.try_get("customer_kind")?,
            owner_col: row.try_get("owner_col")?,
            owner_kind: row.try_get("owner_kind")?,
            via_table: row.try_get("via_table")?,
            via_local_col: row.try_get("via_local_col")?,
            via_remote_col: row.try_get("via_remote_col")?,
        })
    }
}

/// 内置种子灌入 `meta.scope_binding`（upsert，代码为种子真相；管理员手工加的行保留）。
/// 不在事务里（DELETE_RETIRED + 39 条 UPSERT）：中途失败留混合态，但两步都幂等 ——
/// 失败重跑即自愈。
pub async fn seed_rules(store: &OwnedStore) -> anyhow::Result<()> {
    store
        .fixed(DELETE_RETIRED)
        .bind(RETIRED.iter().map(|name| (*name).to_string()).collect::<Vec<_>>())
        .execute()
        .await?;
    // HashMap 迭代序不定：排序后 upsert 的执行/日志顺序逐轮一致
    let mut seeded: Vec<(String, TableRule)> = builtin_rules().into_iter().collect();
    seeded.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    for (t, rule) in &seeded {
        let r = BindingRow::of(t, rule);
        store
            .fixed(UPSERT)
            .bind(r.table_name)
            .bind(r.mode)
            .bind(r.customer_col)
            .bind(r.customer_kind)
            .bind(r.owner_col)
            .bind(r.owner_kind)
            .bind(r.via_table)
            .bind(r.via_local_col)
            .bind(r.via_remote_col)
            .execute()
            .await?;
    }
    Ok(())
}

/// 从 `meta.scope_binding` 加载 `ds` 的权限档案并热更新进程注册表（服务启动调用一次）。
/// 返回装载条数。
pub async fn load_rules(store: &OwnedStore, ds: &str) -> anyhow::Result<usize> {
    let rows: Vec<BindingRow> = store.fixed(LOAD).bind(ds).fetch_all().await?;
    let m: HashMap<String, TableRule> = rows
        .into_iter()
        .filter_map(|r| {
            let table_name = r.table_name.clone();
            r.to_rule().map(|rule| (table_name, rule))
        })
        .collect();
    let n = m.len();
    if n == 0 {
        // 一条都没装上多半是 ds 名写错这类运维事故：空表 = 全表 fail-closed，不能静默
        tracing::warn!("load_rules 装载 0 条权限档案（ds={ds}），受限用户将全表 fail-closed");
    }
    install(RuleSet::from(m));
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 独立抄一份列清单，与两条 SQL 对拍：漏一列是运行时 `ColumnNotFound`，
    /// 而那意味着**整个注册表加载失败 → 全表 fail-closed**，值得一条编译期以外的守卫。
    const COLS: &str =
        "table_name, mode, customer_col, customer_kind, owner_col, owner_kind, via_table, via_local_col, via_remote_col";

    #[test]
    fn cols_match_row_fields() {
        for c in COLS.split(',') {
            let c = c.trim();
            assert!(LOAD.contains(c), "LOAD 缺列 {c}");
            assert!(UPSERT.contains(c), "UPSERT 缺列 {c}");
        }
        assert_eq!(COLS.split(',').count(), 9);
        assert!(UPSERT.contains("$9") && !UPSERT.contains("$10"), "9 列 9 个占位符");
        // ds 谓词与 meta.rs::ds_pred(1) 同形；写侧刻意不带（`ds:any`，见上方 LOAD 注释）
        assert!(LOAD.contains("ds_id IN ($1, '*')"));
        assert!(!UPSERT.contains("ds_id"));
    }

    /// 编解码往返：三臂都不许在中途丢列（via 的 local/remote 对调 = 静默越权）
    #[test]
    fn row_roundtrips_every_arm() {
        for (name, rule) in builtin_rules() {
            let back = BindingRow::of(&name, &rule).to_rule().expect("内置档案必可解回");
            match (&rule, &back) {
                (TableRule::Global, TableRule::Global) => {}
                (TableRule::Scoped(a), TableRule::Scoped(b)) => {
                    assert_eq!(a.customer_col, b.customer_col, "{name}");
                    assert!(a.customer_kind == b.customer_kind, "{name} customer_kind 漂移");
                    assert_eq!(a.owner_col, b.owner_col, "{name}");
                    assert!(a.owner_kind == b.owner_kind, "{name} owner_kind 漂移");
                }
                (
                    TableRule::Via { table, local_col, remote_col },
                    TableRule::Via { table: t2, local_col: l2, remote_col: r2 },
                ) => {
                    assert_eq!((table, local_col, remote_col), (t2, l2, r2), "{name}");
                }
                _ => panic!("{name} 档案类型在往返中变了"),
            }
        }
    }

    #[test]
    fn dws_sales_rule_is_seedable_and_loadable() {
        let rules = builtin_rules();
        let rule = rules.get("dws_off_offline_sale_dfn").expect("内置 DWS 销售权限规则");
        let row = BindingRow::of("dws_off_offline_sale_dfn", rule);
        assert_eq!(row.mode, "scoped");
        assert_eq!(row.customer_col.as_deref(), Some("storecode"));
        assert_eq!(row.customer_kind.as_deref(), Some("required_codes"));
        assert!(row.owner_col.is_none());

        let Some(TableRule::Scoped(back)) = row.to_rule() else {
            panic!("scope_binding 行必须能加载回 scoped 规则");
        };
        assert_eq!(back.customer_col.as_deref(), Some("storecode"));
        assert!(back.customer_kind == CustomerKind::RequiredCodes);
        assert!(back.owner_col.is_none());
    }

    /// 坏行必须跳过（= 该表 fail-closed 拒绝），绝不「猜一个档案」放行
    #[test]
    fn broken_rows_are_skipped() {
        let mut r = BindingRow::of("t_x", &TableRule::Via {
            table: "t_h".into(),
            local_col: "c".into(),
            remote_col: "c".into(),
        });
        r.via_remote_col = None;
        assert!(r.clone().to_rule().is_none(), "via 缺列必须跳过");
        r.mode = "whatever".into();
        assert!(r.to_rule().is_none(), "未知 mode 必须跳过");

        let mut r = BindingRow::of(
            "t_x",
            &TableRule::Scoped(Binding {
                customer_col: Some("customer_code".into()),
                customer_kind: CustomerKind::Codes,
                owner_col: Some("owner_id".into()),
                owner_kind: OwnerKind::Ids,
            }),
        );
        r.customer_kind = Some("shop_code".into());
        assert!(r.clone().to_rule().is_none(), "未知 customer_kind 不得退化为客户编码");
        r.customer_kind = Some("codes".into());
        r.owner_kind = None;
        assert!(r.to_rule().is_none(), "缺失 owner_kind 不得退化为员工 ID");
    }

    /// 注册表默认值 = 当前内置表，且 `install` 真的热更新（`OnceLock` 时代设不了第二次）
    #[test]
    fn snapshot_defaults_to_builtin_and_install_replaces() {
        let expected = builtin_rules().len();
        install(RuleSet::from(builtin_rules()));
        assert_eq!(snapshot().len(), expected);
        install(RuleSet::from(HashMap::from([("t_only".to_string(), TableRule::Global)])));
        assert_eq!(snapshot().len(), 1);
        install(RuleSet::from(builtin_rules()));
        assert_eq!(snapshot().len(), expected);
    }

    #[test]
    fn retired_permission_rows_are_deleted_before_upsert() {
        assert!(DELETE_RETIRED.starts_with("DELETE FROM meta.scope_binding"));
        assert!(!DELETE_RETIRED.contains("ds_id"), "退役清理不得遗漏自定义数据源名下的旧行");
        for table in [
            "t_account_bill_header",
            "t_account_bill_detail",
            "t_winc_purchase_transfer",
            "t_device_requisition",
            "t_device_receive_item",
            "t_device_delivery_item",
        ] {
            assert!(RETIRED.contains(&table));
            assert!(!builtin_rules().contains_key(table));
            let row = BindingRow::of(table, &TableRule::Global);
            assert!(row.to_rule().is_none(), "{table} 的持久化旧行必须在读侧再次拒绝");
        }
    }
}
