//! 四张语义注册表的种子：指标 / 维度 / 值链接码表 / 术语。
//! 变更原因＝口径与码表本身（改口径只碰这一个文件）。
//!
//! 初始搬运源为 `server/src/meta.rs:761-867/894-994/1064-1122`。已验证口径允许在这里版本化迁移，
//! 但必须同时更新说明、来源和指标版本；没有稳定公式的资产不得按字段相似度替换。
//! `METRICS`/`MAPS` 继续留在函数体内，避免再造一套种子装配框架。

use crate::registry::datasource::DMS_DS_ID;
use crate::sales_fact::{self, Metric as SalesMetric};
use sqlx::PgPool;

/// 指标注册：默认销售只认 `sales_fact`；其余指标仅在等价资产已验收后迁移。
pub(crate) async fn seed_metrics(pg: &PgPool) -> anyhow::Result<()> {
    // (code, name, aliases, source_table, agg_expr, scope_filter, time_col, dedup_keys, description, unit)
    // time_col = 默认时间列（DMS Java mapper 权威口径 + 连库核实非空）；空=该指标无时间语义（快照类）
    // dedup_keys = 来源表含系统级重复行时的去重键；空=该表无重复问题
    // unit = percent 表示百分数；ratio 表示小数比值（禁止乘 100）；空=无单位。
    const METRICS: &[(&str, &str, &[&str], &str, &str, &str, &str, &str, &str, &str)] = &[
        // 订单事件口径独立保留；默认销售事实由 `sales_fact` 在本数组之后统一播种。
        ("order_amount", "订单额", &["订单金额", "下单金额", "订单总额"],
         "t_sales_order", "SUM(total_amount)",
         "deleted_flag = 0 AND order_status NOT IN ('0','108','199')",
         "order_time",
         "",
         "【ODS 订单口径，非默认销售额】有效订单额＝SUM(total_amount)（剔除暂存0/无效108/作废199）；物理来源 dms_ods.t_sales_order。默认销售 DWS 没有订单金额与订单号，无法无损迁移；只在用户明确问订单额/下单金额时使用，不得回退替代 Doris DWS 销售额", ""),
        // 别名须覆盖口语问法：「有多少个订单」不含"订单数"三字，漏召回则口径卡与口径补全全失效（评测抓获）
        ("order_count", "订单数", &["订单量", "单量", "成交订单数", "多少单", "多少个订单", "多少订单", "几个订单", "几单", "订单笔数", "下了多少"],
         "t_sales_order", "COUNT(DISTINCT sales_order_code)",
         "deleted_flag = 0 AND order_status NOT IN ('0','108','199')",
         "order_time",
         "",
         "【ODS 订单口径】有效订单数（按单号去重），物理来源 dms_ods.t_sales_order；默认销售 DWS 没有订单号，无法无损迁移，禁止按销售事实行数推算", ""),
        // 「成交客户数」此前只在 `meta.term` 里有条目（术语只解释、不产 SQL），于是
        // **只有无维度那一支**被 `agg_template` 的硬编码分支服务，`成交客户数 × 维度`
        // （「各省成交客户数」这类）压根进不了装配器。补成指标是**加法式**的：
        // 无维度那支仍走模板（指标 only 的让路门保证数与 KPI 环比不变），
        // 新增的只是「带维度」那一类。与 `meta.term` 同名条目两处口径同义、措辞各自维护
        // （漂了就是两个答案 —— 改一边时对照另一边）。
        ("buyer_count", "成交客户数", &["下单客户数", "成交客户", "多少客户", "客户数"],
         "t_sales_order", "COUNT(DISTINCT customer_code)",
         "deleted_flag = 0 AND order_status NOT IN ('0','108','199')",
         "order_time",
         "",
         "【ODS 订单口径】下过有效订单的去重客户数（按 customer_code 去重），物理来源 dms_ods.t_sales_order。默认销售 DWS 只能证明存在销售事实，不能无损还原有效下单事件；禁止按销售事实客户数替代", ""),
        ("avg_order_value", "订单客单价", &["客单价", "订单单均", "平均客单"],
         "t_sales_order", "SUM(total_amount)/NULLIF(COUNT(DISTINCT sales_order_code),0)",
         "deleted_flag = 0 AND order_status NOT IN ('0','108','199')",
         "order_time",
         "",
         "【ODS 订单口径，非默认销售指标】订单客单价＝订单额÷订单数＝SUM(total_amount)/NULLIF(COUNT(DISTINCT sales_order_code),0)，物理来源 dms_ods.t_sales_order。默认销售 DWS 没有订单号与订单额，无法无损迁移；不得拿 Doris DWS 默认销售额作分子", ""),
        ("market_expense", "市场费用", &["营销费用", "费用总额", "推广费"],
         "sales_ads.ads_off_sales_cost_customer_dnf",
         "SUM(COALESCE(sup_staff_cost,0)+COALESCE(comp_pkg,0)+COALESCE(comp_cust_complaint,0)+COALESCE(comp_logistics,0)+COALESCE(comp_out_stock,0)+COALESCE(comp_other,0)+COALESCE(mat_costume,0)+COALESCE(mat_card_sign,0)+COALESCE(mat_freight_install,0)+COALESCE(mat_ip_goods,0)+COALESCE(mat_sticker,0)+COALESCE(mat_tasting_table,0)+COALESCE(mat_stand_banner,0)+COALESCE(mat_kt_board,0)+COALESCE(mat_lightbox,0)+COALESCE(mat_other,0)+COALESCE(mat_insulated_bag,0)+COALESCE(mat_tent,0)+COALESCE(eq_baking,0)+COALESCE(eq_other,0)+COALESCE(eq_freight,0)+COALESCE(eq_fridge,0)+COALESCE(eq_sausage,0)+COALESCE(term_adv_fee,0)+COALESCE(term_other,0)+COALESCE(term_entry_barcode,0)+COALESCE(term_display,0)+COALESCE(term_display_material,0)+COALESCE(offline_adv,0)+COALESCE(brand_adv,0)+COALESCE(act_other,0)+COALESCE(act_outsource,0)+COALESCE(act_tasting_sample,0)+COALESCE(act_logistics,0)+COALESCE(act_venue,0)+COALESCE(act_material_build,0)+COALESCE(rebate_key_cust,0)+COALESCE(rebate_fresh_food,0)+COALESCE(rebate_other,0)+COALESCE(not_act_tasting_sample,0)+COALESCE(other,0))",
         "",
         "data_month",
         "",
         "默认市场/营销费用迁移到已验证 sales_ads 客户月度费用宽表：只汇总十类费用列（长促督导、客户赔偿、营销物料、营销设备、终端费用、广告费用、活动执行、客户返利、非活动样品、其他）。amount 是表内配套销售金额，不是费用；禁止加入费用合计，也禁止使用旧 ODS 合计表或专项子表 fallback", ""),
        ("aftersales_count", "售后单数", &["退货数", "售后量", "退货单数", "售后单有多少", "多少售后", "几个售后单"],
         "t_after_sales_order_header", "COUNT(DISTINCT after_sales_code)",
         "deleted_flag = 0",
         "after_sales_time",
         "",
         "【ODS 售后口径】售后单数（按售后单号去重），物理来源 dms_ods.t_after_sales_order_header。申请、审核、实际退款与实退入库尚无已验收的统一 DWS/ADS 等价公式；不得按销售事实退货行数替代", ""),
        ("refund_amount", "退款额", &["售后退款金额", "售后退款", "退款金额", "售后金额"],
         "t_after_sales_order_header", "SUM(refund_amount)",
         "deleted_flag = 0",
         "after_sales_time",
         "",
         "【ODS 申请退款口径，需明确退款定义】当前为 dms_ods.t_after_sales_order_header.refund_amount 申请退款额；不是 actual_refund_amount 实际退款额，也不是实退入库金额。三类事件尚无已验收的统一 DWS/ADS 等价公式；默认销售事实已含退货负数，禁止再次从销售额冲减", ""),
        ("stock_qty", "库存量", &["库存数量", "存货量", "库存"],
         "scm_warehous_manage", "SUM(in_stock_quantity)",
         "inventory_status = 'ZP'",
         "",
         "",
         "【中台库存口径】物理来源 ywzt_ods.scm_warehous_manage（业务中台 WMS 现行库存，2026-08-11 用户指定）。默认且只计 inventory_status='ZP' 正品在库数量；残损/报损/调出/过期/临期/滞销各状态须用户点名才计。actual_quantity（实际数量）与在库差着锁定/冻结/在途，不许混称。现行表无快照时间轴，同比/环比不可算；本表无金额列，库存金额不许从本表推算；门店/经销商进销存口径请用 t_winc_stock_report", ""),
        ("stock_amount", "库存金额", &["库存额", "存货金额", "库存价值"],
         "t_winc_stock_report", "SUM(stock_amount)",
         "product_stock_date = (SELECT MAX(product_stock_date) FROM t_winc_stock_report)",
         "",
         "",
         "【营销通门店进销存金额口径】物理来源 dms_ods.t_winc_stock_report（门店/经销商侧，非公司仓库存价值——中台库存表 scm_warehous_manage 无金额列，公司总库存价值未接入）；必须取 product_stock_date 全表最大日期，禁止跨日期累加", ""),
        // 「赠品箱数」与上面的销量**只差一个码值**（item_type '2' vs '1'），来源/聚合/去重键
        // /时间列逐字相同 —— 与 GOODS14 的 gold 结构完全一致。补成指标之前它是「① 指标不命中」，
        // 整题只能交 LLM，实测答 75,840 而 gold 是 127,211。
        //
        // 别名里**必须有裸「赠品」**，两个理由：① 问句就写「送出去的赠品有多少箱」，
        // 不认它就命不中指标；② `meta.value_map` 里「赠品」是 `item_type='2'` 的码值名
        // （跨 detail/cart/his 三张表，当前被值过滤的歧义门跳过）。哪天有人把那几行去重了，
        // 「赠品」就会被当成值过滤 → 与本指标自己的 `item_type = '2'` 撞上 G2 → 整条拒。
        // 把「赠品」放进别名，子串门（值名被已消化的指标词包含即不认）会先把它挡回来。
        ("gift_box_qty", "赠品箱数", &["赠品", "送出去的赠品", "赠品数量", "赠品箱"],
         "t_sales_order_detail(JOIN t_sales_order 有效订单)", "SUM(box_quantity)",
         "item_type = '2'",
         "order_time",
         "sales_order_code,sku_code,sku_name,box_quantity,amount",
         "【ODS 订单赠品口径，非默认销量】物理来源 dms_ods.t_sales_order_detail，并关联有效订单头。item_type='2'（SystemConsant.java L39 ITEM_TYPE02，已亲验）——**不是** is_gift：作者实测两者冲突 3128 行，is_gift 不权威。默认销售 DWS 无已验收的赠品类型等价字段；与销量不是同一事实，禁止替代 sales_dw 默认销量", ""),
        ("invoice_amount", "开票金额", &["开票额", "发票金额", "发票"],
         "t_invoice_apply_header UNION ALL t_invoice_new_apply_header", "SUM(invoice_amount)",
         "deleted_flag = 0 AND invoice_status = '2'",
         "apply_time",
         "",
         "【ODS 开票口径，数仓当前无稳定替代表】物理来源 dms_ods 两条开票流；必须筛 invoice_status='2'(已开票,码表InvoiceStatusEnum 0未申请/1申请中/2已开票/3冲红申请中/4已冲红/5失败/6部分开票)。老表 t_invoice_apply_header 与新表 t_invoice_new_apply_header 必须 UNION ALL，交集为0；只查一张会严重漏算，禁止按字段相似迁移到应收表", ""),
        ("activity_expense", "活动费用", &["活动经费", "市场活动费用"],
         "t_activity_main", "SUM(total_amount)",
         "deleted_flag = 0",
         "created_time",
         "",
         "【ODS 活动申请口径，需先澄清申请金额还是已核销费用】当前口径是 dms_ods.t_activity_main.total_amount 活动申请金额，不自动迁移到活动 DWS。status 存码：'0'暂存 '1'待申请 '2'申请中 '3'申请失败 '4'已申请 '6'已下发 '7'下发失败 '8'完成 '9'已删除 '10'部分申请（权威来源 ActivityStatusEnum）；禁止把中文名直接写入码列", ""),
        ("activity_count", "活动场次", &["活动数量", "多少场活动", "办了多少活动"],
         "t_activity_main", "COUNT(DISTINCT activity_no)",
         "deleted_flag = 0",
         "created_time",
         "",
         "【ODS 活动单据口径】物理来源 dms_ods.t_activity_main，按 activity_no 去重。status 存码，统计范围必须由用户明确；现有活动 DWS 未验收为该单据事件的等价事实，禁止替用户默认选择状态，也禁止按活动宽表行数推算场次", ""),
        ("activity_promoter_fee", "活动临促人员费用",
         &["活动临促人员的费用", "临促人员费用", "活动临促费用", "临促费用"],
         "t_activity_promoter_fee", "SUM(total_amount)",
         "deleted_flag = 0",
         "created_time",
         "",
         "【ODS 活动费用子口径，非市场费用总额】物理来源 dms_ods.t_activity_promoter_fee，只取 total_amount，时间统一按 created_time；现有活动 DWS 尚无已验收的临促费用等价公式，勿按退化的 person_type/is_expense 推断或过滤，也不得替代十类市场费用合计", ""),
        ("activity_execution_fee", "活动执行人员费用",
         &["活动执行人员的费用", "执行人员费用", "活动执行费用"],
         "t_market_activity_promoter_expense", "SUM(amount)",
         "deleted_flag = 0",
         "created_time",
         "",
         "【ODS 活动费用子口径，非市场费用总额】物理来源 dms_ods.t_market_activity_promoter_expense，只取 amount；activity_date 全空，时间过滤必须使用 created_time。现有活动 DWS 尚无已验收的执行人员费用等价公式，不得替代十类市场费用合计", ""),
        // ── 快照类余额指标（评测 FIN02/FIN04 三轮全红：**缺的是声明，不是缺判据**）──
        // `meta.table_snapshot` 只声明了 t_customer_balance 的「取最新一条 + balance_status='4'」，
        // 没有任何声明说「账户余额＝可开票(8)+不可开票(9)」——于是 balance_type 只能靠 LLM 猜。
        // 🔴 别名一律是 4 字具体名或口语缩写，**不许**出现裸「余额」：那会让「账户余额」与
        // 「信控余额」互相遮蔽（`match_word` 取最长命中，2 字的「余额」对两条同时命中，
        // 而两条的 balance_type 互斥——混一条就必然答错另一条）。
        // time_col 留空：余额是快照量，无时间语义（本表的 created_time 是流水行的写入时刻，
        // 拿它当统计期筛会把「当期未变动的余额桶」整个筛掉）。
        ("account_balance", "账户余额", &["账余", "帐余", "账户余额合计"],
         "t_customer_balance", "SUM(balance)",
         "deleted_flag = 0 AND balance_status = '4' AND balance_type IN ('8','9')",
         "",
         "",
         "【ODS已验证快照口径；fin_dw 字段公式未验收，暂不自动迁移】物理来源 dms_ods.t_customer_balance。账户余额＝可开票余额(balance_type='8')+不可开票余额('9')两桶之和；\
          【必须】先取每个 (客户,账余类型) 的最新一条：ROW_NUMBER() OVER (PARTITION BY customer_code, balance_type ORDER BY created_time DESC, id DESC) 后取 rn = 1，\
          再 SUM(balance)——本表是滚动流水，直接 SUM 会把同一桶的历史行重复求和(实测 10 倍级虚增)；\
          balance_status='4' 是生效行(CustomerBalanceMapper.xml 权威)。\
          【勿混】信控余额是另一类 balance_type='1'，不要与 8/9 算在一起", ""),
        ("credit_balance", "信控余额", &["信控额度", "信控"],
         "t_customer_balance", "SUM(balance)",
         "deleted_flag = 0 AND balance_status = '4' AND balance_type = '1'",
         "",
         "",
         "【ODS已验证快照口径；fin_dw 字段公式未验收，暂不自动迁移】物理来源 dms_ods.t_customer_balance。信控余额＝balance_type='1' 这一桶(账余类型码表：信控1/市场费用3/可开票余额8/不可开票余额9/设备押金10)；\
          【必须】先取每个 (客户,账余类型) 的最新一条：ROW_NUMBER() OVER (PARTITION BY customer_code, balance_type ORDER BY created_time DESC, id DESC) 后取 rn = 1，\
          再 SUM(balance)——本表是滚动流水，直接 SUM 会把同一桶的历史行重复求和；\
          balance_status='4' 是生效行。【勿混】账户余额是可开票(8)+不可开票(9)两桶之和，不是信控这一桶", ""),
        // ── 动销商品数（裁决 二·J′：`item_type='1'` 从表级退回指标级，这一条负责接住 GOODS15）──
        // 表级 `t_sales_order_detail` 现在只声明 `deleted_flag = 0`（`item_type` 的取值随
        // 「问金额还是问数量」而变，见 seed.rs 的 TABLE_SCOPES note）。GOODS15 原本靠表级那条
        // `item_type='1'` 从 292 修到 173，收窄后必须由本指标的 scope_filter 确定性补上。
        // time_col 留空：时间在订单头 order_time 上，明细表没有可用的统计时间列。
        // dedup_keys 留空：`COUNT(DISTINCT sku_code)` 本身就去重，声明 dedup 只会让 RequireDedup
        // 在已经正确的 SQL 上判红（`check_caliber` 对 COUNT(DISTINCT …) 不触发，但别给它机会）。
        ("active_sku_count", "动销商品数", &["动销商品", "动销SKU", "卖出过的商品数", "有销量的商品数"],
         "t_sales_order_detail", "COUNT(DISTINCT sku_code)",
         "item_type = '1' AND deleted_flag = 0",
         "",
         "",
         "【ODS 订单明细口径，非默认销量】物理来源 dms_ods.t_sales_order_detail。动销商品数＝统计期内卖出过的不同商品个数 COUNT(DISTINCT sku_code)：只算正品行 item_type='1'\
          （剔赠品 '2' 与结算行 '3'）且剔软删；【必须】JOIN t_sales_order 取有效状态\
          (o.deleted_flag = 0 AND o.order_status NOT IN('0','108','199')) 与时间窗——\
          统计时间只在订单头 o.order_time 上，明细表没有可用的时间列；\
          默认销售 DWS 的净销售事实未验收为该正品有效订单定义，禁止按 DWS SKU 去重替代；\
          漏 item_type 实测「2026年6月动销商品有多少个」答出 292 而正确 173（虚高 69%）", ""),
    ];
    for (code, name, aliases, src, agg, scope, tcol, dedup, desc, unit) in METRICS {
        upsert_metric(pg, code, name, aliases, src, agg, scope, tcol, dedup, desc, unit).await?;
    }

    // 默认销售事实只从公开合同播种；订单数不属于该事实合同，也不会由事实行数推算。
    for metric in sales_fact::METRICS {
        upsert_metric(
            pg,
            metric.code(),
            metric.name(),
            metric.aliases(),
            sales_fact::TABLE,
            metric.expression(),
            "",
            sales_fact::ORDER_DATE,
            "",
            metric.description(),
            metric.unit(),
        )
        .await?;
    }

    // 派生退款占比保留售后表分子，但分母由 sales_fact builder 生成，禁止复制销售额 SQL。
    let sales_denominator =
        sales_fact::metric_subquery(SalesMetric::SalesAmount, ":begin", ":end");
    let refund_ratio_expr = format!(
        "ROUND((SELECT SUM(refund_amount) FROM t_after_sales_order_header WHERE deleted_flag = 0 AND after_sales_time >= :begin AND after_sales_time < :end) * 100.0 / NULLIF({sales_denominator}, 0), 2)"
    );
    let refund_ratio_source = format!("t_after_sales_order_header / {}", sales_fact::TABLE);
    upsert_metric(
        pg,
        "refund_ratio",
        "退款占比",
        &["退款率", "售后退款占比", "退款金额占比", "退款占销售额比例", "售后退款金额占销售额"],
        &refund_ratio_source,
        &refund_ratio_expr,
        "",
        "after_sales_time",
        "",
        "退款占比＝申请退款额÷默认销售额×100。分子取 dms_ods.t_after_sales_order_header.refund_amount（deleted_flag=0，时间列 after_sales_time）；这不是实际退款额或实退入库金额。分母严格复用 sales_fact::SalesAmount，即 sales_dw.dws_off_offline_sale_dfn 的 SUM(amount)，时间列 order_date",
        "percent",
    )
    .await?;

    // 指标版本 + 可组合维度白名单。没有逐项验证过的组合不走确定性装配；
    // 复合指标仍可由 LLM 生成，但 prompt 会明确可分析维度，之后还要过口径判据。
    const ORDER_DIMS: &[&str] = &["订单门店", "订单客户分类", "订单大区经理", "订单客户类型"];
    const ORDER_PRODUCT_DIMS: &[&str] = &["订单商品分类", "订单品牌"];
    const METRIC_POLICIES: &[(&str, &str, &[&str])] = &[
        ("order_amount", "2-order", ORDER_DIMS),
        ("order_count", "2-order", ORDER_DIMS),
        ("buyer_count", "2-order", ORDER_DIMS),
        ("avg_order_value", "2-order", ORDER_DIMS),
        ("market_expense", "2026.08.07-sales-ads-v1", &[]),
        ("aftersales_count", "1", &[]),
        ("refund_amount", "1", &[]),
        ("refund_ratio", sales_fact::VERSION, &[]),
        ("stock_qty", "1", &[]),
        ("stock_amount", "1", &[]),
        ("gift_box_qty", "2-order", ORDER_PRODUCT_DIMS),
        ("invoice_amount", "1", &[]),
        ("activity_expense", "1", &["活动状态"]),
        ("activity_count", "1", &["活动状态"]),
        ("activity_promoter_fee", "1", &[]),
        ("activity_execution_fee", "1", &[]),
        ("account_balance", "1", &[]),
        ("credit_balance", "1", &[]),
        ("active_sku_count", "2-order", ORDER_PRODUCT_DIMS),
    ];
    for (code, version, dims) in METRIC_POLICIES {
        // 0 行 = code 与 METRICS 打漂（version/allowed_dimensions 静默不生效）：收集后 warn
        let affected = sqlx::query("UPDATE meta.metric SET version=$1, allowed_dimensions=$2 WHERE ds_id=$3 AND metric_code=$4")
            .bind(version)
            .bind(dims.to_vec())
            .bind(DMS_DS_ID)
            .bind(code)
            .execute(pg)
            .await?
            .rows_affected();
        if affected == 0 {
            tracing::warn!("METRIC_POLICIES 未命中指标行（code={code} 与 METRICS 打漂？）");
        }
    }

    let sales_dims = sales_fact::dimension_names()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    for metric in sales_fact::METRICS {
        // DWS 日事实按 order_date 覆盖完整自然日，不沿用旧发货口径的 yesterday 上限。
        // 显式清空而非依赖列默认值，确保已存在的 sales_amount 行也能消除历史残留。
        sqlx::query("UPDATE meta.metric SET version=$1, allowed_dimensions=$2, time_cap='' WHERE ds_id=$3 AND metric_code=$4")
            .bind(sales_fact::VERSION)
            .bind(&sales_dims)
            .bind(DMS_DS_ID)
            .bind(metric.code())
            .execute(pg)
            .await?;
    }

    // 迁移清理不能只认某一个历史 code：旧版本曾用不同 code 注册同名销售指标，
    // 留下一行就可能被语义召回送回旧订单/物流 SQL。当前合同 code 是唯一保留集合。
    let current_sales_codes = sales_fact::METRICS
        .iter()
        .map(|metric| metric.code().to_string())
        .collect::<Vec<_>>();
    let current_sales_names = sales_fact::METRICS
        .iter()
        .map(|metric| metric.name().to_string())
        .collect::<Vec<_>>();
    let mut stale_codes = sqlx::query_scalar::<_, String>(
        "SELECT metric_code FROM meta.metric
         WHERE ds_id = $3 AND name = ANY($1) AND NOT (metric_code = ANY($2))",
    )
    .bind(&current_sales_names)
    .bind(&current_sales_codes)
    .bind(DMS_DS_ID)
    .fetch_all(pg)
    .await?;
    // 已知历史代码名称并不等于“销售额”，不能依赖同名扫描捎带删除。
    for code in ["ship_net_sales"] {
        if !stale_codes.iter().any(|stale| stale == code) {
            stale_codes.push(code.to_string());
        }
    }
    // 批量两条 DELETE（原来每 code 两次往返；element 与 metric 必须是分开的两条，
    // 同一批 code 一个事务：中途失败不留孤儿）
    let mut tx = pg.begin().await?;
    let elem_ids: Vec<String> = stale_codes.iter().map(|code| format!("metric:{code}")).collect();
    sqlx::query("DELETE FROM meta.element WHERE ds_id = $1 AND element_id = ANY($2)")
        .bind(DMS_DS_ID)
        .bind(&elem_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM meta.metric WHERE ds_id = $1 AND metric_code = ANY($2)")
        .bind(DMS_DS_ID)
        .bind(&stale_codes)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn upsert_metric(
    pg: &PgPool,
    code: &str,
    name: &str,
    aliases: &[&str],
    source_table: &str,
    agg_expr: &str,
    scope_filter: &str,
    time_col: &str,
    dedup_keys: &str,
    description: &str,
    unit: &str,
) -> anyhow::Result<()> {
    // 显式写 ds_id（不靠 DDL DEFAULT；别名绑 Vec<&str> 零逐条 String 分配）
    sqlx::query(
        "INSERT INTO meta.metric(metric_code, name, aliases, source_table, agg_expr, scope_filter, time_col, dedup_keys, description, unit, ds_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
         ON CONFLICT (ds_id, metric_code) DO UPDATE SET
           name=$2, aliases=$3, source_table=$4, agg_expr=$5, scope_filter=$6, time_col=$7, dedup_keys=$8, description=$9, unit=$10",
    )
    .bind(code)
    .bind(name)
    .bind(aliases.to_vec())
    .bind(source_table)
    .bind(agg_expr)
    .bind(scope_filter)
    .bind(time_col)
    .bind(dedup_keys)
    .bind(description)
    .bind(unit)
    .bind(DMS_DS_ID)
    .execute(pg)
    .await?;
    Ok(())
}

/// 首批维度注册（取数口径全部来自 direct.rs 已连库坐实的确定性模板——单一事实源，根治 LLM 分组乱 JOIN/取错列）
pub(crate) async fn seed_dimensions(pg: &PgPool) -> anyhow::Result<()> {
    // (code, name, aliases, source_table, expr, description)
    const DIMENSIONS: &[(&str, &str, &[&str], &str, &str, &str)] = &[
        // 🔴 **不替用户挑状态，把状态暴露成维度让他自己看**（业主裁决）。
        //
        // 由来：「活动费用」的 `scope_filter` 只有 `deleted_flag = 0`，于是装配器答的是
        // **全部状态**的合计 —— 含 1121 行暂存 + 1933 行待申请，实测比「已申请+完成」高 108%
        // （2 283 485.51 vs 1 097 948.76）。而把某几个状态写死进 `scope_filter` 是替业务做判断：
        // 11 个状态里哪些算「生效」随场景变（月度费用核销 vs 预算占用口径不同）。
        //
        // 所以给出维度：用户问「今年各状态的活动费用」就自己看得到分布，
        // 问「今年已申请的活动费用」由值过滤换码（见 MAPS 里 t_activity_main.status 那组）。
        // 码→名的 CASE 与 `ActivityStatusEnum` 逐条对齐；'5'(下发中) 在源码里是注释掉的，
        // 这里照样列出但标注 —— 库里若真出现 '5'，宁可显示「下发中(源码已注释)」也别归成'未知'。
        ("activity_status", "活动状态", &["活动的状态", "按活动状态", "各活动状态"],
         "t_activity_main a",
         "COALESCE(CASE a.status WHEN '0' THEN '暂存' WHEN '1' THEN '待申请' WHEN '2' THEN '申请中' \
          WHEN '3' THEN '申请失败' WHEN '4' THEN '已申请' WHEN '5' THEN '下发中(源码已注释)' \
          WHEN '6' THEN '已下发' WHEN '7' THEN '下发失败' WHEN '8' THEN '完成' \
          WHEN '9' THEN '已删除' WHEN '10' THEN '部分申请' END, '未知')",
         "活动状态码→名，权威来源＝DMS 后端 ActivityStatusEnum。库里实测分布（本轮）：\
          '1'待申请 1933 / '8'完成 1906 / '0'暂存 1121 / '4'已申请 470 / '10'部分申请 458。\
          ⚠️ 费用/场次两个指标都**不筛状态**（那是业务判断，交给用户按需过滤）"),
        ("shop", "订单门店", &["下单门店"],
         "t_sales_order o",
         "COALESCE(o.shop_name,'未知')",
         "ODS 订单口径门店取订单头 shop_name；不是默认销售事实的客户 storename"),
        ("shop_business_region", "门店业务省区", &["门店省区", "门店所属省区"],
         "t_master_shop s",
         "COALESCE(NULLIF(s.province_department_name,''),'未归属')",
         "门店业务省区直接读取 t_master_shop.province_department_name（DMS 生产落库字段）；province 是行政省份，禁止从 t_customer.department_id 或省份字面拼接推断。权威映射含上海→浙江省区、海南→广东省区"),
        ("goods_category", "订单商品分类", &["下单商品分类"],
         "t_sales_order_detail d JOIN t_goods g ON g.goods_code = d.sku_code AND g.deleted_flag = 0",
         "COALESCE(NULLIF(g.goods_category_name,''),'未分类')",
         "分类使用商品主档 t_goods.goods_category_name，兼容 DMS 与数仓同名字段；无分类归'未分类'"),
        ("brand", "订单品牌", &["下单商品品牌"],
         "t_sales_order_detail d JOIN t_goods g ON g.goods_code = d.sku_code AND g.deleted_flag = 0",
         "COALESCE(NULLIF(g.brand_name,''),'未归属')",
         "品牌在商品主档 t_goods.brand_name（明细行无品牌列），连接键 d.sku_code = g.goods_code；空串归'未归属'"),
        ("customer_class", "订单客户分类", &["下单客户分类"],
         "t_sales_order o LEFT JOIN t_customer cus ON cus.customer_code = o.customer_code AND cus.deleted_flag = 0",
         // 码表本体在 `present_cn::CUSTOMER_CLASS`（与 entity 卡共用一份）。这里是**注册表
         // 的 agg_expr**，必须是 `&'static str` —— 不能运行时折 SQL，故保留字面量，
         // 由 `customer_class_case_is_in_sync` 钉住两者一字不差。
         "COALESCE(CASE cus.customer_class WHEN '01' THEN '货架店铺' WHEN '02' THEN '新媒体店铺' WHEN '03' THEN '社团店铺' WHEN '04' THEN '线下客户' WHEN '05' THEN '内部客户' WHEN '06' THEN '其他财务专用' WHEN '99' THEN '外部客户的店铺' END,'未分类')",
         "客户分类=t_customer.customer_class 编码列（字典 key=CustClassif 已坐实：真库 04线下客户占 96%），CASE 翻名免字典 JOIN；NULL 归'未分类'"),
        // 【回归 B06】「各区域经理业绩」原本回落 LLM，LLM 拿 t_customer_online_balance
        // （客户在线余额表）的 SUM(total_fee) 当业绩答了 —— 表错、口径错，数字却看着合理。
        // 根因是注册表里没有这个维度，而「区域经理」与订单头 owner_manager「所属经理」
        // 是**两个不同角色**（列注释分别是「大区经理编号」与「所属经理」）。
        // 别名取 4 字的「区域经理」是必需的：`province` 有别名「区域」、`owner` 有别名「经理」，
        // 两者对该问句都是 2 字命中，靠 `direct::pick` 的最长命中才压得过它们。
        ("area_manager", "订单大区经理", &["下单客户大区经理"],
         "t_sales_order o LEFT JOIN t_customer cus ON cus.customer_code = o.customer_code AND cus.deleted_flag = 0 LEFT JOIN t_employee ea ON ea.employee_id = cus.area_manager_id",
         "COALESCE(ea.actual_name, cus.area_manager_id)",
         "大区经理=客户主档 t_customer.area_manager_id（列注释「大区经理编号」），JOIN t_employee 翻 actual_name，查不到回退工号；【勿混】订单头 owner_manager 是「所属经理」，另一个角色"),
        ("customer_type", "订单客户类型", &["下单客户类型"],
         "t_sales_order o LEFT JOIN t_customer cus ON cus.customer_code = o.customer_code AND cus.deleted_flag = 0",
         "COALESCE(CASE cus.customer_type WHEN 'Z001' THEN '一般销售客户' WHEN 'Z002' THEN '财务专用客户' WHEN 'Z003' THEN '关联方客户' WHEN 'Z004' THEN '货架店铺' WHEN 'Z005' THEN '客户终端仓' END,'未分类')",
         "客户类型=t_customer.customer_type 编码列（字典 key=CUST_TYPE 已坐实：Z001一般销售/Z002财务专用为主），CASE 翻名免字典 JOIN；NULL 归'未分类'"),
    ];
    for (code, name, aliases, src, expr, desc) in DIMENSIONS {
        upsert_dimension(pg, code, name, aliases, src, expr, desc).await?;
    }
    for dimension in sales_fact::DIMENSIONS {
        upsert_dimension(
            pg,
            dimension.code(),
            dimension.name(),
            dimension.aliases(),
            sales_fact::SOURCE_WITH_ALIAS,
            dimension.expression(),
            dimension.description(),
        )
        .await?;
    }
    // `manger` 仅确认是名称字段，业务角色与稳定 ID 均未证实；禁用历史遗留声明，
    // 避免它被当成可用于权限或人员归属分析的“业务员”维度。
    sqlx::query(
        "UPDATE meta.dimension SET status='disabled' \
         WHERE ds_id=$1 AND dim_code IN ('owner','manager_name')",
    )
    .bind(DMS_DS_ID)
    .execute(pg)
    .await?;
    Ok(())
}

async fn upsert_dimension(
    pg: &PgPool,
    code: &str,
    name: &str,
    aliases: &[&str],
    source_table: &str,
    expr: &str,
    description: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO meta.dimension(dim_code, name, aliases, source_table, expr, description, ds_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7)
         ON CONFLICT (ds_id, dim_code) DO UPDATE SET
           name=$2, aliases=$3, source_table=$4, expr=$5, description=$6",
    )
    .bind(code)
    .bind(name)
    .bind(aliases.to_vec())
    .bind(source_table)
    .bind(expr)
    .bind(description)
    .bind(DMS_DS_ID)
    .execute(pg)
    .await?;
    Ok(())
}

/// 32 省行政区划（名, 码）：`t_customer.province` 与 `t_sales_order.receiver_province`
/// 共用同一本字典 —— 单一份事实源，补码只改这里（原来两处逐字抄写）。
const PROVINCE_CODES: &[(&str, &str)] = &[
    ("北京", "110000"), ("天津", "120000"), ("河北", "130000"), ("山西", "140000"),
    ("内蒙古", "150000"), ("辽宁", "210000"), ("吉林", "220000"), ("黑龙江", "230000"),
    ("上海", "310000"), ("江苏", "320000"), ("浙江", "330000"), ("安徽", "340000"),
    ("福建", "350000"), ("江西", "360000"), ("山东", "370000"), ("河南", "410000"),
    ("湖北", "420000"), ("湖南", "430000"), ("广东", "440000"), ("广西", "450000"),
    ("海南", "460000"), ("重庆", "500000"), ("四川", "510000"), ("贵州", "520000"),
    ("云南", "530000"), ("西藏", "540000"), ("陕西", "610000"), ("甘肃", "620000"),
    ("青海", "630000"), ("宁夏", "640000"), ("新疆", "650000"), ("台湾", "710000"),
    ("香港", "810000"), ("澳门", "820000"),
];

/// 值链接码表种子（全部来自 meta.pitfall 已连库坐实的码表教训——不猜字典）
pub(crate) async fn seed_value_maps(pg: &PgPool) -> anyhow::Result<()> {
    // (table, column, [(name, code)], match_kind)
    const MAPS: &[(&str, &str, &[(&str, &str)], &str)] = &[
        // ── 从 DMS 代码枚举**数据驱动**归属过来的 5 列（20 行）────────────────────
        //
        // 归属判据三条**同时**成立（`scratchpad/enum_ownership.py`，788 个未登记候选列 →
        // 299 列取到取值 → 17 列 cov 达标 → 14 列过命名关联 → 5 列证据够强）：
        //   ① `cov = |列的实际取值 ∩ 枚举的码| / |列的实际取值| >= 0.8`
        //      —— **分母是列的取值**（搞反就失效：枚举多几档不该降置信度，列里有枚举没有的码必须降）
        //   ② 枚举类名的词干 ∩ 表名/列名 非空
        //   ③ 该列实际取值数 >= 2
        //
        // 🔴 三条都是**实测逼出来的**，少任一条就灌错码（换码器给错数据 = 问 A 拿到 B 的行）：
        //   · 少 ② ⇒ `wms_type`(仓库类型) 与两个 `category_code` 被归给 `DeviceDemandStatusEnum`
        //     —— 纯属 `01`-`05` 这类**短码空间巧合**，cov 照样 1.0
        //   · 少 ③ ⇒ 8 个**单值列**混进来（单值列 cov 恒为 1，证据近零）。其中
        //     `t_sales_order_his.zt_status`(中台状态) 被归给订单状态枚举、词干只靠表名蹭上
        //   · 少 ① 就退化成纯命名猜 —— `OrderStatusEnum` 与 `t_shop_shipment_order.shipment_status`
        //     名字都带 status 而**同码不同名**（枚举「备货完成=200」vs 库里「配送中=200」）
        //
        // ⚠️ 注意 `t_shop_order_header.order_status` 的「配送中=300」与
        // `t_shop_shipment_order.shipment_status` 的「配送中=200」**并存且都对** ——
        // 那正是「必须按列判归属」的实证：同一个中文名在两张表上是两个码。
        //
        // origin 一律走 DDL 默认的 `seed`（不是 `dict`）：本批**只播了该列实际出现过的码**，
        // 不是完整枚举，所以 `RequireKnownValue` 判据不该对它开火。
        // 把 cov==1.0 那批升 `dict` 是**独立一笔**（那会让那条判据第一次真的生效，
        // 需要可回退开关 + 逐题对拍，别与本批混在一趟测量里 —— 否则分数变了归因不到哪一处）。
        ("t_device_delivery_type_goods", "delivery_type_code",
         &[("有押租赁", "DT01"), ("有押租赁押金折扣", "DT02"), ("客户购买", "DT03"),
           ("无押租赁", "DT04"), ("订单满赠", "DT05"), ("呜呜很忙专用", "DT06"),
           ("广饶小虎队专用", "DT07"), ("直营鲜食专用", "DT08")],
         "eq"),
        ("t_shop_order_header", "order_status",
         &[("待配送", "100"), ("备货完成", "200"), ("配送中", "300"),
           ("已签收", "700"), ("部分退货", "800"), ("退货完成", "900")],
         "eq"),
        ("t_device_demand_apply_header", "bill_status",
         &[("审批通过", "04"), ("审批驳回", "05")], "eq"),
        ("t_device_demand_submit_record", "audit_status",
         &[("草稿", "00"), ("审批驳回", "05")], "eq"),
        ("t_device_demand_submit_record_3", "audit_status",
         &[("草稿", "00"), ("提交", "02")], "eq"),
        // 三张对账表的开票状态：`t_invoice_apply_header.invoice_status` 早已登记全 10 档，
        // 而对账侧这三张只登记了 4-5 档 —— 缺的那 5 档（部分开票/待确认/审核通过/驳回/暂存）
        // 用户一问「审核通过的对账单」「被驳回的开票」就换不出码 ⇒ LLM 猜 ⇒ 返 0 行。
        //
        // 权威来源＝DMS 后端 `InvoiceStatusEnum`。补这 15 条前过了**两条硬约束**
        // （`scratchpad/enum_vs_valuemap.py` 实算，把 32 条候选砍到 15 条）：
        //   ① `agree == 1.0` —— 该列**已登记的码全部**与枚举一致（证明归属没归错字典）
        //   ② 新补的码**不与该列已登记的码相撞** —— 撞了就是两个中文名映射同一个码，
        //      换码器会给错数据
        // 被这两条拒掉的 5 组值得记住，它们是「照清单一刀换」的代价：
        //   · `business_type` × 2 表、`expense_type`、`attachment_status` —— **两名一码**
        //   · `t_shop_shipment_order.shipment_status` —— `agree=0.50`：`OrderStatusEnum` 是
        //     **订单**状态枚举，不是配送单的。它的「备货完成=200/已签收=700」与库里
        //     「配送中=200/配送完成=700」**同码不同名**，补进去必出错答。
        //     （生产数据实测该列只有 '200'(1684 行) 与 '700'(551 行)，库里登记是对的。）
        ("t_account_bill_detail", "invoice_status",
         &[("部分开票", "6"), ("待确认", "7"), ("审核通过", "8"), ("驳回", "9"), ("暂存", "10")],
         "eq"),
        ("t_account_bill_detail_invoice", "invoice_status",
         &[("部分开票", "6"), ("待确认", "7"), ("审核通过", "8"), ("驳回", "9"), ("暂存", "10")],
         "eq"),
        ("t_account_bill_header", "invoice_status",
         &[("部分开票", "6"), ("待确认", "7"), ("审核通过", "8"), ("驳回", "9"), ("暂存", "10")],
         "eq"),
        // 🔴 **代码枚举的码，autodiscover 灌不到 —— 必须手写**。
        //
        // 实测坐实的缺口：`t_activity_main` 在 `meta.value_map` 里只有 `company_code`(31 行)
        // 与 `execute_type`(3 行)，**没有 status** —— 因为 autodiscover 只读生产字典表
        // (`t_dict_key`/`t_dict_value`)，而活动状态的码写在**代码枚举** `ActivityStatusEnum` 里。
        // 后果：问「今年已申请的活动费用」时换码器不认「已申请」⇒ LLM 只能猜 ⇒ 写中文名 ⇒ 返 0 行。
        //
        // ⚠️ 这**不是**活动状态一个的问题：DMS 有 102 个枚举类 / 311 个 (码,名) 对，
        // 凡是码只存在于代码里的列都有同一个缺口。系统化的做法是让 autodiscover 的
        // 「字典来源」除生产字典表外也吃一份代码枚举导出 —— 那是独立一笔（见 _DECISIONS 二·AX）。
        // 本组先手写这一个，因为它有实证的错答与可验证的收益。
        ("t_activity_main", "status",
         &[("暂存", "0"), ("待申请", "1"), ("申请中", "2"), ("申请失败", "3"),
           ("已申请", "4"), ("已下发", "6"), ("下发失败", "7"), ("完成", "8"),
           ("已删除", "9"), ("部分申请", "10")],
         "eq"),
        // 省份=行政区划码（实测 t_customer.province 存 '430000' 这类 6 位码，不是省名）。
        // 缺这组映射时问「湖南省销售额」LLM 无从下手——实测直接漏掉省份过滤答成全量。
        ("t_customer", "province", PROVINCE_CODES, "eq"),
        // 🔴 同一本行政区划字典的第二张表（SALE17 实测的**逃逸列**）：
        // `receiver_province` 实测同样存 6 位码（DISTINCT 抽样全是 '430000' 一族）——
        // 模型在 customer.province 被口径判据追着改时，会逃到 `receiver_province LIKE '%湖南%'`
        // 接着错（码列 LIKE 名称照样 0 行）。同一批值（`PROVINCE_CODES` 单一份事实源），
        // 两个落点，判据与换码卡都得看得见。
        ("t_sales_order", "receiver_province", PROVINCE_CODES, "eq"),
        // 客户分类（字典 CustClassif，与 meta.dimension customer_class 的 CASE 同源）：
        // 「线下客户」这类问法必须换成 '04'，否则 LLM 会去猜别的列（实测猜到了 customer_channel）
        ("t_customer", "customer_class", crate::present_cn::CUSTOMER_CLASS, "eq"),
        // 客户类型（字典 CUST_TYPE）
        ("t_customer", "customer_type", crate::present_cn::CUSTOMER_TYPE, "eq"),
        // 售后类型（SystemConsant.java L148-149 / AfterSalesServiceImpl L1218-1222；Java 无枚举类）
        ("t_after_sales_order_header", "after_sales_type",
         &[("退货", "1"), ("退款", "2"), ("中台售后", "3")], "eq"),
        // InvoiceStatusEnum（pitfall 坐实，真库现存 0/1/2/3/5）
        ("t_invoice_apply_header", "invoice_status",
         &[("未申请", "0"), ("开票申请中", "1"), ("已开票", "2"), ("冲红申请中", "3"),
           ("已冲红", "4"), ("开票失败", "5"), ("部分开票", "6"), ("待确认", "7"),
           ("审核通过", "8"), ("驳回", "9"), ("暂存", "10")], "eq"),
        // InvoiceTypeEnum（t_invoice_apply_header 与 t_customer 同体系）
        ("t_invoice_apply_header", "invoice_type",
         &[("增值税普通发票", "1"), ("普票", "1"), ("增值税专用发票", "2"), ("专票", "2")], "eq"),
        ("t_customer", "invoice_type",
         &[("增值税普通发票", "1"), ("普票", "1"), ("增值税专用发票", "2"), ("专票", "2")], "eq"),
        // 有效订单口径（pitfall 坐实 0暂存/108/199）。🔴 DMS 校准正名（SystemConsant.java:23-38）：
        // 108=已取消、199=已删除 —— 旧名「无效/作废」由 seed_value_maps 开头的 DELETE 收敛
        // （upsert 键含 name，不删旧行就是两名一码）。Java 侧共 17 档，其余 14 档码名
        // 待源码清单导出后补齐（不臆造）。
        // 🔴 16 档全播（原先只有 3 档）。码表见 `present_cn::SALES_ORDER_STATUS` ——
        // 生产 SQL 那段 `CASE` 用了很久的同一份，不是臆造。少播的那 13 档正是业主截图里
        // 单据卡印出裸 `101` 的原因：`translate_cell` 对「已登记但无此码」返回原样。
        ("t_sales_order", "order_status", crate::present_cn::SALES_ORDER_STATUS, "eq"),
        // 与 order_status 同一条：码表本体在 `present_cn`，展示侧码→名与问句侧名→码共用。
        ("t_goods", "on_sale", crate::present_cn::GOODS_ON_SALE, "eq"),
        ("t_goods", "frozen_state", crate::present_cn::GOODS_FROZEN, "eq"),
        // PayWayEnum：真库有逗号组合值——ZX01 纯值可等值，余额类必须 LIKE 含组合（pitfall 坐实）
        ("t_sales_order", "paid_way", &[("在线支付", "ZX01")], "eq"),
        ("t_sales_order", "paid_way",
         &[("信控余额支付", "ZZ01"), ("市场费用支付", "ZF02"), ("不开票余额支付", "ZZ04"),
           ("可开票余额支付", "ZZ05"), ("设备押金支付", "ZZ07")], "like"),
        // 账余类型（pitfall 坐实；15/99 双码在线支付歧义不收录）
        ("t_customer_balance", "balance_type",
         &[("信控", "1"), ("市场费用", "3"), ("可开票余额", "8"), ("不可开票余额", "9"),
           ("设备押金", "10")], "eq"),
        // AccountBillStatusEnum（Java 枚举坐实）+ account_mode
        ("t_account_bill_header", "bill_status",
         &[("待确认", "0"), ("已确认", "1"), ("部分开票", "2"), ("已开票", "3"), ("拒绝", "4")], "eq"),
        ("t_account_bill_header", "account_mode",
         &[("月结", "M"), ("半月", "HM"), ("周结", "WK")], "eq"),
        // 明细行类型（M6w 坐实）。🔴 DMS 校准正名：「商品行」→「正品」（与 SystemConsant.java
        // 注释及 pitfall 教训里的「1正品」口径一致；旧名由 DELETE 收敛，防两名一码）
        ("t_sales_order_detail", "item_type",
         &[("正品", "1"), ("赠品", "2"), ("结算行", "3")], "eq"),
        // ── DMS 后端源码校准补齐（专项调研，码名以 Java 侧枚举/常量为准）──────────────
        // 订单类型六值（SystemConsant.java）
        ("t_sales_order", "order_type",
         &[("线下销售", "SO01"), ("设备", "SO04"), ("样品", "SO10"),
           ("样品领用", "SO12"), ("营销物料", "SO15"), ("积分兑换", "SO16")], "eq"),
        // 支付状态三值
        ("t_sales_order", "paid_status",
         &[("未支付", "0"), ("已支付", "1"), ("支付中", "2")], "eq"),
        // 售后状态九档（AfterSalesStatusEnum.java:19-30）
        ("t_after_sales_order_header", "after_sales_status",
         &[("待提交确认", "1"), ("发货确认中", "2"), ("待退货入库", "3"), ("退款中", "4"),
           ("完成", "5"), ("取消", "6"), ("驳回", "7"), ("退款执行中", "8"), ("退款失败", "9")], "eq"),
    ];
    // 🔴 DMS 校准正名的旧行先收敛：upsert 键含 name，不删旧名行就是两名一码
    // （108 已取消←无效、199 已删除←作废、item_type 1 正品←商品行）。
    // 逐条 bind 而不是 IN 列表字面量：本文件的码值过滤源码守卫会扫 IN 段里的中文字面量。
    for (t, c, old) in [
        ("t_sales_order", "order_status", "无效"),
        ("t_sales_order", "order_status", "作废"),
        ("t_sales_order_detail", "item_type", "商品行"),
    ] {
        sqlx::query(
            "DELETE FROM meta.value_map WHERE ds_id = $1 AND table_name = $2 AND column_name = $3 AND name = $4",
        )
        .bind(DMS_DS_ID)
        .bind(t)
        .bind(c)
        .bind(old)
        .execute(pg)
        .await?;
    }
    for (table, col, pairs, kind) in MAPS {
        for (name, code) in *pairs {
            sqlx::query(
                "INSERT INTO meta.value_map(table_name, column_name, name, code, match_kind, ds_id)
                 VALUES ($1,$2,$3,$4,$5,$6)
                 ON CONFLICT (ds_id, table_name, column_name, name) DO UPDATE SET code=$4, match_kind=$5",
            )
            .bind(table)
            .bind(col)
            .bind(name)
            .bind(code)
            .bind(kind)
            .bind(DMS_DS_ID)
            .execute(pg)
            .await?;
        }
    }
    Ok(())
}

/// 首批业务术语（DomainTerms）：黑话→标准口径，注入 prompt 帮 LLM 理解
pub(crate) async fn seed_terms(pg: &PgPool) -> anyhow::Result<()> {
    const TERMS: &[(&str, &str, &[&str])] = &[
        ("GMV", "成交总额＝dms_ods.t_sales_order 有效订单额 SUM(total_amount)；不是默认 Doris DWS 销售额", &["成交额", "成交总额"]),
        ("动销", "dms_ods 有效订单在统计期内的正品 item_type='1' 去重商品数；必须关联订单头过滤有效状态和 order_time，禁止按默认销售 DWS SKU 去重替代", &["在售", "有销量"]),
        ("成交客户数", "dms_ods.t_sales_order 中下过有效订单的去重客户数 COUNT(DISTINCT customer_code)；默认销售 DWS 不替代订单事件口径", &["下单客户数", "成交客户"]),
        ("复购", "dms_ods.t_sales_order 中同一客户在统计期内有效订单数≥2(COUNT DISTINCT sales_order_code GROUP BY customer_code HAVING>=2)", &["复购客户", "二次购买"]),
        ("订单客单价", "订单客单价＝订单额÷订单数＝SUM(total_amount)/NULLIF(COUNT(DISTINCT sales_order_code),0)；不使用默认 Doris DWS 销售额", &["客单价", "订单单均", "平均客单"]),
    ];
    // 旧术语把订单额写成销售额，保留会与 DWS 默认销售额同时召回。
    sqlx::query("DELETE FROM meta.term WHERE ds_id=$1 AND term='客单价'")
        .bind(DMS_DS_ID)
        .execute(pg)
        .await?;
    sqlx::query("DELETE FROM meta.element WHERE ds_id=$1 AND element_id='term:客单价'")
        .bind(DMS_DS_ID)
        .execute(pg)
        .await?;
    for (term, def, aliases) in TERMS {
        sqlx::query(
            "INSERT INTO meta.term(term, definition, aliases, ds_id) VALUES ($1,$2,$3,$4)
             ON CONFLICT (ds_id, term) DO UPDATE SET definition=$2, aliases=$3",
        )
        .bind(term)
        .bind(def)
        .bind(aliases.to_vec())
        .bind(DMS_DS_ID)
        .execute(pg)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// 🔴 **码值过滤不许写中文名**（业主要求「深度参考源码」时挖出的活错答）。
    ///
    /// ⚠️ 本注释**刻意不写出带引号的中文名**：判据是文本扫描，会扫到自己的文档
    /// （`no_bare_balance_alias` 的注释里记着同一个坑，第一版就这么自伤过一次）。
    ///
    /// 实证：「活动费用」的 description 原本把两个中文状态名直接放进了 status 的 IN 过滤 —— 而
    /// `t_activity_main.status` 存的是**码**（DMS 后端 `ActivityStatusEnum`：'4'=已申请、'8'=完成）。
    /// 三数交叉验证：
    ///   全部状态 2 283 485.51（装配器答的，**虚高 108%** —— `scope_filter` 里没有状态过滤）
    ///   `IN('4','8')` 1 097 948.76（权威）
    ///   中文名那版 **0.00**（LLM 照口径卡抄会得到的）
    /// **同一个问句两条路都错、方向相反、差一倍以上。**
    ///
    /// 判据形态：扫本文件里出现在**过滤位**（`= '…'` / `IN ('…')`）的中文字面量。
    /// 刻意只认过滤位 —— `CASE … THEN '货架店铺'`（码→名映射）与 `COALESCE(x,'未知')`
    /// （兜底输出）都是**正确**写法，一起判就是 11 条假报，而假报会淹掉真报
    /// （扫描器第一版实测正是如此：11 假 2 真）。
    // ── 🚧 一条**刻意不做成编译期判据**的约束（本轮实测得出）──────────────────
    //
    // 「同一列里不许两个中文名映射到同一个码」——这条在**从外部清单补码值时**是硬约束：
    // `OrderStatusEnum` 的「备货完成=200/已签收=700」与库里
    // `t_shop_shipment_order.shipment_status` 的「配送中=200/配送完成=700」**同码不同名**，
    // 照清单补进去 ⇒ 用户问「备货完成的配送单」→ 换成 `200` → 拿到「配送中」的数据
    // ⇒ 确定性错答、route 正常、零报错。
    //
    // 🔴 但它**不是种子本身的不变量**：我写成判据跑了一次，当场抓到
    //   「码 1 被『增值税普通发票』与『普票』共用」「码 2 被『增值税专用发票』与『专票』共用」
    // —— 那是**口语别名**，两名一码在这里是故意且正确的（用户说哪个都要换得出码）。
    // 危险的只是「**语义不同**的两个名共用一码」，而那判不动 —— 是人的判断。
    // 白名单式豁免会随种子增长而腐烂，所以不做。
    //
    // 约束留在它该在的地方：**补码值的流程**里。
    // `scratchpad/enum_vs_valuemap.py` 实现了两条硬约束
    // （① 该列已登记的码全部与枚举一致 ② 新补的码不与已登记码相撞），
    // 本轮用它把 32 条候选砍到 15 条、拒掉 5 组（4 组两名一码 + 1 组归错字典）。
    // 下次从 DMS 枚举补码值前跑它，别凭清单直接抄。
    //
    // DB 层已保证的那一半：`(ds_id, table_name, column_name, name)` 是主键
    // ⇒ **同一个名字不许映射到两个码**（那才是真矛盾）。

    #[test]
    fn code_filters_never_use_chinese_names() {
        let src = include_str!("seed_defs.rs");
        let cjk = |s: &str| s.chars().any(|c| (0x4e00..=0x9fff).contains(&(c as u32)));
        let mut bad: Vec<String> = vec![];
        // 🔴 `IN (` 与 `IN(` **两种都要认**：无空格形态漏判是真漏洞 ——
        // 我自己写的那句警告文案正好是 `IN(` 无空格，第一版判据因此侥幸没自伤，
        // 也就是说它对真正的 bug 形态同样会漏。
        for pat in ["IN (", "IN("] {
        for (i, _) in src.match_indices(pat) {
            let rest = &src[i + pat.len()..];
            let Some(end) = rest.find(')') else { continue };
            // 奇数位是引号之间的内容
            for lit in rest[..end].split('\'').skip(1).step_by(2) {
                if cjk(lit) {
                    bad.push(format!("IN (… '{lit}' …)"));
                }
            }
        }
        }
        for (i, _) in src.match_indices("= '") {
            // 🔴 只判「列名本身是码」的等值（`_code` / `_status` / `_type` / `_flag` / `_id`）。
            // 列名从行尾**往前**提取：声明里 `t.` 之后一律是带空格的 `= '…'`，
            // 而前缀「表.列」里的点也会被当成分隔符 —— 从后往前就能拿到 `status` 这一段
            // （把「表.列」当整体看 `_code`/`_status` 的后缀判据才不会漏 `a.status = '已完成'`）。
            //
            // 不设这条收窄时把**字典的值列**也判了 —— 那正是这个判据要治的病的反面：
            // `t_dict_value` 的 `value_code` 是码、`value_name` 是**名**，后者本来就该写中文
            // 名称列本来就允许中文值，只有码字段过滤位需要拒绝中文名。
            // 同一族问题本仓已三次「判据太宽」：枚举全覆盖的 `CASE THEN`、口语别名（普票/增值税普通发票）、
            // 以及这里 —— 挑了个容易算的量当判据，而它与「错」之间不是充要关系。
            let line_start = src[..i].rfind('\n').map(|j| j + 1).unwrap_or(0);
            let col = src[line_start..i]
                .split(|c: char| c.is_whitespace())
                .rev()
                .find(|s| !s.is_empty())
                .unwrap_or("")
                .rsplit('.')
                .next()
                .unwrap_or("");
            if !(col.ends_with("_code")
                || col.ends_with("_status")
                || col.ends_with("_type")
                || col.ends_with("_flag")
                || col.ends_with("_id"))
            {
                continue;
            }
            let rest = &src[i + 3..];
            if let Some(end) = rest.find('\'') {
                let lit = &rest[..end];
                if cjk(lit) {
                    bad.push(format!("{col} = '{lit}'"));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "码值过滤写了中文名 ⇒ 必然返 0 行（码字段永不等于中文名）：{bad:?}。\
             正确写法：写码，并在同一句里注明码的含义（照「活动费用」那条的形态）"
        );
    }

    #[test]
    fn shop_business_region_dimension_uses_the_persisted_shop_field() {
        let src = include_str!("seed_defs.rs");
        let dimensions = src
            .split("const DIMENSIONS: &[(&str, &str, &[&str], &str, &str, &str)] = &[")
            .nth(1)
            .expect("DIMENSIONS 不见了")
            .split("];")
            .next()
            .unwrap();
        let compact = dimensions.split_whitespace().collect::<String>();

        assert!(compact.contains("(\"shop_business_region\",\"门店业务省区\",&[\"门店省区\",\"门店所属省区\"],\"t_master_shops\",\"COALESCE(NULLIF(s.province_department_name,''),'未归属')\""));
        assert!(dimensions.contains("上海→浙江省区") && dimensions.contains("海南→广东省区"));
        assert!(dimensions.contains("禁止从 t_customer.department_id"));
        let tuple = dimensions
            .split("(\"shop_business_region\"")
            .nth(1)
            .expect("门店业务省区维度不见了")
            .split("),")
            .next()
            .unwrap();
        assert!(!tuple.contains("t_customer cus"), "门店业务省区不能经客户表取部门：{tuple}");
        assert!(!tuple.contains("cus.department_id"), "门店业务省区不能读取 customer.department_id：{tuple}");
    }

    /// 🔴 本文件里不许出现裸「余额」这个名字/别名（种子是 const、在函数体内，测试够不到，
    /// 只能扫源码本身）。`match_word` 取最长命中：2 字的「余额」对「账户余额」与「信控余额」
    /// 两条**同时**命中，而这两条的 `balance_type` 互斥（可开票+不可开票 8/9 vs 信控 1）——
    /// 谁赢由行序决定，等于随机答错另一条。真库坐实的失败面：FIN02 与 FIN04 一起翻红。
    /// （`seed.rs` 的 KW_FORCE 里有一条「余额 → t_customer_balance」，那是另一回事：
    /// 它只强制把表补进 schema 召回，不参与指标命中。本判据只扫本文件。）
    ///
    /// ⚠️ 判据是文本扫描：本文件里连**注释**都不许写出带引号的那个两字词，否则守卫自伤
    /// （第一版就这么红过一次 —— 也算它真的会响）。
    #[test]
    fn no_bare_balance_alias() {
        let src = include_str!("seed_defs.rs");
        assert!(!src.contains("\"余额\""), "「余额」会让账户余额与信控余额互相遮蔽");
    }

    #[test]
    fn default_sales_metrics_only_come_from_sales_fact() {
        let src = include_str!("seed_defs.rs");
        let local_metrics = src
            .split("const METRICS:")
            .nth(1)
            .expect("METRICS 不见了")
            .split("];")
            .next()
            .expect("METRICS 结束锚点不见了");
        for code in [
            "sales_amount",
            "sales_qty",
            "sales_cost",
            "sales_revenue_ex_tax",
            "gross_profit_amount",
            "gross_margin",
        ] {
            // 锚在元组位的指标码（`("code"`）：表名里的子串（如 ads_off_sales_cost_…）不算重复声明
            let anchor = format!("(\"{code}\"");
            assert!(!local_metrics.contains(&anchor), "默认销售指标不得在本地重复声明：{code}");
        }
        for anchor in [
            "for metric in sales_fact::METRICS",
            "sales_fact::TABLE",
            "metric.expression()",
            "metric.description()",
            "metric.unit()",
            "name = ANY($1) AND NOT (metric_code = ANY($2))",
            "for code in [\"ship_net_sales\"]",
            "format!(\"metric:{code}\")",
        ] {
            assert!(src.contains(anchor), "默认销售合同或迁移清理缺失：{anchor}");
        }
        assert!(src.contains("let current_sales_codes = sales_fact::METRICS"));
        assert!(src.contains("if !stale_codes.iter().any(|stale| stale == code)"));
        assert_eq!(
            crate::sales_fact::Metric::GrossMargin.expression(),
            "SUM(gross_profit)/NULLIF(SUM(revenue_excluding_tax),0)"
        );
        assert_eq!(crate::sales_fact::Dimension::Customer.column(), "storename");
        assert_eq!(crate::sales_fact::Dimension::Customer.name(), "客户");
        assert_eq!(crate::sales_fact::Dimension::Region.column(), "region");
        assert_eq!(crate::sales_fact::Dimension::Region.name(), "省区");
    }

    /// 上面 `METRICS` 里既有指标的 名+别名。const 在函数体内、测试够不到，
    /// 只能抄一份 —— 与 `recall/metric.rs::refund_ratio_aliases_do_not_shadow_refund_amount`
    /// 同一处置：**改那边的别名必须改这里**（改漏了下面两条行为断言会红）。
    const OTHERS: &[(&str, &[&str])] = &[
        // 「订单额」曾漏抄（与 buyer_count 同族事故）——下方的覆盖断言就是抓这种漏抄的
        ("订单额", &["订单金额", "下单金额", "订单总额"]),
        ("订单数", &["订单量", "单量", "成交订单数", "多少单", "多少个订单", "多少订单", "几个订单", "几单", "订单笔数", "下了多少"]),
        ("订单客单价", &["客单价", "订单单均", "平均客单"]),
        ("市场费用", &["营销费用", "费用总额", "推广费"]),
        ("售后单数", &["退货数", "售后量", "退货单数", "售后单有多少", "多少售后", "几个售后单"]),
        ("退款额", &["售后退款金额", "售后退款", "退款金额", "售后金额"]),
        ("退款占比", &["退款率", "售后退款占比", "退款金额占比", "退款占销售额比例", "售后退款金额占销售额"]),
        ("库存量", &["库存数量", "存货量", "库存"]),
        ("库存金额", &["库存额", "存货金额", "库存价值"]),
        ("开票金额", &["开票额", "发票金额", "发票"]),
        ("活动费用", &["活动经费", "市场活动费用"]),
        ("活动场次", &["活动数量", "多少场活动", "办了多少活动"]),
        ("活动临促人员费用", &["活动临促人员的费用", "临促人员费用", "活动临促费用", "临促费用"]),
        ("活动执行人员费用", &["活动执行人员的费用", "执行人员费用", "活动执行费用"]),
        ("账户余额", &["账余", "帐余", "账户余额合计"]),
        ("信控余额", &["信控额度", "信控"]),
        // ⚠️ `buyer_count` 是前一轮补的指标，当时**漏了往这份抄本里加**——
        // 于是它的别名（含很容易撞的「客户数」）从来没被碰撞断言核过。这正是上面注释
        // 警告的那种漂，补上。
        ("成交客户数", &["下单客户数", "成交客户", "多少客户", "客户数"]),
    ];
    /// 动销商品数（`active_sku_count`），与 `METRICS` 末条同步
    const ACTIVE_SKU: (&str, &[&str]) =
        ("动销商品数", &["动销商品", "动销SKU", "卖出过的商品数", "有销量的商品数"]);
    /// 本轮新增的赠品箱数，与 `METRICS` 里 `gift_box_qty` 同步
    const GIFT: (&str, &[&str]) = ("赠品箱数", &["赠品", "送出去的赠品", "赠品数量", "赠品箱"]);

    /// 名+别名 → 命中并净化后保留的指标名（与 `recall_metric_hits` 同一套判据，只是省掉 DB）
    fn recalled(question: &str) -> Vec<String> {
        use dms_kernel::nl::text::{map_filter, match_word};
        let hits: Vec<(String, String)> = OTHERS
            .iter()
            .copied()
            .chain(crate::sales_fact::METRICS.iter().map(|metric| (metric.name(), metric.aliases())))
            .chain(std::iter::once(ACTIVE_SKU))
            .chain(std::iter::once(GIFT))
            .filter_map(|(n, al)| {
                let al: Vec<String> = al.iter().map(|s| s.to_string()).collect();
                match_word(question, n, &al).map(|w| (n.to_string(), w))
            })
            .collect();
        map_filter(&hits).into_iter().map(|i| hits[i].0.clone()).collect()
    }

    #[test]
    fn specialised_activity_fee_aliases_select_their_own_metric() {
        assert_eq!(
            recalled("今年活动临促人员的费用一共花了多少钱"),
            vec!["活动临促人员费用"]
        );
        assert_eq!(
            recalled("今年活动执行人员的费用总共多少"),
            vec!["活动执行人员费用"]
        );
        assert_eq!(recalled("今年活动费用是多少"), vec!["活动费用"]);
    }

    #[test]
    fn specialised_activity_fee_metric_contracts_are_seeded_without_dimensions() {
        let src = include_str!("seed_defs.rs");
        let metrics = src
            .split("const METRICS:")
            .nth(1)
            .expect("METRICS 不见了")
            .split("];")
            .next()
            .expect("METRICS 结束锚点不见了")
            .split_whitespace()
            .collect::<String>();
        for contract in [
            concat!("(\"activity_", "promoter_fee\",\"活动临促人员费用\""),
            concat!("\"t_activity_", "promoter_fee\",\"SUM(total_amount)\",\"deleted_flag=0\",\"created_time\""),
            concat!("(\"activity_", "execution_fee\",\"活动执行人员费用\""),
            concat!("\"t_market_activity_", "promoter_expense\",\"SUM(amount)\",\"deleted_flag=0\",\"created_time\""),
            concat!("(\"refund_", "amount\",\"退款额\",&[\"售后退款金额\""),
        ] {
            assert!(metrics.contains(contract), "指标契约缺失：{contract}");
        }

        let policies = src
            .split("const METRIC_POLICIES:")
            .nth(1)
            .expect("METRIC_POLICIES 不见了")
            .split("];")
            .next()
            .expect("METRIC_POLICIES 结束锚点不见了")
            .split_whitespace()
            .collect::<String>();
        for policy in [
            concat!("(\"activity_", "promoter_fee\",\"1\",&[])"),
            concat!("(\"activity_", "execution_fee\",\"1\",&[])"),
        ] {
            assert!(policies.contains(policy), "专项费用必须禁用维度：{policy}");
        }
    }

    /// 🔴 新指标「动销商品数」的名+别名与既有 15 条**交集为空**，且两条真实问法各只命中一条。
    /// 交集非空＝同一个命中词属于两条指标，谁赢由行序决定（`account_balance` 的裸「余额」
    /// 就是这么把 FIN02/FIN04 一起弄红的）。
    #[test]
    fn active_sku_count_collides_with_no_existing_metric() {
        let mine: Vec<&str> =
            std::iter::once(ACTIVE_SKU.0).chain(ACTIVE_SKU.1.iter().copied()).collect();
        let inter: Vec<&str> = OTHERS
            .iter()
            .flat_map(|(n, al)| std::iter::once(*n).chain(al.iter().copied()))
            .filter(|t| mine.contains(t))
            .collect();
        assert!(inter.is_empty(), "{inter:?}");
        // ① 新问法只命中新指标（GOODS15 原句）
        assert_eq!(recalled("2026年6月动销商品有多少个"), ["动销商品数"]);
        // ② 既有问法不被打扰：「本月销量」仍只命中销量
        assert_eq!(recalled("本月销量"), ["销量"]);
        // ③ 唯一一处单向子串（销量 ⊂ 有销量的商品数）由 MapFilter R3 化解：长命中词赢，
        //    问的是商品个数就只出这一张卡 —— 若哪天 R3 变了，这条会红。
        assert_eq!(recalled("有销量的商品数是多少"), ["动销商品数"]);
    }

    /// 🔴 口语问法必须召回到指标 —— **否则整套指标级口径静默缺席**。
    ///
    /// 实证（评测 SALE15）：「本月卖得最好的10个商品是哪些」一个指标都没召回，
    /// 于是五键去重 / `item_type='1'` / 时间列三条声明全部不生效，
    /// `RequireDedup` 也无从开火（它的 keys 来自被召回指标的 `dedup_keys`）。
    /// LLM 只靠表级 warn 自己拼，做了两键 DISTINCT ⇒ 销量**低报 5.6 倍**
    /// （首行 13045 而 gold 72863），而 `caliber_note` 全空、route 正常。
    ///
    /// 这一条钉的是「召回到」而不是「答对」：召回到之后判据链就接上了。
    #[test]
    fn colloquial_phrasings_recall_the_quantity_metric() {
        for q in [
            "本月卖得最好的10个商品是哪些", // SALE15 原句
            "最畅销的商品",
            "哪个商品最好卖",
            "本月卖得最多的商品",
        ] {
            assert_eq!(
                recalled(q),
                ["销量"],
                "口语问法召回不到指标 ⇒ 指标级口径（去重键/item_type/时间列）全部缺席：{q}"
            );
        }
        // 反向：问**金额**的口语不许被销量抢走（「卖了多少」在 sales_amt 的别名里）
        assert_eq!(recalled("本月卖了多少钱"), ["销售额"], "金额问法被销量抢了");
        // 既有问法不许被新别名打扰
        assert_eq!(recalled("本月销量"), ["销量"]);
        assert_eq!(recalled("本月卖了多少件"), ["销量"]);
    }

    /// 🔴 新指标「赠品箱数」：与既有各条**交集为空**，且**不许把销量那一族的问法抢走**。
    ///
    /// 它与销量共用同一张明细表、同一套去重键，只差 `item_type` 的码值 ——
    /// 正因为这么近，抢词的风险最高：`销量`的别名里有「卖了多少箱」，
    /// 我的别名里有「赠品箱」。谁赢由行序决定的话，「本月销量」有可能被算成赠品（数会变小、
    /// 而且 route 仍是 `direct-agg`，没有回炉机会）。
    #[test]
    fn gift_box_qty_does_not_steal_the_sales_qty_questions() {
        let mine: Vec<&str> = std::iter::once(GIFT.0).chain(GIFT.1.iter().copied()).collect();
        let inter: Vec<&str> = OTHERS
            .iter()
            .chain(std::iter::once(&ACTIVE_SKU))
            .flat_map(|(n, al)| std::iter::once(*n).chain(al.iter().copied()))
            .filter(|t| mine.contains(t))
            .collect();
        assert!(inter.is_empty(), "{inter:?}");
        // ① GOODS14 原句只命中新指标
        assert_eq!(recalled("2026年6月我们送出去的赠品有多少箱"), ["赠品箱数"]);
        // ② 销量那一族一个都不许被抢
        assert_eq!(recalled("本月销量"), ["销量"]);
        assert_eq!(recalled("2026年上半年卖了多少件"), ["销量"]);
        assert_eq!(recalled("本月各商品分类销量"), ["销量"]);
    }

    /// 从函数体内 const 段解析「行首元组」的（前两段引号串）：METRICS/MAPS 判据共用的解析器。
    /// 只认 `("` 开头且首段是全小写标识的行（表名/指标码），码值对（中文名, 码）不会误入。
    fn tuple_heads(block: &str) -> Vec<(&str, &str)> {
        block
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("(\""))
            .filter_map(|line| {
                let segs: Vec<&str> = line.split('"').skip(1).step_by(2).collect();
                let (a, b) = (segs.first()?, segs.get(1)?);
                // 首段必须是全小写 ASCII 标识（指标码/表名），滤掉续行的码值对
                (a.chars().all(|c| c.is_ascii_lowercase() || c == '_') && !a.is_empty())
                    .then_some((*a, *b))
            })
            .collect()
    }

    /// 🔴 `METRICS` 里每个 code 在 `METRIC_POLICIES` 里有且仅有一条（buyer_count 漏抄
    /// OTHERS 的事故在这份文件自己记录过 —— 两集合漂移无守卫就会再犯）。
    #[test]
    fn every_metric_has_exactly_one_policy() {
        let src = include_str!("seed_defs.rs");
        let metrics = src.split("const METRICS:").nth(1).expect("METRICS 不见了")
            .split("];").next().expect("METRICS 结束锚点不见了");
        let policies = src.split("const METRIC_POLICIES:").nth(1).expect("METRIC_POLICIES 不见了")
            .split("];").next().expect("METRIC_POLICIES 结束锚点不见了");
        let metric_codes: std::collections::HashSet<&str> =
            tuple_heads(metrics).into_iter().map(|(c, _)| c).collect();
        // policies 只取行首第一个引号段（第二段可能是 `sales_fact::VERSION` 常量而非字面量）
        let policy_codes: std::collections::HashSet<&str> = policies
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("(\""))
            .filter_map(|line| line.split('"').nth(1))
            .collect();
        assert!(metric_codes.len() >= 15, "METRICS 解析异常：{metric_codes:?}");
        // refund_ratio 不在 METRICS 常量里（单独 upsert），但在 POLICIES 里
        let expected: std::collections::HashSet<&str> =
            metric_codes.iter().copied().chain(std::iter::once("refund_ratio")).collect();
        assert_eq!(
            policy_codes, expected,
            "METRICS 与 METRIC_POLICIES 的 code 集合漂移（多退少补都在这红）"
        );
    }

    /// 🔴 `METRICS` 里每个指标名都必须进碰撞断言集（OTHERS/ACTIVE_SKU/GIFT）——
    /// 漏抄 = 它的别名从没被碰撞断言核过（buyer_count「客户数」就是这么漏的）。
    #[test]
    fn every_metric_name_is_in_collision_sets() {
        let src = include_str!("seed_defs.rs");
        let metrics = src.split("const METRICS:").nth(1).expect("METRICS 不见了")
            .split("];").next().expect("METRICS 结束锚点不见了");
        for (code, name) in tuple_heads(metrics) {
            let covered = OTHERS.iter().any(|(n, _)| *n == name)
                || ACTIVE_SKU.0 == name
                || GIFT.0 == name
                // 默认销售六指标由 crate::sales_fact::METRICS 喂进 recalled()，不走本地抄本
                || crate::sales_fact::METRICS.iter().any(|m| m.name() == name);
            assert!(covered, "指标 {code}「{name}」不在碰撞断言集里——别名撞词风险无测试盯着");
        }
    }

    /// MAPS 的（表, 列）条目不得逐字重复（重复段曾让每次启动多打 14 条冗余 upsert）。
    /// 唯一合法的一表两登记是 `paid_way`（eq 纯值 + like 组合值两种 match_kind）。
    #[test]
    fn value_map_entries_have_unique_table_column() {
        let src = include_str!("seed_defs.rs");
        let maps = src.split("const MAPS:").nth(1).expect("MAPS 不见了")
            .split("];").next().expect("MAPS 结束锚点不见了");
        let mut counts: std::collections::HashMap<(&str, &str), usize> = std::collections::HashMap::new();
        for (t, c) in tuple_heads(maps) {
            assert!(t.starts_with("t_"), "MAPS 解析错位：{t} 不像表名");
            *counts.entry((t, c)).or_default() += 1;
        }
        assert!(counts.len() >= 20, "MAPS 解析异常：{counts:?}");
        for (key, n) in &counts {
            let allowed = if *key == ("t_sales_order", "paid_way") { 2 } else { 1 };
            assert_eq!(*n, allowed, "MAPS 条目重复：{}.{}", key.0, key.1);
        }
    }

    /// DMS 后端源码校准补齐的 value_map 条目（码名以 Java 枚举/常量为准）：
    /// order_status 正名 / order_type 六值 / paid_status 三值 / after_sales_status 九档 /
    /// item_type「正品」正名 —— 与正名 DELETE 收敛清单。
    #[test]
    fn dms_calibration_value_maps_are_seeded() {
        let src = include_str!("seed_defs.rs");
        let maps = src.split("const MAPS:").nth(1).expect("MAPS 不见了")
            .split("];").next().expect("MAPS 结束锚点不见了")
            .split_whitespace().collect::<String>();
        for frag in [
            "(\"t_sales_order\",\"order_status\",crate::present_cn::SALES_ORDER_STATUS",
            "(\"线下销售\",\"SO01\")", "(\"设备\",\"SO04\")", "(\"样品\",\"SO10\")",
            "(\"样品领用\",\"SO12\")", "(\"营销物料\",\"SO15\")", "(\"积分兑换\",\"SO16\")",
            "(\"t_sales_order\",\"paid_status\",&[(\"未支付\",\"0\"),(\"已支付\",\"1\"),(\"支付中\",\"2\")]",
            "(\"待提交确认\",\"1\")", "(\"退款执行中\",\"8\")", "(\"退款失败\",\"9\")",
            "(\"t_sales_order_detail\",\"item_type\",&[(\"正品\",\"1\")",
        ] {
            assert!(maps.contains(frag), "校准码表条目缺失：{frag}");
        }
        // 旧名不许留在 MAPS 里（否则与正名两名一码）；DELETE 收敛清单必须在
        assert!(!maps.contains("(\"无效\",\"108\")") && !maps.contains("(\"作废\",\"199\")")
            && !maps.contains("(\"商品行\",\"1\")"), "旧名还在 MAPS 里");
        // 码表本体改由常量承载：这里断言它的完整性（原先只播 3 档，裸 `101` 就是这么漏的）
        let book = crate::present_cn::SALES_ORDER_STATUS;
        assert_eq!(book.len(), 16, "订单状态 16 档少了：{book:?}");
        for (name, code) in [("暂存", "0"), ("待备货", "101"), ("已取消", "108"), ("已删除", "199")] {
            assert!(book.contains(&(name, code)), "订单状态缺 {name}({code})");
        }
        assert!(!book.iter().any(|(n, _)| *n == "无效" || *n == "作废"), "旧名不许回到码表");

        // 客户分类码表：`seed_defs` 的 agg_expr 必须是 `&'static str`，折不出来，
        // 只能保留一份字面量 —— 那就用判据钉住它与码表逐档一致（漂了当场红）。
        let expr = src
            .split("(\"customer_class\", \"订单客户分类\"")
            .nth(1)
            .expect("订单客户分类维度没了");
        for (name, code) in crate::present_cn::CUSTOMER_CLASS {
            assert!(
                expr.contains(&format!("WHEN '{code}' THEN '{name}'")),
                "customer_class 的 SQL CASE 与 present_cn::CUSTOMER_CLASS 漂了：缺 {name}({code})",
            );
        }
        for old in ["(\"t_sales_order\", \"order_status\", \"无效\")",
                    "(\"t_sales_order\", \"order_status\", \"作废\")",
                    "(\"t_sales_order_detail\", \"item_type\", \"商品行\")"] {
            assert!(src.contains(old), "正名 DELETE 收敛清单缺：{old}");
        }
    }

    /// refund_ratio 的 agg_expr 内嵌 :begin/:end 占位符（全文件唯一自带占位符的指标）：
    /// 分母由 `sales_fact::metric_subquery` 生成 —— 钉住这条构造链不断。
    #[test]
    fn refund_ratio_keeps_placeholder_subquery() {
        // 纯函数侧：子查询必须带占位符（装配器替换 :begin/:end 的前提）
        let sub = crate::sales_fact::metric_subquery(crate::sales_fact::Metric::SalesAmount, ":begin", ":end");
        assert!(sub.contains(":begin") && sub.contains(":end"), "{sub}");
        // 构造链锚点：refund_ratio 的分母必须来自 metric_subquery（不许复制销售额 SQL）
        let src = include_str!("seed_defs.rs");
        assert!(
            src.contains("sales_fact::metric_subquery(SalesMetric::SalesAmount, \":begin\", \":end\")"),
            "refund_ratio 的分母构造链断了"
        );
    }
}
