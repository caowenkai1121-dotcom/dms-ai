# dms-ai 进化方案：自学习 / 知识库 / 智能路由 / Doris（2026-08-13 第二轮）

> 产出：5 路研究（prime-agent 自学习机制 / Yuxi 知识库逐环 / Doris 能力面 / 本仓学习面盘点 / 混合路由断点）
> + 3 路设计（自学习架构 / 知识库加强 / 路由收口）+ 2 路对抗验伪。
> 与 `OPTIMIZATION-PLAN-2026-08-13.md` 是**增量**关系：那份是架构与准确性 7 批，这份是智能化与学习能力。


## 研究：prime-agent（PrimeIntellect-ai）机制拆解 → dms_ai「可验证自我学习」落点

prime-agent（MIT，TypeScript，构建在 earendil-works/pi 之上——本仓 AGENT-ARCHITECTURE.md 开头引的就是它）只有两个真实抽象。①**RLM**：模型只有一个内置工具 `ipython`，读写文件、跑命令、调技能、派生子 agent 全部通过持久 IPython 内核里的 Python 代码完成；TypeScript 宿主保留供应商调用、落账、子进程生命周期与安全策略（docs/rlm.md「Core Invariants」1-4）。子 agent 是 `await rlm("task")`，**立刻返回准入句柄而不是答案**，结果只能靠 `agent_message` 或文件回来。②**Continual Harness**（论文 arXiv 2605.09998，Karten 等，评测面是 Pokemon Red/Emerald 的按键成本，不是 BI）：一份 `harness_state.json`，四种条目 `prompt|memory|skill|subagent`，每条带 `id/title/content/path/scope/version/created_at/updated_at/before-after`，分 **local（会话级）/ global（跨会话）** 两个存储，`mergeHarnessStates` 合并、local 轮里 global 条目只读。

自我进化的真身是 `/refine`，全部实现在 `packages/coding-agent/src/core/refinement/refinement.ts`（1017 行），形状很朴素：**两段式 LLM + 确定性 apply**。第一段 `reviewAutoRefine`（:949）拿最近 40k 字轨迹问一个便宜模型「这轮值不值得学」，只回 `{shouldRefine, rationale, instructions}`，提示词明写「拒绝一次性噪声与无支撑假设」；第二段 `planRefinement`（:863）把 `<current_harness_state>`＋`<refinement_history>`（近 20 条）＋`<conversation>`（近 80k 字）＋`<scope_policy>` 喂进去，只准回 JSON `{summary, rationale, expectedOutcome, edits[]}`。**真正改状态的是 TypeScript 不是模型**：`applyRefinementProposal`（:707）逐条 `validateEdit`（基础系统提示词 id 硬拒改，:670）、与 `baselineState` 做乐观并发比对（不一致就判 "entry changed during refinement planning" 丢弃该条）、`version+1`、逐条记 `before/after`。回滚是纯机械的 `rollbackProposal`（:804）——倒序重放 `appliedEdits`，有 `before` 就还原、没有就删；global 批次追加进 `harness/refinements.jsonl` 供任意会话回滚。触发点：显式 `/refine`、内核里 `await refine.run()`、以及**自动档**（默认每 25 个 assistant turn 或压缩时，20 分钟冷却，settings-manager.ts:883）。落盘用临时文件 + rename、0600、坏文件降级成空而不抛。

必须说清楚的两件事：**本仓不存在 RL**——1293 个文件里 `reward|verifier|rollout|train` 零命中，README 把 prime-rl / verifiers 指向另外两个仓；**harness 的召回是把概览直接塞进系统提示词**（每类最多 6 条、每条截 180 字，system-prompt.ts:105），不是向量检索。所谓「验证变好了」在仓内只有 autonomous 模式的 shell 质量闸（`--autonomous-gate "npm run check"`，失败回灌 6000 字给模型重试，autonomous.ts:57-63），文档自己声明「闸过了只证明这个闸查的那点，跑到上限不等于成功」。

对照本仓：`meta.memory / meta.sql_exemplar / meta.pitfall / meta.correction_log / meta.failure_log / meta.skill / meta.query_feedback` 七张表已经把「学什么、存哪」铺完了，review.rs 的三段判词甚至比 prime-agent 更严（人工复核 + 真实只读执行验证，admin_api.rs:362 `validate_exemplar`）。真正缺的是四条：**没有用户维度**（`meta.memory` 只有 ds_id，而 `AskCtx.p.login_name` 就在手边，ctx.rs:29）；**没有批次账本与回滚**（save/set_status 全是裸写，没人能回答「上周二学了什么、怎么撤」）；**蒸馏闸门比语料闸门松**（run.rs:873 只看 route 和行数，绕过了同函数 :863 已有的 `worth_learning`）；**failure_log 只写不读**（零 SELECT，重复失败没有权重）。

### [critical/S] 经验蒸馏漏掉 worth_learning 闸：被判「不可信」的 SQL 照样进所有人的 prompt

- **参考系统怎么做**：prime-agent 的自动 refine 是两段闸：先 reviewAutoRefine 判「这轮值不值得学」（refinement.ts:949，提示词明写「拒绝一次性噪声、无支撑假设与瞬时工具输出」），通过后才 planRefinement 提案，且 apply 侧再逐条 validateEdit（:664）。学与不学从来不由「有没有输出」决定。
- **本仓现状**：crates/agent/src/run.rs:863 语料沉淀走 `if worth_learning(st, &rs)`（run.rs:1040：`st.note.is_some()` 即否决，即口径复核未过/绕开合同的 SQL 不进 few-shot，run.rs:1972 还有一条守卫钉住 note 必须先于它落）。但十行之下的 run.rs:873 经验蒸馏判据是 `if st.route == "llm+repair" && !rs.rows.is_empty()` —— **既不看 st.note 也不看覆盖闸收据**。于是一条挂了 caliber_note（数字明示不可信）、或覆盖闸判 review 的修正版 SQL，被 run.rs:884 拼成 content 落 meta.memory，随后由 gather.rs:167/478 向量召回进**每一个用户**的 prompt「经验复盘」段。同一个文件里两条学习路径两种诚实度。
- **改法**：crates/agent/src/run.rs:873 的条件改成 `if st.route == "llm+repair" && worth_learning(st, &rs)`（`worth_learning` 已含空结果判据，`!rs.rows.is_empty()` 是它的子集，直接删）。这是根因修法而不是调用点补丁：两条沉淀路径从此共用同一个判据函数。判据钉板照 run.rs:1987 `note_before_learn` 的模子再加一条：源码扫描断言 run.rs 的 `st.route == "llm+repair"` 那行 8 行内必须出现 `worth_learning`。若覆盖闸收据也要纳入，把 `st.note` 与 receipt 的 blocked/review 一起收进 `worth_learning` 内部，调用点一个都不用改。
- 证据：crates/agent/src/run.rs:863 `if worth_learning(st, &rs) {`（语料路）; crates/agent/src/run.rs:873 `if st.route == "llm+repair" && !rs.rows.is_empty() {`（经验路，无闸）; crates/agent/src/run.rs:1040-1047 `fn worth_learning`：`st.note.is_some()` 即 false; crates/agent/src/gather.rs:167 与 :478 `recall_memories(cx.pg, cx.ds, qvec, MEMORY_LIMIT)` → gather.rs:34 `MEMORY_LIMIT = 3` 进 prompt; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/refinement/refinement.ts#L949 reviewAutoRefine 两段闸
- confidence=high known=False

### [critical/M] 用户/角色两级经验作用域：把 prime-agent 的 local/global 搬成 (login_name, ds_id) 两层

- **参考系统怎么做**：prime-agent 的 harness 有两个物理存储：global（`~/.prime-agent/harness/harness_state.json`）与 local（会话产物目录），refinement.ts:269-283 两个 getter，:326 `mergeHarnessStates` 读时合并、写时只落请求的那一层。planRefinement 的 `<scope_policy>` 段（:893）硬规定：默认 local，只有「稳定的跨会话教训、持久的用户偏好、可复用技能」才准写 global；local 轮里 global 条目是只读上下文，要覆盖就在 local 新建一条。这就是「不同用户用出来效果不同」的全部机制——没有模型微调，只是作用域。
- **本仓现状**：crates/semantic/src/ddl.rs:141 `meta.memory` 只有 `ds_id/conv_id`，没有任何用户列；crates/semantic/src/registry/memory.rs:72 召回谓词是 `WHERE ds_id = $2 AND embedding IS NOT NULL`；gather.rs:167/478 也只传 `cx.ds`。而 `AskCtx.p: &Principal`（ctx.rs:29）里 `login_name/role_code/department_id` 一直在手边（policy/src/principal.rs:10-18）。结果：全公司共用一份经验池，业主要的「了解不同用户的习惯和经验」在数据模型上就不成立。
- **改法**：① `crates/semantic/src/ddl.rs` 给 meta.memory 加 `ALTER TABLE meta.memory ADD COLUMN IF NOT EXISTS login_name text NOT NULL DEFAULT ''`（空串 = ds 级公有，沿用本仓「空 = 全局」的既有约定，老行零回填）；索引 `idx_memory_scope ON meta.memory(ds_id, login_name)`。② `registry/memory.rs:60` 签名改 `recall_memories(pg, ds, login, qvec, limit)`，谓词 `WHERE ds_id=$2 AND (login_name = $3 OR login_name = '') AND embedding IS NOT NULL`（ds 谓词仍在，drift.rs `every_meta_recall_is_ds_scoped` 不会红）。③ `memory.rs:96 score()` 加一个私有档位：`login_name` 匹配时 ×1.3（个人经验优先，但 ds 级那批是过人工复核的，不能被压死）——纯函数，判据打这里。④ 写侧 run.rs:885 传 `cx.p.login_name`，即**自动蒸馏一律只进个人层**；升格到 ds 公有层只走 admin 复核（照 admin_api.rs:14「meta.sql_exemplar 只许来自真实问答 + 人工复核」同一条纪律）。这条不但不破 I4，反而把今天「一个用户的修正经验直接影响全员」收紧成用户隔离。
- 证据：crates/semantic/src/ddl.rs:141-151 meta.memory DDL（无用户列）; crates/semantic/src/registry/memory.rs:72 `WHERE ds_id = $2 AND embedding IS NOT NULL`; crates/agent/src/ctx.rs:29 `pub p: &'a Principal`；crates/policy/src/principal.rs:10-18; docs/ARCHITECTURE.md:63 I4「缓存不跨用户/不跨源」; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/refinement/refinement.ts#L269-L344 local/global 两存储 + mergeHarnessStates
- confidence=high known=False

### [high/M] 学习事件账本 meta.learn_event：每一次「学下来」都带 before/after 与批次号，一条 SQL 能撤回

- **参考系统怎么做**：prime-agent 把「学」拆成可审计的两半：applyRefinementProposal（refinement.ts:707）对每条 edit 记 `{action, kind, id, before, after, applied, error}`，整批带 `{id, summary, rationale, expectedOutcome}` 落 session JSONL（customType="prime-agent.refinement"）与 global `refinements.jsonl`；回滚是纯机械重放——rollbackProposal（:804）倒序遍历 appliedEdits，有 before 就还原成 update/create、没有就 delete，**不再调模型**。还有一层乐观并发：apply 时与 planning 起点的 baselineState 比对，条目在规划期间被人改过就整条判 "entry changed during refinement planning" 丢弃。
- **本仓现状**：本仓四个学习写口全是裸写、无前值、无批次：memory.rs:38 `save_memory`（INSERT + NOT EXISTS）、exemplar.rs:209 `save_with_context`、exemplar.rs:353 `save_lesson_candidate`、exemplar.rs:387 `set_lesson_status`（裸 UPDATE，0 行只 warn）。于是「上周二系统学了什么」「哪条教训把 E05 带红了」「怎么撤掉这一批」三个问题今天都只能连 PG 手写 SQL 逐表对时间戳。admin 侧只有单条 status 切换（admin_api.rs:345），没有批次概念。
- **改法**：crates/semantic/src/ddl.rs 加一张表：`meta.learn_event(id bigserial, at timestamptz default now(), batch_id text, actor text, target_table text, target_id text, action text, before jsonb, after jsonb, evidence text, expected_outcome text, trace_id text)` + `idx_learn_batch(batch_id, id)`。新建 `crates/semantic/src/registry/learn.rs`（**不塞 exemplar.rs**：它非测试段已 433 行、逼近 D2 的 450）只放两个函数：`log_learn_event(pg, ..)` 与 `rollback_batch(pg, batch_id) -> usize`（按 id 倒序重放 before：before 为 NULL 则 DELETE，否则按 target_table 白名单 UPDATE——白名单是三张表的 &'static str 常量，满足 drift.rs 的 `sql_interpolation_is_allowlisted`）。四个写口各加一行调用。server 侧 admin_api.rs 加两条 admin_only 端点：`GET /api/admin/learn?days=` 与 `POST /api/admin/learn/{batch_id}/rollback`。顺带把 W5#8「correction_log 只写不读」的排障需求从这一个视图一并回答。
- 证据：crates/semantic/src/registry/memory.rs:38 save_memory（无前值）; crates/semantic/src/registry/exemplar.rs:353 save_lesson_candidate、:387 set_lesson_status（裸 UPDATE，affected==0 只 warn）; crates/semantic/src/registry/exemplar.rs:434 `#[cfg(test)]` ⇒ 非测试段 433 行，逼近 D2 450; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/refinement/refinement.ts#L707-L836 apply 记 before/after + rollbackProposal 倒序重放; docs/OPTIMIZATION-PLAN-2026-08-13.md W5#8「correction_log 的 17 个 kind 只写不读」（相邻，非重复：那条是排障读面，本条是学习批次与回滚）
- confidence=high known=False

### [high/S] failure_log 零 SELECT：只有重复出现的失败才配升格教训（同时省掉一次性噪声的 LLM 复盘）

- **参考系统怎么做**：prime-agent 每次提案都把 `<refinement_history>`（近 20 条，含每条 edit 的 applied/failed 与 expectedOutcome，refinement.ts:548 historyForPrompt）喂回给规划模型，让它看得见「这类事发生过几次、上次的修法有没有奏效」；自动档的判词更直接：「拒绝一次性噪声与瞬时工具输出」。频次是一等公民。
- **本仓现状**：crates/semantic/src/registry/exemplar.rs:422 `log_failure_traced` 是全仓唯一提到 meta.failure_log 的非 DDL 语句——**没有任何 SELECT**（correction_log 同样，:408）。而 review.rs:56 `review_failure` 是在失败当场由 run.rs:918 spawn 调的，看到的永远只有这一次，判词 FAILURE_SYSTEM 也无从知道这是第 1 次还是第 7 次。后果两头都坏：一次性抖动也烧一次 fast 复盘并可能落一条候选教训，而真正反复发生的口径坑没有任何权重优势。
- **改法**：新建 `crates/semantic/src/registry/failure.rs`（同样不进 exemplar.rs），一个函数：`pub async fn failure_streak(pg, ds, kind, err_class: &str, days: i32) -> i64`，SQL 为 `SELECT count(*) FROM meta.failure_log WHERE ds_id=$1 AND kind=$2 AND left(error, 60)=$3 AND created_at >= now()-$4::int*interval '1 day'`（failure_log 属日志表，drift.rs 已豁免 ds 谓词，但这里本来就带）。run.rs:918 的 spawn 前先查一次：`streak < 2` 就只落日志不调模型（本轮直接省掉大部分 fast 调用）；`>= 2` 才调 `review_failure`，并把次数拼进 user 段（`已连续第 N 次`），FAILURE_SYSTEM 判词加一句「重复出现的失败优先给出可复用教训」。review.rs:81 `review_lessons` 侧同理：candidate_lessons 的排序从 `ORDER BY id` 改成按对应 streak 降序（或最省的版本：只在 user 段带上次数，让复核模型自己权衡）。
- 证据：crates/semantic/src/registry/exemplar.rs:422 log_failure_traced —— 全仓 meta.failure_log 唯一非 DDL 引用，无 SELECT; crates/semantic/src/registry/exemplar.rs:408 log_correction_traced —— 同上; crates/agent/src/run.rs:911-923 失败当场 spawn review_failure（无频次判据）; crates/agent/src/review.rs:22-25 FAILURE_SYSTEM 判词; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/refinement/refinement.ts#L548 historyForPrompt 把近 20 条历史回灌
- confidence=high known=False

### [high/M] 用户习惯档案 meta.user_pref：零 LLM、纯计数，把 IntentV1 已解析的槽位偏好沉下来

- **参考系统怎么做**：prime-agent 的 `memory` 条目被 REFINEMENT_SYSTEM_PROMPT 定义为「durable facts, decisions, failures, preferences, and outcomes」，规则段明写「durable facts/preferences should become memories」——用户偏好是与失败教训并列的一等 kind，而不是从对话里现推。
- **本仓现状**：本仓的经验只有一种 kind（`review`，ddl.rs:141 注释与 run.rs:884 的 content 模板都写死了「首版错、修正后对」这一种形态）。业主要的「了解不同用户的习惯」——常看哪个粒度、习惯按什么维度拆、要明细还是要汇总、常问哪几个客户——这些槽位 IntentV1 早就解析出来了（`breakdowns` / `time.granularity` / `requested_detail` / `regions`，见 docs/AGENT-ARCHITECTURE §3.1），但一轮问完就丢，`meta.query_log` 虽有 login_name（query_log.rs:41）却没人反向读。
- **改法**：**不要为此调模型**。ddl.rs 加 `meta.user_pref(login_name text, ds_id text, key text, value text, hit_count int default 0, updated_at timestamptz default now(), PRIMARY KEY(login_name, ds_id, key, value))`；`registry/learn.rs` 加 `bump_pref(pg, login, ds, &[(key, value)])`（单条 `INSERT .. ON CONFLICT DO UPDATE SET hit_count = user_pref.hit_count + 1, updated_at = now()`）。写点接在 run.rs 成功出口、与既有 `spawn_bump_hits` 同一形态的 fire-and-forget spawn 里，key 固定四个：`granularity` / `breakdown` / `detail` / `region`，value 直接取 IntentV1 的表面槽位（不落任何数据库值、不落实体 ID——与 `AskResult.intent_summary` 已确立的透出边界逐字同口径）。读点在 gather.rs 与 memory 召回同一波 join!：取该用户 hit_count top-3 且 `hit_count >= 3` 的项，渲染成 prompt.rs 新段 `T_USER_HABITS = "\n## 本用户常用口径（参考，不是硬约束）\n"`（措辞与 T_MEMORIES 同族，I5 同一条防线）。全程零 LLM、零新依赖，且可解释、可导出、可清空（用户自己一键清除 = DELETE 一行谓词）。
- 证据：crates/semantic/src/ddl.rs:141 meta.memory 只有 kind='review' 一种形态；crates/agent/src/run.rs:884 content 模板写死; docs/AGENT-ARCHITECTURE.md §3.1 IntentV1 字段（breakdowns / time 粒度 / requested_detail / regions）; crates/server/src/query_log.rs:41 query_log 有 login_name 但无反向读取; crates/agent/src/prompt.rs:44 T_MEMORIES「参考，不是硬约束」的既有措辞与信任边界; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/refinement/refinement.ts#L146 「durable facts/preferences should become memories」
- confidence=high known=False

### [high/S] expected_outcome + 学习专用回归题集：让「证明它变好了」变成一条可跑的命令

- **参考系统怎么做**：prime-agent 的提案 JSON 有一个强制字段 `expectedOutcome: "what should improve and how to validate it"`（refinement.ts:166），它随批次一起落 refinements.jsonl，并在下次规划时通过 historyForPrompt 原样回灌（:562 `Expected outcome: ...`）。它不是装饰——是把「学了一条」和「怎么知道有用」绑在同一行里。
- **本仓现状**：本仓评测基建已经很硬：tools/regression.py 有金文件 + `--cases` 换题集 + `--bless`；tools/evaluation.py 的判据是 `--runs N` 的失败集交集，头部明写「LLM 路径抖动池 ≥9/38 ≈ 24%，单轮总分分辨不出 ±2」。但**没有任何一条学下来的经验/教训/语料指向一道题**。于是「这条 pitfall 让系统变好了吗」只能靠全量回归的总分去感觉，而总分恰恰是这份文档自己判定为噪声的东西。学错了要回滚，也没有判断依据。
- **改法**：最省的版本：把 `expected_outcome text` 与 `case_id text` 两列只加在上一条的 `meta.learn_event`（不动三张业务表）。教训/经验源自一次真实失败问句时，把那句问句 + 期望路由/期望非空结果写进新题集文件 `tools/regression_cases_learned.json`——`regression.py:63` 的 `--cases` 已经支持任意题集路径，`case_id` 就存题名。验收命令因此是现成的一行：`python tools/regression.py --cases tools/regression_cases_learned.json`。回滚判据也随之确定：某个 batch_id 上线后这份题集净转红，`POST /api/admin/learn/{batch_id}/rollback` 撤回并复跑。不新建评测框架、不改 regression.py 一行代码。
- 证据：tools/regression.py:63 `--cases` 支持替换题集（相对路径按 ROOT 解析）; tools/evaluation.py 头部：「LLM 路径实测抖动池 ≥9/38 ≈ 24%，单轮 38 题分辨不出 ±2 的差异」; crates/server/src/admin_api.rs:362 validate_exemplar 已有「真实只读执行验证」的先例，可作 case 化的第二档; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/refinement/refinement.ts#L166 expectedOutcome 强制字段；#L562 回灌
- confidence=high known=False

### [medium/S] memory.content 把整条 SQL 和他人问句原文灌进所有人的 prompt

- **参考系统怎么做**：prime-agent 的 memory 条目是声明式的（「durable facts, decisions, failures」），且进系统提示词时被 compactText 截到 180 字（refinement.ts:28/421）——概览是路由提示，细节要用时再去读原条目。轨迹原文从不直接变成长期状态。
- **本仓现状**：crates/agent/src/run.rs:884 的 content 是 `format!("问「{q}」：首版 SQL 未过口径复核或执行出错，修正后通过。正确写法：{fixed}")`，`{fixed}` 是完整的候选 SQL（截 400 字，memory.rs:36）。gather.rs:180 渲染成 `[{kind}] {content}` 直接进 prompt「经验复盘」段，而召回只按 ds_id ——**另一个用户的原始问句文本 + 完整 SQL 全文，进了这个用户的 prompt**。注释已经意识到一半（用 st.candidate 而非 wire() 以免带出行级权限条件），但问句原文这一半没守。content 里的问句还是冗余的：question 已是独立列，只用于去重（memory.rs:44）。
- **改法**：两行改动：① run.rs:884 的 content 去掉 `问「{q}」：` 前缀（问句留在 question 列，召回排序靠向量，prompt 里不需要它）；② 与第 2 条的 login_name 分层叠加后，个人层经验只回给本人，ds 公有层只收人工复核过的——原文外泄面收敛到「管理员明知并批准的那几条」。若要更进一步，content 只留修正后的关键谓词而非整条 SQL，但那需要一次 diff 计算，收益不如前两步直接，本轮不做。
- 证据：crates/agent/src/run.rs:884 content 模板含 `问「{q}」` 与完整 `{fixed}`; crates/semantic/src/registry/memory.rs:36 content 截 400 字、question 截 200 字（两列各存一份）; crates/agent/src/gather.rs:180 `format!("[{}] {}", h.kind, h.content)` 直接进 prompt; crates/agent/src/run.rs:868-871 注释已守住 wire() 的行级权限泄漏面，但未守问句原文; docs/ARCHITECTURE.md:63 I4
- confidence=high known=False

### [medium/S] 批量复核前置一道便宜的「值不值得学」闸（review_lessons 今天是逐条硬调）

- **参考系统怎么做**：prime-agent 把成本花在两个档位上：先 `reviewAutoRefine`（4096 token 上限，autoRefineReviewMaxOutputTokens :203）判断整段轨迹要不要学，通过了才发 32000 token 上限的 planRefinement（:198）。而且两个调用都强制关掉 reasoning（`void thinkingLevel`，:911/:975 附近注释写明：推理模型会把预算烧在思考上、最终文本为空导致 JSON 解析失败）。
- **本仓现状**：crates/agent/src/review.rs:81 `review_lessons` 对 candidate_lessons 逐条各发一次 fast 调用，只有「连续 3 次失败即熔断」的可用性保护（:92），没有任何「这批候选整体值不值得复核」的前置判断；`review_exemplar` 同理。候选池被一次性噪声灌大时，成本线性上涨而信噪比下降。另：本仓 review.rs 三处 fast 调用没有关闭 reasoning 的等价处理——若 Fast 档热切到推理型模型，`parse_verdict` 拿到空 content 会静默走 fast() 的 None 分支（:44 debug 一行），复核回路静默停转。
- **改法**：最省的一半先做：把上面第 4 条的 `failure_streak` 结果作为候选教训的排序与准入判据（`streak < 2` 的候选直接 status='disabled' 不发模型），这已经覆盖 prime-agent 那道 review 闸的主要收益，且是纯 SQL、零额外 LLM 调用——比再加一次模型调用更符合本仓成本纪律。另一半是运维保险：crates/agent/src/review.rs:39 的 `ChatRequest::text(ModelTier::Fast, ...)` 处确认 Fast 档模型的 reasoning 开关状态，若 kernel/llm.rs 有对应参数就在这三处显式关掉，并给 fast() 的 None 分支从 debug 升到 warn（复核回路停转不该只留 debug）。
- 证据：crates/agent/src/review.rs:81-104 review_lessons 逐条硬调 + 仅有连续失败熔断; crates/agent/src/review.rs:36-50 `fast()` 的 None 分支只 tracing::debug; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/refinement/refinement.ts#L198-L206 两档输出预算; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/refinement/refinement.ts#L909-L915 强制非推理的注释与理由
- confidence=medium known=False

### [low/S] 明确不采用：RLM / 持久 IPython / rlm() 子 agent / 守护进程与 autonomous 模式

- **参考系统怎么做**：prime-agent 的默认运行时只暴露一个模型工具 `ipython`：文件读写、跑项目命令、调技能、派生子 agent 全部是模型生成的 Python，在持久内核里执行（docs/rlm.md「Execution is programmatic」）。子 agent 是 `await rlm(...)`，返回准入句柄而非答案；会话由 daemon supervisor 常驻，客户端断开继续跑；autonomous 模式按 maxTurns/maxTokens/timeoutMs 加续跑并用 shell 质量闸判完成。README 与 architecture.md 都写明 worker/kernel 分进程**只为生命周期隔离，不是安全沙箱**，「以你的用户权限执行模型生成的 Python」。
- **本仓现状**：本仓的安全叙事完全相反且更强：到生产 MySQL 的 SQL 必是 ScopedSql（I1，ARCHITECTURE.md:60，ScopedSql 字段私有、产出点只有 inject()），knowledge 结构上产不出 SQL（I5，:64），门禁对 crates/agent/src 整树守着 `sqlx::query`（drift.rs EXTERNAL 段注释）。一个能执行任意模型生成代码的 REPL 会一次性作废 I1/I3 与全部 AST 闸门；且 Python 运行时 + 内核协议是 D6 禁止的新依赖/新服务。子 agent 与 daemon 同理无立项理由：AGENT-ARCHITECTURE 明写 P2 有限 Agent loop 未做，深度报告已有自己的 progress/断点续跑，且优化方案「明确不做」里已经以「把等很久后失败变成早点失败」为由否掉过 AskCtx 统一 deadline。
- **改法**：不落地任何代码，只落一段文档：在 docs/ARCHITECTURE.md §8「明确不采用」表加一行 prime-agent 条目，写清三条理由（①程序化执行面 vs I1/I5 的结构性冲突；②Python 运行时 = D6 新依赖；③其自身文档承认非安全沙箱，而本仓是多角色行权限的企业 BI）。这与优化方案里已有的「Milvus/Neo4j/HanLP 不抄」「Yuxi 沙盒容器/子 agent 中间件不抄」同一张表、同一条纪律，目的只有一个：防下一轮重复立项。
- 证据：docs/ARCHITECTURE.md:60 I1、:64 I5、:19 D6; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/README.md 「Prime Agent executes model-generated Python and project commands with your user permissions … they are **not** a security sandbox」; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/docs/rlm.md 「Trust Model」段; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/autonomous.ts#L46-L63 autonomous 默认 maxTurns 12 + shell 质量闸; docs/AGENT-ARCHITECTURE.md 「P2 有限 Agent loop 未做」
- confidence=high known=False

### [low/S] 明确不采用：把 harness 概览塞进系统提示词的召回方式，以及自动向 global 层写入

- **参考系统怎么做**：prime-agent 的 harness 召回不是检索：每次构建系统提示词都调 formatHarnessStateForPrompt，把每类前 6 条、每条内容截 180 字、外加最近 5 条 refinement 事件整段拼进系统提示词，溢出只写「+N more」（refinement.ts:26-28、429-521；system-prompt.ts:105/140）。规划时用的 overviewForPrompt 放宽到每类 40 条 × 240 字（:522）。同时自动档 refine 默认每 25 轮触发一次，虽然默认写 local，但 reviewAutoRefine 有权建议 global。
- **本仓现状**：这两条本仓都**已经比它强**，不该回抄：①召回侧本仓是 pgvector 近邻 + `sim × (1+0.1·ln(1+hit)) × exp(-age/30d)` 重排后取 3 条（memory.rs:60-100），条数不随知识总量膨胀，而 prime-agent 的方式一过几十条就要么爆提示词要么静默丢弃；②本仓 ds 公有层的写入必须过人工复核（admin_api.rs:14/17），而自动写 global 在企业 BI 里就是跨用户口径投毒——优化方案 W5#10 已经把「管理写端点继承 insecure_login_fallback = 语料投毒面」列为要修的洞，此时再开一条自动全局写入通道等于自拆。
- **改法**：同样只落文档：这条与上一条合并进 ARCHITECTURE §8 的同一行条目，写明「harness 的**状态模型**（typed 条目 + version + before/after + local/global 作用域 + 批次回滚）值得移植，其**召回方式**（提示词塞概览）与**自动全局写入**明确不采用，理由分别是 pgvector 召回已更优、以及 ds 公有层必须人工复核」。这句话本身就是本轮三条落地提案（第 2/3/6 条）的边界声明——防止后续实施时顺手把两半一起抄进来。
- 证据：crates/semantic/src/registry/memory.rs:60-100 向量召回 + hit/recency 重排取 3; crates/server/src/admin_api.rs:14「meta.sql_exemplar 只许来自真实问答 + 人工复核」、:17 复核通道是唯一入口; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/refinement/refinement.ts#L26-L28 DEFAULT_OVERVIEW_ENTRY_LIMIT=6 / REFINEMENT_LIMIT=5 / CONTENT_LIMIT=180; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/system-prompt.ts#L105 formatHarnessStateForPrompt 每次构建都拼进系统提示词; https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/src/core/settings-manager.ts#L883-L898 autoRefine 默认 enabled/turnInterval=25/cooldown=20min; docs/OPTIMIZATION-PLAN-2026-08-13.md W5#10（语料投毒面）
- confidence=high known=False


## 研究：yuxi-kb：语析 Yuxi 知识库全链路 vs crates/knowledge 逐环对比

Yuxi v0.7.1 知识库主线：七引擎解析注册表（Docling/MinerU/PaddleX/RapidOCR/云 OCR，factory.py:44-127 实例缓存+健康检查）统一成 Markdown → RAGFlow 式六 preset 分块（presets.py:10-35，带 start/end_char_pos 回链）→ uploaded→parsing→parsed→indexing→indexed 两段状态机（base.py:288-300 update_fields_if_status 乐观锁、milvus.py:538-578 双写回滚）→ Milvus 单库稠密+内建 BM25 稀疏 WeightedRanker(0.7,0.3)（milvus.py:1010-1033）→ 可选 reranker 失败回退向量分 → 图谱增强（实体/三元组种子 → Neo4j 2-hop → PPR 排 chunk → RRF k=60，milvus.py:1103-1242）→ LLM 出题 + P/R/F1@k + judge 的评估闭环。

逐环对完的结论：十环里我们已在七环更强，真差距集中在「主链路之外」。更强的是：文档级 ACL 内联进每条检索 SQL、且已白纸黑字裁决不抄它的「min(授予,角色上限)」（acl.rs:21-24,409-421）；引用与冲突披露是 keep_cited_only/keep_supported_only/disclose_versioned_sources 三层强制 + 「版本与差异」并列表，而 Yuxi 自承 prompt 级引用效果不好已停用；九路加权 RRF vs 它的两路；解析降级链更细且逐扩展名机读自报档位；effective_from/to + enabled 生效期闸它完全没有；存储只用 PG（AGE+pgvector+pg_trgm+jieba）而非 Milvus+Neo4j+Redis+MinIO 四套。分块 preset、字符偏移回链（被导图预览真消费）、启动自愈、配方升级退回 chunked 由 embed_fill 后台补，都已对齐或超出。

真差距四条，均不在 W4 覆盖面：①多轮。KB 入口结构上单轮（answer.rs:84 只吃 question），唯一的追问改写守卫是 SQL 形状的（ask.rs:1583-1593）——KB 轮 payload 没有 sql 键、prev_q 又无公司实体，于是「报销标准是多少 → 那出差住宿呢」把碎片原样送进检索。改的是一个守卫，价值最高。②评测的方向压力装反了：自动评测五个指标全部单调偏好多召回，而检索侧三个标定阈值（VEC_MAX_DIST/TERMS_MIN_HITS/TRGM_MIN）的注释逐条写着「调低会打死近域 nohit」；今天调松阈值五个数一起变好、没有任何判据会红，根因是零负样本。③解析档位不落库：tier-2/3 的 heading_path 恒空，连带打掉标题路、embedding 配方的章节行、引用章节名与导图层级，而这两级恰恰不产 notes——全程无声，引擎升级后也不知道该重跑谁。④落账没有检索证据：九路 stats 与 vec_down 只进 tracing 与 debug 端点，生产上「这题为什么没命中」「多少答案产在 embed 熔断期」事后不可查。

外加两条减法：kb.chunk.ts 生成列 + GIN 索引全仓零读者，可直接删；重跑分块必然重跑解析（扫描件＝重跑按页付费的 OCR），加一张 kb.doc_blocks 旁表即可，不动状态机。

W4 已覆盖、本轮不重复的：rerank 窗口与生产接线（W4#8）、terms 路 IDF（W4#9）、图谱路默认恒缺席（W4#10）、preset 前端不可达与删 Semantic/Book（W4#6）、状态推进 CAS（W4#13）、ext_kb 整路删除（W4#7）、部分覆盖声明与版本冲突口径（W4#1/#2）、citations 丢 source_uri（W4#11）。

### [critical/M] 知识库多轮结构上断链：追问改写的跳过守卫是 SQL 形状的，KB 轮必然被跳过

- **参考系统怎么做**：Yuxi 侧不存在这个形态：会话是 thread 级 FIFO 队列（agent_request_queue_service.py:44-66，同 user+agent+thread 串行、steer 插队），知识检索只是 DeepAgents create_agent 的一个 tool，完整历史进 agent state，指代消解由主模型在全历史上做；超长时由两级摘要中间件（middlewares/summary.py，100k tokens 触发、L1/L2 比例 0.4）压缩而不丢轮次。来源 https://github.com/xerrors/Yuxi
- **本仓现状**：KB 问答入口结构上单轮：crates/knowledge/src/answer.rs:84-94 的 answer() 只有 question: &str，没有历史形参。全仓唯一的追问改写在 crates/agent/src/ask.rs:1562 rewrite_followup，它的跳过守卫写死成数据路形状——crates/agent/src/ask.rs:1583-1593：hist_sql.is_none() && company_span(prev_q).is_none() && !explicit_reference → 原样返回。而 KB 轮的 payload 结构上没有 sql 键（crates/server/src/chat.rs:195 自己注明这一点），prev_q 是制度问句不含公司实体，explicit_reference 只认「它/这个/那个/该/此」五词。结果：「报销标准是多少」之后问「那出差住宿呢」「有例外吗」「要交什么材料」，crates/server/src/main.rs:2549 与 :2642 两个 Knowledge 分支拿到的 prepared.question.effective_question 就是未改写的碎片，直接进 retrieve。AX116 只把长上下文压缩做在数据路上，KB 路一条都没接。
- **改法**：根因是那一个守卫，不在调用点，改三处共约 15 行：①crates/server/src/chat.rs:203 last_turn 返回值从 (String, Option<String>) 扩成 (String, Option<String>, bool)，第三位 prev_is_kb = payload 的 kind 字段等于 "text"（沿用 :234 那个 pick 闭包，深度轮的 result 嵌套档一并读）；②crates/agent/src/ask.rs:75 的 pub type PrevTurn 从 4 元组扩成 5 元组（第五位 bool），crates/server/src/main.rs 的两个构造点（api_ask 与 api_ask_stream 各一处）多传一个值，CLI/xcx/deep/mcp 传 false（与今天传 None 同口径）；③守卫改成 hist_sql.is_none() && !prev_is_kb && company_span(...).is_none() && !explicit_reference，并在 system 提示词（ask.rs:1594-1600）规则 5 之后追加一句「上一轮是知识库问答时，只继承上一问出现的制度/主题名词，不得补造指标、时间或筛选口径」。风险面比数据路小一个量级：改写歪了只会让 answer::keep_cited_only 判无据而回「知识库里没有相关内容」，不会产出错数。验收：tools/regression_cases_multiturn.json 加一条 KB 三轮链（报销标准是多少 → 那出差住宿呢 → 需要交什么材料），断言第 2/3 轮 payload 的 resolved_question 含「出差住宿」「报销」；ask.rs 单测加 prev_is_kb=true 且 prev_sql=None 时必须真发起改写（今天恒返原句）。
- 证据：crates/knowledge/src/answer.rs:84-94（answer 签名只有 question，无历史）; crates/agent/src/ask.rs:1583-1593（守卫三条件：无 SQL + 无公司实体 + 无显式指代 → return 原句）; crates/agent/src/ask.rs:1534-1544（is_followup 的 MARK 含「那/呢」，所以这类问句确实判为追问）; crates/server/src/chat.rs:193-196（注明「上一轮走了知识库（payload 是 Answer，没有 sql 键）」）; crates/server/src/main.rs:2549-2556 与 :2642-2668（两个 Knowledge 分支直接用 effective_question）; docs/PROGRESS.md AX116「长上下文压缩不丢」只落在追问改写的数据路上
- confidence=high known=False

### [high/M] 自动评测零负样本：五个指标全部单调奖励「多召回」，与检索侧三个标定阈值的纪律正相反

- **参考系统怎么做**：Yuxi 的评估闭环是 knowledge_eval_router.py + eval/：LLM 从 chunk 生成 QA 基准（benchmark_generation.py:117，可开图增强选邻居），指标 P/R/F1@k + LLM judge。它同样没有负样本，所以这一条不是抄它——是抄它抄不到的那半：我方 tools/kb_eval.py 那 16 道固定题里已经有 2 道 nohit / 2 道 inject / 2 道 acl，机制现成，只是没接进能自动跑的那条链。来源 https://github.com/xerrors/Yuxi
- **本仓现状**：crates/server/src/kb_eval_api.rs:142-149 的 SAMPLE_SQL 只抽本空间的正样本块；:190 GEN_SYSTEM 第 2 条明令「答案必须确实在片段里」；:196 JUDGE_SYSTEM 只有「声称无法回答但金标准原文其实能答＝wrong」一个方向。run 级五个指标 recall1/3/5/10 + answer_acc（:84-86 的回写）因此全部单调偏好多召回。而检索侧压着「宁可说没有」这条纪律的三个标定值——crates/knowledge/src/retrieve.rs:222 VEC_MAX_DIST=0.55、:73 TERMS_MIN_HITS=2、:192 TRGM_MIN=0.2——注释里逐条写明「调低会先打死近域 nohit / 正向题」。今天把任一阈值调松，自动评测的五个数会一起变好，没有任何判据会红；唯一能红的是手跑 tools/kb_eval.py 那 2 道 nohit（16 题：recall 7 / cite 2 / acl 2 / inject 2 / nohit 2 / conflict 1）。
- **改法**：在 kb_eval_api 里加一档负样本，零新表、零新依赖：①DDL 数组（crates/server/src/kb_eval_api.rs:77-107）追加两条幂等 ALTER：meta.kb_eval_items 加 kind text NOT NULL DEFAULT 'recall'，meta.kb_eval_runs 加 nohit_acc float8；gold_chunk_id 负题写 -1。②负题语料复用现成 SAMPLE_SQL，只换 space_id 参数——取该 viewer 可读的另一个空间（acl 侧已有 space 列表查询），抽 sample_size/5 块出题，然后对本空间提问。③判据是纯函数不调 judge：Answer 的 citations 为空 ∨ markdown 含 answer::NO_HIT 那句即 pass（NO_HIT 已是 answer.rs:30 的 pub 常量族）。④只有一个可读空间时负题数为 0，必须在 run.error 写一句「无第二空间，负样本未测」并让页面显示——不许静默按 0 题算满分（与 tools/kb_eval.py 的反空转退出码闸同一条纪律）。验收：把 VEC_MAX_DIST 临时改成 2.0 重跑一次，nohit_acc 必须掉下来——这就是今天缺的那条会红的判据。
- 证据：crates/server/src/kb_eval_api.rs:142-149（SAMPLE_SQL 只抽正样本）; crates/server/src/kb_eval_api.rs:190-194（GEN_SYSTEM「答案必须确实在片段里」）; crates/server/src/kb_eval_api.rs:196-200（JUDGE_SYSTEM 只有单向判据）; crates/knowledge/src/retrieve.rs:200-222（VEC_MAX_DIST 的实测注释：任何能挡住近域 nohit 的下限都会打死一半正向题）; crates/knowledge/src/retrieve.rs:64-73（TERMS_MIN_HITS 同族注释）; tools/kb_eval_cases.json：16 题 = recall 7 / cite 2 / acl 2 / inject 2 / nohit 2 / conflict 1（自动跑那条链一道都没有）
- confidence=high known=False

### [high/M] 解析档位不落到文档上：tier-2/3 无声降级，打掉标题路+配方章节行+引用章节名+导图层级

- **参考系统怎么做**：Yuxi 的解析是显式引擎注册表：knowledge/parser/registry.py:5-13 登记七个引擎，factory.py:44-127 做实例缓存与健康检查，unified.py:293-432 按扩展名分发；引擎选择是 KB 级配置，因此「这篇是用哪个引擎解析的」可由 KB 配置反推。来源 https://github.com/xerrors/Yuxi
- **本仓现状**：我方降级链比它细（tools/embed_service.py:191 _p_pdf 三级 pymupdf4llm → :362 _pdf_fitz → :378 _pdf_pypdf，外加低文本页 _pdf_ocr_fill 走千问/tesseract，旧 Office 走 soffice），自报也比它诚实（:902 parse_caps 把 .pdf 的 tiers.text/tiers.ocr 机读上报）。但那是**服务级此刻快照**，不落到文档上：crates/connector/src/doc.rs:29-38 的 ParsedDoc 没有 engine 字段；crates/semantic/migrations/0020_kb_init.sql:117-144 的 kb.doc 没有对应列；ingest 只把 notes 拼进 notice（crates/knowledge/src/ingest.rs:838 与 :899-900），而 _pdf_fitz/_pdf_pypdf 在文本层正常时**根本不产 notes**。后果是可测的四条：tier-2/3 的 heading_path 恒空 → ①0020_kb_init.sql:345-351 的 kb.chunk_embedding_text 配方「章节」行对全篇同值（COALESCE(NULLIF(heading_path,''),'正文')）；②crates/knowledge/src/retrieve.rs:942-951 TITLE_SQL 的 word_similarity(q, c.heading_path) 半路失效；③crates/knowledge/src/answer.rs:609 source_of 的引用说不出章节；④kb_mindmap_api 的章节树整篇塌成一层。parser 容器换代（pip 列表变更）或 OCR key 轮换后，运维无从知道该重跑哪些文档。
- **改法**：三处各加一行 + 一条幂等 ALTER：①tools/embed_service.py:814 parse_doc 的返回 dict 加 'engine'：.pdf 取 f"{_pdf_text_engine()}+{_ocr_engine() or 'noocr'}"（两个函数已存在于 :716/:721），其余扩展名取 CAPS[ext][1] 那个能力名（已有）。②crates/connector/src/doc.rs:29 的 ParsedDoc 加 pub engine: String——结构上已有 #[serde(default)]，老服务不带这个键不会反序列化失败。③crates/knowledge/src/store.rs:109 的 KB_DDL_DELTA 追加 ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS parse_engine text NOT NULL DEFAULT ''，doc_cols! 宏与 DocRow 各加一位，ingest::run（ingest.rs:883）落库时写入。④crates/server/src/kb_api.rs 文档列表 JSON 透出 parse_engine，web/src/KbPanel.vue 列表加一格。⑤重跑清单不写代码：SELECT doc_id FROM kb.doc WHERE parse_engine LIKE 'fitz%' OR parse_engine LIKE 'pypdf%' 即是，走既有 reprocess。验收：单测钉住 ParsedDoc 缺 engine 键仍反序列化成功且为空串；ingest.rs:1961 那条 exts_cover_the_doc_service_capabilities 同款加一条「CAPS 每个能力名都能产出非空 engine 串」；上传一份 PDF 后 kb.doc.parse_engine 非空。
- 证据：tools/embed_service.py:191-243（_p_pdf 三级降级，明写后两级 heading_path 恒为空串）; tools/embed_service.py:362-376 与 :378-403（_pdf_fitz/_pdf_pypdf 文本层正常时返回值不含 notes）; tools/embed_service.py:902-921（parse_caps 只上报服务级此刻档位）; crates/connector/src/doc.rs:29-38（ParsedDoc 无 engine 字段）; crates/semantic/migrations/0020_kb_init.sql:117-144（kb.doc 列清单无解析来源列）; crates/knowledge/src/ingest.rs:838 与 :899-900（只有 notes → notice 这一条通道）
- confidence=high known=False

### [high/S] KB 问答落账没有检索证据：九路 stats 与 vec_down 只进 tracing 与 debug 端点，事后不可查

- **参考系统怎么做**：Yuxi 把「哪一路召回了什么」暴露在检索调试端点与管理面（dashboard_router.py 的会话/用户/工具/知识库/agent 五类统计+时序），per-answer 同样不留痕。这一条也不是抄它，是我方自身两侧口径不一致：我方已经有比它细一个量级的九路 SearchStats，却只用于日志。来源 https://github.com/xerrors/Yuxi
- **本仓现状**：crates/knowledge/src/retrieve.rs:325-330 的 SearchReport 带 vector_degraded 与 :305-321 的九路 SearchStats，但消费面只有两处：零命中时的一条 tracing::info（retrieve.rs:566-586）与 /api/kb/search 的诊断 JSON（crates/server/src/kb_api.rs:2357 附近）。落账走的 crates/knowledge/src/qa_log.rs:22-25 Obs 只有 usage 与 llm_calls，:35-50 的 Entry 里 sql 列写的是引用文档清单。于是生产上两个高频问题事后不可回答：「这题为什么没命中」（要靠复现，而复现时索引/阈值可能已变）与「这一周有多少条知识答案是在 embed 熔断期间产出的」。W4#3 让 vec_down 对用户可见并给 /api/health 加 breakers，但那是**当下状态**，没有账面。
- **改法**：只改两个函数、零迁移、不碰 meta.query_log 的共享列清单（kernel::qalog）：①crates/knowledge/src/qa_log.rs:22 的 Obs 加两个字段 routes: [usize; 9] 与 vec_down: bool；crates/knowledge/src/answer.rs:194 的 run() 本来就返回 (out, obs) 二元组，把 report.stats / report.vector_degraded 一并塞进 obs，不加任何形参。②新增纯函数 fn route_census(s: &SearchStats) -> String，产出定长人话「｜召回 v3 e0 tg2 ti1 m0 r0 kg0 x0 tm4 →6」；qa_log::entry（:84）把它追加到既有 sql 摘要串尾部，vec_down 为真时再前缀「｜降级:向量」。③一条纯函数单测钉住格式（顺序与 CHANNEL_NAMES 一致）。收益立刻可用：SELECT count(*) FROM meta.query_log WHERE route='knowledge' AND sql LIKE '%降级:向量%' 直接回答第二个问题，trace_api/usage_api 两个页面自动带出，前端一行不改。
- 证据：crates/knowledge/src/retrieve.rs:305-321（SearchStats 九路字段）与 :325-330（SearchReport.vector_degraded）; crates/knowledge/src/retrieve.rs:566-586（stats 唯一的结构化出口是零命中那条 tracing）; crates/knowledge/src/qa_log.rs:22-25（Obs 只有 usage/llm_calls）; crates/knowledge/src/qa_log.rs:35-50（Entry 的 sql 列＝引用文档清单，是自由格式摘要串）; crates/knowledge/src/retrieve.rs:317-319（terms_candidates 注释自认「不加 kb_api 那边」）; docs/OPTIMIZATION-PLAN-2026-08-13.md W4#3 只做用户可见提示与 health breakers，不落账
- confidence=high known=False

### [medium/S] kb.chunk.ts 生成列 + idx_kb_chunk_ts 全仓零读者：每块多存一份 tsvector、每次写多维护一个 GIN，换 0 次读

- **参考系统怎么做**：Yuxi 的稀疏半是 Milvus 内建 BM25 function + 中文 analyzer + SPARSE_INVERTED_INDEX（milvus.py:38,413-437），稀疏索引真的在被 AnnSearchRequest 查（milvus.py:1010-1033 双路 WeightedRanker）。来源 https://github.com/xerrors/Yuxi
- **本仓现状**：crates/semantic/migrations/0020_kb_init.sql:336 定义 ts tsvector GENERATED ALWAYS AS (to_tsvector('simple', text)) STORED，:370 建 idx_kb_chunk_ts GIN 索引。全仓零读者：grep -rn 'to_tsvector|ts_rank|tsquery' crates --include=*.rs 只命中 crates/knowledge/src/retrieve.rs:179-180、:816-817、:2017 三处**注释**——它们正是记录「那一路对中文恒 0、已被单号/型号 ILIKE 路替换」的实测证据。稀疏半今天由 terms text[] + idx_kb_chunk_terms（0020_kb_init.sql:378-379，jieba 分词由 store::terms_of 产出）承担。更坏的是这一对列/索引长得像「FTS 路还在」，下一个人会照着它把已知对中文恒 0 的那一路加回来。
- **改法**：纯删除，删除 > 新增：①crates/knowledge/src/store.rs:109 的 KB_DDL_DELTA 追加两行 DROP INDEX IF EXISTS kb.idx_kb_chunk_ts; 与 ALTER TABLE kb.chunk DROP COLUMN IF EXISTS ts;（幂等，与该常量既有两条 ALTER 同形）；②同刀删 crates/semantic/migrations/0020_kb_init.sql:336 与 :370 两行；③删 crates/knowledge/src/store.rs:2329 那条钉 GENERATED ALWAYS…STORED 的断言，换成一条反向断言「0020_kb_init.sql 不得再出现 tsvector」（防再次加回）。retrieve.rs:179-180 的实测注释**保留**——它是「别再加中文 FTS 路」的唯一证据。验收：删前删后 tools/kb_eval.py 16 题逐题结果字节一致（该列本来无人读 ⇒ 必须完全一致，任何差异都说明删错了）。
- 证据：crates/semantic/migrations/0020_kb_init.sql:336（ts 生成列）; crates/semantic/migrations/0020_kb_init.sql:370（idx_kb_chunk_ts GIN）; crates/knowledge/src/retrieve.rs:179-180（实测 322 格 ts_rank_cd 恒 0，该路已被替换）; crates/knowledge/src/retrieve.rs:816-822（替换后的 EXACT_SQL 用 ILIKE，不用 tsquery）; crates/semantic/migrations/0020_kb_init.sql:378-379（真正在用的稀疏索引是 terms 的 GIN）; crates/knowledge/src/store.rs:2329（今天有一条测试把这个死列钉成契约）
- confidence=high known=False

### [medium/M] 自动出题被明令禁止多跳，于是 KG-PPR / 相邻合并 / 关系扩展三条机制一条判据都没有

- **参考系统怎么做**：Yuxi 的 eval/benchmark_generation.py:117 出题时可开「图增强」：用图谱邻居 chunk 一起喂给出题模型，产出必须跨片段综合才能答的题，正好用来度量它的 Neo4j 2-hop + PPR + RRF 那一路。来源 https://github.com/xerrors/Yuxi
- **本仓现状**：crates/server/src/kb_eval_api.rs:190-194 的 GEN_SYSTEM 第 2 条明令「不许出需要外部知识、计算或**多片段综合**的题」，:142-149 的 SAMPLE_SQL 一次抽一块，:781-789 的 gold_rank 也只认单个 gold chunk。于是 recall@k 与 answer_acc 只覆盖单跳。而我方专门为多跳建的三样东西——crates/knowledge/src/retrieve.rs:1150 kg_route 的种子扩散 + :1346 personalized_pagerank、:1834 merge_adjacent（MAX_MERGE_SPAN=16）、:1108 relation_candidates——价值全部落在「一块答不了」的题上，今天零判据。这也正是 W4#10「先量图谱路收益再决定删或留」量不出来的原因：用单跳题去量一条只在多跳上生效的路，量出来必然是「没收益」，然后误删一个本可用的能力。
- **改法**：出题侧加一条可选路径，复用现成 Cypher：crates/connector/src/doc_graph.rs 已有 mention_pairs（同实体的 chunk 对，kg_route 正在用）。①crates/server/src/kb_eval_api.rs:642 sample_chunks 旁加 sample_pairs(st, space_id, n)：取同空间内共享 ≥1 实体、且 doc_id 不同的 chunk 对；②新增第二套 GEN_SYSTEM_MULTI（「必须同时用到两段才能答」），item 记两个 gold chunk_id（meta.kb_eval_items 加 gold_chunk_id2 bigint，幂等 ALTER 进 DDL 数组）；③gold_rank 改成「两个 gold 都进 hits 才算命中」（保留既有 merged 区间判定）；④meta.kb_eval_runs 加 multi_recall5 float8。KG 未建图的空间 sample_pairs 返空，自动退回单跳、不报错。排期上必须排在 W4#10 之前或同批——它是 W4#10 那条「先量」的量具。
- 证据：crates/server/src/kb_eval_api.rs:190-194（GEN_SYSTEM 明令禁多片段综合）; crates/server/src/kb_eval_api.rs:142-149（一次抽一块）; crates/server/src/kb_eval_api.rs:781-789（gold_rank 只认单个 gold）; crates/knowledge/src/retrieve.rs:1150-1240（kg_route 种子扩散）与 :1346-1389（personalized_pagerank）; crates/knowledge/src/retrieve.rs:1834-1872（merge_adjacent，MAX_MERGE_SPAN=16）; docs/OPTIMIZATION-PLAN-2026-08-13.md W4#10「先量后动」——今天的量具只有单跳题
- confidence=medium known=False

### [medium/M] 没有 parsed 稳定态：换分块 preset 必然整份重解析，扫描件＝重跑按页付费的 OCR

- **参考系统怎么做**：Yuxi 的状态机把 parse 与 index 分成两段（base.py:31-38 uploaded→parsing→parsed→indexing→indexed），parsed 是一个可停留的稳定态，重建索引不必重解析；乐观锁 update_fields_if_status（base.py:288-300）保证两段各自可独立抢占重跑。来源 https://github.com/xerrors/Yuxi
- **本仓现状**：我方状态是 pending→parsing→chunked→embedded|failed（crates/knowledge/src/store.rs:233-259），没有「parsed」这一档。向量配方升级那条路已经做对了（crates/semantic/migrations/0020_kb_init.sql:352-368 把 embedding 置 NULL、status 退回 chunked，由 crates/server/src/embed_fill.rs 后台补，不重解析）；但**换分块 preset 或改分块逻辑**走 ingest::reprocess（crates/knowledge/src/ingest.rs:531）→ build_shadow（:827）→ parse_input（:962），每次都从磁盘原文重新 doc.parse。代价不只是 120s 超时预算：扫描件走 tools/embed_service.py:293 _pdf_ocr_fill 逐页调千问 vision，重跑一份 100 页扫描件 = 100 次付费 OCR 调用，而分块逻辑本身一个字节都没变。
- **改法**：只加一张旁表、**不改状态机**（改状态机要连带动 stuck_docs 自愈、W4#13 的 CAS、reprocess 影子链一整片，不值）：①crates/knowledge/src/store.rs:109 的 KB_DDL_DELTA 追加 CREATE TABLE IF NOT EXISTS kb.doc_blocks(doc_id text PRIMARY KEY REFERENCES kb.doc(doc_id) ON DELETE CASCADE, engine text NOT NULL, blocks jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT now())；②crates/knowledge/src/ingest.rs:962 parse_input 成功后写一行（engine 取本条#3 新增的 ParsedDoc.engine）；③reprocess/build_shadow 先查这张表，engine 与 /health 当前上报档位相同即复用 blocks 跳过 doc.parse，不同则重解析并覆盖（引擎升级仍真重跑——这正是 #3 想要的行为）；④sheets 不进这张表（表格双通道另有物理表）；⑤单文档 blocks 序列化后超 8MB 不落缓存，写一行 ponytail: 注明这是保险丝天花板。验收：同一份 PDF 连跑两次 reprocess，第二次 parser 容器的 /parse 访问日志为 0 次；把 kb.doc_blocks.engine 人为改成别的值后必须重解析。依赖本报告第 3 条（engine 字段）。
- 证据：crates/knowledge/src/store.rs:233-259（五状态，无 parsed 稳定态）; crates/knowledge/src/ingest.rs:531-585（reprocess）与 :827-882（build_shadow）与 :962-993（parse_input 无条件调 doc.parse）; crates/semantic/migrations/0020_kb_init.sql:352-368（配方升级那条路已经做对：退回 chunked、后台补向量、不重解析）; tools/embed_service.py:293-361（_pdf_ocr_fill 逐页 OCR）与 :608-644（千问 vision 按调用计费）; crates/connector/src/doc.rs:18（PARSE_TIMEOUT_SECS=120，几十 MB PDF 是真的慢）
- confidence=medium known=False

### [low/S] 明确不要抄的六条：这些面我方已更强，抄回去是净退化

- **参考系统怎么做**：Yuxi 对应实现：①权限 permissions/resource_permission.py:87-102,170-196——KB 级 read_scope/manage_scope 双范围 + 有效权限 = min(授予, 角色上限)，无文档级 ACL；②引用 buildin/chatbot/prompt.py:31-32 自承「效果不好」已停用 prompt 级引用，输出无强制 cite；③混检 milvus.py:1010-1033 稠密 + BM25 两路 WeightedRanker(0.7,0.3)；④存储三分（结构在 Neo4j、向量在 Milvus、记录在 PG，外加 Redis/MinIO）；⑤分块 presets.py 的 semantic 无聚类（自认名不副实）；⑥无生效期/启停概念。来源 https://github.com/xerrors/Yuxi
- **本仓现状**：逐条对位我方已更强：①crates/knowledge/src/acl.rs:409-421 的 visible_docs! 是**文档级**并**内联进每条检索 SQL**（login/role/dept 三支取并集，不做查完再过滤），且 acl.rs:21-24 已白纸黑字裁决「不搬 Yuxi 的 min(授予,角色上限)」——我方 perm=read|write 显式授权，语义已等价；②引用是三层强制：answer.rs:667 keep_cited_only、:690 keep_supported_only、:895 disclose_versioned_sources + :1007 disclose_conflicting_numeric_claims，系统提示词（answer.rs:45-71）要求冲突必出「## 版本与差异」并列表并标「需人工确认」、绝不许静默只挑一份；③混检是九路加权 RRF（retrieve.rs:456-521，含 KG-PPR 路与 jieba 词级路），RrfWeights 还有配置闸拒 NaN/负值（:107-131）；④存储只有 PG 一套（AGE + pgvector + pg_trgm + jieba-rs），零新依赖；⑤我方 Semantic/Book 是同一个毛病，W4#6 已排删；⑥retrieve.rs:735-745 的 visible_sql 有 enabled + status + effective_from/to 三重生效期闸，Yuxi 完全没有；另有它没有的逐扩展名解析能力自报（tools/embed_service.py:902-921）。
- **改法**：**不做任何移植**，只做两件零风险的留痕，防下一轮对标的人重新论证一遍或误抄：①docs/research/yuxi.json 加一个新键 we_are_stronger（今天只有 half_baked），把上面六条逐条写成「Yuxi 做法 → 我方对位 file:line → 结论：不抄」；②crates/knowledge/src/acl.rs:21-24 那段「不搬角色上限」的裁决在 docs/ARCHITECTURE.md §4.5 的 acl.rs 行里没有对应句，补一行——裁决只写在源码注释里，文件一重构就消失。两处都是文档改动，零代码、零判据。
- 证据：crates/knowledge/src/acl.rs:21-24（已裁决不搬角色上限，理由写全）; crates/knowledge/src/acl.rs:409-421（visible_docs! 文档级 ACL 内联，login/role/dept 三支）; crates/knowledge/src/answer.rs:45-71（冲突必出并列表、绝不许静默只挑一份）; crates/knowledge/src/answer.rs:667/690/895/1007（四层过滤与披露）; crates/knowledge/src/retrieve.rs:456-521（九路加权 RRF 融合点）与 :107-131（权重配置闸）; crates/knowledge/src/retrieve.rs:735-745（enabled + status + 生效期三重闸）
- confidence=high known=False

### [medium/S] （已在方案内）rerank 生产从未接线，且窗口结构上救不回第 13-24 名

- **参考系统怎么做**：Yuxi 的 reranker 是 recall_top_k → 精排 → final_top_k 三段（models/rerank.py 的 OpenAIReranker/DashscopeReranker，milvus.py:1062-1097 打分失败回退向量分）——精排窗口就是召回候选全序，不是最终输出量的倍数。来源 https://github.com/xerrors/Yuxi
- **本仓现状**：crates/knowledge/src/retrieve.rs:164 RERANK_WINDOW = TOP_K * 2 = 12，而候选是 CANDIDATE_K = TOP_K * 4 = 24（:161），rerank_candidates（:1576）只对前 12 条重排 —— 精排的全部价值就是纠正一阶召回的排序错误，窗口卡在最终输出量的 2 倍等于只允许它在已进前 12 的块之间换位。同时 RerankClient::from_env（crates/connector/src/rerank.rs:55-63）要求 DMS_RERANK_BASE_URL + DMS_RERANK_MODEL 都在，而 scripts/server-restart.sh:188 的 docker run 只注入 DMS_SECRET_KEY —— 生产上这一路恒关。失败回退与降级留痕我方已经做对（retrieve.rs:1576-1583 warn + 原样返回）。
- **改法**：照 W4#8 原案执行，顺序不能反：先把 RERANK_WINDOW 改成 CANDIDATE_K（或删掉该常量，用 ranked.len().min(客户端 MAX_DOCS)）并订正注释，再把 DMS_RERANK_BASE_URL/MODEL 真传进 server 容器跑 kb_eval 定 gate。用一个结构上救不回第 13-24 名的窗口去测「精排有没有收益」，测出来的必然是「没收益」，然后误删一个本可用的能力。
- 证据：crates/knowledge/src/retrieve.rs:161-165（CANDIDATE_K=24 / RERANK_WINDOW=12）; crates/knowledge/src/retrieve.rs:1576-1596（rerank_candidates 只取 window 条）; crates/connector/src/rerank.rs:55-63（from_env 双变量缺一即关闭）; scripts/server-restart.sh:188（docker run 只注入 DMS_SECRET_KEY）; docs/OPTIMIZATION-PLAN-2026-08-13.md W4#8
- confidence=high known=True

### [medium/M] （已在方案内）词级稀疏路无 IDF：terms_of 去重让 TF 恒为 1，通用词与低频专名同权

- **参考系统怎么做**：Yuxi 的稀疏半是 Milvus 内建 BM25 function + 中文 analyzer（milvus.py:38,413-437），BM25 自带 IDF 与长度归一，通用词天然被压权。来源 https://github.com/xerrors/Yuxi
- **本仓现状**：crates/knowledge/src/retrieve.rs:885-893 的 TERMS_SQL 排序键是裸的「命中的不同问句词数」count(*)，而 crates/knowledge/src/store.rs:74 的 terms_of 里 `if !out.iter().any(|x| x == w)` 让每块的 TF 恒为 1 —— 「报销标准是多少」切出 [报销, 标准] 后，一篇满是「标准/管理/规定」的通用制度块与真正讲报销的块并列 2 命中；RRF 按名次融合会把这一路的错序原样传导到最终 TOP_K。TERMS_WEIGHT 恒 1.0（:58）与四路正文同级，所以错序的影响是满权重的。
- **改法**：照 W4#9 原案：TERMS_SQL 的排序键换成 sum(ln(1 + N/greatest(df,1)))，df 由一条对 kb.chunk 的 unnest 聚合子查询现算并缓存进 meta.kv（TTL 与导图快照同款），未登记词按 df=1 给满权；terms_min_hits 准入门槛不动，只换排序不换召回集。先不建 term_df 物化表——建表 + 同事务 upsert 是不可逆写路径改动，先用这版测出 recall 真涨再考虑物化。
- 证据：crates/knowledge/src/retrieve.rs:885-893（TERMS_SQL 按裸命中数排序）; crates/knowledge/src/store.rs:60-79（terms_of 去重 ⇒ TF 恒 1）; crates/knowledge/src/retrieve.rs:56-60（TERMS_WEIGHT 恒 1.0，与正文路同级）; docs/OPTIMIZATION-PLAN-2026-08-13.md W4#9
- confidence=high known=True


## 研究：Apache Doris 能力面 → 本仓「Rust/LLM 侧硬扛」的可下放项

本仓与 Doris 的关系是「只把它当一个能跑 MySQL 协议的大表」：连接池、EXPLAIN、时间模板、元数据探针四处都按通用 MySQL 写，Doris 自己的 MPP 能力面几乎没被调用过。全仓唯一一处用到 Doris 专属语法是 `crates/connector/src/graph.rs:276` 的 `SELECT /*+ SET_VAR(query_timeout=120) */`——说明「Doris hint 可用」这件事早被验证过，只是没有推广到主链。

四个结构性差距：

**① EXPLAIN 的载荷被整条丢弃。** `crates/connector/src/mysql.rs:857-863` 已经为每条数仓 SQL 付了一次 `EXPLAIN` 往返（run.rs:748，8s 预算），但 `Ok(Ok(_)) => Ok(None)` 把返回的执行计划文本直接扔掉，只当布尔用。Doris 的 EXPLAIN 在 OlapScanNode 行里免费给出 `partitions=已扫/总数`、`tablets=`、`cardinality=`、`avgRowSize`，以及（3.x 起）`MaterializedViewRewriteSuccessAndChose` 等改写命中字段。也就是说「这条 SQL 会全分区扫描」在执行前 0 成本可知，我们却要等 30s 超时（`crates/agent/src/gate.rs:23` EXEC_TIMEOUT）才发现。

**② Doris 会话零配置。** 数仓连接池分支（mysql.rs:452-458）只做了 sqlx 握手兼容（`pipes_as_concat(false)/timezone(None)/set_names(false)`），没有任何 `after_connect`；生产 MySQL 分支反而有（:464-465 `MAX_EXECUTION_TIME=2000`）。后果两条：(a) 客户端 30s 超时不取消服务端，Doris 默认 `qe_query_timeout_second=300` 继续跑满 5 分钟，agent 的重试会成倍叠加；(b) `timezone(None)` 让 Doris 会话用服务端默认 `time_zone`，而 PG 侧的「今天」在 `crates/server/src/daily_digest.rs:28-31` 被一丝不苟地钉成 `AT TIME ZONE 'Asia/Shanghai'`——`crates/kernel/src/nl/time.rs` 里 197 处 `CURDATE()` 却跑在一个没人钉过的时钟上。同一条纪律只做了一半。

**③ 预聚合与新鲜度全靠人手维护。** 深度报告的 5 个销售切片（deep_api.rs:3353 `SalesSlice{Region,WarZone,Customer,Goods,Trend}`）是 5 条同表、同时间窗、同权限 IN 列表、只有 GROUP BY 不同的 SQL，并发打同一批分区（deep_api.rs:2134-2176）。Doris 的异步物化视图 + 透明改写能一次覆盖全部——关键是**改写对 SQL 文本完全透明**，我们生成的 SQL 永远只写基表，`meta.metric` 不增一行，`grace_period=0` 时陈旧 MV 直接不被使用，这正好是「口径唯一事实源」纪律要的形态。同理，「数据算到昨天」今天是 `meta.metric.time_cap='yesterday'` 一个静态标志（recall/metric.rs:34），而 Doris 的 `partitions()` TVF 的 `Range` / `VisibleVersionTime` 能零扫描给出真实的最新已落数分区。

**④ 中文名匹配走全表 LIKE。** `entity.rs:1015/1019`、`entity_resolver.rs` 的客户/商品解析用 `LIKE '%名%'`，Doris 上无索引可用；倒排索引 `parser=chinese` + `MATCH_ANY` 既省扫描又带分词召回。

约束必须说清：`docs/warehouse_catalog.md:4` 记录我方是**只读 Doris 账号**，所以 ①②⑤⑥⑧⑨ 今天就能做，③④⑦ 需要数仓侧 DDL 授权（建议只在一个 `dms_ai` 专属库里建 MV/索引，不动业务表）。BITMAP/HLL 经核查不适合本仓，理由见对应条目。

### [critical/S] EXPLAIN 已经付了往返却把执行计划整条丢弃：分区裁剪、基数、MV 命中三个免费信号全没接

- **参考系统怎么做**：Doris 的 `EXPLAIN <sql>` 在 OlapScanNode 行里返回 `partitions=<已扫>/<总数>`、`tablets=<已扫>/<总数>`、`cardinality=<估算行数>`、`avgRowSize`；3.x 起计划尾部还带 `MaterializedViewRewriteSuccessAndChose` / `MaterializedViewRewriteSuccessButNotChose` / `MaterializedViewRewriteFail`（含失败原因）。这些是解析期产物，不取数、亚秒返回。来源：https://doris.apache.org/docs/3.x/sql-manual/sql-statements/data-query/EXPLAIN/ 与 https://doris.apache.org/docs/4.x/query-acceleration/materialized-view/async-materialized-view/faq/
- **本仓现状**：crates/connector/src/mysql.rs:857 已经 `format!("EXPLAIN {wire}")` 并 `fetch_all`，但 :859 `Ok(Ok(_)) => Ok(None)` 把返回的行集整条丢掉，只用成功/失败一个 bit。调用点 crates/agent/src/run.rs:748 因此只能识别「Doris 明确报错」，识别不了「语法合法但要扫全表 1358 万行」。后果：这类查询一路走到 crates/agent/src/gate.rs:23 的 EXEC_TIMEOUT=30s 才失败，且 crates/agent/src/answerers/hits.rs:175 的注释自己承认「系统一个字都没说，最后靠手工 EXPLAIN 才查出是执行超时」。
- **改法**：新增纯函数模块 crates/connector/src/explain_plan.rs（约 60 行，无 IO、可单测，避开 mysql.rs 已 1597 行超 D2 的问题）：`pub fn scan_verdict(plan: &str, total_floor: u32) -> Option<String>`，把 EXPLAIN 文本按行扫 `partitions=(\d+)/(\d+)`，当 已扫==总数 且 总数 >= total_floor（建议 8，即真分区表才判）时返回中文诊断串「计划显示全分区扫描 N/N，请为 <表> 增加分区列时间过滤」；顺带扫 `MaterializedViewRewriteFail` 只写 tracing::debug 不进裁决。mysql.rs:859 改为 `Ok(Ok(rows)) => Ok(explain_plan::scan_verdict(&join_text(&rows), 8))`。**不改 Source::explain 签名**——`Option<String>` 的既有语义（crates/connector/src/source.rs:141-143：Some = 数据库判定有问题、可拿去 repair）恰好适配：全分区扫描就是可被 LLM 修复的缺时间谓词，run.rs:750 现成的 `explain-fail` correction_log 与 repair 轮直接接住，零新增调用点。
- 证据：crates/connector/src/mysql.rs:857 `let stmt = format!("EXPLAIN {wire}");`; crates/connector/src/mysql.rs:859 `Ok(Ok(_)) => Ok(None),`; crates/connector/src/source.rs:141-143 explain 返 Option 的语义合同; crates/agent/src/run.rs:748-750 explain 调用点与 explain-fail 留痕; crates/agent/src/answerers/hits.rs:175 「靠手工 EXPLAIN 才查出是执行超时」; https://doris.apache.org/docs/3.x/sql-manual/sql-statements/data-query/EXPLAIN/
- confidence=high known=False

### [high/S] 数仓连接池没有 after_connect：客户端超时不取消 Doris 侧执行，且会话时区从没被钉过

- **参考系统怎么做**：Doris 会话变量 `query_timeout`（秒）由服务端强制中止查询，FE 默认 `qe_query_timeout_second=300`；`time_zone` 决定 `CURDATE()`/`NOW()` 的求值时区（官方 SHOW VARIABLES 示例里默认值是 `Asia/Hong_Kong`，随部署配置漂）。两者都是普通会话变量，只读账号可设，也可用 `/*+ SET_VAR(query_timeout=N) */` 逐条覆盖。来源：https://doris.apache.org/docs/3.x/sql-manual/basic-element/variables/ 、https://doris.apache.org/docs/3.x/admin-manual/config/fe-config/
- **本仓现状**：crates/connector/src/mysql.rs:452-458 的 warehouse 分支只设了 sqlx 握手兼容项（`pipes_as_concat(false)`/`no_engine_substitution(false)`/`timezone(None)`/`set_names(false)`），**没有任何 `after_connect`**；而同函数 :463-466 的生产 MySQL 分支有（`SET SESSION TRANSACTION READ ONLY` + `MAX_EXECUTION_TIME=2000`）。于是 (a) crates/agent/src/gate.rs:23 的 EXEC_TIMEOUT=30s 只是 `tokio::time::timeout`，超时后 Doris 端继续算到 300s，agent 的重试轮把它叠成 N×300s 的集群负载；(b) `timezone(None)` 意味着不发 SET time_zone，Doris 用服务端默认，而 PG 侧「今天」在 crates/server/src/daily_digest.rs:28-31 被显式钉成 `AT TIME ZONE 'Asia/Shanghai'`（注释还专门写了「应用/库时钟或容器 TZ 不一致时」），crates/kernel/src/nl/time.rs 里 197 处 `CURDATE()` 却跑在一个未钉的时钟上——跨日边界两侧口径可以不一致。全仓唯一给 Doris 设过预算的地方是 crates/connector/src/graph.rs:276 的 `SET_VAR(query_timeout=120)` 内联 hint，证明这条路可用但没推广。
- **改法**：在 crates/connector/src/mysql.rs::connect_read_only 的 warehouse 分支补一个 `after_connect`（4 行，与 :463-466 生产分支对称）：`conn.execute("SET query_timeout = 35")`（= EXEC_TIMEOUT 30s + 5s 余量，常量从 dms_agent::gate 那侧下沉或在 connector 侧新增 `const WAREHOUSE_QUERY_TIMEOUT_SECS: u64 = 35` 并在注释里指回 gate.rs:23）与 `conn.execute("SET time_zone = '+08:00'")`（与 daily_digest.rs:30 同一时钟纪律；用固定偏移而非地名，Doris 各版本地名表不一致）。两条都用 `.ok()` 容错吞掉——Doris 拒绝其中一条不该导致建池失败（fail-closed 优先可用性，I3）。同时把 graph.rs:276 的内联 hint 降为可删（会话已有预算，除非它确实要 120s 例外，那就保留并加注释说明为什么它比全局宽）。
- 证据：crates/connector/src/mysql.rs:452-458 warehouse 分支无 after_connect; crates/connector/src/mysql.rs:463-466 生产分支的 SET SESSION TRANSACTION READ ONLY / MAX_EXECUTION_TIME=2000; crates/agent/src/gate.rs:23 `pub const EXEC_TIMEOUT: Duration = Duration::from_secs(30);`; crates/server/src/daily_digest.rs:28-31 「今天」只从库侧时钟取 + AT TIME ZONE 'Asia/Shanghai'; crates/connector/src/graph.rs:276 `SELECT /*+ SET_VAR(query_timeout=120) */`; https://doris.apache.org/docs/3.x/admin-manual/config/fe-config/ (qe_query_timeout_second 默认 300)
- confidence=high known=False

### [critical/L] 异步物化视图 + 透明改写：预聚合五个销售切片，且天然不产生第二份口径

- **参考系统怎么做**：Doris 异步物化视图（`CREATE MATERIALIZED VIEW ... BUILD IMMEDIATE REFRESH AUTO ON SCHEDULE EVERY 1 HOUR PARTITION BY (date_trunc(order_date,'month')) DISTRIBUTED BY RANDOM BUCKETS n AS SELECT ...`）基于 SPJG 模式做**透明改写**：查询 SQL 一个字都不用改、也不许提到 MV 名，优化器自己判断能否用 MV 应答，并对更严的谓词做补偿。一致性由 `grace_period` 控制：`grace_period=0` 时 MV 与基表不同步就**不被使用**（回落基表）；分区 MV 部分失效时若开 `enable_materialized_view_union_rewrite`（2.1.5+ 默认开）会做 union 改写，用 MV + 基表拼出正确结果。开关 `enable_materialized_view_rewrite`。运维口：`SELECT * FROM mv_infos('database'='x')`、`SELECT * FROM tasks('type'='mv')`、`SHOW PARTITIONS FROM mv`（`SyncWithBaseTables` 列）。已知限制：查询含窗口函数不改写；MV 的表数多于查询的表数不改写；MV 内含 UNION ALL / LIMIT / ORDER BY / CROSS JOIN 不改写。来源：https://doris.apache.org/docs/3.x/query-acceleration/materialized-view/async-materialized-view/overview/ 与 .../functions-and-demands/ 与 .../faq/
- **本仓现状**：深度报告的销售部分是 5 条**同表、同时间窗、同权限 IN 列表、只有 GROUP BY 不同**的 SQL：crates/server/src/deep_api.rs:3353-3359 的 `SalesSlice{Region,WarZone,Customer,Goods,Trend}`，SQL 由 crates/server/src/deep_api.rs:1997-2032 `sales_section_sql` 经 crates/semantic/src/sales_fact.rs:635 `aggregate_sql_with_options` 生成，再由 deep_api.rs:2134-2176 `execute_plan_sections` 并发打到 `sales_dw.dws_off_offline_sale_dfn` 同一批分区。今天没有任何预聚合层，每个切片都是一次独立明细扫描。「口径唯一事实源」纪律使得任何形式的 ADS 预汇总表都被明令禁止（warehouse_catalog.rs:93-97：禁止用 ADS 金额替代默认销售事实），所以过去只能硬扛。
- **改法**：分两步，且**本仓代码零改动**是核心卖点。第一步（需数仓侧 DDL 授权，建议开一个 `dms_ai` 专属库放 MV，不动 `sales_dw`）：建一张与合同同粒度、同度量的 MV —— `CREATE MATERIALIZED VIEW dms_ai.mv_offline_sale_d BUILD IMMEDIATE REFRESH AUTO ON SCHEDULE EVERY 1 HOUR PARTITION BY (date_trunc(order_date,'month')) DISTRIBUTED BY RANDOM BUCKETS 16 PROPERTIES('grace_period'='0') AS SELECT order_date, storecode, storename, skucode, skuname, war_zone, region, SUM(qty) qty, SUM(amount) amount, SUM(cost_excluding_tax) cost_excluding_tax, SUM(revenue_excluding_tax) revenue_excluding_tax, SUM(gross_profit) gross_profit FROM sales_dw.dws_off_offline_sale_dfn GROUP BY 1,2,3,4,5,6,7`——度量表达式逐字抄 crates/semantic/src/sales_fact.rs:36-41 的 SNAPSHOT_COLUMNS 合同，毛利率不进 MV（它是派生比值，sales_fact 本来就不登记成物理列，MV 里也不登记，聚合后再除）。第二步（本仓）：只加**验收**，不加生成逻辑 —— 在 crates/semantic/src/sales_fact.rs 新增 `#[test] fn mv_grain_matches_contract()` 断言 MV 的 SELECT 列表逐字等于 `contract_columns()` 的投影（把 MV DDL 作为 `const MV_DDL: &str` 放在 sales_fact.rs 里做单一事实源，运维照抄，改合同必须同改 DDL 且测试会红）；并复用上一条的 explain_plan.rs 把 `MaterializedViewRewriteSuccessAndChose` 记进 tracing，让「这轮有没有走 MV」可观测。**口径不漂移的三重保证**：(1) 透明改写意味着我们生成的 SQL 永远只写基表，`meta.metric`/`registry` 一行都不增，语义层根本不知道 MV 存在；(2) `grace_period=0` 让陈旧 MV 直接不被采用，宁可慢不可错；(3) MV_DDL 与 `contract_columns()` 由同一个测试钉住，第二份口径在编译期就建不出来。
- 证据：crates/server/src/deep_api.rs:3353-3359 SalesSlice 五切片; crates/server/src/deep_api.rs:1997-2032 sales_section_sql（同 WHERE 不同 GROUP BY）; crates/server/src/deep_api.rs:2134-2176 execute_plan_sections 并发执行; crates/semantic/src/sales_fact.rs:29-41 SNAPSHOT_COLUMNS 度量合同; crates/semantic/src/warehouse_catalog.rs:93-97 「禁止用本表金额替代默认销售事实」; https://doris.apache.org/docs/3.x/query-acceleration/materialized-view/async-materialized-view/functions-and-demands/
- confidence=medium known=False

### [high/M] 数据新鲜度靠一个静态 time_cap 标志猜，而 Doris 的 partitions() TVF 零扫描就能给出真实最新分区

- **参考系统怎么做**：Doris 表值函数 `PARTITIONS("catalog"="internal","database"="<db>","table"="<t>")` 返回每个分区的 `PartitionName`、`Range`（分区区间）、`VisibleVersion`、`VisibleVersionTime`（该分区版本提交时间）、`DataSize`、`State`。这是 FE 元数据读取，不触碰 BE、不扫数据。来源：https://doris.apache.org/docs/3.x/sql-manual/sql-functions/table-valued-functions/partitions/
- **本仓现状**：「算到昨天」今天是一个人手维护的静态标志：crates/semantic/src/recall/metric.rs:34 的 `meta.metric.time_cap`，消费点在 crates/agent/src/gather.rs:244-248，把窗口右端压成 `< CURDATE()`（crates/kernel/src/nl/time.rs:546-553 `cap_at_yesterday`）。这个标志是「假设数仓每天凌晨跑完批」，没有任何一处向 Doris 求证过。真实批次延迟（跑批晚点、当天没跑）时，「本月销售额」照样把不完整的当期算进去并当成完整值答出来——这是准确性问题，不是时延问题。同时 crates/semantic/src/warehouse_catalog.rs:219 的资产合同里写着「时间用 data_date，**并展示数据新鲜度**」，但运行时没有任何代码去取这个新鲜度。
- **改法**：在 crates/semantic/src/warehouse_catalog.rs 新增 `pub async fn probe_partition_freshness(mysql: &..., assets: &[WarehouseAsset]) -> anyhow::Result<Vec<(String, String, DateTime<Utc>)>>`（表名, 最大分区 Range 上界, VisibleVersionTime），SQL 形态 `SELECT PartitionName, Range, VisibleVersionTime FROM partitions("catalog"="internal","database"=$db,"table"=$t) ORDER BY VisibleVersionTime DESC LIMIT 1`，库表名只从编译期白名单 `metadata_assets()`（warehouse_catalog.rs:439）取，绝不拼请求参数（沿用 mysql.rs:73 的同一条纪律）。结果落进已有的 `meta.warehouse_catalog_snapshot`（warehouse_catalog.rs:616-632 的建表处加两列 `fresh_until date`、`fresh_probed_at timestamptz`，`persist_draft`(:689) 同步 upsert），与目录探针同一次启动序完成，日常重启走 needs_sync(:475) 的短路不重复付费。消费侧改一处：gather.rs:248 的 `time_cap == "yesterday"` 判断改成「取 min(声明的 cap, 快照里的 fresh_until)」——声明仍是兜底，探针到就以实测为准。收益是把「答出一个偏低的当期数」变成「窗口自动收到真实已落数日期 + 在收据里说明数据截至哪天」。**低风险降级**：探针失败沿用今天的静态 time_cap，与 warehouse_catalog 的三档降级同构。
- 证据：crates/semantic/src/recall/metric.rs:34 time_cap 定义（''/'yesterday'）; crates/agent/src/gather.rs:244-248 time_cap 消费点; crates/kernel/src/nl/time.rs:546-553 cap_at_yesterday; crates/semantic/src/warehouse_catalog.rs:219 合同要求「展示数据新鲜度」但运行时无实现; crates/semantic/src/warehouse_catalog.rs:616-632 meta.warehouse_catalog_snapshot 建表处; crates/semantic/src/warehouse_catalog.rs:439-444 metadata_assets 编译期白名单
- confidence=high known=False

### [medium/M] 「必须限定时间范围」是 40 多条手写字符串，而分区列本可从 Doris 元数据推导

- **参考系统怎么做**：Doris 的分区键与分区区间可从 `partitions()` TVF 的 `Range` 列、或 `SHOW CREATE TABLE` 的 `PARTITION BY RANGE(col)` 子句读出；分区裁剪由优化器的 PruneOlapScanPartition 规则把 WHERE 谓词匹配到分区树上完成，命中与否直接体现在 EXPLAIN 的 `partitions=X/Y`。来源：https://doris.apache.org/docs/dev/table-design/data-partitioning/basic-concepts/ 、https://doris.apache.org/docs/dev/key-features/partitioning-and-bucketing/
- **本仓现状**：crates/semantic/src/warehouse_catalog.rs:87-389 的 ASSETS 里，每张表的 `time_rule` 都是人手写的自然语言（「时间只用 order_date，默认查询必须限定范围」「表量大，禁止无时间扫描」「统一时间列尚未验收」等，共 48 条资产），这些串只被拼进 `catalog_comment`(:871) 喂给 LLM 当提示词，运行时没有任何确定性校验能确认「模型真的按分区列过滤了」。真值（哪一列是分区列、分区区间到哪天）在 Doris 元数据里现成，我们却在 Rust 侧靠散文维护，且与 explain 的 `partitions=X/Y` 无任何交叉验证。docs/warehouse_catalog.md:348 甚至把「不为方便连接多张大表，优先分步小查询后在应用层合并」写成纪律——这条纪律的成立前提正是「不知道会不会裁剪成功」。
- **改法**：与上一条共用一次 `partitions()` 探针：把每张资产表的分区列名（从 `Range` 字符串首个标识符解析，解析失败即留空、绝不猜）写进 `meta.table_doc` 新列 `partition_col text NOT NULL DEFAULT ''`（DDL 落 crates/semantic/src/ddl.rs，与 :412 的 `ALTER TABLE meta.metric ADD COLUMN IF NOT EXISTS time_cap` 同款幂等形态）。两个消费点：(1) crates/semantic/src/warehouse_catalog.rs 加 `#[test] fn time_rule_names_the_real_partition_column()`——用快照校验每条手写 time_rule 里点名的列确实是分区列，让 48 条散文从「无人核对的注释」变成「有钉板的合同」；(2) 与本清单第 1 条的 explain_plan.rs 合流：`scan_verdict` 的诊断串里补上「本表分区列是 <col>」，让 repair 轮的 LLM 拿到可执行的修法而不是泛泛的「加时间过滤」。**不做**动态生成 time_rule（那些散文里含大量业务语义，不是元数据能推出来的），只做交叉校验。
- 证据：crates/semantic/src/warehouse_catalog.rs:87-389 ASSETS 的 48 条手写 time_rule; crates/semantic/src/warehouse_catalog.rs:871-884 catalog_comment 只把 time_rule 拼给 LLM; crates/semantic/src/ddl.rs:412 幂等 ADD COLUMN 的既有形态; docs/warehouse_catalog.md:348 「优先分步小查询后在应用层合并结果」; https://doris.apache.org/docs/dev/table-design/data-partitioning/basic-concepts/
- confidence=medium known=False

### [medium/M] 深度报告五个切片扫五遍同一批分区，GROUPING SETS 能压成一次扫描（不需要任何 DDL 权限）

- **参考系统怎么做**：Doris 原生支持 `GROUP BY GROUPING SETS ((a),(b),(c))` / `ROLLUP` / `CUBE`，语义等价于多条聚合的 UNION ALL 但只扫一次源；配套 `GROUPING(col)` 与 `GROUPING_ID(...)` 函数用于区分「该列被汇总掉」与「该列值本身是 NULL」。来源：https://doris.apache.org/docs/query-data/multi-dimensional-analytics/ 、https://doris.apache.org/community/design/grouping_sets_design/
- **本仓现状**：crates/server/src/deep_api.rs:1997-2032 `sales_section_sql` 为每个 SalesSlice 单独生成一条 SQL，五条的 WHERE 完全相同（同一个 `primary_sql` 的 WHERE，含时间窗 + 权限 storecode IN 大列表），只有 dimensions 与 sort 不同；deep_api.rs:2134-2176 并发下发。于是同一批分区被扫 5 次，那串上千元素的权限 IN 列表（docs/OPTIMIZATION-PLAN-2026-08-13.md:75 已记录该天花板）也被求值 5 次。
- **改法**：在 crates/semantic/src/sales_fact.rs 的 `QueryOptions`(约 :635 处的结构) 上加 `grouping_sets: &[&[Dimension]]`（默认空 = 今天的行为逐字不变，D9 不重排既有分支），`aggregate_sql_with_options` 在非空时把 `GROUP BY <维度>` 换成 `GROUP BY GROUPING SETS ((..),(..),..)` 并在 SELECT 里补 `GROUPING_ID(...) AS \`__gs\``；因为每个切片各要 `ORDER BY 指标 DESC LIMIT 200`，外层包一层 `SELECT * FROM (... , ROW_NUMBER() OVER (PARTITION BY \`__gs\` ORDER BY \`销售额\` DESC) rn) z WHERE rn <= 200`。deep_api.rs 侧新增 `fn split_by_grouping_id(columns,rows) -> HashMap<u32, (Vec<String>,Vec<Vec<Value>>)>`（约 30 行）把一份结果拆回五个 Section，`execute_plan_sections` 的五个切片分支改成一次 `fetch_sales_sql`。**先量后改**：这条的收益完全取决于 Doris 是否为 5 条同谓词查询各扫一次——用第 1 条落地的 EXPLAIN cardinality 先测一轮再决定，如果单条已经 <1s 就不值得（**这是刻意的 ponytail 停手线**）。注意 MV（第 3 条）与本条**二选一**：物化视图透明改写不支持含窗口函数的查询，上了 ROW_NUMBER 外层就改写不了了。
- 证据：crates/server/src/deep_api.rs:1997-2032 五切片同 WHERE 不同 GROUP BY; crates/server/src/deep_api.rs:2134-2176 execute_plan_sections; crates/semantic/src/sales_fact.rs:635-680 aggregate_sql_with_options; docs/OPTIMIZATION-PLAN-2026-08-13.md:75 权限 IN 列表字面量天花板; https://doris.apache.org/docs/query-data/multi-dimensional-analytics/; https://doris.apache.org/docs/4.x/query-acceleration/materialized-view/async-materialized-view/faq/ (窗口函数不参与透明改写)
- confidence=medium known=False

### [medium/L] 客户/商品名解析走 Doris 全表 LIKE '%名%'，倒排索引 + MATCH_ANY(chinese) 既省扫描又补分词召回

- **参考系统怎么做**：Doris 倒排索引基于 CLucene，DDL 形态 `INDEX <name>(<col>) USING INVERTED PROPERTIES("parser"="chinese","parser_mode"="fine_grained")`；查询用 `MATCH_ANY`（任一词）/`MATCH_ALL`（全部词）/`MATCH_PHRASE`（相邻词）/`MATCH_PHRASE_PREFIX`/`MATCH_REGEXP`。chinese parser 做中文分词，适合 CJK 与中英混排列。来源：https://doris.apache.org/docs/dev/key-features/inverted-index/ 、https://doris.apache.org/docs/dev/key-features/full-text-search/
- **本仓现状**：实体解析对 Doris 上的 `dms_ods` 主数据表全部用 `LIKE '%词%'`：crates/agent/src/answerers/entity.rs:1015（`t_customer.customer_name`）、:1019（`t_goods.goods_name`）、:780（`g.goods_name`）、crates/agent/src/answerers/entity/category.rs:21/33，以及 crates/agent/src/entity_resolver.rs 的 `customer_condition`（测试在 :192-196 钉了 `c.customer_name LIKE '%恒众%'` / `c.customer_short_name LIKE '%恒众%'`）。前置通配的 LIKE 在 Doris 上无索引可用，每次实体消歧都是一次主数据全表扫。准确性上更要命的是：LIKE 是纯字串包含，「恒众超市」查不到「恒众百货」这类同前缀不同后缀的候选，而 CustomerResolution 的三态（NotFound/Unique/Ambiguous，entity_resolver.rs:38-44）恰恰依赖候选集完整性——漏召回会把「歧义」误判成「唯一」，直接答错客户。
- **改法**：需数仓侧 DDL（对已有表 `ALTER TABLE dms_ods.t_customer ADD INDEX idx_cust_name(customer_name) USING INVERTED PROPERTIES("parser"="chinese","parser_mode"="fine_grained")` + `BUILD INDEX`，商品/门店同理）。本仓落点收敛到**一个函数**：crates/agent/src/entity_resolver.rs 的 `customer_condition`——它已经是客户模糊条件的唯一出口（`customer_candidates`(:71) 调它，实体卡与直查都从这里拿条件），把 `LIKE '%x%'` 换成 `MATCH_ANY 'x'`，并保留 LIKE 作为兜底（`(col MATCH_ANY 'x' OR col LIKE '%x%')` 在索引缺失时仍正确，Doris 无索引时 MATCH 会报错，所以更稳的形态是启动时探一次 `SHOW INDEX` 存进 AskCtx，无索引就走原分支）。entity.rs:1015/1019/780 与 category.rs:21/33 目前各写各的条件，属于「同一件事五份实现」——**先把它们收进 entity_resolver 的同一个条件构造器，再谈换 MATCH**（这是根因治理顺序，反过来做就是五处打补丁）。**确定性风险必须说清**：MATCH 的召回面比 LIKE 大，Ambiguous 分支会更常触发，对「准确 > 智能」是正向（宁可追问不可错绑），但要同步复核 CANDIDATE_LIMIT=8（entity_resolver.rs:12）会不会截断掉正确候选。
- 证据：crates/agent/src/entity_resolver.rs:71-80 customer_candidates / customer_condition 单一出口; crates/agent/src/entity_resolver.rs:192-196 测试钉住的 LIKE '%恒众%' 形态; crates/agent/src/entity_resolver.rs:38-44 CustomerResolution 三态依赖候选完整性; crates/agent/src/answerers/entity.rs:1015 / :1019 / :780 各自的 LIKE; crates/agent/src/answerers/entity/category.rs:21 / :33; https://doris.apache.org/docs/dev/key-features/inverted-index/
- confidence=medium known=False

### [medium/S] 季度/半年时间模板用 MAKEDATE + 非常量 INTERVAL 算，Doris 有 date_trunc 一步到位

- **参考系统怎么做**：Doris 提供 `date_trunc(<datetime>, '<unit>')`，unit 支持 second/minute/hour/day/week/month/quarter/year；它同时是异步物化视图 `PARTITION BY (date_trunc(col,'month'))` 的官方推荐写法。YEARWEEK(date[,mode]) 与 MAKEDATE(year,dayofyear) 在 Doris 中确实存在（各有独立函数页），所以现有模板不至于报错。来源：https://doris.apache.org/docs/sql-manual/sql-functions/scalar-functions/date-time-functions/makedate/ 、https://doris.apache.org/docs/dev/sql-manual/sql-functions/scalar-functions/date-time-functions/yearweek/ 、https://doris.apache.org/docs/3.x/query-acceleration/materialized-view/async-materialized-view/functions-and-demands/
- **本仓现状**：crates/kernel/src/nl/time.rs:390-391 的上季度模板是 `{} >= DATE_SUB(MAKEDATE(YEAR(CURDATE()),1) + INTERVAL QUARTER(CURDATE())*3-3 MONTH, INTERVAL 3 MONTH) AND {} < MAKEDATE(YEAR(CURDATE()),1) + INTERVAL QUARTER(CURDATE())*3-3 MONTH` —— 两处 `+ INTERVAL <非常量表达式> MONTH` 用的是 MySQL 的操作符式 INTERVAL 加法且间隔量是运行期表达式，这是全仓最脆的方言点；:381-382 的本季度模板还叠了 `DATE_FORMAT(CONCAT(YEAR(CURDATE()),'-{month}-01'),'%Y-%m-%d')` 这种字符串拼日期再解析的形态。整个 time.rs 有 197 处 MySQL 日期函数，从没在 Doris 上做过方言矩阵验收（本仓也没有对 Doris 的方言测试，crates/kernel/tests/sql_guard.rs 只测解析不测执行）。
- **改法**：两步、只动 time.rs：①先验收再改——写一个一次性脚本（放 tools/，不入主链）把 time.rs 的全部模板在真实 Doris 上跑 `EXPLAIN SELECT <模板>`，产出方言矩阵；这是 S 级且必须先做，否则后面全是猜。②确认 date_trunc 可用后，把 :386-391 的季度三支与 :502-503 的月份支换成 `{} >= date_trunc(CURDATE(),'quarter') - INTERVAL 3 MONTH AND {} < date_trunc(CURDATE(),'quarter')` 形态——从 2 行嵌套表达式压成 1 行、消掉字符串拼日期、消掉非常量 INTERVAL，且与第 3 条 MV 的 `PARTITION BY (date_trunc(...))` 用同一个函数（分区裁剪更容易命中）。**不动**其余五支相对时间模板（time.rs:118-131/218-227 已被 docs/OPTIMIZATION-PLAN-2026-08-13.md:89-90 那批改动覆盖，D9 不重排既有分支）。
- 证据：crates/kernel/src/nl/time.rs:390-391 MAKEDATE + 非常量 INTERVAL 的季度模板; crates/kernel/src/nl/time.rs:381-382 CONCAT 拼日期再 DATE_FORMAT 解析; crates/kernel/src/nl/time.rs:502-503 同款月份模板; crates/kernel/src/nl/time.rs 全文 197 处 MySQL 日期函数（grep 计数）; https://doris.apache.org/docs/sql-manual/sql-functions/scalar-functions/date-time-functions/makedate/; https://doris.apache.org/docs/3.x/query-acceleration/materialized-view/async-materialized-view/functions-and-demands/ (date_trunc 分区写法)
- confidence=low known=False

### [low/S] Doris SQL Cache 对带权限谓词的问数无效，但对图同步这条无权限全量聚合是白捡

- **参考系统怎么做**：Doris `enable_sql_cache`（会话或 global 级 `set enable_sql_cache=true`）缓存整条 SQL 的结果集，支持 OlapTable 内表；缓存键含 SQL 文本与影响结果的会话变量（time_zone/sql_mode/sql_select_limit 等），分区版本一变即失效，因此官方定位是 T+1 离线分析场景。另有独立的 pipeline 级 Query Cache（按 tablet 缓存聚合中间结果、把分区谓词从摘要中剥离）。来源：https://doris.apache.org/docs/query-acceleration/tuning/tuning-plan/accelerating-queries-with-sql-cache/ 、https://doris.apache.org/docs/4.x/query-acceleration/query-cache/
- **本仓现状**：主问数链的每条 SQL 都被注入用户专属的 storecode IN 大列表（crates/server/src/deep_api.rs:2060-2062 gate_on 注入 scope，docs/OPTIMIZATION-PLAN-2026-08-13.md:75 记录该列表可达上千项），SQL 文本因人而异 → SQL Cache 命中率接近 0，全局打开只是白占 FE 内存。但 crates/connector/src/graph.rs:275-288 的图同步聚合是**完全无权限谓词、无时间窗、全量 GROUP BY** 的固定 SQL，每天跑、字节完全一致、上限 250001 行——这是 SQL Cache 的教科书场景，今天没开。
- **改法**：**不做全局开关**（那是 fail-open 式优化，且与 I4「缓存不跨用户/源」在直觉上冲突——虽然 Doris 的键含 SQL 文本因而技术上不跨用户，但把它变成全局默认会让「哪条走了缓存」不可观测）。只在 graph.rs:276 那条已有的 hint 里加一项：`SELECT /*+ SET_VAR(query_timeout=120, enable_sql_cache=true) */ ...`，一行改动，作用域精确到这一条语句。同时在该处注释里写清判据：**只有不含 scope 谓词的固定同步查询才允许开 sql_cache**，防止后来者把它抄到问数主链上。若图同步不是瓶颈就连这一行也不做（YAGNI）——先看 crates/connector/src/graph.rs:271 那个 GRAPH_SOURCE_TIMEOUT=120s 预算平时用掉多少。
- 证据：crates/connector/src/graph.rs:275-288 无权限谓词的固定全量聚合 SQL; crates/connector/src/graph.rs:271-276 GRAPH_SOURCE_TIMEOUT 与既有 SET_VAR hint; crates/server/src/deep_api.rs:2060-2062 gate_on 注入用户 scope 谓词; docs/OPTIMIZATION-PLAN-2026-08-13.md:75 权限列表上千项; https://doris.apache.org/docs/query-acceleration/tuning/tuning-plan/accelerating-queries-with-sql-cache/; https://doris.apache.org/docs/4.x/query-acceleration/query-cache/
- confidence=medium known=False

### [low/XL] BITMAP / HLL 精确与近似去重：核查后判定不适用，去重痛点的根因是扇出不是 COUNT DISTINCT 本身

- **参考系统怎么做**：Doris 的 BITMAP 精确去重要求 key 是整型（varchar 需 `bitmap_hash`/`bitmap_hash64`，有碰撞风险）并把列建成 AGGREGATE 模型的 BITMAP 列，查询侧只能用 `bitmap_union_count`/`bitmap_union` 等配套函数；HLL 与 `APPROX_COUNT_DISTINCT` 是近似路线，官方标称相对标准误差 0.8125%。来源：https://doris.apache.org/docs/3.x/sql-manual/sql-functions/aggregate-functions/bitmap-union-count/ 、https://doris.apache.org/docs/dev/sql-manual/sql-functions/aggregate-functions/approx-count-distinct/ 、https://doris.apache.org/docs/dev/query-acceleration/distinct-counts/hll-approximate-deduplication/
- **本仓现状**：本仓的去重键全是 varchar 业务码：`sales_order_code`（crates/server/src/daily_digest.rs:65 `COUNT(DISTINCT sales_order_code)`）、`customer_code`、`sku_code`（crates/agent/src/answerers/entity.rs:1086/1195-1198/1377-1380）。BITMAP 路线需要 (a) 数仓 DDL 改表模型、(b) 给 varchar 码做字典编码或接受 bitmap_hash 的碰撞——后者直接违反「准确 > 智能」。近似路线（APPROX_COUNT_DISTINCT/HLL）的 0.8%误差落在「订单数 12,847」这类用户会去核对的整数上，同样不可接受。而本仓真正的去重痛点根本不在这里：crates/kernel/src/sql/caliber.rs:296 的 `require_dedup` 规则、crates/agent/src/gather.rs:762 的「扇出边仅 COUNT(DISTINCT) 聚合可过」说明，问题是**JOIN 扇出后把行数当单据数**，BITMAP/HLL 对此一点帮助没有（它们只加速已经写对的去重）。
- **改法**：**明确不做**，并把这条结论写进 docs/OPTIMIZATION-PLAN-2026-08-13.md 的「明确不采用/已覆盖」表（该表已存在，见该文档 :671 那一行的形态），理由三条：varchar 键需碰撞哈希、近似误差与准确性第一轴冲突、真实痛点是扇出而非去重算法。**替代动作（真正值钱的那半）**：`COUNT(DISTINCT a), COUNT(DISTINCT b), COUNT(DISTINCT c)` 多列去重在 Doris 上会退化成多阶段聚合，而 entity.rs:1195-1199 一条 SQL 里就并排了 4 个 COUNT(DISTINCT)（含 `COUNT(DISTINCT DATE_FORMAT(o.order_time,'%Y-%m'))` 这种表达式去重）。若实测该查询偏慢，正确改法是用第 1 条落地的 EXPLAIN cardinality 定位，而不是换去重算法。
- 证据：crates/server/src/daily_digest.rs:65 COUNT(DISTINCT sales_order_code); crates/agent/src/answerers/entity.rs:1195-1199 一条 SQL 四个 COUNT(DISTINCT); crates/kernel/src/sql/caliber.rs:296 require_dedup 规则（扇出才是根因）; crates/agent/src/gather.rs:762 「扇出边仅 COUNT(DISTINCT) 聚合可过」; https://doris.apache.org/docs/3.x/sql-manual/sql-functions/aggregate-functions/bitmap-union-count/; https://doris.apache.org/docs/dev/sql-manual/sql-functions/aggregate-functions/approx-count-distinct/
- confidence=high known=False


## 研究：our-learning：本仓学习/记忆面的活死盘点（写了什么 → 谁读回来 → 断在哪）

今天本仓有 7 个学习面，只有 1 个是全自动闭环，而它恰恰是唯一没有复核门的那个。

**写侧（一次问答之后落了什么）**：① `meta.sql_exemplar`（run.rs:941 `save_with_context`，status=pending/validation_status=unverified，仅 LLM 路且 `worth_learning` 放行时；admin_api.rs:1178 的 HITL sql-edit 也写一条）；② `meta.memory`（run.rs:869-886，route=llm+repair 且有行时 spawn 写一条 kind=review 经验，无状态列）；③ `meta.pitfall` 候选（run.rs:915-921 → review.rs:55 `review_failure` → exemplar.rs:346 `save_lesson_candidate`，status=candidate）；④ `meta.correction_log` 17 个 kind（run.rs:1347）；⑤ `meta.failure_log`（exec-error/zero-rows，run.rs:851/915）；⑥ `meta.query_log`（server 侧观测）；⑦ `meta.query_feedback`（quality_api.rs:92，仅 KB 答案有入口）。

**读侧（下一次问答谁读回来）**：`fewshot`/`nearest`/`suggest_questions` 三条都硬要求 `status='enabled' AND validation_status='valid'`（exemplar.rs:21/174/327），而这个状态的**唯一出口**是 admin 网页上人点一次「验证并启用」（admin_api.rs:289 `EX_VALIDATE_OK_SQL`）—— 没人点，few-shot 段、语义缓存、冷启动推荐三条读路径**恒空**。教训召回要 `status='active'`（pitfall.rs:31），种子那 9 条是 active 的所以活着，但 LLM 产的 candidate 要跑 CLI `review-lessons` 才转正 —— 全仓没有任何调度器/cron 调这两个子命令（main.rs:754/764 是纯 CLI，grep 脚本与 DEPLOY.md 零命中）。`recall_memories`（memory.rs:67-78）只过滤 `ds_id + embedding IS NOT NULL`，**没有任何复核门**，而 embedding 由 server/src/embed_fill.rs 每 600s 自动补 —— 于是「写→向量→进所有人 prompt」是本仓唯一真正自动跑起来的学习回路，且它学的是未经任何人看过的文本。

**断点**：(a) 语料/教训闭环断在「复核只有手动扳手」；(c) 污染面在 `meta.memory`（无门、无 origin、无回滚 API）与 `review_failure` 喂 `scoped.wire()`（把行级权限条件送进 LLM 复盘、产出 ds 级共享教训，隔壁 run.rs:869 的经验蒸馏为同一理由特意用了 candidate）；admin sql-edit 存 `scoped.wire()` 与线上存 candidate 不一致，直接违反 cache.rs:6-8 写死的前提。(d) 反馈只有 KbAnswer.vue:139 一个前端调用点，问数与小程序零入口，且 `query_feedback` 下游只有 admin 质量页，不接任何学习面 —— 用户说「这个数字错了」不会让那条语料停止传播。(e) 死件：`correction_log`/`failure_log` 全仓零 SELECT；`schema_snapshot`/`side_info` 两列写了没人读；`RECALLED_KINDS` 里 `routing`/`column_fix` 两个 kind 零写口。回滚手段只有 `audit-exemplars --fix`（只管语料，且跨源用 DMS 规则），pitfall 与 memory 一旦学错只能连 psql 手写 SQL。

### [critical/S] meta.memory 是全仓唯一自动跑通的学习闭环，而它零复核门、零回滚面、逐字带别人的问句

- **参考系统怎么做**：SuperSonic 的 ChatMemory 与 MemoryReviewTask：记忆行带 status 三态（PENDING/ENABLED/DISABLED）+ llmReviewRet/humanReviewRet 两栏，只有复核后的记忆才参与后续生成；本仓自己在 sql_exemplar 上完整移植了这套（VQR 三态 + AI 初筛不许直接 enabled），却没在 memory 上装同一道门（https://github.com/tencentmusic/supersonic ；本仓映射记录见 docs/INTEGRATION-TRACE.md:44）
- **本仓现状**：crates/semantic/src/ddl.rs:141-152 建的 meta.memory 只有 (id, ds_id, conv_id, kind, question, content, embedding, hit_count, created_at) —— 没有 status 列。crates/semantic/src/registry/memory.rs:67-78 的 recall_memories 只过滤 `ds_id = $2 AND embedding IS NOT NULL`，取近邻 10 条重排后前 3 条进 prompt（crates/agent/src/gather.rs:34 MEMORY_LIMIT=3、:167、crates/agent/src/prompt.rs:144-146）。写侧 crates/agent/src/run.rs:869-886 每次 llm+repair 成功就 spawn 写一条，content 是 `问「{q}」：首版 SQL 未过口径复核或执行出错，修正后通过。正确写法：{fixed}` —— 别人的问句原文（可能含客户名/商品名/金额）逐字进所有同源用户的 prompt。embedding 由 crates/server/src/embed_fill.rs:24 每 600s 自动补齐（MetaVecTarget::Memory，crates/semantic/src/registry/embed_fill.rs:75-80）。全仓无任何 admin API/UI/CLI 触碰 meta.memory（grep `meta.memory` 在 crates/server/ 零命中），学错一条只能连 psql DELETE。
- **改法**：三处小改，零新依赖：① crates/semantic/src/ddl.rs 追加 `ALTER TABLE meta.memory ADD COLUMN IF NOT EXISTS status text NOT NULL DEFAULT 'candidate';`（老行默认 candidate = 立刻停止传播，fail-closed）；② crates/semantic/src/registry/memory.rs 的 recall_memories WHERE 加 `AND status='active'`，并在 crates/semantic/src/registry/embed_fill.rs 的 Memory select_sql 同步加同一条件（不给候选行白烧 embed）；③ 复用 crates/agent/src/review.rs 已有的 `review_lessons` 形状新增 `pub async fn review_memories(llm, pg, limit) -> Result<usize>`（同一 LESSON_SYSTEM 判词、同一 parse_verdict、写口 `memory::set_status(pg, id, status)` 放 memory.rs），挂进 crates/server/src/main.rs:764 已有的 `review-lessons` 子命令一起跑。删除面：不需要新表、新 trait、新端点。
- 证据：crates/semantic/src/ddl.rs:141-152（meta.memory 建表，无 status 列）; crates/semantic/src/registry/memory.rs:67-78（recall 只过滤 ds_id + embedding IS NOT NULL）; crates/agent/src/run.rs:869-886（llm+repair 成功即自动写，content 含别人问句原文）; crates/server/src/embed_fill.rs:24,30-41（每 600s advisory-lock 后台自动补向量）; crates/agent/src/gather.rs:167,178-181 + crates/agent/src/prompt.rs:144-146（前 3 条直接进 prompt）; grep `meta.memory` 在 crates/server/ 零命中（无管理面、无回滚 API）
- confidence=high known=False

### [critical/S] 复核/晋升通道今天没有调度器：语料三条读路径（few-shot / 语义缓存 / 冷启动推荐）默认恒空

- **参考系统怎么做**：SuperSonic 的 MemoryReviewTask 是一个**定时任务**（Spring @Scheduled 扫 PENDING 批量送 LLM 初筛），本仓 review.rs 文件头明写「移植 SuperSonic MemoryReviewTask 定时扫 pending」，但只移植了函数体，没移植「定时」那一半（https://github.com/tencentmusic/supersonic）
- **本仓现状**：crates/semantic/src/registry/exemplar.rs:21/174/327 三条召回 SQL 都硬要求 `status='enabled' AND validation_status='valid'`（且有 only_execution_validated_exemplars_are_recalled 判据钉着）。而这个状态的唯一出口是 crates/server/src/admin_api.rs:289-292 的 EX_VALIDATE_OK_SQL，只能由 admin 在网页上逐条点「验证并启用」触发（admin_api.rs:345-436 validate_exemplar）。AI 初筛（exemplar.rs:280-304 set_ai_review）刻意不授予 enabled，这是对的；但没有任何自动进程把 pending 往前推。crates/server/src/main.rs:754-770 的 `review-pending` / `review-lessons` 是纯 CLI 子命令，全仓（*.sh / *.ps1 / *.py / docs/DEPLOY.md）grep 零调用点，也没有 cron/systemd timer。结果：只要没人手工点，few-shot 段（gather.rs:734）、语义缓存回放（cache.rs:42 nearest）、冷启动推荐（main.rs:1979 suggest_questions）三条读路径全部返回空，而三处都是静默降级（fewshot_text 空语料连标题都不出，gather.rs:737-739），线上看不出「学习面从来没启动过」。
- **改法**：照抄仓内已有的后台循环形态，不引入调度框架：crates/server/src/embed_fill.rs 已经有一条 `spawn(st)` + `pg_try_advisory_lock` + 600s 间隔的循环（:22-41），在同文件加第二个 LOCK_KEY 的 `pub fn spawn_review(st: Arc<AppState>)`，每 3600s 依次调 `dms_agent::review::review_all_pending(&llm, pg, 100)` 与 `review_lessons(&llm, pg, 100)`（两个函数签名已是 `(llm, pg, limit) -> Result<usize>`，review.rs:12-13 明写不许变形），在 main.rs:1346 `embed_fill::spawn(state.clone())` 旁边接一行。人工 VQR 那道门**不动**（AI 仍只能 pending→disabled），自动化的只是「把候选送到人面前」这一步；同时在 admin 质量页把 `pending` 计数露出来（quality_api.rs:174-181 的 summary 加一个字段），让「队列没人处理」变成看得见的数字。
- 证据：crates/semantic/src/registry/exemplar.rs:21,174,327（三条召回的 enabled+valid 硬门）; crates/server/src/admin_api.rs:289-292,345-436（enabled+valid 的唯一出口是人工点击）; crates/server/src/main.rs:754-770（review-pending / review-lessons 只是 CLI）; grep review-pending 在 *.sh/*.ps1/*.py/DEPLOY.md 零命中（无调度）; crates/agent/src/gather.rs:737-739（空语料静默不出段，故障不可见）; crates/server/src/embed_fill.rs:22-41（可直接复用的后台循环形态）
- confidence=high known=False

### [high/S] 死件清单：failure_log 全仓零 SELECT、schema_snapshot/side_info 两列写了没人读、pitfall 三个 kind 里两个零写口

- **参考系统怎么做**：SQLBot / SuperSonic 的分步日志之所以有价值，是因为下游有聚合消费者（同错聚类 → 升格规则）。本仓 ddl.rs:287 的注释自称「报错类由 LLM 复盘产出候选教训」，但那条复盘用的是内存里的错误串，不是表
- **本仓现状**：① `meta.failure_log`：写口只有 crates/semantic/src/registry/exemplar.rs:420-431，全仓 grep `FROM meta.failure_log` 零命中。真正的复盘（review.rs:55 review_failure）是 run.rs:915-921 在内存里拿 `&err` 直接调的 —— spawn 挂掉/进程重启，那条教训永久丢失，表里躺着的行没人补跑。ddl.rs:287-290 写的「同错累计→升格 pitfall」这个累计器不存在。② `meta.sql_exemplar.schema_snapshot` / `side_info`（ddl.rs:319-320，A10 同构快照）：写在 exemplar.rs:209-218，但没有任何 SELECT 取这两列 —— admin 的 EX_LIST_SQL（admin_api.rs:280-284）不选，fewshot 只 `SELECT question, sql`（exemplar.rs:20），渲染函数 fewshot_text（gather.rs:737-746）也只吃两元组。run.rs:1267-1268 每轮都算一遍 snapshot 白付。③ crates/semantic/src/recall/pitfall.rs:19 `RECALLED_KINDS = ["pitfall", "routing", "column_fix"]`，但 `routing` 与 `column_fix` 在全仓只出现在这一行 —— 种子（seed.rs:662）与候选沉淀（exemplar.rs:353）都只写 `'pitfall'`，两个 kind 的读路径恒空。
- **改法**：删除 > 新增，分三刀：① pitfall.rs:19 把 RECALLED_KINDS 收成 `&["pitfall"]`（同步改文件头那句「新增 kind 不进召回」的注释）—— 少一个会漂的清单；② 二选一处置 A10 快照：要么删 —— ddl.rs:319-320 两条 ALTER 保留（不删列，老库无害）但把 exemplar.rs:200-227 的 `save_with_context` 收回成 `save(pg, ds, question, sql)`，run.rs:1249-1268 的 snapshot 计算与 State.snapshot 字段一起删；要么用 —— fewshot 的 SELECT 加 `side_info`，fewshot_text 渲成第三行。倾向删（今天的 few-shot 是两行式，A10 的价值未被证明）。③ failure_log 补一条读路：main.rs 加 `replay-failures [--limit N]` 子命令，`SELECT id, question, sql, error FROM meta.failure_log WHERE kind='exec-error' AND at > now()-interval '7 days'` 逐条调 `review::review_failure`，把「spawn 挂了就永久丢教训」变成可补跑（复用现成函数，零新逻辑）。
- 证据：crates/semantic/src/registry/exemplar.rs:420-431（failure_log 唯一写口）+ grep `FROM meta.failure_log` 零命中; crates/semantic/src/ddl.rs:287-290（注释宣称的「同错累计升格」不存在实现）; crates/semantic/src/ddl.rs:319-320 + crates/semantic/src/registry/exemplar.rs:209-218（写）vs crates/server/src/admin_api.rs:280-284、crates/semantic/src/registry/exemplar.rs:20、crates/agent/src/gather.rs:737-746（三处读侧都不取）; crates/semantic/src/recall/pitfall.rs:19 vs crates/semantic/src/seed.rs:662、crates/semantic/src/registry/exemplar.rs:353（只写 'pitfall'）; docs/OPTIMIZATION-PLAN-2026-08-13.md:399-404（W5-8 已覆盖 correction_log 的读侧，故本条不含它；但该条写的「failure_log 已由既有 FAILED_SQL 覆盖」不成立 —— trace_api.rs:134-141 的 FAILED_SQL 读的是 meta.query_log，不是 failure_log）
- confidence=high known=False

### [high/S] HITL sql-edit 存的是注入后的 SQL（scoped.wire()），把管理员的行级权限条件烙进全员共享语料

- **参考系统怎么做**：本仓自己的纪律：语料存注入前原文、回放时按当轮用户重新注入（I4）。cache.rs 文件头把这条写成了硬前提
- **本仓现状**：crates/server/src/admin_api.rs:1178-1182：`exemplar::save(st.owned.pool(), ds_reg::DMS_DS_ID, question, scoped.wire())` —— 存的是过完 `gate_on` 的串。而线上路径 crates/agent/src/run.rs:941 存的是 `st.candidate`（闸门前原文），crates/agent/src/answerers/cache.rs:6-8 明写「语料里存的是**注入前**的 SQL 原文（`exemplar::save` 存的是 `candidate`），所以回放时重走一遍 `gate_on`。直接复用注入后的串就是把甲的行级条件端给乙」。两条后果：① 语义缓存命中这条语料时会对已注入的 SQL 再注入一次（cache.rs:58 gate_on），结果被那位管理员的 scope 二次收窄 —— 数字看着正常但少算；② few-shot 会把这条 SQL 里的客户编码/员工编码 IN 列表原样喂进别人的 prompt（exemplar.rs:20 `SELECT question, sql`）。管理员不等于超管，`compute_scope_cached` 对非 administrator_flag 的 admin 角色照样产条件。
- **改法**：crates/server/src/admin_api.rs:1178-1182 把 `scoped.wire()` 改成人写的原文 `sql`（它在 :1170 已经过了与线上同一条 `dms_agent::gate`，安全性不降），返回体的 `"sql": scoped.wire()` 保持不变（给管理员看实际执行的是什么）。同刀在 crates/semantic/src/registry/exemplar.rs 的 tests 里加一条源码守卫（照该文件 only_execution_validated_exemplars_are_recalled 的模子，但扫 ../server/src/admin_api.rs 与 ../agent/src/run.rs）：`save`/`save_with_context` 的调用行不许出现 `.wire()` —— 这是 I4 在语料侧唯一可被源码钉住的形状。
- 证据：crates/server/src/admin_api.rs:1178-1182（存 scoped.wire()）; crates/agent/src/run.rs:941（线上存 st.candidate）; crates/agent/src/answerers/cache.rs:5-8（把「存注入前原文」写成硬前提）; crates/agent/src/answerers/cache.rs:58（回放对命中的 SQL 再走一次 gate_on）
- confidence=high known=False

### [high/S] review_failure 拿 scoped.wire() 当复盘素材：行级权限条件进 LLM prompt，产出的 lesson 是 ds 级共享的

- **参考系统怎么做**：同一份代码在隔壁 15 行处已经给出了正确做法与理由，只是没有推广到这一支
- **本仓现状**：crates/agent/src/run.rs:915-921：失败复盘的 spawn 里 `let sql = scoped.wire().to_string();` 然后送 `review::review_failure(llm, &pg, &ds, &q, &sql, &err)`。而同函数 :869-874 的经验蒸馏写着「素材用 `candidate`（闸门前原文）—— wire() 会把行级权限条件写进经验，而经验是 ds 级共享的（跨用户泄漏面，与语料同一条防线）」，那一支用了 st.candidate。复盘产出的 lesson 经 review.rs:73 落 meta.pitfall（candidate → 复核后 active），随后进**所有**同源用户的 prompt（pitfall.rs:69-92 → gather.rs:189）。虽然 FAILURE_SYSTEM（review.rs:21-24）要求「≤80字、表X.列Y 式口径知识、禁止复述错误原文」，但它只是提示词，不是闸门 —— LLM 完全可能写出「表 t_customer.customer_code 需限定在 CUS00xx 范围」。同一行的 `log_failure_traced(cx.pg, "exec-error", cx.question, scoped.wire(), ...)`（run.rs:915、:851）也把别人的 scope 条件落进 failure_log。
- **改法**：crates/agent/src/run.rs:917-919 把 `let sql = scoped.wire().to_string();` 改成 `let sql = st.candidate.clone();`（execute 的签名里已有 &mut State，可取），与 :869-874 完全同源。failure_log 那两行保留 wire()（那是排障取证、写完没人读也没人喂 LLM，风险面不同）但在 exemplar.rs:420 的 doc 上标明「本表含注入后条件，任何把它送进 LLM/prompt 的读路必须先剥」。守卫：在 crates/agent/src/run.rs 的 tests 加源码断言 —— `review_failure(` 与 `save_memory(` 两个调用点所在的 spawn 块里不许出现 `.wire()`（该文件已有 correction_log 十七个 kind 的同款源码守卫，:1649）。
- 证据：crates/agent/src/run.rs:915-921（review_failure 收 scoped.wire()）; crates/agent/src/run.rs:869-874（同文件已写明 wire() 是跨用户泄漏面，经验蒸馏为此用 candidate）; crates/agent/src/review.rs:65,73（wire 进 prompt → lesson 落 meta.pitfall）; crates/semantic/src/recall/pitfall.rs:69-92 + crates/agent/src/gather.rs:189（pitfall 进所有同源用户 prompt）
- confidence=high known=False

### [high/S] fewshot 没有相似度下限：语料库只要非空，每个问句都会被塞两条「相似问题的正确写法」，哪怕相似度≈0

- **参考系统怎么做**：兄弟读路径已经有下限：语义缓存的 nearest 结果由 cache.rs 的三关护栏把关，第一关就是余弦距离 MAX_DIST=0.12
- **本仓现状**：crates/semantic/src/registry/exemplar.rs:19-38 的 fewshot：`ORDER BY word_similarity($1, question) DESC LIMIT 8` 之后 `.take(2)` —— 没有任何 `word_similarity >= x` 的门槛。渲染侧 crates/agent/src/gather.rs:737-746 的 fewshot_text 只判「rows 是否为空」，非空就出「## 相似问题的正确写法（参考口径）」这个标题 —— 于是一条完全不相干的历史语料被冠以「正确写法（参考口径）」送进 precise 模型。这与语义缓存的把关强度不对称（crates/agent/src/answerers/cache.rs:22 MAX_DIST=0.12 + 时间词/数字词全等三关，:97-107）。注意这**不是** OPTIMIZATION-PLAN W3-5 覆盖的那条：那条改的是 crates/semantic/src/recall/schema.rs 的 `trgm_tables` 表召回，与本条不同文件、不同读路。
- **改法**：crates/semantic/src/registry/exemplar.rs 加一个常量 `const FEWSHOT_FLOOR: f32 = …;`，fewshot 的 SQL 改为 `WHERE … AND word_similarity($1, question) >= $3`（绑定顺序连带 ds_pred 下标一起改，ds_pred(2)→ds_pred(3)）。阈值**先量后定**：用一次性 SQL 量一遍现网 `word_similarity` 分布再拍，代码里写 `// ponytail: floor 按 <日期> 分布标定，换语料要重标`（与 W3-5 同一纪律，两处可以共用同一次测量）。同刀给 exemplar.rs 的 tests 加一条：三条召回函数的函数体必须含 `word_similarity` 门或距离门（今天只钉了 enabled+valid 两条件）。
- 证据：crates/semantic/src/registry/exemplar.rs:19-38（fewshot 无相似度下限）; crates/agent/src/gather.rs:737-746（非空即冠以「正确写法（参考口径）」标题）; crates/agent/src/answerers/cache.rs:22,97-107（兄弟读路有 MAX_DIST + 三关护栏，形成不对称）; docs/OPTIMIZATION-PLAN-2026-08-13.md:235-240（W3-5 只覆盖 schema.rs 的 trgm_tables，不含本条）
- confidence=high known=False

### [high/M] 回归/评测脚本走生产 ask 路径，每跑一轮就往三张学习表灌数据（判官污染语料库）

- **参考系统怎么做**：评测与生产共用一条链路是对的（tools/evaluation.py 文件头「不比 SQL 文本，比执行结果」的立意就靠这个），但共用链路必须能关掉副作用写入
- **本仓现状**：crates/server/src/main.rs:506-550 的 `eval_batch_one` 直接调生产 `ask(...)`，crates/server/src/main.rs:2762 的 ask 出口就是 `dms_agent::ask`。于是每一题都会走到 crates/agent/src/run.rs:941（写 meta.sql_exemplar pending）、:869-886（写 meta.memory，无门、10 分钟后自动向量化、随即进所有人 prompt）、:851/915（写 failure_log）、:1347（写 correction_log）。tools/regression.py（79 题）与 tools/evaluation.py（`--runs 3`，文件头自述全量一趟 40 分钟）共用同一 CLI —— 一次三趟评测最多 237 轮，判官账号的问句与 LLM 现写的 SQL 全部进 pending 队列，人工复核页从此被判官题淹没；而 meta.memory 那一路根本不经复核就生效。tools/evaluation.py 用的是 `--legacy-cli` 之外的驻留 batch 模式，同样是这条路。
- **改法**：给学习副作用一个显式开关，落在 agent 的入参上而不是调用点打补丁：crates/agent/src/ask.rs 的 `AskDeps`（或 `AskCtx`，crates/agent/src/ctx.rs:56 附近已有 trace_id/conv_id 这类透传位）加 `pub learn: bool`（Default = true）；crates/agent/src/run.rs 在 `if worth_learning(st, &rs)`（:936）与 `if st.route == "llm+repair"`（:869）两个判据前各 `&& cx.learn`。调用侧只改两处：crates/server/src/main.rs 的 `eval-batch` 与一次性 `ask` 两个 CLI 分支传 false，HTTP 端点不动。零新表、零新 trait（bool 不是 trait）。验证：给 run.rs 加一条源码守卫断言两个 spawn 都在 `cx.learn` 之后。
- 证据：crates/server/src/main.rs:506-550（eval_batch_one 调生产 ask）、:2762（ask → dms_agent::ask）; crates/agent/src/run.rs:936-941（worth_learning → save_exemplar）、:869-886（经验蒸馏 spawn）、:851/915（failure_log）; tools/regression.py:1-8（79 题全量跑同一 CLI）; tools/evaluation.py:6-9,20-26（--runs 3，全量一趟 40 分钟，同一 batch CLI）; crates/server/src/embed_fill.rs:24（判官写的 memory 行 600s 内被自动向量化并生效）
- confidence=high known=False

### [high/M] 用户反馈只有知识库答案有入口，且 query_feedback 不接任何学习面 —— 说「错了」不会让那条语料停止传播

- **参考系统怎么做**：反馈闭环的最小形态是「负反馈直接作用于产生这个答案的那条记忆」；本仓已经有现成的作用点（exemplar::set_status 把语料置 disabled），只是没连线
- **本仓现状**：写侧：crates/server/src/quality_api.rs:72-114 的 `/api/feedback` 端点齐全（trace_id 绑本人 query_log、五种 kind、400ms 重试预算），但全仓**唯一**前端调用点是 web/src/KbAnswer.vue:139（知识库答案的两键 👍/👎，kind ∈ {correct, data}）。问数答案面板（web/src/ResultPanel.vue）没有反馈入口，企业微信小程序（crates/server/src/xcx_api.rs）grep `feedback` 零命中 —— 三端里两端半没有反馈。读侧：`meta.query_feedback` 的唯一消费者是 crates/server/src/quality_api.rs:155-165 的 admin 质量页（列 30 条给人看），不写回任何学习表。于是「用户指出这条答案的口径错了」与「学习面继续按那条口径传播」互不相干；admin 只能手工去语料页找那条 question 再点 disabled。
- **改法**：两步，都不新开端点：① 前端补入口 —— web/src/ResultPanel.vue 复用 KbAnswer.vue:114-145 那套两键 + localStorage 记忆的形状，POST 同一个 `/api/feedback`（trace_id 已在问数的收据里）；小程序同理。② 补上唯一缺的那根线 —— crates/server/src/quality_api.rs 的 `feedback` handler 在 INSERT 成功且 `kind ∈ {caliber, data}` 时，用已经 RETURNING 回来的那行的 `q.question` / `q.ds_id`（把绑定 SQL 的 RETURNING 从 `id` 扩成 `id, question, ds_id`）调一次 `dms_semantic::registry::exemplar::set_status(pg, ds, question, "disabled")`（该函数已存在、已传播错误、已有 0 行 warn，exemplar.rs:258-276）。语义是「用户说这个数字/口径错了 → 这条语料立刻停止当范例」，只做减法不做加法，误伤的代价是少一条 few-shot（可由 admin 重新验证启用），符合 fail-closed。
- 证据：crates/server/src/quality_api.rs:72-114（端点齐全）、:155-165（唯一消费者是 admin 列表）; web/src/KbAnswer.vue:114-145（全仓唯一前端调用点，只在 KB 答案上）; grep `api/feedback` 在 web/src/ 只命中 KbAnswer.vue；grep `feedback` 在 crates/server/src/xcx_api.rs 零命中; crates/semantic/src/registry/exemplar.rs:258-276（现成的 set_status 作用点，无人从反馈侧调用）
- confidence=high known=False

### [medium/S] pitfall 与 memory 一旦学错就没有关闭开关：set_lesson_status 只服务 candidate，且种子与 LLM 产物混在一张表无 origin 列

- **参考系统怎么做**：可回滚是学习面的准入条件；本仓在语料侧做到了（audit-exemplars --fix + admin 页 disable + VQR stale 自动失效），教训与经验两侧没做
- **本仓现状**：crates/semantic/src/registry/exemplar.rs:387-398 的 `set_lesson_status(pg, id, status)` 是 meta.pitfall 唯一的状态写口，而它的唯一调用者是 crates/agent/src/review.rs:101 的 `review_lessons`，那个循环只取 `status='candidate'` 的行（exemplar.rs:378-380）。一条已经转成 `active` 的坏教训（无论来自 LLM 复盘还是种子）没有任何 API / UI / CLI 能关掉 —— crates/server/ 下 grep `meta.pitfall` 零命中，web/src/App.vue grep 「教训」零命中。crates/semantic/src/seed.rs:657 自己记着这笔账：「pitfall 无 origin 列，种子与复核产物分不开，待设计」——于是连「先关掉所有机器学来的、保留种子」这种粗粒度回滚都做不到。meta.memory 更彻底：连 status 列都没有（见本报告第 1 条）。对照语料侧：main.rs:959-1015 的 `audit-exemplars --fix` + admin_api.rs:293-298 的 EX_DISABLE / EX_STALE_SOURCE + seed.rs:72-89 的指标版本变更自动置 stale，三层都有。
- **改法**：两刀，共约 30 行：① crates/semantic/src/ddl.rs 给 pitfall 加 `ALTER TABLE meta.pitfall ADD COLUMN IF NOT EXISTS origin text NOT NULL DEFAULT 'seed';`，crates/semantic/src/registry/exemplar.rs:352-357 的 `save_lesson_candidate` INSERT 显式写 `'llm'`（seed.rs:661-666 的种子 INSERT 保持默认 'seed'）—— 一列就把 seed.rs:657 那笔欠账结清，且让「回滚所有机器学的」变成一条 UPDATE。② crates/server/src/main.rs 加 `lessons [--disable <id>] [--origin llm]` 子命令（形状照 audit-exemplars，main.rs:959）：无参列出 active 教训（id/origin/trigger/lesson 前 60 字），`--disable` 走已有的 `exemplar::set_lesson_status(pg, id, "disabled")`（不需要新写口）。不做管理页 —— 教训是几十条量级，CLI 够，加页面就要再守一个权限面。
- 证据：crates/semantic/src/registry/exemplar.rs:387-398（唯一状态写口）+ crates/agent/src/review.rs:82,101（唯一调用者只处理 candidate）; crates/semantic/src/registry/exemplar.rs:378-380（candidate_lessons 只取 candidate）; crates/semantic/src/seed.rs:657（自记欠账：pitfall 无 origin 列，种子与复核产物分不开）; grep `meta.pitfall` 在 crates/server/ 零命中；grep 「教训」在 web/src/App.vue 零命中; 对照组：crates/server/src/main.rs:959-1015（audit-exemplars --fix）、crates/server/src/admin_api.rs:293-298、crates/semantic/src/seed.rs:72-89
- confidence=high known=False

### [medium/S] audit-exemplars 拿 DMS 的口径规则审计所有源的语料，--fix 会误 disable 上传源语料；这条 SQL 还逃过了 ds 漂移守卫

- **参考系统怎么做**：本仓的 ds 隔离靠 drift.rs 扫源码强制每条 `FROM meta.` 带 ds 谓词 —— 前提是那个文件在扫描清单里
- **本仓现状**：crates/server/src/main.rs:970-975：`SELECT id, question, sql, status FROM meta.sql_exemplar WHERE status <> 'disabled' ORDER BY id` —— **无 ds 过滤**；紧接着 :980 对每一行都用 `build_rules(pg, ds_reg::DMS_DS_ID, q, &tables)` 造规则。上传源（`up_*`）的语料会被拿 DMS 的口径规则去 check_caliber，违规几乎必然，`--fix` 时（:1003-1007）直接 `UPDATE ... SET status='disabled'` —— 把别的源里合法的、已经人工 VQR 通过的语料一次性 disable 掉。而这条 SQL 之所以没被拦下，是因为 crates/semantic/tests/drift.rs:12-21 的 EXTERNAL 清单只含 `../server/src/direct.rs`、`../server/src/corrector.rs`、`../policy/src/rules.rs`，不含 main.rs / admin_api.rs / quality_api.rs —— `every_meta_recall_is_ds_scoped`（drift.rs:57-90）对 server 的大半个 crate 是失明的。
- **改法**：① crates/server/src/main.rs:970-975 的 SELECT 补 `ds_id` 列并逐行用**该行自己的 ds_id** 调 build_rules（audit_tables 已经是逐行的，改动 3 行），或最省：SQL 加 `AND ds_id = $1` 绑 DMS_DS_ID 并把命令输出改成「本次只审 DMS 源」。② 更根因：把这条 SQL 收进 `crates/semantic/src/registry/exemplar.rs`（该文件已是 meta.sql_exemplar 的唯一读写口，文件头 :1-8 就是这么写的）新增 `pub async fn rows_for_audit(pg, ds, limit) -> Result<Vec<(i64,String,String,String)>>`，main.rs 只留编排 —— 这样它自动落进 drift.rs 的 `src/**` 全树扫描，不需要维护清单。③ 顺手在 drift.rs:12-21 的 EXTERNAL 加 `../server/src/main.rs` 与 `../server/src/admin_api.rs`（admin_api 的 EX_* 常量都带 id/status 谓词、无 ds 谓词，加进去要先给它们标 `ds:any` —— 那正是「作者想过这件事」的证据）。
- 证据：crates/server/src/main.rs:970-975（无 ds 过滤的全表 SELECT）; crates/server/src/main.rs:980（对所有源的行都用 DMS_DS_ID 造规则）; crates/server/src/main.rs:1003-1007（--fix 无条件置 disabled）; crates/semantic/tests/drift.rs:12-21（EXTERNAL 清单不含 main.rs / admin_api.rs）; crates/semantic/tests/drift.rs:57-90（守卫本体，只扫 semantic/src/** + 那 3 个外部文件）; crates/semantic/src/registry/exemplar.rs:1-8（文件头自称「meta.sql_exemplar 的唯一读写口」—— main.rs 这条 SQL 违背了它）
- confidence=high known=False

### [medium/S] F6 的 visibility/VIS_PRED 跨用户隔离是文档里的幻影：三份权威文档描述它在跑，代码里一个字都没有

- **参考系统怎么做**：—（这是文档与实现的对账问题，不是外部机制移植）
- **本仓现状**：docs/ARCHITECTURE.md:63 的 I4 行写着「few-shot/教训召回带 `visibility` 谓词」；:96 的 F6 给出方案「meta.sql_exemplar/meta.pitfall 加 visibility+owner_login，召回统一加 VIS_PRED」；:181 说 registry/mod.rs 有「DS_PRED/VIS_PRED 两个谓词常量」；:184 说 exemplar.rs「带 VIS_PRED（F6）」；:210 说 drift.rs 断言「每条召回 SQL 含 DS_PRED 与 VIS_PRED」；:333 的流程图写「语料沉淀 + 异步复核（visibility 默认 private）」。实际：全仓 grep `VIS_PRED` 零命中；meta.sql_exemplar 与 meta.pitfall 都没有 visibility / owner_login 列（crates/semantic/src/ddl.rs:96-103, 153-161, 319-331 全部 ALTER 清单里没有）；crates/semantic/tests/drift.rs 只有 ds 那一条守卫。代码里残留的两处注释（crates/semantic/src/registry/exemplar.rs:4「与 F6 之后的 visibility 两道总闸」、crates/agent/src/review.rs:10「ds/visibility 两道总闸」）会让下一个人以为有第二道闸。真实的跨用户隔离只有一道：人工复核门 `status='enabled' AND validation_status='valid'` —— 而本报告第 2 条指出那道门今天没有自动送人复核的通道，第 1 条指出 meta.memory 连这道门都没有。
- **改法**：按 OPTIMIZATION-PLAN W7 §651 ④ 的处置执行（该条已立项，本条只补两点它没写到的）：① 除了改 ARCHITECTURE.md §2 I4 与 §3 F6，还要清掉代码里的两处误导注释（crates/semantic/src/registry/exemplar.rs:4、crates/agent/src/review.rs:10）以及 ARCHITECTURE.md:181/184/210/333 四处 —— 计划里只点了 §2/§3；② 计划要求的 drift.rs 新守卫（exemplar.rs 里每处 `FROM meta.sql_exemplar` 的读 SELECT 必须同含 enabled+valid）**必须把 meta.memory 一并纳入**：memory 是今天唯一没有任何门的共享召回面，只钉 exemplar 会让守卫给出「F6 已封堵」的假绿。
- 证据：docs/ARCHITECTURE.md:63,96,181,184,210,333（六处描述 visibility/VIS_PRED 在跑）; grep VIS_PRED 全仓零命中；grep visibility 在 crates/semantic/ 只命中 exemplar.rs:4 的注释; crates/semantic/src/ddl.rs:96-103,153-161,319-331（pitfall / sql_exemplar 的完整列清单里无 visibility / owner_login）; crates/agent/src/review.rs:10、crates/semantic/src/registry/exemplar.rs:4（代码里残留的「两道总闸」误导注释）; docs/OPTIMIZATION-PLAN-2026-08-13.md:651 ④（已立项：改写成事实 + 加 drift 守卫）
- confidence=high known=True


## 研究：routing：一句话里同时有知识库问题和问数问题的处理现状与收口方案

真实机制：一次问答今天有**两个编排器**。`prepare_question`（ask.rs:164）在四个入口都跑一次「追问改写 → 日期继承 → 错字归一 → understand()」，产出 `PreparedQuestion`；然后 `ask_prepared`（ask.rs:209）开头两道早返（:216 `route()!=Data` / :222 任一 typed child 非 Data）把 Hybrid 与 Knowledge 全部挡在 agent 门外，直接吐澄清卡。真正的混合编排是 server 的第二套：`hybrid_branch`(main.rs:2246) → `hybrid_payload`(main.rs:2147) → `hybrid_pair`(:2200) 1:1 配对 → `tokio::join!` 两路并行 → `compound::hybrid_summary` 落 `view.insight` + `payload["kb"]` + `hybrid_intent_summary`(:2426) 重写收据。于是 CLI/判官链路（main.rs:1179 → `dms_agent::ask`）与 HTTP 链路对同一句话行为相反：前者只会出反问卡。回归 79 题清一色 Data（route 分布 direct-agg 38 / direct-doc 11 / graph 5 / entity-card 4 / need-intent 3 / compound 2 / business-lookup 1），零 Knowledge、零 Hybrid，而 `tools/regression.py:264` 走的正是那条结构上跑不了混合的 CLI —— 混合路径「测不到」不是漏写题，是链路层面测不了。

断点②核实成立且比描述更深：`IntentV1::route()`(intent.rs:412) 先看 `route_from_subgoals`(:1121)，subgoals 空则回落到 `match (mode, has_data_slots)`(:436)，而该 match 只有 Data/Knowledge 两臂，`(Hybrid, _)` 落 `_ => Unknown`；紧接着 `ground()`(:801) 对 `route()==Unknown` 直接 `return None` → `IntentAttempt::Invalid` → 卡。同一条 `return None` 还吞掉了模型自报的 `ambiguities` 和 `project()` 自己 push 的「结构化子任务归属不唯一」(:629)，用户看到的永远是那句泛泛的「未通过一致性校验」，`coverage_with_evidence` 里 `ambiguity:` → conflicts 那段(:1469)因此结构性不可达。

另有五处此前没被点名的断裂：①server 六处分发里的两道「闸」`prepared_contract_ready`(main.rs:2490) 与 `route==Data && !is_data_executable()`(main.rs:2540/2630、deep_api.rs:4543) 是**可证明的恒真/恒假**（Ready 只能由 `ground()` 造，而它已强制 ambiguities 空且 route≠Unknown），所以 xcx/mcp 少这两道不是「碰巧等价」而是恒等 —— 修法是删不是补测试；②混合单路失败只有 `tracing::warn`(main.rs:2172/2179)，payload 静默换形，用户看不出少答了一半，与 `compound::missing_note`(compound.rs:115) 立的纪律自相矛盾；③`hybrid_intent_summary` 在 `attach_trust`(ctx.rs:356) 之后才覆盖 `intent_summary`，于是资料侧零引用照样顶着数据半算出来的 `verified`；④`hybrid_pair` 只收恰好 1 Data + 1 Knowledge，多一个子问就整轮澄清；⑤深度模式撞上 Hybrid 直接返回普通混合 payload（deep_api.rs:4587），深度子问 `sub_ask`(deep_api.rs:2180) 命中 Knowledge 则 section 静默缺席。收口方向：把路由与两路合成整体上移到 agent 的一个新文件，server 只留「Knowledge+SSE」一个分支。

### [critical/L] ask_prepared 开头两道早返把 Hybrid/Knowledge 挡在 agent 外，混合编排整套住在 server（第二套编排器）

- **参考系统怎么做**：AGENT-ARCHITECTURE §5 自己写的边界是「单一循环，禁止再造平行编排器」（pi 的 explicit agent loop 对照行）。参考的 pi（earendil-works/pi packages/agent/agent-loop.ts）把工具选择与结果合成都放在同一个 loop 里，宿主只负责 IO 形状。
- **本仓现状**：crates/agent/src/ask.rs:216 `if prepared.route() != IntentRoute::Data { return Ok(prepared.clarification_result()); }`，紧接 :222 `.any(|child| child.route != IntentRoute::Data)` 再返一次。于是 agent 对混合/知识只会出澄清卡；真正的编排在 crates/server/src/main.rs:2147 `hybrid_payload`（hybrid_pair→tokio::join!→hybrid_summary→payload["kb"]）。CLI（crates/server/src/main.rs:1179 走 dms_agent::ask）与 HTTP 对同一句话行为相反。
- **改法**：新建 crates/agent/src/route.rs（约 250 行，D2 内；变更原因＝「一次路由怎么分派并合成」）：`pub async fn run(d:&AskDeps<'_>, p:&Principal, prepared:&PreparedQuestion, explicit_ds:Option<&str>) -> anyhow::Result<AskResult>`。内部：match prepared.route() → Data 调现有 ask::ask_prepared；Knowledge 调 answerers::knowledge::answer；Hybrid 用 futures::future::join 并行跑两半（futures 已是 agent 依赖）。AskDeps 增两个**非 Option** 字段 `owned: &'a dms_connector::owned::OwnedStore`、`kb: KbDeps<'a>{ weights:&'a dms_knowledge::retrieve::RrfWeights, space_id: Option<&'a str> }`——非 Option 是关键，CLI 在 main.rs:1152 已经有 owned、cfg 已有 kb_rrf_weights（db.rs:161），结构上不给「CLI 少一路」留口子。AskResult 增 `#[serde(skip_serializing_if="Option::is_none")] pub kb: Option<dms_kernel::Answer>`，与 server 今天注入的 payload["kb"] 逐字节同形，前端 App.vue:3172 零改动。ask::ask_prepared 的两道早返降级为 debug_assert（它从此只被 route.rs 以 Data 合同调用）。server 六处入口只留一个分支：`route()==Knowledge && 客户端要 SSE` → 现有 spawn_kb_worker；其余全部 route::run。删除 hybrid_payload/hybrid_pair/hybrid_cardinality_clarification/hybrid_branch/HybridAsk/xcx_hybrid_payload（约 200 行）。
- 证据：crates/agent/src/ask.rs:216; crates/agent/src/ask.rs:222; crates/server/src/main.rs:2147; crates/server/src/main.rs:2246; crates/server/src/main.rs:1179; crates/server/src/xcx_api.rs:464
- confidence=high known=False

### [critical/M] mode=hybrid 但模型没吐 subgoals → route() 落 Unknown → ground() 整份判 Invalid → 反问卡

- **参考系统怎么做**：结构化意图的路由只承认「已 grounding 的 typed subgoal 或可执行槽位」，模型的 mode 单独不足以定路由（intent.rs:410 注释）。
- **本仓现状**：crates/agent/src/intent.rs:412 `route()`：subgoals 为空时回落到 :436 的 `match (self.mode, has_data_slots)`，该 match 只有 `(Data,true)` 与 `(Knowledge,_)` 两臂，`(Hybrid,*)` 全部落 `_ => IntentRoute::Unknown`；随后 crates/agent/src/intent.rs:801 `if self.route()==Unknown || !ambiguities.is_empty() { return None }` 让整份合同变 Invalid（不是「Ready 但 Unknown」），`user_note()`(:1266) 吐的是「未通过一致性校验」。实测「小虎青菜香菇薄皮包子420g 的信息 和 拆单标准」正是这条链。understand()(:1360) 只调一次模型、无 repair。
- **改法**：两刀，都在 intent.rs：①`route()` 的 match 补一臂 `(IntentMode::Hybrid, true) => IntentRoute::Hybrid`——模型已经明确说了「两件事都要」，这是**已表达的事实**，不是猜；②新增 `IntentAttempt` 的可判别态 `HybridUnsplit(ResolvedIntent)`（或在 ResolvedIntent 上加 `split_proved: bool`），`routed_questions()` 对该态返回两条 RoutedQuestion、**question 都是整句 effective_question**（不拆字符串，也不再要求归属证明），route 分别 Data/Knowledge。route.rs 拿到后照常两路并行，收据打 `hybrid:unsplit`。这正是业主 2026-08-11 那条裁决「意图不很明确时问数与知识库一起查，综合输出」的 typed 落法（今天它只活在死代码 triage::unclear_both_hit 里）。绝不为此再调一次 LLM 拆句：整句喂 KB 检索本来就合法（多余词只稀释召回），整句喂 Data 与今天任何单路问句同形。
- 证据：crates/agent/src/intent.rs:412; crates/agent/src/intent.rs:436; crates/agent/src/intent.rs:801; crates/agent/src/intent.rs:1121; crates/agent/src/intent.rs:1266; crates/agent/src/triage.rs:250
- confidence=high known=False

### [high/M] typed subgoal 归属证不出时今天一律澄清；正确答案是「双路整句 + 降级收据」，澄清只留给零槽位

- **参考系统怎么做**：AGENT-ARCHITECTURE §3.2「多实体归属、省略主语或父子槽位冲突无法可靠证明时直接澄清，两路执行次数均为 0」——这条纪律对 Data 单路成立（错谓词比不答更坏），但对「问数 + 资料」两路是过度收紧：资料半根本不吃谓词。
- **本仓现状**：归属证不出的三条路径全部收敛到同一张卡：`subgoal_slots_grounded` 不过 → ground() 返 None（intent.rs:735）；`project()` push「未找到匹配的结构化子任务」/「结构化子任务归属不唯一」(intent.rs:625/:629) → 再被 :801 吞成 Invalid；`hybrid_pair` 非 1:1 → `hybrid_cardinality_clarification`(main.rs:2219)。三条都不告诉用户「哪一半没证出来」。
- **改法**：在 route.rs 里按半边分级，不再一票否决：①Data 半归属证不出 → 该半 fail-closed（保留今天纪律，谓词错比不答坏），Knowledge 半照跑，容器 caliber_note 点名「数据那半没能确定条件归属」；②Knowledge 半证不出 → 直接用整句检索（无谓词风险），收据打 `hybrid:kb-unsplit`；③两半都证不出 → 才是澄清，且澄清文案必须携带 `ambiguities`/归属失败原因（见本表「ambiguities 被吞」一条）。删掉 hybrid_cardinality_clarification：N 个 Data 子问走现有 `AskResult::compound` 容器（ask.rs:395 已在做），M 个 Knowledge 子问把问句用「；」拼成一次检索（KB 答案本就是多主题文本 + 引用），于是 N:M 不再存在「wire 承载不了」。
- 证据：crates/agent/src/intent.rs:625; crates/agent/src/intent.rs:629; crates/agent/src/intent.rs:801; crates/server/src/main.rs:2200; crates/server/src/main.rs:2219; crates/agent/src/ask.rs:395
- confidence=high known=False

### [high/S] server 六处分发里的两道「闸」是可证明的恒真/恒假死判据——修法是删，不是给它补等价测试

- **参考系统怎么做**：—
- **本仓现状**：`intent_contract_ready`(main.rs:2490) = `ready().is_some_and(|i| i.ambiguities.is_empty()) && route()!=Unknown`。但 ResolvedIntent 只能由 `ground()` 构造，而 ground 在 intent.rs:801 已强制「ambiguities 空 且 route≠Unknown」；`ResolvedIntent::project`(intent.rs:253) 也再 ground 一次并校验 route 相等。故该函数恒等于 `is_ready()`，也恒等于 xcx/mcp 那句 `match route() { Unknown => clarify }`。第二道 `route==IntentRoute::Data && !intent_attempt.is_data_executable()`（main.rs:2540、main.rs:2630、deep_api.rs:4543）里 `is_data_executable()` 的定义就是 `route()==Data`（intent.rs:1231），整个条件**恒假**。
- **改法**：不做 OPTIMIZATION-PLAN W5#7 那种「抽 dispatch + 写等价测试」——那是给两段死代码建档。直接：①删 `intent_contract_ready`/`prepared_contract_ready` 及六处调用；②删三处 `route==Data && !is_data_executable()` 早返块；③删 `IntentAttempt::is_data_executable`（全仓仅这三处 + ctx.rs:369 的 `intent_unverified` 一处消费，后者改用 `is_ready()` 语义更直白）。分发本身随「断点①」的 route::run 一起消失，六处只剩输出形态。净删约 60 行，且把「等价靠巧合」变成「等价靠类型」。
- 证据：crates/server/src/main.rs:2490; crates/server/src/main.rs:2540; crates/server/src/main.rs:2630; crates/server/src/deep_api.rs:4543; crates/agent/src/intent.rs:1231; crates/agent/src/intent.rs:801
- confidence=high known=True

### [high/S] 混合查询单路失败对用户完全不可见：只有 warn，payload 静默换形——与 compound 自己立的 missing_note 纪律直接冲突

- **参考系统怎么做**：仓内既有纪律（compound.rs:115 `missing_note` 的文档）：「只 filter_map(ok) 丢掉的后果是用户问了 3 件事、看到 2 个面板，而他既不知道少了一件、也不知道少的是哪一件」，且措辞必须说清「不是 0、不是没有数据」。
- **本仓现状**：crates/server/src/main.rs:2172 `(Ok(r), Err(e))` → `tracing::warn!("混合查询知识库路失败 → 退化纯问数")`，payload 就是纯 AskResult；:2179 `(Err(_), Ok(a))` → payload 整个换成 Answer（连 sql/trust 都没了）。两条路径都没有任何用户可见标注，前端 App.vue:3172 只按 `result.kb` 是否存在决定要不要画资料面板——半个答案长得和完整答案一模一样。
- **改法**：route.rs 合成时复用既有通道，不加新字段：任一半失败 → `r.caliber_note = Some(...)`，文案照 compound.rs:115 的口径（点名失败的是数据半还是资料半，并明说「不是查到 0 条」）。数据半失败时仍返回 AskResult 容器（sql 空、rows 空、`kb=Some(answer)`），而不是像今天那样把顶层换成 Answer——`caliber_note` 前端 App.vue / ResultPanel.vue 两处已按 ⚠️ 渲染，落点现成。同刀让 `hybrid_summary`(compound.rs:223) 在只有一半素材时不生成综合（今天两半都在才调，失败路径根本不调，容易被误读为「综合缺席=没结论」）。
- 证据：crates/server/src/main.rs:2172; crates/server/src/main.rs:2179; crates/agent/src/compound.rs:115; crates/agent/src/compound.rs:223; web/src/App.vue:3172
- confidence=high known=False

### [high/M] 混合收据在 attach_trust 之后才算 → 资料侧零引用照样顶着数据半算出来的 verified 徽标

- **参考系统怎么做**：ARCHITECTURE §9/§10「SQL 执行成功不等于答案可信」「验证失败仍可展示结果，但收据必须 blocked/review，不得标记 verified」。
- **本仓现状**：`attach_trust`(crates/agent/src/ctx.rs:356) 在数据半命中出口就算完了 trust，其 `receipt_blocked` 只读数据半自己的 `intent_summary`(ctx.rs:369-372)。回到 server 后 `hybrid_payload` 在 main.rs:2187 才用 `hybrid_intent_summary`(main.rs:2426) **覆盖** `payload["intent_summary"]`，把 `hybrid:knowledge:no-citation`/`hybrid:knowledge-failed` 写进 coverage.issues——但 `payload["trust"]` 一个字都没动。于是「资料半零引用」这件事只出现在收据 JSON 里，徽标仍是 verified。这与 W2#7（语义召回降级不进 trust）是同一个病灶的第二个发作点。
- **改法**：①把 `hybrid_intent_summary` 从 main.rs:2426 搬进 agent（route.rs 或 intent.rs，作为 `IntentSummary::merge_hybrid(data: Option<&IntentSummary>, kb_cited: bool) -> IntentSummary`），server 侧 `knowledge_summary_value`/`hybrid_summary_value`/`knowledge_receipt_value` 三个手工拼 JSON 的函数一起删；②在 ctx.rs 加一个 10 行的纯函数 `pub(crate) fn downgrade_trust(r: &mut AskResult, check: &'static str)`：把 `trust.level` 压到 "review" 并往 `checks` 追一行；route.rs 在 kb 半缺席/零引用/数据半 coverage 非 complete 时各调一次。该函数正好是 OPTIMIZATION-PLAN W2#7 需要的第二个调用点（召回降级），先有两个使用者再存在，不违 D7 精神；③硬线：混合容器**永远不给 verified**，最高 high——容器里含模型合成的 `view.insight` 文本。
- 证据：crates/agent/src/ctx.rs:356; crates/agent/src/ctx.rs:369; crates/server/src/main.rs:2187; crates/server/src/main.rs:2426; crates/server/src/main.rs:2475; docs/ARCHITECTURE.md §9「验证失败…收据必须 blocked/review」
- confidence=high known=False

### [high/M] 回归题集 79 题零 Knowledge、零 Hybrid，且判官走的 CLI 在结构上跑不了这两路

- **参考系统怎么做**：—
- **本仓现状**：tools/regression_cases.json 79 题 route 分布：direct-agg 38 / 未断言 15 / direct-doc 11 / graph 5 / entity-card 4 / need-intent 3 / compound 2 / business-lookup 1，全文无 kb/citation 类断言；tools/regression.py:264 `cli("ask", ...)` 走 crates/server/src/main.rs:1151 的 CLI 子命令 → `dms_agent::ask` → ask.rs:216 的早返。即使补题，今天也只能全部断言成反问卡。多轮题集 tools/regression_cases_multiturn.json 仅 3 题，同样全 Data。
- **改法**：依赖「断点①」收口后 CLI 自动获得两路能力（AskDeps 的 kb 字段非 Option，CLI 在 main.rs:1152 已有 owned、cfg 已有 kb_rrf_weights），随后：①regression_cases.json 加两个字段 `kb_contains`（在 `kb.markdown` 里找子串）与 `kb_min_citations`（`kb.citations` 条数下限），check() 里各 5 行；②沿用既有跳过机制（`requires_embed`/`requires_graph` 那套，regression.py:246 `missing_deps`）加 `requires_kb`，素材复用现成的 tools/kb_fixtures + kb_eval.py 的上传流程；③新增 8 题：K01-K03 纯知识（含一题零引用必须 coverage=blocked）、X01「…420g 的信息 和 拆单标准」（typed 拆开）、X02 同题但构造 subgoals 缺失（断言 `hybrid:unsplit` 且两路都出结果，不出反问卡）、X03 2Data+1KB（断言不再是澄清卡）、X04 KB 侧不可达（断言 caliber_note 点名 + trust=review）、X05 混合轮后追问「上月呢」（断言继承到数据半 SQL）。④regression.py 的 selfcheck 里补一条源码级断言：`crates/agent/src/ask.rs` 生产段不再出现 `route() != IntentRoute::Data` 早返——防止收口被回退。
- 证据：tools/regression_cases.json; tools/regression.py:253; tools/regression.py:264; crates/server/src/main.rs:1151; crates/agent/src/ask.rs:216; tools/kb_eval.py:96
- confidence=high known=True

### [medium/M] 深度模式撞上 Hybrid 直接退化成普通回答；深度子问命中 Knowledge 则 section 静默缺席

- **参考系统怎么做**：—
- **本仓现状**：crates/server/src/deep_api.rs:4587 `if route == IntentRoute::Hybrid { … crate::hybrid_payload(...); note(Done); return }`——用户点了「深度」，拿回来的是一份普通混合 payload，没有任何一段报告，也没有一句说明。另一处：deep_api.rs:2180 `sub_ask` 走 `crate::ask`（同样是 ask.rs:216 那条早返），子问被判 Knowledge 时返回 row_count=0 的澄清卡 → deep_api.rs:2192 `Ok(_) => None` → 该 section 静默消失（`Err` 分支至少有 warn，`Ok(空)` 连 warn 都没有）。
- **改法**：①收口后 deep_api 的 Hybrid 分支删掉：深度报告的主查询走 route::run，拿到 `AskResult{kb: Some(..)}` 后，把 kb 的正文与引用作为**报告的资料章节素材**（deep 报告已有 section 装配骨架），而不是把整份深度请求降级；若本轮确实无法出报告，必须给一句显式说明而不是静默换形。②`sub_ask` 的 `Ok(_) => None` 改成与 `Err` 同待遇：warn + 该 section 记「素材缺席」，理由与 compound.rs:115 同源——一个缺席的章节最容易被读成「那一项是零」。
- 证据：crates/server/src/deep_api.rs:4587; crates/server/src/deep_api.rs:2180; crates/server/src/deep_api.rs:2192; crates/agent/src/ask.rs:216; crates/agent/src/compound.rs:115
- confidence=high known=False

### [medium/S] 混合答案的 analysis_receipt 绑定整句问题，素材却只有数据半——AI 解读被要求回答它看不到的那一半

- **参考系统怎么做**：ARCHITECTURE §10：`/api/analysis` 不再信任客户端回传的行/比较/补充表，对问数响应签发与登录人绑定的 HMAC 收据。
- **本仓现状**：crates/server/src/main.rs:2192 `insight_api::attach_analysis_receipt(&mut payload, h.question, h.p)`，`h.question` 是**整句混合原问**（main.rs:2131 字段注释自认「喂给两路的是 kb_q/data_q 两半」）；而 `AnalysisMaterial::from_ask_payload`(insight_api.rs:181) 只收 columns/rows/comparisons/subs，`kb` 键不在素材里。于是用户点「深度解读」时，模型拿到的是 (整句混合问题, 仅数据半的表)——正是 answer_contract 那套 verified-fact 要防的「拿不到事实还得下结论」的形状。HMAC 没被破（素材侧仍被签），但问题↔素材配对是错的。
- **改法**：一行改：main.rs:2192（收口后是 route.rs 的合成出口）把绑定的问题换成数据半的生效问句（`data_prepared.question.effective_question`）。如果要让解读也覆盖资料半，正确做法是把 kb 正文作为一条 `AnswerContract::push_text("KB", ...)` 素材一起进 material 并纳入 HMAC，而不是只把问题写大——但那是第二步，先把配对改对。
- 证据：crates/server/src/main.rs:2192; crates/server/src/main.rs:2131; crates/server/src/insight_api.rs:181; crates/server/src/insight_api.rs:397; docs/ARCHITECTURE.md §10
- confidence=high known=False

### [medium/M] 模型自报的 ambiguities 与归属失败原因被 ground() 整份吞掉，用户只看到「未通过一致性校验」

- **参考系统怎么做**：提示词第 4 条明写「拿不准写入 ambiguities，不得猜」（intent.rs:22），AGENT-ARCHITECTURE §3.3 要求「零命中、多命中或访问受限都必须进入歧义列表」。
- **本仓现状**：crates/agent/src/intent.rs:801 `if self.route()==Unknown || !self.ambiguities.is_empty() { return None }` —— 非空 ambiguities 让整份合同变 `IntentAttempt::Invalid`，`user_note()`(intent.rs:1266) 给的是通用文案「意图解析结果未通过一致性校验」，歧义原文一个字都到不了用户。`project()` 自己 push 的三条诊断（intent.rs:625「未找到匹配的结构化子任务」/:629「结构化子任务归属不唯一」/:725「复合子问未保留父级范围槽位」）同样被这条 return 吞掉。连带 `coverage_with_evidence` 里 `for ambiguity in &intent.ambiguities { conflicts.push(...) }`(intent.rs:1469) 结构性不可达——ResolvedIntent 永远 ambiguities 为空。混合问题正是最容易触发歧义的形态（两半各有主语），于是「反问卡说不清为什么反问」在混合上最密集。
- **改法**：给 `IntentAttempt` 加第四态 `Ambiguous{ intent: ResolvedIntent, reasons: Vec<String> }`（intent.rs:1187 的 enum，今天三态）：ground() 在 route 可解但 ambiguities 非空时进这一态而不是 Invalid；`user_note()` 与 `clarification_result()`(ask.rs:148) 把 reasons 逐条渲进澄清卡与 `clarify_options`（候选生成 `contract_candidates`(ask.rs:590) 已有落点）；`summary()`(intent.rs:1272) 把它们写进 coverage.issues（前缀 `ambiguity:`，与 :1469 同前缀，那段死码随之复活）。执行侧行为不变——仍然不执行，只是这张卡从此说得出「哪一句不确定」。
- 证据：crates/agent/src/intent.rs:801; crates/agent/src/intent.rs:1266; crates/agent/src/intent.rs:1469; crates/agent/src/intent.rs:625; crates/agent/src/intent.rs:629; crates/agent/src/ask.rs:148
- confidence=high known=False

### [medium/S] 三段混合相关死代码（triage::hybrid_clauses / unclear_both_hit / compound::try_compound）零生产调用；但删之前 unclear_both_hit 承载的业主裁决必须先落到 typed 层

- **参考系统怎么做**：—
- **本仓现状**：`crates/agent/src/triage.rs:210 hybrid_clauses`、`:250 unclear_both_hit`、`crates/agent/src/compound.rs:64 try_compound`（连同 `split_questions`/`is_compound`）全仓零生产调用点（grep 只命中自身单测与注释）。混合拆分现由 typed subgoal 承担（ask.rs 文件头第 16 行「Hybrid 不自由拆字符串」），复合拆分由 ask.rs:395 `routed.len()>1` 承担。
- **改法**：删，但有顺序：`unclear_both_hit` 的文档（triage.rs:238-249）记录的是业主 2026-08-11 的裁决「意图不很明确时问数与知识库一起查，综合输出」——那条裁决今天在生产链路上**没有任何落点**（正是断点②的用户体感）。所以先落本表第 2 条的 `HybridUnsplit` 态，再同刀删这三段（triage.rs 686→约 200 行、compound.rs 576→约 90 行只留 hybrid_summary，或整体并入 insight.rs）。删除清单与 OPTIMIZATION-PLAN W6#5 完全重合，此处只补一条前置条件：**不许先删后补**，否则那条裁决会连注释一起消失。
- 证据：crates/agent/src/triage.rs:210; crates/agent/src/triage.rs:238; crates/agent/src/triage.rs:250; crates/agent/src/compound.rs:64; crates/agent/src/ask.rs:16; crates/agent/src/ask.rs:395
- confidence=high known=True

### [medium/M] 纯 Knowledge 轮不写 query_log、混合轮只写数据半 → 路由质量本身没有台账，`coverage.failed 按 route 分布` 这个指标永远算不出来

- **参考系统怎么做**：AGENT-ARCHITECTURE §8 要求的可观测指标里，第 4 条就是「`coverage.failed` 按 route、模型和工具分布」。
- **本仓现状**：crates/server/src/main.rs:2280 的注释白纸黑字：「纯 Knowledge/澄清没有 Data query_log，保持现有落账口径」。`query_log::finish` 只在 `ask`(main.rs:2763) 与 `ask_prepared`(main.rs:2819) 两处调用，两处都只服务 Data 执行；混合轮由 `ask_data_run` 触发，写进去的 route 是数据半的（direct-agg 等），wire 上根本看不出这是一次混合。于是「混合问题占比多少、哪一半更常失败、unsplit 降级发生了几次」全部无法从 PG 里查出来，只能翻 tracing。
- **改法**：收口后 route::run 成为唯一出口，`query_log::finish` 顺势提到它外层（server 的两个 wrapper 合并成一个）：route 列写容器的 route（新增合法值 `"hybrid"`、`"knowledge"`，同步 `answerers/mod.rs:ROUTE_LABELS` 与 main.rs:3657 那条源码扫描断言），已有的 trust/coverage 列直接承载合并后的收据。不新建表、不加端点——`meta.query_log` 与 trace_api::conv_trace 已经在读这几列，改的只是「有没有写」。
- 证据：crates/server/src/main.rs:2280; crates/server/src/main.rs:2763; crates/server/src/main.rs:2819; crates/agent/src/answerers/mod.rs:ROUTE_LABELS; docs/AGENT-ARCHITECTURE.md §8
- confidence=medium known=False


## 设计：self-learning

## 目标形态：不造学习引擎，给既有七张表补三样缺件

本仓已有的学习面（`meta.sql_exemplar` VQR 三态 / `meta.pitfall` 候选→active / `meta.memory` 蒸馏+向量召回 / `meta.correction_log` / `meta.failure_log` / `meta.query_log` / `meta.query_feedback`）在「学什么、存哪」上已经铺完，`review.rs` 的三段判词 + `admin_api.rs:362 validate_exemplar`（真实只读执行验证）比 prime-agent 的两段闸更严。真正缺的只有三样：**作用域**（谁的经验）、**账本**（学了什么、怎么撤）、**频次**（值不值得学）；外加一处已经在跑但没有门的自动回路。全部落在既有链路上，零新增依赖、零新编排器。

### 数据结构

**① 作用域：一列 `owner`，三级阶梯。** `meta.memory` / `meta.pitfall` / `meta.sql_exemplar` 各加 `owner text NOT NULL DEFAULT ''`。编码：`''`=public、`u:<login_name>`=private、`d:<department_id>`=team（`Principal.department_id` 在 `crates/policy/src/principal.rs:14`，`AskCtx.p` 一直在手边，`crates/agent/src/ctx.rs:29`）。一列表达 private→team→public，晋升＝一条 UPDATE 换前缀。老行默认 `''`＝今天的全员共享，行为逐字不变、零回填。索引 `(ds_id, owner)`。

召回谓词与 `DS_PRED` 走**同一个拼接点**（`crates/semantic/src/registry/mod.rs:25` 的 `DS_PRED` + `:34 expand_pred`，`tests/drift.rs:56` 已经在守它）：新增 `OWNER_PRED = " AND $Q.owner IN ('', $U, $D)"` 与 `owner_pred_at(alias, n)`。这条同时**兑现** `docs/ARCHITECTURE.md:63` 那句「few-shot/教训召回带 visibility 谓词」——全仓 grep `VIS_PRED` 零命中，它今天是文档幻影（`docs/OPTIMIZATION-PLAN-2026-08-13.md:651④` 已立项把它改写成「已放弃」，known=true）。本设计把结论从「放弃」改成「用单列 owner 兑现」：不做 `visibility`+`owner_login` 两列的原方案。

**② 账本：`meta.learn_event`（新表之一）。** 列：`id / at / batch_id / actor / target_table / target_id / action / before jsonb / after jsonb / evidence / expected_case / trace_id`，索引 `(batch_id, id)` 与 `(at DESC)`；`ds:any` 行内标记（跨源账本，按 `drift.rs` 既有豁免约定显式写）。`before` 为 NULL ⇒ 回滚即 DELETE，否则按 `target_table` 白名单 UPDATE 回去（白名单是三个 `&'static str` 常量，满足 `drift.rs:183 sql_interpolation_is_allowlisted`）。今天四个学习写口（`memory.rs:38 save_memory`、`exemplar.rs:200 save_with_context`、`:346 save_lesson_candidate`、`:387 set_lesson_status`）全是裸写、无前值、无批次——「上周二学了什么、哪条把 E05 带红了、怎么撤这一批」三个问题今天只能连 psql 逐表比时间戳。

**③ 用户习惯：`meta.user_pref`（新表之二，纯计数、零 LLM、零向量）。** `PRIMARY KEY(login_name, ds_id, key, value)` + `hit_count int` + `updated_at`。`key` 固定四值 `grain|breakdown|detail|region`，`value` 直接取 `IntentV1` 已解析的表面槽位（`crates/agent/src/intent.rs:190 TimeSlot.grain`、`:210 breakdowns`、`:212 requested_detail`、`:208 regions`）——**不落任何数据库值、不落 canonical ID**，与 `AskResult.intent_summary` 已确立的透出边界逐字同口径（`docs/AGENT-ARCHITECTURE.md §3.2`）。今天这些槽位一轮问完就丢，`meta.query_log` 有 `login_name`（`crates/server/src/query_log.rs:43`）却没人反向读。

### 控制流

**写侧（一次问答之后）**，五个点，四个是既有写口只多一行记账：

- `run.rs:863` `worth_learning` → `save_exemplar`，owner 写 `u:<login>`；
- `run.rs:873` 经验蒸馏，判据从 `st.route=="llm+repair" && !rs.rows.is_empty()` 换成 `st.route=="llm+repair" && worth_learning(st,&rs)`——**同一个函数里两条沉淀路两种诚实度**：语料路已由 `worth_learning`（`run.rs:1040`，`st.note.is_some()` 即否决）挡住口径复核未过的 SQL，经验路既不看 `st.note` 也不看覆盖闸收据，于是挂了 `caliber_note` 的 SQL 照样落 `meta.memory` 并被 `gather.rs:167/478` 召回进**每一个**用户的 prompt；
- `run.rs:918` 失败复盘前先查 `failure_streak`，`<2` 只落日志不调模型；
- `review.rs:101/…` 状态变更带 `before`；
- `ctx.rs:356 attach_trust` 尾部 fire-and-forget `bump_pref`（形态与 `gather.rs:288 spawn_bump_hits` 同款）。

**硬线：自动蒸馏一律只写 private（`u:<login>`）。** 晋升到 team/public 只有两条通道：admin 页复核（`admin_api.rs:345/362` 已有）或 CLI。这一条同时收紧今天的 I4 违背面——`run.rs:884` 的 content 模板是 `问「{q}」：…正确写法：{fixed}`，别人的问句原文 + 完整 SQL 逐字进同源全体用户的 prompt（`gather.rs:180`）。同刀把 content 的 `问「{q}」：` 前缀删掉（`question` 已是独立列，只用于去重，`memory.rs:44`；召回排序靠向量，prompt 里不需要它）。

**读侧（下一次问答）**，不加新召回波，只在既有三处各多一个绑定：`gather.rs:167`（波2，与 `recall::retrieve` 同一 `join!`）、`gather.rs:478`（回炉路）、`recall/pitfall.rs:32 pitfalls_sql()`、`registry/exemplar.rs:18/319/171` 三条召回各加 `OWNER_PRED`。`user_pref` 挂波1（与 `fewshot_block` 同一 `join!`，一条 SQL、无向量、无 LLM）。

**排序融合**：`memory.rs:96 score()` 加一个私有档位——`owner` 非空（本人/本部门）时 ×1.3。纯函数，判据打这里。理由：个人经验更贴，但 public 那批是过人工复核的，不能被压死。

**注入预算**：`user_pref` 渲成 `prompt.rs` 新段 `T_USER_HABITS = "\n## 本用户常用口径（参考，不是硬约束）\n"`，**最多 3 条、每条 ≤32 字、总 ≤120 字**（对照：`MEMORY_LIMIT=3` × 400 字，pitfall 5 条）。段位在 `T_MEMORIES` 之后、`fewshot` 之前——权重序即段序（`prompt.rs:112` 的既有纪律），习惯是最弱一档。

### 不变量

- **I4**：召回一律 `ds_pred + owner_pred` 两道。`drift.rs` 加第二条守卫 `every_learning_recall_is_owner_scoped`（形状照 `:56 every_meta_recall_is_ds_scoped`：`FROM meta.{memory,pitfall,sql_exemplar}` 那行 8 行内必须出现 `owner` 或 `{owner_pred}`）。它顺带把 `OPTIMIZATION-PLAN W7④` 要求的「exemplar 每处读 SELECT 必须含 `status='enabled' AND validation_status='valid'`」一起钉住。
- **I5**：三个段标题全带「参考，不是硬约束」；`user_pref.value` 与 memory/pitfall 的文本进 prompt 前一律过 `dms_semantic::ingest::sanitize_comment`（`crates/semantic/src/ingest/mod.rs:42`，F4 的既有清洗器：剥 `<`/`>`/`##`/`【⚠️`/控制字符、截 120 字）——**不写第二份清洗**。学到的东西**绝不进口径判据与闸门**（`memory.rs` 文件头的红线不放宽）。
- **fail-closed**：①`worth_learning` 成为两条蒸馏路的唯一判据；②`failure_streak < 2` 的一次性抖动不学、也不烧那次 fast 复盘；③`user_pref` 读侧门槛 `hit_count >= 3` 且 `updated_at > now()-90d`（证据不足就不用）；④`fewshot` 加 `word_similarity >= FEWSHOT_FLOOR`（今天无下限：语料库非空就恒出「相似问题的正确写法（参考口径）」标题，哪怕相似度≈0，与兄弟读路 `cache.rs:22 MAX_DIST=0.12` 三关护栏严重不对称）；⑤一键回滚 `POST /api/admin/learn/{batch_id}/rollback`。
- **不重复造**：不加 status 列给 `meta.memory`（private 层污染面只有本人，加复核门等于给自己的经验设审批）；public 层的门就是既有人工复核。

### 进化闭环

```
query_log / failure_log / query_feedback
  ├ failure_streak >= 2                 ┐
  ├ 用户 👎 (kind ∈ caliber|data)        ├→ 候选（owner='u:x'，status=candidate/pending）
  └ worth_learning 通过的成功轮          ┘
       └ fast LLM 复核（review.rs 现成）→ active/enabled（仍 private，本人立刻受益）
            └ admin 页/CLI 人工 → owner 改 'd:<dept>'（team）或 ''（public）
                 └ regression_cases_learned.json 净转红 → rollback batch_id
```
负反馈接线：`quality_api.rs:92` 的 INSERT `RETURNING id` 扩成 `RETURNING id, question, ds_id`，`kind ∈ {caliber, data}` 时调一次已有的 `exemplar::set_status(pg, ds, question, "disabled")`（`exemplar.rs:258`，已传播错误、已有 0 行 warn）。语义是「用户说这个数字/口径错了 → 这条语料立刻停止当范例」，只做减法，误伤代价是少一条 few-shot（admin 可重新验证启用）。今天 `meta.query_feedback` 的唯一消费者是 admin 质量页列表（`quality_api.rs:155`），不写回任何学习表；前端唯一调用点是 `web/src/KbAnswer.vue:139`，问数面板与小程序零入口。

**失效与衰减**：memory 的 `exp(-age/30d)` 已在跑；`user_pref` 靠 90 天读窗；exemplar 靠既有 `EX_STALE_SOURCE_SQL`（`admin_api.rs:296`）与指标版本变更置 stale。不加 TTL 清理任务——几千行量级，清理是运维一条 DELETE。

### 怎么证明变好了

学习增益**不看总分**：`tools/evaluation.py` 头部自己写着「LLM 路径抖动池 ≥9/38 ≈24%，单轮 38 题分辨不出 ±2」。判据改成一份专属题集 `tools/regression_cases_learned.json`，`meta.learn_event.expected_case` 存题名，验收就是现成一行 `python tools/regression.py --cases tools/regression_cases_learned.json`（`--cases` 已支持，`tools/regression.py:65`，零改脚本）。影子对照＝学习开关：`AskDeps` 加 `pub learn: bool`（Default true），`eval-batch`（`main.rs:506`）与一次性 `ask` 两个 CLI 分支传 false——这同时修掉今天「判官每跑一趟 79/237 轮全部灌进三张学习表、pending 队列被判官题淹没、memory 那路根本不经复核就生效」的语料污染。AB 分桶不做：单实例 + I4 要求 key 带 login，分桶即噪声。

### 明确不做
prime-agent 的 RLM / 持久 IPython / `rlm()` 子 agent / daemon / autonomous（程序化执行面与 I1/I5 结构性冲突，Python 运行时是 D6 新依赖，其自身文档承认非安全沙箱）；harness 的「把概览塞进系统提示词」召回方式（本仓 pgvector 近邻 + hit/recency 重排更优，条数不随知识总量膨胀）；自动向 public 层写入（企业 BI 里等于跨用户口径投毒）。这三条落 `docs/ARCHITECTURE.md §8` 一行，防下一轮重复立项。

### 步骤

1. **经验蒸馏补上 worth_learning 闸（两条沉淀路统一判据）**（S；依赖：—）
   - 文件：crates/agent/src/run.rs
   - 改法：run.rs:873 的 `if st.route == "llm+repair" && !rs.rows.is_empty()` 改成 `if st.route == "llm+repair" && worth_learning(st, &rs)`（`worth_learning` 已含空结果判据，run.rs:1040-1047，`!rs.rows.is_empty()` 是它的子集，直接删）。同刀把 run.rs:884 的 content 模板去掉 `问「{q}」：` 前缀——question 已是独立列（memory.rs:44 只用于去重），召回排序靠向量，prompt 里不需要别人的问句原文。根因修法而非调用点补丁：从此两条沉淀路共用同一个判据函数，`st.note`（口径复核未过/绕开合同）一次否决两条路。
   - 验收：run.rs 的 mod tests 照 run.rs:1987 `note_before_learn` 的模子加一条源码守卫：`st.route == "llm+repair"` 那行 8 行内必须出现 `worth_learning`；扩 `worth_learning_rejects_uncertain_and_empty_aggregates` 断言 note 非空时两条路都不落。手工：造一条挂 caliber_note 的 llm+repair 轮，`SELECT count(*) FROM meta.memory` 不增。

2. **owner 三级作用域列 + OWNER_PRED 单一拼接点 + 六条召回加谓词**（M；依赖：步骤 1）
   - 文件：crates/semantic/src/ddl.rs, crates/semantic/src/registry/mod.rs, crates/semantic/src/registry/memory.rs, crates/semantic/src/registry/exemplar.rs, crates/semantic/src/recall/pitfall.rs, crates/agent/src/gather.rs, crates/agent/src/run.rs, crates/semantic/tests/drift.rs
   - 改法：①ddl.rs 三条幂等 ALTER（形态照 ddl.rs:408-409）：`meta.memory` / `meta.pitfall` / `meta.sql_exemplar` 各加 `owner text NOT NULL DEFAULT ''` + `CREATE INDEX IF NOT EXISTS idx_<t>_owner ON meta.<t>(ds_id, owner)`。编码 `''`=public / `u:<login_name>`=private / `d:<department_id>`=team。②registry/mod.rs（ds_pred 的家）加 `pub const OWNER_PRED: &str = " AND $Q.owner IN ('', $U, $D)"` 与 `pub fn owner_pred_at(alias:&str, n:usize)->String`，复用既有 `expand_pred`（:34），不开第二份 replace。③新增 `pub fn owner_keys(p:&Principal)->(String,String)` 放 agent 侧（policy 类型不能进 semantic），返回 `("u:{login}", "d:{dept}")`，dept 为 None 时第二位给 `"d:"`（永不匹配）。④`recall_memories` 签名加 `owner:(&str,&str)`，谓词 `WHERE ds_id=$2 AND owner IN ('',$3,$4) AND embedding IS NOT NULL`；`memory.rs:96 score()` 加档位：owner 非空 ×1.3（纯函数，判据打这里）。⑤`exemplar.rs:18 fewshot` / `:171 suggest_questions` / `:319 nearest` 与 `recall/pitfall.rs:32 pitfalls_sql()` 各拼 `owner_pred_at`。⑥写侧：run.rs 的 save_exemplar / save_memory / save_lesson_candidate 三处一律传 `u:<cx.p.login_name>`（自动蒸馏只进 private 层）。known=true，出处 docs/OPTIMIZATION-PLAN-2026-08-13.md:651④——但该条的结论是「把 VIS_PRED 方案改写成已放弃 + 加 drift 守卫」，本步把结论改成「以单列 owner 真的兑现」，两列 visibility+owner_login 的原方案仍不做；docs/ARCHITECTURE.md:63/96/181/184/210/333 六处描述随之改成事实。
   - 验收：drift.rs 加 `every_learning_recall_is_owner_scoped`（形状照 :56 `every_meta_recall_is_ds_scoped`：`FROM meta.memory|meta.pitfall|meta.sql_exemplar` 那行往后 8 行内必须出现 `owner` 或 `{owner_pred}`），同一条测试顺带钉 exemplar 三条读 SELECT 必须含 `status='enabled' AND validation_status='valid'`（W7④ 要求的守卫）。memory.rs 单测：同 sim 同 hit 同 age 时 owner 非空的排在前。手工：A 用户产生一条经验，B 用户同源同问句召回不到；`grep -rn VIS_PRED` 仍零命中且文档不再提它。

3. **meta.learn_event 账本 + registry/learn.rs + 四写口记账 + 两个 admin 端点**（M；依赖：步骤 2）
   - 文件：crates/semantic/src/ddl.rs, crates/semantic/src/registry/learn.rs, crates/semantic/src/registry/mod.rs, crates/agent/src/run.rs, crates/agent/src/review.rs, crates/server/src/admin_api.rs
   - 改法：①ddl.rs 建表 `meta.learn_event(id bigserial, at timestamptz default now(), batch_id text default '', actor text default '', target_table text, target_id bigint, action text, before jsonb, after jsonb, evidence text default '', expected_case text default '', trace_id text)` + `idx_learn_batch(batch_id,id)` + `idx_learn_at(at DESC)`，行内写 `ds:any` 标记（跨源账本，drift.rs 既有豁免约定）。②**新建** `crates/semantic/src/registry/learn.rs`（不塞 exemplar.rs：它非测试段已 433 行，逼近 D2 的 450），只放两个函数 `pub async fn log_event(pg, ev: &LearnEvent)` 与 `pub async fn rollback_batch(pg, batch_id) -> anyhow::Result<usize>`（按 id 倒序重放：before 为 NULL 则 DELETE，否则按 target_table 白名单 UPDATE 回去——白名单是三个 `&'static str` 常量，满足 drift.rs:183 `sql_interpolation_is_allowlisted`）。③四个写口各加一行 log_event：run.rs 的 save_exemplar / save_memory 两个 spawn（batch_id = trace_id，actor='auto'，evidence 带 route 与 note 状态）、review.rs 的 `set_lesson_status`/`set_status` 调用点（batch_id = 本批 uuid，actor='cli:review-lessons'，before 记旧 status）。④admin_api.rs 加 `GET /api/admin/learn?days=` 与 `POST /api/admin/learn/{batch_id}/rollback`，身份用 `resolve_identity_strict`（依赖 W5#10，known=true 出处 OPTIMIZATION-PLAN:413，绝不继承 insecure_login_fallback）。与 W5#8（correction_log 读面）相邻但不重复：那条是排障读面，本条是学习批次与回滚。
   - 验收：learn.rs 单测（纯函数部分）：rollback 的 SQL 生成对三张表各产出正确 UPDATE/DELETE，target_table 不在白名单时返 Err 而不是拼串。集成手测：跑一轮 llm+repair → `SELECT * FROM meta.learn_event WHERE batch_id=<trace_id>` 有两行且 before 为 NULL → POST rollback → `meta.memory`/`meta.sql_exemplar` 对应行消失、rollback 返回 2。drift.rs 的 `sql_interpolation_is_allowlisted` 必须仍绿。

4. **failure_streak 频次闸：只有重复出现的失败才配升格教训**（S；依赖：步骤 1）
   - 文件：crates/semantic/src/registry/failure.rs, crates/agent/src/run.rs, crates/agent/src/review.rs
   - 改法：`meta.failure_log` 今天是全仓唯一写口 `exemplar.rs:420 log_failure_traced` + **零 SELECT**（grep `FROM meta.failure_log` 零命中），ddl.rs:287-290 注释宣称的「同错累计→升格 pitfall」那个累计器不存在。①**新建** `crates/semantic/src/registry/failure.rs`（同样不进 exemplar.rs），一个函数 `pub async fn failure_streak(pg, ds, kind, err_head: &str, days: i32) -> i64`，SQL `SELECT count(*) FROM meta.failure_log WHERE ds_id=$1 AND kind=$2 AND left(error,60)=$3 AND created_at >= now()-$4::int*interval '1 day'`（failure_log 属日志表已豁免 ds 谓词，这里本来就带）。②run.rs:915-921 的 spawn 前先查一次：`streak < 2` → 只落日志不调 `review_failure`（本轮直接省掉大部分一次性抖动的 fast 调用）；`>= 2` → 调用，并把次数拼进 user 段（「已连续第 N 次」），`review.rs:21 FAILURE_SYSTEM` 加一句「重复出现的失败优先给出可复用教训」。③同刀修 run.rs:917 的 I4 漏洞：`let sql = scoped.wire().to_string()` 改 `st.candidate.clone()`——隔壁 run.rs:869-874 的经验蒸馏为同一理由（wire() 把行级权限条件写进 ds 级共享教训）已经用了 candidate，这一支没跟上；`log_failure_traced` 那两行保留 wire()（排障取证，不喂 LLM），在 exemplar.rs:420 的 doc 上标明「本表含注入后条件，任何送进 LLM/prompt 的读路必须先剥」。
   - 验收：failure.rs 单测钉 SQL 形状（含 ds/kind/left(error,60)/时间窗四条件）。run.rs 加源码守卫：`review_failure(` 所在 spawn 块内不许出现 `.wire()`（照 run.rs:1649 correction_log 十七 kind 的同款形态）。手工：同一条错误连报 1 次 → `meta.pitfall` 无新候选、日志有 fast 调用零次；报到第 2 次 → 候选出现。

5. **meta.user_pref + bump_pref + T_USER_HABITS 段（零 LLM 的用户习惯档案）**（M；依赖：步骤 3）
   - 文件：crates/semantic/src/ddl.rs, crates/semantic/src/registry/learn.rs, crates/agent/src/ctx.rs, crates/agent/src/gather.rs, crates/agent/src/prompt.rs
   - 改法：①ddl.rs 建 `meta.user_pref(login_name text, ds_id text, key text, value text, hit_count int default 0, updated_at timestamptz default now(), PRIMARY KEY(login_name, ds_id, key, value))`。②learn.rs 加 `pub async fn bump_pref(pg, login, ds, items: &[(&str,&str)])`（单条 `INSERT .. ON CONFLICT DO UPDATE SET hit_count = user_pref.hit_count + 1, updated_at = now()`）与 `pub async fn top_prefs(pg, login, ds) -> Vec<(String,String)>`（`WHERE hit_count >= 3 AND updated_at > now()-interval '90 days' ORDER BY hit_count DESC LIMIT 3`——证据不足就不用 + 时间衰减，不建清理任务）。③写点：`ctx.rs:356 attach_trust` 尾部加 `spawn_pref_bump(cx, r)`（≤15 行，fire-and-forget，形态照 gather.rs:288 `spawn_bump_hits`），仅在 `r.row_count > 0` 且 `cx.intent` 为 Some 时落，四个 key 取 `IntentV1` 的表面槽位：`grain`←`time.grain`（intent.rs:194）、`breakdown`←`breakdowns[0]`、`detail`←`requested_detail`、`region`←`regions[0]`（intent.rs:208-212）。**不落任何数据库值、不落 canonical ID**，与 `AskResult.intent_summary` 的透出边界同口径。④读点：gather.rs 波1（与 fewshot_block 同一 `join!`，一条 SQL、无向量、无 LLM），值进 prompt 前过 `dms_semantic::ingest::sanitize_comment`（semantic/ingest/mod.rs:42，F4 既有清洗器，不写第二份）。⑤prompt.rs 加 `const T_USER_HABITS: &str = "\n## 本用户常用口径（参考，不是硬约束）\n"` 与 `PromptCtx.habits: Vec<String>`，段位在 T_MEMORIES 之后、fewshot 之前（权重序即段序），最多 3 条、每条 ≤32 字。
   - 验收：prompt.rs 单测：habits 非空时段标题出现且在 `## 经验复盘` 之后 `## 相似问题` 之前；habits 为空时连标题都不出（照 gather.rs:829 `fewshot_text(&[])==""` 的既有纪律）。ctx.rs 单测：row_count=0 或 intent=None 时不产生 bump 项。learn.rs 单测：top_prefs 的 SQL 含 `hit_count >= 3` 与 90 天窗。手工：同一账号连问 3 次「按省区拆分」，第 4 次 prompt 里出现该习惯；换账号不出现。

6. **负反馈接回学习面 + 问数侧与小程序补反馈入口**（M；依赖：步骤 3）
   - 文件：crates/server/src/quality_api.rs, web/src/ResultPanel.vue, crates/server/src/xcx_api.rs
   - 改法：①quality_api.rs:92 的 INSERT `RETURNING id` 扩成 `RETURNING id, (SELECT q.question FROM meta.query_log q WHERE q.trace_id=$1 AND q.login_name=$2 ORDER BY q.id DESC LIMIT 1), ds_id`（question/ds_id 从同一条已定位的 query_log 行取），`kind ∈ {caliber, data}` 时调一次已有的 `dms_semantic::registry::exemplar::set_status(pg, ds, question, "disabled")`（exemplar.rs:258，已传播错误、已有 0 行 warn）。语义：用户说这个数字/口径错了 → 这条语料立刻停止当范例。**只做减法**，误伤代价是少一条 few-shot（admin 可重新验证启用），符合 fail-closed。同刀写一条 learn_event（actor=login，action='disable'，evidence='feedback:<kind>'）让它可回滚。②前端补入口：web/src/ResultPanel.vue 复用 web/src/KbAnswer.vue:114-145 那套两键 👍/👎 + localStorage 记忆的形状，POST 同一个 `/api/feedback`（trace_id 已在问数收据里）；xcx_api 同理（今天 grep `feedback` 零命中）。不新开端点。
   - 验收：quality_api.rs 单测：kind='correct' 不触发 set_status，kind='caliber'/'data' 触发且带 learn_event。手工：对一条已 enabled 的语料所在问答点 👎(口径错) → `SELECT status FROM meta.sql_exemplar WHERE question=…` 变 disabled → 下一轮同类问句 few-shot 段不再出现它 → rollback batch 后恢复。web 侧 vue-tsc 0 错 + 三端各点一次 👎 都能落库。

7. **AskDeps.learn 开关：评测/判官不再往学习表灌数据（影子对照的载体）**（M；依赖：步骤 1）
   - 文件：crates/agent/src/ask.rs, crates/agent/src/ctx.rs, crates/agent/src/run.rs, crates/server/src/main.rs
   - 改法：今天 `main.rs:506 eval_batch_one` 直接调生产 `ask(...)`，于是 regression.py 79 题 + evaluation.py `--runs 3` 一趟最多 237 轮全部写进 `meta.sql_exemplar`(pending) / `meta.memory`(无门、600s 后由 embed_fill 自动向量化随即进所有人 prompt) / failure_log / correction_log——判官题淹没人工复核队列，memory 那路根本不经复核就生效。改法落在入参而不是调用点：`AskDeps` 加 `pub learn: bool`（构造点默认 true），透传进 `AskCtx`；run.rs 在 `if worth_learning(st,&rs)`（:863）与经验蒸馏（:873）两个判据前各 `&& cx.learn`，失败复盘 spawn（:918）同理。调用侧只改两处：main.rs 的 `eval-batch` 与一次性 `ask` 两个 CLI 分支传 false，HTTP 端点不动。零新表、零新 trait（bool 不是 trait）。影子对照由此成立：同一题集 learn=true / learn=false 各跑一遍对比。
   - 验收：run.rs 加源码守卫：两个 spawn 与 save_exemplar 都在 `cx.learn` 之后（照既有 note_before_learn 形态）。手工：`python tools/regression.py` 全量跑完，`SELECT count(*) FROM meta.learn_event WHERE at > <开跑时刻>` 为 0、`meta.memory` 行数不变。

8. **复核调度：把候选真的送到人面前（今天 CLI 存在但全仓零调用点）**（S；依赖：步骤 3）
   - 文件：crates/server/src/embed_fill.rs, crates/server/src/main.rs, crates/server/src/quality_api.rs
   - 改法：`exemplar.rs:21/174/327` 三条召回硬要求 `status='enabled' AND validation_status='valid'`，而这个状态的唯一出口是 admin 网页上人点一次（admin_api.rs:288 EX_VALIDATE_OK_SQL）；`review-pending`/`review-lessons` 是纯 CLI 子命令（main.rs:754/764），全仓 *.sh/*.ps1/*.py/DEPLOY.md grep 零调用、无 cron/systemd timer。结果：没人点则 few-shot 段、语义缓存回放、冷启动推荐三条读路径恒空，且三处都是静默降级（gather.rs:737-739 空语料连标题都不出），线上看不出「学习面从来没启动过」。改法照抄仓内已有的后台循环形态、不引调度框架：embed_fill.rs（已有 `spawn` + `pg_try_advisory_lock` + 600s 循环，:22-41）加第二个 LOCK_KEY 的 `pub fn spawn_review(st: Arc<AppState>)`，每 3600s 依次调 `dms_agent::review::review_all_pending(&llm, pg, 100)` 与 `review_lessons(&llm, pg, 100)`（两函数签名已是 `(llm,pg,limit)->Result<usize>`，review.rs:12-13 明写不许变形），main.rs:1346 `embed_fill::spawn(state.clone())` 旁边接一行。**人工 VQR 那道门不动**（AI 仍只能 pending→disabled，不许自授 enabled）；自动化的只是「把候选送到人面前」。同刀在 admin 质量页 summary（quality_api.rs:174-181）加 pending/candidate 两个计数字段，让「队列没人处理」变成看得见的数字。
   - 验收：embed_fill.rs 单测/源码守卫：两个 spawn 的 LOCK_KEY 不相等。手工：起服务，观察 1 小时内 pending 语料的 ai_review 列被填、candidate 教训转 active/disabled；`GET /api/admin/quality` 返回体含 pending 计数且与直接查库一致。

9. **fewshot 相似度下限（今天语料库非空就恒出「正确写法」标题）**（S；依赖：步骤 2）
   - 文件：crates/semantic/src/registry/exemplar.rs, crates/agent/src/gather.rs
   - 改法：exemplar.rs:19-38 的 fewshot 是 `ORDER BY word_similarity($1, question) DESC LIMIT 8` 之后 `.take(2)`，**没有任何相似度门槛**；渲染侧 gather.rs:738 `fewshot_text` 只判 rows 是否为空，非空就出「## 相似问题的正确写法（参考口径）」标题——一条完全不相干的历史语料被冠以「正确写法」送进 precise 模型。与兄弟读路严重不对称：语义缓存有 `cache.rs:22 MAX_DIST=0.12` + 时间词/数字词全等三关。改法：加 `const FEWSHOT_FLOOR: f32`，SQL 加 `AND word_similarity($1, question) >= $N`（绑定下标连带 ds_pred/owner_pred 一起改）。**阈值先量后定**——用一次性 SQL 量一遍现网 word_similarity 分布再拍，代码里写 `// ponytail: floor 按 <日期> 分布标定，换语料要重标`。与 OPTIMIZATION-PLAN W3-5 同一条纪律、可共用同一次测量，但**不是同一条**：W3-5 改的是 crates/semantic/src/recall/schema.rs 的 trgm_tables 表召回，不同文件不同读路（known=false）。
   - 验收：exemplar.rs tests 加一条：三条召回函数的函数体必须含 `word_similarity` 门或距离门（今天只钉了 enabled+valid 两条件）。手工：拿一个与全部语料都不相干的问句，prompt 里不再出现 few-shot 段。regression.py 全量跑，把改判题号写进提交信息。

10. **学习增益题集 + expected_case 绑定 + 文档订正与「明确不采用」留痕**（S；依赖：步骤 3、步骤 5）
   - 文件：tools/regression_cases_learned.json, crates/semantic/src/registry/learn.rs, crates/server/src/admin_api.rs, docs/ARCHITECTURE.md, docs/AGENT-ARCHITECTURE.md, docs/PROGRESS.md
   - 改法：①新建题集文件 `tools/regression_cases_learned.json`（形状与 regression_cases.json 同构，初始放 3-5 题：每条来自一次真实失败问句 + 期望路由/期望非空结果）。`regression.py:65 --cases` 已支持任意题集路径，**脚本一行不改**。②`meta.learn_event.expected_case` 存题名（步骤 3 已建列），admin 的 `GET /api/admin/learn` 返回体带上它。③验收命令固定为 `python tools/regression.py --cases tools/regression_cases_learned.json`；回滚判据随之确定：某 batch_id 上线后这份题集净转红 → `POST /api/admin/learn/{batch_id}/rollback` 撤回并复跑。④文档：ARCHITECTURE §2 I4 与 §3 F6 的 visibility/VIS_PRED 描述改写成「以单列 owner 三级作用域兑现」（六处：:63/:96/:181/:184/:210/:333，known=true 出处 OPTIMIZATION-PLAN:651④，本步把它从『改写成已放弃』改成『改写成已兑现』）；清掉 exemplar.rs:4 与 review.rs:10 两处「两道总闸」误导注释；§8「明确不采用」加两行——prime-agent 的 RLM/持久 IPython/子 agent/daemon（程序化执行面 vs I1/I5 结构性冲突、Python 运行时＝D6 新依赖、其自身文档承认非安全沙箱）与 harness 的「概览塞系统提示词」召回方式 + 自动向 public 层写入（本仓 pgvector 近邻+重排更优；public 层必须人工复核）。PROGRESS.md 追加本轮记录。
   - 验收：`python tools/regression.py --cases tools/regression_cases_learned.json` 全绿；故意把某条学到的教训 disable 后该题集转红（证明题集真的在量这条学习的收益，不是恒绿）。`tools/audit_trace.py` exit 0（文档引用回查）。grep `VIS_PRED` 仍零命中且文档不再声称它存在。

### 风险
- owner 谓词一旦拼漏就是静默 fail-open（学到的东西继续跨用户），而漏掉的那条召回没有任何用户可见症状。缓解：drift.rs 的 `every_learning_recall_is_owner_scoped` 必须与步骤 2 同一提交落地并开枪验过（改坏一处即红），不许「先改召回、守卫下批补」。
- 自动蒸馏改成只写 private 后，public 层（今天全员共享的那批）**不再增长**——如果人工复核通道继续没人跑（步骤 8 之前的现状），整体表现是「学习面看着还在写，但共享经验冻结在改造当天」。缓解：步骤 8 与步骤 2 必须同批上线，且 admin 质量页要露出 pending/candidate 计数，让「队列没人处理」是个看得见的数字。
- `memory.rs:96 score()` 的 ×1.3 私有档位是拍的，不是量的。个人层条数一多就可能把过人工复核的 public 条目整批挤出前 3（MEMORY_LIMIT=3）。缓解：判据打在纯函数上（单测钉住同 sim/hit/age 时的相对序），上线后用 learn_event + query_log 对照一次真实召回构成再决定是否调档；写 `// ponytail:` 标注这是待标定的系数。
- `meta.user_pref` 是新的用户行为画像面：即使只存表面槽位，「某人常查哪个省区」也是可推断信息。缓解：只存 IntentV1 已经透出到 `intent_summary` 的那四个表面槽位（不落数据库值/canonical ID），读侧只回给本人，且必须能一键清除（`DELETE FROM meta.user_pref WHERE login_name=$1` 一条谓词）——这一条要写进端点，不能只留在设计里。
- `AskDeps.learn` 默认 true + 两个 CLI 分支传 false，是靠人记得改调用点。将来新增第三个批量入口（比如新的评测脚本或 MCP 批处理）会默认开着学习、再次污染语料库。缓解：源码守卫只能守住今天这两处；更稳的形态是让 `learn` 无 Default、构造点必须显式写（编译期强制），但那要改所有 AskDeps 构造点——先按默认 true 上线并在 AskDeps 的字段 doc 上写明这条已知天花板。
- 负反馈直接 disable 语料是「一票否决」：一个误点的 👎 会立刻掐掉一条已过 VQR 的语料。缓解：只做减法（不删只停）+ 全程记 learn_event 可回滚 + admin 页能看到「因反馈停用」的清单重新启用；不给反馈任何加法权限（👍 不得授予 enabled）。
- `tools/regression_cases_learned.json` 的题目是人写的，写歪了会变成恒绿题集（守着空气）。缓解：步骤 10 的验收里明写反向枪测——把对应的教训 disable 掉，该题集必须转红；不转红说明题目没在量这条学习。
- 失败复盘的 `streak >= 2` 门会让**真正致命但只出现一次**的错误（比如新上线表的口径坑）晚一轮才产出教训。这是刻意的取舍（省掉大部分一次性抖动的 fast 调用 + 候选池信噪比），但要在 review.rs 的注释里写明这条天花板与升级路径（真需要即时学习时按 err_class 白名单开例外，不要整体降门槛）。
- confidence=low：`meta.learn_event.before/after` 用 jsonb 存整行快照，行宽随三张表加列而变，回滚 UPDATE 的列清单要跟着漂。缓解：rollback 只回滚**账本里记过的那几列**（status/owner），不做整行还原——insert 类事件回滚就是 DELETE，够用且不会因加列而失效。


## 设计：kb-upgrade：知识库能力包（crates/knowledge + kb_eval + 解析链 + KB 多轮）加强方案

## 结论先行

对完 Yuxi v0.7.1 全链路，十环里我方已在七环更强：文档级 ACL 内联进每条检索 SQL（acl.rs:409，Yuxi 只有 KB 级 read_scope/manage_scope）、引用三层强制 + 冲突并列表（answer.rs:45-71 与 :667/:690/:895/:1007，Yuxi 自承 prompt 级引用效果不好已停用）、九路加权 RRF（retrieve.rs:456-521）对它的两路 WeightedRanker(0.7,0.3)、`enabled + status + effective_from/to` 三重生效期闸（retrieve.rs:735-745，它完全没有）、逐扩展名解析能力机读自报（embed_service.py:902）、字符偏移回链（0020:116-117，被导图预览真消费）、单库 PG（AGE+pgvector+pg_trgm+jieba）对它的 Milvus+Neo4j+Redis+MinIO 四套。

所以本轮**一条主链路机制都不加**。增量全部落在主链路之外的四条断链 + 两条减法，且不与 docs/OPTIMIZATION-PLAN-2026-08-13.md 的 W4 十三条重叠。

## 一、解析与分块：把「档位」从服务快照变成文档属性

降级链本身不动（`_p_pdf` 三级 pymupdf4llm→fitz→pypdf + `_pdf_ocr_fill` 逐页 vision + 旧 Office 走 soffice，比 Yuxi 的七引擎注册表更细）。缺的是**这一篇是哪一档解出来的**：`ParsedDoc`（doc.rs:29-38）无 engine 字段，`kb.doc`（0020:117-144）无对应列，而 `_pdf_fitz`/`_pdf_pypdf` 在文本层正常时根本不产 notes ⇒ tier-2/3 全程无声。后果四条可测：`heading_path` 恒空 → ①embedding 配方的「章节」行对全篇同值（0020:345）②`TITLE_SQL` 的 `word_similarity(q, c.heading_path)` 半路失效（retrieve.rs:942）③`source_of` 的引用说不出章节（answer.rs:615）④导图章节树塌成一层（kb_mindmap_api:930）。

同刀补上第二个「有意简化」：preset 不入库（kb_api.rs:546/674 白纸黑字承认），于是用户选的 laws/qa 分块在第一次自愈或 reprocess 后恒变 general——这是准确性问题不是运维问题。

**目标形态**：`kb.doc` 加 `parse_engine text NOT NULL DEFAULT ''` 与 `chunk_preset text NOT NULL DEFAULT ''` 两条幂等 ALTER（进 `KB_DDL_DELTA`，store.rs:115 已有同形四条）；`doc_cols!` 宏与 `DocRow` 各加两位；`ingest::run`(:883) 落库时写入。重跑清单不写代码，就是一条 `SELECT doc_id FROM kb.doc WHERE parse_engine LIKE 'fitz%' OR parse_engine LIKE 'pypdf%'`，走既有 reprocess。

## 二、索引与融合：一条路都不加，删一条死路

九路 → 加权 RRF(k=60) → 相邻合并(MAX_MERGE_SPAN=16) → 来源多样化截断，形态不变。W4#8（rerank 窗口 12<候选 24）与 W4#9（terms 路无 IDF，`terms_of` 去重让 TF 恒 1）已在方案内，本轮只钉一条排期约束：**W4#8 必须早于任何「精排有没有收益」的结论**——窗口卡在最终输出量的 2 倍，结构上救不回第 13-24 名，用它去测必然测出「没收益」然后误删一个能力。

**减法**：`kb.chunk.ts` 生成列（0020:336）+ `idx_kb_chunk_ts` GIN（0020:370）全仓零读者——`to_tsvector|ts_rank|tsquery` 在 `crates/**/*.rs` 只命中 retrieve.rs 的三处**注释**（:179/:816/:2017），而那正是「中文 FTS 322 格恒 0、已被单号/型号 ILIKE 路替换」的实测证据。每块多存一份 tsvector、每次写多维护一个 GIN，换 0 次读；更坏的是它长得像「FTS 路还在」，下一个人会照着把已知恒 0 的路加回来。删列删索引，`store.rs:2331` 那条把 `GENERATED ALWAYS…STORED` 钉成契约的断言换成反向断言（0020 不得再出现 `tsvector`）；retrieve.rs 的实测注释**保留**——它是「别再加中文 FTS 路」的唯一证据。

## 三、图谱增强与排序：先把量具造出来

W4#10 是「先量后动」，但**量具今天不存在**：`GEN_SYSTEM`（kb_eval_api.rs:190）第 2 条明令「不许出需要外部知识、计算或多片段综合的题」，`SAMPLE_SQL`(:142) 一次抽一块，`gold_rank`(:781) 只认单个 gold。而 KG-PPR（retrieve.rs:1150/1346）、`merge_adjacent`(:1834)、`relation_candidates`(:1108) 三样的价值全部落在「一块答不了」的题上。用单跳题量只在多跳生效的路，结论必然是「没收益」→ 误删。所以多跳题档**必须排在 W4#10 之前或同批**，复用现成的 `doc_graph::mention_pairs`（:798）出题，不新建任何机制。

## 四、评估闭环：本轮最重要的结构性修复

今天自动评测五个指标（recall1/3/5/10 + answer_acc，kb_eval_api.rs:84-86）**全部单调偏好多召回**，而检索侧三个标定阈值的注释逐条写着「调低会先打死近域 nohit」（`VEC_MAX_DIST=0.55` :222、`TERMS_MIN_HITS=2` :73、`TRGM_MIN=0.2` :192）。今天把任一阈值调松，五个数一起变好，**没有任何判据会红**。根因是零负样本。

**目标形态**（零新表）：`meta.kb_eval_items` 加 `kind text NOT NULL DEFAULT 'recall'`（负题 `gold_chunk_id=-1`）与 `gold_chunk_id2 bigint`；`meta.kb_eval_runs` 加 `nohit_acc float8` 与 `multi_recall5 float8`。负题语料复用 `SAMPLE_SQL` 只换 `space_id`（取该 viewer 可读的**另一个**空间出题，对**本**空间提问），判据是纯函数不调 judge：`citations` 为空 ∨ markdown 含 `answer::NO_HIT`（:30 已是常量）。**只有一个可读空间时负题数为 0，必须在 `run.error` 写「无第二空间，负样本未测」并让页面显示**——不许静默按 0 题算满分（与 kb_eval.py 头部 :14-25 的反空转退出码闸同一条纪律）。

**规模**：`tools/kb_eval_cases.json` 今天 16 题（recall 7 / cite 2 / acl 2 / inject 2 / nohit 2 / conflict 1）+ 10 夹具，是唯一会红的那条链。扩到 **28 题**：+3 多跳、+2 半覆盖（W4#1 验收）、+2 生效期（已失效文档必须不参与）、+2 KB 多轮、+1 preset（laws 下条文级召回）。不再上扩——六个 kind 判据补齐即止。**门禁**：`python tools/kb_eval.py` 进 CI，不带 `--allow-skip`（0/1/2 三态退出码已定义）。

## 五、多轮：KB 路结构上单轮

`answer()`（answer.rs:84-94）只吃 `question: &str` 无历史形参；全仓唯一的追问改写 `rewrite_followup`（ask.rs:1562）跳过守卫写死成数据路形状（ask.rs:1583-1593：无 SQL + 无公司实体 + 无显式指代 → 原样返回）。KB 轮 payload 结构上没有 `sql` 键（chat.rs:193-196 自己注明），`prev_q` 是制度问句不含公司实体，`explicit_reference` 只认「它/这个/那个/该/此」五词 ⇒「报销标准是多少 → 那出差住宿呢 → 要交什么材料」，第 2/3 轮进 retrieve 的是碎片。**改的是那一个守卫不是调用点**，约 15 行。风险面比数据路小一个量级：改写歪了只会让 `keep_cited_only` 判无据回「没有相关内容」，不会产出错数。

**不做**多子问句并行检索——OPTIMIZATION-PLAN「明确不做」已裁决（先落计数观察两周）。

## 六、答案协议：W4 四条不重复，只补落账

W4#1/#2/#11/#12 已覆盖部分覆盖声明、版本冲突看引用、source_uri、近域 nohit 文案。增量只有一条且是落账不是协议：`SearchReport.vector_degraded` 与九路 `SearchStats`（retrieve.rs:305-330）今天只进一条 `tracing::info`(:566) 与 `/api/kb/search` 诊断 JSON（kb_api.rs:2320），`qa_log::Obs`(:22-25) 只有 usage/llm_calls ⇒ 生产上「这题为什么没命中」「这周多少答案产在 embed 熔断期」事后不可查。改两个函数、零迁移、不碰 `kernel::qalog` 共享列清单。

## 七、权限与可见范围：零迁移，补一条守卫

已更强（文档级 + 内联 + 三重生效期闸 + acl.rs:21-24 已裁决不搬 Yuxi 的 min(授予,角色上限)）。唯一结构性缺口：`visible_docs!()` 片段**只管「谁能看」**，生命周期过滤是每个内联者自己的义务（acl.rs:405-408 自述）。今天 16 个内联点里，kg.rs:1057 有一条源码断言、retrieve.rs:2091/2115/2226 三条**逐函数手列**——明天新加一条检索 SQL 抓不到。修法是一个约 30 行的新测试文件，照 semantic/tests/drift.rs:24 的 `sources()` 形状扫源码。**不扫 store.rs**：那是管理面 CRUD，属主看见自己停用/失效的文档是正确行为。

## 八、增量与重建成本：加一张旁表，不改状态机

状态机 `pending→parsing→chunked→embedded|failed`（store.rs:233-259）没有 parsed 稳定态。向量配方升级那条路已经做对（0020:352-368 退回 chunked、`embed_fill` 后台补、不重解析）；但换分块 preset 走 `reprocess`(:531)→`build_shadow`(:827)→`parse_input`(:962) 每次都从磁盘重 parse，扫描件 = 重跑按页付费的 vision OCR，而分块逻辑一个字节没变。加 `kb.doc_blocks(doc_id PK, engine, blocks jsonb, created_at)` 旁表，**不动状态机**（动它要连带 stuck_docs 自愈、W4#13 的 CAS、影子链一整片，不值）。engine 不同则真重解析——这正是第一节要的行为。

## 不变量与纪律

I5：零处让外部文本进指令位（`route_census` 是定长字面量拼接；`doc_blocks.blocks` 回读后仍走同一条 `chunk_with_preset`）。I4：`doc_blocks` 不是缓存而是解析产物存档，读取在 `reprocess` 内且 doc_id 已过 ACL；负样本跨空间取语料但只用该 viewer 可读的空间。D6：新增全部是 PG 列/表 + Rust 纯函数 + 既有 serde_json。D2：**不往 retrieve.rs(3114)/store.rs(3039)/ingest.rs(2497)/answer.rs(2327) 四个已超线文件加净新增行**——`route_census` 落 qa_log.rs(351)，ACL 守卫落新 tests 文件。删除 > 新增：净删 `ts` 列 + GIN 索引 + 一条契约断言。

## 明确不抄 Yuxi 的六条（抄回去是净退化）

①KB 级 read_scope/manage_scope + `min(授予,角色上限)`（resource_permission.py:87-102）——我方 acl.rs:409 是文档级并内联，:21-24 已裁决；②停用 prompt 级引用（prompt.py:31-32 自承效果不好）——我方是 keep_cited_only/keep_supported_only/disclose_versioned_sources 三层强制；③稠密 + 内建 BM25 两路 WeightedRanker(0.7,0.3)（milvus.py:1010-1033）——我方九路加权 RRF + 权重配置闸拒 NaN/负值（retrieve.rs:107-131）；④存储三分（Neo4j + Milvus + PG + Redis + MinIO）——我方只有 PG 一套，D6 零新增依赖；⑤`semantic` preset 无聚类（presets.py 自认名不副实）——我方 Semantic/Book 是同一个毛病，W4#6 已排删；⑥无生效期/启停概念——我方 retrieve.rs:735-745 三重闸。另：Yuxi 的 `benchmark_generation.py:117` 图增强出题**要抄**（那是它抄得到我方抄不到的唯一一处，见第三节）。

### 步骤

1. **删死列 kb.chunk.ts + idx_kb_chunk_ts GIN 索引（纯删除打底）**（S；依赖：—）
   - 文件：crates/knowledge/src/store.rs, crates/semantic/migrations/0020_kb_init.sql
   - 改法：①KB_DDL_DELTA(store.rs:115) 追加两行幂等语句 `DROP INDEX IF EXISTS kb.idx_kb_chunk_ts;` 与 `ALTER TABLE kb.chunk DROP COLUMN IF EXISTS ts;`（与该常量既有四条 ALTER 同形）；②同刀删 0020_kb_init.sql:336（ts 生成列）与 :370（GIN 索引）两行；③store.rs:2331 那条 `assert!(chunk.contains("GENERATED ALWAYS AS") && chunk.contains("STORED"))` 换成反向断言「0020_kb_init.sql 全文不得再出现 tsvector」（防再次加回）。retrieve.rs:179-180/:816-822/:2017 三处实测注释**全部保留**——它们是「别再加中文 FTS 路」的唯一证据。
   - 验收：cargo test -p dms-knowledge 全绿；删前删后 `python tools/kb_eval.py` 16 题逐题结果字节一致（该列本来无人读 ⇒ 必须完全一致，任何差异都说明删错了）；新反向断言把 tsvector 加回 0020 立刻红（开枪验一次）。

2. **KB 追问改写守卫：把 SQL 形状的跳过条件改成「上一轮是不是 KB」**（M；依赖：—）
   - 文件：crates/server/src/chat.rs, crates/agent/src/ask.rs, crates/server/src/main.rs
   - 改法：①chat.rs::last_turn(:203) 返回值从 `(String, Option<String>)` 扩成 `(String, Option<String>, bool)`，第三位 prev_is_kb = payload 的 `kind` 字段等于 "text"（沿用 :234 那个 pick 闭包，深度轮的 `result` 嵌套档一并读）；②ask.rs:75 的 `pub type PrevTurn` 从 4 元组扩成 5 元组（第五位 bool），main.rs 的两个构造点(:2301 / :2710)各多传一个值，CLI/xcx/deep/mcp 传 false（与今天传 None 同口径）；③ask.rs:1583-1593 的守卫改成 `hist_sql.is_none() && !prev_is_kb && company_span(prev_q).is_none() && !explicit_reference`；④system 提示词(ask.rs:1594-1600)规则 5 之后追加一句「上一轮是知识库问答时，只继承上一问出现的制度/主题名词，不得补造指标、时间或筛选口径」。约 15 行，不动 answer.rs 签名（KB 侧仍是单轮入参，改写在 agent 侧完成）。
   - 验收：ask.rs 单测：prev_is_kb=true 且 prev_sql=None 时必须真发起改写（今天恒返原句）；tools/regression_cases_multiturn.json 加一条 KB 三轮链（报销标准是多少 → 那出差住宿呢 → 需要交什么材料），断言第 2/3 轮 payload 的 resolved_question 含「出差住宿」「报销」；既有 79 题 route 分布逐条不变。

3. **解析档位与分块 preset 落库：kb.doc 加 parse_engine / chunk_preset 两列**（M；依赖：—）
   - 文件：tools/embed_service.py, crates/connector/src/doc.rs, crates/knowledge/src/store.rs, crates/knowledge/src/ingest.rs, crates/server/src/kb_api.rs, web/src/KbPanel.vue
   - 改法：①embed_service.py:814 parse_doc 返回 dict 加 'engine'：.pdf 取 f"{_pdf_text_engine()}+{_ocr_engine() or 'noocr'}"（两函数已存在于 :716/:721），其余扩展名取 CAPS[ext][1] 的能力名；②doc.rs:29 的 ParsedDoc 加 `pub engine: String`（结构上已有 #[serde(default)]，老服务不带这个键不会反序列化失败）；③store.rs:115 的 KB_DDL_DELTA 追加 `ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS parse_engine text NOT NULL DEFAULT '';` 与 `ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS chunk_preset text NOT NULL DEFAULT '';`；④doc_cols! 宏(store.rs:127)与 DocRow 各加两位；⑤ingest::run(:883) 落库时写入 parse_engine 与 `resolve_preset(req.preset)` 的名字；⑥kb_api.rs 的 reprocess/自愈四处（:546/:605/:640/:676/:696）把 `preset: None` 改成读该 doc 行的 chunk_preset（消掉那两条自认「有意简化」的注释）；⑦kb_api 文档列表 JSON 透出两列，KbPanel.vue 列表加一格。
   - 验收：单测钉住 ParsedDoc 缺 engine 键仍反序列化成功且为空串；ingest.rs:1961 那条 exts_cover_the_doc_service_capabilities 同款加一条「CAPS 每个能力名都能产出非空 engine 串」；带库：上传一份 PDF 后 kb.doc.parse_engine 非空；用 preset=laws 上传后 reprocess，chunk 边界与首次入库一致（今天必不一致）。

4. **kb.doc_blocks 旁表：换 preset 不再重跑按页付费的 OCR**（M；依赖：上一步（ParsedDoc.engine 字段））
   - 文件：crates/knowledge/src/store.rs, crates/knowledge/src/ingest.rs
   - 改法：①KB_DDL_DELTA 追加 `CREATE TABLE IF NOT EXISTS kb.doc_blocks(doc_id text PRIMARY KEY REFERENCES kb.doc(doc_id) ON DELETE CASCADE, engine text NOT NULL, blocks jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT now());`；②ingest.rs:962 parse_input 成功后写一行（engine 取上一步新增的 ParsedDoc.engine；序列化用既有 serde_json，Block 已 Serialize）；③reprocess(:531)/build_shadow(:827) 先查这张表，engine 与 /health 当前上报档位相同即复用 blocks 跳过 doc.parse，不同则重解析并覆盖；④sheets 不进这张表（表格双通道另有物理表）；⑤单文档 blocks 序列化后超 8MB 不落缓存，写一行 `ponytail: 8MB 是保险丝天花板，真撞上再谈分片`。**不改状态机**——动它要连带 stuck_docs 自愈、W4#13 的 CAS、影子链一整片。
   - 验收：同一份 PDF 连跑两次 reprocess，第二次 parser 容器的 /parse 访问日志为 0 次；把 kb.doc_blocks.engine 人为改成别的值后必须重解析；纯函数单测：blocks 序列化→反序列化后与原 Vec<Block> 逐字段相等。

5. **KB 问答落账带检索证据：九路 census + 向量降级进 meta.query_log**（S；依赖：W4#7（删 ext_kb 后 census 从 9 路收成 8 路；先做本条则删 ext_kb 时同刀改常量与单测））
   - 文件：crates/knowledge/src/qa_log.rs, crates/knowledge/src/answer.rs
   - 改法：①qa_log.rs:22 的 Obs 加 `routes: [usize; 9]` 与 `vec_down: bool`；②answer.rs:194 的 run() 本来就返回 (out, obs) 二元组，把 report.stats / report.vector_degraded 一并塞进 obs，**不加任何形参**；③qa_log.rs 新增纯函数 `fn route_census(s: &SearchStats) -> String`，产定长人话「｜召回 v3 e0 tg2 ti1 m0 r0 kg0 x0 tm4 →6」（字段顺序与 retrieve.rs:1463 的 CHANNEL_NAMES 一致）；④qa_log::entry(:84) 把它追加到既有 sql 摘要串尾部，vec_down 为真时前缀「｜降级:向量」。零迁移、不碰 kernel::qalog 的共享列清单、前端一行不改（trace_api/usage_api 两个页面自动带出）。
   - 验收：纯函数单测钉住格式与字段顺序（与 CHANNEL_NAMES 同序，改序即红）；带库：`SELECT count(*) FROM meta.query_log WHERE route='knowledge' AND sql LIKE '%降级:向量%'` 在停掉 embed 服务后能查到行。

6. **kb_eval 加负样本档：让「调松阈值」终于有一个会红的判据**（M；依赖：—）
   - 文件：crates/server/src/kb_eval_api.rs, web/src/KbPanel.vue
   - 改法：①DDL 数组(kb_eval_api.rs:77-107)追加两条幂等 ALTER：meta.kb_eval_items 加 `kind text NOT NULL DEFAULT 'recall'`，meta.kb_eval_runs 加 `nohit_acc float8`；负题 gold_chunk_id 写 -1；②负题语料复用现成 SAMPLE_SQL(:142) 只换 space_id 参数——取该 viewer 可读的**另一个**空间（create_run 已有空间可读闸），抽 sample_size/5 块出题，然后对**本**空间提问；③判据是纯函数不调 judge：Answer 的 citations 为空 ∨ markdown 含 answer::NO_HIT（answer.rs:30 已是 pub 常量族）即 pass；④**只有一个可读空间时负题数为 0，必须在 run.error 写「无第二空间，负样本未测」并让页面显示**——不许静默按 0 题算满分（与 tools/kb_eval.py 头部 :14-25 的反空转退出码闸同一条纪律）；⑤run_json(:422) 与 KbPanel 评测面各加一格 nohit_acc。
   - 验收：把 VEC_MAX_DIST 临时改成 2.0 重跑一次，nohit_acc 必须掉下来——这就是今天缺的那条会红的判据；单空间环境下 run 完成后 error 字段非空且页面可见（不许显示满分）。

7. **kb_eval 加多跳档：W4#10「先量图谱路收益」的量具**（M；依赖：必须排在 W4#10 之前或同批——它是那条「先量」的量具）
   - 文件：crates/server/src/kb_eval_api.rs
   - 改法：①kb_eval_api.rs:642 sample_chunks 旁加 `sample_pairs(st, space_id, n)`：复用 crates/connector/src/doc_graph.rs:798 的 mention_pairs（kg_route 正在用），取同空间内共享 ≥1 实体、doc_id 不同的 chunk 对；②新增第二套 GEN_SYSTEM_MULTI（「必须同时用到两段才能答」，与 :190 的单跳 GEN_SYSTEM 并列，不改后者）；③meta.kb_eval_items 加 `gold_chunk_id2 bigint`（幂等 ALTER 进 DDL 数组）；④gold_rank(:781) 改成「两个 gold 都进 hits 才算命中」（保留既有 merged 区间判定）；⑤meta.kb_eval_runs 加 `multi_recall5 float8`；⑥KG 未建图的空间 sample_pairs 返空 → 自动退回单跳、不报错。
   - 验收：在有图谱的评测空间跑一次，multi_recall5 非 NULL；把 DMS_KG_RETRIEVAL=off 后重跑，multi_recall5 必须下降（若不降说明图谱路本就无收益，这正是 W4#10 要的结论，且这次是可信的）；未建图空间跑一次不报错且 multi_recall5 为 NULL。

8. **kb_eval_cases.json 16→28 题 + CI 门禁**（M；依赖：W4#1（半覆盖题）、本清单第 2 步（多轮题）、第 7 步（多跳题的 gold 形状））
   - 文件：tools/kb_eval_cases.json, tools/kb_fixtures, scripts/check-arch.ps1
   - 改法：现有 16 题（recall 7 / cite 2 / acl 2 / inject 2 / nohit 2 / conflict 1）+ 10 夹具扩到 28：+3 多跳（跨文档综合，判据是 citations 覆盖两篇）、+2 半覆盖（W4#1 的验收题：「出差住宿和市内打车各有什么上限」，expect 必含「知识库里没有关于」）、+2 生效期（已失效/未生效文档必须不参与检索，answer 必回 nohit）、+2 KB 多轮（三轮链，断言第 2/3 轮 resolved_question 含首问的主题名词）、+1 preset（laws 分块下按条文号召回）。**不再上扩**——六个 kind 的判据补齐即止，再多是维护成本。门禁：`python tools/kb_eval.py`（0/1/2 三态退出码已定义）进 CI，**不带 --allow-skip**。
   - 验收：python tools/kb_eval.py --selfcheck 先绿（判定逻辑与退出码闸自检）；28 题全绿；人为把 answer.rs 的 keep_cited_only 短路掉，cite 与 nohit 两族必须红（开枪验一次）。

9. **ACL 生命周期守卫：把「作者记得写」变成 CI 记得**（S；依赖：—）
   - 文件：crates/knowledge/tests/acl_drift.rs
   - 改法：新建 crates/knowledge/tests/acl_drift.rs（约 30 行，照 crates/semantic/tests/drift.rs:24 的 sources() 形状）：扫 crates/knowledge/src/retrieve.rs 与 kg.rs 里每一个 `const *_SQL: &str` 与 `fn *_sql()` 的函数体，断言各自要么含 `visible_docs!()`，要么含 `doc_id = ANY($1`（＝已被预算好的可见集合参数收口）；含 visible_docs!() 的还必须同含 `enabled=true` 与 `status IN ('chunked','embedded')`（acl.rs:405-408 自述「生命周期过滤是每个内联者自己的义务」）。**不扫 store.rs**——那是管理面 CRUD，属主看见自己停用/失效的文档是正确行为，一刀切会误红。今天 retrieve.rs:2091/2115/2226 与 kg.rs:1057 那四条逐函数手列的断言保留不动（本条只是补上「新加的 SQL 也会被抓到」这一层）。
   - 验收：对当前代码即绿；把 retrieve.rs::visible_sql 的 `x.enabled=true` 删掉立刻红（开枪验一次）；新加一条不带 ACL 的检索 SQL 立刻红。

10. **文档留痕：yuxi.json 加 we_are_stronger + ARCHITECTURE §8 一行**（S；依赖：—）
   - 文件：docs/research/yuxi.json, docs/ARCHITECTURE.md
   - 改法：零代码。①docs/research/yuxi.json（今天只有 summary/repo/mechanisms/half_baked 四键）加新键 we_are_stronger，把六条逐条写成「Yuxi 做法 → 我方对位 file:line → 结论：不抄」：KB 级 ACL+min(授予,角色上限)→acl.rs:409/:21-24；停用 prompt 级引用→answer.rs:667/690/895/1007；两路 WeightedRanker→retrieve.rs:456-521 + :107-131；存储四套→PG 单库；semantic preset 无聚类→W4#6 已排删；无生效期概念→retrieve.rs:735-745。同键里明写**要抄的那一条**：benchmark_generation.py:117 的图增强出题（本清单第 7 步）。②acl.rs:21-24 那段「不搬 Yuxi 角色上限」的裁决在 docs/ARCHITECTURE.md §4.5 的 acl.rs 行没有对应句，补一行——裁决只写在源码注释里，文件一重构就消失。③ARCHITECTURE.md §8「明确删掉的」表加一行 kb-upgrade 条目，写清「不抄 Yuxi 的六条」与「知识路不做多子问句并行检索（OPTIMIZATION-PLAN 已裁决，先落计数观察）」。
   - 验收：tools/audit_trace.py exit 0（新增引用必须能被它回查到，静默腐烂当场红）；grep 'we_are_stronger' docs/research/yuxi.json 命中。

### 风险
- kb_eval_api.rs 今天已 1158 行、破 D2 的 450 上限，本方案第 6/7 步还要往里加负样本与多跳两档（约 +120 行）。缓解：两档各自落新文件 kb_eval/{negative,multihop}.rs，kb_eval_api.rs 只留编排；若排期不允许，必须在提交信息里记一条欠账并排进 W7 的拆分批次——不许默默把它推到 1300 行。
- kb.doc_blocks 是本方案唯一的新表。它存的是解析产物（blocks jsonb），单文档可达数 MB，扫描件 PDF 尤甚。8MB 保险丝是拍的不是量的，上线后必须看一周 pg_total_relation_size('kb.doc_blocks')；若增长快于 kb.chunk 就把保险丝调低或改成只缓存 tier-1（有 heading_path 的那档，重解析代价最低的反而最不需要缓存——真要收缩就砍这一档）。
- 多跳评测（第 7 步）依赖 AGE 图谱已建。而 W4#10 的另一半结论可能是「建图按 chunk 烧 2000 次 Fast LLM 不划算，砍掉建图」。两条同批做时必须先跑多跳评测拿到数，再决定砍不砍——顺序反了就是拿没有量具的结论删能力，正是本方案第三节要防的事。
- 第 2 步扩 PrevTurn 元组要动 main.rs 两个构造点 + CLI/xcx/deep/mcp 四处传 false。5 元组已经逼近可读上限（ask.rs:75 今天是 4 元组），再加就该换具名 struct。本轮不换（换要打到全部构造点，且与 W5#4「上一轮结构化意图跨轮继承」重叠），但如果 W5#4 先落地，两条必须合并成同一次改动——不许同一段代码搬两遍。
- 负样本档（第 6 步）依赖「该 viewer 至少能读两个空间」。生产上很可能只有一个空间，于是 nohit_acc 恒为 NULL、这道判据恒不生效——那就退化成又一个恒真判据，与它要修的病是同一种。缓解：run.error 的显式文案 + 页面显示是硬要求（不是 nice-to-have）；若上线两周仍恒 NULL，就改成用同空间内 effective_to 已过期的文档出题（那批文档检索侧本来就不可见，天然是负样本），不再依赖第二个空间。
- 第 1 步删 ts 列会重写 kb.chunk 整表（DROP COLUMN 在 PG 里是标记删除不重写，但 DROP INDEX 会释放空间）。生产 kb.chunk 行数未知——上线前先 `SELECT count(*) FROM kb.chunk`，超过 50 万行则把这两条 DDL 从 KB_DDL_DELTA（启动时同事务跑）里挪出来单独手工执行，避免启动迁移事务长时间持锁。
- confidence=low：第 3 步给 embed_service.py 的 parse_doc 加 'engine' 键，我只读到 :814 的入口与 :716/:721 两个函数名，没有逐行读完 _p_* 全部实现。CAPS[ext][1] 是否每个扩展名都有可用的能力名串需要实施时当场核对（ingest.rs:1961 那条既有的 exts_cover_the_doc_service_capabilities 断言是最好的核对点）。


## 设计：router

## 目标形态

一条链，四段：`prepare_question`（一次理解）→ `route::plan`（一次路由，纯函数零 IO）→ `route::run`（多路并行）→ `route::merge`（一次合成）。server 六个入口（main.rs:2497 api_ask / main.rs:2600 api_ask_stream / xcx_api.rs:451 / xcx_api.rs:500 / mcp_api.rs:401 / deep_api.rs:4543）只剩「输出形态」差异：JSON / SSE / 小程序包 / MCP text / 深度 artifact。CLI（main.rs:1178）与 HTTP 从此对同一句话行为相同 —— 这是断点①与断点⑤的同一个修法。

### 数据结构（新增/修改，全部落 agent）

**① `crates/agent/src/route.rs`**（新文件，约 260 行，变更原因＝「一次问答分几路、怎么合成」）：

```rust
pub struct KbDeps<'a> {
    pub owned: &'a dms_connector::owned::OwnedStore,      // 非 Option 是关键
    pub weights: &'a dms_knowledge::retrieve::RrfWeights,
    pub space_id: Option<&'a str>,
}
pub enum RoutePlan {
    Data(Vec<RoutedQuestion>),
    Knowledge(String),
    Both { data: Vec<RoutedQuestion>, kb: String, unsplit: bool },
    Clarify(Vec<String>),                                  // reasons，非空
}
pub fn plan(prepared: &PreparedQuestion) -> RoutePlan;     // 纯函数、可单测
pub async fn run(d: &AskDeps<'_>, p: &Principal,
                 prepared: &PreparedQuestion, explicit_ds: Option<&str>)
    -> anyhow::Result<AskResult>;
```

`AskDeps`（ask.rs:79）加**一个**具名字段 `pub kb: KbDeps<'a>`（D4：共享状态一律命名，不连排三个裸引用）。`owned` 非 `Option` 是结构性的：CLI 在 main.rs:1149 已有 `owned`、cfg 在 db.rs:161 已有 `kb_rrf_weights`，不给「CLI 少一路」留编译得过的口子。

**② `AskResult`（ctx.rs:74）加一个字段**：

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub kb: Option<dms_kernel::Answer>,
```

与 server 今天注入的 `payload["kb"]`（main.rs:2161）逐字节同形 → 前端零改动。

**③ `IntentAttempt`（intent.rs:1187）第四态**：

```rust
enum IntentAttemptState {
    Ready(ResolvedIntent),
    Ambiguous(ResolvedIntent, Vec<String>),   // 新
    Unavailable, Invalid,
}
```

`ground()`（intent.rs:801）今天对 `route()==Unknown || !ambiguities.is_empty()` 一律 `return None`；改成：route 可解但 ambiguities 非空 → `Ambiguous`。reasons 汇三源：模型自报的 `ambiguities`、`project()` 自 push 的三条诊断（intent.rs:625「未找到匹配的结构化子任务」/:629「归属不唯一」/:725「未保留父级范围槽位」）、`subgoal_slots_grounded` 判否的半边名。执行侧行为不变（仍不执行），只是澄清卡从此说得出「哪一句不确定」，且 `coverage_with_evidence` 里 `ambiguity:` → conflicts 那段（intent.rs:1469）从结构性不可达变成可达。

**④ `route()`（intent.rs:436）的 match 补一臂** `(IntentMode::Hybrid, true) => IntentRoute::Hybrid`。今天 `(Hybrid, *)` 全落 `_ => Unknown`，紧接 :801 判 Invalid → 反问卡。模型说了「两件事都要」是**已表达的事实**，不是猜。

### 表结构

**不建新表**。两处已有列的口径修订：

- `meta.query_log.route` 新增两个合法值 `"knowledge"` / `"hybrid"`（列早就在，只是纯 KB 轮与混合轮从来没写过 —— main.rs:2280 的注释白纸黑字承认）。同步 `answerers/mod.rs` 的 `ROUTE_LABELS`。这就是「路由质量的台账」：`SELECT route, count(*) FROM meta.query_log GROUP BY 1` 从此答得出「混合占比多少」。
- 自学习只用一张**已在 prime-agent 调研里提案**的 `meta.user_pref(login_name, ds_id, key, value, hit_count, updated_at)`，本轮只占一个 key：`key='route'`、`value ∈ {data,knowledge,hybrid}`。

### 控制流

```
prepare_question(一次 understand)
  └─ plan(prepared)                       ← 唯一路由裁决点，纯函数
       ├─ Clarify(reasons)  → clarification_result() 带 reasons 渲进卡与 clarify_options
       ├─ Data(n)           → n==1 走 ask_prepared；n>1 走既有 compound 容器(ask.rs:395)
       ├─ Knowledge(q)      → answerers::knowledge::answer
       └─ Both{data,kb,..}  → futures::join!(data_half, kb_half)   ← futures 已是 agent 依赖
                                └─ merge(data_r, kb_r, unsplit)
```

`plan()` 的判据顺序（断点②的答案，从上往下第一条成立即停）：

1. `route()==Data` 且全部 typed child 都是 Data → `Data`
2. `route()==Knowledge` → `Knowledge`
3. `route()==Hybrid` 且 typed subgoals 齐备 → `Both{unsplit:false}`，data 半取全部 Data child，kb 半把全部 Knowledge child 的问句用「；」拼成**一次**检索
4. `route()==Hybrid` 但 subgoals 空（模型只给了 mode）→ `Both{unsplit:true}`，两半**都用整句 `effective_question`**
5. `Ambiguous` 且至少一半可执行 → 按半边分级（见 R3）
6. 其余 → `Clarify(reasons)`

**为什么不重试、不切句**：重试一次要多付一整个 Fast 往返，而「mode=hybrid 但 subgoals 空」是 schema 完整度失败不是瞬时抖动 —— 同 prompt 同输入重发没有新信息（`understand` 今天也只调一次，intent.rs:1368）。确定性子句切分已被本仓裁决否掉（ask.rs:16「Hybrid 不自由拆字符串」，`triage::hybrid_clauses` 因此成死码）：重开它等于把归属证明的洞再挖一遍。而整句双路零风险 —— KB 检索吃整句本来合法（多余词只稀释召回，不会产生错谓词），Data 半吃整句与今天任何单路问句同形。这正是业主 2026-08-11「意图不明确时问数与知识库一起查，综合输出」那条裁决今天唯一没有生产落点的部分。

### 五条不变量

- **R1 一次理解**：`understand()` 在一次请求里只调一次（今天已成立，收口后由 `plan` 是纯函数结构性保证）。
- **R2 一次路由**：`plan()` 是唯一路由裁决点。源码守卫：server 非测试段不得出现 `clarification_result()`、不得 `match ... IntentRoute::`。
- **R3 归属分级 fail-closed 不对称**：Data 半归属证不出 → 该半 fail-closed（错谓词比不答坏）+ 容器 `caliber_note` 点名；KB 半证不出 → 整句检索（无谓词风险）+ 收据打 `hybrid:kb-unsplit`；两半都证不出 → 才澄清，且卡上带 reasons。
- **R4 混合容器永不 verified**：最高 `high`；KB 零引用 / data coverage≠complete / 任一半失败 → 压到 `review`。理由：容器里含模型合成的 `view.insight`。
- **R5 路由偏好只排序不选路**：`meta.user_pref` 的 `route` 计数**不参与 `plan()`**，只用于 Clarify 分支的 chip 排序与 admin 统计。统计先验证不了归属，用它选路直接破「准确 > 智能」。

### 步骤

1. **route() 补 Hybrid 臂 + IntentAttempt 第四态 Ambiguous**（M；依赖：—）
   - 文件：crates/agent/src/intent.rs
   - 改法：① intent.rs:436 的 `match (self.mode, has_data_slots)` 补一臂 `(IntentMode::Hybrid, true) => IntentRoute::Hybrid`（今天 `(Hybrid,*)` 全落 `_ => Unknown`）。② intent.rs:1187 的 `IntentAttemptState` 加第四态 `Ambiguous(ResolvedIntent, Vec<String>)`；`ground()`（intent.rs:801）的 `if self.route()==Unknown || !ambiguities.is_empty() { return None }` 拆成两条：route 仍 Unknown → None（不变），route 可解但 ambiguities 非空 → 返回一个带 reasons 的可判别值（`ground` 返回类型改 `Option<(ResolvedIntent, Vec<String>)>`，`IntentAttempt::validated`(intent.rs:1206) 按 reasons 是否为空分派 Ready/Ambiguous）。③ reasons 汇三源：模型 `ambiguities`、`project()` 自 push 的三条诊断（:625/:629/:725）、`subgoal_slots_grounded` 判否的半边名。④ `user_note()`(:1266) 对 Ambiguous 返回带 reasons 的具体文案而非那句泛泛的「未通过一致性校验」；`summary()`(:1272) 把 reasons 以 `ambiguity:` 前缀写进 coverage.issues（与 :1469 同前缀，那段死码随之复活）。⑤ `is_data_executable`(:1231) 与 `is_ready()` 语义重复，本步先不动，S11 一起删。
   - 验收：单测三条：`(mode=hybrid, 有槽位, subgoals 空)` 的 IntentV1 必须 `route()==Hybrid`（今天是 Unknown）；`ambiguities=["客户名不唯一"]` 的合同必须落 Ambiguous 而不是 Invalid，且 `user_note()` 含「客户名不唯一」原文；`coverage_with_evidence` 对含 ambiguities 的 intent 必须产出 `ambiguity:` 前缀的 conflict（该断言今天必然为空 —— 结构性不可达）。`cargo test -p dms-agent` 全绿。

2. **新建 route.rs：plan() 纯函数 + run() 多路并行 + merge() 一次合成**（L；依赖：S1）
   - 文件：crates/agent/src/route.rs, crates/agent/src/ask.rs, crates/agent/src/ctx.rs, crates/agent/src/lib.rs
   - 改法：新建 `crates/agent/src/route.rs`（约 260 行）。① `pub struct KbDeps<'a>{ owned: &'a OwnedStore, weights: &'a RrfWeights, space_id: Option<&'a str> }`；`AskDeps`(ask.rs:79) 加一个字段 `pub kb: KbDeps<'a>` —— `owned` 非 Option，结构上不给 CLI 少一路留口子。② `pub fn plan(prepared: &PreparedQuestion) -> RoutePlan` 六条判据（设计段已列全），纯函数零 IO。③ `pub async fn run(d,p,prepared,explicit_ds) -> anyhow::Result<AskResult>`：Data 走现有 `ask::ask_prepared`；Knowledge 走 `answerers::knowledge::answer`；Both 用 `futures::future::join`（futures 已是 agent 依赖，Cargo.toml:15）并行两半。④ `AskResult`(ctx.rs:74) 加 `#[serde(skip_serializing_if="Option::is_none")] pub kb: Option<dms_kernel::Answer>`，与今天 server 注入的 payload["kb"]（main.rs:2161）逐字节同形。⑤ `ask::ask_prepared` 开头两道早返（ask.rs:216/:222）降级成 `debug_assert!`：它从此只被 route.rs 以 Data 合同调用。⑥ lib.rs 加 `pub mod route;`。
   - 验收：`plan()` 的纯函数单测矩阵：Ready-Data / Ready-Knowledge / Ready-Hybrid(有 subgoals) / Ready-Hybrid(subgoals 空) / Ambiguous(单半可执行) / Invalid / Unavailable 七态，逐条断言 RoutePlan 变体与 `unsplit` 位。序列化断言：`AskResult{kb:None}` 的 JSON 里**不得出现** `kb` 键（照 `caliber_note_is_omitted_when_absent` 的模子）；`kb:Some(..)` 时 `payload["kb"]` 与 server 改前的形状 assert_eq。`cargo build -p dms-agent` + `scripts/check-arch.ps1` 绿（route.rs 不得出现 sqlx::query）。

3. **收据合成搬进 agent：IntentSummary::merge_hybrid + downgrade_trust**（M；依赖：S2）
   - 文件：crates/agent/src/intent.rs, crates/agent/src/ctx.rs, crates/agent/src/route.rs
   - 改法：① 把 `hybrid_intent_summary`（main.rs:2426-2473）整条搬进 agent，成 `IntentSummary::merge_hybrid(base, data: Option<&IntentSummary>, kb_cited: bool, unsplit: bool) -> IntentSummary`；`unsplit` 为真时追加 issue `hybrid:unsplit`，KB 半整句检索时追加 `hybrid:kb-unsplit`。② ctx.rs 加纯函数 `pub(crate) fn downgrade_trust(r: &mut AskResult, check: &'static str)`（约 10 行）：把 `trust.level` 压到 `"review"` 并往 `checks` 追一行。③ route.rs 的 merge 出口按 R4 调用：混合容器**永不 verified**（`attach_trust`(ctx.rs:356) 算完后无条件从 `verified` 降到 `high`，理由是容器含模型合成的 `view.insight`），KB 零引用 / data coverage≠complete / 任一半失败三种情况各调一次 `downgrade_trust`。这修的正是「资料侧零引用照样顶着 verified 徽标」—— 今天 `hybrid_intent_summary` 在 `attach_trust` 之后才覆盖 `payload["intent_summary"]`（main.rs:2187），而 `payload["trust"]` 一个字都没动。
   - 验收：纯函数单测：merge_hybrid 五种输入（两半全绿 / data incomplete / kb 无引用 / data 失败 / kb 失败）逐条断言 issues 集合与 status，且必须与 main.rs:2426 改前的输出逐字节相等（先把旧函数的期望值抄成 golden）。trust 单测：kb_cited=false 的混合容器 `trust.level=="review"`；两半全绿的混合容器 `trust.level=="high"`（**不许 verified**）。

4. **server 六入口收口：删第二套编排器（hybrid_payload 全家）**（L；依赖：S3）
   - 文件：crates/server/src/main.rs, crates/server/src/xcx_api.rs, crates/server/src/mcp_api.rs, crates/server/src/deep_api.rs
   - 改法：① 六个入口只保留一个分支：`plan()==Knowledge 且客户端要 SSE` → 现有 `kb_api::spawn_kb_worker`；其余全部 `route::run`。② 删 `HybridAsk`(main.rs:2130)、`hybrid_payload`(:2147)、`hybrid_pair`(:2200)、`hybrid_cardinality_clarification`(:2219)、`hybrid_branch`(:2246)、`hybrid_intent_summary`(:2426)、`hybrid_summary_value`(:2475)、`xcx_hybrid_payload`(xcx_api.rs:678)，约 200 行。③ 删两道**可证明恒真/恒假**的死判据：`prepared_contract_ready`(main.rs:2486)/`intent_contract_ready`(:2490) 及六处调用 —— `ResolvedIntent` 只能由 `ground()` 造，而它已强制「ambiguities 空且 route≠Unknown」，故该函数恒等于 `is_ready()`；`route==Data && !is_data_executable()`（main.rs:2540、:2630、deep_api.rs:4543）里 `is_data_executable()` 的定义就是 `route()==Data`（intent.rs:1231），整个条件恒假。这三处删而不是照 W5#7 抽 dispatch 补等价测试 —— 给死代码建档是反向工作。④ server 的 `ask`(main.rs:2695) / `ask_prepared`(:2769) 两个 wrapper 的 `pg: &PgPool` 形参换成 `owned: &OwnedStore`（`owned.pool()` 现成，deep_api.rs:4645 已这么传），据此构造 `AskDeps.kb`；CLI（main.rs:1149）与四处服务调用点各改一行。
   - 验收：源码扫描断言（扩 main.rs:3645 那条既有题面）：`crates/server/src/` 非测试段不得再出现 `IntentRoute::Hybrid`、`clarification_result()`、`hybrid_`。四个入口各跑一次冒烟：同一句混合问「小虎青菜香菇薄皮包子420g 的信息 和 拆单标准」在 web / 小程序 / MCP / CLI 四条链返回同形 payload（`kb` 键存在、`intent_summary.mode=="hybrid"`）。`tools/regression.py` 79 题零回归。净删行数 ≥ 200（删除 > 新增的账要算给业主看）。

5. **单路失败对用户可见 + analysis_receipt 问题↔素材配对修正**（M；依赖：S4）
   - 文件：crates/agent/src/route.rs, crates/agent/src/compound.rs, crates/server/src/insight_api.rs
   - 改法：① 混合任一半失败时不再只 `tracing::warn`（旧 main.rs:2172/:2179 是 warn + payload 静默换形）：route.rs 的 merge 给 `r.caliber_note` 写一句，文案照 `compound::missing_note`(compound.rs:115) 的既有纪律 —— 点名失败的是数据半还是资料半，并**明说「不是 0、也不是没有数据」**。数据半失败时仍返回 `AskResult` 容器（sql 空、rows 空、`kb=Some(..)`），不像今天那样把顶层整个换成 `Answer`（那连 sql/trust 都没了）。② 两半都在但 `compound::hybrid_summary`(compound.rs:223) 返回 None（＝AnswerContract 把合成结论判为不可证），不许静默缺席综合：写 `caliber_note` 一句「数据结论与资料口径可能不一致，未自动合成，请分别核对」+ 收据 issue `hybrid:synthesis-dropped`。这是断点④「冲突怎么呈现」的最省落法 —— KB 半的版本冲突已由 `answer.rs:895 disclose_versioned_sources` / `:1007 disclose_conflicting_numeric_claims` 出「## 版本与差异」并列表，跨类冲突只需**不替用户选边**，而合成被合同否决恰好就是那个信号。ponytail: 数值级跨类交叉核对（把 KB 正文数字与 data rows 比对）不做，等有一例实测再加。③ `attach_analysis_receipt`(main.rs:2192) 绑定的问题从整句混合原问改成**数据半的生效问句** —— `AnalysisMaterial::from_ask_payload`(insight_api.rs:181) 只收 columns/rows/comparisons/subs，不含 `kb`，今天是「拿不到事实还得下结论」的形状。
   - 验收：单测：mock 一个 KB 半失败的 merge，断言 `caliber_note` 非空且含「资料」二字、且不含「0」的误导表述；断言 `trust.level=="review"`。回归题 X04（S9）钉住端到端。insight_api 单测：混合 payload 签发的收据里 `question` 等于 data 半问句而非整句。

6. **query_log 落账混合/知识轮：路由质量第一次有台账**（M；依赖：S4）
   - 文件：crates/server/src/main.rs, crates/agent/src/answerers/mod.rs
   - 改法：收口后 `route::run` 是唯一出口，把 `query_log::finish`（main.rs:2763 / :2819）提到该出口外层，两个 wrapper 合并成一个。`route` 列写容器的 route：`ROUTE_LABELS`(answerers/mod.rs) 加 `"knowledge"` 与 `"hybrid"` 两个合法值（列早就在，只是纯 KB 轮与混合轮从来没写过 —— main.rs:2280 的注释白纸黑字承认「纯 Knowledge/澄清没有 Data query_log」）。已有的 trust/coverage 列直接承载 S3 合并后的收据。不新建表、不加端点 —— `meta.query_log` 与 `trace_api::conv_trace` 已经在读这几列，改的只是「有没有写」。
   - 验收：跑一轮混合问 + 一轮纯 KB 问，`SELECT route, count(*) FROM meta.query_log WHERE at > now()-interval '10 min' GROUP BY 1` 必须出现 `hybrid` 与 `knowledge` 两行（今天恒为空）。`answerers/mod.rs::route_label_map` 单测扩两个标签后仍绿（它同时守白名单与顺序）。

7. **深度报告吃下 Hybrid，子问 Knowledge 不再静默缺席**（M；依赖：S4）
   - 文件：crates/server/src/deep_api.rs
   - 改法：① 删 deep_api.rs:4587 的 Hybrid 早返（今天用户点了「深度」拿回来的是一份普通混合 payload，没有任何一段报告、也没有一句说明）：主查询走 `route::run`，拿到 `AskResult{kb:Some(..)}` 后把 kb 正文与引用作为**报告的资料章节素材**进既有 section 装配骨架；确实出不了报告时给一句显式说明，不静默换形。② `sub_ask`(deep_api.rs:2180) 的 `Ok(_) => None`（deep_api.rs:2192）改成与 `Err` 同待遇：warn + 该 section 记「素材缺席」。理由与 `compound::missing_note` 同源 —— 一个缺席的章节最容易被读成「那一项是零」，而今天 `Ok(空)` 连 warn 都没有。
   - 验收：深度模式提一个混合问，断言响应含 ≥1 个 section 且资料章节带引用；断言不再返回裸 `hybrid_payload` 形状。构造一个必然命中 Knowledge 的深度子问，断言该 section 出现「素材缺席」标注而不是消失。

8. **回归题集覆盖混合路径（今天结构上测不到）**（M；依赖：S4）
   - 文件：tools/regression.py, tools/regression_cases.json, tools/regression_cases_multiturn.json
   - 改法：S2/S4 收口后 CLI 自动获得两路能力（`AskDeps.kb.owned` 非 Option）。① regression.py 加三个键：`kb_contains`（在 `kb.markdown` 找子串）、`kb_min_citations`（`kb.citations` 条数下限）、`requires_kb`（沿用 `missing_deps` 跳过机制，regression.py:504 那套），`check()`(regression.py:346) 里各约 5 行，`ASSERT_KEYS`/`META_KEYS`(regression.py:77/85) 同步登记否则 preflight 判未知键。② 新增 8 题：K01-K03 纯知识（含一题零引用必须 `coverage_status=blocked`）；X01「…420g 的信息 和 拆单标准」typed 拆开；X02 同题但 subgoals 缺失，断言 `coverage_issues_contains: ["hybrid:unsplit"]` 且两路都出结果、**不出反问卡**；X03 2Data+1KB，断言不再是澄清卡（今天 `hybrid_pair` 只收 1:1，多一个子问整轮澄清）；X04 KB 侧不可达，断言 `caliber_note` 点名 + `trust=review`；X05 混合轮后追问「上月呢」，断言继承到数据半 SQL。素材复用现成 `tools/kb_fixtures` + kb_eval.py 的上传流程。③ selfcheck 补一条源码级断言：`crates/agent/src/ask.rs` 生产段不再出现 `route() != IntentRoute::Data` 早返 —— 防止收口被回退。
   - 验收：`python tools/regression.py --cases tools/regression_cases.json` 79+8 题全绿；`--selfcheck` 绿。反向验伪：临时把 S1 的 Hybrid 臂改回 `_ => Unknown`，X02 必须转红（判据不能是哑测试）。

9. **与自学习的接口：路由决策进台账与偏好计数，但不进 meta.memory**（M；依赖：S6）
   - 文件：crates/semantic/src/ddl.rs, crates/semantic/src/registry/learn.rs, crates/agent/src/route.rs, crates/agent/src/run.rs
   - 改法：断点⑥的答案分两半。**不进经验库那一半（更重要）**：路由决策**绝不**落 `meta.memory` —— 那张表今天零复核门、零回滚面、按 ds_id 召回进**所有人**的 prompt（memory.rs:72 谓词只有 `ds_id + embedding IS NOT NULL`），把「这个用户问 X 通常要问数」写成自然语言塞进别人的提示词，既是跨用户泄漏面又不可回滚。加一条源码守卫：`run.rs` 的 `save_memory` 调用点所在 spawn 块内不得出现 `route::` 或 `IntentRoute`。**进台账那一半**：S6 已让 `meta.query_log.route` 记下每一次路由决策，这就是零成本的路由台账。**偏好计数**：`meta.user_pref(login_name, ds_id, key, value, hit_count, updated_at, PRIMARY KEY(login_name,ds_id,key,value))` 幂等建表进 ddl.rs（形态照 ddl.rs:412 那条 `ADD COLUMN IF NOT EXISTS`），`registry/learn.rs` 一个函数 `bump_pref(pg, login, ds, key, value)`（单条 `INSERT .. ON CONFLICT DO UPDATE SET hit_count = hit_count+1`）；route.rs 的成功出口 fire-and-forget spawn 一次，key 固定 `"route"`、value ∈ {data,knowledge,hybrid}。**读点只有一处，且不选路**：`plan()` 返回 `Clarify` 时，按该用户 `hit_count>=5` 的主导 route 给 `clarify_options` 排序（把可能的那一档 chip 放前面）。R5 是硬线 —— 统计先验证不了归属，用它选路直接破「准确 > 智能」。零 LLM、零向量、可导出、可清空（DELETE 一行谓词）。
   - 验收：`bump_pref` 纯 SQL 单测：同 (login,ds,'route','data') 调两次 → hit_count==2。守卫单测：`run.rs` 的 memory spawn 块内出现 `IntentRoute` 即断言红。`plan()` 单测：同一 Clarify 输入，pref 主导值不同时 `clarify_options` 顺序不同、但 `RoutePlan` 变体**逐字相同**（钉住「只排序不选路」）。手工验：同一账号连问 10 句问数后，一句歧义问的澄清卡里「问数」chip 排第一。

10. **死代码清刀：triage/compound 三段 + 两个恒真判据 + is_data_executable**（S；依赖：S8）
   - 文件：crates/agent/src/triage.rs, crates/agent/src/compound.rs, crates/agent/src/intent.rs
   - 改法：顺序有讲究，**不许先删后补**：S1 的 `HybridUnsplit` 已经把 `triage::unclear_both_hit`(triage.rs:250) 文档里记录的业主 2026-08-11 裁决（「意图不明确时问数与知识库一起查」）落到了 typed 层，此时才可以删。① triage.rs 删 `hybrid_clauses`(:210)、`unclear_both_hit`(:250) 及其词表；② compound.rs 删 `try_compound`(:64)/`split_questions`/`is_compound`，只留 `hybrid_summary` 与 `missing_note`（或整体并入 insight.rs —— 它已持有全部素材，D3 的变更原因本来就是同一个）；③ intent.rs 删 `is_data_executable`(:1231)（定义就是 `route()==Data`，S4 删掉三个调用点后只剩 ctx.rs:369 一处消费者，改用 `is_ready()` 语义更直白）。与 OPTIMIZATION-PLAN W6#5 的删除清单重合，本步只补一条前置条件。
   - 验收：`grep -rn 'hybrid_clauses\|unclear_both_hit\|try_compound\|is_data_executable' crates/` 零命中（含测试）。`cargo test --workspace` 全绿。triage.rs 行数 686→约 200，compound.rs 576→约 90，净删 ≥ 900 行。

11. **文档对账：ARCHITECTURE / AGENT-ARCHITECTURE 写成事实**（S；依赖：S10）
   - 文件：docs/AGENT-ARCHITECTURE.md, docs/ARCHITECTURE.md, docs/PROGRESS.md
   - 改法：① AGENT-ARCHITECTURE §5「单一循环，禁止再造平行编排器」那一行今天是被违反的（第二套编排器住在 server），收口后补一句「唯一编排点＝`agent/src/route.rs::run`，server 只留输出形态」。② §3.2「多实体归属…无法可靠证明时直接澄清，两路执行次数均为 0」按 R3 改成分级口径 —— 那条纪律对 Data 单路成立（错谓词比不答坏），对「问数+资料」两路是过度收紧（资料半根本不吃谓词）。③ ARCHITECTURE §4.6 的 agent 文件表加 route.rs 一行（职责＝一次路由与多路合成）。④ PROGRESS 尾部记 AX118：断点清单逐条对账 + 净删行数。
   - 验收：人工复核。`scripts/check-arch.ps1` 绿（route.rs 落进 agent 预算 ≤15 文件的账要重算一次）。

### 风险
- 【最大的一条】S2/S4 是 L 级双刀且互相咬死：route.rs 落地与 server 删编排器不能分两批上线（中间态会有两条链同时活着，行为发散比现状更坏）。缓解：S2 先只加不删（route.rs 落地 + AskDeps 加字段，server 仍走老路，编译绿即合入），S4 再一刀切换六入口 + 删旧函数。两步之间必须跑一遍 79 题回归。
- AskResult 加 `kb` 字段虽有 skip_serializing_if，但 `AskResult` 是前端 + regression.py + evaluation.py 三方的 serde 契约；`caliber_note_is_omitted_when_absent` 那条既有断言必须原样复制给 `kb`，否则 None 时多出一个恒在的键就是一次形状破坏。confidence=high（本仓已为同类问题立过判据）。
- `plan()` 判据 4（subgoals 空的整句双路）会让今天出反问卡的一批问句开始真执行两路 —— Data 半吃整句可能命中一个「看起来对但覆盖不全」的 SQL。缓解靠 R4（混合容器永不 verified）+ `hybrid:unsplit` 收据 issue，但这条依赖 S3 与 S2 同批上线，顺序颠倒会有一个窗口期出「无标注的整句问数结果」。
- S1 把 `ground()` 的返回类型从 `Option<ResolvedIntent>` 改成带 reasons 的二元组，`ResolvedIntent::project`(intent.rs:237) 与 `intent_from_reply`(intent.rs:1178) 两处调用点必须同改；project 内部还会二次 ground 并校验 route 相等，reasons 在投影后应当**重新计算而非继承**（继承会让单半可执行的分级判据永远判不通过）。confidence=medium，实施时要先把 project 的 ground 语义读清。
- 把 `query_log::finish` 提到 route::run 外层后，纯 Knowledge 轮开始写 query_log —— `trace_api` / `usage_api` / admin 质量页三个下游今天都假设 route 只有 ROUTE_LABELS 那 11 个值（main.rs 有源码扫描断言钉着）。加两个值要同步 web 侧任何按 route 分色/分类的地方，漏一处是显示错误不是数据错误，但会被误读成路由坏了。
- S9 的 requires_kb 题依赖 fixture 文档已上传且 embed 服务在线；判官链路今天没有 KB 前置条件，跳过机制（regression.py:504）若写错会让 8 道新题**静默全跳**而门禁全绿 —— 这正是本仓 `kb_eval.py` 反空转退出码闸防的那件事。必须给 requires_kb 的跳过数打印一行明示计数。
- S10 的 `meta.user_pref` 是本轮唯一新表。若 prime-agent 那条调研提案（用户维度经验分层）先落地，这张表的列清单要与之对齐，否则会出现两张形状相近的用户偏好表。建议两条排同一批，或本轮只建表不建第二个 key。confidence=medium。
- 混合容器永不 verified（R4）会让一批今天顶着 verified 徽标的混合答案降到 high/review。这是修正不是回归，但**用户会感知成「系统变不自信了」** —— 上线说明里要写清，否则会被当成质量下降报上来。


## 对抗验伪结论

- **KEEP** self-learning S1: 经验蒸馏补上 worth_learning 闸 —— 全部引用核实：run.rs:872 确是 `st.route == "llm+repair" && !rs.rows.is_empty()`；worth_learning 在 run.rs:1040 且 `st.note.is_some()` 一票否决；run.rs:884 content 模板确带 `问「{q}」：` 前缀；memory.rs:44 的去重键是独立的 question 列，删前缀不影响去重。两条沉淀路诚实度不一致是真的，改一个判据函数是最短根因修法。
  - 修正形状：引用的测试名 `note_before_learn`(run.rs:1987) 全仓零命中——实际是 `bypass_note_is_set_before_exemplar_learning`(run.rs:1975)。更要紧的是 run.rs:1563 已有 `memory_distill_is_wired_on_repair_success_with_candidate`，它是钉住这一行的源码守卫，必须同刀改，否则本步一落地它当场红。
- **FIX_SHAPE** self-learning S2: owner 三级作用域列 + OWNER_PRED + 六条召回加谓词 —— 符号全对（registry/mod.rs:25 DS_PRED、:34 expand_pred、drift.rs:56、memory.rs:97 score、exemplar 18/171/319）。memory 召回谓词确实只有 `ds_id=$2 AND embedding IS NOT NULL`（memory.rs:70-72），跨用户是真的。但给 meta.sql_exemplar 加 owner 会打断晋升链：admin_api.rs:288 的 EX_VALIDATE_OK_SQL 只改 status/validation_status，不碰 owner——人工复核通过后那条语料仍是 `u:x`，从此**没有任何人**能被它召回，本步的文件清单里没有 admin_api.rs。而 sql_exemplar 本来就有 `status='enabled' AND validation_status='valid'` 人工门（exemplar.rs:20），owner 在它身上基本是第二道重复门。
  - 修正形状：只给 meta.memory（零门）与 meta.pitfall（仅 LLM 门）加 owner，sql_exemplar 不加；若坚持要加，EX_VALIDATE_OK_SQL 必须同刀 `SET owner=''`，且 admin_api.rs 进本步文件清单。另：本步把 OPTIMIZATION-PLAN:651④「visibility 方案已放弃」翻案成「用 owner 兑现」，这是对已排期结论的推翻，要业主点头，不能当增量偷偷做。
- **OVERENGINEERED** self-learning S3: meta.learn_event 账本 + registry/learn.rs + 四写口记账 + 两个 admin 端点 —— 为三张表建一套 before/after jsonb 快照 + 批次 id + 白名单重放回滚引擎 + 两个 HTTP 端点，是给一天几十行的学习写入配了一套微型事件溯源。三张表本来各自都有 created_at，写口全都手里攥着 trace_id（AskCtx.trace_id, ctx.rs:57）。设计自己的 risk 最后一条也承认整行快照会随加列漂，于是又退回「只回滚记过的那几列」——退到那一步，jsonb 快照就没有存在理由了。
  - 修正形状：给 meta.memory 补一列 `trace_id text`（pitfall/sql_exemplar 已有或可同形补），批次＝trace_id，回滚＝一条按 trace_id/created_at 的 DELETE 或 `UPDATE ... SET status=`。零新表、零重放引擎、零新端点，`GET /api/admin/learn` 需要时再用一条现成 SELECT 补。另：本步真要拼 `UPDATE {table}` 就必须往 semantic/tests/drift.rs 的 ALLOW 清单加条目并写明「该值为何不可能来自外部输入」，drift.rs 不在文件清单里。
- **FIX_SHAPE** self-learning S4: failure_streak 频次闸 + run.rs:917 的 wire() 泄漏 —— 两件事绑在一起，一真一错。真的：`grep 'FROM meta.failure_log'` 全仓零命中，累计器确实不存在；run.rs:917 `let sql = scoped.wire().to_string()` 送进 review_failure→LLM→ds 级共享教训，而隔壁 run.rs:873 的经验蒸馏为同一理由已改用 candidate——这是货真价实的 I4 漏洞。错的：`meta.failure_log` **没有 ds_id 列**（ddl.rs:287-294 建表 + :308 只补了 trace_id），log_failure_traced(exemplar.rs:420) 也不写 ds。提案的 `WHERE ds_id=$1` 一跑就是 column does not exist，而设计原话「failure_log 属日志表已豁免 ds 谓词，这里本来就带」是反的：不是豁免，是根本没这一列。
  - 修正形状：拆两刀。刀一（现在就做，S 级、零新文件）：run.rs:917 改 `st.candidate.clone()`，加源码守卫「review_failure 的 spawn 块内不得出现 .wire()」。刀二（failure_streak）：谓词去掉 ds_id，只按 kind + left(error,60) + 时间窗；真要按源统计就先补列 + 补写口。
- **OVERENGINEERED** self-learning S5: meta.user_pref + bump_pref + T_USER_HABITS 段 —— 新表 + 新写路（每轮 spawn）+ 新 PromptCtx 字段 + 新 prompt 段，产出是 ≤120 字、自称「参考，不是硬约束」、明令不许进判据的软提示。业主轴是准确 > 智能，而没有任何一条证据说某题答错是因为模型不知道这人平时按省区拆——grain/breakdowns 本轮问句里已经解析出来了（intent.rs:194/210），要用当轮就有。设计自己的 risk #4 还承认它是新的用户画像面。这条不是缺件，是猜需求。
  - 修正形状：不做。真想量「用户口径偏好值不值得学」，meta.query_log 已有 login_name(query_log.rs:43) 与 question，先跑一条离线 SQL 看有没有稳定模式，有了再谈落表。
- **FIX_SHAPE** self-learning S6: 负反馈接回学习面 + 问数侧与小程序补反馈入口 —— 方向对（👎 只做减法、可回滚、不给加法权限），前端入口确实是真空缺（KbAnswer.vue 独一份）。但提案的 SQL 跑不起来：`RETURNING id, (SELECT …), ds_id` 里的 ds_id 不是 meta.query_feedback 的列——该表定义在 quality_api.rs:14-24，只有 id/trace_id/conv_id/login_name/kind/detail/status/created_at。另，exemplar::set_status(exemplar.rs:258) 是按 `question = $2 AND ds_id = $3` 精确匹配停用，停的是**本问句自己**沉淀的那条语料，不是误导了本次回答的那条 few-shot（后者是 trgm 相似问句），设计文案「这条语料立刻停止当范例」把语义说大了。
  - 修正形状：question 与 ds_id 都从 meta.query_log 取：反馈写入成功后单独发一条 `SELECT question, ds_id FROM meta.query_log WHERE trace_id=$1 AND login_name=$2 ORDER BY id DESC LIMIT 1`，别硬塞进 RETURNING。文案改成「停用该问句自身沉淀的语料」。前端两个入口照 KbAnswer.vue:114-145 抄，这半条最便宜、单独就能落。
- **KEEP** self-learning S7: AskDeps.learn 开关（评测/判官不再灌学习表） —— 污染是实的：CLI `ask`(main.rs:1152-1180) 与 eval_batch_one(main.rs:506) 都直调生产 `ask()`，run.rs 的 save_exemplar / save_memory / log_failure_traced 一个不落；regression.py 79 题正是走这条 CLI。改在入参而不是调用点，符合根因治理。bool 不是 trait，不撞 D7。
  - 修正形状：三处 `&& cx.learn` 可以收成一处：S1 之后 worth_learning 已是语料路与经验路的共同判据，把 learn 判进 worth_learning 就覆盖两条沉淀路，只剩失败复盘 spawn 需要单独一处。设计自己 risk #5 承认的「靠人记得改调用点」天花板照记。
- **FIX_SHAPE** self-learning S8: 复核调度（embed_fill 加第二个后台循环） —— 事实对：review-pending/review-lessons 只在 main.rs:754/764 当 CLI 存在、全仓无 cron；embed_fill.rs:22-41 的 advisory-lock + 循环形态现成可抄。但设计说「人工 VQR 那道门不动，自动化的只是把候选送到人面前」——对 review_all_pending 成立（review_exemplar 只写 ai_review/disabled），对 **review_lessons 不成立**：它按 LESSON_SYSTEM 的一行 verdict 直接 `set_lesson_status(… enabled)`（review.rs:101），候选教训就此进 recall_pitfalls 的 `status='active'` 召回，是纯 LLM 授权。今天没人跑＝这条路从未生效，一开就是每小时自动往召回里加东西。
  - 修正形状：depends_on 从「步骤 3」改成「步骤 2」（owner 必须先在，教训才只毒到本人）。或者本步只挂 review_all_pending，review_lessons 留 CLI/admin 手动，等 owner 列落地再自动化。质量页露出 pending/candidate 计数这半条无条件保留。
- **KEEP** self-learning S9: fewshot 相似度下限 —— 逐字核实：exemplar.rs:18-38 的 fewshot 只有 `ORDER BY word_similarity($1, question) DESC LIMIT 8` 再 `.take(2)`，无任何门槛；gather.rs:738 的 fewshot_text 只判空，非空就冠上「相似问题的正确写法（参考口径）」标题；兄弟读路 cache.rs:22 MAX_DIST=0.12 是三关。不对称成立。与 W3-5 确实不同文件不同读路（那条改 semantic/src/recall/schema.rs 的 trgm_tables），不算重复。一个常量 + 一个谓词，代价最小、收益直指准确轴。
- **KEEP** self-learning S10: 学习增益题集 + expected_case + 文档订正 —— regression.py 的 `--cases` 确实现成（:65 起，相对路径按 ROOT 解析），脚本零改。反向枪测（把学到的教训 disable 掉题集必须转红）是防恒绿题集的正确做法。ARCHITECTURE §8「明确不采用」留痕是零成本的防重复立项。
  - 修正形状：expected_case 依赖 S3 的 learn_event 列；S3 若按建议瘦身成 trace_id，题名就挂在题集文件里而不是库里（本来也够）。文档那半条的措辞取决于 S2 最终范围：owner 若只落 memory+pitfall，:63/:96/:181/:184/:210/:333 六处必须照实写，不许写成三张表全兑现。
- **KEEP** kb-upgrade K1: 删 kb.chunk.ts 生成列 + idx_kb_chunk_ts —— 死件坐实：`to_tsvector|ts_rank|tsquery` 在 crates/**/*.rs 只命中 retrieve.rs:179/:180/:816/:817/:2017 五处**注释**，零 SQL 读者；0020:336 生成列、:370 GIN 索引确在；ts 是 kb.chunk 唯一的 GENERATED 列，所以 store.rs:2329 那条断言换成反向断言不会误伤别的东西。纯删除，删除 > 新增，验收「删前删后 kb_eval 逐题字节一致」是正确的空转判据。
- **FIX_SHAPE** kb-upgrade K2: KB 追问改写守卫（把 SQL 形状的跳过条件改成上一轮是不是 KB） —— 断链是真的：ask.rs:1583-1591 的 `hist_sql.is_none() && company_span(prev_q).is_none() && !(explicit_reference && …)` 三条同时成立就原样返回，制度问句正中三条；chat.rs:193-196 的注释自己写着「上一轮走了知识库 → 没有 sql 键」。但两处要改：① ④ 那条新增提示词规则**已经在了**——ask.rs:1600 的规则 5 逐字就是「上一轮没有 SQL 时，只继承上一问明确出现的实体或主题，不得补造数据指标、时间或筛选口径」，再加一句是同义重复；② PrevTurn 扩成 5 元组与 OPTIMIZATION-PLAN W5#4 正面撞车——那条已经把第 5 位分配给 `&[IntentSlotSummary]`。
  - 修正形状：删掉 ④。PrevTurn 一次到位改具名 struct（W5#4 自己的验伪意见也是「5 元组已逼近可读上限」），两条合并成一次改动打到 main.rs:2301/2710 + CLI/xcx/deep/mcp；不许同一段代码搬两遍。守卫本体（约 15 行）保留。
- **KEEP** kb-upgrade K3: parse_engine / chunk_preset 落库 —— 逐条核实通过：ParsedDoc(doc.rs:27-36) 带 `#[serde(default)]`，加 engine 字段老服务不会反序列化失败；`_pdf_text_engine`/`_ocr_engine` 在 embed_service.py:716/:721；CAPS(:777) 每个扩展名的第 2 位都是非空能力名串（'pdf'/'docx'/'text'/'image'/'doc'…），设计自标 confidence=low 的那点核对通过；kb_api.rs:547/605/640/676/696 五处 `preset: None` 各带一行自认「有意简化」的注释。chunk_preset 不落库导致 laws/qa 上传后一 reprocess 就变 general，这是准确性缺陷不是运维缺陷，判断正确。
- **FIX_SHAPE** kb-upgrade K4: kb.doc_blocks 旁表（换 preset 不重跑 OCR） —— 问题真实（扫描件换分块档要重跑按页付费的 vision OCR），但落法是本方案唯一的新表，存的是数 MB jsonb，还要配一根拍脑袋的 8MB 保险丝、一个 engine 失效键，以及设计自己 risk #2 承认的「上线后看一周 pg_total_relation_size」。为了一个纯缓存往 PG 里塞大对象，是往最贵的存储里放最不需要事务的东西。
  - 修正形状：解析产物写磁盘，不进 PG：原文件已经在 kb_root 下按 doc_id 存着，旁边落一个 `<doc_id>.blocks.json`（engine 写进 json 头）。零 DDL、零表膨胀、零 8MB 保险丝，文档删除时跟原文件一起删。reprocess/build_shadow 读它的逻辑与提案完全一样。
- **KEEP** kb-upgrade K5: 九路 census + vector_degraded 进 meta.query_log —— 落点核实无误：answer.rs:194 的 run() 手里就有 report（`report.stats` / `report.vector_degraded` 在同一 match 臂里），返回值本来就是 (out, obs) 二元组，确实一个形参都不用加；Obs 在 qa_log.rs:22、entry 在 :84、CHANNEL_NAMES 是 `[&str; 9]`(retrieve.rs:1463)。零迁移、不碰 kernel::qalog 共享列清单、前端零改。「这题为什么没命中」是 KB 运营最高频问题，成本对得起。
  - 修正形状：qa_log::finish 有两个调用点（answer.rs:106 与 :185，后者是流式路走 respond_stream）。只改 run() 会让流式轮的 routes 恒 [0;9]，两处都要覆盖。
- **FIX_SHAPE** kb-upgrade K6: kb_eval 加负样本档（nohit_acc） —— 这是整份 KB 方案里最硬的一条诊断，且经得起核：meta.kb_eval_runs 只有 recall1/3/5/10 + answer_acc（kb_eval_api.rs:88），五个指标全部单调偏好多召回，而 VEC_MAX_DIST=0.55(:222)/TERMS_MIN_HITS=2(:73)/TRGM_MIN=0.2(:192) 三条注释都自陈「调松会先打死近域 nohit」——今天调松任何一个，没有任何判据会红。但选的负题语料来源（该 viewer 可读的**另一个**空间）是脆的，设计自己 risk #5 就承认生产上很可能只有一个空间、nohit_acc 恒 NULL，那正好又变成一条恒不生效的判据，与它要修的病同种。
  - 修正形状：直接采用它自己的备选方案当主方案：用同空间内 effective_to 已过期 / enabled=false 的文档出题（SAMPLE_SQL 只需去掉生效期 conjunct）。那批文档检索侧结构上不可见，是天然负样本，不依赖第二个空间、无需 run.error 兜底文案。另：kb_eval_api.rs 已 1158 行破 D2，两档必须各落新文件，不许推到 1300 行。
- **FIX_SHAPE** kb-upgrade K7: kb_eval 加多跳档（W4#10 的量具） —— 排序论点完全正确（用单跳题量只在多跳生效的路，结论必然是「没收益」然后误删），必须排在 W4#10 之前。但实现引用是错的：doc_graph.rs:798 的 `mention_pairs` 行号对，语义不对——它的签名是 `(pg, space_id, doc_ids, entity_ids, chunk_ids, limit)`，且 :806 三个集合任一为空即返空，它是**对已遴选集合取 MENTIONS 明细**的原语，不是「找共享实体的 chunk 对」的原语。要拿它出题得先跑 chunk_nodes + entities_of_chunks 两趟 AGE，再把上千个 id 内联进 Cypher 串，量级和注入面都不对。
  - 修正形状：不碰图谱：`kb.chunk.terms`（jieba 词集合列，0020 里已有 GIN 索引）一条自连接就能取「跨 doc、共享 ≥1 低频词」的 chunk 对。零 AGE 往返、零新原语，而且在没建图的空间也能出题（提案里那条『未建图退回单跳』的分支随之消失）。gold_chunk_id2 + multi_recall5 两列照原样。
- **KEEP** kb-upgrade K8: kb_eval_cases.json 16→28 + CI 门禁 —— 题集实测 16 题 + 10 夹具（tools/kb_eval_cases.json），扩到 28 且明说「六个 kind 判据补齐即止」是有边界的。kb_eval.py 头部 :14-25 的 0/1/2 三态退出码与反空转闸确在，`不带 --allow-skip` 进 CI 是对的。这是全仓唯一会红的 KB 判据链，扩它比扩任何检索机制都值。
  - 修正形状：依赖链要写实：+2 多轮题依赖 K2、+3 多跳题依赖 K7 的 gold 形状、+2 半覆盖题依赖 W4#1。任一未落地就把对应题标 skip 并打印计数（照 kb_eval.py 自己的反空转纪律），不许静默全跳还全绿。
- **KEEP** kb-upgrade K9: ACL 生命周期守卫（新增 crates/knowledge/tests/acl_drift.rs） —— 缺口真实且是越权族：acl.rs:405-408 白纸黑字自陈「片段只管谁能看，生命周期过滤是每个内联者自己的义务」，而 visible_docs! 全仓 21 处内联（retrieve 6 / kg 6 / store 6 / acl 自身 3），今天只有 retrieve.rs:2091/:2115/:2226 与 kg.rs 四条逐函数手列断言——明天新加一条检索 SQL 抓不到。约 30 行、照 semantic/tests/drift.rs:24 的 sources() 形状、纯增测试零行为改动，「不扫 store.rs」的理由（管理面 CRUD，属主看得见自己停用的文档才对）也站得住。
- **KEEP** kb-upgrade K10: 文档留痕 yuxi.json we_are_stronger + ARCHITECTURE §8 —— 零代码。docs/research/yuxi.json 实测确只有 summary/repo/mechanisms/half_baked 四键，加第五键无冲突；acl.rs:21-24 那段「不搬 Yuxi 的 min(授予,角色上限)」裁决确实只活在源码注释里，文件一重构就没了，补进 ARCHITECTURE 是对的。§8 记「明确不采用」防下一轮重复立项，成本近零。
- **BREAKS_INVARIANT** router R1: route() 补 Hybrid 臂 + IntentAttempt 第四态 Ambiguous —— 两处硬伤。① 前提被读漏了：IntentV1::route()(intent.rs:411) 第一步先走 `route_from_subgoals`，`(true,true) => IntentRoute::Hybrid`(intent.rs:1127)——Hybrid 今天是活的（main.rs:2352 / xcx_api:464 / mcp_api:436 都在跑，intent.rs:2331/2404 有断言）。落到 :433 那个 match 的只剩「mode=hybrid 但没有任何 Knowledge 子任务」，也就是**恰好一点资料侧证据都没有**的那一档。给它补臂，直接推翻 :410-411 写着的纪律「只有已 grounding 的 typed subgoal，或可执行槽位，才能成为路由证据；单独的模型 mode 不足以把一个无槽位问句强行分到 Data」。② ground() 改成返回 (ResolvedIntent, reasons) 会开一个 fail-open：ResolvedIntent::project(intent.rs:262) 靠 `.ground(question)?` 兜底，而 project 内部 :625/:629 push 的正是「未找到匹配的结构化子任务」「归属不唯一」——今天这两条 push 就是投影 fail-closed 的机制。ground 一旦对有歧义的意图返回 Some，IntentAttempt::project 就返回 Ready，混合的一半会带着证不出的归属去执行。设计 risk #4 摸到了边（confidence=medium）但没堵上。
  - 修正形状：删掉 Hybrid 臂；「mode=hybrid 但无 typed subgoal」维持澄清，这是归属证明缺席而不是路由缺陷。Ambiguous 第四态可以留（它只服务澄清文案），但 ResolvedIntent::project 必须显式保持「reasons 非空 → None」，且 reasons 在投影后重算不继承。R4/R5 不需要本步，别把它当前置。
- **KEEP** router R2: 新建 route.rs（plan 纯函数 + run 多路并行 + merge） —— 「server 里住着第二套编排器」是实的：main.rs:2130-2478 的 HybridAsk/hybrid_payload/hybrid_pair/hybrid_cardinality_clarification/hybrid_branch/hybrid_intent_summary/hybrid_summary_value 约 200 行 + xcx_api.rs:678 再抄一份，而 AGENT-ARCHITECTURE:115 明写「单一循环，禁止再造平行编排器」。D6 过关：agent 的 Cargo.toml 已经依赖 dms-knowledge 与 futures，KbDeps 不引任何新东西；db.rs:161 的 kb_rrf_weights、main.rs:1149 的 owned 都现成，owned 非 Option 结构性堵住「CLI 少一路」是对的。AskResult 加 `kb` + skip_serializing_if 与 ctx.rs:1-6 的 serde 纪律一致。
  - 修正形状：ask.rs:216/:222 那两道早返不要降成 debug_assert!——ask_prepared 是 pub，release 下 debug_assert 不跑，等于把一道 fail-closed 闸换成注释（I3）。留着它们，成本是零。
- **KEEP** router R3: 收据合成搬进 agent（IntentSummary::merge_hybrid + downgrade_trust） —— 缺陷坐实：main.rs:2185-2187 的 hybrid_intent_summary 在 attach_trust 之后才覆盖 payload["intent_summary"]，而 payload["trust"] 一个字都没动——资料侧零引用（issues 里明明有 `hybrid:knowledge:no-citation`）照样顶着 attach_trust 算出来的等级。搬进 agent 后收据与 trust 同源。「混合容器永不 verified」的理由（容器含模型合成的 view.insight）站得住，与 ctx.rs:344 trust_level 的既有分档不冲突。golden 值对拍旧函数是正确的搬运验收。
- **KEEP** router R4: server 六入口收口 + 删 hybrid_payload 全家 + 删两道死判据 —— 死判据两条都验过。① `intent_contract_ready`(main.rs:2489) ≡ `is_ready()`：ResolvedIntent 只能由 ground()(intent.rs:801) 铸造，而它已强制 ambiguities 空且 route≠Unknown，所以 `ready().is_some_and(ambiguities.is_empty()) && route()!=Unknown` 对 Ready 恒真、对另两态恒假。② `route == Data && !is_data_executable()`（main.rs:2540 / :2630 / deep_api.rs:4542）恒假：is_data_executable() 的定义就是 `self.route() == IntentRoute::Data`(intent.rs:1231)。给死代码抽 dispatch + 补等价测试（W5#7 的做法）确实是反向工作，本步删而不抽是对的。净删 ≥200 行。
  - 修正形状：排期照设计 risk #1：R2 先只加不删（route.rs 落地 + AskDeps 加字段，server 仍走老路，编译绿即合入），R4 再一刀切换六入口，中间跑一遍 79 题。本步不依赖 R1。
- **KEEP** router R5: 单路失败对用户可见 + analysis_receipt 问题↔素材配对 —— 三处都核过。① main.rs:2170-2183：KB 半失败只 warn 后退化成纯问数 payload，问数半失败则把顶层整个换成裸 Answer（sql/trust/intent_summary 全没了），用户看不出少了一半。② compound::missing_note(compound.rs:115) 是仓内已有的文案纪律，照它写不新造。③ insight_api::from_ask_payload(:181) 只投影 columns/rows/comparisons/subs 白名单，不含 kb，拿整句混合问去签一份只有数据半素材的收据确实是「拿不到事实还得下结论」。hybrid_summary 返 None 时不静默缺席、只出信号不替用户选边，是 I5 与「不头疼医头」的正解。
- **FIX_SHAPE** router R6: query_log 落账混合/知识轮 —— 知识那一半**已经在做了**。`qalog::ROUTE_KNOWLEDGE = "knowledge"`(kernel/src/qalog.rs:22)，knowledge/src/qa_log.rs:184 每次 KB 作答都 `.bind(qalog::ROUTE_KNOWLEDGE)` 写 meta.query_log；/api/ask 的 Knowledge 分支经 kb_answer → answerers::knowledge::answer → answer::answer → qa_log::finish 同样落账。quality_api.rs:70-72 的注释还专门写着「问数行与 KB 文档问答行（route='knowledge'，Y2 起由 knowledge 层落账）走同一条通道」。所以验收「今天恒为空」对 knowledge 是假的，往 ROUTE_LABELS 加 "knowledge" 也与 kernel 那个常量成了第二份真相源。缺的只有 `hybrid`：今天一个混合轮产出两行（数据半一行 + 知识半一行），没有容器行。
  - 修正形状：只加 `hybrid` 一个值，且 route 常量取 kernel::qalog 的口径而不是在 answerers/mod.rs 再定义一份。main.rs:2280 那句注释同刀改成事实（它说的是「没有 Data query_log」，不是「没有 query_log」）。
- **KEEP** router R7: 深度报告吃下 Hybrid + sub_ask 的 Ok(空) 不再静默 —— 两处都实：deep_api.rs:4587 的 Hybrid 分支直接返 `hybrid_payload` 的结果，用户点了「深度」拿回一份普通混合 payload，零 section 零说明；deep_api.rs:2192 的 `Ok(_) => None` 与隔壁 `Err` 分支不同待遇，连 warn 都没有，缺席的章节最容易被读成「那一项是零」。后者两行就能修，与 compound::missing_note 同源。
  - 修正形状：拆两刀：sub_ask 的 warn + 「素材缺席」标注是 S 级，先落；「kb 正文进 section 装配骨架」是 L 级的报告改造，藏在 M 里，单独排。
- **KEEP** router R8: 回归题集覆盖混合路径 —— 机制都在：regression.py 的 `--cases`(:65)、ASSERT_KEYS/META_KEYS 白名单收口(:77/:85)、check() 单一入口(:346)、requires_embed/requires_graph 跳过(:504-508)。加三个键各约 5 行且必须同步登记否则 preflight 硬失败——这条纪律脚本自己就写着，设计照做了。收口后 CLI 自动获得两路能力（R2 的 owned 非 Option），混合路径今天结构上测不到是真的。
  - 修正形状：X02（subgoals 缺失走整句双路）建立在 R1 的 Hybrid 臂上；R1 若按建议撤掉，X02 一起去掉，其余 7 题不受影响。requires_kb 的跳过数必须打印明示计数——否则 8 道新题静默全跳而门禁全绿，正是 kb_eval.py 反空转闸防的那件事。
- **OVERENGINEERED** router R9: 路由决策进台账与偏好计数（meta.user_pref） —— 分两半，一半白送一半是猜的。白送的：路由决策绝不落 meta.memory 的源码守卫——memory.rs:70-72 的召回谓词确实只有 ds_id + embedding IS NOT NULL，把「这个用户问 X 通常要问数」写成自然语言塞进所有人 prompt 是真泄漏面，这条守卫零成本；query_log.route 也本来就是零成本台账。猜的：为了给澄清卡上两个 chip 排序，建一张新表 + 每次成功出口 spawn 一次写入 + 一个 top_prefs 读函数。澄清卡本身就是系统已经失败的现场，把 chip 换个顺序不解决准确性，而设计自己的 R5 硬线还规定它「不参与 plan()」——一个不参与决策的持久化统计，就是待清理的表。且与 self-learning S5 是同一张表的两份提案。
  - 修正形状：留守卫 + 留 query_log 台账，删 meta.user_pref。真需要「这个用户常走哪条路」，query_log 的 (login_name, route) 一条 GROUP BY 就有，不必建第二份事实源。
- **KEEP** router R10: 死代码清刀（triage/compound 三段 + is_data_executable） —— 零调用点逐个验过：`try_compound`(compound.rs:64)、`hybrid_clauses`(triage.rs:210)、`unclear_both_hit`(triage.rs:250) 在自己文件之外全仓零命中（只剩各自的单测）；`is_data_executable`(intent.rs:1231) 的定义就是 route()==Data，R4 删掉三个调用点后只剩 ctx.rs:369 一个消费者。triage.rs 685 行 / compound.rs 576 行，净删 ≥900 行，删除 > 新增。与 W7#6 重合但设计自己声明只补前置条件，没有重复立项。
  - 修正形状：「不许先删后补」的顺序纪律要当硬约束执行：triage.rs:250 的 unclear_both_hit 文档里记着业主 2026-08-11 的裁决，删它之前那条裁决必须在 typed 层有落点——R1 若被撤掉，这条前置就不成立，unclear_both_hit 得留到有替代落点为止。
- **KEEP** router R11: 文档对账（AGENT-ARCHITECTURE / ARCHITECTURE / PROGRESS） —— AGENT-ARCHITECTURE:115「单一循环，禁止再造平行编排器」今天确实是被违反的（第二套编排器住在 server），收口后补「唯一编排点＝route.rs::run」是把文档写成事实。§3.2 的「无法可靠证明就澄清」按半边分级也讲得通：那条纪律的成本论据是「错谓词比不答坏」，资料半根本不吃谓词，对它照抄是过度收紧。零代码。
  - 修正形状：§3.2 的改写只在 R3 的「混合容器永不 verified」+ `hybrid:kb-unsplit` 收据同批落地时才成立；R1 若撤，「整句双路」那一档随之不写进文档。
- **KEEP** self-learning S1：经验蒸馏补 worth_learning 闸 + 删 content 里的「问「{q}」：」前缀 —— 实测成立且是全批性价比最高的一条。run.rs:873 确为 `st.route=="llm+repair" && !rs.rows.is_empty()`，而 memory.rs:72 的召回谓词只有 `ds_id + embedding IS NOT NULL`——挂了 caliber_note 的 SQL 被冠以「正确写法」进同源**每一个**用户的 prompt（gather.rs:167/478）。同函数隔壁 :863 已用 worth_learning，两条沉淀路两种诚实度是根因不是调用点。改动 ~3 行，直接打准确轴。
- **OVERENGINEERED** self-learning S2：owner 三级作用域列（''/u:/d:）+ OWNER_PRED 单一拼接点 + 六条召回加谓词 —— 真缺口只有一张表一条谓词。exemplar 三条读 SELECT（exemplar.rs:21/174/319）已全带 `status='enabled' AND validation_status='valid'`，pitfall 读路（recall/pitfall.rs:32）已带 `status='active'`——这两张表的跨用户闸就是既有人工/复核门，加 owner 今天买不到任何东西。唯一无门的是 meta.memory。`d:<dept>` team 层零消费者（没有晋升 UI、没有需求方），是为以后搭脚手架。OWNER_PRED 仿 DS_PRED 造带 $Q/$U/$D 占位的拼接常量，服务两个调用点，抽象比被抽象的多。
  - 修正形状：只给 meta.memory 加一列 `login_name text NOT NULL DEFAULT ''`（''=存量共享层，非空=本人私有），recall_memories 谓词加 `AND (login_name='' OR login_name=$3)`，两个 gather 调用点（:167/:478）传 cx.p.login_name，写侧 run.rs 的 save_memory 传本人。不建 team 层、不给 exemplar/pitfall 加 owner、不造 OWNER_PRED 常量。drift.rs 加五行守卫：`FROM meta.memory` 后 8 行内必须出现 login_name。约 30 行 vs 原案八文件。ARCHITECTURE 的 VIS_PRED 六处按此改写（正好落回 W7#16④ 的原结论口径，不必改成「三级作用域」）。
- **OVERENGINEERED** self-learning S3：meta.learn_event 账本 + registry/learn.rs + 四写口记账 + 两个 admin 端点（含 rollback） —— 新表 + 新文件 + 两个端点 + 白名单重放，服务的是一个没有事故记录的运维问题，而方案自己写着「几千行量级，清理是运维一条 DELETE」。方案还自认 confidence=low 后把 rollback 从整行快照降级成「只回滚记过的那几列」——降级之后剩下的语义就是 DELETE。S1 落地后只有过闸的 SQL 才蒸馏，S2-corrected 落地后爆炸半径只剩本人，「怎么撤这一批」的答案就是一条 DELETE。
  - 修正形状：给 meta.memory 与 meta.sql_exemplar 各加一列 `trace_id text`（幂等 ALTER，写侧各一行）。它把「这条是哪一轮学的」接上 meta.query_log 已有的 login/route/status/trust 全部上下文，撤销＝`DELETE FROM meta.memory WHERE ds_id=$1 AND created_at > $2`。约 6 行，零新表零端点。真出现「一批学坏了查不出来」的现场再谈账本。
- **FIX_SHAPE** self-learning S4：failure_streak 频次闸（新建 registry/failure.rs，streak<2 不调模型） —— 两半价值差一个量级。真缺陷是 run.rs:917 `let sql = scoped.wire().to_string()` 喂进 review_failure → LLM → meta.pitfall（ds 级共享，pitfalls_sql 无用户维度）：行级权限条件进共享教训，而同函数 :869 的经验蒸馏为**同一理由**已经用了 candidate。这条一行、有兄弟先例、破 I4，必须做。频次闸那半在优化一条今天没有消费者的队列：候选教训要转 active 只有 review_lessons，而它全仓零调度（main.rs:767 是 CLI 子命令），候选池根本没人取。省 LLM 钱的主张也无实测量。
  - 修正形状：只改一行：run.rs:917 的 `scoped.wire()` 换 `st.candidate.clone()`，log_failure_traced 那两行保留 wire()（排障取证不喂 LLM），并在 exemplar.rs:420 的 doc 上标明「本表含注入后条件，任何送进 LLM 的读路必须先剥」。run.rs 加源码守卫：review_failure 所在 spawn 块内不许出现 `.wire()`。删掉 failure.rs 与 streak 判据；真想知道复盘烧了多少钱，先跑一条 `SELECT count(*) FROM meta.failure_log WHERE kind='exec-error' AND created_at > now()-interval '7 days'`，零代码。
- **OVERENGINEERED** self-learning S5：meta.user_pref + bump_pref + prompt.rs 的 T_USER_HABITS 段 —— 说不出哪一类问句从错变对。产出是一段「参考，不是硬约束」、≤120 字、排在最弱档位的提示，模型可用可不用；而代价是一张新表、一条新写路、一个新 prompt 段，外加方案自认的用户行为画像面。更糟的是它与准确轴反向：region 槽位会把「按省区」写进 prompt，而省区是本仓明码红线（W6#7「默认下钻维度池含省份——直接撞省区红线」）。用统计先验去改下一次取数的口径，正是「智能」压「准确」。「学错了怎么办」的答案只有 hit_count>=3 + 90 天窗，回答不了「这个用户上季度常看省区、这次问的是客户」这种确定性误导。
  - 修正形状：不做。真想要个性化就用零新表的那一半：exemplar.rs:171 suggest_questions 的冷启动 chip 按 meta.query_log（已有 login_name + question）排一次序——用户在 UI 上看得见、不进 prompt、不改口径。等有人真反馈「它老忘了我按省区看」再谈。
- **FIX_SHAPE** self-learning S6：负反馈接回学习面（👎 自动 disable 语料）+ ResultPanel/小程序补反馈入口 —— 前端那半是本区最实的缺口：grep 确认 /api/feedback 全仓唯一调用点是 web/src/KbAnswer.vue:139，问数面板与小程序零入口——整条学习飞轮的**输入端**在主产品面上不存在。自动 disable 那半方向反了：一次匿名 👎 就静默改掉一条已过 VQR 人工验证的语料，而这是跨全体用户的共享状态；quality_api.rs:155 的 admin 质量页本来就是这个动作的人类执行者。方案给它配的回滚依赖 S3 的账本，而 S3 已判过度设计。
  - 修正形状：只做前端两端入口：ResultPanel.vue 与 xcx_api 复用 KbAnswer.vue:114-145 的两键 + localStorage 形状，POST 同一个 /api/feedback（trace_id 已在收据里），零新端点。quality_api summary 加一个 open 反馈计数，让「没人处理」变成看得见的数字。不接自动 disable——一个用户的点击不许静默改写所有人的语料库。
- **KEEP** self-learning S7：AskDeps.learn 开关（评测/判官不再灌学习表） —— 污染实测成立：main.rs:537 的 eval_batch_one 直调生产 ask()，regression 79 题 + evaluation --runs 3 一趟最多 237 轮全写进 sql_exemplar(pending)/memory/failure_log，而 memory 那条路零复核门、600s 后被 embed_fill 自动向量化随即进所有人 prompt。判官的 SQL 变成真实用户的 few-shot，直接打准确轴。改法落在入参而非调用点，一个 bool + 两个 CLI 分支，已是最省形状。
  - 修正形状：保留改动，删掉「影子对照 / AB」那层包装——这个开关的工作是止污染，不是做实验载体，把它讲成对照组会诱使下一轮去搭分桶。AskDeps 字段 doc 上写明「默认 true 靠人记得改新调用点」这条已知天花板（方案自己的风险 5）。
- **BREAKS_INVARIANT** self-learning S8：embed_fill 加第二个 spawn 每 3600s 跑 review_all_pending + review_lessons —— 方案写「人工 VQR 那道门不动（AI 仍只能 pending→disabled，不许自授 enabled）」——这对 exemplar 成立，对 lessons **不成立**。实测链路：review_lessons(review.rs:80) → parse_verdict(review.rs:189) 可返 `"active"` → set_lesson_status(exemplar.rs:387) 无条件 UPDATE meta.pitfall.status → recall/pitfall.rs:32 的召回条件正是 `WHERE status='active'`。把它挂进每小时循环，等于让一发 Fast LLM 成为「什么话进 ds 内每个用户 prompt」的唯一闸门，全程无人。这同时推翻方案自己的硬线「public 层的门就是既有人工复核」。
  - 修正形状：只做可见性那半：quality_api summary 加 pending/candidate 两个计数（约 10 行），让「队列没人处理」变成 admin 页上的数字。要挂调度就先把 review_lessons 的自授权面关掉——parse_verdict 的 "active" 出口改成 "reviewed"（新状态，不进召回），人工确认才转 active；这一改本身要单独立项、单独枪测。在此之前不许接自动调度。
- **KEEP** self-learning S9：fewshot 加相似度下限 FEWSHOT_FLOOR —— 实测成立：exemplar.rs:18 是 `ORDER BY word_similarity($1, question) DESC LIMIT 8` 后 .take(2)，无任何门槛；渲染侧 gather.rs:738 只判 rows 非空就出「## 相似问题的正确写法（参考口径）」标题。语料库非空 ⇒ 任意不相干历史 SQL 被冠以「正确写法」送进 precise 模型。兄弟读路 cache.rs:22 有 MAX_DIST=0.12 + 时间词/数字词全等三关，对称性明显破了。一个常量 + 一个 SQL 条件，「先量后定」的纪律也对，且可与 W3#5 共用同一次分布测量。
- **FIX_SHAPE** self-learning S10：regression_cases_learned.json 题集 + expected_case 绑定 + 文档订正与「明确不采用」留痕 —— 题集那半值钱且几乎零成本：纯 JSON，regression.py:65 的 --cases 已支持，且它是「学习到底有没有让哪道题从红变绿」的唯一可证伪判据，反向枪测（disable 掉那条教训必须转红）的验收也写对了。expected_case 那半依赖 S3 的 learn_event（已判过度设计）。文档那半与 W7#16④ 重叠，且结论要跟着 S2-corrected 改口径。
  - 修正形状：保留 tools/regression_cases_learned.json（3-5 题）+ 反向枪测验收；删掉 expected_case 列与 admin 返回体绑定——题名写进题集文件本身即可。文档只留两笔：ARCHITECTURE §8 加「明确不采用」两行（prime-agent 的 RLM/持久 IPython/子 agent/daemon；自动向共享层写入），VIS_PRED 六处的改写并进 W7#16④ 一次做完（按 S2-corrected 的 memory.login_name 口径），不要在两个批次里改同一段文档。
- **KEEP** kb-upgrade K1：删 kb.chunk.ts 生成列 + idx_kb_chunk_ts GIN 索引 + 反向断言 —— 实测零读者：`to_tsvector|ts_rank|tsquery` 在 crates/**/*.rs 只命中 retrieve.rs:179/816/2017 三处**注释**，而那三处正是「中文 FTS 322 格恒 0、已被单号/型号 ILIKE 路替换」的实测证据。每块多存一份 tsvector、每次写多维护一个 GIN、换 0 次读，还长得像「FTS 路还在」。纯删除，反向断言（0020 不得再出现 tsvector）也是对的。保留 retrieve.rs 三处注释这一条尤其对——它是「别再加中文 FTS 路」的唯一证据。
- **KEEP** kb-upgrade K2：KB 追问改写守卫（prev_is_kb 进 PrevTurn，把 SQL 形状判据换掉） —— 实测成立：ask.rs:1583 的守卫是 `hist_sql.is_none() && company_span(prev_q).is_none() && !explicit_reference`，KB 轮三条全中 ⇒ 原样返回，第 2/3 轮进 retrieve 的是碎片。而系统提示词 ask.rs:1599 的规则 5「上一轮没有 SQL 时，只继承上一问明确出现的实体或主题」正是为这种情况写的，今天结构上不可达——改的是那一个守卫，不是调用点。三端产品的三分之一（知识库）结构上单轮，是用户能直接说出来的缺陷。风险面也确实小：改写歪了只会让 keep_cited_only 判无据回「没有相关内容」。
  - 修正形状：PrevTurn 别再加第 5 位——D4 明令「共享状态一律命名，禁止连排」，4 元组已经在边上。直接换成具名 struct（6 个构造点各改一行，和加一位元组同价），这样 W5#4「上一轮结构化意图跨轮继承」落地时不用把同一段代码搬第二遍。方案自己的风险 4 也是这么写的，把它从「本轮不换」改成「本轮就换」。
- **KEEP** kb-upgrade K3：kb.doc 加 parse_engine / chunk_preset 两列（解析档位与分块 preset 落库） —— chunk_preset 那半是准确性缺陷不是运维问题：kb_api.rs:546/674 白纸黑字承认 preset 不入库，于是用户选的 laws/qa 在第一次自愈或 reprocess 后恒变 general，条文级召回静默降级。parse_engine 那半让 tier-2/3 解析（heading_path 恒空 → embedding 配方章节行同值、TITLE_SQL 的 heading_path 半路失效、引用说不出章节、导图塌一层）从无声变可查，而它复用同一条 ALTER、同一个 doc_cols! 宏、同一次 DocRow 改动——合在一起做才是省的做法。ParsedDoc(doc.rs:28) 有 #[serde(default)]，老解析服务不带 engine 键不会炸，兼容判断成立。
  - 修正形状：KbPanel.vue 那一格列表展示可以砍——两列先只服务重跑清单那条 SQL 和四处 reprocess 读回，UI 等有人问再加。
- **OVERENGINEERED** kb-upgrade K4：kb.doc_blocks 旁表缓存解析产物（换 preset 不重跑 OCR） —— 投机性。K3 把 chunk_preset 落库之后，「莫名其妙重解析」这条路本身就没了，剩下的只有管理员**主动**改分块策略这种低频动作。为它建一张存数 MB jsonb 的新表、配一个方案自认「拍的不是量的」8MB 保险丝、外加上线后盯一周表增长——ladder 第一级就该停：没有任何一次实测的 OCR 重跑账单。
  - 修正形状：不做。K3 加的 parse_engine 列正好是量具：一个月后跑 `SELECT count(*) FROM kb.doc WHERE parse_engine LIKE '%ocr%'` 与 reprocess 次数对一次账，非零再谈缓存。
- **FIX_SHAPE** kb-upgrade K5：KB 落账带检索证据（Obs 加 routes[9] 九路 census + vec_down） —— 两个问题混在一起。「这周多少答案产在 embed 熔断期」是队列级、必须能查、只需一个 bool；「这题为什么没命中」是逐例问题，而 /api/kb/search 诊断 JSON（kb_api.rs:2320）已经能重放。九路 census 的代价是把日志格式钉死在 CHANNEL_NAMES 的顺序上，而 W4#7 正要删 ext_kb（9 路收成 8 路），做完就得改一次；产物还是塞进 sql 列的一串「v3 e0 tg2…」，trace/usage 两个页面上是天书。
  - 修正形状：只留 vec_down：Obs 加一个 bool，qa_log::entry 在 sql 摘要前缀「｜降级:向量」，约 5 行、零迁移、可 LIKE 查。删掉 routes[9] 与 route_census 纯函数。等真有一次「没命中查不出原因」的现场且诊断端点不够用，再谈落 census——那时 W4#7 也已经把路数定下来了。
- **FIX_SHAPE** kb-upgrade K6：kb_eval 加负样本档（kind 列 + nohit_acc，跨空间出题） —— 病因诊断对了一半：kb_eval_api 的五个指标（recall1/3/5/10 + answer_acc）确实全部单调偏好多召回，调松 VEC_MAX_DIST 只会让数字变好。但「今天没有任何判据会红」不成立——tools/kb_eval_cases.json 实测 16 题里已含 nohit×2，那条链会红，方案自己也称它是「唯一会红的那条链」。真缺口是自助评测面缺负样本。跨空间取语料这个做法方案自己的风险 5 已经把它证伪了：生产很可能只有一个空间 ⇒ nohit_acc 恒 NULL ⇒ 又一个恒真判据，正是它要治的病。
  - 修正形状：负题不跨空间：直接用本空间内被生效期闸挡住的文档（retrieve.rs:735-745 已经让 effective_to 过期/effective_from 未到的文档对检索不可见）出题——它天然是负样本，单空间可用，还顺带把 K8 想加的「+2 生效期」题合并成同一件事。同样两条幂等 ALTER、同样纯函数判据（citations 空 ∨ 含 NO_HIT，不调 judge），但删掉「无第二空间」的降级分支和它的页面文案管线。
- **OVERENGINEERED** kb-upgrade K7：kb_eval 加多跳档（sample_pairs + GEN_SYSTEM_MULTI + gold_chunk_id2 + multi_recall5） —— 「用单跳题量只在多跳生效的路会误删能力」这个论证本身是对的，但结论跳错了。被量的对象——KG-PPR 召回——按 W4#10 自己的描述是「默认恒缺席」，即今天零用户受益，而建图要按 chunk 烧 2000 次 Fast LLM。为一个默认关闭、无人打开、建设成本明码的能力，先花 M 级工程造一套专用量具去论证该不该删它，是拿工程给延期做背书。
  - 修正形状：不建量具。W4#10 的省事解法：默认关闭 + 2000 次 Fast LLM 建图成本 ⇒ 删。真有人要留，举证责任归他。要证据就用 K8 里那 3 道多跳题（纯 JSON，判据是 citations 覆盖两篇）——DMS_KG_RETRIEVAL 开关一开一关跑两遍，同样能看出图谱路有没有收益，零代码。
- **KEEP** kb-upgrade K8：kb_eval_cases.json 16→28 题 + CI 门禁（不带 --allow-skip） —— 本区性价比最高：纯 JSON + 夹具 + 一行 CI，扩的是全仓**唯一会红**的那条链，六个 kind 判据补齐即止的自我克制也对。反向枪测（短路 keep_cited_only 后 cite/nohit 两族必须红）是真验收不是哑测试。
  - 修正形状：多跳那 3 题的判据改成「citations 覆盖两篇不同 doc_id」，不依赖 K7 的 gold_chunk_id2——这样它对 kb_eval.py 是纯数据，K7 砍掉也不掉链。生效期那 2 题与 K6-corrected 的负样本语料是同一件事，合并计。
- **KEEP** kb-upgrade K9：crates/knowledge/tests/acl_drift.rs 生命周期守卫（约 30 行） —— 安全边界的 fail-open 且无用户可见症状，正是必须装守卫的那一类。实测缺口成立：retrieve.rs:2100 那条断言用的是硬编码的 `for sql in [VEC_SQL, EXACT_SQL, TRGM_SQL, TITLE_SQL, METADATA_SQL]` 五元素清单——明天新加一条检索 SQL，这个 for 循环抓不到它。acl.rs:405-408 自述「生命周期过滤是每个内联者自己的义务」，义务今天靠作者记得。30 行、照 semantic/tests/drift.rs:24 的 sources() 形状、不扫 store.rs（管理面 CRUD 该看得见自己停用的文档）——边界也划对了。
- **KEEP** kb-upgrade K10：docs/research/yuxi.json 加 we_are_stronger + ARCHITECTURE §8 一行 + acl.rs 裁决上浮 —— 零代码，防的是下一轮重新立项抄回净退化的六条。acl.rs:21-24 那段「不搬 Yuxi 角色上限」的裁决只活在源码注释里、文件一重构就消失，上浮到 §4.5 是对的。audit_trace.py exit 0 的验收也让新增引用不会静默腐烂。
  - 修正形状：we_are_stronger 六条各留一行「Yuxi 做法 → 我方 file:line → 不抄」即可，别把对照写成小论文；§8 那行是载重部分，其余是注脚。
- **FIX_SHAPE** router R1：route() 补 (Hybrid,true) 臂 + IntentAttempt 第四态 Ambiguous(ResolvedIntent, reasons) —— 两半价值差得远。Hybrid 臂那半实测成立且一行：route_from_subgoals(intent.rs:1121) 只在 typed subgoals 齐备时给 Hybrid，`mode=hybrid + 有槽位 + subgoals 空` 落 intent.rs:436 的 `_ => Unknown` → 反问卡；而模型说了「两件事都要」是已表达的事实。第四态那半代价被低估：ground() 返回类型要改（Option<ResolvedIntent> → 带 reasons），project()(intent.rs:238) 内部二次 ground 还得**重算**而非继承 reasons（方案自己标 confidence=medium），换来的只是澄清卡文案更具体 + 复活一段死码。而 reasons 的素材（intent.ambiguities）今天就在手里，只是被 ground 丢掉。
  - 修正形状：① 保留 `(IntentMode::Hybrid, true) => IntentRoute::Hybrid` 一行。② 第四态不加，改成给现有变体挂载荷：`IntentAttemptState::Invalid` → `Invalid(Vec<String>)`，validated() 在 ground 返 None 时把 intent.ambiguities 顺手存进去，user_note() 据此说出「客户名不唯一」。零 ground 签名变更、零 project 重算、零半边分级。③ 连带删掉 R3 的「归属分级 R3 单半可执行」与 plan() 判据 5——它们只在 Ambiguous 存在时才有意义。
- **KEEP** router R2：新建 route.rs（plan 纯函数 + run 多路并行 + merge）+ AskDeps.kb + AskResult.kb —— 第二套编排器实测住在 server：hybrid_payload 全家（main.rs:2130-2480）+ xcx_hybrid_payload + deep_api.rs:4587 各自拼一遍，六个入口各自 match IntentRoute。CLI 结构上拿不到 KB 路（AskDeps 无 owned），所以 regression.py 79 题**测不到混合路径**——这是「回归跑的不是生产链路」的同一族病。plan() 做成零 IO 纯函数是唯一能给路由判据上单测的形状，路由正确性直接在准确轴上。owned 用非 Option 而不是 Option，结构上堵住「CLI 少一路」，判断也对；答案侧 answerers/knowledge::answer 已在 agent（main.rs:2845 只是薄壳），搬迁面比看上去小。
  - 修正形状：分两批的顺序按方案风险 1 执行不可省：S2 先只加不删（route.rs 落地 + AskDeps 加字段，server 仍走老路，编译绿即合入），S4 再一刀切六入口。两步之间必须跑一遍 79 题。AskResult.kb 的 `skip_serializing_if` 要照抄 caliber_note_is_omitted_when_absent 那条既有断言，否则 None 时多一个恒在的键就是一次 wire 形状破坏。
- **KEEP** router R3：hybrid_intent_summary 搬进 agent 成 IntentSummary::merge_hybrid + downgrade_trust + R4「混合容器永不 verified」 —— 用户可见的错误徽标。实测：main.rs:2187 在 attach_trust **之后**才覆盖 payload["intent_summary"]，payload["trust"] 一个字都没动 ⇒ 资料侧零引用的混合答案照样顶着 verified。这是「说自己很确定但半边没证据」，正踩准确轴。改法是搬既有函数 + 一个约 10 行纯函数，收据合成从此和 trust 在同一处出口，属于根因治理不是调用点补丁。golden 对拍（先把旧函数输出抄成期望值）也是对的验收。
  - 修正形状：上线说明必须写一句：一批今天显示 verified 的混合答案会降到 high/review。这是修正不是回归，但不写清会被当成质量下降报上来（方案风险 8 已列，把它从风险提到发布动作）。
- **FIX_SHAPE** router R4：server 六入口收口 + 删 hybrid_payload 全家（约 200 行）+ 删两个恒真判据 —— 收口与删 hybrid_* 全家成立且是本批最大的一笔净删。但「删两个恒真判据」这一刀方向反了：prepared_contract_ready(main.rs:2486) 守的是**客户端送来的** req.intent（forced chip），它今天恒真只是因为 ground() 当前保证「ambiguities 空且 route≠Unknown」——而这同一批方案（R1）恰恰在提议放宽 ground()。在放宽合同的同一批里删掉那条依赖旧合同才冗余的边界检查，是把安全网和跳板一起拆。`route==Data && !is_data_executable()` 三处确实恒假（is_data_executable 的定义就是 route()==Data，intent.rs:1231），那三处可删。
  - 修正形状：保留 prepared_contract_ready / intent_contract_ready（客户端可控输入上的唯一显式闸，两个函数不到 10 行），只删三处恒假的 `route==Data && !is_data_executable()`。源码扫描断言照加：server 非测试段不得出现 IntentRoute::Hybrid / clarification_result() / hybrid_。净删账仍在 190 行以上。
- **KEEP** router R5：单路失败对用户可见（caliber_note 点名）+ 综合被合同否决不静默 + analysis_receipt 问题↔素材配对修正 —— 三条都是静默降级，且形状退化实测成立：main.rs:2172 知识半失败只 warn + payload 悄悄换成纯问数；:2179 问数半失败时顶层整个换成 Answer，连 sql/trust 都没了——用户拿到一份看不出缺了什么的答案。缺席的那半最容易被读成「那一项是零」，这正是 compound::missing_note(compound.rs:115) 已确立的纪律，此处只是补上同族落点。attach_analysis_receipt 拿整句混合问绑 data-only 素材（AnalysisMaterial::from_ask_payload 不含 kb）也是确凿的问题↔素材错配。全部落在 R2 的 merge 出口，零新机制。
  - 修正形状：「数值级跨类交叉核对」明确不做那条要写进注释（方案已写 ponytail 标记，保留）。合成被否决时只出一句 caliber_note + 一条 issue，不替用户选边——不要顺手加冲突裁决逻辑。
- **FIX_SHAPE** router R6：query_log 落账混合/知识轮（ROUTE_LABELS 加 knowledge / hybrid） —— 前提有一半被证伪。「纯 KB 轮从来没写过 query_log」不成立：crates/knowledge/src/qa_log.rs 文件头第一行就是「一次知识问答一行 meta.query_log（route='knowledge'）」，且 qa_log.rs:245 有源码守卫钉着 `.bind(qalog::ROUTE_KNOWLEDGE)`。所以 knowledge 标签早就在写，也早就在被 trace/usage/质量页消费。真缺的只有混合容器那一个标签。方案据此推出的「路由质量第一次有台账」也就缩水了——今天 `SELECT route, count(*)` 已经答得出 knowledge 占比。
  - 修正形状：缩成一件事：ROUTE_LABELS 加 "hybrid" 一个值，route::run 的混合出口写它，query_log::finish 提到该出口外层。别宣称建立新台账。同刀核一遍 web 侧按 route 分色/分类的地方（漏一处是显示错误），以及 main.rs 那条钉 ROUTE_LABELS 的源码断言。
- **FIX_SHAPE** router R7：深度报告吃下 Hybrid（KB 正文成报告资料章节）+ sub_ask 的 Ok(空) 不再静默 —— 缺陷成立：deep_api.rs:4587 的 Hybrid 分支直接调 crate::hybrid_payload 原样返回——用户点了「深度」，拿回来的是一份普通混合 payload，没有任何一段报告、也没有一句说明。sub_ask 的 `Ok(_) => None` 与 Err 同待遇也对（缺席章节最容易被读成零）。但把 kb 正文与引用装进既有 section 骨架，是在一个 7462 行的文件里加一种新章节类型，那是功能不是修缺陷，且没有需求方。
  - 修正形状：只做两条诚实度修正：① Hybrid 走深度时返回一句显式说明「深度报告暂不支持混合问句，已按问数+资料并列给出」，不静默换形；② sub_ask 的 Ok(空) 加 warn + 该 section 标「素材缺席」。约 20 行。「KB 成为报告资料章节」等有人真提这个需求再做，届时也该排在 deep_api 拆分之后。
- **KEEP** router R8：regression.py 加 kb_contains / kb_min_citations / requires_kb 三键 + 8 道混合与知识题（K01-K03、X01-X05） —— R2 收口的唯一可证伪判据。反向验伪写得对（把 Hybrid 臂改回 `_ => Unknown`，X02 必须转红），这条让题集不是哑测试。素材复用现成 kb_fixtures + kb_eval.py 上传流程，脚本改动只有约 15 行 python。selfcheck 补的源码级断言（ask.rs 生产段不再出现 route()!=Data 早返）防收口被回退，也是便宜的。
  - 修正形状：requires_kb 的跳过必须打印明示计数——方案风险 6 说的正是 kb_eval.py 头部那条反空转闸防的事：8 道新题静默全跳而门禁全绿，比没有这 8 题更坏。这一条是硬要求不是 nice-to-have。
- **OVERENGINEERED** router R9：meta.user_pref（key='route'）+ bump_pref + Clarify 分支 chip 排序 + 「路由不进 meta.memory」源码守卫 —— 新表换一个澄清卡上的 chip 顺序，用户说不出哪里变好。台账那半 R6 已经免费给了（query_log.route）。「路由决策绝不落 meta.memory」的源码守卫更是给一个没人提过的做法立哨——防的是自己想象出来的风险。另注：本轮设计稿里 self-learning S5 与本条各要一张 meta.user_pref，两条都没有用户可感知的产出，两张形状相近的画像表更是方案风险 7 自己点名的坏味道。
  - 修正形状：整条删。真要按用户历史排 chip，用 meta.query_log（已有 login_name + route）一条查询，零新表——且这属于 UI 微调，排到有人开口再说。
- **FIX_SHAPE** router R10：死代码清刀（triage::hybrid_clauses/unclear_both_hit + compound::try_compound/split_questions/is_compound + intent::is_data_executable） —— 前四项实测确为死码：hybrid_clauses/unclear_both_hit 只在 triage.rs 内部出现（定义+注释+测试，零生产调用点），try_compound 更有 ask.rs:2229 的守卫断言生产段不含它。删这批是本批最大净删。但 is_data_executable **不是**死的：grep 实测三个 agent 消费者（answerers/cache.rs:86、ctx.rs:367、run.rs:1385）+ deep_api.rs:4543，且 intent.rs:2897-2898 有两条源码守卫**断言** cache.rs 与 run.rs 必须含 `cx.intent_attempt.is_data_executable()`。方案说的「S4 删掉三个调用点后只剩 ctx.rs:369 一处」把 server 侧的三处和 agent 侧的三处搞混了。
  - 修正形状：删 triage 两个 + compound 三个（并按方案的顺序前提：R1 的 Hybrid 臂先把 2026-08-11 那条业主裁决落到 typed 层，再删 unclear_both_hit）。is_data_executable 原样保留——它有三个活消费者和两条守卫钉着，删它会当场编译红+测试红。净删行数据此重算（triage.rs 实测 685 行、compound.rs 576 行）。
- **KEEP** router R11：文档对账（AGENT-ARCHITECTURE §5 单一编排点 / §3.2 归属分级口径 / ARCHITECTURE §4.6 加 route.rs / PROGRESS AX118） —— 零代码，且 §5「单一循环，禁止再造平行编排器」今天字面被违反（第二套编排器住在 server），收口后必须把文档写成事实，否则下一个人读文档定位代码又走错。check-arch.ps1 的 agent 文件预算要重算一次也记对了。
  - 修正形状：§3.2 那条改口径要跟着 R1-corrected 收窄：Ambiguous 四态不做 ⇒ 不写「按半边分级」，只写「Data 半证不出即 fail-closed，KB 半不吃谓词故整句检索并在收据打标」。别把没落地的分级写进权威文件。