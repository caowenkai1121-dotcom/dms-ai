<script setup lang="ts">
import { ref, onMounted } from 'vue'

const health = ref<Record<string, unknown> | null>(null)

onMounted(async () => {
  try {
    health.value = await (await fetch('/api/health')).json()
  } catch {
    health.value = { ok: false, error: '后端未启动' }
  }
})
</script>

<template>
  <a-layout style="min-height: 100vh">
    <a-layout-content style="padding: 48px; max-width: 720px; margin: 0 auto">
      <a-typography-title :level="3">DMS AI · M0 骨架</a-typography-title>
      <a-card title="服务健康">
        <pre>{{ JSON.stringify(health, null, 2) }}</pre>
      </a-card>
    </a-layout-content>
  </a-layout>
</template>
