# dms-ai 部署包

解压即用。包里已经含**真实配置**（数据库连接、大模型 key），不需要再填任何东西。

---

## 三步部署

### 已经在跑的服务器（日常更新）

双击 `一键部署.cmd`，或在 Git-Bash / Linux / macOS 里：

```bash
bash deploy.sh
```

问你服务器地址和密码，然后自动完成：上传源码 → 服务器构建镜像（5-10 分钟）→ 让向量服务跟上新代码 →
原子切换 API → 更新前端 → 健康检查 → 清理旧产物。任何一步失败都会退回旧版本，生产不会停在半路。

### 全新机器（从零起）

```bash
bash deploy.sh --bootstrap
```

比上面多做一段前置：起元数据 PG（含 age / vector / pg_trgm 三个扩展）、建 Python venv、
装 systemd 单元 `dms-ai-embed`（向量 + 精排 + 文档解析）、建 nginx 前端容器、
装配置与密钥、最后导入业务字典种子（1900 多行，**问数准确性的一半靠它**）。

前置每一步都幂等：已经有的不动，可以重复跑。机器上只需要预装 **Docker（含 compose 插件）** 和 **Python 3**。

### 只想验证包是好的，先不连服务器

```bash
bash deploy.sh --dry-run
```

### 连接参数

默认交互输入。要免交互（比如写进自己的脚本）：

```bash
DEPLOY_HOST=1.2.3.4 DEPLOY_USER=root DEPLOY_PW='密码' bash deploy.sh
```

首次连一台新机器会被拒绝——部署脚本**不自动信任陌生主机**（防中间人）。按提示先做一次：

```bash
ssh-keyscan -p 22 <服务器地址> >> ~/.ssh/known_hosts
```

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

部署完请人工冒烟三题，覆盖三条不同的答题路径：

1. 「本月销售额」——应走确定性问数，带口径收据
2. 「现在总库存量是多少」——应走库存口径
3. 「市场费用的报销政策是什么」——应走知识库，带引用

更细的运维说明在 `source/docs/DEPLOY.md`。
