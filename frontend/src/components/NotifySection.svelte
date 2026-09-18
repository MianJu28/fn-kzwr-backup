<script>
  // 通知设置（监控告警 Webhook）
  export let webhookUrl = '';
  export let busy = false;
  // onSave(webhookUrl: string) => Promise<{success, error}>
  export let onSave = null;

  let input = webhookUrl || '';
  let msg = '';
  let msgOk = false;
  let working = false;

  $: if (webhookUrl !== undefined) input = webhookUrl;

  async function save() {
    working = true;
    const r = await onSave(input.trim());
    working = false;
    if (r.success) {
      msg = input.trim() ? 'Webhook 已保存，后续告警将外发' : '已关闭 Webhook 外发';
      msgOk = true;
    } else {
      msg = `保存失败: ${r.error}`;
      msgOk = false;
    }
  }
</script>

<section>
  <h2>🔔 通知设置</h2>
  <p class="hint">
    备份/恢复失败或配置缺失时会生成告警并在页面顶部展示。填写 Webhook 地址后，告警会同时以 JSON POST 外发（超时 5 秒，失败不影响备份主流程）。
  </p>

  <label>告警 Webhook 地址（留空则仅在应用内展示）
    <input
      bind:value={input}
      type="text"
      placeholder="https://example.com/hook"
      autocomplete="off"
    />
  </label>

  <button on:click={save} disabled={busy || working}>{working ? '保存中...' : '保存'}</button>

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
  .hint {
    color: #5a6a7a;
    font-size: 13px;
    line-height: 1.6;
    margin: 0 0 14px;
  }
  label { display: block; margin: 4px 0 4px; font-size: 14px; color: #42526e; }
  input {
    width: 100%;
    padding: 8px 10px;
    border: 1px solid #d0d7e2;
    border-radius: 6px;
    margin-top: 6px;
    font-size: 13px;
    font-family: monospace;
    box-sizing: border-box;
  }
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
  .ok { color: #22a06b; font-size: 13px; }
  .warn { color: #b45309; font-size: 13px; }
</style>
