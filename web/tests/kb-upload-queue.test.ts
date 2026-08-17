// 上传队列的形态钉：并发宽度、排队而非失败、429 自动重试。
// 跑法：cd web && node --test tests/kb-upload-queue.test.ts
//
// 为什么是源码扫描而不是行为测试：send() 长在 .vue 单文件组件里，拿不出来单独跑；
// 而这三条要防的都是「被改回旧写法」——旧写法的特征在源码里是确定的。
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

const src = readFileSync(new URL('../src/KbPanel.vue', import.meta.url), 'utf8')

test('上传是并发的：宽度 > 1，且用取号器而不是 Promise.all(全部)', () => {
  const width = src.match(/const UPLOAD_PARALLEL = (\d+)/)
  assert.ok(width, '缺 UPLOAD_PARALLEL')
  assert.ok(Number(width[1]) > 1, `并发宽度必须 > 1，当前 ${width[1]}——串行上传时一批文件要排很久`)

  // 取号器：固定 width 个 worker 共享一个游标。不许换成 files.map(上传) 一次全发出去——
  // 那是「无界并发」，会把服务端的入库闸瞬间打满，也没法给用户显示排队位置。
  assert.match(src, /Array\.from\(\{ length: width \}/, '并发必须是固定宽度的取号器')
  assert.match(src, /while \(cursor < jobs\.length\)/, '取号器必须共享游标')
})

test('规划与执行分两段：同名判定不受并发调度影响', () => {
  const plan = src.indexOf('const jobs: Array<{ rowId: number; file: File; folder: string }> = []')
  const exec = src.indexOf('let cursor = 0')
  assert.ok(plan > 0 && exec > plan, '必须先同步规划（建行 + 同名提示）再并发执行')

  // seenDocNames 的登记必须在规划段里；挪进并发段之后「谁先谁后」就随网络时序漂了。
  const dup = src.indexOf('seenDocNames.add(dupKey)')
  assert.ok(dup > plan && dup < exec, '同名登记必须留在同步规划段内')
})

test('429 是排队信号，不是失败：自动退避重试，不落 fail 行', () => {
  assert.match(src, /const UPLOAD_RETRY_MAX = \d+/, '缺 429 重试次数上限')
  assert.match(src, /status === 429 && attempt < UPLOAD_RETRY_MAX/, '429 必须走重试分支')
  assert.match(src, /UPLOAD_RETRY_BASE_MS \* 2 \*\* attempt/, '重试必须指数退避，别原地打服务端')

  // 重试分支必须在「非 2xx 落 fail」之前——顺序反了 429 就先被当成失败收口了。
  const retry = src.indexOf('status === 429 && attempt < UPLOAD_RETRY_MAX')
  const fail = src.indexOf('if (status < 200 || status >= 300)')
  assert.ok(retry > 0 && fail > retry, '429 重试分支必须排在通用失败分支之前')
})

test('队列里的行显示「排队中」，不是空白也不是失败', () => {
  assert.match(src, /msg: jobs\.length < UPLOAD_PARALLEL \? '等待上传' : '排队中'/,
    '超出并发宽度的文件应显示排队中——用户要能分辨「在等」和「挂了」')
})
