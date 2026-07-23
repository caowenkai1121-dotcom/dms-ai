<script setup lang="ts">
import BiChart from './BiChart.vue'
import { fmt, type Semantic } from './format'

interface ColSpec { name: string; role: string; semantic: Semantic }
interface Delta { pct: number; dir: 'up' | 'down' | 'flat'; label: string }
interface Kpi { label: string; value: unknown; semantic: Semantic; delta?: Delta }
interface Block {
  type: 'kpis' | 'entity' | 'chart' | 'table'
  items?: Kpi[]; pairs?: [string, unknown][]
  kind?: 'bar' | 'line' | 'pie'; x?: number; y?: number[]; top?: number | null
}
interface ViewSpec { columns: ColSpec[]; blocks: Block[]; interact?: { drill?: string[] }; insight?: string }
interface Result {
  columns: string[]; rows: unknown[][]; row_count: number; view: ViewSpec
}

const props = defineProps<{ result: Result }>()
const emit = defineEmits<{ (e: 'drill', dim: string): void }>()

function cell(ri: number, ci: number): string {
  return fmt(props.result.rows[ri][ci], props.result.view.columns[ci]?.semantic ?? 'none')
}
function isMetric(ci: number): boolean {
  return props.result.view.columns[ci]?.role === 'metric'
}
</script>

<template>
  <div>
    <div v-if="result.view.insight" class="insight">💡 {{ result.view.insight }}</div>
    <div v-if="result.row_count === 0" class="empty-hint">
      未找到数据。可能：① 该口径本期无记录　② 数据权限范围内无此数据　③ 换个说法试试
    </div>

    <template v-for="(b, bi) in result.view.blocks" :key="bi">
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

      <div v-else-if="b.type === 'entity'" class="entity">
        <div class="entity-hd">单据详情</div>
        <div class="entity-grid">
          <div v-for="(p, pi) in b.pairs" :key="pi" class="entity-cell">
            <div class="ec-k">{{ p[0] }}</div>
            <div class="ec-v">{{ p[1] }}</div>
          </div>
        </div>
      </div>

      <div v-else-if="b.type === 'chart'" class="chart-card">
        <BiChart :kind="b.kind!" :columns="result.view.columns" :rows="result.rows" :x="b.x!" :y="b.y!" :top="b.top" />
      </div>

      <div v-else-if="b.type === 'table' && result.row_count > 0" class="tbl-wrap">
        <table>
          <thead>
            <tr><th v-for="(c, ci) in result.columns" :key="ci" :class="{ num: isMetric(ci) }">{{ c }}</th></tr>
          </thead>
          <tbody>
            <tr v-for="(row, ri) in result.rows.slice(0, 100)" :key="ri">
              <td v-for="(_, ci) in result.columns" :key="ci" :class="{ num: isMetric(ci) }">{{ cell(ri, ci) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </template>

    <div v-if="result.row_count > 0 && result.view.interact?.drill?.length" class="drill">
      <span class="drill-t">换个维度看：</span>
      <span v-for="d in result.view.interact.drill" :key="d" class="pill" @click="emit('drill', d)">按{{ d }} ↓</span>
    </div>
  </div>
</template>
