# 自适应·自进化重构总纲（SuperSonic × deepagents 全功能移植）

> 起因（用户原话）：现在的改动太像针对性改动——查 A 就针对 A 改，这是不对的。要的是**自适应、自进化**。
> 红线不变：DMS 生产库只读；xh-dms 三份源码只读。

## 一、现状批判：点状硬编码清单（要退役/改造的对象）

| 位置 | 点状问题 |
|---|---|
| `direct.rs` detect_sales_dim / DIM_WORDS / agg_template / doc_binding | 关键词触发写死：新问法、新维度、新单据类型都要改代码 |
| `direct.rs` sales_breakdown 6 个手工 SQL 模板 | 「销售额按X」= 每维度一段手写 SQL；第 7 个维度就要改代码 |
| `meta.rs` seed_metrics/seed_dimensions/seed_value_maps | 指标/维度/码表全部手工播种；新码列、新字典不会自动进来 |
| `viewspec.rs` DIM_POOL 下钻词表 | 下钻维度写死 6 个 |
| pipeline 各卡召回 = 关键词 substring 命中 | 别名靠人脑穷举，口语化问法漏召 |

## 二、目标架构：元素驱动 + 通用组合 + 自进化闭环

```
                 ┌──────────── 自进化闭环 ────────────┐
                 │                                    │
  自动发现引擎 ──→ 语义元素注册表 ──→ 元素级向量召回 ──→ 通用组合器 ──→ 执行
  (dict/基数/注释)  (metric/dim/value/join)  (embed,非关键词)  (metric×dim×filter)  │
                 ↑                                    │ 失败/0行/纠错记录
                 └────────── 失败复盘 + 纠错反哺 ←─────┘
```

三条铁律：
1. **一切知识都是数据**（PG meta 里的注册行），不是 Rust 代码里的 if/字符串字面量。加知识 = 插行/自动发现，不改代码。
2. **一切组合都是通用的**：问句 = 元素集合（指标×维度×过滤×时间），组合器按注册表元数据装配 SQL，不按问题类型配模板。
3. **用得越多越聪明**：成功问答→复核入范例；失败/0行/超时→复盘出新教训；校正器每次出手→反哺 prompt/规则。

## 三、SuperSonic 全功能映射（✅已移植 / 🔧改造中 / ⬜待移植）

| SuperSonic 系统 | 现状 | 去向 |
|---|---|---|
| Model/DataSet（模型/数据集） | meta.table_doc + domain 分组 ✅ | 保持 |
| Metric 语义层 | meta.metric 手工种子 🔧 | **自动发现**（数值列+别名学习）+ 手工精修并存（S2） |
| Dimension 语义层 | meta.dimension 手工种子 🔧 | **自动发现**（字典码列/低基数列）+ 手工精修（S1） |
| ValueLinking 值映射 | meta.value_map 手工种子 🔧 | **字典自动对码**（S1） |
| SchemaMapper（问句→元素） | 关键词 substring 命中 ⬜ | **元素级向量召回**：每个元素（名+别名+描述）算 embedding，问句近邻命中（S2） |
| S2SQL + Translator | 物理 SQL + AST 校正链（等效路线）✅ | 不照搬 S2SQL 文法；校正链已是同职责件 |
| Corrector 全家（字段/GroupBy/聚合/值/时间） | 4/5 ✅ | 默认时间范围（评估后定，M6g 已对齐"没提时间别加"） |
| JoinPath 自动推导 | 无（模板内手写 JOIN）⬜ | **join 边注册表 + 通用组合器**（S3）：退役 sales_breakdown 手工模板 |
| KnowledgeBase 术语 | meta.term ✅ | 保持 |
| Few-shot 记忆 + MemoryReviewTask 复核 | ✅ | 保持 |
| 语义缓存（向量近义问答） | ✅ | 保持 |
| 双召回（词典+向量） | ✅ | 保持 |
| textSummary 洞察 | ✅ | 保持 |
| 下钻 recommendedDimensions | DIM_POOL 写死 🔧 | 下钻维度=维度注册表驱动（S3） |
| 自一致性投票（N 次表决） | 未移植 | 暂缓（成本） |
| Plugin 查询插件 | direct.rs 硬编码 🔧 | 快路径注册化：模板=数据行非代码（S6） |
| authorizedCols 列级权限 | 敏感列 schema 剔除 ✅ | 保持（行级权限已是 1:1 复刻） |

## 四、deepagents 全功能映射

| deepagents 支柱 | 现状 | 去向 |
|---|---|---|
| write_todos 规划 | compound 拆解 ✅ | 保持 |
| subagents 隔离并行 | 子问题并行独立上下文 ✅ | 保持 |
| 虚拟文件系统（上下文卸载） | 200 行截断 ⚠️ | 大结果落 PG 暂存+分页/聚合读取（S5，按需） |
| detailed system prompt | 7 条硬规则 ✅ | 保持 |
| —（自改进延伸） | 无 ⬜ | **失败复盘环**：0行/报错/超时自动 LLM 复盘→pitfall 候选（S4）；**纠错反哺环**：校正器出手记录→prompt 强化（S4） |

## 五、自进化三引擎（核心交付物）

### 引擎 A：自动发现（数据驱动注册）
- **A1 字典码列自动对码**（本轮 S1）：列名模式（*_code/_type/_status/_class/_mode/_way/_level）+ 小表（row_estimate<100万）→ 只读探针 DISTINCT 抽样 → 值集 ⊆ 某 dict key 码集（≥80% 且 ≥2 值）→ 自动注册 value_map（eq 换码）+ dimension（CASE 翻名）。字典变了重跑即自适应。
- **A2 低基数维度发现**：非码低基数列（distinct<50 且非 NULL 占比>30%）→ 维度候选（直接取值，无需翻名）。
- **A3 指标候选发现**：数值列（decimal/double/int）按注释关键词（金额/数量/价/费）→ 指标候选（status='candidate'，人工/复核启用）。
- **A4 join 边发现**：列名同构（a.x_code = b.x_code 且两侧均有此列）→ join 边候选，样例验证（EXPLAIN/小样本对拍）后启用。

### 引擎 B：使用中学习（已有，补强）
- 语义缓存/exemplar/复核闭环已有。**B+：纠错反哺**——schema-fix/agg/value 每次出手写 `meta.correction_log`，同错累计 ≥3 次自动升格为 pitfall 教训（prompt 常驻）。

### 引擎 C：失败复盘
- 执行报错/0 行/超时 → 异步 LLM 复盘（对照问题+SQL+schema）→ 产出教训候选 `meta.pitfall status='candidate'` → review-pending 复核启用。

## 六、阶段计划

- **S1（本轮起）**：A1 字典码列自动对码（CLI `meta autodiscover`）。✅ 已实现+实跑；教训=值集对码必须过三闸（≥8 值直通/特征码直通/名称对齐），否则数值小码集互相撞车（menu_type 撞对账单状态、wms_type 撞 28 项发票类型）。
- S2：元素级向量召回（元素注册表 embed 化，替代关键词命中；指标候选发现 A3）。✅ meta.element + sync_elements + recall_elements + pipeline 注入已实现（与 S1 同批）。
- S3：join 边注册表 + 通用组合器（metric×dim×时间窗 自动装配）；sales_breakdown 手工模板退役为组合器特例；下钻池=维度表驱动。
- S4：失败复盘环 + 纠错反哺环（B+/C）。
- S5：大结果上下文卸载（deepagents 虚拟 FS）。
- S6：快路径注册化（direct 模板=注册行）。

每阶段交付=代码+自动发现的数据资产+回归题集扩充，验收照旧走 20 轮门禁。
