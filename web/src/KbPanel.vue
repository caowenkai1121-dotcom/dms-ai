<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue'
import KbDocPreview from './KbDocPreview.vue'
import KbEval from './KbEval.vue'
import KbGraph from './KbGraph.vue'
import KbMindmap from './KbMindmap.vue'

interface DsTable { sheet: string; table: string; rows: number }
interface Ds { ds_id: string; schema: string; tables: DsTable[]; skipped: string[] }
interface Doc {
  doc_id: string; name: string; mime: string; bytes: number
  status: string; error?: string | null; notice?: string | null
  enabled?: boolean
  page_count?: number | null; chunk_count?: number | null
  uploaded_by?: string; created_at?: string; updated_at?: string
  tags?: string[]; business_domain?: string | null
  effective_from?: string | null; effective_to?: string | null; source_uri?: string | null
  document_family?: string | null; document_revision?: string | null
  folder_id?: string | null; folder_path?: string | null; directory_path?: string | null
  description?: string | null
  quality?: { level: string; label: string } | null
  datasource?: Ds | null
}
interface DocRelation {
  doc_id: string; doc_name: string
  folder_id?: string | null; folder_path?: string | null
  document_family?: string | null; document_revision?: string | null
  relation: string
}
interface Folder {
  folder_id: string; name: string; parent_id?: string | null
  path?: string | null; doc_count?: number; children?: Folder[]
}
interface FolderRow { folder: Folder; depth: number }
interface Space {
  space_id: string; name: string; owner: string; visibility: string
  writable: boolean; doc_count: number
}
interface UploadRow {
  id: number; name: string
  state: 'doing' | 'ok' | 'partial' | 'fail'
  msg: string; destination?: string; ds?: Ds | null
}

// 上传支持清单：与服务端 `ingest::EXTS`（23 项）同口径——这里只做选择器过滤与提前反馈，
// 权威判定永远在服务端 `classify`（两份清单漂移时服务端仍是闸）。
const UPLOAD_ACCEPT = [
  '.txt', '.md', '.markdown', '.csv', '.json', '.log', '.html', '.pdf',
  '.docx', '.doc', '.pptx', '.ppt', '.xlsx', '.xls', '.xlsm',
  '.png', '.jpg', '.jpeg', '.webp', '.gif', '.bmp', '.tif', '.tiff',
].join(',')
// 单文件上限 20MB：与服务端 `kb_max_mb` 默认值同口径（服务端配置可调，前端按产品口径预校验）
const MAX_UPLOAD_BYTES = 20 * 1024 * 1024
// 扩展名集合（与 UPLOAD_ACCEPT 同一份清单）：文件夹选择器不认 accept，前端按它逐个预过滤
const UPLOAD_EXTS = new Set(UPLOAD_ACCEPT.split(',').map((ext) => ext.trim().toLowerCase()))
interface Grant {
  grantee_kind: 'login' | 'role'; grantee: string; grantee_name?: string | null
  perm: 'read' | 'write'
}
interface RoleOption { role_code: string; role_name: string }
type Filter = 'all' | 'ready' | 'processing' | 'attention' | 'disabled'
type WorkbenchTab = 'documents' | 'retrieval' | 'graph' | 'mindmap' | 'eval'
interface SearchHit {
  chunk_id: number; doc_id: string; doc_name: string
  heading_path?: string | string[] | null; page?: number | null
  span?: number; preview: string
  tags?: string[]; business_domain?: string | null
  effective_from?: string | null; effective_to?: string | null; source_uri?: string | null
  document_family?: string | null; document_revision?: string | null
  folder_id?: string | null; folder_path?: string | null; directory_path?: string | null
  source_hash?: string; doc_updated_at?: string
}

const props = defineProps<{ token?: string; login?: string; initialSpace?: string }>()
const emit = defineEmits<{
  (e: 'close'): void
  (e: 'auth-expired'): void
  (e: 'space-change', value: { space_id: string; name: string }): void
}>()

const docs = ref<Doc[]>([])
const folders = ref<Folder[]>([])
const spaces = ref<Space[]>([])
const spaceId = ref('')
const selectedFolderId = ref('')
const uploadFolderId = ref('')
const folderApiAvailable = ref<boolean | null>(null)
const foldersLoading = ref(false)
const foldersErr = ref('')
const collapsedFolderIds = ref<string[]>([])
const folderCreateOpen = ref(false)
const folderCreating = ref(false)
const newFolderName = ref('')
const newFolderParentId = ref('')
const folderEditOpen = ref(false)
const folderEditing = ref(false)
const folderEditName = ref('')
const folderEditParentId = ref('')
const folderDialogErr = ref('')
const folderDeletingId = ref('')
const docMovingId = ref('')
// 【KB 管理闸】管理操作区（共享权限/新建空间等）的显隐依据：服务端 `/api/kb/spaces` 过闸才返
// `kb_manager:true`（配置授权的角色/人员 + 管理员）。隐藏只是体验，安全闸在服务端各管理端点。
const kbManager = ref(false)
const spacesErr = ref('')
const listErr = ref('')
const actionErr = ref('')
const loading = ref(false)
const busy = ref(false)
const uploads = ref<UploadRow[]>([])
const dragging = ref(false)
const search = ref('')
const filter = ref<Filter>('all')
const activeTab = ref<WorkbenchTab>('documents')
const retrievalQuestion = ref('')
const retrievalLoading = ref(false)
const retrievalErr = ref('')
const retrievalHits = ref<SearchHit[]>([])
const retrievalRan = ref(false)
const sampleQuestions = ref<string[]>([])
const vectorDegraded = ref(false)
const openedHit = ref<Record<number, string>>({})
const openingHit = ref<Record<number, boolean>>({})
const hitErr = ref<Record<number, string>>({})
const fileEl = ref<HTMLInputElement>()
const dirEl = ref<HTMLInputElement>()
const previewDoc = ref<Doc | null>(null)
const confirmDoc = ref<Doc | null>(null)
const deletingId = ref('')
const deleteDialogErr = ref('')
const reprocessingId = ref('')
const stateChangingId = ref('')
// 从 URL 添加（Y12）：与文件上传共用上传队列反馈与目标目录选择
const urlInput = ref('')
const urlBusy = ref(false)
// 生成描述（Y7）：逐文档 busy 闸，响应即整份 doc，直接写回行内展示
const descGeneratingId = ref('')
const createOpen = ref(false)
const creating = ref(false)
const newSpaceName = ref('')
const newSpaceId = ref('')
const grantOpen = ref(false)
const grants = ref<Grant[]>([])
const grantsLoading = ref(false)
const granting = ref(false)
const revokingGrant = ref('')
const grantKind = ref<'login' | 'role'>('login')
const grantTarget = ref('')
const grantPerm = ref<'read' | 'write'>('read')
const roleOptions = ref<RoleOption[]>([])
const roleSearch = ref('')
const selectedRoleCodes = ref<string[]>([])
const grantBatchLimit = ref(100)
const grantFeedback = ref('')
const grantFeedbackError = ref(false)
const metadataDoc = ref<Doc | null>(null)
const metadataSaving = ref(false)
const metadataErr = ref('')
const metadataTags = ref('')
const metadataDomain = ref('')
const metadataEffectiveFrom = ref('')
const metadataEffectiveTo = ref('')
const metadataSourceUri = ref('')
const metadataFamily = ref('')
const metadataRevision = ref('')
const metadataLoading = ref(false)
const metadataRelationReady = ref(false)
const metadataRelations = ref<DocRelation[]>([])
const metadataRelatedIds = ref<string[]>([])
const metadataRelationSearch = ref('')
let uploadId = 0
let contextEpoch = 0
let spacesRequestId = 0
let assetsRequestId = 0
let retrievalRequestId = 0
let metadataRequestId = 0
let grantsRequestId = 0
let uploadRequestId = 0

const OK = 'embedded'
const PARTIAL = 'chunked'

function headers(): Record<string, string> {
  const token = props.token?.trim()
  if (!token) {
    emit('auth-expired')
    throw new Error('登录会话已失效，请重新登录。')
  }
  return { Authorization: `Bearer ${token}` }
}
async function responseJson(response: Response): Promise<Record<string, any>> {
  const data = await response.json().catch(() => ({}))
  if (response.status === 401) emit('auth-expired')
  return data
}
function spaceQuery(space: string): string {
  const params = new URLSearchParams()
  if (space) params.set('space_id', space)
  const suffix = params.toString()
  return suffix ? `?${suffix}` : ''
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

function contextIsCurrent(epoch: number, space: string): boolean {
  return epoch === contextEpoch && space === spaceId.value
}

function folderTreePath(folderId: string): string {
  const byId = new Map(folders.value.map((folder) => [folder.folder_id, folder]))
  const names: string[] = []
  const seen = new Set<string>()
  let current = byId.get(folderId)
  while (current && !seen.has(current.folder_id)) {
    seen.add(current.folder_id)
    names.unshift(current.name)
    current = current.parent_id ? byId.get(current.parent_id) : undefined
  }
  return names.join('/')
}

function folderPath(value: { folder_id?: string | null; folder_path?: string | null; directory_path?: string | null }): string {
  const explicit = String(value.folder_path || value.directory_path || '').trim().replace(/^\/+|\/+$/g, '')
  if (explicit) return explicit
  const folderId = String(value.folder_id || '').trim()
  return folderId ? folderTreePath(folderId) : ''
}

function normalizeFolders(input: unknown, parentId: string | null = null, parentPath = ''): Folder[] {
  if (!Array.isArray(input)) return []
  const result: Folder[] = []
  for (const raw of input) {
    if (!raw || typeof raw !== 'object') continue
    const item = raw as Record<string, unknown>
    const folderId = String(item.folder_id ?? item.id ?? '').trim()
    const name = String(item.name ?? '').trim()
    if (!folderId || !name) continue
    const path = String(item.path ?? '').trim() || [parentPath, name].filter(Boolean).join('/')
    const folder: Folder = {
      folder_id: folderId,
      name,
      parent_id: item.parent_id == null ? parentId : String(item.parent_id),
      path,
      doc_count: typeof item.doc_count === 'number' ? item.doc_count : undefined,
    }
    result.push(folder)
    result.push(...normalizeFolders(item.children, folderId, path))
  }
  return result
}

function stateOf(status?: string): 'ready' | 'processing' | 'partial' | 'failed' | 'unknown' {
  if (status === OK) return 'ready'
  if (status === PARTIAL) return 'partial'
  if (status === 'pending' || status === 'parsing') return 'processing'
  if (status === 'failed') return 'failed'
  return 'unknown'
}
function statusText(status?: string): string {
  switch (status) {
    case OK: return '可检索'
    case PARTIAL: return '待补向量'
    case 'pending': return '等待处理'
    case 'parsing': return '解析中'
    case 'failed': return '处理失败'
    default: return '状态未知'
  }
}
function docStatusText(d: Doc): string {
  return d.enabled === false ? '已停用' : statusText(d.status)
}
function statusHint(d: Doc): string {
  if (d.enabled === false) return '不参与知识检索，原文件与索引仍保留'
  if (d.error) return d.error
  if (d.notice) return d.notice
  if (d.status === PARTIAL) return '文本已入库，向量索引尚未完成'
  if (d.status === 'pending' || d.status === 'parsing') return '处理完成后即可参与问答'
  if (d.status === OK) return '解析与向量索引均已完成'
  return `服务端状态：${d.status || '空'}`
}
function uploadState(status?: string): UploadRow['state'] {
  if (status === OK) return 'ok'
  if (status === PARTIAL) return 'partial'
  return 'fail'
}
function updateUpload(id: number, patch: Partial<UploadRow>) {
  const row = uploads.value.find((item) => item.id === id)
  if (row) Object.assign(row, patch)
}
function sizeText(n?: number | null): string {
  if (typeof n !== 'number') return '-'
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`
  return `${(n / 1024 / 1024).toFixed(1)} MB`
}
function extOf(name: string): string {
  const ext = name.split('.').pop()
  return ext && ext !== name ? ext.toUpperCase().slice(0, 5) : 'FILE'
}
function typeText(d: Doc): string {
  const ext = extOf(d.name)
  const groups: Record<string, string> = {
    DOC: 'Word', DOCX: 'Word', XLS: 'Excel', XLSX: 'Excel', CSV: 'CSV',
    PPT: 'PPT', PPTX: 'PPT', PDF: 'PDF', TXT: '文本', MD: 'Markdown',
    HTML: '网页', HTM: '网页', JSON: 'JSON', XML: 'XML',
  }
  return groups[ext] ?? ext
}
function dateText(value?: string): string {
  if (!value) return '-'
  const d = new Date(value)
  if (Number.isNaN(d.getTime())) return value.slice(0, 16).replace('T', ' ')
  return new Intl.DateTimeFormat('zh-CN', {
    month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false,
  }).format(d)
}
function hitLocation(hit: SearchHit): string {
  const parts: string[] = []
  const directory = folderPath(hit)
  if (directory) parts.push(`目录 ${directory}`)
  const heading = Array.isArray(hit.heading_path)
    ? hit.heading_path.filter(Boolean).join(' / ')
    : String(hit.heading_path ?? '').trim()
  if (heading) parts.push(heading)
  if (typeof hit.page === 'number') parts.push(`第 ${hit.page} 页`)
  return parts.join(' · ') || '未标注章节或页码'
}
function qualityClass(level?: string): string {
  return ['danger', 'warning', 'good', 'processing'].includes(level ?? '') ? (level ?? '') : ''
}
function displayState(d: Doc): 'ready' | 'processing' | 'attention' | 'disabled' {
  if (d.enabled === false) return 'disabled'
  if (d.quality?.level === 'processing') return 'processing'
  if (d.quality?.level === 'good') return 'ready'
  return 'attention'
}
function displayStatusText(d: Doc): string {
  return d.quality?.label || docStatusText(d)
}
function dateInputValue(value?: string | null): string {
  return value ? value.slice(0, 10) : ''
}
function effectiveText(d: Doc): string {
  if (d.effective_from && d.effective_to) return `${dateInputValue(d.effective_from)} 至 ${dateInputValue(d.effective_to)}`
  if (d.effective_from) return `${dateInputValue(d.effective_from)} 起`
  if (d.effective_to) return `有效至 ${dateInputValue(d.effective_to)}`
  return ''
}
function resetRetrieval() {
  retrievalErr.value = ''
  retrievalHits.value = []
  retrievalRan.value = false
  vectorDegraded.value = false
  openedHit.value = {}
  openingHit.value = {}
  hitErr.value = {}
}

function governanceText(hit: SearchHit): string {
  if (hit.effective_from && hit.effective_to) return `${hit.effective_from} 至 ${hit.effective_to}`
  if (hit.effective_from) return `${hit.effective_from} 起生效`
  if (hit.effective_to) return `有效至 ${hit.effective_to}`
  return '未设置生效期'
}

function versionText(hit: SearchHit): string {
  const date = hit.doc_updated_at ? dateText(hit.doc_updated_at) : '未知时间'
  return [hit.document_family, hit.document_revision, `更新 ${date}`].filter(Boolean).join(' · ')
}

function safeSourceUri(value?: string | null): string {
  const uri = String(value ?? '').trim()
  return /^https?:\/\//i.test(uri) ? uri : ''
}

async function downloadDoc(docId: string, name: string) {
  const requestEpoch = contextEpoch
  const requestSpace = spaceId.value
  try {
    const response = await fetch(`/api/kb/doc/${encodeURIComponent(docId)}/download`, { headers: headers() })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    const blob = await response.blob()
    const url = URL.createObjectURL(blob)
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.download = name || 'knowledge-file'
    document.body.appendChild(anchor)
    anchor.click()
    anchor.remove()
    window.setTimeout(() => URL.revokeObjectURL(url), 0)
  } catch (e) {
    if (contextIsCurrent(requestEpoch, requestSpace)) actionErr.value = `下载原件失败：${errorText(e)}`
  }
}

async function toggleHit(hit: SearchHit) {
  if (openedHit.value[hit.chunk_id]) {
    delete openedHit.value[hit.chunk_id]
    return
  }
  if (openingHit.value[hit.chunk_id]) return
  openingHit.value[hit.chunk_id] = true
  delete hitErr.value[hit.chunk_id]
  const requestEpoch = contextEpoch
  const requestSpace = spaceId.value
  const requestDoc = hit.doc_id
  const requestRetrieval = retrievalRequestId
  const params = new URLSearchParams()
  if ((hit.span ?? 1) > 1) params.set('span', String(hit.span))
  else params.set('window', '1')
  if (hit.source_hash) params.set('source_hash', hit.source_hash)
  if (hit.doc_updated_at) params.set('doc_updated_at', hit.doc_updated_at)
  try {
    const response = await fetch(`/api/kb/chunk/${hit.chunk_id}?${params}`, { headers: headers() })
    const data = await responseJson(response)
    if (requestRetrieval !== retrievalRequestId || !contextIsCurrent(requestEpoch, requestSpace)
      || !retrievalHits.value.some((item) => item.chunk_id === hit.chunk_id && item.doc_id === requestDoc)) return
    if (response.status === 409) {
      hitErr.value[hit.chunk_id] = data.error ?? '来源文档已更新，本条命中已失效，请重新检索。'
      return
    }
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    openedHit.value[hit.chunk_id] = String(data.text ?? '') || '没有可显示的原文。'
  } catch (e) {
    if (requestRetrieval === retrievalRequestId && contextIsCurrent(requestEpoch, requestSpace)) {
      hitErr.value[hit.chunk_id] = errorText(e)
    }
  } finally {
    if (requestRetrieval === retrievalRequestId && contextIsCurrent(requestEpoch, requestSpace)) {
      openingHit.value[hit.chunk_id] = false
    }
  }
}

function relationText(code: string): string {
  return ({
    references: '已关联', referenced_by: '被其他文档关联',
    same_folder: '同文件夹', ancestor_folder: '上级文件夹', descendant_folder: '下级文件夹',
    document_family: '同文档族', document_revision: '同版本',
    same_domain: '同业务域', shared_tag: '共享标签', explicit_link: '已关联',
  } as Record<string, string>)[code] ?? '内容相关'
}

async function openMetadata(d: Doc) {
  const requestId = ++metadataRequestId
  const requestEpoch = contextEpoch
  const requestSpace = spaceId.value
  metadataDoc.value = d
  metadataTags.value = (d.tags ?? []).join(', ')
  metadataDomain.value = d.business_domain ?? ''
  metadataEffectiveFrom.value = dateInputValue(d.effective_from)
  metadataEffectiveTo.value = dateInputValue(d.effective_to)
  metadataSourceUri.value = d.source_uri ?? ''
  metadataFamily.value = d.document_family ?? ''
  metadataRevision.value = d.document_revision ?? ''
  metadataLoading.value = true
  metadataRelationReady.value = false
  metadataRelations.value = []
  metadataRelatedIds.value = []
  metadataRelationSearch.value = ''
  metadataErr.value = ''
  try {
    const response = await fetch(`/api/kb/doc/${encodeURIComponent(d.doc_id)}`, { headers: headers() })
    const data = await responseJson(response)
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    if (requestId !== metadataRequestId || !contextIsCurrent(requestEpoch, requestSpace)
      || metadataDoc.value?.doc_id !== d.doc_id) return
    metadataRelations.value = Array.isArray(data.related_documents) ? data.related_documents : []
    metadataRelatedIds.value = metadataRelations.value
      .filter((relation) => relation.relation === 'references' || relation.relation === 'explicit_link')
      .map((relation) => relation.doc_id)
    metadataRelationReady.value = true
  } catch (e) {
    if (requestId === metadataRequestId && contextIsCurrent(requestEpoch, requestSpace)
      && metadataDoc.value?.doc_id === d.doc_id) metadataErr.value = `关联信息加载失败：${errorText(e)}`
  } finally {
    if (requestId === metadataRequestId && contextIsCurrent(requestEpoch, requestSpace)
      && metadataDoc.value?.doc_id === d.doc_id) metadataLoading.value = false
  }
}

const metadataRelatedSet = computed(() => new Set(metadataRelatedIds.value))
const metadataCandidateDocs = computed(() => {
  const currentId = metadataDoc.value?.doc_id
  const needle = metadataRelationSearch.value.trim().toLocaleLowerCase()
  return docs.value
    .filter((doc) => doc.doc_id !== currentId)
    .filter((doc) => !needle || [doc.name, folderPath(doc), doc.document_family, doc.document_revision, ...(doc.tags ?? [])]
      .some((value) => String(value ?? '').toLocaleLowerCase().includes(needle)))
    .sort((a, b) => Number(metadataRelatedSet.value.has(b.doc_id)) - Number(metadataRelatedSet.value.has(a.doc_id))
      || a.name.localeCompare(b.name, 'zh-CN'))
})
const inferredRelations = computed(() => metadataRelations.value.filter((relation) =>
  relation.relation !== 'references' && relation.relation !== 'explicit_link'))

function toggleRelatedDoc(docId: string) {
  if (metadataRelatedSet.value.has(docId)) {
    metadataRelatedIds.value = metadataRelatedIds.value.filter((id) => id !== docId)
  } else if (metadataRelatedIds.value.length < 50) {
    metadataRelatedIds.value = [...metadataRelatedIds.value, docId]
  } else {
    metadataErr.value = '关联文档最多 50 篇'
  }
}

function closeMetadata(force = false) {
  if (metadataSaving.value && !force) return
  metadataRequestId++
  metadataDoc.value = null
  metadataLoading.value = false
  metadataSaving.value = false
  metadataRelationReady.value = false
  metadataRelations.value = []
  metadataRelatedIds.value = []
  metadataRelationSearch.value = ''
  metadataErr.value = ''
}

function openDeleteConfirm(d: Doc) {
  deleteDialogErr.value = ''
  confirmDoc.value = d
}

async function saveMetadata() {
  const d = metadataDoc.value
  if (!d || metadataSaving.value) return
  const requestId = ++metadataRequestId
  const requestEpoch = contextEpoch
  const requestSpace = spaceId.value
  metadataSaving.value = true
  metadataErr.value = ''
  try {
    const tags = [...new Set(metadataTags.value.split(/[,，]/).map((tag) => tag.trim()).filter(Boolean))]
    const body: Record<string, unknown> = {
      tags,
      business_domain: metadataDomain.value.trim() || null,
      effective_from: metadataEffectiveFrom.value || null,
      effective_to: metadataEffectiveTo.value || null,
      source_uri: metadataSourceUri.value.trim() || null,
      document_family: metadataFamily.value.trim() || null,
      document_revision: metadataRevision.value.trim() || null,
      related_doc_ids: metadataRelatedIds.value,
    }
    const response = await fetch(`/api/kb/doc/${encodeURIComponent(d.doc_id)}/metadata`, {
      method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    const data = await responseJson(response)
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    if (requestId !== metadataRequestId || !contextIsCurrent(requestEpoch, requestSpace)
      || metadataDoc.value?.doc_id !== d.doc_id) return
    closeMetadata(true)
    await loadSpaces(requestSpace)
  } catch (e) {
    if (requestId === metadataRequestId && contextIsCurrent(requestEpoch, requestSpace)
      && metadataDoc.value?.doc_id === d.doc_id) metadataErr.value = `保存元数据失败：${errorText(e)}`
  } finally {
    if (requestId === metadataRequestId && contextIsCurrent(requestEpoch, requestSpace)) metadataSaving.value = false
  }
}

const currentSpace = computed(() => spaces.value.find((space) => space.space_id === spaceId.value) ?? null)
const switchingDisabled = computed(() => busy.value || creating.value || folderCreating.value || folderEditing.value
  || !!folderDeletingId.value || !!docMovingId.value || metadataSaving.value || granting.value
  || !!revokingGrant.value || !!deletingId.value || !!reprocessingId.value || !!stateChangingId.value)
const folderRows = computed<FolderRow[]>(() => {
  const byParent = new Map<string, Folder[]>()
  for (const folder of folders.value) {
    const key = folder.parent_id || ''
    byParent.set(key, [...(byParent.get(key) ?? []), folder])
  }
  for (const children of byParent.values()) children.sort((a, b) => a.name.localeCompare(b.name, 'zh-CN'))
  const rows: FolderRow[] = []
  const seen = new Set<string>()
  const visit = (parent: string, depth: number) => {
    for (const folder of byParent.get(parent) ?? []) {
      if (seen.has(folder.folder_id)) continue
      seen.add(folder.folder_id)
      rows.push({ folder, depth })
      visit(folder.folder_id, depth + 1)
    }
  }
  visit('', 0)
  for (const folder of folders.value) {
    if (!seen.has(folder.folder_id)) rows.push({ folder, depth: 0 })
  }
  return rows
})
const folderChildren = computed(() => {
  const counts = new Map<string, number>()
  for (const folder of folders.value) {
    if (folder.parent_id) counts.set(folder.parent_id, (counts.get(folder.parent_id) ?? 0) + 1)
  }
  return counts
})
const visibleFolderRows = computed(() => {
  const collapsed = new Set(collapsedFolderIds.value)
  const byId = new Map(folders.value.map((folder) => [folder.folder_id, folder]))
  return folderRows.value.filter(({ folder }) => {
    const seen = new Set<string>()
    let parentId = folder.parent_id || ''
    while (parentId && !seen.has(parentId)) {
      if (collapsed.has(parentId)) return false
      seen.add(parentId)
      parentId = byId.get(parentId)?.parent_id || ''
    }
    return true
  })
})
const selectedFolder = computed(() => folders.value.find((folder) => folder.folder_id === selectedFolderId.value) ?? null)
const uploadFolder = computed(() => folders.value.find((folder) => folder.folder_id === uploadFolderId.value) ?? null)
const selectedFolderTrail = computed(() => {
  const byId = new Map(folders.value.map((folder) => [folder.folder_id, folder]))
  const trail: Folder[] = []
  const seen = new Set<string>()
  let current: Folder | undefined = selectedFolder.value ?? undefined
  while (current && !seen.has(current.folder_id)) {
    seen.add(current.folder_id)
    trail.unshift(current)
    current = current.parent_id ? byId.get(current.parent_id) : undefined
  }
  return trail
})
const selectedFolderName = computed(() => {
  if (selectedFolderId.value === '__unfiled__') return '未分类'
  return selectedFolder.value ? folderLabel(selectedFolder.value) : '全部文档'
})
const folderCounts = computed(() => {
  const counts = new Map<string, number>()
  for (const doc of docs.value) {
    if (doc.folder_id) counts.set(doc.folder_id, (counts.get(doc.folder_id) ?? 0) + 1)
  }
  return counts
})
function selectFolder(folderId: string) {
  selectedFolderId.value = folderId
  uploadFolderId.value = folderId === '__unfiled__' ? '' : folderId
}
function toggleFolder(folderId: string) {
  collapsedFolderIds.value = collapsedFolderIds.value.includes(folderId)
    ? collapsedFolderIds.value.filter((id) => id !== folderId)
    : [...collapsedFolderIds.value, folderId]
}
function folderLabel(folder: Folder): string {
  const derived = folderTreePath(folder.folder_id)
  const explicit = String(folder.path || '').trim().replace(/^\/+|\/+$/g, '')
  if (explicit.includes('/') || !folder.parent_id) return explicit || derived || folder.name
  return derived || explicit || folder.name
}
const filteredRoleOptions = computed(() => {
  const needle = roleSearch.value.trim().toLocaleLowerCase()
  if (!needle) return roleOptions.value
  return roleOptions.value.filter((role) =>
    role.role_code.toLocaleLowerCase().includes(needle)
      || role.role_name.toLocaleLowerCase().includes(needle))
})
const selectedRoleSet = computed(() => new Set(selectedRoleCodes.value))
const selectedRoleOptions = computed(() => selectedRoleCodes.value.map((code) => {
  const role = roleOptions.value.find((item) => item.role_code === code)
  return role ?? { role_code: code, role_name: code }
}))
const allFilteredRolesSelected = computed(() => filteredRoleOptions.value.length > 0
  && filteredRoleOptions.value.every((role) => selectedRoleSet.value.has(role.role_code)))

function toggleRole(code: string) {
  if (selectedRoleSet.value.has(code)) {
    selectedRoleCodes.value = selectedRoleCodes.value.filter((item) => item !== code)
    grantFeedback.value = ''
    grantFeedbackError.value = false
    return
  }
  if (selectedRoleCodes.value.length >= grantBatchLimit.value) {
    grantFeedbackError.value = true
    grantFeedback.value = `一次最多选择 ${grantBatchLimit.value} 个角色`
    return
  }
  selectedRoleCodes.value = [...selectedRoleCodes.value, code]
  grantFeedback.value = ''
  grantFeedbackError.value = false
}
function selectFilteredRoles() {
  const next = [...selectedRoleCodes.value]
  const selected = new Set(next)
  for (const role of filteredRoleOptions.value) {
    if (selected.has(role.role_code) || next.length >= grantBatchLimit.value) continue
    selected.add(role.role_code)
    next.push(role.role_code)
  }
  selectedRoleCodes.value = next
  const skipped = filteredRoleOptions.value.filter((role) => !selected.has(role.role_code)).length
  if (skipped) {
    grantFeedbackError.value = true
    grantFeedback.value = `已达到 ${grantBatchLimit.value} 个角色上限，当前结果还有 ${skipped} 个未选择`
  } else {
    grantFeedback.value = ''
    grantFeedbackError.value = false
  }
}
function toggleFilteredRoles() {
  if (!allFilteredRolesSelected.value) {
    selectFilteredRoles()
    return
  }
  const filtered = new Set(filteredRoleOptions.value.map((role) => role.role_code))
  selectedRoleCodes.value = selectedRoleCodes.value.filter((code) => !filtered.has(code))
  grantFeedback.value = ''
  grantFeedbackError.value = false
}
function clearSelectedRoles() {
  selectedRoleCodes.value = []
  grantFeedback.value = ''
  grantFeedbackError.value = false
}
function resetGrantDraft() {
  grantKind.value = 'login'
  grantTarget.value = ''
  grantPerm.value = 'read'
  roleSearch.value = ''
  selectedRoleCodes.value = []
  grantFeedback.value = ''
  grantFeedbackError.value = false
}
function closeSpaceCreate() {
  if (!creating.value) createOpen.value = false
}
function closeFolderCreate() {
  if (folderCreating.value) return
  folderCreateOpen.value = false
  folderDialogErr.value = ''
}
function closeFolderEdit() {
  if (folderEditing.value) return
  folderEditOpen.value = false
  folderDialogErr.value = ''
}
function closeDeleteConfirm() {
  if (deletingId.value) return
  confirmDoc.value = null
  deleteDialogErr.value = ''
}
function closePanel() {
  if (!switchingDisabled.value) emit('close')
}
function closeGrants(force = false) {
  if (!force && (granting.value || !!revokingGrant.value)) return
  grantsRequestId++
  grantOpen.value = false
  grantsLoading.value = false
  granting.value = false
  revokingGrant.value = ''
  grants.value = []
  roleOptions.value = []
  grantBatchLimit.value = 100
  resetGrantDraft()
}
function grantName(g: Grant): string {
  if (g.grantee_kind !== 'role') return g.grantee
  const name = g.grantee_name || roleOptions.value.find((role) => role.role_code === g.grantee)?.role_name
  return name ? `${name} · ${g.grantee}` : g.grantee
}

const counts = computed(() => ({
  all: docs.value.length,
  ready: docs.value.filter((d) => displayState(d) === 'ready').length,
  processing: docs.value.filter((d) => displayState(d) === 'processing').length,
  attention: docs.value.filter((d) => displayState(d) === 'attention').length,
  disabled: docs.value.filter((d) => displayState(d) === 'disabled').length,
}))
const filters = computed<{ value: Filter; label: string; count: number }[]>(() => [
  { value: 'all', label: '全部', count: counts.value.all },
  { value: 'ready', label: '可检索', count: counts.value.ready },
  { value: 'processing', label: '处理中', count: counts.value.processing },
  { value: 'attention', label: '需处理', count: counts.value.attention },
  { value: 'disabled', label: '已停用', count: counts.value.disabled },
])
const visibleDocs = computed(() => {
  const needle = search.value.trim().toLocaleLowerCase()
  return docs.value.filter((d) => {
    if (selectedFolderId.value === '__unfiled__' && d.folder_id) return false
    if (selectedFolderId.value && selectedFolderId.value !== '__unfiled__' && d.folder_id !== selectedFolderId.value) return false
    const state = displayState(d)
    const inFilter = filter.value === 'all'
      || filter.value === state
    if (!inFilter) return false
    if (!needle) return true
    return [d.name, d.mime, d.status, d.error, d.notice, d.uploaded_by,
      d.business_domain, d.source_uri, d.document_family, d.document_revision, folderPath(d), ...(d.tags ?? [])]
      .some((v) => String(v ?? '').toLocaleLowerCase().includes(needle))
  })
})
async function load(space: string, requestId: number, epoch: number): Promise<boolean | null> {
  if (!space) return null
  if (contextIsCurrent(epoch, space)) {
    loading.value = true
    listErr.value = ''
  }
  try {
    const resp = await fetch(`/api/kb/docs${spaceQuery(space)}`, { headers: headers() })
    const data = await responseJson(resp)
    if (!resp.ok) throw new Error(data.error ?? `HTTP ${resp.status}`)
    if (requestId !== assetsRequestId || !contextIsCurrent(epoch, space)) return null
    docs.value = data.docs ?? []
    const hasEmbeddedFolders = Array.isArray(data.folders)
    const embeddedFolders = normalizeFolders(data.folders)
    if (hasEmbeddedFolders) {
      folders.value = embeddedFolders
      folderApiAvailable.value = true
      foldersErr.value = ''
      const available = new Set(folders.value.map((folder) => folder.folder_id))
      if (selectedFolderId.value && selectedFolderId.value !== '__unfiled__' && !available.has(selectedFolderId.value)) selectedFolderId.value = ''
      if (uploadFolderId.value && !available.has(uploadFolderId.value)) uploadFolderId.value = ''
    }
    return hasEmbeddedFolders
  } catch (e) {
    if (requestId !== assetsRequestId || !contextIsCurrent(epoch, space)) return null
    listErr.value = errorText(e)
    docs.value = []
    return null
  } finally {
    if (requestId === assetsRequestId && contextIsCurrent(epoch, space)) loading.value = false
  }
}

async function loadFolders(
  space = spaceId.value,
  existingRequestId?: number,
  epoch = contextEpoch,
) {
  if (!space) return
  const requestId = existingRequestId ?? ++assetsRequestId
  if (existingRequestId == null && contextIsCurrent(epoch, space)) loading.value = false
  if (contextIsCurrent(epoch, space)) {
    foldersLoading.value = true
    foldersErr.value = ''
  }
  try {
    const response = await fetch(`/api/kb/folders${spaceQuery(space)}`, { headers: headers() })
    const data = await responseJson(response)
    if (requestId !== assetsRequestId || !contextIsCurrent(epoch, space)) return
    if (response.status === 404 || response.status === 405) {
      folderApiAvailable.value = false
      folders.value = []
      if (selectedFolderId.value !== '__unfiled__') selectedFolderId.value = ''
      uploadFolderId.value = ''
      return
    }
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    folders.value = normalizeFolders(data.folders ?? data.items ?? data)
    folderApiAvailable.value = true
    const available = new Set(folders.value.map((folder) => folder.folder_id))
    if (selectedFolderId.value && selectedFolderId.value !== '__unfiled__' && !available.has(selectedFolderId.value)) selectedFolderId.value = ''
    if (uploadFolderId.value && !available.has(uploadFolderId.value)) uploadFolderId.value = ''
  } catch (e) {
    if (requestId !== assetsRequestId || !contextIsCurrent(epoch, space)) return
    folderApiAvailable.value = null
    foldersErr.value = errorText(e)
  } finally {
    if (requestId === assetsRequestId && contextIsCurrent(epoch, space)) foldersLoading.value = false
  }
}

async function loadKnowledgeAssets(space = spaceId.value, epoch = contextEpoch) {
  if (!space || !contextIsCurrent(epoch, space)) return
  const requestId = ++assetsRequestId
  foldersLoading.value = false
  const foldersEmbeddedInDocs = await load(space, requestId, epoch)
  if (foldersEmbeddedInDocs === false) await loadFolders(space, requestId, epoch)
}

async function loadSpaces(preferred?: string) {
  const requestId = ++spacesRequestId
  assetsRequestId++
  loading.value = false
  foldersLoading.value = false
  spacesErr.value = ''
  try {
    const response = await fetch('/api/kb/spaces', { headers: headers() })
    const data = await responseJson(response)
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    if (requestId !== spacesRequestId) return
    spaces.value = data.spaces ?? []
    kbManager.value = data.kb_manager === true
    const next = preferred && spaces.value.some((space) => space.space_id === preferred)
      ? preferred
      : spaces.value.some((space) => space.space_id === spaceId.value)
        ? spaceId.value
        : spaces.value.some((space) => space.space_id === props.login)
          ? (props.login ?? '')
          : (spaces.value[0]?.space_id ?? '')
    const changed = next !== spaceId.value
    if (changed) {
      contextEpoch++
      assetsRequestId++
      retrievalRequestId++
      metadataRequestId++
      grantsRequestId++
      uploadRequestId++
      spaceId.value = next
      folders.value = []
      docs.value = []
      uploads.value = []
      busy.value = false
      collapsedFolderIds.value = []
      foldersErr.value = ''
      folderApiAvailable.value = null
      selectedFolderId.value = ''
      uploadFolderId.value = ''
      folderCreateOpen.value = false
      folderEditOpen.value = false
      previewDoc.value = null
      confirmDoc.value = null
      deleteDialogErr.value = ''
      folderDialogErr.value = ''
      folderCreating.value = false
      folderEditing.value = false
      folderDeletingId.value = ''
      docMovingId.value = ''
      reprocessingId.value = ''
      stateChangingId.value = ''
      deletingId.value = ''
      newFolderName.value = ''
      newFolderParentId.value = ''
      resetRetrieval()
      retrievalLoading.value = false
      sampleQuestions.value = []
      closeMetadata(true)
      closeGrants(true)
    }
    const requestEpoch = contextEpoch
    await loadKnowledgeAssets(next, requestEpoch)
    void loadSampleQuestions(next, requestEpoch)
    if (requestId === spacesRequestId && contextIsCurrent(requestEpoch, next) && currentSpace.value) {
      emit('space-change', { space_id: currentSpace.value.space_id, name: currentSpace.value.name })
    }
  } catch (e) {
    if (requestId !== spacesRequestId) return
    contextEpoch++
    assetsRequestId++
    retrievalRequestId++
    metadataRequestId++
    grantsRequestId++
    uploadRequestId++
    spacesErr.value = errorText(e)
    spaces.value = []
    kbManager.value = false
    spaceId.value = ''
    docs.value = []
    folders.value = []
    uploads.value = []
    selectedFolderId.value = ''
    uploadFolderId.value = ''
    busy.value = false
    loading.value = false
    foldersLoading.value = false
    retrievalLoading.value = false
    folderCreating.value = false
    folderEditing.value = false
    folderDeletingId.value = ''
    docMovingId.value = ''
    reprocessingId.value = ''
    stateChangingId.value = ''
    deletingId.value = ''
    resetRetrieval()
    sampleQuestions.value = []
    closeMetadata(true)
    closeGrants(true)
  }
}

async function changeSpace() {
  const requestSpace = spaceId.value
  spacesRequestId++
  contextEpoch++
  assetsRequestId++
  retrievalRequestId++
  metadataRequestId++
  grantsRequestId++
  uploadRequestId++
  const requestEpoch = contextEpoch
  loading.value = false
  foldersLoading.value = false
  search.value = ''
  filter.value = 'all'
  uploads.value = []
  actionErr.value = ''
  folders.value = []
  collapsedFolderIds.value = []
  foldersErr.value = ''
  folderApiAvailable.value = null
  selectedFolderId.value = ''
  uploadFolderId.value = ''
  folderCreateOpen.value = false
  folderEditOpen.value = false
  previewDoc.value = null
  confirmDoc.value = null
  deleteDialogErr.value = ''
  folderDialogErr.value = ''
  folderCreating.value = false
  folderEditing.value = false
  folderDeletingId.value = ''
  docMovingId.value = ''
  reprocessingId.value = ''
  stateChangingId.value = ''
  deletingId.value = ''
  newFolderName.value = ''
  newFolderParentId.value = ''
  closeGrants(true)
  closeMetadata(true)
  resetRetrieval()
  retrievalLoading.value = false
  sampleQuestions.value = []
  await loadKnowledgeAssets(requestSpace, requestEpoch)
  void loadSampleQuestions(requestSpace, requestEpoch)
  if (contextIsCurrent(requestEpoch, requestSpace) && currentSpace.value) {
    emit('space-change', { space_id: currentSpace.value.space_id, name: currentSpace.value.name })
  }
}

async function runRetrieval() {
  const question = retrievalQuestion.value.trim()
  const requestSpace = spaceId.value
  if (!question || !requestSpace || retrievalLoading.value) return
  const requestId = ++retrievalRequestId
  const requestEpoch = contextEpoch
  retrievalLoading.value = true
  resetRetrieval()
  try {
    const response = await fetch('/api/kb/search', {
      method: 'POST',
      headers: { ...headers(), 'Content-Type': 'application/json' },
      body: JSON.stringify({ question, space_id: requestSpace }),
    })
    const data = await responseJson(response)
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    if (requestId !== retrievalRequestId || !contextIsCurrent(requestEpoch, requestSpace)) return
    retrievalHits.value = Array.isArray(data.hits) ? data.hits : []
    vectorDegraded.value = !!data.vector_degraded
    retrievalRan.value = true
  } catch (e) {
    if (requestId === retrievalRequestId && contextIsCurrent(requestEpoch, requestSpace)) retrievalErr.value = errorText(e)
  } finally {
    if (requestId === retrievalRequestId && contextIsCurrent(requestEpoch, requestSpace)) retrievalLoading.value = false
  }
}

// 样例问题只是检索测试的引导提示：接口缺席或失败都静默隐藏，不占位、不报错。
async function loadSampleQuestions(space: string, epoch: number) {
  if (!space || !contextIsCurrent(epoch, space)) return
  try {
    const response = await fetch(`/api/kb/sample-questions${spaceQuery(space)}`, { headers: headers() })
    if (!response.ok) {
      if (response.status === 401) emit('auth-expired')
      return
    }
    const data = await response.json().catch(() => ({}))
    if (!contextIsCurrent(epoch, space)) return
    const list = Array.isArray(data) ? data : (data.questions ?? data.samples ?? data.items ?? [])
    if (!Array.isArray(list)) return
    const seen = new Set<string>()
    const questions: string[] = []
    for (const item of list) {
      const text = String(typeof item === 'string' ? item : (item?.question ?? item?.text ?? item?.q ?? '')).trim()
      if (!text || seen.has(text)) continue
      seen.add(text)
      questions.push(text)
      if (questions.length >= 8) break
    }
    sampleQuestions.value = questions
  } catch { /* 静默隐藏 */ }
}

function askSample(question: string) {
  retrievalQuestion.value = question
  void runRetrieval()
}

async function createSpace() {
  const name = newSpaceName.value.trim()
  if (!name || creating.value) return
  creating.value = true
  actionErr.value = ''
  try {
    const body: Record<string, string> = { name }
    if (newSpaceId.value.trim()) body.space_id = newSpaceId.value.trim()
    const response = await fetch('/api/kb/spaces', {
      method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    const data = await responseJson(response)
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    createOpen.value = false
    newSpaceName.value = ''
    newSpaceId.value = ''
    await loadSpaces(data.space_id)
  } catch (e) {
    actionErr.value = `新建空间失败：${errorText(e)}`
  } finally {
    creating.value = false
  }
}

async function createFolder() {
  const name = newFolderName.value.trim()
  if (!name || folderCreating.value || !spaceId.value) return
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  folderCreating.value = true
  folderDialogErr.value = ''
  actionErr.value = ''
  try {
    const body: Record<string, unknown> = {
      space_id: requestSpace,
      name,
      parent_id: newFolderParentId.value || null,
    }
    const response = await fetch('/api/kb/folders', {
      method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    const data = await responseJson(response)
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    if (!contextIsCurrent(requestEpoch, requestSpace)) return
    folderApiAvailable.value = true
    folderCreateOpen.value = false
    folderDialogErr.value = ''
    newFolderName.value = ''
    newFolderParentId.value = ''
    await loadKnowledgeAssets(requestSpace, requestEpoch)
    if (!contextIsCurrent(requestEpoch, requestSpace)) return
    const createdId = String(data.folder_id ?? data.id ?? '')
    if (createdId && folders.value.some((folder) => folder.folder_id === createdId)) {
      selectedFolderId.value = createdId
      uploadFolderId.value = createdId
    }
  } catch (e) {
    if (contextIsCurrent(requestEpoch, requestSpace)) folderDialogErr.value = `新建文件夹失败：${errorText(e)}`
  } finally {
    if (contextIsCurrent(requestEpoch, requestSpace)) folderCreating.value = false
  }
}

function openFolderEdit() {
  if (!selectedFolder.value) return
  folderDialogErr.value = ''
  folderEditName.value = selectedFolder.value.name
  folderEditParentId.value = selectedFolder.value.parent_id || ''
  folderEditOpen.value = true
}

function folderIsDescendant(candidate: Folder, ancestorId: string): boolean {
  const byId = new Map(folders.value.map((folder) => [folder.folder_id, folder]))
  const seen = new Set<string>()
  let current: Folder | undefined = candidate
  while (current && !seen.has(current.folder_id)) {
    if (current.parent_id === ancestorId) return true
    seen.add(current.folder_id)
    current = current.parent_id ? byId.get(current.parent_id) : undefined
  }
  return false
}

const folderMoveTargets = computed(() => {
  const current = selectedFolder.value
  if (!current) return folderRows.value
  return folderRows.value.filter((row) => row.folder.folder_id !== current.folder_id
    && !folderIsDescendant(row.folder, current.folder_id))
})

async function saveFolderEdit() {
  const folder = selectedFolder.value
  const name = folderEditName.value.trim()
  if (!folder || !name || folderEditing.value) return
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  folderEditing.value = true
  folderDialogErr.value = ''
  actionErr.value = ''
  try {
    const body: Record<string, unknown> = {
      space_id: requestSpace, name, parent_id: folderEditParentId.value || null,
    }
    const response = await fetch(`/api/kb/folder/${encodeURIComponent(folder.folder_id)}`, {
      method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    const data = await responseJson(response)
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    if (!contextIsCurrent(requestEpoch, requestSpace)) return
    folderEditOpen.value = false
    folderDialogErr.value = ''
    await loadKnowledgeAssets(requestSpace, requestEpoch)
  } catch (e) {
    if (contextIsCurrent(requestEpoch, requestSpace)) folderDialogErr.value = `修改文件夹失败：${errorText(e)}`
  } finally {
    if (contextIsCurrent(requestEpoch, requestSpace)) folderEditing.value = false
  }
}

async function deleteSelectedFolder() {
  const folder = selectedFolder.value
  if (!folder || folderDeletingId.value) return
  const childCount = folderChildren.value.get(folder.folder_id) ?? 0
  const docCount = Math.max(folder.doc_count ?? 0, folderCounts.value.get(folder.folder_id) ?? 0)
  if (childCount || docCount) {
    actionErr.value = `无法删除“${folderLabel(folder)}”：目录中还有 ${childCount} 个子文件夹、${docCount} 份文档，请先移动后再删除。`
    return
  }
  if (!window.confirm(`删除文件夹“${folderLabel(folder)}”？仅空文件夹可以删除。`)) return
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  folderDeletingId.value = folder.folder_id
  actionErr.value = ''
  try {
    const response = await fetch(`/api/kb/folder/${encodeURIComponent(folder.folder_id)}${spaceQuery(requestSpace)}`, {
      method: 'DELETE', headers: headers(),
    })
    const data = await responseJson(response)
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    if (!contextIsCurrent(requestEpoch, requestSpace)) return
    selectedFolderId.value = ''
    uploadFolderId.value = ''
    await loadKnowledgeAssets(requestSpace, requestEpoch)
  } catch (e) {
    if (contextIsCurrent(requestEpoch, requestSpace)) actionErr.value = `删除文件夹失败：${errorText(e)}`
  } finally {
    if (contextIsCurrent(requestEpoch, requestSpace)) folderDeletingId.value = ''
  }
}

async function moveDoc(d: Doc, folderId: string) {
  if (docMovingId.value || (d.folder_id || '') === folderId) return
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  const target = folderId ? folders.value.find((folder) => folder.folder_id === folderId) : null
  const destination = target ? folderLabel(target) : '根目录 / 未分类'
  docMovingId.value = d.doc_id
  actionErr.value = ''
  try {
    const body: Record<string, unknown> = { folder_id: folderId || null }
    const response = await fetch(`/api/kb/doc/${encodeURIComponent(d.doc_id)}/folder`, {
      method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    const data = await responseJson(response)
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    if (!contextIsCurrent(requestEpoch, requestSpace)) return
    await loadKnowledgeAssets(requestSpace, requestEpoch)
  } catch (e) {
    if (contextIsCurrent(requestEpoch, requestSpace)) {
      actionErr.value = `移动《${d.name}》到“${destination}”失败：${errorText(e)}。列表已恢复为服务端状态。`
      await loadKnowledgeAssets(requestSpace, requestEpoch)
    }
  } finally {
    if (contextIsCurrent(requestEpoch, requestSpace)) docMovingId.value = ''
  }
}

async function send(files: File[], retrying?: Doc) {
  if (!files.length || busy.value || !spaceId.value) return
  const requestId = ++uploadRequestId
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  const requestedFolder = uploadFolderId.value
  const targetFolder = folders.value.find((folder) => folder.folder_id === requestedFolder)
  if (requestedFolder && !targetFolder) {
    actionErr.value = '上传目标目录已失效，请刷新目录后重新选择。'
    return
  }
  const requestFolder = targetFolder?.folder_id ?? ''
  const destination = targetFolder ? folderLabel(targetFolder) : '根目录 / 未分类'
  busy.value = true
  actionErr.value = ''
  try {
    for (const file of files) {
      // 前端预校验（逐个反馈，不中断队列）：超限/空文件直接落失败行，权威判定仍在服务端
      if (file.size > MAX_UPLOAD_BYTES || file.size === 0) {
        const why = file.size === 0
          ? '文件为空，未上传'
          : `超过单文件 20MB 上限（实际 ${(file.size / 1024 / 1024).toFixed(1)}MB），未上传`
        uploads.value.unshift({ id: ++uploadId, name: file.name, state: 'fail', msg: why, destination })
        continue
      }
      const row: UploadRow = {
        id: ++uploadId, name: file.name, state: 'doing',
        msg: retrying ? `正在重新处理《${retrying.name}》` : '正在上传并建立索引',
        destination,
      }
      uploads.value.unshift(row)
      const rowId = row.id
      try {
        const form = new FormData()
        form.append('file', file, file.name)
        form.append('space_id', requestSpace)
        if (requestFolder) form.append('folder_id', requestFolder)
        const resp = await fetch('/api/kb/upload', {
          method: 'POST', headers: headers(), body: form,
        })
        const data = await responseJson(resp)
        if (!resp.ok) {
          updateUpload(rowId, { state: 'fail', msg: data.error ?? `HTTP ${resp.status}` })
          continue
        }
        const parts = [statusText(data.status), `${data.chunk_count ?? 0} 个切片`]
        if (data.page_count) parts.push(`${data.page_count} 页`)
        if (data.error) parts.push(data.error)
        updateUpload(rowId, {
          state: uploadState(data.status),
          msg: parts.join(' · '),
          ds: data.datasource ?? null,
        })
      } catch (e) {
        updateUpload(rowId, { state: 'fail', msg: errorText(e) })
      }
    }
  } finally {
    if (requestId === uploadRequestId) busy.value = false
    if (contextIsCurrent(requestEpoch, requestSpace)) await loadSpaces(requestSpace)
  }
}

function onPick(e: Event) {
  const el = e.target as HTMLInputElement
  void send(Array.from(el.files ?? []))
  el.value = ''
}
function onDrop(e: DragEvent) {
  dragging.value = false
  void send(Array.from(e.dataTransfer?.files ?? []))
}
function openFilePicker() {
  if (!busy.value) fileEl.value?.click()
}
function openDirPicker() {
  if (!busy.value) dirEl.value?.click()
}
// 上传文件夹：webkitdirectory 一次给出整棵目录树；文件夹名建成同名 KB 文件夹
// （上传契约只认 folder_id，所以先复用/创建目录拿到 id，再走既有批量上传队列 send()）。
function onPickDir(e: Event) {
  const el = e.target as HTMLInputElement
  const files = Array.from(el.files ?? [])
  el.value = ''
  void sendDirectory(files)
}
async function sendDirectory(files: File[]) {
  if (!files.length || busy.value || !spaceId.value) return
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  // webkitRelativePath 形如「根文件夹/子目录/文件」；KB 文件夹名 = 第一段（嵌套子目录不建层级，全部进该文件夹）
  const rootName = String(files[0]?.webkitRelativePath || '').split('/')[0]?.trim()
  const parentId = uploadFolderId.value || null
  const parentLabel = uploadFolder.value ? folderLabel(uploadFolder.value) : ''
  const destination = rootName ? [parentLabel, rootName].filter(Boolean).join(' / ') : '根目录 / 未分类'
  // 逐个预过滤不支持的扩展名（失败行进队列、逐个提示）；超 20MB / 空文件由 send() 预校验同口径处理
  const accepted: File[] = []
  for (const file of files) {
    const dot = file.name.lastIndexOf('.')
    const ext = dot > 0 ? file.name.slice(dot).toLowerCase() : ''
    if (!ext || !UPLOAD_EXTS.has(ext)) {
      uploads.value.unshift({
        id: ++uploadId, name: file.webkitRelativePath || file.name,
        state: 'fail', msg: '不支持的文件类型，未上传', destination,
      })
      continue
    }
    accepted.push(file)
  }
  if (!accepted.length || !rootName) {
    if (accepted.length) void send(accepted)
    return
  }
  // 复用同名同级目录；没有再调既有 POST /api/kb/folders（与「新建文件夹」对话框同一条契约）
  let folderId = folders.value.find((folder) => folder.name === rootName && (folder.parent_id || null) === parentId)?.folder_id ?? ''
  if (!folderId) {
    try {
      const response = await fetch('/api/kb/folders', {
        method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' },
        body: JSON.stringify({ space_id: requestSpace, name: rootName, parent_id: parentId }),
      })
      const data = await responseJson(response)
      if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
      folderId = String(data.folder_id ?? data.id ?? '')
      if (!contextIsCurrent(requestEpoch, requestSpace)) return
      await loadKnowledgeAssets(requestSpace, requestEpoch)
    } catch (e) {
      // 本地目录列表可能过期（服务端已有同名目录）：刷新后按名再认一次，认不到才报错
      if (!contextIsCurrent(requestEpoch, requestSpace)) return
      await loadKnowledgeAssets(requestSpace, requestEpoch)
      folderId = folders.value.find((folder) => folder.name === rootName && (folder.parent_id || null) === parentId)?.folder_id ?? ''
      if (!folderId) {
        actionErr.value = `创建文件夹“${rootName}”失败：${errorText(e)}。文件未上传，请重试或改用「选择文件」。`
        return
      }
    }
  }
  if (!folderId || !contextIsCurrent(requestEpoch, requestSpace)) return
  uploadFolderId.value = folderId
  void send(accepted)
}
// 从 URL 添加入库（Y12）：服务端抓取 HTML/PDF → 与文件上传同一条 ingest 链。
// 反馈复用上传队列行；目标目录沿用「上传到」选择。权威校验（SSRF/大小/类型）全在服务端。
async function ingestUrl() {
  const url = urlInput.value.trim()
  if (!url || urlBusy.value || busy.value || !spaceId.value) return
  if (!/^https?:\/\//i.test(url)) {
    actionErr.value = '只支持 http:// 或 https:// 地址。'
    return
  }
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  const requestedFolder = uploadFolderId.value
  const targetFolder = folders.value.find((folder) => folder.folder_id === requestedFolder)
  const destination = targetFolder ? folderLabel(targetFolder) : '根目录 / 未分类'
  urlBusy.value = true
  actionErr.value = ''
  const row: UploadRow = { id: ++uploadId, name: url, state: 'doing', msg: '正在抓取并建立索引', destination }
  uploads.value.unshift(row)
  const rowId = row.id
  try {
    const body: Record<string, string> = { url, space_id: requestSpace }
    if (requestedFolder && targetFolder) body.folder_id = requestedFolder
    const resp = await fetch('/api/kb/ingest-url', {
      method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    const data = await responseJson(resp)
    if (!resp.ok) {
      updateUpload(rowId, { state: 'fail', msg: data.error ?? `HTTP ${resp.status}` })
      return
    }
    const parts = [statusText(data.status), `${data.chunk_count ?? 0} 个切片`]
    if (data.page_count) parts.push(`${data.page_count} 页`)
    if (data.error) parts.push(data.error)
    updateUpload(rowId, { state: uploadState(data.status), msg: parts.join(' · ') })
    urlInput.value = ''
    if (contextIsCurrent(requestEpoch, requestSpace)) await loadSpaces(requestSpace)
  } catch (e) {
    updateUpload(rowId, { state: 'fail', msg: errorText(e) })
  } finally {
    urlBusy.value = false
  }
}
// 生成描述（Y7）：AI 按文档开头摘录生成并写回；响应是整份 doc，直接取 description 更新行内展示
async function generateDescription(d: Doc) {
  if (descGeneratingId.value) return
  descGeneratingId.value = d.doc_id
  actionErr.value = ''
  try {
    const resp = await fetch(`/api/kb/doc/${encodeURIComponent(d.doc_id)}/description`, {
      method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' }, body: JSON.stringify({}),
    })
    const data = await responseJson(resp)
    if (!resp.ok) {
      actionErr.value = data.error ?? `生成描述失败（HTTP ${resp.status}）`
      return
    }
    d.description = data.description ?? ''
  } catch (e) {
    actionErr.value = `生成描述失败：${errorText(e)}`
  } finally {
    descGeneratingId.value = ''
  }
}
async function reprocess(d: Doc) {
  if (reprocessingId.value) return
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  reprocessingId.value = d.doc_id
  actionErr.value = ''
  try {
    const body: Record<string, string> = {}
    const response = await fetch(`/api/kb/doc/${encodeURIComponent(d.doc_id)}/reprocess`, {
      method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    const data = await responseJson(response)
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    if (contextIsCurrent(requestEpoch, requestSpace)) await loadSpaces(requestSpace)
  } catch (e) {
    if (contextIsCurrent(requestEpoch, requestSpace)) actionErr.value = `重新处理《${d.name}》失败：${errorText(e)}`
  } finally {
    if (contextIsCurrent(requestEpoch, requestSpace)) reprocessingId.value = ''
  }
}

async function toggleState(d: Doc) {
  if (stateChangingId.value) return
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  stateChangingId.value = d.doc_id
  actionErr.value = ''
  try {
    const body: Record<string, string | boolean> = { enabled: d.enabled === false }
    const response = await fetch(`/api/kb/doc/${encodeURIComponent(d.doc_id)}/state`, {
      method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    const data = await responseJson(response)
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    if (contextIsCurrent(requestEpoch, requestSpace)) await loadSpaces(requestSpace)
  } catch (e) {
    if (contextIsCurrent(requestEpoch, requestSpace)) actionErr.value = `修改《${d.name}》状态失败：${errorText(e)}`
  } finally {
    if (contextIsCurrent(requestEpoch, requestSpace)) stateChangingId.value = ''
  }
}

async function refreshGrants(space: string, requestId: number, epoch: number) {
  const response = await fetch(`/api/kb/space/${encodeURIComponent(space)}/grant`, { headers: headers() })
  const data = await responseJson(response)
  if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
  if (requestId !== grantsRequestId || !contextIsCurrent(epoch, space) || !grantOpen.value) return false
  grants.value = data.grants ?? []
  roleOptions.value = data.roles ?? []
  const limit = Number(data.limits?.batch_grants)
  grantBatchLimit.value = Number.isInteger(limit) && limit > 0 ? limit : 100
  const available = new Set(roleOptions.value.map((role) => role.role_code))
  selectedRoleCodes.value = selectedRoleCodes.value
    .filter((code) => available.has(code))
    .slice(0, grantBatchLimit.value)
  return true
}
async function openGrants() {
  if (!currentSpace.value) return
  const requestId = ++grantsRequestId
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  resetGrantDraft()
  grants.value = []
  roleOptions.value = []
  grantBatchLimit.value = 100
  grantOpen.value = true
  grantsLoading.value = true
  actionErr.value = ''
  try {
    await refreshGrants(requestSpace, requestId, requestEpoch)
  } catch (e) {
    if (requestId === grantsRequestId && contextIsCurrent(requestEpoch, requestSpace) && grantOpen.value) {
      actionErr.value = `读取共享权限失败：${errorText(e)}`
      closeGrants()
    }
  } finally {
    if (requestId === grantsRequestId && contextIsCurrent(requestEpoch, requestSpace)) grantsLoading.value = false
  }
}

async function saveGrant() {
  const grantee = grantTarget.value.trim()
  const roles = [...new Set(selectedRoleCodes.value)].slice(0, grantBatchLimit.value)
  if (granting.value || revokingGrant.value || (grantKind.value === 'login' ? !grantee : !roles.length)) return
  const requestId = ++grantsRequestId
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  granting.value = true
  actionErr.value = ''
  grantFeedback.value = ''
  grantFeedbackError.value = false
  try {
    const body: Record<string, unknown> = {
      grantee_kind: grantKind.value,
      grantee: grantKind.value === 'login' ? grantee : '',
      role_codes: grantKind.value === 'role' ? roles : [],
      perm: grantPerm.value,
    }
    const response = await fetch(`/api/kb/space/${encodeURIComponent(requestSpace)}/grant`, {
      method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    })
    const data = await responseJson(response)
    if (!response.ok && response.status !== 207) throw new Error(data.error ?? `HTTP ${response.status}`)
    if (requestId !== grantsRequestId || !contextIsCurrent(requestEpoch, requestSpace) || !grantOpen.value) return
    const failed = Array.isArray(data.failed) ? data.failed : []
    const succeeded = Array.isArray(data.succeeded) ? data.succeeded.length : Number(data.succeeded) || 0
    if (failed.length) {
      grantFeedbackError.value = true
      grantFeedback.value = `已成功 ${succeeded} 项，失败 ${failed.length} 项：${failed
        .slice(0, 5).map((item: any) => `${item.role_code || item.grantee || '未知'}（${item.error || '失败'}）`).join('、')}`
      const available = new Set(roleOptions.value.map((role) => role.role_code))
      selectedRoleCodes.value = failed
        .map((item: any) => String(item.role_code || ''))
        .filter((code: string) => code && available.has(code))
    } else {
      grantFeedback.value = grantKind.value === 'role' ? `已更新 ${succeeded} 个角色的共享权限` : '账号共享权限已更新'
      grantTarget.value = ''
      selectedRoleCodes.value = []
    }
    const response2 = await fetch(`/api/kb/space/${encodeURIComponent(requestSpace)}/grant`, { headers: headers() })
    const refreshed = await responseJson(response2)
    if (requestId === grantsRequestId && contextIsCurrent(requestEpoch, requestSpace) && grantOpen.value && response2.ok) {
      grants.value = refreshed.grants ?? []
      roleOptions.value = refreshed.roles ?? roleOptions.value
    }
    if (requestId === grantsRequestId && contextIsCurrent(requestEpoch, requestSpace)) await loadSpaces(requestSpace)
  } catch (e) {
    if (requestId === grantsRequestId && contextIsCurrent(requestEpoch, requestSpace) && grantOpen.value) {
      grantFeedbackError.value = true
      grantFeedback.value = `授权失败：${errorText(e)}`
    }
  } finally {
    if (requestId === grantsRequestId && contextIsCurrent(requestEpoch, requestSpace)) granting.value = false
  }
}

async function revokeGrant(g: Grant) {
  const key = `${g.grantee_kind}:${g.grantee}:${g.perm}`
  if (granting.value || revokingGrant.value) return
  const requestId = ++grantsRequestId
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  revokingGrant.value = key
  actionErr.value = ''
  try {
    const params = new URLSearchParams({
      grantee_kind: g.grantee_kind, grantee: g.grantee, perm: g.perm,
    })
    const response = await fetch(`/api/kb/space/${encodeURIComponent(requestSpace)}/grant?${params}`, {
      method: 'DELETE', headers: headers(),
    })
    const data = await responseJson(response)
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    if (requestId !== grantsRequestId || !contextIsCurrent(requestEpoch, requestSpace) || !grantOpen.value) return
    await refreshGrants(requestSpace, requestId, requestEpoch)
    if (requestId === grantsRequestId && contextIsCurrent(requestEpoch, requestSpace)) await loadSpaces(requestSpace)
  } catch (e) {
    if (requestId === grantsRequestId && contextIsCurrent(requestEpoch, requestSpace) && grantOpen.value) {
      grantFeedbackError.value = true
      grantFeedback.value = `移除“${grantName(g)}”失败：${errorText(e)}`
    }
  } finally {
    if (requestId === grantsRequestId && contextIsCurrent(requestEpoch, requestSpace)) revokingGrant.value = ''
  }
}

async function removeConfirmed() {
  const d = confirmDoc.value
  if (!d || deletingId.value) return
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  deletingId.value = d.doc_id
  actionErr.value = ''
  try {
    const resp = await fetch(`/api/kb/doc/${encodeURIComponent(d.doc_id)}${spaceQuery(requestSpace)}`, {
      method: 'DELETE', headers: headers(),
    })
    const data = await responseJson(resp)
    if (!resp.ok) throw new Error(data.error ?? `HTTP ${resp.status}`)
    if (!contextIsCurrent(requestEpoch, requestSpace)) return
    confirmDoc.value = null
    await loadSpaces(requestSpace)
  } catch (e) {
    if (contextIsCurrent(requestEpoch, requestSpace)) deleteDialogErr.value = `删除《${d.name}》失败：${errorText(e)}`
  } finally {
    if (contextIsCurrent(requestEpoch, requestSpace)) deletingId.value = ''
  }
}

onBeforeUnmount(() => {
  contextEpoch++
  spacesRequestId++
  assetsRequestId++
  retrievalRequestId++
  metadataRequestId++
  grantsRequestId++
  uploadRequestId++
})

void loadSpaces(props.initialSpace)
</script>

<template>
  <div class="kbp-mask" @click.self="closePanel">
    <section class="kbp" role="dialog" aria-modal="true" aria-labelledby="kb-title" @keydown.esc="closePanel">
      <header class="kbp-head">
        <div>
          <div class="kbp-title-line">
            <h2 id="kb-title">企业知识库</h2>
            <span v-if="currentSpace" class="scope-badge" :class="{ shared: currentSpace.owner !== login }">
              {{ currentSpace.owner === login ? '我的空间' : currentSpace.writable ? '共享可编辑' : '只读共享' }}
            </span>
          </div>
          <p>按知识空间管理业务资料，并跟踪解析、切片与索引状态。</p>
        </div>
        <button class="icon-btn" type="button" title="关闭" aria-label="关闭知识库" :disabled="switchingDisabled" @click="closePanel">×</button>
      </header>

      <div class="kbp-body">
        <section class="space-section" aria-label="知识空间">
          <div class="space-copy">
            <label for="kb-space">当前空间</label>
            <span v-if="currentSpace">{{ currentSpace.doc_count }} 份文档 · 所有者 {{ currentSpace.owner }}</span>
          </div>
          <select id="kb-space" v-model="spaceId" :disabled="!spaces.length || switchingDisabled" @change="changeSpace">
            <option v-for="space in spaces" :key="space.space_id" :value="space.space_id">
              {{ space.name }}{{ space.writable ? '' : '（只读）' }}
            </option>
          </select>
          <div v-if="kbManager" class="space-actions">
            <button v-if="currentSpace" class="secondary-btn" type="button" @click="openGrants">共享权限</button>
            <button class="secondary-btn" type="button" @click="createOpen = true">新建空间</button>
          </div>
        </section>
        <div v-if="spacesErr" class="action-error" role="alert">空间读取失败：{{ spacesErr }}</div>

        <nav class="workbench-tabs" role="tablist" aria-label="知识库工作台">
          <button
            id="kb-documents-tab" type="button" role="tab" :class="{ active: activeTab === 'documents' }"
            :aria-selected="activeTab === 'documents'" aria-controls="kb-documents-panel"
            @click="activeTab = 'documents'"
          >文档管理</button>
          <button
            id="kb-retrieval-tab" type="button" role="tab" :class="{ active: activeTab === 'retrieval' }"
            :aria-selected="activeTab === 'retrieval'" aria-controls="kb-retrieval-panel"
            @click="activeTab = 'retrieval'"
          >检索测试</button>
          <button
            id="kb-graph-tab" type="button" role="tab" :class="{ active: activeTab === 'graph' }"
            :aria-selected="activeTab === 'graph'" aria-controls="kb-graph-panel"
            @click="activeTab = 'graph'"
          >知识图谱</button>
          <button
            id="kb-mindmap-tab" type="button" role="tab" :class="{ active: activeTab === 'mindmap' }"
            :aria-selected="activeTab === 'mindmap'" aria-controls="kb-mindmap-panel"
            @click="activeTab = 'mindmap'"
          >知识导图</button>
          <button
            id="kb-eval-tab" type="button" role="tab" :class="{ active: activeTab === 'eval' }"
            :aria-selected="activeTab === 'eval'" aria-controls="kb-eval-panel"
            @click="activeTab = 'eval'"
          >RAG 评估</button>
        </nav>

        <template v-if="activeTab === 'documents'">
        <section id="kb-documents-panel" class="folder-workbench" role="tabpanel" aria-labelledby="kb-documents-tab">
          <aside class="folder-tree">
            <div class="folder-tree-head">
              <div><strong>资料目录</strong><span>{{ folders.length }} 个文件夹</span></div>
              <button
                v-if="currentSpace?.writable" class="icon-btn" type="button"
                :disabled="switchingDisabled || folderApiAvailable === false"
                :title="folderApiAvailable === false ? '服务端尚未启用目录接口' : '新建文件夹'"
                aria-label="新建文件夹" @click="folderCreateOpen = true"
              >+</button>
            </div>
            <button type="button" class="folder-node root" :class="{ active: !selectedFolderId }" :aria-current="!selectedFolderId ? 'page' : undefined" :disabled="switchingDisabled" @click="selectFolder('')">
              <span class="folder-icon">⌂</span><span>全部文档</span><b>{{ docs.length }}</b>
            </button>
            <div
              v-for="row in visibleFolderRows" :key="row.folder.folder_id" class="folder-node-row"
              :class="{ active: selectedFolderId === row.folder.folder_id }"
              :style="{ paddingLeft: `${8 + row.depth * 16}px` }"
            >
              <button
                v-if="folderChildren.has(row.folder.folder_id)" type="button" class="folder-expander"
                :disabled="switchingDisabled"
                :aria-label="collapsedFolderIds.includes(row.folder.folder_id) ? '展开子文件夹' : '收起子文件夹'"
                :aria-expanded="!collapsedFolderIds.includes(row.folder.folder_id)"
                @click="toggleFolder(row.folder.folder_id)"
              >{{ collapsedFolderIds.includes(row.folder.folder_id) ? '›' : '⌄' }}</button>
              <span v-else class="folder-expander placeholder" aria-hidden="true"></span>
              <button
                type="button" class="folder-node" :title="folderLabel(row.folder)"
                :aria-current="selectedFolderId === row.folder.folder_id ? 'page' : undefined"
                :disabled="switchingDisabled"
                @click="selectFolder(row.folder.folder_id)"
              >
                <span class="folder-icon">▰</span><span>{{ row.folder.name }}</span><b>{{ row.folder.doc_count ?? folderCounts.get(row.folder.folder_id) ?? 0 }}</b>
              </button>
            </div>
            <button v-if="docs.some((doc) => !doc.folder_id)" type="button" class="folder-node" :class="{ active: selectedFolderId === '__unfiled__' }" :aria-current="selectedFolderId === '__unfiled__' ? 'page' : undefined" :disabled="switchingDisabled" @click="selectFolder('__unfiled__')">
              <span class="folder-icon">◇</span><span>未分类</span><b>{{ docs.filter((doc) => !doc.folder_id).length }}</b>
            </button>
            <div v-if="foldersLoading" class="folder-state" role="status">正在读取目录…</div>
            <div v-else-if="foldersErr" class="folder-state error" role="alert">
              <span>目录加载失败</span>
              <button type="button" class="text-btn" :disabled="loading || foldersLoading" @click="loadFolders()">重试</button>
            </div>
            <div v-else-if="folderApiAvailable !== false && !folders.length" class="folder-state">暂无文件夹</div>
            <p v-if="folderApiAvailable === false" class="folder-contract">当前服务端尚未启用目录接口；现有文档仍按“全部文档”管理。</p>
          </aside>
          <div class="folder-content">
            <nav class="folder-breadcrumb" aria-label="当前目录" :title="selectedFolderName">
              <span class="breadcrumb-label">当前位置</span>
              <div class="breadcrumb-path">
                <button type="button" @click="selectFolder('')">全部文档</button>
                <template v-if="selectedFolder">
                  <template v-for="folder in selectedFolderTrail" :key="folder.folder_id">
                    <span aria-hidden="true">/</span>
                    <button type="button" :class="{ current: folder.folder_id === selectedFolderId }" @click="selectFolder(folder.folder_id)">{{ folder.name }}</button>
                  </template>
                </template>
                <template v-else-if="selectedFolderId === '__unfiled__'">
                  <span aria-hidden="true">/</span><strong>未分类</strong>
                </template>
              </div>
              <small>{{ visibleDocs.length }} 份文档</small>
              <div v-if="selectedFolder && currentSpace?.writable" class="folder-commands">
                <button class="text-btn" type="button" :disabled="switchingDisabled" @click="openFolderEdit">改名/移动</button>
                <button class="text-btn danger" type="button" :disabled="!!folderDeletingId" @click="deleteSelectedFolder">{{ folderDeletingId ? '删除中' : '删除' }}</button>
              </div>
            </nav>
        <section v-if="currentSpace?.writable" class="upload-section" aria-label="上传文档">
          <div class="upload-destination">
            <label for="kb-upload-folder">上传到</label>
            <select id="kb-upload-folder" v-model="uploadFolderId" :disabled="busy || folderApiAvailable === false">
              <option value="">根目录 / 未分类</option>
              <option v-for="row in folderRows" :key="row.folder.folder_id" :value="row.folder.folder_id">{{ '　'.repeat(row.depth) }}{{ folderLabel(row.folder) }}</option>
            </select>
            <span>目标路径：{{ uploadFolder ? folderLabel(uploadFolder) : '根目录 / 未分类' }}</span>
          </div>
          <div
            class="drop-zone" :class="{ active: dragging, disabled: busy }" role="button" tabindex="0"
            :aria-busy="busy" :aria-disabled="busy" aria-label="选择或拖放文件上传"
            @dragover.prevent="dragging = true" @dragleave.prevent="dragging = false"
            @drop.prevent="onDrop" @click="openFilePicker"
            @keydown.enter.prevent="openFilePicker" @keydown.space.prevent="openFilePicker"
          >
            <input ref="fileEl" type="file" multiple hidden :accept="UPLOAD_ACCEPT" @click.stop @change="onPick" />
            <span class="upload-mark" aria-hidden="true">↑</span>
            <div class="drop-copy">
              <strong>{{ busy ? '正在处理上传队列' : '拖放文件到此处，或点击选择（可多选）' }}</strong>
              <span>支持 PDF/Word/Excel/PPT/txt/md/csv/json/log/html 与 png/jpg/webp/gif/bmp 等图片；单文件 ≤20MB，逐个上传逐个反馈。</span>
            </div>
            <span class="primary-btn upload-action" aria-hidden="true">{{ busy ? '处理中' : '选择文件' }}</span>
          </div>
          <div class="dir-upload">
            <input ref="dirEl" type="file" webkitdirectory hidden @click.stop @change="onPickDir" />
            <button class="secondary-btn" type="button" :disabled="busy" @click="openDirPicker">📁 上传文件夹</button>
            <span class="dir-hint">文件夹名会建成同名 KB 文件夹（嵌套子目录不建层级）；不支持的类型与超 20MB 的文件逐个跳过并在队列中提示</span>
          </div>

          <form class="url-ingest" @submit.prevent="ingestUrl">
            <label class="sr-only" for="kb-url-input">从 URL 添加</label>
            <input
              id="kb-url-input" v-model="urlInput" type="url" required
              placeholder="https://…（抓取网页或 PDF 入当前空间）" :disabled="urlBusy || busy"
            />
            <button class="secondary-btn" type="submit" :disabled="urlBusy || busy || !urlInput.trim()">
              {{ urlBusy ? '抓取中' : '从 URL 添加' }}
            </button>
          </form>
          <p class="url-ingest-hint">服务端抓取 HTML 页面或 PDF 后走同一套解析/分块/索引/权限流程；仅支持 http/https、单页 ≤5MB。</p>

          <div v-if="uploads.length" class="upload-queue" aria-live="polite">
            <div class="queue-head">
              <strong>本次处理</strong>
              <button type="button" class="text-btn" @click="uploads = uploads.filter((u) => u.state === 'doing')">清除已完成</button>
            </div>
            <div v-for="u in uploads" :key="u.id" class="queue-row" :class="u.state">
              <span class="queue-state" aria-hidden="true">{{ u.state === 'doing' ? '···' : u.state === 'ok' ? '✓' : u.state === 'partial' ? '!' : '×' }}</span>
              <div class="queue-main">
                <strong :title="u.name">{{ u.name }}</strong>
                <span>{{ u.msg }}</span>
                <span v-if="u.destination" class="queue-destination">目标目录：{{ u.destination }}</span>
                <div v-if="u.ds" class="data-source-note">
                  已生成 {{ u.ds.tables.length }} 张可问数表
                  <span v-for="t in u.ds.tables" :key="t.table">{{ t.sheet }} · {{ t.rows }} 行</span>
                  <span v-if="u.ds.skipped.length" class="warn">未建表：{{ u.ds.skipped.join('、') }}</span>
                </div>
              </div>
            </div>
          </div>
        </section>
        <div v-else-if="currentSpace" class="readonly-note">该空间以只读方式共享给你，可以检索和查看文档，但不能上传、重处理或删除。</div>

        <div v-if="actionErr" class="action-error" role="alert">{{ actionErr }}</div>

        <section class="library-section" aria-label="文档列表">
          <div class="library-head">
            <div>
              <h3>文档资产</h3>
              <span>{{ counts.ready }} 份可检索 · {{ counts.attention }} 份需处理</span>
            </div>
            <div class="library-tools">
              <label class="search-box">
                <span class="sr-only">搜索文档</span>
                <input v-model="search" type="search" placeholder="搜索名称、类型、状态或失败原因" />
                <button v-if="search" type="button" title="清空搜索" aria-label="清空搜索" @click="search = ''">×</button>
              </label>
              <button class="icon-btn" type="button" title="刷新列表" aria-label="刷新列表" :disabled="loading || switchingDisabled" @click="loadSpaces(spaceId)">↻</button>
            </div>
          </div>

          <div class="filter-bar" aria-label="文档状态筛选">
            <button
              v-for="item in filters" :key="item.value" type="button"
              :class="{ active: filter === item.value }" @click="filter = item.value"
            >
              {{ item.label }} <span>{{ item.count }}</span>
            </button>
          </div>

          <div v-if="listErr" class="list-state error" role="alert">
            <strong>文档列表加载失败</strong>
            <span>{{ listErr }}</span>
            <button class="secondary-btn" type="button" @click="loadSpaces(spaceId)">重新加载</button>
          </div>
          <div v-else-if="loading && !docs.length" class="list-state">
            <strong>正在读取知识库</strong>
            <span>请稍候。</span>
          </div>
          <div v-else-if="!docs.length" class="list-state empty">
            <strong>知识库还是空的</strong>
            <span>上传制度、产品资料、合同模板或业务表格后，即可在对话中检索和引用。</span>
            <button v-if="currentSpace?.writable" class="primary-btn" type="button" :disabled="busy" @click="openFilePicker">上传第一份文档</button>
          </div>
          <div v-else-if="!visibleDocs.length" class="list-state empty">
            <strong>没有匹配的文档</strong>
            <span>调整关键词或切换状态筛选。</span>
            <button class="secondary-btn" type="button" @click="search = ''; filter = 'all'">清除筛选</button>
          </div>
          <div v-else class="doc-table">
            <div class="doc-table-head" aria-hidden="true">
              <span>文档</span><span>类型</span><span>大小</span><span>内容</span><span>更新时间</span><span>状态与操作</span>
            </div>
            <article v-for="d in visibleDocs" :key="d.doc_id" class="doc-row" :class="[stateOf(d.status), { disabled: d.enabled === false }]">
              <div class="doc-name-cell">
                <span class="file-type">{{ extOf(d.name) }}</span>
                <div>
                  <strong :title="d.name">{{ d.name }}</strong>
                  <span class="doc-lineage">
                    <template v-if="folderPath(d)">目录：{{ folderPath(d) }} · </template><template v-if="d.uploaded_by">上传人：{{ d.uploaded_by }}</template>
                  </span>
                   <div v-if="d.business_domain || d.document_family || d.document_revision || d.tags?.length || effectiveText(d)" class="doc-governance">
                    <span v-if="d.business_domain" class="domain-tag">{{ d.business_domain }}</span>
                    <span v-if="d.document_family">{{ d.document_family }}</span>
                    <span v-if="d.document_revision" class="revision-tag">{{ d.document_revision }}</span>
                    <span v-for="tag in d.tags" :key="tag">{{ tag }}</span>
                    <span v-if="effectiveText(d)" class="effective-tag">{{ effectiveText(d) }}</span>
                  </div>
                  <div v-if="d.description" class="doc-desc" :title="d.description">{{ d.description }}</div>
                </div>
              </div>
              <span class="doc-type" data-label="类型">{{ typeText(d) }}</span>
              <span data-label="大小">{{ sizeText(d.bytes) }}</span>
              <span data-label="内容">{{ d.page_count ? `${d.page_count} 页 · ` : '' }}{{ d.chunk_count ?? 0 }} 切片</span>
              <span data-label="更新时间">{{ dateText(d.updated_at || d.created_at) }}</span>
              <div class="doc-status-cell">
                <div class="status-line">
                  <span class="status-dot" aria-hidden="true"></span>
                  <strong>{{ displayStatusText(d) }}</strong>
                  <span v-if="d.quality?.label" class="quality-badge" :class="qualityClass(d.quality.level)">{{ d.quality.label }}</span>
                </div>
                <span class="status-hint" :class="{ notice: d.notice && !d.error }" :title="statusHint(d)">{{ statusHint(d) }}</span>
                <div class="row-actions">
                  <button type="button" class="text-btn" @click="previewDoc = d">预览</button>
                  <button type="button" class="text-btn" @click="downloadDoc(d.doc_id, d.name)">下载原件</button>
                  <template v-if="currentSpace?.writable">
                    <label class="doc-move" :title="`当前目录：${folderPath(d) || '根目录 / 未分类'}`">
                      <span>移动至</span>
                      <select
                        class="doc-folder-select" :value="d.folder_id || ''" :disabled="!!docMovingId"
                        :aria-label="`移动《${d.name}》到文件夹`"
                        @change="moveDoc(d, ($event.target as HTMLSelectElement).value)"
                      >
                        <option value="">根目录 / 未分类</option>
                        <option v-for="row in folderRows" :key="row.folder.folder_id" :value="row.folder.folder_id">{{ '　'.repeat(row.depth) }}{{ folderLabel(row.folder) }}</option>
                      </select>
                    </label>
                    <span v-if="docMovingId === d.doc_id" class="moving-note" role="status">移动中…</span>
                    <button
                      v-if="stateOf(d.status) !== 'ready'" type="button" class="text-btn"
                      :disabled="!!reprocessingId" title="使用服务器保存的原文件重新解析并建立索引"
                      @click="reprocess(d)"
                    >{{ reprocessingId === d.doc_id ? '处理中' : '重新处理' }}</button>
                    <button
                      type="button" class="text-btn" :disabled="!!stateChangingId"
                      :title="d.enabled === false ? '恢复参与知识检索' : '暂时从知识检索中移除'"
                      @click="toggleState(d)"
                    >{{ stateChangingId === d.doc_id ? '处理中' : d.enabled === false ? '启用' : '停用' }}</button>
                    <button type="button" class="text-btn" @click="openMetadata(d)">元数据</button>
                    <button
                      type="button" class="text-btn" :disabled="!!descGeneratingId"
                      title="AI 按文档开头生成一段描述并写回（覆盖已有描述，参与检索召回）"
                      @click="generateDescription(d)"
                    >{{ descGeneratingId === d.doc_id ? '生成中' : '生成描述' }}</button>
                    <button type="button" class="text-btn danger" :disabled="deletingId === d.doc_id" @click="openDeleteConfirm(d)">删除</button>
                  </template>
                </div>
              </div>
            </article>
          </div>
        </section>
          </div>
        </section>
        </template>

        <section v-else-if="activeTab === 'retrieval'" id="kb-retrieval-panel" class="retrieval-section" role="tabpanel" aria-labelledby="kb-retrieval-tab retrieval-title">
          <div class="retrieval-head">
            <div>
              <h3 id="retrieval-title">检索测试</h3>
              <span>在“{{ currentSpace?.name || '当前空间' }}”中验证问题能否找到正确文档与原文。</span>
            </div>
          </div>

          <div v-if="sampleQuestions.length" class="sample-questions" aria-label="样例问题">
            <span class="sample-questions-label">样例问题</span>
            <button
              v-for="question in sampleQuestions" :key="question" type="button"
              :title="question" :disabled="retrievalLoading"
              @click="askSample(question)"
            >{{ question }}</button>
          </div>

          <form class="retrieval-form" @submit.prevent="runRetrieval">
            <label for="retrieval-question">测试问题</label>
            <div class="retrieval-input-row">
              <textarea
                id="retrieval-question" v-model="retrievalQuestion" rows="2" maxlength="500"
                placeholder="输入用户真实会问的问题，例如：差旅报销的住宿标准是多少？"
                @keydown.ctrl.enter.prevent="runRetrieval" @keydown.meta.enter.prevent="runRetrieval"
              ></textarea>
              <button class="primary-btn" type="submit" :disabled="retrievalLoading || !retrievalQuestion.trim() || !spaceId">
                {{ retrievalLoading ? '检索中' : '开始检索' }}
              </button>
            </div>
            <span>仅检索当前空间；可按 Ctrl + Enter 提交。</span>
          </form>

          <div v-if="retrievalErr" class="action-error" role="alert">检索失败：{{ retrievalErr }}</div>
          <div v-if="vectorDegraded" class="retrieval-warning" role="status">
            向量检索当前不可用，本次结果已降级为关键词检索，排序覆盖面可能下降。
          </div>

          <div v-if="retrievalRan" class="retrieval-summary" aria-live="polite">
            <strong>{{ retrievalHits.length }}</strong> 条命中
            <span v-if="!vectorDegraded">混合检索正常</span>
            <span v-else class="degraded">向量已降级</span>
          </div>
          <div v-if="retrievalLoading" class="retrieval-state">
            <strong>正在检索当前空间</strong>
            <span>正在执行关键词与向量召回。</span>
          </div>
          <div v-else-if="retrievalRan && !retrievalHits.length" class="retrieval-state">
            <strong>没有找到相关内容</strong>
            <span>可换用文档中的具体名词，或检查相关文档是否已完成索引且处于启用状态。</span>
          </div>
          <div v-else-if="retrievalHits.length" class="hit-list">
            <article v-for="(hit, index) in retrievalHits" :key="hit.chunk_id" class="hit-row">
              <div class="hit-rank" aria-hidden="true">{{ index + 1 }}</div>
              <div class="hit-main">
                <div class="hit-title-line">
                  <strong :title="hit.doc_name">{{ hit.doc_name }}</strong>
                </div>
                <div class="hit-location">{{ hitLocation(hit) }}</div>
                <div class="hit-governance">
                  <span v-if="hit.business_domain" class="domain-tag">{{ hit.business_domain }}</span>
                  <span v-for="tag in hit.tags" :key="tag">{{ tag }}</span>
                  <span>{{ governanceText(hit) }}</span>
                  <span>{{ versionText(hit) }}</span>
                </div>
                <p>{{ hit.preview || '该命中没有可展示的文本预览。' }}</p>
                <div class="hit-actions">
                  <button type="button" class="text-btn" @click="toggleHit(hit)">
                    {{ openingHit[hit.chunk_id] ? '加载中' : openedHit[hit.chunk_id] ? '收起原文' : '展开原文' }}
                  </button>
                  <button type="button" class="text-btn" @click="downloadDoc(hit.doc_id, hit.doc_name)">下载原件</button>
                  <a v-if="safeSourceUri(hit.source_uri)" class="text-btn" :href="safeSourceUri(hit.source_uri)" target="_blank" rel="noopener noreferrer">打开来源</a>
                </div>
                <div v-if="hitErr[hit.chunk_id]" class="hit-open-error" role="alert">原文加载失败：{{ hitErr[hit.chunk_id] }}</div>
                <pre v-if="openedHit[hit.chunk_id]" class="hit-original">{{ openedHit[hit.chunk_id] }}</pre>
              </div>
            </article>
          </div>
          <div v-else class="retrieval-state initial">
            <strong>验证知识是否能被准确召回</strong>
            <span>输入真实业务问题，查看命中文档、章节页码和文本片段。</span>
          </div>
        </section>

        <section v-else-if="activeTab === 'graph'" id="kb-graph-panel" class="graph-section" role="tabpanel" aria-labelledby="kb-graph-tab">
          <KbGraph :token="token" :space-id="spaceId" :writable="!!currentSpace?.writable" @auth-expired="emit('auth-expired')" />
        </section>

        <section v-else-if="activeTab === 'mindmap'" id="kb-mindmap-panel" class="mindmap-section" role="tabpanel" aria-labelledby="kb-mindmap-tab">
          <KbMindmap :token="token" :space-id="spaceId" :writable="!!currentSpace?.writable" @auth-expired="emit('auth-expired')" />
        </section>

        <section v-else id="kb-eval-panel" class="eval-section" role="tabpanel" aria-labelledby="kb-eval-tab">
          <KbEval :token="token" :space-id="spaceId" :writable="!!currentSpace?.writable" @auth-expired="emit('auth-expired')" />
        </section>
      </div>

      <div v-if="createOpen" class="confirm-mask" @click.self="closeSpaceCreate()">
        <form class="confirm-box create-box" role="dialog" aria-modal="true" aria-labelledby="create-space-title" @submit.prevent="createSpace" @keydown.esc.stop="closeSpaceCreate()">
          <h3 id="create-space-title">新建知识空间</h3>
          <p>用于集中管理部门、项目或业务主题资料。</p>
          <label>
            <span>空间名称</span>
            <input v-model="newSpaceName" maxlength="60" required placeholder="例如：销售运营知识库" />
          </label>
          <label>
            <span>空间标识（可选）</span>
            <input v-model="newSpaceId" maxlength="64" pattern="[A-Za-z0-9_-]+" placeholder="留空则自动生成" />
          </label>
          <div class="confirm-actions">
            <button class="secondary-btn" type="button" :disabled="creating" @click="closeSpaceCreate()">取消</button>
            <button class="primary-btn" type="submit" :disabled="creating || !newSpaceName.trim()">{{ creating ? '创建中' : '创建空间' }}</button>
          </div>
        </form>
      </div>

      <div v-if="grantOpen" class="confirm-mask" @click.self="closeGrants()">
        <form class="confirm-box grant-box" role="dialog" aria-modal="true" aria-labelledby="grant-title" @submit.prevent="saveGrant" @keydown.esc.stop="closeGrants()">
          <h3 id="grant-title">共享权限 · {{ currentSpace?.name }}</h3>
          <p>授权在检索前生效；“可编辑”同时允许上传、重处理、启停和删除文档。</p>
          <div class="grant-form">
            <label>
              <span>对象类型</span>
              <select v-model="grantKind" :disabled="granting || !!revokingGrant">
                <option value="login">用户账号</option>
                <option value="role">DMS 角色</option>
              </select>
            </label>
            <label v-if="grantKind === 'login'" class="grant-target">
              <span>登录账号</span>
              <input v-model="grantTarget" maxlength="64" required placeholder="输入 DMS 登录账号" :disabled="granting || !!revokingGrant" />
            </label>
            <label>
              <span>权限</span>
              <select v-model="grantPerm" :disabled="granting || !!revokingGrant">
                <option value="read">只读</option>
                <option value="write">可编辑</option>
              </select>
            </label>
            <button
              v-if="grantKind === 'login'" class="primary-btn" type="submit"
              :disabled="granting || !grantTarget.trim()"
            >{{ granting ? '授权中' : '添加账号' }}</button>
          </div>
          <section v-if="grantKind === 'role'" class="role-picker" aria-label="DMS 角色多选">
            <div class="role-picker-head">
              <div><strong>选择 DMS 角色</strong><span>支持搜索、多选和全选当前结果</span></div>
              <b>{{ selectedRoleCodes.length }}/{{ grantBatchLimit }}</b>
            </div>
            <div class="role-picker-tools">
              <label>
                <span class="sr-only">搜索 DMS 角色</span>
                <input v-model="roleSearch" type="search" placeholder="搜索角色名称或编码" :disabled="granting || !!revokingGrant" @keydown.enter.prevent />
              </label>
              <button class="secondary-btn role-batch" type="button" :disabled="granting || !!revokingGrant || !filteredRoleOptions.length" @click="toggleFilteredRoles">{{ allFilteredRolesSelected ? '取消全选' : `全选结果（${filteredRoleOptions.length}）` }}</button>
              <button class="text-btn" type="button" :disabled="granting || !!revokingGrant || !selectedRoleCodes.length" @click="clearSelectedRoles">清空</button>
            </div>
            <div v-if="selectedRoleOptions.length" class="selected-roles" aria-label="已选 DMS 角色">
              <button v-for="role in selectedRoleOptions" :key="role.role_code" type="button" :title="`取消选择 ${role.role_name}`" :disabled="granting || !!revokingGrant" @click="toggleRole(role.role_code)">
                <span>{{ role.role_name }}</span><b aria-hidden="true">×</b>
              </button>
            </div>
            <div class="role-options">
              <label v-for="role in filteredRoleOptions" :key="role.role_code" class="role-option">
                <input
                  type="checkbox" :checked="selectedRoleSet.has(role.role_code)"
                  :disabled="granting || !!revokingGrant || (!selectedRoleSet.has(role.role_code) && selectedRoleCodes.length >= grantBatchLimit)"
                  @change="toggleRole(role.role_code)"
                />
                <span><strong>{{ role.role_name }}</strong><small>{{ role.role_code }}</small></span>
              </label>
              <div v-if="!filteredRoleOptions.length" class="grant-empty">
                {{ roleOptions.length ? '没有匹配的 DMS 角色' : 'DMS 当前没有可共享的角色' }}
              </div>
            </div>
            <button
              class="primary-btn role-save" type="submit"
              :disabled="granting || !selectedRoleCodes.length"
            >{{ granting ? '批量授权中' : `授权所选角色（${selectedRoleCodes.length}）` }}</button>
          </section>
          <div v-if="grantFeedback" class="grant-feedback" :class="{ error: grantFeedbackError }" role="status">
            {{ grantFeedback }}
          </div>
          <div class="grant-list" :aria-busy="grantsLoading">
            <div v-if="grantsLoading" class="grant-empty">正在读取权限</div>
            <div v-else-if="!grants.length" class="grant-empty">尚未共享给其他用户或角色</div>
            <template v-else>
              <div v-for="g in grants" :key="`${g.grantee_kind}:${g.grantee}:${g.perm}`" class="grant-row">
                <span class="grant-kind">{{ g.grantee_kind === 'login' ? '用户' : '角色' }}</span>
                <strong>{{ grantName(g) }}</strong>
                <span>{{ g.perm === 'write' ? '可编辑' : '只读' }}</span>
                <button class="text-btn danger" type="button" :disabled="!!revokingGrant" @click="revokeGrant(g)">
                  {{ revokingGrant === `${g.grantee_kind}:${g.grantee}:${g.perm}` ? '移除中' : '移除' }}
                </button>
              </div>
            </template>
          </div>
          <div class="confirm-actions">
            <button class="secondary-btn" type="button" :disabled="granting || !!revokingGrant" @click="closeGrants()">完成</button>
          </div>
        </form>
      </div>

      <div v-if="folderCreateOpen" class="confirm-mask" @click.self="closeFolderCreate()">
        <form class="confirm-box folder-create-box" role="dialog" aria-modal="true" aria-labelledby="create-folder-title" @submit.prevent="createFolder" @keydown.esc.stop="closeFolderCreate()">
          <h3 id="create-folder-title">新建文件夹</h3>
          <p>用目录组织制度、产品、合同和业务资料；上传时可直接选择该目录。</p>
          <label><span>文件夹名称</span><input v-model.trim="newFolderName" maxlength="80" autofocus placeholder="例如：市场费用制度" /></label>
          <label>
            <span>上级目录</span>
            <select v-model="newFolderParentId">
              <option value="">根目录</option>
              <option v-for="row in folderRows" :key="row.folder.folder_id" :value="row.folder.folder_id">{{ '　'.repeat(row.depth) }}{{ folderLabel(row.folder) }}</option>
            </select>
          </label>
          <div v-if="folderDialogErr" class="action-error" role="alert">{{ folderDialogErr }}</div>
          <div class="confirm-actions">
            <button class="secondary-btn" type="button" :disabled="folderCreating" @click="closeFolderCreate()">取消</button>
            <button class="primary-btn" type="submit" :disabled="folderCreating || !newFolderName.trim()">{{ folderCreating ? '创建中' : '创建文件夹' }}</button>
          </div>
        </form>
      </div>

      <div v-if="folderEditOpen && selectedFolder" class="confirm-mask" @click.self="closeFolderEdit()">
        <form class="confirm-box folder-create-box" role="dialog" aria-modal="true" aria-labelledby="edit-folder-title" @submit.prevent="saveFolderEdit" @keydown.esc.stop="closeFolderEdit()">
          <h3 id="edit-folder-title">整理文件夹</h3>
          <p>修改名称或移动到其他目录；目录下文档与切片路径会同步更新。</p>
          <label><span>文件夹名称</span><input v-model.trim="folderEditName" maxlength="80" autofocus /></label>
          <label>
            <span>上级目录</span>
            <select v-model="folderEditParentId">
              <option value="">根目录</option>
              <option v-for="row in folderMoveTargets" :key="row.folder.folder_id" :value="row.folder.folder_id">{{ '　'.repeat(row.depth) }}{{ folderLabel(row.folder) }}</option>
            </select>
          </label>
          <div v-if="folderDialogErr" class="action-error" role="alert">{{ folderDialogErr }}</div>
          <div class="confirm-actions">
            <button class="secondary-btn" type="button" :disabled="folderEditing" @click="closeFolderEdit()">取消</button>
            <button class="primary-btn" type="submit" :disabled="folderEditing || !folderEditName.trim()">{{ folderEditing ? '保存中' : '保存' }}</button>
          </div>
        </form>
      </div>

      <div v-if="metadataDoc" class="confirm-mask" @click.self="closeMetadata()">
        <form class="confirm-box metadata-box" role="dialog" aria-modal="true" aria-labelledby="metadata-title" @submit.prevent="saveMetadata" @keydown.esc.stop="closeMetadata()">
          <h3 id="metadata-title">文档信息与关联</h3>
          <p :title="metadataDoc.name">{{ metadataDoc.name }}</p>
          <div class="metadata-form">
            <label class="metadata-wide">
              <span>标签</span>
              <input v-model="metadataTags" maxlength="500" placeholder="制度, 财务, 报销（逗号分隔）" />
            </label>
            <label>
              <span>业务域</span>
              <input v-model="metadataDomain" maxlength="60" placeholder="例如：财务管理" />
            </label>
            <label>
              <span>文档族</span>
              <input v-model="metadataFamily" maxlength="120" placeholder="例如：培训报销制度" />
            </label>
            <label>
              <span>版本号</span>
              <input v-model="metadataRevision" maxlength="60" placeholder="例如：v2.1" />
            </label>
            <label>
              <span>生效日期</span>
              <input v-model="metadataEffectiveFrom" type="date" />
            </label>
            <label>
              <span>失效日期</span>
              <input v-model="metadataEffectiveTo" type="date" :min="metadataEffectiveFrom || undefined" />
            </label>
            <label class="metadata-wide">
              <span>来源地址</span>
              <input v-model="metadataSourceUri" maxlength="500" placeholder="https:// 或内部资料地址" />
            </label>
          </div>
          <section class="relation-editor" aria-labelledby="relation-title">
            <header>
              <div>
                <h4 id="relation-title">关联文档</h4>
                <span>关联内容会参与跨文档检索和答案组织</span>
              </div>
              <button v-if="metadataRelatedIds.length" class="text-btn" type="button" @click="metadataRelatedIds = []">清空已选</button>
            </header>
            <div v-if="metadataLoading" class="relation-state" role="status">正在加载关联信息…</div>
            <template v-else-if="metadataRelationReady">
              <label class="relation-search">
                <span class="sr-only">搜索关联文档</span>
                <input v-model="metadataRelationSearch" placeholder="搜索文档名、文件夹、文档族或版本" />
              </label>
              <div class="relation-summary">
                <strong>已选 {{ metadataRelatedIds.length }}</strong>
                <span>最多 50 篇</span>
              </div>
              <div v-if="metadataCandidateDocs.length" class="relation-options">
                <label v-for="doc in metadataCandidateDocs" :key="doc.doc_id" class="relation-option">
                  <input type="checkbox" :checked="metadataRelatedSet.has(doc.doc_id)" @change="toggleRelatedDoc(doc.doc_id)" />
                  <span>
                    <strong :title="doc.name">{{ doc.name }}</strong>
                    <small>{{ folderPath(doc) ? `文件夹 ${folderPath(doc)}` : '未分类' }}<template v-if="doc.document_family"> · {{ doc.document_family }}</template><template v-if="doc.document_revision"> · {{ doc.document_revision }}</template></small>
                  </span>
                </label>
              </div>
              <div v-else class="relation-state">没有匹配的可关联文档</div>
              <div v-if="inferredRelations.length" class="inferred-relations">
                <strong>系统识别的内容关联</strong>
                <span v-for="relation in inferredRelations" :key="`${relation.doc_id}-${relation.relation}`">
                  {{ relationText(relation.relation) }} · {{ relation.doc_name }}<template v-if="relation.folder_path && relation.folder_path !== '/'"> · {{ relation.folder_path }}</template>
                </span>
              </div>
            </template>
            <div v-else class="relation-state error">关联信息未加载，请重新打开后再修改。</div>
          </section>
          <div v-if="metadataErr" class="action-error" role="alert">{{ metadataErr }}</div>
          <div class="confirm-actions">
            <button class="secondary-btn" type="button" :disabled="metadataSaving" @click="closeMetadata()">取消</button>
            <button class="primary-btn" type="submit" :disabled="metadataSaving || !metadataRelationReady">
              {{ metadataSaving ? '保存中' : '保存文档信息' }}
            </button>
          </div>
        </form>
      </div>

      <div v-if="confirmDoc" class="confirm-mask" @click.self="closeDeleteConfirm()">
        <div class="confirm-box" role="alertdialog" aria-modal="true" aria-labelledby="delete-title" @keydown.esc.stop="closeDeleteConfirm()">
          <h3 id="delete-title">删除文档？</h3>
          <p>《{{ confirmDoc.name }}》的原文件、切片和检索索引将一并删除，且无法撤销。</p>
          <div v-if="deleteDialogErr" class="action-error" role="alert">{{ deleteDialogErr }}</div>
          <div class="confirm-actions">
            <button class="secondary-btn" type="button" :disabled="!!deletingId" @click="closeDeleteConfirm()">取消</button>
            <button class="danger-btn" type="button" :disabled="!!deletingId" @click="removeConfirmed">
              {{ deletingId ? '删除中' : '确认删除' }}
            </button>
          </div>
        </div>
      </div>

      <KbDocPreview
        v-if="previewDoc" :token="token" :doc-id="previewDoc.doc_id" :doc-name="previewDoc.name" :mime="previewDoc.mime"
        @close="previewDoc = null" @auth-expired="emit('auth-expired')"
      />
    </section>
  </div>
</template>

<style scoped>
.kbp-mask {
  position: fixed; inset: 0; z-index: 60; padding: 28px;
  display: grid; place-items: center; background: rgba(16, 22, 43, .48);
}
.kbp {
  position: relative; width: min(1080px, 100%); height: min(820px, calc(100vh - 56px));
  display: flex; flex-direction: column; overflow: hidden;
  background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; box-shadow: var(--shadow-lg);
}
.kbp-head {
  flex: 0 0 auto; display: flex; align-items: flex-start; gap: 20px;
  padding: 18px 20px 16px; border-bottom: 1px solid var(--divider);
}
.kbp-title-line { display: flex; align-items: center; gap: 10px; }
.kbp h2 { font-size: 18px; line-height: 1.35; color: var(--text-primary); }
.kbp-head p { margin-top: 4px; font-size: 12px; color: var(--text-muted); }
.scope-badge {
  padding: 2px 7px; border: 1px solid var(--border); border-radius: 999px;
  color: var(--text-muted); background: var(--bg-main); font-size: 11px;
}
.scope-badge.shared { color: var(--primary); background: var(--primary-bg); border-color: rgba(var(--primary-rgb), .2); }
.kbp-head > .icon-btn { margin-left: auto; }
.kbp-body { min-height: 0; overflow: auto; padding: 18px 20px 24px; }
.upload-section, .library-section { width: 100%; }
.space-section {
  display: flex; align-items: center; gap: 10px; margin-bottom: 12px; padding: 11px 12px;
  border: 1px solid var(--border); background: var(--bg-card);
}
.space-copy { min-width: 170px; display: flex; flex-direction: column; gap: 2px; }
.space-copy label { color: var(--text-primary); font-size: 12px; font-weight: 700; }
.space-copy span { color: var(--text-muted); font-size: 10.5px; }
.space-section select {
  min-width: 240px; height: 32px; padding: 0 30px 0 9px; border: 1px solid var(--border); border-radius: 6px;
  outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px;
}
.space-section select:focus { border-color: var(--primary); box-shadow: var(--ring); }
.space-actions { margin-left: auto; display: flex; gap: 8px; }
.workbench-tabs {
  display: flex; gap: 18px; margin: 2px 0 16px; border-bottom: 1px solid var(--divider);
}
.workbench-tabs button {
  height: 34px; padding: 0 2px; border: 0; border-bottom: 2px solid transparent;
  background: transparent; color: var(--text-muted); cursor: pointer; font: inherit; font-size: 12.5px;
}
.workbench-tabs button:hover { color: var(--text-primary); }
.workbench-tabs button.active { border-bottom-color: var(--primary); color: var(--primary); font-weight: 700; }
.workbench-tabs button:focus-visible, .folder-node:focus-visible, .folder-expander:focus-visible, .drop-zone:focus-visible {
  outline: 2px solid var(--primary); outline-offset: -2px;
}
.readonly-note { padding: 10px 12px; border-left: 3px solid var(--primary); background: var(--primary-light); color: var(--text-regular); font-size: 11.5px; }
.folder-workbench { display: grid; grid-template-columns: 220px minmax(0, 1fr); gap: 16px; align-items: start; }
.folder-tree { position: sticky; top: 0; min-width: 0; max-height: 640px; overflow: auto; border: 1px solid var(--border); background: var(--bg-card); }
.folder-tree-head { min-height: 48px; display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-bottom: 1px solid var(--divider); background: var(--bg-main); }
.folder-tree-head > div { min-width: 0; display: flex; flex: 1; flex-direction: column; gap: 1px; }
.folder-tree-head strong { color: var(--text-primary); font-size: 12.5px; }
.folder-tree-head span { color: var(--text-muted); font-size: 10px; }
.folder-node { width: 100%; min-height: 36px; display: grid; grid-template-columns: 16px minmax(0, 1fr) auto; align-items: center; gap: 7px; padding: 6px 10px; border: 0; border-bottom: 1px solid var(--divider); background: transparent; color: var(--text-regular); text-align: left; cursor: pointer; font: inherit; }
.folder-node:hover { background: var(--bg-hover); }
.folder-node.active { background: var(--primary-light); color: var(--primary); box-shadow: inset 3px 0 var(--primary); }
.folder-node-row { min-height: 36px; display: flex; align-items: stretch; border-bottom: 1px solid var(--divider); }
.folder-node-row.active { background: var(--primary-light); box-shadow: inset 3px 0 var(--primary); }
.folder-node-row .folder-node { min-width: 0; padding-left: 2px; border-bottom: 0; }
.folder-node-row.active .folder-node, .folder-node-row.active .folder-icon { color: var(--primary); }
.folder-expander {
  width: 22px; flex: 0 0 22px; border: 0; background: transparent; color: var(--text-muted);
  cursor: pointer; font: 16px/1 var(--font-sans);
}
.folder-expander:hover { color: var(--primary); background: var(--bg-hover); }
.folder-expander.placeholder { pointer-events: none; }
.folder-node > span:nth-child(2) { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 11.5px; }
.folder-node b { color: var(--text-muted); font-size: 10px; font-weight: 650; }
.folder-icon { color: var(--text-faint); font-size: 10px; }
.folder-node.active .folder-icon { color: var(--primary); }
.folder-state { min-height: 42px; display: flex; align-items: center; justify-content: center; gap: 7px; padding: 8px; color: var(--text-muted); font-size: 10.5px; text-align: center; }
.folder-state.error { color: var(--error-text); background: var(--error-bg); }
.folder-contract { margin: 0; padding: 10px; color: var(--text-muted); background: var(--bg-main); font-size: 10.5px; line-height: 1.55; }
.folder-content { min-width: 0; }
.folder-breadcrumb { min-height: 40px; display: flex; align-items: center; gap: 8px; margin-bottom: 8px; padding: 7px 10px; border: 1px solid var(--divider); background: var(--bg-main); font-size: 11px; }
.breadcrumb-label { flex: none; color: var(--text-faint); font-size: 10px; }
.breadcrumb-path { min-width: 0; display: flex; align-items: center; gap: 6px; overflow-x: auto; scrollbar-width: thin; }
.breadcrumb-path span, .folder-breadcrumb small { color: var(--text-muted); }
.breadcrumb-path button { min-width: 0; max-width: 180px; flex: 0 1 auto; overflow: hidden; padding: 0; border: 0; background: transparent; color: var(--text-muted); cursor: pointer; font: inherit; text-overflow: ellipsis; white-space: nowrap; }
.breadcrumb-path button:hover { color: var(--primary); text-decoration: underline; }
.breadcrumb-path button.current { color: var(--text-primary); font-weight: 700; text-decoration: none; }
.breadcrumb-path strong { min-width: 0; overflow: hidden; color: var(--text-primary); text-overflow: ellipsis; white-space: nowrap; }
.folder-breadcrumb small { margin-left: auto; white-space: nowrap; }
.folder-commands { display: flex; align-items: center; gap: 7px; margin-left: 4px; padding-left: 8px; border-left: 1px solid var(--divider); }
.upload-destination { display: grid; grid-template-columns: auto minmax(180px, 280px) minmax(0, 1fr); align-items: center; gap: 8px; margin-bottom: 8px; color: var(--text-muted); font-size: 11px; }
.url-ingest { display: flex; gap: 8px; margin-top: 8px; }
.url-ingest input { flex: 1; min-width: 0; height: 32px; padding: 0 10px; border: 1px solid var(--border); border-radius: 6px; font-size: 12px; background: var(--bg-card); color: var(--text-regular); }
.url-ingest input:focus { outline: none; border-color: var(--primary); }
.url-ingest .secondary-btn { flex: 0 0 auto; height: 32px; }
.url-ingest-hint { margin: 4px 0 0; color: var(--text-muted); font-size: 11px; }
.dir-upload { display: flex; align-items: center; gap: 10px; margin-top: 8px; }
.dir-upload .secondary-btn { flex: 0 0 auto; height: 32px; }
.dir-hint { min-width: 0; color: var(--text-muted); font-size: 11px; line-height: 1.5; }
.doc-desc { margin-top: 4px; color: var(--text-muted); font-size: 11px; line-height: 1.5; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
.upload-destination label { color: var(--text-primary); font-weight: 650; }
.upload-destination select, .folder-create-box select { height: 32px; min-width: 0; padding: 0 28px 0 8px; border: 1px solid var(--border); border-radius: 6px; outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 11.5px; }
.upload-destination select:focus, .folder-create-box select:focus { border-color: var(--primary); box-shadow: var(--ring); }
.upload-destination > span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.drop-zone {
  min-height: 88px; display: flex; align-items: center; gap: 14px; padding: 14px 16px;
  border: 1px dashed var(--border); background: var(--bg-main); cursor: pointer;
  transition: border-color .15s ease, background .15s ease;
}
.drop-zone:hover, .drop-zone.active { border-color: var(--primary); background: var(--primary-light); }
.drop-zone.disabled { cursor: progress; opacity: .72; }
.upload-mark {
  flex: 0 0 38px; height: 38px; display: grid; place-items: center;
  border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card);
  color: var(--primary); font-size: 22px; line-height: 1;
}
.drop-copy { min-width: 0; display: flex; flex: 1; flex-direction: column; gap: 4px; }
.drop-copy strong { color: var(--text-primary); font-size: 13px; }
.drop-copy span { color: var(--text-muted); font-size: 11.5px; }
.upload-queue { margin-top: 10px; border: 1px solid var(--divider); }
.queue-head {
  height: 34px; display: flex; align-items: center; padding: 0 10px;
  border-bottom: 1px solid var(--divider); background: var(--bg-main); font-size: 11.5px;
}
.queue-head .text-btn { margin-left: auto; }
.queue-row { display: flex; gap: 10px; padding: 9px 10px; border-top: 1px solid var(--divider); }
.queue-row:first-of-type { border-top: 0; }
.queue-state {
  flex: 0 0 20px; height: 20px; display: grid; place-items: center; border-radius: 50%;
  background: var(--bg-sunken); color: var(--text-muted); font-size: 11px; font-weight: 800;
}
.queue-row.ok .queue-state { color: var(--success-text); background: var(--success-bg); }
.queue-row.partial .queue-state { color: var(--warning-text); background: var(--warning-bg); }
.queue-row.fail .queue-state { color: var(--error-text); background: var(--error-bg); }
.queue-main { min-width: 0; display: flex; flex: 1; flex-direction: column; gap: 2px; }
.queue-main > strong { overflow: hidden; color: var(--text-primary); font-size: 12px; text-overflow: ellipsis; white-space: nowrap; }
.queue-main > span { color: var(--text-muted); font-size: 11.5px; }
.queue-main > .queue-destination { color: var(--text-faint); font-size: 10.5px; }
.data-source-note { margin-top: 3px; color: var(--text-muted); font-size: 11px; }
.data-source-note span { display: inline-block; margin-left: 6px; padding: 1px 5px; border: 1px solid var(--border); }
.data-source-note .warn { color: var(--warning-text); }
.action-error { margin-top: 10px; padding: 9px 11px; background: var(--error-bg); color: var(--error-text); font-size: 12px; }
.retrieval-section { width: 100%; }
.graph-section, .mindmap-section, .eval-section { width: 100%; }
.retrieval-head { display: flex; align-items: flex-end; gap: 16px; }
.retrieval-head h3 { color: var(--text-primary); font-size: 14px; }
.retrieval-head span { display: block; margin-top: 3px; color: var(--text-muted); font-size: 11.5px; }
.sample-questions { display: flex; align-items: center; flex-wrap: wrap; gap: 6px; margin-top: 10px; }
.sample-questions-label { flex: none; color: var(--text-faint); font-size: 10.5px; }
.sample-questions button {
  max-width: 260px; overflow: hidden; padding: 3px 10px; border: 1px solid var(--border); border-radius: 999px;
  background: var(--bg-card); color: var(--text-muted); cursor: pointer; font: inherit; font-size: 11px;
  text-overflow: ellipsis; white-space: nowrap;
}
.sample-questions button:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-light); }
.sample-questions button:disabled { cursor: not-allowed; opacity: .55; }
.retrieval-form { margin-top: 14px; padding-bottom: 14px; border-bottom: 1px solid var(--divider); }
.retrieval-form > label { display: block; margin-bottom: 6px; color: var(--text-primary); font-size: 11.5px; font-weight: 700; }
.retrieval-input-row { display: flex; align-items: stretch; gap: 8px; }
.retrieval-input-row textarea {
  width: 100%; min-height: 58px; resize: vertical; padding: 9px 10px; border: 1px solid var(--border);
  border-radius: 6px; outline: 0; background: var(--bg-card); color: var(--text-primary);
  font: inherit; font-size: 12.5px; line-height: 1.55;
}
.retrieval-input-row textarea:focus { border-color: var(--primary); box-shadow: var(--ring); }
.retrieval-input-row .primary-btn { flex: 0 0 auto; height: auto; min-height: 38px; }
.retrieval-form > span { display: block; margin-top: 5px; color: var(--text-faint); font-size: 10.5px; }
.retrieval-warning {
  margin-top: 12px; padding: 8px 10px; border-left: 3px solid var(--warning-text);
  background: var(--warning-bg); color: var(--warning-text); font-size: 11.5px;
}
.retrieval-summary { display: flex; align-items: baseline; gap: 5px; margin-top: 14px; color: var(--text-regular); font-size: 12px; }
.retrieval-summary > strong { color: var(--text-primary); font-size: 18px; font-variant-numeric: tabular-nums; }
.retrieval-summary > span { margin-left: 7px; color: var(--success-text); font-size: 10.5px; }
.retrieval-summary > span.degraded { color: var(--warning-text); }
.retrieval-state {
  min-height: 180px; display: flex; align-items: center; justify-content: center; flex-direction: column; gap: 7px;
  border-top: 1px solid var(--divider); color: var(--text-muted); text-align: center; font-size: 12px;
}
.retrieval-state.initial { margin-top: 14px; border-top: 0; background: var(--bg-main); }
.retrieval-state strong { color: var(--text-primary); font-size: 14px; }
.retrieval-state span { max-width: 520px; line-height: 1.6; }
.hit-list { margin-top: 10px; border-top: 1px solid var(--border); border-bottom: 1px solid var(--border); }
.hit-row { display: grid; grid-template-columns: 28px minmax(0, 1fr); gap: 10px; padding: 13px 8px; border-top: 1px solid var(--divider); }
.hit-row:first-child { border-top: 0; }
.hit-row:hover { background: var(--bg-hover); }
.hit-rank {
  width: 24px; height: 24px; display: grid; place-items: center; border: 1px solid var(--border);
  border-radius: 50%; color: var(--text-muted); font-size: 10.5px; font-weight: 700; font-variant-numeric: tabular-nums;
}
.hit-main { min-width: 0; }
.hit-title-line { display: flex; align-items: baseline; gap: 10px; }
.hit-title-line > strong { min-width: 0; overflow: hidden; color: var(--text-primary); font-size: 12.5px; text-overflow: ellipsis; white-space: nowrap; }
.hit-location { margin-top: 3px; color: var(--text-faint); font-size: 10.5px; }
.hit-governance { display: flex; align-items: center; flex-wrap: wrap; gap: 5px; margin-top: 7px; }
.hit-governance > span {
  padding: 1px 6px; border: 1px solid var(--border); border-radius: 999px;
  color: var(--text-muted); background: var(--bg-main); font-size: 10px; font-weight: 550;
}
.hit-governance > .domain-tag { color: var(--primary); background: var(--primary-light); border-color: rgba(var(--primary-rgb), .22); }
.hit-main p { margin-top: 7px; color: var(--text-regular); font-size: 12px; line-height: 1.7; white-space: pre-wrap; overflow-wrap: anywhere; }
.hit-actions { display: flex; align-items: center; gap: 12px; margin-top: 8px; }
.hit-actions a { text-decoration: none; }
.hit-actions a:hover { text-decoration: underline; }
.hit-open-error { margin-top: 8px; padding: 7px 9px; background: var(--error-bg); color: var(--error-text); font-size: 11px; }
.hit-original {
  max-height: 320px; margin: 8px 0 0; padding: 10px 12px; overflow: auto; white-space: pre-wrap;
  border: 1px solid var(--border); border-left: 3px solid var(--primary); background: var(--bg-card);
  color: var(--text-regular); font: 12px/1.7 var(--font-sans); overflow-wrap: anywhere;
}
.library-section { margin-top: 22px; }
.doc-lineage:empty { display: none; }
.folder-create-box { width: min(460px, calc(100% - 32px)); }
.folder-create-box > label { display: block; margin-top: 12px; }
.folder-create-box > label > span { display: block; margin-bottom: 5px; color: var(--text-primary); font-size: 11.5px; font-weight: 650; }
.folder-create-box input, .folder-create-box select { width: 100%; }
.folder-create-box input { height: 34px; padding: 0 9px; border: 1px solid var(--border); border-radius: 6px; outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px; }
.folder-create-box input:focus { border-color: var(--primary); box-shadow: var(--ring); }
.library-head { display: flex; align-items: flex-end; gap: 20px; }
.library-head h3 { color: var(--text-primary); font-size: 14px; }
.library-head > div > span { display: block; margin-top: 3px; color: var(--text-muted); font-size: 11.5px; }
.library-tools { margin-left: auto; display: flex; align-items: center; gap: 8px; }
.search-box { position: relative; width: min(330px, 38vw); }
.search-box input {
  width: 100%; height: 32px; padding: 0 30px 0 10px; border: 1px solid var(--border);
  border-radius: 6px; outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px;
}
.search-box input:focus { border-color: var(--primary); box-shadow: var(--ring); }
.search-box button {
  position: absolute; right: 4px; top: 4px; width: 24px; height: 24px;
  border: 0; background: transparent; color: var(--text-muted); cursor: pointer;
}
.filter-bar { display: flex; gap: 2px; margin: 12px 0 8px; border-bottom: 1px solid var(--divider); }
.filter-bar button {
  height: 30px; padding: 0 10px; border: 0; border-bottom: 2px solid transparent;
  background: transparent; color: var(--text-muted); cursor: pointer; font-size: 12px;
}
.filter-bar button span { margin-left: 3px; color: var(--text-faint); font-variant-numeric: tabular-nums; }
.filter-bar button:hover { color: var(--text-primary); }
.filter-bar button.active { border-bottom-color: var(--primary); color: var(--primary); font-weight: 650; }
.doc-table { border: 1px solid var(--border); overflow: hidden; }
.doc-table-head, .doc-row {
  display: grid; grid-template-columns: minmax(180px, 1.7fr) 60px 70px 88px 96px minmax(180px, 1.35fr);
  align-items: center; column-gap: 12px;
}
.doc-table-head {
  min-height: 36px; padding: 0 12px; background: var(--bg-main); border-bottom: 1px solid var(--border);
  color: var(--text-muted); font-size: 11px; font-weight: 600;
}
.doc-row { min-height: 78px; padding: 10px 12px; border-top: 1px solid var(--divider); font-size: 12px; }
.doc-row:first-of-type { border-top: 0; }
.doc-row:hover { background: var(--bg-hover); }
.doc-name-cell { min-width: 0; display: flex; align-items: center; gap: 9px; }
.file-type {
  flex: 0 0 38px; height: 42px; display: grid; place-items: center;
  border: 1px solid var(--border); border-radius: 5px; background: var(--bg-main);
  color: var(--primary); font-size: 9px; font-weight: 800;
}
.doc-name-cell > div { min-width: 0; }
.doc-name-cell strong { display: block; overflow: hidden; color: var(--text-primary); text-overflow: ellipsis; white-space: nowrap; }
.doc-name-cell span:not(.file-type) { display: block; margin-top: 4px; color: var(--text-faint); font-size: 10.5px; }
.doc-governance { display: flex; flex-wrap: wrap; gap: 3px; margin-top: 5px; }
.doc-name-cell .doc-governance > span {
  display: inline-block; margin-top: 0; padding: 1px 5px; border: 1px solid var(--border); border-radius: 999px;
  color: var(--text-muted); background: var(--bg-main); font-size: 9.5px; line-height: 1.45;
}
.doc-name-cell .doc-governance > .domain-tag { color: var(--primary); background: var(--primary-light); border-color: rgba(var(--primary-rgb), .2); }
.doc-name-cell .doc-governance > .effective-tag { color: var(--text-regular); }
.doc-row > span { color: var(--text-muted); font-variant-numeric: tabular-nums; }
.doc-type { color: var(--text-regular) !important; font-weight: 600; }
.doc-status-cell { min-width: 0; align-self: stretch; display: flex; flex-direction: column; justify-content: center; }
.status-line { display: flex; align-items: center; gap: 6px; }
.status-line strong { color: var(--text-primary); font-size: 11.5px; }
.quality-badge {
  max-width: 92px; overflow: hidden; padding: 1px 5px; border: 1px solid var(--border); border-radius: 999px;
  color: var(--text-muted); background: var(--bg-main); font-size: 9.5px; font-weight: 650; text-overflow: ellipsis; white-space: nowrap;
}
.quality-badge.good { color: var(--success-text); background: var(--success-bg); border-color: var(--success); }
.quality-badge.warning { color: var(--warning-text); background: var(--warning-bg); border-color: var(--warning-text); }
.quality-badge.danger { color: var(--error-text); background: var(--error-bg); border-color: var(--error-ring); }
.quality-badge.processing { color: var(--primary); background: var(--primary-light); border-color: rgba(var(--primary-rgb), .25); }
.revision-tag { color: var(--primary) !important; border-color: rgba(var(--primary-rgb), .24) !important; background: var(--primary-light) !important; }
.status-dot { width: 7px; height: 7px; border-radius: 50%; background: var(--text-faint); }
.doc-row.ready .status-dot { background: var(--success); }
.doc-row.processing .status-dot { background: var(--primary); box-shadow: 0 0 0 3px var(--primary-light); }
.doc-row.partial .status-dot { background: var(--warning-text); }
.doc-row.failed .status-dot { background: var(--error-text); }
.doc-row.disabled { opacity: .7; background: var(--bg-main); }
.doc-row.disabled .status-dot { background: var(--text-faint); box-shadow: none; }
.status-hint { margin-top: 3px; overflow: hidden; color: var(--text-muted); font-size: 10.5px; text-overflow: ellipsis; white-space: nowrap; }
.status-hint.notice { color: var(--warning-text); }
.doc-row.failed .status-hint { color: var(--error-text); }
.doc-row.partial .status-hint { color: var(--warning-text); }
.row-actions { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; margin-top: 5px; }
.doc-move { min-width: 0; display: inline-flex; align-items: center; gap: 5px; color: var(--text-faint); font-size: 10px; }
.doc-folder-select { max-width: 150px; height: 26px; padding: 0 24px 0 7px; border: 1px solid var(--border); border-radius: 5px; background: var(--bg-card); color: var(--text-regular); font: 10.5px var(--font-sans); }
.doc-folder-select:focus { border-color: var(--primary); outline: 0; box-shadow: var(--ring); }
.moving-note { color: var(--primary); font-size: 10.5px; white-space: nowrap; }
.text-btn { padding: 0; border: 0; background: transparent; color: var(--primary); cursor: pointer; font-size: 11.5px; }
.text-btn:hover { text-decoration: underline; }
.text-btn.danger { color: var(--error-text); }
.text-btn:disabled { cursor: not-allowed; opacity: .5; text-decoration: none; }
.list-state {
  min-height: 190px; display: flex; align-items: center; justify-content: center; flex-direction: column; gap: 7px;
  border: 1px solid var(--border); color: var(--text-muted); text-align: center; font-size: 12px;
}
.list-state strong { color: var(--text-primary); font-size: 14px; }
.list-state span { max-width: 520px; line-height: 1.6; }
.list-state.error strong, .list-state.error span { color: var(--error-text); }
.primary-btn, .secondary-btn, .danger-btn, .icon-btn {
  height: 32px; border: 1px solid var(--border); border-radius: 6px; cursor: pointer; font: inherit; font-size: 12px;
}
.primary-btn { padding: 0 13px; border-color: var(--primary); background: var(--primary); color: #fff; }
.primary-btn:hover { background: var(--primary-hover); }
.upload-action { flex: 0 0 auto; display: inline-flex; align-items: center; pointer-events: none; }
.secondary-btn { padding: 0 13px; background: var(--bg-card); color: var(--text-regular); }
.secondary-btn:hover, .icon-btn:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-light); }
.danger-btn { padding: 0 13px; border-color: var(--error-ring); background: var(--error-text); color: #fff; }
.icon-btn { width: 32px; padding: 0; background: var(--bg-card); color: var(--text-regular); font-size: 17px; }
button:disabled { cursor: not-allowed; opacity: .55; }
.confirm-mask { position: absolute; inset: 0; z-index: 2; display: grid; place-items: center; background: rgba(16, 22, 43, .42); }
.confirm-box { width: min(420px, calc(100% - 32px)); padding: 18px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg-card); box-shadow: var(--shadow-lg); }
.confirm-box h3 { color: var(--text-primary); font-size: 15px; }
.confirm-box p { margin-top: 8px; color: var(--text-regular); font-size: 12.5px; line-height: 1.7; }
.create-box label { display: block; margin-top: 12px; }
.create-box label span { display: block; margin-bottom: 5px; color: var(--text-primary); font-size: 11.5px; font-weight: 650; }
.create-box input {
  width: 100%; height: 34px; padding: 0 9px; border: 1px solid var(--border); border-radius: 6px;
  outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px;
}
.grant-box { width: min(720px, calc(100% - 32px)); }
.grant-form { display: grid; grid-template-columns: 112px minmax(150px, 1fr) 104px auto; align-items: end; gap: 8px; margin-top: 14px; }
.grant-form label span { display: block; margin-bottom: 5px; color: var(--text-primary); font-size: 11.5px; font-weight: 650; }
.grant-form input, .grant-form select {
  width: 100%; height: 34px; padding: 0 9px; border: 1px solid var(--border); border-radius: 6px;
  outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px;
}
.grant-form input:focus, .grant-form select:focus { border-color: var(--primary); box-shadow: var(--ring); }
.role-picker { margin-top: 12px; padding: 12px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-page); }
.role-picker-head { display: flex; align-items: flex-start; gap: 12px; margin-bottom: 9px; }
.role-picker-head > div { min-width: 0; display: flex; flex: 1; flex-direction: column; gap: 2px; }
.role-picker-head strong { color: var(--text-primary); font-size: 12px; }
.role-picker-head span { color: var(--text-muted); font-size: 10.5px; }
.role-picker-head > b { flex: none; color: var(--primary); font-size: 11px; font-variant-numeric: tabular-nums; }
.role-picker-tools { display: flex; align-items: center; gap: 10px; }
.role-picker-tools label { min-width: 180px; flex: 1; }
.role-picker-tools input {
  width: 100%; height: 34px; padding: 0 9px; border: 1px solid var(--border); border-radius: 6px;
  outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px;
}
.role-batch { flex: none; height: 34px; white-space: nowrap; }
.selected-roles { display: flex; flex-wrap: wrap; gap: 5px; max-height: 76px; margin-top: 8px; overflow: auto; }
.selected-roles button { max-width: 210px; height: 25px; display: inline-flex; align-items: center; gap: 5px; padding: 0 7px; border: 1px solid rgba(var(--primary-rgb), .22); border-radius: 999px; background: var(--primary-light); color: var(--primary); cursor: pointer; font: inherit; font-size: 10.5px; }
.selected-roles button span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.selected-roles button b { flex: none; font-size: 13px; font-weight: 500; }
.role-options { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 6px; max-height: 230px; margin-top: 10px; overflow: auto; }
.role-option { display: flex; align-items: center; gap: 9px; min-width: 0; padding: 8px 10px; border: 1px solid var(--divider); border-radius: 6px; background: var(--bg-card); cursor: pointer; }
.role-option:has(input:checked) { border-color: var(--primary); background: var(--primary-light); }
.role-option input { width: 15px; height: 15px; flex: none; accent-color: var(--primary); }
.role-option span { min-width: 0; }
.role-option strong, .role-option small { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.role-option strong { color: var(--text-primary); font-size: 12px; }
.role-option small { margin-top: 2px; color: var(--text-muted); font-size: 10.5px; }
.role-save { width: 100%; margin-top: 10px; }
.grant-feedback { margin-top: 10px; padding: 8px 10px; border-radius: 6px; background: var(--success-bg); color: var(--success-text); font-size: 12px; line-height: 1.6; }
.grant-feedback.error { background: var(--warning-bg); color: var(--warning-text); }
.grant-list { min-height: 82px; max-height: 230px; margin-top: 14px; overflow: auto; border-top: 1px solid var(--divider); }
.grant-row { min-height: 40px; display: grid; grid-template-columns: 50px 1fr 64px auto; align-items: center; gap: 8px; border-bottom: 1px solid var(--divider); font-size: 12px; }
.grant-row > span { color: var(--text-muted); }
.grant-kind { color: var(--primary) !important; }
.grant-empty { padding: 26px 8px; color: var(--text-muted); text-align: center; font-size: 12px; }
.metadata-box { width: min(760px, calc(100% - 32px)); max-height: min(820px, calc(100% - 32px)); overflow: auto; }
.metadata-box > p { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.metadata-form { display: grid; grid-template-columns: 1.2fr 1fr 1fr; gap: 10px; margin-top: 14px; }
.metadata-form label span { display: block; margin-bottom: 5px; color: var(--text-primary); font-size: 11.5px; font-weight: 650; }
.metadata-form input {
  width: 100%; height: 34px; min-width: 0; padding: 0 9px; border: 1px solid var(--border); border-radius: 6px;
  outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px;
}
.metadata-form input:focus { border-color: var(--primary); box-shadow: var(--ring); }
.metadata-wide { grid-column: 1 / -1; }
.relation-editor { margin-top: 16px; padding-top: 14px; border-top: 1px solid var(--divider); }
.relation-editor > header { display: flex; align-items: flex-start; gap: 12px; }
.relation-editor > header > div { min-width: 0; }
.relation-editor h4 { margin: 0; color: var(--text-primary); font-size: 13px; }
.relation-editor header span { display: block; margin-top: 2px; color: var(--text-muted); font-size: 11px; }
.relation-editor header .text-btn { margin-left: auto; white-space: nowrap; }
.relation-search { display: block; margin-top: 10px; }
.relation-search input {
  width: 100%; height: 34px; padding: 0 9px; border: 1px solid var(--border); border-radius: 6px;
  outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px;
}
.relation-search input:focus { border-color: var(--primary); box-shadow: var(--ring); }
.relation-summary { display: flex; align-items: center; gap: 7px; margin: 8px 0 6px; font-size: 11px; }
.relation-summary strong { color: var(--primary); }
.relation-summary span { color: var(--text-faint); }
.relation-options { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 6px; max-height: 230px; overflow: auto; }
.relation-option { display: flex; align-items: center; gap: 9px; min-width: 0; padding: 8px 9px; border: 1px solid var(--divider); border-radius: 6px; background: var(--bg-card); cursor: pointer; }
.relation-option:has(input:checked) { border-color: var(--primary); background: var(--primary-light); }
.relation-option input { width: 15px; height: 15px; flex: none; accent-color: var(--primary); }
.relation-option span { min-width: 0; }
.relation-option strong, .relation-option small { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.relation-option strong { color: var(--text-primary); font-size: 11.5px; }
.relation-option small { margin-top: 2px; color: var(--text-muted); font-size: 10px; }
.relation-state { margin-top: 10px; padding: 18px 10px; border: 1px solid var(--divider); color: var(--text-muted); text-align: center; font-size: 11.5px; }
.relation-state.error { color: var(--error-text); background: var(--error-bg); }
.inferred-relations { display: flex; align-items: center; flex-wrap: wrap; gap: 5px; margin-top: 10px; }
.inferred-relations strong { width: 100%; color: var(--text-muted); font-size: 10.5px; }
.inferred-relations span { padding: 2px 6px; border: 1px solid var(--border); border-radius: 5px; background: var(--bg-main); color: var(--text-muted); font-size: 10px; }
.create-box input:focus { border-color: var(--primary); box-shadow: var(--ring); }
.confirm-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 18px; }
.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
/* 文档表中间档：面板内容宽 < 文档表网格最小宽（≈800px）时操作列被裁 —— 820 之上先切卡片堆叠 */
@media (max-width: 1130px) {
  .doc-table-head { display: none; }
  .doc-table { border: 0; overflow: visible; }
  .doc-row {
    grid-template-columns: 1fr 1fr; gap: 8px 14px; margin-bottom: 8px; padding: 12px;
    border: 1px solid var(--border); min-height: 0;
  }
  .doc-row:first-of-type { border-top: 1px solid var(--border); }
  .doc-name-cell, .doc-status-cell { grid-column: 1 / -1; }
  .doc-row > span::before { content: attr(data-label) ' · '; color: var(--text-faint); }
}
@media (max-width: 820px) {
  .kbp-mask { padding: 0; }
  .kbp { width: 100%; height: 100%; border: 0; border-radius: 0; }
  .kbp-body { padding: 14px; }
  .space-section { align-items: stretch; flex-direction: column; }
  .space-section select { width: 100%; min-width: 0; }
  .space-actions { width: 100%; margin-left: 0; }
  .workbench-tabs { margin-bottom: 14px; }
  .folder-workbench { grid-template-columns: 1fr; }
  .folder-tree { position: static; max-height: 220px; }
  .folder-breadcrumb { align-items: flex-start; flex-wrap: wrap; }
  .breadcrumb-path { order: 3; width: 100%; }
  .folder-breadcrumb small { margin-left: auto; }
  .folder-commands { margin-left: 0; }
  .upload-destination { grid-template-columns: auto minmax(0, 1fr); }
  .upload-destination > span { grid-column: 1 / -1; }
  .drop-zone { align-items: flex-start; flex-wrap: wrap; }
  .drop-zone .primary-btn { margin-left: 52px; }
  .library-head { align-items: stretch; flex-direction: column; gap: 10px; }
  .library-tools { width: 100%; margin-left: 0; }
  .search-box { width: 100%; }
  .filter-bar { overflow-x: auto; }
  .retrieval-input-row { flex-direction: column; }
  .retrieval-input-row .primary-btn { height: 36px; }
  .hit-row { padding-right: 2px; padding-left: 2px; }
  .hit-title-line { align-items: flex-start; flex-direction: column; gap: 3px; }
  .metadata-form { grid-template-columns: 1fr; }
  .metadata-wide { grid-column: auto; }
  .relation-options { grid-template-columns: 1fr; }
  .grant-form { grid-template-columns: 1fr 1fr; }
  .grant-target { grid-column: 1 / -1; }
  .role-picker-tools { align-items: flex-start; flex-wrap: wrap; }
  .role-picker-tools label { width: 100%; flex-basis: 100%; }
  .role-batch { flex: 1; }
  .role-options { grid-template-columns: 1fr; }
}
</style>
