//! 图成员：`Relation` → 三条 Cypher 之一 → 三列表格（0-LLM）。变更原因＝图关系问答的落地。
//!
//! 搬运源 `server/src/pipeline.rs:643-650`（门禁）+ `1035-1078`（`try_graph`），逐行搬运。
//! 图 IO 在 `dms_connector::graph`（T9-A1 已迁）。
//!
//! 🔴 **本成员唯一的权限逻辑就是 `accept`，一个条件都不许丢**：图库里没有行级权限
//! （边是全量聚合出来的），Cypher 也过不了三段闸门 —— 所以只对**有资格免注入**的身份开放，
//! 受限用户一律回落 LLM 走 `inject`。`regression_cases.json` 的 F04（tanlibo/city_manager）
//! 断言 `route_not=graph`，F01-F03（admin）断言 `route=graph`。

use std::time::Instant;

use sqlx::PgPool;

use dms_connector::graph;
use dms_kernel::BoxFut;
use dms_policy::{scope::Scope, Principal};

use crate::answerers::Answerer;
use crate::ctx::{AskCtx, AskResult};

/// 图关系问题类型（AGE 图查询）。逐行搬 `server/src/direct.rs:19-27`，**变体名一字不改**：
/// `Debug` 输出进 `AskResult.sql`（`[AGE 图查询] BuyersOfGoods("烤肠")`），改名即改前端可见的字节。
/// ponytail: 与 `DirectHit` 同一笔临时重复 —— 识别函数与本枚举的终态在 semantic
/// （T8 的 `fastpath/relation.rs`），届时本枚举删掉。
#[derive(Debug, PartialEq)]
pub enum Relation {
    /// 买过某商品的客户（含实体名）
    BuyersOfGoods(String),
    /// 某客户买过什么
    GoodsOfCustomer(String),
    /// 买某商品还买什么（共购）
    Copurchase(String),
}

/// 问句 → 关系。实现（`direct::detect_relation`，顺序敏感）仍在 server，故由 wire 侧注入：
/// `GraphAnswerer::new(Box::new(detect))`。用具名 `fn` 强转，理由同 `hits::Produce`。
pub type Detect = Box<dyn Fn(&str) -> Option<Relation> + Send + Sync>;

pub struct GraphAnswerer {
    detect: Detect,
}

impl GraphAnswerer {
    pub fn new(detect: Detect) -> Self {
        Self { detect }
    }

    /// 问句是否是图关系问句（`accept` 的第二个合取项，单独暴露只为可单测）。
    pub fn hit(&self, question: &str) -> Option<Relation> {
        (self.detect)(question)
    }
}

/// 该身份这一轮是否**有资格免注入**（＝能不能铸出 `UnrestrictedProof`）。
/// 铸造点只有 `dms_policy::proof::for_principal`（F2），它要**两个独立证据**：
/// 集合三维度全空 + 角色档确实授予全部。纯函数、无 IO。
pub fn has_proof(p: &Principal, scope: &Scope) -> bool {
    dms_policy::for_principal(p, scope).is_some()
}

impl Answerer for GraphAnswerer {
    fn route(&self) -> &'static str {
        "graph"
    }

    /// 图关系快路径（AGE，0-LLM）：仅全权限用户，且当前数仓目标已有同目标成功快照。
    fn accept(&self, cx: &AskCtx<'_>) -> bool {
        has_proof(cx.p, cx.scope)
            && cx.source.is_warehouse()
            && graph::is_ready_for(cx.source_name)
            && !has_unverified_graph_dimension(cx.question)
            && self.hit(cx.question).is_some()
    }

    fn answer<'a>(&'a self, cx: &'a AskCtx<'a>) -> BoxFut<'a, anyhow::Result<Option<AskResult>>> {
        Box::pin(async move {
            if has_unverified_graph_dimension(cx.question) {
                return Ok(None);
            }
            // 第二次识别（`accept` 已判过一次）：纯 substring 扫问句，无 IO。
            // 分成两半是 Router 的形状要求 —— 权限门禁必须在 `accept` 里，否则漏掉 `accept`
            // 的调用方就绕过了它。
            let Some(rel) = self.hit(cx.question) else { return Ok(None) };
            let Some(lease) = graph::ready_lease(cx.source_name) else { return Ok(None) };
            let answer = try_graph(cx.pg, &rel, cx.t0).await;
            if !cx.source.is_warehouse() || !graph::lease_is_current(&lease) {
                tracing::info!(target = %cx.source_name, "图查询期间目标或快照代次变化，丢弃旧结果");
                return Ok(None);
            }
            Ok(answer)
        })
    }
}

/// 图关系查询 → AskResult（表格形态）。查询失败/空结果返回 None（回落 LLM）。
pub async fn try_graph(pg: &PgPool, rel: &Relation, t0: Instant) -> Option<AskResult> {
    let (entity_label, mut rows_data) = match rel {
        Relation::BuyersOfGoods(name) => ("客户", graph::buyers_of_goods(pg, name, 50).await.ok()?),
        Relation::GoodsOfCustomer(name) => ("商品", graph::goods_of_customer(pg, name, 50).await.ok()?),
        Relation::Copurchase(name) => ("商品", graph::copurchase(pg, name, 50).await.ok()?),
    };
    // 🔴 空结果**不直接放弃**：先问一句「剥词剩下的这坨到底是什么」。
    //
    // 实测缺口（三条问句同一个根因）：
    // ```text
    // 买过烤肠的客户        → graph 50 行 ✅
    // 湖南省买过烤肠的客户  → 剩「湖南省烤肠」→ 当商品名模糊查 → 0 行
    // 买过肉制品的客户      → 「肉制品」是分类名、不在任何商品名里 → 0 行
    // ```
    // 剥词本身没错，错在**剥完之后没人解析剩下的词**。`resolve_entities` 只解析
    // 已验证的商品和省区；分类、省份等语义在出手前 fail-closed，交给后续 SQL/LLM 路径。
    //
    // 只对 `BuyersOfGoods` 做：另两条形态的实体是**客户名**（开集、图里 2606 个），
    // 拿它去试 SalesRegion 只会白查几次图。
    if rows_data.is_empty() {
        if let Relation::BuyersOfGoods(raw) = rel {
            rows_data = resolved_buyers(pg, raw).await?;
        }
    }
    if rows_data.is_empty() {
        return None;
    }
    let columns = vec![
        format!("{entity_label}编码"),
        format!("{entity_label}名称"),
        "购买额".to_string(),
    ];
    let rows: Vec<Vec<serde_json::Value>> = rows_data
        .iter()
        .map(|g| {
            vec![
                serde_json::Value::from(g.code.clone()),
                serde_json::Value::from(g.name.clone()),
                serde_json::Value::from(format!("{:.2}", g.amount)),
            ]
        })
        .collect();
    let row_count = rows.len();
    let view = dms_semantic::present::build(&columns, &rows);
    Some(AskResult {
        sql: format!("[AGE 图查询] {rel:?}"),
        columns,
        truncated: false,
        row_count,
        rows,
        elapsed_ms: t0.elapsed().as_millis(),
        route: "graph".into(),
        view,
        supplemental: None,
        comparisons: vec![],
        subs: vec![],
        caliber_note: None, // 图查询不产 SQL，没有可判的口径
        // Cypher 自带 `LIMIT 50`，到不了 `MAX_ROWS`：截断提示恒缺席（`truncated` 同样恒 false）
        truncation_note: None,
        // 图查询走 AGE、不过 connector 的敏感列防线（列是 code/name/amount 三个固定列）
        redacted: vec![],
        // 图查询不过行权限注入器（走的是 AGE 的 Cypher，不是 ScopedSql）
        scope_note: None,
        trust: None,
        // 由 `ask_single` 的分派循环在命中后补上
        steps: vec![],
    })
}

/// 把剥词残留解析成 (商品, 省区) 再查一次。`None` = 解析不出任何东西。
///
/// 🔴 **解析结果必须被全部用上**，否则返回 `None` 而不是「用能用的那部分」：
/// 那正是本仓反复付账的「消化了词却不装过滤」——「湖南省买过烤肠的客户」如果只用了「烤肠」
/// 而丢掉「湖南省」，用户会拿到**全国**的客户名单，零报错、route 还是 `graph`。
/// 这里的判据是：解析出的实体数必须等于装上的限定数，且残留里**不许有没被解析掉的中文**。
async fn resolved_buyers(pg: &PgPool, raw: &str) -> Option<Vec<graph::GraphRow>> {
    let found = graph::resolve_entities(pg, raw).await.ok()?;
    let (goods, sales_region, province) = into_slots(&found, raw)?;
    let rows = graph::buyers_filtered(
        pg,
        goods.as_deref(),
        sales_region.as_deref(),
        province.as_deref(),
        50,
    )
    .await
    .ok()?;
    if !rows.is_empty() {
        tracing::info!(
            goods = ?goods, sales_region = ?sales_region, province = ?province,
            rows = rows.len(), "图问句限定词解析成功"
        );
    }
    Some(rows)
}

/// 解析结果 → 两个单值槽。`None` = 不许装配（照旧回落）。
///
/// 抽成纯函数就为了让**覆盖率那一条**可测：它是本改动唯一能造成静默错答的地方。
/// 三种拒绝理由，每种都在判据里有一条：
/// ① 什么都没解析出来；② 同类实体出现两次（装不进单值槽，猜哪个是主都是错的）；
/// ③ **覆盖不全** —— 残留里还有没被解析掉的汉字，说明有限定词没被理解，
///    而没被理解的限定词 = 静默丢过滤（「湖南省买过烤肠的客户」答成全国名单）。
fn into_slots(
    found: &[graph::Hit],
    raw: &str,
) -> Option<(Option<String>, Option<String>, Option<String>)> {
    if found.is_empty() {
        return None;
    }
    let (mut goods, mut sales_region, mut province) = (None, None, None);
    for h in found {
        match &h.entity {
            graph::Entity::Goods(g) if goods.is_none() => goods = Some(g.clone()),
            graph::Entity::SalesRegion(r) if sales_region.is_none() => {
                sales_region = Some(r.clone())
            }
            graph::Entity::Province(p) if province.is_none() => province = Some(p.clone()),
            _ => return None, // 同类第二次
        }
    }
    // 🔴 按 **窗口** 算，不是按实体名算。实测的静默错答就出在这一行：
    // 「湖南省烤肠」的窗口「烤肠」模糊匹到 `皇家小虎黑猪肉烤肠（原味）0500G00`，
    // 按实体名算 covered=13 ≥ 5 → 判据被绕过 → 「湖南省」整个丢掉 → 全国 27 个客户。
    let covered: usize = found.iter().map(|h| hanzi_count(&h.window)).sum();
    let required = constraint_hanzi_count(raw);
    if covered < required {
        tracing::info!(
            residue = %raw, covered, total = required,
            "图问句的限定词只解析出一部分 —— 回落而不是丢掉没懂的那半"
        );
        return None;
    }
    Some((goods, sales_region, province))
}

/// 未验证维度必须交给 SQL/LLM 口径链路，不能让图查询忽略维度词后返回更大范围。
fn has_unverified_graph_dimension(question: &str) -> bool {
    let unsupported = [
        "商品分类",
        "商品类型",
        "商品大类",
        "商品小类",
        "肉制品",
        "分类",
        "品类",
        "类别",
        "大类",
        "小类",
        "类型",
        "品种",
        "品牌",
        "客户类型",
        "按省",
        "各省",
        "战区",
        "大区",
        "销售区域",
        "区域",
        "地区",
        "城市",
        "地市",
        "市级",
        "区县",
        "县区",
        "渠道",
        "业务员",
        "经理",
    ];
    unsupported.iter().any(|word| question.contains(word))
}

/// 汉字数。只数汉字：残留里可能有剥词剩下的标点/空格，那些不算「没理解的限定词」。
fn hanzi_count(s: &str) -> usize {
    s.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count()
}

/// “省区/省份”是选择已验证 `region/state` 的维度标签，不是业务值；覆盖率只要求实体值被解析。
fn constraint_hanzi_count(s: &str) -> usize {
    hanzi_count(&s.replace("省区", "").replace("省份", ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    use dms_policy::scope::ScopeSets;

    fn answerer() -> GraphAnswerer {
        // 替身识别器：只认一种问法，够用来证「accept 的两个合取项各自都能否决」
        GraphAnswerer::new(Box::new(|q| {
            q.contains("买过烤肠").then(|| Relation::BuyersOfGoods("烤肠".into()))
        }))
    }

    /// 🔴 权限门禁：**无 proof 必假**。`accept` = `has_proof && hit`，两个合取项在这里各自被否决一次。
    /// 少了 `has_proof` 这一半，受限用户就能从图库读到全量购买关系（图里没有行级过滤，
    /// 且 Cypher 根本进不了三段闸门）—— 而那不会报错，回归里只有 F04 的 `route_not=graph` 会红。
    #[test]
    fn accept_requires_proof_and_relation() {
        let a = answerer();
        let p = crate::gate::anyone();
        // ① 受限用户（集合非空）→ 铸不出 proof → 不许出手，问句再像图问句也不行
        let restricted = Scope::new(ScopeSets { employee_ids: vec![7], ..Default::default() }, false);
        assert!(!has_proof(&p, &restricted));
        assert!(a.hit("买过烤肠的客户有哪些").is_some(), "问句本身是图问句");
        // ② F2 双证据：集合全空但角色档没授予全部（= 忘了算权限）→ 同样铸不出
        assert!(!has_proof(&p, &Scope::new(ScopeSets::default(), false)));
        // ③ 全权限档（集合全空 + 角色档授予全部）→ 有资格
        assert!(has_proof(&p, &Scope::new(ScopeSets::default(), true)));
        // ④ 有资格但问句不是图问句 → 第二个合取项否决
        assert!(a.hit("本月销售额").is_none());
        // ⑤ 成员能进 `Vec<Box<dyn Answerer>>` 且表标签是 Router 的第一位
        let b: Box<dyn Answerer> = Box::new(answerer());
        assert_eq!(b.route(), crate::ROUTER_ORDER[0]);
    }

    /// 🔴 归槽 + **覆盖率**判据。覆盖率那一条是本改动唯一能造成静默错答的地方：
    /// 「湖南省买过烤肠的客户」如果只用了「烤肠」而丢掉「湖南省」，
    /// 用户拿到的是**全国**名单 —— 零报错、route 还是 `graph`、行数看起来很正常。
    #[test]
    fn slots_refuse_partial_understanding() {
        use dms_connector::graph::{Entity, Hit};
        let hit = |start: usize, window: &str, entity: Entity| Hit {
            start,
            window: window.into(),
            entity,
        };
        let g = |s: &str| Entity::Goods(s.into());
        let r = |s: &str| Entity::SalesRegion(s.into());
        let pv = |s: &str| Entity::Province(s.into());

        // ① 正常：窗口拼起来正好覆盖残留 → 商品、省区/省份槽按类型各就各位
        let found = vec![hit(0, "湖南省", pv("湖南省")), hit(3, "烤肠", g("烤肠"))];
        let (gg, rr, pp) = into_slots(&found, "湖南省烤肠").expect("该装配");
        assert_eq!((gg.as_deref(), rr.as_deref(), pp.as_deref()), (Some("烤肠"), None, Some("湖南省")));
        let region_found = vec![hit(0, "湘北省区", r("湘北省区")), hit(4, "烤肠", g("烤肠"))];
        let (_, rr, _) = into_slots(&region_found, "湘北省区烤肠").expect("省区该装配");
        assert_eq!(rr.as_deref(), Some("湘北省区"));

        // ② 🔴🔴 **实测过的那次静默错答**，逐字重演：
        // 「湖南省烤肠」里窗口「烤肠」模糊匹到 `皇家小虎黑猪肉烤肠（原味）0500G00`，
        // 地域限定一个都没解析出来（第一版地域节点名是行政编码 `430000`）。
        // 按**实体名**算 covered=13 ≥ 5 → 判据放行 → 用户拿到**全国** 27 个客户
        // （日志原文：goods=Some("皇家小虎黑猪肉烤肠（原味）0500G00") region=None rows=27）。
        // 按**窗口**算 covered=2 < 5 → 拒。这一条就是那个 bug 的判据。
        let real_bug = vec![hit(3, "烤肠", g("皇家小虎黑猪肉烤肠（原味）0500G00"))];
        assert!(
            into_slots(&real_bug, "湖南省烤肠").is_none(),
            "实体名比窗口长时覆盖率判据被绕过 —— 静默答成全国名单"
        );

        // ③ 覆盖不全 → 拒。「广东」没被解析掉（残留 8 字、窗口只覆盖 5 字）
        let partial = vec![hit(3, "湖南省", pv("湖南省")), hit(6, "烤肠", g("烤肠"))];
        assert!(into_slots(&partial, "广东省湖南省烤肠").is_none(), "静默丢限定词");
        // 差**一个字**也要拒（容差多少都是猜）
        assert!(into_slots(&vec![hit(1, "烤肠", g("烤肠"))], "鲜烤肠").is_none());

        // ④ 同类两次 → 拒（猜哪个是主都是错的）
        let two_goods = vec![hit(0, "烤肠", g("烤肠")), hit(2, "火腿", g("火腿"))];
        assert!(into_slots(&two_goods, "烤肠火腿").is_none());
        let two_regions = vec![hit(0, "湖南省", pv("湖南省")), hit(3, "广东省", pv("广东省"))];
        assert!(into_slots(&two_regions, "湖南省广东省").is_none());

        // ⑤ 什么都没解析出来 → 拒
        assert!(into_slots(&[], "不知道是什么").is_none());

        // ⑥ 非汉字不算「没理解的限定词」（剥词会剩标点/空格）
        assert!(into_slots(&vec![hit(0, "烤肠", g("烤肠"))], "烤肠 ").is_some());
        assert!(into_slots(&vec![hit(1, "烤肠", g("烤肠"))], "「烤肠」").is_some());
        assert_eq!(hanzi_count("烤肠 ABC「」"), 2);
        assert_eq!(constraint_hanzi_count("省区华中烤肠"), 4);
    }

    #[test]
    fn unverified_dimensions_fail_closed_before_graph_io() {
        for q in [
            "买过肉制品分类的客户",
            "买过肉制品的客户",
            "按品类看买过烤肠的客户",
            "各省买过烤肠的客户",
            "华中战区买过烤肠的客户",
        ] {
            assert!(has_unverified_graph_dimension(q), "should fall back: {q}");
        }
        // 省份已是已验证维度（Province 节点来自事实 `state`）：省区、省份两种问法都进图
        assert!(!has_unverified_graph_dimension("湖南省区买过烤肠的客户"));
        assert!(!has_unverified_graph_dimension("湖南省买过烤肠的客户"));
        assert!(!has_unverified_graph_dimension("湖南省份买过烤肠的客户"));
    }

    #[test]
    fn accept_is_bound_to_current_warehouse_graph() {
        let src = include_str!("graph.rs");
        let body = src
            .split("fn accept(&self, cx: &AskCtx<'_>) -> bool {")
            .nth(1)
            .expect("accept 不见了")
            .split("\n    }")
            .next()
            .unwrap();
        assert!(body.contains("cx.source.is_warehouse()"), "production_lookup 仍可能进图：{body}");
        assert!(body.contains("graph::is_ready_for(cx.source_name)"), "图未绑定当前目标：{body}");
    }

    /// `Relation` 是从 `server/src/direct.rs` 复制过来的第二份定义，
    /// 而它的 `Debug` 输出**直接进 `AskResult.sql`**（前端与判官都读那个字段）。
    /// 这条钉住变体名不漂：改名/加字段即红。
    #[test]
    fn debug_form_is_the_sql_field_contract() {
        let s = format!("[AGE 图查询] {:?}", Relation::BuyersOfGoods("烤肠".into()));
        assert_eq!(s, "[AGE 图查询] BuyersOfGoods(\"烤肠\")");
        assert_eq!(format!("{:?}", Relation::GoodsOfCustomer("恒众".into())), "GoodsOfCustomer(\"恒众\")");
        assert_eq!(format!("{:?}", Relation::Copurchase("烤肠".into())), "Copurchase(\"烤肠\")");
    }
}
