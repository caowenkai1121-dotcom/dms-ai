<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount, ref, watch } from 'vue'
import * as echarts from 'echarts/core'
import { BarChart, LineChart, PieChart } from 'echarts/charts'
import { AriaComponent, GridComponent, LegendComponent, TooltipComponent } from 'echarts/components'
// 轴选项类型只从主入口导出（components 子包没有）；type-only 导入不影响打包体积
import type { YAXisComponentOption } from 'echarts'
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
  /** 放大视图需要占满可用空间；普通卡片保持 330px（紧凑态 286px）。 */
  height?: number
  /** 多序列切分列（类别列下标，后端 `semantic::present::trend` 给）。
   *  缺省/`null` ＝ 单序列，走原来的那条路（老服务端不带这个键也不崩）。 */
  series?: number | null
}>()

// —— 布局/轴标签常量（改一处生效，不再散落函数体内）——
/** 普通卡片默认高度 */
const DEFAULT_HEIGHT = 330
/** 紧凑态断点（容器宽低于此值走紧凑布局） */
const COMPACT_WIDTH = 560
/** 紧凑态高度 */
const COMPACT_HEIGHT = 286
/** x 轴标签数上限：超过才抽稀 + 旋转（桌面/紧凑两档） */
const MAX_LABELS = 12
const MAX_LABELS_COMPACT = 6
/** 抽稀时的标签旋转角 */
const ROTATE = 28
const ROTATE_COMPACT = 38
/** 抽稀时单个标签的截断宽 */
const LABEL_W = 110
const LABEL_W_COMPACT = 72

const el = ref<HTMLDivElement>()
const chartHeight = ref(props.height ?? DEFAULT_HEIGHT)
let chart: echarts.ECharts | null = null
let resizeObserver: ResizeObserver | null = null
let themeObserver: MutationObserver | null = null
let wasCompact = false
let lastWidth = -1

const LIGHT_SERIES = ['#4051d3', '#168a8a', '#c77917', '#7352b9', '#b24778', '#358552', '#c64c4c', '#4771c7']
const DARK_SERIES = ['#7b89f0', '#4fc7c7', '#e2a653', '#a98ae4', '#de7aaa', '#75bd8d', '#e37f7f', '#7596df']
const LIGHT_MONO = ['#3343ba', '#4051d3', '#6573df', '#8994ea', '#aeb6f2', '#d1d6f8']
const DARK_MONO = ['#7b89f0', '#8e9af3', '#a1abf5', '#b4bcf7', '#c7cdf9', '#daddfb']

function cssToken(name: string, fallback: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback
}

function readThemeTokens() {
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
type ThemeTokens = ReturnType<typeof readThemeTokens>
let themeCache: ThemeTokens | null = null
/** 主题 tokens 带缓存：render 由 watch 频繁触发，每次 8 趟 getComputedStyle 不值得；
 *  主题翻转时由 MutationObserver 清缓存再 render。 */
function themeTokens(): ThemeTokens {
  return (themeCache ??= readThemeTokens())
}

function isCompact(): boolean {
  return (el.value?.clientWidth ?? 800) < COMPACT_WIDTH
}

function syncHeight(): void {
  chartHeight.value = props.height ?? (isCompact() ? COMPACT_HEIGHT : DEFAULT_HEIGHT)
}

function displayMetric(value: unknown, yi: number): string {
  return fmt(value, ySem(yi)) || '-'
}

function displayAxisMetric(value: unknown, yi: number): string {
  const semantic = ySem(yi)
  if (semantic === 'percent') return displayMetric(value, yi)
  return displayMetric(value, yi).replace(/^¥/, '')
}

/** 毛利率列判定（0~1 ratio ×100 合同的触发条件）：包含「毛利率」即命中
 *  （平均毛利率、毛利率(%)、毛利率（净）等变体都覆盖）。
 *  ⚠️ 同款逻辑还复制在 App.vue / ResultPanel.vue 两处，判据改动需三处同步（待抽进 format.ts）。 */
function isGrossMarginLabel(label: string): boolean {
  return label.replace(/\s+/g, '').includes('毛利率')
}

/** DWS 毛利率合同值为 0~1，这里 ×100 还原成百分数；⚠️ 该合同只覆盖毛利率列 ——
 *  其它 percent 语义列（如税率）若后端也给 0~1 ratio，图与表会同时差 100 倍且无告警，
 *  后端口径改动时必须点检。只变换图表数据副本，原始 rows/CSV/SQL 不动。 */
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

// 多序列逻辑见下方 groups.map 那段（`series` prop 驱动）。
function ySem(yi: number): Semantic {
  const column = props.columns[yi]
  if (column?.semantic && column.semantic !== 'none') return column.semantic
  // 词表认不出时保留 'none'（原样显示）：兜底成 count 会把未识别的金额列（GMV 等）
  // 按「万」压缩还丢 ¥ —— 原样比静默错量纲好。
  return semanticForLabel(column?.name ?? '')
}

/** 按某 y 列值降序的排序器：值先预算一遍（比较器不重复跑 metricNumber），
 *  null 沉底 —— 缺数据行不该被「当 0」顶进 TOP 榜（含负数的指标里 0 比负数大）。 */
function byValueDesc(yi: number): (a: number, b: number) => number {
  const cache = props.rows.map((row) => metricNumber(row[yi], yi))
  return (a, b) => (cache[b] ?? -Infinity) - (cache[a] ?? -Infinity)
}

function render() {
  if (!chart) return
  // 空数据/空指标列直接不画：y=[] 时 sort/轴/series 会静默产出一张空图
  if (!props.rows.length || !props.y.length) return
  const theme = themeTokens()
  const compact = isCompact()
  // TOP 收纳：>top 类按首个 y 值降序取前 top，否则全量
  const allIdx = props.rows.map((_, i) => i)
  const dataIdx =
    props.top && props.rows.length > props.top
      ? [...allIdx].sort(byValueDesc(props.y[0])).slice(0, props.top)
      : allIdx
  const xSem = props.columns[props.x]?.semantic ?? 'none'
  const catList = dataIdx.map((i) => fmt(props.rows[i][props.x], xSem))
  const fmtVal = (v: unknown) => displayMetric(v, props.y[0])

  if (props.kind === 'pie') {
    const yi = props.y[0]
    // 按值降序上色：榜首最深；取模循环色阶，尾部多个扇区不共用同一个最浅色
    const sorted = [...dataIdx].sort(byValueDesc(yi))
    const colorOf = new Map(sorted.map((i, rank) => [i, theme.mono[rank % theme.mono.length]]))
    // 6 类是分水岭：>=6 走滚动图例、不画扇区标签；<=5 画标签、不出图例（两条件不许重叠）
    const showLegend = dataIdx.length > 5 || compact
    const showLabels = !compact && dataIdx.length <= 5
    // 不同原始值可能格式化成同一名字（如未登记区划码原样撞名）：echarts 会把同名单元并成一项，拼序号去重
    const seen = new Map<string, number>()
    chart.setOption({
      backgroundColor: 'transparent',
      tooltip: {
        trigger: 'item',
        confine: true,
        backgroundColor: theme.card,
        borderColor: theme.border,
        textStyle: { color: theme.text, fontSize: 12 },
        formatter: (p: any) => `<b>${escapeHtml(p.name)}</b><br/>${escapeHtml(displayMetric(p.value, yi))} · ${p.percent}%`,
      },
      legend: {
        show: showLegend,
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
        center: ['50%', showLegend ? '45%' : '50%'],
        data: dataIdx.map((i) => {
          const raw = fmt(props.rows[i][props.x], xSem)
          const n = (seen.get(raw) ?? 0) + 1
          seen.set(raw, n)
          return {
            name: n > 1 ? `${raw}（${n}）` : raw,
            value: metricNumber(props.rows[i][yi], yi),
            itemStyle: { color: colorOf.get(i) },
          }
        }),
        label: {
          show: showLabels,
          formatter: '{b}\n{d}%',
          color: theme.text,
          fontSize: 11,
          lineHeight: 16,
        },
        labelLine: { show: showLabels, lineStyle: { color: theme.border } },
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
  const cellKey = (g: string, xv: string) => `${g}${xv}`
  const cellOf = new Map<string, number | null>()
  if (gi !== null) {
    for (const i of dataIdx) {
      // ⚠️ 键是**格式化后**的字符串：依赖后端分组唯一（两个原始类别格式化成同串会静默合并、
      // 同 (g,x) 重复行后者覆盖前者）——后端 trend 切分保证唯一，这里不重复判重。
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
  const yAxis: YAXisComponentOption | YAXisComponentOption[] = dual
    ? [
        { type: 'value' as const, ...axis, axisLabel: { ...axis.axisLabel, formatter: (v: number) => displayAxisMetric(v, props.y[0]) } },
        { type: 'value' as const, ...axis, axisLabel: { ...axis.axisLabel, formatter: (v: number) => displayAxisMetric(v, props.y[1]) }, splitLine: { show: false } },
      ]
    : { type: 'value' as const, ...axis, axisLabel: { ...axis.axisLabel, formatter: (v: number) => displayAxisMetric(v, props.y[0]) } }
  const maxLabels = compact ? MAX_LABELS_COMPACT : MAX_LABELS
  // floor 策略：只超一两个标签时不砍半（ceil 会把 13 个标签抽成 7 个），交给 rotate + hideOverlap 消化
  const labelInterval = xLabels.length > maxLabels ? Math.max(0, Math.floor(xLabels.length / maxLabels) - 1) : 0
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
  // 两分支共享的系列基础键；折线专属键（smooth/showSymbol/symbolSize）只在 line 时展开，bar 不挂无关键。
  const baseSeries = {
    type: props.kind,
    barMaxWidth: 34,
    barGap: '15%',
    emphasis: { focus: 'series' as const },
  }
  const lineOnly = props.kind === 'line' ? { smooth: .24, showSymbol: xLabels.length <= 14, symbolSize: 6 } : {}
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
        rotate: xLabels.length > maxLabels ? (compact ? ROTATE_COMPACT : ROTATE) : 0,
        hideOverlap: true,
        color: theme.muted,
        fontSize: compact ? 10 : 11,
        width: compact ? LABEL_W_COMPACT : LABEL_W,
        overflow: 'truncate',
      },
    },
    yAxis,
    series:
      gi !== null
        ? groups.map((g, si) => ({
            ...baseSeries,
            name: g,
            data: xLabels.map((xv) => cellOf.get(cellKey(g, xv)) ?? null),
            // 8 色取模回绕：超过 8 组时第 9 组与第 1 组同色 —— 已知取舍（图例文字可区分），
            // 真超过 8 类的场景应走 TOP 收纳而不是堆序列。
            itemStyle: { borderRadius: isBar ? [4, 4, 0, 0] : undefined, color: SERIES[si % SERIES.length] },
            ...lineOnly,
            // 分组只切出 1 组时与单序列同口径：给柱顶/点旁数值标签（多组则太挤不给）
            label: !compact && groups.length === 1 && xLabels.length <= MAX_LABELS
              ? { show: true, position: 'top' as const, formatter: (p: any) => fmtVal(p.value), fontSize: 10, color: theme.muted }
              : undefined,
          }))
        : props.y.map((yi, si) => ({
            ...baseSeries,
            name: props.columns[yi]?.name,
            yAxisIndex: dual ? si : 0,
            data: dataIdx.map((i) => metricNumber(props.rows[i][yi], yi)),
            itemStyle: isBar
              ? multi
                ? { borderRadius: [4, 4, 0, 0], color: SERIES[si % SERIES.length] }
                : { borderRadius: [4, 4, 0, 0], color: new echarts.graphic.LinearGradient(0, 1, 0, 0, [{ offset: 0, color: theme.primaryHover }, { offset: 1, color: theme.primary }]) }
              : { color: multi ? SERIES[si % SERIES.length] : theme.primary },
            areaStyle: props.kind === 'line' ? { opacity: multi ? 0 : 0.08 } : undefined,
            lineStyle: props.kind === 'line' ? { width: 2.5 } : undefined,
            ...lineOnly,
            label: !compact && !multi && catList.length <= MAX_LABELS
              ? { show: true, position: 'top' as const, formatter: (p: any) => fmtVal(p.value), fontSize: 10, color: theme.muted }
              : undefined,
          })),
    aria: { enabled: true },
  }, true)
}

function resize() {
  const compact = isCompact()
  const width = el.value?.clientWidth ?? 0
  const before = chartHeight.value
  syncHeight()
  // 自身 chartHeight 变化会再触发一轮 RO：宽度没变且高度已同步过就是回触发，跳过白跑的一轮
  if (width === lastWidth && chartHeight.value === before) return
  lastWidth = width
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
    lastWidth = el.value.clientWidth
    chart = echarts.init(el.value)
    render()
    resizeObserver = new ResizeObserver(resize)
    resizeObserver.observe(el.value)
    themeObserver = new MutationObserver(() => { themeCache = null; render() })
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] })
  }
})
onBeforeUnmount(() => {
  resizeObserver?.disconnect()
  themeObserver?.disconnect()
  chart?.dispose()
})
// series 也要看：同一批 rows 换个切分列（或后端从单序列改成多序列）必须重画。
// 不开 deep：父级（App/ResultPanel）都是整体替换 rows 引用，深比较几百行买的是用不到的能力。
watch(
  () => [props.rows, props.kind, props.series, props.x, props.y, props.top, props.columns, props.height],
  () => { syncHeight(); render() },
)

/** 读屏标签带图型 + 指标列名：同页多张图（同窗补充、深度页多 section）能区分是哪张。 */
const KIND_ARIA: Record<string, string> = { bar: '柱状图', line: '折线图', pie: '饼图' }
const chartAriaLabel = computed(() => {
  const names = props.y.map((yi) => props.columns[yi]?.name).filter(Boolean).join('、')
  return `${KIND_ARIA[props.kind] ?? '图表'}：${names || '业务数据'}`
})
</script>

<template>
  <div ref="el" class="bi-chart" role="img" :aria-label="chartAriaLabel" :style="{ height: `${chartHeight}px` }"></div>
</template>

<style scoped>
.bi-chart { width: 100%; min-width: 0; }
</style>
