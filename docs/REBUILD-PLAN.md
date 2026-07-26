# DMS 智能助手 · 彻底重构：调研报告 + 技术选型 + 项目计划

> 日期：2026-07-23。红线：**DMS 数据库（MySQL `xh_dms`）只读，只允许 SELECT**；三份源码（xh-dms / xh-dms-fornt / xh-xcx）只读不改。
> 本文档 = 架构师提案，选型项待用户拍板后开工。

---

## 一、现有系统调研结论（4 路并行深读，均带源码证据）

### 1.1 DMS 后端（D:\code\hjxh_code\xh-dms，Java）

- **栈**：Java 21 + Spring Boot 3.3.7 + MyBatis-Plus 3.5.10 + Druid + Redis/Redisson + RabbitMQ + **Sa-Token 1.37.0**（认证）。5 模块：gateway(Controller 144 个)→service→infrastructure→domain→support；另有独立 **mcp 模块**（销售订单 9 工具的 MCP 服务端，`/dms/mcp`，X-API-Key 鉴权）。
- **规模**：约 200 张业务表（`t_` 前缀），Mapper 205 个。
- **认证**：`POST /login`，token 头 **`x-access-token`**（simple-uuid，存 Redis，30 天）；万能密码、超管代登录、外部系统 `agent-token` 免登通道齐备。
- **RBAC 9 表**：`t_employee`（含 `administrator_flag` 超管）/ `t_role` / `t_menu`（`api_perms` 后端权限点、`web_perms` 前端按钮点）/ `t_role_menu` / `t_role_employee` / `t_role_data_scope` / `t_department`（`tree_path_id` 树）/ `t_employee_department` / `t_position`。
- **🔴 数据权限（必须 1:1 保留）**：MyBatis Executor 拦截器 + `@DataScope` 注解（15 处），核心类 `DefaultEmployee.java` / `CustomerDataScopeStrategy.java` / `DataScopeViewService.java`：
  - 基础档（data_scope_type=1）按 view_type：`0本人 / 1本部门 / 2本部门及下级 / 3结算客户(哨兵) / 10全部` → 过滤 `owner_manager`；多行取 **MAX**。
  - 定制档（type=2）：`101下属(递归) / 102客户分组 / 103客户经理团队` → 过滤 `customer_code`；多行取**并集**。
  - joinSql 模板 `#or` 分段，跨维度 **OR**，整体括号后 **AND** 进原 WHERE。
  - **哨兵语义**：`in (-1)` = 拒绝（0 行）；空集/不注入 = 放行全部。二者相反，复刻必须分清。
  - 超管短路：`administrator_flag=true` 或 ADMIN_SYSTEM 或 roleCode='admin' → 不注入。
  - 多角色不合并：当前激活单角色生效（roleCode 在 Redis）。
  - scope 集合按 loginName 缓存 Redis 当日过期；角色变更异步清缓存+踢下线。

### 1.2 DMS 前端（D:\code\hjxh_code\xh-dms-fornt）

- **栈**：**Vue 3.5 + Vite 8 + antdv-next（Ant Design Vue 次世代）+ Pinia + vue-router(hash) + ECharts 5.4.3**，SmartAdmin 底子。与重构目标栈同代。
- **认证约定**：token 在 localStorage `smart_admin_user_token`，请求头 `x-access-token`，响应 `{code,msg,data}`，**code===1 成功**；登录返回 menuList/pointsList → 动态建路由 + `v-privilege` 按钮权限。
- **嵌入机制现成**：菜单 `frameFlag/frameUrl` → iframe 渲染（`side-layout.vue:46`）；**无微前端框架**。首页 `views/system/home/index.vue` 已被裁空（只剩公告两列），顶栏已改名「DMS AI」。
- 环境里已有 `dms-ai.huangjiaxiaohu.com` / `/chat-api` 代理 / `ai_chat_token`——原本就规划过挂 AI 服务。
- 生产地址：`https://dms.huangjiaxiaohu.com/dms/#/`，API `.../dms-api`。

### 1.3 小程序（D:\code\hjxh_code\xh-xcx）

- uni-app + Vue3，**微信个人小程序**（appid `wx3d85c8985ed67d23`），与企业微信零关联。微信 code 登录 + 多角色选择 + permissionList，token 头同 `x-access-token`。业务覆盖进销存/门店要货配送/设备/巡店（已有 AI 巡店对话页）/台账/商城。
- 结论：三端#3「企业微信」是**全新通道**（用企微自建应用），不是小程序改造。小程序仅作移动端形态参考。

### 1.4 SuperSonic（tencentmusic/supersonic，commit de60be3）可迁移架构

- **双引擎**：Headless BI（语义层：指标/维度/实体/术语建模）+ Chat BI（对话编排），Chat 复用语义层控幻觉。
- **NL2SQL 流水线**（LLM 介入点极少）：
  `Mapper`（词典 HanLP + 向量双召回，确定性）→ `Parser`（规则路径 0-LLM / LLM 路径产 **S2SQL** 中间表示）→ `Corrector`（Schema/Where/GroupBy/Agg/Time 逐项确定性校正）→ `Translator`（S2SQL→物理 SQL，Calcite，确定性）→ `Executor`。
- **few-shot 闭环**：历史问答复核沉淀样例库，相似召回注入 prompt（>0.989 强制入选）。
- **协议**：parse/execute 两段式；`MsgDataType` + 列语义 `showType(NUMBER/DATE/CATEGORY)` 驱动图表自动决策（MetricCard/Trend/Pie/Bar/Table 优先级短路）；下钻/相似问/推荐维度。
- **权限**：数据集/列/行三级，SPI 可替换——我们替换为 DMS 自有 datascope 语义。

### 1.5 基础设施现状

- 生产 MySQL：`203.0.113.10:3306/xh_dms`（只读账号已有）；dev 环境 `1.95.167.10`。
- 本地 PG 17.7 容器（ParadeDB，:15432）：**pgvector 0.8.1 + pg_search(BM25) + pg_trgm 已装**；无 Apache AGE。
- LLM：DeepSeek（key 已有，fast/precise 双模型）。
- 企微：corpid `wwd8304eb63d2cb14c`，secret 已给（存 gitignore 配置，不入库不入文档）。
- Windows 编译约束：cargo 需 PowerShell + WinLibs mingw 置 PATH 最前（旧项目验证过的坑）。

---

## 二、目标架构（参考 SuperSonic，适配 Rust/Vue3/PG）

```
┌─ 三端 ────────────────────────────────────────────────┐
│ ①独立 Web(Vue3)  ②DMS 首页 iframe 嵌入  ③企业微信应用 │
└──────────────┬────────────────────────────────────────┘
               │ parse/execute 两段式 API + ViewSpec 呈现协议
┌──────────────▼────────────────────────────────────────┐
│ Rust 后端（单二进制）                                  │
│ ├ 认证适配层：DMS token SSO / 企微 OAuth / 本地登录    │
│ ├ 权限内核：principal 加载 + scope 计算 + SQL AST 注入 │←1:1 复刻 @DataScope
│ ├ 语义层：指标/维度/术语建模（存 PG）                  │
│ ├ Mapper：词典(pg_trgm)+BM25(pg_search)+向量(pgvector) │
│ ├ Parser：确定性模板(单号直查/高频聚合) + LLM→S2SQL    │
│ ├ Corrector：schema/时间/聚合/去重口径 确定性校正      │
│ ├ Translator：S2SQL→MySQL 物理 SQL（sqlparser AST）    │
│ ├ Executor：生产 MySQL 只读 SELECT（会话级 READ ONLY） │
│ ├ 记忆闭环：few-shot 样例/口径记忆/语义缓存（pgvector）│
│ └ 图关系：客户-商品-员工-部门 边表+递归CTE（PG）       │
├───────────────────────────────────────────────────────┤
│ PG 17：元数据/语义层/向量/会话/缓存   MySQL：业务数据只读│
│ LLM：DeepSeek（OpenAI 兼容 HTTP，无框架依赖）          │
└───────────────────────────────────────────────────────┘
```

要点：
- **LLM 只产 S2SQL（逻辑层：指标/维度名），物理 SQL 确定性生成** —— SuperSonic 控幻觉核心，照搬。
- **权限注入在 Translator 之后、执行之前**，AST 级注入（sqlparser-rs），语义按 §1.1 规格 1:1。
- 业务数据不搬家：直连生产 MySQL 只读；PG 只放自有元数据。

---

## 三、技术选型（待拍板）

### 3.1 后端 Rust Web 框架

| 方案 | 优势 | 劣势 |
|---|---|---|
| **axum（推荐）** | tokio 官方生态，tower 中间件体系（限流/超时/追踪现成）；社区最活跃、LLM 时代样例最多；与 sqlx/reqwest 组合零摩擦；类型安全提取器 | 无内置 OpenAPI（可加 utoipa） |
| actix-web | 基准性能第一梯队；成熟 | actor 遗产复杂度；与 tokio 生态偶有版本摩擦；本项目瓶颈在 LLM/DB，框架性能差异无意义 |
| poem | 内置 OpenAPI 生成，简洁 | 社区小，长期维护风险 |

**推荐 axum**。配套：`sqlx`（MySQL+PG 双驱动、纯异步）、`sqlparser`（SQL AST 权限注入/安全校验）、`reqwest`（LLM）、`tokio`。**不用 ORM**（取数系统 SQL 是一等公民，ORM 反碍事）。

### 3.2 前端 UI 组件库（Vue3 + Vite + TS + Pinia + ECharts 已定，无争议）

| 方案 | 优势 | 劣势 |
|---|---|---|
| **Ant Design Vue（推荐）** | 与 DMS 前端 antdv-next **同设计语系**——iframe 嵌入首页时视觉无缝；企业中后台组件最全（表格/抽屉/级联） | 包体较大（按需引入可控） |
| Element Plus | 生态最大、文档全 | 视觉与 DMS 现有界面不一致，嵌入后「两张皮」 |
| Naive UI | TS 最友好、轻 | 同上不一致；表格能力弱于 antd |

**推荐 Ant Design Vue**。图表 ECharts 5（DMS 同款）；BI 呈现层自定义（KPI 卡/结果面板/报告页），遵循 dataviz 纪律（单色系、禁双轴、showType 驱动决策）。

### 3.3 图数据库能力

| 方案 | 优势 | 劣势 |
|---|---|---|
| **边表 + 递归 CTE（推荐起步）** | 零新依赖，现容器即用；客户-商品-员工-部门-单据关系图谱本质是 FK 边，递归 CTE 足够（部门树/下属递归/共购关系已被旧项目验证） | 超深图遍历表达力弱 |
| Apache AGE 扩展 | Cypher 查询表达力强 | 现 ParadeDB 镜像**没有** AGE，需换镜像或自编译；运维+学习成本；当前问答场景用不到深图算法 |

**推荐边表+CTE 起步**，schema 设计预留升级 AGE 的映射路径（边表结构与 AGE 图模型同构）。向量=pgvector、关键词=pg_search BM25、模糊=pg_trgm——检索三件套现容器全齐。

### 3.4 认证/SSO 方案（三端统一）

| 端 | 方案 |
|---|---|
| ①独立 Web | 本地登录页：转发 DMS `POST /login`（admin/hjxh@2025 可测）→ 换自有会话 token；完全继承 DMS 账号密码与角色 |
| ②DMS 嵌入 | iframe URL 携带 DMS 的 `x-access-token` → 后端调 DMS `GET /login/getLoginInfo` 验真 + 拿角色 → 颁自有 token。**零新增凭证体系**（备选：HMAC 签名 URL，需 DMS 侧配合，暂不用） |
| ③企微 | 企微 OAuth（code→userid）→ userid↔`t_employee` 映射（手机号/loginName 对照，映射表存 PG，管理页可维护）→ 颁自有 token |

权限计算不依赖 DMS 接口：principal（员工/部门/角色/data_scope 行）从 MySQL 只读现算，scope 语义按 §1.1 规格复刻，判官对拍验收。

### 3.5 嵌入 DMS 首页方式（源码只读约束下）

- **方式 A（推荐，零源码改动）**：DMS 系统管理界面新增外链菜单（`frameFlag=1, frameUrl=新系统地址?sso=token`）——利用现成 iframe 机制，由**你在 DMS 后台点配置**（不是我写库，红线不破）。首页替换可同法：菜单管理里把「首页」指向外链。
- 方式 B：你方修改 `home/index.vue` 一处嵌 iframe（源码只读是对我，你团队可改）。
- 我交付：嵌入页（自适应高度/免登/主题跟随）+ 配置指引文档。

### 3.6 LLM 接入

DeepSeek 现有 key（fast=分类/判断，precise=SQL 生成），OpenAI 兼容 HTTP 直调，**不引入 LangChain 类框架**（Rust 生态无成熟对应物，裸 HTTP + 自建 prompt 装配更可控）。embedding：本地 embed 服务（bge 类模型，旧项目已有 `embed_service.py` 可复用）或 DeepSeek embedding API——M2 时实测选。

---

## 四、项目计划（里程碑制，每轮连库实测验收）

| 里程碑 | 内容 | 验收标准 |
|---|---|---|
| **M0 骨架** | 新仓库 `dms-ai/`（Rust workspace + Vue3 web/）；PG schema（语义层/会话/缓存/向量表）；MySQL 只读连通（会话 `SET TRANSACTION READ ONLY`）；CI 脚本（build/test/restart .ps1） | 双库冒烟 SELECT 通过；`cargo test` 绿；前端 dev 起 |
| **M1 权限内核（关键路径，先行）** | principal 加载（employee/role/data_scope/department 树）；scope 计算（基础 MAX/定制并集/哨兵-1/超管短路/多角色单激活）；sqlparser AST 注入器 | **判官对拍**：≥4 类真实角色（超管/XXJL/城市经理/无角色 fail-closed），注入 SQL 与 Java 端语义逐条对齐，越权 0 行 |
| **M2 语义层+检索** | information_schema+注释采集→PG；指标/维度/术语建模（销售/售后/费用/库存/客户/商品先行）；词典+BM25+向量三路召回；口径规则库（去重/哨兵 9999/快照表/空列等已知 44+ 坑迁移为 Corrector 规则） | 召回命中率抽测；口径规则单测全绿 |
| **M3 NL2SQL 流水线** | Mapper→Parser（规则模板：单号直查/高频聚合 0-LLM；LLM→S2SQL）→Corrector→Translator→Executor；parse/execute 两段式 API；few-shot 记忆闭环 | 回归题集 ≥30 例连库对真值全绿；单号直查 <500ms；LLM 路径 <30s |
| **M4 前端 V1** | Vue3 对话流 + ViewSpec 呈现（showType 图表决策/KPI 卡/表格分页排序/CSV 导出/报告页）+ 多会话 + 追问上下文 | Playwright 走查主场景；双主题 |
| **M5 三端打通** | ②DMS token SSO 换签 + 嵌入页 + 配置指引；③企微应用（OAuth 免登 H5 + 消息推送日报/预警 + userid 映射管理） | 三端真机各过一轮登录→提问→出图 |
| **M6 智能增强** | 实体解析（商品/客户/员工模糊→编码锚定）；图关系问答（边表+CTE）；下钻/相似问/推荐；语义缓存；洞察规则 | 实测案例集通过；重复问 <1s |
| **M7 判官门禁** | 回归扩到 ≥50 例（含权限档/追问链/口径题）；并发 30 路；性能护栏；安全审计（SQL 注入/越权/限流） | 全量回归+并发+安全三绿方可上线 |
| **M8 部署** | 服务器部署（systemd/docker）、embed 服务常驻、监控日志、DMS 后台菜单配置协助 | 生产三端可用 |

节奏：M0-M1 先行（权限是地基）；M2-M3 主体；M4 可与 M3 并行。每里程碑末给你演示+验收。

---

## 五、风险与对策

| 风险 | 对策 |
|---|---|
| 生产库慢查询拖垮 DMS | 只读会话 + 语句超时 + LIMIT 护栏 + 连接池上限 + 避开无索引大扫（旧项目已知：`t_sales_order_detail.sku_code` 无索引） |
| LLM 幻觉答错数 | SuperSonic 式确定性夹层（Mapper 前置/Corrector 兜底/S2SQL 收窄）+ 口径规则库迁移 + 回归判官 |
| 权限复刻偏差 | M1 判官对拍 Java 真值；哨兵/空集语义单测锁死；fail-closed（无角色=拒） |
| 企微映射缺失 | userid↔员工映射管理页 + 未映射友好提示 |
| 旧库数据坑（重复/快照/哨兵值） | 44+ 条已验证口径直接迁入 Corrector 规则与 schema 警告，不重新踩 |

## 六、选型定案（2026-07-23 用户拍板）

1. 后端框架：**axum** ✅
2. 前端 UI：**Ant Design Vue** ✅
3. 图能力：**Apache AGE**（用户选择上真图扩展）✅
   - 落地方式：新建 PG 容器（基于 `apache/age` 镜像或自建 Dockerfile：PG16/17 + AGE + pgvector），端口 15433，不动现有 xhcrm-postgres；关键词检索用 PG 原生 tsvector + pg_trgm（pg_search 为 ParadeDB 专有打包，不强求共存）；M0 实测扩展共存性后定稿。
4. 旧 `dms-copilot`：**归档留作参考** ✅——权限注入器、44+ 条连库验证口径、回归题集迁移复用；新项目全新目录 `dms-ai/` 重写。
