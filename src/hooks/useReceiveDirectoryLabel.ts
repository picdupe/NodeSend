import { useEffect, useState } from 'react';
import { invoke } from '../api/runtime';
import { useTranslation } from 'react-i18next';

function displayReceiveDirectory(path: string) {
  const uncPrefix = '\\\\?\\UNC\\';
  const extendedPrefix = '\\\\?\\';
  if (path.slice(0, uncPrefix.length).toUpperCase() === uncPrefix.toUpperCase()) {
    return `\\\\${path.slice(uncPrefix.length)}`;
  }
  return path.startsWith(extendedPrefix) ? path.slice(extendedPrefix.length) : path;
}

export function useReceiveDirectoryLabel(path: string) {
  const { t } = useTranslation('common');
  const android = /Android/i.test(navigator.userAgent);
  const displayPath = displayReceiveDirectory(path);
  const [label, setLabel] = useState(android ? t('receiveDirectory.reading') : displayPath);
  useEffect(() => {
    if (!android) { setLabel(displayPath); return; }
    let disposed = false;
    setLabel(t('receiveDirectory.reading'));
    void invoke<string>('describe_receive_directory', { root: path }).then(value => {
      if (!disposed) setLabel(value);
    }).catch(() => { if (!disposed) setLabel(t('receiveDirectory.unavailable')); });
    return () => { disposed = true; };
  }, [path, displayPath, android, t]);
  return android ? label : displayPath;
}

export function ReceiveDirectoryLabel({ path }: { path: string }) {
  return useReceiveDirectoryLabel(path);
}
