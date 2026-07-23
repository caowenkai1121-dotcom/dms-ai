<script setup lang="ts">
import { ref, onMounted, nextTick } from 'vue'
import BiChart from './BiChart.vue'
import { fmt, type Semantic } from './format'

interface ColSpec { name: string; role: string; semantic: Semantic }
interface Delta { pct: number; dir: 'up' | 'down' | 'flat'; label: string }
interface Kpi { label: string; value: unknown; semantic: Semantic; delta?: Delta }
interface Block {
  type: 'kpis' | 'entity' | 'chart' | 'table'
  items?: Kpi[]
  pairs?: [string, unknown][]
  kind?: 'bar' | 'line' | 'pie'
  x?: number; y?: number[]; top?: number | null
}
interface Interact { drill?: string[] }
interface ViewSpec { columns: ColSpec[]; blocks: Block[]; interact?: Interact }
interface AskResult {
  sql: string; columns: string[]; rows: unknown[][]; row_count: number
  truncated: boolean; elapsed_ms: number; route: string; view: ViewSpec
}
// 一次问答
interface Turn {
  role: 'user' | 'ai'
  question?: string
  result?: AskResult
  error?: string
  loading?: boolean
  showSql?: boolean
}

const routeLabel: Record<string, string> = {
  'direct-doc': '单号直查', 'direct-agg': '快速聚合', graph: '图关系',
  llm: 'AI 生成', 'llm+repair': 'AI 生成·自修',
}
const QUICK = ['本月销售额是多少', '本月销售额前五的省份', '买过烤肠的客户有哪些', '查一下昨天的订单明细', '各区域经理业绩']

const question = ref('')
const loginName = ref('admin')
const roleCode = ref('')
const sessionToken = ref('')
const embedded = ref(false)
const turns = ref<Turn[]>([])
const chatEl = ref<HTMLElement>()
const health = ref('检查中…')
const healthOk = ref(false)
const theme = ref(localStorage.getItem('theme') || 'light')

function applyTheme() {
  document.documentElement.setAttribute('data-theme', theme.value)
}
function toggleTheme() {
  theme.value = theme.value === 'dark' ? 'light' : 'dark'
  localStorage.setItem('theme', theme.value)
  applyTheme()
}

onMounted(() => {
  applyTheme()
  // 端#3 企微：/#token=xxx
  const tm = location.hash.match(/token=([^&]+)/)
  if (tm) {
    sessionToken.value = tm[1]; embedded.value = true; loginName.value = '企微用户'
    history.replaceState(null, '', location.pathname)
  }
  checkHealth()
})

// 端#2 DMS 嵌入：URL dms_token → SSO
onMounted(async () => {
  const p = new URLSearchParams(location.search)
  const dmsToken = p.get('dms_token')
  if (!dmsToken) return
  embedded.value = true
  try {
    const resp = await fetch('/api/sso', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ dms_token: dmsToken, role_code: p.get('role') || null }),
    })
    const d = await resp.json()
    if (resp.ok) { sessionToken.value = d.token; loginName.value = d.login_name }
    else pushError(`SSO 认证失败：${d.error || ''}`)
  } catch (e) { pushError(`SSO 认证失败：${e}`) }
})

async function checkHealth() {
  try {
    const h = await (await fetch('/api/health')).json()
    healthOk.value = h.ok
    health.value = h.ok ? '服务正常 · 生产库只读' : '服务异常'
  } catch { healthOk.value = false; health.value = '后端未连接' }
}

function pushError(msg: string) {
  turns.value.push({ role: 'ai', error: msg })
  scrollDown()
}

async function scrollDown() {
  await nextTick()
  chatEl.value?.scrollTo({ top: chatEl.value.scrollHeight, behavior: 'smooth' })
}

function newSession() {
  turns.value = []
  question.value = ''
}

async function send(q?: string) {
  const text = (q ?? question.value).trim()
  if (!text) return
  const ai = turns.value.find((t) => t.loading)
  if (ai) return // 有进行中的问答
  turns.value.push({ role: 'user', question: text })
  turns.value.push({ role: 'ai', loading: true })
  // 取数组里的 reactive 代理（深响应式下改原始引用不触发更新）
  const aiTurn = turns.value[turns.value.length - 1]
  question.value = ''
  scrollDown()
  const ctrl = new AbortController()
  const timer = setTimeout(() => ctrl.abort(), 100000) // 100s 兜底，防 LLM 挂起永久 loading
  try {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' }
    if (sessionToken.value) headers.Authorization = `Bearer ${sessionToken.value}`
    const resp = await fetch('/api/ask', {
      method: 'POST', headers, signal: ctrl.signal,
      body: JSON.stringify({
        question: text,
        login_name: sessionToken.value ? null : loginName.value,
        role_code: roleCode.value || null,
      }),
    })
    const data = await resp.json()
    if (!resp.ok) aiTurn.error = data.error || '请求失败'
    else aiTurn.result = data
  } catch (e) {
    aiTurn.error = ctrl.signal.aborted ? '查询超时（>100s），请重试或换个问法' : String(e)
  } finally {
    clearTimeout(timer)
    aiTurn.loading = false
    scrollDown()
  }
}

function drill(dim: string, baseQuestion: string) {
  send(`${baseQuestion} 按${dim}`)
}

function onKey(e: KeyboardEvent) {
  if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send() }
}

// 表格：指标列右对齐 + 语义格式化
function cell(t: Turn, ri: number, ci: number): string {
  const v = t.result!.rows[ri][ci]
  return fmt(v, t.result!.view.columns[ci]?.semantic ?? 'none')
}
function isMetric(t: Turn, ci: number): boolean {
  return t.result!.view.columns[ci]?.role === 'metric'
}
</script>

<template>
  <div class="wrap">
    <!-- 侧栏 -->
    <aside class="side">
      <div class="side-hd">
        <span class="logo">🐯 皇家小虎</span>
        <button class="btn-icon" @click="toggleTheme" :title="'明暗切换'">{{ theme === 'dark' ? '☀️' : '🌙' }}</button>
      </div>
      <div class="sec">
        <div class="sec-t">会话 <button class="btn-sm" @click="newSession">+ 新建</button></div>
      </div>
      <div class="hist">
        <div v-if="!turns.length" class="hist-empty">开始提问，会话记录显示在这里</div>
        <div v-for="(t, i) in turns.filter(x => x.role === 'user')" :key="i" class="hist-item">{{ t.question }}</div>
      </div>
      <div class="sec side-ft">
        <div class="health"><span class="dot" :class="{ ok: healthOk }"></span>{{ health }}</div>
        <div class="readonly">🔒 纯查询模式（无写操作）</div>
      </div>
    </aside>

    <!-- 主区 -->
    <div class="main">
      <div class="topbar">
        <div class="brand">数据智能<span class="sub">DMS · 自然语言取数</span></div>
        <div class="sp"></div>
        <template v-if="!embedded">
          <input v-model="loginName" class="mini-inp" placeholder="登录名" style="width: 110px" />
          <input v-model="roleCode" class="mini-inp" placeholder="角色(默认)" style="width: 120px" />
        </template>
        <span v-else class="dms-user">已登录 <b>{{ loginName || '认证中…' }}</b> · DMS 免登</span>
        <button class="btn-sm" @click="newSession">清空对话</button>
      </div>

      <div class="chat" ref="chatEl">
        <!-- 欢迎语 -->
        <div v-if="!turns.length" class="turn">
          <div class="bubble ai">
            嗷呜~ 我是 <b>皇家小虎 · 数据智能</b>。用自然语言查询任意数据——订单、客户、商品、库存、财务、活动、售后，<b>数据权限与你的 DMS 账号完全一致</b>。<br /><br />
            试试：<i>本月销售额</i> · <i>销售额前五省份</i> · <i>买过烤肠的客户</i> · <i>昨天的订单明细</i>
          </div>
        </div>

        <template v-for="(t, ti) in turns" :key="ti">
          <!-- 用户气泡 -->
          <div v-if="t.role === 'user'" class="turn">
            <div class="bubble user">{{ t.question }}</div>
          </div>
          <!-- AI 气泡 -->
          <div v-else class="turn">
            <div v-if="t.loading" class="thinking"><span class="spin"></span>分析中…</div>
            <div v-else-if="t.error" class="bubble err">{{ t.error }}</div>
            <div v-else-if="t.result" class="bubble ai">
              <div class="res-meta">
                <span class="route-badge">{{ routeLabel[t.result.route] || t.result.route }}</span>
                <span>{{ t.result.row_count }} 行{{ t.result.truncated ? '·截断200' : '' }} · {{ t.result.elapsed_ms }}ms</span>
                <a class="sql-toggle" @click="t.showSql = !t.showSql">{{ t.showSql ? '隐藏' : '查看' }} SQL</a>
              </div>
              <pre v-if="t.showSql" class="sql">{{ t.result.sql }}</pre>

              <template v-for="(b, bi) in t.result.view.blocks" :key="bi">
                <!-- KPI 卡 -->
                <div v-if="b.type === 'kpis'" class="kpi-row">
                  <div v-for="(k, ki) in b.items" :key="ki" class="metric-card">
                    <div class="mc-label">{{ k.label }}</div>
                    <div class="mc-val num">{{ fmt(k.value, k.semantic) }}</div>
                    <div v-if="k.delta" class="mc-delta" :class="k.delta.dir">
                      {{ k.delta.dir === 'up' ? '▲' : k.delta.dir === 'down' ? '▼' : '—' }}
                      {{ Math.abs(k.delta.pct) }}% <span class="mc-vs">{{ k.delta.label }}</span>
                    </div>
                  </div>
                </div>

                <!-- 实体卡 -->
                <div v-else-if="b.type === 'entity'" class="entity">
                  <div class="entity-hd">单据详情</div>
                  <div class="entity-grid">
                    <div v-for="(p, pi) in b.pairs" :key="pi" class="entity-cell">
                      <div class="ec-k">{{ p[0] }}</div>
                      <div class="ec-v">{{ p[1] }}</div>
                    </div>
                  </div>
                </div>

                <!-- 图表 -->
                <div v-else-if="b.type === 'chart'" class="chart-card">
                  <BiChart :kind="b.kind!" :columns="t.result.view.columns" :rows="t.result.rows" :x="b.x!" :y="b.y!" :top="b.top" />
                </div>

                <!-- 表格 -->
                <div v-else-if="b.type === 'table'" class="tbl-wrap">
                  <table>
                    <thead>
                      <tr>
                        <th v-for="(c, ci) in t.result.columns" :key="ci" :class="{ num: isMetric(t, ci) }">{{ c }}</th>
                      </tr>
                    </thead>
                    <tbody>
                      <tr v-for="(row, ri) in t.result.rows.slice(0, 100)" :key="ri">
                        <td v-for="(_, ci) in t.result.columns" :key="ci" :class="{ num: isMetric(t, ci) }">{{ cell(t, ri, ci) }}</td>
                      </tr>
                    </tbody>
                  </table>
                </div>
              </template>

              <!-- 下钻 chips -->
              <div v-if="t.result.view.interact?.drill?.length" class="drill">
                <span class="drill-t">换个维度看：</span>
                <span v-for="d in t.result.view.interact.drill" :key="d" class="pill" @click="drill(d, t.question || turns[ti - 1]?.question || '')">按{{ d }} ↓</span>
              </div>
            </div>
          </div>
        </template>
      </div>

      <!-- 快捷 pill -->
      <div class="quick">
        <span v-for="q in QUICK" :key="q" class="pill" @click="send(q)">{{ q }}</span>
      </div>

      <!-- 输入栏 -->
      <div class="inputbar">
        <textarea v-model="question" placeholder="用自然语言提问，Enter 发送，Shift+Enter 换行…" @keydown="onKey" rows="1"></textarea>
        <button class="send" :disabled="!question.trim()" @click="send()">发送</button>
      </div>
    </div>
  </div>
</template>

<style>
.wrap { display: flex; height: 100vh; min-height: 0; }
/* 侧栏 */
.side { width: 268px; flex-shrink: 0; border-right: 1px solid var(--border); background: var(--bg-card); display: flex; flex-direction: column; min-height: 0; }
.side-hd { padding: 16px; border-bottom: 1px solid var(--divider); display: flex; align-items: center; justify-content: space-between; }
.side-hd .logo { font-size: 16px; font-weight: 650; background: var(--brand-ink); -webkit-background-clip: text; background-clip: text; -webkit-text-fill-color: transparent; }
.sec { padding: 12px 16px; border-bottom: 1px solid var(--divider); }
.sec-t { font-size: 12px; font-weight: 600; color: var(--text-muted); text-transform: uppercase; letter-spacing: .4px; display: flex; align-items: center; justify-content: space-between; }
.hist { flex: 1; overflow-y: auto; padding: 8px 10px; min-height: 0; }
.hist-empty { font-size: 12px; color: var(--text-faint); padding: 8px; }
.hist-item { font-size: 13px; color: var(--text-regular); padding: 7px 10px; border-radius: var(--radius-md); cursor: default; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.hist-item:hover { background: var(--bg-hover); }
.side-ft { margin-top: auto; }
.health { font-size: 12px; color: var(--text-muted); display: flex; align-items: center; gap: 6px; }
.health .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--text-faint); }
.health .dot.ok { background: var(--success); }
.readonly { font-size: 11px; color: var(--text-faint); margin-top: 5px; }
/* 主区 */
.main { flex: 1; min-width: 0; display: flex; flex-direction: column; min-height: 0; }
.topbar { display: flex; align-items: center; gap: 8px; padding: 12px 16px; border-bottom: 1px solid var(--divider); background: var(--bg-card); }
.topbar .brand { font-weight: 650; font-size: 16px; color: var(--text-primary); display: flex; align-items: baseline; gap: 6px; }
.topbar .brand .sub { font-size: 12px; color: var(--text-muted); font-weight: 400; }
.topbar .sp { flex: 1; }
.dms-user { font-size: 12px; color: var(--text-muted); }
.mini-inp { height: 30px; padding: 0 10px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--bg-body); color: var(--text-regular); font-size: 13px; }
.mini-inp:focus { outline: none; border-color: var(--primary); box-shadow: var(--ring); }
/* 对话流 */
.chat { flex: 1; overflow-y: auto; padding: 20px 24px; min-height: 0; }
.turn { margin-bottom: 16px; display: flex; flex-direction: column; }
.bubble { max-width: 82%; padding: 12px 16px; font-size: 14px; line-height: 1.65; word-break: break-word; }
.bubble.user { margin-left: auto; width: fit-content; background: var(--primary); color: #fff; white-space: pre-wrap; border-radius: 12px 12px 4px 12px; }
.bubble.ai { margin-right: auto; width: fit-content; max-width: min(100%, 1120px); background: var(--bg-card); border: 1px solid var(--border); box-shadow: var(--shadow-sm); border-radius: 12px 12px 12px 4px; }
.bubble.err { margin-right: auto; background: var(--error-bg); border: 1px solid var(--error-ring); color: var(--error-text); border-radius: 12px; }
.thinking { display: inline-flex; align-items: center; gap: 10px; background: var(--bg-card); border: 1px solid var(--border); padding: 10px 14px; border-radius: 12px; font-size: 13px; color: var(--text-regular); box-shadow: var(--shadow-sm); width: fit-content; }
.spin { width: 14px; height: 14px; border: 2px solid var(--primary); border-top-color: transparent; border-radius: 50%; animation: dnSpin .7s linear infinite; }
@keyframes dnSpin { to { transform: rotate(360deg); } }
.res-meta { display: flex; align-items: center; gap: 10px; font-size: 12px; color: var(--text-muted); margin-bottom: 10px; }
.res-meta .route-badge { font-weight: 600; color: var(--primary); background: var(--primary-bg); padding: 1px 8px; border-radius: var(--radius-full); }
.res-meta .sql-toggle { margin-left: auto; cursor: pointer; color: var(--primary); }
.sql { background: var(--bg-main); border: 1px solid var(--divider); border-radius: var(--radius-lg); padding: 10px 12px; overflow-x: auto; margin-bottom: 10px; font-family: var(--font-mono); font-size: 12px; color: var(--text-regular); white-space: pre-wrap; }
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
.pill { font-size: 12px; padding: 4px 12px; border: 1px solid var(--border); border-radius: var(--radius-full); background: var(--bg-card); color: var(--text-muted); cursor: pointer; white-space: nowrap; transition: .12s; }
.pill:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-light); }
/* 快捷 pill */
.quick { display: flex; flex-wrap: wrap; gap: 6px; padding: 8px 16px; border-top: 1px solid var(--divider); background: var(--bg-card); }
/* 输入栏 */
.inputbar { display: flex; gap: 8px; align-items: flex-end; padding: 12px 16px; border-top: 1px solid var(--divider); background: var(--bg-card); }
.inputbar textarea { flex: 1; min-height: 42px; max-height: 160px; resize: none; padding: 10px 14px; border: 1px solid var(--border); border-radius: var(--radius-md); background: var(--bg-card); color: var(--text-regular); font-family: inherit; font-size: 14px; line-height: 1.55; }
.inputbar textarea:focus { border-color: var(--primary); box-shadow: var(--ring); outline: none; }
.send { flex: 0 0 auto; height: 42px; padding: 0 22px; background: var(--primary); color: #fff; border: 1px solid var(--primary); border-radius: var(--radius-md); font-size: 14px; font-weight: 600; cursor: pointer; }
.send:disabled { opacity: .55; cursor: not-allowed; }
/* 按钮 */
.btn-icon, .btn-sm { border: 1px solid var(--border); background: var(--bg-card); color: var(--text-regular); border-radius: var(--radius); cursor: pointer; font-size: 12px; }
.btn-icon { width: 30px; height: 30px; padding: 0; font-size: 15px; }
.btn-sm { height: 26px; padding: 0 10px; }
.btn-icon:hover, .btn-sm:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-light); }
@media (max-width: 820px) { .side { display: none; } .bubble { max-width: 94%; } }
</style>
