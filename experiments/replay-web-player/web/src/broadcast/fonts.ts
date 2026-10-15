import '../fonts.css';
import { FONT_WEIGHTS } from './layout';

const FAMILIES = ['Space Grotesk', 'JetBrains Mono'];

/** Load every HUD face before the first render: canvas text does not wait for @font-face. */
export async function loadBroadcastFonts(): Promise<void> {
  await Promise.all(FAMILIES.flatMap(family => FONT_WEIGHTS.map(weight => document.fonts.load(`${weight} 16px "${family}"`))));
}
