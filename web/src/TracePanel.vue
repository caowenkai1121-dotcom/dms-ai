<script setup lang="ts">
import { computed, ref } from 'vue'

/** 【Trace 时间线】会话问答过程回放面板（A10，DataFoundry trace-dag 对应物）。
 * 竖向时间线：每轮问答一组节点 —— 提问 → 路由链 →（重试）→ 回答 →（产物），
 * 节点带类型标记 + 标签 + 耗时；回答节点的 SQL 可点击展开/收起。
 *
 * ■ 数据契约（本组件零请求逻辑，props 即全部输入）：
 * 挂载方（App.vue）在选中会话后请求
 *   GET /api/chat/conv/{id}/trace          （带会话 token；回退 login_name 见 api.ts 惯例）
 * 把响应 JSON 的 `rounds` 字段原样传给本组件：
 *   rounds: TraceRound[]                   （按时间升序，后端已排好）
 *   TraceRound = { msg_id: number|null, question: string, at: string /* RFC3339 *​/,
 *                  status: 'succeeded'|'interrupted'|'failed'|'timeout'|'blocked',
 *                  route: string, elapsed_ms: number|null, events: TraceEvent[] }
 *   TraceEvent 是 tag=kind 的判别联合（五值）：
 *     { kind:'question', at: string, text: string }
 *     { kind:'route',    stage: string, result:'hit'|'miss'|'skip', ms: number }
 *     { kind:'retry',    reason:'repair'|'failed'|'timeout'|'blocked', ms: number|null, error: string }
 *     { kind:'answer',   route: string, ms: number|null, sql: string|null, row_count: number|null }
 *     { kind:'artifact', id: number, title: string, preview_url: string }
 * 空数组 = 会话还没有可回放的问答，组件显示空态。加载中/失败由挂载方自己挡（v-if）。 */
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
const props = defineProps<{ rounds: TraceRound[] }>()

const roundList = computed(() => props.rounds ?? [])

/** 路由成员的展示名（agent router 表标签 → 中文）；未收录的原样显示（新路由不加这里也能看） */
const STAGE_LABEL: Record<string, string> = {
  'semantic-cache': '语义缓存',
  'direct-agg': '聚合快路径',
  'direct-doc': '单据快路径',
  'entity-card': '实体卡',
  'business-lookup': '单据点查',
  graph: '图谱',
  llm: 'LLM 生成',
  knowledge: '知识库',
}
const RESULT_LABEL: Record<string, string> = { hit: '命中', miss: '未命中', skip: '跳过' }
const REASON_LABEL: Record<string, string> = {
  repair: 'SQL 自修重试',
  failed: '执行失败',
  timeout: '取数超时',
  blocked: '被权限/红线拦截',
}
const STATUS_LABEL: Record<string, string> = {
  succeeded: '成功',
  interrupted: '已中断',
  failed: '失败',
  timeout: '超时',
  blocked: '被拦截',
}
/** 节点类型标记（单字徽标，与状态色配合 —— 本仓组件不引图标库） */
const KIND_BADGE: Record<TraceEvent['kind'], string> = {
  question: '问',
  route: '路',
  retry: '试',
  answer: '答',
  artifact: '物',
}

/** 展开的 SQL（按「轮下标」记）：点回答节点开合 */
const expanded = ref<Set<number>>(new Set())
function toggleSql(ri: number) {
  const next = new Set(expanded.value)
  if (next.has(ri)) next.delete(ri)
  else next.add(ri)
  expanded.value = next
}

function fmtMs(ms?: number | null): string {
  if (ms == null) return ''
  return ms < 1000 ? `${ms}ms` : `${(ms / 1000).toFixed(1)}s`
}
/** RFC3339 → 本地 HH:MM:SS（解析不了就显示原串，不猜时区） */
function fmtAt(at?: string): string {
  if (!at) return ''
  const d = new Date(at)
  if (Number.isNaN(d.getTime())) return at
  return d.toLocaleTimeString('zh-CN', { hour12: false })
}
function nodeTitle(ev: TraceEvent): string {
  switch (ev.kind) {
    case 'question': return '用户提问'
    case 'route': return `${STAGE_LABEL[ev.stage ?? ''] ?? ev.stage} · ${RESULT_LABEL[ev.result ?? ''] ?? ev.result}`
    case 'retry': return REASON_LABEL[ev.reason ?? ''] ?? `重试（${ev.reason}）`
    case 'answer': return 'AI 回答'
    case 'artifact': return '产物生成'
  }
}
/** 节点状态色：hit/成功绿、miss 灰、skip 更淡、retry/interrupted 红、repair 黄 */
function nodeTone(ev: TraceEvent): string {
  if (ev.kind === 'route') return ev.result === 'hit' ? 'ok' : ev.result === 'miss' ? 'dim' : 'faint'
  if (ev.kind === 'retry') return ev.reason === 'repair' ? 'warn' : 'bad'
  if (ev.kind === 'answer') return 'ok'
  if (ev.kind === 'artifact') return 'art'
  return 'dim'
}
function roundTone(r: TraceRound): string {
  return r.status === 'succeeded' ? '' : 'bad'
}
</script>

<template>
  <aside class="trace-panel">
    <div class="tl-hd">
      <span class="tl-title">Trace 时间线</span>
      <span class="tl-count">{{ roundList.length }} 轮</span>
    </div>
    <div v-if="!roundList.length" class="tl-empty">该会话还没有可回放的问答记录</div>

    <div v-for="(r, ri) in roundList" :key="r.msg_id ?? `x${ri}`" class="tl-round">
      <!-- 轮头：问句 + 状态 + 整轮耗时 -->
      <div class="tl-round-hd" :class="roundTone(r)">
        <span class="tl-q" :title="r.question">{{ r.question || '（无问句）' }}</span>
        <span class="tl-status">{{ STATUS_LABEL[r.status] ?? r.status }}</span>
        <span v-if="r.elapsed_ms != null" class="tl-ms">{{ fmtMs(r.elapsed_ms) }}</span>
      </div>
      <div class="tl-time">{{ fmtAt(r.at) }}</div>

      <!-- 事件节点：竖向时间线（左边框即时间轴） -->
      <div class="tl-nodes">
        <div
          v-for="(ev, ei) in r.events" :key="ei"
          class="tl-node" :class="[ev.kind, nodeTone(ev), { clickable: ev.kind === 'answer' && ev.sql }]"
          @click="ev.kind === 'answer' && ev.sql ? toggleSql(ri) : undefined"
        >
          <span class="tl-badge">{{ KIND_BADGE[ev.kind] }}</span>
          <div class="tl-body">
            <div class="tl-line">
              <span class="tl-label">{{ nodeTitle(ev) }}</span>
              <span v-if="ev.ms != null" class="tl-ms">{{ fmtMs(ev.ms) }}</span>
            </div>
            <div v-if="ev.kind === 'question'" class="tl-detail">{{ ev.text }}</div>
            <div v-else-if="ev.kind === 'retry' && ev.error" class="tl-detail bad" :title="ev.error">{{ ev.error }}</div>
            <div v-else-if="ev.kind === 'answer'" class="tl-detail">
              <template v-if="ev.row_count != null">{{ ev.row_count }} 行</template>
              <template v-if="ev.route"> · {{ ev.route }}</template>
              <template v-if="ev.sql"> · SQL {{ expanded.has(ri) ? '▾' : '▸' }}</template>
            </div>
            <div v-else-if="ev.kind === 'artifact'" class="tl-detail">
              <a v-if="ev.preview_url" :href="ev.preview_url" target="_blank" rel="noopener" @click.stop>{{ ev.title }}</a>
              <template v-else>{{ ev.title }}</template>
            </div>
            <pre v-if="ev.kind === 'answer' && ev.sql && expanded.has(ri)" class="tl-sql">{{ ev.sql }}</pre>
          </div>
        </div>
      </div>
    </div>
  </aside>
</template>

<style scoped>
.trace-panel { width: 300px; flex-shrink: 0; display: flex; flex-direction: column; gap: 12px; padding: 14px 12px; border-left: 1px solid var(--border); background: var(--bg-card); overflow-y: auto; min-height: 0; }
.tl-hd { display: flex; align-items: baseline; gap: 8px; }
.tl-title { color: var(--text-primary); font-size: 13px; font-weight: 700; }
.tl-count { flex: 1; color: var(--text-muted); font-size: 11px; font-variant-numeric: tabular-nums; }
.tl-empty { color: var(--text-muted); font-size: 12px; line-height: 1.6; }

.tl-round { display: grid; gap: 3px; }
.tl-round-hd { display: flex; align-items: baseline; gap: 6px; }
.tl-round-hd.bad .tl-q, .tl-round-hd.bad .tl-status { color: var(--error-text); }
.tl-q { flex: 1; min-width: 0; color: var(--text-primary); font-size: 12.5px; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tl-status { flex-shrink: 0; color: var(--text-muted); font-size: 10.5px; }
.tl-ms { flex-shrink: 0; color: var(--text-muted); font-size: 10.5px; font-variant-numeric: tabular-nums; }
.tl-time { color: var(--text-faint); font-size: 10.5px; font-variant-numeric: tabular-nums; }

.tl-nodes { display: grid; gap: 6px; margin: 4px 0 2px 9px; padding-left: 12px; border-left: 2px solid var(--border); }
.tl-node { display: flex; gap: 8px; align-items: flex-start; }
.tl-node.clickable { cursor: pointer; }
.tl-badge { position: relative; left: -19px; width: 14px; height: 14px; flex-shrink: 0; border-radius: 50%; display: inline-flex; align-items: center; justify-content: center; font-size: 9px; line-height: 1; color: var(--bg-card); background: var(--text-faint); }
.tl-node.ok .tl-badge { background: var(--success-text); }
.tl-node.dim .tl-badge { background: var(--text-muted); }
.tl-node.faint .tl-badge { background: var(--text-faint); }
.tl-node.warn .tl-badge { background: var(--warning-text, #b7791f); }
.tl-node.bad .tl-badge { background: var(--error-text); }
.tl-node.art .tl-badge { background: var(--primary); }
.tl-body { flex: 1; min-width: 0; margin-left: -6px; }
.tl-line { display: flex; align-items: baseline; gap: 6px; }
.tl-label { flex: 1; min-width: 0; color: var(--text-regular); font-size: 12px; line-height: 1.5; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tl-node.ok .tl-label { color: var(--text-primary); }
.tl-detail { color: var(--text-muted); font-size: 11px; line-height: 1.5; word-break: break-all; }
.tl-detail.bad { color: var(--error-text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tl-detail a { color: var(--primary); text-decoration: none; }
.tl-detail a:hover { text-decoration: underline; }
.tl-sql { margin: 4px 0 0; padding: 6px 8px; border: 1px solid var(--border); border-radius: var(--radius-md); background: var(--bg-sunken); color: var(--text-regular); font-size: 10.5px; line-height: 1.5; white-space: pre-wrap; word-break: break-all; max-height: 180px; overflow-y: auto; }

@media (max-width: 1100px) {
  .trace-panel { display: none; }
}
</style>
