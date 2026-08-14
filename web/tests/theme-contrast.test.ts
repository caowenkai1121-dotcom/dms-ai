// theme.css 的**对比度判据**：把「我算过它达标」变成会红的测试。
//
// 由来（2026-08-13 视觉审计，比值为现算）：
//   --text-faint #8d95ad 对 --bg-card 只有 2.99:1 —— 连非文本的 3:1 都不过，
//   而它被 89 处小字复用（KPI 基期/变化额、表格行号、子任务验收断言……
//   恰好是「判断这个数字可不可信」要读的那批）。
//   暗色 --brand-ink 对 --bg-card 是 1.01:1~1.49:1 —— 侧栏 logo 在暗色下**是隐形的**。
//   暗色主色 #7b89f0 上的 #fff 只有 3.14:1 —— 23 处主按钮与用户气泡全部不过 AA。
//
// 判据现算 WCAG 相对亮度，不写死结论：改 token 时它自己会说过没过。
// 跑法：cd web && node --test tests/theme-contrast.test.ts
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

const css = readFileSync(new URL('../src/theme.css', import.meta.url), 'utf8')

/** `:root { … }` 与 `:root[data-theme="dark"] { … }` 两块里的 `--名: 值;` */
function tokens(selector: string): Record<string, string> {
  const at = css.indexOf(selector)
  assert.ok(at >= 0, `theme.css 里找不到 ${selector}`)
  const block = css.slice(css.indexOf('{', at) + 1, css.indexOf('\n}', at))
  const out: Record<string, string> = {}
  for (const m of block.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)) out[m[1]] = m[2].trim()
  return out
}

function srgb(c: number): number {
  const v = c / 255
  return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4
}

/** 相对亮度。只认 `#rgb` / `#rrggbb`（token 里的实色都是这两种形态）。 */
function luminance(hex: string): number {
  const h = hex.trim().replace('#', '')
  const full = h.length === 3 ? h.split('').map((c) => c + c).join('') : h
  assert.equal(full.length, 6, `不是实色：${hex}`)
  const [r, g, b] = [0, 2, 4].map((i) => parseInt(full.slice(i, i + 2), 16))
  return 0.2126 * srgb(r) + 0.7152 * srgb(g) + 0.0722 * srgb(b)
}

function ratio(a: string, b: string): number {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p)
  return (x + 0.05) / (y + 0.05)
}

/** 渐变里的色标（`linear-gradient(120deg,#aeb8ff 0%,#e8ebf6 100%)` → 两个 hex） */
function stops(value: string): string[] {
  return [...value.matchAll(/#[0-9a-fA-F]{3,6}/g)].map((m) => m[0])
}

for (const [name, selector] of [['亮色', ':root {'], ['暗色', ':root[data-theme="dark"]']] as const) {
  test(`${name}：正文三级层次全部过 AA 4.5:1，且层次不许倒挂`, () => {
    const t = { ...tokens(':root {'), ...(selector === ':root {' ? {} : tokens(selector)) }
    for (const bg of ['--bg-card', '--bg-main', '--bg-body', '--bg-sunken']) {
      for (const fg of ['--text-primary', '--text-regular', '--text-muted', '--text-faint']) {
        const r = ratio(t[fg], t[bg])
        assert.ok(r >= 4.5, `${name} ${fg} 对 ${bg} 只有 ${r.toFixed(2)}:1（要 ≥4.5）`)
      }
    }
    // 三级层次：越次要越淡，但不许淡到比上一级还亮（否则视觉层次是反的）
    const card = '--bg-card'
    assert.ok(
      ratio(t['--text-regular'], t[card]) > ratio(t['--text-muted'], t[card]),
      `${name} regular 不比 muted 更突出 —— 层次倒挂`
    )
    assert.ok(
      ratio(t['--text-muted'], t[card]) > ratio(t['--text-faint'], t[card]),
      `${name} muted 不比 faint 更突出 —— 层次倒挂`
    )
  })

  test(`${name}：主色底上的前景色过 AA`, () => {
    const t = { ...tokens(':root {'), ...(selector === ':root {' ? {} : tokens(selector)) }
    const r = ratio(t['--on-primary'], t['--primary'])
    assert.ok(r >= 4.5, `${name} --on-primary 对 --primary 只有 ${r.toFixed(2)}:1`)
  })

  test(`${name}：品牌渐变两端都看得见`, () => {
    const t = { ...tokens(':root {'), ...(selector === ':root {' ? {} : tokens(selector)) }
    for (const stop of stops(t['--brand-ink'])) {
      const r = ratio(stop, t['--bg-card'])
      assert.ok(r >= 4.5, `${name} 品牌色标 ${stop} 对 --bg-card 只有 ${r.toFixed(2)}:1（logo 会看不见）`)
    }
  })
}

test('原生控件跟随主题：两套主题各自声明 color-scheme', () => {
  assert.match(css, /:root\s*\{[^}]*color-scheme:\s*light/s, '亮色缺 color-scheme')
  assert.match(css, /:root\[data-theme="dark"\][^}]*color-scheme:\s*dark/s, '暗色缺 color-scheme')
})

// ── BiChart 单色阶：色块是「名字 ↔ 扇区」之间唯一的映射，每一阶都要过非文本 3:1 ──
test('BiChart 单色阶每一阶都过 3:1（浅端看不见 = 图例失效）', () => {
  const src = readFileSync(new URL('../src/BiChart.vue', import.meta.url), 'utf8')
  const ramp = (name: string): string[] => {
    const at = src.indexOf(`const ${name} = [`)
    assert.ok(at >= 0, `${name} 没了`)
    const line = src.slice(at, src.indexOf(']', at))
    return [...line.matchAll(/#[0-9a-f]{6}/gi)].map(x => x[0])
  }
  const light = ramp('LIGHT_MONO')
  const dark = ramp('DARK_MONO')
  assert.equal(light.length, 6)
  assert.equal(dark.length, 6)
  for (const c of light) {
    assert.ok(ratio(c, '#ffffff') >= 3, `亮色阶 ${c} 对白卡只有 ${ratio(c, '#ffffff').toFixed(2)}:1`)
  }
  for (const c of dark) {
    assert.ok(ratio(c, '#1a1e2b') >= 3, `暗色阶 ${c} 对暗卡只有 ${ratio(c, '#1a1e2b').toFixed(2)}:1`)
  }
})

test('主色/错误色上的前景走 token，不许再手写 #fff', () => {
  const files = ['App.vue', 'DataMapPanel.vue', 'KbAnswer.vue', 'KbPanel.vue', 'ResultPanel.vue', 'SkillsPanel.vue']
  for (const f of files) {
    const src = readFileSync(new URL(`../src/${f}`, import.meta.url), 'utf8')
    assert.doesNotMatch(src, /color:\s*#fff\b/, `${f} 还有手写白字：暗色主色上只有 3.14:1`)
  }
})
