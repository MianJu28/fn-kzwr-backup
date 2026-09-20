<script>
  // 通知设置（告警 Webhook：地址 + 自定义请求头 + 请求体模板）
  import Icon from './Icon.svelte';
  import { toast } from '../lib/toast.js';

  export let webhookUrl = '';
  export let webhookHeaders = []; // [{ name, value }]
  export let webhookBody = '';
  export let busy = false;
  export let onSave = null; // (url, headers, bodyTemplate) => Promise<{success, error}>
  export let onTest = null; // (url, headers, bodyTemplate) => Promise<{success, status, error}>

  let input = webhookUrl || '';
  let headers = [];
  let body = webhookBody || '';
  let msg = '';
  let msgOk = false;
  let working = false;
  let testing = false;

  // 只在「父组件传入值真正变化」时回填本地可编辑副本。
  // ⚠️ 不能写成 `$: input = webhookUrl || ''`：绑定变量被响应式语句写入时，Svelte 会在
  //    输入回调里连带把 webhookUrl 置脏并重跑该语句 → 用户刚敲进去的字符立刻被清掉
  //    （地址 / 请求体 / 请求头都会变成完全无法输入）。加了「同值不覆盖」判断后，
  //    即使语句因脏标记重跑也不会动用户正在编辑的内容。
  let syncedUrl = null;
  let syncedHeaders = null;
  let syncedBody = null;

  $: if (webhookUrl !== syncedUrl) {
    syncedUrl = webhookUrl;
    input = webhookUrl || '';
  }
  $: if (webhookHeaders !== syncedHeaders) {
    syncedHeaders = webhookHeaders;
    headers = (webhookHeaders || []).map((h) => ({ name: h.name, value: h.value }));
  }
  $: if (webhookBody !== syncedBody) {
    syncedBody = webhookBody;
    body = webhookBody || '';
  }

  function addHeader() {
    headers = [...headers, { name: '', value: '' }];
  }

  function removeHeader(i) {
    headers = headers.filter((_, idx) => idx !== i);
  }

  const clean = () => headers.filter((h) => h.name.trim() !== '');

  async function save() {
    working = true;
    msg = '';
    const r = await onSave(input.trim(), clean(), body);
    working = false;
    if (r.success) {
      msg = input.trim() ? '已保存，后续告警将按此配置外发' : '已关闭 Webhook 外发';
      msgOk = true;
      toast.success(msg);
    } else {
      msg = `保存失败：${r.error}`;
      msgOk = false;
      toast.error(msg);
    }
  }

  async function test() {
    if (!input.trim()) {
      msg = '请先填写 Webhook 地址';
      msgOk = false;
      return;
    }
    testing = true;
    msg = '';
    const r = await onTest(input.trim(), clean(), body);
    testing = false;
    if (r.success) {
      msg = `测试成功：服务器返回 ${r.status ?? 200}`;
      msgOk = true;
      toast.success(msg, '连通性正常');
    } else {
      msg = `测试失败：${r.error}`;
      msgOk = false;
      toast.error(msg);
    }
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="bell" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">通知设置</h2>
      <p class="card-desc">
        备份/恢复失败或配置缺失会生成告警；填写 Webhook 后可同步外发（超时 5 秒，失败不影响主流程）
      </p>
    </div>
    <span class="badge {webhookUrl ? 'badge-ok' : ''}">{webhookUrl ? '已启用' : '未启用'}</span>
  </div>

  <div class="card-body">
    <label class="field">
      <span class="label">告警 Webhook 地址 <span class="opt">（留空则仅在应用内展示）</span></span>
      <input
        class="input mono"
        bind:value={input}
        type="text"
        placeholder="https://example.com/hook"
        autocomplete="off"
      />
    </label>

    <div class="divider-title">自定义请求头</div>
    <p class="card-desc head-desc">
      如 <code>Authorization: Bearer xxx</code>、<code>Content-Type: application/json</code>
    </p>
    {#each headers as h, i}
      <div class="header-row">
        <input class="input mono h-name" bind:value={h.name} placeholder="Header 名称" autocomplete="off" />
        <input class="input mono h-value" bind:value={h.value} placeholder="值" autocomplete="off" />
        <button class="btn-icon" type="button" on:click={() => removeHeader(i)} aria-label="删除请求头">
          <Icon name="trash" size={15} />
        </button>
      </div>
    {/each}
    <button class="btn btn-sm btn-ghost add" type="button" on:click={addHeader}>
      <Icon name="plus" size={13} />添加请求头
    </button>

    <label class="field body-field">
      <span class="label">请求体模板 <span class="opt">（留空则发送默认 JSON）</span></span>
      <textarea
        class="textarea mono"
        rows="4"
        bind:value={body}
        placeholder={'{"text":"[{{level}}] {{source}}: {{message}}"}'}
      ></textarea>
      <span class="field-hint">
        可用占位符：<code>&#123;&#123;message&#125;&#125;</code>
        <code>&#123;&#123;level&#125;&#125;</code>
        <code>&#123;&#123;source&#125;&#125;</code>
        <code>&#123;&#123;ts&#125;&#125;</code>
        <code>&#123;&#123;id&#125;&#125;</code>
      </span>
    </label>

    {#if msg}
      <div class="alert {msgOk ? 'alert-ok' : 'alert-danger'} msg">
        <Icon name={msgOk ? 'check-circle' : 'x-circle'} size={15} />
        <div class="alert-body">{msg}</div>
      </div>
    {/if}
  </div>

  <div class="card-foot foot">
    <button class="btn btn-ghost" on:click={test} disabled={busy || working || testing}>
      {#if testing}<span class="spin"></span>测试中…{:else}<Icon name="wifi" size={15} />测试连通性{/if}
    </button>
    <button class="btn btn-primary" on:click={save} disabled={busy || working || testing}>
      {#if working}<span class="spin"></span>保存中…{:else}<Icon name="check" size={15} />保存{/if}
    </button>
  </div>
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .head-desc {
    margin-bottom: var(--s2);
  }
  .header-row {
    display: flex;
    align-items: center;
    gap: var(--s2);
    margin-bottom: 6px;
  }
  .header-row .h-name {
    flex: 1;
    min-width: 0;
  }
  .header-row .h-value {
    flex: 1.4;
    min-width: 0;
  }
  .add {
    margin-top: 2px;
  }
  .body-field {
    margin-top: var(--s5);
  }
  .msg {
    margin-top: var(--s3);
  }
  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s3);
  }
</style>
