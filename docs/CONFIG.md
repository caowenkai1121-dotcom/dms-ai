# settings.json 配置说明

`settings.example.json` 是形状样例（JSON 不能带注释，故说明放这里）。运维只需关心下面几个键。

⚠️ **键名打错 = 启动失败，不是静默忽略**（`Settings` 上有 `#[serde(deny_unknown_fields)]`）。
实测报文（把 `mcp_keys` 少写一个 s）：

```
settings.json 解析失败（键名打错会在此硬失败，见 docs/CONFIG.md）
  ⟵ unknown field `mcp_key`, expected one of `mysql_url`, `pg_url`, `listen`, ... , `insight_enabled`
```

`expected one of` 后面是**全量已知键清单**，照它改；前半句里的路径就是实际读到的那份
（就近查找 `settings.json` → `../settings.json` → `../../settings.json`；容器里是挂载进去的 `/app/settings.json`）。

为什么宁可起不来：`mcp_key` 少写一个 s 曾经让 `/api/mcp` **永久 404 且零提示**，
「功能被静默关掉」比「启动时一句明确报错」难查一个数量级。**多余的历史键请删掉，别指望被忽略。**

## 凭据加密（`enc:v1`，D1）

敏感字段在 settings.json 里以 **AES-256-GCM 密文**落盘，格式 `enc:v1:<base64(nonce‖ciphertext‖tag)>`。
**落盘密文、内存明文**：服务启动时把文件里的明文敏感字段幂等改写为密文（已是密文的原样），
进程内照旧按明文使用 —— 手写/粘贴明文凭据照常可用，下次启动自动加密；页面保存的也是明文，
服务端加密后才落盘。任何 API 都**永不回传**凭据本体（目录只给脱敏 host 与「已配置」布尔）。

**加密覆盖的字段**：`mysql_url` / `pg_url` / `pg_ro_url` / `llm_api_key` / `wework_secret`、
`llm_keys` 与 `datasources` 的每个值、`mysql_targets` 每个目标的 url（旧字符串与 `{url}` 两种形态）、
`mcp_keys` 的**键名**（值是 login_name，非密）。其余字段（listen、llm_base_url 等）不加密。

**密钥（永不落盘、永不进日志）**，两选一：
- `DMS_SECRET_KEY` 环境变量（**推荐**，≥32 字节任意串，sha2-256 派生）—— 唯一可跨机迁移的形态；
- 未配置时按机器指纹（hostname+username）派生：**跨机/跨用户不可迁移，容器重建会换指纹**
  （容器 hostname 是容器 ID）。docker/生产部署务必配置 `DMS_SECRET_KEY`。

**丢 key / 换机**：密文解不开时服务**启动失败**（报错指回 DMS_SECRET_KEY，不会拿密文去连库）；
恢复 = 配上原来的 `DMS_SECRET_KEY`，或把敏感字段重填为明文（启动时重新加密）。
没有「找回」通道 —— GCM 认证失败即拒解，这是设计如此。

**判官/工具链**：`tools/settings.py` 的 `load()` 透明解密（同一份派钥逻辑），
Python 判官零改动；首次使用需 `pip install cryptography`（仓库 .venv 已装，
明文配置如 settings.docker.json 模板不需要它）。

## 数据库

| 键 | 作用 | 不填的后果 |
|---|---|---|
| `mysql_url` | DMS 身份、角色与数据权限源（只读），只供 `auth_mysql` 使用，禁止承载分析查询 | 身份认证与权限计算不可用 |
| `mysql_targets` | 【业务查询源热切换】目标必须显式声明 `url` 与 `type`。Doris/分析库用 `{"url":"mysql://…","type":"warehouse"}`；生产 DMS 轻点查用 `{"url":"mysql://…","type":"production_lookup"}`。DSN 只在 settings.json：kv 只存名字、API 只给脱敏 host | 旧纯字符串目标按 `production_lookup` 失败关闭；没有目标则拒绝启动，绝不回退 `mysql_url` |
| `pg_url` | 自有 PG 的 **owner 角色**（可写）：`meta.*` / `kb.*` / `chat.*` 都在这里 | 起不来 |
| `pg_ro_url` | **上传表格源问数时用的只读角色**（见下） | 上传照样入知识库、照样能检索，但那张表**问不了数**（错误文案指回这个键） |
| `datasources` | 额外数据源：**`dsn_ref` 键名 → 明文连接串** | 只有 DMS 主源 |

**分析库切换（AX73）**：设置页新增或修改 `mysql_targets` 后即可热切换，保存即生效；
`meta.kv['mysql_target']` 优先，其次选 `doris_warehouse`，最后才选目录中的首个非 DMS 目标。
目标类型不是端口推断：`warehouse` 才允许聚合、趋势、图谱和元数据探针；
`production_lookup` 只允许物理索引最左列命中的单表等值点查，固定显式投影、`LIMIT <= 50`、
两秒超时，禁止 JOIN、LIKE、排序、聚合和全库 schema 探针。
连接不上或只读校验不过的目标**换不进去**（旧池原样保留）。删除当前目标时会先切到
`fallback_db_target` 选出的其他分析目标；没有其他目标就拒绝删除。`dms` 是受保护权限源，
不能选择、不能删除，也不会在 Doris/分析库失败时成为回退。
⚠️ 口径声明（指标/维度/码表/权限档案）按 **DMS schema** 登记：切到同构库（中台镜像）
照常；schema 不同的库会响亮报错，不是静默错答。

### 当前默认销售口径与生产库红线

切换分析目标不会改变默认经营口径。当前销售事实固定为
`sales_dw.dws_off_offline_sale_dfn`，统计时间固定为 `order_date`：

- 销售额=`SUM(amount)`，销量=`SUM(qty)`；
- 不含税成本=`SUM(cost_excluding_tax)`，不含税收入=`SUM(revenue_excluding_tax)`，
  毛利额=`SUM(gross_profit)`；
- 毛利率=`SUM(gross_profit) / NULLIF(SUM(revenue_excluding_tax), 0)`，必须聚合后相除；
- `storecode/storename` 表示客户编码/客户名称，不是门店编码/门店名称；
- 该事实已含退货负数，禁止再拼旧发货/退货 `UNION` 或重复冲减；
- 订单数独立读取 `dms_ods.t_sales_order`，按有效订单的 `sales_order_code` 去重，
  不能使用 DWS 行数。

`mysql_url` 指向的生产 DMS MySQL 不承担分析查询。确需业务点查时，只允许单表、索引条件、
小 `LIMIT`、短超时；禁止 JOIN、UNION、子查询、聚合、无界排序和大扫描。复杂统计必须选择
Doris 分析目标，连接失败时不得静默回退生产 MySQL。

### `pg_ro_url` 必须满足两条

1. **看不见 `meta` / `kb` / `chat` 三个 schema**。`PostgresSource::connect` 会自检，
   填 owner 角色会被**拒绝启动**（架构文档 §3 的 F3）。原因：上传表格源是被 **LLM 生成的 SQL** 查询的，
   若这个角色能读 `kb.chunk` 就等于任何人都能把全员上传的文档、他人的问答历史查出来。
2. 能 `SELECT` 将来每一个 `up_*` schema。**角色名必须叫 `dms_ai_ro`，或者是它的成员**：

```sql
CREATE ROLE dms_ai_ro LOGIN PASSWORD '***';
REVOKE ALL ON SCHEMA meta, kb, chat, public FROM dms_ai_ro;
-- 若 pg_ro_url 用的是别的角色名，把它加进这个组：
-- GRANT dms_ai_ro TO <那个角色>;
```

授权由 `OwnedStore::create_upload_table` 在建表时自动做（`GRANT USAGE ON SCHEMA` +
`GRANT SELECT ON ALL TABLES`），角色不存在则整段跳过。

**为什么不能只靠 `ALTER DEFAULT PRIVILEGES`**（本文档早先就是这么写的，那是不够的）：
它只覆盖「将来在某 schema 里建的**表**」，PG **没有**「将来建的 schema 自动授 USAGE」这种设置。
少了 schema 级的 `USAGE`，`SELECT` 权限再全也是 `permission denied for schema up_xxx` ——
表现为**知识库检索与建表都正常、只有问数死掉**，是最难归因的那种半可用。

### 为什么 `pg_url` 不在 `datasources` 映射里

这是刻意的：谁把某个数据源的 `dsn_ref` 填成 `"pg_url"`，应该在「dsn_ref 未配置」上**失败**，
而不是连上一个能读全员文档的角色。库里永远只存 `dsn_ref` 键名，明文口令只在本文件出现一次。

## LLM（双供应商热切换）

| 键 | 作用 | 不填的后果 |
|---|---|---|
| `llm_provider` | 供应商目录选哪家：`qwen`（千问 dashscope）/ `deepseek`。缺省按 `llm_base_url` 推断 | 按 base_url 推断 |
| `fallback_vision_provider` | 主供应商无 vision 模型时使用的备用多模态供应商，只保存供应商名；key 仍从 `llm_keys` 读取 | 主供应商无视觉能力时，图片调用返回明确能力错误 |
| `llm_keys` | **各家供应商的 key**：`{"qwen": "sk-…", "deepseek": "sk-…"}`。页面切换到哪家取哪家 | 切到没配 key 的那家会拒绝并指回这个键 |
| `llm_base_url` / `llm_api_key` | **文件供应商**那家的地址与 key（老配置兼容语义，等价于 `llm_keys[llm_provider]`） | 与目录默认合并（目录补缺省字段） |
| `llm_model_fast` / `llm_model_precise` | 文件供应商的两档模型名（覆盖目录默认） | 用目录默认（qwen3.7-flash / deepseek-chat） |
| `llm_extra_body` | 供应商特有请求参数（千问 `enable_thinking:false` —— **布尔** false，省 21 倍延迟 35 倍 token）。**禁含 `messages`/`model`**（启动即拒） | 目录默认（千问带 enable_thinking:false） |

**运行时切换不需要重启**：设置页（`/#/settings`）保存 → 写 `meta.kv['llm_provider']` +
进程内热切换，下一个请求就用新配置；它**优先于** `llm_provider` 文件键。
`meta.kv` 只放非密钥（key 永远只在 settings.json，不入库不进响应）。

**图片能力按供应商自动兼容**：主供应商声明了 vision 模型时直接使用主供应商；主供应商
没有 vision（例如 DeepSeek）时，自动解析 `fallback_vision_provider` 对应供应商的 vision 模型
与 key。备用未配置、缺 key 或也不支持视觉时，返回明确的能力错误，不猜供应商、不复制 key。
`GET /api/llm/capabilities` 返回最终可用的 `vision_provider`、`vision_model` 与
`vision_fallback`；`POST /api/vision/chat` 是 Rust 应用侧统一的多模态调用入口。

设置接口：`POST /api/admin/settings/fallback-vision`，请求体中的 `provider` 写供应商名；
传 `null` 或空字符串即清除。保存前完整解析主/备用配置，失败时旧内存配置保持不变，成功后
下一次调用立即生效。接口不返回或记录 key，并且与其他系统设置接口一样同时要求
`administrator_flag` 和 DMS 登录名精确等于 `admin`。

知识库图片上传与重处理也复用同一条运行时视觉路由：主供应商有 vision 就直接识别，主供应商
没有 vision 才调用 `fallback_vision_provider`。AI 图片识别失败时才降级到文档服务的本地 OCR，
并在文档状态中显示固定降级提示；供应商错误、地址和 key 不进入日志或文档状态。权限校验与
文件去重先于视觉调用，越权请求和已经成功入库的重复文件不会消耗模型调用。

**DeepSeek 思考模式默认被关掉**（目录默认 `{"thinking": {"type": "disabled"}}`）。
官方默认是**开**（effort=high），而思考模式下 `temperature`/`top_p` **不生效**
（官方原文「设置了也不会生效」）—— 它会静默拆掉本系统的温度分档
（首轮 0.1 确定性 / 重试 0.5 / SC 投票独立性），CoT 还成倍烧延迟与 token。
想开：文件供应商路径下用 `llm_extra_body` 覆盖目录默认（自行承担温度失效的代价）。

## 对外 MCP（`mcp_keys`）

`POST /api/mcp` 让 n8n / Dify / DataEase 直接调我们的问数与知识库检索。

| 键 | 形状 | 不填的后果 |
|---|---|---|
| `mcp_keys` | `{"<api-key>": "<login_name>"}` | **`/api/mcp` 恒 404，功能关闭** —— 对外面默认关比默认开重要 |

- **一 key 一员工**。key 建议 32 位随机串；轮换＝改配置重启。
- **数据权限等于所映射员工登录后的权限**：请求经 `load_principal` → `compute_scope` → 同一条闸门，
  没有「MCP 就是超管」的旁路；`kb_search` 的可见文档集也按该员工的 `kb.acl` 判。
- ⚠️ **明文 key 与 DSN 同级敏感**：只在本文件出现一次，不入库、不进日志（日志只写前 4 位 + 长度）、
  不进任何响应。**别写进 docker 镜像层**（与 `settings.json` 同款坑）。
- 已知天花板：**多角色账号走 MCP 会被拒**（`load_principal` 不替用户默认选角色 —— 那是修过的越权面）。
  真要支持得让 key 映射带上角色码，等第一个真实需求再加。

### REST API key（同一份 `mcp_keys`，D10）

`mcp_keys` 同时是 REST 侧的 API key 表：集成方/脚本调普通 REST 端点（`/api/ask` 等）时，
不必先换会话 token，两种头任选其一：

```
X-API-Key: <api-key>
# 或
Authorization: Bearer <api-key>
```

- 命中 `mcp_keys` → 身份 = 所映射员工，随后走 `load_principal` 同一条加载链
  （员工有效性与角色逐次现算，**多角色账号 fail-closed**，与 MCP 同语义，没有「key 就是超管」）。
- 未命中 → 落回会话 token 判定（两个头都带是合法形态）；**显式递错 `X-API-Key` 直接 401，
  不降级**到任何 login_name 自报回退；都没有 → 401，与会话端点现有文案一致。
- key 比较是常量时间逐字节比较（防时序侧信道逐位探测前缀）；日志不回显 key 本体。
- `Bearer` 是双义头：先按会话 token 解（本服务颁发的 UUID），解不开再按 API key 查 ——
  老客户端的 `Bearer <会话 token>` 行为一字不变。

## 小程序接入（`xcx_auth_base`）

商城小程序（uni-app）用户就是 DMS 员工。小程序登录后持有商城/DMS 后端签发的
`x-access-token`，本服务**不自己验签**，而是 server-to-server 回调签发方：

```
GET {xcx_auth_base}/login/getLoginInfo
x-access-token: <token>
→ {"code":0,"data":{...},"msg":""}        # code=30007/30012 = token 失效
```

| 键 | 形状 | 不填的后果 |
|---|---|---|
| `xcx_auth_base` | `"https://dms.huangjiaxiaohu.com/dms-api"`（生产值） | **`/api/xcx/*` 恒 404，功能关闭** —— 与 `mcp_keys` 同一条「对外默认关」纪律 |

- 端点：`POST /api/xcx/ask`（问答，与 `/api/ask` 同一条管道）与 `GET /api/xcx/me`（登录态探活）。
- 响应协议：`{"code":0,"data":...,"msg":""}`；token 失效回 HTTP 401 + `{"code":30007,"msg":"token 失效"}`
  （小程序拦截器按 30007 弹登录框）；校验服务不可用回 502 + `code:500`。
- **数据权限等于该员工 Web 登录后的权限**：token 只换成 login/role，随后过 `load_principal`
  （员工禁用 / 多角色未选照样被拒），没有「小程序就是超管」的旁路。
- 进程内缓存：token → 身份 60s TTL、上限 1000 条（满员淘汰最旧），重复问不重复打外部；
  代价是 token 失效/切角色最多滞后 60s 生效。
- 这只是一个 URL、不是凭据：不参与 `enc:v1` 加密，但同样不进任何 API 响应。

## 其它

| 键 | 默认 | 说明 |
|---|---|---|
| `service_url` | `http://127.0.0.1:8077` | Python 侧服务：`/embed` 与 `/parse`、`/chunk` **同进程同端口**（裁决 V1）。一个键，不许拆两个——拆了必然出现「一个填 /embed 一个填根」的配置陷阱 |
| `kb_root` | `data/kb` | 知识库落盘根目录。原始文件名**不进路径**（恒 `<doc_id>.<ext>`，防路径穿越） |
| `kb_max_mb` | `20` | 单文件上限（产品口径 ≤20MB）；同时是 `/api/kb/upload` 的 body limit（axum 默认 2MB 会先触发） |
| `kb_rrf_weights` | `{"metadata":0.2,"relation":0.25,"kg":0.3,"ext_kb":0.2}` | 【Y3】RRF 四路**辅助**召回（元数据/关系扩展/图谱/外部 KB）的融合权重；正文直接命中的四路恒 1.0 不可调。**缺省 = 原编译期常量**（不写或照抄示例都零行为变化）。负值与 NaN/Inf 在启动加载与页面保存两处一律拒绝；`0` = 该路不加权。缺某路 = 该路保留旧值（部分覆盖）。保存即生效（`/api/kb/*` 与 `/api/mcp` 每次请求取配置快照）。⚠️ 例外：`/api/ask` 主链暂用默认值（调用点在 main.rs，Y3 包未接线，见 `agent/src/answerers/knowledge.rs` 注释） |
| `listen` | `127.0.0.1:8100` | —— |
| `insight_enabled` | `true` | 【AI 解读】`POST /api/analysis` 是否真的调 fast 模型。置 `false` = **止血阀**（模型欠费/被限流时只返确定性口径说明，零 LLM 花费），前端不用改。<br>为什么敢默认开：解读是**独立端点**（前端点「AI 解读」才调），取数链路一次 LLM 都不多；评测与回归走 CLI `ask` 与 `/api/ask`，**结构上根本不经过它**，p95 基线不会被污染，也不依赖任何人记得把开关关掉 |
| `sc_samples` | `1` | 【SC】自一致采样数：LLM 路径整条跑几次、按**结果指纹**（只看值不看列名）投票取多数派。`1` = 关，与本项引入前逐字等价。<br>**为什么会想开**：两轮执行级评测都停在 34/38 而失败集换了两个 —— 同一道题今天与 gold 逐值一致、评测那次却高 30%，误差主要来自模型本身（温度已是 0.1）。<br>**代价是线性的**：`3` 即最多 3 倍 precise LLM 调用 + 3 倍取数（前两次指纹一致会提前收工，故常见情形只多付一次）。单次就 20s+ 的重题（`t_sales_order` 全扫那类）要先算清超时预算。<br>**多数派缺席时不静默挑一个**：返回首次结果 + `caliber_note` 明说数字不可信。<br>⚠️ 判官（CLI `ask` 子命令）用的是同一个配置值，不是写死 1 —— 否则「开了 SC 有没有变好」永远量不出来 |
| `wework_redirect_url` | 无 | 企业微信 OAuth 精确回调地址，必须与企微后台一致。生产只接受 HTTPS；服务端不会从 Host 或代理头推导，避免开放重定向与 state 绕过 |

## 文档解析器依赖（Python 侧，不是配置项但会决定「哪些文件能进知识库」）

`GET /health` 的 `parse_ok` 照实反映装了哪几个 —— **它是唯一真相源，别按「应该装了」推断**。

| 类型 | 依赖 | 许可 | 现状 |
|---|---|---|---|
| `.txt` / `.md` / `.csv` | 标准库 | —— | 恒可用 |
| `.xlsx` / `.xlsm` | `openpyxl` | MIT | **已装可用**（纯 Python） |
| `.pdf` | `pypdf` | **BSD-3** | **已装可用**；逐页纯文本，无标题层级 |
| `.pdf`（更好） | `pymupdf4llm` / `PyMuPDF` | **AGPL-3.0** | **刻意未装** —— 见下 |
| `.docx` | `python-docx` | MIT | 装了但**本机不可用**：见下 |
| `.pptx` | `python-pptx` | MIT | 同上 |

**PDF 的三级降级与许可分层是刻意的**（`embed_service.py::_p_pdf`）：
`pymupdf4llm`（保标题层级）→ `fitz` → `pypdf`。前两级同源且都是 **AGPL-3.0**，
对内部商用工具而言 AGPL 的网络条款要法务点头，**那是业主的裁决，不该由部署脚本替他装**。
第三级 `pypdf` 是 BSD-3、纯 Python，于是 **PDF 在零 AGPL 依赖下就能用**，代价是丢标题层级。
想要层级自行装前两级即可，`parse_ok` 会照实变。

**`.docx` / `.pptx` 在本开发机不可用，原因不是许可也不是代码**：两者都依赖 `lxml`，
而 `lxml` 的编译扩展被本机的 Smart App Control 拦掉
（`DLL load failed while importing etree: 应用程序控制策略已阻止此文件`，与裁决 二·E 同一个拦截器）。
Linux 部署下正常。**本机因此无法端到端验这两种类型** —— 记在这里免得被当成「装了就好了」。

## 部署纪律

- **镜像里不装配置**：`docker/server/Dockerfile` 不再 `COPY settings.json`，改运行时挂载
  （设置页需要热修改时使用可写挂载 `-v ./settings.json:/app/settings.json`；禁用页面编辑时才可加 `:ro`）。
  已进过镜像层的口令与 API key 建议轮换。
- 生产 `dev_token` 必须留空（它等于一个「任意 login_name 冒充」的入口，放行时会打 warn 留痕）。
