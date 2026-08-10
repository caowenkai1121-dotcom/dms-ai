//! 数据源标识。**本阶段零消费者**——多源是 K3 —— 这是「没有消费者就不建」的唯一例外：
//! `GLOBAL = "*"` 与「不给 `dms()` 构造器」两条裁决（ARCHITECTURE §5）需要一个落点，
//! 且注册表的 `ds_id IN ($ds,'*')` 谓词、缓存 key 的 ds 维度都要引用同一个类型。

use std::fmt;

use serde::Serialize;

/// 数据源 id（注册表行、缓存 key、连接注册中心的 key）。
/// 故意**不提供** `DsId::dms()`：`'dms'` 是业务默认值，归配置层，不进 kernel。
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct DsId(String);

impl DsId {
    /// 「与源无关」的注册表行标记：`ds_id IN ($ds, '*')` 里的那个 `*`。
    pub const GLOBAL: &'static str = "*";

    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DsId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
