# dms-ai 迭代记录

> 计划与选型：`docs/REBUILD-PLAN.md`；整合蓝图（SuperSonic+deepagents 六期计划）：`docs/INTEGRATION-PLAN.md`。红线：DMS 生产 MySQL 只读（连接级 READ ONLY 兜底）。

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

## M5a 三端认证 + DMS SSO 嵌入（2026-07-23，已验收）
- `auth.rs`：会话 token 体系（uuid，12h 闲置 TTL 活跃滑动续期，>1000 项清理）+ DMS token 验真（调 getLoginInfo 拿 loginName）。
- 端点：`POST /api/sso`（验真 DMS token→颁会话 token）；`POST /api/ask` 身份优先级 = Authorization Bearer 会话 token > body.login_name（开发）。
- 前端：嵌入 boot（URL dms_token→自动 SSO→隐藏登录框「DMS 免登」）+ ask 带 Bearer。
- DMS 登录当时使用国密 SM4 ECB（旧常量已从仓库移除并应视为已轮换）+ 图形验证码（无法自动化，故 SSO 走验真路线不自登录）。
- **验收**：单测 40/40（+auth issue/resolve）；vue-tsc 过；
  **SSO 端到端对接生产 DMS**：假 token→getLoginInfo 返回 code 30007→我方正确验真失败透传原因；
  前端嵌入 boot Playwright 实测（URL dms_token→免登框→自动 SSO→失败提示准确）。
- 配置指引：docs/EMBED.md（DMS 外链菜单 frameUrl=?dms_token={token}，零 DMS 源码改动嵌入首页）。

## M5b 企微 OAuth 端#3（2026-07-23，已验收）
- `wework.rs`：access_token(进程内缓存 2h) + code→userid(auth/getuserinfo) + userid→手机号(user/get) + 手机号→t_employee.phone→login_name。
- `/api/wework/login?code=` → 完整链 → 会话 token → 302 到 `/#token=xxx`（fragment 不进服务端日志）；前端 boot 读 fragment token 免登+清 fragment。
- 映射依据：**phone 全员覆盖(3515/3515)最可靠**；open_id 是 24 位内部 id 非企微 userid。
- **验收**：单测 40/40；vue-tsc 过；
  **企微 API 真调**：gettoken errcode 0 拿到 token；getuserinfo 真调（当前 IP 未白名单→errcode 60020，证明对接正确，生产加 IP 白名单即可）。
- 配置指引 docs/EMBED.md 企微段（授权链接/回调链/可信 IP 白名单）。

## 三端认证全打通（阶段小结）
- 端#1 独立 Web：login_name（开发）；端#2 DMS 嵌入：SSO 验真(对接生产 DMS 坐实)；端#3 企微：OAuth(对接企微 API 坐实)。
- 三端权限计算一致：principal 从 DMS 生产库只读现算，1:1 复刻 @DataScope。

## M4d 界面重构：复刻旧 V1「皇家小虎·数据智能」对话流 BI（2026-07-23，已验收，用户指定）
- 参考旧 dms-copilot `static/index.html`（V1 成熟单文件前端）的设计语言与布局，用 Vue3 复刻。
- `theme.css`：靛蓝 #4051d3 主题 tokens（明暗双主题，brand-ink 渐变/shadow/radius 体系）。
- App.vue 重构为**对话流 ChatBI**：左侧栏（🐯皇家小虎 logo+明暗切换/会话历史/健康点+🔒纯查询模式）+ 主区（数据智能 brand 顶栏/气泡对话流/快捷 pill/输入栏 Enter 发送）。
- 气泡内结果面板：meta 行(路由 badge+行数+耗时+查看SQL) + KPI 卡(顶部靛蓝渐变条+大数字+环比chip) + 实体卡 + 图表卡 + double-bezel 表格(品牌底线+hover 强调条+斑马) + 下钻 chips。移除 AntDesignVue，纯自制 UI 对齐 V1。
- **验收**：vue-tsc 过；Playwright 明暗双主题实测——本月销售额→气泡 KPI 卡 ¥1.66亿 ▼8.4%较上月 321ms+下钻 chips；暗色深空底协调。
- 🔴 修真 bug：Vue3 深响应式下 `push(obj)` 后数组存 reactive 代理，改原始引用不触发更新→改用 `turns.value[len-1]` 取代理引用（否则 loading 永不清、结果不渲染）；fetch 加 100s AbortController 超时兜底(防 LLM 挂起永久 loading)。

## M6d 用户实测三问修复（2026-07-23，已验收）
用户 admin 实测报三问，全部定位+修复：
- **Q1 结果错**（本月销售额按商品分类→28801 全未分类）：根因=下钻走 LLM 丢原口径，LLM 拐到 t_marketing_goods(营销表)用参考价算。修=**销售额下钻确定性模板**（direct.rs sales_breakdown，0-LLM）覆盖省份/客户/业务员/门店/月份/商品分类，连接键连库坐实(detail.sku_code=goods.goods_code、goods.goods_category_code=cat.id)，有效订单口径。**81s→10s，答案对**(脆皮烤肠3000万/虎皮肉肠2203万...)。
- **Q3 图表错**（昨天订单明细→200 行画成趋势线）：根因=决策树趋势线判定抢在多类别前，明细有时间列就画折线。修=对齐 SuperSonic 顺序，**明细/多维(category>1 或有 id 列)→纯表格**，趋势线加 category≤1 约束。明细→table 不配图。
- **Q2 慢**（81s）：Q1 确定性模板 81s→10s；无 JOIN 维度(省份/客户/月份)更快。
- 商品分类模板提速：o 先过滤(本月订单少)驱动 JOIN detail 相关连接(sales_order_code 索引)，避免 detail 277万全表去重(曾 60s 超时→10s)。
- **验收**：单测 41/41(+sales_breakdown_dims)；连库 Q1 direct-agg 10s 正确；Q3 明细→table；Playwright 下钻链+暗色主题实测。

## M6e SuperSonic 深度移植①：SchemaCorrector 字段白名单校验（2026-07-23，已验收）
- ss-corrector agent 源码级深挖 SuperSonic Corrector 全家，产出移植规格（commit de60be3）。核心结论：控幻觉=LLM 产 SQL→纯 AST 校正器按字典掰回合法形态。
- 移植最高价值件 **SchemaCorrector.correctFieldName**（`corrector.rs`）：sqlparser visitor 提取「表别名→表」+「前缀.列」引用，对 meta.column_doc 真实列清单校验，
  幻觉列（前缀映射到 meta 已知物理表但列不存在）→ 携该表**真实可用列清单**自修一次（比执行报 1054 更早+纠正更准）。派生表/CTE 别名列、裸列、中文别名跳过防误伤。
- pipeline：generate→schema_check→有幻觉则 repair(hint)→execute，route=llm+schema-fix。
- **验收**：单测 44/44；CLI 连库实测——幻觉列 receiver_name(真名 receiver)拦截+列清单、幻觉外键 category_code(真名 goods_category_code)拦截、正常 SQL/派生表别名不误伤。命中旧项目两真坑(幽灵列/幻觉外键)。
- SuperSonic Corrector 移植清单（ss-corrector 规格，P0 可直接移植/无需语义层）：
  ①字段白名单校验✅ ②聚合函数名归一 ③group by 补全 ④select 补 groupby 字段+去重 ⑤自一致性投票(N次多数表决) ⑥默认时间范围+补下界 ⑦关键词→聚合/时间规则抽取 ⑧权限注入✅(已有) ⑨prompt 硬规则+结构化 schema 串。
  待做 P1(需轻量元数据)：值链接纠正、指标 defaultAgg 展开。

## M6g SuperSonic 深度移植③：指标注册表（语义层核心，2026-07-23，已验收）
- ss-semantic agent 源码级深挖 SuperSonic 语义层（MetricResp/DimensionResp/SchemaElement/知识库/prompt 装配/记忆闭环），产出移植规格。
- 移植最高价值件 **指标注册表**（meta.metric，对应 SuperSonic MetricResp 最小可用）：指标名+别名→来源表+聚合表达式+口径过滤+说明，**口径单一事实源**。
  首批 5 指标（口径旧项目连库验证）：销售额/订单数/客单价(t_sales_order 有效订单剔0/108/199)、市场费用(t_market_total_expense 合计表)、售后单数。
- `recall_metrics`：问句命中指标名/别名→注入 prompt「指标口径卡」（最高优先级，禁止 LLM 自选表/改算法），对齐 SuperSonic PromptHelper 的 Metrics 段。
- 配套修 sales_breakdown：触发词加「业绩」+ 时间窗可选（对齐 SuperSonic「问题没提时间就别加」）——「各区域经理业绩」现走 owner 确定性模板。
- **验收**：单测 48/48；连库——「各区域经理业绩」route=direct-agg 用对 t_sales_order(月月 5.8亿/蓝莓 1.1亿)；「本月市场费用」LLM+指标卡引导用对 t_market_total_expense(不再拐专项子表)。
- 语义层其余（HanLP trie 词典/pgvector 向量召回/记忆复核闭环 chat_memory）规格已存(ss-semantic)，按需后续移植；当前指标注册表已直击 Q1 类用错表根因。

## SuperSonic 移植进度小结
已移植：SchemaCorrector 字段校验(M6e)、GroupBy 补全(M6f)、指标注册表+口径注入(M6g)、ViewSpec 图表决策/KPI环比/下钻(早期)、few-shot(trgm)、权限注入。
待移植(按需)：聚合函数名归一、默认时间范围、自一致性投票(成本高跳过)、维度/术语注册表、embed 向量召回、记忆复核闭环。

## M5c 多会话持久化（2026-07-23，已验收，用户反复反馈）
- 修复「一问一会话」错误模型：一个会话(conv)含多轮问答(msg)，侧栏列会话(非单个问题)，对齐 SuperSonic conversation。
- 后端 `chat.rs`：PG chat.conv/chat.msg（按 login_name 归属，级联删除）；端点 GET /api/convs、POST /api/conv/new、GET /api/conv/{id}(回放)、DELETE /api/conv/{id}；ask 带 conv_id 存 user+ai 消息(payload=结果 jsonb)，首问设标题(前18字)。
- 前端 App.vue：convs 列表 + curConvId；侧栏列会话(标题+时间+删除, 当前高亮)；新建=建 conv 切过去清空；点击=回放 msgs 重建 turns；send 无会话先建、带 conv_id、成功刷侧栏。归属校验防越权。
- **验收**：vue-tsc 过；Playwright 实测——同会话内「本月销售额」「今天销售额」两问归**一条会话**(两轮对话)，侧栏一条(标题=首问)；新建→侧栏两条(新会话空+旧会话)、主区回欢迎语、当前高亮；切换回放/删除工作。

## M6h SuperSonic 深度移植④：多轮追问改写（2026-07-23，已验收）
- 移植 SuperSonic rewriteMultiTurn（NL2SQLParser.rewriteMultiTurn）：短追问结合会话上一轮问题改写成完整独立问题，直击「上下文不理解」。
- `pipeline.rs`：is_followup(≤14字+追问/指代词 那/再/呢/按/上个/它/该...) → rewrite_followup(fast 模型结合上一轮 question 改写) → 改写后走完整管线。
- `chat.rs::last_question`：取会话最近一轮 user 问题（本轮未落库，取到上一轮）。api_ask 按 conv_id 取 prev 传入。
- **验收**：单测 48/48；连库——同会话「本月销售额」(1.66亿)→追问「那上个月呢」改写成上月销售额(1.81亿, ≠本月)，route=direct-agg。

## M6i SuperSonic 深度移植⑤：记忆复核闭环（2026-07-23，已验收）
- 移植 SuperSonic MemoryReviewTask：成功问答不再无脑当范例，经 LLM 复核质量把关，判错的剔除不进 few-shot（防错误 SQL 当范例传播）。
- `meta.sql_exemplar` 加 status（pending/enabled/disabled）；回写时 pending；ask 成功后 `tokio::spawn` 异步 fast LLM 复核（判 SQL 是否正确回答问题→opinion POSITIVE/NEGATIVE）；
  few-shot 召回 `status != 'disabled'`（剔除判错的）。CLI `review-pending` 批量复核存量（对齐 SuperSonic 定时扫 pending）。
- **验收**：单测 48/48；批量复核 20 条→17 enabled+3 disabled；被剔除的正是 M6d 修过的坏 SQL（本月销售额按商品分类用'其他/未分类'全归一——旧 LLM 拐错表产物）+活动台账问题 SQL，复核精准。

## M6k SuperSonic 深度移植⑥：embed 向量召回（双召回补齐，2026-07-23，已验收）
- 移植 SuperSonic 双召回的向量半（EmbeddingMapper/MetaEmbeddingService）：词典/trgm 召回不足时，语义向量召回补上。
- `tools/embed_service.py`：bge-small-zh-v1.5(512维 fastembed 本地自包含)，build 给 meta.table_doc 算 embedding+HNSW 余弦索引，serve :8077 HTTP。
- `embed.rs`：Rust embed 客户端(reqwest 调 :8077，3s 超时，300s 熔断防挂起)；`to_pgvector` 字面量。
- `meta::retrieve` 三路召回：关键词强制 + **向量召回(embedding <=> query，HNSW)** + trgm。embed 挂则熔断降级(不阻塞)。
- 配套补 is_backup_table：_back/_history/_delete_history 结尾 + 6位日期段(YYMMDD)——清掉备份表污染(246→243 表)。
- **验收**：单测 48/48；口语化问题「顾客都买了些啥东西」「门店铺货」「钱货两清的单据」向量语义召回相关表(词典召回弱)，无备份表污染。
- ⚠️ 依赖 embed 服务常驻：`python tools/embed_service.py serve 8077`（掉线熔断降级不报错）。

## M6l SuperSonic 深度移植⑦：语义缓存（快，2026-07-23，已验收）
- 移植 SuperSonic 向量召回近义问答思想 + 旧项目护栏：近义历史问答命中即 0-LLM 秒出。
- `pipeline::try_semantic_cache`（direct 后 generate 前，非追问）：embed 问句→向量找最近义 enabled 语料(embedding<=>query，余弦距离<0.12)→**时间词/数字词护栏全等**(防"上月"命中"本月"、"前5"命中"前10")→复用其 SQL(数据实时查+权限按当轮注入)。
- 回写 spawn 加存问句向量(query 侧，与查询一致)；embed_service.py build 给存量 enabled 语料补向量。
- **验收**：单测 50/50(+时间/数字护栏)；连库——「昨天销售订单明细」近义命中 semantic-cache 4s(vs LLM 19s)；负面「上月销售额前五省份」时间词护栏拦住不误命中本月缓存(走 direct-agg)。

## M6n SuperSonic 深度移植⑧：结论洞察 textSummary（细节丰富，2026-07-23，已验收）
- 移植 SuperSonic textSummary：排行/趋势结果自动附一句确定性数据解读(0-LLM)。
- `viewspec::compute_insight`：①排行(类别+单指标≥5行)→榜首占比+CR3集中度(前三合计占%)；②趋势(时间+指标≥2行)→首末涨跌%。geo 列翻省名、空串归"未知"、金额万亿压缩。
- 前端 App.vue：AI 气泡结论区显示 insight(💡 靛蓝左边框洞察条)。
- **验收**：单测 50/50；连库+Playwright——排行「榜首未知¥5743.8万占34.5%；前三合计64.5%（共32项）」、趋势「从¥2.25亿到¥1.66亿整体下降26.1%」。
- 顺带修 sales_breakdown 省份 COALESCE→NULLIF(空串归未知)。

## M6u deepagents P0 移植：复杂问题拆解-并行-合并（2026-07-23，已验收）
- probe-deepagents 源码级调研 deepagents(langchain-ai)，四大支柱：planning(write_todos)/subagents(task隔离)/虚拟FS/detailed prompt。
- 移植最高价值 P0 = 复杂问题「规划→多步查询→合并」（对标其 text-to-sql-agent 简单直连 vs 复杂先规划分流 + deep_research 子代理并行范式）。
- `pipeline`：is_compound(明确「分别/对比+和」门控)→split_questions(fast 拆≤3子问题,write_todos 思想)→**并行执行各子问题**(futures::join_all,各走完整 ask_single 管线,独立上下文=deepagents subagent 隔离)→AskResult::compound 合并 subs。
- `ResultPanel.vue` 组件（result→完整呈现，主气泡+子面板复用）；复合时多子面板（🔹子问题标题+ResultPanel）。
- **验收**：单测52/52; 连库+Playwright——「分别统计各省销售额和各商品分类销量」→route=compound 拆2子并行(各省销售额 direct-agg 34行 + 各商品分类销量 llm 64行)，之前一条SQL超时90s；前端多子面板渲染。
- 遗留：商品分类「销量」列口径(detail 数量列/item_type)下轮优化。

## M6v deepagents P3 移植：SQL 预检只读红线硬拦（2026-07-23，已验收）
- is_safe_select 加显式只读红线（INSERT/UPDATE/DELETE/DROP/ALTER/TRUNCATE/CREATE/REPLACE/MERGE/GRANT/REVOKE，尾空格防误伤 deleted_flag/created_time 等列名）。
- 与数据库层 READ ONLY + AST Query 校验形成三道只读防线（DMS 只读铁律）。验收：单测 53/53。

## M6w 销量指标 — 修复商品分类销量 0（M6u 遗留，2026-07-23，已验收）
- meta.metric 加「销量」：SUM(box_quantity) item_type='1' 商品行（item_type 分列 1商品/2赠品/3结算），JOIN 有效订单+detail 2x 去重；清毒化语义缓存。
- 连库「各商品分类销量」用对口径（脆皮烤肠 189 万箱/商用蛋挞 147 万箱，之前全 0）。

## M6x SuperSonic 深度移植⑩：维度注册表（DimensionResp，2026-07-24）
- 移植 SuperSonic DimensionResp 最小可用：维度名+别名→来源表+取值表达式，**分组取数口径单一事实源**，根治 LLM 分组乱 JOIN/取错列（与指标注册表互补）。
- `meta.dimension` + `seed_dimensions` 首批 6 维（口径全部取自 direct.rs 已连库坐实的确定性模板）：
  省份(t_customer.province 区划码,空串归未知)/业务员(owner_manager→t_employee.actual_name)/客户(订单头 customer_name 快照)/门店(shop_name 快照)/商品分类(sku_code→goods→category 链)/月份(DATE_FORMAT '%Y-%m')。
- `recall_dimensions` 问句命中维度名/别名→注入 prompt「维度口径卡」（禁止臆造连接键），pipeline 注入位置在指标卡后、术语前。
- 价值场景：LLM 路径的「本月市场费用按区域」「各品类退款额」类跨域分组——指标卡定口径+维度卡定连接键，双卡夹逼。
- 验收：cargo check 过；单测 +dim_hit 名/别名/未命中（20 轮门禁批量跑）。

## M9a 蓝图第 1 期①：权限红线三连（2026-07-26，INTEGRATION-PLAN P0）
- 前置：深度调研 workflow（6 agent 精读 SuperSonic/deepagents 源码 + dms-ai 审计）→ `docs/INTEGRATION-PLAN.md` 六期蓝图定稿，五份调研报告归档 `docs/research/`。
- ①会话越权修复：api_conv_msgs 原**完全无鉴权**、api_ask 借他人 conv_id 可泄上一问/写入消息——接线 chat::conv_owner 归属校验，非属主 403；ai 行 question 改存 ""（原存用户问题，role 语义错乱；前端回放走 result 不受影响）。
- ②scope_binding 数据化 + fail-closed：meta.scope_binding 表（**scoped/global/via 三态**）+ 内置种子 seed_rules 灌表 + 启动 load_rules 进程注册表（无 PG 回退内置种子）；受限用户 SQL 涉及**未登记表一律拒绝**（原 8 表硬编码之外全放行 = 最大权限暗坑，蓝图 P0 首位）。
  - Java @DataScope 复核 15 个 mapper：新增 scoped 3 表（t_invoice_new_apply_header/t_device_inspection_header manager_code=Codes/t_long_promotion_person）；global 15 表（Java 无 @DataScope，1:1 全量可见：goods/employee/dict/仓库/winc 报表/市场费用等）。
  - **via 模式**堵明细独查泄漏：t_sales_order_detail/logistics、t_after_sales_order_detail 独查时 `EXISTS(SELECT 1 FROM 头表 __ds_h WHERE 键相等 AND 头表权限条件)`；头表同 SELECT 在场则跳过（防双重注入拖慢）；CTE 名入豁免集不误拦。
- ③承接上会话被重启打断的引擎 C WIP：失败复盘 review_failure（exec-error→fast LLM 复盘→候选教训）/review_lessons 复核 + zero-rows/exec-error 落 failure_log。
- 顺带修 2 个只验过 cargo check 的历史坏单测：viewspec「订单数」被 Semantic::Order 抢先判定永不算指标列（Count 前移，M6z 分组柱/双序列因此失效）；direct::strip_annotations 不剥 ASCII 中文括注（`t_sales_order_detail(JOIN ...)` 基表名带尾巴 → 组合器恒 None，M8d 两测挂）。
- 验收：单测 **89/89 全绿**（+7 inject 新测：fail-closed 拒绝/超管放行/via EXISTS 三断言/头表在场跳过/CTE 豁免/物流 via）。

## M9h/M9i 口径教训种子 + 规则时间解析（2026-07-26，第 3 期开工）
- **口径教训种子** `meta::seed_pitfalls`（8 条，连库实测坐实直接 active）+ 表警告 3 条，
  来自六域并行作题 workflow 的 33 条疑点：赠品用 item_type 勿用 is_gift（冲突 537+2591 行）、
  商品排行分组键歧义（按码/按名冠军不同）、费用分组必须用 expense_item_name、province 存行政区划码等。
- **⚠️ 立刻被评测抓到的自伤**：新种的「中台售后」教训措辞含"若用户语义是真实客户售后需显式说明"，
  LLM 读后**主动加了 `after_sales_type != '3'`**，售后单数 1176→329（漏 72%）。
  教训：**pitfall 措辞会直接改写 LLM 行为，必须先写死默认口径再讲例外**；已改为
  「默认一律不加 after_sales_type 过滤；只有用户明确说退货类/剔除中台才加」。
- **规则时间解析** `direct::time_predicate`（移植 SuperSonic TimeRangeParser）：
  原 `time_window` 只认 6 种相对词且列名硬编码 order_time → 现产出**列名占位 `{}` 的谓词模板**，
  覆盖 近/过去/最近 N 天|周|月|年（中文数字 三/十/十五/两）、第N季度·本/上季度、上下半年、
  N月份（含十二月且不误吃"上个月"）、今天·昨天·前天·本/上周·本/上月·今年·去年；
  解析结果注入 prompt「时间范围」段——LLM 不再自己拼日期函数。
- 评测工具加固：连库抖动重试（10054/10060 退避 3 次）+ 题间 2s 节流
  （38 题连跑把远程 MySQL 打到拒连，14 题假失败）；`cell()` 归一剥 %/千分位/货币符。
- 判定不做并记录理由：aho-corasick 精确词典层（需新增依赖违反项目红线，且元素不足 150 个
  substring+MapFilter 已够）；MySQL 方言归一（蓝图基于"目标库是 PG"的误解，我们本就查 MySQL）。
- 验收：单测 157/157。

## M9f 回归失败清单驱动的三修 + EXPLAIN 预检（2026-07-26，回归 48/54 逐条定位）
- 先修**回归脚本自身的假红**：红线判定用子串匹配，`deleted_flag` 被判 delete、`created_time` 被判 update
  → H01-H03 长期假失败（与 M9b 修的 Rust 侧同款 bug）。改 token 化判定 + 复核首 token 必为 select/with。
- **E16【真缺陷】**：「线下客户本月销售额」被手工模板 `sales_breakdown` 装配成「全部客户 TOP200」——
  "线下"这个客户分类限定**被静默丢弃**，答非所问（组合器有 `has_entity_residue` 守卫，手工模板没有）。
  修：守卫抽成通用 `has_residue(问句, 已消化词)`，手工模板按命中维度接入 `consumed_words`；
  长词优先剥离（防「客户分类」被「客户」拆散后残留「分类」）。单测钉住三类值过滤问句必回落 LLM。
- **E13**：`COUNT(*)` 不按指标口径归一。头表一单一行时数值虽同，**JOIN 明细后按行数虚增**。
  修：命中唯一「计数+去重」指标时改写为 `COUNT(DISTINCT 主键)`；多指标歧义或非去重规则保守跳过。
- **E08/E15 是题目 bug**：SQL 完全正确（双表 UNION + invoice_status='2'），但中文列别名
  ``AS `本月已开票金额` `` 含"已开票"被判禁词。禁词断言改为带引号字面量 `'已开票'` 才算命中。
- G01 复合拆解复测为通过（route=compound）；B03 为 MySQL 连接抖动噪声。
- **EXPLAIN 预翻译验证**（SuperSonic 解析期 dry-run）：execute 前 `EXPLAIN` 毫秒级验证列名/语法/类型，
  失败喂 repair 重试一次。**纯增益设计**：仅当数据库明确报错才判定；超时/连接故障不判定——
  网络抖动不该触发改写（可能把本来对的 SQL 改坏，还白花一次 LLM）。
- 顺带风险自查：受限用户（城市经理）跨 10 域实测，**fail-closed 无一误伤**（核心表 scoped/via、维表 global 覆盖到位）。
- 验收：单测 151/151。

## M9e 蓝图第 2 期①：MapFilter 召回净化 + 维度注册表去污（2026-07-26）
- **发现（探针导出全量注册表才看见）**：`autodiscover` 把**列注释原文**当维度名写入 meta.dimension——
  「配送状态：100:待配送, 200:配送中, 700:配送完成」「审批状态(如: Pending, Approved…)」这类整句当名字；
  且同名列在多表各注册一条 → 「所属公司编码」活跃 10 条。召回时会重复注入同一张卡、淹没真维度口径。
- **MapFilter**（移植 SuperSonic SchemaMapper 命中净化，中文适配四规则，`meta::map_filter` 纯函数）：
  R1 命中词 <2 字剔除 / R2 同名去重 / R3 命中词被更长命中词真包含则让位 / R4 同词有满分（名==命中词）则非满分让位。
  配套 `match_word`（同元素多别名命中取**最长**，"多少个订单" 优于 "多少单"）。
  接入 recall_metric_hits / recall_dimensions / recall_terms 三处——问「库存金额」不再同时拖出指标「库存量」（别名"库存"）两卡打架。
- **写入侧去污** `clean_dim_name`：列注释截到首个分隔符（中英文冒号/括号/逗号/斜杠）前，须 2~8 字纯中文，否则退回字典名。
  存量脏行按同规则停用 14 条（active 维度 74→60）。
- 验收：单测 146/146（+7：最长优先/同名去重/单字剔除/满分优先/互不影响/最长别名/注释清洗）。
- 教训：**注册表要定期导出肉眼看**——脏数据不报错、不失败，只是悄悄稀释召回质量；单测与评测都发现不了。

## M9d 蓝图第 1 期④：执行级评测门禁 + 它抓到的 5 个真缺陷（2026-07-26，准确率 72.7%→100%）
- **门禁**（移植 SuperSonic evaluation exec-only 思路）：`tools/evaluation.py` + `tools/eval_cases.json`（12 题跨聚合/分组/时间/权限/口径）。
  不比 SQL 文本，比**生成 SQL 与 gold SQL 各自执行的结果集**（行排序归一 + 0.5% 浮点容差 + 列名不比）——「SQL 看着对、数字错」才拦得住（既有 regression.py 的片段断言对下述 5 个缺陷全部放行）。
  顺带产出 p50/p95 延迟基线、tags 分层通过率、`eval_error_case.json`、带 commit hash 的 `eval_baseline.csv`。
  新增 CLI `exec-sql <login> "<sql>" [role]` 跑 gold（**三道防线一个不少**：只读红线 → 权限注入 → 只读连接）。
- **门禁首跑即抓 5 个真缺陷（全部已修）**：
  1. **口径过滤漏注入**：问「本月有多少个订单」LLM 写 COUNT(*) 且漏 order_status 有效订单过滤 → 数字虚高 17%。
     修：新校正器 `corrector::correct_caliber`（SuperSonic「指标 filter 恒生效」）——命中指标且主表匹配时按顶层 AND 逐条补缺失口径；
     反向问法（全部/含取消/不限状态）、多表 JOIN、子查询口径、用户已写该列条件 全部保守跳过。
  2. **指标召回漏口语问法**：「有多少个订单」不含"订单数"三字 → 指标未命中 → 口径卡与口径补全全部失效。修：补口语别名（多少个订单/几单/订单笔数…）。
  3. **时间列口径缺失**：`invoice_time` 本库**全 NULL（0/2280）**，LLM 用它必返 NULL；售后 after_sales_time vs created_time 摇摆。
     修：`meta.metric` 加 **time_col**（Java mapper 权威 + 连库核实非空：订单 order_time / 售后 after_sales_time / 开票 apply_time），口径卡钉死「时间过滤【必须】用 X 列」。
  4. **明细重复行虚增 41%**：`t_sales_order_detail` 系统级 2x 重复（实测 100.7 万行 vs 去重 83.2 万），组合器直接 SUM。
     修：`meta.metric` 加 **dedup_keys**，组合器基表换为 `(SELECT DISTINCT 键 FROM 表 WHERE 口径) 别名` 并把口径下推；
     **安全门控**：外层对基表引用的列必须全在去重键内，否则不装配（回落 LLM），绝不出错数。
  5. **JOIN 主表漏表级口径**：明细指标经时间桥 JOIN t_sales_order 未带「有效订单」→ 虚增。
     修：新增 `meta.table_scope`（SuperSonic 数据模型 model filter）——表被任何查询触及时恒成立的过滤，组合器按 FROM 中别名自动附加（去重子查询已下推的基表跳过）。
- **顺带修的架构缺陷**：`meta::seed` **只在 `meta sync` 子命令跑**，服务/CLI 启动都不 seed → 注册表种子改了永不生效（新增 time_col 在评测里全空才暴露）。
  修：统一 `bootstrap_meta`（migrate → seed → 权限档案灌表+加载），服务/ask/exec-sql 三条路径共用。
- **口径修正（连库坐实，注册表即真相）**：开票金额来源 = **老表 UNION ALL 新表**（实测本月新表 275 单 2819 万 vs 老表 16 单 73 万，只查老表漏算 97%）；组合器遇 UNION 来源不装配交 LLM。
- 分组默认 LIMIT 50 → **200**（60 个商品分类被静默截成 50，用户无感）。
- 验收：**评测 12/12 = 100%**（起点 72.7%）；单测 **139/139**（+15：口径补全 8 / 去重装配 4 / 表级口径 3）；p50 18.9s p95 33.3s。

## M8d S3② join 边注册表 + 跨基表组合（2026-07-24，组合器 v2）
- `meta.join_edge`（SuperSonic JoinPath 思想）：表间可连接边+**基数标注**（1:N 扇出/N:1 收敛），种子 5 边全部来自已坐实模板连接键（order↔detail/order→customer/order→employee/detail→goods/goods→category）。
- compose_sql v2：同基表直拼保留；跨基表 **BFS 最短路径**（≤3 跳）拼 FROM 链（维度片段内部别名原样保留，metric 裸列限定到 b0）。
- **扇出闸**：路径含 1:N 边时仅 COUNT(DISTINCT) 聚合可过——SUM 单头列走扇出边会按行数虚增（销售额×商品分类仍留手工模板，正因为它要 dd 去重）。
- **时间桥**：order_time 不在 FROM 内时，经一条 join_edge 桥接 t_sales_order o_time（销量×本月 类可用）。
- 修 sales_qty 注册：scope 去 d. 前缀改裸列（组合器自行限定别名）；注册表文本全角括注 strip_annotations 去除（半角括号是 SQL 语法不动）。
- 新兑现组合：**销量×省份/客户/业务员/门店**（detail→order N:1 链）、**销量×商品分类带时间窗**（同基表+时间桥）。
- 验收：cargo check 过；单测 +3（扇出拒绝/跨基表销量省份/时间桥），存量 5 个组合测试同步 4 参签名（批量跑留门禁）。

## M8c S3 通用组合器①：指标×维度数据驱动装配（2026-07-24，退役手工模板第一步）
- 移植 SuperSonic 语义层组合思想：问句=元素组合（指标×维度×时间窗），按注册表元数据装配 SQL，不按问题类型配模板。
- `direct.rs::try_compose`：命中指标注册表+维度注册表 → `compose_sql` 装配
  `SELECT dim.expr, metric.agg FROM dim.source_table(含JOIN) WHERE metric.scope(裸列限定基表别名) [时间窗] GROUP BY`。
- v1 门控（宁缺毋滥，不装配就回落）：同基表（dim.source_table 以 metric.source_table 开头）、口径无子查询（库存快照类）、
  实体守卫（剥时间/指标/维度/数词/连接词后有残留=实体问句，如「恒众餐饮本月销售额按客户」不装配）、
  时间窗仅 t_sales_order 基表（order_time 已知）；时间维度按时间排序其余按指标降序。
- `qualify_cols` 裸列限定器：引号字面量跳过/已有前缀跳过/函数名与 SQL 关键词白名单跳过。
- pipeline：compose 优先、sales_breakdown 手工模板兜底（detail 驱动的商品分类/未命中维度词如「按省」仍走模板），LLM 最后。
- 立刻兑现的组合（原模板没有）：订单数×任意注册维度、客户分类维度确定性化（原 LLM）。
- 验收：cargo check 过；单测 +5（qualify 引号/前缀/函数、装配省份、实体守卫、前 N 无时间、门控跳过）（批量跑留门禁；重启并入 20 轮门禁）。

## M8b S2 元素级向量召回（SuperSonic SchemaMapper）+ B+ 纠错反哺（2026-07-24）
- **元素注册表 meta.element**：metric/dimension/value/term 四注册表统一为可召回元素（SuperSonic SchemaElement）；
  `sync_elements` 幂等同步（search_text 变化自动清向量待重建），挂接 meta sync 与 autodiscover 尾部。
- `recall_elements`：问句 embed → HNSW 近邻（余弦距离 <0.35）→ 渲染口径卡，embed 缺席熔断降级；
  pipeline 注入「语义召回元素」段，与 substring 命中按元素名去重——口语化问法不再靠关键词穷举（自适应召回层）。
- embed_service.py build 支持元素向量化 + HNSW（900 元素已建）。
- **B+ 纠错反哺环**：meta.correction_log + pipeline 四校正器（schema/groupby/agg/value）出手即记录（kind/question/detail），
  为「同错累计≥3 升格 pitfall」攒数据（升格器 S4）。
- 验收：cargo check 过（连库问答验收留 20 轮门禁）。

## M8a 自适应·自进化重构：总纲 + 引擎 A1 字典码列自动对码（2026-07-24，用户战略纠偏）
- **用户纠偏**：此前改动太「查 A 改 A」点状化，要的是自适应、自进化——一切知识是数据不是代码，一切组合是通用装配不是模板，用得越多越聪明。
- `docs/ADAPTIVE-REFACTOR.md` 总纲：点状硬编码清单退役计划；SuperSonic/deepagents 全功能映射表；自进化三引擎（A 自动发现/B 使用中学习/C 失败复盘）；S1→S6 路线。
- **引擎 A1**（meta.autodiscover_dict_columns + CLI `meta autodiscover`）：
  码型后缀列(*_code/_type/_status/_class/_mode/_way/_level)+小表(row_estimate<100万) → 只读 DISTINCT 抽样(≤61值) → 值集 ⊆ dict key 码集(覆盖≥80% 且 2~60 值)
  → 自动注册 value_map(eq,字典全码)+dimension(CASE 翻名)。人工种子优先不覆盖；幂等重跑（字典变了重跑即自适应）。
- 实跑三坑：①cargo run/build 在 Git Bash 下用残缺 mingw 链接失败（crt2.o），须 WinLibs mingw64 前置 PATH（build.ps1 同款）；②旧服务进程锁 exe(os error 5)，先停后建；③row_estimate 严重失真（29 行表真实扫描分钟级）致探针悬挂 → 单探针 10s tokio 超时跳过。
- 实跑结果：**854 候选 → 843 探针（10s/探针超时）→ 65 注册干净资产**（全 name 对齐或大集合直通，零撞车）：
  order_status 销售订单状态全 12 码、after_sales 售后订单状态、shop_type/level 门店类型/等级、company_code 所属公司 ×11 表一致、
  expense_type 费用类型、帐余业务类型 23 码、payment_terms 付款条件、channel_code 渠道细分 11 码、warehouse 仓库类型等。
  元素注册表 900 元素（74 维度+12 指标+5 术语+809 码值）全部向量化（HNSW）。
- 防误配两轮血泪（都转成单测锁死）：①数值小码集互相撞车（menu_type 撞对账单状态、wms_type 撞 28 项发票类型）；
  ②含字母码的字典也是撞车磁铁（data_scope_type={1,2} 撞联系人类型、审批状态撞设备处置状态）。
  终版规则：**注释点名优先**（注释写「数据字典 X」只评 X）+ ≥8 值 cov=1.0 直通 + 名称对齐（≥3 字公共子串）；alpha 码闸门全撤。
  **自适应必须带精度闸：错误映射比没有映射更糟，宁缺毋滥。**
- 验收：cargo check 过；单测 +3（dict_match 基本/拒绝/撞车守卫+注释点名）（批量跑留门禁）；实跑三轮收敛。

## M7g embed 服务常驻化（2026-07-24，语义缓存/向量召回不掉线）
- `scripts/run.ps1` 重写为全栈联动：PG 容器缺席自动 `docker compose up -d` → embed 服务(:8077) 缺席自动拉起（模型加载轮询 20s）→ 编译启动后端；embed 失败明确打印「熔断降级」不装死。
- `tools/embed_service.py` 补 `GET /health`（{"ok","model","dim"}）——原来只有 POST /embed，健康探测无从谈起。
- 坑：PowerShell 5.1 对无 BOM 的 UTF-8 .ps1 按 ANSI 读→中文注释毁字符串→语法错误；scripts/*.ps1 必须 UTF-8 **带 BOM**（PSParser 语法校验过）。
- 验收：py_compile + ps1 PSParser 语法过（全栈拉起实测留 20 轮门禁）。

## M7f 维度注册表扩充②：客户分类/类型（字典坐实）+ 商品分类模板误伤修复（2026-07-24）
- **坐实方式**：`tools/probe_values.py` 只读探针（SET SESSION TRANSACTION READ ONLY，小表 GROUP BY 抽样）——
  customer_class 100% 填充（04线下客户占 96%）、customer_type Z001/Z002 两值、group1/business_type/sale_platform 全 NULL 死列（不做）；
  字典表 t_dict_key/t_dict_value 坐实码表：CustClassif(01货架~99外部客户的店铺 7码)、CUST_TYPE(Z001~Z005 5码)。
- 维度 +2：**客户分类**/**客户类型**（CASE 翻名免字典 JOIN，NULL 归'未分类'，desc 注明字典 key 来源）。
- value_map +2 组：customer_class 7 码/customer_type 5 码（「线下客户的销售额」类过滤问句直写中文名→确定性换码）。
- **修真 bug**：「本月销售额按客户分类」被 detect_sales_dim 的「分类」抢先命中商品分类模板=答非所问——
  客户分类/客户类别/客户类型/客户种类 前置拦截回落 LLM（维度卡接管）。
- 回归题集 +2（E16 过滤换码/E17 客户分类不误走），累计 53 题。
- 验收：cargo check 过；单测 +2 断言（客户分类/类型不命中商品模板）（批量跑留门禁）。

## M7e 前端体验：loading 耗时 + 错误重试 + 发送防抖（2026-07-24）
- **loading 假死感消除**：thinking 气泡实时跳动已耗时秒数（1s interval，查询结束清）+ 「大数据量查询约需 10~60 秒」预期提示。
- **错误可恢复**：错误气泡加 ⚠️ 图标 + 「↻ 重试」按钮（取上一轮用户问题原样重发，避免手敲）。
- **发送防抖可视化**：查询进行中发送按钮禁用且文案变「查询中」（原只有 send() 内部静默 return，用户点了没反应会困惑）。
- 验收：vue-tsc 过（Playwright 实测留 20 轮门禁）。

## M7d M7 判官门禁：回归题集框架（2026-07-24，51 题 + 1 关系断言 ≥50）
- `tools/regression_cases.json` 题集覆盖全里程碑行为面：
  A 确定性聚合×12（direct-agg 路由/KPI环比/有效订单口径/客户数去重）、B 下钻模板×11（六维度/LIMIT前N/趋势饼柱形态）、C 单号直查、D 权限注入×3（城市经理含注入+超管无注入）、
  E LLM 路径口径卡×15（市场费用合计表/名称LIKE/明细≥8列纯表格/销量item_type/开票筛状态+换码/库存快照/售后DISTINCT/品牌维度卡/专票值链接）、
  F 图问答×4（买过X/共购/客户买过/限权回落不走图）、G 复合拆解×2、H 安全红线×3（DML 不得出现于执行 SQL）。
  rules：城市经理值 < 超管全量（权限隔离数值断言）。
- `tools/regression.py` runner：CLI ask 驱动；断言 路由/SQL含禁(忽略大小写空白)/行数/列数/view0/chart_kind/JSON片段/红线DML扫描；LLM 题重试 1 次（旧惯例）；embed/graph 依赖缺席自动 ⏭️ 跳过不算失败；--filter 按名筛题。
- 验收：py_compile + JSON 解析过（51 题 1 规则）；全量连库执行 = 20 轮门禁动作。

## M7c M6c 收尾：AGE 图 nightly 定时刷新（2026-07-24）
- 服务启动 spawn 图刷新循环：`secs_until_next_3am` 算下个本地 03:00（chrono::Local，DST/歧义兜底 1h），睡到低谷期一次性全量重建（~4min，MySQL 只读聚合无压力）。
- 失败记 warn 次日重试不拖垮服务；结果落 `AppState.graph_status`（Arc<Mutex<String>>），`/api/health` 新增 `graph_sync` 字段可观测（never/ok 时间戳 三元组/fail 原因）。
- 设计说明：图无行级权限仍仅全权限用户走图前置（M6b 语义不变）；当日增量订单次日 03:00 补齐，图问答为关系型场景（买过X的客户）对时效不敏感。
- 验收：cargo check 过；单测 +1（next_3am 必在 (60s,24h]）（20 轮门禁批量跑+health 实测）。

## M7b SuperSonic 深度移植⑫：ValueLinker 值链接纠正（2026-07-24）
- 移植 SuperSonic 值链接（SchemaMapper value mapping）：编码列上「中文名直写」确定性换码，直击 pitfall 反复出现的真坑——`invoice_status='已开票'` 必返 0 行（库存码 2）、`paid_way='可开票余额支付'` 等值必返 0 行（真库逗号组合值须 LIKE）。
- `meta.value_map` 码表（表,列,中文名,码,match_kind）：种子 11 组全部来自 pitfall 已坐实教训——invoice_status(11码)/invoice_type(普专票)/order_status(暂存0/无效108/作废199)/paid_way(ZX01等值+余额类like)/balance_type(5码,在线支付15/99歧义不收录)/bill_status/account_mode/item_type(1商品2赠品3结算)。
- `corrector.rs` Linker（sqlparser **VisitMut**）：`col='名'`（含镜像）→eq 换码 / like 列改写 `LIKE '%码%'`；`IN('名1','名2')` 逐项换（like 列跳过）；门控=带前缀且前缀映射 meta 已知表，裸列/已是码/码表无名全不动。
- pipeline 接线：correct_agg 后 correct_value（AST 校正链：schema→groupby→agg→value，全确定性 0-LLM）。
- 验收：cargo check 过；单测 +8（eq/镜像/like改写/IN逐项/like列IN跳过/裸列不动/已是码不动/无名不动）（20 轮门禁批量跑）。

## M7a 注册表扩充②：开票/活动指标 + 品牌维度（2026-07-24，口径 PG 元数据+码表教训坐实）
- 坐实方式：本地 PG meta.column_doc（MySQL information_schema 采集）+ meta.pitfall 码表教训，不猜口径。
- 指标 +3：
  - **开票金额** SUM(invoice_amount) 筛 invoice_status='2'（码表 InvoiceStatusEnum 坐实：0未申请/1申请中/2已开票/…，不筛把申请中/失败虚增）；desc 带发票双流并行教训（老表 IO* + 新表 SQ* 交集为0，全量须 UNION ALL）。
  - **活动费用** SUM(total_amount) / **活动场次** COUNT(DISTINCT activity_no)（t_activity_main，status 暂存/待申请/已申请/完成语义入 desc）。
- 维度 +1：**品牌** = t_goods.brand_name（明细行无品牌列，连接键 d.sku_code=g.goods_code，空串归'未归属'）。
- 客户分类维度暂缓：t_customer 有 customer_class/customer_type/group1 三个候选列，值域（码 or 名）未坐实，不猜。
- 验收：cargo check 过（种子数据随服务启动 upsert；连库问答验收留 20 轮门禁）。

## M6z 多指标图表形态：分组柱 + 多序列趋势 + 双值轴（2026-07-24，M4b 待用清单收尾）
- 补齐 SuperSonic 多指标呈现规格（待用清单最后一件「趋势多指标」）：
  - viewspec 决策树加 4b：**一类别 + ≥2 指标 → 分组柱图**（TOP20 收纳照旧），不再落纯表格；趋势分支本已透传多指标（双序列）。
  - BiChart 多序列配色：单序列保持品牌单色纪律，**多序列切区分色板**（#1677ff/#13c2c2/#fa8c16/#722ed1/#eb2f96）——修掉多序列全撞同一蓝无法区分的缺陷。
  - **双值轴**：两指标且语义不同（金额 vs 单量/占比）→ 左右 yAxis 各挂一条序列，量纲悬殊不互相压扁；多序列面积填充关掉（防遮挡）。
- 验收：cargo check 过；vue-tsc 过；单测 +2（分组柱 y=2 列 / 双序列趋势 y=2 列）（20 轮门禁批量跑）。

## M6y SuperSonic 深度移植⑪：AggCorrector 聚合函数名归一（2026-07-24）
- 移植 SuperSonic correctAggFunction：命中指标卡的聚合列必须归一到注册表默认聚合（口径单一事实源再落一刀）。
  问「订单数」LLM 写 COUNT(sales_order_code) → **COUNT(DISTINCT sales_order_code)**；问「销售额」写 AVG(total_amount) → **SUM(total_amount)**。
- `corrector.rs`：`parse_agg_rule`（agg_expr 解出 (函数,列,DISTINCT)，客单价类复合表达式保守跳过）+ `normalize_agg`（纯 AST 改写，可单测）+ `correct_agg`（问句命中指标→建规则，同列多指标歧义跳过）。
- 保守门控：仅顶层 SELECT 投影（子查询/WHERE 不碰）；COUNT(\*) 不碰；同列已被目标函数占用（SUM/AVG 对比问法）不改名防撞重复列；只下钻 Nested/Cast/Unary/BinaryOp 包装层。
- pipeline 接线：fix_group_by 后 correct_agg（两个纯 AST 校正串行，均不调 LLM）。
- 验收：cargo check 过；单测 +8（规则解析/复合跳过/DISTINCT 补齐/函数归一/已正确不动/COUNT(\*)不动/占用守卫/子查询不碰/异列不动）（20 轮门禁批量跑）。

## SuperSonic 移植累计（11 件）+ deepagents 2 件
SchemaCorrector 字段校验(M6e)、GroupBy 补全(M6f)、指标注册表(M6g)、多会话 conversation(M5c)、rewriteMultiTurn 追问改写(M6h)、MemoryReviewTask 记忆复核(M6i)、embed 向量召回(M6k)、语义缓存(M6l)、textSummary 洞察(M6n)、术语注册表(M6q)、维度注册表(M6x)、AggCorrector 聚合归一(M6y)。deepagents：复杂问题拆解-并行-合并(M6u)、只读红线预检(M6v)。
待搬(纯逻辑)：默认时间范围+补下界、自一致性投票(成本高暂缓)、值链接纠正(需轻量元数据)。

## A0 架构终稿 + 权限红线三修 + workspace 骨架（2026-07-27）
- **架构定稿** `docs/ARCHITECTURE.md`：v1（6-crate 受控内核）+ v2（多数据源 + 知识库）合并终稿，细到「每文件一个变更原因 + 行数预算 + 从哪个 file:line 搬来 + 留下哪个可运行检查」。
  产出方式：7 crate 并行文件级设计 → 跨 crate 契约统一（20 条权威签名 + 25 条冲突裁决）→ 4 路对抗评审（过度设计 / 上帝文件与函数肥胖 / 功能不许丢 / 红线不破）。
  - **减法**：砍掉约 1800 行、13 个文件的空抽象 —— `AskRun` 状态机（575 行 + 8 回调包住 `for attempt in 0..2` 的三个决策）、`RowPolicy`/`RuleTablePolicy`/`col_mask`（7 个 crate 里零调用点）、`kernel::present` 的 11 字段 `PresentLexicon`（其中一个字段参数化的是 `viewspec.rs:190-192` 的空 if = 死代码）、`Dialect` 8→4 方法、4 份没有重试循环的 `is_transient`、只有一个任务的 jobs 注册表、零 dyn 调用点的 `IdentityProvider`。
  - **推翻既有裁决 12 条**（记入 `_DECISIONS.md` 二·C）：最关键是 T10-2 **不拆 bin** —— `scripts/run.ps1:29` 起的是 `dms-ai-server.exe` 无参（今天=serve），拆后启动的是 CLI、空 args 直接退出，M7g 全栈脚本表面成功而后端根本没起。
  - 三处「同一信任边界两份实现」合并（敏感列 2 词 vs 9 词、`SafeIdent` 三套字符集、上传白名单两份）—— 漂出来的宽松那份就是入口。
- **权限红线三修**（评审新发现，全部带 file:line）：
  - **F1 注入 fail-open**（`inject.rs:243`）：条件 parse 失败被 `if let Ok` 静默丢弃 = 权限条件消失、查询照跑。改 `bail!`，**且 parse 成功但 `peek_token() != EOF` 同样阻断** —— `x.owner manager in (1)` 会前缀解析成功（只吃 `x.owner`），是比原缺陷更隐蔽的截断式越权。
  - **F5 敏感列两份真相源 + `SELECT *` 全绕过**：执行侧只拦 `login_pwd|password` 两词，给 LLM 剔 schema 的有 9 词 → `SELECT id_card FROM t_employee` 全程绿灯（该表还是 Global 免注入）。改：`meta::SENSITIVE_COLS` 单一事实源 + 防线移到 `execute` 的**结果列**（整列置 Null + warn），单号直查恒 `SELECT *` 因此一并堵住。
  - **F3b 系统库 deny-list**：`information_schema.`/`mysql.`/`sys.`/`performance_schema.`/`pg_catalog.` 一律拒（按「库名.」形态匹配，不误伤 `sys_no`/`meta_flag` 这类列名）。这是 F2 类型闸门（要等 T3 的三段 newtype）在今天的等效防线。
  - 顺手把 `is_safe_select` 45 行拆成 4 个判定 helper（对齐单函数 ≤60 行纪律）。
- **T1 workspace 骨架**：6 个空 crate（kernel/connector/policy/semantic/knowledge/agent），每个 `lib.rs` 把自己的硬规则写在 crate 文档里；依赖版本收进 `[workspace.dependencies]` 单一事实源；server 只挂 path 依赖不 use（`cargo build` 行为不变）。
- **架构门禁** `scripts/check-arch.ps1`：只有 connector 能造池 / kernel 零 IO 零 DMS 语料 / agent 不引 axum / semantic 与 knowledge 不依赖 policy / server 的 reqwest 仅在身份面 / `cargo tree` 实证依赖单向无环。注释行不参与匹配（否则「不得引 axum」这句话会把自己判红）；server 的池与裸 SQL 规则暂为 warn（100 处，T10 收口后删 `-WarnOnly` 转 FAIL）。
- **验收**：单测 **162 passed / 0 failed**（157 存量一字未动 + 5 新增：完整表达式解析、截断式条件必拒、9 类敏感列必拒、系统库必拒、结果列脱敏）；`cargo build --workspace` 绿；架构门禁全绿；判官题集扫过一遍无 SQL 触到新 deny-list。
- **待业务裁决**：受限用户查「只有 customer 段且客户集合为空」的 4 张表（余额/设备台账等）时段全空 → 不注入 → 看到全表。**与 Java 一致**（空集/不注入=放行），改 `(1=0)` 会让 Rust 与 `judge_scope.py` 的独立 Java 复刻分叉。建议收紧但需同改判官并知会 DMS 团队；本轮只加 `empty_segments_allows_today` 钉住现状。

## T2 kernel 纯算法下沉 + K1 知识库地基（2026-07-27，两轨并行）
- **轨 A / T2**：三路并行把纯算法搬进 `dms-kernel`（文件所有权互不重叠，逐行搬运、只提取子函数不改逻辑）。
  - `errors.rs`（GuardError/PolicyError，**Display 文案逐字等于旧 anyhow 消息** + 漂移单测钉住——repair 轮把 `e.to_string()` 喂 LLM，改文案等于改 prompt）、`ds.rs`（DsId 契约位）、`sql/{lex,ast,guard,dialect}.rs`、`policy/{scope,rules,inject}.rs`、`nl/{time,lexicon,text}.rs`、`present.rs`（只搬类型，算法留 server 待 T7）。
  - 注入算法改吃 `&RuleSet`（裁决 C2），32 张 DMS 表种子与 PG 注册表 IO 留 server；`rule_of` 不留第二份。
  - `prev_window` 由写死 `order_time` 改为**列名占位模板**（与 `time_predicate` 同形），server 侧 `fill_time_col` 填 —— 唯一的语义等价改写，靠 `agg_hits_month_sales` 等断言证明最终 SQL 字节不变。
  - `SENSITIVE_COLS` 收进 `kernel::nl::lexicon`，guard 的执行侧判定与 meta 的 schema 过滤共用一份（F5 的收敛落地）。
  - 门禁 `kernel 不得含 DMS 表名` 会连测试代码一起 grep，故 15 个断言含真表名的搬运测试落 `crates/kernel/tests/`（集成 target，仍归 `cargo test -p dms-kernel`）；kernel 自守测试一律泛化表名。
- **轨 B / K1**：知识库地基跑通。
  - `tools/embed_service.py` +330 行：`POST /parse`（pdf/docx/xlsx/csv/pptx/md/txt，**扫描版 PDF 显式报 `no_text_layer` 而非静默返空**；xlsx/csv 只出 `sheets` 单元格矩阵，markdown 文本通道由 Rust 侧生成，避免两个真相源；GBK csv 兜底）、`POST /chunk`（标题层级优先，**400 token / 重叠 60**，中文 1.6 字符/token；单块上限断言 480）、`/health` 加 `parse_ok` 逐类型报可用性，解析库全部**惰性 import**（缺依赖只让该类型 422，`/embed` 与问数路径不受影响）、`selftest` 子命令。
  - `crates/semantic/migrations/0020_kb_init.sql`：kb schema（space/doc/chunk/acl），`kb.acl` 带 **`perm` read/write**（不设它连「可读不可写」都表达不了 → 任何认证用户都能往他人空间投毒写，而带引用的回答会让同事读到伪造的「制度原文」）。
  - `connector/{doc,embed}.rs`：文档服务客户端（parse 120s / chunk 30s / 300s 熔断；四个确定性错误码落成变体）+ EmbedClient 实例化（批量 + query/passage 双模式），server 侧 `embed.rs` 收成薄包装、调用点一行不改。
  - `knowledge/{store,acl,ingest}.rs`：状态机 pending→parsing→chunked→embedded/failed 每步落库；sha256 走 PG 内置（零新依赖）；uuid 文件名落盘、原名只入库（防路径穿越）；**白名单与大小上限只有 ingest 一处实现**（server 不许再判一遍）；`acl.rs` 独立成文件（本 crate 唯一越权面，`AclEntry` 取代 5 个 `&str` 连排）；embed 挂时停在 `chunked` 并记错，文本检索仍可用。
  - `server/kb_api.rs` + 路由：上传/列表/详情/删除，`DefaultBodyLimit` 按配置（axum 默认 2MB 会先于 50MB 触发）+ 上传并发闸 `Semaphore(4)`（50MB 全量入内存 × N 并发会打爆）。
- **对抗评审抓到的真问题**：A3 删掉 direct.rs 的 3 个测试、以为 A1 会在 kernel 收下，A1 没收 —— 那 3 个是 `qualify_cols`/`base_col_refs`/`from_table_aliases` 搬运的**唯一覆盖**，被删后处于零覆盖。收尾 agent 从 `git show HEAD` 逐字恢复，首跑即绿（证明 kernel 版与搬运前逐字节一致）。
- **team-lead 收尾**：`too_large`/`not_found` 两个码原本落进 `Api{}` 被吞成 500「文档服务不可用」——把用户的文件问题报成我们的故障，补成确定性变体 → 400/404；清掉 3 个迁移期 warning（含 kernel 唯一的 `unused_mut`）；`knowledge` 暂列门禁 `-WarnOnly`（K1 阶段必须写 kb.* 的 SQL，而 T4 的 `OwnedStore::fixed()` 通道还不存在，转 FAIL 时点写进脚本注释）；把 trackB 计划里作废的 700/80 改成 400/60。
- **验收**：`cargo build --workspace` 0 error（余 3 个既有 warning）；`cargo test --workspace` **229 passed / 0 failed**（基线 162 全在位：server 143 + kernel 19 迁入；新增 62；5 个过渡期同名副本 T5/T7 清理）；架构门禁 exit 0（14 条，2 条预期 warn）；`python tools/embed_service.py selftest` 通过；46 个 `.rs`，kernel 最大文件 406 行。
- **待办（连库/环境，未做）**：`.venv` 缺四个解析库（`/health.parse_ok` 现诚实报 false）—— 其中 **PyMuPDF 系是 AGPL-3.0，商用内网系统需先过许可**，建议换 `pypdf`(BSD) 或 `pdfplumber`(MIT)；`PostgresDialect` 两条探针未连库实测；`store::migrate` 必须排在 `meta::migrate` 之后（依赖 vector/pg_trgm 扩展）未连库验证。

## T3 三段闸门 + K2 知识库检索与引用回答（2026-07-27，两轨并行）
- **轨 A / T3：把「execute 收任意字符串」这个洞焊死**
  - `kernel/sql/gate.rs`：`RawSql` / `CheckedSql` / `ScopedSql` 三段 newtype，字段全私有；
    `check()` = 只读红线 → LIMIT 护栏 → 抽表名（顺序不换位：被校验的必须是调用方原文，追加的 LIMIT 是我们自己的常量）；
    `ScopedSql` 的产出点只有 `inject()` 与 `ScopedSql::unrestricted(_, &UnrestrictedProof)`。
    「外部无法构造」用真 `compile_fail` doctest 守（以下游 crate 视角编译，字段私有确实 E0451），不引 trybuild。
  - `pipeline::gate()` 是唯一收口，四条执行路径 + CLI `exec-sql` 判官 + autodiscover 动态探针全部经它；
    `execute`/`explain_check` 只吃 `&ScopedSql`，传字符串编译不过。
  - **F2 的双证据一开始是自证的（对抗评审抓到）**：`UnrestrictedProof::new(sets, admin || sets.is_unrestricted())`
    的第二个证据在该分支内恒真 → `ScopeSets::default()` 仍能自己把自己证成放行，F2 的核心保护被抵消。
    修法：`compute_scope` 现在返回 `Scope{sets, unrestricted_by_role}`（字段私有），
    第二个证据来自**角色档**（超管短路 / 基础档 ALL 两个来源之一），不再把 sets 喂回去。
    新增回归锁 `empty_sets_alone_cannot_mint_release`：空集 + 角色档未授权 → 必须被拒。
  - `PolicyError::NeedsProof` 落地（原为 `Parse` 占位）：无限制档经 `inject()` 会拿到一条语义正确的权限错，
    而不是一条 Parse 面目的错。
  - **F2 审计**（`grep UnrestrictedProof::new|ScopedSql::unrestricted`，剔注释）：生产放行出口**只有 2 处** ——
    `pipeline.rs`（gate 的放行分支）与 `meta.rs`（`meta autodiscover` CLI 管理任务，SQL 由 information_schema
    表名列名拼装、零用户输入），与 ARCHITECTURE §5 说的「只应有两处」一致。
- **轨 B / K2：知识库能问了**
  - `kernel/{answer,llm}.rs`：`Answer/AnswerBody{Table,Text,Composite}/Citation`（`#[serde(tag="kind")]` 写死 ——
    默认 externally tagged + flatten 会在运行时报「can only flatten structs and maps」= 500 + 三个判官 JSONDecodeError）
    + `ChatModel` 契约。`Steps` 变体不建（零生产者），`tools` 字段不建（v1 不做 ReAct）。
  - `knowledge/retrieve.rs`：ACL 内联 SQL（不做查完再过滤）+ 三路召回（向量 HNSW / tsvector / trgm）
    + RRF 融合 + 同文档相邻块合并；可见 doc 少时走精确扫描（HNSW 先取到的邻居可能全不可见）。
  - `knowledge/answer.rs`：三条纪律全在一个文件 —— 无命中**不调 LLM** 直接说没有（省钱且杜绝模型用自身知识编答案）、
    `wrap_untrusted` 含闭合标签逃逸测试、截断三件套。LLM 接入用 `impl ChatModel for LlmClient`（约 20 行），
    不新建第二个 HTTP 客户端。
  - `POST /api/kb/ask` + `GET /api/kb/chunk/{id}`（引用原文回查，过 `doc_for_viewer` 非属主 403）。
  - `tools/kb_eval.py` + 4 份 fixture：五类题（recall@6 / 引用正确性 / ACL 越权必拒 / **注入必拒** / 无命中必说没有），
    注入两个载体（文档正文 + CSV 表头，与 xlsx 走同一条 sheets 渲染路径）。依赖缺席时 skip 退 0，
    但**出现「注入防线未实测」警告即等于注入题没跑**，验收时不许放过。
  - 前端补 `KbAnswer.vue`（自写最小 markdown 渲染，零新依赖 + 角标可点回查原文）；
    `ResultPanel.vue` 的 `view` 改可选 —— 知识库回答没有 view，原先三处无条件解引用会直接白屏。
- **team-lead 收尾**：修 F2 自证（上面）+ 加 `NeedsProof` + 清 2 个环境依赖的**墙钟断言**
  （connector 的 `< 500ms` 在并行跑 9 个 crate 时假红，实测踩到；改用熔断状态/错误变体做确定性断言）。
- **验收**：`cargo build --workspace` 0 error（余 3 个既有 warning）；`cargo test --workspace`
  **263 passed / 0 failed**，连跑 3 次稳定；架构门禁 exit 0；`npx vue-tsc --noEmit` exit 0。
- **仍未连库验的**（照纪律没做）：axum 路由冲突只在 `Router` 构建时 panic（编译期查不出）；
  `kb_eval.py` 全套（要 PG + embed 服务 + 一个对 A 空间无授权的真实账号）；
  `plainto_tsquery('simple', 中文)` 在本机 PG 无 CJK 分词，全文路可能恒空（此时只靠向量+trgm）；
  `exec-sql` 的 gold SQL 现在会被追加 `LIMIT 200`，切换当天要存 `eval_baseline.pre.csv` 逐题 diff。

## T4 connector 收口全部 DB IO（2026-07-27）
- **四个池 + 两条通道**：`ReadOnlyMySql`（池私有、构造即 `SET SESSION TRANSACTION READ ONLY`、`fetch` 里做敏感列整列置空并回 `redacted`）/ `PostgresSource`（**F3 启动期自检**：只读角色若能看见 `meta`/`kb`/`chat` 就 `Err` 拒绝启动，文案含 REVOKE 指引）/ `OwnedStore`（唯一可写通道：**不实现 `SqlSource`、没有 `execute(&str)`、没有任何 `From<ScopedSql>`** —— LLM 产物在类型上到不了）/ `SourceRegistry`（per-ds 懒建 + `probe`）。
- **`fixed(&'static str)` 字面量通道**（裁决 C1）：SQL 必须是编译期字面量，值全走 bind，动态 `IN` 只有 `expand(n)` 一条路。PG 侧 `$k` 编号必须与已 bind 参数接续——最容易错的一处，单测钉了「1 个固定参数 + 2 个 `{in}`」的编号序列；`expand(0)` 不渲染 `IN ()` 而是提前失败。
- **`ddl.rs`（上传建表安全面，全纯函数）**：`SafeIdent` 白名单 + 清洗 + **消重**（同名两列不能塌成一列）+ 类型推断。真实业务坑进单测：`0012` 必须判 Text（否则丢前导零，那是编码列）、整数部 >15 位判 Text（手机号/身份证，超出 f64 精度安全区）、千分位与货币符不算数字、恶意表头 `a; DROP TABLE x` 清洗后必过 `parse`。
- **knowledge 全面转 `fixed()`**：25 处 `sqlx::query` → **0**，门禁那条从 warn **转 FAIL 守**。必须拼的片段（ACL 子查询、三路召回排序）改成 `macro_rules!` + `concat!` **编译期**拼完——于是「把问句或文档内容拼进 SQL」在类型上不再可能。
- **server 接线**：`db.rs` 两个建池函数删除（建池能力从此只在 connector）；`execute`/`explain_check` 搬进 connector，签名从 `&MySqlPool` 换成 **`&dyn SqlSource`**（ds_id 断链的修法：挂第二个源后所有路径都打对库）；**11 处权限查询转 `fixed()`**，手写 `placeholders()` 消失；`/api/health` 修恒真判定（原第三项「PG 扩展列表非空」永远为真 → 显式校验 `vector`/`pg_trgm`/`age`）。
- **收尾三项专项核对**（我复核过结论）：① **红线真收口** —— 全仓 `PoolOptions|Pool::connect` 只命中 connector 内 6 行（3 处生产建池 + 3 处 `#[cfg(test)]`），connector 之外零个自建池，`&MySqlPool` 一个不剩。② **权限查询零漂移** —— 脚本抽 `git show HEAD` 与现文件全部含 SELECT 的字面量做集合比对，14/14 逐字相同（归一化只允许 `{ph}`→`{in}`）；另单独验 `department_employee_ids` 的**双 `{in}`**：`render_in` 替换每个标记、2n 个占位符与 2n 个 bind 对齐——这是「改错=越权」里最容易静默错的一处。③ **单测对账** 263 − 1（3 个测随实现搬去 connector）+ 25 = **287**，实测吻合。
- **team-lead 收尾**：`connector/lib.rs` 补路径再导出（文档写 `dms_connector::OwnedStore`，照文档写不能撞 E0433）；门禁摘掉 knowledge 的 `-WarnOnly`；`render_insert` 标名消费者（K4）而不是掩掉 warning。裁决记入 `_DECISIONS.md` 二·D：**`ReadOnlyMySql` 不给 `pool()`**（给了就能绕过 F5 脱敏与只读会话，且没有测试会红）。
- **验收**：`cargo build --workspace` 0 error（余 3 个既有 warning，connector 侧归零）；`cargo test --workspace --no-fail-fast` **287 passed / 0 failed**；门禁 exit 0，**knowledge 转 FAIL 守**，server 100 → 74 处（仍 warn，T10 收口）。
- **环境风险**：本机 Smart App Control 强制态，按内容哈希随机拦新链接的 test exe（`os error 4551`）。要可复现验收需管理员加 `target` 例外；详见 `_DECISIONS.md` 二·D 末段（含明确不接受的绕法）。
- **未连库验的新增 SQL**（K3 首次接真 PG 必须实测）：`postgres.rs` 两条探针的列数列序与 F3 自检返回值；`store::insert_chunks` 的 `unnest` 批量写；`retrieve.rs` 的 `ORDER BY (embedding <=> $2::vector) + 0` —— **必须 EXPLAIN 一次确认没走 HNSW**，这条错了是静默的召回退化。

## K3 多数据源 + K4 表格双通道 + K5 意图分诊（2026-07-28，功能面收口）
- **K3 数据源注册表**：`meta.datasource`（明文 DSN 绝不入库，只存 `dsn_ref` 键名；`check_dsn_ref` 把误粘的明文串当场 400 拒）+ 种一行 `ds_id='dms'` 让「单源」与「多源」走同一套代码路径 + `visible_datasources`（判据整块在 SQL 里，不做查完再过滤）+ 5 个管理端点（读按 ds 级可见性、写按 `administrator_flag`）。
- **K3 注册表 ds_id 化**（最高危一步，拆三步落）：13 张表加 `ds_id text NOT NULL DEFAULT 'dms'`（`NOT NULL DEFAULT` 让 PG 回填全部存量行）+ 主键前置 `ds_id`（旧键列全保留=超集，单测钉 `starts_with("ds_id")`）→ 21 条召回 SQL 从**单一常量** `DS_PRED` 拼谓词 → 选源上线。
  - **漂移守卫**：`every_meta_recall_is_ds_scoped` 用 `include_str!` 把三个文件的**源码**读进测试，逐行找 `FROM meta.`，断言窗口里必须出现 `ds_id` / `{ds_pred}` / `ds:any` 显式豁免标记之一。写这条守卫时它当场判红了 `review_lessons`（证明它是活的）。
  - **存量零行为变化**三条独立保证：可见源 ≤1 时选源直接短路（不 embed、不查 nearest、不问 LLM）；`DEFAULT 'dms'` + `IN ('dms','*')` 匹配每条回填行；`ON CONFLICT` 目标与 10 张表新主键逐条对齐（对不上会在首次 seed 报「no unique or exclusion constraint matching」）。
- **K4 表格双通道**：上传 xlsx/csv → ① markdown 进 `kb.chunk` ② 每 sheet 建 PG 物理表（`up_<doc_id>` schema）→ 注册数据源 → 可 NL2SQL。列名走 connector 的 `build_columns`（清洗 + 消重），中文表头进列注释；`INSERT … SELECT … FROM unnest($1::text[],…)` 每批 500 行、值全走 bind。通道②失败不让整次上传失败（记 error 继续，文本检索仍可用）。
- **K5 意图分诊**：规则优先 0-LLM（时间词/表名/单号 → Data；制度/流程/办法/文件名 → Knowledge；两侧都命中 → Data 并记日志攒样本）→ fast LLM 兜底 → 失败降级 Data。**hybrid 明确不做**：它要 `AskResult` 与 `Answer` 统一，那是 T9；不为它现在造一个 T9 要拆掉的转换层。
- **team-lead 修的功能级 blocker（收尾评审抓出，四路 agent 都没报全）**：
  1. **`ReadOnlyMySql` 与上传源的取数通道没接**：非主源选中后降级回主源 → 「上传即可问数」实际跑不通。已把 `&SourceRegistry` 传进 `ask`，按 `meta.datasource` 建 `DsSpec` 懒建池；`AskReq` 加 `ds` 显式选源，「无权访问数据源」映 **403**（原会被吞成 422「这问题问不出来」，用户永远不知道是权限问题）。
  2. **受限用户查自己上传的表会被 fail-closed 拒绝** —— 上传表不在 `meta.scope_binding` 里，而受限用户 `ScopeSets` 恒非空，走 `inject` 必拒。新增 `UnrestrictedProof::for_global_source(ds_authorized)` 与 `gate_on(sql, scope, ds_global)`：`policy_kind='global'` 的源整源不做行级过滤（可见性已由选源那层的 ds 级 ACL 判完），**只读红线与 LIMIT 护栏照走**。回归锁 `global_source_skips_injection_but_keeps_redline` 三条断言：不注入 / 写操作仍被拦 / 同一条 SQL 走 DMS 源必须被拒。
  3. **registry 映射键用了 ds_id 而非 `dsn_ref`** → 主源「测试连接」必然报「dsn_ref 未配置」（取数不受影响，因为 preload 的池按 ds_id 先命中）—— 这种「一半功能好一半坏」最难查。改走 `Settings::dsn_map()`。
  4. **`pg_ro_url` 配置键不存在** → 上传源建池必失败。补 `Settings.pg_ro_url` + `datasources` 映射；**`pg_url` 刻意不进映射表**（谁把数据源 `dsn_ref` 填成 `pg_url` 就该在「未配置」上失败，而不是连上一个能读全员文档的 owner 角色）。配置说明另立 `docs/CONFIG.md`（JSON 不能带注释）。
  5. 顺手：`source_kind` 两份实现合一（能过登记校验却建不出池的组合）；删掉 `ddl::render_insert`（K4 最终用 `render_insert_unnest` —— text bind 到 numeric 列没有赋值转换，单行 `VALUES` 形态会在第一个金额列直接报错，这是个真发现），不留第二个渲染器让人挑错。
- **验收**：`cargo build --workspace` 0 error（余 3 个既有 warning）；`cargo test --workspace --no-fail-fast` **321 passed / 0 failed**；架构门禁 exit 0（knowledge 仍是 FAIL 守）。
- **接第二个源之前必须收口的 5 条**（今天只有 'dms' 一格故行为不变）：`corrector.rs` 三条读注册表的校正 SQL（字段白名单/码表/指标口径）无 ds 谓词 —— 会拿 DMS 的口径去校正别的库的 SQL；`inject.rs` 的 `scope_binding` 加载与 upsert 无 ds 维度（随 T5 迁 policy 时一起做）。**且 `corrector.rs`/`inject.rs` 不在漂移守卫的文件清单里**，今天这 5 条没有任何自动化防线 —— 修的同时必须把 `corrector.rs` 加进那个数组。
- **仓库外同口径污染面**：`tools/embed_service.py:310` 的 `UPDATE meta.table_doc SET embedding` 与 `tools/cleanup_autodiscover.py` 的三条 SELECT/DELETE 都无 ds 限定（接第二个源后会跨源乱改/乱删），python 侧不受 Rust 漂移守卫保护。
- **两件本轮没上线、别当它在跑**：① `meta.datasource.embedding` 无写入点 → `nearest_datasources` 恒返空 → 向量选源的两条分支今天走不到，多源必须按「显式选源/主源」验收；② `kb.acl(scope='ds')` 的授予/回收没有端点（只有「上传时自动授上传者」与「注销时连带清理」），把上传表分享给同事要等 K6。

## K6 对外与运营面 + 补三个「宣称了但没生效」的洞（2026-07-28，功能面完）
- **对外 MCP**（`POST /api/mcp`，手写 JSON-RPC 2.0，零新依赖）：`initialize` / `tools/list` / `tools/call`，
  两个工具 `ask` 与 `kb_search`。鉴权 `X-API-Key` → `mcp_keys` 映射到 login_name，
  **然后走和 HTTP 完全同一条链**（`load_principal` → `compute_scope` → `select_source` → `gate_on` → `fetch`）——
  没有「MCP 就是超管」的旁路。未配置 `mcp_keys` 时端点恒 404（对外面默认关比默认开重要）。
  key 脱敏单测钉住「任何长度都不含完整 key」；全仓无 TraceLayer，不存在中间件把头写进日志的面。
- **运营面**：`meta.query_log`（一次问答一行，`tokio::spawn` 旁路写、失败只 warn —— 主链路零个多出来的 `.await`）
  + token 用量（`chat_with_usage` 让现有 8 个调用点一行不改，只接最贵的 precise 那次）
  + `GET /api/stats`（p50/p95 用 `percentile_cont … WITHIN GROUP` 在 SQL 里算，窗口 clamp 1..90，admin_only）。
- **管理面**：术语 CRUD（带 `ds_id` 作用域白名单）、SQL 示例的列表与人工复核状态、
  **`POST/DELETE /api/ds/{id}/grant`**（收尾评审点名的功能缺口：此前把上传表分享给同事没有任何办法）。
  刻意**不提供「新增 SQL 示例」**：示例只能来自真实问答 + 复核 —— 手工塞的一旦进 few-shot 就会自我传播错口径。
- **补三个洞**：① corrector 3 条 + inject 1 条 ds 谓词收口，漂移守卫清单 3 → **5 个文件**（覆盖 32 → 47 处）；
  ② python 两个脚本加 `--ds` 参数（此前会跨源乱改 embedding、乱删 autodiscover 资产）；
  ③ **向量选源真上线** —— `embed_service.py build` 加 datasource 分支给 `meta.datasource.embedding` 写值，
  在此之前 `nearest_datasources` 恒返空、那条路径是死代码。
- **team-lead 收尾修的三处**：
  1. **知识库角色口径不一致**：`/api/mcp` 用**解出来的** `p.role_code`，而 `/api/ask` 用请求里带的
     `Option<String>` —— 单角色账号不传 role_code 时 `roles` 是**空的**，于是同一个人走 MCP 能检索到
     「授权给他角色」的文档、走 `/api/ask` 反而检索不到。向解出来的那侧统一（它就是该账号真实的激活角色）。
  2. **最后 1 条 ds 缺口**：`correct_caliber` → `recall_metric_hits` 硬编码 DMS —— 会把「有效订单剔除
     0/108/199」补到别的源的 SQL 上。**漂移守卫抓不到它**（本函数不内联 SQL，没有 `FROM meta.` 字面量），
     只能靠签名带着 `ds` 走。顺带删掉那个兼容位，不留「不带 ds 也能召回指标」的入口。
  3. **预先掐掉一个假红陷阱**：`query_log` 加进守卫的 EXEMPT 数组 —— 它的 `ds:any` 标记离 SQL 有 11 行，
     谁把 query_log.rs 加进文件清单都会当场红。它和 `correction_log`/`failure_log` 同类，本就该豁免。
  另清掉全部 3 个存量 warning（`unused_parens` 真修；`dim_hit`/`agentid` 加 `#[allow]` **并写明谁是将来的消费者**）。
- **验收**：`cargo build --workspace` **0 error 0 warning**；架构门禁 exit 0；路由 24 条经机械核对无 axum panic 面
  （同层参数名一致、无 wildcard、无同路径重复注册）。
  ⚠️ **server 侧 198 个断言本轮一次都没跑到**：本机 Smart App Control 强制态按内容哈希拦 test exe（`os error 4551`）。
  试过重链接（改内容）、`--release`、换目录全新构建，三条路都被拦（release 与新目录连 build script 与 proc-macro DLL
  一起拦），**没有用改哈希/复制 exe 的绕法**。非 server 的 6 个 crate 148 passed / 0 failed；server 的 test exe
  编译通过（类型检查过）。放开 SAC 后预期 **346 passed**（148 + 198，静态 `#[test]` 点数核过：173 存量 + 25 新增）。
- **四项专项核对结论**（收尾 agent 做、我复核）：① MCP **不是**权限旁路（攻击面逐条走过：`ds` 入参走可见性守卫且
  不吃降级、`space_id` 只能收窄、全仓 `ScopedSql::unrestricted` 仍只 3 处且第 3 处无路由可达）
  ② ds 谓词 47 处 0 真缺口 ③ 观测不进关键路径 ④ 路由无冲突。
- **仍待办**：`settings.example.json`/`CONFIG.md` 已补 `mcp_keys`；PROGRESS 里「corrector/inject 不在守卫清单」
  与「向量选源没上线」两条**本条已更正**。存量观察一条留给下次提交前确认：相对 git HEAD，
  corrector.rs 33→29、direct.rs 34→33、pipeline.rs 14→7 共少 12 个断言，**早于本轮**（321 基线自洽），
  但 T4/K3/K5 几轮都未提交、无从二分定位，值得确认是「被合并/重构掉」还是「丢了」。

## T5 policy 落地 + T7 切片（呈现算法迁 semantic）（2026-07-28）
- **动机不只是分层**：本机 Smart App Control 那几轮把 `dms_ai_server` 的 test exe 拦死，
  **server 侧 198 个断言连续三轮跑不了**（K3/K4/K5/K6 加的约 60 个断言从未执行过）。
  而 policy / semantic 的 test binary 跑得动 —— 把断言搬进它们是当时恢复验证覆盖的唯一途径。
  这个判断后来被更彻底的办法取代（见下面的 Docker 通路），但迁移本身的收益留下了。
- **T5 `dms-policy` 全量落地**（8 src + 4 tests，全部 ≤450 行，最长函数 45 行）：
  principal / scope（`compute_scope` 97 行拆 1 编排 + 4 段，**段序与查询顺序一行未动**）/ dms_tables（7 张 DMS 表
  全走 `fixed(&'static str).expand(n)`）/ cache / rules（`OnceLock` → `RwLock<Arc<RuleSet>>` 热更新 +
  `install` 唯一写入口；注册表初值是 builtin 32 表而**不是空表** —— 空表对受限用户等于全表拒绝）/
  builtin（32 表）/ proof（`for_principal` 是 F2 的唯一业务铸造点）。
  **零新增依赖、零 Cargo.toml 改动**；门禁 `policy 不得 sqlx::query` 是 FAIL 守，全部 SQL 走 connector 通道。
- **F7 修完**（scope 缓存）：旧口径「当日过期 + `SystemTime / 86400`」→ 权限收紧后最长 24h 仍按旧权限出数，
  且那个除法用 **UTC**，翻页正好落在北京时间早上 8 点（上班第一波查询）。
  新口径 **TTL 15 分钟 + 四维 key（login/role/ds/scope_ver）**：版本号从本来就要查的 `t_role_data_scope`
  那批行顺手拼，DMS 侧改配置后**第一次查询即自愈**，零额外往返。
  两处 agent 自己想到的细节值得记：① `scope_ver` **先排序再哈希**（行序由 MySQL 决定，不排序会永不命中缓存，
  症状是限权用户每次都付那 ~10s 计算，还很难查）；② 锁中毒不传染（一次 panic 不该让此后所有权限查询永久 panic）。
- **T7 切片**：呈现算法 + 中文词表 + 34 省码迁 `semantic::present`（449 行）。T2 时只搬类型是对的
  （kernel 不许有业务语料），semantic 才是终点。`build` 92 行按 `RoleIdx` + 6 个分支构造器拆开，
  **判定顺序一行未重排**；省码收敛成全仓唯一一份 Rust 副本，前端 `format.ts` 那份由 drift 单测守。
  顺手删掉 `viewspec.rs` 那个函数体为空的 if（ARCHITECTURE §8 点名的死代码）。
- **闸门收紧（agent 自己想到、比我原设计更好）**：`gate_on(p, sql, scope, ds_global)` 现在**要求传 `Principal`** ——
  铸造放行凭证只能走 `dms_policy::proof::for_principal`，而它要一个身份。于是「谁能不带行级条件查生产库」
  在**类型上**就必须先有 `Principal`，闸门拿不到身份就只有注入这一条出路。
- **验证环境（重要）**：SAC 已从「只拦 server test exe」恶化到**拦所有新链接的未签名产物** ——
  依赖的 build script、proc-macro DLL、连一行 `fn main(){}` 编出来的 exe 都是 `os error 4551`，
  Windows 侧 cargo 对**任何** crate 都不可用。改用 **Docker（`rust:1-slim`，仓库只读挂载，产物落 volume）真跑**，
  并固化成 **`scripts/docker-test.ps1`**（`-Only build|test`、`-Sel '-p dms-policy'`）。
  这不是绕过校验，是换一个没有该策略的环境执行；明确禁止的做法仍是「复制 exe 追加字节改哈希」。
  写脚本时自己踩了两个坑并修掉：`$Args` 撞 PowerShell 自动变量（参数写了不生效）、`bc` 不在 `rust:1-slim` 里（汇总恒空）。
- **验收（Docker 内真跑，19 个 target 全部执行）**：`cargo build --locked --workspace` **0 error 0 warning**；
  `cargo test --locked --workspace --no-fail-fast` = **357 passed / 0 failed**；架构门禁 exit 0。
  逐条对账：server 140 = 198 − 48（46 权限 + 2 条 fail-closed 锁）− 10（present）；policy 58 = 28+15+3+5+7；
  semantic 11 = 10+1；总数 346 → 357，+11 恰好等于三个 agent 各自声明的新增。
  **断言迁移名字级零丢失**：与 `git show HEAD` 的测试函数名做集合比对，唯一「消失」的是 `epoch_day` ——
  它不是测试，正是 F7 那个 UTC bug 的载体，被 `Instant` 取代。
- **待办**：`judge_scope.py` 6/6 与 `scope <login>` 的 stdout 字节 diff 需连生产 MySQL 才能验
  （`compute_scope` 拆分后的等价性，段序已逐条对过，理论字节全等）。

## T7a — meta.rs 解体（2106 行 → dms-semantic 三组）
- `server/src/meta.rs` **已删**。落点：`ddl.rs`(255) `seed.rs`(217) `seed_defs.rs`(280)、
  `registry/{mod,model,lexicon,exemplar,element,datasource}.rs`（62-210 行）、
  `recall/{mod,metric,cards,pitfall,schema}.rs`（39-203 行）、
  `ingest/{mod,schema_sync}` + `ingest/autodiscover/{mod,probe,match_dict,register}.rs`（67-202 行）。
  **全部 ≤450 行硬线**；`migrate` 208→8 行、`sync_elements` 91→9 行、`upsert_element` 8 参收成 `struct Element`。
- server 的 `sqlx::query` **88 → 25 处**（剩 chat 10 / graph 8 / query_log 4 / corrector 3 / main 1，T10 收口）。
  `pipeline.rs` 那 8 处直写 `meta.sql_exemplar` 的 SQL 收进 `registry::exemplar`——
  agent 侧不许自己写 `meta.*` 的 SQL，否则 ds/visibility 两道总闸的漂移守卫扫不到它们。
- **验收（Docker，本人实跑）**：`cargo build --locked --workspace` 0 error 0 warning；
  `== 合计 365 passed / 0 failed（20 个 target 执行）==`；`check-arch.ps1` **exit 0**（唯一 warn = server 25 处，迁移中）。
- 断言账：357 → 365。server 140 → 124（−16），semantic 11 → 34（+23，含 drift.rs 两条），policy +1（F4 守卫），
  probe 反引号守卫 +1。**名字级零丢失**：HEAD 的 155 个 server 测试函数名，全仓逐名比对**一个都没消失**。
- 本轮修的四处（详见 `_DECISIONS.md` 二·F）：门禁 semantic 规则错位（F1）、两条守卫实测会红（F2）、
  `probe_sql` 反引号注入面（F3）、CLI 放行凭证的硬编码自证（F4）。
- **待办**：`meta sync` / `meta autodiscover` 两个子命令的 stdout JSON 需连库对拍（搬迁后形状应字节全等）。

## 连库验收（2026-07-28，生产 MySQL + 本机 PG；具体端点已脱敏）
后端一律容器内跑（`scripts/serve.ps1`，settings **运行时挂载不进镜像层**）。
判官/评测/对拍三个工具原本硬编码 `target/debug/dms-ai-server.exe`（Windows 侧 SAC 起不来 = 全瘫），
新增 `tools/cli.py` 收口，置 `DMSAI_CLI='docker exec -i dms-ai-server /app/dms-ai-server'` 即走容器。

- **MySQL 授权面**：`dms_ai` 只有 `SELECT, LOCK TABLES, SHOW VIEW`（xh_dms/xh_master/xh_scm）+ `PROCESS`。
  **写在授权层就不可能**，会话级 `READ ONLY` 只是第二道。329 张表可见。
- **`meta sync` 通过**：`rekey_ds_pk` 在 10 张表上 DROP+ADD 主键全部成功（幂等跳过判定生效），
  schema 同步 251 表 / 5606 列，stdout JSON 形状不变。**跑前已 `pg_dump` 备份（8.2 MB / 22 表）**。
- **`judge_scope.py` 6/6 全绿**：`compute_scope` 拆进 dms-policy 后，与 Java `DefaultEmployee` 语义的
  独立 Python 复现在生产数据上**集合全等**，行数逐一对齐（超管/财务 226237、XXJL 14976、城市经理 5997…）。
- **HNSW 索引匹配已 EXPLAIN 坐实**（积压最久的静默退化风险）：
  `ORDER BY embedding <=> $2` → `Index Scan using idx_kb_chunk_vec`；
  `ORDER BY (embedding <=> $2) + 0` → `Sort` + `Seq Scan`，**未用索引**。
  即 `Scan::Exact` 拿到的是可见集合内的真最近邻，`retrieve.rs` 那条 `ponytail:` 假设成立。

## 声明式口径 + Rubric 回炉闭环（2026-07-28）
把 SuperSonic 语义层的口径声明**对 LLM 路径也强制**（此前只有确定性 compose 路径吃），
配 deepagents 的 Rubric 回炉。落点：
- `kernel/src/sql/caliber.rs`：`CaliberRule` **六**变体（`RequireCols` / `RequireDedup` / `RequireLatest` /
  `RequirePercentScale` / `RequireJoinAndFilter` / `RequireCodeOnColumn`）+ `check_caliber` 纯 AST 判据 + `keeps_output_shape`
- `semantic/src/registry/caliber.rs`：`build_rules` 从注册表造规则（唯一构造点）
- `agent/src/guard.rs`：`judge()` → `Pass`/`Retry`/`Unresolved`。**不静默改写 SQL**；
  预算用尽则「结果照返 + 标注不可信 + 落 correction_log」（`caliber-retry` / `caliber-unresolved` / `caliber-grader-error`，kind 六→九）
- `meta.table_scope` 补明细表口径、`meta.table_snapshot`（快照取最新一条）、`meta.value_domain`、
  `meta.metric.unit`；`corrector::add_scope_filter` 放宽 JOIN 门

**实测（我本人跑，非采信 agent 报告）**：树 **413 passed / 0 failed（20 target）**、门禁 exit 0；
回归 52/1 → **53/1**；执行级评测 **32/38 → 31/38 = 净 −1**。
逐条归因与由此发现的四个真 bug（投影子查询恒判违规 / 指标口径补全对销量类一直是死的 /
表级声明只有校验器读 / 「只补口径」不可检查）见 `_DECISIONS.md` **二·G**。
一句方法论：**先把能确定性补的用 AST 补掉，再把剩下的交回炉，回炉必须受输出列形状约束**；
反过来做就是这轮实测到的 −1。

## 精确值域词典 + 占比派生指标（2026-07-28 续）
- `kernel::CaliberRule::RequireJoinAndFilter`（表缺席也算违规 —— `RequireCols` 在表缺席时不判，
  恰好在需要它时不出声）；`kernel::keeps_output_shape`（回炉只采纳输出列未变的改写）
- `meta.metric.unit` + `refund_ratio`「退款占比」派生指标（`agg_expr` 刻意含子查询 →
  `compose_sql_with` 见 `SELECT` 即 `return None` → 永不进确定性装配、只走 LLM）
- 名称型值域词典：**取值仓复用 `meta.value_map`**（`name=code=取值`），`meta.value_domain` 只登记
  「哪些列是名称型」；autodiscover 新增名称型探针（上限 2000、不做码型三闸）
- 漂移守卫补盲区：窗口内有 `JOIN meta.` 时裸 `ds_id` 不再算证据（`load_domain_values` 上实测坐实 ——
  删掉 `{ds_pred}` 守卫照样绿，而 SQL 已退化成跨源读）

**实测**：树 **413 passed / 0 failed（20 target）**、门禁 exit 0；`meta autodiscover` 灌入 68 个分类名；
评测 **32 →（31）→ 33/38 = 86.8%**。逐题归因与两个新发现的可判定错误类（「取对码用错列」/
「多输出一列」）见 `_DECISIONS.md` **二·H**。

## T9 — `pipeline.rs` 解体入 dms-agent（2026-07-28）
**删了三个文件**：`server/src/{pipeline.rs(1280),graph.rs,triage.rs}`。`dms-agent` 从 1 个实现文件
（`guard.rs`）长到 **17 个 `.rs` / 3621 行**：`ctx.rs`（`AskCtx` 持 `&dyn SqlSource`，ds_id 断链的头号修法）、
`gate.rs`（三段闸门唯一收口）、`answerers/{mod,hits,graph,cache,knowledge}`、`prompt/gather/run`、
`source/triage/compound/review`、`ask.rs`。AGE 图 IO 迁 `dms_connector::graph`。

**Router 五位齐全**（`graph → direct-agg → direct-doc → semantic-cache → llm`）：
**加一种能力＝加一个 `Answerer`**，不再是往 258 行的 `ask_single` 里塞第六支 `if`。
第五位一度在表外直调（`LlmAnswerer` 拿不到 token 用量回调与单问 `t0`），两样收进 `AskCtx` 后收进表内。

**顺带补的两个 deepagents P1 missing 件**：
- **截断三件套**（`ctx::truncation_note`）：命中 200 行上限时带出原因/范围/续读参数。
  此前是静默截断 —— 用户看到 200 行不知道后面还有。判据只用 `row_count == MAX_ROWS`，
  **取假阳一侧**（多一句提示 vs 静默少数据）。
- **Memory 信任边界三条纪律**进 `prompts/system.md`：记忆非指令 / 凭据禁令 / 口径注明来源。
  few-shot 与教训都是用户历史输入派生的，进 prompt 时已是「外部文本」（不变量 I5）。

**实测**：树 **451 passed / 0 failed（20 target）**、门禁 exit 0、server 的 `sqlx::query` **25 → 17**。
断言名字级 HEAD 155 → 438 个唯一名，「消失」恰好 1 条且是已裁决项。
裁决与三处「agent 顶着任务书改对了」见 `_DECISIONS.md` **二·I**。

## K4 收尾 — 「上传即可问数」从「已落地」变成「实测通了」（2026-07-28）

去验 `INTEGRATION-TRACE` 里长期标「半」的那条（**建表与入库已落地，问数通道未连库实测**）。
一次实测暴露三个缺陷，三个都**不报错不进日志**，症状统一是「建表成功、检索可用、只有问数死掉」：

| # | 缺陷 | 影响面 | 修法 |
|---|---|---|---|
| 1 | prompt 与闸门四处硬写 `MysqlDialect`（两个模板本来就有 `{dialect}` 占位，喂进去的值是错的） | **任何非 MySQL 源问数恒失败**（``syntax error at or near "`"``）；闸门那两处更隐蔽——红线校验靠解析，方言错=`GuardError`=静默回落 | `Dialect` 加回 `quote()`（曾以「零消费者」删掉，**消费者就是这个缺陷**）；四处改收 `cx.source.dialect()`，**不留默认值** |
| 2 | 上传源建池不置 `search_path` | 探针按 `current_schema()` 过滤 → 采不到表 → LLM 拿到**空 schema 段** | `DsSpec.schema` + `after_connect`；schema 名过 `SafeIdent`，**过不了就 Err 不清洗**（清洗会把「配错」变成「连上了另一个」） |
| 3 | 备份表启发式（结尾 ≥4 位数字）误伤 `t0_<uuid>` 表名 | ≈(10/16)⁴ ≈ **15%**，约每 6 份上传 1 份静默不可问数；这次的 uuid 侥幸没中 | `sync_schema(filter_backup)`：别人建的库 true，**自己建名的库 false** |

**实测**：`tools/up_probe.py` exit 0 ——
`SELECT SUM(c2) AS "总销量" FROM t0_… WHERE c0 LIKE '%销售部%'` → 600（340+260）。
双引号别名（方言跟着源走）+ 裸表名（search_path 生效）+ 用对 `c0/c2`（中文表头经**列注释**进了 prompt）。
`column_doc` 4 行注释正确、类型 text/text/numeric/timestamptz 正确。
**红态不是模拟的**：改动前同一个脚本跑出来就是反引号语法错。
顺带 PG 两条探针 + `attnum` int2 解码首次连库验过，两处「⚠️ 未连库验证」注释已改。

**另修一处判据盲区**：`RequireDedup` 结构上看不见 LLM 最常写的
`WITH dedup AS (SELECT DISTINCT …) … SUM(dedup.qty)` —— CTE/派生表把别名洗掉，
前置条件不成立、整条规则一次不触发（`correction_log` 全空）。
`Facts` 现在把 CTE/派生表别名**转发**到内部表；枪测过（关掉转发即红）。

**树 476 passed / 0 failed（20 target）**、门禁 15 条 exit 0。裁决见 `_DECISIONS.md` **二·K / 二·L**。

第四轮评测 **34/38**（与上轮同分，失败集换两个）：GOODS13/STK02 转绿，GOODS17/AS03 转红。
**GOODS17、MKT04 判为噪声**（今天单跑与 gold 逐值一致）；**SALE15 是系统性的**，
根因不是去重键而是 `meta.dimension` 缺「商品」这一行，算术已闭合
（同一 goods_code 两个 sku_name：76,967 + 14,983 = 91,950 = 模型给的数）。
方向对 gold 不利 → 属业务口径裁决，**未改任何一侧**。

## SC 自一致采样：落地、量完、判为不开（2026-07-28）

按上一轮量到的误差源挑的（不是照抄清单）：两轮评测都停 34/38 而失败集换两个。
投票投在**结果指纹**上（只看值不看列名——中文别名每轮措辞会变，算进列名就等于让 SC 永不收敛）；
默认 `sc_samples=1` ＝ 关且零开销；**判官传同一个配置值而不是写死 1**，否则「开了有没有变好」永远量不出来。

| | 通过 | 失败集 | p50 | p95 |
|---|---|---|---|---|
| sc=1 | 34/38 | AS03 / GOODS17 / MKT04 / SALE15 | 19.4s | 62.4s |
| sc=3 | 34/38 | AS03 / **E05** / **GOODS13** | 26.6s | **153.0s** |

**净 0，代价 2.5 倍 p95 → 默认保持关。** 机制上收益与损失**对称**：SC 向众数收敛，
众数对时把「偶尔错」变成「稳定对」，**众数错时把「偶尔对」变成「稳定错」**
（GOODS13 这次 +25.9%，正是它原始缺陷的签名）。
诊断价值已兑现：3 次采样给同一个错值，证明 AS03/SALE15 是系统性的而非噪声。

## 口径层把一条**本来正确**的 SQL 改错了 —— AS03 的真根因

`correction_log` 存着现场：回炉判词命令模型「把 WHERE 里 `after_sales_time` 整段改成 `order_time`」。
**模型第一版是对的**，是判据让它改错的。成因：问句里的「单」把无关指标「订单数」也召回了，
于是 `RequireTimeColumn{after_sales_time}`（售后单数）与 `RequireTimeColumn{order_time}`（订单数）
同一轮生效 —— 而这条变体**刻意不带表名**（为了跨表也能判），正是这个刻意让两条直接对撞。

修法 `kernel::drop_conflicting_time_cols`：**冲突即全部哑掉**，其余规则照判。
不「挑一个」——挑需要「哪个指标更贴问句」的分数，构造侧今天没有，挑错就是重演。
实测 **AS03 ✅**，route 从 `llm+repair` 变回纯 `llm`。枪测过。

**这条改变了对口径层的风险认识**：此前只想到「漏判」（判据看不见 → 答案照旧错）；
现在知道还有更贵的第二种 —— **判据看见了，但看见的是矛盾的东西，于是把对的改错**。
凡「判据可命令模型改写 SQL」的机制，都必须先回答「两条判据互斥时怎么办」。

树 **484 passed / 0 failed**、门禁 exit 0、回归 54/0/0。裁决见 `_DECISIONS.md` **二·M / 二·M′ / 二·N / 二·O**。

**执行级评测 34/38 → 35/38 = 92.1%**（p50 18.7s、p95 72.0s）。分层：`聚合 22/22`、`去重计数 9/9`、
`口径 14/16`（原 12/16）。三轮失败集对比比总分有用得多：

| 轮次 | 失败集 |
|---|---|
| sc=1（改动前） | **AS03** · GOODS17 · MKT04 · SALE15 |
| sc=3 | **AS03** · E05 · GOODS13 · SALE15 |
| 冲突守卫后 | FIN04 · GOODS13 · SALE15 |

**AS03 前两轮必红、这一轮消失** —— 唯一每轮都红的那道被永久修掉。SALE15 三轮都在（卡业务裁决）。
其余五道在轮次间轮换 = 飘。**剩余空间由「飘」主导**，而治飘最直觉的工具（SC）已量出对称、净 0。
正路是把这些题做成确定性（走 direct-agg），见 二·O5a 的三道拦。

## 企业知识库：解析器补上两类，并修掉一处「假装可用」

- **PDF / Excel 已可用**：`pypdf`(BSD-3) + `openpyxl`(MIT)，**零 AGPL 依赖**。
  PDF 原来只有 `pymupdf4llm`/`PyMuPDF` 两个候选、都是 AGPL-3.0 —— 那是要法务点头的事，
  不该由部署脚本替业主装，故补了 BSD 的第三级兜底（丢标题层级，想要再自行装前两级）。
- **Word / PPT 本机不可用**：不是许可也不是代码，是 `lxml` 的编译扩展被 SAC 拦
  （与裁决 二·E 同一个拦截器），Linux 部署正常。
- **`parse_ok` 原来会假装可用**：用 `find_spec` 判断，**只查包在不在、不查能不能 import**。
  装完 python-docx 它就开始谎报 `docx: true` 而真解析在 DLL 上炸 ——
  正好把它自己文档里那句「不许假装可用」破了。改成真 import 一次（带缓存）。
- **空 sheet 曾无声消失**：Python 侧 `_sheet` 对全空 sheet `return None`，
  于是 Rust 的 `TabularSource.skipped` 永远看不到它 —— 而那条契约写的是「不建零列表但**不能静默**」。
  两处一起改才成立：Python 报上来（空表头+空行），Rust 的**文本通道**跳过它
  （否则多一个只有标题的垃圾块进 `kb.chunk`）。`tools/parse_probe.py` 钉住三条判据。

## 三轮实测：34 → 35 → 36/38，以及把「下一步」变成一组数（2026-07-28/29）

| 轮次 | 通过 | 失败集 | p50 | p95 |
|---|---|---|---|---|
| 基线 | 34/38 | AS03 · GOODS17 · MKT04 · SALE15 | 19.4s | 62.4s |
| 冲突守卫（二·N） | 35/38 | FIN04 · GOODS13 · SALE15 | 18.7s | 72.0s |
| 语料清理 + 显式年份（二·P/Q） | **36/38 = 94.7%** | GOODS17 · **SALE15** | **17.1s** | **57.9s** |

`时间 19/19`、`码值筛选 10/10`、`聚合 22/22`、`去重计数 9/9`、`趋势 3/3`。
**SALE15 是三轮唯一每次都在的** —— 卡业务裁决，不是技术问题。
`llm+repair` 从 5 降到 2：触发口径判红的 SQL 变少，与「清掉 6 条违反声明的语料」方向一致。

### 第一杠杆是「确定性覆盖」，不是再调 prompt

route 分布实测：`llm 24 / direct-agg 8 / llm+repair 5 / semantic-cache 1` ——
**76% 过 LLM，而全部失败都在 LLM 路径**（确定性路径至今 0 失败，回归 54/54 也稳）。
新增 `why-not-compose` 子命令（与 `try_compose` 共用同一批判据，不抄第二份）逐题诊断，
最大一档是 **② 维度不命中 17 题**：`try_compose` 强制要维度，
而无维度这条路只有一个硬编码模板、且只认 4 个指标。

两处修完之后，可装配 4 → **7**、② 17 → **14**：

1. **指标 only 通用装配**（`try_compose_metric_only`）——**不写第二个装配器**：
   造一个 `expr` 为空的伪维度喂给 `compose_sql_with` 的无维度模式，
   去重下推/表级口径/时间桥接/扇出/残留守卫全部复用同一份。
   两道自设门：**给 `agg_template` 让路**（否则「本月销售额」的数会从订单头
   `SUM(total_amount)` 换成明细声明那一套 —— 正是未裁决的 `item_type`；且会丢 KPI 环比）、
   **命中维度即退出**（否则静默丢分组）。
2. **时间窗按声明的 `time_col` 放**（此前写死 `t_sales_order`/`order_time`，
   桥不到就整条不装配 → 售后单数/开票金额/动销商品数一律回落 LLM，而声明里明明写着）。
   `MetricDef` 补 `time_col` 字段 —— 装配侧此前根本没读它。

**实测三道转正**：`E02-本月订单数`、`E09-售后单数`、`PERM01-城市经理今年售后单数`
全部 **direct-agg 且与 gold 逐值一致**，延迟从 ~12-20s 降到 ~9s。
PERM01 是权限题 —— 行级注入在确定性路径上照旧生效。
确定性覆盖 8 → **11/38（29%）**，同时从 LLM 的不确定性里搬走 3 道。

## 确定性覆盖 21% → 26%（可装配 4 → 10），两道最可复现的失败转正

按 二·U3 定的新判据（**路由 + 逐值一致**，不看单轮总分 —— 飘动率 24% 下单轮 ±2 无信息量）：

| 题 | 之前 | 之后 |
|---|---|---|
| GOODS13-上半年月度销量趋势 | 偏错的硬币（4 次 2 绿 2 红，两次错值都是 2138540.58） | ✅ **direct-agg，6 行一致** |
| GOODS17-六月分类销量Top5 | 稳定 +30.5% | ✅ **direct-agg，5 行一致** |
| E02 / E09 / PERM01 | llm，~12-20s | ✅ **direct-agg，逐值一致，~9s** |

三处改动叠起来解开的：
1. **指标 only 通用装配** + **时间窗按声明的 `time_col` 放**（`MetricDef` 此前根本没取那个字段）
2. **维度别名补「每个月」**（`match_word` 是子串匹配，「每**个**月」不含「每月」）
3. **虚词表补 `上半年/下半年`、`箱`、排序疑问词 8 个、量词 4 个** ——
   每次只加实测挡住过的那一个，不预先铺表（`元` 会吃「元气森林」、`件` 会吃「件套」）
4. **`detect_top_n` 补「最高的 N 个」** —— 这一条必须**先于**解锁量词，
   否则解锁的题按默认 200 行出数、行数不符：**把「飘着的失败」换成「确定的失败」**

**方法论订正（比分数重要）**：已观测到翻面过的题 **9/38 ≈ 24%**，全在 LLM 路径；
确定性路径（direct-agg / graph / compound / semantic-cache）**至今 0 失败**。
故此后判据三条：①确定性覆盖（单调、与飘无关）②单题路由+逐值 ③比总分要同镜像多轮取交集。
五轮交集只剩 **SALE15**（业务裁决）。

**枪测抓到我自己写的一条恒真断言**（二·T）：我给残留守卫加过「消化显式年份」并在
二·O5a 断言过「STRIP_WORDS 认不出阿拉伯年份 → 残留」——
`has_residue_with` 本来就过滤掉所有 ASCII 数字，**那句是错的、那段是死代码**，已删并订正。

### 本轮终值：35/38，而结构指标全面改善

| | 本轮之前 | 现在 |
|---|---|---|
| 通过 | 34/38 | 35/38 = 92.1% |
| **direct-agg** | 8/38（21%） | **15/38（39%）** |
| **`llm+repair`** | 5 | **0** |
| **p95** | 72.0s | **28.1s** |
| p50 | 19.4s | 16.4s |

`分组 10/10`、`去重计数 9/9`、`趋势 3/3`。**GOODS13 与 GOODS17 在全量里都绿**。
失败三道：FIN04（偏错的硬币，见下）、MKT04（这次返回空聚合）、SALE15（业务裁决）。

### 下一轮最大的单项收益：让装配器读懂快照声明

`compose_gated` 的快照门把余额/库存类问句**永久留在 LLM 路径**（理由正当：
平铺 GROUP BY 不懂「取每分区最新一条」）。而 LLM 把 `rn = 1` 写对的概率实测约 1/3。
**但 `meta.table_snapshot` 已经声明了分区键与取最新的排序列** ——
装配器把基表换成 `(… ROW_NUMBER() OVER (PARTITION BY 分区键 ORDER BY 排序列 DESC) = 1) b0`
即可，与既有的去重子查询是**同一个形状**（`DISTINCT 键` → `rn = 1`），
表级口径下推/时间桥接/残留守卫全部复用。详见 `_DECISIONS.md` **二·X**。

### 企业知识库侧：kb_eval 7/7 → 8/8，并修掉两处「半可用」

- **补回欠的验证**：我改过知识库侧四处却没重跑 kb_eval → 补跑 7/7，未破坏。
- **补 xlsx 端到端判据**：`openpyxl` 本轮才启用，而语料里表格只有 CSV（不经 openpyxl）。
  新增 `差旅补贴标准_表格.xlsx` + `KB08` → **8/8**。夹具数字（1250）别处不出现，
  否则题就变成「检索到任意一份」；第二个 sheet 留空，于是 **「embedded 1 块」本身是判据**
  （空 sheet 不产垃圾块，端到端）。
- 🔴 **加了写路径没加删路径**：`sync_upload_schema` 写注册表，删文档那侧没清 →
  实测 4 组孤儿行（16+5 行已清）。修：`schema_sync::drop_schema_docs`。
- 🔴 **修复之前上传的文件「数据源在、schema 空、永不自愈」**（sha256 去重让重传不走通道②）——
  真实部署里就是「升级后老数据半可用」且不报错。修：`resync-uploads` 子命令（幂等），实测补采 2/2。
- 上传 xlsx 源**可问数**实测通过（`→ 境外出差`，1250 最高）。
- `tools/parse_probe.py` 现在自清理夹具（不再往 `kb_fixtures/` 里留 `_probe.*`）。

### 装配器出 KPI 环比：让路门只剩一条理由

`compose_sql_with_snap` 加时间模板覆盖位，`prev_window` 给上期模板 —— **同一段装配、只换时间窗**。
只在无维度那支出 `prev`（带维度时首格是维度值，`patch_prev` 的 `cell_num` 返 None，
环比用不上，多查一次白花；`agg_template` 也只在无维度时出）。
判据钉的是**「只差时间窗」**而不是「有 prev」：若上期那次重装配顺手换了口径/去重/JOIN，
Δ% 就是拿两个口径不同的数相除 —— **那种错比没有环比更坏，因为它看着像个结论**。
实测「本月有多少个订单」→ `direct-agg` `20093` `delta{pct:-15.9,dir:down,label:较上月}`。

**让路门现在只剩一条理由**：销售额的 `item_type` 取 '1' 还是 '3'（业务裁决）。
它落地后即可撤门、删 `agg_template` —— 那时 T8 的这一半才算真做完。

### 值过滤进确定性装配：声明能解释的值装进 WHERE

`meta.value_map` 里本来就写着 `名字 → (表, 列, 码)`，装配器**不读它** —— 这是本轮反复遇到的
同一个模式（前几处是 `time_col` / `table_snapshot` / `dedup_keys`）。

**先量风险再动手**：936 行 / 82 列，其中 **109 个名字跨 ≥2 个 (表, 列)**。
所以第一条规则是**歧义即不认**（不认 = 那个词照旧是残留 = 回落 LLM，与上线前同形）。

四道 fail-closed 门，全部枪测过（拆掉即红）：
1. **消化了就必须装上**（G1）：装不上 `return None`。不加过滤却把词消化掉，
   就是 E16「线下客户 → 全部客户 TOP200」那类静默丢限定。
2. **口径已钉住该列就拒**（G2）：销量口径 `item_type='1'` 撞问句「赠品」`='2'` = 恒 0 行。
   实测立刻兑现：「退货类型…申请退款多少钱」两词都唯一命中 `after_sales_type`（1/2），当场拒对。
3. **值名被指标/维度词包含（含相等）就不认**：拿全部 92 道题面对 936 行全量对撞，
   扫出两个危险命中且都**无歧义**（歧义门救不了）——「各**业务**员」的 `业务`
   唯一命中 `contact_type=1`；「**市场费用**」既是指标名又是 `balance_type=3` 的码值名。
4. **只认 `match_kind='eq'`**：5 行 `like` 在 `t_sales_order.paid_way`（一单多种支付方式，
   多值串），写 `= '码'` 是确定性地取错集合。第一版忘读这一列，靠断言补上。

值过滤的表按 `join_edge` 桥进来，**且必须桥在「去重/快照包裹之前」** ——
这样去重键守卫、表级口径循环、`starts_with(head)` 三层既有防线自动覆盖新表
（实测自动带上了 `deleted_flag = 0`，与 gold 一致）。扇出边一律拒。
「湖南**省**」的「省」走**位置性同位语**（只在紧跟值名后吃 `省/市/区/县`），
**不进 `STRIP_WORDS`** —— 那张表无位置、全仓共用，全局剥「省」会吃掉实体名里的字。

实测（route + 值双验）：SALE17「本月湖南省的销售额」`llm→direct-agg` 908ms，
`province='430000'`，值与**同一时刻现跑的 gold 逐字节相同**（gold 备注里 46.5M 是月初快照，
「本月累计」每天在长 —— 这类比对必须现跑 gold）；E16 `llm→direct-agg` 395ms，
返回 200 行客户名**全部**以「线下-」开头。确定性覆盖 **15 → 16/38**。

### 注册表读失败曾是静默的（评测照出来的）

一趟评测里 E05「本月各商品分类销量」记 `llm+repair 97.9s` 并答错，而**同一镜像同一问句**
事后连跑 5 次稳定 `direct-agg` 且对数 —— 那一刻 `try_compose` 返了 `None`，
**没有一行日志说为什么**。顺着看到旁边更坏的一条：`load_table_scopes` 用 `unwrap_or_default()`，
读失败 = 装配器**不带表级口径**继续拼 → 确定性错数、route 仍是 `direct-agg`、连回炉都没有
（就是「销量虚高 41%」那个失败面，只不过触发条件是一次读超时而非声明写错）。

判据：**缺了会改数的声明（metric/dimension/join_edge/table_scope/table_snapshot）
读失败就整条不装配 + `warn!`**；`value_map` 是唯一可按缺省走的（空表 = 值名不被消化 =
残留守卫照旧拦 → 只少覆盖、不出错数）。三处同处置：`try_compose` / `metric_only`
（新增 `RegistryDown`，与「指标不命中」分开）/ `compose_verdict`（诊断新增 `⑥`）——
少改一处就会漂出「诊断说可装配、运行时回落」。**这条改的是可观测性，不是数。**

### 补「赠品箱数」指标：与销量只差一个码值

GOODS14「2026年6月我们送出去的赠品有多少箱」原本是 **① 指标不命中** —— 连残留守卫都轮不到，
整题交 LLM，实测 **75,840** vs gold **127,211**（差 40%）。而它的 gold 与「销量」声明**结构逐字相同**，
只差 `item_type` 的 '2' vs '1'。补声明后：`direct-agg`，模型 **127,211 == 现跑 gold**。

两处非显然的地方：
- **别名里必须有裸「赠品」**。它同时是 `value_map` 里 `item_type='2'` 的码值名（跨三张表、
  当前被歧义门跳过）。哪天有人去重了那几行，「赠品」会被当值过滤 → 与本指标自己的
  `item_type='2'` 撞 G2 → 整条拒；放进别名后**子串门**会先挡回来。这是新声明与值过滤四道门的**互锁**。
- **抢词是唯一真风险**：它与销量太近（`销量` 有别名「卖了多少箱」，它有「赠品箱」）。
  抢错了数会变小而 route 仍是 `direct-agg`，**没有回炉机会**。断言钉住销量那一族三种问法不许被抢。
  顺手补了前一轮的漂：测试抄本里漏了 `buyer_count`，它的「客户数」别名从没被碰撞断言核过。

`STRIP_WORDS` 只加了一个纯代词「我们」（必须先于单字「我」，否则剥完留「们」）。
实义词「送出去的赠品」走**指标别名**，不进虚词表 —— 业务词归注册表是 `lexicon.rs` 的收纳边界。

### 本轮收尾测量（值过滤 + 赠品箱数 之后）

- 单测 **508 绿**、架构门禁 15 项绿、`audit_trace` 55 行/41 引用/**0 失效**、回归 **55/56**。
- 评测 **34/38**，四条红（AS01/AS02/SALE15/STK01）**全在 `llm` 路** —— 确定性路**又一次 0 失败**。
  分层里 `去重 4/7→6/7`、`明细口径 0/1→1/1`（赠品箱数那条声明带来的）。
- **三趟失败集交集 = {SALE15, STK01}**。SALE15 是待裁决业务题；
  **STK01 值得单独查：gold 的首行仓库名是空串**（模型给出「佳木斯市鼎顺源食品有限公司仓库」被判红）
  —— 疑似 gold 自身问题，不是模型答错。判红的是**对拍**，不等于模型错。
- ⚠️ 这一趟的 `p50=14.9s / p95=42.1s` **不作基线**：跑的时候有并发工作流抢资源
  （AS01 42s vs 上一趟 21s）。延迟基线要在无并发时重测 —— 同「`Tee-Object` 到结束才落盘」
  与「跑测量时别 `serve.ps1 -Build`」一类的纪律：**测量环境被污染就别拿数字说话**。

### D0「修尺子」完成（方案 D 第一阶段）

四把尺子原来都能**跑绿而什么都没测**，逐条修掉并做了反向验证（打坏 → 证明红 → 恢复）：

1. `kb_eval.py`：夹具/探针失败不再 `return 0`。逐题继续跑，被夹具阻塞的题记**具名第三态**，
   退出码三分（0=真跑了且零阻塞 / 1=题红 / 2=门没开）。`chunk_keywords` 的静默跳过也改第三态
   —— KB03「引用块原文真含关键词」这条唯一校验，此前在端点 404 时**从来没跑过**而没人看得出来。
2. `regression.py`：未知断言键**硬失败**（写错键名 = 断言恒过）；新增 `sql_golden` 金 SQL 快照
   + `--bless/--bless-all`（无 `--yes` 一字节不写）；红线 DML 探测器抽成纯函数并加**正反对照**
   —— 它此前**结构上恒真**（H01-H03 必然不产 SQL → 判「守住」，探测器从没被验证过）。
3. `evaluation.py`：`--runs N` 出**失败集交集 + 抖动池 M**（判据是交集，不是单轮总分）；
   `quiet_alarm` 加 `graded≥10` 门（1 题跑 3 趟恒 M=0，恒真的报警等于没有报警）；
   退出码顺序修正（先判稳定失败再判自检报警）；补 `stdout.reconfigure` ——
   缺它时管道下打结论那刻 `UnicodeEncodeError`，**退出码 1 与「有稳定失败」撞车**。
4. `serve.ps1` 挂 `tools/`（可写，`--csv` 要落盘）+ `why-not-compose` 真 flag 解析。
   顺带堵了两条静默降级：**空位置参数被当问句**（`-Cmd` 尾空格即触发，全量 38 题静默变「问一句空话」）、
   `--cases` 指错文件 → 0 题诊断却退 0。

**对计划的一处修正**：原写「28 条 direct-agg 各加**数值**断言」，改成**金 SQL 快照**。
本轮亲测同一条 SQL 月初 46.5M、月末 50.6M（累计值每天在长），写死数值明天就假红；
而真危险是口径被改，钉 SQL 文本时间无关且正好卡住它。只给确定性路加（LLM 路 SQL 每次都变）。

**两个 runner 都缺反空转闸**（实测 `--filter 打错` → 「通过 0 / 失败 0」→ exit 0），已统一为 exit 2。

实测：单测 511 绿、门禁 15 项绿、`kb_eval` 8/8 且 KB03 ✅、三个 `--selfcheck` 全过、
`--csv` 连跑两次 39 行**逐字节全等**（尺子自身无抖动）。
门分布基线 `tools/why_gates.csv`：`✅13 / ⓿4 / ①7 / ②8 / ④1 / ⑤5` —— 确定性覆盖 **17/38**。

### D1 起步：知识库上传入口（此前为零）

`web/src/*.vue` 里 `upload`/`input type="file"`/`FormData` **全量零命中** ——
后端 `/api/kb/upload` 早通了、`kb_eval.py` 也在用，但**用户在界面上传不了任何文件**。
新建 `web/src/KbPanel.vue`（拖拽多选 + 顺序上传 + 逐文件结果 + 文档列表 + 删除），App.vue 三行接入。

三处刻意设计：① **前端不写支持格式白名单** —— 格式的真相源在 `knowledge::ingest`，
复述一份必漂；服务端拒了就原样回显它的错误消息。② 不做客户端大小校验（同理）。
③ 顺序上传不并发（后端 `UPLOAD_GATE` 最多 4 个）。

**一次被实测抓到的自伤**：我把成功态猜成 `ready`/`ok`，而上传实际返回 `embedded` ——
一次成功会被显示成 ❌。改成对着 `DocStatus` 真相源列全，并**显式区分 `chunked`**
（文本块入了、向量没入 = 向量检索找不到它，也就是「向量服务抖一次就永久检索不到」那个状态）。
契约三条已连库实测：上传 200 + `status=embedded`、列表、删除后消失；`vue-tsc` 与 `vite build` 干净。

### D1 检索质量 + D2「带 AI 分析」

**检索阈值改成连真库量出来的标定值**：trgm `0.3→0.2`（上界由判据块 0.2105 钉、下界由噪声块
0.1818 钉）、向量路新增距离下限 `0.55`（判据块 0.1863~0.4926，远域 nohit 最近块 0.6020，取中点）。
trgm 出结果的题数 **3/14 → 9/14**；KB07 远域 nohit 从召回 6 块变 **0 块**（连 LLM 都不调）。
两个常量各有实测表当断言，不重量就改会当场红。

量出两条新事实：① **中文 FTS 那一路实测 322 格全 0**（`plainto_tsquery('simple')` 不切中文）——
「三路混合」实际是两路；② **距离下限结构上挡不住近域 nohit**（KB13 最近块 0.3395，
比 10 个判据块都近），「库里有没有」最后仍由 `keep_cited_only` 兜。

**多文档冲突（KB10）**：两份夹具写了不同的年度上限，实测 7 次采样中 **1 次静默只挑一份**
（两份文档每次都在 citations 里，所以不是检索问题，是回答层）。根因是 SYSTEM 段对「矛盾」
一个字都没有。补一条规则后 **连采 20 次，20/20 两版都报**。

**AI 解读（`POST /api/analysis`，按需）**：`caliber` 恒有且零 LLM（逐项从**已执行的那条 SQL**
读出来源表/过滤含行级权限/时间窗/去重），`insight` 是 fast 模型那段话、可为 null。
`insight` 为 null 只显示 caliber、**不标成失败** —— 解读失败不许让一次成功的取数看起来失败。
实测出的解读会引口径：「统计了当月…未删除且订单状态有效（排除 0、108、199）…未做去重」。

**`kb_eval` 8 题 → 16 题**（要点完整性 / 多文档冲突 / 跨块 / 表格条件查 / 近域 nohit /
txt 解析链 / 第二道 ACL），实测 **16 题真跑、通过 16、夹具阻塞 0、exit 0**。

三条「两侧各自全绿、合起来不通」的协作缺陷已修（详见 `_DECISIONS` 二·AJ3）：
AI 解读前后端契约对不上（**我的协调错**：给前端说了形状又让后端自己定）、
`redacted` 前端渲染了但后端字段不存在、`kb_eval.py` 被我的三态改动改崩（三条早退仍返二元组）。
两条恒真判据已换成真判据（`insight_api` 那条测的是 `serde_json`；前端那个「字面量类型闸」
实测 `: number = 100` 也 exit 0）。

### D3 的两把便宜刀：一把早已落地、一把补上

- **「召回元素过少就给全表字段」（SuperSonic `PARSER_FIELDS_COUNT_THRESHOLD`）本仓早就满足** ——
  `render_schema` 的 SQL 是 `SELECT column_name … WHERE table_name = $1 ORDER BY ordinal`，
  **整表全字段**，从来不是「只给召回到的列」。差距矩阵把它记成「缺失」是错的，不做。
  （教训：**登记成待做的也要核**，照着做就是重做一遍已落地的。）
- **「按 join 边补对面表卡片」（SQLBot 关系补全）是真缺口，补上了**：
  `join_lines` 只留「至少一端被召回」的边，于是 prompt 有一行 `t_ord.cust = t_cust.cust`
  而 **t_cust 的字段一个都没给**（向量召回按单表打分，看不见「这张表得连另一张才有用」）。
  新增纯函数 `join_counterparts` + `recall::schema_card`，补在召回表之后（不抢相关度前排）。
  实测三题都开火：「手抓饼这个分类」补进来的正是 **t_goods** —— 分类→商品→明细缺的那一环。
  只补 1 跳（2 跳会从 6 张召回表拖进一大片、稀释 prompt；放宽要先量）。

**一个方法论错误，当场纠正**：我先拿 `why-not-compose` 的门分布去验这一刀，数字一点没动 ——
因为**那把尺子量的是确定性装配器，而这一刀只改 LLM 的 prompt**。用错尺子会得出「改动无效」的假结论。
改成加一行 `tracing::info!("JOIN 对面表补卡片")` 让它可观测。

顺带量出：**向量召回对某些问句很差** ——「各品牌的销售额」召回 6 张表里 4 张是市场费用表，
而「品牌」在 `t_goods` 上（2 跳外）。这是召回质量本身的问题，记作下一轮输入。

### D6 格式落地：word / ppt / pdf / 图片 / 旧 Office 端到端通了

`docker/parser/` 新镜像（741MB）把被 SAC 拦死的解析依赖装齐。**五种格式端到端实测**：
上传 → `status=embedded` → 有真块 → **问得出来** ——「境内培训的报销上限是多少」的引用里同时出现
`e2e_docx.docx` / `e2e_pptx.pptx` / `e2e_pdf.pdf` / **`e2e_png.png`（OCR）** / `e2e_doc.doc`。
`settings*.json` 的 `service_url` 已指向 8078（**不改这一行，格式支持在产品路径上到不了**）。

体积照实说：LibreOffice 那一项实测 501MB（正好在批面内），但**整镜像 741MB**，超出那个数字。
OCR 选型实测：tesseract chi_sim 0.22s/31.4MB vs RapidOCR 1.91s/576MB —— 印刷体上质量等价。

**代价是三条真缺陷 + 一条红线，都已修**（详见 `_DECISIONS` 二·AL）：
- 🔴 **红线**：`/parse` 曾是**发布到 0.0.0.0 的无鉴权任意文件读**（我亲手复核：
  `{"path":"/etc/passwd"}` → 200 返全文），而容器挂着全部客户文档。
  两层收容：绑回环 + `guard_path`（路径必须落在 `PARSE_ROOTS` 内，实测含目录穿越三条全 403）。
  根因是一句听起来对的话：「容器就是沙箱边界」—— 它覆盖容器网卡，覆盖不了 `-p` 的发布面。
- 🔴 `pymupdf4llm` 每页输出的 `-----` 被当正文块，**扫描件从「显式 422」变成「已入库 1 块」**
  （界面说入库了、问什么都答不出来）。一行过滤修掉。
- 🔴 `parser.ps1 probe`（交给业主的验收命令）**对那五种格式不可能红** ——
  失败只写彩色文字不置退出码。人眼看到红、退出码是绿，而业主看的是退出码。
- 🔴 新增的 12 个扩展名在 `ingest::classify` 的 `EXTS`（7 项）就被 400 拒掉 = 死代码。
  改成 19 项 + 一条 `include_str!` 读 Python 源对集合相等的跨语言判据。

**还欠三条**（评审抓到，都是静默丢内容那一族，未修）：多帧 TIFF 只 OCR 第 0 帧；
混合 PDF（部分页扫描图）静默丢页；扫描件 PDF 不 OCR（这条是显式失败，优先级最低）。

### 三条「静默丢内容」修完（AL6 那三条欠账）

形态相同：**HTTP 200 + 少了内容**，肉眼看回答看不出来 —— 判据只能是「只出现在第 2 帧/第 2 页的
唯一 token 有没有进块里」。多帧 TIFF 逐帧 OCR、混合 PDF 逐页补 OCR（`fitz` 渲染 → 已有的
`_p_image` 通道）、整份扫描件顺带解决。加 `OCR_PAGE_CAP=30`：几百页要**响亮失败**（`too_large`），
不许「OCR 前 N 页然后说已入库」。三条判据固化进 `parser.ps1 probe`（现 8 条，枪测能红）。

**二进制题集 5/5 全绿**（`kb_eval.py --cases tools/kb_eval_cases_binary.json`，含 KBB05 扫描件 PDF），
主题集仍 **16/16** —— 两个题集共 21 题真跑、零夹具阻塞。

三个自伤/陷阱都记进 `_DECISIONS` 二·AM：
- 我把页注塞进了 `sheets`（Rust 侧是 `Vec<Sheet>`）→ **整份响应会反序列化失败**；
  连带 `_pdf_page_ocr` 解包元数不对 → 整份 PDF **HTTP 500**。两次都是自己实测响应体才看出来。
- **`kb_eval.py --cases` 曾被静默忽略** —— 我以为在跑二进制题集，实际跑的是主题集。
  本轮第三次撞同一族，已补未知参数硬失败。
- **sha256 去重让「修好之后重传」量到历史**：三份夹具在修好后照旧报 `failed`，
  错误文案还是老进程的 —— 服务端按 sha256 命中了旧的 failed 行、根本没再解析。
  为此白查了一轮 `service_url`。验解析改动前**先删非 embedded 的文档**。

**一条订正**：我先说「KB13 的前提是错的」，**说错了** —— 题的前提对。但它的判据测的是**措辞**：
顺着题里的线索试了两轮提示词，两轮都是 10 采样 7 命中，没收敛；未命中那 3 次答的是
「补贴 180 元已含市内交通[^2]」——**回答了两问、有引用、没编数字，不是错答案**。
于是把有依据的那一族收进 `must_any`，并写清**真正该断言而现在没断言的是「不许编数字」**
（需要 `check()` 支持正则/数字白名单）。停手理由也记了：再推提示词就是拿 10 个采样过拟合，
而每加一条都在与「要点列全」「冲突披露」那两条实测有效的规则抢预算。

### 权限回显 + SALE13 口径 + 一条安全判据收紧

- **行权限回显**：`AskResult.scope_note`。受限用户此前看到子集却没有一个字说明「这不是全量」，
  会拿被过滤的数下结论 —— 不报错、无判据。值取自 `ScopedSql::is_unrestricted()`，
  那方法此前是**零生产调用方的死代码**（bit 早就算好了没人取）。
  实测：`tanlibo/city_manager` 拿到回显、`admin` 连这个键都不出现；枪测改成恒 `None` 立刻红。
- **SALE13**（`direct-agg` 却值不对，而「确定性路 0 失败」是不变量）：事后连跑，模型与 gold
  **逐行相同** —— **未能复现**当时行集。但分歧可证：gold 把查不到的员工归成一个「未知」桶，
  声明按原始 id 拆成 N 行 → 逐行对拍错位 + 真人被挤出 `LIMIT 200`。已按 gold 口径改声明，
  注释写明「依据是 SQL 文本分歧、不是复现」。**金文件当场抓到这次改动**（diff 只有那一处）→ bless。
- **ds 级 ACL 判据**：评审说「安全测试读克隆、加 `OR true` 也全绿」—— **核了代码，说过头了**，
  谓词是同一个 const 不会分叉。但**谓词之外**动手确实测不到（两处各 `format!` 一份）。
  已让生产与判据读同一个字符串 + 加整条**形状逐字相等**的锁；枪测在谓词外接一条放行立刻红。
  （我第一版把锁写成「只许一个 WHERE」当场红 —— const 里的 `EXISTS` 自带一个。）

**三处文档缺陷**（会让下一个人做错事）：`App.vue` 里还活着**第四处** `'·截断200'`（我在
`ResultPanel` 写的「前端不再持有行数上限」在隔壁文件不成立）；计划里追问阈值的举例是错的
（那句只有 12 字、今天就能识别，照它改是给不存在的 bug 打补丁）；计划给**已两次判推迟**的
「推荐追问」又排了 3 天预算。三处都已修并留了记录。

### AS02 那条稳定失败修掉了（占比判据不再只认已声明指标）

根因：`RequirePercentScale` 的唯一构造点条件是 `m.unit == UNIT_PERCENT`，**只认已声明指标**，
而「完成率」不在 `METRICS` 里 → 压根不造规则 → 答 `0.9576` 而 gold `95.76`，差 100 倍。
改法 8 行 + 4 词表（**占比/比例/百分比/百分之**，**裸「率」拒收** ——
库存周转率/效率/频率是真投影除法且绝不该 ×100，命中即把对的答案回炉改错）。
判据本体留在 kernel（`f.divide && !f.times_100`），不做除法的问句天然不受影响。
**误伤面逐题核过 114 道 = 0**；顺带把 SALE16（未声明的增长率）也网住了。

实测：**规则真的开火**（判据是日志：`rules=1 detail=[RequirePercentScale{metric:"占比"}]`）、
连采 6 次全是 ×100 形态、`--filter AS02` **✅ 1行一致**。
五项反向验证逐项实跑，每项都让断言变红（含「去掉去重守卫 → 既有那条测试红」）。

AS02 还有**另一半**形态（`'95.81%'` 字符串 —— 数算对了、判红在类型上），
那个形状 `times_100` 为真、新规则静默。另做一件：`system.md` 加第 11 条
「占比列只输出数字，不要拼 %」（`present` 本来就会加百分号，SQL 再拼就是双重加）。
⚠️ **既有断言当场抓了我一次**：第一版用 markdown 反引号写那条规则，
而「PG 提示里不许剩任何标识符反引号」那条断言立刻红 —— 留一个反引号 LLM 就会照抄。

### 新基线：评测 36/38（串行无并发）

`p50=17.3s p95=45.7s`。分层里**六项满格**：`分组 10/10`、`占比 2/2`、`时间 19/19`、
`码值筛选 10/10`、`派生指标 4/4`、`跨表 12/12`、`去重计数 9/9`。
AS02（占比标度）与 SALE13（业务员未知桶）都随本轮两处修改转绿。

剩两条红：
- **SALE15** —— 商品维度口径待 DMS 裁决（算术早已闭合：76,967 + 14,983 = 91,950 = 模型那个数）。
- **STK01** —— **订正我早先的怀疑：gold 没错。** 现在 gold 与模型**逐行相同**
  （都在第 2 行有那个空仓库名分组，26.3 万）。那次失败是空名分组当时排第 1 行、
  而**模型把它过滤掉了** —— 空名是真数据，剔掉就是静默少一个 26 万的分组。
  属 LLM 路抖动（模型有时自作主张加 `warehouse_name <> ''`）。
  修法候选：给 `t_winc_stock_report` 加一条 `meta.pitfall`「不要过滤空名分组，除非问句要求」——
  但那是 LLM 路、判据要靠采样，进下一轮方案。

## A/B/C/D 四期并行落地（业主选了全部四期）

八件落地：A1 口径判据四态 / A2 复合失败子问点名 / A3 值不在码表 / B1 钉 route /
B4 回炉喂全量口径卡 / C1 CLI `prev`（多轮题从「表达不出来」变成可跑）/
C2 追问改写六段 + 失败轮跳过 / C3 图表 `series`（混轴折线那一族）。
**D3 `/api/suggest` 主动砍掉**：没有端点的前端联想是死代码，且写不出能红的判据。

细节与逐条反向验证见 `_DECISIONS.md` 二·AQ。三件值得单独记的：

- **`docker-test.ps1` 的 build 半边此前恒绿**（管道退出码取自 `tail`）。这条让本会话之前
  所有「build 绿」的结论都不可信。一行 `set -o pipefail` 修掉，并自己验过
  （注入编译错误：改前 `[ok] 全绿 EXIT=0`，改后 `[FAIL] EXIT=1`）。
- **E09「本月销售额按品牌」`llm 44586ms` → `direct-agg 4531ms`**。走的不是「补一条明细级
  销售额声明」（那条今天会与订单头那条同名撞成歧义，要装配器会按维度可达性选粒度才行），
  而是给 `sales_breakdown` 加品牌支、**复用刚被业主裁决并逐数验证过的明细口径**（二·AP）。
  判据钉的是「品牌支与分类支的明细子查询逐字相等」—— 各自断言一遍的话，改一支忘另一支不会红。
- **A3 是休眠态，不算收益**：`origin` 列的三个写入点一个都没改，`enum_rules` 恒空。
  唤醒前置四条写在 二·AQ8；「评测没变化」既不能读成判据没用，也不能读成安全
  （唤醒那一刻全部 dict 对码列一次性同时开火）。

交叉审在「已修好」的改动里又抓到 5 条真问题 + 1 条恒真判据，全部已修并逐条反向验证：
第四态的用户可见那半零判据（实测删掉标注 91 条全绿）、改写结果侧没有确定性守卫
（模型抄 SQL 进问句是这一改新造的失败面）、68 条维度的 MySQL 反引号会进 PG 回炉提示、
新判据对 `!=` 会主动指令模型把「不排除」改成「排除一个真实类别」、
`assert_eq!(LITERALS.len() + 3, 9)` 是常量表达式永不可能红。

## 深度审计 + 全部修复（二·AS / 二·AT）

审计找到并修掉的**活的静默错答**：「上周/去年成交客户数是多少」被装配成按客户分组的 200 行
（`route` 仍 `direct-agg`、无报错、`caliber_note` 为空）。根因两层 ——
装配器的 `pick(metrics)` 与 `pick(dims)` 互不减词；`agg_template` 有自己的第二份时间词表，
**两份词表的差集精确地就是曝光面**。都已修，端到端实证四句全部 1 列 1 行。

顺带修掉 kernel 词表两条真缺陷（影响**全仓**残留守卫）：「最近」排在「近」之后 ⇒
「最近三个月…」剥完剩一个「最」；缺「本/上/这季度」⇒ 剩一个「本」。两族都静默回落 LLM。

另修：复核条数可以整体是假的（few-shot 投毒的对策失效）、配置项打错名字静默忽略、
最后一处静默 `unwrap_or_default`、`check-arch.ps1` 的假红**把同一趟的真违规盖在下面**、
三条恒真判据（其中一条「判据活着但被判的代码是死的」—— A3 唤醒仍休眠）。
删掉 3 个死脚本 + `/api/stats` + 三处假 `#[allow(dead_code)]`。

## datanote 五件套移植：S1-S5 全部落地（2026-08-01）

业主要求深度参考其旧项目 datanote（天工司辰 agent），把 **预览 / 用户选择框 / 分析 /
经验复盘 / BI 报表** 五件完整整合进现有系统，且「不许代码膨胀」—— 全部顺着现有骨架长，
零平行系统：一张 `meta.artifact` 表、一个预览面板、一条调度 spawn、经验蒸馏挂在
repair 成功钩子上。逐件裁决与实测见 `_DECISIONS.md` AX60-AX64。

- **S1 预览地基**：`meta.artifact`（产物与日报共用一表）+ 四端点（create 仅 admin /
  view / download / list，全部过会话归属校验）+ 服务端 `md_to_html`（escape-first，
  判据钉注入样例）+ CSP `sandbox allow-scripts`（无 allow-same-origin）与 iframe
  sandbox 双隔离 + 右侧 Codex 式预览面板（拖宽/关闭/下载/深链拦截）。
- **S2 分析报表 artifact 化**：`POST /api/analysis/report`（**零 LLM**：caliber 服务端
  从 SQL 重算不信回传，insight 是用户自己数据的回声）；解读面板「📄 生成报表」→
  立即开面板 + 留可复点的报表卡。写前校 conv 归属。
- **S3 用户选择卡**：need-intent 从 error 红横幅改为 `.ask-card`（主色引导卡：问题 +
  完整问法选项按钮 + Other 由输入栏天然承担）。**零后端改动**——挂起/续跑由会话追问
  机制（rewrite_followup + A17 日期继承）天然承担，datanote 的状态机在这里不需要。
- **S4 经验复盘**：`meta.memory` + 蒸馏（repair 成功钩子，**零 LLM** 模板，素材用
  闸门前 candidate 防行级权限跨用户泄漏，判据钉）+ A9 向量自愈第五类目标 +
  召回三因子重排（`sim × (1+0.1·ln(1+hit)) × exp(-age/30d)`，datanote hitCount+recency
  同构）进 prompt「经验复盘（参考，不是硬约束）」段，**绝不进口径判据与闸门**。
  端到端实测：蒸馏落库 → 补向量 → 相似问句 hit_count 全涨。
- **S5 经营日报**：每日调度（A9 同模子：advisory lock + kv CAS 标记 + 10min 轮），
  **口径零第二份**（全部数字由 `ship_sql_with` 同一构造函数出，`?` 占位 bind 走字面量
  通道）→ 昨日净销售额/订单数/退货额 + 当月累计（**报告日所在月**，1 号不踩空窗）+
  TOP5 省份/分类 + 30 天日趋势 + AI 经营点评（可缺席）→ artifact（conv_id=''）→
  侧栏「经营日报」入口。首轮实测 687 万/963 单/退货 3000，7 月累计 2.13 亿与全期口径
  交叉吻合。
- **测试**：workspace 674 passed / 0 failed（新增判据含：md 注入转义、报表 markdown
  纯函数形状、重排三因子、蒸馏接线源码扫描、日报 SQL 单一口径源、调度三件套）。
- **已知未修**：日报 TOP5 省份显示码值（`ship_dim(Province)` 在问数路径的既有行为，
  一致未动；要省名需在 ship_dim 层接 value_map 词典，影响面超出本轮）。

## 精简/深度双模式（业主 2026-08-01，AX65）

输入栏「精简|深度」切换（localStorage 持久化）。**精简 = 现状一字不改**（全 serde default）。
**深度 = AI 深度参与三段**：① 生成侧 SC 采样抬到 ≥3（多数派投票，实测两票提前收工）；
② 解读侧 Precise 档四段式深度分析（结论/关键发现/口径与可信度/建议，素材 15 行）；
③ 前端自动丰满链：问完生成结构化深度页与可分享 S2 报表卡；右侧预览仅在用户点击入口后打开。
实测抓并修：DeepSeek V4 把换行输出成字面 `\n`（含混合形态），`unescape_newlines`
≥2 处整体还原。复合问/0 行/知识库不触发自动链（没东西可解读或太贵，v1 注明）。

## datanote ChartTool 的 artifact 面：报表/日报嵌 SVG 图（AX66）

盘点后定位真缺口：图表在聊天气泡里早有（`Block::Chart` + BiChart.vue/ECharts），
**artifact 页里没有图**。手绘 inline SVG（bar/line/pie donut，零依赖 —— ECharts CDN
撞「沙箱 + 零外部资源」两条纪律）：横条带负值零线与 top「其他」收纳、折线 series
分组缺测断开、饼图负值丢弃。S2 报表收 chart 规格回声（只下标与图型，数据服务端
自取）、S5 日报三图（趋势折线 + TOP5 省份/分类柱图，图先表后），同一条
`⟦CHART:n⟧` 占位符 + `fill_charts` 机制。planCard / ApprovalGate / radar 判跳
（流式才有实时计划、只读无可批件、YAGNI），理由记入 AX66。685 测试全绿。

## 省份码值解码 + 回炉喂经验（深度优化迭代，AX67）

- **省份出码 → 出名**（AX64 记给业主的尾巴）：`t_customer.province` 存码，
  `t_regions` 就是地区码表（region_code UNI 不扇出、码↔名 1:1 ⇒ 聚合值与分组
  基数零变化）。`ship_dim(Province)` 正反两支 JOIN 解码，`COALESCE(字典名, 原码, '未知')`
  脏码回退诚实形态。SQL 层而非显示层 —— 别名到表列的映射链不存在，启发式
  解码是把错显示变成静默错数据的新入口。`t_regions` 登记 scope_binding global
  （builtin 32→33，否则受限用户省份查询被 fail-closed 整批拒）。金文件 B01/B08/B09
  re-bless（SQL 文本变，数值断言不动）。compose/LLM 路径省份列仍出码（声明 expr
  解码影响面超本轮，记录在案）。
- **回炉喂经验**（S4 补强）：经验的内容就是「上次修这个错的方法」，最该出现的
  地方是回炉提示 —— `gather_all_cards` 尾部追加经验段（与首轮 prompt 同一路
  召回 + hit_count，贴 material 尾部热区），接线判据钉着。
- **回归验证**（61 题 57 过 4 红）：4 红全归因 —— A01 较上月缺失与 B01/B08 view0
  是 8/1 月初日历伪影（prev 同窗为空不填 delta、0 行 chart 缺席，AX55 家族），
  E17 是重查询超时抖动（B10 家族，单跑 43.6s 过）。**数值断言零漂移**。
  日报重出 TOP5 全出省名（湖南省/广东省/山东省…），码值零残留。
  教训：判官输出不许 `| tail`（吃 exit code + 截失败清单，白跑 35 分钟），全量落文件。

## E17 收口：客户分类确定性模板（准确性迭代，AX68）

「销售额按客户分类」原走 LLM（43.6s + SC 撞 30s 超时 → 进程非 0 + 答案形状抖）。
`ShipDim::CustomerClass` 第九变体落地：CASE 翻名与 `meta.dimension` 声明同一份
字面量（twin 钉两侧），detect 护栏「回落 LLM」升级成模板（防商品分类劫道的本意
由变体本身接住）。实测 272ms（**160×**）；「上月…」12.3s 得 213,043,523.77
**与日报 7 月 MTD 分毫不差**（口径自洽铁证）。回归 E17 改钉 direct-agg。
全期无时间词版仍撞 EXEC_TIMEOUT 回落（B10 族，归 DMS 团队裁决），回落答口径正确。
686 测试全绿。

## 单号全族单据卡 + 名词五卡（业主「准确性」点名两类，AX69）

- **单号**：七族前缀全真库探得（销售/售后/对账/需求（下划线！)/开票/新开票/采购调拨），
  短码数字门槛防英文词撞。单据卡 = 头 Entity 键值 + **明细表**主表（DirectHit 加
  detail 字段，零 Router/serde 变化），明细失败保留头卡不塌。
- **名词**：两卡 → 五卡（+品牌/门店/业务员），全部复用发货口径实体过滤形态
  （负向分支 owner/shop 都经 o2 到原单，判据钉）。重名消歧三层：精确闸（饱饱博士=
  品牌、平安=业务员 实测两案）、显式前缀路由（业务员平安）、卡内精确行优先。
- 实测：DXO 头 68 键值 + 明细 2 行、DZD 明细 13 行、六名全对。691 测试全绿。

## 省份解码全路径收口（AX67→AX70）+ 回归 61/61

省份声明（meta.dimension）source 加 t_regions 连接、expr 翻省名 —— compose 与
LLM 卡片两路同时解码（ship 模板上一轮已修，日报跟着好）。实测抓：直辖市辖区码
（110100）在 level=1 下漏原码 → 放宽 IN (1,2)（UNI 无歧义）。「今年各月各省销售额」
直出省名零码残留。**回归 61/61 全绿**：AX68/AX69 零回归、月初日历伪影自然消失、
E17 新钉 direct-agg 过。省份显示三路径（ship 模板 / compose / LLM 声明）至此只有
一处解码形态。

## 图数据库完善（业主要，AX71：装配器前置修复 + 16 边 + 图补偿）

- **前置修复（二·AW 两条）**：装配器路径/桥接一律 LEFT JOIN + 被连表口径进 ON，
  `scope_parts` 跳过 ON 已带口径的表。旧形态 INNER + WHERE 口径 = 售后单的原单
  作废时售后单整行丢（少 13 单）；维度声明 LEFT 被 WHERE 重复口径打回 INNER
  （现役同形，碰巧无害）。判据钉形状 + 「口径只出现一次」。
- **16 条新 join_edge（两证制）**：384 个 mapper XML 提 79 候选（mine_joins.py）→
  生产库 COUNT 实测基数（probe_card.py，全非扇出）→ 种子。域：售后 3 / 主档空间 3 /
  活动费 6 / 票据对账 2 / 履约设备 2，**21 条 active**。
- **验收**：「今年各省份的售后单数」direct-agg 398ms 出 20269 = 权威 20073 +
  一周增量自洽；省份翻名；「未知」桶被 LEFT 保留。
- **AGE 图**：region_level IN (1,2)（与问数同组名字）；启动补偿（never/fail 先补，
  实测重启自动补 2616 客户 / 456 商品 / 101,399 边）。
- **t_regions 错注释**（DMS 原生复制粘贴错「开票申请单」）修元数据侧 + embedding 重生。

## 深度模式复合页（业主「花里胡哨」裁决，AX72）

深度模式改版：单入口 `/api/deep/compose` —— 一次问句出**总值 + AI 深度分析 +
维度拆解（省份/商品分类 图+表）+ 今年各月趋势（折线+表）+ 最近订单明细 + 口径 +
SQL** 的可分享 artifact 页，并在结果中提供预览入口；只有用户点击后才打开右侧预览。子问全部走 `crate::ask` 同一管线
（口径/闸门/权限零第二份真相源），命中 ship 确定性模板（SC=1）。拆解门（纯函数
判据）：单值 KPI + 销售词族 + 无维度词才拆。深度模式隐藏 AI 解读钮（默认做，
按钮是重复入口）。实测「上月销售额」一页 12 段 3 图 8 明细，KPI 与日报分毫不差。

## 业务库热切换（业主要，AX73）

设置页新增「业务数据库」段：`ReadOnlyMySql` 内置锁换池（**先建先验后换** ——
连不上/非只读旧池原样，可写库永远进不来），`settings.mysql_targets{name: DSN}`
目录（DSN 不出 settings.json：kv 只存名字、API 只给脱敏 host）、
`meta.kv['mysql_target']` 启动应用。实测：未知目标 400、同库别名来回切问数照常、
不可达目标 400 且旧池原样。health 带 target 名。**边界**：口径声明按 DMS schema
登记，切同构库（中台镜像）照常，schema 不同响亮报错不静默错答。
真中台 DSN 到位后在 settings.json 的 `mysql_targets` 加一行即可（无需重启）。

## 页面编辑配置（业主要，AX74）

设置页可直接增删改 `mysql_targets` 与 `llm_keys` —— 凭据**仍只住 settings 文件**
（不落 PG / 不进日志 / 不进响应，catalog 只给脱敏 host 与「已配置」布尔）。
服务端先在内存中完成整份 JSON 与 `Settings` 类型校验，再对正式挂载文件原地单次写入；
禁止复制含明文 DSN/key 的 `.bak`，凭据只允许存在于受忽略的正式 settings 文件中。
校验过的同一份配置才进入 RwLock 内存。保存即生效：热切换路径立即用新目录。实测加目标→文件
即时可见→不重启热切换→问数照常→删除同步。189 测试全绿。

## 设置页全功能（AX75：CRUD + 测试连通性 + 人性化表单 + 厂商预设）

- **测试连通性**：DB（一次性池 + 只读确认 + 版本）与 LLM（ping 回延迟/片段/用量），
  「测不通」回 200 ok:false（测试的答案不是端点故障）。
- **DB 结构化表单**：类型/地址/端口/库名/账号/密码 → 前端拼 DSN，不再手写连接串。
- **LLM 厂商预设**（互联网核实 2026-08）：千问/DeepSeek/智谱GLM/Kimi/豆包/OpenAI ——
  下拉即填 url/双模型/思考档/多模态，只填 key。思考级别映射各家关法，
  没档报错不静默。自定义供应商落 `settings.llm_providers`，保存当场进切换列表。
- 实测：真库 879ms、DeepSeek 894ms 回「正常」、坏 key 干净报错；kimi 加删全链。
  701 测试全绿。

## 设置页可修改 + 样式重做（AX76）

- **可修改**：每行「修改」钮回填表单，保存覆盖。凭据不出服务端：DB 密码留空 =
  keep_secret（服务端拼接旧 userinfo）；LLM key 留空 = 保留已存。**dms 可改**
  （改 mysql_url 强制先过连通性，过了才写+热应用；坏地址 400 未动文件）。
  **自定义覆盖内建**（同名 llm_providers 优先内建目录，删除即还原，判据钉）。
- **样式重做**：双卡片（色条标题）、目标列表行（状态点/标签/等宽 host/按钮组）、
  四列栅格表单（label 在上）、测试结果条、key chips —— 不再一排裸 input。

## 深度模式 v2 推倒重来（AX77：LLM 当分析师）+ artifact 分享

- **LLM 驱动报表**：`plan_report` 读注册表目录 → Precise 出结构化计划（板块子问
  +图型+标题）→ 板块并发走同一 ask 管线 → insight_deep 全文分析 → `bi_page`
  渲染（头部/KPI 卡行/AI 高亮卡/板块图表卡/明细/口径/SQL 折叠附录）。
  校验命门纯函数（sections 1..=4、chart 枚举、括号配平挖 JSON）；计划失败回退
  v1 启发式。实测「上月销售额」LLM 自出「区域销售贡献排行/品类销售结构分析/
  日度销售走势监控」三板块 3 图。
- **分享**：uuid share_token（只授读、免登录、同沙箱头），share/unshare/shared
  三端点 + 面板 🔗 按钮（复制链接）。实测发链接/非 uuid 404/撤销 404 全绿。
- **前端身份修法**：切换按钮 POST 的 login_name 从 query 挪进 body（后端只从
  body 读身份 —— 「切换不了」的真根因之一）。

## 生产授权被撤连锁（AX77a：判官与服务同管道）

- **现场**：dms_ai 的 xh_dms SELECT 授权白天被撤 → 炸出两个缺口：CLI 不吃 kv
  （回归 0/61 打错库）、MySQL 握手带默认库（授权一撤启动池握手死，kv 救不了）。
- **修法**：`dms_source` 先按 kv 解析启动 DSN（`db_boot_url`）再直接建池，
  serve/CLI 七处同一管道；kv 目标连不上回退 mysql_url（死也响亮）。
- 当前跑在 **dms_uat**（root 直连 + 会话级只读强制）。业主待办：DBA 补
  `GRANT SELECT ON xh_dms.* TO 'dms_ai'@'%'`；uat 建议换只读账号。

## 枚举归属收口（AX78：46 未归属枚举 → 69 行 dict 首开火）

`tools/enum_ownership.py`（数据驱动对拍，AX7 落地）：91 Java 枚举（整数码形态
补齐）× 1268 候选列 → cov + 词干打分决胜。抓到三个真坑：maps 构造 bug
（name 进 code 位）、`activity_level` 被 FeeType 误归（短码巧合，打分归正
ActivityLevelEnum）、`balance_status` 撞错枚举（probe 带拦住不开火）。
歧义闸：同分不自动归属（4 列人工）。**`origin='dict'` 0 → 69 行**，
RequireKnownValue 首次真开火（回退开关：dict 全改回 seed）。
实证「大型活动的费用」→ `activity_level = '3'` ✓。

## 交叉维度（AX79：月份×维度，BI 基本件）

「今年各月各省销售额」此前被单维劫成全年汇总（答非所问）。`ship_sql_impl` 加
month 键（正反两支各插 `DATE_FORMAT(时间列) AS m`，`GROUP BY u.m, u.k`）——
发货口径与单维**同一条**（判据逐条钉不变量），零新口径零字符串替换派生。
路由：月份**序列词**（各月/每月/按月/月份/月度）+ 另一维度词共现 → 交叉，
跑在单维前。呈现链零改动（present 的 line+series 天生接 (月份,维度,销售额)）。
实测：direct-agg 135ms、[月份，省份，销售额]、line(series=1)、省名解码；
「各月销售额」单维不变。705 测试全绿。

## 分享修复 + 思维过程 + 问题理解 + 聊天内嵌 BI（AX80）

- **分享没反应**：前端 URL 拼错（`share&login_name` 无 `?` 恒 404）+ 反馈写在
  只在设置页渲染的 llmMsg 里。修法：loginQuery 直接用 + 轻 toast。
- **思维过程（Codex 式）**：内存进度表 + 1.2s 轮询（rid=uuid，只有阶段名）——
  主查询→读目录→AI 深度思考→问题理解→板块并发→撰写分析→渲染→完成，
  loading 气泡逐条 ✓/▸。
- **先思考再开始**：PLAN 加 `understanding`（模型对问题的理解）—— 进思维步骤、
  聊天 🧠 块、BI 页 `.bi-under`（数字之前）。
- **聊天内嵌 BI**：compose 响应加 `page` 载荷（与分享页同源），气泡直接渲染
  理解/KPI/AI 分析/板块图表+迷你表/明细。
- **生产恢复**：dms_ai 授权已补，target 回 dms，graph sync 绿（60/143/538）。



















**563 绿 / 0 红 / 零警告，门禁 15 项全绿，回归 55/56。** 详见 `_DECISIONS.md` 二·AS / 二·AT。

## 下一步
- ~~**下一轮的两个前置条件**~~ —— **两条都已办**：① E09/E17 已钉 `route: direct-agg`
  且**都真的走确定性路径**（E09 靠新加的品牌支，E17 本来就是），下钻维度池现在可以加宽了；
  ② CLI `ask` 已开 `prev`/`prev_sql` 两位，`prev` 进了 `KNOWN` 白名单**且由 `ask_argv` 真消费**，
  三道两轮题落在 `tools/regression_cases_multiturn.json`（**独立文件，默认题集不含它** ——
  跑法是 `python tools/regression.py --cases tools/regression_cases_multiturn.json`；
  这三道**连库一趟都没跑过**，route 与取值属「未测」）。
- **下一步的接续项**（按性价比排）：① 唤醒 A3（`register.rs` 写 `origin` + `enum_rules` 按召回表
  过滤，前置四条见 `_DECISIONS.md` 二·AQ8）；② ~~撤 `agg_template` 让路门~~ ——
  **实测撤不掉**（二·AR）：门有两道、Router 走外面那道；全撤后「本月成交客户数」首格变成
  一个客户名（伪维度命中，200 行每行 1，route 仍 `direct-agg` 无报错），客单价丢 `ROUND`
  变成 `10222.77212139`。前置从两条变四条（+修伪维度命中 +客单价声明补 ROUND）；
  ③ B③ 下钻来源改 `meta.metric.drill_dims`；④ B④ 指标召回降档；
  ⑤ 补 C3 的 `series` 门禁（今天零覆盖，混轴折线明天回落也全绿）与两轮题的连库跑。
- **已定位、刻意不在本轮改**：`RequirePercentScale` 只由**已声明指标的 `unit='percent'`** 驱动，
  于是 AS02「今年售后单完成率」（完成率不是声明指标）压根不造规则，实测抖出 `0.95805` vs gold `95.81`。
  改法是**按问句造规则**（问句含 占比/比例/百分比/百分之/率 且没有 percent 指标命中时补一条），
  kernel 那侧的判据 `f.divide && !f.times_100` 已经够窄。**没有立刻做**：本轮已经叠了
  值过滤 + 赠品箱数两个行为变更，再叠一个就没法把变化归因到哪一处（抖动池 ≥9/38，
  单轮总分本来就分辨不出 ±2）。误伤一条 percent 规则会把**本来对的答案回炉改错**（裁决 二·G 实测过）。
- **已裁决并落地**：金额侧 `item_type = **'3'**`（二·J′ 结案，详见 `_DECISIONS.md` 二·AP）。
  以订单头 211,669,529 为独立第三标尺：`'3'` = 211,694,397（+0.0117%，结算尾差）、
  `'1'` = 136,325,361（−35.6%）、不筛 = 215,430,812（+1.78%）。
  只影响 `sales_breakdown` 的 Category 一支（E03/SALE13 走订单头，不经明细）。
  同批补 **SALE18-明细金额按商品分类**（38→39 题）—— 此前没有任何一道题覆盖这个口径，
  所以取哪个值都验不出来。`--filter SALE18` → ✅ 64 行一致。
- **待 DMS 团队裁决**：①SALE15 的「商品」按主数据名合并还是按明细历史名拆（两种判法都要求补 `meta.dimension` 的商品行，不声明就永远随机）②B10 超时的处置 —— **机制已坐实**：同一镜像同一问句连跑两次得到 `llm 93.5s` / `direct-agg 27.9s`，那条硬编码 SQL 本身要 ~28s，超预算就**静默降级成 LLM**。「B10 偶尔红」从此不该再当 LLM 抖动记账。三种解法（补索引/预聚合/抬超时）都动生产或把 28s 等待推给用户，是业务侧的选择。
- 功能已闭环（两个需求：多源智能问数 + 企业知识库）。剩余功能类只有推荐追问与 workspace 隔离，均已判为推迟。
- 轨 A 重构：~~T5 policy~~ → ~~T7a meta.rs 解体~~ → **T7b `direct.rs`(946) + `corrector.rs`(988) 解体**（compose/fastpath/correct 三组）
  → T6 DDL 与种子外置（**需连库**，验收标准是逐表对拍全等）→ T9 agent（`Answer` 统一，hybrid 与 `RowSet.redacted` 在这一步接）→ T10 server 瘦身。
- 连库验收（积压 12 项，见 `_DECISIONS.md` 二·D 与本轮清单）：其中 `retrieve.rs` 的 `ORDER BY (embedding <=> $2::vector) + 0` **必须 EXPLAIN 确认没走 HNSW**（错了是静默的召回退化）；`rekey_ds_pk` 会 DROP+ADD 10 张表主键，先在有备份的 PG 上跑。
- 顺手债：F6 few-shot 跨用户可见性、F7 scope 缓存 TTL+版本、F8（镜像凭据 / 越权删假成功 / `chrono::Local`）。
- 存量计划不变：指标与维度注册表扩充；M5c 端#1 SM4 登录转发（旧常量不得写入仓库 + 图形验证码）；企微生产加可信 IP 白名单。
## 三端统一权限与新 DMS 主库（AX78）

- 独立 UI、DMS iframe、企业微信统一角色换签，角色归属由 DMS 只读表校验。
- 当前运行目标：`dms` / `xh_dms`，健康检查 `session_read_only=true`。
- 修复新库 NULL 金额导致图同步整轮失败；图谱启动重建已成功。
- 验证：前端生产构建通过；Rust workspace 706 passed / 0 failed。
- 生产前置：关闭 `insecure_login_fallback`，继续保持 8100 仅本机/内网网关可达；数据库建议换专用 SELECT 账号；企微主动推送需配置 agentid。

## 独立 UI 登录与深度思考收口（AX81）

- `http://localhost:5180/` 已改为账号密码登录，无验证码；账号密码、启停状态、角色和数据范围均读取 DMS 同源数据。
- 未登录请求默认拒绝；多角色登录必须选择真实角色后换签；独立 UI 可退出并清理会话。
- 企微身份映射已对照旧 agent-harness：手机号优先、唯一花名回退、重名拒绝，权限仍走统一 DMS Principal。
- 深度思考过程改为 Codex 式状态卡；修正 UNION 分支时间过滤未进口径说明的问题，避免 AI 把独立年度趋势误判为本月 KPI 的冲突分项。

## 会话后台任务与深度报告优化（AX82）

- 修复 A 会话分析中切 B 导致 A 消失：会话各自保存运行状态、进度和结果，支持后台并行。
- 修复 Bearer 登录下 artifact 预览未认证：认证 fetch + sandbox srcdoc，分享与下载链保持权限校验。
- 深度模式新增确定性经营摘要（头部贡献、占比、趋势、环比），展示每个板块的实际子问题，并重排聊天与分享页的信息层级。
- 验证：前端生产构建通过；server 198 passed / 0 failed；浏览器实测跨会话回切内容保留，预览无未认证错误。

## Doris 跨系统单号与设备单准确性（AX85）

- 盘点数仓其他库后接入高覆盖资产：中台/基础系统拆分销售单号可确定性映射到 DMS 销售单，并展示对账差异和商品明细。
- 修复对账表多行 JOIN 导致商品明细翻倍：真实样例从错误 18 行恢复为 9 行，按业务明细主键去重。
- 设备需求单支持头信息、收货明细、投放明细；权限按 DMS 源码的“本人申请 OR 区域经理客户”实现，明细继承主单权限。
- 权限/关联档案扩展至 41 张表；当前运行目标为 Doris 数仓，连接会话只读。server 205 passed / 0 failed，真实 HTTP 两类单号均命中 `direct-doc`。

## Doris 跨库准确性增强（AX86）

- 复用数仓中已验证的客户市场费用事实和商品宽表字段；市场费用可返回总值及十类费用明细，
  商品分类不再依赖 Doris 中不存在的 MySQL 分类关联形态。
- 七类 DMS 客户分类已接入统一发货净销售额模板，发货和退货两支使用同一码值过滤；权限档案
  扩展至 42 张表，市场费用按客户编码继承当前 DMS 账号数据范围。
- 当前数仓没有可靠 DMS 发票事实，开票/已开票/专票问题明确返回不可计算，不再由 LLM 猜表。
- 修复可选 JOIN 造成的 SQL 空白漂移，16 个旧销售额金文件无需重签；仅商品分类因 Doris 宽表
  字段的有意变更更新金文件。
- 验证：workspace 731 passed / 0 failed，最终 server 207 passed / 0 failed；完整实库回归
  67 passed / 0 failed / 0 skipped。服务目标 `doris_warehouse`，会话保持只读。

## 多代理并行遗留修复 + 精简问数/深度BI/知识库关联收口（AX87，2026-08-08）

**背景**：前一日多代理并行开发（「所有开发完才测试」）撞上限额，工作树留下 5 处编译错误 +
18 个测试失败。本轮先对账裁决（改代码还是改测试，逐条按会话考古定方向），再完成三个任务的实测收口。

### 基线修复（按会话时间线考古定方向，全部有依据）
- 编译 5 处：`dms_lookup` 借用冲突（表名先落 String 再 normalize）；`recall/cards.rs`/`metric.rs`
  sqlx 元组推断被 `&str` 参数带偏成 `str`（turbofish 钉行型）；`present.rs` 漏搬 `compress`；
  `store.rs::related_docs` 多一个 `)`；`seed_defs` 测试模块裸路径。
- `gate_dms_lookup` LIMIT 超限：**拒绝是后写的**（昨晚 16:06/17:18 两Session钉死），agent 侧旧的
  「钳制」测试改为钉拒绝 + 默认补 LIMIT 50。
- `validate_query` 校验顺序按测试钉的消息优先级重排：查询级子句 → 单表形 → 结构扫描 →
  WHERE 谓词 → 函数调用 → 投影形状（投影类判定最后，子查询/多表/聚合/函数各归其位）。
- `is_safe_select_with`：parse 失败时先词法扫红线词（`INTO OUTFILE` 报「只读红线」而非含糊语法错）；
  `FOR UPDATE/FOR SHARE` 从扫描文本剔除，行锁由 AST 层报「行锁」。
- 未证明单据族（SPC/CG/SHOP_*/PZ 六族）：**昨晚 20:15 的权限收紧是后写的**——识别层仍认得
  （`resolve_code` 分类正确）但两条源都不产 SQL；direct.rs 四条昨晚之前写的旧测试改为钉
  fail-closed 现状。
- 自匹配陷阱三处（漂移测试 `include_str!` 把自己判红）：`UNION ALL`（entity）、
  `事实行数（非订单数）`（deep_api）、`login_name: Option`（vision_api）一律 concat! 拆开；
  artifact「AI 必须最后」改咬 `class="bi-ai"`（原来咬裸 `bi-ai` 会命中 <style> 里的 CSS）。
- `settings_api::del_llm_key` 变量改名使 `llm_keys.remove` 字面锚成立（先拒占用再改配置的
  顺序合约不变）；知识库 SYSTEM 提示词恢复三个实测措辞钉（「不展示」「表格数据行」「出自哪份」）。
- `present` 下钻维度过滤改全名匹配：两字符前缀把「销售日期」错杀在「销售额」里。
- `seed_defs` 默认销售指标判据锚到元组位（`("code"`），表名子串不再假红。
- `sales_fact` {TABLE}/{ALIAS} 与 `seed_defs` {sales_denominator} 登记 drift.rs ALLOW（常量/合同
  构造器产物，无外部输入）；`DELETE_RETIRED` 标 `ds:any`（退役行刻意跨源清理）。

### 任务A：精简模式问数 + 实体详情
- **Fast 统一意图门**（ask.rs `need_intent_reply`）：红线词 → Fast 判定（answer/clarify 两词协议）
  → 本地明确性降级（指标召回→残留→疑问词）。此前 Fast 只在旧规则准备反问时才调用；现在所有
  走到 LLM 兜底的问句统一过一次，模型失败才降级本地规则（明确问句继续、含糊问句澄清）。
- **裸型号识别**：`DHT150-6` 这类「字母+数字+连字符」纯 ASCII 码按商品型号解析（窄判据，
  日期段/纯字母词/中文名都不算）。实测数仓：型号只嵌在商品名尾部（规格字段是「450g*20袋」
  包装规格），型号解析改打 `t_goods.goods_name LIKE`（原来打订单明细规格字段，永远落空）。
- **商品卡**：补省区分布，与客户分布合并成一张标准化补充表（两个查询都由 `sales_fact` 生成、
  同一 `gate_on`）；数仓最近明细改共享 `sales_fact::detail_sql` 构造器。
- 实测（Doris 公网）：`DHT150-6` → entity-card 命中唯一商品；`商品编码 QY-YWKC500G` →
  主档+DWS 六指标+客户/省区分布+DWS 明细齐全；`客户编码 180451` → 主档/六指标/订单数/购买商品/
  信控/最近订单齐全；「那个东西怎么样」→ need-intent；「上周退款最多的三个客户」→ Fast 放行
  llm+repair 出数。

### 任务B：深度模式 BI 收口（实测）
- 产物页顺序实测：分析目标→KPI→同比/环比→高亮→头部贡献→板块图表→明细→AI 最后 ✓；
  SQL 折叠只在聊天载荷（page.sqls 8 条），分享页剥离 ✓；证据编号/内部话术零泄漏 ✓；
  前端已是不自动预览（点击才开）✓；板块执行 ordered_bounded(2) 受控并发、生产库拒分析 ✓。
- 修了实测抓到的显示 bug：确定性摘要「板块内占比 41.8%%」双百分号。

### 任务C：知识库安全关联召回（实测）
- 关联召回链早已落地（同目录/父子目录/文档族/版本/显式链接，seed 与 candidate 双侧内联
  `visible_docs`），本轮合成数据实测：显式 `doc_link` 关联文档以「结构关联/references」低分
  入列（0.0041 vs 直接命中 0.0197，低权重补充成立）；无 ACL 的同族文档全程不出现（跨权限
  零泄露）；相关性不足的同目录文档被 support_sim 阈值正确过滤。合成数据已清理。
- 答案结构保留标题/目录/章节/页码/文档族/版本（citations + doc 端点 related_documents）。

### 环境与运维
- Doris 切公网 `<DORIS_HOST>:9030`（公网地址按部署填）（settings.json `doris_warehouse`）。
- `WAREHOUSE_CATALOG_TIMEOUT` 10s→60s：公网链路目录探针实测 ~27s，10s 必超时（启动硬失败）。
- embed 模型（BAAI/bge-small-zh-v1.5）公网直连 HF 超时：`HF_ENDPOINT=https://hf-mirror.com`
  + `HF_HUB_DISABLE_XET=1` 可下载（xet 通道 401）。
- 验证：workspace 968 测试全绿（21 个 target 全 ok）；前端生产构建通过。

### AX87 补记：回归修齐（按用例契约逐条对账）
- **元素召回歧义修复**：`recall_elements` 加了 JOIN 后裸 `ds_id` 在 PG 报 ambiguous，元素卡整路静默缺席
  → 组合器断粮（A09 成交客户数掉进 llm+repair）。改 `ds_pred_at("e", 3)`。
- **数仓快路径补 `agg_template`**：订单口径高频模板此前只挂在生产链，数仓目标下 A07-A12 全掉 LLM。
- **实体卡按用例契约对齐**：DWS 指标标签改「DWS经营口径」；客户卡订单上下文字段（购买商品数/活跃月份数）
  进主档 pairs；最近订单列名钉 单号/时间/客户/数量/金额/状态；商品卡设备感知（物料类型=资产 →
  设备下单数量/设备订单明细下钻）；组合装编码（名称带 | 或 :数量 尾）不再冒充单品实体。
- **分类卡数仓落地**：分类主档改读已验证 `DW.dim_sku.class2`（ODS 镜像的 goods_category_name 全空）；
  分类经营指标仍按 warehouse_catalog 裁决 fail-closed。
- **图省份维度落地**：Province 节点来自 `t_customer.province` 经 `t_regions` 解码（事实省级列未确认，
  漂移守卫禁读）；`resolve_entities` 标签序 SalesRegion→Province→Goods；`into_slots` 三槽；
  「按省/各省」仍是分组语义，继续回落。
- **图就绪跨进程接管**：`GraphMeta` 顶点只在完整重建成功后写入（drop_graph 天然抹掉 = 缺席不可信），
  CLI 启动 `adopt_if_current` 核验目标名一致才接管 —— 此前 CLI 永远 not-ready，图题全掉 fallback。
- **设备单据**：`t_device_delivery_item` 投影按生产实表核验（无 receive_item_index/ledger_id 等）；
  设备族补 Doris 头表源（明细留空走生产注册表点查）；明细补充保留 direct-doc 路由标签。
- **生产点查索引核验**：改 `information_schema.STATISTICS` 全 CAST 投影（MySQL 8.0.28 按名解码 SHOW INDEX
  直接失败）；核验预算与用户点查 2s 红线分开（30s，一次性）。
- **C05 用例裁决更新**：数仓未同步单据族由「只说明缺口」改为「生产只读轻查询按族权限裁决取数」
  （2026-08-06 权限会话的后写裁决覆盖先写用例）。
- 判官超时随链路可调（`DMS_REGRESSION_TIMEOUT`，内网默认 60s 不变）。
- **「客户名+销售指标」确定性路径**（E02 族）：同步模板链接不住裸客户名问句，而「未确认限定」
  诚实卡会把它们全拦下。现在先探一次客户主档（同闸门、LIMIT 3、只验证存在性），探明就把
  名片段作为 `storename` 过滤交给共享 DWS 合同；探不到照旧回诚实卡，fail-closed 不变。
  别名补「买了多少」→ 销售额。
- **合同方言修复**：`Predicate::contains` 的 `LIKE … ESCAPE '\'` Doris 不支持（1105 语法错误），
  改 `INSTR(…) > 0`（两方言同形、子串语义本就是字面的）。
- **诚实卡占位 FROM**：纯常量投影过不了闸门 ConstantProjection 防线，`sales_fact_unavailable`
  补 `FROM dms_ods.t_dict_value LIMIT 1`（开票/对账不可计算卡早已这么写；B03/B04/B06 此前因此
  被放进 LLM 猜 JOIN）。
- 上午多代理遗留的 dead-code/unused 警告已清零。

### AX87 终验数字（2026-08-08，公网链路 Doris <DORIS_HOST>:9030）
- workspace 单测：21 个 target 全绿（968 项），零编译警告；前端生产构建通过。
- 实库回归 76/76 执行题全过（全量跑 47 题 + 公网抖动失败 29 题分批复跑全绿；R-D01 规则题随 A01 绿而恢复求值）。
- 多轮题集（`regression_cases_multiturn.json`）首次连库实跑 3/3：M02 的钉按真实诚实卡形状订正
  （原钉从未实跑过，语义不变：省份识别为未确认范围、禁止回接偷换）。
- 已知未办：分类**经营指标**（商品分类销售额等）仍按 warehouse_catalog 裁决 fail-closed，
  待事实内分类列验收后接入合同；公网链路下 CLI 单题启动 ~40-100s（判官 `DMS_REGRESSION_TIMEOUT=240`）。

## DataFoundry+Yuxi 集成 P0/P1（AX88，2026-08-08，七路并行代理）

调研沉淀：`docs/research/{datafoundry,yuxi}.json`；对账与分期：`docs/superpowers/plans/2026-08-08-datafoundry-yuxi-integration.md`。
- **A2 SQL 全状态审计**：`meta.query_log` 加 `status`（succeeded/blocked/failed/timeout，幂等迁移，
  错误列脱敏）；闸门拒绝/权限失败/超时全部落库。
- **A1 深度报告 claim 容差硬校验**：AI 分析的每个数值必须绑定到 evidence 查询值（±0.5% 容差、
  万/亿/百分数格式等价互认），绑不上整段回退确定性摘要（ANALYSIS_CLAIM_VALUE_MISMATCH 同款）。
- **A8 数据源级查询策略**：`mysql_targets.<name>.max_rows/timeout_ms` → connector fetch 入口取 min，
  生产 2s 红线只紧不松；调用方零改动。
- **A5 数仓目录降级快照**：`meta.warehouse_catalog_snapshot` 持久化最近一次成功探针（FNV 摘要）；
  探针失败按快照降级启动（trust=degraded 透出），无快照仍 fail-closed。
- **知识库入库**：分块 preset（general/qa/book/laws/semantic，上传 `preset` 参数已接通）+ chunk 字符
  偏移（start/end_char_pos 幂等迁移）+ content_hash 原子去重 + 失败可重试分派。
- **B5 reranker**：`DMS_RERANK_*` 环境变量开启（缺省关闭、行为零变化）；2×TOP_K 召回→精排→截断，
  失败/超时回退 RRF 原序并 warn 留痕。
- **B8 评估闭环**：`tools/kb_bench.py`（generate/run/selftest）——基准 18 题 recall@6=1.0/mrr=1.0，
  支持 --baseline 前后对比。
- 接线修补：enrich 拆解门改回受测试钉住的 `should_enrich`（上午重构曾内联了一个更弱的门）。
- 验证：workspace 1017 单测全绿、零警告；服务重启后 ask/深度/知识库冒烟通过。

## Yuxi 功能原样集成（AX89，2026-08-08，五路并行代理）
图片（tp/ 六张）钉的功能全部落地并实测：
- **文档预览弹窗**：文件（认证 blob 内嵌）/Markdown（chunk 按偏移去重叠重建）/Chunks 三页签；
  端点 `/api/kb/doc/{id}/markdown|chunks`（viewer 可见性内联，不存在与不可见同 403）。
- **知识图谱**：LLM 并发抽取（4 并发+退避重试+容错解析）→ AGE `kb_graph`（Entity/Chunk/MENTIONS/RELATION，
  确定性实体 id 归并）；构建/进度/子图/统计四端点，ACL 全程内联（撤权即不可见）；
  实测 73 chunk → 242 实体/178 关系零失败。前端 canvas 力导向图（构建进度轮询/悬停高亮/详情卡）。
- **知识导图**：文件夹骨架 + LLM 主题标签（失败回退纯骨架），meta.kv 缓存，regenerate 要空间写权限；
  前端 SVG 横排可折叠树。
- **RAG 评估**：出题（fast）→真实检索→答案生成→judge 全后台跑，`meta.kb_eval_runs/items` 落库；
  实测 4/4 题 answer_acc=1.0、recall@3+=1.0。前端报告页：汇总卡+逐题 R@k+评判理由+仅看错误开关。
- **深度模式子任务面板**：板块级 queued/running/done/failed+耗时 进度事件（与阶段同一脱敏纪律），
  前端聊天右侧任务面板（进度条+子任务卡+完成自动折叠）。
- **修的一个真坑**：DeepSeek 思考模式默认开 → 小 max_tokens 的 Fast 调用全返回空（图谱抽取/出题曾
  大批失败）；settings.json 补 `llm_extra_body: {"thinking":{"type":"disabled"}}`（目录默认值此前被
  空文件值架空）。
- 验证：workspace 1017+ 单测全绿零警告；npm build 通过；服务端到端冒烟全过。

## 二期深度融合（AX90，2026-08-08，五路并行代理）
- **图谱增强检索**（Yuxi B6 全链）：实体种子（向量/trgm 两路，权重 1.0/0.8）→ 1~2 hop 扩散（≤200 节点）
  → 自写幂迭代 Personalized PageRank 排 chunk → 第 7 路进 RRF（KG_WEIGHT=0.3）；图无数据/失败回退原路
  + warn；`DMS_KG_RETRIEVAL=off` 可关。实测检索命中带「图谱」通道。
- **证据引用追问**（DataFoundry EvidenceRef 简化形）：`/api/ask` body 加 `refs`（上轮结果片段，剥控制
  字符、500 字×3 段上限、标注入不可信指代素材），空 refs 与旧行为逐字等价；前端结果气泡「引用」
  按钮 + 输入框 chip 区。
- **排队追问**：会话内独立队列（运行中可继续输入、自动续发、单条取消）。
- **使用统计**：`GET /api/usage/summary`（本人口径聚合：今日/总数/路由分布/近 7 天/平均耗时；admin 加
  全局块），前端统计弹窗（自绘 SVG 柱状图）。
- **样例问题**：`GET /api/kb/sample-questions`（fast 生成 5 条、kv 缓存 24h、失败回退保守问题），
  前端检索测试区 chips 一点即查。
- 前端补强：图谱页挂载接管进行中构建的轮询；评估报告页字段映射修正（recall/accuracy 此前恒显示「-」）。
- 验证：21 测试目标全绿零警告；npm build 通过；usage/samples/图谱检索实测通过。

## 三期融合（AX91，2026-08-09，五路并行代理 + 前端一路）
- **分支会话**：`POST /api/chat/conv/{id}/branch`（from_seq 1 基序号，属主校验+同事务深拷贝，
  artifact 只读引用不复制 share token）；前端结果气泡「⑂ 分支」按钮切新会话。
- **Skills 提示词包**：meta.skill CRUD（admin 写、全员读、新建默认停用 fail-closed），启用的包注入
  深度报告规划提示词（untrusted 标注+截断）；前端「🧩 提示词包」管理弹窗。
- **后台任务重启收割**：启动时把卡 running/building 的评估跑与图构建标 interrupted。
- **外部 KB 只读连接器**：Dify 数据集检索为第 8 路召回（EXT_KB_WEIGHT=0.2，env 未配=字节级零变化），
  远程块走合成负 id + source_uri 来源标注。
- **Trace 时间线**：`GET /api/chat/conv/{id}/trace`（chat.msg payload + query_log 失败行组装五类事件），
  前端会话列表「🕓」抽屉展示（问/路/试/答/物节点+耗时+SQL 展开）。
- 验证：21 测试目标全绿零警告；skills/branch/trace 实测通过。

## 四期融合（AX92，2026-08-09，DataLink 数据地图全链落地 + 三侧契约归一）
- **数据地图静态推断**（DataFoundry DataLink 移植，`semantic::datamap`）：只读小样画像
  （已验证目录白名单、500 行 LIMIT、10s/表超时、聚合态/敏感列跳过、guard 全管道无后门）
  → 三类列间推断（joinable 值重叠分档 / synonym 名+注释 / distribution_similar 分布相似）
  → 全部按 pending upsert。CLI `meta datamap-build [ds]`（proof 铸造点在 main.rs，
  同 `meta autodiscover` 先例，非数仓目标 fail-closed）。实测 Doris 公网：55 表/1666 列、
  零跳过，落 185,293 条待审边（joinable 38,299 / synonym 141,618 / distribution 5,376）。
- **使用轨迹校准**（DataLink「静态打底、轨迹校准」，`semantic::datamap_usage`）：
  query_log 近 N 天成功行 → 方言双试解析 → JOIN 表对/同现列对（别名解析、CTE 排除、
  宽语句只记 JOIN 对、坏行留痕不炸轮）→ `co_occurs` 边 upsert，合并公式
  0.6×旧+0.4×归一频次（指数衰减校准，幂等可重入），**status 不进 SET**（人工结论不被冲）。
  按行自带 ds_id 分源聚合；裸列无法归属即丢弃。CLI `meta datamap-calibrate [days]`。
  实测 2 行成功日志 → 66 条列对边、0 解析失败。
- **契约归一（本轮唯一裁决）**：三路并行代理曾对 `meta.datamap_edge` 写出三个不同构表
  （PK(target,kind,src,dst) / PK(src,dst,kind) / id+ds_id+left/right）。裁决以复核域
  （`datamap_api`）形状为正本、kind CHECK 扩为六值闭集
  （join/lineage/joinable/synonym/distribution_similar/co_occurs），加
  `idx_datamap_edge_uniq(ds_id,kind,left_table,left_col,right_table,right_col)` 作两个
  写入侧 ON CONFLICT 的仲裁唯一索引；三处 DDL 文本逐字一致（SHA 对账过），
  CREATE IF NOT EXISTS 先跑者赢但同构无 race。静态侧 `db.table.col` 拆裸表名+列名落库
  （目录保证基础表名跨库唯一，db 维度留 evidence）。
- **地图 API 六端点**（`server::datamap_api`，已接线）：nodes（表/列目录，敏感列不进）、
  edges（注册表合同边 ∪ 推断边统一列表，kind/status 闭集过滤，**confidence 降序**——
  18 万条量级下按入库先后会把最强候选淹掉）、paths（两级 BFS，边取组合器同一加载口，
  500 边护栏 422 不静默截断）、accept/reject（仅 admin；pending→终态不回迁 409；
  **join/joinable 且双列齐才进 `meta.join_edge`**，CAS 与注册表写入同一条 CTE 原子完成，
  缺列 422 不落假账；其余四类只落复核账）、`/api/audit/sql`（query_log 全状态审计，
  admin 全量、非 admin 谓词强制本人）。前端 DataMapPanel / SqlAuditPanel 已挂 App.vue。
- **实测复核门**：accept co_occurs → `join_edge_written=false`（绝不进合同）；
  accept joinable → `join_edge_written=true` 且 `meta.join_edge` 实测落 active 行
  （冒烟后已清理该测试行）；重复复核 409；reject 落账 reviewed_by。
- 验证：workspace 21 测试目标全绿零警告；前端 npm build 通过；判官回归通过
  （`DMS_REGRESSION_TIMEOUT=240 tools/regression.py`，公网链路）。

## 五期融合（AX93，2026-08-09，六路并行代理：ODS 推导应答 / 血缘反推 / 提速 / KB 格式 / 图谱导图 / 差距调研）
- **ODS 推导应答**（用户裁决：合同未覆盖不许再直接「不可计算」，允许 ODS 明细推导但必须显式标注）：
  `direct_hit` 两个卡臂接 `ods_derive`——资格闸（数仓目标+dms）→ 目录明细层候选 top6
  （血缘边可选加权，血缘表空照常工作）→ 仅候选表 schema_card 进 prompt → precise 组 SQL →
  AST 用表硬校验 → 与直连同一个 gate_on 闸门 → 预执行。回答带 `-- 推导口径` SQL 头标 +
  trust=review + 前端「推导口径·未经合同验证」提示条；query_log.route='direct-derive' 可审计。
  **合同在就永远走合同**；推导任一环节失败回落原不可计算卡（一字不改）。
- **derive 双语义闸**（判官 E 系列 5 题红的根因裁决：不是拍脑袋回退，是逐题查 SQL 后补闸）：
  ① 标签语义对账——中文输出别名必须在取数列的注释/列名里有出处
  （`amount(明细金额)` 改名「开票金额」=虚构指标，拒；`created_by(创建人)` 改名「业务员」=码值劫走，拒；
  「销售额」⊂「销售额(元)」放行）；② JOIN 证据闸——每个跨表等值键必须命中 active 合同边或
  joinable≥0.9/已验收边（sku↔goods 只有 0.35 弱证据 → 拒，防扇出膨胀）。任一闸拒 = warn 留痕 + 回落原卡。
  实测三问：合同内题仍 direct-agg/verified；「本月销售额按门店」走 derive 命中
  `dms_ods.t_winc_sale_report`（正是血缘边指的表）返回 200 行；「待确认对账单」推导无候选回落原卡。
- **血缘反推**（`semantic::lineage`，纯 PG 元数据不打 Doris）：目录 layer/domain/grain 直证（词边界）+
  列 schema 重叠三档（剔 15 个技术列、注释一致加权）+ joinable≥0.9 佐证 + 命名规整弱信号（不单独成边），
  封顶 0.95；upsert 六元组、status 不进 SET。实测 546 对评估出 **80 条 lineage 边**（幂等重跑不翻倍）。
  `table_relations` 按表聚合四源关系卡（合同/血缘/统计/共现），API `GET /api/datamap/relations` 已接线。
  CLI `meta lineage-build [ds]` 已接线。
- **提速**（先测量后优化，全部零行为变化）：KPI 题 4 次串行 Doris 公网往返改 join_all 两波
  （direct-agg 长尾大头）；意图门与召回 tokio::join!（LLM 路径省 0.1~0.5s）；召回分 4 波并行
  （~12 次串行 PG+2 embed → 4 波）；注册表 7+6 次串行读改并行；embed/doc HTTP Client 复用
  （每次调用省一次 TCP 握手）；补 4 条分段计时日志（precise ms/取数 ms/主查 ms/gather ms）。
  SC 多采样共享一次 gather（deep 模式省 2×全程召回）。
- **KB 上传+预览对齐 Yuxi**：白名单 19→23 项（+json/log/html/gif；json 美化代码块、html stdlib 去标签
  转文本、svg 拒收可执行脚本面）；单文件 50MB→20MB（settings 可配，前端逐个预校验不中断队列）；
  预览按类型分派（图片 <img>、CSV 表格嗅探分隔符、PDF iframe、md 渲染、Office 看解析 Markdown）；
  图片走既有 vision OCR 全文可检索；下载 mime 改落盘扩展名白名单（顺手修 svg 被当图片内嵌的 XSS 洞）。
- **图谱/导图加强**（Yuxi 对照）：实体类型着色+图例（label 字段本就有，零回填）、节点搜索定位
  （防抖居中+计数）、双击/按钮展开邻居（subgraph 加 center 参数，ACL 内联不变）、边标签缩放阈值；
  导图导出 PNG/SVG（同 layout 重生成内联样式 SVG）、文档数徽标、折叠记忆（localStorage 按空间）。
  **修既有 bug**：`normalizeGraph` 把边端点契约 source/target 当 src/dst，图谱边一条都画不出来——已修。
- **差距清单**（调研 agent 产出，待排期）：top5 = ①KB 问答反馈闭环（knowledge 路由不落 query_log、
  反馈绑不上 trace_id）②凭据 AES-GCM 加密存储 ③高阶解析引擎档（MinerU/云 OCR）④RRF 权重入设置
  ⑤数据地图完备性包（correlated 推断器 + MCP 暴露）。
- 验证：workspace 21 测试目标全绿零警告（server 415 / semantic 158 / agent 165 / knowledge 115…）；
  前端 npm build 通过；判官全量回归 76/76 通过 0 失败（1 跳过为既有取数缺失；E 系列 5 题在双闸落地后恢复全绿）。

## 六期融合（AX94，2026-08-09，五路并行代理：差距清单 top5 落地）
- **KB 问答反馈闭环**（Y2，知识运营核心飞轮）：埋点收在 knowledge 层唯一收口 `answer()`
  （/api/kb/ask、分诊 Knowledge 分支、kb_eval、MCP 四路通吃，main.rs 零改动）；
  `kernel::qalog` 与 server query_log 吃同一份 INSERT_SQL/STATUS/CLIP（写口唯一）；
  route='knowledge'、trace_id 上 wire、sql 列=检索摘要（`KB检索：引用N篇（名…）`）、
  status 四值闭集（ACL 拒=blocked）。反馈复用 /api/feedback（绑定谓词 trace_id+本人，
  路由无关零扩展），前端 KbAnswer 👍/👎（localStorage 记忆、可改主意服务端 upsert）；
  usage 统计 kb_ratio 双指纹（upload_% ∪ route='knowledge'）。实测：KB 问答落账
  （trace_id/引用摘要/llm_calls 齐）→ 反馈绑定 → meta.query_feedback 行（冒烟后已清理测试行）。
- **凭据 AES-GCM 加密**（D1，ring 已在依赖零新增）：`enc:v1:<base64(nonce‖ct‖tag)>`，
  落盘密文/内存明文；启动幂等迁移（明文自动加密、只读挂载 warn 不阻塞）、二次启动逐字节不变；
  敏感字段清单唯一事实源（db.rs SECRET_*：mysql_url/pg_url/pg_ro_url/llm_api_key/wework_secret/
  llm_keys/datasources/mysql_targets.url/mcp_keys 键名）；读取侧全透明（resolve_provider/dsn_map/
  db_targets），读 API 本就掩码不松。密钥：DMS_SECRET_KEY（sha256 派生，唯一跨机形态）或机器指纹
  （跨机/容器重建不可迁移——docker 必须配 env key）。Python 判官链 tools/settings.py 同逻辑镜像
  （.venv 装 cryptography，仅撞 enc:v1 才 lazy-import）。实测：敏感字段全 enc:v1、服务解密连 Doris 正常。
- **扫描件 PDF 高阶档**（Y1，裁决不装 MinerU，用既有 vision 基建）：低文本量检测
  （页均 <50 或全文 <200，env 可调）→ fitz 渲染 200dpi → vision OCR（千问 dashscope 档，
  tesseract 降级）；30 页护栏超帽响亮失败（不搞半吊子入库）；`_pdf_fitz` 二级修掉
  「混合扫描件图像页静默消失」；CAPS 两档自报（text|ocr 可用性，/health 透出）；
  selftest 四段钉（阈值/桩编排/真实夹具/自报一致），实测 OCR 缺席时 422 no_text_layer 确定性失败。
- **RRF 权重入设置**（Y3）：settings.json 新键 `kb_rrf_weights`（四路 metadata/relation/kg/ext_kb，
  缺省=旧常量字节级等价有单测钉死；负值/NaN 拒）；`POST /api/admin/settings/kb-rrf-weights`
  （admin 门禁、保存即热生效）已接线；四条链（ask 主链/kb_api/kb_eval/mcp）全部吃 settings 快照。
- **图谱运营三端点**（Y4，已接线）：failed-chunks（未入图块清单，failed/pending 分类+分页）、
  reset（按空间清图+状态行，幂等，构建中 409）、reconcile（孤儿=doc 删/禁/失效，
  dry-run 默认开、执行闸超帽只许 dry-run，真删按 边→Chunk→实体 三步幂等）；
  前端 KbGraph 失败块抽屉 + 清图/修复按钮（带确认）。
- **correlated 推断器**（D2，DataLink 第四类）：同表两列联合采样（一条 SQL 两列走 MapGate 全管道，
  每表前 8 数值列两两 28 对封顶防 O(n²)）→ Pearson 判据 + Spearman 佐证，|r| 三档 0.4/0.6/0.8；
  跨表不做（行对齐超统计推断边界）。kind CHECK 幂等拓宽至七值（DROP+ADD CONSTRAINT，
  dev 库事务内实证）；三处 DDL 常量逐字一致（SHA d6245e71…）。实测 **149 条相关边**，
  top：ads_fin_profit_loss_dnf 成本~销售额 pearson=0.9996（447 成对样本）。
- **数据地图 MCP 暴露**（D3）：mcp_api 三工具（datamap_search_nodes / datamap_find_paths /
  datamap_list_pending_edges），取数层抽共用（REST/MCP 零 SQL 复制），ds 可见性同一判据函数。
- 验证：workspace 21 测试目标全绿零警告（接线清掉了 12 个未接线 handler 的 dead-code 警告）；
  判官全量回归 76/76 通过 0 失败（1 跳过为既有取数缺失）。

## 七期融合（AX95，2026-08-09，五路并行代理：韧性/产物层/集成面/运营/上下文）
- **深度报告断点续跑**（D4）：`meta.deep_run/deep_section` 账本（板块一完成即落账，落账失败不挡报告）；
  lazy 收割（rid 不在进程 ACTIVE_RUNS = 执行器已死 → interrupted）；手动续跑
  `POST /api/deep/resume`（已完成板块零重跑、queued/failed 按计划重跑、主查询幂等重跑且权限重新过闸）；
  并发闸（ACTIVE_RUNS 认领 + PG CAS 双保险，RunGuard RAII 释放）；裁决手动续跑不做自动（防重启风暴）。
  D8 验收透出：规划契约加可选 assertion（销售编译携带、确定性计划降级 None）、进度事件前置透出、
  末次证据解读**同一发** LLM 自评 满足/部分/未满足（不理会 JSON 指令回退纯文本，绝不阻塞报告）；
  前端报告页断言区 + 子任务面板板块卡 + 错误气泡「续跑」入口。
- **产物层**（D6）：版本链 (conv_id,kind,title)+version 库内单语句自增、唯一索引兜底撞号、
  老数据 row_number 回填（98 行实测 max v6）；versions/export/promote 三端点（版本解析重新过权限判据）；
  导出 csv（BOM+OWASP 公式注入护栏）与 **手写最小 xlsx**（ZIP(stored)+SpreadsheetML 全 inlineStr，
  零新增依赖，openpyxl 回读+CRC 校验实测通过）；promote 走 `chat::save_msg` 公开写口
  （artifact_promote 事件，不污染追问改写），目标会话属主 403 闸；前端预览面板 🕘版本/⬇CSV/⬇Excel/📌引用。
  （注：artifact 面沿用既有 Bearer 会话门禁，D10 API key 不适用 —— 端点挂载与门禁实测，核心逻辑单测覆盖。）
- **REST API key 双通道**（D10）：`auth::resolve_identity_dual`——X-API-Key 显式 / Bearer 双义头
  （先会话后 key），命中 mcp_keys → 该 login（role None 现算，多角色 fail-closed 与 MCP 同语义）；
  **错 key 绝不降级** login_name 自报（401）；常量时间比较、日志不回显 key。main.rs::resolve_identity
  一处接线，13 个调用点全跟着生效。实测三态：错 key+自报=401、对 key（X-API-Key/Bearer 两形态）=200。
- **队列 steer 插话**（Y5）：`POST /api/chat/conv/{id}/steer`（属主闸；未运行 409、队满 429、脱敏 500 字+
  untrusted 标注）；执行器安全点=尝试循环顶，命中整批并入问题上下文重走一次组装（仅一次防循环，
  重组失败沿用原 SQL 不杀死运行）；前端运行中「插话」条。实测 conv 未运行 409 语义正确。
- **KB 运营三件套**（Y12+Y7）：`POST /api/kb/ingest-url`（SSRF 护栏全家桶：形状闸/IP 闸含 v6 与
  v4-mapped/DNS rebinding 钉地址/重定向逐跳重验/15s+5MB 帽；html/pdf 复用既有 ingest 全流程，
  source_uri 记最终落地 URL）；`GET /api/kb/space/{id}/export`（读权限、500 行帽分页）；`POST
  /api/kb/doc/{id}/description`（fast LLM 生成+整形 500 字，失败不写回不编造；description 列走
  KB_DDL_DELTA 幂等迁移，已挂进 metadata 召回语料 GREATEST）。实测：file://=400、127.0.0.1=护栏拒、
  导出分页 next_offset 正确、描述生成质量高（操作手册摘要精准）且已落库。
- **长会话上下文**（Y10+D7）：两级摘要能力层（早期轮>6 压摘要、fast 5s、失败回退硬截；
  缓存键 FNV 逐轮指纹防「摘要漏新消息」；表格>50 行/SQL>800 字符外置）；运行时接线待 chat 属主
  （诚实标注，摘要层全纯函数+全测）。上下文落账：query_log 加 `context_summary` 列
  （prompt_chars/cards/trimmed/summary_used，脱敏只结构尺寸表名），gather→暂存→同一 spawn 落账；
  `/api/audit/sql` 透出（手写 FromRow 破 sqlx 16 元组上限），前端审计面板小展开。
- **公网身份超时修复**（回归暴露的真问题）：身份/角色/权限静态查询通道（fixed.rs 的
  MYSQL_FIXED_TIMEOUT）2s 是局域网时代的红线，公网身份库下登录态/身份核验间歇性硬失败
  （判官 CLI 被「超时 [dms-auth] 2.0s」打死，每次失败题不同 = 抖动指纹）。抬到 8s
  （只抬天花板不改正常耗时；生产点查 2s·50 行红线不动），钉测守「身份通道永不比点查红线更紧」。
- 验证：workspace 21 测试目标全绿零警告；判官全量回归 76/76 通过 0 失败（修复后复跑确认）。

## 八期（AX96，2026-08-10，小程序接入 + 服务器部署）
- **部署上线**（117.72.32.186，京东云 Ubuntu24 + docker29）：源码 git archive 上传 →
  服务端 docker 构建（crates 直连卡死改 rsproxy 镜像；rust 镜像 CARGO_HOME=/usr/local/cargo 的坑）
  → PG（age/vector/pg_trgm）+ embed/解析（host venv + systemd 常驻，全格式解析档实测全绿）
  + web（nginx 容器）。Linux host-gateway ≠ 回环：PG/8100 改绑 docker 网桥 172.17.0.1（公网够不着）。
  宿主 nginx 兜底 server 切到新系统（旧 dms-copilot 配置备份留档）；注册表快照导入 +1283 行、
  向量自愈回填 491 行。实测：本月销售额 direct-agg/verified、按门店 direct-derive/200 行。
- **前端修复**：`crypto.randomUUID` 仅安全上下文可用（http://IP 部署没有它）——
  `format.ts` 新增 `uuid()`（getRandomValues 降级 + Math.random 兜底），App.vue 5 处切换。
- **小程序接入**（xh-xcx uni-app）：底部 tabBar 新增「AI」tab（首页/分类/AI/购物/我的，
  图标 static/3-*.png 按既有编号规律生成）+ `pages/ai-chat/ai-chat.vue` 问答页
  （气泡流/结果表格横滚/SQL 折叠/推导口径提示条/语音输入仅微信小程序端/notLogin 引导）。
  后端 `xcx_api.rs`：`POST /api/xcx/ask` + `GET /api/xcx/me`——`x-access-token` 经
  `{xcx_auth_base}/login/getLoginInfo` server-to-server 校验（60s 进程缓存、5s 超时、
  白名单只认 code=0），解析出 login_name/role 后进同一条 ask 主管道（多轮 conv_id 语义不变）；
  token 失效映 `{code:30007}`（小程序拦截器自动弹登录）；`xcx_auth_base` 未配置 = 404 fail-closed。
  实测：无 token/假 token 均正确 401+30007。
- 验证：server 485 测试全绿零警告。

## AX97（2026-08-10，嵌入/小程序问题修复批）
- **SSO 嵌入 401**：DMS 前端 token 由生产 DMS 签发，AI 后端却拿测试库地址验——
  `dms_base_url` 双侧统一为 `https://dms.huangjiaxiaohu.com/dms-api`；AI 地址全环境走
  `VITE_AGENT_DOMAIN`（xh-dms-fornt home/index.vue 删 DEV 写死分支）；
  小程序 env 补 `VITE_AI_API_URL`。
- **深度/知识库 403**：SSO 会话角色带 `__dms_federated_role__:` 前缀，只有 `auth::load_principal`
  会剥——但 11 处端点直调 policy 版（不剥）→ 全收口（含 usage/vision，守卫测试钉死零直调）。
- **实体识别准确度**：公司形态（线下-前缀/公司后缀）证据收窄候选类型（客户/门店）、
  类型优先级替代 label 字节序排序（客户不再被员工表同名行压掉）、triage 加裸实体名闸
  （形态命中必走 Data 路，不再 LLM 抛硬币）。实测：客户名→客户卡（编码 182980），
  商品名→商品卡，不再误出订单列表。
- **小程序结果呈现**：编码列 nowrap+数值右对齐+表头吸顶+斑马纹；≤3 列 ≤3 行渲染键值卡；
  空结果占位；段落化答案。待 HBuilderX 构建验证。

## AX98-AX99（2026-08-10，嵌入/呈现/推导修复批）
- **SSO 嵌入 401**：token 签发方（生产 DMS）与验签地址不一致——dms_base_url 双侧统一生产 DMS API；
  AI 地址全环境配置化（VITE_AGENT_DOMAIN / 小程序 VITE_AI_API_URL）。
- **federated 角色前缀收口**：11 处端点直调 policy 改为 auth 收口（剥前缀），守卫测试钉死。
- **实体识别**：公司/商品形态证据收窄 + 类型优先级 + triage 裸实体名闸（客户名不再被判成员工）。
- **呈现层**：列名中文化（注释/转译表/词元）+ 码值翻译（待收货(110)）+ 全端金额两位小数；
  精简模式结果卡放宽、意图澄清结构化选项（web+小程序可点）。
- **ODS 推导三修**：时间桶别名豁免（「各月」类不再被闸 1 误杀）、空结果换候选表重试
  （t_winc_sale_report 无数据自动换 t_sales_order）、闸 1 加口径词通道（核心销售词可映射度量列，
  开票类仍拒）。实测：客户限定各月销售额 8 个月数据答出。
- **销售 KPI 补充块**：单指标销售 KPI 自动带 成本/收入/毛利额/毛利率（同口径并行取数，
  主回答 SQL 一字不动）。实测本月销售额 7173.82万 + 毛利率 19.65%。
- **服务器 KB 上传修复**：kbdata 路径软链 + 块落库 CTE 括号错位（FOR UPDATE 掉出 CTE）修复，
  两篇失败文档重处理 embedded。
- 验证：21 测试目标全绿零警告；判官回归 76/76 通过 0 失败；服务器全量部署实测。

## AX100（2026-08-10，小程序 KB/权限/意图治理/导图内容级）
- **小程序 KB 问答修复**：KB 回答的字段是 `markdown+citations`（kind=text），小程序原来只认
  `answer` → 三路兜底 + 气泡加「参考来源」列表（文档名+页码+章节）。
- **KB 管理入口权限配置**：`kb_manager_grants`（roles/logins，缺省=仅管理员）；
  管理面（上传/删除/移动/授权/导出等 20 个端点）统一过闸，检索面（ask/search/预览）不动；
  前端入口按钮按配置显隐。
- **意图治理**：主题出界（如「积分」）→ 新 route `no-topic` 直接答「还没接入」+ 候选问法，
  不再走 SQL 试探报错；fast 判定三词协议（answer/clarify/unsupported）；复合「其中+极值词」
  拆解修复（活动费用+最高客户实测拆对）；注册表零覆盖兜底反问。
- **结果卡降噪**：trust checks/口径明细/权限回显默认折叠进「核查详情」，首屏只留答案+必要提示。
- **导图内容级**：docx 伪标题识别（编号/加粗短句→章节结构），实测巡店 SOP 出
  「一、巡店前准备 / 二、店内标准作业执行>到店拍」真章节，sections 端点返回正常。
- 验证：21 测试目标全绿零警告；判官回归 76/76；服务器全量部署实测。

## AX101（2026-08-10，六角色交叉评审 + 39 项优化批）
六路只读评审（问数准确性/WebUX/小程序UX/性能/安全/KB质量）实测产出 39 项发现，七路并行修复：
- **准确性**：错别字归一词表（消售→销售，同题同答案）；维度值先探成员再客户名 LIKE（直营/湖南
  实测出数）；尾词剥离（同比/环比/怎么样/其中占比不再误报不可计算卡，同比直接占主 delta 位）；
  近N天 off-by-one（7天=今天+前6天）；趋势 insight 排除不足月端点；排行 insight 截断后不报占比；
  毛利率 delta 用百分点；主题出界改走 no-topic 文案。
- **性能**：回炉维度段按召回面过滤（prompt 省 ~15-18KB/轮）；embed 单槽 memo + 注册表读取并行；
  校正器 N+1 改 ANY 一次取回；trace 列表库侧投影 3KB 截断（+msg_payload 单条端点）；
  nginx gzip（静态资源/JSON 省 ~70% 字节）。
- **安全**：login/sso/xcx per-IP 限流（20/分）+ xcx 失效 token 60s 负缓存；datamap/ds/insight/deep
  错误原文改固定文案+warn 留痕；deep/progress 加属主闸；MCP key 复用常量时间比较；xcx 限长。
- **WebUX**：TracePanel 产物链接走沙箱预览管线；移动端抽屉侧栏；顶栏折行；KbPanel 中间档卡片化；
  核心操作键盘可达；触屏 hover 按钮常显；KbEval 状态色修正；toast 层级 1300。
- **小程序**：语音看门狗三件套（hardReset/onHide/网络恢复）；表格手势冲突解除（只 scroll-x）；
  会话本地持久化（按用户隔离 50 条）；发送可取消+超时文案；单元格可复制；贴底才自动滚。
- **KB 质量**：xlsx read_only 丢列修复（reset_dimensions+降级重读）；标题-only 块并入正文块；
  精确 token ILIKE 路替代恒 0 的中文 FTS；表格行感知分块（表头重复）；图谱实体归并去 label+
  抽取 stoplist+关系受控词表（需重建生效）；空结果兜底带检索范围。
- 验证：21 测试目标全绿零警告；判官回归 76/76 通过 0 失败；服务器全量部署实测。

## AX102（2026-08-11，3348 条优化盘点落地 + 部署事故四连修复）
六角色评审 + 两波全仓审计 swarm 产出 **3348 条优化点**（docs/OPTIMIZATION-BACKLOG.md，
含 DMS 后端源码校准项、Yuxi/datafoundry 开源差距、小程序集成点），15 路实施代理落实：
- **覆盖面**：direct.rs/deep_api.rs/kb_api.rs 等大文件各落实 34-60+ 条（safe/test 级全落，
  会改口径值的 scope_filter 类一律未动，需业务确认+回归重签）；DMS 源码校准 value_map 补全
  （order_status 有证据三档+正名 108=已取消/199=已删除、order_type 六值、paid_status、
  after_sales_status 九档、item_type 正名「正品」——只落有源码证据的档位）。
- **web**：App.vue/ResultPanel/format.ts 及全部面板 ~536 条；panel-utils.ts 抽共享；
  补 vue-tsc 零错误基线（dbTest/llmTest 文案兜底、Citation.folder_path 放宽 null）。
- **KB 两项插队需求**：文件夹上传按源目录层级原样建 KB 文件夹树（webkitRelativePath 逐级
  「找到或创建」，path 缓存去重，send() 加 per-file route）；预览新增 office 种类
  （doc/docx/ppt/pptx/xls/xlsx/xlsm 渲染解析后内容），renderMarkdown 补 GFM 表格渲染
  （xlsx 解析产物可直接显示）。
- **部署事故四连**（本批改动引入或触发，全部当日修复并沉淀注释防再犯）：
  ① Dockerfile 被加 BuildKit cache mount，服务器无 buildx 走 legacy builder 直接炸 → 回退单行 RUN；
  ② _build.sh 的 rsproxy sed 未锚定行首，把注释里的同款字面量也替换 → sed 加 `^` 锚定 +
  Dockerfile 注释不再出现该字面量；③ deb.debian.org 拉源列表卡死 6 分钟 → apt 切阿里云镜像；
  ④ scope_binding.customer_kind 批处理改成 Global/Via 落 NULL 但列是 NOT NULL →
  **生产启动崩溃循环**，DDL 加 `DROP NOT NULL` 迁移（读侧本就忽略该列）。
- 验证：21 测试目标全绿零警告；vue-tsc 0 错误；判官回归 76/76 通过 0 失败（1 跳过取值缺失，
  同基线）；服务器全量部署后容器 healthy 无重启循环。

## AX103（2026-08-11，KB 异步入库 + 预览票据/Range + Office 转 PDF 保真 + 列表 Yuxi 化）
针对「上传卡解析中 / 预览等很久 / Office 预览要保真 / 列表太杂乱」四项实测反馈，对照 Yuxi 源码调研改造：
- **根因与修复（上传卡死）**：upload 原在请求内同步 await ingest，46 页 PDF 带 4 页 OCR 超 50s，
  浏览器/nginx 断连后 axum drop handler → 任务腰斩、文档永卡 parsing（服务器日志 BrokenPipe 实锤）。
  ingest 拆 `prepare()`（请求内快路径：校验/去重/建行/落盘/绑目录）+ `run_job()`（spawn 后台
  parse→chunk→embed），upload/reprocess/ingest-url 三入口全部异步，UPLOAD_GATE 许可随任务持有。
- **启动自愈**（Yuxi recover_pending 同款）：启动扫 parsing/chunked/pending 超 10 分钟的文档，
  按 has_chunks 分派首入/重建链、以空间 owner 身份串行重跑。部署后自动救活已卡死的
  《操作手册-DMS市场费用报销核销》（embedded，70 块 46 页）。
- **预览提速**：新增 `POST /api/kb/doc/{id}/preview-ticket`（HMAC-SHA256 单文档 15 分钟票据，
  DMS_SECRET_KEY 既有派钥，常量时间校验）；`GET /api/kb/doc/{id}/file` 支持 ticket 鉴权 +
  `inline=1` + Range 单区间（206/416/Accept-Ranges，seek 分段读不整载入内存）。前端 iframe
  直挂直链，浏览器 PDF 查看器滚动到哪页拉哪页，不再整文件等下载。票据 15 分钟而非 120s：
  渐进阅读后半程 Range 请求会撞过期 401。
- **Office 保真预览**（Yuxi 同架构）：soffice headless 转 PDF + 磁盘缓存
  （doc_id+mtime+size 键、tmp 目录 rename 原子落缓存、per-doc 锁去重、全局 2 并发闸、
  90s 超时 kill_on_drop）；覆盖 doc/docx/ppt/pptx/xls/xlsx/xlsm（比 Yuxi 多 xlsx）；
  转换不可用统一 404 office_pdf_unavailable，前端回落解析内容渲染（AX102 的降级层保留）。
  Dockerfile 装 libreoffice-writer/calc/impress + wqy 中文字体（不装中文变方块）。
- **文档列表 Yuxi 化**（web KbPanel 净删 ~100 行）：操作全部收进单个 ⋯ 竖排菜单
  （预览/下载/移动至/元数据/生成描述/停用/重新处理/删除）；可点状态 pill 兼主操作
  （失败点它=重新处理）；整行点击开预览；面包屑+幽灵按钮工具条（筛选/刷新）；四张统计卡；
  20/50/100 客户端分页；复选批量条（批量重新处理/删除）。功能无一删减。
- **上传体验**：XHR `upload.onprogress` 行内百分比 + 进度条；秒回后进行态文档 2s 轮询
  （5 分钟上限，epoch/space 防护）；≥10 文件时队列头部聚合卡（总计/上传中/解析中/失败）。
- 验证：21 测试目标全绿零警告（新增 10+ 单测：票据签验/Range 解析/自愈 SQL/异步分派）；
  vue-tsc 0 错误；服务器全量部署后容器 healthy，自愈/票据/soffice 三项实测通过。

## AX104（2026-08-11，问数准确度三连修复 + KB 权限/部门授权/词级检索/导图多级展开）
- **问数准确度（最高优先级，实测「销售额问到营销通表」事故）**：
  ①「省份」原在 WAREHOUSE_SALES_UNSUPPORTED（历史 fail-closed 防误用 state 列），整题跌进
  ODS 推导后被 t_winc_sale_report（sale_amount/province 列名太像）截胡答错口径——按业务裁决
  「省份=省区（region）」移出未支持清单、进 Region 别名；② 客户名领头类别词（客户/经销商/
  供应商）剥离——「客户董会琴本月的销售额」整词探库必空的问题修复；③ 血缘人工种子
  （dws_off_offline_sale_dfn ← t_sales_order(+_detail)，status=accepted）补上推断作业
  name_match 连不出的真上游，derive 候选血缘加权从零变有；t_winc_sale_report 合同警告加
  「DMS 销售问题禁止用本表推导」。回归新增 B01S/B01T 两题钉死，direct.rs 三个过时钉板按
  新裁决更新，全量 78/78 通过 0 失败。
- **自动分诊精化**：KB 词表补 政策/规范/手册/指南/sop；both-hit 时「文档名词×询问词」共现
  翻 Knowledge——「市场费用的报销政策是什么」不再被指标词抢去聚合费用总额（v1 不做 hybrid
  的其余纪律不变，两个旧钉板保留）。
- **KB 入口权限可配（web）**：侧栏知识库整节按 kb_manager 显隐（不再只藏按钮留死标签）；
  设置页新增「知识库入口权限」卡片 + `POST /api/admin/settings/kb-manager-grants`
  （管理员闸/名单卫生校验/落盘热更/双空回仅管理员缺省，锚点测试钉住）。
- **部门维度授权**（Yuxi share_config 差距融合）：Grantee::Dept + kb.user_dept 映射表 +
  visible_docs!/space_acl_sql! 宏支路（内联点零改动自动获得部门语义）+ 清单端点并集 +
  授权对话框部门下拉；dept=write 的 store 内联写复核与 export 并集是已知边界（fail-closed 方向）。
- **同名文件冲突提示**：上传前按目录判定同名，队列行 ⚠ 预警不阻断（纯前端）。
- **中文词级稀疏检索第 9 路**（Yuxi BM25 混合检索差距融合）：jieba-rs（D6 破例，锁树无分词
  组件）+ kb.chunk.terms GIN + RRF 第 9 槽权重 1.0；hits>=2 门是先量后定（判据块最低 2/远域
  噪声最高 1）；存量启动回填（ advisory-lock，读库内 text 重算）；DMS_KB_TERMS=off 可关；
  kb_eval 夹具 11/14→12/14 且无回退；顺带修 answer.rs CJK 边界切片 panic。
- **知识导图多级展开**：sections 端点从顶层分桶改 heading_path 逐级建树（跨位置归并/子树
  累计/100 节点闸）；前端圆点=展收、文字=摘要卡双层分工，嵌套章节摘要卡向上找文档祖先。
- **上传与错误体验**：上传失败 502/HTML 折叠成可行动文案；状态档「该怎么处理」指引
  （pill hover 与原因行同源）+ 原因行内联「点这里处理」；问答 errMsg 对网关 HTML 页折叠。
- 验证：cargo 21 测试目标全绿零警告（1699+ 用例）；判官回归 78/78；vue-tsc 0 错误；
  服务器全量部署。

## AX105（2026-08-11，同题不同答根因三连修 + 客户名虚词剥离 + 聚合零值保真）
实测反馈「同一问题两次答案不一样 / 客户本月销售额出 NULL / 死循环」逐条定位修复：
- **根因①（客户名片段被虚词表吃字）**：`customer_name_fragment` 对 STRIP_WORDS 做全局
  replace，单字虚词「有」把「线下-潍坊程祥商贸**有**限公司」剥成「…商贸限公司」——主档探库
  必空，`customer_filtered_sales` 静默 None，整题跌进 ODS 推导。修复：虚词只从**两头**剥
  （名字在问句里恒为连续一段，中间一字不动），顺带剥「怎么样/如何」纯语气尾词。
- **根因②（推导选表非确定）**：目录合同里「DMS 销售问题禁止用 t_winc_sale_report 推导」
  是写给 LLM 的文字，管不住选表——推导池抽到它就出营销通口径。修复：
  `derive_pool_winc_guard`（纯函数）——问句没点名 WinC/营销通/经销商上报/进销存 时，
  t_winc_sale_report/t_winc_stock_report/t_winc_sale_transfer/t_winc_stock_transfer
  一律不进推导候选池；滤空照旧回落原「不可计算」卡（fail-closed 语义不变）。
- **根因③（聚合单行全 NULL 不算空）**：SUM 零命中返回的是「单行全 NULL」不是零行，
  `derive_attempt` 的空结果换表机制（两轮换候选）对聚合题永远失效，[[null,null]] 被当命中
  落地。修复：`rows.is_empty() || 全行全 NULL` 都算 Empty，触发换候选表再来一轮。
- **聚合零值保真**：sales_fact 五个 SUM 指标表达式包 `COALESCE(…,0)`——「本月没卖」的
  业务答案是 0 不是空白（KPI 卡 null 渲染为空字符串，看起来像坏了）。毛利率保持 NULL
  （无销售时比值无定义，不谎称 0%）。合同匹配门（deep_api `measure_contract_compact` /
  exemplar `compact_metric_expressions`）新旧两形都认：LLM/历史 SQL 的裸 SUM 是同一口径，
  共享 `sales_fact::legacy_contract_form` 剥形助手（不许误剥 NULLIF 的除零保护）。
- **链路验证**（本机 8100 + 公网 Doris）：「线下-潍坊程祥商贸有限公司本月销售额和销量」
  连续两次同 SQL 同结果（direct-agg · dws_off_offline_sale_dfn · storename INSTR · 本月窗），
  该客户本月确实无销售（DWS 最新数据 2026-08-11），答 0.00 而非 NULL；反问死循环题
  （菜单路径粘贴长句）出 4 个干净候选（销售表现/销售明细/业绩对比/客户资料），点候选
  一题直达 direct-agg；KB 政策题 route=knowledge 带引用流式回答；库存走中台
  ywzt_ods.scm_warehous_manage（ZP 正品口径）；小程序下单模板带最新快照+省区谓词。
- 验证：workspace 1742/1742 全绿（新增钉板：`customer_name_fragment_keeps_inner_chars`、
  `derive_pool_winc_guard_drops_report_tables_unless_asked`，9 处旧钉板按 COALESCE 新形
  同步）；20 个 SQL 金文件按新表达式重 bless。

## AX106（2026-08-11，自动模式混合查询：KB+问数并行 → AI 综合）
「报销政策是什么，本月费用花了多少」这类横跨文档与取数的问句，过去二选一必丢一半
（强文档意图翻 KB 丢数据半，both-hit 归 Data 丢政策半）。本轮把混合查询落到自动模式：
- **识别（纯函数零成本）**：`triage::hybrid_clauses` 子句级判据 —— 至少一条子句是强文档
  意图（文档名词×询问词共现，与 both-hit 翻 KB 同一份词表）且至少另有一条带问数信号
  （时间词/完整业务问句/表名/单号），两半互斥。整句共现不收（「合同客户的销售额」的
  「合同」是限定词）、单句不收、纯问数多子句不收（那是 compound 的地盘）、显式 chip
  （问数/知识库）不收 —— 存量单路裁决一字不变。钉板 5 组。
- **编排（server）**：`api_ask` / `api/ask/stream` 在分诊前先过 hybrid 判据，命中则
  `tokio::join!` 并行问数（`ask_data_run`，从 `ask_data_payload` 拆出的执行体，错误映射
  403/422 仍一份）与知识库（`kb_answer`）；一路挂退化为另一路单路答案（warn 留痕），
  两路都挂才报错。SSE 端回普通 JSON，前端 handleSync 既有通道零改动。
- **AI 综合**：`compound::hybrid_summary`（与复合汇总同一份 `insight::fast_guarded` 降级、
  同一条 `wrap_untrusted` I5 边界）：数据简报 + KB 正文（截 1200 字）→ fast LLM 2-3 句
  「先数据结论、再资料口径」；失败 None 不塌双路结果。
- **wire/前端**：AskResult 序列化后挂 `kb` 键（Answer 原样）+ 综合落 `view.insight`
  （老前端忽略多出的键，serde 兼容）；App.vue 数据面板下新增「知识库资料」卡
  （KbAnswer 复用，带角标/来源）+「AI 综合分析」面板。xcx 端暂走单路（已知边界）。
- 实测「市场费用的报销政策是什么，本月市场费用花了多少」：数据 366,097.18（direct-agg
  市场费用口径）+ 政策资料带 [^3] 引用 + 综合「本月市场费用为 366,097.18 元。根据知识库
  资料，线下市场费用报销自 2026 年起需统一通过 DMS…」一段正确。workspace 1743 全绿，
  vue-tsc 0 错误。

## AX107（2026-08-11 深夜，11 项实测反馈 swarm 落地：小程序渲染/Excel 原样/战区谓词/空深度页/导图重做/KB 替换/混合扩展）
- **小程序 ai-chat（xh-xcx 仓）**：markdown 原生块渲染（零依赖解析器 + MarkdownView 组件，
  [^n] 角标 badge 点按出来源）；数据答案卡片化（KPI 大数卡 ≥1万压缩两位小数 + 表格卡）；
  等待期进度轮播（理解→查询→核口径→生成）；混合查询 kb/insight 双卡。AGENTS.md 触摸区
  overflow:hidden 禁令遵守（零残留），uni build mp-weixin 通过。
- **Excel 原样预览（web）**：xls/xlsx/xlsm 不再 soffice→PDF（样式失真），前端 SheetJS
  （动态 import 不进首包）解析原文件自绘：cell.w 格式化文本、!merges 合并还原、!cols 列宽、
  多 sheet 页签、冻结首行、2000×200 + 120k 单元格截断保险、解析失败回落解析内容层不白屏。
  doc/ppt 的 PDF 路径一字未动。
- **小程序下单深分析谓词丢失（生产 200 行外省客户事故）**：深度页板块 SQL 谓词透传只认
  dws_off_offline_sale_dfn，小程序事实表（dws_mkt_app_place_order_dnf）整体跌进 LLM 重编
  （跨快照求和/换表/丢 region 三样全错）。修复：scoped_mini_program_where + 唯一受信拆解
  （客户结构）编译期锁定，with_sales_where 整段透传（快照日+region+权限一个不落）；
  问句点名「战区」时口径注释明示「该表无战区字段，按省区统计」（不许拿 region 冒充）。
- **空深度页**：主结果 0 行（反问卡）时照样 save_artifact 出「0 个分析板块」空壳页——
  修复为 sections 空且无明细时不产 artifact、回退主结果（反问/实体卡），账本落 failed 可续跑。
- **知识导图重做**：展收语义反转（默认只展根+一级，圆形 +/− 钮点击才展开，hover 不触发，
  展开集合按空间记忆）；横向树 pastel 分支色 + 胶囊节点 + 类型图标；无限画布（拖拽平移/
  滚轮锚点缩放 0.2–3x/适应屏幕/复位）；摘要卡/重生成/导出 PNG/SVG 全保留。
- **KB 上传与权限**：reprocess 对表格文档退役 409 甩锅文案、分派 Overwrite 影子链
  （双通道旧数据清理重建、失败保旧版）；同名上传前端文案对齐既有替换语义（精确同名，
  doc_id 不变）；全部写按钮仅 kb_manager 可见（canWrite 收口），后端 20 个写端点本就在闸内。
  本机 settings 补 pg_ro_url（问数结构采集失败的真根因）；service_url 指 8077 宿主机解析。
- **纯表头 xlsx 入库失败**：embed_service._fill_table 对 0 数据行的表一个块都不发 →
  「没有可索引的文本」。修：rows 空也发「标题+表头」块（字段清单就是模板表的内容）。
- **混合查询扩展（意图不明双查）**：triage.unclear_both_hit（kb 词×问数信号×非强文档×
  非完整业务问句，四判据全复用）→ 整句喂两路；web 两入口 + xcx 两入口全接上（xcx 错误壳
  映回小程序协议）；「报销政策是什么」单句仍纯 KB、「本月销售额」仍纯问数（钉板钉住）。
- 验证矩阵（本机 8100 实测 7 题）：库存/小程序山东/客户 0.00/纯政策/混合双路/意图不明双查/
  纯销售额全部符合预期；workspace 1749 全绿；vue-tsc 0 错；web build 通过。
