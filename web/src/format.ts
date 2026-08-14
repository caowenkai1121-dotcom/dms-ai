// 数值格式化：业务数值满 1 万统一按“万”展示，原始数据不变。

export type Semantic = 'money' | 'count' | 'percent' | 'geo' | 'customer' | 'goods' | 'order' | 'none'

// crypto.randomUUID 只在安全上下文（HTTPS / localhost）存在；http://IP 部署时它是 undefined，
// 会话 turnKey / rid 全靠它 —— 必须有降级（getRandomValues 在非安全上下文可用，Math.random 兜底）。
// ⚠️ Math.random 兜底**不是加密级**随机，仅用于防撞 key，别拿它当任何安全依据。
export function uuid(): string {
  const c = globalThis.crypto
  if (c?.randomUUID) return c.randomUUID()
  const buf = new Uint8Array(16)
  if (c?.getRandomValues) c.getRandomValues(buf)
  else for (let i = 0; i < 16; i++) buf[i] = Math.floor(Math.random() * 256)
  buf[6] = (buf[6] & 0x0f) | 0x40
  buf[8] = (buf[8] & 0x3f) | 0x80
  const hex = Array.from(buf, (b) => b.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}

export function toNum(v: unknown): number | null {
  if (typeof v === 'number') return v
  if (typeof v === 'string') {
    const text = v.trim()
    // 千分位逗号只认合法分组（1,234.5）：「1,2,3」「,,5」这类畸形串判非数值，
    // 不能先删逗号再校验 —— 那是把脏数据静默洗成 123 / 5。
    const plain = /^[+-]?\d{1,3}(?:,\d{3})+(?:\.\d*)?$/.test(text) ? text.replace(/,/g, '') : text
    if (!/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)$/.test(plain)) return null
    const n = Number(plain)
    return Number.isFinite(n) ? n : null
  }
  return null
}

/** 毛利率列判定：DWS 的毛利率合同值是 0~1 的 ratio，展示前必须 ×100。
 *
 *  🔴 **全仓唯一一份**。此前三处各写一份且判据不同：BiChart 用 `includes('毛利率')`（宽），
 *  ResultPanel / App.vue 用 `=== '毛利率' || === '销售毛利率'`（窄）——
 *  列名一旦是变体（平均毛利率 / 毛利率（%） / 品类毛利率），同一屏图表画成 19.6%、
 *  KPI 卡与明细表显示 0.2%，而 SQL、行数、口径全对，没有任何判据会红（2026-08-13 审计）。
 *  取宽的那份：窄判据漏的都是真毛利率列；而「汇率/频率/功率/倍率/速率」不含「毛利率」，
 *  本来就不会命中。 */
export function isGrossMarginLabel(label: string): boolean {
  // 判据是**词尾**不是包含：`毛利率可计算覆盖率` 含「毛利率」但它是覆盖率，
  // 已经是 0~100 的百分数，再 ×100 就是错数。
  // 「毛利占比」是 sales_fact 登记的别名，两份判据原本都漏。
  // 后端 `deep_api::is_gross_margin_value_label` 与本函数逐字同源，改一处必须改两处。
  const clean = label
    .replace(/\s+/g, '')
    .replace(/[%％]+$/, '')
    .replace(/[（(][^（()）]*[)）]$/, '')
    .replace(/[%％]+$/, '')
  return clean.endsWith('毛利率') || clean.endsWith('毛利占比')
}

/** 列名 → 语义的**猜测**（兜底）。
 *
 *  🔴 声明优先：后端 `ColumnSpec.semantic`（`dms_semantic::present::infer_semantic` 推断）
 *  随 view 一起下发，拿得到就用它，别在这里堆词去「修」某一列。
 *  后端同族兜底是 `crates/server/src/chart_svg.rs::label_kind`：
 *  「率」的物理/金融比率排除（汇率/频率/功率/倍率/速率）与 money/count 词表两边逐字同源，
 *  改一处必须改两处。仍未对齐的是标识列判据 —— 后端 label_kind 的 identity 分支还多认
 *  一批中英文词尾（`_at`/`日期`/`品牌`…），这里只认编码族；对齐需要两边同时改，暂留此注。 */
export function semanticForLabel(label: string): Semantic {
  // 标识列优先于指标词：例如“税率编码”“状态码”必须原样显示，不能被当百分比。
  if (/单号|编号|编码|代码|条码|状态(?:码)?$|区划码|身份证|手机号|电话|批次号|ID$/i.test(label)) return 'order'
  if (/占比|比例|百分比/.test(label)) return 'percent'
  // 「率」只按词尾认，且排除汇率/频率/功率等物理与金融比率 —— 那些不是 0-100 的百分数，
  // 把 6.45 的汇率显示成「6.5%」就是错数。
  if (/率$/.test(label) && !/(?:汇|频|功|倍|速)率$/.test(label)) return 'percent'
  // 「同比/环比」本身是比率；但词尾带 额/量/数 的（同比增长额、环比增量）是金额与单量，
  // 不许加 % —— 交给后面的 money/count 词表或 none 原样。
  if (/同比|环比/.test(label) && !/(?:额|量|数)$/.test(label)) return 'percent'
  if (/销售额|金额|费用|余额|单价|客单价|成本|利润|收入|支出|货值|资产|坪效|人效|营收|售价|现价/.test(label)) return 'money'
  if (/数量|销量|订单数|订单量|单量|笔数|客户数|门店数|商品数|件数|箱数|袋数|台数|人数|次数|行数|库存(?:数|量)?|总数|总量|合计数/.test(label)) return 'count'
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

/** 原样展示族：编码/订单/客户/商品/地理 —— 非数值语义，命中即 `String(v)` 原样返回。 */
const RAW_SEMANTICS: ReadonlySet<Semantic> = new Set(['order', 'customer', 'goods', 'geo'])

/** 按语义格式化单元格显示值。
 *
 *  ⚠️ percent 合同：输入**必须已经是 0-100 的百分数**（19.63 →「19.6%」），fmt 内部**不做 ×100**。
 *  后端给的 ratio 原值（0.1963）由调用方先 ×100（现约定见 ResultPanel.vue 的 displayValue、
 *  BiChart 的轴格式化）—— 新调用方别指望 fmt 帮你乘，忘了乘就是静默错 100 倍。 */
export function fmt(v: unknown, semantic: Semantic = 'none'): string {
  // 纯空白串同空值：不把空格占位当上屏内容
  if (v === null || v === undefined || v === '' || (typeof v === 'string' && !v.trim())) return ''
  // 地理：省级区划码翻省名
  if (semantic === 'geo') {
    const name = PROVINCE[String(v)]
    if (name) return name
  }
  const n = toNum(v)
  // 编码/订单/名称等非数值语义原样展示。
  if (n === null || RAW_SEMANTICS.has(semantic)) {
    return String(v)
  }
  if (semantic === 'percent') return `${round(n, 1)}%`
  // 负号在 ¥ 之前（财务惯例 -¥1.23万，也与 grouping 路径的负号位置一致）
  if (semantic === 'money') return `${n < 0 ? '-' : ''}¥${compress(Math.abs(n))}`
  if (semantic === 'count') return compress(n)
  // 普通维度可能是年份、日期码或未登记编码，不能擅自按“万”压缩。
  return String(v)
}

/** 业务数值：绝对值满 1 万按“万”展示，全端统一**恰好** 2 位小数（2026-08-10 裁决）。 */
export function compress(n: number): string {
  const abs = Math.abs(n)
  // round 先修浮点边界（1.005 的二进制表示会被裸 toFixed 截成「1.00」），toFixed 保住恰好 2 位
  if (abs >= 1e4) return `${round(n / 1e4, 2).toFixed(2)}万`
  return grouping(n)
}

// 模块级单例：结果表几百个单元格 × 每次重渲染都过这里，不能一格 new 一个
//（对齐 ResultPanel.vue 的 deltaNumber / ppNumber 写法）。
const GROUPING = new Intl.NumberFormat('zh-CN', {
  minimumFractionDigits: 0,
  maximumFractionDigits: 2,
  useGrouping: true,
})

function grouping(n: number): string {
  // 防 -0：|n| 小于半个最小刻度按 0 显示，否则 Intl 会输出「-0」
  const value = Math.abs(n) < 0.0005 ? 0 : n
  return GROUPING.format(value)
}

function round(n: number, d: number): number {
  const p = 10 ** d
  // +Number.EPSILON 修二进制浮点边界：1.005*100 = 100.49999… 直接 Math.round 会丢 0.01
  return Math.round((n + Number.EPSILON) * p) / p
}
