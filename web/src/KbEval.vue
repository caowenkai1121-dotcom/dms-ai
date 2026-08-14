<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { sessionHeaders } from './panel-utils'

interface EvalRun {
  id: string; status: string; statusText: string; tone: '' | 'running' | 'failed' | 'done'
  score: number | null; total: number | null
  completed: number | null; durationMs: number | null; createdAt: string
}
interface EvalItem {
  key: string; question: string; answer: string
  recall: (number | null)[]
  correct: boolean | null; reason: string
}
interface EvalSummary {
  status: string; score: number | null; total: number | null; completed: number | null
  durationMs: number | null; recall: (number | null)[]; accuracy: number | null
}
type Verdict = 'correct' | 'wrong' | 'unknown'

/** 召回档位闭集：normalize/摘要卡/明细行共用一处，不再各写字面量。 */
const RECALL_KS = [1, 3, 5, 10] as const
/** 轮询间隔（进行中自动刷新）；报告页文案也引用它，改一处不失真。 */
const POLL_MS = 5000
/** 状态判定闭集：statusText/isRunning/样式类共用同一套正则，不许两处各写一套。 */
const RUNNING_RE = /running|processing|pending|queued|进行中|排队|进行/
const COMPLETED_RE = /completed|done|finished|success|完成/
const FAILED_RE = /failed|error|失败/

const props = defineProps<{ token?: string; spaceId?: string; writable?: boolean }>()
const emit = defineEmits<{ (e: 'auth-expired'): void }>()

const view = ref<'list' | 'report'>('list')
const runs = ref<EvalRun[]>([])
const listLoading = ref(false)
const listUnavailable = ref(false)
const creating = ref(false)
const sampleSize = ref('')
const note = ref('')
const reportId = ref('')
const reportLoading = ref(false)
const reportUnavailable = ref(false)
const reportErr = ref('')
const summary = ref<EvalSummary | null>(null)
const items = ref<EvalItem[]>([])
const onlyWrong = ref(false)
/** 静默轮询上次失败标记：给「重试中」提示，不让页面悄悄停在旧数据。 */
const pollFailed = ref(false)
let evalEpoch = 0
let pollTimer = 0

function headers(): Record<string, string> {
  return sessionHeaders(props.token, () => emit('auth-expired'))
}
/** 会话失效的 message 需要能透出（headers() 抛的那句），其余错误仍走通用兜底。 */
function sessionMsg(e: unknown): string {
  return e instanceof Error && e.message.includes('登录会话') ? e.message : ''
}

function asList(data: unknown, keys: string[]): Record<string, unknown>[] {
  if (Array.isArray(data)) return data.filter((x): x is Record<string, unknown> => !!x && typeof x === 'object')
  if (!data || typeof data !== 'object') return []
  const bag = data as Record<string, unknown>
  for (const key of keys) {
    if (Array.isArray(bag[key])) return (bag[key] as unknown[]).filter((x): x is Record<string, unknown> => !!x && typeof x === 'object')
  }
  return []
}

function num(...values: unknown[]): number | null {
  for (const value of values) {
    // '' 会被 Number 当成 0：空串/空值是「缺失」不是 0，跳过
    if (value === '' || value == null) continue
    const n = Number(value)
    if (Number.isFinite(n)) return n
  }
  return null
}

/** 召回率/准确率兼容 0~1 与 0~100 两种口径，统一成 0~1 */
function ratio(...values: unknown[]): number | null {
  const n = num(...values)
  if (n == null) return null
  return Math.min(1, Math.max(0, n > 1 ? n / 100 : n))
}

function durationMsOf(raw: Record<string, unknown>): number | null {
  const direct = num(raw.duration_ms, raw.elapsed_ms, raw.cost_ms)
  if (direct != null) return direct
  const seconds = num(raw.duration_s, raw.elapsed_s)
  if (seconds != null) return seconds * 1000
  const started = Date.parse(String(raw.started_at ?? ''))
  const finished = Date.parse(String(raw.finished_at ?? raw.completed_at ?? ''))
  return Number.isFinite(started) && Number.isFinite(finished) && finished >= started ? finished - started : null
}

function isRunning(status: string): boolean {
  return RUNNING_RE.test(status.toLowerCase())
}
function isFailed(status: string): boolean {
  return FAILED_RE.test(status.toLowerCase())
}
/** 只有明确完成才给绿色成功样式；未知状态走默认中性灰（避免误报「成功」） */
function isCompleted(status: string): boolean {
  return COMPLETED_RE.test(status.toLowerCase())
}

function statusText(status: string): string {
  const s = status.toLowerCase()
  // 失败判定提前：completed_with_errors 这类状态不许被「已完成」掩盖
  if (FAILED_RE.test(s)) return '失败'
  if (COMPLETED_RE.test(s)) return '已完成'
  if (/pending|queued|排队/.test(s)) return '排队中'
  if (RUNNING_RE.test(s)) return '运行中'
  return status || '未知'
}
/** 状态样式类（对原始 status 判，不匹配翻译后的中文文案 —— 措辞改动不会让样式失效）。 */
function statusTone(status: string): '' | 'running' | 'failed' | 'done' {
  if (isFailed(status)) return 'failed'
  if (isRunning(status)) return 'running'
  if (isCompleted(status)) return 'done'
  return ''
}

function normalizeRun(raw: Record<string, unknown>): EvalRun {
  const status = String(raw.status ?? raw.state ?? '')
  return {
    id: String(raw.run_id ?? raw.id ?? ''),
    status,
    // 展示文案与样式类在归一时预算：不随 onlyWrong 等任意响应式变化逐行重算
    statusText: statusText(status),
    tone: statusTone(status),
    score: ratio(raw.overall_score, raw.score),
    total: num(raw.total_questions, raw.total, raw.sample_size),
    completed: num(raw.completed, raw.finished, raw.done),
    durationMs: durationMsOf(raw),
    createdAt: String(raw.created_at ?? raw.started_at ?? ''),
  }
}

function recallOf(raw: Record<string, unknown>, k: number): number | null {
  const bag = (raw.recall ?? raw.recalls ?? {}) as Record<string, unknown>
  return ratio(
    raw[`recall${k}`], raw[`recall_at_${k}`], raw[`recall@${k}`], raw[`r${k}`], raw[`r_${k}`],
    bag[`recall_at_${k}`], bag[String(k)], bag[`k${k}`], bag[`r${k}`],
  )
}

function verdictOf(raw: Record<string, unknown>): boolean | null {
  const flag = raw.correct ?? raw.pass ?? raw.passed
  if (typeof flag === 'boolean') return flag
  const text = String(raw.verdict ?? raw.judgement ?? raw.judge ?? '').trim().toLowerCase()
  if (!text) return null
  if (/^(正确|对|correct|pass|true|1|yes)$/.test(text)) return true
  if (/^(错误|错|incorrect|wrong|fail|false|0|no)$/.test(text)) return false
  return null
}

function normalizeItem(raw: Record<string, unknown>, index: number): EvalItem {
  const question = String(raw.question ?? raw.query ?? raw.input ?? '')
  return {
    // 稳定 key（原始序号 + 问题）：「仅查看错误」筛选切换时不错位复用
    key: `${index}:${question}`,
    question,
    answer: String(raw.answer ?? raw.generated_answer ?? raw.output ?? ''),
    recall: RECALL_KS.map((k) => recallOf(raw, k)),
    correct: verdictOf(raw),
    reason: String(raw.reason ?? raw.judge_reason ?? raw.explanation ?? ''),
  }
}

function percentText(value: number | null): string {
  return value == null ? '-' : `${(value * 100).toFixed(1)}%`
}
function durationText(ms: number | null): string {
  if (ms == null || ms < 0) return '-'
  if (ms < 1000) return '<1秒'
  const total = Math.round(ms / 1000)
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = total % 60
  if (h) return `${h}时${m}分${s}秒`
  if (m) return `${m}分${s}秒`
  return `${s}秒`
}
// 模块级单例 formatter：列表每行每次渲染都过 dateText，不能一列 new 一个
const DATE_FMT = new Intl.DateTimeFormat('zh-CN', {
  month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false,
})
const DATE_FMT_Y = new Intl.DateTimeFormat('zh-CN', {
  year: '2-digit', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false,
})
function dateText(value: string): string {
  if (!value) return '-'
  const d = new Date(value)
  if (Number.isNaN(d.getTime())) return value.slice(0, 16).replace('T', ' ')
  // 非当年记录补年份，跨年评估记录才能区分
  return (d.getFullYear() === new Date().getFullYear() ? DATE_FMT : DATE_FMT_Y).format(d)
}
function scoreClass(value: number | null): string {
  if (value == null) return ''
  if (value >= 0.8) return 'good'
  if (value >= 0.6) return 'warn'
  return 'bad'
}
function verdictClass(item: EvalItem): Verdict {
  return item.correct == null ? 'unknown' : item.correct ? 'correct' : 'wrong'
}

const anyRunning = computed(() => runs.value.some((run) => isRunning(run.status)))
const visibleItems = computed(() => onlyWrong.value ? items.value.filter((item) => item.correct === false) : items.value)
const reportRunning = computed(() => summary.value != null && isRunning(summary.value.status))
const hasUnjudged = computed(() => items.value.some((item) => item.correct == null))

async function loadRuns(epoch: number, silent = false) {
  if (!silent) {
    listLoading.value = true
    listUnavailable.value = false
  }
  try {
    const response = await fetch(`/api/kb/eval/runs?space_id=${encodeURIComponent(props.spaceId ?? '')}`, { headers: headers() })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    // ok 但非 JSON（网关错误页等）按不可用处理，不静默显示成「还没有评估记录」
    const data = await response.json().catch(() => { throw new Error(`HTTP ${response.status}`) })
    if (epoch !== evalEpoch) return
    runs.value = asList(data, ['runs', 'items', 'list']).map(normalizeRun).filter((run) => run.id)
    pollFailed.value = false
  } catch (e) {
    if (epoch === evalEpoch && !silent) {
      runs.value = []
      listUnavailable.value = true
      const m = sessionMsg(e)
      if (m) note.value = m
    } else if (epoch === evalEpoch) {
      pollFailed.value = true
    }
  } finally {
    if (epoch === evalEpoch && !silent) listLoading.value = false
  }
  // clearTimeout 必须在 epoch 守卫内：旧 epoch 的迟到响应不许清掉新 epoch 刚排好的轮询
  if (epoch === evalEpoch) {
    window.clearTimeout(pollTimer)
    if (anyRunning.value) {
      pollTimer = window.setTimeout(() => void loadRuns(epoch, true), POLL_MS)
    }
  }
}

async function createRun() {
  if (creating.value || !props.writable) return
  const epoch = evalEpoch
  creating.value = true
  note.value = ''
  try {
    const body: Record<string, unknown> = { space_id: props.spaceId ?? '' }
    // 先取整再判断：0.5 不该发出 sample_size: 0；上限 500 在代码里同样钳住
    const size = Math.floor(Number(sampleSize.value))
    if (Number.isFinite(size) && size >= 1) body.sample_size = Math.min(500, size)
    const response = await fetch('/api/kb/eval/runs', {
      method: 'POST',
      headers: { ...headers(), 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    })
    if (response.status === 401) emit('auth-expired')
    const data = await response.json().catch(() => ({}))
    if (!response.ok) {
      const serverErr = typeof data.error === 'string' && data.error.trim() ? data.error : `HTTP ${response.status}`
      throw new Error(serverErr)
    }
    if (epoch !== evalEpoch) return
    sampleSize.value = ''
    await loadRuns(epoch, true)
    const newId = String(data.run_id ?? data.id ?? '')
    if (newId && epoch === evalEpoch) void openReport(newId)
  } catch (e) {
    if (epoch === evalEpoch) {
      // 优先展示服务端返回的具体错误；网络错误时 run 可能已在服务端创建，措辞不下「未生效」结论
      const msg = e instanceof Error && e.message && !e.message.startsWith('HTTP') ? e.message : ''
      note.value = msg ? `创建失败：${msg}` : '创建失败，请稍后重试。'
    }
  } finally {
    if (epoch === evalEpoch) creating.value = false
  }
}

async function openReport(id: string, silent = false) {
  const epoch = evalEpoch
  reportId.value = id
  view.value = 'report'
  if (!silent) {
    onlyWrong.value = false
    reportLoading.value = true
    reportUnavailable.value = false
    reportErr.value = ''
    summary.value = null
    items.value = []
  }
  window.clearTimeout(pollTimer)
  try {
    const response = await fetch(`/api/kb/eval/runs/${encodeURIComponent(id)}`, { headers: headers() })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    const data = await response.json().catch(() => { throw new Error(`HTTP ${response.status}`) })
    if (epoch !== evalEpoch || reportId.value !== id) return
    const report = (data.report ?? data.result ?? data) as Record<string, unknown>
    const sum: Record<string, unknown> = { ...((report.summary ?? report.metrics ?? {}) as Record<string, unknown>) }
    // 顶层覆盖 summary 时跳过 null/undefined：顶层带 score:null 不该抹掉 summary 里的有效值
    for (const [k, v] of Object.entries(report)) if (v != null) sum[k] = v
    summary.value = {
      status: String(sum.status ?? sum.state ?? ''),
      score: ratio(sum.overall_score, sum.score),
      total: num(sum.total_questions, sum.total, sum.sample_size),
      completed: num(sum.completed, sum.finished, sum.done),
      durationMs: durationMsOf(sum),
      recall: RECALL_KS.map((k) => recallOf(sum, k)),
      accuracy: ratio(sum.accuracy, sum.answer_accuracy, sum.answer_acc, sum.correct_rate),
    }
    items.value = asList(report, ['items', 'questions', 'results', 'cases']).map(normalizeItem)
    pollFailed.value = false
  } catch (e) {
    if (!silent && epoch === evalEpoch && reportId.value === id) {
      reportUnavailable.value = true
      reportErr.value = sessionMsg(e)
    } else if (silent && epoch === evalEpoch) {
      pollFailed.value = true
    }
  } finally {
    if (epoch === evalEpoch && reportId.value === id) reportLoading.value = false
  }
  // 同 loadRuns：清/排定时器都在 epoch 守卫内
  if (epoch === evalEpoch && reportId.value === id) {
    window.clearTimeout(pollTimer)
    if (reportRunning.value) {
      pollTimer = window.setTimeout(() => void openReport(id, true), POLL_MS)
    }
  }
}

function backToList() {
  window.clearTimeout(pollTimer)
  reportId.value = ''
  view.value = 'list'
  // 静默刷新：数据通常很新，不闪全屏加载态
  void loadRuns(evalEpoch, true)
}

async function reload() {
  const epoch = ++evalEpoch
  window.clearTimeout(pollTimer)
  view.value = 'list'
  reportId.value = ''
  note.value = ''
  pollFailed.value = false
  runs.value = []
  summary.value = null
  items.value = []
  if (!props.spaceId) {
    listUnavailable.value = true
    return
  }
  await loadRuns(epoch)
}

// token 失效后重新登录（spaceId 没变）也要重载，不能停在「暂不可用」
watch([() => props.spaceId, () => props.token], () => { void reload() })

onBeforeUnmount(() => {
  evalEpoch++
  window.clearTimeout(pollTimer)
})

void reload()
</script>

<template>
  <div class="eval-panel">
    <template v-if="view === 'list'">
      <div class="eval-head">
        <div>
          <h3>RAG 评估</h3>
          <span>用标准题集回归当前空间的检索召回与答案质量；评估在服务端异步执行。</span>
        </div>
        <div class="eval-new">
          <input
            v-model="sampleSize" type="number" min="1" max="500" placeholder="题数（默认全量）"
            aria-label="评估题数" :disabled="creating || !writable"
            :title="writable ? '留空则按全量题集评估' : '只读空间不能新建评估'"
          />
          <button
            class="primary-btn" type="button" :disabled="creating || !writable"
            :title="writable ? '对当前空间发起一次评估' : '只读空间不能新建评估'"
            @click="createRun"
          >{{ creating ? '创建中…' : '新建评估' }}</button>
        </div>
      </div>
      <div v-if="note" class="eval-note" role="status">{{ note }}</div>

      <div v-if="listLoading" class="eval-state" role="status">
        <strong>正在读取评估记录</strong><span>请稍候。</span>
      </div>
      <div v-else-if="!runs.length" class="eval-state">
        <strong>{{ !spaceId ? '请先选择知识空间' : listUnavailable ? '评估功能暂不可用' : '还没有评估记录' }}</strong>
        <span>{{ !spaceId ? '在左侧选择一个知识空间后再查看评估。' : listUnavailable ? '服务端评估接口尚未就绪，请稍后刷新重试。' : '点击右上角「新建评估」对当前空间跑一次回归。' }}</span>
      </div>
      <div v-else class="eval-table" role="table" aria-label="评估记录">
        <div class="eval-table-head" role="row">
          <span role="columnheader">评估 ID</span><span role="columnheader">状态</span><span role="columnheader">总体评分</span><span role="columnheader">题数</span><span role="columnheader">耗时</span><span role="columnheader">创建时间</span><span role="columnheader">操作</span>
        </div>
        <article v-for="run in runs" :key="run.id" class="eval-row" role="row">
          <span class="eval-id" role="cell" data-label="评估 ID" :title="run.id">{{ run.id }}</span>
          <span role="cell" data-label="状态"><i class="eval-status" :class="run.tone">{{ run.statusText }}</i></span>
          <span role="cell" data-label="总体评分"><b class="eval-score" :class="scoreClass(run.score)">{{ percentText(run.score) }}</b></span>
          <span role="cell" data-label="题数">{{ run.completed ?? '-' }}/{{ run.total ?? '-' }}</span>
          <span role="cell" data-label="耗时">{{ durationText(run.durationMs) }}</span>
          <span role="cell" data-label="创建时间">{{ dateText(run.createdAt) }}</span>
          <span role="cell"><button type="button" class="text-btn" @click="openReport(run.id)">查看报告</button></span>
        </article>
      </div>
    </template>

    <template v-else>
      <div class="eval-head">
        <div>
          <h3 :title="`评估报告 · ${reportId}`">评估报告 · {{ reportId }}</h3>
          <span v-if="reportRunning">评估仍在运行，页面每 {{ POLL_MS / 1000 }} 秒自动刷新。<template v-if="pollFailed">（上次刷新失败，正在重试）</template></span>
          <span v-else>召回率按检索前 N 名命中统计；答案准确率为评判通过率。</span>
        </div>
        <label class="eval-only-wrong">
          <input v-model="onlyWrong" type="checkbox" />
          <span>仅查看错误</span>
        </label>
        <button class="secondary-btn" type="button" @click="backToList">返回列表</button>
      </div>

      <div v-if="reportLoading" class="eval-state" role="status">
        <strong>正在读取评估报告</strong><span>题目较多时需要几秒钟。</span>
      </div>
      <div v-else-if="reportUnavailable" class="eval-state">
        <strong>评估报告暂不可用</strong>
        <span>{{ reportErr || '服务端评估接口尚未就绪，或该次评估已被清理。' }}</span>
        <button class="secondary-btn" type="button" @click="backToList">返回列表</button>
      </div>
      <template v-else-if="summary">
        <div class="eval-summary">
          <div class="eval-card">
            <span>状态</span>
            <i class="eval-status" :class="statusTone(summary.status)">{{ statusText(summary.status) }}</i>
          </div>
          <div class="eval-card">
            <span>总体评分</span>
            <b class="eval-score" :class="scoreClass(summary.score)">{{ percentText(summary.score) }}</b>
          </div>
          <div class="eval-card"><span>总问题数</span><b>{{ summary.total ?? '-' }}</b></div>
          <div class="eval-card"><span>完成数</span><b>{{ summary.completed ?? '-' }}</b></div>
          <div class="eval-card"><span>总耗时</span><b>{{ durationText(summary.durationMs) }}</b></div>
          <div class="eval-card wide">
            <span>检索与答案指标</span>
            <b class="eval-metrics">
              <template v-for="(k, i) in RECALL_KS" :key="k">R@{{ k }} <em>{{ percentText(summary.recall[i]) }}</em></template>
              答案准确率 <em :class="scoreClass(summary.accuracy)">{{ percentText(summary.accuracy) }}</em>
            </b>
          </div>
        </div>

        <div class="eval-result-count">{{ onlyWrong ? `仅错误，共 ${visibleItems.length} 条` : `全部结果，共 ${visibleItems.length} 条` }}</div>
        <div v-if="!visibleItems.length" class="eval-state small">
          <strong>{{ onlyWrong ? '没有错误题目' : '还没有题目结果' }}</strong>
          <span>{{ onlyWrong ? (hasUnjudged ? '没有判为错误的题目（存在未评判条目）。' : '本次评估全部通过。') : '评估完成或产生结果后在此展示。' }}</span>
        </div>
        <div v-else class="eval-table report" role="table" aria-label="评估明细">
          <div class="eval-table-head" role="row">
            <span role="columnheader">#</span><span role="columnheader">问题</span><span role="columnheader">生成答案</span><span role="columnheader">检索指标</span><span role="columnheader">答案评判</span>
          </div>
          <article v-for="(item, index) in visibleItems" :key="item.key" class="eval-row report-row" role="row">
            <span class="eval-ord" role="cell">{{ index + 1 }}</span>
            <span class="eval-question" role="cell" data-label="问题" :title="item.question">{{ item.question }}</span>
            <span class="eval-answer" role="cell" data-label="生成答案" :title="item.answer">{{ item.answer || '-' }}</span>
            <span class="eval-recall" role="cell" data-label="检索指标">
              <template v-for="(value, k) in item.recall" :key="k">
                R@{{ RECALL_KS[k] }} <b :class="{ miss: value === 0 }">{{ percentText(value) }}</b>
              </template>
            </span>
            <span class="eval-verdict" role="cell" data-label="答案评判">
              <i class="eval-verdict-badge" :class="verdictClass(item)">
                {{ item.correct == null ? '未评判' : item.correct ? '正确' : '错误' }}
              </i>
              <em :title="item.reason || undefined">{{ item.reason || '-' }}</em>
            </span>
          </article>
        </div>
      </template>
    </template>
  </div>
</template>

<style scoped>
.eval-panel { width: 100%; }
.eval-head { display: flex; align-items: flex-end; gap: 16px; }
.eval-head h3 { max-width: min(560px, 60vw); overflow: hidden; color: var(--text-primary); font-size: 14px; text-overflow: ellipsis; white-space: nowrap; }
.eval-head span { display: block; margin-top: 3px; color: var(--text-muted); font-size: 11.5px; }
.eval-new { margin-left: auto; display: flex; align-items: center; gap: 8px; }
.eval-new input {
  width: 130px; height: 32px; padding: 0 9px; border: 1px solid var(--border); border-radius: 6px;
  outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px;
}
.eval-new input:focus { border-color: var(--primary); box-shadow: var(--ring); }
.primary-btn, .secondary-btn {
  height: 32px; border: 1px solid var(--border); border-radius: 6px; cursor: pointer; font: inherit; font-size: 12px;
}
.primary-btn { padding: 0 13px; border-color: var(--primary); background: var(--primary); color: var(--on-primary); white-space: nowrap; }
.primary-btn:hover { background: var(--primary-hover); }
.secondary-btn { padding: 0 13px; background: var(--bg-card); color: var(--text-regular); white-space: nowrap; }
.secondary-btn:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-light); }
.text-btn { padding: 0; border: 0; background: transparent; color: var(--primary); cursor: pointer; font-size: 11.5px; }
.text-btn:hover { text-decoration: underline; }
button:disabled { cursor: not-allowed; opacity: .55; }
/* 创建失败是错误语义，用 error 配色（不用 warning） */
.eval-note { margin-top: 8px; padding: 7px 10px; border-left: 3px solid var(--error-text); background: var(--error-bg); color: var(--error-text); font-size: 11.5px; }
.eval-state {
  min-height: 200px; margin-top: 12px; display: flex; align-items: center; justify-content: center;
  flex-direction: column; gap: 8px; border: 1px solid var(--border); color: var(--text-muted);
  text-align: center; font-size: 12px;
}
.eval-state.small { min-height: 120px; }
.eval-state strong { color: var(--text-primary); font-size: 14px; }
.eval-state span { max-width: 460px; line-height: 1.6; }
.eval-state .secondary-btn { margin-top: 6px; }
.eval-table { margin-top: 12px; border: 1px solid var(--border); overflow: hidden; }
.eval-table-head, .eval-row {
  display: grid; grid-template-columns: minmax(120px, 1.2fr) 64px 74px 74px 88px 108px 72px;
  align-items: center; column-gap: 10px;
}
.eval-table.report .eval-table-head, .eval-row.report-row {
  grid-template-columns: 34px minmax(150px, 1.4fr) minmax(180px, 1.8fr) minmax(150px, 1fr) minmax(180px, 1.4fr);
}
.eval-table-head {
  min-height: 34px; padding: 0 12px; background: var(--bg-main); border-bottom: 1px solid var(--border);
  color: var(--text-muted); font-size: 11px; font-weight: 600;
}
.eval-row { min-height: 44px; padding: 8px 12px; border-top: 1px solid var(--divider); font-size: 12px; }
.eval-row:first-of-type { border-top: 0; }
.eval-row:hover { background: var(--bg-hover); }
.eval-row > span { min-width: 0; overflow: hidden; color: var(--text-muted); text-overflow: ellipsis; white-space: nowrap; font-variant-numeric: tabular-nums; }
/* 提高特异性压过 .eval-row > span，不用 !important */
.eval-row > span.eval-id { color: var(--text-regular); font-family: var(--font-mono); font-size: 11px; }
.eval-status {
  display: inline-block; padding: 1px 8px; border-radius: 999px; font-style: normal;
  background: var(--bg-sunken); color: var(--text-muted); font-size: 10.5px; font-weight: 650;
}
.eval-status.done { background: var(--success-bg); color: var(--success-text); }
.eval-status.running { background: var(--primary-light); color: var(--primary); }
.eval-status.failed { background: var(--error-bg); color: var(--error-text); }
.eval-score { font-weight: 700; }
.eval-score.good { color: var(--success-text); }
.eval-score.warn { color: var(--warning-text); }
.eval-score.bad { color: var(--error-text); }
.eval-only-wrong {
  margin-left: auto; display: inline-flex; align-items: center; gap: 6px; cursor: pointer;
  color: var(--text-regular); font-size: 12px; white-space: nowrap;
}
.eval-only-wrong input { width: 14px; height: 14px; accent-color: var(--primary); }
.eval-only-wrong span { margin: 0; }
.eval-summary { display: grid; grid-template-columns: repeat(5, minmax(0, auto)) minmax(0, 1fr); gap: 8px; margin-top: 12px; }
.eval-card { padding: 9px 12px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card); }
.eval-card > span { display: block; color: var(--text-faint); font-size: 10.5px; }
.eval-card > b { display: block; margin-top: 4px; color: var(--text-primary); font-size: 14px; font-variant-numeric: tabular-nums; }
.eval-card .eval-status { margin-top: 4px; }
.eval-card.wide { grid-column: span 2; }
.eval-card > b.eval-metrics { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 10px; font-size: 11.5px; font-weight: 500; color: var(--text-muted); }
.eval-metrics em { font-style: normal; font-weight: 700; color: var(--text-primary); }
.eval-metrics em.good { color: var(--success-text); }
.eval-metrics em.warn { color: var(--warning-text); }
.eval-metrics em.bad { color: var(--error-text); }
.eval-result-count { margin-top: 12px; color: var(--text-muted); font-size: 11.5px; }
.eval-row > span.eval-ord { color: var(--text-faint); }
.eval-row > span.eval-question, .eval-row > span.eval-answer { white-space: normal; display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical; line-height: 1.55; }
.eval-row > span.eval-question { color: var(--text-regular); }
.eval-row > span.eval-recall { display: flex; flex-wrap: wrap; gap: 2px 8px; white-space: normal; font-size: 10.5px; }
.eval-recall b { color: var(--primary); font-weight: 650; }
.eval-recall b.miss { color: var(--error-text); }
.eval-row > span.eval-verdict { display: flex; align-items: flex-start; gap: 7px; white-space: normal; }
.eval-verdict-badge {
  flex: none; padding: 1px 7px; border-radius: 999px; font-style: normal; font-size: 10.5px; font-weight: 650;
  background: var(--bg-sunken); color: var(--text-muted);
}
.eval-verdict-badge.correct { background: var(--success-bg); color: var(--success-text); }
.eval-verdict-badge.wrong { background: var(--error-bg); color: var(--error-text); }
.eval-verdict-badge.unknown { background: var(--bg-sunken); color: var(--text-muted); }
.eval-verdict em {
  min-width: 0; overflow: hidden; display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical;
  color: var(--text-muted); font-style: normal; font-size: 11px; line-height: 1.5;
}
@media (max-width: 900px) {
  .eval-head { align-items: stretch; flex-direction: column; gap: 10px; }
  .eval-new, .eval-only-wrong { margin-left: 0; }
  .eval-summary { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .eval-card.wide { grid-column: 1 / -1; }
  .eval-table-head { display: none; }
  .eval-table { border: 0; }
  .eval-row, .eval-row.report-row { grid-template-columns: 1fr 1fr; gap: 6px 12px; margin-bottom: 8px; border: 1px solid var(--border); }
  /* 窄屏表头隐藏后，用 data-label 伪类把列名补回单元格 */
  .eval-row > span[data-label]::before { content: attr(data-label); display: block; margin-bottom: 1px; color: var(--text-faint); font-size: 10px; font-weight: 600; }
}
</style>
