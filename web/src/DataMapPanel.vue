<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { FONT_FAMILY, authHeaders, authQuery, authTail, errMessage, errText } from './panel-utils'

/** 【数据地图】`GET /api/datamap/nodes` + `GET /api/datamap/edges` 的全屏抽屉。
 *  节点=表、边=表间关系：kind ∈ join（已注册关联）/lineage（血缘）/joinable/synonym/
 *  distribution_similar/co_occurs/correlated 共 7 值（决定颜色），
 *  status ∈ pending（推断边，虚线）/ accepted（人工确认，实线）/ rejected（不上画布，只留在左侧列表）。
 *  点节点/边看 evidence 与置信度卡；接受/拒绝走 `POST /api/datamap/edges/{id}/accept|reject`，
 *  按钮只对 admin 渲染 —— 与 SkillsPanel 同一纪律：前端显隐只是体验，后端鉴权仍是唯一判据。
 *  路径查询 `GET /api/datamap/paths?from=&to=` 命中后在画布高亮路径上的表与边。
 *  力学模拟（斥力/弹簧/重力 + alpha 冷却、节点拖拽/滚轮缩放/空白平移）与 KbGraph.vue 同一思路。
 *  字段做宽容归一：节点 id/name/table（列节点另有 column）、边端点 left_table/right_table
 *  （注意边的 source 是来源标识 'inferred'/'registry'，不是端点！）、status/state 都可，
 *  接口未上线/空体按内联提示处理。
 *  Esc/遮罩关闭；401 交回父组件走会话过期。弹窗模式与 UsagePanel.vue 同款。 */
type EdgeStatus = 'pending' | 'accepted' | 'rejected'
interface MapNode {
  id: string; label: string; kind: string; domain: string; comment: string
  degree: number
  x: number; y: number; vx: number; vy: number; r: number; color: string
}
interface MapEdge {
  id: string; source: number; target: number
  kind: string; status: EdgeStatus
  confidence: number | null; evidence: string
}

const props = defineProps<{ token?: string; login?: string; admin?: boolean }>()
const emit = defineEmits<{
  (e: 'close'): void
  (e: 'auth-expired'): void
}>()

// 边颜色按 kind（未知 kind 落灰）；节点颜色按 kind 哈希进调色板。色相已拉开（合同关联红/可关联蓝/血缘紫/同义表粉）。
const KIND_COLORS: Record<string, string> = {
  join: '#e1655b', lineage: '#7a5af5', joinable: '#4a90d9', synonym: '#d45f9e', distribution_similar: '#3bb273', co_occurs: '#f0a63c', correlated: '#38b6c9',
}
const KIND_LABELS: Record<string, string> = {
  join: '已注册关联', lineage: '血缘', joinable: '可关联', synonym: '同义表', distribution_similar: '分布相似', co_occurs: '共现', correlated: '相关',
}
const STATUS_LABELS: Record<EdgeStatus, string> = { pending: '待确认', accepted: '已接受', rejected: '已拒绝' }
/** 节点 kind（table/column）的中文映射：边有 KIND_LABELS，节点也别裸显英文。 */
const NODE_KIND_LABELS: Record<string, string> = { table: '表', column: '列' }
const NODE_PALETTE = ['#4a90d9', '#f0a63c', '#9b6de8', '#3bb273', '#38b6c9', '#e87ab0', '#e1655b']
/** 空 kind 节点的固定色（按 index 取色会让同批空 kind 节点颜色不一、随加载顺序漂移）。 */
const NODE_FALLBACK_COLOR = '#8b93ad'
/** 图例只列当前数据里出现过的 kind（没出现的 kind 不占图例）。 */
const legendKinds = computed(() => Object.keys(KIND_COLORS).filter((k) => edges.value.some((e) => e.kind === k)))

const wrapEl = ref<HTMLDivElement>()
const canvasEl = ref<HTMLCanvasElement>()
const closeBtn = ref<HTMLButtonElement | null>(null)
const loading = ref(true)
const error = ref('')
const note = ref('')
const nodes = ref<MapNode[]>([])
const edges = ref<MapEdge[]>([])
const tab = ref<'edges' | 'nodes'>('edges')
const search = ref('')
const hoverNodeId = ref('')
const selectedNodeId = ref('')
const selectedEdgeId = ref('')
/** 行级写操作（接受/拒绝）互斥锁：一次只跑一条边，避免连点。 */
const actionBusy = ref('')
const pathFrom = ref('')
const pathTo = ref('')
const pathLoading = ref(false)
const pathMsg = ref('')
/** 路径高亮：非 null 时画布只留路径上的节点/边，其余压暗。 */
const pathNodes = ref<Set<string> | null>(null)
const pathPairs = ref<Set<string> | null>(null)

let raf = 0
let alpha = 0
let resizeObserver: ResizeObserver | null = null
let themeObserver: MutationObserver | null = null
let aborter: AbortController | null = null
let alive = true
let noteTimer = 0
const view = { scale: 1, ox: 0, oy: 0 }
// sx/sy 是平移锚点（随 move 更新）；ix/iy 是按下原点（判 moved 阈值）；gx/gy 是抓取偏移
const drag = { mode: '' as '' | 'node' | 'pan', id: '', sx: 0, sy: 0, ix: 0, iy: 0, gx: 0, gy: 0, moved: false }

function kindColor(kind: string): string {
  return KIND_COLORS[kind] ?? '#8b93ad'
}
function kindLabel(kind: string): string {
  return KIND_LABELS[kind] ?? kind
}
function statusLabel(status: EdgeStatus): string {
  return STATUS_LABELS[status] ?? status
}
function nodeKindLabel(kind: string): string {
  return NODE_KIND_LABELS[kind] ?? kind
}
function nodeColor(kind: string, index: number): string {
  if (!kind) return NODE_FALLBACK_COLOR
  let hash = 0
  for (let i = 0; i < kind.length; i++) hash = (hash * 31 + kind.charCodeAt(i)) | 0
  return NODE_PALETTE[Math.abs(hash) % NODE_PALETTE.length]
}
/** 标签截断加省略号：截过的名字不该被当成全名。 */
function clipText(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n)}…` : s
}

function normStatus(raw: unknown): EdgeStatus {
  const s = String(raw ?? '').toLowerCase()
  // active 是后端 registry（合同）边的状态：等价已接受，不是「待确认」
  if (s === 'accepted' || s === 'approved' || s === 'confirmed' || s === 'active') return 'accepted'
  if (s === 'rejected' || s === 'denied') return 'rejected'
  return 'pending'
}
function normConfidence(raw: unknown): number | null {
  let v = Number(raw ?? NaN)
  if (!Number.isFinite(v)) return null
  if (v > 1 && v <= 100) v /= 100 // 兼容 0-100 的百分口径
  return Math.min(1, Math.max(0, v))
}
function evidenceText(raw: unknown): string {
  if (raw == null) return ''
  if (typeof raw === 'string') return raw
  if (Array.isArray(raw)) return raw.map((v) => evidenceText(v)).filter(Boolean).join('；')
  if (typeof raw === 'object') {
    return Object.entries(raw as Record<string, unknown>)
      .map(([k, v]) => `${k}: ${typeof v === 'string' ? v : JSON.stringify(v)}`)
      .join('；')
  }
  return String(raw)
}

function makeNode(id: string, label: string, kind: string, domain: string, comment: string, index: number, total: number): MapNode {
  const angle = (index / Math.max(1, total)) * Math.PI * 2
  return {
    id, label, kind, domain, comment, degree: 0,
    x: Math.cos(angle) * 190 + (Math.random() - 0.5) * 40,
    y: Math.sin(angle) * 190 + (Math.random() - 0.5) * 40,
    // 半径初值只是占位：finishGraph 按度数重算（9 + sqrt(degree) * 3.4，上限 24）
    vx: 0, vy: 0, r: 10,
    color: nodeColor(kind, index),
  }
}

/** 节点契约 {id|name|table（+column）, kind?, domain?, comment?}；
 *  边契约 {id, left_table|right_table（端点）, kind, status, confidence, evidence}。
 *  边端点不在节点清单里时补占位节点 —— 边不该因为节点接口缺行而凭空消失。
 *  索引同时按 id 与 label（裸表名）建：后端节点 id 是 `table:t_a`、边端点是裸表名。 */
function normalizeGraph(nodesRaw: unknown, edgesRaw: unknown): { nodes: MapNode[]; edges: MapEdge[] } {
  const nodeList = Array.isArray(nodesRaw) ? nodesRaw : []
  const outNodes: MapNode[] = []
  const indexById = new Map<string, number>()
  nodeList.forEach((item, index) => {
    if (!item || typeof item !== 'object') return
    const row = item as Record<string, unknown>
    const id = String(row.id ?? row.name ?? row.table ?? row.table_name ?? index)
    const table = String(row.table ?? row.table_name ?? '')
    const column = String(row.column ?? '')
    // 列节点（id 形如 `column:t.c`）的 label 必须带列名，否则同表列节点无法区分
    const fallbackLabel = column ? (table ? `${table}.${column}` : column) : (table || id)
    const label = String(row.label ?? row.name ?? fallbackLabel)
    indexById.set(id, outNodes.length)
    if (label && !indexById.has(label)) indexById.set(label, outNodes.length)
    outNodes.push(makeNode(
      id,
      label,
      String(row.kind ?? row.type ?? ''),
      String(row.domain ?? ''),
      String(row.comment ?? row.description ?? ''),
      index, nodeList.length,
    ))
  })
  const ensure = (id: string): number => {
    const hit = indexById.get(id)
    if (hit != null) return hit
    const index = outNodes.length
    indexById.set(id, index)
    outNodes.push(makeNode(id, id, '', '', '', index, index + 1))
    return index
  }
  const edgeList = Array.isArray(edgesRaw) ? edgesRaw : []
  const outEdges: MapEdge[] = []
  edgeList.forEach((item, index) => {
    if (!item || typeof item !== 'object') return
    const row = item as Record<string, unknown>
    // 端点归一：left_table/right_table 是后端真实端点键，必须在 source 之前
    // （source 在这条 wire 上是来源标识 'inferred'/'registry'，当端点用会把每条边都丢掉）
    const src = String(row.left_table ?? row.source ?? row.from ?? row.source_table ?? row.src ?? '')
    const dst = String(row.right_table ?? row.target ?? row.to ?? row.target_table ?? row.dst ?? '')
    if (!src || !dst || src === dst) return
    outEdges.push({
      id: String(row.id ?? row.edge_id ?? `e${index}`),
      source: ensure(src),
      target: ensure(dst),
      kind: String(row.kind ?? row.type ?? row.relation ?? 'co_occurs'),
      status: normStatus(row.status ?? row.state ?? (row.accepted === true ? 'accepted' : 'pending')),
      confidence: normConfidence(row.confidence ?? row.score),
      evidence: evidenceText(row.evidence),
    })
  })
  return { nodes: outNodes, edges: outEdges }
}

/** 度数与半径在数据变化后重算（接受/拒绝会撤边）；rejected 不计入度数也不上画布。
 *  max(0,…)：0 度节点半径小于 1 度节点，度数信息不丢档。 */
function finishGraph() {
  for (const n of nodes.value) n.degree = 0
  for (const e of edges.value) {
    if (e.status === 'rejected') continue
    nodes.value[e.source].degree++
    nodes.value[e.target].degree++
  }
  for (const n of nodes.value) n.r = Math.min(24, 9 + Math.sqrt(Math.max(0, n.degree)) * 3.4)
}

const canvasEdges = computed(() => edges.value.filter((e) => e.status !== 'rejected'))

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

const REPULSION = 2600
const SPRING = 0.02
const GRAVITY = 0.012
function tick() {
  raf = 0
  const ns = nodes.value
  const es = canvasEdges.value
  if (!ns.length) return
  for (let i = 0; i < ns.length; i++) {
    const a = ns[i]
    for (let j = i + 1; j < ns.length; j++) {
      const b = ns[j]
      let dx = a.x - b.x
      let dy = a.y - b.y
      let dist2 = dx * dx + dy * dy
      if (dist2 > 160000) continue
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
    node.vx *= 0.86
    node.vy *= 0.86
    node.x += node.vx
    node.y += node.vy
  }
  alpha *= 0.995
  render()
  if (alpha > 0.015 || drag.mode === 'node') raf = requestAnimationFrame(tick)
}

function toWorld(event: PointerEvent | MouseEvent | WheelEvent): { x: number; y: number } {
  const rect = canvasEl.value?.getBoundingClientRect()
  const cx = (rect ? event.clientX - rect.left : 0)
  const cy = (rect ? event.clientY - rect.top : 0)
  const { w, h } = canvasSize()
  return {
    x: (cx - w / 2 - view.ox) / view.scale,
    y: (cy - h / 2 - view.oy) / view.scale,
  }
}

function hitNode(wx: number, wy: number): MapNode | null {
  const ns = nodes.value
  for (let i = ns.length - 1; i >= 0; i--) {
    const node = ns[i]
    if (Math.hypot(node.x - wx, node.y - wy) <= node.r + 3) return node
  }
  return null
}

function distToSegment(px: number, py: number, x1: number, y1: number, x2: number, y2: number): number {
  const dx = x2 - x1
  const dy = y2 - y1
  const len2 = dx * dx + dy * dy
  let t = len2 ? ((px - x1) * dx + (py - y1) * dy) / len2 : 0
  t = Math.max(0, Math.min(1, t))
  return Math.hypot(px - (x1 + t * dx), py - (y1 + t * dy))
}

function hitEdge(wx: number, wy: number): MapEdge | null {
  let best: MapEdge | null = null
  let bestDist = 6 / view.scale // 判定宽度按屏幕像素恒定，缩放后不变得难戳
  for (const e of canvasEdges.value) {
    const a = nodes.value[e.source]
    const b = nodes.value[e.target]
    const d = distToSegment(wx, wy, a.x, a.y, b.x, b.y)
    if (d < bestDist) { bestDist = d; best = e }
  }
  return best
}

function pairKey(a: string, b: string): string {
  return [a, b].sort().join('|')
}
function edgePairKey(e: MapEdge): string {
  return pairKey(nodes.value[e.source]?.id ?? '', nodes.value[e.target]?.id ?? '')
}

// 焦点邻接 Set 预算（computed 缓存）：hover 时 render 每帧逐节点查表 O(1)，不再逐节点 canvasEdges.some
const focusNeighbors = computed(() => {
  const focus = hoverNodeId.value || selectedNodeId.value
  if (!focus) return null
  const set = new Set<string>([focus])
  for (const e of canvasEdges.value) {
    const s = nodes.value[e.source]?.id
    const t = nodes.value[e.target]?.id
    if (s === focus && t) set.add(t)
    if (t === focus && s) set.add(s)
  }
  return set
})

function nodeDimmed(node: MapNode): boolean {
  if (pathNodes.value) return !pathNodes.value.has(node.id)
  const adj = focusNeighbors.value
  if (!adj) return false
  return !adj.has(node.id)
}

function edgeDimmed(e: MapEdge): boolean {
  if (pathPairs.value) return !pathPairs.value.has(edgePairKey(e))
  const focus = hoverNodeId.value || selectedNodeId.value
  if (focus) {
    const sourceId = nodes.value[e.source]?.id
    const targetId = nodes.value[e.target]?.id
    return sourceId !== focus && targetId !== focus
  }
  return !!selectedEdgeId.value && e.id !== selectedEdgeId.value
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
  const dark = document.documentElement.dataset.theme === 'dark'
  const labelColor = dark ? 'rgba(232,235,246,.88)' : 'rgba(16,22,43,.78)'
  const faintColor = dark ? 'rgba(232,235,246,.18)' : 'rgba(16,22,43,.12)'
  const es = canvasEdges.value
  const drawLabels = ns.length <= 260
  for (const edge of es) {
    const a = ns[edge.source]
    const b = ns[edge.target]
    const dim = edgeDimmed(edge)
    const hot = !dim && (pathPairs.value != null || edge.id === selectedEdgeId.value)
    ctx.strokeStyle = dim ? faintColor : kindColor(edge.kind)
    ctx.globalAlpha = dim ? 0.5 : edge.status === 'pending' ? 0.75 : 1
    ctx.lineWidth = hot ? 2.6 : 1.2
    ctx.setLineDash(edge.status === 'pending' ? [5, 4] : [])
    ctx.beginPath()
    ctx.moveTo(a.x, a.y)
    ctx.lineTo(b.x, b.y)
    ctx.stroke()
    if (drawLabels && !dim && es.length <= 80) {
      ctx.setLineDash([])
      ctx.fillStyle = dark ? 'rgba(139,147,173,.9)' : 'rgba(100,109,135,.85)'
      ctx.font = `9px ${FONT_FAMILY}`
      ctx.textAlign = 'center'
      ctx.fillText(clipText(kindLabel(edge.kind), 10), (a.x + b.x) / 2, (a.y + b.y) / 2 - 4)
    }
  }
  ctx.setLineDash([])
  ctx.globalAlpha = 1
  for (const node of ns) {
    const dim = nodeDimmed(node)
    ctx.globalAlpha = dim ? 0.22 : 1
    ctx.beginPath()
    ctx.arc(node.x, node.y, node.r, 0, Math.PI * 2)
    ctx.fillStyle = node.color
    ctx.fill()
    if (pathNodes.value?.has(node.id)) {
      ctx.lineWidth = 2
      ctx.strokeStyle = '#f0a63c'
      ctx.beginPath()
      ctx.arc(node.x, node.y, node.r + 3, 0, Math.PI * 2)
      ctx.stroke()
    }
    if (node.id === hoverNodeId.value || node.id === selectedNodeId.value) {
      ctx.lineWidth = 2.5
      ctx.strokeStyle = dark ? '#e8ebf6' : '#10162b'
      ctx.beginPath()
      ctx.arc(node.x, node.y, node.r, 0, Math.PI * 2)
      ctx.stroke()
    }
    if (drawLabels) {
      ctx.fillStyle = dim ? faintColor : labelColor
      ctx.font = `${node.r > 14 ? 11 : 10}px ${FONT_FAMILY}`
      ctx.textAlign = 'center'
      ctx.fillText(clipText(node.label, 12), node.x, node.y + node.r + 11)
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
      // 4px 位移阈值：点击手抖不变成拖拽/反选
      if (!drag.moved && Math.hypot(event.clientX - drag.ix, event.clientY - drag.iy) >= 4) drag.moved = true
      wake(0.2)
    }
    return
  }
  if (drag.mode === 'pan') {
    view.ox += event.clientX - drag.sx
    view.oy += event.clientY - drag.sy
    drag.sx = event.clientX
    drag.sy = event.clientY
    if (!drag.moved && Math.hypot(event.clientX - drag.ix, event.clientY - drag.iy) >= 4) drag.moved = true
    render()
    return
  }
  const node = hitNode(point.x, point.y)
  const next = node?.id ?? ''
  if (next !== hoverNodeId.value) {
    hoverNodeId.value = next
    if (canvasEl.value) canvasEl.value.style.cursor = node ? 'pointer' : 'default'
    render()
  }
}

function endDrag(event: PointerEvent) {
  drag.mode = ''
  drag.id = ''
  // 未持捕获时 release 会抛 DOMException
  const canvas = canvasEl.value
  if (canvas && canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId)
}

function onPointerUp(event: PointerEvent) {
  if (!drag.moved) {
    if (drag.mode === 'node') {
      selectedNodeId.value = selectedNodeId.value === drag.id ? '' : drag.id
      if (selectedNodeId.value) selectedEdgeId.value = ''
    } else {
      const point = toWorld(event)
      const edge = hitEdge(point.x, point.y)
      selectedEdgeId.value = edge?.id ?? ''
      if (edge) selectedNodeId.value = ''
    }
    render()
  }
  endDrag(event)
}

function onPointerCancel(event: PointerEvent) {
  // 触摸被打断：drag 状态必须收尾，否则拖曳态滞留、RAF 因 drag.mode==='node' 永不停
  endDrag(event)
}

function onPointerLeave() {
  // 指针离画布：hover 高亮与 cursor 一并复位，不滞留
  if (hoverNodeId.value) {
    hoverNodeId.value = ''
    render()
  }
  if (canvasEl.value) canvasEl.value.style.cursor = 'default'
}

function onWheel(event: WheelEvent) {
  // deltaMode 归一：Firefox 行滚动（=1）换算成像素级，缩放不暴涨
  const dy = event.deltaMode === 1 ? event.deltaY * 33 : event.deltaY
  const before = toWorld(event)
  view.scale = Math.min(3, Math.max(0.25, view.scale * (dy < 0 ? 1.12 : 0.89)))
  const { w, h } = canvasSize()
  const rect = canvasEl.value?.getBoundingClientRect()
  const cx = rect ? event.clientX - rect.left : w / 2
  const cy = rect ? event.clientY - rect.top : h / 2
  view.ox = cx - w / 2 - before.x * view.scale
  view.oy = cy - h / 2 - before.y * view.scale
  render()
}

/** 复位视图（缩放/平移只有重开面板才复位是不够的）。 */
function resetView() {
  view.scale = 1
  view.ox = 0
  view.oy = 0
  render()
}

function selectNode(id: string) {
  selectedNodeId.value = selectedNodeId.value === id ? '' : id
  if (selectedNodeId.value) selectedEdgeId.value = ''
  render()
}
function selectEdge(id: string) {
  selectedEdgeId.value = selectedEdgeId.value === id ? '' : id
  if (selectedEdgeId.value) selectedNodeId.value = ''
  render()
}

const selectedNode = computed(() => nodes.value.find((n) => n.id === selectedNodeId.value) ?? null)
const selectedEdge = computed(() => edges.value.find((e) => e.id === selectedEdgeId.value) ?? null)
const selectedEdgeNodes = computed(() => {
  const e = selectedEdge.value
  if (!e) return null
  // 索引异常（数据漂移）时返回 null，详情卡整体不渲染，不运行时报错
  const a = nodes.value[e.source]
  const b = nodes.value[e.target]
  if (!a || !b) return null
  return { a, b }
})
const selectedNodeEdgesAll = computed(() => {
  const node = selectedNode.value
  if (!node) return []
  return canvasEdges.value
    .filter((e) => nodes.value[e.source]?.id === node.id || nodes.value[e.target]?.id === node.id)
})
const NODE_EDGES_MAX = 8
const selectedNodeEdges = computed(() => selectedNodeEdgesAll.value.slice(0, NODE_EDGES_MAX))

function edgeLabel(e: MapEdge): string {
  return `${nodes.value[e.source]?.label ?? '?'} ↔ ${nodes.value[e.target]?.label ?? '?'}`
}
function otherLabel(e: MapEdge): string {
  const node = selectedNode.value
  if (!node) return edgeLabel(e)
  return nodes.value[e.source]?.id === node.id
    ? nodes.value[e.target]?.label ?? '?'
    : nodes.value[e.source]?.label ?? '?'
}

const needle = computed(() => search.value.trim().toLowerCase())
const nodeRows = computed(() => {
  const list = nodes.value.filter((n) =>
    !needle.value || n.label.toLowerCase().includes(needle.value) || n.id.toLowerCase().includes(needle.value))
  return [...list].sort((a, b) => b.degree - a.degree || a.label.localeCompare(b.label))
})
const edgeRows = computed(() => {
  const list = edges.value.filter((e) => {
    if (!needle.value) return true
    const a = nodes.value[e.source]
    const b = nodes.value[e.target]
    // 原始 kind 与中文标签都参与匹配（输「共现」也能搜到 co_occurs）
    return !!(a?.label.toLowerCase().includes(needle.value) || b?.label.toLowerCase().includes(needle.value)
      || e.kind.toLowerCase().includes(needle.value) || kindLabel(e.kind).toLowerCase().includes(needle.value))
  })
  // 待确认的推断边排最前（这是 admin 的工作队列），同状态按置信度降序
  const rank = (s: EdgeStatus) => (s === 'pending' ? 0 : s === 'accepted' ? 1 : 2)
  return [...list].sort((x, y) => rank(x.status) - rank(y.status) || (y.confidence ?? 0) - (x.confidence ?? 0))
})
const pendingCount = computed(() => edges.value.filter((e) => e.status === 'pending').length)
/** 路径候选表名去重（列节点 label 与表名重复时，候选列表不大量重复）。 */
const tableOptions = computed(() => [...new Set(nodes.value.map((n) => n.label))])

/** 边的 id 是合成串（`e${index}`，registry 边 id 为 null 时）时不能走接受/拒绝接口 —— POST 必 404。 */
function edgeOperable(e: MapEdge): boolean {
  return /^\d+$/.test(e.id)
}

/** 成功提示自动消隐：不与后续的路径结果/错误长期并存。 */
function flashNote(text: string) {
  note.value = text
  window.clearTimeout(noteTimer)
  noteTimer = window.setTimeout(() => { note.value = '' }, 4000)
}

async function decide(edge: MapEdge, action: 'accept' | 'reject') {
  if (!props.admin || actionBusy.value || !edgeOperable(edge)) return
  actionBusy.value = edge.id
  error.value = ''
  note.value = ''
  try {
    const r = await fetch(`/api/datamap/edges/${encodeURIComponent(edge.id)}/${action}${authQuery(props.token, props.login)}`, {
      method: 'POST', headers: authHeaders(props.token),
    })
    if (r.status === 401) {
      emit('auth-expired')
      error.value = '登录已失效，请重新登录'
      return
    }
    if (!r.ok) {
      error.value = await errText(r, action === 'accept' ? '接受失败' : '拒绝失败')
      return
    }
    if (!alive) return
    // 后端回最终状态就采信，没回就按动作落态；rejected 边随即从画布撤下
    const j: { status?: unknown } = await r.json().catch(() => ({}))
    edge.status = normStatus(j.status ?? (action === 'accept' ? 'accepted' : 'rejected'))
    finishGraph()
    flashNote(`${action === 'accept' ? '已接受' : '已拒绝'} ${edgeLabel(edge)}`)
    wake(0.3)
    render()
  } catch (e) {
    if (alive) error.value = `${action === 'accept' ? '接受' : '拒绝'}失败（网络）：${errMessage(e)}`
  } finally {
    actionBusy.value = ''
  }
}

/** 路径响应宽容归一：{nodes: [裸表名,…]} 优先；{paths: [[ref,…],…]} / {path: [hop,…]} 也接；
 *  ref 可以是字符串（表名/id）或带 id|name|table|left_table 的对象；
 *  hop 对象数组（left_table/right_table 成对，后端 paths 端点契约）串成节点序列。 */
function extractPaths(j: unknown): string[][] {
  const root = (j && typeof j === 'object' ? j : {}) as Record<string, unknown>
  const raw = root.nodes ?? root.paths ?? root.path ?? []
  if (!Array.isArray(raw) || !raw.length) return []
  const asRef = (v: unknown): string => {
    if (typeof v === 'string') return v
    if (v && typeof v === 'object') {
      const o = v as Record<string, unknown>
      return String(o.id ?? o.name ?? o.table ?? o.table_name ?? o.label ?? o.left_table ?? '')
    }
    return ''
  }
  if (raw.every((v) => Array.isArray(v))) {
    return (raw as unknown[][]).map((p) => p.map(asRef).filter(Boolean)).filter((p) => p.length)
  }
  if (raw.every((v) => !!v && typeof v === 'object' && 'left_table' in (v as Record<string, unknown>))) {
    const hops = raw as Record<string, unknown>[]
    const seq: string[] = []
    const first = String(hops[0].left_table ?? '')
    if (first) seq.push(first)
    for (const h of hops) {
      const rt = String(h.right_table ?? '')
      if (rt) seq.push(rt)
    }
    return seq.length > 1 ? [seq] : []
  }
  const single = raw.map(asRef).filter(Boolean)
  return single.length ? [single] : []
}

function clearPath() {
  pathNodes.value = null
  pathPairs.value = null
  pathMsg.value = ''
}

async function runPath() {
  const from = pathFrom.value.trim()
  const to = pathTo.value.trim()
  if (!from || !to || pathLoading.value) return
  if (from === to) {
    pathMsg.value = '起点与终点相同，无需查询'
    return
  }
  pathLoading.value = true
  pathMsg.value = ''
  error.value = ''
  try {
    const r = await fetch(
      `/api/datamap/paths?from=${encodeURIComponent(from)}&to=${encodeURIComponent(to)}${authTail(props.token, props.login)}`,
      { headers: authHeaders(props.token) },
    )
    if (r.status === 401) {
      emit('auth-expired')
      error.value = '登录已失效，请重新登录'
      return
    }
    if (!r.ok) {
      pathMsg.value = await errText(r, '路径查询失败')
      return
    }
    const paths = extractPaths(await r.json().catch(() => ({})))
    if (!alive) return
    if (!paths.length) {
      clearPath()
      pathMsg.value = `${from} → ${to} 之间没有找到路径`
      render()
      return
    }
    const byName = new Map<string, string>()
    for (const n of nodes.value) {
      byName.set(n.id.toLowerCase(), n.id)
      byName.set(n.label.toLowerCase(), n.id)
    }
    const pn = new Set<string>()
    const pp = new Set<string>()
    for (const path of paths) {
      // 未解析的引用会断段：有缺口就断开，不把不相邻两点连成假路径高亮
      let prev = ''
      for (const refName of path) {
        const id = byName.get(refName.toLowerCase()) ?? ''
        if (!id) { prev = ''; continue }
        pn.add(id)
        if (prev) pp.add(pairKey(prev, id))
        prev = id
      }
    }
    if (!pn.size) {
      clearPath()
      pathMsg.value = '路径里的表不在当前地图中'
      render()
      return
    }
    pathNodes.value = pn
    pathPairs.value = pp
    pathMsg.value = `找到 ${paths.length} 条路径，已高亮 ${pn.size} 张表`
    // 清掉选中态，路径高亮才看得清
    selectedNodeId.value = ''
    selectedEdgeId.value = ''
    render()
  } catch (e) {
    if (alive) pathMsg.value = `路径查询失败（网络）：${errMessage(e)}`
  } finally {
    pathLoading.value = false
  }
}

/** 包裹键宽容归一：nodes/edges 之外，items/rows/records 也接（与 SqlAuditPanel 同口径）。 */
function bag(j: unknown, keys: string[]): unknown[] {
  if (Array.isArray(j)) return j
  const o = (j && typeof j === 'object' ? j : {}) as Record<string, unknown>
  for (const k of keys) if (Array.isArray(o[k])) return o[k] as unknown[]
  return []
}

async function load() {
  aborter?.abort()
  const ctl = new AbortController()
  aborter = ctl
  loading.value = true
  error.value = ''
  note.value = ''
  clearPath()
  selectedNodeId.value = ''
  selectedEdgeId.value = ''
  hoverNodeId.value = ''
  if (canvasEl.value) canvasEl.value.style.cursor = 'default'
  try {
    const [rn, re] = await Promise.all([
      fetch(`/api/datamap/nodes${authQuery(props.token, props.login)}`, { headers: authHeaders(props.token), signal: ctl.signal }),
      fetch(`/api/datamap/edges${authQuery(props.token, props.login)}`, { headers: authHeaders(props.token), signal: ctl.signal }),
    ])
    if (rn.status === 401 || re.status === 401) {
      emit('auth-expired')
      error.value = '登录已失效，请重新登录'
      return
    }
    if (!rn.ok) {
      error.value = await errText(rn, '数据地图节点加载失败')
      return
    }
    if (!re.ok) {
      error.value = await errText(re, '数据地图关系加载失败')
      return
    }
    const jn: unknown = await rn.json().catch(() => ({}))
    const je: unknown = await re.json().catch(() => ({}))
    const graph = normalizeGraph(bag(jn, ['nodes', 'items', 'rows', 'records']), bag(je, ['edges', 'items', 'rows', 'records']))
    nodes.value = graph.nodes
    edges.value = graph.edges
    finishGraph()
    view.scale = 1
    view.ox = 0
    view.oy = 0
    wake(1)
  } catch (e) {
    if (ctl.signal.aborted) return
    error.value = `数据地图加载失败（网络）：${errMessage(e)}`
  } finally {
    if (aborter === ctl) loading.value = false
  }
}

function onEsc(e: KeyboardEvent) {
  if (e.key === 'Escape') emit('close')
}
onMounted(() => {
  resizeObserver = new ResizeObserver(resizeCanvas)
  if (wrapEl.value) resizeObserver.observe(wrapEl.value)
  // 主题切换不重排力导：监听 data-theme 补一次 render，画布配色不留滞到下次交互
  themeObserver = new MutationObserver(() => render())
  themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] })
  resizeCanvas()
  void load()
  window.addEventListener('keydown', onEsc)
  closeBtn.value?.focus()
})
onBeforeUnmount(() => {
  alive = false
  aborter?.abort()
  window.clearTimeout(noteTimer)
  window.removeEventListener('keydown', onEsc)
  if (raf) cancelAnimationFrame(raf)
  resizeObserver?.disconnect()
  themeObserver?.disconnect()
})
</script>

<template>
  <div class="dm-mask" @click.self="emit('close')">
    <section class="dm-dialog" role="dialog" aria-modal="true" aria-labelledby="dm-title">
      <header class="dm-head">
        <div>
          <span class="dm-kicker">数据地图</span>
          <h2 id="dm-title">表关系图谱<span v-if="pendingCount" class="dm-pending-tag">{{ pendingCount }} 条待确认</span></h2>
          <p class="dm-sub">
            节点=表，边=表间关系（虚线=待确认推断边，实线=已接受）。点节点/边看证据与置信度{{ admin ? '，可接受或拒绝推断边' : '' }}；画布支持拖拽、滚轮缩放、空白拖动平移。
          </p>
        </div>
        <button ref="closeBtn" type="button" class="dm-close" title="关闭" aria-label="关闭" @click="emit('close')">×</button>
      </header>

      <!-- 路径查询：from/to 表名 → GET /api/datamap/paths，命中路径在画布高亮 -->
      <div class="dm-path">
        <input v-model="pathFrom" list="dm-tables" placeholder="起点表名" aria-label="起点表名" @keydown.enter="!$event.isComposing && runPath()" />
        <span class="dm-path-arrow">→</span>
        <input v-model="pathTo" list="dm-tables" placeholder="终点表名" aria-label="终点表名" @keydown.enter="!$event.isComposing && runPath()" />
        <datalist id="dm-tables">
          <option v-for="l in tableOptions" :key="l" :value="l" />
        </datalist>
        <button
          type="button" class="dm-btn primary"
          :disabled="pathLoading || !pathFrom.trim() || !pathTo.trim() || pathFrom.trim() === pathTo.trim()"
          :title="pathFrom.trim() && pathFrom.trim() === pathTo.trim() ? '起点与终点相同' : ''"
          @click="runPath"
        >
          {{ pathLoading ? '查询中…' : '查路径' }}
        </button>
        <button v-if="pathNodes" type="button" class="dm-btn" @click="clearPath(); render()">清除高亮</button>
        <span v-if="pathMsg" class="dm-path-msg" role="status">{{ pathMsg }}</span>
      </div>

      <div v-if="error" class="dm-error" role="alert">{{ error }}</div>
      <div v-else-if="note" class="dm-note" role="status">{{ note }}</div>

      <div class="dm-body">
        <!-- 左列表：关系（pending 在前，是 admin 的确认队列，计数含已拒绝）/ 表（按度数排序） -->
        <aside class="dm-side">
          <div class="dm-tabs">
            <button type="button" :class="{ on: tab === 'edges' }" @click="tab = 'edges'">关系 <b>{{ edges.length }}</b></button>
            <button type="button" :class="{ on: tab === 'nodes' }" @click="tab = 'nodes'">表 <b>{{ nodes.length }}</b></button>
          </div>
          <input v-model="search" class="dm-search" type="search" placeholder="过滤表名 / 关系类型" aria-label="过滤" />
          <div class="dm-list">
            <template v-if="tab === 'edges'">
              <div v-if="!edgeRows.length" class="dm-empty">{{ needle ? '无匹配结果' : '暂无关系' }}</div>
              <button
                v-for="e in edgeRows" :key="e.id" type="button"
                class="dm-row" :class="{ on: selectedEdgeId === e.id }"
                @click="selectEdge(e.id)"
              >
                <span class="dm-dot" :style="{ background: kindColor(e.kind) }"></span>
                <span class="dm-row-t" :title="edgeLabel(e)">{{ edgeLabel(e) }}</span>
                <span class="dm-pill" :data-s="e.status">{{ statusLabel(e.status) }}</span>
                <span v-if="e.confidence != null" class="dm-conf">{{ Math.round(e.confidence * 100) }}%</span>
              </button>
            </template>
            <template v-else>
              <div v-if="!nodeRows.length" class="dm-empty">{{ needle ? '无匹配结果' : '暂无表节点' }}</div>
              <button
                v-for="n in nodeRows" :key="n.id" type="button"
                class="dm-row" :class="{ on: selectedNodeId === n.id }"
                @click="selectNode(n.id)"
              >
                <span class="dm-dot" :style="{ background: n.color }"></span>
                <span class="dm-row-t" :title="n.comment || n.label">{{ n.label }}</span>
                <span class="dm-conf">{{ n.degree }} 条关系</span>
              </button>
            </template>
          </div>
        </aside>

        <!-- 右画布：力导向图 -->
        <div ref="wrapEl" class="dm-canvas-wrap">
          <canvas
            ref="canvasEl" aria-label="数据地图画布"
            @pointerdown="onPointerDown" @pointermove="onPointerMove" @pointerup="onPointerUp"
            @pointercancel="onPointerCancel" @pointerleave="onPointerLeave"
            @wheel.prevent="onWheel"
          ></canvas>
          <div v-if="loading" class="dm-state" role="status">
            <span class="dm-spin"></span>数据地图加载中…
          </div>
          <div v-else-if="!nodes.length && !error" class="dm-state">
            <strong>暂无表节点</strong>
            <span>数据地图接口尚未返回任何表；接口上线或推断任务跑完后会自动展示。</span>
          </div>
          <button v-if="nodes.length" type="button" class="dm-reset" title="复位缩放与平移" @click="resetView">复位视图</button>
          <!-- 画布口径计数：不含已拒绝边（左侧 tab 计数含已拒绝，两处口径不同是有意的） -->
          <div v-if="nodes.length" class="dm-count" aria-hidden="true">表 {{ nodes.length }} · 画布关系 {{ canvasEdges.length }}</div>
          <div v-if="nodes.length" class="dm-legend" aria-hidden="true">
            <span v-for="k in legendKinds" :key="k"><i class="dm-swatch" :style="{ background: KIND_COLORS[k] }"></i>{{ KIND_LABELS[k] }}</span>
            <span><i class="dm-line"></i>已接受</span>
            <span><i class="dm-line dash"></i>待确认</span>
          </div>

          <!-- 边详情卡：evidence + 置信度 + admin 接受/拒绝 -->
          <aside v-if="selectedEdge && selectedEdgeNodes" class="dm-detail" aria-label="关系详情">
            <header>
              <strong :title="edgeLabel(selectedEdge)">{{ selectedEdgeNodes.a.label }} ↔ {{ selectedEdgeNodes.b.label }}</strong>
              <button type="button" class="dm-x" title="关闭详情" aria-label="关闭详情" @click="selectEdge(selectedEdge.id)">×</button>
            </header>
            <dl>
              <div><dt>类型</dt><dd><span class="dm-dot" :style="{ background: kindColor(selectedEdge.kind) }"></span>{{ kindLabel(selectedEdge.kind) }}</dd></div>
              <div><dt>状态</dt><dd><span class="dm-pill" :data-s="selectedEdge.status">{{ statusLabel(selectedEdge.status) }}</span></dd></div>
              <div><dt>置信度</dt><dd>{{ selectedEdge.confidence != null ? `${Math.round(selectedEdge.confidence * 100)}%` : '—' }}</dd></div>
            </dl>
            <div v-if="selectedEdge.confidence != null" class="dm-conf-bar">
              <i :style="{ width: `${Math.round(selectedEdge.confidence * 100)}%`, background: kindColor(selectedEdge.kind) }"></i>
            </div>
            <div v-if="selectedEdge.evidence" class="dm-evidence">{{ selectedEdge.evidence }}</div>
            <div v-else class="dm-evidence dm-none">暂无证据说明</div>
            <!-- 合成 id（registry 边 id:null 落的 e${index}）没有可操作的后端记录，不渲染操作区 -->
            <div v-if="admin && edgeOperable(selectedEdge) && selectedEdge.status !== 'accepted'" class="dm-ops">
              <button type="button" class="dm-btn primary" :disabled="!!actionBusy" @click="decide(selectedEdge, 'accept')">
                {{ actionBusy === selectedEdge.id ? '提交中…' : '接受' }}
              </button>
              <button v-if="selectedEdge.status !== 'rejected'" type="button" class="dm-btn danger" :disabled="!!actionBusy" @click="decide(selectedEdge, 'reject')">
                {{ actionBusy === selectedEdge.id ? '提交中…' : '拒绝' }}
              </button>
            </div>
            <div v-else-if="admin && edgeOperable(selectedEdge) && selectedEdge.status === 'accepted'" class="dm-ops">
              <button type="button" class="dm-btn danger" :disabled="!!actionBusy" @click="decide(selectedEdge, 'reject')">
                {{ actionBusy === selectedEdge.id ? '提交中…' : '撤销接受' }}
              </button>
            </div>
          </aside>

          <!-- 节点详情卡 -->
          <aside v-else-if="selectedNode" class="dm-detail" aria-label="表详情">
            <header>
              <strong :title="selectedNode.label">{{ selectedNode.label }}</strong>
              <button type="button" class="dm-x" title="关闭详情" aria-label="关闭详情" @click="selectNode(selectedNode.id)">×</button>
            </header>
            <dl>
              <div v-if="selectedNode.kind"><dt>类型</dt><dd>{{ nodeKindLabel(selectedNode.kind) }}<template v-if="selectedNode.domain"> · {{ selectedNode.domain }}</template></dd></div>
              <div><dt>关系数</dt><dd>{{ selectedNode.degree }}</dd></div>
            </dl>
            <div v-if="selectedNode.comment" class="dm-evidence">{{ selectedNode.comment }}</div>
            <div v-if="selectedNodeEdges.length" class="dm-rel">
              <button
                v-for="e in selectedNodeEdges" :key="e.id" type="button"
                class="dm-rel-row" @click="selectEdge(e.id)"
              >
                <span class="dm-dot" :style="{ background: kindColor(e.kind) }"></span>
                <span class="dm-row-t" :title="otherLabel(e)">{{ otherLabel(e) }}</span>
                <span class="dm-pill" :data-s="e.status">{{ statusLabel(e.status) }}</span>
              </button>
            </div>
            <div v-if="selectedNodeEdgesAll.length > NODE_EDGES_MAX" class="dm-rel-more">还有 {{ selectedNodeEdgesAll.length - NODE_EDGES_MAX }} 条关系未列出</div>
          </aside>
        </div>
      </div>
    </section>
  </div>
</template>

<style>
.dm-mask { position: fixed; inset: 0; z-index: 1100; display: grid; place-items: center; padding: 16px; background: rgba(17, 24, 39, .38); backdrop-filter: blur(5px); }
.dm-dialog { width: min(1180px, 100%); height: min(780px, 100%); display: flex; flex-direction: column; border: 1px solid var(--border); border-radius: 8px; background: var(--bg-card); box-shadow: 0 24px 70px rgba(17, 24, 39, .2); overflow: hidden; }
.dm-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 20px; padding: 16px 20px 8px; }
.dm-kicker { display: block; margin-bottom: 4px; color: var(--primary); font-size: 11px; font-weight: 750; }
.dm-head h2 { margin: 0; color: var(--text-primary); font-size: 17px; font-weight: 700; }
.dm-pending-tag { margin-left: 8px; padding: 1px 8px; border-radius: 999px; background: var(--warning-bg); color: var(--warning-text); font-size: 11px; font-weight: 650; vertical-align: 2px; }
.dm-sub { margin: 5px 0 0; color: var(--text-muted); font-size: 11.5px; line-height: 1.6; }
.dm-close { width: 30px; height: 30px; flex-shrink: 0; border: 0; border-radius: 5px; background: transparent; color: var(--text-muted); cursor: pointer; }
.dm-close:hover { background: var(--bg-hover); color: var(--text-primary); }

.dm-path { display: flex; flex-wrap: wrap; align-items: center; gap: 7px; padding: 2px 20px 10px; }
.dm-path input { width: min(210px, 24vw); min-width: 120px; height: 30px; padding: 0 9px; border: 1px solid var(--border); border-radius: 6px; outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px; }
.dm-path input:focus { border-color: var(--primary); box-shadow: var(--ring); }
.dm-path-arrow { color: var(--text-faint); }
.dm-path-msg { color: var(--text-muted); font-size: 11.5px; }
.dm-btn { height: 30px; padding: 0 12px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card); color: var(--text-regular); font: inherit; font-size: 12px; cursor: pointer; white-space: nowrap; }
.dm-btn:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.dm-btn:disabled { opacity: .5; cursor: default; }
.dm-btn.primary { border-color: var(--primary); background: var(--primary); color: var(--on-primary); }
.dm-btn.primary:hover:not(:disabled) { color: var(--on-primary); opacity: .88; }
.dm-btn.danger:hover:not(:disabled) { border-color: var(--error-text); color: var(--error-text); }

.dm-error { margin: 0 20px 8px; padding: 7px 10px; border-left: 3px solid var(--error-text); background: var(--error-bg); color: var(--error-text); font-size: 12px; line-height: 1.6; }
.dm-note { margin: 0 20px 8px; padding: 7px 10px; border-left: 3px solid var(--success-text); background: var(--success-bg); color: var(--success-text); font-size: 12px; line-height: 1.6; }

.dm-body { flex: 1; min-height: 0; display: flex; gap: 12px; padding: 0 20px 16px; }
.dm-side { width: 272px; flex-shrink: 0; display: flex; flex-direction: column; min-height: 0; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-main); }
.dm-tabs { display: flex; gap: 6px; padding: 8px 8px 0; }
.dm-tabs button { flex: 1; height: 28px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card); color: var(--text-muted); font: inherit; font-size: 12px; cursor: pointer; }
.dm-tabs button b { font-variant-numeric: tabular-nums; }
.dm-tabs button.on { border-color: var(--primary); color: var(--primary); background: var(--primary-light); }
.dm-search { margin: 8px; height: 30px; padding: 0 9px; border: 1px solid var(--border); border-radius: 6px; outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px; }
.dm-search:focus { border-color: var(--primary); box-shadow: var(--ring); }
.dm-list { flex: 1; min-height: 0; overflow-y: auto; padding: 0 8px 8px; }
.dm-empty { padding: 18px 8px; color: var(--text-faint); font-size: 12px; text-align: center; }
.dm-row { display: flex; align-items: center; gap: 7px; width: 100%; padding: 6px 8px; border: 1px solid transparent; border-radius: 6px; background: transparent; color: var(--text-regular); font: inherit; font-size: 12px; cursor: pointer; text-align: left; }
.dm-row:hover { background: var(--bg-hover); }
.dm-row.on { border-color: var(--primary); background: var(--primary-light); }
.dm-dot { width: 8px; height: 8px; flex-shrink: 0; border-radius: 50%; }
.dm-row-t { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dm-pill { flex-shrink: 0; padding: 1px 7px; border-radius: 999px; font-size: 10.5px; background: var(--bg-sunken); color: var(--text-muted); }
.dm-pill[data-s='pending'] { background: var(--warning-bg); color: var(--warning-text); }
.dm-pill[data-s='accepted'] { background: var(--success-bg); color: var(--success-text); }
.dm-pill[data-s='rejected'] { background: var(--error-bg); color: var(--error-text); }
.dm-conf { flex-shrink: 0; color: var(--text-faint); font-size: 10.5px; font-variant-numeric: tabular-nums; }

.dm-canvas-wrap { position: relative; flex: 1; min-width: 0; min-height: 0; overflow: hidden; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-main); }
.dm-canvas-wrap canvas { display: block; touch-action: none; }
.dm-state { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; flex-direction: column; gap: 8px; padding: 20px; color: var(--text-muted); text-align: center; font-size: 12px; background: var(--bg-main); }
.dm-state strong { color: var(--text-primary); font-size: 14px; }
.dm-state span { max-width: 460px; line-height: 1.6; }
.dm-spin { width: 14px; height: 14px; border: 2px solid var(--primary); border-top-color: transparent; border-radius: 50%; animation: dmSpin .7s linear infinite; }
@keyframes dmSpin { to { transform: rotate(360deg); } }
@media (prefers-reduced-motion: reduce) { .dm-spin { animation: none; } }
.dm-reset { position: absolute; left: 10px; top: 10px; height: 26px; padding: 0 10px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card); color: var(--text-regular); font-size: 11px; cursor: pointer; }
.dm-reset:hover { border-color: var(--primary); color: var(--primary); }
.dm-count { position: absolute; left: 10px; bottom: 8px; color: var(--text-faint); font-size: 11px; font-variant-numeric: tabular-nums; pointer-events: none; }
.dm-legend { position: absolute; right: 10px; bottom: 8px; display: flex; align-items: center; gap: 10px; color: var(--text-faint); font-size: 10.5px; pointer-events: none; }
.dm-legend span { display: inline-flex; align-items: center; gap: 4px; }
.dm-swatch { width: 8px; height: 8px; border-radius: 50%; display: inline-block; }
.dm-line { width: 14px; border-top: 2px solid var(--text-faint); display: inline-block; }
.dm-line.dash { border-top-style: dashed; }

.dm-detail { position: absolute; top: 10px; right: 10px; width: min(300px, calc(100% - 20px)); max-height: calc(100% - 60px); overflow-y: auto; padding: 10px 12px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg-card); box-shadow: var(--shadow-md); }
.dm-detail header { display: flex; align-items: center; gap: 8px; }
.dm-detail header strong { min-width: 0; flex: 1; overflow: hidden; color: var(--text-primary); font-size: 12.5px; text-overflow: ellipsis; white-space: nowrap; }
.dm-x { width: 22px; height: 22px; flex-shrink: 0; border: 0; border-radius: 5px; background: transparent; color: var(--text-muted); font-size: 14px; cursor: pointer; }
.dm-x:hover { background: var(--bg-hover); color: var(--text-primary); }
.dm-detail dl { margin-top: 8px; display: flex; gap: 14px; flex-wrap: wrap; }
.dm-detail dl div { display: flex; align-items: center; gap: 5px; }
.dm-detail dt { color: var(--text-faint); font-size: 10.5px; }
.dm-detail dd { display: inline-flex; align-items: center; gap: 4px; color: var(--text-primary); font-size: 12px; font-weight: 650; font-variant-numeric: tabular-nums; }
.dm-conf-bar { margin-top: 7px; height: 5px; border-radius: 999px; background: var(--bg-sunken); overflow: hidden; }
.dm-conf-bar i { display: block; height: 100%; border-radius: 999px; }
.dm-evidence { margin-top: 8px; padding: 7px 9px; border-radius: 6px; background: var(--bg-main); color: var(--text-muted); font-size: 11px; line-height: 1.7; white-space: pre-wrap; word-break: break-all; }
.dm-evidence.dm-none { color: var(--text-faint); }
.dm-ops { display: flex; gap: 8px; margin-top: 9px; }
.dm-rel { display: flex; flex-direction: column; gap: 3px; margin-top: 8px; }
.dm-rel-row { display: flex; align-items: center; gap: 6px; width: 100%; padding: 4px 6px; border: 0; border-radius: 5px; background: transparent; color: var(--text-regular); font: inherit; font-size: 11.5px; cursor: pointer; text-align: left; }
.dm-rel-row:hover { background: var(--bg-hover); }
.dm-rel-more { margin-top: 5px; color: var(--text-faint); font-size: 10.5px; }

@media (max-width: 860px) {
  .dm-body { flex-direction: column; }
  .dm-side { width: 100%; max-height: 220px; }
}
</style>
