import importlib.util
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
