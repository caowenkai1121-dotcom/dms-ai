<script setup lang="ts">
import { computed, markRaw, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { FONT_FAMILY, GRAPH_PALETTE } from './panel-utils'

interface GNode {
  id: string; label: string; type: string; weight: number
  x: number; y: number; vx: number; vy: number; r: number; color: string
}
interface GEdge { source: number; target: number; label: string }
interface Neighbor { label: string; relation: string }
// 后端契约 {id,name,label=实体类型,weight} / {src,dst,relation,weight} 归一化后的中间形态
interface RawGraphNode { id: string; label: string; type: string; weight: number }
interface RawGraphEdge { srcId: string; dstId: string; label: string }

const props = defineProps<{ token?: string; spaceId?: string; writable?: boolean }>()
const emit = defineEmits<{ (e: 'auth-expired'): void }>()

const PALETTE = GRAPH_PALETTE

// —— 力导布局常量（调参只改这里）——
const REPULSION = 2600       // 节点间斥力系数
const SPRING = 0.02          // 边弹簧系数
const GRAVITY = 0.012        // 向心引力
const DAMPING = 0.86         // 速度阻尼
const ALPHA_DECAY = 0.995    // 冷却速率
const ALPHA_MIN = 0.015      // 低于此值停止 tick
const REPULSION_CUTOFF2 = 160000  // 斥力截断距离的平方：远距节点互不算，省 O(n²) 里的常数
const LABEL_MAX_NODES = 260  // 画布节点超过此不画标签（全景不糊字）
const LABEL_MAX_EDGES = 40   // 边数超过此只为焦点/放大态画边标签
const SUBGRAPH_LIMIT = 200   // 首屏子图节点上限
const EXPAND_LIMIT = 120     // 邻居展开节点上限
const BUILD_TIMEOUT_MS = 10 * 60 * 1000  // 构建轮询最长 10 分钟
const POLL_MS = 2000         // 构建状态轮询间隔
const RESUME_POLL_MS = 1200  // 接入在途构建的首次轮询间隔
const POLL_MAX_FAILURES = 3  // 轮询连续失败这么多次才放弃（瞬断不放弃）

const wrapEl = ref<HTMLDivElement>()
const canvasEl = ref<HTMLCanvasElement>()
const loading = ref(false)
const unavailable = ref(false)
const note = ref('')
/** 提示条分级：warn 错误黄（默认）/ ok 成功绿 / info 中性灰。 */
const noteKind = ref<'warn' | 'ok' | 'info'>('warn')
const nodes = ref<GNode[]>([])
const edges = ref<GEdge[]>([])
const statEntities = ref<number | null>(null)
const statRelations = ref<number | null>(null)
const building = ref(false)
const buildPercent = ref<number | null>(null)
const buildMessage = ref('')
const buildingStartedAt = ref(0)
// —— Y4 运营区：失败块抽屉 + 清空/修复。端点未注册（404）时一律降级为提示，不影响画布 ——
const failedOpen = ref(false)
const failedLoading = ref(false)
const failedItems = ref<Array<{ chunk_id: number; doc_id: string; ord: number; kind: string; error: string | null }>>([])
const failedTotal = ref(0)
const failedOffset = ref(0)
const FAILED_PAGE = 50
/** 失败块 kind 白名单映射：未知 kind 显示原值、走中性样式（不冒充「失败」警示色）。 */
const FAILED_KIND_LABEL: Record<string, string> = { failed: '失败', pending: '待建' }
const resetting = ref(false)
const reconciling = ref(false)
const hoverId = ref('')
const selectedId = ref('')
const search = ref('')
const expanding = ref(false)
// 画布节点软上限：力导布局是 O(n²)，超出后不再展开邻居（刷新可重来）
const MAX_CANVAS_NODES = 800

let raf = 0
let alpha = 0
let resizeObserver: ResizeObserver | null = null
let themeObserver: MutationObserver | null = null
let pollTimer = 0
let pollFailures = 0
let graphEpoch = 0
const view = { scale: 1, ox: 0, oy: 0 }
// sx/sy 是平移锚点（随 move 更新）；ix/iy 是按下原点（判 moved 阈值）；gx/gy 是抓取偏移
const drag = { mode: '' as '' | 'node' | 'pan', id: '', sx: 0, sy: 0, ix: 0, iy: 0, gx: 0, gy: 0, moved: false }

function headers(): Record<string, string> {
  const token = props.token?.trim()
  if (!token) {
    emit('auth-expired')
    throw new Error('登录会话已失效，请重新登录。')
  }
  return { Authorization: `Bearer ${token}` }
}
/** headers() 抛的会话失效消息要能被 catch 透出（不认的话会被显示成「图谱暂不可用」误导）。 */
function sessionMsg(e: unknown): string {
  return e instanceof Error && e.message.includes('登录会话') ? e.message : ''
}
function setNote(text: string, kind: 'warn' | 'ok' | 'info' = 'warn') {
  note.value = text
  noteKind.value = kind
}

// 类型着色：同一实体类型哈希到同一颜色（跨次加载稳定）；未标注类型用固定中性色，
// 图例里「未标注」一行的颜色才与节点一致（按 index 取色会让同类实体五颜六色）。
function colorOf(type: string): string {
  if (!type) return '#8b93ad'
  let hash = 0
  for (let i = 0; i < type.length; i++) hash = (hash * 31 + type.charCodeAt(i)) | 0
  return PALETTE[Math.abs(hash) % PALETTE.length]
}

/** 标签截断加省略号：截过的名字不该被当成全名。 */
function clipText(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n)}…` : s
}

// 响应 → 归一化中间形态。名称优先取 name（label 在契约里是实体类型，不是显示名；
// name 缺失时回退用 label 作显示名）；边端点优先取 src/dst（服务端契约），再回退各种兼容键。
function parseGraph(data: Record<string, unknown>): { nodes: RawGraphNode[]; edges: RawGraphEdge[] } {
  const rawNodes = Array.isArray(data.nodes) ? data.nodes : []
  const rawEdges = Array.isArray(data.edges) ? data.edges : []
  const seenId = new Set<string>()
  const nodes: RawGraphNode[] = []
  rawNodes.forEach((raw, index) => {
    if (!raw || typeof raw !== 'object') return
    const item = raw as Record<string, unknown>
    const id = String(item.id ?? item.name ?? item.label ?? index)
    if (seenId.has(id)) return
    seenId.add(id)
    // weight 允许显式 0（0 权节点照样画，只是半径最小）；非数字/缺失才落 1
    const w = Number(item.weight ?? item.count ?? 1)
    const weight = Number.isFinite(w) ? Math.max(0, w) : 1
    const hasName = item.name != null && String(item.name) !== ''
    nodes.push({
      id,
      label: String(hasName ? item.name : (item.label ?? id)),
      type: String(item.type ?? item.kind ?? item.entity_type ?? (hasName ? item.label ?? '' : '')),
      weight,
    })
  })
  const ids = new Set(nodes.map((n) => n.id))
  const seenEdge = new Set<string>()
  const edges: RawGraphEdge[] = []
  for (const raw of rawEdges) {
    if (!raw || typeof raw !== 'object') continue
    const item = raw as Record<string, unknown>
    const srcId = String(item.src ?? item.source ?? item.from ?? item.source_id ?? '')
    const dstId = String(item.dst ?? item.target ?? item.to ?? item.target_id ?? '')
    if (!ids.has(srcId) || !ids.has(dstId) || srcId === dstId) continue
    const label = String(item.relation ?? item.label ?? item.type ?? '')
    const key = `${srcId}|${dstId}|${label}`
    if (seenEdge.has(key)) continue
    seenEdge.add(key)
    edges.push({ srcId, dstId, label })
  }
  return { nodes, edges }
}

// 中间形态 → 画布节点：首屏按环形播种；邻居展开时落在锚点周围（新节点不会乱飞）。
// markRaw：tick 每帧改写 x/y/vx/vy，深响应式 proxy setter 是纯浪费（模板只读 label/type/weight）。
function toGNode(raw: RawGraphNode, index: number, total: number, anchor?: GNode): GNode {
  const angle = (index / Math.max(1, total)) * Math.PI * 2
  const radius = anchor ? 80 + Math.random() * 60 : 180
  return markRaw({
    id: raw.id,
    label: raw.label,
    type: raw.type,
    weight: raw.weight,
    x: (anchor?.x ?? 0) + Math.cos(angle) * radius + (Math.random() - 0.5) * 40,
    y: (anchor?.y ?? 0) + Math.sin(angle) * radius + (Math.random() - 0.5) * 40,
    vx: 0, vy: 0,
    r: Math.min(26, 6 + Math.sqrt(raw.weight) * 3.2),
    color: colorOf(raw.type),
  })
}

function normalizeGraph(data: Record<string, unknown>): { nodes: GNode[]; edges: GEdge[] } {
  const parsed = parseGraph(data)
  const nodes = parsed.nodes.map((n, i) => toGNode(n, i, parsed.nodes.length))
  const indexById = new Map(nodes.map((n, i) => [n.id, i]))
  const edges: GEdge[] = []
  for (const e of parsed.edges) {
    const source = indexById.get(e.srcId)
    const target = indexById.get(e.dstId)
    if (source != null && target != null) edges.push({ source, target, label: e.label })
  }
  return { nodes, edges }
}

// 邻域子图合并进当前视图：已有节点保留原位，新节点落在锚点周围；边按 (src,dst,label) 去重。
// 返回新增节点/边数（都为 0 = 没有更多邻居，调用方给提示）。
function mergeSubgraph(data: Record<string, unknown>, anchorId: string): { added: number; addedEdges: number } {
  const parsed = parseGraph(data)
  const indexById = new Map(nodes.value.map((n, i) => [n.id, i]))
  const anchor = nodes.value.find((n) => n.id === anchorId)
  let added = 0
  let addedEdges = 0
  for (const raw of parsed.nodes) {
    if (indexById.has(raw.id)) continue
    indexById.set(raw.id, nodes.value.length)
    nodes.value.push(toGNode(raw, nodes.value.length, parsed.nodes.length, anchor))
    added++
  }
  const have = new Set(edges.value.map((e) => `${nodes.value[e.source]?.id}|${nodes.value[e.target]?.id}|${e.label}`))
  for (const raw of parsed.edges) {
    const source = indexById.get(raw.srcId)
    const target = indexById.get(raw.dstId)
    if (source == null || target == null) continue
    const key = `${raw.srcId}|${raw.dstId}|${raw.label}`
    if (have.has(key)) continue
    have.add(key)
    edges.value.push({ source, target, label: raw.label })
    addedEdges++
  }
  return { added, addedEdges }
}

function canvasSize(): { w: number; h: number } {
  const el = wrapEl.value
  return { w: el?.clientWidth ?? 600, h: el?.clientHeight ?? 420 }
}

function resizeCanvas() {
  const canvas = canvasEl.value
  if (!canvas) return
  const { w, h } = canvasSize()
  const dpr = window.devicePixelRatio || 1
  canvas.width = Math.max(1, Math.round(w * dpr))
  canvas.height = Math.max(1, Math.round(h * dpr))
  canvas.style.width = `${w}px`
  canvas.style.height = `${h}px`
  render()
}

function wake(strength = 0.3) {
  alpha = Math.max(alpha, strength)
  if (!raf) raf = requestAnimationFrame(tick)
}

function tick() {
  raf = 0
  const ns = nodes.value
  const es = edges.value
  if (!ns.length) return
  for (let i = 0; i < ns.length; i++) {
    const a = ns[i]
    for (let j = i + 1; j < ns.length; j++) {
      const b = ns[j]
      let dx = a.x - b.x
      let dy = a.y - b.y
      let dist2 = dx * dx + dy * dy
      if (dist2 > REPULSION_CUTOFF2) continue
      if (dist2 < 0.01) { dx = Math.random() - 0.5; dy = Math.random() - 0.5; dist2 = 1 }
      const force = (REPULSION / dist2) * alpha
      const dist = Math.sqrt(dist2)
      const fx = (dx / dist) * force
      const fy = (dy / dist) * force
      a.vx += fx; a.vy += fy
      b.vx -= fx; b.vy -= fy
    }
  }
  for (const edge of es) {
    const a = ns[edge.source]
    const b = ns[edge.target]
    const dx = b.x - a.x
    const dy = b.y - a.y
    const dist = Math.max(1, Math.hypot(dx, dy))
    const desired = 64 + a.r + b.r
    const force = (dist - desired) * SPRING * alpha
    const fx = (dx / dist) * force
    const fy = (dy / dist) * force
    a.vx += fx; a.vy += fy
    b.vx -= fx; b.vy -= fy
  }
  for (const node of ns) {
    node.vx += -node.x * GRAVITY * alpha
    node.vy += -node.y * GRAVITY * alpha
    if (drag.mode === 'node' && drag.id === node.id) { node.vx = 0; node.vy = 0; continue }
    node.vx *= DAMPING
    node.vy *= DAMPING
    node.x += node.vx
    node.y += node.vy
  }
  alpha *= ALPHA_DECAY
  render()
  if (alpha > ALPHA_MIN || drag.mode === 'node') raf = requestAnimationFrame(tick)
}

function toWorld(event: PointerEvent | MouseEvent | WheelEvent): { x: number; y: number } {
  const canvas = canvasEl.value
  const rect = canvas?.getBoundingClientRect()
  const cx = (rect ? event.clientX - rect.left : 0)
  const cy = (rect ? event.clientY - rect.top : 0)
  const { w, h } = canvasSize()
  return {
    x: (cx - w / 2 - view.ox) / view.scale,
    y: (cy - h / 2 - view.oy) / view.scale,
  }
}

function hitNode(wx: number, wy: number): GNode | null {
  const ns = nodes.value
  for (let i = ns.length - 1; i >= 0; i--) {
    const node = ns[i]
    if (Math.hypot(node.x - wx, node.y - wy) <= node.r + 3) return node
  }
  return null
}

const matchSet = computed(() => {
  const needle = search.value.trim().toLowerCase()
  if (!needle) return null
  return new Set(nodes.value.filter((n) => n.label.toLowerCase().includes(needle)).map((n) => n.id))
})
const matchCount = computed(() => matchSet.value?.size ?? 0)

// 类型图例：当前画布出现过的实体类型 → 颜色（与节点着色同一函数，顺序按首次出现）
const legendAll = computed(() => {
  const seen = new Map<string, string>()
  for (const n of nodes.value) {
    const key = n.type || '未标注'
    if (!seen.has(key)) seen.set(key, colorOf(n.type))
  }
  return [...seen.entries()].map(([type, color]) => ({ type, color }))
})
const LEGEND_MAX = 12
const legend = computed(() => legendAll.value.slice(0, LEGEND_MAX))
const legendMore = computed(() => Math.max(0, legendAll.value.length - LEGEND_MAX))

// 搜索即定位：高亮由 matchSet/isDimmed 完成；这里防抖把最佳命中（权重最高的骨干实体）
// 平移到画布中心。变换口径与 render 一致：screen = w/2 + ox + x*scale。
let searchTimer = 0
watch(search, () => {
  render()
  window.clearTimeout(searchTimer)
  searchTimer = window.setTimeout(() => {
    const matches = matchSet.value
    if (!matches?.size) return
    let best: GNode | null = null
    for (const n of nodes.value) {
      if (matches.has(n.id) && (!best || n.weight > best.weight)) best = n
    }
    if (!best) return
    view.ox = -best.x * view.scale
    view.oy = -best.y * view.scale
    render()
  }, 240)
})

// 焦点邻接 Set 预算（computed 缓存）：render 每帧逐节点查表 O(1)，不再逐节点 edges.some O(N·E)
const focusNeighborSet = computed(() => {
  const focus = hoverId.value || selectedId.value
  if (!focus) return null
  const set = new Set<string>([focus])
  for (const e of edges.value) {
    const s = nodes.value[e.source]?.id
    const t = nodes.value[e.target]?.id
    if (s === focus && t) set.add(t)
    if (t === focus && s) set.add(s)
  }
  return set
})

function isDimmed(node: GNode): boolean {
  const matches = matchSet.value
  if (matches) return !matches.has(node.id)
  const adj = focusNeighborSet.value
  if (!adj) return false
  return !adj.has(node.id)
}

function edgeDimmed(edge: GEdge): boolean {
  const focus = hoverId.value || selectedId.value
  const sourceId = nodes.value[edge.source]?.id
  const targetId = nodes.value[edge.target]?.id
  if (!focus) {
    // 搜索态：两端都命中的边保留（命中路径信息不丢），其余压暗
    const matches = matchSet.value
    if (!matches) return false
    return !(matches.has(sourceId ?? '') && matches.has(targetId ?? ''))
  }
  return sourceId !== focus && targetId !== focus
}

function render() {
  const canvas = canvasEl.value
  const ctx = canvas?.getContext('2d')
  if (!canvas || !ctx) return
  const dpr = window.devicePixelRatio || 1
  const { w, h } = canvasSize()
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
  ctx.clearRect(0, 0, w, h)
  const ns = nodes.value
  if (!ns.length) return
  ctx.translate(w / 2 + view.ox, h / 2 + view.oy)
  ctx.scale(view.scale, view.scale)
  const focus = hoverId.value || selectedId.value
  const dark = document.documentElement.dataset.theme === 'dark'
  const labelColor = dark ? 'rgba(232,235,246,.88)' : 'rgba(16,22,43,.78)'
  const faintColor = dark ? 'rgba(232,235,246,.18)' : 'rgba(16,22,43,.12)'
  const drawLabels = ns.length <= LABEL_MAX_NODES
  for (const edge of edges.value) {
    const a = ns[edge.source]
    const b = ns[edge.target]
    const dim = edgeDimmed(edge)
    ctx.strokeStyle = dim ? faintColor : dark ? 'rgba(139,147,173,.6)' : 'rgba(100,109,135,.55)'
    ctx.lineWidth = !dim && focus ? 1.8 : 1
    ctx.beginPath()
    ctx.moveTo(a.x, a.y)
    ctx.lineTo(b.x, b.y)
    ctx.stroke()
    // 关系边标签：缩放到位（≥1）、挂在焦点节点上、或边足够少时才画 —— 全景不糊字
    if (edge.label && drawLabels && (view.scale >= 1 || (!dim && !!focus) || edges.value.length <= LABEL_MAX_EDGES)) {
      ctx.fillStyle = dim ? faintColor : dark ? 'rgba(139,147,173,.9)' : 'rgba(100,109,135,.85)'
      ctx.font = `9px ${FONT_FAMILY}`
      ctx.textAlign = 'center'
      ctx.fillText(clipText(edge.label, 12), (a.x + b.x) / 2, (a.y + b.y) / 2 - 3)
    }
  }
  for (const node of ns) {
    const dim = isDimmed(node)
    ctx.globalAlpha = dim ? 0.22 : 1
    ctx.beginPath()
    ctx.arc(node.x, node.y, node.r, 0, Math.PI * 2)
    ctx.fillStyle = node.color
    ctx.fill()
    if (node.id === hoverId.value || node.id === selectedId.value) {
      ctx.lineWidth = 2.5
      ctx.strokeStyle = dark ? '#e8ebf6' : '#10162b'
      ctx.stroke()
    }
    // 搜索命中的节点加外圈描边：缩略状态下也比「未命中变暗」多一层正反馈
    if (matchSet.value?.has(node.id)) {
      ctx.lineWidth = 2
      ctx.strokeStyle = dark ? '#e8ebf6' : '#10162b'
      ctx.beginPath()
      ctx.arc(node.x, node.y, node.r + 3.5, 0, Math.PI * 2)
      ctx.stroke()
    }
    if (drawLabels) {
      ctx.fillStyle = dim ? faintColor : labelColor
      ctx.font = `${node.r > 14 ? 11 : 10}px ${FONT_FAMILY}`
      ctx.textAlign = 'center'
      ctx.fillText(clipText(node.label, 10), node.x, node.y + node.r + 11)
    }
    ctx.globalAlpha = 1
  }
}

function onPointerDown(event: PointerEvent) {
  const point = toWorld(event)
  const node = hitNode(point.x, point.y)
  drag.mode = node ? 'node' : 'pan'
  drag.id = node?.id ?? ''
  drag.sx = event.clientX
  drag.sy = event.clientY
  drag.ix = event.clientX
  drag.iy = event.clientY
  drag.moved = false
  if (node) {
    // 记录抓取偏移：节点中心不瞬移到指针（点大节点边缘不跳）
    drag.gx = point.x - node.x
    drag.gy = point.y - node.y
    wake(0.4)
  }
  canvasEl.value?.setPointerCapture(event.pointerId)
}

function onPointerMove(event: PointerEvent) {
  const point = toWorld(event)
  if (drag.mode === 'node') {
    const node = nodes.value.find((n) => n.id === drag.id)
    if (node) {
      node.x = point.x - drag.gx
      node.y = point.y - drag.gy
      // 3px 位移阈值：点击手滑 1px 不丢「点选」语义
      if (!drag.moved && Math.hypot(event.clientX - drag.ix, event.clientY - drag.iy) >= 3) drag.moved = true
      wake(0.2)
    }
    return
  }
  if (drag.mode === 'pan') {
    view.ox += event.clientX - drag.sx
    view.oy += event.clientY - drag.sy
    drag.sx = event.clientX
    drag.sy = event.clientY
    if (!drag.moved && Math.hypot(event.clientX - drag.ix, event.clientY - drag.iy) >= 3) drag.moved = true
    render()
    return
  }
  const node = hitNode(point.x, point.y)
  const next = node?.id ?? ''
  if (next !== hoverId.value) {
    hoverId.value = next
    if (canvasEl.value) canvasEl.value.style.cursor = node ? 'pointer' : 'default'
    render()
  }
}

function endDrag(event: PointerEvent) {
  drag.mode = ''
  drag.id = ''
  // 未持捕获时 release 会抛 DOMException（pointercancel 后即是如此）
  const canvas = canvasEl.value
  if (canvas && canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId)
}

function onPointerUp(event: PointerEvent) {
  if (drag.mode === 'node' && !drag.moved) {
    selectedId.value = selectedId.value === drag.id ? '' : drag.id
    render()
  } else if (drag.mode === 'pan' && !drag.moved && selectedId.value) {
    // 点画布空白（未拖动）清除选中：详情卡不用非得点×或再点节点
    selectedId.value = ''
    render()
  }
  endDrag(event)
}

function onPointerCancel(event: PointerEvent) {
  // 触摸被打断：drag 状态必须收尾，否则拖曳态滞留
  endDrag(event)
}

function onWheel(event: WheelEvent) {
  const before = toWorld(event)
  view.scale = Math.min(3, Math.max(0.25, view.scale * (event.deltaY < 0 ? 1.12 : 0.89)))
  const { w, h } = canvasSize()
  const rect = canvasEl.value?.getBoundingClientRect()
  const cx = rect ? event.clientX - rect.left : w / 2
  const cy = rect ? event.clientY - rect.top : h / 2
  view.ox = cx - w / 2 - before.x * view.scale
  view.oy = cy - h / 2 - before.y * view.scale
  render()
}

/** 缩放按钮（键盘/触屏的滚轮替代）：围绕画布中心缩放。 */
function zoomBy(factor: number) {
  view.scale = Math.min(3, Math.max(0.25, view.scale * factor))
  render()
}
function resetView() {
  view.scale = 1
  view.ox = 0
  view.oy = 0
  render()
}

const selectedNode = computed(() => nodes.value.find((n) => n.id === selectedId.value) ?? null)
const selectedNeighbors = computed<Neighbor[]>(() => {
  const node = selectedNode.value
  if (!node) return []
  const rows: Neighbor[] = []
  const seen = new Set<string>()
  for (const edge of edges.value) {
    const sourceId = nodes.value[edge.source]?.id
    const targetId = nodes.value[edge.target]?.id
    let label = ''
    if (sourceId === node.id) label = nodes.value[edge.target]?.label ?? ''
    else if (targetId === node.id) label = nodes.value[edge.source]?.label ?? ''
    else continue
    if (!label) continue
    const relation = edge.label || '关联'
    // 双向同关系边（A→B、B→A 同 label）只显示一条，:key 也不撞
    const key = `${relation}|${label}`
    if (seen.has(key)) continue
    seen.add(key)
    rows.push({ label, relation })
    if (rows.length >= 8) break
  }
  return rows
})
const selectedDegree = computed(() => {
  const node = selectedNode.value
  if (!node) return 0
  return edges.value.filter((e) => nodes.value[e.source]?.id === node.id || nodes.value[e.target]?.id === node.id).length
})

async function loadStats(epoch: number) {
  try {
    const response = await fetch(`/api/kb/graph/stats?space_id=${encodeURIComponent(props.spaceId ?? '')}`, { headers: headers() })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    const data = await response.json().catch(() => ({}))
    if (epoch !== graphEpoch) return
    statEntities.value = Number(data.entity_count ?? data.entities ?? data.nodes ?? NaN)
    statRelations.value = Number(data.relation_count ?? data.relations ?? data.edges ?? NaN)
    if (!Number.isFinite(statEntities.value)) statEntities.value = null
    if (!Number.isFinite(statRelations.value)) statRelations.value = null
  } catch { /* 统计接口缺席时不占位：底部仍显示当前画布计数 */ }
}

async function loadSubgraph(epoch: number) {
  loading.value = true
  unavailable.value = false
  try {
    const response = await fetch(`/api/kb/graph/subgraph?space_id=${encodeURIComponent(props.spaceId ?? '')}&limit=${SUBGRAPH_LIMIT}`, { headers: headers() })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    const data = await response.json().catch(() => ({}))
    if (epoch !== graphEpoch) return
    const graph = normalizeGraph(data)
    nodes.value = graph.nodes
    edges.value = graph.edges
    hoverId.value = ''
    selectedId.value = ''
    view.scale = 1
    view.ox = 0
    view.oy = 0
    wake(1)
  } catch (e) {
    if (epoch === graphEpoch) {
      nodes.value = []
      edges.value = []
      unavailable.value = true
      // 会话失效要说真实原因（需重新登录），不是「图谱暂不可用」
      const msg = sessionMsg(e)
      if (msg) setNote(msg)
      render()
    }
  } finally {
    if (epoch === graphEpoch) loading.value = false
  }
}

function normalizeStatus(data: Record<string, unknown>): { running: boolean; percent: number | null; message: string } {
  const raw = String(data.status ?? data.state ?? '').toLowerCase()
  const running = ['building', 'running', 'processing', 'pending'].includes(raw)
  let percent = Number(data.percent ?? NaN)
  if (!Number.isFinite(percent)) {
    const progress = Number(data.progress ?? NaN)
    percent = Number.isFinite(progress) ? (progress <= 1 ? progress * 100 : progress) : NaN
  }
  if (!Number.isFinite(percent)) {
    // 服务端契约是 {state,total,done,...}：没有百分数字段时用 done/total 推算
    const total = Number(data.total ?? NaN)
    const done = Number(data.done ?? NaN)
    percent = Number.isFinite(total) && total > 0 && Number.isFinite(done) ? (done / total) * 100 : NaN
  }
  return {
    running,
    percent: Number.isFinite(percent) ? Math.min(100, Math.max(0, percent)) : null,
    message: String(data.message ?? data.stage ?? ''),
  }
}

async function pollStatus(epoch: number) {
  window.clearTimeout(pollTimer)
  if (epoch !== graphEpoch || !building.value) return
  try {
    const response = await fetch(`/api/kb/graph/status?space_id=${encodeURIComponent(props.spaceId ?? '')}`, { headers: headers() })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    const data = await response.json().catch(() => ({}))
    if (epoch !== graphEpoch || !building.value) return
    pollFailures = 0
    const status = normalizeStatus(data)
    buildPercent.value = status.percent
    buildMessage.value = status.message
    if (status.running) {
      if (Date.now() - buildingStartedAt.value < BUILD_TIMEOUT_MS) {
        pollTimer = window.setTimeout(() => void pollStatus(epoch), POLL_MS)
      } else {
        building.value = false
        setNote('构建超时，请稍后手动刷新查看结果。')
      }
    } else {
      building.value = false
      buildPercent.value = null
      buildMessage.value = ''
      await loadSubgraph(epoch)
      await loadStats(epoch)
    }
  } catch (e) {
    if (epoch !== graphEpoch) return
    // 会话失效是持续性错误：直接停轮询并透出真实原因，不做无意义重试
    const session = sessionMsg(e)
    if (session) {
      building.value = false
      buildPercent.value = null
      setNote(session)
      return
    }
    // 轮询瞬断（网络抖动）有限重试：连续失败才放弃，服务端构建仍在跑
    pollFailures += 1
    if (pollFailures < POLL_MAX_FAILURES) {
      pollTimer = window.setTimeout(() => void pollStatus(epoch), POLL_MS + 1000)
    } else {
      building.value = false
      buildPercent.value = null
      setNote('图谱构建状态查询暂不可用。')
    }
  }
}

// 挂载/切换空间后先查一次构建状态：服务端仍在构建时直接接入轮询，
// 不必等用户再点一次「构建图谱」。状态接口缺席时静默——画布展示不受影响。
async function resumeBuilding(epoch: number) {
  try {
    const response = await fetch(`/api/kb/graph/status?space_id=${encodeURIComponent(props.spaceId ?? '')}`, { headers: headers() })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) return
    const data = await response.json().catch(() => ({}))
    if (epoch !== graphEpoch || building.value) return
    const status = normalizeStatus(data)
    if (!status.running) return
    building.value = true
    buildPercent.value = status.percent
    buildMessage.value = status.message
    buildingStartedAt.value = Date.now()
    pollTimer = window.setTimeout(() => void pollStatus(epoch), RESUME_POLL_MS)
  } catch { /* 静默：见函数注释 */ }
}

async function build() {
  if (building.value || !props.writable) return
  const epoch = graphEpoch
  note.value = ''
  // fetch 前置位：快速双击不会发出两个 POST（与 KbMindmap regenerate 同口径）
  building.value = true
  try {
    const response = await fetch('/api/kb/graph/build', {
      method: 'POST',
      headers: { ...headers(), 'Content-Type': 'application/json' },
      body: JSON.stringify({ space_id: props.spaceId ?? '' }),
    })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    if (epoch !== graphEpoch) return
    pollFailures = 0
    buildPercent.value = 0
    buildingStartedAt.value = Date.now()
    pollTimer = window.setTimeout(() => void pollStatus(epoch), RESUME_POLL_MS)
  } catch (e) {
    if (epoch === graphEpoch) {
      building.value = false
      setNote(sessionMsg(e) || '图谱构建接口暂不可用。')
    }
  }
}

// —— Y4 运营区：失败块抽屉 + 清空/修复 ——

// 运营端点统一错误处理：非 2xx 优先透出服务端 {error} 文案（409 的「超闸拒删/构建中」
// 就靠它说清）；端点未注册（404）降级为「尚未上线」的明示，不装死。
async function opsError(response: Response, name: string): Promise<Error> {
  if (response.status === 404) return new Error(`图谱运营接口尚未上线（${name}），编排方注册后可用。`)
  const data = await response.json().catch(() => ({}))
  return new Error(String(data.error ?? `HTTP ${response.status}`))
}

async function loadFailed(offset: number) {
  if (!props.spaceId) return
  failedLoading.value = true
  const epoch = graphEpoch
  try {
    const url = `/api/kb/graph/failed-chunks?space_id=${encodeURIComponent(props.spaceId)}&limit=${FAILED_PAGE}&offset=${offset}`
    const response = await fetch(url, { headers: headers() })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw await opsError(response, 'failed-chunks')
    const data = await response.json().catch(() => ({}))
    if (epoch !== graphEpoch) return
    failedItems.value = Array.isArray(data.items) ? data.items : []
    failedTotal.value = Number(data.total ?? 0) || 0
    // 服务端不回 offset 字段时按本次请求的 offset 记：第 2 页数据不配第 1 页的页码
    const echo = Number(data.offset)
    failedOffset.value = Number.isFinite(echo) ? echo : offset
  } catch (e) {
    if (epoch === graphEpoch) setNote(e instanceof Error ? e.message : '失败块清单读取失败。')
  } finally {
    if (epoch === graphEpoch) failedLoading.value = false
  }
}

function toggleFailed() {
  failedOpen.value = !failedOpen.value
  if (failedOpen.value) {
    // 打开抽屉前清掉上一条操作提示（「图谱已清空」之类），不残留错语境
    note.value = ''
    void loadFailed(0)
  }
}

// 清空图谱：确认里写明后果（删实体与关系、不动文档）；完成后整图重载。
async function resetGraph() {
  if (resetting.value || !props.writable || !props.spaceId) return
  if (!window.confirm(`确认清空空间「${props.spaceId}」的知识图谱？\n实体与关系将被删除（文档本身不受影响），清空后需重新构建。`)) return
  resetting.value = true
  note.value = ''
  const epoch = graphEpoch
  try {
    const response = await fetch('/api/kb/graph/reset', {
      method: 'POST',
      headers: { ...headers(), 'Content-Type': 'application/json' },
      body: JSON.stringify({ space_id: props.spaceId }),
    })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw await opsError(response, 'reset')
    if (epoch !== graphEpoch) return
    failedOpen.value = false
    // reload() 会 ++graphEpoch 令 finally 的 epoch 判等永假：先复位 busy 再重载；
    // 成功文案也要放在 reload 之后（reload 会清 note）
    resetting.value = false
    await reload()
    setNote('图谱已清空。', 'ok')
  } catch (e) {
    if (epoch === graphEpoch) setNote(e instanceof Error ? e.message : '图谱清空失败。')
  } finally {
    if (epoch === graphEpoch) resetting.value = false
  }
}

// 修复图谱：先 dry-run 拿统计 → 确认后真删；超执行闸/构建中（409）直接透出服务端文案。
async function reconcileGraph() {
  if (reconciling.value || !props.writable || !props.spaceId) return
  reconciling.value = true
  note.value = ''
  const epoch = graphEpoch
  try {
    const post = (dryRun: boolean) => fetch('/api/kb/graph/reconcile', {
      method: 'POST',
      headers: { ...headers(), 'Content-Type': 'application/json' },
      body: JSON.stringify({ space_id: props.spaceId, dry_run: dryRun }),
    })
    const probe = await post(true)
    if (probe.status === 401) emit('auth-expired')
    if (!probe.ok) throw await opsError(probe, 'reconcile')
    const plan = await probe.json().catch(() => ({}))
    if (epoch !== graphEpoch) return
    // Number(...) || 0：非数字字符串（NaN）按 0 处理，不误判「需要/无需修复」
    const orphans = Number(plan.orphan_chunks) || 0
    const dangling = Number(plan.dangling_entities) || 0
    const relations = Number(plan.relations_from_orphans) || 0
    if (!orphans && !dangling && !relations) {
      setNote('图谱无需修复：没有孤儿块或悬空实体。', 'info')
      return
    }
    if (!window.confirm(`文档删改遗留：孤儿块 ${orphans}、悬空实体 ${dangling}、孤儿关系 ${relations}。\n确认清理？（只删图数据，不动文档）`)) return
    const done = await post(false)
    if (done.status === 401) emit('auth-expired')
    if (!done.ok) throw await opsError(done, 'reconcile')
    const result = await done.json().catch(() => ({}))
    if (epoch !== graphEpoch) return
    const d = result.deleted ?? {}
    const doneText = `修复完成：清理孤儿块 ${d.chunks ?? 0}、悬空实体 ${d.entities ?? 0}、关系 ${d.relations ?? 0}。`
    failedOpen.value = false
    reconciling.value = false
    await reload()
    setNote(doneText, 'ok')
  } catch (e) {
    if (epoch === graphEpoch) setNote(e instanceof Error ? e.message : '图谱修复失败。')
  } finally {
    if (epoch === graphEpoch) reconciling.value = false
  }
}

// 邻居展开（双击节点或详情卡按钮）：按 center 拉该实体的一跳邻域子图合并进当前视图。
// 走的是同一个 subgraph 端点，可见性过滤在服务端内联 —— 无权文档里的邻居照样回不来。
async function expandNeighbors(id: string) {
  if (expanding.value || !props.spaceId) return
  if (nodes.value.length >= MAX_CANVAS_NODES) {
    setNote(`画布已达 ${MAX_CANVAS_NODES} 个实体，刷新后可重新展开。`, 'info')
    return
  }
  expanding.value = true
  note.value = ''
  const epoch = graphEpoch
  try {
    const url = `/api/kb/graph/subgraph?space_id=${encodeURIComponent(props.spaceId)}&limit=${EXPAND_LIMIT}&center=${encodeURIComponent(id)}`
    const response = await fetch(url, { headers: headers() })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    const data = await response.json().catch(() => ({}))
    if (epoch !== graphEpoch) return
    const { added, addedEdges } = mergeSubgraph(data, id)
    if (!added && !addedEdges) setNote('该实体在当前可见文档内没有更多邻居。', 'info')
    else if (!added) setNote('邻居已在图中，已补充新关系。', 'info')
    wake(0.6)
  } catch {
    if (epoch === graphEpoch) setNote('邻居展开失败，请稍后重试。')
  } finally {
    if (epoch === graphEpoch) expanding.value = false
  }
}

function onDblClick(event: MouseEvent) {
  const point = toWorld(event)
  const node = hitNode(point.x, point.y)
  if (!node) return
  selectedId.value = node.id
  void expandNeighbors(node.id)
}

async function reload() {
  const epoch = ++graphEpoch
  building.value = false
  buildPercent.value = null
  buildMessage.value = ''
  note.value = ''
  failedOpen.value = false
  // 换空间/整图重载：所有在途标志位统一复位，不许有按钮永久卡死
  expanding.value = false
  reconciling.value = false
  resetting.value = false
  failedLoading.value = false
  pollFailures = 0
  window.clearTimeout(pollTimer)
  if (raf) { cancelAnimationFrame(raf); raf = 0 }
  nodes.value = []
  edges.value = []
  statEntities.value = null
  statRelations.value = null
  render()
  if (!props.spaceId) {
    unavailable.value = true
    return
  }
  await loadSubgraph(epoch)
  // 子图加载失败（unavailable）时不再拉统计/接入构建：状态自相矛盾的 UI 不出
  if (epoch === graphEpoch && !unavailable.value) await loadStats(epoch)
  if (epoch === graphEpoch && !unavailable.value) await resumeBuilding(epoch)
}

watch(() => props.spaceId, () => { void reload() })

onMounted(() => {
  resizeObserver = new ResizeObserver(resizeCanvas)
  if (wrapEl.value) resizeObserver.observe(wrapEl.value)
  // 力导冷却后切主题不重排：监听 data-theme 变化补一次 render
  themeObserver = new MutationObserver(() => render())
  themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] })
  resizeCanvas()
  void reload()
})

onBeforeUnmount(() => {
  graphEpoch++
  window.clearTimeout(pollTimer)
  window.clearTimeout(searchTimer)
  if (raf) cancelAnimationFrame(raf)
  resizeObserver?.disconnect()
  themeObserver?.disconnect()
})
</script>

<template>
  <div class="graph-panel">
    <div class="graph-head">
      <div>
        <h3>知识图谱</h3>
        <span>实体与关系来自当前空间文档的自动抽取；可拖拽、缩放、搜索定位，双击节点展开邻居。</span>
      </div>
      <div class="graph-tools">
        <label class="graph-search">
          <input v-model="search" type="search" placeholder="搜索实体" aria-label="搜索实体" />
        </label>
        <span v-if="search.trim()" class="graph-hits">{{ matchCount }} 个匹配</span>
        <!-- 全量口径统计（服务端 stats）；两个数任一缺失就整段不显示，不把「未知」伪装成 0 -->
        <span v-if="statEntities != null && statRelations != null" class="graph-stats">全量实体 {{ statEntities }} · 关系 {{ statRelations }}</span>
        <button
          class="secondary-btn" type="button"
          :disabled="building || !writable"
          :title="writable ? '从当前空间文档重新抽取实体与关系' : '只读空间不能构建图谱'"
          @click="build"
        >{{ building ? '构建中…' : '构建图谱' }}</button>
        <button
          v-if="writable" class="secondary-btn" type="button" :disabled="building"
          title="查看未入图的文本块（抽取失败或构建后新增）"
          @click="toggleFailed"
        >{{ failedOpen ? '收起失败块' : '失败块' }}</button>
        <button
          v-if="writable" class="secondary-btn" type="button" :disabled="building || resetting"
          title="清空本空间的图谱（实体与关系删除，文档不受影响）"
          @click="resetGraph"
        >{{ resetting ? '清空中…' : '清空图谱' }}</button>
        <button
          v-if="writable" class="secondary-btn" type="button" :disabled="building || reconciling"
          title="文档删改后修复图谱：先试运行统计，确认后清理孤儿块与悬空实体"
          @click="reconcileGraph"
        >{{ reconciling ? '修复中…' : '修复图谱' }}</button>
      </div>
    </div>
    <div v-if="building" class="graph-progress" role="status">
      <div
        class="graph-progress-bar" role="progressbar" aria-valuemin="0" aria-valuemax="100"
        :aria-valuenow="buildPercent ?? undefined" :aria-busy="buildPercent == null"
      ><i :style="{ width: `${buildPercent ?? 12}%` }"></i></div>
      <span>{{ buildMessage || '正在构建图谱' }}{{ buildPercent != null ? ` ${buildPercent.toFixed(0)}%` : '…' }}</span>
    </div>
    <div v-if="note" class="graph-note" :class="noteKind" role="status">{{ note }}</div>

    <div ref="wrapEl" class="graph-canvas-wrap">
      <canvas
        ref="canvasEl" role="img" aria-label="知识图谱画布"
        @pointerdown="onPointerDown" @pointermove="onPointerMove" @pointerup="onPointerUp" @pointercancel="onPointerCancel"
        @dblclick="onDblClick"
        @wheel.prevent="onWheel"
      ></canvas>
      <div v-if="loading" class="graph-state" role="status">
        <strong>正在读取图谱</strong><span>节点较多时需要几秒钟。</span>
      </div>
      <div v-else-if="!nodes.length" class="graph-state">
        <strong>{{ !spaceId ? '请先选择知识空间' : unavailable ? '知识图谱暂不可用' : '图谱尚未构建' }}</strong>
        <span>{{ !spaceId ? '在左侧选择一个知识空间后展示图谱。' : unavailable ? '服务端图谱接口尚未就绪，接口上线后刷新页面即可展示。' : '点击右上角「构建图谱」从当前空间文档抽取实体与关系。' }}</span>
        <button v-if="writable && !unavailable && spaceId" class="primary-btn" type="button" :disabled="building" @click="build">构建图谱</button>
      </div>
      <!-- 缩放/重置按钮：滚轮之外的键盘与触屏替代 -->
      <div v-if="nodes.length" class="graph-zoom">
        <button type="button" title="放大" aria-label="放大" @click="zoomBy(1.25)">+</button>
        <button type="button" title="缩小" aria-label="缩小" @click="zoomBy(0.8)">−</button>
        <button type="button" title="重置视角" aria-label="重置视角" @click="resetView">⌂</button>
      </div>
      <div v-if="nodes.length" class="graph-count" aria-hidden="true">画布实体 {{ nodes.length }} · 关系 {{ edges.length }}</div>
      <div v-if="legend.length" class="graph-legend" aria-label="实体类型图例">
        <span v-for="item in legend" :key="item.type"><i :style="{ background: item.color }"></i>{{ item.type }}</span>
        <span v-if="legendMore" class="graph-legend-more">+{{ legendMore }} 类</span>
      </div>
      <aside v-if="failedOpen" class="graph-failed" role="dialog" aria-label="未入图块清单">
        <header>
          <strong>未入图块 {{ failedTotal }}</strong>
          <button class="icon-btn" type="button" title="关闭清单" aria-label="关闭清单" @click="failedOpen = false">×</button>
        </header>
        <div v-if="failedLoading" class="graph-failed-state" role="status">读取中…</div>
        <div v-else-if="!failedItems.length" class="graph-failed-state">没有未入图的块：抽取全部成功（或尚未构建）。</div>
        <ul v-else>
          <li v-for="item in failedItems" :key="`${item.doc_id}-${item.chunk_id}`">
            <span class="graph-failed-kind" :data-kind="item.kind">{{ FAILED_KIND_LABEL[item.kind] ?? (item.kind || '未知') }}</span>
            <span class="graph-failed-id" :title="item.doc_id">{{ item.doc_id }} · 块 {{ item.chunk_id }}</span>
            <span v-if="item.error" class="graph-failed-err" :title="item.error">{{ item.error }}</span>
          </li>
        </ul>
        <footer v-if="failedTotal > FAILED_PAGE">
          <button
            class="secondary-btn" type="button" :disabled="failedOffset === 0 || failedLoading"
            @click="loadFailed(Math.max(0, failedOffset - FAILED_PAGE))"
          >上一页</button>
          <span>{{ failedOffset + 1 }}–{{ Math.min(failedTotal, failedOffset + FAILED_PAGE) }} / {{ failedTotal }}</span>
          <button
            class="secondary-btn" type="button" :disabled="failedOffset + FAILED_PAGE >= failedTotal || failedLoading"
            @click="loadFailed(failedOffset + FAILED_PAGE)"
          >下一页</button>
        </footer>
      </aside>
      <aside v-if="selectedNode" class="graph-detail" role="dialog" aria-label="实体详情" @keydown.esc="selectedId = ''; render()">
        <header>
          <strong :title="selectedNode.label">{{ selectedNode.label }}</strong>
          <button class="icon-btn" type="button" title="关闭详情" aria-label="关闭详情" @click="selectedId = ''; render()">×</button>
        </header>
        <dl>
          <div v-if="selectedNode.type"><dt>类型</dt><dd>{{ selectedNode.type }}</dd></div>
          <div><dt>权重</dt><dd>{{ selectedNode.weight }}</dd></div>
          <div><dt>关联数</dt><dd>{{ selectedDegree }}</dd></div>
        </dl>
        <div v-if="selectedNeighbors.length" class="graph-neighbors">
          <span v-for="neighbor in selectedNeighbors" :key="`${neighbor.relation}-${neighbor.label}`">
            {{ neighbor.relation }} · {{ neighbor.label }}
          </span>
          <span v-if="selectedDegree > selectedNeighbors.length" class="graph-neighbors-more">等 {{ selectedDegree }} 个</span>
        </div>
        <button
          class="secondary-btn graph-expand" type="button" :disabled="expanding"
          title="拉取该实体的下一跳邻居并入当前视图"
          @click="expandNeighbors(selectedNode.id)"
        >{{ expanding ? '展开中…' : '展开邻居' }}</button>
      </aside>
    </div>
  </div>
</template>

<style scoped>
.graph-panel { width: 100%; display: flex; flex-direction: column; }
.graph-head { display: flex; align-items: flex-end; gap: 16px; }
.graph-head h3 { color: var(--text-primary); font-size: 14px; }
/* 只圈头部描述文字：别命中工具区里的 .graph-stats/.graph-hits */
.graph-head > div > span { display: block; margin-top: 3px; color: var(--text-muted); font-size: 11.5px; }
.graph-tools { margin-left: auto; display: flex; align-items: center; gap: 8px; }
.graph-search input {
  width: min(200px, 26vw); height: 32px; padding: 0 10px; border: 1px solid var(--border);
  border-radius: 6px; outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px;
}
.graph-search input:focus { border-color: var(--primary); box-shadow: var(--ring); }
.graph-stats { color: var(--text-muted); font-size: 11px; white-space: nowrap; font-variant-numeric: tabular-nums; }
.graph-hits { color: var(--text-faint); font-size: 11px; white-space: nowrap; font-variant-numeric: tabular-nums; }
.secondary-btn, .primary-btn, .icon-btn {
  height: 32px; border: 1px solid var(--border); border-radius: 6px; cursor: pointer; font: inherit; font-size: 12px;
}
.secondary-btn { padding: 0 13px; background: var(--bg-card); color: var(--text-regular); white-space: nowrap; }
.secondary-btn:hover, .icon-btn:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-light); }
.primary-btn { padding: 0 13px; border-color: var(--primary); background: var(--primary); color: #fff; }
.primary-btn:hover { background: var(--primary-hover); }
.icon-btn { width: 24px; height: 24px; padding: 0; background: var(--bg-card); color: var(--text-regular); font-size: 14px; }
button:disabled { cursor: not-allowed; opacity: .55; }
.graph-progress { display: flex; align-items: center; gap: 10px; margin-top: 10px; }
.graph-progress-bar { flex: 1; height: 6px; overflow: hidden; border-radius: 999px; background: var(--bg-sunken); }
.graph-progress-bar i { display: block; height: 100%; border-radius: 999px; background: var(--primary); transition: width .4s ease; }
.graph-progress span { flex: none; color: var(--text-muted); font-size: 11px; font-variant-numeric: tabular-nums; }
.graph-note { margin-top: 8px; padding: 7px 10px; border-left: 3px solid var(--warning-text); background: var(--warning-bg); color: var(--warning-text); font-size: 11.5px; }
.graph-note.ok { border-left-color: var(--success-text); background: var(--success-bg); color: var(--success-text); }
.graph-note.info { border-left-color: var(--text-faint); background: var(--bg-main); color: var(--text-muted); }
.graph-canvas-wrap {
  position: relative; min-height: 430px; margin-top: 12px; overflow: hidden;
  border: 1px solid var(--border); border-radius: 6px; background: var(--bg-main);
}
.graph-canvas-wrap canvas { display: block; touch-action: none; }
.graph-state {
  position: absolute; inset: 0; display: flex; align-items: center; justify-content: center;
  flex-direction: column; gap: 8px; padding: 20px; color: var(--text-muted); text-align: center; font-size: 12px;
  background: var(--bg-main);
}
.graph-state strong { color: var(--text-primary); font-size: 14px; }
.graph-state span { max-width: 460px; line-height: 1.6; }
.graph-state .primary-btn { margin-top: 6px; }
.graph-zoom { position: absolute; left: 10px; top: 10px; display: flex; flex-direction: column; gap: 4px; }
.graph-zoom button {
  width: 26px; height: 26px; border: 1px solid var(--border); border-radius: 6px;
  background: var(--bg-card); color: var(--text-regular); cursor: pointer; font-size: 14px; line-height: 1;
}
.graph-zoom button:hover { border-color: var(--primary); color: var(--primary); }
.graph-count {
  position: absolute; left: 10px; bottom: 8px; color: var(--text-faint); font-size: 11px;
  font-variant-numeric: tabular-nums; pointer-events: none;
}
.graph-legend {
  position: absolute; right: 10px; bottom: 8px; display: flex; flex-direction: column; gap: 3px;
  max-width: 45%; padding: 6px 9px; border: 1px solid var(--border); border-radius: 6px;
  background: var(--bg-card); opacity: .92; pointer-events: none;
}
.graph-legend span {
  display: flex; align-items: center; gap: 6px; overflow: hidden; color: var(--text-muted);
  font-size: 10.5px; text-overflow: ellipsis; white-space: nowrap;
}
.graph-legend i { flex: none; width: 8px; height: 8px; border-radius: 50%; }
.graph-legend-more { color: var(--text-faint); }
.graph-expand { width: 100%; margin-top: 9px; }
.graph-failed {
  position: absolute; top: 10px; left: 10px; width: min(320px, calc(100% - 20px));
  max-height: calc(100% - 60px); display: flex; flex-direction: column;
  border: 1px solid var(--border); border-radius: 8px;
  background: var(--bg-card); box-shadow: var(--shadow-md);
}
.graph-failed header { display: flex; align-items: center; gap: 8px; padding: 10px 12px 6px; }
.graph-failed header strong { flex: 1; color: var(--text-primary); font-size: 13px; }
.graph-failed ul { margin: 0; padding: 0 12px; overflow: auto; list-style: none; }
.graph-failed li {
  display: flex; align-items: baseline; gap: 6px; padding: 4px 0;
  border-top: 1px solid var(--border); font-size: 11px;
}
.graph-failed li:first-child { border-top: 0; }
/* kind 白名单：failed 黄警示，pending 与其余未知 kind 一律中性灰 */
.graph-failed-kind {
  flex: none; padding: 1px 6px; border-radius: 999px;
  background: var(--bg-sunken); color: var(--text-muted); font-size: 10px;
}
.graph-failed-kind[data-kind="failed"] { background: var(--warning-bg); color: var(--warning-text); }
.graph-failed-id { color: var(--text-regular); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.graph-failed-err { color: var(--text-faint); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.graph-failed-state { padding: 12px; color: var(--text-muted); font-size: 11.5px; }
.graph-failed footer { display: flex; align-items: center; gap: 8px; padding: 8px 12px 10px; }
.graph-failed footer span { color: var(--text-faint); font-size: 10.5px; font-variant-numeric: tabular-nums; }
.graph-failed footer .secondary-btn { height: 26px; padding: 0 10px; font-size: 11px; }
.graph-detail {
  position: absolute; top: 10px; right: 10px; width: min(260px, calc(100% - 20px));
  padding: 10px 12px; border: 1px solid var(--border); border-radius: 8px;
  background: var(--bg-card); box-shadow: var(--shadow-md);
}
.graph-detail header { display: flex; align-items: center; gap: 8px; }
.graph-detail header strong {
  min-width: 0; flex: 1; overflow: hidden; color: var(--text-primary); font-size: 13px;
  text-overflow: ellipsis; white-space: nowrap;
}
.graph-detail dl { margin-top: 8px; display: flex; gap: 14px; }
.graph-detail dl div { display: flex; align-items: baseline; gap: 5px; }
.graph-detail dt { color: var(--text-faint); font-size: 10.5px; }
.graph-detail dd { color: var(--text-primary); font-size: 12px; font-weight: 650; font-variant-numeric: tabular-nums; }
.graph-neighbors { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 9px; max-height: 108px; overflow: auto; }
.graph-neighbors span {
  padding: 2px 7px; border: 1px solid var(--border); border-radius: 999px;
  background: var(--bg-main); color: var(--text-muted); font-size: 10px;
}
.graph-neighbors-more { color: var(--text-faint); }
@media (max-width: 820px) {
  .graph-head { align-items: stretch; flex-direction: column; gap: 10px; }
  .graph-tools { margin-left: 0; flex-wrap: wrap; }
  .graph-search { flex: 1; }
  .graph-search input { width: 100%; }
  .graph-canvas-wrap { min-height: 320px; }
}
</style>
