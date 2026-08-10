//! 【datanote ChartTool 的 artifact 面】手绘 inline SVG 图表：bar / line / pie。
//!
//! 为什么手绘而不是引库：产物页跑在 `sandbox allow-scripts`（无 allow-same-origin）里，
//! 且 `page_shell` 的纪律是「零外部资源可达」（离线部署 + 分享出去的单文件必须自洽）——
//! ECharts CDN 两条都撞。纯 SVG 连脚本都不用：沙箱里它只是静止图形。
//!
//! 数据纪律与 `semantic::present` / `BiChart.vue` 对齐：克制的业务色板（非彩虹）、
//! pie 只在非负时出、TOP 收纳多出来的并入「其他」、标签一律 escape（它们是数据）。

/// 克制的业务色板：主蓝、增长绿、提醒金、结构紫、风险红、辅助青。
const PALETTE: [&str; 6] = ["#3567d6", "#2f8f72", "#c8842f", "#7b61a8", "#c65757", "#3c7f91"];
/// pie 最多几片（多出来的并入「其他」—— 与 present.rs 的 TOP 收纳同一纪律）
const PIE_TOP: usize = 5;

/// 一份图表的取数规格（`view.blocks` Chart 的回声形状；日报服务端也按它自建）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ChartSpec {
    pub kind: String, // "bar" | "line" | "pie"
    pub x: usize,
    pub y: Vec<usize>,
    #[serde(default)]
    pub series: Option<usize>,
    #[serde(default)]
    pub top: Option<usize>,
    #[serde(default)]
    pub title: Option<String>,
}

/// 列/行 → SVG。退化输入（空行、y 缺列、全不可解析）一律空串 —— 缺图不许塌报表。
pub fn chart_svg(
    spec: &ChartSpec,
    columns: &[String],
    rows: &[Vec<serde_json::Value>],
) -> String {
    let Some(&y0) = spec.y.first() else { return String::new() };
    if spec.x >= columns.len() || y0 >= columns.len() || rows.is_empty() {
        return String::new();
    }
    let title = spec.title.clone().unwrap_or_else(|| columns[y0].clone());
    match spec.kind.as_str() {
        "pie" => pie_svg(&title, &columns[y0], &points(spec, columns, rows, y0)),
        "bar" => bar_svg(&title, &columns[y0], &points(spec, columns, rows, y0)),
        "line" => line_svg(&title, spec, columns, rows, y0),
        _ => String::new(),
    }
}

/// (标签, 值) 点列：标签取 x 列、值取 y 列；值不可解析的行丢弃（不编造 0）
fn points(
    spec: &ChartSpec,
    columns: &[String],
    rows: &[Vec<serde_json::Value>],
    y0: usize,
) -> Vec<(String, f64)> {
    let mut v: Vec<(String, f64)> = rows
        .iter()
        .filter_map(|r| {
            let label = cell_text(r.get(spec.x)?);
            let value = num(r.get(y0)?)?;
            Some((label, value))
        })
        .collect();
    // TOP 收纳（bar/pie 共用）：按值降序取前 top，其余并入「其他」
    if let Some(t) = spec.top.filter(|&t| v.len() > t) {
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let rest: f64 = v.split_off(t).into_iter().map(|(_, x)| x).sum();
        v.push(("其他".into(), rest));
    }
    let _ = columns;
    v
}

/// pie：非负才画（present.rs 的 all_nonneg 纪律 —— 负值切片的几何意义是错的）；
/// 超 PIE_TOP 并入「其他」。
fn pie_svg(title: &str, value_label: &str, pts: &[(String, f64)]) -> String {
    let mut pts: Vec<(String, f64)> = pts.iter().filter(|(_, v)| *v >= 0.0).cloned().collect();
    if pts.is_empty() {
        return String::new();
    }
    if pts.len() > PIE_TOP + 1 {
        pts.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let rest: f64 = pts.split_off(PIE_TOP).into_iter().map(|(_, x)| x).sum();
        pts.push(("其他".into(), rest));
    }
    let total: f64 = pts.iter().map(|(_, v)| v).sum();
    if total <= 0.0 {
        return String::new();
    }
    let (cx, cy, r, ir) = (100.0_f64, 100.0_f64, 80.0_f64, 46.0_f64);
    let mut body = String::new();
    let mut angle = -std::f64::consts::FRAC_PI_2; // 12 点方向起
    for (i, (label, v)) in pts.iter().enumerate() {
        let frac = v / total;
        let a2 = angle + frac * std::f64::consts::TAU;
        let large = if frac > 0.5 { 1 } else { 0 };
        let (x1, y1) = (cx + r * angle.cos(), cy + r * angle.sin());
        let (x2, y2) = (cx + r * a2.cos(), cy + r * a2.sin());
        let (x3, y3) = (cx + ir * a2.cos(), cy + ir * a2.sin());
        let (x4, y4) = (cx + ir * angle.cos(), cy + ir * angle.sin());
        let color = PALETTE[i % PALETTE.len()];
        // 单切片 100% 时弧命令退化（起终点同点），画整圆环
        if (frac - 1.0).abs() < 1e-9 {
            body.push_str(&format!(
                "<circle cx=\"{cx}\" cy=\"{cy}\" r=\"{r}\" fill=\"{color}\"/>\
                 <circle cx=\"{cx}\" cy=\"{cy}\" r=\"{ir}\" fill=\"#fff\"/>"
            ));
        } else {
            body.push_str(&format!(
                "<path d=\"M{x1:.1},{y1:.1} A{r},{r} 0 {large} 1 {x2:.1},{y2:.1} \
                 L{x3:.1},{y3:.1} A{ir},{ir} 0 {large} 0 {x4:.1},{y4:.1} Z\" fill=\"{color}\"/>"
            ));
        }
        angle = a2;
        let _ = label;
    }
    // 图例（名称 + 占比）
    let mut legend = String::new();
    for (i, (label, v)) in pts.iter().enumerate() {
        let y = 30 + i * 24;
        legend.push_str(&format!(
            "<rect x=\"210\" y=\"{}\" width=\"12\" height=\"12\" rx=\"2\" fill=\"{}\"/>\
             <text x=\"228\" y=\"{}\" font-size=\"12\" fill=\"#27324a\">{} · {} · {:.1}%</text>",
            y - 10,
            PALETTE[i % PALETTE.len()],
            y,
            escape(&clip_label(label)),
            escape(&display_number(value_label, *v)),
            v / total * 100.0
        ));
    }
    let h = 40 + pts.len() * 24;
    let h = h.max(200);
    format!(
        "<figure class=\"chart\"><figcaption>{title}</figcaption>\
         <svg viewBox=\"0 0 640 {h}\" width=\"100%\" role=\"img\"><title>{title}</title>{body}{legend}</svg></figure>",
        title = escape(title)
    )
}

/// bar：横条（中文标签长，横条不用旋转）。负值支持：零线按 [min(0,min), max] 定位。
fn bar_svg(title: &str, value_label: &str, pts: &[(String, f64)]) -> String {
    if pts.is_empty() {
        return String::new();
    }
    let max_value = pts.iter().map(|(_, v)| *v).fold(f64::NEG_INFINITY, f64::max);
    let min_value = pts.iter().map(|(_, v)| *v).fold(f64::INFINITY, f64::min);
    let max = max_value.max(0.0);
    let min = min_value.min(0.0);
    const LABEL_W: f64 = 150.0;
    const BAR_W: f64 = 400.0;
    // 全零也是有效业务数据：保留分类和 0 标签，不误报“暂无可视化数据”。
    let span = if (max - min).abs() < f64::EPSILON { 1.0 } else { max - min };
    let zero_x = LABEL_W + (0.0 - min) / span * BAR_W; // 零线位置（全非负时 = LABEL_W）
    let row_h = 28;
    let mut body = String::new();
    for (i, (label, v)) in pts.iter().enumerate() {
        let y = 10 + i * row_h;
        let (bx, bw) = if *v >= 0.0 {
            (zero_x, v / span * BAR_W)
        } else {
            (zero_x + v / span * BAR_W, -v / span * BAR_W)
        };
        let (value_x, value_anchor) = if *v >= 0.0 {
            (bx + bw + 6.0, "start")
        } else {
            (bx - 6.0, "end")
        };
        body.push_str(&format!(
            "<text x=\"{}\" y=\"{}\" font-size=\"12\" fill=\"#333\" text-anchor=\"end\">{}</text>\
             <rect x=\"{bx:.1}\" y=\"{}\" width=\"{bw:.1}\" height=\"18\" rx=\"2\" fill=\"{}\"/>\
             <text x=\"{value_x:.1}\" y=\"{}\" font-size=\"11\" fill=\"#566078\" text-anchor=\"{value_anchor}\">{}</text>",
            LABEL_W - 8.0,
            y + 13,
            escape(&clip_label(label)),
            y,
            PALETTE[i % PALETTE.len()],
            y + 13,
            display_number(value_label, *v)
        ));
    }
    // 零线（有负值时才画，全非负时它就是左边界）
    if min < 0.0 {
        body.push_str(&format!(
            "<line x1=\"{zero_x:.1}\" y1=\"4\" x2=\"{zero_x:.1}\" y2=\"{}\" stroke=\"#bbb\"/>",
            10 + pts.len() * row_h
        ));
    }
    let h = 20 + pts.len() * row_h + 8;
    format!(
        "<figure class=\"chart\"><figcaption>{title}</figcaption>\
         <svg viewBox=\"0 0 640 {h}\" width=\"100%\" role=\"img\"><title>{title}</title>{body}</svg></figure>",
        title = escape(title)
    )
}

/// line：单/多序列折线。`series` 列存在时按它分组（首见序），x 域 = 首见序去重标签；
/// 缺测点断开（`None` 不连线 —— 连成 0 是编造数据）。
fn line_svg(
    title: &str,
    spec: &ChartSpec,
    columns: &[String],
    rows: &[Vec<serde_json::Value>],
    y0: usize,
) -> String {
    // x 域：首见序去重
    let mut xs: Vec<String> = vec![];
    for r in rows {
        if let Some(v) = r.get(spec.x) {
            let l = cell_text(v);
            if !xs.contains(&l) {
                xs.push(l);
            }
        }
    }
    if xs.len() < 2 {
        return String::new(); // 单点不成线
    }
    // 序列：有 series 列按它分组（首见序），否则单列单序列（列名做名）
    let mut series: Vec<(String, Vec<Option<f64>>)> = vec![];
    match spec.series {
        Some(sc) if sc < columns.len() => {
            for r in rows {
                let name = r.get(sc).map(cell_text).unwrap_or_default();
                let xi = match r.get(spec.x) {
                    Some(v) => xs.iter().position(|x| *x == cell_text(v)),
                    None => None,
                };
                let (Some(xi), Some(v)) = (xi, r.get(y0).and_then(num)) else { continue };
                let idx = match series.iter().position(|(n, _)| *n == name) {
                    Some(i) => i,
                    None => {
                        series.push((name, vec![None; xs.len()]));
                        series.len() - 1
                    }
                };
                series[idx].1[xi] = Some(v);
            }
        }
        _ => {
            let vals: Vec<Option<f64>> = xs
                .iter()
                .map(|x| {
                    rows.iter()
                        .find(|r| r.get(spec.x).map(cell_text).as_deref() == Some(x))
                        .and_then(|r| r.get(y0))
                        .and_then(num)
                })
                .collect();
            series.push((columns[y0].clone(), vals));
        }
    }
    if series.is_empty() {
        return String::new();
    }
    let max = series
        .iter()
        .flat_map(|(_, vs)| vs.iter().flatten())
        .fold(f64::NEG_INFINITY, |a, &b| a.max(b));
    let min = series
        .iter()
        .flat_map(|(_, vs)| vs.iter().flatten())
        .fold(f64::INFINITY, |a, &b| a.min(b));
    if !min.is_finite() || !max.is_finite() {
        return String::new();
    }
    const PL: f64 = 56.0;
    const PR: f64 = 630.0;
    const PT: f64 = 16.0;
    const PB: f64 = 190.0;
    let x_at = |i: usize| PL + (PR - PL) * i as f64 / (xs.len() - 1) as f64;
    let (domain_min, domain_max) = if min.abs() < f64::EPSILON && max.abs() < f64::EPSILON {
        (-1.0, 1.0)
    } else {
        (min.min(0.0), max.max(0.0))
    };
    let span = domain_max - domain_min;
    if span.abs() < f64::EPSILON {
        return String::new();
    }
    let y_at = |v: f64| PT + (PB - PT) * (1.0 - (v - domain_min) / span);
    // 网格（4 条）+ y 轴刻度
    let mut body = String::new();
    for g in 0..=4 {
        let gy = PT + (PB - PT) * g as f64 / 4.0;
        body.push_str(&format!(
            "<line x1=\"{PL}\" y1=\"{gy:.1}\" x2=\"{PR}\" y2=\"{gy:.1}\" stroke=\"#eee\"/>\
             <text x=\"{}\" y=\"{:.1}\" font-size=\"9\" fill=\"#999\" text-anchor=\"end\">{}</text>",
            PL - 6.0,
            gy + 3.0,
            display_axis_number(&columns[y0], domain_max - span * g as f64 / 4.0)
        ));
    }
    if domain_min < 0.0 && domain_max > 0.0 {
        let zero_y = y_at(0.0);
        body.push_str(&format!(
            "<line x1=\"{PL}\" y1=\"{zero_y:.1}\" x2=\"{PR}\" y2=\"{zero_y:.1}\" stroke=\"#8792a8\" stroke-width=\"1.2\"/>"
        ));
    }
    // x 轴刻度（≤8 个，均抽）
    let step = (xs.len() as f64 / 8.0).ceil() as usize;
    for (i, x) in xs.iter().enumerate().step_by(step.max(1)) {
        body.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{}\" font-size=\"9\" fill=\"#999\" text-anchor=\"middle\">{}</text>",
            x_at(i),
            PB + 14.0,
            escape(&clip_label(x))
        ));
    }
    // 序列折线（缺测分段：逐连续段画 polyline）
    for (si, (name, vals)) in series.iter().enumerate() {
        let color = PALETTE[si % PALETTE.len()];
        let mut seg: Vec<String> = vec![];
        let flush = |seg: &mut Vec<String>, body: &mut String| {
            if seg.len() > 1 {
                body.push_str(&format!(
                    "<polyline points=\"{}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"2\"/>",
                    seg.join(" ")
                ));
            }
            seg.clear();
        };
        for (i, v) in vals.iter().enumerate() {
            match v {
                Some(v) => {
                    seg.push(format!("{:.1},{:.1}", x_at(i), y_at(*v)));
                    body.push_str(&format!(
                        "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"2.5\" fill=\"{color}\"/>",
                        x_at(i),
                        y_at(*v)
                    ));
                }
                None => flush(&mut seg, &mut body),
            }
        }
        flush(&mut seg, &mut body);
        // 图例
        let legend_x = PL + (si % 4) as f64 * 142.0;
        let legend_y = PB + 26.0 + (si / 4) as f64 * 18.0;
        body.push_str(&format!(
            "<rect x=\"{}\" y=\"{}\" width=\"12\" height=\"3\" fill=\"{color}\"/>\
             <text x=\"{}\" y=\"{}\" font-size=\"10\" fill=\"#666\">{}</text>",
            legend_x,
            legend_y,
            legend_x + 16.0,
            legend_y + 4.0,
            escape(&clip_label(name))
        ));
    }
    let legend_rows = ((series.len() + 3) / 4).max(1);
    let height = 220 + legend_rows * 20;
    format!(
        "<figure class=\"chart\"><figcaption>{title}</figcaption>\
         <svg viewBox=\"0 0 640 {height}\" width=\"100%\" role=\"img\"><title>{title}</title>{body}</svg></figure>",
        title = escape(title)
    )
}

fn num(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        serde_json::Value::String(s) => s
            .trim()
            .replace(',', "")
            .parse()
            .ok()
            .filter(|x: &f64| x.is_finite()),
        _ => None,
    }
}

fn cell_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 标签截 10 字（长了挤爆布局；全量在表格区，图只给形状）
fn clip_label(s: &str) -> String {
    if s.chars().count() <= 10 {
        return s.to_string();
    }
    s.chars().take(9).collect::<String>() + "…"
}

fn label_kind(label: &str) -> &'static str {
    let label = label.trim();
    let lower = label.to_ascii_lowercase();
    // 强标识优先于百分比、金额和数量，避免“状态码占比”等字段被当作业务指标压缩。
    if [
        "单号", "编号", "编码", "代码", "状态码", "区划码", "序号", "排名", "名次", "账号", "账户",
        "手机号", "电话", "身份证", "银行卡", "邮编", "地址", "条码", "批次", "车牌",
    ]
    .iter()
    .any(|word| label.contains(word))
        || matches!(
            lower.as_str(),
            "id" | "uuid" | "code" | "no" | "status" | "name" | "date" | "time" | "month"
                | "year" | "key" | "type" | "category" | "province" | "region" | "brand"
                | "customer" | "product" | "goods" | "shop" | "store" | "timestamp"
        )
        || lower
            .split('_')
            .any(|part| matches!(part, "id" | "uuid" | "code" | "no"))
        || [
            "_id", "_uuid", "_code", "_no", "_status", "_name", "_date", "_time", "_month", "_year",
            "_key", "_type", "_category", "_province", "_region", "_brand", "_customer", "_product",
            "_goods", "_shop", "_store", "_timestamp", "_at",
        ]
            .iter()
            .any(|suffix| lower.ends_with(suffix))
        || [
            "日期", "时间", "月份", "年份", "名称", "姓名", "客户", "商品", "门店", "省份", "地区",
            "品牌", "类型", "状态", "规格", "单位",
        ]
        .iter()
        .any(|word| label.ends_with(word))
    {
        "identity"
    } else if ["占比", "比例", "同比", "环比", "百分比", "增幅", "降幅"]
        .iter()
        .any(|word| label.ends_with(word))
        || label.ends_with('率')
        || label.contains('%')
        || matches!(lower.as_str(), "rate" | "ratio" | "pct" | "percent")
        || ["_rate", "_ratio", "_pct", "_percent"].iter().any(|suffix| lower.ends_with(suffix))
    {
        "percent"
    } else if [
        "销售额", "金额", "费用", "余额", "单价", "客单价", "成本", "利润", "收入", "支出",
        "货值", "资产", "坪效", "人效",
    ]
    .iter()
    .any(|word| label.contains(word))
        || label.ends_with('额')
        || ["amount", "revenue", "cost", "price", "fee", "balance", "profit", "income", "expense"]
            .iter()
            .any(|word| lower.contains(word))
    {
        "money"
    } else if [
        "数量", "销量", "订单数", "客户数", "门店数", "商品数", "件数", "箱数", "袋数", "台数",
        "人数", "次数", "库存量", "总数", "合计数", "行数", "单量", "订单量", "笔数",
    ]
    .iter()
    .any(|word| label.contains(word))
        || matches!(lower.as_str(), "qty")
        || ["count", "quantity", "_qty", "volume"].iter().any(|word| lower.contains(word))
    {
        "count"
    } else if ["名称", "姓名", "客户", "商品", "门店", "省份", "地区", "品牌", "类型", "状态"]
        .iter()
        .any(|word| label.contains(word))
    {
        "identity"
    } else {
        "number"
    }
}

fn grouped(v: f64) -> String {
    let v = if v.abs() < 0.000_5 { 0.0 } else { v };
    let raw = format!("{v:.3}");
    let (negative, raw) = raw.strip_prefix('-').map_or((false, raw.as_str()), |value| (true, value));
    let (integer, fraction) = raw.split_once('.').unwrap_or((raw, ""));
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    if negative {
        out.push('-');
    }
    for (index, ch) in integer.chars().enumerate() {
        if index > 0 && (integer.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    let fraction = fraction.trim_end_matches('0');
    if !fraction.is_empty() {
        out.push('.');
        out.push_str(fraction);
    }
    out
}

/// 面向用户的业务数值：满一万固定三位“万”，其余千分位且最多三位小数。
pub(crate) fn business_number(v: f64) -> String {
    if v.abs() >= 10_000.0 {
        format!("{:.3}万", v / 10_000.0)
    } else {
        grouped(v)
    }
}

pub(crate) fn display_number(label: &str, v: f64) -> String {
    match label_kind(label) {
        "percent" => format!("{}%", grouped(v)),
        "money" => format!("¥{}", business_number(v)),
        "identity" => v.to_string(),
        _ => business_number(v),
    }
}

/// 坐标轴只显示量级，不重复货币符号；tooltip、标签和表格仍保留完整语义。
fn display_axis_number(label: &str, v: f64) -> String {
    display_number(label, v).trim_start_matches('¥').to_string()
}

pub(crate) fn display_value(label: &str, value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Number(number) => {
            let Some(value) = number.as_f64().filter(|value| value.is_finite()) else {
                return number.to_string();
            };
            if label_kind(label) == "identity" {
                number.to_string()
            } else {
                display_number(label, value)
            }
        }
        serde_json::Value::String(text) => {
            if label_kind(label) == "identity" {
                return text.clone();
            }
            text.trim()
                .replace(',', "")
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(|value| display_number(label, value))
                .unwrap_or_else(|| text.clone())
        }
        other => other.to_string(),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 报表 markdown 里的图表占位符（`⟦CHART:n⟧`）。选生僻括号：问句/数据撞上的概率
/// 可忽略，且 md_to_html 的 inline() 不动它（先转义后还原的逆用 —— 占位符是**我们**
/// 写的，不是外部文本）。
pub const CHART_MARK: (&str, &str) = ("⟦CHART:", "⟧");

/// 把最终 HTML 里的 `⟦CHART:n⟧` 依次换成 SVG（n 与 charts 下标对应）。
/// 没渲出来时换成业务友好的空态，绝不把内部占位符暴露给用户。
pub fn fill_charts(html: &str, svgs: &[String]) -> String {
    let mut out = html.to_string();
    for (i, svg) in svgs.iter().enumerate() {
        let replacement = if svg.is_empty() {
            "<p class=\"chart-empty\">暂无可视化数据</p>"
        } else {
            svg
        };
        let marker = format!("{}{i}{}", CHART_MARK.0, CHART_MARK.1);
        out = out.replace(&format!("<p>{marker}</p>"), replacement);
        out = out.replace(&marker, replacement);
    }
    while let Some(start) = out.find(CHART_MARK.0) {
        let tail = start + CHART_MARK.0.len();
        if let Some(relative_end) = out[tail..].find(CHART_MARK.1) {
            let marker_end = tail + relative_end + CHART_MARK.1.len();
            let (replace_start, replace_end) = if out[..start].ends_with("<p>")
                && out[marker_end..].starts_with("</p>")
            {
                (start - "<p>".len(), marker_end + "</p>".len())
            } else {
                (start, marker_end)
            };
            out.replace_range(replace_start..replace_end, "<p class=\"chart-empty\">暂无可视化数据</p>");
        } else {
            let end = out[tail..].find('<').map(|offset| tail + offset).unwrap_or(out.len());
            out.replace_range(start..end, "暂无可视化数据");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cols() -> Vec<String> {
        vec!["省份".into(), "销售额".into(), "分类".into()]
    }

    /// 🔴 标签是数据：注入样例必须转义（产物页是可分享的 HTML，与 md_to_html 同一信任边界）
    #[test]
    fn labels_are_escaped_everywhere() {
        let spec = ChartSpec {
            kind: "bar".into(), x: 0, y: vec![1], series: None, top: None, title: None,
        };
        let rows = vec![vec![json!("<script>alert(1)</script>"), json!(100.0)]];
        let svg = chart_svg(&spec, &cols(), &rows);
        assert!(!svg.contains("<script>"), "{svg}");
        assert!(svg.contains("&lt;script&gt;"), "{svg}");
    }

    /// pie：负值切片丢弃（几何意义是错的）；>PIE_TOP 并入「其他」；标签/占比进图例。
    #[test]
    fn pie_discards_negative_and_folds_rest() {
        let spec = ChartSpec {
            kind: "pie".into(), x: 0, y: vec![1], series: None, top: None, title: None,
        };
        let rows: Vec<Vec<serde_json::Value>> = (0..8)
            .map(|i| vec![json!(format!("p{i}")), json!(100.0 - i as f64)])
            .chain(std::iter::once(vec![json!("负"), json!(-5.0)]))
            .collect();
        let svg = chart_svg(&spec, &cols(), &rows);
        assert!(!svg.contains(">负<"), "负值切片必须丢弃：{svg}");
        assert!(svg.contains("其他"), "8 片必须并出「其他」：{svg}");
        assert!(svg.contains("100.0%") || svg.contains('%'), "{svg}");
        // 全负 = 空串（缺图不许塌报表）
        let neg = vec![vec![json!("a"), json!(-1.0)]];
        assert!(chart_svg(&spec, &cols(), &neg).is_empty());
    }

    /// bar：负值出零线；top 收纳并「其他」。
    #[test]
    fn bar_handles_negative_and_top() {
        let spec = ChartSpec {
            kind: "bar".into(), x: 0, y: vec![1], series: None, top: Some(2), title: None,
        };
        let rows = vec![
            vec![json!("a"), json!(10.0)],
            vec![json!("b"), json!(-4.0)],
            vec![json!("c"), json!(2.0)],
            vec![json!("d"), json!(1.0)],
        ];
        let svg = chart_svg(&spec, &cols(), &rows);
        assert!(svg.contains("stroke=\"#bbb\""), "有负值必须画零线：{svg}");
        assert!(svg.contains("其他"), "top=2 必须并出「其他」：{svg}");
    }

    /// line：series 列分组（首见序）、缺测断开不连线、单序列无 series 列。
    #[test]
    fn line_groups_series_and_breaks_gaps() {
        let spec = ChartSpec {
            kind: "line".into(), x: 0, y: vec![1], series: Some(2), top: None, title: None,
        };
        let rows = vec![
            vec![json!("07-01"), json!(1.0), json!("甲")],
            vec![json!("07-02"), json!(2.0), json!("甲")],
            vec![json!("07-01"), json!(3.0), json!("乙")],
            // 乙在 07-02 缺测：那个序列必须断开而不是连到 0
        ];
        let svg = chart_svg(&spec, &cols(), &rows);
        // 甲两个点连成线；乙只有一个点 → 出点**不出线**（单点不成线，连到 0 是编造数据）
        assert_eq!(svg.matches("<polyline").count(), 1, "只有够两点的序列才出线：{svg}");
        assert_eq!(svg.matches("<circle").count(), 3, "三个实测点都在（含乙的孤立点）：{svg}");
        assert!(svg.contains("甲") && svg.contains("乙"), "{svg}");
        // 单点不成线
        let one = vec![vec![json!("07-01"), json!(1.0), json!("甲")]];
        assert!(chart_svg(&spec, &cols(), &one).is_empty());
        // 无 series 列：单列单序列
        let spec2 = ChartSpec { series: None, ..spec };
        let rows2 = vec![
            vec![json!("07-01"), json!(1.0), json!("甲")],
            vec![json!("07-02"), json!(2.0), json!("甲")],
        ];
        assert!(chart_svg(&spec2, &cols(), &rows2).contains("<polyline"));
    }

    /// 退化输入一律空串：未知 kind / 空行 / 列下标越界 / 值不可解析
    #[test]
    fn degenerate_input_is_empty_string() {
        let spec = ChartSpec {
            kind: "radar".into(), x: 0, y: vec![1], series: None, top: None, title: None,
        };
        let rows = vec![vec![json!("a"), json!(1.0)]];
        assert!(chart_svg(&spec, &cols(), &rows).is_empty(), "radar 不在 v1");
        let spec = ChartSpec { kind: "bar".into(), ..spec };
        assert!(chart_svg(&spec, &cols(), &[]).is_empty());
        let spec = ChartSpec { y: vec![9], ..spec };
        assert!(chart_svg(&spec, &cols(), &rows).is_empty());
        let bad = vec![vec![json!("a"), json!("不是数")]];
        let spec = ChartSpec { y: vec![1], ..spec };
        assert!(chart_svg(&spec, &cols(), &bad).is_empty());
    }

    /// 占位符 survives md_to_html（inline 不动它）；渲染失败必须给用户空态，不能暴露内部标记。
    #[test]
    fn chart_mark_survives_md_and_fills_by_index() {
        let md = "## 数据\n\n⟦CHART:0⟧\n\n⟦CHART:1⟧\n";
        let html = crate::artifact_api::md_to_html(md);
        assert!(html.contains("⟦CHART:0⟧"), "占位符被渲染器吃掉了：{html}");
        let out = fill_charts(&html, &["<svg>A</svg>".into(), String::new()]);
        assert!(out.contains("<svg>A</svg>"), "{out}");
        assert!(out.contains("暂无可视化数据"), "空 SVG 必须显示业务空态：{out}");
        assert!(!out.contains("⟦CHART:"), "内部占位符不得暴露：{out}");
    }

    #[test]
    fn user_numbers_keep_metric_and_identity_semantics() {
        assert_eq!(display_number("销售额", 12_345_678.9), "¥1234.568万");
        assert_eq!(display_number("客户销售额", 10_000.0), "¥1.000万");
        assert_eq!(display_number("商品销量", 10_000.0), "1.000万");
        assert_eq!(display_number("门店数", 10_000.0), "1.000万");
        assert_eq!(display_number("状态占比", 10_000.0), "10,000%");
        assert_eq!(display_number("环比变化额", 10_000.0), "¥1.000万");
        assert_eq!(display_axis_number("销售额", 10_000.0), "1.000万");
        assert_eq!(display_axis_number("环比", 12.3), "12.3%");
        assert_eq!(display_value("状态码占比", &json!(101)), "101");
        assert_eq!(display_value("商品编码数量", &json!(123456789)), "123456789");
        assert_eq!(display_value("销售单号数量", &json!(202608060001_u64)), "202608060001");
        assert_eq!(display_value("status_code_ratio", &json!(101)), "101");
        assert_eq!(display_value("order_no_count", &json!(202608060001_u64)), "202608060001");
        assert_eq!(display_value("商品编码", &json!(123456789)), "123456789");
        assert_eq!(display_value("订单状态", &json!(101)), "101");
        assert_eq!(display_value("customer_id", &json!("001234")), "001234");
        assert_eq!(display_value("province", &json!(430000)), "430000");
        assert_eq!(display_value("created_at", &json!(1722859200)), "1722859200");
        assert_eq!(display_number("qty", 10_000.0), "1.000万");
    }
}
