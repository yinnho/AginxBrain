import { check, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';

export interface UpdateInfo {
  version: string;
  date: string;
  body: string;
  _update: Update;
}

export async function checkForUpdate(): Promise<UpdateInfo | null> {
  // Guard: Tauri APIs only available in production (desktop app)
  if (!(window as any).__TAURI__) return null;
  try {
    const update = await check();
    if (!update) return null;
    return {
      version: update.version,
      date: update.date ?? '',
      body: update.body ?? '',
      _update: update,
    };
  } catch {
    return null;
  }
}

export async function installUpdate(updateInfo: UpdateInfo): Promise<void> {
  await updateInfo._update.downloadAndInstall();
  await relaunch();
}
