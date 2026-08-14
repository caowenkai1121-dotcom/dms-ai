/** AI 深度解读正文（markdown）→ 结论/异常/建议卡片。
 *
 *  从 ResultPanel.vue 抽出来只为一件事：这套解析有真实判据（见 tests/insight-cards.test.ts），
 *  .vue 里的私有函数没法直接跑。渲染仍然只在 ResultPanel.vue。
 *
 *  分桶标题必须与 crates/agent/src/insight.rs 的三份 SYSTEM_DEEP* prompt 对齐：
 *    SYSTEM_DEEP          ## 核心结论 / ## 异常与机会 / ## 行动建议
 *    SYSTEM_DEEP_DOCUMENT ## 单据结论 / ## 关键明细 / ## 后续核验
 *    SYSTEM_DEEP_ENTITY   ## 实体结论 / ## 数据观察 / ## 建议动作
 *  但**白名单不是判据**：任何没列进来的标题都落进 `other` 通用桶照常展示，
 *  绝不再并进上一个桶被关键词正则乱分。
 */

export type InsightKind = 'conclusion' | 'risk' | 'action' | 'other'
export interface InsightCard { kind: InsightKind; title: string; items: string[] }

/** 卡片顺序 = 展示顺序；`other` 垫底但**可见**。 */
export const INSIGHT_KINDS: InsightKind[] = ['conclusion', 'risk', 'action', 'other']

const INSIGHT_TITLE: Record<InsightKind, string> = {
  conclusion: '结论',
  risk: '异常与关注',
  action: '建议',
  other: '其他要点',
}

/** 已知标题 → 桶。命不中不代表丢弃，见 `headingKind`。 */
const HEADING_KIND: [RegExp, InsightKind][] = [
  [/^(?:核心结论|单据结论|实体结论|经营结论|结论|摘要|概览|总体结论)$/, 'conclusion'],
  [/^(?:异常与机会|异常与关注|关键变化|模块分析|关键发现|异常|风险|问题|预警)$/, 'risk'],
  [/^(?:行动建议|后续核验|建议动作|下周行动建议|优化建议|建议|下一步|措施)$/, 'action'],
  [/^(?:关键明细|数据观察)$/, 'other'],
]

/** 每桶最多展示几条（多出来的不静默吞，见 `OVERFLOW_NOTE`）。 */
const BUCKET_LIMIT = 3
const OVERFLOW_NOTE = (n: number) => `还有 ${n} 条未展示`

function headingKind(heading: string): InsightKind {
  return HEADING_KIND.find(([re]) => re.test(heading))?.[1] ?? 'other'
}

/** markdown 语法上的标题行：`## 标题` 或整行加粗。用它判断「这行是不是标题」，
 *  而不是用词表 —— 词表外的标题以前会被当正文，整块内容跟着错桶。 */
function syntacticHeading(rawLine: string): string | null {
  const t = rawLine.trim()
  if (!/^#{1,6}\s*\S/.test(t) && !/^(?:\*\*[^*]+\*\*|__[^_]+__)$/.test(t)) return null
  return cleanInsight(t).replace(/[：:]$/, '').trim() || null
}

export function buildInsightCards(insight: string): InsightCard[] {
  if (!insight) return []
  const buckets: Record<InsightKind, string[]> = { conclusion: [], risk: [], action: [], other: [] }
  const dropped: Record<InsightKind, number> = { conclusion: 0, risk: 0, action: 0, other: 0 }
  let current: InsightKind = 'conclusion'
  // insight 已经过 sanitizeInsight（那边去过 \r），这里不再重复洗
  const lines = insight.split('\n')
  for (const [index, rawLine] of lines.entries()) {
    const line = cleanInsight(rawLine)
    if (!line) continue
    const heading = syntacticHeading(rawLine)
      ?? (HEADING_KIND.some(([re]) => re.test(line.replace(/[：:]$/, ''))) ? line.replace(/[：:]$/, '') : null)
    if (heading) { current = headingKind(heading); continue }

    const cells = markdownTableCells(rawLine, lines[index + 1])
    if (cells?.length === 0) continue
    const fragments = cells
      ? [cells.join('；')]
      : (line.match(/[^。！？；]+[。！？；]?/g) ?? [line])
    for (const sentence of fragments) {
      const text = cleanInsight(sentence)
      if (!text) continue
      const kind = cells
        ? current
        : /建议|应当|可以|优先|下一步|行动|跟进|排查|优化|关注/.test(text)
          ? 'action'
          : /异常|风险|问题|预警|下降|波动|偏低|偏高|集中|缺失|失衡/.test(text)
            ? 'risk'
            : current
      const clipped = clipInsight(text)
      if (buckets[kind].includes(clipped)) continue
      if (buckets[kind].length < BUCKET_LIMIT) buckets[kind].push(clipped)
      else dropped[kind] += 1
    }
  }

  return INSIGHT_KINDS
    .filter((kind) => buckets[kind].length)
    .map((kind) => ({
      kind,
      title: INSIGHT_TITLE[kind],
      items: dropped[kind] ? [...buckets[kind], OVERFLOW_NOTE(dropped[kind])] : buckets[kind],
    }))
}

export function sanitizeInsight(text: string): string {
  const visible: string[] = []
  let hidingInternalSection = false
  for (const rawLine of text.replace(/\r/g, '').split('\n')) {
    const plainLine = rawLine.trim().replace(/^[-*>\d.、\s]+/, '').replace(/\*\*|__/g, '')
    const heading = /^(#{1,6})\s*(.*)$/.exec(rawLine.trim())
    const strongHeading = /^(?:\*\*([^*]+)\*\*|__([^_]+)__)$/.exec(rawLine.trim())
    const headingText = heading?.[2] ?? strongHeading?.[1] ?? strongHeading?.[2]
    if (headingText !== undefined) {
      hidingInternalSection = /^(?:证据|证据与边界|数据边界|可信度|技术诊断|口径与可信度|内部校验)/.test(headingText.trim())
      if (hidingInternalSection) continue
    } else if (hidingInternalSection) {
      continue
    }
    if (/^(?:证据(?:编号|与边界)?|数据边界|可信度|技术诊断|口径与可信度|内部校验)\s*[:：|]/.test(plainLine)) continue
    if (rawLine.includes('|') && /\b(?:KPI|SEC|CON)-\d+\b/i.test(rawLine)) continue
    if (!/^\s*\|?\s*(?:证据|证据编号|可信度|技术诊断)\s*\|/.test(rawLine)) visible.push(rawLine)
  }
  // 这里**不许**再改模型正文措辞：曾经把「证据不足」全局换成「数据不足」，
  // 「证据不足以支撑该因果判断」（有数推不出）被改成「数据不足…」（没数），方向相反。
  return visible.join('\n')
    .replace(/\[(?:KPI|SEC|CON)-\d+\]/gi, '')
    .replace(/\b(?:KPI|SEC|CON)-\d+\b/gi, '')
    .replace(/(?:证据编号|KPI引用|SEC引用)\s*[:：]?\s*/gi, '')
    .replace(/\s+([，。；：])/g, '$1')
    .trim()
}

/** GFM 分隔行：`|---|`、`| :--- | ---: |`、`| :-: |`，最少一个 `-`（同 knowledge/src/answer.rs 的 is_table_separator）。 */
export function isTableSeparator(line: string | undefined): boolean {
  const t = line?.trim()
  if (!t || !t.includes('|') || !t.includes('-')) return false
  const cells = t.replace(/^\|/, '').replace(/\|$/, '').split('|').map((c) => c.trim())
  return cells.length > 0 && cells.every((c) => /^:?-+:?$/.test(c))
}

/** 返回该行的单元格；`[]` = 该行是分隔行或表头（不渲染）；`null` = 不是表格行。
 *  表头判据是 GFM 的真判据 —— **下一行是分隔行**，不是中文词表（词表外的表头会被当数据渲染成卡片）。 */
export function markdownTableCells(rawLine: string, nextLine?: string): string[] | null {
  const line = rawLine.trim()
  if (!line.includes('|')) return null
  if (isTableSeparator(line)) return []
  const cells = line
    .replace(/^\|/, '')
    .replace(/\|$/, '')
    .split('|')
    .map(cleanInsight)
    .filter(Boolean)
  if (cells.length < 2) return null
  return isTableSeparator(nextLine) ? [] : cells
}

export function cleanInsight(text: string): string {
  return text
    .replace(/^#{1,6}\s*/, '')
    .replace(/^[-*•]\s*/, '')
    .replace(/\*\*|__/g, '')
    .replace(/`/g, '')
    .trim()
}

export function clipInsight(text: string): string {
  // Array.from 按码点切：slice 会在 emoji/代理对中间砍出乱码
  return text.length > 76 ? `${Array.from(text).slice(0, 75).join('').trim()}…` : text
}
