<script>
  // 通知设置（监控告警 Webhook：地址 + 自定义请求头 + 请求体模板）
  export let webhookUrl = '';
  export let webhookHeaders = []; // [{ name, value }]
  export let webhookBody = '';
  export let busy = false;
  // onSave(url, headers, bodyTemplate) => Promise<{success, error}>
  export let onSave = null;
  // onTest(url, headers, bodyTemplate) => Promise<{success, status, error}>
  export let onTest = null;

  let input = webhookUrl || '';
  let headers = [];
  let body = webhookBody || '';
  let msg = '';
  let msgOk = false;
  let working = false;
  let testing = false;

  $: if (webhookUrl !== undefined) input = webhookUrl;
  $: headers = (webhookHeaders || []).map((h) => ({ name: h.name, value: h.value }));
  $: body = webhookBody || '';

  function addHeader() {
    headers = [...headers, { name: '', value: '' }];
  }

  function removeHeader(i) {
    headers = headers.filter((_, idx) => idx !== i);
  }

  async function save() {
    working = true;
    const cleanHeaders = headers.filter((h) => h.name.trim() !== '');
    const r = await onSave(input.trim(), cleanHeaders, body);
    working = false;
    if (r.success) {
      msg = input.trim() ? 'Webhook 配置已保存，后续告警将按此配置外发' : '已关闭 Webhook 外发';
      msgOk = true;
    } else {
      msg = `保存失败: ${r.error}`;
      msgOk = false;
    }
  }

  // 测试连通性：用当前表单值直接发送一条测试通知
  async function test() {
    if (!input.trim()) {
      msg = '请先填写 Webhook 地址';
      msgOk = false;
      return;
    }
    testing = true;
    msg = '';
    const cleanHeaders = headers.filter((h) => h.name.trim() !== '');
    const r = await onTest(input.trim(), cleanHeaders, body);
    testing = false;
    if (r.success) {
      msg = `测试成功：服务器返回 ${r.status ?? 200}`;
      msgOk = true;
    } else {
      msg = `测试失败: ${r.error}`;
      msgOk = false;
    }
  }
</script>

<section>
  <h2>🔔 通知设置</h2>
  <p class="hint">
    备份/恢复失败或配置缺失时会生成告警并在页面顶部展示。填写 Webhook 地址后，告警会同时外发（超时 5 秒，失败不影响备份主流程）。
  </p>

  <label>告警 Webhook 地址（留空则仅在应用内展示）
    <input
      bind:value={input}
      type="text"
      placeholder="https://example.com/hook"
      autocomplete="off"
    />
  </label>

  <div class="headers-block">
    <h3>自定义请求头</h3>
    <p class="hint sub">如 <code>Authorization: Bearer xxx</code>、<code>Content-Type: application/json</code>。</p>
    {#each headers as h, i}
      <div class="header-row">
        <input class="h-name" bind:value={h.name} placeholder="Header 名称" autocomplete="off" />
        <input class="h-value" bind:value={h.value} placeholder="值" autocomplete="off" />
        <button class="remove" type="button" on:click={() => removeHeader(i)}>✕</button>
      </div>
    {/each}
    <button class="ghost" type="button" on:click={addHeader}>+ 添加请求头</button>
  </div>

  <label>请求体模板（留空则发送默认 JSON）
    <textarea
      rows="4"
      bind:value={body}
      placeholder={'{"text":"[{{level}}] {{source}}: {{message}}"}'}
    ></textarea>
  </label>
  <p class="hint sub">
    可用占位符：<code>&#123;&#123;message&#125;&#125;</code>
    <code>&#123;&#123;level&#125;&#125;</code>
    <code>&#123;&#123;source&#125;&#125;</code>
    <code>&#123;&#123;ts&#125;&#125;</code>
    <code>&#123;&#123;id&#125;&#125;</code>
  </p>

  <div class="btn-row">
    <button on:click={save} disabled={busy || working || testing}>{working ? '保存中...' : '保存'}</button>
    <button class="ghost" on:click={test} disabled={busy || working || testing}>
      {testing ? '测试中...' : '测试连通性'}
    </button>
  </div>

  {#if msg}
    <p class:ok={msgOk} class:warn={!msgOk}>{msg}</p>
  {/if}
</section>

<style>
  section {
    background: #fff;
    border-radius: 10px;
    padding: 20px;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.06);
  }
  h2 { margin: 0 0 8px; font-size: 18px; }
  h3 { margin: 0 0 4px; font-size: 15px; }
  .hint {
    color: #5a6a7a;
    font-size: 13px;
    line-height: 1.6;
    margin: 0 0 14px;
  }
  .hint.sub { margin: 4px 0 8px; font-size: 12px; }
  label { display: block; margin: 4px 0 4px; font-size: 14px; color: #42526e; }
  input,
  textarea {
    width: 100%;
    padding: 8px 10px;
    border: 1px solid #d0d7e2;
    border-radius: 6px;
    margin-top: 6px;
    font-size: 13px;
    font-family: monospace;
    box-sizing: border-box;
  }
  textarea { resize: vertical; }
  code { background: #f1f5f9; padding: 1px 5px; border-radius: 4px; font-size: 12px; }
  .headers-block { margin-top: 16px; padding-top: 14px; border-top: 1px solid #eef1f6; }
  .header-row { display: flex; gap: 8px; margin-top: 6px; align-items: center; }
  .header-row input { margin: 0; }
  .header-row .h-name { flex: 1; min-width: 0; }
  .header-row .h-value { flex: 1.4; min-width: 0; }
  button {
    background: #2563eb;
    color: #fff;
    border: none;
    border-radius: 6px;
    padding: 9px 16px;
    font-size: 14px;
    cursor: pointer;
    margin-top: 12px;
  }
  button:disabled { background: #9db4e8; cursor: not-allowed; }
  button.ghost { background: #eef2ff; color: #2563eb; margin-top: 0; }
  .btn-row { display: flex; gap: 10px; flex-wrap: wrap; margin-top: 12px; }
  .btn-row button { margin-top: 0; }
  button.remove {
    background: transparent;
    color: #b91c1c;
    padding: 2px 8px;
    margin: 0;
    flex-shrink: 0;
  }
  .ok { color: #22a06b; font-size: 13px; }
  .warn { color: #b45309; font-size: 13px; }
</style>
