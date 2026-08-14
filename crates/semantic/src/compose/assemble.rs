//! T8 搬运：逐行迁自 `server/src/direct.rs`（**只搬不改**，一个字节的行为改动都会让
//! `evaluation.py` 的逐题结果集对拍失去意义）。顺序即行为，只提取不重排。

#![allow(clippy::too_many_arguments)]

use std::collections::{HashMap, HashSet};

use sqlx::PgPool;

use dms_kernel::nl::text::strip_annotations;
use dms_kernel::nl::time::{detect_top_n, fill_time_col, prev_window, time_predicate, yoy_window};
use dms_kernel::sql::lex::{base_col_refs, from_table_aliases, qualify_cols};

use crate::compose::*;
use crate::registry::model::{DimensionDef as DimDef, JoinEdge, MetricDef, TableSnapshot, ValueRef};
use crate::{DirectHit, DirectOutcome, ExecutionEvidence, IntentSlotKind, Relation};

// 同批搬来的兄弟模块（原文件里是同一个作用域，拆文件后要显式引）
#[allow(unused_imports)]
use crate::compose::{metric::*, path::*, values::*};
#[allow(unused_imports)]
use crate::fastpath::{derive::*, finance::*, graph_rows::*, ops::*, relation::*, sales::*, stock::*, template::*};

use crate::sales_fact;

/// 组合 SQL 装配（纯函数可单测）。无表级口径的简化入口，测试用。
/// T8 搬运后本项被 `server/src/direct.rs` 的测试跨 crate 使用：`#[cfg(test)]` 在下游测试里
/// 不可见（它只在本 crate 自测时编译），故去掉该门。lib 的 `pub` 项不会触发 never-used 告警，
/// 原注释担心的那件事不会发生。
pub fn compose_sql(m: &MetricDef, d: &DimDef, question: &str, edges: &[JoinEdge]) -> Option<String> {
    compose_sql_with(m, d, question, edges, &[])
}


/// 路径/桥接 JOIN 的统一形态：**LEFT JOIN + 被连表的表级口径进 ON**（裁决 二·AW 前置①）。
/// INNER + 口径进 WHERE = 被连表口径不满足时行整行丢（售后单的原单作废 → 售后单消失，
/// 实测 20073→20060 少 13 单）；LEFT + 口径进 ON = 行保留、被连表列落 NULL（维度归「未知」），
/// 主表行一个不少。口径进 ON 后，`scope_parts` 循环靠 `caliber_in_on` 跳过它
/// （再进 WHERE 会把 LEFT 打回 INNER —— 前置②，两条必须一起，只改一条会被另一条抵消）。
pub fn left_join(to: &str, alias: &str, on_cond: &str, table_scopes: &[(String, String)]) -> String {
    let mut j = format!(" LEFT JOIN {to} {alias} ON {on_cond}");
    if let Some((_, f)) = table_scopes.iter().find(|(tn, _)| table_eq(tn, to)) {
        if !f.trim().is_empty() {
            j.push_str(&format!(" AND {}", qualify_cols(f, alias)));
        }
    }
    j
}


/// 该表的表级口径是否已经在它自己 JOIN 的 ON 段里（前置②的检测）。
/// 判据是 ON 段里出现「等式之外」的被连表列条件（` AND alias.`）——
/// 连接等式本身总是第一个条件，口径永远排在它后面。
pub fn caliber_in_on(from: &str, table: &str, alias: &str) -> bool {
    let pat = format!("JOIN {table} {alias} ON");
    let Some(i) = from.find(&pat) else { return false };
    let seg = &from[i + pat.len()..];
    let end = seg.find(" JOIN ").map(|j| &seg[..j]).unwrap_or(seg);
    end.contains(&format!(" AND {alias}."))
}


/// 组合 SQL 装配（带表级标准口径）。无快照声明的入口。
///
/// `#[cfg(test)]`：生产路径全部走 `compose_sql_with_snap`（要 `snaps` 与 `vals`），
/// 这一层的唯一调用者是上面同样 `cfg(test)` 的 `compose_sql`。
/// 不加就是每次 `cargo build` 一条 `never used` 警告 —— 而警告堆多了就没人看告警了。
/// T8 搬运后本项被 `server/src/direct.rs` 的测试跨 crate 使用：`#[cfg(test)]` 在下游测试里
/// 不可见（它只在本 crate 自测时编译），故去掉该门。lib 的 `pub` 项不会触发 never-used 告警，
/// 原注释担心的那件事不会发生。
pub fn compose_sql_with(
    m: &MetricDef,
    d: &DimDef,
    question: &str,
    edges: &[JoinEdge],
    table_scopes: &[(String, String)],
) -> Option<String> {
    compose_sql_with_snap(m, d, question, edges, table_scopes, None, None, &[])
}


/// 大写归一后的 SQL 文本里是否含某个**词元**（SELECT/UNION 这类关键字；非字母数字都算词界）。
/// `contains` 子串判两头错：`'SELECTED'` 这类字面量会误中（过度拒，安全方向但白扔覆盖），
/// 而 `" UNION "` 要求两侧都是空格 —— `UNION\nALL`（换行）会从网眼漏掉（该拒没拒）。
pub fn sql_has_keyword(sql_up: &str, kw: &str) -> bool {
    sql_up.split(|c: char| !c.is_ascii_alphanumeric()).any(|t| t == kw)
}


/// 组合 SQL 装配（带表级标准口径 + 可选快照声明）
pub fn compose_sql_with_snap(
    m: &MetricDef,
    d: &DimDef,
    question: &str,
    edges: &[JoinEdge],
    table_scopes: &[(String, String)],
    snap: Option<&TableSnapshot>,
    // `time_tpl`：时间谓词模板的覆盖（`None` = 按问句解析当期）。KPI 环比拿它传**平移后的
    // 上期模板**，与 `agg_template` 出 `prev` 的做法同形：同一段装配、只换时间窗。
    time_tpl: Option<&str>,
    // `vals`：`meta.value_map` 全量。问句里能被**唯一**一条码值声明解释的词（湖南 →
    // `t_customer.province = '430000'`）从此既被残留守卫消化、也真的装进 WHERE。
    // 空切片 = 不启用（既有调用点与单测保持原行为）。
    vals: &[ValueRef],
) -> Option<String> {
    // 口径/来源去中文括注（注册表文本带人类说明）
    let m_src = strip_annotations(&m.source_table);
    let m_scope = strip_annotations(&m.scope_filter);
    let m_agg = strip_annotations(&m.agg_expr);
    // 关键字按词元判（`sql_has_keyword`）：子串判会误中 'SELECTED' 字面量、漏掉 UNION\nALL
    if sql_has_keyword(&m_scope.to_uppercase(), "SELECT") || sql_has_keyword(&m_agg.to_uppercase(), "SELECT") {
        return None; // 子查询内裸列归属子查询表，限定会改错——走 LLM
    }
    if sql_has_keyword(&m_src.to_uppercase(), "UNION") {
        return None; // 多流来源（发票新老双表）须 UNION ALL 合并，模板拼不出——交 LLM 按口径卡写
    }
    // 值过滤：问句里能被**唯一**一条码值声明解释的词，先认下来（下面它会被残留守卫消化），
    // 装不上去时**整条拒**（G1，见下方 `scope_parts` 那段）。顺序必须是「先认、后消化」。
    let vfs = value_filters(question, vals, &registry_words(m, d));
    if has_entity_residue(question, m, d, &vfs) {
        return None; // 实体问句（恒众餐饮本月销售额）→ 实体/安全分析路径
    }
    // 维度来源与指标侧同规格：先剥人类注解（`t_x(JOIN …)`）再取标识符 ——
    // 否则带注解的声明会取出 `t_x(JOIN` 这种既不是表也不是别名的串，路径/桥接全找不到
    let d_src = strip_annotations(&d.source_table);
    let dim_base = dms_kernel::sql::lex::first_ident_of(&d_src)?;
    let dim_alias = d_src.split_whitespace().nth(1)?.to_string();
    // split_whitespace 合并连续空白；`splitn` 不合并 —— `"t  cus JOIN…"` 会把 `"cus JOIN…"`
    // 错当 rest，FROM 拼出 `t cus cus JOIN` 这种坏串
    let dim_rest: String = d_src.split_whitespace().skip(2).collect::<Vec<_>>().join(" ");

    // 去重键：来源表含系统级重复行（ETL 双写）时，基表换成 DISTINCT 子查询再聚合，
    // 否则 SUM 直接虚增（实测明细 100.7 万行 vs 去重 83.2 万行，销量虚高 41%）。
    let dedup: Vec<String> = m
        .dedup_keys
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    let m_tcol = strip_annotations(&m.time_col);
    let metric_bound_time_dim = !table_eq(&dim_base, &m_src) && is_time_expr(&d.expr) && !m_tcol.is_empty();

    // FROM 装配 + 扇出检查 + 各表别名登记
    let mut from: String;
    let mut table_aliases: Vec<(String, String)> = vec![]; // (table, alias)
    if table_eq(&dim_base, &m_src) {
        // 同基表：直接用维度来源串（剥过注解、含其内部 JOIN 链）
        from = d_src.clone();
        table_aliases.push((dim_base.clone(), dim_alias.clone()));
    } else if metric_bound_time_dim {
        // 时间维度是分桶定义，不要求 JOIN 它登记时的业务表。
        from = format!("{m_src} b0");
        table_aliases.push((m_src.clone(), "b0".to_string()));
    } else {
        // 跨基表：BFS 路径拼接；扇出边仅 COUNT(DISTINCT) 聚合可过（防 SUM 单头列虚增）
        let path = find_path(&m_src, &dim_base, edges)?;
        // 先 trim 再判：声明的前导空格会让这道扇出检查失效（SUM 沿 1:N 虚增的防线不能被空格绕过）
        if path.iter().any(|h| h.3) && !m_agg.trim().to_uppercase().starts_with("COUNT(DISTINCT") {
            return None;
        }
        from = format!("{m_src} b0");
        table_aliases.push((m_src.clone(), "b0".to_string()));
        let mut prev_alias = "b0".to_string();
        for (i, (to, to_col, from_col, _)) in path.iter().enumerate() {
            let last = i == path.len() - 1;
            let alias = if last { dim_alias.clone() } else { format!("b{}", i + 1) };
            from.push_str(&left_join(to, &alias, &format!("{alias}.{to_col} = {prev_alias}.{from_col}"), table_scopes));
            table_aliases.push((to.clone(), alias.clone()));
            prev_alias = alias;
        }
        if !dim_rest.is_empty() {
            from.push(' ');
            from.push_str(&dim_rest);
        }
    }
    let base_alias = table_aliases[0].1.clone();
    let dim_expr = if metric_bound_time_dim {
        bind_time_dimension(&d.expr, &format!("{base_alias}.{m_tcol}"))?
    } else {
        d.expr.clone()
    };

    // 时间窗。**先按指标声明的 `time_col` 放**，放不下才回到「桥接订单头」那条老路。
    //
    // 🔴 老路写死 `t_sales_order` / `order_time`：在 FROM 里找不到订单头就试着桥一条边，
    // 桥不到就**整条不装配**。于是时间语义不在订单头上的指标 —— 售后单数（`after_sales_time`）、
    // 开票金额、动销商品数 —— 一律放不下时间窗、一律回落 LLM，而声明里明明写着该用哪一列。
    // 实测（`why-not-compose` 逐题诊断）：这是「指标 only 也不接」的主因。
    //
    // 判据：声明的列**不是** `order_time` 时就放在**指标基表**上 ——
    // 声明说「这个指标按这一列算」，而指标的基表就是它自己的表。
    // 声明为 `order_time`（或未声明）时保持老路：明细类指标的 `order_time` 确实在订单头上，
    // 那条桥接不可省（漏了它连「有效订单」表级口径一起丢）。
    // 覆盖优先（环比传上期模板），否则按问句解析当期
    let tpl_src = time_tpl.map(String::from).or_else(|| time_predicate(question));
    let time_and = match tpl_src {
        Some(tpl) if !m_tcol.is_empty() && m_tcol != "order_time" => {
            format!(" AND {}", fill_time_col(&tpl, &format!("{base_alias}.{m_tcol}")))
        }
        Some(tpl) => {
            // 先定别名、再带别名填列：填完再拿子串替换会把模板里任何含 `order_time` 的
            // 标识符（如 `prev_order_time`）一起改坏，且填了再换是两次活
            let alias = if let Some((_, a)) = table_aliases.iter().find(|(t, _)| table_eq(t, "t_sales_order")) {
                a.clone()
            } else if let Some((e, base_is_left)) = find_edge(&m_src, "t_sales_order", edges) {
                let (c_base, c_ord) = if base_is_left { (&e.lc, &e.rc) } else { (&e.rc, &e.lc) };
                from.push_str(&left_join(
                    "t_sales_order",
                    "o_time",
                    &format!("o_time.{c_ord} = {base_alias}.{c_base}"),
                    table_scopes,
                ));
                "o_time".to_string()
            } else {
                return None;
            };
            format!(" AND {}", fill_time_col(&tpl, &format!("{alias}.order_time")))
        }
        None => String::new(),
    };

    // 值过滤的表若不在 FROM 里，**按 `meta.join_edge` 桥一条**（与上面桥订单头同形）。
    //
    // 为什么必须在这里、而不是等到下面拼 WHERE 时才找别名：放在这个位置，后面三层守卫
    // 全部自动覆盖新桥进来的表 —— ① 去重装配的 `base_col_refs(&from, …)` 会看见新 JOIN
    // 引用的基表列，不在去重键里就整条拒；② 表级标准口径那个循环靠 `from_table_aliases`
    // 扫 FROM，新表的恒需过滤会跟着加上；③ 快照/去重的 `from.starts_with(&head)` 只看首段，
    // 尾部追加 JOIN 不影响。若改到下面再桥，这三层就全绕过去了。
    //
    // 扇出边一律拒：`SUM` 沿 1:N 边会把单头列乘一遍（实测销量虚高 41% 就是这么来的）。
    // 「本月湖南省的销售额」这条路是 明细→订单头→客户，两跳都是 N:1（收敛），所以能过。
    let mut vf_conds: Vec<(String, String)> = vec![]; // (列引用, 条件)
    // FROM 的 (表, 别名) 只扫一次，桥进新表后增量登记 —— 原来每个 vf、每一跳都重扫一遍 FROM 串
    let mut from_aliases = from_table_aliases(&from);
    for (i, v) in vfs.iter().enumerate() {
        let existing =
            from_aliases.iter().find(|(t, _)| table_eq(t, &v.table)).map(|(_, a)| a.clone());
        let alias = match existing {
            Some(a) => a,
            None => {
                let path = find_path(&m_src, &v.table, edges)?;
                if path.iter().any(|h| h.3) {
                    return None;
                }
                let mut prev = base_alias.clone();
                let mut last = String::new();
                for (j, (to, to_col, from_col, _)) in path.iter().enumerate() {
                    // 路径上已在 FROM 里的表复用其别名（例如时间窗刚桥进来的 `o_time`），
                    // 不重复 JOIN 同一张表
                    let found =
                        from_aliases.iter().find(|(t, _)| table_eq(t, to)).map(|(_, a)| a.clone());
                    match found {
                        Some(ex) => {
                            prev = ex.clone();
                            last = ex;
                        }
                        None => {
                            let na = format!("vf{i}_{j}");
                            from.push_str(&left_join(to, &na, &format!("{na}.{to_col} = {prev}.{from_col}"), table_scopes));
                            from_aliases.push((to.clone(), na.clone()));
                            prev = na.clone();
                            last = na;
                        }
                    }
                }
                last
            }
        };
        vf_conds.push((format!("{alias}.{}", v.column), format!("{alias}.{} = '{}'", v.column, v.code)));
    }

    let mut scope = if m_scope.trim().is_empty() { String::new() } else { qualify_cols(&m_scope, &base_alias) };
    let agg = qualify_cols(&m_agg, &base_alias);

    // 快照装配：基表 → (SELECT * FROM (… ROW_NUMBER() OVER (PARTITION BY 分区键 ORDER BY 排序) rn …) WHERE rn=1) 别名。
    //
    // 与去重装配**同一个形状**（都是「把基表换成派生表 + 把口径下推进去」），只是把
    // `DISTINCT 键` 换成 `rn = 1`。分区键 / 排序 / 额外过滤三样全部来自 `meta.table_snapshot`
    // —— 装配器不自己猜「哪一条算最新」。
    //
    // 口径必须**下推进最内层**（与去重那层同理）：窗口函数要在**已过滤**的集合上算，
    // 否则「最新一条」可能是一条被口径排除的行（例如 balance_status 不生效的那条），
    // rn=1 取到它就等于整条记录被丢掉。gold 也是这么写的（过滤在子查询内）。
    if let Some(s) = snap {
        let parts: Vec<String> =
            s.partition_cols.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect();
        if parts.is_empty() {
            return None;
        }
        let base_scope = table_scopes
            .iter()
            .find(|(tn, _)| table_eq(tn, &m_src))
            .map(|(_, f)| f.trim())
            .unwrap_or("");
        // 🔴 按**原子条件**去重，不是整串去重：`balance_status='4'` 一次作为独立的
        // `extra_filter` 出现、一次嵌在指标口径的 AND 链里（`deleted_flag=0 AND
        // balance_status='4' AND balance_type IN(...)`）。整串比较抓不到后者，
        // 于是同一个条件会拼两遍 —— 语义上无害，但 SQL 噪声会让人以为哪里错了。
        // 用既有的 `split_top_and`（`add_scope_filter` 也是靠它）。
        let mut inner: Vec<String> = vec![];
        for src in [m_scope.trim(), base_scope, s.extra_filter.trim()] {
            for c in dms_kernel::sql::lex::split_top_and(src) {
                let c = c.trim().to_string();
                if !c.is_empty() && !inner.contains(&c) {
                    inner.push(c);
                }
            }
        }
        let inner_where =
            if inner.is_empty() { String::new() } else { format!(" WHERE {}", inner.join(" AND ")) };
        let sub = format!(
            "(SELECT * FROM (SELECT *, ROW_NUMBER() OVER (PARTITION BY {} ORDER BY {}) AS rn \
             FROM {m_src}{inner_where}) rk WHERE rk.rn = 1) {base_alias}",
            parts.join(", "),
            s.order_cols.trim()
        );
        let head = format!("{m_src} {base_alias}");
        if !from.starts_with(&head) {
            return None;
        }
        from = format!("{sub}{}", &from[head.len()..]);
        scope.clear(); // 口径已下推进子查询
    }

    // 去重装配：基表 → (SELECT DISTINCT 键 FROM 基表 WHERE 口径) 别名。
    // 安全门控：外层对基表引用的所有列必须都在去重键里，否则子查询取不到 → 宁可不装配（回落 LLM）。
    if !dedup.is_empty() {
        let mut refs = base_col_refs(&from, &base_alias);
        refs.extend(base_col_refs(&agg, &base_alias));
        refs.extend(base_col_refs(&dim_expr, &base_alias));
        refs.extend(base_col_refs(&time_and, &base_alias));
        if !refs.iter().all(|c| dedup.contains(c)) {
            return None;
        }
        let keys = dedup.join(", ");
        // 🔴 **表级口径也必须一起下推**。基表在这里被换成派生表 `(SELECT DISTINCT …) 别名`，
        // 而下面那个补表级口径的循环靠 `from_table_aliases` 找表名 —— 它看不见括号里的东西，
        // 所以会跳过基表（那行 `continue` 的注释写着「其口径已下推」）。
        // 若这里只下推指标自己的 `scope_filter`，表级那条就**两头都漏**：
        // 实测明细表的 `deleted_flag = 0` 既没进子查询也没进外层 WHERE，
        // 软删的明细行被算进销量 —— 而这是确定性 0-LLM 路径，连回炉的机会都没有。
        // 构建期守卫 `deterministic_templates_satisfy_table_scopes` 就是抓到这一条的。
        let base_scope = table_scopes
            .iter()
            .find(|(tn, _)| table_eq(tn, &m_src))
            .map(|(_, f)| f.trim())
            .unwrap_or("");
        let mut inner: Vec<&str> = vec![];
        if !m_scope.trim().is_empty() {
            inner.push(m_scope.trim());
        }
        // 相等时不重复拼（种子里两者不重叠，但声明是人写的，重了也只是多一个恒真条件）
        if !base_scope.is_empty() && base_scope != m_scope.trim() {
            inner.push(base_scope);
        }
        let inner_where =
            if inner.is_empty() { String::new() } else { format!(" WHERE {}", inner.join(" AND ")) };
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
        // 前置②（裁决 二·AW）：口径已在它自己 JOIN 的 ON 里 → 跳过。再进 WHERE 会把
        // LEFT 打回 INNER（被连表口径不满足的行整行丢 —— 售后单数少 13 单就是这么来的）。
        if caliber_in_on(&from, &t, &alias) {
            continue;
        }
        if let Some((_, f)) = table_scopes.iter().find(|(tn, _)| table_eq(tn, &t)) {
            let qualified = qualify_cols(f, &alias);
            if !scope_parts.contains(&qualified) {
                scope_parts.push(qualified);
            }
        }
    }

    // 值过滤落地。上面 `vfs` 里的名字**已经被残留守卫消化掉了**，所以这里每一条都必须
    // 真的装上；装不上就 `return None` —— 消化了词却不装过滤，正是 E16「线下客户本月销售额
    // → 全部客户 TOP200」那类静默丢限定的翻车，宁可回落 LLM。
    for (col_ref, cond) in &vf_conds {
        // G1：别名必须仍然指向 FROM 里一张**真表**。基表被去重/快照派生表包住时，
        // `from_table_aliases` 看不见括号内的表名 → 这里查不到 → 拒。
        // 不要为它「补一条 alias 映射」：派生表只 SELECT 去重键，那会拼出引用不存在列的 SQL。
        let alias = col_ref.split('.').next().unwrap_or("");
        if !from_table_aliases(&from).iter().any(|(_, a)| a == alias) {
            return None;
        }
        // G2：该列已被口径约束 → 拒。销量口径是 `item_type = '1'`，若问句说「赠品」
        // （声明 `item_type = '2'`）就会拼出两条互斥条件 = 恒 0 行，而这是确定性路径，
        // 静默返回「0」比回落 LLM 坏得多。口径与问句冲突该由人去看，不是装配器调和。
        // `contains(col_ref)` 是**子串**判据（`b0.qty` 会被 `b0.qty_total` 误中）——
        // 刻意的宽判：误中的代价是多拒一条（回落 LLM），漏判的代价是恒 0 行静默答错。
        if scope_parts.iter().any(|p| p.contains(col_ref)) || time_and.contains(col_ref) {
            return None;
        }
        if !scope_parts.contains(cond) {
            scope_parts.push(cond.clone());
        }
    }
    let scope = scope_parts.join(" AND ");
    let where_sql = match (scope.is_empty(), time_and.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("WHERE {scope}"),
        (true, false) => format!("WHERE {}", time_and.trim_start_matches(" AND ")),
        (false, false) => format!("WHERE {scope}{time_and}"),
    };
    // 【无维度模式】`dim_expr` 为空 = 调用方要的是「指标 only」：不出维度列、不 GROUP BY、
    // 不 ORDER BY（单行结果排序无意义）、不 LIMIT（纯聚合，`ensure_limit` 也不会补）。
    // 入口是 `try_compose_metric_only`，那里说明了为什么需要它。
    if dim_expr.trim().is_empty() {
        return Some(format!("SELECT {} AS `{}`\nFROM {}\n{}", agg, m.name, from, where_sql));
    }
    let lim = ranking_limit(question);
    // 时间维度按时间排序（趋势语义），其余按问句指定的高低方向排序。
    let order = if is_time_expr(&dim_expr) {
        format!("ORDER BY {} LIMIT {lim}", dim_expr)
    } else {
        format!("ORDER BY `{}` {} LIMIT {lim}", m.name, rank_direction(question))
    };
    Some(format!(
        "SELECT {} AS `{}`, {} AS `{}`\nFROM {}\n{}\nGROUP BY {}\n{order}",
        dim_expr, d.name, agg, m.name, from, where_sql, dim_expr
    ))
}


/// 把通用时间分桶表达式中的第一个“别名.列”替换为指标自己的时间列。
/// 无法证明表达式形态时返回 None，继续回落而不是猜列。
pub fn bind_time_dimension(expr: &str, column: &str) -> Option<String> {
    let open = expr.find('(')? + 1;
    let tail = &expr[open..];
    let end = tail.find(|c| c == ',' || c == ')')?;
    let candidate = tail[..end].trim();
    let valid = candidate.split_once('.').is_some_and(|(alias, name)| {
        !alias.is_empty()
            && !name.is_empty()
            && alias.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
            && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
    });
    valid.then(|| format!("{}{}{}", &expr[..open], column, &tail[end..]))
}

