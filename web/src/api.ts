// 后端端点的**单一事实源**。
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
 *  `{question, sql, columns, rows(前几行), row_count(总行数!), caliber_note, login_name, role_code}`。
 *  ⚠️ `row_count` 必须给总行数而不是 `rows.length`，否则解读会把「前 5 行」当成全部。 */
export const ANALYSIS_URL = '/api/analysis'
