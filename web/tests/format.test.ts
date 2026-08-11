// format.ts 的单测：node --test 直接跑（Node ≥22.18 原生剥类型，无需装依赖）。
// 跑法：cd web && node --test tests/format.test.ts
// 放在 tests/ 而非 src/：tsconfig 只 include src/**，别让应用侧 vue-tsc 去收 node:test 的类型。
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { compress, fmt, semanticForLabel, toNum } from '../src/format.ts'

test('toNum 千分位只认合法分组，畸形串判非数值（不静默洗白）', () => {
  assert.equal(toNum('1,234.5'), 1234.5)
  assert.equal(toNum('12,345'), 12345)
  assert.equal(toNum('-1,234'), -1234)
  assert.equal(toNum('1,2,3'), null)
  assert.equal(toNum(',,5'), null)
  assert.equal(toNum('1,23'), null)
  assert.equal(toNum(' 42 '), 42)
  assert.equal(toNum('.5'), 0.5)
  assert.equal(toNum('abc'), null)
  assert.equal(toNum(7), 7)
  assert.equal(toNum(null), null)
})

test('semanticForLabel：裸「率」收窄为词尾 + 排除物理/金融比率', () => {
  assert.equal(semanticForLabel('毛利率'), 'percent')
  assert.equal(semanticForLabel('费用率'), 'percent')
  assert.equal(semanticForLabel('环比增长率'), 'percent')
  assert.equal(semanticForLabel('汇率'), 'none')
  assert.equal(semanticForLabel('频率'), 'none')
  assert.equal(semanticForLabel('功率'), 'none')
  assert.equal(semanticForLabel('占比'), 'percent')
})

test('semanticForLabel：同比/环比与额/量词尾联合判定', () => {
  assert.equal(semanticForLabel('销售额同比'), 'percent')
  assert.equal(semanticForLabel('销量环比'), 'percent')
  // 增长额/增量是金额与单量，不许加 %
  assert.notEqual(semanticForLabel('同比增长额'), 'percent')
  assert.notEqual(semanticForLabel('环比增量'), 'percent')
})

test('semanticForLabel：money 补营收/售价/现价，库存允许裸词', () => {
  assert.equal(semanticForLabel('营收'), 'money')
  assert.equal(semanticForLabel('售价'), 'money')
  assert.equal(semanticForLabel('现价'), 'money')
  assert.equal(semanticForLabel('库存'), 'count')
  assert.equal(semanticForLabel('库存量'), 'count')
})

test('semanticForLabel：标识列优先于指标词', () => {
  assert.equal(semanticForLabel('订单编号'), 'order')
  assert.equal(semanticForLabel('税率编码'), 'order')
  assert.equal(semanticForLabel('user_id'), 'order')
  assert.equal(semanticForLabel('id'), 'order')
})

test('fmt percent 合同：输入已是 0-100，内部不 ×100', () => {
  assert.equal(fmt(19.63, 'percent'), '19.6%')
  assert.equal(fmt(0.1963, 'percent'), '0.2%') // ratio 原值由调用方先 ×100，忘了就长这样
})

test('fmt money：负号在 ¥ 之前', () => {
  assert.equal(fmt(-12300, 'money'), '-¥1.23万')
  assert.equal(fmt(-500, 'money'), '-¥500')
  assert.equal(fmt(9500, 'money'), '¥9,500')
})

test('compress：恰好 2 位小数 + 浮点边界修正', () => {
  assert.equal(compress(10000), '1.00万')
  assert.equal(compress(10050), '1.01万') // 1.005 边界：裸 toFixed 会截成 1.00
  assert.equal(compress(123456.789), '12.35万')
  assert.equal(compress(-0.0001), '0') // 防 -0
})

test('fmt：纯空白串归空', () => {
  assert.equal(fmt('   ', 'none'), '')
  assert.equal(fmt('', 'money'), '')
  assert.equal(fmt(null, 'count'), '')
})
