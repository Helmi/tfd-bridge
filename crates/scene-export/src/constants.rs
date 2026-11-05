//! Per-version WoWS constants (`constants.json` + `constants/<major.minor>.json`).
//!
//! WoWS changes some per-build layouts (battle-result field order, the
//! player/bot arena-state member maps) on patch boundaries. The app bundles the
//! newest constants as `constants.json` and keeps older ones in `constants/`.
//! A replay uses the newest file whose `VERSION` is not newer than the replay.

use std::path::{Path, PathBuf};

/// Newest build whose player/bot member maps are built into the wows-toolkit
/// (version-gated in `wows_replays`). Replays up to this version need no
/// constants override for the scene; newer ones take the selected file's maps.
pub const BUILTIN_MEMBER_MAPS_THROUGH: (u32, u32) = (15, 9);

/// Parse `"15.10"` (a constants `VERSION` / short game version) into `(15, 10)`.
pub fn parse_major_minor(raw: &str) -> Option<(u32, u32)> {
    let (major, minor) = raw.trim().split_once('.')?;
    Some((major.parse().ok()?, minor.trim().parse().ok()?))
}

/// Pick which of `available` serves a replay of version `replay`: the newest
/// version `<= replay`. A replay newer than all falls back to the newest; a
/// replay older than all falls back to the oldest. Returns an index into
/// `available`, or `None` when it is empty.
pub fn select_version(available: &[(u32, u32)], replay: (u32, u32)) -> Option<usize> {
    let best = |keep: &dyn Fn(&(u32, u32)) -> bool, newest: bool| {
        let mut found: Option<usize> = None;
        for (i, v) in available.iter().enumerate().filter(|(_, v)| keep(v)) {
            let better = match found {
                None => true,
                Some(j) => (newest && *v > available[j]) || (!newest && *v < available[j]),
            };
            if better {
                found = Some(i);
            }
        }
        found
    };
    best(&|v| *v <= replay, true).or_else(|| best(&|_| true, false))
}

/// The constants files found in a resources directory, by `VERSION`.
#[derive(Debug, Clone, Default)]
pub struct ConstantsSet {
    entries: Vec<((u32, u32), PathBuf)>,
}

impl ConstantsSet {
    pub fn from_entries(entries: Vec<((u32, u32), PathBuf)>) -> Self {
        Self { entries }
    }

    /// Scan `<dir>/constants.json` and `<dir>/constants/*.json`, reading each
    /// file's `VERSION`. Unreadable files and duplicate versions are skipped.
    pub fn scan(dir: &Path) -> Self {
        let mut paths = vec![dir.join("constants.json")];
        if let Ok(read) = std::fs::read_dir(dir.join("constants")) {
            let mut extra: Vec<PathBuf> = read
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect();
            extra.sort();
            paths.extend(extra);
        }
        let mut entries: Vec<((u32, u32), PathBuf)> = Vec::new();
        for path in paths {
            let Some(version) = read_version(&path) else { continue };
            if !entries.iter().any(|(v, _)| *v == version) {
                entries.push((version, path));
            }
        }
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The file serving `replay` and its own `VERSION`.
    pub fn select(&self, replay: (u32, u32)) -> Option<(&(u32, u32), &Path)> {
        let versions: Vec<(u32, u32)> = self.entries.iter().map(|(v, _)| *v).collect();
        let (version, path) = &self.entries[select_version(&versions, replay)?];
        Some((version, path.as_path()))
    }

    /// The newest file (a replay newer than every file gets this one).
    pub fn newest(&self) -> Option<(&(u32, u32), &Path)> {
        self.select((u32::MAX, u32::MAX))
    }

    /// Constants JSON to hand to `GameConstants::for_build` for a scene export.
    /// `None` for replays the toolkit's built-in member maps already cover.
    pub fn scene_overrides(&self, replay: Option<(u32, u32)>) -> Option<serde_json::Value> {
        let replay = replay?;
        if replay <= BUILTIN_MEMBER_MAPS_THROUGH {
            return None;
        }
        let (version, path) = self.select(replay)?;
        if *version <= BUILTIN_MEMBER_MAPS_THROUGH {
            return None;
        }
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }
}

fn read_version(path: &Path) -> Option<(u32, u32)> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    // `VERSION` is `{"VERSION": "15.10", "BUILD": …}` in wows-replay-data files.
    let version = value.get("VERSION")?;
    parse_major_minor(version.get("VERSION").unwrap_or(version).as_str()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVAILABLE: [(u32, u32); 3] = [(15, 9), (15, 10), (15, 4)];

    #[test]
    fn selects_newest_not_newer_than_replay() {
        let pick = |r| AVAILABLE[select_version(&AVAILABLE, r).unwrap()];
        assert_eq!(pick((15, 10)), (15, 10));
        assert_eq!(pick((15, 9)), (15, 9));
        assert_eq!(pick((15, 8)), (15, 4));
        assert_eq!(pick((14, 12)), (15, 4));
        assert_eq!(pick((15, 11)), (15, 10));
        assert_eq!(pick((16, 0)), (15, 10));
        assert_eq!(select_version(&[], (15, 9)), None);
    }

    #[test]
    fn parses_major_minor() {
        assert_eq!(parse_major_minor("15.10"), Some((15, 10)));
        assert_eq!(parse_major_minor("15"), None);
        assert_eq!(parse_major_minor("x.y"), None);
    }

    fn write(path: &Path, version: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("{{\"VERSION\":{{\"VERSION\":\"{version}\",\"BUILD\":1}},\"PLAYER_NUM_MEMBER_MAP\":{{}}}}")).unwrap();
    }

    #[test]
    fn scan_reads_version_from_each_file_and_selects() {
        let dir = std::env::temp_dir().join(format!("tfd-constants-set-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir.join("constants.json"), "15.10");
        write(&dir.join("constants").join("15.9.json"), "15.9");
        let set = ConstantsSet::scan(&dir);
        assert_eq!(set.newest().unwrap().0, &(15, 10));
        assert_eq!(set.select((15, 6)).unwrap().0, &(15, 9));
        assert_eq!(set.select((15, 9)).unwrap().1, dir.join("constants").join("15.9.json"));
        assert_eq!(set.select((15, 10)).unwrap().1, dir.join("constants.json"));
        // Scene: toolkit built-ins cover <= 15.9; 15.10 takes the override.
        assert!(set.scene_overrides(Some((15, 9))).is_none());
        assert!(set.scene_overrides(None).is_none());
        assert!(set.scene_overrides(Some((15, 10))).is_some());
        assert!(set.scene_overrides(Some((15, 11))).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
