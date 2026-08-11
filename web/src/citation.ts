// 知识库引用（citation）的「命中位置」口径：目录 / 章节 / 页码的提取、截断、去重规则
// 只维护这一份。消费方：KbAnswer.vue 来源行；小程序 ai-chat.vue 按同规则另抄了一份 JS
// （uni-app 工程在仓库外，引不进来——改这里时记得同步那边，见 mapCitations 注释）。

/** 来源行需要的位置字段（App.vue / KbAnswer.vue 的 Citation 都是它的超集）。 */
export interface CitationLocation {
  doc_id: string
  page?: number | null
  /** 服务端字符串形态是「 > 」拼接（store.rs 口径）；兼容数组形态。 */
  heading_path?: string | string[] | null
  folder_path?: string | null
  directory_path?: string | null
}

export interface LocationPart {
  kind: 'folder' | 'heading' | 'page'
  /** 徽标展示文本（章节超长按 LOCATION_HEADING_MAX 截断）。 */
  text: string
  /** 完整文本：折 title/悬浮提示用。 */
  full: string
}

/** 章节徽标展示上限（超出截断补 …，完整路径放 title）。 */
export const LOCATION_HEADING_MAX = 40

/** heading_path：数组或 " > " 串统一成「a > b > c」展示串；空值归一为 ''。 */
export function headingTextOf(c: CitationLocation): string {
  const raw = c.heading_path
  const text = Array.isArray(raw) ? raw.filter(Boolean).join(' > ') : String(raw ?? '')
  return text.trim()
}

/** 目录路径：folder_path 优先、directory_path 兜底；根目录「/」等于没给（与 KbAnswer 旧 locationOf 同口径）。 */
export function folderTextOf(c: CitationLocation): string {
  const text = String(c.folder_path || c.directory_path || '').trim()
  return text === '/' ? '' : text
}

function truncate(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max)}…` : text
}

/**
 * 位置徽标组：📁目录 / 章节 / 第N页，有哪个出哪个，顺序固定。
 * 三个都没有 = 空数组 —— 来源行降级为纯文档名，不留空徽标。
 */
export function locationParts(c: CitationLocation): LocationPart[] {
  const parts: LocationPart[] = []
  const folder = folderTextOf(c)
  if (folder) parts.push({ kind: 'folder', text: `📁 ${folder}`, full: folder })
  const heading = headingTextOf(c)
  if (heading) parts.push({ kind: 'heading', text: `章节：${truncate(heading, LOCATION_HEADING_MAX)}`, full: heading })
  if (typeof c.page === 'number' && c.page > 0) {
    parts.push({ kind: 'page', text: `第 ${c.page} 页`, full: `第 ${c.page} 页` })
  }
  return parts
}

/** 去重键：同一文档 + 同一页 + 同一章节的重复命中只显示一行（同一文档不同位置仍分行）。 */
export function dedupeKey(c: CitationLocation): string {
  return `${c.doc_id}|${c.page ?? ''}|${headingTextOf(c)}`
}

/** 保序去重：返回每个去重键首次出现的下标（0-based，升序）。 */
export function dedupeFirstIndex<T extends CitationLocation>(list: T[]): number[] {
  const seen = new Set<string>()
  const out: number[] = []
  list.forEach((c, i) => {
    const key = dedupeKey(c)
    if (seen.has(key)) return
    seen.add(key)
    out.push(i)
  })
  return out
}
