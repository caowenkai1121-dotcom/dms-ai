<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'
import { FONT_FAMILY, GRAPH_PALETTE } from './panel-utils'

// docId ＝文档叶子；chunkCount ＝章节节点（内容级，由 sections 端点懒加载嫁接，不是服务端树的固有层）
interface MNode {
  name: string; children: MNode[]
  docId?: string; chunkCount?: number; excerpt?: string; page?: number | null
}
type NodeKind = 'root' | 'branch' | 'doc' | 'section' | 'leaf'
interface LayoutNode {
  key: string; parentKey: string; name: string; label: string; kind: NodeKind
  depth: number; x: number; y: number; w: number; branch: number
  icon: string; iconW: number; labelW: number
  toggle: boolean; expanded: boolean; hiddenCount: number; docCount: number
  docId: string | null; chunkCount: number; excerpt: string; page: number | null
}
interface LayoutEdge { x1: number; y1: number; x2: number; y2: number; branch: number }
interface SectionCard { docId: string; docName: string; name: string; chunkCount: number; excerpt: string; page: number | null }

const props = defineProps<{ token?: string; spaceId?: string; writable?: boolean }>()
const emit = defineEmits<{ (e: 'auth-expired'): void }>()

const PALETTE = GRAPH_PALETTE
const ROW_H = 38          // 叶节点行高：可见子树按行分配纵向空间，节点再多也不重叠
const COL_GAP = 48
/** 贝塞尔边控制点水平偏移（模板与导出 SVG 共用，只维护一处）。 */
const EDGE_CP = 40
const CAP_H = 26          // 胶囊高（根节点 30）
const CAP_PAD = 9         // 胶囊左内边距
const CAP_TAIL = 10       // 胶囊右内边距
const TOGGLE_R = 6.5      // 展收圆钮半径
const TOGGLE_STUB = 15    // 展收钮 + 边引出线在列宽里占的水平空间
const ZOOM_MIN = 0.2
const ZOOM_MAX = 3
/** 展开记忆最多保留的空间数（防 localStorage 残留无限膨胀）。 */
const EXPAND_MAX_SPACES = 8

const root = ref<MNode | null>(null)
const loading = ref(false)
const unavailable = ref(false)
const regenerating = ref(false)
const note = ref('')
/** 提示条分级：warn 错误黄（默认）/ ok 成功绿 / info 中性灰。 */
const noteKind = ref<'warn' | 'ok' | 'info'>('warn')
const expandedKeys = ref<string[]>([])
const exporting = ref(false)
// 章节缓存只活在本会话：不按空间记忆——章节数据是懒加载的，展开即拉取
const docSections = ref<Record<string, MNode[]>>({})
const loadingDoc = ref('')
const activeSection = ref<SectionCard | null>(null)
const cardCloseBtn = ref<HTMLButtonElement | null>(null)
const wrapEl = ref<HTMLDivElement>()
const svgEl = ref<SVGSVGElement>()
const panning = ref(false)
let mindmapEpoch = 0

// —— 无限画布视口：screen = view.o + world × view.scale，与 KbGraph 同一套变换口径 ——
const view = reactive({ scale: 1, ox: 0, oy: 0 })
// sx/sy 是平移锚点（随 move 更新）；ix/iy 是按下原点（3px 阈值内仍算点击，不丢点选语义）
const drag = { active: false, sx: 0, sy: 0, ix: 0, iy: 0, moved: false }
/** 平移拖拽松手后的那一次 click 必须吞掉，否则拖完画布会误触节点/空白点击。 */
let suppressClick = false

// 展开状态记忆：按空间存 localStorage（键是名称路径+兄弟序号，跨会话稳定；导图重生成后的失效键无害，
// 它们只会「展开一个不存在的路径」，不落任何副作用）。默认只展开根（第一级可见），其余一律点击才展开。
const EXPAND_PREFIX = 'kb_mindmap_expanded:'
const EXPAND_INDEX = 'kb_mindmap_expanded_spaces'
function storageKey(): string {
  return `${EXPAND_PREFIX}${props.spaceId ?? ''}`
}
function restoreExpanded(): string[] {
  if (!props.spaceId) return []
  try {
    // 旧版按「折叠集合」记忆，语义相反，一次性清掉避免残留
    localStorage.removeItem(`kb_mindmap_collapsed:${props.spaceId}`)
    localStorage.removeItem('kb_mindmap_collapsed_spaces')
    const arr: unknown = JSON.parse(localStorage.getItem(storageKey()) ?? '[]')
    return Array.isArray(arr) ? arr.filter((k): k is string => typeof k === 'string') : []
  } catch {
    return []
  }
}
function saveExpanded() {
  if (!props.spaceId) return
  try {
    localStorage.setItem(storageKey(), JSON.stringify(expandedKeys.value))
    // 按前缀裁剪：只保留最近 N 个空间的记忆，删除/改名的空间键不永久残留
    const index: unknown = JSON.parse(localStorage.getItem(EXPAND_INDEX) ?? '[]')
    const prev = Array.isArray(index) ? index.filter((s): s is string => typeof s === 'string') : []
    const nextList = [props.spaceId, ...prev.filter((s) => s !== props.spaceId)].slice(0, EXPAND_MAX_SPACES)
    for (const s of prev) if (!nextList.includes(s)) localStorage.removeItem(`${EXPAND_PREFIX}${s}`)
    localStorage.setItem(EXPAND_INDEX, JSON.stringify(nextList))
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

/** 12px 字号的估算字宽（全角 12.5 / 半角 7），factor 供根节点 13px 加粗放大。 */
function labelWidth(name: string, factor = 1): number {
  let width = 0
  for (const ch of name) width += ch.charCodeAt(0) > 0xff ? 12.5 : 7
  return Math.min(240, width * factor + 4)
}

/** 名称截断：胶囊文本上限约 216px，全名由 <title> 兜底。 */
function clipName(name: string): string {
  let width = 0
  let i = 0
  for (; i < name.length; i++) {
    width += name.charCodeAt(i) > 0xff ? 12.5 : 7
    if (width > 216) break
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

// 节点语义类型：根 / 分支目录 / 文档（docId）/ 章节（chunkCount）/ 纯叶子
function kindOf(node: MNode, depth: number): NodeKind {
  if (depth === 0) return 'root'
  if (node.docId) return 'doc'
  if (node.chunkCount) return 'section'
  return node.children.length ? 'branch' : 'leaf'
}
const NODE_ICON: Record<NodeKind, string> = { root: '📚', branch: '📁', doc: '📄', section: '§', leaf: '•' }
const NODE_ICON_W: Record<NodeKind, number> = { root: 18, branch: 17, doc: 17, section: 11, leaf: 8 }

// 展示树＝服务端骨架 + 已拉取文档下嫁接的章节节点（原树保持纯净，重生成/换空间直接丢嫁接层）。
// 是否「看见」章节由展开集合控制，与缓存解耦——恢复的记忆键不会因缓存缺席而显示空分支。
const displayRoot = computed<MNode | null>(() => {
  const tree = root.value
  if (!tree) return null
  const graft = (node: MNode): MNode => {
    if (node.docId && docSections.value[node.docId]) {
      return { ...node, children: docSections.value[node.docId] }
    }
    return { ...node, children: node.children.map(graft) }
  }
  return graft(tree)
})

const layout = computed(() => {
  const tree = displayRoot.value
  if (!tree) return { nodes: [] as LayoutNode[], edges: [] as LayoutEdge[], width: 0, height: 0 }
  const expanded = new Set(expandedKeys.value)

  // 子树计数单趟自底向上预算（未展开分支的 hiddenCount 也要用）：主遍历不再每层重复递归
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

  /** 胶囊宽度（cap＝视觉胶囊，foot＝含展收钮的列宽占位）：测量与排版共用，两处不漂移。 */
  const capsule = (node: MNode, depth: number): { cap: number; foot: number; iconW: number; labelW: number } => {
    const kind = kindOf(node, depth)
    const labelW = labelWidth(clipName(node.name), depth === 0 ? 1.12 : 1)
    const iconW = NODE_ICON_W[kind]
    const c = counts.get(node) ?? { docs: 0, descendants: 0 }
    let cap = CAP_PAD + iconW + labelW
    if (!node.docId && !node.chunkCount && c.docs) cap += 5 + badgeWidth(c.docs)
    if (node.chunkCount) cap += 5 + badgeWidth(node.chunkCount)
    cap += CAP_TAIL
    const foot = cap + (node.children.length > 0 || node.docId ? TOGGLE_STUB : 0)
    return { cap, foot, iconW, labelW }
  }

  // 列宽按全量展开量：分支展开/收起时列不跳动
  const colWidths: number[] = []
  const measure = (node: MNode, depth: number) => {
    colWidths[depth] = Math.max(colWidths[depth] ?? 0, capsule(node, depth).foot)
    for (const child of node.children) measure(child, depth + 1)
  }
  measure(tree, 0)
  const colX: number[] = [24]
  for (let d = 1; d < colWidths.length; d++) colX[d] = colX[d - 1] + (colWidths[d - 1] ?? 0) + COL_GAP

  const nodes: LayoutNode[] = []
  const edges: LayoutEdge[] = []
  let leaf = 0
  // 只遍历可见节点（未展开分支整个不进渲染列表，>500 节点也只在展开时增量布局）；
  // 可见叶节点自上而下各占一行，父节点取首末子节点中点——按可见子树高度分配纵向空间。
  // key 是名称路径 + 兄弟序号（同父同名不撞 key、不串展开状态），parentKey 直接记录不靠字符串切分
  const visit = (node: MNode, depth: number, branch: number, path: string, index: number): number => {
    const key = path ? `${path}/${node.name}#${index}` : node.name
    const isOpen = expanded.has(key) && node.children.length > 0
    const m = capsule(node, depth)
    const x = colX[depth] ?? depth * 220
    let y: number
    if (!node.children.length || !isOpen) {
      y = 30 + leaf * ROW_H
      leaf++
    } else {
      const childYs = node.children.map((child, i) => visit(child, depth + 1, depth === 0 ? i : branch, key, i))
      y = (childYs[0] + childYs[childYs.length - 1]) / 2
    }
    const c = counts.get(node) ?? { docs: 0, descendants: 0 }
    const kind = kindOf(node, depth)
    nodes.push({
      key, parentKey: path, name: node.name, label: clipName(node.name), kind, depth, x, y,
      w: m.cap, branch, icon: NODE_ICON[kind], iconW: m.iconW, labelW: m.labelW,
      toggle: node.children.length > 0 || !!node.docId,
      expanded: isOpen,
      hiddenCount: !isOpen && node.children.length ? c.descendants : 0,
      docCount: node.docId || node.chunkCount ? 0 : c.docs,
      docId: node.docId ?? null,
      chunkCount: node.chunkCount ?? 0,
      excerpt: node.excerpt ?? '',
      page: node.page ?? null,
    })
    return y
  }
  visit(tree, 0, 0, '', 0)
  // 父节点坐标在子节点之后才定，边在第二遍按 parentKey 补；起点让开展收钮（+TOGGLE_STUB）
  const byKey = new Map(nodes.map((n) => [n.key, n]))
  for (const node of nodes) {
    if (node.depth === 0) continue
    const parent = byKey.get(node.parentKey)
    if (parent) edges.push({ x1: parent.x + parent.w + (parent.toggle ? TOGGLE_STUB : 2), y1: parent.y, x2: node.x, y2: node.y, branch: node.branch })
  }
  const width = Math.max(400, (colX[colX.length - 1] ?? 0) + (colWidths[colWidths.length - 1] ?? 120) + 40)
  const height = Math.max(160, leaf * ROW_H + 60)
  return { nodes, edges, width, height }
})

function toggle(key: string) {
  const opening = !expandedKeys.value.includes(key)
  expandedKeys.value = opening
    ? [...expandedKeys.value, key]
    : expandedKeys.value.filter((k) => k !== key)
  saveExpanded()
  // 收起分支时若摘要卡属于其下章节（含该文档本身），卡片一并关（内容对应节点已不可见）
  if (!opening && activeSection.value) {
    const docKey = layout.value.nodes.find((n) => n.docId === activeSection.value?.docId)?.key
    if (docKey && (docKey === key || docKey.startsWith(`${key}/`))) activeSection.value = null
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
  // 以排版态为准：恢复的记忆键在缓存缺席时不算展开（isOpen 要求子级真实存在），
  // 这里据此决定「收起」还是「拉取并展开」， stale 键不会把首次点击误判成收起。
  if (node.expanded) {
    toggle(node.key)
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
  if (!expandedKeys.value.includes(node.key)) {
    expandedKeys.value = [...expandedKeys.value, node.key]
    saveExpanded()
  }
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

// 点击路由分两层（用户核心诉求：有子级就能展收，且只有点击才展收，hover 不触发）——
// 圆形 +/− 钮 = 展开/收起；胶囊本体 = 主动作（章节出摘要卡 / 文档展收章节 / 分支展收）。
// 带子级的章节两个动作都够得着：点圆钮展子章节、点胶囊看摘要。
function onToggle(node: LayoutNode) {
  if (suppressClick) return
  if (node.docId) { void toggleDoc(node); return }
  toggle(node.key)
}
function onCapsuleClick(node: LayoutNode) {
  if (suppressClick) return
  if (node.chunkCount) { openSection(node); return }
  if (node.docId) { void toggleDoc(node); return }
  if (node.toggle) toggle(node.key)
}
function onBlankClick() {
  if (suppressClick) return
  // 点画布空白（未拖动）关闭摘要卡：不用非得点×
  activeSection.value = null
}

/** 可交互判定：纯叶子（无子节点、非文档、非章节）不可点，不给 pointer 光标。 */
function isClickable(node: LayoutNode): boolean {
  return node.toggle || !!node.chunkCount
}
function toggleLabel(node: LayoutNode): string {
  if (node.docId) return `${node.expanded ? '收起' : '展开'}文档 ${node.name} 的章节`
  return node.expanded ? `收起 ${node.name}` : `展开 ${node.name}`
}

function branchColor(branch: number): string {
  return PALETTE[((branch % PALETTE.length) + PALETTE.length) % PALETTE.length]
}
function nodeColor(node: LayoutNode): string {
  return node.depth === 0 ? 'var(--primary)' : branchColor(node.branch)
}

// ==================== 无限画布：平移 / 缩放 ====================

function canvasSize(): { w: number; h: number } {
  const el = wrapEl.value
  return { w: el?.clientWidth ?? 600, h: el?.clientHeight ?? 430 }
}

/** 围绕画布上某点缩放：该点下的世界坐标缩放前后不动（滚轮＝鼠标中心，按钮＝画布中心）。 */
function zoomAt(cx: number, cy: number, factor: number) {
  const wx = (cx - view.ox) / view.scale
  const wy = (cy - view.oy) / view.scale
  const next = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, view.scale * factor))
  view.scale = next
  view.ox = cx - wx * next
  view.oy = cy - wy * next
}

function onWheel(event: WheelEvent) {
  const rect = svgEl.value?.getBoundingClientRect()
  zoomAt(event.clientX - (rect?.left ?? 0), event.clientY - (rect?.top ?? 0), event.deltaY < 0 ? 1.12 : 0.89)
}

/** 缩放按钮（键盘/触屏的滚轮替代）：围绕画布中心缩放。 */
function zoomBy(factor: number) {
  const { w, h } = canvasSize()
  zoomAt(w / 2, h / 2, factor)
}

/** 复位 100%：内容小于画布则居中，否则回到左上留白起点。 */
function resetView() {
  const { w, h } = canvasSize()
  view.scale = 1
  view.ox = layout.value.width < w ? (w - layout.value.width) / 2 : 24
  view.oy = layout.value.height < h ? (h - layout.value.height) / 2 : 20
}

/** 适应屏幕：整棵树缩放进可视区并居中（加载/重生成后自动做一次）。 */
function fitView() {
  const { w, h } = canvasSize()
  const lw = Math.max(1, layout.value.width)
  const lh = Math.max(1, layout.value.height)
  const s = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, Math.min((w - 48) / lw, (h - 48) / lh)))
  view.scale = s
  view.ox = (w - lw * s) / 2
  view.oy = (h - lh * s) / 2
}

const zoomPercent = computed(() => Math.round(view.scale * 100))
const viewTransform = computed(() => `translate(${view.ox} ${view.oy}) scale(${view.scale})`)

function onPointerDown(event: PointerEvent) {
  if (event.button !== 0) return
  drag.active = true
  drag.moved = false
  drag.sx = drag.ix = event.clientX
  drag.sy = drag.iy = event.clientY
  // 合成/已释放的 pointerId 会抛 NotFoundError：捕获失败不拖累平移本身
  try { svgEl.value?.setPointerCapture(event.pointerId) } catch { /* 无捕获也能平移 */ }
}

function onPointerMove(event: PointerEvent) {
  if (!drag.active) return
  // 3px 位移阈值：点击手滑 1px 不丢「点选」语义
  if (!drag.moved && Math.hypot(event.clientX - drag.ix, event.clientY - drag.iy) < 3) return
  drag.moved = true
  panning.value = true
  view.ox += event.clientX - drag.sx
  view.oy += event.clientY - drag.sy
  drag.sx = event.clientX
  drag.sy = event.clientY
}

function endDrag(event: PointerEvent) {
  if (drag.active && drag.moved) {
    suppressClick = true
    // click 与 pointerup 同任务派发，下一任务再复位：吞掉的只是拖拽收尾那一次点击
    window.setTimeout(() => { suppressClick = false }, 0)
  }
  drag.active = false
  panning.value = false
  // 未持捕获时 release 会抛 DOMException（pointercancel 后即是如此）
  const svg = svgEl.value
  if (svg && svg.hasPointerCapture(event.pointerId)) svg.releasePointerCapture(event.pointerId)
}

async function load() {
  const epoch = ++mindmapEpoch
  loading.value = true
  unavailable.value = false
  note.value = ''
  expandedKeys.value = restoreExpanded()
  root.value = null
  // 内容级状态随树一起失效（章节挂在 doc_id 上，换空间/重生成后旧嫁接层无意义）
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
    // 默认只展开根（第一级可见）；恢复的记忆键叠加在其上
    const rootKey = root.value?.name
    if (rootKey && !expandedKeys.value.includes(rootKey)) {
      expandedKeys.value = [rootKey, ...expandedKeys.value]
    }
    if (root.value) void nextTick(() => { if (epoch === mindmapEpoch) fitView() })
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
    const color = branchColor(e.branch)
    const d = `M ${e.x1} ${e.y1} C ${e.x1 + EDGE_CP} ${e.y1}, ${e.x2 - EDGE_CP} ${e.y2}, ${e.x2} ${e.y2}`
    parts.push(`<path d="${d}" fill="none" stroke="${color}" stroke-width="1.3" opacity="0.8"/>`)
  }
  for (const n of ns) {
    const color = n.depth === 0 ? primary : branchColor(n.branch)
    const h = n.depth === 0 ? 30 : CAP_H
    const parts1: string[] = [
      `<rect width="${n.w}" height="${h}" y="${-h / 2}" rx="${h / 2}" fill="${color}" fill-opacity="0.09" stroke="${color}" stroke-width="1.2"/>`,
      `<text x="${CAP_PAD}" y="4" font-size="12" ${font}>${n.icon}</text>`,
    ]
    const fs = n.depth === 0 ? 13 : 12
    const fw = n.depth === 0 ? ' font-weight="700"' : ''
    parts1.push(`<text x="${CAP_PAD + n.iconW}" y="${n.depth === 0 ? 4.5 : 4}" fill="${n.depth === 0 ? textPrimary : textRegular}" font-size="${fs}"${fw} ${font}>${escXml(n.label)}</text>`)
    const bx = CAP_PAD + n.iconW + n.labelW + 5
    if (n.docCount) {
      const w = badgeWidth(n.docCount)
      parts1.push(`<rect x="${bx}" y="-7" width="${w}" height="14" rx="7" fill="${color}" fill-opacity="0.12" stroke="${color}" stroke-width="0.8" stroke-opacity="0.55"/>`)
      parts1.push(`<text x="${bx + w / 2}" y="2.5" fill="${textFaint}" font-size="9.5" text-anchor="middle" ${font}>${badgeText(n.docCount)}</text>`)
    }
    if (n.chunkCount) {
      const w = badgeWidth(n.chunkCount)
      parts1.push(`<rect x="${bx}" y="-7" width="${w}" height="14" rx="7" fill="${color}" fill-opacity="0.12" stroke="${color}" stroke-width="0.8" stroke-opacity="0.55" stroke-dasharray="3 2"/>`)
      parts1.push(`<text x="${bx + w / 2}" y="2.5" fill="${textFaint}" font-size="9.5" text-anchor="middle" ${font}>${badgeText(n.chunkCount)}</text>`)
    }
    if (n.toggle) {
      parts1.push(`<circle cx="${n.w + 1}" r="${TOGGLE_R}" fill="${bg}" stroke="${color}" stroke-width="1.3"/>`)
      parts1.push(`<text x="${n.w + 1}" y="3.5" fill="${color}" font-size="11" font-weight="600" text-anchor="middle" ${font}>${n.expanded ? '−' : '+'}</text>`)
    }
    if (n.hiddenCount) {
      parts1.push(`<text x="${n.w + 12}" y="3.5" fill="${textFaint}" font-size="10" ${font}>+${n.hiddenCount}</text>`)
    }
    parts.push(`<g transform="translate(${n.x}, ${n.y})">${parts1.join('')}</g>`)
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
        <span>按主题层级归纳当前空间文档；点圆形 +/− 钮展开/收起分支，点击文档节点展开章节，点击章节查看内容摘要；拖拽空白平移、滚轮缩放（展开状态按空间记忆）。</span>
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
      <div ref="wrapEl" class="mindmap-canvas">
        <div v-if="loading" class="mindmap-state" role="status">
          <strong>正在读取知识导图</strong><span>导图较大时需要几秒钟。</span>
        </div>
        <div v-else-if="!root" class="mindmap-state">
          <strong>{{ !spaceId ? '请先选择知识空间' : unavailable ? '知识导图暂不可用' : '导图尚未生成' }}</strong>
          <span>{{ !spaceId ? '在左侧选择一个知识空间后展示导图。' : unavailable ? '服务端导图接口尚未就绪，请刷新页面重试。' : '点击右上角「重新生成」按当前空间文档归纳导图。' }}</span>
          <button v-if="writable && !unavailable && spaceId" class="primary-btn" type="button" :disabled="regenerating" @click="regenerate">重新生成</button>
        </div>
        <svg
          v-else ref="svgEl" class="mm-svg" :class="{ panning }" role="img" aria-label="知识导图"
          @pointerdown="onPointerDown" @pointermove="onPointerMove" @pointerup="endDrag" @pointercancel="endDrag"
          @wheel.prevent="onWheel"
        >
          <!-- 透明背景层：空白点击（关摘要卡）的命中区；平移在 svg 上统一捕获，不依赖此层 -->
          <rect class="mm-bg" width="100%" height="100%" @click="onBlankClick" />
          <g :transform="viewTransform">
            <path
              v-for="(edge, index) in layout.edges" :key="index"
              class="mm-edge" :stroke="branchColor(edge.branch)"
              :d="`M ${edge.x1} ${edge.y1} C ${edge.x1 + EDGE_CP} ${edge.y1}, ${edge.x2 - EDGE_CP} ${edge.y2}, ${edge.x2} ${edge.y2}`"
            />
            <g v-for="node in layout.nodes" :key="node.key" :transform="`translate(${node.x}, ${node.y})`">
              <g class="mm-node" :class="{ clickable: isClickable(node) }" @click="onCapsuleClick(node)">
                <title v-if="node.label !== node.name">{{ node.name }}</title>
                <rect
                  class="mm-cap" :class="{ root: node.depth === 0 }"
                  :width="node.w" :height="node.depth === 0 ? 30 : CAP_H" :y="node.depth === 0 ? -15 : -CAP_H / 2" :rx="node.depth === 0 ? 15 : CAP_H / 2"
                  :fill="nodeColor(node)" :stroke="nodeColor(node)"
                />
                <text class="mm-icon" :x="CAP_PAD" y="4">{{ node.icon }}</text>
                <text
                  class="mm-label" :class="{ root: node.depth === 0 }"
                  :x="CAP_PAD + node.iconW" :y="node.depth === 0 ? 4.5 : 4"
                >{{ node.label }}</text>
                <g v-if="node.docCount" class="mm-badge" :transform="`translate(${CAP_PAD + node.iconW + node.labelW + 5}, 0)`">
                  <rect
                    :width="badgeWidth(node.docCount)" height="14" y="-7" rx="7"
                    :fill="nodeColor(node)" :stroke="nodeColor(node)"
                  />
                  <text :x="badgeWidth(node.docCount) / 2" y="2.5">{{ badgeText(node.docCount) }}</text>
                </g>
                <g v-if="node.chunkCount" class="mm-badge chunks" :transform="`translate(${CAP_PAD + node.iconW + node.labelW + 5}, 0)`">
                  <rect
                    :width="badgeWidth(node.chunkCount)" height="14" y="-7" rx="7"
                    :fill="nodeColor(node)" :stroke="nodeColor(node)"
                  />
                  <text :x="badgeWidth(node.chunkCount) / 2" y="2.5">{{ badgeText(node.chunkCount) }}</text>
                </g>
              </g>
              <!-- 展收钮：只有点击它（或胶囊本体）才展开/收起，hover 不触发；
                   透明命中圆把 r=6.5 的钮扩到 r=11 点击目标，交互/键盘/ARIA 都挂这层 -->
              <g
                v-if="node.toggle" class="mm-toggle" role="button" tabindex="0"
                :aria-label="toggleLabel(node)" :aria-expanded="node.expanded"
                :transform="`translate(${node.w + 1}, 0)`"
                @click.stop="onToggle(node)" @keydown.enter.prevent="onToggle(node)" @keydown.space.prevent="onToggle(node)"
              >
                <circle class="mm-toggle-hit" r="11" />
                <circle class="mm-toggle-face" :r="TOGGLE_R" :stroke="nodeColor(node)" />
                <text class="mm-toggle-sign" y="3.5" :fill="nodeColor(node)">{{ node.expanded ? '−' : '+' }}</text>
              </g>
              <text v-if="node.hiddenCount" class="mm-count" :x="node.w + 12" y="3.5">+{{ node.hiddenCount }}</text>
            </g>
          </g>
        </svg>
        <!-- 缩放/视角工具条：滚轮之外的键盘与触屏替代 -->
        <div v-if="!loading && root" class="mm-zoom">
          <button type="button" title="适应屏幕" aria-label="适应屏幕" @click="fitView">⤢</button>
          <button type="button" title="放大" aria-label="放大" @click="zoomBy(1.25)">+</button>
          <button type="button" title="缩小" aria-label="缩小" @click="zoomBy(0.8)">−</button>
          <button type="button" class="mm-zoom-pct" title="复位 100%" aria-label="复位 100%" @click="resetView">{{ zoomPercent }}%</button>
        </div>
        <div v-if="!loading && root" class="mm-hint" aria-hidden="true">拖拽平移 · 滚轮缩放 · 节点 {{ layout.nodes.length }}</div>
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
/* stage 包住无限画布与摘要卡：卡片相对 stage 定位，平移/缩放导图时不跟动 */
.mindmap-stage { position: relative; margin-top: 12px; }
.mindmap-canvas {
  position: relative; min-height: 460px; overflow: hidden;
  border: 1px solid var(--border); border-radius: 8px; background: var(--bg-main);
}
.mm-svg {
  position: absolute; inset: 0; display: block; width: 100%; height: 100%;
  cursor: grab; user-select: none; touch-action: none;
}
.mm-svg.panning { cursor: grabbing; }
.mm-bg { fill: transparent; }
.mindmap-state {
  position: absolute; inset: 0; display: flex; align-items: center; justify-content: center;
  flex-direction: column; gap: 8px; padding: 20px; color: var(--text-muted); text-align: center; font-size: 12px;
}
.mindmap-state strong { color: var(--text-primary); font-size: 14px; }
.mindmap-state span { max-width: 460px; line-height: 1.6; }
.mindmap-state .primary-btn { margin-top: 6px; }
/* —— 横向树：分支线/胶囊描边同 branch 色，胶囊底色是该色 9% Tint（柔和 pastel 观感） —— */
.mm-edge { fill: none; stroke-width: 1.3; opacity: .8; }
.mm-cap { fill-opacity: .09; stroke-width: 1.2; transition: fill-opacity .15s ease; }
.mm-node.clickable .mm-cap { cursor: pointer; }
.mm-node.clickable:hover .mm-cap { fill-opacity: .2; }
.mm-icon, .mm-label, .mm-badge { pointer-events: none; }
.mm-icon { font-size: 12px; }
.mm-label { fill: var(--text-regular); font-size: 12px; }
.mm-label.root { fill: var(--text-primary); font-size: 13px; font-weight: 700; }
.mm-badge rect { fill-opacity: .12; stroke-width: .8; stroke-opacity: .55; }
.mm-badge.chunks rect { stroke-dasharray: 3 2; }
.mm-badge text { fill: var(--text-faint); font-size: 9.5px; text-anchor: middle; }
.mm-count { fill: var(--text-faint); font-size: 10px; pointer-events: none; }
.mm-toggle { cursor: pointer; outline: none; }
.mm-toggle-hit { fill: transparent; }
.mm-toggle-face { fill: var(--bg-card); stroke-width: 1.3; }
.mm-toggle:hover .mm-toggle-face, .mm-toggle:focus .mm-toggle-face { stroke-width: 2; }
.mm-toggle:focus .mm-toggle-face { stroke: var(--primary); }
.mm-toggle-sign { font-size: 11px; font-weight: 600; text-anchor: middle; pointer-events: none; }
/* 视角工具条（与知识图谱同款位置/观感） */
.mm-zoom { position: absolute; left: 10px; top: 10px; z-index: 2; display: flex; flex-direction: column; gap: 4px; }
.mm-zoom button {
  min-width: 26px; height: 26px; padding: 0 4px; border: 1px solid var(--border); border-radius: 6px;
  background: var(--bg-card); color: var(--text-regular); cursor: pointer; font-size: 13px; line-height: 1;
}
.mm-zoom button:hover { border-color: var(--primary); color: var(--primary); }
.mm-zoom .mm-zoom-pct { font-size: 10px; font-variant-numeric: tabular-nums; }
.mm-hint {
  position: absolute; left: 10px; bottom: 8px; color: var(--text-faint); font-size: 11px;
  font-variant-numeric: tabular-nums; pointer-events: none;
}
.mm-card {
  position: absolute; top: 12px; right: 12px; z-index: 2; width: min(300px, calc(100% - 24px)); max-height: calc(100% - 24px);
  display: flex; flex-direction: column; padding: 13px 15px; overflow: auto;
  border: 1px solid var(--border); border-radius: 10px; background: var(--bg-card); box-shadow: var(--shadow-md);
}
.mm-card header {
  display: flex; align-items: flex-start; gap: 8px; padding-bottom: 8px; border-bottom: 1px solid var(--border);
}
.mm-card header strong {
  min-width: 0; flex: 1; overflow: hidden; color: var(--text-primary); font-size: 13px;
  text-overflow: ellipsis; white-space: nowrap;
}
.mm-card-close {
  flex: none; width: 22px; height: 22px; padding: 0; border: 1px solid var(--border); border-radius: 5px;
  background: var(--bg-card); color: var(--text-regular); cursor: pointer; font: inherit; font-size: 14px; line-height: 1;
}
.mm-card-close:hover { border-color: var(--primary); color: var(--primary); }
.mm-card-meta { margin-top: 8px; color: var(--text-faint); font-size: 11px; }
.mm-card p {
  margin: 8px 0 0; color: var(--text-regular); font-size: 12px; line-height: 1.7;
  white-space: pre-wrap; overflow-wrap: anywhere;
}
@media (max-width: 820px) {
  .mindmap-head { align-items: stretch; flex-direction: column; gap: 10px; }
  .mindmap-tools { margin-left: 0; }
  .mindmap-canvas { min-height: 340px; }
}
</style>
