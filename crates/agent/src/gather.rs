//! prompt 素材的 **IO 装配**：多路召回 + few-shot 语料 + 规则时间段 → `PromptCtx`。
//! 变更原因＝「一次生成要召回哪些东西、按什么顺序」。渲染在 `prompt.rs`（纯函数，无库无网可单测）。
//!
//! 搬运源 `server/src/pipeline.rs:203-215`（`fewshot_block`）与 `229-326`（`generate_sql` 的召回段）。
//! 下面这行是**拆分前的串行出处顺序**（历史存档，召回路后来已扩到十几路）：
//! 表召回 → 指标 → 维度 → 术语 → 元素 → 教训 → few-shot → 取值编码 → 值域命中。
//! 现行的 await 顺序是分波并行，见 `gather` 开头的波次注释。
//! 依赖没变：教训召回要吃前面拿到的表名、元素召回要与前三路去重，换位就换了输入。
//!
//! 🔴 **agent 不许自己写 `meta.*` 的 SQL**：一律经 `dms_semantic::{recall::*, registry::exemplar::*}`。
//! 否则 ds 谓词与 visibility 两道总闸的漂移守卫（`semantic/tests/drift.rs` 扫的是 semantic 的源码）
//! 扫不到这里，跨源/跨用户的泄露就没有任何测试能抓。

use std::fmt::Write;

use sqlx::PgPool;

use dms_connector::embed::{to_pgvector, EmbedClient, EmbedMode};
use dms_kernel::nl::time::time_predicate;
use dms_semantic::recall::{self, RecallCtx, TableCtx};
use dms_semantic::registry::exemplar;
use dms_semantic::registry::model::{
    load_dimensions, load_join_edges, load_metrics, DimensionDef, JoinEdge, MetricDef,
};

use crate::ctx::{AskCtx, ContextCard, ContextSummary};
use crate::prompt::PromptCtx;

/// 表召回/教训召回的条数上限：波1 `rc0`、波3 `rc_pitfalls`、回炉 `schema_section` 三处同一值
/// （拆分前 pipeline.rs 就是 6，改一边另一边不跟着变 —— 所以提常量）。
const RECALL_LIMIT: usize = 6;

/// 经验召回的条数上限：首轮 `gather` 波2 与回炉 `gather_all_cards` 两处同一值。
const MEMORY_LIMIT: usize = 3;

/// 一次生成的全部召回。第二个返回值 = 本轮的**规则召回面**（召回表 ∪ JOIN 对面表），
/// 给口径复核的 `caliber::build_rules` —— 值问题的码列判据只在对面表在列时才造得出
/// （LLM 不受召回约束照样 JOIN 对面表，规则必须在）。pitfalls 仍按召回原表取。
/// 第三个返回值 = 【A17 ②】口径二选一 chip（落选指标的改问建议，可空）。
///
/// 问句向量在这里算一次（semantic 不持 HTTP 客户端）：表召回与元素召回共用同一份，
/// embed 缺席（`None`）时两条向量路各自降级跳过 —— 与拆分前 `embed_query()` 返 `None` 等价。
pub async fn gather(
    cx: &AskCtx<'_>,
    embed: &EmbedClient,
) -> anyhow::Result<(PromptCtx, Vec<String>, Vec<String>)> {
    let t0 = std::time::Instant::now();
    // 【分波并行】原来 2 次 embed HTTP + 12 次 PG 召回**全串行**，每一问都白付这些往返之和。
    // 依赖序即波次序，波内互不依赖（每一路的降级语义——warn + 该卡缺席——与串行版逐路相同）：
    //   波1：两路 embed + 全部**不读向量**的召回（指标/维度/术语/few-shot/码值/值域/关联图/源背景）
    //   波2：要整句/切片向量的两路（表召回、经验召回）
    //   波3：吃前两波结果的两路（术语递归、教训）
    //   波4：吃「链好术语映射的前三路」的元素召回，与 JOIN 对面表卡片（吃关联图 + 召回表）
    // 【A8】问句切片向量：整句向量被长问句稀释时，专名片段（「烤肠」「湖南省」）照样打得中。
    // 滑窗用 kernel 那份（与图路径实体抽取同一个函数）；批量 embed 一次打完
    // （`embed_batch` 内部 64 一批），N 片只多一次往返。整句仍在首位 ——
    // 🔴 模式必须是 `Query`：元素卡的召回阈值（STRICT=0.35 / LOOSE=0.5 / DS_MAX_DIST）
    // 全是拿 **query 向量**标定的，拿 passage 向量去比会让口语化问法整体召回漂移；
    // 且 passage 熔断槽是知识库入库在用的 —— 一次入库失败会顺手掐掉 5 分钟的切片召回。
    // 短问句时整句本来就是最好的那片。embed 缺席 → 两个调用都快速熔断返 None，
    // `slice_vecs` 为空，`recall_elements` 退回「embed=None ⇒ 空」的老降级。
    const MAX_SLICES: usize = 24; // 20 字问句全窗口 60+ 片：embed 服务单线程，封顶（长词优先取前）
    // 问句只 `to_string` 一次：`seen` 与 `slice_texts` 首位共用同一份
    let q = cx.question.to_string();
    let mut seen = std::collections::HashSet::from([q.clone()]);
    let slice_texts: Vec<String> = std::iter::once(q)
        .chain(
            dms_kernel::nl::text::candidate_windows(cx.question)
                .into_iter()
                .map(|(_, w)| w)
                // 先判重再 clone：重复窗口不为注定失败的 insert 白付一次分配
                .filter(move |w| !seen.contains(w) && seen.insert(w.clone())),
        )
        .take(MAX_SLICES + 1)
        .collect();
    // 波1。`rc0` 的 embed 字段给 None：这几路本来就不读它（读 `embed`/`embed_slices` 的只有
    // `retrieve` 与 `recall_elements`，grep 可证）——哪天给某一路加了向量读取，必须把它挪去波2。
    let rc0 = RecallCtx {
        question: cx.question,
        tables: &[],
        limit: RECALL_LIMIT,
        ds: cx.ds,
        embed: None,
        embed_slices: &[],
    };
    let (qvec, slice_vecs, metric_hits, dims, terms, fewshot, value_hints, domain_hits, edges, ds_row) =
        tokio::join!(
            embed.embed_query(cx.question),
            embed.embed_batch(&slice_texts, EmbedMode::Query),
            recall::recall_metric_hits(cx.pg, &rc0),
            recall::recall_dimensions(cx.pg, &rc0),
            recall::recall_terms(cx.pg, &rc0),
            fewshot_block(cx.pg, cx.ds, cx.question),
            recall::recall_value_hints(cx.pg, &rc0),
            recall::recall_value_domains(cx.pg, &rc0),
            load_join_edges(cx.pg, cx.ds),
            dms_semantic::registry::datasource::get_datasource(cx.pg, cx.ds),
        );
    // 本轮召回降级清单（见 `PromptCtx::degraded`）：每条同时是日志文案
    let mut degraded: Vec<&'static str> = vec![];
    let qvec = qvec.map(|v| to_pgvector(&v));
    // `slice_vecs` 与 `qvec` 同一降级类（embed 缺席，熔断器自己记日志），不是 PG 召回失败 ——
    // 所以这里与上一行同类（都不进召回降级的 warn 判据），形态随各自类型（`.map` / `match`）。
    let slice_vecs: Vec<String> = match slice_vecs {
        Some(vs) => vs.iter().map(|v| to_pgvector(v)).collect(),
        None => vec![],
    };
    // 🔴 每一路召回失败一律**降级成「这张卡缺席」而不是 `?`**：少几张卡最多让 LLM 少看点素材，
    // 让整轮问答失败是过度反应（裁决 二·G 同族）。但**每一路都要吼一声** ——
    // 「召回为什么是空的」是本仓最高频的排查题，而原来这几行是纯静默 `unwrap_or_default()`：
    // PG 抖一下 / 谓词写错 / 表没建，日志里与「本来就没命中」完全无法区分。
    // 2026-08-14 起统一走 `degrade(...)`：**日志与 `PromptCtx::degraded` 同源**，
    // 口径卡（指标/维度）缺席时 `run_llm` 会给结果挂标注、trust 降 review ——
    // 「答错了还很自信」这条路就是这么堵上的。判据 `gather_warns_on_every_recall_degradation`
    // 钉着「本函数里再没有手抄的 map_err(warn)」。
    // 指标召回走 `MetricHit`（不走卡片版）：`time_cap` 只在结构化形态上 ——
    // 有指标声明「算到昨天」时，规则时间窗的右端必须当场压掉（实测：卡片提示与
    // 规则窗并排时，模型照抄规则窗，含今天虚 1.8%）。卡片仍由 `metric_card` 渲染。
    let metric_hits = degrade(metric_hits, "指标召回失败 → 指标卡缺席", &mut degraded);
    // `time_cap` 的 `.any()` 挪进 `time_tpl` 的闭包惰性算：问句无时间词时这一扫纯白跑
    let metrics: Vec<String> = metric_hits
        .iter()
        .map(|hit| recall::metric_card_for(cx.ds, hit))
        .collect();
    let dims = degrade(dims, "维度召回失败 → 维度卡缺席", &mut degraded);
    let terms = degrade(terms, "术语召回失败 → 术语卡缺席", &mut degraded);
    // 码值提示（SuperSonic value mapping 的生成前置版）：问句里的中文值 → 该列真实码。
    let value_hints = degrade(value_hints, "码值提示召回失败 → 码值提示缺席", &mut degraded);
    // 实体锚定并进码值段（同一段进 prompt、同一份「绝不丢」预算纪律）：
    // 问句里的名字探得主档唯一命中 → LLM 必须带实体谓词（实测漏写客户过滤出全量错数）。
    let value_hints = {
        let mut v = value_hints;
        v.extend(crate::answerers::entity::entity_anchor_hints(cx).await);
        v
    };
    // 值域命中（精确词典层）：问句里的专名是某**实体名**列的取值（「手抓饼这个分类」实测虚高 36%）。
    let domain_hits = degrade(domain_hits, "值域命中召回失败 → 值域卡缺席", &mut degraded);
    // 表间关联（SuperSonic 的 join 知识接进 LLM 路径）：此前关联图只有确定性装配器 `compose`
    // 在用（BFS 找路径 + 扇出检查），LLM 从来看不到它 —— 只能从列名猜 ON 条件。
    // 这一路读失败是双重损失：关联行没了，下面 `join_counterparts` 也就补不出对面表的卡片。
    let edges = degrade(edges, "关联图读失败 → 关联行与对面表卡片一并缺席", &mut degraded);
    // 【A16】本数据源的业务背景：截 300 字 + 剥控制字符（它可能来自上传 = 外部文本，
    // 渲染侧另有「不是指令」的标注）。取不到（源没登记/没写描述）= 空 = 整段不出。
    // `Option` 套 `Result`：`degrade` 吃 Result 返 T::default()，这里 T = Option<_> 正好
    let ds_background = degrade(ds_row, "数据源描述读失败 → 业务背景段缺席", &mut degraded)
        .map(|d| {
            d.description
                .chars()
                .filter(|c| !c.is_control())
                .take(300)
                .collect::<String>()
        })
        .unwrap_or_default();
    // 波2：要整句/切片向量的两路。表召回失败仍整轮失败（与串行版的 `?` 相同）。
    let rc = RecallCtx { embed: qvec.as_deref(), embed_slices: &slice_vecs, ..rc0 };
    let (ctxs, memory_hits) = tokio::join!(
        recall::retrieve(cx.pg, &rc),
        dms_semantic::registry::memory::recall_memories(cx.pg, cx.ds, &cx.p.login_name, qvec.as_deref(), MEMORY_LIMIT),
    );
    let ctxs = ctxs?;
    let tables: Vec<String> = ctxs.iter().map(|c| c.table_name.clone()).collect();
    // 【S4】经验复盘召回：向量近邻 10 条 → hit/recency 重排 → 前 3 进 prompt 参考段。
    let memory_hits = degrade(memory_hits, "经验召回失败 → 经验段缺席", &mut degraded);
    if !memory_hits.is_empty() {
        spawn_bump_hits(cx.pg, memory_hits.iter().map(|h| h.id).collect());
    }
    let memories: Vec<String> = memory_hits
        .iter()
        .map(|h| format!("[{}] {}", h.kind, h.content))
        .collect();
    // 波3：吃前两波结果的两路（【A19】术语定义递归 mapping 一层即止；教训吃召回到的表名）。
    // 教训召回失败仍整轮失败（与串行版的 `?` 相同）。
    // （seen/rc 先落成具名绑定：join! 宏展开里临时值活不到 await 结束）
    let seen_wave3: &[&[String]] = &[&metrics, &dims, &terms];
    let rc_pitfalls = RecallCtx { tables: &tables, limit: RECALL_LIMIT, ..rc };
    let (term_mapped, pitfalls) = tokio::join!(
        recall::recall_term_mapped(cx.pg, &rc, seen_wave3),
        recall::recall_pitfalls(cx.pg, &rc_pitfalls),
    );
    // 波3 判完再发波4：教训召回失败时，元素召回与对面表卡片查询不白跑一轮
    let pitfalls = pitfalls?;
    let term_mapped = degrade(term_mapped, "术语递归召回失败 → 术语映射卡缺席", &mut degraded);
    let terms: Vec<String> = terms.into_iter().chain(term_mapped).collect();
    // 波4：元素召回要吃**链好术语映射**的前三路（去重口径与串行版相同），
    // 与对面表卡片（吃关联图 + 召回表）互不依赖，一起发。
    let counterparts = join_counterparts(&edges, &tables);
    let seen_wave4: &[&[String]] = &[&metrics, &dims, &terms];
    let (elems, counter_rows) = tokio::join!(
        elements(cx.pg, &rc, seen_wave4),
        futures::future::join_all(counterparts.iter().map(|t| recall::schema_card(cx.pg, cx.ds, t))),
    );
    let joins = join_lines_for(cx.ds, &edges, &tables);
    // JOIN 对面表的 schema 卡：关联行给了「怎么连」，但对面表的字段一个都没给
    // （向量召回按单表打分，看不见「这张表得连另一张才有用」）。补在召回表之后 ——
    // **不插到前面**：召回顺序＝相关度顺序，这些是补充素材，不该抢相关度靠前的位置。
    let mut schema = schema_text(&ctxs);
    let (counter_cards, added) = collect_counter_cards(&counterparts, counter_rows);
    // 吼出来：这一刀只改 prompt，**装配器与门分布都看不见它** —— 没有日志就没法知道它
    // 到底有没有开火（本轮踩过一次：拿 `why-not-compose` 的门分布去验一个只影响 LLM 路的改动，
    // 数字当然一点没动）。`missing` 是「边指向它、但 meta.table_doc 里没有这张表」的声明缺口
    //（读失败被退回的也在这里面 —— 读失败本身在 `collect_counter_cards` 里已单独 warn）。
    if !counterparts.is_empty() {
        let missing: Vec<&String> =
            counterparts.iter().filter(|t| !added.contains(&t.as_str())).collect();
        tracing::info!(
            recalled = ?tables, added = ?added, missing = ?missing,
            "JOIN 对面表补卡片"
        );
    }
    // 逐张拼进 schema，不先拼出一份完整中间串再拷贝
    for c in &counter_cards {
        schema.push_str(c);
    }
    // 🔴 规则召回面 = 召回表 ∪ JOIN 对面表：`build_rules` 的 `recalled_tables` 只吃召回表时，
    // 「湖南省销售额」这类**值问题**的码列判据（RequireKnownValue/RequireCodeEq）造不出来
    // —— t_customer 只在对面表集合里，而 LLM 不受召回约束照样 JOIN 它（实测两次
    // `province LIKE '%湖南%'` 一路绿灯）。对面表是 join 图一跳可达，血量有界。
    // pitfalls 用召回原表（触发词的表归属是声明侧语义，不吃补卡的表）。
    let tables_for_rules: Vec<String> = {
        // `added` 来自 `join_counterparts`，已按**大小写不敏感**排除过召回表 —— 直接拼，不再判重
        let mut v = tables.clone();
        v.extend(added.iter().map(|t| (*t).to_string()));
        v
    };
    // 【个性化】用户习惯：**只在本轮问句自己没给时间/分组时**才带 —— 习惯不覆盖显式表达。
    // 判据用现成的两把尺：时间看 `nl::time` 认不认得出窗口，分组看问句有没有「按X」的说法。
    // 取不到/证据不足 = 空串 = 整段不出（`user_pref` 内部已经守住 MIN_SUPPORT）。
    let asked_time = dms_kernel::nl::time::time_predicate(cx.question).is_some();
    let asked_breakdown = cx.question.contains('按') || cx.question.contains("各");
    let user_habits = if asked_time && asked_breakdown {
        String::new()
    } else {
        let pref = dms_semantic::registry::user_pref::load(cx.pg, &cx.p.login_name).await;
        // 本轮已经给了的那一维不再提（提了就是噪声，还可能与用户这轮的说法打架）
        let pref = dms_semantic::registry::user_pref::UserPref {
            time_word: if asked_time { None } else { pref.time_word },
            breakdown_word: if asked_breakdown { None } else { pref.breakdown_word },
        };
        pref.prompt_section()
    };
    let pc = PromptCtx {
        metrics,
        dims,
        terms,
        // 规则时间解析（SuperSonic TimeRangeParser）：时间是 BI 最高频错误源，能规则算出的
        // 区间直接给 LLM，别让它自己拼日期函数（「近三个月」「第二季度」「6月」这类最易错）。
        // 指标声明「算到昨天」（`time_cap='yesterday'`）时右端压成 `< CURDATE()` ——
        // 卡片提示与规则窗并排时模型照抄规则窗（实测含今天虚 1.8%），所以改窗口本身。
        time_tpl: time_predicate(cx.question).map(|t| {
            // 有时间词才算 time_cap（问句无时间词时这一扫是纯浪费）
            if metric_hits.iter().any(|h| h.time_cap == "yesterday") {
                dms_kernel::nl::time::cap_at_yesterday(&t)
            } else {
                t
            }
        }),
        value_hints,
        domain_hits,
        elems,
        joins,
        schema,
        pitfalls,
        fewshot,
        memories,
        ds_background,
        user_habits,
        degraded,
    };
    let budget = BudgetReport {
        kept_recalled: ctxs.len(),
        kept_counters: !counter_cards.is_empty(),
    };
    // 【D7】本轮上下文落账：实际进 prompt 的卡 + 被裁项（脱敏：只结构/尺寸/表名），
    // 按 trace_id 进程内暂存，server `query_log::finish` 取走落 `meta.query_log.context_summary`。
    let summary = build_context_summary(&pc, &ctxs, &added, &counter_cards, &budget);
    let prompt_chars = summary.prompt_chars; // 摘要里已算过，日志复用（别再全量重算一遍）
    stash_context(&cx.trace_id, &summary);
    // 【A17 ②】口径二选一 chip：命中词与第一名等长的落选指标（问句没分清是哪个）。
    // 从 `metric_hits` 拿（卡片已经渲染完，hit_word 只在结构化形态上）。
    let alt_qs = recall::alt_questions(&metric_hits);
    tracing::info!(
        ms = t0.elapsed().as_millis(),
        tables = tables.len(),
        prompt_chars,
        "prompt 素材召回完成（分波并行）"
    );
    Ok((pc, tables_for_rules, alt_qs))
}

/// 命中行异步 hit_count+1（rerank 的「被印证次数」依据；失败只少点排序依据，不拖问答，
/// 但留一条 debug —— 本文件「每一路降级都要吼一声」的纪律，bump 失败也不例外）。
fn spawn_bump_hits(pg: &PgPool, ids: Vec<i64>) {
    let pg2 = pg.clone();
    tokio::spawn(async move {
        if let Err(e) = dms_semantic::registry::memory::bump_hits(&pg2, &ids).await {
            tracing::debug!(err = %e, "经验命中数 bump 失败（只少排序依据）");
        }
    });
}

/// 一路召回失败 → 降级成「这张卡缺席」，并**留痕两处**：日志一行 + `degraded` 一项。
///
/// `what` 同时是日志文案与降级项名（同源，改一处两处一起变）。此前这里是 12 份
/// `map_err(|e| tracing::warn!(...)).unwrap_or_default()` 手抄 —— 抄漏一处就是一路静默降级，
/// 而「召回为什么是空的」是本仓最高频的排查题。
fn degrade<T: Default, E: std::fmt::Display>(
    r: Result<T, E>,
    what: &'static str,
    degraded: &mut Vec<&'static str>,
) -> T {
    match r {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(err = %e, "{}", what);
            degraded.push(what);
            T::default()
        }
    }
}

/// 口径类降级：缺了这几张卡，模型就是在**没有口径定义**的情况下写 SQL。
/// 判据据此把 `trust` 降 review —— 其余几路（术语/关联图/经验…）缺席只是素材少，不改口径。
pub(crate) const CALIBER_CARDS: &[&str] = &["指标召回失败 → 指标卡缺席", "维度召回失败 → 维度卡缺席"];

/// 各段字节量合计（只用于审计实际送入模型的上下文体量，不参与裁剪）。
fn section_chars(pc: &PromptCtx) -> usize {
    let v = |xs: &[String]| xs.iter().map(|s| s.len()).sum::<usize>();
    v(&pc.metrics)
        + v(&pc.dims)
        + v(&pc.terms)
        + pc.time_tpl.as_deref().unwrap_or("").len()
        + v(&pc.value_hints)
        + v(&pc.domain_hits)
        + v(&pc.elems)
        + v(&pc.joins)
        + pc.schema.len()
        + v(&pc.pitfalls)
        + pc.fewshot.len()
        + pc.ds_background.len()
        + v(&pc.memories)
}

/// 预算护栏的执行回报：schema 段最终留了几张表、对面表卡片还在不在。
///（原先还有一个 `notes: Vec<TrimNote>` —— 恒 `vec![]` 的死件，2026-08-14 随
/// `ContextSummary.trimmed` 一起删掉。）
#[derive(Debug)]
struct BudgetReport {
    /// 最终 schema 段里召回表保留的张数（④ 没开火 = 全部）
    kept_recalled: usize,
    /// JOIN 对面表卡片是否还在 schema 段里（③ 没开火 = 在）
    kept_counters: bool,
}

/// 卡片头里的注册表名（`【指标·销售额】…` → `销售额`）：剥【】、剥四类前缀、剥版本后缀
/// `·vN`（**N 全为数字才剥** —— 注册名本身含「·v」的，如「新客·vip」，不许被截断）。
/// `card_name` 与 `prompt_card_has_name` 共用这一段头解析（曾经各写一遍）。
fn card_header_name(card: &str) -> Option<&str> {
    let rest = card.strip_prefix('【')?;
    let header = &rest[..rest.find('】')?];
    let header = ["指标·", "维度·", "术语·", "码值·"]
        .iter()
        .find_map(|prefix| header.strip_prefix(prefix))
        .unwrap_or(header);
    Some(match header.find("·v") {
        Some(i)
            if !header[i + "·v".len()..].is_empty()
                && header[i + "·v".len()..].chars().all(|c| c.is_ascii_digit()) =>
        {
            &header[..i]
        }
        _ => header,
    })
}

/// 卡片头里的注册表名（owning 版）；解析不出来（无 `【】` 头的卡种）→ None，只记尺寸。
fn card_name(card: &str) -> Option<String> {
    card_header_name(card).map(str::to_string)
}

/// JOIN 对面表卡片结果的分拣（`gather` 波4）：`Ok(Some)` 收卡并记表名；
/// `Ok(None)` 是「边指向它、但 meta.table_doc 里没有这张表」的声明缺口（调用方 `missing` 段统一记）；
/// `Err` 是 PG 读失败 —— 与声明缺口**不是一类**，单独 warn（本文件「降级都要吼一声」的同一纪律）。
fn collect_counter_cards<'a>(
    counterparts: &'a [String],
    rows: Vec<anyhow::Result<Option<String>>>,
) -> (Vec<String>, Vec<&'a str>) {
    let mut cards = Vec::new();
    let mut added = Vec::new();
    for (t, row) in counterparts.iter().zip(rows) {
        match row {
            Ok(Some(card)) => {
                cards.push(card);
                added.push(t.as_str());
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(err = %e, table = %t, "JOIN 对面表卡片读失败 → 该卡缺席"),
        }
    }
    (cards, added)
}

/// 【D7】本轮上下文摘要的组装（**纯函数**：素材全是 `gather` 的局部量，零 IO，好断言）。
///
/// 🔴 脱敏红线：`name` 只给「名字来自 meta 注册表」的卡种（指标/维度/术语/元素/schema 表名）。
/// 码值卡（`1→商品行` 这种映射）、值域卡（客户/分类**专名**）、few-shot（**别人的问句+SQL**）、
/// 经验正文 —— 里面有真实数据值与用户问句，只记 kind+chars。`meta.query_log` 只增不删，
/// 落进去的就是审计能看到的全部，宁少勿多（`context_summary_never_carries_data_values` 钉着）。
fn build_context_summary(
    pc: &PromptCtx,
    ctxs: &[TableCtx],
    counter_tables: &[&str],
    counter_cards: &[String],
    report: &BudgetReport,
) -> ContextSummary {
    let mut cards: Vec<ContextCard> = Vec::new();
    let mut named = |kind: &'static str, xs: &[String]| {
        cards.extend(xs.iter().map(|c| ContextCard { kind, name: card_name(c), chars: c.len() }));
    };
    named("metric", &pc.metrics);
    named("dim", &pc.dims);
    named("term", &pc.terms);
    named("elem", &pc.elems);
    if let Some(t) = &pc.time_tpl {
        cards.push(ContextCard { kind: "time", name: None, chars: t.len() });
    }
    // 含数据值或长正文的卡种：只记 kind+chars（见函数头红线）
    for (kind, xs) in [
        ("value_hint", &pc.value_hints),
        ("domain_hit", &pc.domain_hits),
        ("join", &pc.joins),
        ("pitfall", &pc.pitfalls),
        ("memory", &pc.memories),
    ] {
        cards.extend(xs.iter().map(|c| ContextCard { kind, name: None, chars: c.len() }));
    }
    let (kept_recalled, kept_counters) = (report.kept_recalled, report.kept_counters);
    // schema 段按**预算后的实际留存**记：召回表留几张是 report 说的，对面表卡片同理
    for c in ctxs.iter().take(kept_recalled) {
        cards.push(ContextCard { kind: "schema", name: Some(c.table_name.clone()), chars: c.schema_text.len() });
    }
    if kept_counters {
        for (t, card) in counter_tables.iter().zip(counter_cards) {
            cards.push(ContextCard { kind: "schema_counter", name: Some((*t).to_string()), chars: card.len() });
        }
    }
    if !pc.fewshot.is_empty() {
        cards.push(ContextCard { kind: "fewshot", name: None, chars: pc.fewshot.len() });
    }
    if !pc.ds_background.is_empty() {
        cards.push(ContextCard { kind: "ds_background", name: None, chars: pc.ds_background.len() });
    }
    ContextSummary {
        prompt_chars: section_chars(pc),
        cards,
        degraded: pc.degraded.clone(),
    }
}

/// 【D7】摘要暂存的写口（独立小函数：`gather` 体内「warn 条数 == unwrap_or_default 条数」
/// 的判据钉着，多一条 warn 字面量就会把它弄红）。
/// 按 `trace_id` **进程内**暂存（纯内存，无 SQL —— 本 crate 门禁不许 `sqlx::query`），
/// server `query_log::finish` 在同一 spawn 里取走落库，主链一个 `.await` 都不多。
/// 序列化只含字符串/数字，Err 分支纯防御：失败只少一条观测，绝不影响问答
/// （`query_log.rs` 纪律 1 的同一族）。
fn stash_context(trace_id: &str, cs: &ContextSummary) {
    match serde_json::to_string(cs) {
        Ok(json) => crate::ctx::stash_context_summary(trace_id, json),
        Err(e) => tracing::warn!(err = %e, "上下文摘要序列化失败 → 本轮不落 context_summary"),
    }
}

/// 回炉（`repair`）的材料：schema 段 + **全量**指标声明 + **按召回面过滤的**维度声明
/// （指标侧对照 SuperSonic `AllFieldMapper` / `MapModeEnum.ALL`；维度侧见下面【性能①】）。
///
/// 🔴 为什么回炉不能只给 schema（这里原来叫 `gather_schema`，只给 schema 段）：
/// 首轮失败最大的一档就是**口径卡没命中** —— `why-not-compose` 逐题诊断 38 题时
/// 「①指标不命中」9 题、「②装配器拒」9 题，而回炉是唯一一次补救机会。
/// 只喂 schema 等于让模型拿着上一轮同样缺的那张牌再猜一遍，那次机会就白烧了。
/// **指标段仍全量、不过 `match_word` 命中判据**，这正是要点：命中判据已经错过一次了。
/// 指标只有 18 行 / 2.8KB（`meta.metric` 全是手工声明，autodiscover 不写它），全量喂得起。
///
/// 【性能①】维度段为什么改成**按召回面过滤**（召回表 ∪ JOIN 对面表，与首轮
/// `tables_for_rules` 同一集合语义）：实测维度段 54 行 / 13 906 字符 / 19 956 字节，
/// 其中 **87% 是 autodiscover 灌的码→名 CASE**（`meta.dimension` 78 行里 68 行，
/// `company_code` 那条 1.08KB 的 CASE 被灌进 13 张表），回炉每轮为这段多付 2-5s。
/// 而来源表不在召回面里的声明救不了这次修复 —— 那些表的 schema 本来就不在材料里，
/// 分组表达式给了也落不了地。过滤纯函数是 `dims_for_repair`（单测钉着）。
///
/// ponytail: 若实测发现回炉质量被维度段稀释拖低，下一步是整段砍维度（删 `dim_lines_for`
/// 那行），**别去截断单张卡** —— 截半条 CASE 阶梯会让模型照抄一条语法不全的 CASE。
pub async fn gather_all_cards(cx: &AskCtx<'_>, embed: &EmbedClient) -> anyhow::Result<String> {
    // 【性能②】问句向量只算一次：schema 召回与经验召回共用（此前两处各发一次 embed HTTP，
    // 与首轮 `gather` 那次也是同一个问句 —— 跨调用那一层由 `EmbedClient` 的问句 memo 兜）。
    let qvec = embed.embed_query(cx.question).await.map(|v| to_pgvector(&v));
    // 【性能②】五读并行（原来 schema → 指标 → 维度串行，回炉每轮白付两段注册表往返；
    // 经验召回只吃上面已算好的 `qvec`，串行等在 join! 之后等于每轮回炉白付一次 PG 往返）。
    // 注册表读失败降级成「这一段缺席」而不是让整轮回炉失败（回炉本身就是补救路径），
    // 但**必须吼一声**：注册表读失败曾经是静默的，一趟评测才把它照出来（裁决 二·AE）。
    // schema 召回失败仍整轮失败（与串行版的 `?` 相同）。
    let (schema, metrics, dims, edges, mems) = tokio::join!(
        schema_section(cx, qvec.as_deref()),
        load_metrics(cx.pg, cx.ds),
        load_dimensions(cx.pg, cx.ds),
        load_join_edges(cx.pg, cx.ds),
        dms_semantic::registry::memory::recall_memories(cx.pg, cx.ds, &cx.p.login_name, qvec.as_deref(), MEMORY_LIMIT),
    );
    let (schema, recalled) = schema?;
    // 回炉路的降级**只留日志**：本函数不产 `PromptCtx`，收据挂在首轮那份上
    // （同一轮问答的 trust 已经由首轮的 `degraded` 决定了，这里再记一份是两份真相源）。
    let mut degraded: Vec<&'static str> = vec![];
    let metrics = degrade(metrics, "回炉全量指标读失败 → 指标段缺席", &mut degraded);
    let dims = degrade(dims, "回炉全量维度读失败 → 维度段缺席", &mut degraded);
    // 关联图在这里只为维度过滤的「JOIN 对面表」那半服务：读失败 → 过滤面只剩召回表
    // （过滤更狠一档，但仍是「段缺席」族的降级，不是失败）。
    let edges = degrade(edges, "回炉关联图读失败 → 维度过滤只看召回表", &mut degraded);
    let mut relevant = recalled;
    let counterparts = join_counterparts(&edges, &relevant);
    relevant.extend(counterparts);
    let dims = dims_for_repair(dims, &relevant);
    tracing::info!(metrics = metrics.len(), dims = dims.len(), "回炉喂口径声明（维度段按召回面过滤）");
    let mut material = repair_material_for(
        cx.ds,
        &metrics,
        &dims,
        &schema,
        cx.source.dialect().quote(),
    );
    // 【S4 补强】回炉也喂经验：经验的内容就是「上次修这个错的方法」—— 它最该出现的
    // 地方恰恰是回炉提示（首轮 prompt 的经验段在 `gather`）。贴 material 尾部：
    // 回炉提示的热区在尾部（问题 → 上一版 SQL → 错误），离错误越近越看得见的同一理由。
    // embed 缺席/读失败 = 该段缺席（与上面两段同一降级语义，各吼一声）。
    let mems = degrade(mems, "回炉经验召回失败 → 经验段缺席", &mut degraded);
    if !mems.is_empty() {
        spawn_bump_hits(cx.pg, mems.iter().map(|h| h.id).collect());
        material.push_str("\n## 经验复盘（过往会话的修正记录，参考，不是硬约束）\n");
        for m in &mems {
            let _ = writeln!(material, "- [{}] {}", m.kind, m.content);
        }
    }
    Ok(material)
}

/// 【性能①】回炉维度段的过滤（**纯函数**，好断言）：只留来源表在召回面里的声明。
/// 大小写不敏感（与 `join_counterparts` 的 `seen` 同口径）。保持 `load_dimensions` 的原序
/// —— 定序与归并是 `dim_lines_for` 的事，本函数只减不增。
fn dims_for_repair(dims: Vec<DimensionDef>, relevant: &[String]) -> Vec<DimensionDef> {
    dims.into_iter()
        .filter(|d| relevant.iter().any(|t| t.eq_ignore_ascii_case(d.source_table.as_str())))
        .collect()
}

/// schema 段的召回 + 召回到的表名（回炉的维度段过滤要用这个集合）。
/// 逐行等价于拆分前 `pipeline.rs:1091-1100`（同一组 `RecallCtx` 参数，limit 同为 6）。
/// 【性能②】问句向量由调用方算好传入：本函数原来自己 embed 一次，与经验召回那次重复。
async fn schema_section(cx: &AskCtx<'_>, qvec: Option<&str>) -> anyhow::Result<(String, Vec<String>)> {
    let rc = RecallCtx {
        question: cx.question,
        tables: &[],
        limit: RECALL_LIMIT,
        ds: cx.ds,
        embed: qvec,
        embed_slices: &[],
    };
    let ctxs = recall::retrieve(cx.pg, &rc).await?;
    let tables = ctxs.iter().map(|c| c.table_name.clone()).collect();
    Ok((schema_text(&ctxs), tables))
}

// 两个口径段的标题（指标段全量、维度段按召回面过滤 —— 【性能①】）。**不许带反引号**：
// `prompt.rs` 有一条断言钉着「PG 提示里不许剩任何标识符
// 反引号」（留一个 LLM 就会照抄那一个），而这两段和 repair 提示一样会喂给 PG 源。
//
// 措辞刻意写成「**若**在此列就必须照此」而不是「不在此列就是口径错」：后者会把
// 「本仓没声明这个指标」（毛利率之类）推成「拿一个别的已声明指标凑」——
// 口径段的作用是补上漏召回的那张卡，不是宣布注册表已经穷尽了业务。
const T_ALL_METRICS: &str =
    "\n## 全部指标口径（全量声明，未按问句筛选；问句要的指标若在此列，口径与来源表必须严格照此，不许自己选表或改算法）\n";
const T_ALL_DIMS: &str =
    "\n## 相关维度口径（按本轮召回的相关表筛选；问句要的维度若在此列，分组必须照抄这里的表达式，禁止自己臆造连接键）\n";

/// 回炉材料的拼装（**纯函数**，好断言）。段序：schema → 全量指标 → 过滤后的维度
/// （过滤在 `gather_all_cards` 的 `dims_for_repair`，本函数只渲染收到的声明）。
///
/// 🔴 为什么口径段在 schema **之后**（与首轮 `build_user_prompt` 正好相反）：
/// `prompts/repair.md` 的 `{schema}` 槽在「## 可用表结构」标题**之下**，把卡片塞到它前面会
/// 渲染出一个空的「可用表结构」标题 —— 空标题会让模型以为「这里本该有东西」而去编
/// （`prompt::section` 那条断言守的正是这件事）。而回炉提示的热区在**尾部**
/// （问题 → 上一版 SQL → 错误），卡片贴在 schema 之后反而离错误更近。
///
/// 空清单不出标题：注册表两条读都失败时，材料与改动前**逐字节相同**
/// （`prompt.rs::repair_prompt_is_byte_identical_to_pre_split` 仍然守着那个形态）。
#[cfg(test)]
pub(crate) fn repair_material(
    metrics: &[MetricDef], dims: &[DimensionDef], schema: &str, quote: &str,
) -> String {
    repair_material_for("", metrics, dims, schema, quote)
}

fn repair_material_for(
    ds: &str,
    metrics: &[MetricDef],
    dims: &[DimensionDef],
    schema: &str,
    quote: &str,
) -> String {
    let mut s = String::from(schema);
    push_cards(&mut s, T_ALL_METRICS, &metric_lines_for(ds, metrics, quote));
    let dl = dim_lines_for(ds, dims, quote);
    push_cards(&mut s, T_ALL_DIMS, &dl);
    s
}

/// 声明里的标识符引号按**本源方言**归一。
///
/// 为什么必须在这儿做：`meta.dimension` 里 78 条 active 有 **68 条**的 expr 带 MySQL 反引号
/// （autodiscover 按 MySQL 登记的码→名 CASE）。回炉材料是**逐字**塞进提示词的 `{schema}` 槽，
/// 而 `dialect_and_quote_come_from_the_source_not_a_default` 只判 `build_system_prompt` 的输出 ——
/// 那 ~33KB 材料一个字都没判。今天所有维度都是 `ds_id='dms'`（MySQL）所以没出事，
/// 那是**巧合不是判据**：接第一个 PG 源的那天，提示词里会同时出现「用双引号」的指令
/// 和 68 条反引号示例，而 LLM 照抄的是示例（本会话已实测过一次「留一个反引号 LLM 就照抄那一个」）。
///
/// 根子在登记侧（`register.rs` 该按源方言 quote），这里是渲染侧的兜底；两处都做才算完。
/// MySQL 源上 `quote == "`"` ⇒ 本函数是恒等变换，不改今天的任何字节。
fn requote<'a>(s: &'a str, quote: &str) -> std::borrow::Cow<'a, str> {
    // MySQL 恒等路径借用不分配（今天唯一路径，每条指标/维度行各省一次全量拷贝）。
    // ponytail：`replace` 是**无差别**替换 —— 字符串字面量里的反引号（如 `WHEN 'it`s' THEN'`）
    // 也会被换。今天的声明全是标识符引号用法（纪律：声明里的字面值不许含反引号）；
    // 接 PG 源前若有人往声明里写字面量反引号，这里得升级成引号感知替换。
    if quote == "`" { std::borrow::Cow::Borrowed(s) } else { std::borrow::Cow::Owned(s.replace('`', quote)) }
}

fn push_cards(out: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    out.push_str(title);
    for it in items {
        let _ = writeln!(out, "- {it}");
    }
}

/// 全量指标声明行（**纯函数**）。四段文案（口径过滤 / 时间列 / 去重键）刻意与
/// `recall::metric_card` 同形，但**不能复用它**：`metric_card` 吃的是 `MetricHit`
/// （带 `description`/`unit`），而全量装载器 `load_metrics` 返的 `MetricDef` 没有那两列；
/// 给 `MetricDef` 加字段会当场打断 `server/src/direct.rs` 的 4 处结构体字面量
/// —— 同一笔欠账 `semantic/src/registry/caliber.rs:30` 已经记过一次（`CaliberMetric` 也是
/// 为了这个才另起一个投影），两处都不是本任务的文件。
fn metric_lines_for(ds: &str, metrics: &[MetricDef], quote: &str) -> Vec<String> {
    metrics
        .iter()
        .map(|m| {
            let mut s = format!(
                "【指标·{}】= {}，来源表 {}",
                m.name,
                requote(&m.agg_expr, quote),
                dms_semantic::registry::warehouse_qualified_source(ds, &m.source_table)
            );
            if !m.scope_filter.is_empty() {
                s += &format!("；口径过滤：{}", m.scope_filter);
            }
            if !m.time_col.is_empty() {
                s += &format!("；时间过滤【必须】用 {} 列", m.time_col);
            }
            if !m.dedup_keys.is_empty() {
                s += &format!("；聚合前【必须】先按 ({}) DISTINCT 去重", m.dedup_keys);
            }
            s
        })
        .collect()
}

/// 全量维度声明行（**纯函数**）。两件事：
///
/// ① **同名同表达式的多张表并成一行**：autodiscover 把同一条码→名 CASE 灌进了很多表，
///    `company_code` 那条 1.08KB 的 CASE 实测出现在 13 张表上 —— 不并就是 14KB 纯重复进提示。
///    表达式不同的同名维度**各留一行**（那是两个不同口径，合并等于静默丢一个）。
/// ② **自己定序**（名字, 表达式, 表名）：`load_dimensions` 只 `ORDER BY name`，同名之间是 PG
///    物理行序，而种子每次启动都 UPDATE 一遍 → 同一个问句在不同部署上拿到不同的 prompt 字节。
///    `recall_dimensions` 那条注释记的是同一件事（它靠 `ORDER BY name, source_table` 解决）。
#[cfg(test)]
fn dim_lines(dims: &[DimensionDef], quote: &str) -> Vec<String> {
    dim_lines_for("", dims, quote)
}

fn dim_lines_for(ds: &str, dims: &[DimensionDef], quote: &str) -> Vec<String> {
    let mut sorted: Vec<&DimensionDef> = dims.iter().collect();
    sorted.sort_by(|a, b| {
        (&a.name, &a.expr, &a.source_table).cmp(&(&b.name, &b.expr, &b.source_table))
    });
    let mut rows: Vec<(&str, &str, Vec<&str>)> = vec![];
    for d in sorted {
        match rows.last_mut() {
            Some((n, e, tables)) if *n == d.name && *e == d.expr => {
                if !tables.iter().any(|table| table.eq_ignore_ascii_case(&d.source_table)) {
                    tables.push(&d.source_table);
                }
            }
            _ => rows.push((&d.name, &d.expr, vec![&d.source_table])),
        }
    }
    rows.iter()
        .map(|(n, e, tables)| {
            // 归一放在**合并之后**：合并是按原始 expr 逐字比的，先归一再合并会把
            // 「同名不同口径」的两条在 PG 源上误并成一条（那是静默丢一个口径）。
            let sources = tables
                .iter()
                .map(|table| dms_semantic::registry::warehouse_qualified_source(ds, table))
                .collect::<Vec<_>>()
                .join(" / ");
            format!("【维度·{n}】分组取值 {}，来源 {sources}", requote(e, quote))
        })
        .collect()
}

/// 卡片头解析的布尔版：头里的注册表名 == `name`（`·vN` 后缀已剥，见 `card_header_name`）。
/// 无 `【】` 头的卡种走 fallback：卡片以「`name` =」开头（`name` 为空串时恒真，先挡住）。
fn prompt_card_has_name(card: &str, name: &str) -> bool {
    if let Some(n) = card_header_name(card) {
        return n == name;
    }
    !name.is_empty()
        && card
            .strip_prefix(name)
            .is_some_and(|tail| tail.trim_start().starts_with('='))
}

/// 元素向量召回（移植 SuperSonic SchemaMapper）：substring 命中之外的语义双保险；按元素名去重。
/// `seen` 是前三路已命中的卡片（指标/维度/术语），名字出现在里面的元素不再重复摆一遍。
async fn elements(pg: &PgPool, rc: &RecallCtx<'_>, seen: &[&[String]]) -> Vec<String> {
    // 卡头解析在循环外做一次（O(cards)），候选元素直接查名字集；
    // 无【】头的卡（走 `prompt_card_has_name` 的 fallback 那类）留个零头清单逐对判。
    let mut seen_names = std::collections::HashSet::new();
    let mut fallback_cards = Vec::new();
    for card in seen.iter().flat_map(|group| group.iter()) {
        match card_header_name(card) {
            Some(n) => {
                seen_names.insert(n);
            }
            None => fallback_cards.push(card.as_str()),
        }
    }
    recall::recall_elements(pg, &RecallCtx { limit: 8, ..*rc })
        .await
        .into_iter()
        .filter(|(name, _)| {
            !seen_names.contains(name.as_str())
                && !fallback_cards.iter().any(|card| prompt_card_has_name(card, name))
        })
        .map(|(_, card)| card)
        .collect()
}

/// few-shot 语料的取法（trgm 相似 / 剔 disabled / ds 谓词）在 `registry::exemplar::fewshot`；
/// 这里只剩 prompt 侧的拼装。
async fn fewshot_block(pg: &PgPool, ds: &str, question: &str) -> String {
    fewshot_text(&exemplar::fewshot(pg, ds, question).await)
}

/// few-shot 段的渲染（**纯函数**）。空语料 → 空串（连标题都不出）。
fn fewshot_text(rows: &[(String, String)]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n## 相似问题的正确写法（参考口径）\n");
    for (q, sql) in rows {
        let _ = write!(s, "问：{q}\n```sql\n{sql}\n```\n");
    }
    s
}

/// schema 段：多张表的 `schema_text` 顺序拼接（召回顺序＝相关度顺序，不许重排）。
fn schema_text(ctxs: &[TableCtx]) -> String {
    ctxs.iter().map(|c| c.schema_text.as_str()).collect()
}

/// 表间关联段（**纯函数**）：`meta.join_edge` → 每条边一行权威关联键 + 扇出警告。
///
/// **只留至少一端被召回的边**（不是「两端都召回」）：值域命中卡常常要求 JOIN 一张**没被召回**的表
/// （「手抓饼这个分类」要 JOIN 分类表），那条边正是「怎么连过去」的答案，滤掉它就等于只说了要连、
/// 没说怎么连。边总量是个位数（今天 5 条），不会稀释 prompt。
///
/// 扇出警告按方向渲染：`card` 是 `lt→rt` 的基数。`1:N` 意味着 JOIN 右表后**左表的行会重复**，
/// 于是对左表列求和会按右表行数虚增 —— 那正是 `compose` 见到扇出边就拒绝装配的理由
/// （`direct.rs` 的「扇出边仅 COUNT(DISTINCT) 聚合可过」），LLM 此前对此毫不知情。
/// 被保留的边里**没被召回的那一端**（去重、保序）。
///
/// 🔴 为什么需要它：`join_lines` 只留「至少一端被召回」的边，于是 prompt 里会出现
/// 一行权威关联键 `t_a.x = t_b.y`，而 **t_b 的字段一个都没给** ——
/// 向量召回是按**单表**打分的，它天然看不见「这张表得跟另一张连起来才有用」。
/// LLM 于是只能猜 t_b 还有哪些列，或者干脆不 JOIN。
/// 这是 SQLBot「表关系补全」在本仓缺的那一半：关联行早就给了，**对面表的卡片没给**。
///
/// 纯函数，好断言。边总量是个位数（今天 5 条），补进来的表也是个位数，不会稀释 prompt。
fn join_counterparts(edges: &[JoinEdge], recalled: &[String]) -> Vec<String> {
    let seen = |t: &str| recalled.iter().any(|r| r.eq_ignore_ascii_case(t));
    let mut out: Vec<String> = vec![];
    for e in edges.iter().filter(|e| seen(&e.lt) || seen(&e.rt)) {
        for t in [&e.lt, &e.rt] {
            if !seen(t) && !out.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                out.push(t.clone());
            }
        }
    }
    out
}

#[cfg(test)]
fn join_lines(edges: &[JoinEdge], recalled: &[String]) -> Vec<String> {
    join_lines_for("", edges, recalled)
}

fn join_lines_for(ds: &str, edges: &[JoinEdge], recalled: &[String]) -> Vec<String> {
    let seen = |t: &str| recalled.iter().any(|r| r.eq_ignore_ascii_case(t));
    edges
        .iter()
        .filter(|e| seen(&e.lt) || seen(&e.rt))
        .map(|e| {
            let lt = dms_semantic::registry::warehouse_qualified_source(ds, &e.lt);
            let rt = dms_semantic::registry::warehouse_qualified_source(ds, &e.rt);
            let note = match e.card.as_str() {
                "1:N" => format!(
                    "一对多：JOIN {} 后 {} 的行会重复，对 {} 的列求和会按 {} 的行数虚增；\
                     须先按业务键去重，或改在 {} 这一级算",
                    rt, lt, lt, rt, rt
                ),
                "N:1" => format!("多对一：JOIN {rt} 不会让 {lt} 的行重复"),
                other => format!("基数 {other}"),
            };
            format!("{lt}.{} = {rt}.{}（{note}）", e.lc, e.rc)
        })
        .collect()
}

#[cfg(test)]
mod tests {

    /// 🔴 习惯**不许覆盖用户这一轮的显式表达**（那是猜，不是懂）。
    ///
    /// 判据打在接线上：`gather` 必须先判「本轮有没有给时间/分组」，给了就把对应那一维
    /// 从习惯里摘掉。少了这道判断，「上月销售额」会被上个月常问「本月」的用户的习惯带偏。
    #[test]
    fn user_habits_never_override_what_the_user_just_said() {
        let src = include_str!("gather.rs");
        let body = src.split("let user_habits =").nth(1).expect("习惯接线没了");
        let head = body.split("let pc = PromptCtx").next().unwrap();
        assert!(head.contains("if asked_time { None }"), "本轮给了时间还带习惯：{head}");
        assert!(head.contains("if asked_breakdown { None }"), "本轮给了分组还带习惯：{head}");
        // 两个都给了就整段不取（连那次 PG 往返都省掉）
        assert!(
            body.contains("if asked_time && asked_breakdown {"),
            "两维都给全时应整段跳过：{body}"
        );
    }
    use super::*;

    fn ctx(name: &str, text: &str) -> TableCtx {
        TableCtx {
            table_name: name.into(),
            schema_text: text.into(),
            score: 1.0,
            forced: false,
        }
    }

    /// 两段拼装的字节形态（它们直接进 `build_user_prompt` 的 golden）：
    /// 空语料**不出标题**、多表按召回顺序拼、每条语料一个 ```sql 围栏。
    #[test]
    fn fewshot_and_schema_text_shape() {
        assert_eq!(fewshot_text(&[]), "", "没有语料时连标题都不许出");
        let rows = vec![
            ("上月销售额".to_string(), "SELECT 1".to_string()),
            ("本周订单数".to_string(), "SELECT 2".to_string()),
        ];
        assert_eq!(
            fewshot_text(&rows),
            "\n## 相似问题的正确写法（参考口径）\n\
             问：上月销售额\n```sql\nSELECT 1\n```\n\
             问：本周订单数\n```sql\nSELECT 2\n```\n"
        );
        assert_eq!(schema_text(&[]), "");
        assert_eq!(
            schema_text(&[ctx("t_a", "表 t_a\n"), ctx("t_b", "表 t_b\n")]),
            "表 t_a\n表 t_b\n"
        );
    }

    fn dim(name: &str, table: &str, expr: &str) -> DimensionDef {
        DimensionDef {
            name: name.into(),
            aliases: vec![],
            source_table: table.into(),
            expr: expr.into(),
        }
    }

    /// 回炉材料不按人工预算裁掉维度声明：模型应看到本轮相关表的全部口径。
    #[test]
    fn repair_material_keeps_all_relevant_dims() {
        let m = || MetricDef {
            name: "销量".into(),
            aliases: vec![],
            source_table: "t_a".into(),
            agg_expr: "SUM(qty)".into(),
            scope_filter: String::new(),
            dedup_keys: String::new(),
            time_col: String::new(),
        };
        let dims: Vec<DimensionDef> =
            (0..30).map(|i| dim(&format!("维度{i:02}"), "t_a", &"长".repeat(3000))).collect();
        let full = repair_material(&[m()], &dims, &"S".repeat(50_000), "`");
        assert!(full.contains("SUM(qty)"), "指标段被动了");
        assert!(full.starts_with(&"S".repeat(100)), "schema 被截了");
        assert_eq!(full.matches("【维度·维度").count(), dims.len());
    }

    /// 🔴 全量维度段的两条性质：**同名同表达式跨表并成一行** + **输出定序**。
    ///
    /// 由来（实测）：`meta.dimension` 有 78 条 active，其中 68 条是 autodiscover 灌的码→名 CASE，
    /// `company_code` 那条 1.08KB 的 CASE 出现在 13 张表上 —— 归并后维度段 54 行 13 906 字符，
    /// 不归并 78 行 28 558 字符，**多出来的一半全是逐字重复**。
    /// 定序是因为 `load_dimensions` 只 `ORDER BY name`：同名之间是 PG 物理行序，
    /// 而种子每次启动都 UPDATE 一遍 → 同一个问句在不同部署上拿到不同的 prompt 字节。
    #[test]
    fn dim_lines_merge_same_declaration_across_tables_and_are_ordered() {
        let a = dim_lines(&[
            dim("所属公司", "t_b", "CASE company_code WHEN"),
            dim("品牌", "t_x", "g.brand_name"),
            dim("所属公司", "t_a", "CASE company_code WHEN"),
        ], "`");
        // 输入顺序颠倒得到同一份输出（这就是「不同部署同一份 prompt」那条性质）
        let b = dim_lines(&[
            dim("所属公司", "t_a", "CASE company_code WHEN"),
            dim("所属公司", "t_b", "CASE company_code WHEN"),
            dim("品牌", "t_x", "g.brand_name"),
        ], "`");
        assert_eq!(a, b);
        assert_eq!(
            a,
            vec![
                "【维度·品牌】分组取值 g.brand_name，来源 t_x",
                "【维度·所属公司】分组取值 CASE company_code WHEN，来源 t_a / t_b",
            ]
        );
        // 同名但**表达式不同** → 两行都留：那是两个不同口径，并掉等于静默丢一个
        // （`行类型` 在 t_sales_order_cart 与 t_sales_order_his_detai 上就是这种形态）
        let two = dim_lines(&[dim("行类型", "t_a", "CASE x"), dim("行类型", "t_b", "CASE y")], "`");
        assert_eq!(two.len(), 2, "{two:?}");
        assert!(dim_lines(&[], "`").is_empty());
    }

    /// 空清单**不出标题**：注册表两条读都失败时，回炉材料与「只给 schema」逐字节相同
    /// （空标题会让模型以为「这里本该有东西」而去编）。
    #[test]
    fn repair_material_without_declarations_is_just_the_schema() {
        assert_eq!(repair_material(&[], &[], "表 t（x）\n", "`"), "表 t（x）\n");
    }

    /// 🔴 **接线**判据：`repair_material` 的渲染有单测，但「`gather_all_cards` 到底有没有去读
    /// 注册表」一条判据都没有 —— 把那两行 `load_*` 换成 `Default::default()`，
    /// 渲染判据与 `prompt.rs` 的 golden 照旧全绿（交叉审实证）。无库单测覆盖不到这段 IO，
    /// 所以照本仓既有的 `run::correction_kinds_all_present` 形态用**源码**守。
    ///
    /// 判的是「函数体里同时有这两个装载调用」。它挡不住有人把调用挪到别处，
    /// 但挡得住最可能的那次退化：为了让某个测试变绿而把 `load_*` 换成默认值。
    #[test]
    fn gather_all_cards_actually_reads_the_registry() {
        let src = include_str!("gather.rs");
        let s = src
            .split("pub async fn gather_all_cards")
            .nth(1)
            .expect("函数改名了 —— 顺手把这条判据一起改");
        // 函数体到下一个顶层 `///` 文档注释为止（本文件每个顶层项前都有文档注释）
        let body = s.split("\n///").next().unwrap();
        for call in ["load_metrics(cx.pg, cx.ds)", "load_dimensions(cx.pg, cx.ds)"] {
            assert!(body.contains(call), "回炉不再读注册表了：缺 {call}");
        }
        // 【S4 补强】回炉必须喂经验（「上次修这个错的方法」最该在回炉提示里）
        assert!(
            body.contains(concat!("recall_", "memories(")),
            "回炉的经验段掉线了 —— 经验最该出现的地方就是回炉"
        );
        // 防恒真：切出来的必须真的是那个函数体而不是整份源码 —— 用**结构性判据**守
        // （不含下一个顶层 fn 的签名；早年用字节数上限，函数一变长就得手抬，太脆）
        assert!(!body.contains("fn dims_for_repair"), "切过头了，吃进了下一个函数");
        assert!(body.contains("repair_material"), "切段没切住：{body}");
        // 【性能②】问句只许 embed 一次（schema 召回与经验召回共用同一个 qvec），
        // 注册表两读 + 关联图必须与 schema 召回并行（原来串行，回炉每轮白付两段往返）
        assert_eq!(body.matches("embed_query").count(), 1, "回炉重复 embed 问句了：{body}");
        assert!(body.contains("tokio::join!"), "回炉的注册表两读不许退回串行：{body}");
        // 【性能①】维度段必须过召回面过滤（87% 是 autodiscover 的码→名 CASE，约 20KB）
        assert!(body.contains("dims_for_repair"), "维度段的召回面过滤掉了：{body}");
        // schema_section 不再自己 embed（qvec 由 gather_all_cards 传入），且必须交出召回表名
        let ss = src
            .split("async fn schema_section")
            .nth(1)
            .expect("schema_section 改名了 —— 顺手把这条判据一起改")
            .split("\n///")
            .next()
            .unwrap();
        assert!(!ss.contains("embed_query"), "schema_section 自己 embed 就是第二次：{ss}");
        assert!(ss.contains("table_name"), "召回表名没交出来 —— 维度过滤的集合从哪来：{ss}");
    }

    /// 【性能①】回炉维度段的过滤判据：只留来源表在召回面（召回表 ∪ JOIN 对面表）里的声明；
    /// 大小写不敏感；保持输入序（定序与归并是 `dim_lines_for` 的事，过滤只减不增）；
    /// 召回面为空 → 一行不留（那些表的 schema 不在材料里，分组表达式给了也落不了地）。
    #[test]
    fn repair_dims_are_filtered_to_the_recall_surface() {
        let dims = vec![
            dim("品牌", "t_x", "g.brand_name"),
            dim("所属公司", "t_a", "CASE company_code WHEN '1' THEN 'x' END"),
            dim("省份", "T_Customer", "c.province"),
        ];
        let relevant = vec!["t_a".to_string(), "t_customer".to_string()];
        let kept = dims_for_repair(dims, &relevant);
        let names: Vec<&str> = kept.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            ["所属公司", "省份"],
            "t_x 不在召回面必须滤掉；T_Customer 大小写不敏感命中；顺序保持输入序：{names:?}"
        );
        assert!(dims_for_repair(vec![dim("品牌", "t_x", "e")], &["t_a".to_string()]).is_empty());
        assert!(
            dims_for_repair(vec![dim("品牌", "t_x", "e")], &[]).is_empty(),
            "召回面为空 → 维度段整段缺席（一行不留）"
        );
    }

    /// 召回面 = 召回表 ∪ JOIN 对面表（与首轮 `tables_for_rules` 同一集合语义）：
    /// 对面表上的维度（「省份」在 t_customer 上）正是回炉要救的那类卡，不许被过滤误伤。
    #[test]
    fn repair_dim_surface_includes_join_counterparts() {
        let edges = vec![edge("t_ord", "cust", "t_cust", "cust", "N:1")];
        let mut relevant = vec!["t_ord".to_string()];
        let counterparts = join_counterparts(&edges, &relevant);
        relevant.extend(counterparts);
        assert!(relevant.iter().any(|t| t == "t_cust"), "对面表必须进召回面");
        let kept = dims_for_repair(vec![dim("省份", "t_cust", "c.province")], &relevant);
        assert_eq!(kept.len(), 1, "对面表上的维度必须留住");
    }

    /// 🔴 **降级必须留痕**：`gather` 里每一处 `unwrap_or_default()` 都得配一条 `tracing::warn!`。
    ///
    /// 由来：这几路召回原来是纯静默 `unwrap_or_default()`（裁决 二·AS5），而
    /// 「召回为什么是空的」是最高频的排查题 —— 静默降级把「读失败」和「本来没命中」
    /// 压成了同一种日志（都是没有日志）。同一族的坑在 `gather_all_cards`（二·AE）、
    /// `direct.rs` 的 `reg_load!`、`corrector.rs:478` 上各修过一次，所以这里用**条数相等**钉住：
    /// 新加一路静默召回 → 红；把某一路的 `map_err` 删掉 → 红。
    /// （挡不住有人把 warn 挪到别的函数里，挡得住「顺手再加一处静默降级」这个真实回归。）
    #[test]
    fn gather_warns_on_every_recall_degradation() {
        let src = include_str!("gather.rs");
        let s = src.split("pub async fn gather(").nth(1).expect("函数改名了 —— 顺手把这条判据一起改");
        let body = s.split("\n///").next().unwrap();
        // 防恒真①：切出来的必须真的**只是** `gather` 的函数体 —— 切歪成空串时下面 0 == 0 恒绿，
        // 切过头（吃进后面的函数）时条数又会莫名对上。所以两头都钉：有本函数的收尾语句、
        // 且没跑进下一个函数。**不用长度上限**：`body.len()` 是**字节**数而注释全是中文，
        // 第一版写 `< 4000` 当场就红（实测 3814 字符 / 远超 4000 字节）。
        assert!(body.contains("PromptCtx {") && body.contains("Ok((pc, tables_for_rules, alt_qs))"), "切段没切住：{body}");
        // 判的是**下一个函数的签名**而不是它的名字：本函数体的注释里就提到了 `gather_all_cards`
        // （第一版按名字判，当场红 —— 那次红得其所，说明这条判据真的在看内容）。
        assert!(!body.contains("pub async fn gather_all_cards"), "切过头了，吃进了下一个函数");
        // 2026-08-14：12 份手抄的 `map_err(warn).unwrap_or_default()` 收敛成一个 `degrade(...)`
        // —— 日志与 `PromptCtx::degraded` 从此同源（抄漏一处就是一路静默降级，而
        // 「召回为什么是空的」是本仓最高频的排查题）。判的从「两数相等」变成「一个都没漏」。
        let calls = body.matches("degrade(").count();
        // 防恒真②：这九路（指标/维度/术语/码值/值域/关联图/源背景/经验/术语递归）本来就在，数不到就是切歪了
        assert!(calls >= 9, "只数到 {calls} 处降级 —— 既有召回路哪去了？");
        // 禁的是**手抄形态**（`map_err(|e| tracing::warn!(...))`），不是 `unwrap_or_default()` 本身：
        // 后者在 `Option` 链上是正常写法（ds_background 那条），一刀切会把对的代码判红。
        assert!(
            !body.contains("map_err(|e| tracing::warn!"),
            "又出现手抄的降级：一律走 degrade(...)，否则 trust 收不到口径卡缺席"
        );
    }

    /// 【A8】切片向量必须真的进 `RecallCtx`：滑窗（kernel）→ 批量 embed → `embed_slices`。
    /// 缺任何一环，元素召回就退回「整句一条向量」，长问句稀释问题原样回来。
    #[test]
    fn gather_feeds_question_slices_to_recall() {
        let src = include_str!("gather.rs");
        let s = src.split("pub async fn gather(").nth(1).expect("函数改名了");
        let body = s.split("\n///").next().unwrap();
        assert!(body.contains("candidate_windows"), "滑窗没了（该用 kernel 那份）");
        assert!(body.contains("embed_batch"), "批量 embed 没了 —— 逐片单调是 N 倍往返");
        assert!(
            body.contains("EmbedMode::Query"),
            "切片必须用 query 向量：阈值是拿 query 标的，passage 会整体漂移"
        );
        assert!(body.contains("embed_slices"), "切片没进 RecallCtx —— 上面两步白做");
        assert!(body.contains("MAX_SLICES"), "片数上限没了（embed 服务单线程）");
    }

    /// 指标声明「算到昨天」时规则时间窗必须当场压右端（`time_cap` → `cap_at_yesterday`）。
    /// 卡片提示与规则窗并排时模型照抄规则窗（实测含今天虚 1.8%）—— 所以只能改窗口本身。
    /// 走 `MetricHit` 而不是卡片串：`time_cap` 只在结构化形态上。
    #[test]
    fn gather_caps_time_window_at_yesterday_for_declared_metrics() {
        let src = include_str!("gather.rs");
        let s = src.split("pub async fn gather(").nth(1).expect("函数改名了");
        let body = s.split("\n///").next().unwrap();
        assert!(body.contains("recall_metric_hits"), "time_cap 只在 MetricHit 上，卡片串拿不到");
        assert!(body.contains("time_cap"), "没读 time_cap —— 声明白加");
        assert!(body.contains("cap_at_yesterday"), "规则窗没压右端 —— 提示照旧被规则窗压过");
    }

    /// 🔴 声明里的标识符引号必须按**本源方言**归一，否则 PG 源的回炉提示里会同时出现
    /// 「用双引号」的指令和 68 条反引号示例 —— 而 LLM 照抄的是示例。
    /// 今天全部维度都是 MySQL 源，所以这条守的是**接第一个 PG 源那一天**，不是今天的字节。
    #[test]
    fn declarations_are_requoted_to_the_source_dialect() {
        let d = [dim("行类型", "t_a", "CASE `item_type` WHEN '1' THEN `商品行` END")];
        let m = [MetricDef {
            name: "销量".into(),
            aliases: vec![],
            source_table: "t_a".into(),
            agg_expr: "SUM(`box_quantity`)".into(),
            scope_filter: String::new(),
            time_col: String::new(),
            dedup_keys: String::new(),
        }];
        // MySQL：恒等，一个字节都不许动
        let my = repair_material(&m, &d, "", "`");
        assert!(my.contains("SUM(`box_quantity`)"), "{my}");
        assert!(my.contains("CASE `item_type`"), "{my}");
        // PG：反引号一个都不许剩，且真的换成了双引号（不是删掉）
        let pg = repair_material(&m, &d, "", "\"");
        assert!(!pg.contains('`'), "PG 材料里剩了反引号：{pg}");
        assert!(pg.contains("SUM(\"box_quantity\")"), "{pg}");
        assert!(pg.contains("CASE \"item_type\""), "{pg}");
        // 防恒真：两份输出必须真的不同（`requote` 写成恒等也会让上面三条 PG 断言里
        // 的前一条绿 —— 前提是输入里真有反引号，所以这条同时也在守输入）
        assert_ne!(my, pg);
        assert!(my.contains('`'), "输入里没有反引号 → 上面全部断言恒真");
    }

    fn edge(lt: &str, lc: &str, rt: &str, rc: &str, card: &str) -> JoinEdge {
        JoinEdge {
            lt: lt.into(),
            lc: lc.into(),
            rt: rt.into(),
            rc: rc.into(),
            card: card.into(),
        }
    }

    /// 🔴 表间关联段：**至少一端被召回**就留、扇出方向要出警告、一端都没召回才滤掉。
    ///
    /// 「两端都召回才留」是错的过滤：值域命中卡常要求 JOIN 一张**没被召回**的表
    /// （「手抓饼这个分类」要 JOIN 分类表），那条边正是「怎么连过去」的答案。
    #[test]
    fn join_lines_keep_edges_touching_recalled_tables() {
        let edges = vec![
            edge("t_ord", "code", "t_dtl", "code", "1:N"),
            edge("t_ord", "cust", "t_cust", "cust", "N:1"),
            edge("t_x", "a", "t_y", "b", "N:1"), // 两端都没召回 → 滤掉
        ];
        let recalled = vec!["t_ord".to_string()];
        let out = join_lines(&edges, &recalled);
        assert_eq!(out.len(), 2, "{out:?}");
        // 扇出边：必须点明「对左表列求和会虚增」——那是 compose 见到它就拒绝装配的理由
        assert!(out[0].starts_with("t_ord.code = t_dtl.code（一对多："), "{}", out[0]);
        assert!(out[0].contains("虚增"), "{}", out[0]);
        // 收敛边：明确说不会重复，免得 LLM 为它也去做无谓的去重
        assert!(out[1].contains("多对一") && !out[1].contains("虚增"), "{}", out[1]);
        // 只有右端被召回也要留（那正是「要连过去的那张表」的场景）
        assert_eq!(join_lines(&edges, &["t_dtl".to_string()]).len(), 1);
        // 一条都不沾 → 空（空清单在 `section` 里连标题都不出）
        assert!(join_lines(&edges, &["t_zzz".to_string()]).is_empty());
        assert!(join_lines(&[], &recalled).is_empty());
    }

    /// 🔴 **给了关联行就必须给对面表的字段**。
    ///
    /// 缺陷形态：prompt 里有一行权威关联键 `t_ord.cust = t_cust.cust`，而 `t_cust` 的字段
    /// 一个都没给 —— 向量召回按**单表**打分，天然看不见「这张表得跟另一张连起来才有用」。
    /// LLM 于是只能猜对面表还有哪些列，或者干脆不 JOIN。
    /// 判据钉在「哪些表要补卡片」这个纯函数上（真去取卡片要连库，那部分靠 kb/regression 兜）。
    #[test]
    fn join_counterparts_are_exactly_the_unrecalled_ends() {
        let edges = vec![
            edge("t_ord", "code", "t_dtl", "code", "1:N"),
            edge("t_ord", "cust", "t_cust", "cust", "N:1"),
            edge("t_x", "a", "t_y", "b", "N:1"), // 两端都没召回 → 这条边被滤，不许带出 t_x/t_y
        ];
        let recalled = vec!["t_ord".to_string()];
        assert_eq!(join_counterparts(&edges, &recalled), vec!["t_dtl", "t_cust"]);
        // 已召回的一端**不许**重复补（那会让同一张表的 schema 出现两遍）
        let both = vec!["t_ord".to_string(), "t_dtl".to_string()];
        assert_eq!(join_counterparts(&edges, &both), vec!["t_cust"]);
        // 两端都召回 → 无需补
        let all = vec!["t_ord".to_string(), "t_dtl".to_string(), "t_cust".to_string()];
        assert!(join_counterparts(&edges, &all).is_empty());
        // 被滤掉的边不许贡献对面表（否则整库的表都会被拖进 prompt）
        assert!(!join_counterparts(&edges, &recalled).iter().any(|t| t == "t_x" || t == "t_y"));
        // 大小写不敏感（与 join_lines 的 `seen` 同口径）
        assert!(join_counterparts(&edges, &["T_ORD".to_string(), "T_DTL".to_string()])
            .iter()
            .all(|t| t == "t_cust"));
        // 同一张表被两条边同时指向时只补一次
        let dup = vec![
            edge("t_ord", "a", "t_cust", "a", "N:1"),
            edge("t_dtl", "b", "t_cust", "b", "N:1"),
        ];
        assert_eq!(
            join_counterparts(&dup, &["t_ord".to_string(), "t_dtl".to_string()]),
            vec!["t_cust"]
        );
        assert!(join_counterparts(&[], &recalled).is_empty());
    }

    /// 【D7】卡清单 = **预算后实际进 prompt 的那些**：注册表口径名与表名带出、版本后缀剥掉、
    /// 含数据值的卡种只有 kind+chars、schema 按 report 的留存记。
    #[test]
    fn context_summary_lists_what_actually_entered_the_prompt() {
        let ctxs = vec![ctx("t_sales_order", "表 t_sales_order\n"), ctx("t_customer", "表 t_customer\n")];
        let counter_cards = vec!["表 t_goods（补卡）\n".to_string()];
        let pc = PromptCtx {
            metrics: vec!["【指标·销售额】= SUM(qty)，来源表 t_sales_order".into()],
            dims: vec!["【维度·所属公司】分组取值 g.company，来源 t_a".into()],
            terms: vec!["【术语·动销·v2】= 有销量的商品数".into()],
            time_tpl: Some("AND o.order_time >= '2026-08-01'".into()),
            value_hints: vec!["「商品行」→ item_type = 1".into()],
            domain_hits: vec!["「手抓饼」是 goods_type 的取值".into()],
            elems: vec!["【元素·客户数】按客户去重计数".into()],
            joins: vec!["t_sales_order.cust = t_customer.cust（多对一）".into()],
            schema: schema_text(&ctxs),
            pitfalls: vec!["教训：不许猜时间列".into()],
            fewshot: "问：上月销售额\n```sql\nSELECT 1\n```\n".into(),
            ds_background: "源背景".into(),
            user_habits: String::new(),
            memories: vec!["[fix] 上次把 order_time 写成了 create_time".into()],
            degraded: vec!["指标召回失败 → 指标卡缺席"],
        };
        let report = BudgetReport { kept_recalled: 2, kept_counters: true };
        let cs = build_context_summary(&pc, &ctxs, &["t_goods"], &counter_cards, &report);
        let j = serde_json::to_value(&cs).unwrap();
        // 合同形状（审计侧按这个解析）
        assert_eq!(j["prompt_chars"], section_chars(&pc));
        // 降级项照实带出（trust 与审计面板都读它）
        assert_eq!(j["degraded"][0], "指标召回失败 → 指标卡缺席");
        // 注册表口径名与表名带出（版本后缀 ·v2 必须剥掉）
        let names: Vec<&str> = j["cards"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"销售额"), "{names:?}");
        assert!(names.contains(&"所属公司"), "{names:?}");
        assert!(names.contains(&"动销"), "版本后缀 ·v2 必须剥掉：{names:?}");
        assert!(names.iter().any(|n| n.contains("客户数")), "{names:?}");
        assert!(names.contains(&"t_sales_order") && names.contains(&"t_goods"), "{names:?}");
        // 含数据值的卡种：卡在（kind+chars），name 键缺席
        let vh = j["cards"].as_array().unwrap().iter().find(|c| c["kind"] == "value_hint").unwrap();
        assert!(vh.get("name").is_none(), "{vh}");
        assert_eq!(vh["chars"], pc.value_hints[0].len());
        // 卡种全覆盖：每一类素材都要在清单里
        for k in [
            "metric", "dim", "term", "time", "value_hint", "domain_hit", "elem", "join",
            "pitfall", "memory", "schema", "schema_counter", "fewshot", "ds_background",
        ] {
            assert!(j["cards"].as_array().unwrap().iter().any(|c| c["kind"] == k), "缺卡种 {k}");
        }
        // 预算砍掉的表不进清单：report 说只留 1 张时 t_customer 不许出现
        let trimmed_report = BudgetReport { kept_recalled: 1, kept_counters: false };
        let j2 = serde_json::to_value(&build_context_summary(&pc, &ctxs, &["t_goods"], &counter_cards, &trimmed_report)).unwrap();
        let names2: Vec<&str> = j2["cards"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(!names2.contains(&"t_customer") && !names2.contains(&"t_goods"), "{names2:?}");
    }

    /// 🔴 脱敏红线：落账 JSON 里绝不许出现**用户数据值** —— 码值映射、值域专名、
    /// few-shot 的问句、教训/经验正文都只在卡里记尺寸；表名与注册表口径名**必须**在
    /// （那是审计要的结构，不是数据值）。
    #[test]
    fn context_summary_never_carries_data_values() {
        let ctxs = vec![ctx("t_sales_order", "表 t_sales_order（order_time, qty）\n")];
        let pc = PromptCtx {
            metrics: vec!["【指标·销售额】= SUM(qty)".into()],
            dims: vec![],
            terms: vec![],
            time_tpl: None,
            value_hints: vec!["「南京苏宇食品有限公司」→ customer_code = C0093".into()],
            domain_hits: vec!["「手抓饼」是 goods_type 的取值".into()],
            elems: vec![],
            joins: vec![],
            schema: schema_text(&ctxs),
            pitfalls: vec!["教训：客户 南京苏宇食品有限公司 的码容易写错".into()],
            fewshot: "问：南京苏宇食品有限公司上月销量\n```sql\nSELECT ... /* hunter2 */\n```\n".into(),
            ds_background: String::new(),
            user_habits: String::new(),
            memories: vec!["[fix] C0093 这个码上次写错了".into()],
            degraded: vec![],
        };
        let report = BudgetReport { kept_recalled: 1, kept_counters: false };
        let json = serde_json::to_string(&build_context_summary(&pc, &ctxs, &[], &[], &report)).unwrap();
        for v in ["南京苏宇食品有限公司", "手抓饼", "hunter2", "C0093"] {
            assert!(!json.contains(v), "数据值 {v} 落账了：{json}");
        }
        assert!(!json.contains("上月销量"), "few-shot 的问句落账了：{json}");
        // 结构信息必须在（审计要的就是这些）
        assert!(json.contains("t_sales_order"), "表名必须落：{json}");
        assert!(json.contains("销售额"), "注册表口径名必须落：{json}");
        assert!(json.contains("\"prompt_chars\""), "{json}");
        // 防恒真：输入里确实有这些值（输入造错了上面全恒绿）
        assert!(pc.value_hints[0].contains("南京苏宇食品有限公司") && pc.fewshot.contains("hunter2"));
    }

    /// 卡头解析：`·vN` 版本后缀只在 N 全为数字时剥（注册名本身含「·v」的 —— 如「新客·vip」——
    /// 不许被静默截断）；四类前缀剥掉；无【】头的卡 → None。
    #[test]
    fn card_header_name_strips_only_numeric_version_suffixes() {
        assert_eq!(card_header_name("【指标·销售额】= SUM(qty)"), Some("销售额"));
        assert_eq!(card_header_name("【术语·动销·v2】= 有销量的商品数"), Some("动销"), "数字版本后缀要剥");
        assert_eq!(card_header_name("【术语·新客·vip】= …"), Some("新客·vip"), "非数字后缀不许剥");
        assert_eq!(card_header_name("【术语·新客·v】= …"), Some("新客·v"), "空版本号不许剥");
        assert_eq!(card_header_name("【码值·商品行】1→x"), Some("商品行"));
        assert_eq!(card_header_name("plain card"), None);
        // 布尔版共用同一段解析；fallback（无【】头、形如「名 =」）空名不许恒真
        assert!(prompt_card_has_name("【指标·销售额·v12】…", "销售额"));
        assert!(!prompt_card_has_name("【指标·销售额·v12】…", "销售额·v12"));
        assert!(prompt_card_has_name("销售额 = SUM(qty)", "销售额"));
        assert!(!prompt_card_has_name("销售额 = SUM(qty)", ""));
    }

    /// 🔴 接线判据：`gather` 必须把预算回报组装成摘要并暂存 —— 删掉那两行，
    /// `build_context_summary` 的单测照样全绿（纯函数成了孤儿），而 query_log 的
    /// context_summary 列会永远空着。无库单测覆盖不到 `gather` 的 IO，照本仓既有形态用源码守。
    #[test]
    fn gather_stashes_context_summary_for_query_log() {
        let src = include_str!("gather.rs");
        let body = src
            .split("pub async fn gather(")
            .nth(1)
            .expect("gather 没了")
            .split("\n///")
            .next()
            .unwrap();
        assert!(!body.contains("enforce_prompt_budget"), "不应再按人工预算丢弃 prompt 卡片");
        assert!(body.contains("build_context_summary"), "摘要组装没了 —— context_summary 列会永远空着");
        assert!(body.contains("stash_context("), "暂存没了 —— 摘要组了也到不了 query_log");
    }
}
