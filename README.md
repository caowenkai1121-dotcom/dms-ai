# dms_agent

DMS 智能助手：自然语言问数、深度 BI、企业知识库、DMS 首页免登嵌入和企业微信入口。

## 核心能力

- **准确问数**：语义指标、值域词典、业务图谱、SQL 闸门、执行校正和数据权限共用一条受控链路。
- **双模式**：精简模式优先确定性快路由；深度模式由多阶段 Agent 生成总览、同比/环比、结构、趋势、明细、图表和末尾 AI 经营分析。
- **企业知识库**：多格式解析、OCR、目录层级、跨文档关联、混合召回、DMS 角色共享和引用定位。
- **三端一致**：独立 Web、DMS 首页和企业微信共用 DMS 账号状态、角色及数据范围。
- **运行时热切换**：管理员可新增、修改、删除、测试并切换分析数据库和模型；主模型无视觉能力时才使用备用多模态模型。

## 数据与安全边界

- 默认销售事实固定为 `sales_dw.dws_off_offline_sale_dfn`；销售额、销量、成本、收入、毛利和毛利率均从该 DWS 事实聚合。
- Doris/分析库承载聚合、趋势、排行和深度 BI。
- DMS 生产 MySQL 只允许单表、索引等值、显式列、小 `LIMIT`、短超时的只读点查；禁止 JOIN、聚合、范围扫描、排序、子查询和写操作。
- 所有 DSN、数据库密码、LLM key、企业微信 secret 只存在于 `settings.json` 或 `settings.docker.json`，两者均被 gitignore。
- DMS 三份源代码仓库保持只读；首页替换件位于 [`integrations/dms-home`](integrations/dms-home)。

详细配置见 [`docs/CONFIG.md`](docs/CONFIG.md)，**服务器部署（含初始数据加载）见 [`docs/DEPLOY.md`](docs/DEPLOY.md)**，三端认证见 [`docs/EMBED.md`](docs/EMBED.md)。

## 技术结构

```text
kernel -> connector -> policy ----+
             |                    |
             +-> semantic -------+-> agent -> server
             +-> knowledge ------+

web: Vue 3 + Vite + ECharts
meta: PostgreSQL + Apache AGE + pgvector + pg_trgm
business: Doris/MySQL-compatible warehouse; DMS MySQL only for point lookup
```

## 配置

1. 从 `settings.example.json` 创建本机 `settings.docker.json`。
2. 填写自有 PG、只读业务源、DMS 身份源和模型供应商。
3. 生产 DMS 目标必须声明为 `production_lookup`，Doris/中台分析库声明为 `warehouse`。
4. 保持 `insecure_login_fallback=false`。

真实配置文件不能提交、不能写入镜像层。仓库应上传：

- `settings.example.json`：字段形状和占位符。
- `crates/semantic/migrations/*.sql`、`docker/age/init/*.sql`：元数据、知识库和扩展初始化的版本化 SQL。
- 不上传 `settings.json`、`settings.docker.json`、知识库原文、日志、构建产物。

业务语义种子和元数据库迁移由服务启动时自动执行，不需要人工在 DMS 业务库运行 SQL。

## 本机启动

前置条件：Docker Desktop、Node.js 20+、PowerShell 7。

```powershell
# PG、解析/向量服务、Rust API（首次会构建镜像）
.\scripts\run.ps1

# Web
cd web
npm install
npm run dev
```

默认地址：Web `http://localhost:5180/`，API `http://127.0.0.1:8100/`，自有 PG `127.0.0.1:15433`。

若只需重建并启动 API：

```powershell
.\scripts\serve.ps1 -Build
```

## 验证

Windows 本地产物受 Smart App Control 限制，Rust 构建和测试统一在容器执行：

```powershell
.\scripts\docker-test.ps1
cd web
npm run build
```

部署前还应执行回归、深度 BI 合约、知识库评测和浏览器 E2E；任何失败都不能用生产 MySQL 的复杂查询绕过。
