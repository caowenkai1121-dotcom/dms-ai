# dms-ai 迭代记录

> 计划与选型：仓库外 `../REBUILD-PLAN.md`。红线：DMS 生产 MySQL 只读（连接级 READ ONLY 兜底）。

## M0 骨架（2026-07-23，已验收）
- axum 服务 :8100（/api/health）+ Vue3/antdv 前端 :5180（/api 代理）+ PG 容器 :15433。
- 实测：PG 18.1 + AGE 1.7.0 + pgvector 0.8.5 + pg_trgm；MySQL 会话 `transaction_read_only=1`；全链路 ok=true。
- 坑：PG18+ 镜像卷必须挂 `/var/lib/postgresql`（挂 /data 子目录循环重启）。

## M1 权限内核（2026-07-23，已验收）
- `principal.rs`（员工+激活角色加载，多角色单激活，无角色 fail-closed）+ `scope.rs`（1:1 复刻 Java DefaultEmployee 集合计算）+ `inject.rs`（sqlparser AST 注入，含子查询/CTE/UNION 递归）。
- 语义锁死要点（均注 Java 行号）：
  - 基础档 type=1 取 MAX view_type：0本人/1本部门/2部门及下级/3结算客户=哨兵/10全部；**ALL 或无 type=1 行 → 整体短路不限制**（Java L281-292）。
  - 定制档：101 下属=任职表 manager_id 递归（deleted=0+service_status=0，含本人）；102=FIND_IN_SET(组码, t_customer.customer_group)；103=contacts contact_name=**姓名** + contact_type IN ('Y1','Y3')。
  - customer_codes = 基础(area_manager_id IN 基础ids) + 公用客户字典(payment_customer_for_inside/for_all) + 102 + 103 + 下属客户；各段哨兵跳过标旗，终空且旗 → ['-1']。
  - 哨兵 in(-1)=拒绝 vs 空=放行，语义相反。超管/admin 全短路。
  - 部门员工=主部门 OR 任职部门（任职行 deleted=0+service_status=0，EmployeeMapper.xml L179）；子部门=status=1+deleted=0 按 parent_id 递归。
- 绑定注册表（binding_of，@DataScope joinSql 逐条探库核实）：t_sales_order/his(owner_manager)、t_customer/balance(area_manager_id；balance 无此列只绑客户)、t_after_sales_order_header(owner_manager)、t_activity_main(created_id)、t_invoice_apply_header(manager)、t_account_bill_header(created_by=登录名 Codes 维)、设备/巡店类仅 customer_code。
- **验收**：单测 9/9；`tools/judge_scope.py` 判官 6/6 全绿（city_manager/XXJL/STYY01/financial_accounting/provincial_general_manager/admin，Python 按 Java 独立复刻 vs Rust CLI 集合逐一致 + t_sales_order 行级 COUNT 同快照一致）；无角色员工 fail-closed exit=1。
- 坑：①tracing 必须走 stderr（sqlx 慢查询 WARN 混 stdout 毁 JSON）②现网实时写入使两次 COUNT 差 1——判官改单语句双子查询同快照。

## M2 语义层+检索（2026-07-23，已验收）
- `meta.rs`：PG `meta` schema（table_doc/column_doc/kw_force/pitfall/sql_exemplar 含 vector(512) 预留）；
  `meta sync` 采集 MySQL information_schema → 244 表/5488 列（备份表过滤：数字尾/bak_/copy/backups/del_log + 陈旧行清理）；
  `retrieve` 三路召回 = 关键词强制补表(必入) + word_similarity trgm 排序（中文短问句 similarity 不行，word_similarity 才行）。
- 资产迁移：旧库 skill_memory **234 条**全量入 meta.pitfall（45 pitfall/142 码表/26 值域/20 列修正/1 路由，tools/migrate_pitfalls.py）；
  20 表 ⚠️ schema 警告 + 关键词强制补表（含核心域主表保底：销售/订单/客户/商品/员工/门店）。
- pitfall 触发词形态=「表名.列名」——按**召回表名**匹配（旧设计：trigger 锚到会被检索到的表名上）。
- **验收**：单测 11/11；六问冒烟主表全命中（余额/销售/买过/市场费用/库存/分类排行），pitfall 召回 2~5 条/问。
- 坑：①information_schema 文本列被 sqlx 误识 LONGBLOB→全部 CAST AS CHAR（旧项目同款坑复发）；②TABLE_ROWS 是 BIGINT UNSIGNED→CAST AS SIGNED。

## M3 NL2SQL 流水线（2026-07-23，已验收）
- `llm.rs`：DeepSeek OpenAI 兼容 HTTP（precise 生成/fast 预留），无框架；extract_sql 抽围栏。
- `pipeline.rs`：检索→生成(schema+pitfall+few-shot+身份+今天日期注入)→安全校验→LIMIT护栏→权限注入→只读执行(30s超时)→few-shot 回写。
  - 安全校验：单条 SELECT / 敏感列 / 占位符幻觉（`__XX__`/`_placeholder`）/ into outfile 全拦。
  - 自修一次：安全校验失败或执行报错携错误重写（旧项目实证通道）。
  - system prompt 硬规则 7 条：权限勿臆造/名称 LIKE 不等值/遵守⚠️警告/deleted_flag/相对时间不硬编码年份/明细≥8列/禁占位符。
  - 结果 JSON：DECIMAL→字符串保精度，日期格式化，200 行截断标记。
- CLI `ask <login> "<问>" [role]` + HTTP `POST /api/ask`；前端 App.vue 对话+表格+SQL 折叠（Ant Design Vue）。
- **验收**：单测 22/22（+反引号表注入 e2e 复现锁死）；e2e_m3.py 7/7 全绿（超管全量 1.63亿/城市经理注入 12.9万<全量/明细13列/市场费用走合计表/名称LIKE）；
  Playwright 浏览器实测「本月销售额前五省份」出表（广东1405万…河北1135万，48s）。
- 坑：①LLM 非确定单次抽风生成幻觉列→e2e 对 LLM 路径重试一次（旧项目惯例）；②tanlibo 不带 role_code 默认取 role_id 最小角色（可能全权限），生产由登录 set-active-role 显式定，非 bug。
- 亮点：LLM 自觉遵守 t_sales_order_detail 2x 去重 pitfall（生成派生表 GROUP BY），口径注入生效。

## M6a 确定性快路径（2026-07-23，已验收）
- `direct.rs`：`try_direct` = 单号直查 + 高频销售聚合，命中 0-LLM，跳过生成但仍过安全校验+权限注入+只读执行（权限不旁路）。
  - 单号直查：HJXH-DXO/DSO(销售)/DRO(售后)/DZD(对账)/SPC-(赢销通) 前缀映射→表.主号列，出单据卡 SELECT *。
  - 高频聚合：时间窗(今天/昨天/本月/上月/本周/今年)×指标(销售额/订单数/客单价)，有效订单口径(剔 0/108/199)。
  - **剥词守卫**（旧项目实证）：去时间/指标/语气词后有残留=实体问句→回落 LLM（「恒众餐饮本月销售额」不误走全量模板）；维度词(排行/各/省/分类…)→回落。
- pipeline::ask 确定性优先，执行失败静默回落 LLM。
- **验收**：单测 28/28；e2e 9/9；**本月销售额 15~48s→1.1s**（direct-agg）、单号直查 1.2s 出 80 列卡；
  限权用户 direct-agg 值 12.9万=LLM 路径值（口径逐字一致），注入✅。
- 遗留：scope 计算连库慢（限权用户 11s，多在部门/客户集合查询）→ 加进程内缓存（当日过期，对齐 Java Redis）是下一优化点。

## M4 前端 BI 呈现（2026-07-23，基础已验收）
- 深度参考 SuperSonic chat-sdk（列语义 showType + getMsgContentType 决策树）+ 旧项目 ViewSpec V2 方案（第186轮）。
- 后端 `viewspec.rs`：列语义推断（role=metric/category/time/id + semantic=money/count/percent/geo/customer/goods/order）
  + 决策树 build()：①单行全指标→KPI卡 ②单行多列→实体卡 ③时间列+≥2行→趋势线 ④1类别+1指标：≤6全正非%→环形饼/≤50→柱(>18 TOP18收纳) ⑤兜底表格。AskResult 加 view 字段。
- 前端 `format.ts`（金额万/亿压缩¥、千分位、百分比、省码→省名字典）+ `BiChart.vue`（ECharts 封装：柱渐变/环形/趋势，单色明度纪律、TOP收纳、数值标签）
  + `App.vue` 按 view.blocks 渲染 KPI卡/实体卡/图表/表格（指标列右对齐+语义格式化）。
- **验收**：Rust 单测 35/35（viewspec 7）；vue-tsc 通过；Playwright 三形态实测——
  KPI卡「本月销售额 ¥1.64亿」1060ms、环形图「前五省份」单色明度+省名、表格金额格式化。
- 坑：饼图默认彩虹→改单色明度阶(榜首最深)；bodyStyle 须对象非字符串；省份存区划码→format 翻名。

## M4b SuperSonic 规格对齐 + KPI 自动环比（2026-07-23，已验收）
- probe-ss-view 回传 SuperSonic chat-sdk 完整源码级规格（决策树顺序/showType 枚举/图表阈值/getFormattedValue/环比 statistics/下钻 requery）。
- 校准：①Pie 阈值 6→10（对齐桌面）②Bar TOP 18→20（对齐 Trend slice20）③万压缩 2位→1位（对齐 getFormattedValue：亿2位/万1位）。
- **KPI 自动环比**（对齐 aggregateInfo.metricInfos.statistics）：direct-agg 单指标时平移时间窗查上期算 Δ%——
  direct.rs prev_window(本月→上月/今天→昨天/今年→去年…)、viewspec.rs patch_kpi_delta(上期0跳过/±0.05阈值判 up/down/flat)、
  前端 KPI 卡 ▲红▼绿 chip + 标签。实测「本月销售额 ¥1.64亿 ▼9.4% 较上月」1518ms。
- 验收：Rust 单测 36/36（+patch_kpi_delta 分支）；vue-tsc 过；Playwright 环比 chip 实测。
- SuperSonic 规格待用清单（下步）：下钻 requery（recommendedDimensions+onLoadData）/showType='more' 参考列剔除/authorized 列权限/趋势多指标 slice20。

## M6b scope 缓存 + AGE 图关系问答（2026-07-23，已验收）
- **scope 进程内缓存**（scope.rs::compute_scope_cached）：key=(登录名,角色)，当日过期（对齐 Java Redis）。
  限权用户第二问 **15.2s→3.4s**（省 ~11s 权限集合连库计算）。CLI 跨进程不共享，服务同进程受益。
- **AGE 图关系问答**（graph.rs，0-LLM）：客户-购买-商品图。
  - `graph sync`：MySQL 聚合有效订单口径的客户-商品边（277万明细→**2591客户/455商品/98759边**），UNWIND 批量建点建边入 AGE，239s 一次性。
  - 三类查询：buyers_of_goods(买过X的客户)/goods_of_customer(X买过什么)/copurchase(买X还买什么)，name 正则匹配+sum(amount) ORDER BY。
  - pipeline 图前置（仅全权限用户——图无行级权限，限权回落 LLM 走注入）；detect_relation 识别+剥词抽实体名。
  - **实测：图查询 11~38ms**（vs MySQL 关系查询 6~20s，快 300~1800 倍，兑现 AGE 选型）。
- 坑：agtype 类型 sqlx 不识别→外层包 `::text` cast 再解析（string 带引号 unquote、number 裸数字 parse）。
- **验收**：单测 39/39（+relation_detect/graph esc/unquote）；Playwright 实测「买过烤肠的客户」graph 38ms 单色柱图 TOP20+表格（鸣望 7475万降序）。

## M4c 下钻交互（2026-07-23，已验收，彻底参考 SuperSonic）
- 对齐 SuperSonic DrillDownDimensions + onLoadData 参数化重查（recommendedDimensions）。
- 后端 viewspec.rs `interact.drill`：有指标结果时推断可下钻维度（DIM_POOL=省份/商品分类/业务员/客户/门店/月份，剔除结果已用维度）。
- 前端 App.vue 结果底部下钻 chips「换个维度看：按X↓」，点击=原问题+"按X"参数化重问（lastQuestion 追踪）。
- **验收**：单测 39/39；vue-tsc 过；Playwright 下钻链实测——「本月销售额」KPI卡→点「按省份」→31省单色柱图+表格(广东1424万降序)，chips 更新剔除省份。
- 注：当前下钻走 LLM 重问（务实版）；SuperSonic 是纯 0-LLM 语义层重查——我方无语义层，SQL 改写下钻通用性差，LLM 重问更稳。dateShift 已有 0-LLM(prev_window)。

## 下一步（M5/M6c）
- M5 三端打通（DMS SSO 换签+嵌入页；企微应用）——走向最终交付形态。
- M6c：图关系+行级权限；实体锚定；语义缓存(接 embed)；SchemaCorrector(执行前幻觉列拦截)；graph sync 定时刷新。
