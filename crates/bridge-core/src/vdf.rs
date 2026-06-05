/// Minimal VDF (Valve Data Format) parser, just enough to extract Steam
/// library folder paths from `libraryfolders.vdf`.
///
/// We only need string key/value pairs; nested blocks are tracked for depth
/// but not stored. No allocations beyond the returned paths.
use crate::detection::DetectError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Parse `libraryfolders.vdf` content and return all library paths found
/// under the top-level `libraryfolders` block.
///
/// The VDF format we care about looks like:
/// ```text
/// "libraryfolders"
/// {
///     "1"
///     {
///         "path"    "D:\\SteamLibrary"
///         ...
///     }
/// }
/// ```
pub fn parse_library_folders(content: &str) -> Result<Vec<PathBuf>, DetectError> {
    let mut paths = Vec::new();
    let mut depth: usize = 0;
    let mut inside_entry = false;
    let mut pending_key: Option<String> = None;
    let mut current_entry: HashMap<String, String> = HashMap::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();

        if line == "{" {
            depth += 1;
            if depth == 2 {
                // Entering a numbered library entry block
                inside_entry = true;
                current_entry.clear();
            }
            pending_key = None;
            continue;
        }
        if line == "}" {
            if depth == 2 && inside_entry {
                // Leaving a library entry block; collect its "path" key
                if let Some(p) = current_entry.get("path") {
                    let p = p.replace("\\\\", "/").replace('\\', "/");
                    paths.push(PathBuf::from(p));
                }
                inside_entry = false;
                current_entry.clear();
            }
            depth = depth.saturating_sub(1);
            pending_key = None;
            continue;
        }

        // Quoted token extraction
        let tokens = extract_quoted_tokens(line);
        match tokens.len() {
            1 => {
                // Single quoted string — upcoming key or block label
                pending_key = Some(tokens[0].clone());
            }
            2 => {
                let (k, v) = (&tokens[0], &tokens[1]);
                if inside_entry && depth == 2 {
                    current_entry.insert(k.to_lowercase(), v.clone());
                }
                pending_key = None;
            }
            _ => {
                let _ = pending_key.take();
            }
        }
    }

    Ok(paths)
}

/// Extract up to 2 double-quoted string values from a single VDF line.
fn extract_quoted_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '"' {
            let mut token = String::new();
            let mut escaped = false;
            for ch in chars.by_ref() {
                if escaped {
                    token.push(ch);
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    break;
                } else {
                    token.push(ch);
                }
            }
            tokens.push(token);
            if tokens.len() == 2 {
                break;
            }
        }
    }
    tokens
}

/// Given a list of Steam library roots, find the World of Warships install
/// directory if it exists.
pub fn find_wows_in_libraries(library_roots: &[PathBuf]) -> Option<PathBuf> {
    for root in library_roots {
        let candidate = root.join("steamapps").join("common").join("World of Warships");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// Derive the replays folder path from a WoWS install root.
///
/// Steam WoWS uses `<install>/replays/` by default.  The real canonical path
/// is read from `preferences.xml`, but this is the fallback used when that
/// file is absent or unparseable.
pub fn default_replays_from_install(install_root: &Path) -> PathBuf {
    install_root.join("replays")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_VDF: &str = r#"
"libraryfolders"
{
    "0"
    {
        "path"    "C:\\Program Files (x86)\\Steam"
        "label"   ""
    }
    "1"
    {
        "path"    "D:\\SteamLibrary"
        "label"   "Games"
    }
}
"#;

    #[test]
    fn parse_extracts_two_paths() {
        let paths = parse_library_folders(SAMPLE_VDF).unwrap();
        assert_eq!(paths.len(), 2);
        // Backslashes normalised to forward slashes
        assert!(paths[0].to_string_lossy().contains("Steam"));
        assert!(paths[1].to_string_lossy().contains("SteamLibrary"));
    }

    #[test]
    fn parse_empty_vdf_returns_empty() {
        let paths = parse_library_folders("").unwrap();
        assert!(paths.is_empty());
    }

    #[test]
    fn find_wows_returns_none_when_absent() {
        let roots = vec![PathBuf::from("/nonexistent/path")];
        assert!(find_wows_in_libraries(&roots).is_none());
    }

    #[test]
    fn default_replays_path() {
        let root = PathBuf::from("/steam/games/World of Warships");
        assert_eq!(
            default_replays_from_install(&root),
            PathBuf::from("/steam/games/World of Warships/replays")
        );
    }
}
