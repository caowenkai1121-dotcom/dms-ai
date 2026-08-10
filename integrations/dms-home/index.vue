<template>
  <section class="dms-agent-home">
    <iframe
      :key="frameKey"
      ref="agentFrame"
      class="dms-agent-frame"
      :src="agentUrl"
      title="DMS 数据智能助手"
      allow="clipboard-write"
      @load="onFrameLoad"
    />

    <div v-if="loading && !errorMessage" class="dms-agent-state" role="status">
      <span class="dms-agent-spinner" aria-hidden="true"></span>
      <strong>正在进入数据智能助手</strong>
      <span>正在复用当前 DMS 登录身份</span>
    </div>

    <div v-if="errorMessage" class="dms-agent-state dms-agent-error" role="alert">
      <strong>AI 首页加载失败</strong>
      <span>{{ errorMessage }}</span>
      <button type="button" @click="reload">重新加载</button>
    </div>
  </section>
</template>

<script setup>
  import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
  import LocalStorageKeyConst from '/@/constants/local-storage-key-const';
  import { localRead } from '/@/utils/local-util';

  const agentFrame = ref(null);
  const frameKey = ref(0);
  const loading = ref(true);
  const errorMessage = ref('');
  let loadTimer = 0;
  let ssoPending = false;

  const configuredBase = String(import.meta.env.VITE_AGENT_DOMAIN || 'http://localhost:5180').trim();
  const agentUrl = computed(() => {
    const url = new URL(configuredBase, window.location.origin);
    url.searchParams.set('embed', 'dms-home');
    return url.toString();
  });
  const agentOrigin = computed(() => new URL(agentUrl.value).origin);

  function clearLoadTimer() {
    if (loadTimer) window.clearTimeout(loadTimer);
    loadTimer = 0;
  }

  function armLoadTimer() {
    clearLoadTimer();
    loadTimer = window.setTimeout(() => {
      if (ssoPending) errorMessage.value = '身份认证超时，请检查助手地址或重新加载。';
    }, 15000);
  }

  function sendCurrentIdentity() {
    if (ssoPending) return;
    const dmsToken = localRead(LocalStorageKeyConst.USER_TOKEN);
    if (!dmsToken) {
      clearLoadTimer();
      ssoPending = false;
      loading.value = false;
      errorMessage.value = '当前 DMS 登录已失效，请重新登录 DMS。';
      return;
    }
    ssoPending = true;
    armLoadTimer();
    agentFrame.value?.contentWindow?.postMessage({ type: 'dms-ai:sso', dmsToken }, agentOrigin.value);
  }

  function receiveAgentMessage(event) {
    if (event.source !== agentFrame.value?.contentWindow || event.origin !== agentOrigin.value) return;
    if (event.data?.type === 'dms-ai:ready') {
      if (event.data.reason === 'expired') ssoPending = false;
      sendCurrentIdentity();
      return;
    }
    if (event.data?.type === 'dms-ai:sso-ok') {
      clearLoadTimer();
      ssoPending = false;
      loading.value = false;
      errorMessage.value = '';
      return;
    }
    if (event.data?.type === 'dms-ai:sso-error') {
      clearLoadTimer();
      ssoPending = false;
      loading.value = false;
      errorMessage.value = event.data.message || 'DMS 身份认证失败，请重新加载。';
    }
  }

  function onFrameLoad() {
    loading.value = false;
    errorMessage.value = '';
    sendCurrentIdentity();
  }

  function reload() {
    clearLoadTimer();
    ssoPending = false;
    loading.value = true;
    errorMessage.value = '';
    frameKey.value += 1;
  }

  onMounted(() => {
    window.addEventListener('message', receiveAgentMessage);
  });

  onBeforeUnmount(() => {
    clearLoadTimer();
    window.removeEventListener('message', receiveAgentMessage);
  });
</script>

<style lang="less" scoped>
  .dms-agent-home {
    position: relative;
    width: 100%;
    height: calc(100vh - 112px);
    min-height: 640px;
    overflow: hidden;
    background: #f5f7fb;
  }

  .dms-agent-frame {
    display: block;
    width: 100%;
    height: 100%;
    border: 0;
    background: #f5f7fb;
  }

  .dms-agent-state {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    color: #667085;
    background: rgba(245, 247, 251, 0.96);
  }

  .dms-agent-state strong {
    color: #182230;
    font-size: 16px;
  }

  .dms-agent-spinner {
    width: 28px;
    height: 28px;
    border: 3px solid #d8def5;
    border-top-color: #4f5bd5;
    border-radius: 50%;
    animation: dms-agent-spin 0.8s linear infinite;
  }

  .dms-agent-error button {
    padding: 7px 16px;
    border: 1px solid #4f5bd5;
    border-radius: 4px;
    color: #fff;
    background: #4f5bd5;
    cursor: pointer;
  }

  @keyframes dms-agent-spin {
    to {
      transform: rotate(360deg);
    }
  }

  @media (max-width: 768px) {
    .dms-agent-home {
      height: calc(100vh - 88px);
      min-height: 520px;
    }
  }
</style>
