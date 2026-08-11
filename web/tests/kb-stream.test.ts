// kb-stream.ts 的单测：node --test 直接跑（与 format.test.ts 同一条跑法，无需装依赖）。
// 跑法：cd web && node --test tests/kb-stream.test.ts
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { SseParser, parseEventData } from '../src/kb-stream.ts'

test('SseParser：一帧一字节块的基本形态', () => {
  const p = new SseParser()
  const out = p.feed('event: meta\ndata: {"trace_id":"t-1"}\n\n')
  assert.equal(out.length, 1)
  assert.equal(out[0].event, 'meta')
  assert.equal(out[0].data, '{"trace_id":"t-1"}')
})

test('SseParser：帧跨 chunk 切开、一块多帧、CRLF', () => {
  const p = new SseParser()
  assert.deepEqual(p.feed('event: del'), [])
  assert.deepEqual(p.feed('ta\nda'), [])
  const out = p.feed('ta: {"text":"报"}\r\n\r\nevent: done\ndata: {"answer":{}}\n\n')
  assert.equal(out.length, 2)
  assert.equal(out[0].event, 'delta')
  assert.equal(JSON.parse(out[0].data).text, '报')
  assert.equal(out[1].event, 'done')
})

test('SseParser：注释行（keep-alive）不产事件；缺省事件名是 message', () => {
  const p = new SseParser()
  assert.deepEqual(p.feed(': ping\n\n'), [])
  const out = p.feed('data: x\n\n')
  assert.equal(out.length, 1)
  assert.equal(out[0].event, 'message')
  // 注释行夹在帧中间不拆帧
  const p2 = new SseParser()
  const out2 = p2.feed('data: a\n: ping\ndata: b\n\n')
  assert.equal(out2.length, 1)
  assert.equal(out2[0].data, 'a\nb', '多行 data 按规范以 \\n 拼接')
})

test('SseParser：end() 吐出流尾残余（无换行收尾的最后一帧）', () => {
  const p = new SseParser()
  assert.deepEqual(p.feed('event: error\ndata: {"message":"中断"}'), [])
  const out = p.end()
  assert.equal(out.length, 1)
  assert.equal(out[0].event, 'error')
  assert.deepEqual(p.end(), [], 'end 只吐一次')
})

test('parseEventData：坏 JSON / 非对象返回 null（跳过该帧，不炸流）', () => {
  assert.deepEqual(parseEventData({ event: 'meta', data: '{"trace_id":"t"}' }), { trace_id: 't' })
  assert.equal(parseEventData({ event: 'meta', data: '{oops' }), null)
  assert.equal(parseEventData({ event: 'meta', data: '[1,2]' }), null)
  assert.equal(parseEventData({ event: 'meta', data: '"str"' }), null)
})
