import { useEffect, useState } from 'react';
import { invoke } from '../api/runtime';

export function useReceiveDirectoryLabel(path: string) {
  const android = /Android/i.test(navigator.userAgent);
  const [label, setLabel] = useState(android ? '读取保存目录…' : path);
  useEffect(() => {
    if (!android) { setLabel(path); return; }
    let disposed = false;
    setLabel('读取保存目录…');
    void invoke<string>('describe_receive_directory', { root: path }).then(value => {
      if (!disposed) setLabel(value);
    }).catch(() => { if (!disposed) setLabel('目录不可用，请重新选择'); });
    return () => { disposed = true; };
  }, [path, android]);
  return android ? label : path;
}

export function ReceiveDirectoryLabel({ path }: { path: string }) {
  return useReceiveDirectoryLabel(path);
}
