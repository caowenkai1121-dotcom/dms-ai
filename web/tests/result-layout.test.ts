import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

const app = readFileSync(new URL('../src/App.vue', import.meta.url), 'utf8')

test('深度与精简模式的结构化结果都使用全宽结果气泡', () => {
  assert.match(app, /'result-bubble': t\.result\.kind !== 'text'/)
  assert.doesNotMatch(app, /'result-bubble':[^}\n]*t\.mode !== 'deep'/)
})

test('桌面结构化结果按主栏比例伸缩且不按内容塌成窄列', () => {
  assert.match(app, /\.chat \{[^}]*min-width: 0;/)
  assert.match(app, /\.turn \{[^}]*width: 100%;[^}]*min-width: 0;/)
  assert.match(app, /\.bubble\.ai\.result-bubble \{[^}]*width: 82%;[^}]*max-width: 100%;[^}]*min-width: 0;/)
  assert.doesNotMatch(app, /\.bubble\.ai\.result-bubble \{[^}]*1120px/)
  assert.match(app, /\.bubble\.ai\.result-bubble > \.result-panel \{[^}]*width: 100%;[^}]*min-width: 0;/)
})

test('移动端结构化结果恢复全宽单列', () => {
  assert.match(app, /@media \(max-width: 820px\) \{[\s\S]*?\.bubble\.ai\.result-bubble \{ width: 100%; max-width: 100%; \}/)
})

test('AI 综合分析只渲染一遍：三块 insight 串成一条 v-if 链', () => {
  // 混合结果同时命中「kb && view.insight」与「subs && compoundAnalysis」，
  // 而 compoundAnalysis 返回的正是 view.insight —— 同一段文字上下贴着出两块。
  // 中间那块必须是 v-else-if，否则链断开。
  assert.match(app, /<div v-else-if="t\.page\?\.insight" class="ai-panel deep-insight">/)
  assert.match(app, /<div v-else-if="t\.result\.subs\?\.length && compoundAnalysis\(t\.result\)"/)
})

test('sticky 首列 hover 不用半透明底（横滚时会透出下层文字）', () => {
  assert.match(app, /\.dtable tr:hover td:first-child \{ background: color-mix\(/)
  const panel = readFileSync(new URL('../src/ResultPanel.vue', import.meta.url), 'utf8')
  assert.match(panel, /\.tbl-wrap tbody tr:hover \.row-index \{ background: color-mix\(/)
})

test('单张 KPI 卡不吃满整行（auto-fit 空轨道会塌）', () => {
  const panel = readFileSync(new URL('../src/ResultPanel.vue', import.meta.url), 'utf8')
  assert.match(panel, /\.kpi-row:not\(\.solo\) \{ grid-template-columns: repeat\(auto-fit, minmax\(180px, 300px\)\)/)
})

test('零消费者的全局样式已删除', () => {
  for (const dead of ['scope-note', 'tbl-foot']) {
    assert.doesNotMatch(app, new RegExp(dead), `${dead} 全仓无引用，留着会让人以为存在第二条呈现路径`)
  }
})
