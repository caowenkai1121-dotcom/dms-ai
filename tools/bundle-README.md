# dms-ai 部署包

解压即用。包里已经含**真实配置**（数据库连接、大模型 key），不需要再填任何东西。

---

## ⚠️ 先看这一条：不要手工解包上传

**请跑 `deploy.sh`，不要把包打成 tar 传到服务器上解开。**

不是洁癖，是有代价的。2026-08-17 第二台生产机就是手工解包上的，结果：

| 现象 | 后果 |
|---|---|
| `registry_snapshot.json` 没导入 | **少 90 条人工沉淀的 SQL 样例 + 48 条教训**——「本月销售额按省份的分布」这类问句直接答「不可计算」，而老服务器答得出来 |
| 98 条 SQL 样例没有向量 | 库里有也召回不到 |
| `dms-ai-embed` 是手工起的裸进程 | 没有 systemd 单元，重启机器服务就没了，部署换代码它也不跟着变（**2026-08-17 起这套服务已改成容器**，见下） |
| 源码平铺在 `/opt/dms-ai/` 根上 | 没有 `app` 链接和 `releases/`，**没有回滚位**，也没有原子切换 |

**最要命的是这四条在 `/api/health` 上全是绿的**——`ok=true`、`vector_ready` 三个 true、
`breakers` 全 false。因为 `vector_ready` 只覆盖三张表，样例表不在里面。
部署"成功"、服务"健康"、答案变差，没人会发现。

`deploy.sh` 做的就是这些手工步骤：探测缺什么、补前置、导种子、原子切换、最后逐表对账。
真要手工部署，**至少把最后一步补上**（见下面「验收」）。

---

## 三步部署

### 已经在跑的服务器（日常更新）

双击 `一键部署.cmd`，或在 Git-Bash / Linux / macOS 里：

```bash
bash deploy.sh
```

它会先探测目标机的五个前置（配置 / venv / PG 容器 / web 容器 / 向量服务），
**缺任何一个就自动补齐**，不需要你记得加参数。然后：上传源码 → 服务器构建镜像（5-10 分钟）
→ 让向量服务跟上新代码 → 原子切换 API → 更新前端 → 导入业务字典种子（幂等）
→ **逐表对账验收**。任何一步失败都会退回旧版本，生产不会停在半路。

### 全新机器

同一条命令。`--bootstrap` 仍然接受（强制铺前置），但**不加也不会漏**——探测会发现。

机器上只需要预装 **Docker（含 compose 插件）** 和 **Python 3**。

### 向量·精排·解析服务已经是容器了

不用再装 systemd 单元、不用在服务器上建 venv 装 LibreOffice/tesseract ——
`deploy.sh` 会构建并起 `dms-ai-embed` 容器，依赖与代码都在镜像里，
带 `--restart unless-stopped`，**机器重启自己回来**。单独装/重装：

```bash
DMS_RUNTIME_ROOT=/opt/dms-ai bash /opt/dms-ai/app/scripts/embed-install.sh
```

服务器上已经有旧形态（systemd 单元或手工起的裸进程）在占 8078 时，它会**拒绝并说清占用者是谁**。
确认可以中断向量服务几秒后：

```bash
DMS_EMBED_TAKEOVER=1 bash /opt/dms-ai/app/scripts/embed-install.sh
```

实测（2026-08-17，Ubuntu + Docker 29）：镜像 1.11GB，9 种格式解析全绿
（pdf/docx/pptx/xlsx/txt/doc/xls/ppt/图片 OCR），重启 docker 守护进程后容器自己回来。

### 只想验证包是好的，先不连服务器

```bash
bash deploy.sh --dry-run
```

### 明知缺前置也只更新代码

```bash
bash deploy.sh --update-only
```

会打印「明知缺什么」再继续。除非你清楚自己在做什么，否则别用。

### 连接参数

默认交互输入。要免交互：

```bash
DEPLOY_HOST=1.2.3.4 DEPLOY_USER=root DEPLOY_PW='密码' bash deploy.sh
```

首次连一台新机器会被拒绝——部署脚本**不自动信任陌生主机**（防中间人）。按提示先做一次：

```bash
ssh-keyscan -p 22 <服务器地址> >> ~/.ssh/known_hosts
```

---

## 验收：部署完到底成没成

在**服务器上**跑一条，它给出裁决：

```bash
DMS_RUNTIME_ROOT=/opt/dms-ai bash /opt/dms-ai/app/scripts/server-verify.sh
```

它核对四件 `/api/health` 答不了的事：注册表逐表行数（基准取自包里那份快照自己）、
SQL 样例的向量覆盖率、`dms-ai-embed` 是不是真的由 systemd 托管（端口有响应 ≠ 单元活着）、
以及版本布局是不是带回滚位的 `app`+`releases`。

跑 `deploy.sh` 的话这一步是自动的（第 5/5 步）。手工部署的话，**请务必手动跑一次**。
另外每次 `server-restart.sh` 收尾也会跑它（只报不拦），日志里能看到。

---

## 包里有什么

| 路径 | 内容 | 类别 |
|---|---|---|
| `source/` | 完整源码树（Rust workspace + Vue 前端 + 部署脚本 + 文档） | 代码 |
| `payload/web-dist.tar.gz` | 前端构建产物，**已构建**，目标机不需要装 Node | 代码 |
| `payload/registry_snapshot.json` | 业务字典种子：码值、维度、软删过滤、SQL 样例、教训 | 数据（种子） |
| `payload/requirements-embed.lock.txt` | 现网 venv 的 pip freeze，新机器照它装 | 代码 |
| `config/settings.docker.json` | 真实配置：PG / Doris / DMS-MySQL 的 DSN、千问与 deepseek 的 key、企微凭据、API key | **凭据** |
| `config/secret.key` | 上面那份配置的解密主钥 | **凭据** |
| `MANIFEST.json` | 打包时的 commit、每个文件的 sha256、组件版本 | 元数据 |

`MANIFEST.json` 只记本脚本产出的文件。包根下如果有别的文件（比如你自己打的 tar），
打包时会打印一行 NOTE 提醒——它们不进清单，但同步/传输会带走。

### 不在包里的东西（有意的）

- **知识库原件** `kbdata/`：那是业务数据，几百个文件，随部署走没有意义。迁移机器时要单独整目录搬——
  数据库里有记录而原件缺失，是永久性的数据损坏。
- **PG 数据目录**：同理。换机器请用 `pg_dump` / 卷备份，不要指望部署包。

---

## 关于凭据的两句话

`config/settings.docker.json` 里的 DSN 与 key 是 AES-256-GCM 密文（`enc:v1:` 前缀），
`config/secret.key` 是解它的主钥。**两个文件合在一起等价于明文凭据**——分开放没有意义，
它们必须成对；打包时已经验证过这一对能解开，解不开的包会当场拒绝生成。

这意味着：**这个目录要当成密码本对待**。如果它放在会自动同步的网盘目录里（OneDrive / 百度网盘 / iCloud），
凭据就随同步走了一份到云端。要么把包挪到不同步的本地目录，要么接受这个前提。
换机器部署时，`deploy.sh` 只在服务器上**没有**配置时才装入（`cp -n` 语义），不会覆盖现网那份。

---

## 出问题时

部署脚本每一步都打印做了什么。常见几种：

| 现象 | 多半是 |
|---|---|
| `未找到 bash` | 没装 Git for Windows |
| `连不上 ...` | 主机键不在 known_hosts（见上面的 ssh-keyscan），或地址/密码不对 |
| 停在 `服务器构建失败` | 服务器拉 crates.io 慢。`/root/.cargo/config.toml` 放一份 rsproxy 镜像再重试 |
| 停在 `解析服务无法读取知识库探针` | 向量/解析服务没起来：`journalctl -u dms-ai-embed -n 50` |
| 停在 `HEALTH TIMEOUT` | API 起了但健康检查不过：`docker logs dms-ai-server --tail 50` |
| `发现上次部署遗留容器 dms-ai-server-rollback` | 上次部署中断留下的。核对无误后 `docker rm -f dms-ai-server-rollback` 再重试 |
| 验收报 `meta.sql_exemplar 库里 N 行 < 快照 M 行` | 业务字典种子没导全。重跑 `bash deploy.sh` 即可（导入幂等） |
| 验收报 `dms-ai-embed 单元 inactive` | 端口上多半是个手工起的孤儿进程。`systemctl enable --now dms-ai-embed` 收编（会短暂中断向量服务） |

部署完请人工冒烟三题，覆盖三条不同的答题路径：

1. 「本月销售额」——应走确定性问数，带口径收据
2. 「现在总库存量是多少」——应走库存口径
3. 「市场费用的报销政策是什么」——应走知识库，带引用

更细的运维说明在 `source/docs/DEPLOY.md`。
