const KEY = 'tfd.hud';

function storage(): Pick<Storage, 'getItem' | 'setItem'> | undefined {
  try { return typeof localStorage === 'undefined' ? undefined : localStorage; } catch { return undefined; }
}

/** True when the Pixi HUD is on. The render worker page shares this localStorage. */
export function pixiHudEnabled(store = storage()): boolean {
  try { return store?.getItem(KEY) === 'pixi'; } catch { return false; }
}

/** `?hud=pixi` or `?hud=dom` on load writes the flag. Other values change nothing. */
export function syncHudFlagFromUrl(search = typeof location === 'undefined' ? '' : location.search, store = storage()): void {
  const value = new URLSearchParams(search).get('hud');
  if (value !== 'pixi' && value !== 'dom') return;
  try { store?.setItem(KEY, value); } catch { /* storage blocked: the flag stays off */ }
}
