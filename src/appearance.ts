export type AppearancePreference = 'system' | 'light' | 'dark';

const STORAGE_KEY = 'nodesend.appearance';
let preference: AppearancePreference = 'system';
let systemScheme: MediaQueryList | undefined;

export function readAppearancePreference(): AppearancePreference {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    return stored === 'light' || stored === 'dark' ? stored : 'system';
  } catch {
    return 'system';
  }
}

function applyEffectiveAppearance(): void {
  const dark = preference === 'dark' || (preference === 'system' && !!systemScheme?.matches);
  const root = document.documentElement;
  root.dataset.theme = dark ? 'dark' : 'light';
  root.style.colorScheme = dark ? 'dark' : 'light';
}

export function setAppearancePreference(next: AppearancePreference): void {
  preference = next;
  try {
    localStorage.setItem(STORAGE_KEY, next);
  } catch {
    // The selected appearance still applies for this session when storage is unavailable.
  }
  applyEffectiveAppearance();
}

export function initializeAppearance(): void {
  systemScheme = window.matchMedia('(prefers-color-scheme: dark)');
  preference = readAppearancePreference();
  applyEffectiveAppearance();
  systemScheme.addEventListener('change', applyEffectiveAppearance);
}
