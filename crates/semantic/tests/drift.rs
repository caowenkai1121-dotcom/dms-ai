//! 把「单一事实源」变成会红的测试。本轮只有 ds 谓词那一条（其余随 recall/correct 落地时补）。
//!
//! 搬运源 `server/src/meta.rs:1883-1929`（`every_meta_recall_is_ds_scoped`，断言体一字不改）。
//! 清单按新位置重写：semantic 自己的 `src/` **全树**（`recall/*` 一落地就自动被扫到，
//! 不依赖谁记得改清单）+ 仍在别的 crate 里读 `meta.*` 的 4 个文件。
//! 跨 crate 的那 4 个**按相对路径在运行时读**（不建依赖边；读不到就当场炸，不静默跳过）。

use dms_semantic::registry::{ds_pred, ds_pred_at, DS_PRED};

/// 仍在别的 crate 里读 `meta.*` 的文件（相对 `crates/semantic/`）。
/// 随 T8/T9 把它们的 SQL 收进 `registry::*` 而逐条删除 —— 删空那天这个常量也就没了。
const EXTERNAL: &[&str] = &[
    "../server/src/direct.rs",
    // `server/src/pipeline.rs` 整块迁 `dms-agent`（T9）后**没有继任条目**，刻意的：
    // 它自己从不写 `meta.*` 的 SQL（一律经 `registry::exemplar`），而 agent 侧连写都写不出来 ——
    // `scripts/check-arch.ps1` 对整个 `crates/agent/src` 守着 `sqlx::query`（FAIL 级），
    // 所以 agent 的每一条 meta 查询都必须落在本 crate 的 `src/**` 里，而那已经全被扫了。
    "../server/src/corrector.rs",
    // 权限档案的两条 SQL 随 inject.rs 迁 dms-policy（`scope_binding` 的 ds 谓词现在在那里）
    "../policy/src/rules.rs",
];

/// `(展示名, 源码)`：semantic 的 `src/**.rs` + `EXTERNAL`。
fn sources() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out: Vec<(String, String)> = vec![];
    let mut stack = vec![root.join("src")];
    while let Some(dir) = stack.pop() {
        for ent in std::fs::read_dir(&dir).expect("crates/semantic/src 必须可读") {
            let path = ent.expect("目录项可读").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let name = path.strip_prefix(root).unwrap_or(&path).display().to_string();
                let src = std::fs::read_to_string(&path).expect("源码可读");
                out.push((name, src));
            }
        }
    }
    for rel in EXTERNAL {
        let src = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("{rel} 读不到（跨 crate 清单漂了？）：{e}"));
        out.push((rel.to_string(), src));
    }
    out
}

/// 🔴【K3-B ②】漂移守卫：每一条 `FROM meta.` 的召回/加载 SQL 都必须带 ds 限定。
/// 这是「以后有人加召回忘了加谓词」的**唯一**防线 —— 漏一条就是 DMS 的口径卡污染别的库
/// （「有效订单剔除 0/108/199」跑到 CRM 的问句上）。
///
/// 判据：`FROM meta.<表>` 那一行往后 8 行内必须出现 `ds_id`（谓词/列/绑定）或 `{ds_pred}`
/// （`DS_PRED` 的唯一拼接点）。豁免只有两类：`meta.datasource` 自身与日志表；
/// 跨源管理批处理写行内标记 `ds:any` 显式豁免（写标记 = 作者想过这件事）。
#[test]
fn every_meta_recall_is_ds_scoped() {
    // 豁免的都是**日志/注册表自身**：它们按定义跨源（每行自带 ds_id 或就是源的清单），
    // 加 `ds_id IN (ds,'*')` 谓词反而会让运营视图只看得见一个源。
    // `query_log` 预先入列：它的 `ds:any` 标记离 SQL 有 11 行，谁把 query_log.rs 加进下面的
    // 文件清单都会当场假红——先在这里断掉那个陷阱。
    const EXEMPT: &[&str] = &["datasource", "correction_log", "failure_log", "query_log"];
    let mut checked = 0usize;
    for (name, src) in &sources() {
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let Some(rest) = line.split("FROM meta.").nth(1) else { continue };
            let table: String =
                rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if table.is_empty() || EXEMPT.contains(&table.as_str()) {
                continue;
            }
            checked += 1;
            // 往前带 2 行：`ds:any` 豁免标记与 `let ds_pred = …` 都写在 SQL 上方
            let win = lines[i.saturating_sub(2)..(i + 8).min(lines.len())].join("\n");
            // 🔴 两张 meta 表 JOIN 时，裸 `ds_id` 不再算证据：JOIN 的 ON 条件本身就要写
            // `AND a.ds_id IN (b.ds_id, '*')` 这类**连接**条件，于是 `win.contains("ds_id")`
            // 恒真 —— 把 WHERE 上的 `{ds_pred}` 整条删掉守卫照样绿（`load_domain_values`
            // 上实测坐实），而 SQL 已退化成「取所有源的行」＝真跨源污染。
            // 判据是行窗口的字符串命中，分不清「ds 限定在这张表上」与「ds_id 三个字出现在附近」；
            // 有 JOIN 时只认 `{ds_pred}`（唯一拼接点）或显式 `ds:any` 豁免标记。
            let joined = win.contains("JOIN meta.");
            let scoped = win.contains("{ds_pred}")
                || win.contains("ds:any")
                || (!joined && win.contains("ds_id"));
            assert!(
                scoped,
                // 文案里的模块名随本轮搬迁更新（meta:: → registry::）：判据一字未动，
                // 但指向一个已删除模块的报错会让下一个人先去找 meta.rs。
                "{name}:{} 读 meta.{table} 却没有 ds 限定（谓词必须拼 registry::DS_PRED{}）:\n{win}",
                i + 1,
                if joined { "；窗口内有 JOIN meta.，裸 ds_id 不算证据" } else { "" }
            );
        }
    }
    // 空转跳闸：清单/遍历漂了就会一条都不检查而「永远绿」。这不是覆盖率目标，只是「真的扫到了」。
    assert!(checked >= 10, "只检查了 {checked} 处 FROM meta.，守卫已成哑测试");
    // 谓词本体不许被改宽：必须是 `ds_id IN (本源, 全局)` 形态
    assert!(DS_PRED.contains("ds_id IN") && DS_PRED.contains("'*'"), "{DS_PRED}");
    assert_eq!(ds_pred(2), " AND ds_id IN ($2, '*')");
    assert_eq!(ds_pred_at("c", 1), " AND c.ds_id IN ($1, '*')");
}

/// 【A20】`table_doc.enabled` 的**同形守卫**：三路召回（向量/trgm）与渲染总闸
/// （`render_schema`，forced 与对面表卡片在此汇流）都必须带 enabled 谓词 ——
/// 漏一路就等于没关（计划原话；与 `every_meta_recall_is_ds_scoped` 同一个判据形状）。
#[test]
fn disabled_tables_are_filtered_on_every_recall_path() {
    let src = include_str!("../src/recall/schema.rs");
    // 每处 `FROM meta.table_doc` 的后 3 行内必须出现 `enabled`
    let lines: Vec<&str> = src.lines().collect();
    let mut checked = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if !line.contains("FROM meta.table_doc") {
            continue;
        }
        checked += 1;
        let win = lines[i..(i + 3).min(lines.len())].join("\n");
        assert!(win.contains("enabled"), "schema.rs:{} 的 table_doc 读取没带 enabled 谓词:\n{win}", i + 1);
    }
    assert!(checked >= 3, "三路召回 + render 总闸，至少 3 处 table_doc 读取：{checked}");
}

/// 🔴 SQL 里许可的插值名。**加一项就要在这里写明「这个值为何不可能来自外部输入」**——
/// 这份白名单就是那道门槛本身。`(文件后缀, 插值名)`：名字太通用（`t`/`cols`）时按文件锁定。
const ALLOW: &[(&str, &str)] = &[
    // registry::ds_pred_at 生成，形如 " AND ds_id IN ($1, '*')"，唯一拼接点有断言守形态
    ("", "ds_pred"),
    // ddl::rekey_ds_pk 的 ALTER TABLE：表名/主键列来自本文件的 &'static 数组，标识符无法参数化
    ("ddl.rs", "t"),
    ("ddl.rs", "cols"),
    // 均为 datasource.rs 自己的 &'static str 常量：固定列清单 / kb.acl 可见性判据
    // （判据里的 login 与 roles 走 $1/$2 bind，拼进去的只有判据骨架本身）
    ("datasource.rs", "DS_COLS"),
    ("datasource.rs", "DS_VISIBLE_PRED"),
    // probe_sql 的 MySQL 反引号标识符：**必须**先过 ident() 白名单字符校验才拼（见该函数注释）；
    // where_del 是本函数的二值字面量。这三项的安全性由 backtick_identifier_cannot_break_out 守。
    ("probe.rs", "col"),
    ("probe.rs", "table"),
    ("probe.rs", "where_del"),
    // ops_caliber 的公式构造器只接收本文件私有函数中的硬编码别名/列/聚合片段，
    // 用来避免把同一段23省区CASE和有效记录过滤复制十几次；没有外部输入入口。
    ("ops_caliber.rs", "col"),
    ("ops_caliber.rs", "alias"),
    ("ops_caliber.rs", "fallback"),
    ("ops_caliber.rs", "av"),
    ("ops_caliber.rs", "iv"),
    ("ops_caliber.rs", "expr"),
    // sales_fact 的 {TABLE}/{ALIAS} 是本模块的 &'static 常量（已验证 DWS 事实表名与固定别名
    // `sf`），标识符无法参数化，编译期钉死，无外部输入入口。
    ("sales_fact.rs", "TABLE"),
    ("sales_fact.rs", "ALIAS"),
    // seed_defs 的 {sales_denominator} 是 `sales_fact::metric_subquery` 按共享合同现场生成的
    // 固定 SQL 片段（退款占比的分母），内容由 sales_fact 常量决定，无外部输入入口。
    ("seed_defs.rs", "sales_denominator"),
];

/// 🔴 SQL 拼接守卫：semantic 是 `meta.*` 的唯一读写口，门禁不再对它守 `sqlx::query`
/// （ARCHITECTURE §2 I2 残缺列 + 裁决 T7a-F1）。**这条测试就是那条规则的替身，而且更紧**：
/// 原规则只问「有没有用 sqlx::query」，这条问「拼进 SQL 的到底是什么」。
///
/// 判据：每个 `format!(` 块若含 SQL 关键字，块内所有 `{标识符}` 必须在 `ALLOW` 里。
/// 漏掉一个就是「把外部文本拼进 SQL」——那是注入面，也是 I5「外部文本永不成为指令」的反面。
///
/// 块边界按**引号配平**定：从 `format!(` 起累加行，到字面量的收尾引号为止（≤25 行封顶）。
/// 这样只覆盖格式串本身 —— 比「遇 `)` 或 `.` 止」准得多（后者会一路吃到下面的 `assert!` 消息，
/// 把断言文案里的 `{s}` 当成 SQL 插值，第一次就是这么假红的）。命名参数行不含 `{名}`，漏掉无害。
#[test]
fn sql_interpolation_is_allowlisted() {
    const KW: &[&str] =
        &["SELECT", "INSERT INTO", "UPDATE ", "DELETE FROM", "ALTER TABLE", "FROM ", "WHERE ", "JOIN ", "VALUES"];
    let allowed = |file: &str, name: &str| {
        ALLOW.iter().any(|(f, n)| *n == name && (f.is_empty() || file.ends_with(f)))
    };
    let mut blocks = 0usize;
    for (name, src) in &sources() {
        if name.starts_with("..") {
            continue; // 别的 crate 的 SQL 由它们自己的门禁守（server 仍是 WarnOnly）
        }
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.contains("format!(") {
                continue;
            }
            let quotes = |s: &str| s.replace("\\\"", "").matches('"').count();
            let mut block = vec![*line];
            let mut q = quotes(line);
            for next in lines.iter().skip(i + 1).take(24) {
                if q >= 2 && q % 2 == 0 {
                    break;
                }
                block.push(next);
                q += quotes(next);
            }
            let text = block.join("\n");
            if !KW.iter().any(|k| text.contains(k)) {
                continue;
            }
            blocks += 1;
            for seg in text.split('{').skip(1) {
                let ident: String =
                    seg.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                let closed = seg[ident.len()..].starts_with('}');
                if !closed || ident.is_empty() {
                    continue; // `{{` 转义、`{}` 位置参数（probe 的 CAST 用过）都不是命名插值
                }
                assert!(
                    allowed(name, &ident),
                    "{name}:{} 把 `{{{ident}}}` 拼进了 SQL。若该值不可能来自外部输入，\
                     把 (文件, 名字) 与理由一并写进 drift.rs 的 ALLOW；否则改用 bind 参数：\n{text}",
                    i + 1
                );
            }
        }
    }
    assert!(blocks >= 15, "只扫到 {blocks} 个 SQL format! 块，守卫已成哑测试");
}
