// 数值格式化：业务数值满 1 万统一按“万”展示，原始数据不变。

export type Semantic = 'money' | 'count' | 'percent' | 'geo' | 'customer' | 'goods' | 'order' | 'none'

export function toNum(v: unknown): number | null {
  if (typeof v === 'number') return v
  if (typeof v === 'string') {
    const text = v.trim().replace(/,/g, '')
    if (!/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)$/.test(text)) return null
    const n = Number(text)
    return Number.isFinite(n) ? n : null
  }
  return null
}

export function semanticForLabel(label: string): Semantic {
  // 标识列优先于指标词：例如“税率编码”“状态码”必须原样显示，不能被当百分比。
  if (/单号|编号|编码|代码|条码|状态(?:码)?$|区划码|身份证|手机号|电话|批次号|(?:^|_)id$|ID$/i.test(label)) return 'order'
  if (/占比|比例|率|同比|环比|百分比/.test(label)) return 'percent'
  if (/销售额|金额|费用|余额|单价|客单价|成本|利润|收入|支出|货值|资产|坪效|人效/.test(label)) return 'money'
  if (/数量|销量|订单数|订单量|单量|笔数|客户数|门店数|商品数|件数|箱数|袋数|台数|人数|次数|库存(?:数|量)|总数|总量|合计数/.test(label)) return 'count'
  return 'none'
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
  // 编码/订单/名称等非数值语义原样展示。
  if (n === null || semantic === 'order' || semantic === 'customer' || semantic === 'goods' || semantic === 'geo') {
    return String(v)
  }
  if (semantic === 'percent') return `${round(n, 1)}%`
  if (semantic === 'money') return `¥${compress(n)}`
  if (semantic === 'count') return compress(n)
  // 普通维度可能是年份、日期码或未登记编码，不能擅自按“万”压缩。
  return String(v)
}

/** 业务数值：绝对值满 1 万固定 3 位小数，否则千分位且最多 3 位小数。 */
export function compress(n: number): string {
  const abs = Math.abs(n)
  if (abs >= 1e4) return `${(n / 1e4).toFixed(3)}万`
  return grouping(n)
}

function grouping(n: number): string {
  const value = Math.abs(n) < 0.0005 ? 0 : n
  return new Intl.NumberFormat('zh-CN', {
    minimumFractionDigits: 0,
    maximumFractionDigits: 3,
    useGrouping: true,
  }).format(value)
}

function round(n: number, d: number): number {
  const p = 10 ** d
  return Math.round(n * p) / p
}
