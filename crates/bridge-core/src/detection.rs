/// WoWS replays-folder detection.
///
/// All OS-specific "where do I look?" paths are passed in via `SearchRoots`
/// so that the logic is unit-testable on macOS with fixture trees.
use crate::vdf;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum DetectError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("XML parse error: {0}")]
    Xml(String),
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// The outcome of an auto-detection attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedPath {
    /// Absolute path to the replays folder.
    pub path: PathBuf,
    /// Where the path was found.
    pub source: DetectSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DetectSource {
    /// Found via Steam `libraryfolders.vdf` + `preferences.xml`.
    Steam,
    /// Found via Wargaming Game Center `preferences.xml`.
    WargamingGameCenter,
    /// Fallback: `<install>/replays/` derived from Steam install without preferences.xml.
    SteamFallback,
}

/// Injectable search roots — all detection code uses this struct instead of
/// hardcoded Windows paths, so tests can pass a fixture tree on any OS.
#[derive(Debug, Clone)]
pub struct SearchRoots {
    /// Paths to check for `Steam/steamapps/libraryfolders.vdf`.
    /// On Windows: `C:\Program Files (x86)\Steam`, `C:\Program Files\Steam`, etc.
    pub steam_roots: Vec<PathBuf>,
    /// Paths to check for Wargaming Game Center `preferences.xml`.
    /// On Windows: `%APPDATA%\Wargaming.net\GameCenter`, etc.
    pub wgc_roots: Vec<PathBuf>,
}

impl SearchRoots {
    /// Returns the default Windows search roots using well-known paths.
    /// Not used in tests; tests pass explicit roots.
    pub fn windows_defaults() -> Self {
        SearchRoots {
            steam_roots: vec![
                PathBuf::from(r"C:\Program Files (x86)\Steam"),
                PathBuf::from(r"C:\Program Files\Steam"),
            ],
            // WGC installs WoWS to a user-chosen dir; the default is C:\Games\World_of_Warships.
            // The replaysSettings dir value lives in the game's own preferences.xml, NOT in the
            // WGC config dir (C:\ProgramData\Wargaming.net\GameCenter). We therefore list common
            // WoWS game-install roots here. Programmatic WGC-metadata discovery (reading the WGC
            // config to find an arbitrary install dir) is not implemented; the manual picker
            // covers custom installs (AC#2). Verify on Windows T5.
            wgc_roots: wgc_game_install_defaults(),
        }
    }
}

fn wgc_game_install_defaults() -> Vec<PathBuf> {
    // C:\Games\World_of_Warships is the canonical WGC default install location.
    // Additional common variants included for coverage.
    vec![
        PathBuf::from(r"C:\Games\World_of_Warships"),
        PathBuf::from(r"C:\Games\WorldOfWarships"),
        PathBuf::from(r"C:\Wargaming.net\WorldOfWarships"),
        PathBuf::from(r"C:\Wargaming.net\World_of_Warships"),
    ]
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Run auto-detection using the provided search roots.
///
/// Returns a list of all candidate replays folders found, in priority order
/// (Steam via preferences.xml first, then Wargaming GC, then Steam fallback).
pub fn detect_replays_paths(roots: &SearchRoots) -> Vec<DetectedPath> {
    let mut results = Vec::new();

    // 1. Steam — parse libraryfolders.vdf, then preferences.xml
    results.extend(detect_steam(roots));

    // 2. Wargaming Game Center — parse preferences.xml directly
    results.extend(detect_wgc(roots));

    results
}

/// Validate that a given path looks like a WoWS replays directory.
///
/// We accept it if:
/// - The directory exists, AND
/// - It contains at least one `.wowsreplay` file  OR  the directory name is
///   "replays" (empty fresh install).
pub fn validate_replays_folder(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    let name_ok = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase().contains("replays"))
        .unwrap_or(false);
    if name_ok {
        return true;
    }
    // Also accept if there's any .wowsreplay file inside
    has_replay_file(path)
}

/// The outcome of resolving a user-picked folder to the canonical replays dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedReplays {
    /// The canonical replays folder to store.
    pub path: PathBuf,
    /// True when `path` differs from the picked folder because we descended
    /// into a `replays` child or ascended to a `replays` ancestor.
    pub corrected: bool,
}

/// Resolve a user-picked folder to the canonical WoWS replays folder.
///
/// People mis-pick in two predictable ways; this fixes both transparently:
///  1. The pick already *is* a replays folder (its name contains "replays")
///     → keep it as-is.
///  2. The pick is the WoWS **game folder** that holds a `replays` child
///     (e.g. `…\World_of_Warships`) → descend into that child.
///  3. The pick is a folder **inside** replays — typically a version-sorter
///     mod's subfolder (`…\replays\15.4.0.0`) → ascend to the nearest
///     `replays` ancestor that contains them all.
///
/// Returns `None` when the pick is not a directory or has no structural
/// relationship to a replays folder; the caller then surfaces the normal
/// "not a replays directory" error. A custom-named folder that directly holds
/// `.wowsreplay` files is accepted as-is (`corrected = false`), preserving the
/// lenient behaviour of [`validate_replays_folder`].
pub fn resolve_replays_folder(picked: &Path) -> Option<ResolvedReplays> {
    if !picked.is_dir() {
        return None;
    }

    // 1. The pick itself looks like the replays folder.
    if dir_name_is_replays(picked) {
        return Some(ResolvedReplays {
            path: picked.to_path_buf(),
            corrected: false,
        });
    }

    // 2. Game folder: it holds a `replays` child directory.
    if let Some(child) = replays_child(picked) {
        return Some(ResolvedReplays {
            path: child,
            corrected: true,
        });
    }

    // 3. Subfolder inside replays: walk up to the nearest `replays` ancestor.
    if let Some(ancestor) = nearest_replays_ancestor(picked) {
        return Some(ResolvedReplays {
            path: ancestor,
            corrected: true,
        });
    }

    // 4. Lenient fallback: a custom-named folder that directly holds replays.
    if has_replay_file(picked) {
        return Some(ResolvedReplays {
            path: picked.to_path_buf(),
            corrected: false,
        });
    }

    None
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn detect_steam(roots: &SearchRoots) -> Vec<DetectedPath> {
    let mut results = Vec::new();

    for steam_root in &roots.steam_roots {
        let vdf_path = steam_root.join("steamapps").join("libraryfolders.vdf");
        let content = match std::fs::read_to_string(&vdf_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let library_paths = match vdf::parse_library_folders(&content) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let wows_install = match vdf::find_wows_in_libraries(&library_paths) {
            Some(p) => p,
            None => continue,
        };

        // Try preferences.xml first
        let prefs_path = wows_install.join("preferences.xml");
        if prefs_path.exists() {
            let pref_results = parse_preferences_xml(&prefs_path, DetectSource::Steam);
            if !pref_results.is_empty() {
                results.extend(pref_results);
                continue;
            }
        }

        // Fallback: use <install>/replays/
        let fallback = vdf::default_replays_from_install(&wows_install);
        if fallback.is_dir() {
            results.push(DetectedPath {
                path: fallback,
                source: DetectSource::SteamFallback,
            });
        }
    }

    results
}

fn detect_wgc(roots: &SearchRoots) -> Vec<DetectedPath> {
    let mut results = Vec::new();

    for wgc_root in &roots.wgc_roots {
        let prefs_path = wgc_root.join("preferences.xml");
        if prefs_path.exists() {
            results.extend(parse_preferences_xml(
                &prefs_path,
                DetectSource::WargamingGameCenter,
            ));
        }
    }

    results
}

/// Parse a WoWS `preferences.xml` and extract replays folder path(s).
///
/// The file can contain multiple `<scriptsPreferences>` blocks (one per
/// server/region). We collect all distinct replays paths found.
///
/// Relevant structure (simplified):
/// ```xml
/// <root>
///   <scriptsPreferences>
///     <record name="scriptsPreferences">
///       <value name="replaysSettings">
///         <value name="version">...</value>
///         <value name="dir">replays</value>
///         <!-- dir may be relative to the install dir or absolute -->
///       </value>
///     </record>
///   </scriptsPreferences>
/// </root>
/// ```
///
/// We use a small hand-rolled parser so we don't need a heavy XML dependency.
fn parse_preferences_xml(path: &Path, source: DetectSource) -> Vec<DetectedPath> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let install_dir = path.parent().unwrap_or(Path::new(""));
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Walk all `<value name="dir">...</value>` entries that look like replay paths.
    // This is intentionally lenient — the XML structure has varied across WoWS versions.
    for dir_value in extract_xml_values(&content, "dir") {
        if dir_value.is_empty() {
            continue;
        }
        let candidate = if std::path::Path::new(&dir_value).is_absolute() {
            PathBuf::from(&dir_value)
        } else {
            install_dir.join(&dir_value)
        };

        // Normalise and deduplicate
        let canonical = candidate.to_string_lossy().to_string();
        if seen.contains(&canonical) {
            continue;
        }

        // Only include if it actually exists or its name suggests replays
        if candidate.is_dir() || canonical.to_lowercase().contains("replays") {
            seen.insert(canonical);
            results.push(DetectedPath {
                path: candidate,
                source: source.clone(),
            });
        }
    }

    results
}

/// Extract the text content of all `<value name="dir">TEXT</value>` elements.
fn extract_xml_values(xml: &str, attr_name: &str) -> Vec<String> {
    let needle = format!(r#"name="{}""#, attr_name);
    let mut values = Vec::new();
    let mut search = xml;

    while let Some(tag_start) = search.find(&needle) {
        // Find the end of the opening tag
        let from = &search[tag_start..];
        if let Some(gt) = from.find('>') {
            let after_tag = &from[gt + 1..];
            // Self-closing? (<value ... />)
            if from[..gt].ends_with('/') {
                search = after_tag;
                continue;
            }
            // Extract up to the next closing tag
            if let Some(close) = after_tag.find("</") {
                let text = after_tag[..close].trim().to_string();
                values.push(text);
                search = &after_tag[close..];
            } else {
                break;
            }
        } else {
            break;
        }
    }

    values
}

/// Derive the WoWS game directory from a replays path.
///
/// Strategy: walk up from `replays_path` looking for an ancestor component
/// whose name is "replays" (case-insensitive).  The game directory is that
/// ancestor's parent.  If `replays_path` itself is named "replays", the
/// game dir is its parent.  Fallback: return `replays_path.parent()` and
/// log a warning.
///
/// Examples:
/// - `C:\Games\World_of_Warships\replays` → `C:\Games\World_of_Warships`
/// - `C:\Games\World_of_Warships\replays\15.4.0.0` → `C:\Games\World_of_Warships`
pub fn derive_game_dir(replays_path: &Path) -> Option<PathBuf> {
    // Walk path components from root to find the "replays" ancestor.
    // We look at each prefix: if the last component is "replays", its parent
    // is the game directory.
    let components: Vec<_> = replays_path.components().collect();
    for i in (0..components.len()).rev() {
        let name = components[i].as_os_str().to_string_lossy();
        if name.to_lowercase() == "replays" {
            // Reconstruct path up to (but not including) the replays component.
            let game_dir: PathBuf = components[..i].iter().collect();
            if game_dir.as_os_str().is_empty() {
                break;
            }
            return Some(game_dir);
        }
    }
    // Fallback: parent of the provided path.
    log::warn!(
        "derive_game_dir: could not find 'replays' component in '{}', falling back to parent",
        replays_path.display()
    );
    replays_path.parent().map(PathBuf::from)
}

/// True if the dir's own name contains "replays" (case-insensitive) — the same
/// name test [`validate_replays_folder`] uses.
fn dir_name_is_replays(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase().contains("replays"))
        .unwrap_or(false)
}

/// Find a direct child directory whose name looks like a replays folder.
///
/// Uses a directory scan rather than `join("replays")` so the match is
/// case-insensitive even on case-sensitive filesystems (test fixtures on
/// macOS). An exact "replays" match is preferred over names that merely
/// contain it; ties break on the lowercased name for deterministic results.
fn replays_child(parent: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(parent)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && dir_name_is_replays(p))
        .collect();
    candidates.sort_by_key(|p| {
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase());
        let exact = name.as_deref() == Some("replays");
        (!exact, name.unwrap_or_default())
    });
    candidates.into_iter().next()
}

/// Walk up from `path` to the nearest ancestor whose name looks like a replays
/// folder (case-insensitive). Returns that ancestor, or `None` if there is one.
fn nearest_replays_ancestor(path: &Path) -> Option<PathBuf> {
    let mut current = path.parent();
    while let Some(dir) = current {
        if dir_name_is_replays(dir) {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

fn has_replay_file(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .ok()
        .map(|entries| {
            entries.flatten().any(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext.eq_ignore_ascii_case("wowsreplay"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Build a fixture Steam tree:
    ///
    /// ```
    /// <tmp>/
    ///   steamapps/
    ///     libraryfolders.vdf   (points to <tmp>/library)
    ///   library/
    ///     steamapps/
    ///       common/
    ///         World of Warships/
    ///           replays/
    ///           preferences.xml (optional)
    /// ```
    fn make_steam_fixture(tmp: &TempDir, prefs_xml: Option<&str>) -> (PathBuf, PathBuf) {
        let steam_root = tmp.path().join("steam");
        let library_root = tmp.path().join("library");
        let wows_root = library_root
            .join("steamapps")
            .join("common")
            .join("World of Warships");
        let replays_dir = wows_root.join("replays");

        fs::create_dir_all(steam_root.join("steamapps")).unwrap();
        fs::create_dir_all(&replays_dir).unwrap();

        // Write libraryfolders.vdf
        let vdf_content = format!(
            r#""libraryfolders"
{{
    "1"
    {{
        "path"    "{}"
    }}
}}"#,
            library_root.to_string_lossy().replace('\\', "\\\\")
        );
        fs::write(
            steam_root.join("steamapps").join("libraryfolders.vdf"),
            vdf_content,
        )
        .unwrap();

        if let Some(xml) = prefs_xml {
            fs::write(wows_root.join("preferences.xml"), xml).unwrap();
        }

        (steam_root, replays_dir)
    }

    const PREFS_XML_REL: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<root>
  <scriptsPreferences>
    <record name="scriptsPreferences">
      <value name="replaysSettings">
        <value name="dir">replays</value>
      </value>
    </record>
  </scriptsPreferences>
</root>"#;

    const PREFS_XML_ABS_TEMPLATE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<root>
  <scriptsPreferences>
    <record name="scriptsPreferences">
      <value name="replaysSettings">
        <value name="dir">REPLAYS_PATH</value>
      </value>
    </record>
  </scriptsPreferences>
</root>"#;

    #[test]
    fn steam_detection_via_prefs_relative_path() {
        let tmp = TempDir::new().unwrap();
        let (steam_root, expected_replays) = make_steam_fixture(&tmp, Some(PREFS_XML_REL));

        let roots = SearchRoots {
            steam_roots: vec![steam_root],
            wgc_roots: vec![],
        };
        let results = detect_replays_paths(&roots);
        assert!(!results.is_empty(), "Should detect at least one path");
        assert_eq!(results[0].source, DetectSource::Steam);
        // The detected path should point to <wows>/replays
        assert_eq!(results[0].path, expected_replays);
    }

    #[test]
    fn steam_fallback_when_no_prefs() {
        let tmp = TempDir::new().unwrap();
        let (steam_root, expected_replays) = make_steam_fixture(&tmp, None);

        let roots = SearchRoots {
            steam_roots: vec![steam_root],
            wgc_roots: vec![],
        };
        let results = detect_replays_paths(&roots);
        assert!(!results.is_empty(), "Should fall back to install/replays");
        assert_eq!(results[0].source, DetectSource::SteamFallback);
        assert_eq!(results[0].path, expected_replays);
    }

    #[test]
    fn steam_detection_absolute_path_in_prefs() {
        let tmp = TempDir::new().unwrap();
        let abs_replays = tmp.path().join("custom_replays");
        fs::create_dir_all(&abs_replays).unwrap();

        let xml = PREFS_XML_ABS_TEMPLATE.replace("REPLAYS_PATH", &abs_replays.to_string_lossy());
        let (steam_root, _) = make_steam_fixture(&tmp, Some(&xml));

        let roots = SearchRoots {
            steam_roots: vec![steam_root],
            wgc_roots: vec![],
        };
        let results = detect_replays_paths(&roots);
        assert!(!results.is_empty());
        assert_eq!(results[0].path, abs_replays);
    }

    #[test]
    fn no_steam_root_returns_empty() {
        let roots = SearchRoots {
            steam_roots: vec![PathBuf::from("/nonexistent/steam")],
            wgc_roots: vec![],
        };
        assert!(detect_replays_paths(&roots).is_empty());
    }

    #[test]
    fn wgc_detection_via_prefs() {
        let tmp = TempDir::new().unwrap();
        let wgc_root = tmp.path().join("wgc");
        let wows_install = tmp.path().join("wows_wgc");
        let replays_dir = wows_install.join("replays");
        fs::create_dir_all(&wgc_root).unwrap();
        fs::create_dir_all(&replays_dir).unwrap();

        let xml = PREFS_XML_ABS_TEMPLATE.replace("REPLAYS_PATH", &replays_dir.to_string_lossy());
        fs::write(wgc_root.join("preferences.xml"), xml).unwrap();

        let roots = SearchRoots {
            steam_roots: vec![],
            wgc_roots: vec![wgc_root],
        };
        let results = detect_replays_paths(&roots);
        assert!(!results.is_empty());
        assert_eq!(results[0].source, DetectSource::WargamingGameCenter);
        assert_eq!(results[0].path, replays_dir);
    }

    #[test]
    fn validate_replays_folder_named_replays() {
        let tmp = TempDir::new().unwrap();
        let replays = tmp.path().join("replays");
        fs::create_dir_all(&replays).unwrap();
        assert!(validate_replays_folder(&replays));
    }

    #[test]
    fn validate_replays_folder_with_replay_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("my_folder");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("battle.wowsreplay"), b"fake").unwrap();
        assert!(validate_replays_folder(&dir));
    }

    #[test]
    fn validate_replays_folder_rejects_nonexistent() {
        assert!(!validate_replays_folder(Path::new("/does/not/exist")));
    }

    #[test]
    fn validate_replays_folder_rejects_unrelated_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("documents");
        fs::create_dir_all(&dir).unwrap();
        // no replay files, name doesn't contain "replays"
        assert!(!validate_replays_folder(&dir));
    }

    // ── derive_game_dir tests ─────────────────────────────────────────────────

    #[test]
    fn derive_game_dir_from_replays_subfolder() {
        // replays_path is a version subfolder under replays/
        let path = Path::new(r"C:\Games\World_of_Warships\replays\15.4.0.0");
        let result = derive_game_dir(path).expect("should derive game dir");
        assert_eq!(result, Path::new(r"C:\Games\World_of_Warships"));
    }

    #[test]
    fn derive_game_dir_from_replays_itself() {
        // replays_path IS the replays directory
        let path = Path::new(r"C:\Games\World_of_Warships\replays");
        let result = derive_game_dir(path).expect("should derive game dir");
        assert_eq!(result, Path::new(r"C:\Games\World_of_Warships"));
    }

    #[test]
    fn derive_game_dir_case_insensitive() {
        // "Replays" with capital R
        let path = Path::new(r"C:\Games\World_of_Warships\Replays");
        let result = derive_game_dir(path).expect("should derive game dir");
        assert_eq!(result, Path::new(r"C:\Games\World_of_Warships"));
    }

    #[test]
    fn derive_game_dir_fallback_when_no_replays_component() {
        // Path has no "replays" component — fallback to parent
        let path = Path::new(r"C:\SomePath\battles\15.4.0.0");
        let result = derive_game_dir(path);
        // Should return the parent as fallback
        assert!(result.is_some());
        assert_eq!(result.unwrap(), Path::new(r"C:\SomePath\battles"));
    }

    // ── resolve_replays_folder tests ─────────────────────────────────────────

    #[test]
    fn resolve_keeps_replays_folder_unchanged() {
        let tmp = TempDir::new().unwrap();
        let replays = tmp.path().join("replays");
        fs::create_dir_all(&replays).unwrap();

        let r = resolve_replays_folder(&replays).expect("should resolve");
        assert_eq!(r.path, replays);
        assert!(!r.corrected, "picking replays itself is not a correction");
    }

    #[test]
    fn resolve_descends_into_replays_child_of_game_folder() {
        // User picked the game folder; it holds a `replays` child.
        let tmp = TempDir::new().unwrap();
        let game = tmp.path().join("World_of_Warships");
        let replays = game.join("replays");
        fs::create_dir_all(&replays).unwrap();
        // A sibling dir that must NOT be chosen.
        fs::create_dir_all(game.join("bin")).unwrap();

        let r = resolve_replays_folder(&game).expect("should resolve");
        assert_eq!(r.path, replays);
        assert!(r.corrected, "descending into the child is a correction");
    }

    #[test]
    fn resolve_descends_case_insensitively() {
        // Game folder with a capital-R `Replays` child (case-insensitive match).
        let tmp = TempDir::new().unwrap();
        let game = tmp.path().join("WorldOfWarships");
        let replays = game.join("Replays");
        fs::create_dir_all(&replays).unwrap();

        let r = resolve_replays_folder(&game).expect("should resolve");
        assert_eq!(r.path, replays);
        assert!(r.corrected);
    }

    #[test]
    fn resolve_ascends_from_version_subfolder() {
        // Version-sorter mod created `replays/15.4.0.0`; user picked that.
        let tmp = TempDir::new().unwrap();
        let replays = tmp.path().join("replays");
        let version = replays.join("15.4.0.0");
        fs::create_dir_all(&version).unwrap();

        let r = resolve_replays_folder(&version).expect("should resolve");
        assert_eq!(r.path, replays);
        assert!(r.corrected, "ascending to the parent is a correction");
    }

    #[test]
    fn resolve_ascends_through_nested_subfolders() {
        // Picked a directory two levels below replays.
        let tmp = TempDir::new().unwrap();
        let replays = tmp.path().join("replays");
        let nested = replays.join("15.4.0.0").join("ranked");
        fs::create_dir_all(&nested).unwrap();

        let r = resolve_replays_folder(&nested).expect("should resolve");
        assert_eq!(r.path, replays);
        assert!(r.corrected);
    }

    #[test]
    fn resolve_keeps_custom_folder_holding_replay_files() {
        // A non-"replays"-named folder that directly contains a .wowsreplay.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("battles");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("battle.wowsreplay"), b"fake").unwrap();

        let r = resolve_replays_folder(&dir).expect("should resolve");
        assert_eq!(r.path, dir);
        assert!(!r.corrected);
    }

    #[test]
    fn resolve_rejects_unrelated_folder() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("documents");
        fs::create_dir_all(&dir).unwrap();
        assert!(resolve_replays_folder(&dir).is_none());
    }

    #[test]
    fn resolve_rejects_nonexistent_path() {
        assert!(resolve_replays_folder(Path::new("/does/not/exist")).is_none());
    }

    #[test]
    fn resolve_prefers_exact_replays_child_over_lookalike() {
        // Game folder holds both `replays` and `replays_backup`; pick the exact.
        let tmp = TempDir::new().unwrap();
        let game = tmp.path().join("game");
        let replays = game.join("replays");
        fs::create_dir_all(&replays).unwrap();
        fs::create_dir_all(game.join("replays_backup")).unwrap();

        let r = resolve_replays_folder(&game).expect("should resolve");
        assert_eq!(r.path, replays);
        assert!(r.corrected);
    }

    #[test]
    fn multiple_regions_parsed_from_prefs() {
        // preferences.xml with two dir entries (simulating multiple server profiles)
        let tmp = TempDir::new().unwrap();
        let wgc_root = tmp.path().join("wgc");
        let replays_eu = tmp.path().join("replays_eu");
        let replays_na = tmp.path().join("replays_na");
        fs::create_dir_all(&wgc_root).unwrap();
        fs::create_dir_all(&replays_eu).unwrap();
        fs::create_dir_all(&replays_na).unwrap();

        let xml = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<root>
  <scriptsPreferences>
    <record name="EU">
      <value name="replaysSettings">
        <value name="dir">{}</value>
      </value>
    </record>
    <record name="NA">
      <value name="replaysSettings">
        <value name="dir">{}</value>
      </value>
    </record>
  </scriptsPreferences>
</root>"#,
            replays_eu.to_string_lossy(),
            replays_na.to_string_lossy()
        );
        fs::write(wgc_root.join("preferences.xml"), xml).unwrap();

        let roots = SearchRoots {
            steam_roots: vec![],
            wgc_roots: vec![wgc_root],
        };
        let results = detect_replays_paths(&roots);
        assert_eq!(results.len(), 2, "Should detect both region paths");
    }
}
