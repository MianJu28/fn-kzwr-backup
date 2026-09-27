<script>
  /**
   * 通用「多账号」区块渲染器（`UiBlock::Accounts`）
   *
   * **不含任何插件专属逻辑**：路径、字段名、标题、取值范围全部来自区块声明
   * （见后端 `plugin/api.rs` 的 `Accounts` 数据契约），因此任何「一个插件管多份
   * 凭据」的插件都能直接用，新增插件不必改前端。
   *
   * 契约：
   * - `GET  {list}`            → `{"accounts":[{id,name,configured,meta}]}`
   * - `POST {add}`             → `{name, <credential_field>}`
   * - `POST {update}`          → `{id, name, <credential_field>}`（凭据留空 = 不修改）
   * - `POST {remove}`          → `{id}`
   * - `POST {edit_action}`     → `{id, <edit_field>: number}`（可选：每项的数值设置）
   *
   * 安全：列表**永不回传凭据明文**，输入框只提示「已设置（留空则保持不变）」。
   */
  import Icon from './Icon.svelte';
  import { api } from '../lib/api.js';
  import { toast } from '../lib/toast.js';
  import { confirmDialog } from '../lib/confirm.js';

  export let plugin = null;
  /** 本区块的声明（type='accounts'） */
  export let block = null;
  /** 外层正在执行其它动作时禁用交互 */
  export let busy = false;
  /** 变更后的回调（让页面刷新指标/告警） */
  export let onDone = null;

  let accounts = [];
  let loading = false;
  let submitting = false;
  let error = '';
  /** 新增表单 */
  let adding = false;
  let newName = '';
  let newCred = '';
  /** 正在编辑的账号 id（改名 / 换凭据） */
  let editingId = '';
  let editName = '';
  let editCred = '';
  /** 展开数值设置（如按账号阈值）的账号 id */
  let tuneId = '';
  let tuneValue = 0;

  const path = (p) => (p && p.startsWith('/') ? p : `/${p || ''}`);
  const credField = () => block.credential_field || 'token';
  const credLabel = () => block.credential_label || '凭据';
  const editField = () => block.edit_field || 'percent';
  const label = () => block.label || '已配置账号';

  async function load(fresh = false) {
    if (!plugin || !block || !block.list) return;
    loading = true;
    error = '';
    try {
      const q = fresh ? (block.list.includes('?') ? '&' : '?') + 'fresh=1' : '';
      const r = await api.pluginGet(plugin.api_base, path(block.list) + q);
      if (r && r.error) {
        error = String(r.error);
      } else {
        accounts = (r && r.accounts) || [];
        if (fresh) toast.success('已刷新实时状态');
      }
    } catch (e) {
      error = e.message || '读取账号列表失败';
    } finally {
      loading = false;
    }
  }

  /** 统一提交：成功则刷新列表并通知外层 */
  async function submit(action, body, okText) {
    if (!plugin || !action) return false;
    submitting = true;
    error = '';
    try {
      const r = await api.pluginPost(plugin.api_base, path(action), body);
      if (!r || r.error || r.success === false) {
        const msg = String((r && (r.error || r.message)) || '操作失败');
        error = msg;
        toast.error(msg);
        return false;
      }
      const msg = String(r.message || okText || '已完成');
      error = '';
      toast.success(msg);
      await load(false);
      if (onDone) await onDone();
      return true;
    } catch (e) {
      error = e.message || '操作失败';
      toast.error(error);
      return false;
    } finally {
      submitting = false;
    }
  }

  function startAdd() {
    adding = true;
    editingId = '';
    tuneId = '';
    newName = '';
    newCred = '';
  }

  async function doAdd() {
    if (!newCred.trim()) {
      toast.error(`请填写${credLabel()}`);
      return;
    }
    const ok = await submit(block.add, { name: newName.trim(), [credField()]: newCred.trim() }, '已添加');
    if (ok) {
      adding = false;
      newName = '';
      newCred = '';
    }
  }

  function startEdit(a) {
    tuneId = '';
    adding = false;
    if (editingId === a.id) {
      editingId = '';
      return;
    }
    editingId = a.id;
    editName = a.name || '';
    // 凭据明文前端永远拿不到 → 留空表示保持不变
    editCred = '';
  }

  async function doEdit(a) {
    const body = { id: a.id, name: editName.trim() };
    if (editCred.trim()) body[credField()] = editCred.trim();
    const ok = await submit(block.update, body, '已保存');
    if (ok) {
      editingId = '';
      editCred = '';
    }
  }

  async function doRemove(a) {
    const yes = await confirmDialog({
      title: '删除账号',
      message: `确定删除「${a.name || a.id}」？该账号的${credLabel()}与单独设置将一并移除，且不可恢复。`,
      confirmText: '删除',
      danger: true,
    });
    if (!yes) return;
    if (editingId === a.id) editingId = '';
    await submit(block.remove, { id: a.id }, '已删除');
  }

  function startTune(a) {
    adding = false;
    editingId = '';
    if (tuneId === a.id) {
      tuneId = '';
      return;
    }
    tuneId = a.id;
    // 已有值优先（meta 里的实时值），否则用列表返回的默认值
    const cur = a.meta && a.meta[block.edit_field || 'percent'];
    tuneValue = Number(cur !== undefined && cur !== null ? cur : a.meta?.threshold ?? block.edit_min ?? 0);
  }

  async function doTune(a) {
    const min = block.edit_min ?? 0;
    const max = block.edit_max ?? 100;
    const n = Number(tuneValue);
    if (!Number.isFinite(n) || n < min || n > max) {
      toast.error(`${block.edit_label || '设置'}需为 ${min}-${max} 之间的数值`);
      return;
    }
    const ok = await submit(block.edit_action, { id: a.id, [editField()]: n }, '已保存');
    if (ok) tuneId = '';
  }

  function metaText(a) {
    const m = a.meta || {};
    if (m.state) return String(m.state);
    if (m.space) return `${m.space}${m.percent !== undefined ? ` · 已用 ${m.percent}%` : ''}`;
    return '';
  }

  function metaWarn(a) {
    const m = a.meta || {};
    return !!m.state || (m.percent !== undefined && m.threshold > 0 && m.percent >= m.threshold);
  }

  // 首次挂载 / 切换插件或区块时拉取列表（不带 fresh：离线也要能管理账号）
  let initedKey = null;
  $: key = `${plugin?.id || ''}|${block?.list || ''}`;
  $: if (key && key !== initedKey) {
    initedKey = key;
    accounts = [];
    adding = false;
    editingId = '';
    tuneId = '';
    error = '';
    load(false);
  }

  const disabled = () => loading || submitting || busy;
</script>

<div class="accounts">
  <div class="acc-head">
    <span class="acc-title">{label()}</span>
    <span class="acc-count">{accounts.length}</span>
    <div class="grow"></div>
    <button class="btn btn-sm" on:click={() => load(true)} disabled={disabled()}>
      <Icon name="refresh" size={13} />实时刷新
    </button>
    {#if block.multiple !== false}
      <button class="btn btn-sm btn-primary" on:click={startAdd} disabled={disabled() || adding}>
        <Icon name="plus" size={13} />添加
      </button>
    {/if}
  </div>

  {#if loading && accounts.length === 0}
    <p class="acc-hint">读取中…</p>
  {/if}
  {#if !loading && accounts.length === 0}
    <p class="acc-hint">
      还没有{label()}。点击「添加」填入{credLabel()}
      {#if block.multiple !== false}（可添加多个）{/if}。
    </p>
  {/if}

  {#each accounts as a (a.id)}
    <div class="acc-row">
      <div class="acc-main">
        <div class="acc-name">
          {a.name || a.id}
          {#if a.configured === false}<span class="acc-tag">未设置</span>{/if}
        </div>
        {#if metaText(a)}
          <div class="acc-sub" class:warn={metaWarn(a)}>{metaText(a)}</div>
        {/if}
        {#if a.meta && a.meta.plan}
          <div class="acc-sub">{a.meta.plan}</div>
        {/if}
      </div>
      <div class="acc-ops">
        <button class="btn btn-sm" on:click={() => startEdit(a)} disabled={disabled()}>编辑</button>
        {#if block.edit_action}
          <button class="btn btn-sm" on:click={() => startTune(a)} disabled={disabled()}>
            {block.edit_label || '设置'}
          </button>
        {/if}
        <button class="btn btn-sm btn-danger" on:click={() => doRemove(a)} disabled={disabled()}>
          删除
        </button>
      </div>
    </div>

    {#if editingId === a.id}
      <div class="acc-panel">
        <div class="field">
          <label for="acc-name-{a.id}">名称</label>
          <div class="field-row">
            <input
              id="acc-name-{a.id}"
              type="text"
              placeholder="便于识别的名字，如「工作盘」"
              value={editName}
              on:input={(e) => (editName = e.target.value)}
              disabled={disabled()}
            />
          </div>
        </div>
        <div class="field">
          <label for="acc-cred-{a.id}">{credLabel()}</label>
          <div class="field-row">
            <input
              id="acc-cred-{a.id}"
              type="password"
              placeholder="留空则保持原值不变；输入新值可覆盖"
              value={editCred}
              on:input={(e) => (editCred = e.target.value)}
              disabled={disabled()}
            />
            <button class="btn btn-primary" on:click={() => doEdit(a)} disabled={disabled()}>保存</button>
            <button class="btn" on:click={() => (editingId = '')} disabled={disabled()}>取消</button>
          </div>
        </div>
      </div>
    {/if}

    {#if block.edit_action && tuneId === a.id}
      <div class="acc-panel">
        <div class="field">
          <label for="acc-tune-{a.id}">
            {block.edit_label || '设置'}{#if block.edit_suffix}（{block.edit_suffix}）{/if}
          </label>
          <div class="field-row">
            <input
              id="acc-tune-{a.id}"
              type="number"
              min={block.edit_min ?? 0}
              max={block.edit_max ?? 100}
              step="1"
              value={tuneValue}
              on:input={(e) => (tuneValue = e.target.value)}
              disabled={disabled()}
            />
            <button class="btn btn-primary" on:click={() => doTune(a)} disabled={disabled()}>保存</button>
            <button class="btn" on:click={() => (tuneId = '')} disabled={disabled()}>取消</button>
          </div>
          {#if block.edit_hint}
            <p class="acc-hint">{block.edit_hint}</p>
          {:else}
            <p class="acc-hint">
              达到该数值即报警；{block.edit_min ?? 0} 表示关闭。
            </p>
          {/if}
        </div>
      </div>
    {/if}
  {/each}

  {#if adding}
    <div class="acc-panel">
      <div class="field">
        <label for="acc-new-name">名称</label>
        <div class="field-row">
          <input
            id="acc-new-name"
            type="text"
            placeholder="便于识别的名字，如「工作盘」（留空自动起名）"
            value={newName}
            on:input={(e) => (newName = e.target.value)}
            disabled={disabled()}
          />
        </div>
      </div>
      <div class="field">
        <label for="acc-new-cred">{credLabel()}</label>
        <div class="field-row">
          <input
            id="acc-new-cred"
            type="password"
            placeholder={block.credential_placeholder || ''}
            value={newCred}
            on:input={(e) => (newCred = e.target.value)}
            disabled={disabled()}
          />
          <button class="btn btn-primary" on:click={doAdd} disabled={disabled()}>保存</button>
          <button class="btn" on:click={() => (adding = false)} disabled={disabled()}>取消</button>
        </div>
        <p class="acc-hint">保存前会向服务商校验一次，无效不会写入配置。</p>
      </div>
    </div>
  {/if}

  {#if error}
    <p class="acc-error">{error}</p>
  {/if}
</div>

<style>
  .accounts {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .acc-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .acc-title {
    font-weight: 600;
    font-size: 13px;
  }
  .acc-count {
    font-size: 11px;
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--surface-2, rgba(127, 127, 127, 0.15));
    color: var(--text-dim, inherit);
  }
  .grow {
    flex: 1;
  }
  .acc-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border: 1px solid var(--border, rgba(127, 127, 127, 0.22));
    border-radius: 10px;
  }
  .acc-main {
    flex: 1;
    min-width: 0;
  }
  .acc-name {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .acc-sub {
    font-size: 12px;
    color: var(--text-dim, inherit);
  }
  .acc-sub.warn {
    color: var(--warn, #b8860b);
  }
  .acc-tag {
    font-size: 11px;
    padding: 0 5px;
    border-radius: 4px;
    background: var(--surface-2, rgba(127, 127, 127, 0.15));
    color: var(--text-dim, inherit);
  }
  .acc-ops {
    display: flex;
    gap: 6px;
    flex-shrink: 0;
  }
  .acc-panel {
    padding: 4px 10px 8px;
    border: 1px dashed var(--border, rgba(127, 127, 127, 0.28));
    border-radius: 10px;
  }
  .acc-hint {
    margin: 4px 0 0;
    font-size: 12px;
    color: var(--text-dim, inherit);
  }
  .acc-error {
    margin: 0;
    font-size: 12px;
    color: var(--danger, #c0392b);
  }
  .btn-sm {
    padding: 3px 9px;
    font-size: 12px;
  }
</style>
