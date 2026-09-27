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
  /**
   * 渲染形态：
   * - `card`（默认）：完整卡片（标题 + 状态徽标 + 内容），用于插件页列表；
   * - `embedded`：只渲染内容，不套卡片外壳 —— 用于弹窗内（弹窗已有自己的标题栏）。
   */
  export let variant = 'card';

  let busy = false;
  let values = {};
  // 上传并发路数（仅当插件声明支持并发回传时显示；每插件独立）
  let parallelValue = plugin?.parallel ?? 0;
  let parBusy = false;
  let parMsg = '';
  let parOk = false;
  /** action -> 结果文案 */
  let results = {};
  /** action -> 是否成功 */
  let oks = {};
  /** field -> 覆盖用的占位符（宿主代管的密钥只提示「已设置」） */
  let placeholders = {};
  /** 插件未加载，宿主无法区分密钥字段 → 所有已存值均只提示「已设置」 */
  let hostRedacted = false;
  /** 动态指标：`metric.action` → { value, hint }（渲染时 GET 该路径取实时值） */
  let metrics = {};

  $: blocks = (plugin && plugin.ui && plugin.ui.blocks) || [];

  // 仅在**切换到另一个插件**时重置表单/结果：外层刷新插件清单时 plugin 对象会换新引用，
  // 若每次都重置，操作结果与用户已填内容会立刻被清掉。
  let initedId = null;
  $: if (plugin && plugin.id && plugin.id !== initedId) {
    initedId = plugin.id;
    const next = {};
    for (const b of blocks) {
      if (b.type === 'text' || b.type === 'number' || b.type === 'toggle') {
        next[b.field] = b.value ?? (b.type === 'toggle' ? false : '');
      }
    }
    values = next;
    results = {};
    oks = {};
    placeholders = {};
    hostRedacted = false;
    metrics = {};
    loadHostData();
    loadMetrics();
  }

  /**
   * 载入**动态指标**（`metric.action`）
   *
   * 静态 `value` 只是占位文案；插件可通过 `action` 暴露一个 GET 接口返回实时值
   * （如云端空间用量），避免为了一个数字去写专用前端组件。
   * 读取失败**不覆盖**静态文案（保留插件给的兜底说明），也不弹错打断渲染。
   */
  async function loadMetrics() {
    const dyn = blocks.filter((b) => b.type === 'metric' && b.action);
    if (dyn.length === 0) return;
    const next = {};
    await Promise.all(
      dyn.map(async (b) => {
        const path = b.action.startsWith('/') ? b.action : `/${b.action}`;
        try {
          const r = await api.pluginGet(plugin.api_base, path);
          if (r && !r.error && r.value) {
            next[b.action] = { value: String(r.value), hint: r.hint ? String(r.hint) : '' };
          } else if (r && r.error) {
            next[b.action] = { value: String(r.value || '读取失败'), hint: String(r.error) };
          }
        } catch (e) {
          /* 保留静态文案 */
        }
      })
    );
    metrics = { ...metrics, ...next };
  }

  /**
   * 载入宿主代管配置并回填 `scope: "host"` 的字段
   *
   * `secret: true` 的字段后端只回传「是否已设置」（布尔），此时**不回填输入框**
   * （明文拿不到，也不该拿到），改在占位符上提示「已设置，留空则不修改」。
   */
  async function loadHostData() {
    if (!blocks.some((b) => b.scope === 'host')) return;
    let data = {};
    try {
      const d = await api.pluginData(plugin.id);
      data = (d && d.data) || {};
      // `redacted`：插件未加载时后端无法区分哪些键是密钥，故全部按密钥处理
      if (d && d.redacted) hostRedacted = true;
    } catch (e) {
      return; // 读取失败不阻断渲染（用户仍可重新填写保存）
    }
    const next = { ...values };
    const hints = {};
    for (const b of blocks) {
      if (b.scope !== 'host' || !b.field) continue;
      const v = data[b.field];
      if (v === undefined || v === null) continue;
      if (typeof v === 'boolean') {
        // 密钥：只提示是否已设置（布尔值本身不构成「已填内容」）
        hints[b.field] = v ? '已设置（留空则保持不变，输入新值可覆盖）' : '';
      } else if (b.type === 'toggle') {
        next[b.field] = v === 'true' || v === true;
      } else {
        next[b.field] = String(v);
      }
    }
    values = next;
    placeholders = { ...placeholders, ...hints };
  }

  function mark(action, text, ok) {
    results = { ...results, [action]: text };
    oks = { ...oks, [action]: ok };
  }

  // 注：Svelte 不允许把 `bind:` 绑到 `obj[key]` 这类成员表达式，故手写 input 事件
  function setValue(field, v) {
    values = { ...values, [field]: v };
  }

  async function saveParallel() {
    parBusy = true;
    parMsg = '';
    try {
      const n = Math.max(0, Math.min(8, Math.floor(Number(parallelValue) || 0)));
      const r = await api.pluginParallel(plugin.id, n);
      if (r && r.error) {
        parOk = false;
        parMsg = r.error;
        toast.error(r.error);
      } else {
        parOk = true;
        parMsg = `已保存（并发 ${r?.parallel ?? n}），下次备份生效`;
        toast.success('已保存，下次备份生效');
      }
    } catch (e) {
      parOk = false;
      parMsg = e.message;
    } finally {
      parBusy = false;
    }
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
      // 宿主代存（`scope: "host"`）：纯目标插件没有自己的路由，字段值提交到
      // `/api/plugins/<id>/data`，由宿主加密落盘并注入 `target_json.config`。
      // 空字符串 = 删除该键（便于清除已保存的凭据）。
      if (b.scope === 'host') {
        const fields = {};
        const remove = [];
        for (const [k, v] of Object.entries(body)) {
          const s = typeof v === 'boolean' ? (v ? 'true' : 'false') : String(v ?? '');
          if (s === '') remove.push(k);
          else fields[k] = s;
        }
        const r = await api.pluginDataSet(plugin.id, fields, remove);
        if (r && r.error) {
          mark(b.action, r.error, false);
          toast.error(r.error);
        } else {
          mark(b.action, '已保存（下次备份生效）', true);
          toast.success('已保存');
          if (b.field && b.type !== 'toggle') values = { ...values, [b.field]: '' };
          if (onDone) await onDone();
        }
        return;
      }
      // 动作路径：容错处理（插件写 "hello" 或 "/hello" 都能调用）
      const actionPath = b.action.startsWith('/') ? b.action : `/${b.action}`;
      const r = await api.pluginPost(plugin.api_base, actionPath, body);
      if (r && r.error) {
        mark(b.action, r.error, false);
        toast.error(r.error);
      } else {
        // 插件可以返回 { message } / { detail } 作为操作结果文案，直接展示给用户
        const text = (r && (r.message || r.detail)) || '已完成';
        mark(b.action, String(text), true);
        if (r && r.message) toast.success(String(r.message));
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

<section class="card" class:embedded={variant === 'embedded'}>
  {#if variant === 'card'}
    <div class="card-head">
      <div class="icon-wrap"><Icon name="package" size={18} /></div>
      <div class="grow">
        <h2 class="card-title">{plugin?.ui?.title || plugin?.name || '插件'}</h2>
        <p class="card-desc">
          由插件 <code>{plugin?.id}</code> 提供{plugin?.description ? ` · ${plugin.description}` : ''}
        </p>
      </div>
      <span class="badge {plugin?.disabled ? 'badge-warn' : plugin?.available ? 'badge-ok' : ''}">
        {plugin?.disabled ? '已停用' : plugin?.available ? '已启用' : '未启用'}
      </span>
    </div>
  {/if}

  <div class="card-body">
    {#if hostRedacted}
      <div class="alert alert-warn">
        <Icon name="shield_alert" size={15} />
        <div class="alert-body">
          该插件当前<strong>未加载</strong>，宿主无法判断哪些字段是密钥，
          因此已保存的值一律只显示「是否已设置」，不显示明文。
          输入新值即可覆盖，留空则保持不变。
        </div>
      </div>
    {/if}
    {#each blocks as b, i (i)}
      {#if b.type === 'tips'}
        <div class="alert alert-info">
          <Icon name="info" size={15} />
          <div class="alert-body">{b.text}</div>
        </div>
      {:else if b.type === 'metric'}
        <div class="stat">
          <div class="stat-label">{b.label}</div>
          <div class="stat-value">{metrics[b.action]?.value ?? b.value}</div>
          {#if metrics[b.action]?.hint ?? b.hint}
            <div class="stat-sub">{metrics[b.action]?.hint ?? b.hint}</div>
          {/if}
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
              placeholder={placeholders[b.field] || b.placeholder || ''}
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

    {#if plugin?.supports_plan}
      <div class="field">
        <label for="pb-par-{plugin?.id}">上传并发路数（本插件独立）</label>
        <div class="field-row">
          <input
            id="pb-par-{plugin?.id}"
            type="number"
            min="0"
            max="8"
            value={parallelValue}
            on:input={(e) => (parallelValue = e.target.value)}
            disabled={parBusy}
          />
          <button class="btn" on:click={saveParallel} disabled={parBusy}>保存</button>
        </div>
        <p class="field-hint">
          0 或 1 = 顺序上传；≥2 = 并发回传（该插件声明支持）。保存后下次备份生效，无需重启。
        </p>
        {#if parMsg}
          <p class={parOk ? 'field-hint' : 'field-error'}>{parMsg}</p>
        {/if}
      </div>
    {/if}

    {#if blocks.length === 0}
      <p class="card-desc">该插件暂未声明界面（可在插件清单里补充 <code>ui.blocks</code>）。</p>
    {/if}
  </div>
</section>

<style>
  /* 弹窗内嵌形态：去掉卡片外壳（弹窗自带标题栏与内边距），只保留内容 */
  .card.embedded {
    border: none;
    background: none;
    box-shadow: none;
    border-radius: 0;
  }
  .card.embedded .card-body {
    padding: 0;
  }
</style>
