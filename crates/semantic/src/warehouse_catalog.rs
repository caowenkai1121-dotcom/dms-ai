//! Doris 业务分层资产目录。
//!
//! 只登记已经核实、可进入运行时选表的静态库表。旧表、空表、陈旧副本只留在文档，
//! 不进入 `metadata_assets`，因此不能被元数据探针或 LLM 静默当作 fallback。
//!
//! ## A5 数仓目录快照与降级启动
//!
//! 公网链路下 information_schema 探针可能失败。`probe_with_fallback` 把「探针失败即
//! 拒绝启动」软化成三档：
//! - **成功**：先过 `validate_required_snapshot`（最低可用条件，不过按失败处理），把
//!   tables/columns 统计与目录内容摘要（排序表名清单 + 结构摘要的 FNV-1a 摘要值）upsert 进
//!   `meta.warehouse_catalog_snapshot`（按 target 一行、幂等建表、只留最近一次成功），
//!   返回 `trust = Authoritative` 且 `snapshot = Some(本次探针结果)`；
//! - **降级**：探针/校验失败但存在历史快照，返回 `trust = Degraded { snapshot_at }`、
//!   `snapshot = None`，`stats` 为快照当时的统计；
//! - **硬失败**：探针失败且无任何历史快照，Err（维持原 fail-closed 行为）。
//!
//! 快照落库前必过最低可用校验，因此 degraded 启动沿用的语义至少曾经完整。`trust` 标签
//! 随 `FallbackCatalog` 传给调用方（`CatalogTrust::as_str()` 供日志/健康检查透出）。
//!
//! ### 调用点改法（server 侧接线说明，本模块不改动 main.rs）
//!
//! `bootstrap_meta`（crates/server/src/main.rs）与 `meta sync` 子命令当前的硬失败点是
//! `mysql.probe_schema_with_warehouse_catalog(&assets).await.map_err(...)?`，改为把探针
//! 结果交给本模块裁决：
//! ```ignore
//! let probed = dms_semantic::warehouse_catalog::probe_with_fallback(
//!     pg,
//!     &target,
//!     mysql.probe_schema_with_warehouse_catalog(&assets)
//!         .await
//!         .map_err(|e| anyhow::anyhow!("数仓目录探针失败：{e}")),
//! )
//! .await?;
//! match probed.snapshot {
//!     Some(mut snapshot) => {
//!         // trust = Authoritative：现有流程原样 —— validate_required_snapshot（幂等重验）
//!         // → enrich_dms_snapshot → enrich_schema_snapshot → sync_schema → mark_synced，
//!         // stats 一律用 probed.stats。
//!     }
//!     None => {
//!         // trust = Degraded：跳过 sync_schema 与 mark_synced（版本标记不动，下次启动
//!         // 自动重试探针），沿用 PG 内既有 meta.table_doc/column_doc，并把
//!         // probed.trust / snapshot_at 写进启动日志与健康检查透出。
//!     }
//! }
//! ```

use dms_connector::mysql::{WarehouseAsset, WarehouseCatalogStats};
use dms_connector::source::{ColumnInfo, SchemaSnapshot};
use sqlx::types::chrono::{DateTime, Utc};
use sqlx::PgPool;

/// 目录内容、字段合同或禁用规则变化时必须递增。日期对齐本轮业务交接日，避免未来版本。
pub const VERSION: &str = "2026.08.06-warehouse-catalog-v1";
const VERSION_KEY: &str = "warehouse_catalog_version";

pub struct Asset {
    pub database: &'static str,
    pub table: &'static str,
    pub layer: &'static str,
    pub domain: &'static str,
    pub grain: &'static str,
    pub time_rule: &'static str,
    pub metrics: &'static str,
    pub forbidden: &'static str,
    pub comparison: &'static str,
}

macro_rules! asset {
    ($db:literal, $table:literal, $layer:literal, $domain:literal, $grain:literal,
     $time:literal, $metrics:literal, $forbidden:literal, $comparison:literal) => {
        Asset {
            database: $db,
            table: $table,
            layer: $layer,
            domain: $domain,
            grain: $grain,
            time_rule: $time,
            metrics: $metrics,
            forbidden: $forbidden,
            comparison: $comparison,
        }
    };
}

pub const ASSETS: &[Asset] = &[
    asset!("sales_dw", "dws_off_offline_sale_dfn", "DWS", "线下销售", "日期×客户×商品×战区×省区",
        "时间只用 order_date，默认查询必须限定范围",
        "字段合同：storecode/storename=客户编码/名称，skucode/skuname=商品编码/名称，war_zone=战区，region=省区；销售额=SUM(amount)；销量=SUM(qty)；不含税成本=SUM(cost_excluding_tax)；不含税收入=SUM(revenue_excluding_tax)；毛利额=SUM(gross_profit)；毛利率=SUM(gross_profit)/NULLIF(SUM(revenue_excluding_tax),0)",
        "禁止按行数或 id 推算订单数；禁止平均行毛利率；禁止再拼旧发货/退货 UNION；出货量/发货量属于独立物流事件，不是 qty 的同义词；禁止把客户名称解释成门店；禁止猜测本合同未登记的品牌、门店、业务员或商品分类字段",
        "允许：同口径、同权限、同长度完整周期按 order_date 做同比/环比；不完整当期必须标记"),
    asset!("sales_ads", "ads_off_offline_region_sale_dfn", "ADS", "省区经营", "月×战区×省区×省份×商品大类",
        "时间用 data_month；月目标只在已确认粒度比较",
        "省区月目标、分摊损益和费用专题；实际销售额/销量/成本/收入/毛利仍从默认销售 DWS 取数",
        "禁止用本表金额替代默认销售事实；禁止把月目标跨商品大类重复求和；禁止与 DWS 明细再次叠加",
        "条件允许：目标或已验收分摊指标按 data_month 比较；实际销售同比/环比必须回默认销售 DWS"),
    asset!("sales_ads", "ads_off_sales_cost_region_dnf", "ADS", "销售费用", "月×战区×省区",
        "时间用 data_month",
        "省区销售费用十类结构、表内配套销售金额、费销比",
        "禁止把 amount 当费用；禁止再关联费用明细放大；禁止把费销比跨省求和",
        "允许：同费用分类、同省区粒度按 data_month 做同比/环比"),
    asset!("sales_ads", "ads_off_sales_cost_customer_dnf", "ADS", "销售费用", "月×客户×经理×部门",
        "时间用 data_month",
        "客户销售费用十类结构、表内配套销售金额、费销比",
        "禁止把 amount 当费用；禁止用本表 amount 替代默认销售额；禁止宽表 SUM(*)",
        "允许：同客户/分类粒度按 data_month 做同比/环比"),
    asset!("sales_dw", "dws_off_sales_cost_dnf", "DWS", "销售费用", "月×客户×经理×部门",
        "时间用 data_month",
        "客户、经理、部门维度的销售费用分类列",
        "禁止 SUM(*)；必须点名费用分类；禁止把表内配套销售额混入费用合计",
        "允许：同费用分类、同粒度按 data_month 做同比/环比"),
    asset!("sales_dw", "dws_off_sales_cost_notshare_dnf", "DWS", "销售费用", "日期×费用单据×客户×费用三级分类",
        "时间用 data_date",
        "未分摊费用不含税金额、单据数、客户和费用分类结构",
        "禁止代表全部销售费用；禁止与已汇总费用表重复相加",
        "允许：仅对未分摊费用按 data_date 做同口径同比/环比"),
    asset!("sales_dw", "dws_off_shop_cost_dnf", "DWS", "门店费用", "日期×费用单据×客户×门店×费用项",
        "时间用 data_date",
        "真实门店费用、报销/核销金额、费用项和地域结构",
        "禁止用客户名称冒充门店；禁止与汇总费用表重复相加",
        "允许：同门店、同费用项按 data_date 做同比/环比"),
    asset!("sales_dw", "dws_off_activity_promoter_fin", "DWS", "市场活动", "活动×客户×门店×起止日期",
        "统计日用 data_date；活动周期用 start_date/end_date，二者不可混用",
        "活动费用、实际销售、费率、活动天数和活动/门店结构",
        "daysale 语义未确认，禁用；禁止把活动实际销售替代默认销售额",
        "条件允许：只在同一时间语义和同一活动状态范围内比较"),
    asset!("sales_dw", "dws_off_management_cost_dnf", "DWS", "管理费用", "月×单据×人员×部门",
        "时间用 data_date，并按月粒度解释",
        "管理费用合计、费用大类、人员、部门和单据结构",
        "total_management_cost 已是合计，禁止再加分项；sales_amount 不替代默认销售额",
        "允许：同部门/费用类按月做同比/环比"),
    asset!("sales_dw", "dws_off_management_cost_detail_dnf", "DWS", "管理费用", "日期×报销单×人员×部门×费用类型",
        "时间用 data_date",
        "管理费用明细金额、报销单数、人员/部门/费用类型结构",
        "禁止与管理费用汇总表重复叠加；禁止不分费用类型的宽表相加",
        "允许：同费用类型按 data_date 做同比/环比"),
    asset!("sales_ads", "ads_off_dept_management_cost_dnf", "ADS", "管理费用", "月×部门",
        "时间用 data_month",
        "部门管理费用合计、费用结构、管理费率",
        "total_management_cost 已是合计，禁止与分项再相加；管理费率禁止求和",
        "允许：同部门按 data_month 做同比/环比"),
    asset!("sales_ads", "ads_off_customer_device_efficiency_dnf", "ADS", "设备效率", "客户×地域×设备类型",
        "无时间字段，只表示当前快照",
        "客户设备数、鲜食销售贡献和设备类型结构",
        "禁止趋势、同比、环比；禁止把设备销售贡献替代默认销售额",
        "禁止：无可比时间轴"),
    asset!("sales_ads", "ads_off_shop_device_efficiency_dnf", "ADS", "设备效率", "客户×门店×地域×设备类型",
        "无时间字段，只表示当前快照",
        "门店设备数、烤肠/蛋挞销售效率和设备类型结构",
        "禁止趋势、同比、环比；真实门店只用 shop_code/shop_name",
        "禁止：无可比时间轴"),
    asset!("sales_ads", "ads_off_mmhm_device_requirement_dnf", "ADS", "设备需求", "需求单×门店×设备",
        "未登记统一默认时间列，按明确的申请/发货/收货事件时间查询",
        "设备需求数、申请数、未发货数、收货数和单据明细",
        "禁止猜测时间列；禁止汇总联系方式/地址等敏感字段",
        "禁止默认同比/环比；只有明确同一事件时间后才可比较"),
    asset!("sales_dw", "dwd_off_baigeyun_device_delivery_item_scd", "DWD", "设备定位", "设备×拉链有效期",
        "当前状态必须 is_current=1；历史用 start_date/end_date",
        "去重设备数、当前位置、在线状态和历史有效期",
        "禁止把拉链版本数当设备数；禁止把历史版本与当前版本混加",
        "条件允许：历史比较必须按有效期重建同一时点快照"),
    asset!("sales_dw", "dwd_off_pos_sales_min", "DWD", "KA POS", "月×客户×门店名称×SKU",
        "月度时间列名尚未在运行时代码验收，禁止自动生成时间过滤",
        "POS 销量、POS 销售额、客户/门店名称/SKU 结构",
        "POS 门店无稳定 shop_code；禁止与 DMS sell-in 销售额混称或相加",
        "禁止默认同比/环比，待月度时间列验收"),
    asset!("sales_ads", "ads_off_new_product_sales_dnf", "ADS", "新品销售", "日期×客户×商品×区域×经理",
        "业务日期列名尚未在运行时代码验收，禁止自动生成时间过滤",
        "新品销售额、总数量、箱数、渠道和区域表现",
        "qty 与 box_qty 单位不同，禁止混加；经理姓名不是稳定人员键",
        "禁止默认同比/环比，待业务日期列验收"),
    asset!("sales_ads", "ads_off_offline_new_goods_sale_dfn", "ADS", "新品销售", "日期×客户×新品×区域×渠道",
        "统一时间列尚未验收，只接受明确日期列的查询",
        "新品销售、市场份额、铺市率和渠道结构",
        "市场份额/铺市率禁止求和；storecode 按客户解释；禁止自动猜时间列",
        "禁止默认同比/环比，待统一时间列验收"),
    asset!("sales_dw", "dws_off_region_sales_plan_min", "DWS", "销售预测", "月×省区×商品",
        "时间用 date_month 字符串月份",
        "实际销量、计划销量、数量偏差",
        "禁止把数量偏差百分比跨 SKU 求和；本表没有已验收预测准确率",
        "允许：同商品/省区按 date_month 做月度同比/环比"),
    asset!("sales_ads", "ads_off_region_sales_plan_min", "ADS", "销售预测", "月×省区",
        "时间用 date_month 字符串月份",
        "实际销量、计划销量、偏差、预测准确率",
        "qty_diffent_persent 为已计算比例，禁止跨省求和",
        "允许：按省区和月份展示同比/环比，比例按原粒度比较"),
    asset!("sales_dw", "dws_off_sales_bonus_detail_dnf", "DWS", "销售激励", "月×经理",
        "时间用 data_month（YYYYMM）",
        "新品激励销量、排名、触发条件、奖金",
        "排名、触发条件、销量、奖金不可混加；经理姓名不是稳定人员键",
        "允许：同奖金/销量指标按 data_month 比较；排名只比较不求和"),
    asset!("sales_dw", "dws_off_storeprice_dnf", "DWS", "客户价格", "客户×商品",
        "无生效时间，只表示当前价格",
        "当前渠道价、标准价、箱规、条码和价格状态",
        "禁止价格历史、调价次数、同比、环比；禁止把价格乘事实数量冒充销售额",
        "禁止：无价格历史时间轴"),
    asset!("sales_dw", "dws_off_third_party_sales_dnf", "DWS", "第三方销售", "日期×客户×商品×区域×品牌",
        "时间用 order_date",
        "第三方产品销量、销售额、品牌和区域结构",
        "禁止与默认自有线下销售混加；禁止把第三方品牌维度回填默认销售事实",
        "允许：同第三方产品范围按 order_date 做同比/环比"),
    asset!("sales_dw", "dws_off_msy_skuinfor_min", "DWS", "市场竞争", "日期×省区×外部商品",
        "统一时间列尚未验收，查询必须显式指定已登记日期列",
        "外部监测销售额、价格、品牌、口味和排名",
        "禁止与自有销售额相加；排名和价格禁止求和；禁止自动猜时间列",
        "禁止默认同比/环比，待统一时间列验收"),
    asset!("sales_dw", "dws_sales_state_sales_dnf", "DWS", "出库销售", "出库日期×销售单×SKU×仓库×客户",
        "时间用 created_date；必须限定日期或精确单号",
        "出库数量 ship_qty、销售单数、仓库/渠道/状态结构",
        "表量大，禁止无时间扫描；当前合同没有已验收的出库销售额公式，禁止把出库数量、订单金额或相似 amount 字段称为默认销售额",
        "允许：同出库状态口径按 created_date 做同比/环比"),
    asset!("sales_dw", "dws_fin_shipment_check_dnf", "DWS", "财务对账", "DMS销售单×中台出库单×Base来源单",
        "时间用 ship_at，单号查询优先",
        "拆单映射、行数差异、金额差异、换品异常数",
        "禁止把任一对账金额当销售额；禁止把一对多映射金额重复汇总",
        "条件允许：仅对同类对账异常数/差异额按 ship_at 比较"),
    asset!("sales_dw", "dws_fin_receivable_check_dnf", "DWS", "财务对账", "中台出库单×应收单×金蝶单",
        "时间用 data_date，并展示数据新鲜度",
        "应收链路行数/金额差异、缺单和映射异常",
        "禁止与销售事实合并统计经营销售额；禁止忽略更新时间差异",
        "条件允许：只比较同系统链路、同异常定义的周期结果"),
    asset!("sales_dw", "dws_fin_receivable_adjust_check_dnf", "DWS", "财务对账", "DMS调整单×中台调整单×金蝶应收单",
        "时间用 bizdate，单号查询优先",
        "调整单行数/金额差异和缺单异常",
        "禁止与普通销售订单、普通应收或经营销售额混算",
        "条件允许：同调整单类型按 bizdate 比较"),
    asset!("sales_dw", "dws_off_app_distribution_inventory_dfn", "DWS", "小程序经销存", "日期×订单×门店×商品",
        "时间用 order_date",
        "需求、发货、签收、退货数量；订单数按 order_no 去重",
        "禁止与陈旧 dws_mkt 同结构表 UNION；ruturn_qty 为已知物理拼写；禁止把数量当销售额",
        "允许：同事件数量按 order_date 做同比/环比"),
    asset!("sales_dw", "dws_mkt_app_place_order_dnf", "DWS", "小程序下单", "统计日×客户",
        "必须按 data_date 取最新快照；同行含当日与当月累计",
        "最新快照中的当日/当月微信支付、账余支付、总下单和取消订单",
        "禁止跨 data_date SUM 累计列；禁止混加当日值与月累计值",
        "条件允许：只比较各周期末最新快照的同名指标"),
    asset!("sales_dw", "dws_mkt_sampleorder_infor_dnf", "DWS", "样品单", "统计日×客户",
        "必须按 data_date 取最新快照",
        "最新快照中的本月/上月订单与样品单数量、金额",
        "禁止跨统计日求和月累计列；样品单金额不得替代销售额",
        "条件允许：仅使用同一最新快照内已定义的本月/上月对比"),

    asset!("fin_dw", "dws_receivable_sale_min", "DWS", "应收销售", "客户×SKU应收销售行",
        "时间用 data_date 字符串；先确认格式并限定范围",
        "应收数量、含税金额、不含税金额",
        "禁止称为发货销售额或经营销售额；禁止与经营销售事实相加；应收金额只能回答明确的应收事件",
        "条件允许：data_date 格式和完整性确认后做同应收口径同比/环比"),
    asset!("fin_ads", "ads_fin_receivable_dnf", "ADS", "应收经营", "月×客户×SKU×区域分类",
        "时间用 data_month",
        "月度应收金额、计价数量、成本结构",
        "禁止替代默认销售额；禁止把应收金额与销售额、订单额混加",
        "允许：同应收口径按 data_month 做同比/环比"),
    asset!("fin_ads", "ads_fin_profit_loss_dnf", "ADS", "财务损益", "月×省区×城市×客户",
        "时间用 data_month，并展示冻结月份/更新时间",
        "收入、成本、毛利、管理费用、销售费用、税前利润、净利润",
        "禁止替代实时销售事实；禁止与鲜食/冻品子表重复叠加；比例禁止求和",
        "允许：同分摊规则、同冻结口径按 data_month 做同比/环比"),
    asset!("fin_ads", "ads_fin_profit_loss_fresh_dnf", "ADS", "鲜食损益", "月×省区×城市×客户",
        "时间用 data_month，并展示冻结月份/更新时间",
        "鲜食收入、成本、毛利、费用、税前/净利润",
        "禁止与总损益或冻品损益重复叠加；禁止替代实时销售额",
        "允许：同鲜食分摊规则按 data_month 做同比/环比"),
    asset!("fin_ads", "ads_fin_profit_loss_frozen_dnf", "ADS", "冻品损益", "月×省区×城市×客户",
        "时间用 data_month，并展示冻结月份/更新时间",
        "冻品收入、成本、毛利、费用、税前/净利润",
        "禁止与总损益或鲜食损益重复叠加；禁止替代实时销售额",
        "允许：同冻品分摊规则按 data_month 做同比/环比"),
    asset!("fin_ads", "ads_fin_receivable_agg_sku_m", "ADS", "应收经营", "月×标准省区×分类×SKU",
        "时间用 data_month",
        "应收 SKU 金额、计价销量、成本和分类结构",
        "禁止称为经营净销售额；禁止与默认销量或销售成本静默混用",
        "允许：同应收计价口径按 data_month 做同比/环比"),
    asset!("fin_dw", "dws_fin_customer_balance_dnf", "DWS", "客户余额", "客户×期间×余额类型",
        "余额是期间快照；明确 time_period，未指定时按 data_date 取最新期间",
        "可开票、不可开票、信控、市场费等期末余额",
        "禁止跨期间 SUM 余额；禁止把余额、应收、销售额互相替代；字段公式未验收前不自动迁移 ODS 指标",
        "条件允许：只比较相同余额类型的期末快照，差额不得解释为期间流量"),
    asset!("fin_dw", "dws_fin_credit_balance_dnf", "DWS", "信控余额", "日期×客户×余额类型",
        "按 data_date 取客户×类型最新快照",
        "信控额度、信控余额和关联销售单信息",
        "禁止跨日期 SUM 余额；sales_amount 是配套字段，不替代默认销售额",
        "条件允许：只比较相同类型的期末快照"),
    asset!("fin_dw", "dws_fin_terminal_system_fees_fin", "DWS", "终端费用", "终端费用单×客户×门店",
        "时间用 reimbursement_time",
        "终端陈列费、核销金额、单据数和客户/门店结构",
        "禁止把终端费用代表全部市场费用；禁止展示敏感人员和地址字段",
        "允许：同费用类型按 reimbursement_time 做同比/环比"),
    asset!("hr_dw", "dws_hr_city_manger_min", "DWS", "组织映射", "月×省区×省份×城市×经理",
        "时间用 data_month",
        "城市经理与区域的月度映射、覆盖城市/省区数量",
        "禁止用经理姓名作稳定人员键或权限键；禁止从映射表汇总销售业绩",
        "条件允许：只比较组织映射变化，不生成经营 KPI 同比/环比"),

    asset!("dms_ods", "t_sales_order", "ODS", "订单", "订单头，一单一 sales_order_code",
        "时间用 order_time",
        "有效订单数、订单额、成交客户数、订单客单价、订单状态结构",
        "必须过滤 deleted_flag=0 和无效状态；订单额不是销售额；禁止与明细 JOIN 后重复 SUM 订单额",
        "允许：同订单状态口径按 order_time 做同比/环比"),
    asset!("dms_ods", "t_sales_order_detail", "ODS", "订单明细", "订单×行类型×商品行",
        "业务统计时间来自订单头；created_time/delivery_time 只按明确事件使用",
        "商品需求量、箱数、订单行金额、赠品和动销商品数",
        "禁止用行数当订单数；禁止忽略 item_type；禁止与物流多批次 JOIN 后放大",
        "条件允许：必须与同口径订单头时间窗配套后比较"),
    asset!("dms_ods", "t_sales_order_logistics", "ODS", "订单物流", "物流批次明细，一单可多批",
        "时间用 delivery_time",
        "实际发货批次、发货数量、仓库、出库单、签收结构",
        "禁止把物流行数当订单数；禁止汇总订单头金额；禁止大范围无时间查询",
        "允许：同发货事件口径按 delivery_time 做同比/环比"),
    asset!("dms_ods", "t_after_sales_order_header", "ODS", "售后", "售后单头，一单一 after_sales_code",
        "时间用 after_sales_time",
        "售后单数、申请退款额、实际退款额、售后类型/状态",
        "必须区分 refund_amount 与 actual_refund_amount；默认销售事实已含退货负数，禁止再次冲减",
        "允许：同退款定义和状态范围按 after_sales_time 做同比/环比"),
    asset!("dms_ods", "t_after_sales_order_detail", "ODS", "售后明细", "售后单×商品行",
        "按问题使用 created_time 或 delivery_time，不可互换",
        "SKU退款金额、申请数量、实退数量及单位结构",
        "箱/袋/统一数量禁止混加；禁止 JOIN 单头后重复汇总头金额",
        "条件允许：明确事件时间和数量单位后比较"),
    asset!("dms_ods", "t_winc_stock_report", "ODS", "经销商库存", "库存日期×客户×仓库×SKU快照行",
        "默认 product_stock_date=全表最大日期；重复版本未验收前需披露边界",
        "当前库存量、库存金额和客户/仓库/SKU结构",
        "禁止跨日期 SUM 快照；禁止把负库存截为0；禁止用相似库存表补缺",
        "条件允许：只比较同粒度、已处理重复版本的两个期末快照"),
    asset!("dms_ods", "t_winc_sale_report", "ODS", "经销商销售上报", "上报日期×客户×门店×商品",
        "按问题使用 stat_date 或 bill_date，不可互换",
        "经销商 sell-through 数量、金额和客户/门店结构",
        "禁止与 DMS sell-in 默认销售额相加或混称；禁止自动猜日期语义；\
         DMS 销售问题（销售额/销量/按省区按客户等）禁止用本表推导——口径不同源：推导走 t_sales_order(+t_sales_order_detail)，\
         本表仅在用户明确问 WinC/营销通/经销商上报流水时使用",
        "条件允许：明确同一日期语义后做同比/环比"),
    asset!("dms_ods", "t_customer", "ODS", "客户主数据", "一客户一 customer_code",
        "当前主数据，无经营时间轴",
        "客户档案、类型、分类、渠道、区域和启停状态",
        "禁止汇总敏感字段；禁止从主数据推算销售额/余额；默认展示必须脱敏和按权限过滤",
        "禁止默认同比/环比；历史变化需专门快照资产"),
    asset!("dms_ods", "t_goods", "ODS", "商品主数据", "一商品一 goods_code",
        "当前主数据，无经营时间轴",
        "商品名称、品牌、条码、上下架和说明",
        "goods_category_name 覆盖不足，禁止作为销售分类 fallback；禁止从主数据推算销量/销售额",
        "禁止默认同比/环比；历史变化需专门快照资产"),
    asset!("dms_ods", "t_master_shop", "ODS", "门店主数据", "一门店一 shop_code，归属 customer_code",
        "当前主数据，必须过滤删除状态",
        "门店档案、客户归属、地域、面积和门店类型",
        "monthly_sales/area 覆盖不足，禁止默认计算销售额、店效、坪效、人效",
        "禁止默认同比/环比；历史变化需专门快照资产"),
    asset!("dms_ods", "t_activity_main", "ODS", "市场活动", "活动单头，一活动一 activity_no",
        "时间用 created_time；状态范围必须由问题明确",
        "活动场次、活动申请金额和状态结构",
        "禁止默认替用户选择状态；活动申请金额不等于已核销费用",
        "允许：同状态范围按 created_time 做同比/环比"),
    asset!("dms_ods", "t_activity_promoter_fee", "ODS", "活动临促费用", "活动×临促费用行",
        "时间用 created_time",
        "临促人员费用 total_amount",
        "禁止按退化的 person_type/is_expense 猜过滤；禁止 JOIN 活动明细造成扇出",
        "允许：同临促费用口径按 created_time 做同比/环比"),
    asset!("dms_ods", "t_market_activity_promoter_expense", "ODS", "活动执行费用", "活动×执行费用行",
        "时间只用 created_time；activity_date 不可用",
        "活动执行人员费用 amount",
        "禁止使用空的 activity_date；禁止把执行人员费用代表全部活动费用",
        "允许：同执行费用口径按 created_time 做同比/环比"),
    asset!("dms_ods", "t_customer_balance", "ODS", "客户余额", "客户×余额类型×滚动快照",
        "每个 customer_code×balance_type 按 created_time DESC,id DESC 取最新一行",
        "账户余额、信控余额及其他余额类型的最新余额",
        "禁止直接 SUM 历史流水；禁止混合余额类型；禁止把余额差额当销售额或现金流",
        "禁止默认同比/环比；只有明确历史时点且可重建快照时才比较"),
    asset!("dms_ods", "t_invoice_apply_header", "ODS", "开票", "旧开票流申请单头",
        "时间用 apply_time",
        "已开票金额（invoice_status='2'）和申请单数",
        "单查旧流会严重漏算；禁止把申请中/失败计入；必须与新流 UNION ALL 才是总开票",
        "条件允许：新旧两流使用相同状态和 apply_time 窗口后比较"),
    asset!("dms_ods", "t_invoice_new_apply_header", "ODS", "开票", "新开票流申请单头",
        "时间用 apply_time",
        "已开票金额（invoice_status='2'）和申请单数",
        "单查新流会漏存量；禁止把申请中/失败计入；必须与旧流 UNION ALL 才是总开票",
        "条件允许：新旧两流使用相同状态和 apply_time 窗口后比较"),
];

/// 库名是资产自身合同，不能由 DWS/ADS 层级推断。
pub const fn database_of(asset: &Asset) -> &'static str {
    asset.database
}

/// 元数据探针只接收这份编译期静态白名单，不接受请求参数或动态枚举。
pub fn metadata_assets() -> Vec<WarehouseAsset> {
    ASSETS
        .iter()
        .map(|asset| WarehouseAsset::new(database_of(asset), asset.table))
        .collect()
}

/// target 归一（trim + 小写）：版本标记、快照草稿、快照读取三处同一形态，别开第四份。
fn normalize_target(target: &str) -> String {
    target.trim().to_ascii_lowercase()
}

/// 🔴 与 `mark_synced` 的 `requested` 入参是同一口径的两端：调用方（main.rs 启动序）传的是
/// 探针 `stats.requested`（资产条数）。两端相等仅靠「基础表名跨库唯一」（下方目录测试钉住）——
/// 一旦出现跨库同名表，去重数 < 资产数，版本标记永不匹配 → 每次启动都全量探针。改任一端先读这条。
fn requested_tables() -> usize {
    ASSETS
        .iter()
        .map(|asset| asset.table.to_ascii_lowercase())
        .collect::<std::collections::HashSet<_>>()
        .len()
}

/// 🔴 单 ds 前提：`tables`/`missing` 来自按 ds_id 过滤的 table_doc 计数，`requested` 却是
/// 数仓全局白名单数 —— 多 ds 部署下两端作用域不同，标记永远不等（永久重同步）。
/// 当前部署模型是单 DMS 数仓；要上多 ds，先把标记两端统一成同一作用域。
fn version_marker(target: &str, requested: usize, tables: usize, missing: usize) -> String {
    format!(
        "{VERSION}|target={}|requested={requested}|tables={tables}|missing={missing}",
        normalize_target(target)
    )
}

/// 版本一致且上次成功采集的目录行仍完整存在时，日常重启不再访问 information_schema。
/// 物理上缺失的可选资产不会写入 table_doc，相关问数由活性门禁 fail-closed；缺失未归零时
/// 标记永远不视为完成，因此后续启动会继续做白名单 information_schema 探针并自动发现补表。
pub async fn needs_sync(pg: &PgPool, ds: &str, target: &str) -> anyhow::Result<bool> {
    // ds:any —— meta.kv 是全局版本标记表（无 ds_id 列），标记是数仓全局状态，不按源切
    let marker: Option<(String,)> =
        sqlx::query_as("SELECT v FROM meta.kv WHERE k = $1")
            .bind(VERSION_KEY)
            .fetch_optional(pg)
            .await?;
    let names = ASSETS.iter().map(|asset| asset.table.to_string()).collect::<Vec<_>>();
    // table_name 与目录名（全小写）统一按小写比：大小写混存的行不错漏（否则 default_ready 永假）
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name, custom_comment FROM meta.table_doc
         WHERE ds_id = $1 AND lower(table_name) = ANY($2)",
    )
    .bind(ds)
    .bind(&names)
    .fetch_all(pg)
    .await?;
    let default_ready = rows
        .iter()
        .any(|(table, _)| table.eq_ignore_ascii_case(crate::sales_fact::TABLE_NAME));
    let comments_ready = rows.iter().all(|(_, comment)| !comment.trim().is_empty());
    let expected = version_marker(target, requested_tables(), rows.len(), 0);
    let marker_match = marker.as_ref().is_some_and(|(value,)| value == &expected);
    let needs = !default_ready || !comments_ready || !marker_match;
    if needs {
        // 三扇否决门任一挡住都返 true：是哪扇必须可观测，否则「为什么又全量探针」无从排查
        tracing::debug!(default_ready, comments_ready, marker_match, "目录快照未就绪，本轮做全量探针");
    }
    Ok(needs)
}

/// 默认销售事实是目录的最低可用条件：表或任一业务确认字段缺失都不允许服务启动。
pub fn validate_required_snapshot(snapshot: &SchemaSnapshot) -> anyhow::Result<()> {
    anyhow::ensure!(
        snapshot
            .tables
            .iter()
            .any(|table| table.name.eq_ignore_ascii_case(crate::sales_fact::TABLE_NAME)),
        "数仓目录缺少默认销售事实 {}",
        crate::sales_fact::TABLE
    );
    let available = snapshot
        .columns
        .iter()
        .filter(|(table, _)| table.eq_ignore_ascii_case(crate::sales_fact::TABLE_NAME))
        .map(|(_, column)| column.name.to_ascii_lowercase())
        .collect::<std::collections::HashSet<_>>();
    let missing = crate::sales_fact::contract_columns()
        .filter(|column| !available.iter().any(|c| c.eq_ignore_ascii_case(column)))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        missing.is_empty(),
        "默认销售事实 {} 缺少业务确认字段：{}",
        crate::sales_fact::TABLE,
        missing.join(", ")
    );
    Ok(())
}

/// 只在探针、schema 同步和完整 seed 全部成功后写入；失败时旧标记不动，下次启动重试。
pub async fn mark_synced(
    pg: &PgPool,
    target: &str,
    requested: usize,
    tables: usize,
    missing: usize,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO meta.kv(k, v) VALUES ($1, $2)
         ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v",
    )
    .bind(VERSION_KEY)
    .bind(version_marker(target, requested, tables, missing))
    .execute(pg)
    .await?;
    Ok(())
}

// ── A5 数仓目录快照与降级 ─────────────────────────────────────────────

/// 目录可信度标签，随 `FallbackCatalog` 传给调用方。
/// `Authoritative` = 本次实时探针；`Degraded { snapshot_at }` = 历史快照兜底。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogTrust {
    Authoritative,
    Degraded { snapshot_at: DateTime<Utc> },
}

impl CatalogTrust {
    /// 日志与健康检查透出的稳定小写标签。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Authoritative => "authoritative",
            Self::Degraded { .. } => "degraded",
        }
    }
}

/// `probe_with_fallback` 的三档裁决结果。`stats` 三档都有值（degraded 时为快照当时
/// 统计）；`snapshot` 仅 authoritative 为 `Some` —— 调用方凭 `snapshot.is_none()`
/// 跳过 sync_schema/mark_synced，沿用 PG 内既有目录。
#[derive(Debug)]
pub struct FallbackCatalog {
    pub trust: CatalogTrust,
    pub stats: WarehouseCatalogStats,
    pub snapshot: Option<SchemaSnapshot>,
}

/// `meta.warehouse_catalog_snapshot` 的一行：最近一次成功探针的统计与目录内容摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSnapshot {
    pub target: String,
    pub version: String,
    pub probed_at: DateTime<Utc>,
    pub stats: WarehouseCatalogStats,
    /// 排序后的物理表名清单（目录内容摘要的可读部分）。
    pub table_names: Vec<String>,
    /// 结构规范的 FNV-1a 摘要值（表名/注释 + 列名/类型/序号/注释；行数估值不参与，
    /// 避免每次探针都因估算抖动而漂移）。跨版本确定性，仅用于变化观察，不作信任裁决。
    pub digest: String,
}

/// 待写入的快照草稿（`probed_at` 由 PG `now()` 赋值，Rust 侧不造时间）。
#[derive(Debug)]
struct SnapshotDraft {
    target: String,
    version: String,
    stats: WarehouseCatalogStats,
    table_names: Vec<String>,
    digest: String,
}

/// 幂等建表。save/load 入口各自确保，不依赖 `ddl::migrate` 的执行顺序。
/// 进程内只发一次 DDL（并发首发可能重复 —— `IF NOT EXISTS` 幂等，无害）。
async fn ensure_snapshot_table(pg: &PgPool) -> anyhow::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static READY: AtomicBool = AtomicBool::new(false);
    if READY.load(Ordering::Relaxed) {
        return Ok(());
    }
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS meta.warehouse_catalog_snapshot(
           target text PRIMARY KEY,
           version text NOT NULL,
           probed_at timestamptz NOT NULL DEFAULT now(),
           n_requested bigint NOT NULL,
           n_tables bigint NOT NULL,
           n_columns bigint NOT NULL,
           n_missing bigint NOT NULL,
           table_names text[] NOT NULL,
           digest text NOT NULL
         )",
    )
    .execute(pg)
    .await?;
    READY.store(true, Ordering::Relaxed);
    Ok(())
}

/// FNV-1a 64：零依赖、跨版本确定的摘要（`std` 的 DefaultHasher 不保证跨版本稳定）。
/// pub(crate)：`autodiscover::register` 的 dim_code 截断哈希复用同一份。
pub(crate) fn fnv1a64(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 从探针结果构建快照草稿：表名清单排序（大小写不敏感），摘要对输入顺序免疫。
fn snapshot_draft(
    target: &str,
    snapshot: &SchemaSnapshot,
    stats: WarehouseCatalogStats,
) -> SnapshotDraft {
    let mut tables = snapshot.tables.iter().collect::<Vec<_>>();
    tables.sort_by_cached_key(|table| table.name.to_ascii_lowercase());
    let mut columns_by_table: std::collections::HashMap<String, Vec<&ColumnInfo>> =
        std::collections::HashMap::new();
    for (table, column) in &snapshot.columns {
        columns_by_table
            .entry(table.to_ascii_lowercase())
            .or_default()
            .push(column);
    }
    use std::fmt::Write as _;
    let mut canonical = String::new();
    for table in &tables {
        let key = table.name.to_ascii_lowercase();
        let _ = write!(canonical, "table|{key}|{}\n", table.comment);
        if let Some(columns) = columns_by_table.get_mut(&key) {
            columns.sort_by_cached_key(|column| (column.ordinal, column.name.to_ascii_lowercase()));
            for column in columns {
                let _ = write!(
                    canonical,
                    "column|{}|{}|{}|{}\n",
                    column.name.to_ascii_lowercase(),
                    column.data_type,
                    column.ordinal,
                    column.comment
                );
            }
        }
    }
    SnapshotDraft {
        target: normalize_target(target),
        version: VERSION.to_string(),
        stats,
        table_names: tables.iter().map(|table| table.name.clone()).collect(),
        digest: fnv1a64(&canonical),
    }
}

async fn persist_draft(pg: &PgPool, draft: &SnapshotDraft) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO meta.warehouse_catalog_snapshot(
           target, version, probed_at, n_requested, n_tables, n_columns, n_missing, table_names, digest)
         VALUES ($1, $2, now(), $3, $4, $5, $6, $7, $8)
         ON CONFLICT (target) DO UPDATE SET
           version = EXCLUDED.version, probed_at = EXCLUDED.probed_at,
           n_requested = EXCLUDED.n_requested, n_tables = EXCLUDED.n_tables,
           n_columns = EXCLUDED.n_columns, n_missing = EXCLUDED.n_missing,
           table_names = EXCLUDED.table_names, digest = EXCLUDED.digest",
    )
    .bind(&draft.target)
    .bind(&draft.version)
    // 计数超 i64 时饱和而不截断（as 强转会绕回负数）
    .bind(i64::try_from(draft.stats.requested).unwrap_or(i64::MAX))
    .bind(i64::try_from(draft.stats.tables).unwrap_or(i64::MAX))
    .bind(i64::try_from(draft.stats.columns).unwrap_or(i64::MAX))
    .bind(i64::try_from(draft.stats.missing).unwrap_or(i64::MAX))
    .bind(&draft.table_names)
    .bind(&draft.digest)
    .execute(pg)
    .await?;
    Ok(())
}

/// 独立写口（如 `meta sync` CLI 成功后刷新快照）。与 `probe_with_fallback` 同一不变量：
/// 未过最低可用校验的探针结果绝不落库。
pub async fn save_snapshot(
    pg: &PgPool,
    target: &str,
    snapshot: &SchemaSnapshot,
    stats: WarehouseCatalogStats,
) -> anyhow::Result<()> {
    validate_required_snapshot(snapshot)?;
    ensure_snapshot_table(pg).await?;
    persist_draft(pg, &snapshot_draft(target, snapshot, stats)).await
}

/// 快照统计 i64 → usize：负值/超界是脏数据，不静默清零 —— warn 留痕后按 0 计。
fn snapshot_stat(v: i64) -> usize {
    usize::try_from(v).unwrap_or_else(|_| {
        tracing::warn!("warehouse_catalog_snapshot 统计脏值 {v}，按 0 处理");
        0
    })
}

/// 读 target 的最近一次成功快照；没有则为 `None`（调用方据此决定硬失败）。
pub async fn load_snapshot(pg: &PgPool, target: &str) -> anyhow::Result<Option<CatalogSnapshot>> {
    ensure_snapshot_table(pg).await?;
    let row: Option<(
        String,
        String,
        DateTime<Utc>,
        i64,
        i64,
        i64,
        i64,
        Vec<String>,
        String,
    )> = sqlx::query_as(
        "SELECT target, version, probed_at, n_requested, n_tables, n_columns, n_missing, table_names, digest
         -- ds:any 数仓目录快照按数仓 target 键控（一个数仓目标一行），无 ds 作用域维度（同 meta.kv 版本标记）
         FROM meta.warehouse_catalog_snapshot WHERE target = $1",
    )
    .bind(normalize_target(target))
    .fetch_optional(pg)
    .await?;
    Ok(row.map(
        |(target, version, probed_at, requested, tables, columns, missing, table_names, digest)| {
            CatalogSnapshot {
                target,
                version,
                probed_at,
                stats: WarehouseCatalogStats {
                    requested: snapshot_stat(requested),
                    tables: snapshot_stat(tables),
                    columns: snapshot_stat(columns),
                    missing: snapshot_stat(missing),
                },
                table_names,
                digest,
            }
        },
    ))
}

/// 三档裁决的纯决策部分（PG I/O 全在 `probe_with_fallback` 里，这里可单测）。
#[derive(Debug)]
enum FallbackPlan {
    /// 成功：落草稿 + 返回 authoritative 目录。
    Refresh { draft: SnapshotDraft, catalog: FallbackCatalog },
    /// 降级：不落库，返回携带快照时间与当时统计的 degraded 目录；
    /// `probe_err` 随计划带出，供入口打进 warn（否则探针失败原因无留痕）。
    Reuse { catalog: FallbackCatalog, probe_err: String },
}

fn plan_fallback(
    target: &str,
    probe: Result<(SchemaSnapshot, WarehouseCatalogStats), String>,
    stored: Option<CatalogSnapshot>,
) -> Result<FallbackPlan, String> {
    match probe {
        Ok((snapshot, stats)) => Ok(FallbackPlan::Refresh {
            draft: snapshot_draft(target, &snapshot, stats),
            catalog: FallbackCatalog {
                trust: CatalogTrust::Authoritative,
                stats,
                snapshot: Some(snapshot),
            },
        }),
        Err(probe_err) => match stored {
            Some(stored) => {
                if stored.version != VERSION {
                    // 旧版快照只作降级透出（stats/snapshot_at），不参与信任裁决 ——
                    // warn 留痕，但不升级为硬失败（降级的意义就是扛过探针失败期）
                    tracing::warn!(
                        stored = %stored.version,
                        current = %VERSION,
                        "目录快照来自旧版合同，degraded 启动沿用其统计"
                    );
                }
                Ok(FallbackPlan::Reuse {
                    catalog: FallbackCatalog {
                        trust: CatalogTrust::Degraded {
                            snapshot_at: stored.probed_at,
                        },
                        stats: stored.stats,
                        snapshot: None,
                    },
                    probe_err,
                })
            }
            None => Err(format!(
                "数仓目录探针失败且没有任何历史快照，拒绝用空/旧语义启动：{probe_err}"
            )),
        },
    }
}

/// 探针 + 快照兜底的三档入口：成功→刷新快照并返回 `Authoritative`；失败→有快照则
/// 返回 `Degraded`（带快照时间）；两者都无→Err（fail-closed 不变）。
///
/// 成功探针先过 `validate_required_snapshot`，不过按失败处理（可能落入降级档）——
/// 保证落库与兜底的语义都至少曾经完整。PG 写失败属于元数据层故障，直接 Err。
pub async fn probe_with_fallback(
    pg: &PgPool,
    target: &str,
    probe: anyhow::Result<(SchemaSnapshot, WarehouseCatalogStats)>,
) -> anyhow::Result<FallbackCatalog> {
    ensure_snapshot_table(pg).await?;
    let probe = probe
        .map_err(|e| format!("{e:#}"))
        .and_then(|(snapshot, stats)| match validate_required_snapshot(&snapshot) {
            Ok(()) => Ok((snapshot, stats)),
            Err(e) => Err(format!("{e:#}")),
        });
    // 只有失败路径才需要读历史快照，成功路径省一次往返。
    let stored = if probe.is_err() {
        load_snapshot(pg, target).await?
    } else {
        None
    };
    match plan_fallback(target, probe, stored).map_err(anyhow::Error::msg)? {
        FallbackPlan::Refresh { draft, catalog } => {
            persist_draft(pg, &draft).await?;
            Ok(catalog)
        }
        FallbackPlan::Reuse { catalog, probe_err } => {
            if let CatalogTrust::Degraded { snapshot_at } = catalog.trust {
                tracing::warn!(
                    target = %target,
                    snapshot_at = %snapshot_at,
                    stats = ?catalog.stats,
                    probe_err = %probe_err,
                    "数仓目录探针失败，按历史快照降级启动（trust=degraded）"
                );
            }
            Ok(catalog)
        }
    }
}

fn catalog_comment(asset: &Asset) -> String {
    format!(
        "【{}·{}】物理表：{}.{}（生成 SQL 必须使用完整库表名）。粒度：{}。时间/快照：{}。可用指标：{}。禁用规则：{}。比较能力：{}",
        asset.layer,
        asset.domain,
        database_of(asset),
        asset.table,
        asset.grain,
        asset.time_rule,
        asset.metrics,
        asset.forbidden,
        asset.comparison,
    )
}

/// 从静态白名单中挑出与当前问题最相关的资产合同，供深度报表规划选表。
///
/// 这里只做确定性文本排序，不访问业务库，也不把“相似”提升为“可用”：最终 SQL 仍要经过
/// schema 活性、注册表口径和执行闸门。默认销售事实只在问句明确涉及其确认指标时加权，
/// 因而费用、库存、设备、应收等专用主题不会再被通用销售表抢到首位。
pub fn relevant_contracts(question: &str, limit: usize) -> Vec<String> {
    scored_assets(question)
        .into_iter()
        .take(limit)
        .map(|(_, asset)| catalog_comment(asset))
        .collect()
}

/// 明细层（ODS / DIM）= direct-derive 的候选层。合同层（DWS/ADS）未覆盖某维度/语义时，
/// 只允许从明细层推导并显式标注「未经合同验证」；合同层自己绝不进推导候选
/// （否则推导就变成了「换个合同表猜」，fail-closed 顺序就被颠倒了）。
pub fn detail_layer(layer: &str) -> bool {
    layer.eq_ignore_ascii_case("ods") || layer.eq_ignore_ascii_case("dim")
}

/// 按问句相关性打分的全量目录资产（分数 desc → 层高 desc → 表名 asc，全序确定）。
///
/// `relevant_contracts`（深度报表选表）与 `recall::ods`（direct-derive 候选召回）
/// 共用这一份打分 —— 抄第二份就会漂出「深度报表与推导看到的相关序不一致」。
pub fn scored_assets(question: &str) -> Vec<(usize, &'static Asset)> {
    const NOISE: &[&str] = &[
        "多少", "数据", "分析", "情况", "经营", "本周", "上周", "本月", "上月",
        "去年", "同期", "同比", "环比", "当前", "明细", "统计", "变化", "报告",
    ];
    const DEFAULT_SALES_TERMS: &[&str] = &[
        "销售额", "销量", "销售量", "销售数量", "不含税收入", "未税收入", "不含税成本",
        "销售成本", "成本", "成本额", "收入", "净收入", "毛利", "毛利额", "毛利润", "毛利率",
    ];
    const SPECIALIZED_CONTEXT: &[&str] = &[
        "订单", "发货", "出库", "物流", "应收", "损益", "财务", "费用", "活动",
        "第三方", "新品",
    ];

    let question = question.trim().to_ascii_lowercase();
    if question.is_empty() {
        return Vec::new();
    }
    // 窗口词先去重（问句重复短语不重复加分），权重随词 hoist 出资产循环
    let windows: Vec<(String, usize)> = dms_kernel::nl::text::candidate_windows(&question)
        .into_iter()
        .map(|(_, word)| word)
        .filter(|word| !NOISE.contains(&word.as_str()))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .map(|word| {
            let weight = word.chars().count().min(8);
            (word, weight)
        })
        .collect();
    let asks_default_sales = DEFAULT_SALES_TERMS.iter().any(|word| question.contains(word));
    let specialized = SPECIALIZED_CONTEXT.iter().any(|word| question.contains(word))
        || has_pos_context(&question);
    let mut scored = ASSETS
        .iter()
        .zip(asset_corpora())
        .filter_map(|(asset, (corpus, domain))| {
            let mut score = if question.contains(domain) { 32usize } else { 0 };
            for (word, weight) in &windows {
                if corpus.contains(word.as_str()) {
                    score += *weight;
                }
            }
            // 专门上下文里的默认事实是「不加权」而不是扣分（原 +0 分支并入条件）
            if asks_default_sales && !specialized && asset.table == crate::sales_fact::TABLE_NAME {
                score += 40;
            }
            (score > 0).then_some((score, asset))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| layer_rank(right.layer).cmp(&layer_rank(left.layer)))
            .then_with(|| left.table.cmp(right.table))
    });
    scored
}

/// "pos" 按词匹配（不按子串）：英文问句里的 "post"/"purpose" 不再误判专门上下文。
fn has_pos_context(question: &str) -> bool {
    question
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| token == "pos")
}

/// 每资产的（小写语料, 小写 domain）：内容编译期确定，进程内只建一次
/// （原来每问每资产 format!+lowercase 重建）。字段间用 `\n` 分隔 —— 单空格可能与
/// 窗口词拼出跨字段幽灵命中（前字段尾 + 后字段头），`\n` 不可能出现在窗口词里。
fn asset_corpora() -> &'static [(String, String)] {
    static CORPORA: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    CORPORA.get_or_init(|| {
        ASSETS
            .iter()
            .map(|asset| {
                let corpus = format!(
                    "{}\n{}\n{}\n{}\n{}\n{}\n{}",
                    asset.domain,
                    asset.grain,
                    asset.metrics,
                    asset.time_rule,
                    asset.database,
                    asset.table,
                    asset.comparison,
                )
                .to_ascii_lowercase();
                (corpus, asset.domain.to_ascii_lowercase())
            })
            .collect()
    })
}

/// 层级序（与 `detail_layer` 同口径大小写不敏感；目录测试另钉大写不变量，这里不依赖它）。
fn layer_rank(layer: &str) -> u8 {
    if layer.eq_ignore_ascii_case("ads") {
        4
    } else if layer.eq_ignore_ascii_case("dws") {
        3
    } else if layer.eq_ignore_ascii_case("dwd") {
        2
    } else if layer.eq_ignore_ascii_case("ods") {
        1
    } else {
        0
    }
}

pub async fn seed(pg: &PgPool, ds: &str) -> anyhow::Result<()> {
    // 整批包一个事务：中途失败不留半更新的 table_doc（每次启动经 seed.rs 执行）
    let mut tx = pg.begin().await?;
    let mut missed: Vec<&str> = Vec::new();
    for asset in ASSETS {
        let comment = catalog_comment(asset);
        let warn = format!("{}；{}", asset.forbidden, asset.comparison);
        // table_name 统一按小写比（目录名全小写）：大小写混存的行不错漏
        let affected = sqlx::query(
            "UPDATE meta.table_doc SET custom_comment=$1, domain=$2, warn=$3 WHERE ds_id=$4 AND lower(table_name)=$5",
        )
        .bind(&comment)
        .bind(asset.domain)
        .bind(&warn)
        .bind(ds)
        .bind(asset.table)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if affected == 0 {
            // table_doc 缺行（同步跳过/失败）时不能静默 seed 空气
            missed.push(asset.table);
        }
    }
    tx.commit().await?;
    if !missed.is_empty() {
        tracing::warn!("目录 seed 未命中 table_doc 行（{} 张）：{}", missed.len(), missed.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_static_unique_and_fully_qualified() {
        let mut physical = std::collections::HashSet::new();
        let mut base_tables = std::collections::HashSet::new();
        for asset in ASSETS {
            assert!(
                physical.insert((asset.database, asset.table)),
                "重复资产：{}.{}",
                asset.database,
                asset.table
            );
            assert!(
                base_tables.insert(asset.table),
                "跨库同名基础表：{}",
                asset.table
            );
            assert!(matches!(asset.layer, "ODS" | "DWD" | "DWS" | "ADS"));
            assert!(matches!(
                database_of(asset),
                "sales_dw" | "sales_ads" | "fin_dw" | "fin_ads" | "hr_dw" | "dms_ods"
            ));
            assert!(!asset.grain.is_empty());
            assert!(!asset.time_rule.is_empty());
            assert!(!asset.metrics.is_empty());
            assert!(!asset.forbidden.is_empty());
            assert!(
                asset.comparison.starts_with("允许")
                    || asset.comparison.starts_with("条件允许")
                    || asset.comparison.starts_with("禁止")
            );
            let comment = catalog_comment(asset);
            assert!(comment.contains(&format!("{}.{}", asset.database, asset.table)));
            assert!(comment.contains("必须使用完整库表名"));
        }
        assert_eq!(ASSETS.len(), 57, "运行时白名单数量变化必须同步资产文档");
        assert_eq!(metadata_assets().len(), ASSETS.len());
        for (database, expected) in [
            ("sales_dw", 21usize),
            ("sales_ads", 10),
            ("fin_dw", 4),
            ("fin_ads", 5),
            ("hr_dw", 1),
            ("dms_ods", 16),
        ] {
            assert_eq!(
                ASSETS.iter().filter(|asset| asset.database == database).count(),
                expected,
                "{database} 白名单数量变化必须显式评审"
            );
        }
        for (database, table) in [
            ("sales_dw", "dws_off_offline_sale_dfn"),
            ("fin_dw", "dws_fin_customer_balance_dnf"),
            ("fin_ads", "ads_fin_profit_loss_dnf"),
            ("hr_dw", "dws_hr_city_manger_min"),
            ("dms_ods", "t_sales_order"),
        ] {
            assert!(metadata_assets().iter().any(|asset| {
                asset.database() == database && asset.table() == table
            }));
        }
        assert!(!base_tables.contains("dws_mkt_app_distribution_inventory_dfn"));
    }

    #[test]
    fn planning_contracts_prefer_the_verified_business_fact() {
        let sales = relevant_contracts("本月销售额按省区", 4);
        assert!(sales[0].contains(crate::sales_fact::TABLE), "{sales:?}");
        for question in ["本月毛利", "本月收入", "本月成本"] {
            let contracts = relevant_contracts(question, 4);
            assert!(contracts[0].contains(crate::sales_fact::TABLE), "{question}: {contracts:?}");
        }
        let costs = relevant_contracts("本周省区营销费用和费销比", 4);
        assert!(costs[0].contains("销售费用"), "{costs:?}");
        assert!(!costs[0].contains(crate::sales_fact::TABLE), "{costs:?}");
        let stock = relevant_contracts("当前库存金额和缺货风险", 4);
        assert!(stock.iter().any(|contract| contract.contains("库存")), "{stock:?}");
        let device = relevant_contracts("设备需求单和未发货设备", 4);
        assert!(device.iter().any(|contract| contract.contains("设备需求")), "{device:?}");
        let receivable = relevant_contracts("本月应收收入和成本", 4);
        assert!(!receivable[0].contains(crate::sales_fact::TABLE), "{receivable:?}");
        assert!(relevant_contracts("", 4).is_empty());
    }

    /// direct-derive 候选层判据：ODS/DIM 才算明细层（大小写不敏感），合同层一律不算。
    /// 顺序颠倒防线就靠这一条：推导候选里永远不该出现 DWS/ADS。
    #[test]
    fn detail_layer_is_ods_or_dim_only() {
        assert!(detail_layer("ODS"));
        assert!(detail_layer("ods"));
        assert!(detail_layer("dim"));
        for contract in ["DWS", "ADS", "DWD"] {
            assert!(!detail_layer(contract), "{contract} 是合同层，不许进推导候选");
        }
    }

    /// 共享打分的形态：分数 desc 全序确定；空问句恒空（`relevant_contracts` 的旧早退语义
    /// 由 `take` 与这里的空集共同保持，两个调用方的行为一字不变）。
    #[test]
    fn scored_assets_is_total_and_deterministic() {
        assert!(scored_assets("").is_empty());
        assert!(scored_assets("   ").is_empty());
        let first = scored_assets("本月销售额按省区");
        let second = scored_assets("本月销售额按省区");
        let names: Vec<&str> = first.iter().map(|(_, asset)| asset.table).collect();
        assert_eq!(names, second.iter().map(|(_, asset)| asset.table).collect::<Vec<_>>());
        assert_eq!(names[0], crate::sales_fact::TABLE_NAME, "销售问句第一名必须是默认事实：{names:?}");
        // 与 relevant_contracts 的同一份打分：取前 4 必须逐字一致（抄第二份就会在这里红）
        let contracts = relevant_contracts("本月销售额按省区", 4);
        assert_eq!(contracts.len(), 4.min(names.len()));
        assert!(contracts[0].contains(crate::sales_fact::TABLE), "{contracts:?}");
    }

    /// 明细层候选：门店/订单/开票问句的真实召回形态（与共享打分器逐字同源），
    /// 以及「对账单」这种候选为空的回落形态（direct-derive 因此回落不可计算卡）。
    #[test]
    fn detail_candidates_for_an_uncovered_dimension() {
        let detail: Vec<&str> = scored_assets("本月销售额按门店")
            .iter()
            .filter(|(_, asset)| detail_layer(asset.layer))
            .map(|(_, asset)| asset.table)
            .collect();
        assert!(detail.contains(&"t_master_shop"), "门店主数据必须在候选里：{detail:?}");
        assert!(!detail.contains(&crate::sales_fact::TABLE_NAME));
        // 开票问句：两张开票 ODS 申请表是「未同步开票事实」卡的推导素材
        let invoice: Vec<&str> = scored_assets("本月专票开了多少金额")
            .iter()
            .filter(|(_, asset)| detail_layer(asset.layer))
            .map(|(_, asset)| asset.table)
            .collect();
        assert!(invoice.contains(&"t_invoice_apply_header"), "{invoice:?}");
        assert!(invoice.contains(&"t_invoice_new_apply_header"), "{invoice:?}");
        // 对账单：ODS 目录里没有对应资产 → 候选为空（推导无从起手，回落不可计算卡）
        let empty: Vec<&str> = scored_assets("待确认对账单有多少")
            .iter()
            .filter(|(_, asset)| detail_layer(asset.layer))
            .map(|(_, asset)| asset.table)
            .collect();
        assert!(empty.is_empty(), "对账单不许有推导候选：{empty:?}");
    }

    // ── A5 快照与降级（纯决策层，PG I/O 不在单测范围） ──

    fn sample_snapshot() -> SchemaSnapshot {
        use dms_connector::source::TableInfo;
        SchemaSnapshot {
            tables: vec![
                TableInfo { name: "b_table".into(), comment: "表B".into(), row_estimate: 10 },
                TableInfo { name: "a_table".into(), comment: "表A".into(), row_estimate: 20 },
            ],
            columns: vec![
                ("b_table".to_string(), ColumnInfo {
                    name: "id".into(), data_type: "bigint".into(), comment: String::new(), ordinal: 1,
                }),
                ("a_table".to_string(), ColumnInfo {
                    name: "amt".into(), data_type: "decimal".into(), comment: "金额".into(), ordinal: 2,
                }),
                ("a_table".to_string(), ColumnInfo {
                    name: "order_date".into(), data_type: "date".into(), comment: String::new(), ordinal: 1,
                }),
            ],
        }
    }

    fn sample_stats() -> WarehouseCatalogStats {
        WarehouseCatalogStats { requested: 57, tables: 55, columns: 900, missing: 2 }
    }

    fn stored_snapshot() -> CatalogSnapshot {
        CatalogSnapshot {
            target: "sales_dw".into(),
            version: VERSION.into(),
            probed_at: Utc::now(),
            stats: sample_stats(),
            table_names: vec!["a_table".into()],
            digest: "0123456789abcdef".into(),
        }
    }

    #[test]
    fn fallback_success_refreshes_snapshot_and_is_authoritative() {
        let snapshot = sample_snapshot();
        let stats = sample_stats();
        let plan = plan_fallback("SALES_DW ", Ok((snapshot, stats)), None)
            .expect("成功探针必须走 Refresh 档");
        let (draft, catalog) = match plan {
            FallbackPlan::Refresh { draft, catalog } => (draft, catalog),
            FallbackPlan::Reuse { .. } => panic!("成功探针不允许走 Reuse 档"),
        };        assert_eq!(catalog.trust, CatalogTrust::Authoritative);
        assert_eq!(catalog.trust.as_str(), "authoritative");
        assert!(catalog.snapshot.is_some(), "authoritative 必须带回实时探针结果");
        assert_eq!(catalog.stats, stats);
        // 草稿即落库内容：target 归一小写去空白、版本对齐、统计原样、表名排序。
        assert_eq!(draft.target, "sales_dw");
        assert_eq!(draft.version, VERSION);
        assert_eq!(draft.stats, stats);
        assert_eq!(draft.table_names, vec!["a_table", "b_table"]);
        assert_eq!(draft.digest.len(), 16);
    }

    #[test]
    fn fallback_failure_with_snapshot_is_degraded() {
        let stored = stored_snapshot();
        let plan = plan_fallback("sales_dw", Err("connection refused".to_string()), Some(stored.clone()))
            .expect("有历史快照时必须降级而不是硬失败");
        let catalog = match plan {
            FallbackPlan::Reuse { catalog, .. } => catalog,
            FallbackPlan::Refresh { .. } => panic!("失败探针不允许走 Refresh 档"),
        };
        assert_eq!(
            catalog.trust,
            CatalogTrust::Degraded { snapshot_at: stored.probed_at },
            "degraded 必须带快照时间"
        );
        assert_eq!(catalog.trust.as_str(), "degraded");
        assert_eq!(catalog.stats, stored.stats, "degraded 的统计来自快照当时");
        assert!(catalog.snapshot.is_none(), "degraded 不带实时探针结果");
    }

    #[test]
    fn fallback_failure_without_snapshot_fails_closed() {
        let err = plan_fallback("sales_dw", Err("timeout after 8s".to_string()), None)
            .expect_err("无快照时必须维持 fail-closed");
        assert!(err.contains("没有任何历史快照"), "{err}");
        assert!(err.contains("timeout after 8s"), "必须带原始探针错误：{err}");
    }

    /// 旧版快照仍走降级档（warn 语义，不升级为硬失败），且探针错误随计划带出供 warn 留痕。
    #[test]
    fn fallback_reuses_stale_version_snapshot_and_carries_probe_err() {
        let mut stored = stored_snapshot();
        stored.version = "2000.01.01-old-contract".into();
        let plan = plan_fallback("sales_dw", Err("connection reset".to_string()), Some(stored))
            .expect("旧版快照必须仍走降级（stats 只是透出，不是信任裁决）");
        match plan {
            FallbackPlan::Reuse { catalog, probe_err } => {
                assert!(matches!(catalog.trust, CatalogTrust::Degraded { .. }));
                assert_eq!(probe_err, "connection reset", "探针错误必须随 Reuse 带出");
            }
            FallbackPlan::Refresh { .. } => panic!("失败探针不允许走 Refresh 档"),
        }
    }

    /// 打分器纪律：pos 按词匹配（post/purpose 不误伤）；重复窗口词不重复加分；
    /// 语料字段间用 \n 分隔（不可能出现在窗口词里）。
    #[test]
    fn scoring_discipline_pos_word_dedup_and_separator() {
        assert!(has_pos_context("销售额 pos机"));
        assert!(has_pos_context("pos 销量"));
        assert!(!has_pos_context("post sales"), "post 子串不许触发专门上下文");
        assert!(!has_pos_context("purpose"), "purpose 子串不许触发专门上下文");
        let fact = |q: &str| {
            scored_assets(q)
                .into_iter()
                .find(|(_, a)| a.table == crate::sales_fact::TABLE_NAME)
                .map(|(s, _)| s)
        };
        let base = fact("本月销售额").expect("销售问句必须命中默认事实");
        // pos 词命中专门上下文 → 默认事实拿不到那 40 分（窗口词只加不减，方向断言足够）
        assert!(
            fact("本月销售额 pos机").expect("pos 问句仍命中默认事实") < base,
            "pos 词必须让默认事实失去 +40"
        );
        assert_eq!(
            fact("销售额销售额"),
            fact("销售额"),
            "重复窗口词只计一次分"
        );
        assert!(
            asset_corpora()[0].0.contains('\n'),
            "语料字段分隔符必须是不可能出现在窗口词里的 \\n"
        );
    }

    #[test]
    fn snapshot_digest_is_deterministic_order_free_and_content_sensitive() {
        let base = snapshot_draft("sales_dw", &sample_snapshot(), sample_stats());
        let again = snapshot_draft("sales_dw", &sample_snapshot(), sample_stats());
        assert_eq!(base.digest, again.digest, "同输入必须同摘要");

        let mut reversed = sample_snapshot();
        reversed.tables.reverse();
        reversed.columns.reverse();
        let shuffled = snapshot_draft("sales_dw", &reversed, sample_stats());
        assert_eq!(base.digest, shuffled.digest, "摘要必须对输入顺序免疫");
        assert_eq!(base.table_names, shuffled.table_names);

        let mut changed = sample_snapshot();
        changed.columns[0].1.data_type = "int".to_string();
        assert_ne!(
            base.digest,
            snapshot_draft("sales_dw", &changed, sample_stats()).digest,
            "列类型变化必须改变摘要"
        );
        let mut renamed = sample_snapshot();
        renamed.tables[0].name = "b_table_v2".to_string();
        assert_ne!(
            base.digest,
            snapshot_draft("sales_dw", &renamed, sample_stats()).digest,
            "表名变化必须改变摘要"
        );
    }
}
