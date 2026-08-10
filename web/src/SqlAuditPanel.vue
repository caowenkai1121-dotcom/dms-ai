<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'

/** 【SQL 审计】`GET /api/audit/sql?status=&limit=100` 的只读抽屉。
 *  表格列：时间 / 用户 / 路由 / 状态 / 耗时 / SQL 摘要；行点击展开完整 SQL（含错误信息）。
 *  状态过滤：succeeded/blocked/failed/timeout，切换即重拉；全员只读 —— 没有任何写口。
 *  字段做宽容归一（items/rows/records、at/created_at、elapsed_ms/ms 都可），接口未上线/空体按内联提示处理。
 *  Esc/遮罩关闭；401 交回父组件走会话过期。抽屉形态与 App.vue 的 Trace 抽屉同款。 */
interface CtxCard { kind: string; name?: string; chars: number }
interface CtxTrim { kind: string; dropped: number; kept: number; names?: string[] }
/** 【D7】本轮实际进 prompt 的上下文摘要（结构/尺寸/表名，无数据值）；老行/无摘要 = null */
interface CtxSummary { prompt_chars: number; cards: CtxCard[]; trimmed: CtxTrim[]; summary_used: boolean }
interface AuditRow {
  id: string; at: string; user: string; route: string; status: string
  ms: number | null; sql: string; error: string
  ctx: CtxSummary | null
}

const props = defineProps<{ token?: string; login?: string; routeLabels?: Record<string, string> }>()
const emit = defineEmits<{
  (e: 'close'): void
  (e: 'auth-expired'): void
}>()

const STATUS_LABELS: Record<string, string> = {
  succeeded: '成功', blocked: '已拦截', failed: '失败', timeout: '超时',
}

const loading = ref(true)
const error = ref('')
const rows = ref<AuditRow[]>([])
const statusFilter = ref('')
const expandedId = ref('')

function authTail(): string {
  return props.token ? '' : `&login_name=${encodeURIComponent(props.login ?? '')}`
}
function authHeaders(): Record<string, string> {
  return props.token ? { Authorization: `Bearer ${props.token}` } : {}
}
/** 先取 text 再试解析：端点未上线时 axum 兜底 404 是空体，直接 .json() 只会抛 SyntaxError。 */
async function errText(r: Response, fallback: string): Promise<string> {
  const raw = await r.text()
  let body: { error?: string } | null = null
  try { body = raw ? JSON.parse(raw) : null } catch { /* 非 JSON 按原文报 */ }
  return body?.error || raw.trim().slice(0, 200) || `${fallback}（HTTP ${r.status}）`
}

function normalize(raw: unknown): AuditRow[] {
  const root = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>
  const list = Array.isArray(raw) ? raw
    : Array.isArray(root.items) ? root.items
      : Array.isArray(root.rows) ? root.rows
        : Array.isArray(root.records) ? root.records
          : Array.isArray(root.entries) ? root.entries : []
  const out: AuditRow[] = []
  list.forEach((item, index) => {
    if (!item || typeof item !== 'object') return
    const r = item as Record<string, unknown>
    const ms = Number(r.elapsed_ms ?? r.ms ?? r.duration_ms ?? r.latency_ms ?? NaN)
    out.push({
      id: String(r.id ?? index),
      at: String(r.at ?? r.created_at ?? r.time ?? r.ts ?? ''),
      user: String(r.login_name ?? r.user ?? r.login ?? r.username ?? ''),
      route: String(r.route ?? ''),
      status: String(r.status ?? r.state ?? '').toLowerCase(),
      ms: Number.isFinite(ms) ? ms : null,
      sql: String(r.sql ?? r.query ?? r.statement ?? ''),
      error: String(r.error ?? r.message ?? ''),
      ctx: normCtx(r.context_summary),
    })
  })
  return out
}

function statusLabel(status: string): string {
  return STATUS_LABELS[status] ?? (status || '—')
}
/** 【D7】上下文摘要卡种的中文标签（键名是后端 JSON 的契约值，不许翻成别的字面值） */
const CTX_KIND_LABELS: Record<string, string> = {
  metric: '指标', dim: '维度', term: '术语', time: '时间', value_hint: '码值',
  domain_hit: '值域', elem: '元素', join: '关联', schema: '表结构', schema_counter: '关联表',
  pitfall: '教训', memory: '经验', fewshot: '样例', ds_background: '源背景',
}
function ctxKindLabel(kind: string): string {
  return CTX_KIND_LABELS[kind] ?? kind
}
/** context_summary 可能是解析好的对象（新 API）、JSON 文本（旧缓存）或 null（老行/无摘要）。 */
function normCtx(raw: unknown): CtxSummary | null {
  let v: unknown = raw
  if (typeof v === 'string') {
    if (!v.trim()) return null
    try { v = JSON.parse(v) } catch { return null }
  }
  if (!v || typeof v !== 'object' || Array.isArray(v)) return null
  const o = v as Record<string, unknown>
  const promptChars = Number(o.prompt_chars)
  if (!Number.isFinite(promptChars)) return null
  const list = (x: unknown): Record<string, unknown>[] =>
    Array.isArray(x) ? x.filter((i): i is Record<string, unknown> => !!i && typeof i === 'object') : []
  return {
    prompt_chars: promptChars,
    summary_used: o.summary_used === true,
    cards: list(o.cards).map(c => ({
      kind: String(c.kind ?? ''),
      name: typeof c.name === 'string' ? c.name : undefined,
      chars: Number(c.chars ?? 0),
    })),
    trimmed: list(o.trimmed).map(t => ({
      kind: String(t.kind ?? ''),
      dropped: Number(t.dropped ?? 0),
      kept: Number(t.kept ?? 0),
      names: Array.isArray(t.names) ? t.names.map(String) : undefined,
    })),
  }
}
function fmtMs(ms: number | null): string {
  if (ms == null) return '—'
  if (ms < 1000) return `${Math.round(ms)}ms`
  if (ms < 60000) return `${(ms / 1000).toFixed(ms < 10000 ? 1 : 0)}s`
  return `${Math.floor(ms / 60000)}m${Math.round((ms % 60000) / 1000)}s`
}
/** ISO 串压成「MM-DD HH:mm:ss」；不是这个格式就原样显示（title 里总有全文）。 */
function shortAt(at: string): string {
  const m = at.match(/^\d{4}-(\d{2}-\d{2})[T ](\d{2}:\d{2}:\d{2})/)
  return m ? `${m[1]} ${m[2]}` : at
}
function toggle(row: AuditRow) {
  expandedId.value = expandedId.value === row.id ? '' : row.id
}

async function load() {
  loading.value = true
  error.value = ''
  expandedId.value = ''
  try {
    const r = await fetch(
      `/api/audit/sql?status=${encodeURIComponent(statusFilter.value)}&limit=100${authTail()}`,
      { headers: authHeaders() },
    )
    if (r.status === 401) {
      emit('auth-expired')
      error.value = '登录已失效，请重新登录'
      return
    }
    if (!r.ok) {
      error.value = await errText(r, 'SQL 审计加载失败')
      return
    }
    rows.value = normalize(await r.json().catch(() => null))
  } catch (e) {
    error.value = `SQL 审计加载失败（网络）：${e}`
  } finally {
    loading.value = false
  }
}

function onEsc(e: KeyboardEvent) {
  if (e.key === 'Escape') emit('close')
}
onMounted(() => {
  void load()
  window.addEventListener('keydown', onEsc)
})
onBeforeUnmount(() => window.removeEventListener('keydown', onEsc))
</script>

<template>
  <div class="sa-mask" @click.self="emit('close')">
    <section class="sa-drawer" role="dialog" aria-modal="true" aria-labelledby="sa-title">
      <header class="sa-head">
        <div>
          <span class="sa-kicker">SQL 审计</span>
          <h2 id="sa-title">查询执行记录</h2>
          <p class="sa-sub">每次取数落一条：时间 / 用户 / 路由 / 状态 / 耗时 / SQL。只读，点行展开完整 SQL。</p>
        </div>
        <button type="button" class="sa-close" title="关闭" @click="emit('close')">✕</button>
      </header>

      <div class="sa-tools">
        <select v-model="statusFilter" aria-label="状态过滤" @change="load">
          <option value="">全部状态</option>
          <option value="succeeded">成功</option>
          <option value="blocked">已拦截</option>
          <option value="failed">失败</option>
          <option value="timeout">超时</option>
        </select>
        <button type="button" class="sa-btn" :disabled="loading" @click="load">刷新</button>
        <span v-if="!loading && !error" class="sa-count">{{ rows.length }} 条</span>
      </div>

      <div v-if="loading" class="sa-state"><span class="sa-spin"></span>审计记录加载中…</div>
      <div v-else-if="error" class="sa-state sa-error">{{ error }}</div>
      <div v-else-if="!rows.length" class="sa-state">暂无审计记录</div>
      <div v-else class="sa-table-wrap">
        <table class="sa-table">
          <thead>
            <tr><th>时间</th><th>用户</th><th>路由</th><th>状态</th><th class="num">耗时</th><th>SQL 摘要</th></tr>
          </thead>
          <tbody>
            <template v-for="row in rows" :key="row.id">
              <tr class="sa-row" :class="{ on: expandedId === row.id }" @click="toggle(row)">
                <td class="sa-at" :title="row.at">{{ shortAt(row.at) || '—' }}</td>
                <td>{{ row.user || '—' }}</td>
                <td>{{ routeLabels?.[row.route] || row.route || '—' }}</td>
                <td><span class="sa-pill" :data-s="row.status">{{ statusLabel(row.status) }}</span></td>
                <td class="num">{{ fmtMs(row.ms) }}</td>
                <td class="sa-sql" :title="row.sql">{{ row.sql || '—' }}</td>
              </tr>
              <tr v-if="expandedId === row.id" class="sa-expand">
                <td colspan="6">
                  <pre class="sa-full">{{ row.sql || '（无 SQL 文本）' }}</pre>
                  <div v-if="row.error" class="sa-err">{{ row.error }}</div>
                  <details v-if="row.ctx" class="sa-ctx">
                    <summary>
                      本轮上下文 {{ row.ctx.prompt_chars }} 字节 · {{ row.ctx.cards.length }} 张卡
                      <template v-if="row.ctx.trimmed.length"> · 裁掉 {{ row.ctx.trimmed.length }} 项</template>
                      <template v-if="row.ctx.summary_used"> · 含历史摘要</template>
                    </summary>
                    <ul class="sa-ctx-list">
                      <li v-for="(c, i) in row.ctx.cards" :key="i">
                        {{ ctxKindLabel(c.kind) }}<template v-if="c.name">·{{ c.name }}</template>
                        <span class="sa-ctx-chars">{{ c.chars }}</span>
                      </li>
                      <li v-for="(t, i) in row.ctx.trimmed" :key="'t' + i" class="sa-ctx-trim">
                        裁掉 {{ ctxKindLabel(t.kind) }} ×{{ t.dropped }}（留 {{ t.kept }}）
                        <template v-if="t.names?.length">：{{ t.names.join('、') }}</template>
                      </li>
                    </ul>
                  </details>
                </td>
              </tr>
            </template>
          </tbody>
        </table>
      </div>
    </section>
  </div>
</template>

<style>
.sa-mask { position: fixed; inset: 0; z-index: 1100; background: rgba(17, 24, 39, .38); backdrop-filter: blur(5px); }
.sa-drawer { position: absolute; top: 0; right: 0; bottom: 0; width: min(880px, 96vw); display: flex; flex-direction: column; border-left: 1px solid var(--border); background: var(--bg-card); box-shadow: -18px 0 50px rgba(17, 24, 39, .18); }
.sa-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 20px; padding: 16px 18px 8px; }
.sa-kicker { display: block; margin-bottom: 4px; color: var(--primary); font-size: 11px; font-weight: 750; }
.sa-head h2 { margin: 0; color: var(--text-primary); font-size: 17px; font-weight: 700; }
.sa-sub { margin: 5px 0 0; color: var(--text-muted); font-size: 11.5px; line-height: 1.6; }
.sa-close { width: 30px; height: 30px; flex-shrink: 0; border: 0; border-radius: 5px; background: transparent; color: var(--text-muted); cursor: pointer; }
.sa-close:hover { background: var(--bg-hover); color: var(--text-primary); }

.sa-tools { display: flex; align-items: center; gap: 8px; padding: 2px 18px 10px; }
.sa-tools select { height: 30px; padding: 0 8px; border: 1px solid var(--border); border-radius: 6px; outline: 0; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: 12px; }
.sa-tools select:focus { border-color: var(--primary); box-shadow: var(--ring); }
.sa-btn { height: 30px; padding: 0 12px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card); color: var(--text-regular); font: inherit; font-size: 12px; cursor: pointer; }
.sa-btn:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.sa-btn:disabled { opacity: .5; cursor: default; }
.sa-count { color: var(--text-faint); font-size: 11px; font-variant-numeric: tabular-nums; }

.sa-state { display: flex; align-items: center; justify-content: center; gap: 9px; padding: 34px 22px; color: var(--text-muted); font-size: 13px; }
.sa-error { color: var(--error-text); line-height: 1.7; text-align: center; }
.sa-spin { width: 14px; height: 14px; border: 2px solid var(--primary); border-top-color: transparent; border-radius: 50%; animation: saSpin .7s linear infinite; }
@keyframes saSpin { to { transform: rotate(360deg); } }

.sa-table-wrap { flex: 1; min-height: 0; overflow: auto; padding: 0 18px 16px; }
.sa-table { width: 100%; border-collapse: collapse; font-size: 12px; }
.sa-table th { position: sticky; top: 0; z-index: 1; padding: 7px 8px; background: var(--bg-card); border-bottom: 1px solid var(--border); color: var(--text-faint); font-size: 11px; font-weight: 650; text-align: left; white-space: nowrap; }
.sa-table td { padding: 7px 8px; border-bottom: 1px solid var(--divider); color: var(--text-regular); vertical-align: top; }
.sa-table .num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
.sa-row { cursor: pointer; }
.sa-row:hover td { background: var(--bg-hover); }
.sa-row.on td { background: var(--primary-light); }
.sa-at { color: var(--text-muted); white-space: nowrap; font-variant-numeric: tabular-nums; }
.sa-pill { display: inline-block; padding: 1px 8px; border-radius: 999px; font-size: 10.5px; background: var(--bg-sunken); color: var(--text-muted); white-space: nowrap; }
.sa-pill[data-s='succeeded'] { background: var(--success-bg); color: var(--success-text); }
.sa-pill[data-s='blocked'] { background: var(--warning-bg); color: var(--warning-text); }
.sa-pill[data-s='failed'] { background: var(--error-bg); color: var(--error-text); }
.sa-pill[data-s='timeout'] { background: var(--primary-light); color: var(--primary); }
.sa-sql { max-width: 320px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text-muted); font-family: var(--font-mono); font-size: 11px; }
.sa-expand td { background: var(--bg-main); }
.sa-full { margin: 2px 0 4px; padding: 9px 11px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card); color: var(--text-regular); font-family: var(--font-mono); font-size: 11.5px; line-height: 1.7; white-space: pre-wrap; word-break: break-all; }
.sa-err { margin-top: 4px; color: var(--error-text); font-size: 11.5px; line-height: 1.6; white-space: pre-wrap; word-break: break-all; }
.sa-ctx { margin-top: 6px; }
.sa-ctx summary { cursor: pointer; color: var(--text-muted); font-size: 11.5px; }
.sa-ctx summary:hover { color: var(--primary); }
.sa-ctx-list { margin: 6px 0 2px; padding-left: 18px; max-height: 180px; overflow: auto; color: var(--text-regular); font-size: 11.5px; line-height: 1.8; }
.sa-ctx-chars { margin-left: 6px; color: var(--text-faint); font-variant-numeric: tabular-nums; }
.sa-ctx-trim { color: var(--warning-text); }
</style>
