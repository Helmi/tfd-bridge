# Replay map artwork

The selected enhanced map set is **Bees to Honey, Ice Islands, Two Brothers, and
Shards**. Water and land are authored separately and bundled as local assets for
Replayer and video rendering. No image generation or download is required while
playing a replay. Other maps retain their original game artwork.

The artwork improves surface detail and softens the original bright shore edging.
The original game's alpha mask defines the coastlines, tiny rocks, gaps, and edge
crops. Generated island outlines never define the final geometry.

## Files and provenance

- `map-ids.txt` selects the four maps for authoring. It is intentionally narrower
  than `sources/manifest.json`, which inventories 54 original map/operation/port
  layer pairs extracted from game build 13015811.
- `sources/spaces/<id>/minimap.png` and `minimap_water.png` are unchanged original
  760 x 760 PNGs. Their hashes and alpha statistics are in the source manifest.
  `reference.png` is a clean 768 x 768 RGB composite made by the same toolkit
  compositor as Bridge. Use it for image generation: transparent PNGs can contain
  meaningless RGB outside land that an image model may misinterpret.
- `style-reference.png` preserves the requested visual direction. Its islands
  are not a geometry reference for another map.
- `generated/<id>/land.png` and `water.png` archive the chosen raw built-in
  `image_gen` outputs. Land uses a magenta background as an intermediate color key;
  the final layer does not use that background or the generated alpha.
- `records/<id>.json` contains the **exact prompts actually used**, ordered input
  references, source/output SHA-256 hashes, and requested versus actual dimensions.
  References and archived-file paths are relative to the repository root.
  Shards also retains its first water image and the subsequent color-correction
  prompt. Bees retains the exact earlier water-only generation prompt.
- `generate-prompts.cjs` produces `generation-jobs.json`, the current four-map
  templates with portable clean-reference paths. These templates can evolve;
  historical records remain the authority for existing artwork.

The built-in tool returned **1254 x 1254** images. Most prompts requested
1520 x 1520; the earlier Bees water prompt requested 2048 x 2048. The finishing
step resamples RGB artwork to 1520 x 1520. Requested dimensions are not presented
as the actual generated resolution.

## Geometry and finishing

`bake-layers.cjs` reads archived RGB artwork and the verified original land PNG.
The final mask is an exact integer **2x** copy of the original 760 x 760 alpha:

```text
finalAlpha(x, y) = originalAlpha(floor(x / 2), floor(y / 2))
```

Every original alpha sample becomes a 2 x 2 block, including partially transparent
shore samples. `register-land.cjs` aligns RGB artwork within that fixed coverage:

1. Identify original land components and generated paint components separately.
   Reject magenta paint and erode the remaining clean paint by **5 output pixels**
   to remove the antialiased pink fringe. This erosion affects usable RGB paint,
   never the original land alpha.
2. Match components by proximity and relative area, processing larger original
   islands first and using each generated component at most once. The minimum
   search reach is 65 output pixels to accommodate the observed AI paint shifts;
   large islands can have a proportionally larger search radius.
3. Fit each matched generated component's RGB bounding rectangle to its original
   island's bounding rectangle. Fill gaps from that component's nearest clean
   paint samples, so neighboring islands cannot bleed into its texture. Apply
   the fitted RGB only where the original island already has coverage.
4. Retain softened original RGB for unmatched original features, including tiny
   fragments or cropped land missed by generation. Never drop an original island
   just because the generated paint omitted it.

Component matching and bounding-box fitting can change the painted terrain's
internal appearance. They cannot move a coastline: all alpha comes directly from
the independent original mask. Water remains fully opaque beneath the separate
land layer. Registration centers, component counts, texture extension, and
fallback pixels are recorded for review alongside the alpha proof.

The output is a separate `land.webp` and `water.webp` for each selected map under
`../web/public/assets/maps/coastal-v1/`. `../web/src/assets/maps/catalog.json`
identifies these bundled layers and the original scene-composite hashes they
replace. The original image must match before an enhancement is applied.

The baker decodes each encoded land WebP and compares **every output alpha sample**
with the expected 2x original mask. Any mismatch fails the bake. It writes source,
generated, and output hashes plus alpha mismatch/repair information to
`verification/<id>.json`, and a review composite to `previews/<id>.png`.

Exact alpha validation proves coastline geometry, not artistic quality. Inspect
the composites for color-key fringes, terrain alignment, biome preservation,
water texture, and marker readability before accepting an asset. The generation
records do not claim that this final composed visual review has passed.

## Rebuild from archived artwork

Run from the repository root with Node.js, Playwright, and Chrome available:

```powershell
node experiments/replay-web-player/map-art/bake-layers.cjs
```

To rebuild one map, append its short ID, for example `23_Shards`.
`PLAYWRIGHT_MODULE` can point to an existing Playwright module if it is not on the
normal Node module path. The script uses an isolated headless Chrome instance.

Rebaking uses archived artwork and needs no image service. Keep the same browser
and encoder versions when comparing encoded-file hashes; encoded RGB bytes can
change across tool versions. The exact-alpha requirement remains unchanged.
Fresh AI generation is nondeterministic: the same prompt and references do not
guarantee identical artwork, so preserve approved raw outputs and their hashes.

## Extract originals and add another map

The Rust example uses Bridge's pinned toolkit and the same version-matched game
resource loader as scene export. It extracts only minimap PNGs and writes clean
composite references, metadata, and hashes. For a matching locally installed game:

```powershell
cargo run -p scene-export --example export_map_layers --offline -- 'C:\Games\World_of_Warships' '15,7,0,13015811' 'private-sync/notes/map-style-v3/originals'
```

Use the appropriate installation path and replay client version for another
machine/build. `spaceSize` in the manifest is in raw BigWorld units, not meters.
The full extraction includes ports; an extracted directory is not automatically
an eligible battle map. Ocean's empty original land mask must remain empty.

1. Copy the chosen original pair and clean `reference.png` into `sources/`, and
   retain their matching source-manifest hashes. Do not replace original PNGs with
   generated artwork.
2. Add the map ID to `map-ids.txt`, then run
   `node experiments/replay-web-player/map-art/generate-prompts.cjs`.
3. Inspect the clean map, original water, and style reference. Resolve job
   references against the repository root and use the built-in `image_gen` tool
   for one land and one water image. Keep the map's own terrain and climate.
4. Archive the chosen raw outputs under `generated/<id>/`, and add an exact record
   including any corrective prompt, actual dimensions, input paths, and hashes.
5. Add the bundled paths, 1520 x 1520 size, and exact original composite hash to
   the runtime catalog. Bake, require zero alpha mismatches, and review the final
   composition and replay before treating the new map as ready.
