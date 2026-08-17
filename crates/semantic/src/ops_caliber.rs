//! 运营看板口径（逐项血缘版 v0.1.19）。
//!
//! 文档权威源：`运营看板_各指标计算逻辑及数据来源_逐项血缘版_v0.1.19.docx`。
//! 其中一部分字段来自观远数据集加工层，不能伪装成 DMS 物理列。本模块只把能从 DMS
//! 源码与只读库严格还原的口径注册为可执行指标；折算人数、月末门店总盘等外部口径注册为
//! 业务术语，让模型明确数据源边界，禁止拿相似列猜算。

use crate::registry::datasource::DMS_DS_ID;
use sqlx::PgPool;

const DOC_VERSION: &str = "运营看板逐项血缘版 v0.1.19";

/// 运营看板口径起算日（v0.1.19：只纳入该日及以后的数据）。
/// `activity_valid`/`inspection_valid` 的 SQL 与 `TERMS` 文案三处共用（改口径只改这里）。
const OPS_EPOCH: &str = "2026-06-01";

struct Metric {
    code: &'static str,
    name: &'static str,
    aliases: &'static [&'static str],
    source: &'static str,
    agg: String,
    scope: &'static str,
    time_col: &'static str,
    description: &'static str,
    unit: &'static str,
}

/// 省份→23 个标准省区的 SQL CASE。**由 `warehouse_catalog::standard_region_pairs` 生成**，
/// 不再手抄。
///
/// 🔴 手抄那版漏了**上海与海南**（权威表里分别归浙江省区、广东省区），
/// 而 `inspection_valid` 里那句 `IS NOT NULL` 会把映射不出来的行**整批静默排除** ——
/// 于是「本月上海的巡店次数」恒 0、「今年各省区巡店次数」全国合计偏低，一个字的提示都没有。
/// 无法映射的（港澳台）仍然排除，不编造「其他」——那是 fail-closed，不是漏。
fn province_region(col: &str) -> String {
    let whens: String = region_pairs()
        .iter()
        .map(|(province, region)| format!("WHEN {col} REGEXP '{province}' THEN '{region}' "))
        .collect();
    format!("CASE {whens}END")
}

/// 省区值域（23 个短名，去重后保持首见顺序 —— 顺序即行为）。`activity_region` 的 IN 列表用它。
fn standard_regions() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = vec![];
    for (_, region) in region_pairs() {
        if !out.contains(&region) {
            out.push(region);
        }
    }
    out
}

/// 单点取权威对照（本文件三处共用一份；改名只改这里）
fn region_pairs() -> Vec<(&'static str, &'static str)> {
    crate::warehouse_catalog::standard_region_pairs()
}


fn activity_region(alias: &str) -> String {
    let fallback = province_region(&format!("{alias}.store_province"));
    // REPLACE 归一链存局部复用（原来 IN 列表与 THEN 各写一份，漂移风险）
    let normalized = format!("REPLACE(REPLACE({alias}.department_name,'省区',''),'大区','')");
    // 值域与 CASE 同源（原来 IN 列表是手抄的第三份，与 CASE 早已漂移）
    let in_list = standard_regions()
        .iter()
        .map(|r| format!("'{r}'"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "CASE \
         WHEN {alias}.department_name IN ('苏南大区','苏北大区','江苏省区') THEN '江苏' \
         WHEN {normalized} IN ({in_list}) \
         THEN {normalized} \
         ELSE {fallback} END"
    )
}

fn activity_valid(alias: &str) -> String {
    format!(
        "{alias}.deleted_flag = 0 AND {alias}.status <> '0' \
         AND {alias}.start_date >= '{OPS_EPOCH}' AND ({}) IS NOT NULL",
        activity_region(alias)
    )
}

/// ⚠️ 已知口径相互作用（2026-08-14 记账，**待业务裁决**）：下面的排除子查询读 `t_employee`，
/// 而该表已按 Java `EmployeeDao.java:35` 的 `@DataScope` 登记成 scoped（修的是「全员花名册
/// 对受限账号可见」这个确定的越权）。注入是递归的，子查询同样会被加上
/// `t_employee.employee_id IN (…)` —— 于是**受限身份**看运营看板时，
/// 「巡店人是三方/副总则不计入」这条排除只对他可见的员工生效，方向是**少排除、数字偏高**。
///
/// 为什么先这样：花名册泄漏是**确定的**越权（任何受限账号一句话就能拿到全量姓名/登录名/
/// 部门），而口径漂移只在「受限身份 + 运营看板」这个交叉面上出现（OPS 题集今天是 admin 跑的）。
/// 两害相权先堵越权，并把这条相互作用显式写下来 —— 静默接受才是仓内反复记录的那类坏账。
///
/// 彻底解法有两条，都要业务点头：① 把三方/副总排除名单在口径装载期物化成常量清单
/// （与权限无关，口径恒定）；② 该子查询走豁免通道（需要注入器支持「口径子查询不吃行权限」
/// 这个新概念，那是 kernel 的语义扩展，不该顺手加）。`ops_caliber_notes_employee_scope`
/// 钉板守着这段说明不被静默删掉。
fn inspection_valid(alias: &str) -> String {
    format!(
        "{alias}.deleted_flag = 0 AND {alias}.inspection_date >= '{OPS_EPOCH}' \
         AND ({}) IS NOT NULL AND NOT EXISTS (\
           SELECT 1 FROM t_employee oe JOIN t_position op ON op.position_id = oe.position_id \
           WHERE oe.login_name = {alias}.inspector AND oe.deleted_flag = 0 AND op.deleted_flag = 0 \
             AND (op.position_name LIKE '%三方%' OR op.position_name LIKE '%副总%'))",
        province_region(&format!("{alias}.province"))
    )
}

fn time_and(question: Option<&str>, col: &str) -> String {
    match question.and_then(dms_kernel::nl::time::time_predicate) {
        Some(tpl) => format!(" AND {}", dms_kernel::nl::time::fill_time_col(&tpl, col)),
        None => format!(" /* 时间条件必须加在这一行：{col} */"),
    }
}

/// 问句里的省/省区 → 标准省区短名。**同一份对照的第三种形态**（另两种是 SQL 的 CASE
/// 与 `activity_region` 的 IN 列表），2026-08-14 一并收敛到
/// `warehouse_catalog::standard_region_pairs`。
///
/// 🔴 手抄那版和 CASE 一样漏了上海与海南 —— 于是「上海运营活动费用」既筛不出省区、
/// 又因为残留词判定而整条快路径不接。
///
/// 三类**只出现在问句里**的说法额外挂在这里（它们不是行政省名，映射表里没有）：
/// 「苏南大区/苏北大区/江苏省区」→ 江苏、「川渝藏」自身、「西北」自身。
fn region_of(question: &str) -> Option<(&'static str, &'static str)> {
    const EXTRA: &[(&str, &str)] = &[
        ("苏南大区", "江苏"), ("苏北大区", "江苏"), ("江苏省区", "江苏"),
        ("川渝藏", "川渝藏"), ("西北", "西北"),
    ];
    // 长词优先：「内蒙古自治区」必须先于「内蒙」命中，否则残留词判定会把「古自治区」当残留
    let mut table: Vec<(&'static str, &'static str)> = EXTRA.to_vec();
    table.extend(region_pairs());
    table.sort_by_key(|(word, _)| std::cmp::Reverse(word.chars().count()));
    table.into_iter().find(|(word, _)| question.contains(word))
}

fn region_and(region: Option<&str>, expr: String) -> String {
    region.map(|r| format!(" AND ({expr}) = '{r}'")).unwrap_or_default()
}

/// 活动/巡店域三个 agg 的共用片段：valid/time/region 一次算好
/// （原来 `activity_agg`/`promoter_agg`/`inspection_agg` 各自重复 format! 同一串）。
struct AggCtx {
    valid: String,
    time: String,
    region: String,
}

impl AggCtx {
    fn activity(question: Option<&str>, region: Option<&str>) -> Self {
        Self {
            valid: activity_valid("a"),
            time: time_and(question, "a.start_date"),
            region: region_and(region, activity_region("a")),
        }
    }

    fn inspection(question: Option<&str>, region: Option<&str>) -> Self {
        Self {
            valid: inspection_valid("r"),
            time: time_and(question, "r.inspection_date"),
            region: region_and(region, province_region("r.province")),
        }
    }
}

fn activity_agg(expr: &str, question: Option<&str>, region: Option<&str>) -> String {
    let c = AggCtx::activity(question, region);
    format!("(SELECT {expr} FROM t_activity_main a WHERE {}{}{})", c.valid, c.time, c.region)
}

fn promoter_agg(expr: &str, question: Option<&str>, region: Option<&str>) -> String {
    let c = AggCtx::activity(question, region);
    format!(
        "(SELECT {expr} FROM t_activity_promoter_fee p \
         JOIN t_activity_main a ON a.id = p.activity_id \
         WHERE p.deleted_flag = 0 AND {}{}{})",
        c.valid, c.time, c.region
    )
}

fn inspection_agg(expr: &str, question: Option<&str>, region: Option<&str>) -> String {
    let c = AggCtx::inspection(question, region);
    format!(
        "(SELECT {expr} FROM t_shop_inspection_records r WHERE {}{}{})",
        c.valid, c.time, c.region
    )
}

/// 注册指标的 agg 表达式：缺分支当场指出 code（原来裸 unwrap，panic 不带是哪个指标）。
fn expr_or_panic(code: &'static str) -> String {
    metric_expr(code, None, None).unwrap_or_else(|| panic!("metric_expr 缺分支：{code}"))
}

fn metric_expr(code: &str, question: Option<&str>, region: Option<&str>) -> Option<String> {
    Some(match code {
        // 与同族指标（sales/cost）的空集语义一致：空集返 0 而不是 NULL
        "ops_activity_sessions" => activity_agg(
            "COALESCE(SUM(CASE WHEN a.duration_days > 0 THEN a.duration_days ELSE GREATEST(DATEDIFF(a.end_date,a.start_date)+1,1) END),0)",
            question,
            region,
        ),
        "ops_activity_sales" => promoter_agg("COALESCE(SUM(p.actual_sales),0)", question, region),
        "ops_activity_cost" => activity_agg("COALESCE(SUM(a.total_amount),0)", question, region),
        "ops_activity_cost_ratio" => format!(
            "ROUND({} * 100.0 / NULLIF({},0),2)",
            activity_agg("SUM(a.total_amount)", question, region),
            promoter_agg("SUM(p.actual_sales)", question, region)
        ),
        "ops_activity_roi" => format!(
            "ROUND({} / NULLIF({},0),2)",
            promoter_agg("SUM(p.actual_sales)", question, region),
            activity_agg("SUM(a.total_amount)", question, region)
        ),
        "ops_promoter_day_price" => {
            promoter_agg("ROUND(SUM(p.total_amount)/NULLIF(SUM(p.work_days),0),2)", question, region)
        }
        "ops_inspection_count" => inspection_agg("COUNT(DISTINCT r.id)", question, region),
        "ops_inspected_shop_count" => inspection_agg(
            "COUNT(DISTINCT COALESCE(NULLIF(r.shop_code,''),NULLIF(r.shop_name,'')))",
            question,
            region,
        ),
        "ops_avg_display_sku" => inspection_agg("AVG(r.sku_count)", question, region),
        "ops_avg_freezer" => inspection_agg("AVG(r.display_freezer_count)", question, region),
        "ops_avg_sausage_price" => inspection_agg("AVG(r.sausage_retail_price)", question, region),
        "ops_avg_tart_shell_price" => inspection_agg("AVG(r.tart_shell_retail_price)", question, region),
        "ops_avg_tart_liquid_price" => inspection_agg("AVG(r.tart_liquid_retail_price)", question, region),
        _ => return None,
    })
}

fn metrics() -> Vec<Metric> {
    vec![
        Metric {
            code: "ops_activity_sessions", name: "运营活动场次",
            aliases: &["运营看板活动场次", "线下运营活动场次", "按持续天数折算的活动场次"],
            source: "t_activity_main",
            agg: expr_or_panic("ops_activity_sessions"),
            scope: "", time_col: "start_date",
            description: "有效活动持续天数之和；一条跨多日活动折算为多场。只纳入 status<>'0'、标准23省区，日期按活动开始日。与通用DMS指标“活动场次”(按活动编号去重)不是同一口径。",
            unit: "",
        },
        Metric {
            code: "ops_activity_sales", name: "运营活动销售额",
            aliases: &["活动台账销售额", "线下活动销售额", "运营看板销售额"],
            source: "t_activity_promoter_fee / t_activity_main",
            agg: expr_or_panic("ops_activity_sales"), scope: "", time_col: "start_date",
            description: "活动销售额来自促销员明细 actual_sales 求和，不是订单销售额，也不在活动主表 total_amount 中。有效活动=status<>'0'且属于标准23省区。",
            unit: "",
        },
        Metric {
            code: "ops_activity_cost", name: "运营活动费用",
            aliases: &["六项活动费用", "活动六项费用", "运营看板活动费用"],
            source: "t_activity_main",
            agg: expr_or_panic("ops_activity_cost"), scope: "", time_col: "start_date",
            description: "六项费用合计：促销员+试吃+物料+场地+运输+其他。DMS只读对拍97条有效活动：主表 total_amount 与六张费用明细逐项合计差异0，可走主表快速聚合。",
            unit: "",
        },
        Metric {
            code: "ops_activity_cost_ratio", name: "活动费比",
            aliases: &["运营活动费比", "活动费用率", "运营看板费比"],
            source: "t_activity_main / t_activity_promoter_fee",
            agg: expr_or_panic("ops_activity_cost_ratio"),
            scope: "", time_col: "start_date",
            description: "整体活动费比=有效活动六项费用总和÷活动销售额总和，不是逐活动费比的算术平均；销售额为0时返回空值。",
            unit: "percent",
        },
        Metric {
            code: "ops_activity_roi", name: "运营活动ROI",
            aliases: &["活动ROI", "整体ROI", "运营看板平均ROI"],
            source: "t_activity_main / t_activity_promoter_fee",
            agg: expr_or_panic("ops_activity_roi"),
            scope: "", time_col: "start_date",
            description: "活动主卡ROI=总销售额÷总费用，是整体加权ROI；优秀案例模块的平均ROI才是单条ROI算术平均，两者不可互换。",
            unit: "",
        },
        Metric {
            code: "ops_promoter_day_price", name: "促销员人天单价",
            aliases: &["临促人天单价", "活动人天单价"],
            source: "t_activity_promoter_fee / t_activity_main",
            agg: expr_or_panic("ops_promoter_day_price"),
            scope: "", time_col: "start_date",
            description: "促销员费用合计÷临促人天合计；分母为0时返回空值，不按0处理。DMS服务同口径汇总 total_amount 与 work_days。",
            unit: "",
        },
        Metric {
            code: "ops_inspection_count", name: "运营巡店次数",
            aliases: &["有效巡店次数", "巡店记录数", "运营看板巡店次数"],
            source: "t_shop_inspection_records",
            agg: expr_or_panic("ops_inspection_count"), scope: "", time_col: "inspection_date",
            description: "巡店按业务日期统计、按非空巡店ID去重，并排除职位含三方/副总的人员；省份必须能归一到标准23省区。DMS物理表主键id对应数据集inspection_id。",
            unit: "",
        },
        Metric {
            code: "ops_inspected_shop_count", name: "巡店门店数",
            aliases: &["去重巡店门店数", "巡过的门店数"],
            source: "t_shop_inspection_records",
            agg: expr_or_panic("ops_inspected_shop_count"),
            scope: "", time_col: "inspection_date",
            description: "先执行有效巡店过滤，再按门店编码优先、名称兜底去重。",
            unit: "",
        },
        Metric {
            code: "ops_avg_display_sku", name: "平均陈列SKU",
            aliases: &["平均陈列SKU数", "陈列SKU均值"], source: "t_shop_inspection_records",
            agg: expr_or_panic("ops_avg_display_sku"), scope: "", time_col: "inspection_date",
            description: "有效巡店范围内 sku_count 非空值的算术平均；0参与均值。", unit: "",
        },
        Metric {
            code: "ops_avg_freezer", name: "平均冰柜数",
            aliases: &["陈列冰柜均值", "平均陈列冰柜数"], source: "t_shop_inspection_records",
            agg: expr_or_panic("ops_avg_freezer"), scope: "", time_col: "inspection_date",
            description: "有效巡店范围内 display_freezer_count 非空值的算术平均；0参与均值。", unit: "",
        },
        Metric {
            code: "ops_avg_sausage_price", name: "烤肠均价",
            aliases: &["烤肠平均零售价"], source: "t_shop_inspection_records",
            agg: expr_or_panic("ops_avg_sausage_price"), scope: "", time_col: "inspection_date",
            description: "只排除空值，数值0仍参与均值；文档明确“非0”标题与实际代码有差异。", unit: "",
        },
        Metric {
            code: "ops_avg_tart_shell_price", name: "蛋挞皮均价",
            aliases: &["蛋挞皮平均零售价"], source: "t_shop_inspection_records",
            agg: expr_or_panic("ops_avg_tart_shell_price"), scope: "", time_col: "inspection_date",
            description: "有效巡店范围内蛋挞皮零售价非空值平均，0参与。", unit: "",
        },
        Metric {
            code: "ops_avg_tart_liquid_price", name: "蛋挞液均价",
            aliases: &["蛋挞液平均零售价"], source: "t_shop_inspection_records",
            agg: expr_or_panic("ops_avg_tart_liquid_price"), scope: "", time_col: "inspection_date",
            description: "有效巡店范围内蛋挞液零售价非空值平均，0参与。", unit: "",
        },
    ]
}

/// 指标集进程内只构建一次（每条 agg 含多次 format!；原来 `direct_metric` 每问句重建、
/// `seed_metrics` 每轮构建两遍）。
fn metrics_cached() -> &'static [Metric] {
    static MS: std::sync::OnceLock<Vec<Metric>> = std::sync::OnceLock::new();
    MS.get_or_init(metrics)
}

/// 运营口径的无维度确定性命中。只承接“一个指标 + 可选时间窗”；任何省区、城市、
/// 客户等未兑现限定都会被残留守卫拒绝，交回完整组合/LLM 链。
pub fn direct_metric(question: &str) -> Option<(String, String)> {
    direct_metric_with_evidence(question).map(|(sql, name, _)| (sql, name))
}

/// 同上，外带**本条 SQL 已经兑现了哪些槽位**的证明。
///
/// 🔴 为什么必须显式声明（2026-08-14 回归 OPS04）：这条 SQL 是**代码写死**的，
/// 时间窗写成字面日期（`a.start_date >= '2026-06-01'`）、省区写成 `CASE(...) = '湖南'`，
/// 而覆盖闸是按 LLM SQL 的形状判的 —— 它认不出这两样，于是判
/// `missing: time:2026年6月` 直接回落，整条运营口径路**平时根本不生效**。
/// 声明的是「代码确实消化了这个槽」这一事实，不是放宽判据（销售快路径早就这么做，
/// 见 `fastpath_intent` 里 Region/Time 两处 `resolve`）。
pub fn direct_metric_with_evidence(
    question: &str,
) -> Option<(String, String, crate::direct_types::ExecutionEvidence)> {
    let ms = metrics_cached();
    let (m, hit) = ms
        .iter()
        .filter_map(|m| {
            let aliases = m.aliases.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            dms_kernel::nl::text::match_word(question, m.name, &aliases).map(|w| (m, w))
        })
        // 词长平手时按名字字典序取最大：语义任意但**确定性**是有意的（同问句同结果）
        .max_by_key(|(m, w)| (w.chars().count(), m.name))?;
    let region = region_of(question);
    // 「吗」「了」「呢」是本清单的增量；「总共/一共」已在 STRIP_WORDS 里（has_residue_with 会再剥）
    let mut consumed = vec![hit, "呢".into(), "吗".into(), "了".into()];
    if let Some((word, _)) = region {
        consumed.push(word.into());
    }
    consumed.push(m.name.into());
    consumed.extend(m.aliases.iter().map(|s| s.to_string()));
    if dms_kernel::nl::text::has_residue_with(
        question,
        &consumed,
        dms_kernel::nl::lexicon::STRIP_WORDS,
    ) {
        return None;
    }
    let expr = metric_expr(m.code, Some(question), region.map(|(_, normalized)| normalized))?;
    let mut evidence = crate::direct_types::ExecutionEvidence::default()
        .resolve(crate::direct_types::IntentSlotKind::Metric, m.name);
    if let Some((word, _)) = region {
        evidence = evidence.resolve(crate::direct_types::IntentSlotKind::Region, word);
    }
    // 时间窗由 `metric_expr` 里的 `time_and` 真的拼进了 SQL（没有时间词时它写的是注释占位，
    // 那种情况下 `time_phrase_of` 也返 None，两边一致）
    // 用与销售快路径同一份表面词提取（相对词 / 两个 ISO 日期 / 显式年月），
    // **不用**它的整句兜底：那会把没处理的限定一起当成已消化
    if let Some(surface) = crate::fastpath::intent_time_surface(question) {
        if surface != question {
            evidence = evidence.resolve(crate::direct_types::IntentSlotKind::Time, surface);
        }
    }
    Some((format!("SELECT {expr} AS `{}`", m.name), m.name.into(), evidence))
}

const TERMS: &[(&str, &str, &[&str])] = &[
    ("运营看板数据范围", "v0.1.19：月份来自巡店 inspection_date 与活动 start_date 并集，只纳入2026-06-01及以后；查询继承当前账号行权限；单个观远数据集最多60000行。", &["线下营运看板口径"]),
    ("标准省区", "固定23个：福建、贵州、广东、云南、湖南、河北、天津、江苏、北京、浙江、湖北、川渝藏、山西、山东、河南、广西、江西、安徽、吉林、辽宁、黑龙江、内蒙、西北。苏南/苏北/江苏省区统一为江苏；无法归一的数据排除。", &["省区归一化"]),
    ("有效活动", "活动开始日期属所选月、归一后属于标准省区、status!='0'。活动场次按持续天数折算，不等于台账行数。", &["活动有效记录"]),
    ("有效巡店", "巡店日期属所选月、归一后属于标准省区、职位不含三方或副总、按非空inspection_id去重；空ID不参与ID去重。", &["巡店有效记录"]),
    ("折算人数", "来源观远城市经理人数数据集 k41cea4fcbc1e4824824ca9f：按姓名去重，重复行的折算人数取最大值；业务月缺失时取人数表最新月。DMS t_employee没有该加工字段，禁止用员工行数替代。", &["城市经理折算人数"]),
    ("门店总盘", "来源观远门店总盘数据集 s0cbb642853b0466cad50330：优先当月自然月末快照；最新月尚无月末快照时取当月update_time最新快照。禁止用t_master_shop当前行数替代历史月快照。", &["门店总盘口径"]),
    ("巡店覆盖率", "去重巡店门店数÷门店总盘；分子走有效巡店，分母走对应省区的月末/最新快照。分母来自外部数据集，未接入时不许猜算。", &["门店覆盖率"]),
    ("人均巡店次数", "有效巡店次数÷折算人数；人数为0显示“-”。折算人数来自城市经理人数数据集，不是DMS员工COUNT。", &["人均巡店"]),
    ("人均活动场次", "按持续天数折算的有效活动场次÷折算人数；人数按对应月，缺月取人数表最新月。", &["人均活动"]),
    ("大日期风险", "在 improvement_items 与 competitor_info 拼接文本中命中大日期/临期/过期/效期/保质期/日期不好/日期较久/日期不新鲜；每条巡店最多计1次。", &["效期风险", "临期提及"]),
    ("竞品提及", "只识别固定品牌、商品和动作词典；无/暂无/没有/无竞品/0/-等整段值无效；同一记录同一词最多计1次，词云最多32项。", &["竞品分析"]),
    ("巡店改进项", "improvement_items 按固定主题词典归类：陈列、日期、补货、缺货、排面、价格、库存、冰柜、位置、促销、动销、标签、断货、卫生；每主题每记录最多计1次。", &["改进项关键词"]),
    ("专柜门店占比", "仅识别专柜/整柜/整组柜/包柜/全柜/一整柜；按客户×省份×城市统计，专柜门店去重数÷全部巡店门店去重数。", &["整柜门店占比"]),
    ("优秀活动案例", "单条有效活动销售额>10000且单条ROI>=8；按活动台账记录判断，不按跨天折算场次复制。优秀案例平均ROI是单条ROI算术平均，与主卡整体ROI不同。", &["活动优秀案例"]),
    ("异常活动", "单条活动任一命中：费用0且销售>0、有费用但销售0、费比>100%、ROI<1、促销员人天单价>所在省区整体人天单价2倍。", &["活动异常"]),
    ("异常客户", "客户编码优先、名称兜底聚合；客户整体费比>50%，或有活动费用但整月0巡店，或有费用但销售0。", &["客户异常"]),
    ("异常门店", "门店编码优先、名称兜底。费用表主筛选：费比>100%或有费用但销售0；巡店异常：次数>所在省门店均值+3×总体标准差，或有活动但整月0巡店。", &["门店异常"]),
    ("异常城市经理", "人数表人员为全集：在职0巡店；个人巡店/折算人数低于事业群整体人均50%；或名下活动费比>50%/有费用但销售0。", &["城市经理异常"]),
];

const EDGES: &[(&str, &str, &str, &str, &str, &str)] = &[
    ("t_shop_inspection_records", "customer_code", "t_customer", "customer_code", "N:1", "巡店→客户；只读基数探测：客户有效主档键唯一"),
    ("t_activity_main", "customer_code", "t_customer", "customer_code", "N:1", "运营活动→客户；客户编码优先聚合"),
];

async fn seed_metrics(pg: &PgPool) -> anyhow::Result<()> {
    const ACTIVITY_DIMS: &[&str] = &["活动省区", "活动级别"];
    const INSPECTION_DIMS: &[&str] = &["巡店省区", "巡店城市"];
    // 单循环一次遍历（原来 insert/update 两循环各自重建一遍 metrics()）；
    // UPDATE 0 行 = insert 与 policy 写漂，warn 留痕
    for m in metrics_cached() {
        sqlx::query(
            "INSERT INTO meta.metric(ds_id,metric_code,name,aliases,source_table,agg_expr,scope_filter,time_col,dedup_keys,description,unit,time_cap)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'',$9,$10,'')
             ON CONFLICT (ds_id,metric_code) DO UPDATE SET name=$3,aliases=$4,source_table=$5,
               agg_expr=$6,scope_filter=$7,time_col=$8,dedup_keys='',description=$9,unit=$10,time_cap=''",
        )
        .bind(DMS_DS_ID).bind(m.code).bind(m.name)
        .bind(m.aliases.to_vec())
        .bind(m.source).bind(&m.agg).bind(m.scope).bind(m.time_col)
        .bind(format!("{}。依据：{DOC_VERSION}", m.description)).bind(m.unit)
        .execute(pg).await?;
        let dims = if m.code.starts_with("ops_activity") || m.code == "ops_promoter_day_price" {
            ACTIVITY_DIMS
        } else {
            INSPECTION_DIMS
        };
        let affected = sqlx::query("UPDATE meta.metric SET version=$1, allowed_dimensions=$2 WHERE ds_id=$3 AND metric_code=$4")
            .bind(DOC_VERSION)
            .bind(dims.to_vec())
            .bind(DMS_DS_ID)
            .bind(m.code)
            .execute(pg).await?
            .rows_affected();
        if affected == 0 {
            tracing::warn!("运营指标 policy 未命中行（code={} 与 insert 写漂？）", m.code);
        }
    }
    Ok(())
}

async fn seed_dimensions(pg: &PgPool) -> anyhow::Result<()> {
    let dims = [
        ("ops_activity_region", "活动省区", vec!["运营活动省区", "活动标准省区"], "t_activity_main a", activity_region("a"), "province_region优先、province兜底的23标准省区归一化"),
        ("ops_activity_level", "活动级别", vec!["活动类型", "活动规模"], "t_activity_main a", "COALESCE(CASE a.activity_level WHEN 1 THEN '小型' WHEN 2 THEN '中型' WHEN 3 THEN '大型' WHEN 4 THEN 'CP' END,'未分级')".into(), "DMS ActivityLevelEnum：1小型/2中型/3大型/4 CP"),
        ("ops_inspection_region", "巡店省区", vec!["巡店标准省区"], "t_shop_inspection_records r", province_region("r.province"), "巡店省份归一到运营看板23标准省区；无法映射排除"),
        ("ops_inspection_city", "巡店城市", vec!["巡店地级市"], "t_shop_inspection_records r", "COALESCE(NULLIF(r.city,''),'未知')".into(), "巡店分析模块内省份→地级市二次筛选"),
    ];
    for (code, name, aliases, source, expr, desc) in dims {
        sqlx::query(
            "INSERT INTO meta.dimension(ds_id,dim_code,name,aliases,source_table,expr,description)
             VALUES ($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT (ds_id,dim_code) DO UPDATE SET name=$3,aliases=$4,source_table=$5,expr=$6,description=$7",
        )
        .bind(DMS_DS_ID).bind(code).bind(name).bind(aliases).bind(source).bind(expr)
        .bind(format!("{desc}。依据：{DOC_VERSION}"))
        .execute(pg).await?;
    }
    Ok(())
}

async fn seed_terms(pg: &PgPool) -> anyhow::Result<()> {
    for (term, def, aliases) in TERMS {
        sqlx::query(
            "INSERT INTO meta.term(ds_id,term,definition,aliases) VALUES ($1,$2,$3,$4)
             ON CONFLICT (ds_id,term) DO UPDATE SET definition=$3,aliases=$4,status='active'",
        )
        .bind(DMS_DS_ID).bind(term).bind(format!("{def} 依据：{DOC_VERSION}"))
        .bind(aliases.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .execute(pg).await?;
    }
    Ok(())
}

async fn seed_graph_and_docs(pg: &PgPool) -> anyhow::Result<()> {
    for (lt, lc, rt, rc, card, note) in EDGES {
        sqlx::query(
            "INSERT INTO meta.join_edge(ds_id,left_table,left_col,right_table,right_col,card,note)
             VALUES ($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT (ds_id,left_table,left_col,right_table,right_col)
             DO UPDATE SET card=$6,note=$7,status='active'",
        )
        .bind(DMS_DS_ID).bind(lt).bind(lc).bind(rt).bind(rc).bind(card).bind(note)
        .execute(pg).await?;
    }
    for (table, filter, note) in [
        ("t_shop_inspection_records", "deleted_flag = 0", "巡店台账软删过滤；业务日期必须用inspection_date"),
        ("t_master_shop", "deleted_flag = 0", "门店主档软删过滤；历史重复门店码会被排除"),
        ("t_activity_main", "deleted_flag = 0", "活动主表软删过滤；status!='0'是运营指标级口径，不是所有活动查询的表级口径"),
    ] {
        sqlx::query(
            "INSERT INTO meta.table_scope(ds_id,table_name,filter,note) VALUES ($1,$2,$3,$4)
             ON CONFLICT (ds_id,table_name) DO UPDATE SET filter=$3,note=$4",
        )
        .bind(DMS_DS_ID).bind(table).bind(filter).bind(note).execute(pg).await?;
    }
    // 🔴 这里原来钉六个关键词到 `t_shop_inspection_records`，2026-08-17 全部退役，两个理由：
    //
    // ① **它们从来没生效过**：`forced_tables` 有一句
    //    `if !catalog_table(cx.ds, t) { continue; }`，而巡店表不在 `ASSETS` 白名单里 ——
    //    六条钉子一次都没命中过。实测「巡店记录有多少条」召回不到该表。
    //
    // ② **「大日期 / 临期」根本不属于巡店语境**：`meta.kw_force` 是**全局命名空间**
    //    （PK 只有 ds_id+keyword），本模块按运营看板的语境把这两个通用词据为己有。
    //    在通用业务语境里它们指的是**库存效期**（`scm_warehous_manage.invalid_date`），
    //    而不是巡店文本里命中「大日期」三个字。业主 2026-08-17 问
    //    「京东仓的大日期商品」正是撞在这上面。
    //    本模块的「大日期风险」指标（`meta.term`）保留 —— 那是巡店看板里的另一件事，
    //    有自己的定义，不需要靠劫持全局关键词来实现。
    //
    // 退役由 `seed.rs::KW_FORCE_RETIRED` 统一执行（要真删库里的旧行，不是不再写入就完事）。
    let docs = [
        ("t_shop_inspection_records", "运营巡店台账：业务日期inspection_date；客户/门店/省市；陈列SKU、冰柜、烤肠/蛋挞价格、竞品信息、改进项。运营统计须做标准省区归一、职位排除与ID去重。"),
        ("t_activity_main", "运营活动主表：开始/结束日期、持续天数、活动级别、客户/门店/经理。total_amount是六张费用明细合计；活动销售额不在本表，来自促销员明细actual_sales。"),
        ("t_activity_promoter_fee", "活动促销员明细：actual_sales=活动销售额，total_amount=促销员费用，work_days=临促人天；按activity_id关联活动主表。"),
    ];
    let mut doc_missed: Vec<&str> = vec![];
    for (table, comment) in docs {
        let affected = sqlx::query("UPDATE meta.table_doc SET custom_comment=$3 WHERE ds_id=$1 AND table_name=$2")
            .bind(DMS_DS_ID).bind(table).bind(format!("{comment} 依据：{DOC_VERSION}"))
            .execute(pg).await?
            .rows_affected();
        if affected == 0 {
            doc_missed.push(table);
        }
    }
    if !doc_missed.is_empty() {
        // 表名打错时静默 seed 空气（首次启动 table_doc 为空属合法，对照 seed.rs 同款模式）
        tracing::warn!("运营看板 custom_comment 未命中 table_doc 行：{doc_missed:?}");
    }
    Ok(())
}

pub(crate) async fn seed_ops_caliber(pg: &PgPool) -> anyhow::Result<()> {
    seed_metrics(pg).await?;
    seed_dimensions(pg).await?;
    seed_terms(pg).await?;
    seed_graph_and_docs(pg).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_version_and_core_formulas_are_pinned() {
        assert_eq!(DOC_VERSION, "运营看板逐项血缘版 v0.1.19");
        let ms = metrics();
        let sessions = ms.iter().find(|m| m.code == "ops_activity_sessions").unwrap();
        assert!(sessions.agg.contains("duration_days > 0") && sessions.agg.contains("DATEDIFF"));
        assert!(sessions.agg.contains("start_date >= '2026-06-01'"));
        let ratio = ms.iter().find(|m| m.code == "ops_activity_cost_ratio").unwrap();
        assert!(ratio.agg.contains("SUM(a.total_amount)") && ratio.agg.contains("SUM(p.actual_sales)"));
        assert_eq!(ratio.unit, "percent");
    }

    #[test]
    fn direct_metric_fills_time_and_rejects_unhandled_filters() {
        let (sql, name) = direct_metric("2026年6月运营活动场次是多少").unwrap();
        assert_eq!(name, "运营活动场次");
        assert!(sql.contains("a.start_date >= '2026-06-01'"));
        assert!(sql.contains("a.start_date < '2026-07-01'"));
        assert!(!sql.contains("时间条件必须加在这一行"));
        let hunan = direct_metric("2026年6月湖南运营活动场次").unwrap().0;
        assert!(hunan.contains("= '湖南'") && !hunan.contains("= '430000'"));
        let west = direct_metric("2026年6月陕西运营活动场次").unwrap().0;
        assert!(west.contains("= '西北'"));
        assert!(direct_metric("2026年6月长沙运营活动场次").is_none());
        // 🔴 回归 OPS04 的原问句（费用族 + 「是多少」尾巴）：它此前落到自由 SQL
        let cost = direct_metric("2026年6月湖南运营活动费用是多少").expect("OPS04 原问句必须走口径路");
        assert!(cost.0.contains("= '湖南'"), "{}", cost.0);
        // 🔴 代码写死的 SQL 必须**自己声明**兑现了哪些槽位：时间窗是字面日期、
        // 省区是 CASE 比较，覆盖闸按 LLM SQL 的形状认不出来 → 判 missing → 整条路被挡回去
        //（2026-08-14 实测 OPS04 就是这么掉的：`coverage=missing:["time:2026年6月"]`）。
        let (_, _, ev) = direct_metric_with_evidence("2026年6月湖南运营活动费用是多少").unwrap();
        use crate::direct_types::IntentSlotKind as K;
        assert!(ev.proves(K::Time, "2026年6月"), "时间窗没声明：覆盖闸会判 missing");
        assert!(ev.proves(K::Region, "湖南"), "省区没声明：覆盖闸会判 unverifiable");
        assert!(ev.proves(K::Metric, "运营活动费用"), "指标没声明");
        // 没有时间词时不许**凭空**声明（那才是放宽判据）
        let (_, _, bare) = direct_metric_with_evidence("运营活动费用是多少").unwrap();
        assert!(!bare.proves(K::Time, ""), "无时间词却声明了时间槽");
        // 🔴 让路门二：`compose_hit` 里的 `warehouse_sales_question` 若判真，
        // 整条 direct-agg 直接 return None —— 运营口径路连门都进不去。
        assert!(
            !crate::fastpath::warehouse_sales_question("2026年6月湖南运营活动费用是多少"),
            "运营问句被 warehouse_sales_question 判成销售问句 → compose_hit 直接让路"
        );
        // 🔴 让路门：运营口径路排在数仓销售快路径**之后**（`try_compose_metric_only` 首行），
        // 所以运营问句一旦被销售指标词表命中，就永远走不到这里。OPS04 实测就是这么掉的。
        assert!(
            crate::fastpath::warehouse_sales_metrics("2026年6月湖南运营活动费用是多少").is_empty(),
            "运营口径问句被数仓销售指标词表劫走了：{:?}",
            crate::fastpath::warehouse_sales_metrics("2026年6月湖南运营活动费用是多少")
        );
        assert!(direct_metric("2026年6月活动费比").unwrap().0.contains("NULLIF"));
    }

    #[test]
    fn external_snapshot_metrics_are_not_faked_as_mysql_metrics() {
        let names: Vec<&str> = metrics().iter().map(|m| m.name).collect();
        assert!(!names.contains(&"折算人数") && !names.contains(&"巡店覆盖率"));
        let coverage = TERMS.iter().find(|(t, _, _)| *t == "巡店覆盖率").unwrap().1;
        assert!(coverage.contains("未接入时不许猜算"));
    }

    #[test]
    fn generic_dms_activity_metrics_are_not_overwritten() {
        let names: Vec<&str> = metrics().iter().map(|m| m.name).collect();
        assert!(!names.contains(&"活动场次") && !names.contains(&"活动费用"));
    }

    #[test]
    fn non_unique_shop_master_is_not_claimed_as_many_to_one() {
        assert!(EDGES.iter().all(|(_, _, rt, _, _, _)| *rt != "t_master_shop"));
    }

    /// 无时间词问句的全时段语义钉住：直答照给，SQL 里留「时间条件必须加在这一行」提示注释
    /// （刻意不拒答 —— 拒答会把这类问句赶回 LLM 链，属行为变更，需评审）。
    #[test]
    fn timeless_question_runs_full_range_with_marker() {
        let (sql, _) = direct_metric("运营活动场次是多少").unwrap();
        assert!(sql.contains("时间条件必须加在这一行"), "无时间词的全时段直答语义变了：{sql}");
        assert!(sql.contains(OPS_EPOCH), "{sql}");
    }

    /// 维度组归属钉死：ops_activity_* + ops_promoter_day_price 归活动维度组，
    /// 其余归巡店组；新增前缀不符的活动类指标会在这里红（原来靠 starts_with 静默分流）。
    #[test]
    fn dims_assignment_is_pinned() {
        const ACTIVITY: &[&str] = &[
            "ops_activity_sessions", "ops_activity_sales", "ops_activity_cost",
            "ops_activity_cost_ratio", "ops_activity_roi", "ops_promoter_day_price",
        ];
        const INSPECTION: &[&str] = &[
            "ops_inspection_count", "ops_inspected_shop_count", "ops_avg_display_sku",
            "ops_avg_freezer", "ops_avg_sausage_price", "ops_avg_tart_shell_price",
            "ops_avg_tart_liquid_price",
        ];
        for m in metrics_cached() {
            let is_activity = m.code.starts_with("ops_activity") || m.code == "ops_promoter_day_price";
            assert_eq!(is_activity, ACTIVITY.contains(&m.code), "{} 的维度组归属变了", m.code);
            assert!(
                ACTIVITY.contains(&m.code) || INSPECTION.contains(&m.code),
                "新指标 {} 未登记维度组归属",
                m.code
            );
            // 顺带：每个注册 code 都有 metric_expr 分支（expr_or_panic 的运行期兜底印证）
            assert!(metric_expr(m.code, None, None).is_some(), "metric_expr 缺分支：{}", m.code);
        }
    }

    /// 巡店/活动域的 customer 血缘边钉住（删边无断言会红，对照 seed.rs 的血缘钉法）。
    #[test]
    fn ops_lineage_edges_are_pinned() {
        assert_eq!(EDGES.len(), 2, "巡店/活动域 customer 边数量变化需评审");
        assert!(EDGES.iter().any(|e| e.0 == "t_shop_inspection_records" && e.2 == "t_customer"));
        assert!(EDGES.iter().any(|e| e.0 == "t_activity_main" && e.2 == "t_customer"));
    }
}

#[cfg(test)]
mod scope_interaction {
    /// 🔴 `t_employee` 转 scoped 与运营口径排除子查询的相互作用必须**有记录**。
    /// 删掉那段说明 = 把一条已知的口径漂移变回静默（仓内反复抓到的那类坏账）。
    #[test]
    fn ops_caliber_notes_employee_scope() {
        let src = include_str!("ops_caliber.rs");
        let head = src.split("fn inspection_valid").next().expect("函数改名了");
        assert!(head.contains("待业务裁决"), "口径相互作用的记账没了");
        assert!(head.contains("EmployeeDao.java:35"), "缺 Java 侧证据出处");
    }
    /// 🔴 省区对照只有**一份**（`warehouse_catalog::standard_region_pairs`），
    /// 且上海/海南必须在里面。
    ///
    /// 由来：手抄的 CASE 漏了这两地，而 `inspection_valid` 的 `IS NOT NULL` 会把映射不出来的行
    /// **整批静默排除** —— 「本月上海的巡店次数」恒 0，没有任何提示。
    #[test]
    fn region_case_covers_every_mapped_province_including_shanghai_and_hainan() {
        let case = super::province_region("s.province");
        for (province, region) in super::region_pairs() {
            assert!(
                case.contains(&format!("REGEXP '{province}' THEN '{region}'")),
                "CASE 少了 {province}→{region}"
            );
        }
        // 两个特例逐字钉住（权威表就是这么记的）
        assert!(case.contains("REGEXP '上海' THEN '浙江'"), "上海归浙江省区：{case}");
        assert!(case.contains("REGEXP '海南' THEN '广东'"), "海南归广东省区：{case}");
        // 港澳台不许出现（生产映射里没有，fail-closed）
        for none in ["台湾", "香港", "澳门"] {
            assert!(!case.contains(none), "未映射省份不许进 CASE：{none}");
        }
        // 值域与 CASE 同源：activity_region 的 IN 列表逐字来自它
        let regions = super::standard_regions();
        assert_eq!(regions.len(), 23, "省区值域应为 23 个：{regions:?}");
        let activity = super::activity_region("a");
        for r in &regions {
            assert!(activity.contains(&format!("'{r}'")), "IN 列表少了 {r}");
        }
    }

}
