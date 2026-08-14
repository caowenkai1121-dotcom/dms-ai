import test from 'node:test'
import assert from 'node:assert/strict'
import { snapshotQueuedAsk } from '../src/ask-queue.ts'

test('入队时冻结意图、模式、知识空间和引用', () => {
  const refs = ['上一轮结论']
  const queued = snapshotQueuedAsk('q1', '保修期多久', refs, {
    intent: 'knowledge', mode: 'lite', spaceId: 'space-a',
  })
  refs.push('后来新增')
  assert.deepEqual(queued, {
    id: 'q1', text: '保修期多久', refs: ['上一轮结论'],
    forceIntent: 'knowledge', forceMode: 'lite', spaceId: 'space-a',
  })
})
