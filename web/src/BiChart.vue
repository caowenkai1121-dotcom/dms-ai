<script setup lang="ts">
import { onMounted, onBeforeUnmount, ref, watch } from 'vue'
import * as echarts from 'echarts/core'
import { BarChart, LineChart, PieChart } from 'echarts/charts'
import { AriaComponent, GridComponent, LegendComponent, TooltipComponent } from 'echarts/components'
import { CanvasRenderer } from 'echarts/renderers'
import { toNum, fmt, semanticForLabel, type Semantic } from './format'

echarts.use([
  BarChart,
  LineChart,
  PieChart,
  AriaComponent,
  GridComponent,
  LegendComponent,
  TooltipComponent,
  CanvasRenderer,
])

const props = defineProps<{
  kind: 'bar' | 'line' | 'pie'
  columns: { name: string; semantic: Semantic }[]
  rows: unknown[][]
  x: number
  y: number[]
  top?: number | null
  /** 放大视图需要占满可用空间；普通卡片保持原来的 340px。 */
  height?: number
  /** 多序列切分列（类别列下标，后端 `semantic::present::trend` 给）。
   *  缺省/`null` ＝ 单序列，走原来的那条路（老服务端不带这个键也不崩）。 */
  series?: number | null
}>()

const el = ref<HTMLDivElement>()
const chartHeight = ref(props.height ?? 330)
let chart: echarts.ECharts | null = null
let resizeObserver: ResizeObserver | null = null
let themeObserver: MutationObserver | null = null
let wasCompact = false

const LIGHT_SERIES = ['#4051d3', '#168a8a', '#c77917', '#7352b9', '#b24778', '#358552', '#c64c4c', '#4771c7']
const DARK_SERIES = ['#7b89f0', '#4fc7c7', '#e2a653', '#a98ae4', '#de7aaa', '#75bd8d', '#e37f7f', '#7596df']
const LIGHT_MONO = ['#3343ba', '#4051d3', '#6573df', '#8994ea', '#aeb6f2', '#d1d6f8']
const DARK_MONO = ['#7b89f0', '#8e9af3', '#a1abf5', '#b4bcf7', '#c7cdf9', '#daddfb']

function cssToken(name: string, fallback: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback
}

function themeTokens() {
  const dark = document.documentElement.dataset.theme === 'dark'
  return {
    dark,
    primary: cssToken('--primary', dark ? '#7b89f0' : '#4051d3'),
    primaryHover: cssToken('--primary-hover', dark ? '#93a0f5' : '#3445bd'),
    card: cssToken('--bg-card', dark ? '#1a1e2b' : '#ffffff'),
    text: cssToken('--text-regular', dark ? '#c3c9dd' : '#3f4760'),
    muted: cssToken('--text-muted', dark ? '#8b93ad' : '#646d87'),
    border: cssToken('--border', dark ? '#2c3247' : '#e2e6ef'),
    divider: cssToken('--divider', dark ? '#232838' : '#edf0f6'),
    series: dark ? DARK_SERIES : LIGHT_SERIES,
    mono: dark ? DARK_MONO : LIGHT_MONO,
  }
}

function isCompact(): boolean {
  return (el.value?.clientWidth ?? 800) < 560
}

function syncHeight(): void {
  chartHeight.value = props.height ?? (isCompact() ? 286 : 330)
}

function displayMetric(value: unknown, yi: number): string {
  return fmt(value, ySem(yi)) || '-'
}

function displayAxisMetric(value: unknown, yi: number): string {
  const semantic = ySem(yi)
  if (semantic === 'percent') return displayMetric(value, yi)
  return displayMetric(value, yi).replace(/^¥/, '')
}

function isGrossMarginLabel(label: string): boolean {
  const normalized = label.replace(/\s+/g, '')
  return normalized === '毛利率' || normalized === '销售毛利率'
}

/** DWS 毛利率合同值为 0~1；只变换图表数据副本，原始 rows/CSV/SQL 不动。 */
function metricNumber(value: unknown, yi: number): number | null {
  const number = toNum(value)
  if (number === null) return null
  return isGrossMarginLabel(props.columns[yi]?.name ?? '') ? number * 100 : number
}

function escapeHtml(value: unknown): string {
  return String(value ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;')
}

// 这里原来有两个**没有任何调用者**的 `labels()` / `series(yi)`（`git show HEAD` 里就已无人调用）。
// 删掉的直接理由：新加的多序列走 `props.series`，而那个 prop 与死函数 `series()` **同名** ——
// 读代码的人会以为多序列走的是这个函数，实际逻辑在下面 `groups.map` 那段。
// `labels()` 的逻辑已在 `catList` 里。
function ySem(yi: number): Semantic {
  const column = props.columns[yi]
  if (column?.semantic && column.semantic !== 'none') return column.semantic
  const inferred = semanticForLabel(column?.name ?? '')
  return inferred === 'none' ? 'count' : inferred
}

function render() {
  if (!chart) return
  const theme = themeTokens()
  const compact = isCompact()
  // TOP 收纳：>top 类按首个 y 值降序取前 top，否则全量
  const allIdx = props.rows.map((_, i) => i)
  const dataIdx =
    props.top && props.rows.length > props.top
      ? [...allIdx].sort((a, b) => (metricNumber(props.rows[b][props.y[0]], props.y[0]) ?? 0) - (metricNumber(props.rows[a][props.y[0]], props.y[0]) ?? 0)).slice(0, props.top)
      : allIdx
  const xSem = props.columns[props.x]?.semantic ?? 'none'
  const catList = dataIdx.map((i) => fmt(props.rows[i][props.x], xSem))
  const fmtVal = (v: unknown) => displayMetric(v, props.y[0])

  if (props.kind === 'pie') {
    const yi = props.y[0]
    // 按值降序上色：榜首最深
    const sorted = [...dataIdx].sort((a, b) => (metricNumber(props.rows[b][yi], yi) ?? 0) - (metricNumber(props.rows[a][yi], yi) ?? 0))
    const colorOf = new Map(sorted.map((i, rank) => [i, theme.mono[Math.min(rank, theme.mono.length - 1)]]))
    chart.setOption({
      backgroundColor: 'transparent',
      color: theme.mono,
      tooltip: {
        trigger: 'item',
        confine: true,
        backgroundColor: theme.card,
        borderColor: theme.border,
        textStyle: { color: theme.text, fontSize: 12 },
        formatter: (p: any) => `<b>${escapeHtml(p.name)}</b><br/>${escapeHtml(displayMetric(p.value, yi))} · ${p.percent}%`,
      },
      legend: {
        show: dataIdx.length > 5 || compact,
        type: 'scroll',
        bottom: 0,
        icon: 'circle',
        itemWidth: 8,
        itemHeight: 8,
        textStyle: { color: theme.muted, fontSize: compact ? 10 : 11 },
      },
      series: [{
        type: 'pie',
        radius: compact ? ['42%', '64%'] : ['44%', '69%'],
        center: ['50%', dataIdx.length > 5 || compact ? '45%' : '50%'],
        data: dataIdx.map((i) => ({
          name: fmt(props.rows[i][props.x], xSem),
          value: metricNumber(props.rows[i][yi], yi),
          itemStyle: { color: colorOf.get(i) },
        })),
        label: {
          show: !compact && dataIdx.length <= 6,
          formatter: '{b}\n{d}%',
          color: theme.text,
          fontSize: 11,
          lineHeight: 16,
        },
        labelLine: { show: !compact && dataIdx.length <= 6, lineStyle: { color: theme.border } },
        emphasis: { scaleSize: 6 },
        itemStyle: { borderRadius: 3, borderColor: theme.card, borderWidth: 2 },
      }],
      aria: { enabled: true },
    }, true) // notMerge，理由见下方非饼那一处
    return
  }

  const isBar = props.kind === 'bar'
  // 单序列跟随主题品牌色；多序列使用低饱和的可区分色阶。
  const SERIES = theme.series

  // 🔴 **按类别列切多序列**（`series` 列下标由后端 `semantic::present::trend` 给）。
  //
  // 不切的时候「时间 + 恰 1 类别 + 1 指标」（例「今年各月各品类销售额」12 月 × 6 品类 = 72 行）
  // 走的是下面那条单序列路：x 轴按行取值 → 「2026-01」重复 6 次，echarts 按**行序**
  // 把 6 个品类的点连成一根线。图是错的，而 SQL / 口径 / 行数全对，没有判据会红。
  // （这条推理来自后端决策树，不是连库截图 —— 前端这一半今天没有能红的判据，见交付说明。）
  //
  // 分组后：x 轴＝去重后的时间点（首现顺序，即 SQL 的 ORDER BY），每个类别一根线；
  // 某类别在某时间点缺行 → `null` ＝断点（不许 `?? 0` 补零，那是**编造数据**）。
  const gi = props.series ?? null
  const gSem = gi === null ? 'none' : (props.columns[gi]?.semantic ?? 'none')
  const groups = gi === null ? [] : [...new Set(dataIdx.map((i) => fmt(props.rows[i][gi], gSem)))]
  const xLabels = gi === null ? catList : [...new Set(catList)]
  // 分组时值按 (类别, x) 查表 —— 一趟 O(行数) 建表，不在双层循环里 find
  // 分隔符必须是不可见的：品类名里有空格/连字符是常态（「手抓饼 - 原味」），拿可见字符拼会串键
  const cellKey = (g: string, xv: string) => `${g}\u0000${xv}`
  const cellOf = new Map<string, number | null>()
  if (gi !== null) {
    for (const i of dataIdx) {
      cellOf.set(cellKey(fmt(props.rows[i][gi], gSem), fmt(props.rows[i][props.x], xSem)), metricNumber(props.rows[i][props.y[0]], props.y[0]))
    }
  }

  const multi = props.y.length > 1
  // 双指标且语义不同（如 金额 vs 单量）→ 双值轴，量纲悬殊不互相压扁。
  // 分组时所有序列是**同一个指标列**，双轴无意义（后端也只在 metric.len()==1 时给 series）。
  const dual = gi === null && props.y.length === 2 && ySem(props.y[0]) !== ySem(props.y[1])
  const axis = {
    axisLabel: { color: theme.muted, fontSize: compact ? 10 : 11 },
    axisLine: { show: false },
    axisTick: { show: false },
    splitLine: { lineStyle: { color: theme.divider, type: 'dashed' as const } },
  }
  const yAxis: any = dual
    ? [
        { type: 'value', ...axis, axisLabel: { ...axis.axisLabel, formatter: (v: number) => displayAxisMetric(v, props.y[0]) } },
        { type: 'value', ...axis, axisLabel: { ...axis.axisLabel, formatter: (v: number) => displayAxisMetric(v, props.y[1]) }, splitLine: { show: false } },
      ]
    : { type: 'value', ...axis, axisLabel: { ...axis.axisLabel, formatter: (v: number) => displayAxisMetric(v, props.y[0]) } }
  const maxLabels = compact ? 6 : 12
  const labelInterval = xLabels.length > maxLabels ? Math.ceil(xLabels.length / maxLabels) - 1 : 0
  const tooltipFormatter = (params: any) => {
    const items = Array.isArray(params) ? params : [params]
    if (!items.length) return ''
    const heading = escapeHtml(items[0]?.axisValueLabel ?? items[0]?.name ?? '')
    const lines = items.map((item: any) => {
      const yi = gi !== null ? props.y[0] : (props.y[item.seriesIndex] ?? props.y[0])
      return `${item.marker ?? ''}${escapeHtml(item.seriesName ?? '')}<span style="float:right;margin-left:18px;font-weight:700">${escapeHtml(displayMetric(item.value, yi))}</span>`
    })
    return `<div style="min-width:140px"><b>${heading}</b><br/>${lines.join('<br/>')}</div>`
  }
  // notMerge=true：echarts 默认按**下标**合并 series，而序列条数现在会随结果变
  // （6 个品类 → 下一次 1 条），合并会把上一次多出来的 5 条线留在图上。同理 pie↔bar 换 kind
  // 会留下上一次的 xAxis/配色。这不是新问题，但 `series` 让序列条数真的开始变了。
  chart.setOption({
    backgroundColor: 'transparent',
    grid: { left: compact ? 2 : 8, right: dual ? 6 : 12, bottom: compact ? 12 : 8, top: multi || groups.length > 1 ? 38 : 22, containLabel: true },
    tooltip: {
      trigger: 'axis',
      confine: true,
      backgroundColor: theme.card,
      borderColor: theme.border,
      textStyle: { color: theme.text, fontSize: 12 },
      axisPointer: { type: isBar ? 'shadow' : 'line', lineStyle: { color: theme.primary }, shadowStyle: { color: theme.divider, opacity: .45 } },
      formatter: tooltipFormatter,
    },
    legend: multi || groups.length > 1
      ? { top: 0, type: 'scroll', icon: 'roundRect', itemWidth: 12, itemHeight: 7, textStyle: { color: theme.muted, fontSize: compact ? 10 : 11 } }
      : undefined,
    xAxis: {
      type: 'category',
      data: xLabels,
      axisTick: { show: false },
      axisLine: { lineStyle: { color: theme.border } },
      axisLabel: {
        interval: labelInterval,
        rotate: xLabels.length > maxLabels ? (compact ? 38 : 28) : 0,
        hideOverlap: true,
        color: theme.muted,
        fontSize: compact ? 10 : 11,
        width: compact ? 72 : 110,
        overflow: 'truncate',
      },
    },
    yAxis,
    series:
      gi !== null
        ? groups.map((g, si) => ({
            name: g,
            type: props.kind,
            data: xLabels.map((xv) => cellOf.get(cellKey(g, xv)) ?? null),
            barMaxWidth: 34,
            barGap: '15%',
            itemStyle: { borderRadius: isBar ? [4, 4, 0, 0] : undefined, color: SERIES[si % SERIES.length] },
            smooth: props.kind === 'line' ? .24 : false,
            showSymbol: props.kind === 'line' ? xLabels.length <= 14 : undefined,
            symbolSize: 6,
            emphasis: { focus: 'series' },
          }))
        : props.y.map((yi, si) => ({
            name: props.columns[yi]?.name,
            type: props.kind,
            yAxisIndex: dual ? si : 0,
            data: dataIdx.map((i) => metricNumber(props.rows[i][yi], yi)),
            barMaxWidth: 34,
            barGap: '15%',
            itemStyle: isBar
              ? multi
                ? { borderRadius: [4, 4, 0, 0], color: SERIES[si % SERIES.length] }
                : { borderRadius: [4, 4, 0, 0], color: new echarts.graphic.LinearGradient(0, 1, 0, 0, [{ offset: 0, color: theme.primaryHover }, { offset: 1, color: theme.primary }]) }
              : { color: multi ? SERIES[si % SERIES.length] : theme.primary },
            areaStyle: props.kind === 'line' ? { opacity: multi ? 0 : 0.08 } : undefined,
            lineStyle: props.kind === 'line' ? { width: 2.5 } : undefined,
            smooth: props.kind === 'line' ? .24 : false,
            showSymbol: props.kind === 'line' ? xLabels.length <= 14 : undefined,
            symbolSize: 6,
            emphasis: { focus: 'series' },
            label: !compact && !multi && catList.length <= 12
              ? { show: true, position: 'top', formatter: (p: any) => fmtVal(p.value), fontSize: 10, color: theme.muted }
              : undefined,
          })),
    aria: { enabled: true },
  }, true)
}

function resize() {
  const compact = isCompact()
  syncHeight()
  chart?.resize()
  if (compact !== wasCompact) {
    wasCompact = compact
    render()
  }
}

onMounted(() => {
  if (el.value) {
    syncHeight()
    wasCompact = isCompact()
    chart = echarts.init(el.value)
    render()
    resizeObserver = new ResizeObserver(resize)
    resizeObserver.observe(el.value)
    themeObserver = new MutationObserver(() => render())
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] })
  }
})
onBeforeUnmount(() => {
  resizeObserver?.disconnect()
  themeObserver?.disconnect()
  chart?.dispose()
})
// series 也要看：同一批 rows 换个切分列（或后端从单序列改成多序列）必须重画
watch(
  () => [props.rows, props.kind, props.series, props.x, props.y, props.top, props.columns, props.height],
  () => { syncHeight(); render() },
  { deep: true },
)
</script>

<template>
  <div ref="el" class="bi-chart" role="img" aria-label="业务数据图表" :style="{ height: `${chartHeight}px` }"></div>
</template>

<style scoped>
.bi-chart { width: 100%; min-width: 0; }
</style>
