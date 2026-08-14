//! 【A9】向量自愈调度：启动即跑一轮 + 之后按 `INTERVAL` 周期把 `embedding IS NULL` 的行补回。
//!
//! 写入点原来只有离线脚本（`tools/embed_service.py build/revec`）：服务侧只有体检
//! （health 的 `vector_ready`）没有修复，于是行被置 NULL 之后没人补，向量路静默哑掉
//! （选源/元素召回/语义缓存三处各自降级成「结果为空」，与「本来没命中」无法区分）。
//! SQLBot 的 `SingleWorkerGuard` 对应物 = PG advisory lock（多实例部署也只跑一个）。
//!
//! 文本配方在 semantic（`registry::embed_fill`，与离线 build 逐字一致，有判据钉着）；
//! 本文件只有「什么时候跑、一次跑多少、失败怎么办」三件事。

use std::sync::Arc;

use anyhow::Context as _;

use dms_connector::embed::{to_pgvector, EmbedMode};
use dms_semantic::registry::datasource::list_datasources;
use dms_semantic::registry::embed_fill::{null_vec_rows, write_vec, MetaVecTarget, FILL_BATCH};

use crate::AppState;

/// advisory lock 键（任意常数，本服务内唯一用途即可）
const LOCK_KEY: i64 = 7_720_031;
/// 两轮间隔（首轮启动即跑：重启就把攒下的 NULL 补掉）
const INTERVAL: std::time::Duration = std::time::Duration::from_secs(600);
/// meta 侧文本截断上限（字符）：与离线 `tools/embed_service.py` `_revec` 的截断口径
///（`(r[1] or '')[:1000]`）逐字对账，改一边必须同步另一边。
const TEXT_CHAR_CAP: usize = 1000;

/// 挂后台。失败只 warn：向量是**增强**不是主链路，它哑了问答照样出数（只是少几路召回）。
pub fn spawn(st: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            match run_round(&st).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("向量自愈：本轮补回 {n} 行"),
                Err(e) => tracing::warn!("向量自愈本轮失败（下轮重试）: {e:#}"),
            }
            tokio::time::sleep(INTERVAL).await;
        }
    });
}

async fn run_round(st: &AppState) -> anyhow::Result<u64> {
    // 多实例只跑一个。锁必须握在**同一条连接**上：advisory lock 是会话级的，
    // 从池里抓另一条连接 unlock 解的是空气（锁还会跟着这条连接被还回池而泄漏）。
    let mut conn = st.owned.pool().acquire().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(LOCK_KEY)
        .fetch_one(&mut *conn)
        .await?;
    if !locked {
        // 与「本轮无待补行」（fill_all 末尾的 debug）区分：这是锁被别的实例持有。
        tracing::debug!("向量自愈：advisory 锁由其他实例持有，本轮跳过");
        return Ok(0);
    }
    let r = fill_all(st).await;
    if let Err(e) = sqlx::query("SELECT pg_advisory_unlock($1)").bind(LOCK_KEY).execute(&mut *conn).await {
        // 解锁失败 = 这条连接带着会话级锁还回池、终身占锁（见上方注释）—— 至少留 warn。
        tracing::warn!("向量自愈：advisory 解锁失败，该连接将带锁还回池: {e:#}");
    }
    r
}

async fn fill_all(st: &AppState) -> anyhow::Result<u64> {
    let pg = st.owned.pool();
    let mut filled = 0u64;
    // ds 限定的三类按**每个已登记源**各补一轮（上传源 `up_*` 的元素/表卡行也带自己的 ds_id）；
    // `Datasource` 是注册表本身，无 ds 谓词只跑一趟。
    // 只补 active 源：disabled 源每轮白跑空查询没意义，重新启用后下轮自然补上。
    let ds_ids: Vec<String> = list_datasources(pg)
        .await?
        .into_iter()
        .filter(|d| d.status == "active")
        .map(|d| d.ds_id)
        .collect();
    for t in MetaVecTarget::ALL {
        // 按 (target, ds) 粒度容错：单点失败只留 warn 并继续，不中断整轮
        //（否则排在前面的源失败会把其余源和 fill_kb 一起跳过）。
        let scopes: Vec<&str> =
            if t.ds_scoped() { ds_ids.iter().map(String::as_str).collect() } else { vec![""] };
        for ds in scopes {
            match fill_target(st, t, ds).await {
                Ok(n) => filled += n,
                Err(e) => tracing::warn!("向量自愈：{t:?} ds={ds} 本轮失败，继续其余目标: {e:#}"),
            }
        }
    }
    // 知识库块：ingest 在向量服务不可用时把文档停在 chunked，这里补块 + 推状态
    filled += fill_kb(st).await?;
    if filled == 0 {
        // 与「锁被别的实例持有」（run_round 的 debug）区分：这是真的没活干。
        tracing::debug!("向量自愈：本轮无待补行");
    }
    Ok(filled)
}

async fn fill_target(st: &AppState, t: MetaVecTarget, ds: &str) -> anyhow::Result<u64> {
    let pg = st.owned.pool();
    let rows = null_vec_rows(pg, ds, t, FILL_BATCH).await?;
    if rows.is_empty() {
        return Ok(0);
    }
    tracing::debug!("向量自愈：{t:?} ds={ds} 待补 {} 行", rows.len());
    let texts: Vec<String> =
        rows.iter().map(|(_, x)| x.chars().take(TEXT_CHAR_CAP).collect()).collect();
    // 语料问句是**问句侧**向量，其余是文档侧 —— 与离线 `_revec(.., is_query)` 逐条一致，
    // 混一种模式 = 同一列两套不可比向量（语义缓存按问句近邻召回，对模式最敏感）。
    let vecs = match t.is_query_side() {
        true => st.embed.embed_batch(&texts, EmbedMode::Query).await,
        false => st.embed.embed_batch(&texts, EmbedMode::Passage).await,
    }
    // 缺席即报错（warn 留痕）；kb 侧同款缺席是静默跳过（见 fill_kb）—— 两处不对称是刻意的。
    .with_context(|| format!("{t:?} embed 服务缺席（ds={ds}），本轮跳过"))?;
    anyhow::ensure!(
        vecs.len() == rows.len(),
        "{t:?} embed 返回条数不符（{} ≠ {}）",
        vecs.len(),
        rows.len()
    );
    let mut n = 0u64;
    for ((id, _), v) in rows.iter().zip(&vecs) {
        write_vec(pg, ds, t, id, &to_pgvector(v)).await?;
        n += 1;
    }
    Ok(n)
}

async fn fill_kb(st: &AppState) -> anyhow::Result<u64> {
    let rows = dms_knowledge::store::null_vec_chunks(&st.owned, FILL_BATCH).await?;
    let mut n = 0u64;
    if !rows.is_empty() {
        // 块长已由 ingest 定界，不再截断；meta 侧文本无界才截 TEXT_CHAR_CAP（见 fill_target）。
        let texts: Vec<String> = rows.iter().map(|row| row.text.clone()).collect();
        if let Some(vecs) = st.embed.embed_batch(&texts, EmbedMode::Passage).await {
            anyhow::ensure!(
                vecs.len() == rows.len(),
                "kb embed 返回条数不符（{} ≠ {}）",
                vecs.len(),
                rows.len()
            );
            for (row, v) in rows.iter().zip(&vecs) {
                if dms_knowledge::store::set_chunk_embedding(
                    &st.owned,
                    row.chunk_id,
                    &row.text,
                    row.recipe,
                    &to_pgvector(v),
                )
                .await?
                {
                    n += 1;
                }
            }
        } else {
            // embed 缺席：块保持 NULL 下轮再试（不是错误 —— 文本检索仍可用）。
            // 与 meta 侧（fill_target 报错留痕）不对称是刻意的：kb 有文本检索兜底。
            tracing::debug!("向量自愈：kb embed 服务缺席，{} 块保持 NULL 下轮再试", rows.len());
        }
    }
    // 刻意每轮对账（含 embed 缺席、本轮无新块时）：把块已补完的 chunked 文档推到 embedded
    //（ingest 正常路径的同款状态迁移），历史遗留只靠这里兜底。
    dms_knowledge::store::flip_embedded_docs(&st.owned).await?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    /// 🔴 advisory lock 的三个坑全在源码层，无库单测碰不到：
    /// ① 必须 `try`（阻塞锁会让第二个实例的调度线程睡死在锁上）；
    /// ② 锁与解锁必须在**同一条连接**上（会话级锁，换连接 unlock 是解空气）；
    /// ③ 失败路径也要解锁（否则这个实例终身占锁，别的实例永远替补不上）。
    #[test]
    fn advisory_lock_is_try_same_conn_and_always_unlocked() {
        let src = include_str!("embed_fill.rs");
        let body = src
            .split("async fn run_round(")
            .nth(1)
            .expect("run_round 没了")
            .split("\nasync fn ")
            .next()
            .unwrap();
        assert!(body.contains("pg_try_advisory_lock"), "阻塞锁会把替补实例睡死：{body}");
        assert!(body.contains("pg_advisory_unlock"), "没有解锁：{body}");
        // 同一条 conn：`.acquire()` 一次，之后没有第二次
        assert_eq!(body.matches(".acquire()").count(), 1, "锁与解锁不在同一条连接上：{body}");
        // 解锁在 fill_all 之后、返回之前（`let r = fill_all(…); … unlock; r`）
        let fill = body.find("fill_all(st).await").expect("fill_all 没了");
        let unlock = body.find("pg_advisory_unlock").unwrap();
        assert!(fill < unlock, "失败路径不解锁会终身占锁：{body}");
    }

    /// embed 缺席不许让本轮报错之外还丢状态：kb 块保持 NULL 下轮再试（与 ingest 的降级同款）。
    #[test]
    fn kb_embed_absence_is_not_an_error() {
        let src = include_str!("embed_fill.rs");
        let body = src
            .split("async fn fill_kb(")
            .nth(1)
            .expect("fill_kb 没了")
            .split("\n    //")
            .next()
            .unwrap();
        assert!(body.contains("if let Some(vecs)"), "embed 缺席不该 `?` 掉本轮：{body}");
    }

    /// 单点失败不中断整轮：fill_target 的错误按 (target, ds) 粒度 catch 后继续；
    /// disabled 源不进补向量清单（每轮白跑空查询没意义）。
    #[test]
    fn one_target_failure_does_not_abort_the_round() {
        let src = include_str!("embed_fill.rs");
        let body = src
            .split("async fn fill_all(")
            .nth(1)
            .expect("fill_all 没了")
            .split("\nasync fn ")
            .next()
            .unwrap();
        assert!(!body.contains("fill_target(st, t, ds).await?"), "单目标失败不该 `?` 掉整轮：{body}");
        assert!(body.contains("继续其余目标"), "失败分支应留 warn 并明示继续：{body}");
        assert!(body.contains("d.status == \"active\""), "disabled 源每轮白跑空查询：{body}");
    }
}
