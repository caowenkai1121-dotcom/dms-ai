<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import KbDocPreview from './KbDocPreview.vue'
import KbEval from './KbEval.vue'
import KbGraph from './KbGraph.vue'
import KbMindmap from './KbMindmap.vue'
import { ingestUploadState, isActiveIngest, isTerminalIngest } from './ingest-state'

interface DsTable { sheet: string; table: string; rows: number }
interface Ds { ds_id: string; schema: string; tables: DsTable[]; skipped: string[] }
// folder_path 与 directory_path 双字段并存是 legacy 兼容：folderPath() 优先 folder_path，
// 空则回退 directory_path，再退化到 folder_id 现拼路径（SearchHit 同口径）。
interface Doc {
  doc_id: string; name: string; mime: string; bytes: number
  status: string; error?: string | null; notice?: string | null; last_ingest_error?: string | null
  last_ingest_status?: string | null
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
  /** 同名替换提示（上传前按目标文件夹内同名检出，不阻断上传——服务端会用新文件替换同名旧文档）；终态后仍保留在行上 */
  warn?: string
  /** 上传阶段进度（0-100），仅 phase='upload' 时有值；进入解析或终态后清空 */
  progress?: number | null
  /** doing 的细分阶段：upload=网络传输中（行内百分比），parse=服务端秒回后后台解析中（轮询跟踪） */
  phase?: 'upload' | 'parse'
}
interface UploadPollTarget {
  rowId: number; docId: string; baselineUpdatedAt: string; deadline: number
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
// 关联文档上限：toggleRelatedDoc 的判定与界面文案共用一处，不各写 50
const MAX_RELATED = 50
// 上传队列上限：队列只增不清，批量上传大目录时行数封顶（保留最近 N 条）
const UPLOAD_QUEUE_MAX = 200
// 同时在传的文件数。此前是 1（`for` + `await` 串行），而服务端的入库许可有 8 个 ——
// 客户端一次只用一个位子，传得慢，还容易被「前几个还在后台解析」撞出 429。
// 取 4 是留余量：多用户同时传时服务端还有位子接得住，超出的在服务端排队而不是失败。
const UPLOAD_PARALLEL = 4
// 429 自动重试：服务端排队仍没轮到才会给 429，那时退避重来，别让用户手动一个个重试。
const UPLOAD_RETRY_MAX = 4
const UPLOAD_RETRY_BASE_MS = 1000
// 上传秒回后的轮询节奏：每 2s 重拉列表按 doc_id 对状态，5 分钟未落定即停（防无限轮询）
const UPLOAD_POLL_MS = 2000
const UPLOAD_POLL_TIMEOUT_MS = 5 * 60 * 1000
// 上传队列聚合卡的启用阈值：少于 10 个文件时逐行状态已足够清楚（Yuxi 双模式思路）
const UPLOAD_AGG_MIN = 10
// 模块级单例 formatter：文档行多时每行渲染都过 dateText，不能一格 new 一个
const DATE_FMT = new Intl.DateTimeFormat('zh-CN', {
  month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false,
})
// 扩展名集合（与 UPLOAD_ACCEPT 同一份清单）：文件夹选择器不认 accept，前端按它逐个预过滤
const UPLOAD_EXTS = new Set(UPLOAD_ACCEPT.split(',').map((ext) => ext.trim().toLowerCase()))
interface Grant {
  grantee_kind: 'login' | 'role' | 'dept'; grantee: string; grantee_name?: string | null
  perm: 'read' | 'write'
}
interface RoleOption { role_code: string; role_name: string }
/** 部门目录项（share_config v2 部门授权）：dept_id 是 t_department.department_id 的字符串形 */
interface DeptOption { dept_id: string; dept_name: string }
type Filter = 'all' | 'ready' | 'processing' | 'failed' | 'attention' | 'disabled'
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
// 删文件夹用自定义确认框（与删文档的 confirm-box 同款），不再用原生 window.confirm
const folderDeleteConfirm = ref(false)
const folderDeleteErr = ref('')
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
// 新建空间对话框的专属错误位：失败时对话框还开着，写 actionErr 会被对话框盖住看不到
const createErr = ref('')
const newSpaceName = ref('')
const newSpaceId = ref('')
const grantOpen = ref(false)
const grants = ref<Grant[]>([])
const grantsLoading = ref(false)
const granting = ref(false)
const revokingGrant = ref('')
const grantKind = ref<'login' | 'role' | 'dept'>('login')
const grantTarget = ref('')
const grantPerm = ref<'read' | 'write'>('read')
const roleOptions = ref<RoleOption[]>([])
const roleSearch = ref('')
const selectedRoleCodes = ref<string[]>([])
// 部门授权（share_config v2）：目录由 space_grants 随授权清单同包下发，dept_id 即提交的 grantee
const deptOptions = ref<DeptOption[]>([])
const grantDeptId = ref('')
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
// 文档列表（Yuxi 对齐）：客户端分页 / 复选框多选 / 行内 ⋯ 菜单 / 筛选下拉 / 移动对话框
const page = ref(0)
const pageSize = ref(20)
const checkedIds = ref<string[]>([])
const menuDocId = ref('')
const filterMenuOpen = ref(false)
const moveDocTarget = ref<Doc | null>(null)
const moveTargetFolderId = ref('')
const batchBusy = ref(false)
const batchDeleteOpen = ref(false)
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
  return d.enabled === false ? '已停用' : d.last_ingest_error ? '处理失败' : statusText(d.status)
}
function statusHint(d: Doc): string {
  if (d.enabled === false) return '不参与知识检索，原文件与索引仍保留'
  if (d.last_ingest_error) return d.last_ingest_error
  if (d.error) return d.error
  if (d.notice) return d.notice
  if (d.status === PARTIAL) return '文本已入库，向量索引尚未完成'
  if (d.status === 'pending' || d.status === 'parsing') return '处理完成后即可参与问答'
  if (d.status === OK) return '解析与向量索引均已完成'
  return `服务端状态：${d.status || '空'}`
}
function uploadState(source: IngestOutcome): UploadRow['state'] { return ingestUploadState(source) }
function updateUpload(id: number, patch: Partial<UploadRow>) {
  const row = uploads.value.find((item) => item.id === id)
  if (row) Object.assign(row, patch)
}
/** 入队统一走这里：行数封顶（队列只增不清，批量上传大目录时保留最近 N 条）。 */
function pushUpload(row: UploadRow) {
  uploads.value.unshift(row)
  if (uploads.value.length > UPLOAD_QUEUE_MAX) uploads.value.length = UPLOAD_QUEUE_MAX
}
function extOf(name: string): string {
  const ext = name.split('.').pop()
  return ext && ext !== name ? ext.toUpperCase().slice(0, 5) : 'FILE'
}
function dateText(value?: string): string {
  if (!value) return '-'
  const d = new Date(value)
  if (Number.isNaN(d.getTime())) return value.slice(0, 16).replace('T', ' ')
  return DATE_FMT.format(d)
}
/** 列表时间列固定 MM-DD HH:mm（DATE_FMT 是 zh-CN 斜杠风格，检索命中等处仍在用，不动它）。 */
function docTimeText(value?: string): string {
  if (!value) return '-'
  const d = new Date(value)
  if (Number.isNaN(d.getTime())) return value.slice(0, 16).replace('T', ' ')
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}
/** 内容量列：切片数为主、页数有的就带；datasource 表格文档按行数/表数说话更直观。 */
function contentText(d: Doc): string {
  if (d.datasource?.tables?.length) {
    const rows = d.datasource.tables.reduce((sum, t) => sum + (t.rows || 0), 0)
    return `${rows} 行 · ${d.datasource.tables.length} 表`
  }
  return `${d.page_count ? `${d.page_count} 页 · ` : ''}${d.chunk_count ?? 0} 切片`
}
/** 文件名悬浮提示：目录与上传人不再占行（旧版第二行 lineage 的信息收口到这里）。 */
function nameTitle(d: Doc): string {
  const parts = [d.name, `目录：${folderPath(d) || '根目录 / 未分类'}`]
  if (d.uploaded_by) parts.push(`上传人：${d.uploaded_by}`)
  return parts.join('\n')
}
function hitLocation(hit: SearchHit): string {
  const parts: string[] = []
  const directory = folderPath(hit)
  // 与文档行的「目录：X」同款分隔符
  if (directory) parts.push(`目录：${directory}`)
  const heading = Array.isArray(hit.heading_path)
    ? hit.heading_path.filter(Boolean).join(' / ')
    : String(hit.heading_path ?? '').trim()
  if (heading) parts.push(heading)
  if (typeof hit.page === 'number') parts.push(`第 ${hit.page} 页`)
  return parts.join(' · ') || '未标注章节或页码'
}
/** 「需处理」判定：只有真要用户动手的才算——失败/空内容/待向量/日期失效/带失败语义的提示。
 *  「第 N 页无文本层已用 OCR 补」这类**系统已自动消化**的提示不算（留痕展示即可，没什么可处理的）。 */
function attentionInfo(d: Doc): { actionable: boolean; reason: string } | null {
  if (d.enabled === false) return null
  if (d.last_ingest_error) return { actionable: true, reason: d.last_ingest_error.trim() }
  const level = d.quality?.level
  if (!level || level === 'processing' || level === 'good') return null
  const reason = (d.error || d.notice || d.quality?.label || '').trim()
  // 已自动消化的纯告知提示（OCR 补页等）：可检索、无动作项，不进「需处理」
  const digested = level === 'warning' && d.status === OK && (d.chunk_count ?? 0) > 0
    && !!d.notice && !/失败|请|重试|不可用|缺失/.test(d.notice)
    && !['待生效', '已失效'].includes(d.quality?.label ?? '')
  if (digested) return null
  const actionable = d.status === 'failed' || level === 'danger' || d.status === PARTIAL
    || /失败|重试|请重新/.test(reason)
  return { actionable, reason: reason || '状态待确认' }
}
function displayState(d: Doc): 'ready' | 'processing' | 'attention' | 'disabled' {
  if (d.enabled === false) return 'disabled'
  if (d.quality?.level === 'processing') return 'processing'
  if (attentionInfo(d)) return 'attention'
  return 'ready'
}
type PillState = 'ready' | 'processing' | 'attention' | 'disabled' | 'failed'
/** 状态 pill 配色态：失败从「需处理」里单独染红，其余沿用 displayState 口径。 */
function pillState(d: Doc): PillState {
  if (d.enabled === false) return 'disabled'
  if (d.status === 'failed' || d.last_ingest_error) return 'failed'
  return displayState(d)
}
/** pill 文案：需处理档直接亮服务端质量标签（待向量化/待生效/无可检索内容…），不笼统说「可检索」。 */
function pillText(d: Doc): string {
  return pillState(d) === 'attention' ? (d.quality?.label ?? '需处理') : docStatusText(d)
}
/** pill 可点 = 主操作（重新处理）：失败/空内容/待向量/采集失败这类「处理动作能解决」的档。 */
function pillClickable(d: Doc): boolean {
  return canWrite.value && !!attentionInfo(d)?.actionable
}
/** 各状态档的「该怎么处理」指引（pill hover 与原因行同源）：每个非就绪档都要有明确动作。 */
function statusGuidance(d: Doc): string {
  const writable = canWrite.value
  if (d.status === 'failed' || d.last_ingest_error) {
    return writable ? '点击本状态重新处理；反复失败请检查原文件后重新上传' : '处理失败，请联系空间管理员重新处理'
  }
  if (d.status === PARTIAL) {
    return writable
      ? '文本已入库（可关键词检索），向量索引未建、语义召回偏弱：点击本状态补建，或等系统自动补'
      : '向量索引未建、语义召回偏弱，请联系空间管理员重新处理'
  }
  switch (d.quality?.label ?? '') {
    case '无可检索内容': return '文档没有解析出文本：扫描件请确认 OCR 可用后重新处理，或检查原文件是否损坏'
    case '待生效': return '有效期未开始：⋯ 菜单「元数据」里调整生效日期，或到期自动生效'
    case '已失效': return '有效期已过、不参与检索：⋯ 菜单「元数据」里调整或清除失效日期'
    case '状态待确认': return '服务端状态未能确认：点击本状态重新处理一次试试'
    default:
      if (/失败|请重新|重试|不可用/.test(d.last_ingest_error || d.error || d.notice || '')) {
        return writable ? '点击本状态重新处理；仍失败请按提示调整后重新上传' : '请联系空间管理员处理'
      }
      return ''
  }
}
/** pill 悬浮说明：原因直接可见 + 该怎么处理；处理中明示「点了也没用」。 */
function pillTitle(d: Doc): string {
  const state = pillState(d)
  if (state === 'processing') return '正在处理，完成后自动转为可检索'
  const info = attentionInfo(d)
  if (info) return [info.reason || '未知原因', statusGuidance(d)].filter(Boolean).join('；')
  return [statusHint(d), d.quality?.label].filter(Boolean).join(' · ')
}
/** 文件名下的原因行：需处理文档亮原因（错误>提示>质量标签），hover 给完整处理指引；
 *  已消化的提示淡色留痕。 */
function issueText(d: Doc): string {
  return attentionInfo(d)?.reason ?? (d.last_ingest_error || d.notice || d.error || '')
}
/** 原因行的悬浮：原因 + 处理指引（与 pill hover 同一份）。 */
function issueTitle(d: Doc): string {
  return [issueText(d), statusGuidance(d)].filter(Boolean).join('；')
}
function dateInputValue(value?: string | null): string {
  return value ? value.slice(0, 10) : ''
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

/** 换空间的作用域状态复位：loadSpaces 自动换房/失败清空、changeSpace 三处共用，字段只维护一处
 *  （search/filter/actionErr 也在此复位 —— 旧关键词/筛选不许跨空间残留）。 */
function resetSpaceScopedState() {
  docs.value = []
  folders.value = []
  uploads.value = []
  busy.value = false
  search.value = ''
  filter.value = 'all'
  actionErr.value = ''
  collapsedFolderIds.value = []
  foldersErr.value = ''
  folderApiAvailable.value = null
  selectedFolderId.value = ''
  uploadFolderId.value = ''
  folderCreateOpen.value = false
  folderEditOpen.value = false
  folderDeleteConfirm.value = false
  folderDeleteErr.value = ''
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
  page.value = 0
  checkedIds.value = []
  menuDocId.value = ''
  filterMenuOpen.value = false
  moveDocTarget.value = null
  batchBusy.value = false
  batchDeleteOpen.value = false
  resetRetrieval()
  retrievalLoading.value = false
  sampleQuestions.value = []
  closeMetadata(true)
  closeGrants(true)
}

function governanceText(hit: SearchHit): string {
  // 与文档行同口径：截到日期，不拼完整 ISO
  if (hit.effective_from && hit.effective_to) return `${dateInputValue(hit.effective_from)} 至 ${dateInputValue(hit.effective_to)}`
  if (hit.effective_from) return `${dateInputValue(hit.effective_from)} 起生效`
  if (hit.effective_to) return `有效至 ${dateInputValue(hit.effective_to)}`
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
    // 延迟回收：0ms 在部分浏览器（Firefox）下载尚未开始即被回收，可能截断
    window.setTimeout(() => URL.revokeObjectURL(url), 1000)
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
  // 复位保存闸：上一份文档保存中打开新文档时，旧 finally 因 requestId 失效会跳过复位，
  // 不在这里清零的话新文档的保存按钮会被永久禁用
  metadataSaving.value = false
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
    metadataErr.value = ''
  } else if (metadataRelatedIds.value.length < MAX_RELATED) {
    metadataRelatedIds.value = [...metadataRelatedIds.value, docId]
    // 成功勾选后清掉旧的超限错误，不留滞
    metadataErr.value = ''
  } else {
    metadataErr.value = `关联文档最多 ${MAX_RELATED} 篇`
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
/** 写操作显隐闸：kb_manager（全局管理授权）+ 当前空间可写，两者齐备才露上传/重处理/删除等写按钮。
 *  隐藏只是体验对齐，安全闸在服务端各管理端点（fail-closed）。 */
const canWrite = computed(() => kbManager.value && !!currentSpace.value?.writable)
const switchingDisabled = computed(() => busy.value || creating.value || folderCreating.value || folderEditing.value
  || !!folderDeletingId.value || !!docMovingId.value || metadataSaving.value || granting.value
  || !!revokingGrant.value || !!deletingId.value || !!reprocessingId.value || !!stateChangingId.value
  || urlBusy.value || !!descGeneratingId.value || batchBusy.value)
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
/** 同名判定键：目标文件夹 + 文件名（与服务端 `find_by_name_in_folder` 同口径：精确匹配、大小写敏感；根目录/未分类的 folder_id 为空串） */
const sameNameKey = (name: string, folderId: string) => `${folderId}\x00${name}`
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
  grantDeptId.value = ''
  grantFeedback.value = ''
  grantFeedbackError.value = false
}
function closeSpaceCreate() {
  if (creating.value) return
  createOpen.value = false
  createErr.value = ''
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
  deptOptions.value = []
  grantBatchLimit.value = 100
  resetGrantDraft()
}
function grantName(g: Grant): string {
  if (g.grantee_kind === 'role') {
    const name = g.grantee_name || roleOptions.value.find((role) => role.role_code === g.grantee)?.role_name
    return name ? `${name} · ${g.grantee}` : g.grantee
  }
  if (g.grantee_kind === 'dept') {
    const name = g.grantee_name || deptOptions.value.find((dept) => dept.dept_id === g.grantee)?.dept_name
    return name ? `${name} · ${g.grantee}` : g.grantee
  }
  return g.grantee
}

const counts = computed(() => {
  // 单趟聚合：失败独立计数；「需处理」保留非失败的待向量/空内容/有效期等动作项。
  const c = { all: 0, ready: 0, processing: 0, failed: 0, attention: 0, disabled: 0 }
  for (const d of docs.value) {
    c.all++
    if (d.enabled !== false && (d.status === 'failed' || !!d.last_ingest_error)) c.failed++
    else c[displayState(d)]++
  }
  return c
})
const unfiledCount = computed(() => docs.value.filter((doc) => !doc.folder_id).length)
/** 已结束（成功/部分/失败）的上传行数：没有可清的行时「清除已结束」按钮禁用。 */
const uploadsDoneCount = computed(() => uploads.value.filter((u) => u.state !== 'doing').length)
const filters = computed<{ value: Filter; label: string; count: number }[]>(() => [
  { value: 'all', label: '全部', count: counts.value.all },
  { value: 'ready', label: '可检索', count: counts.value.ready },
  { value: 'processing', label: '处理中', count: counts.value.processing },
  { value: 'failed', label: '处理失败', count: counts.value.failed },
  { value: 'attention', label: '需处理', count: counts.value.attention },
  { value: 'disabled', label: '已停用', count: counts.value.disabled },
])
const visibleDocs = computed(() => {
  const needle = search.value.trim().toLocaleLowerCase()
  return docs.value.filter((d) => {
    if (selectedFolderId.value === '__unfiled__' && d.folder_id) return false
    if (selectedFolderId.value && selectedFolderId.value !== '__unfiled__' && d.folder_id !== selectedFolderId.value) return false
    const state: Filter = d.enabled !== false && (d.status === 'failed' || !!d.last_ingest_error) ? 'failed' : displayState(d)
    const inFilter = filter.value === 'all'
      || filter.value === state
    if (!inFilter) return false
    if (!needle) return true
    return [d.name, d.mime, d.status, d.last_ingest_error, d.error, d.notice, d.uploaded_by,
      d.business_domain, d.source_uri, d.document_family, d.document_revision, folderPath(d), ...(d.tags ?? [])]
      .some((v) => String(v ?? '').toLocaleLowerCase().includes(needle))
  })
})
// 客户端分页：数据本就不分页拉取，切片即可；筛选/搜索/目录/页大小变化时回到第一页
const pageCount = computed(() => Math.max(1, Math.ceil(visibleDocs.value.length / pageSize.value)))
const pagedDocs = computed(() =>
  visibleDocs.value.slice(page.value * pageSize.value, (page.value + 1) * pageSize.value))
watch([search, filter, selectedFolderId, pageSize], () => { page.value = 0 })
// 列表变短（删除/刷新）后页码可能越界：钳回最后一页
watch(pageCount, (count) => { if (page.value > count - 1) page.value = count - 1 })
// 刷新后已删文档的勾选项不能残留
watch(docs, (list) => {
  const alive = new Set(list.map((d) => d.doc_id))
  checkedIds.value = checkedIds.value.filter((id) => alive.has(id))
})
const checkedSet = computed(() => new Set(checkedIds.value))
const pageCheckedCount = computed(() => pagedDocs.value.filter((d) => checkedSet.value.has(d.doc_id)).length)
const pageAllChecked = computed(() => pagedDocs.value.length > 0 && pageCheckedCount.value === pagedDocs.value.length)
const currentFilterLabel = computed(() =>
  filters.value.find((item) => item.value === filter.value)?.label ?? '全部')
/** 上传队列聚合计数：≥UPLOAD_AGG_MIN 个文件时替代逐行扫读（Yuxi 双模式思路）。 */
const uploadAgg = computed(() => {
  const agg = { total: uploads.value.length, uploading: 0, parsing: 0, failed: 0 }
  for (const u of uploads.value) {
    if (u.state === 'fail') agg.failed++
    else if (u.state === 'doing' && u.phase === 'parse') agg.parsing++
    else if (u.state === 'doing') agg.uploading++
  }
  return agg
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
    resumeUploadPollForActiveDocs(space, docs.value)
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
  // 已知目录接口 404（folderApiAvailable===false）时不再多发一个注定失败的请求
  if (foldersEmbeddedInDocs === false && folderApiAvailable.value !== false) await loadFolders(space, requestId, epoch)
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
      clearUploadPolls()
      contextEpoch++
      assetsRequestId++
      retrievalRequestId++
      metadataRequestId++
      grantsRequestId++
      uploadRequestId++
      spaceId.value = next
      resetSpaceScopedState()
    }
    const requestEpoch = contextEpoch
    await loadKnowledgeAssets(next, requestEpoch)
    void loadSampleQuestions(next, requestEpoch)
    if (requestId === spacesRequestId && contextIsCurrent(requestEpoch, next) && currentSpace.value) {
      emit('space-change', { space_id: currentSpace.value.space_id, name: currentSpace.value.name })
    }
  } catch (e) {
    if (requestId !== spacesRequestId) return
    spacesErr.value = errorText(e)
    loading.value = false
    foldersLoading.value = false
  }
}

async function changeSpace() {
  clearUploadPolls()
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
  resetSpaceScopedState()
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
  createErr.value = ''
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
    createErr.value = `新建空间失败：${errorText(e)}`
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

function folderIsDescendant(candidate: Folder, ancestorId: string, byId?: Map<string, Folder>): boolean {
  const map = byId ?? new Map(folders.value.map((folder) => [folder.folder_id, folder]))
  const seen = new Set<string>()
  let current: Folder | undefined = candidate
  while (current && !seen.has(current.folder_id)) {
    if (current.parent_id === ancestorId) return true
    seen.add(current.folder_id)
    current = current.parent_id ? map.get(current.parent_id) : undefined
  }
  return false
}

const folderMoveTargets = computed(() => {
  const current = selectedFolder.value
  if (!current) return folderRows.value
  // byId 只建一次、闭包复用：逐行调 folderIsDescendant 各自重建 Map 是 O(n²)
  const byId = new Map(folders.value.map((folder) => [folder.folder_id, folder]))
  return folderRows.value.filter((row) => row.folder.folder_id !== current.folder_id
    && !folderIsDescendant(row.folder, current.folder_id, byId))
})

async function saveFolderEdit() {
  const folder = selectedFolder.value
  const name = folderEditName.value.trim()
  if (!folder || !name || folderEditing.value) return
  // 名称/父目录都没改：不发 POST 不整刷，直接关窗
  if (name === folder.name && (folderEditParentId.value || '') === (folder.parent_id || '')) {
    folderEditOpen.value = false
    return
  }
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
  folderDeleteErr.value = ''
  folderDeleteConfirm.value = true
}

function closeFolderDeleteConfirm() {
  if (folderDeletingId.value) return
  folderDeleteConfirm.value = false
  folderDeleteErr.value = ''
}

async function removeFolderConfirmed() {
  const folder = selectedFolder.value
  if (!folder || folderDeletingId.value) return
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  folderDeletingId.value = folder.folder_id
  folderDeleteErr.value = ''
  try {
    const response = await fetch(`/api/kb/folder/${encodeURIComponent(folder.folder_id)}${spaceQuery(requestSpace)}`, {
      method: 'DELETE', headers: headers(),
    })
    const data = await responseJson(response)
    if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
    if (!contextIsCurrent(requestEpoch, requestSpace)) return
    folderDeleteConfirm.value = false
    selectedFolderId.value = ''
    uploadFolderId.value = ''
    await loadKnowledgeAssets(requestSpace, requestEpoch)
  } catch (e) {
    if (contextIsCurrent(requestEpoch, requestSpace)) folderDeleteErr.value = `删除文件夹失败：${errorText(e)}`
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

/** 行内 ⋯ 菜单：同一时刻只开一个，且与筛选下拉互斥；document 级点击统一关闭。 */
function toggleRowMenu(docId: string) {
  menuDocId.value = menuDocId.value === docId ? '' : docId
  filterMenuOpen.value = false
}
function closeMenus() {
  menuDocId.value = ''
  filterMenuOpen.value = false
}

/** 「移动至…」改走对话框（旧版行内下拉在 64px 操作列里放不下）：选择后仍调既有 moveDoc。 */
function openMoveDialog(d: Doc) {
  moveDocTarget.value = d
  moveTargetFolderId.value = d.folder_id || ''
}
function closeMoveDialog() {
  if (docMovingId.value) return
  moveDocTarget.value = null
}
async function confirmMoveDoc() {
  const d = moveDocTarget.value
  if (!d || docMovingId.value) return
  const folderId = moveTargetFolderId.value
  moveDocTarget.value = null
  await moveDoc(d, folderId)
}

function toggleCheck(docId: string, on: boolean) {
  checkedIds.value = on
    ? [...checkedIds.value, docId]
    : checkedIds.value.filter((id) => id !== docId)
}
function toggleCheckPage(on: boolean) {
  const pageIds = new Set(pagedDocs.value.map((d) => d.doc_id))
  checkedIds.value = on
    ? [...new Set([...checkedIds.value, ...pageIds])]
    : checkedIds.value.filter((id) => !pageIds.has(id))
}

/** 批量操作逐条走既有单文档端点（契约不变）：失败计数汇总提示，成功仍整刷空间。 */
async function batchReprocessChecked() {
  if (batchBusy.value || !canWrite.value) return
  const targets = docs.value.filter((d) => checkedSet.value.has(d.doc_id))
  if (!targets.length) return
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  batchBusy.value = true
  actionErr.value = ''
  let failed = 0
  try {
    for (const d of targets) {
      try {
        const response = await fetch(`/api/kb/doc/${encodeURIComponent(d.doc_id)}/reprocess`, {
          method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' }, body: '{}',
        })
        const data = await responseJson(response)
        if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
      } catch { failed++ }
      if (!contextIsCurrent(requestEpoch, requestSpace)) return
    }
    checkedIds.value = []
    if (failed) actionErr.value = `${failed} 份文档重新处理发起失败，其余已进入处理队列。`
    await loadSpaces(requestSpace)
  } finally {
    if (contextIsCurrent(requestEpoch, requestSpace)) batchBusy.value = false
  }
}

async function removeCheckedConfirmed() {
  if (batchBusy.value) return
  const targets = docs.value.filter((d) => checkedSet.value.has(d.doc_id))
  if (!targets.length) {
    batchDeleteOpen.value = false
    return
  }
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  batchBusy.value = true
  actionErr.value = ''
  let failed = 0
  try {
    for (const d of targets) {
      try {
        const resp = await fetch(`/api/kb/doc/${encodeURIComponent(d.doc_id)}${spaceQuery(requestSpace)}`, {
          method: 'DELETE', headers: headers(),
        })
        const data = await responseJson(resp)
        if (!resp.ok) throw new Error(data.error ?? `HTTP ${resp.status}`)
      } catch { failed++ }
      if (!contextIsCurrent(requestEpoch, requestSpace)) return
    }
    checkedIds.value = []
    batchDeleteOpen.value = false
    if (failed) actionErr.value = `${failed} 份文档删除失败，其余已删除。`
    await loadSpaces(requestSpace)
  } finally {
    if (contextIsCurrent(requestEpoch, requestSpace)) batchBusy.value = false
  }
}

interface UploadResponse { status: number; data: Record<string, any> }
/** XHR 版上传：fetch 拿不到 upload.onprogress，行内百分比只能靠 XHR。鉴权/表单字段与原 fetch 版一致。 */
function uploadViaXhr(file: File, space: string, folderId: string, onProgress: (pct: number) => void): Promise<UploadResponse> {
  const token = props.token?.trim()
  if (!token) {
    emit('auth-expired')
    return Promise.reject(new Error('登录会话已失效，请重新登录。'))
  }
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest()
    xhr.open('POST', '/api/kb/upload')
    xhr.setRequestHeader('Authorization', `Bearer ${token}`)
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable && e.total) onProgress(Math.min(100, Math.round((e.loaded / e.total) * 100)))
    }
    xhr.onload = () => {
      let data: Record<string, any> = {}
      try { data = xhr.responseText ? JSON.parse(xhr.responseText) : {} } catch { /* 非 JSON 错误体按空处理 */ }
      resolve({ status: xhr.status, data })
    }
    xhr.onerror = () => reject(new Error('网络连接失败，上传中断'))
    xhr.onabort = () => reject(new Error('上传已中断'))
    const form = new FormData()
    form.append('file', file, file.name)
    form.append('space_id', space)
    if (folderId) form.append('folder_id', folderId)
    xhr.send(form)
  })
}

/** 上传/文档详情的公共字段子集（秒回响应与列表行都按这个口径读）。 */
interface IngestOutcome {
  status?: string; chunk_count?: number | null; page_count?: number | null
  error?: string | null; notice?: string | null; last_ingest_error?: string | null
  updated_at?: string | null
}
/** 终态行的统一文案：与异步化之前同步处理的展示口径一致（状态 + 切片/页数 + 降级文案）。 */
function ingestOutcomeText(source: IngestOutcome): string {
  const parts = [source.last_ingest_error ? '处理失败' : statusText(source.status), `${source.chunk_count ?? 0} 个切片`]
  if (source.page_count) parts.push(`${source.page_count} 页`)
  if (source.last_ingest_error) parts.push(String(source.last_ingest_error))
  else if (source.error) parts.push(String(source.error))
  else if (source.notice) parts.push(String(source.notice))
  return parts.join(' · ')
}

const uploadPolls = new Map<string, Map<number, UploadPollTarget>>()
const resumedUploadPolls = new Map<string, number>()
let uploadPollTimer: number | undefined
let uploadPollRunning = false

function scheduleUploadPoll() {
  if (uploadPollTimer !== undefined || uploadPollRunning || (!uploadPolls.size && !resumedUploadPolls.size)) return
  uploadPollTimer = window.setTimeout(() => {
    uploadPollTimer = undefined
    void tickUploadPolls()
  }, UPLOAD_POLL_MS)
}

/** 同一空间的全部异步入库共用一次列表刷新，避免 N 份文档产生 N 个全量轮询。 */
async function tickUploadPolls() {
  if (uploadPollRunning) return
  uploadPollRunning = true
  try {
    const spaces = new Set([...uploadPolls.keys(), ...resumedUploadPolls.keys()])
    for (const space of spaces) {
      const targets = uploadPolls.get(space)
      if (space !== spaceId.value || (!targets?.size && !resumedUploadPolls.has(space))) {
        uploadPolls.delete(space)
        resumedUploadPolls.delete(space)
        continue
      }
      const epoch = contextEpoch
      try { await loadKnowledgeAssets(space, epoch) } catch { /* 刷新失败下轮再试 */ }
      if (!contextIsCurrent(epoch, space)) {
        uploadPolls.delete(space)
        resumedUploadPolls.delete(space)
        continue
      }
      const now = Date.now()
      for (const [rowId, target] of [...(targets ?? [])]) {
        if (now > target.deadline) {
          updateUpload(rowId, {
            state: 'partial', phase: undefined,
            msg: '后台处理超过 5 分钟未落定：可能仍在处理，请稍后刷新列表查看。',
          })
          targets?.delete(rowId)
          continue
        }
        const doc = docs.value.find((item) => item.doc_id === target.docId)
        const changed = !target.baselineUpdatedAt || (!!doc?.updated_at && doc.updated_at !== target.baselineUpdatedAt)
        if (doc && changed && isTerminalIngest(doc)) {
          updateUpload(rowId, {
            state: uploadState(doc), msg: ingestOutcomeText(doc),
            ds: doc.datasource ?? null, phase: undefined,
          })
          targets?.delete(rowId)
        }
      }
      if (!targets?.size) uploadPolls.delete(space)
      const resumedDeadline = resumedUploadPolls.get(space)
      if (resumedDeadline !== undefined
        && (now > resumedDeadline || !docs.value.some(isActiveIngest))) resumedUploadPolls.delete(space)
    }
  } finally {
    uploadPollRunning = false
    scheduleUploadPoll()
  }
}

function clearUploadPolls() {
  if (uploadPollTimer !== undefined) window.clearTimeout(uploadPollTimer)
  uploadPollTimer = undefined
  uploadPolls.clear()
  resumedUploadPolls.clear()
}

/** load 成功后从服务端真状态恢复轮询；已有登记不续期，避免僵尸任务无限刷新。 */
function resumeUploadPollForActiveDocs(space: string, list: Doc[]) {
  if (!contextIsCurrent(contextEpoch, space)) return
  if (!list.some(isActiveIngest)) {
    resumedUploadPolls.delete(space)
    return
  }
  if (!resumedUploadPolls.has(space)) {
    resumedUploadPolls.set(space, Date.now() + UPLOAD_POLL_TIMEOUT_MS)
  }
  scheduleUploadPoll()
}

function pollUploadDoc(rowId: number, docId: string, space: string, epoch: number, baselineUpdatedAt = '') {
  if (!contextIsCurrent(epoch, space)) return
  const targets = uploadPolls.get(space) ?? new Map<number, UploadPollTarget>()
  targets.set(rowId, { rowId, docId, baselineUpdatedAt, deadline: Date.now() + UPLOAD_POLL_TIMEOUT_MS })
  uploadPolls.set(space, targets)
  scheduleUploadPoll()
}

// route：按文件给出目标文件夹与目的地文案（文件夹层级上传用）；不传则全部进 uploadFolderId 当前选择
async function send(files: File[], route?: (file: File) => { folderId: string; destination: string } | undefined) {
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
  // 同名替换提示（裁决：重复上传＝替换原文件）：服务端按「同空间 + 同目录 + 同名」命中后用新内容
  // 原地重建既有 doc_id（引用它的会话/预览链接不失效），不会产生第二份同名文档。上传前按
  // 「目标文件夹内同名」（与服务端同口径：精确匹配、大小写敏感，根目录/未分类的 folder_id 为空串）
  // 检出并在队列行上提示，不阻断上传。集合随本批逐个登记：目录批量上传时同批先到的同名文件
  // 也算「已有」，后到的同样命中提示。
  const seenDocNames = new Set(docs.value.map((doc) => sameNameKey(doc.name, doc.folder_id || '')))
  // 记录是否发生过实际上传：全部预校验失败（无一发起请求）时不整刷空间
  let attempted = false
  busy.value = true
  actionErr.value = ''
  try {
    // ① 同步规划：预校验 + 建行 + 同名提示。这一趟不发任何请求，所以同名判定的顺序
    //    只取决于用户选文件的顺序，不受下面并发调度影响（串行时代它是隐式成立的，
    //    并发之后必须显式分成两段才还成立）。
    const jobs: Array<{ rowId: number; file: File; folder: string }> = []
    for (const file of files) {
      const routed = route?.(file)
      // route 显式给 '' 是「根目录」的意思，不能用 || 回退（?? 只认 null/undefined）
      const fileFolder = routed?.folderId ?? requestFolder
      const fileDestination = routed?.destination ?? destination
      // 层级上传时行名带相对路径，同名的多级文件才分得清谁是谁
      const displayName = routed ? (file.webkitRelativePath || file.name) : file.name
      // 前端预校验（逐个反馈，不中断队列）：超限/空文件直接落失败行，权威判定仍在服务端
      if (file.size > MAX_UPLOAD_BYTES || file.size === 0) {
        const why = file.size === 0
          ? '文件为空，未上传'
          : `超过单文件 ${MAX_UPLOAD_BYTES / 1024 / 1024}MB 上限（实际 ${(file.size / 1024 / 1024).toFixed(1)}MB），未上传`
        pushUpload({ id: ++uploadId, name: displayName, state: 'fail', msg: why, destination: fileDestination })
        continue
      }
      attempted = true
      const dupKey = sameNameKey(file.name, fileFolder)
      const warn = seenDocNames.has(dupKey)
        ? `已有同名文档《${file.name}》，本次上传将替换原文件`
        : undefined
      seenDocNames.add(dupKey)
      const row: UploadRow = {
        id: ++uploadId, name: displayName, state: 'doing',
        msg: jobs.length < UPLOAD_PARALLEL ? '等待上传' : '排队中',
        warn, destination: fileDestination, progress: 0, phase: 'upload',
      }
      pushUpload(row)
      jobs.push({ rowId: row.id, file, folder: fileFolder })
    }

    // ② 并发执行：固定宽度的取号器。队列里的文件是「等着」，不是「失败」——
    //    这条纪律两端都要成立，服务端闸满时也排队（kb_api.rs::upload_permit）。
    let cursor = 0
    const width = Math.min(UPLOAD_PARALLEL, jobs.length)
    await Promise.all(
      Array.from({ length: width }, async () => {
        while (cursor < jobs.length) {
          const job = jobs[cursor++]
          await sendOne(job.rowId, job.file, requestSpace, job.folder, requestEpoch)
        }
      }),
    )
  } finally {
    if (requestId === uploadRequestId) busy.value = false
    if (contextIsCurrent(requestEpoch, requestSpace) && attempted) await loadSpaces(requestSpace)
  }
}

// 单个文件的上传 + 结果收口。429（入库排队满/超时）不算失败：退避后重排，
// 重试用尽才落失败行 —— 「稍后重试」这件事不该甩给用户手动做。
async function sendOne(rowId: number, file: File, requestSpace: string, fileFolder: string, requestEpoch: number) {
  for (let attempt = 0; ; attempt++) {
    try {
      const { status, data } = await uploadViaXhr(file, requestSpace, fileFolder, (pct) => {
        updateUpload(rowId, { progress: pct, msg: pct >= 100 ? '等待服务端响应' : `上传中 ${pct}%` })
      })
      if (status === 401) emit('auth-expired')
      if (status === 429 && attempt < UPLOAD_RETRY_MAX) {
        // 服务端已经排过一轮队还是没轮到：说明真的在忙，退避后再来（1s、2s、4s…）
        const wait = UPLOAD_RETRY_BASE_MS * 2 ** attempt
        updateUpload(rowId, {
          msg: `服务端繁忙，${Math.round(wait / 1000)} 秒后自动重试（第 ${attempt + 1}/${UPLOAD_RETRY_MAX} 次）`,
          progress: 0,
        })
        await new Promise((resolve) => setTimeout(resolve, wait))
        continue
      }
      if (status < 200 || status >= 300) {
        // 失败原因必须说清：服务端 JSON 错误原文优先；网关/反代 HTML 或空体折叠成可行动文案
        const why = data.error ?? (status >= 500
          ? `服务暂时不可用（网关错误 ${status}），请稍后重试`
          : `请求未成功（HTTP ${status}），请重试`)
        updateUpload(rowId, { state: 'fail', msg: why, progress: null, phase: undefined })
        return
      }
      // 同名覆盖响应带回的是切换前的旧线上行（通常已 embedded），不能按旧终态
      // 立即收口；必须继续轮询到 updated_at 变化，才知道本次覆盖成功还是失败。
      if (!data.replaced && isTerminalIngest(data)) {
        // 终态（同步处理完/失败/带降级文案的 chunked）：按原口径落行；同名覆盖带「已覆盖旧版本」
        const coverNote = data.replaced ? ' · 已覆盖旧版本' : ''
        updateUpload(rowId, {
          state: uploadState(data), msg: ingestOutcomeText(data) + coverNote,
          ds: data.datasource ?? null, progress: null, phase: undefined,
        })
      } else {
        // 进行态（秒回 parsing / 无降级文案的 chunked）：行挂「解析中」，轮询跟踪到终态
        const docId = String(data.doc_id ?? data.id ?? '')
        updateUpload(rowId, { msg: '解析中…（后台建立索引）', progress: null, phase: 'parse' })
        if (docId) pollUploadDoc(rowId, docId, requestSpace, requestEpoch, String(data.updated_at ?? ''))
        else updateUpload(rowId, { state: 'partial', phase: undefined, msg: '已提交后台处理：服务端未返回文档标识，请稍后刷新列表查看结果。' })
      }
      return
    } catch (e) {
      updateUpload(rowId, { state: 'fail', msg: errorText(e), progress: null, phase: undefined })
      return
    }
  }
}

function onPick(e: Event) {
  const el = e.target as HTMLInputElement
  void send(Array.from(el.files ?? []))
  el.value = ''
}
// 拖放高亮用进入/离开计数：划过子元素（upload-mark 等）时 dragleave 频繁触发会闪烁
let dragDepth = 0
function onDragEnter(e: DragEvent) {
  e.preventDefault()
  dragDepth++
  dragging.value = true
}
function onDragLeave(e: DragEvent) {
  e.preventDefault()
  dragDepth = Math.max(0, dragDepth - 1)
  if (!dragDepth) dragging.value = false
}
function onDrop(e: DragEvent) {
  dragging.value = false
  dragDepth = 0
  // 拖入目录项时 files 会给 size 0 的假文件，误报「文件为空」：识别出来指路「上传文件夹」
  const items = Array.from(e.dataTransfer?.items ?? [])
  if (items.some((item) => item.webkitGetAsEntry?.()?.isDirectory)) {
    actionErr.value = '拖入的是文件夹：请改用下方「上传文件夹」按钮。'
    return
  }
  void send(Array.from(e.dataTransfer?.files ?? []))
}
function openFilePicker() {
  if (!busy.value) fileEl.value?.click()
}
function openDirPicker() {
  if (!busy.value) dirEl.value?.click()
}
// 上传文件夹：webkitdirectory 一次给出整棵目录树；目录层级原样映射成 KB 文件夹树
// （上传契约只认 folder_id，所以按相对路径逐级复用/创建目录拿到 id，再走既有批量上传队列 send()）。
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
  const baseParentId = uploadFolderId.value || null
  const baseLabel = uploadFolder.value ? folderLabel(uploadFolder.value) : ''
  // webkitRelativePath 形如「根文件夹/子目录/…/文件」：目录段逐级映射为 KB 文件夹，
  // 源文件夹有几层就建几层（「上传到」选择是整个目录树的挂载点）
  const dirSegsOf = (file: File): string[] =>
    String(file.webkitRelativePath || file.name).split('/').map((seg) => seg.trim()).filter(Boolean).slice(0, -1)
  const destinationOf = (dirSegs: string[]): string =>
    [baseLabel, ...dirSegs].filter(Boolean).join(' / ') || '根目录 / 未分类'
  // 逐个预过滤不支持的扩展名（失败行进队列、逐个提示）；超 20MB / 空文件由 send() 预校验同口径处理
  const accepted: File[] = []
  for (const file of files) {
    const dot = file.name.lastIndexOf('.')
    const ext = dot > 0 ? file.name.slice(dot).toLowerCase() : ''
    if (!ext || !UPLOAD_EXTS.has(ext)) {
      pushUpload({
        id: ++uploadId, name: file.webkitRelativePath || file.name,
        state: 'fail', msg: '不支持的文件类型，未上传', destination: destinationOf(dirSegsOf(file)),
      })
      continue
    }
    accepted.push(file)
  }
  if (!accepted.length) return
  // 逐级「找到或创建」KB 文件夹（与「新建文件夹」对话框同一条 POST /api/kb/folders 契约）。
  // pathCache 以「根/子/…」相对路径为键整批共享：同一目录无论多少文件只建一次
  const pathCache = new Map<string, string>()
  const findChild = (name: string, parentId: string | null): string =>
    folders.value.find((folder) => folder.name === name && (folder.parent_id || null) === parentId)?.folder_id ?? ''
  // 返回末级 folder_id（'' = 根目录）；null = 空间/上下文已切换，调用方整批放弃
  const ensurePath = async (dirSegs: string[]): Promise<string | null> => {
    let parentId = baseParentId
    let prefix = ''
    for (const seg of dirSegs) {
      prefix = prefix ? `${prefix}/${seg}` : seg
      const cached = pathCache.get(prefix)
      if (cached != null) { parentId = cached || null; continue }
      let folderId = findChild(seg, parentId)
      if (!folderId) {
        try {
          const response = await fetch('/api/kb/folders', {
            method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' },
            body: JSON.stringify({ space_id: requestSpace, name: seg, parent_id: parentId }),
          })
          const data = await responseJson(response)
          if (!response.ok) throw new Error(data.error ?? `HTTP ${response.status}`)
          folderId = String(data.folder_id ?? data.id ?? '')
          if (!contextIsCurrent(requestEpoch, requestSpace)) return null
          await loadKnowledgeAssets(requestSpace, requestEpoch)
        } catch (e) {
          // 本地目录列表可能过期（服务端已有同名目录）：刷新后按名再认一次，认不到才报错
          if (!contextIsCurrent(requestEpoch, requestSpace)) return null
          await loadKnowledgeAssets(requestSpace, requestEpoch)
          folderId = findChild(seg, parentId)
          if (!folderId) throw e
        }
      }
      pathCache.set(prefix, folderId)
      parentId = folderId || null
    }
    return parentId ?? ''
  }
  // 先把整批文件的目录层级建齐（逐文件拿到末级 folder_id），再走既有批量上传队列
  const routeMap = new Map<File, { folderId: string; destination: string }>()
  try {
    for (const file of accepted) {
      const dirSegs = dirSegsOf(file)
      const folderId = dirSegs.length ? await ensurePath(dirSegs) : (baseParentId ?? '')
      if (folderId === null) return
      routeMap.set(file, { folderId, destination: destinationOf(dirSegs) })
    }
  } catch (e) {
    if (contextIsCurrent(requestEpoch, requestSpace)) {
      actionErr.value = `创建目录层级失败：${errorText(e)}。文件未上传，请重试或改用「选择文件」。`
    }
    return
  }
  if (!contextIsCurrent(requestEpoch, requestSpace)) return
  void send(accepted, (file) => routeMap.get(file))
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
  // 与 send() 同口径：目标目录已失效（folders 过期）时不静默落到根目录，明确报错
  if (requestedFolder && !targetFolder) {
    actionErr.value = '上传目标目录已失效，请刷新目录后重新选择。'
    return
  }
  const destination = targetFolder ? folderLabel(targetFolder) : '根目录 / 未分类'
  urlBusy.value = true
  actionErr.value = ''
  const row: UploadRow = { id: ++uploadId, name: url, state: 'doing', msg: '正在抓取并建立索引', destination }
  pushUpload(row)
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
    if (isTerminalIngest(data)) {
      updateUpload(rowId, { state: uploadState(data), msg: ingestOutcomeText(data), phase: undefined })
    } else {
      const docId = String(data.doc_id ?? data.id ?? '')
      updateUpload(rowId, { state: 'doing', msg: '解析中…（后台建立索引）', phase: 'parse' })
      if (docId) pollUploadDoc(rowId, docId, requestSpace, requestEpoch, String(data.updated_at ?? ''))
      else updateUpload(rowId, { state: 'partial', phase: undefined, msg: '已提交后台处理：服务端未返回文档标识，请稍后刷新列表查看结果。' })
    }
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
  const requestSpace = spaceId.value
  const requestEpoch = contextEpoch
  descGeneratingId.value = d.doc_id
  actionErr.value = ''
  try {
    const resp = await fetch(`/api/kb/doc/${encodeURIComponent(d.doc_id)}/description`, {
      method: 'POST', headers: { ...headers(), 'Content-Type': 'application/json' }, body: JSON.stringify({}),
    })
    const data = await responseJson(resp)
    // 全程按 requestEpoch+requestSpace 守卫：成功/失败都不写可能已不属当前空间的状态
    if (!contextIsCurrent(requestEpoch, requestSpace)) return
    if (!resp.ok) {
      actionErr.value = data.error ?? `生成描述失败（HTTP ${resp.status}）`
      return
    }
    d.description = data.description ?? ''
  } catch (e) {
    if (contextIsCurrent(requestEpoch, requestSpace)) actionErr.value = `生成描述失败：${errorText(e)}`
  } finally {
    if (contextIsCurrent(requestEpoch, requestSpace)) descGeneratingId.value = ''
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
    const baseline = String(data.updated_at ?? d.updated_at ?? '')
    const deadline = Date.now() + UPLOAD_POLL_TIMEOUT_MS
    while (contextIsCurrent(requestEpoch, requestSpace) && Date.now() <= deadline) {
      await new Promise((resolve) => window.setTimeout(resolve, UPLOAD_POLL_MS))
      await loadKnowledgeAssets(requestSpace, requestEpoch)
      const current = docs.value.find((item) => item.doc_id === d.doc_id)
      if (current?.updated_at && current.updated_at !== baseline && isTerminalIngest(current)) break
    }
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
  deptOptions.value = Array.isArray(data.departments) ? data.departments : []
  const limit = Number(data.limits?.batch_grants)
  grantBatchLimit.value = Number.isInteger(limit) && limit > 0 ? limit : 100
  const available = new Set(roleOptions.value.map((role) => role.role_code))
  selectedRoleCodes.value = selectedRoleCodes.value
    .filter((code) => available.has(code))
    .slice(0, grantBatchLimit.value)
  // 目录刷新后被停用/删除的部门不能残留为待提交的选中项
  if (grantDeptId.value && !deptOptions.value.some((dept) => dept.dept_id === grantDeptId.value)) {
    grantDeptId.value = ''
  }
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
  deptOptions.value = []
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
  const deptId = grantDeptId.value
  const targetMissing = grantKind.value === 'login' ? !grantee : grantKind.value === 'dept' ? !deptId : !roles.length
  if (granting.value || revokingGrant.value || targetMissing) return
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
      grantee: grantKind.value === 'login' ? grantee : grantKind.value === 'dept' ? deptId : '',
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
      const detail = failed
        .slice(0, 5).map((item: any) => `${item.role_code || item.grantee || '未知'}（${item.error || '失败'}）`).join('、')
      // 超过 5 条时补「等 N 条」，不让用户以为只有 5 条
      const more = failed.length > 5 ? ` 等 ${failed.length} 条` : ''
      grantFeedback.value = `已成功 ${succeeded} 项，失败 ${failed.length} 项：${detail}${more}`
      const available = new Set(roleOptions.value.map((role) => role.role_code))
      selectedRoleCodes.value = failed
        .map((item: any) => String(item.role_code || ''))
        .filter((code: string) => code && available.has(code))
    } else {
      grantFeedback.value = grantKind.value === 'role'
        ? `已更新 ${succeeded} 个角色的共享权限`
        : grantKind.value === 'dept'
          ? '部门共享权限已更新'
          : '账号共享权限已更新'
      grantTarget.value = ''
      grantDeptId.value = ''
      selectedRoleCodes.value = []
    }
    // 复用 refreshGrants（grants/roles/batch limit 一把刷，失败 codes 重选逻辑不被冲掉）
    await refreshGrants(requestSpace, requestId, requestEpoch)
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
  clearUploadPolls()
  document.removeEventListener('click', closeMenus)
})

/** 主对话框打开即聚焦（aria-modal 的键盘起点；section 本身 tabindex=-1 可聚焦）。 */
const panelEl = ref<HTMLElement>()
onMounted(() => {
  panelEl.value?.focus()
  // ⋯ 菜单与筛选下拉的点外关闭：按钮自身 @click.stop，落到这里的都是「点在外面」
  document.addEventListener('click', closeMenus)
})

/** WAI Tabs：←/→ 在 tab 间切换并移动焦点（自动激活模式）。 */
function onTabKeydown(e: KeyboardEvent) {
  if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return
  const tabs: WorkbenchTab[] = ['documents', 'retrieval', 'graph', 'mindmap', 'eval']
  const index = tabs.indexOf(activeTab.value)
  const next = tabs[(index + (e.key === 'ArrowRight' ? 1 : tabs.length - 1)) % tabs.length]
  activeTab.value = next
  const nav = (e.currentTarget as HTMLElement | null)
  nav?.querySelector<HTMLElement>(`#kb-${next}-tab`)?.focus()
}

void loadSpaces(props.initialSpace)
</script>

<template>
  <div class="kbp-mask" @click.self="closePanel">
    <section ref="panelEl" class="kbp" role="dialog" aria-modal="true" aria-labelledby="kb-title" tabindex="-1" @keydown.esc="closePanel">
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
            <button v-if="currentSpace" class="secondary-btn" type="button" :disabled="switchingDisabled" @click="openGrants">共享权限</button>
            <button class="secondary-btn" type="button" :disabled="switchingDisabled" @click="createErr = ''; createOpen = true">新建空间</button>
          </div>
        </section>
        <div v-if="spacesErr" class="action-error" role="alert">空间读取失败：{{ spacesErr }}</div>

        <nav class="workbench-tabs" role="tablist" aria-label="知识库工作台" @keydown="onTabKeydown">
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
                v-if="canWrite" class="icon-btn" type="button"
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
            <button v-if="unfiledCount" type="button" class="folder-node" :class="{ active: selectedFolderId === '__unfiled__' }" :aria-current="selectedFolderId === '__unfiled__' ? 'page' : undefined" :disabled="switchingDisabled" @click="selectFolder('__unfiled__')">
              <span class="folder-icon">◇</span><span>未分类</span><b>{{ unfiledCount }}</b>
            </button>
            <div v-if="foldersLoading" class="folder-state" role="status">正在读取目录…</div>
            <div v-else-if="foldersErr" class="folder-state error" role="alert">
              <span>目录加载失败</span>
              <!-- 整体刷新：单调 loadFolders 走 ++assetsRequestId，会令在途的 docs 加载失效被静默丢弃 -->
              <button type="button" class="text-btn" :disabled="loading || foldersLoading" @click="loadKnowledgeAssets()">重试</button>
            </div>
            <div v-else-if="folderApiAvailable !== false && !folders.length" class="folder-state">暂无文件夹</div>
            <p v-if="folderApiAvailable === false" class="folder-contract">当前服务端尚未启用目录接口；现有文档仍按“全部文档”管理。</p>
          </aside>
          <div class="folder-content">
            <!-- 页头统计卡：点击卡片即按对应状态筛选（Yuxi 的可点统计卡思路），与工具条筛选下拉同源 -->
            <div v-if="docs.length" class="stat-cards" aria-label="文档状态统计">
              <button type="button" class="stat-card" :class="{ active: filter === 'ready' }" @click="filter = 'ready'">
                <strong>{{ counts.ready }}</strong><span>可检索</span>
              </button>
              <button type="button" class="stat-card" :class="{ active: filter === 'processing' }" @click="filter = 'processing'">
                <strong>{{ counts.processing }}</strong><span>处理中</span>
              </button>
              <button type="button" class="stat-card failed" :class="{ active: filter === 'failed' }" title="解析或处理失败的文档；点击进入失败列表查看原因并重新处理" @click="filter = 'failed'">
                <strong>{{ counts.failed }}</strong><span>处理失败</span>
              </button>
              <button type="button" class="stat-card" :class="{ active: filter === 'attention' }" title="需要调整的非失败文档（待向量化、内容为空、已失效等，原因显示在文档名下方）；OCR 补页这类已自动消化的提示不计入" @click="filter = 'attention'">
                <strong>{{ counts.attention }}</strong><span>需处理</span>
              </button>
              <button type="button" class="stat-card" :class="{ active: filter === 'all' }" @click="filter = 'all'">
                <strong>{{ counts.all }}</strong><span>全部文档</span>
              </button>
            </div>
        <section v-if="canWrite" class="upload-section" aria-label="上传文档">
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
            @dragover.prevent @dragenter.prevent="onDragEnter" @dragleave.prevent="onDragLeave"
            @drop.prevent="onDrop" @click="openFilePicker"
            @keydown.enter.prevent="openFilePicker" @keydown.space.prevent="openFilePicker"
          >
            <input ref="fileEl" type="file" multiple hidden :accept="UPLOAD_ACCEPT" @click.stop @change="onPick" />
            <span class="upload-mark" aria-hidden="true">↑</span>
            <div class="drop-copy">
              <strong>{{ busy ? '正在处理上传队列' : '拖放文件到此处，或点击选择（可多选）' }}</strong>
              <span>支持 PDF/Word/Excel/PPT/txt/md/csv/json/log/html 与 png/jpg/webp/gif/bmp 等图片；单文件 ≤{{ MAX_UPLOAD_BYTES / 1024 / 1024 }}MB；同时上传 {{ UPLOAD_PARALLEL }} 个，其余排队等待，逐个反馈。</span>
            </div>
            <span class="primary-btn upload-action" aria-hidden="true">{{ busy ? '处理中' : '选择文件' }}</span>
          </div>
          <div class="dir-upload">
            <input ref="dirEl" type="file" webkitdirectory hidden @click.stop @change="onPickDir" />
            <button class="secondary-btn" type="button" :disabled="busy || folderApiAvailable === false" :title="folderApiAvailable === false ? '服务端尚未启用目录接口' : '上传整个文件夹'" @click="openDirPicker">▰ 上传文件夹</button>
            <span class="dir-hint">按源文件夹的目录层级原样建 KB 文件夹（嵌套子目录逐级保留）；不支持的类型与超 {{ MAX_UPLOAD_BYTES / 1024 / 1024 }}MB 的文件逐个跳过并在队列中提示</span>
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
              <button type="button" class="text-btn" :disabled="!uploadsDoneCount" @click="uploads = uploads.filter((u) => u.state === 'doing')">清除已结束</button>
            </div>
            <!-- 大批量时逐行扫读不现实：≥10 个文件加聚合卡（少于 10 个逐行状态已足够） -->
            <div v-if="uploads.length >= UPLOAD_AGG_MIN" class="queue-agg">
              <span>总计 {{ uploadAgg.total }}</span>
              <span>上传中 {{ uploadAgg.uploading }}</span>
              <span>解析中 {{ uploadAgg.parsing }}</span>
              <span :class="{ bad: uploadAgg.failed }">失败 {{ uploadAgg.failed }}</span>
            </div>
            <div v-for="u in uploads" :key="u.id" class="queue-row" :class="u.state">
              <span class="queue-state" aria-hidden="true">{{ u.state === 'doing' ? '···' : u.state === 'ok' ? '✓' : u.state === 'partial' ? '!' : '×' }}</span>
              <div class="queue-main">
                <strong :title="u.name">{{ u.name }}</strong>
                <span>{{ u.msg }}</span>
                <div
                  v-if="u.state === 'doing' && u.phase === 'upload' && u.progress != null" class="queue-progress"
                  role="progressbar" :aria-valuenow="u.progress" aria-valuemin="0" aria-valuemax="100"
                ><i :style="{ width: `${u.progress}%` }"></i></div>
                <span v-if="u.destination" class="queue-destination">目标目录：{{ u.destination }}</span>
                <span v-if="u.warn" class="queue-warn" role="note">⚠ {{ u.warn }}</span>
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
          <!-- 工具条：左面包屑（可点回退，复用文件夹逻辑），右搜索 + 筛选下拉 + 刷新（幽灵按钮） -->
          <div class="doc-toolbar">
            <nav class="folder-breadcrumb" aria-label="当前目录" :title="selectedFolderName">
              <div class="breadcrumb-path">
                <button type="button" @click="selectFolder('')">全部文档</button>
                <template v-if="selectedFolder">
                  <template v-for="folder in selectedFolderTrail" :key="folder.folder_id">
                    <span aria-hidden="true">/</span>
                    <button type="button" :class="{ current: folder.folder_id === selectedFolderId }" :aria-current="folder.folder_id === selectedFolderId ? 'page' : undefined" @click="selectFolder(folder.folder_id)">{{ folder.name }}</button>
                  </template>
                </template>
                <template v-else-if="selectedFolderId === '__unfiled__'">
                  <span aria-hidden="true">/</span><strong>未分类</strong>
                </template>
              </div>
              <div v-if="selectedFolder && canWrite" class="folder-commands">
                <button class="text-btn" type="button" :disabled="switchingDisabled" @click="openFolderEdit">改名/移动</button>
                <button class="text-btn danger" type="button" :disabled="switchingDisabled" @click="deleteSelectedFolder">{{ folderDeletingId ? '删除中' : '删除' }}</button>
              </div>
            </nav>
            <div class="doc-toolbar-tools">
              <label class="search-box">
                <span class="sr-only">搜索文档</span>
                <input v-model="search" type="text" placeholder="搜索文档（名称、标签、目录等）" />
                <button v-if="search" type="button" title="清空搜索" aria-label="清空搜索" @click="search = ''">×</button>
              </label>
              <div class="ghost-drop">
                <button
                  class="ghost-btn" type="button" :class="{ on: filter !== 'all' }"
                  :aria-expanded="filterMenuOpen" aria-haspopup="menu" title="按状态筛选"
                  @click.stop="filterMenuOpen = !filterMenuOpen; menuDocId = ''"
                ><span aria-hidden="true">▽</span>{{ currentFilterLabel }}</button>
                <div v-if="filterMenuOpen" class="ghost-menu" role="menu" aria-label="文档状态筛选">
                  <button
                    v-for="item in filters" :key="item.value" type="button" role="menuitemradio"
                    :aria-checked="filter === item.value" :class="{ active: filter === item.value }"
                    @click="filter = item.value; filterMenuOpen = false"
                  >{{ item.label }} <span>{{ item.count }}</span></button>
                </div>
              </div>
              <button class="ghost-btn" type="button" title="刷新列表" aria-label="刷新列表" :disabled="loading || switchingDisabled" @click="loadSpaces(spaceId)">↻</button>
            </div>
          </div>

          <div v-if="listErr" class="list-state error" role="alert">
            <strong>文档列表加载失败</strong>
            <span>{{ listErr }}</span>
            <button class="secondary-btn" type="button" :disabled="loading" @click="loadSpaces(spaceId)">重新加载</button>
          </div>
          <div v-if="loading && !docs.length" class="list-state">
            <strong>正在读取知识库</strong>
            <span>请稍候。</span>
          </div>
          <div v-else-if="!listErr && !docs.length" class="list-state empty">
            <strong>知识库还是空的</strong>
            <span>上传制度、产品资料、合同模板或业务表格后，即可在对话中检索和引用。</span>
            <button v-if="canWrite" class="primary-btn" type="button" :disabled="busy" @click="openFilePicker">上传第一份文档</button>
          </div>
          <div v-else-if="!visibleDocs.length" class="list-state empty">
            <strong>没有匹配的文档</strong>
            <span>调整关键词或切换状态筛选。</span>
            <button class="secondary-btn" type="button" @click="search = ''; filter = 'all'; selectFolder('')">清除筛选</button>
          </div>
          <template v-if="docs.length && visibleDocs.length">
            <!-- 批量条：勾选后出现；批量操作逐条走既有单文档端点 -->
            <div v-if="checkedIds.length" class="batch-bar" aria-live="polite">
              <span>已选 {{ checkedIds.length }} 项</span>
              <template v-if="canWrite">
                <button type="button" class="text-btn" :disabled="batchBusy" @click="batchReprocessChecked">批量重新处理</button>
                <button type="button" class="text-btn danger" :disabled="batchBusy" @click="batchDeleteOpen = true">批量删除</button>
              </template>
              <button type="button" class="text-btn" @click="checkedIds = []">清空选择</button>
            </div>
            <!-- 整行可点 = 打开预览；复选框/⋯ 菜单/文件名链接各自 @click.stop 拦截 -->
            <table class="doc-table">
              <thead>
                <tr>
                  <th class="col-check">
                    <input
                      type="checkbox" :checked="pageAllChecked" aria-label="选择本页全部文档"
                      @change="toggleCheckPage(($event.target as HTMLInputElement).checked)"
                    />
                  </th>
                  <th>文件名</th>
                  <th class="col-content">内容量</th>
                  <th class="col-status">状态</th>
                  <th class="col-time">时间</th>
                  <th class="col-ops"><span class="sr-only">操作</span></th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="d in pagedDocs" :key="d.doc_id" class="doc-tr" :class="{ disabled: d.enabled === false }" @click="previewDoc = d">
                  <td class="col-check" @click.stop>
                    <input type="checkbox" :checked="checkedSet.has(d.doc_id)" :aria-label="`选择 ${d.name}`" @change="toggleCheck(d.doc_id, ($event.target as HTMLInputElement).checked)" />
                  </td>
                  <td class="col-name">
                    <div class="doc-name-cell">
                      <span class="file-type" aria-hidden="true">{{ extOf(d.name) }}</span>
                      <div class="doc-name-main">
                        <button type="button" class="doc-name-link" :title="nameTitle(d)" @click.stop="previewDoc = d">{{ d.name }}</button>
                        <!-- 需处理/有提示的文档把原因亮在名字下（比描述重要），可处理的档带内联动作；其余才看描述行 -->
                        <span v-if="issueText(d)" class="doc-issue-line" :class="{ attention: !!attentionInfo(d) }" :title="issueTitle(d)">
                          {{ issueText(d) }}<button
                            v-if="attentionInfo(d)?.actionable && canWrite" type="button" class="issue-act"
                            :disabled="!!reprocessingId" @click.stop="reprocess(d)"
                          >{{ reprocessingId === d.doc_id ? '处理中' : '点这里处理' }}</button>
                        </span>
                        <span v-else-if="d.description" class="doc-desc-line" :title="d.description">{{ d.description }}</span>
                      </div>
                    </div>
                  </td>
                  <td class="col-content" :title="contentText(d)">{{ contentText(d) }}</td>
                  <td class="col-status">
                    <!-- 状态 pill 兼主操作：可处理的档（失败/待向量/采集失败…）可点=重新处理；处理中点不动但 title 有说明 -->
                    <button
                      v-if="pillClickable(d)" type="button"
                      class="status-pill clickable" :class="pillState(d)" :title="pillTitle(d)" :disabled="!!reprocessingId"
                      @click.stop="reprocess(d)"
                    >{{ reprocessingId === d.doc_id ? '处理中' : pillText(d) }}</button>
                    <span v-else class="status-pill" :class="pillState(d)" :title="pillTitle(d)">{{ pillText(d) }}</span>
                  </td>
                  <td class="col-time">{{ docTimeText(d.updated_at || d.created_at) }}</td>
                  <td class="col-ops" @click.stop>
                    <button
                      type="button" class="ops-btn" :aria-expanded="menuDocId === d.doc_id" aria-haspopup="menu"
                      :aria-label="`打开 ${d.name} 的操作菜单`" @click.stop="toggleRowMenu(d.doc_id)"
                    >⋯</button>
                    <div v-if="menuDocId === d.doc_id" class="ops-menu" role="menu">
                      <button type="button" role="menuitem" @click="previewDoc = d; menuDocId = ''">预览</button>
                      <button type="button" role="menuitem" @click="downloadDoc(d.doc_id, d.name); menuDocId = ''">下载原件</button>
                      <template v-if="canWrite">
                        <button type="button" role="menuitem" :disabled="folderApiAvailable === false" @click="openMoveDialog(d); menuDocId = ''">移动至…</button>
                        <button type="button" role="menuitem" @click="openMetadata(d); menuDocId = ''">元数据</button>
                        <button
                          type="button" role="menuitem" :disabled="!!descGeneratingId"
                          title="AI 按文档开头生成一段描述并写回（覆盖已有描述，参与检索召回）"
                          @click="generateDescription(d); menuDocId = ''"
                        >{{ descGeneratingId === d.doc_id ? '生成中' : '生成描述' }}</button>
                        <button
                          type="button" role="menuitem" :disabled="!!stateChangingId"
                          :title="d.enabled === false ? '恢复参与知识检索' : '暂时从知识检索中移除'"
                          @click="toggleState(d); menuDocId = ''"
                        >{{ d.enabled === false ? '启用' : '停用' }}</button>
                        <button
                          v-if="stateOf(d.status) !== 'ready' || !!d.last_ingest_error" type="button" role="menuitem"
                          :disabled="!!reprocessingId" title="使用服务器保存的原文件重新解析并建立索引"
                          @click="reprocess(d); menuDocId = ''"
                        >重新处理</button>
                        <button type="button" role="menuitem" class="danger" :disabled="deletingId === d.doc_id" @click="openDeleteConfirm(d); menuDocId = ''">删除</button>
                      </template>
                    </div>
                  </td>
                </tr>
              </tbody>
            </table>
            <div class="doc-pager">
              <span class="doc-pager-total">共 {{ visibleDocs.length }} 项</span>
              <div class="doc-pager-controls">
                <label>每页
                  <select v-model.number="pageSize" aria-label="每页条数">
                    <option :value="20">20</option>
                    <option :value="50">50</option>
                    <option :value="100">100</option>
                  </select>
                  条
                </label>
                <button type="button" :disabled="page <= 0" @click="page--">上一页</button>
                <span class="doc-pager-pages">{{ page + 1 }} / {{ pageCount }}</span>
                <button type="button" :disabled="page >= pageCount - 1" @click="page++">下一页</button>
              </div>
            </div>
          </template>
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

        <!-- graph/mindmap/eval 三个面板用 v-if 互斥挂载、切 tab 即销毁是刻意取舍：
             keep-alive 保活会让隐藏画布的 RAF/轮询空转；评估草稿等瞬态丢失可接受，切回重来。 -->
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
            <input v-model="newSpaceId" maxlength="64" pattern="[A-Za-z0-9_-]+" title="仅字母、数字、下划线、短横线" placeholder="留空则自动生成" />
          </label>
          <div v-if="createErr" class="action-error" role="alert">{{ createErr }}</div>
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
                <option value="dept">DMS 部门</option>
              </select>
            </label>
            <label v-if="grantKind === 'login'" class="grant-target">
              <span>登录账号</span>
              <input v-model="grantTarget" maxlength="64" required placeholder="输入 DMS 登录账号" :disabled="granting || !!revokingGrant" />
            </label>
            <label v-if="grantKind === 'dept'" class="grant-target">
              <span>部门</span>
              <select v-model="grantDeptId" :disabled="granting || !!revokingGrant">
                <option value="" disabled>{{ deptOptions.length ? '选择要授权的部门' : 'DMS 当前没有可共享的部门' }}</option>
                <option v-for="dept in deptOptions" :key="dept.dept_id" :value="dept.dept_id">{{ dept.dept_name }}（{{ dept.dept_id }}）</option>
              </select>
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
            <button
              v-if="grantKind === 'dept'" class="primary-btn" type="submit"
              :disabled="granting || !grantDeptId"
            >{{ granting ? '授权中' : '添加部门' }}</button>
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
            <div v-else-if="!grants.length" class="grant-empty">尚未共享给其他用户、角色或部门</div>
            <template v-else>
              <div v-for="g in grants" :key="`${g.grantee_kind}:${g.grantee}:${g.perm}`" class="grant-row">
                <span class="grant-kind">{{ g.grantee_kind === 'login' ? '用户' : g.grantee_kind === 'dept' ? '部门' : '角色' }}</span>
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

      <div v-if="folderDeleteConfirm && selectedFolder" class="confirm-mask" @click.self="closeFolderDeleteConfirm()">
        <div class="confirm-box" role="alertdialog" aria-modal="true" aria-labelledby="delete-folder-title" @keydown.esc.stop="closeFolderDeleteConfirm()">
          <h3 id="delete-folder-title">删除文件夹？</h3>
          <p>仅没有文档的目录树可以删除。「{{ folderLabel(selectedFolder) }}」及其空子文件夹将被删除，且无法撤销。</p>
          <div v-if="folderDeleteErr" class="action-error" role="alert">{{ folderDeleteErr }}</div>
          <div class="confirm-actions">
            <button class="secondary-btn" type="button" :disabled="!!folderDeletingId" @click="closeFolderDeleteConfirm()">取消</button>
            <button class="danger-btn" type="button" :disabled="!!folderDeletingId" @click="removeFolderConfirmed">
              {{ folderDeletingId ? '删除中' : '确认删除' }}
            </button>
          </div>
        </div>
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
              <input v-model="metadataEffectiveFrom" type="date" :max="metadataEffectiveTo || undefined" />
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
              <button v-if="metadataRelatedIds.length" class="text-btn" type="button" :disabled="metadataSaving" @click="metadataRelatedIds = []">清空已选</button>
            </header>
            <div v-if="metadataLoading" class="relation-state" role="status">正在加载关联信息…</div>
            <template v-else-if="metadataRelationReady">
              <label class="relation-search">
                <span class="sr-only">搜索关联文档</span>
                <input v-model="metadataRelationSearch" type="search" placeholder="搜索文档名、文件夹、标签、文档族或版本" />
              </label>
              <div class="relation-summary">
                <strong>已选 {{ metadataRelatedIds.length }}</strong>
                <span>最多 {{ MAX_RELATED }} 篇</span>
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

      <div v-if="moveDocTarget" class="confirm-mask" @click.self="closeMoveDialog()">
        <form class="confirm-box folder-create-box" role="dialog" aria-modal="true" aria-labelledby="move-doc-title" @submit.prevent="confirmMoveDoc" @keydown.esc.stop="closeMoveDialog()">
          <h3 id="move-doc-title">移动文档</h3>
          <p :title="moveDocTarget.name">《{{ moveDocTarget.name }}》</p>
          <label>
            <span>目标文件夹</span>
            <select v-model="moveTargetFolderId" :disabled="!!docMovingId">
              <option value="">根目录 / 未分类</option>
              <option v-for="row in folderRows" :key="row.folder.folder_id" :value="row.folder.folder_id">{{ '　'.repeat(row.depth) }}{{ folderLabel(row.folder) }}</option>
            </select>
          </label>
          <div class="confirm-actions">
            <button class="secondary-btn" type="button" :disabled="!!docMovingId" @click="closeMoveDialog()">取消</button>
            <button class="primary-btn" type="submit" :disabled="!!docMovingId">{{ docMovingId ? '移动中' : '移动' }}</button>
          </div>
        </form>
      </div>

      <div v-if="batchDeleteOpen" class="confirm-mask" @click.self="!batchBusy && (batchDeleteOpen = false)">
        <div class="confirm-box" role="alertdialog" aria-modal="true" aria-labelledby="batch-delete-title" @keydown.esc.stop="!batchBusy && (batchDeleteOpen = false)">
          <h3 id="batch-delete-title">删除所选文档？</h3>
          <p>已选 {{ checkedIds.length }} 份文档的原文件、切片和检索索引将一并删除，且无法撤销。</p>
          <div class="confirm-actions">
            <button class="secondary-btn" type="button" :disabled="batchBusy" @click="batchDeleteOpen = false">取消</button>
            <button class="danger-btn" type="button" :disabled="batchBusy" @click="removeCheckedConfirmed">
              {{ batchBusy ? '删除中' : '确认删除' }}
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
.breadcrumb-path { min-width: 0; display: flex; align-items: center; gap: 6px; overflow-x: auto; scrollbar-width: thin; }
.breadcrumb-path span { color: var(--text-muted); }
.breadcrumb-path button { min-width: 0; max-width: 180px; flex: 0 1 auto; overflow: hidden; padding: 0; border: 0; background: transparent; color: var(--text-muted); cursor: pointer; font: inherit; text-overflow: ellipsis; white-space: nowrap; }
.breadcrumb-path button:hover { color: var(--primary); text-decoration: underline; }
.breadcrumb-path button.current { color: var(--text-primary); font-weight: 700; text-decoration: none; }
.breadcrumb-path strong { min-width: 0; overflow: hidden; color: var(--text-primary); text-overflow: ellipsis; white-space: nowrap; }
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
.queue-agg {
  display: flex; align-items: center; gap: 14px; padding: 7px 10px;
  border-bottom: 1px solid var(--divider); background: var(--bg-main);
  color: var(--text-muted); font-size: 11px; font-variant-numeric: tabular-nums;
}
.queue-agg .bad { color: var(--error-text); }
.queue-progress { height: 4px; margin-top: 5px; overflow: hidden; border-radius: 999px; background: var(--bg-sunken); }
.queue-progress i { display: block; height: 100%; border-radius: 999px; background: var(--primary); transition: width .2s ease; }
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
.queue-main > .queue-warn { color: var(--warning-text); font-size: 10.5px; }
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
.folder-create-box { width: min(460px, calc(100% - 32px)); }
.folder-create-box > label { display: block; margin-top: 12px; }
.folder-create-box > label > span { display: block; margin-bottom: 5px; color: var(--text-primary); font-size: 11.5px; font-weight: 650; }
.folder-create-box input, .folder-create-box select { width: 100%; }
.folder-create-box input { height: 34px; padding: 0 9px; border: 1px solid var(--border); border-radius: 6px; outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px; }
.folder-create-box input:focus { border-color: var(--primary); box-shadow: var(--ring); }
/* 页头统计卡：点击即筛选（Yuxi 可点统计卡思路） */
.stat-cards { display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); gap: 8px; margin-bottom: 10px; }
.stat-card {
  display: flex; align-items: baseline; gap: 7px; padding: 8px 12px; border: 1px solid var(--border);
  border-radius: 8px; background: var(--bg-card); cursor: pointer; font: inherit; text-align: left;
}
.stat-card:hover { border-color: var(--primary); background: var(--primary-light); }
.stat-card.active { border-color: var(--primary); background: var(--primary-light); box-shadow: inset 0 0 0 1px var(--primary); }
.stat-card strong { color: var(--text-primary); font-size: 17px; font-variant-numeric: tabular-nums; }
.stat-card span { color: var(--text-muted); font-size: 11px; }
.stat-card.failed strong, .stat-card.failed span { color: var(--error-text); }
.stat-card.failed.active { border-color: var(--error-text); background: var(--error-bg); box-shadow: inset 0 0 0 1px var(--error-text); }
/* 列表工具条：左面包屑、右搜索 + 筛选下拉 + 刷新（幽灵按钮） */
.doc-toolbar { display: flex; align-items: center; gap: 10px; margin-bottom: 8px; }
.doc-toolbar .folder-breadcrumb { flex: 1; min-width: 0; min-height: 0; margin: 0; padding: 0; border: 0; background: transparent; }
.doc-toolbar-tools { display: flex; align-items: center; gap: 6px; }
.doc-toolbar .search-box { width: min(230px, 30vw); }
.doc-toolbar .search-box input { height: 28px; }
.doc-toolbar .search-box button { top: 2px; }
.ghost-btn {
  height: 28px; display: inline-flex; align-items: center; gap: 5px; padding: 0 8px;
  border: 1px solid transparent; border-radius: 6px; background: transparent;
  color: var(--text-muted); cursor: pointer; font: inherit; font-size: 11.5px;
}
.ghost-btn:hover:not(:disabled) { border-color: var(--border); background: var(--bg-main); color: var(--text-primary); }
.ghost-btn.on { color: var(--primary); background: var(--primary-light); }
.ghost-drop { position: relative; }
.ghost-menu {
  position: absolute; top: 32px; right: 0; z-index: 6; min-width: 128px; padding: 4px;
  border: 1px solid var(--border); border-radius: 8px; background: var(--bg-card); box-shadow: var(--shadow-lg);
}
.ghost-menu button {
  width: 100%; display: flex; align-items: center; justify-content: space-between; gap: 10px;
  padding: 6px 9px; border: 0; border-radius: 5px; background: transparent;
  color: var(--text-regular); cursor: pointer; font: inherit; font-size: 12px; text-align: left;
}
.ghost-menu button:hover { background: var(--primary-light); color: var(--primary); }
.ghost-menu button.active { color: var(--primary); font-weight: 700; }
.ghost-menu button span { color: var(--text-faint); font-variant-numeric: tabular-nums; }
/* 批量条：复选框勾选后出现 */
.batch-bar {
  display: flex; align-items: center; gap: 14px; margin-bottom: 6px; padding: 5px 10px;
  border: 1px solid rgba(var(--primary-rgb), .25); border-radius: 6px; background: var(--primary-light);
  color: var(--text-regular); font-size: 11.5px;
}
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
/* 文档表：Yuxi small 密度（th 8px 10px / td 7px 10px / 13px），整行可点、hover 淡主色底 */
.doc-table { width: 100%; table-layout: fixed; border-collapse: collapse; border: 1px solid var(--border); font-size: 13px; }
.doc-table th {
  padding: 8px 10px; border-bottom: 1px solid var(--border); background: var(--bg-main);
  color: var(--text-muted); font-size: 11px; font-weight: 600; text-align: left;
}
.doc-table td {
  padding: 7px 10px; overflow: hidden; border-top: 1px solid var(--divider);
  color: var(--text-muted); text-overflow: ellipsis; white-space: nowrap;
}
.doc-table tbody tr:first-child td { border-top: 0; }
.doc-tr { cursor: pointer; }
.doc-tr:hover { background: var(--primary-light); }
.doc-tr.disabled { opacity: .62; }
/* 特异性要压过 .doc-table th/td（0,1,1），否则复选框列不居中 */
.doc-table th.col-check, .doc-table td.col-check { width: 32px; text-align: center; }
.col-check input { width: 14px; height: 14px; accent-color: var(--primary); cursor: pointer; }
.col-content { width: 110px; font-variant-numeric: tabular-nums; }
.col-status { width: 104px; }
.col-time { width: 96px; font-variant-numeric: tabular-nums; }
.col-ops { width: 64px; text-align: center; }
/* ⋯ 菜单挂在单元格上：position 需要 relative，且不能继承 td 的 overflow:hidden */
td.col-ops { position: relative; overflow: visible; }
.doc-name-cell { min-width: 0; display: flex; align-items: center; gap: 9px; }
.file-type {
  flex: 0 0 38px; height: 42px; display: grid; place-items: center;
  border: 1px solid var(--border); border-radius: 5px; background: var(--bg-main);
  color: var(--primary); font-size: 9px; font-weight: 800;
}
.doc-table .file-type { flex: 0 0 30px; height: 32px; font-size: 8.5px; }
.doc-name-main { min-width: 0; display: flex; flex-direction: column; gap: 1px; }
.doc-name-link {
  display: block; max-width: 100%; overflow: hidden; padding: 0; border: 0; background: transparent;
  color: var(--text-primary); cursor: pointer; font: inherit; font-weight: 600;
  text-align: left; text-overflow: ellipsis; white-space: nowrap;
}
.doc-name-link:hover { color: var(--primary); text-decoration: underline; }
.doc-desc-line { overflow: hidden; color: var(--text-faint); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
/* 原因行：默认淡色留痕（已消化的提示）；需处理档亮 warning 色直接可见 */
.doc-issue-line { overflow: hidden; color: var(--text-faint); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
.doc-issue-line.attention { color: var(--warning-text); }
/* 原因行内的处理动作：链接态小号字，row 点击（开预览）由 @click.stop 隔开 */
.issue-act { margin-left: 6px; padding: 0; border: 0; background: none; color: var(--primary); cursor: pointer; font: inherit; font-size: 11px; }
.issue-act:hover:not(:disabled) { text-decoration: underline; }
.issue-act:disabled { cursor: not-allowed; opacity: .55; }
/* 状态 pill（24px）：可检索绿 / 处理中蓝 / 需处理黄 / 已停用灰 / 失败红；失败可点 = 重新处理 */
.status-pill {
  display: inline-flex; align-items: center; height: 24px; padding: 0 8px; border: 0; border-radius: 6px;
  font: inherit; font-size: 12px; white-space: nowrap;
}
.status-pill.ready { color: var(--success-text); background: var(--success-bg); }
.status-pill.processing { color: var(--primary); background: var(--primary-light); }
.status-pill.attention { color: var(--warning-text); background: var(--warning-bg); }
.status-pill.disabled { color: var(--text-muted); background: var(--bg-sunken); }
.status-pill.failed { color: var(--error-text); background: var(--error-bg); }
.status-pill.clickable { cursor: pointer; }
.status-pill.clickable:hover:not(:disabled) { box-shadow: inset 0 0 0 1px var(--error-text); }
.ops-btn {
  width: 28px; height: 28px; border: 1px solid transparent; border-radius: 6px; background: transparent;
  color: var(--text-muted); cursor: pointer; font: inherit; font-size: 15px; line-height: 1;
}
.ops-btn:hover { border-color: var(--border); background: var(--bg-main); color: var(--text-primary); }
.ops-menu {
  position: absolute; top: 30px; right: 6px; z-index: 6; min-width: 128px; padding: 4px;
  border: 1px solid var(--border); border-radius: 8px; background: var(--bg-card); box-shadow: var(--shadow-lg);
}
.ops-menu button {
  width: 100%; display: block; padding: 6px 10px; border: 0; border-radius: 5px; background: transparent;
  color: var(--text-regular); cursor: pointer; font: inherit; font-size: 12px; text-align: left;
}
.ops-menu button:hover:not(:disabled) { background: var(--primary-light); color: var(--primary); }
.ops-menu button.danger { color: var(--error-text); }
.ops-menu button.danger:hover:not(:disabled) { background: var(--error-bg); }
.ops-menu button:disabled { cursor: not-allowed; opacity: .5; }
.doc-pager { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 8px 2px 0; color: var(--text-muted); font-size: 11.5px; }
.doc-pager-controls { display: flex; align-items: center; gap: 8px; }
.doc-pager-controls label { display: inline-flex; align-items: center; gap: 5px; }
.doc-pager-controls select {
  height: 24px; padding: 0 4px; border: 1px solid var(--border); border-radius: 5px;
  background: var(--bg-card); color: var(--text-regular); font: inherit; font-size: 11px;
}
.doc-pager-controls > button {
  height: 24px; padding: 0 9px; border: 1px solid var(--border); border-radius: 5px;
  background: var(--bg-card); color: var(--text-regular); cursor: pointer; font: inherit; font-size: 11px;
}
.doc-pager-controls > button:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.doc-pager-pages { font-variant-numeric: tabular-nums; }
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
.primary-btn { padding: 0 13px; border-color: var(--primary); background: var(--primary); color: var(--on-primary); }
.primary-btn:hover { background: var(--primary-hover); }
.upload-action { flex: 0 0 auto; display: inline-flex; align-items: center; pointer-events: none; }
.secondary-btn { padding: 0 13px; background: var(--bg-card); color: var(--text-regular); }
.secondary-btn:hover, .icon-btn:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-light); }
/* 底色是 --error-text（暗色下 #ec8f8f 偏亮），前景另给一个 token：白字在它上面不过 AA */
.danger-btn { padding: 0 13px; border-color: var(--error-ring); background: var(--error-text); color: var(--on-error); }
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
.create-box input:focus { border-color: var(--primary); box-shadow: var(--ring); }
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
.confirm-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 18px; }
.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
.kbp:focus { outline: none; }
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
  .folder-commands { margin-left: 0; }
  .upload-destination { grid-template-columns: auto minmax(0, 1fr); }
  .upload-destination > span { grid-column: 1 / -1; }
  .drop-zone { align-items: flex-start; flex-wrap: wrap; }
  .drop-zone .primary-btn { margin-left: 52px; }
  .stat-cards { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .doc-toolbar { align-items: flex-start; flex-wrap: wrap; }
  .doc-toolbar-tools { width: 100%; }
  .doc-toolbar .search-box { flex: 1; width: auto; }
  /* 窄屏弃次要列保文件名与状态（内容量/时间收进 name 的 title 与元数据对话框） */
  .col-content, .col-time { display: none; }
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
