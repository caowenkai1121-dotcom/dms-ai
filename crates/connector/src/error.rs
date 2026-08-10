//! connector 的唯一对外错误类型。手写 `Display` + `Error`（D6 零新增依赖，不引 thiserror）。
//!
//! **没有 `is_transient`**：全仓零退避重试调用点，ARCHITECTURE §8 已判删 —— 真要重试时
//! 在唯一需要的那个 await 点写 3 行循环，比留一个没人调的分类函数便宜。
//!
//! 文案纪律：每条消息都带**库/源标识**（`[dms]` / `[owned-pg]`）。运维在日志里看到
//! 「查询失败 [dms] Unknown column 'x' in 'field list'」就知道是哪个源出的错，不用回翻上下文。
//! 五个构造器把标识做成**必填参数** —— 靠自觉在每个 call site 手 format 一定会漏。

use std::error::Error;
use std::fmt;
use std::time::Duration;

// 没有 `From<sqlx::Error>`：那条路会把源标识丢掉（`?` 处没人知道是哪个库）。
// 转换统一在 `fixed::classify` / `mysql.rs` / `postgres.rs`，那里手上有标识。

/// 五个变体的划分依据是**调用方要做的决定**，不是错误来源的分类学：
/// `Query` 表示「数据库明确判定语句有问题」（可以拿去 repair），其余四个都不该触发 SQL 改写。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectorError {
    /// DSN 缺失/不可解析、方言名认不出、启动期自检不通过（F3 的 meta/kb/chat 可见性）
    Config(String),
    /// 建池、握手、连接被拒
    Connect(String),
    /// 数据库明确判定语句有问题：SQL 错误、列不存在、权限不足、死锁
    Query(String),
    /// **我们这侧**的超时（`tokio::time::timeout`），不是数据库回的错
    Timeout(String),
    /// 取到行但解不成目标类型：列类型漂移、`FromRow` 不匹配、bind 编码失败
    Decode(String),
}

/// `[源标识] 细节` —— 全部构造器共用的一行拼装
fn tag(at: &str, detail: impl fmt::Display) -> String {
    format!("[{at}] {detail}")
}

impl ConnectorError {
    /// `at` = 库/源标识：只读源用 `DsId`（`ds.as_str()`），自有库用 `owned-pg`
    pub fn config(at: &str, detail: impl fmt::Display) -> Self {
        Self::Config(tag(at, detail))
    }

    pub fn connect(at: &str, detail: impl fmt::Display) -> Self {
        Self::Connect(tag(at, detail))
    }

    pub fn query(at: &str, detail: impl fmt::Display) -> Self {
        Self::Query(tag(at, detail))
    }

    /// 超时只需要「等了多久」，底层不会给别的信息
    pub fn timeout(at: &str, waited: Duration) -> Self {
        Self::Timeout(tag(at, format!("等待 {:.1}s 未返回", waited.as_secs_f32())))
    }

    pub fn decode(at: &str, detail: impl fmt::Display) -> Self {
        Self::Decode(tag(at, detail))
    }
}

impl fmt::Display for ConnectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(m) => write!(f, "配置错误 {m}"),
            Self::Connect(m) => write!(f, "连接失败 {m}"),
            Self::Query(m) => write!(f, "查询失败 {m}"),
            Self::Timeout(m) => write!(f, "超时 {m}"),
            Self::Decode(m) => write!(f, "结果解码失败 {m}"),
        }
    }
}

impl Error for ConnectorError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 运维视角：一眼看出「哪类问题 + 哪个源 + 什么细节」
    #[test]
    fn wording_carries_source_and_detail() {
        assert_eq!(
            ConnectorError::query("dms", "Unknown column 'x' in 'field list'").to_string(),
            "查询失败 [dms] Unknown column 'x' in 'field list'"
        );
        assert_eq!(
            ConnectorError::connect("owned-pg", "connection refused").to_string(),
            "连接失败 [owned-pg] connection refused"
        );
        assert_eq!(
            ConnectorError::config("ds-7", "dsn_ref 未配置").to_string(),
            "配置错误 [ds-7] dsn_ref 未配置"
        );
        assert_eq!(
            ConnectorError::decode("dms", "列 amount 不是 i64").to_string(),
            "结果解码失败 [dms] 列 amount 不是 i64"
        );
    }

    #[test]
    fn timeout_prints_waited_seconds() {
        assert_eq!(
            ConnectorError::timeout("dms", Duration::from_millis(4500)).to_string(),
            "超时 [dms] 等待 4.5s 未返回"
        );
    }
}
