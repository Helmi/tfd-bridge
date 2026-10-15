"""One-command refresh of everything TFD Bridge derives from World of Warships game data.

    uv run --with ijson --with httpx python scripts/update-game-data.py [--dry-run] [--keep-dump]

Steps: read client version/build, dump GameParams, rebuild the three id indexes, write the
ship/map name tables to game-data/, fetch the build's
constants.json from landaire/wows-replay-data, then print a summary and the manual checklist.
Never commits, pushes, or edits Rust/TS code. --dry-run writes nothing into the repo.
See docs/game-update-cycle.md.
"""
import argparse
import json
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

CONSTANTS_URL = "https://raw.githubusercontent.com/landaire/wows-replay-data/main/{folder}/constants.json"
CRITICAL_CONSTANTS = ("CLIENT_PUBLIC_RESULTS", "PLAYER_PRIVATE_RESULTS", "COMMON_ECONOMICS",
                      "SUBTOTAL_ECONOMICS", "INIT_ECONOMICS")
TOOLKIT_PATHS = ("crates/wows-replays", "crates/wowsunpack", "crates/wows-battle-world", "crates/minimap-renderer")
CHECKLIST = """\
Manual follow-ups (details in docs/game-update-cycle.md):
  [ ] Review the diff of src-tauri/resources/*.json, then commit it
  [ ] Bump the wows-toolkit pin (crates/bridge-core + crates/scene-export Cargo.toml) and docs/wows-toolkit-rev.md
  [ ] Add the new <major>.<minor> to KNOWN_GOOD in crates/bridge-core/src/battle_result.rs
  [ ] Decode sweep on replays recorded with the new client
  [ ] Refresh the Dropbox wows-assets subset
  [ ] Notify the Engine and recorder agents
  [ ] Update WANT_VERSIONS in the private xp-analysis pipeline"""


def parse_client_version(xml_text):
    """Return (version, build) from game_info.xml, e.g. ("15.9.0.0.13357625", "13357625")."""
    for node in ET.fromstring(xml_text).iter("version"):
        if node.get("name") == "client":
            version = node.get("installed")
            if not version:
                break
            return version, version.rsplit(".", 1)[-1]
    raise ValueError("no <version name=\"client\" installed=...> in game_info.xml")


def constants_folder(version):
    """'15.9.0.0.13357625' -> '15.9.0_13357625' (wows-replay-data folder naming)."""
    parts = version.split(".")
    if len(parts) < 4:
        raise ValueError(f"unexpected client version {version!r}")
    return f"{parts[0]}.{parts[1]}.{parts[2]}_{parts[-1]}"


def diff_keys(old, new):
    """Return (added, removed, changed) top-level keys, each sorted."""
    return (sorted(new.keys() - old.keys()), sorted(old.keys() - new.keys()),
            sorted(k for k in old.keys() & new.keys() if old[k] != new[k]))


def critical_changes(keys):
    """Keys belonging to a critical constants family (incl. *_INDICES variants)."""
    return [k for k in keys if any(k == c or k == c + "_INDICES" for c in CRITICAL_CONSTANTS)]


def known_good(rust_source, major, minor):
    m = re.search(r"const\s+KNOWN_GOOD\s*:[^=]*=\s*&\[(.*?)\]\s*;", rust_source, re.S)
    if not m:
        raise ValueError("KNOWN_GOOD not found in battle_result.rs")
    pairs = {(int(a), int(b)) for a, b in re.findall(r"\(\s*(\d+)\s*,\s*(\d+)\s*\)", m.group(1))}
    return (major, minor) in pairs


def pinned_rev(cargo_toml):
    m = re.search(r'^wows_replays\s*=.*?rev\s*=\s*"([0-9a-f]+)"', cargo_toml, re.M)
    if not m:
        raise ValueError("wows_replays rev not found in Cargo.toml")
    return m.group(1)


def render_constants(data):
    """Repo format: 2-space indent, UTF-8, no trailing newline."""
    return json.dumps(data, indent=2, ensure_ascii=False)


def norm(text):
    return text.replace("\r\n", "\n")


def write_if_changed(path, text):
    """Write LF text unless the file already has the same content modulo line endings."""
    if path.exists() and norm(path.read_text(encoding="utf-8")) == text:
        return False
    path.write_bytes(text.encode("utf-8"))
    return True


def read_mo(data):
    """Parse a little-endian gettext .mo file into {msgid: msgstr} (UTF-8, no charset header needed)."""
    magic, _revision, count, orig_off, trans_off = struct.unpack_from("<5I", data, 0)
    if magic != 0x950412DE:
        raise ValueError("not a little-endian .mo file")
    out = {}
    for i in range(count):
        klen, koff = struct.unpack_from("<II", data, orig_off + 8 * i)
        vlen, voff = struct.unpack_from("<II", data, trans_off + 8 * i)
        out[data[koff:koff + klen].decode("utf-8")] = data[voff:voff + vlen].decode("utf-8")
    return out


def game_data_version(version):
    """'15.9.0.0.13357625' -> '15.9.0.13357625' (folder name under game-data/)."""
    parts = version.split(".")
    if len(parts) < 4:
        raise ValueError(f"unexpected client version {version!r}")
    return ".".join(parts[:3] + [parts[-1]])


def build_ship_names(catalog, ship_index):
    """Return ({ship_id: {index, short, full}}, skipped). Ships without IDS_<index> are skipped."""
    ships, skipped = {}, 0
    for ship_id, info in ship_index.items():
        idx = info["index"]
        short = catalog.get("IDS_" + idx)
        if short is None:
            skipped += 1
            continue
        ships[ship_id] = {"index": idx, "short": short, "full": catalog.get("IDS_" + idx + "_FULL", short)}
    return ships, skipped


def build_map_names(catalog, space_dirs=()):
    """{space code: name} for every IDS_SPACES/<KEY> (not *_DESCR). Original case from space_dirs, else lowercase."""
    by_lower = {d.lower(): d for d in space_dirs}
    prefix = "IDS_SPACES/"
    maps = {}
    for key, name in catalog.items():
        if key.startswith(prefix) and not key.endswith("_DESCR"):
            code = key[len(prefix):].lower()
            maps[by_lower.get(code, code)] = name
    return maps


def list_space_dirs(wowsunpack, game, build, work):
    """Folder names under spaces/ in the game resources (metadata dump, about 3 seconds)."""
    out = work / "metadata.txt"
    r = run([str(wowsunpack), "-p", str(game / "res_packages"), "-i", str(game / "bin" / build / "idx"),
             "metadata", str(out)])
    if r.returncode != 0 or not out.exists():
        print(f"warning: wowsunpack metadata failed, map codes will be lowercase:\n{r.stderr}")
        return []
    text = out.read_text(encoding="utf-8", errors="replace")
    return sorted(set(re.findall(r"/spaces/([^/\s]+)/", text)))


def render_table(game_version, generated_at, key, entries):
    doc = {"schema_version": "1.0", "game_version": game_version, "generated_at": generated_at, key: entries}
    return json.dumps(doc, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


def write_table(path, game_version, key, entries, now):
    """Write a table file; reuse the old generated_at when the rest is identical. Returns True if written."""
    generated_at = now
    if path.exists():
        try:
            old = json.loads(path.read_text(encoding="utf-8"))
            if render_table(game_version, old["generated_at"], key, entries) == norm(path.read_text(encoding="utf-8")):
                generated_at = old["generated_at"]
        except (ValueError, KeyError):
            pass
    path.parent.mkdir(parents=True, exist_ok=True)
    return write_if_changed(path, render_table(game_version, generated_at, key, entries))


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace", **kw)


def dump_gameparams(wowsunpack, game, build, out):
    cmd = [str(wowsunpack), "-p", str(game / "res_packages"), "-i", str(game / "bin" / build / "idx"),
           "game-params", "--full", "--ugly", str(out)]
    r = run(cmd)
    if r.returncode != 0 or not out.exists():
        sys.exit(f"wowsunpack failed ({r.returncode}):\n{r.stdout}\n{r.stderr}")


def index_report(label, old, new, namer):
    added, removed, _ = diff_keys(old, new)
    print(f"{label}: {len(new)} total, +{len(added)} new, -{len(removed)} removed")
    for k in added:
        print(f"  + {k}  {namer(new[k])}")
    for k in removed:
        print(f"  - {k}  {namer(old[k])}")


def fetch_constants(folder):
    import httpx
    r = httpx.get(CONSTANTS_URL.format(folder=folder), timeout=60, follow_redirects=True)
    if r.status_code == 404:
        return None
    r.raise_for_status()
    return r.json()


def toolkit_commits(toolkit, rev):
    if not (toolkit / ".git").exists():
        return f"toolkit checkout not found at {toolkit}"
    fetch = run(["git", "-C", str(toolkit), "fetch", "--quiet", "--all"])
    note = "" if fetch.returncode == 0 else f" (git fetch failed: {fetch.stderr.strip()})"
    if run(["git", "-C", str(toolkit), "cat-file", "-e", rev + "^{commit}"]).returncode != 0:
        return f"pinned rev {rev[:8]} not in the checkout{note}"
    log = run(["git", "-C", str(toolkit), "log", "--oneline", f"{rev}..origin/main", "--", *TOOLKIT_PATHS])
    if log.returncode != 0:
        return f"git log failed: {log.stderr.strip()}{note}"
    lines = log.stdout.strip().splitlines()
    return (f"{len(lines)} upstream commit(s) since pin {rev[:8]}{note}" + "".join("\n  " + l for l in lines))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--game", type=Path, default=Path(r"C:\Games\World_of_Warships"), help="WoWS install dir")
    ap.add_argument("--repo", type=Path, default=HERE.parent, help="repo root")
    ap.add_argument("--wowsunpack", type=Path,
                    default=Path(r"C:\Users\fhelm\code\wows-toolkit\target\release\wowsunpack.exe"))
    ap.add_argument("--toolkit", type=Path, default=Path(r"C:\Users\fhelm\code\wows-toolkit"),
                    help="wows-toolkit checkout")
    ap.add_argument("--dry-run", action="store_true", help="write nothing into the repo")
    ap.add_argument("--keep-dump", action="store_true", help="keep the GameParams dump")
    args = ap.parse_args()

    import build_indexes

    res = args.repo / "src-tauri" / "resources"
    version, build = parse_client_version((args.game / "game_info.xml").read_text(encoding="utf-8"))
    major, minor = (int(x) for x in version.split(".")[:2])
    print(f"Client {version} (build {build}){'  [dry run]' if args.dry_run else ''}")

    work = Path(tempfile.mkdtemp(prefix="tfd-gamedata-"))
    try:
        dump = work / "GameParams.json"
        print("Dumping GameParams (1-2 minutes)...")
        dump_gameparams(args.wowsunpack, args.game, build, dump)
        print("Building indexes...")
        outputs, (ships, _old, _dropped, achievements, bonuses) = build_indexes.build_outputs(
            str(dump), str(res / "ship_index.json"))
        old_json = {n: json.loads((res / n).read_text(encoding="utf-8")) for n in outputs if (res / n).exists()}
        new_json = {"ship_index.json": ships, "achievement_index.json": achievements, "bonus_index.json": bonuses}
        catalog = read_mo((args.game / "bin" / build / "res" / "texts" / "en" / "LC_MESSAGES" / "global.mo").read_bytes())
        ship_names, skipped = build_ship_names(catalog, ships)
        map_names = build_map_names(catalog, list_space_dirs(args.wowsunpack, args.game, build, work))
        gd_version = game_data_version(version)
        now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        constants_old = json.loads((res / "constants.json").read_text(encoding="utf-8"))
        folder = constants_folder(version)
        constants_new = fetch_constants(folder)
        written = []
        if not args.dry_run:
            written = [n for n, t in outputs.items() if write_if_changed(res / n, t)]
            for sub in (gd_version, "latest"):
                for name, key, entries in (("ship_names.json", "ships", ship_names), ("map_names.json", "maps", map_names)):
                    if write_table(args.repo / "game-data" / sub / name, gd_version, key, entries, now):
                        written.append(f"game-data/{sub}/{name}")
            if constants_new is not None and write_if_changed(res / "constants.json", render_constants(constants_new)):
                written.append("constants.json")
        if args.keep_dump:
            kept = Path(tempfile.gettempdir()) / f"GameParams-{build}.json"
            shutil.move(str(dump), kept)
            print(f"Kept dump: {kept}")
    finally:
        shutil.rmtree(work, ignore_errors=True)

    print("\n=== Summary ===")
    index_report("Ships", old_json.get("ship_index.json", {}), new_json["ship_index.json"], lambda v: v.get("name"))
    print(f"Names: {len(ship_names)} ships ({skipped} skipped, no IDS_<index>), {len(map_names)} maps")
    index_report("Achievements", old_json.get("achievement_index.json", {}), new_json["achievement_index.json"], str)
    index_report("Bonuses", old_json.get("bonus_index.json", {}), new_json["bonus_index.json"], lambda v: v.get("name"))
    if constants_new is None:
        print(f"Constants: constants not yet published (wows-replay-data/{folder})")
    else:
        added, removed, changed = diff_keys(constants_old, constants_new)
        print(f"Constants ({folder}): +{len(added)} added {added}, -{len(removed)} removed {removed}, "
              f"{len(changed)} changed {changed}")
        crit = critical_changes(added + removed + changed)
        if crit:
            print(f"!!! WARNING: critical decode constants changed: {crit} - re-check the battle-result decoder !!!")
    rust = (args.repo / "crates/bridge-core/src/battle_result.rs").read_text(encoding="utf-8")
    print(f"{major}.{minor} in KNOWN_GOOD: {'yes' if known_good(rust, major, minor) else 'NO - add it after the decode sweep'}")
    rev = pinned_rev((args.repo / "crates/bridge-core/Cargo.toml").read_text(encoding="utf-8"))
    print("wows-toolkit: " + toolkit_commits(args.toolkit, rev))
    print("Repo files written: " + (", ".join(written) if written else "none"))
    print("\n" + CHECKLIST)


if __name__ == "__main__":
    main()
