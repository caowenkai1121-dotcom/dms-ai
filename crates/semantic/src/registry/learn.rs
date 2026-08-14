//! 学习事件账本：**每一次「学下来」都留前值与批次号，一条 SQL 能撤回**。
//!
//! 为什么单独一个文件而不是塞进 `exemplar.rs`：那个文件非测试段已逼近 D2 的 450 行，
//! 且本模块的变更原因是「学习行为怎么被审计与撤销」，与「语料怎么召回」是两件事（D3）。
//!
//! 形态借 prime-agent 的 refinement（`packages/coding-agent/src/core/refinement`）：
//! 逐条记 `before`/`after`，回滚是**纯机械的倒序重放**，不再调模型。本仓更简单 ——
//! 写口本身就是确定性的，缺的只是账本。
//!
//! 🔴 落账失败**不许拖垮学习本身**：账本是观测面，学习是主路。写不进账本只 warn，
//! 与 `qa_log`/`correction_log` 同一条纪律。反过来也成立：没有账本的写入等于不可回滚，
//! 所以四个写口都要接，接漏了由 `learn_writes_are_all_ledgered` 钉板抓。

use sqlx::PgPool;

/// 可回滚的学习目标表白名单。**回滚 SQL 的表名只能从这里取**（`&'static str`），
/// 满足 `drift.rs` 的 `sql_interpolation_is_allowlisted`：拼进 SQL 的永远是编译期常量，
/// 不是外部输入。加表必须同步加回滚分支，否则 `rollback_batch` 会静默跳过它。
pub const LEDGERED_TABLES: &[&str] = &["meta.sql_exemplar", "meta.memory", "meta.pitfall"];

/// 记一条学习事件。`before = None` 表示新增（回滚即删除）。
///
/// 失败只 warn 不传播：账本挂了不能让学习链路整体失败（见文件头纪律）。
pub async fn log_event(
    pg: &PgPool,
    batch_id: &str,
    actor: &str,
    target_table: &str,
    target_id: &str,
    action: &str,
    before: Option<serde_json::Value>,
    after: Option<serde_json::Value>,
) {
    if batch_id.trim().is_empty() {
        // 不拒绝写入（学习本身不许被账本拖垮），但必须说清后果
        tracing::warn!(target_table, "学习事件没有批次号 —— 它将无法回滚");
    }
    if !LEDGERED_TABLES.contains(&target_table) {
        tracing::warn!(target_table, "学习事件的目标表不在白名单里 —— 它将无法回滚");
    }
    // ds:any —— 全局审计表（见 recent_batches 的说明）
    // `trace_id` 列**不写**：它与 `batch_id` 恒等（同一个值 bind 了两遍），一列白存。
    // 列本身留在表里不删 —— 历史行还带着值，`DROP COLUMN` 会让旧账本少一列可读信息。
    let sql = "INSERT INTO meta.learn_event\
               (batch_id, actor, target_table, target_id, action, before, after)\
               VALUES ($1,$2,$3,$4,$5,$6,$7)";
    if let Err(e) = sqlx::query(sql)
        .bind(batch_id)
        .bind(actor)
        .bind(target_table)
        .bind(target_id)
        .bind(action)
        .bind(before)
        .bind(after)
        .execute(pg)
        .await
    {
        tracing::warn!(err = %e, target_table, "学习事件落账失败（学习本身已生效，但这一条将无法回滚）");
    }
}

/// 一个批次的摘要行（给 admin 列表用）。
///
/// 手写 `From<元组>` 而不是 `#[derive(FromRow)]`：本 crate 的 sqlx 没开 `macros` feature
/// （D6 的另一面 —— 不为一个派生宏开一整个 feature），元组解码同样精确。
#[derive(Debug, serde::Serialize)]
pub struct BatchRow {
    pub batch_id: String,
    pub actor: String,
    pub events: i64,
    pub tables: Vec<String>,
    /// 这一批的首/末事件时间（`::text`）。缺了它，账本回答不了「上周二学了什么」。
    pub first_at: String,
    pub last_at: String,
    /// 已撤条数（>0 = 这一批被回滚过，界面不该再让人点一次）
    pub rolled_back: i64,
}

/// 最近 N 天的学习批次（新→旧）。回答「上周二系统学了什么」。
pub async fn recent_batches(pg: &PgPool, days: i32, limit: i64) -> anyhow::Result<Vec<BatchRow>> {
    // ds:any —— `meta.learn_event` 是**全局审计表**（无 ds 列：一次学习可能同时动多个源的
    // 语料，按源切账本反而拼不回一次完整的学习行为），与 `meta.kv` 同类豁免。
    // 🔴 `min(at)` 原来只出现在 ORDER BY、没进结果集 —— 于是那句立项时写的
    // 「回答上周二学了什么」在**结构上就答不了**：列表里一个时间都没有。
    // `::text` 与 `admin_api` 的 `created_at::text` 同口径（省一个时间类型 feature，零新增依赖）。
    // `rolled_back` 也带出来：已经撤过的批次不该让人再点一次「回滚」。
    let rows: Vec<(String, String, i64, Vec<String>, String, String, i64)> = sqlx::query_as(
        "SELECT batch_id, min(actor) AS actor, count(*)::bigint AS events,                 array_agg(DISTINCT target_table) AS tables,                 min(at)::text AS first_at, max(at)::text AS last_at,                 count(*) FILTER (WHERE rolled_back_at IS NOT NULL)::bigint AS rolled_back          FROM meta.learn_event          WHERE at > now() - make_interval(days => $1) AND batch_id <> ''          GROUP BY batch_id ORDER BY min(at) DESC LIMIT $2",
    )
    .bind(days)
    .bind(limit)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(batch_id, actor, events, tables, first_at, last_at, rolled_back)| BatchRow {
            batch_id,
            actor,
            events,
            tables,
            first_at,
            last_at,
            rolled_back,
        })
        .collect())
}

/// 回滚一个批次：**倒序重放**每条事件的 `before`。
///
/// - `before IS NULL` → 该条是新增 → DELETE
/// - `before` 有值 → 该条是更新 → 用 before 里的 `status` 还原（当前三张表的可撤销改动
///   都只动 `status` 一列；动更多列的写口出现时在这里加分支，**不要**改成通用 UPDATE：
///   通用 UPDATE 意味着列名来自数据，那就是把外部输入拼进 SQL）。
///
/// 返回 [`Undone`]：**撤了几条、跳过几条、失败几条**。三个数字分开报是刻意的 ——
/// 端点要能诚实地说「撤了 3 条、跳过 1 条（目标行已不在）、失败 1 条（库报错）」，
/// 而不是一个含糊的总数。幂等：重复回滚同一批次第二次全是 0（已撤的事件行带着 `rolled_back_at`）。
pub async fn rollback_batch(pg: &PgPool, batch_id: &str) -> anyhow::Result<Undone> {
    // 🔴 空批次号 = **全表**：`WHERE batch_id = ''` 会匹配到所有没带批次号的历史事件，
    // 一次 POST 就能撤光全部学习。拒绝在最前面，不给任何分支绕过去的机会。
    anyhow::ensure!(!batch_id.trim().is_empty(), "空批次号不许回滚（那等于撤光全表）");
    // ds:any —— 同上：账本是全局审计面，按 batch_id 取回一次学习行为的全部事件
    let events = sqlx::query_as::<_, (i64, String, String, Option<serde_json::Value>)>(
        "SELECT id, target_table, target_id, before FROM meta.learn_event \
         WHERE batch_id = $1 AND rolled_back_at IS NULL ORDER BY id DESC",
    )
    .bind(batch_id)
    .fetch_all(pg)
    .await?;
    let mut out = Undone::default();
    for (id, table, target_id, before) in events {
        let Ok(target) = target_id.parse::<i64>() else {
            tracing::warn!(id, target_id, "学习事件的目标主键不是数字 —— 跳过该条");
            continue;
        };
        // 🔴 表名只从白名单常量取：拼进 SQL 的永远是编译期字面量
        // ds:any —— 回滚按**主键**定位（id 来自本批账本自己的记录，不是外部输入）：
        // 加 ds 谓词等于「只撤回某个源的那半批」，而一次学习行为要么整批撤要么不撤。
        let Some(stmt) = undo_stmt(&table, before.is_some()) else {
            tracing::warn!(table, "该表没有回滚分支 —— 跳过（白名单与分支必须同步加）");
            continue;
        };
        let q = sqlx::query(stmt).bind(target);
        let q = match &before {
            Some(v) => q.bind(v.get("status").and_then(|s| s.as_str()).unwrap_or("pending")),
            None => q,
        };
        match q.execute(pg).await {
            // 🔴 标记只在**真撤成功**（影响到行）时才落。撤失败也标 = 那一批永久撤不回来；
            // 影响 0 行 = 目标行已经不在了（别处删过），算跳过而不是成功。
            Ok(r) if r.rows_affected() > 0 => {
                out.undone += r.rows_affected();
                // ds:any —— 同上。写的是独立的 `rolled_back_at/by`，**不覆盖 action**：
                // 「这条当初是新增还是改状态」是复核时要看的第一件事。
                let marked = sqlx::query(
                    "UPDATE meta.learn_event SET rolled_back_at = now(), rolled_back_by = $2 WHERE id = $1",
                )
                .bind(id)
                .bind(batch_id)
                .execute(pg)
                .await;
                if let Err(e) = marked {
                    tracing::warn!(err = %e, id, "回滚已生效但标记没落上 —— 重跑会再撤一次（幂等由目标行不存在保证）");
                }
            }
            Ok(_) => {
                tracing::info!(id, table, "目标行已不在，跳过（不标记，留给人复核）");
                out.skipped += 1;
            }
            Err(e) => {
                tracing::warn!(err = %e, id, table, "回滚该条失败（继续下一条，不标记）");
                out.failed += 1;
            }
        }
    }
    Ok(out)
}

/// 回滚语句（**纯函数**，故判据能直接打它）。`None` = 该表没有回滚分支。
///
/// 🔴 表名与列名只能是**编译期字面量**：一旦改成通用 `UPDATE {table} SET {col}`，
/// 拼进 SQL 的就成了数据（`drift.rs` 的 `sql_interpolation_is_allowlisted` 也会当场红）。
/// 加表必须同步加分支，否则 `LEDGERED_TABLES` 里有、这里没有 = 静默跳过。
/// `$1` = 目标主键；`$2`（更新分支）= `before` 里的 status。
fn undo_stmt(table: &str, has_before: bool) -> Option<&'static str> {
    Some(match (table, has_before) {
        // ds:any —— 回滚按**主键**定位（id 来自本批账本自己的记录，不是外部输入）；
        // 加 ds 谓词等于「只撤某个源的那半批」，而一次学习行为要么整批撤要么不撤。
        ("meta.sql_exemplar", false) => "DELETE FROM meta.sql_exemplar WHERE id = $1", // ds:any
        ("meta.memory", false) => "DELETE FROM meta.memory WHERE id = $1", // ds:any
        ("meta.pitfall", false) => "DELETE FROM meta.pitfall WHERE id = $1", // ds:any
        ("meta.sql_exemplar", true) => "UPDATE meta.sql_exemplar SET status = $2 WHERE id = $1", // ds:any
        ("meta.pitfall", true) => "UPDATE meta.pitfall SET status = $2 WHERE id = $1", // ds:any
        _ => return None,
    })
}

/// 一次回滚的三个数字。**分开报**：一个总数说不清「没撤成」和「本来就没有」的差别。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Undone {
    /// 真撤掉的行数
    pub undone: u64,
    /// 目标行已不在（别处删过）—— 不算失败，也不标记
    pub skipped: u64,
    /// 库报错 —— 不标记，重跑还能再来
    pub failed: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 🔴 回滚 SQL 的表名只能是编译期字面量（否则就是把数据拼进 SQL）。
    #[test]
    fn rollback_statements_are_compile_time_literals() {
        let src = include_str!("learn.rs");
        assert!(
            src.contains(r#"anyhow::ensure!(!batch_id.trim().is_empty()"#),
            "空批次号必须在最前面拒绝 —— 它等于撤光全表"
        );
        // 判据打**纯函数** `undo_stmt`（2026-08-14 从 rollback_batch 里抽出来：
        // 那个函数当时 60+ 行、越过 D1 的 40 行，而这段又恰恰是最不能出错的一段）
        let head = src.split("fn undo_stmt(").nth(1).expect("undo_stmt 改名了");
        let head = head.split("
}").next().unwrap();
        assert!(!head.contains("format!"), "回滚 SQL 不许拼串：{head}");
        assert!(!head.contains("{table}"), "表名不许来自数据：{head}");
        for t in LEDGERED_TABLES {
            assert!(head.contains(t), "白名单里的 {t} 没有回滚分支 —— 它会被静默跳过");
        }
        // 行为半（纯函数可直接调）：新增走 DELETE、更新走 UPDATE、没登记的表返 None
        // ds:any —— 下面几行是**判据**不是查询（漂移守卫按行窗口扫，得给它一个豁免标记）
        assert_eq!(undo_stmt("meta.memory", false), Some("DELETE FROM meta.memory WHERE id = $1"));
        assert!(undo_stmt("meta.pitfall", true).unwrap().starts_with("UPDATE meta.pitfall SET status"));
        assert_eq!(undo_stmt("meta.memory", true), None, "memory 没有可撤的状态列 —— 不许静默当成功");
        assert_eq!(undo_stmt("meta.query_log", false), None, "白名单外的表不许有回滚分支");
    }

    /// 落账失败不许传播：账本是观测面，学习是主路。
    #[test]
    fn ledger_failure_never_breaks_learning() {
        let src = include_str!("learn.rs");
        // 切到函数体结束（下一个顶层 `///`）为止：切到文件尾会把别的函数一起收进来，
        // 那样断言就不再是在说这个函数（判据要盯准它的对象）。
        let body = src
            .split("pub async fn log_event")
            .nth(1)
            .expect("函数改名了")
            .split("
///")
            .next()
            .unwrap();
        assert!(!body.contains("-> anyhow::Result"), "log_event 不该返回 Result：{body}");
        assert!(body.contains("tracing::warn!"), "落账失败必须留痕：{body}");
        assert!(!body.contains("?;"), "落账失败不许用 ? 传播：{body}");
    }
    /// 🔴 `learn.rs` 文件头白纸黑字写着「接漏了由本判据抓」，而它此前全仓零命中 ——
    /// 下一个人加第五个写口时不会有任何东西变红，账本会从那里开始漏。
    ///
    /// 判两件事：① 每个学习状态写口后面 25 行内必须有 `log_event`；
    /// ② 账本调用的**批次号/操作者不许是字面量空串**（那样的事件永远撤不回来）。
    /// `set_embedding` / `bump_hits` / 两张日志表的 INSERT **不在清单里** ——
    /// 它们是观测写入不是学习状态，别往里加。
    #[test]
    fn learn_writes_are_all_ledgered() {
        const WRITES: &[&str] = &[
            "INSERT INTO meta.sql_exemplar",
            "INSERT INTO meta.pitfall",
            "INSERT INTO meta.memory",
            "UPDATE meta.pitfall SET status",
            // 2026-08-14 补进清单：语料的状态变更（AI 初筛 / 人工复核）同样是学习状态，
            // 此前完全在账本之外 —— AI 把一条语料打成 disabled，撤都撤不回来。
            "UPDATE meta.sql_exemplar",
        ];
        let mut checked = 0;
        // 三个文件都要扫：`pitfall.rs` 是 2026-08-14 从 exemplar 拆出来的（D2 >500 必拆），
        // 拆的时候这条判据当场变红（少数到一个写口）—— 它正是这么用的。
        for src in [include_str!("exemplar.rs"), include_str!("memory.rs"), include_str!("pitfall.rs")] {
            let lines: Vec<&str> = src.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if !WRITES.iter().any(|w| line.contains(w)) {
                    continue;
                }
                checked += 1;
                // 窗口取**前后各 25 行**：`set_lesson_status` 的落账在 UPDATE **之前**
                // （要先读前值才撤得回来），只往后看会假红 —— 这条测试第一次跑就抓到了它。
                let window = lines[i.saturating_sub(25)..lines.len().min(i + 25)].join("
");
                // 两种算数：直接落账，或走共用的落账小函数（`ledger_status_change` 内部就是
                // 一次 `log_event` —— 三个状态写口共用一份读前值+落账，好过抄三遍）
                assert!(
                    window.contains("learn::log_event") || window.contains("ledger_status_change("),
                    "第 {} 行的学习写口没落账本：{line}",
                    i + 1
                );
                assert!(
                    !window.contains(r#"log_event(
            pg, "", """#),
                    "第 {} 行的账本调用还在写空批次号：那条学习永远撤不回来",
                    i + 1
                );
            }
        }
        assert!(checked >= 6, "只扫到 {checked} 个学习写口 —— 切漏了（今天有 6 处）");
    }
    /// 🔴 回滚标记只在**真撤成功**时落，且不许覆盖 `action`。
    ///
    /// 反面两条都实测过代价：撤失败也标 → 那一批**永久撤不回来**；覆盖 action →
    /// 连「这条当初是新增还是改状态」都查不出来（复核第一眼要看的就是它）。
    #[test]
    fn rollback_marks_only_what_it_really_undid() {
        let src = include_str!("learn.rs");
        let body = src
            .split("pub async fn rollback_batch")
            .nth(1)
            .expect("函数改名了")
            .split("
}")
            .next()
            .unwrap();
        assert!(!body.contains("action = 'rolled_back'"), "又在覆盖 action 列：{body}");
        assert!(body.contains("rolled_back_at IS NULL"), "取事件的谓词还在看 action：{body}");
        // 标记必须与「影响到行」出现在同一分支
        let arm = body
            .split("Ok(r) if r.rows_affected() > 0")
            .nth(1)
            .expect("成功分支没了 —— 标记会落在失败上");
        let arm = arm.split("Ok(_) =>").next().unwrap();
        assert!(arm.contains("rolled_back_at = now()"), "成功分支里没落标记：{arm}");
        // 失败/跳过分支一律不标
        let rest = body.split("Ok(_) =>").nth(1).unwrap();
        assert!(!rest.contains("rolled_back_at = now()"), "跳过或失败也标了标记：{rest}");
    }
    /// 🔴 账本必须能回答「上周二学了什么」—— 那是它立项时写下的那句话。
    /// `min(at)` 原来只在 ORDER BY 里，结果集一个时间都没有，结构上就答不了。
    #[test]
    fn batch_listing_carries_time_and_rollback_state() {
        let src = include_str!("learn.rs");
        let body = src
            .split("pub async fn recent_batches")
            .nth(1)
            .expect("函数改名了")
            .split("
}")
            .next()
            .unwrap();
        assert!(body.contains("min(at)::text AS first_at"), "列表没有起始时间：{body}");
        assert!(body.contains("max(at)::text AS last_at"), "列表没有结束时间：{body}");
        assert!(
            body.contains("FILTER (WHERE rolled_back_at IS NOT NULL)"),
            "撤过的批次要看得出来，否则界面会让人再点一次回滚：{body}"
        );
    }
}
