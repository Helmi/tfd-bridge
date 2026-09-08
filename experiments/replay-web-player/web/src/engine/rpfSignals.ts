import type { EvaluatedRpfSector, EvaluatedShip, ReplayScene } from '../types';
import { parseRpfDirection, type RpfDirection } from './rpf';

interface Report extends RpfDirection { t: number }
const reportsByScene = new WeakMap<ReplayScene, Map<string, Report[]>>();

function reportsFor(scene: ReplayScene): Map<string, Report[]> {
  const cached = reportsByScene.get(scene);
  if (cached) return cached;
  const reports = new Map<string, Report[]>();
  for (const chat of scene.chat ?? []) {
    const direction = parseRpfDirection(chat.message);
    if (!direction) continue;
    // Older scenes may only have a username. Never guess between duplicate names.
    const senders = scene.ships.filter(ship => chat.senderId
      ? ship.id === chat.senderId : ship.playerName === chat.senderName);
    if (senders.length !== 1) continue;
    const id = senders[0].id;
    const track = reports.get(id) ?? [];
    track.push({ t: chat.t, ...direction });
    reports.set(id, track);
  }
  for (const track of reports.values()) track.sort((a, b) => a.t - b.t);
  reportsByScene.set(scene, reports);
  return reports;
}

/** Recomputed from replay time, so seeking never retains a future report. */
export function evaluateRpfSectors(scene: ReplayScene, ships: EvaluatedShip[], time: number): EvaluatedRpfSector[] {
  const reports = reportsFor(scene);
  return ships.flatMap(ship => {
    if (ship.destroyed || ship.knowledge === 'hidden') return [];
    const track = reports.get(ship.definition.id);
    if (!track) return [];
    for (let i = track.length - 1; i >= 0; i--) {
      const report = track[i];
      if (report.t <= time) return [{
        shipId: ship.definition.id, reportedAt: report.t,
        position: { x: ship.displayPose.x, y: ship.displayPose.y },
        startBearing: report.startBearing, endBearing: report.endBearing,
      }];
    }
    return [];
  });
}
