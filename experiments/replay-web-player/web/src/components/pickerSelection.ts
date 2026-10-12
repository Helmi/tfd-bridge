export const MAX_BATCH_SELECTION = 20;

/** Toggles a replay id; a new id is ignored once the limit is reached. */
export function toggleSelection(selected: string[], id: string, max = MAX_BATCH_SELECTION): string[] {
  if (selected.includes(id)) return selected.filter(item => item !== id);
  return selected.length >= max ? selected : [...selected, id];
}

export function selectionFull(selected: string[], max = MAX_BATCH_SELECTION): boolean {
  return selected.length >= max;
}

/** Selected replays that are still loaded (selection is keyed by id, not by list position). */
export function selectedReplays<T extends { id: string }>(replays: T[], selected: string[]): T[] {
  const wanted = new Set(selected);
  return replays.filter(replay => wanted.has(replay.id));
}
