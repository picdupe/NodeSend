import type { CSSProperties } from 'react';
const paths = {
  send: 'M4 12 20 4l-6 16-3-7-7-1Zm7 1 9-9',
  receive: 'M12 3v12m-5-5 5 5 5-5M4 15v5h16v-5',
  tasks: 'M5 5h14M5 12h14M5 19h9',
  folder: 'M3 7V4h6l3 3h9v13H3V7Z',
  file: 'M6 3h8l4 4v14H6V3Zm8 0v5h4',
  text: 'M5 6h14M5 12h14M5 18h9',
  image: 'm4 19 5-6 4 4 3-4 4 6M5 5h14v14H5V5Zm3 4h.01',
  app: 'M5 4h14v16H5V4Zm4 4h2m2 0h2M9 12h2m2 0h2M9 16h6',
  clipboard: 'M9 4h6v3H9V4Zm-3 2H4v14h16V6h-2M8 12h8m-8 4h5',
  device: 'M3 4h18v12H3V4Zm5 16h8m-4-4v4',
  settings: 'M4 7h16M4 17h16M8 4v6m8 4v6',
  shield: 'M12 2 3 6v6c0 5 9 10 9 10s9-5 9-10V6l-9-4Zm-4 10 3 3 5-6',
  plus: 'M12 5v14M5 12h14',
  close: 'm6 6 12 12M6 18 18 6',
  menu: 'M4 6h16M4 12h16M4 18h16',
  check: 'm5 12 4 4L19 6',
  arrow: 'M4 12h16m-6-6 6 6-6 6',
  refresh: 'M5.11 10.78a7 7 0 0 1 11.84-3.73M18.89 13.22a7 7 0 0 1-11.84 3.73',
  discover: 'M19.5 11a8.5 8.5 0 1 0-8.5 8.5M16.2 11a5.2 5.2 0 1 0-5.2 5.2',
  search: 'M3 10.5a7.5 7.5 0 1 0 15 0 7.5 7.5 0 1 0-15 0M21 21l-4.6-4.6',
  info: 'M12 11v6m0-10h.01M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z',
  heart: 'M20.8 4.6a5.4 5.4 0 0 0-7.6 0L12 5.8l-1.2-1.2a5.4 5.4 0 0 0-7.6 7.6L12 21l8.8-8.8a5.4 5.4 0 0 0 0-7.6Z',
  'chevron-down': 'm6 9 6 6 6-6',
  'chevron-up': 'm6 15 6-6 6 6',
  'chevron-left': 'm15 18-6-6 6-6',
  'chevron-right': 'm9 18 6-6-6-6',
  link: 'm9 15 6-6m-8 2-2 2a4 4 0 0 0 6 6l2-2m-2-10 2-2a4 4 0 0 1 6 6l-2 2',
  pause: 'M8 4v16M16 4v16',
  play: 'm7 4 13 8-13 8V4Z',
};
export type IconName = keyof typeof paths;
export function Icon({ name, size = 20, style }: { name: IconName; size?: number; style?: CSSProperties }) {
  return <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={style}><path d={paths[name]} />{name === 'refresh' && <path d="M18.2 9.3 14.2 7.3 17.95 4.68ZM5.8 14.7 9.8 16.7 6.05 19.32Z" fill="currentColor" strokeWidth=".6" />}{name === 'discover' && <><circle cx="11" cy="11" r="1" fill="currentColor" stroke="none" /><path d="m15 15 6 2.1-2.8 1.1-1.1 2.8Z" fill="currentColor" strokeWidth="1" /></>}</svg>;
}
