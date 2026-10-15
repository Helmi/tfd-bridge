"""Build the economic-bonus index (GameParams id -> bonus info) from a full GameParams dump.

Output: src-tauri/resources/bonus_index.json, read by the bridge's battle-result
decoder to resolve the numeric ids in a replay's private `subtotal_economics`
modifier chains (owner `economic_bonuses` / `economic_breakdown`).

Selected entries: typeinfo.type == "Exterior" with species
  "Camoboost"  -> kind "booster"   (PCEA* consumable economic boosters)
  "Multiboost" -> kind "permanent" (PCEU* permanent ship/commander bonuses)
Each entry: {index, kind, modifiers, name, rarity}; rarity is
RarityCategory.name when present (boosters: Common/Uncommon/Rare/Epic/Legendary).

Regenerate per game patch (same cadence as ship_index.json / achievement_index.json):
    wowsunpack -p <game>/res_packages -i <game>/bin/<build>/idx game-params --full --ugly GameParams.json
    python scripts/build_bonus_index.py GameParams.json <repo>/src-tauri/resources/bonus_index.json

GameParams.json is ~3 GB with several region buckets ({region: {paramName: {...}}}),
so it is streamed with ijson (see build_achievement_index.py) and only param
names starting with "PCE" are materialized. Requires: ijson
"""
import argparse
import json
import sys

import ijson
from ijson.common import ObjectBuilder

SPECIES_KIND = {"Camoboost": "booster", "Multiboost": "permanent"}


def scan_pce(path):
    """Stream GameParams.json and return {name: node} for params named PCE*."""
    found = {}
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
                        found.setdefault(name, builder.value)
                        builder = None
                continue
            depth = len(stack)
            if event == "map_key" and depth == 2:
                name = value
            elif event == "start_map":
                if depth == 2 and isinstance(name, str) and name.startswith("PCE"):
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
    return found


def build_index(params):
    index = {}
    for name, node in params.items():
        if not isinstance(node, dict):
            continue
        typeinfo = node.get("typeinfo") or {}
        kind = SPECIES_KIND.get(typeinfo.get("species"))
        if typeinfo.get("type") != "Exterior" or kind is None:
            continue
        rarity = node.get("RarityCategory")
        index[str(int(node["id"]))] = {
            "index": node["index"],
            "kind": kind,
            "modifiers": {k: float(v) for k, v in (node.get("modifiers") or {}).items()},
            "name": name,
            "rarity": rarity.get("name") if isinstance(rarity, dict) else None,
        }
    return index


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("gameparams", help="path to a full GameParams.json dump")
    ap.add_argument("out", help="output path for bonus_index.json")
    args = ap.parse_args()

    index = build_index(scan_pce(args.gameparams))
    if not index:
        sys.exit("no economic bonuses found - wrong GameParams shape?")
    with open(args.out, "w", encoding="utf-8", newline="\n") as f:
        f.write(json.dumps(index, indent=1, sort_keys=True))
    kinds = {}
    for v in index.values():
        kinds[v["kind"]] = kinds.get(v["kind"], 0) + 1
    print(f"wrote {len(index)} entries {kinds} -> {args.out}")


if __name__ == "__main__":
    main()
