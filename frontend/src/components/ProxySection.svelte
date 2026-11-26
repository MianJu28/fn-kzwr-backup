<script>
  /**
   * 网络代理设置（全局一份）
   *
   * ## 为什么是「全局定义 + 按目标启用」
   * 代理通常是环境级的（公司出口 / 软路由 / 机场），改一次希望全员生效；
   * 但「哪些目标该走代理」是每个目标自己的事 —— 官方 kzwr 可能要翻墙，
   * 自建局域网 WebDAV 则绝不能被代理（内网地址发到出口就废了）。
   * 因此地址与凭据在这里维护，开关在「目标」页逐个勾选。
   */
  import Icon from './Icon.svelte';
  import { api } from '../lib/api.js';
  import { toast } from '../lib/toast.js';

  export let busy = false;

  let loading = true;
  let saving = false;
  let enabled = false;
  let url = '';
  let noProxy = '';
  let username = '';
  let password = '';
  let usernameSet = false;
  let usable = false;
  let msg = '';
  let msgOk = true;
  /** 测试结果（与「保存」结果分开显示，避免两个操作互相覆盖提示） */
  let testing = false;
  let testMsg = '';
  let testOk = true;
  let testDetail = '';

  async function load() {
    loading = true;
    try {
      const r = await api.getProxy();
      enabled = !!r.enabled;
      url = r.url || '';
      noProxy = r.no_proxy || '';
      usernameSet = !!r.username_set;
      usable = !!r.usable;
    } catch (e) {
      msg = `读取代理设置失败：${e.message || e}`;
      msgOk = false;
    } finally {
      loading = false;
    }
  }

  load();

  async function save() {
    saving = true;
    msg = '';
    try {
      const body = { enabled, url: url.trim(), no_proxy: noProxy.trim() };
      // 凭据「缺省不改」：只有用户真的填了才提交，否则后端保持原值
      if (username !== '') body.username = username;
      if (password !== '') body.password = password;
      const r = await api.saveProxy(body);
      if (r.success) {
        msg = enabled
          ? '已保存（下次备份/恢复生效；已勾选「使用代理」的目标会走它）'
          : '已保存（代理功能已关闭，所有目标直连）';
        msgOk = true;
        toast.success('代理设置已保存', '网络代理');
        username = '';
        password = '';
        await load();
      } else {
        msg = `保存失败：${r.error}`;
        msgOk = false;
      }
    } catch (e) {
      msg = `保存失败：${e.message || e}`;
      msgOk = false;
    } finally {
      saving = false;
    }
  }

  /**
   * 测试代理连通性
   *
   * 与「保存」的区别：保存只校验格式，这里**真的经代理发一次请求**。
   * 地址合法但代理没在跑、端口不通、认证错误，只有真连一次才能发现。
   */
  async function testProxy() {
    testing = true;
    testMsg = '';
    testDetail = '';
    try {
      const body = { enabled, url: url.trim(), no_proxy: noProxy.trim() };
      // 用户新填了凭据就带上；否则让后端用已保存的（留空 = 不改的同一语义）
      if (username !== '') body.username = username;
      if (password !== '') body.password = password;
      const r = await api.testProxy(body);
      if (r.success) {
        testOk = true;
        testMsg = r.message || '代理可用';
        // 401/403 也代表链路通，单独说明，免得用户以为失败
        testDetail = r.note || '';
      } else {
        testOk = false;
        testMsg = r.error || '测试失败';
        testDetail = r.hint || '';
      }
    } catch (e) {
      testOk = false;
      testMsg = `测试失败：${e.message || e}`;
    } finally {
      testing = false;
    }
  }

  async function clearCredentials() {
    saving = true;
    msg = '';
    try {
      const r = await api.saveProxy({
        enabled,
        url: url.trim(),
        no_proxy: noProxy.trim(),
        username: '',
        password: '',
      });
      if (r.success) {
        msg = '已清除代理认证凭据';
        msgOk = true;
        await load();
      } else {
        msg = `清除失败：${r.error}`;
        msgOk = false;
      }
    } finally {
      saving = false;
    }
  }
</script>

<div class="card">
  <div class="card-head">
    <h3>网络代理</h3>
    <span class="muted">可选 · 供「目标」页勾选后使用</span>
  </div>

  {#if loading}
    <p class="muted">读取中…</p>
  {:else}
    <label class="switch-row">
      <input type="checkbox" bind:checked={enabled} disabled={busy || saving} />
      <span>启用网络代理</span>
    </label>
    <p class="field-hint">
      关闭时所有目标直连（即使某个目标勾了「使用代理」）。配错代理会让全部备份失败，
      这里是紧急止血开关。
    </p>

    <div class="field">
      <label for="proxy-url">代理地址</label>
      <input
        id="proxy-url"
        type="text"
        bind:value={url}
        disabled={busy || saving}
        placeholder="http://192.168.1.2:7890 或 socks5h://127.0.0.1:1080"
      />
      <p class="field-hint">
        支持 <code>http</code> / <code>https</code> / <code>socks5</code> /
        <code>socks5h</code>。跨境备份建议用 <code>socks5h</code>：域名交给代理解析，
        避免本地 DNS 污染导致连接被墙。
      </p>
    </div>

    <div class="field-row">
      <div class="field">
        <label for="proxy-user">认证用户名</label>
        <input
          id="proxy-user"
          type="text"
          bind:value={username}
          disabled={busy || saving}
          placeholder={usernameSet ? '已设置（留空则不修改）' : '可选'}
        />
      </div>
      <div class="field">
        <label for="proxy-pass">认证密码</label>
        <input
          id="proxy-pass"
          type="password"
          bind:value={password}
          disabled={busy || saving}
          placeholder={usernameSet ? '已设置（留空则不修改）' : '可选'}
        />
      </div>
    </div>
    {#if usernameSet}
      <p class="field-hint">
        已保存代理凭据（明文不回传）。留空保存不会改动它。
        <button class="link-btn" on:click={clearCredentials} disabled={busy || saving}>
          清除凭据
        </button>
      </p>
    {/if}

    <div class="field">
      <label for="no-proxy">不走代理的地址</label>
      <input
        id="no-proxy"
        type="text"
        bind:value={noProxy}
        disabled={busy || saving}
        placeholder="localhost,127.0.0.1,*.local"
      />
      <p class="field-hint">
        逗号分隔，只比对主机名。命中的目标**必定直连**，即使它勾了「使用代理」
        —— 否则局域网 WebDAV 的私有地址会被发到公网出口，必然连不上。
      </p>
    </div>

    <div class="row-actions">
      <button class="btn btn-primary" on:click={save} disabled={busy || saving || testing}>
        {saving ? '保存中…' : '保存代理设置'}
      </button>
      <button class="btn" on:click={testProxy} disabled={busy || saving || testing || !url.trim()}>
        {testing ? '测试中…' : '测试代理'}
      </button>
      {#if usable}
        <span class="badge badge-ok">当前生效</span>
      {:else if enabled && url}
        <span class="badge badge-warn">地址不完整</span>
      {/if}
    </div>

    {#if msg}
      <p class="msg {msgOk ? 'ok' : 'err'}">
        <Icon name={msgOk ? 'check' : 'alert'} size={14} />
        {msg}
      </p>
    {/if}

    {#if testMsg}
      <div class="test-result {testOk ? 'ok' : 'err'}">
        <p class="msg {testOk ? 'ok' : 'err'}">
          <Icon name={testOk ? 'check' : 'alert'} size={14} />
          {testMsg}
        </p>
        {#if testDetail}
          <p class="test-detail">{testDetail}</p>
        {/if}
      </div>
    {/if}
  {/if}
</div>

<style>
  .card { border: 1px solid var(--border); border-radius: 10px; padding: 14px 16px; margin-bottom: 14px; }
  .card-head { display: flex; align-items: baseline; gap: 8px; margin-bottom: 10px; }
  .card-head h3 { margin: 0; font-size: 15px; }
  .muted { color: var(--text-dim); font-size: 12px; }
  .switch-row { display: flex; align-items: center; gap: 8px; }
  .field { margin-top: 12px; }
  .field label { display: block; font-size: 13px; margin-bottom: 4px; }
  .field input { width: 100%; }
  .field-row { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
  .field-hint { font-size: 12px; color: var(--text-dim); margin: 4px 0 0; line-height: 1.6; }
  .field-hint code { font-size: 11px; padding: 1px 4px; border-radius: 3px; background: var(--bg-soft, rgba(127,127,127,.12)); }
  .row-actions { display: flex; align-items: center; gap: 10px; margin-top: 14px; }
  .badge { font-size: 11px; padding: 2px 7px; border-radius: 999px; }
  .badge-ok { background: rgba(60,180,110,.16); color: #2f9e63; }
  .badge-warn { background: rgba(230,160,60,.18); color: #b8791f; }
  .msg { display: flex; align-items: center; gap: 6px; font-size: 12.5px; margin: 10px 0 0; }
  .test-result {
    margin-top: 8px;
    padding: 8px 10px;
    border-radius: 8px;
    background: rgba(127, 127, 127, 0.07);
  }
  .test-result .msg { margin: 0; }
  .test-detail {
    margin: 6px 0 0;
    font-size: 12px;
    color: var(--text-dim);
    line-height: 1.6;
  }
  .msg.ok { color: #2f9e63; }
  .msg.err { color: #d05a5a; }
  .link-btn { background: none; border: none; color: var(--accent, #4a8df8); cursor: pointer; font-size: 12px; padding: 0 2px; }
</style>
