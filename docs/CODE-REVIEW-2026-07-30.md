# dms-ai 全局代码审查报告（2026-07-30）

> 审查方式：8 路并行深审（架构合规 / kernel / connector+policy / semantic / agent+knowledge / server / 前端三端 / 文档配置参考对齐）+ 本地构建验证 + 关键 P0 人工复核。
> 基线：`cargo check --workspace --all-targets` 全绿（仅 1 个 noop-clone warning）；`pwsh scripts/check-arch.ps1` 13/13 绿；kernel 110 测试绿。
> 重要前提：**整个 6-crate 重构目前全部未提交**（HEAD 停在 M9j，`git status` 大量 M/D/?? 文件，含 .gitignore 的关键修改）。**修任何问题之前先提交当前工作树**，否则修复与存量改动混在一起无法 review。
> 每条问题给出 file:line 证据与修法。标注【已复核】的是主审查人二次验证过的；其余为子审查证据，修前请再读一遍对应代码确认。
> 文末「已验证无问题的面」列出核对为健康的部分——**不要顺手改它们**，避免引入回归。
> **历史快照**：本文记录 2026-07-30 当时的审查发现，行号、部署状态和修复状态可能已变化；
> 当前约束以 `docs/CONFIG.md` 与 `docs/ARCHITECTURE.md` 为准。历史凭据仅保留泄漏结论，不保留原值。

---

## 一、P0：安全 / 红线（最优先）

### P0-1 JOIN ON 子查询是权限注入与 fail-closed 的双重盲区【已复核】
- 位置：`crates/kernel/src/policy/inject.rs:100-191`（`inject_select`/`subqueries_of`）、`:204-232`（`count_expr_subqueries`）。全文 grep 无 `join_operator`/`JoinConstraint`。
- 问题：注入只覆盖 TableFactor + 投影/WHERE/HAVING/GROUP BY 四处子查询；**JOIN 的 ON 约束表达式两条路都不走**，且计数器同样看不见 ON → `SubqueryNotCovered` 对拍也不触发，**未登记表 fail-closed 在此同样失效**。
- 复现（受限用户，`secrets` 未登记）：
  ```sql
  SELECT o.cust_code FROM orders o
  JOIN bills b ON b.order_code = o.order_code
    AND EXISTS (SELECT 1 FROM secrets s WHERE s.owner_id = o.owner_id AND s.salary > 50000)
  ```
  `secrets` 不注入任何条件、不报 `UnregisteredTable` → 存在性探测 oracle（越权读）。**这是唯一在 MySQL 主链路今天就可构造利用的红线漏洞。**
- 修法：`subqueries_of` 与 `count_expr_subqueries` 对称遍历 `sel.from[].joins[].join_operator` 的 `JoinConstraint::On(expr)`（只走表达式不走 relation，避免与派生表双注入）；`Query` 级 `order_by` 同理；加「ON 里有子查询必须注入」回归测试。

### P0-2 系统库 deny-list 用子串匹配，一个空格/反引号/双引号即绕过【已复核】
- 位置：`crates/kernel/src/sql/guard.rs:89-95`（`system_schema_ref`，判据为 `stripped.contains("information_schema.")` 等子串）。
- 问题：以下三种形态都能通过 `is_safe_select_with`：
  ```sql
  SELECT * FROM information_schema . tables   -- 点号两侧空格
  SELECT * FROM `kb`.`chunk`                  -- 反引号使子串 "kb." 不存在
  SELECT * FROM "meta"."chunk"                -- lex.rs:17 把 " 当字符串引号剥掉
  ```
  受限用户有 inject 的 `UnregisteredTable` 兜底，**裸奔的是 proof/unrestricted 路径与字符串级 `rewrite()`**：免注入档可读 `mysql.user` 密码哈希。F3 修法②「无条件拒绝非业务 schema」合同字面失效。
- 修法：判定移到规范化文本（剥反引号、折叠 `\s*\.\s*` → `.`）或 AST 层（遍历 `ObjectName`，段数 ≥2 判首段；`table_names_of` 已拿复合名）。文本扫描留作兜底。补三条构造 SQL 的回归断言（现有 `rejects_system_schema` 只测了无空格形态）。

### P0-3 `SELECT … INTO [TEMP] TABLE` 过闸：PG 源上的真写操作
- 位置：`crates/kernel/src/sql/guard.rs:65-73`（`forbidden_token` 词表有 `outfile`/`dumpfile` 无 `into`）。
- 问题：sqlparser 0.53 对**所有方言**把 `SELECT * INTO TEMP TABLE t2 FROM t` 解析为 `Statement::Query`（`Select.into`），单语句 ✓、Query ✓、词表无命中 ✓ → 全线放行。DMS MySQL 不支持该语法（只是报错），但 **PG 默认把 TEMP 授给 PUBLIC**，「只读角色」照样能建临时表——只读红线在 PG 上传源被实质绕过。
- 修法：AST 层拒——遍历 Query，`Select.into.is_some()` 即 `GuardError::WriteToken("into")`；补 PG/MySQL 双方言回归测试。

### P0-4 F6 完全未落地：few-shot 与语义缓存跨用户明文泄露（三个审查面独立命中）
- 位置：
  - DDL：`crates/semantic/src/ddl.rs:74-89`——`meta.sql_exemplar`/`meta.pitfall` **无 `visibility`/`owner_login` 列**【已复核：grep 0 命中】；
  - 召回：`crates/semantic/src/registry/exemplar.rs:16-28`（`fewshot`，谓词仅 `question != $1 AND status != 'disabled'` + ds——**pending 未复核也进召回**）、`:98-117`（`nearest`）；`crates/semantic/src/recall/pitfall.rs:21-23`；
  - 消费：`crates/agent/src/gather.rs:304-318`（few-shot 直接渲染进 prompt）、`crates/agent/src/answerers/cache.rs:42-66`（缓存命中 SQL 进答案展示）；
  - 复核判词：`crates/agent/src/review.rs:31-33`（`EXEMPLAR_SYSTEM` 无「含客户名/人名/金额一律判 disabled」）；
  - 守卫缺失：`crates/semantic/src/registry/mod.rs:23` 只有 `DS_PRED`，全仓无 `VIS_PRED`；drift.rs 无对应守卫。
- 后果：用户 A 的问句原文与 SQL（含客户编码/人名/金额）以两种形态到用户 B：① few-shot 塞进 B 的 LLM prompt；② 语义缓存命中后作为 B 答案里展示的 SQL。权限本身不旁路（cache 回放按当轮用户重新注入，这一半是好的），泄露的是明文。ARCHITECTURE §3 F6 自认「本轮一并修」，实际一行未修。
- 修法：两表 `ADD COLUMN visibility text NOT NULL DEFAULT 'private'` + `owner_login`；registry 加 `VIS_PRED` 与 `DS_PRED` 同一拼接点；`fewshot`/`nearest`/`recall_pitfalls` 统一拼 `VIS_PRED`（`owner_login=$n OR visibility='public'`）；晋升 public 只在复核通道；`EXEMPLAR_SYSTEM` 加隐私判据；drift.rs 加第三条守卫。

### P0-5 凭据与认证防线：文档明文口令、git 历史口令、生产配置无认证回退（合并四个审查面的发现）
- **a) 生产凭据曾明文进入 git 跟踪文档**：DMS 联调口令、旧 SM4 常量和企业微信标识均曾被记录。→ 从文档删除并**轮换**；历史记录只保留事故结论。
- **b) PG 超管旧口令曾进入 git 历史**（当前文档已脱敏），且旧 compose 曾把 `15433:5432` 绑定到 **0.0.0.0**。→ 轮换口令；compose 改由 gitignore 配置注入，并把发布面收为 `127.0.0.1:15433`。
- **c) `settings.docker.json` 的 .gitignore 保护只在未提交的工作树里**【已复核：`git diff .gitignore` 显示 `settings.docker.json` 与 `.venv/` 是未提交新增】；该文件含真实 MySQL 口令、DeepSeek key、pg_ro 口令。任何基于 HEAD 的提交操作都可能把它提交进去。→ 先提交 .gitignore。
- **d) 生产容器配置开着无认证回退**：`settings.docker.json` = `insecure_login_fallback: true` + `listen: 0.0.0.0:8100`，是 docker 部署的唯一配置模板（`scripts/serve.ps1:42` 挂载）。`crates/server/src/db.rs:78-95` 注释自认「开了等于没有认证，冒充 admin 返回 true」。当前仅靠 serve.ps1 的 127.0.0.1 端口映射兜底；换部署方式（compose/k8s/0.0.0.0 映射）即局域网人人可 `curl -d '{"question":"…","login_name":"admin"}'`。且 **CONFIG.md 对该键零记载**。→ 拆 `settings.prod.json`（fallback 缺省 false）；CONFIG.md 补警告条目；部署流水线断言生产配置不含此键。

### P0-6 AGE 图查询 `esc()` 不防 `$$`：向可写自有库注入 SQL
- 位置：`crates/connector/src/graph.rs:24-26`（`esc` 只删反斜杠、转单引号，`$` 原样通过）、`:134-167`（三个图查询把实体名拼进 `$$ ... $$` 包裹的 cypher）；实体名提取 `crates/server/src/direct.rs:1033`（`strip_relation_words` 只剥中文关系词，`$$`/`--`/空格保留）。执行池是 `OwnedStore::pool()`（**可写**）。
- 载荷示例：`x$$ ) AS (code agtype, name agtype) UNION SELECT ... -- ` 闭合 dollar-quote 后续写 SQL（sqlx 扩展协议挡多语句，挡不住 UNION 与 PG 数据修改 CTE）。后果：读 `kb.chunk`/`chat.msg`（F3 要防的跨用户数据）+ 任意删改自有库。
- 缓解：`agent/src/answerers/graph.rs:68-70` 的 `accept` 要求 `has_proof`，仅免注入身份可达——但这只是一层运行时布尔。
- 修法：①实体名白名单（拒 `$`）；②三个图查询函数加 `_proof: &UnrestrictedProof` 形参（ARCHITECTURE §4.2 契约本来就要求「查询要求 &UnrestrictedProof」，实现缺）；③顺手补图查询超时（三条 cypher 无 `tokio::time::timeout`）。

---

## 二、P1：正确性 bug

### 数据正确性 / SQL 管道
1. **LIMIT 护栏被行尾注释绕过，且 `inject` 重渲染把补上的 LIMIT 整个丢掉** — `kernel/src/sql/guard.rs:164`（`ensure_limit_with` 字符串追加）+ `kernel/src/policy/inject.rs:61-65`（AST 重渲染丢注释）。`SELECT * FROM t_sales_order; -- 统计订单` → LIMIT 落进注释 → 重渲染后 wire 串**完全无 LIMIT** → 生产库无界扫描。unrestricted 路径同样裸奔。修法：追加前剥行尾注释，或改 AST 置 `q.limit` 后 `to_string()`；最低成本是追加后 re-parse 断言 `limit.is_some()` 否则拒。加回归测试。
2. **「近N天」窗口宽 N+1 天** — `kernel/src/nl/time.rs:186-191`：`rule_recent_n` 产 `[CURDATE()-N, CURDATE()+1)`，「近7天」= 8 个自然日，与两种主流口径（含今天 7 天 / 过去 7 个完整天）都不符。BI 最高频入口的系统性错数。需先裁决口径（注意这是继承自旧 direct.rs 的行为，改它要同步 golden 与评测），再连 golden 一起改。
3. **`SetExpr::Table` / `SetExpr::Values` 不注入、不 fail-closed** — `kernel/src/policy/inject.rs:96`（`_ => Ok(())`）+ `kernel/src/sql/guard.rs:141`。PG `TABLE t` 命令与 `VALUES ROW(...)` 落在通配臂：受限用户 `TABLE secrets` 不注入不报未登记。当前伤害低（追加 LIMIT 使 PG 语法报错、门面写死 MySQL 方言），但是 fail-closed 的结构性破洞。修法：`Table` 按实表走档案（查不到 → `UnregisteredTable`），`Values` 直接 `Err(NotSelect)`。
4. **失败路径把注入后的 SQL（`scoped.wire()`）喂给 LLM 并落库** — `crates/agent/src/run.rs:338`（zero-rows）、`:353-359`（exec-error 的 `log_failure`+`review_failure`）。`run.rs:242-243` 自己写明「candidate 是闸门前原文，不能换成注入后的 wire()：那会把权限条件教给 LLM、也会沉淀进语料」——但失败三条路传的全是 `scoped.wire()`（含受限用户的 `owner_manager IN (7,8,9)` 清单）。权限条件 → review_failure 的 LLM prompt → lesson 通道 → 复核 active 后召回给其他用户（叠加 P0-4）。修法：三处改传 `st.candidate`。
5. **policy `to_rule` 对 scoped 档案两列全 NULL 不拒 = 静默 Global，受限用户整表放行** — `crates/policy/src/rules.rs:100-107`：`mode="scoped"` 且 `customer_col`/`owner_col` 同 `None` 的 Binding 进 `build_condition` 恒返 `None` → 零条件注入。同文件 via 臂缺列是「跳过=fail-closed」，scoped 臂却放行，与 I3 及文件注释直接矛盾。触发面：`meta.scope_binding` 一行坏数据。修法：scoped 臂校验两列全 None → 返 None（跳过=该表被拒）。
6. **pitfall 召回失败会打死整轮问答** — `crates/agent/src/gather.rs:65`：`recall::recall_pitfalls(...).await?` 用 `?`，而同函数其余六路召回全部 `map_err(warn).unwrap_or_default()` 降级，文件头注释（:44-50）明写「失败一律降级成这张卡缺席」。PG 抖一下整轮 500。修法：改成同款降级；`gather_warns_on_every_recall_degradation` 断言条数 +1。
7. **分块 token 按 1.6 字符/token 估算，实测纯中文 ≈1.0——满块尾部被 bge 512 窗口静默截断** — `tools/embed_service.py:589`（`CHARS_PER_TOKEN = 1.6`）、`:671-676`（target_chars=640）。用生产同款 tokenizer 实测：中文散文 0.97 字符/token → 640 字中文块 ≈640 token > 512，**尾部 ~128 token 不进向量**；`_emit` 的 assert 用估算值挡不住。这正是 ARCHITECTURE §5 chunk 契约要防的「检索时好时坏」。修法：`CHARS_PER_TOKEN` 降到 ≤1.1 或用 tokenizer 真计数（fastembed 进程内就有），自检断言改真实计数 ≤512。
8. **fewshot/nearest 召回失败完全静默** — `crates/semantic/src/registry/exemplar.rs:27`（`.unwrap_or_default()`）、`:115-116`（`.ok().flatten()`）均无 `tracing::warn!`。对照同 crate `recall/schema.rs:90`、`recall/cards.rs:187` 都有 warn 且有源码守测试钉着「静默降级不许回来」。修法：两处加 warn + 同款源码守测试。

### server / API
9. **会话删除假成功（F8 明判未修）** — `crates/server/src/main.rs:1079-1080`（`let _ = chat::delete_conv(...)` 恒返 `{ok:true}`）+ `crates/server/src/chat.rs:149-156`（不查 `rows_affected`）。ARCHITECTURE §3 F8 与 §4.7 都明判「删除按 `rows_affected==0→403`」，同仓 `admin_api.rs:69` 与 `kb_api.rs:265-268` 已落地，唯独 conv 这条漏。修法：`delete_conv` 返 `rows_affected`，`==0` → 403/404。
10. **CLI 分派无兜底：子命令输错/少参 → 静默启动服务器** — `main.rs:235-639` 的 if 链全部不命中则无条件落到 serve。`dms-ai-server ask zhangsan`（少一参）不报错不退码直接建池起服务——判官/运维参数写错时症状是「进程挂着不退出」，与文档痛斥的「宽容解析=假绿」同形。修法：if 链末尾加 `else { eprintln!(用法); exit(2) }`。
11. **graph sync 时区：容器里「本地 03:00」= 北京时间上午 11:00** — `main.rs:1085`（`chrono::Local::now()`）+ `docker/server/Dockerfile` 无 `TZ`（Debian 默认 UTC），serve.ps1 也无 `-e TZ`。注释声称凌晨低谷全量重建 ~4min，实际落在业务高峰。修法：Dockerfile 加 `ENV TZ=Asia/Shanghai`。
12. **`/api/ds/{id}/sync` 的拒绝理由已过时，非 dms 源采集被焊死** — `crates/server/src/ds_api.rs:183-199`：注释声称 table_doc/column_doc 还没有 ds_id 列，故 `id != 'dms'` 一律 422；实际 ds_id 化早已落地（`ddl.rs:252-253`、`schema_sync.rs:100-108` 按 ds 限定），且 `main.rs:324`、`kb_api.rs:152` 已对非 dms 源跑 `sync_schema`。修法：删 422 分支与过时注释。
13. **上传 handler 先读全量 body 再认证** — `crates/server/src/kb_api.rs:69-73`：`UPLOAD_GATE.try_acquire()` → `read_form`（multipart 全量入内存）→ 第 73 行才 `viewer()` 认证。未认证请求可让服务器把最多 50MB×4 并发读进内存后才 401。修法：`viewer()` 提到 `read_form` 之前（identity 走 header/query）。
14. **企微 OAuth 回调无 state 校验（login CSRF）** — `main.rs:793-796`（`WeworkQuery { code }`，无 state），`docs/EMBED.md:48` 的授权链接模板也没有 state。攻击者用自己的 code 诱导受害者打开回调 → 受害者浏览器 302 到 `/#token=<攻击者token>`，此后以攻击者身份问答。修法：发起端生成 state 存 cookie 带回校验。
15. **图查询未要求 `&UnrestrictedProof`** — 见 P0-6 修法②（契约违反单列于此便于勾销）。
16. **agent 侧 panic 点** — `main.rs:919/926`（`serde_json::to_value(&r).unwrap()`，请求路径）、`main.rs:704`（`graph_status.lock().unwrap()`，Mutex 中毒后 graph sync task 静默死亡、health 永远报旧值）。修法：`unwrap_or_else`/安全写法（`main.rs:1135` 已是安全写法，照抄）。
17. **`extract_sql` 大写偏移可切在非 UTF-8 边界 panic** — `crates/agent/src/prompt.rs:200-202`：`t.to_uppercase().find("SELECT")` 的偏移切原串 `t[pos..]`，`ß`/`ﬁ` 等字符大写后字节长度变化 → 偏移错位 → 切在非字符边界 panic（单请求 500）。修法：在原串上按词法找，或对 uppercase 切片。
18. **present.rs 对外部数据直接索引，行宽不齐即 panic** — `crates/semantic/src/present.rs:162,186-187,279,296`（`&r[mi]`、`rows[0][mi]` 等；同函数 :163 自己都用 `r.get(ci)` 防御）。`build(columns, rows)` 是 pub API，调用方构造不齐即 panic。修法：统一 `.get(i)`。

### 前端
19. **ECharts tooltip 是 HTML 注入面（存储型 XSS 通路）** — `web/src/BiChart.vue:54`（饼图 `formatter` 拼 `p.name` 即数据库维度值：客户名/商品名）、`:112`（轴触发默认 formatter 同样按 HTML 渲染类目名/序列名）。ECharts tooltip 默认 `renderMode:'html'` 不转义。有档案写权限的人在名称里塞 `<img src=x onerror=…>`，任何查看图表的用户 hover 即执行。修法：formatter 内对所有库值过 HTML 转义（与 `KbAnswer.vue:24-26` 的 `esc()` 同款）。
20. **need-intent 反问的建议按钮永远渲染不出来，且空态文案自相矛盾** — 后端 `crates/agent/src/ask.rs:203` 把建议放 `view.interact.drill` 且 `row_count: 0`；前端 `web/src/ResultPanel.vue:157` 的 drill 区 `v-if="result.row_count > 0 && …"` → 恒不显示；同时 `:102` 的「未找到数据。可能：①该口径本期无记录…」照常显示，与 caliber_note「直接猜会查错表」语义打架。修法：drill 渲染条件去掉 `row_count > 0`（或 `|| caliber_note`）；need-intent 轮跳过 empty-hint。
21. **`drill()` 把完整问句当维度名拼** — `web/src/App.vue:365-367` `send(\`${baseQuestion} 按${dim}\`)`，而后端 need-intent 的建议是完整问句（`ask.rs:231-232`）。修好 #20 后点一下会发出「东风本田 按今年销售额是多少」。修法：drill 项区分维度名与完整问句（后端换字段或前端按内容直发原文）。

### 文档/门禁真实性
22. **audit_trace.py 对最脆弱的引用类形同虚设，且此刻就漏着一条已失效判据** — `tools/audit_trace.py:64` 只校验含 `::`/`/` 的引用，INTEGRATION-TRACE.md 证明列几乎全是裸测试名 → 全部免检。实测：TRACE 引 `snapshot_source_metric_never_composed` 为证，该测试已不存在（行为已从「见快照就拒」反转为「按声明装配」，现测试是 `snapshot_source_metric_composed_per_declaration`，`server/src/direct.rs:216,229-246,2241`），`python tools/audit_trace.py` 仍 exit 0。这正是它设计要抓的「静默腐烂」。修法：裸标识符纳入校验（排除自然语言词表）、符号定位到声明文件、证明列测试名独立一类强制回查。
23. **TRACE「零 AGPL 依赖」为假** — `docker/parser/Dockerfile:46-48` 实打实装了 `pymupdf4llm==0.0.17`（AGPL-3.0），且 `tools/embed_service.py:127-148` 的 PDF 一级解析就是它——真正让 PDF「可用」的容器路径是 AGPL 的。CONFIG.md:91「刻意未装」只对宿主机 .venv 成立。修法：更正 TRACE/CONFIG 表述；补 LICENSE/NOTICE 或在 CONFIG.md 写清「容器镜像含 AGPL 组件、镜像不对外分发、服务边界即许可边界」；根目录补 LICENSE 文件（当前无）。

---

## 三、P2：设计 / 架构偏离

### 架构纪律（对照 ARCHITECTURE.md）
1. **server 未解体，架构迁移实际只走完一半**：8373 行（K6 修订预算 ≈2600 的 3.2 倍）。`direct.rs` 2807 行 + `corrector.rs` 1230 行（纯业务算法+SQL 拼装，占 48%）原封在位且含 94 个 legacy 测试；`main.rs` 1223 行是 CLI 分派（13 子命令内联，约 510 行）+装配+9 个 handler+jobs+认证收口四合体（`main` 函数 551 行）；§4.7 的 `api/`、`mw/`、`cli/`、`state.rs`、`session.rs`、`identity.rs`、`jobs.rs`、`lib.rs` 全部不存在。→ 按 T8/T10 完成搬迁，迁完把 check-arch 的 server 规则从 WarnOnly 转 FAIL。
2. **check-arch.ps1 三处口径收窄/降级**：
   - §1 门禁原文「`MySqlPool|PgPool|…|sqlx::query` 命中 server → exit 1」被脚本自我降级为 `-WarnOnly`（server 现有 17 处命中，含 `main.rs:470/494` 直写 `meta.sql_exemplar`，绕过 §4.4 钦定的语料唯一读写口 `registry::exemplar`；`corrector.rs:74-77,265-268,306-311` 三处 `format!` 拼 SQL 不受 semantic drift 测试守护）；
   - 门禁 pattern 从裸 `PgPool` 收窄为 `PoolOptions` 后，agent 大量函数直接吃 `&PgPool`（`agent/src/ask.rs:78`、`ctx.rs:34`、`gather.rs:293`、`review.rs:46`、`source.rs:39`、`triage.rs:60`、`answerers/graph.rs:84`）——§5 契约写的是 `AskCtx.source: &dyn SqlSource`，这条偏离**没有任何裁决记录**（semantic 的同类偏离有 T7a 裁决+drift 双测试替身，agent 没有）；
   - 脚本是**无 BOM UTF-8**，`powershell.exe`（5.1）下解析失败，仅 pwsh 7 可跑——CI 宿主选错全门静默失效。
3. **D2 全线崩溃：19 个文件 >450 行、18 个 >500（"必拆"级）**：`server/direct.rs` 2807、`server/corrector.rs` 1230、`kernel/sql/caliber.rs` 1229（§4.1 里根本没有这个文件的条目）、`server/main.rs` 1223、`semantic/registry/caliber.rs` 995、`agent/run.rs` 870、`kernel/nl/time.rs` 657、`agent/ask.rs` 654、`knowledge/answer.rs` 653、`agent/gather.rs` 621、`knowledge/retrieve.rs` 609、`kernel/policy/inject.rs` 608、`semantic/seed_defs.rs` 590、`semantic/present.rs` 583、`agent/prompt.rs` 558、`agent/insight.rs` 546、`server/mcp_api.rs` 532、`server/kb_api.rs` 520、`server/admin_api.rs` 484。
4. **D1 点名的拆分多数未做**：§0 D1 名单 8 个超线函数至少 5 个原样或更胖——`compose_sql_with_snap` **321 行**（`server/direct.rs:430`，原 154 行目标拆成 Plan+6 方法 ≤35 行，反而翻倍）、`sales_breakdown` 109（direct.rs:1056）、`rewrite_agg` 83（corrector.rs:718）、`post_visit_expr` 70（corrector.rs:163）、`compute_insight` 72（semantic/present.rs:128）。另有 `main` 551、`seed_metrics` 186、`judge` 110（kernel/sql/caliber.rs:177，新冒出的大函数）等 20+ 个 >60 行。
5. **膨胀哨兵触发**：knowledge 2764/1500（+84%）、agent 6014/3621（+66%，且多出文档树外的 `guard.rs`/`insight.rs`，`prompts/*.md` 契约文件缺失）、connector 2876/2250（+28%）、kernel 5099/3600（+42%）。按 §0 判据，超 20% 且说不出具名功能的部分即过度设计回爬。
6. **测试基线口径混乱**：`#[test]` 557 + `#[tokio::test]` 34 ≈ 591，文档口径「156 搬运 + 约 40 新增」≈196。server 的 94 个 legacy 测试（direct.rs 58 + corrector.rs 36）与 kernel/semantic/agent 的搬运副本**疑似双份并存**（caliber 测试在 kernel 25 个、semantic 14 个、server 36 个三处都有）——需要一次去重对拍，并把 ARCHITECTURE/TRACE/PROGRESS 三处的测试数（465/557/563 三说）统一。
7. **kernel 契约缺口**：`kernel/run.rs` 不存在（`AskError`/`SqlTrace`/`Budget{max_repair_rounds:2}` 全仓 grep 无定义）；`errors.rs` 无 `AskError`；§4.1 文件表与实际漂移。→ 补实现或回写文档。
8. **repair 预算名实不符**：`agent/src/run.rs:30` `MAX_ATTEMPTS=2` → 循环内实际只有 **1 次** repair（attempt 1 上红线 bail、执行失败 Err、caliber 恒 Unresolved）；ARCHITECTURE §4.6 写「`for round in 0..=budget.max_repair_rounds`」（=2 次），§8 又写原物是 `for attempt in 0..2`——文档自相矛盾。二选一：按 §4.6 改 3 轮 2 repair（需重跑回归），或订正 §4.6 措辞并钉死常量注释。

### F4 半落地（上传表头注入防线）
9. `column_doc.origin` 列未进 DDL、未写入（`semantic/src/ingest/mod.rs:22-30` 自认 ponytail；`ddl.rs:61-68` 无该列；`ORIGIN_UPLOAD` 是死常量）；`recall/schema.rs:184` 用 `ds == DMS_DS_ID` 代替 `origin='upload'` 做包裹判据（第三个权威源接入当天就错）；`agent/prompts/system.md:7` 第 3 条「【⚠️】必须逐条遵守」**未限定** `origin='information_schema'`，且全文没有任何一句告诉 LLM `<untrusted_schema>` 里的内容不可信。缓解事实：`sanitize_comment` 实现正确且入库路径确实经过它，伪造【⚠️】这条最毒通道**已封**——剩下的是收尾。修法：DDL 补 origin 列 + `upsert_column_doc` 写入（upload 源传 `ORIGIN_UPLOAD`）；render 判据收紧；system.md 第 3 条限定来源 + 补 untrusted_schema 语义说明。

### 口径与路由
10. **「有效订单状态码统一读 table_scope」未做**：至少 3 处生产代码仍内联 `'0','108','199'`——`server/src/direct.rs:1079`、`:1356`、`crates/connector/src/graph.rs:38`（图 ETL，§4.4 明写「消灭第 8 处内联」，graph 还住在 connector 没迁 semantic）。兜底：`direct.rs:2646` 的 `deterministic_templates_satisfy_table_scopes` 对拍测试会抓红模板侧；graph.rs:38 不在对拍范围。
11. **fastpath 模板不看 ds**：`server/src/direct.rs:1410-1411` `direct_hit = try_direct(question)`，签名无 ds；`agg_template`/`sales_breakdown`/`sniff_doc_code` 写死 DMS 表名。今天（upload 源）：每次白跑一次注定失败的查询；明天（第二个 `policy_kind='dms_datascope'` 的 MySQL 业务源）：**静默答出另一个库的数据**。修法：`try_direct`/`compose_hit` 入口加 `cx.ds == DMS_DS_ID` 门。
12. **drift 守卫擦边通道**：`semantic/tests/drift.rs:82-84` 非 JOIN 判据是 8 行窗口 `win.contains("ds_id")`——`exemplar.rs:87-94` 的 `pending()` 与 `element.rs` 四条 sync SELECT 靠 SELECT 列表里的 `ds_id` 字样过守卫，**没标 `ds:any`**（对比 `exemplar.rs:147` 规规矩矩标了）。守卫不恒真（有跳闸），但这几个实例在给后来人示范错误范式。修法：补 `ds:any` 行内标记或收紧判据。
13. **pitfall 种子只插不改**：`semantic/src/seed.rs:299-311` `INSERT ... WHERE NOT EXISTS (trigger_words, lesson 全等)`——改文案=新行插入，旧行照样 active 参与召回；同文件其他种子全是 `ON CONFLICT DO UPDATE`。修法：加唯一约束改 `ON CONFLICT DO UPDATE`。
14. **`time_tokens` 两份拷贝且两边注释互相声称对方是唯一**：`agent/src/answerers/cache.rs:107-112` 与 `agent/src/triage.rs:32-37` 各一份 12 词表，注释都写「收敛到对方」，实际各调各的——正是两边都警告的「会漂的表」已经漂进来了。删一份，另一处 `use`。
15. **triage 实际在 rewrite 之前，且 hybrid 未实现**：`server/src/main.rs:892`（triage 用 `req.question`）先于 `agent/src/ask.rs:109`（rewrite），与 ARCHITECTURE §6 顺序相反；`triage.rs` 无 Hybrid 变体。后果：知识库轮的追问（「那第二条呢」，上一轮无 SQL → rewrite 跳过）不含任何 kb/data 关键词 → fast LLM 对无上下文短句分类 → 大概率误判 data 去产 SQL。修法：短期把「上一轮是 knowledge」作为 forced 信号传入；中期按 §6 挪 triage；hybrid 做不做回写 ARCHITECTURE。
16. **知识库回答出口无 URL 过滤**（I5「summarize 不得输出 URL」只盖住 agent 侧两个文本出口）：`knowledge/src/answer.rs:86-105` `respond` 无 `has_url` 闸，SYSTEM 也无禁令——恶意文档可让带角标的含链接句子穿过 `keep_cited_only`（可点击钓鱼/外泄通道）。修法：`respond` 过 `has_url` + SYSTEM 加禁令。
17. **kb.acl 的 space/doc 级授权无产品入口**：`knowledge/src/acl.rs:104-134` 的 grant/revoke 是纯库函数，唯一 HTTP 调用点 `server/src/admin_api.rs:293-302` 恒 `AclScope::Ds` 且 admin_only——同事间共享文档/空间今天做不到；`/api/kb/docs` 用 `space_writable` 判定导致 read-only 授权者连列表都看不了。前端 `KbPanel.vue` 也没有 ARCHITECTURE 点名要的「授权」UI（§4.7 承诺的第四件事）。修法：加 space/doc 级授权端点（属主或 write 持有者可授）+ KbPanel 入口 + docs 列表改 read 级判定。
18. **serde golden 守的不是线上实际形状**：`/api/ask` 数据路径返 `agent::AskResult`（无 `kind` 键，`server/src/main.rs:919`），知识库路径才返带 `kind` 的 `kernel::Answer`；`kernel/answer.rs:133-150` 的 golden 测试测的是端点不用的类型。ARCHITECTURE §7「单结果必含 kind 等八键」与线上不符。二选一：端点统一改返 `kernel::Answer`，或把 golden 断言改到 `AskResult` 的真实 wire 形状并修订 §7。
19. **server 配置面**：`db.rs:147-158` 当时无 `DMSAI_` env 覆盖（§4.7 要求）；`default_dms_base()` 曾硬编码真实 DMS 地址并使用明文 HTTP；`server/src/embed.rs:9-14` 当时还有全局单例和固定 loopback 地址，容器内外端口不一致会让 retrieve 静默降级。真实地址已从历史记录移除。
20. **server 协议面**：无统一认证中间件（13 个 handler 手写 `resolve_identity`，当前覆盖完整但无结构保证；`admin_api.rs:78` 自认 `admin()` 与 `ds_api::caller` 是第二份拷贝）；权限错误 HTTP 映射靠**字符串 contains**（`main.rs:913` `msg.contains("无权访问数据源")` 才给 403，其余 PolicyError 落 422，与 §4.7「PolicyError→403」不符）；`/api/health` 的 `ro_source_isolated` 恒 `null`（`main.rs:1134`，F3 观测面未接——pg_ro 懒建导致启动后 health 无法反映隔离状态）。
21. **policy 小问题**：`rules.rs:103-106` `owner_kind` 未知值静默归 `Ids`（与文件自述「未知即跳过」不符，拼写错误变静默错档案）；`rules.rs:173-182` `load_rules` 全量替换丢 builtin 兜底 + `seed_rules` 不清陈旧行（builtin 删表后旧档案继续生效）；`cache.rs` 缓存无容量上限（只按访问惰性清过期）。
22. **kernel 小问题**：`nl/lex.rs:133` 把 DMS 命名约定 `t_` 前缀硬编码进 kernel（`from_table_aliases` 对非 `t_` 前缀的上传 PG 表静默返回空 Vec，下游按「查不到别名」拒绝）；`sql/lex.rs:179-197` `qualify_cols` 函数名白名单不全（`CONCAT`/`CAST`/`LEFT` 等不在表 → 产出 `o.CONCAT(o.a,o.b)` 非法 SQL；更稳的修法：标识符后紧跟 `(` 即函数名不前缀）；`guard.rs:105-118` 占位符幻觉防线只认单引号（MySQL 下 `"__ORDER_CODE__"` 双引号字面量漏判）；kernel 全用 `CURDATE()`（DB 时钟）vs prompt 用 `chrono::Local`（app 时钟）——两者 TZ 不同时「今天/昨天」边界最多差 8 小时，建议加启动自检或文档钉死「DMS MySQL 必须跑 CST」。
23. **FTS 对中文恒空，「三路混合」实为两路**：`knowledge/src/retrieve.rs:319-322` `plainto_tsquery('simple',…)` 对中文 322 格全 0（注释已自证），ARCHITECTURE §4.5 仍写「tsvector 20」。修法：订正文档或引 zhparser/bigram。另 `retrieve.rs:289-292` HNSW 分支可见 doc ≥50 时 `doc_id=ANY` 是索引后过滤，存在「全局最近邻整批属于别人 → 滤完欠额」的召回损失（注释已承认），中期建议 pgvector 迭代扫描。
24. **企微端多角色账号被硬阻断**：`main.rs:806` 企微 token 恒 `role_code=None` 签发；多角色被 fail-closed 拒后，前端 `App.vue:271` `offerRoles` 第一行 `if (sessionToken && !dmsToken) return`——企微用户没有 dms_token 可重换，选择器永不出现（前端注释自认「真要修得改服务端」）。修法：服务端加「带角色重签」端点（校验当前 token 后重 issue），前端对它弹选择器。
25. **前端集成杂项**：`?dms_token=` 读完不清 URL（`App.vue:229-238`，DMS 的 x-access-token 全程挂在 iframe 地址栏/浏览器历史；企微那条路倒是清了）；SSO 验真明文 HTTP（P2 与 #19 合并修）；`ResultPanel.vue` 的 `route` 未透传导致 need-intent 无法特判；`App.vue:66-69` routeLabel 缺 `compound`/`semantic-cache`/`need-intent` 三个（徽标直接显示英文原文）。
26. **INTEGRATION-TRACE.md 失效/名不副实 9 条**（修 audit_trace 后应全部逼红）：
    - `snapshot_source_metric_never_composed` 判据已失效且语义反转（见 P1-22）；
    - 双通道行路径错误：`create_upload_table` 在 `connector/src/owned.rs:82` 不在 TRACE 写的 `ddl.rs`；
    - `meta.element + HNSW` 名不副实：element 只有 `embedding vector(512)` 列，**没有 HNSW 索引**（HNSW 只在 table_doc），元素召回是全表余弦扫描（`ddl.rs:60,173-183`、`recall/cards.rs:173-175`）；
    - 「八个 correction_log kind」→ 九个；「12 条 sql_guard 断言」→ 13；「present 11 条断言」→ 44；「五规则」→ R1-R4 四条、「7 条搬运断言」→ 6 个测试函数；
    - 表头引用计数 131/54/22 → 实测 152/62/33；§四「465 passed/20 target」→ 557 个 `#[test]`（PROGRESS 又写 563）；「门禁 15 条」→ 脚本实为 13 条；回归题数「53-54/55/56」三处打架。
27. **deepagents/SQLBot 对齐静默掉单**：PLAN §A P0 的 **AggOption 聚合裁决**（防双重聚合）与 **MetricDrillDownChecker**（necessary_dims/allowed_drill_dims）零实现，且 TRACE 未做清单未收留；同环比 RATIO 算子只有 `prev_window` 雏形。做了但没记录：`Dialect` trait 双实现（ARCHITECTURE 有载、TRACE 无行）。另注意两处「形态不同但成立」：精确词典层是 value_domain 声明式探针+手写自动机（非 PLAN 的 aho-corasick）；Rubric 自评是确定性判据+judge（非 LLM grader 子代理）——建议回写 PLAN 注明重设计。
28. **CONFIG.md 与代码漂移**：只文档化 20 个字段中的 11 个（缺 `llm_api_key`/`llm_base_url`/`llm_model_*`/`dms_base_url`/`wework_*` 与 `insecure_login_fallback`）；末行引用了代码里不存在的 `dev_token`。PROGRESS.md 未收录 M9j 与两个 Task7 文档提交。
29. **脚本与部署**：`scripts/run.ps1`/`build.ps1` 假设 Windows 能 cargo build 直接跑 exe，与 SAC 裁决（`docker-test.ps1:3-7`「任何新链接 exe 都是 os error 4551」）直接矛盾——本机死脚本，未标注；`build.ps1:2` 硬编码 winget 用户目录路径。
30. **其他**：`semantic/src/seed.rs:34` 表名 `t_sales_order_his_detai` 疑似 `..._detail` 拼写截断（请连库核对；若真错这条 warn 种子从未命中任何表）；`main.rs:246` 的 `meta sync` CLI 只跑 `seed()` 不跑 `seed_datasources()`，与 `bootstrap_meta` 不一致（纯 CLI 初始化的库缺 meta.datasource 的 dms 行）；`ds_api::sync`（ds_api.rs:202-207）采集后不 seed，与 CLI 行为不齐——合并两处为同一函数。

---

## 四、P3：建议（精选）

- **MCP 无 rate limit**：`mcp_api.rs` 全文无限流，泄漏的 key 可无限烧 LLM；`question` 无长度上限（2MB body 全进 prompt）。建议 per-key 令牌桶 + question 截断 2000 字。
- **内部错误原文回客户端**：`main.rs:916`、`mcp_api.rs:302` 把 `e.to_string()` 直接回 422/-32000（含 MySQL 表名列名、LLM 上游错误体）。建议 5xx 泛化文案 + 详情只进 query_log。
- **会话 token 生命周期**：进程内 HashMap 重启全员登出、无主动撤销端点；`api_sso`/`wework` 每次新建 `reqwest::Client` 无连接池复用。
- **iframe 安全头缺位**：server 零安全头（无 CORS/X-Frame-Options/frame-ancestors）。当前默认可被任意站点 iframe（clickjacking 面）。建议 `Content-Security-Policy: frame-ancestors <DMS origin>`。`/api/health` 无认证暴露 PG 扩展清单等内部信息，建议生产收敛。
- **CSV 公式注入**：`App.vue:436` 只转义双引号；单元格以 `= + - @` 开头会在 Excel 里按公式执行。建议四类前缀前补 `'`。
- **前端工程**：死依赖（`ant-design-vue`、`vue-router` 零 import；pinia 无 store）；`vue-tsc` 不进构建（`"build"` 改 `vue-tsc --noEmit && vite build`）；无 lint 无前端单测；`AskResult` 接口注释「全部可选」与声明矛盾（kind=text 响应实际没有 sql/columns 等 required 键）；BiChart 4 处 `(p: any)`。
- **企微体验**：顶栏身份写死「企微用户」+ 恒显示「DMS 免登」（缺 `/api/me` 端点）；401 文案恒写「请从 DMS 重新打开本页」对企微端指错方向；无登出按钮/端点；dmsToken 在内存时 401 后可静默重换再提示。
- **connector**：`registry.rs:57-63` 同一 ds 并发首连建两个池（自认 ponytail）；`owned.rs:44-49` `dead_pg_pool_for_tests` 是生产可用 pub fn（建议 cfg(test)）；`mysql.rs:254-277` BIT/BLOB 等非文本二进制列静默变 Null（丢数据无告警）；`embed.rs:65-72` HTTP 状态码不检查（500+非预期 JSON 静默降级）；`registry.rs:157-174` `redact_dsn` 不遮 `password = x`（等号带空格）；错误消息会带 DB 用户名（`user@host`）上浮到 API 响应。
- **knowledge**：越界角标（6 块写 `[^9]`）原样留在正文，前端 `citations[n-1]` 越界渲染破链——重编号时把越界角标一并剥掉；`answer.rs:430` noop `.clone()`（cargo check warning，顺手删）。
- **agent**：`schema_fix` 失败静默无 warn（run.rs:485-496）；`compound.rs:69` `join_all` 无 panic 隔离（子问 panic 整轮 500，可换 JoinSet）；`lib.rs:16-17` 文档「6+2」与实现「6+3」不符；`source.rs:115-120` `nearest_datasources` 先取 top-4 再按可见过滤（可见源 >4 且最近 4 个均不可见时错误回主源，入清单备查）。
- **kernel**：`ensure_limit_with` 文档说「非纯聚合才追加」实现无聚合判定（注释说谎）；`LOCK IN SHARE MODE` + 追加 LIMIT → 语法错（可用性）；`AnswerBody::Composite.summary` 无 `skip_serializing_if`（落地后会多个恒在 `"summary": null` 键）；`errors.rs:105` `SubqueryNotCovered` 文案含大段连续空格且不在文案冻结测试覆盖里；`split_top_and` 不识别字符串字面量（`x = 'A and B'` 会从中间切开，前提应写成测试）；`prev_window` 月末锚点微不对称（31 号环比系统性偏低）；`sql/lex.rs:240` 测试数据含 DMS 口径串顶破 kernel 零语料边界（换泛化测试串）。
- **policy**：`UnrestrictedProof::new`/`Scope::new` 是 pub「检查过的构造器」可伪造（自认 ponytail，确认已进 `/ponytail-debt` 清单）；`rules.rs:246-252` 单测写全局注册表，中途 panic 污染同进程其他测试。
- **semantic**：`register.rs:114` CASE 拼装只剥单引号不剥反斜杠、列名不过 `ident()` 白名单（复用 probe.rs 的 `ident()`）；`ddl.rs::migrate` 非事务（全语句幂等可重跑，风险可控）；`migrations/` 只有 0020（0001-0019 由 ddl.rs 幂等 DDL 承担，功能等价但结构偏离 §4.4）。
- **前端长查询**：100s 自动 abort、无用户取消按钮、无 SSE/流式（与 INTEGRATION-PLAN 第 5 期 SSE 规划差距已知）。
- **企微 302 依赖未写明的部署前提**：后端不服务静态文件，`/#token=` 能落地的前提是「前端静态站与 /api 同源反代」，EMBED.md/部署文档都没写。

---

## 五、三端实现状态（用户特别关心的问题）

| 端 | 状态 | 关键证据 |
|---|---|---|
| ① 自有 UI | **已实现，功能完整**（问答/会话/知识库/解读/导出） | `web/src/App.vue` 739 行 |
| ② 嵌入 DMS 首页 | **只有助手侧一半**：iframe URL `?dms_token=` → `POST /api/sso` 换签已实现（`App.vue:229-256`）；**DMS 父页 → iframe 的 token 传递链路两头都没有代码** | 见下 |
| ③ 企业微信 | **后端 OAuth 链已实现**（`crates/server/src/wework.rs` 全文、回调 302 `/#token=`）；**前端只有 fragment 读 token 的 boot**（`App.vue:218-223`），无 UA 检测、无移动端布局（仅一条 820px media query）、无过期重授权、多角色账号被硬阻断（P2-24） | `main.rs:799-816` |

**嵌入 DMS 的最大障碍不是 iframe 机制本身，而是 token 传递**：
1. iframe 机制真实存在（DMS SmartAdmin 菜单 `frameFlag=1, frameUrl=...` → `router/index.js:145-147` 挂 `iframe-index.vue`）；EMBED.md 对机制的判断是对的。
2. 但 `frameUrl` 是 DB 里的静态字符串，**DMS 前端全仓没有任何模板变量替换**（`menu-operate-modal.vue:50-51` 只是 `<a-input>`）；EMBED.md:21 写的 `{当前登录token}` 占位符没有任何代码会替换它。postMessage 备选两侧都是零。
3. 今天唯一可走的路径是管理员把**一个写死的 token** 配进 frameUrl → 全员共享同一身份，权限体系形同虚设，不可接受。
4. token 键名假设已核实无误（`localStorage['smart_admin_user_token']`），SSO 验真端点有假 token 负例验证，**但真 token 的端到端 happy path 从未跑通过**（EMBED.md:37 自认「无法自动化」）。
5. 「菜单配置即可替换 DMS 首页」**不成立**：DMS 首页是静态路由（`router/system/home.js` 随 `createRouter` 注册），DB 菜单驱动的动态路由由 `buildRoutes` 后加，同 path 先注册先匹配——菜单管理里改「首页」为外链不会替换真首页。
6. 次要障碍：`iframe-index.vue:11` 写死 `height="800" scrolling="yes"`（双滚动条、不自适应）。

**结论：端②上线需要 DMS 前端一处小改**（`iframe-index.vue` 或菜单渲染处拼 `?dms_token=` + 当前 token，约 5 行）；「零 DMS 源码改动」的说法需修订为「零 DMS 后端改动」。前端侧配套：SSO 成功后清 URL（P2-25）。

---

## 六、参考项目对齐结论（SuperSonic / deepagents / SQLBot）

**SuperSonic 对齐实质良好**（读过实现确认行为真实，不是同名摆设）：语义层注册表族（metric/dimension/term/value_map/table_scope/join_edge）、MapFilter R1-R4 净化、三级时间规则+中文数字、EXPLAIN 预翻译回炉、校正链（schema_check/fix_group_by/add_scope_filter/correct_caliber）、SC 投票五判据（实现完整、默认关、实测记录详实——全表最诚实的一条）、值链接歧义判据、BFS ≤3 跳、parse-execute 两段契约均在代码里真实工作。
**但有 9 条 TRACE 引用失效/名不副实**（见 P2-26），且 audit_trace.py 的盲区恰好放过最脆弱的那条（P1-22）——先修门禁再修文档，否则下次还会烂。
**deepagents**：闭环思想对齐但机制重设计（确定性判据+judge 而非 LLM grader 子代理）；两个 P0 计划项静默掉单（P2-27）。
**SQLBot**：三条「拒抄」理由真实落在代码里（`agent/src/guard.rs:7` 等），不是口头声明。

---

## 七、已验证无问题的面（不要顺手改）

以下面都经过代码级核对为健康，改动它们引入回归的风险大于收益：

- **构建与依赖**：`cargo check --workspace --all-targets` 全绿；7 个 crate 全部 `workspace = true`，无白名单外新依赖；crate 依赖方向无反向边；kernel 非注释源码零 IO、零 DMS 业务语料、不引 chrono。
- **只读红线执行层**：MySQL `after_connect` 对**每条新建连接** `SET SESSION TRANSACTION READ ONLY`；池私有无任何裸池访问器；`fetch` 只收 `&ScopedSql`；`ScopedSql` 唯二产出点（inject/unrestricted+proof）全仓无绕权限出口（grep `unrestricted(` 仅 3 处生产调用点）；proof 铸造点收敛且有「空集合不能铸 proof」回归锁。
- **F1/F2/F3/F5/F7/F8(镜像层) 已落地**：条件解析 EOF 阻断（F1）✓；unrestricted 需 proof（F2）✓；PG ro 源启动自检 `has_schema_privilege` 可见即拒（F3 ①）✓；敏感列 9 词单一事实源 + 结果列整列置 Null + 点名敏感列被 check 拦（F5，含 `SELECT *` 兜底）✓；scope 缓存 TTL15m+scope_ver+DsId（F7）✓；Dockerfile 不 COPY settings（F8 镜像层）✓。
- **权限内核**：段序 base→common→102→103→下属、哨兵 [-1]=拒绝 vs 空=放行、超管短路、无角色/未登记表/条件截断/子查询漏走全部 fail-closed，28+15+3 断言在位，与 Java 1:1 对拍（judge_scope 6/6）。
- **多语句/注释混淆/写词扫描**：第二语句拒、可执行注释拒、词边界写操作扫描、非 Query 语句拒、LIMIT AST 判定（已有 LIMIT 保留、子查询 LIMIT 不误判、字面量含 "limit" 不误判）全部正确且有测试。
- **LLM 修复循环闸门纪律**：每轮 repair 产物重算 candidate 并重过 check→inject→explain→fetch，不存在绕过闸门的执行；explain 的 Some/None 语义保持；九个 correction_log kind 齐全。
- **语义缓存回放**：按当轮用户重新注入；GuardError 回落且 warn、PolicyError 上抛不吞。
- **knowledge 结构安全**：源码面无 SQL 能力（I5）✓；ACL 全部 SQL 内联 JOIN（有断言）✓；上传白名单唯一实现、uuid 落盘防路径穿越（有测试）、sha256 去重、tabular 双通道单事务写 datasource+acl ✓；`wrap_untrusted` 转义与闭合标签防御有测试 ✓；无命中必答且不调 LLM ✓。
- **markdown XSS 链**：`KbAnswer.vue` 渲染前全量 esc，最小渲染器不支持链接/图片语法，`<script>`/`onerror`/`javascript:` 结构上进不来；SQL/表格单元格/实体卡全部文本插值。
- **前端 token 卫生**：会话 token 与 DMS token 只在内存 ref，localStorage 只有 theme；token 不进 URL query；SSO dms_token 走 POST body；企微 token 走 fragment 不进服务端日志。
- **MCP**：key 空→恒 404、不匹配→401、日志只写前 4 位+长度；权限模型=映射员工的完整 principal/scope 链，无「MCP 即超管」旁路；只有 ask/kb_search 两工具不能执行任意 SQL。
- **上传入口其余面**：50MB body limit + 4 并发闸 + 白名单单一事实源 + 空间写权限 fail-closed 均在位；kb 文档删除 `rows_affected==0`→404 已落地。
- **Answer serde**：`#[serde(tag="kind", rename_all="snake_case")]` 写死，Table 八键 golden 断言恰好 8 键。
- **日志面**：server 全部 tracing 调用不含 LLM key/会话 token/问句原文；query_log 入库前 2000 字截断。

---

## 八、修复优先级路线图（建议）

**第 0 步（前置）**：提交当前工作树（整个重构未提交），并立即提交 `.gitignore`（保护 settings.docker.json）。

**第一批 · 红线（1-2 天）**：
1. P0-1 JOIN ON 子查询注入（kernel/inject.rs）+ 回归测试
2. P0-2 deny-list 子串绕过（kernel/guard.rs）+ 三条构造 SQL 断言
3. P0-3 SELECT INTO（kernel/guard.rs AST 拒）
4. P0-6 graph esc `$$` + proof 形参 + 超时
5. P0-4 F6 全套（DDL 加列、VIS_PRED、复核判词、drift 守卫）
6. P0-5 凭据轮换（DMS admin、PG 口令、SM4 密钥）+ 文档删除明文 + compose 收 127.0.0.1 + settings.prod.json 拆分

**第二批 · 数据正确性（2-3 天）**：P1-1（LIMIT 注释）、P1-2（近N天口径裁决+golden）、P1-4（注入后 SQL 喂 LLM）、P1-5（scoped 双 NULL）、P1-7（token 估算）、P1-19/20/21（前端 tooltip 转义 + need-intent 两件套）、P1-9（删除假成功）、P1-13（上传先认证）、P1-14（企微 state）。

**第三批 · 结构债（随 T8/T10 节奏）**：server 解体 + check-arch 转 FAIL + agent `&PgPool` 收敛 `&dyn SqlSource` + F4 收尾（origin 列+system.md）+ 有效订单口径收口 + fastpath ds 门 + repair 预算名实收拢 + D1/D2 超标文件拆分。

**第四批 · 文档与门禁真实性（1 天）**：audit_trace.py 修盲区 → TRACE 9 条更正 → ARCHITECTURE/CONFIG/PROGRESS 数字与字段对齐 → LICENSE/NOTICE + AGPL 表述 → 企微端与嵌入端的部署前提写进 EMBED.md。

**三端推进建议**：端②（嵌入）是最小改动路径——DMS 前端 5 行 + 前端清 URL + 修订「零改动」说法；端③（企微）先补「带角色重签」端点与 `/api/me`，移动端布局单独立项。
