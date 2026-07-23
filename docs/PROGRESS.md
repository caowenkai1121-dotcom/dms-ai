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

## 下一步
- M3 NL2SQL 流水线：LLM→S2SQL→Corrector（吃 pitfall/警告）→Translator→只读执行；确定性模板（单号直查/高频聚合）；parse/execute 两段式 API；embed 服务接入激活向量召回与 few-shot。
- 遗留：inject 绑定注册表迁 PG；scope 进程内缓存（当日过期）；code_dict 142 条用于结果 CASE 翻译。
