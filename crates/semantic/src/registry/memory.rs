//! 【S4】经验复盘（datanote `AiMemoryService` 的精简对应物）：蒸馏 → 向量化 → 召回。
//!
//! - **蒸馏（learn）**：口径/执行回炉**成功**后，agent 把「这问首版错过、修正版对了」
//!   沉淀为一条 `kind=review` 经验（零 LLM 模板文本 —— 修正版 SQL 本身就是教材，
//!   再花一次模型调用去改写它只会引入新的错）。embedding 留 NULL，
//!   由 A9 向量自愈（`MetaVecTarget::Memory`）按同一配方补。
//! - **召回（recall）**：问句向量近邻取 10 条，Rust 侧按
//!   `sim × (1 + 0.1·ln(1+hit)) × exp(-age/30天)` 重排（datanote 的 vector+hitCount+recency
//!   同构），取前 N 条进 prompt 的「经验复盘」段，命中行 `hit_count+1`。
//!
//! 🔴 **经验绝不进口径判据与闸门**：它是未连库验证的二手材料（判据的输入只有
//! 声明表与当轮 SQL）。prompt 段标题已标注「参考，不是硬约束」（I5 同族防线）。

use sqlx::PgPool;

/// 一条召回的经验（`sim` 与 `age_days` 只在重排时用，不进 prompt）
#[derive(Clone, Debug)]
pub struct MemoryHit {
    pub id: i64,
    pub kind: String,
    pub content: String,
    pub hit_count: i64,
    pub age_days: f64,
    pub sim: f64,
}

/// 蒸馏写入。`content` 截 400 字、`question` 截 200 字（与日志表同口径：经验是提示不是
/// 教材全文，截断同时是去重键的一部分）。
/// 同 `(ds_id, kind, question)` 已存在则不重复沉（`NOT EXISTS`，与 `exemplar::save` 同形）：
/// 同一问句再次回炉成功不产生孪生行 —— 旧条的 hit_count 还在涨， rerank 自然把它顶上来。
// 归属人 `login`：空串 = ds 级公有（只有人工复核通道该写空串）；自动蒸馏一律写真实 login
// —— 一个用户的修正经验此前会直接进全员 prompt（跨用户污染，与 I4 同一条防线）。
#[allow(clippy::too_many_arguments)]
pub async fn save_memory(
    pg: &PgPool,
    // `who` = (批次号, 操作者)：账本用它定位「这一轮学了什么」。此前这里把 `conv_id` 当批次号
    // 传给账本，而调用方给的又是空串 —— 两个坑叠在一起：账本撤不回来、`meta.memory.conv_id`
    // 也恒空（追问链回看不到这条经验属于哪次会话）。现在两者分开各给各的。
    who: (&str, &str),
    ds: &str,
    login: &str,
    conv_id: &str,
    kind: &str,
    question: &str,
    content: &str,
) -> anyhow::Result<bool> {
    // 判官模式：观察系统不许改变系统（见 `registry::judge_mode`）
    if super::judge_mode() {
        return Ok(false);
    }
    let content: String = content.chars().take(400).collect();
    let question: String = question.chars().take(200).collect();
    let row: Option<(i64,)> = sqlx::query_as(
        "INSERT INTO meta.memory(ds_id, login_name, conv_id, kind, question, content) \
         SELECT $1,$2,$3,$4,$5,$6 WHERE NOT EXISTS( \
           SELECT 1 FROM meta.memory WHERE ds_id=$1 AND login_name=$2 AND kind=$4 AND question=$5) \
         RETURNING id",
    )
    .bind(ds)
    .bind(login)
    .bind(conv_id)
    .bind(kind)
    .bind(&question)
    .bind(&content)
    .fetch_optional(pg)
    .await?;
    // 学习账本：新增记 before=NULL（回滚即删除）。落账失败不拖垮学习（见 learn.rs 文件头）
    if let Some((id,)) = row {
        super::learn::log_event(
            pg, who.0, who.1, "meta.memory", &id.to_string(), "insert", None,
            Some(serde_json::json!({ "kind": kind, "question": question })),
        )
        .await;
    }
    Ok(row.is_some())
}

/// 向量近邻召回 + 重排。`qvec=None`（embed 缺席）→ 空，与各路召回同一降级语义。
/// 🔴 硬上限：近邻粗排固定取 10 条再 rerank —— `limit > 10` 的调用者静默只得 10 条
/// （钉在这里：上限不是 limit 形参能放宽的）。
pub async fn recall_memories(
    pg: &PgPool,
    ds: &str,
    login: &str,
    qvec: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<MemoryHit>> {
    let Some(v) = qvec else { return Ok(vec![]) };
    let rows: Vec<(i64, String, String, i64, f64, f64)> = sqlx::query_as(
        "SELECT id, kind, content, hit_count::bigint, \
                (EXTRACT(EPOCH FROM (now() - created_at)) / 86400.0)::float8, \
                1 - (embedding <=> $1::vector) \
         FROM meta.memory \
         WHERE ds_id = $2 AND (login_name = $3 OR login_name = '') AND embedding IS NOT NULL \
         ORDER BY embedding <=> $1::vector LIMIT 10",
    )
    .bind(v)
    .bind(ds)
    .bind(login)
    .fetch_all(pg)
    .await?;
    let mut hits: Vec<MemoryHit> = rows
        .into_iter()
        .map(|(id, kind, content, hit_count, age_days, sim)| {
            MemoryHit { id, kind, content, hit_count, age_days, sim }
        })
        .collect();
    rerank(&mut hits);
    hits.truncate(limit);
    Ok(hits)
}

/// 重排分（纯函数，判据打这里）：相似度是主词，命中次数对数加权，按 30 天 1/e 衰减
/// （exp(-age/30)：30 天剩 1/e，半衰期实为 20.8 天）。
/// datanote 的 `vector + hitCount + recency` 同构 —— 高分新条与屡验旧条都能赢，
/// 纯按向量排会被「一次都没被印证过的新条」刷屏。
/// `sim` 可为负/略>1（浮点余弦）：负分参与排序无害（相对序不变），刻意不 clamp。
/// `age_days < 0`（时钟回拨/未来 created_at）按 0 计（负年龄会给 exp 正增益）。
pub fn score(h: &MemoryHit) -> f64 {
    h.sim * (1.0 + 0.1 * (1.0 + h.hit_count as f64).ln()) * (-h.age_days.max(0.0) / 30.0).exp()
}

pub fn rerank(hits: &mut [MemoryHit]) {
    hits.sort_by(|a, b| score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal));
}

/// 命中计数 +1（召回侧 fire-and-forget；失败最多让 rerank 少点依据，不值得拖慢问答）。
/// ds 谓词豁免：id 来自本轮 `recall_memories` 的召回结果（本可信主键），不是外部输入。
pub async fn bump_hits(pg: &PgPool, ids: &[i64]) -> anyhow::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query("UPDATE meta.memory SET hit_count = hit_count + 1 WHERE id = ANY($1)")
        .bind(ids)
        .execute(pg)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: i64, sim: f64, hit_count: i64, age_days: f64) -> MemoryHit {
        MemoryHit { id, kind: "review".into(), content: String::new(), hit_count, age_days, sim }
    }

    /// 重排：sim 是主词；同 sim 时多印证的赢；陈旧条输给等 sim 的新条。
    #[test]
    fn rerank_prefers_proven_and_fresh() {
        let mut v = vec![
            hit(1, 0.80, 0, 0.0),   // 新、没印证过
            hit(2, 0.80, 20, 0.0),  // 同 sim、20 次印证 → 赢 1；且 0.3 对数加成 > 3 的 0.1 sim 差
                                    // （屡验条赢过略像的新条 —— 这是**设计**，datanote hitCount 同构）
            hit(3, 0.90, 0, 0.0),   // sim 高 0.1 但零印证 → 第二
            hit(4, 0.80, 20, 90.0), // 90 天旧条：衰减 exp(-3) ≈ 0.05 → 垫底
        ];
        rerank(&mut v);
        let ids: Vec<i64> = v.iter().map(|h| h.id).collect();
        assert_eq!(ids, [2, 3, 1, 4], "{ids:?}");
        assert!(score(&hit(0, 0.80, 20, 90.0)) < score(&hit(0, 0.80, 20, 0.0)));
        assert!(score(&hit(0, 0.80, 20, 0.0)) > score(&hit(0, 0.80, 0, 0.0)));
        // 全零安全（不 NaN）
        assert!(score(&hit(0, 0.0, 0, 0.0)) == 0.0);
        // 负年龄（时钟回拨/未来 created_at）按 0 计，不给 exp 正增益
        assert_eq!(score(&hit(0, 0.8, 0, -5.0)), score(&hit(0, 0.8, 0, 0.0)));
    }

    /// bump 空清单不许发 SQL（无谓往返）+ SQL 形状锚点
    #[test]
    fn bump_empty_is_noop() {
        // 空列表提前返回是纯逻辑（源码锚点守），非空路径要连库，不进单测
        let src = include_str!("memory.rs");
        let body = src.split("pub async fn bump_hits").nth(1).expect("函数改名了");
        assert!(body.contains("ids.is_empty()"), "空清单早退被删了：{body}");
        assert!(body.contains("hit_count = hit_count + 1"), "{body}");
        assert!(body.contains("id = ANY($1)"), "{body}");
    }

    /// 蒸馏去重键 = (ds_id, kind, question)：同问句再次回炉成功不产孪生行。
    #[test]
    fn save_dedups_on_question() {
        let src = include_str!("memory.rs");
        let body = src.split("pub async fn save_memory").nth(1).expect("函数改名了");
        assert!(body.contains("NOT EXISTS"), "{body}");
        assert!(body.contains("kind=$4 AND question=$5"), "{body}");
        // 作用域进了去重键：同一句话不同用户各留一条个人经验，互不覆盖也互不可见
        assert!(body.contains("login_name=$2"), "去重键少了归属人：{body}");
        // 截长判据（经验是提示不是教材全文）
        assert!(body.contains(".take(400)"), "{body}");
    }

    /// 🔴 召回 SELECT 的形状锚点（实测抓到的：`hit_count` 是 INT4，Rust 读 i64 直接
    /// 解码报错 → 经验段静默缺席，只剩一条 warn）。列类型与 Rust 元组必须逐位对齐。
    #[test]
    fn recall_select_casts_hit_count() {
        let src = include_str!("memory.rs");
        let body = src.split("pub async fn recall_memories").nth(1).expect("函数改名了");
        assert!(body.contains("hit_count::bigint"), "INT4 列必须显式转 bigint：{body}");
        assert!(body.contains(")::float8"), "EXTRACT 是 NUMERIC，必须转 float8：{body}");
        assert!(body.contains("embedding IS NOT NULL"), "{body}");
        // 🔴 作用域谓词：本人经验 + ds 级公有，别人的私人经验一条都不许召回（I4）
        assert!(
            body.contains("(login_name = $3 OR login_name = '')"),
            "召回缺作用域谓词：全公司共用一个经验池：{body}"
        );
        assert!(body.contains("LIMIT 10"), "近邻粗排取 10 再 rerank 的形状被改了：{body}");
    }
}
