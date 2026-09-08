# Bundled replay maps

`catalog.json` selects the four enhanced maps and pins each to the SHA-256 of
its original scene-composite PNG. `engine/enhancedMap.ts` loads and composes the
separate land/water WebP files from `public/assets/maps/coastal-v1/`, sharing a
leased texture between RePlayer and the video renderer. Unknown or changed game
maps and failed optional asset loads use the original scene image.

Original land alpha is preserved exactly at 2x resolution. Authoring inputs,
archived generated artwork, exact prompts, the finishing scripts, and validation
records are in [map-art](../../../../map-art/README.md).