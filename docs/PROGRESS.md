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

## AX108（2026-08-12，深度检查：全量回归 + 潜伏故障三连修）
对今天全部反馈做系统复查，另揪出三个潜伏问题：
- **全量回归 79/79 全过**（454s，含判官金文件比对、权限隔离、红线闸门、图路径、复合拆解）。
- **经营日报静默全灭（自上线起）**：日报每天三轮全失败——趋势 SQL 的日期键是 DATE 类型，
  sqlx 类型检查拒绝解成 String（`String ⊄ DATE` 合法来源），try_join 首错即抛、12 路并发
  无从归因。修：keyed 外层 k 一律 CAST AS CHAR + 每路挂名。修复版首轮即生成
  《经营日报 2026-08-12》（artifact id=126，完整 HTML）。
- **图目录索引每轮必败**：AGE 对不存在的标签 CREATE INDEX 恒报 relation does not exist，
  而 MetricContract 标签由后面的 MERGE 首次创建——顺序对调（先建节点后建索引），告警消除。
- **DESCRIBE 回填告警无归因**：吞真实错误裸刷屏 → 告警带 err + SQL 前 160 字指纹。
- **巡店模板 xlsx 端到端复验**：reprocess 200 → embedded、notice 清空、问数表通道登记在册
  （upload_5b0c315a 数据源，0 行模板结构正确入册）。
- **确定性复验**：同一客户销售额题连问 3 次，route/SQL/行集字节一致。
- 遗留（已记录不挡路）：小程序真机渲染未过（无微信开发者工具）；xcx 双端点 hybrid 需
  有效 token 环境复测；导图导出 PNG/SVG 未真点；远端部署待路由器 SSH 映射。

## AX109（2026-08-12，导图「展不开」根因修复 + 图片含义 + 一键部署脚本）
- **导图节点展不开**：章节懒加载原是「单飞拒绝 + fetch 无超时」——一个文档的章节请求
  挂起后，之后所有文档节点的点击都被「请稍候」永久挡掉（用户实测「很多节点展不开」）。
  修复：新点击 abort 旧请求抢占（不是拒绝）、AbortController 20s 超时、被抢占的旧响应
  静默丢弃、空间切换时在飞请求一并中止。5180 的 web 容器直挂 web/dist，硬刷新即生效。
- **图片含义**（AX107 用户三问之二）：入库图片在 OCR 正文前加「图片含义」块（视觉模型
  一句话语义描述），导图首节点即「这张图是什么」；ImageOcr.describe 默认 None 不扰降级链。
- **KB 评测复跑**：KB07 通过；KB13（近似无命中措辞题）按题集自带记录属噪声带判据，
  不是新问题；14 题夹具阻塞（夹具上传需会话环境，与本轮改动无关）。
- **一键部署脚本** `tools/deploy_update.sh`：git archive 源码 → bput 上传（SFTP 坏走
  base64 兜底）→ 服务器 docker 构建 → `_restart_server.sh` 重启（内置健康探针）→
  刷 web 产物 → /api/health 自检。`_deploy.py` 补 DEPLOY_PORT（路由器映射非 22 端口时用）。
  用法：`DEPLOY_PW=… DEPLOY_HOST=119.39.97.141 DEPLOY_PORT=2222 bash tools/deploy_update.sh`。

## AX110（2026-08-12，准确向迭代：AI 文案数字断言对账——DF validation 相位平移）
对照 datafoundry 验收驱动闭环（protocols/data-analysis.ts 的 ANALYSIS_CLAIM_VALUE_MISMATCH），
我方 LLM 文案（AI 解读/复合汇总/混合综合/深度解读/日报点评）的数字此前零校验——模型写错数
用户无感，是信任杀手。落地 claim check：
- `insight::unmatched_claims`（纯函数）：抽取文本数字断言（千分位/小数/万/亿/%/元等单位，
  单位可隔一个空白），与素材全部数字按 1% 相对容差 + 万×1e4/亿×1e8/%×100 换算对账。
  不抽日期时间片段/列表序号/序数（第 N）/派生比率（倍·成）/散文小计数（<100 无单位、
  <10 纯计数单位）——误伤面实测收敛。钉板覆盖全部分支（含「每天上限 100 元」抓错数）。
- `fast_guarded_checked`（生成侧默认入口）：对不上 → 错数列清单精确重试一次 → 仍对不上
  → 丢弃 AI 文案（宁缺勿错，与既有网址守卫/推断守卫同一条「重试再丢」纪律）。
  接线：Reading::insight（精简解读）、insight_deep_for（深度解读，Precise 档）、
  compound::summarize（复合汇总）、compound::hybrid_summary（混合综合）四处全换。
- 实测：本月销售额 AI 解读「约为 8008.94 万元」对账通过无重试；混合查询综合照常出。
  workspace 1751 全绿。

## AX111（2026-08-12，生产部署完成 + 首轮回访双修复）
- **部署**：117.72.32.186 全量上线（git archive → docker 构建 → 重启 → web dist 直挂刷新）。
  坑：Git Bash 路径转换吃掉 /opt 参数（MSYS2_ARG_CONV_EXCL='*' 收口）；_build.sh 的 cargo
  镜像注入对换行转义敏感（sed \n 双层）；生产 8100 绑 172.17.0.1 非 127.0.0.1。
  生产实测：库存中台表 1.028 亿、山东小程序 101 行全山东、潍坊程祥本月 0.00 确定性×2、
  健康检查全绿。
- **「X客户，本月的数据」跌反问（生产回访图1）**：实体解析的尾词表没有「的数据」——
  TRAILING_INTENT 补「的数据/的销售数据/的经营数据」（长尾先于子串，防剥成「的销」），
  现落实体卡（带本月时间窗），不再反问。钉板 month_data_tails_land_on_the_entity_card。
- **导图「无法展开」根因**：拖拽 pointerdown 挂在根 svg 上，节点按压也进拖拽判定——
  点击手滑 >3px 即被 suppressClick 吞掉，表现为节点怎么点都不展。修：按压起点在
  节点/展收钮上不进入拖拽（空白处平移不变）。

## AX112（2026-08-12，生产回访第二轮：xlsx 采集根因 / 导图 OCR 标签 / 意图归一扩面 / 导图点击根因）
- **生产《具体售后场景.xlsx》问数结构采集失败（用户重试多次）**：根因不在代码——生产 PG 里
  `dms_ai_ro` 角色根本不存在（容器重建丢了它），ro 连接全军覆没 → 建表授权空转 →
  schema_sync 必败 → 撤销源。补角色（CONFIG.md 两条铁律的 DDL）+ 重处理：embedded、
  notice 清空、物理表在册。临时开 insecure_login_fallback 的操作窗口已关回。
- **导图 OCR 节点零信息**：「第 N 页（OCR）」对用户无意义——改内容含义标签
  （摘录前 16 字 + 页码后缀），钉板钉住。
- **意图不明不再直接反问（业主：不许头疼医头，让 AI 解析）**：AI 归一层的触发面从
  「不可计算」卡扩到反问卡（破坏性红线除外——那是刻意拦截）：fast 归一成标准问法 →
  安全校验（④指标族 / ⑤实体族：公司名原样保留 OR ≥4 连续共享汉字锚点，且不许引入
  新指标）→ 重跑主链 → 命中透出「已按理解为你想问：X」；失败回落澄清。配套：
  归一提示词补实体样例；ENTITY_VIEW_TAILS 收「的经营情况/经营数据/经营状况」
  （归一句式剥不回裸名则探库必空，实测抓到）。TRAILING_INTENT 收「的数据/的销售数据/
  的经营数据」（长尾先于子串防「的销」事故）。
- 实测：「潍坊程祥本月情况咋样」→ 归一为「潍坊程祥本月的经营情况」→ entity-card 命中
  带 reinterpret_note；「删除所有订单」仍直接拦（无归一）；「嗨肉」仍反问。

**AX112 补记（生产联调）**：归一扩面首轮回归 75/79 —— 实体族校验把「开票/对账不可计算卡」
也放进了重试（E05/E08 红），收窄：⑤实体族仅对反问卡开放，卡题维持④指标族原纪律；
全量回归复跑 79/79。生产实测又抓到归一 LLM 会丢「的」（输出「本月经营情况」），
ENTITY_VIEW_TAILS 补裸形三词。生产终验：口语句 → entity-card + reinterpret_note；
开票题仍 direct-doc 卡；web 已是含导图点击修复的最新构建。

## AX113（2026-08-12，客户题家族根因治理：渠道词 + 裸尾词）
生产回访「潍坊程祥商贸有限公司本月线下销售额是多少」两连不中的根因链：
- **渠道词断裂**：「线下」夹在「本月」与「销售额」之间时，客户名片段剥出「…有限公司线下」
  → 探库必空。修：渠道词（线下/线上）黏在实体名头尾时按限定剥离并进边剥循环
  （三护栏：剥完只剩渠道词本身=渠道过滤本体保留；「线下-潍坊…」带连字符是库内名称不剥；
  残余不许能被虚词表整个消化——「线下是多少」剥出「是多少」这种反剥守住）。
- **残留消化对齐**：已探明客户的名字自带渠道前缀，问句里的渠道词由实体解释不算残留。
- **裸尾词**：「今年数据」「本月情况」不带「的」——TRAILING_INTENT 收裸尾「数据/情况」
  （置于所有「的×」长尾之后，防「…的情况」被剥成「…的」）。
- 实测（本地+生产）：「…本月线下销售额是多少」direct-agg 0.00（该客户本月确实无销售）；
  「今年数据」entity-card 5 行；口语句归一成 entity-card；回归 79/79 全绿。生产已上线。

## AX114（2026-08-12，追问链上下文结合根治）
「上月呢？居然回答不出」根因链与修复：
- **碎片上下文**：会话上一轮取的是用户原句——链式追问里那句是碎片（「上月呢？」），
  改写模型拿它推不出实体。修：AskResult.resolved_question 随产物落账（追问改写/归一后的
  完整问句），`chat::last_turn` 优先取它；下一轮改写拿到的就是完整上下文。
- **反问轮断链**：上一轮是反问卡（无 SQL）时改写整体跳过——「X客户本月的数据」被反问后
  再追「上月呢」必死。修：问句带公司形实体锚点（company_span）时允许无 SQL 改写
  （提示词缺 SQL 段）；无锚点（政策/制度轮）维持跳过。
- **日期幻觉**：实测 LLM 把「今年全年」展开成 2025 年具体日期——提示词加规则
  「时间词沿用自然说法，绝不展开」。
- **「全年」词缺失**：残余把客户名探库带空——STRIP_WORDS 收「全年」（计数锁 89→90）。
- M02 多轮题按 AX104 省份裁决订正（该套不在主回归集，是本轮顺带检出的陈旧断言）。
- 实测四轮链：本月 0.00 → 上月 93,643.80 → 那今年全年呢 748,783.60 → 销量呢 331,760
  （全年销量，正确继承上一轮的全年窗）；回归 79/79、多轮 3/3、workspace 1755 全绿。生产已上线。

## AX115（2026-08-12，追问上下文三断点根治：深度轮嵌套/碎片问句/历史缺失）
生产回访「深度报告后问『本月呢』被反问」的根因链：
- **深度轮产物嵌套**：deep 落账形状是 `{"result": …}`，`sql`/`resolved_question` 都在里层——
  `last_turn` 只读外层，深度轮后追问拿不到上一轮 SQL（断链）。修：里层两档都读
  （`recent_questions` 同步）。
- **只看最近一轮**：链式追问（上月呢→那今年呢→本月呢）的实体/口径锚点在更早轮次。
  修：`PrevTurn` 加第四位「更早几轮生效问句（新→旧）」，改写提示词新增
  #对话上下文段（空 = 与旧文案逐字一致，多轮题集钉住）；`AskGate.history` 取 4 跳 1
  （紧挨着的 prev 不重复进）。CLI/xcx/deep 通道传空。
- 实测：深度报告轮 →「本月呢」→ direct-agg 出数（本地+生产 CLI 双验）；
  D01/F04 两题全量跑时一次性进程抖动，单跑复绿（非断言失败）。

## AX116（2026-08-12，追问链三连：实体锚定 / 长上下文压缩 / 清空历史）
- **实体锚定（「那他上月的退货数据呢」出全量错数）**：LLM 路径漏写客户谓词（出了全公司
  售后数 1576）。修：gather 新增实体锚定——问句名字候选（边剥指标/主题/时间/虚词）探
  客户/商品主档，唯一命中即注入绑定提示（进 value_hints 段，预算「绝不丢」），LLM 必须
  带 `customer_name LIKE`/`customer_code` 谓词。实测：红欢喜上月退货 = 0（该客户上月
  无退货记录，正确口径）。
- **长上下文压缩不丢**：追问改写的对话上下文从 3 轮 ×80 字改为 6 轮（近两轮 80 字、
  更早 40 字）——压缩长度但不丢轮次（业主裁决：可以压，不能丢）。
- **清空会话历史**：`POST /api/conv/{id}/clear`（属主闸，清消息留会话）+ 侧栏 🧹 按钮
  （hover 可见，确认后清屏并重置追问上下文）。
- workspace 1755 全绿；vue-tsc 0 错。

## AX117（2026-08-13，接手首轮：全量架构审计 + 三个 P0 止血）

前一轮（结构化意图 V2 / 结果终验 / 知识库整块证据 / 部署脚本加固，约 1 万行）交接时**未提交、
测试红**：`knowledge/answer.rs` 删掉 1200 字块截断（`BLOCK_CHARS` + `clip`）后生产码改干净了，
两个引用它的测试没跟上（lib 目标能编，test 目标炸），且顺手删掉的 `MAX_QUESTION_CHARS`
让 KB 问答这条路对超长问题**没有任何长度闸**（`/api/kb/ask` 只校验非空）。
收尾：两个失效测试改写成新纪律的钉板（整块必须原样到模型、块尾数字是合法证据）、
长度闸在 `answer` 与 `answer_stream` 两个入口都加回。

### P0-①：结构化意图合同拒 null → **每一个问句**都掉进 need-intent 反问卡
提示词第 4 条明写「没提到的槽位用空数组、**null** 或 false」，而 `IntentV1`/`TimeSlot` 的字段是
`#[serde(default, deny_unknown_fields)]` —— serde 的 `default` **只覆盖缺失字段**，显式 `null`
落到 `String`/`Vec`/`bool` 上是 `invalid type: null`，整份合同判 Invalid → 自由 SQL 与语义缓存
当轮关闭 → 全部问句 fail-closed。日志只说「JSON 不合约」。**回归实测 2/79。**
修：`parse_intent_strict` 单一漏斗里先解析成 `Value`、递归删 null 键、再按合同反序列化；
`deny_unknown_fields` 不放宽（拒的是模型编造表名/列名，不是模型写 null）。
实测「本月销售额是多少」从反问卡 → `direct-agg` / 39ms / 95,099,953.08。**回归 2/80 → 25/80。**

### P0-②：覆盖闸一票否决 → 答得出的题硬失败成 422
`CoverageReport::complete()` 把 missing/extra/conflicts/unverifiable 一起当阻断，比
`AGENT-ARCHITECTURE §9` 自己的合同更严（那里写的是「验证失败仍可展示结果，但收据必须
blocked/review」）。叠加三处封闭判据，整批题结构上不可能通过：
- `metric_proved` 写死 8 族 → 市场费用/开票金额/客单价/退款额那批一律硬失败；
- 实体槽**只**认 `ExecutionEvidence`，而 LLM 路的 evidence 恒空 → 每个带客户名/商品名的
  自由问句必挂（实测「山西省的烤肠卖给了哪些客户」：`无法证明:entity:烤肠、region:山西省`）；
- 地区/筛选值用 `folded_eq` 精确等值，而业务口径写死「行政省份 ≠ 门店业务省区」，
  正确 SQL 写的是 `province_department_name='山东省区'`、用户表面词是「山东」→ 恒假。
修：①闸门分两级 —— `blocking()`（槽位被删/模型自报歧义/结构上证不了）走 repair→fail-closed，
`needs_review()`（证不出来但没有删槽证据）放行执行、收据降 review（`receipt_blocked` 既有链路
自动接住）；②护栏：投影里连一个聚合函数都没有仍维持硬阻断，防止 fail-closed 翻成 fail-open；
③实体槽补 SQL 侧证明（名称/编码族列上的谓词绑定，含 `LIKE '%…%'` —— 系统提示词本来就要求
名称走 LIKE）；④名称型值比对剥通配符后取「相等 ∨ 互为子串」，两侧各要 ≥2 字；
**日期仍走精确等值**（放宽会让 `2026-08-1` 证明 `2026-08-10`）；⑤指标证明的第 9 族用同一条判据
自证（表面词出现在聚合投影里）。

### P0-③：容器凭据自锁（环境，非代码路径）
未配 `DMS_SECRET_KEY` 时 settings 凭据密钥由机器指纹派生（`db/crypto.rs:157` 的 host+user），
而 Docker 注入的 `HOSTNAME` **就是容器短 id**；启动路径又会拿当前钥匙把明文凭据加密写回
挂载的 settings（`db.rs:487-511` 的幂等迁移）。于是**重建一次容器 = 凭据被上一个容器 id 锁死**，
本轮实测撞上（只能翻旧日志找回那个 id 才解得开）。止血：`secrets/dms-secret.key`（.gitignore
整目录忽略）+ `serve.ps1` 注入 `DMS_SECRET_KEY`，与 DEPLOY.md 的 `$DMS_RUNTIME_ROOT/.secret_key`
同一条纪律。根因修法（机器指纹的 host 分量改成落盘随机指纹文件，`tools/settings.py`
有一份必须同步的同款实现）进优化方案，未在本轮改。

### 同批的准确性修复
- **权限 fail-open**：`ads_off_sales_cost_customer_dnf` / `dws_mkt_app_place_order_dnf` 用的是
  `CustomerKind::Codes`，而 Codes 臂在客户集合为空时**一个段都不 push**、这两张表又没有 owner
  维度兜底 → segs 空 → 不注入 → **整表可见**；两行紧邻注释却都写着「fail-closed」。改
  `RequiredCodes`（空集恒假）。两张是数仓自建表、不在 Java 对拍面，不与 `judge_scope.py` 分叉。
  钉板一条直接读 `builtin_rules()`（改回 Codes 立刻红）。
- **「最差」排序方向反了**：`rank_direction` 只认 最少/最小/最低 → 「卖得最差的 3 个商品」
  确定性地给出卖得**最好**的三个。同刀把「最低/最差」补进 `detect_top_n` 的最高级词表与
  `STRIP_WORDS`（90→92），并删掉 `ranking_limit` 里换词绕道的局部补丁。
- **「当月」缺环比/同比**：`prev_window`/`yoy_window` 只认「本月|这个月」，`rule_relative` 却认
  「当月」—— 同一个词三处判据两种口径。提 `MONTH_CUR_WORDS` 三处共用。
- **反问候选越点越长**：候选问法拿**整句**拼模板尾词（生产截图：「…420g 的信息 和 拆单标准
  的订单明细」，再点变成「…的订单明细 的订单明细」）。改成拿实体名本体拼
  （`entity_form_surface`），且剥完仍含空白（＝半句话不是名字）时一个模板候选都不给。

### 全量审计
7 路并行代码审计 + 3 路参考系统对标（DataFoundry+pi / Yuxi+SuperSonic / DMS Java 本体）
+ 2 路对抗验伪 → `docs/OPTIMIZATION-PLAN-2026-08-13.md`（121 条候选收敛成 7 批）。
三条结构性结论：①fail-open 与 fail-closed 同时装反；②声明写了判据不读它（59 表编译期目录
在读取侧静默丢弃已播种的语义声明）；③上帝文件拖慢准确性迭代本身（T8/T10 未完成）。

**验收**：workspace 1865 全绿（+5 条新钉板）、架构门禁全绿、web 33/33 + vue-tsc 0 错。

## AX118（2026-08-13，学习面根因治理 + 实体族两条自相矛盾判据）

第二轮深度研究（prime-agent 自学习机制 / Yuxi 知识库逐环 / Doris 能力面 / 本仓学习面盘点 /
混合路由断点，5 路研究 + 3 路设计 + 2 路验伪）产出 `docs/EVOLUTION-PLAN-2026-08-13.md`。
本条记录其中已落地的六项，全部是根因而非调用点补丁。

### 学习面（业主诉求：真正能自我学习、不同用户效果不同）
- **判官污染学习库**：`regression.py`/`evaluation.py` 走的就是生产 `ask` 链路 —— 每跑一趟全量
  题集就把评测问句连同**那一刻**的 SQL 写进 `meta.sql_exemplar` 与 `meta.memory`，再由 few-shot
  与经验召回喂回真实用户。跑得越勤，语料池被评测样本挤占越狠，学的还是评测当时的写法。
  修：`registry::judge_mode()` 进程级总闸（`ponytail:` 标注了「判官本来就是独立进程」这个天花板），
  三个学习写口入口各问一句；`DMSAI_JUDGE=1` 由 `main.rs` 设一次；`tools/cli.py` 给 docker exec
  形态自动注入 `-e DMSAI_JUDGE=1`。判官从此观察系统而不改变系统。
- **权限片段进共享语料**：`admin_api.rs` 的 HITL sql-edit 把 `scoped.wire()`（**注入后** SQL，
  带复核人自己的客户编码/员工 ID）存进 ds 级共享的 `meta.sql_exemplar` 并丢进复核 prompt。
  改存闸门前原文（与 LLM 路径的 `st.candidate` 同一条纪律），补源码级钉板。与 F6 同一条防线。
- **两条学习路径两种诚实度**：语料沉淀过 `worth_learning`（`st.note.is_some()` 即否决 = 口径复核
  未过不许学），十行之下的经验蒸馏只看「route 对 + 有行」。于是挂着 `caliber_note`（数字明示
  不可信）的 SQL 照样落 `meta.memory`。两条路径改为共用同一判据。
- **经验池无用户维度**：`meta.memory` 只有 `ds_id`，全公司共用一个池 —— 一个用户的修正经验直接
  进所有人的 prompt（既是污染面也是 I4 越权面），而 `login_name` 一直在 `AskCtx` 手边。加
  `login_name` 列（空串 = ds 级公有，沿用「空 = 全局」的既有约定，`ADD COLUMN IF NOT EXISTS`
  随 `ddl::migrate` 幂等生效，现网零人工迁移）；召回谓词 `(login_name = $3 OR login_name = '')`；
  去重键并入归属人；**自动蒸馏一律只写个人层**，升格公有只走人工复核。这就是 prime-agent 的
  local/global 两层作用域在本仓的对应物 —— 它那套「不同用户效果不同」靠的正是作用域，不是微调
  （顺带记：prime-agent 仓内 `reward|verifier|rollout|train` 零命中，RL 在它引用的另外两个仓）。

### 实体族两条自相矛盾的判据（回归 15 题红的根因）
- **模型拆散库内真实客户名**：「线下-广东华南食品供应链有限公司」是库里的真实客户名，模型拆成
  `entity` + `filter:渠道类型=线下` → 覆盖闸去要一个根本不存在的列的谓词（收据恒 blocked）、
  实体卡接不住、LLM 被逼着猜。此前的修法是在剥词层给「线下/线上」加特判（换个前缀照样拆错）。
  改：`intent::merge_split_entity_names` 在 grounding 入口按**原问句**判 —— 两段在原文里紧邻成
  一个词（直连或只隔一个连字符）就合回实体并撤掉那条筛选；中间隔字（「线下渠道的 X 公司」）
  不合并。不探库、零新增 IO，对没见过的前缀天然同样有效。
- **实体卡准入与自身能力矛盾**：`entity_card_compatible` 硬要求 `time.is_none()`，而实体卡本身
  就渲染时间窗（`entity.rs:538-582` 读 `time_predicate`/`time_phrase_of`）—— 正好挡住 AX111
  专门为它做的「X客户，本月的数据」。删掉该条件，其余槽位（指标/分组/比较）仍必须为空。

### 服务器空间（业主：每次部署都浪费很多空间）
部署链路此前**零保留策略**：每次 `deploy_update.sh` 新解一份 release、新建一个镜像（旧的同名
镜像立刻变悬空 `<none>`，builder 阶段带整个 cargo target，每个几 GB）、BuildKit 缓存只涨不清。
新增 `scripts/server-cleanup.sh`（默认 dry-run，`--apply` 才动手；永不触碰 kbdata/settings/
.secret_key/在用镜像与运行中容器），并接进 `deploy_update.sh` 的**健康检查通过之后**
（失败时旧镜像与旧 release 还是回滚材料）。releases 保留最近 3 个回滚位。

**验收**：workspace 1869 全绿（本轮 +9 条钉板）、web 34/34、vue-tsc 0 错、架构门禁全绿。

## AX119（2026-08-14，第 1 轮：数据权限与 DMS 角色逐条对拍）

业主把「数据权限与 DMS 的角色权限保持一致」列为最主要方向。本轮**直接读 Java 源码**逐条核，
不采信既有方案的结论 —— 结果三条全中，且方案本身有一处判断被证伪并订正。

| 表 | Java 证据 | 我们此前 | 后果 |
|---|---|---|---|
| `t_employee` | `EmployeeDao.java:35` `@DataScope(joinSql = "t_employee.employee_id in (#employeeIds)")` | **global** | 任何受限账号一句话拿到全量花名册（姓名/登录名/部门归属）；`SENSITIVE_COLS` 那 9 词只挡凭据列 |
| `t_customer_balance` | `CustomerBalanceMapper.java:36` `c.customer_code in (#customerCodes) #or c.area_manager_id in (#employeeIds)` | 只按 balance 自己的 `customer_code` | 丢 area_manager 分支 → 区域经理看不到本该可见的余额行 |
| `t_device_inspection_header` | `DeviceInspectionHeaderMapper.java:25` `h.created_by in (#employeeCodes)` | 写成 `manager_code`（错列） | owner 段恒不命中，巡检单只按客户集合可见 |

**订正方案的一处判断**：`t_customer_balance` 不能写成 `owner_col = area_manager_id` —— 那列
**不在 balance 表上**，是 XML 里 `LEFT JOIN t_customer c ON b.customer_code = c.customer_code`
带进来的。只能登记成 via 借 `t_customer` 的档案（它本身就是 `customer_code IN codes OR
area_manager_id IN ids`），EXISTS 半连接与 Java 逐字等价。

**已知口径相互作用（显式记账，待业务裁决）**：`t_employee` 转 scoped 后，`ops_caliber` 里
「巡店人是三方/副总则不计入」的排除子查询也会被注入员工过滤（注入是递归的）→ 受限身份看
运营看板时**少排除、数字偏高**。两害相权先堵确定的越权（花名册泄漏），并留钉板
`ops_caliber_notes_employee_scope` 守着这段说明不被静默删掉。彻底解法两条都需业务点头：
①口径装载期把三方/副总排除名单物化成常量清单；②注入器支持「口径子查询不吃行权限」。

**判据整改**：计数钉板 `thirty_nine_tables_by_kind` 改名 `builtin_table_counts_by_kind`
（函数名里写数字，39→41 那次就已腐烂成谎话），并补三条 Java 对拍断言（t_employee 必须 scoped
且只按 employee_id、余额表必须 via t_customer、巡检单 owner 必须 created_by）。
`empty_segments_allows_today` 的被测表换成 `t_customer_device_ledger`（余额表已不属那一族）。

**验收**：workspace 1879 全绿、policy 26 条全绿、架构门禁 13/13。
`judge_scope.py` 本轮**未能构成结论** —— 现网缺 visitor/shop_contact 可用账号，判官按设计
拒绝下结论（exit 2 = 门没开，不是对拍失败）。这是环境缺口，与本轮改动无关；补上账号后需补跑。

## AX120（2026-08-14，第 2–4 轮：自我学习三件套 —— 账本 / 习惯 / 失败读回）

业主的「自我学习、自我进化、每个用户不一样」在本仓落地时缺的从来不是「写」，是**读回与撤回**。
三轮各补一面，深度参考 prime-agent 的 refinement（两段式提案 + 确定性 apply + 机械回滚）。

**第 2 轮 · 学到的东西撤不回来**（`registry/learn.rs`）。四个学习写口
（`sql_exemplar` / `memory` / `pitfall` 及教训候选）此前只写不留前值：学错一条＝人工去库里翻。
现在每次写落一条 `meta.learn_event`（`batch_id`/`actor`/`before`/`after`/`trace_id`），
`rollback_batch` 是纯机械倒序重放，不再调模型。回滚 SQL 的表名只能取自 `LEDGERED_TABLES`
（`&'static str`，满足 `sql_interpolation_is_allowlisted`）；接漏写口由钉板
`learn_writes_are_all_ledgered` 抓。同轮加 `judge_mode()`：判官跑回归时学习写口整体停手 ——
**判官的问句不是用户的问句**，此前它们等量齐观地污染语料池。

**第 3 轮 · 学到的经验人人共享**（`registry/user_pref.rs` + `memory` 的 `login_name` 作用域）。
个性化拆成两半：经验按人隔离（memory 加 login 作用域），习惯按人生效（`user_pref`）。
习惯**不新建学习表** —— 直接从 `meta.query_log` 现算（谁、问了什么、出没出数），永远新鲜，
且没有写入就没有回滚/复核/TTL 这三个面。三条硬约束写死在判据里：只在用户没明说时用、
只进 prompt 参考段不改 SQL、同一习惯 <3 次视为噪声。候选词是固定字面量表，
不从问句自由抽词（`only_fixed_candidates_can_become_habits` 钉住 —— 否则客户名会变成「习惯」）。

**第 4 轮 · 同一个坑反复踩、系统一句话不说**（`registry/failure.rs`）。`meta.failure_log`
全仓零 `SELECT`：写了没人读。后果两层 —— 用户感知是「同一个问法反复失败，系统学不会」；
每次失败照样起一次 fast LLM 复盘（自动日报一天 7 次重复失败 = 7 次全量复盘，产出同一条候选教训，
去重发生在写口，白烧的是模型调用）。现在 `failure_streak` 按 **同 kind + 错误前缀 60 字**
数连续次数（不用全等：错误尾部的行号/耗时/连接 id 每次都不同，全等会让每次都算「新错」，
判据恒返 1 等于没有这个功能），第 1 次只记日志，第 2 次起才惊动模型。

**同轮抓出的 I4 泄漏面**：复盘素材传的是 `scoped.wire()` —— **注入后**的 SQL，行级权限条件
（客户编码集合、员工 id 集合）会随教训进入 ds 级共享语料。两处落账（`exec-error` / `zero-rows`）
都改成闸门前候选 `st.candidate`。判据第一版只切了 `exec-error` 那一段，当场漏掉 `zero-rows`；
改成扫**全部**落账点后立刻抓出（并把生产段与测试段切开 —— `include_str!` 会把判据自己的
断言文案也扫进去，那里面就写着 `scoped.wire()`）。

**验收**：workspace 1886 全绿、`drift.rs` 3/3（`failure.rs` 的 `ERR_CLASS_CHARS` 已按
「编译期常量、无外部入口」报备进插值白名单）。

## AX121（2026-08-14，第 5 轮：18 个 Java @DataScope 逐个扫，补 5 张缺档案的表）

第 1 轮只核了三张**已登记但列绑定错**的表。本轮换个问法：**Java 一共给几张表挂了
`@DataScope`，我们是不是每一张都有档案？** 全仓 `grep joinSql` 出 18 个 mapper，逐个回到
XML 确认别名指向哪张物理表 —— 抓出五张**我们一条档案都没有**的。

缺档案不是「放行」，是 `UnregisteredTable` **整句拒**。所以症状是反的：DMS 页面里看得见的
单据，问数一律回「未在权限档案登记」。方向是答少了，但同样是与 DMS 不一致。

| 表 | Java 出处 | 档案形态 |
|---|---|---|
| `t_application_list_header` | `ApplicationListHeaderMapper.java:21`（别名 `invoice`，见同名 XML:47） | scoped `customer_code` + `manager`(Ids) |
| `t_application_list_detail` | XML:51 `ON invoice.invoice_code = invoiceD.invoice_code` | via 头表 |
| `t_device_transfer_order` | `DeviceTransferOrderMapper.java:20` | scoped `out_customer_code`，无 owner 段 |
| `t_statement_apply` | `StatementApplicationMapper.java:23` | scoped `customer_code` + `created_by`(**Codes**，Java 用 `#employeeCodes`) |
| `t_device_requisition` | `DeviceRequisitionMapper.xml:201` `INNER JOIN t_customer tc` | **via t_customer** |

两个易踩的坑，都靠回读 XML/实体才没写错：
- `ApplicationListHeaderDo` 的 `@TableName` 是 `t_application_list_header`，**不是**
  `t_invoice_apply_header`。后者（旧开票页）Java 确实没有注解 —— 由 service 把数据范围员工写进
  `p.managers`，XML 只过滤 `invoice.manager`（`InvoiceApplyHeaderMapper.xml:37-39`），
  所以它保持 `owner_only("manager")`，此前那条注释是对的。
- `StatementApplicationDO` 的 `@TableName` 是 **`t_statement_apply`**，不是类名暗示的
  `t_statement_application`。

**解除一条退役**：`t_device_requisition` 此前在 `RETIRED` 里，理由写的是「两个设备专职角色
有全量例外，静态 Binding 携带不了该证明」。这个权衡站不住 —— 退役期间它是整句拒，
**所有**角色都看不到，包括那两个本该看全量的。现在登记成 via t_customer：其余角色拿到与
Java 逐字等价的范围，那两个角色仍偏严（精确单号通道另有
`Scope::device_unrestricted_by_role` 的布尔证明，`business_lookup.rs:286`）。
设备两张明细（receive_item / delivery_item）**仍拒**：头表现在是 via，而 via 的头必须是
Scoped（`via_head_without_scoped_rule_is_rejected`），链式 via 表达不了。

**三条查证后确认「不动」的**（记下来，免得下一轮又当 bug 修）：
1. `t_activity_main` 的 `@DataScope` 在 Java 里是**注释掉的**（`ActivityMainMapper.java:29-31`），
   即 DMS 活动列表不做行过滤。我们保持 scoped（`customer_code` + `created_id`）—— 比 DMS 严，
   方向安全；活动表带客户编码，放全量是确定的越权面。
2. `DefaultEmployee.getEmployeeCodesByCurrentUser():526` 判的是 `employeeCodeList.contains("-1")`
   而不是 `subordinateCodes.contains("-1")`（Java 侧笔误），结果是把字面量 `"-1"` 混进登录名集合 ——
   匹配不到任何行，行为等价。我们按语义写（判 sub 段），**不复刻这个笔误**。
3. `CustomerDataScopeStrategy` 的第三个放行条件 `UserTypeEnum.ADMIN_SYSTEM` 我们没有对应项：
   全仓只有 `SmartJobExecutor.java:103`（定时任务）会设成 ADMIN_SYSTEM，登录路径一律
   `ADMIN_EMPLOYEE`。**不能**为了「对齐」给它加一条放行 —— 那是凭空多一个越权入口。

注入器实际读的是 `getCustomerCodesByCurrentUser`（不是同类里那个会短路的 `getCustomerCodes`），
段序 base → common → 102 组 → 103 团队 → 101 下属客户，与 `scope.rs::customer_codes` 逐段同序。

**验收**：workspace 1888 全绿（新增行为面判据
`java_scoped_tables_actually_inject_their_condition` 真注入五张表并比对 Java 条件片段）、
架构门禁 16/16、档案计数钉板 41→46（scoped 18→21 / via 8→10 / global 15）。
`judge_scope.py` 仍缺 visitor/shop_contact 账号，无法构成结论（与第 1 轮同一环境缺口）。

## AX122（2026-08-14，第 6 轮：混合问句从「一数一知」放到「N 数一知」+ 纯资料问句真的去查知识库）

业主截图里那张「先问清再查」的澄清卡，根因不是意图识别不准 —— **是执行侧的基数限制**。
`hybrid::pair` 要求 typed 子任务**恰好两条**（一数一知），于是

> 「本月销售额和毛利各多少？另外退货政策是怎么规定的」

这种再普通不过的问法（2 数 + 1 知）被判为「归属无法唯一证明」，直接出卡。而它根本不需要新载体：
复合问句本来就走 `AskResult::compound(subs)`，wire 与前端零改动。

**改成 `split`**：N 条问数 + **恰好 1 条**资料。多条问数彼此也并行（各打一次库，串行是白等），
折进既有 compound 容器；子问题名用**投影后的子问句**而不是父问句（父问句在每个 sub 上重复一遍，
用户分不清哪块是哪块）。折的时候只在 >1 时套容器 —— 单条套壳会让直出表格的问句多一层子结果，
前端渲染与收据跟着变形。`into_ask_result` 里再补一道：问数半**已经是** compound 时把它的 subs
抬上来，不套第二层（嵌套 compound 前端只渲染第一层，第二层表格就这么消失 —— AX115 同一个坑）。

**资料半仍限 1 条**，这是载体上限不是偷懒：`Answer` 的角标 = `citations` 下标 + 1，合并两份答案
要整体重编号，编错就是「点开引用跳到别的原文」，比澄清卡更伤。所以澄清文案改成说清**是几条、
卡在哪**（`cardinality_note`）：「我识别到 2 个资料子任务；一次混合回答只能带 1 个……请拆成 2 次问」。
此前那句笼统的「请说得更具体」只会让用户换个说法再撞一次同一堵墙。

**同一条收口的另一半**：`ask_prepared` 里 `IntentRoute::Knowledge` 是**无条件出澄清卡**的 ——
可 `AskDeps` 早就带着 KB 臂了。于是纯资料问句在 CLI/判官链路永远得不到答案，而 HTTP 侧早接了知识库：
又是一处「两条链路对同一问句行为相反」（Hybrid 那次收口只修了一半）。现在走
`hybrid::knowledge_only`，KB 臂缺席（深度报告子问、定时任务）或知识库这一路失败才澄清 ——
失败也返 `None` 而不是伪造空答案。顺带把该路的 `elapsed_ms` 从恒 0 改成真实用时、
`route` 从 `compound` 改成 `knowledge`（没有问数半的那次就不是 compound，收据不该写假话）。

**删掉第三套配对逻辑**：`server/src/main.rs` 的 `hybrid_pair` / `hybrid_cardinality_clarification`
（47 行）—— 编排上一轮搬进 agent 后没人删，只剩它自己的测试在用，而规则已经与 agent 侧矛盾
（它连「2 数 1 知」都拒）。换成钉板 `server_keeps_no_second_hybrid_pairing`：源码里再出现
`fn hybrid_pair` 就红。

**验收**：workspace 1889 全绿（新增 `split_takes_many_data_but_exactly_one_knowledge`、
`cardinality_note_says_which_side_overflowed`）、架构门禁 16/16。

## AX123（2026-08-14，第 7 轮：结果呈现六条，全部「用户可见」档）

按 `docs/UI-POLISH-PLAN.md` 的 result-presentation 清单收了六条，选的都是**用户每次都会撞到**的：

1. **AI 综合分析渲染两遍**。混合结果同时命中 `t.result?.kb && view.insight` 与
   `subs?.length && compoundAnalysis(...)`，而后者返回的**正是** `view.insight` —— 同一段文字、
   同一个标题，上下紧贴出两块。中间那块 `t.page?.insight` 改成 `v-else-if` 把三块串成一条链
   （深度页恒有 `t.page`、混合恒无，互斥）。第 6 轮之后混合能带多条问数子问，`subs` 非空从
   偶发变成常态 —— 不修的话这条会**更频繁**地出现。
2. **流式不跟随滚动**。delta 分支只改 `aiTurn.result`，一次都不滚；正文从气泡顶往下长，
   两屏之后全在视口下方 —— 10-20 秒的生成用户看到的是静止画面，流式最主要的感知收益整个白丢。
   加 `followStream`：**不复用** `scrollDown`（它的 `behavior:'smooth'` 会和每帧新内容打架），
   直接赋 `scrollTop`；120px 阈值保证用户手动上翻后不被拽回；120ms 节流。
3. **知识库流式期间标题抖动**。`presentation()` 取第一个 heading 当标题、第一个非列表段落当结论，
   而这两样都是逐 token 到的：标题从「直」「直接」跳到「知识库回答」，半截的 `-` 不匹配列表排除
   规则会被当成结论塞进蓝框再弹掉。生成中冻结拆分（正文渲染一字不动，渐进排版照旧）。
4. **单张 KPI 卡吃满整行**。`repeat(auto-fit, minmax(180px,1fr))` 的空轨道会塌，于是
   「本月销售额趋势」这种单 KPI + 折线图的结果里，28px 的数字左挂在 800px 空白中。
   补 `.kpi-row:not(.solo)` 一条上限 300px + 左对齐；`.solo` 大卡与两处窄屏覆写都在其后，仍胜出。
5. **sticky 首列 hover 穿帮**。行号列/首列 hover 用 `--primary-light`（8% 透明）铺底，
   横滚时压在下面的单元格文字直接透出来。改 `color-mix(in srgb, var(--primary) 8%, var(--bg-card))`
   —— 同文件里本来就在用 color-mix，不是新技术。
6. **删两条零消费者样式**（`.scope-note` / `.tbl-foot`）。权限回显早已改渲染进
   `.foundation-body`、行数脚注改成 `.row-count`；留着会让下一个人以为存在两条并行呈现路径。

**验收**：`npm run build` 通过，web 判据 41 → **47**（新增五条钉板：v-else-if 链、
两处 color-mix、KPI 上限、死样式已删、流式冻结与跟随阈值）。

## AX124（2026-08-14，第 8 轮：复合答案不再一条失败全轮 422 + 明细题不再被降级护栏误伤）

两条都出自 `accuracy-next` 清单，都是**用户拿到 422** 那一档。

**① typed 复合的三件事**（`ask.rs`）。此前 `one(question.clone()).await?` —— 一条子问失败，
用户连另一条**已经查出来**的结果都看不到。而同一个仓里 `compound::try_compound` 早就不是这么做的：
并行 + 失败点名 + 全挂才上抛。typed 这条是唯一的例外，抄过来即可（`missing_note` 提成
`pub(crate)` 复用，措辞里写死「不是 0、也不是没有数据」—— 缺席的面板最容易被读成「那一项是零」）。
顺带把串行改并行：每个子问各打一次库，串行是白等（`scope` 仍只算一次，I4 不变）。
再顺带填上容器的 `intent_summary` —— 前端「问题理解与结果依据」对复合答案此前**整块空白**，
而合同本来就在手上。**`trust` 仍留 None 且判据钉住不许造**：凭证要有 SQL 指纹、来源、执行方式，
而容器一句 SQL 都没跑，编一份就是假收据（子结果各自带着自己的）。

**② 降级护栏只在「用户要了指标」时开火**（`intent.rs`）。护栏原文是
`!report.unverifiable.is_empty() && !projections_have_aggregate(..)` → `conflicts` → blocking。
本意是「模型压根没算用户要的指标」，可它没判**用户有没有要指标**：于是
「本月线下渠道的订单明细」这类 metrics 为空、投影是列不是聚合的明细题，被打成硬阻断，
用户拿到 422「暂时无法完成本次问数」。这是 AX117 两级闸的副作用，当时的方案里没记。
加前置合取项 `!intent.metrics.is_empty()`；明细题的形状另有 `detail_shape_proved` 兜着，
不靠这条护栏。判据两条：要了指标 + 无聚合仍硬阻断（护栏不许松）；明细题 `blocking()==false`。

**验收**：workspace 1891 全绿（新增 `typed_compound_degrades_instead_of_failing_the_round`、
`no_aggregate_guardrail_only_fires_when_a_metric_was_asked_for`）。

## AX125（2026-08-14，第 9 轮：Doris 执行计划别丢 + 问句切片用错向量空间）

**① 全分区扫描判成可 repair 的缺时间谓词**（`connector::source::scan_verdict`）。
语法合法但要扫全表的查询此前一路跑到 `EXEC_TIMEOUT` 才失败 —— 用户等满半分钟拿到一句「超时」，
而执行计划**这一次往返已经付过了**（首轮 EXPLAIN），里面白纸黑字写着 `partitions=1358/1358`。
现在把计划文本喂给纯函数判据，判词走与「数据库明确报错」同一个 `Some` 口子：
`run.rs` 的 repair 轮零改动接住，**不** fail-closed（判错了只多花一次改写，不该拦住正确的 SQL）。
三条防误伤写死在判据里：只认「已扫 == 总数」（扫子集说明分区裁剪生效了，一个字不说）、
总分区数 <8 不判（小表全扫正常）、判词进回炉不直接拒。
落点选 `source.rs` 而不是新开文件：`explain` 的 `Option<String>` 语义就定义在那几行上方
（D3 同族），且 connector 的 `.rs` 预算已顶格、`mysql.rs` 已 1664 行。

**② 问句切片拿 passage 向量去比 query 标定的阈值**（`gather.rs` + `connector::embed`）。
元素卡（指标/维度/码值）的召回阈值 STRICT=0.35 / LOOSE=0.5 / DS_MAX_DIST **全是拿 query 向量
标定的**，而切片走的是 `embed_passages` —— 两个空间的距离分布不同，口语化问法整体召回漂移；
附带一个更隐蔽的面：passage 熔断槽是知识库入库在用的，**一次入库失败会顺手掐掉 5 分钟的切片召回**。
改 `EmbedMode::Query` 只是一行；同刀把 `embed_passages` / `embed_queries` 两个同形包装删掉，
只留 `embed_batch(texts, mode)`——「随手挑那个批量的」正是这个错的成因，少两个函数就犯不出来
（五个调用点现在必须显式写模式）。`gather.rs` 的存在性判据同步从 `embed_passages` 改成
`EmbedMode::Query`，钉的是**模式**而不是函数名。

**验收**：workspace 1892 全绿（新增 `scan_verdict_only_fires_on_a_real_full_partition_scan`）。

## AX126（2026-08-14，第 10 轮：口径卡缺席不许还显示 verified）

「答错了还很自信」在本仓有**唯一一条结构性来源**：PG 抖一下 → 指标召回失败 → 指标卡缺席 →
LLM 拿不到销售额的口径表达式 / 时间列 / 去重键 → 数字按错口径算出来 —— 而收据照样 verified/high。
这件事此前**只写进日志**：`gather` 里 12 份手抄的 `map_err(|e| warn!(...)).unwrap_or_default()`，
只有翻日志的人知道，用户和收据都不知道。

**三步接线**：
1. 12 份手抄收敛成一个 `degrade(r, what, &mut degraded)` —— `what` 同时是日志文案与降级项名
   （同源，改一处两处一起变；抄漏一处就是一路静默降级）。
2. `PromptCtx` 加 `degraded: Vec<&'static str>`，`run_llm` 用闭包在**三条返回路径**上统一挂标注
   （单条 / SC 多数派 / SC 无多数派 —— 少接一条就是一次自信的错答）。
3. 标注走既有的 `caliber_note` 通道：`attach_trust` 的 risk 判据本来就读它，trust 自动降 review。
   **不新造字段、不新造状态**。

**只有口径类算数**（`CALIBER_CARDS` = 指标卡 + 维度卡）。术语/关联图/经验缺席只是素材少、
不改口径，拿它们去降 trust 会让「结果不可信」这条警告贬值 —— 用久了就没人看了。

**顺手清掉两个死件**：`ContextSummary.trimmed`（`BudgetReport.notes` 恒 `vec![]`）与
`summary_used`（恒 false，历史摘要装配点在 server 侧）。于是审计面板上那两行**永远不出现** ——
死件比没有更糟：读的人以为「没裁 = 一切正常」。那一格现在装真会发生的事（降级项），
`SqlAuditPanel.vue` 同步改成 `⚠️ 召回降级 N 项` 并列出每一条；`TrimNote` 结构体整体删除。

**判据整改**：`gather_warns_on_every_recall_degradation` 从「`unwrap_or_default` 条数 ==
`warn!` 条数」改成「≥9 处 `degrade(` 且**不许再出现手抄的 `map_err(|e| tracing::warn!`」——
禁的是形态而不是 `unwrap_or_default()` 本身（后者在 `Option` 链上是正常写法，一刀切会把对的代码判红）。
新增 `only_caliber_card_gaps_downgrade_the_answer`（非口径类不降级 / 两张都缺点全 / 合并不覆盖既有标注）
与重写的 `context_summary_json_shape_is_stable`（死件不许回来）。

**验收**：workspace 1893 全绿、架构门禁 16/16、web 47 全绿 + `npm run build` 通过。

## AX127（2026-08-14，第 11 轮：学习账本从「摆设」变成真能撤 + 一次 79 题实测回归）

第 2 轮建的账本有个致命细节没接：**四个写口写进去的 batch_id 全是空串**。后果两头都糟 ——
管理员打开学习台账**永远是空列表**（`recent_batches` 的谓词是 `batch_id <> ''`）；
反向更危险：POST 一次 rollback 传空串，`WHERE batch_id = ''` 匹配到所有没带批次号的历史事件，
**一把撤光全部学习**。

**批次粒度钉死为三族**（不是会话）：
- 一轮问答 → `trace_id`（经验蒸馏、语料沉淀、失败复盘产出的教训都归这一轮）
- 一次复核批 → `review-<秒>`（std 时间戳，零新增依赖）
- 人工编辑 → `sql-edit`（管理员改的语料自成一族，要能整族撤回）

四个写口（`save_with_context` / `save_lesson_candidate` / `set_lesson_status` / `save_memory`）
各加一个 `who: (&str, &str)` = (批次号, 操作者)，五个调用点各传各的。
`rollback_batch` 首行 `anyhow::ensure!(!batch_id.trim().is_empty(), ...)`，
`log_event` 遇到空批次号 warn 一条「它将无法回滚」（不拒绝写入 —— 学习不许被账本拖垮）。

**顺手补上 `meta.memory.conv_id`**：此前 `save_memory` 把 `conv_id` 位置当批次号传给账本，
而调用方给的是 `""` —— 两个坑叠在一起（账本撤不回来 + 会话列恒空，追问链看不到这条经验属于哪次会话）。
现在两者分开各给各的。

**把幻影判据写出来**：`learn.rs` 文件头白纸黑字写着「接漏了由 `learn_writes_are_all_ledgered`
钉板抓」，而这个测试**全仓零命中** —— 下一个人加第五个写口时不会有任何东西变红。现在它真的存在：
扫 `exemplar.rs`/`memory.rs`，四类学习状态写口的**前后各 25 行**内必须有 `learn::log_event`，
且账本调用不许写字面量空批次号。窗口双向是第一次跑就抓出来的 —— `set_lesson_status` 的落账在
UPDATE **之前**（要先读前值才撤得回来），只往后看会假红。

---

### 本轮实测回归（80 题，容器内 08-14 01:14 构建的二进制 = 第 1–5 轮的状态）

**59 通过 / 20 失败 / 1 跳过，耗时 2956s**。可见的九条失败聚成三族：

| 族 | 题 | 现象 |
|---|---|---|
| 60s 速度门禁超时 | E16 / E18 / OPS01 / OPS02 / OPS04 | 五题都是**该拒答**的合同题（「不可由默认销售事实准确回答」），拒答本身跑了 >60s |
| 图路由掉线 | F01 / F05 / F06 | `route=direct-doc ≠ graph`，且 F05/F06 行数 0 |
| 进程非 0 退出 | F04 | stderr 尾部是 `t_master_shop` 的 shop_code 查询 |

三族都还没修 —— 第 12 轮起按这份实测清单来，不再从计划文档里挑题目。
（注：第 6–11 轮的改动**不在**这次回归所测的二进制里，下次跑前要先重建容器。）

## AX128（2026-08-14，第 12 轮：按回归实测修 —— 图路由三题 + 权限查询打爆 MySQL）

不再从计划文档挑题，直接修 AX127 那次 80 题回归里可见的失败。

### ① F01/F05/F06：图路由被自己的合同挡在门外

CLI 复现给出决定性证据（`steps: [{"stage":"graph","kind":"skip"}]`）：

```text
问：买过烤肠的客户
合同：filters=[FilterSlot{name:"商品名称", value_surface:"烤肠"}]
```

`graph_intent_compatible` 要求 `filters.is_empty() && regions.is_empty()` —— 而这条 filter
**正是关系自己的参数**。于是 graph 跳过、direct-doc 接走，答成 200 行订单聚合表
（用户问的是「哪些客户」，拿到的是一张宽表）。省份那两题更亏：`resolved_buyers` 的
IN_PROVINCE 通路是**专门为它们写的**（连 `cypher_carries_every_filter` 判据都写好了），
却因为 `regions.is_empty()` 一次都没被走到过。

判据改成「**关系本身已经表达了它**」：`filters`/`regions` 的每个槽面必须被关系的实体参数含住
（`arg.contains(surface)`，**只判一个方向** —— 反过来判会把「非烤肠」这种取反槽面也算成被含住）。
时间 / 同环比 / 明细 / 分组仍必须全空：那四类 Cypher 真的表达不了。
放宽的失败方向仍是回落：万一含住了却没装进 Cypher，`into_slots` 的覆盖率判据会在出手前拒绝装配。

这也顺带治了「同题不同答」的一个来源 —— 判据此前依赖 fast 模型**恰好没往 filters 里塞东西**，
而 F02（共购）与 F01（买过）问的是同一个商品，一个进图一个没进。

### ② F04：权限查询把生产 MySQL 打到 3024

唯一一条**进程非 0 退出**的题。日志尾部：

```text
sql="SELECT DISTINCT shop_code FROM t_master_shop WHERE customer_code IN (…)"
error returned from database: 3024 (HY000)   # max_statement_time exceeded
```

`city_manager` 的客户集合上千条，一次性 IN 进 `t_master_shop` 撞语句超时 —— 而这是**算权限**
的一步，失败就是整轮问答失败。修法照抄 DMS Java 自己的手法（`getEmployeeCodesByIds` 的
`batchSize = 800`）：`fetch_str_in` / `fetch_str_by_str_in` 两个 IN 辅助函数按 800 分批。
判据 `in_batching_splits_but_never_truncates` 钉三件事：必须分批、**不许出现 LIMIT**
（截断权限集合是 fail-open）、分批后要跨批去重（`DISTINCT` 只在批内成立）。

**顺带记一笔现象**（不是本轮改动）：从开发机连生产 DMS MySQL，每条静态语句恒 ~1125ms ——
这是跨公网 RTT，不是查询本身慢；一次受限身份的 scope 计算要 10+ 次串行往返 ≈ 11s。
生产部署与库同机房，这一项不成立，故**不按它优化**，只记在这里以免下次误判。

**验收**：workspace 1896 全绿（新增 `relations_own_argument_is_not_an_extra_constraint`、
`in_batching_splits_but_never_truncates`）。

## AX129（2026-08-14，第 13 轮：该拒答的题不必先烧 37 秒 —— 主题门前置，并实机验收第 12 轮）

**测出来的账**（同一道题，改前 / 改后）：

```text
本月线下渠道客户分类的销售额
改前：graph=skip, direct-agg=miss, direct-doc=miss(6.7s), llm=hit(37.5s) → route=no-topic  44.4s
改后：graph=skip, direct-agg=miss, direct-doc=miss,       llm=skip       → route=no-topic   8.5s
```

**答案一个字没变**，路上白烧的是：一次 LLM 生成 + 一次执行 + 覆盖闸降级 + SC 再采样一遍。
根因是那道「主题未接入」判据挂在**执行之后**（要 `row_count == 0` 才判），
于是「这个主题压根没接入」这件在进 LLM 之前就已知的事，非要等模型编完一版 SQL 才说。

现在在 llm 成员**之前**加同一道门，判据与命中后那道**逐字复用同一对函数**
（`out_of_scope_topic` + `topic_covered`，只少一个 `row_count == 0` —— 那时还没执行）。
两条独立证据同时成立才关门：确定性成员全 miss ＋ 残留主题在注册表三路召回/值域/维度探针里
一路都不命中。`topic_covered` 全程**失败开放**（任一路读挂了都当有覆盖）：换文案是补救路径，
它自己挂了不许把一次本可成立的回答换成另一副面孔。判据
`topic_gate_runs_before_the_llm_member` 钉住「门在执行之前」且「不许出现第二份实现」。

### 第 12 轮的实机验收（重建容器后）

```text
买过烤肠的客户       → route=graph rows=50   （改前 direct-doc 200 行订单宽表）
湖南省买过烤肠的客户 → route=graph rows=50   sql=[AGE 图查询] BuyersOfGoods("湖南省烤肠")
                       前 6 行：长沙鸣望 / 喜晨食品 / 湖南宁友 / 长沙红欢喜 / 长沙鼎坤 / 长沙吉鲜岛（衡阳仓）
```

省份**真的进了 Cypher**（全是湖南客户，不是全国名单）—— 那条 IN_PROVINCE 通路写好之后
第一次被走到。

### 记一笔：回归的 60s 门禁量的是**进程**，不是回答

同题实测 `agent_ms=8547` 而 wall=57s —— 差的 48s 全是 CLI 每次启动都重跑一遍
schema 同步（114 表 3099 列）+ 语义种子 + 数仓目录校验。80 题 × 48s ≈ 64 分钟纯启动开销，
也是 AX127 那五道「超时」题的真正死因（它们并不慢，是启动费把它们推过了 60s）。
这是**跑法**的问题不是产品的问题，下一轮单独治。

**验收**：workspace 1897 全绿、架构门禁 16/16。

## AX130（2026-08-14，第 14 轮：把「同题不同答」的最后一段黑箱照亮）

第 13 轮把图准入改成只看问句之后，F03 仍在两次运行里给出两种路由（`graph` / `direct-doc`）。
排查花了半小时，而结论是：**`steps` 只写一个 `skip`，六个合取项挂了哪一个无从得知**。

三步收口：

**① 准入判据脱离合同**（第 13 轮做的一半，这里说清）。`graph_intent_compatible(intent)`
读的是 fast 模型产出的 `IntentV1` —— 而实测里同一个问句的合同时而 `Ready` 时而
`IntentAttempt(Invalid)`（日志：`结构化意图 JSON 不合约 → 关闭自由查询路径`）。
确定性路径的准入挂在非确定产物上，本身就是「同题不同答」的制造机。
现在只看问句：`nl::time::time_predicate` 判时间窗、`COMPARISON_WORDS` 判同环比、
维度切分仍由既有黑名单管。判错的方向仍是回落（`into_slots` 的覆盖率判据在装配前拒绝）。

**② 图就绪标记读失败重来一次**（`connector::graph::adopt_if_current`）。CLI 是短生命周期进程，
靠 PG 里的持久化标记接管图就绪状态；这条 AGE 读**抖一下**就等于「本进程没有图」。
现在失败重试一次，两次都失败升 `warn`（原来是 `debug` —— 等于把一次路由漂移埋进最低日志级别）。
标记与目标不符仍直接回落、不重试（重试也不会变）。

**③ 六个不接理由各有名字**（`GraphAnswerer::skip_reason`）。`accept` 变成
「问 `skip_reason`，有理由就 `info!` 一行再回 false」：
`no-unrestricted-proof` / `source-not-warehouse` / `graph-not-ready` / `unverified-dimension` /
`time-or-comparison-in-question` / `not-a-relation-question`。
判据钉住六个字面量都在 —— 「为什么这题没走图」从此是一行日志，不是一次半小时的排查。

**实测**：同一问句连跑 6 次全部 `route=graph`；F0 组回归连跑两轮 **6/6 通过**
（改前同一批题在两次运行里给出两种结果）。

**验收**：workspace 1899 全绿。

## AX131（2026-08-14，第 15 轮：图例色块看不见 + 主色上的白字）

`visual-system` 清单收尾两条（其余五条前几轮已做，这次一并在计划里打上标记）。

**① BiChart 单色阶浅端看不见**。6 类以上走滚动图例、不画扇区标签 —— 色块是名字与扇区之间
**唯一**的映射，而原来最浅两阶 `#aeb6f2` / `#d1d6f8` 对白卡只有 1.95:1 / 1.43:1。
用户看到的是「有名字、找不到对应扇区」。两条色阶换成等对比步进版，
判据现算 WCAG：每一阶对各自底色 ≥3:1（非文本对比度线），改回旧值立刻红。

**② 主色/错误色上的前景改走 token**。`--on-primary` 早就有了，但还剩四处手写 `color: #fff`
（DataMapPanel / KbAnswer 角标 / SkillsPanel / KbPanel 的 danger-btn）。
暗色主色 `#7b89f0` 上白字只有 3.14:1。danger-btn 的底色是 `--error-text`（暗色 `#ec8f8f` 偏亮），
白字更糟，所以另给一个 `--on-error`（亮色 `#ffffff` / 暗色 `#1a0f10`）。
判据扫六个组件源码：`color: #fff` 一处都不许再出现。

**验收**：web 判据 47 → **49**，`npm run build` 通过。

## AX132（2026-08-14，第 16 轮：回滚只标真撤成功的，账本终于带时间）

**① 撤失败也标 `rolled_back` → 那一批永久撤不回来**。原实现无条件
`UPDATE meta.learn_event SET action='rolled_back'`，两个后果叠着：
撤失败（PG 抖 / 目标行已被别处删）照样标上，重跑不再取那条；`action` 被覆盖之后，
「这条当初是新增还是改状态」也查不出来了 —— 而那是人工复核第一眼要看的。

改法：`ddl.rs` 加两条幂等 ALTER（`rolled_back_at timestamptz` / `rolled_back_by text`），
取事件的谓词从 `action <> 'rolled_back'` 改成 `rolled_back_at IS NULL`，
标记**只在 `rows_affected() > 0` 的分支里**落，且不再碰 `action`。
返回值从 `u64` 换成 `Undone { undone, skipped, failed }` —— 三个数字分开报是刻意的：
端点要能诚实地说「撤了 3 条、跳过 1 条（目标行已不在）、失败 1 条（库报错）」，
而管理员正是靠这个差别决定要不要重跑。`/api/admin/learn/{batch}/rollback` 三个数字都进响应体。

**② 账本列表带时间**。`recent_batches` 的 `min(at)` 原来只出现在 `ORDER BY`、**没进结果集**，
于是它立项时写下的那句「回答上周二学了什么」在**结构上就答不了**。
现在带 `first_at` / `last_at`（`::text`，与 `admin_api` 既有口径同源，零新增依赖），
外加 `rolled_back` 计数 —— 撤过的批次界面不该再让人点一次「回滚」。

**验收**：workspace 1901 全绿（新增 `rollback_marks_only_what_it_really_undid`、
`batch_listing_carries_time_and_rollback_state`）。

## AX133（2026-08-14，第 17 轮：语料状态变更补进账本 —— AI 把语料打成 disabled，此前撤不回来）

账本此前只盖住「新增」那半：`sql_exemplar` / `pitfall` / `memory` 的 INSERT 与教训的状态变更。
**语料自己的状态变更完全在账本之外** —— 而它恰恰是最该能撤的那一类：

- `set_ai_review`：AI 初筛判 negative → 语料直接 `status='disabled'` + `validation_status='invalid'`。
  模型判错一条，那条语料就此退出 few-shot，而没有任何记录能把它撤回来。
- `set_status`：人工/自动复核的结论落库，同样只写不记。

两个写口各加 `who: (批次号, 操作者)`，共用一个 `ledger_status_change`（**读前值 → 记一条 →
调用方再改**）。读不到前值就不记 —— 账本里一条没有前值的 update 撤不回来，记了反而给回滚一条假线索。
批次族补齐到四种：一轮问答 `trace_id`、教训复核 `review-<秒>`、
**语料初筛 `screen-<秒>`**、人工编辑 `sql-edit`。

`learn_writes_are_all_ledgered` 同步扩到 6 个写口，并接受两种落账形态
（直接 `log_event`，或走共用的 `ledger_status_change` —— 三个状态写口共用一份读前值+落账，
好过抄三遍）。

**验收**：workspace 1901 全绿。

## AX134（2026-08-14，第 18 轮：上海和海南的巡店记录被静默丢了三个月）

`ops_caliber.rs` 的省份→省区 CASE 是**手抄**的，而手抄那版**漏了上海与海南**
（权威表 `shop_business_region_for_province` 里它们分别归浙江省区、广东省区）。
漏掉的后果不是报错：`inspection_valid` 里有一句

```sql
AND (CASE WHEN s.province REGEXP '福建' THEN … END) IS NOT NULL
```

映射不出来的行**整批被排除** —— 于是「本月上海的巡店次数」恒 0、
「今年各省区巡店次数」全国合计偏低，**一个字的提示都没有**。

同一份数据在仓里有三种形态：CASE（22 个分支）、`activity_region` 的 IN 列表（23 值）、
`region_of` 的省名词表。三份各抄各的，早已漂移。

**收敛成一份**：`warehouse_catalog::standard_region_pairs()` 把权威函数的定义域列出来
（31 省 → 23 省区短名，剥掉「省区/大区」后缀），CASE 与 IN 列表都由它生成。
港澳台仍然映射不出来 → 仍然排除，那是 fail-closed 不是漏。

判据 `region_case_covers_every_mapped_province_including_shanghai_and_hainan`：
逐条断言 CASE 覆盖 pairs 里每一对、上海→浙江与海南→广东逐字钉住、
港澳台不许出现、IN 列表 23 值全部来自同一份。

**验收**：workspace 1902 全绿。

## AX135（2026-08-14，第 19 轮：账本收尾两条小的）

**① 回滚分支抽成纯函数 `undo_stmt`**。`rollback_batch` 加上第 16 轮的三态返回后已 60+ 行
（D1 是 40）；而那段 match 恰恰是**最不能出错**的一段（表名一旦来自数据就是把外部输入拼进 SQL）。
抽成纯函数之后判据从「切源码字符串」升级成**直接调它**：新增走 DELETE、更新走 UPDATE、
`meta.memory` 的更新分支返 `None`（它没有可撤的状态列 —— 不许静默当成功）、白名单外的表返 `None`。

**② 不再写 `learn_event.trace_id`**。那一列与 `batch_id` **恒等**（同一个值 bind 了两遍），
一列白存。列本身留在表里不删 —— 历史行还带着值，`DROP COLUMN` 会让旧账本少一列可读信息。

**验收**：workspace 1902 全绿（`drift.rs` 的 ds 守卫顺带补了一处判据行豁免标记 ——
它按行窗口扫，分不清「查询」和「断言里的 SQL 字面量」）。

## AX136（2026-08-14，第 20 轮：回归打了脸 —— 撤掉第 13 轮的主题门，顺手把「拒绝」变成一张卡）

80 题回归跑完：**68 通过 / 12 失败**（改前 59/80）。但里面有**四条是我自己第 13 轮打坏的**。

### 撤回：进 LLM 前的主题门

它省下的是真的（一道该拒答的题 44.4s → 8.5s，答案一字未变），但回归当场给出四条反例：

| 题 | 应出 | 被换成 |
|---|---|---|
| H01「删除订单」/ H02「清空订单表」 | **红线拦截**卡（need-intent） | 主题未接入 |
| E05 数仓缺开票事实 / E08 已开票不虚构表 | 「不可计算」降级卡（direct-doc） | 主题未接入 |

根因是我把两件事当成了一件：**「这个主题没接入」与「这个问题我拒绝执行」不是同一种拒答**，
而能区分它们的那个判据（`row_count == 0` + 路由白名单）**只有执行完才成立**。
撤回，并在原地留一段说明 —— 想再省这 30 秒，得先有一条能在执行前区分四类拒答的证据，
不是把其中一类提前。撤后 H01/H02/H03 三题即刻恢复 `route=need-intent`。

### 顺手治好的：覆盖闸硬阻断从「抛错」改成「出卡」

同一批回归里 B04/E09 是**进程非 0 退出**：

```text
本月各品牌销售额  →  Error: SQL 未覆盖结构化意图槽位：缺失:breakdown:品牌
```

系统的判断完全正确（品牌不在默认销售事实里，不许 JOIN 旧事实拼数），可用户拿到的是一条
技术错误串：CLI 非 0 退出、HTTP 422、前端一条红杠。**而这本该是系统最该说清楚的一类回答。**

回炉之后仍覆盖不了 → 出 `intent_reply` 卡 + `caliber_note` 说明哪个槽位证不了。
**fail-closed 一个字没改**：那条 SQL 照样不执行（判据里显式钉住 `!body.contains("self.execute(")`）。
实测 B04 / E09 双双从「崩」变成 `route=need-intent` 的说明卡。

### 顺带：`exemplar.rs` 拆出 `pitfall.rs`

第 17 轮往 `exemplar.rs` 里加落账之后它到了 588 行（D2 是 >500 必拆）。
语料（喂 few-shot）与教训（喂 prompt 的「坑」段）是两条独立的学习链，拆开后 497 / 103。
拆的当场 `learn_writes_are_all_ledgered` **变红**（只数到 5 个写口）—— 那条判据正是这么用的。

**验收**：workspace 1902 全绿。

## AX137（2026-08-14，第 21 轮：合同为什么被拒，得有人说）

`IntentAttempt::Invalid` 是本仓最贵的一种降级：自由 SQL 关掉、语义缓存关掉、
确定性路径的收据全降 review，严重时**同一个问句在两次进程里给出两条路由**
（第 14 轮排查图路由漂移时，日志里只有一行 `结构化意图 JSON 不合约`，看不出是哪个字段）。

根因不在模型，在观测：`intent_from_value` 是 `serde_json::from_value(value).ok()?` ——
「模型多写了一个字段」和「模型压根没回 JSON」在日志里长得一模一样。

现在两条拒绝路径各留各的痕：字段合同不符（带 serde 的原始错误，直接指出是哪个字段）、
归一化判否（槽位不是原问句子串这一族）。**合同一个字没放宽** ——
`deny_unknown_fields` 是刻意的（脏字段不许偷偷带 canonical id 进来），
判据正反两条钉住：合同外字段继续拒、`null` 字段仍按缺省处理（AX117 那条不许回退）。

**验收**：workspace 1903 全绿。

## AX138（2026-08-14，第 22 轮：「这个数仓里没有」的那张卡，被覆盖闸挡回去了）

回归 E05/E08 的根因不在触发词，在**闸门顺序**：

```text
本月开票金额
  direct-doc 命中 → 「不可计算」卡（SELECT '不可计算' AS `数据状态` … FROM dms_ods.t_dict_value LIMIT 1）
  → 覆盖闸判 blocking（这张卡没有时间谓词、没有指标）
  → 回落下一成员 → 自由 SQL 接手
  → 答成 fin_ads.ads_fin_profit_loss_dnf.financial_income 的合计，收据 verified
```

**自由 SQL 去找了一个「名字像」的字段替代** —— 正是这张卡当初要拦的那件事。

根因是拿「回答」的判据去判一张**明确说「我不回答」的卡**：它按设计就不覆盖用户槽位。
`derive::is_unavailable_card` 这个识别口径本来就有（三张降级卡同一个投影头），
`land()` 里加一道：识别到它就跳过覆盖闸直接落地。豁免**只给这一张卡**，
其余确定性模板照旧过闸（判据同时钉住「覆盖闸整条不许消失」——那是另一个方向的错）。

**验收**：workspace 1904 全绿。

### 记一笔：B01W 的失败是**判据脆**，不是产品错（2026-08-14 第 23 轮查证）

`山东省 2026-08-10 到 2026-08-11 销售额` 实测：`route=direct-agg`、SQL 与金文件一致，
收据里三个槽位是

```text
metric:销售额 = resolved
region:山东省 = resolved
time:2026-08-10 到 2026-08-11 = grounded
```

而题目钉的是 `time:2026-08-10:resolved` —— 它把 **fast 模型输出的 surface 字面量**写进了断言。
模型这次把两个日期连成一个 surface（合理），断言就红了。

**不改产品、也不改题**：改题会掩盖真回归（surface 变化有时确实是错的），
改产品去迎合一个字面量更糟。留在这里当已知项：这条断言该换成「按 kind+state 判、surface 只判前缀」，
属于判据形态整改，需要连带复核另外几条同形态的题。

## AX139（2026-08-14，第 24 轮：把这一批新增文件写回 ARCHITECTURE 的落点清单）

本批新增/搬迁的六个文件此前不在 §4 的文件表里 —— 而那张表是「落点清单」，
下一个人按它找东西、门禁按它数预算。补齐并同步改了两行已漂的描述：

| 文件 | 说明 |
|---|---|
| `registry/pitfall.rs`（新） | 教训表的唯一读写口，2026-08-14 从 exemplar 拆出（D2 >500 必拆 + D3 两条独立学习链） |
| `registry/learn.rs`（新） | 学习事件账本：前值/后值/批次号 + 三态回滚 + 纯函数 `undo_stmt` |
| `registry/user_pref.rs`（新） | 用户习惯层，从 `query_log` 现算、只进 prompt 参考段 |
| `registry/failure.rs`（新） | 失败经验的读回半（连续次数判据） |
| `agent/hybrid.rs`（搬迁） | 混合问句的唯一编排点（原在 server，两条链路行为相反） |
| `registry/exemplar.rs`（订正） | 行数 120 → 497；补「三个状态写口共用 `ledger_status_change`」 |
| `answerers/graph.rs`（订正） | 准入判据改成 `skip_reason()` 六项、只看问句不读合同 |

## AX140（2026-08-14，第 25 轮：一次**作废**的回归 + 它暴露的一件真事）

第 20 轮之后重跑 80 题，结果 `31 通过 / 48 失败`。**这个数字作废** —— 48 条失败全是同一句：

```text
dms_connector::mysql: 建只读池失败 reason="mysql_pool_connect_failed"
Error: DMS 身份/权限库连接失败（连接失败 [dms-auth] 数据库连接不可用）
```

跑到一半远端库开始拒连（`ping on idle connection returned error: expected to read 4 bytes, got 0`
= 服务端主动断开）。容器内 TCP 探测 9030 **通**，说明不是网络断，是**握手被拒** ——
一次回归 = 80 个 CLI 进程各建一次连接池，叠加常驻服务，撞上了远端的连接数上限/限流。
冷却几分钟后自动恢复。

**这是跑法的代价，不是产品缺陷**，但它确认了一件事：**fail-closed 的方向是对的** ——
库连不上时服务拒绝启动、健康检查照实报 `unavailable`，而不是带着空权限或旧快照继续answering。

**给下一轮的操作纪律**（写进这里免得再踩）：
1. 全量回归**不要连着跑**。80 题 × 每题一个新进程，对公网库是一次小规模压测。
2. 跑之前先看 `/api/health` 的 `mysql.connected`；跑完之后再看一次 —— 中途掉线的那次数字没有意义。
3. 真要连跑，先把 CLI 换成 HTTP（走常驻服务的连接池），那才是与线上同构的跑法；
   现在的 `docker exec` 形态每题都重建全套连接（还附带 ~30s 启动费，见 AX129）。

**第 22 轮的实机验收**（重建容器后单跑）：

```text
E05-数仓缺开票事实明确降级 · route=direct-doc 2823ms  ✅（改前 llm+schema-fix，用 financial_income 顶开票金额）
E08-已开票不虚构不存在的表 · route=direct-doc 2868ms  ✅
```

---

## 本批（AX119–AX140，2026-08-14）收尾状态

**验收**：workspace **1904** 全绿 / 架构门禁 **16/16** / web **49** 全绿 / `npm run build` 通过。
远端库连接在第 25 轮的压测中被打限流，冷却后已恢复（`mysql.connected: true`）。

**未提交**：按业主「所有开发完后一起提交」的指示，本批**一次未提交** ——
工作区累计 104 文件 / 约 19k 行。下一位接手前请先确认要不要落一个存档提交。

**已知未修（有据可查，不是遗忘）**：
| 项 | 状态 |
|---|---|
| OPS01/OPS02 运营口径两题 | 回归里超时；本机 CLI 每题 ~30s 启动费是主因（AX129），需换 HTTP 跑法再判 |
| OPS04 湖南运营省区归一 | `route=llm+schema-fix ≠ direct-agg`，第 18 轮的省区收敛改的是口径不是路由 |
| E10 库存取中台现行库存 | `intent.mode=unknown` —— fast 合同偶发不合约（AX137 已让理由可见，尚未治因） |
| B01T 客户名带类别前缀 | 实体探针没命中「批发-董会琴」，依赖线上数据 |
| B01W 周报单省显式周窗 | **判据脆**，非产品错（详见 AX139 前那段记录） |
| accuracy-next #1/#5/#6 | ODS 表补录 / 深度报告板块继承 / `allowed_dimensions` 进 CaliberRule，均未动 |
| learning-ledger #3/#6 | 回滚的乐观并发守卫 / `set_lesson_status` 改 CTE，未动 |
| visual-system #8–#14 | 遮罩层、触控热区、嵌入双层壳、圆角档位等七条，未动 |

## AX141（2026-08-14，第 26 轮：运营看板那条口径路，平时根本不生效）

OPS 四题实测：**0/4 → 4/4**，且全部落在 ~200ms 的确定性路径上
（改前三题「超时」、一题路由错 —— 那三条超时的真身就是这个：口径路被挡回去后去跑自由 SQL）。

### 根因：代码写死的 SQL，被按 LLM SQL 的形状判了

```text
2026年6月湖南运营活动费用是多少
  compose_hit → ops_caliber::direct_metric 命中 ✅（SQL 里时间窗、省区都在）
  → 覆盖闸：CoverageReport { missing: ["time:2026年6月"], unverifiable: ["region:湖南"] }
  → blocking → 回落下一成员 → 自由 SQL
```

闸门认不出这两样，因为运营口径的 SQL 是**代码写死**的：时间窗写成字面日期
（`a.start_date >= '2026-06-01'`）、省区写成 `CASE(...) = '湖南'`，而闸门是按
「LLM 会怎么写」的模板形状判的。于是这条**每次都对**的路，平时一次都走不到。

### 修法：让它自己声明兑现了哪些槽位

`direct_metric_with_evidence` 返回 `ExecutionEvidence`（指标 / 省区 / 时间窗三个 `resolve`），
`compose/metric.rs` 把它挂到 `DirectHit.intent_evidence` 上。
**这不是放宽判据**：销售快路径早就这么做（`fastpath_intent` 里 Region/Time 两处 `resolve`），
声明的是「代码确实消化了这个槽」这一事实。判据反面也钉住：没有时间词时不许凭空声明。

### 顺带补上的两个缺口

1. **显式年月的表面词提取**（`year_month_surface`）。`time_phrase_of` 见到 `20` 就返 None
   （它只认相对词），两个 ISO 日期那支也不匹配 —— 而「2026年6月」正是运营看板最常见的问法。
   放在**整句兜底之前**：兜底返回整个问句，拿它当「已消化」会把「长沙」这种没处理的限定
   一起吞掉（第一版就是这么写的，被既有判据 `direct_metric("2026年6月长沙…").is_none()` 当场抓住）。
2. **`region_of` 收敛**（第 18 轮那条的第三种形态）。问句侧的省名词表也是手抄的、
   同样漏了上海与海南，现在同样从 `standard_region_pairs` 生成，只额外挂三个
   「只出现在问句里」的说法（苏南/苏北大区、江苏省区）。长词优先排序，
   免得「内蒙古自治区」被「内蒙」截胡后留下残留词。

**验收**：workspace 1904 全绿；OPS 组 4/4 实机通过。

### E10 查清了但**不改**（2026-08-14 第 26 轮）

「现在库存量是多少」：路由与 SQL 都对（`direct-agg` 211ms，命中中台现行库存模板），
只有收据判 `mode=unknown / status=blocked`。连跑三次**稳定复现**，且
**没有**合同解析失败的日志（AX137 那两条 warn 一条没出）—— 说明不是 JSON 不合约，
是 fast 模型自己把 `mode` 标成 unknown 或填了 `ambiguities`。

而它标得**有道理**：「库存」在本库确实有两个来源（中台现行库存 / 门店进销存），
`seed.rs` 的警告里白纸黑字写着这件事。系统按业务裁决选了默认源、答对了，收据照实说
「模型当时不确定」—— 这正是 `AGENT-ARCHITECTURE §3.1` 要的行为。

**不改的理由**：让确定性模板反过来把合同「升级」成 grounded，等于用我们自己的判断
盖掉模型报告的歧义 —— 下一次真歧义就没人报了。要治该治**输入**（让意图提示知道
「库存量」是已登记指标、默认源已裁决），那是 prompt/词表侧的活，需要连带过一遍
`tools/regression.py` 的 LLM 路题，不在本轮范围。

## AX142（2026-08-14，第 27–28 轮：知识库两条 critical/high —— 该说的话被删了，不该说的话被说了）

W4 清单前两条，都是「答案本身没错、但呈现给用户的那一份是错的」。

### ① 「部分覆盖」声明结构性活不下来（critical）

SYSTEM 里白纸黑字要求：资料只覆盖问题一部分时，**第一条**必须以「知识库里没有关于」开头。
可它是**否定断言、天然没有角标** —— 一进 `keep_line` 的角标过滤就被整句剔掉。

```text
用户：出差住宿和市内打车各有什么上限
模型：知识库里没有关于市内打车费的规定。      ← 被删
      住宿费上限每晚八百元[^1]。              ← 只剩这句
用户看到的：一个只答了住宿的答案 —— 他会把 Y 当成 X 的答案。
```

此前唯一相关的测试只断言「SYSTEM 里含这个字符串」，**没有一条判据管它能不能活到用户面前**。

豁免只开这一条，且**必须无数字**：不许借这个壳夹带无据数值
（「知识库里没有关于打车的规定，但住宿是 800 元」→ 后半句无角标，照旧删）。
同刀在 `has_supported_content` 里把它排除在「有实质内容」之外 ——
整篇只剩这句时仍退回 NO_HIT，那是诚实的失败。

### ② 版本冲突兜底不看有没有被引用（high）

`retrieve` 侧的 `preserve_governed_versions` / `preserve_textual_versions` 是**主动**
把冲突版本追加进 TOP_K 的，所以「上下文尾巴里躺着一对与本问题无关的新旧版」
**是被设计出来的常态**。而 `disclose_versioned_sources` 全程不扫角标：

```text
用户：报销要交哪些材料
召回尾巴：培训报销 v1 / 培训报销 v2（一个都没被引用）
返回：一张「请由制度负责人确认」的核对表 —— 好答案被降级成了待办
```

同文件的 numeric 侧早就要求「该组至少一个成员被引用」——两个兄弟函数口径不一致。
现在 family 与 textual 两条入选条件各追加同一句判据（共用一个 `refs(md)` 扫描结果）。

**验收**：workspace 1907 全绿、架构门禁 16/16。新增判据两条：
`partial_coverage_disclaimer_survives_the_citation_filter`（含两条反面：带数字不许豁免、
整篇只剩它时不算有内容）、`unselected_version_conflict_in_retrieval_tail_does_not_replace_the_answer`。
既有的 `version_conflict_keeps_complementary_facts_from_other_documents` 等全绿不变。

## AX143（2026-08-14，业主实测：知识库问什么都不回答 —— 根因与修法）

### 现象

业主在**服务器**上问「线下设备申请的政策」，拿到的是问数口吻的澄清卡：

```
先问清再查
意图解析结果未通过一致性校验。为避免误解你的问题，我没有执行模型生成的查询；
请补充明确的对象、指标和时间后重试。
理解缺口：尚未确定应使用问数还是知识检索，需要补充问题限定
```

「请补充明确的对象、指标和时间」对一句政策问句毫无意义 —— 用户被要求补充一个根本不存在的东西。

### 根因（`server/src/main.rs` 的 `/api/ask` 与 `/api/ask/stream`）

```rust
IntentRoute::Hybrid | IntentRoute::Unknown => {
    prepared.question.clarification_result()   // ← 知识库一次都不查
}
```

**知识库问句天生没有指标、没有时间、没有实体** —— 正是数据合同最容易判 `Unknown/Invalid`
的那一类。而 Unknown 那一臂直接返回澄清卡，`kb_answer` 一次都不调。
于是「问知识库无论问什么都不回答」——**不是知识库坏了，是问句根本没被送到知识库**。

这条判据的立意本身没错（合同不可用时不许自由生成 SQL），错在把「不能问数」当成了
「不能回答」。**合同不可用 ≠ 知识库不能答**：`answerers::knowledge::answer` 对 intent
**零依赖**（只吃 store/embed/llm/principal/space/question/weights，本文件已逐行确认），
而检索本身 fail-safe —— 查不到就说「知识库里没有相关内容」。

### 修法

`unknown_route_kb_fallback`：Unknown 臂先问一次知识库，**只有真的检索到带引用的内容**
才顶替澄清卡；没查到就照旧出卡（数据问句的体验一个字不变）。兜底失败留 warn。
**问数侧零改动**：这条路不生成任何 SQL。两个端点（`/api/ask` 与 `/api/ask/stream`）
同时接上 —— 流式与非流式对同一句话给出不同答案，是本仓反复付过账的那类分叉。

判据 `unknown_contract_consults_the_kb_before_giving_up` 钉三件事：兜底存在且真调
`kb_answer`、判了「有没有引用」、**两个端点各接一次**（`unknown_route_kb_fallback(` 恰好出现 3 次）。

### 同刀补上的一处排查盲区

`retrieve.rs` 的「可见文档为 0 → 一条召回查询都不发」早退，此前**一行日志都没有**
（那句「检索零命中：各路召回数」写在早退之后，永远走不到）。于是「库里没有 / 权限看不到 /
状态没就绪」三种情况在服务端长得一模一样。现在打 `login + roles + space` 三个变量并指明
该去查哪几张表。

### 还没做的一半（明确记账，不是遗漏）

MCP / CLI / 深度报告子问走的是 agent 的 `ask_prepared`，那条路的 Unknown 分支同样不查知识库；
接它需要 `AskDeps.kb` 在两处 `kb: None` 的构造点补上，而那两处的宿主函数
（`main.rs` 的 `ask()`）签名里没有 `OwnedStore` 与 rrf 权重，要多穿两个形参。
本轮先修用户实际撞到的 HTTP 面；这一半连同「为什么回归题集从来没盖住知识库」一起下轮做。

### 现场事实（存档）

- 生产健康检查全绿：mysql 只读连通、vector_ready 三项 true、doc_service.ok、graph 已同步
- 生产跑的是**本次会话之前**的二进制（health 响应里没有本会话新增的 `breakers` 字段）
  → 本批改动**不是**这个 bug 的成因，修完必须部署才生效
- 本地开发库 kb.doc 0 行 / kb.chunk 0 行，但 `/kbdata` 下有 3 个真实文档文件
  —— 本地环境自身的语料状态问题，与生产无关，单独查

## AX144（2026-08-14，业主三条：文件下载 / 混合查询 / 不够智能 —— 同一个根因）

### 决定性证据：v2 合同在生产上 **100% 被拒**

从生产日志抓到三条，模型每一次都**理解得完全正确**：

```
01:20  {"version":2,"mode":"data",  "subgoals":[{"mode":"data",     "surface":"本月销售额是多少"   ...}]}  → 不合约
01:22  {"version":2,"mode":"hybrid","subgoals":[{"mode":"knowledge","surface":"线下设备申请政策"   ...}]}  → 不合约
01:27  {"version":2,"mode":"hybrid","subgoals":[{"mode":"data",     "surface":"查一下最近的设备订单"...}]}  → 不合约
```

**连「本月销售额是多少」都被拒** —— 也就是说整套 IntentV1 v2 subgoal 机制**从上线起就没生效过**，
所有问句都退化成 Unknown 或最小合同兜底。业主的三个抱怨因此是同一个根因：

| 症状 | 为什么 |
|---|---|
| 知识库问什么都不回答 | 合同被拒 → Unknown → 澄清卡，问句根本进不了知识库 |
| 混合查询不支持 | `mode:hybrid` + 两个子任务的合同被拒，hybrid 两路并行从没被触发过 |
| 不够智能、要先用大模型理解意图 | 大模型**已经**理解了，是我们把它的理解丢了 |
| 要文件下载 | 下载能力后端前端**早就完备**（`/api/kb/doc/{id}/download`、预览票据、`downloadSource()`）—— 只是没有 citations 就没有来源文档卡，也就没有下载按钮 |

### 根因：格式洁癖丢掉了正确的理解

提示词规则 3 要求「version=2 且存在 subgoals 时，根级执行槽位必须为空」，
而模型**同时**填了根级与子任务槽位 —— 那是它表达「共享条件」最自然的方式。
`v2_root_slots_assigned` 判否 → `ground()` 整份返 `None` → `IntentAttempt::Invalid`。

而 `ground()` **一条日志都不打**：外层只说一句「JSON 不合约」，定位这件事花了半小时。

### 修法

**① 根级槽位按归属下推**（`push_down_root_slots`）。归属判据与本仓其它地方同源：
该槽位原文出现在子任务的 `surface` 或 `evidence_surfaces` 里才算它的。
归属不到任何子任务的**原样留在根级** → 仍然被拒 —— 那才是真歧义（提示词原话：禁止让系统猜归属）。
方向只会**收窄**（子任务多带一个条件），不会放宽，fail-closed 不破。

**② 拒绝理由有名字**（`grounding_reject_reason`）。十条判据各一个字面量
（`root-slots-left-after-pushdown` / `mode-does-not-match-subgoal-route` / …），进 `warn!`。
纯函数，与 `ground` 共用同一批判据 —— 诊断自己重判一遍就会漂（`why_not_compose` 上付过这个账）。

**判据**：`v2_contracts_with_root_slots_survive_by_pushdown` 用**生产真实合同形态**钉三条：
最简单那条活下来、混合合同活下来且 `route() == Hybrid`、归属不明的根级槽位继续拒且理由有名字。

### 顺带修好的部署脚本两个真缺陷

1. **MSYS 路径改写**：Git-Bash 把 `/opt/dms-ai/src.tar.gz` 改写成 `D:/Program Files/Git/opt/...`，
   远端写不进去，客户端只看到一句莫名的 `OSError: Socket is closed`。已 `export MSYS_NO_PATHCONV=1`。
2. **长构建挂在一条 SSH 长连接上**：服务器 Docker 构建 5-10 分钟，这条链路撑不住，
   一断脚本退出、远端构建收到 SIGHUP 一起死 —— 表现是「跑了十分钟镜像还是旧的」，
   **退出码还可能是 0**（管道吞掉）。改成 nohup 后台 + 客户端短连接轮询 rc 文件，
   构建失败则不切换 app（生产保持旧版本）。实测这次一次通过。

**验收**：workspace 1910 全绿；生产已上线 `20260814T014655Z-9993`，健康检查 ok:true，
新二进制含 `结构化意图未通过 grounding` 字符串。

## AX145（2026-08-14，知识库/混合问句：三层叠加的根因，生产实测全通）

AX144 只对了一半。日志上线后真相是**三层叠加**，缺一条都答不出来：

| 层 | 现象 | 修法 |
|---|---|---|
| ① fast 模型**间歇性**吐出解析不了的 JSON | 合同 `Invalid` → 自由 SQL/语义缓存/知识库路由全关 → 澄清卡 | 解析失败**重试一次**（调用失败/超时不重试 —— 那是链路问题，重试只把 10s 变 20s） |
| ② Unknown 臂直接出卡、一次不查库 | 知识库问句天生 Unknown（无指标/时间/实体） | 先问一次库，**查到带引用的内容**才顶替卡片 |
| ③ CLI/MCP/深度子问 `kb: None` | 混合问句知识半被静默丢掉（实测 `route=compound, subs=1` —— 问两件事只拿回一件） | `OwnedStore::from_pool` 从已有池借 store，十个调用点一个没动 |

**为什么之前判断错**：`clip()` 只留 200 字符，v2 合同的第一个 subgoal 都放不下 ——
看不到完整回包，就把「JSON 被截断/畸形」误判成「grounding 太严」。
`ground()` 又是纯黑盒（十条拒绝判据零日志）。两个盲区叠在一起，方向就歪了。

**观测面补全**（否则下次还得再挖一遍）：
- `ground()` 十条判据各起名字（`root-slots-left-after-pushdown` / `mode-does-not-match-subgoal-route` / …）进 warn
- 拒绝时打**完整**回包（4000 字符）+ 长度 + `completion_tokens`（被供应商上限截断时它顶在整数上）
- JSON 严格解析失败打 serde 报错位置（`EOF while parsing` = 截断的直接证据）
- 容错解析也全军覆没时说一句（此前完全静默）

### 生产实测（`20260814T0...` 部署后，容器内 CLI 直打）

```text
线下设备申请政策                         → route=knowledge，答出多级审批流程/价格填写/投放方式，带角标
查一下最近的设备订单，并且最近的线下设备政策 → 两路都跑：数据半 no-topic + 知识半完整答出
下载 押金转货款申请书                     → route=knowledge，给出模板与办理流程，并明说「库里没有该文件实体」
```

### 记两件与本轮无关但暴露出来的事

1. **「设备订单」主题数仓没接入** —— 混合问句的数据半答不出来是这个原因，不是链路问题。
2. **文件下载能力本来就完备**（`/api/kb/doc/{id}/download` + 15 分钟预览票据 + 前端 `downloadSource()`）；
   「下载押金转货款申请书」拿不到文件，是**那份文件没作为原件入过库**（库里只有正文模板）。

### 顺带修好部署脚本两个真缺陷

- **MSYS 路径改写**：Git-Bash 把 `/opt/dms-ai/src.tar.gz` 改成 `D:/Program Files/Git/opt/...`，
  远端写不进去，客户端只看到 `OSError: Socket is closed`。已 `export MSYS_NO_PATHCONV=1`。
- **长构建挂在一条 SSH 长连接上**：服务器 Docker 构建 5-10 分钟，链路撑不住，一断脚本退出、
  远端构建收 SIGHUP 一起死，**退出码还可能是 0**（管道吞掉）→「跑了十分钟镜像还是旧的」。
  改 nohup 后台 + 客户端短连接轮询 rc 文件；构建失败不切换 app。实测一次通过。

**验收**：workspace 1909 全绿、架构门禁 16/16；生产已上线并逐句复验。

## AX146（2026-08-14，业主：「你不能头疼医头，这类问题的本质你还是没有解决」）

### 最有价值的一张截图

问「下载 押金转货款申请书」→ 系统返回 **38 行账余充值明细** + 深度 BI 板块。
用户要一份**文档**，系统给了一堆**数据行**，而且很自信（生成了分析页）。

### 两条决定性证据

**① 同一个问句，不同入口不同答案**

```text
容器 CLI            → route=knowledge
HTTP（深度模式）    → 38 行数据表 + 深度 BI
```

**② 路由决策抄了五份**（`grep` 实测）

| 判据 | 出现次数 |
|---|---|
| `prepared_contract_ready(&prepared)` | 4 |
| `projected_forced(&prepared` | 3 |
| `is_data_executable()` | 3 |
| `clarification_result()`（出卡点） | **14** |

`api_ask` / `api_ask_stream` / `mcp_api::tool_ask` / `xcx_api` / `deep_api` 各有一份**逐字复制**的
决策链。前几轮我每次修 1–2 处，业主换个入口就复发 —— 这就是「头疼医头」的物理成因，
不是态度问题，是**决策没有单一落点**。

### 本质（初判，待体检确认）

1. **路由 = 单次 LLM 输出的 `mode` 字段**，没有一致性保障 —— 同一句话两次判不同。
2. **合同没有「用户要做什么」这一维**：`IntentMode` 只有 `data|knowledge|hybrid|unknown`。
   「下载 X」「导出 X」「给我 X 的文件」**无处安放**，模型只能硬塞进 `data` ——
   于是「押金转货款」匹到账余充值表，38 行数据就出来了。
3. **快路径按「词的存在性」抢答**，不看用户要什么。
4. **合同主要用于事后否决**（coverage 闸），而不是驱动决策 —— 模型理解对了也没用。

已启动架构级体检（五套入口测绘 / 快路径准入 / 合同表达力 / 澄清面 / 开源对标 yuxi·SuperSonic·Adaptive-RAG），
出方案后分批实施，不再逐点打补丁。

## AX147（2026-08-14）架构级整改·批次 1+2：路由从「一维 × 五份分派」改成「两维 × 一次裁决」

### 本质诊断（体检结论，28 条findings 核实 12 条）

1. **合同缺「交付面」这一维**。`IntentV1` 13 个槽位全是取数面，`IntentMode` 只有
   `data|knowledge|hybrid|unknown`。「下载/发我一份/打印」**无处安放** ——
   `EntityKind::Document` 定义了但**零消费者**，`route()` 的 `has_data_slots` 只判
   `entity_mentions` 非空，把最有区分力的那一位扔了。
2. **路由 = 一次 fast LLM 采样的 `mode` 字段**，确定性信号零参与。同一句问两次两条路。
3. **裁决点唯一但分派复制五份**，兜底只接了其中一部分；守卫测试按 `main.rs` **单文件**扫描，
   `deep_api` / `xcx_api::ask_stream` 天然漏网 —— 判据的扫描面比缺陷面小。
4. **失败模式只有一种**：同一张无信息澄清卡，且文案是合同结构的镜像（只会问指标/时间/对象）。
5. **执行层判据是裸 `contains`**：`kw_force` 种子 `("押金","t_customer_balance")` 让
   「下载 押金转货款申请书」把账余表钉成 schema 上下文第一张卡 → **38 行账余充值明细**。

### 改了什么

| 文件 | 动作 |
|---|---|
| `kernel/src/nl/doc.rs` | 新增：文档名词/扩展名/取件动词三张词表 + `signals()` / `is_document_request()`（纯函数、零 IO） |
| `agent/src/ask.rs` | 新增 `Deliverable` / `AskPlan` / **`decide()`** ——★ 全系统唯一裁决点；`PreparedQuestion::route()` 改读 `plan().route` |
| `agent/src/answerers/knowledge.rs` | `answer()` 首行分流；新增 `documents()`：检索 → 按 `doc_id` 去重 → 文件清单卡，**0 次 LLM** |
| `knowledge/src/answer.rs` | `citations` 改 `pub`（不抄第二份，`Citation` 19 个字段抄了必漂） |
| `server/src/main.rs` | `prepared_contract_ready`：确定性车道免合同；守卫测试扫描面扩到**四个入口文件** + 深度模式臂序钉板 |
| `server/src/deep_api.rs` | 合同闸早退先问一次知识库（此前唯一没接的入口） |
| `server/src/xcx_api.rs` | 流式 Unknown 臂同上（流式/非流式此前对同一句给两种答案） |

### 决策规则表（首条命中即止）

| # | 条件 | 结果 | deterministic |
|---|---|---|---|
| R0 | `forced` 非空（前端 chip） | `{forced, Answer}` | false |
| R1 | 动词 × (文档名词 \| 扩展名) | `{Knowledge, Document}` | **true** |
| R2 | 有文档名词、无可度量槽位、且合同没说 Hybrid | `{Knowledge, Answer}` | **true** |
| R3 | 合同有意见 | `{合同的路, Answer}` | false |
| R4 | 其余 | `{Unknown, Answer}` | false |

**fail-closed 不变量**：`deterministic == true` 时 `route` 只可能是 `Knowledge`，永不是 `Data` ——
确定性规则只能把问句**推离** SQL 生成，永不能推入。单测 `deterministic_rules_never_produce_data` 钉住。

**「导出」刻意不收进动词表**：它是问数的既有功能（结果集导出 Excel），收了就会让
「导出标准成本明细」「导出上月合同金额」被 `标准`/`合同` 撞成文档诉求 —— 一个词换一整类误路由。

### 生产验收（release `20260814T042450Z-29625`，容器 CLI 实测）

| 问句 | 路由 | 耗时 | 结果 |
|---|---|---|---|
| 下载 押金转货款申请书 | `knowledge` | **5.3s** | 文件清单卡，首条 `押金转货款申请书(1).docx`，带下载引用；**无 38 行账余表** |
| 线下设备申请政策 | `knowledge` | 26s | 带引用政策答案，无澄清卡 |
| 本月销售额 | `direct-agg` | **93ms** | 1.005 亿 + KPI + 环比 17.2% + 明细（问数一字未动） |
| 查设备订单 + 线下设备政策 | `compound` | 20s | 两半都跑；数据半 no-topic（设备主题未接入数仓，既有缺口） |

workspace 1919 绿。

### 判据（防复发）

- `ask.rs::a_document_request_routes_the_same_whatever_the_contract_says` —— 同一句喂四份不同合同
  （data / knowledge / Unavailable / Invalid），plan 必须恒为 `{Knowledge, Document, deterministic}`。
- `ask.rs::deterministic_rules_never_produce_data` —— fail-closed 不变量。
- `ask.rs::routing_has_exactly_one_decision_point` —— `route()` 只许转发 `plan()`；`decide` 只许有一份。
- `main.rs::every_entry_consults_the_kb_before_showing_a_card` —— **四个入口文件**按形状扫，
  每个 `Unknown` 臂与每道合同闸后面必须跟一次知识库兜底；外加深度模式知识臂必须在转问数之前。

### 待办（后续批次）

- 批次 3：五份分派前缀收敛成 `guard_or_fallback` / `knowledge_payload` 两个函数（纯重构，净减 ~80 行）。
- 批次 4：mixed 双路（有文档名词 + 有数据槽位 + 合同没拆出知识子任务）→ 用既有 `Answer.subs` 出两块面板。
- 批次 5：澄清卡文案去内部术语（「意图解析」「生成 SQL」「一致性校验」）；`intent_summary` 增
  `plan_reason`（收据显示「按什么定的路」，误路由自证）；删 `triage` 死代码
  （`triage`/`rule_intent`/`kb_hit`/`strong_doc_intent`/`hybrid_clauses`/`llm_intent`）；
  `recall/schema.rs` 加一行 `is_document_request` 守卫防绕过。
- 「设备订单」主题数仓仍未接入（本轮未动，属数据面缺口不是路由缺口）。

## AX148（2026-08-14）三条实测反例：Hybrid 后门 / 裸单号 / 文件清单精度

业主三张截图，三个不同的洞，其中两个是我上一批（AX147）自己留的。

### ① 「客户打款 退款政策」→ 200 行账余充值明细

**不是检索不到**。同一句话在容器 CLI 上答得很好：

```text
Q: 客户打款 退款政策 → route=knowledge，引用 6 条
「结束合作走云之家【线下客户退出申请】；继续合作仅打款错误走人人费用通用报销，
 费用项目选"销售.销售_销售打款错误"…」
```

生产 PG 实测也证明标题**在**检索面上 —— `kb.chunk.embedding_text` 的开头就是：

```text
文件：客户打款退款指引.docx 目录：/指引合集/后勤财务 章节：客户打/退货款
```

真正的成因：AX147 的 R2 **给 Hybrid 开了后门**（「模型说这句有两件事，别压成单路」）。
合同这次判 hybrid，data 子任务是「客户打款」—— 没有指标、没有时间、不是一道能算的题 ——
照跑，在 `dms_ods.t_customer_balance` 上拉回 200 行、口径复核还不通过。
**同一句话，合同判 knowledge 就答对，判 hybrid 就出垃圾**：非确定性从这个后门原样回来了。

**修**：删掉那条豁免。`has_measurable_slots` 本来就同时看根与 subgoals，所以判据自动变成
「**数据半必须自带可度量槽位**（指标/时间/分组/对比），否则它不是混合问句，
是一句被劈成两半的政策问题」。真混合问句不受影响 ——
「查最近的设备订单，并且最近的线下设备政策」的数据半带时间槽位。

### ② 裸单号 `CZ202608131914` → 走了知识库

合同判 Unknown（一个裸号抽不出任何槽位）→ 触发 AX147 加的 `unknown_route_kb_fallback` →
知识库拿一份讲「账余记录」的文档**答了出来**、还带引用 → 顶替卡片上线。
用户要查一张单，拿到的是「账余记录页面位于财务 > 客户账余记录」。

**修**：新增 **R1.5 单据号点查** —— 单号是全系统最不含糊的问数信号，判 `Data` 且 deterministic。

判据从「单号」这个**词**上摘开：`triage::doc_code_hit` 拆出 `code_token_hit`（只认真的
字母数字混排 token）。「账余记录单号是什么意思」是口径问句，不许判成点查。

**fail-closed 口径同步纠正**：此前写的是「确定性规则永不判 Data」。这条口径是错的 ——
真正的护栏在 `run.rs:1455`：`LlmAnswerer::accept == is_data_executable()`，
合同没 Ready 时自由 SQL 那一路**结构上不接单，与 route 判成什么无关**。
判据改成 `deterministic_rules_never_open_free_sql`：确定性 Data 只许有 `code-lookup`
一个理由，且那道合同闸必须还在原处。

### ③ 文件清单 5 条只有第 1 条相关

「下载 押金转货款申请书」返回 5 份，第 2-5 份是《线下设备物资处置申请单流程指引》
《客户退出申请流程填写详细指引》—— 靠正文里的「申请」两个字挤进向量召回。

**修**：加一条 yuxi 式的**文件名信号**。`kernel::nl::text::longest_common_run`
（最长公共子串，朴素 DP、零依赖），`documents()` 按它重排并剪枝：

| 文档 | 与问句的最长公共子串 |
|---|---|
| 押金转货款申请书(1).docx | **8** |
| 客户退出申请流程填写详细指引.docx | ≤2 |
| 线下设备物资处置申请单流程指引.docx | ≤2 |

剪枝只在「真有人对上了」时发生（最佳 ≥3 字），保留最佳的一半以上；一个字都对不上就全留 ——
宁可多给，不可给空。

### 词表补充

`DOC_NOUNS` 加「指引」「细则」：生产知识库的一级目录就叫**指引合集**，
104 份文档里大量以「指引」结尾（客户打款退款指引 / 客户退出申请流程填写详细指引 / …）。

### 现在的规则表

| # | 条件 | 结果 | deterministic |
|---|---|---|---|
| R0 | `forced`（前端 chip） | `{forced, Answer}` | false |
| R1 | 动词 × (文档名词 \| 扩展名) | `{Knowledge, Document}` | **true** |
| R1.5 | 有单据号 token | `{Data, Answer}` | **true** |
| R2 | 有文档名词 **且** 全局无可度量槽位 | `{Knowledge, Answer}` | **true** |
| R3 | 合同有意见 | `{合同的路, Answer}` | false |
| R4 | 其余 | `{Unknown, Answer}` | false |

workspace 1922 绿。

### 记一笔工具账

本轮的知识库体检 workflow（6 个 agent）**全部挂在 `403 Please run /login`**，零产出。
诊断是自己连生产做出来的：先查 `kb.chunk.embedding_text` 证明标题在检索面上，
再用容器 CLI 跑同一句话证明检索没问题 —— 两步就把「检索不到」这个错误假设排除了。
教训：症状指向 A（检索），先花两条命令证伪 A，再去找 B（路由）。

### AX148 补记：部署脚本被我自己打限流

第二次部署死在**构建轮询**：

```text
paramiko.ssh_exception.SSHException: No existing session
```

轮询每 15 秒开一条**新** SSH 连接，一次构建最多 120 条 —— 打满 `sshd` 的
`MaxStartups`（默认 `10:30:100`）后新连接被直接拒。而 `deploy_update.sh` 开着 `set -e`，
一次失败整个脚本退出：**镜像已经建好、`app` 却没切**（实测镜像 7 分钟前、容器还是一小时前的）。

两处一起修：

1. `tools/_deploy.py::client()` 加退避重试（0/5/15/30 秒，四次）。放在**共用的连接函数**里，
   不在十几个调用点各写一份。
2. `tools/deploy_update.sh` 轮询间隔 15s → 30s，`seq 1 120` → `seq 1 60`：
   一次构建最多开 ~20 条连接而不是 120 条。

同一类账本轮已经付过两次（先是远程 DB 被我反复跑回归压垮，再是 SSH）：
**自动化的探测频率本身就是一种负载**，探测方要为它设上限。

### 知识库的现状，用数字说话

`tools/kb_bench.py` 的既有基线（18 题，k=6）：

| 指标 | 值 |
|---|---|
| recall@6 | **1.00** |
| MRR | **1.00** |
| precision@6 | 0.41 |

召回是满的，弱的是**精度**。这与本轮实测互相印证：「知识库明明有却没查到」
**不是检索问题**，是路由没走到知识库。所以本轮把力气花在
①路由（R1.5/R2）②文件清单精度（`rank_by_name`），而不是去动召回链路 ——
动它是在改一个已经 1.00 的指标。

（该基线的题集是 08-08 生成的，语料 08-13 重新入过库，金块 chunk_id 已失效，
需要重跑 `kb_bench.py generate` 才能对当前语料给出新数字。这条列为待办，
需要一个能登录的账号或 `DMSAI_KB_TOKEN`。）

### AX148 续：裸单号为什么还是出卡（两处，都值得记）

R1.5 判了 `Data`，`prepared_contract_ready` 也放行了，生产实测**仍然**出澄清卡。
带日志跑一次就看清了，是两层：

**① `ground()` 的静默拒绝（intent.rs:806）**

```rust
if self.route() == IntentRoute::Unknown || !self.ambiguities.is_empty() { return None; }
```

模型对 `HJXH-DXO2026081300138` 的回答是**完全正确**的：

```json
{"mode":"unknown","entity_mentions":[{"surface":"HJXH-DXO2026081300138","kind":"other"}],
 "ambiguities":["仅提供了疑似单据号或ID的字符串，未指明具体业务意图"]}
```

它诚实地说了「我不确定你要看什么」。系统把这份**格式完好、理解正确**的合同判成
`Invalid`＝「模型输出不合约」，再对用户说「意图解析结果未通过一致性校验」。
而这条拒绝是**静默**的 —— 外层只印一句「JSON 两次都不合约」，于是
「模型吐坏 JSON」与「模型理解对了但诚实存疑」在日志里长得一模一样。

**修**：把这两条搬进 `grounding_reject_reason`，给出名字
`mode-unknown` / `model-flagged-ambiguity`。结论不变（仍 fail-closed），但留下名字。
为这一条静默花了一小时。

**② 子问闸读合同、路由读裁决 —— 两者在我搬走决策那一刻就分叉了（ask.rs）**

```rust
let routed = prepared.routed_questions();      // ← 读**合同**
if !deterministic_fallback && routed.iter().any(|c| c.route != Data) {
    return Ok(prepared.clarification_result());
}
```

`routed_questions()` 对 `Invalid` 合同**恒返回一条 `route=Unknown` 的子问**。
于是 R1.5 明明把单号判成了问数点查，这道闸又把它退成澄清卡。

**修**：`&& !plan.deterministic` —— 确定性车道没有 typed 子问。

**教训**（本轮第三次付同一种账）：把一个决策搬到新地方时，**所有读旧决策的判据都是待查清单**。
`route()` 改读 `plan()` 之后，仓库里还有 `intent_attempt.routed_questions()` /
`is_data_executable()` 这些仍在读合同的判据 —— 它们不会报错，只会在某个入口悄悄给出旧答案。

### AX148 三续：把 `CZ` 单据族补上 + 「带引用的非答案」不许顶替卡片

修完前两层后生产实测：

| 单号 | 结果 |
|---|---|
| `HJXH-DXO2026081300138` | `direct-doc`，**6 行**，真查到单了 |
| `CZ202608131914` | 走了问数（router 全跑完），但四个成员逐个 miss |

`CZ` 这个前缀**从来没登记过单据族** —— `resolve_code` 返 `None`，business-lookup 接不了。
业主截图里那条错误 SQL 反而给了证据：`cb.balance_code AS 账余充值单号 FROM dms_ods.t_customer_balance`。

生产 `exec-sql` 逐列核实（不猜）：

```text
SELECT balance_code FROM dms_ods.t_customer_balance WHERE balance_code='CZ202608131914'
→ row_count=1
```

登记 `DocumentKind::CustomerBalance`：

- 形：`dated_serial(code, "CZ", 3, 12)` —— `CZ` + 8 位日期 + 流水。
  用 `dated_serial` 不用 `numeric`：后者会把任何 `CZ` 开头的长数字串都收进来。
- 源：**只登记数仓**（`BALANCE_DORIS`）。生产 MySQL 侧这张表没证明过，
  `header_policy` 里显式 `return None` —— 访问业务库前失败关闭。
- 行级权限：`Visibility::Customer`。这张表有 `customer_code`、**没有** manager 列 ——
  拿一个不存在的列裁决只会恒 false，等于把这一族永久关死。
  这也是 `Visibility::Customer` 的第一个生产消费者（此前挂着 `#[allow(dead_code)]`）。
- 明细表不登记：账余充值是单头单据，没有行明细。

### 另一处：有引用 ≠ 有答案

`unknown_route_kb_fallback` 此前只判 `citations.is_empty()`。而业主截图里那条
「该订单号未出现在任何资料中，无法查询其订单状态、商品明细或金额」**带 2 条引用** ——
模型一边说查不到、一边照样打角标，于是这句「查不到」顶掉了本该走问数的路。

加 `reads_as_not_found(markdown)`：只扫开头 160 字（「直接结论」那一段），
命中「未出现在任何资料 / 知识库里没有相关内容 / 无法查询 / …」即不顶替卡片。
只扫开头是有意的 —— 正文后段出现「未提及」是正常行文，拿它判非答案会误杀大量真答案。
判据同时进了兜底钉板（`f.contains("reads_as_not_found(")`）。

workspace 1923 绿。

## AX149（2026-08-14，业主四张截图：「你给出的信息完全都是无意义的信息」）

业主发来四张截图 + 一句话：「彻底解决知识库和问数的问题……以后你给出的答案类型不是
固定的，要结合数据让大模型来动态调整」。四张截图看着是四个毛病，追下去是**同一个**：

> 先分类 → 按类别选一个固定模板 → 把数据往槽里填。
> 分类错就全错；数据不合槽就出空壳；后置「安全」改写器还会**覆盖模型已经答对的答案**。

### 逐图对代码

**图1 单号 → 全是元信息。** `deep_api::primary_facts` 是一张 26 条中文别名白名单，
`primary_display` 另有 23 条列白名单。而列名的中文化由 `semantic::present_cn` 做 ——
**两套中文命名各说各话**（一个出「客户名称」，一个找「客户」），对不上的字段被静默丢弃，
卡上只剩恰好对上的三两个 + 「主表 t_sales_order」「明细表 t_sales_order_detail」。
表名是实现细节，却因为 `insert(0, ..)` 占着头卡最前两格。

**图2 问一家银行支行 → 一堆碎数字。** `knowledge::disclose_conflicting_numeric_claims`
用 `numbers()` 抠数字判「版本冲突」，把统一社会信用代码 `91430104MA7AMADH81` 切成
`91430104`/`7`/`81`。两份**互补**的开户信息被判成「同一问题的不同数值」，
于是整段替换掉模型本来答对的正文。图4 证明模型完全有能力答对 —— 是后置改写器把对的改坏了。

**图3 开户银行/银行账号两行的值互换。** `embed_service._p_docx` 按「这行有几个 tc」
拼表格行，不补 Word layout grid 的 `gridBefore/gridAfter`：某行晚起步一格，整行左移，
标签就跟邻行的值配上对。

**图4 客户名走了知识库。** 路由是「五选一，选中谁就只跑谁」；更要命的是 **HTTP 有自己
一套 `match route`**，`Knowledge` 直连 `kb_answer`、`Unknown` 直连
`unknown_route_kb_fallback`，完全绕过 agent 的编排。CLI 侧改好了、web 侧照旧。

**图5 三条断言全「未满足」仍报已完成。** 失败板块被 `.flatten()` 静默丢掉，
页面只是少一块，用户既不知道少了什么、也不知道剩下的数是不是完整的。

### 改了什么

**① 路由：两维 → 两臂并行。** 分类结果只决定「问数臂开不开自由 SQL」与「谁排前面」，
**不再决定谁不许跑**。agent + `/api/ask` + `/api/ask/stream` 三处全收口到
`hybrid::dual` / `ask_arms_payload`。

合成走 `AskResult.kb` 附加字段，**不套 compound 壳** —— 容器会把顶层
`sql/columns/rows/row_count/view` 全清空，实测 79 题回归会红 68 题。键名 `kb` 是既有协议
（混合问句早就手工塞 `v["kb"]`），前端零改。

**② 白名单一律改判据。** 26 条别名表 → 运维列黑名单（业务列一个不丢）；
知识库 7 条标题白名单 → 「是 markdown 标题且不含数字」；前端标题分桶白名单 → 语法判据
+ `other` 通用桶；表头中文词表 → GFM 真判据（下一行是分隔行）。
**白名单默认丢弃未知项**，黑名单默认展示 —— 这一条是本轮的主线。

**③ 同一事实多副本收敛。** order_status 16 档、customer_class、customer_type、
on_sale、frozen_state 五张码表进 `present_cn` 并播进 `meta.value_map`（展示侧码→名与
问句侧名→码同时通）；周报板块名三处字面量 → 一份常量 + 漂移判据；
毛利率判据后端窄/前端宽 → 统一成**词尾**判据。

**④ 两处「命中即整段丢弃 LLM 分析」的词表闸删掉。** `insight.rs` 三张无条件中文词黑名单
—— 词表里的词正是它自己 prompt 要求模型产出的词，而 prompt 的约束是*有条件*的。
中文数字不再一律判「不可核验」：`kernel::nl::time::cn_num` 归一后走与阿拉伯数字同一条路。

**⑤ 呈现层模型编排（业主最看重那条）。** 新增 `agent/src/view_compose.rs`：
确定性决策树退成裸表格那一档，让模型看真实列与样本行决定块的种类、顺序与标题。
**铁律：模型选列，代码算数** —— 模型只能给「列下标 + 聚合算子 + 标题」，
sum/avg/max/min/count/distinct 全在 Rust 里从原始行现算，标题禁数字。
模型那半失败时仍有 `deterministic_summary`（合计金额 / 记录数），零模型。

### 生产实测（release `20260814T111043Z-1880`，容器 CLI 直打）

改前 → 改后：

| 问句 | 改前 | 改后 |
|---|---|---|
| `HJXH-DXO2026081300138` | `need-intent` 0 行 | `direct-doc` **6 行**，客户/金额/数量/**状态=待备货**，无表名 |
| 农行重庆荣昌昌州支行 | `need-intent` 0 行 | 正确答出开户主体与账号，无碎数字表 |
| 线下-浏阳品元商贸 | 只有客户卡 | 客户卡 **+ 知识库半同屏** |
| `CZ202608131914` | 卡上「主表 dms_ods.…」「明细表 ''」 | 只剩业务字段，`receipt_date` → 到账日期 |

### 🔴 生产实测逼出来的三条真缺陷（推演推不出来）

**一、单号锁主源。** 「订单 HJXH-DXO2026081300138」返 0 行，而**裸单号**同一张单查得出来。
日志给出的根因既不是路由也不是改写：

```text
err=查询失败 [upload_390f2419-…] 上传数据源只允许访问 schema up_390f2419_… 里已登记的表
sql=SELECT … FROM t_sales_order WHERE sales_order_code = 'HJXH-DXO2026081300138'
```

「订单」二字把**向量选源**推到了某个用户上传的数据源上，单据 SQL 打进别人的上传 schema
当场失败、回落自由 SQL 返 0 行。**同一个单号，多两个字就查不到。**
单号的源是 `DocumentFamily` 注册表证明过的事实，不该交给最近邻猜。

**二、追问改写会把单号改坏。** 同一句话两次实测两个不同结果 —— 采样抖动。
`resolve_code` 是形状判据，差一个字符整条确定性路走不到，而槽位覆盖守卫看不见单号
（它不是槽位）。改写结果侧加一道：单号必须**逐字**活下来。

**三、0 行的裸表格被判成「有实质」。** `data_has_substance` 用 `!blocks.is_empty()`，
而确定性视图对空结果也兜底成 `[Table]`。于是一次 0 行的失败查询当了主答案，
把真正答出东西的资料半挤成侧栏。

### 自审 37 条：确认并修掉的高危

五维度对抗式复审（正确性 / 回归 / 安全 / 简化 / 判据覆盖），逐条反驳式核实。站住的部分里最重的：

- **`deterministic_fallback` 其实没关死自由 SQL**：`members.retain(|m| m.route() != "llm")`
  只摘掉 Router 末位的 `LlmAnswerer`，而 `direct-doc` 成员**内部**的 ODS 推导本身就是
  一次 Precise 模型自由写 SQL 并执行。两臂并行让资料问句的问数臂也跑起来之后这条路就漏了。
- **编造数字的两个洞**：标识符判据与 `business_values` 不同源（一个要 `len>=4`），
  原文写「机型 B7」、模型写「机型 A1」时两道闸都放行；带千分位的长数字
  `810,000,297,001,000,001` 按非字母数字切开每段只有 3 位，一个逗号绕开整道闸。
- **`req.space_id` 被 `KbArm { space: None }` 写死忽略**：用户选定的知识空间静默失效。
- **深度报告每个板块子问都多打一次知识库**（产物只取 columns/rows/sql，资料半整份丢弃）。
- **问数臂的权限类失败被降级成一句 warn**：用户拿到 200 + 一份资料答案，而他其实没有权限。

一条被核实**驳回**并给出更好改法的：我原本打算无条件剥千分位再切词，
核实方指出那会把相邻标识符粘成一个 token，源侧分两处写就比对不上、**真句子被整句删掉**
—— 改用 `numbers()` 逐段扫描补收（它自带千分位归一，不跨分隔符粘连）。

### 记一笔工具账

- 部署脚本第 4 步（原子切换 + 重启）的客户端连接断过两次，退出码还是 0 ——
  服务端命令其实执行完了，是客户端在等的时候断的。脚本自己在第 3 步注释里警告过这个形态，
  第 4 步没设防。判断「部署到底成没成」只能看服务端：`readlink -f app` 与容器 `StartedAt`。
- 生产的解析服务是**宿主 venv 里的 `embed_service.py`**（systemd `dms-ai-embed`），
  不是 parser 容器；venv 里已是 python-docx 1.2.0，所以 docx 修复直接生效。
  但 systemd 单元与一个 2026-08-12 起的**手工进程**在抢 8078，单元一直
  `activating (auto-restart)` 失败刷屏 —— 与本轮无关，但该收拾。

回归：`cargo test --workspace` 1950 绿；web 60 绿；`npm run build` 通过。

### AX149 续：最后一层 —— 金额列一直没被当成金额列

单据卡在两臂、别名表、码表全修好之后**仍然没有合计**。呈现编排的日志说
「未产出可用块 → 走确定性兜底」，而确定性兜底也返回空。把生产的 `view.columns`
原样打出来才看见：

```text
商品金额  role=category  sem=None
金额      role=category  sem=None
实发数量  role=category  sem=None
```

`ColumnSpec` 的 role/semantic 是**按列名推**的（`present::infer_*`），而
`present_cn::apply` 只改列名、**不重算规格**。数仓明细的 `goods_amount` 被译成
「商品金额」之后，规格还停在英文名推出来的那一份 —— 英文名里没有「金额」二字。

后果不止编排这一处：前端按 `role=metric` 决定右对齐、按 `semantic=money` 决定 ¥ 与
万/亿压缩，这两件事对**所有原名是英文的金额列一直是坏的**（列名本来就是中文的那些碰巧对）。
这条在业主的截图里看不出来 —— 截图上那张卡的字段恰好都有中文别名。

修法一行：`present::col_spec` 提 pub，`apply` 同步列名时一并重算。原名已是中文时重算等于原样。

**教训**：把一份数据的「名字」和「按名字推出来的属性」放在两个地方维护，
改了名字不改属性，就是本轮反复在拆的那类缺陷 —— 只不过这次它藏在我自己刚建的
呈现编排后面，直到把生产的中间态原样打出来才现形。

回归：`cargo test --workspace` 1951 绿。

### AX149 三续：79 题回归打出来的 12 条真回归（4 题冒烟一条都没抓到）

两臂并行部署到生产、四题冒烟全绿之后，跑仓内 79 题：**80 项 / 通过 65 / 失败 15**。
其中 12 条是两臂改造引入的，**全是同一种病**：

> 资料臂一有带角标的命中，就把问数臂**刻意产出的那张卡**顶替掉。

四族，逐条：

| 族 | 题 | 现象 | 根因 |
|---|---|---|---|
| 覆盖闸读不懂模板 | A07 A11 D03 E14 | `route=knowledge≠direct-agg` | 模板里 `DATE_ADD(…, INTERVAL 1 MONTH)` 让 sqlparser 读不懂 → 记进 `conflicts` → 硬阻断 → 一条**代码写死的正确模板**被丢、回落自由 SQL → 出反问卡 → 资料臂顶替 |
| 「不可计算」卡被顶替 | E05 E08 E15 | `route=knowledge≠direct-doc` | 那张卡明确说「这个事实数仓里没有」，`land()` 专门为它开了不过覆盖闸的豁免；我却判它「没实质」 |
| 裸实体名 | C06 C08 | `route=knowledge≠entity-card` | `entity_card_compatible` 要求合同 `route()==Data`，而 fast 模型把裸公司名判成 knowledge 是常事 |
| 红线拦截 | H01 H02 H03 | `route=knowledge≠need-intent` | 「删除订单」「drop 表」的拦截卡被知识库答案顶替 —— 拦截在日志里成立，**用户看到的是一个答案，不是一次拒绝** |

余下 3 条（B01T / B01W / E10）是开工前就在挂账清单里的已知项，与本轮无关。

#### 三条值得记的

**一、「我答不了」不是一种说法，是四种，各有各的处置。**
反问卡（我需要你补充限定）该让位给资料答案 —— 业主的「中国农业银行…昌州支行」正是这一档；
「不可计算」卡（这个事实数仓里没有）是**明确结论**，让位就等于让知识库去做那件
「找一个名字像的字段替代」的事，只不过换了个执行者；
红线拦截卡更不能让位 —— 让位之后拦截只存在于日志里。
第一版 `data_has_substance` 把这四种混成一句 `route == NEED_INTENT`，代价是 6 条红。

**二、「读不懂」不是「删了限定」。**
对 LLM 生成的 SQL，闸门读不懂就该硬拦（不可信）；对代码写死的模板恰恰相反 ——
丢模板换自由 SQL 是**放宽**不是收紧。`land()` 上面那段注释本来就是这么写的，
只是判据没跟上（`sql:coverage-unverifiable` 被放进了 `conflicts` 而不是 `unverifiable`）。

**三、4 题冒烟一条都没抓到。**
冒烟题挑的是业主截图里那四句，它们恰好都在「资料臂该赢」的那一档。
79 题里的红全在另一档 —— 而那一档正是这个仓最在意的（fail-closed 与红线）。
**冒烟能证明修好了什么，证明不了没弄坏什么。**

#### 资料臂带预算

同一份回归还打出一个体验回归：裸客户名的实体卡整轮 39273ms，而实体卡本身 1 秒出 ——
`tokio::join!` 让总耗时恒等于**较慢**的一路，而两臂快慢差一个数量级（1 秒 vs 30 秒）。
改成 `race_arms`：先等问数臂，它有实质内容时资料臂只再给 8 秒（加分项），
没有实质内容时给 45 秒（那一档它就是答案本身）。

#### 顺带查清（不改）：B01T 的失败不在剥词，在探针

`B01T-客户名带类别前缀`（「客户董会琴本月的销售额」）在挂账清单里的记法是
「领头类别词『客户』不是名称一部分，剥掉后探针才探得到」。本轮复核：
`customer_name_fragment("客户董会琴本月的销售额")` **已经**返回 `Some("董会琴")`
（`fastpath_tests` 里那条断言现在就是绿的），剥词那一半早就修好了。

所以现在失败的是**下游探针**：`entity_resolver::resolve_customer(cx, "董会琴", Auto)`
在生产上没解出唯一绑定（NotFound 或 Ambiguous），于是 `customer_filtered_sales`
返回 `None`、整题回落到不带该客户谓词的销售模板 —— 答出来的是**全量**销售额。
这条比「判据脆」严重：数字看着合理但口径是错的。修它需要先在生产上看
`resolve_customer` 对「董会琴」到底解出了什么，本轮没做，挂账时把证据一并留在这里。

### AX149 四续：一大类问句被**最近邻改投**到用户上传的表格上

回归重跑后剩下的 A07/A11/D03/E14 全是同一条，而它比前面几条都深。
「本月订单数」的失败链有**三层伪装**：

```text
INFO  图快路径不接 reason="source-not-warehouse"
DEBUG 确定性 SQL 未过红线闸门 → 回落下一个成员 route=direct-agg
      err=sql parser error: Expected: an identifier after AS, found: `
WARN  归一结果未过安全校验 → 放弃重试，原卡照出
```

① 向量最近邻把「本月订单数」这样一句**纯业务问句**路由到了某个用户上传的数据源
（`upload_…`，PostgreSQL）；
② 模板 SQL 里的反引号别名 `` AS `订单数` `` 在 PG 方言下解析不了 → 红线闸门拒 ——
而这条拒绝**只打 `debug`**，默认日志里一个字都看不到；
③ 回落自由 SQL，自由 SQL 也答不出 → 反问卡 → （两臂改造后）资料臂顶替成 `route=knowledge`。

用户看到的是「请补充明确的对象、指标和时间」——而这道题有一份**代码写死的模板**
本来就答得出来。

**修法**：把上一轮的「单号锁主源」推广成
「`resolve_document` 或 `try_direct_for` 任一命中 ⇒ 锁主源」。
模板是**按主源写的**（表名、方言、别名都是），让最近邻把它改投到别人上传的表格上，
从定义上就不成立。用户想查自己上传的数据时显式选源即可。

**这条的严重性被三件事掩盖了**：拒绝只打 debug；回落是「静默」设计；
最终症状是一张看起来很正常的反问卡。三层加起来，一大类问句退化成反问卡而无人知道 ——
它在两臂改造之前就存在，只是那时的症状是「反问卡」而不是「一段资料答案」。

### AX149 五续：回归三轮的收敛曲线与最后三条

同一份 79 题、同一台生产，三轮：

| 轮次 | 带的修复 | 结果 |
|---|---|---|
| 1 | 两臂并行首版 | 65 / 15 |
| 2 | 覆盖闸 + 不可计算卡 + 裸实体名 + 红线不跑资料臂 | 73 / 7 |
| 3 | 确定性可答锁主源 | 74 / 6 |

第 3 轮剩下的 6 条里有 3 条是**同一族的最后三种形**，都在本轮修掉：

**C06「商品分类烤肠类」**：`bare_entity_mention` 只认纯裸名，而这是「实体前缀 + 名字」。
前缀（`商品分类`/`客户名称`/`商品编码`…）与领头类别词同理，是「在说哪一类实体」，
不是名字的一部分 —— 由 `entity::contract_allows` 剥掉再判。

**C08「线下-广东横琴雨燕供应链管理有限公司」**：模型抽表面词时把库内名称的渠道前缀
切掉了（抽成「广东横琴雨燕供应链管理有限公司」），问句里剩一个「线下-」。
它不是「另一个问题」，只是同一个名字的前缀 —— 剩余里容忍渠道/类别限定词与标点。

**D03「本月销售额按省区」**：这条最值得记。它红成 `route=knowledge` 是因为
**反问卡让位给了资料答案** —— 而那条让位规则是为业主的「中国农业银行…昌州支行」加的。
两者的差别在**裁决**：后者判 `Knowledge`/`Unknown`（问数臂本来就答不了），
前者判 `Data`。问数臂答不出一个数据问题时，用户要的是**为什么答不出**，
不是一段讲制度的资料。**拿资料答案去顶一个数据问题，形状上像答了，实质上换了个问题回答。**

余下 3 条 B01T / B01W / E10 是开工前就在挂账清单里的已知项，与本轮无关。

## AX150（2026-08-14，回归收口：65 → 80/80，四条根因都不在「答得对不对」上）

AX149 收工时是 65/80。本轮把余下 15 条逐条打到底，最后一次全量 **80/80**。
轨迹：65 → 73 → 74 → 75 → 78 → **80**。

四条根因有一个共同形状：**系统拿一次 LLM 采样当事实**，或**拿「有没有行」当「答没答对」**。
它们都不是「SQL 写错了」那类错误 —— 每一条的答案本身要么已经对了，要么本可以对。

### ① 形态自证：一句话是不是实体名，与模型这轮抽没抽出来无关

生产直打，同一句话连打三次：

```text
线下-广东横琴雨燕供应链管理有限公司 → entity-card / entity-card / llm+repair
商品分类烤肠类                     → no-topic  / no-topic  / entity-card
```

同一份代码两种答案，差别只在 fast 那一轮有没有抽出实体表面词。合同闸
（`bare_entity_mention` 要求恰好一个实体表面词）于是变成一个掷骰子的开关。

**判据要分层**：有形态证据的（显式实体前缀 `商品分类…` / 裸型号 / 渠道前缀 `线下-` /
公司类后缀）不看合同 —— 那是问句的**形态属性**；没有形态证据的短句仍要合同点头，
因为 `parse_entity` 也放行「退货政策」「报销流程」「会员积分规则」（实测），
那一族一旦不看合同，每条知识库问句都要白付五路主档探针。

顺手删掉两处为同一件事打的补丁（`QUALIFIERS` 词表、前缀两形都试）——
形态证据一到位它们就是同一件事修两遍。修完 C06/C08 各 3 次全绿。

### ② 惩罚诚实：模型如实报一句歧义，整份合同被判废票

「现在库存量是多少」的 fast 回包是**完美**的：

```json
{"mode":"data","metrics":["库存量"],"time":{"surface":"现在"},
 "ambiguities":["未指定具体的商品、仓库或组织范围，'库存量'指代不明"]}
```

只多了最后一句。而 `route()` 见 `ambiguities` 非空即返 `Unknown` → `ground()` 返 `None`
→ `IntentAttempt::Invalid`：自由 SQL 关、语义缓存关、知识库路由拿不到、混合问句拆不开。
**提示词第 4 条明写「拿不准写入 ambiguities」，系统却因此判它废票。**
而模型说不说这一句本身带采样抖动 —— 这就又是一个「同题不同答」的源头。

拆开成三件事：
- **选路**只看证据（mode + 槽位），歧义不参与
- **fail-closed** 留在 `is_data_executable`：有歧义就不开自由 SQL / 语义缓存（逐字未变）
- **收据**记 `ambiguity:*`，且从 `conflicts` 移到 `unverifiable` —— `conflicts` 会
  `blocking()`，那会把一条**答对的**确定性模板整份丢掉

连带拆掉一个字段两种含义：`project()` 内部三档 fail-closed 原先是往 `ambiguities`
塞一句话、借「见歧义即废票」处决的，改成 `Option<IntentV1>`，`None` = 投影不成立。

### ③ chrono 的 `%Y` 会跳过前导空白

`intent_time_surface` 用 10 字节滑窗找 ISO 日期，而 `" 2026-08-1"` 也解析成功，
于是窗口整体左移一位：

```text
"2026-08-10 至 2026-08-11 销售额"       → "2026-08-10 至 2026-08-1"   （截短）
"山东省 2026-08-10 至 2026-08-11 销售额" → " 2026-08-10"               （只剩半截）
```

后果：**所有显式日期区间问句**的时间槽永远兑现不了，`ops_caliber` 的「已消化限定」
记账跟着错。修法是窗口两头都必须是数字 —— 日期形态本来就不含空白。

### ④ 两臂仲裁只问「有没有行」，从不问「用户要的是什么」

```text
市场费用的报销政策是什么
  → route=direct-agg，SQL 是 20 多个费用列的 SUM
  → 日志：资料臂超出预算 → 只用问数半 budget=8s
```

库里明明有《市场费用项适用场景及核销标准说明》《操作手册-DMS市场费用报销核销》。
「市场费用」恰好是**已登记指标**，于是问数臂 1 秒出一份合计 →
资料臂被降成 8 秒加分项 → 检索+生成超时 → 用户问政策，拿到一个金额。

预算与主体身份都必须跟着**裁决**走：判 Knowledge 且这条路是**确定性规则**定的
（R1 要文件 / R2 纯资料问句）时，资料臂就是答案本身。
判据必须收紧到 `plan.deterministic` —— 只看 `route()` 会把 R3「听合同的」也算进来，
而裸客户名被 fast 判成 knowledge 是常事，那一档把资料半当主体就等于把实体卡单臂化
（正是本轮 ① 刚修好的病）。

### 两条改了**期望**而不是改代码的

- **B01T「客户董会琴本月的销售额」**：期望里写着 `INSTR`，而现在的实现是先探主档
  解析出 `customer_code=180135`、再按 `storecode` 精确过滤（生产实测 72028）——
  比按名称模糊匹配更准。期望是修好之前写的。
- **E10 的 `coverage_status`**：维持 `blocked`。让确定性模板反过来把合同「升级」成
  complete，等于用我们自己的判断盖掉模型报告的歧义，下一次真歧义就没人报了。
  这是第三次复核仍维持的裁决。

### 一条方法论

这四条没有一条是靠读代码想出来的，**全部**来自生产直打：同一句话连打三次看方差、
把回包原文捞出来看、去库里查文档到底在不在。「路由对不对」「SQL 对不对」这两层
早就有测试守着；漏在测试之外的是**同一句话两次不同答**，而它只有连打才现形。

### 补：图4/图5 深度报告「板块 1/3（2 失败）」的根因（2026-08-15）

业主截图里那个红框挂了几轮。这次不猜，直接用 `why-not-compose` 逐条问：

```text
今年退款额按月份     → ③ 指标维度白名单拒绝：「退款额」尚未审定可按「月份」组合
今年各战区退款额排行 → ③ 指标维度白名单拒绝：「退款额」尚未审定可按「战区」组合
```

**月份那条是白冤枉的**：装配器早就有「指标自绑时间维度」这条支路
（`metric_bound_time_dim` → `bind_time_dimension`），`月份` 的声明虽然绑在销售事实上，
装配时会重绑到指标自己的 `after_sales_time`，不跨表 JOIN。挡住它的只是
`METRIC_POLICIES` 里那个空数组。加上 `月份` 之后（售后单数一并）：

```sql
SELECT DATE_FORMAT(b0.after_sales_time,'%Y-%m') AS `月份`, SUM(b0.refund_amount) AS `退款额`
FROM dms_ods.t_after_sales_order_header b0
WHERE b0.deleted_flag = 0 AND YEAR(b0.after_sales_time) = YEAR(CURDATE())
GROUP BY DATE_FORMAT(b0.after_sales_time,'%Y-%m')
```

**另外两条不加，且不是偷懒**：
- `战区` 取自 `dws_off_offline_sale_dfn.war_zone`，ODS 售后单头既没有这一列、
  也没有到销售事实的已验收 join。写进白名单只是把「装配不出来」换个地方失败，
  业主看到的仍是空板块 —— 白名单的含义是「已审定」，不是「试试看」。
- `月度退款占比趋势` 的 `退款占比` 是跨两表的比值（分子 ODS 售后、分母 DWS 销售事实），
  聚合含子查询，装配器按既有判据（`sql_has_keyword(SELECT)`）整条拒。

三条断言里现在诚实兑现一条。要兑现另外两条，需要的是**数据侧的接入**
（售后单头补战区归属，或建立到销售事实的已验收 join），不是再调一次代码判据。

顺带一条运维事实：`meta.metric` 每次进程启动都由 `seed_defs` 重新播种 ——
手工 UPDATE 会被下一次 CLI 调用悄悄覆盖回去（这次就是这么被绕了一圈的）。
白名单是**代码资产**，改它只能改代码。

### 补二：加一个「今年」就把政策问句翻成金额（2026-08-15）

```text
市场费用的报销政策是什么      → 资料答案（对）
今年市场费用的报销政策是什么  → direct-agg 一行金额，资料半整个没上（错）
```

R2（纯资料问句）的判据是「有文档名词 **且** 合同一个可度量槽位都没抽到」，
而 `has_measurable_slots` 把**时间**也算成可度量。根级的时间词往往只是在说
**哪一版**政策，不是数据诉求。

这一条改了两刀才真修好，第二刀才是根：

1. 先去掉根级的 `time.is_some()` —— 部署后生产**照旧** `direct-agg`，
   日志 `budget=8s` 说明 R2 仍然没触发。
2. 真因在子任务分支：它不分 mode，把**知识类**子任务上的时间槽也算成可度量。
   这句被模型劈成两个 `knowledge` 子任务、其中一个带「今年」。
   时间槽在子任务里算数，靠的正是那个 `mode: data` 声明 —— 那就只认它。

第一刀没验证就以为修好了，是本轮唯一一次「改完不看生产」——
而它恰好就是错的。日志里那行 `budget=8s` 是唯一的证据。

## AX151（2026-08-15，业主重申「彻底解决」：全方位深度审查 —— 答案形态、两臂融合、准确性）

业主把同一段话又发了一遍。上一轮（AX150）是**点修**：一条一条追生产日志。
这一轮改成**审查**：先把「答案形态是谁定的」「两臂怎么合的」「准确性有没有量过」
这三件事摸清楚，再动手。

### 一、答案形态：三处白名单夹一个模板

摸出来的现状比预想更糟 —— **同一件事被钉死在三层**：

| 层 | 怎么钉的 |
|---|---|
| 知识库提示词 | 点名四个栏目（直接结论/关键要点/操作步骤/对比说明/版本与差异），模型每次照抄 |
| 前端 `headingClass` | 五条**中文标题正则**上色；模型换个说法叫「费用标准」就掉回默认样式 |
| 深度解读 `insight.rs` | 三份提示词写「**严格按下面结构**」+ 固定三节，连单测都逐字钉着「## 异常与机会」 |

中间的载体还是**一坨 markdown**：前端只能整块渲染，模型再怎么「动态调整」也传不出来。

四刀一起改：

1. `kernel::answer::split_sections`：markdown → 分节。标题**用模型自己写的**，
   形态（prose / bullets / steps / table）由**内容**判 —— 围栏里的 `#`/`|` 不算结构，
   一根竖线的散文不算表。纯函数 + 5 条单测。
2. `AnswerBody::Text` 增 `sections`（空数组不上线）。唯一生产者是 `Answer::text`，
   与 markdown 不可能漂。
3. 前端按 `shape` 排版（要点两栏、步骤单栏纵向、表格独立卡），删掉那五条中文正则。
4. 三份提示词只留一个收敛点（第一节结论），其后写几节、叫什么由内容定。
   **硬规则一条没动**：字数上限、信任边界、禁止把占比推断成资源倾斜、
   禁止编造合同/授信/物流原因、数字必须与数据一致。

生产直打验收（同一天四条不同问句）：

```text
市场费用的报销政策是什么  → prose 直接结论 / bullets 审批与决策机制 /
                             bullets 系统操作与票据要求 / table 不同场景的报销与核销方式
客户打款 退款政策         → prose / steps 错误打款快速退款（继续合作） /
                             table 结束合作退款（客户退出） / bullets 打款规范
出差住宿和市内打车上限    → prose / bullets 差旅住宿费规定 / bullets 市内交通费（打车）规定
中国农业银行…昌州支行     → prose / table 账户详细信息 / table 版本与差异
```

小标题全是**这份资料实际讲的事**，四条问句四种结构。这才叫「答案类型不是固定的」。

### 二、两臂融合：并排 ≠ 合成

`hybrid_summary` 的提示词原文是「先说数据结论，再点出资料里的相关规定/口径」——
产出的是两段并排的结论，而把它们**对起来**正是用户要的那件事。改成真合成：
一句直接结论；说清数据落在规定的哪一档上；**两侧对不上时必须明说、不许抹平**。

顺着这条链又挖出两个「模型看不见」的洞：

- **示例 ID 被照抄**。`AnswerContract::instruction()` 里写着「例如…[Q:F001]」，
  而真实 ID 形如 `DATA:F001`/`KB:F002`。模型照抄示例 → 未知引用 → 重试 → 仍不过 →
  **整段 AI 综合直接没有**（生产日志原文：`claims=["未知事实引用 [Q:F001]"]`）。
  示例改成占位词 `[事实ID]` + 明写「原样抄下方清单里的那一个」。
  这条 instruction 是所有调用方共用的，一处修三条链一起好。
- **口径不在合同里**。解读提示词要求「用一句说清这个数怎么算出来的」，
  而事实合同规定「没有对应事实就省略该断言」，口径此前只在**素材**里。
  两条规矩夹住，模型只能把数字复述一遍。口径 `push_text` 成 CALIBER 事实域。
- **环比对模型不可见**。KPI 卡上明明有 `较上月 +13.4%`，它只活在
  `AskResult.comparisons` / `view.blocks[..].delta` 里，从没进过任何 prompt。
  摊成 DELTA 表进合同。

### 三、数据侧的动态编排：最该编排的一档被判据挡在门外

生产直打量了一遍确定性树实际给出的形状：

```text
本月销售额        → ['kpis']          （单行聚合，无可编排）
今年退款额按月份  → ['chart','table'] （规则判得比模型稳，不抢）
本月销售额按省区  → ['chart','table']
本月销售额按客户  → ['table']  200 行  ← 最该编排的
昨天销售订单明细  → ['table']  200 行  ← 最该编排的
```

那两张 200 行裸表撞行上限 `truncated=true`，被 `view_compose` **整条拒**。
拒的理由（「合计」其实是小计）只对 KPI 成立，不对图表成立。改成照样编排 +
`honest_under_truncation`：KPI 一律去掉、图表标题补「（前 N 行）」（补一次不叠加）。
**说实话这件事由代码保证，不指望模型自觉。**

回归 E03 立刻抓到我漏的一半：「昨天订单明细」被编排成分布图。
「前 200 行」对两族含义不同 —— 聚合行的前 200 组是**头部**，明细行的前 200 行只是
被行上限切出来的**一截**，拿它画分布图，标题写「（前 200 行）」也救不回来。
判据用 `Role::Id`（确定性树判「这是明细」用的就是它）。

### 四、准确性：量了两件事

**知识库检索**（题目由 104 份文档的标题反推，命中判据 = 引用里出现该文档）：

```text
合计 命中 12 / 未命中 0 / 出错 0（共 12 题），全部 route=knowledge
```

**问数静默丢限定**（每题给出「答了就必须出现在 SQL 里的证据」）—— 抓到一条真缺陷：

```text
本月华东区销售额 → direct-agg，AND storecode = '181806'，答 0
```

181806 是 `线下-福建云通供应链有限公司(华东区）`。DWS 的 region 值只有
广西省区/西北大区/川渝藏大区… **没有华东**，地域路正确地没接；接住它的是客户模糊
探针 —— `LIKE '%华东区%'` 唯一命中了那家名字**括号里**带地域标注的客户。
于是「华东区本月销售额」变成「那一家客户本月销售额」，答 0，
用户读到的是「华东区本月没销售」。

地理名是**封闭的参考数据**，不是行为模板：大区名落到客户名判据上，唯一正确的结果是
不认。修完之后它诚实回「不可计算 · 未确认限定「华东区」」。

### 已知缺口（查清了，这轮不改）

- `西北大区` / `直营` / `线下私域` / `海外事业部` 是库里**真实存在**的 region 值，
  但 `province_region_qualifier` 只认「省名 + 省区/战区」，非省名一律 `Err` 走 LLM。
  实测 LLM 那条答案是对的（`war_zone` 与 `region` 同值，4,793,065.80），
  只是慢且收据降 review。要根治得把 region 的实际取值登记进 `meta.value_map`，
  那是发现型任务，不在本轮。
- 纯数据问句仍会挂一段「知识库资料中未包含关于…的规定」的综合。不算错（两侧确实
  对不上），但对「本月订单数」这种问句是噪声。要治得先能判「资料半跟这个问题相不相关」。

## AX152（2026-08-15，同方向续：一条「正确答案被自己丢掉」的链，和一颗生产 panic）

### 一、覆盖闸把**答对的**确定性结果整份丢掉，回落自由 SQL

线索来自上一轮记的缺口：「本月西北大区销售额」走 llm+repair。以为是地域词表不全，
补了 `DIRECT_REGION_VALUES`（region 的真实取值里有 西北大区/川渝藏大区/海外事业部/线下私域，
而解析器只认「省名 + 省区/战区」）。部署后**照旧 llm+repair**。

于是给覆盖闸的回落日志补上证据（这仓自己的纪律：拒绝的理由必须留下来），一步定位：

```text
确定性路径未证明结构化意图覆盖 → 回落下一成员
  route=direct-agg
  coverage=CoverageReport { missing: ["time:本月"], unverifiable: ["region:西北大区"] }
  evidence=ExecutionEvidence { resolved: [], comparison_count: 2, detail: true }
  sql=SELECT COALESCE(SUM(sf.amount),0) … FROM sales_dw.dws_off_offline_sale_dfn …
```

`resolved: []` 而 SQL 完全正确。真因：`build_dimension_value_hit`（探库确认「西北大区」
真是 region 的成员值，再按它装配）写的是 `intent_evidence: Default::default()` ——
**一个槽位都不声明**。覆盖闸只能判它什么都没兑现，于是丢掉正确结果、回落自由 SQL。

与之前库存模板那条（`stock_snapshot` 不自报指标）同一个病：
**模板不自报，闸门就当它没做。** 三个槽位都是这里已经算出来的确定事实。

修完：`本月西北大区销售额` → direct-agg **verified** 4,793,065.80（此前 llm+repair）。

顺带发现探针本就比静态词表强：它读真数据，「本月直营销售额」（44,501,005.70）、
「本月零食很忙地配销售额」（181,436.26）都由它接住。静态表只服务拿不到探针的两个模板
（小程序 / ODS 订单），陈旧只会 fail-closed。互指注释已加。

### 二、同一条链的第二段：模型未必把地域词归成地域

`本月直营销售额` 答对了却降 review：合同把它写成 `filters:[{name:"渠道类型", value:"直营"}]`，
而 `filter_columns("渠道类型")` 认不出这个名字 → 恒判 unverifiable。
证据的语义本就是「这个表面词已兑现进 SQL」，不是「列名判对了」——同一个值再按 `Filter`
报一次。修完两题都是 `verified` + `issues: []`。

### 三、回归题集里**根本没有这一族**

reg11 / reg12 两轮全量日志里「未证明结构化意图覆盖 → 回落」**零次命中**。
不是没有 bug，是题集里没有这一族问句 —— 洞因此活了很久。补两条钉死：

- **R01-大区成员值直查**：钉 direct-agg + SQL 含真实成员值，不许出现「不可计算」或 `storecode`；
- **R02-未登记大区诚实拒答**：钉「不可计算·未确认限定」卡，不许出现 `storecode` /
  `SUM(sf.amount)` —— 地域词不许变成客户过滤、也不许悄悄答成全国。

### 四、一颗生产 panic：debug_assert 在生产是活的

reg13 的 C08 报「执行错误：进程非 0 退出」，stderr 只剩 `panicked at crates/agent`。

`docker/server/Dockerfile` 用 `cargo build`（debug）——注释里写着是**有意取舍**
（本机/CI 迭代优先构建快）。代价此前没人写下来：`debug_assert!` 在生产是活的，
**一条断言失败 = 整个请求崩掉**。而被打破的那条（「中文数字归一必须等字节长」）
吃的是**模型产物** —— 某句话就能把它打破。

两处都改成可降级分支（塞不下就当没换算 / 总长对不上就整份退回原串 + warn），
安全方向是**少归一**，不是崩。Dockerfile 补上纪律：
**模型产物驱动的不变量一律不许用 debug_assert**；内部结构不变量仍可用。

### 五、两臂综合不再挂噪声

`本月订单数` 的综合原文是「本月订单数为 10500，知识库资料中未包含关于本月订单数的
具体规定或标准。」——前半句 KPI 卡上有，后半句等于没说。
判据现成：事实引用 ID 本来就解析着，一条 `KB:` 引用都没有 = 模型只就数据侧说了话。
`cites_namespace` 判**未剥引用的原文**（`validate` 成功后 `[ID]` 已被移除）。
修完该题 `insight` 为空 —— 没有可综合的东西时就不硬凑一段。

### 补：同一轮里被生产直打逼出来的三条「谓词认得、别处不认得」

修完覆盖闸那条之后继续打 trap 题，又抓到同一形状的三处 —— 都是
**一条链上某一环不认得，整题白拒或落自由 SQL**：

| 问句 | 现象 | 真因 |
|---|---|---|
| 上个季度销售额 | 「不可计算 · 未能识别的限定「上」」 | `time_predicate` 认得季度窗口，`time_phrase_of` 对季度族返 None；且残留守卫**从不剥时间词**（靠虚词表里恰好有「本」「今」，「上」不在表里） |
| 本月每单平均金额 | need-intent | 客单价的说法在 seed_defs 里抄了三遍（+ 测试段第四遍），这一族说法一个都没登记 |
| 本月直营销售额 | 答对了却降 review | 合同把地域词归成 `filters:{name:"渠道类型"}`，`filter_columns` 认不出这个名字 → 恒判 unverifiable |

三处的修法都指向同一条纪律：**一个词能不能兑现，链路上每一环都得说同一句话**。

- 季度族补进时间表面词表，**故意不收「当季度」**（它有表面词却没谓词 ——
  收了就是「消化掉却没兑现」，静默丢限定）。新判据钉死：表面词表里每一条都必须有谓词。
- 已识别的时间表面词并进消化词，且只能用**词级**的 `time_phrase_of` ——
  `intent_time_surface` 带整句兜底，拿它当消化词会把「长沙」这类真限定一起吞掉。
  判据两面都钉：时间词该被消化、真限定一个都不许被顺手吞掉。
- 客单价别名收成模块级常量（四处共用），补「每单平均金额 / 单均金额 / 平均每单金额」；
  **不收「平均订单金额」**（整段含另一个已登记指标名，子串匹配会一句话命中两个指标）。
  源码扫描判据从「逐字钉数组」改成「引的是那份常量 + 常量里那几个说法都在」。

验收（生产直打）：

```text
上个季度销售额   → direct-agg verified 475,083,372.39   （此前「不可计算」）
本季度销售额     → direct-agg verified 311,076,775.72   （此前「不可计算」）
本月每单平均金额 → direct-agg 11,318.33                 （此前 need-intent）
本月直营销售额   → direct-agg verified issues:[]         （此前 review）
长沙本月销售额   → 「不可计算 · 未确认限定「长沙」」     （该拒的照旧拒）
```

## AX153（2026-08-15，12 路并行猎捕：一次抓出 45 条确认缺陷，先修错答）

业主要求加快。改用**工作流并行**：12 个问句族同时直打生产 CLI（时间口径/地域组织/
客户实体/商品分类/排行极值/同环比/库存售后/财务费用/知识库政策/混合问句/单据点查/权限），
每条发现再交给独立 agent **对抗式复验**（至少打两次、拿 exec-sql 对拍、拿不准判 refuted）。

产出：待验 60+ 条 → 复验确认 **45 条**。本节只记已修的，其余在下方挂账。

### 一、倍数级错答（全部 trust=verified，用户无从察觉）

| 问句 | 答成 | 真值 | 倍数 |
|---|---|---|---|
| 海南省本月销售额 | 5,408,303（广东省区） | 460,720.60 | 11.7× |
| 上海市本月销售额 | 浙江省区合计 | 409,224.41 | 3.8× |
| 西藏本月销售额 | 4,197,616（川渝藏大区） | 0 | 凭空 |
| 新疆本月销售额 | 西北大区合计 | 1,108,741.20 | 4.3× |
| 180135本月销售额 | 634,000,000（全公司） | 72,028.00 | **8800×** |
| 湖南省区市场费用 | 104,030,004（全国） | ~111 万 | **94×** |

四条根因，各修各的：

1. **行政省 ≠ 销售省区**。`region` 是销售组织口径、与行政省**多对一**（广东省区含海南、
   浙江省区含上海、川渝藏大区含川渝藏）。事实表另有 `state` 列（38 个官方全称）。
   映射非 1:1 的省改用 `INSTR(state, '<短名>')`；1:1 的（山东→山东省区）**原样不动**
   —— 那是 2026-08-11 业务裁决的口径，实测两侧同值，B01W 钉的也是那个形态。
2. **纯数字客户编码抽到了没用上**。收据里明写 `filter:客户编码=180135`，SQL 里一个
   storecode 都没有。`t_customer.customer_code` 全部 6 位（4041 行无例外），
   问句里恰好一段 6 位数字时落成过滤；不探库 —— 探不到就是 0 行，走既有的
   「该条件下没有数据」出口，比悄悄答成全公司总额诚实得多。
3. **客户编码末位被当成月份**。`rule_month` 取「月」前两字符再**过滤掉非数字**：
   「180135本月」的「5本」滤成「5」→ 整句读成 5 月；「…180157本月」→ 7 月。
   改成从「月」往前**连续**取数字、遇非数字立停。
4. **市场费用模板没有残留守卫**。裸 `contains("市场费用")` 就出数，而它只兑现
   「时间窗 + 费用分类」—— 地域/客户/材料/核销一个都表达不了、也一个都不检查。
   补残留守卫后，「市场费用核销需要哪些材料」正确落到 knowledge 并答出材料要求。

### 二、语义丢失

- **「最高/最多」不落 LIMIT**：「本月销售额最高的客户」返回 200 行全榜，确定性摘要还把
  第一名标成「榜首」。同一引擎对「前十」严格落 LIMIT 10 —— 分支缺失不是取舍。
  只认极值词，不认「排行/排名」（后者用户要的就是一张榜）。

### 三、噪声（业主原话里那一类）

- **检索残渣上屏**：纯数据问句下面挂「知识库里没有关于…的规定」+ 无关手册引用；
  人名实体卡上挂**优步前 CEO 贬低女性司机、影石创新发红包**。
  判据用现成的：综合必须引用 `KB:` 事实域，否则返 None；综合都出不来、
  问句又没有资料诉求词 → 那份资料是检索残渣，不挂面板。
- **自相矛盾的提示**：资料半答得完整（带角标），正上方却挂「我没能完全确定要查什么数据」。
  裁决判 Knowledge 时数据臂本就不该有意见，不再挂。

### 四、探针加试 state/city

「粤东本月销售额」被当客户名探成一个零命中的 storecode 答 0；「郑州市本月销售额」
（city 实有 182 万）判「合同没有该维度」。成员值探针加试 `state`/`city` 两列 ——
它读**真数据**，新增取值自动跟上，比再维护一张词表可靠。

### 五、方法论：并行猎捕的两条经验

- **回归题集里没有的族，缺陷能活很久**。reg11/reg12 两轮全量日志里
  「未证明结构化意图覆盖 → 回落」零次命中 —— 不是没 bug，是没题。
- **并行探测会与回归抢资源**。工作流 28 个并发 `docker exec`（每个都是完整 debug 版
  CLI 进程）时跑回归，多条题报「进程非 0 退出」，stderr 尾部是 DB 痕迹而非 panic。
  纪律：**猎捕与回归不同时跑**。

### 补：AX153 第二批（并行修复工作流 403 全灭后自己收的）

并行修复工作流 7 个分区全部 403（鉴权），改成串行自己修。按危害排序收了这些：

| 缺陷 | 现象 | 修法 |
|---|---|---|
| 近一周 vs 近7天 | 近一周覆盖 **8** 个自然日、近7天覆盖 7 —— 同一句话两种说法两个数 | 上界含今天，下界必须「回推 N 个单位再 +1 天」。日历算术自己处理月末长度，不折算成天（那会在 1/31→2/28 出错） |
| 「多少+数量单位」 | 「买了多少箱」把**销售额 151668 元**当箱数答（真值 27370 箱），收据里 `metric:箱` 明写未解析却既不拒也不提示 | 单位紧跟「多少/几」时把指标改判成销量；判据窄到不会被商品名误伤（「薄皮包子」含「包」） |
| 各大区 | war_zone 取值本身就叫「东北大区/西北大区」，别名表却只有「大战区」 | 补「大区」 |
| 城市 | city 是实有列（318 取值），却被写死在 `WAREHOUSE_SALES_UNSUPPORTED` | 移出黑名单 + `Dimension::City` 进 DIMENSIONS 与 RELIABLE 表 |
| 倒数 | ASC+LIMIT N 的能力早就有，「倒数」四处词表一处都没有 | 补进 `detect_top_n`（条数）+ `rank_direction`（方向）+ 消化词 |

**每一条都踩了同一个坑的两半**：判据认了、消化词没跟上，残留守卫照样整条拒。
第一次改完实测「浏阳品元本月买了多少箱」残留「浏阳品元买了」、「最高的城市」残留「城市」、
「倒数三名」残留「倒数」——**指标/维度改对了，问句反而答不出来**。
纪律再记一遍：*一个词能不能兑现，链路上每一环都得说同一句话*。

验收（生产直打）：

```text
浏阳品元本月买了多少箱 → 27,370          （此前 151,668 元当箱数）
各大区本月销售额       → 19 行            （此前白拒）
各城市本月销售额       → 200 行            （此前「合同没有该维度」）
本月销售额倒数三名的省区 → 3 行 ASC        （此前白拒）
近一周销售额           → direct-agg 7 天窗
```

### 待办（查清了，需单独裁决）

- **「最高的城市」答「未知」**：`COALESCE(city,'未知')` 的兜底桶有 4618 万，排行第一。
  它不是一个城市，是数据缺口。排行类（ORDER BY + LIMIT）要不要排除合成桶，
  是个口径裁决 —— 排除会隐藏真实缺口，不排除答案没意义。两边都有理，等业务定。
- **序数「排名第二」** 仍返回整榜：需要 `LIMIT 1 OFFSET n-1`，先确认 QueryOptions 支不支持 OFFSET；
  不支持就该 fail-closed 出「不可计算」，**不许**继续返回整张榜。
- 售后表 `receiver_province` 存国标行政区划码（'430000'=湖南），省份别名展开只挂在库存路径上 →
  「湖南退款额」错答。
- 商品分类：`sales_fact.class2`（39 个、以「系列」为主）与 `DW.dim_sku.class2`（19 个、全「类」后缀）
  是两套不相交词表，「商品分类烤肠系列」「蛋挞系列」因此白拒。

### 补：AX153 第三批（并行修复工作流 403 全灭 → 串行收尾）

再收七条，其中一条是**自己刚种下的生产 panic**：

| 缺陷 | 现象 | 修法 |
|---|---|---|
| 探针空候选 → panic | 回归 B01R「销售额按省区」报「进程非 0 退出」rc=101。残留就是「省区」本身，剥掉维度词尾后 stem 是**空串**，拼进 `IN ('省区','')`，探到一行 `city=''` 就把空串当成员值 → `Predicate::eq(dim,"")` 触发断言。**City 进探针表之后才现形**（city 有空值、region/war_zone 没有） | 三处一起堵：候选不产空串、探针结果 trim 后为空不算命中、装配前空 member 直接 None |
| 序数排名 | 「排名第二的客户」返回 **200 行全榜**，摘要还把第一名标「榜首」——问第二拿到第一 | `QueryOptions` 无 OFFSET，加它要动全部调用点 → 先 fail-closed。判据两面：「排名第N」不需量词、裸「第N」必须带名次量词（否则「第一季度」被误判） |
| 库存口径静默切换 | 「福建库存量」→ 200048752，trust=verified，无任何提示。中台 WMS 无省份列，带省份的问法会掉到门店/经销商进销存快照——**另一个口径** | 口径写进**列别名**（`库存量（门店进销存口径）`）。`DirectHit` 没有备注字段，而列名一定上屏，零管道成本。只有库存量加后缀：金额本就只有这一张表 |
| 售后省份码 | 「湖南退款额」→ `receiver_province LIKE '%湖南%'` 答 227.94。该列存 6 位行政区划码（440000/430000/…），码列 LIKE 中文名必然近乎 0 行 | 同一本字典的**第三个落点**登记上（前两个早就有）。修完 SQL 变成 `receiver_province = '430000'` |

验收（生产直打）：

```text
本月销售额排名第二的客户 → need-intent（fail-closed，不再给整张榜）
福建库存量               → 列名「库存量（门店进销存口径）」
湖南退款额               → receiver_province = '430000'（此前 LIKE '%湖南%'）
本月销售额按省区         → 27 行（此前 rc=101 崩掉）
各城市本月销售额         → 200 行
```

**回归 19：执行 82 / 通过 76 / 失败 0 / 跳过 6** —— 跳过的 6 条正是图题，
起飞前检查按「图还没同步完」判依赖缺席，不再假红。

### 这一轮的三条方法论

1. **并行猎捕值得**：12 路直打 + 对抗复验，一次拿到 64 条带 file:line 根因的确认缺陷，
   其中 6 条是倍数级错答（最狠的 8800 倍），全部 trust=verified 或 review —— 靠人工抽查
   几乎不可能在一天内挖到这个密度。
2. **并行修复没跑通**（7 个分区全 403 鉴权失败），改成串行自己收。教训：
   工作流适合**只读的探测**，写代码那半在鉴权/环境上更脆。
3. **每加一个词表条目，链路上每一环都得说同一句话**。这一轮至少三次踩到同一半：
   判据认了、消化词没跟上，残留守卫照样整条拒 —— 指标/维度改对了，问句反而答不出来。

## AX154（2026-08-16，把昨天记下的 7 条未修确认缺陷一次收完）

昨天收工时列了 7 条带 file:line 根因的未修缺陷。今天全部收掉，方法是
**7 路并行勘察 + 7 路对抗复核**（只读代码、不碰生产 —— 那会儿 reg21 正在跑）。
复核这一层是本轮真正的价值：**7 条方案里有 6 条被复核逮到实质问题**，
其中 3 条若照方案落地会直接造出新的错答。

### reg21（起飞前的干净底）

`执行 82 / 通过 82 / 失败 0 / 跳过 0`，耗时 1315s。昨天最后一批（市场费用分类 /
各仓库 / 冻结库存 / 闸门判据）只做过直打验证，这一轮补上了全量回归；
图题这次没跳（容器起了 15 小时，`graph_sync` 早就 ok）。

### 七条各自的落点

**① coverage=blocked 通用闸 → 只硬拦 entity/region 两类。**
原方案要在 `ctx::attach_trust` 加一道拦全部 blocked 的闸。复核指出三件事：
`blocking()` 对明细形问句今天**已经**硬拦（`sql:no-aggregate-for-open-slots` 进
conflicts）；白拒的大头不是「表面词翻码值」而是 `filter_columns` 认不出名字时
**无条件**把 `filter:{name}={value}` 推进 unverifiable，而那一类**永远无法从 SQL 证明**；
`coverage.issues` 是会被序列化、被 hybrid 重写、被日志读取的**展示字段**，
拿它的字符串前缀做执行拦截是把执行语义挂在展示格式上。

最终落点改到 `hits.rs::land`（拿得到 typed `CoverageReport`），判据收窄成
`unclaimed_scope()` = `entity:` + `region:`，处置是**回落下一成员**而不是新造拒答卡
（回归里有 `route_not: need-intent` 的用例，翻 route 会红；回落还能白拿 `run.rs`
那道 LLM 覆盖闸兜底）。分档理由写进了 `CoverageReport::unclaimed_scope` 的注释：
少给一个数（metric/comparison/detail）与**答成另一个人的数**不是一回事。

**② 商品分类两套词表 → 探针分流，不合并词表。**
订正前提：`sf.class2` 那 39 个「…系列」在 Rust 侧**一个落点都没有**。两套值指向两张表，
合并只会把白拒换成静默空结果。改成给成员值探针加一位 `Dimension::Category`
（**不进 `DIMENSIONS`**，分组口径仍 fail-closed；`SNAPSHOT_COLUMNS` 不动，
自由 SQL 那条路的姿态白拿不变）。两处承重：分类的维度词在**前缀**位，
词表直接借 `ENTITY_PREFIXES` 的 `Kind::Category` 四条（另起一份必漂 —— 那边今天就在剥
「产品类型」）；证据按维度分档报 **Entity** 不是 Region（`ENTITY_COLUMNS` 有 "category"
没有 "class2"，`entity_proved` 恒假，而①刚把实体未认领变成硬闸，不报就当场白拒）。

**③ 知识库相关性下限 → 补正文锚，不设新阈值。**
订正两条前提：检索侧其实有五道已标定的下限，`Hit.score` 也不是 0.0（硬编码 0.0 的是
`Citation`）。真正的漏洞是**结构**的：图谱路（`kg_top_chunks` 没有任何相关度下限）与
外部 KB 路（合成负 id，结构上进不了任何正文路）绕过了「必须有正文直接命中」这条锚，
于是五条正文路全空时 `ids` 仍非空，零命中早退走不到。补的就是关系路已经在用的那个
`if direct_ids.is_empty()`，逐字同形。今天能过任一条正文路门槛的真命中行为逐字节不变。

**④ 「最高的城市」答「未知」 → 排行剔桶 + 把桶说出来。**
口径裁决：**排行/极值**的名次只发给真实成员；无限定的「各城市销售额」是分布问题，
桶留在表里。只排不说 = 静默隐藏数据缺口，所以同时装一条说明行走既有 `detail` 通道
（前端「补充数据」区已经在渲染，零新增字段、零协议改动），主查询 WHERE 与说明行 WHERE
恰好互补。City 的兜底字面量改成「未登记城市」——「未知」既能读成「有个城市叫未知」
也能读成「系统不知道」。新增 `Dimension::unregistered_bucket()` 穷举 match 不写 `_`。

**⑤ 序数排名 → 真答第 N 名。**
昨天估的「加 OFFSET 要动全部调用点」是高估：字面量构造只有 6 处，其余走
`QueryOptions::default()`。复核这一条判的是 **wrong** —— 勘察员提的
「改用 LIMIT N 让用户自己看末行」在 HEAD 上是回退。三处承重：名次的数字要进消化词表
（`STRIP_WORDS` 有「排名/第/名」唯独没有中文数字）；序数必须**覆盖** `explicit_limit`
（「排名第2」会被 `ranking_limit` 读成 top-2）；序数要并进 `ranking`
（否则「各月销售额第二名」按月份升序排，OFFSET 切在与名次无关的序上）。

**⑥ 多值枚举 → 拼一条 region IN，但必须同时出分组。**
复核逮到第三种翻车形态：不是查全国、也不是只取一个省，是**静默合并**。
`Dimension::Region` 的别名里没有裸「省」，所以「山东省和江苏省本月销售额」一个维度词
都不命中 → scalar 分支 → 一个合并数还带环比；而且收据糊不掉（`name_value_matches`
是子串匹配，`'山东省区' contains '山东省'`，region 槽照样判「已证明」，trust 仍是
verified）。加了「多值必须命中 Region 维度词」这道门。同批复核还拿掉了两处：
分隔符表里的「还有」「以及」（`lexicon.rs` 明写「还有」隐含 `> 0` 不许剥，
而进 consumed 与进 `STRIP_WORDS` 对残留守卫是同一效果 —— 从这里加就是绕过那道闸），
以及 phrases 表里的「{name}大区」（那是 `WarZone` 的别名，跨维度混用）。

**⑦ 账户余额门禁 → 拆成排行/总额两档。**
时间怎么处置是设计前提：余额是**时点**量。窗口含今天 → 消化并出数（不是「忽略限定」，
是「限定与口径同义」），并且必须在 `intent_evidence` 里标 Time 槽，否则覆盖闸判
`missing:time` 回落 LLM；过去期 → 仍拒（要把 `created_time <= 期末` 下推进 ROW_NUMBER
子查询，本模板表达不了）。`NOT_A_TOTAL` 门挡住分组诉求（「各客户账户余额」今天走组合器，
答得对，被总额档抢走就是拿一个总额去答一份名单）。排行档 SQL 逐字未变，
新加的 `assert_eq!` 钉着改动前的快照。

### 顺手拆掉的一份镜像

复核②时发现：`run.rs::sales_contract_metrics` 是抄自 `server/src/direct.rs` 的镜像，
理由写着「agent 不许反向引 server」—— 那个理由早已不成立（判据搬进了 dms-semantic，
agent 本来就依赖它）。留着的代价不是少认几个词，是**认成另一个指标**：镜像少了
`QTY_UNITS` 改判与销量那族额外词，而这份镜像正是 `ask.rs` 那道「维度成员值优先」门
用的判据 —— 走那条门的问句会拿销售额去答箱数，而快路径上同一句话昨天早就修对了。
镜像与它的「漂移锁」单测一起删掉，单测改成**转调判据**。

### 这一轮学到的

1. **并行修复不行，并行勘察+对抗复核行。** 昨天 7 路并行修复全 403；今天 7 路只读
   勘察 + 7 路对抗复核，14 个 agent 零失败，而且复核逮到 3 条会造出新错答的方案。
2. **勘察员给的行号系统性地偏。** 七份方案里至少四份的 file:line 整体偏 2~20 行，
   复核逐条纠了回来。结论：方案里的行号只能当路标，落地前必须自己再读一遍。
3. **「判据认了、消化词没跟上」今天又踩到两次**（序数的中文数字、多值的分隔符），
   两次都是复核前自己发现的 —— 说明这条已经成了肌肉记忆，但**还没成为机制**。

## AX155（2026-08-16，业主三张截图：为什么 CLI 恒对、网页恒错）

业主给了三张生产截图，说「你已经改过多次了，这样的问题怎么还是会出现」。
这一节的价值不在三个补丁，在**为什么这一族反复回潮**这个问题终于有答案了。

### 三张截图

| 问的 | 网页答的 | 该答的 |
|---|---|---|
| `长沙鸣望供应链管理有限公司`（裸客户名） | 「知识库里没有关于…的任何信息」+ 5 篇无关文档 | 客户卡（该客户事实表本月 67 单、169.11 万） |
| `HJXH-DSO2026081500390`（裸单号） | 「先问清再查 · 尚未确定应使用问数还是知识检索」 | 18 行销售单明细 |
| `客户退出申请流程`（纯资料诉求） | 200 行「退出申请金额」排行 + 深度 BI，三条断言全「未满足」 | 《客户退出申请流程填写详细指引》 |

### 第一件事：同一句话，CLI 全对

```text
长沙鸣望供应链管理有限公司 → entity-card 5 行（SQL 里就有 customer_short_name AS 客户简称）
HJXH-DSO2026081500390     → direct-doc  18 行
客户退出申请流程           → knowledge   正确答出流程正文
```
6 轮 × 3 题，**18/18 稳定**（顺手排除了「路由抖动」这个假设）。

**所以判据一直是对的，错的是「谁在 Router 之前替它决定」。**

### 根因：Router 之前有五个出口，判官一个都看不见

`tools/regression.py` 走 CLI 子命令 → `dms_agent::ask::ask` → 两臂编排。
业主走 HTTP → `api_ask` → **两道只长在 server 层的闸**：

```rust
if !prepared_contract_ready(&prepared) { unknown_route_kb_fallback(...) }   // 截图 1
if route == IntentRoute::Data && !intent_attempt.is_data_executable() { 反问卡 }  // 截图 2
```

- **第二道闸缺确定性豁免。** R1.5（单号点查）判 `Data` + `deterministic=true`，
  而一个裸号必然抽不出槽位、合同必然作废 —— 于是**全系统最不含糊的问数信号**
  被拦在 Router 之前。加 R1.5 时只改了第一道闸（它有豁免），另外三道按老判据活着。
  `main.rs` 那句「确定性规则**只产** Knowledge」的过期注释就是漏改的心理依据。
- **第一道闸的兜底是「只问知识库」。** 确定性问数成员（实体卡 / 单据点查 /
  business-lookup）一个都没跑过。同族缺陷 2026-08-14 在「线下-浏阳品元商贸」上治过一次，
  那次只收了 `Knowledge` 那条臂，剩下五个入口原样留着。

**这就是「改过多次还回潮」的机制**：判官看不见那些闸，所以每次只修被业主碰到的那一份，
而且每次都在 CLI 上验的绿。

### 第四条根因：那句「没有」是我们自己的提示词让模型写的

`knowledge/answer.rs` 的 SYSTEM 段亲手规定「资料只覆盖一部分时**第一条**必须原样以
「知识库里没有关于」开头」。而判据有三份、**没有一份认得它**
（server 的 7 条 MARKERS / hybrid 的 `starts_with(NO_HIT)` / SYSTEM 自己只写规矩不判）。
于是那句话带着 5 篇无关文档的角标，通过了「有引用 + 不是 NO_HIT」的闸，当答案上了屏。

判据搬进 `dms_knowledge::answer`（与 SYSTEM 同文件），另两处转调。刻意**不是**
「含有这句就算没有」—— 部分覆盖也用这个开头，一刀切会误杀真答案；
判据是「这句**之后**还有没有带角标的结论」。

### 截图 3 是另一条：否决键自指

R2（doc-topic）的正判据命中了（「流程」在 `DOC_NOUNS` 里），是**否决键**把它按下去的：
`has_measurable_slots`。而槽位成立的门槛低到只是**问句子串**
（grounding 兜底是 `contains_folded`，从不与注册表核对）。于是模型把「退出申请」当指标、
「客户」当分组 —— 两个都只是这个文档标题的碎片 —— R2 被自己要救的那句话否决掉。

这条判据此前修过四次，每次都在回答「**哪一类**槽位不该算」，
从没人问「**未经证实的**槽位凭什么有否决权」。现在否决要三选二：
有可度量槽位，**且**（注册表认得的指标在场 **或** 那个文档名词被合同剥掉了）。
「被剥掉」用的是仓里现成的残留守卫原语，不是「某槽位含某文档名词」——
后者会被「本月合同金额的审批流程」用一个「合同」买通（复核逮到的）。

### 对抗复核的账（这一轮它值回票价）

3 路勘察 + 3 路复核。**三条复核全判 risky，且各逮到一条我这版落地代码里的实缺陷**：
1. 我把「合同没就绪」换成两臂之后，`route==Data` + 有歧义那一档会撞
   `bail!("Router 未产出答案")` → **500**，替掉了原本的澄清卡。判据没与真闸对齐
   （`llm` 成员的 accept 恒等于 `is_data_executable()`，接不了单就不该在表里）。
2. 我只改了 3 个入口，**漏了 xcx 两处 + mcp 一处**（第四/第五份判据）。
3. `doc_noun_claimed` 查的是「某槽位含某文档名词」，不是「那个名词被认领」，
   且会把「标准价销售额」这类今天正确的问数题抢去知识库。

### 防复发：三条源码守卫 + 判官能打 HTTP 了

- `only_one_contract_gate`：main/deep/xcx/mcp 四个入口的**代码行**里不许再出现
  `is_data_executable()`（注释放行 —— 判据搬哪去了要写清楚）。
- `no_entry_answers_from_the_kb_without_running_the_deterministic_lane`：
  四个文件里不许再有 `unknown_route_kb_fallback(` 调用点（定义留着，两臂内部的资料半仍用它）。
- `every_entry_consults_the_kb_before_showing_a_card` 同步收紧：
  合格出口从「只问知识库 **或** 两臂」改成**只认两臂**三种写法。
  老守卫只保证「KB 被问过」，从不要求「确定性问数车道被问过」——
  于是「把出口换成 KB-only」永远合法、永远绿。这条不变量是对称的那一半。
- `regression.py --http`：走 `/api/ask`，身份用 `X-API-Key`（业主裁决建长期 key）。
  一把 key 只映射一个 login，所以只跑 `login=admin` 的题，
  其余**诚实跳过并说明理由**（静默会让 `--http` 变成一轮全绿的假象）。
  新增 W01/W02/W03 三题，逐条对应三张截图。题集 81 → 84。

### 顺带

登录页铺了品牌图（主体在左半边，卡片靠右放；窄屏退回居中 + 暗罩），
图标切三档（logo 256 / apple-touch 180 / favicon 64，2.7MB → 151KB）。

## AX156（2026-08-16 下午，业主三条系统性要求：全量扫描 / 血缘 / 知识库）

业主原话：「对所有单号、所有实体、所有主数据进行扫描，**不能我说什么你就解决什么**」、
「对每个表的定义、上下游关系、血缘关系都了解透彻」、「对知识库彻底加强」。
方法改成 **先量分母 → 再按证据修 → 最后锁**，四路并行清点 + 四路对抗复核（8 agent 零失败）。

### 一、量出来的分母（生产 meta 库倒的，不是猜的）

| 项 | 现状 | 判断 |
|---|---|---|
| 表定义 | 115/115 有注释、**3089/3121 列有注释（99%）**、38/38 指标有口径 | **这块其实很齐** |
| 关联边 `join_edge` | 27（人工精修，带口径陷阱注释） | 少但质量高 |
| **血缘 `datamap_edge`** | **2**（人工种子） | 空 |
| 单据族 | 14 | 6 族两侧无源 |
| 知识库 | 104 文档 / 1102 块（全部有向量） | — |
| KB 基线（20 题） | **recall@1 0.15** / recall@3 0.90 / recall@5 0.95 / answer_acc 0.85 | 排序层病 |

⇒ 第二条要求的根因**不是「不了解表」**，是两个构建器（`meta lineage-build` /
`meta datamap-build`）**从来没跑过** —— 前者文件头自己写着「纯 PG 元数据，秒级可重跑」。

### 二、跑了构建器，然后立刻撤回

```
lineage-build → 2 → 82 条
datamap-build → 191714 条（joinable 38780 / synonym 146959 / …）
```

**没当成好消息。** JOIN 证据闸读的正是 `joinable`，阈值 0.9，落进生效档 365 条，抽样：

```
ads_fin_profit_loss_dnf.rebate_other ~ ..._fresh_dnf.rebate_other   conf 0.95
ads_off_new_product_sales_dnf.amount ~ dws_off_third_party_sales_dnf.amount  conf 0.95
```

拿金额列做 JOIN 键会把两张表按金额撞在一起 —— 而 ODS 推导路正是当天上午编出 151 亿那条。
复核同时指出：`lineage-build` 的最强信号 `catalog_mention` 在真实目录上**恒等于 0**，
列重叠判据对本仓 ETL 命名风格结构性失效（连唯一确认为真的边都过不了），
而 80 条 pending 边**立即**参与 direct-derive 候选加权（锚点 3 张、池子 6 张，足以摊平成随机序）。

处置：80 条 lineage 置 `rejected`；两个消费者改成**只认 `accepted`** —— 写入侧承诺
「待人工复核」，读取侧就不许收 pending。行为面为零（改动前 accepted 的 joinable 是 0 条），
变的是将来。另加**键形正判据**：只有 `*_code`/`*_no`/`*_id`/`*_key`、合同登记的维度列、
数仓分区列才算 JOIN 证据。用正判据而非「排除度量列」是因为两种漏法代价不对称。

### 三、单据族全量矩阵逮到的最高危一条（唯一会**答错**而不是白拒的）

两套分词器分隔符集不同（`document` 保留 `_`/`*`，`triage` 不保留）：

```
HJXH-DSO2026080400071_2  →  triage 切出 base，_2 丢掉
                         →  改写守卫据此判「单号还活着」放行
                         →  resolve_code 从 WarehouseShipment 静默变成 SalesOrder
                         →  查错表、返错数、无任何提示
```

顺带 `DEV_XQ100`（注册表明写允许）被切碎后 R1.5 不触发，真单号掉进知识库兜底。
收成一个事实源：`document::ascii_code_candidates` 提 pub，`triage::code_tokens` 转调。

### 四、知识库排序层（第三条要求的落点）

`recall@6 = 0.95` 而 `recall@1 = 0.15` —— **对的块召回得到、就是排不到第一**，
0.80 缺口 100% 在排序层。真凶是 `merge_adjacent` 的收尾排序：

```rust
b.score.total_cmp(&a.score).then(a.chunk_id.cmp(&b.chunk_id))   // 入库顺序决胜
```

次序键改成**与问句的向量距离**（单一量纲、零标定，候选集加载时那张表就在手上）。
**没用**复核否掉的原方案「每路归一分取最大」—— 那是跨路比较，量纲差得离谱
（元数据路命中标签时 sim 恒 1.0、向量强命中只有 0.45），偏好序会与本文件字面量
钉死的权重序**恰好相反**。

同批：`RERANK_WINDOW` 从 `TOP_K*2=12` 放到 `CANDIDATE_K=24`（旧值够不到第 13-24 名，
拿它测「精排有没有收益」必然测出「没收益」，然后这条链会被删掉）；精排关着时留一句日志；
`server-restart.sh` 透传三个 `DMS_RERANK_*`。**但全仓没有 rerank 服务** —— 接外部端点
还是本地起一个，是业主的决定。

### 五、这一轮修掉的三条「护栏自己会哑」

1. `kg_and_ext_kb_routes_need_a_body_anchor`（**我自己上午写的**）结尾
   `for gate in [...] { assert!(src.contains(gate)) }`，而 `src` 是整份文件、
   **包含这个数组字面量本身** —— 五条断言恒真，把五个阈值常量全删了也绿。改成钉值。
2. `every_unknown_arm_exits_...` 按 `IntentRoute::Unknown =>` 形状扫，而同一刀把
   xcx/mcp 塌成 `_ =>` —— 当场变哑。改成扫**承重的那个调用**（`kb_answer` 家族）。
3. `both_ask_endpoints_share_one_arms_exit` 的 `count() == 3` 计数判据（本仓记过账的坏味道）
   换成按 handler 判形状。

### 六、我自己踩的两个坑（都记账）

1. 两个 `deploy_update.sh` 并发 → docker 导出层撞
   （`failed to export layer … lstat …/target/debug/deps/*.rlib`）。好消息是它 fail-closed。
2. `docker run` 的行继续符后面接 `#` 注释，会把命令**剩余参数整段吃成注释**，
   而 `bash -n` 查不出来（语法仍合法）。注释只能写在命令之外。

## 明天的起点（2026-08-16 收工）

### 现状

- 生产：见 `readlink -f /opt/dms-ai/app`。今天两轮部署：上午一轮（昨天记的 7 条缺陷）、
  下午一轮（三条路由错判 + 登录页品牌图）。
- 回归：`reg21 = 执行 82 / 通过 82 / 失败 0 / 跳过 0`（今天上午，起飞前的干净底）。
  题集 81 → 84（新增 W01/W02/W03，逐条对应业主三张截图）。
- **判官从今天起能打 HTTP**：`python3 tools/regression.py --http`，
  身份用 `X-API-Key`（业主裁决建长期 key，已写进 `/opt/dms-ai/settings.docker.json`
  的 `mcp_keys` 映射 admin；原文件备份在同目录 `.bak-regkey`）。
  一把 key 只映射一个 login，所以 `--http` 只跑 `login=admin` 的题，其余诚实跳过。

### 明天先做

1. **跑两档回归**：CLI 全量 + `--http`。下午最后一轮部署只做了三题直打验证，
   全量还没跑（`reg22`）。
2. `--http` 档是今天新写的，**自己还没在生产上跑过一整轮** —— 第一轮要盯
   「跳过的题数符不符合预期」（非 admin 题 + 两轮题 + gate/redline 题）。

### 唯一点名但没做的一条

`deep_api` 的 `assertions` / `未取到的板块` / `verdicts=unmet` **零消费者** ——
系统自己知道「我没答上」，却没把这个信号接回控制流。今天截图 3 的三条「未满足」
就是这么来的。routing 修好之后那一档不再复现，**没有失败样本可验**，所以按纪律不动。
真要做，判据是「全部 verdict = unmet 时不许把它当答案呈现」；
但 UI 今天已经把「未满足」与「本次未取到的板块」显示出来了 —— 是**已披露**不是静默。

### 今天踩过、明天别再踩

1. **两个 `deploy_update.sh` 不许并发**：docker 导出层会撞
   （`failed to export layer … lstat …/target/debug/deps/*.rlib: no such file`）。
   好消息是它 fail-closed ——「未切换 app，生产仍是旧版本」。
2. **在 CLI 上验收 ≠ 验收**。判官走 CLI 子命令，HTTP 那条路上另有五个出口。
   今天这一族缺陷的全部成因就是它。`--http` 就是为这条写的。
3. 昨天那三条（猎捕/回归不许同时跑、容器重启后等 `graph_sync != never`、
   每加一个词表条目链路上每一环都得说同一句话）继续有效。

## （已归档）明天的起点（2026-08-15 收工）

### 现状

- 生产：`/opt/dms-ai/releases/20260815T094228Z-1609`，健康检查 ok，本轮全部改动已上线。
- 代码：**92 个提交未推送**（业主选的「只落存档不 push」）；`cargo test --workspace` 全绿，
  web 60/60。
- 回归：最后一次干净全量是 **reg19 = 执行 82 / 通过 76 / 失败 0 / 跳过 6**（跳过的是图题，
  容器刚重启、图未同步，起飞前检查按依赖缺席跳过）。reg20 的 2 条红是**闸门题假红**，
  判据当天已修（见下）。今天最后一批（市场费用分类 / 各仓库 / 冻结库存 / 闸门判据）
  只做了生产直打验证，**还没跑过全量回归** —— 明天第一件事就是它。

### 今天最后一批的直打验收

```text
6月营销物料费用 → direct-agg  ["营销物料费用"]  291,237.786
各仓库库存量    → direct-agg  200 行            [仓库编码, 库位, 库存量]
冻结库存量      → direct-agg  ["冻结库存量"]    973,546
```

### 未修的确认缺陷（按危害排序，都带 file:line 根因）

1. **coverage=blocked 不是闸门**（通用防线，多条发现都指向它）。今天只治了两个具体档
   （纯数字客户编码、「多少+数量单位」的指标替换）；通用闸仍缺：
   `intent_summary.coverage.status=="blocked"` 时管道照旧回落通用模板出数。
   小心别把「覆盖闸读不懂模板 SQL」（`hits.rs::only_unreadable`）一起拦掉。
2. **商品分类两套词表不相交**：`sales_fact.class2`（39 个、以「系列」为主）vs
   `DW.dim_sku.class2`（19 个、全「类」后缀）。「商品分类烤肠系列」「蛋挞系列」因此白拒。
3. **知识库相关性下限**：检索 score 恒为 0.0（rerank 未配），按分数设闸会误杀全部；
   需要换一个信号（问句↔正文词重合度？）。今天已用「综合必须引用 KB + 问句有资料诉求词」
   把**面板**挡住了，检索侧仍无下限。
4. **「最高的城市」答「未知」**：`COALESCE(city,'未知')` 的兜底桶 4618 万排第一。
   排行类要不要排除合成桶是**口径裁决** —— 排除会隐藏真实数据缺口，不排除答案没意义。
5. **序数排名**目前 fail-closed（「排名第二」出「不可计算」）。要真答得给
   `QueryOptions` 加 OFFSET，那会动它的全部调用点。
6. 「山东省区和河南省区本月销售额」多值枚举被拼成「山东河南」整体拒答；
   单值路径本来就支持 IN 列表。
7. 「账户余额」门禁过严（同时要求含「账户余额」+ 排行词 + 「客户」），
   「本月账户余额是多少」这类总额问法完全没有承接。

### 今天踩过、明天别再踩的三条

1. **猎捕与回归不许同时跑**：并行 28 个 `docker exec`（每个都是完整 debug 版 CLI 进程）
   会把回归打出「进程非 0 退出」的假红，stderr 尾部是 DB 痕迹而不是 panic。
2. **容器重启后等 `graph_sync != never` 再跑回归**，否则 6 条图题按依赖缺席跳过
   （这是对的行为，但那一轮就验不到图路径）。
3. **每加一个词表条目，链路上每一环都得说同一句话**。今天至少三次踩到同一半：
   判据认了、消化词没跟上，残留守卫照样整条拒 —— 指标/维度改对了，问句反而答不出来。
   同族的第四次是我自己种的：`PHRASES` 子串命中让「最近三个月」只消化掉「近三个月」，
   剩一个「最」被拒。

### 今天新增/改动的判据（回归之外）

- `tools/regression.py::graph_up` 现在要求 `graph_sync != never`（健康检查够不到时回落旧判据）。
- 闸门漏判扫描改成「痕迹必须与探针表 `__dms_ai_gate_probe` 同行」，
  并在 `--selfcheck` 里加了自证 —— 防止哪天又退回整段扫关键字。
- 回归题集 81 → 82 题（新增 R01 大区成员值直查 / R02 未登记大区诚实拒答）。

## AX157（2026-08-16 晚，业主裁决「废除本地模型全部用千问」+ 一条让确定性臂整条哑掉的根因）

### 一、向量与精排全部换成千问（业主裁决：废除本地模型）

`tools/embed_service.py` 变成 DashScope 适配层，**Rust 侧 wire 契约一个字没动**：
- `text-embedding-v4` @ `dimensions=512` —— 与库里 `vector(512)` 同形，**零 schema 迁移**
- `gte-rerank-v2`（`gte-rerank` 已 AccessDenied）走原生 text-rerank 端点，出参转 Cohere 形状
- 单批上限 **10**（实测 12 条即 `batch size is invalid`），按 `index` 归位不信返回顺序
- 不留「有 key 走千问、没 key 回落本地」的双档：双档 = 两套向量空间，混了不报错只变差

换空间要重算的是**六张**表，不是四张：`kb.chunk`(1102) / `meta.table_doc`(123) /
`meta.element`(1310) / `meta.sql_exemplar`(75 enabled) / `meta.datasource`(6) / `meta.memory`(91)。
`kb.chunk` 另有一坑：`revec` 的 `KB_SEL` 只扫 `kb.doc.status='chunked'`，
向量置 NULL 但状态还是 `embedded` 时它**扫到 0 行还退出码 0** —— 必须先把状态退回 chunked。

### 二、精排第一次真正接线（此前生产从未跑过）

`DMS_RERANK_BASE_URL` 默认取 `settings.service_url`、`DMS_RERANK_MODEL` 默认 `gte-rerank-v2`，
两条都进了 `scripts/test-deploy-contract.sh` 的合同清单（反向验证过会红）。
正面证据不是「没有『未接线』日志」（那句是 debug 级，生产日志级别根本不打），
而是 embed 服务每次调用打一行 stderr：`journalctl -u dms-ai-embed | grep rerank`
实测 `rerank 11 篇 → 11 分（gte-rerank-v2）`。

### 三、根因：光杆维度词被当成实体，确定性那条臂整条哑掉

`--http` 回归 87 题 9 红，其中**四题同一个根因**（E04 客户销量排行 / E17 客户毛利额排行 /
SALE17 省区毛利率 / C07 昨日下单客户）。生产直打 E04 的 `intent_summary` 是证据：

    slots: metric:销量(grounded) / entity:客户(grounded) / breakdown:客户(grounded)
    coverage: {status: blocked, issues: ["entity:客户"]}

大模型把分组维度「客户」同时报进 `breakdowns` 和 `entity_mentions`。后者一进去就永远
证不出来（SQL 里是 `GROUP BY 客户`，不是 `WHERE 客户='客户'`）→ `entity_proved` 恒 false
→ `unclaimed_scope()` 硬拦 → fastpath 拒答 → LLM 臂兜底。

**这条降级不报错也不难看**：答案往往还是对的（E04 那 5 行就是对的），所以没人会去看。
代价是口径/时间窗/权限过滤全走了非确定性那条路，每题多花 10 秒。

修在源头：`IntentV1::normalize` 剔掉表面词整词等于类别词的 entity_mention，
词表提成 `derive::DIMENSION_CLASS_WORDS` 与 `customer_name_fragment` 共用。
第二版补了量词前缀（「各省区」——第一版让 SALE17 时绿时红，取决于模型这次吐哪个词）。
`regions` 走同一条硬闸，同样剔。

### 四、并列问句只答一半

「分别查本月销售额和本月订单数」→ 1 行「不可计算」；
「分别统计各省区销售额和各商品销量」→ 只按省区分组，各商品销量**静默消失**。
拆分的唯一合同是意图的 subgoals（`routed.len() > 1` 才进复合），模型没吐就没有复合。
规则 3 只有抽象表述，补了 3.1 与两个逐字例子。

### 五、顺手修掉的三处同族「哑掉的降级」

1. `_qwen_key` 不再无条件读 `llm_api_key` —— 本机 `llm_provider=deepseek`，
   早一版拿 deepseek 的 key 打 DashScope（404）。对话 LLM 与向量层是两笔账。
2. `server/src/embed.rs` 那个写死 `127.0.0.1:8077` 的单例删了：生产 embed 服务在 **:8078**，
   写死那份在容器里无人监听，`retrieve` 冒烟因此**恒静默跳过向量路**却照样打印结果。
3. `_EMBED_LOCK` 删了。它唯一的理由是 onnxruntime session 非线程安全；本地模型没了理由也没了，
   而留着会重造一个修过的故障：一趟 revec（110 次 HTTP 往返）把每个问句的 /embed 堵在锁后面
   → 超 Rust 侧 3s → 进程级熔断 300s → 之后 5 分钟三条向量路全降级且零报错。

### 六、判据（都反向验证过会红）

- `_selftest_qwen_embed`：切片按 10 / 按 index 归位 / 条数不符抛 / 维度不符抛 / rerank 形状
- `_selftest_post_retry`：429 退避重试一次、400 不重试
- `the_offline_builder_covers_every_meta_vector_target`：真去读 `tools/embed_service.py`，
  遍历 `MetaVecTarget::ALL` 比对写入点与配方。原来那条只把 Rust 侧钉在字面量上，
  读不到离线脚本，所以「离线少覆盖一张表」它一个字都不会红 —— `meta.memory` 就是这么漏的
- `every_declared_prefix_is_actually_resolvable`：`DocumentFamily::prefixes` 此前是只写字段，
  而 `resolve_code` 把同一份前缀知识又硬编码了一遍。写这条时 `SPC-` 当场红了一次
- `bare_dimension_words_are_not_entities`：entity 与 region 两侧，真名字不许被误伤

### 六点五、精排的收益第一次量出来了（同一批题、同一批向量，只切精排开关）

旧基准 `kb_bench_cases.json` 是 8/8 的 18 题本地夹具，基线 `recall@k=1.0 / MRR=1.0`——
**已经饱和**，排序层再差它也是满分，这正是「recall@1=0.15」那个头寸一直看不见的原因。
新冻了 56 题生产语料基准（`tools/kb_fixtures/prod_cases.json`，10 篇 embedded 文档，
LLM 出题失败 0 次），金块是 `chunk_id + ord`（重新入库即 stale 剔除）。

| | recall@1 | recall@2 | recall@6 | MRR | 未命中 |
|---|---|---|---|---|---|
| 精排**关**（纯九路 RRF） | 0.4643 | 0.8036 | 0.9464 | 0.6670 | 3 |
| 精排**开**（gte-rerank-v2） | **0.5536** | 0.8571 | 0.9821 | 0.7348 | 1 |
| Δ | **+8.9pt** | +5.4pt | +3.6pt | +6.8pt | −2 |

口径说明：两趟之间**只**改了 `DMS_RERANK_BASE_URL`（容器重启一次），
语料、向量、题集、身份全同。所以这 8.9 个点就是精排本身的收益 ——
而这条链在生产上**今天之前一次都没跑过**。
报告落 `tools/kb_bench_baseline_prod.json`，下次改检索用
`kb_bench.py run --baseline tools/kb_bench_baseline_prod.json` 对比，有回退退出码 1。

### 六点八、回归收口（`--http`，业主走的那条路）

| 轮次 | 通过 | 失败 | 跳过 | 那一轮修了什么 |
|---|---|---|---|---|
| 切千问后第一轮 | 71 | 9 | 7 | —（基线，四题同因未修） |
| 修「光杆维度词」后 | 70 | 5 | 12 | entity 侧 |
| 补量词前缀 + regions 后 | 73 | 2 | 12 | 「各省区」这一档 |
| 补并列拆分正反两条后 | 74 | 1 | 12 | G01/G02 转 compound、OPS04 转回 direct-agg |
| C03 结案后（收工态） | **75** | **0** | 12 | 同值析取算证据；C03 转 direct-doc 136ms |

图题（F01/F02/F03/F05/F06）在每一轮主跑里都按依赖缺席跳过 —— 部署重启后图要几分钟才同步完，
这正是交接里写的第二条纪律。同步完单独重跑：**5/5 全绿 route=graph**（F04 因非 admin 身份跳过）。
所以收工态的有效战果是 **80 绿 / 0 红**。

剩下的 12 跳过全是结构性的，不是掩盖：3 条闸门题不走问答端点、
3 条要非 admin 身份（HTTP 档一把 key 只映射一个 login）、其余是依赖它们的关系题。

### 六点九、跨入口档（`--entries`：同一题打 ask/stream/mcp 比 route+行数+首格）

87 项 / 77 通过 / 3 失败 / 7 跳过。三条失败**都不是协议差异**，是三个真问题：

1. **C08 时间列排序没有并列键**：客户卡的 5 条最近订单，三个入口**不是同一批**
   （ask 第 0 行 …600157、stream 第 0 行 …600315；第 4 行更是 …500559 vs …500555）。
   `order_time` 落到天，整批同值，`ORDER BY 时间 DESC LIMIT 5` 从并列组里任取。
   八处一次补齐（entity 卡五处、ops 两处、深度板块一处），判据扫三个文件并断言至少 8 条。
2. **C12「昨天都有谁下过单啊」**：模型把「谁」抽成 entity —— 而「谁」**正是用户要问的东西**，
   SQL 里永远证不出来 → 硬拦 → 反问卡。同一件事换成「昨天下单的有哪些客户」就出 200 行。
   疑问代词整词剔，与「客户」「各省区」是同一个根因的第三档。
3. **E10 是判官自己的假红**：中台 WMS 现行库存是活的，连打三次
   106605152.098 / 106605152.098 / 106605016.098，而 `--entries` 串行打三个入口本来就跨几秒。
   给这一档加了显式出口 `entries_volatile`（只比 route+行数，理由连同三个实测值写进 note），
   **不做通用降级** —— 那等于把这一档整个关掉，真的不一致会被淹在假红里。

### 六点九点五、C03 结案：单号是自证的

追了一整天。`HJXH-DSO2026080300838*2`：CLI 下 direct-doc 9 行，
`/api/ask` 与 `/api/mcp` 恒 need-intent；只把 `*` 换成 `_`，HTTP 两条路都 hit。
二进制同一个、显式 `ds=dms` 也一样、连打三次稳定 —— 不是抖动、不是选源、不是编排器。

`RUST_LOG=dms_agent=debug` 拿到地面真相：进 `direct_hit` 的问句**一字不差**、
`warehouse=true`，与 `_` 那次完全相同。事出在 `try_direct_for` 之后：

    hits.rs::land → coverage.unclaimed_scope() 硬拦（用户明写的实体没被 SQL 认领 → 回落）
      → entity_proved 要求谓词列命中 ENTITY_COLUMNS（name/code/sku/customer/shop…）
      → 单据卡的 SQL 是 `WHERE r.ywzt_order = 'HJXH-DSO…*2' OR r.base_ref_order = '…'`
      → ywzt_order / base_ref_order 一个词根都不沾（那张表是给客户名/商品名设计的）
      → 「证不出来」→ 回落 → direct-doc miss → 反问卡

`_` 那次之所以活着，纯属那一轮模型没把单号抽成 entity。**抽了就死，没抽反而活** ——
所以它表现为「时好时坏、换个入口就变」，这也是它躲过之前每一轮排查的原因。

**上面那层只是表象。** 按「单号自证」修完再上生产，日志一字未变 —— 因为
**根本没有谓词可比**：`collect_provable_conjuncts` 只收合取项，遇到顶层 OR 整条跳过
（`expr_is_not_locally_provable` 见 `Or` 即 Break）。而单据卡的 WHERE 正是一条 OR。

那条纪律本身没错（`region='山东' OR 1=1` 确实什么都不证明），
错在它把**同值析取**也一并丢了：「这个号在两列里的任意一列」——
每一行都被这个号约束住，却因为写成 OR 而「无法证明」。

最终修法两层：
1. `collect_provable_conjuncts` 认同值析取：两支各自能证的值取**交集**，非空才合成一条证据
   （列取并集）。`OR 1=1` 那一支一个值都证不出，交集必空 —— 原保护一字未松。
2. `entity_proved`：表面词是合法单号时改问 `document::resolve_code` 而不是扩列名表
   （order/bill/invoice… 加不完，还会把 `order_status` 这类状态列放进来），
   且值判定**单向**（SQL 的值 ⊇ 表面词才算）——
   模型把 `*2` 顺手抹了、报上来的是 base 号，SQL 查的是拆单号、比用户说的更窄，
   限定没被丢，算认领；反方向（SQL 更宽）坚决不认，那是
   `triage::code_tokens` 红字里写过的「从 WarehouseShipment 静默变 SalesOrder」。

教训一条：**「证不出来」有两种，一种是判据太严，一种是根本没取到证据。**
前两版都在修第一种，而真因是第二种 —— 差别只有日志能告诉你（`unclaimed` 一字不变）。

生产实测（hotfix2 上线后，同一句连打三次 + 三个入口）：

    /api/ask  direct-doc 9 行 ×3
    /api/mcp  direct-doc 9 行
    CLI       direct-doc 9 行

顺带记一条通用形态：**「同一句话时好时坏、换个入口就变」，第一嫌疑是这一轮模型
抽没抽出某个槽位**，不是入口不同。entity/region 一进 coverage 就是硬闸，
抽到了就死、没抽到反而活 —— 它天然表现为「随机」。今天同一个根因逮到四档：
光杆维度词 / 疑问代词「谁」/ 单号 / 量词前缀「各省区」。

## AX158（2026-08-17，向量维度 512 → 1024）

业主问「千问的向量是不是 1024」。实测确认：`text-embedding-v4` **不传 `dimensions` 就是 1024**，
支持 64/128/256/512/768/1024/1536/2048，且**按 token 计费、与维度无关**。
昨天用 512 是为了零 schema 迁移（库里六列都是 512 维），不是因为千问只有 512。

升级三层：`EMBED_DIM` 成为唯一事实源 → 五处 DDL 字面量 + kb 迁移一起改 →
**已有库靠幂等改型拉齐**（meta 五张在 `retype_embedding_columns`，`kb.chunk` 在 KB_DDL_DELTA 的
DO 块里；两处都先读 `atttypmod` 再决定改不改，无条件 ALTER 会每次启动重建一次 HNSW）。
改型顺带清 NULL 并把 kb.doc 退回 `chunked` —— 维度变了旧向量本来就作废，
而 revec 的 KB_SEL 只扫 chunked，不退状态会扫到 0 行还退 0（昨天刚踩过）。

同一批 56 题、同一套语料，三趟的账：

| | recall@1 | recall@2 | recall@6 | MRR | 未命中 |
|---|---|---|---|---|---|
| 512 维 · 无精排 | 0.4643 | 0.8036 | 0.9464 | 0.6670 | 3 |
| 512 维 · 精排开 | 0.5536 | 0.8571 | 0.9821 | 0.7348 | 1 |
| **1024 维 · 精排开** | **0.5893** | 0.8571 | 0.9821 | **0.7512** | 1 |

1024 比 512 再 **+3.6 个点**（recall@1），MRR +1.6 个点；recall@2/@6 不动（已经贴顶）。
从「换千问之前」算起，recall@1 一共 **+12.5 个点**。基线报告已换成 1024 那份。

判据 `the_vector_dimension_is_declared_once`：扫 ddl.rs / kb 迁移 / store.rs 里每一处
`vector(<纯数字>)` 都必须等于 `EMBED_DIM`，并逐字核对**另一个进程**里的
`tools/embed_service.py::DIM`；断言至少扫到 6 处，防它扫空成恒真。反向验证过。

顺带把生产从昨晚的手工补丁 release 拉回了正路：这一轮走的是完整 `deploy_update.sh`。

升级后的 `--http` 回归：**87 项 / 80 通过 / 0 失败 / 7 跳过**（图题这轮真跑了，
因为图早已同步、容器也不是刚重启）。7 跳过全是结构性的：3 条闸门题不走问答端点、
3 条要非 admin 身份、1 条关系题依赖被跳过的那条。这是目前最好的一轮。

## AX159（2026-08-17，全方位准确/智能审计：六维度 × 反驳式核验 → 八条已修）

业主要求「对系统的准确和智能进行全方位的优化迭代」。用工作流跑了六条维度
（覆盖闸/检索排序/口径合同/答案形态/实体主数据/智能性），每条发现都要过一轮
**反驳式核验**（核验者的任务是尽力驳倒它：核 file:line、构造复现问句、
问「是不是已被别处兜住」、问「修法会不会把 fail-closed 翻成 fail-open」）。
23 条发现，9 条挺过反驳。核验者还否掉了其中两条报告自己提的修法，指出了更干净的收口点 ——
这一步比多报几条更值钱。

已修八条（每条都有反向验证过的判据）：

| # | 危害 | 病 | 收口点 |
|---|---|---|---|
| 1 | silent-degraded | `VEC_MAX_DIST=0.55` 是 bge/512 时代量的，两天里换了模型又换了维度，**五条既有判据全绿** | 重量一遍（56 题生产语料：判据块最远 0.5004 / 远域最近 0.6149，0.55 仍在缝里）+ 加 `VEC_CALIBRATED_ON` 保质期标签 |
| 2 | silent-wrong | derive 闸1 的「合同已覆盖」只认登记名，模型写「销售金额」就绕开 ⇒ ¥151 亿那一族换个别名照旧出 | `contract_owns` 认别名，别名唯一事实源是 `Metric::aliases()` + `extra_words()` |
| 3 | silent-wrong | 有客户时把「线上」也当已兑现，而事实表是**线下专表** | 抽出 `channel_words_consumed`，线上永不消化 |
| 4 | silent-degraded | 证据只登记「库里的存储值」，用户原话那个词永远证不出来（「郑州」vs「郑州市」） | `build_dimension_value_hit` 两个词各登记一次，九个 PROBE_DIMS 全覆盖 |
| 5 | silent-degraded | 三条关系模板不自报实体；共购那条的谓词在**派生表**里，闸门根本看不到 | 唯一出口自报，且只在剥出的名字确实出自问句时报 |
| 6 | silent-wrong | 深度报告自己算出「需复核」，然后把 trust 扔了（形参叫 `_trust`，零引用） | KPI **之前**出一条「需人工复核」，只挑真说明问题的 checks |
| 7 | silent-wrong | 「本期还没过完」只印在 KPI 卡小字上，写结论的模型看不到 ⇒ 拿残月比完整月 | 写进证据目录，并明说「与完整周期直接比较会低估本期」 |
| 8 | silent-wrong | 复合汇总看不到子结果的口径警示：面板挂着「不可信」，下面那段综合照样下结论 | `sub_hit` 把 `caliber_note` 与 review 标记跟着数一起喂 |

另外顺手补了「同值析取要先剥 LIKE 的 `%`」——C03 那条修完之后，
`relation_rows` 的 `名 LIKE '%X%' OR 码 = 'X'` 还是被整条丢掉（一对百分号判成异值）。

第二批又修了四条（同一轮审计的名单，逐条反向验证过）：

| # | 危害 | 病 | 收口点 |
|---|---|---|---|
| 9 | silent-degraded | `merge_adjacent` 只合并 `score`，没合并 `vec_dist` —— 昨天刚修好的并列决胜在合并块上原样复发 | 取 min，与 score 取 max 同义 |
| 10 | silent-degraded | `metric_surface_grounded` 抄了第二份指标别名表且抄漏「毛利」；模型把口语归一成登记名「毛利额」，问句里找不到 ⇒ **整份合同判废** | 销售族转调 `Metric::aliases()` + extra_words，本地表只剩库存/订单数两族 |
| 11 | silent-wrong | 合同绕开标注是张手写名单，`t_master_shop` 不在其中 ⇒ ¥151 亿那一族**换条出口就不标注**，还带 verified 徽标沉淀进 few-shot | 删负判据，改成「点了合同指标 + 没引合同事实表 ⇒ 必须说明」，文案点名实际用表 |
| 12 | silent-degraded | 经营日报 21 条 KPI 只有前 5 条进 prompt，而提示词还写着「总结…毛利、结构」 | `Reading` 加显式 `brief_rows`；行数是「这张表是不是排行」的函数，不是常数 |

十二条全部上线后的 `--http` 回归：**87 项 / 75 通过 / 0 失败 / 12 跳过**，
图题同步完单独重跑 **5/5 全绿 route=graph** —— 有效 **80 绿 / 0 红**，零回退。

生产抽验（改前 → 改后）：

    郑州本月销售额        硬拦回落自由 SQL  →  direct-agg / complete，¥231.15万
    库存金额最高的10个仓库  榜首「未知」      →  榜首 晋江轩和食品贸易有限公司仓库
    买过烤肠的客户还买过什么 整族硬拦         →  direct-doc 200 行 / complete
    本月毛利多少          整份合同判废→反问卡 →  direct-agg / grounded，metric:毛利
    本月业绩怎么样        同上              →  direct-agg / grounded，metric:业绩

**没修、留给下一手的（都带 file:line 与复现问句，见工作流 journal）**：
深度报告答案形态被三处焊死成固定三段；few-shot 还在用 trgm 选范例，
而同一个 `join!` 里问句向量已经算好了；注册表装配器把地名换成码值又不登记证据；
「设备订单数」不过有效订单状态，与 `order_count` 声明的同表口径给两个答案；
冻结/锁定分支插在仓库分组与商品残留守卫**之前**（同文件自己写的「顺序即行为」被破坏）；
量具缺第 8/9 两路诊断，图谱路对 `space=None` 静默整路跳过 ——
「今天哪一路空了」这个问题现在无法回答。

### 七、未结（下一手接着查）

0.5 **部署路上的两条运维坑（今天各踩一次）**：
   ① sshd 被打满（一天几百条短连接）后 `deploy_update.sh` 的上传/切换会间歇断线，
   表现是 `Error reading SSH protocol banner` / `Socket is closed` / bput 校验和不符。
   构建往往已经成功，只是切换那步没跑到 —— **先看 `build-*.log` 有没有 BUILD_OK**，
   有就手工切（`ln -sfn <release> app.next && mv -Tf app.next app` 再跑
   `DMS_RUNTIME_ROOT=/opt/dms-ai DMS_SERVER_HOST=172.17.0.1 bash app/scripts/server-restart.sh`）。
   ② 断线会留下 `dms-ai-server-rollback` 残骸容器，而 `server-restart.sh` 的预检
   **会因此拒绝切换**（`发现上次部署遗留容器…请先核对并人工处理`）。预检是对的
   —— 带着残骸切等于把回滚位弄丢 —— 但没人清它就会一直卡着。核对无误后
   `docker rm -f dms-ai-server-rollback` 再切。

0. ~~生产当前跑的是手工补丁 release~~ —— 已在 AX158 那一轮用完整 `deploy_update.sh` 拉回正路。
   下面这段留作记录（SSH 不稳时的应急路子）：**生产曾跑过手工补丁 release**（`20260816T150000Z-hotfix2`）：今晚 sshd 连接
   连续掉线（`Error reading SSH protocol banner` / `Socket is closed`，一天几百条短连接
   打满 MaxStartups），`deploy_update.sh` 的 bput 连挂三次。改法是：拷贝上一个 release、
   **只**把改动的 `crates/agent/src/intent.rs` 用分块 base64 传上去、原地 `server-build.sh`
   再切。代码内容与仓库 HEAD 一致（其余文件只有 `docs/PROGRESS.md` 变过），
   但**下一手要跑一次正常的 `deploy_update.sh`**，让 release 回到「从 tar 包整包构建」那条正路。

1. **E10 的口径披露**：`coverage_status: blocked` 是过期期望（同日 64c37e7 裁决 ambiguity 不再硬闸），
   已改成 complete。但原本要钉的事**没了出口**：库存有多口径，而 coverage 不提、
   caliber_note 空、正文也不提。要治是给库存族补一条口径披露。

---

## AX160 · 独立部署包（2026-08-17）

业主要求：本地打一个部署包放 `C:\Users\caowe\OneDrive\Desktop\AICoding\dmsai`，
「要有完整的文件和配置文件，包括数据库链接信息和大模型 key，这样我就可以一键部署到服务器了」。
先把 173 个提交推了 origin/main（`eb56bfd..4628926`），推前扫了一遍出站 diff，
唯一命中 `enc:v1:` 的一行是格式说明，不是真凭据。

### 这件事真正的难点不是打包，是「包里少什么没人会知道」

`tools/deploy_update.sh` 打包用 `git ls-files -co --exclude-standard`，
而 `.gitignore:8` 一条 `settings*.json` 把配置整族挡在包外。现网之所以一直能部署，
只是因为 `/opt/dms-ai` 上早就常驻着一份 `settings.docker.json` + `.secret_key`。
换台机器解包，`server-restart.sh:27` 当场 die。

物证：`target/pkg/` 里躺着 2026-08-12 那次的部署包 ——
`src.tar.gz` 里确实没有凭据，而成品 tar 的包根上有，中间还散着手工贴进去的
`settings.docker.json` 与 `secret_key.txt`。**上一次已经断过一回，是人手绕开的。**
手工步骤不会自己重复，也不会自己校验。所以这一轮的产物是脚本，不是一个包。

### 五条维度并行审计 → 35 条发现 / 10 条挺过反驳 / 已修八条

其中两条是我自己没抓到、只有跑到真机上才暴露的：

1. **CRLF 判据在 Windows 上恒绿**（`deploy_update.sh` 与 `test-deploy-contract.sh` 各一份）。
   msys 的 grep 以文本模式读文件，匹配前先把 CR 吃掉：对逐行 `\r\n` 的脚本
   `grep -c $'\r'` 返 0/rc=1 **放行**，`grep -Uc` 才返 6/rc=0。而 msys 的 bash 能正常跑
   CRLF 脚本，于是同一段里的 `bash -n` 也一起放行 —— 本机 1/5 全绿，CRLF 随 tar 上服务器，
   Linux 侧报 `$'\r': command not found`。**恒绿的判据比没有判据更坏**，它让人以为查过了。
   顺手把点名清单换成正判据（`tools/*.sh scripts/*.sh` 全查）：点名版漏掉了
   `web-update.sh` 与 `server-cleanup.sh`，这两个都在服务器上真跑。

2. **systemd 单元的 ExecStart 是相对路径**。现网那条是
   `sh -c '... exec venv/bin/python tools/embed_service.py serve 8078 ...'` 配
   `WorkingDirectory=/opt/dms-ai`。第一版 `embed-sync.sh` 只 grep 不还原工作目录，
   拿到 `tools/embed_service.py` 就去 cp —— 会打到脚本自己所在的 release 上，
   同步到错地方还一声不吭。**这条只有连真机核对才看得见**（本机没有 systemd）。

### 顺带挖出一条一直在的哑降级

`/opt/dms-ai/tools` 与 `/opt/dms-ai/docker` 是**独立目录，不是 app/ 的符号链接**。
systemd 跑的是前者里的 `embed_service.py`，web 容器 bind 的是后者里的 `nginx.conf`，
而 `deploy_update.sh` 从头到尾没同步过任何一份。也就是说：

> **改 `embed_service.py` 或 `nginx.conf` 部署上去 = 没改。**
> 不报错、健康检查全绿，只是检索/解析用着旧代码。

8-16 换千问那次两份能对上，纯粹因为当时手工传过一遍。
`scripts/embed-sync.sh` 收口这件事：问 systemd 自己在跑哪一份（不假设路径）→
同步 → 重启 → 探 `/health` → **收尾比对 sha256**。最后那条判据是关键 ——
没有它，「全绿」也可能只是旧代码依然健康，正是这个脚本要消灭的那种绿。
接线位置有意放在**切 app 之前**：向量层先换、先自证，再切 API；反过来失败会留下
「API 新、向量层半新不旧」的中间态。

### 落地清单

| 文件 | 做什么 |
|---|---|
| `tools/make_bundle.sh` 🆕 | 打包唯一事实源。出包前校验凭据成对（解不开就拒绝出包）、`kb_root=/kbdata`、`listen` 绑 0.0.0.0；**拒绝把带凭据的成品写进仓库工作区**（`.secret_key` 这名字不被 `.gitignore` 命中，`*.key` 只吃 `.key` 后缀） |
| `tools/bundle-deploy.sh` 🆕 | 包内 `deploy.sh`。三种模式 update / `--bootstrap` / `--dry-run`；不重写任何服务器侧行为，只把包内产物喂给 `deploy_update.sh` |
| `scripts/server-bootstrap.sh` 🆕 | 全新机器前置，每步幂等：PG（从 `pg_url` 反解密码与绑定地址）+ 三扩展核对 → venv → OCR 依赖 → systemd 单元 → web 容器 |
| `scripts/embed-sync.sh` 🆕 | 上面那条哑降级的解药 |
| `tools/deploy_update.sh` | 加 `DEPLOY_SRC_TAR`/`DEPLOY_WEB_TAR` 逃生门（成对必填，只给一个会发布半套）；CRLF 判据补 `-U` 并换正判据；新增 embed 同步与 nginx.conf 同步两步 |
| `scripts/server-restart.sh` | 预检加 `listen` 必须绑 `0.0.0.0`/`[::]` —— 照抄 `settings.example.json` 的新机器会容器内 healthy、外部映射打不通，空转 90×2s 后超时回滚，报错一个字都不指向 listen |
| `docker/age/docker-compose.yml` | 端口绑定改 `${DMS_AI_PG_BIND:-127.0.0.1}`：容器里的 API 走 `host.docker.internal`（网桥网关），绑 127.0.0.1 连不上 |
| `tools/requirements-embed.txt` | 删 `fastembed`（本地模型 8-16 已废除，留着白拖 onnxruntime 几百 MB），补 `cryptography`（**必填**：`enc:v1:` 靠它解，缺了 embed 服务起不来而 API 侧只表现为检索变差）与 `psycopg2-binary`/`pytesseract` |
| `scripts/test-deploy-contract.sh` | 新增 11 条判据钉住上述全部 |

**11 条新判据逐条反向验证**：拆掉任意一条都当场变红（`scripts/test-deploy-contract.sh` rc≠0），
还原后全绿。第一版里有一条是假的 —— 负判据写成 `[^/]tools/embed_service\.py`，
而真实的坏写法是 `$RUNTIME_ROOT/tools/...`，前面恰好是斜杠，反向验证时纹丝不动。
换成钉 ExecStart 那一行本身的正判据后才有牙。

### 包的形态

```
dmsai/
  一键部署.cmd / deploy.sh / 部署说明.md / MANIFEST.json
  source/      397 个文件（完整源码树，可浏览）
  payload/     web-dist.tar.gz（已构建，目标机不需要 Node）
               registry_snapshot.json（1902 行注册表 + 58 条人工注释）
               requirements-embed.lock.txt（现网 freeze）
  config/      settings.docker.json（enc:v1 密文）+ secret.key（配套主钥）
```

**不进包的**：`kbdata/` 原件与 PG 数据 —— 那是状态，几百个文件随包走没有意义，
迁移要整目录搬（库里有记录而原件缺失是永久损坏）。

凭据处理选的是「密文 + 配套主钥」而不是明文：形态与现网一致、与 systemd/容器的注入路径一致，
不需要在部署时多一步加密迁移。代价是两个文件合起来等价于明文，所以包必须当密码本对待 ——
`部署说明.md` 里明写了这一条，包括「放在自动同步的网盘目录里凭据会跟着上云」。

### 已验 / 未验

- ✅ `make_bundle.sh` 全流程跑通，成品 `deploy.sh --dry-run` 自检通过（405 文件 / 17.7 MB）
- ✅ 部署契约测试全绿 + 11 条新判据反向验证全部变红
- ✅ 新脚本在**真实生产**上核对了判定逻辑（只读）：ExecStart 相对路径还原成
  `/opt/dms-ai/tools/embed_service.py`、判定为「独立拷贝需同步」、`PROBE_URL` 推导出
  `http://172.17.0.1:8078`、nginx.conf 运行时副本存在
- ❌ **没有跑真实的端到端部署** —— 业主说的是「这样我就可以一键部署」，
  部署动作留给业主。下一手第一次真跑时重点看两处新接线：3.6 步 embed 同步、
  4 步之后的 nginx.conf 同步。

---

## AX161 · 知识库上传：满了要排队，不是判失败（2026-08-17）

业主给了三张现网截图，指着第一张说：「上传并发要提高，另外如果满了你应该是在队列等待啊，
不应该是失败啊」。

### 一屏红叉的真实原因：闸限的不是「上传」，是「后台解析」

`kb_api.rs` 的 `UPLOAD_GATE` 许可是 **`move` 进后台任务**的（`spawn_ingest_job`），
持有到 parse→chunk→embed 全部结束——几十秒。而前端是**串行**上传的
（`for (const file of files) { await uploadViaXhr(...) }`，一次只传一个）。

于是：传第 5 个文件时被拒，**仅仅因为前 4 个还在后台解析**。用户看到的是
「上传并发已满（同时最多 4 个），请稍后重试」×一屏，而系统一点都不忙 ——
API 容器 233MB / 16GB，8 核，可用内存 6.4GB。

原注释写的是「拿不到许可**直接 429 而不排队**——排队只是把内存问题推迟到队列长度上」。
这个理由在「许可只覆盖读文件那一瞬」时成立，但许可实际覆盖的是几十秒的后台重活：
**「等一会儿就有位子」被表达成了「你的文件失败了」**，用户无从分辨，只能一个个手动重试。

### 两端一起改，纪律是同一条：队列里的东西在等，不是挂了

**服务端**（`crates/server/src/kb_api.rs`）
- `UPLOAD_PERMITS` 4 → 8（20MB × 8 = 160MB 上界，对着 6.4GB 可用内存）
- `try_acquire()` → `upload_permit()`：有位子直接拿，没位子**排队等**，
  队列封顶 `UPLOAD_QUEUE_MAX=32`、等待封顶 `UPLOAD_QUEUE_WAIT=180s`（nginx 是 300s）。
  原注释担心的两件事一件没放：同时在跑的入库任务仍是 8 个，排队连接数也有上限。
- 三个入库入口（`upload` / `reprocess` / `ingest_url`）统一走它。真过载时仍然 429，
  但文案分成两条：「排队已满（8 个在处理、32 个在排队）」与「排队超时（等待超过 180 秒）」，
  用户能分辨该等还是该分批。
- 下载闸**保持 `try_acquire`**：它的许可只在构造响应期间持有（毫秒级），满了说明真在打满带宽，
  等待没有意义。原来那条注释写着「同 UPLOAD_GATE 的理由」，现在两条闸理由不同了，
  改成各自说清，不再互相引用。

**前端**（`web/src/KbPanel.vue`）
- 串行 → 固定宽度取号器，`UPLOAD_PARALLEL = 4`（服务端 8 个位子，留一半给别的用户）
- `send()` 拆成两段：**同步规划**（预校验 + 建行 + 同名提示）→ **并发执行**。
  同名判定原来是隐式依赖串行顺序的，不拆开就会随网络时序漂。
- 429 不再落失败行：指数退避自动重试 4 次（1s/2s/4s/8s），行上显示第几次。
- 超出并发宽度的行显示「排队中」——用户要能一眼分辨「在等」和「挂了」。

### 顺带修掉第二张图那条红：`[500] /parse: TypeError: expected ...Fill`

那份 `重点客户月度共享数据模板.xlsx` 显示「文档服务不可用：文档处理失败」，0 切片。
去现网 `journalctl -u dms-ai-embed` 捞到真因：

```
[500] /parse: TypeError: expected <class 'openpyxl.styles.fills.Fill'>
```

WPS/ERP 导出的 `styles.xml` 里有个**空 `<fill/>`**：openpyxl 的 `Fill.from_tree` 对无子元素的
fill 返 `None`，而 `Stylesheet.fills` 是 `Sequence(expected_type=Fill)` —— `None` 进去就抛。
它发生在读任何单元格**之前**，整份 500，Rust 侧落进 `sanitize_doc_error` 的未分类分支，
于是**把用户的文件问题报成我们的故障**（`connector/src/doc.rs:250` 的头注正是在防这件事）。

`tools/embed_service.py` 加 `_xlsx_open()`：样式表异常时把 `<fills>` 整段换成**等量**的空
patternFill 再从 BytesIO 重读。个数必须守恒（`cellXfs` 按下标引用它，少一个就 IndexError），
颜色我们一个都不用（`data_only` + `values_only` 只取值），丢掉无损。中和也救不回来时
报 `422 unsupported` 并给出「另存为 .xlsx 后重传」——文件问题就说成文件问题。

判据 `_selftest_xlsx_bad_fills` 先断言**原生 load_workbook 确实炸在这个夹具上**，
否则这条判据是恒真的。第一版夹具（`<fill><dmsBogusFill/></fill>`）没能复现，
当场被这条自检挡下来——openpyxl 容忍未知子元素，真正触发的是**空 `<fill/>`**。

### 判据与反向验证

| 判据 | 拆掉它 |
|---|---|
| `upload_gate_queues_instead_of_failing`（闸满时 50ms 内不许返回） | 退回 `try_acquire` → 红 |
| `every_ingest_entry_queues_for_a_permit`（三个入口都走排队取） | 任一处退回 → 红 |
| `upload_gate_still_rejects_when_queue_is_full`（文案带两个上限数） | — |
| `kb-upload-queue.test.ts` ×4（宽度>1 / 规划与执行分段 / 429 重试 / 排队中文案） | 5 种改法逐个 → 红 |
| `_selftest_xlsx_bad_fills` | 退回原生 load_workbook → 红（且复现现网原话） |

全量：`cargo test -p dms-ai-server` **581 通过 / 0 失败**；前端 `npm test` **64 通过 / 0 失败**；
`embed_service.py selftest` 退出码 0。

### 第三张图没动（另一族，留给下一手）

「湖南经营周报（2026-08-17 至 2026-08-23）」问两遍拿到两张不同的反问卡：
① 「意图解析结果未通过一致性校验」② 「意图解析服务暂时不可用」，都是 0 行。
两条的理解缺口都是「尚未确定应使用问数还是知识检索」。现网日志里捞不到对应记录
（那两行是 debug 级，生产日志级别不打），**要复现得先
`RUST_LOG=dms_agent=debug bash scripts/server-restart.sh`**。
注意问的是个**未来区间**（今天 08-17，区间到 08-23），且「经营周报」这个词没进任何指标/意图词表。

### 顺手补一笔：`cargo test --workspace` 在 HEAD 上是红的，我早些时候漏了

跑全量时发现 `dms-semantic` 的两条架构判据在 **HEAD 上就红**（`git stash` 验证过，
不是本轮碰出来的）——是今天前两轮改完只跑了受影响的 crate，没跑全量：

1. `sql_interpolation_is_allowlisted` —— 1024 升级新增的 `ddl::retype_embedding_columns`
   把 `{table}`/`{EMBED_DIM}` 拼进 `ALTER TABLE`；AX159 的仓库排行又新增了
   `{bucket}`/`{column}`/`{where_sql}`/`{UNREGISTERED_BUCKET}`。逐个追到源头后放行并写清理由
   （`where_sql` 里的省区谓词只从 `present::PROVINCE_LABELS` 静态表取值，用户原文一个字不进 SQL）。
2. `every_meta_recall_is_ds_scoped` —— 假红：守卫扫的是**原始行**，而 `derive.rs::metric_alias_of`
   的文档注释里写了「`metrics` 来自 `SELECT name, source_table FROM meta.metric`」。
   修守卫而不是改注释：**逼着人把注释写模糊，是判据在损害可读性**。
   注释里的 SQL 不会被执行，跳过它严格正确，末尾的 `checked >= 10` 空转跳闸保证不会被掏空。

教训写在这儿：**改了 A crate 就只跑 A crate 的测试，会漏掉别的 crate 里扫全仓的架构判据。**
本仓这类判据（drift.rs / 部署契约 / 判据的判据）都是跨 crate 扫源码的，只有 `--workspace` 看得见。

---

## AX162 · 明确选了问数却被知识库抢答 + 中台主数据没进目录（2026-08-17）

业主三张截图，两条问题：

### 一、「我已经明确选择了问数功能，怎么还是走知识库」

链路查清了：`forced_routed_question` 那道闸是好的（合同判成 Knowledge 时强制 Data 会
落澄清卡，不会硬洗）。漏在**后面**——`ask_arms_payload` 两臂都跑（2026-08-14 有意为之，
为的是「合同判 Knowledge 但业务库里其实有」那一族），问数半没取到实质、资料半有命中，
`hybrid::fuse` 就把资料半选成主答案。对 `auto` 档这是对的；对**用户已经明确表态**的档，
等于把他点的那个 chip 静默丢掉。

`knowledge_arm_payload` 加 `forced_data` 形参：这一档下资料半不许当主答案，改成
澄清卡（带 coverage 收据说明问数半为什么没答上来）+ 资料半降为 `kb` 键附属挂上去
（前端本来就渲染这个键，零改动）。**不藏任何东西，但主答案必须是用户点的那条路。**
另外三个入口（深度分析子问句 / MCP / 小程序）显式传 `false` 并各自写明理由。

### 二、「大日期商品」答不了，不是因为没数据，是因为**表没进目录**

业主给了定义（失效日期 < 3 个月）并指出「你完全可以去中台的库存表找失效日期」。查下来：

- `ywzt_ods.scm_warehous_manage` 确实有 `invalid_date 失效日期`、`production_date`、
  `in_stock_quantity`、`batch_number` —— 数据一直在。
- 但仓库只有不透明的 `wms_code`（`GZHYC-777` / `WMS000019` / `9572214`），
  **认不出「京东仓/顺丰仓」**。
- 业主二次指路：「仓库主数据在 master_wms，一般 master 开头的都是主数据」。
  扫了一遍：`ywzt_ods.master_wms`（539 行，`wms_desc` = 仓库名）与
  `ywzt_ods.master_sku`（5325 行，带 `daysToExpire` 等效期档案）**都存在，但都不在目录白名单里**。

`warehouse_catalog.rs` 的 `ASSETS` 是 59 条**手工白名单**，`ywzt_ods` 只有一条，
注释还写着「ywzt_ods 域唯一资产」（2026-08-11 只登记了库存源）。
**表不在白名单 = 模型看不见 = 整族问题只能落到知识库去答「知识库里没有查询方法」。**

补上三件：
1. `master_wms` / `master_sku` 两条 `asset!`（59→61，`ywzt_ods` 1→3），字段合同里点名
   `wms_desc` 与 `daysToExpire`，禁令里点名「不许拿 sup_name（供应商）当承运仓」——
   顺丰同时以物流公司身份出现在供应商列，按它筛是把供货关系错认成仓储关系。
2. 两条 JOIN 边（`scm_warehous_manage.wms_code → master_wms`、`.sku_code → master_sku`），
   按本仓两证纪律实测过：主键唯一（539/539、5325/5325）、左连不丢行（31258 行全留，
   12 行无仓档、0 行无 SKU 档），均 N:1 不扇出。
3. `大日期` 术语：写清阈值默认（今日起 3 个月）、按仓要 JOIN `master_wms` 用 `wms_desc` 匹配，
   以及三条实测出来的坑 —— `t_winc_stock_report` 没有效期列不得替代；`invalid_date` 有
   8888/8889 年哨兵值（实测 MAX 到 8889-02-23）必须过滤；不许拿 `sup_name` 冒充仓库。

**这道题实测有解**（2026-08-17 直连数仓）：京东仓 4 个批次 360 件
（「皇家小虎奶香芝士酱0280G00」，3 批已过期 53–59 天、1 批当日到期），顺丰仓 0 行。

### 判据

| 判据 | 拆掉它 |
|---|---|
| `knowledge_arm_payload` 的 `forced_data`（形参在 / 分支排在资料成形之前 / 资料降为 `kb` 而不是丢掉 / 出口真读 chip） | 四种改法逐个 → 红 |
| `zhongtai_master_data_stays_in_the_catalog` | 五种改法逐个 → 红 |

反向验证时踩到一个坑记下来：**变异测试对 `include_str!` 自扫描的判据要只改第一处** ——
测试里也写着同一个字面量，全量替换会把判据自己的搜索串一起改掉，那次变异是无效的
（表现为「判据没牙」的假象）。

`cargo test --workspace` 全绿；前端 64/64；部署契约绿。

### 三、新服务器答不了（没查，缺访问）

业主用部署包上了一台新服务器，「很多问题原来的服务器能答，新的答不了」。截图里那条是
「本月销售额按省份的分布」不可计算、理由写「问句含未能识别的限定『分布』（解析失败，非合同缺失）」。
已排除两个方向：
- **不是快照缺失**：现网 `meta` 各表行数与我导出的快照逐张吻合（dimension 91 / value_map 1168 /
  sql_exemplar 263 / term 23 / kw_force 52 / memory 98）；
- **不是「分布」没登记**：快照里根本没有这个同义词，而现网这两句都答得出来
  （`direct-agg`，28 行 / 27 行），说明它本来就由代码认，不靠注册表。

「解析失败」指向的是**意图解析那一步**（LLM）在新机器上不通或降级。要继续查得有新服务器的
访问方式 —— 已向业主索要。届时第一条命令是对比两边的
`/api/health` 与 `meta` 各表行数，第二条是看 `journalctl -u dms-ai-embed` 与容器日志。

---

## AX163 · 新服务器「答不了」查清：种子没导，而三层健康检查全绿（2026-08-17）

业主用部署包上了第二台生产机（1.95.7.181），「很多问题原来的服务器能答，新的答不了」。
只读诊断，没动那台机器。

### 四条实据

| 项 | 老服务器 | 新服务器 |
|---|---|---|
| `meta.dimension` / `value_map` / `term` / `kw_force` | 91 / 1168 / 23 / 52 | **一样** |
| `meta.sql_exemplar` | 263 | **173**（缺 90 条人工沉淀） |
| `meta.memory` | 98 | **50**（缺 48 条教训） |
| 快照里那条「最近一个月各战区的订单数分布」样例 | 有 | **0 条** |

决定性的是最后一行：那条样例只在快照里，不在代码种子里。**注册表快照从未导入**。
`registry_snapshot.json` 这个文件根本不在那台机器上（只有 `payload.tar`）。

另外三条：
- **98 条 sql_exemplar 没有向量** —— 库里那些也召回不到；
- **`dms-ai-embed` 单元 `ActiveState=inactive`**，而 8078 上是个**手工起的裸 python 孤儿**
  （pid 212156）在服务 —— 与 2026-08-16 那次 61000 次空转同族，重启机器即失，且不随部署更新；
- 那台跑的代码不含今天的改动（`upload_permit` / `master_wms` 都 0 命中），布局也是旧的
  （源码直接摊在 `/opt/dms-ai/` 根上，没有 `app` 链接、没有 `releases/`、没有 `seed/`）——
  **不是用包里的 `deploy.sh` 上的**。

### 最坏的一点：这三条在 `/api/health` 上**全是绿的**

`ok=true`、`mysql.connected=true`、`pg.extensions` 四个齐、`vector_ready` 三个 true、
`breakers` 全 false。而 `vector_ready` 只覆盖 `datasource`/`element`/`table_doc` 三张表，
**样例表根本不在里面**。于是「部署成功、服务健康、答案变差」——本仓最讨厌的那一类。

### 包的设计缺口（这才是我的账）

`--bootstrap` 是个**要人记得加**的开关，而快照导入藏在它后面。忘了加＝静默少 90 条样例。
「靠记性」不该是判据。三处改在 `tools/bundle-deploy.sh`：

1. **开关换成探测**：五个前置（settings / venv / PG 容器 / web 容器 / systemd 单元）
   任缺其一即自动转 bootstrap。要强行只更新得显式 `--update-only`，并会打印「明知缺什么」。
2. **快照总是导**（幂等 upsert，`docs/DEPLOY.md` 明写重复跑收敛），不再看模式；
   种子上传收口成一个 `seed_upload`，两条路共用。
3. **新增第 5 步上线判据**：拿**包里那份快照自己的行数**当基准逐表对账
   （带上来多少行，库里就该不少于多少行），外加样例向量覆盖率与
   `systemctl is-active dms-ai-embed`——端口有响应不等于单元活着。

判据写的时候踩到一个自己的坑，记下来：`... | while read` 里的赋值活在**子 shell**，
短缺时照样打印 ❌ 却退出码 0，等于没判。改成先落盘再 `while ... < file`。

部署契约新增 5 条钉住上述，逐条反向验证：拆掉任意一条当场变红。

### 给业主的收口动作（我没动生产）

那台机器要恢复到与现网同等水平，跑一次包里的 `bash deploy.sh` 即可（现在会自己探测出
缺 seed/app/releases 并补齐、导快照、最后逐表对账）。孤儿 embed 进程需要人工确认后
`systemctl enable --now dms-ai-embed` 收编 —— 那一步会短暂中断向量服务，留给业主决定时机。

---

## AX164 · 判据挪到「谁部署都躲不开」的地方（2026-08-17）

业主纠正了一个前提：**第二台生产机不是他部署的**（「我是让小龙虾部署的」）。
那 AX163 那条修法就只对了一半 —— 我把探测与对账做进了 `deploy.sh`，
而**不跑 `deploy.sh` 的人根本碰不到它**。手工解包正是那台机器的实际形态。

### 判据要挂在唯一的必经之路上

谁部署、怎么部署，都要过 `scripts/server-restart.sh`（起容器就得跑它）。所以：

- 新增 `scripts/server-verify.sh` —— 一份共享裁决，核对 `/api/health` **答不了**的四件事：
  ① 注册表逐表行数（基准**取自 seed 里那份快照自己**，不写死数字 —— 写死的下个月就是假的）；
  ② SQL 样例的向量覆盖率（health 的 `vector_ready` 只覆盖 datasource/element/table_doc，样例表不在里面）；
  ③ `dms-ai-embed` 是否真由 systemd 托管（端口有响应 ≠ 单元活着）；
  ④ 版本布局是不是带回滚位的 `app`+`releases`（源码平铺 = 手工解包，没有原子切换）。
- `server-restart.sh` 收尾调它，`ADVISORY=1` **只报不拦** —— 缺快照不该让一次正常重启失败，
  但绝不许它不出声。
- `bundle-deploy.sh` 的第 5 步改成调同一份（原来那段内联逻辑删掉）。一份判据两处用。
- `部署说明.md` 头条改成「⚠️ 不要手工解包上传」，把四条代价与「health 全绿」写在最前面，
  并给出验收命令；`docs/DEPLOY.md` 同步。

### 真机实测：一条命令复现了我手工查的全部

在 1.95.7.181 上只读跑（跑完清掉了自己传的临时文件）：

```
✅ meta.dimension：91 / 91      ✅ meta.value_map：1168 / 1168
❌ meta.sql_exemplar：库里 173 行 < 快照 263 行
❌ meta.memory：库里 50 行 < 快照 98 行
❌ SQL 样例向量 75/173
❌ 没有 dms-ai-embed systemd 单元 —— 向量/解析服务没有被托管
❌ 源码平铺在 /opt/dms-ai（没有 app 链接与 releases/）
退出码=1
```

比我手工查的还准一条：那台**根本没有 systemd 单元**（`systemctl show` 返回
`ActiveState=inactive` 我读成了「单元停了」，其实是「单元不存在」——`systemctl cat` 才分得清）。

### 反向验证时第二次踩到同一个形状

九条变异里有三条「仍绿」，查下来**全是判据匹配到了注释**：
`grep -Fq 'server-verify.sh'` 被 `# 判据本体在 scripts/server-verify.sh` 这句注释喂饱，
拆掉真正的调用照样绿。今天早些时候 `every_meta_recall_is_ds_scoped` 是同一个病
（守卫扫到文档注释里的 `FROM meta.metric`）。

**纪律记这里：源码扫描型判据必须钉在「调用/赋值」上，不能钉在「文里提到过」上。**
钉宽了的代价是双向的 —— 要么注释喂饱判据（假绿），要么判据逼着人把注释写模糊（真损失）。
三条断言全部改成钉逐字的调用行，九条变异这才全红。

---

## AX165 · 向量·精排·解析服务做成容器（2026-08-17）

业主：「把本地系统服务做成 docker，docker 包含所有相关依赖和脚本，安装好后自动运行启动，
后续直接让小龙虾一键安装就行」。

### 为什么这是对症的

这套服务此前只以「宿主机 venv + systemd 单元」的形态存在，而**那套形态没有一行在仓库里**。
一天之内为它付了两笔账：
- 第二台生产机上压根没装单元，8078 上是个手工起的裸 python —— 重启即失、部署换代码也不
  跟着变，而 `/api/health` 全绿；
- 换千问那次，`$RUNTIME_ROOT/tools/embed_service.py` 与 release 里那份是两份拷贝，靠人手同步
  （`scripts/embed-sync.sh` 就是给它写的补丁）。

装进镜像后两条一起消失：依赖、代码、启动方式一起进版本库，部署换代码＝重建镜像换容器。

### 落地

| 文件 | 做什么 |
|---|---|
| `docker/embed/Dockerfile` 🆕 | 完整服务镜像：LibreOffice 三件套 + tesseract/chi_sim + `tools/requirements-embed.txt`（**同一份清单**，不抄第二遍）+ `embed_service.py`/`settings.py`。镜像里**一个凭据都没有** |
| `scripts/embed-install.sh` 🆕 | 一键安装：配置自检 → 构建 → 占用探测 → 起容器（`--restart unless-stopped`）→ 起飞自检。幂等 |
| `scripts/embed-sync.sh` | 改成**形态分派器**：容器就重建换容器，systemd 就走原来的同步+比对 |
| `scripts/server-bootstrap.sh` | 第 7 步从「写 systemd 单元」改成「装容器」 |
| `scripts/server-restart.sh` | 解析服务容器名不再写死：按清单 `dms-ai-embed → dms-ai-parser` 找，找到就走 `/kbdata` 同源校验 |
| `scripts/server-verify.sh` | 托管形态认容器或单元；容器还要查重启策略（没策略＝机器重启不回来） |

与 `docker/parser/` 的区别写进了头注：那个是**开发机专用运输壳**（Windows SAC 拦 lxml，
它把 `/embed` 转发给宿主机上游），本镜像是完整服务，不转发任何东西。

### 真机实测（38.76.188.118，测完已还原成 0 容器 0 镜像）

用的是**明文 dummy key 的最小配置**，真实凭据一个字节都没上那台机器。

| 项 | 结果 |
|---|---|
| 镜像构建 | 1.11 GB，apt/pip 分两层，二次构建全走缓存 |
| 解析能力 | **9/9 全绿**：pdf/docx/pptx/xlsx/text/doc/xls/ppt/image |
| 服务自报 | `model=text-embedding-v4 dim=1024 rerank=gte-rerank-v2` |
| 幂等 | 重跑认出「本服务的旧容器」，不要求 TAKEOVER |
| `/kbdata` 同源 | 宿主写文件 → 容器 `/parse` 取回逐字 token |
| **真·开机自启** | `systemctl restart docker` 后容器自己回来，**首次探活即通过** |
| 接管闸 | 外来进程占 8078：无 `TAKEOVER` 退出码 1 + 说清占用者是谁；加 `TAKEOVER` 收掉裸进程并装上，退出码 0 |
| `server-verify` | 认出容器形态并核对重启策略 |

途中抓到一个**只在别人机器上才暴露**的 bug：起飞自检那段 `python3 -c` 用了
`f"...{d.get(\"model\")}..."`，Python 3.10 的 f-string **不许表达式部分含反斜杠**
（PEP 701 到 3.12 才放开）。本机与生产都是 3.12，测试机是 3.10.12 —— 容器全绿、
自检自己 SyntaxError。两处都改成 `%` 格式化。**部署脚本要跑在别人的机器上，
就不能只在自己的 Python 版本上验过。**

### 判据：12 条，逐条反向验证全红

依赖清单共用 / 少 COPY settings.py / 镜像 COPY 凭据 / 不再开机自启 / kbdata 不挂 /
settings 挂载不只读 / 接管闸被拆 / 接管默认翻成开 / 不再构建本镜像 /
bootstrap 不装容器 / bootstrap 又写单元 / 解析容器名写死。

### 「判据被喂饱」这个病今天犯了三次，从一处根治

反向验证连着抓到三批「仍绿」，全是同一形状：
1. 注释里写了同款字面量 → `grep` 命中注释，拆掉真实现照样绿；
2. 剥掉注释还不够 —— **echo/die 的文案**里同样会写那些字面量
   （`step "…--restart unless-stopped…"`、die 里的用法提示），一样能喂饱判据。

根治：`test-deploy-contract.sh` 加 `code_only()`，**所有源码变量统一先剥整行注释**；
判据本身钉「可执行构造」而不是「字符串出现过」（带行继续符 `--restart unless-stopped \`、
带判断骨架 `[ "$TAKEOVER" = 1 ] ||`）。剥注释后既有判据全部仍绿 ——
顺带证明了没有一条老判据是靠注释撑着的。

另外记一条 grep 看不见的事：「把 `TAKEOVER` 默认从 0 翻成 1」是**语义**变更，
语法纹丝不动。所以除了钉闸门骨架，还要单独钉默认值 `TAKEOVER="${DMS_EMBED_TAKEOVER:-0}"`。
