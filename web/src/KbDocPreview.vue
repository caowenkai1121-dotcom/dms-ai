<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue'

interface ChunkRow {
  ord: number; page?: number | null; heading?: string; text: string
}
type PreviewTab = 'file' | 'markdown' | 'chunks'

const props = defineProps<{ token?: string; docId: string; docName: string; mime?: string }>()
const emit = defineEmits<{
  (e: 'close'): void
  (e: 'auth-expired'): void
}>()

const activeTab = ref<PreviewTab>('file')
const fileLoading = ref(false)
const fileUrl = ref('')
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
let previewEpoch = 0

function headers(): Record<string, string> {
  const token = props.token?.trim()
  if (!token) {
    emit('auth-expired')
    throw new Error('登录会话已失效，请重新登录。')
  }
  return { Authorization: `Bearer ${token}` }
}

// 错误文案以服务端 `{"error": msg}` 为准（404「原始文件已不存在」与「暂无解析文本」是两种病，
// 笼统的「接口暂不可用」会把用户引到错的等待上）
async function errorText(response: Response): Promise<string> {
  try {
    const data = await response.json() as Record<string, unknown>
    if (typeof data?.error === 'string' && data.error.trim()) return data.error
  } catch { /* 非 JSON 错误体 */ }
  return `HTTP ${response.status}`
}

function extOf(name: string): string {
  const ext = name.split('.').pop()
  return ext && ext !== name ? ext.toLowerCase() : ''
}

// 原件预览分派：扩展名优先（上传白名单保证它存在），mime 兜底（服务端下载已按扩展名白名单改写）。
// 🔴 svg 刻意不收（可执行脚本的 XSS 面）；tif/tiff 浏览器解不了 → 落 none 走下载提示；
// html 不按标记渲染，只展示转义后的原文（安全转文本）。
type FileKind = 'image' | 'pdf' | 'csv' | 'markdown' | 'json' | 'text' | 'html' | 'none'
function kindOf(name: string, mime: string): FileKind {
  const ext = extOf(name)
  if (['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp'].includes(ext)) return 'image'
  if (ext === 'pdf') return 'pdf'
  if (ext === 'csv') return 'csv'
  if (['md', 'markdown'].includes(ext)) return 'markdown'
  if (ext === 'json') return 'json'
  if (['txt', 'log'].includes(ext)) return 'text'
  if (ext === 'html') return 'html'
  const type = mime.split(';')[0].trim().toLowerCase()
  if (/^image\/(png|jpeg|webp|gif|bmp)$/.test(type)) return 'image'
  if (type === 'application/pdf') return 'pdf'
  if (type === 'text/plain') return 'text'
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
function parseCsv(text: string): { rows: string[][]; truncated: boolean } {
  const firstLine = text.split(/\r?\n/, 1)[0] ?? ''
  const delim = [',', ';', '\t', '|'].reduce(
    (a, b) => (firstLine.split(b).length > firstLine.split(a).length ? b : a), ',',
  )
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

function revokeFileUrl() {
  if (fileUrl.value) URL.revokeObjectURL(fileUrl.value)
  fileUrl.value = ''
}

async function fetchBlob(): Promise<Blob> {
  const response = await fetch(`/api/kb/doc/${encodeURIComponent(props.docId)}/download`, { headers: headers() })
  if (response.status === 401) emit('auth-expired')
  if (!response.ok) throw new Error(await errorText(response))
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
  revokeFileUrl()
  fileText.value = ''
  csvRows.value = []
  csvTruncated.value = false
  const kind = kindOf(props.docName, props.mime || '')
  fileKind.value = kind
  // 不可内嵌的格式（Office 等）不浪费一次下载：直接落「下载 + Markdown 页签」提示
  if (kind === 'none') {
    fileLoading.value = false
    void autoMarkdown()
    return
  }
  try {
    const blob = await fetchBlob()
    if (epoch !== previewEpoch) return
    if (kind === 'image' || kind === 'pdf') {
      fileUrl.value = URL.createObjectURL(blob)
    } else {
      const text = await blobText(blob)
      if (epoch !== previewEpoch) return
      if (kind === 'csv') {
        const parsed = parseCsv(text)
        csvRows.value = parsed.rows
        csvTruncated.value = parsed.truncated
      } else {
        fileText.value = kind === 'json' ? prettyJson(text) : text
      }
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

// md 原件直接复用 Markdown 页签的渲染器（同一份实现，不引入第二份）
const fileMarkdownHtml = computed(() => (fileKind.value === 'markdown' ? renderMarkdown(fileText.value).html : ''))

interface TocEntry { id: string; level: number; text: string }

function esc(value: string): string {
  return value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')
}
function inlineMd(value: string): string {
  return value
    .replace(/\*\*([^*]+)\*\*/g, '<b>$1</b>')
    .replace(/`([^`]+)`/g, '<code>$1</code>')
}
// 渲染 + 顺手收目录（对齐 Yuxi 预览形态：目录跳转）。标题锚点 id 每次渲染重编，
// 弹窗单实例，不会与页面上其他元素撞 id。
function renderMarkdown(md: string): { html: string; toc: TocEntry[] } {
  const out: string[] = []
  const toc: TocEntry[] = []
  let listTag: 'ul' | 'ol' | null = null
  let inCode = false
  const code: string[] = []
  const closeList = () => { if (listTag) { out.push(`</${listTag}>`); listTag = null } }
  for (const line of esc(md).split(/\r?\n/)) {
    if (line.trimStart().startsWith('```')) {
      if (inCode) { out.push(`<pre class="md-code">${code.join('\n')}</pre>`); code.length = 0; inCode = false }
      else { closeList(); inCode = true }
      continue
    }
    if (inCode) { code.push(line); continue }
    if (!line.trim()) { closeList(); continue }
    const heading = /^(#{1,6})\s+(.*)$/.exec(line)
    if (heading) {
      closeList()
      const level = Math.min(6, heading[1].length + 1)
      const id = `kdp-mdh-${toc.length}`
      toc.push({ id, level, text: heading[2].replace(/[*`]/g, '').trim() })
      out.push(`<h${level} id="${id}">${inlineMd(heading[2])}</h${level}>`)
      continue
    }
    const item = /^\s*([-*+]|\d+[.)])\s+(.*)$/.exec(line)
    if (item) {
      const nextTag: 'ul' | 'ol' = /^\d/.test(item[1]) ? 'ol' : 'ul'
      if (listTag !== nextTag) { closeList(); out.push(`<${nextTag}>`); listTag = nextTag }
      out.push(`<li>${inlineMd(item[2])}</li>`)
      continue
    }
    closeList()
    out.push(`<p>${inlineMd(line)}</p>`)
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
    const response = await fetch(`/api/kb/doc/${encodeURIComponent(props.docId)}/markdown`, { headers: headers() })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw new Error(await errorText(response))
    const raw = await response.text()
    if (epoch !== previewEpoch) return
    let text = raw
    try {
      const data = JSON.parse(raw) as Record<string, unknown>
      text = String(data.markdown ?? data.text ?? data.content ?? '')
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

function normalizeChunks(input: unknown): ChunkRow[] {
  const list = Array.isArray(input) ? input
    : Array.isArray((input as Record<string, unknown>)?.chunks) ? (input as Record<string, unknown>).chunks as unknown[]
      : Array.isArray((input as Record<string, unknown>)?.items) ? (input as Record<string, unknown>).items as unknown[]
        : []
  const rows: ChunkRow[] = []
  list.forEach((raw, index) => {
    if (!raw || typeof raw !== 'object') return
    const item = raw as Record<string, unknown>
    const heading = Array.isArray(item.heading_path)
      ? item.heading_path.filter(Boolean).join(' / ')
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

// 原文对照（对齐 Yuxi 预览形态）：PDF 原件按页锚点跳（:key 强制 iframe 重挂，查看器才会认新页码）
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
    const response = await fetch(`/api/kb/doc/${encodeURIComponent(props.docId)}/chunks`, { headers: headers() })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw new Error(await errorText(response))
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
  } catch { /* 预览弹窗内下载失败不打扰：文件页签已有占位提示 */ }
  finally {
    if (epoch === previewEpoch) downloading.value = false
  }
}

function close() {
  emit('close')
}

onBeforeUnmount(() => {
  previewEpoch++
  revokeFileUrl()
})

void loadFile()
</script>

<template>
  <div class="kdp-mask" @click.self="close">
    <section class="kdp" role="dialog" aria-modal="true" aria-labelledby="kdp-title" @keydown.esc.stop="close">
      <header class="kdp-head">
        <span class="kdp-ext" aria-hidden="true">{{ extOf(docName).toUpperCase() || 'FILE' }}</span>
        <strong id="kdp-title" :title="docName">{{ docName }}</strong>
        <nav class="kdp-tabs" role="tablist" aria-label="预览方式">
          <button type="button" role="tab" :class="{ active: activeTab === 'file' }" :aria-selected="activeTab === 'file'" @click="switchTab('file')">文件</button>
          <button type="button" role="tab" :class="{ active: activeTab === 'markdown' }" :aria-selected="activeTab === 'markdown'" @click="switchTab('markdown')">Markdown</button>
          <button type="button" role="tab" :class="{ active: activeTab === 'chunks' }" :aria-selected="activeTab === 'chunks'" @click="switchTab('chunks')">Chunks</button>
        </nav>
        <div class="kdp-actions">
          <button class="secondary-btn" type="button" :disabled="downloading" @click="downloadOriginal">{{ downloading ? '下载中' : '下载' }}</button>
          <button class="icon-btn" type="button" title="关闭" aria-label="关闭预览" @click="close">×</button>
        </div>
      </header>

      <div class="kdp-body">
        <section v-show="activeTab === 'file'" class="kdp-pane file" role="tabpanel" aria-label="原件预览">
          <div v-if="fileLoading" class="kdp-state" role="status">
            <strong>正在加载原件</strong><span>大文件可能需要几秒钟。</span>
          </div>
          <div v-else-if="fileFailed" class="kdp-state">
            <strong>原件预览暂不可用</strong>
            <span>{{ fileErr || '服务端暂未提供该文档的原件内容' }}；可切换到 Markdown 或 Chunks 页签查看解析结果。</span>
          </div>
          <div v-else-if="fileKind === 'image' && fileUrl" class="kdp-image-wrap">
            <img :src="fileUrl" :alt="docName">
          </div>
          <iframe v-else-if="fileKind === 'pdf' && fileUrl" :key="pdfFrag" :src="fileUrl + pdfFrag" title="文档原件预览"></iframe>
          <div v-else-if="fileKind === 'csv' && csvRows.length" class="kdp-table-wrap">
            <table class="kdp-table">
              <thead><tr><th v-for="(h, i) in csvRows[0]" :key="i">{{ h || '（空表头）' }}</th></tr></thead>
              <tbody>
                <tr v-for="(row, ri) in csvRows.slice(1)" :key="ri">
                  <td v-for="(cell, ci) in row" :key="ci">{{ cell }}</td>
                </tr>
              </tbody>
            </table>
            <p v-if="csvTruncated" class="kdp-note">仅预览前 {{ CSV_PREVIEW_ROWS }} 行；完整内容请下载原件，或切换到 Markdown 页签查看解析结果。</p>
          </div>
          <article v-else-if="fileKind === 'markdown' && fileText" class="kdp-markdown" v-html="fileMarkdownHtml"></article>
          <pre v-else-if="fileKind === 'text' || fileKind === 'json' || fileKind === 'html'" class="kdp-text">{{ fileText }}</pre>
          <div v-else class="kdp-state">
            <strong>该格式不支持内嵌预览</strong>
            <span>可下载原件后用本地应用打开；Markdown 与 Chunks 页签可直接查看解析内容。</span>
            <button class="primary-btn" type="button" :disabled="downloading" @click="downloadOriginal">下载原件</button>
          </div>
        </section>

        <section v-show="activeTab === 'markdown'" class="kdp-pane" role="tabpanel" aria-label="Markdown 预览">
          <div v-if="markdownLoading" class="kdp-state" role="status">
            <strong>正在加载 Markdown</strong><span>解析文本较大时需要几秒钟。</span>
          </div>
          <div v-else-if="markdownHtml" class="kdp-md-wrap">
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
          </div>
        </section>

        <section v-show="activeTab === 'chunks'" class="kdp-pane" role="tabpanel" aria-label="切片列表">
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
  width: min(980px, 100%); height: min(760px, calc(100vh - 44px));
  display: flex; flex-direction: column; overflow: hidden;
  background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; box-shadow: var(--shadow-lg);
}
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
.secondary-btn, .primary-btn, .icon-btn {
  height: 32px; border: 1px solid var(--border); border-radius: 6px; cursor: pointer; font: inherit; font-size: 12px;
}
.secondary-btn { padding: 0 13px; background: var(--bg-card); color: var(--text-regular); }
.secondary-btn:hover, .icon-btn:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-light); }
.primary-btn { padding: 0 13px; border-color: var(--primary); background: var(--primary); color: #fff; }
.primary-btn:hover { background: var(--primary-hover); }
.icon-btn { width: 32px; padding: 0; background: var(--bg-card); color: var(--text-regular); font-size: 17px; }
button:disabled { cursor: not-allowed; opacity: .55; }
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
.kdp-state .primary-btn { margin-top: 6px; }
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
.kdp-toc-item.lv4, .kdp-toc-item.lv5, .kdp-toc-item.lv6 { padding-left: 26px; }
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
