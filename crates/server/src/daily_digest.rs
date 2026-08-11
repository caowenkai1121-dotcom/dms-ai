//! 【S5】经营日报（datanote `DailyDigestScheduler` 的对应物）：每天一份 DWS 经营
//! 快照写成 `meta.artifact`（`conv_id=''`）。它是系统定时生成的全量经营产物，不继承
//! 单个会话或用户的数据范围；读取端必须单独限制为具备全量经营权限的身份。
//!
//! 调度形态与 A9 向量自愈同一个模子：启动即查 + 按 `INTERVAL` 周期一轮 + advisory lock
//!（多实例只跑一个）；「今天出过了吗」落 `meta.kv['digest_date']` —— **出成功了才写**，
//! 失败下轮重试（CAS 标记，datanote 的 daily marker 同构）。
//!
//! 🔴 口径只有一份：全部销售数字由 `dms_semantic::sales_fact` 出，与问数共享
//! `sales_dw.dws_off_offline_sale_dfn` 事实合同。日报只绑定服务端生成的日期。

use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

use chrono::{Datelike, Duration, NaiveDate};
use dms_connector::source::SqlSource;

use crate::AppState;
use dms_semantic::sales_fact::{self, Dimension, Metric};

/// advisory lock 键（与 A9 的 7_720_031 不同即可）
const LOCK_KEY: i64 = 7_720_033;
/// 两轮间隔（首轮启动即查：重启赶上今天就补出）
const INTERVAL: std::time::Duration = std::time::Duration::from_secs(600);
/// 「今日已出」标记（CAS：出成功了才写）
const KV_DIGEST_DATE: &str = "digest_date";
/// 「今天」唯一定义在库侧时钟，与 TODAY_DIGEST_SQL / PRUNE_DAILY_SQL 同一个
/// 'Asia/Shanghai' 表达式（上海无 DST，与 +8 等价）——应用/库时钟或容器 TZ 不一致时，
/// KV 标记与 artifact 实物也不会错位。`::text` 输出恒为 YYYY-MM-DD，免 chrono 解码依赖。
const BUSINESS_TODAY_SQL: &str =
    "SELECT ((CURRENT_TIMESTAMP AT TIME ZONE 'Asia/Shanghai')::date)::text";
/// 存在性判断只需 EXISTS：不取 id 也就不需要排序。
const TODAY_DIGEST_SQL: &str = "SELECT EXISTS( \
     SELECT 1 FROM meta.artifact \
     WHERE conv_id = '' AND created_by = 'daily-digest' \
       AND (created_at AT TIME ZONE 'Asia/Shanghai')::date = \
           (CURRENT_TIMESTAMP AT TIME ZONE 'Asia/Shanghai')::date \
   )";
const PRUNE_DAILY_SQL: &str = "DELETE FROM meta.artifact \
     WHERE conv_id = '' AND created_by = 'daily-digest' AND ( \
       (created_at AT TIME ZONE 'Asia/Shanghai')::date < \
         (CURRENT_TIMESTAMP AT TIME ZONE 'Asia/Shanghai')::date OR ( \
         (created_at AT TIME ZONE 'Asia/Shanghai')::date = \
           (CURRENT_TIMESTAMP AT TIME ZONE 'Asia/Shanghai')::date \
         AND id <> COALESCE(( \
           SELECT id FROM meta.artifact \
           WHERE conv_id = '' AND created_by = 'daily-digest' \
             AND (created_at AT TIME ZONE 'Asia/Shanghai')::date = \
               (CURRENT_TIMESTAMP AT TIME ZONE 'Asia/Shanghai')::date \
           ORDER BY id DESC LIMIT 1 \
         ), -1) \
        ) \
     ) RETURNING id";

/// 「今天」只从库侧时钟取（见 BUSINESS_TODAY_SQL）：应用时钟（旧 business_today）与 PG 时钟
/// 各取一份的时代，容器 TZ 不一致会把 KV 标记与 artifact 实物算错位、触发跨午夜重复生成。
async fn business_today(st: &AppState) -> anyhow::Result<NaiveDate> {
    let row: Option<(String,)> = st.owned.fixed(BUSINESS_TODAY_SQL).fetch_optional().await?;
    let (s,) = row.ok_or_else(|| anyhow::anyhow!("库侧时钟查询无返回行"))?;
    Ok(NaiveDate::parse_from_str(&s, "%Y-%m-%d")?)
}

/// 昨日订单数（订单口径：`agg_template` 订单数分支的同款过滤，判据钉着两边一致）。
/// 下单时间 order_time —— 它回答的是「昨天来了多少单」，不能从 DWS 明细行数推算。
const ORDERS_SQL: &str = "SELECT COUNT(DISTINCT sales_order_code) AS `订单数` FROM dms_ods.t_sales_order \
     WHERE deleted_flag = 0 AND order_status NOT IN ('0','108','199') \
       AND order_time >= ? AND order_time < ?";

/// DWS 销售 SQL（启动时建一次，Box::leak 成 `&'static`；每天复用）。
struct Sqls {
    /// 六个经营指标一次扫表返回：bind 起止（昨日用 y/today，本月用 month_start/today）
    kpis: &'static str,
    trend: &'static str,
    top_region: &'static str,
    top_customer: &'static str,
}

fn sqls() -> &'static Sqls {
    static S: OnceLock<Sqls> = OnceLock::new();
    S.get_or_init(|| {
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        let aggregate = |dims: &[Dimension]| {
            sales_fact::aggregate_sql(Metric::SalesAmount, dims, "?", "?")
        };
        let kpis = sales_fact::aggregate_sql_many(
            &[
                Metric::SalesAmount,
                Metric::SalesQuantity,
                Metric::RevenueExcludingTax,
                Metric::CostExcludingTax,
                Metric::GrossProfit,
                Metric::GrossMargin,
            ],
            &[],
            "?",
            "?",
        );
        let keyed = |inner: String, alias: &'static str| {
            leak(format!("SELECT z.`{alias}` AS k, CAST(z.`销售额` AS DOUBLE) AS v FROM ({inner}) z"))
        };
        let ranked = |mut sql: String| {
            sql.push_str(" ORDER BY `销售额` DESC LIMIT 5");
            sql
        };
        Sqls {
            kpis: leak(format!(
                "SELECT CAST(z.`销售额` AS DOUBLE), CAST(z.`销量` AS DOUBLE), \
                 CAST(z.`不含税收入` AS DOUBLE), CAST(z.`不含税成本` AS DOUBLE), \
                 CAST(z.`毛利额` AS DOUBLE), CAST(z.`毛利率` AS DOUBLE) FROM ({kpis}) z"
            )),
            trend: keyed(aggregate(&[Dimension::OrderDate]), Dimension::OrderDate.name()),
            top_region: keyed(ranked(aggregate(&[Dimension::Region])), Dimension::Region.name()),
            top_customer: keyed(
                ranked(aggregate(&[Dimension::Customer])),
                Dimension::Customer.name(),
            ),
        }
    })
}

/// 挂后台。失败只 warn：日报是**增强**不是主链路，它哑了问答照样出数。
pub fn spawn(st: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            match run_round(&st).await {
                Ok(true) => {} // 生成打点（含 data_day）在锁内 generate 成功后发出
                Ok(false) => {}
                Err(e) => tracing::warn!("经营日报本轮失败（下轮重试）: {e:#}"),
            }
            tokio::time::sleep(INTERVAL).await;
        }
    });
}

/// `Ok(true)` = 本轮真出了一份；`Ok(false)` = 今天已出过 / 别的实例握着锁。
async fn run_round(st: &AppState) -> anyhow::Result<bool> {
    if !st.mysql.is_warehouse() {
        tracing::debug!(target = %st.mysql.target_name(), "经营日报仅在显式数仓目标上运行");
        return Ok(false);
    }
    let today = business_today(st).await?;
    let today_s = today.to_string();
    let done: Option<(String,)> =
        st.owned.fixed(crate::admin_api::KV_GET_SQL).bind(KV_DIGEST_DATE).fetch_optional().await?;
    let marked_today = done.map(|(v,)| v).as_deref() == Some(today_s.as_str());
    // KV 只是成功标记，不是日报本身：两者都在才可短路；短路也顺手清掉历史/重复日报。
    if marked_today && today_digest_exists(st).await? {
        // 清理失败不该让「今天已出过」变成整轮 Err 的告警噪音：降级 warn，下轮还会再清。
        if let Err(e) = prune_daily(st).await {
            tracing::warn!("经营日报：历史产物清理失败（下轮重试）: {e:#}");
        }
        return Ok(false);
    }
    // 多实例只跑一个（同 A9：锁握在同一条连接上，失败路径也解锁）。
    // 锁连接占用期间 KV/清理等查询走池内**其他**连接：池上限必须 >= 2，否则自己把自己等死。
    let mut conn = st.owned.pool().acquire().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(LOCK_KEY)
        .fetch_one(&mut *conn)
        .await?;
    if !locked {
        return Ok(false);
    }
    let r: anyhow::Result<bool> = async {
        // 清理和“是否已有今日份”的最终判断都在锁内，避免多实例重复生成。
        prune_daily(st).await?;
        let generated = if today_digest_exists(st).await? {
            false
        } else {
            generate(st, today).await?;
            tracing::info!(data_day = %(today - Duration::days(1)), "经营日报：今日份已生成");
            true
        };
        // 已有日报但 KV 丢失时只补标记；新生成则仍是成功后才落标记。
        st.owned.fixed(crate::admin_api::KV_SET_SQL)
            .bind(KV_DIGEST_DATE).bind(&today_s).execute().await?;
        Ok(generated)
    }.await;
    if let Err(e) = sqlx::query("SELECT pg_advisory_unlock($1)").bind(LOCK_KEY).execute(&mut *conn).await {
        // 解锁失败 = 连接带会话级锁还回池、终身占锁（同 A9 的坑）—— 至少留 warn。
        tracing::warn!("经营日报：advisory 解锁失败，该连接将带锁还回池: {e:#}");
    }
    r
}

async fn today_digest_exists(st: &AppState) -> anyhow::Result<bool> {
    let row: Option<(bool,)> = st.owned.fixed(TODAY_DIGEST_SQL).fetch_optional().await?;
    Ok(row.map(|(exists,)| exists).unwrap_or(false))
}

async fn prune_daily(st: &AppState) -> anyhow::Result<()> {
    let removed: Vec<(i64,)> = st.owned.fixed(PRUNE_DAILY_SQL).fetch_all().await?;
    if !removed.is_empty() {
        tracing::info!(removed = removed.len(), "经营日报：已清理历史或重复产物，旧分享链接同步失效");
    }
    Ok(())
}

/// 系统全量经营数据 → markdown → 整页 HTML → artifact。
/// 销售 KPI、趋势和结构榜全部经 `sales_fact` 合同生成；订单数独立走订单事实去重。
async fn generate(st: &AppState, today: NaiveDate) -> anyhow::Result<()> {
    let s = sqls();
    let y = today - Duration::days(1);
    // 月累计 = **报告日（昨天）所在月**的 1 号起：1 号出日报时昨天在上个月，
    // 用 today 的月首会算出恒 0 的空窗（实测 2026-08-01 首轮 MTD=0，AI 点评还得替它解释）
    let month_start = y.with_day(1).expect("每月必有 1 号");
    let prev_day = y - Duration::days(1);
    let yoy_day = previous_year(y);
    let yoy_day_end = yoy_day + Duration::days(1);
    let prev_month_start = previous_month_start(month_start);
    let elapsed_mtd = today - month_start;
    let prev_mtd_end = comparable_period_end(prev_month_start, elapsed_mtd);
    let yoy_month_start = previous_year(month_start);
    let yoy_mtd_end = comparable_period_end(yoy_month_start, elapsed_mtd);
    let t30 = today - Duration::days(30);
    let ys = y.to_string();
    let report_day = today.to_string();

    // 同一时间窗的六个经营指标一次扫描，避免为每个 KPI 重复扫无分区事实表。
    // 12 路互不依赖的查询并发发出：整轮耗时从「总和」降为「最大单路」（每天一轮，值得）。
    let (
        kpis_y, kpis_prev_day, kpis_yoy_day,
        kpis_mtd, kpis_prev_mtd, kpis_yoy_mtd,
        top_region, top_customer, trend,
        orders_y, orders_prev_day, orders_yoy_day,
    ) = tokio::try_join!(
        one_kpis(st, s.kpis, y, today),
        one_kpis(st, s.kpis, prev_day, y),
        one_kpis(st, s.kpis, yoy_day, yoy_day_end),
        one_kpis(st, s.kpis, month_start, today),
        one_kpis(st, s.kpis, prev_month_start, prev_mtd_end),
        one_kpis(st, s.kpis, yoy_month_start, yoy_mtd_end),
        top(st, s.top_region, y, today),
        top(st, s.top_customer, y, today),
        async {
            st.mysql.raw_dates_all::<(String, f64)>(s.trend, &[t30, today])
                .await
                .map_err(anyhow::Error::from)
        },
        one_orders(st, y, today),
        one_orders(st, prev_day, y),
        one_orders(st, yoy_day, yoy_day_end),
    )?;

    let digest = Digest {
        report_day: &report_day,
        data_day: &ys,
        kpis_y,
        kpis_prev_day,
        kpis_yoy_day,
        orders_y,
        orders_prev_day,
        orders_yoy_day,
        kpis_mtd,
        kpis_prev_mtd,
        kpis_yoy_mtd,
        top_region: &top_region,
        top_customer: &top_customer,
        trend: &trend,
    };
    let md = report_md(&digest);
    let chart_svgs = charts(&digest);
    // AI 经营点评（fast 档一次）：素材全是系统自己算的数，失败/开关关掉 = 该段缺席，
    // 日报照常出（与 insight_api「解读失败绝不让取数看起来失败」同一条）。
    let insight = if st.insight_enabled {
        let cols = vec!["指标".to_string(), "值".to_string()];
        let row = |name: &str, val: String| {
            vec![serde_json::Value::from(name), serde_json::Value::from(val)]
        };
        // i64 → f64 一次性换算：订单数量级远低于 2^53，精度损失纯理论（report_md 同款）。
        let (oy, opd, oyd) = (orders_y as f64, orders_prev_day as f64, orders_yoy_day as f64);
        let rows = vec![
            row("昨日销售额", money_opt(kpis_y.sales_amount)),
            row("昨日销售额环比", change(kpis_y.sales_amount, kpis_prev_day.sales_amount, false)),
            row("昨日销售额同比", change(kpis_y.sales_amount, kpis_yoy_day.sales_amount, false)),
            row("昨日销量", number_opt(kpis_y.sales_quantity)),
            row("昨日不含税收入", money_opt(kpis_y.revenue_excluding_tax)),
            row("昨日不含税成本", money_opt(kpis_y.cost_excluding_tax)),
            row("昨日毛利额", money_opt(kpis_y.gross_profit)),
            row("昨日毛利率", percent_opt(kpis_y.gross_margin)),
            row("昨日毛利率环比", change(kpis_y.gross_margin, kpis_prev_day.gross_margin, true)),
            row("昨日毛利率同比", change(kpis_y.gross_margin, kpis_yoy_day.gross_margin, true)),
            row("昨日订单数", crate::chart_svg::display_number("订单数", oy)),
            row("昨日订单数环比", change(Some(oy), Some(opd), false)),
            row("昨日订单数同比", change(Some(oy), Some(oyd), false)),
            row("当月累计销售额（报告日所在月）", money_opt(kpis_mtd.sales_amount)),
            row("当月累计销售额环比", change(kpis_mtd.sales_amount, kpis_prev_mtd.sales_amount, false)),
            row("当月累计销售额同比", change(kpis_mtd.sales_amount, kpis_yoy_mtd.sales_amount, false)),
            row("当月累计销量", number_opt(kpis_mtd.sales_quantity)),
            row("当月累计不含税收入", money_opt(kpis_mtd.revenue_excluding_tax)),
            row("当月累计不含税成本", money_opt(kpis_mtd.cost_excluding_tax)),
            row("当月累计毛利额", money_opt(kpis_mtd.gross_profit)),
            row("当月累计毛利率", percent_opt(kpis_mtd.gross_margin)),
        ];
        let r = dms_agent::Reading {
            question: "经营日报（DWS验证口径）。只根据给出的 KPI、同比环比和结构数据，用不超过 5 条短结论总结量级、变化、毛利、结构和待跟进事项；先指出变化最大的指标，不复述口径，不写空话。",
            sql: s.kpis,
            columns: &cols,
            rows: &rows,
            row_count: rows.len(),
            caliber_note: None,
        };
        r.insight(&st.llm).await
    } else {
        None
    };
    // report_md 只产出一个 <!--AI-->（report_md_shape 测试钉着计数），replace 换第一处即全部。
    let md = match insight {
        Some(i) => md.replace("<!--AI-->", &i),
        None => md.replace("<!--AI-->", "（本次没有模型点评）"),
    };
    let title = format!("经营日报 {report_day}");
    // 先渲 markdown 再换图表占位符（反了 SVG 会被当文本转义掉）
    let html_body = crate::chart_svg::fill_charts(&crate::artifact_api::md_to_html(&md), &chart_svgs);
    let html = crate::artifact_api::page_shell(&title, &html_body);
    crate::artifact_api::save_artifact(st, "", "report", &title, &html, "daily-digest").await?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
struct SalesKpis {
    sales_amount: Option<f64>,
    sales_quantity: Option<f64>,
    revenue_excluding_tax: Option<f64>,
    cost_excluding_tax: Option<f64>,
    gross_profit: Option<f64>,
    gross_margin: Option<f64>,
}

/// 六个经营指标一次扫表返回；全为空时保持 `None`，不把数据缺口伪装成零。
async fn one_kpis(
    st: &AppState,
    sql: &'static str,
    a: chrono::NaiveDate,
    b: chrono::NaiveDate,
) -> anyhow::Result<SalesKpis> {
    type Row = (
        Option<f64>, Option<f64>, Option<f64>,
        Option<f64>, Option<f64>, Option<f64>,
    );
    let row: Option<Row> = st.mysql.raw_dates_all(sql, &[a, b]).await?.into_iter().next();
    Ok(row.map(|r| SalesKpis {
        sales_amount: r.0,
        sales_quantity: r.1,
        revenue_excluding_tax: r.2,
        cost_excluding_tax: r.3,
        gross_profit: r.4,
        gross_margin: r.5,
    }).unwrap_or_default())
}

async fn one_orders(
    st: &AppState,
    a: chrono::NaiveDate,
    b: chrono::NaiveDate,
) -> anyhow::Result<i64> {
    let row: Option<(i64,)> = st.mysql.raw_dates_all(ORDERS_SQL, &[a, b]).await?.into_iter().next();
    Ok(row.map(|(value,)| value).unwrap_or_default())
}

fn previous_year(date: NaiveDate) -> NaiveDate {
    date.with_year(date.year() - 1).unwrap_or_else(|| {
        NaiveDate::from_ymd_opt(date.year() - 1, 2, 28).expect("2 月 28 日恒存在")
    })
}

fn previous_month_start(date: NaiveDate) -> NaiveDate {
    let (year, month) = if date.month() == 1 {
        (date.year() - 1, 12)
    } else {
        (date.year(), date.month() - 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1).expect("合法年月恒有 1 日")
}

fn next_month_start(date: NaiveDate) -> NaiveDate {
    let (year, month) = if date.month() == 12 {
        (date.year() + 1, 1)
    } else {
        (date.year(), date.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1).expect("合法年月恒有 1 日")
}

fn comparable_period_end(start: NaiveDate, elapsed: Duration) -> NaiveDate {
    std::cmp::min(start + elapsed, next_month_start(start))
}

/// 维度榜（强类型日期起止）。
async fn top(
    st: &AppState,
    sql: &'static str,
    a: chrono::NaiveDate,
    b: chrono::NaiveDate,
) -> anyhow::Result<Vec<(String, f64)>> {
    Ok(st.mysql.raw_dates_all(sql, &[a, b]).await?)
}

fn money(v: f64) -> String {
    crate::chart_svg::display_number("金额", v)
}

fn money_opt(v: Option<f64>) -> String {
    v.map(money).unwrap_or_else(|| "暂无".to_string())
}

fn number_opt(v: Option<f64>) -> String {
    v.map(|n| crate::chart_svg::display_number("销量", n))
        .unwrap_or_else(|| "暂无".to_string())
}

fn percent_opt(v: Option<f64>) -> String {
    v.map(|n| format!("{:.2}%", n * 100.0))
        .unwrap_or_else(|| "暂无".to_string())
}

/// change() 的零判定阈值（业务口径的「准零」）：f64::EPSILON（≈2.2e-16）太严，
/// 1e-9 级基线会算出天文百分比；1e-6 以下一律按零处理。
const ZERO_EPS: f64 = 1e-6;

fn change(current: Option<f64>, baseline: Option<f64>, percentage_points: bool) -> String {
    let (Some(current), Some(baseline)) = (current, baseline) else {
        return "暂无".to_string();
    };
    if percentage_points {
        return format!("{:+.2} 个百分点", (current - baseline) * 100.0);
    }
    // 双零分支与正常分支统一带符号（"+0.0%" 对 "{:+.1}%" 的 "+0.0%"）。
    if baseline.abs() < ZERO_EPS {
        return if current.abs() < ZERO_EPS { "+0.0%" } else { "新增" }.to_string();
    }
    format!("{:+.1}%", (current - baseline) / baseline.abs() * 100.0)
}

/// 报表素材（纯数据 —— `report_md` 是纯函数，判据打它不打连库的 `generate`）
struct Digest<'a> {
    report_day: &'a str,
    data_day: &'a str,
    kpis_y: SalesKpis,
    kpis_prev_day: SalesKpis,
    kpis_yoy_day: SalesKpis,
    orders_y: i64,
    orders_prev_day: i64,
    orders_yoy_day: i64,
    kpis_mtd: SalesKpis,
    kpis_prev_mtd: SalesKpis,
    kpis_yoy_mtd: SalesKpis,
    top_region: &'a [(String, f64)],
    top_customer: &'a [(String, f64)],
    trend: &'a [(String, f64)],
}

/// markdown 组装（纯函数）。`<!--AI-->` 是点评回填位 —— 点评是异步可选件，
/// 先出骨架再回填比把 Option 传给每个段落顺。
fn report_md(d: &Digest<'_>) -> String {
    // i64 → f64 一次性换算（量级远低于 2^53，无精度顾虑；generate 里喂点评的换算同款）。
    let (oy, opd, oyd) = (d.orders_y as f64, d.orders_prev_day as f64, d.orders_yoy_day as f64);
    let mut s = format!(
        "# 经营日报（{report_day}）\n\n\
         数据日期：{data_day}（昨日）。\n\n\
         数据范围：系统定时生成的全量经营快照，不继承单个会话或用户的数据权限；仅应向具备全量经营权限的身份开放。\n\n\
         口径：销售经营指标来自 sales_dw.dws_off_offline_sale_dfn；销售额=SUM(amount)，订单数=有效订单事实按单号去重，禁止用 DWS 行数推算。\n\n\
         ## 昨日核心指标\n\n| 指标 | 昨日 | 环比前日 | 同比去年同日 |\n|---|---:|---:|---:|\n\
         | 销售额 | {sy} | {sy_mom} | {sy_yoy} |\n| 销量 | {qty} | {qty_mom} | {qty_yoy} |\n| 不含税收入 | {rev} | {rev_mom} | {rev_yoy} |\n| 不含税成本 | {cost} | {cost_mom} | {cost_yoy} |\n| 毛利额 | {gp} | {gp_mom} | {gp_yoy} |\n| 毛利率 | {gm} | {gm_mom} | {gm_yoy} |\n| 订单数（订单事实去重） | {od} | {od_mom} | {od_yoy} |\n\n\
         ## 本月累计指标\n\n| 指标 | 本月累计 | 环比上月同期 | 同比去年同期 |\n|---|---:|---:|---:|\n\
         | 销售额 | {mtd} | {mtd_mom} | {mtd_yoy} |\n| 销量 | {mqty} | {mqty_mom} | {mqty_yoy} |\n| 不含税收入 | {mrev} | {mrev_mom} | {mrev_yoy} |\n| 不含税成本 | {mcost} | {mcost_mom} | {mcost_yoy} |\n| 毛利额 | {mgp} | {mgp_mom} | {mgp_yoy} |\n| 毛利率 | {mgm} | {mgm_mom} | {mgm_yoy} |\n\n\
         > 成本、收入、毛利字段缺失时显示“暂无”，不将空值解释为 0；销量单位沿用数仓 qty，不擅自标注为箱。\n\n",
        report_day = d.report_day,
        data_day = d.data_day,
        sy = money_opt(d.kpis_y.sales_amount),
        sy_mom = change(d.kpis_y.sales_amount, d.kpis_prev_day.sales_amount, false),
        sy_yoy = change(d.kpis_y.sales_amount, d.kpis_yoy_day.sales_amount, false),
        qty = number_opt(d.kpis_y.sales_quantity),
        qty_mom = change(d.kpis_y.sales_quantity, d.kpis_prev_day.sales_quantity, false),
        qty_yoy = change(d.kpis_y.sales_quantity, d.kpis_yoy_day.sales_quantity, false),
        rev = money_opt(d.kpis_y.revenue_excluding_tax),
        rev_mom = change(d.kpis_y.revenue_excluding_tax, d.kpis_prev_day.revenue_excluding_tax, false),
        rev_yoy = change(d.kpis_y.revenue_excluding_tax, d.kpis_yoy_day.revenue_excluding_tax, false),
        cost = money_opt(d.kpis_y.cost_excluding_tax),
        cost_mom = change(d.kpis_y.cost_excluding_tax, d.kpis_prev_day.cost_excluding_tax, false),
        cost_yoy = change(d.kpis_y.cost_excluding_tax, d.kpis_yoy_day.cost_excluding_tax, false),
        gp = money_opt(d.kpis_y.gross_profit),
        gp_mom = change(d.kpis_y.gross_profit, d.kpis_prev_day.gross_profit, false),
        gp_yoy = change(d.kpis_y.gross_profit, d.kpis_yoy_day.gross_profit, false),
        gm = percent_opt(d.kpis_y.gross_margin),
        gm_mom = change(d.kpis_y.gross_margin, d.kpis_prev_day.gross_margin, true),
        gm_yoy = change(d.kpis_y.gross_margin, d.kpis_yoy_day.gross_margin, true),
        od = crate::chart_svg::display_number("订单数", oy),
        od_mom = change(Some(oy), Some(opd), false),
        od_yoy = change(Some(oy), Some(oyd), false),
        mtd = money_opt(d.kpis_mtd.sales_amount),
        mtd_mom = change(d.kpis_mtd.sales_amount, d.kpis_prev_mtd.sales_amount, false),
        mtd_yoy = change(d.kpis_mtd.sales_amount, d.kpis_yoy_mtd.sales_amount, false),
        mqty = number_opt(d.kpis_mtd.sales_quantity),
        mqty_mom = change(d.kpis_mtd.sales_quantity, d.kpis_prev_mtd.sales_quantity, false),
        mqty_yoy = change(d.kpis_mtd.sales_quantity, d.kpis_yoy_mtd.sales_quantity, false),
        mrev = money_opt(d.kpis_mtd.revenue_excluding_tax),
        mrev_mom = change(d.kpis_mtd.revenue_excluding_tax, d.kpis_prev_mtd.revenue_excluding_tax, false),
        mrev_yoy = change(d.kpis_mtd.revenue_excluding_tax, d.kpis_yoy_mtd.revenue_excluding_tax, false),
        mcost = money_opt(d.kpis_mtd.cost_excluding_tax),
        mcost_mom = change(d.kpis_mtd.cost_excluding_tax, d.kpis_prev_mtd.cost_excluding_tax, false),
        mcost_yoy = change(d.kpis_mtd.cost_excluding_tax, d.kpis_yoy_mtd.cost_excluding_tax, false),
        mgp = money_opt(d.kpis_mtd.gross_profit),
        mgp_mom = change(d.kpis_mtd.gross_profit, d.kpis_prev_mtd.gross_profit, false),
        mgp_yoy = change(d.kpis_mtd.gross_profit, d.kpis_yoy_mtd.gross_profit, false),
        mgm = percent_opt(d.kpis_mtd.gross_margin),
        mgm_mom = change(d.kpis_mtd.gross_margin, d.kpis_prev_mtd.gross_margin, true),
        mgm_yoy = change(d.kpis_mtd.gross_margin, d.kpis_yoy_mtd.gross_margin, true),
    );
    // 【图表】一图一表，图先表后（先看形状再看数）：占位符 `⟦CHART:n⟧` 由 generate
    // 在 md_to_html 后换成 inline SVG（与 S2 报表同一条 fill_charts 路）。
    // 编号序与 charts() 的返回序一一对应：0=趋势、1=省区、2=客户。
    let section = |idx: usize, title: &str, rows: &[(String, f64)], s: &mut String| {
        if rows.is_empty() {
            return;
        }
        let _ = write!(s, "## {title}\n\n");
        let _ = write!(s, "{}{idx}{}\n\n", crate::chart_svg::CHART_MARK.0, crate::chart_svg::CHART_MARK.1);
        s.push_str("| 名称 | 销售额 |\n|---|---|\n");
        for (k, v) in rows {
            // 维度值先消毒：`|` 换全角、换行/回车换空格，脏值也不能撑破 markdown 表。
            let _ = writeln!(s, "| {} | {} |", k.replace('|', "｜").replace(['\n', '\r'], " "), money(*v));
        }
        s.push('\n');
    };
    section(0, "近 30 天日趋势", d.trend, &mut s);
    section(1, "昨日 TOP5 省区", d.top_region, &mut s);
    section(2, "昨日 TOP5 客户（不是门店）", d.top_customer, &mut s);
    s.push_str("## AI 经营点评\n\n<!--AI-->\n\n");
    s
}

/// 日报的三张图（规格 + 数据都在服务端，零回声零信任问题）。
/// 序 0 = 趋势、1 = 省区、2 = 客户 —— 与 `report_md` 的占位符编号一一对应。
fn charts(d: &Digest<'_>) -> Vec<String> {
    let to_rows = |rows: &[(String, f64)]| -> Vec<Vec<serde_json::Value>> {
        rows.iter().map(|(k, v)| vec![serde_json::Value::from(k.clone()), serde_json::Value::from(*v)]).collect()
    };
    let cols = || vec!["名称".to_string(), "销售额".to_string()];
    let specs = [
        crate::chart_svg::ChartSpec {
            kind: "line".into(), x: 0, y: vec![1], series: None, top: None,
            title: Some("近 30 天日趋势".into()),
        },
        crate::chart_svg::ChartSpec {
            kind: "bar".into(), x: 0, y: vec![1], series: None, top: None,
            title: Some("昨日 TOP5 省区".into()),
        },
        crate::chart_svg::ChartSpec {
            kind: "bar".into(), x: 0, y: vec![1], series: None, top: None,
            title: Some("昨日 TOP5 客户（不是门店）".into()),
        },
    ];
    let data = [
        to_rows(d.trend),
        to_rows(d.top_region),
        to_rows(d.top_customer),
    ];
    specs.iter().zip(&data).map(|(sp, rows)| crate::chart_svg::chart_svg(sp, &cols(), rows)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 报表骨架：核心指标齐、千分位、确认维度完整、AI 回填位在。
    #[test]
    fn report_md_shape() {
        let tp = vec![("湖南".to_string(), 100.5)];
        let customers = vec![("客户甲".to_string(), 66.0)];
        let tr = vec![("2026-07-31".to_string(), 88.0), ("2026-08-01".to_string(), 99.0)];
        let d = Digest {
            report_day: "2026-08-01",
            data_day: "2026-07-31",
            kpis_y: SalesKpis {
                sales_amount: Some(1234.5),
                sales_quantity: Some(88.0),
                revenue_excluding_tax: Some(1000.0),
                cost_excluding_tax: Some(700.0),
                gross_profit: Some(300.0),
                gross_margin: Some(0.3),
            },
            kpis_prev_day: SalesKpis {
                sales_amount: Some(1000.0),
                sales_quantity: Some(80.0),
                revenue_excluding_tax: Some(900.0),
                cost_excluding_tax: Some(650.0),
                gross_profit: Some(250.0),
                gross_margin: Some(0.25),
            },
            kpis_yoy_day: SalesKpis {
                sales_amount: Some(1100.0),
                sales_quantity: Some(90.0),
                revenue_excluding_tax: Some(950.0),
                cost_excluding_tax: Some(680.0),
                gross_profit: Some(270.0),
                gross_margin: Some(0.28),
            },
            orders_y: 42,
            orders_prev_day: 40,
            orders_yoy_day: 35,
            kpis_mtd: SalesKpis {
                sales_amount: Some(9999.0),
                sales_quantity: Some(880.0),
                revenue_excluding_tax: Some(8000.0),
                cost_excluding_tax: Some(5600.0),
                gross_profit: Some(2400.0),
                gross_margin: Some(0.3),
            },
            kpis_prev_mtd: SalesKpis {
                sales_amount: Some(9000.0),
                sales_quantity: Some(800.0),
                revenue_excluding_tax: Some(7000.0),
                cost_excluding_tax: Some(5000.0),
                gross_profit: Some(2000.0),
                gross_margin: Some(0.285),
            },
            kpis_yoy_mtd: SalesKpis {
                sales_amount: Some(8000.0),
                sales_quantity: Some(700.0),
                revenue_excluding_tax: Some(6500.0),
                cost_excluding_tax: Some(4700.0),
                gross_profit: Some(1800.0),
                gross_margin: Some(0.277),
            },
            top_region: &tp,
            top_customer: &customers,
            trend: &tr,
        };
        let md = report_md(&d);
        assert!(md.contains("# 经营日报（2026-08-01）"), "{md}");
        assert!(md.contains("数据日期：2026-07-31（昨日）"), "{md}");
        assert!(md.contains("系统定时生成的全量经营快照"), "公共日报必须显式声明全量属性：{md}");
        assert!(md.contains("仅应向具备全量经营权限的身份开放"), "公共日报必须提示访问控制责任：{md}");
        assert!(md.contains("| 销售额 | ¥1,234.5 | +23.4% | +12.2% |"), "{md}");
        assert!(md.contains("| 销量 | 88 | +10.0% | -2.2% |"), "{md}");
        assert!(md.contains("| 毛利率 | 30.00% | +5.00 个百分点 | +2.00 个百分点 |"), "{md}");
        assert!(md.contains("| 订单数（订单事实去重） | 42 | +5.0% | +20.0% |"), "{md}");
        assert!(md.contains("| 销售额 | ¥9,999 | +11.1% | +25.0% |"), "{md}");
        assert!(md.contains("<!--AI-->"), "点评回填位必须在：{md}");
        assert_eq!(md.matches("<!--AI-->").count(), 1, "回填位必须唯一（replace 只换第一处）：{md}");
        assert!(md.contains("| 湖南 | ¥100.5 |"), "{md}");
        assert!(md.contains("昨日 TOP5 客户（不是门店）"), "{md}");
        assert!(md.contains("| 2026-07-31 | ¥88 |"), "{md}");
        assert!(md.contains("⟦CHART:0⟧") && md.contains("⟦CHART:1⟧"), "{md}");
        assert!(md.contains("⟦CHART:2⟧"), "{md}");
        // 口径声明印在数字之前
        assert!(md.find("sales_dw.dws_off_offline_sale_dfn").unwrap() < md.find("## 昨日核心指标").unwrap());
        // 图表 SVG：趋势、省区、客户均来自默认销售事实确认维度。
        let svgs = charts(&d);
        assert_eq!(svgs.len(), 3);
        assert!(svgs[0].contains("<polyline"), "{}", svgs[0]);
        assert!(svgs[1].contains("<rect"), "{}", svgs[1]);
        assert!(svgs[2].contains("<rect"), "{}", svgs[2]);
        assert!(svgs[2].contains("不是门店"), "客户图题必须带判官口径的「非门店」：{}", svgs[2]);
        // 端到端小闭环：占位符在渲染后真的被换掉。
        let html = crate::chart_svg::fill_charts(&crate::artifact_api::md_to_html(&md), &svgs);
        assert!(html.contains("<polyline") && !html.contains("⟦CHART:0⟧"), "{html}");
        assert!(!html.contains("⟦CHART:2⟧"), "客户图占位符必须替换：{html}");
    }

    /// 金额：满一万转“万”并固定三位，小金额千分位且最多三位。
    #[test]
    fn money_groups_by_thousands() {
        assert_eq!(money(1234.5), "¥1,234.5");
        assert_eq!(money(-206084819.194), "¥-20608.482万");
        assert_eq!(money(88.0), "¥88");
        assert_eq!(money(1000.0), "¥1,000");
        assert_eq!(money(0.0), "¥0");
    }

    #[test]
    fn comparison_windows_never_spill_into_the_next_month() {
        let feb_2025 = NaiveDate::from_ymd_opt(2025, 2, 1).unwrap();
        assert_eq!(
            comparable_period_end(feb_2025, Duration::days(31)),
            NaiveDate::from_ymd_opt(2025, 3, 1).unwrap(),
        );
        let feb_2024 = NaiveDate::from_ymd_opt(2024, 2, 1).unwrap();
        assert_eq!(
            comparable_period_end(feb_2024, Duration::days(28)),
            NaiveDate::from_ymd_opt(2024, 2, 29).unwrap(),
        );
        assert_eq!(change(Some(10.0), Some(0.0), false), "新增");
        assert_eq!(change(Some(0.30), Some(0.25), true), "+5.00 个百分点");
    }

    /// 零判定是业务阈值（1e-6）不是 f64::EPSILON；双零分支与正常分支统一带符号。
    #[test]
    fn change_uses_business_zero_threshold() {
        assert_eq!(change(Some(0.0), Some(0.0), false), "+0.0%");
        assert_eq!(change(Some(1.0), Some(1e-9), false), "新增");
        assert_eq!(change(Some(0.002), Some(0.001), false), "+100.0%");
    }

    /// 脏维度值（换行/竖线）不能撑破 markdown 表：换行换空格、竖线换全角。
    #[test]
    fn dirty_dimension_values_cannot_break_tables() {
        let bad = vec![("客\n户|甲".to_string(), 1.0)];
        let d = Digest {
            report_day: "2026-08-01",
            data_day: "2026-07-31",
            kpis_y: SalesKpis::default(),
            kpis_prev_day: SalesKpis::default(),
            kpis_yoy_day: SalesKpis::default(),
            orders_y: 0,
            orders_prev_day: 0,
            orders_yoy_day: 0,
            kpis_mtd: SalesKpis::default(),
            kpis_prev_mtd: SalesKpis::default(),
            kpis_yoy_mtd: SalesKpis::default(),
            top_region: &[],
            top_customer: &bad,
            trend: &[],
        };
        let md = report_md(&d);
        assert!(md.contains("| 客 户｜甲 |"), "{md}");
        assert!(!md.contains("客\n户"), "{md}");
    }

    /// 🔴 日报没有第二份口径：销售 SQL 全部来自共享 DWS 事实合同。
    #[test]
    fn digest_sqls_come_from_the_single_dws_builder() {
        let s = sqls();
        for sql in [s.kpis, s.trend, s.top_region, s.top_customer] {
            assert!(sql.contains(sales_fact::TABLE), "{sql}");
            assert!(sql.contains("sf.order_date >= ? AND sf.order_date < ?"), "{sql}");
            assert!(sql.contains("SUM(sf.amount)"), "{sql}");
            assert!(!sql.contains(" JOIN "), "DWS 日报不应再做多表 JOIN：{sql}");
            for retired in ["UNION ALL", "t_sales_order", "t_sales_order_detail", "t_order_logistics"] {
                assert!(!sql.contains(retired), "DWS 日报不得回退旧发货/订单事实 {retired}：{sql}");
            }
            // 值列显式 DOUBLE（DECIMAL 解码不依赖驱动行为）
            assert!(sql.contains("CAST(z.`销售额` AS DOUBLE)"), "{sql}");
            // 占位之外没有第二个动态口（启动时 leak，之后就是静态串）
            assert!(!sql.contains('{'), "{sql}");
        }
        // Row 六元组位次与 kpis 外层投影列序隐式耦合：钉死列序，sqls() 调列序测试先红。
        assert!(s.kpis.starts_with(
            "SELECT CAST(z.`销售额` AS DOUBLE), CAST(z.`销量` AS DOUBLE), \
             CAST(z.`不含税收入` AS DOUBLE), CAST(z.`不含税成本` AS DOUBLE), \
             CAST(z.`毛利额` AS DOUBLE), CAST(z.`毛利率` AS DOUBLE) FROM ("
        ), "外层投影列序即 one_kpis 的 Row 位次：{}", s.kpis);
        let src = include_str!("daily_digest.rs");
        let one = src
            .split("async fn one_kpis(").nth(1).expect("one_kpis 没了")
            .split("\nasync fn ").next().unwrap();
        for frag in [
            "sales_amount: r.0", "sales_quantity: r.1", "revenue_excluding_tax: r.2",
            "cost_excluding_tax: r.3", "gross_profit: r.4", "gross_margin: r.5",
        ] {
            assert!(one.contains(frag), "Row 位次与 SalesKpis 字段映射变了：{one}");
        }
        for expression in [
            "SUM(sf.qty)", "SUM(sf.revenue_excluding_tax)",
            "SUM(sf.cost_excluding_tax)", "SUM(sf.gross_profit)",
        ] {
            assert!(s.kpis.contains(expression), "{}", s.kpis);
        }
        assert!(s.kpis.contains(
            "SUM(sf.gross_profit)/NULLIF(SUM(sf.revenue_excluding_tax),0)"
        ), "毛利率必须聚合后相除：{}", s.kpis);
        assert!(s.trend.contains("DATE(sf.order_date)"), "{}", s.trend);
        assert!(s.top_region.contains("LIMIT 5"), "{}", s.top_region);
        assert!(s.top_region.contains("sf.region"),
                "省区榜必须使用业务确认字段 region：{}", s.top_region);
        assert!(s.top_customer.contains("sf.storename"), "{}", s.top_customer);
        assert!(!s.top_customer.contains("shop"), "客户字段不得冒充门店：{}", s.top_customer);
        assert!(ORDERS_SQL.contains("order_status NOT IN ('0','108','199')"));
    }

    /// 调度判据：try 锁 / 同连接解锁 / KV 与今日产物双确认 / 成功后才写标记。
    #[test]
    fn scheduler_is_try_locked_same_conn_and_cas_marked() {
        let src = include_str!("daily_digest.rs");
        let body = src
            .split("async fn run_round(").nth(1).expect("run_round 没了")
            .split("async fn today_digest_exists").next().expect("run_round 边界没了");
        assert!(body.contains("!st.mysql.is_warehouse()"), "生产业务库必须在拿锁和取数前退出：{body}");
        assert!(body.contains("pg_try_advisory_lock"), "{body}");
        assert!(body.contains("pg_advisory_unlock"), "{body}");
        assert_eq!(body.matches(".acquire()").count(), 1, "锁与解锁不在同一条连接上：{body}");
        // KV 不能单独短路：今日实物必须存在；否则仍会进入锁内生成。
        let fast = body.find("marked_today && today_digest_exists(st).await?").expect("双确认没了");
        let lock = body.find("pg_try_advisory_lock").expect("锁没了");
        assert!(fast < lock, "常态路径应在拿锁前双确认：{body}");
        // CAS：生成代码位于写标记之前；generate 失败到不了 SET。
        let gen = body.find("generate(st, today).await?").expect("generate 没了");
        let set = body.find("KV_SET_SQL").expect("标记写入没了");
        assert!(gen < set, "失败路径也会写标记 = 今天再也补不出来：{body}");
        // 锁内先清理、再看今日实物、最后才决定是否生成。
        let locked = body.split("let r: anyhow::Result<bool> = async").nth(1).expect("锁内任务没了");
        assert!(locked.find("prune_daily(st).await?").unwrap()
            < locked.find("today_digest_exists(st).await?").unwrap());
        assert!(locked.find("today_digest_exists(st).await?").unwrap()
            < locked.find("generate(st, today).await?").unwrap());
    }

    /// 保留策略只动全局日报：历史全删，同日仅留 id 最大的一份，未来时间不误删。
    #[test]
    fn prune_keeps_only_todays_newest_digest() {
        assert!(PRUNE_DAILY_SQL.contains("conv_id = ''"), "{PRUNE_DAILY_SQL}");
        assert!(PRUNE_DAILY_SQL.contains("created_by = 'daily-digest'"), "{PRUNE_DAILY_SQL}");
        assert!(PRUNE_DAILY_SQL.contains("AT TIME ZONE 'Asia/Shanghai'"), "{PRUNE_DAILY_SQL}");
        assert!(PRUNE_DAILY_SQL.contains("id <> COALESCE"), "{PRUNE_DAILY_SQL}");
        assert!(PRUNE_DAILY_SQL.contains("ORDER BY id DESC LIMIT 1"), "{PRUNE_DAILY_SQL}");
        assert!(PRUNE_DAILY_SQL.contains("RETURNING id"), "清理必须显式返回被删产物，使旧分享 token 随行删除：{PRUNE_DAILY_SQL}");
        assert!(!PRUNE_DAILY_SQL.contains("CURRENT_DATE"),
            "日报边界必须按业务时区而不是容器/PG 会话时区：{PRUNE_DAILY_SQL}");
        // 存在性判断是短查询：不取 id、不排序（排序只对「留最新一份」的清理 SQL 有意义）。
        assert!(TODAY_DIGEST_SQL.starts_with("SELECT EXISTS("), "{TODAY_DIGEST_SQL}");
        assert!(!TODAY_DIGEST_SQL.contains("ORDER BY"), "{TODAY_DIGEST_SQL}");
        assert!(TODAY_DIGEST_SQL.contains("AT TIME ZONE 'Asia/Shanghai'"), "{TODAY_DIGEST_SQL}");
    }

    /// 「今天」只从库侧时钟取，且 run_round 算一次、generate 复用（跨午夜不错位）；
    /// 互不依赖的取数并发发出。
    #[test]
    fn today_comes_from_db_clock_once() {
        let src = include_str!("daily_digest.rs");
        assert!(BUSINESS_TODAY_SQL.contains("AT TIME ZONE 'Asia/Shanghai'"), "{BUSINESS_TODAY_SQL}");
        let gen = src.split("async fn generate(").nth(1).expect("generate 没了");
        let head = gen.split('{').next().unwrap();
        assert!(head.contains("today: NaiveDate"), "today 必须由 run_round 传入：{head}");
        let body = gen.split("\nasync fn ").next().unwrap();
        assert!(!body.contains("business_today"), "generate 不得自带时钟：{body}");
        assert!(body.contains("try_join!"), "互不依赖的取数应并发：{body}");
    }

    /// 短路路径的清理失败降级 warn：「今天已出过」不该变成整轮 Err 告警。
    #[test]
    fn fast_path_prune_failure_is_not_a_round_error() {
        let src = include_str!("daily_digest.rs");
        let body = src
            .split("async fn run_round(").nth(1).expect("run_round 没了")
            .split("async fn today_digest_exists").next().expect("run_round 边界没了");
        let fast = body.split("pg_try_advisory_lock").next().unwrap();
        assert!(fast.contains("if let Err"), "短路路径的 prune 失败应 catch 降级：{fast}");
        assert!(!fast.contains("prune_daily(st).await?"), "短路路径的 prune 失败不该 `?`：{fast}");
    }
}
