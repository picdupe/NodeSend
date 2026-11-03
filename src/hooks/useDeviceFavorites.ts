import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { Peer } from '../types/network';

export interface FavoriteDevice {
  node_id: string;
  display_name: string;
  addresses: string[];
}

const storageKey = 'nodesend.device-favorites.v1';

function readFavorites(): FavoriteDevice[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(storageKey) ?? '[]');
    if (!Array.isArray(value)) return [];
    const seen = new Set<string>();
    return value.filter((item): item is FavoriteDevice => {
      if (!item || typeof item.node_id !== 'string' || typeof item.display_name !== 'string'
        || !Array.isArray(item.addresses) || !item.addresses.every((address: unknown) => typeof address === 'string')
        || seen.has(item.node_id)) return false;
      seen.add(item.node_id);
      return true;
    });
  } catch { return []; }
}

export function useDeviceFavorites() {
  const { t } = useTranslation('errors');
  const [favorites, setFavorites] = useState(readFavorites);
  const [favoriteError, setFavoriteError] = useState('');
  function save(next: FavoriteDevice[]) {
    try {
      localStorage.setItem(storageKey, JSON.stringify(next));
      setFavorites(next);
      setFavoriteError('');
    } catch { setFavoriteError(t('settings.favoriteSaveFailed')); }
  }
  function removeFavorite(nodeId: string) {
    save(favorites.filter(item => item.node_id !== nodeId));
  }
  function toggleFavorite(peer: Peer) {
    if (favorites.some(item => item.node_id === peer.node_id)) {
      removeFavorite(peer.node_id);
    } else {
      save([...favorites, {
        node_id: peer.node_id,
        display_name: peer.display_name,
        addresses: [...new Set(peer.endpoints.map(endpoint => endpoint.address))],
      }]);
    }
  }
  return { favorites, favoriteError, toggleFavorite, removeFavorite };
}
