export function displayVersion(version: string): string {
  const normalized = version.replace(/^v/i, '');
  const match = normalized.match(/^(.+?)(?:-build\.(\d+)|\.(\d+))$/i);
  if (!match) return `v${normalized}`;
  return `v${match[1]} Build ${match[2] ?? match[3]}`;
}

export function displayBuildVersion(version: string): string {
  return displayVersion(version);
}
