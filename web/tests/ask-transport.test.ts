import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createSessionExpiryGuard, runAskTransport } from '../src/ask-transport.ts'

test('2xx 流在首帧前断开：只发一次请求，不回退同步端点', async () => {
  const calls: string[] = []
  const response = new Response(null, { headers: { 'content-type': 'text/event-stream' } })
  await assert.rejects(() => runAskTransport(
    async (url) => { calls.push(url); return response },
    async () => { throw new Error('首帧前断流') },
    async () => { assert.fail('已拿到 2xx 后不得同步重发') },
    new AbortController().signal,
  ), /首帧前断流/)
  assert.deepEqual(calls, ['/api/ask/stream'])
})

test('收到 HTTP 响应前网络失败：允许回退一次同步端点', async () => {
  const calls: string[] = []
  await runAskTransport(
    async (url) => {
      calls.push(url)
      if (url.endsWith('/stream')) throw new TypeError('network failed')
      return new Response('{}', { headers: { 'content-type': 'application/json' } })
    },
    async () => assert.fail('网络失败不应消费流'),
    async (response) => assert.equal(response.status, 200),
    new AbortController().signal,
  )
  assert.deepEqual(calls, ['/api/ask/stream', '/api/ask'])
})

test('abort 不回退，且传播给 active 请求', async () => {
  const controller = new AbortController()
  const calls: string[] = []
  const pending = runAskTransport(
    async (url) => {
      calls.push(url)
      await new Promise<void>((_, reject) => controller.signal.addEventListener('abort', () => reject(controller.signal.reason), { once: true }))
      return new Response()
    },
    async () => {},
    async () => assert.fail('abort 不得回退'),
    controller.signal,
  )
  controller.abort(new DOMException('stopped', 'AbortError'))
  await assert.rejects(() => pending, /stopped/)
  assert.deepEqual(calls, ['/api/ask/stream'])
})

test('同一登录态的重复 401 只处理一次', async () => {
  let handled = 0
  const expire = createSessionExpiryGuard(async () => { handled += 1 })
  await Promise.all([expire('token-a'), expire('token-a'), expire('token-a')])
  await expire('token-a')
  assert.equal(handled, 1)
  await expire('token-b')
  assert.equal(handled, 2)
})
