import { Container, Graphics, Rectangle, type Sprite, type Text, type TextStyleOptions } from 'pixi.js';
import { DIVISION_COLOR, SELECTED_SHIP_COLOR, isOwnDivision, shipIconColor } from '../divisions';
import type { EntityId, EvaluatedShip, ShipDefinition } from '../types';
import { BOX, COLORS, FLEET_COLUMN_WIDTH, FLEET_GAP, FLEET_PADDING_TOP, FONT_MONO, FONT_SANS, ROSTER_ROW_HEIGHT } from './layout';
import { setTextIfChanged, truncateToWidth } from './text';
import type { BroadcastFrameState, BroadcastLayer, LayerFactory } from './types';

// Inner metrics: the LAST matching rules for .fleets, .roster-row, .roster-name and .health-track
// in components/broadcast.css. Row origin is the top-left of the row box.
const FLEETS_BORDER = 1;
/** `.fleet+.fleet`: 1px left border plus 24px padding. */
const SECOND_COLUMN_INSET = 25;
const ICON_BOX = 24;
const ROW_GAP = 9;
const NAME_X = ICON_BOX + ROW_GAP;
const TRACK_WIDTH = 58;
const TRACK_HEIGHT = 4;
const NAME_WIDTH_LOSS = NAME_X + ROW_GAP + TRACK_WIDTH;
/** Line box centres of the two name lines (16px at 1.1, gap 3, 13px at 1.2, centred in 44). */
const NAME_CENTER_Y = 12.7;
const SUB_CENTER_Y = 32.3;
/** The 13px division letter sits on the 16px baseline, a little lower than the line centre. */
const LETTER_CENTER_Y = 13.8;
const LETTER_MARGIN = 7;
const MARKER = { x: -9, y: 6, w: 2, h: 29 } as const;
const SUNK_ROW_ALPHA = 0.42;
const DEAD_ICON = { tint: '#68766d', alpha: 0.45 } as const;
const NAME_COLOR = '#e1e9e2';
const SUB_COLOR = '#b8c2bc';
const SUB_COLOR_SUNK = '#92aa9b';

const NAME_STYLE: TextStyleOptions = { fontFamily: FONT_SANS, fontSize: 16, fontWeight: '500', fill: NAME_COLOR };
const SUB_STYLE: TextStyleOptions = { fontFamily: FONT_SANS, fontSize: 13, fontWeight: '400', fill: SUB_COLOR };
const LETTER_STYLE: TextStyleOptions = { fontFamily: FONT_MONO, fontSize: 13, fontWeight: '600', fill: COLORS.green };

const number = (n: number) => Math.round(n).toLocaleString('en-US');

export interface RosterRowModel {
  id: EntityId;
  shipClass: ShipDefinition['shipClass'];
  shipName: string;
  /** `[clan] player`, or just the player. */
  playerLine: string;
  divisionLabel?: string;
  divisionColor: string;
  /** Class icon colour of a ship that is still afloat. */
  iconColor: string;
  teamColor: string;
  sunk: boolean;
  self: boolean;
  /** 0..1 */
  health: number;
  /** Screen reader text. */
  title: string;
}

export function rosterRowModel(ship: EvaluatedShip, frame: Pick<BroadcastFrameState, 'scene' | 'selectedId'>, enemy: boolean): RosterRowModel {
  const { definition } = ship;
  const teamColor = enemy ? COLORS.red : COLORS.green;
  return {
    id: definition.id,
    shipClass: definition.shipClass,
    shipName: definition.shipName,
    playerLine: `${definition.clan ? `[${definition.clan}] ` : ''}${definition.playerName}`,
    divisionLabel: definition.divisionLabel || undefined,
    divisionColor: isOwnDivision(definition, frame.scene.ships) ? DIVISION_COLOR : teamColor,
    iconColor: shipIconColor(definition, frame.scene.ships, frame.selectedId, teamColor),
    teamColor,
    sunk: ship.health <= 0,
    self: definition.id === frame.selectedId,
    health: Math.max(0, Math.min(1, ship.health / definition.maxHealth)),
    title: `${definition.shipName}, ${definition.playerName}, ${number(ship.health)} / ${number(definition.maxHealth)} HP`,
  };
}

/** Both columns in frame order (`compareFleetShips`, applied by the stage). */
export function rosterModels(frame: Pick<BroadcastFrameState, 'scene' | 'selectedId' | 'ally' | 'enemy'>): { ally: RosterRowModel[]; enemy: RosterRowModel[] } {
  return {
    ally: frame.ally.map(ship => rosterRowModel(ship, frame, false)),
    enemy: frame.enemy.map(ship => rosterRowModel(ship, frame, true)),
  };
}

interface Column { container: Container; x: number; width: number }

interface RowView {
  container: Container;
  icon: Sprite;
  letter?: Text;
  name: Text;
  sub: Text;
  fill: Graphics;
  marker: Graphics;
  onSelect: () => void;
  shown: { iconTint?: string; iconAlpha?: number; letterColor?: string; nameColor?: string; subColor?: string; scaleX?: number; title?: string };
}

function createRow(ctx: Parameters<LayerFactory>[0], model: RosterRowModel, column: Column): RowView {
  const container = new Container({ label: `roster-row:${model.id}` });
  container.hitArea = new Rectangle(0, 0, column.width, ROSTER_ROW_HEIGHT);

  const icon = ctx.classIconSprite(model.shipClass);
  const fit = Math.min(ICON_BOX / icon.width, ICON_BOX / icon.height);
  icon.scale.set(fit);
  icon.position.set(ICON_BOX / 2, ROSTER_ROW_HEIGHT / 2);

  const marker = new Graphics().rect(MARKER.x, MARKER.y, MARKER.w, MARKER.h).fill(SELECTED_SHIP_COLOR);
  const track = new Graphics().rect(0, 0, TRACK_WIDTH, TRACK_HEIGHT).fill('#253d30');
  track.position.set(column.width - TRACK_WIDTH, (ROSTER_ROW_HEIGHT - TRACK_HEIGHT) / 2);
  const fill = new Graphics().rect(0, 0, TRACK_WIDTH, TRACK_HEIGHT).fill(model.teamColor);
  fill.position.copyFrom(track.position);

  let nameX = NAME_X;
  let letter: Text | undefined;
  if (model.divisionLabel) {
    letter = ctx.createText(LETTER_STYLE);
    letter.anchor.set(0, 0.5);
    letter.position.set(NAME_X, LETTER_CENTER_Y);
    setTextIfChanged(letter, model.divisionLabel);
    nameX += letter.width + LETTER_MARGIN;
  }
  const textWidth = column.width - NAME_WIDTH_LOSS;
  const name = ctx.createText(NAME_STYLE);
  name.anchor.set(0, 0.5);
  name.position.set(nameX, NAME_CENTER_Y);
  setTextIfChanged(name, truncateToWidth(model.shipName, NAME_STYLE, textWidth - (nameX - NAME_X)));
  const sub = ctx.createText(SUB_STYLE);
  sub.anchor.set(0, 0.5);
  sub.position.set(NAME_X, SUB_CENTER_Y);
  setTextIfChanged(sub, truncateToWidth(model.playerLine, SUB_STYLE, textWidth));

  container.addChild(marker, icon, ...(letter ? [letter] : []), name, sub, track, fill);
  return { container, icon, letter, name, sub, fill, marker, onSelect: () => {}, shown: {} };
}

/** Both fleet rosters: class icon, division letter, ship name, player, health bar. Rows select a ship. */
export const createRosterLayer: LayerFactory = (ctx) => {
  const root = new Container({ label: 'rosters' });
  const rowsTop = BOX.fleets.y + FLEETS_BORDER + FLEET_PADDING_TOP;
  const secondX = BOX.fleets.x + FLEET_COLUMN_WIDTH + FLEET_GAP;
  const columns: Record<'ally' | 'enemy', Column> = {
    ally: { container: new Container({ label: 'roster-ally' }), x: BOX.fleets.x, width: FLEET_COLUMN_WIDTH },
    enemy: { container: new Container({ label: 'roster-enemy' }), x: secondX + SECOND_COLUMN_INSET, width: FLEET_COLUMN_WIDTH - SECOND_COLUMN_INSET },
  };
  const lines = new Graphics()
    .rect(BOX.fleets.x, BOX.fleets.y, BOX.fleets.w, FLEETS_BORDER).fill(COLORS.line)
    .rect(secondX, rowsTop, FLEETS_BORDER, BOX.fleets.y + BOX.fleets.h - rowsTop).fill(COLORS.line);
  root.addChild(lines, columns.ally.container, columns.enemy.container);

  const rows = new Map<EntityId, RowView>();
  const orders: Record<'ally' | 'enemy', string> = { ally: '', enemy: '' };
  let current: BroadcastFrameState | undefined;
  let interactive = false;

  const setInteractive = (row: RowView, on: boolean) => {
    const { container } = row;
    if (on) {
      container.eventMode = 'static';
      container.cursor = 'pointer';
      container.accessible = true;
      container.accessibleType = 'button';
      container.tabIndex = 0;
      container.on('pointertap', row.onSelect);
      container.on('click', row.onSelect);
    } else {
      container.off('pointertap', row.onSelect);
      container.off('click', row.onSelect);
      container.eventMode = 'passive';
      container.cursor = 'default';
      container.accessible = false;
    }
  };

  const apply = (row: RowView, model: RosterRowModel, index: number, column: Column) => {
    const { shown } = row;
    row.container.position.set(column.x, rowsTop + index * ROSTER_ROW_HEIGHT);
    row.container.alpha = model.sunk ? SUNK_ROW_ALPHA : 1;
    row.marker.visible = model.self;
    const iconTint = model.sunk ? DEAD_ICON.tint : model.iconColor;
    if (shown.iconTint !== iconTint) { row.icon.tint = iconTint; shown.iconTint = iconTint; }
    const iconAlpha = model.sunk ? DEAD_ICON.alpha : 1;
    if (shown.iconAlpha !== iconAlpha) { row.icon.alpha = iconAlpha; shown.iconAlpha = iconAlpha; }
    if (row.letter && shown.letterColor !== model.divisionColor) { row.letter.style.fill = model.divisionColor; shown.letterColor = model.divisionColor; }
    const nameColor = model.self ? SELECTED_SHIP_COLOR : NAME_COLOR;
    if (shown.nameColor !== nameColor) { row.name.style.fill = nameColor; shown.nameColor = nameColor; }
    const subColor = model.sunk ? SUB_COLOR_SUNK : SUB_COLOR;
    if (shown.subColor !== subColor) { row.sub.style.fill = subColor; shown.subColor = subColor; }
    if (shown.scaleX !== model.health) {
      row.fill.scale.x = model.health;
      row.fill.visible = model.health > 0;
      shown.scaleX = model.health;
    }
    if (interactive && shown.title !== model.title) { row.container.accessibleTitle = model.title; shown.title = model.title; }
  };

  return {
    container: root,
    update(frame) {
      current = frame;
      if (frame.interactive !== interactive) {
        interactive = frame.interactive;
        for (const row of rows.values()) {
          setInteractive(row, interactive);
          row.shown.title = undefined;
        }
      }
      const models = rosterModels(frame);
      for (const side of ['ally', 'enemy'] as const) {
        const column = columns[side];
        models[side].forEach((model, index) => {
          let row = rows.get(model.id);
          if (!row) {
            row = createRow(ctx, model, column);
            row.onSelect = () => current?.selectShip(model.id);
            rows.set(model.id, row);
            column.container.addChild(row.container);
            if (interactive) setInteractive(row, true);
          }
          apply(row, model, index, column);
        });
        // Keep the DOM order of the accessibility layer (Tab order) equal to the visible order.
        const order = models[side].map(model => model.id).join('|');
        if (order !== orders[side]) {
          for (const model of models[side]) column.container.addChild(rows.get(model.id)!.container);
          orders[side] = order;
        }
      }
    },
    destroy() { root.destroy({ children: true }); },
  } satisfies BroadcastLayer;
};
