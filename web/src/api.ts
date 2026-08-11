// 高漂移风险端点的集中处 —— **不是**全仓端点的单一事实源。
// 一次性的 fetch 就地内联是有意为之（见下方收录规则），全仓约 90 处内联 URL 不在此列；
// 别拿这个文件去 grep「系统有哪些端点」。
//
// 🔴 为什么值得一个文件：URL 是字符串字面量，类型系统管不到它。
// 实测翻车：AI 解读那条缝上，前端写 `GET /api/record/{id}/analysis`、后端实现
// `POST /api/analysis`，两侧单测都绿、`vue-tsc` 零输出（可选字段 + 字符串字面量都无从检查）、
// 合起来功能 100% 不通，而**没有任何判据会红**。
// 收在这里至少让「只改了一侧」在 grep 里是一处而不是两处。
//
// 命名规则：常量名 = 后端路由的形状，注释里写清方法与响应键。
// 只放**跨组件复用**或**容易两侧漂**的端点；一次性的 fetch 不必进来（那反而多一层间接）。

/** `POST` → `{ caliber: string, insight: string | null }`
 *
 *  `caliber` 恒有（确定性、零 LLM 的口径说明：来源表/过滤/时间窗/去重）；
 *  `insight` 是 fast 模型那段话，**可能为 null**（模型挂了 / 回了网址 / 开关关着）。
 *  遇 null 只显示 caliber，**不标成失败** —— 解读失败不许让一次成功的取数看起来失败。
 *
 *  请求体是前端手上那次 `/api/ask` 结果的素材（服务端不存这次结果，所以没有 id 可用）：
 *  `{question, sql, columns, rows(前几行), row_count(总行数!), caliber_note, deep, login_name, role_code}`。
 *  `deep` = 深度模式开关（true 时 Precise 档四段式解读；缺省/null = fast 2-4 句）。
 *  ⚠️ `row_count` 必须给总行数而不是 `rows.length`，否则解读会把「前 5 行」当成全部。
 *
 *  收录理由：调用点虽只有 App.vue 一处，但它正是头注里那次「两侧各改一半」的事故端点 ——
 *  留在这是防复发，不是误收。 */
export const ANALYSIS_URL = '/api/analysis'

/** `POST /api/analysis/report` → `{ id, title, preview_url, download_url }`
 *
 *  把一次解读固化成报表 artifact（零 LLM：服务端重算口径，insight 原样回声）。
 *  请求体在 ANALYSIS_URL 素材之上多 `insight`（回声）、`charts`（图表规格回声）、
 *  `conv_id`（**字符串型数字** —— 服务端 `parse::<i64>()`，与 `/api/ask` 的数字型
 *  `conv_id` 不是一个契约，别跨端点抄类型）。
 *  与 ANALYSIS_URL 同族、同样的两侧漂风险，故同收录。 */
export const ANALYSIS_REPORT_URL = '/api/analysis/report'
