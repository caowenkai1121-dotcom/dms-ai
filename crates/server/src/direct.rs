//! 确定性快路径（0-LLM）：单号直查 + 高频销售聚合模板。
//! 命中即秒级零幻觉出结果，跳过 LLM；未命中回落 pipeline 的 LLM 路径。
//! 生成的 SQL 仍过 is_safe_select + 权限注入 + 只读执行（复用 pipeline），权限不旁路。

/// 确定性命中：SQL（未注入）+ 路由标签 + 可选上期查询（KPI 环比）
pub struct DirectHit {
    pub sql: String,
    pub route: String,
    /// (上期 SQL, 环比标签如"较上月")——仅高频聚合单指标时有
    pub prev: Option<(String, String)>,
}

/// 图关系问题类型（AGE 图查询）
#[derive(Debug, PartialEq)]
pub enum Relation {
    /// 买过某商品的客户（含实体名）
    BuyersOfGoods(String),
    /// 某客户买过什么
    GoodsOfCustomer(String),
    /// 买某商品还买什么（共购）
    Copurchase(String),
}

/// 指标定义（meta.metric 行）
pub struct MetricDef {
    pub name: String,
    pub aliases: Vec<String>,
    pub source_table: String,
    pub agg_expr: String,
    pub scope_filter: String,
    /// 去重键（逗号分隔列）：该来源表含系统级重复行时必填，聚合前须按这些列 DISTINCT。
    /// 空=表无重复问题。t_sales_order_detail 实测 100.7 万行原始 vs 83.2 万去重后。
    pub dedup_keys: String,
}

/// 维度定义（meta.dimension 行）
pub struct DimDef {
    pub name: String,
    pub aliases: Vec<String>,
    pub source_table: String,
    pub expr: String,
}

/// JOIN 边（meta.join_edge 行）
pub struct JoinEdge {
    pub lt: String,
    pub lc: String,
    pub rt: String,
    pub rc: String,
    pub card: String, // lt→rt: "1:N"(扇出) / "N:1"(收敛)
}

/// 通用组合器（S3，SuperSonic 语义层组合思想）：指标×维度 数据驱动装配，退役手工模板。
/// 问句同时命中指标注册表与维度注册表 → 装配 GROUP BY 查询。门控（宁缺毋滥，装配不出就回落）：
/// 同基表直拼 / 跨基表走 join_edge BFS 路径（≤3 跳，扇出边仅 COUNT(DISTINCT) 聚合可过）、
/// 口径无子查询、实体守卫、时间窗=order_time 在 FROM 内或可经一条边桥接 t_sales_order。
pub async fn try_compose(pg: &sqlx::PgPool, question: &str) -> Option<DirectHit> {
    let metrics: Vec<(String, Vec<String>, String, String, String, String)> = sqlx::query_as(
        "SELECT name, aliases, source_table, agg_expr, scope_filter, dedup_keys FROM meta.metric WHERE status = 'active'",
    )
    .fetch_all(pg)
    .await
    .ok()?;
    let dims: Vec<(String, Vec<String>, String, String)> = sqlx::query_as(
        "SELECT name, aliases, source_table, expr FROM meta.dimension WHERE status = 'active'",
    )
    .fetch_all(pg)
    .await
    .ok()?;
    let edges: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT left_table, left_col, right_table, right_col, card FROM meta.join_edge WHERE status = 'active'",
    )
    .fetch_all(pg)
    .await
    .ok()?;
    let edges: Vec<JoinEdge> = edges
        .into_iter()
        .map(|(lt, lc, rt, rc, card)| JoinEdge { lt, lc, rt, rc, card })
        .collect();
    // 表级标准口径（SuperSonic model filter）：JOIN 到的表恒需附加的过滤
    let scopes: Vec<(String, String)> =
        sqlx::query_as("SELECT table_name, filter FROM meta.table_scope")
            .fetch_all(pg)
            .await
            .unwrap_or_default();
    let hit = |name: &str, aliases: &[String]| {
        question.contains(name) || aliases.iter().any(|a| question.contains(a.as_str()))
    };
    let m = metrics.iter().find(|(n, a, ..)| hit(n, a))?;
    let d = dims.iter().find(|(n, a, ..)| hit(n, a))?;
    let metric = MetricDef {
        name: m.0.clone(),
        aliases: m.1.clone(),
        source_table: m.2.clone(),
        agg_expr: m.3.clone(),
        scope_filter: m.4.clone(),
        dedup_keys: m.5.clone(),
    };
    let dim = DimDef {
        name: d.0.clone(),
        aliases: d.1.clone(),
        source_table: d.2.clone(),
        expr: d.3.clone(),
    };
    compose_sql_with(&metric, &dim, question, &edges, &scopes)
        .map(|sql| DirectHit { sql, route: "direct-agg".into(), prev: None })
}

/// 去注册表文本里的全角括注（维护给人类看的说明，不是 SQL 的一部分；半角括号是 SQL 语法不动）
fn strip_annotations(s: &str) -> String {
    // 全角（）恒为注记；ASCII () 仅当组内含中文才是注记（否则是真 SQL 如 SUM(col)/IN('0','1')）
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '（' || c == '(' {
            let (open, close) = if c == '（' { ('（', '）') } else { ('(', ')') };
            let mut depth = 1;
            let mut j = i + 1;
            while j < chars.len() && depth > 0 {
                if chars[j] == open {
                    depth += 1;
                } else if chars[j] == close {
                    depth -= 1;
                }
                j += 1;
            }
            let group: String = chars[i..j].iter().collect();
            let has_cjk = group.chars().any(|ch| ('\u{4E00}'..='\u{9FFF}').contains(&ch));
            if open == '(' && !has_cjk {
                out.push_str(&group);
            }
            i = j;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out.trim().to_string()
}

/// BFS 找 metric 基表 → 维度驱动表 的最短 join 路径（≤3 跳）。返回 hop 序列。
fn find_path<'a>(
    from: &str,
    to: &str,
    edges: &'a [JoinEdge],
) -> Option<Vec<(String, String, String, bool)>> {
    // hop = (to_table, to_col, from_col, fanout)
    if from == to {
        return Some(vec![]);
    }
    let mut queue: std::collections::VecDeque<(String, Vec<(String, String, String, bool)>)> =
        std::collections::VecDeque::new();
    let mut visited = std::collections::HashSet::new();
    queue.push_back((from.to_string(), vec![]));
    visited.insert(from.to_string());
    while let Some((cur, path)) = queue.pop_front() {
        if path.len() >= 3 {
            continue;
        }
        for e in edges {
            let (next, to_col, from_col, fanout) = if e.lt == cur {
                (e.rt.clone(), e.rc.clone(), e.lc.clone(), e.card == "1:N")
            } else if e.rt == cur {
                (e.lt.clone(), e.lc.clone(), e.rc.clone(), e.card == "N:1")
            } else {
                continue;
            };
            if visited.contains(&next) {
                continue;
            }
            let mut p = path.clone();
            p.push((next.clone(), to_col, from_col, fanout));
            if next == to {
                return Some(p);
            }
            visited.insert(next.clone());
            queue.push_back((next, p));
        }
    }
    None
}

/// 找两表间的直接边（时间桥用）
fn find_edge<'a>(a: &str, b: &str, edges: &'a [JoinEdge]) -> Option<(&'a JoinEdge, bool)> {
    // 返回 (edge, a_is_left)
    edges.iter().find_map(|e| {
        if e.lt == a && e.rt == b {
            Some((e, true))
        } else if e.rt == a && e.lt == b {
            Some((e, false))
        } else {
            None
        }
    })
}

/// 组合 SQL 装配（纯函数可单测）。无表级口径的简化入口，测试与旧调用点用。
#[cfg(test)]
fn compose_sql(m: &MetricDef, d: &DimDef, question: &str, edges: &[JoinEdge]) -> Option<String> {
    compose_sql_with(m, d, question, edges, &[])
}

/// 组合 SQL 装配（带表级标准口径）
fn compose_sql_with(
    m: &MetricDef,
    d: &DimDef,
    question: &str,
    edges: &[JoinEdge],
    table_scopes: &[(String, String)],
) -> Option<String> {
    // 口径/来源去中文括注（注册表文本带人类说明）
    let m_src = strip_annotations(&m.source_table);
    let m_scope = strip_annotations(&m.scope_filter);
    let m_agg = strip_annotations(&m.agg_expr);
    if m_scope.to_uppercase().contains("SELECT") || m_agg.to_uppercase().contains("SELECT") {
        return None; // 子查询内裸列归属子查询表，限定会改错——走 LLM
    }
    if m_src.to_uppercase().contains(" UNION ") {
        return None; // 多流来源（发票新老双表）须 UNION ALL 合并，模板拼不出——交 LLM 按口径卡写
    }
    if has_entity_residue(question, m, d) {
        return None; // 实体问句（恒众餐饮本月销售额）→ agg_template/LLM
    }
    let dim_base = d.source_table.split_whitespace().next()?.to_string();
    let dim_alias = d.source_table.split_whitespace().nth(1)?.to_string();
    let dim_rest: String = {
        let mut parts = d.source_table.splitn(3, char::is_whitespace);
        parts.next();
        parts.next();
        parts.next().unwrap_or("").to_string()
    };

    // 去重键：来源表含系统级重复行（ETL 双写）时，基表换成 DISTINCT 子查询再聚合，
    // 否则 SUM 直接虚增（实测明细 100.7 万行 vs 去重 83.2 万行，销量虚高 41%）。
    let dedup: Vec<String> = m
        .dedup_keys
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    // FROM 装配 + 扇出检查 + 各表别名登记
    let mut from: String;
    let mut table_aliases: Vec<(String, String)> = vec![]; // (table, alias)
    if dim_base == m_src {
        // 同基表：直接用维度来源串（含其内部 JOIN 链）
        from = d.source_table.clone();
        table_aliases.push((dim_base.clone(), dim_alias.clone()));
    } else {
        // 跨基表：BFS 路径拼接；扇出边仅 COUNT(DISTINCT) 聚合可过（防 SUM 单头列虚增）
        let path = find_path(&m_src, &dim_base, edges)?;
        if path.iter().any(|h| h.3) && !m_agg.to_uppercase().starts_with("COUNT(DISTINCT") {
            return None;
        }
        from = format!("{m_src} b0");
        table_aliases.push((m_src.clone(), "b0".to_string()));
        let mut prev_alias = "b0".to_string();
        for (i, (to, to_col, from_col, _)) in path.iter().enumerate() {
            let last = i == path.len() - 1;
            let alias = if last { dim_alias.clone() } else { format!("b{}", i + 1) };
            from.push_str(&format!(" JOIN {to} {alias} ON {alias}.{to_col} = {prev_alias}.{from_col}"));
            table_aliases.push((to.clone(), alias.clone()));
            prev_alias = alias;
        }
        if !dim_rest.is_empty() {
            from.push(' ');
            from.push_str(&dim_rest);
        }
    }
    let base_alias = table_aliases[0].1.clone();

    // 时间窗：order_time 在 FROM 内→用其别名；不在→可经一条边桥接 t_sales_order；否则不装配
    let time_and = match time_window(question) {
        Some(p) => {
            let alias = if let Some((_, a)) = table_aliases.iter().find(|(t, _)| t == "t_sales_order") {
                a.clone()
            } else if let Some((e, base_is_left)) = find_edge(&m_src, "t_sales_order", edges) {
                let (c_base, c_ord) = if base_is_left { (&e.lc, &e.rc) } else { (&e.rc, &e.lc) };
                from.push_str(&format!(
                    " JOIN t_sales_order o_time ON o_time.{c_ord} = {base_alias}.{c_base}"
                ));
                "o_time".to_string()
            } else {
                return None;
            };
            format!(" AND {}", p.replace("order_time", &format!("{alias}.order_time")))
        }
        None => String::new(),
    };

    let mut scope = if m_scope.trim().is_empty() { String::new() } else { qualify_cols(&m_scope, &base_alias) };
    let agg = qualify_cols(&m_agg, &base_alias);

    // 去重装配：基表 → (SELECT DISTINCT 键 FROM 基表 WHERE 口径) 别名。
    // 安全门控：外层对基表引用的所有列必须都在去重键里，否则子查询取不到 → 宁可不装配（回落 LLM）。
    if !dedup.is_empty() {
        let mut refs = base_col_refs(&from, &base_alias);
        refs.extend(base_col_refs(&agg, &base_alias));
        refs.extend(base_col_refs(&d.expr, &base_alias));
        refs.extend(base_col_refs(&time_and, &base_alias));
        if !refs.iter().all(|c| dedup.contains(c)) {
            return None;
        }
        let keys = dedup.join(", ");
        let inner_where = if m_scope.trim().is_empty() {
            String::new()
        } else {
            format!(" WHERE {m_scope}")
        };
        let sub = format!("(SELECT DISTINCT {keys} FROM {m_src}{inner_where}) {base_alias}");
        // 替换 FROM 首段的 `基表 别名`（同基表分支）或 `基表 b0`（跨基表分支）
        let head = format!("{m_src} {base_alias}");
        if !from.starts_with(&head) {
            return None;
        }
        from = format!("{sub}{}", &from[head.len()..]);
        scope.clear(); // 口径过滤已下推进子查询
    }

    // 表级标准口径：FROM 中每张登记表按其别名附加恒成立过滤（明细指标桥接订单主表时
    // 漏掉「有效订单」是数值虚增的头号来源——评测抓获销量虚高 41%）。
    // 跳过已被去重子查询替换的基表（其口径已下推）。
    let mut scope_parts: Vec<String> = vec![];
    if !scope.is_empty() {
        scope_parts.push(scope.clone());
    }
    for (t, alias) in from_table_aliases(&from) {
        if !dedup.is_empty() && alias == base_alias {
            continue;
        }
        if let Some((_, f)) = table_scopes.iter().find(|(tn, _)| *tn == t) {
            let qualified = qualify_cols(f, &alias);
            if !scope_parts.contains(&qualified) {
                scope_parts.push(qualified);
            }
        }
    }
    let scope = scope_parts.join(" AND ");
    let where_sql = match (scope.is_empty(), time_and.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("WHERE {scope}"),
        (true, false) => format!("WHERE {}", time_and.trim_start_matches(" AND ")),
        (false, false) => format!("WHERE {scope}{time_and}"),
    };
    let lim = detect_top_n(question);
    // 时间维度按时间排序（趋势语义），其余按指标降序
    let order = if d.expr.contains("DATE_FORMAT") || d.expr.contains("order_time") {
        format!("ORDER BY {} LIMIT {lim}", d.expr)
    } else {
        format!("ORDER BY `{}` DESC LIMIT {lim}", m.name)
    };
    Some(format!(
        "SELECT {} AS `{}`, {} AS `{}`\nFROM {}\n{}\nGROUP BY {}\n{order}",
        d.expr, d.name, agg, m.name, from, where_sql, d.expr
    ))
}

/// 从 FROM 串里解析出 (真实表名, 别名) 列表：`t_x a JOIN t_y b ON ...` / `(子查询) a`。
/// 纯文本扫描（组合器自己拼的串形态固定），子查询段跳过。纯函数可单测。
fn from_table_aliases(from: &str) -> Vec<(String, String)> {
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
fn base_col_refs(frag: &str, alias: &str) -> Vec<String> {
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

/// 残留守卫（纯函数）：把问句里被模板/组合器「消化掉」的词剥光后，
/// 若还剩实义字（CJK/字母数字）→ 说明问句含模板表达不了的限定（实体名、值过滤、
/// 未支持的维度），必须回落 LLM，绝不能装配一条**丢掉限定**的 SQL 静默答错。
///
/// 真实翻车（回归 E16 抓获）：「线下客户本月销售额」被销售额×客户模板装配成
/// 「全部客户 TOP200 销售额」——"线下"这个客户分类过滤被静默丢弃，答非所问。
fn has_residue(question: &str, consumed: &[String]) -> bool {
    let mut s = question.to_string();
    // 先剥业务词（长词优先，防"客户分类"被"客户"拆散后留下"分类"）
    let mut words: Vec<&String> = consumed.iter().collect();
    words.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    for w in words {
        s = s.replace(w.as_str(), "");
    }
    // 再剥通用虚词/时间词/排序词
    for w in [
        "今天", "今日", "昨天", "昨日", "本月", "这个月", "上月", "上个月", "本周", "这周", "今年",
        "上周", "去年", "近", "最近", "天", "周", "月", "年", "季度", "至今",
        "按", "各", "的", "是多少", "多少", "有", "查", "查询", "统计", "看看", "帮我", "我", "一下",
        "排行", "排名", "前", "第", "名", "top", "TOP", "对比", "和", "与", "分别",
        "一", "二", "三", "四", "五", "六", "七", "八", "九", "十", "百",
    ] {
        s = s.replace(w, "");
    }
    let s: String = s
        .chars()
        .filter(|c| !c.is_ascii_digit() && !c.is_whitespace() && !"，。？?、,.~～!！:：".contains(*c))
        .collect();
    s.chars().any(|c| c.is_alphanumeric() || (c as u32) > 0x2E7F)
}

/// 组合器专用：消化词 = 指标名/别名 + 维度名/别名
fn has_entity_residue(question: &str, m: &MetricDef, d: &DimDef) -> bool {
    let mut words: Vec<String> = vec![m.name.clone(), d.name.clone()];
    words.extend(m.aliases.iter().cloned());
    words.extend(d.aliases.iter().cloned());
    has_residue(question, &words)
}

/// 裸列限定到基表别名：非函数、未限定、非关键字的标识符 → alias.col。
/// 单引号字面量段原样跳过；已有前缀（a.col）的列原样跳过。纯函数可单测。
fn qualify_cols(expr: &str, alias: &str) -> String {
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
    let mut flush = |tok: &mut String, out: &mut String, qualify: bool| {
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

/// 识别图关系问题并抽实体名。顺序敏感：共购(还买)先于买过，买过先于"X买了"。
pub fn detect_relation(q: &str) -> Option<Relation> {
    // 共购：买X还买 / 买了X还买什么
    if (q.contains("还买") || q.contains("还购买") || q.contains("关联购买") || q.contains("一起买")) && q.contains("买") {
        let name = strip_relation_words(q);
        if !name.is_empty() {
            return Some(Relation::Copurchase(name));
        }
    }
    // 买过 X 的客户 / 哪些客户买过 X
    if (q.contains("买过") || q.contains("购买过") || q.contains("买了")) && (q.contains("客户") || q.contains("哪些") || q.contains("门店")) {
        let name = strip_relation_words(q);
        if !name.is_empty() {
            return Some(Relation::BuyersOfGoods(name));
        }
    }
    // X 买过什么 / X 买了哪些商品
    if (q.contains("买过什么") || q.contains("买了什么") || q.contains("买过哪些") || q.contains("买了哪些") || q.contains("购买清单")) {
        let name = strip_relation_words(q);
        if !name.is_empty() {
            return Some(Relation::GoodsOfCustomer(name));
        }
    }
    None
}

/// 剥关系词/疑问词，剩下实体名
fn strip_relation_words(q: &str) -> String {
    let mut s = q.to_string();
    for w in [
        "还买过什么", "还买什么", "还买了什么", "还购买", "还买", "关联购买", "一起买",
        "买过什么", "买了什么", "买过哪些", "买了哪些", "购买清单", "购买过", "买过", "买了",
        "的客户", "哪些客户", "哪些门店", "哪些", "客户", "门店", "商品", "有", "的", "是", "什么", "都", "买",
    ] {
        s = s.replace(w, "");
    }
    s.trim().to_string()
}

pub fn try_direct(question: &str) -> Option<DirectHit> {
    sniff_doc_code(question)
        .or_else(|| sales_breakdown(question))
        .or_else(|| agg_template(question))
}

/// 销售额按维度下钻（0-LLM 确定性模板，口径固化——修复 LLM 下钻拐到营销表算错的问题）。
/// 连接键已连库坐实：detail.sku_code=t_goods.goods_code、goods.goods_category_code=cat.id。
fn sales_breakdown(question: &str) -> Option<DirectHit> {
    // 必须是销售额类 + 维度（时间窗可选，无则查全部）
    if !(question.contains("销售额") || question.contains("销售总额") || question.contains("营业额")
        || question.contains("卖了多少") || question.contains("业绩") || question.contains("销售业绩"))
    {
        return None;
    }
    let dim = detect_sales_dim(question)?;
    // 残留守卫：模板只会「指标×维度」，问句里若还剩实义词（如「**线下**客户本月销售额」的
    // 客户分类限定），装配出来的 SQL 会静默丢掉该限定 → 必须回落 LLM。
    if has_residue(question, &consumed_words(&dim)) {
        return None;
    }
    // 有时间窗则加时间过滤，无则查全部（对齐 SuperSonic：问题没提时间就别加）
    let time_and = match time_window(question) {
        Some(p) => format!(" AND {}", p.replace("order_time", "o.order_time")),
        None => String::new(),
    };
    let lim = detect_top_n(question); // "前5/前十"→5/10，默认 50
    let base_where = format!(
        "o.deleted_flag = 0 AND o.order_status NOT IN ('0','108','199'){time_and}"
    );
    let sql = match dim {
        // 商品分类走明细（金额在明细级）。o 先过滤（时间窗+权限，订单数少）驱动，
        // JOIN detail 相关连接（sales_order_code 有索引，不全表扫），DISTINCT 去 2x 重复行。
        SalesDim::Category => format!(
            "SELECT COALESCE(cat.category_name,'未分类') AS `商品分类`, SUM(dd.amount) AS `销售额`
             FROM (
               SELECT DISTINCT d.sales_order_code, d.sku_code, d.box_quantity, d.bag_quantity, d.amount
               FROM t_sales_order o
               JOIN t_sales_order_detail d ON d.sales_order_code = o.sales_order_code AND d.deleted_flag = 0
               WHERE {base_where}
             ) dd
             JOIN t_goods g ON g.goods_code = dd.sku_code AND g.deleted_flag = 0
             LEFT JOIN t_goods_category cat ON g.goods_category_code = cat.id
             GROUP BY COALESCE(cat.category_name,'未分类') ORDER BY `销售额` DESC LIMIT {lim}"
        ),
        // 以下维度金额用单头 total_amount
        SalesDim::Province => format!(
            "SELECT COALESCE(NULLIF(cus.province,''),'未知') AS `省份`, SUM(o.total_amount) AS `销售额`
             FROM t_sales_order o
             LEFT JOIN t_customer cus ON cus.customer_code = o.customer_code AND cus.deleted_flag = 0
             WHERE {base_where}
             GROUP BY COALESCE(NULLIF(cus.province,''),'未知') ORDER BY `销售额` DESC LIMIT {lim}"
        ),
        SalesDim::Owner => format!(
            "SELECT COALESCE(e.actual_name, o.owner_manager) AS `业务员`, SUM(o.total_amount) AS `销售额`
             FROM t_sales_order o
             LEFT JOIN t_employee e ON e.employee_id = o.owner_manager
             WHERE {base_where}
             GROUP BY COALESCE(e.actual_name, o.owner_manager) ORDER BY `销售额` DESC LIMIT {lim}"
        ),
        SalesDim::Customer => format!(
            "SELECT COALESCE(o.customer_name,'未知') AS `客户`, SUM(o.total_amount) AS `销售额`
             FROM t_sales_order o WHERE {base_where}
             GROUP BY COALESCE(o.customer_name,'未知') ORDER BY `销售额` DESC LIMIT {lim}"
        ),
        SalesDim::Shop => format!(
            "SELECT COALESCE(o.shop_name,'未知') AS `门店`, SUM(o.total_amount) AS `销售额`
             FROM t_sales_order o WHERE {base_where}
             GROUP BY COALESCE(o.shop_name,'未知') ORDER BY `销售额` DESC LIMIT {lim}"
        ),
        SalesDim::Month => format!(
            "SELECT DATE_FORMAT(o.order_time,'%Y-%m') AS `月份`, SUM(o.total_amount) AS `销售额`
             FROM t_sales_order o WHERE {base_where}
             GROUP BY DATE_FORMAT(o.order_time,'%Y-%m') ORDER BY `月份`"
        ),
    };
    Some(DirectHit { sql, route: "direct-agg".into(), prev: None })
}

#[derive(Debug, PartialEq)]
enum SalesDim {
    Province,
    Category,
    Owner,
    Customer,
    Shop,
    Month,
}

/// 该模板能「消化」的词：销售额同义词 + 命中维度的全部触发词/输出词。
/// 剥这些词后仍有实义残留 = 模板表达不了的限定 → 回落 LLM。
fn consumed_words(dim: &SalesDim) -> Vec<String> {
    let mut w: Vec<&str> = vec![
        "销售额", "销售总额", "营业额", "卖了多少", "销售业绩", "业绩", "销售",
    ];
    w.extend(match dim {
        SalesDim::Category => vec!["商品分类", "商品品类", "分类", "品类", "类别", "商品"],
        SalesDim::Province => vec!["省份", "各省", "省市", "省", "地区", "区域"],
        SalesDim::Owner => vec!["业务员", "销售员", "经理", "负责人", "员工", "人员"],
        SalesDim::Customer => vec!["客户", "客户名", "经销商"],
        SalesDim::Shop => vec!["门店", "店铺", "终端", "店"],
        SalesDim::Month => vec!["月份", "按月", "每月", "各月", "月度", "趋势"],
    });
    w.into_iter().map(|s| s.to_string()).collect()
}

fn detect_sales_dim(q: &str) -> Option<SalesDim> {
    // 顺序敏感：分类先于客户（"客户分类"罕见），业务员先于客户
    // 「客户分类/客户类型」是客户维度（字典码 CustClassif/CUST_TYPE），不是商品分类——
    // 无确定性模板，回落 LLM 由维度口径卡接管（误走商品分类模板=答非所问）
    if q.contains("客户分类") || q.contains("客户类别") || q.contains("客户类型") || q.contains("客户种类") {
        return None;
    }
    if q.contains("分类") || q.contains("品类") || q.contains("类别") {
        Some(SalesDim::Category)
    } else if q.contains("省") {
        Some(SalesDim::Province)
    } else if q.contains("业务员") || q.contains("经理") || q.contains("负责人") || q.contains("员工") {
        Some(SalesDim::Owner)
    } else if q.contains("门店") || q.contains("店") {
        Some(SalesDim::Shop)
    } else if q.contains("客户") {
        Some(SalesDim::Customer)
    } else if q.contains("月份") || q.contains("按月") || q.contains("每月") || q.contains("各月") {
        Some(SalesDim::Month)
    } else {
        None
    }
}

/// 单据前缀 → (表, 主号列)。后缀字母区分单据类型，区分度足够（免 UNION 探测开销）。
fn doc_binding(code: &str) -> Option<(&'static str, &'static str)> {
    let up = code.to_uppercase();
    if up.starts_with("SPC-") {
        return Some(("t_winc_purchase_transfer", "bill_code"));
    }
    // HJXH-D**xxxx：按第 6-8 位单据类型字母段
    if let Some(rest) = up.strip_prefix("HJXH-") {
        let tag: String = rest.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        return match tag.as_str() {
            "DXO" | "DSO" | "XO" | "SO" => Some(("t_sales_order", "sales_order_code")),
            "DRO" | "RO" => Some(("t_after_sales_order_header", "after_sales_code")),
            "DZD" | "ZD" => Some(("t_account_bill_header", "bill_code")),
            _ => None,
        };
    }
    None
}

/// 从问句抽单号（HJXH-字母+数字 / SPC-日期-序号），命中即出单据卡（SELECT * 单行）。
fn sniff_doc_code(question: &str) -> Option<DirectHit> {
    for token in question.split(|c: char| c.is_whitespace() || matches!(c, '，' | ',' | '。' | '的' | '是')) {
        let t = token.trim();
        if t.len() < 6 {
            continue;
        }
        // 单号字符集：字母数字与连字符
        if !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            continue;
        }
        if let Some((table, col)) = doc_binding(t) {
            // 单号里不含引号，无注入风险；仍转义防御
            let safe = t.replace('\'', "''");
            return Some(DirectHit {
                sql: format!("SELECT * FROM {table} WHERE {col} = '{safe}' LIMIT 1"),
                route: "direct-doc".into(),
                prev: None,
            });
        }
    }
    None
}

/// 高频销售聚合模板：时间窗 + 单指标，无维度、无实体（含则回落 LLM 做 GROUP BY/实体锚定）。
fn agg_template(question: &str) -> Option<DirectHit> {
    // 维度词（触发分组下钻，回落 sales_breakdown/LLM）。不含"客户/商品"——它们是实体名常见字，
    // "各客户/按商品"靠"各/按"拦，避免误伤"成交客户数""商品销量"这类指标问句。
    const DIM_WORDS: &[&str] = &["排行", "排名", "前", "各", "按", "分类", "省", "市", "区域", "门店", "占比", "对比", "趋势", "明细"];
    if DIM_WORDS.iter().any(|w| question.contains(w)) {
        return None;
    }
    // 剥词守卫（旧项目实证）：去掉时间/指标/语气/连接词后仍有残留=实体问句，回落 LLM。
    // 例：「恒众餐饮本月销售额」剥后剩「恒众餐饮」→ 不命中；「本月销售额是多少」剥后空→命中。
    let mut stripped = question.to_string();
    for w in [
        "今天", "今日", "昨天", "昨日", "本月", "这个月", "上月", "上个月", "本周", "这周", "今年",
        "销售额", "销售总额", "营业额", "订单数", "多少单", "几单", "客单价", "卖了多少",
        "成交客户数", "成交客户", "客户数", "多少客户",
        "是多少", "多少", "有", "的", "呢", "吗", "总共", "一共", "了", "查", "查询", "看看", "帮我",
    ] {
        stripped = stripped.replace(w, "");
    }
    if stripped.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }
    let time_pred = time_window(question)?;
    let metric = if question.contains("客户数") || question.contains("成交客户") || question.contains("多少客户") {
        "COUNT(DISTINCT customer_code) AS `成交客户数`"
    } else if question.contains("订单数") || question.contains("多少单") || question.contains("几单") {
        "COUNT(DISTINCT sales_order_code) AS `订单数`"
    } else if question.contains("客单价") {
        "ROUND(SUM(total_amount)/NULLIF(COUNT(DISTINCT sales_order_code),0), 2) AS `客单价`"
    } else if question.contains("销售额") || question.contains("销售总额") || question.contains("营业额") || question.contains("卖了多少") {
        "SUM(total_amount) AS `销售额`"
    } else {
        return None;
    };
    let base = |pred: &str| {
        format!(
            "SELECT {metric} FROM t_sales_order \
             WHERE deleted_flag = 0 AND order_status NOT IN ('0','108','199') AND {pred}"
        )
    };
    // 上期查询（环比）：平移时间窗
    let prev = prev_window(question).map(|(pred, label)| (base(pred), label.to_string()));
    Some(DirectHit {
        sql: base(&time_pred),
        route: "direct-agg".into(),
        prev,
    })
}

/// "前N/topN" → 限制条数（中文数字支持），默认 50
fn detect_top_n(q: &str) -> usize {
    const CN: &[(&str, usize)] = &[
        ("十", 10), ("两", 2), ("一", 1), ("二", 2), ("三", 3), ("四", 4),
        ("五", 5), ("六", 6), ("七", 7), ("八", 8), ("九", 9),
    ];
    // "前N" / "前十"
    if let Some(pos) = q.find('前') {
        let after = &q[pos + '前'.len_utf8()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<usize>() {
            if (1..=200).contains(&n) {
                return n;
            }
        }
        for (cn, v) in CN {
            if after.starts_with(cn) {
                return *v;
            }
        }
    }
    // "topN"
    let lower = q.to_lowercase();
    if let Some(pos) = lower.find("top") {
        let digits: String = lower[pos + 3..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<usize>() {
            if (1..=200).contains(&n) {
                return n;
            }
        }
    }
    // 未提 TopN → 不截断到小值：分组基数常超 50（商品分类 60 个），
    // 截断会让"各分类销量"静默少 10 个分类且用户无感（评测抓获）。对齐全局 MAX_ROWS。
    200
}

/// 时间窗 → 上一期谓词 + 环比标签
fn prev_window(q: &str) -> Option<(&'static str, &'static str)> {
    if q.contains("今天") || q.contains("今日") {
        Some(("DATE(order_time) = CURDATE() - INTERVAL 1 DAY", "较昨天"))
    } else if q.contains("昨天") || q.contains("昨日") {
        Some(("DATE(order_time) = CURDATE() - INTERVAL 2 DAY", "较前天"))
    } else if q.contains("本月") || q.contains("这个月") {
        Some(("order_time >= DATE_FORMAT(CURDATE() - INTERVAL 1 MONTH,'%Y-%m-01') AND order_time < DATE_FORMAT(CURDATE(),'%Y-%m-01')", "较上月"))
    } else if q.contains("上月") || q.contains("上个月") {
        Some(("order_time >= DATE_FORMAT(CURDATE() - INTERVAL 2 MONTH,'%Y-%m-01') AND order_time < DATE_FORMAT(CURDATE() - INTERVAL 1 MONTH,'%Y-%m-01')", "较上上月"))
    } else if q.contains("本周") || q.contains("这周") {
        Some(("YEARWEEK(order_time, 1) = YEARWEEK(CURDATE() - INTERVAL 7 DAY, 1)", "较上周"))
    } else if q.contains("今年") {
        Some(("YEAR(order_time) = YEAR(CURDATE()) - 1", "较去年"))
    } else {
        None
    }
}

/// 相对时间词 → MySQL 谓词（基于 CURDATE()，零硬编码年份）
/// 中文数字 → 阿拉伯数字（仅覆盖 1~99，够用于「近三个月」「第二季度」这类问法）
fn cn_num(s: &str) -> Option<u32> {
    const D: &[(&str, u32)] = &[
        ("零", 0), ("一", 1), ("两", 2), ("二", 2), ("三", 3), ("四", 4),
        ("五", 5), ("六", 6), ("七", 7), ("八", 8), ("九", 9),
    ];
    if let Ok(n) = s.parse::<u32>() {
        return Some(n);
    }
    let c: Vec<&str> = s.split("").filter(|x| !x.is_empty()).collect();
    let val = |x: &str| D.iter().find(|(k, _)| *k == x).map(|(_, v)| *v);
    match c.as_slice() {
        [a] if *a == "十" => Some(10),
        [a] => val(a),
        ["十", b] => val(b).map(|v| 10 + v),               // 十二
        [a, "十"] => val(a).map(|v| v * 10),                // 三十
        [a, "十", b] => Some(val(a)? * 10 + val(b)?),       // 三十五
        _ => None,
    }
}

/// 抽「近/过去/最近 N 天|周|月|年」里的 N 与单位
fn recent_n(q: &str) -> Option<(u32, &'static str)> {
    for lead in ["最近", "过去", "近"] {
        let Some(pos) = q.find(lead) else { continue };
        let rest: String = q[pos + lead.len()..].chars().take(6).collect();
        let num: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || "零一两二三四五六七八九十".contains(*c))
            .collect();
        if num.is_empty() {
            continue;
        }
        let Some(n) = cn_num(&num) else { continue };
        let tail = &rest[num.len()..];
        let unit = if tail.starts_with('天') || tail.starts_with('日') {
            "DAY"
        } else if tail.starts_with('周') || tail.starts_with("个周") || tail.starts_with('星') {
            "WEEK"
        } else if tail.starts_with('月') || tail.starts_with("个月") {
            "MONTH"
        } else if tail.starts_with('年') {
            "YEAR"
        } else {
            continue;
        };
        if n >= 1 && n <= 60 {
            return Some((n, unit));
        }
    }
    None
}

/// 规则时间解析（移植 SuperSonic TimeRangeParser 思路）：问句 → 半开区间 [起, 止)。
/// 返回的是**列名占位为 `{}` 的谓词模板**，调用方填真实时间列。
/// 时间是 BI 最高频错误源；能规则解析的一律不交给 LLM 猜。
pub fn time_predicate(q: &str) -> Option<String> {
    // 近 N 天/周/月/年（含中文数字）
    if let Some((n, unit)) = recent_n(q) {
        return Some(format!(
            "{{}} >= DATE_SUB(CURDATE(), INTERVAL {n} {unit}) AND {{}} < DATE_ADD(CURDATE(), INTERVAL 1 DAY)"
        ));
    }
    // 第 N 季度 / 本季度 / 上季度
    if let Some(pos) = q.find("季度") {
        let head: String = q[..pos].chars().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect();
        let qn = ["一", "二", "三", "四"]
            .iter()
            .position(|c| head.contains(c))
            .map(|i| i as u32 + 1)
            .or_else(|| head.chars().rev().find(|c| ('1'..='4').contains(c)).and_then(|c| c.to_digit(10)));
        if let Some(n) = qn {
            let start_month = (n - 1) * 3 + 1;
            return Some(format!(
                "{{}} >= DATE_FORMAT(CONCAT(YEAR(CURDATE()),'-{start_month:02}-01'),'%Y-%m-%d') \
                 AND {{}} < DATE_ADD(DATE_FORMAT(CONCAT(YEAR(CURDATE()),'-{start_month:02}-01'),'%Y-%m-%d'), INTERVAL 3 MONTH)"
            ));
        }
        if head.contains('本') || head.contains('这') {
            return Some("QUARTER({}) = QUARTER(CURDATE()) AND YEAR({}) = YEAR(CURDATE())".into());
        }
        if head.contains('上') {
            return Some(
                "{} >= DATE_SUB(MAKEDATE(YEAR(CURDATE()),1) + INTERVAL QUARTER(CURDATE())*3-3 MONTH, INTERVAL 3 MONTH) \
                 AND {} < MAKEDATE(YEAR(CURDATE()),1) + INTERVAL QUARTER(CURDATE())*3-3 MONTH"
                    .into(),
            );
        }
    }
    // 上半年 / 下半年（本年度）
    if q.contains("上半年") {
        return Some(
            "{} >= DATE_FORMAT(CURDATE(),'%Y-01-01') AND {} < DATE_FORMAT(CURDATE(),'%Y-07-01')".into(),
        );
    }
    if q.contains("下半年") {
        return Some(
            "{} >= DATE_FORMAT(CURDATE(),'%Y-07-01') AND {} < DATE_FORMAT(DATE_ADD(CURDATE(), INTERVAL 1 YEAR),'%Y-01-01')".into(),
        );
    }
    // N 月 / N 月份（本年度；「上个月」等相对词在下方兜底，先排除）
    if !q.contains("个月") && !q.contains("上月") {
        if let Some(pos) = q.find('月') {
            let head: String = q[..pos].chars().rev().take(2).collect::<Vec<_>>().into_iter().rev().collect();
            let num: String = head
                .chars()
                .filter(|c| c.is_ascii_digit() || "一两二三四五六七八九十".contains(*c))
                .collect();
            if let Some(m) = cn_num(&num).filter(|m| (1..=12).contains(m)) {
                return Some(format!(
                    "{{}} >= DATE_FORMAT(CONCAT(YEAR(CURDATE()),'-{m:02}-01'),'%Y-%m-%d') \
                     AND {{}} < DATE_ADD(DATE_FORMAT(CONCAT(YEAR(CURDATE()),'-{m:02}-01'),'%Y-%m-%d'), INTERVAL 1 MONTH)"
                ));
            }
        }
    }
    // 相对词兜底
    let p = if q.contains("今天") || q.contains("今日") {
        "DATE({}) = CURDATE()"
    } else if q.contains("昨天") || q.contains("昨日") {
        "DATE({}) = CURDATE() - INTERVAL 1 DAY"
    } else if q.contains("前天") {
        "DATE({}) = CURDATE() - INTERVAL 2 DAY"
    } else if q.contains("本月") || q.contains("这个月") || q.contains("当月") {
        "{} >= DATE_FORMAT(CURDATE(),'%Y-%m-01') AND {} < DATE_ADD(DATE_FORMAT(CURDATE(),'%Y-%m-01'), INTERVAL 1 MONTH)"
    } else if q.contains("上月") || q.contains("上个月") {
        "{} >= DATE_FORMAT(CURDATE() - INTERVAL 1 MONTH,'%Y-%m-01') AND {} < DATE_FORMAT(CURDATE(),'%Y-%m-01')"
    } else if q.contains("本周") || q.contains("这周") {
        "YEARWEEK({}, 1) = YEARWEEK(CURDATE(), 1)"
    } else if q.contains("上周") {
        "YEARWEEK({}, 1) = YEARWEEK(CURDATE() - INTERVAL 1 WEEK, 1)"
    } else if q.contains("今年") || q.contains("本年") || q.contains("年初至今") {
        "YEAR({}) = YEAR(CURDATE())"
    } else if q.contains("去年") {
        "YEAR({}) = YEAR(CURDATE()) - 1"
    } else {
        return None;
    };
    Some(p.to_string())
}

/// 谓词模板填入真实时间列
pub fn fill_time_col(tpl: &str, col: &str) -> String {
    tpl.replace("{}", col)
}

fn time_window(q: &str) -> Option<String> {
    time_predicate(q).map(|tpl| fill_time_col(&tpl, "order_time"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_prefixes() {
        assert_eq!(doc_binding("HJXH-DXO2026072300384").unwrap().0, "t_sales_order");
        assert_eq!(doc_binding("HJXH-DRO2026072300047").unwrap().0, "t_after_sales_order_header");
        assert_eq!(doc_binding("HJXH-DZD20261230000261").unwrap().0, "t_account_bill_header");
        assert_eq!(doc_binding("SPC-20260718-8").unwrap().0, "t_winc_purchase_transfer");
        assert!(doc_binding("HJXH-XXX123").is_none());
    }

    #[test]
    fn sniff_in_sentence() {
        let h = sniff_doc_code("帮我查下 HJXH-DXO2026072300384 这张单").unwrap();
        assert!(h.sql.contains("t_sales_order"));
        assert!(h.sql.contains("HJXH-DXO2026072300384"));
        assert_eq!(h.route, "direct-doc");
    }

    #[test]
    fn agg_hits_month_sales() {
        let h = agg_template("本月销售额是多少").unwrap();
        assert!(h.sql.contains("SUM(total_amount)"));
        assert!(h.sql.contains("NOT IN ('0','108','199')"));
        assert!(h.sql.contains("DATE_FORMAT"));
        assert_eq!(h.route, "direct-agg");
    }

    #[test]
    fn agg_order_count() {
        let h = agg_template("今天有多少订单数").unwrap();
        assert!(h.sql.contains("COUNT(DISTINCT sales_order_code)"));
        assert!(h.sql.contains("DATE(order_time) = CURDATE()"));
    }

    #[test]
    fn agg_skips_dimension() {
        // 带维度词 → 回落 LLM
        assert!(agg_template("本月销售额前五的省份").is_none());
        assert!(agg_template("各商品分类的销量").is_none());
        assert!(agg_template("恒众餐饮本月销售额").is_none()); // 含"客户"实体? 不，含"恒众"但无维度词——靠"客户"词挡不住
    }

    #[test]
    fn agg_needs_time_and_metric() {
        assert!(agg_template("销售额").is_none()); // 无时间窗
        assert!(agg_template("本月天气").is_none()); // 无指标
    }

    #[test]
    fn top_n_detect() {
        assert_eq!(detect_top_n("本月销售额前5的省份"), 5);
        assert_eq!(detect_top_n("销售额前十的客户"), 10);
        assert_eq!(detect_top_n("前三名商品分类"), 3);
        assert_eq!(detect_top_n("销售额top20省份"), 20);
        // 无前N默认 200（对齐全局 MAX_ROWS）：50 会把 60 个商品分类静默截成 50
        assert_eq!(detect_top_n("各省份销售额"), 200);
    }

    #[test]
    fn sales_breakdown_top_n() {
        let h = sales_breakdown("本月销售额前5的省份").unwrap();
        assert!(h.sql.contains("LIMIT 5"), "{}", h.sql);
        let h2 = sales_breakdown("本月销售额按客户").unwrap();
        assert!(h2.sql.contains("LIMIT 200"), "{}", h2.sql);
    }

    #[test]
    fn sales_breakdown_dims() {
        // 商品分类下钻走确定性模板，用 t_sales_order/detail 正确口径（非 marketing_goods）
        let h = sales_breakdown("本月销售额是多少 按商品分类").unwrap();
        assert!(h.sql.contains("t_goods_category"), "{}", h.sql);
        assert!(h.sql.contains("t_sales_order_detail"), "{}", h.sql);
        assert!(!h.sql.contains("marketing_goods"), "{}", h.sql);
        assert!(h.sql.contains("NOT IN ('0','108','199')"), "{}", h.sql);
        assert_eq!(h.route, "direct-agg");
        // 省份下钻 JOIN t_customer
        let p = sales_breakdown("本月销售额 按省份").unwrap();
        assert!(p.sql.contains("t_customer") && p.sql.contains("province"), "{}", p.sql);
        // 业务员下钻 JOIN t_employee
        let o = sales_breakdown("本月销售额按业务员").unwrap();
        assert!(o.sql.contains("t_employee") && o.sql.contains("owner_manager"), "{}", o.sql);
        // 无维度不命中（交给 agg_template）
        assert!(sales_breakdown("本月销售额是多少").is_none());
        // 非销售额不命中
        assert!(sales_breakdown("本月订单数按省份").is_none());
        // 「客户分类/客户类型」是客户维度（CustClassif/CUST_TYPE 字典码），不是商品分类——回落 LLM 维度卡接管
        assert!(sales_breakdown("本月销售额按客户分类").is_none());
        assert!(sales_breakdown("销售额按客户类型").is_none());
    }

    #[test]
    fn relation_detect() {
        assert_eq!(detect_relation("买过烤肠的客户有哪些"), Some(Relation::BuyersOfGoods("烤肠".into())));
        assert_eq!(detect_relation("恒众买过什么"), Some(Relation::GoodsOfCustomer("恒众".into())));
        // 共购：还买优先
        assert_eq!(detect_relation("买烤肠的还买什么"), Some(Relation::Copurchase("烤肠".into())));
        assert!(detect_relation("本月销售额").is_none());
    }

    fn sales_metric() -> MetricDef {
        MetricDef {
            name: "销售额".into(),
            aliases: vec!["业绩".into()],
            source_table: "t_sales_order".into(),
            agg_expr: "SUM(total_amount)".into(),
            scope_filter: "deleted_flag = 0 AND order_status NOT IN ('0','108','199')".into(),
            dedup_keys: String::new(),
        }
    }
    fn qty_metric() -> MetricDef {
        MetricDef {
            name: "销量".into(),
            aliases: vec![],
            source_table: "t_sales_order_detail(JOIN t_sales_order 有效订单)".into(),
            agg_expr: "SUM(box_quantity)".into(),
            scope_filter: "item_type = '1'".into(),
            dedup_keys: "sales_order_code,sku_code,sku_name,box_quantity,amount".into(),
        }
    }
    fn dim(name: &str, expr: &str) -> DimDef {
        DimDef {
            name: name.into(),
            aliases: vec![],
            source_table: "t_sales_order o LEFT JOIN t_customer cus ON cus.customer_code = o.customer_code AND cus.deleted_flag = 0".into(),
            expr: expr.into(),
        }
    }
    fn cat_dim() -> DimDef {
        DimDef {
            name: "商品分类".into(),
            aliases: vec![],
            source_table: "t_sales_order_detail d JOIN t_goods g ON g.goods_code = d.sku_code AND g.deleted_flag = 0 LEFT JOIN t_goods_category cat ON g.goods_category_code = cat.id".into(),
            expr: "COALESCE(cat.category_name,'未分类')".into(),
        }
    }
    fn edges() -> Vec<JoinEdge> {
        vec![
            JoinEdge { lt: "t_sales_order".into(), lc: "sales_order_code".into(), rt: "t_sales_order_detail".into(), rc: "sales_order_code".into(), card: "1:N".into() },
            JoinEdge { lt: "t_sales_order".into(), lc: "customer_code".into(), rt: "t_customer".into(), rc: "customer_code".into(), card: "N:1".into() },
            JoinEdge { lt: "t_sales_order".into(), lc: "owner_manager".into(), rt: "t_employee".into(), rc: "employee_id".into(), card: "N:1".into() },
            JoinEdge { lt: "t_sales_order_detail".into(), lc: "sku_code".into(), rt: "t_goods".into(), rc: "goods_code".into(), card: "N:1".into() },
            JoinEdge { lt: "t_goods".into(), lc: "goods_category_code".into(), rt: "t_goods_category".into(), rc: "id".into(), card: "N:1".into() },
        ]
    }

    #[test]
    fn qualify_bare_cols() {
        // 裸列限定、引号字面量跳过、已有前缀跳过、函数名跳过
        assert_eq!(
            qualify_cols("deleted_flag = 0 AND order_status NOT IN ('0','108','199')", "o"),
            "o.deleted_flag = 0 AND o.order_status NOT IN ('0','108','199')"
        );
        assert_eq!(qualify_cols("SUM(total_amount)", "o"), "SUM(o.total_amount)");
        assert_eq!(
            qualify_cols("COUNT(DISTINCT sales_order_code)", "o"),
            "COUNT(DISTINCT o.sales_order_code)"
        );
        assert_eq!(
            qualify_cols("COALESCE(NULLIF(cus.province,''),'未知')", "o"),
            "COALESCE(NULLIF(cus.province,''),'未知')"
        );
    }

    #[test]
    fn compose_province() {
        let sql = compose_sql(&sales_metric(), &dim("省份", "COALESCE(NULLIF(cus.province,''),'未知')"), "本月销售额按省份", &edges()).unwrap();
        assert!(sql.contains("FROM t_sales_order o LEFT JOIN t_customer"), "{sql}");
        assert!(sql.contains("SUM(o.total_amount)"), "{sql}");
        assert!(sql.contains("o.deleted_flag = 0"), "{sql}");
        assert!(sql.contains("o.order_time >="), "{sql}");
        assert!(sql.contains("GROUP BY COALESCE(NULLIF(cus.province,''),'未知')"), "{sql}");
    }

    #[test]
    fn compose_entity_question_skipped() {
        // 实体残留（恒众餐饮）→ 不装配
        assert!(compose_sql(&sales_metric(), &dim("客户", "COALESCE(o.customer_name,'未知')"), "恒众餐饮本月销售额按客户", &edges()).is_none());
    }

    #[test]
    fn compose_topn_and_no_time() {
        let sql = compose_sql(&sales_metric(), &dim("省份", "cus.province"), "销售额前五省份", &edges()).unwrap();
        assert!(sql.contains("LIMIT 5"), "{sql}");
        assert!(!sql.contains("order_time"), "{sql}"); // 没提时间不加（SuperSonic 对齐）
    }

    #[test]
    fn compose_skips_mismatch() {
        // 子查询口径（库存快照）→ 不装配
        let stock = MetricDef {
            name: "库存量".into(),
            aliases: vec![],
            source_table: "t_winc_stock_report".into(),
            agg_expr: "SUM(stock_quantity)".into(),
            scope_filter: "product_stock_date = (SELECT MAX(product_stock_date) FROM t_winc_stock_report)".into(),
            dedup_keys: String::new(),
        };
        assert!(compose_sql(&stock, &dim("省份", "cus.province"), "本月库存量按省份", &edges()).is_none());
    }

    #[test]
    fn compose_fanout_rejected_for_sum() {
        // 单头 SUM × 明细驱动维度（1:N 扇出）→ 拒绝（防 total_amount 按行数虚增），交手工模板
        assert!(compose_sql(&sales_metric(), &cat_dim(), "本月销售额按商品分类", &edges()).is_none());
    }

    #[test]
    fn compose_qty_province_cross_base() {
        // 销量(detail) × 省份(header→customer)：N:1 链扇出安全 → 装配
        let sql = compose_sql(&qty_metric(), &dim("省份", "COALESCE(NULLIF(cus.province,''),'未知')"), "本月销量按省份", &edges()).unwrap();
        // 基表走去重子查询（明细含系统级重复行），口径过滤下推进子查询
        assert!(sql.contains("FROM (SELECT DISTINCT sales_order_code, sku_code, sku_name, box_quantity, amount FROM t_sales_order_detail WHERE item_type = '1') b0"), "{sql}");
        assert!(sql.contains("JOIN t_sales_order o ON o.sales_order_code = b0.sales_order_code"), "{sql}");
        assert!(sql.contains("SUM(b0.box_quantity)"), "{sql}");
        assert!(sql.contains("o.order_time >="), "{sql}");
    }

    #[test]
    fn compose_qty_category_time_bridge() {
        // 销量 × 商品分类（同基表 detail）：时间窗经边桥接 t_sales_order o_time
        let sql = compose_sql(&qty_metric(), &cat_dim(), "本月销量按商品分类", &edges()).unwrap();
        assert!(sql.contains("JOIN t_sales_order o_time ON o_time.sales_order_code = d.sales_order_code"), "{sql}");
        assert!(sql.contains("SUM(d.box_quantity)"), "{sql}");
        assert!(sql.contains("o_time.order_time >="), "{sql}");
    }

    #[test]
    fn dedup_subquery_for_detail_metric() {
        // 明细类指标（含系统级重复行）必须走 DISTINCT 子查询，否则 SUM 虚增 41%（评测抓获）
        let sql = compose_sql(&qty_metric(), &cat_dim(), "本月销量按商品分类", &edges()).unwrap();
        assert!(sql.contains("SELECT DISTINCT sales_order_code, sku_code, sku_name, box_quantity, amount"), "{sql}");
        assert!(sql.contains("WHERE item_type = '1') d"), "口径过滤下推进子查询: {sql}");
        // 外层不再重复加口径过滤
        assert_eq!(sql.matches("item_type").count(), 1, "{sql}");
    }

    #[test]
    fn dedup_skipped_when_col_not_in_keys() {
        // 外层引用了不在去重键里的列 → 子查询取不到 → 不装配（回落 LLM），绝不出错数
        let m = MetricDef {
            name: "销量".into(), aliases: vec![],
            source_table: "t_sales_order_detail".into(),
            agg_expr: "SUM(box_quantity)".into(),
            scope_filter: "item_type = '1'".into(),
            dedup_keys: "sales_order_code,sku_code".into(), // 缺 box_quantity
        };
        assert!(compose_sql(&m, &cat_dim(), "本月销量按商品分类", &edges()).is_none());
    }

    #[test]
    fn no_dedup_metric_unchanged() {
        // 无去重键的指标保持原装配（不引入子查询开销）
        let sql = compose_sql(&sales_metric(), &dim("省份", "cus.province"), "本月销售额按省份", &edges()).unwrap();
        assert!(!sql.contains("SELECT DISTINCT"), "{sql}");
    }

    #[test]
    fn base_col_refs_extracts() {
        assert_eq!(base_col_refs("SUM(d.box_quantity)", "d"), vec!["box_quantity"]);
        assert_eq!(base_col_refs("g.goods_code = d.sku_code AND d.sku_code > 0", "d"), vec!["sku_code"]);
        // 别名前缀不得被相似别名误命中
        assert!(base_col_refs("xd.foo", "d").is_empty());
        assert!(base_col_refs("COALESCE(cat.category_name,'未分类')", "d").is_empty());
    }

    fn scopes() -> Vec<(String, String)> {
        vec![
            ("t_sales_order".into(), "deleted_flag = 0 AND order_status NOT IN ('0','108','199')".into()),
            ("t_customer".into(), "deleted_flag = 0".into()),
        ]
    }

    #[test]
    fn table_scope_applied_to_bridge() {
        // 明细指标经时间桥 JOIN 订单主表 → 必须带上有效订单口径（漏则销量虚高 41%，评测抓获）
        let sql = compose_sql_with(&qty_metric(), &cat_dim(), "本月销量按商品分类", &edges(), &scopes()).unwrap();
        assert!(sql.contains("o_time.order_status NOT IN ('0','108','199')"), "{sql}");
        assert!(sql.contains("o_time.deleted_flag = 0"), "{sql}");
    }

    #[test]
    fn table_scope_not_duplicated_for_metric_base() {
        // 指标基表本身已有 scope_filter → 不重复叠加同一条件
        let sql = compose_sql_with(&sales_metric(), &dim("省份", "cus.province"), "本月销售额按省份", &edges(), &scopes()).unwrap();
        assert_eq!(sql.matches("order_status NOT IN").count(), 1, "{sql}");
        // 维度侧 JOIN 的客户表也吃到表级口径
        assert!(sql.contains("cus.deleted_flag = 0"), "{sql}");
    }

    #[test]
    fn from_table_aliases_parses() {
        let f = "t_sales_order_detail d JOIN t_goods g ON g.goods_code = d.sku_code JOIN t_sales_order o_time ON o_time.sales_order_code = d.sales_order_code";
        let got = from_table_aliases(f);
        assert_eq!(got, vec![
            ("t_sales_order_detail".to_string(), "d".to_string()),
            ("t_goods".to_string(), "g".to_string()),
            ("t_sales_order".to_string(), "o_time".to_string()),
        ]);
        // 去重子查询形态：括号内不算 FROM 项
        let f2 = "(SELECT DISTINCT a, b FROM t_sales_order_detail WHERE item_type = '1') d JOIN t_goods g ON g.goods_code = d.sku_code";
        assert_eq!(from_table_aliases(f2), vec![("t_goods".to_string(), "g".to_string())]);
    }

    #[test]
    fn breakdown_rejects_value_filtered_question() {
        // 回归 E16 抓获：模板只会「指标×维度」，问句带值过滤(线下客户/某省/某商品)必须回落 LLM，
        // 否则装配出的 SQL 静默丢掉限定 → 答非所问（曾把「线下客户销售额」答成全部客户 TOP200）
        assert!(sales_breakdown("线下客户本月销售额").is_none());
        assert!(sales_breakdown("恒众餐饮本月销售额按客户").is_none());
        assert!(sales_breakdown("烤肠本月销售额按省份").is_none());
    }

    #[test]
    fn breakdown_accepts_clean_questions() {
        // 纯「指标×维度(×时间×TopN)」问句照常走确定性模板
        for q in ["本月各省销售额", "销售额前5的客户", "各商品分类销售额", "本月销售额按业务员",
                  "本月各门店销售额", "各月销售额趋势"] {
            assert!(sales_breakdown(q).is_some(), "{q}");
        }
    }

    #[test]
    fn has_residue_basics() {
        let w: Vec<String> = ["销售额", "客户"].iter().map(|s| s.to_string()).collect();
        assert!(has_residue("线下客户本月销售额", &w));
        assert!(!has_residue("本月客户销售额排行前十", &w));
        // 长词优先剥离：不因先剥"客户"而在"客户分类"上留下"分类"
        let w2: Vec<String> = ["销售额", "客户", "客户分类"].iter().map(|s| s.to_string()).collect();
        assert!(!has_residue("本月客户分类销售额", &w2));
    }

    // ── 规则时间解析（SuperSonic TimeRangeParser 思路）──
    fn tp(q: &str) -> String {
        time_predicate(q).unwrap_or_else(|| panic!("未解析: {q}"))
    }

    #[test]
    fn time_recent_n_with_cn_numbers() {
        assert!(tp("近7天销售额").contains("INTERVAL 7 DAY"));
        assert!(tp("最近三个月销售额").contains("INTERVAL 3 MONTH"));
        assert!(tp("过去两周订单数").contains("INTERVAL 2 WEEK"));
        assert!(tp("近十天销量").contains("INTERVAL 10 DAY"));
        assert!(tp("最近十五天销售额").contains("INTERVAL 15 DAY"));
    }

    #[test]
    fn time_quarter_and_half_year() {
        assert!(tp("第二季度销售额").contains("-04-01"));
        assert!(tp("三季度销售额").contains("-07-01"));
        assert!(tp("上半年销售额").contains("-01-01"));
        assert!(tp("下半年销售额").contains("-07-01"));
    }

    #[test]
    fn time_explicit_month() {
        assert!(tp("6月销售额").contains("-06-01"));
        assert!(tp("十二月销量").contains("-12-01"));
        // 「上个月/本月」不得被当成 N 月解析
        assert!(tp("上个月销售额").contains("INTERVAL 1 MONTH"));
        assert!(tp("本月销售额").contains("%Y-%m-01"));
    }

    #[test]
    fn time_relative_words() {
        assert!(tp("今天销售额").contains("CURDATE()"));
        assert!(tp("前天订单数").contains("INTERVAL 2 DAY"));
        assert!(tp("上周销售额").contains("YEARWEEK"));
        assert!(tp("去年销售额").contains("YEAR(CURDATE()) - 1"));
        assert!(time_predicate("销售额是多少").is_none(), "无时间词不得臆造时间窗");
    }

    #[test]
    fn time_col_is_parameterized() {
        // 谓词模板列名可填——同一解析结果给不同表用不同时间列
        let tpl = time_predicate("本月").unwrap();
        assert!(fill_time_col(&tpl, "after_sales_time").contains("after_sales_time"));
        assert!(!fill_time_col(&tpl, "after_sales_time").contains("{}"));
    }

    #[test]
    fn cn_num_parses() {
        assert_eq!(cn_num("3"), Some(3));
        assert_eq!(cn_num("三"), Some(3));
        assert_eq!(cn_num("十"), Some(10));
        assert_eq!(cn_num("十二"), Some(12));
        assert_eq!(cn_num("三十"), Some(30));
        assert_eq!(cn_num("三十五"), Some(35));
        assert_eq!(cn_num("两"), Some(2));
        assert_eq!(cn_num("abc"), None);
    }
}
