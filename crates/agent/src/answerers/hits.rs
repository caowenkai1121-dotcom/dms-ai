//! 确定性命中的**共同落地**：`DirectHit` → 三段闸门 → 取数 → 视图 → KPI 环比。
//! 变更原因＝「一条已经确定的 SQL 怎么变成答案」。
//!
//! 搬运源 `server/src/pipeline.rs:653-704`（`ask_single` 里那段确定性快路径），逐行搬运：
//! 分支顺序、`return`/回落位置、注释里的依据一字不改。
//!
//! 「组合器」（`direct::try_compose`）与「模板」（`direct::try_direct`）**共用这一段落地**，
//! 差别只在**谁产出 `DirectHit`** —— 于是这里只有一个 `HitAnswerer`，两个成员是它的两个实例。
//!
//! 🔴 `route` 取 **`DirectHit.route`** 给的值（`direct-agg` / `direct-doc`），
//! 不是 `Answerer::route()` 那个表标签：26 题回归断言 `direct-agg`、单号直查断言 `direct-doc`，
//! 混用即全红（`answerers/mod.rs` 的文件头纪律第四条）。

use std::time::Instant;

use dms_kernel::BoxFut;

use crate::answerers::Answerer;
use crate::ctx::{table_answer, AskCtx, AskResult, SalesContextResult, SupplementalResult};
use crate::gate::{gate_on, is_guard_err, EXEC_TIMEOUT, MAX_ROWS};

/// 确定性命中：SQL（未注入）+ 路由标签 + 可选上期查询（KPI 环比）。
/// 逐行搬 `server/src/direct.rs:11-17`。
///
/// ponytail: 这是**本轮唯一允许的临时重复**。`direct.rs` 的解体是 T8（`compose/*` + `fastpath/*`
/// 迁 semantic），届时本类型删掉、全仓改引 `dms_semantic::DirectHit`（ARCHITECTURE §4.4 `lib.rs` 行）。
/// 今天 agent 引不了 server（那是反向依赖边），所以在这里放一份同形结构 + 由调用方产出。
pub struct DirectHit {
    pub sql: String,
    pub route: String,
    /// (上期 SQL, 环比标签如"较上月")——仅高频聚合单指标时有
    pub prev: Option<(String, String)>,
    /// 额外基期查询（销售类通常为同比）。第一基期继续走 `prev`，保证旧调用与精简模式兼容。
    pub comparisons: Vec<(String, String)>,
    /// 补充明细 SQL：单据保留 Entity 头卡，聚合保留 KPI 卡，再追加图表/表格。
    /// CSV 与 AI 分析使用补充明细行，SQL 展示保留两次查询。
    pub detail: Option<String>,
    /// 销售单指标 KPI 的同窗补充 SQL（一条五值：销售额/不含税成本/不含税收入/毛利额/毛利率，
    /// 指标集＝`sales_fact::CONTEXT_METRICS`）。**只有 sales_fact 标量命中**（无维度、单指标）
    /// 由装配方给出；取数/落账一切失败 = None，主回答一个字符不变（`r.sql` 不追加它——
    /// 金标把展示 SQL 逐字钉死）。
    pub sales_context: Option<String>,
}

/// 「谁产出 `DirectHit`」是入参：`try_compose`（异步，读注册表）与 `try_direct`（同步，手工模板）
/// 都住在 `server/src/direct.rs`，agent 调不到。
///
/// **用具名 `fn` 而不是闭包**：闭包在这条 HRTB（返回的 future 借着入参的生命周期）上推断很脆，
/// 具名 `fn` 一定能强转。wire 侧的形态：
/// ```text
/// fn compose_hit<'a>(cx: &'a AskCtx<'a>) -> BoxFut<'a, Option<DirectHit>> {
///     Box::pin(async move { direct::try_compose(cx.pg, cx.ds, cx.question).await.map(into_hit) })
/// }
/// HitAnswerer::new("direct-agg", Box::new(compose_hit))
/// ```
pub type Produce =
    Box<dyn for<'a> Fn(&'a AskCtx<'a>) -> BoxFut<'a, Option<DirectHit>> + Send + Sync>;

/// 组合器/模板两成员共用的成员类型：只有「表标签」与「谁产出」两个字段不同。
pub struct HitAnswerer {
    label: &'static str,
    produce: Produce,
}

impl HitAnswerer {
    /// `label` 必须是 `ROUTER_ORDER` 里的表标签（compose → `direct-agg`，fastpath → `direct-doc`）。
    pub fn new(label: &'static str, produce: Produce) -> Self {
        Self { label, produce }
    }
}

impl Answerer for HitAnswerer {
    fn route(&self) -> &'static str {
        self.label
    }

    /// **恒真**（裁决 二·C 推翻了给它加 `is_unrestricted` 的想法：`regression_cases.json`
    /// D01/D03 是 tanlibo/city_manager 断言 `route=direct-agg`，加门禁当场红）。
    /// 行级权限由下面的 `gate_on` 一条不少地注入。
    fn accept(&self, _cx: &AskCtx<'_>) -> bool {
        true
    }

    fn answer<'a>(&'a self, cx: &'a AskCtx<'a>) -> BoxFut<'a, anyhow::Result<Option<AskResult>>> {
        Box::pin(async move {
            // `t0` 的起点：拆分前是 `ask_single` 入口（`pipeline.rs:641`）。`AskCtx` 今天没有
            // `t0` 字段，故成员内自取；差值＝router 前几位的耗时（graph 的 `accept` 是纯判断）。
            // 想让 `elapsed_ms` 起点一字不差，`AskCtx` 加一个 `t0` 即可（见交接单）。
            let t0 = cx.t0;
            match (self.produce)(cx).await {
                Some(hit) => land(cx, hit, t0).await,
                None => Ok(None),
            }
        })
    }
}

/// 落地：闸门 → 取数 → 视图 → KPI 环比。`Ok(None)` = 没接住（回落下一个成员）。
///
/// 🔴 确定性路径（compose / fastpath / graph / semantic-cache）**刻意不跑口径复核**，
/// 别顺手加上去：compose 的 SQL 就是按同一批声明（scope_filter / dedup_keys / 分区取最新）
/// 装配出来的，再判一遍是自己判自己——判红只说明装配器与校验器对声明的理解不一致，
/// 而那种不一致该由 compose 侧的断言抓，不该在运行时把一条正确的 SQL 送去回炉。
/// graph 根本不产 SQL；semantic-cache 复用的是已复核通过的语料（复核在入库那侧）。
/// 口径复核只补 LLM 路径那个缺口：那条路上没有任何东西强制声明生效。
/// 逐步等价于拆分前的 `is_safe_select(..).is_ok()` + `inject(..)?`：
/// 红线不过 → 静默回落 LLM；权限注入失败 → 硬失败（fail-closed 不许降级成 LLM 重试）。
pub async fn land(
    cx: &AskCtx<'_>,
    hit: DirectHit,
    t0: Instant,
) -> anyhow::Result<Option<AskResult>> {
    let DirectHit { sql, route, prev, comparisons, detail, sales_context } = hit;
    let gated = match gate_on(cx.p, &sql, cx.scope, cx.ds_global, cx.source.dialect()) {
        Ok(s) => Some(s),
        Err(e) if is_guard_err(&e) => None,
        Err(e) => return Err(e),
    };
    let Some(scoped) = gated else { return Ok(None) };
    // 计时：公网 Doris 取数是确定性路径上最大的一段，逐次留痕
    let t_fetch = Instant::now();
    let rs = match cx.source.fetch(&scoped, MAX_ROWS, EXEC_TIMEOUT).await {
        Ok(rs) => rs,
        Err(e) => {
            // 确定性 SQL 执行失败（列漂移、超时…）→ 回落 LLM，但**不许静默**。
            //
            // 这行 `warn!` 是补的：原实现用 `let Ok(..) else` 把错误**整个丢掉**，
            // 连日志都没有。代价是实测过的：某条回归题连续 5 轮在批量下失败，
            // 而系统一个字都没说 —— 最后靠手工 EXPLAIN 才查出是执行超时
            // （190 万行进临时表做 DISTINCT，29.98s 撞在 30s 上限）。
            // 回落本身保留：LLM 有时会写出更轻的 SQL；但「为什么回落」必须留痕，
            // 否则下一个人还要再查一遍。
            tracing::warn!(
                route = %route,
                err = %e,
                sql = %scoped.wire(),
                "确定性路径执行失败，回落 LLM"
            );
            return Ok(None);
        }
    };
    tracing::info!(
        ms = t_fetch.elapsed().as_millis(),
        rows = rs.rows.len(),
        route = %route,
        "确定性路径主查询取数完成"
    );
    // Doris 可能尚未同步当天新单。已识别为 DMS 单据时，数仓零行不能冒充最终答案：
    // 回落到后面的 business-lookup，由它通过独立 DMS 连接执行主表/明细表单表索引点查。
    // 只放行有生产登记的单据族；普通明细零行、聚合零行和数仓专属单据仍保持原行为。
    if rs.rows.is_empty()
        && route == "direct-doc"
        && cx.source.is_warehouse()
        && dms_semantic::document::resolve_document(cx.question, false)
            .and_then(|document| document.family.production)
            .is_some()
    {
        tracing::info!(route = %route, "数仓未命中已登记 DMS 单据，回落生产轻点查");
        return Ok(None);
    }
    let mut r = table_answer(&scoped, rs, route, t0);
    // direct-derive：SQL 展示文本头部带「推导口径」标记 —— 执行已经完成，
    // 这里只改给人看/给日志留痕的那份（query_log.sql、前端「查看 SQL」都是它）。
    r.sql = mark_derived_sql(&r.route, std::mem::take(&mut r.sql));
    // 【并行取数】KPI 基期（prev 环比 + comparisons 同比）与补充明细互不依赖：
    // 各自的 SQL 在装配期已定，唯一前置是主结果在手（`cur` 从主结果取、明细要主行非空）。
    // 公网 Doris 上每一次串行 fetch 都是用户可见的等待（主查询 + 环比 + 同比 + 明细最多
    // 4 个串行往返），这里一次并发掉。并行的只有「取数」—— 对 `r` 的改写仍在 join 之后
    // **按原顺序**逐个落（prev → comparisons → detail），终态与串行逐字节相同。
    let cur = r.rows.first().and_then(|row| row.first()).and_then(cell_num);
    let prev_specs: Vec<&(String, String)> = prev.iter().chain(comparisons.iter()).collect();
    let prevs = futures::future::join_all(
        prev_specs.iter().map(|(prev_sql, _)| fetch_prev(cx, prev_sql, cur)),
    );
    // 【单据卡】头行在手且带了明细 SQL → 补明细（缺席不塌卡：头行键值卡照旧给）
    let want_detail = detail.is_some() && r.row_count > 0;
    let dsql = detail.unwrap_or_default();
    let detail_rows = async { if want_detail { fetch_detail(cx, &dsql).await } else { None } };
    // 【同窗补充】销售单指标 KPI 落定后补一条五值（销售额/成本/收入/毛利额/毛利率）。
    // 触发判据收窄：仅 direct-agg 且主结果是**单行单值 KPI** —— 维度拆解/明细问题
    // 自带这些列，不挂补充；补充缺席同样不塌主卡。
    let want_context = sales_context.is_some()
        && r.route == "direct-agg"
        && r.row_count == 1
        && r.columns.len() == 1;
    let csql = sales_context.unwrap_or_default();
    let context_rows = async { if want_context { fetch_sales_context(cx, &csql).await } else { None } };
    let (prev_vals, detail_rows, context_rows) = tokio::join!(prevs, detail_rows, context_rows);
    for (spec, val) in prev_specs.iter().zip(prev_vals) {
        if let (Some(cur), Some(prev)) = (cur, val) {
            apply_prev(&mut r, cur, &spec.1, prev);
        }
    }
    if let Some(d) = detail_rows {
        let replace_primary = r.route == "direct-doc";
        attach_detail(d, replace_primary, &mut r);
    }
    if let Some(c) = context_rows {
        attach_sales_context(c, &mut r);
    }
    Ok(Some(r))
}

/// direct-derive 的 SQL 展示头标：推导口径的显式标记。**只改展示文本**（`AskResult.sql`）——
/// 参与执行的是 `scoped`，打标在它之后。其他 route 原样返回，一个字符都不多。
fn mark_derived_sql(route: &str, sql: String) -> String {
    if route == "direct-derive" {
        format!("-- 推导口径：由 ODS 明细推导，未经合同验证（route=direct-derive）\n{sql}")
    } else {
        sql
    }
}

/// 补充明细的取数半（供 `land` 与基期查询**并行**发起）。
/// 一切失败（闸门拒/执行错/零行）= None：调用方保留主结果 —— 两句 warn 与拆分前
/// `attach_detail` 的文案逐字相同（失败不许 `?` 掉整张卡）。
async fn fetch_detail(cx: &AskCtx<'_>, dsql: &str) -> Option<(String, dms_connector::source::RowSet)> {
    let scoped = match gate_on(cx.p, dsql, cx.scope, cx.ds_global, cx.source.dialect()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(err = %e, "单据明细闸门未过 → 只给头卡");
            return None;
        }
    };
    let drs = match cx.source.fetch(&scoped, MAX_ROWS, EXEC_TIMEOUT).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(err = %e, sql = %scoped.wire(), "单据明细取数失败 → 只给头卡");
            return None;
        }
    };
    if drs.rows.is_empty() {
        return None; // 无明细行（如调拨单无明细表）—— 头卡就是全部
    }
    Some((scoped.wire().to_string(), drs))
}

/// 补充明细的落账半：闸门 → 取数已在 `fetch_detail` 里完成，这里只把明细写进答案。
/// 单据查询把顶层换成明细；聚合查询写入独立 supplemental，顶层主指标的行列契约保持不变。
fn attach_detail(detail: (String, dms_connector::source::RowSet), replace_primary: bool, r: &mut AskResult) {
    let (d_sql, drs) = detail;
    // 头行键值先抽（`present::build` 对单行出的就是 Entity 卡；防御：没出就手工拼）
    let header_pairs = r
        .view
        .blocks
        .iter()
        .find_map(|b| match b {
            dms_kernel::present::Block::Entity { pairs } => Some(pairs.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            r.columns
                .iter()
                .cloned()
                .zip(r.rows.first().cloned().unwrap_or_default())
                .filter(|(_, v)| !v.is_null())
                .collect()
        });
    let d_rows = drs.rows.len();
    let d_trunc = d_rows >= MAX_ROWS;
    let dview = dms_semantic::present::build(&drs.columns, &drs.rows);
    // SQL 展示两条都留（头查询 + 明细查询，按执行序）。
    r.sql = format!("{};\n\n-- 明细\n{}", r.sql, d_sql);
    if !replace_primary {
        r.supplemental = Some(SupplementalResult {
            columns: drs.columns,
            rows: drs.rows,
            row_count: d_rows,
            truncated: d_trunc,
            view: dview,
        });
        return;
    }
    // 单据卡维持既有契约：头查询只负责 Entity 头卡，顶层行列给真实业务明细，
    // CSV 与单据型深度分析因此仍拿明细行。
    r.columns = drs.columns;
    r.rows = drs.rows;
    r.row_count = d_rows;
    r.truncated = d_trunc;
    let leading = std::mem::take(&mut r.view.blocks)
        .into_iter()
        .find(|b| matches!(b,
            dms_kernel::present::Block::Entity { .. }
                | dms_kernel::present::Block::Kpis { .. }
        ));
    let mut blocks: Vec<_> = leading.into_iter().collect();
    if blocks.is_empty() {
        blocks.push(dms_kernel::present::Block::Entity { pairs: header_pairs });
    }
    blocks.extend(dview.blocks);
    r.view.blocks = blocks;
    r.view.columns = dview.columns;
    r.view.interact = dview.interact;
    if dview.insight.is_some() {
        r.view.insight = dview.insight;
    }
}

/// 同窗补充的取数半（供 `land` 与基期/明细**并行**发起）。与主查询同一条闸门、
/// 同一次权限注入、同一个执行超时 —— 补充不是第二条通道，是同口径的再一次取数。
/// 一切失败（闸门拒/执行错/零行）= None：补充缺席不塌主答案（文案与 `fetch_detail` 同族）。
async fn fetch_sales_context(cx: &AskCtx<'_>, csql: &str) -> Option<dms_connector::source::RowSet> {
    let scoped = match gate_on(cx.p, csql, cx.scope, cx.ds_global, cx.source.dialect()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(err = %e, "销售同窗补充闸门未过 → 只给主 KPI");
            return None;
        }
    };
    let crs = match cx.source.fetch(&scoped, MAX_ROWS, EXEC_TIMEOUT).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(err = %e, sql = %scoped.wire(), "销售同窗补充取数失败 → 只给主 KPI");
            return None;
        }
    };
    if crs.rows.is_empty() {
        return None; // 窗口内无事实行 —— 主 KPI 就是全部
    }
    Some(crs)
}

/// 同窗补充的落账半：只写独立 `sales_context` 字段。**不碰**主结果的 SQL 展示串与行列 ——
/// 金标把展示 SQL 逐字钉死（含 `-- 明细` 附录），主标量行列是 API/CSV/评测的主契约。
fn attach_sales_context(crs: dms_connector::source::RowSet, r: &mut AskResult) {
    r.sales_context = Some(SalesContextResult { columns: crs.columns, rows: crs.rows });
}

/// KPI 环比：单指标聚合时查上期算 Δ%（闸门失败=跳过环比，与拆分前同）。
/// 顺序与 `pipeline.rs:677-688` 一字不差：`cur` 与 `gate_on(prev)` 在同一个元组里求值
/// （即 `cur` 为 `None` 时闸门照走一遍，纯 CPU 无 IO），两者都成才发上期那次取数。
///
/// 本函数只是**取数半**（供 `land` 把 prev/comparisons 多路并行发起）；
/// 取数失败/零行/非数 = None（基期缺席不塌主答案，与原 `if let Ok(prs)` 的静默跳过一致）。
async fn fetch_prev(cx: &AskCtx<'_>, prev_sql: &str, cur: Option<f64>) -> Option<f64> {
    let (Some(_), Ok(prev_scoped)) = (
        cur,
        gate_on(cx.p, prev_sql, cx.scope, cx.ds_global, cx.source.dialect()),
    ) else {
        return None;
    };
    let prs = cx.source.fetch(&prev_scoped, MAX_ROWS, EXEC_TIMEOUT).await.ok()?;
    prs.rows.first().and_then(|row| row.first()).and_then(cell_num)
}

/// KPI 基期值的**落账半**：patch 视图 + `comparisons` 去重追加。顺序敏感
/// （去重判据读的是累积中的 `r.comparisons`），所以留在 `land` 里按
/// prev → comparisons 的原序逐个做 —— 与拆分前逐次 `patch_prev` 的终态逐字节相同。
fn apply_prev(r: &mut AskResult, cur: f64, label: &str, prev: f64) {
    dms_semantic::present::patch_kpi_delta(&mut r.view, cur, prev, label.to_string());
    if !r.comparisons.iter().any(|item| item.label == label) {
        // 基期为 0 仍是有效业务事实（例如新业务同比新增），不能把整项比较吞掉。
        // `pct=0` 只是底层兼容占位；深度报告会结合 baseline 输出“新增”，不伪造 0%。
        let pct = if prev.abs() >= f64::EPSILON {
            (cur - prev) / prev * 100.0
        } else {
            0.0
        };
        r.comparisons.push(crate::ctx::KpiComparison {
            label: label.to_string(),
            current: cur,
            baseline: prev,
            change: cur - prev,
            pct: (pct * 10.0).round() / 10.0,
            dir: if cur - prev > 0.000_001 {
                "up"
            } else if cur - prev < -0.000_001 {
                "down"
            } else {
                "flat"
            },
        });
    }
}

/// JSON 单元格 → f64（DECIMAL 存字符串，数字直取）。逐行搬 `pipeline.rs:172-178`。
/// 使用者只有 KPI 环比，故按迁移表「随各自使用者走」落在本文件。
pub fn cell_num(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// KPI 环比唯一的纯判据。**DECIMAL 那一支不能丢**：`fetch` 把 DECIMAL 映成字符串保精度
    /// （`connector/src/mysql.rs`），只认 `Number` 就等于「销售额类指标永远算不出环比」——
    /// 而那不会报错，只是 Δ% 静默消失。
    #[test]
    fn cell_num_reads_decimal_strings() {
        use serde_json::{json, Value};
        assert_eq!(cell_num(&json!(12)), Some(12.0));
        assert_eq!(cell_num(&json!(1.5)), Some(1.5));
        assert_eq!(cell_num(&Value::String(" 1234.56 ".into())), Some(1234.56));
        // 非数：不算 0（0 会被当成「上期为 0」算出 +∞% 的假环比）
        assert_eq!(cell_num(&Value::String("华南大区".into())), None);
        assert_eq!(cell_num(&Value::Null), None);
        assert_eq!(cell_num(&json!([1])), None);
    }

    /// 交接单里那段 wire 形态**能编译**：具名 `fn` 强转成 `Produce`、成员能进
    /// `Vec<Box<dyn Answerer>>`、表标签在 `ROUTER_ORDER` 里。
    /// 这条只跑构造（`answer()` 要池，agent 里不许造池）—— 但 HRTB 的强转正是最容易在
    /// wire 那天才炸的地方，把它挪到本轮来炸。
    #[test]
    fn wire_shape_compiles_for_both_members() {
        fn no_hit<'a>(_cx: &'a AskCtx<'a>) -> BoxFut<'a, Option<DirectHit>> {
            Box::pin(async { None })
        }
        for label in ["direct-agg", "direct-doc"] {
            let a: Box<dyn Answerer> = Box::new(HitAnswerer::new(label, Box::new(no_hit)));
            assert_eq!(a.route(), label);
            assert!(crate::ROUTER_ORDER.contains(&a.route()), "表标签不在 Router 顺序里：{label}");
        }
    }

    /// 【单据卡】接线判据（源码扫描 —— 取数/落账两半各要池与闸门，无库测不了）：
    /// ① detail 缺席不碰原答案；② 一切明细失败 = 保留头卡（不许 `?` 掉整张卡）；
    /// ③ 主表必须换成明细（用户要的是明细行，头行 80 列只是「这是什么单」）。
    #[test]
    fn doc_card_detail_is_additive_and_never_breaks_the_header_card() {
        let src = include_str!("hits.rs");
        // ① land() 里 detail 门：只有 detail 在且头行在才取明细
        let land = src.split("pub async fn land(").nth(1).expect("land 没了");
        assert!(land.contains("detail.is_some() && r.row_count > 0"), "{land}");
        // ② 取数半（fetch_detail）两处失败路径都是 warn + None（不是 `?`）：闸门拒、执行错
        let fetch = src.split("async fn fetch_detail(").nth(1).expect("fetch_detail 没了");
        assert!(fetch.contains("单据明细闸门未过 → 只给头卡"), "{fetch}");
        assert!(fetch.contains("单据明细取数失败 → 只给头卡"), "{fetch}");
        assert!(!fetch.contains("fetch_detail(cx"), "切片吃到调用点了（本判据只判函数体）");
        // ③ 落账半（attach_detail）：主表换明细 + 视图头插 Entity
        let body = src.split("fn attach_detail(").nth(1).expect("attach_detail 没了");
        for anchor in ["r.columns = drs.columns", "r.rows = drs.rows",
                       "Block::Entity { pairs: header_pairs }"] {
            assert!(body.contains(anchor), "缺 {anchor}：{body}");
        }
    }

    /// 聚合答案的主标量是问句的直接答案，结构/明细只能作为补充结果附加。
    /// 单据卡仍以明细替换头表，两条路径必须由 `replace_primary` 明确区分。
    #[test]
    fn aggregate_detail_preserves_the_primary_result_contract() {
        let src = include_str!("hits.rs");
        let body = src
            .split("fn attach_detail(")
            .nth(1)
            .expect("attach_detail 没了")
            .split("/// KPI 环比")
            .next()
            .expect("attach_detail 函数边界没了");
        assert!(body.contains("if !replace_primary"), "缺聚合/单据分流：{body}");
        let aggregate = body
            .split("if !replace_primary")
            .nth(1)
            .and_then(|tail| tail.split("// 单据卡维持既有契约").next())
            .expect("聚合分支没了");
        assert!(aggregate.contains("r.supplemental = Some(SupplementalResult"), "聚合明细没有进入 supplemental");
        assert!(aggregate.contains("return;"), "聚合分支必须提前返回，不能落入主表替换");
        assert!(!aggregate.contains("r.columns = drs.columns"), "聚合分支覆盖了主结果");
        assert!(body.contains("r.columns = drs.columns"), "单据卡不再替换主表");
        assert!(body.contains("columns: drs.columns"), "补充结果缺列：{body}");
        assert!(body.contains("rows: drs.rows"), "补充结果缺行：{body}");
        assert!(body.contains("row_count: d_rows"), "补充结果缺行数：{body}");
    }

    /// 【同窗补充】接线判据（源码扫描 —— 取数要池与闸门，无库测不了）：
    /// ① 触发收窄到「direct-agg 且单行单值 KPI」；② 一切失败 = None 不塌主答案；
    /// ③ 落账只写 `sales_context`，主 SQL 展示串与主结果行列一个字不动（金标逐字钉死）。
    #[test]
    fn sales_context_is_additive_gated_and_never_touches_the_primary_answer() {
        let src = include_str!("hits.rs");
        // ① land() 触发门：补充 SQL 在手 + direct-agg + 单行 + 单列（维度拆解/明细不挂）。
        //    切片止于 land 之后的 mark_derived_sql 文档，判据只落在 land 函数体上。
        let land = src
            .split("pub async fn land(")
            .nth(1)
            .expect("land 没了")
            .split("/// direct-derive 的 SQL 头标")
            .next()
            .expect("land 边界没了");
        for anchor in [
            "sales_context.is_some()",
            "r.route == \"direct-agg\"",
            "r.row_count == 1",
            "r.columns.len() == 1",
            "attach_sales_context(c, &mut r)",
        ] {
            assert!(land.contains(anchor), "同窗补充触发门缺 {anchor}：{land}");
        }
        // ② 取数半（fetch_sales_context）两处失败路径都是 warn + None（不是 `?`），零行同样 None
        let fetch = src
            .split("async fn fetch_sales_context(")
            .nth(1)
            .expect("fetch_sales_context 没了")
            .split("/// 同窗补充的落账半")
            .next()
            .expect("fetch_sales_context 边界没了");
        assert!(fetch.contains("销售同窗补充闸门未过 → 只给主 KPI"), "{fetch}");
        assert!(fetch.contains("销售同窗补充取数失败 → 只给主 KPI"), "{fetch}");
        assert!(fetch.contains("crs.rows.is_empty()"), "{fetch}");
        assert!(!fetch.contains("fetch_sales_context(cx"), "切片吃到调用点了（本判据只判函数体）");
        // ③ 落账半：只写独立字段；不给 r.sql / r.columns / r.rows 赋值
        let body = src
            .split("fn attach_sales_context(")
            .nth(1)
            .expect("attach_sales_context 没了")
            .split("/// KPI 环比")
            .next()
            .expect("attach_sales_context 边界没了");
        assert!(body.contains("r.sales_context = Some(SalesContextResult"), "{body}");
        assert!(body.contains("columns: crs.columns") && body.contains("rows: crs.rows"), "{body}");
        for forbidden in ["r.sql =", "r.columns =", "r.rows =", "r.view ="] {
            assert!(!body.contains(forbidden), "同窗补充不许改主回答 {forbidden}：{body}");
        }
    }

    #[test]
    fn warehouse_zero_row_document_falls_through_to_production_lookup() {
        let src = include_str!("hits.rs");
        let land = src
            .split("pub async fn land(")
            .nth(1)
            .expect("land missing")
            .split("/// 补充明细")
            .next()
            .unwrap();
        for anchor in [
            "rs.rows.is_empty()",
            "route == \"direct-doc\"",
            "cx.source.is_warehouse()",
            "resolve_document(cx.question, false)",
            ".and_then(|document| document.family.production)",
            "return Ok(None)",
        ] {
            assert!(land.contains(anchor), "数仓零行单据缺生产轻点查回落：{anchor}");
        }
    }

    /// direct-derive 的 SQL 头标：只有推导 route 的展示 SQL 带标记，其他 route 逐字不动。
    /// 标记在**执行之后**才落到 `AskResult.sql`（query_log.sql 与前端「查看 SQL」都是这份），
    /// 所以不可能影响闸门与执行。
    #[test]
    fn derived_sql_carries_the_trust_mark_and_others_untouched() {
        let marked = mark_derived_sql("direct-derive", "SELECT 1".to_string());
        assert!(marked.starts_with("-- 推导口径"), "{marked}");
        assert!(marked.contains("未经合同验证") && marked.ends_with("SELECT 1"), "{marked}");
        assert_eq!(mark_derived_sql("direct-agg", "SELECT 1".to_string()), "SELECT 1");
        assert_eq!(mark_derived_sql("llm", "SELECT 1".to_string()), "SELECT 1");
        // 接线钉点：land 必须在 table_answer 之后立刻打标（晚于它就是没打）
        let src = include_str!("hits.rs");
        let land = src
            .split("pub async fn land(")
            .nth(1)
            .expect("land 没了")
            .split("/// 补充明细")
            .next()
            .expect("land 边界没了");
        let built = land.find("table_answer(&scoped, rs, route, t0)").expect("land 构造点没了");
        let marked_at = land.find("mark_derived_sql(&r.route").expect("land 里没给推导 SQL 打标");
        assert!(built < marked_at, "打标必须在 table_answer 之后");
    }
}
