// citation.ts 的单测：node --test 直接跑（Node ≥22.18 原生剥类型，无需装依赖）。
// 跑法：cd web && node --test tests/citation.test.ts
// 放在 tests/ 而非 src/：tsconfig 只 include src/**，别让应用侧 vue-tsc 去收 node:test 的类型。
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { dedupeFirstIndex, dedupeKey, folderTextOf, headingTextOf, locationParts } from '../src/citation.ts'

test('headingTextOf：数组按 " > " 拼接，字符串原样，空值归一', () => {
  assert.equal(headingTextOf({ doc_id: 'a', heading_path: ['第一章', '第二节'] }), '第一章 > 第二节')
  assert.equal(headingTextOf({ doc_id: 'a', heading_path: '第一章 > 第二节' }), '第一章 > 第二节')
  assert.equal(headingTextOf({ doc_id: 'a', heading_path: ['', '  '] }), '')
  assert.equal(headingTextOf({ doc_id: 'a' }), '')
  assert.equal(headingTextOf({ doc_id: 'a', heading_path: null }), '')
})

test('folderTextOf：folder_path 优先，directory_path 兜底，根目录视同无', () => {
  assert.equal(folderTextOf({ doc_id: 'a', folder_path: '/市场管理/门店管理' }), '/市场管理/门店管理')
  // 兜底只看空值（'' / null / undefined）；'/' 是真值，选中后按「根目录 = 无」归空
  assert.equal(folderTextOf({ doc_id: 'a', folder_path: '', directory_path: '/兜底' }), '/兜底')
  assert.equal(folderTextOf({ doc_id: 'a', folder_path: '/', directory_path: '/兜底' }), '')
  assert.equal(folderTextOf({ doc_id: 'a', folder_path: '/' }), '')
  assert.equal(folderTextOf({ doc_id: 'a' }), '')
})

test('locationParts：目录/章节/页码按序出，有才出', () => {
  const parts = locationParts({
    doc_id: 'a', folder_path: '/市场管理', heading_path: '总则 > 流程', page: 3,
  })
  assert.deepEqual(parts.map((p) => p.kind), ['folder', 'heading', 'page'])
  assert.deepEqual(parts.map((p) => p.text), ['📁 /市场管理', '章节：总则 > 流程', '第 3 页'])
  // 全无 = 空数组（来源行降级为纯文档名，不留空徽标）
  assert.deepEqual(locationParts({ doc_id: 'a' }), [])
  assert.deepEqual(locationParts({ doc_id: 'a', page: null, folder_path: '/' }), [])
  // page 非正整数/非数字不出页码徽标
  assert.deepEqual(locationParts({ doc_id: 'a', page: 0 }).map((p) => p.kind), [])
})

test('locationParts：章节超 40 字截断补 …，完整串留在 full（title 兜底）', () => {
  const long = '非常长的章节标题'.repeat(6) // 48 字
  const [part] = locationParts({ doc_id: 'a', heading_path: long })
  assert.equal(part.text, `章节：${long.slice(0, 40)}…`)
  assert.equal(part.full, long)
  // 恰好 40 字不截断
  const exact = '章节标题'.repeat(10)
  assert.equal(locationParts({ doc_id: 'a', heading_path: exact })[0].text, `章节：${exact}`)
})

test('dedupeFirstIndex：相同 (doc_id+页+章节) 去重保序，不同位置分行', () => {
  const list = [
    { doc_id: 'a', page: 1, heading_path: 'X' },
    { doc_id: 'a', page: 1, heading_path: 'X' },   // 重复 → 去掉
    { doc_id: 'a', page: 2, heading_path: 'X' },   // 不同页 → 留
    { doc_id: 'b', page: 1, heading_path: 'X' },   // 不同文档 → 留
    { doc_id: 'a', page: 1, heading_path: ['X'] }, // 数组形态与字符串同键 → 去掉
  ]
  assert.deepEqual(dedupeFirstIndex(list), [0, 2, 3])
  assert.equal(dedupeKey(list[0]), dedupeKey(list[4]))
})
