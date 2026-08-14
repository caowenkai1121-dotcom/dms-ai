import test from 'node:test'
import assert from 'node:assert/strict'
import { intentIssueText, isReceiptBlocked, projectKnowledgeReceipt } from '../src/result-receipt.ts'

test('intent issue 不泄漏后端内部 code', () => {
  assert.equal(intentIssueText('knowledge:no-citation'), '本回答没有可核验的知识库来源')
  assert.equal(
    intentIssueText('hybrid:unsupported-cardinality'),
    '一次问题包含多个问数或知识子任务；为避免漏答，请拆成多个问题后分别提交',
  )
  assert.equal(intentIssueText('metric:销售额'), '尚未验证指标「销售额」')
  assert.equal(intentIssueText('data:region:山东省'), '问数部分：尚未验证地区「山东省」')
  assert.equal(intentIssueText('result:comparison-invalid'), '对比值未通过数值与计算公式校验')
  assert.equal(intentIssueText('result:metric-unverified:库存量'), '结果中没有找到可验证的指标「库存量」')
  assert.equal(intentIssueText('future:new-code'), '部分问题条件尚未通过验证')
})

test('混合结果的知识卡不携带问数侧缺口', () => {
  const projected = projectKnowledgeReceipt({
    mode: 'hybrid', status: 'blocked', slots: [],
    coverage: { status: 'blocked', issues: ['hybrid:data-incomplete', 'data:metric:销售额'] },
  }, true)
  assert.deepEqual(projected?.coverage, { status: 'complete', issues: [] })

  const uncited = projectKnowledgeReceipt({
    mode: 'hybrid', status: 'blocked', slots: [],
    coverage: { status: 'blocked', issues: ['hybrid:knowledge:no-citation'] },
  }, false)
  assert.deepEqual(uncited?.coverage, { status: 'blocked', issues: ['knowledge:no-citation'] })
})

test('blocked 收据只看 coverage 合同', () => {
  assert.equal(isReceiptBlocked(undefined), false)
  assert.equal(isReceiptBlocked({ mode: 'knowledge', status: 'grounded', slots: [], coverage: { status: 'blocked', issues: [] } }), true)
})
