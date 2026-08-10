//! 【K3】向量选源：这一问该查哪个数据源。变更原因＝选源判据。
//!
//! 逐行搬 `server/src/pipeline.rs:427-528`（`DS_GAP` / `pick_by_gap` / `select_source` /
//! `nearest_visible` / `pick_by_llm`）。**判据顺序不许动**：
//! 显式选源 → `visible_datasources` 候选 → 距离差 > 0.08 直接用 → 否则 fast LLM 选一次。
//!
//! 可见性一律经 `registry::datasource::visible_datasources`（ACL 在 SQL 内 JOIN，
//! **不许查完再过滤**，不变量 I4）。本文件不做第二份 ACL 判据。

use sqlx::PgPool;

use dms_connector::embed::{to_pgvector, EmbedClient};
use dms_kernel::{ChatModel, ChatRequest, ModelTier};
use dms_policy::Principal;
use dms_semantic::registry::datasource as ds_reg;

/// 两个候选的余弦距离差小于这个值 = 不确定，交 fast LLM 二选一（差得多就别浪费一次调用）
const DS_GAP: f64 = 0.08;

/// 最近的源也**没有近到这个程度** ⇒ 问句与所有源都不相似 ⇒ 回主源，不选。
///
/// 🔴 由来（2026-07-31 实测抓到的回归）：灌上 `meta.datasource.embedding` 之后自动选源
/// 第一次真的开始工作，而**没有这条兜底时它会给不相似的问句强行挑一个最近的源**。
/// 回归当场红：`C01-单号直查`「帮我查下 HJXH-DXO2026072300384」被选到
/// `upload_…（员工台账）`（距离 0.5982），而 dms 是 0.6899 —— 距离差 0.0917 > `DS_GAP`
/// 于是连 LLM 二选一都不走，直接用错的源；那个源上没有订单表 ⇒ 答不出 ⇒ 回落 ⇒ 被意图反问拦下。
///
/// 阈值按实测定，不是猜的（同日，本地 bge-small-zh 512 维，`<=>` 余弦距离）：
/// | 问句 | 最近源 | 距离 |
/// |---|---|---|
/// | 差旅补贴标准是多少 | 上传源《差旅补贴标准》 | **0.247** |
/// | 通讯补贴按岗位怎么分级 | 上传源《通讯补贴标准》 | **0.2424** |
/// | 员工台账里有多少人 | 上传源《员工台账》 | **0.4259** |
/// | 本月销售额是多少 | dms | 0.5625 |
/// | 帮我查下 HJXH-DXO… | 上传源（**错**） | 0.5982 |
/// | 买过烤肠的客户 | dms | 0.7103 |
/// | 今天天气怎么样 | 上传源（无关） | 0.6801 |
/// 真匹配全 ≤0.43、错匹配与无关全 ≥0.56 ⇒ 取 0.5，两侧各留 0.07/0.06 的缓冲，不是踩线。
///
/// 顺带省掉三次注定白花的 LLM 二选一：上表里 dms 的三条距离差都 <`DS_GAP`，
/// 原来每句都要问一次 fast LLM 才能确认「就用主源」。
///
/// 失败方向是安全的：判紧了 ⇒ 回主源 ⇒ 主源答不出就反问；判松了才会**答出别的源的数**。
const DS_MAX_DIST: f64 = 0.5;

/// 距离裁决（**纯函数**，无库无网可单测）。入参已按距离升序（`nearest_datasources` 的 ORDER BY）。
///
/// 三态，顺序即行为：
/// - `TooFar` = 最近的也不够近（`> DS_MAX_DIST`）⇒ 调用方回主源，**连 LLM 都不问**
/// - `Pick` = 最近的明显更近 ⇒ 直接用
/// - `Ambiguous` = 两个咬得太紧 ⇒ 交 fast LLM 二选一
///
/// `TooFar` 必须判在最前面：先判距离差就会让「与所有源都不相似」的问句
/// 因为恰好差得够远而被直接采用 —— 那正是 C01 单号直查踩的坑（见 `DS_MAX_DIST`）。
#[derive(Debug, PartialEq)]
enum DsPick<'a> {
    TooFar,
    Pick(&'a str),
    Ambiguous,
}

fn pick_by_gap(cands: &[(String, f64)]) -> DsPick<'_> {
    let Some((first, d0)) = cands.first().map(|(s, d)| (s.as_str(), *d)) else {
        return DsPick::TooFar;
    };
    if d0 > DS_MAX_DIST {
        return DsPick::TooFar;
    }
    match cands.get(1) {
        Some((_, d1)) if d1 - d0 <= DS_GAP => DsPick::Ambiguous,
        _ => DsPick::Pick(first),
    }
}

/// 选源：显式 > 向量最近邻 > 单源直通。返回 ds_id。
///
/// ① 调用方显式指定（前端选了源）→ 必须在 `visible_datasources` 里，否则拒（越权面）
/// ② 可见源 ≤1 → 用它。**存量场景的零行为变化就靠这一条短路**：只有 'dms' 时既不 embed
///    也不问 LLM，一次多余的 IO 都没有
/// ③ 多个可见源 → embed 问句 → `nearest_datasources` → 距离差 > 0.08 直接用最近的；
///    否则 fast LLM 二选一（给 name + description），失败取最近的
pub async fn select_source(
    llm: &dyn ChatModel,
    pg: &PgPool,
    embed: &EmbedClient,
    p: &Principal,
    question: &str,
    explicit: Option<&str>,
) -> anyhow::Result<String> {
    // 可见性判据整块在 SQL 里（`registry::datasource::visible_datasources_sql`），这里不做第二份 ACL
    let visible = match ds_reg::visible_datasources(pg, &p.login_name, &[p.role_code.clone()]).await
    {
        Ok(v) => v,
        // 算不出可见集合 → 走主源。这**不是**放宽权限：DMS 主源按 policy_kind='dms_datascope'
        // 本就对所有认证用户可见，行级权限由 `inject` 兜。它维持的是 K3-B 之前的行为
        // （那时根本没有这一步）—— `ask` 子命令的引导不建 `kb.acl`，propagate 会打死判官链路。
        // 显式选源**不吃**这个降级：那会把「无权访问」静默变成「换个源给你查」。
        Err(e) if explicit.is_none() => {
            tracing::warn!("可见数据源查询失败（{e}）→ 本轮走主源 dms");
            return Ok(ds_reg::DMS_DS_ID.to_string());
        }
        Err(e) => return Err(e),
    };
    if let Some(want) = explicit {
        if !visible.iter().any(|d| d == want) {
            anyhow::bail!("无权访问数据源 {want}");
        }
        return Ok(want.to_string());
    }
    if visible.len() <= 1 {
        return Ok(visible.into_iter().next().unwrap_or_else(|| ds_reg::DMS_DS_ID.to_string()));
    }
    let cands = nearest_visible(pg, embed, question, &visible).await;
    if cands.is_empty() {
        // 选不出就走主源，绝不乱猜。
        //
        // ⚠️ 现网这一支是**恒真**的：`meta.datasource.embedding` 一行都没写过
        //（写入点已经有了——`tools/embed_service.py build` 的 datasource 分支——但从未跑），
        // 于是 `nearest_datasources` 恒空、下面两支（距离差 / LLM 二选一）走不到。
        // 恒真这件事本身没变，变的只是**代价**：`nearest_visible` 现在先用一句 EXISTS 体检，
        // 确认库里真有带向量的源才去 embed（原来是先花一次注定白费的 HTTP 才发现返空）。
        // 也就是说**自动选源今天是空转的**：用户上传表格后不显式选源，问句永远由主源回答。
        // 那一天要开，得先把测试遗留的上传源清干净：每个 `active` 上传源都会去竞争
        // 所有问句的路由（`tools/up_probe.py` 因此默认自清理）。开之前必须重跑
        // 回归 + 评测——它改的是每一句问话的选源行为，不是一个新功能开关。
        return Ok(ds_reg::DMS_DS_ID.to_string());
    }
    match pick_by_gap(&cands) {
        DsPick::Pick(ds) => Ok(ds.to_string()),
        // 与所有源都不相似 → 主源。日志留一条：这一支静默的话，「为什么没走我上传的表」查不出来
        DsPick::TooFar => {
            tracing::info!(
                nearest = %cands[0].0, dist = cands[0].1, max = DS_MAX_DIST,
                "问句与所有数据源都不相似 → 回主源（未问 LLM）"
            );
            Ok(ds_reg::DMS_DS_ID.to_string())
        }
        DsPick::Ambiguous => {
            Ok(pick_by_llm(llm, pg, question, &cands).await.unwrap_or_else(|| cands[0].0.clone()))
        }
    }
}

/// 向量最近邻，**再按可见集合过滤**：向量召回不许绕过 ds 级 ACL。
async fn nearest_visible(
    pg: &PgPool,
    embed: &EmbedClient,
    question: &str,
    visible: &[String],
) -> Vec<(String, f64)> {
    // 🔴 **先问库有没有候选，再花那次 embed HTTP** —— 这个顺序就是修复本身。
    // 原来 embed 在最前面，而 `nearest_datasources` 的谓词是 `embedding IS NOT NULL`，
    // 2026-07-28 查库：`meta.datasource` 4 行 active、**0 行有向量** ⇒ 那条 SQL 恒返空
    // ⇒ 恒回主源 ⇒ 那次 HTTP 今天 100% 是白花的，而它站在问答链的**最前面**（用户看到
    // 第一个字之前）。省多少按实测说话，2026-07-30 打 :8077 单条 query：冷启 311 ms、
    // 热身后 14~19 ms；换来的体检是 1.2 ms 规划 + 2.1 ms 执行（`ddl::vector_ready`，
    // 三张表全 NULL 的最坏情况）。也就是 ~7 倍（热）/ ~100 倍（冷），
    // **不是**三个数量级。3s 只是客户端超时**上限**（单线程 :8077 被占时的长尾），不是日常值。
    //
    // ⚠️ 这条路径今天只有 `admin` 会走到：查库 4 个 active 源里 3 个是 upload/global，
    // 而 `kb.acl(scope='ds')` 那 3 行全授给 login=admin ⇒ 只有 admin 的 `visible.len() > 1`。
    // 别的 login 在上面 `visible.len() <= 1` 就短路了，压根到不了这里。
    if !ds_vector_ready(pg).await {
        return vec![];
    }
    let Some(v) = embed.embed_query(question).await else {
        return vec![];
    };
    let lit = to_pgvector(&v);
    ds_reg::nearest_datasources(pg, &lit, 4)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|(ds, _)| visible.iter().any(|x| x == ds))
        .collect()
}

/// `meta.datasource` 里有没有一行算出了 embedding（向量选源的**候选存在性**）。
///
/// 读失败 → 当作没有：与下面 `nearest_datasources` 读失败时的 `unwrap_or_default()`
/// **同一个降级方向**（回主源）。这一句体检不许把选源判宽或判窄，它只决定「值不值得花那次
/// embed HTTP」。就绪位的三个谓词与各自生产 SQL 的一致性由
/// `dms_semantic::ddl::tests::vector_ready_matches_the_production_predicate` 钉着。
async fn ds_vector_ready(pg: &PgPool) -> bool {
    dms_semantic::ddl::vector_ready(pg)
        .await
        .map_err(|e| tracing::warn!(err = %e, "向量就绪位查询失败 → 本轮不走向量选源"))
        .is_ok_and(|r| r.datasource)
}

/// fast LLM 二选一（给 name + description）。答不上/挂了 → None，调用方取最近的。
/// 🔴 `description` 可能来自上传（K4 的表格源），是**外部文本**：剥换行 + 截 200 字，
/// 且回答只用于在候选集合里**查表**（`find`）——模型说什么都出不了这个集合。
async fn pick_by_llm(
    llm: &dyn ChatModel,
    pg: &PgPool,
    question: &str,
    cands: &[(String, f64)],
) -> Option<String> {
    let rows = ds_reg::list_datasources(pg).await.ok()?;
    let menu: String = cands
        .iter()
        .filter_map(|(ds, _)| rows.iter().find(|r| &r.ds_id == ds))
        .map(|r| {
            let desc: String = r.description.replace(['\n', '\r'], " ").chars().take(200).collect();
            format!("- {}：{}｜{}\n", r.ds_id, r.name, desc)
        })
        .collect();
    let system = "你按问题从候选数据源里选一个最合适的。只输出 ds_id 本身，不要解释。";
    let user = format!("候选数据源：\n{menu}\n问题：{question}\nds_id=");
    // 温度 0.1 = 搬运前 `LlmClient::chat` 写死的那个值（`server/src/llm.rs:53`）
    let resp = llm.chat(ChatRequest::text(ModelTier::Fast, system, &user, Some(0.1))).await.ok()?;
    let resp = resp.content?;
    cands.iter().map(|(d, _)| d).find(|d| resp.contains(d.as_str())).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 【K3-B ③】选源的距离裁决（纯函数）。钉住四件：太远回主源、单候选直取、
    /// 咬得紧交 LLM、差得开直接用最近的。两个阈值都是契约（ARCHITECTURE §4.6）。
    #[test]
    fn source_pick_by_gap() {
        let c = |v: &[(&str, f64)]| -> Vec<(String, f64)> {
            v.iter().map(|(s, d)| (s.to_string(), *d)).collect()
        };
        assert_eq!(pick_by_gap(&[]), DsPick::TooFar); // 没候选 → 调用方降级主源
        assert_eq!(pick_by_gap(&c(&[("dms", 0.31)])), DsPick::Pick("dms"));
        // 差 0.2 > 0.08：最近的明显更近，不花 LLM
        assert_eq!(pick_by_gap(&c(&[("dms", 0.20), ("crm", 0.40)])), DsPick::Pick("dms"));
        // 差 0.02 ≤ 0.08：咬得太紧，交 LLM 二选一
        assert_eq!(pick_by_gap(&c(&[("dms", 0.20), ("crm", 0.22)])), DsPick::Ambiguous);
        // 边界：差正好等于阈值也算不确定（宁可多问一次，也别选错源答错数）
        assert_eq!(pick_by_gap(&c(&[("dms", 0.0), ("crm", DS_GAP)])), DsPick::Ambiguous);
    }

    /// 🔴 绝对距离兜底：**用 2026-07-31 实测到的真实距离**，不是编的数。
    /// 这条判据的价值全在「顺序」上 —— `TooFar` 必须判在距离差之前，
    /// 否则单号直查那一例（错源 0.5982、dms 0.6899、差 0.0917 > `DS_GAP`）
    /// 会因为「差得够远」被直接采用，而那正是它当初红掉的原因。
    #[test]
    fn too_far_beats_the_gap_rule() {
        let c = |v: &[(&str, f64)]| -> Vec<(String, f64)> {
            v.iter().map(|(s, d)| (s.to_string(), *d)).collect()
        };
        // ① 实测原案：单号直查。错源更近、差还够大 —— 没有 TooFar 就会选错源
        let doc_code = c(&[("upload_3ee5efc0", 0.5982), ("dms", 0.6899), ("upload_655e", 0.6912)]);
        assert_eq!(pick_by_gap(&doc_code), DsPick::TooFar, "单号直查会被选到上传源");
        // ② 实测原案：三条真匹配都必须照旧被选中（阈值判紧了就是「永不选源」）
        for (ds, d) in [("upload_655e", 0.247), ("upload_cb45", 0.2424), ("upload_3ee5", 0.4259)] {
            assert_eq!(
                pick_by_gap(&c(&[(ds, d), ("dms", d + 0.2)])),
                DsPick::Pick(ds),
                "真匹配 {ds}({d}) 被误拦 —— 阈值 {DS_MAX_DIST} 判得太紧"
            );
        }
        // ③ 实测原案：dms 自己就是最近的但也超过阈值 → 照样回主源（结果相同，但省一次 LLM）
        assert_eq!(pick_by_gap(&c(&[("dms", 0.5625), ("upload_655e", 0.6256)])), DsPick::TooFar);
        assert_eq!(pick_by_gap(&c(&[("dms", 0.7103), ("upload_cb45", 0.7767)])), DsPick::TooFar);
        // ④ 实测原案：完全无关的问句（最近 0.6801、差 0.0096）
        assert_eq!(pick_by_gap(&c(&[("upload_cb45", 0.6801), ("upload_655e", 0.6897)])), DsPick::TooFar);
        // ⑤ 边界：正好等于阈值算**近**（`>` 不是 `>=`）—— 缓冲带在 0.43↔0.5625，边界怎么算都安全，
        //    但得钉住，否则改成 `>=` 时 0.5 那一档静默换边
        assert_eq!(pick_by_gap(&c(&[("dms", DS_MAX_DIST)])), DsPick::Pick("dms"));
        assert_eq!(pick_by_gap(&c(&[("dms", DS_MAX_DIST + 0.0001)])), DsPick::TooFar);
        // ⑥ 两个阈值的关系必须成立：真匹配的上界 0.4259 < DS_MAX_DIST < 最近错匹配 0.5625
        assert!(0.4259 < DS_MAX_DIST && DS_MAX_DIST < 0.5625, "阈值滑出实测缓冲带了");
    }

    /// 🔴 **embed 必须在候选存在性检查之后**。判的是**位置**，不是「那个函数存在」：
    /// 把 `ds_vector_ready` 那句删掉、或挪到 `embed_query` 后面，这条就红。
    ///
    /// 由来：`meta.datasource` 实测 0 行有 embedding ⇒ `nearest_datasources` 恒返空
    /// ⇒ 先发的那次 embed HTTP（单线程 :8077、3s 超时上限、站在问答链最前面）恒是白花的。
    /// 这段是 IO，无库无网的单测覆盖不到（`EmbedClient` 是具体类型，没有可替身的 trait），
    /// 故按本仓既有形态用源码守（同 `gather::gather_all_cards_actually_reads_the_registry`）。
    #[test]
    fn embed_happens_only_after_the_candidate_check() {
        let src = include_str!("source.rs");
        let body = src
            .split("async fn nearest_visible(")
            .nth(1)
            .expect("函数改名了 —— 顺手把这条判据一起改")
            .split("\n///")
            .next()
            .unwrap();
        // 防恒真，两头都钉：切出来的必须真是这个函数体，且没跑进下一个函数。
        // **不拿 `body.len()` 当上限**：那是字节数而注释全是中文，写数字必假红。
        assert!(body.contains("nearest_datasources"), "切段没切住：{body}");
        assert!(!body.contains("async fn "), "切过头了，吃进了下一个函数：{body}");
        let check = body.find("ds_vector_ready(pg)").expect("候选存在性检查没了：embed 又白花了");
        let http = body.find("embed_query(").expect("切段没切住：本函数里应该有 embed_query");
        assert!(check < http, "embed 又跑到候选存在性检查前面了：\n{body}");
    }
}
