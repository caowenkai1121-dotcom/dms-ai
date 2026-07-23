<script setup lang="ts">
import { ref } from 'vue'

interface AskResult {
  sql: string
  columns: string[]
  rows: unknown[][]
  row_count: number
  truncated: boolean
  elapsed_ms: number
  route: string
}

const question = ref('')
const loginName = ref('admin')
const roleCode = ref('')
const loading = ref(false)
const error = ref('')
const result = ref<AskResult | null>(null)
const showSql = ref(false)

async function ask() {
  if (!question.value.trim() || loading.value) return
  loading.value = true
  error.value = ''
  result.value = null
  try {
    const resp = await fetch('/api/ask', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        question: question.value,
        login_name: loginName.value,
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
</script>

<template>
  <a-layout style="min-height: 100vh">
    <a-layout-header style="background: #001529; color: #fff; font-size: 18px; font-weight: 600">
      DMS AI · 智能取数
    </a-layout-header>
    <a-layout-content style="padding: 24px; max-width: 1080px; margin: 0 auto; width: 100%">
      <a-space style="margin-bottom: 12px">
        <a-input v-model:value="loginName" addon-before="登录名" style="width: 200px" />
        <a-input v-model:value="roleCode" addon-before="角色" placeholder="留空取默认" style="width: 220px" />
      </a-space>
      <a-input-search
        v-model:value="question"
        placeholder="例如：本月销售额是多少 / 查一下昨天的订单明细"
        enter-button="提问"
        size="large"
        :loading="loading"
        @search="ask"
      />

      <a-alert v-if="error" type="error" :message="error" show-icon style="margin-top: 16px" />

      <a-spin :spinning="loading" tip="生成中…" style="margin-top: 16px; display: block">
        <template v-if="result">
          <a-card style="margin-top: 16px">
            <template #title>
              结果（{{ result.row_count }} 行{{ result.truncated ? '，已截断至 200' : '' }}）
            </template>
            <template #extra>
              <a-tag>{{ result.route }}</a-tag>
              <a-tag color="blue">{{ result.elapsed_ms }}ms</a-tag>
              <a @click="showSql = !showSql">{{ showSql ? '隐藏' : '查看' }} SQL</a>
            </template>
            <pre v-if="showSql" style="background: #f5f5f5; padding: 12px; overflow-x: auto; font-size: 12px">{{ result.sql }}</pre>
            <a-table
              :columns="result.columns.map((c, i) => ({ title: c, dataIndex: i, key: i, ellipsis: true }))"
              :data-source="result.rows.map((r, i) => ({ key: i, ...Object.fromEntries(r.map((v, j) => [j, v])) }))"
              :scroll="{ x: true }"
              size="small"
              :pagination="{ pageSize: 20 }"
            />
          </a-card>
        </template>
      </a-spin>
    </a-layout-content>
  </a-layout>
</template>
