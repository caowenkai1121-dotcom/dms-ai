//! 只读红线的搬运断言（`pipeline.rs:1006-1099` 的 12 个测试，**一字不改**）。
//!
//! 为什么在 `tests/` 而不是 `guard.rs` 的 `#[cfg(test)]`：断言里的 SQL 用例带 DMS 表名
//! （`t_sales_order`/`t_employee`），而 `scripts/check-arch.ps1` 对 `crates/kernel/src`
//! grep `t_[a-z_]{3,}` 判红。「断言一字不改」与「门禁 exit 0」只能这样同时成立
//! （policy crate 的 46 权限断言用的也是 `tests/` 这条路，ARCHITECTURE §4.3）。

use dms_kernel::sql::guard::{ensure_limit_with, is_safe_select_with};
use dms_kernel::{GuardError, MysqlDialect};

/// 迁移前 `sensitive_ref` 取 `meta::SENSITIVE_COLS`（9 词），这里给同一份词表。
const SENSITIVE_COLS: &[&str] = &[
    "login_pwd", "password", "passwd", "secret", "private_key", "id_card", "id_number", "token", "salt",
];

fn is_safe_select(sql: &str) -> Result<(), GuardError> {
    is_safe_select_with(sql, &MysqlDialect, SENSITIVE_COLS)
}

fn ensure_limit(sql: &str) -> String {
    ensure_limit_with(sql, &MysqlDialect, 200)
}

/// 🔴 **评测题集里的每一条 gold SQL 都必须能过闸门。**
///
/// 由来：`tools/eval_cases.json` 是本仓「正确 SQL 的定义」（39 条，逐条连库复审过），
/// 而 `is_safe_select` 从来**没有被喂过它们一次**。后果是闸门可以悄悄收紧到拒绝正确 SQL
/// 而没有任何测试会红 —— 实测就发生了：AS04 的 gold（两个标量子查询相除、顶层无 FROM）
/// 被 `constant_projection` 判成「模型的试探」，评测 **3/3 确定性失败**，
/// 而 `rejects_constant_projection` 的反面清单里 4 条**全部带顶层 FROM**，
/// 那个盲区一条都没测 —— 判据把守卫证成了「不拦 SELECT 1」，没证「不拦真表」。
///
/// 跨语言 `include_str!` 有先例与同一个理由（`knowledge/src/ingest.rs` 的解析器对拍）：
/// 改了一侧当场红，比「两边各写一份、悄悄漂」强。
#[test]
fn every_gold_sql_passes_the_guard() {
    const RAW: &str = include_str!("../../../tools/eval_cases.json");
    let v: serde_json::Value = serde_json::from_str(RAW).expect("eval_cases.json 解析不了");
    let cases = v["cases"].as_array().expect("顶层缺 cases 数组");
    let golds: Vec<(&str, &str)> = cases
        .iter()
        .filter_map(|c| Some((c["name"].as_str()?, c["gold_sql"].as_str()?)))
        .filter(|(_, s)| !s.trim().is_empty())
        .collect();
    // 🔴 防空转：入参解析不动就是「循环一次不转、判据恒绿」。
    // 本仓已四次踩过这一族（`cli.py` 的自匹配、run.rs 判据切歪成恒真…），所以先钉数量。
    assert!(
        golds.len() >= 30,
        "只读到 {} 条 gold —— 语料没解析进来，本判据在空转",
        golds.len()
    );
    let bad: Vec<String> = golds
        .iter()
        .filter_map(|(name, sql)| {
            is_safe_select(sql).err().map(|e| format!("{name}: {e}"))
        })
        .collect();
    assert!(
        bad.is_empty(),
        "闸门拒了 {} 条**正确**的 gold SQL —— 闸门错了，不是 gold 错了：\n  {}",
        bad.len(),
        bad.join("\n  ")
    );
    // 🔴 防「把 is_safe_select 整体改坏也全绿」：它必须仍然会拒该拒的
    assert!(is_safe_select("SELECT 1").is_err(), "常量投影不拦了");
    assert!(is_safe_select("DELETE FROM t_x").is_err(), "只读红线不拦了");
}

#[test]
fn safe_select_passes() {
    assert!(is_safe_select("SELECT a FROM b WHERE c = 1").is_ok());
}

#[test]
fn rejects_multi_statement() {
    assert!(is_safe_select("SELECT 1; DROP TABLE x").is_err());
}

#[test]
fn rejects_non_select() {
    assert!(is_safe_select("UPDATE t SET a = 1").is_err());
}

/// 【A13】危险函数黑名单（SQLBot 的分库清单）：合法 SELECT 里的合法函数，
/// AST 锁 Query 锁不住 —— 读服务器文件系统/执行命令的唯一通道就是这种函数。
/// 业务列 `upload_time`（下划线保在 token 里）必须不受影响。
#[test]
fn rejects_dangerous_functions_but_not_business_columns() {
    assert!(is_safe_select("SELECT LOAD_FILE('/etc/passwd') FROM t_sales_order LIMIT 1").is_err());
    assert!(is_safe_select("SELECT pg_read_file('/etc/passwd') FROM t_sales_order LIMIT 1").is_err());
    assert!(is_safe_select("SELECT xp_cmdshell('dir') FROM t_sales_order LIMIT 1").is_err());
    // 下划线列名是独立 token，不许误伤（售后单查询天天用它）
    assert!(is_safe_select("SELECT h.upload_time FROM t_after_sales_order_header h LIMIT 1").is_ok());
}

#[test]
fn rejects_sensitive() {
    assert!(is_safe_select("SELECT login_pwd FROM t_employee").is_err());
}

#[test]
fn rejects_placeholder() {
    assert!(is_safe_select("SELECT * FROM t WHERE code = '__ORDER_CODE__'").is_err());
    assert!(is_safe_select("SELECT * FROM t WHERE code = 'X_PLACEHOLDER'").is_err());
}

#[test]
fn readonly_redline() {
    // 只读红线：DML/DDL 硬拦
    assert!(is_safe_select("DELETE FROM t_sales_order").is_err());
    assert!(is_safe_select("DROP TABLE t").is_err());
    assert!(is_safe_select("UPDATE t SET a=1").is_err());
    // 但 deleted_flag/created_time/updated_time 列名不误伤
    assert!(is_safe_select("SELECT deleted_flag, created_time, updated_time FROM t_sales_order WHERE deleted_flag = 0").is_ok());
}

#[test]
fn rejects_all_sensitive_cols() {
    // 词表单一事实源：9 类敏感列全部拦（原先只拦 login_pwd/password，后 7 类能查能回显）
    for col in SENSITIVE_COLS {
        let sql = format!("SELECT {col} FROM t_employee");
        assert!(is_safe_select(&sql).is_err(), "{sql} 应被拦");
    }
}

#[test]
fn rejects_system_schema() {
    assert!(is_safe_select("SELECT * FROM information_schema.tables").is_err());
    assert!(is_safe_select("SELECT user FROM mysql.user").is_err());
    // 不误伤业务列名：sys_no / meta_flag 不含「库名.」形态
    assert!(is_safe_select("SELECT sys_no, meta_flag FROM t_sales_order").is_ok());
    // 🔴 自有库那三个 schema（纵深防御，`ARCHITECTURE` §3 的 F3 修法②）。
    // 逐个断言而不是「至少拒一个」：漏掉其中任一个的话，剩下两个照样让断言绿。
    for sql in [
        "SELECT * FROM meta.value_map",
        "SELECT text FROM kb.chunk",
        "SELECT payload FROM chat.msg",
        // 带别名/JOIN 的形态也要拒（子串判据本来就覆盖，钉住它别被换成「只看 FROM 首表」）
        "SELECT c.text FROM t_sales_order o JOIN kb.chunk c ON 1 = 1",
    ] {
        assert!(is_safe_select(sql).is_err(), "该拒：{sql}");
    }
    // 反面（防恒真）：这四条业务 SQL 一条都不许被这道名单误伤 ——
    // 没有它，把 `system_schema_ref` 写成「一律 Err」上面全部断言也绿
    for sql in [
        "SELECT * FROM t_sales_order",
        "SELECT metadata FROM t_goods",
        "SELECT chatter_id FROM t_customer",
        "SELECT kb_code FROM t_goods",
    ] {
        assert!(is_safe_select(sql).is_ok(), "不该拒：{sql}");
    }
}

/// 🔴 **常量投影必须拒**（业主报的准确度问题，已实证）。
///
/// 聊天框只发一个客户名「嗨肉」（该客户有 31567 单 / 144.6 万），
/// LLM 输出 `SELECT 1 AS 探针结果` —— 那是它自言自语的试探，而我们执行了它、
/// 把「探针结果 = 1」当答案给了用户。零报错、零告警、有列名有值。
///
/// 判据是**「有没有引用真表」**而不是「投影像不像常量」——
/// 反面那一半（`SELECT COUNT(*) FROM t` 的投影也不含列引用）必须仍然放行，
/// 否则这道门会把一整族合法聚合打回。
#[test]
fn rejects_constant_projection() {
    for sql in [
        "SELECT 1",
        "SELECT 1 AS `探针结果`",
        "SELECT 'x' AS a, 2 AS b",
        "SELECT NOW()",
        // 嵌套一层也算：`(SELECT 1)` 外面没有 FROM
        "SELECT * FROM (SELECT 1 AS x) t2",
    ] {
        let r = is_safe_select(sql);
        // 最后那条其实有 FROM（派生表）——它**不该**被这道门拒，单独判
        if sql.contains("FROM") {
            assert!(r.is_ok(), "派生表算查了表，不许被常量投影门拒：{sql}");
        } else {
            assert!(r.is_err(), "该拒（模型的试探不是答案）：{sql}");
        }
    }
    // 🔴 反面（防恒真）：这些都查了真表，一条都不许被误伤。
    // 没有这一半，把 `constant_projection` 写成恒 true 上面也全绿 ——
    // 而那会把**全部**问数打回，是最坏的一档。
    for sql in [
        "SELECT COUNT(*) FROM t_sales_order",
        "SELECT SUM(total_amount) FROM t_sales_order WHERE deleted_flag = 0",
        "SELECT 1 FROM t_sales_order LIMIT 1",
        "SELECT * FROM t_customer",
        // 🔴 **顶层没有 FROM、真表全在投影的标量子查询里** —— 这一族原来被误拒。
        //
        // 这不是造的形状：`meta.metric` 的 `refund_ratio`（退款占比）`agg_expr` 就是
        // 两个标量子查询相除，`description` 还明写「必须各写成独立子查询再相除，
        // 不许 JOIN 后聚合」，而那句原样进 prompt ⇒ 模型照口径卡写出来就是这个形状。
        // 评测 AS04 因此 **3/3 确定性失败**：闸门判「模型的试探不是答案」→
        // repair 的错误文案对这个形状是假话 → 次轮 `bail!` 硬失败。
        // 装配器那边又因 `agg_expr` 含 `SELECT` 注定装不出 ⇒ 这一族占比指标永久失败。
        "SELECT ROUND((SELECT SUM(a.refund_amount) FROM t_after_sales_order_header a \
         WHERE a.deleted_flag = 0) / NULLIF((SELECT SUM(o.total_amount) FROM t_sales_order o \
         WHERE o.deleted_flag = 0), 0) * 100.0, 2) AS `退款占比`",
        // 同族更简形态：顶层无 FROM、单个子查询
        "SELECT (SELECT COUNT(*) FROM t_sales_order) AS `单数`",
    ] {
        assert!(is_safe_select(sql).is_ok(), "查了真表却被拒：{sql} → {:?}", is_safe_select(sql));
    }
    // 🔴 放宽之后**业主报的那个现场必须照旧被拒** —— 否则这次修法就把防线拆了。
    // 「嗨肉」那次模型输出的就是下面第一条，而我们执行了它、把「探针结果 = 1」当答案给了用户。
    for sql in ["SELECT 1 AS `探针结果`", "SELECT 1", "SELECT NOW()", "SELECT 1 + 1 AS x"] {
        assert!(
            is_safe_select(sql).is_err(),
            "放宽到「任意层级引了真表」时把一张表都没有的空壳也放过了：{sql}"
        );
    }
}

#[test]
fn limit_appended() {
    assert!(ensure_limit("SELECT * FROM t").ends_with("LIMIT 200"));
    assert_eq!(ensure_limit("SELECT * FROM t LIMIT 5"), "SELECT * FROM t LIMIT 5");
}

#[test]
fn literal_keywords_not_blocked() {
    // 字面量里的敏感词不误拦（AST 化后旧子串扫描的误伤修复）
    assert!(is_safe_select("SELECT * FROM t WHERE remark LIKE '%update %'").is_ok());
    assert!(is_safe_select("SELECT * FROM t WHERE note = 'please delete me'").is_ok());
    // REPLACE() 字符串函数合法（REPLACE INTO 语句被 AST 层拒）
    assert!(is_safe_select("SELECT REPLACE(name, 'a', 'b') FROM t").is_ok());
}

#[test]
fn executable_comment_rejected() {
    assert!(is_safe_select("SELECT /*! 1 */ a FROM t").is_err());
    assert!(is_safe_select("SELECT /*+ hint */ a FROM t").is_err());
}

#[test]
fn limit_literal_not_fooled() {
    // 字面量含 "limit" 不算已限流——必须仍追加 LIMIT（漏判=无界扫描）
    assert!(ensure_limit("SELECT * FROM t WHERE remark = 'limit'").ends_with("LIMIT 200"));
}


/// 🔴 F3：时间桶列判据的四条两面（诊断 wf_c921b918 的修法清单第 F3 条）。
///
/// 为什么在这里而不是 `caliber.rs` 的 `#[cfg(test)]`：用例里的 SQL 带 DMS 表名
/// （`t_after_sales_order_header`），而 `scripts/check-arch.ps1` 对 `crates/kernel/src`
/// grep `t_[a-z_]{3,}` 判红。与 `rejects_constant_projection` 同一条安置理由。
#[test]
fn bucket_column_judged_separately_from_filter() {
    let rule = dms_kernel::CaliberRule::RequireTimeColumn {
        col: "after_sales_time".into(),
        human: "退款按售后单提交时点算".into(),
    };
    // ① gold 原文（过滤与分桶都用对列）→ 必须绿
    let gold = "SELECT DATE_FORMAT(after_sales_time, '%Y-%m') AS `月份`, ROUND(SUM(refund_amount), 2) AS `退款金额`                 FROM t_after_sales_order_header WHERE deleted_flag = 0                 AND after_sales_time >= DATE_FORMAT(CURDATE(), '%Y-01-01')                 AND after_sales_time < DATE_ADD(DATE_FORMAT(CURDATE(), '%Y-01-01'), INTERVAL 1 YEAR)                 GROUP BY `月份` ORDER BY `月份`";
    assert!(dms_kernel::check_caliber(gold, std::slice::from_ref(&rule)).is_empty(), "gold 被判红：{gold}");

    // ② 实测的错形状（过滤用了对列、分桶用了别的时间列）→ 必须红，且 hint 同时出现两列
    let bad = "SELECT DATE_FORMAT(o.order_time, '%Y-%m') AS `月份`, SUM(a.refund_amount) AS `退款金额`                FROM t_after_sales_order_header a JOIN t_sales_order o ON o.sales_order_code = a.sales_order_code                WHERE a.deleted_flag = 0 AND a.after_sales_time >= '2026-01-01'                GROUP BY `月份` ORDER BY `月份`";
    let v = dms_kernel::check_caliber(bad, std::slice::from_ref(&rule));
    assert_eq!(v.len(), 1, "分桶用错列却没红：{bad}");
    assert_eq!(v[0].rule, "require_time_column:after_sales_time");
    assert!(v[0].hint.contains("order_time"), "hint 没点名用错的那一列：{:?}", v[0]);
    assert!(v[0].hint.contains("after_sales_time"), "hint 没点名该用的那一列：{:?}", v[0]);

    // ③ 对称红：同一条 gold 配 `RequireTimeColumn{order_time}` 必须红。
    //    这一条替代了「断言内部字段非空」—— 采集要是恒返空，②③ 会**同时绿**，
    //    那就是本仓四次踩过的空转（`cli.py` 自匹配 / run.rs 判据切歪成恒真）。
    let sym = dms_kernel::CaliberRule::RequireTimeColumn { col: "order_time".into(), human: "按下单时点".into() };
    assert!(!dms_kernel::check_caliber(gold, std::slice::from_ref(&sym)).is_empty(), "采集恒返空 → 对称红也变绿");

    // ④ 时间列在投影但不是桶、且没有 GROUP BY → 必须绿（不扩大判据的嘴）。
    //    必须给一句合法的时间过滤，否则**第一问**（条件里约束了该列吗）会开火，那不是这条要测的。
    let not_a_bucket = "SELECT MAX(after_sales_time) FROM t_after_sales_order_header                         WHERE deleted_flag = 0 AND after_sales_time >= '2026-01-01'";
    assert!(
        dms_kernel::check_caliber(not_a_bucket, std::slice::from_ref(&rule)).is_empty(),
        "时间列在投影但不是桶却被判红：{not_a_bucket}"
    );
}

/// 🔴 条件里的日期截断函数**不算**桶（采集只在投影里找）——
/// WHERE 里写 `DATE_FORMAT(col,'%Y-%m') = '2026-01'` 是过滤，不该被当成「分桶用了它」。
#[test]
fn date_function_in_where_is_a_filter_not_a_bucket() {
    let rule = dms_kernel::CaliberRule::RequireTimeColumn { col: "after_sales_time".into(), human: "x".into() };
    let sql = "SELECT SUM(refund_amount) FROM t_after_sales_order_header                WHERE deleted_flag = 0 AND DATE_FORMAT(after_sales_time, '%Y-%m') >= '2026-01'";
    assert!(dms_kernel::check_caliber(sql, std::slice::from_ref(&rule)).is_empty(), "WHERE 里的 DATE_FORMAT 被当成桶列：{sql}");
}

/// 🔴 FIN01：`NoFanoutJoin` 的真表名两面。
/// 为什么在 `tests/`：用例带 DMS 表名，与 `rejects_constant_projection` 同一条安置理由。
///
/// 实测（评测 FIN01，2026-07-31）：模型为取客户名把发票单头
/// `LEFT JOIN t_sales_order ON customer_code`（一个客户 N 张订单），
/// 开票金额放大 299 倍（654888936 = 2190264 × 299，整除得整整齐齐）。
/// keys 与 `semantic::seed::EDGES` 的 card 推导口径一致（N:1 取左、1:N 取右）。
#[test]
fn fin01_fanout_join_flagged_and_gold_passes() {
    let keys = vec![
        ("t_sales_order".to_string(), "customer_code".to_string()),
        ("t_sales_order".to_string(), "owner_manager".to_string()),
        ("t_sales_order_detail".to_string(), "sales_order_code".to_string()),
        ("t_sales_order_detail".to_string(), "sku_code".to_string()),
        ("t_goods".to_string(), "goods_category_code".to_string()),
    ];
    let rule = dms_kernel::CaliberRule::NoFanoutJoin { keys, human: "多侧键复制行".into() };

    // ① 实测错答（query_log 原文，UNION ALL 两分支都 JOIN 进订单表）→ 两条都红
    let bad = "SELECT `客户`, SUM(invoice_amount) AS `开票金额` FROM (\
         SELECT COALESCE(o.customer_name, '未知') AS `客户`, i.invoice_amount \
         FROM t_invoice_apply_header i LEFT JOIN t_sales_order o ON i.customer_code = o.customer_code \
         WHERE i.deleted_flag = 0 AND i.invoice_status = '2' \
         UNION ALL \
         SELECT COALESCE(o.customer_name, '未知') AS `客户`, i.invoice_amount \
         FROM t_invoice_new_apply_header i LEFT JOIN t_sales_order o ON i.customer_code = o.customer_code \
         WHERE i.deleted_flag = 0 AND i.invoice_status = '2') t \
       GROUP BY `客户` ORDER BY SUM(invoice_amount) DESC LIMIT 10";
    let v = dms_kernel::check_caliber(bad, std::slice::from_ref(&rule));
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].rule, "no_fanout_join:t_sales_order.customer_code");

    // ② gold（发票双流 UNION ALL 后 JOIN t_customer 取名字）→ 必须绿：
    //    t_customer 是主档（customer_code 在它里面唯一），不是重复键。
    let gold = "SELECT c.customer_name AS `客户`, SUM(u.invoice_amount) AS `开票金额` FROM (SELECT customer_code, invoice_amount FROM t_invoice_apply_header WHERE deleted_flag = 0 AND invoice_status = '2' AND apply_time >= DATE_FORMAT(CURDATE(), '%Y-01-01') AND apply_time < DATE_ADD(DATE_FORMAT(CURDATE(), '%Y-01-01'), INTERVAL 1 YEAR) UNION ALL SELECT customer_code, invoice_amount FROM t_invoice_new_apply_header WHERE deleted_flag = 0 AND invoice_status = '2' AND apply_time >= DATE_FORMAT(CURDATE(), '%Y-01-01') AND apply_time < DATE_ADD(DATE_FORMAT(CURDATE(), '%Y-01-01'), INTERVAL 1 YEAR)) u JOIN t_customer c ON c.customer_code = u.customer_code AND c.deleted_flag = 0 GROUP BY c.customer_name ORDER BY `开票金额` DESC LIMIT 10";
    assert!(dms_kernel::check_caliber(gold, std::slice::from_ref(&rule)).is_empty(),
            "gold 被判红（误伤会把对的答案回炉改错）：{gold}");

    // ③ 对称绿：「各客户的销售额」（FROM 订单表 JOIN 客户主档）——
    //    重复键在基表侧、度量聚的就是基表，这是每天的正确写法，不许红。
    let daily = "SELECT c.customer_name, SUM(o.total_amount) FROM t_sales_order o \
                 JOIN t_customer c ON o.customer_code = c.customer_code AND c.deleted_flag = 0 \
                 WHERE o.deleted_flag = 0 GROUP BY c.customer_name";
    assert!(dms_kernel::check_caliber(daily, std::slice::from_ref(&rule)).is_empty(),
            "基表侧重复键被判红：{daily}");

    // ④ 🔴 **业主发货净销售额口径**（防误伤，2026-08-01 实测误判过一次）：
    //    `a JOIN b(dup) JOIN c` 取 `b.price × c.qty` —— a 的行被 b 复制了，
    //    但 a 一列度量都不贡献；c 的行数由它自己与 b 的连接粒度决定。
    //    第一版判据（「度量前缀全落在被 JOIN 侧才放行」）把它判红，
    //    回炉两轮把模型从正确口径上逼走（丢产成品过滤、换错时间列）。
    let owner = "SELECT SUM(CASE WHEN b.is_gift = '1' THEN 0 ELSE b.apportioned_price * c.batch_delivery_quantity END) \
       FROM t_sales_order a \
       JOIN t_sales_order_detail b ON b.sales_order_code = a.sales_order_code AND b.item_type = '3' \
       JOIN t_sales_order_logistics c ON c.sales_order_code = a.sales_order_code AND c.item_code = b.item_code AND c.deleted_flag = '0' \
       JOIN t_goods ds ON ds.goods_code = c.sku_code AND ds.group_number = 'CHJZFL05-SYS' \
       WHERE (a.order_status NOT IN ('0','100') OR a.paid_status = '1') AND c.batch_delivery_quantity != '0' \
         AND c.delivery_time >= '2026-08-01' AND c.delivery_time < CURDATE()";
    assert!(dms_kernel::check_caliber(owner, std::slice::from_ref(&rule)).is_empty(),
            "业主口径被误判扇出（③只看等值另一边贡献的度量）：{owner}");
}

/// 🔴 F4：裸列的**表归属**。
/// 为什么在 `tests/`：用例带 DMS 表名，与 `rejects_constant_projection` 同一条安置理由。（诊断 wf_c921b918 的修法清单第 F4 条）。
    /// 四条两面：两条抓错答、两条挡「为了让别的测试绿而放宽回去」。
    #[test]
    fn bare_column_only_counts_when_unaliased_or_single_base_table() {
        let rule = dms_kernel::CaliberRule::RequireCols {
            table: "t_customer".into(),
            cols: vec!["deleted_flag".into()],
            human: "客户表软删口径".into(),
        };
        // ① 实测错答（FIN01 的形状）：两张发票表（未写别名）各自写裸 `deleted_flag`，
        //    `LEFT JOIN t_customer c`（**写了别名**）漏了 `c.deleted_flag` ——
        //    裸列不许冒充 `t_customer` 的约束，必须判红
        let bad = "SELECT c.customer_name, SUM(u.invoice_amount) FROM (\
                     SELECT customer_code, invoice_amount FROM t_invoice_a WHERE deleted_flag = 0 \
                     UNION ALL SELECT customer_code, invoice_amount FROM t_invoice_b WHERE deleted_flag = 0 \
                   ) u LEFT JOIN t_customer c ON c.customer_code = u.customer_code \
                   GROUP BY c.customer_name";
        let v = dms_kernel::check_caliber(bad, std::slice::from_ref(&rule));
        assert_eq!(v.len(), 1, "发票子查询的裸 deleted_flag 冒充了 t_customer 的约束：{bad}");
        assert_eq!(v[0].rule, "require_cols:t_customer");

        // ② 同一条补上 `c.deleted_flag = 0` 后必返空（判据没把对的也判了）
        let ok = "SELECT c.customer_name, SUM(u.invoice_amount) FROM (\
                    SELECT customer_code, invoice_amount FROM t_invoice_a WHERE deleted_flag = 0 \
                    UNION ALL SELECT customer_code, invoice_amount FROM t_invoice_b WHERE deleted_flag = 0 \
                  ) u LEFT JOIN t_customer c ON c.customer_code = u.customer_code AND c.deleted_flag = 0 \
                  GROUP BY c.customer_name";
        assert!(dms_kernel::check_caliber(ok, std::slice::from_ref(&rule)).is_empty(), "补上后仍判红：{ok}");

        // ③ gold 形态必须绿：多表、**目标表没写别名**、裸列 —— 裸列在该表自己头上是合法的
        //    （FIN01 的 gold：`t_customer` 没写别名，口径条件就是裸 `deleted_flag`）
        let gold = "SELECT customer_name, SUM(invoice_amount) FROM t_customer \
                    WHERE deleted_flag = 0 GROUP BY customer_name";
        assert!(
            dms_kernel::check_caliber(gold, std::slice::from_ref(&rule)).is_empty(),
            "多表场景下目标表没写别名时，裸列不许被误判：{gold}"
        );

        // ④ 单表裸列照旧合法（这条把「基表数 ≤1」那一档钉住，防有人把门改成一律判红）
        let single = "SELECT SUM(invoice_amount) FROM t_invoice_a WHERE deleted_flag = 0";
        assert!(dms_kernel::check_caliber(single, std::slice::from_ref(&rule)).is_empty(), "单表裸列被判红：{single}");
    }
