# Doris 数仓 DMS 资产目录

> 盘点日期：2026-08-06；运行时合同更新：2026-08-07  
> 范围：当前只读 Doris 账号可见的 `sales_dw`、`sales_ads`、`dms_ods`、`fin_dw`、
> `fin_ads`、`hr_dw`，以及 `DW` / `ADS` 中与 DMS 问数直接相关的 DWD/DWS/ADS 资产。  
> 本文不包含 DSN、地址、账号或密码。

## 1. 盘点方法与可信级别

本次只执行了只读低风险操作：

- `information_schema.TABLES/COLUMNS`：表、字段、注释、近似行数和更新时间；
- `SHOW CREATE TABLE`：Doris Key 模型与分桶信息；
- 候选表最多 500 行的字段抽样，不做全表扫描；
- 各取 50 个客户/商品编码做小规模精确 `IN` 匹配；
- 结合 DMS 源码与项目既有血缘决策，确认单据、主数据和权限语义。

`TABLE_ROWS` 是元数据近似值，不作为业务指标。本文使用以下可信级别：

| 级别 | 含义 |
|---|---|
| A | 表结构、样例、DMS 业务语义或已验证口径一致，可进入受控问数合同 |
| B | 表结构和样例已确认，但衍生公式、状态码、完整性或更新时间仍需业务验收 |
| C | 仅完成资产发现，存在旧表、空表、陈旧表、累计快照或跨系统语义风险，不进入默认问数 |

## 2. 可见分层概览

| 库 | 可见相关资产 | 定位 |
|---|---:|---|
| `sales_dw` | 2 DWD、20 DWS、6 DWM/其他，共 28 | 线下销售、费用、对账、设备、预测等主要经营事实 |
| `sales_ads` | 10 ADS | 省区/客户/月度经营汇总与专题应用 |
| `dms_ods` | 67 | DMS 订单、售后、客户、商品、门店、库存和费用原始业务表 |
| `fin_dw` | 5 DWS | 余额、信控、应收和终端费用 |
| `fin_ads` | 6 ADS | 月度损益和应收经营分析 |
| `hr_dw` | 1 DWS | 城市经理与区域月度映射 |
| `DW` | 4 DWD、41 DWS，另含 DIM/DWM | 较多旧版或历史中间层资产，必须逐表判断新旧关系 |
| `ADS` | 10 | 较早的销售出库和市场费用应用表，多张已陈旧或空表 |

`fac_dw/fac_ads` 主要属于生产、采购、工厂和中台 SCM 域，不自动视为 DMS 经营事实。
其中超大销售出库分配表必须有独立血缘、权限和性能评审后才能接入，不能因字段相似直接替代
DMS 销售口径。

### 2.1 运行时静态白名单

`crates/semantic/src/warehouse_catalog.rs` 的运行时元数据资产已从 32 项扩展为 57 项，并由每项资产
直接声明物理库名，不再按 `DWS/ADS` 层级猜成 `sales_dw/sales_ads`：

| 物理库 | 运行时允许资产 | 范围 |
|---|---:|---|
| `sales_dw` | 21 | 当前销售、费用、对账、设备、预测和小程序 DWD/DWS |
| `sales_ads` | 10 | 省区经营、费用、设备、新品和预测 ADS |
| `fin_dw` | 4 | 应收、客户余额、信控和终端费用 DWS |
| `fin_ads` | 5 | 应收和财务损益 ADS |
| `hr_dw` | 1 | 城市经理与区域月度映射 |
| `dms_ods` | 16 | 订单、售后、库存、主数据、活动、余额和开票的必要权威表 |

这 57 项同时是 `information_schema` 元数据探针的**精确编译期白名单**。探针不会动态枚举库表，
不会接受请求参数，也不会纳入本文标为 C/禁用的旧表、空表或陈旧副本。每项运行时合同均包含：
完整库表名、层级、粒度、时间列或最新快照规则、可用指标、禁用聚合及同比/环比能力。

`sales_dw.dws_mkt_app_distribution_inventory_dfn` 虽保留在本文作为历史血缘说明，但已从运行时
白名单删除；当前只允许 `sales_dw.dws_off_app_distribution_inventory_dfn`，二者不得 UNION 或
互相 fallback。`DW`、`ADS` 旧库及 DIM/DWM 历史资产也不进入运行时白名单。

## 3. 默认经营口径

### 3.1 线下销售事实（A）

**默认表**：`sales_dw.dws_off_offline_sale_dfn`

| 项目 | 合同 |
|---|---|
| 业务确认字段 | `order_date`、`storecode`、`storename`、`skucode`、`skuname`、`war_zone`、`region`、`qty`、`amount`、`cost_excluding_tax`、`revenue_excluding_tax`、`gross_profit` |
| 业务分析粒度 | 日期 × 客户 × 商品 × 战区 × 省区；默认问数只使用本表中已确认的 12 个字段 |
| 时间列 | `order_date` |
| 客户键 | `storecode`，客户名称为 `storename` |
| 商品键 | `skucode`，商品名称为 `skuname` |
| 区域维度 | `war_zone`（战区）、`region`（省区）；省份、城市必须使用独立已验证资产 |
| 商品维度 | `skucode`、`skuname`；商品分类必须使用独立已验证资产 |
| 销量 | `SUM(qty)` |
| 销售额 | `SUM(amount)` |
| 不含税成本 | `SUM(cost_excluding_tax)` |
| 不含税收入 | `SUM(revenue_excluding_tax)` |
| 毛利额 | `SUM(gross_profit)` |
| 毛利率 | `SUM(gross_profit) / NULLIF(SUM(revenue_excluding_tax), 0)`，禁止对行毛利率求平均 |
| 权限键 | DMS 客户权限映射到 `storecode`；空权限集合必须返回 0 行 |

核验结论：

- 50 个 `storecode` 全部命中 `dms_ods.t_customer.customer_code`，0 个命中
  `t_master_shop.shop_code`，因此物理注释中的“店铺”必须解释为**客户**，不能解释为门店；
- 默认 `SUM(amount)` / `SUM(qty)` 按业务确认合同解释为经营销售额/销量，不再拼旧发货
  UNION，也不根据未确认类型列自行拆分销售与退货；
- “出货量/发货量”属于物流或出库事件，不是默认 `SUM(qty)` 的同义词；必须使用已验证的
  物流/DWD/DWS 出库合同，并按对应事件时间统计；
- 省份、城市、商品分类、经理、门店和订单类型均不属于默认事实能力。只有独立资产完成
  表、列、粒度、时间和权限合同登记后才能使用，禁止静默回落到物理表同名列；
- 样例中不含税收入和毛利字段存在空值。展示毛利类指标时必须同时给出覆盖率或缺口提示；
- 本表没有销售订单号，**订单数不得按行数或 `id` 数量计算**。

### 3.2 省区月度经营（A/B）

**首选表**：`sales_ads.ads_off_offline_region_sale_dfn`

| 项目 | 合同 |
|---|---|
| Doris Key | `UNIQUE KEY(id)` |
| 粒度 | 月 × 战区 × 省区 × 省份 × 商品大类 |
| 时间列 | `data_month` |
| 维度 | `war_zone`、`region`、`state`、`goods_type` |
| 指标 | `sales_revenue` 月目标及分摊损益/费用专题字段；实际销售 KPI 不从本表取数 |

适合省区目标达成和月度分摊损益专题。实际销售额、销量、不含税成本/收入、毛利额和毛利率
仍必须从 `sales_dw.dws_off_offline_sale_dfn` 按相同省区、月份和权限范围计算，禁止用 ADS
同名金额替代。`sales_revenue` 是否已按省份/商品大类分摊需要继续业务验收；在确认前，不能
跨更细粒度盲目求和月目标。

## 4. 核心业务资产目录

### 4.1 订单、出库与对账

| 优先级 | 表 | 粒度与主键 | 时间列 | 推荐用途 | 主要风险 |
|---|---|---|---|---|---|
| A | `dms_ods.t_sales_order` | 订单头，`sales_order_code` 唯一 | `order_time` | 订单数、订单状态、订单金额、客户订单详情 | 必须过滤 `deleted_flag=0` 及无效/作废状态；订单额不是默认销售额 |
| A | `dms_ods.t_sales_order_detail` | 订单 × 行类型 × 行号 | `created_time` / `delivery_time` | 商品需求、价格、订单行 | 样例 `actual_delivery_quantity` 大量为 0，不作为发货量首选；行类型需明确 |
| A | `dms_ods.t_sales_order_logistics` | 物流明细，`id` 唯一 | `delivery_time` | 实际批次发货、出库单、仓库、签收 | 一张订单行可多批物流，聚合订单金额前必须防扇出 |
| A/B | `DW.dwd_mkt_order_information_dfn` | 订单出库商品行，`id` 唯一 | `order_date`、`shipat` | 应发/实发数量金额、短发、出库单 | `DW` 历史层；先确认保留周期与退款标志完整性 |
| B | `sales_dw.dws_sales_state_sales_dnf` | 出库日 × 销售单 × SKU × 仓库 × 客户，DUPLICATE KEY 日期 | `created_date` | 出库数量、仓库/渠道/状态结构 | 约 1,358 万行，必须限定日期或精确单号；无经营销售额字段 |
| A | `sales_dw.dws_fin_shipment_check_dnf` | DMS 单 × 中台出库单 × Base 来源单，`id` 唯一 | `ship_at` | 拆单映射、行数/金额差异、换品核对 | 只用于对账，不把任一对账金额当默认销售额 |
| A/B | `sales_dw.dws_fin_receivable_check_dnf` | 中台出库单 × 应收单 × 金蝶单 | `data_date` | 中台/Base/金蝶应收链路核对 | 更新时间落后于当前经营事实；仅用于对账 |
| A/B | `sales_dw.dws_fin_receivable_adjust_check_dnf` | DMS 调整单 × 中台调整单 × 金蝶单 | `bizdate` | 费用调整单对账 | 不与普通销售订单或经营销售额混算 |
| B | `fin_dw.dws_receivable_sale_min` | 客户 × SKU 应收销售行 | `data_date`（字符串） | 应收口径数量、含税/不含税金额 | 不是发货经营口径；更新时间与日期类型需检查 |
| B | `fin_ads.ads_fin_receivable_dnf` | 月 × 客户 × SKU × 区域分类 | `data_month` | 月度应收、计价数量、成本结构 | 应收口径，不替代 DWS 默认销售额 |

订单指标合同：

- 订单数优先使用 `COUNT(DISTINCT t_sales_order.sales_order_code)`，按有效状态过滤；
- 客单价使用同一订单口径的订单额 / 订单数，不能拿 DWS 销售额除以订单数；
- 发货量优先物流或已验证出库 DWD/DWS；不能从订单行数、销售事实行数推算；
- 泛指“销售额/销量/不含税成本/不含税收入/毛利额/毛利率”时，默认销售 DWS 优先于
  订单、物流、应收和财务损益专题；只有用户明确指出订单额、出库、应收或损益口径时才切换；
- “发货销售额/出库销售额/物流销售额/应收销售额”不会自动改写成默认销售额。对应专题没有
  已验收金额公式时必须明确返回数据边界，禁止恢复旧发货 UNION 或拿订单金额、应收金额代替；
- “财务/损益”与销售指标同时出现时，先按财务专题合同解释；泛指“毛利/收入/成本”才使用
  默认销售 DWS。目录召回会保留两类合同供核对，但不会把专题金额静默并入默认销售事实；
- 单号查询可从对账表映射中台拆分单，但最终权限仍继承 DMS 原销售单客户范围。

### 4.2 售后与退款

| 优先级 | 表 | 粒度与主键 | 时间列 | 推荐用途 | 主要风险 |
|---|---|---|---|---|---|
| A | `dms_ods.t_after_sales_order_header` | 售后单头，`id` 唯一，业务号 `after_sales_code` | `after_sales_time` | 申请退款额、实际退款额、售后单数、类型/状态 | `refund_amount` 与 `actual_refund_amount` 含义不同；状态码需按 DMS 枚举翻译 |
| A | `dms_ods.t_after_sales_order_detail` | 售后商品行，`id` 唯一 | `created_time` / `delivery_time` | SKU 退款金额与申请/实退数量 | 数量有箱/袋/统一数量多套字段，必须显式选择单位 |
| B | `DW.dwd_mkt_return_warehous_dfn` | 退货入库单 × 出库单 × 客户 × SKU | `ship_at`（字符串） | 实际退货入库数量和金额 | 时间列是字符串；需确认时间格式、退货完成状态和保留周期 |
| C | `DW.dws_fin_returncust_mfn` | 售后费用申请 × 客户 × SKU | `data_date` | 售后费用流程分析 | 数据量小且更新时间较旧，不代表完整退款事实 |
| 禁用 | `ADS.b2b_daily_sku_return` | 空表 | `return_date` | 无 | 当前 0 行且长期未更新，不能用于退款答案 |

退款率必须先声明分子：申请退款、实际退款还是实退入库。若使用默认销售额作分母，分子和分母要用
各自正确时间列并采用同长度窗口；不能把售后申请时间与订单时间混为同一事件。

### 4.3 库存与经销存

| 优先级 | 表 | 粒度与主键 | 时间列 | 推荐用途 | 主要风险 |
|---|---|---|---|---|---|
| A/B | `dms_ods.t_winc_stock_report` | 经销商库存快照行，`id` 唯一 | `product_stock_date` / `dealer_stock_date` | 当前库存量、库存金额、客户/仓库/SKU 库存 | 快照表禁止跨日期相加；500 行样例的候选业务键仅 410 个唯一，需继续验证同日重复版本去重 |
| B | `dms_ods.t_winc_sale_report` | 经销商销售上报行，`id` 唯一 | `stat_date` / `bill_date` | 经销商 sell-through、客户/门店销售上报 | 这是经销商上报口径，不与默认 DMS sell-in 销售额相加 |
| A/B | `sales_dw.dws_off_app_distribution_inventory_dfn` | 日期 × 小程序订单 × 门店 × SKU | `order_date` | 小程序需求、发货、签收、退货量 | 订单数按 `order_no` 去重；字段物理名 `ruturn_qty` 拼写异常 |
| C | `sales_dw.dws_mkt_app_distribution_inventory_dfn` | 与上表近似 | `order_date` | 历史市场域副本 | 行数少、更新时间旧；与 `dws_off_*` 未确认去重关系，禁止 UNION |

库存默认策略是最新有效快照，而不是所有历史快照求和。当前合同至少要求
`product_stock_date = MAX(product_stock_date)`；在完成重复版本核验后，还应评估是否需要按
客户 × 仓库 × SKU 选最新 `updated_time/id`。

### 4.4 客户、商品与门店主数据

| 优先级 | 表 | 业务主键 | 推荐用途 | 主要风险 |
|---|---|---|---|---|
| A | `dms_ods.t_customer` | `customer_code` | 当前客户档案、客户类型/分类、渠道、区域经理和启停状态 | 含电话、证件、税号、银行等敏感信息；默认答案必须脱敏且按权限过滤 |
| B/C | `DW.dim_cust_offline_information_min` | 物理 `id`，含 `clear_code` | 历史/月度客户快照与数仓区域映射 | 更新时间较旧；不能覆盖当前 DMS 客户主档 |
| A | `dms_ods.t_goods` | `goods_code` | 当前商品名称、品牌、条码、上下架和产品说明 | 抽样 50 个销售 SKU 的 `goods_category_name` 均为空，不能作为销售分类首选 |
| A | `DW.dim_sku` | `sku_code` | 商品二/三级分类、产品渠道、物料属性 | 50 个销售 SKU 中 47 个有 `class2`；需对未映射部分展示“未分类” |
| A | `dms_ods.t_master_shop` | `shop_code`，关联 `customer_code` | 当前门店档案、客户归属、地域、面积、门店类型 | 必须过滤删除；`monthly_sales` 和 `area` 样例覆盖极低，不能作为销售额/坪效默认事实 |
| B/C | `DW.dim_cust_master_shop_min` | `id`，含 `data_month` | 历史门店快照 | 更新时间较旧；不能替代当前门店档案 |
| B | `DW.region_shop_stats` | 省区 | 当前省区门店数应用表 | 需确认门店有效状态、去重规则和更新时间口径 |
| B | `hr_dw.dws_hr_city_manger_min` | 月 × 省区 × 省份 × 城市 × 经理 | 城市经理区域月度映射 | 经理仍是姓名；跨月、同名和人员变更需谨慎 |

客户与门店必须分开：默认销售事实的 `storecode` 是客户编码；只有明确含 `shop_code/shop_name`
的表才能回答真实门店问题。门店销售额、店均销售额、坪效不得用客户销售额冒充。

### 4.5 销售费用、活动费用与管理费用

| 优先级 | 表 | 粒度 | 时间列 | 推荐用途 | 主要风险 |
|---|---|---|---|---|---|
| A | `sales_ads.ads_off_sales_cost_customer_dnf` | 月 × 客户 × 经理 × 部门 | `data_month` | 客户级销售费用结构、费销比 | `amount` 是本月销售金额，不是费用合计；费用需按分类列求和 |
| A | `sales_ads.ads_off_sales_cost_region_dnf` | 月 × 战区 × 省区 | `data_month` | 省区销售费用结构、费销比 | 同上；不得把销售金额加进费用总额 |
| A/B | `sales_dw.dws_off_sales_cost_dnf` | 月 × 客户 × 经理 × 部门 | `data_month` | 客户/经理/部门费用宽表拆解 | 无单一费用合计列，必须明确费用分类公式 |
| A | `sales_dw.dws_off_sales_cost_notshare_dnf` | 日期 × 费用单据 × 客户 × 三级分类 | `data_date` | 未分摊费用明细追溯 | 仅代表未分摊费用，不能代表全部销售费用 |
| A | `sales_dw.dws_off_shop_cost_dnf` | 日期 × 费用单据 × 客户 × 门店 × 费用项 | `data_date` | 门店费用、核销/报销、地域结构 | 只有该类含真实 `shop_code/shop_name` 的事实才可做门店费用 |
| A/B | `sales_dw.dws_off_activity_promoter_fin` | 活动 × 客户 × 门店 × 起止日期 | `data_date`、`start_date/end_date` | 活动费用、活动销售、费率、活动天数 | `amount` 已是费用合计；`daysale` 注释与人员数冲突，未确认前禁用 |
| A | `dms_ods.t_activity_main` | 活动单头，`activity_no` | `created_time` | 活动申请金额、活动场次和状态结构 | `total_amount` 是活动申请口径，不等于实际发生或已核销费用；状态码范围必须由问题明确 |
| A | `dms_ods.t_activity_promoter_fee` | 活动 × 临促费用行 | `created_time` | 临促人员费用 | 只取 `total_amount`；不能代表活动费用或市场费用总额 |
| A | `dms_ods.t_market_activity_promoter_expense` | 活动 × 执行费用行 | `created_time` | 活动执行人员费用 | `activity_date` 为空，禁止作为时间列；只取 `amount`，不能代表活动费用总额 |
| A/B | `sales_dw.dws_off_management_cost_dnf` | 月 × 单据 × 人员 × 部门 | `data_date` | 管理费用、人员和部门分析 | `total_management_cost` 已是合计，禁止再与分项相加；表内销售额不替代默认销售事实 |
| A | `sales_dw.dws_off_management_cost_detail_dnf` | 日期 × 报销单 × 人员 × 部门 × 费用类型 | `data_date` | 管理费用明细追溯 | 不与汇总表重复叠加 |
| A/B | `sales_ads.ads_off_dept_management_cost_dnf` | 月 × 部门 | `data_month` | 部门月度管理费用与管理费率 | 合计列与分项不可重复相加 |

销售费用十类建议固定为：长促督导、客户赔偿、营销物料、营销设备、终端费用、广告费用、
活动执行、客户返利、非活动样品和其他。费销比只能使用同表配套销售金额，或明确与默认销售事实
做同客户、同月、同权限范围对账后计算。

开票仍是独立 ODS 双流合同：`dms_ods.t_invoice_apply_header`（旧流）和
`dms_ods.t_invoice_new_apply_header`（新流）都按 `apply_time` 过滤，且只统计
`invoice_status='2'` 的已开票金额。两流交集为 0，必须 `UNION ALL`；任意单流都会漏数，
应收表也不得替代开票金额。

### 4.6 财务损益与余额

| 优先级 | 表 | 粒度 | 时间列 | 推荐用途 | 主要风险 |
|---|---|---|---|---|---|
| B | `fin_ads.ads_fin_profit_loss_dnf` | 月 × 省区 × 城市 × 客户 | `data_month` | 月度收入、成本、毛利、费用、税前/净利润 | 综合管理口径；更新时间和分摊规则必须展示，不替代实时销售事实 |
| B | `fin_ads.ads_fin_profit_loss_fresh_dnf` | 同上，鲜食子口径 | `data_month` | 鲜食经营损益 | 与总表/冻品表互斥使用，禁止相互重复叠加 |
| B | `fin_ads.ads_fin_profit_loss_frozen_dnf` | 同上，冻品子口径 | `data_month` | 冻品经营损益 | 同上 |
| A/B | `fin_ads.ads_fin_receivable_agg_sku_m` | 月 × 标准省区 × 分类 × SKU | `data_month` | 应收 SKU 金额、销量、成本 | 应收计价口径，不等于经营净销售额 |
| A/B | `fin_dw.dws_fin_customer_balance_dnf` | 客户 × 期间 | `time_period` / `data_date` | 客户可开票、不可开票、信控、市场费余额 | 快照/期间表，必须选明确期间或最新记录，禁止跨期求和余额 |
| A/B | `fin_dw.dws_fin_credit_balance_dnf` | 日期 × 客户 × 类型 | `data_date` | 信控额度、余额、关联销售单 | `sales_amount` 是表内信控配套值，不替代默认销售额 |
| B | `fin_dw.dws_fin_terminal_system_fees_fin` | 终端费用单 × 客户/门店 | `reimbursement_time` | 终端陈列费核销明细 | 涉及人员、地址等敏感字段，答案需脱敏 |

### 4.7 专题应用资产

| 优先级 | 表 | 粒度 | 推荐用途 | 主要风险 |
|---|---|---|---|---|
| A/B | `sales_dw.dws_off_region_sales_plan_min` | 月 × 省区 × SKU | 商品计划/实际销量和差异 | 月份为字符串；不得把差异百分比跨 SKU 相加 |
| A/B | `sales_ads.ads_off_region_sales_plan_min` | 月 × 省区 | 省区预测准确率 | 比例为已计算值，跨省不能直接求和 |
| B | `sales_dw.dws_off_sales_bonus_detail_dnf` | 月 × 经理 | 新品激励、排名和奖金 | 经理只有姓名；排名、触发条件、销量、奖金不可混加 |
| A/B | `sales_dw.dws_off_storeprice_dnf` | 客户 × SKU | 当前客户商品价格、渠道价、箱规 | 无生效时间，不能回答价格历史或调价次数 |
| B | `sales_dw.dws_off_third_party_sales_dnf` | 日期 × 客户 × SKU × 区域 × 品牌 | 第三方产品销售 | 只用于第三方产品，不与默认自有线下销售混加 |
| B | `sales_dw.dwd_off_pos_sales_min` | 月 × 客户 × POS 门店名称 × SKU | KA POS sell-out | POS 口径；门店缺稳定编码，不能与 DMS sell-in 混称 |
| B | `sales_dw.dwm_off_keycustomer_possales_min` | 配送日 × 客户 × 终端 × 条码 | 重点客户 POS/配送数据 | `data_type` 区分 POS 与出库配送，必须过滤 |
| B | `sales_dw.dws_off_msy_skuinfor_min` | 日期 × 省区 × 外部商品 | 外部市场、价格、品牌和排名 | 外部监测口径，不与自有销售相加 |
| B | `sales_dw.dwm_mkt_msy_statedata_sku_min` | 日期 × 省份 × 外部商品 | 市占率、售价、铺市率 | 比例为已计算值，禁止再次求和 |
| B | `sales_ads.ads_off_new_product_sales_dnf` | 日期 × 客户 × 新品 × 区域 × 经理 | 新品销售业绩 | `qty` 与 `box_qty` 单位不同；经理仅姓名 |
| B | `sales_ads.ads_off_offline_new_goods_sale_dfn` | 日期 × 客户 × 新品 × 区域 × 渠道 | 新品销售和铺市率 | 已计算比例不可求和；`storecode` 仍按客户解释 |
| A/B | `sales_dw.dwd_off_baigeyun_device_delivery_item_scd` | 设备 × 拉链有效期 | 设备位置、在线状态和历史 | 当前状态必须使用当前版本标志；版本数不是设备数 |
| B | `sales_ads.ads_off_customer_device_efficiency_dnf` | 客户 × 地域 × 设备类型 | 客户设备效率快照 | 无时间列，只能回答当前快照，不能做同比/环比 |
| B | `sales_ads.ads_off_shop_device_efficiency_dnf` | 客户 × 门店 × 地域 × 设备类型 | 门店设备效率快照 | 同上；真实门店使用 `shop_code/shop_name` |
| A/B | `sales_ads.ads_off_mmhm_device_requirement_dnf` | 需求单 × 门店 × 设备 | 设备需求、申请、未发货、收货 | 联系方式/地址敏感；单号与权限需继承 DMS 设备链 |
| B | `sales_dw.dws_mkt_app_place_order_dnf` | 统计日 × 客户 | 小程序当日/本月支付与取消 | 同行含当日与月累计，必须按 `data_date` 取最新快照，禁止跨日 SUM 累计列 |
| B | `sales_dw.dws_mkt_sampleorder_infor_dnf` | 统计日 × 客户 | 本月/上月订单与样品单累计 | 月累计快照，必须取最新统计日；`store_code` 按客户核验 |

## 5. 新旧表替代关系

| 现行首选 | 旧/备选资产 | 结论 |
|---|---|---|
| `sales_dw.dws_off_offline_sale_dfn` | `DW.dws_mkt_offline_sale_dfn`、`ADS.b2b_daily_sku_sales*` | 默认经营销售统一用现行 DWS；旧表更新时间或口径不同，禁止 UNION、静默回退或拼接历史 |
| `sales_ads.ads_off_offline_region_sale_dfn` | `DW.dws_mkt_offline_region_sale_dfn` | 现行 ADS 更新更及时、字段更完整；旧表仅历史核验 |
| `sales_dw.dws_fin_shipment_check_dnf` | `DW.dws_fin_shipment_check_min` | 现行对账表优先；旧表行数和更新时间明显落后 |
| `sales_dw.dws_fin_receivable_check_dnf` | `DW.dws_fin_receivable_check_min` | 现行表优先，但仍需监控自身更新时间 |
| `sales_dw.dws_off_storeprice_dnf` | `DW.dws_storeprice_dnf` | 现行表覆盖更高且更新更及时；旧表不可兜底补齐 |
| `sales_dw.dws_off_app_distribution_inventory_dfn` | `sales_dw.dws_mkt_app_distribution_inventory_dfn` | `off` 表为当前首选；两者来源域未确认，禁止合并 |
| `sales_ads.ads_off_sales_cost_*` | `sales_dw.dwm_off_sales_cost_1..4_dnf`、旧 `ADS.b2b_market_cost` | ADS 用于稳定汇总，DWM/明细用于追溯；旧 ADS 已陈旧，不能作为当前费用默认源 |
| `sales_ads.ads_off_sales_cost_customer_dnf` 十类费用列 | `dms_ods.t_market_total_expense` 及各专项费用表 | 泛指市场/营销费用已迁移到 ADS 十类费用列合计；`amount` 是配套销售金额，禁止纳入费用；ODS 旧合计与专项表不再作为默认 fallback |
| `dms_ods.t_customer/t_goods/t_master_shop` | `DW.dim_cust_*` 等月度快照 | 当前主数据优先 ODS；DIM 只用于明确历史月份或 ODS 缺失的已验证分类映射 |
| 独立商品分类资产（尚未接入默认销售执行合同） | `DW.dim_sku.class2`、`dms_ods.t_goods.goods_category_name` | 两者均不得作为默认销售事实的静默 fallback；完成粒度、时间、权限与映射验收前，商品分类经营问数 fail-closed |

任何替换必须显式版本化。新旧表行数差异不能解释为“新表缺数据”或“旧表更完整”，需要结合
保留周期、去重、退货、重跑范围和业务口径共同判断。

### 5.1 默认指标迁移状态

| 指标族 | 当前默认来源 | 状态与原因 |
|---|---|---|
| 销售额、销量、不含税成本/收入、毛利额、毛利率 | `sales_dw.dws_off_offline_sale_dfn` | 已迁移且为唯一默认销售事实；订单额、应收、费用和库存均不得替代 |
| 市场/营销费用 | `sales_ads.ads_off_sales_cost_customer_dnf` | 已迁移；按十类费用列求和，`amount` 是配套销售金额，不计入费用 |
| 订单额、订单数、成交客户数、订单客单价 | `dms_ods.t_sales_order` | 保留 ODS 已验证订单口径；默认销售 DWS 没有订单号，无法无损迁移 |
| 售后单数、申请退款额 | `dms_ods.t_after_sales_order_header` | 保留 ODS；申请退款、实际退款、实退入库是不同事件，现有 DWS/ADS 无统一等价公式 |
| 库存量、库存金额 | `dms_ods.t_winc_stock_report` 最新快照 | 保留 ODS；尚无已验证的当前库存 DWS，且同日重复版本规则仍待确认 |
| 活动申请金额、活动场次、临促/执行费用 | 对应 `dms_ods` 活动表 | 保留 ODS；活动申请、实际发生和核销口径尚未统一，不以相似活动宽表替换 |
| 账户余额、信控余额 | `dms_ods.t_customer_balance` 每客户×类型最新行 | 保留 ODS；`fin_dw` 资产已纳入目录，但余额类型和最新期间字段公式尚未完成一一验收 |
| 开票金额 | `dms_ods.t_invoice_apply_header` 与 `t_invoice_new_apply_header` 并集 | 保留 ODS；数仓没有已确认的新旧两流完整事实，应收金额不能替代开票金额 |
| 赠品箱数、动销商品数 | `dms_ods.t_sales_order_detail` + 有效订单口径 | 保留 ODS；赠品类型在销售 DWS 不可识别，动销定义也未验收为净销售事实去重公式 |

`fin_ads` 损益/应收、`fin_dw` 余额/信控和 `hr_dw` 组织表已能进入运行时选表，但这里只登记
资产能力，不凭字段名称自动新增默认指标。新增指标前必须补齐字段公式、权限键、时间/快照和
同比环比合同。

## 6. 各业务域口径风险清单

### 销售

1. 默认销售额是 `SUM(sales_dw.dws_off_offline_sale_dfn.amount)`，时间列仅用 `order_date`。
2. 事实同时含出库正数和退货负数；禁止再减一次退款或拼旧退货 UNION。
3. DWS 行数不是订单数；订单数必须回到订单头业务号。
4. `storecode/storename` 是客户，不是门店。
5. 毛利率必须先汇总分子分母再相除；不含税收入/成本/毛利存在空值，需要展示覆盖率。
6. 同比、环比使用同口径、同权限和可比时间长度；当期不完整时必须明确标记。

### 订单与出库

1. 订单金额、经营销售额、应收金额、出库金额是四种口径，不能混称。
2. 订单主表、明细和物流是一对多链路；任何 JOIN 都可能放大订单金额。
3. `t_sales_order_detail.actual_delivery_quantity` 抽样大量为 0，发货量优先物流或数仓出库事实。
4. 精确单号可以走明细和对账；统计类不要跨回生产业务库做多表大 JOIN。

### 退款与售后

1. 区分申请退款额、实际退款额和实退入库金额。
2. 售后数量存在箱、袋和统一数量多套字段，报告必须标单位。
3. 默认销售事实已经包含退货负数，退款金额只作售后分析，不能重复冲减默认销售额。
4. 空的旧退款 ADS 表必须禁用。

### 库存

1. 库存是快照，禁止跨日期相加。
2. 最新全局日期不一定等于每个客户/仓库/SKU 的最新有效版本；需继续验证迟报和重报。
3. 数量允许负数，可能代表调整或异常，不能强制截断为 0。
4. 经销商库存、DMS 可售库存、工厂库存是不同域，禁止混加。

### 客户、商品、门店

1. 客户编码和门店编码必须分开；客户销售额不能用于门店排行。
2. 客户主数据含大量敏感字段，默认只展示经营相关字段并脱敏。
3. 商品 ODS 类别覆盖不足；销售分析优先事实内分类或 `DW.dim_sku`。
4. 门店 `monthly_sales`、面积覆盖不足，不能默认计算店均销售额、坪效或人效。

### 费用与利润

1. 费用宽表的 `amount` 可能是配套销售额，而非费用合计。
2. `total_management_cost`、`activity.amount` 等合计列不能与分项再次相加。
3. 未分摊费用只是费用子集，不能代表总费用。
4. 财务损益 ADS 是月度分摊后的管理口径，需展示数据月份、更新时间和分摊边界。
5. 比例、率、排名、目标和月累计字段通常不可跨维度直接求和。

## 7. 推荐选表优先级

1. **已验证 DWS/ADS 经营事实**：默认销售、月度省区经营、费用汇总、对账。
2. **DWD 明细**：订单出库、退货入库、设备拉链和 POS 明细，必须带时间或精确单号。
3. **运行时白名单内的 `fin_dw/fin_ads/hr_dw` 资产**：应收、损益、余额和组织映射只按各自合同
   使用，不得替代默认销售额、订单额或权限键。
4. **运行时白名单内的 16 张 `dms_ods` 业务表**：单据详情、当前主数据、售后、库存、活动、
   余额和开票；在 Doris 上可读，但仍按单表/小范围原则使用，不扩展成 ODS 全库枚举。
5. **DIM**：补充分类、组织和历史快照，不产生经营金额，当前不进入运行时资产白名单。
6. **旧 `DW/ADS` 或 DWM 底表**：只做血缘核对和明细追溯，不进入运行时选表。
7. **空表、陈旧表和来源不明的跨系统表**：禁用，直到完成业务验收和版本登记。

## 8. 查询与性能边界

- 默认销售查询必须包含明确 `order_date` 范围；实体查询再加客户或商品编码。
- `dws_sales_state_sales_dnf`、订单明细、物流和大型 DWD 必须限定日期或精确单号并限制行数。
- 不为“方便”连接 DWS、DWD、ODS 多张大表；优先分步小查询后在应用层合并结果。
- 生产 DMS MySQL 若被热切为查询目标，只允许单表、索引等值/小 IN/前缀匹配、短超时和小 LIMIT；
  禁止 JOIN、子查询、聚合、排序大扫描和跨表统计。
- 权限过滤必须在每个事实查询内生效，不能取全量后再在前端过滤。
- 结果中展示物理来源、数据日期和口径名称，但不展示连接凭据或内部敏感字段。

## 9. 尚待业务确认

1. `sales_ads.ads_off_offline_region_sale_dfn.sales_revenue` 在省份/商品大类粒度下的目标分摊规则；
2. `t_winc_stock_report` 同客户 × 仓库 × SKU × 日期的重复版本与迟报处理；
3. 售后状态码与“申请、审核、完成、实际退款、实退入库”的最终枚举映射；
4. `dws_off_activity_promoter_fin.daysale` 的真实语义；
5. 财务损益 ADS 的分摊规则、冻结时间和鲜食/冻品/总表互斥关系；
6. 城市经理姓名变更、同名人员和跨月组织调整的稳定人员键；
7. 门店数、有效门店、营业门店、下单门店及坪效/人效所需外部数据的权威口径。
8. `fin_dw.dws_fin_customer_balance_dnf` 与 `dms_ods.t_customer_balance` 的余额类型、最新期间和字段公式映射；确认前账户余额/信控余额保留 ODS 已验证快照口径。
9. 活动申请金额、实际活动费用和已核销费用的统一业务定义；确认前 `t_activity_main.total_amount` 只表示活动申请口径，不按字段相似迁移到活动 DWS。
10. 开票新旧两流在数仓的完整同步事实；确认前必须使用 ODS 两流并集，不能用应收表替代开票金额。
11. `fin_ads` 损益和应收表各金额字段的最终公式、冻结规则与权限键；资产可被召回，但未形成稳定公式的指标不自动播种。

这些问题在确认前应以“数据边界”呈现，不允许用相似字段或全量数值替代。
