<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'

interface Citation {
  doc_id: string; doc_name: string; chunk_id: number
  page?: number | null; heading_path?: string
  folder_path?: string
  span?: number | null
  effective_from?: string | null; effective_to?: string | null
  document_family?: string | null; document_revision?: string | null
  source_hash?: string; doc_updated_at?: string
}
interface TextResult { markdown?: string; citations?: Citation[] }

const props = defineProps<{ result: TextResult; token?: string; login?: string; traceId?: string }>()
const emit = defineEmits<{ (e: 'auth-expired'): void }>()
const citations = computed<Citation[]>(() => props.result.citations ?? [])
const sourceDocs = computed(() => new Set(citations.value.map((c) => c.doc_id)).size)
function governedVersionsConflict(left: Citation, right: Citation): boolean {
  return [
    [left.document_revision, right.document_revision],
    [left.effective_from, right.effective_from],
    [left.effective_to, right.effective_to],
  ].some(([a, b]) => {
    const leftValue = a?.trim()
    const rightValue = b?.trim()
    return !!leftValue && !!rightValue && leftValue !== rightValue
  })
}
const conflictingFamilies = computed(() => {
  const families = new Map<string, Citation[]>()
  for (const citation of citations.value) {
    const family = citation.document_family?.trim()
    if (!family) continue
    const members = families.get(family) ?? []
    members.push(citation)
    families.set(family, members)
  }
  return [...families.entries()]
    .filter(([, members]) => members.some((member, i) => members.slice(i + 1).some((other) => governedVersionsConflict(member, other))))
    .map(([family]) => family)
})
const hasVersionRisk = computed(() => {
  if (conflictingFamilies.value.length) return true
  const markdown = props.result.markdown ?? ''
  return /(版本与差异|多版本|版本提示|口径.{0,8}(不同|差异|冲突))/i.test(markdown)
})
const versionRiskText = computed(() => conflictingFamilies.value.length
  ? `本回答同时参考了“${conflictingFamilies.value.join('、')}”文档族的多个版本。系统不会自动选用其中一份，请并列核对差异并由制度负责人确认。`
  : '回答中包含版本或口径差异。系统不会自动选用其中一份，请并列核对并人工确认。')
const opened = ref<Record<number, string>>({})
const loading = ref<Record<number, boolean>>({})
const errors = ref<Record<number, string>>({})
const stale = ref<Record<number, boolean>>({})
const downloadError = ref('')
const evidenceEls = ref<Record<number, HTMLElement | null>>({})
const highlighted = ref(0)
const sourcesOpen = ref(false)
const sourcesEl = ref<HTMLElement | null>(null)
let answerGeneration = 0

const answerKey = computed(() => JSON.stringify({
  markdown: props.result.markdown ?? '',
  citations: citations.value.map((c) => [
    c.doc_id, c.chunk_id, c.span ?? 1, c.source_hash ?? '', c.doc_updated_at ?? '',
    c.doc_name, c.folder_path ?? '', c.document_revision ?? '',
  ]),
}))
function activeAnswerKey(): string {
  return `${props.token ?? ''}\u0000${props.login ?? ''}\u0000${answerKey.value}`
}

// 【Y2】👍/👎 轻量反馈：绑当次回答的 trace_id（服务端落账后由 Answer 带上 wire）。
// 👍 映 'correct'（服务端自旋 resolved）、👎 映 'data' —— 反馈五类闭集里最贴近
// 「内容不对/没用」的一档，不为轻量反馈扩 CHECK 约束（零迁移）。服务端按
// (trace_id, login) upsert，改主意（👍→👎）合法。已反馈形态按 trace_id 记
// localStorage：历史会话重开、组件重挂载都还在。
type FeedbackKind = 'correct' | 'data'
const feedback = ref<'' | FeedbackKind>('')
const feedbackBusy = ref(false)
const feedbackError = ref('')
const feedbackKey = computed(() => (props.traceId ? `kb-fb:${props.traceId}` : ''))
function loadFeedback() {
  const saved = feedbackKey.value ? localStorage.getItem(feedbackKey.value) : null
  feedback.value = saved === 'correct' || saved === 'data' ? saved : ''
  feedbackError.value = ''
}

async function sendFeedback(kind: FeedbackKind) {
  if (!props.traceId || feedbackBusy.value || feedback.value === kind) return
  feedbackBusy.value = true
  feedbackError.value = ''
  try {
    // token 与 login 二选一都给：Bearer 优先（服务端唯一可信来源），login_name
    // 只在内网回退开关开着时才生效 —— 与 resolve_identity 的口径一致
    const headers: Record<string, string> = { 'Content-Type': 'application/json' }
    const token = props.token?.trim()
    if (token) headers.Authorization = `Bearer ${token}`
    const response = await fetch('/api/feedback', {
      method: 'POST',
      headers,
      body: JSON.stringify({ trace_id: props.traceId, kind, detail: '', login_name: props.login ?? null }),
    })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw new Error('feedback_failed')
    feedback.value = kind
    if (feedbackKey.value) localStorage.setItem(feedbackKey.value, kind)
  } catch {
    feedbackError.value = '反馈提交失败，请稍后重试。'
  } finally {
    feedbackBusy.value = false
  }
}

watch([answerKey, () => props.token, () => props.login, () => props.traceId], () => {
  answerGeneration += 1
  opened.value = {}
  loading.value = {}
  errors.value = {}
  stale.value = {}
  evidenceEls.value = {}
  downloadError.value = ''
  highlighted.value = 0
  sourcesOpen.value = false
  loadFeedback()
}, { flush: 'sync' })
loadFeedback()

function sessionHeaders(): Record<string, string> {
  const token = props.token?.trim()
  if (!token) {
    emit('auth-expired')
    throw new Error('登录会话已失效，请重新登录。')
  }
  return { Authorization: `Bearer ${token}` }
}

function esc(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}
function inline(s: string): string {
  return s
    .replace(/\[\^(\d+)\]/g, (_m, n: string) => {
      const index = Number(n)
      return index >= 1 && index <= citations.value.length
        ? `<button class="cite" type="button" data-n="${index}" title="查看来源原文" aria-label="查看来源原文">来源</button>`
        : ''
    })
    .replace(/\[(?:KPI|SEC|CON)-[^\]\r\n]+\]/gi, '')
    .replace(/\b(?:KPI|SEC|CON)-[A-Z0-9_-]+/gi, '')
    .replace(/\*\*([^*]+)\*\*/g, '<b>$1</b>')
    .replace(/`([^`]+)`/g, '<code>$1</code>')
}
function cleanMarkdown(md: string): string {
  const output: string[] = []
  let hiddenLevel = 0
  for (const line of md.split(/\r?\n/)) {
    const trimmed = line.trim()
    const heading = /^(#{1,6})\s+(.+)$/.exec(trimmed)
    if (heading) {
      const level = heading[1].length
      const title = heading[2].trim()
      if (/^(证据|证据详情|证据列表|证据链|来源依据|引用依据|内部依据)$/i.test(title) || /(内部|技术).{0,6}(证据|检索|评分)|检索(证据|过程|明细|评分)|召回(结果|明细)|相似度|rerank|bm25|向量得分/i.test(title)) {
        hiddenLevel = level
        continue
      }
      if (hiddenLevel && level <= hiddenLevel) hiddenLevel = 0
    }
    if (hiddenLevel) continue
    if (/^\[\^\d+\]:/.test(trimmed)) continue
    if (/^(?:[-*+]\s*)?(?:rerank|bm25|similarity|vector\s*score|检索分数|相似度|向量得分|召回分数)\s*[:=：|]/i.test(trimmed)) continue
    if (/^(?:[-*+]\s*)?(?:证据编号|内部编号|KPI引用|SEC引用|CON引用)\s*[:：]/i.test(trimmed)) continue
    output.push(line)
  }
  return output.join('\n')
    .replace(/\[(?:KPI|SEC|CON)-[^\]\r\n]+\]/gi, '')
    .replace(/\b(?:KPI|SEC|CON)-[A-Z0-9_-]+/gi, '')
    .replace(/(?:^|\s)(?:rerank|bm25|similarity|vector\s*score|检索分数|相似度)\s*[:=：]\s*[-+]?\d+(?:\.\d+)?/gim, '')
    .trim()
}
function headingClass(title: string): string {
  if (/(结论|摘要|核心答案|直接回答)/.test(title)) return 'conclusion'
  if (/(关键要点|核心要点|重点|关键信息|主要内容)/.test(title)) return 'points'
  if (/(建议|步骤|操作|行动|怎么做|处理方式)/.test(title)) return 'action'
  if (/(对比|比较|区别|差别|变化)/.test(title)) return 'compare'
  if (/(注意|风险|限制|边界|异常|差异)/.test(title)) return 'caution'
  return 'default'
}
function cellClass(value: string): string {
  const plain = value.replace(/\[\^\d+\]/g, '').replace(/[*`]/g, '').replace(/<[^>]+>/g, '').trim()
  if (/^[¥￥]?[-+]?\d[\d,.]*(?:\s*(?:%|元|万元|亿元|万|亿|天|次|个|份|项|小时|分钟))?$/.test(plain)) return ' class="num"'
  if (/(异常|风险|逾期|禁止|不允许|失败|冲突|废止|需人工确认)/.test(plain)) return ' class="risk"'
  return ''
}
function render(md: string): string {
  const out: string[] = []
  let listTag: 'ul' | 'ol' | null = null
  let inCode = false, inTable = false, code: string[] = []
  let section = 'default'
  const closeList = () => { if (listTag) { out.push(`</${listTag}>`); listTag = null } }
  const closeTable = () => { if (inTable) { out.push('</tbody></table></div>'); inTable = false } }
  const closeCode = () => { out.push(`<pre class="kb-code">${code.join('\n')}</pre>`); code = []; inCode = false }
  for (const line of esc(md).split(/\r?\n/)) {
    if (line.trimStart().startsWith('```')) {
      if (inCode) closeCode()
      else { closeList(); closeTable(); inCode = true }
      continue
    }
    if (inCode) { code.push(line); continue }
    if (!line.trim()) { closeList(); closeTable(); continue }
    if (/^\s*([-*_])\1{2,}\s*$/.test(line)) {
      closeList(); closeTable(); out.push('<hr>'); continue
    }
    const cells = /^\s*\|(.*)\|\s*$/.exec(line)?.[1].split('|').map((x) => x.trim())
    if (cells) {
      closeList()
      if (cells.every((x) => /^:?-+:?$/.test(x))) continue
      if (!inTable) {
        out.push(`<div class="kb-table-wrap ${section}"><table><thead><tr>`)
        cells.forEach((x) => out.push(`<th${cellClass(x)}>${inline(x)}</th>`))
        out.push('</tr></thead><tbody>')
        inTable = true
      } else {
        out.push('<tr>')
        cells.forEach((x) => out.push(`<td${cellClass(x)}>${inline(x)}</td>`))
        out.push('</tr>')
      }
      continue
    }
    closeTable()
    const heading = /^(#{1,4})\s+(.*)$/.exec(line)
    if (heading) {
      closeList()
      const level = Math.min(6, heading[1].length + 2)
      section = headingClass(heading[2])
      out.push(`<h${level} class="kb-section-title ${section}">${inline(heading[2])}</h${level}>`)
      continue
    }
    const quote = /^\s*&gt;\s?(.*)$/.exec(line)
    if (quote) {
      closeList()
      out.push(`<blockquote>${inline(quote[1])}</blockquote>`)
      continue
    }
    const item = /^\s*([-*+]|\d+[.)])\s+(.*)$/.exec(line)
    if (item) {
      const nextTag: 'ul' | 'ol' = /^\d/.test(item[1]) ? 'ol' : 'ul'
      if (listTag !== nextTag) { closeList(); out.push(`<${nextTag} class="kb-list ${section}">`); listTag = nextTag }
      out.push(`<li>${inline(item[2])}</li>`)
      continue
    }
    closeList()
    const keyLine = /^(结论|答案|建议|注意|提示)[:：]\s*(.+)$/.exec(line)
    out.push(keyLine
      ? `<p class="kb-key-line"><b>${inline(keyLine[1])}</b><span>${inline(keyLine[2])}</span></p>`
      : `<p>${inline(line)}</p>`)
  }
  closeList()
  closeTable()
  if (inCode) closeCode()
  return out.join('')
}

function presentation(md: string): { title: string; summary: string; body: string } {
  const lines = md.trim().split(/\r?\n/)
  let title = '回答摘要'
  let summary = ''
  const consumed = new Set<number>()
  const heading = lines.findIndex((line) => /^#{1,4}\s+\S/.test(line))
  if (heading >= 0) {
    const headingText = lines[heading].replace(/^#{1,4}\s+/, '').trim()
    title = ['直接结论', '关键要点', '操作步骤', '对比说明', '适用范围', '注意事项', '版本与差异'].includes(headingText)
      ? '知识库回答'
      : headingText
    consumed.add(heading)
  }
  const conclusionHeading = lines.findIndex((line) => /^#{1,4}\s+(?:直接结论|核心答案|结论|答案|回答摘要|摘要)\s*$/.test(line.trim()))
  const conclusionLine = lines.findIndex((line) => /^(?:结论|答案|摘要)[:：]\s*\S/.test(line.trim()))
  const paragraph = conclusionLine >= 0 ? conclusionLine : lines.findIndex((line, index) => {
    const value = line.trim()
    return !consumed.has(index)
      && (conclusionHeading >= 0 ? index > conclusionHeading : heading < 0 || index > heading)
      && value
      && !/^(#{1,4}\s+|```|\||[-*+]\s+|\d+[.)]\s+)/.test(value)
  })
  if (paragraph >= 0) {
    summary = lines[paragraph].trim().replace(/^(?:结论|答案|摘要)[:：]\s*/, '')
    consumed.add(paragraph)
    if (conclusionHeading >= 0 && paragraph === conclusionHeading + 1) consumed.add(conclusionHeading)
  }
  return { title, summary, body: lines.filter((_line, index) => !consumed.has(index)).join('\n').trim() }
}

const displayMarkdown = computed(() => cleanMarkdown(props.result.markdown ?? ''))
const presented = computed(() => presentation(displayMarkdown.value))
const summaryHtml = computed(() => inline(esc(presented.value.summary)))
const html = computed(() => render(presented.value.body))

function locationOf(c: Citation): string {
  const folderPath = c.folder_path?.trim()
  return [folderPath && folderPath !== '/' ? `目录 ${folderPath}` : '', c.heading_path, c.page ? `第 ${c.page} 页` : ''].filter(Boolean).join(' · ') || '文档正文'
}
function spanOf(c: Citation): number {
  return c.span && c.span > 1 ? c.span : 1
}
function effectiveOf(c: Citation): string {
  if (c.effective_from && c.effective_to) return `${c.effective_from} 至 ${c.effective_to}`
  if (c.effective_from) return `${c.effective_from} 起生效`
  if (c.effective_to) return `有效至 ${c.effective_to}`
  return ''
}
function versionOf(c: Citation): string {
  return [c.document_family, c.document_revision].filter(Boolean).join(' · ')
}
async function downloadSource(c: Citation) {
  const generation = answerGeneration
  const activeKey = activeAnswerKey()
  downloadError.value = ''
  try {
    const response = await fetch(`/api/kb/doc/${encodeURIComponent(c.doc_id)}/download`, { headers: sessionHeaders() })
    if (response.status === 401) emit('auth-expired')
    if (!response.ok) throw new Error('download_unavailable')
    if (generation !== answerGeneration || activeKey !== activeAnswerKey()) return
    const blob = await response.blob()
    if (generation !== answerGeneration || activeKey !== activeAnswerKey()) return
    const url = URL.createObjectURL(blob)
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.download = c.doc_name || 'knowledge-file'
    document.body.appendChild(anchor)
    anchor.click()
    anchor.remove()
    window.setTimeout(() => URL.revokeObjectURL(url), 0)
  } catch {
    if (generation === answerGeneration && activeKey === activeAnswerKey()) {
      downloadError.value = '原件暂时无法下载，请稍后重试。'
    }
  }
}

function setEvidenceEl(n: number, el: unknown) {
  evidenceEls.value[n] = el instanceof HTMLElement ? el : null
}

async function focusEvidence(n: number) {
  sourcesOpen.value = true
  await nextTick()
  sourcesEl.value?.scrollIntoView({ behavior: 'smooth', block: 'nearest' })
  await nextTick()
  evidenceEls.value[n]?.scrollIntoView({ behavior: 'smooth', block: 'center' })
  highlighted.value = n
  window.setTimeout(() => { if (highlighted.value === n) highlighted.value = 0 }, 1400)
}

async function show(n: number, focusOnly = false) {
  const c = citations.value[n - 1]
  if (!c || loading.value[n]) return
  if (opened.value[n]) {
    if (focusOnly) {
      await focusEvidence(n)
      return
    }
    delete opened.value[n]
    delete errors.value[n]
    delete stale.value[n]
    return
  }
  loading.value[n] = true
  const generation = answerGeneration
  const activeKey = activeAnswerKey()
  delete errors.value[n]
  delete stale.value[n]
  const query = new URLSearchParams()
  if (spanOf(c) > 1) query.set('span', String(spanOf(c)))
  else query.set('window', '1')
  if (c.source_hash) query.set('source_hash', c.source_hash)
  if (c.doc_updated_at) query.set('doc_updated_at', c.doc_updated_at)
  try {
    const response = await fetch(`/api/kb/chunk/${c.chunk_id}?${query}`, {
      headers: sessionHeaders(),
    })
    if (response.status === 401) emit('auth-expired')
    const data = await response.json().catch(() => ({}))
    if (generation !== answerGeneration || activeKey !== activeAnswerKey()) return
    if (response.status === 409) {
      stale.value[n] = true
      errors.value[n] = '来源文档已更新，本条引用已失效，请重新提问以获取最新引用。'
    } else {
      if (!response.ok) throw new Error('source_unavailable')
      const text = data.text ?? (Array.isArray(data.chunks)
        ? data.chunks.map((chunk: { text?: string }) => chunk.text ?? '').join('\n\n')
        : '')
      opened.value[n] = text || '该引用没有可显示的原文内容。'
    }
  } catch {
    if (generation === answerGeneration && activeKey === activeAnswerKey()) {
      errors.value[n] = '原文暂时无法加载，请稍后重试。'
    }
  } finally {
    if (generation === answerGeneration && activeKey === activeAnswerKey()) loading.value[n] = false
  }
  if (generation === answerGeneration && activeKey === activeAnswerKey()) await focusEvidence(n)
}

function onBodyClick(e: MouseEvent) {
  const target = (e.target as HTMLElement | null)?.closest<HTMLElement>('[data-n]')
  const n = target?.dataset.n
  if (n) void show(Number(n), true)
}
function onSourcesToggle(e: Event) {
  sourcesOpen.value = (e.target as HTMLDetailsElement).open
}
</script>

<template>
  <section class="kb-answer">
    <template v-if="result.markdown">
      <header class="answer-lead">
        <div class="answer-topline">
          <span class="answer-kicker">企业知识库</span>
          <span v-if="citations.length" class="answer-meta">综合 {{ sourceDocs }} 份资料</span>
        </div>
        <h3>{{ presented.title }}</h3>
        <div v-if="presented.summary" class="answer-summary">
          <span>直接结论</span>
          <p @click="onBodyClick" v-html="summaryHtml"></p>
        </div>
      </header>
      <div v-if="presented.body" class="answer-body" @click="onBodyClick" v-html="html"></div>
      <!-- 【Y2】👍/👎 轻量反馈：绑当次 trace_id；老会话缓存行没有它时不显示（反馈无处可绑） -->
      <div v-if="traceId" class="answer-feedback">
        <span class="fb-label">这个回答有帮助吗？</span>
        <button type="button" :class="{ active: feedback === 'correct' }" :disabled="feedbackBusy" @click="sendFeedback('correct')">👍 有帮助</button>
        <button type="button" :class="{ active: feedback === 'data' }" :disabled="feedbackBusy" @click="sendFeedback('data')">👎 没用</button>
        <span v-if="feedback" class="fb-done">已反馈，感谢</span>
        <span v-if="feedbackError" class="fb-error" role="alert">{{ feedbackError }}</span>
      </div>
    </template>
    <div v-else class="answer-empty">
      <strong>暂无回答</strong>
      <span>本次未生成可展示内容，请换一种问法重试。</span>
    </div>

    <div v-if="hasVersionRisk" class="evidence-alert version" role="status">
      <span class="alert-mark" aria-hidden="true">!</span>
      <div>
        <strong>检测到版本或口径差异</strong>
        <span>{{ versionRiskText }}</span>
      </div>
    </div>
    <details v-if="citations.length" ref="sourcesEl" class="evidence" :open="sourcesOpen" @toggle="onSourcesToggle">
      <summary class="evidence-head">
        <span>来源文档</span>
        <small>{{ sourceDocs }} 份文档</small>
        <b>{{ sourcesOpen ? '收起' : '展开' }}</b>
      </summary>

      <div class="evidence-list" aria-label="回答来源">
        <article
          v-for="(c, i) in citations" :key="`${c.doc_id}-${c.chunk_id}-${i}`"
          :ref="(el) => setEvidenceEl(i + 1, el)"
          class="evidence-row" :class="{ highlighted: highlighted === i + 1 }"
        >
          <button class="evidence-toggle" type="button" :aria-expanded="!!opened[i + 1]" @click="show(i + 1)">
            <span class="source-mark" aria-hidden="true"></span>
            <span class="source-main">
              <strong :title="c.doc_name">{{ c.doc_name }}</strong>
              <span>{{ locationOf(c) }}</span>
            </span>
            <span class="source-action">{{ loading[i + 1] ? '加载中' : opened[i + 1] ? '收起' : '查看原文' }}</span>
          </button>
          <div class="source-governance">
            <span v-if="effectiveOf(c)">{{ effectiveOf(c) }}</span>
            <span v-if="versionOf(c)">{{ versionOf(c) }}</span>
            <button type="button" @click="downloadSource(c)">下载原件</button>
          </div>
          <div v-if="errors[i + 1]" class="source-error" role="alert">
            <span>{{ errors[i + 1] }}</span>
            <button v-if="!stale[i + 1]" type="button" @click="show(i + 1)">重试</button>
          </div>
          <div v-if="opened[i + 1]" class="source-preview">
            <div class="preview-label">引用原文</div>
            <pre>{{ opened[i + 1] }}</pre>
          </div>
        </article>
      </div>
      <div v-if="downloadError" class="source-error download-error" role="alert">{{ downloadError }}</div>
    </details>
  </section>
</template>

<style scoped>
.kb-answer { color: var(--text-regular); font-size: 14px; line-height: 1.75; }
.answer-lead { min-width: 0; padding: 18px 20px 20px; border: 1px solid var(--border); border-top: 3px solid var(--primary); border-radius: 6px; background: var(--bg-card); box-shadow: var(--shadow-sm); }
.answer-topline { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.answer-kicker { color: var(--primary); font-size: 11px; font-weight: 750; }
.answer-meta { color: var(--text-faint); font-size: 10.5px; white-space: nowrap; }
.answer-lead h3 { margin: 4px 0 10px; overflow-wrap: anywhere; color: var(--text-primary); font-size: 18px; line-height: 1.4; }
.answer-summary { display: grid; grid-template-columns: 58px minmax(0, 1fr); align-items: start; gap: 11px; padding: 12px 14px; border: 1px solid rgba(var(--primary-rgb), .18); border-left: 3px solid var(--primary); background: var(--primary-bg); }
.answer-summary > span { padding-top: 1px; color: var(--primary); font-size: 11px; font-weight: 750; }
.answer-summary p { min-width: 0; margin: 0; overflow-wrap: anywhere; color: var(--text-primary); font-size: 14px; line-height: 1.75; }
.answer-body { margin-top: 14px; padding: 1px 2px; }
.answer-body :deep(p) { margin: 0 0 11px; overflow-wrap: anywhere; }
.answer-body :deep(h3), .answer-body :deep(h4), .answer-body :deep(h5), .answer-body :deep(h6) {
  margin: 18px 0 9px; padding: 0 0 7px 10px; border-bottom: 1px solid var(--divider); border-left: 3px solid var(--border); color: var(--text-primary); font-size: 14px; font-weight: 750; line-height: 1.45;
}
.answer-body :deep(.kb-section-title.conclusion) { border-left-color: var(--primary); }
.answer-body :deep(.kb-section-title.points) { border-left-color: #5174c8; }
.answer-body :deep(.kb-section-title.action) { border-left-color: #3c9460; }
.answer-body :deep(.kb-section-title.compare) { border-left-color: #7b67b8; }
.answer-body :deep(.kb-section-title.caution) { border-left-color: #d38b19; }
.answer-body :deep(ul), .answer-body :deep(ol) { margin: 0 0 12px; padding: 0; list-style: none; counter-reset: kb-step; }
.answer-body :deep(li) { position: relative; min-width: 0; margin: 7px 0; padding: 8px 10px 8px 25px; overflow-wrap: anywhere; border: 1px solid var(--divider); background: var(--bg-card); }
.answer-body :deep(li)::before { content: ''; position: absolute; left: 11px; top: 1.25em; width: 5px; height: 5px; border-radius: 50%; background: var(--primary); }
.answer-body :deep(ol li) { min-height: 34px; padding-left: 43px; counter-increment: kb-step; }
.answer-body :deep(ol li)::before {
  content: counter(kb-step); left: 10px; top: 7px; width: 23px; height: 23px; display: grid; place-items: center;
  border: 1px solid rgba(var(--primary-rgb), .25); border-radius: 50%; background: var(--primary-bg);
  color: var(--primary); font-size: 10px; font-weight: 750;
}
.answer-body :deep(.kb-list.points) { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px; }
.answer-body :deep(.kb-list.points li) { margin: 0; border-left: 3px solid #5174c8; }
.answer-body :deep(.kb-list.action li) { border-left: 3px solid #3c9460; }
.answer-body :deep(.kb-list.caution li) { border-left: 3px solid #d38b19; }
.answer-body :deep(b) { color: var(--text-primary); }
.answer-body :deep(blockquote) {
  margin: 10px 0 14px; padding: 10px 12px; border-left: 3px solid var(--primary);
  background: var(--primary-bg); color: var(--text-primary); overflow-wrap: anywhere;
}
.answer-body :deep(.kb-key-line) { display: grid; grid-template-columns: 42px minmax(0, 1fr); gap: 9px; padding: 9px 11px; background: var(--bg-main); }
.answer-body :deep(.kb-key-line b) { color: var(--primary); }
.answer-body :deep(hr) { margin: 16px 0; border: 0; border-top: 1px solid var(--divider); }
.answer-body :deep(code) {
  padding: 1px 4px; border: 1px solid var(--border); border-radius: 4px;
  background: var(--bg-main); color: var(--text-primary); font-family: var(--font-mono); font-size: 12px;
}
.answer-body :deep(.kb-code) {
  margin: 10px 0; padding: 11px 12px; overflow-x: auto; white-space: pre-wrap;
  border: 1px solid var(--border); border-radius: 6px; background: var(--bg-main);
  color: var(--text-regular); font-family: var(--font-mono); font-size: 12px; line-height: 1.65;
}
.answer-body :deep(.kb-table-wrap) { max-width: 100%; margin: 8px 0 14px; overflow-x: auto; border: 1px solid var(--border); border-radius: 6px; -webkit-overflow-scrolling: touch; }
.answer-body :deep(.kb-table-wrap.compare) { border-top: 3px solid #7b67b8; }
.answer-body :deep(table) { width: 100%; min-width: 480px; border-collapse: collapse; font-size: 12.5px; font-variant-numeric: tabular-nums; }
.answer-body :deep(th), .answer-body :deep(td) { padding: 8px 10px; border-bottom: 1px solid var(--divider); text-align: left; vertical-align: top; }
.answer-body :deep(th) { background: var(--bg-main); color: var(--text-primary); font-weight: 650; }
.answer-body :deep(td) { overflow-wrap: anywhere; }
.answer-body :deep(th.num), .answer-body :deep(td.num) { text-align: right; white-space: nowrap; }
.answer-body :deep(td.risk) { color: var(--error-text); font-weight: 650; }
.answer-body :deep(tbody tr:nth-child(even)) { background: var(--bg-main); }
.answer-body :deep(tbody tr:hover) { background: var(--bg-hover); }
.answer-body :deep(tr:last-child td) { border-bottom: 0; }
.answer-summary :deep(button.cite), .answer-body :deep(button.cite) {
  height: 18px; margin: 0 3px; padding: 0 6px; border: 0; border-radius: 999px;
  background: var(--primary-bg); color: var(--primary); cursor: pointer;
  font: inherit; font-size: 9.5px; font-weight: 700; line-height: 18px; vertical-align: 1px;
}
.answer-summary :deep(button.cite:hover), .answer-body :deep(button.cite:hover) { background: var(--primary); color: #fff; }
.evidence-alert {
  margin-top: 14px; padding: 10px 12px; display: flex; align-items: flex-start; gap: 9px;
  border: 1px solid var(--border); border-left-width: 3px; background: var(--bg-main);
}
.evidence-alert .alert-mark {
  width: 20px; height: 20px; flex: 0 0 20px; display: grid; place-items: center;
  border-radius: 50%; color: #fff; font-size: 11px; font-weight: 800;
}
.evidence-alert div { min-width: 0; display: flex; flex-direction: column; gap: 1px; }
.evidence-alert strong { color: var(--text-primary); font-size: 12px; line-height: 1.5; }
.evidence-alert span:last-child { color: var(--text-muted); font-size: 11px; line-height: 1.55; }
.evidence-alert.version { border-left-color: #d38b19; background: rgba(211, 139, 25, .06); }
.evidence-alert.version .alert-mark { background: #d38b19; }
.evidence { max-width: 100%; margin-top: 16px; border: 1px solid var(--border); border-radius: 6px; overflow: hidden; background: var(--bg-card); }
.evidence-head { min-height: 42px; display: flex; align-items: center; gap: 8px; padding: 0 11px; cursor: pointer; list-style: none; }
.evidence-head::-webkit-details-marker { display: none; }
.evidence-head > span { color: var(--text-primary); font-size: 12.5px; font-weight: 700; }
.evidence-head small { color: var(--text-muted); font-size: 10.5px; }
.evidence-head b { margin-left: auto; color: var(--primary); font-size: 10.5px; font-weight: 600; }
.evidence-list { border-top: 1px solid var(--border); }
.evidence-row { border-top: 1px solid var(--divider); }
.evidence-row.highlighted { box-shadow: inset 3px 0 var(--primary); background: var(--primary-bg); }
.evidence-row:first-child { border-top: 0; }
.evidence-toggle {
  width: 100%; min-height: 52px; display: grid; grid-template-columns: 12px minmax(160px, 1fr) auto;
  align-items: center; gap: 9px; padding: 8px 10px; border: 0; background: var(--bg-card);
  color: var(--text-regular); text-align: left; cursor: pointer;
}
.evidence-toggle:hover { background: var(--bg-hover); }
.source-mark { width: 6px; height: 6px; border-radius: 50%; background: var(--primary); }
.source-main { min-width: 0; display: flex; flex-direction: column; gap: 2px; }
.source-main strong { overflow: hidden; color: var(--text-primary); font-size: 12px; text-overflow: ellipsis; white-space: nowrap; }
.source-main span { overflow: hidden; color: var(--text-muted); font-size: 10.5px; text-overflow: ellipsis; white-space: nowrap; }
.source-action { min-width: 50px; color: var(--primary); font-size: 11px; text-align: right; white-space: nowrap; }
.source-governance {
  display: flex; align-items: center; flex-wrap: wrap; gap: 5px; padding: 0 10px 8px 31px;
  background: var(--bg-card);
}
.source-governance span, .source-governance a, .source-governance button {
  padding: 1px 6px; border: 1px solid var(--border); border-radius: 999px;
  color: var(--text-muted); background: var(--bg-main); font-size: 10px; font-weight: 550; line-height: 1.5;
  text-decoration: none; font-family: inherit;
  overflow-wrap: anywhere;
}
.source-governance button { color: var(--primary); cursor: pointer; }
.source-governance button:hover { border-color: var(--primary); }
.download-error { margin-top: 8px; padding-left: 10px; }
.source-preview { padding: 0 10px 10px 31px; background: var(--bg-main); }
.preview-label { padding: 9px 0 5px; color: var(--text-muted); font-size: 10.5px; font-weight: 650; }
.source-preview pre {
  max-height: 300px; margin: 0; padding: 10px 12px; overflow: auto; white-space: pre-wrap;
  border-left: 3px solid var(--primary); background: var(--bg-card); color: var(--text-regular);
  font-family: var(--font-sans); font-size: 12px; line-height: 1.7;
}
.source-error { display: flex; align-items: center; gap: 8px; padding: 8px 10px 8px 31px; background: var(--error-bg); color: var(--error-text); font-size: 11.5px; }
.source-error button { margin-left: auto; padding: 0; border: 0; background: transparent; color: inherit; cursor: pointer; text-decoration: underline; }
.answer-empty { min-height: 104px; display: flex; align-items: center; justify-content: center; flex-direction: column; gap: 4px; border: 1px solid var(--border); border-radius: 6px; color: var(--text-muted); font-size: 12px; text-align: center; }
.answer-empty strong { color: var(--text-primary); font-size: 14px; }
.answer-feedback { margin-top: 12px; display: flex; align-items: center; flex-wrap: wrap; gap: 8px; }
.answer-feedback .fb-label { color: var(--text-faint); font-size: 11px; }
.answer-feedback button { padding: 2px 10px; border: 1px solid var(--border); border-radius: 999px; background: var(--bg-card); color: var(--text-muted); font: inherit; font-size: 11px; cursor: pointer; }
.answer-feedback button:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.answer-feedback button.active { border-color: var(--primary); background: var(--primary-bg); color: var(--primary); font-weight: 650; }
.answer-feedback button:disabled { opacity: .6; cursor: default; }
.answer-feedback .fb-done { color: var(--text-faint); font-size: 10.5px; }
.answer-feedback .fb-error { color: var(--error-text); font-size: 11px; }
@media (max-width: 680px) {
  .kb-answer { font-size: 13px; }
  .answer-lead { padding: 13px 12px 14px; }
  .answer-topline { align-items: flex-start; flex-wrap: wrap; }
  .answer-meta { white-space: normal; }
  .answer-lead h3 { font-size: 16px; }
  .answer-summary { grid-template-columns: 1fr; gap: 3px; padding: 9px 10px; }
  .answer-body :deep(h3), .answer-body :deep(h4), .answer-body :deep(h5), .answer-body :deep(h6) { margin-top: 15px; }
  .answer-body :deep(.kb-key-line) { grid-template-columns: 1fr; gap: 2px; }
  .answer-body :deep(.kb-list.points) { grid-template-columns: 1fr; }
  .answer-body :deep(th), .answer-body :deep(td) { min-width: 112px; padding: 7px 8px; }
  .evidence-toggle { grid-template-columns: 10px minmax(0, 1fr); }
  .source-action { grid-column: 2; text-align: left; }
  .source-main strong, .source-main span { white-space: normal; overflow-wrap: anywhere; }
  .source-preview, .source-error, .source-governance { padding-left: 10px; }
  .source-governance > * { max-width: 100%; }
  .source-preview pre { max-width: 100%; overflow-wrap: anywhere; word-break: break-word; }
}
</style>
