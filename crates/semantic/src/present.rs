//! 呈现算法与 DMS 中文词表：列语义推断 + 图表自动决策 + 结论洞察（搬运源 `server/src/viewspec.rs` 全文）。
//!
//! 类型与 serde 形状（前端字节兼容契约）在 `dms_kernel::present`，本文件只有算法与词表
//! ——它们全是 DMS 业务语料（销售额/省份/订单数/34 省码），kernel 不许有，落点就是这里。
//! 参考 SuperSonic chat-sdk：后端给列语义（showType/role），前端按语义智能选呈现形态。
//!
//! **顺序即行为**（D9）：明细/多维在趋势线之前、`infer_semantic` 的 Count 先于 Order、
//! 饼图阈值 10 / 柱 TOP20。拆函数只许提取不许重排。

use serde_json::Value;

pub use dms_kernel::present::*;

/// 默认下钻只给跨指标也相对稳定的维度，不主动诱导人员或门店口径。
const DEFAULT_DIM_POOL: &[&str] = &["省份", "商品分类", "客户", "月份"];
/// DWS 销售事实的业务确认维度；不从物理表的其他列自动扩展能力。
const DWS_SALES_DIM_POOL: &[&str] =
    &["省区", "战区", "客户", "客户编码", "商品", "商品编码", "销售日期", "月份"];
const DWS_SALES_METRICS: &[&str] =
    &["销售额", "销量", "不含税成本", "不含税收入", "毛利额", "毛利率"];

fn infer_drill(specs: &[ColumnSpec], has_metric: bool) -> Vec<String> {
    if !has_metric {
        return vec![]; // 无指标（明细/实体卡）不下钻
    }
    let used: String = specs.iter().map(|c| c.name.as_str()).collect();
    let pool = if specs
        .iter()
        .any(|c| DWS_SALES_METRICS.iter().any(|metric| c.name.contains(metric)))
    {
        DWS_SALES_DIM_POOL
    } else {
        DEFAULT_DIM_POOL
    };
    pool
        .iter()
        .filter(|d| {
            // 该维度名未在结果列名出现才建议。按**全名**判：两字符前缀会把「销售日期」
            // 错杀在「销售额」里（「销售」既是指标前缀也是维度前缀，实测撞过）。
            !used.contains(*d)
        })
        .map(|s| s.to_string())
        .collect()
}

/// 列名 → 语义
fn infer_semantic(name: &str) -> Semantic {
    if name.contains('率') || name.contains("占比") || name.contains('%') {
        Semantic::Percent
    } else if name.contains("金额") || name.contains("销售额") || name.contains("营业额") || name.contains("客单价")
        || name.contains("余额") || name.contains("费用") || name.contains("成本") || name.contains("收入")
        || name.ends_with('额')
    {
        Semantic::Money
    } else if name.contains("省") || name.contains("市") || name.contains("区县") || name.contains("地区") {
        Semantic::Geo
    } else if name.contains("客户") {
        Semantic::Customer
    } else if name.contains("商品") || name.to_ascii_lowercase().contains("sku") {
        // SKU 只小写化 ASCII（`Sku`/`sKu` 同族）；中文关键词不受影响
        Semantic::Goods
    } else if name.contains('数') || name.contains("销量") || name.ends_with('量') {
        // 「订单数/售后单数」是计数指标——必须先于 Order 判定，否则被"订单"抢走永远不算指标列
        Semantic::Count
    } else if name.contains("单号") || name.contains("订单") {
        Semantic::Order
    } else {
        Semantic::None
    }
}

/// 一列是否数值（所有非空值可解析为数字）
fn is_numeric_col(rows: &[Vec<Value>], i: usize) -> bool {
    let mut saw = false;
    for row in rows {
        match row.get(i) {
            Some(Value::Null) | None => {}
            Some(Value::Number(_)) => saw = true,
            Some(Value::String(s)) => {
                if s.trim().parse::<f64>().is_err() {
                    return false;
                }
                saw = true;
            }
            _ => return false,
        }
    }
    saw
}

/// 列名 + 数据 → role
fn infer_role(name: &str, rows: &[Vec<Value>], i: usize) -> Role {
    // ASCII 尾巴大小写无关（`下单DATE`/`EndTime`/`ORDER_ID` 同族）；中文关键词不受影响
    let lower = name.to_ascii_lowercase();
    // 时间（趋势线 x 轴）：含时间/日期关键词，或月份/季度/年月等时间维度
    if name.contains("时间") || name.contains("日期") || name.contains("月份") || name.contains("季度")
        || name.contains("年月") || lower.ends_with("date") || lower.ends_with("time")
    {
        return Role::Time;
    }
    // 编码/单号：名称信号 + 值非纯聚合数字
    if name.contains("编码") || name.contains("单号") || name.contains("编号") || lower.ends_with("code") || lower.ends_with("_id") {
        return Role::Id;
    }
    // 指标：名称含指标语义 且 列数值
    let sem = infer_semantic(name);
    let metric_sem = matches!(sem, Semantic::Money | Semantic::Count | Semantic::Percent);
    if metric_sem && is_numeric_col(rows, i) {
        return Role::Metric;
    }
    // 纯数值列（无指标名）保守归类别：短枚举类数值（如状态码）不该当指标。
    Role::Category
}

const PIE_MAX: usize = 10; // 对齐 SuperSonic 桌面阈值
const BAR_MAX: usize = 50;
const BAR_TOP: usize = 20; // 对齐 SuperSonic Trend slice(0,20)
const ENTITY_MIN_COLS: usize = 6;

/// 单指标 KPI 比较补丁：`delta` 保留第一项兼容旧前端；完整比较列表由 AskResult 承载。
pub fn patch_kpi_delta(view: &mut ViewSpec, cur: f64, prev: f64, label: String) {
    if let Some(Block::Kpis { items }) = view.blocks.first_mut() {
        if items.len() == 1 {
            if items[0].delta.is_some() {
                return; // 已有 delta：先判，后面的计算全白做
            }
            if prev.abs() < f64::EPSILON {
                return; // 上期为 0，环比无意义
            }
            // 🔴 百分比指标的 delta 必须用**百分点**，不是相对百分比：
            // 毛利率 19.30%→19.63% 按相对算是「+1.7%」（实测判官抓获），正确说法是 +0.33pp。
            // 毛利率族是小数比值口径（前端 isGrossMarginLabel 显示时 ×100），pp 要 ×100；
            // 其它百分比列按既有约定已是 0-100 数值，pp 直接相减。
            let (pct, flat_eps, factor) = if matches!(items[0].semantic, Semantic::Percent) {
                let points = if is_ratio_percent_label(&items[0].label) {
                    (cur - prev) * 100.0
                } else {
                    cur - prev
                };
                (points, 0.005, 100.0) // pp 保留两位小数（0.33pp），flat 阈值 0.005pp
            } else {
                ((cur - prev) / prev * 100.0, 0.05, 10.0)
            };
            let dir = if pct > flat_eps { "up" } else if pct < -flat_eps { "down" } else { "flat" };
            let delta = Delta {
                pct: (pct * factor).round() / factor,
                dir,
                label,
                baseline: prev,
                change: cur - prev,
            };
            items[0].delta = Some(delta);
        }
    }
}

/// 毛利率族标签 = 小数比值口径（与前端 `isGrossMarginLabel`/`isGrossMarginValueLabel` 同源，
/// 两处都是去掉空白后全等匹配；显示侧 ×100，所以 delta 也要 ×100 才是百分点）。
fn is_ratio_percent_label(label: &str) -> bool {
    let normalized: String = label.chars().filter(|c| !c.is_whitespace()).collect();
    normalized == "毛利率" || normalized == "销售毛利率"
}

/// 组装 ViewSpec：推断下钻维度 + 结论洞察（角色下标复用 `build` 算好的 `ix`，不再重扫）
fn mk(specs: Vec<ColumnSpec>, blocks: Vec<Block>, rows: &[Vec<Value>], ix: &RoleIdx) -> ViewSpec {
    let drill = infer_drill(&specs, !ix.metric.is_empty());
    let insight = compute_insight(&specs, rows, ix);
    ViewSpec { columns: specs, blocks, interact: Interact { drill }, insight }
}

/// 结论洞察（移植 SuperSonic textSummary）：排行占比+CR3集中度 / 趋势涨跌，确定性 0-LLM。
fn compute_insight(specs: &[ColumnSpec], rows: &[Vec<Value>], ix: &RoleIdx) -> Option<String> {
    // 「今天」按业务时区（东八区，与 SQL 侧 CURDATE() 同口径）取：用进程本地时区
    // （容器常 UTC）会在月初/月末当天差一天，不足月判定就错。
    let today = chrono::FixedOffset::east_opt(8 * 3600)
        .map(|tz| chrono::Local::now().with_timezone(&tz).date_naive())
        .unwrap_or_else(|| chrono::Local::now().date_naive());
    compute_insight_on(specs, rows, today, ix)
}

/// 全局行上限（= `dms_agent::MAX_ROWS`；agent→semantic 单向依赖，这里只能复刻数值
/// —— 两侧相等由 agent 侧的 `row_cap_matches_agent_max_rows` 断言守着）。
/// 判据与 `agent::ctx` 的截断提示同源：行数顶到上限即「可能被截断」。
pub const ROW_CAP: usize = 200;

/// 月份标签（`YYYY-MM`，月度维度的出数形态）是不是「本月至今」的不足月：
/// 等于当前年月、且今天还不是月末最后一天（月末当天全月已齐，端点可用）。
/// 日粒度标签（`YYYY-MM-DD`）与任何非法形态一律不算。
fn is_partial_current_month(label: &str, today: chrono::NaiveDate) -> bool {
    use chrono::Datelike;
    let b = label.as_bytes();
    if b.len() != 7 || b[4] != b'-' || !b[..4].iter().chain(b[5..].iter()).all(u8::is_ascii_digit) {
        return false;
    }
    let (Ok(y), Ok(m)) = (label[..4].parse::<i32>(), label[5..].parse::<u32>()) else {
        return false;
    };
    if !(1..=12).contains(&m) || (y, m) != (today.year(), today.month()) {
        return false;
    }
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let Some(next_month) = chrono::NaiveDate::from_ymd_opt(ny, nm, 1) else {
        return false;
    };
    today.day() < (next_month - chrono::Duration::days(1)).day()
}

/// `today` 参数化：不足月判定要「今天」，测试用固定日期打判据
/// （生产路径恒为本地今天，与 SQL 侧 CURDATE() 同口径）。
fn compute_insight_on(
    specs: &[ColumnSpec],
    rows: &[Vec<Value>],
    today: chrono::NaiveDate,
    ix: &RoleIdx,
) -> Option<String> {
    // 🔴 **单行全 NULL 要说话**。`SUM` over 0 行给的是一行 NULL，不是 0 行 ——
    // 于是它既不走「无结果」提示、也没有洞察，前端渲染成**一个空格子**，
    // 用户分不清「这段时间没数据」和「系统坏了」。实测现场：数据从 2025-09-29 起，
    // 问「2025年上半年的销量」得到 `rows=[[null]]`、`insight=None`、只有一张空表。
    // 这一支放在最前面：它与「有几个指标列」无关，后面那些判据都会先被指标数挡掉。
    // （空行 `vec![[]]` 不算：`all()` 对空迭代恒真，空列空行不该报「没有数据」。）
    if rows.len() == 1 && !rows[0].is_empty() && rows[0].iter().all(|c| matches!(c, Value::Null)) {
        return Some(
            "该条件下没有数据（聚合结果为空）——请确认时间范围与筛选条件；\
             若时间早于系统数据起点，换个区间再试。"
                .into(),
        );
    }
    if ix.metric.len() != 1 {
        return None;
    }
    let mi = ix.metric[0];
    let cat_i = ix.cat.first().copied();
    let time_i = ix.time.first().copied();
    let sem = specs[mi].semantic;
    // 毛利率族是小数比值口径（`patch_kpi_delta` 的 ×100 同族）：洞察里 0.1963 要渲染成 19.6%
    let ratio_pct = is_ratio_percent_label(&specs[mi].name);
    let unit = |v: f64| match sem {
        Semantic::Money => format!("¥{}", compress(v)),
        Semantic::Percent if ratio_pct => format!("{:.1}%", v * 100.0),
        Semantic::Percent => format!("{:.1}%", v),
        _ => compress(v),
    };

    // 排行（类别+单指标，≥5 行）：榜首占比 + 前三合计占比（CR3）
    if let Some(ci) = cat_i {
        if rows.len() >= 5 {
            let is_geo = matches!(specs[ci].semantic, Semantic::Geo);
            let mut vals: Vec<(String, f64)> = rows
                .iter()
                .filter_map(|r| {
                    let v = cell_f64(&r[mi])?;
                    let raw = r.get(ci).map(val_str).unwrap_or_default();
                    let mut name = if is_geo { province_cn(&raw).map(|s| s.to_string()).unwrap_or(raw) } else { raw };
                    if name.trim().is_empty() {
                        name = "未知".to_string();
                    }
                    Some((name, v))
                })
                .collect();
            let total: f64 = vals.iter().map(|(_, v)| v).sum();
            if total > 0.0 && vals.len() >= 3 {
                vals.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                let top = &vals[0];
                let cr3: f64 = vals.iter().take(3).map(|(_, v)| v).sum();
                // 🔴 行数顶到全局行上限 = 结果可能被截断：分母只是截断子集的小计，
                // 拿它算「占 XX%」就是把子集份额冒充全量份额（实测 TOP200 截断下「占 35.6%」）。
                // 本函数是纯函数、拿不到全量合计（一次 SUM 要改管线签名），按裁决**不显示占比**。
                if rows.len() >= ROW_CAP {
                    return Some(format!(
                        "榜首「{}」{}；前三合计 {}（结果已截断为前 {} 行，占比缺全量分母从略）",
                        top.0, unit(top.1), unit(cr3), rows.len()
                    ));
                }
                // 含负值时占比无意义：分母被负值压低，`top/total` 能算出 >100% 的「占比」
                // （饼图分支的 `all_nonneg` 守卫同族）。只报名次与合计，不出百分比。
                if vals.iter().any(|(_, v)| *v < 0.0) {
                    return Some(format!(
                        "榜首「{}」{}；前三合计 {}（含负值项，占比从略；共 {} 项）",
                        top.0, unit(top.1), unit(cr3), vals.len()
                    ));
                }
                return Some(format!(
                    "榜首「{}」{}，占 {:.1}%；前三合计占 {:.1}%（共 {} 项）",
                    top.0, unit(top.1), top.1 / total * 100.0, cr3 / total * 100.0, vals.len()
                ));
            }
        }
    }
    // 趋势（时间+单指标，≥2 行）：首末对比涨跌
    if let Some(ti) = time_i {
        if rows.len() >= 2 {
            // 🔴 末段是「本月至今」的不足月时，拿它当端点必出假涨跌 —— 实测「最近三个月
            // 销售额趋势」拿 5 月全月对 8 月 10 天得出「下降 38%」（实际连续大涨）。
            // 不足月末段**排除出端点比较**；只剩一个完整端点时宁可不出趋势洞察，不比。
            let partial_tail = rows
                .last()
                .and_then(|row| row.get(ti))
                .is_some_and(|cell| is_partial_current_month(&val_str(cell), today));
            let end = if partial_tail { rows.len() - 1 } else { rows.len() };
            if end >= 2 {
                let first = cell_f64(&rows[0][mi])?;
                let last = cell_f64(&rows[end - 1][mi])?;
                if first.abs() > f64::EPSILON {
                    let pct = (last - first) / first * 100.0;
                    let note = if partial_tail { "（末月为本月至今，未纳入端点比较）" } else { "" };
                    // 首末完全相等是「持平」，不是「增长 0.0%」
                    if pct.abs() < 1e-9 {
                        return Some(format!("从 {} 到 {}，整体持平{}", unit(first), unit(last), note));
                    }
                    let dir = if pct > 0.0 { "增长" } else { "下降" };
                    return Some(format!(
                        "从 {} 到 {}，整体{} {:.1}%{}",
                        unit(first),
                        unit(last),
                        dir,
                        pct.abs(),
                        note
                    ));
                }
            }
        }
    }
    None
}

fn val_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// 省级区划码 → 省名。**全仓唯一一份 Rust 副本**（前端 `web/src/format.ts` 那份是渲染路径，
/// 由本文件 `province_codes_match_frontend` 守着不漂）。
pub const PROVINCE_LABELS: &[(&str, &str)] = &[
    ("110000", "北京"), ("120000", "天津"), ("130000", "河北"), ("140000", "山西"), ("150000", "内蒙古"),
    ("210000", "辽宁"), ("220000", "吉林"), ("230000", "黑龙江"), ("310000", "上海"), ("320000", "江苏"),
    ("330000", "浙江"), ("340000", "安徽"), ("350000", "福建"), ("360000", "江西"), ("370000", "山东"),
    ("410000", "河南"), ("420000", "湖北"), ("430000", "湖南"), ("440000", "广东"), ("450000", "广西"),
    ("460000", "海南"), ("500000", "重庆"), ("510000", "四川"), ("520000", "贵州"), ("530000", "云南"),
    ("540000", "西藏"), ("610000", "陕西"), ("620000", "甘肃"), ("630000", "青海"), ("640000", "宁夏"),
    ("650000", "新疆"), ("710000", "台湾"), ("810000", "香港"), ("820000", "澳门"),
];

/// 省级区划码 → 省名（insight 里 geo 列翻名，与前端 format.ts 一致）
// ponytail: 34 项线性扫描，消费点（排行洞察）行数上限 200（ROW_CAP）时无所谓；真要热再上 phf。
fn province_cn(code: &str) -> Option<&'static str> {
    PROVINCE_LABELS.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
}

/// 各 role 的列下标（D4：命名索引，不连排四个 `Vec<usize>`）
struct RoleIdx {
    metric: Vec<usize>,
    cat: Vec<usize>,
    time: Vec<usize>,
    id: Vec<usize>,
}

fn index_roles(specs: &[ColumnSpec]) -> RoleIdx {
    let of = |r: Role| -> Vec<usize> {
        specs.iter().enumerate().filter(|(_, s)| s.role == r).map(|(i, _)| i).collect()
    };
    RoleIdx { metric: of(Role::Metric), cat: of(Role::Category), time: of(Role::Time), id: of(Role::Id) }
}

/// 决策树（SuperSonic getMsgContentType 对齐 + 增强）
pub fn build(columns: &[String], rows: &[Vec<Value>]) -> ViewSpec {
    // 行宽恒等于列数是调用方（DB RowSet）的约定：下游按列下标直取，锯齿行会当场 panic
    debug_assert!(
        rows.iter().all(|r| r.len() == columns.len()),
        "build 要求每行宽度 == 列数（锯齿行是调用方 bug）"
    );
    let specs: Vec<ColumnSpec> = columns
        .iter()
        .enumerate()
        .map(|(i, name)| ColumnSpec {
            name: name.clone(),
            role: infer_role(name, rows, i),
            semantic: infer_semantic(name),
        })
        .collect();
    let ix = index_roles(&specs);
    let blocks = blocks_of(&specs, rows, &ix);
    mk(specs, blocks, rows, &ix)
}

/// **判定顺序即行为**：明细/多维必须在趋势线之前，兜底才是纯表格。只许提取不许重排。
fn blocks_of(specs: &[ColumnSpec], rows: &[Vec<Value>], ix: &RoleIdx) -> Vec<Block> {
    kpis(specs, rows, ix)
        .or_else(|| entity(specs, rows))
        .or_else(|| detail_table(rows, ix))
        .or_else(|| trend(rows, ix))
        .or_else(|| one_cat_one_metric(specs, rows, ix))
        .or_else(|| grouped_bar(rows, ix))
        .unwrap_or_else(|| vec![Block::Table])
}

/// 1. 单行全指标 → KPI 卡
fn kpis(specs: &[ColumnSpec], rows: &[Vec<Value>], ix: &RoleIdx) -> Option<Vec<Block>> {
    if rows.len() != 1 || ix.metric.is_empty() || ix.metric.len() != specs.len() {
        return None;
    }
    let items = ix
        .metric
        .iter()
        .map(|&i| Kpi {
            label: specs[i].name.clone(),
            value: rows[0][i].clone(),
            semantic: specs[i].semantic,
            delta: None,
        })
        .collect();
    Some(vec![Block::Kpis { items }])
}

/// 2. 单行多列 → 实体卡（单据卡）
fn entity(specs: &[ColumnSpec], rows: &[Vec<Value>]) -> Option<Vec<Block>> {
    if rows.len() != 1 || specs.len() < ENTITY_MIN_COLS {
        return None;
    }
    let pairs = specs
        .iter()
        .enumerate()
        .filter(|(i, _)| !matches!(rows[0][*i], Value::Null))
        .map(|(i, s)| (s.name.clone(), rows[0][i].clone()))
        .collect();
    Some(vec![Block::Entity { pairs }])
}

/// 3. 明细/多维形态 → 纯表格（对齐 SuperSonic：categoryField>1 或有 id 列 → TABLE）。
///    防止 200 行订单明细被误画成趋势线（每行不同订单，时间轴无意义）。
fn detail_table(rows: &[Vec<Value>], ix: &RoleIdx) -> Option<Vec<Block>> {
    (rows.len() > 1 && (ix.cat.len() > 1 || !ix.id.is_empty())).then(|| vec![Block::Table])
}

/// 4. 有时间列 + ≥2 行 + ≥1 指标 + 类别≤1（趋势序列，非明细）→ 趋势线图
fn trend(rows: &[Vec<Value>], ix: &RoleIdx) -> Option<Vec<Block>> {
    if ix.time.is_empty() || rows.len() < 2 || ix.metric.is_empty() || ix.cat.len() > 1 {
        return None;
    }
    // 🔴 时间 + 恰 1 类别 + 恰 1 指标 → 按类别切**多序列**，否则不同类别的点被连成一根混轴折线。
    // 形态（计划里 C3 那一族）：「今年各月各品类销售额」12 月 × 6 品类 = 72 行，
    // x 轴上「2026-01」重复 6 次。**不是连库实测，是从决策树读出来的**：本函数的守卫允许
    // `cat.len()==1`，而 `series` 之前不存在，前端只能按行序连线。
    // 三段闸门/口径判据/回归对它全绿 —— 它们看的是 SQL 与数，没人看图的拓扑。
    // 单测 `time_one_cat_one_metric_splits_series` 是它唯一的判据（反向验证：改成恒 None 即红）。
    // ponytail: 1 类别 + ≥2 指标（如「各月各品类的销售额和订单数」）仍是混轴 ——
    // 那要 类别×指标 的双层序列 + 双值轴，等它真出现在回归里再做。
    let series = (ix.cat.len() == 1 && ix.metric.len() == 1).then(|| ix.cat[0]);
    Some(vec![
        Block::Chart { kind: ChartKind::Line, x: ix.time[0], y: ix.metric.clone(), top: None, series },
        Block::Table,
    ])
}

/// 4a. 恰一类别列 + 恰一指标列 → 饼/柱
///
/// **这里刻意不加 `series`**（计划里那一支）：走到本函数说明 `trend` 已经 `None`，
/// 而在本函数自己的守卫（cat==1 && metric==1）下 `trend` 只有两种 `None` ——
/// ①无时间列（x 就是那唯一的类别列，没有第二个维度可切）②`rows < 2`（一个点，切了也是它自己）。
/// 两种都让那一支恒不触发＝死代码，而「加一支再给它写个绿判据」正是本仓反复抓的形态。
fn one_cat_one_metric(specs: &[ColumnSpec], rows: &[Vec<Value>], ix: &RoleIdx) -> Option<Vec<Block>> {
    // 0 行守卫：空结果落纯表格（前面 kpis/entity/detail/trend 全被行数挡掉，这里不能出空图块）
    if ix.cat.len() != 1 || ix.metric.len() != 1 || rows.is_empty() || rows.len() > BAR_MAX {
        return None;
    }
    let (x, y) = (ix.cat[0], ix.metric[0]);
    let all_nonneg = rows.iter().all(|r| cell_f64(&r[y]).map(|v| v >= 0.0).unwrap_or(true));
    let is_pct = matches!(specs[y].semantic, Semantic::Percent);
    let chart = if rows.len() <= PIE_MAX && all_nonneg && !is_pct {
        Block::Chart { kind: ChartKind::Pie, x, y: vec![y], top: None, series: None }
    } else {
        Block::Chart { kind: ChartKind::Bar, x, y: vec![y], top: bar_top(rows.len()), series: None }
    };
    Some(vec![chart, Block::Table])
}

/// 4b. 恰一类别列 + ≥2 指标列 → 分组柱图（对齐 SuperSonic：多指标同类别并排呈现）
fn grouped_bar(rows: &[Vec<Value>], ix: &RoleIdx) -> Option<Vec<Block>> {
    // 0 行守卫同 `one_cat_one_metric`：空结果落纯表格，不出空图块
    if ix.cat.len() != 1 || ix.metric.len() < 2 || rows.is_empty() || rows.len() > BAR_MAX {
        return None;
    }
    Some(vec![
        // 多指标本身就是多序列（`y` 有几列就几根柱），不需要 `series` 再切一层
        Block::Chart { kind: ChartKind::Bar, x: ix.cat[0], y: ix.metric.clone(), top: bar_top(rows.len()), series: None },
        Block::Table,
    ])
}

fn bar_top(nrows: usize) -> Option<usize> {
    (nrows > BAR_TOP).then_some(BAR_TOP)
}

fn cell_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// 大数压缩为 万/亿 中文计数（搬运自旧 `server/src/viewspec.rs:404`，逐字保留）。
fn compress(n: f64) -> String {
    let abs = n.abs();
    if abs >= 1e8 {
        format!("{:.2}亿", n / 1e8)
    } else if abs >= 1e4 {
        format!("{:.1}万", n / 1e4)
    } else {
        format!("{:.0}", n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cols(c: &[&str]) -> Vec<String> {
        c.iter().map(|s| s.to_string()).collect()
    }

    /// 🔴 空聚合必须**说出来**。`SUM` over 0 行返回的是**一行 NULL**，不是 0 行 ——
    /// 它从「无结果」与「有结果」两条分支之间漏过去，前端于是渲染成一个空格子，
    /// 用户分不清「这段时间没数据」与「系统坏了」。
    /// 实测现场：库里数据从 2025-09-29 起，问「2025年上半年的销量」得到 `rows=[[null]]`。
    #[test]
    fn all_null_single_row_says_no_data() {
        let v = build(&cols(&["销量箱数"]), &[vec![Value::Null]]);
        let s = v.insight.as_deref().unwrap_or("");
        assert!(s.contains("没有数据"), "空聚合必须给洞察，实得 {s:?}");
        // 多列全 NULL 同样算空聚合（`SELECT SUM(a), SUM(b)` over 0 行）
        let v2 = build(&cols(&["单数", "金额"]), &[vec![Value::Null, Value::Null]]);
        assert!(v2.insight.as_deref().unwrap_or("").contains("没有数据"));
        // 有值时不许误报（否则每个正常答案都挂一句「没有数据」）
        let v3 = build(&cols(&["销售额"]), &[vec![json!("163000000")]]);
        assert!(!v3.insight.as_deref().unwrap_or("").contains("没有数据"));
        // 部分 NULL 不算空（明细里某列为空是常态）
        let v4 = build(&cols(&["客户", "销售额"]), &[vec![Value::Null, json!("12")]]);
        assert!(!v4.insight.as_deref().unwrap_or("").contains("没有数据"));
        // 多行里有一行全 NULL 也不算（那是数据，不是空窗口）
        let v5 = build(&cols(&["月份", "销售额"]), &[vec![Value::Null, Value::Null], vec![json!("2026-01"), json!("5")]]);
        assert!(!v5.insight.as_deref().unwrap_or("").contains("没有数据"));
    }

    #[test]
    fn single_metric_row_kpi() {
        let v = build(&cols(&["销售额"]), &[vec![json!("163000000")]]);
        assert!(matches!(v.blocks[0], Block::Kpis { .. }));
        assert_eq!(v.columns[0].role, Role::Metric);
        assert_eq!(v.columns[0].semantic, Semantic::Money);

        let stock = build(&cols(&["库存量"]), &[vec![json!("201426668.004")]]);
        assert!(matches!(stock.blocks[0], Block::Kpis { .. }));
        assert_eq!(stock.columns[0].role, Role::Metric);
        assert_eq!(stock.columns[0].semantic, Semantic::Count);
    }

    #[test]
    fn dws_sales_drill_only_exposes_verified_dimensions() {
        let v = build(&cols(&["销售额"]), &[vec![json!("163000000")]]);
        assert_eq!(
            v.interact.drill,
            DWS_SALES_DIM_POOL.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
        assert!(!v.interact.drill.iter().any(|d| d.contains("业务员") || d.contains("门店") || d.contains("经理")));
    }

    #[test]
    fn category_metric_small_pie() {
        let rows = vec![
            vec![json!("广东"), json!("100")],
            vec![json!("山东"), json!("90")],
            vec![json!("河南"), json!("80")],
        ];
        let v = build(&cols(&["省份", "销售额"]), &rows);
        match &v.blocks[0] {
            Block::Chart { kind: ChartKind::Pie, .. } => {}
            b => panic!("expected pie, got {}", serde_json::to_string(b).unwrap()),
        }
    }

    #[test]
    fn category_metric_many_bar_top() {
        let rows: Vec<Vec<Value>> = (0..30).map(|i| vec![json!(format!("省{i}")), json!(i.to_string())]).collect();
        let v = build(&cols(&["省份", "销售额"]), &rows);
        match &v.blocks[0] {
            Block::Chart { kind: ChartKind::Bar, top: Some(20), .. } => {}
            b => panic!("expected bar top20, got {}", serde_json::to_string(b).unwrap()),
        }
    }

    #[test]
    fn time_series_line() {
        let rows = vec![
            vec![json!("2026-07-01"), json!("10")],
            vec![json!("2026-07-02"), json!("20")],
        ];
        let v = build(&cols(&["下单时间", "销售额"]), &rows);
        assert!(matches!(v.blocks[0], Block::Chart { kind: ChartKind::Line, .. }));
        assert_eq!(v.columns[0].role, Role::Time);
    }

    /// 🔴 「时间 + 恰 1 类别 + 1 指标」必须切多序列。
    /// 不切就是**一条混轴折线**：x 轴「2026-01」出现 3 次（3 个品类各一行），
    /// echarts 把 3 个品类的点按行序连成一根线 —— 图是错的，而 SQL、口径、行数全对，
    /// 所以此前没有任何一条判据会红（回归只看 `blocks[0].kind`）。
    #[test]
    fn time_one_cat_one_metric_splits_series() {
        let rows = vec![
            vec![json!("2026-01"), json!("烤肠"), json!("10")],
            vec![json!("2026-01"), json!("包子"), json!("7")],
            vec![json!("2026-02"), json!("烤肠"), json!("12")],
            vec![json!("2026-02"), json!("包子"), json!("6")],
        ];
        let v = build(&cols(&["月份", "商品分类", "销售额"]), &rows);
        // 前提先钉住：角色判错的话下面那句 series 断言会变成在量别的东西
        assert_eq!(v.columns[0].role, Role::Time);
        assert_eq!(v.columns[1].role, Role::Category);
        assert_eq!(v.columns[2].role, Role::Metric);
        match &v.blocks[0] {
            Block::Chart { kind: ChartKind::Line, x, y, series, .. } => {
                assert_eq!(*x, 0, "x 必须是时间列");
                assert_eq!(y, &vec![2]);
                assert_eq!(*series, Some(1), "series 必须等于类别列下标（1＝商品分类）");
            }
            b => panic!("expected multi-series line, got {}", serde_json::to_string(b).unwrap()),
        }
        // 上线的 JSON 里 series 是那个下标（前端契约）
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["blocks"][0]["series"], 1);
    }

    /// 反面：没有类别列就**不许**填 series（无脑填 = 前端按 y 值去分组，图同样是错的）。
    #[test]
    fn time_only_metric_has_no_series() {
        let rows = vec![vec![json!("2026-01"), json!("10")], vec![json!("2026-02"), json!("12")]];
        let v = build(&cols(&["月份", "销售额"]), &rows);
        match &v.blocks[0] {
            Block::Chart { kind: ChartKind::Line, series, .. } => assert_eq!(*series, None),
            b => panic!("expected line, got {}", serde_json::to_string(b).unwrap()),
        }
        let j = serde_json::to_value(&v).unwrap();
        assert!(j["blocks"][0].get("series").is_none(), "series=None 不许上线（旧 JSON 逐字节不变）");
    }

    #[test]
    fn category_multi_metric_grouped_bar() {
        // 一类别 + 两指标 → 分组柱（y 含两个指标列），不再落纯表格
        let rows = vec![
            vec![json!("广东"), json!("100"), json!("5")],
            vec![json!("山东"), json!("90"), json!("4")],
        ];
        let v = build(&cols(&["省份", "销售额", "订单数"]), &rows);
        match &v.blocks[0] {
            Block::Chart { kind: ChartKind::Bar, y, .. } => assert_eq!(y.len(), 2),
            b => panic!("expected grouped bar, got {}", serde_json::to_string(b).unwrap()),
        }
    }

    #[test]
    fn time_multi_metric_line_series() {
        // 时间 + 两指标 → 双序列趋势线
        let rows = vec![
            vec![json!("2026-07-01"), json!("10"), json!("2")],
            vec![json!("2026-07-02"), json!("20"), json!("3")],
        ];
        let v = build(&cols(&["下单时间", "销售额", "订单数"]), &rows);
        match &v.blocks[0] {
            Block::Chart { kind: ChartKind::Line, y, .. } => assert_eq!(y.len(), 2),
            b => panic!("expected multi-series line, got {}", serde_json::to_string(b).unwrap()),
        }
    }

    #[test]
    fn single_row_many_cols_entity() {
        let cols_v = cols(&["单号", "客户", "金额", "状态", "时间", "备注", "经办"]);
        let row = vec![json!("HJXH-1"), json!("恒众"), json!("100"), json!("完成"), json!("2026"), json!("x"), json!("张三")];
        let v = build(&cols_v, &[row]);
        assert!(matches!(v.blocks[0], Block::Entity { .. }));
    }

    #[test]
    fn multi_dim_table() {
        let rows = vec![
            vec![json!("广东"), json!("烤肠"), json!("100")],
            vec![json!("山东"), json!("包子"), json!("90")],
        ];
        let v = build(&cols(&["省份", "商品", "销量"]), &rows);
        assert!(matches!(v.blocks.last().unwrap(), Block::Table));
    }

    #[test]
    fn percent_uses_bar_not_pie() {
        let rows = vec![vec![json!("广东"), json!("50.5")], vec![json!("山东"), json!("30.2")]];
        let v = build(&cols(&["省份", "占比"]), &rows);
        assert!(matches!(v.blocks[0], Block::Chart { kind: ChartKind::Bar, .. }));
    }

    #[test]
    fn kpi_delta_up_down_and_zero() {
        let mut v = build(&cols(&["销售额"]), &[vec![json!("120")]]);
        patch_kpi_delta(&mut v, 120.0, 100.0, "较上月".into());
        if let Block::Kpis { items } = &v.blocks[0] {
            let d = items[0].delta.as_ref().unwrap();
            assert_eq!(d.dir, "up");
            assert_eq!(d.pct, 20.0);
            assert_eq!(d.baseline, 100.0);
            assert_eq!(d.change, 20.0);
        } else {
            panic!("no kpi");
        }
        // 上期为 0 → 不填 delta
        let mut v2 = build(&cols(&["销售额"]), &[vec![json!("120")]]);
        patch_kpi_delta(&mut v2, 120.0, 0.0, "较上月".into());
        if let Block::Kpis { items } = &v2.blocks[0] {
            assert!(items[0].delta.is_none());
        }
    }

    /// ③ 不足月末段必须排除出端点比较：实测「最近三个月销售额趋势」拿 5 月全月对
    /// 8 月 10 天得出「下降 38%」（实际连续大涨）。修后端点取到上一个完整月，并标注末月未纳入。
    #[test]
    fn trend_insight_excludes_partial_current_month() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 10).unwrap();
        let rows = vec![
            vec![json!("2026-05"), json!("100")],
            vec![json!("2026-06"), json!("200")],
            vec![json!("2026-07"), json!("400")],
            vec![json!("2026-08"), json!("150")], // 本月至今 10 天 —— 当端点就是「下降 62.5%」的假数
        ];
        let v = build(&cols(&["月份", "销售额"]), &rows);
        let s = compute_insight_on(&v.columns, &rows, today, &index_roles(&v.columns)).unwrap();
        assert!(s.contains("整体增长 300.0%"), "端点必须是 05→07（100→400）：{s}");
        assert!(s.contains("本月至今"), "必须标注末月未纳入：{s}");
        assert!(!s.contains("下降"), "{s}");

        // 月末当天：全月已齐，端点可用（不标注）
        let month_end = chrono::NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        let s2 = compute_insight_on(&v.columns, &rows, month_end, &index_roles(&v.columns)).unwrap();
        assert!(s2.contains("整体增长 50.0%"), "月末当天 08 月是完整月：{s2}");
        assert!(!s2.contains("本月至今"), "{s2}");

        // 末段是已过去的完整月：正常比较，一个字不多
        let rows_done = vec![
            vec![json!("2026-05"), json!("100")],
            vec![json!("2026-06"), json!("200")],
            vec![json!("2026-07"), json!("400")],
        ];
        let v3 = build(&cols(&["月份", "销售额"]), &rows_done);
        let s3 = compute_insight_on(&v3.columns, &rows_done, today, &index_roles(&v3.columns)).unwrap();
        assert_eq!(s3, "从 ¥100 到 ¥400，整体增长 300.0%");

        // 不足月排除后只剩一个完整端点：宁可不出趋势洞察，也不拿不足月比
        let rows_two = vec![
            vec![json!("2026-07"), json!("400")],
            vec![json!("2026-08"), json!("150")],
        ];
        let v4 = build(&cols(&["月份", "销售额"]), &rows_two);
        assert!(compute_insight_on(&v4.columns, &rows_two, today, &index_roles(&v4.columns)).is_none());

        // 不足月判定本体：日粒度/非法标签/往年同月都不算
        assert!(is_partial_current_month("2026-08", today));
        assert!(!is_partial_current_month("2026-08-10", today), "日粒度没有「不足月」一说");
        assert!(!is_partial_current_month("2025-08", today), "去年同月是完整月");
        assert!(!is_partial_current_month("2026-8", today));
        assert!(!is_partial_current_month("2026-13", today));
        assert!(is_partial_current_month("2026-08", chrono::NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()));
        assert!(!is_partial_current_month("2026-08", month_end));
        // 跨年末月（12 月 → 次年 1 月的月末推算）
        assert!(is_partial_current_month("2026-12", chrono::NaiveDate::from_ymd_opt(2026, 12, 15).unwrap()));
    }

    /// ④ 行数顶到全局上限（200）= 可能被截断：占比分母是截断子集小计，按裁决不显示占比。
    #[test]
    fn ranking_insight_suppresses_share_when_truncated() {
        let big: Vec<Vec<Value>> = (0..200)
            .map(|i| vec![json!(format!("客户{i:03}")), json!((1000 - i).to_string())])
            .collect();
        let v = build(&cols(&["客户", "销售额"]), &big);
        let s = v.insight.as_deref().unwrap_or("");
        assert!(s.contains("榜首"), "{s}");
        assert!(!s.contains('%'), "截断时不许出现任何百分比（分母是子集小计）：{s}");
        assert!(s.contains("截断"), "必须明说为什么没占比：{s}");
        // 未触上限：占比照出，一个字不改
        let small: Vec<Vec<Value>> = (0..100)
            .map(|i| vec![json!(format!("客户{i:03}")), json!((1000 - i).to_string())])
            .collect();
        let v2 = build(&cols(&["客户", "销售额"]), &small);
        let s2 = v2.insight.as_deref().unwrap_or("");
        assert!(s2.contains('%'), "未截断必须保留占比：{s2}");
        assert!(s2.contains("共 100 项"), "{s2}");
    }

    /// 排行洞察含负值不出百分比（分母被负值压低，`top/total` 能算出 >100% 的假占比）；
    /// 首末相等报「持平」；0 行结果落纯表格不出空图块；空列空行不报「没有数据」。
    #[test]
    fn insight_edge_cases_negative_flat_and_empty() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 10).unwrap();
        // 负值：只报名次与合计，一个 % 都不出
        let rows = vec![
            vec![json!("客户A"), json!("100")],
            vec![json!("客户B"), json!("90")],
            vec![json!("客户C"), json!("80")],
            vec![json!("客户D"), json!("-50")],
            vec![json!("客户E"), json!("60")],
        ];
        let v = build(&cols(&["客户", "毛利额"]), &rows);
        let s = v.insight.as_deref().unwrap_or("");
        assert!(s.contains("榜首"), "{s}");
        assert!(s.contains("含负值项"), "{s}");
        assert!(!s.contains('%'), "含负值不许出百分比：{s}");
        // 非负照旧出占比（防「守卫误伤常态」）
        let pos: Vec<Vec<Value>> = (0..5).map(|i| vec![json!(format!("c{i}")), json!((100 - i).to_string())]).collect();
        let v2 = build(&cols(&["客户", "毛利额"]), &pos);
        assert!(v2.insight.as_deref().unwrap_or("").contains('%'), "非负必须保留占比");
        // 首末相等 → 「持平」，不是「增长 0.0%」
        let flat = vec![vec![json!("2026-05"), json!("100")], vec![json!("2026-06"), json!("100")]];
        let v3 = build(&cols(&["月份", "销售额"]), &flat);
        let s3 = compute_insight_on(&v3.columns, &flat, today, &index_roles(&v3.columns)).unwrap();
        assert!(s3.contains("持平"), "{s3}");
        assert!(!s3.contains("增长") && !s3.contains("下降"), "{s3}");
        // 0 行：落纯表格，不出空 Pie/Bar
        let v4 = build(&cols(&["省份", "销售额"]), &[]);
        assert!(matches!(v4.blocks[0], Block::Table), "0 行不许出图块：{}", serde_json::to_string(&v4.blocks).unwrap());
        let v5 = build(&cols(&["省份", "销售额", "订单数"]), &[]);
        assert!(matches!(v5.blocks[0], Block::Table), "0 行分组柱同理");
        // 空列空行（vec![[]]）：不报「没有数据」（all() 对空迭代恒真的那个坑）
        let v6 = build(&cols(&[]), &[vec![]]);
        assert!(!v6.insight.as_deref().unwrap_or("").contains("没有数据"), "{:?}", v6.insight);
        // 毛利率族洞察按 0-100 渲染：0.1963 → 19.6%，不是 0.2%
        let pct_rows = vec![
            vec![json!("2026-05"), json!("0.1930")],
            vec![json!("2026-06"), json!("0.1963")],
        ];
        let v7 = build(&cols(&["月份", "毛利率"]), &pct_rows);
        let s7 = compute_insight_on(&v7.columns, &pct_rows, today, &index_roles(&v7.columns)).unwrap();
        assert!(s7.contains("19.6%") && s7.contains("19.3%"), "比值口径必须 ×100：{s7}");
    }

    /// ⑤ 百分比指标的 KPI delta 用百分点：毛利率 19.30%→19.63% 是 +0.33pp，不是「+1.7%」。
    #[test]
    fn percent_kpi_delta_uses_percentage_points() {
        let mut v = build(&cols(&["毛利率"]), &[vec![json!("0.1963")]]);
        patch_kpi_delta(&mut v, 0.1963, 0.1930, "较上月".into());
        if let Block::Kpis { items } = &v.blocks[0] {
            assert_eq!(items[0].semantic, Semantic::Percent, "前提：毛利率是百分比语义");
            let d = items[0].delta.as_ref().unwrap();
            assert_eq!(d.pct, 0.33, "delta 必须是百分点");
            assert_eq!(d.dir, "up");
            assert!((d.baseline - 0.1930).abs() < 1e-9, "{}", d.baseline);
            assert!((d.change - 0.0033).abs() < 1e-9, "{}", d.change);
        } else {
            panic!("no kpi");
        }
        // 0-100 口径的百分比列（非比值）：pp 直接相减
        let mut v2 = build(&cols(&["占比"]), &[vec![json!("36.9")]]);
        patch_kpi_delta(&mut v2, 36.9, 35.6, "较上月".into());
        if let Block::Kpis { items } = &v2.blocks[0] {
            assert_eq!(items[0].delta.as_ref().unwrap().pct, 1.3);
        } else {
            panic!("no kpi");
        }
        // 金额照旧是相对百分比（回归锁，不许被百分点那支拐走）
        let mut v3 = build(&cols(&["销售额"]), &[vec![json!("120")]]);
        patch_kpi_delta(&mut v3, 120.0, 100.0, "较上月".into());
        if let Block::Kpis { items } = &v3.blocks[0] {
            assert_eq!(items[0].delta.as_ref().unwrap().pct, 20.0);
        } else {
            panic!("no kpi");
        }
    }

    /// 省码单一事实源：前端 format.ts 那份是渲染路径，码与名必须逐条对上（少一个就是两处漂了）。
    #[test]
    fn province_codes_match_frontend() {
        let ts = include_str!("../../../web/src/format.ts");
        assert_eq!(PROVINCE_LABELS.len(), 34);
        for (code, name) in PROVINCE_LABELS {
            assert!(ts.contains(&format!("'{code}': '{name}'")), "format.ts 缺省码 {code}={name}");
        }
    }
}
