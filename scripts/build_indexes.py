"""Build the bridge's bundled id indexes from a full GameParams dump in ONE streaming pass.

Writes into <resources> (normally <repo>/src-tauri/resources):
    ship_index.json         ship id -> {index, name, level, species, nation, group}
    achievement_index.json  achievement id -> WG name (e.g. "PCH012_Arsonist")
    bonus_index.json        economic booster / permanent bonus id -> info (see build_bonus_index.py)

Normally run through scripts/update-game-data.py. Standalone, after dumping the patch's GameParams:
    wowsunpack -p <game>/res_packages -i <game>/bin/<build>/idx game-params --full --ugly GameParams.json
    python scripts/build_indexes.py GameParams.json <repo>/src-tauri/resources

GameParams.json is ~3 GB with several region buckets ({region: {paramName: {...}}}); loading it whole
risks running out of memory. It is streamed with ijson: each paramName node is materialized one at a
time and only once (all regions carry identical definitions). Replaces the old load-everything
build_ship_index.py. Output formats match the committed files, so a patch refresh is a reviewable diff.
Existing ship ids keep their file order; new ships are appended. Requires: ijson
"""
import argparse
import json
import os
import sys

import ijson
from ijson.common import ObjectBuilder

from build_bonus_index import build_index as build_bonus


def scan(path):
    """Return (ships, achievements, pce_nodes) from one pass over GameParams.json."""
    ships, achievements, pce = {}, {}, {}
    seen = set()
    stack = []
    name = None
    builder = None
    bdepth = 0
    with open(path, "rb") as f:
        for _prefix, event, value in ijson.parse(f):
            if builder is not None:
                builder.event(event, value)
                if event in ("start_map", "start_array"):
                    bdepth += 1
                elif event in ("end_map", "end_array"):
                    bdepth -= 1
                    if bdepth == 0:
                        node = builder.value
                        builder = None
                        if isinstance(node, dict):
                            classify(name, node, ships, achievements, pce)
                continue
            depth = len(stack)
            if event == "map_key" and depth == 2:
                name = value
            elif event == "start_map":
                if depth == 2 and name not in seen:
                    seen.add(name)
                    builder = ObjectBuilder()
                    builder.event(event, value)
                    bdepth = 1
                else:
                    stack.append("map")
            elif event == "end_map":
                stack.pop()
            elif event == "start_array":
                stack.append("array")
            elif event == "end_array":
                stack.pop()
    return ships, achievements, pce


def classify(name, node, ships, achievements, pce):
    typeinfo = node.get("typeinfo") or {}
    kind = typeinfo.get("type")
    if kind == "Ship" and node.get("id") is not None:
        ships.setdefault(str(node["id"]), {
            "index": node.get("index"),
            "name": node.get("name", name),
            "level": node.get("level"),
            "species": typeinfo.get("species"),
            "nation": typeinfo.get("nation"),
            "group": node.get("group"),
        })
    elif kind == "Achievement" and node.get("id") is not None:
        achievements.setdefault(str(node["id"]), node.get("name", name))
    elif isinstance(name, str) and name.startswith("PCE"):
        pce.setdefault(name, node)


def build_outputs(gameparams, ship_path):
    """Return ({filename: text}, stats). Existing ship ids in ship_path keep their file order."""
    ships, achievements, pce = scan(gameparams)
    bonuses = build_bonus(pce)
    if not ships or not achievements or not bonuses:
        sys.exit(f"empty result (ships={len(ships)} achievements={len(achievements)} bonuses={len(bonuses)}) - wrong GameParams shape?")
    old = {}
    if os.path.exists(ship_path):
        with open(ship_path, encoding="utf-8") as f:
            old = json.load(f)
    merged = {k: ships[k] for k in old if k in ships}
    merged.update({k: v for k, v in ships.items() if k not in merged})
    dropped = [k for k in old if k not in ships]
    outputs = {
        "ship_index.json": json.dumps(merged),
        "achievement_index.json": json.dumps(achievements, indent=2, sort_keys=True),
        "bonus_index.json": json.dumps(bonuses, indent=1, sort_keys=True),
    }
    return outputs, (merged, old, dropped, achievements, bonuses)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("gameparams", help="path to a full GameParams.json dump")
    ap.add_argument("resources", help="output directory, normally <repo>/src-tauri/resources")
    args = ap.parse_args()

    outputs, stats = build_outputs(args.gameparams, os.path.join(args.resources, "ship_index.json"))
    for fname, text in outputs.items():
        with open(os.path.join(args.resources, fname), "w", encoding="utf-8", newline="") as f:
            f.write(text)
    merged, old, dropped, achievements, bonuses = stats
    print(f"ships {len(merged)} (+{len(merged) - (len(old) - len(dropped))} new, {len(dropped)} no longer in GameParams), "
          f"achievements {len(achievements)}, bonuses {len(bonuses)} -> {args.resources}")


if __name__ == "__main__":
    main()
