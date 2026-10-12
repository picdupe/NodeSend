import type { UpdateChannel, UpdateEndpoints } from '../api/update';
import { invoke, frontendPreview } from '../api/runtime';

/** Read the per-user AppData configuration; source order is the configured order. */
export async function resolveUpdateEndpoints(channel: UpdateChannel): Promise<UpdateEndpoints> {
  if (frontendPreview) throw new Error('更新功能仅在桌面应用中可用。');
  const sources = await invoke<string[]>('get_update_sources', { channel });
  return { sources };
}
