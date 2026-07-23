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

## 下一步
- M2 语义层+检索（information_schema 采集→PG、指标/维度建模、词典+BM25+向量三路召回、口径规则库迁移）。
- M1 遗留：绑定注册表迁 PG 元数据（M2 一并）；scope 结果加进程内缓存（当日过期，对齐 Java Redis 缓存策略）。
