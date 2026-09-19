/**
 * 轻提示（Toast）全局状态
 *
 * 用法：`toast.success('已保存')` / `toast.error('失败', '备份')`
 * 渲染由 <Toast /> 组件负责（挂在 App 根部）。
 */
import { writable } from 'svelte/store';

export const toasts = writable([]);

let seq = 0;

function push(type, message, title = '', timeout = 4000) {
  const id = ++seq;
  toasts.update((list) => [...list, { id, type, message, title }]);
  if (timeout > 0) {
    setTimeout(() => dismiss(id), timeout);
  }
  return id;
}

export function dismiss(id) {
  toasts.update((list) => list.filter((t) => t.id !== id));
}

export const toast = {
  success: (message, title = '成功', timeout) => push('ok', message, title, timeout),
  error: (message, title = '出错了', timeout = 6000) => push('error', message, title, timeout),
  warn: (message, title = '注意', timeout = 5000) => push('warn', message, title, timeout),
  info: (message, title = '', timeout) => push('info', message, title, timeout),
  push,
};
