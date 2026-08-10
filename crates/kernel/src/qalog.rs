//! 问答落账（`meta.query_log`）的跨 crate 共享件：INSERT 列清单、route/status 取值域、
//! 脱敏与截断、超时文案判据。
//!
//! 为什么落 kernel：两个写口（server 问数链 `query_log.rs`、knowledge 文档问答 `qa_log.rs`）
//! 吃同一份 —— 第二张 INSERT、第二份脱敏就是漂移（本仓「同一信任边界两份实现必然漂」）。
//! 编译方向（knowledge 够不到 server）决定共享件只能下沉到这里。内容全是无 IO 的
//! 常量与纯函数（收纳判据 1/3/4）；`meta.query_log` 是本系统自建观测表，不属于
//! 「DMS 业务语料」那条收纳禁令（它管的是生产库的表名/码值/业务词）。

/// `meta.query_log` 的唯一 INSERT（列顺序＝server `query_log.rs` 建表 DDL 的语义顺序）。
/// 两个写口共用这一条：改列清单只许改这里。
pub const INSERT_SQL: &str =
    "INSERT INTO meta.query_log
     (login_name, ds_id, route, question, sql, row_count, elapsed_ms, cache_hit,
      prompt_tokens, completion_tokens, error, trace_id, conv_id, llm_calls, status)
     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)";

/// `route` 列的 KB 取值。`Answer::text`（wire 上的 route）与 knowledge 落账共用同一常量 ——
/// 两处同值是「usage 路由分布 / admin 质量页 / 反馈绑定」对上号的前提。
pub const ROUTE_KNOWLEDGE: &str = "knowledge";

/// `status` 列的取值域（A2 全状态审计）。
/// `blocked` = 被权限/红线拒之门外（没出数）；`timeout` = 执行预算耗尽；
/// 其余 `Err` 一律 `failed`。空串只存在于本列上线前的老行。
/// audit 端点（`datamap_api::valid_audit_status`）的白名单按这四个字面值钉着，加取值要三路同改。
pub const STATUS_SUCCEEDED: &str = "succeeded";
pub const STATUS_BLOCKED: &str = "blocked";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_TIMEOUT: &str = "timeout";

/// 问句/SQL/错误 的入库上限（字符，非字节）—— 一行日志几百 KB 会把这张表写成事故本身。
pub const CLIP_CHARS: usize = 2000;

/// 按**字符**截断（按字节截会把中文切成半个字，入库即乱码）
pub fn clip(s: &str) -> String {
    s.chars().take(CLIP_CHARS).collect()
}

/// 入库原因脱敏：上游报错可能把 URL / 键值对原文带回来（reqwest 错误含完整 URL、
/// 驱动错误可能含连接串片段），而日志表只增不删 —— 宁多剥一层，不许凭据形态落库。
/// 不影响既有口径：正常错误文案里没有这些形态，剥完逐字不变。
pub fn sanitize(s: &str) -> String {
    redact_key_values(&redact_url_userinfo(s))
}

/// `scheme://user:pass@host` → `scheme://***@host`。userinfo 只认「`://` 之后、
/// 第一段不含路径分隔符的 `@`」这一形态，误剥面限定在真 URL 上。
fn redact_url_userinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("://") {
        let head_end = i + 3; // "://" 之后
        let tail = &rest[head_end..];
        match tail.find('@').filter(|&a| {
            !tail[..a].chars().any(|c| matches!(c, '/' | '?' | '#' | ' ' | '\t'))
        }) {
            Some(a) => {
                out.push_str(&rest[..head_end]);
                out.push_str("***@");
                rest = &tail[a + 1..];
            }
            None => {
                out.push_str(&rest[..head_end]);
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `password=abc` → `password=***`。键大小写不敏感；值剥到空白 / `&` / `;` / 引号为止。
fn redact_key_values(s: &str) -> String {
    /// 键名命中即剥值（覆盖 DSN 参数与 LLM 配置里出现过的形态）
    const SENSITIVE_KEYS: &[&str] =
        &["password", "passwd", "pwd", "secret", "api_key", "apikey", "token", "access_token"];
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('=') {
        let key_start = rest[..i]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .map(|p| p + 1)
            .unwrap_or(0);
        if SENSITIVE_KEYS.iter().any(|k| rest[key_start..i].eq_ignore_ascii_case(k)) {
            let val_start = i + 1;
            let val_end = rest[val_start..]
                .find(|c: char| c.is_whitespace() || matches!(c, '&' | ';' | '\'' | '"'))
                .map(|p| val_start + p)
                .unwrap_or(rest.len());
            out.push_str(&rest[..val_start]);
            out.push_str("***");
            rest = &rest[val_end..];
        } else {
            out.push_str(&rest[..=i]);
            rest = &rest[i + 1..];
        }
    }
    out.push_str(rest);
    out
}

/// 超时文案判据（两个写口的 status 分类共用）：本仓错误文案里这三个词只用于真超时。
/// typed 判据（各 crate 自己的错误类型）在各写口自己手里，这里只管丢了类型的文案形态。
pub fn timeout_marked(msg: &str) -> bool {
    msg.contains("超时")
        || msg.to_ascii_lowercase().contains("timed out")
        || msg.to_ascii_lowercase().contains("timeout")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// INSERT 是本表的唯一写入口型：15 列、含 status/trace_id/llm_calls（三表关联与
    /// 全状态审计都靠后三个），列清单漂移到这里当场红
    #[test]
    fn insert_sql_is_the_single_column_contract() {
        assert!(INSERT_SQL.contains("INSERT INTO meta.query_log"), "{INSERT_SQL}");
        for col in [
            "login_name", "ds_id", "route", "question", "sql", "row_count", "elapsed_ms",
            "cache_hit", "prompt_tokens", "completion_tokens", "error", "trace_id", "conv_id",
            "llm_calls", "status",
        ] {
            assert!(INSERT_SQL.contains(col), "列 {col} 丢了: {INSERT_SQL}");
        }
        assert!(INSERT_SQL.contains("$15"), "15 个绑定位: {INSERT_SQL}");
        assert!(!INSERT_SQL.contains("$16"), "多于 15 列就是另一张表的事了: {INSERT_SQL}");
    }

    /// route/status 取值域是跨端点契约：audit 白名单、usage/quality 的显示口径都按字面值认
    #[test]
    fn route_and_status_domains_are_frozen() {
        assert_eq!(ROUTE_KNOWLEDGE, "knowledge");
        for s in [STATUS_SUCCEEDED, STATUS_BLOCKED, STATUS_FAILED, STATUS_TIMEOUT] {
            assert!(["succeeded", "blocked", "failed", "timeout"].contains(&s), "{s}");
        }
    }

    /// 问句/SQL 必须按**字符**截断到 2000（几百 KB 一行会把日志表写成事故）
    #[test]
    fn clip_is_char_based() {
        let long_cn = "销".repeat(3000);
        let out = clip(&long_cn);
        assert_eq!(out.chars().count(), CLIP_CHARS);
        assert_eq!(out.len(), CLIP_CHARS * 3, "按字节截会切出半个中文字");
        assert_eq!(clip("短问句"), "短问句");
        assert_eq!(CLIP_CHARS, 2000, "入库上限的契约值");
    }

    /// 脱敏：URL userinfo 与凭据键值对不许落库；无凭据形态时逐字不变（剥过头会把
    /// LLM 自修要看的报错改坏）
    #[test]
    fn sanitize_strips_credentials_and_keeps_normal_text() {
        let s = sanitize("连接失败: dsn=postgres://svc:TopSecret@db.internal/mds password=hunter2 API_KEY=sk-123");
        assert!(!s.contains("TopSecret") && !s.contains("hunter2") && !s.contains("sk-123"), "{s}");
        assert!(s.contains("postgres://***@db.internal"), "{s}");
        assert!(s.contains("password=***") && s.contains("API_KEY=***"), "{s}");
        assert_eq!(sanitize("查询失败 [dms] Unknown column 'x'"), "查询失败 [dms] Unknown column 'x'");
    }

    /// 超时判据：中文「超时」、reqwest 的「operation timed out」、裸「timeout」都认，
    /// 普通报错不误伤（`timeout_marked` 是两个写口 status 分类的唯一文案判据）
    #[test]
    fn timeout_markers_match_query_log_discipline() {
        assert!(timeout_marked("LLM 请求失败: error sending request: operation timed out"));
        assert!(timeout_marked("超时 [dms] 等待 30.0s 未返回"));
        assert!(timeout_marked("connect Timeout"));
        assert!(!timeout_marked("查询失败 [dms] Unknown column 'x' in 'field list'"));
        assert!(!timeout_marked("生成失败（自修后仍不可用）"));
    }
}
