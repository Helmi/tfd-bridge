const KNOWN_LABELS: Record<string, string> = {
  damage: 'Damage',
  health: 'Max HP',
  reload: 'Reload',
  repair: 'Restoration',
};

/** Turn a version-specific game marker key into a compact display fallback. */
export function buffMarkerLabel(markerName: string | null | undefined): string {
  if (!markerName) return 'Powerup';
  const base = markerName
    .replace(/_(?:in)?active(?:_small)?$/i, '')
    .replace(/_small$/i, '');
  const known = KNOWN_LABELS[base.toLowerCase()];
  if (known) return known;
  return base
    .replace(/([a-z\d])([A-Z])/g, '$1 $2')
    .replace(/[_-]+/g, ' ')
    .replace(/\b\w/g, (letter) => letter.toUpperCase())
    .trim() || 'Powerup';
}
