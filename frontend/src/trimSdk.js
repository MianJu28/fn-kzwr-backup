// 飞牛 fnOS JS SDK 封装
// 用于在配置备份路径时调用系统文件/目录选择器（pickUserFile），
// 让用户直接在飞牛文件管理器里选择要备份的文件夹，拿到真实路径。
//
// 需要：
//   - manifest 声明 micro_app=true
//   - config/resource 声明 api-scope: ["trim.file.userAccess"]

import { TrimApp } from '@trimjs/web-app';

let _sdk = null;

/**
 * 获取飞牛 SDK 实例（懒加载，避免在非宿主环境下初始化报错）。
 * @returns {{isWeb:boolean, isStandaloneWeb:boolean, pickUserFile:Function}|null}
 */
export function getTrimSdk() {
  if (_sdk) return _sdk;
  try {
    _sdk = new TrimApp();
  } catch (e) {
    // 非微应用环境（如本地开发、独立浏览器），返回 null，调用方降级为手动输入
    _sdk = null;
  }
  return _sdk;
}

/** 当前是否运行在飞牛宿主环境中 */
export function isInTrimHost() {
  const sdk = getTrimSdk();
  return !!sdk && sdk.isStandaloneWeb === false;
}

/**
 * 调用飞牛系统目录选择器，选择要备份的文件夹。
 * 成功后返回选中的目录路径数组；用户取消返回 null；返回了无法识别的
 * 结果时抛错（附结果摘要，便于定位宿主返回形态变化）。
 * @returns {Promise<string[]|null>}
 */
export async function pickBackupFolder() {
  const sdk = getTrimSdk();
  if (!sdk) {
    throw new Error('当前环境不支持飞牛目录选择，请手动输入路径');
  }

  if (sdk.isStandaloneWeb) {
    // 独立浏览器环境：需通过授权页回调（当前前端为 SPA，未实现回调页）。
    // 提示用户改用宿主环境（应用中心内打开）或手动输入。
    throw new Error('请在飞牛应用内使用目录选择；独立浏览器环境请手动输入路径');
  }

  // 桥接初始化可能尚未完成：不等待 ready 就调用，宿主的响应会丢失
  //（Promise 以 undefined 结束），表现为「点了确定却提示已取消」。
  if (typeof sdk.ready === 'function') {
    try {
      await sdk.ready();
    } catch {
      /* 旧版本无 ready：忽略，继续尝试 */
    }
  }

  const result = await sdk.pickUserFile({
    directory: true,
    multiple: true,
    title: '选择要备份的文件夹',
    okText: '确认',
    sidebarGroup: ['myFiles', 'otherShare', 'favorites'],
  });

  const paths = normalizePickResult(result);
  if (paths && paths.length > 0) {
    return paths;
  }
  if (result === undefined || result === null) {
    // 宿主未回传结果（多为用户取消）
    return null;
  }
  // 有响应但形态无法识别：抛出摘要，便于跟进宿主返回结构的变化
  throw new Error(
    '目录选择器返回了无法识别的结果：' + JSON.stringify(result).slice(0, 200)
  );
}

/**
 * 归一化选择器结果为路径数组。
 * 兼容形态：string[] / { data: string[] } / { data: [{path}] } /
 * { paths: string[] } / 纯字符串。
 */
function normalizePickResult(result) {
  if (!result) return null;
  if (Array.isArray(result)) return asPathList(result);
  if (Array.isArray(result.data)) return asPathList(result.data);
  if (typeof result.data === 'string') return [result.data];
  if (Array.isArray(result.paths)) return asPathList(result.paths);
  if (typeof result === 'string') return [result];
  return null;
}

/** 把（元素可能为字符串或对象的）数组整理成路径字符串数组 */
function asPathList(arr) {
  const out = [];
  for (const item of arr) {
    if (typeof item === 'string' && item.trim()) {
      out.push(item.trim());
    } else if (item && typeof item === 'object') {
      const p = item.path || item.filePath || item.fullPath || item.srcPath;
      if (typeof p === 'string' && p.trim()) out.push(p.trim());
    }
  }
  return out;
}
