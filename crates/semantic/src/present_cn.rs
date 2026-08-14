//! 结果呈现中文化：列名改中文 + 码值翻名（业主痛点：单据详情/结果表格里字段名全是英文，
//! 状态 100/101、省份区划码原样输出）。
//!
//! 纯函数全在本文件，DB 只有两条 `meta.*` 加载（ds 谓词沿用 `registry::DS_PRED` 纪律，
//! drift.rs 两条守卫盯着）。加载结果按 ds 进 TTL 缓存 —— 翻译是一次内存映射，零额外查询。
//!
//! ## 判据顺序即行为（列名）
//! ① 列名含中文（SQL 里 `AS \`销售额\`` 的别名）→ 一个字不动；
//! ② `meta.column_doc` 有中文注释（`COALESCE(NULLIF(custom_comment,''), col_comment)`，
//!    人工优先在 SQL 里完成，与 `recall::schema` 同一句）→ 用注释，超长按 [`core_comment`] 截到 ≤8 字；
//! ③ 没注释 → 通用转译表 [`GENERIC_COL_NAMES`] → snake_case 词元词典 [`TOKEN_DICT]
//!   （**全部**词元都译得出才拼接，有一个译不出就保留英文 —— 半吊子翻译比英文更误导）。
//!
//! ## 码值翻译（值）
//! 某列在 `meta.value_map` 登记过（按 结果 SQL 涉及的表 → ds 级唯一登记 两级查找）且
//! 单元格命中 code → 显示中文名。原始码不丢：调用方把 `(列, 码, 名)` 收进 `value_labels` 带出。
//! `like` 列（多值串，如 `paid_way`）按分隔符逐词元翻。省份区划码列没登记时走
//! [`crate::present::PROVINCE_LABELS`] 词表兜底；没映射就原样。
//!
//! 表归属**不发明血缘**：结果 SQL 实表由 `kernel::sql::ast::table_names_of` 给出
//! （`insight.rs` 口径说明同款），列名在各表间撞名时按「涉及表优先、ds 级唯一登记兜底」。

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use sqlx::PgPool;

use dms_kernel::present::{Block, ViewSpec};
use dms_kernel::sql::ast;
use dms_kernel::MysqlDialect;

/// 快照有效期：词表是运营侧低频维护数据，5 分钟内的陈旧可接受（改注释立即生效不是需求）。
const SNAP_TTL: Duration = Duration::from_secs(300);

/// 展示名上限（汉字字符数）。超长注释按 [`core_comment`] 去废话后截断。
const LABEL_MAX_CHARS: usize = 8;

// ─────────────────────────── 通用转译表与词元词典（任务 1 ③） ───────────────────────────

/// 常见列名的整名转译（**整名精确命中**才用，优先级高于词元拼接）。
/// 只收高频且无歧义的；拿不准的不收 —— 译错比不译更坏。
const GENERIC_COL_NAMES: &[(&str, &str)] = &[
    ("order_time", "下单时间"),
    ("order_date", "订单日期"),
    ("order_no", "订单编号"),
    ("order_status", "订单状态"),
    ("customer_code", "客户编码"),
    ("customer_name", "客户名称"),
    ("total_amount", "订单金额"),
    ("qty", "数量"),
    ("price", "单价"),
    ("amount", "金额"),
    ("status", "状态"),
    ("goods_code", "商品编码"),
    ("goods_name", "商品名称"),
    ("sku_code", "SKU编码"),
    ("sku_name", "SKU名称"),
    ("sales_order_code", "销售单号"),
    ("created_time", "创建时间"),
    ("create_time", "创建时间"),
    ("updated_time", "更新时间"),
    ("update_time", "更新时间"),
    ("deleted_flag", "删除标志"),
    ("remark", "备注"),
    ("province", "省份"),
    ("city", "城市"),
    ("district", "区县"),
    ("address", "地址"),
    ("shop_code", "门店编码"),
    ("shop_name", "门店名称"),
    ("store_name", "门店名称"),
    ("employee_code", "员工编码"),
    ("employee_name", "员工姓名"),
    ("data_month", "月份"),
    ("phone", "电话"),
    ("mobile", "手机号"),
];

/// 销售订单状态码表 —— **全仓唯一事实源**，形状对齐 `seed_defs` 的 `(中文名, 码)`。
///
/// 此前它有两份且都不完整：`fastpath::template::sales_status_sql` 里一段 16 臂 `CASE`
/// （只有「SQL 自己翻译」的那条路能出中文），`meta.value_map` 种子里只播了 3 档
/// （0/108/199）。生产轻点查按物理列取数、不经过那段 SQL，命中「已登记但无此码」
/// 直接原样输出 —— 业主截图里单据卡上那个裸 `101` 就是这么来的。
///
/// 这 16 档不是臆造：它们是生产 SQL 里跑了很久的那段 `CASE` 的逐字搬运。
/// 播进 `meta.value_map` 后**两个方向**同时通了 —— 展示侧码→名，问句侧名→码
/// （「待备货的订单」这类问法此前换不出码）。
pub const SALES_ORDER_STATUS: &[(&str, &str)] = &[
    ("暂存", "0"),
    ("未支付", "100"),
    ("待备货", "101"),
    ("备货中", "102"),
    ("等待配送", "103"),
    ("交易完成", "104"),
    ("待核销", "105"),
    ("售后中", "106"),
    ("已退款", "107"),
    ("已取消", "108"),
    ("部分收货", "109"),
    ("待收货", "110"),
    ("部分发货", "111"),
    ("取消中", "150"),
    ("取消失败-退款失败", "151"),
    ("已删除", "199"),
];

/// 客户分类码表（`t_customer.customer_class`）。形状同 [`SALES_ORDER_STATUS`]：`(中文名, 码)`。
///
/// 与 order_status 修前完全同型 —— 此前这 7 档在 `agent::answerers::entity` 与
/// `semantic::seed_defs` 各嵌一份 SQL `CASE`，于是点查路以外的展示、以及问句侧
/// 「货架店铺的客户」名→码，全都拿不到。
pub const CUSTOMER_CLASS: &[(&str, &str)] = &[
    ("货架店铺", "01"),
    ("新媒体店铺", "02"),
    ("社团店铺", "03"),
    ("线下客户", "04"),
    ("内部客户", "05"),
    ("其他财务专用", "06"),
    ("外部客户的店铺", "99"),
];

/// 客户类型码表（`t_customer.customer_type`）。
pub const CUSTOMER_TYPE: &[(&str, &str)] = &[
    ("一般销售客户", "Z001"),
    ("财务专用客户", "Z002"),
    ("关联方客户", "Z003"),
    ("货架店铺", "Z004"),
    ("客户终端仓", "Z005"),
];

/// 商品上架状态（`t_goods.on_sale`）。
pub const GOODS_ON_SALE: &[(&str, &str)] = &[("已上架", "1"), ("未上架", "0")];

/// 商品冻结状态（`t_goods.frozen_state`）。
pub const GOODS_FROZEN: &[(&str, &str)] = &[("已冻结", "1"), ("正常", "0")];

/// snake_case 词元 → 中文。**全部词元命中才拼接**（见文件头 ③）。
const TOKEN_DICT: &[(&str, &str)] = &[
    ("order", "订单"),
    ("sale", "销售"),
    ("sales", "销售"),
    ("time", "时间"),
    ("date", "日期"),
    // 生产实测补的（2026-08-14 账余充值单卡上 `receipt_date` 仍是英文，
    // 而 `t_customer_balance` 那几列在 `meta.column_doc` 里没有中文注释）
    ("receipt", "收款"),
    ("pay", "支付"),
    ("month", "月份"),
    ("year", "年份"),
    ("day", "日"),
    ("customer", "客户"),
    ("client", "客户"),
    ("goods", "商品"),
    ("product", "商品"),
    ("sku", "SKU"),
    ("code", "编码"),
    ("name", "名称"),
    ("id", "ID"),
    ("no", "编号"),
    ("num", "数量"),
    ("qty", "数量"),
    ("quantity", "数量"),
    ("count", "数量"),
    ("amount", "金额"),
    ("money", "金额"),
    ("price", "单价"),
    ("cost", "成本"),
    ("fee", "费用"),
    ("total", "合计"),
    ("balance", "余额"),
    ("discount", "折扣"),
    ("status", "状态"),
    ("state", "状态"),
    ("type", "类型"),
    ("category", "分类"),
    ("brand", "品牌"),
    ("province", "省份"),
    ("city", "城市"),
    ("region", "区域"),
    ("district", "区县"),
    ("address", "地址"),
    ("phone", "电话"),
    ("mobile", "手机"),
    ("remark", "备注"),
    ("memo", "备注"),
    ("desc", "说明"),
    ("user", "用户"),
    ("employee", "员工"),
    ("staff", "员工"),
    ("shop", "门店"),
    ("store", "门店"),
    ("warehouse", "仓库"),
    ("stock", "库存"),
    ("flag", "标志"),
    ("operator", "操作人"),
    ("creator", "创建人"),
    ("dept", "部门"),
    ("channel", "渠道"),
    ("level", "等级"),
    ("unit", "单位"),
    ("spec", "规格"),
    ("model", "型号"),
    ("batch", "批次"),
    ("tax", "税"),
    ("rate", "比率"),
];

/// 注释里的废话词（任务 1 ②）：剥掉再截断。「的」按子串剥 —— 「订单的下单时间」→「订单下单时间」。
const COMMENT_FILLERS: &[&str] = &["用于", "表示", "字段", "的"];

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// 注释 → 展示名（≤ [`LABEL_MAX_CHARS`] 字）：取首段（括号/冒号/逗号等之后都是补充说明）→
/// 剥废话词 → 按**字符**截（按字节截会把中文切成半个字 —— 本仓踩过三次）。
fn core_comment(raw: &str) -> String {
    let head = raw
        .split(|c| matches!(c, '(' | '（' | ':' | '：' | ',' | '，' | ';' | '；' | '、' | '/' | '\\'))
        .next()
        .unwrap_or("")
        .trim();
    let mut s = head.to_string();
    for filler in COMMENT_FILLERS {
        s = s.replace(filler, "");
    }
    s.trim().chars().take(LABEL_MAX_CHARS).collect()
}

/// 整名转译表命中（大小写不敏感）。
fn generic_name(raw: &str) -> Option<&'static str> {
    GENERIC_COL_NAMES
        .iter()
        .find(|(en, _)| en.eq_ignore_ascii_case(raw))
        .map(|(_, zh)| *zh)
}

/// 词元逐词转译：**所有**词元都在词典里才返回拼接结果，否则 `None`（保留英文）。
fn token_name(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut n = 0usize;
    for tok in raw.split('_').filter(|t| !t.is_empty()) {
        let zh = TOKEN_DICT.iter().find(|(en, _)| en.eq_ignore_ascii_case(tok))?.1;
        out.push_str(zh);
        n += 1;
    }
    (n > 0).then_some(out)
}

// ─────────────────────────── 快照：column_doc + value_map 的 ds 级缓存 ───────────────────────────

/// 一条码值（`meta.value_map` 行）。`like` = 多值串列，须按分隔符逐词元翻（同 `ValueRef` 的语义）。
#[derive(Clone, Debug, PartialEq)]
struct CodeEntry {
    code: String,
    name: String,
    like: bool,
}

/// 一个 ds 的词表快照（缓存单元）。键全部小写化（MySQL 列名大小写随系统设置漂，查找侧统一小写）。
#[derive(Clone, Default)]
struct Snap {
    /// (表, 列) → 注释原文（custom_comment 优先已在 SQL 里完成）
    comments: HashMap<(String, String), String>,
    /// (表, 列) → 码值项
    values: HashMap<(String, String), Vec<CodeEntry>>,
    /// 列名 → 注释：**全 ds 只有一个不同注释**才收录（撞名注释互不相同 = 歧义，不猜）
    uniq_comments: HashMap<String, String>,
    /// 列名 → 码值项：**全 ds 恰好一张表**登记该列才收录（多表登记同名列 = 歧义，不猜）
    uniq_values: HashMap<String, Vec<CodeEntry>>,
}

struct TimedSnap {
    at: Instant,
    snap: Snap,
}

static SNAPS: OnceLock<Mutex<HashMap<String, TimedSnap>>> = OnceLock::new();

fn snaps() -> &'static Mutex<HashMap<String, TimedSnap>> {
    SNAPS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 原始行 → 快照（**纯函数**，uniq 两级索引在这里建成；IO 那半只是喂行）。
fn build_snap(
    comments: Vec<(String, String, String)>,
    values: Vec<(String, String, String, String, String)>,
) -> Snap {
    let mut snap = Snap::default();
    // 列名 → 不同注释集合（判唯一用）
    let mut comment_variants: HashMap<String, HashSet<String>> = HashMap::new();
    // 列名 → 登记表集合（判唯一用）
    let mut value_tables: HashMap<String, HashSet<String>> = HashMap::new();
    for (table, col, cmt) in comments {
        let (table, col) = (table.to_ascii_lowercase(), col.to_ascii_lowercase());
        let cmt = cmt.trim().to_string();
        if cmt.is_empty() {
            continue;
        }
        comment_variants.entry(col.clone()).or_default().insert(cmt.clone());
        snap.comments.insert((table, col), cmt);
    }
    for (table, col, code, name, match_kind) in values {
        let (table, col) = (table.to_ascii_lowercase(), col.to_ascii_lowercase());
        if code.is_empty() || name.is_empty() {
            continue;
        }
        value_tables.entry(col.clone()).or_default().insert(table.clone());
        snap.values.entry((table, col)).or_default().push(CodeEntry {
            code,
            name,
            like: match_kind.eq_ignore_ascii_case("like"),
        });
    }
    for (col, variants) in comment_variants {
        if variants.len() == 1 {
            snap.uniq_comments.insert(col, variants.into_iter().next().unwrap());
        }
    }
    for (col, tables) in value_tables {
        if tables.len() == 1 {
            let table = tables.into_iter().next().unwrap();
            if let Some(entries) = snap.values.get(&(table, col.clone())) {
                snap.uniq_values.insert(col, entries.clone());
            }
        }
    }
    snap
}

/// 加载一个 ds 的词表快照：缓存命中（未过期）零查询；未命中两条 `meta.*` 查询。
/// **失败不缓存**（下次重试），有旧快照时沿用旧的（陈旧好过没有）。
async fn load_snap(pg: &PgPool, ds: &str) -> Snap {
    {
        let cache = snaps().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = cache.get(ds) {
            if t.at.elapsed() < SNAP_TTL {
                return t.snap.clone();
            }
        }
    }
    match fetch_snap(pg, ds).await {
        Ok(snap) => {
            let mut cache = snaps().lock().unwrap_or_else(|p| p.into_inner());
            cache.insert(ds.to_string(), TimedSnap { at: Instant::now(), snap: snap.clone() });
            snap
        }
        Err(e) => {
            let stale = {
                let cache = snaps().lock().unwrap_or_else(|p| p.into_inner());
                cache.get(ds).map(|t| t.snap.clone())
            };
            match stale {
                Some(s) => {
                    tracing::warn!(err = %e, ds, "呈现词表刷新失败 → 沿用旧快照");
                    s
                }
                None => {
                    tracing::warn!(err = %e, ds, "呈现词表加载失败 → 本轮不翻译（结果原样返回）");
                    Snap::default()
                }
            }
        }
    }
}

/// 两条加载 SQL。ds 谓词沿用 `DS_PRED`；value_map 与 `registry::model::load_value_map`
/// 同样拼「存活表」谓词（组合进名为 `ds_pred` 的变量 —— drift.rs 白名单只认这个插值名）。
async fn fetch_snap(pg: &PgPool, ds: &str) -> anyhow::Result<Snap> {
    let ds_pred = crate::registry::ds_pred(1);
    let comment_rows: Vec<(String, String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT table_name, column_name, COALESCE(NULLIF(custom_comment, ''), col_comment)
         FROM meta.column_doc WHERE 1 = 1{ds_pred}"
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    let ds_pred = format!(
        "{}{}",
        crate::registry::ds_pred(1),
        crate::registry::table_asset_live_pred_at("", 1)
    );
    let value_rows: Vec<(String, String, String, String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT table_name, column_name, name, code, match_kind FROM meta.value_map
         WHERE 1 = 1{ds_pred}"
    ))
    .bind(ds)
    .fetch_all(pg)
    .await?;
    Ok(build_snap(
        comment_rows
            .into_iter()
            .filter_map(|(t, c, cmt)| cmt.map(|c2| (t, c, c2)))
            .collect(),
        value_rows
            .into_iter()
            // 与 `load_value_map` 同一道目录闸：不在目录里的表不进词表
            .filter(|(table, ..)| crate::registry::catalog_allows_table(ds, table))
            .map(|(t, c, name, code, kind)| (t, c, code, name, kind.unwrap_or_default()))
            .collect(),
    ))
}

/// 结果 SQL 涉及的实表 → 裸表名（小写、去库名前缀）。解析失败 = 空（只剩 ds 级唯一兜底可查）。
fn tables_of(sql: &str) -> Vec<String> {
    ast::table_names_of(sql, &MysqlDialect)
        .unwrap_or_default()
        .iter()
        .map(|t| {
            t.rsplit('.')
                .next()
                .unwrap_or(t)
                .trim_matches('`')
                .to_ascii_lowercase()
        })
        .collect()
}

// ─────────────────────────── PresentCn：一次结果的翻译上下文 ───────────────────────────

/// 一次结果呈现的中文化上下文：涉及表 + 该 ds 的词表快照。
/// 改名/翻译全是内存查找；构造失败（词表加载挂）= 空快照 = 一切原样。
pub struct PresentCn {
    tables: Vec<String>,
    snap: Snap,
}

impl PresentCn {
    /// 生产入口：解析 SQL 实表 + 取（或加载）该 ds 的快照。永不失败 —— 加载挂 = 空快照。
    pub async fn load(pg: &PgPool, ds: &str, sql: &str) -> Self {
        PresentCn { tables: tables_of(sql), snap: load_snap(pg, ds).await }
    }

    /// 纯内存构造（单测与 agent 侧判据用；生产走 [`PresentCn::load`]）。
    /// `comments` = (表, 列, 注释)；`values` = (表, 列, 码, 名, match_kind)。
    pub fn from_parts(
        tables: &[&str],
        comments: Vec<(&str, &str, &str)>,
        values: Vec<(&str, &str, &str, &str, &str)>,
    ) -> Self {
        PresentCn {
            tables: tables.iter().map(|t| t.to_ascii_lowercase()).collect(),
            snap: build_snap(
                comments
                    .into_iter()
                    .map(|(t, c, m)| (t.to_string(), c.to_string(), m.to_string()))
                    .collect(),
                values
                    .into_iter()
                    .map(|(t, c, code, name, kind)| {
                        (t.to_string(), c.to_string(), code.to_string(), name.to_string(), kind.to_string())
                    })
                    .collect(),
            ),
        }
    }

    /// 列的注释查找：涉及表优先（按表名序），撞名歧义时落 ds 级唯一注释。
    fn comment_of(&self, col: &str) -> Option<&str> {
        let key = col.to_ascii_lowercase();
        self.tables
            .iter()
            .find_map(|t| self.snap.comments.get(&(t.clone(), key.clone())))
            .or_else(|| self.snap.uniq_comments.get(&key))
            .map(String::as_str)
    }

    /// 列的码值登记查找：同一两级（涉及表 → ds 级唯一登记）。
    fn entries_of(&self, col: &str) -> Option<&[CodeEntry]> {
        let key = col.to_ascii_lowercase();
        self.tables
            .iter()
            .find_map(|t| self.snap.values.get(&(t.clone(), key.clone())))
            .or_else(|| self.snap.uniq_values.get(&key))
            .map(Vec::as_slice)
    }

    /// 任务 1：英文列名 → 中文展示名。`None` = 不动（中文别名/译不出）。
    pub fn rename_column(&self, raw: &str) -> Option<String> {
        // ① 含中文（SQL 里起好的中文别名）一律不动
        if raw.chars().any(is_cjk) {
            return None;
        }
        // ② column_doc 中文注释（人工优先已在 SQL 里）；注释不是中文（英文注释）不算，落 ③
        if let Some(cmt) = self.comment_of(raw) {
            let core = core_comment(cmt);
            if !core.is_empty() && core.chars().any(is_cjk) {
                return Some(core);
            }
        }
        // ③ 通用转译表 → 词元词典
        generic_name(raw).map(str::to_string).or_else(|| token_name(raw))
    }

    /// 任务 2：码值单元格 → 中文名。`None` = 原样（未登记/未命中/不是文本数值格）。
    pub fn translate_cell(&self, col_raw: &str, v: &Value) -> Option<String> {
        let s = cell_text(v)?;
        if let Some(entries) = self.entries_of(col_raw) {
            // eq 命中（like 项也先按整格精确试一次：单值串与 eq 同形态）
            if let Some(e) = entries.iter().find(|e| e.code == s) {
                return Some(e.name.clone());
            }
            // like 多值串：按分隔符逐词元翻，一个都没中才算未命中
            if entries.iter().any(|e| e.like) {
                return translate_like(&s, entries);
            }
            // 登记过的码值列没命中 → 原样（不再落省份兜底：登记了就不是区划码语义）
            return None;
        }
        // 省份区划码兜底：列名像省份 + 值恰是 34 省码之一
        if looks_like_province_col(col_raw) {
            return crate::present::PROVINCE_LABELS
                .iter()
                .find(|(c, _)| *c == s)
                .map(|(_, n)| (*n).to_string());
        }
        None
    }

    /// 把改名与翻译应用到一个结果块（主结果或 supplemental 共用）。
    /// 返回 `(显示列名, 原始码, 中文名)` 留痕（去重由调用方合并时做）。
    ///
    /// `view` 与 `columns` 的对齐校验：只有 `view.columns` 与改名前逐字一致才按下标同步
    /// （`table_answer`/`business_lookup`/`entity` 都是这个形态）；不一致就跳过 view 列名，
    /// 绝不按下标瞎改。
    pub fn apply(
        &self,
        columns: &mut Vec<String>,
        rows: &mut [Vec<Value>],
        view: &mut ViewSpec,
        redacted: &mut Vec<String>,
    ) -> Vec<(String, String, String)> {
        let raw = columns.clone();
        // ── 列名：改名 + 防撞（与既有列名或已定名撞了就保留英文，两张同名表头比英文更坏）──
        let mut taken: HashSet<String> = raw.iter().filter(|n| n.chars().any(is_cjk)).cloned().collect();
        let mut finals: Vec<String> = Vec::with_capacity(raw.len());
        let mut renamed: Vec<Option<String>> = Vec::with_capacity(raw.len());
        for name in &raw {
            let candidate = self.rename_column(name).filter(|n| !taken.contains(n) && !raw.contains(n));
            match &candidate {
                Some(n) => {
                    taken.insert(n.clone());
                }
                None => {
                    taken.insert(name.clone());
                }
            }
            finals.push(candidate.clone().unwrap_or_else(|| name.clone()));
            renamed.push(candidate);
        }
        // view 列名按下标同步（对齐校验见函数头）
        if view.columns.len() == raw.len()
            && view.columns.iter().zip(raw.iter()).all(|(c, r)| c.name == *r)
        {
            for (spec, name) in view.columns.iter_mut().zip(finals.iter()) {
                spec.name = name.clone();
            }
        }
        *columns = finals.clone();
        // 脱敏列名与 columns 逐字同源（前端按字符串相等定位列）
        for r in redacted.iter_mut() {
            if let Some(n) = self.rename_column(r) {
                if finals.iter().any(|f| f == &n) {
                    *r = n;
                }
            }
        }
        // ── 值：按**原列名**查登记（改名不影响翻译判定），翻译后收集留痕 ──
        let mut labels: Vec<(String, String, String)> = vec![];
        for (i, raw_name) in raw.iter().enumerate() {
            for row in rows.iter_mut() {
                let Some(cell) = row.get_mut(i) else { continue };
                if let Some(zh) = self.translate_cell(raw_name, cell) {
                    if let Some(code) = cell_text(cell) {
                        labels.push((finals[i].clone(), code, zh.clone()));
                    }
                    *cell = Value::String(zh);
                }
            }
        }
        // KPI 卡与实体卡里的列名/值是 columns/rows 的副本，同步改（图表块只有下标，无副本）
        let rename_of = |name: &str| -> Option<String> {
            raw.iter()
                .zip(renamed.iter())
                .find(|(r, _)| r.as_str() == name)
                .and_then(|(_, n)| n.clone())
                .or_else(|| self.rename_column(name))
        };
        for block in view.blocks.iter_mut() {
            match block {
                Block::Kpis { items } => {
                    for item in items.iter_mut() {
                        if let Some(n) = rename_of(&item.label) {
                            item.label = n;
                        }
                    }
                }
                Block::Entity { pairs } => {
                    let mut pair_names: HashSet<String> = pairs.iter().map(|(k, _)| k.clone()).collect();
                    for (key, value) in pairs.iter_mut() {
                        // 翻译判定用**改名前**的原列名（中文展示名查不到登记）
                        let orig_key = key.clone();
                        if let Some(n) = rename_of(&orig_key) {
                            if !pair_names.contains(&n) {
                                pair_names.remove(orig_key.as_str());
                                pair_names.insert(n.clone());
                                *key = n;
                            }
                        }
                        // 实体卡的值（单据头的字段值）同样翻码值
                        if let Some(zh) = self.translate_cell(&orig_key, value) {
                            if let Some(code) = cell_text(value) {
                                labels.push((key.clone(), code, zh.clone()));
                            }
                            *value = Value::String(zh);
                        }
                    }
                }
                _ => {}
            }
        }
        labels
    }
}

/// 单元格 → 可比较的文本（字符串去空白；数字归一化 `100.0` → `100`，与码表写法对齐）。
fn cell_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        Value::Number(n) => {
            let s = n.to_string();
            Some(s.strip_suffix(".0").map(str::to_string).unwrap_or(s))
        }
        _ => None,
    }
}

/// like 多值串翻译：`,` `，` `、` `/` `|` 空格 分隔的词元逐个对码表，中的翻、不中的原样。
fn translate_like(s: &str, entries: &[CodeEntry]) -> Option<String> {
    let mut out = String::new();
    let mut token = String::new();
    let mut hit = false;
    let flush = |token: &mut String, out: &mut String, hit: &mut bool| {
        match entries.iter().find(|e| e.code == *token) {
            Some(e) => {
                out.push_str(&e.name);
                *hit = true;
            }
            None => out.push_str(token),
        }
        token.clear();
    };
    for ch in s.chars() {
        if matches!(ch, ',' | '，' | '、' | '/' | '|' | ' ') {
            flush(&mut token, &mut out, &mut hit);
            out.push(ch);
        } else {
            token.push(ch);
        }
    }
    flush(&mut token, &mut out, &mut hit);
    hit.then_some(out)
}



/// 省份区划码列的列名判据（值那半是 34 省码精确命中，两道闸一起才翻）。
fn looks_like_province_col(col: &str) -> bool {
    let l = col.to_ascii_lowercase();
    l.contains("province") || col.contains('省')
}

#[cfg(test)]
mod tests {
    use super::*;
    use dms_kernel::present::{ColumnSpec, Interact, Kpi, Role, Semantic};
    use serde_json::json;

    fn cn() -> PresentCn {
        PresentCn::from_parts(
            &["t_sales_order", "t_sales_order_detail"],
            vec![
                ("t_sales_order", "order_time", "订单的下单时间"),
                ("t_sales_order", "total_amount", "订单总金额（含税，用于对账）"),
                ("t_sales_order", "custom_col", "custom name"), // 英文注释 → 落 ③
                ("t_sales_order_detail", "qty", "数量"),
            ],
            vec![
                ("t_sales_order", "status", "100", "待审核", "eq"),
                ("t_sales_order", "status", "101", "已审核", "eq"),
                ("t_sales_order", "paid_way", "1", "微信", "like"),
                ("t_sales_order", "paid_way", "2", "支付宝", "like"),
            ],
        )
    }

    // ─────────── 任务 1：列名中文化 ───────────

    /// ① 中文别名一律不动；② 注释优先（人工优先在 SQL COALESCE，这里是喂进来的行）；
    /// ③ 通用表 → 词元词典 → 译不出保留英文。
    #[test]
    fn rename_rules_in_order() {
        let cn = cn();
        // ① 含中文不动
        assert_eq!(cn.rename_column("销售额"), None);
        assert_eq!(cn.rename_column("客户编码"), None);
        // ② 注释命中（去「的」后 ≤8 字）
        assert_eq!(cn.rename_column("order_time").as_deref(), Some("订单下单时间"));
        // ② 超长注释：取首段 + 剥废话 + ≤8 字
        assert_eq!(cn.rename_column("total_amount").as_deref(), Some("订单总金额"));
        // 英文注释不算中文注释 → 落 ③（custom_col 词元译不出 → None）
        assert_eq!(cn.rename_column("custom_col"), None);
        // ③ 涉及表外的列也能用注释（detail 在涉及表里）
        assert_eq!(cn.rename_column("qty").as_deref(), Some("数量"));
        // ③ 通用转译表
        assert_eq!(cn.rename_column("customer_code").as_deref(), Some("客户编码"));
        // ③ 词元拼接
        assert_eq!(cn.rename_column("sales_qty").as_deref(), Some("销售数量"));
        // 译不出 → 保留英文
        assert_eq!(cn.rename_column("ext_ref_no_2"), None);
        assert_eq!(cn.rename_column("n"), None, "单字母别名不许瞎猜");
    }

    /// 截断规则（任务 1 ②）：剥「用于/的/字段」类废话，保留核心名词，≤8 字。
    #[test]
    fn comment_core_strips_fillers_and_caps_at_eight() {
        assert_eq!(core_comment("下单时间"), "下单时间");
        assert_eq!(core_comment("订单的下单时间"), "订单下单时间");
        assert_eq!(core_comment("订单总金额（含税，用于对账）"), "订单总金额");
        assert_eq!(core_comment("用于统计的销售额字段"), "统计销售额");
        // 超 8 字截断（按字符，不切半个中文）
        let long = core_comment("客户在客户关系管理系统中的编码");
        assert!(long.chars().count() <= 8, "{long}");
        // 空注释 / 剥完为空 → 空串（调用方落 ③）
        assert_eq!(core_comment(""), "");
        assert_eq!(core_comment("（仅测试）"), "");
    }

    /// 词元词典的边界：全中才拼，一个译不出就整体保留英文。
    #[test]
    fn token_dict_all_or_nothing() {
        assert_eq!(token_name("customer_name").as_deref(), Some("客户名称"));
        assert_eq!(token_name("qty_x9"), None, "一个词元译不出就整体保留英文");
        assert_eq!(token_name("n"), None);
        assert_eq!(generic_name("qty"), Some("数量"));
        assert_eq!(generic_name("QTY"), Some("数量"), "大小写不敏感");
    }

    // ─────────── 任务 2：码值翻译 ───────────

    #[test]
    fn value_map_translation_eq_like_and_numeric() {
        let cn = cn();
        // eq 字符串命中
        assert_eq!(cn.translate_cell("status", &json!("100")).as_deref(), Some("待审核"));
        // eq 数字格（JSON Number 与码表 '100' 对齐；100.0 归一成 100）
        assert_eq!(cn.translate_cell("status", &json!(101)).as_deref(), Some("已审核"));
        assert_eq!(cn.translate_cell("status", &json!(100.0)).as_deref(), Some("待审核"));
        // 未命中 → 原样
        assert_eq!(cn.translate_cell("status", &json!("999")), None);
        // like 多值串逐词元翻
        assert_eq!(cn.translate_cell("paid_way", &json!("1,2")).as_deref(), Some("微信,支付宝"));
        assert_eq!(cn.translate_cell("paid_way", &json!("1，2")).as_deref(), Some("微信，支付宝"));
        // like 单值也翻；部分词元未登记时已中的照翻、未中的原样留在串里
        assert_eq!(cn.translate_cell("paid_way", &json!("2")).as_deref(), Some("支付宝"));
        assert_eq!(cn.translate_cell("paid_way", &json!("1,9")).as_deref(), Some("微信,9"));
        assert_eq!(cn.translate_cell("paid_way", &json!("8,9")), None, "一个都没中才算未命中");
    }

    /// 省份区划码兜底：列名像省份 + 值命中 34 省码；登记过 value_map 的列不走这条。
    #[test]
    fn province_code_falls_back_to_labels() {
        let cn = cn();
        assert_eq!(cn.translate_cell("province", &json!("110000")).as_deref(), Some("北京"));
        assert_eq!(cn.translate_cell("province_code", &json!("430000")).as_deref(), Some("湖南"));
        // 非省份列的同样值不翻（两道闸：列名 + 码值）
        assert_eq!(cn.translate_cell("customer_code", &json!("110000")), None);
        // 没映射的原样
        assert_eq!(cn.translate_cell("province", &json!("999999")), None);
        // NULL / 空串不翻
        assert_eq!(cn.translate_cell("province", &Value::Null), None);
        assert_eq!(cn.translate_cell("province", &json!("")), None);
    }

    /// ds 级唯一登记兜底：列不在涉及表的登记里、但全 ds 只有一张表登记它 → 照翻。
    #[test]
    fn unique_registration_fallback_across_tables() {
        let cn = PresentCn::from_parts(
            &["t_other"],
            vec![],
            vec![("t_sales_order", "after_sales_type", "1", "退货", "eq")],
        );
        assert_eq!(cn.translate_cell("after_sales_type", &json!("1")).as_deref(), Some("退货"));
        // 两张表登记同名列 = 歧义 → 不猜（涉及表里没有时宁可原样）
        let cn2 = PresentCn::from_parts(
            &["t_other"],
            vec![],
            vec![
                ("t_a", "flag", "1", "甲义", "eq"),
                ("t_b", "flag", "1", "乙义", "eq"),
            ],
        );
        assert_eq!(cn2.translate_cell("flag", &json!("1")), None, "撞名歧义不许猜");
    }

    // ─────────── apply：列名/值/视图/脱敏 一次同步 ───────────

    fn view_of(columns: &[String]) -> ViewSpec {
        ViewSpec {
            columns: columns
                .iter()
                .map(|c| ColumnSpec { name: c.clone(), role: Role::Category, semantic: Semantic::None })
                .collect(),
            blocks: vec![],
            interact: Interact::default(),
            insight: None,
        }
    }

    #[test]
    fn apply_renames_columns_view_and_redacted_in_sync() {
        let cn = cn();
        let mut columns = vec!["status".to_string(), "order_time".to_string(), "销售额".to_string()];
        let mut rows = vec![vec![json!("100"), json!("2026-08-01"), json!("12")]];
        let mut view = view_of(&columns);
        let mut redacted = vec!["order_time".to_string()];
        let labels = cn.apply(&mut columns, &mut rows, &mut view, &mut redacted);
        assert_eq!(columns, vec!["状态", "订单下单时间", "销售额"]);
        assert_eq!(view.columns[0].name, "状态");
        assert_eq!(view.columns[2].name, "销售额", "中文列原样");
        assert_eq!(redacted, vec!["订单下单时间"], "脱敏名必须与 columns 逐字同源");
        // 值翻译 + 留痕
        assert_eq!(rows[0][0], json!("待审核"));
        assert!(labels.contains(&("状态".to_string(), "100".to_string(), "待审核".to_string())), "{labels:?}");
    }

    /// 防撞：改名撞上既有列名/已改出的名字时保留英文。
    #[test]
    fn rename_collision_keeps_original() {
        let cn = cn();
        // 「状态」已被占 → status 保留英文
        let mut columns = vec!["状态".to_string(), "status".to_string()];
        let mut rows = vec![];
        let mut view = view_of(&columns);
        let mut redacted = vec![];
        cn.apply(&mut columns, &mut rows, &mut view, &mut redacted);
        assert_eq!(columns, vec!["状态", "status"]);
        // 两列译出同一个名字：先者得名，后者保留英文
        let cn2 = PresentCn::from_parts(&["t"], vec![], vec![]);
        let mut columns = vec!["province".to_string(), "city".to_string()];
        // province→省份（通用表），city→城市 —— 不撞；再造一对真撞的：
        let mut view = view_of(&columns);
        cn2.apply(&mut columns, &mut rows, &mut view, &mut redacted);
        assert_eq!(columns, vec!["省份", "城市"]);
        let mut dup = vec!["qty".to_string(), "quantity".to_string()];
        let mut view2 = view_of(&dup);
        cn2.apply(&mut dup, &mut rows, &mut view2, &mut redacted);
        assert_eq!(dup, vec!["数量", "quantity"], "后者撞名保留英文");
    }

    /// 实体卡/KPI 卡里的列名与值是 columns/rows 的副本，必须同步。
    #[test]
    fn entity_and_kpi_blocks_follow_the_rename() {
        let cn = cn();
        let mut columns = vec!["status".to_string()];
        let mut rows = vec![vec![json!("101")]];
        let mut view = view_of(&columns);
        view.blocks = vec![
            Block::Entity { pairs: vec![("status".into(), json!("100")), ("单号".into(), json!("X1"))] },
            Block::Kpis {
                items: vec![Kpi { label: "status".into(), value: json!(3), semantic: Semantic::Count, delta: None }],
            },
        ];
        let mut redacted = vec![];
        let labels = cn.apply(&mut columns, &mut rows, &mut view, &mut redacted);
        match &view.blocks[0] {
            Block::Entity { pairs } => {
                assert_eq!(pairs[0].0, "状态");
                assert_eq!(pairs[0].1, json!("待审核"), "实体卡的值也要翻");
                assert_eq!(pairs[1].0, "单号", "中文键不动");
            }
            _ => panic!(),
        }
        match &view.blocks[1] {
            Block::Kpis { items } => assert_eq!(items[0].label, "状态"),
            _ => panic!(),
        }
        assert!(labels.iter().any(|(c, code, _)| c == "状态" && code == "100"));
    }

    /// view 与 columns 不对齐时跳过 view 列名同步（防御：不许按下标瞎改）。
    #[test]
    fn misaligned_view_is_left_alone() {
        let cn = cn();
        let mut columns = vec!["status".to_string()];
        let mut rows = vec![];
        let mut view = view_of(&["别的".to_string()]);
        let mut redacted = vec![];
        cn.apply(&mut columns, &mut rows, &mut view, &mut redacted);
        assert_eq!(view.columns[0].name, "别的");
        assert_eq!(columns[0], "状态");
    }

    /// 空快照 = 一切原样（词表加载失败的降级形态）。
    #[test]
    fn empty_snapshot_is_a_noop() {
        let cn = PresentCn::from_parts(&["t_sales_order"], vec![], vec![]);
        // 空快照仍走 ③：通用表/词典是编译期词表，不依赖快照
        assert_eq!(cn.rename_column("customer_code").as_deref(), Some("客户编码"));
        assert_eq!(cn.rename_column("ext_x"), None);
        assert_eq!(cn.translate_cell("status", &json!("100")), None);
    }

    /// uniq 注释的歧义判据：同名列注释不一致 → 不落 ds 级兜底。
    #[test]
    fn uniq_comment_requires_single_variant() {
        let cn = PresentCn::from_parts(
            &["t_c"],
            vec![("t_a", "flag", "有效标志"), ("t_b", "flag", "删除标志")],
            vec![],
        );
        // t_c 涉及表无注释；t_a/t_b 注释互不相同 → ③ 词元：flag → 标志
        assert_eq!(cn.rename_column("flag").as_deref(), Some("标志"));
        let cn2 = PresentCn::from_parts(
            &["t_c"],
            vec![("t_a", "flag", "有效标志"), ("t_b", "flag", "有效标志")],
            vec![],
        );
        assert_eq!(cn2.rename_column("flag").as_deref(), Some("有效标志"), "全 ds 一致才用注释");
    }
}
