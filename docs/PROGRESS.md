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

## M5a 三端认证 + DMS SSO 嵌入（2026-07-23，已验收）
- `auth.rs`：会话 token 体系（uuid，12h 闲置 TTL 活跃滑动续期，>1000 项清理）+ DMS token 验真（调 getLoginInfo 拿 loginName）。
- 端点：`POST /api/sso`（验真 DMS token→颁会话 token）；`POST /api/ask` 身份优先级 = Authorization Bearer 会话 token > body.login_name（开发）。
- 前端：嵌入 boot（URL dms_token→自动 SSO→隐藏登录框「DMS 免登」）+ ask 带 Bearer。
- DMS 登录用国密 SM4 ECB（密钥 `1024lab__1024lab` 硬编码）+ 图形验证码（无法自动化，故 SSO 走验真路线不自登录）。
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

## 下一步（M6c/M7）
- 指标注册表继续扩充(售后率/毛利等) + 维度注册表扩充(仓库/品牌/客户分类等，先连库坐实口径)。
- M6c：图关系+行级权限；实体锚定；graph sync 定时刷新。
- M5c：端#1 SM4 登录转发（密钥 1024lab__1024lab + 图形验证码流程）；企微/DMS 真 token 生产联调（企微 errcode 60020 → 生产加可信 IP 白名单）。
- M7 判官门禁：回归题集扩到 ≥50 例 + 并发 + 安全审计。
