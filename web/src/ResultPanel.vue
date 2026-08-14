<script setup lang="ts">
import { computed, defineAsyncComponent, h, ref } from 'vue'
import { fmt, isGrossMarginLabel, semanticForLabel, toNum, type Semantic } from './format'
import { intentIssueText, type IntentSummary } from './result-receipt'
import { buildInsightCards, sanitizeInsight } from './insight-cards'

// 弱网 / chunk 加载失败时图表区不能长期空白无反馈：loading 占位 + error 兜底
const BiChart = defineAsyncComponent({
  loader: () => import('./BiChart.vue'),
  loadingComponent: { render: () => h('div', { class: 'chart-state' }, '图表加载中…') },
  errorComponent: { render: () => h('div', { class: 'chart-state' }, '图表组件加载失败，请刷新重试') },
})

interface ColSpec { name: string; role: string; semantic: Semantic }
interface Delta {
  pct: number; dir: 'up' | 'down' | 'flat'; label: string
  baseline?: number; change?: number
}
interface Kpi { label: string; value: unknown; semantic: Semantic; delta?: Delta }
interface Block {
  type: 'kpis' | 'entity' | 'chart' | 'table'
  items?: Kpi[]; pairs?: [string, unknown][]
  kind?: 'bar' | 'line' | 'pie'; x?: number; y?: number[]; top?: number | null
  /** 多序列切分列下标（后端 `skip_serializing_if`，单序列时整键不上线 → 可选）。
   *  不往下传 = 「时间 + 1 类别 + 1 指标」继续被画成一条混轴折线。 */
  series?: number | null
}
interface ViewSpec { columns: ColSpec[]; blocks: Block[]; interact?: { drill?: string[] }; insight?: string }
interface SupplementalResult {
  columns: string[]; rows: unknown[][]; row_count: number
  truncated: boolean; view: ViewSpec
}
// view 可选：知识库回答是 {kind:'text', markdown, citations} —— 没有 view。
// 声明成必填时这里多处解引用会直接 TypeError 白屏（App.vue 只按 subs 是否为空分派）。
interface Result {
  columns: string[]; rows: unknown[][]; row_count: number; view?: ViewSpec
  route?: string
  sql?: string
  /** 独立补充结果：用于结构拆解和明细，不得覆盖主 KPI/标量。 */
  supplemental?: SupplementalResult
  /** 是否命中行上限（后端 `agent::gate::MAX_ROWS` 判的）。**前端不持有那个数字** ——
   *  否则就是第三处口径，而它必然漂（见 script 里「前端不再持有行数上限」那段注释）。 */
  truncated?: boolean
  /** 敏感列防线（`connector::redact`，F5）把命中的列**整列置 Null** 后回报的列名。
   *  不渲染它 = 用户把「已脱敏」读成「系统坏了 / 这列没数据」，
   *  裁决 二·D T4-5 记的就是这笔债（后端一直在算，前端没有消费者）。
   *  后端 `skip_serializing_if`（空数组不上线），故可选 —— 老服务端不带这个键也不崩。 */
  redacted?: string[]
  /** 口径复核未通过 / 命中行上限的两条标注。**子结果也带这两个字段**，
   *  而此前只有 `App.vue` 顶层在读、本接口里压根没声明它们 ——
   *  于是复合问每个子问的口径提醒与截断提醒一个字都看不见
   *  （容器自身这两项恒 `None`，见 `agent::ctx::AskResult::compound`）。 */
  caliber_note?: string
  truncation_note?: string
  /** 行级权限**生效了**的回显（后端 `skip_serializing_if`，故可选）。
   *  不渲染它 = 受限用户看到子集却以为是全量，拿着被过滤的数下结论 ——
   *  那件事不报错、也没有任何判据抓得到，属正确性而非产品面。
   *  【结果卡降噪】它是「判断/校验类信息」：渲染在底部「核查详情」折叠条里（默认收起），不占首屏。 */
  scope_note?: string
  /** 用户原问被补全/归一后，本轮实际采用的理解。只展示后端事实，不在前端猜口径。 */
  reinterpret_note?: string
  resolved_question?: string
  /** 同一份结构化意图合同的安全摘要；不含 SQL、prompt、内部实体 ID。 */
  intent_summary?: IntentSummary
  /** 可信核查凭证（`agent::ctx::attach_trust`）：级别/来源/权限边界/执行方式/指纹/checks 清单。
   *  全是判断/校验类信息 —— 裁决（2026-08-10 结果卡降噪）：收进「核查详情」折叠条，
   *  数据一项不丢，只是默认收起。后端 `skip_serializing_if`，老服务端不带这个键也不崩。 */
  trust?: {
    level: 'verified' | 'high' | 'review'; trace_id: string; source: string; route: string
    access: string; execution: string; fingerprint: string; checks: string[]
  }
  /** 销售单指标 KPI 的同窗补充（裁决：销售额/销量/毛利额等答案顺带成本/收入/毛利）。
   *  与主查询同一时间窗、同一权限闸门；后端 `skip_serializing_if` —— 补充查询失败/为空
   *  时整键不上线（本组件就不渲染），主回答一个字符不变。恒单行四值，列名＝合同中文别名。 */
  sales_context?: { columns: string[]; rows: unknown[][] }
}

const props = defineProps<{ result: Result }>()
const emit = defineEmits<{ (e: 'drill', dim: string): void; (e: 'pick', q: string): void }>()

const customIntent = ref('')
const customIntentComposing = ref(false)
const blocks = computed(() => props.result.view?.blocks ?? [])
const kpis = computed(() => blocks.value.flatMap((b) => b.type === 'kpis' ? (b.items ?? []) : []))
/** 畸形 chart block（缺 kind/x/y）直接不渲染：BiChart 拿到 undefined 只会画空白或当场报错。 */
const validChart = (b: Block) => b.type === 'chart' && b.x !== undefined && !!b.y?.length
const trendCharts = computed(() => blocks.value.filter((b) => validChart(b) && b.kind === 'line'))
// 构成图只认 bar/pie 白名单（`!== 'line'` 宽放会把 kind 缺失的畸形块当构成图渲出去）
const compositionCharts = computed(() => blocks.value.filter((b) => validChart(b) && (b.kind === 'bar' || b.kind === 'pie')))
const entityBlocks = computed(() => blocks.value.filter((b) => b.type === 'entity'))
const tableBlocks = computed(() => blocks.value.filter((b) => b.type === 'table'))
const supplemental = computed(() => props.result.supplemental)
const supplementalBlocks = computed(() => supplemental.value?.view.blocks ?? [])
const supplementalKpis = computed(() => supplementalBlocks.value.flatMap((b) => b.type === 'kpis' ? (b.items ?? []) : []))
const supplementalTrendCharts = computed(() => supplementalBlocks.value.filter((b) => validChart(b) && b.kind === 'line'))
const supplementalCompositionCharts = computed(() => supplementalBlocks.value.filter((b) => validChart(b) && (b.kind === 'bar' || b.kind === 'pie')))
const supplementalEntityBlocks = computed(() => supplementalBlocks.value.filter((b) => b.type === 'entity'))
const supplementalTableBlocks = computed(() => supplementalBlocks.value.filter((b) => b.type === 'table'))
const hasSupplemental = computed(() => {
  const detail = supplemental.value
  return !!detail && (
    supplementalKpis.value.length > 0 || supplementalTrendCharts.value.length > 0
    || supplementalCompositionCharts.value.length > 0 || supplementalEntityBlocks.value.length > 0
    || (supplementalTableBlocks.value.length > 0 && detail.rows.length > 0)
  )
})
const drillOptions = computed(() => props.result.view?.interact?.drill ?? [])

/** 同窗补充小卡：固定四格（成本/收入/毛利/毛利率），按列名定位 ——
 *  缺列/空行/空值那一格不显示；全缺 = 整条不渲染（后端失败降级语义的前端一半）。
 *  毛利率是 ratio 原值，×100 后走 fmt 的 percent（1 位小数，与 KPI 值同一条路径）；
 *  金额走 fmt 的 money（已是 2 位）。 */
const SALES_CONTEXT_CELLS: Array<{ column: string; label: string; percent?: boolean }> = [
  { column: '不含税成本', label: '成本' },
  { column: '不含税收入', label: '收入' },
  { column: '毛利额', label: '毛利' },
  { column: '毛利率', label: '毛利率', percent: true },
]
const salesContextItems = computed(() => {
  const ctx = props.result.sales_context
  const row = ctx?.rows?.[0]
  if (!ctx || !row) return []
  return SALES_CONTEXT_CELLS.flatMap((cell) => {
    const ci = ctx.columns.indexOf(cell.column)
    const raw = ci < 0 ? null : row[ci]
    if (raw === null || raw === undefined || raw === '') return []
    const n = toNum(raw)
    const text = cell.percent
      ? (n === null ? '—' : fmt(n * 100, 'percent'))
      : (fmt(raw, 'money') || '—')
    return [{ label: cell.label, title: cell.column, text }]
  })
})
/** 实体候选匹配的 SQL 占位前缀（后端 semantic 层约定）：判路由与显 SQL 过滤共用，两处不许各写一份。 */
const ENTITY_CANDIDATE_SQL = '实体候选匹配：'
const isEntityCandidate = computed(() =>
  props.result.route === 'entity-card'
  && (props.result.sql ?? '').startsWith(ENTITY_CANDIDATE_SQL)
  && drillOptions.value.length > 0,
)
const entityChoices = computed(() => isEntityCandidate.value
  ? drillOptions.value.map((query, index) => {
      const row = props.result.rows[index] ?? []
      return {
        query,
        kind: String(row[0] ?? '业务对象'),
        code: String(row[1] ?? ''),
        name: String(row[2] ?? query),
      }
    })
  : [],
)
const intentOptions = computed(() => drillOptions.value.filter((q) =>
  !/^(?:输入想法|自由输入|其他|其它)(?:[（(].*[）)])?$|^other$/i.test(q.trim()),
))

/** 单 KPI 结果（无图/无表/无补充）：宽松的大数字卡，不与表格结果挤同一套密度。 */
const soloKpi = computed(() => kpis.value.length === 1
  && !trendCharts.value.length && !compositionCharts.value.length
  && !entityBlocks.value.length && !tableBlocks.value.length && !hasSupplemental.value)

/** 宽表阈值：超过这个列数就出「左右滑动」提示（主表/补充表同一条，不许各写一份）。 */
const WIDE_TABLE_COLS = 3
const hasWideTable = computed(() => props.result.columns.length > WIDE_TABLE_COLS)
const supplementalHasWideTable = computed(() => (supplemental.value?.columns.length ?? 0) > WIDE_TABLE_COLS)
/** 窄屏表格提示（≤720px 才显示，见样式）：窄屏多是触屏没有悬停，文案说「点击」不说「悬停」。 */
const TABLE_SCROLL_HINT = '左右滑动可查看完整字段，点击单元格可查看完整内容'


const insightText = computed(() => sanitizeInsight(props.result.view?.insight ?? ''))
const displaySql = computed(() => {
  const sql = (props.result.sql ?? '').trim()
  return sql && !sql.startsWith(ENTITY_CANDIDATE_SQL) ? sql : ''
})

/** 反问/主题未接入两族 route：回答主体是引导文案（caliber_note），不是取数结果。 */
const isAskRoute = computed(() => props.result.route === 'need-intent' || props.result.route === 'no-topic')

/** 【结果卡降噪（裁决 2026-08-10）】判断/校验类信息默认折叠进「核查详情」：
 *  口径复核明细（caliber_note 原文）、权限注入回显（scope_note）、可信凭证（trust）。
 *  数据一项不丢，只是默认收起；首屏只留答案 + 必要提示（截断/推导口径/脱敏）。
 *  caliber-warn 那句一句话警告仍留首屏 —— 「数字不可信」这件事本身不能折叠。 */
const auditTrust = computed(() => props.result.trust)
const auditCaliberNote = computed(() => (isAskRoute.value ? '' : (props.result.caliber_note ?? '')))
const resolvedQuestion = computed(() => (props.result.resolved_question ?? '').trim())
const reinterpretNote = computed(() => (props.result.reinterpret_note ?? '').trim())
const intentSummary = computed(() => props.result.intent_summary)
const understandingText = computed(() => reinterpretNote.value || (resolvedQuestion.value
  ? `本轮实际按「${resolvedQuestion.value}」执行。`
  : ''))
const hasFoundation = computed(() => !!(
  auditCaliberNote.value || props.result.scope_note || auditTrust.value
  || reinterpretNote.value || resolvedQuestion.value || intentSummary.value
))
const INTENT_MODE_LABEL: Record<string, string> = {
  data: '问数', knowledge: '知识检索', hybrid: '数据 + 知识', unknown: '待确认',
}
const INTENT_SLOT_LABEL: Record<string, string> = {
  metric: '指标', entity: '对象', region: '地区', time: '时间', filter: '筛选',
  breakdown: '拆分', comparison: '比较', detail: '明细',
}
const intentStatusText = computed(() => {
  const summary = intentSummary.value
  if (!summary) return ''
  if (summary.coverage.status === 'complete') return `${INTENT_MODE_LABEL[summary.mode] ?? summary.mode}意图已完整覆盖`
  return summary.status === 'clarification' ? '需要补充问题限定' : '意图覆盖未通过'
})
const TRUST_LEVEL_LABEL: Record<string, string> = {
  verified: '已验证',
  high: '已校验',
  review: '需复核',
}
const TRUST_LEVEL_NOTE: Record<string, string> = {
  verified: '确定性业务路径',
  high: '模型查询已通过安全、权限与执行校验',
  review: '存在明确风险，使用前请核对',
}
/** 分桶/表头/截断判据都在 insight-cards.ts（有单测）：这里只负责渲染。 */
const insightCards = computed(() => (isEntityCandidate.value ? [] : buildInsightCards(insightText.value)))

const deltaNumber = new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 1 })

function entityValue(pair: [string, unknown]): string {
  return displayValue(pair[0], pair[1]) || '—'
}

function displayValue(label: string, value: unknown, semantic?: Semantic, metric = false): string {
  const number = toNum(value)
  if (isGrossMarginLabel(label) && number !== null) return fmt(number * 100, 'percent')
  const inferred = semanticForLabel(label)
  const resolved = semantic && semantic !== 'none' ? semantic : inferred !== 'none' ? inferred : metric ? 'count' : 'none'
  return fmt(value, resolved)
}

function formatDelta(value: number): string {
  return deltaNumber.format(Math.abs(value))
}

const ppNumber = new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 2 })
/** 值与 delta 共用同一条百分判据（semantic=percent，或标签判毛利率 —— 值路径在 displayValue 里 ×100）。
 *  两条判据分叉时会出现「值按百分显示、delta 按相对 %」的错配。 */
const isPercentKpi = (k: Kpi): boolean => k.semantic === 'percent' || isGrossMarginLabel(k.label)
/** KPI delta 文本：百分比指标的 delta 后端已按**百分点**出数（毛利率 19.30%→19.63% = +0.33 个百分点），
 *  其它指标是相对百分比。方向由左侧箭头表达，这里只出绝对值+单位。 */
function deltaText(k: Kpi): string {
  const d = k.delta
  if (!d || !Number.isFinite(d.pct)) return ''
  return isPercentKpi(k) ? `${ppNumber.format(Math.abs(d.pct))} 个百分点` : `${formatDelta(d.pct)}%`
}

function deltaDetail(kpi: Kpi): string {
  const delta = kpi.delta
  if (!delta || typeof delta.baseline !== 'number' || !Number.isFinite(delta.baseline)) return ''
  const semantic = kpi.semantic === 'none' ? semanticForLabel(kpi.label) : kpi.semantic
  const baseline = displayValue(kpi.label, delta.baseline, semantic, true)
  const change = typeof delta.change === 'number' && Number.isFinite(delta.change)
    ? `${delta.change > 0 ? '+' : delta.change < 0 ? '-' : ''}${displayValue(kpi.label, Math.abs(delta.change), semantic, true)}`
    : '—'
  return `基期 ${baseline} · 变化额 ${change}`
}

function chartTitle(block: Block, view = props.result.view): string {
  const x = (block.x === undefined ? undefined : view?.columns[block.x]?.name) ?? '维度'
  const metrics = (block.y ?? []).map((i) => view?.columns[i]?.name).filter(Boolean).join('、') || '指标'
  if (block.kind === 'line') return `${metrics}趋势`
  if (block.kind === 'pie') return `${x}构成`
  return `${metrics}按${x}对比`
}

function chartCaption(
  block: Block,
  view = props.result.view,
  rows: unknown[] = props.result.rows,
): string {
  const x = (block.x === undefined ? undefined : view?.columns[block.x]?.name) ?? '维度'
  const base =
    block.kind === 'line' ? `按${x}观察变化与拐点`
    : block.kind === 'pie' ? `各${x}占比与集中度`
    : `各${x}贡献与排名`
  // 🔴 TOP 收纳必须告知：200 个客户只画 10 根柱，而标题写「各客户贡献与排名」——
  // 用户会把这 10 个当成全部。把「图只画了一部分」这件事赌在用户自己去数表格行数上，
  // 是把正确性外包给用户（2026-08-13 视觉审计）。
  const top = block.top
  return top && rows.length > top ? `${base}（前 ${top} 项，共 ${rows.length} 项）` : base
}

function entityTitle(block: Block): string {
  const labels = (block.pairs ?? []).map((p) => p[0]).join(' ')
  if (/客户|经销商/i.test(labels)) return '客户档案'
  // storecode/storename 是门店字段的连写形态 —— 挂在客户分支会先命中，被误判成「客户档案」
  if (/门店|店铺|shop_|store_?name|store_?code/i.test(labels)) return '门店档案'
  if (/商品|品类|品牌|规格/.test(labels)) return '商品档案'
  if (/单号|订单|单据/.test(labels)) return '单据详情'
  return '基础信息'
}

function submitCustomIntent(event?: Event): void {
  if (customIntentComposing.value || (event instanceof KeyboardEvent && (event.isComposing || event.keyCode === 229))) return
  if (event instanceof KeyboardEvent) event.preventDefault()
  const q = customIntent.value.trim()
  if (!q) return
  emit('pick', q)
  customIntent.value = ''
}

function finishCustomIntentComposition(): void {
  // 部分中文输入法在 compositionend 后复用同一个 Enter 触发表单提交，延后一拍再解锁。
  setTimeout(() => { customIntentComposing.value = false }, 0)
}

/** 🔴 **前端不再持有行数上限**。
 *
 *  原来这里是 `slice(0, 100)`，而后端截断文案说「本次只返回前 200 行」、CSV 导出也是 200 ——
 *  三处口径不一致，界面上**没有一个字**说明下面还有 100 行，用户拿 100 行当全量下结论。
 *
 *  第一版的修法是 `const MAX_TABLE_ROWS: 200 = 200`，靠字面量类型「钉死口径」。
 *  那是个**假闸**：实测把它改成 `: 100 = 100` 或 `: number = 100`，`vue-tsc` 都 exit 0 ——
 *  唯一会红的是「注解写 200 而值写别的」，没人会那么改。真正的漂移源是后端
 *  `agent::gate::MAX_ROWS`，而那一行与它之间没有任何连接。
 *
 *  正解是**没有第二个数字**：模板直接渲染服务端给的全部行（`result.rows`，零透传中间层），
 *  行数与截断都用服务端已经在返的 `row_count` / `truncated` / `truncation_note` 表达。
 *  没有第三处口径，就没有第三处口径可漂。
 *  （200 行 DOM 不需要虚拟滚动：`overflow:auto` 的容器 + 200×N 个 `<td>` 是十毫秒级。） */
/** 行数脚注：恒显示。截断由服务端判（`truncated`），前端不猜 ——
 *  `row_count` 是**本次返回**的行数，不是表里的总行数；「后面还有」那句由后端的
 *  `truncation_note` 原文说（它才知道上限是多少、怎么续读，见模板 trunc-note 那条）。 */
const rowFoot = computed(() => rowFootFor(props.result))
const supplementalRowFoot = computed(() => supplemental.value ? rowFootFor(supplemental.value) : '')

// 主表/补充表同一句截断文案，不再按位置分粗细两版
function rowFootFor(result: Pick<Result, 'rows' | 'truncated'>): string {
  const base = `共 ${result.rows.length} 行`
  return result.truncated ? `${base} · 当前展示部分数据` : base
}

const redacted = computed(() => props.result.redacted ?? [])
const redactedSet = computed(() => new Set(redacted.value))

/** 列级预算：redacted/metric/semantic 每列算一次。
 *  模板热路径是 行×列 双循环，格级再各算一遍就是 200 行 × N 列 × 3 次的重复。 */
interface ColMeta { redacted: boolean; metric: boolean; semantic: Semantic }
function colMetaOf(result: Pick<Result, 'columns' | 'view'>, redactedNames: ReadonlySet<string>): ColMeta[] {
  return result.columns.map((name, ci) => ({
    redacted: redactedNames.has(name),
    metric: result.view?.columns[ci]?.role === 'metric',
    semantic: columnSemanticFor(result, ci),
  }))
}
const mainColMeta = computed(() => colMetaOf(props.result, redactedSet.value))
// 补充结果没有 redacted 字段（接口未声明），脱敏列不会出现 —— 传空集
const suppColMeta = computed(() => (supplemental.value ? colMetaOf(supplemental.value, new Set()) : []))

function isRedacted(ci: number): boolean {
  return mainColMeta.value[ci]?.redacted ?? false
}
function columnSemanticFor(result: Pick<Result, 'columns' | 'view'>, ci: number): Semantic {
  const spec = result.view?.columns[ci]
  if (spec?.semantic && spec.semantic !== 'none') return spec.semantic
  const inferred = semanticForLabel(result.columns[ci] ?? '')
  return inferred !== 'none' ? inferred : spec?.role === 'metric' ? 'count' : 'none'
}
// rows[ri] 可能是 null 行（畸形 JSON）：两段解引用都带 ?.，空值兜底 '—'
function cellFor(result: Pick<Result, 'columns' | 'rows'>, meta: ColMeta[], ri: number, ci: number): string {
  const value = displayValue(result.columns[ci] ?? '', result.rows[ri]?.[ci], meta[ci]?.semantic)
  return value || '—'
}
// 悬停全文复用格内文本的格式化结果（原来 cell/cellTitle 对同一值各算一遍 displayValue）
function cellTitleFor(result: Pick<Result, 'columns' | 'rows'>, meta: ColMeta[], ri: number, ci: number): string {
  const raw = result.rows[ri]?.[ci]
  if (raw === null || raw === undefined || raw === '') return '无数据'
  return cellFor(result, meta, ri, ci)
}
function cell(ri: number, ci: number): string {
  return cellFor(props.result, mainColMeta.value, ri, ci)
}
function cellTitle(ri: number, ci: number): string {
  return cellTitleFor(props.result, mainColMeta.value, ri, ci)
}
function isMetric(ci: number): boolean {
  return mainColMeta.value[ci]?.metric ?? false
}
function supplementalCell(ri: number, ci: number): string {
  return supplemental.value ? cellFor(supplemental.value, suppColMeta.value, ri, ci) : '—'
}
function supplementalCellTitle(ri: number, ci: number): string {
  return supplemental.value ? cellTitleFor(supplemental.value, suppColMeta.value, ri, ci) : '无数据'
}
function supplementalIsMetric(ci: number): boolean {
  return suppColMeta.value[ci]?.metric ?? false
}

/** KPI 卡视图模型：值/delta 文本/detail 每轮渲染算一次（原来模板里 v-if + 插值各调一遍），
 *  顺带统一空值兜底 '—' 与方向的无障碍文案（主区/补充区同一条产线）。 */
interface KpiCardView {
  label: string; value: string; dir?: 'up' | 'down' | 'flat'
  dirLabel?: string; delta: string; vs?: string; detail: string
}
function kpiCardOf(k: Kpi): KpiCardView {
  return {
    label: k.label,
    value: displayValue(k.label, k.value, k.semantic, true) || '—',
    dir: k.delta?.dir,
    // 中式约定涨红跌绿（颜色见 .mc-delta.up/down）；方向不能只靠颜色传达（色弱/读屏）
    dirLabel: k.delta ? (k.delta.dir === 'up' ? '上升' : k.delta.dir === 'down' ? '下降' : '持平') : undefined,
    delta: deltaText(k),
    vs: k.delta?.label,
    detail: deltaDetail(k),
  }
}
const kpiCards = computed(() => kpis.value.map(kpiCardOf))
const supplementalKpiCards = computed(() => supplementalKpis.value.map(kpiCardOf))
</script>

<template>
  <!-- 无 view 早退：本组件只渲染表格类结果；文本类（知识库）由 KbAnswer.vue 接 -->
  <div v-if="!result.view" class="empty-hint">该结果暂不支持表格化展示。</div>
  <div v-else class="result-panel">
    <!-- 口径 / 截断标注在**每个结果自己**这一层渲染（含复合的每个子问）：
         放在数字之前，否则「照返 + 标注」等于没标注。 -->
    <!-- 【S3】need-intent 选择卡（datanote AskUserTool 对应物）：反问是**澄清**不是报错，
         不用 error 红。no-topic（主题未接入）同形 —— 它也是引导回答，不是取数结果。
         选项 = 后端给的完整问法（view.interact.drill），点击原样发送 —
         挂起/续跑由会话追问机制天然承担（rewrite_followup + 日期继承），无需状态机。 -->
    <div v-if="isAskRoute" class="ask-card">
      <div class="ask-hd">{{ result.route === 'no-topic' ? '📭 这个主题还没接入数据' : '🤔 先问清再查' }}</div>
      <div v-if="result.caliber_note" class="ask-q">{{ result.caliber_note }}</div>
      <div v-if="intentOptions.length" class="ask-opts">
        <button v-for="d in intentOptions" :key="d" type="button" class="ask-opt" @click.stop="emit('pick', d)">{{ d }}</button>
      </div>
      <form class="ask-custom" @submit.stop.prevent="submitCustomIntent" @click.stop @keydown.stop>
        <input
          v-model="customIntent"
          class="ask-input"
          type="text"
          maxlength="200"
          autocomplete="off"
          aria-label="输入你的想法"
          placeholder="输入你的想法"
          @click.stop
          @compositionstart="customIntentComposing = true"
          @compositionend="finishCustomIntentComposition"
          @keydown.enter.stop="submitCustomIntent"
        >
        <button class="ask-submit" type="submit" :disabled="!customIntent.trim()">提交</button>
      </form>
      <div class="ask-hint">选择一个问法，或输入自己的问题</div>
    </div>
    <div v-else-if="isEntityCandidate" class="entity-choice-card">
      <div class="entity-choice-head">
        <div>
          <span class="section-kicker">精确匹配</span>
          <h3>请选择具体对象</h3>
        </div>
        <span>{{ entityChoices.length }} 个候选</span>
      </div>
      <div class="entity-choice-list">
        <button
          v-for="(choice, ci) in entityChoices"
          :key="`${choice.query}-${ci}`"
          type="button"
          class="entity-choice"
          @click.stop="emit('pick', choice.query)"
        >
          <span class="entity-choice-kind">{{ choice.kind }}</span>
          <span class="entity-choice-name">{{ choice.name }}</span>
          <span v-if="choice.code" class="entity-choice-code">{{ choice.code }}</span>
          <span class="entity-choice-action">查看详情 →</span>
        </button>
      </div>
      <p class="entity-choice-hint">选择后将按编码精确查询，系统不会自动猜测。</p>
    </div>
    <div v-else-if="result.caliber_note" class="caliber-warn" role="alert">当前结果未通过业务口径复核，请调整问法后重试。</div>
    <!-- direct-derive：合同未覆盖时的 ODS 推导降级。提示条放数字之前 ——
         与口径/截断标注同一纪律：先看见信任等级，再看数字。 -->
    <div v-if="result.route === 'direct-derive'" class="derive-note" role="note">
      <b>推导口径 · 未经合同验证</b>：以下结果由 ODS 明细推导，仅作排查参考；经营决策请使用已验证口径。
    </div>
    <!-- 截断三件套（原因/范围/续读参数）渲染后端原文 —— 硬编码文案会把续读参数吞掉 -->
    <div v-if="result.truncation_note" class="trunc-note" role="note">{{ result.truncation_note }}</div>
    <!-- 脱敏回显。单行实体卡会把 Null 列**整条丢掉**（`semantic::present` 的 pairs 过滤 Null），
         所以这条横幅同时是实体形态下唯一的说明处 —— 列名在这里列全。 -->
    <div v-if="redacted.length" class="redact-note" role="note">
      敏感列已按数据策略<b>整列脱敏</b>：{{ redacted.join('、') }}
    </div>

    <!-- need-intent/no-topic（反问与主题未接入）、entity-card（总览卡）、business-lookup（业务库点查，
         查不到时 caliber_note 已说明）都不是取数结果，不出「未找到数据」——
         实测（tp/b39c9a32）：反问气泡下叠这句，读的人以为「这个客户没数据」 -->
    <div v-if="result.row_count === 0 && !isAskRoute && result.route !== 'entity-card' && result.route !== 'business-lookup'" class="empty-hint" role="note">
      未找到数据。可能：① 该口径本期无记录；② 数据权限范围内无此数据；③ 换个说法试试
    </div>

    <!-- 在 AI 结论和业务数字之前给出依据：等级只来自后端 trust，不在 UI 伪造置信度。 -->
    <details
      v-if="hasFoundation && !isEntityCandidate"
      class="foundation"
      :open="auditTrust?.level === 'review' || intentSummary?.coverage.status === 'blocked'"
    >
      <summary>
        <span v-if="auditTrust" class="trust-badge" :class="auditTrust.level">
          <i aria-hidden="true"></i>{{ TRUST_LEVEL_LABEL[auditTrust.level] ?? auditTrust.level }}
        </span>
        <span v-else-if="intentSummary" class="trust-badge" :class="intentSummary.coverage.status === 'complete' ? 'verified' : 'review'">
          <i aria-hidden="true"></i>{{ intentSummary.coverage.status === 'complete' ? '已理解' : '待确认' }}
        </span>
        <span class="foundation-title">{{ intentSummary ? '问题理解与结果依据' : '结果依据' }}</span>
        <small v-if="auditTrust">{{ TRUST_LEVEL_NOTE[auditTrust.level] }}</small>
        <small v-else-if="intentStatusText">{{ intentStatusText }}</small>
        <b>查看理解与证据</b>
      </summary>
      <div class="foundation-body">
        <div v-if="understandingText" class="foundation-row">
          <span>本轮理解</span>
          <p>{{ understandingText }}</p>
        </div>
        <div v-if="intentSummary" class="foundation-row intent-row">
          <span>识别条件</span>
          <div class="intent-slots">
            <span class="intent-mode">{{ INTENT_MODE_LABEL[intentSummary.mode] ?? intentSummary.mode }}</span>
            <span v-for="(slot, si) in intentSummary.slots" :key="`${slot.kind}-${slot.surface}-${si}`" class="intent-slot">
              <i>{{ INTENT_SLOT_LABEL[slot.kind] ?? slot.kind }}</i>{{ slot.surface }}
            </span>
            <span v-if="!intentSummary.slots.length" class="intent-empty">尚未识别到可执行限定</span>
          </div>
        </div>
        <div v-if="intentSummary?.coverage.issues.length" class="foundation-row risk">
          <span>理解缺口</span>
          <ul class="foundation-checks"><li v-for="(issue, ii) in intentSummary.coverage.issues" :key="ii">{{ intentIssueText(issue) }}</li></ul>
        </div>
        <div v-if="auditTrust" class="foundation-facts" aria-label="结果来源与边界">
          <div><span>数据来源</span><b>{{ auditTrust.source }}</b></div>
          <div><span>执行方式</span><b>{{ auditTrust.execution }}</b></div>
          <div><span>权限范围</span><b>{{ auditTrust.access }}</b></div>
        </div>
        <div v-else-if="result.scope_note" class="foundation-row">
          <span>权限范围</span>
          <p>{{ result.scope_note }}</p>
        </div>
        <div v-if="auditCaliberNote" class="foundation-row risk">
          <span>口径风险</span>
          <p>{{ auditCaliberNote }}</p>
        </div>
        <div v-if="auditTrust?.checks.length" class="foundation-row">
          <span>已完成校验</span>
          <ul class="foundation-checks"><li v-for="(c, ci) in auditTrust.checks" :key="ci">{{ c }}</li></ul>
        </div>
        <div v-if="auditTrust" class="foundation-trace">
          <span>Trace {{ auditTrust.trace_id }}</span><span>计算指纹 {{ auditTrust.fingerprint }}</span>
        </div>
      </div>
    </details>

    <section v-if="insightCards.length" class="result-section insight-section">
      <div class="section-head">
        <div>
          <span class="section-kicker">AI</span>
          <h3>结论与建议</h3>
        </div>
        <span class="analysis-basis">基于本次查询结果</span>
      </div>
      <div class="insight-grid">
        <article v-for="card in insightCards" :key="card.kind" class="insight-card" :class="card.kind">
          <div class="insight-card-head"><span class="insight-dot"></span>{{ card.title }}</div>
          <ul>
            <li v-for="(item, ii) in card.items" :key="ii">{{ item }}</li>
          </ul>
        </article>
      </div>
    </section>

    <section v-if="kpis.length" class="result-section kpi-section">
      <div class="section-head">
        <div>
          <span class="section-kicker">关键结果</span>
          <h3>核心指标</h3>
        </div>
      </div>
      <div class="kpi-row" :class="{ solo: soloKpi }">
        <div v-for="(c, ki) in kpiCards" :key="ki" class="metric-card">
          <div class="mc-label">{{ c.label }}</div>
          <div class="mc-val num">{{ c.value }}</div>
          <div v-if="c.dir" class="mc-delta" :class="c.dir">
            <span class="delta-mark" :aria-label="c.dirLabel">{{ c.dir === 'up' ? '↑' : c.dir === 'down' ? '↓' : '—' }}</span>
            {{ c.delta }} <span class="mc-vs">{{ c.vs }}</span>
          </div>
          <div v-if="c.detail" class="mc-delta-detail">{{ c.detail }}</div>
        </div>
      </div>
    </section>

    <!-- 同窗补充（销售单指标 KPI 专属）：与主卡同时间窗的成本/收入/毛利/毛利率，
         样式对齐 KPI 卡但小一号；补充缺席时后端不上线这个键，这里整条不渲染。 -->
    <section v-if="salesContextItems.length" class="sales-context" aria-label="同窗成本与毛利">
      <div v-for="(item, ii) in salesContextItems" :key="`sc-${ii}`" class="sc-cell" :title="item.title">
        <span class="sc-label">{{ item.label }}</span>
        <span class="sc-val">{{ item.text }}</span>
      </div>
    </section>

    <section v-if="trendCharts.length" class="result-section">
      <div class="section-head">
        <div>
          <span class="section-kicker">时间变化</span>
          <h3>趋势变化</h3>
        </div>
      </div>
      <div class="chart-grid">
        <article v-for="(b, bi) in trendCharts" :key="`trend-${bi}`" class="chart-card">
          <header class="chart-head">
            <div>
              <h4>{{ chartTitle(b) }}</h4>
              <p>{{ chartCaption(b) }}</p>
            </div>
            <span class="chart-type">趋势</span>
          </header>
          <BiChart :kind="b.kind!" :columns="result.view.columns" :rows="result.rows" :x="b.x!" :y="b.y!" :top="b.top" :series="b.series" />
        </article>
      </div>
    </section>

    <section v-if="compositionCharts.length" class="result-section">
      <div class="section-head">
        <div>
          <span class="section-kicker">结构分布</span>
          <h3>构成与排名</h3>
        </div>
      </div>
      <div class="chart-grid" :class="{ paired: compositionCharts.length > 1 }">
        <article v-for="(b, bi) in compositionCharts" :key="`composition-${bi}`" class="chart-card">
          <header class="chart-head">
            <div>
              <h4>{{ chartTitle(b) }}</h4>
              <p>{{ chartCaption(b) }}</p>
            </div>
            <span class="chart-type">{{ b.kind === 'pie' ? '占比' : b.top != null ? '排名' : '对比' }}</span>
          </header>
          <BiChart :kind="b.kind!" :columns="result.view.columns" :rows="result.rows" :x="b.x!" :y="b.y!" :top="b.top" :series="b.series" />
        </article>
      </div>
    </section>

    <section v-if="entityBlocks.length" class="result-section">
      <div class="section-head">
        <div>
          <span class="section-kicker">业务对象</span>
          <h3>档案信息</h3>
        </div>
      </div>
      <div v-for="(b, bi) in entityBlocks" :key="`entity-${bi}`" class="entity">
        <div class="entity-hd">{{ entityTitle(b) }}</div>
        <div class="entity-grid">
          <div v-for="(p, pi) in b.pairs" :key="pi" class="entity-cell">
            <div class="ec-k">{{ p[0] }}</div>
            <div class="ec-v">{{ entityValue(p) }}</div>
          </div>
        </div>
      </div>
    </section>

    <section v-if="tableBlocks.length && result.rows.length > 0 && !isEntityCandidate" class="result-section table-section">
      <div class="section-head table-heading">
        <div>
          <span class="section-kicker">业务数据</span>
          <h3>业务明细</h3>
        </div>
        <span class="row-count">{{ rowFoot }}</span>
      </div>
      <div class="tbl-wrap">
        <table aria-label="业务明细数据表">
          <thead>
            <tr>
              <th class="row-index" scope="col">#</th>
              <th v-for="(c, ci) in result.columns" :key="ci" scope="col" :class="{ num: isMetric(ci) }">
                {{ c }}<span v-if="isRedacted(ci)" class="redact-lock" role="img" aria-label="本列已脱敏" title="敏感列：本列已整列脱敏">🔒</span>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(_, ri) in result.rows" :key="ri">
              <td class="row-index">{{ ri + 1 }}</td>
              <!-- 脱敏列逐格写「已脱敏」：一列空值会被读成故障 -->
              <td v-for="(_, ci) in result.columns" :key="ci"
                  :title="isRedacted(ci) ? '敏感列已脱敏' : cellTitle(ri, ci)"
                  :class="{ num: isMetric(ci) && !isRedacted(ci), 'redact-cell': isRedacted(ci) }">
                {{ isRedacted(ci) ? '已脱敏' : cell(ri, ci) }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div v-if="hasWideTable" class="table-scroll-hint">{{ TABLE_SCROLL_HINT }}</div>
    </section>

    <!-- 补充结果拥有独立 rows/view；直接在本组件内渲染，避免递归 ResultPanel 产生重复操作栏。 -->
    <section v-if="hasSupplemental && supplemental" class="supplemental-section" aria-label="结构与明细">
      <div class="supplemental-head">
        <div>
          <span class="section-kicker">补充数据</span>
          <h3>结构与明细</h3>
          <p>在主结果之外展开构成、变化与业务记录</p>
        </div>
      </div>

      <div v-if="supplementalKpiCards.length" class="kpi-row supplemental-kpis">
        <div v-for="(c, ki) in supplementalKpiCards" :key="`supplemental-kpi-${ki}`" class="metric-card">
          <div class="mc-label">{{ c.label }}</div>
          <div class="mc-val num">{{ c.value }}</div>
          <div v-if="c.dir" class="mc-delta" :class="c.dir">
            <span class="delta-mark" :aria-label="c.dirLabel">{{ c.dir === 'up' ? '↑' : c.dir === 'down' ? '↓' : '—' }}</span>
            {{ c.delta }} <span class="mc-vs">{{ c.vs }}</span>
          </div>
          <div v-if="c.detail" class="mc-delta-detail">{{ c.detail }}</div>
        </div>
      </div>

      <div v-if="supplementalTrendCharts.length" class="supplemental-group">
        <div class="supplemental-subhead"><h4>趋势变化</h4><span>观察时间变化与拐点</span></div>
        <div class="chart-grid">
          <article v-for="(b, bi) in supplementalTrendCharts" :key="`supplemental-trend-${bi}`" class="chart-card">
            <header class="chart-head">
              <div><h4>{{ chartTitle(b, supplemental.view) }}</h4><p>{{ chartCaption(b, supplemental.view, supplemental.rows) }}</p></div>
              <span class="chart-type">趋势</span>
            </header>
            <BiChart :kind="b.kind!" :columns="supplemental.view.columns" :rows="supplemental.rows" :x="b.x!" :y="b.y!" :top="b.top" :series="b.series" />
          </article>
        </div>
      </div>

      <div v-if="supplementalCompositionCharts.length" class="supplemental-group">
        <div class="supplemental-subhead"><h4>结构分布</h4><span>查看贡献、占比与排名</span></div>
        <div class="chart-grid" :class="{ paired: supplementalCompositionCharts.length > 1 }">
          <article v-for="(b, bi) in supplementalCompositionCharts" :key="`supplemental-composition-${bi}`" class="chart-card">
            <header class="chart-head">
              <div><h4>{{ chartTitle(b, supplemental.view) }}</h4><p>{{ chartCaption(b, supplemental.view, supplemental.rows) }}</p></div>
              <span class="chart-type">{{ b.kind === 'pie' ? '占比' : b.top != null ? '排名' : '对比' }}</span>
            </header>
            <BiChart :kind="b.kind!" :columns="supplemental.view.columns" :rows="supplemental.rows" :x="b.x!" :y="b.y!" :top="b.top" :series="b.series" />
          </article>
        </div>
      </div>

      <div v-if="supplementalEntityBlocks.length" class="supplemental-group">
        <div class="supplemental-subhead"><h4>关联信息</h4><span>补充业务对象属性</span></div>
        <div v-for="(b, bi) in supplementalEntityBlocks" :key="`supplemental-entity-${bi}`" class="entity">
          <div class="entity-hd">{{ entityTitle(b) }}</div>
          <div class="entity-grid">
            <div v-for="(p, pi) in b.pairs" :key="pi" class="entity-cell">
              <div class="ec-k">{{ p[0] }}</div>
              <div class="ec-v">{{ entityValue(p) }}</div>
            </div>
          </div>
        </div>
      </div>

      <div v-if="supplementalTableBlocks.length && supplemental.rows.length && supplemental.columns.length" class="supplemental-group supplemental-table">
        <div class="supplemental-subhead"><h4>明细数据</h4><span>{{ supplementalRowFoot }}</span></div>
        <div class="tbl-wrap">
          <table aria-label="补充结构与明细数据表">
            <thead><tr><th class="row-index" scope="col">#</th><th v-for="(c, ci) in supplemental.columns" :key="ci" scope="col" :class="{ num: supplementalIsMetric(ci) }">{{ c }}</th></tr></thead>
            <tbody>
              <tr v-for="(_, ri) in supplemental.rows" :key="ri">
                <td class="row-index">{{ ri + 1 }}</td>
                <td v-for="(_, ci) in supplemental.columns" :key="ci" :title="supplementalCellTitle(ri, ci)" :class="{ num: supplementalIsMetric(ci) }">
                  {{ supplementalCell(ri, ci) }}
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <div v-if="supplementalHasWideTable" class="table-scroll-hint">{{ TABLE_SCROLL_HINT }}</div>
      </div>
    </section>

    <div v-if="result.row_count > 0 && drillOptions.length && !isEntityCandidate" class="drill">
      <span class="drill-t">换个维度看：</span>
      <button v-for="d in drillOptions" :key="d" type="button" class="pill" @click.stop="emit('drill', d)">按{{ d }} →</button>
    </div>

    <details v-if="displaySql" class="sql-details">
      <summary>查看 SQL</summary>
      <pre>{{ displaySql }}</pre>
    </details>

  </div>
</template>

<style scoped>
/* 🔴 样式双源声明：本组件还依赖 App.vue 的全局类 —— .empty-hint / .caliber-warn / .trunc-note /
   .redact-note / .ask-card（含 ask-hd/ask-q/ask-opts/ask-opt/ask-hint）/ .pill / .num /
   .tbl-wrap 的 th·td nowrap 与 border-bottom（App.vue 样式表 3233-3310 一带）。
   删那些全局规则之前先看这里：scoped 只覆盖了一半，删了就是静默破损。 */
.result-panel { container-type: inline-size; min-width: 0; max-width: 100%; color: var(--text-regular); }

/* direct-derive 提示条：warning 档（同 trunc-note 色系）——它不是错误（caliber-warn 的 error 红），
   也不是「没数据」（empty-hint），是「有数但口径未经合同验证」。 */
.derive-note {
  margin-bottom: 12px; padding: 8px 12px; border-left: 3px solid var(--warning-text);
  border-radius: var(--radius); background: var(--warning-bg);
  color: var(--text-regular); font-size: 12px; line-height: 1.6;
}
.derive-note b { color: var(--warning-text); }

.foundation {
  margin: 0 0 14px; border: 1px solid var(--border); border-radius: 8px;
  background: var(--bg-card); box-shadow: var(--shadow-sm); overflow: hidden;
}
.foundation summary {
  min-height: 42px; display: flex; align-items: center; gap: 8px; padding: 7px 12px;
  color: var(--text-regular); cursor: pointer; list-style: none; user-select: none;
}
.foundation summary::-webkit-details-marker { display: none; }
.foundation-title { color: var(--text-primary); font-size: 12.5px; font-weight: 700; }
.foundation summary small { min-width: 0; color: var(--text-muted); font-size: 11px; overflow-wrap: anywhere; }
.foundation summary > b { margin-left: auto; color: var(--primary); font-size: 10.5px; font-weight: 600; white-space: nowrap; }
.trust-badge {
  flex: 0 0 auto; display: inline-flex; align-items: center; gap: 5px; padding: 2px 7px;
  border: 1px solid var(--border); border-radius: 999px; font-size: 10.5px; font-weight: 700;
}
.trust-badge i { width: 6px; height: 6px; border-radius: 50%; background: currentColor; }
.trust-badge.verified { border-color: rgba(60, 148, 96, .28); background: rgba(60, 148, 96, .07); color: var(--success-text); }
.trust-badge.high { border-color: rgba(var(--primary-rgb), .25); background: var(--primary-light); color: var(--primary); }
.trust-badge.review { border-color: rgba(211, 139, 25, .3); background: rgba(211, 139, 25, .07); color: var(--warning-text); }
.foundation[open] summary { border-bottom: 1px solid var(--divider); }
.foundation-body { display: grid; gap: 11px; padding: 12px; color: var(--text-regular); font-size: 11.5px; line-height: 1.65; }
.foundation-row { display: grid; grid-template-columns: 76px minmax(0, 1fr); gap: 10px; }
.foundation-row > span, .foundation-facts span { color: var(--text-muted); font-size: 10.5px; }
.foundation-row p { margin: 0; overflow-wrap: anywhere; }
.foundation-row.risk { padding: 8px 9px; border-left: 3px solid var(--warning-text); background: var(--warning-bg); }
.intent-row { align-items: start; }
.intent-slots { display: flex; flex-wrap: wrap; gap: 6px; min-width: 0; }
.intent-mode, .intent-slot, .intent-empty {
  display: inline-flex; align-items: center; gap: 4px; min-width: 0; padding: 3px 7px;
  border: 1px solid var(--divider); border-radius: 999px; background: var(--bg-main);
  color: var(--text-regular); font-size: 10.5px; line-height: 1.4; overflow-wrap: anywhere;
}
.intent-mode { border-color: rgba(var(--primary-rgb), .22); background: var(--primary-light); color: var(--primary); font-weight: 700; }
.intent-slot i { color: var(--text-muted); font-style: normal; }
.intent-empty { color: var(--text-muted); }
.foundation-facts { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px; }
.foundation-facts div { min-width: 0; padding: 8px 9px; border: 1px solid var(--divider); border-radius: 6px; background: var(--bg-main); }
.foundation-facts span, .foundation-facts b { display: block; }
.foundation-facts b { margin-top: 2px; color: var(--text-primary); font-size: 11.5px; font-weight: 650; overflow-wrap: anywhere; }
.foundation-checks { margin: 0; padding-left: 17px; }
.foundation-checks li { margin: 1px 0; }
.foundation-trace { display: flex; flex-wrap: wrap; gap: 5px 12px; color: var(--text-faint); font: 10px/1.5 var(--font-mono); overflow-wrap: anywhere; }

.result-section { min-width: 0; margin: 22px 0 0; }
.result-section:first-of-type { margin-top: 14px; }
.section-head {
  display: flex; align-items: flex-end; justify-content: space-between; gap: 14px;
  margin-bottom: 10px; padding-bottom: 8px; border-bottom: 1px solid var(--divider);
}
.section-head h3 { display: flex; align-items: center; gap: 7px; margin: 2px 0 0; color: var(--text-primary); font-size: 15px; line-height: 1.35; font-weight: 700; }
.section-kicker { display: block; color: var(--primary); font-size: 10px; line-height: 1.2; font-weight: 750; }
.kpi-row { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 10px; margin: 0; }
/* auto-fit 会把空轨道塌掉，于是**一张卡吃满整行**：28px 的数字左挂在 800px 空白里。
   `.solo` 那条大卡规则在后面，仍然胜出；窄屏的两处覆写（容器查询 / @media）也在后面，
   仍然 1fr 铺满 —— 这条只管「有图有表时的单卡」这一档。 */
.kpi-row:not(.solo) { grid-template-columns: repeat(auto-fit, minmax(180px, 300px)); justify-content: start; }
.metric-card {
  position: relative; min-width: 0; min-height: 116px; padding: 15px 16px; border: 1px solid var(--border);
  border-radius: 8px; background: var(--bg-card); box-shadow: var(--shadow-sm); overflow: hidden;
}
.metric-card::before { content: ""; position: absolute; inset: 0 0 auto; height: 2px; background: var(--primary); }
.mc-label { color: var(--text-muted); font-size: 12px; line-height: 1.4; text-transform: none; letter-spacing: 0; }
.mc-val {
  margin-top: 8px; color: var(--text-primary); font-size: 28px; line-height: 1.15; font-weight: 750;
  font-variant-numeric: tabular-nums; overflow-wrap: anywhere;
}
.mc-delta { display: flex; align-items: center; gap: 4px; margin-top: 8px; font-size: 12px; font-weight: 650; }
/* 中式约定涨红跌绿：方向另有 ↑/↓ 箭头与 aria-label（上升/下降/持平），不只靠颜色 */
.mc-delta.up { color: var(--error-text); }
.mc-delta.down { color: var(--success-text); }
.mc-delta.flat { color: var(--text-muted); }
.delta-mark { font-size: 14px; line-height: 1; }
.mc-delta .mc-vs { margin-left: 2px; color: var(--text-muted); font-weight: 500; }
.mc-delta-detail { margin-top: 4px; color: var(--text-faint); font-size: 10.5px; font-variant-numeric: tabular-nums; }

/* 单 KPI 大数字卡：精简模式一句话问答的主形态，宽松密度是刻意而非空洞 */
.kpi-row.solo { grid-template-columns: minmax(0, 1fr); }
.kpi-row.solo .metric-card { min-height: 148px; padding: 24px 28px; }
.kpi-row.solo .mc-label { font-size: 13px; }
.kpi-row.solo .mc-val { margin-top: 12px; font-size: 40px; }
.kpi-row.solo .mc-delta { margin-top: 10px; font-size: 13px; }

/* 同窗补充小卡：对齐 metric-card 的边框/圆角/投影，但小一号（标签+数值一行，不做大数字） */
.sales-context {
  display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 8px;
  min-width: 0; margin: 10px 0 0;
}
.sc-cell {
  display: flex; align-items: baseline; justify-content: space-between; gap: 8px;
  min-width: 0; padding: 8px 12px; border: 1px solid var(--border);
  border-radius: 8px; background: var(--bg-card); box-shadow: var(--shadow-sm); overflow: hidden;
}
.sc-label { flex: 0 0 auto; color: var(--text-muted); font-size: 11px; line-height: 1.4; white-space: nowrap; }
.sc-val {
  min-width: 0; color: var(--text-primary); font-size: 14px; line-height: 1.3; font-weight: 700;
  font-variant-numeric: tabular-nums; text-align: right; overflow-wrap: anywhere;
}

.chart-grid { display: grid; grid-template-columns: minmax(0, 1fr); gap: 12px; }
.chart-grid.paired { grid-template-columns: repeat(2, minmax(0, 1fr)); }
.chart-card {
  min-width: 0; margin: 0; padding: 14px 14px 8px; border: 1px solid var(--border);
  border-radius: 8px; background: var(--bg-card); box-shadow: var(--shadow-sm); overflow: hidden;
}
.chart-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; padding: 0 2px 8px; }
.chart-head > div { min-width: 0; }
.chart-head h4 { margin: 0; color: var(--text-primary); font-size: 14px; line-height: 1.4; font-weight: 700; overflow-wrap: anywhere; }
.chart-head p { margin: 3px 0 0; color: var(--text-muted); font-size: 11px; line-height: 1.45; }
.chart-type {
  flex: 0 0 auto; padding: 2px 7px; border: 1px solid rgba(var(--primary-rgb), .2);
  border-radius: 4px; background: var(--primary-light); color: var(--primary); font-size: 10px; font-weight: 700;
}
/* 异步图表组件的加载/失败占位（defineAsyncComponent 的 loading/errorComponent） */
.chart-state { display: grid; place-items: center; min-height: 120px; color: var(--text-faint); font-size: 12px; }

.entity { margin: 0 0 10px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg-card); overflow: hidden; }
.entity:last-child { margin-bottom: 0; }
.entity-hd { padding: 10px 13px; background: var(--bg-main); color: var(--text-primary); font-size: 13px; font-weight: 700; }
.entity-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); }
.entity-cell { min-width: 0; padding: 10px 13px; border-right: 1px solid var(--divider); border-bottom: 1px solid var(--divider); }
.ec-k { color: var(--text-muted); font-size: 11px; }
.ec-v { margin-top: 4px; color: var(--text-primary); font-size: 13px; line-height: 1.55; overflow-wrap: anywhere; word-break: normal; }

.table-heading { align-items: center; }
.row-count { color: var(--text-muted); font-size: 11px; text-align: right; }
.tbl-wrap {
  width: 100%; max-width: 100%; max-height: 520px; margin: 0; padding: 0;
  border: 1px solid var(--border); border-radius: 8px; background: var(--bg-card); box-shadow: none;
  overflow: auto; overscroll-behavior-inline: contain; scrollbar-gutter: stable;
}
.tbl-wrap table { width: 100%; border-collapse: collapse; border-radius: 0; font-size: 13px; }
.tbl-wrap th, .tbl-wrap td { max-width: 320px; padding: 10px 12px; line-height: 1.55; overflow: hidden; text-overflow: ellipsis; }
.tbl-wrap th { position: sticky; top: 0; z-index: 1; background: var(--bg-main); color: var(--text-regular); font-size: 11.5px; letter-spacing: 0; text-align: left; }
.tbl-wrap tbody tr:nth-child(even) td { background: color-mix(in srgb, var(--bg-main) 56%, var(--bg-card)); }
.tbl-wrap tbody tr:hover td { background: var(--primary-light); }
.tbl-wrap .row-index {
  position: sticky; left: 0; z-index: 2; width: 38px; min-width: 38px; max-width: 38px;
  padding-inline: 6px; background: var(--bg-card); color: var(--text-faint); text-align: center;
  font-size: 10.5px; font-variant-numeric: tabular-nums;
}
.tbl-wrap thead .row-index { z-index: 3; background: var(--bg-main); }
/* 斑马行的 sticky 行号格与同行数据格同一底色（color-mix），不能一行两种底 */
.tbl-wrap tbody tr:nth-child(even) .row-index { background: color-mix(in srgb, var(--bg-main) 56%, var(--bg-card)); }
  /* sticky 首列不能用半透明底：横滚时被压在下面的单元格文字会直接透出来 */
.tbl-wrap tbody tr:hover .row-index { background: color-mix(in srgb, var(--primary) 8%, var(--bg-card)); }
.redact-lock { margin-left: 4px; font-size: 10px; }
.table-scroll-hint { display: none; margin-top: 6px; color: var(--text-muted); font-size: 10.5px; text-align: right; }

.supplemental-section { min-width: 0; margin-top: 26px; padding-top: 18px; border-top: 1px solid var(--divider); }
.supplemental-head { display: flex; align-items: flex-end; justify-content: space-between; gap: 16px; margin-bottom: 14px; }
.supplemental-head > div { min-width: 0; }
.supplemental-head h3 { margin: 3px 0 0; color: var(--text-primary); font-size: 16px; line-height: 1.35; font-weight: 750; }
.supplemental-head p { margin: 4px 0 0; color: var(--text-muted); font-size: 11.5px; line-height: 1.5; }
.supplemental-kpis { margin-bottom: 16px; }
.supplemental-group { min-width: 0; margin-top: 18px; }
.supplemental-subhead { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; margin-bottom: 8px; }
.supplemental-subhead h4 { margin: 0; color: var(--text-primary); font-size: 13px; line-height: 1.4; font-weight: 700; }
.supplemental-subhead span { color: var(--text-muted); font-size: 10.5px; text-align: right; }
.supplemental-table .tbl-wrap { max-height: 440px; }

.entity-choice-card {
  padding: 16px; border: 1px solid rgba(var(--primary-rgb), .28); border-left: 3px solid var(--primary);
  border-radius: 8px; background: color-mix(in srgb, var(--primary-light) 38%, var(--bg-card));
}
.entity-choice-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.entity-choice-head h3 { margin: 3px 0 0; color: var(--text-primary); font-size: 15px; line-height: 1.4; }
.entity-choice-head > span { color: var(--text-muted); font-size: 11px; white-space: nowrap; }
.entity-choice-list { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px; margin-top: 13px; }
.entity-choice {
  display: grid; grid-template-columns: auto minmax(0, 1fr) auto; grid-template-areas: "kind name action" "kind code action";
  align-items: center; gap: 2px 10px; min-width: 0; padding: 11px 12px; border: 1px solid var(--border);
  border-radius: 7px; background: var(--bg-card); color: var(--text-regular); text-align: left; cursor: pointer;
}
.entity-choice:hover { border-color: var(--primary); background: var(--primary-light); }
.entity-choice:focus-visible { outline: 2px solid var(--primary); outline-offset: 2px; }
.entity-choice-kind {
  grid-area: kind; padding: 3px 6px; border-radius: 4px; background: var(--bg-main);
  color: var(--primary); font-size: 10px; font-weight: 700; white-space: nowrap;
}
.entity-choice-name { grid-area: name; min-width: 0; color: var(--text-primary); font-size: 12.5px; font-weight: 700; overflow-wrap: anywhere; }
.entity-choice-code { grid-area: code; min-width: 0; color: var(--text-muted); font-size: 10.5px; overflow-wrap: anywhere; }
.entity-choice-action { grid-area: action; color: var(--primary); font-size: 11px; white-space: nowrap; }
.entity-choice-hint { margin: 10px 0 0; color: var(--text-muted); font-size: 10.5px; line-height: 1.5; }

.drill { margin-top: 16px; padding-top: 12px; border-top: 1px solid var(--divider); }
.drill-t { color: var(--text-muted); }
.pill { border-radius: 5px; }

.sql-details {
  margin-top: 18px; border: 1px solid var(--border); border-radius: 7px;
  background: var(--bg-card); overflow: hidden;
}
.sql-details summary {
  padding: 10px 13px; color: var(--text-muted); font-size: 11.5px; font-weight: 650;
  cursor: pointer; user-select: none;
}
.sql-details[open] summary { border-bottom: 1px solid var(--divider); color: var(--text-primary); }
.sql-details pre {
  max-height: 320px; margin: 0; padding: 13px; overflow: auto; background: var(--bg-main);
  color: var(--text-regular); font: 11.5px/1.65 ui-monospace, SFMono-Regular, Consolas, monospace;
  white-space: pre; tab-size: 2;
}

.insight-section { margin-top: 18px; padding-top: 2px; }
.analysis-basis { color: var(--text-muted); font-size: 11px; }
/* auto-fit：1-2 张卡时不压成 1/3 宽留白 */
.insight-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 10px; }
.insight-card {
  min-width: 0; padding: 13px 14px; border: 1px solid var(--border); border-left-width: 3px;
  border-radius: 7px; background: var(--bg-card);
}
.insight-card.conclusion { border-left-color: var(--primary); }
.insight-card.risk { border-left-color: var(--warning-text); }
.insight-card.action { border-left-color: var(--success-text); }
/* other = prompt 之外的标题落的通用桶：必须看得见（此前这类内容被并进上一个桶乱分） */
.insight-card.other { border-left-color: var(--text-faint); }
.insight-card-head { display: flex; align-items: center; gap: 7px; color: var(--text-primary); font-size: 12px; font-weight: 750; }
.insight-dot { width: 7px; height: 7px; border-radius: 50%; background: currentColor; }
.insight-card.conclusion .insight-dot { color: var(--primary); }
.insight-card.risk .insight-dot { color: var(--warning-text); }
.insight-card.action .insight-dot { color: var(--success-text); }
.insight-card.other .insight-dot { color: var(--text-faint); }
.insight-card ul { display: grid; gap: 7px; margin: 9px 0 0; padding: 0; list-style: none; }
.insight-card li { position: relative; padding-left: 11px; color: var(--text-regular); font-size: 12px; line-height: 1.65; overflow-wrap: anywhere; }
.insight-card li::before { content: ""; position: absolute; top: .72em; left: 0; width: 3px; height: 3px; border-radius: 50%; background: var(--text-faint); }
/* 与文件其余样式同款的单行紧凑写法；同族 .ask-card 系列在 App.vue 全局（见文件头声明） */
.ask-custom { display: flex; gap: 8px; margin-top: 10px; }
.ask-input { min-width: 0; flex: 1; height: 36px; padding: 0 12px; border: 1px solid var(--border); border-radius: var(--radius); outline: none; background: var(--bg-card); color: var(--text-regular); font: inherit; }
.ask-input:focus { border-color: var(--primary); box-shadow: 0 0 0 2px var(--primary-bg); }
.ask-submit { height: 36px; padding: 0 16px; border: 1px solid var(--primary); border-radius: var(--radius); background: var(--primary); color: var(--on-primary); font: inherit; cursor: pointer; }
.ask-submit:disabled { opacity: .45; cursor: not-allowed; }

@container (max-width: 720px) {
  .chart-grid.paired, .insight-grid { grid-template-columns: 1fr; }
  .entity-choice-list { grid-template-columns: 1fr; }
  .entity-grid { grid-template-columns: 1fr; }
  .entity-cell { border-right: 0; }
  .kpi-row { grid-template-columns: repeat(auto-fit, minmax(145px, 1fr)); }
  .table-scroll-hint { display: block; }
  .supplemental-head { align-items: flex-start; }
  .foundation-facts { grid-template-columns: 1fr; }
}

/* 与上方 @container(720px) 同形规则是**双断点刻意并存**，不是重复：
   container 管「面板在宽视口里被挤窄」（预览分屏）；media 管「视口本身窄」（手机/小窗，
   且兼容不支持 container query 的端）。删一边另一边管不到。 */
@media (max-width: 600px) {
  .result-section { margin-top: 18px; }
  .section-head { align-items: flex-start; margin-bottom: 8px; }
  .section-head h3 { font-size: 14px; }
  .kpi-row { grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 8px; }
  .metric-card { min-height: 98px; padding: 13px 14px; }
  .mc-val { font-size: 24px; }
  .chart-grid.paired { grid-template-columns: 1fr; }
  .chart-card { padding: 12px 8px 4px; }
  .chart-head { padding: 0 5px 5px; }
  .entity-grid { grid-template-columns: 1fr; }
  .entity-cell { border-right: 0; }
  .table-heading { align-items: flex-end; }
  .row-count { max-width: 52%; }
  .supplemental-head { align-items: flex-start; }
  .supplemental-subhead { align-items: flex-start; }
  .tbl-wrap { max-height: 420px; }
  .tbl-wrap th, .tbl-wrap td { max-width: 210px; padding: 8px 9px; font-size: 11.5px; }
  .insight-grid { grid-template-columns: 1fr; gap: 8px; }
  .analysis-basis { display: none; }
  .foundation summary { align-items: flex-start; flex-wrap: wrap; }
  .foundation summary > b { margin-left: 0; }
  .foundation-row { grid-template-columns: 1fr; gap: 3px; }
  .ask-custom { flex-direction: column; }
  .ask-submit { width: 100%; }
}
</style>
