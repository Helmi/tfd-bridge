import importlib.util
import json
import struct
from pathlib import Path

import pytest

spec = importlib.util.spec_from_file_location("ugd", Path(__file__).with_name("update-game-data.py"))
ugd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ugd)

XML = """<?xml version="1.0" encoding="utf-8"?>
<protocol name="game_info"><game><part_versions>
<version name="client" installed="15.9.0.0.13357625" available="15.9.0.0.13357625"/>
<version name="hotfix" installed="15.9.0.0.1" available="15.9.0.0.1"/>
</part_versions></game></protocol>"""


def test_parse_client_version():
    assert ugd.parse_client_version(XML) == ("15.9.0.0.13357625", "13357625")


def test_parse_client_version_missing():
    with pytest.raises(ValueError):
        ugd.parse_client_version("<protocol><version name='hotfix' installed='1.2.3.4.5'/></protocol>")


def test_constants_folder():
    assert ugd.constants_folder("15.9.0.0.13357625") == "15.9.0_13357625"
    assert ugd.constants_folder("15.10.1.0.99") == "15.10.1_99"


def test_diff_keys():
    assert ugd.diff_keys({"a": 1, "b": 2, "c": 3}, {"b": 2, "c": 4, "d": 5}) == (["d"], ["a"], ["c"])


def test_critical_changes():
    keys = ["CLIENT_PUBLIC_RESULTS_INDICES", "RIBBONS", "COMMON_ECONOMICS", "VERSION"]
    assert ugd.critical_changes(keys) == ["CLIENT_PUBLIC_RESULTS_INDICES", "COMMON_ECONOMICS"]


RUST = "const KNOWN_GOOD: &[(u32, u32)] = &[(15, 3), (15, 8), (15, 9)];\n"


def test_known_good():
    assert ugd.known_good(RUST, 15, 9)
    assert not ugd.known_good(RUST, 15, 10)


def test_pinned_rev():
    toml = 'wows_replays = { git = "https://x/y", rev = "6e1973e3c2" }\n'
    assert ugd.pinned_rev(toml) == "6e1973e3c2"


def test_write_if_changed_ignores_line_endings(tmp_path):
    p = tmp_path / "f.json"
    p.write_bytes(b"a\r\nb\n")
    assert not ugd.write_if_changed(p, "a\nb\n")
    assert ugd.write_if_changed(p, "x\n")
    assert p.read_bytes() == b"x\n"


def make_mo(pairs, header=b""):
    """Hand-built little-endian .mo; the header entry has no charset on purpose."""
    items = sorted([(b"", header)] + [(k.encode(), v.encode()) for k, v in pairs])
    n = len(items)
    ktab, vtab = 28, 28 + 8 * n
    pos = vtab + 8 * n
    kmeta, vmeta, blob = [], [], b""
    for k, _ in items:
        kmeta.append((len(k), pos + len(blob)))
        blob += k + b"\0"
    for _, v in items:
        vmeta.append((len(v), pos + len(blob)))
        blob += v + b"\0"
    out = struct.pack("<7I", 0x950412DE, 0, n, ktab, vtab, 0, 0)
    for m in (kmeta, vmeta):
        out += b"".join(struct.pack("<II", *x) for x in m)
    return out + blob


def test_read_mo_non_ascii_no_charset():
    data = make_mo([("IDS_A", "C.\u00a0Colombo \u00e9")], header=b"Project-Id-Version: x\n")
    cat = ugd.read_mo(data)
    assert cat["IDS_A"] == "C.\u00a0Colombo \u00e9"
    assert cat[""] == "Project-Id-Version: x\n"


def test_read_mo_rejects_garbage():
    with pytest.raises(ValueError):
        ugd.read_mo(b"\x00" * 40)


CAT = {"IDS_PAAA001": "A.\u00a0Short", "IDS_PAAA001_FULL": "Alpha Full", "IDS_PAAA002": "Solo",
       "IDS_SPACES/22_TIERRA_DEL_FUEGO": "Land of Fire", "IDS_SPACES/22_TIERRA_DEL_FUEGO_DESCR": "d",
       "IDS_SPACES/45_ZIGZAG": "Loop"}
INDEX = {"1": {"index": "PAAA001"}, "2": {"index": "PAAA002"}, "3": {"index": "PXXX999"}}


def test_build_ship_names():
    ships, skipped = ugd.build_ship_names(CAT, INDEX)
    assert skipped == 1 and set(ships) == {"1", "2"}
    assert ships["1"] == {"index": "PAAA001", "short": "A.\u00a0Short", "full": "Alpha Full"}
    assert ships["2"]["full"] == "Solo"


def test_build_map_names_case():
    assert ugd.build_map_names(CAT, ["22_tierra_del_fuego", "45_Zigzag"]) == {
        "22_tierra_del_fuego": "Land of Fire", "45_Zigzag": "Loop"}
    assert ugd.build_map_names(CAT) == {"22_tierra_del_fuego": "Land of Fire", "45_zigzag": "Loop"}


def test_game_data_version():
    assert ugd.game_data_version("15.9.0.0.13357625") == "15.9.0.13357625"


def test_write_table_keeps_generated_at(tmp_path):
    p = tmp_path / "v" / "ship_names.json"
    ships, _ = ugd.build_ship_names(CAT, INDEX)
    assert ugd.write_table(p, "15.9.0.1", "ships", ships, "2026-01-01T00:00:00Z")
    first = p.read_bytes()
    assert not ugd.write_table(p, "15.9.0.1", "ships", ships, "2027-01-01T00:00:00Z")
    assert p.read_bytes() == first and b"\r" not in first
    assert json.loads(first)["generated_at"] == "2026-01-01T00:00:00Z"
    assert ugd.write_table(p, "15.9.0.1", "ships", {}, "2027-01-01T00:00:00Z")


REAL = Path(r"C:\Games\World_of_Warships")


@pytest.mark.skipif(not REAL.exists(), reason="WoWS not installed")
def test_real_catalog_examples():
    mo = max((REAL / "bin").glob("*/res/texts/en/LC_MESSAGES/global.mo"), key=lambda p: int(p.parts[-6]))
    cat = ugd.read_mo(mo.read_bytes())
    idx = {"a": {"index": "PISB110"}, "b": {"index": "PISD710"}, "c": {"index": "PGSB708"}}
    ships, skipped = ugd.build_ship_names(cat, idx)
    assert skipped == 0
    assert ships["a"]["short"] == "C.\u00a0Colombo" and ships["a"]["full"] == "Cristoforo Colombo"
    assert ships["b"]["short"] == "A. da Barbiano"
    assert ships["c"]["short"] == "Bismarck\u00a0'41"
    maps = ugd.build_map_names(cat)
    assert maps["22_tierra_del_fuego"] == "Land of Fire" and maps["45_zigzag"] == "Loop" and maps["19_oc_prey"] == "Trap"
