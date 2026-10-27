import type { IconName } from '../components/Icon';

/** 本机设备图标：移动端按设备类型显示手机图标，避免始终使用 PC 图标。 */
export function localDeviceIcon(): IconName {
  return /Android|iPhone|iPad|iPod/i.test(navigator.userAgent) ? 'phone' : 'device';
}
