<script setup lang="ts">
import { computed, ref, watch } from 'vue'

/** 【子任务面板】深度模式右侧面板：阶段进度条（思维过程同一份 stages）
 *  + 板块子任务卡片（标题 + 状态点 + 耗时）。数据全部来自 `/api/deep/progress`
 *  的 1.2s 轮询（App.vue startProgress 写到 turn 上），本组件不自己发请求。 */
interface DeepSectionTask {
  title: string
  state: 'queued' | 'running' | 'done' | 'failed'
  ms?: number
  /** 【D8】板块验收断言（规划透出；老服务端不带此键 = 不显示） */
  assertion?: string
}
const props = defineProps<{
  turn: {
    loading?: boolean
    elapsed?: number
    error?: string
    progress?: string[]
    tasks?: DeepSectionTask[]
  }
}>()

/** 与 deep_api ProgressStage 同一条固定脱敏管线（仅用于进度条比例与当前阶段定位）。 */
const STAGE_ORDER = ['检索知识库', '执行主查询', '规划分析板块', '查询关联数据', '整理经营明细', '计算同期对比', '生成 BI 报告', '生成经营分析', '完成']
const STATE_LABEL: Record<DeepSectionTask['state'], string> = {
  queued: '入列',
  running: '执行中',
  done: '完成',
  failed: '失败',
}

const stages = computed(() => props.turn.progress ?? [])
const tasks = computed(() => props.turn.tasks ?? [])
const running = computed(() => !!props.turn.loading)
const failed = computed(() => !!props.turn.error || stages.value.includes('处理失败'))
const tasksDone = computed(() => tasks.value.filter((task) => task.state === 'done').length)
const tasksFailed = computed(() => tasks.value.filter((task) => task.state === 'failed').length)

const stageIndex = computed(() =>
  stages.value.reduce((hit, stage) => Math.max(hit, STAGE_ORDER.indexOf(stage)), -1)
)
const percent = computed(() => {
  if (!running.value) return 100
  if (stageIndex.value < 0) return 6
  return Math.min(96, Math.round(((stageIndex.value + 1) / STAGE_ORDER.length) * 100))
})
const headState = computed(() => {
  if (running.value) return `执行中 · ${props.turn.elapsed ?? 0}s`
  return failed.value ? '已中断' : '已完成'
})

/** 全部完成（或中断）后自动折叠成一行摘要；进行中始终展开，也可手动开合。 */
const collapsed = ref(false)
watch(
  () => props.turn.loading,
  (now, before) => {
    if (now) collapsed.value = false
    else if (before) collapsed.value = true
  }
)

function fmtMs(ms: number): string {
  return ms < 1000 ? `${ms}ms` : `${(ms / 1000).toFixed(1)}s`
}
</script>

<template>
  <aside class="task-panel" :class="{ collapsed }">
    <div class="tp-hd" @click="collapsed = !collapsed">
      <span class="tp-title">深度分析</span>
      <span class="tp-state" :class="{ bad: failed && !running }">{{ headState }}</span>
      <span class="tp-fold">{{ collapsed ? '▸' : '▾' }}</span>
    </div>

    <template v-if="!collapsed">
      <!-- 阶段进度条：stages 与聊天气泡「思维过程」是同一份轮询数据 -->
      <div class="tp-bar"><i :style="{ width: percent + '%' }" :class="{ bad: failed && !running }"></i></div>
      <div class="tp-stages">
        <div
          v-for="(s, i) in stages" :key="`${i}:${s}`" class="tp-stage"
          :class="{ current: running && i === stages.length - 1, bad: s === '处理失败' }"
        >
          <span class="tp-dot"></span><span>{{ s }}</span>
        </div>
        <div v-if="!stages.length" class="tp-stage current"><span class="tp-dot"></span><span>理解问题与业务口径</span></div>
      </div>

      <!-- 板块子任务卡片 -->
      <div v-if="tasks.length" class="tp-tasks">
        <div class="tp-sec-t">
          子任务
          <b>{{ tasksDone }}/{{ tasks.length }}</b>
          <em v-if="tasksFailed">{{ tasksFailed }} 失败</em>
        </div>
        <div v-for="task in tasks" :key="task.title" class="tp-task" :class="task.state">
          <span class="tp-task-dot"></span>
          <span class="tp-task-body">
            <span class="tp-task-title" :title="task.title">{{ task.title }}</span>
            <!-- 【D8】验收断言前置透出：板块还没跑完，用户已看到它要证明什么 -->
            <span v-if="task.assertion" class="tp-task-acc" :title="task.assertion">验收：{{ task.assertion }}</span>
          </span>
          <span class="tp-task-meta">{{ STATE_LABEL[task.state] ?? task.state }}<template v-if="task.ms != null"> · {{ fmtMs(task.ms) }}</template></span>
        </div>
      </div>
    </template>
    <div v-else class="tp-summary">
      {{ failed ? '分析中断' : '分析完成' }}<template v-if="tasks.length"> · 板块 {{ tasksDone }}/{{ tasks.length }}<template v-if="tasksFailed">（{{ tasksFailed }} 失败）</template></template>
    </div>
  </aside>
</template>

<style scoped>
.task-panel { width: 264px; flex-shrink: 0; display: flex; flex-direction: column; gap: 10px; padding: 14px 12px; border-left: 1px solid var(--border); background: var(--bg-card); overflow-y: auto; min-height: 0; }
.tp-hd { display: flex; align-items: baseline; gap: 8px; cursor: pointer; user-select: none; }
.tp-title { color: var(--text-primary); font-size: 13px; font-weight: 700; }
.tp-state { flex: 1; min-width: 0; color: var(--text-muted); font-size: 11px; font-variant-numeric: tabular-nums; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tp-state.bad { color: var(--error-text); }
.tp-fold { color: var(--text-faint); font-size: 11px; }

.tp-bar { height: 4px; border-radius: var(--radius-full); background: var(--bg-sunken); overflow: hidden; }
.tp-bar i { display: block; height: 100%; border-radius: var(--radius-full); background: var(--primary); transition: width .5s ease; }
.tp-bar i.bad { background: var(--error-text); }

.tp-stages { display: grid; gap: 5px; }
.tp-stage { display: flex; align-items: center; gap: 7px; color: var(--text-muted); font-size: 12px; line-height: 1.4; }
.tp-stage .tp-dot { width: 6px; height: 6px; flex-shrink: 0; border-radius: 50%; background: var(--success-text); }
.tp-stage.current { color: var(--text-primary); font-weight: 600; }
.tp-stage.current .tp-dot { background: var(--primary); box-shadow: 0 0 0 3px var(--primary-light); }
.tp-stage.bad { color: var(--error-text); }
.tp-stage.bad .tp-dot { background: var(--error-text); }

.tp-tasks { display: grid; gap: 6px; }
.tp-sec-t { display: flex; align-items: baseline; gap: 6px; margin-top: 2px; color: var(--text-muted); font-size: 11px; }
.tp-sec-t b { color: var(--text-primary); font-variant-numeric: tabular-nums; }
.tp-sec-t em { color: var(--error-text); font-style: normal; }
.tp-task { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border: 1px solid var(--border); border-radius: var(--radius-md); background: var(--bg-main); }
.tp-task-dot { width: 7px; height: 7px; flex-shrink: 0; border-radius: 50%; background: var(--text-faint); }
.tp-task.running .tp-task-dot { background: var(--primary); box-shadow: 0 0 0 3px var(--primary-light); }
.tp-task.done .tp-task-dot { background: var(--success-text); }
.tp-task.failed .tp-task-dot { background: var(--error-text); }
.tp-task-body { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
.tp-task-title { min-width: 0; color: var(--text-regular); font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
/* 【D8】板块验收断言（前置透出小字） */
.tp-task-acc { min-width: 0; color: var(--text-faint); font-size: 10px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tp-task.running .tp-task-title { color: var(--text-primary); font-weight: 600; }
.tp-task-meta { flex-shrink: 0; color: var(--text-muted); font-size: 10.5px; font-variant-numeric: tabular-nums; }
.tp-task.failed .tp-task-meta { color: var(--error-text); }

.tp-summary { color: var(--text-muted); font-size: 11.5px; line-height: 1.5; }

@media (max-width: 1100px) {
  .task-panel { display: none; }
}
</style>
