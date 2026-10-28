export function displayVersion(version: string): string {
  const normalized = version.replace(/^v/i, '').replace(/-build\.(\d+)$/i, '.$1');
  return `v${normalized}`;
}

export function displayBuildVersion(version: string): string {
  const match = version.replace(/^v/i, '').match(/^(.+?)(?:-build\.(\d+)|\.(\d+))$/i);
  if (!match) return displayVersion(version);
  return `v${match[1]} Build ${match[2] ?? match[3]}`;
}
