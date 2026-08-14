//! 实体解析的确定性边界：表面词 -> 主档候选 -> 唯一 canonical 绑定。
//!
//! 第一切片只收客户主档。商品在 DMS 主档与 WMS 库存中有不同的事实源与唯一键，
//! 未完成源适配器前不能假装成同一个 resolver；库存 SKU 仍由 server 的 WMS 探针解析。

use dms_connector::source::RowSet;

use crate::ctx::AskCtx;
use crate::gate::{gate_on, EXEC_TIMEOUT, MAX_ROWS};
use crate::intent::{ExecutionEvidence, IntentSlotKind};

const CANDIDATE_LIMIT: usize = 8;

/// 用户对客户字段的显式提示。`Auto` 只在编码、全名与简称间搜索，不猜其它列。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomerMatchField {
    Auto,
    Code,
    Name,
    Alias,
}

/// 已由可见客户主档唯一确认的绑定。内部键只供执行使用，不进入意图摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomerBinding {
    pub surface: String,
    pub canonical_code: String,
    pub canonical_name: String,
}

impl CustomerBinding {
    /// 只有唯一主档绑定才能生成实体槽位证据。
    pub fn execution_evidence(&self) -> ExecutionEvidence {
        ExecutionEvidence::default().resolve(IntentSlotKind::Entity, self.surface.clone())
    }
}

/// 解析结果不以 `Option` 表示：零命中与歧义的处置完全不同，调用方不得任选第一条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomerResolution {
    NotFound,
    Unique(CustomerBinding),
    Ambiguous(Vec<CustomerBinding>),
}

/// 精确优先、模糊其次地解析客户主档。所有 SQL 都经过与问数主链相同的安全/权限闸门。
pub async fn resolve_customer(
    cx: &AskCtx<'_>,
    surface: &str,
    field: CustomerMatchField,
) -> anyhow::Result<CustomerResolution> {
    let exact = customer_candidates(cx, surface, field, true).await?;
    if exact.len() == 1 {
        return Ok(CustomerResolution::Unique(
            exact.into_iter().next().unwrap(),
        ));
    }
    if exact.len() > 1 {
        return Ok(CustomerResolution::Ambiguous(exact));
    }
    if field == CustomerMatchField::Code {
        return Ok(CustomerResolution::NotFound);
    }
    let fuzzy = customer_candidates(cx, surface, field, false).await?;
    Ok(match fuzzy.len() {
        0 => CustomerResolution::NotFound,
        1 => CustomerResolution::Unique(fuzzy.into_iter().next().unwrap()),
        _ => CustomerResolution::Ambiguous(fuzzy),
    })
}

/// 候选查询也是共享契约：实体卡要把多候选呈现给用户，直查路径要据此 fail closed。
pub(crate) async fn customer_candidates(
    cx: &AskCtx<'_>,
    surface: &str,
    field: CustomerMatchField,
    exact: bool,
) -> anyhow::Result<Vec<CustomerBinding>> {
    validate_surface(surface)?;
    let condition = customer_condition(field, surface, exact);
    let sql = format!(
        "SELECT c.customer_code, c.customer_name FROM t_customer c \
         WHERE c.deleted_flag = 0 AND ({condition}) \
         ORDER BY c.customer_name, c.customer_code LIMIT {CANDIDATE_LIMIT}"
    );
    let rows = fetch_rows(cx, &sql).await?;
    Ok(bindings(surface, &rows))
}

fn customer_condition(field: CustomerMatchField, surface: &str, exact: bool) -> String {
    let op = if exact { "=" } else { "LIKE" };
    let value = if exact {
        format!("'{}'", escape_literal(surface))
    } else {
        format!("'%{}%'", escape_like(surface))
    };
    let columns: &[&str] = match (field, exact) {
        (CustomerMatchField::Code, _) => &["c.customer_code"],
        (CustomerMatchField::Name, _) => &["c.customer_name"],
        (CustomerMatchField::Alias, _) => &["c.customer_short_name"],
        (CustomerMatchField::Auto, true) => &["c.customer_code", "c.customer_name"],
        (CustomerMatchField::Auto, false) => &[
            "c.customer_code",
            "c.customer_name",
            "c.customer_short_name",
        ],
    };
    columns
        .iter()
        .map(|column| format!("{column} {op} {value}"))
        .collect::<Vec<_>>()
        .join(" OR ")
}

async fn fetch_rows(cx: &AskCtx<'_>, sql: &str) -> anyhow::Result<RowSet> {
    let scoped = gate_on(cx.p, sql, cx.scope, cx.ds_global, cx.source.dialect())?;
    Ok(cx.source.fetch(&scoped, MAX_ROWS, EXEC_TIMEOUT).await?)
}

fn bindings(surface: &str, rows: &RowSet) -> Vec<CustomerBinding> {
    let mut out = Vec::new();
    for row in &rows.rows {
        let Some(code) = row
            .first()
            .and_then(value_text)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            continue;
        };
        let Some(name) = row
            .get(1)
            .and_then(value_text)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            continue;
        };
        if out.iter().any(|binding: &CustomerBinding| {
            binding.canonical_code == code && binding.canonical_name == name
        }) {
            continue;
        }
        out.push(CustomerBinding {
            surface: surface.to_string(),
            canonical_code: code.to_string(),
            canonical_name: name.to_string(),
        });
    }
    out
}

fn value_text(value: &serde_json::Value) -> Option<&str> {
    value.as_str()
}

fn validate_surface(surface: &str) -> anyhow::Result<()> {
    let n = surface.chars().count();
    if !(2..=80).contains(&n)
        || surface
            .chars()
            .any(|ch| matches!(ch, '\'' | '"' | '%' | ';' | '\\') || ch.is_control())
    {
        anyhow::bail!("客户实体表面词不合法")
    }
    Ok(())
}

fn escape_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "''")
}

/// MySQL/Doris 默认反斜杠转义 LIKE 通配符；这里不拼 `ESCAPE`，兼容两种执行源。
fn escape_like(value: &str) -> String {
    escape_literal(value)
        .replace('_', "\\_")
        .replace('%', "\\%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn customer_lookup_is_exact_first_and_field_bounded() {
        let exact = customer_condition(CustomerMatchField::Auto, "恒众", true);
        assert!(exact.contains("c.customer_code = '恒众'"));
        assert!(exact.contains("c.customer_name = '恒众'"));
        assert!(!exact.contains("customer_short_name"));

        let fuzzy = customer_condition(CustomerMatchField::Auto, "恒众", false);
        assert!(fuzzy.contains("c.customer_name LIKE '%恒众%'"));
        assert!(fuzzy.contains("c.customer_short_name LIKE '%恒众%'"));

        let code = customer_condition(CustomerMatchField::Code, "C001", false);
        assert_eq!(code, "c.customer_code LIKE '%C001%'");
    }

    #[test]
    fn only_unique_binding_can_emit_original_surface_evidence() {
        let binding = CustomerBinding {
            surface: "恒众餐饮".into(),
            canonical_code: "C001".into(),
            canonical_name: "线下-恒众餐饮有限公司".into(),
        };
        assert_eq!(
            binding.execution_evidence().resolved,
            vec![crate::intent::ResolvedSlot {
                kind: IntentSlotKind::Entity,
                surface: "恒众餐饮".into(),
            }]
        );
    }

    #[test]
    fn unsafe_or_too_short_surfaces_are_rejected() {
        assert!(validate_surface("恒众").is_ok());
        for value in ["客", "恒众%", "恒众'", "恒众;select", "恒众\\门店"] {
            assert!(validate_surface(value).is_err(), "{value}");
        }
    }
}
