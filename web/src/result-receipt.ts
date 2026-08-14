/** 问数/知识/混合结果共用的意图收据 wire 类型。 */
export interface IntentSlot {
  kind: 'metric' | 'entity' | 'region' | 'time' | 'filter' | 'breakdown' | 'comparison' | 'detail'
  surface: string
  state: 'grounded' | 'resolved'
}

export interface IntentSummary {
  mode: 'data' | 'knowledge' | 'hybrid' | 'unknown'
  status: 'grounded' | 'clarification' | 'blocked'
  slots: IntentSlot[]
  coverage: { status: 'complete' | 'blocked'; issues: string[] }
}

const SLOT_LABEL: Record<string, string> = {
  metric: '指标', entity: '业务对象', region: '地区', date: '日期', time: '时间范围',
  filter: '筛选条件', breakdown: '拆分维度', comparison: '比较要求', detail: '明细要求',
}

const EXACT_ISSUE_TEXT: Record<string, string> = {
  'coverage:not-evaluated': '本次结果尚未完成意图覆盖校验',
  'route:unknown': '尚未确定应使用问数还是知识检索，需要补充问题限定',
  'sql:coverage-unverifiable': '无法证明当前执行计划已完整覆盖问题条件',
  'knowledge:no-citation': '本回答没有可核验的知识库来源',
  'knowledge:failed': '知识检索执行失败',
  'hybrid:data-incomplete': '问数部分未完整覆盖问题条件',
  'hybrid:data-unverified': '问数部分未返回可核验的意图收据',
  'hybrid:data-failed': '问数部分执行失败',
  'hybrid:knowledge:no-citation': '知识部分没有可核验的来源',
  'hybrid:knowledge-failed': '知识检索部分执行失败',
  'hybrid:unsupported-cardinality': '一次问题包含多个问数或知识子任务；为避免漏答，请拆成多个问题后分别提交',
  'result:empty': '查询已执行，但没有返回可验证的数据',
  'result:row-count-mismatch': '返回行数与结果载荷不一致，结果已降级为待复核',
  'result:column-shape-mismatch': '主结果列结构不完整，结果已降级为待复核',
  'result:supplemental-row-count-mismatch': '补充明细行数不一致，结果已降级为待复核',
  'result:supplemental-column-shape-mismatch': '补充明细列结构不完整，结果已降级为待复核',
  'result:sales-context-shape-mismatch': '销售背景数据结构不完整，结果已降级为待复核',
  'result:comparison-invalid': '对比值未通过数值与计算公式校验',
  'result:comparison-incomplete': '问题要求对比，但结果没有返回完整的基期与变化值',
  'result:comparison-current-mismatch': '对比结果中的本期值与主查询结果不一致',
  'result:detail-empty': '问题要求明细，但本次只返回了汇总结果',
}

/** 后端 issue code 只是协议字段；UI 始终输出可行动的中文，不泄漏内部码。 */
export function intentIssueText(issue: string): string {
  const clean = issue.trim()
  if (!clean) return '部分问题条件尚未通过验证'
  if (EXACT_ISSUE_TEXT[clean]) return EXACT_ISSUE_TEXT[clean]
  if (clean.startsWith('data:')) return `问数部分：${intentIssueText(clean.slice(5))}`
  if (clean.startsWith('result:metric-unverified:')) {
    const metric = clean.slice('result:metric-unverified:'.length).trim()
    return metric ? `结果中没有找到可验证的指标「${metric}」` : '结果指标尚未通过验证'
  }

  const [slot, ...rest] = clean.split(':')
  const value = rest.join('：').trim()
  const label = SLOT_LABEL[slot]
  if (label) return value ? `尚未验证${label}「${value}」` : `尚未验证${label}`
  if (slot === 'ambiguity') return value ? `问题存在歧义：${value}` : '问题存在歧义，需要确认'
  if (slot === 'conflict') return value ? `问题条件存在冲突：${value}` : '问题条件存在冲突'
  if (slot === 'missing') return value ? `问题条件尚未覆盖：${value}` : '问题条件尚未覆盖'
  if (slot === 'unverifiable') return value ? `问题条件尚未获得执行证据：${value}` : '问题条件尚未获得执行证据'
  return '部分问题条件尚未通过验证'
}

export function isReceiptBlocked(summary?: IntentSummary): boolean {
  return summary?.coverage.status === 'blocked'
}

/** 混合结果的知识子卡只展示知识侧收据，不把问数缺口误挂到来源卡上。 */
export function projectKnowledgeReceipt(summary: IntentSummary | undefined, hasCitation: boolean): IntentSummary | undefined {
  if (!summary || summary.mode !== 'hybrid') return summary
  const issues = summary.coverage.issues.flatMap((issue) => {
    if (issue === 'hybrid:knowledge:no-citation') return ['knowledge:no-citation']
    if (issue === 'hybrid:knowledge-failed') return ['knowledge:failed']
    return []
  })
  if (!hasCitation && !issues.includes('knowledge:no-citation')) issues.push('knowledge:no-citation')
  const complete = issues.length === 0
  return {
    ...summary,
    mode: 'knowledge',
    status: complete ? 'grounded' : 'blocked',
    coverage: { status: complete ? 'complete' : 'blocked', issues },
  }
}
