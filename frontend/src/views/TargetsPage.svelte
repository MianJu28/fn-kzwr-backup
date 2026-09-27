<script>
  /**
   * 目标管理：远程存储目的地（当前为 WebDAV）
   *
   * 一个目标 = 一个地址 + 一套账号凭据；多个任务可共用同一个目标。
   * 每个目标在云端各自独立（快照按「目标账号」分桶），互不影响。
   *
   * **上传并发**（并发回传）也在这里配置：它是目标类型（插件）的能力，
   * 但开关属于「这个目标用几条连接」，与地址/凭据同属一个目标的属性，
   * 故与凭据放在同一处编辑，而不是散落在插件页。
   */
  import Icon from '../components/Icon.svelte';
  import { api } from '../lib/api.js';
  import { toast } from '../lib/toast.js';
  import { confirmDialog } from '../lib/confirm.js';

  /** 目标列表（由外层载入后传入，加载完会回调 onChanged 让外层刷新） */
  export let targets = [];
  /** 插件清单（查目标类型是否支持并发回传、以及当前并发度） */
  export let plugins = [];
  export let busy = false;
  export let onChanged = null; // () => Promise

  const DEFAULT_URL = 'https://dav.kzwr.com/dav';

  /** 正在编辑的目标（null = 未编辑；id 为空 = 新建） */
  let editing = null;
  let saving = false;
  let testing = null; // 正在测试的目标 id
  let formMsg = '';
  let formOk = true;
  /** 并发度：正在保存的目标 id（与目标保存分开，互不影响） */
  let parSaving = null;

  /** 按目标类型（插件 id）查插件条目 */
  function pluginOf(kind) {
    return (plugins || []).find((p) => p.id === kind) || null;
  }

  /** 该目标类型是否支持并发回传（插件自身能力声明，非用户开关） */
  function supportsPlan(kind) {
    const p = pluginOf(kind);
    return !!(p && p.supports_plan);
  }

  /** 该目标类型当前的并发度（0/1 = 顺序；≥2 = 并发） */
  function parallelOf(kind) {
    const p = pluginOf(kind);
    return (p && p.parallel) || 0;
  }

  /** 保存某目标类型的上传并发路数（并发度按**插件**配置，同类型目标共用） */
  async function saveParallel(kind, n) {
    parSaving = kind;
    try {
      const v = Math.max(0, Math.min(8, Math.floor(Number(n) || 0)));
      const r = await api.pluginParallel(kind, v);
      if (r && r.error) {
        toast.error(r.error);
        return;
      }
      toast.success(v >= 2 ? `已启用并发回传（${v} 路），下次备份生效` : '已改为顺序上传，下次备份生效');
      if (onChanged) await onChanged();
    } catch (e) {
      toast.error(e.message);
    } finally {
      parSaving = null;
    }
  }

  function startCreate() {
    editing = { id: '', name: '', kind: 'webdav', url: DEFAULT_URL, username: '', password: '', enabled: true };
    formMsg = '';
  }

  function startEdit(t) {
    editing = {
      id: t.id,
      name: t.name || '',
      kind: t.kind || 'webdav',
      url: t.url || DEFAULT_URL,
      username: t.username || '',
      password: '',
      enabled: t.enabled !== false,
      password_set: t.password_set,
    };
    formMsg = '';
  }

  function cancelEdit() {
    editing = null;
    formMsg = '';
  }

  async function save() {
    if (!editing) return;
    if (!editing.url.trim()) {
      formMsg = '请填写目标地址';
      formOk = false;
      return;
    }
    if (!editing.username.trim()) {
      formMsg = '请填写账号';
      formOk = false;
      return;
    }
    if (!editing.id && !editing.password) {
      formMsg = '新建目标需要填写应用密码';
      formOk = false;
      return;
    }
    saving = true;
    formMsg = editing.password ? '正在实测连通性…' : '正在保存…';
    formOk = true;
    try {
      const body = {
        name: editing.name.trim() || undefined,
        kind: editing.kind || 'webdav',
        url: editing.url.trim(),
        username: editing.username.trim(),
        enabled: !!editing.enabled,
      };
      if (editing.id) body.id = editing.id;
      if (editing.password) body.password = editing.password;
      const r = await api.saveTarget(body);
      if (r.error) {
        formMsg = r.error;
        formOk = false;
        return;
      }
      toast.success(editing.id ? '目标已更新' : '目标已创建');
      editing = null;
      if (onChanged) await onChanged();
    } catch (e) {
      formMsg = e.message;
      formOk = false;
    } finally {
      saving = false;
    }
  }

  async function test(t) {
    testing = t.id;
    try {
      const r = await api.testTarget(t.id);
      if (r.error) toast.error(r.error, `「${t.name || t.id}」连接失败`);
      else toast.success(`连接成功：${r.url || t.url}`, `「${t.name || t.id}」`);
    } catch (e) {
      toast.error(e.message);
    } finally {
      testing = null;
    }
  }

  async function remove(t) {
    const yes = await confirmDialog({
      title: '删除目标？',
      message: `将删除目标「${t.name || t.id}」。云端已有数据不会被删除，但引用它的任务需先改到其它目标。`,
      confirmText: '删除',
      danger: true,
    });
    if (!yes) return;
    try {
      const r = await api.deleteTarget(t.id);
      if (r.error) {
        toast.error(r.error);
        return;
      }
      toast.success('目标已删除');
      if (onChanged) await onChanged();
    } catch (e) {
      toast.error(e.message);
    }
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="cloud" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">备份目标</h2>
      <p class="card-desc">
        远程存储目的地（当前支持 WebDAV）。一个目标 = 一个地址 + 一套账号凭据；
        <strong>多个任务可共用同一个目标</strong>，每个目标在云端各自独立（快照按目标账号分桶）
      </p>
    </div>
    <span class="badge">{targets.length} 个</span>
  </div>

  <div class="card-body">
    {#if targets.length === 0}
      <div class="empty">
        <Icon name="cloud" size={22} />
        <p>还没有配置任何目标，先添加一个才能创建备份任务</p>
      </div>
    {/if}

    {#each targets as t (t.id)}
      <div class="row-item">
        <div class="grow">
          <div class="row-title">
            {t.name || t.id}
            <span class="badge {t.ready ? 'badge-ok' : 'badge-warn'}">
              {t.ready ? '已就绪' : '未配置凭据'}
            </span>
            {#if !t.enabled}<span class="badge badge-danger">已停用</span>{/if}
          </div>
          <div class="row-sub">
            <code>{t.backend || t.url || '—'}</code>
            {#if t.username}<span class="meta"><b>账号</b>{t.username}</span>{/if}
            <span class="meta"><b>被引用</b>{t.tasks || 0} 个任务</span>
            {#if supportsPlan(t.kind)}
              <span class="meta">
                <b>上传并发</b>{parallelOf(t.kind) >= 2 ? `${parallelOf(t.kind)} 路` : '顺序'}
              </span>
            {/if}
          </div>
        </div>
        <div class="row-actions">
          <button class="btn btn-sm" on:click={() => test(t)} disabled={busy || testing === t.id}>
            {testing === t.id ? '测试中…' : '测试连接'}
          </button>
          <button class="btn btn-sm" on:click={() => startEdit(t)} disabled={busy}>编辑</button>
          <button class="btn btn-sm btn-danger" on:click={() => remove(t)} disabled={busy}>删除</button>
        </div>
      </div>

      <!-- 该目标类型的上传并发（能力由插件声明；值按插件配置，同类型目标共用） -->
      {#if supportsPlan(t.kind)}
        <div class="parallel-row">
          <div class="grow">
            <div class="parallel-label">上传并发路数</div>
            <p class="field-hint">
              0 或 1 = 顺序上传；≥2 = 并发回传（最多 8）。同一类型的目标共用该设置；
              保存后下次备份生效，无需重启。并发会同时占用多条连接。
            </p>
          </div>
          <input
            class="parallel-input"
            type="number"
            min="0"
            max="8"
            value={parallelOf(t.kind)}
            disabled={parSaving === t.kind}
            on:change={(e) => saveParallel(t.kind, e.target.value)}
          />
        </div>
      {/if}
    {/each}

    {#if editing}
      <div class="editor">
        <div class="field">
          <label for="tg-name">名称</label>
          <input id="tg-name" placeholder="如：酷族主账号" value={editing.name}
            on:input={(e) => (editing = { ...editing, name: e.target.value })} disabled={saving} />
        </div>
        <div class="field">
          <label for="tg-url">地址</label>
          <input id="tg-url" placeholder={DEFAULT_URL} value={editing.url}
            on:input={(e) => (editing = { ...editing, url: e.target.value })} disabled={saving} />
          <p class="field-hint">留空则用官方默认地址 {DEFAULT_URL}</p>
        </div>
        <div class="field">
          <label for="tg-user">账号</label>
          <input id="tg-user" placeholder="酷族用户名 / 邮箱" value={editing.username}
            on:input={(e) => (editing = { ...editing, username: e.target.value })} disabled={saving} />
        </div>
        <div class="field">
          <label for="tg-pass">应用密码</label>
          <input id="tg-pass" type="password"
            placeholder={editing.password_set ? '留空 = 不修改已保存的密码' : '在 kzwr 官网「应用密码」创建'}
            value={editing.password}
            on:input={(e) => (editing = { ...editing, password: e.target.value })} disabled={saving} />
          <p class="field-hint">保存前会实测一次连通性；建议选择「永不过期」与读写权限</p>
        </div>
        <label class="switch-row">
          <input type="checkbox" checked={editing.enabled}
            on:change={(e) => (editing = { ...editing, enabled: e.target.checked })} disabled={saving} />
          <span>启用该目标</span>
        </label>

        {#if formMsg}
          <div class="alert {formOk ? 'alert-ok' : 'alert-warn'}">
            <Icon name={formOk ? 'check' : 'alert'} size={15} />
            <div class="alert-body">{formMsg}</div>
          </div>
        {/if}

        <div class="row-actions">
          <button class="btn btn-primary" on:click={save} disabled={saving}>
            {saving ? '保存中…' : editing.id ? '保存修改' : '创建目标'}
          </button>
          <button class="btn btn-ghost" on:click={cancelEdit} disabled={saving}>取消</button>
        </div>
      </div>
    {:else}
      <div class="row-actions">
        <button class="btn btn-primary" on:click={startCreate} disabled={busy}>
          <Icon name="plus" size={14} /> 新建目标
        </button>
      </div>
    {/if}
  </div>
</section>

<style>
  /* 上传并发：紧贴所属目标的卡片下方，视觉上归入该目标 */
  .parallel-row {
    display: flex;
    align-items: center;
    gap: var(--s3);
    margin: calc(-1 * var(--s2)) 0 var(--s3);
    padding: var(--s3) var(--s4);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--r-md);
  }
  .parallel-row .field-hint {
    margin: 2px 0 0;
  }
  .parallel-label {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-2);
  }
  .parallel-input {
    width: 76px;
    flex-shrink: 0;
    text-align: center;
  }
</style>
