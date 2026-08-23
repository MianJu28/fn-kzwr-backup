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
 * 成功后返回选中的目录路径数组；失败/不支持时返回 null 并给出提示。
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

  // 宿主环境：直接调用系统目录选择器（directory:true = 选目录，返回已授权路径）
  const result = await sdk.pickUserFile({
    directory: true,
    title: '选择要备份的文件夹',
    okText: '确认',
    sidebarGroup: ['myFiles', 'otherShare', 'favorites'],
  });

  if (result && Array.isArray(result.data) && result.data.length > 0) {
    return result.data;
  }
  // 用户取消或未选择（code 非 0 或 data 为空）
  return null;
}
