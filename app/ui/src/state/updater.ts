import { check, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';

export interface UpdateInfo {
  version: string;
  body?: string;
}

/** 已发现的待安装更新句柄（确认后 downloadAndInstall 复用同一对象） */
let pending: Update | null = null;

/**
 * 启动时静默检查更新（轴一 1.2）：endpoint 为 GitHub Releases 的 latest.json。
 * 离线 / 无 release / 签名校验失败一律返回 null——更新检查永不打扰正常使用。
 */
export async function checkForUpdate(): Promise<UpdateInfo | null> {
  try {
    pending = await check();
  } catch {
    pending = null;
  }
  return pending ? { version: pending.version, body: pending.body ?? undefined } : null;
}

/** 下载安装并重启；失败抛错由调用方 toast（句柄保留，可重试） */
export async function installUpdate(): Promise<void> {
  if (!pending) return;
  await pending.downloadAndInstall();
  await relaunch();
}

/** 放弃本次更新：关闭句柄释放下载资源 */
export async function dismissUpdate(): Promise<void> {
  if (pending) {
    try {
      await pending.close();
    } catch {
      // 句柄已关闭/未下载：忽略
    }
    pending = null;
  }
}
