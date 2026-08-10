//! A1 第三段：**注册**。命中结果 `DictHit` → `meta.value_map`（全码 eq 换码）
//! + `meta.dimension`（CASE 翻名）。变更原因＝自动发现的落库形态。
//!
//! 搬运源 `server/src/meta.rs:1464-1517`（两条 upsert SQL、CASE 拼装、维度名清洗逐字保留）。
//! 原 `register_match` 的 9 个连排参数由 `DictHit` 收口（D4）。
//! 名称型值域取值（`register_domain_values`）落同一张 `meta.value_map`，但**不派生维度**。

use sqlx::PgPool;

use crate::registry::clean_dim_name;

/// 一次对码命中。`table`/`col`/`comment` 借候选行，其余是 `best_dict_match` 的产出。
pub struct DictHit<'a> {
    pub table: &'a str,
    pub col: &'a str,
    /// 列注释：维度名的第一来源（清洗不出才退回字典名）
    pub comment: &'a str,
    pub dict_key: String,
    pub dict_name: String,
    /// 字典**全码**（不止抽样命中的那几个）：未来新值也自适应
    pub pairs: Vec<(String, String)>,
    pub coverage: f64,
    /// 抽样到的不同值个数（只进返回给运维看的 JSON）
    pub distinct: usize,
}

/// 注册一次命中，返回给运维看的那条 JSON（顺序＝先值映射、后维度、最后成条目）。
pub async fn register_match(
    pg: &PgPool,
    ds: &str,
    h: &DictHit<'_>,
) -> anyhow::Result<serde_json::Value> {
    // 注册 value_map（eq）——字典全码注册，未来新值也自适应
    for (code, name) in &h.pairs {
        // `origin` 盖 dict 章：这一列的取值**可证完整枚举**（登记字典全码 + 抽样 uniq > 60 整列跳过），
        // 于是 `caliber::load_enum_values`（只取 dict 那批）才会把它造成 `RequireKnownValue`
        // ——「值不在码表 → SQL 合法 → 返 0 行 → 用户读成『没有这类数据』」那族静默错答的唯一判据。
        // 🔴 `DO UPDATE` 里也必须带：DDL 默认值是最保守的 `seed`，而**既有 936 行**全是默认值
        // 落的；不带就重跑也纠不回来，判据永远休眠（上一轮整轮休眠的原因，裁决 二·AQ8）。
        sqlx::query(
            "INSERT INTO meta.value_map(table_name, column_name, name, code, match_kind, ds_id, origin)
             VALUES ($1,$2,$3,$4,'eq',$5,$6)
             ON CONFLICT (ds_id, table_name, column_name, name) DO UPDATE SET code=$4, match_kind='eq', origin=$6",
        )
        .bind(h.table)
        .bind(h.col)
        .bind(name)
        .bind(code)
        .bind(ds)
        // 常量而非字面量：`ddl::tests::value_map_origin_defaults_to_the_most_conservative`
        // 守着这三个常量与 DDL 默认值的耦合，写死 'dict' 就绕开了那道门。
        // 走 **bind 不走插值**：`tests/drift.rs` 的 SQL 插值白名单只放 `ds_pred`。
        .bind(crate::ddl::VALUE_ORIGIN_DICT)
        .execute(pg)
        .await?;
    }
    // 注册 dimension（CASE 翻名；码数 >60 仅注册值映射，CASE 过长伤 prompt）
    if h.pairs.len() <= 60 {
        register_dimension(pg, ds, h).await?;
    }
    Ok(serde_json::json!({
        "table": h.table, "column": h.col, "dict": h.dict_key, "dict_name": h.dict_name,
        "distinct_values": h.distinct, "coverage": h.coverage,
    }))
}

/// 名称型值域取值入库：`meta.value_map` 的 `name` 与 `code` **各为取值本身**
/// （裁决：复用码值表，不新建 —— DDL/主键/写入路径全现成）。返回写入条数。
///
/// **先删该 (ds,表,列) 旧行再插**：分类改过名，旧名不许永久残留（重跑即自适应）。
/// **绝不**顺手给名称型生成 `meta.dimension` 的 CASE 翻名（码型路径会做那件事）：
/// 60 个分类名的 CASE 是纯垃圾，会把 prompt 撑爆。
pub async fn register_domain_values(
    pg: &PgPool,
    ds: &str,
    table: &str,
    col: &str,
    values: &[String],
) -> anyhow::Result<usize> {
    sqlx::query(
        "DELETE FROM meta.value_map
         WHERE ds_id = $1 AND table_name = $2 AND column_name = $3",
    )
    .bind(ds)
    .bind(table)
    .bind(col)
    .execute(pg)
    .await?;
    for v in values {
        // DISTINCT 后 trim 仍可能撞名（' 手抓饼' / '手抓饼'）→ DO NOTHING
        // `origin` 盖 probe 章（**不是** dict）：抽样上限 `DOMAIN_LIMIT = 2000` 会截断 ⇒ 不是完整
        // 枚举 ⇒ `RequireKnownValue` 一律不许对这批开火，否则每个未抽到的真取值都是一次假红。
        // `DO NOTHING` 不必带 origin：上面刚 DELETE 过该 (ds,表,列)，走到这儿的都是新行。
        sqlx::query(
            "INSERT INTO meta.value_map(table_name, column_name, name, code, match_kind, ds_id, origin)
             VALUES ($1,$2,$3,$3,'eq',$4,$5) ON CONFLICT DO NOTHING",
        )
        .bind(table)
        .bind(col)
        .bind(v)
        .bind(ds)
        .bind(crate::ddl::VALUE_ORIGIN_PROBE)
        .execute(pg)
        .await?;
    }
    Ok(values.len())
}

async fn register_dimension(pg: &PgPool, ds: &str, h: &DictHit<'_>) -> anyhow::Result<()> {
    let (col, table) = (h.col, h.table);
    let cases: String = h
        .pairs
        .iter()
        .map(|(c, n)| format!("WHEN '{}' THEN '{}'", c.replace('\'', ""), n.replace('\'', "")))
        .collect::<Vec<_>>()
        .join(" ");
    let expr = format!("CASE `{col}` {cases} END");
    let dim_code: String = format!("auto_{table}_{col}").chars().take(80).collect();
    // 维度名取列注释的**首段**：注释常是「配送状态：100:待配送, 200:配送中」这种带码值说明的长句，
    // 整句当维度名既不可能被问句命中，又污染注册表（同名重复十几条）。清洗不出就退回字典名。
    let dim_name = match clean_dim_name(h.comment) {
        Some(n) => n,
        None => h.dict_name.clone(),
    };
    let desc = format!(
        "自动发现：编码列对码字典 {}({})，抽样覆盖率 {:.0}%",
        h.dict_name, h.dict_key, h.coverage
    );
    sqlx::query(
        "INSERT INTO meta.dimension(dim_code, name, aliases, source_table, expr, description, ds_id)
         VALUES ($1,$2,'{}',$3,$4,$5,$6)
         ON CONFLICT (ds_id, dim_code) DO UPDATE SET name=$2, source_table=$3, expr=$4, description=$5",
    )
    .bind(&dim_code)
    .bind(&dim_name)
    .bind(table)
    .bind(&expr)
    .bind(&desc)
    .bind(ds)
    .execute(pg)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// 🔴 **接线**判据：两处 `meta.value_map` 写入都必须盖来源章，且 dict 那条的
    /// `DO UPDATE` 里也要带。无库单测覆盖不到这段 IO，故照 `agent::gather` 的
    /// `gather_all_cards_actually_reads_the_registry` 用**源码**守。
    ///
    /// 载荷是「唤醒」：`origin` 的 DDL 默认值是最保守的 `seed`，`caliber::load_enum_values`
    /// 只取 dict 那批 —— 这两行 `bind` 一去掉，那条 SQL 恒 0 行 ⇒ `enum_rules` 恒空
    /// ⇒ `RequireKnownValue` 一次都不会触发（上一轮就这么整轮休眠，裁决 二·AQ8）。
    /// `DO UPDATE` 里漏带更隐蔽：新库全对，而既有库那 936 行永远停在 seed，重跑也纠不回来。
    #[test]
    fn both_value_map_writes_stamp_the_origin() {
        const SRC: &str = include_str!("register.rs");
        // 切一段函数体（含它下方那个函数的文档注释，无害）。防恒真门槛按**比例**而不是绝对字节：
        // 实测 SRC 9700 字节 / dict 段 2507 / probe 段 1187，而下界 `to` 切歪时 body 是
        // 「register_match 到文件末尾」≈8100 —— 1/3（3233）把两者分得开，且不会因为将来注释变长而假红。
        let seg = |from: &str, to: &str| -> String {
            let s = SRC.split(from).nth(1).expect("函数改名了 —— 顺手把这条判据一起改");
            let body = s.split(to).next().unwrap().to_string();
            assert!(
                body.len() < SRC.len() / 3,
                "切段没切住，{} 字符（SRC {}）—— 断言会因为看的是整份源码而恒真",
                body.len(),
                SRC.len()
            );
            assert!(body.contains("INSERT INTO meta.value_map"), "切段没切住：{body}");
            body
        };
        // ① 字典对码：INSERT 与 DO UPDATE **两段都要**盖章。
        // 🔴 从 `INSERT INTO` 起切，不能直接 `dict.split_once("DO UPDATE")` ——
        // 上面那条注释里就写着「DO UPDATE 里也必须带」，第一次命中的是注释而不是 SQL，
        // `ins` 于是不含 INSERT 那一行（本判据第一版就这么红了一次，恰好是它该有的表现）。
        let dict = seg("pub async fn register_match", "pub async fn register_domain_values");
        let sql = dict.split_once("INSERT INTO meta.value_map").expect("dict 那条 INSERT 没了").1;
        let (ins, upd) = sql.split_once("DO UPDATE").expect("dict 那条丢了 ON CONFLICT DO UPDATE");
        assert!(ins.contains(", origin)"), "INSERT 的列清单没写 origin：{ins}");
        assert!(
            upd.lines().next().unwrap().contains("origin="),
            "DO UPDATE 没带 origin —— 既有行永远停在 seed，判据永远休眠：{upd}"
        );
        assert!(dict.contains("VALUE_ORIGIN_DICT"), "{dict}");
        // ② 名称型探针：2000 封顶会截断 ⇒ 必须是 probe，盖成 dict 就是给每个未抽到的真取值判假红
        let probe = seg("pub async fn register_domain_values", "async fn register_dimension");
        assert!(probe.contains(", origin)") && probe.contains("VALUE_ORIGIN_PROBE"), "{probe}");
        // ③ 三个来源一律走 `ddl.rs` 的常量：写字面量就绕开了
        //    `ddl::tests::value_map_origin_defaults_to_the_most_conservative` 那道耦合判据。
        //    两处刻意：needle **拼起来写**（直接写死那五个字符，这行断言自己就会被自己匹配到 ——
        //    `admin_api::no_create_exemplar_route` 踩过同一个坑）；且**只扫代码行**
        //    （注释里必须能引用那几个取值来解释理由，否则这条判据会逼着注释说不清话 ——
        //    第一版就是被自己上面那句注释判红的）。
        use crate::ddl::{VALUE_ORIGIN_DICT, VALUE_ORIGIN_PROBE, VALUE_ORIGIN_SEED};
        let code: Vec<&str> = SRC.lines().filter(|l| !l.trim_start().starts_with("//")).collect();
        // 防恒真：代码行必须真的还在（全被滤掉的话下面三条恒绿）
        assert!(code.iter().any(|l| l.contains("INSERT INTO meta.value_map")), "代码行全被滤掉了");
        for c in [VALUE_ORIGIN_SEED, VALUE_ORIGIN_DICT, VALUE_ORIGIN_PROBE] {
            let lit = format!("'{c}'");
            assert!(!code.iter().any(|l| l.contains(&lit)), "别写字面量 {lit}，用 ddl.rs 的常量");
        }
    }
}
