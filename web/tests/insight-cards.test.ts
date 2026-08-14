// insight-cards.ts 的单测：node --test 直接跑（Node ≥22.18 原生剥类型，无需装依赖）。
// 跑法：cd web && node --test tests/insight-cards.test.ts
import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { buildInsightCards, isTableSeparator, markdownTableCells, sanitizeInsight } from '../src/insight-cards.ts'

/** crates/agent/src/insight.rs 的 SYSTEM_DEEP 逐字要求的板块。 */
const DEEP = [
  '## 核心结论',
  '本月销售额 120 万元，环比增长 8%。',
  '## 异常与机会',
  '| 发现 | 数据证据 | 业务影响 |',
  '|---|---|---|',
  '| 华东下滑 | 华东 12 万元，环比 -15% | 拖低整体增速 |',
  '## 行动建议',
  '| 优先级 | 动作 | 依据 |',
  '|---|---|---|',
  '| 高 | 核实华东订单明细 | 华东环比 -15% |',
].join('\n')

/** SYSTEM_DEEP_DOCUMENT 的三个板块，前端旧白名单一个都不认。 */
const DOCUMENT = [
  '## 单据结论',
  '该单为已审核销售出库单，客户 A，金额 3.2 万元。',
  '## 关键明细',
  '| 核验项 | 数据证据 |',
  '| :--- | ---: |',
  '| 出库数量 | 120 件 |',
  '## 后续核验',
  '核对关联销售订单号是否一致。',
].join('\n')

/** SYSTEM_DEEP_ENTITY 的三个板块。 */
const ENTITY = [
  '## 实体结论',
  '客户 A 为华东区年采购 480 万元的经销商。',
  '## 数据观察',
  '| 观察 | 数据证据 |',
  '|---|---|',
  '| 采购集中在两个品类 | 前两类占 78% |',
  '## 建议动作',
  '下钻该客户订单明细核实构成。',
].join('\n')

function card(md: string, kind: string) {
  return buildInsightCards(md).find((c) => c.kind === kind)
}

test('SYSTEM_DEEP 的三个板块各自落桶（## 异常与机会 不再并进上一个桶）', () => {
  const cards = buildInsightCards(DEEP)
  assert.deepEqual(cards.map((c) => c.kind), ['conclusion', 'risk', 'action'])
  assert.deepEqual(card(DEEP, 'conclusion')?.items, ['本月销售额 120 万元，环比增长 8%。'])
  assert.deepEqual(card(DEEP, 'risk')?.items, ['华东下滑；华东 12 万元，环比 -15%；拖低整体增速'])
  assert.deepEqual(card(DEEP, 'action')?.items, ['高；核实华东订单明细；华东环比 -15%'])
})

test('SYSTEM_DEEP_DOCUMENT 的单据结论/关键明细/后续核验都有可见归宿', () => {
  const cards = buildInsightCards(DOCUMENT)
  assert.deepEqual(card(DOCUMENT, 'conclusion')?.items, ['该单为已审核销售出库单，客户 A，金额 3.2 万元。'])
  assert.deepEqual(card(DOCUMENT, 'action')?.items, ['核对关联销售订单号是否一致。'])
  // 「关键明细」不是结论也不是建议：进通用桶展示，不许被并进上一个桶
  assert.deepEqual(card(DOCUMENT, 'other')?.items, ['出库数量；120 件'])
  assert.ok(cards.every((c) => c.title))
})

test('SYSTEM_DEEP_ENTITY 的实体结论/数据观察/建议动作都有可见归宿', () => {
  assert.deepEqual(card(ENTITY, 'conclusion')?.items, ['客户 A 为华东区年采购 480 万元的经销商。'])
  assert.deepEqual(card(ENTITY, 'other')?.items, ['采购集中在两个品类；前两类占 78%'])
  assert.deepEqual(card(ENTITY, 'action')?.items, ['下钻该客户订单明细核实构成。'])
})

test('白名单外的标题进通用桶，正文不再被上一个桶吞掉', () => {
  const md = ['## 核心结论', '本月销售额 120 万元。', '## 供应链诊断', '在途库存 3 万件。'].join('\n')
  const cards = buildInsightCards(md)
  assert.deepEqual(card(md, 'conclusion')?.items, ['本月销售额 120 万元。'])
  assert.deepEqual(card(md, 'other')?.items, ['在途库存 3 万件。'])
  assert.ok(cards.find((c) => c.kind === 'other')?.title, '通用桶必须有可见标题')
})

test('表头判据是「下一行是分隔行」，不是中文词表', () => {
  // 旧白名单里没有「发现」「动作」「依据」→ 表头被当数据渲染成一张洞察卡
  assert.deepEqual(markdownTableCells('| 发现 | 数据证据 | 业务影响 |', '|---|---|---|'), [])
  assert.deepEqual(markdownTableCells('| 优先级 | 动作 | 依据 |', '|---|---|---|'), [])
  assert.deepEqual(markdownTableCells('| 核验项 | 数据证据 |', '| :--- | ---: |'), [])
  // 同样的词出现在数据行（下一行不是分隔行）时必须照常渲染
  assert.deepEqual(markdownTableCells('| 发现 | 数据证据 | 业务影响 |', '| 华东 | -15% | 拖低 |'), ['发现', '数据证据', '业务影响'])
  assert.deepEqual(markdownTableCells('| 结论 | 建议 |', undefined), ['结论', '建议'])
  assert.equal(markdownTableCells('普通句子。', undefined), null)
  for (const md of [DEEP, DOCUMENT, ENTITY]) {
    for (const c of buildInsightCards(md)) {
      assert.ok(!c.items.some((i) => i.includes('数据证据')), `表头被当数据渲染：${JSON.stringify(c.items)}`)
    }
  }
})

test('分隔行本身不渲染（|---|---| 无空格也要认出来）', () => {
  assert.ok(isTableSeparator('|---|---|'))
  assert.ok(isTableSeparator('| :--- | ---: | :---: |'))
  assert.ok(isTableSeparator('| - | - |'))
  assert.ok(!isTableSeparator('| 发现 | 证据 |'))
  assert.ok(!isTableSeparator(undefined))
  assert.deepEqual(markdownTableCells('|---|---|', undefined), [])
  const md = ['## 核心结论', '| a | b |', '|---|---|', '| 1 | 2 |'].join('\n')
  assert.deepEqual(card(md, 'conclusion')?.items, ['1；2'])
})

test('超出上限的条目不静默丢弃，桶尾留一条「还有 N 条未展示」', () => {
  const md = ['## 核心结论', '第一句。', '第二句。', '第三句。', '第四句。', '第五句。'].join('\n')
  assert.deepEqual(card(md, 'conclusion')?.items, ['第一句。', '第二句。', '第三句。', '还有 2 条未展示'])
})

test('未超上限时不出现未展示提示', () => {
  const md = ['## 核心结论', '第一句。', '第二句。'].join('\n')
  assert.deepEqual(card(md, 'conclusion')?.items, ['第一句。', '第二句。'])
})

test('sanitizeInsight 不许改写模型措辞：证据不足 ≠ 数据不足', () => {
  const text = '## 核心结论\n证据不足以支撑该因果判断。'
  assert.match(sanitizeInsight(text), /证据不足以支撑该因果判断/)
  assert.doesNotMatch(sanitizeInsight(text), /数据不足/)
  const src = readFileSync(new URL('../src/insight-cards.ts', import.meta.url), 'utf8')
  assert.doesNotMatch(src, /replace\(\/证据不足\/g/, '全局替换会把「有数推不出」改成「没数」，方向相反')
})

test('sanitizeInsight 仍然屏蔽内部证据编号与技术板块', () => {
  const text = ['## 核心结论', '销售额 120 万元 [KPI-1]。', '## 证据与边界', '内部：SEC-2'].join('\n')
  const out = sanitizeInsight(text)
  assert.doesNotMatch(out, /KPI-1|SEC-2|证据与边界/)
  assert.match(out, /销售额 120 万元。/)
})

test('意图 mode/slot 标签有兜底，后端新增取值不渲染成空白', () => {
  const panel = readFileSync(new URL('../src/ResultPanel.vue', import.meta.url), 'utf8')
  assert.match(panel, /INTENT_MODE_LABEL\[intentSummary\.mode\] \?\? intentSummary\.mode/)
  assert.match(panel, /INTENT_MODE_LABEL\[summary\.mode\] \?\? summary\.mode/)
  assert.match(panel, /INTENT_SLOT_LABEL\[slot\.kind\] \?\? slot\.kind/)
})
