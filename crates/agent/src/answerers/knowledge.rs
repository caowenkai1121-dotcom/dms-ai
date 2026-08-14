//! 【K5】知识库适配器：`Principal` → `Viewer` → `dms_knowledge` 的检索与作答。
//! 变更原因＝「问数身份怎么变成看文档的身份」。
//!
//! 搬运源 `server/src/main.rs:569-576`（`kb_answer`，行号为搬运时点），逐行搬运（含那段「roles 必须用解出来的
//! role_code」的依据）。
//!
//! ## 为什么它是本目录里唯一不实现 `Answerer` 的成员
//! ① **不进 Router**（ARCHITECTURE §4.6 + `answerers/mod.rs` 纪律第三条）：进链会让
//!    文档问句在没命中时回落到 SQL 生成，破不变量 I5。分派由 triage 直接做。
//! ② 它的产物是 `dms_kernel::Answer`（`kind:"text"` + `citations`），而 `Answerer::answer` 交的是
//!    `AskResult` —— 后者是取数结果集的形状（`sql`/`columns`/`rows`/`view`），**没有 `kind` 字段**，
//!    文本回答塞不进去。两个协议的统一是 T9 之后的活（`server/src/triage.rs` 文件头也这么记的）。
//!
//! ## I5 的编译期保证
//! 本文件不出现 SQL 相关的任何类型（下面有一条 `include_str!` 的漂移断言把这条钉成会红的测试）：
//! 知识库路径**结构上产不出 SQL**。文档正文的 `<untrusted_document>` 包裹与转义在
//! `dms_knowledge::answer`（那里三条纪律集中在一个文件，不许在这里复述第二份）。
//!
//! ## rerank（B5）为什么不在本层接线
//! 精排由 `retrieve::search_report` 按 `DMS_RERANK_*` 环境变量内部插入 —— `/api/ask`、
//! `/api/mcp`、kb 调试入口共用那一条检索函数，配置因此只有一份，本层签名一个参数都不加。

use dms_connector::embed::EmbedClient;
use dms_connector::owned::OwnedStore;
use dms_kernel::{Answer, ChatModel};
use dms_knowledge::{KbError, Viewer};
use dms_policy::Principal;

/// 知识库回答。`space = None` 表示**不限空间**（全部可见文档），不是个人空间：
/// 被授权看别人空间的人也得能检索到，ACL 由 `retrieve` 在 SQL 内把关，这里不拼第二份。
///
/// 形参是**透传**（与 `dms_knowledge::answer::answer` 一一对应）：为它造一个只有一个调用点的
/// 上下文结构，只会多一层要跟着漂的类型。
///
/// `weights` 由调用方给 settings 快照（`st.cfg().kb_rrf_weights`）——主链与
/// kb_api / kb_eval / mcp 四条链至此全部吃页面可配的生效值。
pub async fn answer(
    store: &OwnedStore,
    embed: &EmbedClient,
    llm: &dyn ChatModel,
    p: &Principal,
    space: Option<&str>,
    question: &str,
    weights: &dms_knowledge::retrieve::RrfWeights,
) -> Result<Answer, KbError> {
    let v = viewer(p);
    // 🔴 **要文件的问句不生成回答**（2026-08-14 架构级体检 R1）。
    //
    // 业主实测「下载 押金转货款申请书」返回 38 行账余充值明细 —— 合同没有「动作」维，
    // 「下载」无处安放，被塞进 `data` 后由 `kw_force` 的「押金 → 账余表」种子钉成数据卡。
    // 判据在 `dms_kernel::nl::doc`（确定性、零 IO），分流放在**这一个函数**里而不是各入口：
    // 五套入口都经过这里，改一处五处同时生效。
    if dms_kernel::nl::doc::is_document_request(question) {
        if let Some(list) = documents(store, embed, &v, space, question, weights).await? {
            return Ok(list);
        }
        // 一份都没检索到就回落 —— 由 `dms_knowledge::answer` 去说「知识库里没有相关内容」，
        // 空清单卡比一句人话更难堪。
    }
    dms_knowledge::answer::answer(store, embed, llm, &v, space, question, weights).await
}

/// 文件清单卡：检索一次，按文件去重，直接把**可下载的文件**列给用户。
///
/// **零 LLM 调用** —— 用户要的是文件本身，不是一段关于文件的话。前端
/// `KbAnswer.vue` 已为每条 `citation` 渲染「下载原件」按钮（走
/// `/api/kb/doc/{doc_id}/download`），所以这里一个字节的前端改动都不需要。
///
/// 返回 `None` = 一条都没检索到，交调用方回落到常规问答。
pub async fn documents(
    store: &OwnedStore,
    embed: &EmbedClient,
    v: &Viewer,
    space: Option<&str>,
    question: &str,
    weights: &dms_knowledge::retrieve::RrfWeights,
) -> Result<Option<Answer>, KbError> {
    let t0 = std::time::Instant::now();
    let hits = dms_knowledge::retrieve::search(store, embed, v, space, question, weights).await?;
    // 检索返回的是**片段**，用户要的是**文件**：一份文件只留命中最好的那一块（`search`
    // 已按相关度排序，`insert` 首次为真即最好那条）。8 条封顶 —— 再多就不是「给你文件」
    // 而是「你自己找」了。
    let mut seen = std::collections::HashSet::new();
    let picked: Vec<&dms_knowledge::retrieve::Hit> =
        hits.iter().filter(|h| seen.insert(h.doc_id.clone())).take(8).collect();
    let picked = rank_by_name(question, picked);
    if picked.is_empty() {
        return Ok(None);
    }
    let mut md = format!("为你找到 {} 份相关文件，点条目下方的「下载原件」即可取用：\n\n", picked.len());
    for (i, h) in picked.iter().enumerate() {
        // 角标 = citations 下标 + 1（`AnswerBody::Text` 的契约），前端靠它把条目连到下载按钮
        md.push_str(&format!("{}. **{}**[^{}]\n", i + 1, h.doc_name, i + 1));
        let mut meta = Vec::new();
        if !h.folder_path.is_empty() {
            meta.push(format!("目录：{}", h.folder_path));
        }
        if let Some(from) = &h.effective_from {
            meta.push(format!("生效：{from}"));
        }
        if !h.doc_updated_at.is_empty() {
            meta.push(format!("更新于 {}", h.doc_updated_at));
        }
        if !meta.is_empty() {
            md.push_str(&format!("   {}\n", meta.join(" · ")));
        }
    }
    let cites = dms_knowledge::answer::citations(picked.into_iter());
    Ok(Some(Answer::text(md, cites, t0.elapsed().as_millis())))
}

/// `Principal` → `Viewer`（knowledge 刻意不依赖 policy，两者唯一交集就是这两个字符串）。
///
/// 🔴 `roles` 用 **principal 解出来的** `role_code`，不是请求里带的那个 `Option<String>`：
/// 单角色账号不传 role_code 时后者是 `None` → `roles` 为空 → 授权给「角色」的知识库文档
/// 在 `/api/ask` 检索不到，而同一个人走 `/api/mcp` 却能检索到（那条用的是解出来的角色）。
/// 两处口径必须一致，且应向**解出来的**那侧统一：它就是该账号真实的激活角色，从不超过其真实权限。
pub fn viewer(p: &Principal) -> Viewer {
    Viewer::new(&p.login_name, vec![p.role_code.clone()])
}

/// 按**文件名与问句的最长公共子串**重排并剪枝。
///
/// 🔴 业主 2026-08-14 实测：「下载 押金转货款申请书」返回 5 份文件，只有第 1 份对得上，
/// 其余是《线下设备物资处置申请单流程指引》《客户退出申请流程填写详细指引》——
/// 它们靠正文里的「申请」两个字挤进了向量召回。**要文件的人是在念文件名**，
/// 名字对不上就不是他要的那份。
///
/// 剪枝只在「真有人对上了」时发生（最佳 ≥3 字），且保留最佳的一半以上；
/// 一个字都对不上就**全留**，交给检索原序 —— 宁可多给，不可给空。
/// `sort_by_key` 是稳定排序，同分者维持检索相关度顺序。
fn rank_by_name<'a>(
    question: &str,
    mut hits: Vec<&'a dms_knowledge::retrieve::Hit>,
) -> Vec<&'a dms_knowledge::retrieve::Hit> {
    let score = |h: &dms_knowledge::retrieve::Hit| {
        dms_kernel::nl::text::longest_common_run(question, &h.doc_name)
    };
    let best = hits.iter().map(|h| score(h)).max().unwrap_or(0);
    if best >= 3 {
        hits.retain(|h| score(h) * 2 >= best);
    }
    hits.sort_by_key(|h| std::cmp::Reverse(score(h)));
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 身份映射：login 与**解出来的**角色码各一条。角色丢了不会报错，
    /// 只是「授权给角色的文档」在这条链路上永远检索不到（而在 `/api/mcp` 上又能）。
    #[test]
    fn viewer_carries_login_and_resolved_role() {
        let v = viewer(&crate::gate::anyone());
        assert_eq!(v.login, "t10gate");
        assert_eq!(v.roles, vec!["city_manager".to_string()]);
    }

    /// 🔴 不变量 I5 的结构性保证，变成一条会红的断言：知识库路径不许出现 SQL 类型。
    /// 判据与 `scripts/check-arch.ps1` 同款——**注释行不参与匹配**（规则本身要写在文件头，
    /// 否则这句话会把自己判红）。只滤 `//` 行注释：块注释不参与豁免（本文件没有；有了会误判红）。
    #[test]
    fn no_sql_types_in_the_knowledge_path() {
        let src = include_str!("knowledge.rs");
        let body = src.split("#[cfg(test)]").next().unwrap_or(src); // split 首段恒存在
        let code: Vec<&str> =
            body.lines().filter(|l| !l.trim_start().starts_with("//")).collect();
        // 三个 SQL 类型名。「Scoped」是裸子串 —— 误判面：任何含它的合法标识符/字符串都会撞红
        // （今天的命中面就是 ScopedSql，先记在这里）。**不查 sqlx 的查询宏**：`scripts/check-arch.ps1`
        // 已对整个 `crates/agent/src` 守着它，在这里复述一遍反而让本文件自己撞上那条 grep（实测撞过）。
        for needle in ["sqlparser", "RawSql", "Scoped"] {
            assert!(
                !code.iter().any(|l| l.contains(needle)),
                "知识库路径出现了 {needle}：I5 的编译期保证被打开了"
            );
        }
    }
}
