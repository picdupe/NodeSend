import { invoke as nativeInvoke } from '@tauri-apps/api/core';

export const frontendPreview = import.meta.env.DEV && !('__TAURI_INTERNALS__' in window);

export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (frontendPreview) {
    const { previewInvoke } = await import('../preview/data');
    return structuredClone(await previewInvoke(command, args ?? {})) as T;
  }
  const result = await nativeInvoke<{ handled: boolean; value?: T }>('service_invoke', { command, args: args ?? {} });
  if (result.handled) return result.value as T;
  return nativeInvoke<T>(command, args);
}
