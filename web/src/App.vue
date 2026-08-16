<script setup lang="ts">
import { ref, computed, reactive, watch, onMounted, onBeforeUnmount, nextTick, defineAsyncComponent } from 'vue'
import ResultPanel from './ResultPanel.vue'
import KbAnswer from './KbAnswer.vue'
import KbPanel from './KbPanel.vue'
import DeepTaskPanel from './DeepTaskPanel.vue'
import UsagePanel from './UsagePanel.vue'
import SkillsPanel from './SkillsPanel.vue'
import DataMapPanel from './DataMapPanel.vue'
import SqlAuditPanel from './SqlAuditPanel.vue'
import TracePanel from './TracePanel.vue'
import { fmt, isGrossMarginLabel, semanticForLabel, toNum, uuid, type Semantic } from './format'
import { ANALYSIS_URL, ANALYSIS_REPORT_URL } from './api'
import { SseParser, parseEventData } from './kb-stream'
import { createSessionExpiryGuard, runAskTransport } from './ask-transport'
import type { IntentSummary } from './result-receipt'
import { projectKnowledgeReceipt } from './result-receipt'
import { snapshotQueuedAsk, type QueuedAskSnapshot } from './ask-queue'

const BiChart = defineAsyncComponent(() => import('./BiChart.vue'))

interface ColSpec { name: string; role: string; semantic: Semantic }
interface Delta { pct: number; dir: 'up' | 'down' | 'flat'; label: string }
interface Kpi { label: string; value: unknown; semantic: Semantic; delta?: Delta }
interface Block {
  type: 'kpis' | 'entity' | 'chart' | 'table'
  items?: Kpi[]
  pairs?: [string, unknown][]
  kind?: 'bar' | 'line' | 'pie'
  x?: number; y?: number[]; top?: number | null; series?: number | null
}
interface Interact { drill?: string[] }
interface ViewSpec { columns: ColSpec[]; blocks: Block[]; interact?: Interact; insight?: string }
interface SupplementalResult {
  columns: string[]; rows: unknown[][]; row_count: number
  truncated: boolean; view: ViewSpec
}
interface KpiComparison {
  label: string; current: number; baseline: number; change: number; pct: number
}
interface SalesContextResult { columns: string[]; rows: unknown[][] }
interface SubResult { question: string; result: AskResult }
/** 与 KbAnswer.vue 的本地 Citation 双份声明：字段增减两边同步（来源行位置徽标口径在 citation.ts）。 */
interface Citation {
  doc_id: string; doc_name: string; chunk_id: number
  page?: number | null; heading_path?: string | string[]; score?: number
  relations?: string[]
  tags?: string[]; business_domain?: string | null
  effective_from?: string | null; effective_to?: string | null; source_uri?: string | null
  document_family?: string | null; document_revision?: string | null
  folder_id?: string | null; folder_path?: string | null; directory_path?: string | null
  source_hash?: string; doc_updated_at?: string; channels?: string[]
  /** 引用覆盖的连续块数（合并命中的首块是 chunk_id）；回查靠它取回模型看到的那一段。
   *  只在这里声明形状，用它的地方是 KbAnswer.vue。 */
  span?: number | null
}
// kind='text'（知识库回答）没有 view/columns/rows —— 全部按可选声明，分派看 kind
interface AskResult {
  kind?: string
  sql: string; columns: string[]; rows: unknown[][]; row_count: number
  truncated: boolean; elapsed_ms: number; route: string; view?: ViewSpec
  /** 主结果之外的结构/明细数据。它有独立数据集与视图，不能替换主结果。 */
  supplemental?: SupplementalResult
  /** 已执行出的同比/环比原值；AI 解读按独立 COMPARE 事实域使用。 */
  comparisons?: KpiComparison[]
  /** 与主指标同时间窗的收入/成本/毛利补充；AI 解读按独立 CONTEXT 事实域使用。 */
  sales_context?: SalesContextResult
  subs?: SubResult[]
  markdown?: string; citations?: Citation[]
  /** 【Y2】一次问答的关联键：知识库回答由 Answer 自带（👍/👎 反馈绑它）；
   *  问数侧的关联键在 trust.trace_id，两处不混。 */
  trace_id?: string
  /** 口径复核未通过（回炉预算用尽仍违反声明）：非空即「下方数字不可信」。
   *  后端 `skip_serializing_if`，所以是可选字段 —— 老服务端不带这个键也不会崩。
   *  **渲染在 `ResultPanel` 里**（每个结果自己那一层，含复合的每个子问）；
   *  这里顶层只在「容器自己带了它」时才显示，否则单结果会显示两遍。 */
  caliber_note?: string
  /** 命中行上限时的截断三件套（原因 / 范围 / 续读参数），同上由 ResultPanel 渲染 */
  truncation_note?: string
  /** 被敏感列防线整列置空的列名（`RowSet.redacted`）；渲染在 ResultPanel */
  redacted?: string[]
  /** 意图不明时后端给的候选问法（chip 与「换个维度看」同款，点击 = 把 question 原样发出）。
   *  字段缺席 = 现状不变（老服务端不带这个键也不崩）。 */
  clarify_options?: { label: string; question: string }[]
  /** 【混合查询】问数结果携带的知识库答案（自动模式「文档+取数」双半问句两路并行）；
   *  老服务端不带这个键 = 纯问数，渲染分支不出现。AI 综合落在 `view.insight`。 */
  kb?: AskResult
  scope_note?: string
  reinterpret_note?: string
  resolved_question?: string
  /** 后端同一份结构化意图合同的安全摘要：只含用户表面词与覆盖状态，不含 SQL、prompt 或内部 ID。 */
  intent_summary?: IntentSummary
  trust?: {
    level: 'verified' | 'high' | 'review'; trace_id: string; source: string; route: string
    access: string; execution: string; fingerprint: string; checks: string[]
  }
  /** `/api/ask` 对可供 AI 解读/报表使用的完整事实集签发；客户端只原样回传。 */
  analysis_receipt?: string
}
/** AI 解读（按需拉取，不随 /api/ask 同步等模型）。
 *  `caliber` 恒有（零 LLM 的口径说明）、`insight` 可缺（模型那段话）——
 *  两者分开存，因为「模型没给解读」不等于「这次解读失败」。 */
interface Analysis {
  open: boolean; loading: boolean; caliber?: string; insight?: string; error?: string
  /** `/api/analysis` 对事实材料和 insight 整体签发，生成报表时原样回传。 */
  reportReceipt?: string
  /** 【S2】报表固化中 / 已固化的 artifact（点击走深链拦截开预览面板） */
  saving?: boolean; artifact?: { url: string; title: string }
}
// 一次问答
interface Turn {
  role: 'user' | 'ai'
  /** 跨会话切换时禁止 Vue 复用另一个会话的输入或展开状态。 */
  turnKey?: string
  /** 会话归属固定在创建时；异步完成后不得借用当前正在查看的会话。 */
  convId?: number
  question?: string
  result?: AskResult
  error?: string
  loading?: boolean
  elapsed?: number // loading 已耗时秒（慢查询有预期）
  showSql?: boolean
  analysis?: Analysis
  /** 多角色账号被 fail-closed 拒时的可选角色（`/api/roles`）。
   *  只在**能拿到清单且真的多于一个**时才有值 —— 拿不到就保持后端那句错误文案，
   *  绝不给一个空下拉框（那比现在更糟：用户会以为自己没有角色可选）。 */
  roles?: string[]
  /** 【深度模式】compose 端点附带的产物卡（点击走深链拦截开预览面板） */
  artifact?: { url: string; title: string }
  /** 【D6】promote 事件回放：别的会话钉过来的产物引用（渲染成产物卡，不当作一轮问答） */
  promoted?: { url: string; title: string; note?: string; version?: number }
  /** 【思维过程】深度模式的阶段清单（轮询 `/api/deep/progress` 实时刷） */
  progress?: string[]
  /** 【子任务面板】深度模式板块级状态（与 progress 同一次轮询刷新；只含标题/状态/耗时） */
  tasks?: DeepSectionTask[]
  /** 【深度页聊天内嵌】页面数据载荷（理解/KPI/分析/板块，聊天框直接渲染） */
  page?: DeepPage
  /** 该轮发送时的模式快照；用户之后切模式不应改变正在运行气泡的样式/解析。 */
  mode?: 'deep' | 'lite'
  /** 【SSE 流式】知识库回答生成中：result 里是增量预览（meta 的候选引用 + delta 拼的正文），
   *  done 事件到达时整体替换成过完口径后处理的最终 Answer。 */
  streaming?: boolean
  /** 该轮被用户、会话生命周期或超时中止；中止后不得继续提交排队问题。 */
  abortReason?: 'user' | 'session' | 'lifecycle' | 'timeout'
  /** 周报错误重试必须保留完整提示词与强制深度参数，不能拿短展示标题重新查询。 */
  retryQuestion?: string
  retryOptions?: SendOptions
  feedback?: string
  /** 【D4】深度运行的进度 id（服务端断点续跑的账本主键）；仅深度轮携带 */
  rid?: string
  /** 【D4】该轮出错后服务端账本判定可续跑（interrupted/failed/重启孤儿）→ 显示「续跑」 */
  resumable?: boolean
  /** 【D4】续跑请求进行中（防重复点击） */
  resuming?: boolean
}

/** 深度页板块（与 deep_api Section 同形） */
interface DeepSection {
  title: string
  question?: string
  kind: 'bar' | 'line' | 'pie' | 'table'
  columns: string[]
  rows: unknown[][]
  /** 仅是前端展示状态，不回传服务端。 */
  view?: 'chart' | 'table'
}
/** 【子任务面板】板块级进度（与 deep_api SectionProgress 同形：state = queued|running|done|failed） */
interface DeepSectionTask {
  title: string
  state: 'queued' | 'running' | 'done' | 'failed'
  ms?: number
  /** 【D8】板块验收断言（规划透出；老服务端不带此键 = 不显示） */
  assertion?: string
}
interface DeepComparison {
  label: string
  basis?: string
  current?: number
  baseline?: number
  change?: number
  pct?: number | string | null
  dir: 'up' | 'down' | 'flat'
}
/** 【D8】验收断言（与 dms_agent analysis::Assertion + deep_api page 载荷同形）。
 *  verdict：末次证据解读同发 LLM 的自评；null/缺 = 待评（LLM 降级时断言仍透出） */
interface DeepAssertion {
  section: string
  text: string
  verdict?: 'met' | 'partial' | 'unmet' | null
}
interface DeepPage {
  kind?: 'metric' | 'breakdown' | 'trend' | 'comparison' | 'attribution' | 'document' | 'entity' | 'detail' | 'general'
  label?: string
  understanding?: string | null
  /** 规划了却没跑出来的板块标题（后端 `missing_sections`）。老服务端没有这个键。 */
  missing_sections?: string[]
  /** 【D8】验收断言透出区（报告页顶部小字区；老服务端不带此键 = 不渲染） */
  assertions?: DeepAssertion[]
  kpi?: { label: string; value: string } | null
  /** comparison 为历史会话兼容；新结果统一使用 comparisons。 */
  comparison?: DeepComparison | null
  comparisons?: DeepComparison[]
  facts?: { label: string; value: string }[]
  highlights?: { label: string; value: string; note: string }[]
  contributions?: unknown[][]
  insight?: string | null
  /** 深度页其他字段全都按可选防御，这个也不能例外：老服务端缺键时整轮崩溃的就是必填声明 */
  sections?: DeepSection[]
  recent?: { columns: string[]; rows: unknown[][] } | null
  sqls?: { title: string; sql: string }[]
}

const routeLabel: Record<string, string> = {
  'direct-doc': '业务单据', 'direct-agg': '快速聚合', 'direct-derive': '推导口径', graph: '图关系',
  'entity-card': '实体总览', 'business-lookup': '业务库点查', 'semantic-cache': '语义缓存', 'need-intent': '需要澄清', 'no-topic': '主题未接入',
  llm: 'AI 生成', 'llm+repair': 'AI 生成·自修', knowledge: '知识库',
}
const QUICK_FALLBACK = ['本月销售额是多少', '本月销售额按省区', '本月销售额按战区', '买过烤肠的客户有哪些', '查一下昨天的订单明细']
// 【A15】推荐问句：后端给人工复核过（enabled）的真实问句，拿不到时回退固定清单 ——
// 推荐位在冷启动第一天也不能是空的。随登录名变化重取（推荐跟着人走）。
const quick = ref<string[]>(QUICK_FALLBACK)
const isUnsupportedSalesPersonnelSuggestion = (q: unknown) =>
  typeof q === 'string' && /区域经理|大区经理|销售经理|销售负责人/.test(q) && !/订单|下单|售后|费用|活动|巡店|促销/.test(q)
async function loadSuggest() {
  try {
    const r = await fetch(`/api/suggest${loginQuery()}`, { headers: authHeaders(false) })
    if (!r.ok) return
    const j = await r.json()
    if (Array.isArray(j.suggestions) && j.suggestions.length) {
      const supported = j.suggestions.filter((q: unknown): q is string => typeof q === 'string' && !isUnsupportedSalesPersonnelSuggestion(q))
      quick.value = supported.length ? supported : QUICK_FALLBACK
    }
  } catch { /* 推荐缺席不挡主流程：固定清单还在 */ }
}

// 【双供应商】设置页（`/#/settings` —— 好记的路径就是需求本身）。
// 热切换：保存 → 后端写 meta.kv + 进程内热改，**不需要重启**（`set_conf` 热锁）。
// 本地缓存只能恢复会话，不能授予设置权限；设置页必须等服务端管理接口确认后再显示。
const view = ref<'chat' | 'settings'>('chat')
const llmCfg = ref<any>(null)
const llmMsg = ref('')
const llmSaving = ref(false)
const llmSwitching = ref('')
const adminConfirmed = ref(false)
// 模板里挂了 5 处：普通函数每次重渲染都重算，改 computed 缓存（脚本侧调用一律 .value）
const hasAdminAccess = computed(() =>
  sessionValidated.value && !!sessionToken.value && loginName.value === 'admin' && adminConfirmed.value)
async function confirmAdminAccess(): Promise<void> {
  adminConfirmed.value = false
  if (!sessionValidated.value || !sessionToken.value || loginName.value !== 'admin') {
    if (location.hash === '#/settings') goChat()
    return
  }
  try {
    const r = await fetch('/api/admin/llm-config', { headers: authHeaders(false) })
    if (r.ok) {
      llmCfg.value = await r.json()
      adminConfirmed.value = true
      if (location.hash === '#/settings') {
        view.value = 'settings'
        loadSettingsPage()
      }
    } else if (location.hash === '#/settings') {
      goChat()
    }
  } catch {
    if (location.hash === '#/settings') goChat()
    // 设置入口保持隐藏；普通对话不受管理服务影响。
  }
}
function loadSettingsPage() {
  if (!hasAdminAccess.value) return
  llmMsg.value = ''
  void Promise.all([loadLlmConfig(), loadDbConfig(), loadSettingsCatalog(), loadQuality(), loadExemplars()])
}
function goSettings() {
  if (!hasAdminAccess.value) { goChat(); return }
  if (location.hash !== '#/settings') location.hash = '/settings'
  else { view.value = 'settings'; loadSettingsPage() }
}
function goChat() {
  history.replaceState(null, '', location.pathname + location.search)
  view.value = 'chat'
}
function handleSettingsDenied(status: number): boolean {
  if (status !== 401 && status !== 403) return false
  adminConfirmed.value = false
  llmCfg.value = null
  llmMsg.value = status === 401 ? '登录已失效，请重新登录' : '当前账号没有系统设置权限'
  goChat()
  return true
}
function handleHashChange() {
  if (location.hash !== '#/settings') { view.value = 'chat'; return }
  if (!hasAdminAccess.value) {
    if (sessionValidated.value && sessionToken.value && loginName.value === 'admin') void confirmAdminAccess()
    else goChat()
    return
  }
  view.value = 'settings'
  loadSettingsPage()
}
window.addEventListener('hashchange', handleHashChange)
async function loadLlmConfig() {
  if (!hasAdminAccess.value) return
  try {
    const r = await fetch(`/api/admin/llm-config${loginQuery()}`, { headers: authHeaders(false) })
    const j = await r.json()
    if (!r.ok) { handleSettingsDenied(r.status); llmMsg.value = j.error || `加载失败 ${r.status}`; return }
    llmCfg.value = j
  } catch { llmMsg.value = '加载失败（网络）' }
}
async function saveProvider(name: string) {
  if (!hasAdminAccess.value || llmSaving.value) return
  llmSaving.value = true
  llmSwitching.value = name
  llmMsg.value = `正在切换到 ${providerLabel(name)}…`
  try {
    const r = await fetch(`/api/admin/llm-provider${loginQuery()}`, {
      method: 'POST', headers: authHeaders(true),
      // login_name 必须在 body：后端从 JSON 体取身份，query 里的它不读（实测 401 的现场）
      body: JSON.stringify({ provider: name, login_name: sessionToken.value ? null : loginName.value, role_code: roleCode.value || null }),
    })
    const j = await r.json()
    if (!r.ok) { handleSettingsDenied(r.status); llmMsg.value = j.error || `保存失败 ${r.status}`; return }
    llmMsg.value = `已切换到 ${providerLabel(name)}，即时生效（无需重启）`
    await Promise.all([loadLlmConfig(), loadSettingsCatalog()])
  } catch { llmMsg.value = '保存失败（网络）' } finally {
    llmSaving.value = false
    llmSwitching.value = ''
  }
}
async function saveFallbackVision() {
  if (!hasAdminAccess.value || fallbackVisionSaving.value) return
  fallbackVisionSaving.value = true
  try {
    const ok = await postSettings('/api/admin/settings/fallback-vision', {
      provider: fallbackVisionProvider.value.trim() || null,
      login_name: sessionToken.value ? null : loginName.value,
      role_code: roleCode.value || null,
    }, fallbackVisionProvider.value ? `备用多模态模型已切换到 ${fallbackVisionProvider.value}` : '备用多模态模型已清除')
    if (!ok) fallbackVisionProvider.value = settingsCat.value?.fallback_vision_provider ?? ''
  } finally {
    fallbackVisionSaving.value = false
  }
}

// 【分析库热切换】与 LLM 供应商同一模子：目录（脱敏 host）+ 当前生效 + 保存即生效。
// DSN 只在服务端 settings.json，页面只见 host:port/db。
interface DbCfg {
  target: string
  targets: { name: string; host: string; type?: 'warehouse' | 'production_lookup'; current: boolean; builtin?: boolean; protected?: boolean; selectable?: boolean; purpose?: string }[]
  note?: string
}
const dbCfg = ref<DbCfg | null>(null)
const dbSaving = ref(false)
const dbSwitching = ref('')
async function loadDbConfig() {
  if (!hasAdminAccess.value) return
  try {
    const r = await fetch(`/api/admin/db-config${loginQuery()}`, { headers: authHeaders(false) })
    const j = await r.json()
    if (r.ok) dbCfg.value = j
    else handleSettingsDenied(r.status)
  } catch { /* 业务库段缺席不挡设置页（老服务端没有这个端点） */ }
}
async function saveDbTarget(name: string) {
  if (!hasAdminAccess.value || dbSaving.value) return
  dbSaving.value = true
  dbSwitching.value = name
  llmMsg.value = `正在切换分析数据库到 ${name}…`
  try {
    const r = await fetch(`/api/admin/db-target${loginQuery()}`, {
      method: 'POST', headers: authHeaders(true),
      // login_name 必须在 body：后端从 JSON 体取身份，query 里的它不读（实测 401 的现场）
      body: JSON.stringify({ target: name, login_name: sessionToken.value ? null : loginName.value, role_code: roleCode.value || null }),
    })
    const j = await r.json()
    if (!r.ok) { handleSettingsDenied(r.status); llmMsg.value = j.error || `切换失败 ${r.status}`; return }
    llmMsg.value = `分析数据库已切换到 ${name}，即时生效（无需重启）`
    await loadDbConfig()
  } catch { llmMsg.value = '切换失败（网络）' } finally {
    dbSaving.value = false
    dbSwitching.value = ''
  }
}

// 【页面编辑配置】mysql_targets / llm_keys 的增删改（写 settings 文件 + 内存热更新）。
// 明文只进不出：提交后页面只见脱敏 host 与「已配置」布尔。
interface MysqlTargetCatalogRow {
  name: string
  host: string
  type?: 'warehouse' | 'production_lookup'
  builtin: boolean
  protected: boolean
  query_target: boolean
  purpose: string
  current?: boolean
  selectable?: boolean
}
interface SettingsCatalog {
  mysql_targets: MysqlTargetCatalogRow[]
  llm_keys: { name: string; key_ready: boolean; protected?: boolean }[]
  llm_presets?: { name: string; label: string; base_url: string; model_fast: string; model_precise: string; thinking_levels: string[]; vision: string | null }[]
  llm_providers?: { name: string; base_url: string; model_fast: string; model_precise: string; thinking?: string; vision: string | null; key_ready: boolean }[]
  fallback_vision_provider?: string
  vision_candidates?: { name: string; supports_vision: boolean; vision_model: string | null; key_ready: boolean; selectable: boolean }[]
  effective_vision?: { provider: string; model: string; fallback: boolean } | null
  /** KB 管理入口授权（设置页「知识库入口权限」卡片初值；两个名单都空 = 仅管理员） */
  kb_manager_grants?: { roles: string[]; logins: string[] }
}
const settingsCat = ref<SettingsCatalog | null>(null)
const fallbackVisionSaving = ref(false)
const fallbackVisionProvider = ref('')
// 知识库入口权限卡片的输入态：逗号/顿号/空格/换行分隔的名单文本，保存时才解析成数组
const kbGrantsText = ref({ roles: '', logins: '' })
const kbGrantsSaving = ref(false)
/** 名单文本 → 数组（分隔符：逗号/顿号/分号/空白/换行；空段丢弃，服务端还做一道去重与卫生闸） */
function parseGrantList(text: string): string[] {
  return text.split(/[,，、;；\s]+/).map((s) => s.trim()).filter(Boolean)
}
async function saveKbGrants() {
  if (kbGrantsSaving.value) return
  kbGrantsSaving.value = true
  try {
    await postSettings('/api/admin/settings/kb-manager-grants', {
      roles: parseGrantList(kbGrantsText.value.roles),
      logins: parseGrantList(kbGrantsText.value.logins),
      login_name: sessionToken.value ? null : loginName.value,
    }, '知识库入口权限已保存并即时生效')
  } finally { kbGrantsSaving.value = false }
}
interface QualityData {
  days: number
  summary: { total: number; success_rate: number; p50_ms: number; p95_ms: number; llm_rate: number; cache_rate: number; avg_tokens: number; feedback_count: number; error_count: number }
  routes: { route: string; count: number; p95_ms: number; errors: number }[]
  feedback: { id: number; kind: string; detail: string; status: string; at: string; login_name: string; question: string; route: string }[]
}
const quality = ref<QualityData | null>(null)
const qualityDays = ref(7)
const qualityLoading = ref(false)
interface ExemplarRow {
  id: number; ds_id: string; question: string; sql: string; status: string
  validation_status: string; ai_review: string; reviewed_by: string; reviewed_at: string
  validated_at: string; validated_source: string; validated_fingerprint: string
  invalid_reason: string; metric_versions: string; created_at: string
}
const exemplars = ref<ExemplarRow[]>([])
const exemplarLoading = ref(false)
const exemplarBusy = ref<number | null>(null)
const exemplarFilter = ref('')
async function loadSettingsCatalog() {
  if (!hasAdminAccess.value) return
  try {
    const r = await fetch(`/api/admin/settings-catalog${loginQuery()}`, { headers: authHeaders(false) })
    const j = await r.json()
    if (r.ok) {
      settingsCat.value = j
      fallbackVisionProvider.value = j.fallback_vision_provider ?? ''
      // KB 入口授权名单回填（数组 → 逗号分隔文本；缺省/空 = 仅管理员，输入框留空即是该语义）
      kbGrantsText.value = {
        roles: (j.kb_manager_grants?.roles ?? []).join(', '),
        logins: (j.kb_manager_grants?.logins ?? []).join(', '),
      }
    } else handleSettingsDenied(r.status)
  } catch { /* 老服务端没有，静默 */ }
}
async function postSettings(url: string, body: object | null, okMsg: string, method = 'POST') {
  if (!hasAdminAccess.value) return false
  llmMsg.value = ''
  try {
    const r = await fetch(`${url}${loginQuery()}`, {
      method, headers: authHeaders(true),
      body: body ? JSON.stringify(body) : undefined,
    })
    const j = await r.json().catch(() => ({}))
    if (!r.ok) { handleSettingsDenied(r.status); llmMsg.value = j.error || `保存失败 ${r.status}`; return false }
    llmMsg.value = okMsg
    await Promise.all([loadSettingsCatalog(), loadDbConfig(), loadLlmConfig()])
    return true
  } catch { llmMsg.value = '保存失败（网络）'; return false }
}

// ── DB 目标：结构化表单（类型/地址/端口/库名/账号/密码 → 拼 DSN）+ 测试连通性 ──
const dbForm = ref({ name: '', type: 'warehouse', host: '', port: 9030, db: '', user: '', pass: '' })
const dbTest = ref<{ ok: boolean; ms?: number; version?: string; error?: string } | null>(null)
const dbTesting = ref(false)
const dbEditor = ref<'closed' | 'new' | 'edit'>('closed')
const dbEditingName = ref('')
const emptyDbForm = () => ({ name: '', type: 'warehouse', host: '', port: 9030, db: '', user: '', pass: '' })
function newDbTarget() {
  dbForm.value = emptyDbForm()
  dbEditingName.value = ''
  dbTest.value = null
  dbEditor.value = 'new'
}
function cancelDbEdit() {
  dbForm.value = emptyDbForm()
  dbEditingName.value = ''
  dbTest.value = null
  dbEditor.value = 'closed'
}
function dbTargetRemovable(target: { protected?: boolean; selectable?: boolean; current?: boolean }): boolean {
  // 当前项也允许发起删除，让服务端用实时配置做最终校验并返回明确原因。
  return !target.protected && target.selectable !== false
}
function composeDsn(): string {
  const f = dbForm.value
  const enc = encodeURIComponent
  const rawHost = f.host.trim()
  const host = rawHost.includes(':') && !rawHost.startsWith('[') ? `[${rawHost}]` : rawHost
  return `mysql://${enc(f.user)}:${enc(f.pass)}@${host}:${f.port}/${f.db}`
}
function splitHostPort(value: string, fallbackPort: number): [string, number] {
  if (value.startsWith('[')) {
    const end = value.indexOf(']')
    if (end > 0) return [value.slice(1, end), Number(value.slice(end + 2)) || fallbackPort]
  }
  const colon = value.lastIndexOf(':')
  if (colon > 0 && value.indexOf(':') === colon) {
    return [value.slice(0, colon), Number(value.slice(colon + 1)) || fallbackPort]
  }
  return [value, fallbackPort]
}
function dbFormValid(): boolean {
  const f = dbForm.value
  // 修改时账号/密码可同时留空以保留原凭据；新增必须完整填写。
  const credentialsOk = dbEditor.value === 'edit'
    ? ((!f.user.trim() && !f.pass) || !!(f.user.trim() && f.pass))
    : !!(f.user.trim() && f.pass)
  return !!(f.name.trim() && f.host.trim() && f.port && f.db.trim() && credentialsOk)
}
function editDbTarget(t: { name: string; host: string; type?: 'warehouse' | 'production_lookup' }) {
  const [hp, db] = t.host.split('/')
  const type = t.type ?? 'production_lookup'
  const [host, port] = splitHostPort(hp ?? '', type === 'warehouse' ? 9030 : 3306)
  dbForm.value = { name: t.name, type, host, port, db: db ?? '', user: '', pass: '' }
  dbEditingName.value = t.name
  dbTest.value = null
  dbEditor.value = 'edit'
}
async function testDbConn() {
  if (!hasAdminAccess.value) return
  if (!dbFormValid()) { llmMsg.value = '先把目标名/地址/端口/库名/账号填齐'; return }
  llmMsg.value = `正在测试数据库 ${dbForm.value.name.trim()}…`
  dbTesting.value = true; dbTest.value = null
  try {
    const r = await fetch(`/api/admin/settings/test-db${loginQuery()}`, {
      method: 'POST', headers: authHeaders(true),
      body: JSON.stringify({
        dsn: composeDsn(), name: dbForm.value.name.trim(), type: dbForm.value.type,
        keep_secret: dbEditor.value === 'edit' && !dbForm.value.user.trim() && !dbForm.value.pass,
        login_name: sessionToken.value ? null : loginName.value,
      }),
    })
    const result = await r.json().catch(() => ({ ok: false, error: `测试失败 ${r.status}` }))
    dbTest.value = result
    if (!r.ok) {
      if (!handleSettingsDenied(r.status)) llmMsg.value = result.error || `数据库测试失败 ${r.status}`
      return
    }
    llmMsg.value = result.ok ? `数据库 ${dbForm.value.name.trim()} 连通正常` : (result.error || '数据库连通性测试未通过')
  } catch {
    dbTest.value = { ok: false, error: '数据库测试失败（网络）' }
    llmMsg.value = dbTest.value.error ?? '数据库测试失败（网络）'
  } finally { dbTesting.value = false }
}
/** 内建分析目标名（settings 里那个出厂 DMS 数仓连接）—— 魔法字符串只许在这一处。 */
const BUILTIN_DB_TARGET = 'dms'
async function addTarget() {
  if (!hasAdminAccess.value || dbSaving.value) return
  if (!dbFormValid()) { llmMsg.value = '先把目标名/地址/端口/库名/账号填齐'; return }
  dbSaving.value = true
  const name = dbForm.value.name.trim()
  const keep = dbEditor.value === 'edit' && !dbForm.value.user.trim() && !dbForm.value.pass
  const effectiveNow = name.toLowerCase() === BUILTIN_DB_TARGET || dbCfg.value?.target === name
  try {
    const ok = await postSettings('/api/admin/settings/mysql-target',
      { name, dsn: composeDsn(), type: dbForm.value.type, keep_secret: keep, login_name: sessionToken.value ? null : loginName.value },
      effectiveNow
        ? `目标 ${name} 已保存并即时生效${keep ? '（保留原账号密码）' : ''}`
        : `目标 ${name} 已保存${keep ? '（保留原账号密码）' : ''}；需要启用时请点击“切换”`)
    if (ok) cancelDbEdit()
  } finally { dbSaving.value = false }
}
async function removeTarget(name: string) {
  const target = dbCfg.value?.targets.find((t) => t.name === name)
  if (!hasAdminAccess.value || !target || !dbTargetRemovable(target)) {
    llmMsg.value = target?.current
      ? '当前生效数据库不能删除，请先切换到其他目标'
      : target?.protected ? 'DMS 权限库受保护，不能删除' : '该数据库目标不可删除'
    return
  }
  if (!window.confirm(`确定删除数据库目标“${name}”吗？此操作不可撤销。`)) return
  const ok = await postSettings(`/api/admin/settings/mysql-target/${encodeURIComponent(name)}`, null, `目标 ${name} 已删除`, 'DELETE')
  if (ok && dbEditingName.value === name) cancelDbEdit()
}

// ── LLM：预设下拉（自动填 url/模型/思考档/多模态）+ key + 测试连通性 ──
const llmForm = ref({ preset: '', name: '', base_url: '', model_fast: '', model_precise: '', thinking: 'off', vision: '', key: '' })
const llmTest = ref<{ ok: boolean; ms?: number; usage?: { prompt_tokens?: number; completion_tokens?: number }; error?: string } | null>(null)
const llmTesting = ref(false)
const llmEditor = ref<'closed' | 'new' | 'edit'>('closed')
const llmEditingName = ref('')
const emptyLlmForm = () => ({ preset: '', name: '', base_url: '', model_fast: '', model_precise: '', thinking: 'off', vision: '', key: '' })
interface LlmProviderRow {
  name: string; base_url: string; model_fast: string; model_precise: string
  thinking: string; vision: boolean; vision_model: string | null; key_ready: boolean; custom: boolean
}
const llmProviderRows = computed<LlmProviderRow[]>(() => {
  const custom = settingsCat.value?.llm_providers ?? []
  const rows = new Map<string, LlmProviderRow>()
  for (const p of llmCfg.value?.providers ?? []) {
    const key = String(p.name).toLowerCase()
    const previous = rows.get(key)
    rows.set(key, {
      name: previous?.name ?? p.name,
      base_url: p.base_url || previous?.base_url || '',
      model_fast: p.model_fast || previous?.model_fast || '',
      model_precise: p.model_precise || previous?.model_precise || '',
      thinking: p.thinking || previous?.thinking || 'off',
      vision: !!p.vision || !!previous?.vision,
      // 新接口直接返回视觉模型名；布尔值兼容热更新期间仍在运行的旧服务端。
      vision_model: typeof p.vision === 'string'
        ? p.vision
        : (p.vision ? (previous?.vision_model ?? p.model_fast ?? p.model_precise ?? null) : (previous?.vision_model ?? null)),
      key_ready: !!p.key_ready || !!previous?.key_ready,
      custom: previous?.custom ?? false,
    })
  }
  for (const c of custom) {
    const key = c.name.toLowerCase()
    const previous = rows.get(key)
    rows.set(key, {
      name: c.name,
      base_url: c.base_url,
      model_fast: c.model_fast,
      model_precise: c.model_precise || c.model_fast,
      thinking: c.thinking ?? 'none',
      vision: !!c.vision,
      vision_model: c.vision,
      key_ready: !!c.key_ready || !!previous?.key_ready,
      custom: true,
    })
  }
  return [...rows.values()]
})
const visionCandidates = computed(() => (settingsCat.value?.vision_candidates ?? []).filter((p) => p.selectable))
const primaryHasVision = computed(() => !!llmCfg.value?.effective?.vision)
const selectedFallbackVision = computed(() => visionCandidates.value.find((p) => p.name.toLowerCase() === fallbackVisionProvider.value.toLowerCase()))
function providerLabel(name: string): string {
  return ({ qwen: '千问（Qwen）', deepseek: 'DeepSeek' } as Record<string, string>)[name.toLowerCase()] || name
}
function newLlmProvider() {
  llmForm.value = emptyLlmForm()
  llmEditingName.value = ''
  llmTest.value = null
  llmEditor.value = 'new'
}
function cancelLlmEdit() {
  llmForm.value = emptyLlmForm()
  llmEditingName.value = ''
  llmTest.value = null
  llmEditor.value = 'closed'
}
function llmProviderRemovable(p: LlmProviderRow): boolean {
  // 主模型/备用模型是否正在占用必须由服务端按实时状态判断。
  return p.custom
}
function onPreset() {
  // custom = OpenAI 兼容手动填写：没有可套的预设，保留已填内容并给占位名，不做静默无操作
  if (llmForm.value.preset === 'custom') {
    llmForm.value.name = llmForm.value.name || 'custom'
    return
  }
  const p = settingsCat.value?.llm_presets?.find((x) => x.name === llmForm.value.preset)
  if (!p) return
  llmForm.value.name = llmForm.value.name || p.name
  llmForm.value.base_url = p.base_url
  llmForm.value.model_fast = p.model_fast
  llmForm.value.model_precise = p.model_precise
  llmForm.value.thinking = p.thinking_levels.includes('off') ? 'off' : (p.thinking_levels[0] ?? 'none')
  llmForm.value.vision = p.vision ?? ''
}
function editLlmProvider(c: { name: string; base_url: string; model_fast: string; model_precise: string; thinking: string; vision: string | null }) {
  llmForm.value = {
    preset: '', name: c.name, base_url: c.base_url,
    model_fast: c.model_fast, model_precise: c.model_precise,
    thinking: c.thinking, vision: c.vision ?? '', key: '',
  }
  llmEditingName.value = c.name
  llmTest.value = null
  llmEditor.value = 'edit'
}
function llmFormValid(): boolean {
  const f = llmForm.value
  // key 留空 = 保留已存 key（修改已有供应商/内建覆盖时）；新增必须填 key
  const name = f.name.trim().toLowerCase()
  const exists = (settingsCat.value?.llm_providers ?? []).some((c) => c.name.toLowerCase() === name)
    || (llmCfg.value?.providers ?? []).some((p: any) => String(p.name).toLowerCase() === name && p.key_ready)
  return !!(f.name.trim() && f.base_url.trim() && (f.model_fast.trim() || f.model_precise.trim()) && (f.key.trim() || exists))
}
async function testLlmConn() {
  if (!hasAdminAccess.value) return
  const f = llmForm.value
  const model = f.model_fast.trim() || f.model_precise.trim()
  if (!f.base_url.trim() || !model || !f.key.trim()) {
    llmMsg.value = '测试要先填 url / 模型 / key'; return
  }
  llmMsg.value = `正在测试模型 ${model}…`
  llmTesting.value = true; llmTest.value = null
  try {
    const r = await fetch(`/api/admin/settings/test-llm${loginQuery()}`, {
      method: 'POST', headers: authHeaders(true),
      body: JSON.stringify({
        base_url: f.base_url.trim(), model, key: f.key.trim(),
        login_name: sessionToken.value ? null : loginName.value,
      }),
    })
    const result = await r.json().catch(() => ({ ok: false, error: `测试失败 ${r.status}` }))
    llmTest.value = result
    if (!r.ok) {
      if (!handleSettingsDenied(r.status)) llmMsg.value = result.error || `模型测试失败 ${r.status}`
      return
    }
    llmMsg.value = result.ok ? `模型 ${model} 连通正常` : (result.error || '模型连通性测试未通过')
  } catch {
    llmTest.value = { ok: false, error: '模型测试失败（网络）' }
    llmMsg.value = llmTest.value.error ?? '模型测试失败（网络）'
  } finally { llmTesting.value = false }
}
async function addLlmProvider() {
  if (!hasAdminAccess.value || llmSaving.value) return
  if (!llmFormValid()) { llmMsg.value = '名字 / url / 模型 / key 都要填'; return }
  llmSaving.value = true
  const f = llmForm.value
  const runtimeName = f.name.trim().toLowerCase()
  const editingRuntime = llmCfg.value?.provider?.toLowerCase() === runtimeName
    || fallbackVisionProvider.value.toLowerCase() === runtimeName
  try {
    const ok = await postSettings('/api/admin/settings/llm-provider', {
      name: f.name.trim(), base_url: f.base_url.trim(),
      model_fast: f.model_fast.trim(), model_precise: f.model_precise.trim(),
      thinking: f.thinking, vision: f.vision.trim() || null, key: f.key.trim(),
      login_name: sessionToken.value ? null : loginName.value,
    }, editingRuntime
      ? `供应商 ${providerLabel(f.name)} 已保存并即时生效`
      : `供应商 ${providerLabel(f.name)} 已保存；需要启用时请点击“切换”`)
    if (ok) cancelLlmEdit()
  } finally { llmSaving.value = false }
}
async function removeLlmProvider(name: string) {
  const provider = llmProviderRows.value.find((p) => p.name === name)
  if (!hasAdminAccess.value || !provider || !llmProviderRemovable(provider)) {
    llmMsg.value = '内建模型供应商不可删除'
    return
  }
  if (!window.confirm(`确定删除模型供应商“${name}”吗？关联配置将一并移除。`)) return
  const ok = await postSettings(`/api/admin/settings/llm-provider/${encodeURIComponent(name)}`, null, `供应商 ${name} 已删除`, 'DELETE')
  if (ok && llmEditingName.value === name) cancelLlmEdit()
}
/** key 删除钮的禁用原因文案：protected / 主模型占用 / 备用占用 三种各说各的（原来恒写「删除该 Key」）。 */
function llmKeyDelTitle(k: { name: string; protected?: boolean }): string {
  if (k.protected) return '基础 llm_api_key 仍在兜底，不能单独删除'
  if (llmCfg.value?.provider?.toLowerCase() === k.name.toLowerCase()) return '当前生效供应商的 key 不能删除'
  if (fallbackVisionProvider.value.toLowerCase() === k.name.toLowerCase()) return '备用多模态供应商的 key 不能删除'
  return '删除该 Key'
}
async function removeLlmKey(name: string) {
  if (!hasAdminAccess.value) return
  if (settingsCat.value?.llm_keys.find((key) => key.name.toLowerCase() === name.toLowerCase())?.protected) {
    llmMsg.value = '该 Key 仍由基础 llm_api_key 配置兜底，需先迁移基础配置后再删除'
    return
  }
  if (llmCfg.value?.provider?.toLowerCase() === name.toLowerCase()) {
    llmMsg.value = '当前生效供应商的 key 不能单独删除，请先切换模型'
    return
  }
  if (fallbackVisionProvider.value.toLowerCase() === name.toLowerCase()) {
    llmMsg.value = '备用多模态供应商的 key 不能删除，请先清空或切换备用模型'
    return
  }
  if (!window.confirm(`确定删除“${name}”的 API Key 吗？`)) return
  await postSettings(`/api/admin/settings/llm-key/${encodeURIComponent(name)}`, null, `${name} 的 key 已删除`, 'DELETE')
}

// 【S1】artifact 右侧预览面板（datanote 形态：Codex 式分屏，可拖宽、记忆宽度、深链拦截）。
// 面板内容全部是服务端的 CSP 沙箱页（无 allow-same-origin ⇒ 透明源，碰不到本页与 Cookie）。
const previewW = ref('46%')
const preview = ref<{ sourceUrl: string; html?: string; title: string; loading?: boolean; error?: string } | null>(null)
let previewRequestId = 0
async function openPreview(url: string, title: string, retryAuth = true, version?: number) {
  const requestId = ++previewRequestId
  const sourceUrl = url.split('?')[0]
  // 【D6】版本快照：缺省（undefined）= 该产物 id 自身那版；显式切产物/深链打开时清空回看态
  previewVer.value = version ?? null
  pvVersionsOpen.value = false
  pvPromoteOpen.value = false
  preview.value = { sourceUrl, title, loading: true }
  try {
    // iframe 导航无法附带 Bearer；父页先认证取 HTML，再交给无同源权限的 sandbox Blob。
    const r = await fetch(sourceUrl + previewAuthQuery(version ? `version=${version}` : ''), { headers: authHeaders(false) })
    if (r.status === 401 && retryAuth) {
      await handleSessionExpired()
      if (sessionToken.value) void openPreview(sourceUrl, title, false, version)
      return
    }
    if (!r.ok) {
      const [body, raw] = await readBody(r)
      throw new Error(errMsg(r, body, raw))
    }
    const html = await r.text()
    if (requestId !== previewRequestId || !preview.value || preview.value.sourceUrl !== sourceUrl) return
    preview.value = { sourceUrl, html, title }
  } catch (e) {
    if (requestId === previewRequestId && preview.value?.sourceUrl === sourceUrl) {
      preview.value = { sourceUrl, title, error: String(e) }
    }
  }
}
async function loadQuality() {
  if (!hasAdminAccess.value) return
  qualityLoading.value = true
  try {
    const r = await fetch(`/api/admin/quality?days=${qualityDays.value}${loginQuery().replace('?', '&')}`, { headers: authHeaders(false) })
    const [body, raw] = await readBody(r)
    if (r.ok) quality.value = body as QualityData
    else if (r.status !== 403) llmMsg.value = `质量数据加载失败：${errMsg(r, body, raw)}`
  } catch { llmMsg.value = '质量数据加载失败（网络）' }
  finally { qualityLoading.value = false }
}
async function loadExemplars() {
  if (!hasAdminAccess.value) return
  exemplarLoading.value = true
  try {
    const status = exemplarFilter.value ? `&status=${encodeURIComponent(exemplarFilter.value)}` : ''
    const r = await fetch(`/api/admin/exemplars${loginQuery()}${status}`, { headers: authHeaders(false) })
    const [body, raw] = await readBody(r)
    if (r.ok) exemplars.value = Array.isArray((body as any)?.exemplars) ? (body as any).exemplars : []
    else if (r.status !== 403) llmMsg.value = `样例审计加载失败：${errMsg(r, body, raw)}`
  } catch { llmMsg.value = '样例审计加载失败（网络）' }
  finally { exemplarLoading.value = false }
}
async function setExemplarStatus(id: number, status: 'enabled' | 'disabled') {
  if (!hasAdminAccess.value || exemplarBusy.value !== null) return
  exemplarBusy.value = id
  try {
    const r = await fetch(`/api/admin/exemplars/${id}/status${loginQuery()}`, {
      method: 'POST', headers: authHeaders(),
      body: JSON.stringify({ status, login_name: sessionToken.value ? null : loginName.value, role_code: roleCode.value || null }),
    })
    const [body, raw] = await readBody(r)
    if (!r.ok) showToast(errMsg(r, body, raw))
    else showToast(status === 'enabled' ? '真实只读执行验证通过，样例已启用' : '样例已禁用')
    await loadExemplars()
  } catch { showToast('样例状态更新失败（网络）') }
  finally { exemplarBusy.value = null }
}
function validationLabel(s: string): string {
  return ({ valid: '执行已验证', unverified: '待执行验证', invalid: '验证失败', stale: '已失效' } as Record<string,string>)[s] || s
}
// 反馈处理/重开的在飞闸：无 busy 态时双击会发两个 POST（同 exemplarBusy 一个手法）
const feedbackBusy = ref<number | null>(null)
async function resolveFeedback(id: number, status: 'open' | 'resolved') {
  if (!hasAdminAccess.value || feedbackBusy.value !== null) return
  feedbackBusy.value = id
  try {
    const r = await fetch(`/api/admin/feedback/${id}/status${loginQuery()}`, {
      method: 'POST', headers: authHeaders(),
      body: JSON.stringify({ status, login_name: sessionToken.value ? null : loginName.value, role_code: roleCode.value || null }),
    })
    if (r.ok) loadQuality()
    else showToast('反馈状态更新失败')
  } catch { showToast('反馈状态更新失败（网络）') }
  finally { feedbackBusy.value = null }
}
function fmtLatency(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`
  if (ms < 60000) return `${(ms / 1000).toFixed(ms < 10000 ? 1 : 0)}s`
  return `${Math.floor(ms / 60000)}m${Math.round((ms % 60000) / 1000)}s`
}
function closePreview() {
  previewRequestId++
  preview.value = null
  previewVer.value = null
  pvVersionsOpen.value = false
  pvPromoteOpen.value = false
}
async function downloadPreview() {
  const current = preview.value
  if (!current) return
  // 下载与正在回看的版本一致（?version=N 与 view 同一条服务端判据）
  const verQ = previewVer.value ? `version=${previewVer.value}` : ''
  const url = current.sourceUrl.replace('/view', '/download') + previewAuthQuery(verQ)
  try {
    let r = await fetch(url, { headers: authHeaders(false) })
    if (r.status === 401) {
      await handleSessionExpired()
      if (!sessionToken.value) return
      r = await fetch(url, { headers: authHeaders(false) })
    }
    if (!r.ok) { showToast(`下载失败 ${r.status}`); return }
    const href = URL.createObjectURL(await r.blob())
    const a = document.createElement('a')
    a.href = href; a.download = `${current.title || 'report'}.html`; a.click()
    setTimeout(() => URL.revokeObjectURL(href), 1000) // 0ms 回收与下载起动有竞态（大文件/Firefox 会下到空 blob），宽限 1s
  } catch { showToast('下载失败（网络）') }
}
function openPreviewWindow() {
  const p = preview.value
  if (!p?.html) { showToast('报表尚未加载完成'); return }
  // 直接访问 view URL 无法携带 Bearer。新窗口只充当外壳，报表仍放在无同源权限的
  // sandbox iframe 中：能铺满浏览器，同时不能接触主系统 DOM、Cookie 或会话存储。
  const opened = window.open(
    'about:blank', '_blank',
    `popup=yes,width=${screen.availWidth},height=${screen.availHeight},left=0,top=0`,
  )
  if (!opened) { showToast('浏览器阻止了新窗口，请允许本站弹出窗口'); return }
  opened.opener = null
  opened.document.title = p.title
  opened.document.documentElement.style.cssText = 'width:100%;height:100%;margin:0;background:#fff'
  opened.document.body.style.cssText = 'width:100%;height:100%;margin:0;overflow:hidden;background:#fff'
  const frame = opened.document.createElement('iframe')
  frame.setAttribute('sandbox', 'allow-scripts')
  frame.title = p.title
  frame.style.cssText = 'display:block;width:100%;height:100%;border:0;background:#fff'
  frame.srcdoc = p.html
  opened.document.body.appendChild(frame)
}
// 【分享】发链接并复制到剪贴板（fallback：老浏览器用 prompt 让用户自己 Ctrl+C）
async function shareArtifact(id: number | null) {
  if (!id) return
  try {
    // loginQuery 自带 `?`（或空串）—— 此前多叠一道 replace('?','&') 把 URL 拼成
    // `share&login_name=…`（没有 ?），路由不匹配恒 404：「点了没反应」的真根因
    let r = await fetch(`/api/artifact/${id}/share${loginQuery()}`, { method: 'POST', headers: authHeaders() })
    if (r.status === 401) {
      await handleSessionExpired()
      if (!sessionToken.value) return
      r = await fetch(`/api/artifact/${id}/share${loginQuery()}`, { method: 'POST', headers: authHeaders() })
    }
    const j = await r.json().catch(() => ({}))
    if (!r.ok || !j.share_url) { showToast(j.error || `分享失败 ${r.status}`); return }
    const url = new URL(j.share_url, location.origin).href
    try { await navigator.clipboard.writeText(url) } catch { window.prompt('复制分享链接：', url); return }
    showToast('分享链接已复制（免登录只读）')
  } catch { showToast('分享失败（网络）') }
}
// ─────────────────── 【D6】版本历史 / 表格导出 / 引用到会话 ───────────────────
// 版本回看：`?version=N` 由服务端在同一 (conv,kind,title) 链内解析，权限判据与 view 同一条。
const previewVer = ref<number | null>(null)
const pvVersionsOpen = ref(false)
const pvVersions = ref<{ id: number; version: number; created_at: string; latest: boolean }[] | null>(null)
const pvPromoteOpen = ref(false)

// loginQuery() 自带前导 `?`（或空串）；有额外参数时把它的 `?` 改 `&` 接到后面
function previewAuthQuery(extra = ''): string {
  return extra ? `?${extra}${loginQuery().replace('?', '&')}` : loginQuery()
}

async function toggleVersions() {
  pvVersionsOpen.value = !pvVersionsOpen.value
  pvPromoteOpen.value = false
  const id = artifactIdOf(preview.value?.sourceUrl)
  if (!pvVersionsOpen.value || !id) return
  pvVersions.value = null
  try {
    const r = await fetch(`/api/artifact/${id}/versions${loginQuery()}`, { headers: authHeaders(false) })
    const j = await r.json().catch(() => ({}))
    if (!r.ok) { showToast(j.error || `版本列表加载失败 ${r.status}`); pvVersionsOpen.value = false; return }
    pvVersions.value = Array.isArray(j.versions) ? j.versions : []
  } catch { showToast('版本列表加载失败（网络）'); pvVersionsOpen.value = false }
}

// 回看指定版本：复用 openPreview 的 Bearer fetch + srcdoc 沙箱形态，只在取数 URL 上带 version
async function openVersion(version: number) {
  const p = preview.value
  if (!p) return
  pvVersionsOpen.value = false
  await openPreview(p.sourceUrl, p.title, true, version)
}

async function exportPreview(fmt: 'csv' | 'xlsx') {
  const p = preview.value
  const id = artifactIdOf(p?.sourceUrl)
  if (!p || !id) return
  const verQ = previewVer.value ? `fmt=${fmt}&version=${previewVer.value}` : `fmt=${fmt}`
  const url = `/api/artifact/${id}/export${previewAuthQuery(verQ)}`
  try {
    let r = await fetch(url, { headers: authHeaders(false) })
    if (r.status === 401) {
      await handleSessionExpired()
      if (!sessionToken.value) return
      r = await fetch(url, { headers: authHeaders(false) })
    }
    if (!r.ok) {
      const j = await r.json().catch(() => ({}))
      showToast(j.error || `导出失败 ${r.status}`)
      return
    }
    const href = URL.createObjectURL(await r.blob())
    const a = document.createElement('a')
    a.href = href; a.download = `${p.title || 'report'}.${fmt}`; a.click()
    setTimeout(() => URL.revokeObjectURL(href), 1000) // 0ms 回收与下载起动有竞态（大文件/Firefox 会下到空 blob），宽限 1s
  } catch { showToast('导出失败（网络）') }
}

// 引用到会话：服务端复核产物读权限与目标会话属主（fail-closed）；
// 成功即让目标会话缓存失效，下次打开能看到 promote 事件卡。
async function promoteArtifact(targetConvId: number, targetTitle: string) {
  const p = preview.value
  const id = artifactIdOf(p?.sourceUrl)
  if (!p || !id) return
  pvPromoteOpen.value = false
  try {
    const r = await fetch(`/api/artifact/${id}/promote${loginQuery()}`, {
      method: 'POST', headers: authHeaders(),
      body: JSON.stringify({
        target_conv_id: targetConvId,
        version: previewVer.value,
        note: null,
        login_name: sessionToken.value ? null : loginName.value,
        role_code: roleCode.value || null,
      }),
    })
    const j = await r.json().catch(() => ({}))
    if (!r.ok) { showToast(j.error || `引用失败 ${r.status}`); return }
    turnsByConv.delete(targetConvId)
    if (targetConvId === curConvId.value) void openConv(targetConvId)
    showToast(`已引用到「${targetTitle}」`)
  } catch { showToast('引用失败（网络）') }
}

// 轻 toast（右下角浮层 3s）：llmMsg 只在设置页渲染，聊天页的操作反馈全走这里
const toastMsg = ref('')
let toastTimer: ReturnType<typeof setTimeout> | undefined
function showToast(msg: string) {
  toastMsg.value = msg
  clearTimeout(toastTimer)
  toastTimer = setTimeout(() => (toastMsg.value = ''), 3000)
}
function artifactIdOf(url: string | undefined): number | null {
  const m = url?.match(/\/api\/artifact\/(\d+)\/view/)
  return m ? Number(m[1]) : null
}
// 深链拦截：聊天气泡里渲染出的 /api/artifact/N/view 链接不跳页，开右侧预览面板
function onChatClick(e: MouseEvent) {
  // 只拦无修饰键的左键：Ctrl/⌘/中键点击是想新窗口打开的用户，别强行拽进面板；
  // 已 defaultPrevented 说明内层元素自己处理过了，不重复拦。
  if (e.defaultPrevented || e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) return
  const a = (e.target as HTMLElement).closest?.('a[href*="/api/artifact/"]') as HTMLAnchorElement | null
  if (!a) return
  const href = a.getAttribute('href')
  if (!href) return
  const target = new URL(href, location.origin)
  if (target.origin !== location.origin || !/^\/api\/artifact\/\d+\/view$/.test(target.pathname)) return
  e.preventDefault()
  openPreview(target.pathname, a.textContent?.trim() || '产物预览')
}
function startDrag(e: MouseEvent) {
  e.preventDefault()
  const move = (ev: MouseEvent) => {
    const w = Math.min(Math.max(window.innerWidth - ev.clientX, 320), window.innerWidth * 0.75)
    previewW.value = `${Math.round(w)}px`
  }
  const up = () => { window.removeEventListener('mousemove', move); window.removeEventListener('mouseup', up) }
  window.addEventListener('mousemove', move)
  window.addEventListener('mouseup', up)
}
// 能力切换（K5 forced intent 的前端占位）：auto 不传，其余原样透传给 /api/ask 的 body.intent
const CAPS: { v: 'auto' | 'data' | 'knowledge'; t: string }[] = [
  { v: 'auto', t: '自动' }, { v: 'data', t: '问数' }, { v: 'knowledge', t: '知识库' },
]

interface Conv { id: number; title: string; time: string }

const question = ref('')
const intent = ref<'auto' | 'data' | 'knowledge'>('auto')
const loginName = ref(sessionStorage.getItem('dms-login') || '')
const roleCode = ref('')
const sessionToken = ref(sessionStorage.getItem('dms-session') || '')
// sessionStorage 只能说明“曾登录”，不能证明当前 token 仍有效；管理入口必须等 /api/roles 校验通过。
const sessionValidated = ref(false)
/** DMS 的 x-access-token。只由父页 postMessage 注入并驻留内存；多角色选择后用它重换会话。 */
const dmsToken = ref('')
const dmsHomeEmbedded = new URLSearchParams(location.search).get('embed') === 'dms-home'
const embedded = ref(dmsHomeEmbedded || /token=/.test(location.hash))
const convs = ref<Conv[]>([])        // 会话列表（一个会话含多轮）
const curConvId = ref<number | null>(null)
const draftTurns = ref<Turn[]>([])
const turnsByConv = reactive(new Map<number, Turn[]>())
// 每个会话拥有自己的运行中 Turn；切换只换视图，不销毁另一个会话的请求与思考过程。
const turns = computed<Turn[]>({
  get: () => curConvId.value == null ? draftTurns.value : (turnsByConv.get(curConvId.value) ?? []),
  set: (v) => {
    if (curConvId.value == null) draftTurns.value = v
    else turnsByConv.set(curConvId.value, v)
  },
})
function turnRunning(t: Turn): boolean { return t.loading === true || t.streaming === true }
const chatEl = ref<HTMLElement>()
// 移动端（≤820px）侧栏改为抽屉：顶栏 ☰ 拉开，遮罩/点会话收起；桌面端恒为 false 不影响布局
const sideOpen = ref(false)
const health = ref('检查中…')
const healthOk = ref(false)
const healthBusy = ref(false)
// 与 index.html 的首帧内联脚本**同一表达式**：没存过偏好时跟随系统，否则暗色系统的新用户第一次进来是亮色
const theme = ref(localStorage.getItem('theme')
  || (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'))
// 知识库管理面（上传/列表/删除）。此前前端**没有任何上传入口** ——
// 后端 `/api/kb/upload` 早通了、`kb_eval.py` 也在用，但用户在界面上传不了文件。
const kbOpen = ref(false)
// 【KB 管理闸】管理入口显隐的唯一依据：服务端确认（与管理员判定同源 —— `confirmAdminAccess`
// 探 `/api/admin/llm-config`，这里探 `/api/kb/spaces`）。该端点已过管理闸：200 且 `kb_manager:true`
// = 管理人；403 = 非授权（未配置授权时仅管理员能过）。隐藏只是体验，安全闸在服务端。
const kbManager = ref(false)
async function confirmKbManager(): Promise<void> {
  kbManager.value = false
  if (!sessionValidated.value || !sessionToken.value) return
  try {
    const r = await fetch('/api/kb/spaces', { headers: authHeaders(false) })
    kbManager.value = r.ok && (await r.json()).kb_manager === true
  } catch { kbManager.value = false }
}
const knowledgeSpaceId = ref(localStorage.getItem('dms-kb-space') || '')
const knowledgeSpaceName = ref(localStorage.getItem('dms-kb-space-name') || '')
function rememberKnowledgeSpace(value: { space_id: string; name: string }) {
  knowledgeSpaceId.value = value.space_id
  knowledgeSpaceName.value = value.name
  localStorage.setItem('dms-kb-space', value.space_id)
  localStorage.setItem('dms-kb-space-name', value.name)
}
const sending = computed(() => turns.value.some(turnRunning))
/** 【子任务面板】当前会话最近一轮有进度数据的深度问答（进行中或刚完成；历史会话不存进度，不显示）。 */
const deepTaskTurn = computed(() => {
  for (let i = turns.value.length - 1; i >= 0; i--) {
    const t = turns.value[i]
    if (t.role === 'ai' && t.mode === 'deep' && (t.loading || t.progress?.length || t.tasks?.length)) return t
  }
  return null
})
function convRunning(id: number): boolean { return turnsByConv.get(id)?.some(turnRunning) ?? false }

// ── 【排队追问】每个会话一条独立队列（key = convId，切换会话不串）──
// 当前 run 还在跑时，输入框/快捷 pill 的新问句进本会话队列；run 结束（成败都算）
// 在 send 的 finally 里出队续发。重试/下钻/周报这类带参调用不排队（语义已固定，维持直接早退）。
type QueuedAsk = QueuedAskSnapshot
const queueByConv = reactive(new Map<number, QueuedAsk[]>())
const activeAskControllers = new Map<string, { controller: AbortController; turn: Turn }>()
const curQueue = computed<QueuedAsk[]>(() => (curConvId.value == null ? [] : (queueByConv.get(curConvId.value) ?? [])))
function enqueueAsk(text: string, refs: string[]) {
  const convId = curConvId.value
  if (convId == null) return
  const queue = queueByConv.get(convId) ?? []
  queue.push(snapshotQueuedAsk(uuid(), text, refs, {
    intent: intent.value,
    mode: deepMode.value ? 'deep' : 'lite',
    spaceId: knowledgeSpaceId.value || null,
  }))
  queueByConv.set(convId, queue)
}
function cancelQueued(id: string) {
  const convId = curConvId.value
  if (convId == null) return
  const queue = queueByConv.get(convId)
  if (queue) queueByConv.set(convId, queue.filter((item) => item.id !== id))
}
function drainQueue(convId: number) {
  const next = queueByConv.get(convId)?.shift()
  if (next) void send(next.text, {
    targetConvId: convId, refs: next.refs,
    forceIntent: next.forceIntent, forceMode: next.forceMode, spaceId: next.spaceId,
  })
}

function abortTurn(t: Turn, reason: Turn['abortReason']) {
  if (t.convId != null) queueByConv.delete(t.convId)
  const active = t.turnKey ? activeAskControllers.get(t.turnKey) : undefined
  if (!active || active.controller.signal.aborted) return
  t.abortReason = reason
  active.controller.abort()
}

function abortAllTurns(reason: Turn['abortReason']) {
  queueByConv.clear()
  for (const { controller, turn } of activeAskControllers.values()) {
    turn.abortReason = reason
    if (!controller.signal.aborted) controller.abort()
  }
}

function stopGeneration(t: Turn) {
  abortTurn(t, 'user')
}

// ── 【Y5 插话】运行中给当前任务插一条修正指令（「不是这个口径，按 X 重算」）──
// 运行结束自动隐藏（模板绑 sending）；已受理条数按会话记，新 run 发出时清零
// （服务端信箱同步清空：steer 只属于正在跑的那一次计算）。
const steerText = ref('')
const steerBusy = ref(false)
const steerCountByConv = reactive(new Map<number, number>())
const curSteerCount = computed(() => (curConvId.value == null ? 0 : (steerCountByConv.get(curConvId.value) ?? 0)))
async function sendSteer() {
  const text = steerText.value.trim()
  const convId = curConvId.value
  if (!text || convId == null || steerBusy.value) return
  steerBusy.value = true
  try {
    const r = await okJson<{ queued?: number }>(await fetch(`/api/chat/conv/${convId}/steer`, {
      method: 'POST', headers: authHeaders(),
      body: JSON.stringify({ content: text, login_name: sessionToken.value ? null : loginName.value }),
    }), '插话失败', () => true, showToast)
    if (!r) return
    steerCountByConv.set(convId, typeof r.queued === 'number' ? r.queued : curSteerCount.value + 1)
    steerText.value = ''
    showToast('插话已受理：将在当前计算的下一安全点并入重算')
  } catch {
    showToast('插话失败（网络）')
  } finally {
    steerBusy.value = false
  }
}

// ── 【引用上轮】输入框上方的引用 chip 区；发送时随 body 带 refs，发出/入队即清空 ──
const pendingRefs = ref<string[]>([])
/** 引用素材 = 问题 + 该轮结论摘要（整体截到 300 字）。摘要复用这轮已经算出的结论
 *  （深度页 insight → 结果视图 insight → 按需解读 → 知识库正文 → 复合综合分析），
 *  都没有就退成「N 行结果」的客观描述 —— 不为一个引用再多调一次模型。 */
function refTextOf(t: Turn): string {
  const ask = (t.question ?? '').trim()
  const r = t.result
  let summary = ''
  if (t.page?.insight) summary = t.page.insight
  else if (r?.view?.insight) summary = r.view.insight
  else if (t.analysis?.insight) summary = t.analysis.insight
  else if (r?.kind === 'text') summary = r.markdown ?? ''
  else if (r?.subs?.length) summary = compoundAnalysis(r)
  summary = userFacingMarkdown(summary).replace(/\s+/g, ' ').trim()
  if (!summary && r) summary = `返回 ${r.row_count} 行${r.route ? `（${routeLabel[r.route] || r.route}）` : ''}`
  return `问题：${ask}\n结论：${summary || '（该轮无文字结论）'}`.slice(0, 300)
}
function quoteTurn(t: Turn) {
  const text = refTextOf(t)
  if (text && !pendingRefs.value.includes(text)) pendingRefs.value.push(text)
}
function refChipLabel(text: string): string {
  const ask = text.split('\n')[0].replace(/^问题：/, '')
  return ask.length > 24 ? `${ask.slice(0, 24)}…` : ask
}

// 【使用统计】头部入口 + UsagePanel 弹窗（组件内自拉 /api/usage/summary）
const usageOpen = ref(false)
// 【提示词包】头部入口 + SkillsPanel 弹窗（组件内自管 /api/skills 读写；写权限在后端 admin_only）
const skillsOpen = ref(false)
// 【数据地图】头部入口 + DataMapPanel 弹窗（组件内自拉 /api/datamap/*；接受/拒绝按钮只对 admin 渲染）
const datamapOpen = ref(false)
// 【SQL 审计】头部入口 + SqlAuditPanel 抽屉（组件内自拉 /api/audit/sql；全员只读无写口）
const auditOpen = ref(false)

// ── 【分支会话】AI 轮气泡的「⑂ 分支」：把该轮（含）之前的消息复制成新会话并切过去 ──
const branchBusy = ref(false)
async function branchTurn(t: Turn, ti: number) {
  const convId = t.convId
  if (convId == null || branchBusy.value) return
  branchBusy.value = true
  try {
    // from_seq = 该轮在会话消息流里的 1 基序号（turns 与 chat.msg 行一一对应、同序），
    // 后端语义是「复制前 N 条」，恰好含这一轮 AI 回答本身；越界由后端钳进 [0, total]。
    const r = await okJson<{ conv_id?: number }>(await fetch(`/api/chat/conv/${convId}/branch`, {
      method: 'POST', headers: authHeaders(),
      body: JSON.stringify({ from_seq: ti + 1, login_name: sessionToken.value ? null : loginName.value }),
    }), '分支会话失败', () => true, showToast)
    if (!r) return
    if (typeof r.conv_id !== 'number') { showToast('分支会话失败：服务端没返回新会话 id'); return }
    await loadConvs()
    await openConv(r.conv_id)
  } catch {
    showToast('分支会话失败（网络）')
  } finally {
    branchBusy.value = false
  }
}

// ── 【Trace 时间线】侧栏会话项的 🕓：拉 /api/chat/conv/{id}/trace 开右侧抽屉，Esc/遮罩关闭 ──
/** rounds 契约与 TracePanel.vue 文件头注释同形（那边是 props 的事实源，这里只复述形状）。 */
interface TraceEvent {
  kind: 'question' | 'route' | 'retry' | 'answer' | 'artifact'
  at?: string
  text?: string
  stage?: string
  result?: 'hit' | 'miss' | 'skip'
  ms?: number | null
  reason?: string
  error?: string
  route?: string
  sql?: string | null
  row_count?: number | null
  trust_level?: string | null
  issues?: string[]
  id?: number
  title?: string
  preview_url?: string
}
interface TraceRound {
  msg_id: number | null
  question: string
  at: string
  status: string
  route: string
  elapsed_ms: number | null
  events: TraceEvent[]
}
const traceConvId = ref<number | null>(null)
const traceRounds = ref<TraceRound[]>([])
const traceLoading = ref(false)
const traceError = ref('')
let traceFetchSeq = 0
async function openTrace(id: number, ev: Event) {
  ev.stopPropagation()
  const seq = ++traceFetchSeq
  traceConvId.value = id
  traceRounds.value = []
  traceError.value = ''
  traceLoading.value = true
  window.addEventListener('keydown', onTraceEsc)
  try {
    // 错误进抽屉内联展示（不弹 toast）：抽屉还开着，toast 会藏在遮罩后面。
    const r = await okJson<{ rounds?: TraceRound[] }>(
      await fetch(`/api/chat/conv/${id}/trace${loginQuery()}`, { headers: authHeaders(false) }),
      'Trace 加载失败', () => seq === traceFetchSeq, (m) => { traceError.value = m })
    if (!r || seq !== traceFetchSeq) return
    traceRounds.value = Array.isArray(r.rounds) ? r.rounds : []
  } catch (e) {
    if (seq === traceFetchSeq) traceError.value = `Trace 加载失败（网络）：${e}`
  } finally {
    if (seq === traceFetchSeq) traceLoading.value = false
  }
}
function closeTrace() {
  traceConvId.value = null
  window.removeEventListener('keydown', onTraceEsc)
}
function onTraceEsc(e: KeyboardEvent) {
  if (e.key === 'Escape') closeTrace()
}
const loginBusy = ref(false)
// 角色换签在飞闸：pickRole 里两个连续 await，双击会重复换签（按钮侧 :disabled 同步吃它）
const rolePicking = ref(false)
const loginError = ref('')
const loginPassword = ref('')
const loginRoles = ref<string[]>([])
const loginVisible = computed(() => (!embedded.value && !sessionToken.value) || loginRoles.value.length > 1)

function rememberSession(token: string, login: string) {
  // 预览 HTML 是按旧身份取回的授权快照。登录、续签或切换角色后必须丢弃。
  closePreview()
  adminConfirmed.value = false
  kbManager.value = false
  sessionToken.value = token
  loginName.value = login
  sessionValidated.value = true
  sessionStorage.setItem('dms-session', token)
  sessionStorage.setItem('dms-login', login)
  if (login !== 'admin' && (view.value === 'settings' || location.hash === '#/settings')) goChat()
}
function clearSession() {
  closePreview()
  sessionToken.value = ''; loginName.value = ''; roleCode.value = ''; loginRoles.value = []
  sessionValidated.value = false
  adminConfirmed.value = false
  kbManager.value = false
  digests.value = []; weeklyOpen.value = false
  sessionStorage.removeItem('dms-session'); sessionStorage.removeItem('dms-login')
  if (view.value === 'settings' || location.hash === '#/settings') goChat()
}
async function afterLogin() {
  await Promise.all([loadConvs(), loadSuggest(), confirmAdminAccess(), confirmKbManager()])
  await loadDigests()
}
/** 服务端 roles 数组只收字符串（脏数据会渲染成空按钮）—— passwordLogin / validateSession / offerRoles 同一条过滤。 */
function filterRoles(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((role): role is string => typeof role === 'string') : []
}
async function validateSession(): Promise<boolean> {
  if (!sessionToken.value) return false
  try {
    const r = await fetch('/api/roles', { headers: authHeaders(false) })
    if (!r.ok) { clearSession(); return false }
    const d = await r.json()
    const verifiedLogin = typeof d.login_name === 'string' ? d.login_name.trim() : ''
    if (!verifiedLogin) { clearSession(); return false }
    loginName.value = verifiedLogin
    sessionStorage.setItem('dms-login', verifiedLogin)
    sessionValidated.value = true
    roleCode.value = d.active || ''
    const roles = filterRoles(d.roles)
    loginRoles.value = roles.length > 1 && !roleCode.value ? roles : []
    if (!loginRoles.value.length) await afterLogin()
    return true
  } catch {
    // 网络级失败（抖动/后端重启）**不清**本地会话：token 可能仍然有效，清了就是逼用户重登。
    // 标未校验 + 提示；后续请求真遇 401 仍走 handleSessionExpired 正常清。
    sessionValidated.value = false
    showToast('后端暂时不可达，已保留本地会话；请稍后刷新重试')
    return false
  }
}

const expireSession = createSessionExpiryGuard(async () => {
  abortAllTurns('session')
  kbOpen.value = false
  if (embedded.value && dmsToken.value) {
    if (await reSsoWithRole(roleCode.value || null)) return
  }
  clearSession()
  // DMS 首页以 postMessage 传 token：请求父页重发即可无感换签；独立 UI 则显示账号密码登录。
  const origin = parentOrigin()
  if (embedded.value && origin) window.parent.postMessage({ type: 'dms-ai:ready', reason: 'expired' }, origin)
  else embedded.value = false
})
async function handleSessionExpired() {
  await expireSession(sessionToken.value || `login:${loginName.value}`)
}

async function openKnowledge() {
  // 管理入口只对有授权的人开放（服务端还有真闸，这里只是显隐）
  if (!kbManager.value) return
  kbOpen.value = true
}
async function passwordLogin() {
  loginError.value = ''
  if (!loginName.value.trim() || !loginPassword.value) { loginError.value = '请输入账号和密码'; return }
  loginBusy.value = true
  try {
    const r = await fetch('/api/login', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ login_name: loginName.value.trim(), password: loginPassword.value }),
    })
    const d = await r.json().catch(() => ({}))
    if (!r.ok) { loginError.value = d.error || '登录失败'; return }
    loginPassword.value = ''
    rememberSession(d.token, d.login_name)
    const roles = filterRoles(d.roles)
    loginRoles.value = roles.length > 1 ? roles : []
    roleCode.value = d.active || ''
    if (!loginRoles.value.length) await afterLogin()
  } catch { loginError.value = '登录服务连接失败' }
  finally { loginBusy.value = false }
}
function logout() {
  abortAllTurns('lifecycle')
  clearSession(); closePreview(); draftTurns.value = []; turnsByConv.clear(); queueByConv.clear(); pendingRefs.value = []; usageOpen.value = false; skillsOpen.value = false; datamapOpen.value = false; auditOpen.value = false; closeTrace(); convs.value = []; curConvId.value = null
  // 账号级状态一并清：换账号后旧账号的设置缓存/插话计数/质量数据不许滞留在内存里
  steerText.value = ''; steerCountByConv.clear(); llmCfg.value = null; settingsCat.value = null; quality.value = null; exemplars.value = []
}

function authHeaders(json = true): Record<string, string> {
  const h: Record<string, string> = {}
  if (json) h['Content-Type'] = 'application/json'
  if (sessionToken.value) h.Authorization = `Bearer ${sessionToken.value}`
  return h
}
function loginQuery(): string {
  return sessionToken.value ? '' : `?login_name=${encodeURIComponent(loginName.value)}`
}

/** 取 text 再试 JSON。**不直接 `.json()`**：axum 兜底 404 / 网关 502 是**空体或 HTML**，
 *  `.json()` 会当场抛 `Unexpected end of JSON input` —— 那句话对用户毫无意义。 */
async function readBody(resp: Response): Promise<[unknown, string]> {
  const raw = await resp.text()
  try { return [raw ? JSON.parse(raw) : null, raw] } catch { return [null, raw] }
}

/** 非 2xx 的统一用户提示；401 引导回到账号密码登录。 */
function errMsg(resp: Response, body: unknown, raw: string): string {
  const serverMsg = (body as { error?: string } | null)?.error?.trim()
  if (serverMsg) {
    return resp.status === 401 ? `未登录或登录已过期（${serverMsg}）。请重新登录。` : serverMsg
  }
  const trimmed = raw.trim()
  // 网关/反代的 HTML 错误页（502/504 等）原样展出 = 一屏 `<html><head>…` 垃圾（实测）——
  // 折叠成一句可行动的文案；空体同此。非 HTML 的短文本照旧透传（纯文本错误有其信息量）。
  const friendly = resp.status >= 500
    ? `服务暂时不可用（网关错误 ${resp.status}），请稍后重试`
    : `请求未成功（HTTP ${resp.status}）`
  const msg = !trimmed || /^<(!doctype|html)/i.test(trimmed) ? friendly : trimmed.slice(0, 200)
  return resp.status === 401 ? `未登录或登录已过期（${msg}）。请重新登录。` : msg
}

/** 【SSE 流式】读 `/api/ask/stream` 的知识库事件流（协议见 `kb-stream.ts` 头注）：
 *  meta  → 出半成品气泡（候选引用先挂上，用户先看到命中文档）；
 *  delta → 追加正文预览（KbAnswer 本来就渲半成品 markdown；未过口径后处理）；
 *  done  → 整体替换成过完口径后处理的最终 Answer（最终 citations/trace_id 在这里才挂）；
 *  error → 服务端已收口的友好文案，直接透出（重试同错，不回退）；
 *  HTTP 2xx 已代表服务端接受请求：无论首帧是否到达，之后断流都只报错，绝不自动重放。 */
async function consumeAskStream(resp: Response, aiTurn: Turn, signal: AbortSignal): Promise<void> {
  const reader = resp.body?.getReader()
  if (!reader) {
    aiTurn.error = '服务响应无法读取，请重试'
    aiTurn.streaming = false
    aiTurn.result = undefined
    return
  }
  const parser = new SseParser()
  const decoder = new TextDecoder()
  let draft = ''
  let streamIntentSummary: IntentSummary | undefined
  let streamResolvedQuestion: string | undefined
  /** 半成品预览不作数：最终答案只有过完口径后处理的那份（角标/冲突披露不许有第二形态） */
  const failWith = (msg: string) => {
    aiTurn.error = msg
    aiTurn.streaming = false
    aiTurn.result = undefined
  }
  try {
    for (;;) {
      const { done, value } = await reader.read()
      const events = done ? parser.end() : parser.feed(decoder.decode(value, { stream: true }))
      for (const ev of events) {
        const data = parseEventData(ev)
        if (!data) continue // 坏帧跳过，不炸整条流
        if (ev.event === 'meta') {
          streamIntentSummary = data.intent_summary && typeof data.intent_summary === 'object'
            ? data.intent_summary as IntentSummary
            : undefined
          streamResolvedQuestion = typeof data.resolved_question === 'string'
            ? data.resolved_question
            : undefined
          aiTurn.loading = false
          aiTurn.streaming = true
          aiTurn.result = {
            kind: 'text', route: 'knowledge', elapsed_ms: 0,
            sql: '', columns: [], rows: [], row_count: 0, truncated: false,
            markdown: '',
            citations: (Array.isArray(data.citations) ? data.citations : []) as Citation[],
            trace_id: typeof data.trace_id === 'string' ? data.trace_id : undefined,
            intent_summary: streamIntentSummary,
            resolved_question: streamResolvedQuestion,
          }
        } else if (ev.event === 'delta') {
          const piece = typeof data.text === 'string' ? data.text : ''
          if (piece && aiTurn.result) {
            draft += piece
            // 新对象触发响应式（KbAnswer 按 answerKey 监听内容变化）
            aiTurn.result = { ...aiTurn.result, markdown: draft }
            // 正文从气泡顶往下长，两屏之后全在视口下方 —— 不跟随的话「流式」这 10-20 秒
            // 用户看到的是静止画面，最主要的感知收益整个白丢。120ms 节流够跟上人眼。
            const now = Date.now()
            if (now - lastFollow >= 120) {
              lastFollow = now
              void nextTick(followStream)
            }
          }
        } else if (ev.event === 'done') {
          const answer = data.answer as AskResult | undefined
          if (!answer || typeof answer !== 'object') { failWith('服务响应异常，请稍后重试'); return }
          // 最终帧通常已带齐上下文；兼容只在 meta 帧提供结构化意图的服务端，避免替换时丢失。
          aiTurn.result = {
            ...answer,
            intent_summary: answer.intent_summary ?? streamIntentSummary,
            resolved_question: answer.resolved_question ?? streamResolvedQuestion,
          }
          aiTurn.streaming = false
          return
        } else if (ev.event === 'error') {
          const msg = typeof data.message === 'string' ? data.message.trim() : ''
          failWith(msg || '暂时无法完成知识检索，请稍后重试')
          return
        }
      }
      if (done) break
    }
    // 流结束却没有 done/error 终止帧 = 断流
    failWith('回答生成中断，请重试')
  } catch (error) {
    if (signal.aborted) throw error
    failWith('回答生成中断，请重试')
  }
}

/** 会话面四处 fetch 的**唯一**响应闸。返回 `null` = 已经报过错了，调用方直接 return。
 *
 *  🔴 修的是同一个根因、四处症状：这四处原来都是 `await (await fetch(...)).json()` ——
 *  非 2xx 的响应体也是合法 JSON（`{"error":…}`），`.json()` **正常返回**，
 *  于是没有一处看过 `resp.status`，字段静默变 `undefined`：
 *  - `newSession`：`curConvId.value = r.id` 得到 `undefined`，紧接着 `turns.value = []`
 *    把**屏幕上已有的问答清空** —— 看起来像「正常新建了个空会话」，零提示；
 *    而 `send()` 里 `if (curConvId.value == null)`（`undefined == null` 为真）
 *    让**每次提问都再清一次**。
 *  - `loadConvs`：`r.convs` 为 undefined → 侧栏显示「还没有会话」，与真的没会话无法区分。
 *  - `openConv`：`r.msgs` 为 undefined → 切过去一片空白。
 *  - `delConv`：连 `.json()` 都没有，删失败也照样从侧栏消失（下次 loadConvs 才闪回来）。
 *
 *  认证本轮改成**默认拒**，401 从此是前端最常见的失败，所以这条闸不是锦上添花。 */
async function okJson<T>(
  resp: Response,
  what: string,
  shouldReport: () => boolean = () => true,
  report: (message: string) => void = pushError,
): Promise<T | null> {
  const [body, raw] = await readBody(resp)
  if (resp.ok) return (body ?? {}) as T
  if (shouldReport()) report(`${what}：${errMsg(resp, body, raw)}`)
  return null
}

async function loadConvs() {
  try {
    const r = await okJson<{ convs?: Conv[] }>(
      await fetch(`/api/convs${loginQuery()}`, { headers: authHeaders(false) }),
      '会话列表加载失败', () => true, showToast)
    if (r) convs.value = r.convs || []
    // 网络级失败（fetch 自己抛）不弹气泡：侧栏底部的 `health` 已经写着「后端未连接」，
    // 而 loadConvs 是 onMounted 就跑的 —— 在这里弹会把首屏欢迎语顶掉，换来一条
    // 与健康灯重复的红字。okJson 那一路（服务端**答了**一个错误却被无视）才是要弹的。
  } catch { /* 保留上次成功的会话列表，健康灯已给出网络状态 */ }
}

let conversationNavigationId = 0

// 新建会话（后端建 conv，切过去，清空对话流）
async function newSession() {
  const navigationId = ++conversationNavigationId
  try {
    const r = await okJson<{ id?: number }>(await fetch('/api/conv/new', {
      method: 'POST', headers: authHeaders(),
      body: JSON.stringify({ login_name: sessionToken.value ? null : loginName.value }),
    }), '新建会话失败', () => navigationId === conversationNavigationId, showToast)
    if (!r) return
    // 🔴 清 `turns` 必须排在拿到**合法数字 id** 之后。`typeof` 而不是 `if (r.id)`：
    // 会话 id 是 bigint 自增，理论上不会是 0，但「用 truthy 判数字」正是本轮抓到的
    // 恒真形态之一（`if c.get(x)` 跳过合法的 0），不在这里再种一个。
    if (typeof r.id !== 'number') { showToast('新建会话失败：服务端没返回会话 id'); return }
    if (navigationId !== conversationNavigationId) return
    curConvId.value = r.id
    turnsByConv.set(r.id, [])
    question.value = ''
    await loadConvs()
    return true
  } catch {
    if (navigationId === conversationNavigationId) showToast('新建会话失败（网络）')
  }
}

// 打开会话：回放该会话所有消息
async function openConv(id: number) {
  if (id === curConvId.value) return
  const navigationId = ++conversationNavigationId
  // 已在本页打开过（尤其仍在运行）的会话直接切缓存；重新拉库会丢掉尚未落库的 loading turn。
  if (turnsByConv.has(id)) {
    if (navigationId !== conversationNavigationId) return
    curConvId.value = id
    scrollDown()
    return
  }
  try {
    const r = await okJson<{ msgs?: { role: string; question?: string; result?: AskResult }[] }>(
      await fetch(`/api/conv/${id}${loginQuery()}`, { headers: authHeaders(false) }), '打开会话失败',
      () => navigationId === conversationNavigationId, showToast)
    // 失败就**留在当前会话**（原来先 `curConvId = id; turns = []` 再 fetch：
    // 403「无权访问该会话」时用户丢掉当前会话内容、换来一片空白）
    if (!r) return
    const loaded: Turn[] = []
    let lastQuestion = ''
    for (const m of r.msgs || []) {
      if (m.role === 'user') {
        lastQuestion = m.question ?? ''
        loaded.push({ role: 'user', turnKey: `${id}:${loaded.length}:user`, convId: id, question: m.question })
      }
      else if ((m.result as { kind?: string } | undefined)?.kind === 'artifact_promote') {
        // 【D6】promote 事件不是一轮问答：回放成「钉进来的产物卡」，不进 result 渲染管线
        const p = m.result as { preview_url?: string; title?: string; note?: string; version?: number }
        loaded.push({
          role: 'ai', turnKey: `${id}:${loaded.length}:ai`, convId: id, question: lastQuestion, mode: 'lite',
          promoted: { url: p.preview_url || '', title: p.title || '产物', note: p.note, version: p.version },
        })
      }
      else {
        // 深度消息落库形状是 {result, artifact, page}；精简消息直接是 AskResult。
        const payload = m.result as ({ result?: AskResult; artifact?: { preview_url?: string; title?: string }; page?: DeepPage } & AskResult) | undefined
        if (payload?.result) loaded.push({
          role: 'ai', turnKey: `${id}:${loaded.length}:ai`, convId: id,
          question: lastQuestion, result: payload.result, page: payload.page,
          mode: payload.page || payload.artifact?.preview_url ? 'deep' : 'lite',
          artifact: payload.artifact?.preview_url ? { url: payload.artifact.preview_url, title: payload.artifact.title || '深度分析' } : undefined,
        })
        else loaded.push({
          role: 'ai', turnKey: `${id}:${loaded.length}:ai`, convId: id,
          question: lastQuestion, result: payload || undefined, mode: 'lite',
        })
      }
    }
    turnsByConv.set(id, loaded)
    if (navigationId !== conversationNavigationId) return
    curConvId.value = id
    scrollDown()
  } catch {
    if (navigationId === conversationNavigationId) showToast('打开会话失败（网络）')
  }
}

async function delConv(id: number, ev: Event) {
  ev.stopPropagation()
  try {
    // 删失败就别动侧栏与当前会话：原来无条件往下走，DELETE 401 时会话照样「消失」
    if (!await okJson(await fetch(`/api/conv/${id}${loginQuery()}`,
      { method: 'DELETE', headers: authHeaders(false) }), '删除会话失败', () => true, showToast)) return
    turnsByConv.delete(id)
    queueByConv.delete(id)
    if (id === curConvId.value) { curConvId.value = null; draftTurns.value = [] }
    await loadConvs()
  } catch { showToast('删除会话失败（网络）') }
}

/** 清空会话全部问答记录（保留会话本体）：追问上下文随之重置，下一问就是干净首问。 */
async function clearConv(id: number, ev: Event) {
  ev.stopPropagation()
  if (!window.confirm('清空该会话的全部问答记录？会话本身保留，追问上下文将重置。')) return
  try {
    if (!await okJson(await fetch(`/api/conv/${id}/clear${loginQuery()}`,
      { method: 'POST', headers: authHeaders(false) }), '清空会话失败', () => true, showToast)) return
    // 消息缓存作废：当前打开的立即清屏，其它会话下次打开重新拉（空）
    turnsByConv.delete(id)
    if (id === curConvId.value) draftTurns.value = []
    showToast('已清空该会话记录')
  } catch { showToast('清空会话失败（网络）') }
}

function applyTheme() {
  document.documentElement.setAttribute('data-theme', theme.value)
}
function toggleTheme() {
  theme.value = theme.value === 'dark' ? 'light' : 'dark'
  localStorage.setItem('theme', theme.value)
  applyTheme()
}

onMounted(async () => {
  applyTheme()
  // 端#3 企微：/#token=xxx
  const tm = location.hash.match(/token=([^&]+)/)
  if (tm) {
    rememberSession(decodeURIComponent(tm[1]), '企微用户'); embedded.value = true
    history.replaceState(null, '', location.pathname)
  }
  checkHealth()
  // DMS 首页嵌入只信任父页本次传入的 DMS token。若先校验 sessionStorage 里的旧 AI token，
  // 它的迟到 401 会把刚换好的新会话清掉，表现为首页多等一轮甚至偶发认证失败。
  if (dmsHomeEmbedded) {
    clearSession()
    return
  }
  await validateSession()
  // 直开/刷新设置页由 confirmAdminAccess 在服务端确认后打开；未登录时保留路径供登录后继续。
})
function parentOrigin(): string | null {
  if (window.parent === window) return null
  try { return new URL(document.referrer).origin } catch { return null }
}

/** DMS 首页嵌入：父页用 postMessage 传 token，避免把凭据放进 iframe URL/历史/日志。 */
async function receiveParentSso(event: MessageEvent) {
  const origin = parentOrigin()
  if (!origin || event.source !== window.parent || event.origin !== origin || event.data?.type !== 'dms-ai:sso') return
  const token = typeof event.data.dmsToken === 'string' ? event.data.dmsToken : ''
  const requestedRole = typeof event.data.roleCode === 'string' ? event.data.roleCode.trim() : ''
  if (!token) return
  embedded.value = true
  dmsToken.value = token
  const ok = await reSsoWithRole(requestedRole || null)
  window.parent.postMessage({
    type: ok ? 'dms-ai:sso-ok' : 'dms-ai:sso-error',
    message: ok ? '' : 'DMS 身份认证失败，请刷新首页或重新登录。',
  }, origin)
}

onMounted(() => {
  const origin = parentOrigin()
  if (!origin || new URLSearchParams(location.search).get('embed') !== 'dms-home') return
  window.addEventListener('message', receiveParentSso)
  window.parent.postMessage({ type: 'dms-ai:ready', reason: 'boot' }, origin)
})

onBeforeUnmount(() => {
  abortAllTurns('lifecycle')
  closePreview()
  window.removeEventListener('hashchange', handleHashChange)
  window.removeEventListener('message', receiveParentSso)
})

// 侧栏只展示今天生成的经营日报；普通全局 artifact 不能混进日报入口。
const digests = ref<{ id: number; title: string; preview_url: string }[]>([])
async function loadDigests() {
  digests.value = []
  if (!hasAdminAccess.value) return
  try {
    const r = await fetch(`/api/artifact/list?feed=daily&limit=1${loginQuery().replace('?', '&')}`, { headers: authHeaders(false) })
    if (!r.ok) return
    const d = await r.json()
    digests.value = Array.isArray(d.artifacts) ? d.artifacts.slice(0, 1) : []
  } catch { /* 日报是增强件，拉不到就不显示（侧栏主功能不受影响） */ }
}

let digestTimer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  digestTimer = setInterval(() => { if (hasAdminAccess.value) void loadDigests() }, 10 * 60_000)
})
onBeforeUnmount(() => { if (digestTimer) clearInterval(digestTimer) })

interface SendOptions {
  forceDeep?: boolean
  /** 重试使用原轮次模式，不受用户之后切换“精简/深度”影响。 */
  forceMode?: 'deep' | 'lite'
  /** 同一轮重试固定原分诊与知识空间，避免界面切换改变问题语义。 */
  forceIntent?: 'auto' | 'data' | 'knowledge'
  spaceId?: string | null
  displayQuestion?: string
  /** 基于历史轮次触发的重试/澄清必须固定原会话，不能在异步等待后借用当前会话。 */
  targetConvId?: number
  /** 【排队追问】出队时原样带回入队那一刻的引用快照（chip 区在入队时已清空）。 */
  refs?: string[]
}
interface WeekRange { start: string; end: string }
const weeklyOpen = ref(false)
const weeklyBusy = ref(false)
const weeklyError = ref('')
const weeklyProvince = ref(localStorage.getItem('dms-weekly-province') ?? '')
const weeklyProvinceInput = ref<HTMLInputElement | null>(null)
const weeklyRange = ref<WeekRange>(currentWeekRange())

function localDate(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

function currentWeekRange(now = new Date()): WeekRange {
  const mondayOffset = (now.getDay() || 7) - 1
  const monday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - mondayOffset)
  const sunday = new Date(monday.getFullYear(), monday.getMonth(), monday.getDate() + 6)
  return { start: localDate(monday), end: localDate(sunday) }
}

function openWeeklyReport() {
  if (weeklyBusy.value) return
  weeklyRange.value = currentWeekRange()
  weeklyError.value = ''
  weeklyOpen.value = true
  nextTick(() => weeklyProvinceInput.value?.focus())
}

function closeWeeklyReport() {
  if (!weeklyBusy.value) weeklyOpen.value = false
}

function weeklyReportPrompt(province: string, range: WeekRange): string {
  return `请生成【单省区周度经营分析报告】。

分析范围：
省区：${province}
周期：${range.start} 至 ${range.end}
对比周期：上周、去年同期

请基于以下数据进行分析：
1. 线下销售数据：销售额、销量、订单数、客单价、同比、环比
2. 单品销量：TOP 单品、销量变化、贡献占比、异常波动
3. 单店效率：门店数、店均销售额、店均销量、坪效/人效（如有）
4. 营销费用：费用总额、费用率、投放渠道、费用产出比
5. 库存与缺货：重点单品库存、缺货影响、滞销风险（如有）

输出要求：
- 口径必须固定：销售额只取 sales_dw.dws_off_offline_sale_dfn 的 SUM(amount)，销量只取 SUM(qty)
- 订单额是独立订单口径，只有用户明确要求“订单额”时才可展示；不得用订单总额、订单明细金额或旧发货链路替代销售额
- 订单数与客单价只能使用已验证的独立订单数据；不得按销售宽表事实行数推算订单数
- 先给经营结论，不超过 5 条
- 再按模块分析：销售表现、单品表现、门店效率、营销费用、库存与缺货、问题与机会
- 每个模块突出关键变化、原因判断和改进建议
- 标出异常数据和需要跟进事项
- 最后输出下周行动建议，按优先级排序
- 语言简洁，偏经营管理口径，不写空话
- 不得将客户编码/客户名称解释为门店；门店数、店均指标、坪效和人效只有取得已验证门店事实时才能展示，否则明确标注数据缺口
- 营销费用、费用率、费用产出比、库存、缺货和滞销风险必须来自对应模块的实际查询结果；查询未返回有效数据时只展示缺口，不得用销售额、客户或商品结构替代
- 必须对比上周和去年同期；数据不足时明确标注数据缺口，不得猜测、推算或编造`
}

async function generateWeeklyReport() {
  const province = weeklyProvince.value.trim()
  if (!province) {
    weeklyError.value = '请输入要分析的省区名称'
    await nextTick()
    weeklyProvinceInput.value?.focus()
    return
  }
  if (turns.value.some(turnRunning) || curQueue.value.length) {
    // 队列里还有排队问题：周报带参调用不排队，直接跑会插队 —— 提示用户等队列排空
    weeklyError.value = '当前会话仍有分析中或排队的问题，完成后再生成周报'
    return
  }
  weeklyBusy.value = true
  weeklyError.value = ''
  localStorage.setItem('dms-weekly-province', province)
  const range = weeklyRange.value
  const displayQuestion = `${province}经营周报（${range.start} 至 ${range.end}）`
  weeklyOpen.value = false
  try {
    await send(weeklyReportPrompt(province, range), { forceDeep: true, displayQuestion })
  } finally {
    weeklyBusy.value = false
  }
}

/** 用 `dmsToken` 换（或**重换**）会话 token。角色是 `auth::issue` 时**签进 token** 的，
 *  而 `resolve_identity`（main.rs:821-825）里 Bearer 优先于 body —— 也就是说会话 token 在手时，
 *  `/api/ask` body 里的 `role_code` **到不了** `load_principal`。
 *  所以「选完角色重试」对 SSO 用户只有这一条路：带新角色重换一次 token。 */
async function reSsoWithRole(role: string | null): Promise<boolean> {
  try {
    const resp = await fetch('/api/sso', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ dms_token: dmsToken.value, role_code: role }),
    })
    const d = await resp.json()
    if (!resp.ok) { pushError(`SSO 认证失败：${d.error || ''}`); return false }
    rememberSession(d.token, d.login_name)
    roleCode.value = d.active || ''
    loginRoles.value = d.roles?.length > 1 && !d.active ? d.roles : []
    // 多角色账号先完成角色选择；否则会在无角色会话下白拉会话/推荐/日报，既慢又会让页面半加载。
    if (!loginRoles.value.length) void afterLogin()
    return true
  } catch { pushError('SSO 认证失败（网络）'); return false }
}

/** 后端 fail-closed 拒绝多角色账号时那句话的标记（`policy/src/principal.rs:67-70`）。
 *  **靠文案匹配**是因为那条 403 只有 `{error: string}`、没有错误码；匹配不上时不会有任何副作用
 *  （照旧显示后端原文），所以文案改了最坏就是退回今天的行为，不会误弹选择器。 */
const ROLE_AMBIGUOUS = '请选择登录角色'

/** 拉可选角色，够条件才挂到这一轮上。三端都通过 `/api/session/role` 换签生效。 */
async function offerRoles(turn: Turn) {
  try {
    const r = await (await fetch(`/api/roles${loginQuery()}`, { headers: authHeaders(false) })).json()
    const roles = filterRoles(r.roles)
    // 单角色/空清单不给选择器：单角色账号本来就不会被这条拒绝挡住，弹出来纯属噪音
    if (roles.length > 1) turn.roles = roles
  } catch { /* 拉不到清单 → 什么都不做，后端那句「可选角色 A / B」还在气泡里 */ }
}

/** 选定角色 → 让它生效 → 原题重问一次。 */
async function pickRole(role: string, question?: string, options: SendOptions = {}, targetConvId?: number) {
  if (rolePicking.value) return
  rolePicking.value = true
  roleCode.value = role
  if (sessionToken.value) {
    try {
      const resp = await fetch('/api/session/role', {
        method: 'POST', headers: authHeaders(), body: JSON.stringify({ role_code: role }),
      })
      const d = await resp.json()
      if (!resp.ok) { pushError(`切换角色失败：${d.error || ''}`, targetConvId); rolePicking.value = false; return }
      rememberSession(d.token, d.login_name)
      loginRoles.value = []
      await afterLogin()
    } catch { pushError('切换角色失败（网络）', targetConvId); rolePicking.value = false; return }
  }
  if (question) await send(question, { ...options, targetConvId })
  rolePicking.value = false
}

async function checkHealth() {
  try {
    const h = await (await fetch('/api/health')).json()
    healthOk.value = h.ok
    healthBusy.value = h.mysql?.status === 'busy'
    health.value = h.ok ? '服务正常 · 业务库只读' : healthBusy.value ? '业务源繁忙 · 稍后重试' : '服务异常'
  } catch { healthOk.value = false; healthBusy.value = false; health.value = '后端未连接' }
}

function pushError(msg: string, targetConvId?: number) {
  const convId = targetConvId ?? curConvId.value ?? undefined
  const target = convId == null ? draftTurns.value : turnsByConv.get(convId)
  if (!target) { showToast(msg); return }
  target.push({ role: 'ai', turnKey: uuid(), convId, error: msg })
  if (convId == null ? curConvId.value == null : curConvId.value === convId) scrollDown()
}

async function scrollDown() {
  await nextTick()
  chatEl.value?.scrollTo({ top: chatEl.value.scrollHeight, behavior: 'smooth' })
}

// 流式跟随。**不复用 scrollDown**：`behavior:'smooth'` 的动画会和每帧新内容打架，
// 越滚越跟不上；这里直接赋 scrollTop。
// 120px 阈值 = 用户手动上翻后就不再被拽回底部（正在读上文时被拉走比不跟随更糟）。
let lastFollow = 0
function followStream() {
  const el = chatEl.value
  if (!el) return
  if (el.scrollHeight - el.scrollTop - el.clientHeight > 120) return
  el.scrollTop = el.scrollHeight
}

async function send(q?: string, options: SendOptions = {}) {
  const text = (q ?? question.value).trim()
  if (!text) return
  const displayText = options.displayQuestion?.trim() || text
  // 只有「输入框 / 快捷 pill」这种不带参数的新问句才消费引用 chip 区、才允许排队；
  // 重试/下钻/周报等带参调用语义已固定，不排队也不带走引用（维持原来的直接早退）。
  const isPlainAsk = Object.keys(options).length === 0
  const targetTurns = options.targetConvId == null ? turns.value : turnsByConv.get(options.targetConvId)
  if (targetTurns?.some(turnRunning)) {
    // 【排队追问】本会话有进行中的问答：新问句带着引用快照进队列，run 结束由 finally 续发。
    if (isPlainAsk && curConvId.value != null) {
      enqueueAsk(text, pendingRefs.value.splice(0))
      if (q === undefined) question.value = ''
    }
    return
  }
  // 无当前会话则先建一个（同一会话内多轮归一，不再一问一会话）。
  // 建不出来就停在这里：`newSession` 已经报过一句人话，再往下走只会在它下面
  // 叠一条同源的 401 —— 而且顺序还是错的（错误气泡排在用户问句**之前**）。
  if (options.targetConvId == null && curConvId.value == null && !(await newSession())) return
  const convId = options.targetConvId ?? curConvId.value!
  steerCountByConv.delete(convId) // 新一轮 run：插话受理计数清零（服务端信箱同步清空）
  if (!turnsByConv.has(convId) && options.targetConvId != null) {
    showToast('原会话已不可用，请在当前会话重新提问')
    return
  }
  // 知识库走引用式 RAG，不能被深度问数接口劫持；重试固定首次发送时的分诊与空间。
  const requestedIntent = options.forceDeep ? 'data' : (options.forceIntent ?? intent.value)
  const isKnowledge = requestedIntent === 'knowledge'
  const usesKnowledgeSpace = requestedIntent === 'auto' || isKnowledge
  const selectedSpaceId = options.spaceId !== undefined ? options.spaceId : (knowledgeSpaceId.value || null)
  const requestedDeep = options.forceMode ? options.forceMode === 'deep' : (options.forceDeep || deepMode.value)
  const isDeep = requestedDeep && !isKnowledge
  // 必须从 reactive Map 重新 get：首次创建时直接继续写入原始 []，Vue 不会追踪后续 push/字段更新。
  if (!turnsByConv.has(convId)) turnsByConv.set(convId, [])
  const convTurns = turnsByConv.get(convId)!
  convTurns.push({ role: 'user', turnKey: uuid(), convId, question: displayText })
  convTurns.push({
    role: 'ai', turnKey: uuid(), convId, question: text,
    loading: true, elapsed: 0, mode: isDeep ? 'deep' : 'lite',
    retryQuestion: text,
    retryOptions: {
      ...options,
      forceMode: isDeep ? 'deep' : 'lite',
      forceIntent: requestedIntent,
      spaceId: usesKnowledgeSpace ? selectedSpaceId : null,
      displayQuestion: displayText,
    },
  })
  const aiTurn = convTurns[convTurns.length - 1]
  if (curConvId.value === convId) {
    question.value = ''
    scrollDown()
  }
  // 【思维过程】深度模式：带 rid 轮询服务端阶段清单（Codex 式：等的时候知道在做什么）
  const rid = isDeep ? uuid() : ''
  const progressStop = rid ? startProgress(rid, aiTurn) : () => {}
  // 【D4】rid 留在轮上：出错后凭它查服务端账本可否断点续跑
  aiTurn.rid = rid || undefined
  // 已耗时实时跳动（大查询 10~60s 有预期，不再是「假死」）
  const t0 = Date.now()
  const tick = setInterval(() => { aiTurn.elapsed = Math.floor((Date.now() - t0) / 1000) }, 1000)
  const ctrl = new AbortController()
  activeAskControllers.set(aiTurn.turnKey!, { controller: ctrl, turn: aiTurn })
  // 深度模式多两份等待：SC×3 生成 + 之后的深度解读，超时相应放宽
  const timer = setTimeout(() => abortTurn(aiTurn, 'timeout'), isDeep ? 180000 : 100000)
  try {
    // 【深度模式】单入口：/api/deep/compose 一次返回 {result, artifact}（总值+拆解+趋势+明细+图+AI 全在服务端同管线出）
    // 【引用上轮】chip 区快照随 body 发出；带参调用（含队列出队）用 options 里那份。
    const sendRefs = options.refs ?? (isPlainAsk ? pendingRefs.value.splice(0) : [])
    // 重试快照补上引用：chip 区在首次发送时已清空，不回填的话失败后点「重试」引用静默丢失
    if (aiTurn.retryOptions) aiTurn.retryOptions.refs = sendRefs
    const bodyFields = {
      question: text,
      ...(displayText !== text ? { display_question: displayText } : {}),
      login_name: sessionToken.value ? null : loginName.value,
      role_code: roleCode.value || null,
      conv_id: convId,
      // 强制意图：auto 传 null 交给后端分诊（K5 之前后端忽略该字段，多传无害）
      intent: requestedIntent === 'auto' ? null : requestedIntent,
      space_id: usesKnowledgeSpace ? selectedSpaceId : null,
      // 深度模式：服务端 SC 抬到 ≥3（生成侧深度参与）；缺省精简，body 与老前端同形
      mode: isDeep ? 'deep' : null,
      // 思维过程轮询 id（缺省 null = 不登记，后端零变化）
      rid: rid || null,
      // 【引用上轮】引用快照（后端把每条当作上轮上下文素材）；空 = null，与老前端同形
      refs: sendRefs.length ? sendRefs : null,
    }
    const post = (url: string) => fetch(url, {
      method: 'POST', headers: authHeaders(), signal: ctrl.signal,
      body: JSON.stringify(bodyFields),
    })
    const askSessionKey = sessionToken.value || `login:${loginName.value}`
    // 🔴 同步响应走同一套 `readBody`/`errMsg`，与会话面四处一致：
    // 原来是 `await resp.json()` + `data.error || '请求失败'` —— 两个洞。
    // ① 401 只显示服务端那三个字「未认证」，用户不知道该做什么，而认证本轮改成默认拒、
    //    这里是全站最常走的端点（每次提问都过），比会话面四处更常见。
    // ② 网关 502 / 兜底 404 是空体或 HTML → `.json()` 抛 `Unexpected end of JSON input`，
    //    被下面的 catch 抓成 `String(e)`，气泡里是这句 SyntaxError（同 `toggleAnalysis` 的坑）。
    // 本函数同时是：深度模式的唯一路径、流式端点分诊落 data 的普通 JSON 路径、流式失败后的兜底重试。
    const handleSync = async (resp: Response) => {
      const [body, raw] = await readBody(resp)
      if (!resp.ok) {
        aiTurn.error = errMsg(resp, body, raw)
        if (resp.status === 401) await expireSession(askSessionKey)
        // 多角色账号被 fail-closed 拒（那是**正确**的安全行为，不许放宽）——
        // 角色选择由 `/api/roles` 提供，选择后服务端重新校验角色归属并换签。
        if (aiTurn.error.includes(ROLE_AMBIGUOUS)) await offerRoles(aiTurn)
      } else if (!body) {
        // 200 但空体/非 JSON：渲染一个空结果气泡就是又一次静默吞错（本轮要杀的形态本身）
        aiTurn.error = '服务端返回了空响应（HTTP 200 但没有 JSON 结果）'
      } else if (isDeep) {
        // 【深度模式】compose 端点回 {result, artifact, page}；精简端点回 AskResult 本体
        const d = body as { result: AskResult; artifact?: { preview_url: string; title: string }; page?: DeepPage }
        aiTurn.result = d.result
        aiTurn.page = d.page
        if (d.artifact?.preview_url) {
          aiTurn.artifact = { url: d.artifact.preview_url, title: d.artifact.title }
        }
      } else aiTurn.result = body as AskResult
    }
    if (isDeep) {
      await handleSync(await post('/api/deep/compose'))
    } else {
      await runAskTransport(post, (resp) => consumeAskStream(resp, aiTurn, ctrl.signal), handleSync, ctrl.signal)
    }
  } catch (e) {
    aiTurn.error = aiTurn.abortReason === 'user'
      ? '已停止生成'
      : aiTurn.abortReason === 'session'
        ? '登录已失效，请重新登录'
        : ctrl.signal.aborted
          ? '查询超时，请重试或换个问法'
          : '查询失败（网络），请重试'
  } finally {
    clearTimeout(timer)
    clearInterval(tick)
    progressStop()
    if (aiTurn.turnKey && activeAskControllers.get(aiTurn.turnKey)?.controller === ctrl) {
      activeAskControllers.delete(aiTurn.turnKey)
    }
    aiTurn.loading = false
    aiTurn.streaming = false
    // 【D4】深度轮出错：查服务端账本是否可断点续跑（可 → 错误气泡出「续跑」入口）
    if (isDeep && aiTurn.error && rid) checkResumable(rid, aiTurn)
    if (curConvId.value === convId) scrollDown()
    // 刷新侧栏标题/时间。本轮已经报过错就别刷：认证挂掉时 `/api/ask` 与 `loadConvs`
    // 会各弹一条 401 气泡，一次提问两条同源错误；且此时侧栏本来也没有新标题可刷。
    if (!aiTurn.error) loadConvs()
    // 主动停止、超时、注销与 401 都不再提交下一条，避免用户已终止后后台继续消费队列。
    if (!aiTurn.abortReason) drainQueue(convId)
  }
}

// 【思维过程】轮询：1.2s 一拍刷阶段清单到 loading 气泡；done/结束/出错即停。
function startProgress(rid: string, aiTurn: Turn): () => void {
  let inFlight = false
  const timer = setInterval(async () => {
    if (inFlight) return // 上一拍还没回来就跳过本拍：一次请求超过 1.2s 不许并发叠
    inFlight = true
    try {
      const r = await fetch(`/api/deep/progress?rid=${encodeURIComponent(rid)}`, { headers: authHeaders(false) })
      if (!r.ok) return
      const j = await r.json()
      aiTurn.progress = j.steps ?? []
      if (Array.isArray(j.sections)) aiTurn.tasks = j.sections
      if (j.done) clearInterval(timer)
    } catch { /* 轮询失败静默（结果才是主路） */ } finally { inFlight = false }
  }, 1200)
  return () => clearInterval(timer)
}

// 【D4】出错后查一次运行态：服务端账本（meta.deep_run）判定可续跑才亮「续跑」入口。
// 可续跑 = failed/interrupted/重启孤儿（running 但执行器已死）；活执行器/已完成 = 不亮。
async function checkResumable(rid: string, t: Turn) {
  try {
    const r = await fetch(`/api/deep/progress?rid=${encodeURIComponent(rid)}`, { headers: authHeaders(false) })
    if (!r.ok) return
    const j = await r.json()
    t.resumable = j.resumable === true
  } catch { /* 静默：续跑入口缺席不挡主流程 */ }
}

// 【D4】断点续跑：POST /api/deep/resume 与 compose 同形返回（已完成板块服务端零重跑）。
// 复用同一 rid 轮询进度（板块面板原地复活）；409 = 已在执行/状态已变，文案直接透出。
async function resumeDeep(t: Turn) {
  if (!t.rid || t.resuming) return
  const rid = t.rid
  t.resuming = true
  t.resumable = false
  t.error = undefined
  t.loading = true
  t.elapsed = 0
  const progressStop = startProgress(rid, t)
  const t0 = Date.now()
  const tick = setInterval(() => { t.elapsed = Math.floor((Date.now() - t0) / 1000) }, 1000)
  const ctrl = new AbortController()
  const timer = setTimeout(() => ctrl.abort(), 180000)
  try {
    const resp = await fetch('/api/deep/resume', {
      method: 'POST', headers: authHeaders(), signal: ctrl.signal,
      body: JSON.stringify({
        rid,
        login_name: sessionToken.value ? null : loginName.value,
        role_code: roleCode.value || null,
        conv_id: t.convId ?? null,
      }),
    })
    const [body, raw] = await readBody(resp)
    if (!resp.ok) {
      t.error = errMsg(resp, body, raw)
      checkResumable(rid, t) // 续跑失败（含 409 并发闸/状态已变）：重新评估入口，别让气泡只剩语义不同的「重试」
    } else if (!body) {
      t.error = '服务端返回了空响应（HTTP 200 但没有 JSON 结果）'
    } else {
      const d = body as { result: AskResult; artifact?: { preview_url: string; title: string }; page?: DeepPage }
      t.result = d.result
      t.page = d.page
      if (d.artifact?.preview_url) {
        t.artifact = { url: d.artifact.preview_url, title: d.artifact.title }
      }
    }
  } catch (e) {
    t.error = ctrl.signal.aborted ? '续跑超时，请重试' : '续跑失败（网络），请重试'
    checkResumable(rid, t)
  } finally {
    clearTimeout(timer)
    clearInterval(tick)
    progressStop()
    t.loading = false
    t.resuming = false
    if (!t.error) loadConvs()
  }
}

// 【深度页内嵌】列语义（BiChart 要 semantic 才能格式化金额轴）：直接用 semanticForLabel，不再过一层零价值转发
function metricSemantic(name: string): Semantic {
  const semantic = semanticForLabel(name)
  return semantic === 'none' ? 'count' : semantic
}
function formatMetricValue(label: string, value: unknown): string {
  const number = toNum(value)
  if (isGrossMarginLabel(label) && number !== null) return fmt(number * 100, 'percent')
  return fmt(value, metricSemantic(label))
}
function secCols(sec: DeepSection) {
  return sec.columns.map((name) => ({
    name,
    semantic: semanticForLabel(name),
  }))
}
function secChartKind(sec: DeepSection): 'bar' | 'line' | 'pie' {
  return sec.kind === 'table' ? 'bar' : sec.kind
}
// 三列（时间/维度/值）的折线必须切多序列 —— 与 present.rs 的 series 规则同一条
function secSeries(sec: DeepSection): number | null {
  return sec.kind === 'line' && sec.columns.length === 3 ? 1 : null
}
function secY(sec: DeepSection): number[] { return [Math.max(0, sec.columns.length - 1)] } // 单列板块钳到下标 0：原来的 max(1,…) 会把越界的 1 传给 BiChart
function isMetricPeriodCell(sec: DeepSection, ci: number): boolean {
  return sec.columns[0]?.trim() === '指标' && /^(?:本周|上周|去年同期)$/.test(sec.columns[ci]?.trim() ?? '')
}
function secCellSemantic(sec: DeepSection, row: unknown[], ci: number): Semantic {
  return isMetricPeriodCell(sec, ci) ? metricSemantic(String(row[0] ?? '')) : semanticForLabel(sec.columns[ci] ?? '')
}
function secCell(sec: DeepSection, row: unknown[], ci: number): string {
  const value = row[ci]
  return isMetricPeriodCell(sec, ci)
    ? formatMetricValue(String(row[0] ?? ''), value)
    : formatCell(sec.columns, value, ci)
}
function formatCell(columns: string[], value: unknown, ci: number): string {
  const label = columns[ci] ?? ''
  return isGrossMarginLabel(label) ? formatMetricValue(label, value) : fmt(value, semanticForLabel(label))
}
function formatLabeledValue(label: string, value: unknown): string {
  return formatMetricValue(label, value) || '—'
}
function secView(sec: DeepSection): 'chart' | 'table' {
  if (sec.kind === 'table') return 'table'
  return sec.view ?? (sec.columns.length > 3 ? 'table' : 'chart')
}
const DEEP_TABLE_PREVIEW_ROWS = 24
function deepComparisons(page: DeepPage): DeepComparison[] {
  return page.comparisons?.length ? page.comparisons : (page.comparison ? [page.comparison] : [])
}
// 【D8】验收自评档 → 文案（与 dms_agent analysis::Acceptance 的 code 对齐）
function verdictLabel(verdict?: 'met' | 'partial' | 'unmet' | null): string {
  return ({ met: '满足', partial: '部分', unmet: '未满足' } as Record<string, string>)[verdict ?? ''] ?? '待评'
}
function comparisonPct(value: unknown): number {
  const parsed = typeof value === 'number' ? value : Number(value)
  return Number.isFinite(parsed) ? parsed : 0
}
function comparisonRate(cmp: DeepComparison): string {
  if (cmp.pct == null) {
    if (cmp.baseline === 0 && (cmp.current ?? 0) > 0) return '新增'
    if (cmp.baseline === 0 && (cmp.current ?? 0) < 0) return '转负'
    return '不适用'
  }
  const pct = comparisonPct(cmp.pct)
  return `${pct > 0 ? '+' : ''}${pct.toFixed(1)}%`
}
function comparisonNumber(value: number | undefined, label: string): string {
  return typeof value === 'number' && Number.isFinite(value) ? formatMetricValue(label, value) : '-'
}
function signedComparison(value: number | undefined, label: string): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) return '-'
  if (isGrossMarginLabel(label)) {
    const points = (Math.abs(value) * 100).toFixed(1)
    return `${value > 0 ? '+' : value < 0 ? '-' : ''}${points} 个百分点`
  }
  const rendered = formatMetricValue(label, Math.abs(value))
  return `${value > 0 ? '+' : value < 0 ? '-' : ''}${rendered}`
}
function turnSqls(t: Turn): { title: string; sql: string }[] {
  if (t.page?.sqls?.length) return t.page.sqls
  const sqls: { title: string; sql: string }[] = []
  if (t.result?.sql) sqls.push({ title: '主查询', sql: t.result.sql })
  for (const sub of t.result?.subs ?? []) {
    if (sub.result.sql && !sqls.some((item) => item.sql === sub.result.sql)) {
      sqls.push({ title: sub.question || '子查询', sql: sub.result.sql })
    }
  }
  return sqls
}
// 模板对同一 result 一轮渲染会调多遍（folders 判空/列表/溢出计数共 3 处）；
// result 对象每轮只赋值一次，按引用缓存结果，任意响应式变动引发的重渲染只花一次计算。
const knowledgeSourcesCache = new WeakMap<AskResult, { documents: number; folders: string[] }>()
function knowledgeSources(result: AskResult): { documents: number; folders: string[] } {
  const hit = knowledgeSourcesCache.get(result)
  if (hit) return hit
  const citations = result.citations ?? []
  const documents = new Set(citations.map((citation) => citation.doc_id).filter(Boolean)).size
  const folders = [...new Set(citations
    .map((citation) => citation.folder_path || citation.directory_path || '')
    .map((path) => path.trim().replace(/^\/+|\/+$/g, ''))
    .filter(Boolean))]
  const value = { documents, folders }
  knowledgeSourcesCache.set(result, value)
  return value
}
function userFacingMarkdown(text: string): string {
  const visible: string[] = []
  let hidingInternalSection = false
  for (const rawLine of text.replace(/\r/g, '').split('\n')) {
    const plainLine = rawLine.trim().replace(/^[-*>\d.、\s]+/, '').replace(/\*\*|__/g, '')
    const heading = /^(#{1,6})\s*(.*)$/.exec(rawLine.trim())
    const strongHeading = /^(?:\*\*([^*]+)\*\*|__([^_]+)__)$/.exec(rawLine.trim())
    const headingText = heading?.[2] ?? strongHeading?.[1] ?? strongHeading?.[2]
    if (headingText !== undefined) {
      hidingInternalSection = /^(?:证据|证据与边界|数据边界|可信度|技术诊断|口径与可信度|内部校验)/.test(headingText.trim())
      if (hidingInternalSection) continue
    } else if (hidingInternalSection) {
      continue
    }
    if (/^(?:证据(?:编号|与边界)?|数据边界|可信度|技术诊断|口径与可信度|内部校验)\s*[:：|]/.test(plainLine)) continue
    if (rawLine.includes('|') && /\b(?:KPI|SEC|CON)-\d+\b/i.test(rawLine)) continue
    const line = rawLine
      .replace(/\[\^(?:\d+)\]/g, '')
      .replace(/\[(?:KPI|SEC|CON)-\d+\]/gi, '')
      .replace(/\b(?:KPI|SEC|CON)-\d+\b/gi, '')
      .replace(/(?:证据编号|KPI引用|SEC引用)\s*[:：]?\s*/gi, '')
      .replace(/\s+([，。；：])/g, '$1')
    if (!/^\s*\|?\s*(?:证据|证据编号|可信度|技术诊断)\s*\|/i.test(line)) visible.push(line)
  }
  return visible.join('\n').replace(/\n{3,}/g, '\n\n').trim()
}
function knowledgePresentation(result: AskResult): {
  markdown?: string; citations?: Citation[]; intent_summary?: IntentSummary; resolved_question?: string
} {
  return {
    markdown: userFacingMarkdown(result.markdown ?? ''), citations: result.citations,
    intent_summary: result.intent_summary, resolved_question: result.resolved_question,
  }
}
function hybridKnowledgePresentation(result: AskResult): ReturnType<typeof knowledgePresentation> {
  const kb = result.kb!
  return {
    ...knowledgePresentation(kb),
    intent_summary: kb.intent_summary ?? projectKnowledgeReceipt(result.intent_summary, !!kb.citations?.length),
    resolved_question: kb.resolved_question ?? result.resolved_question,
  }
}
function dataOnlyResult(result: AskResult): AskResult {
  if (!result.view?.insight) return result
  return { ...result, view: { ...result.view, insight: undefined } }
}
/** 【意图澄清】clarify_options 只过干净项：空 label/question 的脏数据不进 UI。
 *  一轮渲染调两遍（v-if + v-for），按 result 引用缓存（同 knowledgeSources）。 */
const clarifyOptionsCache = new WeakMap<AskResult, { label: string; question: string }[]>()
function clarifyOptionsOf(result?: AskResult): { label: string; question: string }[] {
  if (!result || !Array.isArray(result.clarify_options)) return []
  const hit = clarifyOptionsCache.get(result)
  if (hit) return hit
  const value = result.clarify_options.filter((o) =>
    o && typeof o.label === 'string' && typeof o.question === 'string' && o.label.trim() && o.question.trim())
  clarifyOptionsCache.set(result, value)
  return value
}
// 一轮渲染调两遍（v-if 判空 + KbAnswer 入参），按 result 引用缓存（同 knowledgeSources）
const compoundAnalysisCache = new WeakMap<AskResult, string>()
function compoundAnalysis(result: AskResult): string {
  const hit = compoundAnalysisCache.get(result)
  if (hit !== undefined) return hit
  const summary = userFacingMarkdown(result.view?.insight ?? '')
  const value = summary || (result.subs ?? [])
    .map((sub) => {
      const insight = userFacingMarkdown(sub.result.view?.insight ?? '')
      return insight ? `### ${sub.question}\n${insight}` : ''
    })
    .filter(Boolean)
    .join('\n\n')
  compoundAnalysisCache.set(result, value)
  return value
}
const biFocus = ref<DeepSection | null>(null)
// bi-focus 沉浸层：Esc 关闭（与 trace 抽屉同一手法：打开挂 keydown，关闭摘掉）
function onBiFocusEsc(e: KeyboardEvent) { if (e.key === 'Escape') biFocus.value = null }
watch(biFocus, (v) => {
  if (v) window.addEventListener('keydown', onBiFocusEsc)
  else window.removeEventListener('keydown', onBiFocusEsc)
})
function downloadCsv(columns: string[], rows: unknown[][], filename: string) {
  // 防公式注入：= + - @ 开头的文本进 Excel 会被当公式执行，前置 ' 转义
  const cell = (value: unknown) => {
    const text = String(value ?? '')
    return `"${(/^[=+\-@]/.test(text) ? `'${text}` : text).replace(/"/g, '""')}"`
  }
  const csv = [columns, ...rows].map((row) => row.map(cell).join(',')).join('\r\n')
  const href = URL.createObjectURL(new Blob(['\ufeff', csv], { type: 'text/csv;charset=utf-8' }))
  const a = document.createElement('a')
  a.href = href
  a.download = `${filename.replace(/[\\/:*?"<>|]/g, '_') || 'dms-export'}.csv`
  a.click()
  setTimeout(() => URL.revokeObjectURL(href), 1000) // 0ms 回收与下载起动有竞态（大文件/Firefox 会下到空 blob），宽限 1s
}
function exportSection(sec: DeepSection) {
  downloadCsv(sec.columns, sec.rows, sec.title || 'BI数据')
}

function drill(dim: string, baseQuestion: string, targetConvId?: number) {
  send(`${baseQuestion} 按${dim}`, { targetConvId })
}

// 【深度模式】精简(现状)/深度切换。深度生成产物卡；只有用户点击卡片才打开预览。
// 持久化在 localStorage（模式是用户习惯，不该每开一次页面重选）。
const deepMode = ref(localStorage.getItem('dms-mode') === 'deep')
function setMode(deep: boolean) {
  deepMode.value = deep
  localStorage.setItem('dms-mode', deep ? 'deep' : 'lite')
}

const askInput = ref<HTMLTextAreaElement | null>(null)
// 输入框随内容自动增高（rows=1 起步，上限 160px 见 CSS）；发送清空后收回一行
function growAskInput() {
  const el = askInput.value
  if (!el) return
  el.style.height = 'auto'
  el.style.height = `${Math.min(el.scrollHeight, 160)}px`
}
watch(question, () => void nextTick(growAskInput))

function onKey(e: KeyboardEvent) {
  if (e.isComposing || e.keyCode === 229) return
  if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send() }
}

/** AI 解读：折叠 + **按需** `POST /api/analysis`（URL 在 `api.ts`，理由见那里）。
 *
 *  为什么按需而不是随 `/api/ask` 一起出：解读要再过一次模型，问数 p95 本来就几十秒级
 *  （机器调用方与只看数的人不该为它付这份延迟）。结果缓存在这一轮里，收起再展开不重拉。
 *
 *  为什么是 POST + 回传素材、而不是 `GET /记录id/analysis`：服务端**不存**这次结果，
 *  也就没有 id 可给（`AskResult` 里没有 `record_id`，`meta.query_log` 的行号拿不到）。
 *  素材全在前端手上，但必须连同 `/api/ask` 签发的 receipt 原样回传；服务端会拒绝被
 *  截断或改写的问句/SQL/列/行/补充明细/比较/经营上下文。这样无需再存一份结果，也不会
 *  把客户端自造数字送进模型。`row_count` 必须仍给服务端返回的总行数。
 *
 *  响应是 `{caliber, insight, report_receipt}`：
 *  - `caliber` **恒有**（确定性、零 LLM）：这个数是怎么算的（来源表/过滤/时间窗/去重）。
 *    这正是计划 D2-3 点名要的「解读必须带口径说明」，**不许因为 insight 为 null 就整块不显示**。
 *  - `insight` 可能是 `null`（模型挂了/回了网址/开关关着）→ 只显示 caliber，**不标成失败**。
 *    解读失败绝不能让一次已经成功的取数看起来失败。
 *  - `report_receipt` 绑定事实与 insight，报表入口据此拒绝客户端改写后的“AI 解读”。
 *
 *  失败一律**原样显示服务端消息**：端点没上线时 axum 兜底 404 是**空体**，
 *  `resp.json()` 会当场抛 `Unexpected end of JSON input` —— 那句话对用户毫无意义。
 *  所以先取 text 再试解析，解析不出就把状态码/原文当消息。 */
function analysisMaterialOf(question: string, r: AskResult): Record<string, unknown> {
  return {
    question,
    sql: r.sql,
    columns: r.columns,
    rows: r.rows ?? [],
    row_count: r.row_count,
    caliber_note: r.caliber_note ?? null,
    supplemental: r.supplemental ? {
      columns: r.supplemental.columns,
      rows: r.supplemental.rows ?? [],
      row_count: r.supplemental.row_count,
    } : null,
    comparisons: (r.comparisons ?? []).map(({ label, current, baseline, change, pct }) => ({
      label, current, baseline, change, pct,
    })),
    sales_context: r.sales_context ? {
      columns: r.sales_context.columns,
      rows: r.sales_context.rows ?? [],
      row_count: r.sales_context.rows?.length ?? 0,
    } : null,
    subs: (r.subs ?? []).map((sub) => analysisMaterialOf(sub.question, sub.result)),
  }
}

async function toggleAnalysis(t: Turn) {
  const a = (t.analysis ??= { open: false, loading: false })
  a.open = !a.open
  if (!a.open || a.loading || a.caliber || a.insight) return
  const r = t.result
  if (!r) return
  if (!r.analysis_receipt) {
    a.error = '该结果缺少分析素材凭证，请重新查询后再解读'
    return
  }
  a.loading = true
  a.error = undefined
  try {
    const resp = await fetch(ANALYSIS_URL, {
      method: 'POST',
      headers: authHeaders(),
      body: JSON.stringify({
        ...analysisMaterialOf(t.question ?? '', r),
        analysis_receipt: r.analysis_receipt,
        // 深度模式：Precise 档四段式（结论/关键发现/口径与可信度/建议）；精简 = fast 2-4 句
        deep: t.mode === 'deep' ? true : null,
        login_name: sessionToken.value ? null : loginName.value,
        role_code: roleCode.value || null,
      }),
    })
    const raw = await resp.text()
    let d: { caliber?: string; insight?: string | null; report_receipt?: string; error?: string } = {}
    try { d = raw ? JSON.parse(raw) : {} } catch { /* 非 JSON（404 空体 / 网关 HTML）：按原文报 */ }
    if (resp.ok && (d.caliber || d.insight)) {
      a.caliber = d.caliber
      a.insight = d.insight ?? undefined
      a.reportReceipt = d.report_receipt
    } else {
      a.error = d.error || (raw.trim() ? raw.trim().slice(0, 300) : `HTTP ${resp.status}`)
    }
  } catch (e) {
    a.error = String(e)
  } finally {
    a.loading = false
  }
}

// 【S2】把这次解读固化成报表 artifact（零 LLM：服务端重算口径，并验 facts + insight receipt）。
// 成功后只生成产物卡；预览必须由用户点击卡片触发，完成回调不得抢占当前页面。
async function saveReport(t: Turn) {
  const a = t.analysis, r = t.result
  if (!a || !r || a.saving) return
  if (t.convId == null) { a.error = '该历史消息缺少会话归属，无法生成报表'; return }
  if (!r.analysis_receipt) { a.error = '该结果缺少分析素材凭证，请重新查询后再生成报表'; return }
  if (!a.reportReceipt) { a.error = '该解读缺少报表凭证，请重新生成解读'; return }
  a.saving = true
  a.error = undefined
  try {
    const resp = await fetch(ANALYSIS_REPORT_URL, {
      method: 'POST',
      headers: authHeaders(),
      body: JSON.stringify({
        ...analysisMaterialOf(t.question ?? '', r),
        analysis_receipt: r.analysis_receipt,
        insight: a.insight ?? null,
        report_receipt: a.reportReceipt,
        // 【图表】view.blocks 的 Chart 块回声（只是下标与图型 —— 数据服务端用 columns/rows 自取）
        charts: (r.view?.blocks ?? [])
          .filter((b) => b.type === 'chart')
          .map((b) => ({ kind: b.kind, x: b.x, y: b.y, series: b.series ?? null, top: b.top ?? null })),
        conv_id: String(t.convId),
        login_name: sessionToken.value ? null : loginName.value,
        role_code: roleCode.value || null,
      }),
    })
    const d = await resp.json().catch(() => ({}))
    if (!resp.ok) { a.error = d.error || `报表保存失败 ${resp.status}`; return }
    a.artifact = { url: d.preview_url, title: d.title }
  } catch (e) {
    a.error = String(e)
  } finally {
    a.saving = false
  }
}

// CSV 导出（SuperSonic chat-sdk 气泡脚 ⬇CSV）：从结果 rows 生成 CSV 下载
function exportCsv(t: Turn) {
  if (!t.result) return
  downloadCsv(t.result.columns, t.result.rows, (t.question || 'dms-export').slice(0, 20))
}
function exportSupplementalCsv(t: Turn) {
  const detail = t.result?.supplemental
  if (!detail) return
  downloadCsv(detail.columns, detail.rows, `${(t.question || 'dms-export').slice(0, 20)}-明细`)
}

</script>

<template>
  <div class="wrap" :class="{ 'has-preview': !!preview }">
    <!-- 侧栏 -->
    <aside class="side" :class="{ open: sideOpen }">
      <div class="side-hd">
        <span class="logo"><img src="/logo.png" alt="" width="22" height="22" />皇家小虎</span>
        <button class="btn-icon" @click="toggleTheme" title="明暗切换">{{ theme === 'dark' ? '☀️' : '🌙' }}</button>
      </div>
      <div class="sec">
        <div class="sec-t">会话 <button class="btn-sm" @click="newSession">+ 新建</button></div>
      </div>
      <!-- 知识库管理入口：整节按 kb_manager 显隐（设置页可配角色/人员名单，缺省仅管理员）；
           不过闸的人连「知识库」标签都不见，不留一个点了没反应的死入口 -->
      <div class="sec" v-if="kbManager">
        <div class="sec-t">知识库 <button class="btn-sm" @click="openKnowledge">📁 上传/管理</button></div>
      </div>
      <!-- 今日经营日报：服务端 feed=daily 只返回当天生成的一份。 -->
      <div class="sec" v-if="hasAdminAccess && digests.length">
        <div class="sec-t">经营日报</div>
        <button v-for="a in digests" :key="a.id" type="button" class="hist-item" @click="openPreview(a.preview_url, a.title); sideOpen = false">
          <span class="hi-title">📊 {{ a.title }}</span>
        </button>
      </div>
      <div class="sec weekly-sec">
        <div class="sec-t">
          <span>经营周报</span>
          <button class="weekly-create" :disabled="weeklyBusy || sending || curQueue.length > 0" :title="sending || curQueue.length > 0 ? '当前会话分析完成后可生成周报' : '生成单省区经营周报'" @click="openWeeklyReport">
            <span v-if="weeklyBusy" class="spin"></span>
            {{ weeklyBusy ? '生成中' : '生成周报' }}
          </button>
        </div>
        <div class="weekly-caption">单省区 · 本周对比上周与去年同期</div>
      </div>
      <div class="hist">
        <div v-if="!convs.length" class="hist-empty">还没有会话，点「+ 新建」或直接提问</div>
        <div v-for="c in convs" :key="c.id" class="hist-item" :class="{ active: c.id === curConvId }" role="button" tabindex="0"
             @click="openConv(c.id); sideOpen = false" @keydown.self.enter.prevent="openConv(c.id); sideOpen = false" @keydown.self.space.prevent="openConv(c.id); sideOpen = false">
          <span v-if="convRunning(c.id)" class="hi-run" title="该会话仍在分析"><span class="spin"></span></span>
          <span class="hi-title">{{ c.title }}</span>
          <span class="hi-time">{{ c.time }}</span>
          <button class="hi-trace" title="Trace 时间线：回放该会话的问答过程" aria-label="Trace 时间线" @click="openTrace(c.id, $event)">🕓</button>
          <button class="hi-clear" title="清空会话记录（保留会话，追问上下文将重置）" aria-label="清空会话记录" @click="clearConv(c.id, $event)">🧹</button>
          <button class="hi-del" title="删除会话" aria-label="删除会话" @click="delConv(c.id, $event)">×</button>
        </div>
      </div>
      <div class="sec side-ft">
        <div class="health" role="button" tabindex="0" title="点击重新检查后端状态" @click="checkHealth" @keydown.enter="checkHealth"><span class="dot" :class="{ ok: healthOk, busy: healthBusy }"></span>{{ health }}</div>
        <div class="readonly">🔒 纯查询模式（无写操作）</div>
      </div>
    </aside>
    <!-- 移动端抽屉遮罩：仅 ≤820px 抽屉态可见（桌面端 .side-mask display:none） -->
    <div v-if="sideOpen" class="side-mask" @click="sideOpen = false"></div>

    <KbPanel
      v-if="kbOpen" :token="sessionToken" :login="loginName" :initial-space="knowledgeSpaceId"
      @close="kbOpen = false" @auth-expired="handleSessionExpired" @space-change="rememberKnowledgeSpace"
    />

    <!-- 【使用统计】用量弹窗（今日/累计/路由分布/近 7 天） -->
    <UsagePanel
      v-if="usageOpen" :token="sessionToken" :login="loginName" :route-labels="routeLabel"
      @close="usageOpen = false" @auth-expired="handleSessionExpired"
    />

    <!-- 【提示词包】Skills 管理弹窗（列表/启停/编辑/删除；写入口按 admin 显隐） -->
    <SkillsPanel
      v-if="skillsOpen" :token="sessionToken" :login="loginName" :admin="hasAdminAccess"
      @close="skillsOpen = false" @auth-expired="handleSessionExpired"
    />

    <!-- 【数据地图】表关系图谱弹窗（力导向图 + 路径高亮；接受/拒绝按钮只对 admin 渲染） -->
    <DataMapPanel
      v-if="datamapOpen" :token="sessionToken" :login="loginName" :admin="hasAdminAccess"
      @close="datamapOpen = false" @auth-expired="handleSessionExpired"
    />

    <!-- 【SQL 审计】执行记录抽屉（状态过滤 + 行点击展开完整 SQL；全员只读） -->
    <SqlAuditPanel
      v-if="auditOpen" :token="sessionToken" :login="loginName" :route-labels="routeLabel"
      @close="auditOpen = false" @auth-expired="handleSessionExpired"
    />

    <!-- 【Trace 时间线】会话回放抽屉（侧栏每项 🕓 打开；Esc/遮罩关闭） -->
    <div v-if="traceConvId != null" class="trace-mask" @click.self="closeTrace">
      <div class="trace-drawer" role="dialog" aria-modal="true" aria-label="Trace 时间线">
        <header class="trace-hd">
          <span class="trace-title">🕓 Trace 时间线 · 会话 #{{ traceConvId }}</span>
          <button type="button" class="trace-close" title="关闭" aria-label="关闭" @click="closeTrace">✕</button>
        </header>
        <div v-if="traceLoading" class="trace-state"><span class="spin"></span>加载中…</div>
        <div v-else-if="traceError" class="trace-state trace-err">{{ traceError }}</div>
        <TracePanel v-else :rounds="traceRounds" @preview="openPreview" />
      </div>
    </div>

    <!-- 主区 -->
    <div class="main">
      <div class="topbar">
        <button type="button" class="btn-icon mobile-menu" title="会话列表与导航" aria-label="会话列表与导航" @click="sideOpen = true">☰</button>
        <div class="brand"><img src="/logo.png" alt="" width="24" height="24" class="brand-mark" />数据智能<span class="sub">DMS · 自然语言取数</span></div>
        <div class="sp"></div>
        <span v-if="sessionToken" class="dms-user">已登录 <b>{{ loginName || '认证中…' }}</b><template v-if="roleCode"> · {{ roleCode }}</template></span>
        <button v-if="kbManager" class="btn-sm mobile-kb" title="企业知识库" @click="openKnowledge">知识库</button>
        <button v-if="sessionToken" class="btn-sm" title="今日/累计提问与路由分布" @click="usageOpen = true">📈 使用统计</button>
        <button v-if="sessionToken" class="btn-sm" title="提示词包：注入深度报告规划提示词，admin 可写" @click="skillsOpen = true">🧩 提示词包</button>
        <button v-if="sessionToken" class="btn-sm" title="表关系图谱：节点=表、边=关系，admin 可接受/拒绝推断边" @click="datamapOpen = true">🗺 数据地图</button>
        <button v-if="sessionToken" class="btn-sm" title="SQL 执行审计：状态过滤，点行展开完整 SQL（只读）" @click="auditOpen = true">🧾 SQL 审计</button>
        <button v-if="hasAdminAccess" class="btn-sm" title="模型与系统设置" @click="goSettings">⚙ 设置</button>
        <button class="btn-sm" @click="newSession">+ 新会话</button>
        <button v-if="sessionToken && !embedded" class="btn-sm" @click="logout">退出</button>
      </div>

      <!-- 设置页（`/#/settings`）：业务库/模型供应商的切换、编辑与连通性测试，保存即生效 -->
      <div v-if="view === 'settings' && hasAdminAccess" class="chat set-wrap">
        <div class="set-head">
          <div class="set-title">系统设置</div>
          <button type="button" class="set-back" @click="goChat">← 返回对话</button>
        </div>
        <div v-if="llmMsg" class="set-msg">{{ llmMsg }}</div>

          <!-- ═══ 数据库连接 ═══ -->
        <section class="set-card">
          <div class="set-hd">
            <span class="set-bar"></span>
              <div><b>数据库连接</b><span class="set-sub">DMS 权限源不自动参与查询；当前查询库：{{ dbCfg?.target ?? '…' }} · 保存和切换即时生效</span></div>
            <button v-if="dbEditor === 'closed'" class="btn-mini primary set-add" @click="newDbTarget">＋ 新增数据库</button>
          </div>
          <div v-if="!dbCfg" class="set-note">加载中…</div>
          <template v-else>
            <div class="tgt-list">
              <div v-for="t in dbCfg.targets" :key="t.name" class="tgt" :class="{ on: t.current }">
                <span class="tgt-dot"></span>
                <div class="tgt-info">
                  <b>{{ t.name }}<span v-if="t.builtin" class="tgt-tag">内建</span><span v-else-if="t.type" class="tgt-tag">{{ t.type === 'production_lookup' ? '生产轻点查' : '数仓分析' }}</span></b>
                  <span class="tgt-host">{{ t.host }}</span>
                </div>
                <div class="tgt-ops">
                  <button class="btn-mini" @click="editDbTarget(t)">修改</button>
                  <button v-if="!t.protected && !t.builtin" class="btn-mini danger" :disabled="!dbTargetRemovable(t)" :title="t.current ? '删除请求将由服务端校验当前生效状态' : '删除数据库目标'" @click="removeTarget(t.name)">删除</button>
                  <button v-if="!t.current && t.selectable !== false" class="btn-mini primary" :disabled="dbSaving" @click="saveDbTarget(t.name)">{{ dbSwitching === t.name ? '切换中…' : '切换' }}</button>
                  <span v-else-if="t.protected" class="tgt-protected">权限库</span>
                  <span v-else class="tgt-on">生效中</span>
                </div>
              </div>
            </div>
            <div v-if="dbEditor !== 'closed'" class="set-editor">
              <div class="set-form-title">
                <span>{{ dbEditor === 'edit' ? `修改数据库 · ${dbEditingName}` : '新增数据库' }}</span>
                <span v-if="dbEditor === 'edit'" class="set-tip">密码留空时保留原密码，凭据不会回显</span>
              </div>
              <div class="f-grid">
                <label class="f-item"><span>目标名</span><input v-model="dbForm.name" :disabled="dbEditor === 'edit'" placeholder="如 zhongtai" /></label>
                <label class="f-item"><span>类型</span>
                  <select v-model="dbForm.type">
                    <option value="warehouse">Doris / 分析数仓</option>
                    <option value="production_lookup">生产 MySQL / 轻点查</option>
                  </select>
                </label>
                <label class="f-item f-w2"><span>地址</span><input v-model="dbForm.host" placeholder="主机 / IP" /></label>
                <label class="f-item"><span>端口</span><input v-model.number="dbForm.port" type="number" /></label>
                <label class="f-item"><span>数据库名</span><input v-model="dbForm.db" /></label>
                <label class="f-item"><span>账号</span><input v-model="dbForm.user" autocomplete="off" placeholder="修改时可留空" /></label>
                <label class="f-item f-w2"><span>密码</span><input v-model="dbForm.pass" type="password" autocomplete="new-password" placeholder="修改时可留空" /></label>
              </div>
              <div class="set-note">
                {{ dbForm.type === 'warehouse'
                  ? '分析数仓可用于聚合、趋势和 BI 报表。'
                  : '生产库仅允许命中索引的单表等值点查，禁止 JOIN、聚合、排序和模糊搜索。' }}
              </div>
              <div class="f-actions">
                <button class="btn" :disabled="dbTesting" @click="testDbConn">{{ dbTesting ? '测试中…' : '测试连通性' }}</button>
                <button class="btn primary" :disabled="dbSaving" @click="addTarget">{{ dbSaving ? '保存中…' : '保存' }}</button>
                <button class="btn" @click="cancelDbEdit">取消</button>
              </div>
              <div v-if="dbTest" class="f-test" :class="dbTest.ok ? 'ok' : 'bad'">
                {{ dbTest.ok ? `连通正常（${dbTest.ms}ms · ${dbForm.type === 'warehouse' ? 'Doris' : 'MySQL'} ${dbTest.version} · 只读已确认）` : dbTest.error }}
              </div>
            </div>
          </template>
        </section>

        <!-- ═══ 模型供应商 ═══ -->
        <section class="set-card">
          <div class="set-hd">
            <span class="set-bar"></span>
            <div><b>模型供应商</b><span class="set-sub" v-if="llmCfg">当前生效：{{ llmCfg.effective.model_fast }} · {{ llmCfg.effective.vision ? '支持多模态' : '无多模态' }}</span></div>
            <button v-if="llmEditor === 'closed'" class="btn-mini primary set-add" @click="newLlmProvider">＋ 新增供应商</button>
          </div>
          <div v-if="!llmCfg && !llmMsg" class="set-note">加载中…</div>
          <template v-if="llmCfg">
            <div class="vision-fallback">
              <div class="vf-copy">
                <b>备用多模态供应商</b>
                <span v-if="primaryHasVision">主模型已支持图片，图片直接由主模型处理；{{ selectedFallbackVision ? `已配置的 ${providerLabel(selectedFallbackVision.name)} 备用模型当前不会调用。` : '无需配置备用模型。' }}</span>
                <span v-else-if="selectedFallbackVision">主模型不支持图片，图片将由 {{ providerLabel(selectedFallbackVision.name) }} · {{ selectedFallbackVision.vision_model }} 处理。</span>
                <span v-else>主模型不支持图片，请选择一个已配置 Key 的多模态供应商。</span>
              </div>
              <select v-model="fallbackVisionProvider" class="vf-select" :disabled="fallbackVisionSaving" @change="saveFallbackVision">
                <option value="">不设置备用</option>
                <option v-for="p in visionCandidates" :key="p.name" :value="p.name">{{ providerLabel(p.name) }} · {{ p.vision_model }}</option>
              </select>
            </div>
            <div class="tgt-list">
              <div v-for="p in llmProviderRows" :key="p.name.toLowerCase()" class="tgt" :class="{ on: llmCfg.provider?.toLowerCase() === p.name.toLowerCase() }">
                <span class="tgt-dot"></span>
                <div class="tgt-info">
                  <b>{{ providerLabel(p.name) }}<span v-if="p.custom" class="tgt-tag">自定义</span><span v-if="llmCfg.provider?.toLowerCase() === p.name.toLowerCase()" class="tgt-tag">主模型</span><span v-if="fallbackVisionProvider.toLowerCase() === p.name.toLowerCase()" class="tgt-tag">备用图片</span></b>
                  <span class="tgt-host">{{ p.model_fast }}{{ p.model_precise !== p.model_fast ? ' / ' + p.model_precise : '' }}
                    <em class="tgt-vision" :class="{ off: !p.vision }">{{ p.vision ? '✓ 多模态' : '✗ 无多模态' }}</em>
                    <em v-if="!p.key_ready" class="tgt-vision off">key 未配置</em>
                  </span>
                </div>
                <div class="tgt-ops">
                  <button class="btn-mini" @click="editLlmProvider({ name: p.name, base_url: p.base_url, model_fast: p.model_fast, model_precise: p.model_precise, thinking: p.thinking, vision: p.vision_model })">修改</button>
                  <button v-if="p.custom" class="btn-mini danger" :disabled="!llmProviderRemovable(p)" :title="llmCfg.provider?.toLowerCase() === p.name.toLowerCase() || fallbackVisionProvider.toLowerCase() === p.name.toLowerCase() ? '删除请求将由服务端校验模型占用状态' : '删除模型供应商'" @click="removeLlmProvider(p.name)">删除</button>
                  <button v-if="llmCfg.provider?.toLowerCase() !== p.name.toLowerCase()" class="btn-mini primary" :disabled="llmSaving || !p.key_ready" :title="p.key_ready ? '切换为该供应商' : 'key 未配置，请先配置'" @click="saveProvider(p.name)">{{ llmSwitching.toLowerCase() === p.name.toLowerCase() ? '切换中…' : '切换' }}</button>
                  <span v-else class="tgt-on">生效中</span>
                </div>
              </div>
            </div>
            <div v-if="llmEditor !== 'closed'" class="set-editor">
              <div class="set-form-title">
                <span>{{ llmEditor === 'edit' ? `修改供应商 · ${llmEditingName}` : '新增模型供应商' }}</span>
                <span class="set-tip">可从预设自动填充<template v-if="llmEditor === 'edit'">，Key 留空时保留已存值</template></span>
              </div>
              <div class="f-grid">
                <label class="f-item f-w2"><span>供应商预设</span>
                  <select v-model="llmForm.preset" @change="onPreset"><option value="">手动填写</option><option v-for="p in settingsCat?.llm_presets ?? []" :key="p.name" :value="p.name">{{ p.label }}</option><option value="custom">自定义（OpenAI 兼容）</option></select>
                </label>
                <label class="f-item"><span>供应商名</span><input v-model="llmForm.name" :disabled="llmEditor === 'edit'" placeholder="如 kimi" /></label>
                <label class="f-item"><span>思考级别</span><select v-model="llmForm.thinking"><option value="off">关</option><option value="low">低</option><option value="high">高</option><option value="none">默认</option><option v-if="llmForm.thinking === 'keep'" value="keep">保留高级参数</option></select></label>
                <label class="f-item f-w2"><span>Base URL</span><input v-model="llmForm.base_url" /></label>
                <label class="f-item"><span>快速模型</span><input v-model="llmForm.model_fast" /></label>
                <label class="f-item"><span>精准模型</span><input v-model="llmForm.model_precise" placeholder="可空" /></label>
                <label class="f-item"><span>多模态模型</span><input v-model="llmForm.vision" placeholder="可空，表示不支持" /></label>
                <label class="f-item f-w2"><span>API Key</span><input v-model="llmForm.key" type="password" autocomplete="new-password" placeholder="修改时可留空" /></label>
              </div>
              <div class="f-actions">
                <button class="btn" :disabled="llmTesting" @click="testLlmConn">{{ llmTesting ? '测试中…' : '测试连通性' }}</button>
                <button class="btn primary" :disabled="llmSaving" @click="addLlmProvider">{{ llmSaving ? '保存中…' : '保存' }}</button>
                <button class="btn" :disabled="llmSaving" @click="cancelLlmEdit">取消</button>
              </div>
              <div v-if="llmTest" class="f-test" :class="llmTest.ok ? 'ok' : 'bad'">
                {{ llmTest.ok ? `连通正常（${llmTest.ms}ms · 输入 ${llmTest.usage?.prompt_tokens ?? 0} / 输出 ${llmTest.usage?.completion_tokens ?? 0} tokens）` : llmTest.error }}
              </div>
              <div class="set-keys" v-if="settingsCat?.llm_keys?.length">
                <span class="set-tip">已配置 Key</span>
                <span v-for="k in settingsCat.llm_keys" :key="k.name" class="key-chip">{{ k.name }} ✓<button class="key-del" :disabled="k.protected || llmCfg.provider?.toLowerCase() === k.name.toLowerCase() || fallbackVisionProvider.toLowerCase() === k.name.toLowerCase()" :title="llmKeyDelTitle(k)" @click="removeLlmKey(k.name)">×</button></span>
              </div>
            </div>
          </template>
        </section>

        <!-- ═══ 知识库入口权限 ═══ -->
        <section class="set-card">
          <div class="set-hd">
            <span class="set-bar"></span>
            <div><b>知识库入口权限</b><span class="set-sub">侧栏「知识库」与上传/管理面对谁开放；检索问答不受此限</span></div>
          </div>
          <div class="set-note">
            两个名单是「或」的关系，<b>都留空 = 仅管理员可见</b>（缺省）。角色码即 DMS 角色编码（如 kb_admin），登录名逐人开放；保存即生效，被移除的人刷新后入口消失。
          </div>
          <div class="f-grid">
            <label class="f-item f-w2"><span>按角色开放（角色码，逗号分隔）</span>
              <input v-model="kbGrantsText.roles" placeholder="如 kb_admin, ops_manager；留空不按角色开放" />
            </label>
            <label class="f-item f-w2"><span>按人员开放（登录名，逗号分隔）</span>
              <input v-model="kbGrantsText.logins" placeholder="如 zhangsan, lisi；留空不按人员开放" />
            </label>
          </div>
          <div class="f-actions">
            <button class="btn primary" :disabled="kbGrantsSaving" @click="saveKbGrants">{{ kbGrantsSaving ? '保存中…' : '保存' }}</button>
          </div>
        </section>

        <!-- ═══ 质量控制面 ═══ -->
        <section v-if="quality || qualityLoading" class="set-card quality-card">
          <div class="set-hd"><span class="set-bar"></span>质量与性能
            <span class="set-sub">真实问答日志聚合，不展示 SQL 与敏感结果</span>
            <select v-model.number="qualityDays" class="q-days" @change="loadQuality"><option :value="1">1天</option><option :value="7">7天</option><option :value="30">30天</option></select>
          </div>
          <div v-if="qualityLoading && !quality" class="set-note">加载中…</div>
          <template v-else-if="quality">
            <div class="q-kpis">
              <div><span>查询总量</span><b>{{ quality.summary.total }}</b></div>
              <div><span>成功率</span><b>{{ quality.summary.success_rate.toFixed(1) }}%</b></div>
              <div><span>P95</span><b>{{ fmtLatency(quality.summary.p95_ms) }}</b></div>
              <div><span>LLM 路径</span><b>{{ quality.summary.llm_rate.toFixed(1) }}%</b></div>
              <div><span>反馈</span><b>{{ quality.summary.feedback_count }}</b></div>
            </div>
            <div class="q-grid">
              <div>
                <div class="set-form-title">路由质量</div>
                <table class="q-table"><thead><tr><th>路由</th><th>次数</th><th>P95</th><th>失败</th></tr></thead>
                  <tr v-for="r in quality.routes" :key="r.route"><td>{{ routeLabel[r.route] || r.route }}</td><td>{{ r.count }}</td><td>{{ fmtLatency(r.p95_ms) }}</td><td>{{ r.errors }}</td></tr>
                </table>
              </div>
              <div>
                <div class="set-form-title">最近反馈</div>
                <div v-if="!quality.feedback.length" class="set-note">暂无反馈。</div>
                <div v-for="f in quality.feedback" :key="f.id" class="q-feedback">
                  <div><b>{{ f.kind }}</b><span>{{ f.login_name }} · {{ f.route }}</span><button class="btn-mini" :disabled="feedbackBusy !== null" @click="resolveFeedback(f.id, f.status === 'open' ? 'resolved' : 'open')">{{ f.status === 'open' ? '处理' : '重开' }}</button></div>
                  <p>{{ f.question }}</p><small v-if="f.detail">{{ f.detail }}</small>
                </div>
              </div>
            </div>
          </template>
        </section>

        <!-- ═══ VQR 可信样例 ═══ -->
        <section class="set-card vqr-card">
          <div class="set-hd"><span class="set-bar"></span>可信 SQL 样例
            <span class="set-sub">AI 只做初筛，必须通过当前分析库真实只读执行才参与召回</span>
            <select v-model="exemplarFilter" class="q-days" @change="loadExemplars">
              <option value="">全部</option><option value="pending">待复核</option>
              <option value="enabled">已启用</option><option value="disabled">已禁用</option>
            </select>
          </div>
          <div v-if="exemplarLoading && !exemplars.length" class="set-note">加载中…</div>
          <div v-else-if="!exemplars.length" class="set-note">暂无 SQL 样例。</div>
          <div v-else class="vqr-list">
            <article v-for="e in exemplars" :key="e.id" class="vqr-row">
              <div class="vqr-main">
                <div class="vqr-title"><b>{{ e.question }}</b><code>#{{ e.id }}</code></div>
                <div class="vqr-meta">
                  <span class="vqr-state" :class="e.validation_status">{{ validationLabel(e.validation_status) }}</span>
                  <span>{{ e.ds_id }}<template v-if="e.validated_source"> · {{ e.validated_source }}</template></span>
                  <span v-if="e.ai_review">AI {{ e.ai_review }}</span>
                  <span v-if="e.metric_versions">{{ e.metric_versions }}</span>
                  <span v-if="e.reviewed_by">{{ e.reviewed_by }}</span>
                </div>
                <p v-if="e.invalid_reason" class="vqr-error">{{ e.invalid_reason }}</p>
                <details class="vqr-sql"><summary>查看 SQL</summary><pre>{{ e.sql }}</pre></details>
              </div>
              <div class="vqr-ops">
                <button class="btn-mini primary" :disabled="exemplarBusy !== null" @click="setExemplarStatus(e.id, 'enabled')">
                  {{ exemplarBusy === e.id ? '验证中…' : (e.validation_status === 'valid' ? '重新验证' : '验证并启用') }}
                </button>
                <button v-if="e.status !== 'disabled'" class="btn-mini danger" :disabled="exemplarBusy !== null" @click="setExemplarStatus(e.id, 'disabled')">禁用</button>
              </div>
            </article>
          </div>
        </section>
      </div>

      <div v-else class="chat" ref="chatEl" @click="onChatClick">
        <!-- 欢迎语 -->
        <div v-if="!turns.length" class="turn">
          <div class="bubble ai">
            <img src="/logo.png" alt="" width="40" height="40" class="hello-mark" />
            嗷呜~ 我是 <b>皇家小虎 · 数据智能</b>。用自然语言查询任意数据——订单、客户、商品、库存、财务、活动、售后，<b>数据权限与你的 DMS 账号完全一致</b>。<br /><br />
            试试：<i>本月销售额</i> · <i>销售额按省区</i> · <i>买过烤肠的客户</i> · <i>昨天的订单明细</i>
          </div>
        </div>

        <template v-for="(t, ti) in turns" :key="t.turnKey || `${curConvId ?? 'draft'}:${ti}`">
          <!-- 用户气泡 -->
          <div v-if="t.role === 'user'" class="turn">
            <div class="bubble user">{{ t.question }}</div>
          </div>
          <!-- AI 气泡 -->
          <div v-else class="turn">
            <div v-if="t.loading" class="thinking" :class="{ deep: t.mode === 'deep' }">
              <template v-if="t.mode === 'deep'">
                <div class="think-state">
                  <span class="spin"></span>
                  <div>
                    <span><b>分析中…</b><strong>{{ t.elapsed ?? 0 }}s</strong></span>
                    <small>{{ t.progress?.length ? t.progress[t.progress.length - 1] : '理解问题与业务口径' }}</small>
                  </div>
                </div>
                <div class="think-steps">
                  <div v-for="(s, i) in (t.progress?.length ? t.progress.slice(-4) : ['理解问题与业务口径'])" :key="`${i}:${s}`" class="think-step" :class="{ current: i === Math.min(t.progress?.length || 1, 4) - 1 }">
                    <span class="ts-ok">{{ i === Math.min(t.progress?.length || 1, 4) - 1 ? '›' : '✓' }}</span><span>{{ s }}</span>
                  </div>
                </div>
              </template>
              <template v-else>
                <span class="spin"></span><span>分析中… <b class="elapsed">{{ t.elapsed ?? 0 }}s</b></span>
                <span class="thinking-hint">大数据量查询约需 10~60 秒</span>
              </template>
              <button type="button" class="stop-generation" @click="stopGeneration(t)">停止生成</button>
            </div>
            <div v-else-if="t.error" class="bubble err">
              <div>⚠️ {{ t.error }}</div>
              <!-- 多角色账号：拉到清单才出这一排（拿不到就只剩上面那句后端文案）。
                   按钮而不是下拉框：少一次点击，也不存在「空下拉框」这种更糟的形态。 -->
              <div v-if="t.roles?.length" class="role-pick">
                <span>选择角色后重试（不同角色的数据权限档不同）：</span>
                <button v-for="r in t.roles" :key="r" class="btn-sm" :disabled="rolePicking" @click="pickRole(r, t.retryQuestion || turns[ti - 1]?.question, t.retryOptions, t.convId)">{{ r }}</button>
              </div>
              <!-- 【D4】断点续跑：服务端账本判定可续跑才显示（已完成板块零重跑） -->
              <button v-if="t.resumable && t.rid" type="button" class="retry" :class="{ disabled: t.resuming }" @click="resumeDeep(t)">↻ 续跑（从断点继续，不重跑已完成板块）</button>
              <button v-else-if="t.retryQuestion || turns[ti - 1]?.question" type="button" class="retry" @click="send(t.retryQuestion || turns[ti - 1]?.question, { ...t.retryOptions, targetConvId: t.convId })">↻ 重试</button>
            </div>
            <!-- 【D6】promote 回放：别的会话钉进来的产物引用（点击走深链拦截开预览面板） -->
            <div v-else-if="t.promoted" class="bubble ai">
              <div class="res-meta"><span>📌 引用的产物<template v-if="t.promoted.version"> · v{{ t.promoted.version }}</template></span></div>
              <div class="art-card">
                <a class="art-link" :href="t.promoted.url">📄 <b>{{ t.promoted.title }}</b><span class="art-hint">已钉到本会话 · 点击预览/分享</span></a>
                <button type="button" class="art-share" title="发分享链接" @click.stop="shareArtifact(artifactIdOf(t.promoted.url))">🔗</button>
              </div>
              <p v-if="t.promoted.note" class="promote-note">{{ t.promoted.note }}</p>
            </div>
            <div v-else-if="t.result" class="bubble ai" :class="{ 'knowledge-bubble': t.result.kind === 'text', 'result-bubble': t.result.kind !== 'text' }">
              <div class="res-meta">
                <!-- 知识库只展示面向业务的回答与关联资料概览，不暴露内部引用编号/调试计数。 -->
                <span v-if="t.result.kind === 'text'">{{ t.streaming ? '已命中资料，正在生成…' : '已关联资料' }}</span>
                <template v-else>
                  <!-- 🔴 **不写具体行数上限**。这里原来是 `'·截断200'` —— 那是全仓第**四**处 200
                   字面量，与后端 `agent::gate::MAX_ROWS` 零连接、零判据；
                   而隔壁 ResultPanel.vue 顶上那段「前端不再持有行数上限」的长注释因此不成立。
                   具体数字与续读参数由后端 `truncation_note` 说全（ResultPanel 渲染它）。 -->
              <span>{{ t.result.row_count }} 行{{ t.result.truncated ? ' · 已截断' : '' }}</span>
                  <!-- 深度模式不出 AI 解读钮：分析默认做（在产物页里），按钮是重复入口 -->
                  <button v-if="t.mode !== 'deep' && t.result.row_count > 0" type="button" class="sql-toggle" @click="toggleAnalysis(t)">
                    {{ t.analysis?.open ? '收起解读' : '🤖 AI 解读' }}
                  </button>
                  <button v-if="t.result.row_count > 0" type="button" class="sql-toggle" @click="exportCsv(t)">⬇ 导出 CSV</button>
                  <button v-if="t.result.supplemental" type="button" class="sql-toggle" @click="exportSupplementalCsv(t)">⬇ 导出明细 CSV</button>
                  <button v-if="turnSqls(t).length" type="button" class="sql-toggle" @click="t.showSql = !t.showSql">{{ t.showSql ? '隐藏' : '查看' }} SQL</button>
                </template>
                <button v-if="t.streaming" type="button" class="stop-generation" @click="stopGeneration(t)">停止生成</button>
                <!-- 【引用上轮】该轮问题+结论摘要进输入框上方的引用 chip 区，随下一条提问发出 -->
                <button type="button" class="sql-toggle" title="引用该轮问答作为下一条提问的上下文" @click="quoteTurn(t)">↩ 引用</button>
                <!-- 【分支会话】只挂在持久会话的轮上（draft 轮没有 convId 可分支）；from_seq 是该轮的 1 基序号 -->
                <button v-if="t.convId != null" type="button" class="sql-toggle branch-toggle" :class="{ disabled: branchBusy }" title="从该轮岔出一个新会话（复制到该轮为止的上下文）" @click="branchTurn(t, ti)">⑂ 分支</button>
              </div>
              <div v-if="t.showSql && t.result.kind !== 'text'" class="sql-stack">
                <details v-for="(item, qi) in turnSqls(t)" :key="qi" class="sql-item" :open="qi === 0">
                  <summary>{{ item.title }}</summary>
                  <pre class="sql">{{ item.sql }}</pre>
                </details>
              </div>
              <!-- 【深度模式】产物卡（compose 端点给的富页：总值+拆解+趋势+明细+图+AI 分析）。
                   点击走深链拦截 → 右侧沙箱面板；与 S2 的 .art-card 同形 -->
              <div v-if="t.artifact" class="art-card">
                <a class="art-link" :href="t.artifact.url">
                📄 <b>{{ t.artifact.title }}</b><span class="art-hint">深度分析页已生成 · 点击预览/分享</span>
                </a>
                <button type="button" class="art-share" title="发分享链接" @click.stop="shareArtifact(artifactIdOf(t.artifact.url))">🔗</button>
              </div>
               <!-- 【深度页聊天内嵌】问题理解 → KPI → 板块（图+表）→ 明细 → AI 分析收尾。
                    数据全在 page 载荷里，与分享页同源（同一次取数、同一份内容） -->
               <template v-if="t.page">
                 <div class="deep-page-head">
                   <div class="deep-page-title">
                     <span>深度 BI</span>
                     <b>{{ t.page.label || t.question || '经营数据分析' }}</b>
                   </div>
                   <div class="deep-page-meta">
                     <span>{{ t.page.sections?.length ?? 0 }} 个分析板块</span>
                     <span v-if="t.page.facts?.length">{{ t.page.facts.length }} 项关键数据</span>
                   </div>
                 </div>
                 <div v-if="t.page.understanding" class="deep-objective">
                   <span>分析目标</span>
                   <p>{{ t.page.understanding }}</p>
                 </div>
                 <!-- 规划了却没跑出来的板块**必须点名**：此前它们被静默丢掉，页面只是少一块，
                      用户既不知道少了什么、也不知道剩下的数是不是完整的。
                      老服务端不带 missing_sections 键 = 整区不渲染（降级同 assertions）。 -->
                 <div v-if="t.page.missing_sections?.length" class="deep-objective deep-gap">
                   <span>本次未取到的板块</span>
                   <p>{{ t.page.missing_sections.join('、') }}。以上结论只覆盖已取到的部分。</p>
                 </div>
                 <!-- 【D8】验收断言透出区：规划时定的「每板块要证明什么」+ 末次自评。
                      老服务端不带 assertions 键 = 整区不渲染（降级同后端纪律） -->
                 <div v-if="t.page.assertions?.length" class="daccept">
                   <div class="daccept-t">验收断言</div>
                   <div v-for="a in t.page.assertions" :key="`${a.section}:${a.text}`" class="daccept-item">
                     <em class="daccept-v" :class="a.verdict || 'pending'">{{ verdictLabel(a.verdict) }}</em>
                     <span v-if="a.section" class="daccept-sec">{{ a.section }}</span>
                     <span class="daccept-text">{{ a.text }}</span>
                   </div>
                 </div>
                 <div v-if="t.page.kpi" class="dkpi">
                  <div class="dk-main">
                    <span class="dk-l">{{ t.page.kpi.label }}</span>
                    <span class="dk-v">{{ formatLabeledValue(t.page.kpi.label, t.page.kpi.value) }}</span>
                    <small>本期实际值</small>
                  </div>
                  <div v-if="deepComparisons(t.page).length" class="dk-comparisons">
                    <div v-for="cmp in deepComparisons(t.page)" :key="`${cmp.label}-${cmp.basis || ''}`" class="dk-compare" :class="cmp.dir">
                      <span>{{ cmp.label }}</span>
                      <b>{{ comparisonRate(cmp) }}</b>
                      <small>{{ cmp.basis || '同口径、同长度窗口' }}</small>
                      <div v-if="typeof cmp.baseline === 'number' || typeof cmp.change === 'number'" class="dk-compare-detail">
                        <span>基期 {{ comparisonNumber(cmp.baseline, t.page.kpi.label) }}</span>
                        <span>变化额 {{ signedComparison(cmp.change, t.page.kpi.label) }}</span>
                      </div>
                    </div>
                  </div>
                </div>
                <div v-if="t.page.facts?.length" class="df-grid">
                  <div v-for="f in t.page.facts" :key="f.label" class="df-card">
                    <span>{{ f.label }}</span><b>{{ formatLabeledValue(f.label, f.value) }}</b>
                  </div>
                </div>
                <div v-if="t.page.highlights?.length" class="dh-grid">
                  <div v-for="h in t.page.highlights" :key="h.label" class="dh-card">
                    <span>{{ h.label }}</span><b>{{ formatLabeledValue(h.label, h.value) }}</b><small>{{ h.note }}</small>
                  </div>
                </div>
                <div v-if="t.page.contributions?.length" class="dsec contribution-sec">
                  <div class="dsec-head">
                    <div class="dsec-copy">
                      <div class="dsec-t">头部贡献与集中度</div>
                      <div class="dsec-q">基于已执行结构板块，展示前三项及板块内占比</div>
                    </div>
                  </div>
                  <div class="dtable-wrap">
                    <table class="dtable">
                      <tr><th>板块</th><th>排名</th><th>对象</th><th>指标</th><th>指标值</th><th>板块内占比</th></tr>
                      <tr v-for="(r, ri) in t.page.contributions" :key="ri">
                        <td>{{ r[0] ?? '' }}</td><td>{{ r[1] ?? '' }}</td><td>{{ r[2] ?? '' }}</td><td>{{ r[3] ?? '' }}</td>
                        <td>{{ fmt(r[4], metricSemantic(String(r[3] ?? ''))) }}</td><td>{{ r[5] ?? 0 }}%</td>
                      </tr>
                    </table>
                  </div>
                </div>
                <div v-for="(sec, si) in t.page.sections ?? []" :key="si" class="dsec" :class="{ 'table-sec': sec.kind === 'table' }">
                  <div class="dsec-head">
                    <div class="dsec-copy">
                      <div class="dsec-t">{{ sec.title }}</div>
                      <div v-if="sec.question" class="dsec-q">{{ sec.question }}</div>
                    </div>
                    <div class="dsec-tools">
                      <span class="dsec-stat">{{ sec.rows.length }} 行 · {{ sec.columns.length }} 列</span>
                      <div v-if="sec.kind !== 'table'" class="dsec-seg" aria-label="展示方式">
                        <button :class="{ on: secView(sec) === 'chart' }" @click="sec.view = 'chart'">图表</button>
                        <button :class="{ on: secView(sec) === 'table' }" @click="sec.view = 'table'">数据</button>
                      </div>
                      <button class="dsec-icon" @click="exportSection(sec)" title="导出当前板块 CSV" aria-label="导出当前板块 CSV">↓</button>
                      <button class="dsec-icon" @click="biFocus = sec" title="放大查看" aria-label="放大查看">⛶</button>
                    </div>
                  </div>
                  <BiChart v-if="sec.rows.length && secView(sec) === 'chart'" :kind="secChartKind(sec)" :columns="secCols(sec)" :rows="sec.rows" :x="0" :y="secY(sec)" :series="secSeries(sec)" />
                  <div v-else-if="secView(sec) === 'table'" class="dtable-wrap">
                    <table class="dtable">
                      <thead><tr><th v-for="c in sec.columns" :key="c" :class="{ num: semanticForLabel(c) !== 'none' }">{{ c }}</th></tr></thead>
                      <tbody><tr v-for="(r, ri) in sec.rows.slice(0, DEEP_TABLE_PREVIEW_ROWS)" :key="ri"><td v-for="(_, ci) in sec.columns" :key="ci" :class="{ num: secCellSemantic(sec, r, ci) !== 'none' }">{{ secCell(sec, r, ci) }}</td></tr></tbody>
                    </table>
                  </div>
                  <div v-if="secView(sec) === 'table' && sec.rows.length > DEEP_TABLE_PREVIEW_ROWS" class="dmore">当前显示 {{ DEEP_TABLE_PREVIEW_ROWS }} 行，共 {{ sec.rows.length }} 行 · 可导出完整 CSV</div>
                </div>
                <div v-if="t.page.recent?.rows?.length" class="dsec">
                  <div class="dsec-t">最近订单明细</div>
                  <div class="dtable-wrap">
                    <table class="dtable">
                      <thead><tr><th v-for="c in t.page.recent.columns" :key="c">{{ c }}</th></tr></thead>
                      <tr v-for="(r, ri) in t.page.recent.rows.slice(0, 6)" :key="ri"><td v-for="(_, ci) in t.page.recent.columns" :key="ci">{{ formatCell(t.page.recent.columns, r[ci], ci) }}</td></tr>
                    </table>
                  </div>
                </div>
              </template>

              <!-- 🔴 口径复核未通过 / 截断三件套的渲染**已下移到 ResultPanel**（结果自己那一层）：
                   此前只有这里读顶层，于是复合问**每个子问**的口径提醒与截断提醒全看不见。
                   这里只保留「容器自己带了它」这一路（`AskResult::compound` 今天恒 None，
                   将来若给容器补上标注，这两行就是它的落点），
                   条件带 `subs?.length` 是为了不让单结果显示两遍。 -->
              <template v-if="t.result.subs?.length">
                <div v-if="t.result.caliber_note" class="caliber-warn">{{ t.result.caliber_note }}</div>
                <div v-if="t.result.truncation_note" class="trunc-note">{{ t.result.truncation_note }}</div>
              </template>

              <!-- 复合问题拆解（deepagents）：多子面板 -->
              <template v-if="t.result.subs?.length">
                <div v-for="(sub, si) in t.result.subs" :key="si" class="sub-panel">
                  <div class="sub-title">🔹 {{ sub.question }}</div>
                  <ResultPanel :result="dataOnlyResult(sub.result)" @drill="(d: string) => drill(d, sub.question, t.convId)" @pick="(q: string) => send(q, { targetConvId: t.convId })" />
                </div>
              </template>
              <!-- 知识库回答：markdown + 角标 + 来源（没有 view，走不了 ResultPanel） -->
              <template v-else-if="t.result.kind === 'text'">
                <div v-if="knowledgeSources(t.result).folders.length" class="knowledge-folder-trail" aria-label="关联资料目录">
                  <span>资料路径</span>
                  <b v-for="path in knowledgeSources(t.result).folders.slice(0, 4)" :key="path">{{ path }}</b>
                  <small v-if="knowledgeSources(t.result).folders.length > 4">+{{ knowledgeSources(t.result).folders.length - 4 }}</small>
                </div>
                <KbAnswer
                  :result="knowledgePresentation(t.result)"
                  :token="sessionToken"
                  :login="loginName"
                  :trace-id="t.result.trace_id"
                  :streaming="t.streaming === true"
                  @auth-expired="handleSessionExpired"
                />
              </template>
              <!-- 单结果 -->
              <ResultPanel v-else-if="!t.page" :result="t.result" @drill="(d: string) => drill(d, t.question || '', t.convId)" @pick="(q: string) => send(q, { targetConvId: t.convId })" />

              <!-- 【混合查询】数据面板之下挂知识库回答（两路并行那一路的产物） -->
              <div v-if="t.result?.kb" class="ai-panel hybrid-kb-panel">
                <div class="ai-hd"><span class="ai-mark">📚</span> 知识库资料<span class="ai-hint">同一问题的资料侧回答</span></div>
                <KbAnswer
                  :result="hybridKnowledgePresentation(t.result)"
                  :token="sessionToken"
                  :login="loginName"
                  :trace-id="t.result.kb.trace_id"
                  @auth-expired="handleSessionExpired"
                />
              </div>
              <!-- 混合查询的 AI 综合（数据 + 资料两段结论合一），位置与复合汇总同规 -->
              <div v-if="t.result?.kb && t.result.view?.insight" class="ai-panel deep-insight">
                <div class="ai-hd"><span class="ai-mark">AI</span> 综合分析<span class="ai-hint">基于上方数据与资料，聚焦关联与行动</span></div>
                <KbAnswer :result="{ markdown: userFacingMarkdown(t.result.view.insight) }" />
              </div>

              <!-- 所有深度/复合数据渲染完成后，AI 分析统一收尾。
                   🔴 `v-else-if` 不是笔误：上面那块（混合查询的 AI 综合）与下面那块
                   （复合汇总）都可能命中同一段文字 —— `compoundAnalysis` 返回的正是
                   `view.insight`。混合问句现在还能带多条问数子问（`hybrid::split`），
                   `subs` 非空是常态，三块必须串成一条链，否则同一段结论上下贴着出两遍。
                   深度页恒有 `t.page`、混合恒无，两者互斥，链接安全。 -->
              <div v-else-if="t.page?.insight" class="ai-panel deep-insight">
                <div class="ai-hd"><span class="ai-mark">AI</span> 经营分析<span class="ai-hint">基于本次查询数据，聚焦变化、异常与行动</span></div>
                <KbAnswer :result="{ markdown: userFacingMarkdown(t.page.insight) }" />
              </div>
              <div v-else-if="t.result.subs?.length && compoundAnalysis(t.result)" class="ai-panel deep-insight compound-insight">
                <div class="ai-hd"><span class="ai-mark">AI</span> 综合分析<span class="ai-hint">基于上方全部数据，聚焦关联变化与行动</span></div>
                <KbAnswer :result="{ markdown: compoundAnalysis(t.result) }" />
              </div>

              <!-- 普通模式的按需 AI 必须位于全部 KPI、图表、表格、明细和复合子结果之后。 -->
              <div v-if="t.analysis?.open" class="ai-panel analysis-last">
                <div class="ai-hd"><span class="ai-mark">AI</span> 分析结论<span class="ai-hint">基于上方数据，聚焦变化、异常与行动</span>
                  <button v-if="t.analysis.caliber && !t.analysis.artifact" type="button" class="sql-toggle"
                     @click="saveReport(t)">{{ t.analysis.saving ? '生成中…' : '生成报表' }}</button>
                </div>
                <div v-if="t.analysis.artifact" class="art-card">
                  <a class="art-link" :href="t.analysis.artifact.url"><b>{{ t.analysis.artifact.title }}</b><span class="art-hint">报表已生成 · 点击预览/分享</span></a>
                  <button type="button" class="art-share" title="发分享链接" @click.stop="shareArtifact(artifactIdOf(t.analysis.artifact.url))">🔗</button>
                </div>
                <div v-if="t.analysis.loading" class="ai-loading"><span class="spin"></span>解读中…</div>
                <div v-else-if="t.analysis.error" class="ai-err">{{ t.analysis.error }}</div>
                <KbAnswer v-else-if="t.analysis.insight" :result="{ markdown: userFacingMarkdown(t.analysis.insight) }" />
                <div v-else class="ai-hint">本次没有可展示的分析结论</div>
              </div>

              <!-- 【意图澄清】后端 clarify_options：意图不明时的候选问法，chip 同「换个维度看」，
                   点击 = 把该 question 直接发出（沿用会话追问机制）。字段缺席 = 现状不变。 -->
              <div v-if="clarifyOptionsOf(t.result).length" class="clarify-opts">
                <span class="clarify-t">你可以这样问：</span>
                <button v-for="c in clarifyOptionsOf(t.result)" :key="c.question" type="button" class="pill" @click="send(c.question, { targetConvId: t.convId })">{{ c.label }}</button>
              </div>
            </div>
          </div>
        </template>
      </div>

      <!-- 能力切换 + 快捷 pill -->
      <div class="quick">
        <button v-for="c in CAPS" :key="c.v" type="button" class="pill cap" :class="{ on: intent === c.v }"
              :title="'问答能力：' + c.t" @click="intent = c.v">{{ c.t }}</button>
        <span v-if="intent !== 'data' && knowledgeSpaceName" class="kb-scope" :title="intent === 'auto' ? `自动分诊为知识库时仅检索 ${knowledgeSpaceName}` : `仅检索 ${knowledgeSpaceName}`">
          {{ knowledgeSpaceName }}
        </span>
        <span class="cap-sp"></span>
        <button type="button" class="pill mobile-weekly" :disabled="weeklyBusy || sending || curQueue.length > 0" @click="openWeeklyReport">经营周报</button>
        <button v-for="q in quick" :key="q" type="button" class="pill" @click="send(q)">{{ q }}</button>
      </div>

      <!-- 【引用上轮】chip 区 + 输入栏 +【排队追问】队列：一个外壳共用一条上边框 -->
      <div class="composer">
        <div v-if="pendingRefs.length" class="refbar">
          <span class="ref-tag">引用</span>
          <span v-for="(r, ri) in pendingRefs" :key="ri" class="ref-chip" :title="r">
            <em class="chip-text">{{ refChipLabel(r) }}</em>
            <button class="chip-del" title="移除该引用" @click="pendingRefs.splice(ri, 1)">×</button>
          </span>
        </div>

      <!-- 输入栏（当前提问还在跑时，新问句进本会话队列等待续发） -->
      <div class="inputbar">
        <!-- 【深度模式】精简|深度：深度 = AI 深度参与并生成可点击预览的报表卡 -->
        <div class="mode-seg" :class="{ disabled: intent === 'knowledge' }" :title="intent === 'knowledge' ? '知识库模式固定使用引用式回答' : '深度模式：AI 深度参与生成与分析，自动出深度解读与报表（更慢更丰满）'">
          <button type="button" :class="{ on: !deepMode }" :disabled="intent === 'knowledge'" @click="intent !== 'knowledge' && setMode(false)">精简</button>
          <button type="button" :class="{ on: deepMode }" :disabled="intent === 'knowledge'" @click="intent !== 'knowledge' && setMode(true)">深度</button>
        </div>
        <textarea ref="askInput" v-model="question" :placeholder="sending ? '当前提问仍在分析，Enter 发送将排队等待…' : '用自然语言提问，Enter 发送，Shift+Enter 换行…'" @keydown="onKey" @input="growAskInput" rows="1"></textarea>
        <button class="send" :disabled="!question.trim()" @click="send()">{{ sending ? '排队' : '发送' }}</button>
      </div>

      <!-- 【Y5 插话】运行中可插一条修正指令（「不是这个口径，按 X 重算」）；运行结束自动隐藏 -->
      <div v-if="sending && curConvId != null" class="steerbar">
        <input v-model="steerText" class="steer-input" maxlength="500"
               placeholder="插话修正当前计算：例如「不是这个口径，按净额重算」…"
               @keydown.enter.prevent="sendSteer" />
        <button class="steer-btn" :disabled="!steerText.trim() || steerBusy" @click="sendSteer">插话</button>
        <span v-if="curSteerCount" class="steer-tag">已受理 {{ curSteerCount }}</span>
      </div>

      <div v-if="curQueue.length" class="queuebar">
        <span class="queue-tag">已排队 {{ curQueue.length }}</span>
        <span v-for="item in curQueue" :key="item.id" class="q-chip" :title="item.text">
          <em class="chip-text">{{ item.text }}</em>
          <button class="chip-del" title="取消这条排队" @click="cancelQueued(item.id)">×</button>
        </span>
      </div>
      </div>
    </div>

    <!-- 【子任务面板】深度模式：聊天右侧呈现阶段进度条与板块子任务卡片（完成后可折叠） -->
    <DeepTaskPanel v-if="view === 'chat' && deepTaskTurn" :key="deepTaskTurn.turnKey || 'deep-task'" :turn="deepTaskTurn" />

    <!-- 轻 toast（操作反馈浮层） -->
    <div v-if="toastMsg" class="toast" role="status" aria-live="polite">{{ toastMsg }}</div>

    <div v-if="weeklyOpen" class="weekly-mask" @click.self="closeWeeklyReport" @keydown.esc="closeWeeklyReport">
      <form class="weekly-dialog" role="dialog" aria-modal="true" aria-labelledby="weekly-title" @submit.prevent="generateWeeklyReport">
        <header class="weekly-head">
          <div>
            <span class="weekly-kicker">经营周报</span>
            <h2 id="weekly-title">生成单省区周度分析</h2>
          </div>
          <button type="button" class="weekly-close" title="关闭" @click="closeWeeklyReport">✕</button>
        </header>
        <p class="weekly-intro">汇总销售与单品表现；门店、费用、库存仅在取得已验证数据时分析，缺失项会明确标注。</p>
        <label class="weekly-field">
          <span>省区名称</span>
          <input
            ref="weeklyProvinceInput" v-model="weeklyProvince" maxlength="30"
            autocomplete="off" placeholder="例如：湖南省" @input="weeklyError = ''"
          />
        </label>
        <div class="weekly-period">
          <span>分析周期</span>
          <b>{{ weeklyRange.start }} 至 {{ weeklyRange.end }}</b>
          <small>对比上周、去年同期</small>
        </div>
        <div v-if="weeklyError" class="weekly-error">{{ weeklyError }}</div>
        <footer class="weekly-actions">
          <button type="button" class="weekly-cancel" @click="closeWeeklyReport">取消</button>
          <button class="weekly-submit" :disabled="weeklyBusy || !weeklyProvince.trim()">
            {{ weeklyBusy ? '正在生成…' : '开始生成' }}
          </button>
        </footer>
      </form>
    </div>

    <div v-if="loginVisible" class="login-mask" role="dialog" aria-modal="true" aria-label="登录">
      <form class="login-box" @submit.prevent="passwordLogin">
        <div class="login-brand"><img src="/logo.png" alt="" width="34" height="34" />皇家小虎</div>
        <h1>DMS 数据智能</h1>
        <p>使用 DMS 账号登录，数据权限与 DMS 完全一致</p>
        <template v-if="!loginRoles.length">
          <label><span>账号</span><input v-model.trim="loginName" autocomplete="username" autofocus /></label>
          <label><span>密码</span><input v-model="loginPassword" type="password" autocomplete="current-password" /></label>
          <div v-if="loginError" class="login-error">{{ loginError }}</div>
          <button class="login-submit" :disabled="loginBusy">{{ loginBusy ? '登录中…' : '登录' }}</button>
        </template>
        <template v-else>
          <div class="login-role-title">选择本次使用的 DMS 角色</div>
          <button v-for="r in loginRoles" :key="r" type="button" class="login-role" :disabled="rolePicking" @click="pickRole(r)">{{ r }}<span>›</span></button>
        </template>
      </form>
    </div>

    <!-- 【S1】artifact 右侧预览面板（datanote 形态）。iframe 双重沙箱：服务端 CSP sandbox
         + 这里的 sandbox 属性（无 allow-same-origin ⇒ 透明源，够不到本页 DOM 与 Cookie）。
         下载按钮走同一身份查询串（download 端点在服务端做同样的归属校验）。 -->
    <div v-if="preview" class="pv" :style="{ flexBasis: previewW }">
      <div class="pv-drag" @mousedown="startDrag" title="拖动调整宽度"></div>
      <div class="pv-hd">
        <span class="pv-title">{{ preview.title }}<template v-if="previewVer"> · v{{ previewVer }}</template></span>
        <button type="button" class="pv-act" @click="toggleVersions" title="版本历史（重生成留版本，可回看老版本）">🕘 版本</button>
        <button type="button" class="pv-act" @click="exportPreview('csv')" title="把产物页里的表格导成 CSV">⬇ CSV</button>
        <button type="button" class="pv-act" @click="exportPreview('xlsx')" title="把产物页里的表格导成 Excel">⬇ Excel</button>
        <button type="button" class="pv-act" @click="pvPromoteOpen = !pvPromoteOpen; pvVersionsOpen = false" title="把该产物引用到自己的另一会话">📌 引用</button>
        <button type="button" class="pv-act" @click="shareArtifact(artifactIdOf(preview.sourceUrl))" title="发分享链接（免登录只读）">🔗 分享</button>
        <button type="button" class="pv-act" @click="downloadPreview" title="下载">⬇ 下载</button>
        <button type="button" class="pv-act" @click="openPreviewWindow" title="在浏览器新窗口打开">↗ 新窗口</button>
        <button type="button" class="pv-act pv-x" @click="closePreview" title="关闭">✕</button>
      </div>
      <!-- 【D6】版本历史浮层：点版本回看那一版（iframe 仍是同一 CSP 沙箱页） -->
      <div v-if="pvVersionsOpen" class="pv-pop">
        <div v-if="!pvVersions" class="pv-pop-item">加载中…</div>
        <template v-else>
          <button v-for="v in pvVersions" :key="v.version" type="button" class="pv-pop-item" :class="{ on: previewVer ? v.version === previewVer : v.latest }"
             @click="openVersion(v.version)">v{{ v.version }}<small>{{ (v.created_at || '').slice(5, 16).replace('T', ' ') }}<template v-if="v.latest"> · 最新</template></small></button>
        </template>
      </div>
      <!-- 【D6】引用到会话浮层：侧栏只列自己的会话，服务端 promote 再核一次属主（fail-closed） -->
      <div v-if="pvPromoteOpen" class="pv-pop">
        <div v-if="!convs.length" class="pv-pop-item">还没有会话</div>
        <button v-for="c in convs" :key="c.id" type="button" class="pv-pop-item" @click="promoteArtifact(c.id, c.title)">📌 {{ c.title }}</button>
      </div>
      <div v-if="preview.loading" class="pv-state"><span class="spin"></span>正在加载预览…</div>
      <div v-else-if="preview.error" class="pv-state pv-error">{{ preview.error }}</div>
      <iframe v-else-if="preview.html" class="pv-frame" :srcdoc="preview.html" :title="preview.title" sandbox="allow-scripts"></iframe>
    </div>

    <!-- 单个 BI 板块的沉浸查看：图表/数据仍共用同一份结果，不重复查询。 -->
    <div v-if="biFocus" class="bi-focus" @click.self="biFocus = null">
      <section class="bi-focus-card">
        <header class="bi-focus-hd">
          <div><b>{{ biFocus.title }}</b><small v-if="biFocus.question">{{ biFocus.question }}</small></div>
          <div class="dsec-tools">
            <span class="dsec-stat">{{ biFocus.rows.length }} 行 · {{ biFocus.columns.length }} 列</span>
            <div v-if="biFocus.kind !== 'table'" class="dsec-seg">
              <button :class="{ on: secView(biFocus) === 'chart' }" @click="biFocus.view = 'chart'">图表</button>
              <button :class="{ on: secView(biFocus) === 'table' }" @click="biFocus.view = 'table'">数据</button>
            </div>
            <button class="dsec-icon" @click="exportSection(biFocus)" title="导出 CSV" aria-label="导出 CSV">↓</button>
            <button class="dsec-icon" @click="biFocus = null" title="关闭" aria-label="关闭">✕</button>
          </div>
        </header>
        <div class="bi-focus-body">
          <BiChart v-if="biFocus.rows.length && secView(biFocus) === 'chart'" :kind="secChartKind(biFocus)" :columns="secCols(biFocus)" :rows="biFocus.rows" :x="0" :y="secY(biFocus)" :series="secSeries(biFocus)" :height="520" />
          <div v-else class="dtable-wrap bi-focus-table">
            <table class="dtable">
              <thead><tr><th v-for="c in biFocus.columns" :key="c" :class="{ num: semanticForLabel(c) !== 'none' }">{{ c }}</th></tr></thead>
              <tbody><tr v-for="(r, ri) in biFocus.rows" :key="ri"><td v-for="(_, ci) in biFocus.columns" :key="ci" :class="{ num: secCellSemantic(biFocus, r, ci) !== 'none' }">{{ secCell(biFocus, r, ci) }}</td></tr></tbody>
            </table>
          </div>
        </div>
      </section>
    </div>
  </div>
</template>

<style>
.wrap { display: flex; height: 100vh; min-height: 0; }
.wrap.has-preview .side { width: 220px; }
.wrap.has-preview .topbar { padding-inline: 12px; }
.wrap.has-preview .topbar .brand .sub { display: none; }
.wrap.has-preview .chat { padding-inline: 16px; }
/* 侧栏 */
.side { width: 268px; flex-shrink: 0; border-right: 1px solid var(--border); background: var(--bg-card); display: flex; flex-direction: column; min-height: 0; }
.side-hd { padding: 16px; border-bottom: 1px solid var(--divider); display: flex; align-items: center; justify-content: space-between; }
.side-hd .logo { font-size: 16px; font-weight: 650; background: var(--brand-ink); -webkit-background-clip: text; background-clip: text; -webkit-text-fill-color: transparent; }
.sec { padding: 12px 16px; border-bottom: 1px solid var(--divider); }
.sec-t { font-size: 12px; font-weight: 600; color: var(--text-muted); text-transform: uppercase; letter-spacing: .4px; display: flex; align-items: center; justify-content: space-between; }
.weekly-sec { padding-block: 11px; }
.weekly-create { height: 27px; display: inline-flex; align-items: center; gap: 6px; padding: 0 10px; border: 1px solid var(--primary); border-radius: 5px; background: var(--primary-bg); color: var(--primary); font-family: inherit; font-size: 12px; font-weight: 650; line-height: 1; cursor: pointer; }
.weekly-create:hover { background: var(--primary); color: var(--on-primary); }
.weekly-create:disabled { opacity: .62; cursor: wait; }
.weekly-create .spin { width: 11px; height: 11px; border-width: 1.5px; }
.weekly-caption { margin-top: 7px; color: var(--text-faint); font-size: 11px; line-height: 1.45; }
.hist { flex: 1; overflow-y: auto; padding: 8px 10px; min-height: 0; }
.hist-empty { font-size: 12px; color: var(--text-faint); padding: 8px; }
/* 设置页：列表优先，编辑器按需展开 */
.set-wrap { padding: 20px 28px 60px; }
.set-head { max-width: 920px; margin: 4px auto 14px; display: flex; align-items: baseline; justify-content: space-between; }
.set-title { font-size: 18px; font-weight: 650; color: var(--text-primary); }
.set-head .set-back { padding: 0; border: 0; background: none; font-family: inherit; font-size: 13px; color: var(--primary); cursor: pointer; }
.set-head .set-back:hover { text-decoration: underline; }
.set-wrap > .set-msg { max-width: 920px; margin: 0 auto 12px; }
.set-card { max-width: 920px; margin: 0 auto 16px; background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; padding: 16px 18px 18px; box-shadow: var(--shadow-sm); }
.set-hd { display: flex; align-items: center; gap: 10px; font-size: 15px; font-weight: 650; color: var(--text-primary); margin-bottom: 12px; }
.set-hd > div { min-width: 0; display: flex; align-items: baseline; flex-wrap: wrap; gap: 4px 10px; }
.set-add { margin-left: auto; flex-shrink: 0; }
.set-bar { display: inline-block; width: 4px; height: 15px; border-radius: 2px; background: var(--primary); transform: translateY(2px); }
.set-sub { font-size: 12px; font-weight: 400; color: var(--text-muted); }
.set-note { font-size: 12px; color: var(--text-muted); line-height: 1.8; margin-top: 6px; }
.set-msg { background: var(--primary-bg); color: var(--primary); border-radius: 8px; padding: 7px 12px; font-size: 13px; }
.vision-fallback { display: flex; align-items: center; gap: 16px; margin-bottom: 12px; padding: 10px 12px; border: 1px solid var(--divider); border-radius: 7px; background: var(--bg-main); }
.vf-copy { min-width: 0; flex: 1; display: flex; flex-direction: column; gap: 3px; }
.vf-copy b { font-size: 12.5px; color: var(--text-primary); }
.vf-copy span { font-size: 11.5px; color: var(--text-muted); line-height: 1.5; }
.vf-select { width: min(330px, 42%); height: 32px; border: 1px solid var(--border); border-radius: 7px; padding: 0 9px; background: var(--bg-card); color: var(--text-regular); font-size: 12px; }
.vf-select:focus { outline: none; border-color: var(--primary); box-shadow: var(--ring); }
/* 目标列表行 */
.tgt-list { display: flex; flex-direction: column; gap: 7px; }
.tgt { display: flex; align-items: center; gap: 12px; border: 1px solid var(--border); border-radius: 7px; padding: 9px 12px; background: var(--bg-card); transition: border-color .12s, background .12s; }
.tgt:hover { border-color: var(--primary); }
.tgt.on { border-color: var(--primary); background: var(--primary-bg); }
.tgt-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--text-faint); flex-shrink: 0; }
.tgt.on .tgt-dot { background: var(--primary); }
.tgt-info { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
.tgt-info b { font-size: 13.5px; color: var(--text-primary); display: flex; align-items: center; gap: 6px; }
.tgt-tag { font-size: 10px; font-weight: 500; color: var(--primary); background: var(--primary-bg); border: 1px solid var(--primary); border-radius: 99px; padding: 0 6px; }
.tgt-host { font-size: 12px; color: var(--text-muted); font-family: var(--font-mono); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.tgt-vision { font-style: normal; margin-left: 8px; color: var(--success-text, #178a50); }
.tgt-vision.off { color: var(--text-faint); }
.tgt-ops { display: flex; align-items: center; gap: 6px; flex-shrink: 0; }
.tgt-on { font-size: 11px; color: var(--primary); font-weight: 600; }
.tgt-protected { font-size: 11px; color: var(--text-muted); font-weight: 600; }
.btn-mini { height: 26px; padding: 0 10px; font-size: 12px; border: 1px solid var(--border); background: var(--bg-card); color: var(--text-regular); border-radius: 6px; cursor: pointer; }
.btn-mini:hover { border-color: var(--primary); color: var(--primary); }
.btn-mini.primary { background: var(--primary); border-color: var(--primary); color: var(--on-primary); }
.btn-mini.primary:disabled { opacity: .5; cursor: not-allowed; }
.btn-mini.danger { color: var(--error-text); }
.btn-mini.danger:hover { border-color: var(--error-ring); background: var(--error-bg); }
.btn-mini:disabled { opacity: .42; cursor: not-allowed; }
/* 表单 */
.set-editor { margin-top: 14px; padding-top: 14px; border-top: 1px solid var(--divider); }
.set-form-title { display: flex; align-items: baseline; flex-wrap: wrap; gap: 4px 8px; font-size: 12px; font-weight: 650; color: var(--text-primary); margin-bottom: 10px; }
.set-tip { font-weight: 400; color: var(--text-faint); margin-left: 6px; }
.f-grid { display: grid; grid-template-columns: repeat(4, 1fr); gap: 10px 12px; margin-bottom: 12px; }
.f-item { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
.f-item.f-w2 { grid-column: span 2; }
.f-item span { font-size: 11px; color: var(--text-muted); }
.f-item input, .f-item select { height: 32px; padding: 0 10px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg-card); color: var(--text-regular); font-size: 13px; width: 100%; }
.f-item input:focus, .f-item select:focus { outline: none; border-color: var(--primary); box-shadow: var(--ring); }
.f-item input:disabled { background: var(--bg-main); color: var(--text-muted); cursor: not-allowed; }
.f-actions { display: flex; gap: 8px; }
.btn { height: 32px; padding: 0 14px; font-size: 13px; border: 1px solid var(--border); background: var(--bg-card); color: var(--text-regular); border-radius: 7px; cursor: pointer; }
.btn:hover { border-color: var(--primary); color: var(--primary); }
.btn.primary { background: var(--primary); border-color: var(--primary); color: var(--on-primary); font-weight: 600; }
.btn.primary:hover { filter: brightness(1.06); }
.btn:disabled { opacity: .55; cursor: not-allowed; }
.f-test { margin-top: 10px; font-size: 12.5px; border-radius: 7px; padding: 7px 12px; background: var(--primary-bg); color: var(--primary); }
.f-test.bad { background: var(--error-bg); color: var(--error-text); }
.set-keys { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 12px; align-items: center; }
.key-chip { display: inline-flex; align-items: center; gap: 4px; font-size: 12px; color: var(--success-text, #178a50); background: var(--primary-bg); border: 1px solid var(--border); border-radius: 99px; padding: 2px 10px; }
.key-del { border: 0; padding: 0 1px; background: transparent; color: var(--text-faint); cursor: pointer; font-size: 14px; line-height: 1; }
.key-del:hover { color: var(--error-text); }
.key-del:disabled { opacity: .35; cursor: not-allowed; }
.quality-card .set-hd { flex-wrap: wrap; }
.q-days { margin-left: auto; height: 28px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card); color: var(--text-regular); padding: 0 8px; }
.q-kpis { display: grid; grid-template-columns: repeat(5,minmax(0,1fr)); gap: 8px; margin-bottom: 14px; }
.q-kpis > div { min-width: 0; padding: 10px 12px; border: 1px solid var(--border); background: var(--bg-main); border-radius: 7px; display: flex; flex-direction: column; gap: 3px; }
.q-kpis span { font-size: 11px; color: var(--text-muted); }
.q-kpis b { color: var(--text-primary); font-size: 18px; font-variant-numeric: tabular-nums; }
.q-grid { display: grid; grid-template-columns: .9fr 1.1fr; gap: 16px; }
.q-grid .set-form-title { margin-top: 0; }
.q-table { width: 100%; border-collapse: collapse; font-size: 12px; }
.q-table th,.q-table td { border-bottom: 1px solid var(--divider); padding: 7px 5px; text-align: right; }
.q-table th:first-child,.q-table td:first-child { text-align: left; }
.q-feedback { padding: 8px 0; border-bottom: 1px solid var(--divider); }
.q-feedback > div { display: flex; align-items: center; gap: 7px; }
.q-feedback > div b { font-size: 11px; color: var(--primary); }
.q-feedback > div span { flex: 1; min-width: 0; color: var(--text-faint); font-size: 11px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.q-feedback p { margin: 4px 0 0; font-size: 12px; color: var(--text-regular); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.q-feedback small { display: block; margin-top: 2px; color: var(--text-muted); font-size: 11px; }
.vqr-card .set-hd { flex-wrap: wrap; }
.vqr-list { display: flex; flex-direction: column; border-top: 1px solid var(--divider); }
.vqr-row { display: flex; gap: 14px; padding: 13px 0; border-bottom: 1px solid var(--divider); }
.vqr-main { min-width: 0; flex: 1; }
.vqr-title { display: flex; align-items: center; gap: 8px; min-width: 0; }
.vqr-title b { font-size: 13px; color: var(--text-primary); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.vqr-title code { font-size: 10px; color: var(--text-faint); }
.vqr-meta { display: flex; flex-wrap: wrap; gap: 5px 10px; margin-top: 5px; font-size: 11px; color: var(--text-muted); }
.vqr-state { font-weight: 650; color: var(--text-muted); }
.vqr-state.valid { color: var(--success-text, #178a50); }
.vqr-state.invalid { color: var(--error-text); }
.vqr-state.stale { color: var(--warning-text); }
.vqr-error { margin: 6px 0 0; font-size: 11px; line-height: 1.55; color: var(--error-text); }
.vqr-sql { margin-top: 7px; font-size: 11px; color: var(--text-muted); }
.vqr-sql summary { cursor: pointer; width: fit-content; }
.vqr-sql pre { max-height: 180px; overflow: auto; margin: 6px 0 0; padding: 8px 10px; border: 1px solid var(--divider); border-radius: 6px; background: var(--bg-main); color: var(--text-regular); white-space: pre-wrap; font-family: var(--font-mono); }
.vqr-ops { display: flex; align-items: flex-start; gap: 6px; flex-shrink: 0; }
.hist-item { display: flex; align-items: center; gap: 6px; width: 100%; font-family: inherit; font-size: 13px; text-align: left; border: 0; background: none; color: var(--text-regular); padding: 7px 10px; border-radius: var(--radius-md); cursor: pointer; }
.hist-item:hover { background: var(--bg-hover); }
.hist-item.active { background: var(--primary-light); color: var(--primary); }
.hist-item .hi-title { flex: 1; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.hi-run { width: 16px; height: 16px; display: inline-flex; align-items: center; justify-content: center; flex: 0 0 auto; }
.hi-run .spin { width: 11px; height: 11px; border-width: 1.5px; }
.hist-item .hi-time { font-size: 11px; color: var(--text-faint); flex-shrink: 0; }
.hist-item .hi-del { opacity: 0; border: none; background: none; color: var(--text-faint); cursor: pointer; font-size: 15px; line-height: 1; flex-shrink: 0; }
.hist-item .hi-trace { opacity: 0; border: none; background: none; color: var(--text-faint); cursor: pointer; font-size: 12px; line-height: 1; flex-shrink: 0; }
.hist-item .hi-clear { opacity: 0; border: none; background: none; color: var(--text-faint); cursor: pointer; font-size: 12px; line-height: 1; flex-shrink: 0; }
.hist-item:hover .hi-trace, .hist-item:focus-within .hi-trace { opacity: 1; }
.hist-item:hover .hi-clear, .hist-item:focus-within .hi-clear { opacity: 1; }
.hist-item .hi-trace:hover { color: var(--primary); }
.hist-item:hover .hi-del, .hist-item:focus-within .hi-del { opacity: 1; }
.hist-item .hi-del:hover { color: var(--error-text); }
/* 触屏无 hover：行内删除/Trace 按钮常显，否则永远无法点 */
@media (hover: none) { .hist-item .hi-trace, .hist-item .hi-del { opacity: 1; } }
.side-ft { margin-top: auto; }
.health { font-size: 12px; color: var(--text-muted); display: flex; align-items: center; gap: 6px; cursor: pointer; }
.health .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--text-faint); }
.health .dot.ok { background: var(--success); }
.health .dot.busy { background: var(--warning-text); }
.readonly { font-size: 11px; color: var(--text-faint); margin-top: 5px; }
/* 主区 */
.main { flex: 1; min-width: 0; display: flex; flex-direction: column; min-height: 0; }
.topbar { display: flex; align-items: center; flex-wrap: wrap; gap: 6px 8px; padding: 12px 16px; border-bottom: 1px solid var(--divider); background: var(--bg-card); }
.topbar .brand { font-weight: 650; font-size: 16px; color: var(--text-primary); display: flex; align-items: baseline; gap: 6px; }
.topbar .brand .sub { font-size: 12px; color: var(--text-muted); font-weight: 400; }
.topbar .sp { flex: 1; }
.dms-user { font-size: 12px; color: var(--text-muted); }
/* 对话流 */
.chat { flex: 1; min-width: 0; overflow-y: auto; padding: 20px 24px; min-height: 0; }
.turn { width: 100%; min-width: 0; margin-bottom: 16px; display: flex; flex-direction: column; }
.bubble { max-width: 82%; padding: 12px 16px; font-size: 14px; line-height: 1.65; word-break: break-word; }
.bubble.user { margin-left: auto; width: fit-content; background: var(--primary); color: var(--on-primary); white-space: pre-wrap; border-radius: 12px 12px 4px 12px; }
.bubble.ai { margin-right: auto; width: fit-content; max-width: min(100%, 1120px); background: var(--bg-card); border: 1px solid var(--border); box-shadow: var(--shadow-sm); border-radius: 12px 12px 12px 4px; }
/* 结构化结果（含深度模式返回的澄清卡）按主栏比例伸缩，不按最短内容或固定像素收缩。 */
.bubble.ai.result-bubble { align-self: flex-start; width: 82%; max-width: 100%; min-width: 0; }
.bubble.ai.result-bubble > .result-panel { width: 100%; min-width: 0; }
.bubble.err { margin-right: auto; background: var(--error-bg); border: 1px solid var(--error-ring); color: var(--error-text); border-radius: 12px; }
.thinking { display: inline-flex; align-items: center; gap: 10px; background: var(--bg-card); border: 1px solid var(--border); padding: 10px 14px; border-radius: 8px; font-size: 13px; color: var(--text-regular); box-shadow: var(--shadow-sm); width: fit-content; }
.thinking .elapsed { color: var(--primary); font-variant-numeric: tabular-nums; }
.thinking-hint { font-size: 11px; color: var(--text-faint); border-left: 1px solid var(--divider); padding-left: 10px; }
.stop-generation { margin-left: auto; border: 0; background: none; color: var(--error-text); font: inherit; font-size: 12px; cursor: pointer; white-space: nowrap; }
.stop-generation:hover { text-decoration: underline; }
.thinking.deep { width: min(100%, 620px); display: grid; gap: 7px; padding: 10px 12px; align-items: start; border-radius: 6px; box-shadow: none; }
.think-state { display: flex; gap: 10px; align-items: flex-start; min-width: 0; }
.think-state .spin { width: 16px; height: 16px; margin-top: 2px; flex: 0 0 auto; }
.think-state > div { display: grid; gap: 2px; min-width: 0; }
.think-state span { display: flex; gap: 8px; align-items: baseline; color: var(--text-primary); }
.think-state b { font-size: 13px; }
.think-state strong { color: var(--primary); font-size: 12px; font-variant-numeric: tabular-nums; }
.think-state small { color: var(--text-muted); font-size: 11.5px; line-height: 1.35; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.bubble.err .retry { display: inline-block; margin-top: 8px; padding: 0; border: 0; background: none; font-family: inherit; font-size: 12px; color: var(--primary); cursor: pointer; }
.bubble.err .retry:hover { text-decoration: underline; }
/* 角色选择器（多角色账号被 fail-closed 拒时唯一的出口）：一行标题 + 每个角色一个按钮 */
.bubble.err .role-pick { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; margin-top: 8px; font-size: 12px; }
.spin { width: 14px; height: 14px; border: 2px solid var(--primary); border-top-color: transparent; border-radius: 50%; animation: dnSpin .7s linear infinite; }
@keyframes dnSpin { to { transform: rotate(360deg); } }
.res-meta { display: flex; align-items: center; gap: 10px; font-size: 12px; color: var(--text-muted); margin-bottom: 10px; }
.res-meta .route-badge { font-weight: 600; color: var(--primary); background: var(--primary-bg); padding: 1px 8px; border-radius: var(--radius-full); }
.res-meta .steps { font-size: 12px; }
.res-meta .steps summary { display: inline; cursor: pointer; color: var(--primary); }
.res-meta .steps .step { margin-right: 10px; white-space: nowrap; }
.res-meta .sql-toggle { padding: 0; border: 0; background: none; font: inherit; color: var(--primary); cursor: pointer; }
/* 操作按钮组整体靠右：只有紧跟行数文本的第一个按钮吃 auto 间距，其余按 gap 自然排 ——
   原来是四个互相咬合的内联 style 三元表达式，深度无 page 的边角下按钮就不再靠右 */
.res-meta > span + .sql-toggle { margin-left: auto; }
/* 【分支会话】紧跟「↩ 引用」右排，不瓜分 auto 间距；busy 时只变样不挡其他轮点击（函数内互斥） */
.res-meta .sql-toggle.branch-toggle { margin-left: 10px; }
.res-meta .sql-toggle.branch-toggle.disabled { opacity: .5; pointer-events: none; }

/* 【Trace 时间线】右侧抽屉（侧栏 🕓 打开）。TracePanel 本体是 300px 侧栏 aside
   且窄屏 media query 会 display:none —— 抽屉里必须恒可见、铺满抽屉，故带 !important 覆写。 */
.trace-mask { position: fixed; inset: 0; z-index: 1100; background: rgba(17, 24, 39, .38); backdrop-filter: blur(5px); }
.trace-drawer { position: absolute; top: 0; right: 0; bottom: 0; width: min(360px, 94vw); display: flex; flex-direction: column; border-left: 1px solid var(--border); background: var(--bg-card); box-shadow: -18px 0 50px rgba(17, 24, 39, .18); }
.trace-hd { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 13px 14px 10px; }
.trace-title { color: var(--text-primary); font-size: 13px; font-weight: 700; }
.trace-close { width: 28px; height: 28px; border: 0; border-radius: 5px; background: transparent; color: var(--text-muted); cursor: pointer; }
.trace-close:hover { background: var(--bg-hover); color: var(--text-primary); }
.trace-state { display: flex; align-items: center; justify-content: center; gap: 9px; padding: 30px 16px; color: var(--text-muted); font-size: 13px; }
.trace-err { color: var(--error-text); line-height: 1.7; text-align: center; }
.trace-drawer > .trace-panel { display: flex !important; width: 100% !important; flex: 1; border-left: 0 !important; min-height: 0; }
.sql-stack { margin: 0 0 12px; border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--bg-card); overflow: hidden; }
.sql-item + .sql-item { border-top: 1px solid var(--divider); }
.sql-item summary { cursor: pointer; padding: 9px 12px; color: var(--text-regular); font-size: 12px; font-weight: 650; background: var(--bg-main); }
.sql-item .sql { margin: 0; border-radius: 0; max-height: 280px; }
.sql { background: var(--bg-main); border: 1px solid var(--divider); border-radius: var(--radius-lg); padding: 10px 12px; overflow-x: auto; margin-bottom: 10px; font-family: var(--font-mono); font-size: 12px; color: var(--text-regular); white-space: pre-wrap; }
@media (max-width: 760px) {
  .q-kpis { grid-template-columns: repeat(2,minmax(0,1fr)); }
  .q-grid { grid-template-columns: 1fr; }
  .vqr-row { flex-direction: column; }
  .set-wrap { padding: 14px 12px 40px; }
  .set-card { padding: 14px 12px; }
  .set-hd { align-items: flex-start; flex-wrap: wrap; }
  .set-add { width: 100%; margin-left: 14px; }
  .tgt { align-items: flex-start; flex-wrap: wrap; }
  .vision-fallback { align-items: stretch; flex-direction: column; gap: 8px; }
  .vf-select { width: 100%; }
  .tgt-info { width: calc(100% - 24px); }
  .tgt-ops { width: 100%; padding-left: 20px; flex-wrap: wrap; }
  .f-grid { grid-template-columns: 1fr; }
  .f-item.f-w2 { grid-column: span 1; }
  .f-actions { flex-wrap: wrap; }
}
.empty-hint { background: var(--warning-bg); border-left: 3px solid var(--warning-text); border-radius: var(--radius); padding: 10px 14px; margin-bottom: 12px; font-size: 13px; color: var(--text-regular); line-height: 1.7; }
/* 口径复核未通过：用 error 色而非 warning —— 它说的是「下面这些数字不可信」，
   与「没查到数据」（.empty-hint 用 warning）不是一个量级。加粗是刻意的：
   这条要能在用户读到数字**之前**拦住视线，否则「照返 + 标注」等于没标注。 */
.caliber-warn { background: var(--error-bg); border-left: 3px solid var(--error-ring); border-radius: var(--radius); padding: 10px 14px; margin-bottom: 12px; font-size: 13px; font-weight: 600; color: var(--error-text); line-height: 1.7; }
/* 【S3】need-intent 选择卡：澄清用主色底（中性引导），不用 error 红 —— 它不是失败 */
.ask-card { background: var(--primary-bg); border: 1px solid var(--primary); border-radius: var(--radius-lg); padding: 12px 14px; margin-bottom: 12px; }
.ask-hd { font-weight: 650; font-size: 14px; color: var(--primary); margin-bottom: 6px; }
.ask-q { font-size: 13px; color: var(--text-regular); line-height: 1.7; margin-bottom: 10px; white-space: pre-wrap; }
.ask-opts { display: flex; flex-wrap: wrap; gap: 8px; }
.ask-opt { border: 1px solid var(--primary); background: var(--bg-card); color: var(--primary); border-radius: var(--radius-full); padding: 6px 14px; font-size: 13px; cursor: pointer; }
.ask-opt:hover { background: var(--primary); color: var(--on-primary); }
.ask-hint { font-size: 12px; color: var(--text-faint); margin-top: 8px; }
.trunc-note { background: var(--warning-bg); border-left: 3px solid var(--warning-text); border-radius: var(--radius); padding: 8px 12px; margin-bottom: 12px; font-size: 12px; color: var(--text-regular); line-height: 1.6; word-break: break-all; }
/* 脱敏回显：用中性底而非 error 底 —— 它说的是「按策略不给看」，不是出错。
   用户误判的方向恰恰相反（把空列当故障），所以措辞要正面说明「已脱敏」。 */
.redact-note { background: var(--bg-main); border-left: 3px solid var(--text-muted); border-radius: var(--radius); padding: 8px 12px; margin-bottom: 12px; font-size: 12px; color: var(--text-regular); line-height: 1.6; }
.redact-lock { margin-left: 4px; font-size: 10px; }
.tbl-wrap td.redact-cell { color: var(--text-faint); font-style: italic; text-align: center; }
/* AI 解读折叠面板 */
.ai-panel { border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--bg-main); padding: 10px 12px; margin-bottom: 12px; }
.ai-hd { display: flex; align-items: baseline; gap: 8px; font-size: 12.5px; font-weight: 650; color: var(--text-primary); margin-bottom: 6px; }
/* 「生成报表」钮（button.sql-toggle）在标题行靠右；按钮复位与 .res-meta 那条同款 */
.ai-hd .sql-toggle { margin-left: auto; padding: 0; border: 0; background: none; font: inherit; color: var(--primary); cursor: pointer; }
.ai-hd .ai-hint { font-size: 11px; font-weight: 400; color: var(--text-faint); }
.ai-loading { display: flex; align-items: center; gap: 8px; font-size: 12.5px; color: var(--text-muted); }
.ai-err { font-size: 12.5px; color: var(--error-text); line-height: 1.6; word-break: break-word; }
.deep-insight { background: var(--bg-card); border-color: var(--border); border-left: 4px solid var(--primary); color: var(--text-regular); padding: 0; overflow: hidden; box-shadow: var(--shadow-sm); margin-top: 18px; }
.deep-insight .ai-hd { color: var(--text-primary); font-size: 14px; padding: 13px 16px; margin: 0; border-bottom: 1px solid var(--divider); background: var(--bg-main); }
.deep-insight .ai-hint { color: var(--text-muted); }
.deep-insight .ai-mark { display: inline-grid; place-items: center; width: 27px; height: 20px; border-radius: 4px; background: var(--primary); color: var(--on-primary); font-size: 10px; font-weight: 750; }
.deep-insight .kb { color: var(--text-regular) !important; padding: 12px 16px 15px; }
.deep-insight .kb-body p, .deep-insight .kb-body li { color: var(--text-regular) !important; }
.deep-insight .kb-body h3, .deep-insight .kb-body h4, .deep-insight .kb-body h5, .deep-insight .kb-body h6, .deep-insight .kb-body b { color: var(--text-primary) !important; }
.deep-insight .kb-body code { color: var(--primary) !important; background: var(--primary-bg) !important; border-color: var(--border) !important; }
.deep-insight .kb-body table { font-size: 12px; }
/* 口径说明：逐项一行、恒显示。`pre-wrap` 保留服务端排的行，长条件不横向溢出 */
.ai-caliber {
  font-size: 12px; line-height: 1.7; color: var(--text-regular); margin: 0 0 8px;
  padding: 8px 10px; background: var(--bg-card, #fff); border-left: 3px solid var(--primary);
  border-radius: var(--radius); white-space: pre-wrap; word-break: break-word;
  font-family: var(--font-mono);
}
.ai-panel > .ai-hint { font-size: 12px; color: var(--text-muted); }
.bubble.ai.knowledge-bubble { width: min(100%, 1120px); padding: 0; border: 0; background: transparent; box-shadow: none; }
.knowledge-folder-trail { display: flex; align-items: center; flex-wrap: wrap; gap: 6px; margin: 0 0 10px; padding: 0 2px; }
.knowledge-folder-trail > span { color: var(--text-muted); font-size: 10.5px; }
.knowledge-folder-trail b { max-width: 260px; overflow: hidden; padding: 2px 7px; border: 1px solid rgba(var(--primary-rgb), .18); border-radius: 4px; background: var(--primary-light); color: var(--primary); font-size: 10.5px; font-weight: 650; text-overflow: ellipsis; white-space: nowrap; }
.knowledge-folder-trail small { color: var(--text-muted); font-size: 10.5px; }
.sub-panel { margin-bottom: 18px; }
.sub-title { font-size: 14px; font-weight: 650; color: var(--primary); margin: 10px 0 8px; padding-left: 10px; border-left: 3px solid var(--primary); }
/* KPI 卡 */
.kpi-row { display: flex; gap: 14px; flex-wrap: wrap; margin: 4px 0 12px; }
.metric-card { flex: 1; min-width: 180px; background: var(--bg-card); border: 1px solid var(--border); border-radius: var(--radius-xl); padding: 16px 18px; box-shadow: var(--shadow-sm); position: relative; overflow: hidden; }
.metric-card::before { content: ""; position: absolute; top: 0; left: 0; right: 0; height: 3px; background: var(--brand-ink); }
.mc-label { font-size: 12px; color: var(--text-muted); text-transform: uppercase; letter-spacing: .05em; }
.mc-val { font-size: 26px; font-weight: 780; color: var(--primary); margin-top: 6px; }
.mc-delta { margin-top: 6px; font-size: 13px; }
.mc-delta.up { color: var(--error-text); }
.mc-delta.down { color: var(--success-text); }
.mc-delta .mc-vs { color: var(--text-faint); }
/* 实体卡 */
.entity { border: 1px solid var(--border); border-radius: var(--radius-xl); margin: 6px 0 12px; overflow: hidden; }
.entity-hd { padding: 9px 14px; font-weight: 650; font-size: 13px; color: var(--text-primary); background: var(--bg-main); border-bottom: 1px solid var(--divider); }
.entity-grid { display: grid; grid-template-columns: 1fr 1fr; }
.entity-cell { padding: 8px 14px; border-bottom: 1px solid var(--divider); border-right: 1px solid var(--divider); }
.ec-k { font-size: 11px; color: var(--text-muted); }
.ec-v { font-size: 13px; color: var(--text-regular); margin-top: 2px; word-break: break-all; }
/* 图表卡 */
.chart-card { border: 1px solid var(--border); border-radius: var(--radius-xl); padding: 12px; margin: 8px 0 12px; background: var(--bg-card); }
/* 表格 double-bezel */
.tbl-wrap { margin: 12px 0; padding: 6px; background: var(--bg-sunken); border: 1px solid var(--border); border-radius: var(--radius-xl); box-shadow: var(--shadow-sm); overflow-x: auto; }
.tbl-wrap table { background: var(--bg-card); border-radius: calc(var(--radius-xl) - 6px); overflow: hidden; border-collapse: collapse; font-size: 12.5px; width: max-content; min-width: 100%; }
.tbl-wrap th, .tbl-wrap td { padding: 9px 14px; text-align: left; white-space: nowrap; border-bottom: 1px solid var(--divider); }
.tbl-wrap th { background: var(--bg-main); font-weight: 650; color: var(--text-regular); font-size: 11.5px; letter-spacing: .05em; position: sticky; top: 0; border-bottom: 2px solid var(--primary-bg); }
.tbl-wrap th.num, .tbl-wrap td.num { text-align: right; font-variant-numeric: tabular-nums; }
.tbl-wrap tbody tr:nth-child(even) td { background: var(--bg-main); }
.tbl-wrap tbody tr:hover td { background: var(--primary-light); }
.tbl-wrap tbody tr:hover td:first-child { box-shadow: inset 3px 0 0 0 var(--primary); }
.tbl-wrap tbody tr:last-child td { border-bottom: none; }
/* 下钻 chips */
.drill { margin-top: 8px; display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
.drill-t { font-size: 12px; color: var(--text-muted); }
.pill { font-family: inherit; font-size: 12px; padding: 4px 12px; border: 1px solid var(--border); border-radius: var(--radius-full); background: var(--bg-card); color: var(--text-muted); cursor: pointer; white-space: nowrap; transition: .12s; }
.pill:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-light); }
/* 【意图澄清】clarify_options chip 排：与 .drill 同形态（上边框分隔的操作行） */
.clarify-opts { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-top: 14px; padding-top: 12px; border-top: 1px solid var(--divider); font-size: 12px; }
.clarify-t { color: var(--text-muted); }
/* 快捷 pill */
.quick { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; padding: 8px 16px; border-top: 1px solid var(--divider); background: var(--bg-card); }
/* 能力切换 chip（知识库/问数/自动） */
.pill.cap.on { border-color: var(--primary); background: var(--primary-bg); color: var(--primary); font-weight: 600; }
.kb-scope {
  max-width: 220px; overflow: hidden; padding: 3px 9px; border-left: 1px solid var(--divider);
  color: var(--text-muted); font-size: 11px; text-overflow: ellipsis; white-space: nowrap;
}
.cap-sp { width: 1px; height: 18px; background: var(--divider); margin: 0 6px; }
/* 输入栏 */
/* 输入区外壳：【引用上轮】chip 区 + 输入栏 +【排队追问】队列共用一条上边框 */
.composer { border-top: 1px solid var(--divider); background: var(--bg-card); }
.refbar { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; padding: 10px 16px 0; }
.queuebar { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; padding: 0 16px 10px; }
.ref-tag, .queue-tag { font-size: 11px; color: var(--text-faint); flex-shrink: 0; }
/* 【Y5 插话】运行中修正指令条：与队列条同一外壳、同一节奏 */
.steerbar { display: flex; align-items: center; gap: 8px; padding: 0 16px 10px; }
.steer-input { flex: 1; min-width: 0; padding: 7px 12px; border: 1px dashed var(--border); border-radius: var(--radius-md); background: var(--bg-card); color: var(--text-regular); font-family: inherit; font-size: 12px; }
.steer-input:focus { border-color: var(--primary); outline: none; }
.steer-btn { flex: 0 0 auto; padding: 7px 14px; border: 1px solid var(--primary); border-radius: var(--radius-md); background: transparent; color: var(--primary); font-size: 12px; font-weight: 600; cursor: pointer; }
.steer-btn:disabled { opacity: .55; cursor: not-allowed; }
.steer-tag { font-size: 11px; color: var(--primary); flex-shrink: 0; }
.ref-chip, .q-chip { display: inline-flex; align-items: center; gap: 4px; max-width: 320px; min-width: 0; font-size: 12px; border: 1px solid var(--border); border-radius: var(--radius-full); padding: 3px 10px; }
.ref-chip { color: var(--primary); background: var(--primary-bg); }
.q-chip { color: var(--text-regular); background: var(--bg-main); }
.chip-text { font-style: normal; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.chip-del { border: 0; padding: 0 1px; background: transparent; color: var(--text-faint); cursor: pointer; font-size: 14px; line-height: 1; flex-shrink: 0; }
.chip-del:hover { color: var(--error-text); }
.inputbar { display: flex; gap: 8px; align-items: flex-end; padding: 12px 16px; }
.inputbar textarea { flex: 1; min-height: 42px; max-height: 160px; resize: none; padding: 10px 14px; border: 1px solid var(--border); border-radius: var(--radius-md); background: var(--bg-card); color: var(--text-regular); font-family: inherit; font-size: 14px; line-height: 1.55; }
.inputbar textarea:focus { border-color: var(--primary); box-shadow: var(--ring); outline: none; }
.send { flex: 0 0 auto; height: 42px; padding: 0 22px; background: var(--primary); color: var(--on-primary); border: 1px solid var(--primary); border-radius: var(--radius-md); font-size: 14px; font-weight: 600; cursor: pointer; }
.send:disabled { opacity: .55; cursor: not-allowed; }
.toast { position: fixed; right: 18px; bottom: 18px; z-index: 1300; background: var(--text-primary); color: var(--bg-card); font-size: 13px; padding: 9px 14px; border-radius: 9px; box-shadow: var(--shadow-md, 0 4px 16px rgba(0,0,0,.18)); animation: toastIn .18s ease-out; }
@keyframes toastIn { from { transform: translateY(8px); opacity: 0; } to { transform: none; opacity: 1; } }
.weekly-mask { position: fixed; inset: 0; z-index: 1100; display: grid; place-items: center; padding: 20px; background: rgba(17, 24, 39, .38); backdrop-filter: blur(5px); }
.weekly-dialog { width: min(480px, 100%); overflow: hidden; border: 1px solid var(--border); border-radius: 8px; background: var(--bg-card); box-shadow: 0 24px 70px rgba(17, 24, 39, .2); }
.weekly-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 20px; padding: 22px 24px 14px; }
.weekly-kicker { display: block; margin-bottom: 5px; color: var(--primary); font-size: 11px; font-weight: 750; }
.weekly-head h2 { margin: 0; color: var(--text-primary); font-size: 19px; font-weight: 700; letter-spacing: 0; }
.weekly-close { width: 30px; height: 30px; border: 0; border-radius: 5px; background: transparent; color: var(--text-muted); cursor: pointer; }
.weekly-close:hover { background: var(--bg-hover); color: var(--text-primary); }
.weekly-intro { margin: 0; padding: 0 24px 18px; border-bottom: 1px solid var(--divider); color: var(--text-muted); font-size: 12.5px; line-height: 1.65; }
.weekly-field { display: flex; flex-direction: column; gap: 7px; padding: 20px 24px 0; }
.weekly-field span, .weekly-period > span { color: var(--text-regular); font-size: 12px; font-weight: 650; }
.weekly-field input { width: 100%; height: 42px; box-sizing: border-box; border: 1px solid var(--border); border-radius: 6px; padding: 0 12px; background: var(--bg-card); color: var(--text-primary); font-family: inherit; font-size: 14px; }
.weekly-field input:focus { outline: none; border-color: var(--primary); box-shadow: var(--ring); }
.weekly-period { display: grid; grid-template-columns: auto 1fr; align-items: baseline; gap: 5px 14px; margin: 14px 24px 0; padding: 12px 14px; border-left: 3px solid var(--primary); border-radius: 5px; background: var(--bg-main); }
.weekly-period b { color: var(--text-primary); font-size: 13px; font-variant-numeric: tabular-nums; }
.weekly-period small { grid-column: 2; color: var(--text-muted); font-size: 11px; }
.weekly-error { margin: 12px 24px 0; padding: 8px 10px; border-radius: 5px; background: var(--error-bg); color: var(--error-text); font-size: 12px; }
.weekly-actions { display: flex; justify-content: flex-end; gap: 8px; padding: 20px 24px 22px; }
.weekly-cancel, .weekly-submit { height: 36px; padding: 0 17px; border-radius: 6px; font-size: 13px; font-weight: 650; cursor: pointer; }
.weekly-cancel { border: 1px solid var(--border); background: var(--bg-card); color: var(--text-regular); }
.weekly-submit { border: 1px solid var(--primary); background: var(--primary); color: var(--on-primary); }
.weekly-cancel:hover { border-color: var(--primary); color: var(--primary); }
.weekly-submit:disabled { opacity: .5; cursor: not-allowed; }
/* 【思维过程】Codex 风格：已完成步骤收敛，当前步骤高亮 */
.think-steps { min-width: 0; display: flex; flex-direction: column; gap: 3px; padding-left: 24px; }
.think-step { display: grid; grid-template-columns: 13px minmax(0, 1fr); gap: 6px; font-size: 11.5px; color: var(--text-faint); line-height: 1.3; }
.think-step .ts-ok { color: var(--primary); font-weight: 700; text-align: center; }
.think-step.current { color: var(--text-primary); }
.think-step.current .ts-ok { animation: thinkPulse 1.1s ease-in-out infinite; }
@keyframes thinkPulse { 50% { opacity: .35; transform: translateX(2px); } }
/* 登录页铺品牌图。主体（虎 + 字标）在图的**左半边**，所以卡片靠右放 ——
   居中会正好压住脸。窄屏没有右半边可站，退回居中并压一层暗罩保对比度。 */
.login-mask { position: fixed; inset: 0; z-index: 1000; display: grid; place-items: center end; padding: 20px clamp(20px, 7vw, 110px);
  background: url('/login-bg.jpg') center left / cover no-repeat, var(--bg-body); }
/* 图是品牌资产，不做模糊/去色：只在卡片一侧压一层同色渐变，让白卡不糊在亮黄上 */
.login-mask::before { content: ''; position: absolute; inset: 0; pointer-events: none;
  background: linear-gradient(90deg, transparent 40%, rgba(120, 78, 0, .22) 100%); }
.login-box { position: relative; width: min(400px, 100%); background: var(--bg-card); border: 1px solid var(--border); border-radius: 12px; box-shadow: 0 24px 70px rgba(60, 40, 0, .28); padding: 34px 36px 36px; }
.login-brand { display: flex; align-items: center; gap: 9px; color: var(--primary); font-size: 15px; font-weight: 700; margin-bottom: 18px; }
.login-brand img { border-radius: 8px; }
/* 品牌标：三处共用一套（侧栏 / 顶栏 / 欢迎语）。图本身是圆形主体，只补一点圆角防锯齿。 */
.side-hd .logo img, .topbar .brand-mark, .bubble.ai .hello-mark { border-radius: 50%; vertical-align: middle; }
.side-hd .logo { display: flex; align-items: center; gap: 7px; }
.topbar .brand-mark { margin-right: 8px; align-self: center; }
/* 欢迎语里的头像浮在左上，正文绕排 —— 不改气泡的既有排版 */
.bubble.ai .hello-mark { float: left; margin: 1px 10px 4px 0; }
@media (max-width: 900px) {
  .login-mask { place-items: center; padding: 20px; background-position: center center; }
  .login-mask::before { background: rgba(20, 14, 0, .34); }
}
.login-box h1 { margin: 0 0 8px; color: var(--text-primary); font-size: 24px; letter-spacing: 0; }
.login-box > p { margin: 0 0 26px; color: var(--text-muted); font-size: 13px; line-height: 1.6; }
.login-box label { display: flex; flex-direction: column; gap: 7px; margin-bottom: 16px; }
.login-box label span, .login-role-title { color: var(--text-regular); font-size: 13px; font-weight: 600; }
.login-box input { height: 42px; border: 1px solid var(--border); border-radius: 6px; padding: 0 12px; background: var(--bg-card); color: var(--text-primary); font-size: 14px; }
.login-box input:focus { outline: none; border-color: var(--primary); box-shadow: var(--ring); }
.login-submit { width: 100%; height: 42px; margin-top: 6px; border: 0; border-radius: 6px; background: var(--primary); color: white; font-weight: 650; cursor: pointer; }
.login-submit:disabled { opacity: .6; cursor: wait; }
.login-error { color: var(--error-text); background: var(--error-bg); border-radius: 6px; padding: 9px 11px; margin: -4px 0 12px; font-size: 12px; }
.login-role-title { margin-bottom: 10px; }
.login-role { width: 100%; min-height: 42px; display: flex; justify-content: space-between; align-items: center; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card); color: var(--text-primary); padding: 0 13px; margin-top: 8px; cursor: pointer; }
.login-role:hover { border-color: var(--primary); color: var(--primary); }
/* 【深度页内嵌】聊天框里的 BI 段 */
.df-grid { display: grid; grid-template-columns: repeat(auto-fit,minmax(190px,1fr)); gap: 8px; margin-bottom: 14px; }
.df-card { min-width: 0; border: 1px solid var(--border); border-radius: 6px; padding: 10px 12px; background: var(--bg-card); }
.df-card span { display: block; margin-bottom: 4px; color: var(--text-muted); font-size: 10.5px; }
.df-card b { display: block; color: var(--text-primary); font-size: 13px; line-height: 1.45; overflow-wrap: anywhere; font-weight: 650; }
.dkpi { display: grid; grid-template-columns: minmax(230px, .8fr) minmax(0, 1.7fr); width: 100%; background: var(--bg-card); border: 1px solid var(--border); border-top: 3px solid var(--primary); border-radius: 7px; margin-bottom: 14px; box-shadow: var(--shadow-sm); overflow: hidden; }
.dk-main { display: flex; flex-direction: column; justify-content: center; gap: 5px; min-width: 0; padding: 16px 18px; }
.dkpi .dk-l { font-size: 12px; color: var(--text-muted); }
.dkpi .dk-v { font-size: 26px; font-weight: 720; color: var(--text-primary); font-variant-numeric: tabular-nums; }
.dk-main small { color: var(--text-faint); font-size: 10.5px; }
.dk-comparisons { display: grid; grid-template-columns: repeat(auto-fit, minmax(190px, 1fr)); border-left: 1px solid var(--divider); background: var(--bg-main); }
.dk-compare { display: grid; grid-template-columns: 1fr auto; align-content: center; gap: 3px 10px; min-width: 0; padding: 12px 16px; border-right: 1px solid var(--divider); }
.dk-compare:last-child { border-right: 0; }
.dk-compare span { color: var(--text-muted); font-size: 11px; }
.dk-compare b { font-size: 18px; color: var(--text-primary); font-variant-numeric: tabular-nums; }
.dk-compare.up b { color: var(--error-text); }
.dk-compare.down b { color: var(--success-text); }
.dk-compare small { grid-column: 1 / -1; color: var(--text-faint); font-size: 10.5px; }
.dk-compare-detail { grid-column: 1 / -1; display: flex; flex-wrap: wrap; gap: 4px 12px; padding-top: 5px; border-top: 1px solid var(--divider); }
.dk-compare-detail span { font-size: 10.5px; font-variant-numeric: tabular-nums; }
.dh-grid { display: grid; grid-template-columns: repeat(3,minmax(0,1fr)); gap: 10px; margin-bottom: 14px; }
.dh-card { min-width: 0; border: 1px solid var(--border); border-radius: 7px; padding: 11px 13px; background: var(--bg-main); display: flex; flex-direction: column; gap: 3px; }
.dh-card span { font-size: 11px; color: var(--text-muted); }
.dh-card b { font-size: 17px; color: var(--text-primary); font-variant-numeric: tabular-nums; }
.dh-card small { font-size: 11px; color: var(--text-faint); line-height: 1.45; }
.deep-page-head { display: flex; align-items: flex-end; justify-content: space-between; gap: 20px; margin: 4px 0 12px; padding: 0 2px 10px; border-bottom: 1px solid var(--divider); }
.deep-page-title { min-width: 0; display: flex; align-items: baseline; gap: 10px; }
.deep-page-title span { flex: 0 0 auto; color: var(--primary); font-size: 10.5px; font-weight: 750; }
.deep-page-title b { min-width: 0; overflow: hidden; color: var(--text-primary); font-size: 17px; font-weight: 700; text-overflow: ellipsis; white-space: nowrap; }
.deep-page-meta { flex: 0 0 auto; display: flex; gap: 12px; color: var(--text-muted); font-size: 10.5px; font-variant-numeric: tabular-nums; }
.deep-objective { display: grid; grid-template-columns: 72px minmax(0, 1fr); gap: 12px; align-items: start; margin-bottom: 14px; padding: 11px 14px; border-left: 3px solid var(--primary); border-radius: 5px; background: var(--bg-main); }
.deep-objective span { color: var(--primary); font-size: 11px; font-weight: 700; }
.deep-objective p { margin: 0; color: var(--text-regular); font-size: 12.5px; line-height: 1.65; }
/* 【D8】验收断言透出区（小字区：自评徽标 + 板块名 + 验收陈述） */
.daccept { display: grid; gap: 6px; margin: -6px 0 14px; padding: 10px 14px; border: 1px dashed var(--border); border-radius: var(--radius-md); background: var(--bg-card); }
.daccept-t { color: var(--text-muted); font-size: 10.5px; font-weight: 700; letter-spacing: 1px; }
.daccept-item { display: flex; align-items: baseline; gap: 8px; min-width: 0; }
.daccept-v { flex-shrink: 0; padding: 1px 7px; border-radius: var(--radius-full); font-size: 10px; font-style: normal; font-weight: 700; }
.daccept-v.met { color: var(--success-text); background: var(--bg-sunken); }
.daccept-v.partial { color: var(--warning-text); background: var(--bg-sunken); }
.daccept-v.unmet { color: var(--error-text); background: var(--bg-sunken); }
.daccept-v.pending { color: var(--text-faint); background: var(--bg-sunken); }
.daccept-sec { flex-shrink: 0; color: var(--text-muted); font-size: 10.5px; font-weight: 600; }
.daccept-text { min-width: 0; color: var(--text-regular); font-size: 11px; line-height: 1.5; overflow-wrap: anywhere; }
.dsec { border: 1px solid var(--border); border-radius: 7px; padding: 16px 18px; margin-bottom: 14px; background: var(--bg-card); box-shadow: var(--shadow-sm); }
.dsec.table-sec { padding-bottom: 12px; border-top: 3px solid var(--primary); }
.dsec-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 18px; margin-bottom: 8px; }
.dsec-copy { min-width: 0; }
.dsec-t { font-size: 15px; font-weight: 680; color: var(--text-primary); margin-bottom: 2px; }
.dsec-q { font-size: 11.5px; color: var(--text-muted); line-height: 1.55; }
.dsec-tools { flex: 0 0 auto; display: flex; align-items: center; gap: 6px; }
.dsec-stat { color: var(--text-faint); font-size: 10.5px; font-variant-numeric: tabular-nums; white-space: nowrap; }
.dsec-seg { height: 28px; display: inline-flex; border: 1px solid var(--border); border-radius: 5px; overflow: hidden; }
.dsec-seg button { min-width: 46px; border: 0; border-right: 1px solid var(--border); background: var(--bg-card); color: var(--text-muted); font-size: 11.5px; cursor: pointer; }
.dsec-seg button:last-child { border-right: 0; }
.dsec-seg button.on { background: var(--primary); color: var(--on-primary); }
.dsec-icon { width: 28px; height: 28px; border: 1px solid var(--border); border-radius: 5px; background: var(--bg-card); color: var(--text-muted); cursor: pointer; font-size: 14px; }
.dsec-icon:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-bg); }
.dtable-wrap { max-height: 460px; overflow: auto; border: 1px solid var(--divider); border-radius: 5px; background: var(--bg-card); }
.dtable { border-collapse: separate; border-spacing: 0; width: 100%; min-width: 680px; font-size: 12.5px; }
.dtable th, .dtable td { border-bottom: 1px solid var(--divider); padding: 9px 12px; text-align: left; white-space: nowrap; }
.dtable th { position: sticky; top: 0; z-index: 2; background: var(--bg-main); color: var(--text-regular); font-size: 11.5px; font-weight: 680; }
.dtable th.num, .dtable td.num { text-align: right; font-variant-numeric: tabular-nums; }
.dtable th:first-child { left: 0; z-index: 3; }
.dtable td:first-child { position: sticky; left: 0; z-index: 1; background: var(--bg-card); font-weight: 600; }
.dtable tr:nth-child(even) td { background: var(--bg-main); }
.dtable tr:last-child td { border-bottom: 0; }
.dtable tr:hover td { background: var(--primary-light); }
/* 首列 sticky（见上）：半透明底会让横滚时压在下面的单元格文字透出来，与 ResultPanel 同解 */
.dtable tr:hover td:first-child { background: color-mix(in srgb, var(--primary) 8%, var(--bg-card)); }
.dmore { padding-top: 7px; color: var(--text-faint); font-size: 10.5px; text-align: right; }
.contribution-sec { border-left: 3px solid var(--primary); }
.bi-focus { position: fixed; inset: 0; z-index: 1200; display: grid; place-items: center; padding: 28px; background: rgba(17, 24, 39, .42); backdrop-filter: blur(5px); }
.bi-focus-card { width: min(1440px, 96vw); height: min(840px, 92vh); display: flex; flex-direction: column; min-height: 0; background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; box-shadow: 0 24px 80px rgba(17,24,39,.28); overflow: hidden; }
.bi-focus-hd { display: flex; align-items: center; justify-content: space-between; gap: 18px; padding: 14px 18px; border-bottom: 1px solid var(--divider); background: var(--bg-main); }
.bi-focus-hd > div:first-child { min-width: 0; display: flex; flex-direction: column; gap: 3px; }
.bi-focus-hd b { color: var(--text-primary); font-size: 16px; }
.bi-focus-hd small { color: var(--text-muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.bi-focus-body { flex: 1; min-height: 0; overflow: auto; padding: 22px 26px; }
.bi-focus-table { max-height: none; height: 100%; }
/* 【深度模式】精简|深度 segmented */
.mode-seg { display: flex; border: 1px solid var(--border); border-radius: var(--radius-md); overflow: hidden; flex: 0 0 auto; height: 42px; }
.mode-seg button { display: flex; align-items: center; padding: 0 12px; border: 0; font: inherit; font-size: 13px; color: var(--text-muted); cursor: pointer; background: var(--bg-card); }
.mode-seg button.on { background: var(--primary); color: var(--on-primary); font-weight: 600; }
.mode-seg.disabled { opacity: .48; }
.mode-seg.disabled button { cursor: not-allowed; }
/* 按钮 */
.btn-icon, .btn-sm { border: 1px solid var(--border); background: var(--bg-card); color: var(--text-regular); border-radius: var(--radius); cursor: pointer; font-size: 12px; }
.btn-icon { width: 30px; height: 30px; padding: 0; font-size: 15px; }
.btn-sm { height: 26px; padding: 0 10px; }
.mobile-kb { display: none; }
.mobile-weekly { display: none; }
.mobile-menu { display: none; }
.side-mask { display: none; }
.btn-icon:hover, .btn-sm:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-light); }
@media (max-width: 520px) { .weekly-mask { padding: 10px; align-items: end; } .weekly-dialog { border-radius: 8px 8px 0 0; } .weekly-head, .weekly-intro, .weekly-field, .weekly-actions { padding-inline: 18px; } .weekly-period { margin-inline: 18px; grid-template-columns: 1fr; } .weekly-period small { grid-column: 1; } }
@media (max-width: 980px) { .dh-grid { grid-template-columns: 1fr; } .dkpi { grid-template-columns: 1fr; } .dk-comparisons { border-left: 0; border-top: 1px solid var(--divider); } }
/* ≤820px 预览面板是全屏 fixed 浮层，侧栏转为抽屉常驻（见上），本条只约束桌面档 */
@media (min-width: 821px) and (max-width: 1360px) { .wrap.has-preview .side { display: none; } }
/* 【S1】artifact 预览面板 */
.art-card { display: flex; align-items: center; gap: 8px; border: 1px solid var(--border); border-radius: var(--radius); padding: 8px 12px; margin-bottom: 10px; font-size: 13px; color: var(--text-regular); background: var(--primary-bg); }
/* 产物卡 = 外壳 div + 锚点 .art-link + 同级分享钮：锚点里不许再嵌 button（非法嵌套交互元素） */
.art-card .art-link { display: flex; align-items: center; gap: 8px; flex: 1; min-width: 0; color: inherit; text-decoration: none; cursor: pointer; }
.art-card:hover { border-color: var(--primary); }
.art-card .art-hint { margin-left: auto; font-size: 12px; color: var(--text-faint); }
.art-card .art-share { margin-left: 6px; padding: 0; border: 0; background: none; font-family: inherit; font-size: 14px; cursor: pointer; }
.pv { position: relative; flex: 0 0 46%; min-width: 340px; max-width: 75%; border-left: 1px solid var(--border); background: var(--bg-card); display: flex; flex-direction: column; min-height: 0; }
.pv-drag { position: absolute; left: -3px; top: 0; bottom: 0; width: 6px; cursor: col-resize; z-index: 2; }
.pv-drag:hover { background: var(--primary-light); }
.pv-hd { display: flex; align-items: center; gap: 8px; padding: 8px 12px; border-bottom: 1px solid var(--divider); }
.pv-title { flex: 1; font-size: 13px; font-weight: 600; color: var(--text-primary); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.pv-act { padding: 0; font-family: inherit; font-size: 12px; color: var(--primary); cursor: pointer; border: none; background: none; text-decoration: none; white-space: nowrap; }
.pv-act:hover { text-decoration: underline; }
/* 【D6】版本/引用浮层：挂在预览面板头部下方，点条目即动作（回看版本 / 引用到会话） */
.pv-pop { position: absolute; top: 38px; right: 10px; z-index: 30; min-width: 200px; max-width: 320px; max-height: 60vh; overflow-y: auto; background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; box-shadow: 0 10px 30px rgba(31, 45, 77, .16); padding: 6px; }
.pv-pop-item { display: flex; justify-content: space-between; align-items: baseline; gap: 10px; width: 100%; border: 0; background: none; font: inherit; text-align: left; padding: 7px 10px; font-size: 12.5px; color: var(--text-primary); border-radius: 6px; cursor: pointer; text-decoration: none; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.pv-pop-item small { color: var(--text-muted); font-size: 11px; flex-shrink: 0; }
.pv-pop-item:hover { background: var(--bg-hover, rgba(53, 103, 214, .08)); }
.pv-pop-item.on { color: var(--primary); font-weight: 650; }
.promote-note { margin: 8px 0 0; font-size: 12.5px; color: var(--text-muted); }
.pv-x { font-size: 14px; }
.pv-frame { flex: 1; border: none; min-height: 0; background: #fff; }
.pv-state { flex: 1; display: flex; align-items: center; justify-content: center; gap: 9px; color: var(--text-muted); font-size: 13px; background: var(--bg-main); }
.pv-error { color: var(--error-text); padding: 24px; text-align: center; line-height: 1.7; }
/* 移动端侧栏：整栏 display:none 会同时丢掉会话列表/新建/主题入口 —— 改为 ☰ 拉开的抽屉 */
@media (max-width: 820px) {
  .mobile-menu { display: inline-flex; }
  .side { position: fixed; top: 0; left: 0; bottom: 0; z-index: 1150; width: min(300px, 86vw); transform: translateX(-105%); transition: transform .18s ease-out; }
  .side.open { transform: none; box-shadow: 18px 0 50px rgba(17, 24, 39, .18); }
  .side-mask { display: block; position: fixed; inset: 0; z-index: 1140; background: rgba(17, 24, 39, .38); backdrop-filter: blur(5px); }
  .mobile-kb, .mobile-weekly { display: inline-flex; align-items: center; }
  .bubble { max-width: 94%; }
  .bubble.ai.result-bubble { width: 100%; max-width: 100%; }
  .pv { position: fixed; inset: 0; z-index: 1200; display: flex; width: 100%; min-width: 0; max-width: none; flex-basis: auto !important; border-left: 0; }
  .pv-drag { display: none; }
  .pv-hd { padding: 9px 10px; overflow-x: auto; }
  .pv-title { min-width: 120px; }
  .dsec-head { flex-direction: column; }
  .dsec-tools { width: 100%; flex-wrap: wrap; }
  .deep-page-head { align-items: flex-start; flex-direction: column; gap: 5px; }
  .deep-page-meta { flex-wrap: wrap; }
  .deep-objective { grid-template-columns: 1fr; gap: 4px; }
.deep-gap { border-left-color: var(--warning-text); background: var(--warning-bg); }
  .bi-focus { padding: 8px; }
  .bi-focus-card { width: 100%; height: 96vh; }
  .bi-focus-body { padding: 12px; }
}

</style>
