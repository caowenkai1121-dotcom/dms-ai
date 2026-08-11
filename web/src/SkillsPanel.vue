<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { authHeaders, authQuery, errMessage, errText } from './panel-utils'

/** 【提示词包 / Skills】`GET /api/skills` 的管理弹窗（对应后端 `skills_api`）。
 *  语义：**enabled 即注入**深度报告规划（PLAN）系统提示（最多 5 包、每包截前 2000 字，
 *  裹 `<untrusted_skill>`）；新建缺省 `enabled=false`（fail-closed）。
 *  读全认证、写一律 admin —— 写入口按 props.admin 隐藏，后端 `admin_only` 仍是唯一判据。
 *  Esc/遮罩关闭；401 交回父组件走会话过期。弹窗模式与 UsagePanel.vue 同款。 */
interface Skill {
  id: number
  name: string
  content: string
  enabled: boolean
  updated_by?: string
}

/** 启用上限（与后端注入口径一致：enabled 即注入，最多 5 包）。 */
const MAX_ENABLED = 5
/** 内容存储上限（字）；注入时只截前 2000 字，两个上限不是一回事。 */
const MAX_CONTENT_CHARS = 20000

const props = defineProps<{ token?: string; login?: string; admin?: boolean }>()
const emit = defineEmits<{
  (e: 'close'): void
  (e: 'auth-expired'): void
}>()

const loading = ref(true)
const error = ref('')
const skills = ref<Skill[]>([])

// 顶部表单：editingId=null 是新建（POST /api/skills），否则保存修改（PUT /api/skills/{id}）。
const editingId = ref<number | null>(null)
const formName = ref('')
const formContent = ref('')
const formError = ref('')
const saving = ref(false)
/** 行级操作（启停/删除）的互斥锁：一次只跑一个，避免连点把列表打乱。
 *  save 不在此锁内（走自己的 saving 闸），但保存中禁止 toggle、toggle 中禁止保存。 */
const busyId = ref<number | null>(null)
/** 表单基线：startEdit/resetForm 时快照，用于关闭前的脏检查。 */
const baseName = ref('')
const baseContent = ref('')
const formDirty = computed(() => formName.value !== baseName.value || formContent.value !== baseContent.value)
const enabledCount = computed(() => skills.value.filter((s) => s.enabled).length)

/** 写操作的身份回退：无 token 时 login_name 进 body（toggle/DELETE 无 body，走 query）。 */
function identBody(): Record<string, unknown> {
  return props.token ? {} : { login_name: props.login ?? '' }
}

async function load() {
  loading.value = true
  error.value = ''
  try {
    const r = await fetch(`/api/skills${authQuery(props.token, props.login)}`, { headers: authHeaders(props.token) })
    if (r.status === 401) {
      emit('auth-expired')
      error.value = '登录已失效，请重新登录'
      return
    }
    if (!r.ok) {
      error.value = await errText(r, '提示词包加载失败')
      return
    }
    const j: { skills?: Skill[] } = await r.json().catch(() => ({}))
    // 逐项防御：缺 id 的项会让 :key 冲突，直接滤掉
    skills.value = Array.isArray(j.skills) ? j.skills.filter((it) => typeof it?.id === 'number') : []
  } catch (e) {
    error.value = `提示词包加载失败（网络）：${errMessage(e)}`
  } finally {
    loading.value = false
  }
}

function resetForm() {
  editingId.value = null
  formName.value = ''
  formContent.value = ''
  formError.value = ''
  baseName.value = ''
  baseContent.value = ''
}
function startEdit(s: Skill) {
  editingId.value = s.id
  formName.value = s.name
  formContent.value = s.content
  formError.value = ''
  baseName.value = s.name
  baseContent.value = s.content
}

async function save() {
  const name = formName.value.trim()
  // 内容保留换行原样提交；空白/超长/控制字符的硬校验在后端 normalize，前端只挡明显的空。
  const content = formContent.value
  formError.value = ''
  if (!name) { formError.value = '名称不能为空'; return }
  if (!content.trim()) { formError.value = '内容不能为空'; return }
  saving.value = true
  try {
    const isEdit = editingId.value != null
    const r = await fetch(isEdit ? `/api/skills/${editingId.value}` : '/api/skills', {
      method: isEdit ? 'PUT' : 'POST',
      headers: authHeaders(props.token, true),
      body: JSON.stringify({ name, content, ...identBody() }),
    })
    if (r.status === 401) {
      emit('auth-expired')
      formError.value = '登录已失效，请重新登录'
      return
    }
    if (!r.ok) {
      formError.value = await errText(r, isEdit ? '保存失败' : '新建失败')
      return
    }
    resetForm()
    await load()
  } catch (e) {
    formError.value = `保存失败（网络）：${errMessage(e)}`
  } finally {
    saving.value = false
  }
}

async function toggle(s: Skill) {
  // 保存（save 后会 load() 全量替换列表）与 toggle 并发时 toggle 结果会被覆盖，互斥掉
  if (busyId.value != null || saving.value) return
  busyId.value = s.id
  error.value = ''
  try {
    const r = await fetch(`/api/skills/${s.id}/toggle${authQuery(props.token, props.login)}`, { method: 'POST', headers: authHeaders(props.token) })
    if (r.status === 401) {
      emit('auth-expired')
      error.value = '登录已失效，请重新登录'
      return
    }
    if (!r.ok) {
      error.value = await errText(r, '启停失败')
      return
    }
    // 后端返回翻转后的 enabled，直接采信就地更新，不为一格开关重拉全表。
    const j: { enabled?: boolean } = await r.json().catch(() => ({}))
    if (typeof j.enabled === 'boolean') s.enabled = j.enabled
    else await load()
  } catch (e) {
    error.value = `启停失败（网络）：${errMessage(e)}`
  } finally {
    busyId.value = null
  }
}

async function removeSkill(s: Skill) {
  if (busyId.value != null) return
  // 原生 confirm：管理弹窗里的低频高危操作，与全站自绘弹窗风格不一是已知取舍，暂不单独自绘
  if (!window.confirm(`确定删除提示词包“${s.name}”吗？此操作不可撤销。`)) return
  busyId.value = s.id
  error.value = ''
  try {
    const r = await fetch(`/api/skills/${s.id}${authQuery(props.token, props.login)}`, { method: 'DELETE', headers: authHeaders(props.token) })
    if (r.status === 401) {
      emit('auth-expired')
      error.value = '登录已失效，请重新登录'
      return
    }
    if (!r.ok) {
      error.value = await errText(r, '删除失败')
      return
    }
    if (editingId.value === s.id) resetForm()
    await load()
  } catch (e) {
    error.value = `删除失败（网络）：${errMessage(e)}`
  } finally {
    busyId.value = null
  }
}

/** tooltip 截断：content 最长 20000 字，原样塞 title 会撑爆悬浮框。 */
function contentTitle(text: string): string {
  return text.length > 200 ? `${text.slice(0, 200)}…` : text
}

/** 关闭出口统一走这里：表单有未保存修改时先确认，避免 Esc/点遮罩静默丢内容。 */
function requestClose() {
  if (formDirty.value && !window.confirm('有未保存的修改，确定关闭吗？')) return
  emit('close')
}

function onEsc(e: KeyboardEvent) {
  if (e.key === 'Escape') requestClose()
}
onMounted(() => {
  void load()
  window.addEventListener('keydown', onEsc)
})
onBeforeUnmount(() => window.removeEventListener('keydown', onEsc))
</script>

<template>
  <div class="sk-mask" @click.self="requestClose">
    <section class="sk-dialog" role="dialog" aria-modal="true" aria-labelledby="sk-title">
      <header class="sk-head">
        <div>
          <span class="sk-kicker">提示词包</span>
          <h2 id="sk-title">提示词包管理</h2>
          <p class="sk-sub">
            注入深度报告规划提示词，admin 可写。启用即注入（已启用 {{ enabledCount }}/{{ MAX_ENABLED }} 包、每包前 2000 字），
            改动后约 2 分钟命中旧规划缓存。内容最多 {{ MAX_CONTENT_CHARS }} 字是存储上限，注入时只截前 2000 字。
          </p>
        </div>
        <button type="button" class="sk-close" title="关闭" aria-label="关闭" @click="requestClose">✕</button>
      </header>

      <!-- 新建 / 编辑表单（写一律 admin；非 admin 整个表单不出） -->
      <form v-if="admin" class="sk-form" @submit.prevent="save">
        <div class="sk-form-t">{{ editingId != null ? `编辑提示词包 · #${editingId}` : '新建提示词包' }}</div>
        <input v-model="formName" class="sk-input" maxlength="64" placeholder="名称（唯一，最多 64 字）" aria-label="提示词包名称" :disabled="saving" />
        <textarea v-model="formContent" class="sk-textarea" rows="5" :maxlength="MAX_CONTENT_CHARS" placeholder="内容：注入深度报告规划（PLAN）系统提示的文本，最多 20000 字" aria-label="提示词包内容" :disabled="saving"></textarea>
        <div v-if="formError" class="sk-form-err">{{ formError }}</div>
        <div class="sk-form-ops">
          <button type="submit" class="sk-btn primary" :disabled="saving || busyId != null">{{ saving ? '保存中…' : editingId != null ? '保存修改' : '新建' }}</button>
          <button v-if="editingId != null" type="button" class="sk-btn" :disabled="saving" @click="resetForm">取消编辑</button>
          <span v-if="editingId == null" class="sk-form-note">新建缺省不启用，保存后在列表里手动开启</span>
        </div>
      </form>

      <div v-if="loading" class="sk-state"><span class="sk-spin"></span>提示词包加载中…</div>
      <div v-else-if="error && !skills.length" class="sk-state sk-error">{{ error }}</div>
      <template v-else>
        <div v-if="error" class="sk-list-err">{{ error }}</div>
        <div v-if="!skills.length" class="sk-note">还没有提示词包{{ admin ? '，用上面的表单新建一个' : '' }}</div>
        <div v-for="s in skills" :key="s.id" class="sk-item" :class="{ off: !s.enabled }">
          <div class="sk-item-hd">
            <b class="sk-name" :title="s.name">{{ s.name }}</b>
            <label v-if="admin" class="sk-switch" :title="s.enabled ? '点击停用（不再注入）' : '点击启用（注入深度报告规划）'">
              <input type="checkbox" :checked="s.enabled" :disabled="busyId != null" @change="toggle(s)" />
              <span>{{ s.enabled ? '已启用' : '已停用' }}</span>
            </label>
            <span v-else class="sk-flag" :class="{ on: s.enabled }">{{ s.enabled ? '已启用' : '已停用' }}</span>
            <template v-if="admin">
              <button type="button" class="sk-btn" :disabled="busyId != null" @click="startEdit(s)">编辑</button>
              <button type="button" class="sk-btn danger" :disabled="busyId != null" @click="removeSkill(s)">删除</button>
            </template>
          </div>
          <div class="sk-content" :title="contentTitle(s.content)">{{ s.content }}</div>
          <div class="sk-item-meta">#{{ s.id }}<template v-if="s.updated_by"> · 最近修改 {{ s.updated_by }}</template></div>
        </div>
      </template>
    </section>
  </div>
</template>

<style>
/* 遮罩/对话框/头部/关闭按钮/加载态样式与 UsagePanel.vue 同款，调整时请两边同步 */
.sk-mask { position: fixed; inset: 0; z-index: 1100; display: grid; place-items: center; padding: 20px; background: rgba(17, 24, 39, .38); backdrop-filter: blur(5px); }
.sk-dialog { width: min(560px, 100%); max-height: 86vh; overflow-y: auto; border: 1px solid var(--border); border-radius: 8px; background: var(--bg-card); box-shadow: 0 24px 70px rgba(17, 24, 39, .2); padding-bottom: 20px; }
.sk-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 20px; padding: 20px 22px 12px; }
.sk-kicker { display: block; margin-bottom: 5px; color: var(--primary); font-size: 11px; font-weight: 700; }
.sk-head h2 { margin: 0; color: var(--text-primary); font-size: 18px; font-weight: 700; }
.sk-sub { margin: 6px 0 0; color: var(--text-muted); font-size: 11.5px; line-height: 1.6; }
.sk-close { width: 30px; height: 30px; flex-shrink: 0; border: 0; border-radius: 5px; background: transparent; color: var(--text-muted); cursor: pointer; }
.sk-close:hover { background: var(--bg-hover); color: var(--text-primary); }
.sk-state { display: flex; align-items: center; justify-content: center; gap: 9px; padding: 34px 22px; color: var(--text-muted); font-size: 13px; }
.sk-error { color: var(--error-text); line-height: 1.7; text-align: center; }
.sk-spin { width: 14px; height: 14px; border: 2px solid var(--primary); border-top-color: transparent; border-radius: 50%; animation: skSpin .7s linear infinite; }
@keyframes skSpin { to { transform: rotate(360deg); } }
.sk-note { margin: 0 22px 12px; font-size: 12px; color: var(--text-faint); }
.sk-list-err { margin: 0 22px 10px; font-size: 12px; color: var(--error-text); line-height: 1.6; }

.sk-form { display: grid; gap: 8px; margin: 0 22px 14px; padding: 12px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg-main); }
.sk-form-t { font-size: 12px; font-weight: 600; color: var(--text-primary); }
.sk-input { padding: 7px 10px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card); color: var(--text-primary); font-size: 12.5px; }
.sk-input:focus { outline: none; border-color: var(--primary); }
.sk-textarea { padding: 7px 10px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card); color: var(--text-primary); font-size: 12.5px; line-height: 1.6; resize: vertical; font-family: inherit; }
.sk-textarea:focus { outline: none; border-color: var(--primary); }
.sk-form-err { font-size: 12px; color: var(--error-text); line-height: 1.6; }
.sk-form-ops { display: flex; align-items: center; gap: 8px; }
.sk-form-note { font-size: 11px; color: var(--text-faint); }

.sk-btn { padding: 4px 12px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-card); color: var(--text-regular); font-size: 12px; cursor: pointer; }
.sk-btn:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.sk-btn:disabled { opacity: .5; cursor: not-allowed; }
.sk-btn.primary { border-color: var(--primary); background: var(--primary); color: #fff; }
.sk-btn.primary:hover:not(:disabled) { color: #fff; opacity: .88; }
.sk-btn.danger:hover:not(:disabled) { border-color: var(--error-text); color: var(--error-text); }

.sk-item { margin: 0 22px 10px; padding: 10px 12px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg-main); }
.sk-item.off { opacity: .72; }
.sk-item-hd { display: flex; align-items: center; gap: 8px; }
.sk-name { flex: 1; min-width: 0; color: var(--text-primary); font-size: 13px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.sk-switch { display: inline-flex; align-items: center; gap: 5px; flex-shrink: 0; color: var(--text-muted); font-size: 11px; cursor: pointer; }
.sk-switch input { accent-color: var(--primary); cursor: pointer; }
.sk-flag { flex-shrink: 0; font-size: 11px; color: var(--text-faint); }
.sk-flag.on { color: var(--success-text); }
.sk-content { display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 3; overflow: hidden; margin-top: 6px; color: var(--text-muted); font-size: 12px; line-height: 1.6; white-space: pre-wrap; word-break: break-all; }
.sk-item-meta { margin-top: 6px; color: var(--text-faint); font-size: 10.5px; }
</style>
