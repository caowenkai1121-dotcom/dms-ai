<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'

// docId ＝文档叶子；chunkCount ＝章节节点（内容级，由 sections 端点懒加载嫁接，不是服务端树的固有层）
interface MNode {
  name: string; children: MNode[]
  docId?: string; chunkCount?: number; excerpt?: string; page?: number | null
}
interface LayoutNode {
  key: string; name: string; depth: number; x: number; y: number
  branch: number; hasChildren: boolean; collapsed: boolean; hiddenCount: number
  docCount: number; labelW: number
  docId: string | null; chunkCount: number; excerpt: string; page: number | null
}
interface LayoutEdge { x1: number; y1: number; x2: number; y2: number; branch: number }

const props = defineProps<{ token?: string; spaceId?: string; writable?: boolean }>()
const emit = defineEmits<{ (e: 'auth-expired'): void }>()

const PALETTE = ['#e0a43c', '#4a90d9', '#3bb273', '#9b6de8', '#e1655b', '#38b6c9', '#e87ab0', '#c9a53c']
const ROW_H = 36
const COL_GAP = 56

const root = ref<MNode | null>(null)
const loading = ref(false)
const unavailable = ref(false)
const regenerating = ref(false)
const note = ref('')
const collapsedKeys = ref<string[]>([])
const exporting = ref(false)
// 内容级展开状态（只活在本会话：不按空间记忆——章节数据是懒加载的，展开即拉取）
const expandedDocs = ref<string[]>([])
const docSections = ref<Record<string, MNode[]>>({})
const loadingDoc = ref('')
interface SectionCard { docId: string; docName: string; name: string; chunkCount: number; excerpt: string; page: number | null }
const activeSection = ref<SectionCard | null>(null)
let mindmapEpoch = 0

// 折叠状态记忆：按空间存 localStorage（键是名称路径，跨会话稳定；导图重生成后的失效键无害，
// 它们只会「折叠一个不存在的路径」，不落任何副作用）
const COLLAPSE_PREFIX = 'kb_mindmap_collapsed:'
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

function countDescendants(node: MNode): number {
  return node.children.reduce((sum, child) => sum + 1 + countDescendants(child), 0)
}

// 文档数徽标的口径：只数文档叶子（章节节点是内容级，不计入文档数）
function countDocs(node: MNode): number {
  if (node.docId) return 1
  return node.children.reduce((sum, child) => sum + countDocs(child), 0)
}

// 徽标宽度按位数分档（1 位/2 位/3+ 位）
function badgeWidth(count: number): number {
  return count < 10 ? 16 : count < 100 ? 22 : 30
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

  const nodes: LayoutNode[] = []
  const edges: LayoutEdge[] = []
  let leaf = 0
  // 叶节点自上而下各占一行，父节点取首末子节点中点；key 是从根出发的名称路径
  const visit = (node: MNode, depth: number, branch: number, path: string): number => {
    const key = path ? `${path}/${node.name}` : node.name
    const isCollapsed = collapsed.has(key) && node.children.length > 0
    const x = colX[depth] ?? depth * 200
    let y: number
    if (!node.children.length || isCollapsed) {
      y = 28 + leaf * ROW_H
      leaf++
    } else {
      const childYs = node.children.map((child, index) => visit(child, depth + 1, depth === 0 ? index : branch, key))
      y = (childYs[0] + childYs[childYs.length - 1]) / 2
    }
    nodes.push({
      key, name: node.name, depth, x, y, branch,
      hasChildren: node.children.length > 0,
      collapsed: isCollapsed,
      hiddenCount: isCollapsed ? countDescendants(node) : 0,
      docCount: node.docId || node.chunkCount ? 0 : countDocs(node),
      labelW: labelWidth(node.name),
      docId: node.docId ?? null,
      chunkCount: node.chunkCount ?? 0,
      excerpt: node.excerpt ?? '',
      page: node.page ?? null,
    })
    return y
  }
  visit(tree, 0, 0, '')
  // 父节点坐标在子节点之后才定，边在第二遍按 key 的父路径补
  const byKey = new Map(nodes.map((n) => [n.key, n]))
  for (const node of nodes) {
    if (node.depth === 0) continue
    const parent = byKey.get(node.key.slice(0, node.key.lastIndexOf('/')))
    if (parent) edges.push({ x1: parent.x, y1: parent.y, x2: node.x, y2: node.y, branch: node.branch })
  }
  const width = Math.max(400, (colX[colX.length - 1] ?? 0) + (colWidths[colWidths.length - 1] ?? 120) + 60)
  const height = Math.max(160, leaf * ROW_H + 48)
  return { nodes, edges, width, height }
})

function toggle(key: string) {
  collapsedKeys.value = collapsedKeys.value.includes(key)
    ? collapsedKeys.value.filter((k) => k !== key)
    : [...collapsedKeys.value, key]
  saveCollapsed()
}

// ==================== 内容级：文档 → 章节 ====================
// 章节数据走 `/api/kb/doc/{id}/sections`（契约见 kb_mindmap_api.rs ③，端点未注册时
// 优雅降级为「只到文档」并提示，不炸导图）。

function normalizeSections(input: unknown): MNode[] {
  const list = Array.isArray((input as Record<string, unknown>)?.sections)
    ? (input as Record<string, unknown>).sections as unknown[]
    : []
  const out: MNode[] = []
  for (const raw of list) {
    if (!raw || typeof raw !== 'object') continue
    const item = raw as Record<string, unknown>
    const name = String(item.section ?? item.name ?? '').trim()
    if (!name) continue
    out.push({
      name,
      children: [],
      chunkCount: Number(item.chunk_count) || 0,
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
    if (loadingDoc.value) return
    loadingDoc.value = docId
    note.value = ''
    try {
      const response = await fetch(`/api/kb/doc/${encodeURIComponent(docId)}/sections`, { headers: headers() })
      if (response.status === 401) emit('auth-expired')
      if (!response.ok) throw new Error(`HTTP ${response.status}`)
      const data = await response.json().catch(() => ({}))
      docSections.value = { ...docSections.value, [docId]: normalizeSections(data) }
    } catch {
      note.value = '章节展开接口尚未上线，当前导图只能展开到文档。'
      loadingDoc.value = ''
      return
    }
    loadingDoc.value = ''
  }
  if (!docSections.value[docId]?.length) {
    note.value = `《${node.name}》没有可展开的章节结构。`
    return
  }
  expandedDocs.value = [...expandedDocs.value, docId]
}

function openSection(node: LayoutNode, doc: LayoutNode | undefined) {
  activeSection.value = {
    docId: doc?.docId ?? '',
    docName: doc?.name ?? '',
    name: node.name,
    chunkCount: node.chunkCount,
    excerpt: node.excerpt,
    page: node.page,
  }
}

// 节点点击路由：章节→摘要卡；文档→展开/收起章节；分支/根→折叠记忆（原有行为）
function onNodeClick(node: LayoutNode) {
  if (node.chunkCount) {
    const parent = layout.value.nodes.find((n) => n.key === node.key.slice(0, node.key.lastIndexOf('/')))
    openSection(node, parent)
    return
  }
  if (node.docId) { void toggleDoc(node); return }
  if (node.hasChildren) toggle(node.key)
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
  } catch {
    if (epoch === mindmapEpoch) unavailable.value = true
  } finally {
    if (epoch === mindmapEpoch) loading.value = false
  }
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
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    if (epoch !== mindmapEpoch) return
    await load()
  } catch {
    if (epoch === mindmapEpoch) note.value = '导图生成接口暂不可用。'
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
  const font = 'font-family="\'Segoe UI\',\'PingFang SC\',\'Microsoft YaHei\',sans-serif"'
  const parts: string[] = [
    `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">`,
    `<rect width="${width}" height="${height}" fill="${bg}"/>`,
  ]
  for (const e of es) {
    const d = `M ${e.x1} ${e.y1} C ${e.x1 + 44} ${e.y1}, ${e.x2 - 44} ${e.y2}, ${e.x2} ${e.y2}`
    parts.push(`<path d="${d}" fill="none" stroke="${branchColor(e.branch)}" stroke-width="1.4" opacity="0.75"/>`)
  }
  for (const n of ns) {
    const color = n.depth === 0 ? primary : branchColor(n.branch)
    const fill = n.collapsed || !n.hasChildren ? color : bg
    parts.push(`<circle cx="${n.x}" cy="${n.y}" r="4.5" fill="${fill}" stroke="${color}" stroke-width="1.6"/>`)
    const fs = n.depth === 0 ? 13 : 12
    const fw = n.depth === 0 ? ' font-weight="700"' : ''
    parts.push(`<text x="${n.x + 11}" y="${n.y + 4}" fill="${n.depth === 0 ? textPrimary : textRegular}" font-size="${fs}"${fw} ${font}>${escXml(n.name)}</text>`)
    let bx = n.x + 16 + n.labelW
    if (n.docCount) {
      const w = badgeWidth(n.docCount)
      parts.push(`<rect x="${bx}" y="${n.y - 8}" width="${w}" height="14" rx="7" fill="none" stroke="${color}" stroke-width="1" opacity="0.85"/>`)
      parts.push(`<text x="${bx + w / 2}" y="${n.y + 2.5}" fill="${textFaint}" font-size="9.5" text-anchor="middle" ${font}>${n.docCount}</text>`)
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
  a.click()
  window.setTimeout(() => URL.revokeObjectURL(url), 5000)
}

function exportSvg() {
  if (!root.value) return
  download(`知识导图-${props.spaceId ?? 'space'}.svg`, new Blob([buildExportSvg()], { type: 'image/svg+xml;charset=utf-8' }))
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
      download(`知识导图-${props.spaceId ?? 'space'}.png`, blob)
    } finally {
      URL.revokeObjectURL(url)
    }
  } catch {
    note.value = '导出 PNG 失败，可改用导出 SVG。'
  } finally {
    exporting.value = false
  }
}

onBeforeUnmount(() => { mindmapEpoch++ })

void load()
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
        >{{ exporting ? '导出中' : '导出 PNG' }}</button>
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
        >{{ regenerating ? '生成中' : '重新生成' }}</button>
      </div>
    </div>
    <div v-if="note" class="mindmap-note" role="status">{{ note }}</div>

    <div class="mindmap-canvas">
      <div v-if="loading" class="mindmap-state" role="status">
        <strong>正在读取知识导图</strong><span>导图较大时需要几秒钟。</span>
      </div>
      <div v-else-if="!root" class="mindmap-state">
        <strong>{{ unavailable ? '知识导图暂不可用' : '导图尚未生成' }}</strong>
        <span>{{ unavailable ? '服务端导图接口尚未就绪，接口上线后会自动展示。' : '点击右上角「重新生成」按当前空间文档归纳导图。' }}</span>
        <button v-if="writable && !unavailable" class="primary-btn" type="button" :disabled="regenerating" @click="regenerate">生成导图</button>
      </div>
      <svg
        v-else :width="layout.width" :height="layout.height" :viewBox="`0 0 ${layout.width} ${layout.height}`"
        role="tree" aria-label="知识导图"
      >
        <path
          v-for="(edge, index) in layout.edges" :key="index"
          class="mm-edge" :stroke="branchColor(edge.branch)"
          :d="`M ${edge.x1} ${edge.y1} C ${edge.x1 + 44} ${edge.y1}, ${edge.x2 - 44} ${edge.y2}, ${edge.x2} ${edge.y2}`"
        />
        <g v-for="node in layout.nodes" :key="node.key" :transform="`translate(${node.x}, ${node.y})`">
          <circle
            class="mm-dot" :class="{ collapsed: node.collapsed, leaf: !node.hasChildren && !node.docId, doc: !!node.docId, section: !!node.chunkCount }"
            :stroke="node.depth === 0 ? 'var(--primary)' : branchColor(node.branch)"
            :fill="node.collapsed || (!node.hasChildren && !node.docId) ? (node.depth === 0 ? 'var(--primary)' : branchColor(node.branch)) : 'var(--bg-card)'"
            r="4.5" role="button" :aria-label="node.chunkCount ? `查看章节 ${node.name} 摘要` : node.docId ? `展开文档 ${node.name} 的章节` : node.collapsed ? `展开 ${node.name}` : `折叠 ${node.name}`"
            @click="onNodeClick(node)"
          />
          <text
            class="mm-label" :class="{ root: node.depth === 0, clickable: node.hasChildren || !!node.docId || !!node.chunkCount }"
            x="11" y="4" @click="onNodeClick(node)"
          >{{ node.name }}</text>
          <g v-if="node.docCount" class="mm-badge" :transform="`translate(${16 + node.labelW}, 0)`">
            <rect
              :width="badgeWidth(node.docCount)" height="14" y="-8" rx="7"
              :stroke="node.depth === 0 ? 'var(--primary)' : branchColor(node.branch)"
            />
            <text :x="badgeWidth(node.docCount) / 2" y="2.5">{{ node.docCount }}</text>
          </g>
          <g v-if="node.chunkCount" class="mm-badge chunks" :transform="`translate(${16 + node.labelW}, 0)`">
            <rect
              :width="badgeWidth(node.chunkCount)" height="14" y="-8" rx="7"
              :stroke="branchColor(node.branch)"
            />
            <text :x="badgeWidth(node.chunkCount) / 2" y="2.5">{{ node.chunkCount }}</text>
          </g>
          <text
            v-if="node.hiddenCount" class="mm-count"
            :x="18 + node.labelW + (node.docCount ? badgeWidth(node.docCount) + 5 : 0)" y="4"
          >+{{ node.hiddenCount }}</text>
        </g>
      </svg>
      <aside v-if="activeSection" class="mm-card" role="dialog" aria-label="章节摘要">
        <header>
          <strong :title="activeSection.name">{{ activeSection.name }}</strong>
          <button type="button" class="mm-card-close" aria-label="关闭摘要" @click="activeSection = null">×</button>
        </header>
        <div class="mm-card-meta">
          {{ activeSection.docName }} · {{ activeSection.chunkCount }} 块<template v-if="activeSection.page"> · 第 {{ activeSection.page }} 页起</template>
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
.mindmap-canvas {
  position: relative; min-height: 430px; margin-top: 12px; overflow: auto;
  border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card);
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
.mm-dot { cursor: pointer; stroke-width: 1.6; }
.mm-dot.leaf { cursor: default; }
.mm-dot.doc { stroke-dasharray: 2.5 2; }
.mm-dot.section { r: 3.5; }
.mm-badge.chunks rect { stroke-dasharray: 3 2; }
.mm-card {
  position: absolute; top: 12px; right: 12px; z-index: 2; width: 290px; max-height: calc(100% - 24px);
  display: flex; flex-direction: column; padding: 12px 14px; overflow: auto;
  border: 1px solid var(--border); border-radius: 8px; background: var(--bg-card); box-shadow: var(--shadow-lg);
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
