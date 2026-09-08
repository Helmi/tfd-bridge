const fs = require('node:fs');
const path = require('node:path');
const root = __dirname;
const catalog = JSON.parse(fs.readFileSync(path.join(root, 'sources/manifest.json'), 'utf8').replace(/^\uFEFF/, ''));
// Store repository-relative paths so the prompts remain usable on other machines.
// Resolve each reference against the repository root before calling image_gen.
const repositoryPath = 'experiments/replay-web-player/map-art';
const style = `${repositoryPath}/style-reference.png`;
const selectedIds = fs.readFileSync(path.join(root, 'map-ids.txt'), 'utf8')
  .replace(/^\uFEFF/, '').split(/\r?\n/).map(line => line.trim()).filter(Boolean);
if (new Set(selectedIds).size !== selectedIds.length) throw new Error('Duplicate selected map ID');
const common = 'Straight-down orthographic tactical game map covering approximately 35–40 kilometres. Preserve the exact square framing, normalized coordinates, scale, north-up orientation and edge crops. No perspective, camera tilt, zoom, padding, labels, text, grid, ships, buildings, roads, map symbols or watermark. All detail must read at this enormous aerial scale, not giant individual trees or close-up waves.';
const jobs = selectedIds.map(mapId => {
  const map = catalog.maps.find(candidate => candidate.id === mapId);
  if (!map) throw new Error(`Selected map is missing from original source catalog: ${mapId}`);
  const id = map.id.slice(7);
  const referencePath = `sources/${map.id}/reference.png`;
  if (!fs.existsSync(path.join(root, referencePath))) {
    throw new Error(`Missing clean original reference: ${referencePath}; run export_map_layers first`);
  }
  const originalLand = `${repositoryPath}/${referencePath}`;
  const originalWater = `${repositoryPath}/sources/${map.water.path}`;
  return {
    id, mapId: map.id, sourceCompositeSha256: map.sceneCompositeSha256,
    land: {
      references: [originalLand, style],
      prompt: `Use case: style-transfer. Create the LAND RGB PAINT layer for ${id}. Image 1 is the authoritative original land geometry and biome. Image 2 gives only the desired softness, painted satellite terrain detail and restrained finish; do NOT copy its islands or turn snow/desert into green. ${common} Match EVERY island silhouette, location, size, rotation and water gap from image 1, including tiny rocks and cropped land at the edges. No landmass may be added, removed, moved, enlarged, merged or split. Preserve this map's own terrain, snow/ice, vegetation, rock, sand and climate from image 1. Improve the terrain artwork with believable fine relief and texture, soft natural coast colours and MUCH lower-contrast coastline edging; no bright gold outlines or glow. Outside land, use one completely FLAT SOLID MAGENTA colour RGB(255,0,255), including all water gaps. This is an intermediate colour-keyed paint layer, NOT a finished map: no water, no transparency, no checkerboard. The final transparent land layer will use the original game's exact alpha mask. Output a 1520x1520 square PNG.`
    },
    water: {
      references: [originalLand, originalWater, style],
      prompt: `Use case: style-transfer / background reconstruction. Create the FULL OPAQUE WATER-ONLY layer for ${id}. Image 1 shows the original island positions and biome ONLY to guide depth variation around shores; do NOT render any land. Image 2 is this map's original water colour family: preserve its blue/green/cold character while making it attractive. Image 3 is the approved stylistic reference for fine textured ocean and restrained teal depth variation. ${common} Ocean must cover EVERY pixel, including where image 1 has land; the islands will be composited separately afterwards. Add subtle, fine, diffuse water texture and broad gentle variation appropriate to a 35–40km map, in the style of image 3. Slight shallow-water colour variation may follow the ORIGINAL coastline positions, but there must be no solid land, rocks, islands, ice floes, foam bands, large wave crests, clouds or ghost land silhouettes anywhere. Keep the water dark and calm enough for small coloured ship markers to remain readable. Avoid excessive contrast, sparkling highlights, close-up water, repeated tiling or shore outlines. No checkerboard or transparency. Output one full opaque 1520x1520 square PNG of WATER ONLY.`
    }
  };
});
fs.writeFileSync(path.join(root, 'generation-jobs.json'), JSON.stringify({
  version: 2,
  tool: 'built-in image_gen',
  referencePathBase: 'repository-root',
  purpose: 'Current generation templates; records/<id>.json preserves the exact prompts used for archived artwork.',
  jobs,
}, null, 2)+'\n');
console.log(`Prepared documented prompts for ${jobs.length} maps.`);
