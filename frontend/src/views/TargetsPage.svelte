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

  /**
   * 该目标是否支持并发回传
   *
   * 优先用**目标自己**带回来的 `supports_plan`（后端按目标所用插件现算），
   * 没有则回退到按插件类型查询 —— 兼容后端尚未下发该字段的旧版本。
   */
  function supportsPlan(t) {
    if (t && typeof t.supports_plan === 'boolean') return t.supports_plan;
    const p = pluginOf(t?.kind);
    return !!(p && p.supports_plan);
  }

  /**
   * 该目标当前的并发度（0/1 = 顺序；≥2 = 并发）
   *
   * **按目标取值**：同一个插件（如 webdav）会被多个目标实例化（多账号各一套凭据），
   * 并发度属于「这个目标用几条连接」，故读目标自己的 `parallel`；
   * 未设置时回退到插件级旧值（兼容升级前的配置）。
   */
  function parallelOf(t) {
    if (t && t.parallel != null) return t.parallel;
    const p = pluginOf(t?.kind);
    return (p && p.parallel) || 0;
  }

  /** 保存**该目标**的上传并发路数（并发度按目标配置，互不影响） */
  async function saveParallel(id, n) {
    parSaving = id;
    try {
      const v = Math.max(0, Math.min(8, Math.floor(Number(n) || 0)));
      const r = await api.targetParallel(id, v);
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

  /**
   * 可选的目标类型（**只列目标插件**）
   *
   * `/api/plugins` 里同时有 target 与 enhance 两类，只有 `kind === 'target'`
   * 的插件能作为备份目标。被禁用的插件也排除（选了也装配不出来）。
   */
  $: targetPlugins = (plugins || []).filter(
    (p) => p.kind === 'target' && !p.disabled
  );

  /** 插件显示名（查不到则回退 id 本身，避免表单里出现空白） */
  function pluginName(kind) {
    const p = pluginOf(kind);
    return (p && p.name) || kind;
  }

  /** 某目标类型是否需要凭据（缺省 true，与后端一致） */
  function needsCreds(kind) {
    const p = pluginOf(kind);
    return p ? p.needs_credentials !== false : true;
  }

  /** 地址字段的标签 / 占位 / 说明（插件可自定义，缺省按 WebDAV） */
  function urlLabel(kind) {
    const p = pluginOf(kind);
    return (p && p.url_label) || '地址';
  }
  function urlPlaceholder(kind) {
    const p = pluginOf(kind);
    return (p && p.url_placeholder) || DEFAULT_URL;
  }
  function urlHint(kind) {
    const p = pluginOf(kind);
    return (
      (p && p.url_hint) ||
      `留空则用官方默认地址 ${DEFAULT_URL}`
    );
  }

  /** 新建时的默认类型：优先 webdav（保持老用户习惯），否则第一个目标插件 */
  function defaultKind() {
    const has = targetPlugins.some((p) => p.id === 'webdav');
    return has ? 'webdav' : (targetPlugins[0] && targetPlugins[0].id) || 'webdav';
  }

  /** 按插件声明的语义给出默认地址（不用凭据的插件通常需要绝对路径，不能填 WebDAV 地址） */
  function defaultUrl(kind) {
    return needsCreds(kind) ? DEFAULT_URL : '';
  }

  function startCreate() {
    const kind = defaultKind();
    editing = {
      id: '',
      name: '',
      kind,
      url: defaultUrl(kind),
      username: '',
      password: '',
      enabled: true,
    };
    formMsg = '';
  }

  /** 切换目标类型：地址等字段的语义随之改变，故重置为适合该插件的默认值 */
  function changeKind(kind) {
    if (!editing) return;
    editing = {
      ...editing,
      kind,
      // 换类型后旧地址多半不适用（WebDAV 地址 vs 本地路径），清成该类型的默认
      url: defaultUrl(kind),
      username: '',
      password: '',
    };
    formMsg = '';
  }

  function startEdit(t) {
    editing = {
      id: t.id,
      name: t.name || '',
      kind: t.kind || 'webdav',
      url: t.url || '',
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
    const creds = needsCreds(editing.kind);
    if (!editing.url.trim()) {
      formMsg = `请填写${urlLabel(editing.kind)}`;
      formOk = false;
      return;
    }
    // 只有「需要凭据」的目标才校验账号密码；本地目录这类目标留空是正常的
    if (creds) {
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
    }
    saving = true;
    formMsg =
      creds && editing.password ? '正在实测连通性…' : '正在保存…';
    formOk = true;
    try {
      const body = {
        name: editing.name.trim() || undefined,
        kind: editing.kind || 'webdav',
        url: editing.url.trim(),
        enabled: !!editing.enabled,
        // 不用凭据的目标不提交账号密码（后端也不会因缺凭据而拒绝）
        username: creds ? editing.username.trim() : undefined,
      };
      if (editing.id) body.id = editing.id;
      if (creds && editing.password) body.password = editing.password;
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

      <!-- 本目标的上传并发（能力由插件声明；**值按目标**，各目标互不影响） -->
      {#if supportsPlan(t)}
        <div class="parallel-row">
          <div class="grow">
            <div class="parallel-label">上传并发路数</div>
            <p class="field-hint">
              0 或 1 = 顺序上传；≥2 = 并发回传（最多 8）。**仅作用于本目标**；
              保存后下次备份生效，无需重启。并发会同时占用多条连接。
            </p>
          </div>
          <input
            class="parallel-input"
            type="number"
            min="0"
            max="8"
            value={parallelOf(t)}
            disabled={parSaving === t.id}
            on:change={(e) => saveParallel(t.id, e.target.value)}
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

        <!-- 目标类型：来自**目标插件**清单（enhance 类插件不会出现在这里）。
             编辑既有目标时不允许改类型（改了等于换一种存储，凭据/语义都不同）。 -->
        <div class="field">
          <label for="tg-kind">类型</label>
          {#if editing.id}
            <input id="tg-kind" value={pluginName(editing.kind)} disabled />
            <p class="field-hint">类型创建后不可更改（如需换类型请新建目标）</p>
          {:else}
            <select id="tg-kind" value={editing.kind}
              on:change={(e) => changeKind(e.target.value)} disabled={saving}>
              {#each targetPlugins as p (p.id)}
                <option value={p.id}>{p.name}{p.builtin ? '' : '（外置插件）'}</option>
              {/each}
            </select>
            <p class="field-hint">每种类型对应一个目标插件；选错类型会导致备份无法写入</p>
          {/if}
        </div>

        <div class="field">
          <label for="tg-url">{urlLabel(editing.kind)}</label>
          <input id="tg-url" placeholder={urlPlaceholder(editing.kind)} value={editing.url}
            on:input={(e) => (editing = { ...editing, url: e.target.value })} disabled={saving} />
          <p class="field-hint">{urlHint(editing.kind)}</p>
        </div>

        <!-- 账号/密码：只有声明「需要凭据」的插件才显示。
             本地目录这类目标不用凭据，强制要求会让用户根本建不出目标。 -->
        {#if needsCreds(editing.kind)}
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
        {:else}
          <p class="field-hint">
            该目标类型**不需要账号密码**，只需填写上面的{urlLabel(editing.kind)}。
          </p>
        {/if}
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
