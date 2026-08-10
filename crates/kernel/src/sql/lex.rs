//! SQL 纯文本词法：**不 parse**，只扫字符。变更原因＝字符扫描规则。
//! 需要 sqlparser 的一律在 `sql::ast`。
//!
//! 搬运源（逐行搬，分支顺序与字符串字面量原样）：
//! `pipeline.rs:152-202`（`strip_literals_and_comments`）、
//! `corrector.rs:385-429`（`split_top_and` / `first_ident_of`）、
//! `direct.rs:362-424/467-523`（`from_table_aliases` / `base_col_refs` / `qualify_cols`）。

/// 词法剥离：去掉字符串字面量（'…'/"…"，支持 \ 转义与 '' 重复转义）与注释（--、#、/* */）。
/// 安全关键词扫描专用——字面量里的敏感词不再干扰判定。
pub fn strip_literals_and_comments(sql: &str) -> String {
    let b: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ ('\'' | '"') => {
                i += 1;
                while i < b.len() {
                    if b[i] == '\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == q {
                        if i + 1 < b.len() && b[i + 1] == q {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                out.push(' ');
            }
            '-' if i + 1 < b.len() && b[i + 1] == '-' => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '#' => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < b.len() && b[i + 1] == '*' => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                out.push(' ');
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// scope_filter 按顶层 AND 切成原子条件（不解析括号内部；口径过滤都是简单 AND 串）
pub fn split_top_and(filter: &str) -> Vec<String> {
    let mut out = vec![];
    let mut depth = 0usize;
    let mut cur = String::new();
    let chars: Vec<char> = filter.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '(' => {
                depth += 1;
                cur.push('(');
            }
            ')' => {
                depth = depth.saturating_sub(1);
                cur.push(')');
            }
            _ if depth == 0
                && chars[i..].len() >= 5
                && chars[i..i + 5].iter().collect::<String>().eq_ignore_ascii_case(" and ") =>
            {
                out.push(cur.trim().to_string());
                cur.clear();
                i += 5;
                continue;
            }
            c => cur.push(c),
        }
        i += 1;
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out.into_iter().filter(|s| !s.is_empty()).collect()
}

/// 取条件串里的第一个标识符（列名），小写去反引号
pub fn first_ident_of(cond: &str) -> Option<String> {
    let t: String = cond
        .trim()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '`')
        .collect();
    let t = t.trim_matches('`').to_lowercase();
    if t.is_empty() { None } else { Some(t) }
}

/// 从 FROM 串里解析出 (真实表名, 别名) 列表：`t_x a JOIN t_y b ON ...` / `(子查询) a`。
/// 纯文本扫描（组合器自己拼的串形态固定），子查询段跳过。纯函数可单测。
pub fn from_table_aliases(from: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    // 去掉括号内内容（子查询/ON 条件里的函数），避免误把子查询里的表当作 FROM 项
    let mut flat = String::with_capacity(from.len());
    let mut depth = 0usize;
    for c in from.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                flat.push(' ');
            }
            _ if depth == 0 => flat.push(c),
            _ => {}
        }
    }
    let toks: Vec<&str> = flat.split_whitespace().collect();
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i];
        let is_table_pos = i == 0 || toks[i - 1].eq_ignore_ascii_case("join") || toks[i - 1].eq_ignore_ascii_case("from");
        if is_table_pos && t.starts_with("t_") {
            if let Some(a) = toks.get(i + 1) {
                if !a.eq_ignore_ascii_case("on") && !a.eq_ignore_ascii_case("join") {
                    out.push((t.to_string(), a.trim_end_matches(',').to_string()));
                    i += 2;
                    continue;
                }
            }
            out.push((t.to_string(), t.to_string()));
        }
        i += 1;
    }
    out
}

/// 收集 SQL 片段里对某别名的列引用（`别名.列`），小写去重。纯函数可单测。
pub fn base_col_refs(frag: &str, alias: &str) -> Vec<String> {
    let pat = format!("{alias}.");
    let mut out: Vec<String> = vec![];
    let lower = frag.to_lowercase();
    let pat = pat.to_lowercase();
    let mut from = 0usize;
    while let Some(pos) = lower[from..].find(&pat) {
        let start = from + pos;
        // 前一个字符必须是非标识符字符（防 xo.col 里的 o. 误命中）
        let prev_ok = start == 0
            || !lower[..start]
                .chars()
                .next_back()
                .map(|c| c.is_alphanumeric() || c == '_' || c == '.')
                .unwrap_or(false);
        let col: String = lower[start + pat.len()..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if prev_ok && !col.is_empty() && !out.contains(&col) {
            out.push(col);
        }
        from = start + pat.len();
    }
    out
}

/// 裸列限定到基表别名：非函数、未限定、非关键字的标识符 → alias.col。
/// 单引号字面量段原样跳过；已有前缀（a.col）的列原样跳过。纯函数可单测。
pub fn qualify_cols(expr: &str, alias: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "AND", "OR", "NOT", "IN", "IS", "NULL", "DISTINCT", "CASE", "WHEN", "THEN", "ELSE", "END",
        "AS", "ASC", "DESC", "LIKE", "BETWEEN", "EXISTS", "TRUE", "FALSE", "COALESCE", "NULLIF",
        "DATE", "YEAR", "MONTH", "DAY", "CURDATE", "NOW", "INTERVAL", "YEARWEEK", "DATE_FORMAT",
        "DATE_ADD", "DATE_SUB", "ROUND", "IF", "IFNULL",
        "SUM", "COUNT", "AVG", "MAX", "MIN", "GROUP_CONCAT",
    ];
    let mut out = String::with_capacity(expr.len() + 16);
    let mut in_quote = false;
    let mut after_dot = false; // '.' 后的标识符=已被前缀限定的列，原样跳过
    let mut tok = String::new();
    let flush = |tok: &mut String, out: &mut String, qualify: bool| {
        if tok.is_empty() {
            return;
        }
        let up = tok.to_uppercase();
        let word = tok.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false);
        if qualify && word && !KEYWORDS.contains(&up.as_str()) {
            out.push_str(&format!("{alias}.{tok}"));
        } else {
            out.push_str(tok);
        }
        tok.clear();
    };
    for c in expr.chars() {
        if in_quote {
            out.push(c);
            if c == '\'' {
                in_quote = false;
            }
            continue;
        }
        match c {
            '\'' => {
                flush(&mut tok, &mut out, !after_dot);
                after_dot = false;
                out.push(c);
                in_quote = true;
            }
            '.' => {
                // '.' 前的 token 是表前缀（原样），'.' 后的列已被限定（跳过）
                flush(&mut tok, &mut out, false);
                after_dot = true;
                out.push(c);
            }
            c if c.is_alphanumeric() || c == '_' => tok.push(c),
            _ => {
                flush(&mut tok, &mut out, !after_dot);
                after_dot = false;
                out.push(c);
            }
        }
    }
    flush(&mut tok, &mut out, !after_dot);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDER_SCOPE: &str = "deleted_flag = 0 AND order_status NOT IN ('0','108','199')";

    #[test]
    fn strip_literals_basics() {
        assert_eq!(strip_literals_and_comments("a 'x''y' b"), "a   b");
        assert_eq!(strip_literals_and_comments("a -- drop t\nb"), "a \nb");
        assert_eq!(strip_literals_and_comments("a /* delete */ b"), "a   b");
    }

    #[test]
    fn split_top_and_basics() {
        assert_eq!(split_top_and(ORDER_SCOPE),
                   vec!["deleted_flag = 0", "order_status NOT IN ('0','108','199')"]);
        // 括号内的 and 不切
        assert_eq!(split_top_and("a = 1 AND (b = 2 and c = 3)"), vec!["a = 1", "(b = 2 and c = 3)"]);
    }

    #[test]
    fn first_ident_strips_backticks() {
        assert_eq!(first_ident_of("`Deleted_Flag` = 0"), Some("deleted_flag".to_string()));
    }

    #[test]
    fn first_ident_none_when_no_ident() {
        assert_eq!(first_ident_of(""), None);
        assert_eq!(first_ident_of("   "), None);
        assert_eq!(first_ident_of("(a = 1)"), None); // 括号开头：无首标识符
    }
}
