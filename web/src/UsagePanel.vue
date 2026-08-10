<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'

/** 【使用统计】`GET /api/usage/summary` 的只读弹窗：今日 / 累计 / 路由分布 / 近 7 天柱状图。
 *  柱状图是自绘 SVG（零依赖，与全站一致）；打开即拉一次，不轮询；401 交回父组件走会话过期。 */
interface UsageDay { date: string; count: number }
interface UsageRoute { route: string; count: number }
interface UsageSummary { today: number; total: number; routes: UsageRoute[]; days: UsageDay[] }

const props = defineProps<{ token?: string; login?: string; routeLabels?: Record<string, string> }>()
const emit = defineEmits<{
  (e: 'close'): void
  (e: 'auth-expired'): void
}>()

const loading = ref(true)
const error = ref('')
const summary = ref<UsageSummary | null>(null)

function authQuery(): string {
  return props.token ? '' : `?login_name=${encodeURIComponent(props.login ?? '')}`
}

/** 后端契约是 {today, total, routes: [{route, count}], days: [{date, count}]}；
 *  routes 兼容 map 形态，字段缺失一律落 0 —— 统计面板不能因为口径加减字段就白屏。 */
function normalize(raw: unknown): UsageSummary {
  const j = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>
  const num = (v: unknown): number => (typeof v === 'number' && Number.isFinite(v) ? v : 0)
  const routesRaw = j.routes ?? {}
  const routes: UsageRoute[] = (Array.isArray(routesRaw)
    ? routesRaw.map((r) => {
        const row = (r && typeof r === 'object' ? r : {}) as Record<string, unknown>
        return { route: String(row.route ?? ''), count: num(row.count) }
      })
    : Object.entries(routesRaw).map(([route, count]) => ({ route, count: num(count) }))
  ).filter((r) => r.route)
  routes.sort((a, b) => b.count - a.count)
  const daysRaw = Array.isArray(j.days) ? j.days : []
  const days: UsageDay[] = daysRaw.map((d) => {
    const row = (d && typeof d === 'object' ? d : {}) as Record<string, unknown>
    return { date: String(row.date ?? ''), count: num(row.count) }
  }).filter((d) => d.date)
  return { today: num(j.today), total: num(j.total), routes, days: days.slice(-7) }
}

async function load() {
  loading.value = true
  error.value = ''
  try {
    const r = await fetch(`/api/usage/summary${authQuery()}`, {
      headers: props.token ? { Authorization: `Bearer ${props.token}` } : {},
    })
    if (r.status === 401) {
      emit('auth-expired')
      error.value = '登录已失效，请重新登录'
      return
    }
    // 先取 text 再试解析：端点未上线时 axum 兜底 404 是空体，直接 .json() 只会抛 SyntaxError。
    const raw = await r.text()
    let body: unknown = null
    try { body = raw ? JSON.parse(raw) : null } catch { /* 非 JSON 按原文报 */ }
    if (!r.ok) {
      error.value = (body as { error?: string } | null)?.error || raw.trim().slice(0, 200) || `统计加载失败（HTTP ${r.status}）`
      return
    }
    summary.value = normalize(body)
  } catch (e) {
    error.value = `统计加载失败（网络）：${e}`
  } finally {
    loading.value = false
  }
}

const weekTotal = computed(() => (summary.value?.days ?? []).reduce((sum, d) => sum + d.count, 0))
const routeMax = computed(() => Math.max(1, ...(summary.value?.routes ?? []).map((r) => r.count)))

// 近 7 天柱状图：固定 viewBox，柱高按最大值归一；今天满透明度、其余半透明。
const CHART_W = 288
const CHART_H = 104
const PAD_T = 15
const PAD_B = 17
const pad2 = (n: number) => String(n).padStart(2, '0')
const now = new Date()
const todayStr = `${now.getFullYear()}-${pad2(now.getMonth() + 1)}-${pad2(now.getDate())}`
const bars = computed(() => {
  const days = summary.value?.days ?? []
  const max = Math.max(1, ...days.map((d) => d.count))
  const innerH = CHART_H - PAD_T - PAD_B
  const step = CHART_W / Math.max(days.length, 1)
  const w = Math.min(26, Math.round(step * 0.56))
  return days.map((d, i) => {
    const h = d.count > 0 ? Math.max(3, Math.round((d.count / max) * innerH)) : 0
    return { ...d, x: Math.round(i * step + (step - w) / 2), y: CHART_H - PAD_B - h, w, h }
  })
})

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
  <div class="up-mask" @click.self="emit('close')">
    <section class="up-dialog" role="dialog" aria-modal="true" aria-labelledby="up-title">
      <header class="up-head">
        <div>
          <span class="up-kicker">使用统计</span>
          <h2 id="up-title">我的提问用量</h2>
        </div>
        <button type="button" class="up-close" title="关闭" @click="emit('close')">✕</button>
      </header>
      <div v-if="loading" class="up-state"><span class="up-spin"></span>统计加载中…</div>
      <div v-else-if="error" class="up-state up-error">{{ error }}</div>
      <template v-else-if="summary">
        <div class="up-kpis">
          <div><span>今日提问</span><b>{{ summary.today }}</b></div>
          <div><span>累计提问</span><b>{{ summary.total }}</b></div>
          <div><span>近 7 天</span><b>{{ weekTotal }}</b></div>
        </div>
        <div class="up-sec-t">近 7 天</div>
        <div v-if="!summary.days.length" class="up-note">近 7 天暂无提问记录</div>
        <svg v-else class="up-chart" :viewBox="`0 0 ${CHART_W} ${CHART_H}`" role="img" aria-label="近 7 天提问数柱状图">
          <line class="up-axis" x1="0" :y1="CHART_H - PAD_B" :x2="CHART_W" :y2="CHART_H - PAD_B" />
          <g v-for="b in bars" :key="b.date">
            <rect class="up-bar" :class="{ today: b.date === todayStr }" :x="b.x" :y="b.y" :width="b.w" :height="b.h" rx="3" />
            <text v-if="b.count" class="up-val" :x="b.x + b.w / 2" :y="b.y - 3">{{ b.count }}</text>
            <text class="up-day" :x="b.x + b.w / 2" :y="CHART_H - 5">{{ b.date === todayStr ? '今天' : b.date.slice(5) }}</text>
          </g>
        </svg>
        <div class="up-sec-t">路由分布</div>
        <div v-if="!summary.routes.length" class="up-note">暂无路由数据</div>
        <div v-for="r in summary.routes" :key="r.route" class="up-route">
          <span class="up-route-name" :title="r.route">{{ routeLabels?.[r.route] || r.route }}</span>
          <span class="up-route-bar"><i :style="{ width: `${Math.round((r.count / routeMax) * 100)}%` }"></i></span>
          <b>{{ r.count }}</b>
        </div>
      </template>
    </section>
  </div>
</template>

<style>
.up-mask { position: fixed; inset: 0; z-index: 1100; display: grid; place-items: center; padding: 20px; background: rgba(17, 24, 39, .38); backdrop-filter: blur(5px); }
.up-dialog { width: min(520px, 100%); max-height: 86vh; overflow-y: auto; border: 1px solid var(--border); border-radius: 8px; background: var(--bg-card); box-shadow: 0 24px 70px rgba(17, 24, 39, .2); padding-bottom: 20px; }
.up-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 20px; padding: 20px 22px 12px; }
.up-kicker { display: block; margin-bottom: 5px; color: var(--primary); font-size: 11px; font-weight: 750; }
.up-head h2 { margin: 0; color: var(--text-primary); font-size: 18px; font-weight: 700; }
.up-close { width: 30px; height: 30px; border: 0; border-radius: 5px; background: transparent; color: var(--text-muted); cursor: pointer; }
.up-close:hover { background: var(--bg-hover); color: var(--text-primary); }
.up-state { display: flex; align-items: center; justify-content: center; gap: 9px; padding: 34px 22px; color: var(--text-muted); font-size: 13px; }
.up-error { color: var(--error-text); line-height: 1.7; text-align: center; }
.up-spin { width: 14px; height: 14px; border: 2px solid var(--primary); border-top-color: transparent; border-radius: 50%; animation: upSpin .7s linear infinite; }
@keyframes upSpin { to { transform: rotate(360deg); } }
.up-kpis { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px; padding: 4px 22px 14px; }
.up-kpis > div { min-width: 0; padding: 10px 12px; border: 1px solid var(--border); background: var(--bg-main); border-radius: 7px; display: flex; flex-direction: column; gap: 3px; }
.up-kpis span { font-size: 11px; color: var(--text-muted); }
.up-kpis b { color: var(--text-primary); font-size: 20px; font-variant-numeric: tabular-nums; }
.up-sec-t { padding: 4px 22px 8px; font-size: 12px; font-weight: 650; color: var(--text-primary); }
.up-note { margin: 0 22px 12px; font-size: 12px; color: var(--text-faint); }
.up-chart { display: block; width: calc(100% - 44px); margin: 0 22px 10px; }
.up-axis { stroke: var(--divider); stroke-width: 1; }
.up-bar { fill: var(--primary); opacity: .45; }
.up-bar.today { opacity: 1; }
.up-val { fill: var(--text-muted); font-size: 9px; text-anchor: middle; }
.up-day { fill: var(--text-faint); font-size: 9px; text-anchor: middle; }
.up-route { display: flex; align-items: center; gap: 10px; margin: 0 22px; padding: 5px 0; font-size: 12px; }
.up-route-name { width: 88px; flex-shrink: 0; color: var(--text-regular); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.up-route-bar { flex: 1; height: 8px; border-radius: 4px; background: var(--bg-main); overflow: hidden; }
.up-route-bar i { display: block; height: 100%; border-radius: 4px; background: var(--primary); opacity: .75; }
.up-route b { width: 36px; text-align: right; color: var(--text-primary); font-variant-numeric: tabular-nums; }
</style>
