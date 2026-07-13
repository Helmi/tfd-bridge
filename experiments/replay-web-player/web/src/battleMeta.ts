// Shared formatting for replay metadata surfaced in the picker and the title
// bar: the raw WoWS `matchGroup` and the raw `dateTime` header string.

const BATTLE_TYPE_LABELS: Record<string, string> = {
  pvp: 'Random',
  pve: 'Co-op',
  cooperative: 'Co-op',
  ranked: 'Ranked',
  clan: 'Clan Battle',
  cw: 'Clan Battle',
  brawl: 'Brawl',
  pvp_premade: 'Division',
  pve_premade: 'Operation',
  scenario: 'Operation',
};

/** Map the raw WoWS `matchGroup` to a friendly battle-type label. */
export function battleTypeLabel(raw?: string): string | undefined {
  if (!raw) return undefined;
  return (
    BATTLE_TYPE_LABELS[raw.toLowerCase()]
    ?? raw.replace(/[-_]+/g, ' ').replace(/\b\w/g, (letter) => letter.toUpperCase())
  );
}

/** Format the raw WoWS `dateTime` ("DD.MM.YYYY HH:MM:SS") for display. */
export function formatWowsDateTime(raw?: string): string | undefined {
  if (!raw) return undefined;
  const match = raw.match(/^(\d{2})\.(\d{2})\.(\d{4})\s+(\d{2}):(\d{2})(?::(\d{2}))?$/);
  if (!match) return raw;
  const [, dd, mm, yyyy, hh, min] = match;
  const date = new Date(Number(yyyy), Number(mm) - 1, Number(dd), Number(hh), Number(min));
  if (Number.isNaN(date.getTime())) return raw;
  return new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(date);
}
