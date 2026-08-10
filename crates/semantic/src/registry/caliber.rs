//! 注册表声明 → `Vec<CaliberRule>`：口径规则的**唯一构造点**。变更原因＝声明如何变成规则。
//!
//! 判定与文案在 `dms_kernel::sql::caliber`（纯 AST，零 DMS 语料），本文件只负责
//! 「哪些声明这一轮该生效」。三条纪律：
//! 1. **声明缺失 ≠ 违规**：只为「本轮召回到的表 + 问句命中的指标」造规则，没登记的一律不造。
//! 2. **只造规则不改 SQL**：违规的处置是命名违规 + 回炉重生成（裁决 V3 拒绝 LLM 改写 SQL 路线）。
//! 3. **人话来自声明本身**：`human` 一律是种子里那句 note/描述（LLM 逐字读它），不在代码里另编一套。

use std::collections::{BTreeMap, BTreeSet};

use dms_kernel::nl::text::{map_filter, match_word};
use dms_kernel::nl::time::{time_predicate, window_includes_today};
use dms_kernel::sql::lex::{first_ident_of, split_top_and};
use dms_kernel::CaliberRule;
use sqlx::PgPool;

use crate::ddl::VALUE_ORIGIN_DICT;
use crate::registry::{
    catalog_allows_column, catalog_allows_metric_record, source_asset_live_pred_at,
    table_asset_live_pred_at,
};
use crate::registry::lexicon::{
    load_domain_values, load_value_domains, longest_value_hit, ValueDomain,
};
use crate::registry::model::{
    load_join_edges, load_table_scope_rows, load_table_snapshots, JoinEdge, TableScope, TableSnapshot,
};

/// `meta.metric.unit` 的取值约定：`""` 无单位 / 本值 占比 / `amount` 金额 / `qty` 数量。
pub const UNIT_PERCENT: &str = "percent";
/// 小数比值（例如毛利率 0.23）；不得触发百分数 ×100 判据。
pub const UNIT_RATIO: &str = "ratio";

/// 口径规则要用到的指标声明（`meta.metric` 的一个投影）。
///
/// 为什么不复用 `model::MetricDef`：它少一个 `unit`，而给它加字段会当场打断
/// `server/src/direct.rs` 的 4 处结构体字面量（不是本任务的文件）。等 server 迁移落地后
/// 这两个类型该并成一个 —— 见 `docs/PROGRESS.md` 的欠账。
pub struct CaliberMetric {
    pub name: String,
    pub aliases: Vec<String>,
    /// 来源表声明，可能带装配提示（`t_x(JOIN t_y …)`）或 UNION 串
    pub source_table: String,
    /// 该指标恒需的过滤（`item_type = '1'`、`invoice_status = '2'` …）。
    /// **口径分歧放在这一级**：表级只放真正恒需的（软删），随「问金额还是问数量」而变的放这里
    /// （裁决 二·J′：明细表的 `item_type` 金额侧偏 '3'、数量侧确定 '1'，所以它不是表级恒需）。
    pub scope_filter: String,
    /// 该指标的**时间语义列**（`order_time` / `after_sales_time` / …）。空 = 快照类无时间语义。
    /// 「同表多个时间列语义不同」是 BI 最高频错法之一：注册表钉死了它，
    /// 但此前这条声明**只进 prompt、没有判据** —— 实测「上半年每月销量」用了发货时间，
    /// 既没按下单时点分月、也顺带丢掉主表的有效状态过滤，虚高 26%。
    pub time_col: String,
    /// 指标级时间窗上限（'' = 无；'yesterday' = 算到昨天）。只在**当期问法**时造
    /// `RequireTimeCap`（`window_includes_today` 当闸）—— 问「上月」时那条件本就不必出现。
    pub time_cap: String,
    pub dedup_keys: String,
    pub unit: String,
}

pub async fn load_caliber_metrics(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<CaliberMetric>> {
    let ds_pred = format!(
        "{}{}",
        crate::registry::ds_pred(1),
        source_asset_live_pred_at("", 1)
    );
    let rows: Vec<(
        String,
        Vec<String>,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    )> = sqlx::query_as(&format!(
        "SELECT name, aliases, source_table, agg_expr, scope_filter, time_col, time_cap,
                dedup_keys, unit, version, description
         FROM meta.metric WHERE status = 'active'{ds_pred} ORDER BY name",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(name, _, source, expr, scope, time_col, time_cap, dedup, unit, version, description)| {
            catalog_allows_metric_record(
                ds, name, source, expr, scope, time_col, dedup, description, unit, time_cap,
                version,
            )
        })
        .map(
            |(
                name,
                aliases,
                source_table,
                _,
                scope_filter,
                time_col,
                time_cap,
                dedup_keys,
                unit,
                _,
                _,
            )| CaliberMetric {
                name,
                aliases,
                source_table,
                scope_filter,
                time_col,
                time_cap,
                dedup_keys,
                unit,
            },
        )
        .collect())
}

/// 本轮该生效的口径规则。`recalled_tables` = 已召回到的物理表名（没召回到的表不造规则）。
pub async fn build_rules(
    pg: &PgPool,
    ds: &str,
    question: &str,
    recalled_tables: &[String],
) -> anyhow::Result<Vec<CaliberRule>> {
    let mut out = rules_from(
        question,
        recalled_tables,
        &load_table_scope_rows(pg, ds).await?,
        &load_table_snapshots(pg, ds).await?,
        &load_caliber_metrics(pg, ds).await?,
    );
    // 值域那条不进 `rules_from`：它的输入是另外两张表，且与召回无关（表缺席本身就是它要判的
    // 违规）。合成两段而不是加两个形参 —— 那会改到既有断言体内的调用。
    out.extend(domain_rules(
        question,
        &load_value_domains(pg, ds).await?,
        &load_domain_values(pg, ds).await?,
    ));
    out.extend(code_rules(question, &load_code_values(pg, ds).await?));
    // 与 `code_rules` 分开取一次，不合并成一条 SQL：那条要 `length(code) >= 3` 早筛掉短码
    // （短码无区分度），而这条**一个取值都不许少** —— 少一个就等于把一个真实取值判成非法值。
    out.extend(enum_rules(&load_enum_values(pg, ds).await?, recalled_tables));
    // `RequireCodeEq`（SALE17 两次实测没拦住才加的判据）：码列上的名称写法必返 0 行
    // （`province LIKE '%湖南%'`）。**不依赖字典完整性**（名已登记在另一个码下面就是
    // 全部证据），所以连 seed 批次（非完整枚举）也收 —— 与 enum_rules 的 dict-only 刻意不同。
    out.extend(code_eq_rules(&load_code_eq_values(pg, ds).await?, recalled_tables));
    // 扇出判据（FIN01）：普适规则、不靠问句召回 —— 它只在 SQL 真把「一对多」的多侧键
    // JOIN 进来时才开火（自我限定），召回不召回都得拦。实测：为取客户名把发票
    // `LEFT JOIN t_sales_order ON customer_code`，开票金额放大 299 倍。
    let keys = fanout_keys(&load_join_edges(pg, ds).await?);
    if !keys.is_empty() {
        out.push(CaliberRule::NoFanoutJoin {
            keys,
            human: "JOIN 进一个「一对多」的多侧键会把另一边的行整批复制，度量聚合被放大同样倍数".into(),
        });
    }
    Ok(out)
}

/// `join_edge` 的 card → 「这列在自己表里有重复值」的键清单（`NoFanoutJoin` 的输入）。
/// `N:1` 的多侧是左表、`1:N` 的多侧是右表；`1:1` 与未标注不产键（漏判方向）。
fn fanout_keys(edges: &[JoinEdge]) -> Vec<(String, String)> {
    let mut keys: Vec<(String, String)> = edges
        .iter()
        .flat_map(|e| match e.card.as_str() {
            "N:1" => vec![(e.lt.clone(), e.lc.clone())],
            "1:N" => vec![(e.rt.clone(), e.rc.clone())],
            _ => vec![],
        })
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// 码值域的取值 `(表, 列, 名, 码)`。`length(code) >= 3` 这一刀留在 SQL 里只是**早筛**
/// （短码占 value_map 一半以上，拉回来再扔纯浪费；谓词一律不出 SQL 是本仓纪律）——
/// 权威判据是 `code_is_distinctive`，断言打在它上面。
pub async fn load_code_values(
    pg: &PgPool,
    ds: &str,
) -> anyhow::Result<Vec<(String, String, String, String)>> {
    let ds_pred = format!(
        "{}{}",
        crate::registry::ds_pred(1),
        table_asset_live_pred_at("", 1)
    );
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(&format!(
        "SELECT table_name, column_name, name, code FROM meta.value_map
         WHERE length(code) >= 3{ds_pred}",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(table, column, ..)| catalog_allows_column(ds, table, column))
        .collect())
}

/// 码表行 `(表, 列, 名, 码, 来源)`。`origin = $2` 那一刀留在 SQL 里只是**早筛**
/// （与 `load_code_values` 的 `length(code) >= 3` 同一形态）：权威判据是 `enum_rules` 里
/// 对 `VALUE_ORIGIN_DICT` 的比对，断言打在那上面 —— 只筛在 SQL 里的话，「来源不对就不判」
/// 这一条就只剩「构造侧看不见这几行」一句话，枪测不到。
///
/// 来源走 **bind 而不是插值**：`tests/drift.rs` 的 SQL 插值白名单只放 `ds_pred`，
/// 往那份白名单加例外是要写理由的（那道门槛本身就是它的价值）。
pub async fn load_enum_values(
    pg: &PgPool,
    ds: &str,
) -> anyhow::Result<Vec<(String, String, String, String, String)>> {
    let ds_pred = format!(
        "{}{}",
        crate::registry::ds_pred(1),
        table_asset_live_pred_at("", 1)
    );
    let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(&format!(
        "SELECT table_name, column_name, name, code, origin FROM meta.value_map
         WHERE origin = $2{ds_pred}",
    ))
    .bind(ds)
    .bind(VALUE_ORIGIN_DICT)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(table, column, ..)| catalog_allows_column(ds, table, column))
        .collect())
}

/// `RequireCodeEq` 的取值：**seed + dict 全量**（dict 是完整枚举、seed 是手工登记的
/// 实际出现过的码 —— 对「名已登记在另一个码下面」这条证据两者等价）。
/// 只收 `name != code` 的行（name=code 的名称型值域行不是证据）。
pub async fn load_code_eq_values(
    pg: &PgPool,
    ds: &str,
) -> anyhow::Result<Vec<(String, String, String, String)>> {
    let ds_pred = format!(
        "{}{}",
        crate::registry::ds_pred(1),
        table_asset_live_pred_at("", 1)
    );
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(&format!(
        "SELECT table_name, column_name, name, code FROM meta.value_map
         WHERE origin IN ('seed', 'dict') AND name <> code{ds_pred}",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(table, column, ..)| catalog_allows_column(ds, table, column))
        .collect())
}

/// 码列上的名称写法 → `RequireCodeEq`（**纯函数**）。
/// 判据形态刻意只收「名≠码」的证据行：写名必返 0 行是可证的，与字典完不完整无关。
fn code_eq_rules(
    rows: &[(String, String, String, String)],
    recalled: &[String],
) -> Vec<CaliberRule> {
    let mut map: BTreeMap<(String, String), Vec<(String, String)>> = BTreeMap::new();
    for (t, c, name, code) in rows {
        if name == code {
            continue; // 名称型值域行不是证据（判据文档：名≠码才是）
        }
        map.entry((t.to_lowercase(), c.to_lowercase()))
            .or_default()
            .push((name.clone(), code.clone()));
    }
    map.into_iter()
        .filter(|((t, _), _)| is_recalled(recalled, t))
        .map(|((t, c), values)| CaliberRule::RequireCodeEq {
            table: t,
            human: format!("{c} 列存的是编码不是名称（用名称当过滤值必返 0 行）"),
            col: c,
            values,
        })
        .collect()
}

/// 「这张表本轮召回到了吗」。`rules_from` 与 `enum_rules` **共用一份**：各写一个闭包就会漂
/// （一边大小写敏感一边不敏感，正是本仓反复抓的「两处真相源」形态）。
fn is_recalled(recalled: &[String], t: &str) -> bool {
    fn bare(table: &str) -> &str {
        table
            .rsplit('.')
            .next()
            .unwrap_or(table)
            .trim_matches(|c| matches!(c, '`' | '"'))
    }
    recalled.iter().any(|r| bare(r).eq_ignore_ascii_case(bare(t)))
}

/// 完整枚举的码表列 → `RequireKnownValue`（**纯函数**，`rows` 形状同 `load_enum_values`）。
///
/// 治**最阴的那一族静默错答**：在已登记码表的列上写一个不存在的中文值 → SQL 合法、
/// 三段闸门放行、执行成功、**返 0 行**。无报错、无告警、route 正常、`caliber_note` 为空，
/// 用户读成「本月没有这类客户」。确定性换码器在码表里查不到那个名字时是**原样放行**的
/// （断言 `value_unknown_name_untouched` 守着「不许乱改用户的值」，那条没错 ——
/// 缺的是另一侧：查不到就该有人喊一声）。
///
/// 两道闸，缺一道就误伤（而误伤一条会连带把本来对的答案回炉改错，裁决 二·G）：
/// ① **只认 `origin = dict`**：`meta.value_map` 三种来源里只有「自动发现字典对码」那批可证
///    完整枚举（登记字典全码 + 抽样 `uniq.len() > 60` 即整列跳过）。手写种子只播了会用到的
///    那几个取值、名称型探针 2000 封顶会截断 —— 对这两批开火全是假红，见 `ddl::VALUE_ORIGIN_*`。
/// ② **歧义即弃**：同一个列名登记在多张表上、而两边取值集**不同** → 相关的一条都不造。
///    kernel 的事实采集只记列名不记前缀（同名列几乎总是同一本字典，那是刻意的），
///    于是取值集不同时，用了 A 的合法值会被 B 判红。取值集相同则照造 ——
///    同一本字典注册到十几张表的同类列上是常态（实测 109 个名字跨 ≥2 个 (表, 列)，裁决 二·AD1）。
///
/// `recalled` = 本轮召回到的物理表名（与 `rules_from` 同口径，同一个 `is_recalled`）。
/// 它治的是**血量**，不是正确性（kernel 侧 `known_value` 本来就要求声明的表在 FROM/JOIN 里）。
/// 连库实测（`meta.value_map` 936 行 / 82 个 (表,列)，其中 dict 那批 **68 个 (表,列) / 775 个取值**，
/// 41 个不同列名且**歧义为 0**（同名列取值集全同）⇒ 上面那道歧义门一条都不弃）：
/// - 不过滤 ⇒ **每个问句 68 条规则**，而 `agent/src/run.rs:225` 是
///   `tracing::info!(rules = …, detail = ?r, …)` ⇒ 775 个 `(名,码)` 对按 Debug 进 INFO。
///   光 `company_code` 一列就在 **15 张表**上各 31 个取值（465 行，占 `value_map` 一半）。
/// - 过滤后 ⇒ 单表最多 6 个 dict 列（`t_market_expense_application`，62 个取值），
///   典型召回 3~5 张表落回个位数条。
/// 🔴 过滤放在**产出侧**，歧义判定仍跑全量声明：先按召回裁掉再判歧义，就看不见没被召回的
/// 那张兄弟表 —— 而 SQL 真把它 JOIN 进来时（LLM 不受召回约束），用了它的合法值会被这条判红。
fn enum_rules(
    rows: &[(String, String, String, String, String)],
    recalled: &[String],
) -> Vec<CaliberRule> {
    let mut by_col: BTreeMap<(String, String), Vec<(String, String)>> = BTreeMap::new();
    for (t, c, name, code, _) in rows.iter().filter(|r| r.4 == VALUE_ORIGIN_DICT) {
        let vs = by_col.entry((t.to_lowercase(), c.to_lowercase())).or_default();
        let pair = (name.trim().to_string(), code.trim().to_string());
        if !pair.0.is_empty() && !pair.1.is_empty() && !vs.contains(&pair) {
            vs.push(pair);
        }
    }
    // 按**码**排一次：加载 SQL 没有 ORDER BY，而判词里那串合法取值必须逐次一致
    // （回炉指令有 golden 对比）。码序也正好是字典自己的序，读起来像那本字典。
    for vs in by_col.values_mut() {
        vs.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    }
    let mut seen: BTreeMap<&str, &Vec<(String, String)>> = BTreeMap::new();
    let mut ambiguous: BTreeSet<&str> = BTreeSet::new();
    for ((_, c), vs) in &by_col {
        if seen.insert(c.as_str(), vs).is_some_and(|prev| prev != vs) {
            ambiguous.insert(c.as_str());
        }
    }
    by_col
        .iter()
        .filter(|((t, c), _)| !ambiguous.contains(c.as_str()) && is_recalled(recalled, t))
        .map(|((t, c), vs)| CaliberRule::RequireKnownValue {
            table: t.clone(),
            col: c.clone(),
            values: vs.clone(),
            human: format!(
                "「{t}.{c}」存的是编码，而它对上的是一份**完整**的编码字典（自动发现按字典全码登记）：\
                 写一个不在这份字典里的值，SQL 合法、执行也不报错，但结果是空的 —— \
                 用户会当成「没有这类数据」"
            ),
        })
        .collect()
}

/// 「按分类维度问」的问法词。中文业务词判定刻意留在语义层（kernel 不认业务词，
/// 与 `RequirePercentScale` 同一先例）。
const CATEGORY_WORDS: &[&str] = &["分类", "品类", "类别"];

/// 「问的是百分数」的问法词。与 `CATEGORY_WORDS` 同一先例（业务词留语义层，kernel 不认业务词）。
///
/// 🔴 **一律 ≥2 字，绝不放裸「率」。** 真误伤的不是汇率/税率（那是存量列、没有投影除法、
/// 被 kernel 的 `f.divide` 天然挡住），而是**库存周转率**（出库额/平均库存＝倍数）、
/// **效率**（件/人时）、**频率**（次/周）—— 这三类是货真价实的投影除法且**绝不该 ×100**，
/// 一旦命中就是「把本来对的答案回炉改错」（裁决 二·G 实测过那种误伤）。
/// 而裸「率」对本仓**零收益**：AS02 原句「今年的售后单里已经处理完成的**占比**是多少？」
/// 命中的是「占比」，问句里压根没有「率」（只有题名有）。
/// 要补率覆盖就用**白名单**（完成率/达成率/增长率…）而不是黑名单 —— 黑名单会被下一个新指标静默突破。
const PERCENT_WORDS: &[&str] = &["占比", "比例", "百分比", "百分之"];

/// 值域命中 → `RequireJoinAndFilter`（**纯函数**，`values` 形状同 `load_domain_values`）。
///
/// 🔴 **命中即造是错的**：「手抓饼」既是分类名、也确实是多个 `sku_name` 的子串，问
/// 「手抓饼卖了多少」时按商品名过滤是合理问法，硬判红就是误伤。只有问句明说
/// 「这个分类 / 品类 / 类别」时，才断言「必须 JOIN 分类表并用分类名列过滤」。
/// `human` 一律是 `meta.value_domain.note`（种子里那句带实测数字的人话）。
fn domain_rules(
    question: &str,
    domains: &[ValueDomain],
    values: &[(String, String, String)],
) -> Vec<CaliberRule> {
    if !CATEGORY_WORDS.iter().any(|w| question.contains(w)) {
        return vec![];
    }
    domains
        .iter()
        .filter(|d| {
            let same = |t: &String, c: &String| {
                t.eq_ignore_ascii_case(&d.table_name) && c.eq_ignore_ascii_case(&d.column_name)
            };
            let vs = values.iter().filter(|(t, c, _)| same(t, c)).map(|(_, _, v)| v.as_str());
            longest_value_hit(question, vs).is_some()
        })
        .map(|d| CaliberRule::RequireJoinAndFilter {
            table: d.table_name.to_lowercase(),
            col: d.column_name.to_lowercase(),
            human: d.note.clone(),
        })
        .collect()
}

/// 码够独特才配得上「这个码只许出现在这一列」这条断言。
/// `'1'`/`'2'`/`'4'` 这种短码在任何 SQL 里都可能出现（状态、标志、行数、rn = 1…），
/// 按它判必然大面积误伤 —— 而误伤一条会连带把本来对的答案回炉改错（裁决 二·G）。
/// 门槛＝**长度 ≥ 3 且全为 ASCII 数字或大写字母**：`430000`/`Z001`/`ZZ05` 造，`1`/`10`/`01`/`HM` 不造。
/// 名称型值域的取值（`name = code = 中文名`）也被这条挡在外面：那批归 `RequireJoinAndFilter` 管，
/// 两条判据不重叠。
fn code_is_distinctive(code: &str) -> bool {
    code.len() >= 3 && code.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
}

/// 码值命中 → `RequireCodeOnColumn`（**纯函数**，`rows` 形状同 `load_code_values`）。
///
/// 治「取对了码、用错了列」：提示卡把 `(表, 列, 码)` 三样都摆给了 LLM，它把码抄对了、
/// 却写到另一张表上名字相近的列去（实测那题答出的数字看着完全合理，语义全错）。
///
/// 三道闸，缺一道就误伤：
/// ① 码本身够独特（`code_is_distinctive`）；
/// ② **问句必须命中该码的名字**（`longest_value_hit`：最长优先、单字不算）——
///    没命中名字就说明这个码不是从问句来的，管它用在哪一列纯属多事；
/// ③ **歧义即弃**：同一个码登记在两个 `(表, 列)` 上时一条不造 —— 用了其中一个就必然
///    违反另一条，那是判据自己造的假红（自动发现会把同一本字典注册到多张表的同类列上）。
fn code_rules(question: &str, rows: &[(String, String, String, String)]) -> Vec<CaliberRule> {
    let mut by_col: BTreeMap<(String, String), Vec<(&str, &str)>> = BTreeMap::new();
    for (t, c, name, code) in rows.iter().filter(|r| code_is_distinctive(&r.3)) {
        by_col.entry((t.to_lowercase(), c.to_lowercase())).or_default().push((name, code));
    }
    // 每个 (表, 列) 只认最长命中的那一个名字（问句提了两个取值时也只造一条，SQL 用 IN 即通过）
    let hits: Vec<(&String, &String, &str, &str)> = by_col
        .iter()
        .filter_map(|((t, c), vs)| {
            let hit = longest_value_hit(question, vs.iter().map(|(n, _)| *n))?;
            Some((t, c, hit, vs.iter().find(|(n, _)| *n == hit)?.1))
        })
        .collect();
    hits.iter()
        .filter(|h| hits.iter().filter(|x| x.3 == h.3).count() == 1)
        .map(|(t, c, name, code)| CaliberRule::RequireCodeOnColumn {
            table: t.to_string(),
            col: c.to_string(),
            code: code.to_string(),
            human: format!(
                "「{name}」在这个库里存的是编码 {code}，而这个编码属于 {t}.{c}：\
                 换成别的列写同一个编码会取到另一批数据（码对、列错，数字看着合理但语义全变）"
            ),
        })
        .collect()
}

/// 声明 → 规则（**纯函数**，DB 只负责把声明取出来 —— 断言全打在这里）。
fn rules_from(
    question: &str,
    recalled: &[String],
    scopes: &[TableScope],
    snaps: &[TableSnapshot],
    metrics: &[CaliberMetric],
) -> Vec<CaliberRule> {
    let seen = |t: &str| is_recalled(recalled, t);
    let mut out: Vec<CaliberRule> = vec![];
    // 同一张表可能在 table_scope 与 table_snapshot 各有一半声明（快照表的 extra_filter 也是
    // 「恒需的过滤」）。合并成每表一条 RequireCols：否则同一张表会报两条 `require_cols:表名`。
    let mut cols: BTreeMap<&str, (Vec<String>, &str)> = BTreeMap::new();
    for s in scopes.iter().filter(|s| seen(&s.table_name)) {
        merge_cols(&mut cols, &s.table_name, &s.filter, &s.note);
    }
    for s in snaps.iter().filter(|s| seen(&s.table_name)) {
        merge_cols(&mut cols, &s.table_name, &s.extra_filter, &s.note);
        out.push(CaliberRule::RequireLatest {
            table: s.table_name.to_lowercase(),
            partition: split_list(&s.partition_cols),
            human: s.note.clone(),
        });
    }
    out.extend(cols.into_iter().filter(|(_, (c, _))| !c.is_empty()).map(|(t, (c, note))| {
        CaliberRule::RequireCols { table: t.to_lowercase(), cols: c, human: note.to_string() }
    }));
    out.extend(metric_rules(question, metrics));
    // 🔴 问句明说要百分数，而本轮**没有** `unit='percent'` 的指标命中 → 补一条。
    //
    // 缺陷现场（评测 AS02，连续几趟稳定红）：「今年的售后单里已经处理完成的占比是多少？」
    // 答 `0.9576` 而 gold 是 `95.76` —— 差 100 倍。根因是 `RequirePercentScale` 的唯一构造点
    // 在 `metric_rules` 里、条件是 `m.unit == UNIT_PERCENT`，也就是**只认已声明指标**；
    // 而「完成率」不在 `METRICS` 里（唯一 percent 的是 `refund_ratio`）→ 压根不造规则 → 回炉链无从触发。
    //
    // 判据本体不在这里，在 kernel（`f.divide && !f.times_100`，且 `divide` 只在**非条件位置**置位）——
    // 所以不做除法的问句天然不受影响，这条只负责「把规则造出来」。
    // 误伤面**实测为 0**：逐题核过 38+55+16+5 道，含率词的只有 AS02/AS04/SALE16 三道且 gold 全已 ×100
    // （规则对 gold 静默）；反向那侧「除法且不 ×100」只有两道客单价，问句不含任何率词，
    // 且三重免疫（客单价 `unit=""` / direct-agg 不跑 `check_caliber` / 复合句每个子问独立建规则）。
    // 顺带收益：SALE16（未声明的环比增长率，问句含「百分之」）今天没有任何占比判据，改完就有了。
    //
    // 去重守卫是**载荷不是洁癖**：现存断言 `only_percent_unit_yields_scale_rule` 的问句含「比例」，
    // 去掉守卫那条当场红（两条规则 + human 串味）—— 等于自带一次非恒真验证。
    let matched_ratio = metrics.iter().any(|metric| {
        metric.unit == UNIT_RATIO
            && match_word(question, &metric.name, &metric.aliases).is_some()
    });
    if PERCENT_WORDS.iter().any(|w| question.contains(w))
        && !matched_ratio
        && !out.iter().any(|r| matches!(r, CaliberRule::RequirePercentScale { .. }))
    {
        out.push(CaliberRule::RequirePercentScale {
            // `metric` 会进 rule id 与 hint（kernel 侧 `viol("require_percent_scale", metric, …)`）。
            // 取「占比」而不是「完成率」—— 不许假造一个不存在的指标名。
            metric: "占比".into(),
            human: "问句问的是占比/百分比：除法结果必须 * 100.0 再 ROUND(…, 2)，\
                    否则答出 0.9576 而不是 95.76（评测 AS02）"
                .into(),
        });
    }
    out
}

/// 把一条 filter 串的列名并进该表的口径列集合（`note` 首次登记者胜出，不拼接两句人话）。
fn merge_cols<'a>(
    map: &mut BTreeMap<&'a str, (Vec<String>, &'a str)>,
    table: &'a str,
    filter: &str,
    note: &'a str,
) {
    let e = map.entry(table).or_insert_with(|| (vec![], note));
    for c in cols_of_filter(filter) {
        if !e.0.contains(&c) {
            e.0.push(c);
        }
    }
}

/// filter 串 → 被约束的列名：按顶层 AND 切开，每个原子条件取第一个标识符。
/// `deleted_flag = 0 AND order_status NOT IN ('0','108','199')` → `[deleted_flag, order_status]`。
/// 声明里的 filter 一律**不带表前缀**（带了会取到前缀 → 该列判不了，漏判方向）。
fn cols_of_filter(filter: &str) -> Vec<String> {
    split_top_and(filter).iter().filter_map(|c| first_ident_of(c)).collect()
}

/// 问句命中的指标 → 去重键与占比单位两条规则。命中判据与净化与四种卡片召回同一套
/// （`match_word` 最长别名 + `map_filter`：问「库存金额」不该同时拖出「库存量」）。
fn metric_rules(question: &str, metrics: &[CaliberMetric]) -> Vec<CaliberRule> {
    let matched: Vec<(usize, String)> = metrics
        .iter()
        .enumerate()
        .filter_map(|(i, m)| match_word(question, &m.name, &m.aliases).map(|w| (i, w)))
        .collect();
    let pairs: Vec<(String, String)> =
        matched.iter().map(|(i, w)| (metrics[*i].name.clone(), w.clone())).collect();
    let mut out = vec![];
    for k in map_filter(&pairs) {
        let m = &metrics[matched[k].0];
        let base = base_table(&m.source_table);
        // 🔴 指标级口径也要有**校验器背书**，不能只靠 `correct_caliber` 的确定性补全。
        //
        // 由来（裁决 二·J′）：`item_type='1'` 原本登记在**表级**（`meta.table_scope`），
        // 那是过宽的 —— 它是数量侧口径，金额侧偏 '3'（与订单头差 0.012%），用 '1' 低报 35.7%。
        // 收窄表级声明是对的，但收窄之后 `item_type` 在校验器侧**一条规则都不剩**：
        // 数量类问句只剩 `add_scope_filter` 一道防线，它一旦因形态不支持而放弃
        // （自连接、目标表出现两次、派生表内同名表…），就静默回到「动销商品数 292 vs 正确 173」。
        // 从指标 `scope_filter` 造 `RequireCols` 把那道背书补回来，且**不会**把数量口径强加到金额问句上
        // —— 规则只在该指标被问句命中时才造。
        //
        // 含子查询的 `scope_filter` 跳过（库存类的 `product_stock_date = (SELECT MAX(…))`）：
        // 那种 filter 切出来的列名不代表「必须被约束的列」，与 `add_scope_filter` 同一道门。
        if !m.scope_filter.trim().is_empty()
            && !base.is_empty()
            && !m.scope_filter.to_uppercase().contains("SELECT")
        {
            let cols = cols_of_filter(&m.scope_filter);
            if !cols.is_empty() {
                out.push(CaliberRule::RequireCols {
                    table: base.clone(),
                    cols,
                    human: format!(
                        "指标「{}」的口径：{}（这是**指标级**口径，随问的是金额还是数量而变，\
                         所以不在表级声明里）",
                        m.name, m.scope_filter
                    ),
                });
            }
        }
        if !m.dedup_keys.is_empty() && !base.is_empty() {
            out.push(CaliberRule::RequireDedup {
                table: base.clone(),
                keys: split_list(&m.dedup_keys),
                human: format!(
                    "指标「{}」的来源表 {base} 含系统级重复行（ETL 双写整行 ×2），\
                     聚合前必须先按去重键去重，否则数值虚增一倍",
                    m.name
                ),
            });
        }
        // 🔴 声明的时间列 → `RequireTimeColumn`，**只在问句真的带时间范围时才造**。
        //
        // 由来：`metric_card` 早就写着「时间过滤【必须】用 xxx 列」，但那只是提示 ——
        // 实测「上半年每月销量」用了明细表自己的发货时间列，既没按下单时点分月、
        // 也顺带丢掉了主表上的有效状态过滤（那张表压根没 JOIN 进来），虚高 26%。
        // 而 `RequireCols` 遇「表整个缺席」按宁缺毋滥不判，于是那条错 SQL 一路绿灯。
        //
        // `time_predicate(question).is_some()` 这道闸是必需的：没有它，
        // 「销售额top3商品分类」这类**无时间边界**的合法问法会被判红 —— 误伤一条
        // 会连带把本来对的答案回炉改错（裁决 二·G 实测过）。
        if !m.time_col.trim().is_empty() && time_predicate(question).is_some() {
            out.push(CaliberRule::RequireTimeColumn {
                col: m.time_col.trim().to_string(),
                human: format!(
                    "指标「{}」的时间语义钉在 {} 列上（同表/跨表多个时间列语义不同，是 BI 最高频错法）",
                    m.name,
                    m.time_col.trim()
                ),
            });
        }
        // 🔴 `time_cap='yesterday'` + **当期问法** → `RequireTimeCap`（上限必须 `< CURDATE()`）。
        // 由来（ship_net_sales，2026-08-01 实测）：卡片 ⚠️ 句与规则窗追加的 `< CURDATE()`
        // 都在 prompt 里，模型仍照抄期月末日（把追加条件当冗余清理），含今天虚 1.8% ——
        // 提示赢不了，只能上判据。`window_includes_today` 当闸：往期间法（上月/去年）
        // 本就不必出现这条条件，判了就是误伤。
        if m.time_cap == "yesterday"
            && !m.time_col.trim().is_empty()
            && window_includes_today(question)
        {
            out.push(CaliberRule::RequireTimeCap {
                col: m.time_col.trim().to_string(),
                human: format!("指标「{}」算到**昨天**（今天的数据不全，含今天数字会虚）", m.name),
            });
        }
        if m.unit == UNIT_PERCENT {
            out.push(CaliberRule::RequirePercentScale {
                metric: m.name.clone(),
                human: format!(
                    "指标「{}」的单位是占比（百分数）：除法结果必须 * 100.0 再 ROUND(…, 2)，\
                     否则答出 0.049 而不是 4.9",
                    m.name
                ),
            });
        }
    }
    out
}

/// 来源表声明 → SQL 里会出现的**末段表名**（kernel 按它比对）：取第一个标识符。
/// `t_sales_order_detail(JOIN t_sales_order 有效订单)` → `t_sales_order_detail`。
/// `A UNION ALL B` 只认 A（B 不判 —— 漏判方向，符合「宁缺毋滥」）。
fn base_table(source_table: &str) -> String {
    let table = source_table
        .trim()
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .trim_matches('`');
    table
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<String>()
        .to_lowercase()
}

/// 逗号分隔列清单 → 小写列名（`partition_cols` / `dedup_keys` 共用）
fn split_list(s: &str) -> Vec<String> {
    s.split(',').map(|c| c.trim().to_lowercase()).filter(|c| !c.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(t: &str, f: &str) -> TableScope {
        TableScope { table_name: t.into(), filter: f.into(), note: format!("{t} 的口径人话") }
    }
    fn snap() -> TableSnapshot {
        TableSnapshot {
            table_name: "t_customer_balance".into(),
            partition_cols: "customer_code,balance_type".into(),
            order_cols: "created_time DESC, id DESC".into(),
            extra_filter: "balance_status = '4'".into(),
            note: "快照表取最新一条".into(),
        }
    }
    fn qty_metric() -> CaliberMetric {
        CaliberMetric {
            name: "销量".into(),
            aliases: ["销售量", "卖了多少箱"].iter().map(|s| s.to_string()).collect(),
            source_table: "t_sales_order_detail(JOIN t_sales_order 有效订单)".into(),
            // 与种子 `sales_qty` 逐字相同（数量侧口径在**指标级**，不在表级——裁决 二·J′）
            scope_filter: "item_type = '1'".into(),
            // 与种子 `sales_qty` 逐字相同：时间语义在**订单头**的 order_time 上，
            // 不在明细表自己的时间列上（那正是 GOODS13 虚高 26% 的错法）
            time_col: "order_time".into(),
            time_cap: String::new(),
            dedup_keys: "sales_order_code,sku_code,sku_name,box_quantity,amount".into(),
            unit: String::new(),
        }
    }
    fn tables(ts: &[&str]) -> Vec<String> {
        ts.iter().map(|s| s.to_string()).collect()
    }

    /// filter 串 → 列名：三种真实声明形态（等值 / NOT IN / 子查询）
    #[test]
    fn filter_splits_into_column_names() {
        assert_eq!(cols_of_filter("item_type = '1' AND deleted_flag = 0"), ["item_type", "deleted_flag"]);
        assert_eq!(
            cols_of_filter("deleted_flag = 0 AND order_status NOT IN ('0','108','199')"),
            ["deleted_flag", "order_status"]
        );
        assert_eq!(
            cols_of_filter("product_stock_date = (SELECT MAX(product_stock_date) FROM t_winc_stock_report)"),
            ["product_stock_date"]
        );
        assert!(cols_of_filter("").is_empty());
    }

    /// 来源表声明里的装配提示与 UNION 串不能被当成表名（kernel 按末段表名比对）
    #[test]
    fn base_table_strips_composition_hints() {
        assert_eq!(base_table("t_sales_order_detail(JOIN t_sales_order 有效订单)"), "t_sales_order_detail");
        assert_eq!(base_table("t_invoice_apply_header UNION ALL t_invoice_new_apply_header"), "t_invoice_apply_header");
        assert_eq!(base_table("t_sales_order"), "t_sales_order");
        assert_eq!(base_table("sales_dw.dws_off_offline_sale_dfn"), "dws_off_offline_sale_dfn");
    }

    /// 🔴 声明缺失 ≠ 违规：没召回到的表一条规则都不造（判错一条会让所有人学会忽略校验器）
    #[test]
    fn unrecalled_tables_produce_no_rules() {
        let r = rules_from("本月销售额", &tables(&["t_customer"]), &[scope("t_sales_order_detail", "item_type = '1'")], &[snap()], &[]);
        assert!(r.is_empty(), "{r:?}");
    }

    /// 明细表召回到 → RequireCols 带上 filter 里的两列（治「动销商品数」虚高 69%）
    #[test]
    fn recalled_scope_becomes_require_cols() {
        let scopes = [scope("t_sales_order_detail", "item_type = '1' AND deleted_flag = 0")];
        let r = rules_from("六月动销商品数", &tables(&["t_sales_order_detail", "t_sales_order"]), &scopes, &[], &[]);
        assert_eq!(
            r,
            vec![CaliberRule::RequireCols {
                table: "t_sales_order_detail".into(),
                cols: vec!["item_type".into(), "deleted_flag".into()],
                human: "t_sales_order_detail 的口径人话".into(),
            }]
        );
    }

    /// 快照表：RequireLatest + extra_filter 的列并进**同一条** RequireCols（不报两条同名违规）
    #[test]
    fn snapshot_yields_latest_plus_merged_cols() {
        let scopes = [scope("t_customer_balance", "deleted_flag = 0")];
        let r = rules_from("账户余额最高的10个客户", &tables(&["t_customer_balance"]), &scopes, &[snap()], &[]);
        assert_eq!(
            r,
            vec![
                CaliberRule::RequireLatest {
                    table: "t_customer_balance".into(),
                    partition: vec!["customer_code".into(), "balance_type".into()],
                    human: "快照表取最新一条".into(),
                },
                CaliberRule::RequireCols {
                    table: "t_customer_balance".into(),
                    cols: vec!["deleted_flag".into(), "balance_status".into()],
                    human: "t_customer_balance 的口径人话".into(),
                },
            ]
        );
    }

    /// 指标命中 → **三条**规则：指标级口径（`RequireCols`）+ 去重键（`RequireDedup`）
    /// + 声明的时间列（`RequireTimeColumn`，仅当问句带时间范围）。
    ///
    /// 🔴 `RequireCols` 那条是裁决 二·J′ 补的，别当成冗余删掉：`item_type='1'` 原先登记在
    /// **表级**，那是过宽的（数量侧口径，金额侧偏 '3'，用 '1' 低报 35.7%）。收窄表级声明之后，
    /// 若指标级不造 `RequireCols`，`item_type` 在校验器侧**一条规则都不剩** ——
    /// 数量类问句只剩 `add_scope_filter` 一道防线，它一旦因形态不支持而放弃就静默回到
    /// 「动销商品数 292 vs 正确 173」。
    ///
    /// 🔴 `RequireTimeColumn` 那条同理：`metric_card` 早就写着「时间过滤【必须】用某列」，
    /// 但那只是提示 —— 实测「上半年每月销量」用了明细表自己的发货时间列，
    /// 既没按下单时点分月、也顺带丢掉了主表上的有效状态过滤（那张表压根没 JOIN 进来），虚高 26%。
    #[test]
    fn hit_metric_yields_caliber_dedup_and_time_rules() {
        let r = rules_from("本月卖了多少箱", &[], &[], &[], &[qty_metric()]);
        assert_eq!(r.len(), 3, "{r:?}");
        // ① 指标级口径：来源表取末段表名，列从 scope_filter 切出
        let CaliberRule::RequireCols { table, cols, human } = &r[0] else { panic!("{r:?}") };
        assert_eq!(table, "t_sales_order_detail");
        assert_eq!(cols, &["item_type"]);
        assert!(human.contains("销量") && human.contains("指标级"), "{human}");
        // ② 去重键
        let CaliberRule::RequireDedup { table, keys, human } = &r[1] else { panic!("{r:?}") };
        assert_eq!(table, "t_sales_order_detail");
        assert_eq!(keys.len(), 5);
        assert_eq!(keys[3], "box_quantity");
        assert!(human.contains("销量") && human.contains("虚增"), "{human}");
        // ③ 声明的时间列（问句有「本月」→ 造）
        let CaliberRule::RequireTimeColumn { col, human } = &r[2] else { panic!("{r:?}") };
        assert_eq!(col, "order_time");
        assert!(human.contains("销量") && human.contains("order_time"), "{human}");
        // 问句不含指标名/别名 → 一条都不造（命中的指标才算声明生效）
        assert!(rules_from("本月有多少个客户", &[], &[], &[], &[qty_metric()]).is_empty());
        // 来源表声明不是标识符开头 → 两条**表级定位**的规则都不造（宁缺毋滥）。
        // 时间列那条仍造：它按设计与表无关（声明只知道列名，不知道它在哪张表上，
        // 而带上表名会让「JOIN 进来但用了别名」的正确写法误红）。
        let weird = CaliberMetric { source_table: "(子查询)".into(), ..qty_metric() };
        let rw = rules_from("本月卖了多少箱", &[], &[], &[], &[weird]);
        assert_eq!(rw.len(), 1, "{rw:?}");
        assert!(matches!(rw[0], CaliberRule::RequireTimeColumn { .. }), "{rw:?}");
        // 口径含子查询（库存类的 `= (SELECT MAX(…))`）→ 不造 RequireCols，只留去重那条
        let sub = CaliberMetric {
            scope_filter: "product_stock_date = (SELECT MAX(product_stock_date) FROM t_x)".into(),
            ..qty_metric()
        };
        let r2 = rules_from("本月卖了多少箱", &[], &[], &[], &[sub]);
        assert_eq!(r2.len(), 2, "{r2:?}");
        assert!(matches!(r2[0], CaliberRule::RequireDedup { .. }), "{r2:?}");
        // 🔴 问句**无时间边界** → 不造时间列规则（少了这道闸，「销售额top3商品分类」
        // 这类合法问法会被判红，而误伤一条会连带把对的答案回炉改错，裁决 二·G 实测过）
        let no_time = rules_from("卖了多少箱", &[], &[], &[], &[qty_metric()]);
        assert!(
            !no_time.iter().any(|r| matches!(r, CaliberRule::RequireTimeColumn { .. })),
            "{no_time:?}"
        );
        // 时间列声明为空（快照类指标）→ 即便问句带时间也不造
        let no_tcol = CaliberMetric { time_col: String::new(), ..qty_metric() };
        let r3 = rules_from("本月卖了多少箱", &[], &[], &[], &[no_tcol]);
        assert!(
            !r3.iter().any(|r| matches!(r, CaliberRule::RequireTimeColumn { .. })),
            "{r3:?}"
        );
    }

    /// 🔴 端到端：本文件造的规则喂给 kernel，**gold 必须过、评测那条错答必须红**。
    /// 只有这条断言能证明声明真的接上了判据（前面几条只证明规则长得对）。
    #[test]
    fn real_gold_passes_and_the_evaluated_wrong_sql_is_flagged() {
        let scopes = [
            scope("t_sales_order_detail", "item_type = '1' AND deleted_flag = 0"),
            scope("t_sales_order", "deleted_flag = 0 AND order_status NOT IN ('0','108','199')"),
        ];
        let recalled = tables(&["t_sales_order_detail", "t_sales_order", "t_customer_balance"]);
        let rules = rules_from("2026年6月动销商品有多少个", &recalled, &scopes, &[snap()], &[]);
        // GOODS15 gold（实测 173）：两张表的口径列都约束了 → 零违规
        let gold = "SELECT COUNT(DISTINCT d.sku_code) FROM t_sales_order_detail d \
            JOIN t_sales_order o ON o.sales_order_code = d.sales_order_code \
            WHERE d.item_type = '1' AND d.deleted_flag = 0 AND o.deleted_flag = 0 \
            AND o.order_status NOT IN ('0','108','199') AND o.order_time >= '2026-06-01'";
        assert!(dms_kernel::check_caliber(gold, &rules).is_empty());
        // 评测实际答出的那条（292，虚高 69%）＝ gold 去掉明细表那两列
        let wrong = gold.replace("d.item_type = '1' AND d.deleted_flag = 0 AND ", "");
        let v = dms_kernel::check_caliber(&wrong, &rules);
        assert_eq!(v.iter().map(|x| x.rule.as_str()).collect::<Vec<_>>(), ["require_cols:t_sales_order_detail"]);
        assert!(v[0].hint.contains("item_type") && v[0].hint.contains("deleted_flag"), "{:?}", v[0]);
        // FIN02 gold（快照分桶取最新）过；裸扫快照表红在 require_latest + 少了 balance_status
        let fin_gold = "SELECT c.customer_name, SUM(t.balance) FROM (SELECT customer_code, balance_type, balance, \
            ROW_NUMBER() OVER (PARTITION BY customer_code, balance_type ORDER BY created_time DESC, id DESC) AS rn \
            FROM t_customer_balance WHERE deleted_flag = 0 AND balance_status = '4' AND balance_type IN ('8','9')) t \
            JOIN t_customer c ON c.customer_code = t.customer_code WHERE t.rn = 1 GROUP BY t.customer_code";
        assert!(dms_kernel::check_caliber(fin_gold, &rules).is_empty());
        let fin_bad = "SELECT b.customer_code, SUM(b.balance) FROM t_customer_balance b \
            WHERE b.deleted_flag = 0 GROUP BY b.customer_code ORDER BY SUM(b.balance) DESC LIMIT 10";
        let mut got: Vec<String> =
            dms_kernel::check_caliber(fin_bad, &rules).into_iter().map(|x| x.rule).collect();
        got.sort();
        assert_eq!(got, ["require_cols:t_customer_balance", "require_latest:t_customer_balance"]);
    }

    /// 🔴 AS02：问句问的是**占比**、而「完成率」**不是已声明指标** → 也必须造规则。
    ///
    /// 缺陷现场：`RequirePercentScale` 的唯一构造点条件是 `m.unit == UNIT_PERCENT`，
    /// 也就是只认已声明指标；AS02 的口径不在 `METRICS` 里 → 压根不造规则 →
    /// 答 `0.9576` 而 gold 是 `95.76`，连续几趟稳定红。
    #[test]
    fn percent_question_without_declared_metric_still_yields_scale_rule() {
        // eval_cases.json 里 AS02 的原句，一字不改
        const Q: &str = "今年的售后单里已经处理完成的占比是多少？";
        let rules = rules_from(Q, &[], &[], &[], &[]); // 零声明 ＝ AS02 的现状
        assert_eq!(rules.len(), 1, "{rules:?}");
        assert!(matches!(rules[0], CaliberRule::RequirePercentScale { .. }), "{rules:?}");
        // ① 评测那条错答的形状（0.9576）必须红
        let bad = "SELECT ROUND(COUNT(DISTINCT CASE WHEN after_sales_status = '5' \
                   THEN after_sales_code END) / COUNT(DISTINCT after_sales_code), 4) \
                   AS `完成率` FROM t_after_sales_order_header";
        // 🔴 防恒真：`check_caliber` 解析失败会**返空**（漏判方向）—— 那样下面两条断言
        // 都会「因为看不懂而绿」。先钉住它真的解析动了。本仓已四次踩「入参变空 → 断言恒真」。
        assert!(dms_kernel::sql::caliber::output_shape(bad).is_some(), "解析不动 → 判据恒返空");
        let viols = dms_kernel::check_caliber(bad, &rules);
        let got: Vec<&str> = viols.iter().map(|v| v.rule.as_str()).collect();
        assert_eq!(got, ["require_percent_scale:占比"], "{got:?}");
        // ② gold（带 * 100.0）必须绿 —— 只有这条能排除「恒判红」
        let gold = bad.replace("END) /", "END) * 100.0 /");
        assert!(dms_kernel::check_caliber(&gold, &rules).is_empty());
        // ③ 客单价：同是投影除法、问句无占比词 → 一条不造（E10 与 A08 的金文件靠这条不被打坏）
        assert!(rules_from("本月客单价", &[], &[], &[], &[]).is_empty());
        // ④ 裸「率」刻意不在词表：周转率是除法但**不是**百分数，命中就是把对的答案回炉改错
        assert!(rules_from("库存周转率是多少", &[], &[], &[], &[]).is_empty());
        assert!(rules_from("人均效率是多少", &[], &[], &[], &[]).is_empty());
        // ⑤ SALE16 那种未声明的增长率（问句含「百分之」）今天没有任何占比判据，现在有了
        assert_eq!(rules_from("本月销售额比上月增长了百分之多少", &[], &[], &[], &[]).len(), 1);
    }

    /// unit 判定：只有 'percent' 才造占比规则（空/amount/qty 一律不造）
    #[test]
    fn only_percent_unit_yields_scale_rule() {
        let pct = CaliberMetric {
            name: "退款占比".into(),
            aliases: vec!["退款比例".into()],
            source_table: "t_after_sales_order_header".into(),
            scope_filter: String::new(),
            time_col: String::new(),
            time_cap: String::new(),
            dedup_keys: String::new(),
            unit: UNIT_PERCENT.into(),
        };
        let r = rules_from("今年退款比例是多少", &[], &[], &[], &[pct]);
        assert_eq!(
            r,
            vec![CaliberRule::RequirePercentScale {
                metric: "退款占比".into(),
                human: "指标「退款占比」的单位是占比（百分数）：除法结果必须 * 100.0 再 ROUND(…, 2)，\
                        否则答出 0.049 而不是 4.9"
                    .into(),
            }]
        );
        for unit in ["", "amount", "qty", UNIT_RATIO] {
            let m = CaliberMetric {
                name: "退款额".into(),
                aliases: vec![],
                source_table: "t_after_sales_order_header".into(),
                scope_filter: String::new(),
                time_col: String::new(),
                time_cap: String::new(),
                dedup_keys: String::new(),
                unit: unit.into(),
            };
            assert!(rules_from("今年退款额是多少", &[], &[], &[], &[m]).is_empty(), "unit={unit}");
        }

        let ratio = CaliberMetric {
            name: "毛利率".into(),
            aliases: vec!["毛利占比".into()],
            source_table: "sales_dw.dws_off_offline_sale_dfn".into(),
            scope_filter: String::new(),
            time_col: "order_date".into(),
            time_cap: String::new(),
            dedup_keys: String::new(),
            unit: UNIT_RATIO.into(),
        };
        assert!(
            !rules_from("本月毛利占比", &[], &[], &[], &[ratio])
                .iter()
                .any(|rule| matches!(rule, CaliberRule::RequirePercentScale { .. })),
            "毛利率合同返回小数比值，不能强制乘 100"
        );
    }

    /// 🔴 值域命中 **且** 分类级问法才造规则；顺带端到端：GOODS16 的 gold 过、评测错答红。
    #[test]
    fn category_question_with_domain_hit_yields_join_and_filter() {
        const NOTE: &str = "过滤必须写 cat.category_name LIKE，【不要】写 d.sku_name LIKE（实测虚高 36%）";
        // 声明侧大小写与 value_map 侧不一致是真库常态；规则一律小写
        let d = [ValueDomain {
            table_name: "T_Goods_Category".into(),
            column_name: "Category_Name".into(),
            note: NOTE.into(),
        }];
        let v: Vec<(String, String, String)> = ["手抓饼", "烤肠"]
            .iter()
            .map(|x| ("t_goods_category".into(), "category_name".into(), x.to_string()))
            .collect();
        let rules = domain_rules("2026年6月手抓饼这个分类卖了多少箱", &d, &v);
        assert_eq!(
            rules,
            vec![CaliberRule::RequireJoinAndFilter {
                table: "t_goods_category".into(),
                col: "category_name".into(),
                human: NOTE.into(),
            }]
        );
        // 品类 / 类别 同算分类级问法
        assert_eq!(domain_rules("手抓饼这个品类卖了多少箱", &d, &v).len(), 1);
        assert_eq!(domain_rules("手抓饼类别的销量", &d, &v).len(), 1);
        // 🔴 命中但不是分类级问法 → 一条不造：按商品名过滤是合理问法，硬判红就是误伤
        assert!(domain_rules("2026年6月手抓饼卖了多少箱", &d, &v).is_empty());
        // 分类级问法但没命中取值（词典未灌 / 问的是别的分类）→ 不造，宁缺毋滥
        assert!(domain_rules("2026年6月各分类卖了多少箱", &d, &v).is_empty());
        assert!(domain_rules("手抓饼这个分类卖了多少箱", &d, &[]).is_empty());
        // 端到端：gold 经 t_goods_category 过滤 → 零违规
        let gold = "SELECT SUM(d.box_quantity) FROM t_sales_order_detail d \
            JOIN t_goods g ON g.goods_code = d.sku_code \
            JOIN t_goods_category cat ON cat.id = g.goods_category_code \
            WHERE cat.category_name LIKE '%手抓饼%' AND d.item_type = '1'";
        assert!(dms_kernel::check_caliber(gold, &rules).is_empty());
        // 评测实际答出的那条（按商品名过滤，156847 vs 正确 115175）→ 红，且人话带实测数字
        let bad = dms_kernel::check_caliber(
            "SELECT SUM(d.box_quantity) FROM t_sales_order_detail d WHERE d.sku_name LIKE '%手抓饼%'",
            &rules,
        );
        assert_eq!(
            bad.iter().map(|x| x.rule.as_str()).collect::<Vec<_>>(),
            ["require_join_and_filter:t_goods_category.category_name"]
        );
        assert!(bad[0].human.contains("虚高 36%"), "{:?}", bad[0]);
    }

    fn vm(v: &[(&str, &str, &str, &str)]) -> Vec<(String, String, String, String)> {
        v.iter()
            .map(|(t, c, n, k)| (t.to_string(), c.to_string(), n.to_string(), k.to_string()))
            .collect()
    }

    /// 🔴 码值命中 → 「这个码只许用在声明的那一列上」；端到端：SALE17 的 gold 过、评测那条错答红。
    #[test]
    fn code_hit_yields_require_code_on_column() {
        // 声明侧大小写与真库不一致是常态；规则一律小写。第三行是短码（另一条断言用）
        let rows = vm(&[
            ("T_Customer", "Province", "湖南", "430000"),
            ("t_customer", "province", "湖北", "420000"),
            ("t_sales_order_detail", "item_type", "商品行", "1"),
        ]);
        let rules = code_rules("本月湖南省的销售额是多少", &rows);
        let [CaliberRule::RequireCodeOnColumn { table, col, code, human }] = &rules[..] else {
            panic!("{rules:?}");
        };
        assert_eq!((table.as_str(), col.as_str(), code.as_str()), ("t_customer", "province", "430000"));
        assert!(human.contains("湖南") && human.contains("t_customer.province"), "{human}");
        // 端到端① gold 走地区表按名过滤：码压根没出现 → 不判（这正是「宁缺毋滥」那一侧）
        let gold = "SELECT SUM(o.total_amount) FROM t_sales_order o \
            JOIN t_customer c ON c.customer_code = o.customer_code \
            JOIN t_regions r ON r.region_code = c.province \
            WHERE r.region_name LIKE '%湖南%' AND o.deleted_flag = 0";
        assert!(dms_kernel::check_caliber(gold, &rules).is_empty());
        // 端到端② 直接换码写在声明列上，也是正确写法 → 不判
        let by_code = "SELECT SUM(o.total_amount) FROM t_sales_order o \
            JOIN t_customer c ON c.customer_code = o.customer_code WHERE c.province = '430000'";
        assert!(dms_kernel::check_caliber(by_code, &rules).is_empty());
        // 端到端③ 评测实际答出的那条：码抄对了、列换成了订单表上名字相近的那一列 → 红
        let wrong = "SELECT SUM(o.total_amount) FROM t_sales_order o \
            WHERE o.receiver_province = '430000' AND o.deleted_flag = 0";
        let v = dms_kernel::check_caliber(wrong, &rules);
        assert_eq!(
            v.iter().map(|x| x.rule.as_str()).collect::<Vec<_>>(),
            ["require_code_on_column:t_customer.province"]
        );
        assert!(v[0].hint.contains("receiver_province"), "{:?}", v[0]);
    }

    /// 🔴 三道闸各自单独会红：短码不造 / 问句没命中名字不造 / 同码多列不造
    #[test]
    fn code_rules_refuse_the_three_ambiguous_cases() {
        // ① 短码：'1' 在任何 SQL 里都可能出现（状态、标志、rn = 1），按它判必然误伤
        let short = vm(&[("t_sales_order_detail", "item_type", "商品行", "1")]);
        assert!(code_rules("本月商品行的销量", &short).is_empty());
        for c in ["10", "01", "HM", "z001", "手抓饼"] {
            assert!(!code_is_distinctive(c), "{c}");
        }
        for c in ["430000", "Z001", "ZZ05", "108"] {
            assert!(code_is_distinctive(c), "{c}");
        }
        // ② 问句没命中该取值的名字 → 这个码不是从问句来的，一条不造
        let prov = vm(&[("t_customer", "province", "湖南", "430000")]);
        assert!(code_rules("本月销售额是多少", &prov).is_empty());
        assert_eq!(code_rules("本月湖南的销售额", &prov).len(), 1);
        // ③ 同一个码登记在两个 (表, 列) 上 → 用了其中一个就必然违反另一条，一条不造
        let dup = vm(&[
            ("t_customer", "province", "湖南", "430000"),
            ("t_sales_order", "receiver_province", "湖南", "430000"),
        ]);
        assert!(code_rules("本月湖南的销售额", &dup).is_empty());
    }

    fn vm5(v: &[(&str, &str, &str, &str, &str)]) -> Vec<(String, String, String, String, String)> {
        v.iter()
            .map(|(t, c, n, k, o)| {
                (t.to_string(), c.to_string(), n.to_string(), k.to_string(), o.to_string())
            })
            .collect()
    }

    /// 🔴 值不在码表 → SQL 合法 → 三段闸门放行 → 执行成功 → **返 0 行**：
    /// 无报错、无告警、route 正常、`caliber_note` 为空，用户读成「本月没有这类客户」。
    /// 此前七条判据一条都管不到它（它们只看列有没有被约束，不看值）。
    #[test]
    fn unknown_value_on_a_fully_enumerated_code_column_is_flagged() {
        use crate::ddl::{VALUE_ORIGIN_PROBE, VALUE_ORIGIN_SEED};
        // 声明侧大小写与真库不一致是常态（同一个 (表,列) 的三行分两种写法）；规则一律小写
        let rows = vm5(&[
            ("T_Customer", "Customer_Class", "货架店铺", "01", VALUE_ORIGIN_DICT),
            ("t_customer", "customer_class", "线下客户", "04", VALUE_ORIGIN_DICT),
            ("t_customer", "customer_class", "其他财务专用", "06", VALUE_ORIGIN_DICT),
        ]);
        let rules = enum_rules(&rows, &tables(&["t_customer"]));
        let [CaliberRule::RequireKnownValue { table, col, values, human }] = &rules[..] else {
            panic!("{rules:?}");
        };
        assert_eq!((table.as_str(), col.as_str()), ("t_customer", "customer_class"));
        // 按码排序（加载 SQL 无 ORDER BY，判词必须逐次一致）
        assert_eq!(
            values,
            &[
                ("货架店铺".to_string(), "01".to_string()),
                ("线下客户".to_string(), "04".to_string()),
                ("其他财务专用".to_string(), "06".to_string()),
            ]
        );
        assert!(human.contains("完整") && human.contains("t_customer.customer_class"), "{human}");
        // 端到端① 三种正确写法（码 / 登记过的中文名 / IN 列表）一条都不许判 —— 这是最贵的假红
        for ok in [
            "SELECT COUNT(*) FROM t_customer c WHERE c.customer_class = '04' AND c.deleted_flag = 0",
            "SELECT COUNT(*) FROM t_customer c WHERE c.customer_class = '线下客户'",
            "SELECT COUNT(*) FROM t_customer c WHERE c.customer_class IN ('01','04')",
        ] {
            assert!(dms_kernel::check_caliber(ok, &rules).is_empty(), "{ok}");
        }
        // 端到端② 那条静默错答：码表里压根没有「线上客户」→ 执行成功且返 0 行
        let bad = "SELECT COUNT(DISTINCT c.customer_code) FROM t_customer c \
                   WHERE c.customer_class = '线上客户' AND c.deleted_flag = 0";
        // 🔴 防恒真：`check_caliber` 解析失败会**返空**（漏判方向）—— 那样下面两条断言都会
        // 「因为看不懂而绿」。先钉住它真的解析动了。本仓已多次踩「入参变空 → 断言恒真」。
        assert!(dms_kernel::sql::caliber::output_shape(bad).is_some(), "解析不动 → 判据恒返空");
        let v = dms_kernel::check_caliber(bad, &rules);
        assert_eq!(
            v.iter().map(|x| x.rule.as_str()).collect::<Vec<_>>(),
            ["require_known_value:t_customer.customer_class"]
        );
        // 判词必须点名那个不存在的值 + 列出合法取值（那是 LLM 唯一能据以改对的信息）
        assert!(v[0].hint.contains("线上客户") && v[0].hint.contains("线下客户=04"), "{:?}", v[0]);
        // 🔴 ③ 来源不是 dict → 一条都不造：手写种子只播了会用到的那几个取值、名称型探针
        // 2000 封顶会截断，对它们开火就是给一堆本来正确的 SQL 判红
        for origin in [VALUE_ORIGIN_SEED, VALUE_ORIGIN_PROBE] {
            let other = vm5(&[
                ("t_customer", "customer_class", "货架店铺", "01", origin),
                ("t_customer", "customer_class", "线下客户", "04", origin),
            ]);
            // 召回表照给（与上面那条逐字相同）—— 否则「空」可能只是因为没召回到，
            // 那样这条断言就不再是在验 origin 了
            assert!(
                enum_rules(&other, &tables(&["t_customer"])).is_empty(),
                "origin={origin} 不许造规则"
            );
        }
        // ④ 歧义即弃：同一列名在两张表上、取值集**不同** → 相关的一条都不造
        //（kernel 只记列名不记前缀，用了 A 的合法值会被 B 判红 —— 判据自己造的假红）
        let split = vm5(&[
            ("t_a", "x_code", "甲", "01", VALUE_ORIGIN_DICT),
            ("t_b", "x_code", "乙", "02", VALUE_ORIGIN_DICT),
        ]);
        assert!(enum_rules(&split, &tables(&["t_a", "t_b"])).is_empty(), "{split:?}");
        // 取值集相同（同一本字典注册到多张表的同类列上，实测常态）→ 照造，不许连带丢掉
        let same = vm5(&[
            ("t_a", "x_code", "甲", "01", VALUE_ORIGIN_DICT),
            ("t_b", "x_code", "甲", "01", VALUE_ORIGIN_DICT),
        ]);
        assert_eq!(enum_rules(&same, &tables(&["t_a", "t_b"])).len(), 2);
        // ⑤ 名或码有一侧为空的脏行不进取值集（空串会让「值是空字符串」变成合法值）
        let dirty = vm5(&[
            ("t_a", "y_code", "甲", "01", VALUE_ORIGIN_DICT),
            ("t_a", "y_code", "", "02", VALUE_ORIGIN_DICT),
        ]);
        let dr = enum_rules(&dirty, &tables(&["t_a"]));
        let [CaliberRule::RequireKnownValue { values, .. }] = &dr[..] else { panic!("{dr:?}") };
        assert_eq!(values.len(), 1);
    }

    /// 🔴 召回表过滤（与 `rules_from` 同口径）：**两侧都判** —— 只写一侧的话，把过滤写成
    /// 恒真或恒假都能全绿。
    ///
    /// 它治的是血量不是正确性（kernel 侧本来就要求声明的表在 FROM/JOIN 里）：连库实测 dict 那批
    /// 有 **68 个 (表,列) / 775 个取值**（`company_code` 一列就在 15 张表上各 31 个），
    /// 不过滤时每个问句 68 条规则，而 `run.rs:225` 的 `detail = ?r` 会把 775 个 `(名,码)` 对
    /// 按 Debug 打进 INFO。
    #[test]
    fn enum_rules_only_for_recalled_tables() {
        let rows = vm5(&[
            ("T_Customer", "customer_class", "线下客户", "04", VALUE_ORIGIN_DICT),
            ("T_Customer", "customer_class", "货架店铺", "01", VALUE_ORIGIN_DICT),
        ]);
        // ① 召回到 → 必须造（声明侧大小写与真库不一致是常态，故大小写无关）
        assert_eq!(enum_rules(&rows, &tables(&["t_sales_order", "T_CUSTOMER"])).len(), 1);
        // ② 没召回到 → 一条不造
        assert!(enum_rules(&rows, &tables(&["t_sales_order"])).is_empty());
        assert!(enum_rules(&rows, &[]).is_empty());
        // ③ 🔴 歧义判定仍跑**全量声明**，不是召回后的子集：kernel 只比列名不比前缀
        // （`known_value` ④），SQL 把没被召回的 t_b 也 JOIN 进来时（LLM 不受召回约束），
        // 用 t_b 的合法值会被 t_a 的规则判红。先裁后判就看不见 t_b —— 这条钉住顺序。
        let split = vm5(&[
            ("t_a", "x_code", "甲", "01", VALUE_ORIGIN_DICT),
            ("t_b", "x_code", "乙", "02", VALUE_ORIGIN_DICT),
        ]);
        assert!(enum_rules(&split, &tables(&["t_a"])).is_empty(), "{split:?}");
    }

    /// `join_edge` 的 card → 重复键清单：`N:1` 取左、`1:N` 取右、`1:1` 不产键。
    /// 取错侧 = 把「主档侧」判成扇出源，误伤每天的正确写法（`FROM 订单 JOIN 客户`）。
    #[test]
    fn fanout_keys_picks_the_many_side() {        let edges = vec![
            JoinEdge { lt: "a".into(), lc: "x".into(), rt: "b".into(), rc: "y".into(), card: "N:1".into() },
            JoinEdge { lt: "c".into(), lc: "u".into(), rt: "d".into(), rc: "v".into(), card: "1:N".into() },
            JoinEdge { lt: "e".into(), lc: "k".into(), rt: "f".into(), rc: "k2".into(), card: "1:1".into() },
        ];
        let keys = fanout_keys(&edges);
        assert_eq!(keys, [("a".to_string(), "x".to_string()), ("d".to_string(), "v".to_string())]);
        // 排序去重：同一条键在两条边上出现只留一份（判词去重依赖它）
        let dup = vec![
            JoinEdge { lt: "a".into(), lc: "x".into(), rt: "b".into(), rc: "y".into(), card: "N:1".into() },
            JoinEdge { lt: "a".into(), lc: "x".into(), rt: "g".into(), rc: "z".into(), card: "N:1".into() },
        ];
        assert_eq!(fanout_keys(&dup).len(), 1);
    }

    /// `time_cap='yesterday'` + 当期问法 → 造 `RequireTimeCap`；往期间法与空 cap 一律不造
    /// （问「上月」时 `< CURDATE()` 本就不必出现，造了就是误伤 —— 与 RequireTimeColumn 的闸同理）。
    #[test]
    fn time_cap_rule_only_for_current_period_questions() {
        let mut m = qty_metric();
        // 名字必须命中问句（`metric_rules` 先按名/别名召回，召回不到什么规则都不造）
        m.name = "延迟确认指标".into();
        m.aliases = vec![];
        m.time_cap = "yesterday".into();
        m.time_col = "confirmed_time".into();
        let ms = [m];
        let has_cap = |q: &str| {
            metric_rules(q, &ms).iter().any(|r| matches!(r, CaliberRule::RequireTimeCap { .. }))
        };
        assert!(has_cap("本月延迟确认指标"), "当期必须造");
        assert!(has_cap("今年延迟确认指标"), "当期必须造");
        assert!(!has_cap("上月延迟确认指标"), "往期间法造了就是误伤");
        assert!(!has_cap("2025年延迟确认指标"), "往年问法不造");
        // 空 cap 与空 time_col 同样不造
        let mut m2 = qty_metric();
        m2.time_col = "delivery_time".into();
        assert!(!metric_rules("本月销量", &[m2]).iter()
            .any(|r| matches!(r, CaliberRule::RequireTimeCap { .. })), "空 time_cap 不造");
    }

    /// `RequireCodeEq` 的构造：名≠码才收（name=code 的名称型值域不是证据）、按召回表过滤
    /// （没召回到的表不造，与 enum_rules 同一条闸）。
    #[test]
    fn code_eq_rules_collect_only_name_neq_code_and_recalled_tables() {
        let rows: Vec<(String, String, String, String)> = vec![
            ("t_customer".into(), "province".into(), "湖南".into(), "430000".into()),
            ("t_customer".into(), "province".into(), "湖北".into(), "420000".into()),
            ("t_goods_category".into(), "category_name".into(), "手抓饼".into(), "手抓饼".into()), // name=code：不收
        ];
        let rules = code_eq_rules(&rows, &tables(&["t_customer"]));
        assert_eq!(rules.len(), 1, "{rules:?}");
        let CaliberRule::RequireCodeEq { table, col, values, .. } = &rules[0] else { panic!("{rules:?}") };
        assert_eq!((table.as_str(), col.as_str()), ("t_customer", "province"));
        assert_eq!(values.len(), 2, "名≠码的两条都要收：{values:?}");
        // 没召回到 → 一条不造
        assert!(code_eq_rules(&rows, &tables(&["t_goods_category"])).is_empty());
    }
}
