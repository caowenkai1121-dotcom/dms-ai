# DataFoundry + Yuxi 全功能集成计划（调研终稿 → 分期实施）

> 调研沉淀：`docs/research/datafoundry.json`、`docs/research/yuxi.json`（逐机制带 file:line 证据）。
> 本地克隆：`target/tmp/research/{datafoundry,Yuxi}`（不进 git）。
> 本文是全功能对账：**每个功能点 → 已有 / 移植（到哪、怎么验）/ 不做（为什么）**。

## 0. 定位判断

- **DataFoundry = 问数侧的「任务治理」**：它的 NL2SQL 本身不如我们（词法护栏、无语义注册表、按问题动态生成口径断言），它强在**分析协议闭环、证据引用、上下文工程、会话拓扑、审计回放、多数据源适配**。
- **Yuxi = 知识库侧的「RAG 质量与平台化」**：它强在**解析引擎矩阵、分块 preset、BM25+向量混合检索、reranker、文档级知识图谱+PPR、评估闭环、沙盒/审批/子 agent、队列可靠性**。
- **我们的护城河不能动**：口径单一事实源（sales_fact 合同/注册表）、权限注入三段闸门、确定性模板、实体卡、判官回归。**集成 = 在既有架构纪律内吸收对方的机制，不是替换。**

## 1. 全功能对账（按「先准确性、再质量、后平台」排序）

### A. 问数可验证性（DataFoundry → 我方）
| # | 功能 | 现状 | 处置 |
|---|---|---|---|
| A1 | 分析协议状态机（requirements→断言→claim 容差提交+审计绑定） | 有口径校正器+复核，无 claim 级容差提交 | **移植（简化形）**：深度报告每条量化结论生成时记录 evidence 行与容差校验（`validate_evidence_insight` 已做一半——编号/数字闸门），补「claim 数值必须等于绑定查询值」硬校验 + query_log 关联。验收：构造错数 claim 必被拦 |
| A2 | SQL 审计全状态（blocked/timeout/failed 也落库） | query_log 只记成功路径 | **移植**：gate 拒绝/超时/失败也写 query_log（status 列）。验收：红线题在 query_log 里能查到 blocked 行 |
| A3 | 证据引用 EvidenceRef（追问引用上轮产物/选中区域） | 多轮只有 prev 问句+SQL | **移植**：追问可带 `ref`（上轮结果/选中行），服务端解析进改写上下文。验收：多轮题集加一道引用题 |
| A4 | 上下文包 token 预算+快照 | gather 有 bytes 预算（40k），无快照回放 | **部分移植**：保留预算/裁剪日志，存每轮 context 摘要到 query_log（不重做整套 ContextPackage） |
| A5 | 三级语义降级链（live→快照→物理 schema，trust 标签） | 目录探针失败即硬失败 | **移植**：catalog 探针失败时用上次成功快照+`trust=degraded` 标记进 trust envelope，不再启动即死 |
| A6 | 28 数据源适配（含 ES/Mongo/ClickHouse/Oracle…） | MySQL/PG/Doris/上传表 | **按需移植**：先做 ClickHouse/ES（我方生态可能出现），适配器接口照 `DataSourceAdapter` 三方法（inspectSchema/previewTable/runSqlReadonly）。其余挂清单待业务点名 |
| A7 | 凭据引用化（credential_ref+AES-GCM） | settings.json 明文 | **移植**：敏感字段（DB 密码/LLM key）加密落盘，UI 只回显掩码（已有 keep_secret 语义，补加密存储） |
| A8 | 行数/超时三层取最小 + 字段 masking | MAX_ROWS/EXEC_TIMEOUT 全局两档 | **移植**：数据源级 queryPolicy（maxRows/timeoutMs）进数据源注册表，与全局取 min |
| A9 | 分支会话/checkpoint/幂等重放 | 会话各自后台任务（AX82） | **移植（轻量形）**：会话可从任意历史消息分支（新 conv 复制前缀）；崩溃恢复=未答消息重跑提示 |
| A10 | 运行历史回放/Trace DAG | query_log+steps 有基础 | **延后**（有 steps+trust envelope，先不建 DAG 画布） |
| A11 | TUI 客户端 | 无 | **不做**（无场景） |
| A12 | 排队追问 | 前端已有轮询进度 | **移植**：前端队列（当前 run 结束后自动发下一条） |

### B. 知识库质量（Yuxi → 我方）
| # | 功能 | 现状 | 处置 |
|---|---|---|---|
| B1 | 解析引擎矩阵（MinerU/PaddleX/RapidOCR/Docling/云端 OCR） | 自研解析+embed 服务 OCR | **移植**：parser 引擎注册表（本地 RapidOCR/远程 MinerU 服务/云端 VL 模型三档），按文件类型与可用性降级；PDF 版面优先 MinerU 兼容端点 |
| B2 | 分块 preset（qa/book/laws/semantic/separator） | 单一分块 | **移植**：按文档类型/用户选择 preset；QA 对从表格抽取；laws 层级合并；标题路径注入已有 |
| B3 | chunk 字符偏移回链 | 有 merged span | **移植**：chunk 记 start/end_char_pos，引用回查按偏移定位 |
| B4 | 混合检索（稠密+BM25 稀疏，WeightedRanker） | 向量/fts/trgm/标题/元数据五路 RRF | **不搬 BM25**（PG tsvector+trgm 已覆盖词法路），**移植权重可配**（各路权重入设置） |
| B5 | Reranker（OpenAI/DashScope，失败回退向量分） | 无 | **移植**：recall_top_k→rerank→final_top_k，失败回退原分；模型走既有 fallback provider 机制 |
| B6 | 文档级知识图谱（LLM 抽取实体/三元组→图存储→PPR+RRF 增强检索） | 无（AGE 只有业务关系图） | **移植（用 AGE，不引 Neo4j）**：抽取流水（并发+重试+失败样本可查）→ AGE 存 Entity/RELATION/MENTIONS → 查询期种子召回+1~2 hop 扩散+PPR 排 chunk+RRF 融合。这是知识库准确性的最大单项增量 |
| B7 | 入库两段状态机+双向回滚+content_hash 去重 | 有状态字段 | **补齐**：content_hash 秒传去重；失败态可见可重试 |
| B8 | RAG 评估闭环（QA 生成+P/R/F1+LLM judge） | kb_eval 判官雏形 | **补齐**：基准生成器 + 指标报表 |
| B9 | 外部 KB 连接器（Dify/Notion 只读） | 无 | **延后**（无现状需求） |
| B10 | 思维导图/样例问题/自动描述 | 无 | **延后**（锦上添花） |
| B11 | 文档级 ACL | **我方更强**（内联 SQL 级 ACL，Yuxi 只有 KB 级） | 保持我方，不回退 |

### C. 平台化（两者 → 我方）
| # | 功能 | 现状 | 处置 |
|---|---|---|---|
| C1 | 队列可靠性（先落事实再投递+worker 恢复扫描） | 内存进度表 | **移植**：后台任务落 PG（chat.msg 扩展状态列），服务重启可恢复/标记中断 |
| C2 | 沙盒执行（隔离容器+路径白名单+审批 interrupt） | 无 | **延后**（Agent 工具化阶段再做；当前无代码执行场景） |
| C3 | Skills 体系（SKILL.md+依赖门控） | 无 | **移植（轻量形）**：提示词包注册表（周报模板等已是雏形），不做远程市场 |
| C4 | 审批流（write/edit/execute interrupt） | 无写操作 | **不做**（只读系统） |
| C5 | 用户体系（OIDC/API Key/部门/角色上限 share_config） | DMS 身份绑定 | **部分移植**：API Key（对外调用面）；OIDC/部门按 DMS 主数据走，不另建 |
| C6 | Dashboard 统计（五类+时序） | 无 | **移植（轻量）**：基于 query_log 的使用统计页 |
| C7 | MCP 客户端（挂外部工具） | 有 mcp_api（服务端） | **评估**：客户端模式（调外部 MCP）按需求点 |
| C8 | CLI 客户端 | 有 cargo 子命令 | 已有，不扩 |

### D. 明确不做（记录理由）
- DataFoundry 的 TUI、DataLink 独立服务（我方 warehouse_catalog+AGE 单据图已覆盖数据地图核心）、全文换 BM25（PG 路已够）、Multi-tenant workspaces（DMS 组织即租户）。
- Yuxi 的 Neo4j（用 AGE）、MinIO（PG bytea/本地卷已够）、ARQ/Redis（Axum 内任务+PG 事实表）、Vue 前端重写（现有前端继续）。

## 2. 分期与验收门禁

- **P0（问数可信）**：A1、A2、A8、A5 → 验收：深度报告错数必拦 + query_log 全状态 + 回归 76 题不破。
- **P1（知识库质量）**：B2、B3、B5、B7、B8 → 验收：kb_eval 基线提升可测；分块/偏移单测。
- **P2（图谱增强检索）**：B6 → 验收：图增强开关 A/B 对比命中率提升；AGE 无新依赖。
- **P3（平台化）**：A3、A9、A12、C1、C6、A7 → 按价值排。
- **P4（按需）**：A6（新数据源）、B9、B10、C2、C7。

每期纪律不变：单一事实源 / 权限闸门 / 判官回归全绿 / 漂移测试随行。

## 3. 第一期（P0）落地清单（开工即此）
1. `query_log` 加 `status`：gate 拒绝/执行超时/失败全落库（A2）。
2. 深度报告 claim 容差校验（A1 简化形）：`validate_evidence_insight` 后加「数值必须出现在对应 evidence 查询结果内」硬判。
3. 数据源级 queryPolicy（A8）：注册表加 max_rows/timeout_ms，fetch 取 min。
4. 语义降级链（A5）：catalog 探针失败 → 上次快照 + degraded 标记。
