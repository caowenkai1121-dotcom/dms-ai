//! 确定性校正链（T8 搬运中）。`server/src/corrector.rs` 是它的旧址，逐批迁空后整文件删除。
//!
//! 纪律：每族一个文件、一个变更原因（D3）；全部纯函数，链的先后由 `agent::run` 定，
//! 这里不含任何编排。

pub mod agg;
pub mod agg_rewrite;
pub mod caliber;
pub mod groupby;
pub mod schema;
pub mod value;

use std::collections::HashMap;

/// 提取 (别名→表, 带前缀列引用)。纯函数，可单测。
pub(crate) fn collect(sql: &str) -> anyhow::Result<(HashMap<String, String>, Vec<(String, String)>)> {
    dms_kernel::sql::ast::collect(sql, &dms_kernel::MysqlDialect).map_err(anyhow::Error::from)
}
