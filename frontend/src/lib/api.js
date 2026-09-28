/**
 * 后端 API 客户端（所有 HTTP 调用集中于此，组件不直接拼 URL）
 *
 * 约定：后端统一返回 200 + JSON（错误放在 `error` 字段），
 * 因此这里只做网络层异常抛出与 JSON 解析，业务错误由调用方判断。
 *
 * 路径统一加网关前缀（APP_BASE）：经飞牛统一网关访问时，应用在
 * `/app/<appname>` 下，裸 `/api` 会落到宿主服务上。
 */
import { APP_BASE } from './appBase.js';

async function req(path, options = {}) {
  const res = await fetch(APP_BASE + path, options);
  let data;
  try {
    data = await res.json();
  } catch (e) {
    throw new Error(`响应解析失败（HTTP ${res.status}）`);
  }
  if (!res.ok && data && data.error === undefined) {
    throw new Error(`请求失败（HTTP ${res.status}）`);
  }
  return data;
}

const get = (path) => req(path);

const post = (path, body) =>
  req(path, {
    method: 'POST',
    headers: body === undefined ? undefined : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });

export const api = {
  // 健康检查
  health: () => get('/api/health'),

  // 配置（核心项：调试日志开关；备份路径/定时/保留策略按任务管理，见 tasks()）
  config: () => get('/api/config'),
  saveConfig: (cfg) => post('/api/config', cfg),

  // age 密钥
  keys: () => get('/api/keys'),
  setKey: (private_key) => post('/api/keys', { private_key }),
  generateKey: () => post('/api/keys/generate'),
  exportKey: (passphrase) => post('/api/keys/export', { passphrase }),
  backupAck: () => post('/api/keys/backup-ack'),

  // 告警与通知
  alerts: () => get('/api/alerts'),
  clearAlerts: () => req('/api/alerts', { method: 'DELETE' }),
  saveWebhook: (webhook_url, headers, body_template) =>
    post('/api/notify/webhook', { webhook_url, headers, body_template }),
  testWebhook: (webhook_url, headers, body_template) =>
    post('/api/notify/webhook/test', { webhook_url, headers, body_template }),

  // 配置导入 / 导出（需管理员口令）
  exportConfig: (passphrase) => post('/api/config/export', { passphrase }),
  importConfig: (passphrase, config) => post('/api/config/import', { passphrase, config }),

  // 恢复（备份按任务触发：runTask(id)，见下）
  /** 备份文件夹概况（文件数/文件夹数/总大小） */
  restoreFiles: () => get('/api/restore/files'),
  /** 按目录懒加载：只取一层子项（目录附递归统计）；task 指定任务（多任务下同一路径可能属于多个任务） */
  restoreTree: (source, dir = '', task = '') =>
    get(
      `/api/restore/tree?source=${encodeURIComponent(source)}&dir=${encodeURIComponent(dir)}` +
        (task ? `&task=${encodeURIComponent(task)}` : '')
    ),
  /** 恢复：all=true 时恢复该源路径（可用 dir 限定子目录）下的全部文件 */
  restore: (files, source_path, all = false, dir = '', task = '') =>
    post('/api/restore/run', { files, source_path, all, dir, ...(task ? { task } : {}) }),
  /** 清理快照中云端已不存在的文件记录（只动快照，不删云端文件） */
  pruneMissing: (source_path, task = '') =>
    post('/api/restore/prune', { source_path, ...(task ? { task } : {}) }),

  // ── 多目标 / 多任务（ADR-014）──────────────────────────────────
  /** 目标列表（凭据不回传，密码只回传「是否已设置」） */
  targets: () => get('/api/targets'),
  /** 新建/更新目标（password 缺省 = 不修改凭据；test 默认 true 先实测连通性） */
  saveTarget: (body) => post('/api/targets', body),
  deleteTarget: (id) => post(`/api/targets/${encodeURIComponent(id)}/delete`, {}),
  testTarget: (id) => post(`/api/targets/${encodeURIComponent(id)}/test`, {}),
  /** 任务列表（含目标名、就绪状态、下次触发时间与可选目标） */
  tasks: () => get('/api/tasks'),
  /** 新建/更新任务（未传字段保持原值） */
  saveTask: (body) => post('/api/tasks', body),
  /** 删除任务（purge=true 同时清理它的快照记录） */
  deleteTask: (id, purge = false) =>
    post(`/api/tasks/${encodeURIComponent(id)}/delete`, { purge }),
  /** 立即执行某个任务 */
  runTask: (id) => post(`/api/tasks/${encodeURIComponent(id)}/run`, {}),

  // ── 插件（插件自带路由统一挂在 /api/p/<插件id> 下）──────────────
  /** 插件清单（id/名称/类别/可用性/UI 区块描述）—— 前端区块由它驱动 */
  plugins: () => get('/api/plugins'),
  /** 通用插件调用：base 为插件 api_base（如 /api/p/kzwr），path 为插件内路径 */
  pluginGet: (base, path) => get(`${base}${path}`),
  pluginPost: (base, path, body) => post(`${base}${path}`, body || {}),
  /**
   * 设置**某个目标**的上传并发路数（并发回传，**按目标**配置）
   *
   * 同一个插件（如 webdav）会被多个目标实例化（多账号各一套凭据），
   * 并发度属于「这个目标用几条连接」，故按目标 id 配置 —— 互不影响。
   */
  targetParallel: (id, parallel) => post(`/api/targets/${id}/parallel`, { parallel }),
  /**
   * 设置某个**目标插件**的上传并发路数（并发回传）
   *
   * ⚠️ 旧接口，仅作兼容保留：插件级一份会让同类型目标改一个全变。
   * 新代码请用 `targetParallel`（按目标）。
   */
  pluginParallel: (id, parallel) => post(`/api/plugins/${id}/parallel`, { parallel }),
  /**
   * 启用/禁用某个插件（运行时生效，无需重启）
   *
   * 禁用是**逻辑摘除**（不卸载 `.so`）。若该插件仍被任务引用，后端会**级联停用**
   * 那些任务并在 `affected_tasks` 里回报；若正在执行备份的任务用它，则**拒绝**
   * 本次操作（返回 `error` + `running_task`），避免打断备份。
   */
  pluginSetEnabled: (id, enabled) =>
    post(`/api/plugins/${encodeURIComponent(id)}/enable`, { enabled }),
  /**
   * 卸载清除该插件的宿主代管数据（ADR-013 决策 2）
   *
   * 仍被目标/任务引用时后端**拒绝**，返回 `referenced_by`（引用项名称列表）。
   */
  pluginPurge: (id) => post(`/api/plugins/${encodeURIComponent(id)}/purge`, {}),
  /**
   * 安装外置插件（上传 .so + .so.sig，后端**先验签再落盘**）
   *
   * 用 base64 JSON 而不是 multipart：后端未引入 multer，且 `.so` 体积不大。
   * 安装时提供的公钥会与该文件名绑定（「一插件一公钥」，A 的公钥无法验过 B）。
   */
  pluginInstall: (body) => post('/api/plugins/install', body),
  /** 热重加载外置插件（按当前配置重新扫描装载；无需重启应用） */
  pluginReload: () => post('/api/plugins/reload', {}),
  /** 卸载**外置**插件（删 .so/.sig + 解绑公钥；内置/随包插件不可卸载） */
  pluginUninstall: (file) =>
    post(`/api/plugins/${encodeURIComponent(file)}/uninstall`, {}),
  /** 读取该插件的宿主代管配置（`secret` 字段只回传「是否已设置」） */
  pluginData: (id) => get(`/api/plugins/${encodeURIComponent(id)}/data`),
  /** 写入该插件的宿主代管配置（fields 明文，后端加密落盘；remove 为要删除的键） */
  pluginDataSet: (id, fields, remove = []) =>
    post(`/api/plugins/${encodeURIComponent(id)}/data`, { fields, remove }),

  // 注：kzwr 增强（token / 空间阈值 / 回收站）不再有专用方法 ——
  // 它现在是普通插件，界面由后端 `ui.blocks` 驱动，统一走 pluginGet/pluginPost。
  // 需要账号信息时用 `pluginGet('/api/p/kzwr', '/user')`。

  /** 定时任务预览：cron → 未来 5 次触发时间（服务器本地时区） */
  schedulePreview: (cron) => post('/api/schedule/preview', { cron }),
  /** 一键体检：逐项检查配置与连通性，返回可操作建议 */
  setupCheck: () => get('/api/setup/check'),
  /** 操作审计日志（最新在前） */
  auditLog: (limit = 100) => get(`/api/audit?limit=${limit}`),
  /** 清空审计日志（需管理员口令） */
  auditClear: (passphrase) => post('/api/audit/clear', { passphrase }),
  /** 运行日志末尾（默认 800 行） */
  logsTail: (tail = 800) => get(`/api/logs?tail=${tail}`),
  /** 清空运行日志 */
  logsClear: () => post('/api/logs/clear'),
  /** 运行日志下载地址（直接 <a>/window.open） */
  logsDownloadUrl: `${APP_BASE}/api/logs/download`,
};
