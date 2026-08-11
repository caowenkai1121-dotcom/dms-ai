<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { escHtml, sessionHeaders } from './panel-utils'

interface ChunkRow {
  ord: number; page?: number | null; heading?: string; text: string
}
type PreviewTab = 'file' | 'markdown' | 'chunks'
/** 原件预览分派种类（kindOf 的闭集）。office = Word/Excel/PPT：浏览器内嵌不了，渲染解析后的内容。 */
type FileKind = 'image' | 'pdf' | 'csv' | 'markdown' | 'json' | 'text' | 'html' | 'office' | 'none'
/** 目录条目（renderMarkdown 顺手收集，供目录跳转）。 */
interface TocEntry { id: string; level: number; text: string }

// initialPage：引用带进来的命中页码，仅 pdf 类预览（pdf 原件 / office 转换版 PDF，
// 两者页码一致）在加载时直挂 #page=N；txt/md/csv/图片等非 pdf 类忽略。
const props = defineProps<{ token?: string; docId: string; docName: string; mime?: string; initialPage?: number }>()
const emit = defineEmits<{
  (e: 'close'): void
  (e: 'auth-expired'): void
}>()

const activeTab = ref<PreviewTab>('file')
const fileLoading = ref(false)
const fileUrl = ref('')
const embedLoading = ref(false)
const officePdfUrl = ref('')
const fileKind = ref<FileKind>('none')
const fileText = ref('')
const csvRows = ref<string[][]>([])
const csvTruncated = ref(false)
const fileFailed = ref(false)
const fileErr = ref('')
const pdfFrag = ref('')
const markdownLoading = ref(false)
const markdownFailed = ref(false)
const markdownRan = ref(false)
const markdownHtml = ref('')
const markdownToc = ref<TocEntry[]>([])
const markdownErr = ref('')
const chunksLoading = ref(false)
const chunksFailed = ref(false)
const chunksRan = ref(false)
const chunks = ref<ChunkRow[]>([])
const chunksErr = ref('')
const chunkPage = ref(0)
const downloading = ref(false)
const downloadErr = ref('')
const dialogEl = ref<HTMLElement | null>(null)
let previewEpoch = 0

/** 入口页码归一化：只认正整数页（0/负数/小数/缺省都按「不跳页」）。 */
const initialPdfPage = computed(() => {
  const page = props.initialPage
  return typeof page === 'number' && Number.isInteger(page) && page > 0 ? page : 0
})

// 错误文案以服务端 `{"error": msg}` 为准（404「原始文件已不存在」与「暂无解析文本」是两种病，
// 笼统的「接口暂不可用」会把用户引到错的等待上）
async function errorText(response: Response): Promise<string> {
  try {
    const data = await response.json() as Record<string, unknown>
    if (typeof data?.error === 'string' && data.error.trim()) return data.error
  } catch { /* 非 JSON 错误体 */ }
  return `服务暂不可用（HTTP ${response.status}）`
}

/** 三段拉取同款流程：鉴权头 + 401 上报 + 非 ok 抛服务端 error 文案。 */
async function fetchOrThrow(url: string): Promise<Response> {
  const response = await fetch(url, { headers: sessionHeaders(props.token, () => emit('auth-expired')) })
  if (response.status === 401) emit('auth-expired')
  if (!response.ok) throw new Error(await errorText(response))
  return response
}

function extOf(name: string): string {
  const dot = name.lastIndexOf('.')
  // 点前无字符（.gitignore 这类点开头文件）视为无扩展名
  if (dot <= 0) return ''
  return name.slice(dot + 1).toLowerCase()
}

// 原件预览分派：扩展名优先（上传白名单保证它存在），mime 兜底（服务端下载已按扩展名白名单改写）。
// 🔴 svg 刻意不收（可执行脚本的 XSS 面）；tif/tiff 浏览器解不了 → 落 none 走下载提示；
// html 不按标记渲染，只展示转义后的原文（安全转文本）；
// Office（Word/Excel/PPT）归 office：优先服务端转换的 PDF 直链，转换不可用才回落到解析内容渲染。
function kindOf(name: string, mime: string): FileKind {
  const ext = extOf(name)
  if (['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp', 'avif'].includes(ext)) return 'image'
  if (ext === 'pdf') return 'pdf'
  if (ext === 'csv') return 'csv'
  if (['md', 'markdown'].includes(ext)) return 'markdown'
  if (ext === 'json') return 'json'
  if (['txt', 'log'].includes(ext)) return 'text'
  if (ext === 'html') return 'html'
  if (['doc', 'docx', 'ppt', 'pptx', 'xls', 'xlsx', 'xlsm'].includes(ext)) return 'office'
  const type = mime.split(';')[0].trim().toLowerCase()
  if (/^image\/(png|jpeg|webp|gif|bmp|avif)$/.test(type)) return 'image'
  if (type === 'application/pdf') return 'pdf'
  if (type === 'text/csv') return 'csv'
  if (type === 'application/json') return 'json'
  if (type === 'text/markdown') return 'markdown'
  if (type === 'text/plain') return 'text'
  if (type === 'application/msword' || type === 'application/vnd.ms-excel' || type === 'application/vnd.ms-powerpoint'
    || type.startsWith('application/vnd.openxmlformats-officedocument.')) return 'office'
  return 'none'
}

// 与服务端 `_read_text` 同口径：utf-8 优先，GBK 兜底（中文 Windows 产物常是 GBK）
async function blobText(blob: Blob): Promise<string> {
  const buf = await blob.arrayBuffer()
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(buf)
  } catch {
    return new TextDecoder('gbk').decode(buf)
  }
}

function prettyJson(text: string): string {
  try {
    return JSON.stringify(JSON.parse(text), null, 2)
  } catch {
    return text   // 非法 JSON 按原文展示（服务端入库时也是这个降级口径）
  }
}

// CSV 预览解析：引号/转义按 RFC4180，分隔符在 ,;\t| 里按首行嗅探（与服务端 `_p_csv` 同集合）。
// 大表只取前 CSV_PREVIEW_ROWS 行——预览不是数据面，全量走下载或问数。
const CSV_PREVIEW_ROWS = 200
/** 首行嗅探分隔符：跳过引号内字符（"a,b" 里的逗号不算分隔符）。 */
function sniffDelim(firstLine: string): string {
  const count = (delim: string): number => {
    let n = 0
    let quoted = false
    for (let i = 0; i < firstLine.length; i++) {
      const c = firstLine[i]
      if (c === '"') {
        if (quoted && firstLine[i + 1] === '"') i++
        else quoted = !quoted
      } else if (!quoted && c === delim) n++
    }
    return n
  }
  return [',', ';', '\t', '|'].reduce((a, b) => (count(b) > count(a) ? b : a), ',')
}
function parseCsv(text: string): { rows: string[][]; truncated: boolean } {
  const firstLine = text.split(/\r?\n/, 1)[0] ?? ''
  const delim = sniffDelim(firstLine)
  const rows: string[][] = []
  let field = ''
  let row: string[] = []
  let inQuotes = false
  let truncated = false
  const pushRow = () => {
    row.push(field)
    field = ''
    if (row.some((c) => c !== '')) rows.push(row)
    row = []
  }
  for (let i = 0; i < text.length; i++) {
    const c = text[i]
    if (inQuotes) {
      if (c === '"') {
        if (text[i + 1] === '"') { field += '"'; i++ } else inQuotes = false
      } else field += c
      continue
    }
    if (c === '"') { inQuotes = true; continue }
    if (c === delim) { row.push(field); field = ''; continue }
    if (c === '\n' || c === '\r') {
      if (c === '\r' && text[i + 1] === '\n') i++
      pushRow()
      if (rows.length > CSV_PREVIEW_ROWS) { truncated = true; break }
      continue
    }
    field += c
  }
  if (!truncated && (field !== '' || row.length)) pushRow()
  if (rows.length > CSV_PREVIEW_ROWS + 1) { rows.length = CSV_PREVIEW_ROWS + 1; truncated = true }
  return { rows, truncated }
}
/** 表体行预计算：模板里 slice(1) 每次渲染都新建数组。 */
const csvBodyRows = computed(() => csvRows.value.slice(1))

/** 预览票据：直链改走 ticket（120s 有效），iframe/img 才能绕开 Authorization 头直挂 URL。 */
async function previewTicket(): Promise<string> {
  const response = await fetch(`/api/kb/doc/${encodeURIComponent(props.docId)}/preview-ticket`, {
    method: 'POST',
    headers: sessionHeaders(props.token, () => emit('auth-expired')),
  })
  if (response.status === 401) emit('auth-expired')
  if (!response.ok) throw new Error(await errorText(response))
  const data = await response.json().catch(() => ({})) as Record<string, unknown>
  const ticket = String(data.ticket ?? '')
  if (!ticket) throw new Error('预览票据签发失败')
  return ticket
}

/** 文件直链（ticket 鉴权、inline 渲染、支持 Range）；office_pdf=1 取服务端转换版 PDF。 */
function directFileUrl(ticket: string, officePdf = false): string {
  return `/api/kb/doc/${encodeURIComponent(props.docId)}/file?ticket=${encodeURIComponent(ticket)}&inline=1${officePdf ? '&office_pdf=1' : ''}`
}

/** 直链由浏览器渐进拉取：load 事件才撤 loading（v-show 常驻 DOM，否则等不到事件）。 */
function onEmbedLoad() {
  embedLoading.value = false
}
/** 直链失败（票据过期/原件缺失）：回到原有失败态 + Markdown 自动降级。 */
function onEmbedError() {
  embedLoading.value = false
  fileFailed.value = true
  fileErr.value = '原件直链加载失败'
  void autoMarkdown()
}
/** office 转换版 iframe 加载失败：不亮错误页，静默落回解析内容渲染分支。 */
function onOfficeEmbedError() {
  officePdfUrl.value = ''
  if (!markdownRan.value && !markdownLoading.value) void loadMarkdown()
}

async function fetchBlob(): Promise<Blob> {
  const response = await fetchOrThrow(`/api/kb/doc/${encodeURIComponent(props.docId)}/download`)
  return response.blob()
}

// 原件不可用（格式不可内嵌 / 原件已缺失）时，解析文本就是用户要的「预览」：
// 自动切到 Markdown 页签；解析文本也没有时才停在原件页签展示真实错误。
async function autoMarkdown() {
  if (!markdownRan.value && !markdownLoading.value) await loadMarkdown()
  if (markdownHtml.value) activeTab.value = 'markdown'
}

async function loadFile() {
  const epoch = previewEpoch
  fileLoading.value = true
  fileFailed.value = false
  fileErr.value = ''
  pdfFrag.value = ''
  embedLoading.value = false
  officePdfUrl.value = ''
  fileUrl.value = ''
  fileText.value = ''
  csvRows.value = []
  csvTruncated.value = false
  const kind = kindOf(props.docName, props.mime || '')
  fileKind.value = kind
  // 不可内嵌的格式（tif/svg 等）不浪费一次下载：直接落「下载 + Markdown 页签」提示
  if (kind === 'none') {
    fileLoading.value = false
    void autoMarkdown()
    return
  }
  try {
    // pdf/image：票据换直链，浏览器按 Range 渐进渲染，不再整 blob 下载；
    // loading 由 iframe/img 的 load 事件关闭（embedLoading），onerror 走原有失败态降级
    if (kind === 'image' || kind === 'pdf') {
      const ticket = await previewTicket()
      if (epoch !== previewEpoch) return
      embedLoading.value = true
      fileUrl.value = directFileUrl(ticket)
      // 引用跳页：与 jumpToPdfPage 同机制（iframe :key=pdfFrag），直链 + #page=N 一次到位
      if (kind === 'pdf' && initialPdfPage.value) pdfFrag.value = `#page=${initialPdfPage.value}`
      // 个别浏览器的内嵌 PDF 查看器不触发 iframe load：兜底撤 loading，不能把人锁在加载页
      window.setTimeout(() => { if (epoch === previewEpoch) embedLoading.value = false }, 8000)
      return
    }
    // office：先探测转换版 PDF（Range 1 字节，206 才有）；404 office_pdf_unavailable 等
    // 一律回落到「解析内容渲染」分支（保留的降级路径，与 Markdown 页签同源）
    if (kind === 'office') {
      try {
        const ticket = await previewTicket()
        if (epoch !== previewEpoch) return
        const probe = await fetch(directFileUrl(ticket, true), { headers: { Range: 'bytes=0-0' } })
        if (epoch !== previewEpoch) return
        if (probe.status === 206) {
          // office 转换版 PDF 的页码与原件一致：引用页码同样直挂 #page=N
          officePdfUrl.value = directFileUrl(ticket, true) + (initialPdfPage.value ? `#page=${initialPdfPage.value}` : '')
          return
        }
      } catch { /* 票据/探测失败都按转换不可用处理 */ }
      if (epoch !== previewEpoch) return
      if (!markdownRan.value && !markdownLoading.value) void loadMarkdown()
      return
    }
    // text/csv/json/html/md：小文件维持 blob 解析渲染不变
    const blob = await fetchBlob()
    if (epoch !== previewEpoch) return
    const text = await blobText(blob)
    if (epoch !== previewEpoch) return
    if (kind === 'csv') {
      const parsed = parseCsv(text)
      csvRows.value = parsed.rows
      csvTruncated.value = parsed.truncated
    } else {
      fileText.value = kind === 'json' ? prettyJson(text) : text
    }
  } catch (e) {
    if (epoch === previewEpoch) {
      fileFailed.value = true
      fileErr.value = e instanceof Error ? e.message : ''
      void autoMarkdown()
    }
  } finally {
    if (epoch === previewEpoch) fileLoading.value = false
  }
}

// md 原件复用本文件内 Markdown 页签的渲染器（本文件内只有这一份实现）；
// 注意 KbAnswer.vue 还各有一份面向回答正文的渲染器（支持表格/引用块），两边分叉是已知状态，合并是后话。
const fileMarkdownHtml = computed(() => (fileKind.value === 'markdown' ? renderMarkdown(fileText.value, 'kdp-mdh-file').html : ''))

function inlineMd(value: string): string {
  return value
    .replace(/\*\*([^*]+)\*\*/g, '<b>$1</b>')
    .replace(/`([^`]+)`/g, '<code>$1</code>')
}
// md 表格渲染：| a | b | 行切单元格；紧跟的 |---|---| 分隔行把首行抬成表头（GFM 的够用子集）。
// 主要消费者是 xlsx 解析产物（每 sheet 一张 md 表）与模型/文档里的表格段。
function renderTable(block: string[]): string {
  const cellsOf = (line: string): string[] =>
    line.trim().replace(/^\|/, '').replace(/\|$/, '').split('|').map((cell) => cell.trim())
  const isSepRow = (line: string): boolean => line.includes('-') && /^[|\s:-]+$/.test(line.trim())
  let header: string[] | null = null
  let bodyLines = block
  if (block.length >= 2 && isSepRow(block[1])) {
    header = cellsOf(block[0])
    bodyLines = block.slice(2)
  }
  const headHtml = header
    ? `<thead><tr>${header.map((cell) => `<th>${inlineMd(escHtml(cell))}</th>`).join('')}</tr></thead>`
    : ''
  const bodyHtml = bodyLines
    .filter((line) => !isSepRow(line))
    .map((line) => `<tr>${cellsOf(line).map((cell) => `<td>${inlineMd(escHtml(cell))}</td>`).join('')}</tr>`)
    .join('')
  return `<table class="md-table">${headHtml}<tbody>${bodyHtml}</tbody></table>`
}
// 渲染 + 顺手收目录（对齐 Yuxi 预览形态：目录跳转）。标题锚点 id 每次渲染重编；
// idPrefix 区分「文件页签的 md 原件」与「Markdown 页签」两份并存 DOM，防止 getElementById 撞车。
// 注意：目录文本在未转义原文上提取（esc 后的 &amp; 会直接显示在目录按钮上）。
function renderMarkdown(md: string, idPrefix = 'kdp-mdh'): { html: string; toc: TocEntry[] } {
  const out: string[] = []
  const toc: TocEntry[] = []
  let listTag: 'ul' | 'ol' | null = null
  let inCode = false
  const code: string[] = []
  const closeList = () => { if (listTag) { out.push(`</${listTag}>`); listTag = null } }
  const isTableLine = (line: string) => /^\s*\|.*\|\s*$/.test(line)
  const lines = md.split(/\r?\n/)
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]
    if (line.trimStart().startsWith('```')) {
      if (inCode) { out.push(`<pre class="md-code">${code.join('\n')}</pre>`); code.length = 0; inCode = false }
      else { closeList(); inCode = true }
      continue
    }
    if (inCode) { code.push(escHtml(line)); continue }
    // 表格块：连续的 |…| 行整体交给 renderTable（代码围栏内的竖线行不算——上面已拦截）
    if (isTableLine(line)) {
      closeList()
      const block: string[] = []
      while (i < lines.length && isTableLine(lines[i])) { block.push(lines[i]); i++ }
      i--
      out.push(renderTable(block))
      continue
    }
    if (!line.trim()) { closeList(); continue }
    const heading = /^(#{1,6})\s+(.*)$/.exec(line)
    if (heading) {
      closeList()
      // 标题降一级（+1）：预览弹窗场景；KbAnswer 回答卡片降两级（+2），差异有意勿对齐
      const level = Math.min(6, heading[1].length + 1)
      const id = `${idPrefix}-${toc.length}`
      toc.push({ id, level, text: heading[2].replace(/[*`]/g, '').trim() })
      out.push(`<h${level} id="${id}">${inlineMd(escHtml(heading[2]))}</h${level}>`)
      continue
    }
    const item = /^\s*([-*+]|\d+[.)])\s+(.*)$/.exec(line)
    if (item) {
      const nextTag: 'ul' | 'ol' = /^\d/.test(item[1]) ? 'ol' : 'ul'
      if (listTag !== nextTag) { closeList(); out.push(`<${nextTag}>`); listTag = nextTag }
      out.push(`<li>${inlineMd(escHtml(item[2]))}</li>`)
      continue
    }
    closeList()
    out.push(`<p>${inlineMd(escHtml(line))}</p>`)
  }
  closeList()
  if (inCode) out.push(`<pre class="md-code">${code.join('\n')}</pre>`)
  return { html: out.join(''), toc }
}

function jumpToHeading(id: string) {
  document.getElementById(id)?.scrollIntoView({ behavior: 'smooth', block: 'start' })
}

async function loadMarkdown() {
  const epoch = previewEpoch
  markdownLoading.value = true
  markdownFailed.value = false
  markdownErr.value = ''
  try {
    const response = await fetchOrThrow(`/api/kb/doc/${encodeURIComponent(props.docId)}/markdown`)
    const raw = await response.text()
    if (epoch !== previewEpoch) return
    let text = raw
    try {
      const data = JSON.parse(raw) as Record<string, unknown>
      // 只采信字符串字段：服务端误回对象/数字时按空处理，不渲染「[object Object]」
      text = typeof data.markdown === 'string' ? data.markdown
        : typeof data.text === 'string' ? data.text
          : typeof data.content === 'string' ? data.content : ''
    } catch { /* 服务端直接回 text/plain 时按原文渲染 */ }
    const rendered = renderMarkdown(text)
    markdownHtml.value = rendered.html
    markdownToc.value = rendered.toc
    markdownRan.value = true
  } catch (e) {
    if (epoch === previewEpoch) {
      markdownFailed.value = true
      markdownRan.value = true
      markdownErr.value = e instanceof Error ? e.message : ''
    }
  } finally {
    if (epoch === previewEpoch) markdownLoading.value = false
  }
}

/** 失败态重试：失败后 ran=true 不再自动重拉（防重试风暴），由按钮显式复位重跑。 */
function retryMarkdown() {
  markdownRan.value = false
  void loadMarkdown()
}

function normalizeChunks(input: unknown): ChunkRow[] {
  const list = Array.isArray(input) ? input
    : Array.isArray((input as Record<string, unknown>)?.chunks) ? (input as Record<string, unknown>).chunks as unknown[]
      : Array.isArray((input as Record<string, unknown>)?.items) ? (input as Record<string, unknown>).items as unknown[]
        : []
  const rows: ChunkRow[] = []
  list.forEach((raw, index) => {
    if (!raw || typeof raw !== 'object') return
    const item = raw as Record<string, unknown>
    // heading_path 数组形态的分隔符与服务端字符串形态一致用「 > 」（store.rs 的拼接口径）
    const heading = Array.isArray(item.heading_path)
      ? item.heading_path.filter(Boolean).join(' > ')
      : String(item.heading_path ?? item.heading ?? '').trim()
    rows.push({
      ord: Number(item.ord ?? item.chunk_ord ?? item.index ?? index + 1) || index + 1,
      page: typeof item.page === 'number' ? item.page : typeof item.page_no === 'number' ? item.page_no : null,
      heading,
      text: String(item.text ?? item.content ?? item.preview ?? ''),
    })
  })
  return rows
}

// 切片分页（对齐 Yuxi 预览形态：分页）：大文档几百块一次渲染既卡又难读，按页翻
const CHUNK_PAGE_SIZE = 20
const chunkPageCount = computed(() => Math.max(1, Math.ceil(chunks.value.length / CHUNK_PAGE_SIZE)))
const pagedChunks = computed(() =>
  chunks.value.slice(chunkPage.value * CHUNK_PAGE_SIZE, (chunkPage.value + 1) * CHUNK_PAGE_SIZE))
function flipChunkPage(delta: number) {
  chunkPage.value = Math.min(chunkPageCount.value - 1, Math.max(0, chunkPage.value + delta))
}

// 原文对照（对齐 Yuxi 预览形态）：PDF 原件按页锚点跳（:key 强制 iframe 重挂，查看器才会认新页码）。
// 直链后 fileUrl 是普通 URL，`直链 + #page=N` 与 blob URL 一样有效。
function jumpToPdfPage(page?: number | null) {
  if (!page || fileKind.value !== 'pdf' || !fileUrl.value) return
  pdfFrag.value = `#page=${page}`
  activeTab.value = 'file'
}

async function loadChunks() {
  const epoch = previewEpoch
  chunksLoading.value = true
  chunksFailed.value = false
  chunksErr.value = ''
  try {
    const response = await fetchOrThrow(`/api/kb/doc/${encodeURIComponent(props.docId)}/chunks`)
    const data = await response.json().catch(() => ({}))
    if (epoch !== previewEpoch) return
    chunks.value = normalizeChunks(data)
    chunkPage.value = 0
    chunksRan.value = true
  } catch (e) {
    if (epoch === previewEpoch) {
      chunksFailed.value = true
      chunksRan.value = true
      chunksErr.value = e instanceof Error ? e.message : ''
    }
  } finally {
    if (epoch === previewEpoch) chunksLoading.value = false
  }
}

/** 失败态重试：同 retryMarkdown 的口径。 */
function retryChunks() {
  chunksRan.value = false
  void loadChunks()
}

function switchTab(tab: PreviewTab) {
  if (tab === activeTab.value) return
  activeTab.value = tab
  if (tab === 'markdown' && !markdownRan.value && !markdownLoading.value) void loadMarkdown()
  if (tab === 'chunks' && !chunksRan.value && !chunksLoading.value) void loadChunks()
}

async function downloadOriginal() {
  if (downloading.value) return
  const epoch = previewEpoch
  downloading.value = true
  downloadErr.value = ''
  try {
    const blob = await fetchBlob()
    if (epoch !== previewEpoch) return
    const url = URL.createObjectURL(blob)
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.download = props.docName || 'knowledge-file'
    document.body.appendChild(anchor)
    anchor.click()
    anchor.remove()
    window.setTimeout(() => URL.revokeObjectURL(url), 0)
  } catch {
    // 轻量错误提示（头部按钮旁红字，自动消隐）：下载失败不能零反馈
    if (epoch === previewEpoch) {
      downloadErr.value = '下载失败，请稍后重试'
      window.setTimeout(() => { downloadErr.value = '' }, 3000)
    }
  } finally {
    if (epoch === previewEpoch) downloading.value = false
  }
}

function close() {
  emit('close')
}

// 防御性复用：父级目前靠 v-if 重挂载（KbPanel），但一旦复用为「切文档不关窗」，
// 各页签状态必须随 docId 复位重拉，否则内容滞留。
watch(() => props.docId, (id, old) => {
  if (id === old) return
  previewEpoch++
  activeTab.value = 'file'
  embedLoading.value = false
  officePdfUrl.value = ''
  markdownRan.value = false
  markdownFailed.value = false
  markdownHtml.value = ''
  markdownToc.value = []
  markdownErr.value = ''
  chunksRan.value = false
  chunksFailed.value = false
  chunks.value = []
  chunksErr.value = ''
  chunkPage.value = 0
  void loadFile()
})

// Esc 走 document 监听：section 不可聚焦时（先点了遮罩）也能关掉
function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') close()
}
onMounted(() => {
  document.addEventListener('keydown', onKey)
  dialogEl.value?.focus()
})
onBeforeUnmount(() => {
  document.removeEventListener('keydown', onKey)
  previewEpoch++
})

void loadFile()
</script>

<template>
  <div class="kdp-mask" @click.self="close">
    <section ref="dialogEl" class="kdp" role="dialog" aria-modal="true" aria-labelledby="kdp-title" tabindex="-1">
      <header class="kdp-head">
        <span class="kdp-ext" aria-hidden="true">{{ (extOf(docName).toUpperCase() || 'FILE').slice(0, 4) }}</span>
        <strong id="kdp-title" :title="docName">{{ docName }}</strong>
        <!-- 普通按钮组（非 WAI tablist 半套实现）：用 aria-pressed 表达选中态 -->
        <nav class="kdp-tabs" aria-label="预览方式">
          <button type="button" :class="{ active: activeTab === 'file' }" :aria-pressed="activeTab === 'file'" @click="switchTab('file')">文件</button>
          <button type="button" :class="{ active: activeTab === 'markdown' }" :aria-pressed="activeTab === 'markdown'" @click="switchTab('markdown')">Markdown</button>
          <button type="button" :class="{ active: activeTab === 'chunks' }" :aria-pressed="activeTab === 'chunks'" @click="switchTab('chunks')">Chunks</button>
        </nav>
        <div class="kdp-actions">
          <span v-if="downloadErr" class="kdp-dl-err" role="alert">{{ downloadErr }}</span>
          <button class="secondary-btn" type="button" :disabled="downloading" @click="downloadOriginal">{{ downloading ? '下载中' : '下载' }}</button>
          <button class="icon-btn" type="button" title="关闭" aria-label="关闭预览" @click="close">×</button>
        </div>
      </header>

      <div class="kdp-body">
        <section v-show="activeTab === 'file'" class="kdp-pane file" aria-label="原件预览">
          <div v-if="fileLoading" class="kdp-state" role="status">
            <strong>正在加载原件</strong><span>大文件可能需要几秒钟。</span>
          </div>
          <div v-else-if="fileFailed" class="kdp-state">
            <strong>原件预览暂不可用</strong>
            <span>{{ fileErr || '服务端暂未提供该文档的原件内容' }}；可切换到 Markdown 或 Chunks 页签查看解析结果。</span>
          </div>
          <!-- pdf/image 直链：iframe/img 用 v-show 常驻 DOM（v-if 会等不到 load 事件），loading 层盖住直到加载完成 -->
          <template v-else-if="fileKind === 'image' && fileUrl">
            <div v-if="embedLoading" class="kdp-state" role="status">
              <strong>正在加载原件</strong><span>图片由浏览器按原始尺寸渐进加载。</span>
            </div>
            <div v-show="!embedLoading" class="kdp-image-wrap">
              <img :src="fileUrl" :alt="docName" @load="onEmbedLoad" @error="onEmbedError">
            </div>
          </template>
          <template v-else-if="fileKind === 'pdf' && fileUrl">
            <div v-if="embedLoading" class="kdp-state" role="status">
              <strong>正在加载原件</strong><span>PDF 由浏览器渐进渲染，大文件需要几秒钟。</span>
            </div>
            <iframe v-show="!embedLoading" :key="pdfFrag" :src="fileUrl + pdfFrag" title="文档原件预览" @load="onEmbedLoad" @error="onEmbedError"></iframe>
          </template>
          <div v-else-if="fileKind === 'csv' && csvRows.length" class="kdp-table-wrap">
            <table class="kdp-table">
              <thead><tr><th v-for="(h, i) in csvRows[0]" :key="i">{{ h || '（空表头）' }}</th></tr></thead>
              <tbody>
                <tr v-for="(row, ri) in csvBodyRows" :key="ri">
                  <td v-for="(cell, ci) in row" :key="ci">{{ cell }}</td>
                </tr>
              </tbody>
            </table>
            <p v-if="csvTruncated" class="kdp-note">仅预览前 {{ CSV_PREVIEW_ROWS }} 行数据；完整内容请下载原件，或切换到 Markdown 页签查看解析结果。</p>
          </div>
          <article v-else-if="fileKind === 'markdown' && fileText" class="kdp-markdown" v-html="fileMarkdownHtml"></article>
          <pre v-else-if="(fileKind === 'text' || fileKind === 'json' || fileKind === 'html') && fileText" class="kdp-text">{{ fileText }}</pre>
          <div v-else-if="fileKind === 'office'" class="kdp-office">
            <!-- 服务端转换版 PDF（探测 206 才进这分支）；转换不可用落解析内容渲染 -->
            <template v-if="officePdfUrl">
              <p class="kdp-note kdp-office-note">已转为 PDF 预览，原始文件请下载查看。</p>
              <iframe class="kdp-office-frame" :src="officePdfUrl" title="Office 文档的 PDF 转换预览" @error="onOfficeEmbedError"></iframe>
            </template>
            <template v-else>
              <p class="kdp-note kdp-office-note">Office 原件按解析后的内容预览，版式可能与原件有差异；原始排版请下载原件查看。</p>
              <div v-if="markdownLoading" class="kdp-state" role="status">
                <strong>正在解析内容</strong><span>Office 文档解析需要几秒钟。</span>
              </div>
              <article v-else-if="markdownHtml" class="kdp-markdown" v-html="markdownHtml"></article>
              <div v-else class="kdp-state">
                <strong>未能解析出可预览的内容</strong>
                <span>{{ markdownFailed ? (markdownErr || '解析失败，请稍后重试。') : '该文档没有解析出文本内容。' }}可下载原件后用本地应用打开。</span>
                <button class="primary-btn" type="button" :disabled="downloading" @click="downloadOriginal">下载原件</button>
              </div>
            </template>
          </div>
          <div v-else-if="fileKind !== 'none'" class="kdp-state">
            <strong>文件为空</strong>
            <span>原件没有任何内容；可切换到 Markdown 或 Chunks 页签查看解析结果。</span>
          </div>
          <div v-else class="kdp-state">
            <strong>该格式不支持内嵌预览</strong>
            <span>可下载原件后用本地应用打开；Markdown 与 Chunks 页签可直接查看解析内容。</span>
            <button class="primary-btn" type="button" :disabled="downloading" @click="downloadOriginal">下载原件</button>
          </div>
        </section>

        <section v-show="activeTab === 'markdown'" class="kdp-pane" aria-label="Markdown 预览">
          <div v-if="markdownLoading" class="kdp-state" role="status">
            <strong>正在加载 Markdown</strong><span>解析文本较大时需要几秒钟。</span>
          </div>
          <div v-else-if="markdownHtml" class="kdp-md-wrap">
            <!-- 单标题不出目录：只有一个标题时目录没有导航价值 -->
            <nav v-if="markdownToc.length >= 2" class="kdp-toc" aria-label="文档目录">
              <div class="kdp-toc-title">目录</div>
              <button
                v-for="entry in markdownToc" :key="entry.id" type="button"
                class="kdp-toc-item" :class="`lv${entry.level}`" :title="entry.text"
                @click="jumpToHeading(entry.id)"
              >{{ entry.text }}</button>
            </nav>
            <article class="kdp-markdown" v-html="markdownHtml"></article>
          </div>
          <div v-else class="kdp-state">
            <strong>{{ markdownFailed ? 'Markdown 暂不可用' : '没有可显示的 Markdown' }}</strong>
            <span>{{ markdownFailed ? (markdownErr || '解析文本读取失败，请稍后重试。') : '该文档没有解析出文本内容。' }}</span>
            <button v-if="markdownFailed" class="secondary-btn" type="button" @click="retryMarkdown">重试</button>
          </div>
        </section>

        <section v-show="activeTab === 'chunks'" class="kdp-pane" aria-label="切片列表">
          <div v-if="chunksLoading" class="kdp-state" role="status">
            <strong>正在加载切片</strong><span>按文档顺序读取全部文本块。</span>
          </div>
          <template v-else-if="chunks.length">
            <div class="kdp-chunks-summary">
              <span>共 {{ chunks.length }} 个切片 · 第 {{ chunkPage + 1 }} / {{ chunkPageCount }} 页</span>
              <span v-if="chunkPageCount > 1" class="kdp-pager">
                <button type="button" :disabled="chunkPage === 0" @click="flipChunkPage(-1)">上一页</button>
                <button type="button" :disabled="chunkPage >= chunkPageCount - 1" @click="flipChunkPage(1)">下一页</button>
              </span>
            </div>
            <article v-for="chunk in pagedChunks" :key="chunk.ord" class="kdp-chunk">
              <header>
                <span class="kdp-chunk-ord">#{{ chunk.ord }}</span>
                <span v-if="chunk.page" class="kdp-chunk-page">第 {{ chunk.page }} 页</span>
                <span v-if="chunk.heading" class="kdp-chunk-heading" :title="chunk.heading">{{ chunk.heading }}</span>
                <button
                  v-if="chunk.page && fileKind === 'pdf' && fileUrl" type="button" class="kdp-jump"
                  :title="`在原件中打开第 ${chunk.page} 页`" @click="jumpToPdfPage(chunk.page)"
                >对照原件</button>
              </header>
              <p>{{ chunk.text || '（空切片）' }}</p>
            </article>
          </template>
          <div v-else class="kdp-state">
            <strong>{{ chunksFailed ? '切片列表暂不可用' : '没有切片' }}</strong>
            <span>{{ chunksFailed ? (chunksErr || '切片读取失败，请稍后重试。') : '该文档尚未完成切片。' }}</span>
            <button v-if="chunksFailed" class="secondary-btn" type="button" @click="retryChunks">重试</button>
          </div>
        </section>
      </div>
    </section>
  </div>
</template>

<style scoped>
.kdp-mask {
  position: fixed; inset: 0; z-index: 70; padding: 22px;
  display: grid; place-items: center; background: rgba(16, 22, 43, .55);
}
.kdp {
  /* 高度里的 44px = 遮罩上下 padding 22×2，两者耦合勿单改 */
  width: min(980px, 100%); height: min(760px, calc(100vh - 44px));
  display: flex; flex-direction: column; overflow: hidden;
  background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; box-shadow: var(--shadow-lg);
}
.kdp:focus { outline: none; }
.kdp-head {
  flex: 0 0 auto; display: flex; align-items: center; gap: 10px;
  padding: 10px 14px; border-bottom: 1px solid var(--divider);
}
.kdp-ext {
  flex: 0 0 34px; height: 34px; display: grid; place-items: center;
  border: 1px solid var(--border); border-radius: 5px; background: var(--bg-main);
  color: var(--primary); font-size: 9px; font-weight: 800;
}
.kdp-head > strong {
  min-width: 0; max-width: 300px; overflow: hidden; color: var(--text-primary); font-size: 13px;
  text-overflow: ellipsis; white-space: nowrap;
}
.kdp-tabs { display: flex; gap: 4px; margin-left: 14px; }
.kdp-tabs button {
  height: 30px; padding: 0 12px; border: 0; border-bottom: 2px solid transparent;
  background: transparent; color: var(--text-muted); cursor: pointer; font: inherit; font-size: 12.5px;
}
.kdp-tabs button:hover { color: var(--text-primary); }
.kdp-tabs button.active { border-bottom-color: var(--primary); color: var(--primary); font-weight: 700; }
.kdp-actions { margin-left: auto; display: flex; align-items: center; gap: 8px; }
.kdp-dl-err { color: var(--error-text); font-size: 11.5px; white-space: nowrap; }
.secondary-btn, .primary-btn, .icon-btn {
  height: 32px; border: 1px solid var(--border); border-radius: 6px; cursor: pointer; font: inherit; font-size: 12px;
}
.secondary-btn { padding: 0 13px; background: var(--bg-card); color: var(--text-regular); }
.secondary-btn:hover, .icon-btn:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-light); }
.primary-btn { padding: 0 13px; border-color: var(--primary); background: var(--primary); color: #fff; }
.primary-btn:hover { background: var(--primary-hover); }
.icon-btn { width: 32px; padding: 0; background: var(--bg-card); color: var(--text-regular); font-size: 17px; }
/* 禁用态只圈本组件用到的按钮类，不用裸 button:disabled 影响未来子组件 */
.secondary-btn:disabled, .primary-btn:disabled, .icon-btn:disabled, .kdp-pager button:disabled { cursor: not-allowed; opacity: .55; }
.kdp-body { min-height: 0; flex: 1; display: flex; flex-direction: column; }
.kdp-pane { min-height: 0; flex: 1; display: flex; flex-direction: column; overflow: auto; }
.kdp-pane.file { overflow: hidden; }
.kdp-pane.file iframe { flex: 1; width: 100%; border: 0; background: var(--bg-main); }
.kdp-image-wrap {
  flex: 1; min-height: 0; display: grid; place-items: center; overflow: auto;
  padding: 16px; background: var(--bg-main);
}
.kdp-image-wrap img {
  max-width: 100%; max-height: 100%; object-fit: contain;
  border: 1px solid var(--border); border-radius: 6px; background: #fff;
}
.kdp-table-wrap { flex: 1; min-height: 0; overflow: auto; padding: 12px 16px; }
.kdp-table { border-collapse: collapse; width: 100%; color: var(--text-regular); font-size: 12px; }
.kdp-table th, .kdp-table td {
  max-width: 260px; padding: 5px 9px; overflow: hidden; border: 1px solid var(--border);
  text-align: left; text-overflow: ellipsis; white-space: nowrap;
}
.kdp-table thead th {
  position: sticky; top: 0; z-index: 1; background: var(--bg-main);
  color: var(--text-primary); font-weight: 700;
}
.kdp-note { margin: 10px 2px 0; color: var(--text-muted); font-size: 11.5px; }
/* Office 预览：file 页签容器是 overflow:hidden（为 iframe 定的），解析内容要自己开滚动 */
.kdp-office { flex: 1; min-height: 0; display: flex; flex-direction: column; overflow: auto; }
.kdp-office-note { flex: none; margin: 10px 18px 0; }
.kdp-office .kdp-markdown { flex: 1; }
/* 转换版 PDF 的 iframe 嵌在 .kdp-office 里（不是 .kdp-pane.file 直接子级）， flex 高度要单独给 */
.kdp-office-frame { flex: 1; min-height: 0; width: 100%; border: 0; background: var(--bg-main); }
.kdp-text {
  flex: 1; min-height: 0; margin: 0; padding: 16px 20px; overflow: auto;
  background: var(--bg-main); color: var(--text-regular);
  font: 12px/1.7 var(--font-mono); white-space: pre-wrap; overflow-wrap: anywhere;
}
.kdp-state {
  flex: 1; min-height: 240px; display: flex; align-items: center; justify-content: center;
  flex-direction: column; gap: 8px; color: var(--text-muted); text-align: center; font-size: 12px;
}
.kdp-state strong { color: var(--text-primary); font-size: 14px; }
.kdp-state span { max-width: 460px; line-height: 1.6; }
.kdp-state .primary-btn, .kdp-state .secondary-btn { margin-top: 6px; }
.kdp-markdown { padding: 18px 22px 28px; color: var(--text-regular); font-size: 13px; line-height: 1.75; overflow-wrap: anywhere; }
.kdp-md-wrap { flex: 1; min-height: 0; display: flex; overflow: auto; }
.kdp-md-wrap .kdp-markdown { flex: 1; min-width: 0; }
.kdp-toc {
  flex: 0 0 190px; position: sticky; top: 0; align-self: flex-start; max-height: 100%; overflow: auto;
  padding: 14px 10px 14px 14px; border-right: 1px solid var(--divider);
}
.kdp-toc-title { margin-bottom: 6px; color: var(--text-faint); font-size: 11px; }
.kdp-toc-item {
  display: block; width: 100%; padding: 3px 6px; overflow: hidden; border: 0; border-radius: 4px;
  background: transparent; color: var(--text-muted); cursor: pointer; font: inherit; font-size: 11.5px;
  text-align: left; text-overflow: ellipsis; white-space: nowrap;
}
.kdp-toc-item:hover { background: var(--primary-light); color: var(--primary); }
.kdp-toc-item.lv3 { padding-left: 16px; }
.kdp-toc-item.lv4 { padding-left: 26px; }
.kdp-toc-item.lv5 { padding-left: 36px; }
.kdp-toc-item.lv6 { padding-left: 46px; }
.kdp-pager { margin-left: auto; display: flex; gap: 6px; }
.kdp-pager button {
  height: 22px; padding: 0 9px; border: 1px solid var(--border); border-radius: 4px;
  background: var(--bg-card); color: var(--text-regular); cursor: pointer; font: inherit; font-size: 11px;
}
.kdp-pager button:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.kdp-jump {
  flex: none; margin-left: auto; height: 20px; padding: 0 8px; border: 1px solid var(--border); border-radius: 4px;
  background: var(--bg-card); color: var(--text-muted); cursor: pointer; font: inherit; font-size: 10.5px;
}
.kdp-jump:hover { border-color: var(--primary); color: var(--primary); }
.kdp-markdown :deep(h2), .kdp-markdown :deep(h3), .kdp-markdown :deep(h4),
.kdp-markdown :deep(h5), .kdp-markdown :deep(h6) {
  margin: 16px 0 8px; color: var(--text-primary); line-height: 1.4;
}
.kdp-markdown :deep(h2) { font-size: 19px; }
.kdp-markdown :deep(h3) { font-size: 16.5px; }
.kdp-markdown :deep(h4) { font-size: 14.5px; }
.kdp-markdown :deep(h5), .kdp-markdown :deep(h6) { font-size: 13px; }
.kdp-markdown :deep(p) { margin: 7px 0; }
.kdp-markdown :deep(ul), .kdp-markdown :deep(ol) { margin: 7px 0; padding-left: 22px; }
.kdp-markdown :deep(li) { margin: 3px 0; }
.kdp-markdown :deep(code) {
  padding: 1px 5px; border: 1px solid var(--border); border-radius: 4px;
  background: var(--bg-main); font-family: var(--font-mono); font-size: 12px;
}
.kdp-markdown :deep(.md-code) {
  margin: 10px 0; padding: 10px 12px; overflow: auto; white-space: pre-wrap;
  border: 1px solid var(--border); border-radius: 6px; background: var(--bg-main);
  font: 12px/1.65 var(--font-mono);
}
.kdp-markdown :deep(.md-table) { margin: 10px 0; width: 100%; border-collapse: collapse; font-size: 12px; }
.kdp-markdown :deep(.md-table th), .kdp-markdown :deep(.md-table td) {
  max-width: 280px; padding: 5px 9px; overflow: hidden; border: 1px solid var(--border);
  text-align: left; text-overflow: ellipsis;
}
.kdp-markdown :deep(.md-table thead th) { background: var(--bg-main); color: var(--text-primary); font-weight: 700; }
.kdp-chunks-summary {
  position: sticky; top: 0; z-index: 1; display: flex; align-items: center; gap: 8px; padding: 8px 14px;
  border-bottom: 1px solid var(--divider); background: var(--bg-main);
  color: var(--text-muted); font-size: 11.5px;
}
.kdp-chunk { margin: 10px 14px 0; padding: 10px 12px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card); }
.kdp-chunk:last-child { margin-bottom: 14px; }
.kdp-chunk header { display: flex; align-items: center; gap: 8px; }
.kdp-chunk-ord {
  flex: none; padding: 1px 7px; border-radius: 999px;
  background: var(--primary-light); color: var(--primary); font-size: 10.5px; font-weight: 700;
  font-variant-numeric: tabular-nums;
}
.kdp-chunk-page { flex: none; color: var(--text-faint); font-size: 10.5px; }
.kdp-chunk-heading {
  min-width: 0; overflow: hidden; color: var(--text-muted); font-size: 11px;
  text-overflow: ellipsis; white-space: nowrap;
}
.kdp-chunk p {
  margin-top: 7px; color: var(--text-regular); font-size: 12px; line-height: 1.7;
  white-space: pre-wrap; overflow-wrap: anywhere;
}
@media (max-width: 720px) {
  .kdp-mask { padding: 0; }
  .kdp { width: 100%; height: 100%; border: 0; border-radius: 0; }
  .kdp-head { flex-wrap: wrap; }
  .kdp-tabs { order: 3; width: 100%; margin-left: 0; }
}
</style>
