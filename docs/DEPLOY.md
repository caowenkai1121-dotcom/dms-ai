# 服务器部署指南

本指南按「全新机器从零到可用」写。已在本机做过**空库部署演练**（2026-08-10：空 PG 库 → 启动 → 导入快照 → 三问实测路由/结果与现网一致）。

## 组件

| 组件 | 形态 | 说明 |
|---|---|---|
| 元数据 PG | `docker/age`（postgres16 + Apache AGE + pgvector + pg_trgm） | 唯一可写库（注册表/知识库/会话/日志） |
| 解析+向量服务 | `tools/embed_service.py`（Python 3.10+） | 文档解析（含扫描件 OCR 档）与 bge 向量化 |
| Rust API | `dms-ai-server`（容器或裸机 exe） | 问数/知识库/数据地图全部 API |
| Web | `web/`（Vue3 构建产物，nginx 托管） | `docker/web` 有现成 nginx 配置 |
| 业务源 | Doris（warehouse）/ DMS 生产 MySQL（production_lookup，仅只读点查） | 部署方提供只读账号 |

## 步骤

### 1. 配置

```bash
cp settings.example.json settings.docker.json   # 容器部署；裸机用 settings.json
```

必填：`pg_url`（自有 PG）、`mysql_targets`（数仓目标 `type: warehouse`）、`mysql_url`（DMS 身份源）、`llm_keys`（各家模型供应商 key）。`service_url`（embed/parser 服务地址）默认 `http://127.0.0.1:8077`，裸机同机部署不必填，容器/跨机时才需改。

**必须设置环境变量 `DMS_SECRET_KEY`（≥32 字节随机串）**：settings 里的凭据落盘即 AES-256-GCM 加密（enc:v1）。不配则密钥由机器指纹派生——容器重建/换机后密文解不开，需重填明文凭据。

### 2. 起依赖

```powershell
# PG（自动建扩展 age/vector/pg_trgm，仅默认库）+ 解析/向量服务 + API（首次构建镜像）
.\scripts\run.ps1
```

裸机 Linux（PG 仍走容器）：`docker compose -f docker/age/docker-compose.yml up -d`；embed 服务 `python tools/embed_service.py serve 8077`（模型自动下载，离线环境先备 `BAAI/bge-small-zh-v1.5`）。

⚠️ 若元数据库不是 compose 默认库（另建的库）：age/vector/pg_trgm 三个扩展都只由初始化脚本建在默认库上，需手动补齐：`psql -d <库> -c "CREATE EXTENSION IF NOT EXISTS age; CREATE EXTENSION IF NOT EXISTS vector; CREATE EXTENSION IF NOT EXISTS pg_trgm"`。缺 age 图谱功能不可用，缺 vector/pg_trgm 向量与模糊召回不可用。

### 3. 初始数据加载（决定问数准确性的关键一步）

服务**启动时自动**完成：全量 DDL 迁移 → 代码种子（指标/维度/术语/JOIN 合同/码值/权限档案）→ 数仓目录探针同步。但还有一半是**数据驱动登记与人工沉淀**（码值字典 938 行、auto 维度 70 条、软删表过滤 35 条、SQL 样例 172 条、教训 18 条……数字为撰写时口径，随现网漂移，以现网导出为准），代码种子里没有，必须从现网快照导入：

```bash
# 现网导出一次（随部署包私下传递，勿进公开仓库——含业务字典值）
python tools/registry_snapshot.py export registry_snapshot.json
# 新部署导入（幂等，重复跑/与代码种子混跑都收敛；--pg-url 可显式指目标库）
python tools/registry_snapshot.py import registry_snapshot.json
```

导入后由服务的「向量自愈」自动回填 embedding：启动即跑一轮，之后每 10 分钟一轮（embed 服务需先就绪；`/api/health` 的 `vector_ready` 三个 true 即完成）。

可选刷新（都幂等，建议初次部署后各跑一次）：

```bash
dms-ai-server meta autodiscover        # 数据字典自适应（字典变了重跑即自适应）
dms-ai-server meta datamap-build       # 数据地图：静态画像推断（joinable/synonym/distribution/correlated 边）
dms-ai-server meta lineage-build       # 血缘反推（DWS/ADS ← ODS）
dms-ai-server meta datamap-calibrate   # 使用轨迹校准（query_log → co_occurs 边）
```

### 4. 验证

```bash
curl http://127.0.0.1:8100/api/health
# ok:true；mysql.connected:true 且 mysql.session_read_only:true；vector_ready 三个 true；pg.extensions 含 age/vector/pg_trgm
```

判官回归（问数正确性的验收尺，76 题）：

```bash
DMS_REGRESSION_TIMEOUT=240 python tools/regression.py
```

三道人工冒烟：「本月销售额」（应 direct-agg/verified）→「销售额按门店」（应 direct-derive 带推导标注）→「待确认对账单有多少」（应明确不可计算卡）。知识库：上传一篇 PDF 问一句内容题，回答应带引用。

## 运维注意

- `insecure_login_fallback` 保持缺省/false；对外调用用 `mcp_keys` 发 API key（`X-API-Key` 头）。
- 知识库原文目录（`kb_root`）要持久化卷，且与解析服务看到的字符串路径一致。
- 日志脱敏已内建；`meta.query_log` 是全状态审计面（`/api/audit/sql`），别清。
