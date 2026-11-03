import { useEffect } from 'react';
import type { FavoriteDevice } from '../hooks/useDeviceFavorites';
import type { Peer } from '../types/network';
import { addressPriority } from '../utils/address';
import { Icon } from './Icon';
import { useTranslation } from 'react-i18next';

export function FavoritesDialog({ favorites, peers, error, selectedId, canSend, onSelect, onDetails, onSend, onRemove, onClose }: {
  favorites: FavoriteDevice[];
  peers: Peer[];
  error: string;
  selectedId: string;
  canSend: boolean;
  onSelect: (nodeId: string) => void;
  onDetails: (nodeId: string) => void;
  onSend: () => void;
  onRemove: (nodeId: string) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation('common');
  const selected = favorites.some(item => item.node_id === selectedId);
  const onlineIds = new Set(peers.filter(peer => peer.online).map(peer => peer.node_id));
  const sortedFavorites = [...favorites].sort((a, b) => Number(onlineIds.has(b.node_id)) - Number(onlineIds.has(a.node_id)));
  useEffect(() => {
    const escape = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    window.addEventListener('keydown', escape);
    return () => window.removeEventListener('keydown', escape);
  }, [onClose]);
  return <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget) onClose(); }}>
    <section className="modal surface" role="dialog" aria-modal="true" aria-labelledby="favorites-title">
      <div className="modal-heading"><div><span className="eyebrow">FAVORITES</span><h2 id="favorites-title">{t('favorites.title')}</h2></div><button autoFocus type="button" className="icon-button" aria-label={t('favorites.close')} onClick={onClose}><Icon name="close" /></button></div>
      {error && <div className="notice error" role="alert">{error}</div>}
      {!favorites.length ? <div className="empty-state"><Icon name="heart" size={32} /><h3>{t('favorites.emptyTitle')}</h3><p>{t('favorites.emptyHint')}</p></div> : <div className="device-list">
        {sortedFavorites.map(saved => {
          const peer = peers.find(item => item.node_id === saved.node_id);
          const addresses = [...new Set((peer ? peer.endpoints.map(endpoint => endpoint.address) : saved.addresses)
            .map(address => address.replace(/%[^\]:]+(?=\]|$)/, '')))]
            .sort((a, b) => addressPriority(a) - addressPriority(b));
          return <button type="button" className={'device-card ' + (selectedId === saved.node_id ? 'selected' : '')} key={saved.node_id} aria-pressed={selectedId === saved.node_id} onClick={() => onSelect(saved.node_id)} onContextMenu={event => { event.preventDefault(); onDetails(saved.node_id); }}>
            <span className="device-icon"><Icon name="device" size={25} /></span>
            <span className="device-copy"><strong>{peer?.display_name ?? saved.display_name}</strong><span>{addresses[0] || t('favorites.waitingAddress')}</span><small>{saved.node_id.slice(0, 12)} · {peer?.trusted ? t('device.trusted') : peer?.verified ? t('device.verified') : t('device.verifyOnConnect')}</small></span>
            <span className={'status-dot ' + (peer?.online ? 'online' : '')} aria-label={peer?.online ? t('device.online') : t('device.offline')} />
            {selectedId === saved.node_id && <Icon name="check" size={18} />}
          </button>;
        })}
      </div>}
      <div className="modal-actions"><button type="button" disabled={!selected} onClick={() => onRemove(selectedId)}>{t('favorites.cancelFavorite')}</button><button type="button" className="primary" disabled={!selected || !canSend} onClick={onSend}>{t('actions.send')}</button></div>
    </section>
  </div>;
}
