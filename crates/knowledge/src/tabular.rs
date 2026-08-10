//! 【K4】上传表格的**通道②**：每个 sheet → 自有 PG 的物理表（schema `up_<doc_id>`），
//! 交出 `TabularSource` 描述符供 server 登记成 datasource。变更原因＝表格物化。
//!
//! 通道①（sheet → markdown → `kb.chunk`）的渲染实现在 `ingest.rs`，本文件只出契约入口
//! `sheet_blocks` 转调它 —— 两份 markdown 渲染必然漂。
//!
//! ## 纪律
//! - **一个 sanitize 都不自己写**：schema/表名/列名全部经 `dms_connector::ddl` 的
//!   `SafeIdent` / `build_columns`（ARCHITECTURE §5：全仓只有 connector 一份）。
//!   中文表头进**列注释**，不进列名；类型走 `infer_col_type`（前导零/长数字判 Text 已在里面）。
//! - **不碰 `meta.*`**：登记数据源 + 授权上传者由 server 的 `meta::register_upload_datasource`
//!   一个函数（同一事务）做完 —— 只登记不授权 = 私有台账对全员敞开。本文件只返回描述符。
//! - 超限**报错不截断**：静默截断会让用户以为数据全了。

use crate::KbError;
use dms_connector::ddl::{build_columns, infer_col_type, ColType, SafeIdent, UploadTableSpec};
use dms_connector::doc::{Block, Sheet};
use dms_connector::owned::OwnedStore;

/// 单 sheet 上限（ARCHITECTURE §4.5）。超出一律 `BadInput`，**不截断**。
const MAX_ROWS: usize = 200_000;
const MAX_COLS: usize = 200;
/// 类型推断的取样行数：为猜类型扫 20 万行不值当，前 200 行足够代表。
const SAMPLE_ROWS: usize = 200;

#[derive(Debug, Clone)]
pub struct TabularTable {
    /// 原始 sheet 名（不可信文本，只用于展示与数据源描述）
    pub sheet: String,
    /// 落库表名（已过 `SafeIdent` 白名单）
    pub table: String,
    pub rows: usize,
}

#[derive(Debug, Clone)]
pub struct TabularSource {
    pub ds_id: String,
    pub schema: String,
    pub tables: Vec<TabularTable>,
    /// 空表 / 无表头被跳过的 sheet 名。不建零列表，但**不能静默**——
    /// 用户以为整份文件都能问数，结果少了一个 sheet，那是个安静的数据缺口。
    pub skipped: Vec<String>,
}

/// 通道①：sheet → markdown 表格块。实现复用 `ingest::sheet_block`（行上限与降级文案
/// 在那里有既存单测钉住），本函数只是 §4.5 契约里的入口名。
pub fn sheet_blocks(sheets: &[Sheet]) -> Vec<Block> {
    // 全空的 sheet 跳过：`sheet_block` 对它产出的是 `"# 名字\n\n"`——一个只有标题的垃圾块，
    // 会进 `kb.chunk` 并参与检索。它不该被丢在 Python 侧（那样 `plan` 就没法把它计入
    // `skipped`，见 `embed_service::_sheet` 的红字），而该在这里、在**文本通道**跳过。
    sheets
        .iter()
        .filter(|s| !(s.header.is_empty() && s.rows.is_empty()))
        .map(crate::ingest::sheet_block)
        .collect()
}

/// 上传源的 `ds_id` —— **唯一真相源**：删文档时 server 要拼同一个串去注销
/// （`meta::delete_datasource`），两处各拼一次就会漂成「删不掉的活数据源」。
/// `server::ds_api::valid_ds_id` 放行字母数字与 `_-`，uuid 原样即可。
pub fn upload_ds_id(doc_id: &str) -> String {
    format!("upload_{doc_id}")
}

/// `ds_id` → 该上传源的 PG schema 名；非上传源 `None`。
///
/// **建池时的 `search_path` 靠它**：上传源共用一条 `pg_ro_url`，schema 却一份一个，
/// 不设 `search_path` 则 `probe_schema()`（按 `current_schema()` 过滤）采不到任何表，
/// LLM 拿到空 schema，「上传即可问数」恒答不出来。
///
/// 与 `upload_ds_id` / `schema_ident` 是**同一个真相源**：两者都由 doc_id 派生，
/// 故从 ds_id 剥掉前缀即可还原。别在别处再拼一次 `"up_" + replace('-', "_")` ——
/// 那份副本漂了之后表现为「问数查了个不存在的 schema」。
pub fn upload_schema_of_ds(ds_id: &str) -> Option<String> {
    let doc_id = ds_id.strip_prefix("upload_")?;
    Some(schema_ident(doc_id).as_str().to_string())
}

/// schema 名 `up_<doc_id>`（uuid 的 `-` 由 sanitize 换成 `_`）。
/// 用 `sanitize` 而不是 `parse`：doc_id 虽是我们生成的 uuid，但「标识符一律过清洗」不留例外
/// —— `sanitize` 的返回值必然通过 `parse`（connector 那边有单测钉住）。
fn schema_ident(doc_id: &str) -> SafeIdent {
    SafeIdent::sanitize(&format!("up_{doc_id}"), 0)
}

/// 通道②：建 schema + 每 sheet 一张表 + 灌数据。
///
/// 顺序：**先把全部 sheet 规划完（含上限校验）再动 DDL** —— 第 3 个 sheet 超限时不该已经
/// 建好两张表。规划全失败（全是空表）时连 schema 都不建。
pub async fn materialize(
    store: &OwnedStore,
    doc_id: &str,
    sheets: &[Sheet],
) -> Result<TabularSource, KbError> {
    let schema = schema_ident(doc_id);
    let plan = plan(&schema, sheets)?;
    store.create_upload_schema(&schema).await?;
    let mut tables = Vec::with_capacity(plan.specs.len());
    for (i, spec) in &plan.specs {
        store.create_upload_table(spec).await?;
        let rows = store.insert_upload_rows(spec, &sheets[*i].rows).await?;
        tables.push(TabularTable {
            sheet: sheets[*i].name.clone(),
            table: spec.table.as_str().to_string(),
            rows,
        });
    }
    Ok(TabularSource {
        ds_id: upload_ds_id(doc_id),
        schema: schema.as_str().to_string(),
        tables,
        skipped: plan.skipped,
    })
}

/// 删文档时连带清理：`DROP SCHEMA up_<doc_id> CASCADE`。
/// 非表格文档调它是幂等 no-op（`IF EXISTS`），故调用方不必先判文件类型。
pub async fn drop_source(store: &OwnedStore, doc_id: &str) -> Result<(), KbError> {
    store.drop_upload_schema(&schema_ident(doc_id)).await?;
    Ok(())
}

/// 规划结果：`specs` 的 `usize` 是 sheet 在入参里的下标（灌数据时要取回 `rows`）。
struct Plan {
    specs: Vec<(usize, UploadTableSpec)>,
    skipped: Vec<String>,
}

/// 纯函数：跳过判定 → 上限校验 → 组装每张表的 `UploadTableSpec`。
/// 本文件的单测全打在这里（建表/灌数是 IO，归连库验收）。
fn plan(schema: &SafeIdent, sheets: &[Sheet]) -> Result<Plan, KbError> {
    let mut out = Plan { specs: Vec::new(), skipped: Vec::new() };
    for (i, s) in sheets.iter().enumerate() {
        // 无表头就无从命名列（`c0..cn` 那种表对 NL2SQL 也没有意义）→ 跳过，不建零列表
        if s.header.iter().all(|h| h.trim().is_empty()) {
            out.skipped.push(s.name.clone());
            continue;
        }
        check_limits(s)?;
        out.specs.push((i, spec_of(schema, i, s)));
    }
    if out.specs.is_empty() {
        return Err(KbError::BadInput("表格里没有可建表的 sheet（空表或无表头）".into()));
    }
    Ok(out)
}

/// 上限：**报错不截断**。文案要带 sheet 名与实际数量，否则用户不知道该拆哪个表。
fn check_limits(s: &Sheet) -> Result<(), KbError> {
    if s.rows.len() > MAX_ROWS {
        return Err(KbError::BadInput(format!(
            "sheet「{}」有 {} 行，超过上限 {MAX_ROWS} 行，请拆分后重传（不做截断）",
            s.name,
            s.rows.len()
        )));
    }
    if s.header.len() > MAX_COLS {
        return Err(KbError::BadInput(format!(
            "sheet「{}」有 {} 列，超过上限 {MAX_COLS} 列",
            s.name,
            s.header.len()
        )));
    }
    Ok(())
}

/// 表名 `t<序号>_<清洗后的 sheet 名>`：序号前缀保证两个 sheet 绝不塌成一张表
/// （中文 sheet 名清洗后都退化成同一串）。原名进 `TabularTable.sheet` 与数据源描述。
fn spec_of(schema: &SafeIdent, ord: usize, s: &Sheet) -> UploadTableSpec {
    let cols: Vec<(&str, ColType)> = s
        .header
        .iter()
        .enumerate()
        .map(|(i, h)| (h.as_str(), infer_col_type(&samples(s, i))))
        .collect();
    UploadTableSpec {
        schema: schema.clone(),
        table: SafeIdent::sanitize(&format!("t{ord}_{}", s.name), ord),
        columns: build_columns(&cols),
    }
}

/// 第 `i` 列的取样值。行比表头短时 `get` 返 `None`（缺格不参与推断），空白值由
/// `infer_col_type` 自己滤。
fn samples(s: &Sheet, i: usize) -> Vec<&str> {
    s.rows.iter().take(SAMPLE_ROWS).filter_map(|r| r.get(i).map(String::as_str)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "11111111-2222-3333-4444-555555555555";

    fn sheet(name: &str, header: &[&str], rows: &[&[&str]]) -> Sheet {
        Sheet {
            name: name.into(),
            header: header.iter().map(|s| (*s).into()).collect(),
            rows: rows.iter().map(|r| r.iter().map(|c| (*c).into()).collect()).collect(),
        }
    }

    fn schema() -> SafeIdent {
        schema_ident(DOC)
    }

    /// schema 与 ds_id 都由 doc_id 派生：删文档时 server 靠这两个纯函数找回该清理什么
    #[test]
    fn schema_and_ds_id_derive_from_doc_id() {
        assert_eq!(schema().as_str(), "up_11111111_2222_3333_4444_555555555555");
        assert!(SafeIdent::parse(schema().as_str()).is_some());
        assert_eq!(upload_ds_id(DOC), "upload_11111111-2222-3333-4444-555555555555");
        // ds_id 必须过 server 侧 `valid_ds_id`（字母数字 + `_-`，≤64）
        let id = upload_ds_id(DOC);
        assert!(id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    }

    /// 🔴 全空 sheet：**两条通道各自的正确行为不同**，一起钉住。
    ///
    /// 通道①（文本）跳过它 —— `sheet_block` 对空表产出 `"# 名字\n\n"`，
    /// 一个只有标题的块进了 `kb.chunk` 就会参与检索。
    /// 通道②（建表）不建零列表，但**要把名字放进 `skipped`**（`plan` 负责）。
    /// 两件加起来才是契约里那句「不建表但不能静默」。
    ///
    /// 前置条件是 Python 侧**必须把空 sheet 报上来**（`embed_service::_sheet` 曾 `return None`，
    /// 于是两个 sheet 的 xlsx 只回 1 个、另一个无声消失，`skipped` 永远看不到它）。
    #[test]
    fn empty_sheet_skips_text_channel_but_is_reported_by_plan() {
        let empty = Sheet { name: "空表".into(), header: vec![], rows: vec![] };
        let good = sheet("一月台账", &["客户名称", "金额"], &[&["甲", "1"]]);
        // 通道①：只出非空那一个块
        let blocks = sheet_blocks(&[good.clone(), empty.clone()]);
        assert_eq!(blocks.len(), 1, "空 sheet 不许产块");
        assert!(blocks[0].text.contains("客户名称"), "{}", blocks[0].text);
        // 通道②：空表进 skipped（名字必须在，那是「不静默」的全部内容）
        let p = plan(&schema(), &[good, empty]).unwrap();
        assert_eq!(p.specs.len(), 1);
        assert_eq!(p.skipped, ["空表"], "空 sheet 必须被报出来");
    }

    /// 🔴 `ds_id` → schema 必须**能还原成建表时那一个**（建池的 `search_path` 取它）。
    /// 对着字面量断言就没意义了：这里刻意让两个方向的函数互相印证。
    #[test]
    fn schema_round_trips_from_the_ds_id() {
        assert_eq!(upload_schema_of_ds(&upload_ds_id(DOC)).as_deref(), Some(schema().as_str()));
        // 非上传源不许被当成上传源（给 dms 主源设 search_path 会让存量取数全查错 schema）
        assert_eq!(upload_schema_of_ds("dms"), None);
        assert_eq!(upload_schema_of_ds("crm_pg"), None);
    }

    /// 注入题：恶意 sheet 名与表头清洗后必须仍是合法标识符，原文只出现在注释/描述里
    #[test]
    fn injection_in_headers_never_reaches_identifiers() {
        let s = sheet(
            "'; DROP SCHEMA up_x CASCADE --",
            &["a; DROP TABLE x", "\"; --"],
            &[&["1", "2"]],
        );
        let p = plan(&schema(), &[s]).unwrap();
        let spec = &p.specs[0].1;
        assert!(SafeIdent::parse(spec.table.as_str()).is_some(), "{}", spec.table.as_str());
        for c in &spec.columns {
            assert!(SafeIdent::parse(c.name.as_str()).is_some(), "{}", c.name.as_str());
        }
        assert_eq!(spec.columns[0].name.as_str(), "a__drop_table_x");
        // 原始表头一个字不丢：它进列注释（`render_column_comments` 负责转义）
        assert_eq!(spec.columns[1].header, "\"; --");
    }

    /// 中文表头进列注释，不进列名
    #[test]
    fn chinese_headers_go_to_comments_not_names() {
        let s = sheet("一月台账", &["客户名称", "金额"], &[&["甲", "1"]]);
        let p = plan(&schema(), &[s]).unwrap();
        let spec = &p.specs[0].1;
        let names: Vec<&str> = spec.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["c0", "c1"], "中文列名退化为 c{{ord}}");
        let headers: Vec<&str> = spec.columns.iter().map(|c| c.header.as_str()).collect();
        assert_eq!(headers, ["客户名称", "金额"]);
        // 中文 sheet 名整段退化成 `_`，序号前缀仍在（表名照样是合法标识符）
        assert!(spec.table.as_str().starts_with("t0_"), "{}", spec.table.as_str());
        assert!(SafeIdent::parse(spec.table.as_str()).is_some());
    }

    /// 类型逐列推断（规则本身在 `ddl::infer_col_type`，这里钉的是「按列取样」的接线）
    #[test]
    fn types_inferred_per_column() {
        let s = sheet(
            "s",
            &["amt", "day", "code", "note"],
            &[&["1", "2024-01-31", "0012", "甲"], &["-2.5", "2024/2/1", "0013", ""]],
        );
        let p = plan(&schema(), &[s]).unwrap();
        let tys: Vec<ColType> = p.specs[0].1.columns.iter().map(|c| c.ty).collect();
        assert_eq!(tys, [ColType::Numeric, ColType::Timestamptz, ColType::Text, ColType::Text]);
    }

    /// 缺格（行比表头短）不该把整列打成 Text
    #[test]
    fn ragged_rows_do_not_poison_inference() {
        let s = sheet("s", &["a", "b"], &[&["1", "2"], &["3"]]);
        let p = plan(&schema(), &[s]).unwrap();
        assert_eq!(p.specs[0].1.columns[1].ty, ColType::Numeric);
    }

    /// `Plan` 不实现 Debug（`UploadTableSpec` 也不），故错误分支用 let-else 取而不是 `unwrap_err`
    fn plan_err(sheets: &[Sheet]) -> KbError {
        match plan(&schema(), sheets) {
            Err(e) => e,
            Ok(p) => panic!("本该报错，却规划出了 {} 张表", p.specs.len()),
        }
    }

    /// 超限**报错**，不截断（静默截断 = 用户以为数据全了）
    #[test]
    fn over_limit_errors_instead_of_truncating() {
        let wide: Vec<&str> = vec!["a"; MAX_COLS + 1];
        let e = plan_err(&[sheet("宽", &wide, &[])]);
        assert!(matches!(&e, KbError::BadInput(m) if m.contains("超过上限 200 列")), "{e}");

        let one = vec!["1".to_string()];
        let tall = Sheet {
            name: "高".into(),
            header: vec!["a".into()],
            rows: vec![one; MAX_ROWS + 1],
        };
        let e = plan_err(&[tall]);
        assert!(matches!(&e, KbError::BadInput(m) if m.contains("超过上限 200000 行")), "{e}");
        // 边界值本身放行
        assert!(plan(&schema(), &[sheet("边", &vec!["a"; MAX_COLS], &[])]).is_ok());
    }

    /// 空表 / 无表头：跳过且在返回值里体现；全被跳过时报错（不登记一个空数据源）
    #[test]
    fn empty_sheets_are_skipped_and_reported() {
        let sheets = vec![
            sheet("空", &[], &[]),
            sheet("无表头", &["", "  "], &[&["1", "2"]]),
            sheet("正常", &["a"], &[&["1"]]),
        ];
        let p = plan(&schema(), &sheets).unwrap();
        assert_eq!(p.skipped, ["空", "无表头"]);
        assert_eq!(p.specs.len(), 1);
        assert_eq!(p.specs[0].0, 2, "下标要对得上，否则灌错 sheet 的数据");

        let e = plan_err(&[sheet("空", &[], &[])]);
        assert!(matches!(&e, KbError::BadInput(m) if m.contains("没有可建表的 sheet")), "{e}");
    }

    /// 有表头没数据的 sheet 照样建表（空的台账模板，之后可以问「有哪些列」）
    #[test]
    fn header_only_sheet_still_builds_table() {
        let p = plan(&schema(), &[sheet("模板", &["a", "b"], &[])]).unwrap();
        assert_eq!(p.specs[0].1.columns.len(), 2);
        assert!(p.skipped.is_empty());
    }

    /// 同名 sheet 不许塌成一张表（序号前缀），同名表头也不许塌成一列（`build_columns` 消重）
    #[test]
    fn duplicate_sheet_and_header_names_stay_distinct() {
        let s = sheet("Sheet1", &["amount", "amount"], &[&["1", "2"]]);
        let p = plan(&schema(), &[s.clone(), s]).unwrap();
        assert_eq!(p.specs[0].1.table.as_str(), "t0_sheet1");
        assert_eq!(p.specs[1].1.table.as_str(), "t1_sheet1");
        let names: Vec<&str> = p.specs[0].1.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["amount", "amount_2"]);
    }

    /// 通道①的入口确实转调 `ingest` 那份渲染（不是第二份实现）
    #[test]
    fn sheet_blocks_delegates_to_ingest_renderer() {
        let blocks = sheet_blocks(&[sheet("一月", &["日期"], &[&["d1"]])]);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].heading_path, "一月");
        assert!(blocks[0].text.starts_with("# 一月"));
        assert!(blocks[0].text.contains("| 日期 |"));
        assert!(sheet_blocks(&[]).is_empty());
    }
}
