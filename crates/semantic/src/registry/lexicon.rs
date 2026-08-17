//! 文本命中侧的注册表行类型与读取：术语 / 值链接码表。
//! 变更原因＝命中侧读到的形状。召回的命中与卡片渲染在 `recall/*`，这里只管取行。
//!
//! 搬运源 `server/src/meta.rs:871-877`（`recall_terms` 的加载段）与
//! `server/src/meta.rs:1133-1140`（`recall_value_hints` 的加载段）—— SQL 文本与绑定序号原样。
//!
//! `DocBinding` 本轮不落：`meta.doc_binding` 这张表还不存在（DDL 16 张里没有它，
//! 单号直查读的是 `direct.rs` 里的硬编码前缀表），造个空类型只是替将来占位。

use crate::registry::{catalog_allows_column, ds_pred, table_asset_live_pred_at};
use sqlx::PgPool;

/// 业务术语（meta.term 行）
#[derive(Debug)]
pub struct TermDef {
    pub term: String,
    pub definition: String,
    pub aliases: Vec<String>,
}

/// 值链接码表（meta.value_map 行）。`match_kind`：eq=等值换码 / like=组合值列须 LIKE '%码%'
/// （与 `model::ValueRef` 同表同字段的另一份行类型：字段名不同，合并要动 server 侧消费点
/// —— 欠账，两处注释互指。）
#[derive(Debug)]
pub struct ValueMap {
    pub table_name: String,
    pub column_name: String,
    pub name: String,
    pub code: String,
    pub match_kind: String,
}

/// 实体名值域声明（meta.value_domain 行）：这一列的取值是**业务实体名**，不是码值。
/// **取值不在这张表里**：由 `meta autodiscover` 的名称型探针灌进 `meta.value_map`
/// （`name = code = 取值`，复用码值表不新建 —— 重跑即自适应），读取见 `load_domain_values`。
#[derive(Debug)]
pub struct ValueDomain {
    pub table_name: String,
    pub column_name: String,
    /// 人话：该用哪一列过滤、误用哪一列会怎样（LLM 逐字读，渲染进值域命中卡）
    pub note: String,
}

pub async fn load_value_domains(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<ValueDomain>> {
    let ds_pred = format!(
        "{}{}",
        crate::registry::ds_pred(1),
        table_asset_live_pred_at("", 1)
    );
    // ORDER BY 钉死行序：caliber 的 domain_rules 产出规则序不随物理行序漂
    let rows: Vec<(String, String, String)> = sqlx::query_as(&format!(
        "SELECT table_name, column_name, note FROM meta.value_domain WHERE 1 = 1{ds_pred}
         ORDER BY table_name, column_name",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(table_name, column_name, ..)| {
            catalog_allows_column(ds, table_name, column_name)
        })
        .map(|(table_name, column_name, note)| ValueDomain { table_name, column_name, note })
        .collect())
}

/// 名称型值域的**取值** `(表, 列, 取值)`：`meta.value_map` 里 `(表,列)` 在 `meta.value_domain`
/// 登记过的那批（名称型的 `name = code = 取值`）。
/// 交集在 SQL 里 JOIN 完 —— 不查全表再回 Rust 过滤（谓词一律留在 SQL 内是本仓纪律）。
pub async fn load_domain_values(
    pg: &PgPool,
    ds: &str,
) -> anyhow::Result<Vec<(String, String, String)>> {
    let ds_pred = format!(
        "{}{}",
        crate::registry::ds_pred_at("v", 1),
        table_asset_live_pred_at("v", 1)
    );
    let rows: Vec<(String, String, String)> = sqlx::query_as(&format!(
        "SELECT v.table_name, v.column_name, v.name FROM meta.value_map v
         JOIN meta.value_domain d ON d.table_name = v.table_name
           AND d.column_name = v.column_name AND d.ds_id IN (v.ds_id, '*')
         WHERE 1 = 1{ds_pred}
         ORDER BY v.table_name, v.column_name, v.name",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(table_name, column_name, ..)| {
            catalog_allows_column(ds, table_name, column_name)
        })
        .collect())
}

/// 值域命中：问句里出现的最长取值（纯函数，`values` 是该列的取值集）。
///
/// **最长优先**是必须的：实体名值域里「手抓饼」与「手抓饼卷」并存，短名先中会把统计范围放大。
/// 单字取值一律不算（与 `recall_value_hints` 的 `>= 2` 同一门槛，避免「饼」命中一切）。
///
/// ponytail: 按长度倒序 `contains`，O(n·m)。值域规模百级（真库 60 个分类名），真到万级再谈
/// 多模式自动机 —— 本轮明令不引 aho-corasick。
pub fn longest_value_hit<'a>(
    question: &str,
    values: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    // 长度键随收集预算一次（原来 sort_by_key 的 key 在 O(n log n) 次比较里重算 chars().count()）
    let mut vs: Vec<(usize, &str)> = values
        .into_iter()
        .filter_map(|v| {
            let n = v.chars().count();
            (n >= 2).then_some((n, v))
        })
        .collect();
    // 早退判空：空取值集不进 sort/find
    if vs.is_empty() {
        return None;
    }
    vs.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    vs.into_iter().find(|(_, v)| question.contains(*v)).map(|(_, v)| v)
}

/// 术语加载。无 asset-live 谓词的豁免说明：term 不挂物理表（纯文本知识，无表活性可判），
/// 刻意只按 status/ds 过滤 —— 漂移守卫（grep 谓词）读到这里别当漏网。
pub async fn load_terms(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<TermDef>> {
    let rows: Vec<(String, String, Vec<String>)> = sqlx::query_as(&format!(
        "SELECT term, definition, aliases FROM meta.term WHERE status = 'active'{ds_pred}",
        ds_pred = ds_pred(1)
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(term, definition, aliases)| TermDef { term, definition, aliases })
        .collect())
}

/// 命中侧码值全量加载。
/// 🔴 与 `model::load_value_map` 同表两份加载：过滤口径（`catalog_allows_column` vs
/// `catalog_allows_table`）、返回类型都不同 —— 各自服务不同判据，改一边先看另一边。
/// ORDER BY 与 model 侧同序（确定性：同名多列的卡序/码查找不随物理行序漂）。
pub async fn load_value_maps(pg: &PgPool, ds: &str) -> anyhow::Result<Vec<ValueMap>> {
    let ds_pred = format!(
        "{}{}",
        crate::registry::ds_pred(1),
        table_asset_live_pred_at("", 1)
    );
    let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(&format!(
        "SELECT table_name, column_name, name, code, match_kind
         FROM meta.value_map WHERE 1 = 1{ds_pred} ORDER BY name, table_name, column_name",
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(table_name, column_name, ..)| {
            catalog_allows_column(ds, table_name, column_name)
        })
        .map(|(table_name, column_name, name, code, match_kind)| ValueMap {
            table_name,
            column_name,
            name,
            code,
            match_kind,
        })
        .collect())
}

/// 问句里**现场给出的口径**（`X 的意思是 Y`）→ 一条本轮临时 `TermDef`。
///
/// 🔴 为什么必须有它：业主原话是「查看下京东和顺丰的大日期商品，**大日期的意思是失效日期小于3月的**」
/// —— 他已经把口径讲清楚了，而系统还是去知识库问「有没有大日期的查询方法」。
/// 意图合同 `IntentV1` 里根本没有承载临时口径的字段，模型就算读懂了也无处安放。
///
/// 走的是**已有那条管道**，不造第二套：产出直接当 `TermDef` 交给 `recall_terms`
/// （注入 prompt 的 DomainTerms 段）与 `recall_term_mapped`（拿定义当新问句再召回真表）。
/// 于是「失效日期」三个字会把 `scm_warehous_manage.invalid_date` 那张表拉进上下文 ——
/// 这一步正是没有它就断掉的一环。
///
/// 覆盖面（同一条路，不是一个 case）：
///   「我们说的活跃客户是指近30天下过单的客户」
///   「所谓大客户就是年销售额超过100万的」
///   「毛利率定义为 (收入-成本)/收入」
///   「财年指的是4月到次年3月」
///
/// **保守优先**：宁可漏抽，不可错抽 —— 错抽一条会把用户没说过的口径塞进 prompt，
/// 那是凭空造口径，比不抽坏得多。所以术语名有长度与字符白名单，定义要够长，
/// 且只认「显式下定义」的几个连接词（`就是` 单独出现不认 —— 「这就是我要的」会中招，
/// 只在 `所谓…就是` 这种带锚点的形态里认）。
pub fn extract_inline_terms(question: &str) -> Vec<TermDef> {
    // 连接词按**长的在前**：`的意思就是` 必须先于 `的意思是` 试，否则术语名会多带一截。
    const SEPS: &[&str] = &["的意思就是", "的意思是", "指的是", "是指的", "是指", "定义为", "就是指"];
    // 术语名到此为止（句读、引号、空白）。左扫到最近的一个即停。
    const LEFT_STOP: &[char] = &[
        '，', '。', '；', '、', '！', '？', '：', '\n', '\r', '\t', ' ', '　',
        '「', '」', '“', '”', '(', ')', '（', '）', ',', '.', ';', '!', '?', ':',
    ];
    // 定义到此为止。句末标点必停；**逗号也停** —— 不停的代价是定义吃进后半句问题本身
    // （「毛利率定义为 (收入-成本)/收入，按这个算本月毛利率」会把「按这个算…」也当成口径），
    // 那段文本随后被当作召回问句用，等于往上下文里灌噪音。
    const RIGHT_STOP: &[char] = &['。', '；', '！', '？', '\n', '\r', '，', ','];
    // 唯一的例外：逗号后紧跟连接词时是**同一条定义的下半句**（「A，且 B」），继续吃。
    // 顿号不在停止符里，所以「A、B、C」这种并列本来就完整。
    const COMMA_CONT: &[&str] = &["且", "并且", "并", "以及", "或者", "或", "而且", "同时", "还要", "还需"];
    // 这些做不了术语名：代词/指示词，抽出来就是噪音。
    const NOT_A_TERM: &[&str] = &["这", "那", "它", "他", "她", "我", "你", "您", "其", "此", "该"];

    let mut out: Vec<TermDef> = vec![];
    let chars: Vec<char> = question.chars().collect();
    let mut at = 0usize;
    while at < chars.len() {
        // 找当前位置之后最早出现的连接词（同起点时取最长的那个）
        let mut hit: Option<(usize, &str)> = None;
        for sep in SEPS {
            let sc: Vec<char> = sep.chars().collect();
            let mut i = at;
            while i + sc.len() <= chars.len() {
                if chars[i..i + sc.len()] == sc[..] {
                    if hit.is_none_or(|(h, hs)| i < h || (i == h && sep.len() > hs.len())) {
                        hit = Some((i, sep));
                    }
                    break;
                }
                i += 1;
            }
        }
        let Some((pos, sep)) = hit else { break };
        let sep_len = sep.chars().count();

        // 术语名：从 pos 往左扫到停止符；「所谓」这个锚点要剥掉
        let mut left = pos;
        while left > 0 && !LEFT_STOP.contains(&chars[left - 1]) {
            left -= 1;
            if pos - left >= 16 {
                break; // 超长即不像术语名，交给下面的长度闸拒掉
            }
        }
        let mut term: String = chars[left..pos].iter().collect();
        for anchor in ["所谓的", "所谓", "这里的", "我们说的", "我说的"] {
            if let Some(rest) = term.strip_prefix(anchor) {
                term = rest.to_string();
            }
        }
        let term = term.trim().trim_matches(|c: char| LEFT_STOP.contains(&c)).to_string();

        // 定义：从连接词之后到句末标点
        let mut right = pos + sep_len;
        let start = right;
        while right < chars.len() && !RIGHT_STOP.contains(&chars[right]) {
            right += 1;
        }
        // 逗号 + 连接词 = 同一条定义的下半句，跨过去接着吃（可连续多段）
        while right < chars.len() && (chars[right] == '，' || chars[right] == ',') {
            let tail: String = chars[right + 1..].iter().take(4).collect();
            let tail = tail.trim_start();
            if !COMMA_CONT.iter().any(|w| tail.starts_with(w)) {
                break;
            }
            right += 1;
            while right < chars.len() && !RIGHT_STOP.contains(&chars[right]) {
                right += 1;
            }
        }
        let definition: String = chars[start..right].iter().collect();
        // 尾巴上的「的」「的的」「那种」这类残留剥掉；口语里很常见（业主原话就是「小于3月的的」）
        let definition = definition
            .trim()
            .trim_end_matches(|c: char| c == '的' || c == '，' || c == ',' || c == '。')
            .trim()
            .to_string();

        at = right.max(pos + sep_len);

        // ——— 保守闸：任一不过就丢掉，不留半成品 ———
        let term_len = term.chars().count();
        let ok_term = (2..=12).contains(&term_len)
            && !NOT_A_TERM.contains(&term.as_str())
            && !term.chars().all(|c| c.is_ascii_digit())
            // 术语名里不许有句读/括号（左扫已挡大部分，这里兜底）
            && !term.chars().any(|c| LEFT_STOP.contains(&c));
        let ok_def = definition.chars().count() >= 3;
        if !ok_term || !ok_def {
            continue;
        }
        // 同一术语只取**第一次**定义：后面再出现按重复处理，不覆盖
        if out.iter().any(|t| t.term == term) {
            continue;
        }
        out.push(TermDef { term, definition, aliases: vec![] });
    }
    out
}

#[cfg(test)]
mod tests {

    /// 问句内现场口径的抽取（`extract_inline_terms`）。
    ///
    /// 正例是**业主原话**（2026-08-17）：那句话里口径讲得清清楚楚，而系统去知识库
    /// 问「有没有大日期的查询方法」—— 抽不出来这一条，后面整条链都无从谈起。
    /// 反例比正例重要：**错抽一条＝凭空造口径塞进 prompt**，比不抽坏得多。
    #[test]
    fn inline_terms_are_extracted_conservatively() {
        // ① 业主原话，逐字（注意「是」后有空格、尾巴是「的的」）
        let got = extract_inline_terms("你需要你查看下 京东 和 顺丰 的大日期 商品，大日期的意思是 失效日期 小于3月的的");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].term, "大日期");
        assert_eq!(got[0].definition, "失效日期 小于3月", "尾部的「的的」要剥掉：{got:?}");

        // ② 同一条路要能覆盖的其它说法 —— 这是它值不值得存在的判据
        for (q, term, def) in [
            ("我们说的活跃客户是指近30天下过单的客户", "活跃客户", "近30天下过单的客户"),
            ("所谓大客户就是指年销售额超过100万的", "大客户", "年销售额超过100万"),
            ("毛利率定义为 (收入-成本)/收入，按这个算本月毛利率", "毛利率", "(收入-成本)/收入"),
            ("财年指的是4月到次年3月，本财年销售额多少", "财年", "4月到次年3月"),
        ] {
            let got = extract_inline_terms(q);
            assert_eq!(got.len(), 1, "「{q}」应抽出恰好一条：{got:?}");
            assert_eq!(got[0].term, term, "{q}");
            assert_eq!(got[0].definition, def, "{q}");
        }

        // ②b 逗号断句，但「A，且 B」是同一条定义的下半句 —— 两种都要对
        let cut = extract_inline_terms("毛利率定义为 (收入-成本)/收入，按这个算本月毛利率");
        assert_eq!(cut[0].definition, "(收入-成本)/收入", "逗号后是新话题，必须断开：{cut:?}");
        let cont = extract_inline_terms("活跃客户是指近30天下过单，且金额超1万的客户");
        assert_eq!(cont[0].definition, "近30天下过单，且金额超1万的客户", "「，且」是下半句，不许断：{cont:?}");

        // ③ 一句话里两条定义都要抽到
        let two = extract_inline_terms("活跃客户是指近30天下过单的；大客户是指年销售额超100万的");
        assert_eq!(two.len(), 2, "{two:?}");

        // ④ 🔴 反例：这些**一条都不许抽**。抽出来就是把用户没说过的口径塞进 prompt。
        for q in [
            "这就是我要的报表",            // 裸「就是」不认
            "本月销售额是多少",            // 没有下定义
            "他是指导老师",                // 「是指」在词内，术语侧是代词
            "那的意思是什么",              // 术语侧是指示代词
            "1的意思是2",                  // 纯数字不是术语
            "京东仓和顺丰仓的库存分别是多少", // 完全没有定义句式
        ] {
            assert!(extract_inline_terms(q).is_empty(), "「{q}」不该抽出口径：{:?}", extract_inline_terms(q));
        }

        // ⑤ 同名重复只取第一次（后面的不覆盖，避免一句话里自相矛盾时行为不定）
        let dup = extract_inline_terms("大客户是指年销售额超100万的；大客户是指年销售额超200万的");
        assert_eq!(dup.len(), 1, "{dup:?}");
        assert!(dup[0].definition.contains("100万"), "{dup:?}");
    }
    use super::*;

    #[test]
    fn value_domain_hit_takes_the_longest() {
        // 真库分类名里「手抓饼」与「手抓饼卷」并存：短名先中会把别的分类算进来
        let cats = ["手抓饼", "手抓饼卷", "烤肠"];
        assert_eq!(
            longest_value_hit("2026年6月手抓饼卷这个分类卖了多少箱", cats),
            Some("手抓饼卷")
        );
        assert_eq!(longest_value_hit("2026年6月手抓饼这个分类卖了多少箱", cats), Some("手抓饼"));
        assert_eq!(longest_value_hit("本月销售额", cats), None);
        // 单字取值不算（否则「饼」命中一切）
        assert_eq!(longest_value_hit("手抓饼卖了多少", ["饼"]), None);
    }
}
