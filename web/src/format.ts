// 数值格式化：BI 呈现的"好看"基础——金额万/亿压缩、千分位、百分比。

export type Semantic = 'money' | 'count' | 'percent' | 'geo' | 'customer' | 'goods' | 'order' | 'none'

export function toNum(v: unknown): number | null {
  if (typeof v === 'number') return v
  if (typeof v === 'string') {
    const n = parseFloat(v)
    return Number.isFinite(n) ? n : null
  }
  return null
}

// 省级行政区划码 → 省名（DMS province 列存区划码，翻名可读）
const PROVINCE: Record<string, string> = {
  '110000': '北京', '120000': '天津', '130000': '河北', '140000': '山西', '150000': '内蒙古',
  '210000': '辽宁', '220000': '吉林', '230000': '黑龙江', '310000': '上海', '320000': '江苏',
  '330000': '浙江', '340000': '安徽', '350000': '福建', '360000': '江西', '370000': '山东',
  '410000': '河南', '420000': '湖北', '430000': '湖南', '440000': '广东', '450000': '广西',
  '460000': '海南', '500000': '重庆', '510000': '四川', '520000': '贵州', '530000': '云南',
  '540000': '西藏', '610000': '陕西', '620000': '甘肃', '630000': '青海', '640000': '宁夏',
  '650000': '新疆', '710000': '台湾', '810000': '香港', '820000': '澳门',
}

/** 按语义格式化单元格显示值 */
export function fmt(v: unknown, semantic: Semantic = 'none'): string {
  if (v === null || v === undefined || v === '') return ''
  // 地理：省级区划码翻省名
  if (semantic === 'geo' && PROVINCE[String(v)]) return PROVINCE[String(v)]
  const n = toNum(v)
  // 编码/订单/名称等非数值语义：原样（避免把手机号/编码压成万亿）
  if (n === null || semantic === 'order' || semantic === 'customer' || semantic === 'goods') {
    return String(v)
  }
  if (semantic === 'percent') return `${round(n, 1)}%`
  if (semantic === 'money') return `¥${compress(n)}`
  if (semantic === 'count') return grouping(n)
  return String(v)
}

/** 万/亿压缩（对齐 SuperSonic getFormattedValue：亿 2 位、万 1 位） */
export function compress(n: number): string {
  const abs = Math.abs(n)
  if (abs >= 1e8) return `${round(n / 1e8, 2)}亿`
  if (abs >= 1e4) return `${round(n / 1e4, 1)}万`
  return grouping(round(n, 2))
}

function grouping(n: number): string {
  const [int, dec] = String(n).split('.')
  const g = int.replace(/\B(?=(\d{3})+(?!\d))/g, ',')
  return dec ? `${g}.${dec}` : g
}

function round(n: number, d: number): number {
  const p = 10 ** d
  return Math.round(n * p) / p
}
