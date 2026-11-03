import type { UpdateChannel, UpdateEndpoints } from '../api/update';
import { invoke, frontendPreview } from '../api/runtime';
import i18next from 'i18next';

export async function resolveUpdateEndpoints(channel: UpdateChannel): Promise<UpdateEndpoints> {
  if (frontendPreview) throw new Error(i18next.t('update.desktopOnly', { ns: 'errors' }));
  const sources = await invoke<string[]>('get_update_sources', { channel });
  return { sources };
}
