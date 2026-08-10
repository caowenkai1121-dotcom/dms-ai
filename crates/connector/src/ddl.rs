//! 上传建表的**全仓唯一**安全面：标识符白名单 + 类型推断 + DDL 渲染 + 字面量转义。
//!
//! 为什么在 connector：它是唯一能执行 DDL 的 crate。清洗面与执行面同 crate 才能一次 review 完
//! 整个注入面（ARCHITECTURE §5 已判：`knowledge` 不得再有第二份 `SafeIdent`）。
//! 本文件零 IO、全纯函数；`OwnedStore::create_upload_table` 只吃这里产出的 `UploadTableSpec`。
//!
//! 双保险：所有标识符先过白名单（`SafeIdent`），渲染时**再**用双引号包裹。
//! 值一律走 `$n` bind；只有列注释（`COMMENT ON` 不可参数化）才用 `quote_literal`。

/// 通过白名单的 PG 标识符：`^[a-z][a-z0-9_]{0,62}$`。
/// 字段私有——除 `parse`/`sanitize` 外无从构造，裸串在类型上进不来。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafeIdent(String);

/// PG 标识符长度上限（NAMEDATALEN-1）
const MAX_IDENT: usize = 63;

impl SafeIdent {
    /// 严格校验，不合规返回 `None`。用于代码里写死的 schema 名等已知安全串。
    pub fn parse(raw: &str) -> Option<SafeIdent> {
        let mut cs = raw.chars();
        if !matches!(cs.next(), Some(c) if c.is_ascii_lowercase()) {
            return None;
        }
        if raw.len() > MAX_IDENT || !cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return None;
        }
        Some(SafeIdent(raw.to_string()))
    }

    /// 清洗任意外部文本（Excel 表头）：小写、非 `[a-z0-9_]` 一律换 `_`、首字符非字母则前缀 `c`、
    /// 截 63 字；清洗后为空或全 `_` 则退化为 `c{ord}`（`ord` = 列序号）。**返回值必然通过 `parse`。**
    pub fn sanitize(raw: &str, ord: usize) -> SafeIdent {
        let mut s: String = raw
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
            .collect();
        if s.chars().all(|c| c == '_') {
            return SafeIdent(format!("c{ord}"));
        }
        if !s.starts_with(|c: char| c.is_ascii_lowercase()) {
            s.insert(0, 'c');
        }
        s.truncate(MAX_IDENT);
        SafeIdent(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColType {
    Text,
    Numeric,
    Timestamptz,
}

impl ColType {
    pub fn pg_type(self) -> &'static str {
        match self {
            ColType::Text => "text",
            ColType::Numeric => "numeric",
            ColType::Timestamptz => "timestamptz",
        }
    }
}

pub struct UploadColumn {
    /// 落库列名（已过白名单，同一 spec 内互不重名）
    pub name: SafeIdent,
    pub ty: ColType,
    /// 原始表头，仅进列注释；是**不可信文本**
    pub header: String,
}

pub struct UploadTableSpec {
    pub schema: SafeIdent,
    pub table: SafeIdent,
    pub columns: Vec<UploadColumn>,
}

/// 表头 → 列定义：逐列 `sanitize` 后**消重**（`_2`/`_3`…），同名两列绝不塌成一列。
/// 组装 `UploadTableSpec.columns` 的唯一推荐入口（`knowledge::tabular` 调它，不自己拼）。
pub fn build_columns(cols: &[(&str, ColType)]) -> Vec<UploadColumn> {
    let mut used: Vec<String> = Vec::with_capacity(cols.len());
    cols.iter()
        .enumerate()
        .map(|(ord, (header, ty))| UploadColumn {
            name: dedup(SafeIdent::sanitize(header, ord), &mut used),
            ty: *ty,
            header: (*header).to_string(),
        })
        .collect()
}

fn dedup(base: SafeIdent, used: &mut Vec<String>) -> SafeIdent {
    let mut cand = base.0;
    let mut n = 2usize;
    while used.iter().any(|u| *u == cand) {
        let suffix = format!("_{n}");
        let keep = MAX_IDENT - suffix.len();
        cand.truncate(cand.len().min(keep));
        cand.push_str(&suffix);
        n += 1;
        // 截断后可能与更早的候选再撞，循环继续；base 首字符是字母，截断不改首字符
        if n > 1000 {
            cand = format!("c{}", used.len());
        }
    }
    used.push(cand.clone());
    SafeIdent(cand)
}

/// 类型推断：全可解析数字 → `Numeric`；全可解析日期 → `Timestamptz`；否则 `Text`。
/// 空白样本跳过（空列/缺格不该把整列打成 Text）；无有效样本 → `Text`。**推断失败一律 Text，宁缺毋滥。**
pub fn infer_col_type(samples: &[&str]) -> ColType {
    let vals: Vec<&str> = samples.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if vals.is_empty() {
        return ColType::Text;
    }
    if vals.iter().all(|s| is_plain_number(s)) {
        return ColType::Numeric;
    }
    if vals.iter().all(|s| is_civil_datetime(s)) {
        return ColType::Timestamptz;
    }
    ColType::Text
}

/// 纯十进制数：可选负号 + 无前导零整数部 + 可选小数部。
/// 前导零（`0012`）、千分位、货币符、科学计数、空格一律不算数字——那些是编码列，判 Text 才不丢信息。
fn is_plain_number(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = body.split_once('.').unwrap_or((body, "0"));
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    if !digits(int) || !digits(frac) {
        return false;
    }
    // ponytail: 15 位以上整数当编码列（手机号/身份证/长单号），要真数值时改这个阈值
    int.len() <= 15 && (int.len() == 1 || !int.starts_with('0'))
}

/// 公历日期（时间可选）：`YYYY-MM-DD` / `YYYY/MM/DD`，可跟 ` HH:MM[:SS]` 或 `THH:MM[:SS]`。
/// 带时区偏移、月份名、两位年一律不认（→ Text）。
fn is_civil_datetime(s: &str) -> bool {
    let (date, time) = s.split_once([' ', 'T']).unwrap_or((s, ""));
    let d: Vec<&str> = date.split(['-', '/']).collect();
    let num = |p: &str, w: usize| -> Option<u32> {
        if p.is_empty() || p.len() > w || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    let ok_date = match d.as_slice() {
        [y, m, dd] => matches!((num(y, 4), num(m, 2), num(dd, 2)),
            (Some(y), Some(m), Some(dd)) if y >= 1000 && (1..=12).contains(&m) && (1..=31).contains(&dd)),
        _ => false,
    };
    ok_date && (time.is_empty() || is_civil_time(time))
}

fn is_civil_time(time: &str) -> bool {
    let t: Vec<&str> = time.trim_end_matches('Z').split(':').collect();
    let sec = t.get(2).map(|s| s.split('.').next().unwrap_or("")).unwrap_or("0");
    let p = |v: &str, hi: u32| v.len() <= 2 && v.parse::<u32>().map(|n| n < hi).unwrap_or(false);
    matches!(t.len(), 2 | 3) && p(t[0], 24) && p(t[1], 60) && p(sec, 60)
}

/// `CREATE TABLE IF NOT EXISTS "schema"."table" (...)`。
/// schema/table 类型上只能是 `SafeIdent`——裸串传不进来，这是本函数的全部安全论证。
pub(crate) fn render_create_table(spec: &UploadTableSpec) -> String {
    let cols: Vec<String> = spec
        .columns
        .iter()
        .map(|c| format!("  \"{}\" {}", c.name.as_str(), c.ty.pg_type()))
        .collect();
    format!(
        "CREATE TABLE IF NOT EXISTS {} (\n{}\n)",
        qualified(spec),
        cols.join(",\n")
    )
}

/// 中文表头进列注释，一列一条语句（`COMMENT ON` 不能参数化，故走 `quote_literal`）。
/// 表头清洗后为空的列不产语句。
pub(crate) fn render_column_comments(spec: &UploadTableSpec) -> Vec<String> {
    spec.columns
        .iter()
        .filter_map(|c| {
            let text = comment_text(&c.header);
            (!text.is_empty()).then(|| {
                format!(
                    "COMMENT ON COLUMN {}.\"{}\" IS {}",
                    qualified(spec),
                    c.name.as_str(),
                    quote_literal(&text)
                )
            })
        })
        .collect()
}

// 这里原有一个单行 `INSERT … VALUES ($1,$2)` 渲染器（T4 建、无消费者）。K4 落地时发现它用不了：
// 值按 text bind 时 sqlx 在 Parse 里报 text 的 OID，而 PG 没有 text→numeric 的**赋值**转换，
// 单行形态会在第一个金额列上直接报「column is of type numeric but expression is of type text」。
// 真正上线的是 `owned.rs::render_insert_unnest`（每列一个 text[] + 显式 `::pg_type` 转换，
// 顺带把往返从每行一次降到每 500 行一次），安全属性由 owned.rs 的
// `insert_renders_only_placeholders_and_quoted_idents` 钉住。
// 按「没有消费者就删」删掉，不留第二个渲染器让人挑（挑错那个的症状是连库才报的类型错）。
fn qualified(spec: &UploadTableSpec) -> String {
    format!("\"{}\".\"{}\"", spec.schema.as_str(), spec.table.as_str())
}

/// PG 字面量转义，**仅供注释文本**。含反斜杠时用 `E''` 语法，避免
/// `standard_conforming_strings` 两种取值下语义分叉。
pub(crate) fn quote_literal(s: &str) -> String {
    let q = s.replace('\'', "''");
    if q.contains('\\') {
        format!("E'{}'", q.replace('\\', "\\\\"))
    } else {
        format!("'{q}'")
    }
}

/// 注释文本清洗：控制字符与换行 → 空格，去掉块注释结束符 `*/`，压空白，截 120 字。
fn comment_text(raw: &str) -> String {
    let flat: String = raw
        .replace("*/", "")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(headers: &[&str]) -> UploadTableSpec {
        let cols: Vec<(&str, ColType)> = headers.iter().map(|h| (*h, ColType::Text)).collect();
        UploadTableSpec {
            schema: SafeIdent::parse("kb_up").unwrap(),
            table: SafeIdent::parse("t1").unwrap(),
            columns: build_columns(&cols),
        }
    }

    #[test]
    fn parse_is_strict() {
        assert!(SafeIdent::parse("a").is_some());
        assert!(SafeIdent::parse("a_1b").is_some());
        for bad in ["", "A", "1a", "_a", "a-b", "a b", "客户", "a\"b", &"a".repeat(64)] {
            assert!(SafeIdent::parse(bad).is_none(), "{bad} 应被拒");
        }
    }

    /// 恶意/退化表头清洗后必须都是合法标识符
    #[test]
    fn sanitize_output_always_parses() {
        let nasty = [
            "a; DROP TABLE x",
            "\"; --",
            "客户编码",
            "",
            "   ",
            "__",
            "2024年1月",
            "a'b\\c",
            "*/ COMMENT",
            &"很长的中文表头".repeat(20),
            &"z".repeat(200),
        ];
        for (i, h) in nasty.iter().enumerate() {
            let id = SafeIdent::sanitize(h, i);
            assert!(SafeIdent::parse(id.as_str()).is_some(), "{h:?} -> {:?}", id.as_str());
            assert!(id.as_str().len() <= MAX_IDENT);
        }
        assert_eq!(SafeIdent::sanitize("a; DROP TABLE x", 0).as_str(), "a__drop_table_x");
        assert_eq!(SafeIdent::sanitize("客户编码", 3).as_str(), "c3");
        assert_eq!(SafeIdent::sanitize("", 7).as_str(), "c7");
        assert_eq!(SafeIdent::sanitize("2024年", 1).as_str(), "c2024_");
    }

    #[test]
    fn duplicate_headers_never_collapse() {
        let s = spec(&["金额", "金额", "amount", "amount", "amount_2"]);
        let names: Vec<&str> = s.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["c0", "c1", "amount", "amount_2", "amount_2_2"]);
        let mut uniq = names.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), names.len(), "列名必须互不重名");
    }

    #[test]
    fn long_duplicate_headers_stay_in_bounds() {
        let long = "z".repeat(70);
        let s = spec(&[&long, &long, &long]);
        for c in &s.columns {
            assert!(c.name.as_str().len() <= MAX_IDENT);
            assert!(SafeIdent::parse(c.name.as_str()).is_some());
        }
        assert_ne!(s.columns[0].name, s.columns[1].name);
        assert_ne!(s.columns[1].name, s.columns[2].name);
    }

    #[test]
    fn infer_prefers_text_when_unsure() {
        assert_eq!(infer_col_type(&[]), ColType::Text);
        assert_eq!(infer_col_type(&["", "  "]), ColType::Text);
        assert_eq!(infer_col_type(&["1", "abc"]), ColType::Text);
        assert_eq!(infer_col_type(&["0012", "0013"]), ColType::Text, "前导零是编码列");
        assert_eq!(infer_col_type(&["1,234"]), ColType::Text, "千分位不算数字");
        assert_eq!(infer_col_type(&["$12"]), ColType::Text);
        assert_eq!(infer_col_type(&["12%"]), ColType::Text);
        assert_eq!(infer_col_type(&["1e5"]), ColType::Text);
        assert_eq!(infer_col_type(&["12."]), ColType::Text);
        assert_eq!(infer_col_type(&[".5"]), ColType::Text);
        assert_eq!(infer_col_type(&["13800138000123456"]), ColType::Text, "超长数字当编码");
    }

    #[test]
    fn infer_numeric_and_timestamptz() {
        assert_eq!(infer_col_type(&["1", "-2.50", "0", "0.5"]), ColType::Numeric);
        assert_eq!(infer_col_type(&["12", "", "13"]), ColType::Numeric, "空格跳过");
        assert_eq!(infer_col_type(&["2024-01-31", "2024/2/1"]), ColType::Timestamptz);
        assert_eq!(
            infer_col_type(&["2024-01-31 08:00", "2024-01-31T08:00:01.5"]),
            ColType::Timestamptz
        );
        assert_eq!(infer_col_type(&["2024-13-01"]), ColType::Text, "月份越界");
        assert_eq!(infer_col_type(&["2024-01-32"]), ColType::Text);
        assert_eq!(infer_col_type(&["2024-01-01 25:00"]), ColType::Text);
        assert_eq!(infer_col_type(&["24-01-01"]), ColType::Text, "两位年不认");
        assert_eq!(infer_col_type(&["20240131"]), ColType::Numeric, "纯数字先判数字");
    }

    #[test]
    fn create_table_quotes_every_ident() {
        let s = UploadTableSpec {
            schema: SafeIdent::parse("kb_up").unwrap(),
            table: SafeIdent::parse("t_sales").unwrap(),
            columns: build_columns(&[("金额", ColType::Numeric), ("日期", ColType::Timestamptz)]),
        };
        assert_eq!(
            render_create_table(&s),
            "CREATE TABLE IF NOT EXISTS \"kb_up\".\"t_sales\" (\n  \"c0\" numeric,\n  \"c1\" timestamptz\n)"
        );
    }

    #[test]
    fn comments_carry_header_text_safely() {
        let s = spec(&["客户名称", "a'b */ c\nd", "\t"]);
        let out = render_column_comments(&s);
        assert_eq!(out.len(), 2, "空白表头不产注释");
        assert_eq!(out[0], "COMMENT ON COLUMN \"kb_up\".\"t1\".\"c0\" IS '客户名称'");
        assert_eq!(out[1], "COMMENT ON COLUMN \"kb_up\".\"t1\".\"a_b____c_d\" IS 'a''b c d'");
        for stmt in &out {
            assert!(!stmt.contains("*/") && !stmt.contains('\n') && !stmt.contains('\r'));
        }
    }

    #[test]
    fn quote_literal_handles_quotes_and_backslash() {
        assert_eq!(quote_literal("a'b"), "'a''b'");
        assert_eq!(quote_literal("'; DROP TABLE x --"), "'''; DROP TABLE x --'");
        assert_eq!(quote_literal("a\\'b"), "E'a\\\\''b'");
        assert_eq!(quote_literal(""), "''");
    }

    #[test]
    fn comment_text_truncates() {
        let long = "客".repeat(200);
        assert_eq!(comment_text(&long).chars().count(), 120);
        assert_eq!(comment_text("a\u{7}b"), "a b");
    }
}
