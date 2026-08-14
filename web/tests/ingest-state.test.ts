import test from 'node:test'
import assert from 'node:assert/strict'
import { ingestUploadState, isActiveIngest, isTerminalIngest } from '../src/ingest-state.ts'

test('pending/parsing 是后台进行态，不是失败', () => {
  for (const status of ['pending', 'parsing']) {
    assert.equal(isActiveIngest({ status }), true)
    assert.equal(isTerminalIngest({ status }), false)
    assert.equal(ingestUploadState({ status }), 'doing')
  }
})

test('重新打开面板可识别仍在运行的影子入库', () => {
  assert.equal(isActiveIngest({ status: 'embedded', last_ingest_status: 'processing' }), true)
  assert.equal(isActiveIngest({ status: 'embedded', last_ingest_status: 'running' }), true)
  assert.equal(isActiveIngest({ status: 'embedded' }), false)
  assert.equal(isActiveIngest({ status: 'failed' }), false)
})

test('入库终态与降级态映射', () => {
  assert.equal(ingestUploadState({ status: 'embedded' }), 'ok')
  assert.equal(ingestUploadState({ status: 'failed' }), 'fail')
  assert.equal(ingestUploadState({ status: 'chunked', notice: '待补向量' }), 'partial')
  assert.equal(ingestUploadState({ status: 'chunked' }), 'doing')
})
