import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Icon } from './Icon';

export function SelectMenu<T extends string>({ label, value, options, onChange, disabled = false }: {
  label: string; value: T; options: readonly { value: T; label: string }[]; onChange: (value: T) => void; disabled?: boolean;
}) {
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState({ left: 0, top: 0, width: 160, maxHeight: 240 });
  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      if (!trigger.current) return;
      const rect = trigger.current.getBoundingClientRect();
      const height = Math.min(options.length * 43 + 12, 260);
      const below = window.innerHeight - rect.bottom - 14;
      const above = rect.top - 14;
      const flip = below < height && above > below;
      const maxHeight = Math.max(40, Math.min(height, flip ? above : below));
      const width = Math.min(rect.width, window.innerWidth - 16);
      setPosition({ left: Math.max(8, Math.min(rect.left, window.innerWidth - width - 8)), top: flip ? rect.top - maxHeight - 6 : rect.bottom + 6, width, maxHeight });
    };
    place();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => { window.removeEventListener('resize', place); window.removeEventListener('scroll', place, true); };
  }, [open, options.length]);
  useEffect(() => {
    if (!open) return;
    menu.current?.querySelector<HTMLButtonElement>('[aria-selected="true"]')?.focus();
    const outside = (e: PointerEvent) => { if (!trigger.current?.contains(e.target as Node) && !menu.current?.contains(e.target as Node)) setOpen(false); };
    document.addEventListener('pointerdown', outside);
    return () => document.removeEventListener('pointerdown', outside);
  }, [open]);
  const choose = (next: T) => { onChange(next); setOpen(false); trigger.current?.focus(); };
  return <div className="policy-picker share-select"><button ref={trigger} type="button" className="policy-trigger" disabled={disabled} aria-label={label} aria-haspopup="listbox" aria-expanded={open} onClick={() => setOpen(!open)}><span><strong>{options.find(option => option.value === value)?.label}</strong></span><Icon name={open ? 'chevron-up' : 'chevron-down'} size={18} /></button>{open && createPortal(<div ref={menu} className="policy-menu share-select-menu" style={position} role="listbox" aria-label={label} onKeyDown={event => {
    if (event.key === 'Escape') { event.preventDefault(); setOpen(false); trigger.current?.focus(); }
    if (event.key === 'Tab') setOpen(false);
    if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      const buttons = Array.from(menu.current?.querySelectorAll<HTMLButtonElement>('button') ?? []);
      const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
      const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length;
      buttons[next]?.focus();
    }
  }}>{options.map(option => <button type="button" key={option.value} role="option" aria-selected={option.value === value} className={'policy-option ' + (option.value === value ? 'selected' : '')} onClick={() => choose(option.value)}><span><strong>{option.label}</strong></span>{option.value === value && <Icon name="check" size={17} />}</button>)}</div>, document.body)}</div>;
}
