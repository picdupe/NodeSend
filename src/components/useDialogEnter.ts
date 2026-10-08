import { useEffect } from 'react';

/** Only the top dialog handles Enter, including when its background has focus. */
export function useDialogEnter(active: boolean, confirm: () => void) {
  useEffect(() => {
    if (!active) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Enter' || event.isComposing || event.keyCode === 229) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      if (!event.repeat) confirm();
    };
    window.addEventListener('keydown', onKeyDown, true);
    return () => window.removeEventListener('keydown', onKeyDown, true);
  }, [active, confirm]);
}
