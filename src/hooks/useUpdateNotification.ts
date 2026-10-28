import { useEffect, useRef, useState } from 'react';
import type { UpdateInfo } from '../api/update';
import { frontendPreview } from '../api/runtime';
import {
  OPEN_UPDATE_PROMPT_EVENT,
  findAvailableUpdate,
  getSkippedUpdateVersion,
  setSkippedUpdateVersion,
} from '../services/updatePrompt';

/** 每次打开窗口自动检查一次更新；跳过仅对当前版本生效。 */
export function useUpdateNotification() {
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const checking = useRef(false);
  const lastCheck = useRef(0);

  useEffect(() => {
    const open = (event: Event) => setUpdate((event as CustomEvent<UpdateInfo>).detail);
    window.addEventListener(OPEN_UPDATE_PROMPT_EVENT, open);
    return () => window.removeEventListener(OPEN_UPDATE_PROMPT_EVENT, open);
  }, []);

  useEffect(() => {
    if (frontendPreview) return;
    async function run() {
      if (checking.current || Date.now() - lastCheck.current < 10000) return;
      lastCheck.current = Date.now();
      checking.current = true;
      try {
        const found = await findAvailableUpdate();
        if (!found || found.version === getSkippedUpdateVersion()) return;
        setUpdate(current => current ?? found);
      } catch {
        // 自动检查失败时保持静默，手动检查仍可给出具体错误。
      } finally {
        checking.current = false;
      }
    }
    void run();
    const onVisibility = () => {
      if (document.visibilityState === 'visible') void run();
    };
    document.addEventListener('visibilitychange', onVisibility);
    return () => document.removeEventListener('visibilitychange', onVisibility);
  }, []);

  return {
    update,
    close: () => setUpdate(null),
    skip: () => {
      if (update) setSkippedUpdateVersion(update.version);
      setUpdate(null);
    },
  };
}
