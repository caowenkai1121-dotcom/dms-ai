<script setup lang="ts">
import { onMounted, onBeforeUnmount, ref, watch } from 'vue'
import * as echarts from 'echarts'
import { toNum, compress, fmt, type Semantic } from './format'

const props = defineProps<{
  kind: 'bar' | 'line' | 'pie'
  columns: { name: string; semantic: Semantic }[]
  rows: unknown[][]
  x: number
  y: number[]
  top?: number | null
}>()

const el = ref<HTMLDivElement>()
let chart: echarts.ECharts | null = null

// 品牌单色系（dataviz 纪律：单色明度轴，非彩虹）
const PRIMARY = '#1677ff'
const GRAD = ['#4096ff', '#1677ff']
// 饼图单色明度阶（深→浅，按值降序上色，榜首最深）
const MONO = ['#0958d9', '#1677ff', '#4096ff', '#69b1ff', '#91caff', '#bae0ff']

function labels(): string[] {
  return props.rows.map((r) => String(r[props.x] ?? ''))
}
function series(yi: number): number[] {
  return props.rows.map((r) => toNum(r[yi]) ?? 0)
}
function ySem(yi: number): Semantic {
  return props.columns[yi]?.semantic ?? 'none'
}

function render() {
  if (!chart) return
  // TOP 收纳：>top 类按首个 y 值降序取前 top，否则全量
  const allIdx = props.rows.map((_, i) => i)
  const dataIdx =
    props.top && props.rows.length > props.top
      ? [...allIdx].sort((a, b) => (toNum(props.rows[b][props.y[0]]) ?? 0) - (toNum(props.rows[a][props.y[0]]) ?? 0)).slice(0, props.top)
      : allIdx
  const xSem = props.columns[props.x]?.semantic ?? 'none'
  const catList = dataIdx.map((i) => fmt(props.rows[i][props.x], xSem))
  const fmtVal = (v: number) => compress(v)

  if (props.kind === 'pie') {
    const yi = props.y[0]
    // 按值降序上色：榜首最深
    const sorted = [...dataIdx].sort((a, b) => (toNum(props.rows[b][yi]) ?? 0) - (toNum(props.rows[a][yi]) ?? 0))
    const colorOf = new Map(sorted.map((i, rank) => [i, MONO[Math.min(rank, MONO.length - 1)]]))
    chart.setOption({
      color: MONO,
      tooltip: { trigger: 'item', formatter: (p: any) => `${p.name}<br/>${fmtVal(p.value)} (${p.percent}%)` },
      series: [{
        type: 'pie',
        radius: ['45%', '70%'],
        data: dataIdx.map((i) => ({
          name: fmt(props.rows[i][props.x], xSem),
          value: toNum(props.rows[i][yi]) ?? 0,
          itemStyle: { color: colorOf.get(i) },
        })),
        label: { formatter: '{b}\n{d}%' },
        itemStyle: { borderRadius: 4, borderColor: '#fff', borderWidth: 2 },
      }],
    })
    return
  }

  const isBar = props.kind === 'bar'
  chart.setOption({
    grid: { left: 8, right: 16, bottom: 8, top: 32, containLabel: true },
    tooltip: { trigger: 'axis', valueFormatter: (v: any) => fmtVal(Number(v)) },
    legend: props.y.length > 1 ? { top: 0 } : undefined,
    xAxis: { type: 'category', data: catList, axisLabel: { interval: 0, rotate: catList.length > 8 ? 35 : 0 } },
    yAxis: { type: 'value', axisLabel: { formatter: (v: number) => compress(v) }, splitLine: { lineStyle: { type: 'dashed', opacity: 0.5 } } },
    series: props.y.map((yi) => ({
      name: props.columns[yi]?.name,
      type: props.kind,
      data: dataIdx.map((i) => toNum(props.rows[i][yi]) ?? 0),
      barMaxWidth: 34,
      itemStyle: isBar
        ? { borderRadius: [4, 4, 0, 0], color: new echarts.graphic.LinearGradient(0, 1, 0, 0, [{ offset: 0, color: GRAD[0] }, { offset: 1, color: GRAD[1] }]) }
        : { color: PRIMARY },
      areaStyle: props.kind === 'line' ? { opacity: 0.08 } : undefined,
      smooth: props.kind === 'line',
      label: props.y.length === 1 && catList.length <= 24 ? { show: true, position: isBar ? 'top' : 'top', formatter: (p: any) => fmtVal(p.value), fontSize: 10, color: '#666' } : undefined,
    })),
  })
}

function resize() {
  chart?.resize()
}

onMounted(() => {
  if (el.value) {
    chart = echarts.init(el.value)
    render()
    window.addEventListener('resize', resize)
  }
})
onBeforeUnmount(() => {
  window.removeEventListener('resize', resize)
  chart?.dispose()
})
watch(() => [props.rows, props.kind], render, { deep: true })
</script>

<template>
  <div ref="el" style="width: 100%; height: 340px"></div>
</template>
