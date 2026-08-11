/** 各管理/只读面板共享的鉴权与报错小工具。
 *  UsagePanel / SkillsPanel / SqlAuditPanel / DataMapPanel 原本四处逐字重复，改动时请只动这里。 */

/** token 优先；无 token 时把登录名拼成 query 首参（login 为空则不拼，避免 `?login_name=` 空值参数）。 */
export function authQuery(token?: string, login?: string): string {
  return token || !login ? '' : `?login_name=${encodeURIComponent(login)}`
}

/** 与 authQuery 同款，但拼成 `&login_name=` 续参（URL 上已有 query 时用）。 */
export function authTail(token?: string, login?: string): string {
  return token || !login ? '' : `&login_name=${encodeURIComponent(login)}`
}

/** 鉴权头：有 token 带 Authorization；json=true 时补 Content-Type。 */
export function authHeaders(token?: string, json = false): Record<string, string> {
  const h: Record<string, string> = {}
  if (json) h['Content-Type'] = 'application/json'
  if (token) h.Authorization = `Bearer ${token}`
  return h
}

/** 先取 text 再试解析：端点未上线时 axum 兜底 404 是空体，直接 .json() 只会抛 SyntaxError。 */
export async function errText(r: Response, fallback: string): Promise<string> {
  const raw = await r.text()
  let body: { error?: string } | null = null
  try { body = raw ? JSON.parse(raw) : null } catch { /* 非 JSON 按原文报 */ }
  return body?.error || raw.trim().slice(0, 200) || `${fallback}（HTTP ${r.status}）`
}

/** catch 到未知值时的展示文案：Error 取 message，其余 String()，避免「[object Object]」。 */
export function errMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

/** HTML 文本转义（& < > "）：KbAnswer / KbDocPreview 共用一份实现，别处别再手写 esc。 */
export function escHtml(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')
}

/** 导图/图谱分支调色板（KbMindmap / KbGraph 共用一份，同屏视觉语言不漂移；以 KbGraph 的 10 色为准）。 */
export const GRAPH_PALETTE = ['#f0a63c', '#e1655b', '#4a90d9', '#9b6de8', '#3bb273', '#38b6c9', '#e87ab0', '#c9a53c', '#7b89f0', '#d45f9e']

/** 与 theme.css 的 --font-sans 同一字族：canvas 2d / 导出 SVG 不解析 CSS 变量时用这份。 */
export const FONT_FAMILY = '"Segoe UI","PingFang SC","Microsoft YaHei UI","Microsoft YaHei",system-ui,sans-serif'

/** 会话接口（Bearer 必备）的鉴权头：无 token 时先回调 auth-expired 再抛错，
 *  调用处 catch 应优先透出该 message（KbDocPreview / KbAnswer 原本各有一份逐字重复）。 */
export function sessionHeaders(token: string | undefined, onAuthExpired: () => void): Record<string, string> {
  const t = token?.trim()
  if (!t) {
    onAuthExpired()
    throw new Error('登录会话已失效，请重新登录。')
  }
  return { Authorization: `Bearer ${t}` }
}
