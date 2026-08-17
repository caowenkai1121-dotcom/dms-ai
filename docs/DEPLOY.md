# 服务器部署指南

本指南按「全新机器从零到可用」写。已在本机做过**空库部署演练**（2026-08-10：空 PG 库 → 启动 → 导入快照 → 三问实测路由/结果与现网一致）。

## 组件

| 组件 | 形态 | 说明 |
|---|---|---|
| 元数据 PG | `docker/age`（postgres16 + Apache AGE + pgvector + pg_trgm） | 唯一可写库（注册表/知识库/会话/日志） |
| 解析+向量+精排服务 | `tools/embed_service.py`（Python 3.10+） | 文档解析（含扫描件 OCR 档）+ 千问 `text-embedding-v4`(**1024 维**，2026-08-17 由 512 升级) 向量化 + `gte-rerank-v2` 精排 |
| Rust API | `dms-ai-server`（容器或裸机 exe） | 问数/知识库/数据地图全部 API |
| Web | `web/`（Vue3 构建产物，nginx 托管） | `docker/web` 有现成 nginx 配置 |
| 业务源 | Doris（warehouse）/ DMS 生产 MySQL（production_lookup，仅只读点查） | 部署方提供只读账号 |

## 最省事的路：用部署包

`bash tools/make_bundle.sh <输出目录>` 打出一个**自带真实配置**的独立包（源码 + 已构建的前端 +
业务字典种子 + `settings.docker.json` 与配套 `.secret_key`）。拿到包的人：

```bash
bash deploy.sh              # 更新已经在跑的服务器
bash deploy.sh --bootstrap  # 全新机器：连 PG/venv/systemd/web 容器一起铺
bash deploy.sh --dry-run    # 只自检不连服务器
```

包里的 `deploy.sh` 不重写任何服务器侧行为，它只是把包内产物喂给 `source/tools/deploy_update.sh`
（`DEPLOY_SRC_TAR` / `DEPLOY_WEB_TAR` 两个逃生门），所以目标机不需要 git，也不需要 Node。

打包脚本会在生成前**校验配置与密钥成对**（密文解不开就拒绝出包）、校验 `kb_root=/kbdata` 与
`listen` 绑 0.0.0.0，并拒绝把带凭据的成品写进仓库工作区（`.secret_key` 这个名字不被 `.gitignore`
命中，落进工作区一次 `git add .` 就进历史）。

⚠️ 包 = 密码本：密文 + 配套主钥合在一起等价于明文凭据。放在会自动同步的网盘目录里，
凭据就跟着同步走了一份。

### ⚠️ 不要把包打成 tar 手工传上去解开

2026-08-17 第二台生产机就是这么上的，代价（四条在 `/api/health` 上**全是绿的**）：
业务字典种子没导 → SQL 样例少 90 行、教训少 48 行，「销售额按省份的分布」这类问句
直接不可计算；98 条样例没有向量；`dms-ai-embed` 是手工起的裸进程（单元 `inactive`，
重启机器即失、部署换代码也不跟着变）；源码平铺在 `/opt/dms-ai/` 根上，没有 `app` 链接
与 `releases/`，**没有回滚位**。

`health` 的 `vector_ready` 只覆盖 `datasource`/`element`/`table_doc` 三张表，
样例表根本不在里面 —— 所以「部署成功、服务健康、答案变差」不会有人发现。

### 验收：`scripts/server-verify.sh`

```bash
DMS_RUNTIME_ROOT=/opt/dms-ai bash /opt/dms-ai/app/scripts/server-verify.sh
```

核对 `health` 答不了的四件事：注册表逐表行数（基准取自 seed 里那份快照自己，不写死数字）、
SQL 样例向量覆盖率、`dms-ai-embed` 是否真由 systemd 托管、版本布局是否带回滚位。
`deploy.sh` 的第 5/5 步与 `server-restart.sh` 的收尾（`ADVISORY=1` 只报不拦）都调它 ——
一份判据，谁怎么部署都躲不开。

下面是不用包、手工从零起的完整路径。

## 步骤

### 1. 配置

```bash
# 运行时状态目录与源码目录分开：app 是指向 releases/<版本> 的稳定链接
export DMS_RUNTIME_ROOT=/opt/dms-ai
mkdir -p "$DMS_RUNTIME_ROOT/kbdata"
cp settings.example.json "$DMS_RUNTIME_ROOT/settings.docker.json"   # 容器部署；裸机用 settings.json
```

容器部署必须把 `settings.docker.json` 的 `kb_root` 改为精确的 `"/kbdata"`。启动脚本会硬校验这一项；保留示例默认值 `data/kb` 会把原件写进容器镜像层，解析服务和下次重建后的容器都看不到。

必填：`pg_url`（自有 PG）、`mysql_targets`（数仓目标 `type: warehouse`）、`mysql_url`（DMS 身份源）、`llm_keys`（各家模型供应商 key）。`service_url` 指向解析/向量服务：Rust API 在容器、Python 服务在宿主机时使用宿主机可达地址（现网是 `http://host.docker.internal:8078`，靠 `server-restart.sh` 的 `--add-host host.docker.internal:host-gateway` 解析），不能沿用容器内的 `127.0.0.1`。

**必须把 `DMS_SECRET_KEY`（≥32 字节随机串）持久化到 `$DMS_RUNTIME_ROOT/.secret_key`**。🔴 只有**全新空库**部署才现生成：钥匙是 `sha256(DMS_SECRET_KEY 原始字节)`，拿一把新钥匙配一份既有的 `enc:v1:` 密文配置，启动会直接拒绝（「敏感字段解密失败」）——带配置迁移时必须把 settings 与 `.secret_key` **成对**搬过去。启动脚本从这里注入容器；settings 里的凭据落盘即 AES-256-GCM 加密（enc:v1）。运行时密钥丢失后，容器重建/换机将无法解密既有配置。

### 2. 起依赖

```powershell
# PG（自动建扩展 age/vector/pg_trgm，仅默认库）+ 解析/向量服务 + API（首次构建镜像）
.\scripts\run.ps1
```

裸机 Linux（PG 仍走容器）的这一整段前置 —— PG 容器、Python venv、systemd 单元 `dms-ai-embed`、
`/kbdata` 软链、nginx 前端容器 —— 已经收口成一个幂等脚本，不要再手敲：

```bash
DMS_RUNTIME_ROOT=/opt/dms-ai DMS_SEED_DIR=/opt/dms-ai/seed bash <release>/scripts/server-bootstrap.sh
```

它从 `$DMS_SEED_DIR` 读 `settings.docker.json` / `secret.key`（**已存在则保留现网那份，绝不覆盖**），
从 `pg_url` 反解密码与绑定地址起 PG 并核对 age/vector/pg_trgm 三个扩展，建 venv 装
`tools/requirements-embed.txt`（`cryptography` 是必填：`enc:v1:` 凭据靠它解，缺了 embed 服务起不来
而 API 侧只表现为检索变差），写 systemd 单元并探 `/health`，最后建 `dms-ai-web` 容器
（`--add-host host.docker.internal:host-gateway` 不能省，nginx 启动期解析不到就 emerg 拒启、全站宕）。

### 向量·精排·解析服务：容器形态（2026-08-17 起）

这套服务不再是「宿主机 venv + systemd 单元」，而是一个容器：

```bash
DMS_RUNTIME_ROOT=/opt/dms-ai bash scripts/embed-install.sh
```

`docker/embed/Dockerfile` 把依赖（LibreOffice 三件套、tesseract+chi_sim、
`tools/requirements-embed.txt` 里那十个包）与代码（`embed_service.py` + `settings.py`）
一起装进镜像；容器带 `--restart unless-stopped`，**机器重启自己回来**。

换形态的原因是两笔实账：① 第二台生产机上压根没装单元，8078 上是个手工起的裸 python ——
重启即失、部署换代码也不跟着变，而 `/api/health` 全绿；② 单元跑的
`$RUNTIME_ROOT/tools/embed_service.py` 与 release 里那份是两份拷贝，靠人手同步。
装进镜像后这两个问题从根上消失：部署换代码＝重建镜像换容器。

- **接管旧形态**要显式开：`DMS_EMBED_TAKEOVER=1`（会中断向量服务数秒，时机由运维定）。
- 镜像里**一个凭据都没有**：settings 运行时只读挂到 `/app/settings.json`，
  `DMS_SECRET_KEY` 运行时注入。
- `/kbdata` 与 `dms-ai-server` 挂**同一个宿主目录** —— 解析接口收到的是路径不是字节，
  指错目录会稳定 404。`server-restart.sh` 的预检会核对这一条（按容器名清单找：
  `dms-ai-embed` → `dms-ai-parser`）。
- 存量的 systemd 形态仍兼容：`scripts/embed-sync.sh` 认形态分派 ——
  容器就重建镜像换容器，单元就同步文件 + 重启 + 比对 sha256。

2026-08-17 在 38.76.188.118 实测：镜像 1.11GB，解析能力 9/9 全绿
（pdf/docx/pptx/xlsx/text/doc/xls/ppt/image），`systemctl restart docker` 后容器自己回来、
首次探活即通过；外来进程占着 8078 时无 `TAKEOVER` 非零退出。

知识库解析接口接收的是 `/kbdata/<doc_id>.<ext>` **路径，不是文件字节**，所以 Rust 容器和解析服务必须读取同一宿主目录：

- 解析服务运行在宿主机：`scripts/server-restart.sh` 会幂等建立 `/kbdata -> $DMS_RUNTIME_ROOT/kbdata`；若 `/kbdata` 已指向别处则在停止旧服务前失败，不会覆盖。
- 解析服务使用名为 `dms-ai-parser` 的容器：该容器必须把 `$DMS_RUNTIME_ROOT/kbdata` 以 bind mount 挂到 `/kbdata`。启动脚本会核对 mount 类型、真实源目录和读探针。

启动 API 容器：

```bash
DMS_RUNTIME_ROOT=/opt/dms-ai bash /opt/dms-ai/app/scripts/server-restart.sh
```

脚本启动前会验证：配置 `kb_root=/kbdata`、密钥至少 32 字节、持久目录可写；随后向配置中的真实 `service_url` 发送最小 `/parse` 请求，必须从 `/kbdata` 读回同一份唯一探针，再检查该服务 `/health` 的 `ok=true`、文本解析可用且 xlsx/pdf/docx 至少一种可用。即使机器上另有一个健康的 `dms-ai-parser`，配置指错服务或指向另一份目录也会在停止旧 API 前失败。

⚠️ 若元数据库不是 compose 默认库（另建的库）：age/vector/pg_trgm 三个扩展都只由初始化脚本建在默认库上，需手动补齐：`psql -d <库> -c "CREATE EXTENSION IF NOT EXISTS age; CREATE EXTENSION IF NOT EXISTS vector; CREATE EXTENSION IF NOT EXISTS pg_trgm"`。缺 age 图谱功能不可用，缺 vector/pg_trgm 向量与模糊召回不可用。

### 3. 初始数据加载（决定问数准确性的关键一步）

服务**启动时自动**完成：全量 DDL 迁移 → 代码种子（指标/维度/术语/JOIN 合同/码值/权限档案）→ 数仓目录探针同步。但还有一半是**数据驱动登记与人工沉淀**（码值字典 938 行、auto 维度 70 条、软删表过滤 35 条、SQL 样例 172 条、教训 18 条……数字为撰写时口径，随现网漂移，以现网导出为准），代码种子里没有，必须从现网快照导入：

```bash
# 现网导出一次（随部署包私下传递，勿进公开仓库——含业务字典值）
python3 tools/registry_snapshot.py export registry_snapshot.json
# 新部署导入（幂等，重复跑/与代码种子混跑都收敛；--pg-url 可显式指目标库）
python3 tools/registry_snapshot.py import registry_snapshot.json
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
curl -fsS http://172.17.0.1:8100/api/health | python3 -c \
  'import json,sys; d=json.load(sys.stdin); print(d); raise SystemExit(0 if d.get("ok") is True else 1)'
# ok:true；mysql.connected:true 且 mysql.session_read_only:true；vector_ready 三个 true；pg.extensions 含 age/vector/pg_trgm
```

`tools/deploy_update.sh` 使用严格 SSH 主机密钥校验。首次部署前请从可信渠道核对服务器指纹，
再把主机键写入 `~/.ssh/known_hosts`；也可用 `DEPLOY_KNOWN_HOSTS=/path/to/known_hosts`
指定专用文件。脚本不会自动接受陌生主机，避免密码部署被中间人劫持。
部署包来自当前工作区中受 Git 管理且仍存在的文件，以及未忽略的新文件，而不是只取 `HEAD`；
因此可先在本机完成测试再直接发布尚未提交的修复，待提交删除也不会让 tar 因缺文件失败。
`.gitignore` 排除的密钥、配置、`kbdata`、构建缓存不会进入包。源码先解到独立
`$DMS_RUNTIME_ROOT/releases/<版本>` 并完成镜像构建，再原子切换 `$DMS_RUNTIME_ROOT/app` 链接；
重启失败会恢复旧容器和旧 release，不会在原目录混合覆盖。
若 `dms-ai-web` 容器存在，部署脚本读取其 `/usr/share/nginx/html` 的宿主 bind 源，在宿主解包并校验 `index.html`，原子切换目录后重启容器；新目录未就绪或哈希不一致会自动恢复旧目录。这样兼容容器内只读挂载，不再尝试向只读 nginx 根目录复制。只有该容器确实不存在时才明确打印 `SKIP`，留给现网的独立 Web 发布流程处理。

判官回归（问数正确性的验收尺，76 题）：

```bash
DMS_REGRESSION_TIMEOUT=240 python3 tools/regression.py
```

三道人工冒烟：「本月销售额」（应 direct-agg/verified）→「销售额按门店」（应 direct-derive 带推导标注）→「待确认对账单有多少」（应明确不可计算卡）。知识库：上传一篇 PDF 问一句内容题，回答应带引用。

## 运维注意

- `insecure_login_fallback` 保持缺省/false；对外调用用 `mcp_keys` 发 API key（`X-API-Key` 头）。
- `/opt/dms-ai/app` 是当前 release 的稳定符号链接；实际源码在 `/opt/dms-ai/releases/<版本>`。`settings.docker.json`、`.secret_key`、`kbdata` 必须留在 `DMS_RUNTIME_ROOT`，不要复制进 release 目录。首次从旧版实体 `app/` 升级时，脚本会把它迁入 releases 并保留为回滚版本。
- `git archive` 和 `registry_snapshot.py` 都不包含 `kbdata`。迁移 `kb.doc/kb.chunk` 所在 PG 时，必须同时备份并恢复整个 `$DMS_RUNTIME_ROOT/kbdata`，否则数据库有记录但原件永久缺失。
- 旧版错误脚本可能把失败上传写到 `/opt/dms-ai/app/kbdata`。修复部署前先只读核对；确认是同批原件后用 `cp -an /opt/dms-ai/app/kbdata/. /opt/dms-ai/kbdata/` 恢复，校验数量/大小后再在页面点“重新处理”，不要直接覆盖或删除旧目录。
- 日志脱敏已内建；`meta.query_log` 是全状态审计面（`/api/audit/sql`），别清。
