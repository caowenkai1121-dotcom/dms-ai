<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { FONT_FAMILY, GRAPH_PALETTE } from './panel-utils'

// docId ＝文档叶子；chunkCount ＝章节节点（内容级，由 sections 端点懒加载嫁接，不是服务端树的固有层）
interface MNode {
  name: string; children: MNode[]
  docId?: string; chunkCount?: number; excerpt?: string; page?: number | null
}
interface LayoutNode {
  key: string; parentKey: string; name: string; label: string; depth: number; x: number; y: number
  branch: number; hasChildren: boolean; collapsed: boolean; hiddenCount: number
  docCount: number; labelW: number
  docId: string | null; chunkCount: number; excerpt: string; page: number | null
}
interface LayoutEdge { x1: number; y1: number; x2: number; y2: number; branch: number }

const props = defineProps<{ token?: string; spaceId?: string; writable?: boolean }>()
const emit = defineEmits<{ (e: 'auth-expired'): void }>()

const PALETTE = GRAPH_PALETTE
const ROW_H = 36
const COL_GAP = 56
/** 贝塞尔边控制点偏移（模板与导出 SVG 共用，只维护一处）。 */
const EDGE_CP = 44
/** 折叠记忆最多保留的空间数（防 localStorage 残留无限膨胀）。 */
const COLLAPSE_MAX_SPACES = 8

const root = ref<MNode | null>(null)
const loading = ref(false)
const unavailable = ref(false)
const regenerating = ref(false)
const note = ref('')
/** 提示条分级：warn 错误黄（默认）/ ok 成功绿 / info 中性灰。 */
const noteKind = ref<'warn' | 'ok' | 'info'>('warn')
const collapsedKeys = ref<string[]>([])
const exporting = ref(false)
// 内容级展开状态（只活在本会话：不按空间记忆——章节数据是懒加载的，展开即拉取）
const expandedDocs = ref<string[]>([])
const docSections = ref<Record<string, MNode[]>>({})
const loadingDoc = ref('')
interface SectionCard { docId: string; docName: string; name: string; chunkCount: number; excerpt: string; page: number | null }
const activeSection = ref<SectionCard | null>(null)
const cardCloseBtn = ref<HTMLButtonElement | null>(null)
let mindmapEpoch = 0

// 折叠状态记忆：按空间存 localStorage（键是名称路径+兄弟序号，跨会话稳定；导图重生成后的失效键无害，
// 它们只会「折叠一个不存在的路径」，不落任何副作用）
const COLLAPSE_PREFIX = 'kb_mindmap_collapsed:'
const COLLAPSE_INDEX = 'kb_mindmap_collapsed_spaces'
function storageKey(): string {
  return `${COLLAPSE_PREFIX}${props.spaceId ?? ''}`
}
function restoreCollapsed(): string[] {
  if (!props.spaceId) return []
  try {
    const arr: unknown = JSON.parse(localStorage.getItem(storageKey()) ?? '[]')
    return Array.isArray(arr) ? arr.filter((k): k is string => typeof k === 'string') : []
  } catch {
    return []
  }
}
function saveCollapsed() {
  if (!props.spaceId) return
  try {
    localStorage.setItem(storageKey(), JSON.stringify(collapsedKeys.value))
    // 按前缀裁剪：只保留最近 N 个空间的记忆，删除/改名的空间键不永久残留
    const index: unknown = JSON.parse(localStorage.getItem(COLLAPSE_INDEX) ?? '[]')
    const prev = Array.isArray(index) ? index.filter((s): s is string => typeof s === 'string') : []
    const nextList = [props.spaceId, ...prev.filter((s) => s !== props.spaceId)].slice(0, COLLAPSE_MAX_SPACES)
    for (const s of prev) if (!nextList.includes(s)) localStorage.removeItem(`${COLLAPSE_PREFIX}${s}`)
    localStorage.setItem(COLLAPSE_INDEX, JSON.stringify(nextList))
  } catch { /* 隐私模式等写不进就静默：记忆缺席不影响本次使用 */ }
}

function headers(): Record<string, string> {
  const token = props.token?.trim()
  if (!token) {
    emit('auth-expired')
    throw new Error('登录会话已失效，请重新登录。')
  }
  return { Authorization: `Bearer ${token}` }
}

function normalizeNode(raw: unknown): MNode | null {
  if (!raw || typeof raw !== 'object') return null
  const item = raw as Record<string, unknown>
  const name = String(item.name ?? item.label ?? item.title ?? '').trim()
  if (!name) return null
  const rawChildren = Array.isArray(item.children) ? item.children
    : Array.isArray(item.nodes) ? item.nodes
      : Array.isArray(item.items) ? item.items : []
  const node: MNode = { name, children: rawChildren.map(normalizeNode).filter((n): n is MNode => n != null) }
  // 文档叶子带 doc_id（导图端点契约）：内容级展开靠它懒加载章节
  if (typeof item.doc_id === 'string' && item.doc_id) node.docId = item.doc_id
  return node
}

function normalizeTree(data: unknown): MNode | null {
  if (!data || typeof data !== 'object') return null
  const bag = data as Record<string, unknown>
  return normalizeNode(bag.root ?? bag.tree ?? bag.mindmap ?? (bag.name != null || bag.label != null ? bag : null))
}

function labelWidth(name: string): number {
  let width = 0
  for (const ch of name) width += ch.charCodeAt(0) > 0xff ? 12.5 : 7
  return Math.min(240, width + 8)
}

/** 名称截断：列宽上限 240px（约 224px 文本 + 省略号），全名由 <title> 兜底。 */
function clipName(name: string): string {
  let width = 0
  let i = 0
  for (; i < name.length; i++) {
    width += name.charCodeAt(i) > 0xff ? 12.5 : 7
    if (width > 224) break
  }
  return i >= name.length ? name : `${name.slice(0, i)}…`
}

// 徽标宽度按位数分档（1 位/2 位/3+ 位）；≥1000 显示 999+（4 字符也放得下 30px 档）
function badgeWidth(count: number): number {
  return count < 10 ? 16 : count < 100 ? 22 : 30
}
function badgeText(count: number): string {
  return count > 999 ? '999+' : String(count)
}

// 展示树＝服务端骨架 + 已展开文档下嫁接的章节节点（原树保持纯净，重生成/换空间直接丢嫁接层）
const displayRoot = computed<MNode | null>(() => {
  const tree = root.value
  if (!tree) return null
  const graft = (node: MNode): MNode => {
    if (node.docId && expandedDocs.value.includes(node.docId)) {
      return { ...node, children: docSections.value[node.docId] ?? [] }
    }
    return { ...node, children: node.children.map(graft) }
  }
  return graft(tree)
})

const layout = computed(() => {
  const tree = displayRoot.value
  if (!tree) return { nodes: [] as LayoutNode[], edges: [] as LayoutEdge[], width: 0, height: 0 }
  const collapsed = new Set(collapsedKeys.value)
  // 列宽按全量展开量：折叠分支再展开时列不跳动
  const colWidths: number[] = []
  const measure = (node: MNode, depth: number) => {
    colWidths[depth] = Math.max(colWidths[depth] ?? 0, labelWidth(node.name) + 26)
    for (const child of node.children) measure(child, depth + 1)
  }
  measure(tree, 0)
  const colX: number[] = [24]
  for (let d = 1; d < colWidths.length; d++) colX[d] = colX[d - 1] + (colWidths[d - 1] ?? 0) + COL_GAP

  // 子树计数单趟自底向上预算（折叠分支的 hiddenCount 也要用）：主遍历不再每层重复递归
  const counts = new Map<MNode, { docs: number; descendants: number }>()
  const countUp = (node: MNode): { docs: number; descendants: number } => {
    // 文档数徽标的口径：只数文档叶子（章节节点是内容级，不计入文档数）
    let docs = node.docId ? 1 : 0
    let descendants = 0
    for (const child of node.children) {
      const c = countUp(child)
      docs += c.docs
      descendants += c.descendants + 1
    }
    const v = { docs, descendants }
    counts.set(node, v)
    return v
  }
  countUp(tree)

  const nodes: LayoutNode[] = []
  const edges: LayoutEdge[] = []
  let leaf = 0
  // 叶节点自上而下各占一行，父节点取首末子节点中点；
  // key 是名称路径 + 兄弟序号（同父同名不撞 key、不串折叠状态），parentKey 直接记录不靠字符串切分
  const visit = (node: MNode, depth: number, branch: number, path: string, index: number): number => {
    const key = path ? `${path}/${node.name}#${index}` : node.name
    const isCollapsed = collapsed.has(key) && node.children.length > 0
    const x = colX[depth] ?? depth * 200
    let y: number
    if (!node.children.length || isCollapsed) {
      y = 28 + leaf * ROW_H
      leaf++
    } else {
      const childYs = node.children.map((child, i) => visit(child, depth + 1, depth === 0 ? i : branch, key, i))
      y = (childYs[0] + childYs[childYs.length - 1]) / 2
    }
    const c = counts.get(node) ?? { docs: 0, descendants: 0 }
    nodes.push({
      key, parentKey: path, name: node.name, label: clipName(node.name), depth, x, y, branch,
      hasChildren: node.children.length > 0,
      collapsed: isCollapsed,
      hiddenCount: isCollapsed ? c.descendants : 0,
      docCount: node.docId || node.chunkCount ? 0 : c.docs,
      labelW: labelWidth(node.name),
      docId: node.docId ?? null,
      chunkCount: node.chunkCount ?? 0,
      excerpt: node.excerpt ?? '',
      page: node.page ?? null,
    })
    return y
  }
  visit(tree, 0, 0, '', 0)
  // 父节点坐标在子节点之后才定，边在第二遍按 parentKey 补
  const byKey = new Map(nodes.map((n) => [n.key, n]))
  for (const node of nodes) {
    if (node.depth === 0) continue
    const parent = byKey.get(node.parentKey)
    if (parent) edges.push({ x1: parent.x, y1: parent.y, x2: node.x, y2: node.y, branch: node.branch })
  }
  const width = Math.max(400, (colX[colX.length - 1] ?? 0) + (colWidths[colWidths.length - 1] ?? 120) + 60)
  const height = Math.max(160, leaf * ROW_H + 48)
  return { nodes, edges, width, height }
})

function toggle(key: string) {
  const collapsing = !collapsedKeys.value.includes(key)
  collapsedKeys.value = collapsing
    ? [...collapsedKeys.value, key]
    : collapsedKeys.value.filter((k) => k !== key)
  saveCollapsed()
  // 折叠分支时若摘要卡属于其下章节，卡片一并关（内容对应节点已不可见）
  if (collapsing && activeSection.value) {
    const docKey = layout.value.nodes.find((n) => n.docId === activeSection.value?.docId)?.key
    if (docKey && docKey.startsWith(`${key}/`)) activeSection.value = null
  }
}

// ==================== 内容级：文档 → 章节 ====================
// 章节数据走 `/api/kb/doc/{id}/sections`（契约见 kb_mindmap_api.rs ③，端点未注册时
// 优雅降级为「只到文档」并提示，不炸导图）。

function normalizeSections(input: unknown): MNode[] {
  const list = Array.isArray((input as Record<string, unknown>)?.sections)
    ? (input as Record<string, unknown>).sections as unknown[]
    : []
  return sectionNodes(list)
}

/** 章节节点递归映射（服务端 children 同形嵌套——多级标题逐级可展）；
 *  0 块章节不进树（否则节点语义/样式/行为三处矛盾）。 */
function sectionNodes(list: unknown[]): MNode[] {
  const out: MNode[] = []
  for (const raw of list) {
    if (!raw || typeof raw !== 'object') continue
    const item = raw as Record<string, unknown>
    const name = String(item.section ?? item.name ?? '').trim()
    if (!name) continue
    const chunkCount = Number(item.chunk_count) || 0
    if (!chunkCount) continue
    out.push({
      name,
      children: Array.isArray(item.children) ? sectionNodes(item.children) : [],
      chunkCount,
      excerpt: String(item.excerpt ?? ''),
      page: typeof item.page === 'number' ? item.page : null,
    })
  }
  return out
}

async function toggleDoc(node: LayoutNode) {
  const docId = node.docId
  if (!docId) return
  if (expandedDocs.value.includes(docId)) {
    expandedDocs.value = expandedDocs.value.filter((d) => d !== docId)
    if (activeSection.value?.docId === docId) activeSection.value = null
    return
  }
  if (!docSections.value[docId]) {
    if (loadingDoc.value) {
      note.value = '正在读取另一文档的章节，请稍候。'
      noteKind.value = 'info'
      return
    }
    loadingDoc.value = docId
    note.value = ''
    // epoch 守卫：拉取途中换空间/卸载后，旧响应不许写回新状态
    const epoch = mindmapEpoch
    try {
      const response = await fetch(`/api/kb/doc/${encodeURIComponent(docId)}/sections`, { headers: headers() })
      if (response.status === 401) emit('auth-expired')
      if (!response.ok) throw Object.assign(new Error(`HTTP ${response.status}`), { status: response.status })
      const data = await response.json().catch(() => ({}))
      if (epoch !== mindmapEpoch) return
      docSections.value = { ...docSections.value, [docId]: normalizeSections(data) }
    } catch (e) {
      if (epoch !== mindmapEpoch) return
      // 仅 404 是「接口未上线」；401 已走 auth-expired，其余按普通读取失败
      const status = (e as { status?: number }).status
      const session = e instanceof Error && e.message.includes('登录会话')
      note.value = session ? (e as Error).message
        : status === 404 ? '章节展开接口尚未上线，当前导图只能展开到文档。'
          : '章节读取失败，请稍后重试。'
      noteKind.value = 'warn'
      loadingDoc.value = ''
      return
    }
    loadingDoc.value = ''
  }
  if (!docSections.value[docId]?.length) {
    note.value = `《${node.name}》没有可展开的章节结构。`
    noteKind.value = 'info'
    return
  }
  expandedDocs.value = [...expandedDocs.value, docId]
}

/** 章节所在文档的祖先节点（嵌套章节的直接父级可能是章节不是文档）：摘要卡要 docId/docName。 */
function docAncestorOf(node: LayoutNode): LayoutNode | undefined {
  let cur: LayoutNode | undefined = node
  while (cur) {
    if (cur.docId) return cur
    cur = layout.value.nodes.find((n) => n.key === cur?.parentKey)
  }
  return undefined
}

function openSection(node: LayoutNode) {
  const doc = docAncestorOf(node)
  activeSection.value = {
    docId: doc?.docId ?? '',
    docName: doc?.name ?? '',
    name: node.name,
    chunkCount: node.chunkCount,
    excerpt: node.excerpt,
    page: node.page,
  }
  // 焦点移入卡片（Esc 关闭才有键盘起点）
  void nextTick(() => cardCloseBtn.value?.focus())
}

// 点击路由分两层（用户核心诉求：有节点就能展开）——
// 圆点 = 展开/收起（有子级时）；文字 = 主动作（章节出摘要卡 / 文档展收章节 / 分支折叠）。
// 带子级的章节两个动作都够得着：点圆点展子章节、点文字看摘要。
function onDotClick(node: LayoutNode) {
  if (node.docId) { void toggleDoc(node); return }
  if (node.hasChildren) { toggle(node.key); return }
  if (node.chunkCount) openSection(node)
}
function onLabelClick(node: LayoutNode) {
  if (node.chunkCount) { openSection(node); return }
  if (node.docId) { void toggleDoc(node); return }
  if (node.hasChildren) toggle(node.key)
}

/** 可交互判定：纯叶子（无子节点、非文档、非章节）不可点，不给 button 语义。 */
function isClickable(node: LayoutNode): boolean {
  return node.hasChildren || !!node.docId || !!node.chunkCount
}
function dotActionLabel(node: LayoutNode): string {
  if (node.docId) return `${expandedDocs.value.includes(node.docId) ? '收起' : '展开'}文档 ${node.name} 的章节`
  if (node.hasChildren) return node.collapsed ? `展开 ${node.name}` : `折叠 ${node.name}`
  if (node.chunkCount) return `查看章节 ${node.name} 摘要`
  return node.name
}

function branchColor(branch: number): string {
  return PALETTE[((branch % PALETTE.length) + PALETTE.length) % PALETTE.length]
}

async function load() {
  const epoch = ++mindmapEpoch
  loading.value = true
  unavailable.value = false
  note.value = ''
  collapsedKeys.value = restoreCollapsed()
  root.value = null
  // 内容级状态随树一起失效（章节挂在 doc_id 上，换空间/重生成后旧嫁接层无意义）
  expandedDocs.value = []
  docSections.value = {}
  activeSection.value = null
  loadingDoc.value = ''
  if (!props.spaceId) {
    loading.value = false
    unavailable.value = true
    return
  }
  try {
    const response = await fetch(`/api/kb/mindmap?space_id=${encodeURIComponent(props.spaceId)}`, { headers: headers() })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    const data = await response.json().catch(() => ({}))
    if (epoch !== mindmapEpoch) return
    root.value = normalizeTree(data)
  } catch (e) {
    // 401（会话失效）交给 auth-expired，不置 unavailable
    if (epoch === mindmapEpoch && !(e instanceof Error && e.message.includes('登录会话'))) unavailable.value = true
  } finally {
    if (epoch === mindmapEpoch) loading.value = false
  }
}

/** 运营类接口的错误文案：优先透传服务端 {error}（与 KbGraph 的 opsError 同款口径）。 */
async function opsError(response: Response, fallback: string): Promise<Error> {
  const data = await response.json().catch(() => ({})) as Record<string, unknown>
  const msg = typeof data.error === 'string' && data.error.trim() ? data.error : fallback
  return new Error(msg)
}

async function regenerate() {
  if (regenerating.value || !props.writable) return
  const epoch = mindmapEpoch
  regenerating.value = true
  note.value = ''
  try {
    const response = await fetch('/api/kb/mindmap/regenerate', {
      method: 'POST',
      headers: { ...headers(), 'Content-Type': 'application/json' },
      body: JSON.stringify({ space_id: props.spaceId ?? '' }),
    })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw await opsError(response, '导图生成接口暂不可用。')
    if (epoch !== mindmapEpoch) return
    // load() 会 ++mindmapEpoch 令 finally 的 epoch 判等永假：先复位 busy，再重载
    regenerating.value = false
    await load()
    // 成功后给明确反馈：新旧树相似时用户也能感知已生效
    if (root.value) {
      note.value = '导图已重新生成。'
      noteKind.value = 'ok'
    }
  } catch (e) {
    if (epoch === mindmapEpoch) {
      note.value = e instanceof Error && e.message ? e.message : '导图生成接口暂不可用。'
      noteKind.value = 'warn'
    }
  } finally {
    if (epoch === mindmapEpoch) regenerating.value = false
  }
}

watch(() => props.spaceId, () => { void load() })

// ==================== 导出 PNG/SVG ====================
// 不从 DOM 克隆（scoped CSS 跟不出去），而是用同一份 layout 重新生成带内联样式的独立 SVG，
// 页面主题换肤/样式调整都不会让导出物失真。

function escXml(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')
}

// 导出瞬间解析一次 CSS 变量：导出物跟当前主题（亮/暗）一致
function themeColor(name: string, fallback: string): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim()
  return v || fallback
}

function buildExportSvg(): string {
  const { nodes: ns, edges: es, width, height } = layout.value
  const bg = themeColor('--bg-card', '#ffffff')
  const textRegular = themeColor('--text-regular', '#3c4257')
  const textPrimary = themeColor('--text-primary', '#10162b')
  const textFaint = themeColor('--text-faint', '#8b93ad')
  const primary = themeColor('--primary', '#4a90d9')
  const font = `font-family="${FONT_FAMILY}"`
  const parts: string[] = [
    `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">`,
    `<rect width="${width}" height="${height}" fill="${bg}"/>`,
  ]
  for (const e of es) {
    const d = `M ${e.x1} ${e.y1} C ${e.x1 + EDGE_CP} ${e.y1}, ${e.x2 - EDGE_CP} ${e.y2}, ${e.x2} ${e.y2}`
    parts.push(`<path d="${d}" fill="none" stroke="${branchColor(e.branch)}" stroke-width="1.4" opacity="0.75"/>`)
  }
  for (const n of ns) {
    const color = n.depth === 0 ? primary : branchColor(n.branch)
    const fill = n.collapsed || !n.hasChildren ? color : bg
    // 与屏幕渲染同款细节：章节点更小半径（3.5）、文档点虚线描边
    const r = n.chunkCount ? 3.5 : 4.5
    const dash = n.docId ? ' stroke-dasharray="2.5 2"' : ''
    parts.push(`<circle cx="${n.x}" cy="${n.y}" r="${r}" fill="${fill}" stroke="${color}" stroke-width="1.6"${dash}/>`)
    const fs = n.depth === 0 ? 13 : 12
    const fw = n.depth === 0 ? ' font-weight="700"' : ''
    parts.push(`<text x="${n.x + 11}" y="${n.y + 4}" fill="${n.depth === 0 ? textPrimary : textRegular}" font-size="${fs}"${fw} ${font}>${escXml(n.label)}</text>`)
    let bx = n.x + 16 + n.labelW
    if (n.docCount) {
      const w = badgeWidth(n.docCount)
      parts.push(`<rect x="${bx}" y="${n.y - 8}" width="${w}" height="14" rx="7" fill="none" stroke="${color}" stroke-width="1" opacity="0.85"/>`)
      parts.push(`<text x="${bx + w / 2}" y="${n.y + 2.5}" fill="${textFaint}" font-size="9.5" text-anchor="middle" ${font}>${badgeText(n.docCount)}</text>`)
      bx += w + 5
    }
    // 章节块数徽标（虚线胶囊，与屏幕一致）
    if (n.chunkCount) {
      const w = badgeWidth(n.chunkCount)
      parts.push(`<rect x="${bx}" y="${n.y - 8}" width="${w}" height="14" rx="7" fill="none" stroke="${color}" stroke-width="1" opacity="0.85" stroke-dasharray="3 2"/>`)
      parts.push(`<text x="${bx + w / 2}" y="${n.y + 2.5}" fill="${textFaint}" font-size="9.5" text-anchor="middle" ${font}>${badgeText(n.chunkCount)}</text>`)
      bx += w + 5
    }
    if (n.hiddenCount) {
      parts.push(`<text x="${bx + 2}" y="${n.y + 4}" fill="${textFaint}" font-size="10" ${font}>+${n.hiddenCount}</text>`)
    }
  }
  parts.push('</svg>')
  return parts.join('\n')
}

function download(filename: string, blob: Blob) {
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  // 老 Firefox 对未挂 DOM 的锚点 download 不生效：挂上再点再摘
  document.body.appendChild(a)
  a.click()
  a.remove()
  window.setTimeout(() => URL.revokeObjectURL(url), 5000)
}

/** 导出文件名：spaceId 空串/含路径非法字符时兜底 'space'。 */
function exportName(ext: string): string {
  const safe = (props.spaceId ?? '').replace(/[\\/:*?"<>|]/g, '').trim()
  return `知识导图-${safe || 'space'}.${ext}`
}

function exportSvg() {
  if (!root.value) return
  download(exportName('svg'), new Blob([buildExportSvg()], { type: 'image/svg+xml;charset=utf-8' }))
}

// PNG = 把独立 SVG 喂给 <img> 再画上 canvas（2 倍采样防糊）；失败只提示，不污染导图本身
async function exportPng() {
  if (!root.value || exporting.value) return
  exporting.value = true
  note.value = ''
  try {
    const url = URL.createObjectURL(new Blob([buildExportSvg()], { type: 'image/svg+xml;charset=utf-8' }))
    try {
      const img = new Image()
      img.src = url
      await img.decode()
      const scale = 2
      const canvas = document.createElement('canvas')
      canvas.width = Math.ceil((img.naturalWidth || layout.value.width) * scale)
      canvas.height = Math.ceil((img.naturalHeight || layout.value.height) * scale)
      const ctx = canvas.getContext('2d')
      if (!ctx) throw new Error('canvas 2d 不可用')
      ctx.scale(scale, scale)
      ctx.drawImage(img, 0, 0)
      const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, 'image/png'))
      if (!blob) throw new Error('PNG 编码失败')
      download(exportName('png'), blob)
    } finally {
      URL.revokeObjectURL(url)
    }
  } catch {
    note.value = '导出 PNG 失败，可改用导出 SVG。'
    noteKind.value = 'warn'
  } finally {
    exporting.value = false
  }
}

onMounted(() => { void load() })
onBeforeUnmount(() => { mindmapEpoch++ })
</script>

<template>
  <div class="mindmap-panel">
    <div class="mindmap-head">
      <div>
        <h3>知识导图</h3>
        <span>按主题层级归纳当前空间文档；分支圆点折叠/展开，点击文档节点展开章节，点击章节查看内容摘要（分支折叠状态按空间记忆）。</span>
      </div>
      <div class="mindmap-tools">
        <button
          class="secondary-btn" type="button"
          :disabled="!root || exporting" title="把当前导图导出为 PNG 图片（2 倍清晰度）"
          @click="exportPng"
        >{{ exporting ? '导出中…' : '导出 PNG' }}</button>
        <button
          class="secondary-btn" type="button"
          :disabled="!root" title="把当前导图导出为 SVG 矢量图"
          @click="exportSvg"
        >导出 SVG</button>
        <button
          class="secondary-btn" type="button"
          :disabled="regenerating || !writable"
          :title="writable ? '重新归纳当前空间文档生成导图' : '只读空间不能重新生成'"
          @click="regenerate"
        >{{ regenerating ? '生成中…' : '重新生成' }}</button>
      </div>
    </div>
    <div v-if="note" class="mindmap-note" :class="noteKind" role="status">{{ note }}</div>

    <div class="mindmap-stage">
      <div class="mindmap-canvas">
        <div v-if="loading" class="mindmap-state" role="status">
          <strong>正在读取知识导图</strong><span>导图较大时需要几秒钟。</span>
        </div>
        <div v-else-if="!root" class="mindmap-state">
          <strong>{{ !spaceId ? '请先选择知识空间' : unavailable ? '知识导图暂不可用' : '导图尚未生成' }}</strong>
          <span>{{ !spaceId ? '在左侧选择一个知识空间后展示导图。' : unavailable ? '服务端导图接口尚未就绪，请刷新页面重试。' : '点击右上角「重新生成」按当前空间文档归纳导图。' }}</span>
          <button v-if="writable && !unavailable && spaceId" class="primary-btn" type="button" :disabled="regenerating" @click="regenerate">重新生成</button>
        </div>
        <svg
          v-else :width="layout.width" :height="layout.height" :viewBox="`0 0 ${layout.width} ${layout.height}`"
          preserveAspectRatio="xMinYMid" role="img" aria-label="知识导图"
        >
          <path
            v-for="(edge, index) in layout.edges" :key="index"
            class="mm-edge" :stroke="branchColor(edge.branch)"
            :d="`M ${edge.x1} ${edge.y1} C ${edge.x1 + EDGE_CP} ${edge.y1}, ${edge.x2 - EDGE_CP} ${edge.y2}, ${edge.x2} ${edge.y2}`"
          />
          <g v-for="node in layout.nodes" :key="node.key" :transform="`translate(${node.x}, ${node.y})`">
            <circle
              class="mm-dot" :class="{ collapsed: node.collapsed, leaf: !node.hasChildren && !node.docId, doc: !!node.docId, section: !!node.chunkCount }"
              :stroke="node.depth === 0 ? 'var(--primary)' : branchColor(node.branch)"
              :fill="node.collapsed || (!node.hasChildren && !node.docId) ? (node.depth === 0 ? 'var(--primary)' : branchColor(node.branch)) : 'var(--bg-card)'"
              :r="node.chunkCount ? 3.5 : 4.5"
            />
            <!-- 透明命中圆：圆点 r=4.5 点击目标过小，交互/键盘/ARIA 都挂这层；
                 圆点=展开/收起（有子级），文字=主动作（摘要卡等），两层分工见 onDotClick/onLabelClick -->
            <circle
              v-if="isClickable(node)" class="mm-hit" r="12" role="button" tabindex="0"
              :aria-label="dotActionLabel(node)"
              @click="onDotClick(node)" @keydown.enter.prevent="onDotClick(node)" @keydown.space.prevent="onDotClick(node)"
            />
            <text
              class="mm-label" :class="{ root: node.depth === 0, clickable: isClickable(node) }"
              x="11" y="4" @click="onLabelClick(node)"
            >{{ node.label }}<title v-if="node.label !== node.name">{{ node.name }}</title></text>
            <g v-if="node.docCount" class="mm-badge" :transform="`translate(${16 + node.labelW}, 0)`">
              <rect
                :width="badgeWidth(node.docCount)" height="14" y="-8" rx="7"
                :stroke="node.depth === 0 ? 'var(--primary)' : branchColor(node.branch)"
              />
              <text :x="badgeWidth(node.docCount) / 2" y="2.5">{{ badgeText(node.docCount) }}</text>
            </g>
            <g v-if="node.chunkCount" class="mm-badge chunks" :transform="`translate(${16 + node.labelW}, 0)`">
              <rect
                :width="badgeWidth(node.chunkCount)" height="14" y="-8" rx="7"
                :stroke="branchColor(node.branch)"
              />
              <text :x="badgeWidth(node.chunkCount) / 2" y="2.5">{{ badgeText(node.chunkCount) }}</text>
            </g>
            <text
              v-if="node.hiddenCount" class="mm-count"
              :x="18 + node.labelW + (node.docCount ? badgeWidth(node.docCount) + 5 : 0)" y="4"
            >+{{ node.hiddenCount }}</text>
          </g>
        </svg>
      </div>
      <aside v-if="activeSection" class="mm-card" role="dialog" aria-label="章节摘要" @keydown.esc="activeSection = null">
        <header>
          <strong :title="activeSection.name">{{ activeSection.name }}</strong>
          <button ref="cardCloseBtn" type="button" class="mm-card-close" title="关闭摘要" aria-label="关闭摘要" @click="activeSection = null">×</button>
        </header>
        <div class="mm-card-meta">
          {{ activeSection.docName }} · {{ activeSection.chunkCount }} 块<template v-if="activeSection.page != null"> · 第 {{ activeSection.page }} 页起</template>
        </div>
        <p>{{ activeSection.excerpt || '（本节无摘录内容）' }}</p>
      </aside>
    </div>
  </div>
</template>

<style scoped>
.mindmap-panel { width: 100%; display: flex; flex-direction: column; }
.mindmap-head { display: flex; align-items: flex-end; gap: 16px; }
.mindmap-head h3 { color: var(--text-primary); font-size: 14px; }
.mindmap-head span { display: block; margin-top: 3px; color: var(--text-muted); font-size: 11.5px; }
.mindmap-tools { margin-left: auto; display: flex; flex-wrap: wrap; gap: 8px; }
.secondary-btn, .primary-btn {
  height: 32px; border: 1px solid var(--border); border-radius: 6px; cursor: pointer; font: inherit; font-size: 12px;
}
.secondary-btn { padding: 0 13px; background: var(--bg-card); color: var(--text-regular); white-space: nowrap; }
.secondary-btn:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-light); }
.primary-btn { padding: 0 13px; border-color: var(--primary); background: var(--primary); color: #fff; }
.primary-btn:hover { background: var(--primary-hover); }
button:disabled { cursor: not-allowed; opacity: .55; }
.mindmap-note { margin-top: 8px; padding: 7px 10px; border-left: 3px solid var(--warning-text); background: var(--warning-bg); color: var(--warning-text); font-size: 11.5px; }
.mindmap-note.ok { border-left-color: var(--success-text); background: var(--success-bg); color: var(--success-text); }
.mindmap-note.info { border-left-color: var(--text-faint); background: var(--bg-main); color: var(--text-muted); }
/* stage 包住滚动画布与摘要卡：卡片相对 stage 定位，横向滚动导图时不跟滚 */
.mindmap-stage { position: relative; margin-top: 12px; }
.mindmap-canvas {
  min-height: 430px; overflow: auto;
  border: 1px solid var(--border); border-radius: 6px; background: var(--bg-main);
}
.mindmap-canvas svg { display: block; min-width: 100%; }
.mindmap-state {
  position: absolute; inset: 0; display: flex; align-items: center; justify-content: center;
  flex-direction: column; gap: 8px; padding: 20px; color: var(--text-muted); text-align: center; font-size: 12px;
}
.mindmap-state strong { color: var(--text-primary); font-size: 14px; }
.mindmap-state span { max-width: 460px; line-height: 1.6; }
.mindmap-state .primary-btn { margin-top: 6px; }
.mm-edge { fill: none; stroke-width: 1.4; opacity: .75; }
.mm-dot { stroke-width: 1.6; }
.mm-dot.doc { stroke-dasharray: 2.5 2; }
.mm-hit { fill: transparent; cursor: pointer; outline: none; }
.mm-hit:focus { stroke: var(--primary); stroke-width: 1.2; }
.mm-badge.chunks rect { stroke-dasharray: 3 2; }
.mm-card {
  position: absolute; top: 12px; right: 12px; z-index: 2; width: min(290px, calc(100% - 24px)); max-height: calc(100% - 24px);
  display: flex; flex-direction: column; padding: 12px 14px; overflow: auto;
  border: 1px solid var(--border); border-radius: 8px; background: var(--bg-card); box-shadow: var(--shadow-md);
}
.mm-card header { display: flex; align-items: flex-start; gap: 8px; }
.mm-card header strong {
  min-width: 0; flex: 1; overflow: hidden; color: var(--text-primary); font-size: 13px;
  text-overflow: ellipsis; white-space: nowrap;
}
.mm-card-close {
  flex: none; width: 22px; height: 22px; padding: 0; border: 1px solid var(--border); border-radius: 5px;
  background: var(--bg-card); color: var(--text-regular); cursor: pointer; font: inherit; font-size: 14px; line-height: 1;
}
.mm-card-close:hover { border-color: var(--primary); color: var(--primary); }
.mm-card-meta { margin-top: 5px; color: var(--text-faint); font-size: 11px; }
.mm-card p {
  margin: 8px 0 0; color: var(--text-regular); font-size: 12px; line-height: 1.7;
  white-space: pre-wrap; overflow-wrap: anywhere;
}
.mm-label { fill: var(--text-regular); font-size: 12px; }
.mm-label.root { fill: var(--text-primary); font-size: 13px; font-weight: 700; }
.mm-label.clickable { cursor: pointer; }
.mm-label.clickable:hover { fill: var(--primary); }
.mm-count { fill: var(--text-faint); font-size: 10px; }
.mm-badge rect { fill: none; stroke-width: 1; opacity: .85; }
.mm-badge text { fill: var(--text-faint); font-size: 9.5px; text-anchor: middle; }
@media (max-width: 820px) {
  .mindmap-head { align-items: stretch; flex-direction: column; gap: 10px; }
  .mindmap-tools { margin-left: 0; }
  .mindmap-canvas { min-height: 320px; }
}
</style>
