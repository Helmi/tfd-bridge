# Acceptance check — 2026-09-06

The four maps were baked with recipe 2 and installed locally with Bridge 0.17.0.
All 9,241,600 encoded land-alpha samples match the original game masks at 2x
resolution. Each map also reports zero detected magenta-key contamination.
The eight runtime WebP files total 1,044,638 bytes.

The final previews use decoded land and water WebPs, matching runtime composition.
The four-map sheet and the installed Bees to Honey replay were visually checked.
Tiny unmatched features retain softened original terrain: approximately 1.64% of
covered land pixels on Bees (primarily the cropped east edge), 0.026% on Ice
Islands, 0.112% on Two Brothers, and none on Shards. They retain original alpha.

Validation included 94 passing frontend tests, TypeScript and the Bridge build;
actual map-hash matching, shared texture leases, idle eviction and fallback;
and successful 1920x1080 SharedFrameRenderer frames for all four map assets.
Only Bees used its genuine battle data; the other three were explicitly marked
as layout checks with borrowed battle data. No full video encode was needed for
this artwork change. The installed server returned all eight exact verified
files with image/webp MIME types. The installed roster check confirmed that only
the enemy top strip is reversed and both side rosters retain class/tier order.

Per-map JSON records in this directory contain the input/output hashes and
registration statistics. Regenerate those records when artwork or baking changes;
this dated acceptance note describes this specific local build.
