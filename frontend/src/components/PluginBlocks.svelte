<script>
  /**
   * 插件通用渲染器（UI Schema）
   *
   * 当后端返回的 `ui.component` 不是前端内置组件时使用：按 `ui.blocks` 渲染
   * 指标 / 输入 / 按钮 / 开关 / 提示，操作统一 POST 到 `${api_base}${action}`。
   * 目的：**外置插件不需要重新打包前端**也能提供设置界面。
   *
   * 样式复用 app.css 的全局类（card / field / btn / stat / alert），与内置卡片观感一致。
   */
  import Icon from './Icon.svelte';
  import { api } from '../lib/api.js';
  import { toast } from '../lib/toast.js';
  import { confirmDialog } from '../lib/confirm.js';

  /** 插件条目（/api/plugins 的一项） */
  export let plugin = null;
  /** 操作成功后的回调（让外层刷新账号信息/消息提醒等） */
  export let onDone = null;

  let busy = false;
  let values = {};
  /** action -> 结果文案 */
  let results = {};
  /** action -> 是否成功 */
  let oks = {};

  $: blocks = (plugin && plugin.ui && plugin.ui.blocks) || [];

  // 初始化输入值（schema 给了 value 就用它作默认）
  $: if (plugin && plugin.id) {
    const next = {};
    for (const b of blocks) {
      if (b.type === 'text' || b.type === 'number' || b.type === 'toggle') {
        next[b.field] = b.value ?? (b.type === 'toggle' ? false : '');
      }
    }
    values = next;
    results = {};
    oks = {};
  }

  function mark(action, text, ok) {
    results = { ...results, [action]: text };
    oks = { ...oks, [action]: ok };
  }

  // 注：Svelte 不允许把 `bind:` 绑到 `obj[key]` 这类成员表达式，故手写 input 事件
  function setValue(field, v) {
    values = { ...values, [field]: v };
  }

  async function action(b) {
    if (b.type === 'button' && b.confirm) {
      const yes = await confirmDialog({
        title: b.label,
        message: b.confirm,
        confirmText: '继续',
        danger: !!b.danger,
      });
      if (!yes) return;
    }
    busy = true;
    try {
      const body = {};
      if (b.field) {
        const v = values[b.field];
        body[b.field] =
          b.type === 'number' ? Number(v || 0) : b.type === 'toggle' ? !!v : String(v ?? '');
      }
      const r = await api.pluginPost(plugin.api_base, b.action, body);
      if (r && r.error) {
        mark(b.action, r.error, false);
        toast.error(r.error);
      } else {
        mark(b.action, '已完成', true);
        if (b.field && b.type !== 'toggle') values = { ...values, [b.field]: '' };
        if (onDone) await onDone();
      }
    } catch (e) {
      mark(b.action, e.message, false);
      toast.error(e.message);
    } finally {
      busy = false;
    }
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="package" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">{plugin?.ui?.title || plugin?.name || '插件'}</h2>
      <p class="card-desc">
        由插件 <code>{plugin?.id}</code> 提供{plugin?.description ? ` · ${plugin.description}` : ''}
      </p>
    </div>
    <span class="badge {plugin?.available ? 'badge-ok' : ''}">
      {plugin?.available ? '已启用' : '未启用'}
    </span>
  </div>

  <div class="card-body">
    {#each blocks as b, i (i)}
      {#if b.type === 'tips'}
        <div class="alert alert-info">
          <Icon name="info" size={15} />
          <div class="alert-body">{b.text}</div>
        </div>
      {:else if b.type === 'metric'}
        <div class="stat">
          <div class="stat-label">{b.label}</div>
          <div class="stat-value">{b.value}</div>
          {#if b.hint}<div class="stat-sub">{b.hint}</div>{/if}
        </div>
      {:else if b.type === 'text' || b.type === 'number'}
        <div class="field">
          <label for="pb-{plugin?.id}-{b.field}">
            {b.label}{#if b.type === 'number' && b.suffix}（{b.suffix}）{/if}
          </label>
          <div class="field-row">
            <input
              id="pb-{plugin?.id}-{b.field}"
              type={b.type === 'number' ? 'number' : b.secret ? 'password' : 'text'}
              placeholder={b.placeholder || ''}
              value={values[b.field] ?? ''}
              on:input={(e) => setValue(b.field, e.target.value)}
              disabled={busy}
            />
            <button class="btn btn-primary" on:click={() => action(b)} disabled={busy}>
              {b.button || '保存'}
            </button>
          </div>
          {#if results[b.action]}
            <p class={oks[b.action] ? 'field-hint' : 'field-error'}>{results[b.action]}</p>
          {/if}
        </div>
      {:else if b.type === 'toggle'}
        <div class="field">
          <div class="field-row">
            <label for="pb-{plugin?.id}-{b.field}">{b.label}</label>
            <input
              id="pb-{plugin?.id}-{b.field}"
              type="checkbox"
              checked={!!values[b.field]}
              on:change={(e) => setValue(b.field, e.target.checked)}
              disabled={busy}
            />
            <button class="btn" on:click={() => action(b)} disabled={busy}>应用</button>
          </div>
          {#if results[b.action]}
            <p class={oks[b.action] ? 'field-hint' : 'field-error'}>{results[b.action]}</p>
          {/if}
        </div>
      {:else if b.type === 'button'}
        <div class="field">
          <button
            class="btn {b.danger ? 'btn-danger' : ''}"
            on:click={() => action(b)}
            disabled={busy}
          >
            {b.label}
          </button>
          {#if results[b.action]}
            <p class={oks[b.action] ? 'field-hint' : 'field-error'}>{results[b.action]}</p>
          {/if}
        </div>
      {/if}
    {/each}

    {#if blocks.length === 0}
      <p class="card-desc">该插件暂未声明界面（可在插件清单里补充 <code>ui.blocks</code>）。</p>
    {/if}
  </div>
</section>
