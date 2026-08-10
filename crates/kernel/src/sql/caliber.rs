//! 纯 AST 口径校验器：把上游声明的口径与一条 SQL 比对，返回违规清单。变更原因＝口径判据。
//!
//! **只判不改**：不产 SQL、不改 AST（裁决 V3——拒绝「LLM 改写 SQL」路线）。
//! 违规的处置是「命名违规 + 回炉重生成」，由调用方决定，本模块不掺和。
//!
//! 三条防误伤原则（判错一条会让所有人学会忽略校验器）：
//! 1. **声明缺失 ≠ 违规**：声明里的表没出现在 FROM/JOIN 里 → 一律不判。
//!    唯一例外是 `RequireJoinAndFilter`（它的声明本身就断言「该表必须在场」，见其文档）。
//! 2. **只看列是否被约束，不比对值**：`x = '1'` 与 `x = '2'` 同等通过。
//! 3. **认多种正确写法**：ON 里的约束算约束；窗口函数与关联子查询都算「取最新一条」。
//! 因此所有模糊地带一律偏向**漏判**（返回空）而非误判。
//!
//! 零 DMS 语料：表名/列名/指标名全部由调用方以声明传入。

use core::ops::ControlFlow;
use std::collections::HashSet;

use sqlparser::ast::{
    DuplicateTreatment, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments, GroupByExpr,
    JoinConstraint, JoinOperator, Query, Select, SelectItem, SetExpr,
    Statement, TableFactor, TableWithJoins, Value, Visit, Visitor, WindowType,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

/// 一条口径声明。语义层从注册表构造，kernel 只判不取数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaliberRule {
    /// 该表出现在 FROM/JOIN 里，就必须约束这些列（表级标准口径）
    RequireCols { table: String, cols: Vec<String>, human: String },
    /// 该表含系统级重复行，聚合其列前必须先去重
    RequireDedup { table: String, keys: Vec<String>, human: String },
    /// 快照流水表：必须取「每个分区最新一条」
    RequireLatest { table: String, partition: Vec<String>, human: String },
    /// 占比类指标必须放大到百分数
    RequirePercentScale { metric: String, human: String },
    /// 值域命中：问句里的专名是某列的取值，那么该表必须被 JOIN 进来、且该列必须被约束。
    /// 与 `RequireCols` 的差别是**表缺席也算违规**（`RequireCols` 表缺席即不判）。
    RequireJoinAndFilter { table: String, col: String, human: String },
    /// 码值域命中：`code` 是 `表.列` 的取值，那么这个码只许出现在那一列的条件里。
    /// 治「取对了码、用错了列」——码是对的、列是错的，数字看着合理而语义全变。
    /// **码没出现在任何条件里 → 不判**（JOIN 字典表按名过滤是另一种正确写法）。
    RequireCodeOnColumn { table: String, col: String, code: String, human: String },
    /// 该列的取值是一份**可证完整枚举**的码表：条件里出现该列的等值条件、而值不在
    /// `values` 里 → 违规。`values` 是登记的 `(名, 码)` 对（两侧都算合法值）。
    ///
    /// 治**最阴的那一族静默错答**：在已登记码表的列上写一个不存在的中文值 —— SQL 合法、
    /// 三段闸门放行、执行成功、**返 0 行**。无报错、无告警、route 正常、`caliber_note` 为空，
    /// 用户读成「本月没有这类数据」。其余六条判据一条都管不到它（它们只看列有没有被约束）。
    ///
    /// 🔴 **只判不改**：上游那条 `removeUnmappedFilterValue` 是**删掉那个条件**，本仓不抄 ——
    /// 删了会把「0 行」换成一个更宽的错数（裁决 二·N 有一次「口径层把本来正确的 SQL 改错」的账）。
    RequireKnownValue {
        table: String,
        col: String,
        /// 登记的 `(名, 码)` 对。**判据按 `values.is_empty()` 兜底不判** —— 空集会把
        /// 该列上**每一个**中文值判红，那是最贵的一种误伤（连带把对的答案回炉改错，裁决 二·G）。
        values: Vec<(String, String)>,
        human: String,
    },
    /// 声明的时间列：问句带时间范围时，条件里**必须**约束这一列（无论它在哪张表上）。
    ///
    /// 治「同表/跨表多个时间列语义不同」——注册表把时间列钉死了，口径卡也写着「必须用某列」，
    /// 但那只是提示。实测：问「上半年每月订单明细数量」时用了明细表自己的另一个时间列，
    /// 于是既没按下单时间分月、也顺带丢掉了主表上的有效状态过滤，数字虚高 26%。
    ///
    /// **表名不入判据**是刻意的：声明只知道列名，不知道它在哪张表（注册表的 `time_col`
    /// 是「该指标的时间语义」而不是「某表的列」）。列名在本库里足够独特，
    /// 而带上表名会让「JOIN 进来但用了别名」的正确写法误红。
    RequireTimeColumn { col: String, human: String },
    /// 禁止「度量聚合 + JOIN 进已知会重复的键」同场（扇出）：被 JOIN 进来的那张表的连接列
    /// 在它自己表里有重复值（`meta.join_edge` 的 N 侧），于是另一边的每一行被复制 N 份，
    /// SUM/AVG 被放大同样倍数 —— SQL 语法没错、执行不报错，只是数字悄悄错几百倍。
    ///
    /// 实测（FIN01）：为取客户名把发票单头 `LEFT JOIN t_sales_order ON customer_code`
    /// （一个客户 N 张订单），开票金额放大 299 倍（654888936 = 2190264 × 299）。
    ///
    /// `keys` = 已知的重复键清单 `(表, 列)`（构造侧从 `join_edge` 的 card 推出，
    /// kernel 自己一个表名都不认识）。
    ///
    /// 判据形态（三处全偏漏判 —— 判错一条会把对的答案回炉改错）：
    /// ① 只认「被 JOIN 进来那一侧」的键：基表侧的重复键是正常方向
    ///    （`FROM t_sales_order JOIN t_customer` 是每天的正确写法）；
    /// ② 只在存在 SUM/AVG/MIN/MAX 列入参时判：COUNT 族数的是行，扇出往往恰是本意
    ///    （`FROM 客户 JOIN 订单 COUNT(*)` 数订单数），这一族误伤面太大；
    /// ③ 只在「等值**另一边**贡献了度量」时判：被复制的只有另一边的行，
    ///    第三张表的行数由它自己的连接粒度决定（`a JOIN b(dup) JOIN c` 取
    ///    `b.price × c.qty` 是正确 SQL —— 实测误判过，判红把模型从正确口径上逼走）；
    ///    裸前缀解不到表一律算命中（扇出在场的裸度量本身是歧义写法）。
    NoFanoutJoin { keys: Vec<(String, String)>, human: String },
    /// 声明的时间窗上限是「到昨天」（`meta.metric.time_cap='yesterday'`）：当期问法
    /// （今天/本月/今年…）下，该指标时间列的上限必须写 `< CURDATE()`。
    ///
    /// 适用于业务动作次日才确认的延迟指标：即使卡片和规则窗都提示 `< CURDATE()`，
    /// 模型仍可能照抄自然期末并把追加条件当冗余清理掉；提示无法保证时必须上判据。
    ///
    /// 只认 `< CURDATE()` 这一种写法：回炉指令给的就是它，多认一种等价形态
    /// （`<= CURDATE()-1`、`DATE(x) < CURRENT_DATE`）就是多一分漏判面。
    /// 构造侧只在当期问法时造这条（`window_includes_today`）：问「上月」时这条
    /// 条件本就不必出现，判了就是误伤。
    RequireTimeCap { col: String, human: String },
    /// **码列上的名称写法**：已登记「名=码」映射里，SQL 用了**名**当过滤值（等值或 LIKE），
    /// 而该列存的是码 —— 名称写法**必返 0 行**（可证，不依赖字典完整性：
    /// 名已登记在**另一个码**下面，写名必然匹不到任何码值）。
    ///
    /// 由来（SALE17 两次实测）：`province LIKE '%湖南%'` —— 值卡劝了两次没拦住
    /// （卡片管劝不管判）。与 `RequireKnownValue` 的分工：那条管「值不在码表里」
    /// （要字典完整才敢判），这条管「写法是名不是码」（名单在册即可判，
    /// 不完整的 seed 批次也照样生效）。
    RequireCodeEq { table: String, col: String, values: Vec<(String, String)>, human: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// 机器可读的违规名（`require_cols:表名` 形态），用于去重与统计
    pub rule: String,
    /// 声明自带的人话解释（调用方原样给 LLM 看）
    pub human: String,
    /// 该怎么改（由声明里的列/键/分区拼出）
    pub hint: String,
}

/// 顶层输出列的「形状」：每列的别名（无别名则取表达式原文）。
/// `None` = 解析不出（调用方按「无法比较」处理，不要当成不相等）。
///
/// 用途只有一个：**口径回炉不许改变输出列**。回炉的指令是「只补口径，不要改变原有的查询意图、
/// 输出列与排序」，但那只是一句请求 —— 实测 LLM 会整条重构：判词只要一个软删过滤，
/// 它却把一个真实的分类 JOIN 换成「取商品名前两个字」并多出两列排名，
/// 一条本来正确的题被打红。输出列变了就说明改的不只是口径，那种改写宁可不采纳。
pub fn output_shape(sql: &str) -> Option<Vec<String>> {
    let stmts = Parser::parse_sql(&GenericDialect {}, sql).ok()?;
    let [Statement::Query(q)] = &stmts[..] else { return None };
    let SetExpr::Select(s) = &*q.body else { return None };
    Some(
        s.projection
            .iter()
            .map(|p| match p {
                SelectItem::ExprWithAlias { alias, .. } => alias.value.trim_matches('`').to_lowercase(),
                // 🔴 裸引用也要剥反引号：模型把 `SELECT sku_name AS 商品名称` 改写成
                // `SELECT `商品名称` FROM (…)`（引别名、不重命名）时，输出列其实一个都没动，
                // 带反引号比原文会让**合规的修复被形状闸整批否决**（SALE15 实测：两次回炉
                // 全被这一句挡掉，坏 SQL 原样返回 —— 闸门的形状必须比的是列不是字节）。
                other => other.to_string().trim_matches('`').to_lowercase(),
            })
            .collect(),
    )
}

/// 回炉后的 SQL 是否只动了口径（输出列一致）。**比不出来时返回 `true`**：
/// 那意味着解析失败，而解析失败已经在 `check_caliber` 里被定义为漏判方向，
/// 这里若返回 `false` 就会因为「看不懂」而丢掉一次本可能正确的自修。
pub fn keeps_output_shape(before: &str, after: &str) -> bool {
    match (output_shape(before), output_shape(after)) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// SQL 与声明比对，返回违规清单（空 = 通过）。
/// **解析失败返回空**——语法错由 `sql::gate::check` 那道闸负责报，这里再报一次只会搅乱错误信息。
pub fn check_caliber(sql: &str, rules: &[CaliberRule]) -> Vec<Violation> {
    if rules.is_empty() {
        return vec![];
    }
    // ponytail: 固定 GenericDialect（它同时吃反引号与双引号标识符）。签名不带方言是刻意的——
    // 本函数只在「已经过 check() 闸」的 SQL 上跑，方言差异导致的解析失败一律降级为漏判。
    let Ok(stmts) = Parser::parse_sql(&GenericDialect {}, sql) else {
        return vec![];
    };
    let mut f = Facts::default();
    for s in &stmts {
        if let Statement::Query(q) = s {
            scan_query(q, &mut f, false);
        }
    }
    let keep = drop_conflicting_time_cols(rules);
    rules
        .iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .filter_map(|(_, r)| judge(r, &f))
        .collect()
}

/// 逐条返回「这条规则要不要判」。**只处理一种冲突**：同一轮里出现两个及以上不同的
/// `RequireTimeColumn` 列 → 那几条**全部不判**。
///
/// 🔴 这不是洁癖，是实测被它咬了一次（AS03，`_DECISIONS` 二·N）：
/// 问「今年退货类型的售后单有多少单」时，「单」把无关指标「订单数」也召回了，
/// 于是同时有 `RequireTimeColumn{after_sales_time}`（售后单数）与
/// `RequireTimeColumn{order_time}`（订单数）。而这条变体**刻意不带表名**
/// （当初是为了跨表也能判），于是两条互相矛盾。
/// 后果不是「没判出来」，是**判据把一条本来正确的 SQL 改错了**：
/// 模型第一版老实用了 `after_sales_time`，判词命令它换成 `order_time`，它照做，
/// 于是另一条当场判红、预算用尽、返回错值（2779 vs 2990）。
///
/// 丢掉而不是「挑一个」：挑需要一个「哪个指标更贴问句」的分数，而构造侧今天没有那个分数；
/// 挑错了就是重演上面那件事。丢掉只回到「本轮不查时间列」（漏判方向），
/// 与裁决 二·G 的宁缺毋滥同一条口径 —— **判错一条会连带把对的答案回炉改错**。
///
/// ponytail: 升级路径是让构造侧（`semantic::registry::caliber`）带上召回分数、
/// 只保留最高分那个指标的时间列声明；那需要 `recall_metric_hits` 把分数一路带下来。
fn drop_conflicting_time_cols(rules: &[CaliberRule]) -> Vec<bool> {
    let mut cols: Vec<&str> = rules
        .iter()
        .filter_map(|r| match r {
            CaliberRule::RequireTimeColumn { col, .. } => Some(col.as_str()),
            _ => None,
        })
        .collect();
    cols.sort_unstable();
    cols.dedup();
    let conflict = cols.len() > 1;
    rules
        .iter()
        .map(|r| !(conflict && matches!(r, CaliberRule::RequireTimeColumn { .. })))
        .collect()
}

// ---------- 判据 ----------

fn judge(r: &CaliberRule, f: &Facts) -> Option<Violation> {
    match r {
        CaliberRule::RequireCols { table, cols, human } => {
            let al = f.aliases_of(table);
            let miss: Vec<String> =
                cols.iter().filter(|c| !f.constrained(table, &al, c)).map(|c| c.to_lowercase()).collect();
            if al.is_empty() || miss.is_empty() {
                return None;
            }
            viol("require_cols", table, human,
                 format!("表 {table} 未约束口径列 {}：请在 WHERE 或 JOIN ON 里补上", miss.join(", ")))
        }
        CaliberRule::RequireDedup { table, keys, human } => {
            let al = f.aliases_of(table);
            // 🔴 前置条件必须能**穿透 CTE / 子查询**。
            //
            // 原来只认「带该表别名前缀的聚合列」（`al.contains(p)`），于是把去重放进 CTE、
            // 外层用**无前缀**的列名聚合，整条判据就被绕过 —— 订单明细数量 TOP10 示例：
            // ```sql
            // WITH dedup AS (SELECT DISTINCT d.sku_name, d.box_quantity FROM t_sales_order_detail d …)
            // SELECT sku_name, SUM(box_quantity) FROM dedup GROUP BY sku_name ORDER BY 订单明细数量 DESC LIMIT 10
            // ```
            // 声明的去重键是五个 `(sales_order_code, sku_code, sku_name, box_quantity, amount)`，
            // SQL 只 DISTINCT 了两个 ⇒ 同一商品在不同订单里箱数相同的行被折成一行 ⇒
            // 首行 13045 而 gold 72863，**低报 5.6 倍**；而 `caliber_note` 里只提了软删过滤，
            // 去重这条**一个字都没有** —— 判据在最需要它的那一刻弃权了。
            //
            // 扩展方式的两条收窄（第一版只写了前者，被自己的测试用例当场证伪 ——
            // 那一版要求「无前缀的聚合列必须出现在声明的去重键里」，而 SALE15 的
            // `box_quantity` **恰好**在五键里，一般情况下被聚合的度量列并不在去重键里
            // （`dedup()` 用例的 keys 是 `[a, b]`、聚合的是 `qty`，当场 left=0））：
            //   ① 列名在声明的去重键里 —— 那它一定是这张表的列；
            //   ② 或者 SQL 里存在 DISTINCT 子查询 —— 「有人已经试图去重」本身就证明了
            //      这张表参与了聚合链，而这正是「CTE 去重后外层无前缀聚合」那个形态。
            // kernel 拿不到 schema（判据不带表名就是为了跨表判），只能用这两个信号。
            // 误判方向：多判一条去重违规 → 回炉 → 保守（本仓「宁可回落，绝不出错数」）。
            // 表根本没出现（`al.is_empty()`）时一律不判，那一档由 `RequireCols` 的口径管。
            let touches_table = f.agg_cols.iter().any(|(p, c)| {
                al.contains(p)
                    || (p.is_empty()
                        && (f.distinct_select
                            || keys.iter().any(|k| k.trim().eq_ignore_ascii_case(c.trim()))))
            });
            if al.is_empty() || !touches_table {
                return None; // 表没出现 / 没在这张表的列上做非 DISTINCT 聚合 → 不判
            }
            if f.distinct_select {
                // 有 DISTINCT 子查询：还要看**键对不对**。此前只判「有没有 DISTINCT」——
                // 实测声明 5 个键、SQL 只写 4 个（漏了金额列），于是只在金额上不同的行被折掉、
                // 销量少算、排行榜第 3 名换了人，而判据全绿。少一个键就是少算。
                let missing: Vec<String> = keys
                    .iter()
                    .map(|k| k.trim().to_lowercase())
                    .filter(|k| !k.is_empty() && !f.distinct_cols.contains(k))
                    .collect();
                if missing.is_empty() {
                    return None;
                }
                return viol("require_dedup", table, human,
                     format!("去重键不全：DISTINCT 里缺 {}。声明的键是 ({})——\
                              少一个键会把只在该列上不同的行折掉，数值**少算**",
                             missing.join(", "), keys.join(", ")));
            }
            viol("require_dedup", table, human,
                 format!("表 {table} 含重复行，聚合前须先按 ({}) 去重：\
                          套一层 SELECT DISTINCT 子查询，或改用 COUNT(DISTINCT …)", keys.join(", ")))
        }
        CaliberRule::RequireLatest { table, partition, human } => {
            let ok = (f.ranked && f.eq_one) || (f.subquery && f.max_agg);
            if ok || f.aliases_of(table).is_empty() {
                return None;
            }
            viol("require_latest", table, human,
                 format!("表 {table} 每个 ({}) 有多条历史记录，须只取最新一条：ROW_NUMBER() OVER \
                          (PARTITION BY … ORDER BY … DESC) 后取 rn = 1，或关联 (SELECT MAX(…)) 子查询",
                        partition.join(", ")))
        }
        CaliberRule::RequirePercentScale { metric, human } => (f.divide && !f.times_100)
            .then(|| viol("require_percent_scale", metric, human,
                          format!("{metric} 是占比：除法结果须 * 100.0 后再 ROUND(…, 2)")))
            .flatten(),
        CaliberRule::RequireJoinAndFilter { table, col, human } => {
            let al = f.aliases_of(table);
            if !al.is_empty() && f.constrained(table, &al, col) {
                return None;
            }
            viol("require_join_and_filter", &format!("{table}.{col}"), human,
                 format!("必须 JOIN {table} 并用 {table}.{col} 过滤，\
                          不要用名称相近的其它列代替"))
        }
        CaliberRule::RequireCodeOnColumn { table, col, code, human } => {
            code_on_column(table, col, code, human, f)
        }
        CaliberRule::RequireKnownValue { table, col, values, human } => {
            known_value(table, col, values, human, f)
        }
        // 只看「这一列有没有被任何条件约束」，不管用什么比较、也不管在哪张表上（见变体文档）。
        // 调用方只在**问句真的带时间范围**时才传这条规则进来 —— 否则「本月销售额 top3 分类」
        // 这类无时间边界的问法会被判红，那是误伤（判错一条会连带把对的答案回炉改错，裁决 二·G）。
        CaliberRule::RequireTimeColumn { col, human } => {
            let c = col.to_lowercase();
            // 🔴 **第二问：分桶用的对时间列吗**。第一问（条件里约束了它吗）只管「有没有过滤」，
            // 而 AS01 那种「过滤对了、分桶用了别的时间列」的错答**两问都满足**，所以要在桶列上再判一次。
            // 判据只在「有 GROUP BY 且投影里采到了桶列」时才开 —— 无桶可判就不判（漏判方向，保守）。
            if f.grouped && !f.bucket_cols.is_empty() && !f.bucket_cols.contains(&c) {
                let wrong = {
                    let mut v: Vec<String> = f.bucket_cols.iter().cloned().collect();
                    v.sort();
                    v.join(" / ")
                };
                return viol(
                    "require_time_column",
                    &c,
                    human,
                    format!(
                        "分桶用了别的时间列 {wrong}，而该指标的时间语义钉在 {col} 上。\
                         ① 若 {col} 就在当前已连接的表上 —— 把投影里的 DATE_FORMAT/DATE_TRUNC 首参从 {wrong} 改成 {col}，其余一字不动；\
                         ② 若 {col} 在另一张表上 —— 必须 JOIN 那张表、把分桶列放到它的 {col} 上，并且**连带补上那张表的口径过滤**。\
                         **不要把 {wrong} 就地改名成 {col}** —— 列不在这张表上时那是个不存在的列。\
                         两列是不同业务时点，换错会把「上一期发生、本期才产生该业务动作」的那批单分错桶"
                    ),
                );
            }
            if f.cond_bare.contains(&c) || f.cond_cols.iter().any(|(_, x)| *x == c) {
                return None;
            }
            // 🔴 **把用错的那一列点出来**，不要只说该用哪一列。
            // 实测（AS03）：判词只写「时间过滤必须用 after_sales_time」，回炉一轮后模型
            // 原封不动仍用 `order_time` —— 它压根没意识到自己写的那个是「别的时间列」。
            // 判词里出现「把 X 换成 Y」这种可执行的指令，才是模型能照做的形态。
            let wrong = f.time_ish_conds(&c);
            let hint = match wrong.is_empty() {
                // 🔴 判词必须**同时覆盖同表与跨表两种形态**，不能只写「就地改名」。
                // 实测两种都真实存在：
                //   · 同表（AS03）：`after_sales_time` 与 `order_time` 都在售后单头上 → 改名即对；
                //   · 跨表（GOODS13）：明细表只有 `delivery_time`，声明的 `order_time`
                //     **在订单头上** → 就地改名会得到一个不存在的列（1054），
                //     正解是 JOIN 订单头、把时间过滤放到它的列上，并带上它的口径过滤。
                // 本判据在 kernel、**拿不到 schema**，分不清是哪一种（这是刻意的：
                // 变体不带表名才能跨表判）。故措辞给两条分支 + 明确禁止盲目改名。
                false => format!(
                    "时间过滤用错了列：现在约束的是 {}，而该指标的时间语义钉在 {col} 上。\
                     ① 若 {col} 就在当前已连接的表上 —— 把 WHERE 里 {} 的那几个条件整段改成 {col}，其余一字不动；\
                     ② 若 {col} 在另一张表上（明细按订单头的下单时点算就是这种）—— 必须 JOIN 那张表、\
                     把时间过滤放到它的 {col} 上，并且**连带补上那张表的口径过滤**（漏了它就等于放开了无效单）。\
                     **不要把 {} 就地改名成 {col}** —— 列不在这张表上时那是个不存在的列。\
                     两列是不同业务时点，换错会漏掉「上一期发生、本期才产生该业务动作」的那批单",
                    wrong.join(" / "),
                    wrong.join(" / "),
                    wrong.join(" / ")
                ),
                true => format!(
                    "时间过滤必须用 {col} 列（该指标的时间语义钉在它上面）：\
                     换成同表的别的时间列会按另一种业务时点分组，\
                     而它常常在别的表上 —— 漏了那张表就连带丢掉它的口径过滤"
                ),
            };
            viol("require_time_column", col, human, hint)
        }
        CaliberRule::NoFanoutJoin { keys, human } => {
            // ② 没有 SUM/AVG/MIN/MAX 列入参就不判（COUNT 族数的是行，扇出常是本意）
            if keys.is_empty() || f.measure_aggs.is_empty() {
                return None;
            }
            // ① 找「被 JOIN 进来那一侧是重复键」的等值连接（基表侧重复键是正常方向）
            let hit = f.join_eqs.iter().find(|(ja, jc, _, _)| {
                f.tables_of_alias(ja)
                    .iter()
                    .any(|t| keys.iter().any(|(kt, kc)| kt == t && kc == jc))
            });
            let Some((ja, jc, oa, _)) = hit else { return None };
            let dup = f.tables_of_alias(ja);
            // ③ 🔴 **被放大的只有「等值另一边」的度量**，不是任意第三张表的度量。
            //
            // 第一版判「度量前缀全落在被 JOIN 侧才放行」—— 把一条合法三表复合金额
            // （`a JOIN b(dup) JOIN c`，SUM 取 `b.price × c.qty`）当场判红：
            // a 的行确实被 b 复制了，但 a 一列度量都不贡献；c 的行数由**它自己与 b 的**
            // 连接粒度决定，与 a↔b 的扇出无关。判据一红，回炉两轮把模型**从正确口径上**
            // 逼走（丢过滤、换时间列）——「判错一条连带把对的答案回炉改错」的活样本。
            //
            // 所以判据是「等值另一边（`oa`）的表贡献了度量」；
            // 裸前缀解不到任何表，扇出在场时它本身就是歧义写法 —— 也算命中（要求写明前缀）。
            let other = f.tables_of_alias(oa);
            let hit_by_other = |p: &String| {
                p.is_empty() || f.tables_of_alias(p).iter().any(|t| other.contains(t))
            };
            if !f.measure_aggs.iter().any(hit_by_other) {
                return None;
            }
            let t = dup.first().copied().unwrap_or(ja);
            viol("no_fanout_join", &format!("{t}.{jc}"), human, format!(
                "JOIN 进 {t}.{jc}（别名 {ja}）会把另一边的每一行复制多份 —— {jc} 在 {t} 里有重复值，\
                 SUM/AVG 随之被放大同样倍数（语法没错、执行不报错，只是数字悄悄错几倍到几百倍）。\
                 三选一：① 这个 JOIN 只为取名称/属性 —— 改去该实体的**主档表** JOIN（一对一、不复制行）；\
                 ② 只为过滤另一边的行 —— 用 EXISTS 或 IN 子查询代替这个 JOIN；\
                 ③ 确需同时聚合两边 —— 先把其中一边按连接键预聚合/去重成一行一键，再 JOIN"))
        }
        CaliberRule::RequireTimeCap { col, human } => {
            // 只认 `< CURDATE()` 这一种（回炉指令给的也是它 —— 等价形态多认一种多一分漏判面）。
            // 列名不带表名（与 `RequireTimeColumn` 同一条：声明只知道列，不知道它在哪张表）。
            let c = col.to_lowercase();
            if f.cap_curdate.contains(&c) {
                return None;
            }
            viol("require_time_cap", col, human, format!(
                "时间过滤的上限必须是 `{c} < CURDATE()` —— 该指标算到**昨天**，今天的数据不全，\
                 含今天数字会虚。把 `{c}` 的上限从期月末日/今天改成 `< CURDATE()`，其余一字不动"))
        }
        CaliberRule::RequireCodeEq { table, col, values, human } => {
            if values.is_empty() || f.aliases_of(table).is_empty() {
                return None;
            }
            let want = col.to_lowercase();
            // cond_lits 已含 LIKE 家族（`pair_op` 剥了 %）：名即字面量逐字等值。
            // 名≠码是构造侧就过滤好的 —— 写名必返 0 行（可证，见变体文档）。
            let mut named: Vec<(&str, &str)> = f
                .cond_lits
                .iter()
                .filter(|(c, _)| *c == want)
                .filter_map(|(_, v)| {
                    values.iter().find(|(n, code)| n == v && code != v).map(|(n, code)| (n.as_str(), code.as_str()))
                })
                .collect();
            named.sort();
            named.dedup();
            let Some((name, code)) = named.first() else { return None };
            viol("require_code_eq", &format!("{table}.{col}"), human, format!(
                "{table}.{col} 存的是**码不是名**：现在写的 `{want}` 用了名称「{name}」 —— \
                 名称写法在码列上**必返 0 行**（`{want}` 里没有一个字是码）。\
                 「{name}」登记的码是 `{code}`：把条件改成 `{want} = '{code}'，其余一字不动"))
        }
    }
}

/// 「码取对了、列用错了」：只在**该码确实被用作某列的取值**时才判，判的是「用在哪一列」。
///
/// 三处刻意偏漏判：① 码压根没出现 → 不判（换写法按名过滤是正解，硬判就是误伤）；
/// ② 声明列上也写了同一个码（哪怕别的列上也写了）→ 不判；
/// ③ 只比列名不比前缀 —— 同名列在另一张表上几乎总是同一本字典（`c.x` 与 `cus.x`），
/// 而「前缀写错」不是本判据要治的错法。
fn code_on_column(
    table: &str,
    col: &str,
    code: &str,
    human: &str,
    f: &Facts,
) -> Option<Violation> {
    let want = col.to_lowercase();
    let mut used: Vec<&str> =
        f.cond_lits.iter().filter(|(_, v)| v == code).map(|(c, _)| c.as_str()).collect();
    if used.is_empty() || used.contains(&want.as_str()) {
        return None;
    }
    used.sort_unstable(); // HashSet 序不定，hint 必须逐次一致（回炉指令有 golden 对比）
    used.dedup();
    viol("require_code_on_column", &format!("{table}.{col}"), human,
         format!("取值 {code} 是 {table}.{col} 的编码，条件里却把它用在 {} 上：\
                  同一个编码换一列含义就变了，请改用 {table}.{col} 约束（或按名称过滤）",
                used.join(", ")))
}

/// 回炉说明里列出的合法取值上限。**这几个值是 LLM 唯一能据以改对的信息**，不是可省的装饰：
/// 只说「这个值不存在」，它只会换一个同样不存在的近义词（`RequireTimeColumn` 那条判词已经
/// 吃过一次「不点名就照旧」的账）。上限存在是因为一本字典可能有几百个码（登记的是字典全码，
/// 而抽样那 60 个上限只管「要不要对码」），整本抄进回炉指令是纯浪费。
const LEGAL_VALUES_IN_HINT: usize = 30;

/// 「值不在已登记的取值里」→ 返 0 行。判据形态见 `CaliberRule::RequireKnownValue`。
///
/// 四处刻意偏漏判（每一处都是一次假红的来源，而误伤一条会连带把对的答案回炉改错）：
/// ① **只判非 ASCII 字面量**（＝名字型的值）。登记集**不保证覆盖列里全部的码**：构造侧那批
///    自动发现的码表只要求抽样覆盖 ≥80%，剩下 ≤20% 未覆盖的真码写进 SQL 是**对的**，
///    按登记集判它就是假红。而名字型的值在码列上只有两种命运 —— 登记过（确定性换码器会换成码）
///    或返 0 行，两种都不需要「它可能是个真值」这条退路。
/// ② 声明的表不在 FROM/JOIN 里 → 不判（防误伤原则①）。同名列在别的表上可能压根不是码列。
/// ③ `values` 为空 → 不判（空集会把每一个名字型的值判红）。
/// ④ 只比列名不比前缀（与 `code_on_column` ③ 同一条：同名列几乎总是同一本字典）。
fn known_value(
    table: &str,
    col: &str,
    values: &[(String, String)],
    human: &str,
    f: &Facts,
) -> Option<Violation> {
    if values.is_empty() || f.aliases_of(table).is_empty() {
        return None;
    }
    let want = col.to_lowercase();
    let known = |v: &str| values.iter().any(|(n, c)| n == v || c == v);
    // `eq_lits` 而非 `cond_lits`：只有等值家族（`=` / `IN`）才配得上下面那句
    // 「请把值换成合法取值之一」。见 `Facts::eq_lits` 的文档 —— 对 `!=` / `NOT IN` / `LIKE`
    // 照这句去改，是判据主动指令了一次语义改写。
    let mut bad: Vec<&str> = f
        .eq_lits
        .iter()
        .filter(|(c, v)| *c == want && !v.is_ascii() && !known(v))
        .map(|(_, v)| v.as_str())
        .collect();
    if bad.is_empty() {
        return None;
    }
    bad.sort_unstable(); // HashSet 序不定，hint 必须逐次一致（回炉指令有 golden 对比）
    bad.dedup();
    let list: Vec<String> = values
        .iter()
        .take(LEGAL_VALUES_IN_HINT)
        .map(|(n, c)| format!("{n}={c}"))
        .collect();
    let more = match values.len() > LEGAL_VALUES_IN_HINT {
        true => format!("…（共 {} 个取值，只列了前 {LEGAL_VALUES_IN_HINT} 个）", values.len()),
        false => String::new(),
    };
    viol("require_known_value", &format!("{table}.{col}"), human,
         format!("{table}.{col} 的取值是一份完整的码表，而条件里写的 {} 不在里面：\
                  这条 SQL 语法没错、执行也不报错，但那个值**一行都匹配不到** —— \
                  只有这一个等值条件时就是 0 行，用户会读成「库里没有这类数据」。\
                  合法取值（名=码）：{}{more}。请把值换成其中之一（用码最稳）。\
                  🔴 **不要删掉这个条件、也不要挪到别的列上试** —— 那会把「0 行」换成一个更宽的错数；\
                  若这份取值里确实没有问句要的那个，那就是库里真的没有这类数据。",
                 bad.join(", "), list.join(" / ")))
}

fn viol(kind: &str, key: &str, human: &str, hint: String) -> Option<Violation> {
    Some(Violation { rule: format!("{kind}:{}", key.to_lowercase()), human: human.to_string(), hint })
}

// ---------- 事实采集 ----------

#[derive(Default)]
struct Facts {
    /// (别名或表名, 真实表名)，含所有嵌套层级
    aliases: Vec<(String, String)>,
    /// **写了别名**的真实表名。
    ///
    /// 🔴 由来（评测 FIN01，实测差 **299 倍**）：发票两条子查询写的是**裸** `deleted_flag = 0`，
    /// 而 `Facts::constrained` 把「裸列出现过」一律当成「该列被约束」—— 于是
    /// `RequireCols{t_customer,[deleted_flag]}` 的 miss 为空、判绿，
    /// 而 `LEFT JOIN t_customer c` 实际**漏了** `c.deleted_flag = 0` ⇒ 客户主档一码多行
    /// ⇒ 组内扇出（654888936 = 2190264 × 299 整）。
    ///
    /// 判据只能认两类**安全**的裸列：
    ///   ① 该表**没写别名**（写别名后裸列在多表下有歧义，从宽就会把别的表的条件当成它的）；
    ///   ② 整条 SQL 只有**一张**基表（单表下裸列无歧义）。
    /// 两者都不满足时，裸列**不算**约束 —— 判红回炉，失败方向保守。
    aliased: HashSet<String>,
    /// WHERE / JOIN ON 里出现的 (前缀, 列)
    cond_cols: HashSet<(String, String)>,
    /// WHERE / JOIN ON 里出现的裸列（无前缀，单表查询的常见写法）
    cond_bare: HashSet<String>,
    /// WHERE / JOIN ON 里「列 ↔ 字面量」的配对 `(列名, 字面量)`，前缀不记（见 `code_on_column` ③）。
    /// 收 `= / != / IN / LIKE` 四种形态 —— 它回答的是「这个字面量被当成该列的取值用了吗」。
    cond_lits: HashSet<(String, String)>,
    /// `cond_lits` 里**只有等值家族**（`=` 与 `IN`）的那一部分。
    ///
    /// 🔴 为什么必须与 `cond_lits` 分开：`known_value` 的判词是「这个值不在码表里 ⇒ 匹配不到行，
    /// 请换成合法取值之一」。这句话对 `!=` 是**反的** —— `col != '不存在的值'` 今天等于不过滤，
    /// 照判词去改会把「不过滤」变成「排除掉一个真实类别」，那是判据自己指令了一次语义改写，
    /// 比原来的偏差更难发现。对 `LIKE '%中文%'` 同样错（模糊匹配不要求等于码表某一项）。
    /// `code_on_column` 那条判据要的是四种形态全收，所以不能就地收窄 `cond_lits`。
    eq_lits: HashSet<(String, String)>,
    /// 非 DISTINCT 的 SUM/AVG/COUNT 入参里引用的 (前缀, 列)
    agg_cols: HashSet<(String, String)>,
    /// 子查询里出现过 `SELECT DISTINCT`。存在的意义是兜住「内外同名别名」：
    /// `SUM(d.qty) FROM (SELECT DISTINCT … FROM tbl d) d` 里 agg 的前缀与基表别名撞车。
    distinct_select: bool,
    /// 嵌套 `SELECT DISTINCT` 投影里出现的列名（不带前缀）。
    /// 用来判「去重用的是不是**声明的那几个键**」—— 只判「有没有 DISTINCT」是不够的：
    /// 少一个键就会把只在该列上不同的行折掉，实测少算之后排行榜第 3 名就换了人。
    distinct_cols: HashSet<String>,
    /// ROW_NUMBER() OVER (PARTITION BY 非空)
    ranked: bool,
    /// 任意 `列 = 1`（配合 ranked 认「rn = 1」形态）
    eq_one: bool,
    subquery: bool,
    max_agg: bool,
    /// 投影里有除法（条件里的除法不算，避免误伤）
    divide: bool,
    times_100: bool,
    /// 🔴 **时间桶列**：投影里的日期截断函数（`DATE_FORMAT/DATE_TRUNC/…`）首参里出现的时间列。
    ///
    /// 由来（评测 AS01「今年每个月的售后退款金额趋势」，**三种配置都错**）：
    /// ```sql
    /// SELECT DATE_FORMAT(o.order_time, '%Y-%m') AS `月份`, SUM(a.refund_amount) …
    /// WHERE … AND a.after_sales_time >= '2026-01-01'   -- 过滤是对的
    /// ```
    /// `RequireTimeColumn` 整条判据只有一行「这一列在任何**条件**里被提到过就绿」，
    /// 而过滤恰好是对的 —— 错在**分桶**用了别的时间列（`o.order_time` 而非 `after_sales_time`），
    /// 于是 `Verdict::Pass`、`caliber_note` 空、10 桶错数原样返回。
    /// 更糟的是判词只说「把时间**过滤**改到 after_sales_time」，模型照做后判据当场变绿、
    /// 桶列一字未动 —— **回炉指令把判据教成了绿灯**。
    ///
    /// 采集刻意只在**投影**里找（`!cond`）：WHERE 里的 `DATE_FORMAT(col,…)` 是过滤不是桶。
    bucket_cols: HashSet<String>,
    /// 有非空的 `GROUP BY`。**只判存在性不看表达式**：
    /// AS01 的 gold 与错答都写 `GROUP BY 月份`（按输出别名），只看 GROUP BY 会同时漏掉两边 ——
    /// 真相在投影表达式里（哪个时间列被拿来截断）。
    grouped: bool,
    /// JOIN 等值条件 `(被JOIN侧别名, 列, 另一侧别名, 列)`。只收 INNER/LEFT/FULL 族的
    /// `ON a.x = b.y`：RIGHT 的方向语义相反（被 JOIN 侧是「主」侧）、LLM 实际不写 —— 漏判方向。
    join_eqs: Vec<(String, String, String, String)>,
    /// SUM/AVG/MIN/MAX（非 DISTINCT）列入参的**前缀**（裸列记空串）。COUNT 族不进 ——
    /// `NoFanoutJoin` 的②（COUNT 数的是行，扇出常是本意）。
    measure_aggs: HashSet<String>,
    /// 写过 `col < CURDATE()` 的列（裸列名）。`RequireTimeCap` 认的唯一形态 —
    /// 等价写法（`<= CURDATE()-1`）刻意不认：回炉指令给的就是 `< CURDATE()`，收敛到一种。
    cap_curdate: HashSet<String>,
}

impl Facts {
    fn aliases_of(&self, table: &str) -> Vec<String> {
        let t = table.to_lowercase();
        self.aliases.iter().filter(|(_, tb)| *tb == t).map(|(a, _)| a.clone()).collect()
    }
    /// 条件里出现的、**看起来是时间列**的列名（去重、排序稳定），排除 `except`。
    ///
    /// 只服务于判词措辞（「你现在约束的是 X」），**不参与任何判定** —— 所以这里用列名的
    /// 词法特征就够，不必去查真实类型。判宽了最坏是判词里多点一个列名（模型照样知道换哪个），
    /// 判窄了则退回原来那句泛泛的措辞：两个方向都不会改变红/绿。
    fn time_ish_conds(&self, except: &str) -> Vec<String> {
        let ish = |c: &str| c.contains("time") || c.contains("date") || c.contains("_at");
        let mut v: Vec<String> = self
            .cond_bare
            .iter()
            .cloned()
            .chain(self.cond_cols.iter().map(|(_, c)| c.clone()))
            .filter(|c| c != except && ish(c))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// 基表数（按真实表名去重）。裸列在只有一张基表时才无歧义。
    fn base_table_count(&self) -> usize {
        self.aliases.iter().map(|(_, t)| t).collect::<HashSet<_>>().len()
    }

    /// 别名 → 真实表名。一个别名可经转发登记到多张表（派生表的外层别名），故返回 Vec。
    fn tables_of_alias<'s>(&'s self, alias: &str) -> Vec<&'s str> {
        self.aliases
            .iter()
            .filter(|(a, _)| a == alias)
            .map(|(_, t)| t.as_str())
            .collect()
    }

    /// 列被约束＝以任一别名限定出现在条件里；**裸列只在两类安全形态下才算**：
    /// 该表没写别名，或整条 SQL 只有一张基表（见 `Facts::aliased` 的文档）。
    fn constrained(&self, table: &str, aliases: &[String], col: &str) -> bool {
        let c = col.to_lowercase();
        let bare_safe = !self.aliased.contains(&table.to_lowercase()) || self.base_table_count() <= 1;
        (bare_safe && self.cond_bare.contains(&c))
            || aliases.iter().any(|a| self.cond_cols.contains(&(a.clone(), c.clone())))
    }
}

/// `nested`＝这条 Query 是子查询/CTE/派生表。它决定两件事：`subquery` 标志，
/// 以及 `SELECT DISTINCT` 算不算「去重子查询」——顶层的 DISTINCT 去的是输出行，不是输入行。
fn scan_query(q: &Query, f: &mut Facts, nested: bool) {
    f.subquery |= nested;
    if let Some(w) = &q.with {
        for c in &w.cte_tables {
            scan_aliased_subquery(&c.query, f, Some(c.alias.name.value.as_str()));
        }
    }
    scan_setexpr(&q.body, f, nested);
}

fn scan_setexpr(b: &SetExpr, f: &mut Facts, nested: bool) {
    match b {
        SetExpr::Select(s) => scan_select(s, f, nested),
        SetExpr::Query(q) => scan_query(q, f, nested),
        SetExpr::SetOperation { left, right, .. } => {
            scan_setexpr(left, f, nested);
            scan_setexpr(right, f, nested);
        }
        _ => {}
    }
}

fn scan_select(s: &Select, f: &mut Facts, nested: bool) {
    if nested && s.distinct.is_some() {
        f.distinct_select = true;
        // 记下这条 DISTINCT 到底按哪些列去重。取**列**不取别名：`d.amount AS amt` 的
        // 去重语义由 `amount` 决定，把 `amt` 记下来会让「键齐不齐」判错。
        for it in &s.projection {
            let expr = match it {
                SelectItem::UnnamedExpr(e) => e.to_string(),
                SelectItem::ExprWithAlias { expr, .. } => expr.to_string(),
                other => other.to_string(),
            };
            let last = expr.rsplit('.').next().unwrap_or(&expr);
            let col = last.trim().trim_matches('`').to_lowercase();
            if !col.is_empty() {
                f.distinct_cols.insert(col);
            }
        }
    }
    for twj in &s.from {
        scan_twj(twj, f);
    }
    for e in s.selection.iter().chain(s.prewhere.iter()) {
        grab(e, f, true);
    }
    for it in &s.projection {
        grab(it, f, false);
    }
    // 🔴 **时间桶列**：只看投影里的日期截断函数，且只看**非条件**那一支。
    // 遍历的是投影里的 `DATE_FORMAT(col, …)` 这类函数，取其首参 ——
    // WHERE 里的同名函数是过滤不是桶（由上面的 `grab(e, f, true)` 在 cond=true 时扫，这里不重复）。
    // `grouped` 只看存在性：gold 与错答都写 `GROUP BY 月份`（按别名），看表达式会两边都漏。
    if let GroupByExpr::Expressions(gs, _) = &s.group_by {
        f.grouped |= !gs.is_empty();
    }
    for e in s.having.iter().chain(s.qualify.iter()) {
        grab(e, f, false);
    }
}

fn scan_twj(t: &TableWithJoins, f: &mut Facts) {
    scan_factor(&t.relation, f);
    for j in &t.joins {
        scan_factor(&j.relation, f);
        // 不逐个匹配 JoinOperator 的 14 个变体：派生的 Visit 会走到 ON/USING 里的表达式
        grab(&j.join_operator, f, true);
        // `NoFanoutJoin` 的原始材料：被 JOIN 侧的别名 + 它的等值键对
        if let TableFactor::Table { name, alias, .. } = &j.relation {
            let ja = alias
                .as_ref()
                .map(|a| a.name.value.to_lowercase())
                .unwrap_or_else(|| {
                    name.0.last().map(|p| p.value.to_lowercase()).unwrap_or_default()
                });
            if let Some(on) = on_expr_of(&j.join_operator) {
                collect_join_eqs(on, &ja, f);
            }
        }
    }
}

/// INNER/LEFT/FULL 族的 ON 表达式。RIGHT 方向语义相反（被 JOIN 侧是「主」侧）、
/// LLM 实际不写 —— 漏判方向。CROSS/USING/NATURAL 没有可收的等值对。
fn on_expr_of(op: &JoinOperator) -> Option<&Expr> {
    use sqlparser::ast::JoinOperator as J;
    match op {
        J::Inner(JoinConstraint::On(e))
        | J::LeftOuter(JoinConstraint::On(e))
        | J::FullOuter(JoinConstraint::On(e)) => Some(e),
        _ => None,
    }
}

/// ON 里的 `joined.x = other.y` 等值对（沿 AND 链与括号下钻，别的形态一律不碰）。
fn collect_join_eqs(e: &Expr, ja: &str, f: &mut Facts) {
    use sqlparser::ast::BinaryOperator as B;
    match e {
        Expr::Nested(inner) => collect_join_eqs(inner, ja, f),
        Expr::BinaryOp { left, op: B::And, right } => {
            collect_join_eqs(left, ja, f);
            collect_join_eqs(right, ja, f);
        }
        Expr::BinaryOp { left, op: B::Eq, right } => {
            let (Some(l), Some(r)) = (prefixed_col(left), prefixed_col(right)) else { return };
            if l.0 == ja {
                f.join_eqs.push((l.0, l.1, r.0, r.1));
            } else if r.0 == ja {
                f.join_eqs.push((r.0, r.1, l.0, l.1));
            }
        }
        _ => {}
    }
}

/// `a.x` → `(a, x)`（小写、去反引号）。裸列与函数包裹一律 None ——
/// 连接键不带前缀的写法在多表 JOIN 里本来就歧义，不收是漏判方向。
fn prefixed_col(e: &Expr) -> Option<(String, String)> {
    let Expr::CompoundIdentifier(p) = e else { return None };
    if p.len() < 2 {
        return None;
    }
    Some((
        p[p.len() - 2].value.to_lowercase(),
        p[p.len() - 1].value.trim_matches('`').to_lowercase(),
    ))
}

fn scan_factor(tf: &TableFactor, f: &mut Facts) {
    match tf {
        TableFactor::Table { name, alias, .. } => {
            let t = name.0.last().map(|p| p.value.to_lowercase()).unwrap_or_default();
            let key = alias.as_ref().map(|a| a.name.value.to_lowercase()).unwrap_or_else(|| t.clone());
            f.aliases.push((key, t.clone()));
            // 🔴 「该表写了别名」**只在 `Table` 分支记**，不能用「`aliases_of(t)` 的键 ≠ t」判断。
            // `scan_aliased_subquery` 会把派生表别名 `inv` **转发**登记给内部表（那是「键≠t」），
            // 用它当「写了别名」会把子查询里的表也算成写了别名 → 把别的表的裸列条件当成它的约束
            // —— 那正是 FIN01 要堵的形态（发票子查询裸 `deleted_flag` 冒充了 `t_customer` 的约束）。
            if alias.is_some() {
                f.aliased.insert(t);
            }
        }
        TableFactor::Derived { subquery, alias, .. } => {
            scan_aliased_subquery(subquery, f, alias.as_ref().map(|a| a.name.value.as_str()))
        }
        TableFactor::NestedJoin { table_with_joins, .. } => scan_twj(table_with_joins, f),
        _ => {}
    }
}

/// 扫一个 CTE / 派生表，并把它的**外层别名登记成内部所有表的转发别名**。
///
/// 🔴 没有这一步，判据看不见 LLM 实际最常写的那个形状：
/// ```text
/// WITH dedup AS (SELECT DISTINCT … FROM t_x d …) SELECT SUM(dedup.qty) FROM dedup
/// ```
/// 聚合前缀是 `dedup`，而 `aliases_of("t_x")` 只有 `d` —— `RequireDedup` 的前置条件
/// （「在这张表的列上做了聚合」）不成立，**整条规则一次都不触发**，`correction_log` 全空。
/// 实测就是这样漏掉一次「声明 5 个去重键、SQL 只写 3 个 → 少算 → 排行榜换人」。
///
/// `distinct_select` 那个字段的注释里写的「兜住内外同名别名」是同一个问题的**权宜解**：
/// 它只在 LLM 恰好把派生表命名成与内层别名相同（`… FROM tbl d) d`）时生效，
/// 换成 `) t` 或 `WITH dedup` 就失效。转发别名是治本的那一处。
///
/// 误差方向是刻意的：别名集**变宽** → `RequireCols` 更宽松（少误报）、
/// `RequireDedup` 前置条件更容易成立（该判的判得到）。两个方向都是要的。
fn scan_aliased_subquery(q: &Query, f: &mut Facts, outer: Option<&str>) {
    let before = f.aliases.len();
    scan_query(q, f, true);
    let Some(outer) = outer else { return };
    let outer = outer.to_lowercase();
    // 先收集再追加：`f.aliases` 正在被借
    let inner: Vec<String> = f.aliases[before..].iter().map(|(_, t)| t.clone()).collect();
    for t in inner {
        f.aliases.push((outer.clone(), t));
    }
}

/// 在任意 AST 节点上跑一遍表达式级采集。`cond`＝该节点是 WHERE/ON（决定列是否记为「被约束」）。
fn grab<N: Visit>(node: &N, f: &mut Facts, cond: bool) {
    let _ = node.visit(&mut Grab { f, cond, agg: false, measure: false });
}

struct Grab<'a> {
    f: &'a mut Facts,
    cond: bool,
    agg: bool,
    /// 当前在 SUM/AVG/MIN/MAX（非 DISTINCT）的入参里 → 列前缀收进 `measure_aggs`
    measure: bool,
}

impl Visitor for Grab<'_> {
    type Break = ();

    /// 只在表达式内层被调到 → 必是子查询（顶层 Query 走 `scan_query`，不经这里）。
    ///
    /// 🔴 必须把子查询交回 `scan_query` 走一遍，不能只置标志位：Visitor 的 `cond` 是
    /// **进入点决定**的，投影里的标量子查询以 `cond = false` 进来，于是
    /// `pre_visit_table_factor` 登记了它的表、而它 `WHERE` 里的列**一列都不记**
    /// → 「表在册、约束不在册」→ `RequireCols` 对「只出现在投影子查询里的表」恒判违规。
    /// 实测：占比类派生指标（分子分母各一个标量子查询，两侧 WHERE 都写足了口径列）
    /// 被判红、回炉一轮后仍红 → 白花一次 precise LLM 还给用户挂上「结果不可信」。
    ///
    /// ponytail: 交回 `scan_query` 后，本 Visitor 仍会把同一棵子树再走一遍（拿旧 `cond`）。
    /// 事实采集全是 set/flag，重复无害；`aliases` 是 Vec 会有重复项，`aliases_of` 不在意。
    /// 天花板是深层嵌套下的重复遍历，实际深度 2-3 层。要精确就得让 Visitor 带作用域栈，
    /// 那是把 sqlparser 的 Visit 重写一遍，不值。
    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<()> {
        self.f.subquery = true;
        scan_query(q, self.f, true);
        ControlFlow::Continue(())
    }

    /// 表达式里的子查询也可能引表（`IN (SELECT … FROM x)`），一并登记
    fn pre_visit_table_factor(&mut self, tf: &TableFactor) -> ControlFlow<()> {
        if matches!(tf, TableFactor::Table { .. }) {
            scan_factor(tf, self.f);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
        match e {
            Expr::CompoundIdentifier(p) if p.len() >= 2 => {
                let pair = (
                    p[p.len() - 2].value.to_lowercase(),
                    p[p.len() - 1].value.trim_matches('`').to_lowercase(),
                );
                if self.agg {
                    self.f.agg_cols.insert(pair.clone());
                }
                if self.measure {
                    self.f.measure_aggs.insert(pair.0.clone());
                }
                if self.cond {
                    self.f.cond_cols.insert(pair);
                }
            }
            Expr::Identifier(i) => {
                let c = i.value.trim_matches('`').to_lowercase();
                // 🔴 **无前缀的聚合列也要收**（前缀记空串）。
                //
                // 原来这一支只在 `self.cond` 时收进 `cond_bare`，于是 CTE / 派生表外层的
                // `SUM(qty)`（裸列、无别名前缀）在 `agg_cols` 里**根本不存在** ——
                // `RequireDedup` 的前置条件拿不到它，整条判据弃权。
                // 实测代价（评测 SALE15）：声明五个去重键、SQL 只 DISTINCT 两个，
                // 订单明细数量低报 5.6 倍，而 `caliber_note` 里去重这条一个字都没有。
                //
                // 空前缀不会让判据变宽：`RequireDedup` 那边用「列名必须出现在**声明的去重键**里」
                // 收窄（见那里的注释）。`agg_cols` 全仓只有 `RequireDedup` 读，无别的连带影响。
                if self.agg {
                    self.f.agg_cols.insert((String::new(), c.clone()));
                }
                if self.measure {
                    self.f.measure_aggs.insert(String::new());
                }
                if self.cond {
                    self.f.cond_bare.insert(c);
                }
            }
            Expr::BinaryOp { left, op, right } => self.binop(left, op, right),
            Expr::Function(fun) => self.func(fun),
            // `IN (…)` 是等值的析取：某个成员不在码表里，那一块就一行都匹配不到 ——
            // 「换成合法取值之一」这句指令对它是对的，所以算等值家族。
            // `NOT IN` 与 `!=` 同理**不算**等值家族（照判词去改会把「不排除」变成
            // 「排除掉一个真实类别」），故取 `!negated`。
            Expr::InList { expr, list, negated, .. } if self.cond => {
                for v in list {
                    self.pair_op(expr, v, !negated);
                }
            }
            Expr::Like { expr, pattern, .. } | Expr::ILike { expr, pattern, .. } if self.cond => {
                self.pair(expr, pattern)
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

impl Grab<'_> {
    /// 条件里的「列 ↔ 字面量」配对（`col = 'x'` / `IN (…)` / `LIKE '%x%'` 三种形态共用）。
    /// 字面量两侧的 `%` 剥掉：组合值列的正确写法就是 `LIKE '%码%'`，那也算「码用在这一列上」。
    fn pair(&mut self, col: &Expr, lit: &Expr) {
        self.pair_op(col, lit, false)
    }

    /// `eq = true` 时同时记进 `eq_lits`（只有 `=` 与 `IN` 这么调，见 `eq_lits` 的文档）
    fn pair_op(&mut self, col: &Expr, lit: &Expr, eq: bool) {
        if let (Some(c), Some(v)) = (col_name(col), lit_of(lit)) {
            let v = v.trim_matches('%').to_string();
            if eq {
                self.f.eq_lits.insert((c.clone(), v.clone()));
            }
            self.f.cond_lits.insert((c, v));
        }
    }

    fn binop(&mut self, left: &Expr, op: &sqlparser::ast::BinaryOperator, right: &Expr) {
        use sqlparser::ast::BinaryOperator as B;
        // `col < CURDATE()`：`RequireTimeCap` 认的唯一形态（回炉指令给的也是它）
        if matches!(op, B::Lt) && is_curdate(right) {
            if let Some(c) = col_name(left) {
                self.f.cap_curdate.insert(c.to_lowercase());
            }
        }
        // 只有等值家族算「把这个字面量当成该列的取值」：`amt > 108` 里的 108 是阈值不是码，
        // 认它就会凭一个无关的阈值判红（宁缺毋滥的同一侧）。
        if self.cond && matches!(op, B::Eq | B::NotEq) {
            self.pair_op(left, right, matches!(op, B::Eq));
            self.pair_op(right, left, matches!(op, B::Eq));
        }
        match op {
            // 条件里的除法不算「投影里的占比」——那是过滤阈值，不是输出单位
            B::Divide if !self.cond => self.f.divide = true,
            B::Multiply if is_num(left, 100.0) || is_num(right, 100.0) => self.f.times_100 = true,
            B::Eq
                if is_num(right, 1.0)
                    && matches!(left, Expr::Identifier(_) | Expr::CompoundIdentifier(_)) =>
            {
                self.f.eq_one = true
            }
            _ => {}
        }
    }

    fn func(&mut self, fun: &Function) {
        let name = fun.name.0.last().map(|i| i.value.to_lowercase()).unwrap_or_default();
        if name == "row_number" {
            if let Some(WindowType::WindowSpec(w)) = &fun.over {
                self.f.ranked |= !w.partition_by.is_empty();
            }
        }
        if name == "max" {
            self.f.max_agg = true;
        }
        // 🔴 **时间桶列**：日期截断函数的首参。
        //
        // 只在**投影**里采（`!self.cond`）：WHERE 里的 `DATE_FORMAT(col,…)` 是过滤不是桶。
        // 函数名限定日期截断族，且首参必须是列引用 —— 没有这两条收窄时 `LEFT(sku_name, 2)`
        // 会被当成桶列，把 GOODS17 那条正确 SQL 当场判红（那类假红会把对的答案回炉改错）。
        // 复用 `time_ish_conds` 里那个 `ish` 词法谓词：含 time/date/_at 才算时间列。
        if !self.cond
            && matches!(
                name.as_str(),
                "date_format" | "date" | "year" | "month" | "quarter" | "week" | "yearweek"
                    | "date_trunc" | "left" | "substr"
            )
        {
            if let Some(arg) = first_arg_column(fun) {
                let col = arg.to_lowercase();
                if col.contains("time") || col.contains("date") || col.contains("_at") {
                    self.f.bucket_cols.insert(col);
                }
            }
        }
        // COUNT(DISTINCT …) 自带去重，不进 agg_cols。
        // `measure`（SUM/AVG/MIN/MAX）单独一旗：MIN/MAX 不进 `agg_cols`（`RequireDedup` 的
        // 前置语义不变），但它们的入参前缀要进 `measure_aggs`（`NoFanoutJoin` 的②）。
        let agg = matches!(name.as_str(), "sum" | "avg" | "count");
        let measure = matches!(name.as_str(), "sum" | "avg" | "min" | "max");
        if let (true, FunctionArguments::List(l)) = (agg || measure, &fun.args) {
            if !matches!(l.duplicate_treatment, Some(DuplicateTreatment::Distinct)) {
                let _ = l.visit(&mut Grab { f: &mut *self.f, cond: self.cond, agg, measure });
            }
        }
    }
}

/// 日期截断函数的首参是列引用 → 列名。`DATE_FORMAT(col, fmt)` / `DATE_TRUNC('unit', col)` 两种都认。
///
/// `first_arg` 的实现刻意**不**只看第一个参数：`DATE_TRUNC` 的首参是粒度字面量（`'month'`），
/// 真正的时间列在第二个参数 —— 所以取**第一个能解析出列名的参数**，而不是 `args[0]`。
/// 只取列引用（`col_name`），函数包裹的列不认 —— 漏判方向（`LEFT(CONCAT(x,y),2)` 不被当桶）。
fn first_arg_column(fun: &Function) -> Option<String> {
    if let FunctionArguments::List(l) = &fun.args {
        for arg in &l.args {
            let expr = match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
                FunctionArg::Named { arg: FunctionArgExpr::Expr(e), .. } => Some(e),
                _ => None,
            }?;
            if let Some(c) = col_name(expr) {
                return Some(c);
            }
        }
    }
    None
}

/// 表达式是列引用 → 末段列名（小写、去反引号）。别的形态一律 `None`（函数包裹的列不认，漏判方向）。
fn col_name(e: &Expr) -> Option<String> {
    let id = match e {
        Expr::Identifier(i) => i,
        Expr::CompoundIdentifier(p) => p.last()?,
        _ => return None,
    };
    Some(id.value.trim_matches('`').to_lowercase())
}

/// `CURDATE()` 函数调用（`RequireTimeCap` 认的唯一上限形态）。
/// `CURRENT_DATE` 裸标识符刻意不认 —— 回炉指令给的是 `CURDATE()`，收敛到一种。
fn is_curdate(e: &Expr) -> bool {
    let Expr::Function(f) = e else { return false };
    f.name.0.last().map(|i| i.value.eq_ignore_ascii_case("curdate")).unwrap_or(false)
}

/// 表达式是字面量 → 它的原文（字符串与数字都要：LLM 两种都写）
fn lit_of(e: &Expr) -> Option<String> {
    match e {
        Expr::Value(Value::SingleQuotedString(s)) => Some(s.clone()),
        Expr::Value(Value::Number(n, _)) => Some(n.clone()),
        _ => None,
    }
}

fn is_num(e: &Expr, want: f64) -> bool {
    matches!(e, Expr::Value(Value::Number(n, _)) if n.parse::<f64>().is_ok_and(|v| v == want))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> CaliberRule {
        CaliberRule::RequireCols {
            table: "tbl_dtl".into(),
            cols: vec!["type_flag".into(), "del_flag".into()],
            human: "明细表标准口径".into(),
        }
    }

    /// 🔴 口径回炉不许改变输出列：只补 WHERE 条件 → 形状不变 → 采纳；
    /// 整条重构（多出列、换了分组表达式）→ 形状变了 → 那不是「只补口径」，不采纳。
    #[test]
    fn repair_must_keep_output_shape() {
        let before = "SELECT cat AS `c`, SUM(amt) AS `v` FROM tbl_dtl GROUP BY cat";
        // 只补口径：输出列一字不变 → 采纳
        let only_filter =
            "SELECT cat AS `c`, SUM(amt) AS `v` FROM tbl_dtl WHERE del_flag = 0 GROUP BY cat";
        assert!(keeps_output_shape(before, only_filter));
        // 重构：多出两列、分组表达式也换了 → 不采纳
        let restructured = "SELECT LEFT(nm, 2) AS `c`, nm AS `n`, SUM(amt) AS `v`, \
                            ROW_NUMBER() OVER (PARTITION BY LEFT(nm, 2) ORDER BY SUM(amt) DESC) AS `r` \
                            FROM tbl_dtl GROUP BY nm";
        assert!(!keeps_output_shape(before, restructured));
        // 列数相同但别名换了也算改了意图（前端列头会变）
        assert!(!keeps_output_shape(before, "SELECT cat AS `x`, SUM(amt) AS `v` FROM tbl_dtl GROUP BY cat"));
        // 解析不出 → 返回 true（漏判方向，与 check_caliber 一致：不许因为看不懂就丢掉自修）
        assert!(keeps_output_shape(before, "这不是 SQL"));
        assert_eq!(output_shape("这不是 SQL"), None);
        // 🔴 裸引用带反引号 = 同一输出列（SALE15 实测：合规修复被这句挡掉，坏 SQL 原样返回）
        let before_cn = "SELECT sku_name AS 商品名称, SUM(amt) AS 订单明细数量 FROM dedup GROUP BY sku_name";
        let same_cols_quoted = "SELECT `商品名称`, SUM(`订单明细数量`) AS `订单明细数量` FROM (SELECT DISTINCT sku_name, amt FROM tbl_dtl) t GROUP BY `商品名称`";
        assert!(keeps_output_shape(before_cn, same_cols_quoted), "反引号只是字节差异，列一个都没动");
        // 但「换了个别名列」照样不许过（防借剥反引号放水）
        assert!(!keeps_output_shape(before_cn, "SELECT `商品`, SUM(`订单明细数量`) AS `订单明细数量` FROM t GROUP BY `商品`"));
    }

    /// 🔴 投影里的标量子查询：表与它的 WHERE 在**同一个**子查询里，必须一起被采集。
    /// 此前 Visitor 以进入点的 `cond = false` 走进去 → 登记了表、丢掉了约束
    /// → 这类比值查询恒判违规（回炉一轮仍红，白烧一次 precise LLM）。
    #[test]
    fn cols_seen_inside_projection_subquery() {
        // 比值形态：外层没有 FROM，两侧各一个标量子查询，口径列都写在子查询的 WHERE 里
        let ratio = "SELECT ROUND((SELECT SUM(amt) FROM tbl_dtl \
                     WHERE type_flag = '1' AND del_flag = 0) * 100.0 / \
                     (SELECT SUM(amt) FROM tbl_b WHERE del_flag = 0), 2) AS r";
        assert_eq!(check_caliber(ratio, &[cols()]), vec![], "{ratio}");
        // 真漏一列仍要判红（别把守卫一起放宽了）
        let missing = "SELECT (SELECT SUM(amt) FROM tbl_dtl WHERE type_flag = '1') AS r";
        assert_eq!(check_caliber(missing, &[cols()]).len(), 1);
        // 子查询在 WHERE 里（IN 形态）同样算
        let in_form = "SELECT 1 FROM tbl_b WHERE id IN \
                       (SELECT id FROM tbl_dtl WHERE type_flag = '1' AND del_flag = 0)";
        assert_eq!(check_caliber(in_form, &[cols()]), vec![], "{in_form}");
    }
    fn dedup() -> CaliberRule {
        CaliberRule::RequireDedup {
            table: "tbl_dtl".into(),
            keys: vec!["a".into(), "b".into()],
            human: "明细表有重复行".into(),
        }
    }
    fn latest() -> CaliberRule {
        CaliberRule::RequireLatest {
            table: "tbl_snap".into(),
            partition: vec!["owner_code".into()],
            human: "快照表取最新一条".into(),
        }
    }
    fn pct() -> CaliberRule {
        CaliberRule::RequirePercentScale { metric: "ratio".into(), human: "占比要百分数".into() }
    }
    fn joinf() -> CaliberRule {
        CaliberRule::RequireJoinAndFilter {
            table: "tbl_cat".into(),
            col: "cat_name".into(),
            human: "该专名是这一列的取值".into(),
        }
    }
    fn code() -> CaliberRule {
        CaliberRule::RequireCodeOnColumn {
            table: "tbl_a".into(),
            col: "col_x".into(),
            code: "430000".into(),
            human: "该码是这一列的取值".into(),
        }
    }
    fn rules_of(sql: &str, r: &[CaliberRule]) -> Vec<String> {
        check_caliber(sql, r).into_iter().map(|v| v.rule).collect()
    }

    /// 只采事实、不判规则：给「事实采集本身」的判据用（方言与 `check_caliber` 保持同一个）。
    fn facts(sql: &str) -> Facts {
        let mut f = Facts::default();
        for s in &Parser::parse_sql(&GenericDialect {}, sql).expect("测试 SQL 必须可解析") {
            if let Statement::Query(q) = s {
                scan_query(q, &mut f, false);
            }
        }
        f
    }

    #[test]
    fn empty_rules_and_parse_error_are_silent() {
        assert!(check_caliber("SELECT 1", &[]).is_empty());
        assert!(check_caliber("SELEKT FROM WHERE (", &[cols(), dedup(), latest(), pct()]).is_empty());
    }

    #[test]
    fn undeclared_table_absent_is_never_judged() {
        // 声明的表压根没出现 → 一律不判（声明缺失 ≠ 违规）
        let sql = "SELECT COUNT(*) FROM tbl_other o WHERE o.dt = '2026-06'";
        assert!(rules_of(sql, &[cols(), dedup(), latest()]).is_empty());
    }

    #[test]
    fn require_cols_flags_unconstrained_scope() {
        let sql = "SELECT COUNT(DISTINCT d.gid) FROM tbl_dtl d \
                   JOIN tbl_ord o ON o.id = d.oid WHERE o.dt >= '2026-06-01'";
        let v = check_caliber(sql, &[cols()]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "require_cols:tbl_dtl");
        assert!(v[0].hint.contains("type_flag") && v[0].hint.contains("del_flag"));
        assert_eq!(v[0].human, "明细表标准口径");
    }

    #[test]
    fn require_cols_accepts_on_clause_and_ignores_the_value() {
        // 防误伤①：ON 里的约束算约束；且只看列被没被约束，不比对值（'2' 是合法问法）
        let sql = "SELECT SUM(d.qty) FROM tbl_ord o \
                   JOIN tbl_dtl d ON d.oid = o.id AND d.type_flag = '2' AND d.del_flag = 0 \
                   WHERE o.dt >= '2026-06-01'";
        assert!(rules_of(sql, &[cols()]).is_empty());
    }

    #[test]
    fn require_cols_accepts_bare_column_in_single_table_query() {
        let sql = "SELECT COUNT(*) FROM tbl_dtl WHERE type_flag = '1' AND del_flag = 0";
        assert!(rules_of(sql, &[cols()]).is_empty());
    }


    #[test]
    fn require_cols_reports_only_the_missing_one() {
        let sql = "SELECT COUNT(*) FROM tbl_dtl d WHERE d.type_flag = '1'";
        let v = check_caliber(sql, &[cols()]);
        assert_eq!(v.len(), 1);
        assert!(v[0].hint.contains("del_flag") && !v[0].hint.contains("type_flag"));
        // 反引号标识符必须解析得动：解析失败＝静默漏判，这条断言用「该红」形态才守得住
        let quoted = "SELECT COUNT(*) FROM `tbl_dtl` AS d WHERE d.`type_flag` = '1'";
        assert_eq!(rules_of(quoted, &[cols()]), vec!["require_cols:tbl_dtl"]);
    }

    #[test]
    fn require_dedup_flags_plain_aggregate() {
        let sql = "SELECT SUM(d.qty) FROM tbl_dtl d WHERE d.type_flag = '1' AND d.del_flag = 0";
        assert_eq!(rules_of(sql, &[dedup()]), vec!["require_dedup:tbl_dtl"]);
    }

    #[test]
    fn require_dedup_not_triggered_by_count_distinct() {
        // 防误伤②：COUNT(DISTINCT …) 本身即去重
        let sql = "SELECT COUNT(DISTINCT d.gid) FROM tbl_dtl d";
        assert!(rules_of(sql, &[dedup()]).is_empty());
    }

    #[test]
    fn require_dedup_accepts_distinct_subquery() {
        // 聚合作用在派生表别名上：本表没被直接聚合，不判
        let plain = "SELECT SUM(x.qty) FROM (SELECT DISTINCT a, b, qty FROM tbl_dtl) x";
        assert!(rules_of(plain, &[dedup()]).is_empty());
        // 内外同名（LLM 常这么写）：别名遮蔽会让上一条判据失效，靠 distinct 子查询标志兜住
        let shadow = "SELECT SUM(d.qty) FROM (SELECT DISTINCT d.a, d.b, d.qty FROM tbl_dtl d) d";
        assert!(rules_of(shadow, &[dedup()]).is_empty());
        // 顶层 DISTINCT 去的是输出行、不是输入行，不算已去重
        let top = "SELECT DISTINCT d.gid, SUM(d.qty) FROM tbl_dtl d GROUP BY d.gid";
        assert_eq!(rules_of(top, &[dedup()]), vec!["require_dedup:tbl_dtl"]);
    }

    /// 🔴 **有 DISTINCT 不等于去重对了**：键少一个就把只在该列上不同的行折掉 → 少算。
    ///
    /// 实测：订单明细指标声明 5 个去重键，LLM 只写了 4 个（漏掉金额列），订单明细数量少算、
    /// 排行榜第 3 名换了人 —— 而当时判据只问「有没有 DISTINCT」，全绿放过。
    #[test]
    fn require_dedup_checks_the_key_set_not_just_presence() {
        // 键齐（多给一列无害：更细的粒度只会少折，不会多折）
        let ok = "SELECT SUM(x.qty) FROM (SELECT DISTINCT d.a, d.b, d.qty FROM tbl_dtl d) x \
                  JOIN tbl_dtl d ON d.a = x.a";
        assert!(rules_of(ok, &[dedup()]).is_empty(), "{ok}");
        // 缺键 `b` → 判红，且 hint 必须点名缺的是哪个
        let miss = "SELECT SUM(d.qty) FROM (SELECT DISTINCT d.a, d.qty FROM tbl_dtl d) d";
        let v = check_caliber(miss, &[dedup()]);
        assert_eq!(v.len(), 1, "{miss}");
        assert_eq!(v[0].rule, "require_dedup:tbl_dtl");
        assert!(v[0].hint.contains("缺 b"), "{:?}", v[0]);
        assert!(v[0].hint.contains("少算"), "{:?}", v[0]);
        // 取**列**不取别名：`d.b AS bb` 的去重语义由 b 决定
        let aliased = "SELECT SUM(d.qty) FROM (SELECT DISTINCT d.a, d.b AS bb, d.qty FROM tbl_dtl d) d";
        assert!(rules_of(aliased, &[dedup()]).is_empty(), "{aliased}");
    }

    /// 🔴 **CTE / 派生表的别名必须转发到内部表**，否则判据看不见 LLM 实际写的形状。
    ///
    /// 上面那条测试用的是 `) d`（外层别名恰好与内层别名同名）与 `) x JOIN tbl_dtl d`
    /// （另有一个真的 `d`）—— 两种都**碰巧**让 `aliases_of("tbl_dtl")` 命中聚合前缀。
    /// 实测 LLM 写的是 `WITH dedup AS (…) … SUM(dedup.qty)` 与 `FROM (…) t … SUM(t.qty)`，
    /// 这两种下 `aliases_of` 只有 `d`，前置条件不成立 → **整条规则一次都不触发**，
    /// 于是「声明 5 键、SQL 只写 3 键」被全绿放过。这条测试就是那个漏洞的形状。
    #[test]
    fn dedup_rule_sees_through_cte_and_derived_aliases() {
        // ① CTE：缺键 `b`，聚合前缀是 CTE 名
        let cte = "WITH dedup AS (SELECT DISTINCT d.a, d.qty FROM tbl_dtl d) \
                   SELECT SUM(dedup.qty) FROM dedup";
        let v = check_caliber(cte, &[dedup()]);
        assert_eq!(v.len(), 1, "CTE 形状漏判了：{cte}");
        assert!(v[0].hint.contains("缺 b"), "{:?}", v[0]);
        // ② 派生表用**与内层无关**的别名
        let derived = "SELECT SUM(t.qty) FROM (SELECT DISTINCT d.a, d.qty FROM tbl_dtl d) t";
        let v = check_caliber(derived, &[dedup()]);
        assert_eq!(v.len(), 1, "派生表形状漏判了：{derived}");
        // ③ 键齐时两种形状都不许误报
        for ok in [
            "WITH dd AS (SELECT DISTINCT d.a, d.b, d.qty FROM tbl_dtl d) SELECT SUM(dd.qty) FROM dd",
            "SELECT SUM(t.qty) FROM (SELECT DISTINCT d.a, d.b, d.qty FROM tbl_dtl d) t",
        ] {
            assert!(rules_of(ok, &[dedup()]).is_empty(), "{ok}");
        }
    }

    /// 🔴 **聚合列完全没有前缀**的那一档 —— 上面那条只覆盖了 `SUM(dedup.qty)`（带 CTE 名前缀）。
    ///
    /// 订单明细数量排行漏判示例（低报 **5.6 倍**）：
    /// ```sql
    /// WITH dedup AS (SELECT DISTINCT d.sku_name, d.box_quantity FROM t_sales_order_detail d …)
    /// SELECT sku_name AS 商品名称, SUM(box_quantity) AS 订单明细数量 FROM dedup GROUP BY sku_name …
    /// ```
    /// 声明五个去重键、SQL 只 DISTINCT 两个 ⇒ 同一商品在不同订单里箱数相同的行被折成一行 ⇒
    /// 首行 13045 而 gold 72863。而 `caliber_note` 里去重这条**一个字都没有**：
    /// 前置条件 `al.contains(p)` 拿 `p = ""` 去比 `{d}`，恒假 ⇒ 整条判据弃权。
    #[test]
    fn dedup_rule_sees_unprefixed_aggregate_after_cte() {
        // ① SALE15 的真实骨架：CTE 里只 DISTINCT 两键，外层 `SUM(qty)` **无前缀**
        let sql = "WITH dedup AS (SELECT DISTINCT d.a, d.qty FROM tbl_dtl d) \
                   SELECT a AS `名`, SUM(qty) AS `量` FROM dedup GROUP BY a ORDER BY `量` DESC LIMIT 10";
        let v = check_caliber(sql, &[dedup()]);
        assert_eq!(v.len(), 1, "无前缀聚合漏判了（SALE15 的原形）：{sql}");
        assert!(v[0].hint.contains("缺 b"), "{:?}", v[0]);
        // ② 派生表 + 无前缀聚合，同样要判
        let derived = "SELECT SUM(qty) FROM (SELECT DISTINCT d.a, d.qty FROM tbl_dtl d) t";
        assert_eq!(check_caliber(derived, &[dedup()]).len(), 1, "{derived}");
        // ③ 键齐时不许误报（扩展前置条件最容易带出来的假红）
        let ok = "WITH dd AS (SELECT DISTINCT d.a, d.b, d.qty FROM tbl_dtl d) \
                  SELECT a, SUM(qty) FROM dd GROUP BY a";
        assert!(rules_of(ok, &[dedup()]).is_empty(), "{ok}");
        // ④ 收窄那一半：**这张表压根没出现**时不许开火（那一档归 `RequireCols` 管）。
        //    第一版的 ④ 写的是「无前缀列不在 keys 里就不判」，而那条收窄本身是错的 ——
        //    被聚合的度量列一般**不在**去重键里（`dedup()` 的 keys 是 `[a,b]`、聚合 `qty`），
        //    按那条收窄 SALE15 的原形照样漏判。现在的收窄是「表出现 + 有 DISTINCT 子查询」。
        let elsewhere = "WITH dd AS (SELECT DISTINCT x.a FROM other_tbl x) SELECT SUM(qty) FROM dd";
        assert!(
            rules_of(elsewhere, &[dedup()]).is_empty(),
            "tbl_dtl 压根没出现却开火了（那会在每条带 DISTINCT 的 SQL 上假红）：{elsewhere}"
        );
        // ⑤ 没有任何 DISTINCT 且聚合列不在 keys 里 → 不判（两条收窄同时不成立）
        let plain = "SELECT SUM(qty) FROM tbl_dtl";
        assert!(rules_of(plain, &[dedup()]).is_empty(), "两条收窄都不成立却开火了：{plain}");
    }

    #[test]
    fn require_dedup_silent_without_aggregate_on_that_table() {
        let sql = "SELECT d.gid, d.qty FROM tbl_dtl d LIMIT 10";
        assert!(rules_of(sql, &[dedup()]).is_empty());
    }

    #[test]
    fn require_latest_flags_naked_snapshot_scan() {
        let sql = "SELECT s.owner_code, s.amt FROM tbl_snap s ORDER BY s.amt DESC LIMIT 10";
        let v = check_caliber(sql, &[latest()]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "require_latest:tbl_snap");
        assert!(v[0].hint.contains("owner_code"));
    }

    #[test]
    fn require_latest_accepts_row_number_form() {
        let sql = "WITH r AS (SELECT s.owner_code, s.amt, \
                   ROW_NUMBER() OVER (PARTITION BY s.owner_code ORDER BY s.ct DESC) AS rn \
                   FROM tbl_snap s) SELECT owner_code, amt FROM r WHERE rn = 1";
        assert!(rules_of(sql, &[latest()]).is_empty());
    }

    #[test]
    fn require_latest_accepts_max_subquery_form() {
        let sql = "SELECT s.owner_code, s.amt FROM tbl_snap s WHERE s.ct = \
                   (SELECT MAX(z.ct) FROM tbl_snap z WHERE z.owner_code = s.owner_code)";
        assert!(rules_of(sql, &[latest()]).is_empty());
    }


    #[test]
    fn require_percent_scale_flags_bare_division() {
        let sql = "SELECT SUM(a.x) / SUM(a.y) AS ratio FROM tbl_a a";
        assert_eq!(rules_of(sql, &[pct()]), vec!["require_percent_scale:ratio"]);
    }

    #[test]
    fn require_percent_scale_accepts_hundred_factor() {
        let sql = "SELECT ROUND(SUM(a.x) * 100.0 / SUM(a.y), 2) AS ratio FROM tbl_a a";
        assert!(rules_of(sql, &[pct()]).is_empty());
        // 投影里没有除法也不判
        assert!(rules_of("SELECT SUM(a.x) FROM tbl_a a", &[pct()]).is_empty());
        // 条件里的除法是阈值，不是输出单位
        assert!(rules_of("SELECT a.x FROM tbl_a a WHERE a.x / a.y > 1", &[pct()]).is_empty());
    }

    #[test]
    fn require_join_and_filter_demands_the_table_be_present() {
        // ① 本变体存在的理由：表整个缺席也算违规（拿另一列的相近取值顶替，RequireCols 会静默放过）
        let absent = "SELECT SUM(d.qty) FROM tbl_dtl d WHERE d.item_name LIKE '%x%'";
        let v = check_caliber(absent, &[joinf()]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "require_join_and_filter:tbl_cat.cat_name");
        assert!(v[0].hint.contains("tbl_cat.cat_name"));
        assert_eq!(v[0].human, "该专名是这一列的取值");
        // ② 表 JOIN 进来了、但那一列没被约束 → 仍是违规
        let unfiltered = "SELECT SUM(d.qty) FROM tbl_dtl d JOIN tbl_cat c ON c.id = d.cid";
        assert_eq!(rules_of(unfiltered, &[joinf()]), vec!["require_join_and_filter:tbl_cat.cat_name"]);
        // ③ 列在 WHERE 里被约束 → 通过（不比对值，LIKE 与 = 同等）
        let in_where = "SELECT SUM(d.qty) FROM tbl_dtl d JOIN tbl_cat c ON c.id = d.cid \
                        WHERE c.cat_name LIKE '%x%'";
        assert!(rules_of(in_where, &[joinf()]).is_empty());
        // ④ 列在 JOIN ON 里被约束 → 通过
        let in_on = "SELECT SUM(d.qty) FROM tbl_dtl d \
                     JOIN tbl_cat c ON c.id = d.cid AND c.cat_name = 'x'";
        assert!(rules_of(in_on, &[joinf()]).is_empty());
    }

    /// 🔴 两条互相矛盾的时间列声明 → **两条都不判**（AS03 的真根因，见 二·N）。
    ///
    /// 这条测试守的不是「判得更全」，是「**判据不许把对的答案改错**」：
    /// 冲突时若照判，判词会命令模型把 A 换成 B，另一条随即判红、预算用尽 → 返回错值。
    /// 实测就是这么把 2990 变成 2779 的。
    #[test]
    fn conflicting_time_columns_disable_both() {
        let t = |c: &str| CaliberRule::RequireTimeColumn { col: c.into(), human: "h".into() };
        // 用了 after_sales_time 的正确 SQL：单独判 order_time 会判红（这就是伤害的来源）
        let sql = "SELECT COUNT(1) FROM t_as WHERE after_sales_time >= '2026-01-01'";
        assert_eq!(rules_of(sql, &[t("order_time")]), ["require_time_column:order_time"]);
        // 两条冲突同在 → 一条都不判
        assert!(
            rules_of(sql, &[t("order_time"), t("after_sales_time")]).is_empty(),
            "冲突时必须两条都不判"
        );
        // 冲突只让**时间列**那几条闭嘴，别的规则照判
        let both = [t("order_time"), t("after_sales_time"), cols()];
        let v = rules_of("SELECT SUM(d.qty) FROM tbl_dtl d", &both);
        assert_eq!(v, ["require_cols:tbl_dtl"], "只该哑掉时间列那几条");
        // 同一列重复声明（两个指标同口径）**不算冲突** —— 照判
        assert_eq!(
            rules_of(sql, &[t("order_time"), t("order_time")]),
            ["require_time_column:order_time", "require_time_column:order_time"]
        );
    }

    /// `time_ish_conds` 只服务判词措辞，但它判宽了会往判词里塞无关列名、判窄了会退回泛泛措辞
    /// —— 两个方向都影响回炉能不能修对，所以边界要钉住。
    #[test]
    fn time_ish_conds_picks_time_columns_only() {
        let sql = "SELECT SUM(t.qty) FROM tbl_dtl t WHERE t.ship_time >= '2026-01-01' \
                   AND t.deleted_flag = 0 AND t.biz_date < '2026-07-01' AND t.sku_code = 'A' \
                   AND t.created_at > '2026-01-01'";
        let f = facts(sql);
        // time / date / _at 三种词法特征都要认；非时间列一个都不许进
        assert_eq!(f.time_ish_conds("ord_time"), ["biz_date", "created_at", "ship_time"]);
        // 排除项（声明的那一列）不出现在里面 —— 否则判词会变成「把 X 换成 X」
        let g = facts("SELECT 1 FROM t WHERE ord_time > '2026-01-01' AND ship_time > '2026-01-01'");
        assert_eq!(g.time_ish_conds("ord_time"), ["ship_time"]);
        // 没有任何时间列被约束 → 空（判词退回泛泛那一支）
        let h = facts("SELECT 1 FROM t WHERE deleted_flag = 0");
        assert!(h.time_ish_conds("ord_time").is_empty());
    }

    /// 🔴 声明的时间列：注册表把「该指标按哪个时间点算」钉死了，此前那条声明**只进 prompt、
    /// 没有判据**。实测：问「上半年每月订单明细数量」时用了明细表自己的另一个时间列，
    /// 于是既没按下单时点分月、也顺带丢掉了主表上的有效状态过滤，虚高 26%。
    #[test]
    fn require_time_column_flags_the_wrong_time_field() {
        let rule = CaliberRule::RequireTimeColumn {
            col: "ord_time".into(),
            human: "该指标按下单时点算".into(),
        };
        // ① 用了另一个时间列（而且没 JOIN 主表）→ 违规。这正是它存在的理由：
        //    `RequireCols` 遇「表整个缺席」按宁缺毋滥不判，于是那条错 SQL 一路绿灯。
        let wrong = "SELECT DATE_FORMAT(t.ship_time,'%Y-%m') m, SUM(t.qty) FROM tbl_dtl t \
                     WHERE t.ship_time >= '2026-01-01' GROUP BY m";
        let v = check_caliber(wrong, &[rule.clone()]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "require_time_column:ord_time");
        assert!(v[0].hint.contains("ord_time"), "{:?}", v[0]);
        assert_eq!(v[0].human, "该指标按下单时点算");
        // 🔴 判词必须**点名用错的那一列**。实测（AS03）：只说「必须用 after_sales_time」时，
        // 回炉一轮后模型原封不动仍用 `order_time` —— 它不知道自己写的那个是「别的时间列」。
        // 判词里出现「把 X 换成 Y」这种可执行指令才是模型照得动的形态。
        //
        // ⚠️ 用错的那一列可能出现在**两处**，判词要分别点名：
        //   · 条件里用错（AS03 的原始形状）：判词只说「时间过滤用错了列 ship_time」；
        //   · 分桶用错（AS01 的形状，F3 补的那一问）：判词说「分桶用了别的时间列 ship_time」。
        // 这个用例的 SQL 两处都有 `ship_time`（WHERE 里过滤 + 投影里分桶），
        // 第二问（F3 新增的）先开火，所以判词是「分桶」那一版。
        assert!(v[0].hint.contains("ship_time"), "判词没点名用错的列：{:?}", v[0]);
        assert!(
            v[0].hint.contains("分桶用了别的时间列") || v[0].hint.contains("时间过滤用错了列"),
            "判词要说明是哪一处用错了：{:?}",
            v[0]
        );
        // 🔴 判词必须**同时给跨表那一支**，且明确禁止盲目改名。
        // 实测两种形态都真实存在：AS03 是同表（改名即对）；GOODS13 是跨表 ——
        // 明细表只有 `delivery_time`，声明的 `order_time` 在订单头上，
        // 就地改名会得到一个不存在的列（1054）。只写「整段改成」就是给错建议。
        assert!(v[0].hint.contains("JOIN"), "跨表那一支缺了：{:?}", v[0]);
        assert!(v[0].hint.contains("不要把 ship_time 就地改名"), "{:?}", v[0]);
        assert!(v[0].hint.contains("口径过滤"), "跨表时必须提醒连带补那张表的口径：{:?}", v[0]);
        // ② 声明的列被约束（限定形态）→ 通过
        let ok_q = "SELECT SUM(d.qty) FROM tbl_dtl d JOIN tbl_ord o ON o.id = d.oid \
                    WHERE o.ord_time >= '2026-01-01' AND o.ord_time < '2026-07-01'";
        assert!(rules_of(ok_q, &[rule.clone()]).is_empty());
        // ③ 裸列形态也算（单表查询的常见写法）
        let bare = "SELECT SUM(qty) FROM tbl_ord WHERE ord_time >= '2026-01-01'";
        assert!(rules_of(bare, &[rule.clone()]).is_empty());
        // ④ 在 JOIN ON 里约束也算
        let on = "SELECT SUM(d.qty) FROM tbl_dtl d \
                  JOIN tbl_ord o ON o.id = d.oid AND o.ord_time >= '2026-01-01'";
        assert!(rules_of(on, &[rule]).is_empty());
    }

    /// 🔴「取对了码、用错了列」：码是对的、列是错的 —— 此前五条判据一条都管不到它。
    #[test]
    fn require_code_on_column_flags_the_code_used_elsewhere() {
        // ① 码用在声明列上 → 通过（限定/裸列/IN/LIKE 四种写法都算）
        for ok in [
            "SELECT SUM(d.amt) FROM tbl_dtl d JOIN tbl_a a ON a.id = d.aid WHERE a.col_x = '430000'",
            "SELECT COUNT(*) FROM tbl_a WHERE col_x = '430000'",
            "SELECT COUNT(*) FROM tbl_a WHERE col_x IN ('430000', '420000')",
            "SELECT COUNT(*) FROM tbl_a WHERE col_x LIKE '%430000%'",
            "SELECT COUNT(*) FROM `tbl_a` a WHERE a.`col_x` = '430000'",
        ] {
            assert!(rules_of(ok, &[code()]).is_empty(), "{ok}");
        }
        // ② 码用在别的列上 → 违规，且 hint 指名两边
        let wrong = "SELECT SUM(o.amt) FROM tbl_ord o WHERE o.col_y = '430000'";
        let v = check_caliber(wrong, &[code()]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "require_code_on_column:tbl_a.col_x");
        assert!(v[0].hint.contains("col_y") && v[0].hint.contains("tbl_a.col_x"), "{:?}", v[0]);
        assert_eq!(v[0].human, "该码是这一列的取值");
        // 不带引号的数字字面量同算（LLM 两种都写）
        assert_eq!(rules_of("SELECT 1 FROM tbl_ord o WHERE o.col_y = 430000", &[code()]).len(), 1);
        // ③ 码压根没出现 → 不判：JOIN 字典表按名过滤是另一种正确写法，判它就是误伤
        let by_name = "SELECT SUM(o.amt) FROM tbl_ord o JOIN tbl_r r ON r.rc = o.col_y \
                       WHERE r.rname LIKE '%zz%'";
        assert!(rules_of(by_name, &[code()]).is_empty());
        // ④ 声明列上也写了（哪怕别的列上也写了）→ 不判，偏漏判一侧
        let both = "SELECT 1 FROM tbl_a a JOIN tbl_ord o ON o.aid = a.id \
                    WHERE a.col_x = '430000' AND o.col_y = '430000'";
        assert!(rules_of(both, &[code()]).is_empty());
        // ⑤ 不在条件里的同名字面量不算（投影/LIMIT 都不是过滤），阈值比较也不算
        assert!(rules_of("SELECT '430000' AS c FROM tbl_ord LIMIT 430000", &[code()]).is_empty());
        assert!(rules_of("SELECT 1 FROM tbl_ord o WHERE o.amt > 430000", &[code()]).is_empty());
    }

    fn known() -> CaliberRule {
        CaliberRule::RequireKnownValue {
            table: "tbl_a".into(),
            col: "cls_code".into(),
            values: [("甲级", "01"), ("乙级", "04"), ("丙级", "06")]
                .iter()
                .map(|(n, c)| (n.to_string(), c.to_string()))
                .collect(),
            human: "这一列是完整码表".into(),
        }
    }

    /// 🔴 值不在码表 → SQL 合法 → 三段闸门放行 → 执行成功 → **返 0 行**：
    /// 本仓最阴的一族静默错答，此前六条判据一条都管不到它（它们只看列有没有被约束、不看值）。
    #[test]
    fn require_known_value_flags_the_value_that_returns_zero_rows() {
        // 🔴 防恒真前置：`check_caliber` 解析失败会**返空**（漏判方向），那样下面每一条
        // 「该红」的断言都会因为「看不懂」而绿。本仓已多次踩「入参变空 → 断言恒真」。
        let bad = "SELECT COUNT(*) FROM tbl_a a WHERE a.cls_code = '丁级'";
        assert!(output_shape(bad).is_some(), "解析不动 → 判据恒返空");
        let v = check_caliber(bad, &[known()]);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].rule, "require_known_value:tbl_a.cls_code");
        assert_eq!(v[0].human, "这一列是完整码表");
        assert!(v[0].hint.contains("丁级"), "没点名那个不存在的值：{:?}", v[0]);
        // 🔴 判词必须带**名=码**的合法取值。只说「这个值不存在」时模型只会换一个同样不存在的
        // 近义词（`RequireTimeColumn` 已经吃过一次「判词不点名 → 回炉一轮原封不动」的账）。
        assert!(v[0].hint.contains("甲级=01") && v[0].hint.contains("丙级=06"), "{:?}", v[0]);
        assert!(v[0].hint.contains("0 行"), "{:?}", v[0]);
        // 同一列多个非法值：一条违规报清全部，且**顺序稳定**（HashSet 序不定，判词有 golden 对比）
        let two = "SELECT COUNT(*) FROM tbl_a a WHERE a.cls_code IN ('乙级', '丁级', '戊级')";
        let v2 = check_caliber(two, &[known()]);
        assert_eq!(v2.len(), 1, "{v2:?}");
        assert!(v2[0].hint.contains("丁级, 戊级"), "{:?}", v2[0]);
        // 一条都不许判的六种。每一种都是一次假红，而误伤一条会连带把对的答案回炉改错（二·G）
        for ok in [
            // ① 登记过的名（确定性换码器会把它换成码）；裸列形态同算
            "SELECT COUNT(*) FROM tbl_a a WHERE a.cls_code = '乙级'",
            "SELECT COUNT(*) FROM tbl_a WHERE cls_code = '甲级'",
            // ② 登记过的码，本来就是最正确的写法
            "SELECT COUNT(*) FROM tbl_a a WHERE a.cls_code = '04'",
            // ③ 未登记但**是 ASCII**：自动对码只要求覆盖 ≥80%，未覆盖的真码写出来是对的
            "SELECT COUNT(*) FROM tbl_a a WHERE a.cls_code = '99'",
            // ④ 声明的表不在 FROM/JOIN 里（防误伤原则①）：同名列在别的表上可能不是码列
            "SELECT COUNT(*) FROM tbl_b b WHERE b.cls_code = '丁级'",
            // ⑤ 不在条件里的字面量不是过滤
            "SELECT '丁级' AS c FROM tbl_a a",
            // ⑥ **非等值家族**：判词说的是「这个值匹配不到行，请换成合法取值之一」，
            //    而这句话对 `!=` / `NOT IN` 是**反的** —— 那三种今天等于不过滤，
            //    照判词去改会把「不排除」变成「排除掉一个真实类别」，
            //    即判据自己指令了一次语义改写（比原来的偏差更难发现）。
            //    `LIKE` 同样不该判：模糊匹配不要求等于码表某一项。
            "SELECT COUNT(*) FROM tbl_a a WHERE a.cls_code != '丁级'",
            "SELECT COUNT(*) FROM tbl_a a WHERE a.cls_code NOT IN ('丁级','戊级')",
            "SELECT COUNT(*) FROM tbl_a a WHERE a.cls_code LIKE '%丁级%'",
        ] {
            assert!(rules_of(ok, &[known()]).is_empty(), "{ok}");
        }
        // ⑥ 的反面（防恒真）：把这三句的运算符换成等值家族**必须**判 ——
        // 没有这一条时，把 `known_value` 写成「一律不判」上面整个循环也全绿。
        for must in [
            "SELECT COUNT(*) FROM tbl_a a WHERE a.cls_code = '丁级'",
            "SELECT COUNT(*) FROM tbl_a a WHERE a.cls_code IN ('丁级','戊级')",
        ] {
            assert_eq!(rules_of(must, &[known()]).len(), 1, "{must}");
        }
        // ⑥ 取值集为空 → 不判：空集会把该列上**每一个**名字型的值判红（最贵的一种误伤）
        let empty = CaliberRule::RequireKnownValue {
            table: "tbl_a".into(),
            col: "cls_code".into(),
            values: vec![],
            human: "空集".into(),
        };
        assert!(rules_of(bad, &[empty]).is_empty());
        // ⑦ 大字典：判词截断到前 N 个并说清共几个（登记的是字典全码，整本抄进回炉指令是浪费）
        let many = CaliberRule::RequireKnownValue {
            table: "tbl_a".into(),
            col: "cls_code".into(),
            values: (0..LEGAL_VALUES_IN_HINT + 5)
                .map(|i| (format!("第{i}类"), format!("{i:03}")))
                .collect(),
            human: "大字典".into(),
        };
        let vm = check_caliber(bad, &[many]);
        assert_eq!(vm.len(), 1, "{vm:?}");
        let n = LEGAL_VALUES_IN_HINT + 5;
        assert!(vm[0].hint.contains(&format!("共 {n} 个取值")), "{:?}", vm[0]);
        assert!(
            !vm[0].hint.contains(&format!("第{LEGAL_VALUES_IN_HINT}类")),
            "超过上限的取值不许进判词：{:?}",
            vm[0]
        );
    }

    // ---------- `NoFanoutJoin`（FIN01：为查名字 JOIN 进重复键，SUM 被放大 299 倍） ----------

    fn fanout() -> CaliberRule {
        CaliberRule::NoFanoutJoin {
            // (表, 列)：构造侧从 join_edge 的 card 推出，kernel 一个真表名都不认识
            keys: vec![("tbl_ord".into(), "cust_id".into()), ("tbl_dtl".into(), "oid".into())],
            human: "多侧键会复制行".into(),
        }
    }

    /// FIN01 原形：为取客户名 LEFT JOIN 订单表（一个客户 N 单），SUM 被放大。
    /// 真表名版本（含 DMS 那条 SQL 逐字形）在 `kernel/tests/sql_guard.rs` —— 本文件不进 DMS 语料。
    #[test]
    fn fanout_join_flags_lookup_join_into_dup_key() {
        let bad = "SELECT o2.cust_name, SUM(i.amt) AS total FROM tbl_inv i \
                   LEFT JOIN tbl_ord o2 ON i.cust_id = o2.cust_id GROUP BY o2.cust_name";
        let v = check_caliber(bad, &[fanout()]);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].rule, "no_fanout_join:tbl_ord.cust_id");
        assert!(v[0].hint.contains("EXISTS") && v[0].hint.contains("主档"), "{:?}", v[0].hint);
    }

    /// 三个**不判**：基表侧重复键是正常方向；COUNT 族数行，扇出恰是本意；聚的都是被 JOIN 侧自己。
    #[test]
    fn fanout_join_passes_the_three_legit_shapes() {
        // ① 重复键在基表侧（FROM tbl_ord JOIN tbl_cus）—— 每天的正确写法
        let ok1 = "SELECT c.cust_name, SUM(o.amt) FROM tbl_ord o \
                   JOIN tbl_cus c ON o.cust_id = c.cust_id GROUP BY c.cust_name";
        assert!(check_caliber(ok1, &[fanout()]).is_empty(), "基表侧重复键不许判");
        // ② 只有 COUNT —— 数行，扇出即本意（每个客户的订单数）
        let ok2 = "SELECT c.cust_name, COUNT(*) FROM tbl_cus c \
                   JOIN tbl_ord o ON c.cust_id = o.cust_id GROUP BY c.cust_name";
        assert!(check_caliber(ok2, &[fanout()]).is_empty(), "COUNT 族不许判");
        // ③ 度量前缀全落在被 JOIN 侧（SUM(o.amt) 聚的就是订单表）
        let ok3 = "SELECT c.cust_name, SUM(o.amt) FROM tbl_cus c \
                   JOIN tbl_ord o ON c.cust_id = o.cust_id GROUP BY c.cust_name";
        assert!(check_caliber(ok3, &[fanout()]).is_empty(), "聚被 JOIN 侧自己的列不许判");
    }

    /// 两边都聚：被 JOIN 侧参与了，但另一边的度量仍被放大 —— 必须判。
    #[test]
    fn fanout_join_flags_mixed_measures() {
        let bad = "SELECT SUM(i.amt), SUM(o2.fee) FROM tbl_inv i JOIN tbl_ord o2 \
                   ON i.cust_id = o2.cust_id";
        assert_eq!(rules_of(bad, &[fanout()]), ["no_fanout_join:tbl_ord.cust_id"]);
    }

    /// FIN01 的真实形态：JOIN 藏在 UNION ALL 的两个分支里，SUM 在最外层且是裸列。
    /// 跨层拍平的事实集必须照样判得到。判词一条（`judge` 每条规则至多产一条），
    /// 点名第一个命中的重复键 —— 回炉指令要的是「什么形态错了」，不是逐分支点名。
    #[test]
    fn fanout_join_fires_through_union_and_derived_table() {
        let bad = "SELECT cust, SUM(amt) FROM (\
                     SELECT o2.cust_name AS cust, i.amt FROM tbl_inv i \
                     LEFT JOIN tbl_ord o2 ON i.cust_id = o2.cust_id \
                     UNION ALL \
                     SELECT o3.cust_name AS cust, n.amt FROM tbl_inv_new n \
                     LEFT JOIN tbl_ord o3 ON n.cust_id = o3.cust_id) t \
                   GROUP BY cust ORDER BY SUM(amt) DESC LIMIT 10";
        let v = check_caliber(bad, &[fanout()]);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].rule, "no_fanout_join:tbl_ord.cust_id");
    }

    /// 键清单为空 / 没写到 ON 里（RIGHT JOIN 与 USING 都不收）→ 不判（漏判方向）。
    #[test]
    fn fanout_join_silent_when_no_keys_or_no_on_eq() {
        let sql = "SELECT SUM(i.amt) FROM tbl_inv i LEFT JOIN tbl_ord o2 ON i.cust_id = o2.cust_id";
        assert!(check_caliber(sql, &[]).is_empty());
        let empty_keys = CaliberRule::NoFanoutJoin { keys: vec![], human: "h".into() };
        assert!(check_caliber(sql, &[empty_keys]).is_empty());
        let right = "SELECT SUM(o2.amt) FROM tbl_ord o2 RIGHT JOIN tbl_inv i \
                     ON i.cust_id = o2.cust_id";
        assert!(check_caliber(right, &[fanout()]).is_empty(), "RIGHT 不收，漏判方向");
        // 事实层自证：join_eqs 与 measure_aggs 真的采到了（防空转断言）
        let f = facts(sql);
        assert_eq!(f.join_eqs.len(), 1, "{:?}", f.join_eqs);
        assert_eq!(f.join_eqs[0].0, "o2");
        assert!(f.measure_aggs.contains("i"), "SUM(i.amt) 的前缀：{:?}", f.measure_aggs);
    }

    // ---------- `RequireTimeCap`（中性延迟确认指标；默认 DWS 销售不使用此规则） ----------

    fn cap_rule() -> CaliberRule {
        CaliberRule::RequireTimeCap { col: "confirmed_at".into(), human: "确认到昨天".into() }
    }

    #[test]
    fn time_cap_flags_period_end_upper_bound() {
        // 实测形状：月窗（含今天）→ 红，且 hint 给出唯一认的写法
        let bad = "SELECT SUM(x.amt) FROM tbl_a x WHERE x.confirmed_at >= DATE_FORMAT(CURDATE(),'%Y-%m-01') \
                   AND x.confirmed_at < DATE_ADD(DATE_FORMAT(CURDATE(),'%Y-%m-01'), INTERVAL 1 MONTH)";
        let v = check_caliber(bad, &[cap_rule()]);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].rule, "require_time_cap:confirmed_at");
        assert!(v[0].hint.contains("confirmed_at < CURDATE()"), "{:?}", v[0].hint);
    }

    #[test]
    fn time_cap_passes_curdate_upper_bound() {
        // 延迟指标标准形态：上限 `< CURDATE()` → 绿（带不带表前缀都认，与 RequireTimeColumn 同纪律）
        let ok = "SELECT SUM(x.amt) FROM tbl_a x WHERE x.confirmed_at >= '2026-08-01' AND x.confirmed_at < CURDATE()";
        assert!(check_caliber(ok, &[cap_rule()]).is_empty());
        // 事实层自证：采集真的记下了这列（防空转）
        let f = facts(ok);
        assert!(f.cap_curdate.contains("confirmed_at"), "{:?}", f.cap_curdate);
        // 防「今天」形态：= CURDATE() 不是上限排除 → 照样红
        let today = "SELECT SUM(x.amt) FROM tbl_a x WHERE DATE(x.confirmed_at) = CURDATE()";
        assert_eq!(rules_of(today, &[cap_rule()]), ["require_time_cap:confirmed_at"]);
    }

    // ---------- `RequireCodeEq`（SALE17：码列上写名称，必返 0 行） ----------

    fn code_eq() -> CaliberRule {
        CaliberRule::RequireCodeEq {
            table: "tbl_cus".into(),
            col: "prov".into(),
            values: vec![("湘南".into(), "430000".into()), ("北山".into(), "110000".into())],
            human: "prov 存码".into(),
        }
    }

    #[test]
    fn code_eq_fires_on_name_like_and_name_eq() {
        // LIKE 家族（SALE17 原形）与等值名称写法，都是必返 0 行的名写码列
        let like = "SELECT SUM(o.amt) FROM tbl_ord o JOIN tbl_cus c ON o.cid = c.cid WHERE c.prov LIKE '%湘南%'";
        let v = check_caliber(like, &[code_eq()]);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].rule, "require_code_eq:tbl_cus.prov");
        assert!(v[0].hint.contains("430000") && v[0].hint.contains("必返 0 行"), "{:?}", v[0].hint);
        let eq = "SELECT SUM(o.amt) FROM tbl_ord o JOIN tbl_cus c ON o.cid = c.cid WHERE c.prov = '湘南'";
        assert_eq!(rules_of(eq, &[code_eq()]), ["require_code_eq:tbl_cus.prov"]);
    }

    #[test]
    fn code_eq_passes_code_form_and_foreign_table() {
        // 写码：唯一正确写法
        let ok = "SELECT SUM(o.amt) FROM tbl_ord o JOIN tbl_cus c ON o.cid = c.cid WHERE c.prov = '430000'";
        assert!(check_caliber(ok, &[code_eq()]).is_empty());
        // 声明的表不在场：不判（同名列在别的表上不一定是码列）
        let foreign = "SELECT SUM(o.amt) FROM tbl_ord o WHERE o.prov LIKE '%湘南%'";
        assert!(check_caliber(foreign, &[code_eq()]).is_empty());
        // 表在场但没用到这列：不判
        let unused = "SELECT SUM(o.amt) FROM tbl_ord o JOIN tbl_cus c ON o.cid = c.cid";
        assert!(check_caliber(unused, &[code_eq()]).is_empty());
    }
}
