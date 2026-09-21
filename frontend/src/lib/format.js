/** 通用格式化工具（前端展示层，不含业务逻辑） */

/** 字节可读化：1024 进制，保留 1 位小数 */
export function fmtBytes(n) {
  if (n === null || n === undefined || isNaN(n)) return '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = Number(n);
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** 速率格式化（字节/秒） */
export function fmtBps(bps) {
  if (!bps || bps <= 0) return '—';
  return `${fmtBytes(bps)}/s`;
}

/** 时长格式化：m:ss / h:mm:ss */
export function fmtDuration(ms) {
  if (!ms || ms < 0) return '0s';
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (x) => String(x).padStart(2, '0');
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

/**
 * 宿主（NAS）时区相对 UTC 的分钟偏移，由 App 在拿到 /api/config 后注入。
 *
 * 时间戳在传输层统一是 epoch（毫秒，与时区无关），**展示层统一按宿主时区渲染**，
 * 这样浏览器时区与 NAS 不同也能看到与服务器一致的时间（日志、告警、审计口径一致）。
 */
let hostOffsetMinutes = null;

/** 设置宿主时区偏移（分钟）；传 null/非法值则退回浏览器本地时区 */
export function setHostTimezone(offsetMinutes) {
  hostOffsetMinutes = Number.isFinite(offsetMinutes) ? offsetMinutes : null;
}

/** 宿主时区是否已注入 */
export function hasHostTimezone() {
  return hostOffsetMinutes !== null;
}

/** 把 epoch 毫秒按宿主偏移平移，再按 UTC 输出 = 宿主墙钟时间 */
function shifted(ts) {
  const ms = Number(ts);
  return new Date(hostOffsetMinutes === null ? ms : ms + hostOffsetMinutes * 60000);
}

/** 宿主时区（日期 + 时分秒） */
export function fmtTime(ts) {
  if (!ts) return '';
  try {
    const d = shifted(ts);
    return hostOffsetMinutes === null
      ? d.toLocaleString()
      : d.toLocaleString('zh-CN', { timeZone: 'UTC', hour12: false });
  } catch (e) {
    return '';
  }
}

/** 宿主时区（仅时分秒） */
export function fmtClock(ts) {
  if (!ts) return '';
  try {
    const d = shifted(ts);
    return hostOffsetMinutes === null
      ? d.toLocaleTimeString()
      : d.toLocaleTimeString('zh-CN', { timeZone: 'UTC', hour12: false });
  } catch (e) {
    return '';
  }
}

/** 百分比（0-100 整数） */
export function pctOf(done, total) {
  if (!total) return 0;
  return Math.min(100, Math.round((done / total) * 100));
}

/** 千分位整数 */
export function fmtInt(n) {
  if (n === null || n === undefined || isNaN(n)) return '0';
  return Number(n).toLocaleString();
}
