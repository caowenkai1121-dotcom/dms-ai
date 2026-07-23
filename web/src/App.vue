<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import BiChart from './BiChart.vue'
import { fmt, type Semantic } from './format'

interface ColSpec { name: string; role: string; semantic: Semantic }
interface Delta { pct: number; dir: 'up' | 'down' | 'flat'; label: string }
interface Kpi { label: string; value: unknown; semantic: Semantic; delta?: Delta }
interface Block {
  type: 'kpis' | 'entity' | 'chart' | 'table'
  items?: Kpi[]
  pairs?: [string, unknown][]
  kind?: 'bar' | 'line' | 'pie'
  x?: number
  y?: number[]
  top?: number | null
}
interface Interact { drill?: string[] }
interface ViewSpec { columns: ColSpec[]; blocks: Block[]; interact?: Interact }
interface AskResult {
  sql: string; columns: string[]; rows: unknown[][]; row_count: number
  truncated: boolean; elapsed_ms: number; route: string; view: ViewSpec
}

const question = ref('')
const loginName = ref('admin')
const roleCode = ref('')
const sessionToken = ref('')      // SSO 换签后的会话 token（端#2 DMS 嵌入）
const embedded = ref(false)       // 嵌入模式：隐藏登录名输入框
const loading = ref(false)
const error = ref('')
const result = ref<AskResult | null>(null)
const showSql = ref(false)

const routeLabel: Record<string, string> = {
  'direct-doc': '单号直查', 'direct-agg': '快速聚合', llm: 'AI 生成', 'llm+repair': 'AI 生成(自修)',
}

const cols = computed(() => result.value?.view.columns ?? [])
const lastQuestion = ref('')

// 端#3 企微：OAuth 回调 302 → /#token=xxx（fragment 不进服务端日志）
onMounted(() => {
  const tm = location.hash.match(/token=([^&]+)/)
  if (tm) {
    sessionToken.value = tm[1]
    embedded.value = true
    loginName.value = '企微用户'
    history.replaceState(null, '', location.pathname) // 清 fragment 防泄漏
  }
})

// 端#2 DMS 嵌入：URL 带 dms_token → SSO 换会话 token（免登）
onMounted(async () => {
  const p = new URLSearchParams(location.search)
  const dmsToken = p.get('dms_token')
  if (!dmsToken) return
  embedded.value = true
  try {
    const resp = await fetch('/api/sso', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ dms_token: dmsToken, role_code: p.get('role') || null }),
    })
    const d = await resp.json()
    if (resp.ok) {
      sessionToken.value = d.token
      loginName.value = d.login_name
    } else {
      error.value = `SSO 认证失败：${d.error || ''}`
    }
  } catch (e) {
    error.value = `SSO 认证失败：${e}`
  }
})

// 下钻：原问题 + "按X" 参数化重问（对齐 SuperSonic onLoadData 追加维度）
function drill(dim: string) {
  question.value = `${lastQuestion.value} 按${dim}`
  ask()
}

async function ask() {
  if (!question.value.trim() || loading.value) return
  lastQuestion.value = question.value
  loading.value = true
  error.value = ''
  result.value = null
  try {
    const headers: Record<string, string> = { 'Content-Type': 'application/json' }
    if (sessionToken.value) headers.Authorization = `Bearer ${sessionToken.value}`
    const resp = await fetch('/api/ask', {
      method: 'POST',
      headers,
      // 会话 token 优先；无 token 时用 login_name（开发/独立模式）
      body: JSON.stringify({
        question: question.value,
        login_name: sessionToken.value ? null : loginName.value,
        role_code: roleCode.value || null,
      }),
    })
    const data = await resp.json()
    if (!resp.ok) error.value = data.error || '请求失败'
    else result.value = data
  } catch (e) {
    error.value = String(e)
  } finally {
    loading.value = false
  }
}

// 表格列（Ant Table）
const tableCols = computed(() =>
  cols.value.map((c, i) => ({
    title: c.name, dataIndex: i, key: i, ellipsis: true,
    align: c.role === 'metric' ? 'right' : 'left',
    customRender: ({ text }: { text: unknown }) => fmt(text, c.semantic),
  }))
)
const tableData = computed(() =>
  (result.value?.rows ?? []).map((r, i) => ({ key: i, ...Object.fromEntries(r.map((v, j) => [j, v])) }))
)
</script>

<template>
  <a-layout style="min-height: 100vh">
    <a-layout-header style="background: #001529; color: #fff; font-size: 18px; font-weight: 600; display: flex; align-items: center">
      皇家小虎 · DMS 智能取数
    </a-layout-header>
    <a-layout-content style="padding: 24px; max-width: 1120px; margin: 0 auto; width: 100%">
      <a-space v-if="!embedded" style="margin-bottom: 12px">
        <a-input v-model:value="loginName" addon-before="登录名" style="width: 200px" />
        <a-input v-model:value="roleCode" addon-before="角色" placeholder="留空取默认" style="width: 220px" />
      </a-space>
      <div v-else style="margin-bottom: 12px; color: #888; font-size: 13px">
        已登录：<b>{{ loginName || '认证中…' }}</b>（DMS 免登）
      </div>
      <a-input-search
        v-model:value="question"
        placeholder="例如：本月销售额是多少 / 本月销售额前五的省份 / 查一下昨天的订单明细"
        enter-button="提问" size="large" :loading="loading" @search="ask"
      />

      <a-alert v-if="error" type="error" :message="error" show-icon style="margin-top: 16px" />

      <a-spin :spinning="loading" tip="生成中…" style="margin-top: 16px; display: block; min-height: 40px">
        <div v-if="result">
          <div style="display: flex; gap: 8px; align-items: center; margin: 16px 0 8px; color: #888; font-size: 13px">
            <a-tag color="blue">{{ routeLabel[result.route] || result.route }}</a-tag>
            <span>{{ result.row_count }} 行{{ result.truncated ? '（截断至 200）' : '' }} · {{ result.elapsed_ms }}ms</span>
            <a style="margin-left: auto" @click="showSql = !showSql">{{ showSql ? '隐藏' : '查看' }} SQL</a>
          </div>
          <pre v-if="showSql" style="background: #f5f5f5; padding: 12px; border-radius: 6px; overflow-x: auto; font-size: 12px">{{ result.sql }}</pre>

          <template v-for="(b, bi) in result.view.blocks" :key="bi">
            <!-- KPI 卡带 -->
            <div v-if="b.type === 'kpis'" style="display: flex; gap: 16px; flex-wrap: wrap; margin-bottom: 16px">
              <a-card v-for="(k, ki) in b.items" :key="ki" :bordered="false"
                style="flex: 1; min-width: 200px; box-shadow: 0 1px 6px rgba(0,0,0,0.08)">
                <div style="color: #888; font-size: 13px; letter-spacing: 0.05em">{{ k.label }}</div>
                <div style="font-size: 28px; font-weight: 700; color: #1677ff; margin-top: 6px">{{ fmt(k.value, k.semantic) }}</div>
                <div v-if="k.delta" style="margin-top: 6px; font-size: 13px"
                  :style="{ color: k.delta.dir === 'up' ? '#cf1322' : k.delta.dir === 'down' ? '#389e0d' : '#888' }">
                  <span>{{ k.delta.dir === 'up' ? '▲' : k.delta.dir === 'down' ? '▼' : '—' }}</span>
                  {{ Math.abs(k.delta.pct) }}% <span style="color: #aaa">{{ k.delta.label }}</span>
                </div>
              </a-card>
            </div>

            <!-- 实体卡（单据卡） -->
            <a-card v-else-if="b.type === 'entity'" title="单据详情" style="margin-bottom: 16px">
              <a-descriptions bordered size="small" :column="2">
                <a-descriptions-item v-for="(p, pi) in b.pairs" :key="pi" :label="p[0]">{{ p[1] }}</a-descriptions-item>
              </a-descriptions>
            </a-card>

            <!-- 图表 -->
            <a-card v-else-if="b.type === 'chart'" style="margin-bottom: 16px">
              <BiChart :kind="b.kind!" :columns="cols" :rows="result.rows" :x="b.x!" :y="b.y!" :top="b.top" />
            </a-card>

            <!-- 表格 -->
            <a-card v-else-if="b.type === 'table'" style="margin-bottom: 16px" :body-style="{ padding: '0' }">
              <a-table :columns="tableCols" :data-source="tableData" :scroll="{ x: true }"
                size="small" :pagination="{ pageSize: 20, hideOnSinglePage: true }" />
            </a-card>
          </template>

          <!-- 下钻维度 chips（SuperSonic 招牌交互） -->
          <div v-if="result.view.interact?.drill?.length" style="margin-top: 4px">
            <span style="color: #888; font-size: 13px; margin-right: 8px">换个维度看：</span>
            <a-tag v-for="d in result.view.interact.drill" :key="d" color="blue"
              style="cursor: pointer; user-select: none" @click="drill(d)">按{{ d }} ↓</a-tag>
          </div>
        </div>
      </a-spin>
    </a-layout-content>
  </a-layout>
</template>
