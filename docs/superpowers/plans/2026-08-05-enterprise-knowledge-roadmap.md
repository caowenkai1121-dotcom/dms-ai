# 企业知识库增强与重构路线

> 调研快照：2026-08-05  
> 范围：独立 UI、DMS 首页嵌入、企业微信共用同一知识内核。  
> 原则：ACL 在召回前执行；文档内容永不成为可执行指令；先修生命周期和质量闭环，再扩连接器与图谱。

## 1. 同类产品结论

| 产品/框架 | 值得吸收的能力 | 不直接照搬的部分 |
|---|---|---|
| Dify Knowledge | 知识流水线、父子分块、元数据过滤、混合检索、文档更新 API | 工作区权限不能替代 DMS 的行级账号权限 |
| RAGFlow | 版面/表格/OCR 解析、解析配置、团队权限、数据源同步 | 不能另起一套知识管理服务和身份体系 |
| FastGPT | 向量+全文+RRF、团队资源权限、评测、可观测性 | 不引入第二个应用编排运行时 |
| MaxKB | 国产化管理 UI、标签元数据、混合检索、重排节点 | 评测与可观测能力仍需本系统自建门禁 |
| Open WebUI | 多解析引擎、OCR、BM25+向量、Cross Encoder、知识 ACL | Group ACL 不能替代 DMS 用户/角色/组织关系 |
| GraphRAG | Local/Global Search，适合制度、客户、项目、产品关系 | 只做辅助索引，不替代普通 RAG 和确定性数据库问数 |
| LlamaIndex | 摄取流水线、融合检索、Citation Engine、评测与 instrumentation | 作为组件参考，不引入一套平行管理系统 |

官方依据：

- Dify：[索引与混合检索](https://docs.dify.ai/en/cloud/use-dify/knowledge/create-knowledge/setting-indexing-methods)、[分块](https://docs.dify.ai/en/cloud/use-dify/knowledge/create-knowledge/chunking-and-cleaning-text)、[元数据](https://docs.dify.ai/en/cloud/use-dify/knowledge/metadata)、[更新文档 API](https://docs.dify.ai/en/api-reference/documents/update-document-by-file)
- RAGFlow：[官方仓库](https://github.com/infiniflow/ragflow)、[PDF 解析器](https://ragflow.io/docs/select_pdf_parser)、[权限系统](https://ragflow.io/docs/permission_system_overview)、[数据源同步](https://ragflow.io/docs/add_data_source/add_to_knowledge_base_and_sync)
- FastGPT：[知识库引擎](https://doc.fastgpt.io/zh-CN/guide/dataset/dataset_engine)、[团队权限](https://doc.fastgpt.io/zh-CN/guide/workspace/team/team_roles_permissions)、[评测](https://doc.fastgpt.io/zh-CN/guide/build/evaluation)、[SigNoz](https://doc.fastgpt.io/zh-CN/self-host/config/signoz)
- MaxKB：[官方仓库](https://github.com/1Panel-dev/MaxKB)、[混合检索实现](https://github.com/1Panel-dev/MaxKB/blob/main/apps/embedding/sql/blend_search.sql)、[重排节点](https://github.com/1Panel-dev/MaxKB/tree/main/apps/application/flow/step_node/reranker_node)
- Open WebUI：[Knowledge](https://docs.openwebui.com/features/workspace/knowledge/)、[文档解析](https://docs.openwebui.com/features/chat-conversations/rag/document-extraction/)、[RBAC](https://docs.openwebui.com/features/authentication-access/rbac/)
- GraphRAG：[架构概览](https://microsoft.github.io/graphrag/index/overview/)、[Local Search](https://microsoft.github.io/graphrag/query/local_search/)、[Global Search](https://microsoft.github.io/graphrag/query/global_search/)
- LlamaIndex：[摄取流水线](https://github.com/run-llama/llama_index/blob/main/llama-index-core/llama_index/core/ingestion/pipeline.py)、[引用引擎](https://github.com/run-llama/llama_index/blob/main/llama-index-core/llama_index/core/query_engine/citation_query_engine.py)、[评测](https://github.com/run-llama/llama_index/tree/main/llama-index-core/llama_index/core/evaluation)

## 2. 当前能力与本轮落地

现有系统已有：19 类文件、Office/PDF/图片 OCR、表格双通道、页码/标题分块、向量/FTS/trigram 混合召回、RRF、SQL 内 ACL、引用回查、提示注入隔离和 16 题知识库评测。

本轮增强：

1. **企业知识空间**：个人空间、企业空间、用户/角色的只读或可编辑共享；空间 owner 自动可写。
2. **文档生命周期**：文档可停用/恢复，停用后原文保留但检索 SQL 强制排除；失败文档可重处理。
3. **非破坏重处理**：解析、切片和向量先在影子版本完整构建，再用单条数据库语句切换；失败时旧索引继续可用。
4. **检索准确性**：全角/大小写/标点归一化，保留型号字符；新增文件名/章节标题召回；RRF 单路去重；相邻块合并、正文去重和来源多样化。
5. **管理体验**：空间选择与创建、共享权限、拖放多文件、状态筛选、失败/降级提示、重处理、启停和删除。
6. **回答体验**：Markdown 表格和代码块、结构化来源、连续切片范围、点击角标核对原文、加载失败重试。
7. **数值与版本可信化**：回答中的阿拉伯数字只有在引用原文中逐项出现才允许保留；检索到多个版本时
   并列展示全部版本引用和冲突提示，不由模型猜测当前生效版本。前端同步展示“数值引用已核对”和
   证据编号标签，使问数报告与知识答案使用一致的可追溯视觉语言。

## 3. 后续 P0

1. **版本模型**：增加 `document_family/document_revision/ingest_job`，记录解析器、分块器和 Embedding 版本；支持生效期、替代关系和回滚。
2. **表格通道状态**：拆分 `rag_status/table_status/table_error`；登记数据源或 schema 同步失败时前端明确展示“部分可用”，并提供幂等修复。
3. **元数据治理**：密级、部门、业务系统、文档类型、生效/失效日期、标签；自动抽取后人工校正，作为召回前过滤条件。
4. **解析质量工作台**：页级预览、OCR 路径、空白/失败页、表格数量、分块边界与异常提示，审核通过后发布。
5. **中文稀疏检索与重排**：引入可用的中文 BM25/分词；ACL 召回 30-50 个候选后，对前 24 个做轻量 Cross Encoder 重排。
6. **评测发布闸**：固定 Recall@K、MRR、nDCG、引用准确率、答案忠实度、OCR 完整率、ACL 泄漏率和 P95 延迟；模型或解析器切换必须回归。
7. **知识 Trace**：复用 `trace_id/query_log`，记录各召回路线数量、ACL 可见文档数、RRF/重排分数、阶段耗时、模型调用与降级原因。

## 4. 后续 P1

1. 统一连接器接入 DMS 附件、企微文档、OneDrive/SharePoint、共享目录和网页，使用哈希差异、删除墓碑、Webhook/定时同步。
2. 深度模式采用 Agentic Retrieval：搜索、打开来源、跨文档比较、冲突核验；精简模式保留单轮快速检索。
3. 对制度关系、客户项目、产品技术关系增加 GraphRAG 辅助索引；普通问答仍走混合检索。
4. 为数字、日期和制度结论增加 claim-evidence 校验，保存证据摘录、字符偏移与文档 revision/hash。
5. 上传改为流式落盘和增量 SHA，增加 magic/MIME、解压后字节、总页数/字符/单元格限制与恶意文件扫描。

## 5. 验收指标

| 方向 | 门禁 |
|---|---|
| 准确 | 核心 16 题全绿；标题/文件名/型号题进入黄金集；引用原文能支撑答案 |
| 权限 | 跨用户、跨角色、只读共享、停用文档和引用回查均不得泄露 |
| 智能 | 冲突资料必须并列说明；只覆盖部分问题时明确缺失项；不凭空补事实 |
| 美观 | 管理状态、引用和解析降级可扫描；移动端不溢出；错误均有下一步操作 |
| 速度 | 可见集合为空零召回；检索 P95、解析耗时、重排耗时和模型耗时分别可观测 |
