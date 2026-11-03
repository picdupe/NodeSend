import { useEffect, useState } from 'react';
import { invoke } from '../api/runtime';
import { useTranslation } from 'react-i18next';

export function useReceiveDirectoryLabel(path: string) {
  const { t } = useTranslation('common');
  const android = /Android/i.test(navigator.userAgent);
  const [label, setLabel] = useState(android ? t('receiveDirectory.reading') : path);
  useEffect(() => {
    if (!android) { setLabel(path); return; }
    let disposed = false;
    setLabel(t('receiveDirectory.reading'));
    void invoke<string>('describe_receive_directory', { root: path }).then(value => {
      if (!disposed) setLabel(value);
    }).catch(() => { if (!disposed) setLabel(t('receiveDirectory.unavailable')); });
    return () => { disposed = true; };
  }, [path, android, t]);
  return android ? label : path;
}

export function ReceiveDirectoryLabel({ path }: { path: string }) {
  return useReceiveDirectoryLabel(path);
}
