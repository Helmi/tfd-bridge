//! Replay battle-result decoder (td-28d9e7).
//!
//! Decodes a `.wowsreplay` file by shelling out to the `replayshark` sidecar,
//! parsing its JSONL dump output, and resolving the positional player arrays
//! using `constants.json` + `ship_index.json` into a structured [`BattleData`].
//!
//! # Design notes
//! - Pure Tauri-free module; all path resolution happens in the caller.
//! - `decode_battle_result` is the main entry point; `resolve_player` is
//!   `pub(crate)` so unit tests can call it directly with synthetic data.
//! - Tolerant: null / missing index / wrong type → `None`, never panics on
//!   a single bad field.
//! - Timeout watchdog via a background thread that kills the child process.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

// ── Known-good version set ─────────────────────────────────────────────────────

/// Pairs (major, minor) for which constants.json + sidecar are confirmed good.
const KNOWN_GOOD: &[(u32, u32)] = &[(15, 3), (15, 4)];

// ── Output structs ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct BattleData {
    pub meta: BattleMeta,
    pub players: Vec<BattlePlayer>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BattleMeta {
    pub schema_version: String,
    pub arena_unique_id: i64,
    pub map_name: String,
    pub game_version: String,
    pub game_version_short: Option<String>,
    pub match_group: Option<String>,
    pub duration_seconds: Option<i64>,
    pub winner_team: Option<i64>,
    pub battle_time: Option<i64>,
    pub source_file_hash: String,
    pub owner_account_db_id: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BattlePlayer {
    pub account_db_id: i64,
    pub player_name: Option<String>,
    pub clan_id: Option<i64>,
    pub clan_tag: Option<String>,
    pub ship_id: Option<i64>,
    pub ship_name: Option<String>,
    pub ship_tier: Option<i64>,
    pub ship_class: Option<ShipClass>,
    pub team_id: Option<i64>,
    pub prebattle_id: Option<i64>,
    pub exp: Option<i64>,
    pub raw_exp: Option<i64>,
    pub damage_dealt: Option<i64>,
    pub damage_to_buildings: Option<i64>,
    pub damage_potential: Option<i64>,
    pub shots_fired: Option<i64>,
    pub hits: Option<i64>,
    pub frags: Option<i64>,
    pub xp_contribution: Option<f64>,
    pub ribbons_torpedo_hits: Option<i64>,
    pub ribbons_plane_kills: Option<i64>,
    pub ribbons_hits: Option<i64>,
    pub afk: Option<bool>,
    pub survived: Option<bool>,
    pub is_self: bool,
    pub won: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShipClass {
    AirCarrier,
    Battleship,
    Cruiser,
    Destroyer,
    Submarine,
    Auxiliary,
}

impl ShipClass {
    fn from_species(species: &str) -> Self {
        match species {
            "AirCarrier" => ShipClass::AirCarrier,
            "Battleship" => ShipClass::Battleship,
            "Cruiser" => ShipClass::Cruiser,
            "Destroyer" => ShipClass::Destroyer,
            "Submarine" => ShipClass::Submarine,
            _ => ShipClass::Auxiliary,
        }
    }
}

// ── Config + tables ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DecodeConfig {
    pub sidecar_path: PathBuf,
    pub game_dir: PathBuf,
    pub constants_path: PathBuf,
    pub ship_index_path: PathBuf,
    pub timeout: Duration,
}

pub struct Tables {
    pub public_indices: HashMap<String, usize>,
    pub common_results: Vec<String>,
    pub interaction_details: Vec<String>,
    pub private_results: Vec<String>,
    pub ships: HashMap<String, ShipInfo>,
}

pub struct ShipInfo {
    pub index: String,
    pub level: i64,
    pub species: String,
}

impl Tables {
    pub fn load(constants_path: &Path, ship_index_path: &Path) -> Result<Tables, DecodeError> {
        // ── constants.json ────────────────────────────────────────────────────
        let constants_bytes = fs::read(constants_path)
            .map_err(|e| DecodeError::Resources(format!("cannot read constants.json: {e}")))?;
        let constants: serde_json::Value = serde_json::from_slice(&constants_bytes)
            .map_err(|e| DecodeError::Resources(format!("constants.json parse error: {e}")))?;

        // CLIENT_PUBLIC_RESULTS_INDICES: object {name → index}
        let pub_obj = constants
            .get("CLIENT_PUBLIC_RESULTS_INDICES")
            .and_then(|v| v.as_object())
            .ok_or_else(|| {
                DecodeError::Resources(
                    "constants.json missing CLIENT_PUBLIC_RESULTS_INDICES".into(),
                )
            })?;
        let mut public_indices = HashMap::new();
        for (k, v) in pub_obj {
            if let Some(idx) = v.as_u64() {
                public_indices.insert(k.clone(), idx as usize);
            }
        }

        // COMMON_RESULTS: ordered array of field names
        let common_results = constants
            .get("COMMON_RESULTS")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                DecodeError::Resources("constants.json missing COMMON_RESULTS array".into())
            })?
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();

        // CLIENT_VEH_INTERACTION_DETAILS: ordered array
        let interaction_details = constants
            .get("CLIENT_VEH_INTERACTION_DETAILS")
            .and_then(|v| v.as_array())
            .unwrap_or(&vec![])
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();

        // PLAYER_PRIVATE_RESULTS: ordered array (is_afk at index 37)
        let private_results = constants
            .get("PLAYER_PRIVATE_RESULTS")
            .and_then(|v| v.as_array())
            .unwrap_or(&vec![])
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();

        // ── ship_index.json ───────────────────────────────────────────────────
        let ship_bytes = fs::read(ship_index_path)
            .map_err(|e| DecodeError::Resources(format!("cannot read ship_index.json: {e}")))?;
        let ship_json: serde_json::Value = serde_json::from_slice(&ship_bytes)
            .map_err(|e| DecodeError::Resources(format!("ship_index.json parse error: {e}")))?;

        let mut ships = HashMap::new();
        if let Some(obj) = ship_json.as_object() {
            for (id_str, info) in obj {
                let index = info
                    .get("index")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let level = info.get("level").and_then(|v| v.as_i64()).unwrap_or(0);
                let species = info
                    .get("species")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                ships.insert(
                    id_str.clone(),
                    ShipInfo {
                        index,
                        level,
                        species,
                    },
                );
            }
        }

        Ok(Tables {
            public_indices,
            common_results,
            interaction_details,
            private_results,
            ships,
        })
    }
}

// ── Error ──────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("sidecar not found at {0}")]
    SidecarMissing(PathBuf),
    #[error("resource error: {0}")]
    Resources(String),
    #[error("sidecar spawn failed: {0}")]
    SidecarSpawn(#[source] std::io::Error),
    #[error("sidecar exited with status {code:?}: {stderr}")]
    SidecarFailed { code: Option<i32>, stderr: String },
    #[error("sidecar timed out after {0:?}")]
    Timeout(Duration),
    #[error("no BattleResults packet (early quit / live / non-pvp)")]
    NoBattleResults,
    #[error("malformed dump output: {0}")]
    Malformed(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ── SHA-256 (identical to src-tauri/uploader.rs::sha256_hex) ──────────────────

/// Lowercase hex SHA-256 of `bytes` — the source_file_hash and donation dedup key.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// ── Temp file drop guard ───────────────────────────────────────────────────────

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

// ── Main decode function ───────────────────────────────────────────────────────

/// Decode a `.wowsreplay` file into structured [`BattleData`].
///
/// Pre-checks the sidecar exists, reads replay bytes for the source hash,
/// runs the sidecar with a timeout watchdog, parses the JSONL output, and
/// resolves players via `tables`.
pub fn decode_battle_result(
    replay_path: &Path,
    cfg: &DecodeConfig,
    tables: &Tables,
) -> Result<BattleData, DecodeError> {
    // Pre-check: sidecar exists.
    if !cfg.sidecar_path.exists() {
        return Err(DecodeError::SidecarMissing(cfg.sidecar_path.clone()));
    }

    // Read replay bytes for the SHA-256 hash.
    let replay_bytes = fs::read(replay_path)?;
    let source_file_hash = sha256_hex(&replay_bytes);
    drop(replay_bytes); // release memory; we only needed the hash

    // Unique temp file for the JSONL dump. A process-static monotonic counter
    // (combined with the PID) guarantees a distinct path per call even under
    // concurrent decodes — independent of wall-clock resolution.
    static DUMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = DUMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_path = std::env::temp_dir().join(format!(
        "tfd-bridge-dump-{}-{}.jsonl",
        std::process::id(),
        seq
    ));
    let _tmp_guard = TempFile(tmp_path.clone());

    // Spawn sidecar.
    let child = Command::new(&cfg.sidecar_path)
        .arg("-g")
        .arg(&cfg.game_dir)
        .arg("dump")
        .arg("-o")
        .arg(&tmp_path)
        .arg(replay_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(DecodeError::SidecarSpawn)?;

    // Timeout watchdog: run wait_with_output on a dedicated thread and enforce
    // cfg.timeout from the main thread via a channel with recv_timeout.
    //
    // Design: the child and its I/O are owned exclusively by the waiter thread.
    // The waiter sends the Output back through a channel.  The main thread
    // blocks on that channel with a deadline; on timeout it signals the waiter
    // to kill the child (via a shared kill-request flag checked by a short
    // polling loop inside the waiter), then waits for the waiter to finish so
    // no thread is ever leaked.
    //
    // kill_requested: set by the main thread when the deadline is exceeded.
    // The waiter polls it via try_wait every 50 ms and kills the child when set.
    // Polling instead of a blocking wait_with_output avoids the problem of the
    // waiter being stuck in a non-cancellable kernel wait.
    let timeout = cfg.timeout;
    let kill_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let kill_requested_clone = Arc::clone(&kill_requested);
    let (result_tx, result_rx) = mpsc::channel::<Result<std::process::Output, std::io::Error>>();

    let waiter = std::thread::spawn(move || {
        // `child` is moved into this closure; re-bind as mut so we can call
        // try_wait / kill / wait_with_output.
        let mut child = child;
        // Poll until the child exits or a kill is requested.
        let poll_interval = Duration::from_millis(50);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break, // Child exited — fall through to wait_with_output.
                Ok(None) => {
                    // Still running.
                    if kill_requested_clone.load(std::sync::atomic::Ordering::Acquire) {
                        // Main thread timed out: confirm still running before killing
                        // (closes the PID-reuse race) then kill.
                        let _ = child.kill();
                        break;
                    }
                    std::thread::sleep(poll_interval);
                }
                Err(_) => break, // OS error — fall through to wait_with_output.
            }
        }
        // Collect stdout/stderr regardless of kill path.
        let _ = result_tx.send(child.wait_with_output());
    });

    let output = match result_rx.recv_timeout(timeout + Duration::from_millis(500)) {
        Ok(res) => res.map_err(DecodeError::SidecarSpawn)?,
        Err(_) => {
            // recv_timeout expired: signal the waiter to kill the child.
            kill_requested.store(true, std::sync::atomic::Ordering::Release);
            // Wait for the waiter to finish (it will kill then return quickly).
            let _ = waiter.join();
            return Err(DecodeError::Timeout(timeout));
        }
    };
    // Waiter finished normally — join it (should return immediately).
    let _ = waiter.join();

    if !output.status.success() {
        let stderr: String = String::from_utf8_lossy(&output.stderr)
            .chars()
            .rev()
            .take(400)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        return Err(DecodeError::SidecarFailed {
            code: output.status.code(),
            stderr,
        });
    }

    // Parse the JSONL output file.
    let jsonl = fs::read_to_string(&tmp_path)?;
    parse_jsonl_and_build(jsonl, source_file_hash, replay_path, tables)
}

/// Parse the JSONL dump and assemble BattleData.
fn parse_jsonl_and_build(
    jsonl: String,
    source_file_hash: String,
    replay_path: &Path,
    tables: &Tables,
) -> Result<BattleData, DecodeError> {
    let mut meta_obj: Option<serde_json::Value> = None;
    let mut battle_results: Option<serde_json::Value> = None;

    for (line_no, line) in jsonl.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parsed: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| DecodeError::Malformed(format!("line {line_no}: {e}")))?;

        // Line 0: if no "packet_type" key → meta object
        if line_no == 0 && parsed.get("packet_type").is_none() {
            meta_obj = Some(parsed);
            continue;
        }

        // Find a BattleResults packet by its payload shape:
        // {"payload":{"BattleResults":"<json>"}} — independent of whether the
        // packet carries a top-level "packet_type" key. This mirrors
        // extract_all.py (which keys only on the payload.BattleResults string)
        // and stays robust if a future sidecar version reshapes the wrapper.
        if let Some(payload) = parsed.get("payload") {
            if let Some(br_str) = payload.get("BattleResults").and_then(|v| v.as_str()) {
                // Parse the inner JSON string.
                match serde_json::from_str::<serde_json::Value>(br_str) {
                    Ok(br) => {
                        battle_results = Some(br);
                        // Keep the LAST BattleResults (most complete).
                    }
                    Err(e) => {
                        return Err(DecodeError::Malformed(format!(
                            "BattleResults inner JSON parse error on line {line_no}: {e}"
                        )));
                    }
                }
            }
        }
    }

    let br = battle_results.ok_or(DecodeError::NoBattleResults)?;

    // ── Extract common fields ─────────────────────────────────────────────────

    let common_list = br
        .get("commonList")
        .and_then(|v| v.as_array())
        .ok_or_else(|| DecodeError::Malformed("missing or non-array commonList".into()))?;

    // Build a map from common_results names → values by zipping.
    let common_map: HashMap<&str, &serde_json::Value> = tables
        .common_results
        .iter()
        .enumerate()
        .filter_map(|(i, name)| common_list.get(i).map(|v| (name.as_str(), v)))
        .collect();

    let arena_unique_id = br
        .get("arenaUniqueID")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let owner_account_db_id = br.get("accountDBID").and_then(|v| v.as_i64());
    let owner_db_id = owner_account_db_id.unwrap_or(0);

    let winner_team: Option<i64> = common_map
        .get("winner_team_id")
        .and_then(|v| to_i64_tolerant(v))
        .filter(|&w| w >= 0); // -1 means unknown

    let duration_seconds: Option<i64> = common_map
        .get("duration_sec")
        .and_then(|v| to_i64_tolerant(v));

    let battle_time: Option<i64> = common_map.get("start_dt").and_then(|v| to_i64_tolerant(v));

    // ── Meta from sidecar line-0 ──────────────────────────────────────────────

    let game_version = meta_obj
        .as_ref()
        .and_then(|m| m.get("clientVersionFromExe"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let game_version_short = parse_version_short(&game_version);

    let match_group = meta_obj
        .as_ref()
        .and_then(|m| m.get("matchGroup"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let map_name = meta_obj
        .as_ref()
        .and_then(|m| m.get("mapName"))
        .and_then(|v| v.as_str())
        .map(|s| s.strip_prefix("spaces/").unwrap_or(s).to_string())
        .unwrap_or_default();

    // ── Version and sanity warnings ───────────────────────────────────────────

    let mut warnings: Vec<String> = Vec::new();

    if !game_version.is_empty() {
        if let Some(ref short) = game_version_short {
            let parts: Vec<u32> = short
                .splitn(2, '.')
                .filter_map(|p| p.parse().ok())
                .collect();
            if parts.len() == 2 {
                let maj = parts[0];
                let min = parts[1];
                if !KNOWN_GOOD.contains(&(maj, min)) {
                    warnings.push(format!(
                        "game version {short} is not in the known-good set {:?}; constants/sidecar may be stale",
                        KNOWN_GOOD
                    ));
                }
            }
        }
    }

    // ── Players ───────────────────────────────────────────────────────────────

    let players_public = br
        .get("playersPublicInfo")
        .and_then(|v| v.as_object())
        .ok_or_else(|| DecodeError::Malformed("missing or non-object playersPublicInfo".into()))?;

    // privateDataList is for the owner only — a flat array.
    let private_list: Option<&Vec<serde_json::Value>> =
        br.get("privateDataList").and_then(|v| v.as_array());

    let mut players: Vec<BattlePlayer> = Vec::with_capacity(players_public.len());
    let mut unknown_ship_count = 0usize;

    for (db_id_str, arr_val) in players_public {
        let arr = match arr_val.as_array() {
            Some(a) => a,
            None => continue, // skip non-array entries
        };

        let db_id: i64 = match db_id_str.parse() {
            Ok(id) => id,
            Err(_) => continue,
        };

        let is_owner = db_id == owner_db_id;
        let private_for_owner = if is_owner { private_list } else { None };

        let player = resolve_player(
            arr,
            db_id,
            tables,
            owner_db_id,
            winner_team,
            private_for_owner.map(|v| v.as_slice()),
        );

        if player.ship_id.is_some() && player.ship_name.is_none() {
            unknown_ship_count += 1;
        }

        players.push(player);
    }

    // Sanity: ship resolution rate.
    if !players.is_empty() {
        let total_with_ship_id = players.iter().filter(|p| p.ship_id.is_some()).count();
        if total_with_ship_id > 0 {
            let resolved = players.iter().filter(|p| p.ship_name.is_some()).count();
            let rate = resolved as f64 / total_with_ship_id as f64;
            if rate < 0.8 {
                warnings.push(format!(
                    "ship resolution rate {:.0}% is low ({} of {} ships resolved); ship_index.json may be stale",
                    rate * 100.0,
                    resolved,
                    total_with_ship_id
                ));
            }
        }
        let _ = unknown_ship_count; // already counted via ship_name.is_none()
    }

    // XP ratio sanity check: winner team exp should be higher on average.
    if let Some(wt) = winner_team {
        let winners_raw: Vec<i64> = players
            .iter()
            .filter(|p| p.team_id == Some(wt))
            .filter_map(|p| p.raw_exp)
            .collect();
        let losers_raw: Vec<i64> = players
            .iter()
            .filter(|p| p.team_id.is_some() && p.team_id != Some(wt))
            .filter_map(|p| p.raw_exp)
            .collect();
        if !winners_raw.is_empty() && !losers_raw.is_empty() {
            let w_avg = winners_raw.iter().sum::<i64>() as f64 / winners_raw.len() as f64;
            let l_avg = losers_raw.iter().sum::<i64>() as f64 / losers_raw.len() as f64;
            if w_avg < l_avg * 0.8 {
                warnings.push(format!(
                    "XP sanity: winner team avg raw_exp {w_avg:.0} < loser avg {l_avg:.0}; data may be misresolved"
                ));
            }
        }
    }

    let _ = replay_path; // used by caller for logging if needed

    Ok(BattleData {
        meta: BattleMeta {
            schema_version: "1.0".into(),
            arena_unique_id,
            map_name,
            game_version,
            game_version_short,
            match_group,
            duration_seconds,
            winner_team,
            battle_time,
            source_file_hash,
            owner_account_db_id,
            warnings,
        },
        players,
    })
}

// ── Player resolver ────────────────────────────────────────────────────────────

/// Resolve a player's positional array to a [`BattlePlayer`].
///
/// All field reads are tolerant: wrong type / out of bounds / null → `None`.
pub(crate) fn resolve_player(
    arr: &[serde_json::Value],
    db_id: i64,
    tables: &Tables,
    owner_db_id: i64,
    winner_team: Option<i64>,
    private_for_owner: Option<&[serde_json::Value]>,
) -> BattlePlayer {
    let pub_idx = &tables.public_indices;

    let get_val = |name: &str| -> Option<&serde_json::Value> {
        let &idx = pub_idx.get(name)?;
        arr.get(idx)
    };

    let get_i64 = |name: &str| -> Option<i64> { to_i64_tolerant(get_val(name)?) };

    // account_db_id: from array[0] (should match key, but use the key db_id as ground truth)
    let account_db_id = db_id;

    let player_name = get_val("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let clan_id = get_i64("clan_id").filter(|&v| v != 0);

    let clan_tag = get_val("clan_tag")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let ship_id: Option<i64> = get_i64("vehicle_type_id");

    // Ship enrichment.
    let (ship_name, ship_tier, ship_class) = match ship_id {
        Some(id) => {
            let key = id.to_string();
            match tables.ships.get(&key) {
                Some(info) => (
                    Some(info.index.clone()),
                    Some(info.level),
                    Some(ShipClass::from_species(&info.species)),
                ),
                None => (None, None, None),
            }
        }
        None => (None, None, None),
    };

    let team_id = get_i64("team_id");
    let prebattle_id = get_i64("prebattle_id");
    let exp = get_i64("exp");
    let raw_exp = get_i64("raw_exp");
    let damage_dealt = get_i64("damage");
    let frags = get_i64("ships_killed");

    // Composite sums — coerce each component to i64 (null/missing → 0).
    let agro_art = get_val("agro_art").and_then(to_i64_tolerant).unwrap_or(0);
    let agro_tpd = get_val("agro_tpd").and_then(to_i64_tolerant).unwrap_or(0);
    let agro_air = get_val("agro_air").and_then(to_i64_tolerant).unwrap_or(0);
    let agro_dbomb = get_val("agro_dbomb").and_then(to_i64_tolerant).unwrap_or(0);
    let damage_potential = Some(agro_art + agro_tpd + agro_air + agro_dbomb);

    let shots_ap = get_val("shots_main_ap")
        .and_then(to_i64_tolerant)
        .unwrap_or(0);
    let shots_cs = get_val("shots_main_cs")
        .and_then(to_i64_tolerant)
        .unwrap_or(0);
    let shots_he = get_val("shots_main_he")
        .and_then(to_i64_tolerant)
        .unwrap_or(0);
    let shots_fired = Some(shots_ap + shots_cs + shots_he);

    let hits_ap = get_val("hits_main_ap")
        .and_then(to_i64_tolerant)
        .unwrap_or(0);
    let hits_cs = get_val("hits_main_cs")
        .and_then(to_i64_tolerant)
        .unwrap_or(0);
    let hits_he = get_val("hits_main_he")
        .and_then(to_i64_tolerant)
        .unwrap_or(0);
    let hits = Some(hits_ap + hits_cs + hits_he);

    let ribbons_torpedo_hits = get_i64("RIBBON_TORPEDO");
    let ribbons_plane_kills = get_i64("RIBBON_PLANE");
    let ribbons_hits = get_i64("RIBBON_MAIN_CALIBER");

    // is_alive → survived: accept bool or 0/1 int.
    let survived: Option<bool> = get_val("is_alive").and_then(to_bool_tolerant);

    let is_self = db_id == owner_db_id;

    // won: winner_team == team_id (None when winner_team is unknown).
    let won: Option<bool> = winner_team.and_then(|wt| team_id.map(|tid| wt == tid));

    // afk: owner only, from privateDataList[37] (PLAYER_PRIVATE_RESULTS[37] == "is_afk").
    let afk: Option<bool> = if is_self {
        // Find is_afk index in PLAYER_PRIVATE_RESULTS.
        let afk_idx = tables
            .private_results
            .iter()
            .position(|name| name == "is_afk")
            .unwrap_or(37); // spec says 37; use confirmed default
        private_for_owner
            .and_then(|pd| pd.get(afk_idx))
            .and_then(to_bool_tolerant)
    } else {
        None
    };

    BattlePlayer {
        account_db_id,
        player_name,
        clan_id,
        clan_tag,
        ship_id,
        ship_name,
        ship_tier,
        ship_class,
        team_id,
        prebattle_id,
        exp,
        raw_exp,
        damage_dealt,
        damage_to_buildings: None, // v1.0: None (no public key; buildingInteractions not wired)
        damage_potential,
        shots_fired,
        hits,
        frags,
        xp_contribution: None, // v1.0: None (team-share derived, deferred)
        ribbons_torpedo_hits,
        ribbons_plane_kills,
        ribbons_hits,
        afk,
        survived,
        is_self,
        won,
    }
}

// ── Tolerant type coercion helpers ─────────────────────────────────────────────

/// Coerce a JSON value to i64: accepts int, float (truncated), null → None.
fn to_i64_tolerant(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else {
                n.as_f64().map(|f| f as i64)
            }
        }
        serde_json::Value::Null => None,
        _ => None,
    }
}

/// Coerce a JSON value to bool: accepts bool directly, or 0/1 int.
fn to_bool_tolerant(v: &serde_json::Value) -> Option<bool> {
    match v {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::Number(n) => n.as_i64().map(|i| i != 0),
        serde_json::Value::Null => None,
        _ => None,
    }
}

/// Parse "15,4,0,12506899" → "15.4" (major.minor).
fn parse_version_short(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let parts: Vec<&str> = raw.splitn(4, ',').collect();
    if parts.len() >= 2 {
        let major = parts[0].trim();
        let minor = parts[1].trim();
        if !major.is_empty() && !minor.is_empty() {
            return Some(format!("{major}.{minor}"));
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── Paths ─────────────────────────────────────────────────────────────────

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
    }

    fn constants_path() -> PathBuf {
        // Use the full staged constants.json in src-tauri/resources/
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("src-tauri")
            .join("resources")
            .join("constants.json")
    }

    fn ship_index_min_path() -> PathBuf {
        fixture_dir().join("ship_index_min.json")
    }

    fn battle_results_fixture_path() -> PathBuf {
        fixture_dir().join("battle_results_min.json")
    }

    // ── Tables load ───────────────────────────────────────────────────────────

    #[test]
    fn tables_load_succeeds() {
        let tables =
            Tables::load(&constants_path(), &ship_index_min_path()).expect("tables must load");
        assert!(tables.public_indices.contains_key("account_db_id"));
        assert!(tables.public_indices.contains_key("exp"));
        assert!(tables.public_indices.contains_key("damage"));
        assert!(!tables.common_results.is_empty());
        assert!(!tables.ships.is_empty());
    }

    #[test]
    fn tables_common_results_is_array() {
        let tables = Tables::load(&constants_path(), &ship_index_min_path()).unwrap();
        // COMMON_RESULTS must be an ordered array starting with "arena_id"
        assert_eq!(tables.common_results[0], "arena_id");
        assert_eq!(tables.common_results[3], "winner_team_id");
        assert_eq!(tables.common_results[8], "duration_sec");
    }

    #[test]
    fn tables_client_veh_interaction_details_is_array() {
        let tables = Tables::load(&constants_path(), &ship_index_min_path()).unwrap();
        // Should parse as a Vec (may be empty or populated)
        // Just verify it loaded without error and is a vec
        let _ = tables.interaction_details;
    }

    // ── sha256_hex ────────────────────────────────────────────────────────────

    #[test]
    fn sha256_hex_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    // ── parse_version_short ───────────────────────────────────────────────────

    #[test]
    fn parse_version_short_standard() {
        assert_eq!(parse_version_short("15,4,0,12506899"), Some("15.4".into()));
        assert_eq!(parse_version_short("15,3,0,12267945"), Some("15.3".into()));
    }

    #[test]
    fn parse_version_short_empty() {
        assert_eq!(parse_version_short(""), None);
    }

    // ── resolve_player ────────────────────────────────────────────────────────

    fn make_tables_from_fixture() -> Tables {
        Tables::load(&constants_path(), &ship_index_min_path()).unwrap()
    }

    /// Build a sparse player array large enough to hold all needed indices.
    /// All positions default to JSON null; callers set specific slots.
    fn make_arr(size: usize) -> Vec<serde_json::Value> {
        vec![serde_json::Value::Null; size]
    }

    fn set_i(arr: &mut [serde_json::Value], idx: usize, v: i64) {
        arr[idx] = serde_json::json!(v);
    }

    fn set_f(arr: &mut [serde_json::Value], idx: usize, v: f64) {
        arr[idx] = serde_json::json!(v);
    }

    fn set_s(arr: &mut [serde_json::Value], idx: usize, v: &str) {
        arr[idx] = serde_json::json!(v);
    }

    fn set_b(arr: &mut [serde_json::Value], idx: usize, v: bool) {
        arr[idx] = serde_json::json!(v);
    }

    #[test]
    fn resolve_player_confirmed_fields() {
        let tables = make_tables_from_fixture();
        let mut arr = make_arr(540);

        // Fill confirmed fields per spec indices.
        set_i(&mut arr, 0, 591735977); // account_db_id
        set_s(&mut arr, 1, "FrankDrake"); // name
        set_i(&mut arr, 2, 0); // clan_id (0 → None)
        set_s(&mut arr, 3, "-TFD-"); // clan_tag
        set_i(&mut arr, 6, 1); // team_id
        set_i(&mut arr, 7, 3551475536u32 as i64); // vehicle_type_id
        set_i(&mut arr, 8, 0); // prebattle_id
        set_i(&mut arr, 15, 90000); // max_health (not in output but verify index works)
        set_b(&mut arr, 21, false); // is_alive → survived
        set_i(&mut arr, 32, 2); // ships_killed → frags
        set_i(&mut arr, 35, 54); // shots_main_ap
        set_i(&mut arr, 36, 0); // shots_main_cs
        set_i(&mut arr, 37, 6); // shots_main_he
        set_i(&mut arr, 66, 30); // hits_main_ap
        set_i(&mut arr, 67, 0); // hits_main_cs
        set_i(&mut arr, 68, 1); // hits_main_he
        set_i(&mut arr, 403, 876); // raw_exp
        set_i(&mut arr, 404, 1314); // exp
        set_f(&mut arr, 416, 862200.0); // agro_art
        set_f(&mut arr, 417, 115200.0); // agro_tpd
        set_i(&mut arr, 418, 0); // agro_air
        set_i(&mut arr, 419, 0); // agro_dbomb
        set_i(&mut arr, 426, 75898); // damage → damage_dealt
        set_i(&mut arr, 446, 5); // RIBBON_MAIN_CALIBER → ribbons_hits
        set_i(&mut arr, 447, 3); // RIBBON_TORPEDO → ribbons_torpedo_hits
        set_i(&mut arr, 449, 0); // RIBBON_PLANE → ribbons_plane_kills

        let player = resolve_player(
            &arr,
            591735977,
            &tables,
            591735977, // is owner
            Some(1),   // winner_team = 1
            None,
        );

        assert_eq!(player.account_db_id, 591735977);
        assert_eq!(player.player_name, Some("FrankDrake".into()));
        assert_eq!(player.clan_id, None); // 0 → None
        assert_eq!(player.clan_tag, Some("-TFD-".into()));
        assert_eq!(player.ship_id, Some(3551475536u32 as i64));
        // Ship known in ship_index_min
        assert_eq!(player.ship_name, Some("PFSB709".into()));
        assert_eq!(player.ship_tier, Some(9));
        assert_eq!(player.ship_class, Some(ShipClass::Battleship));
        assert_eq!(player.team_id, Some(1));
        assert_eq!(player.prebattle_id, Some(0));
        assert_eq!(player.exp, Some(1314));
        assert_eq!(player.raw_exp, Some(876));
        assert_eq!(player.damage_dealt, Some(75898));
        assert_eq!(player.damage_to_buildings, None);
        // damage_potential = 862200 + 115200 + 0 + 0 = 977400
        assert_eq!(player.damage_potential, Some(977400));
        // shots_fired = 54 + 0 + 6 = 60
        assert_eq!(player.shots_fired, Some(60));
        // hits = 30 + 0 + 1 = 31
        assert_eq!(player.hits, Some(31));
        assert_eq!(player.frags, Some(2));
        assert_eq!(player.xp_contribution, None);
        assert_eq!(player.ribbons_torpedo_hits, Some(3));
        assert_eq!(player.ribbons_plane_kills, Some(0));
        assert_eq!(player.ribbons_hits, Some(5));
        assert_eq!(player.survived, Some(false));
        assert!(player.is_self);
        // won: winner_team(1) == team_id(1) → true
        assert_eq!(player.won, Some(true));
    }

    #[test]
    fn resolve_player_is_self_false_for_non_owner() {
        let tables = make_tables_from_fixture();
        let arr = make_arr(540);

        let player = resolve_player(
            &arr,
            510621696, // not owner
            &tables,
            591735977, // owner_db_id
            Some(1),
            None,
        );

        assert!(!player.is_self);
        assert_eq!(player.afk, None); // non-owner → no afk
    }

    #[test]
    fn resolve_player_won_none_when_winner_unknown() {
        let tables = make_tables_from_fixture();
        let mut arr = make_arr(540);
        set_i(&mut arr, 6, 1); // team_id = 1

        let player = resolve_player(
            &arr, 123456, &tables, 999999, None, // winner unknown
            None,
        );

        assert_eq!(player.won, None);
    }

    #[test]
    fn resolve_player_won_false_for_loser() {
        let tables = make_tables_from_fixture();
        let mut arr = make_arr(540);
        set_i(&mut arr, 6, 0); // team_id = 0 (loser team)

        let player = resolve_player(
            &arr,
            123456,
            &tables,
            999999,
            Some(1), // team 1 won
            None,
        );

        assert_eq!(player.won, Some(false));
    }

    /// winner_team_id = -1 (unknown) should map meta.winner_team to None and all
    /// players' won to None (spec §8: "-1/absent => unknown").
    #[test]
    fn parse_jsonl_winner_team_minus_one_maps_to_none() {
        let tables = Tables::load(&constants_path(), &ship_index_min_path()).unwrap();
        let inner = serde_json::json!({
            "arenaUniqueID": 55555,
            "accountDBID": 100,
            "commonList": [55555, 0, 0, -1i64, 0, 0, "regular", 0, 300, 0, "domination", 0, 7, "", {}, 0, 0, 0],
            "playersPublicInfo": {
                "100": vec![serde_json::Value::Null; 540]
            },
            "privateDataList": []
        });
        let inner_str = serde_json::to_string(&inner).unwrap();
        let jsonl = format!(
            "{{\"matchGroup\":\"pvp\",\"clientVersionFromExe\":\"15,4,0,1\"}}\n{{\"packet_type\":34,\"clock\":1.0,\"payload\":{{\"BattleResults\":{inner_str:?}}}}}"
        );
        let data =
            parse_jsonl_and_build(jsonl, "hashx".into(), Path::new("test.wowsreplay"), &tables)
                .expect("should parse");
        // winner_team_id = -1 → meta.winner_team must be None.
        assert_eq!(
            data.meta.winner_team, None,
            "winner_team_id=-1 must yield meta.winner_team=None"
        );
        // All players' won must be None when winner_team is unknown.
        for p in &data.players {
            assert_eq!(
                p.won, None,
                "player won must be None when winner_team is unknown"
            );
        }
    }

    /// resolve_player with winner_team=Some(0) and team_id=0 should yield won=Some(true).
    /// This validates the spec's "winner==0 case" (team 0 can be the winner).
    #[test]
    fn resolve_player_won_true_when_winner_is_team_zero() {
        let tables = make_tables_from_fixture();
        let mut arr = make_arr(540);
        set_i(&mut arr, 6, 0); // team_id = 0

        let player = resolve_player(
            &arr,
            123456,
            &tables,
            999999,
            Some(0), // team 0 won
            None,
        );

        assert_eq!(
            player.won,
            Some(true),
            "won must be Some(true) when winner_team=0 and team_id=0"
        );
    }

    #[test]
    fn resolve_player_survived_accepts_bool() {
        let tables = make_tables_from_fixture();
        let mut arr = make_arr(540);
        set_b(&mut arr, 21, true); // is_alive = true (bool)

        let player = resolve_player(&arr, 1, &tables, 999, None, None);
        assert_eq!(player.survived, Some(true));
    }

    #[test]
    fn resolve_player_survived_accepts_int_one() {
        let tables = make_tables_from_fixture();
        let mut arr = make_arr(540);
        set_i(&mut arr, 21, 1); // is_alive = 1 (int, future-proofing)

        let player = resolve_player(&arr, 1, &tables, 999, None, None);
        assert_eq!(player.survived, Some(true));
    }

    #[test]
    fn resolve_player_ship_enrich_unknown_id() {
        let tables = make_tables_from_fixture();
        let mut arr = make_arr(540);
        set_i(&mut arr, 7, 9999999999i64); // unknown ship id

        let player = resolve_player(&arr, 1, &tables, 999, None, None);
        assert_eq!(player.ship_id, Some(9999999999i64));
        assert_eq!(player.ship_name, None);
        assert_eq!(player.ship_tier, None);
        assert_eq!(player.ship_class, None);
    }

    #[test]
    fn resolve_player_ship_enrich_known_id() {
        let tables = make_tables_from_fixture();
        let mut arr = make_arr(540);
        // 3543086896 = PGSB717 level 7 Battleship in ship_index_min
        set_i(&mut arr, 7, 3543086896u32 as i64);

        let player = resolve_player(&arr, 1, &tables, 999, None, None);
        assert_eq!(player.ship_name, Some("PGSB717".into()));
        assert_eq!(player.ship_tier, Some(7));
        assert_eq!(player.ship_class, Some(ShipClass::Battleship));
    }

    #[test]
    fn resolve_player_composite_sums_correct() {
        let tables = make_tables_from_fixture();
        let mut arr = make_arr(540);

        // shots_main_* → shots_fired
        set_i(&mut arr, 35, 10); // shots_main_ap
        set_i(&mut arr, 36, 5); // shots_main_cs
        set_i(&mut arr, 37, 20); // shots_main_he

        // hits_main_* → hits
        set_i(&mut arr, 66, 3); // hits_main_ap
        set_i(&mut arr, 67, 1); // hits_main_cs
        set_i(&mut arr, 68, 7); // hits_main_he

        // agro_* → damage_potential
        set_f(&mut arr, 416, 1000.0); // agro_art
        set_f(&mut arr, 417, 500.0); // agro_tpd
        set_i(&mut arr, 418, 250); // agro_air
        set_i(&mut arr, 419, 100); // agro_dbomb

        let player = resolve_player(&arr, 1, &tables, 999, None, None);
        assert_eq!(player.shots_fired, Some(35)); // 10+5+20
        assert_eq!(player.hits, Some(11)); // 3+1+7
        assert_eq!(player.damage_potential, Some(1850)); // 1000+500+250+100
    }

    #[test]
    fn resolve_player_null_fields_are_none() {
        let tables = make_tables_from_fixture();
        // All-null array — every optional field should be None.
        let arr = make_arr(540);

        let player = resolve_player(
            &arr,
            123,
            &tables,
            456, // not owner
            Some(1),
            None,
        );

        assert_eq!(player.player_name, None);
        assert_eq!(player.clan_id, None);
        assert_eq!(player.clan_tag, None);
        assert_eq!(player.ship_id, None);
        assert_eq!(player.exp, None);
        assert_eq!(player.raw_exp, None);
        assert_eq!(player.damage_dealt, None);
        assert_eq!(player.ribbons_torpedo_hits, None);
        assert_eq!(player.survived, None);
    }

    #[test]
    fn resolve_player_afk_from_private_data() {
        let tables = make_tables_from_fixture();
        let arr = make_arr(540);

        // Build a privateDataList with is_afk=true at index 37.
        let mut private_data: Vec<serde_json::Value> = vec![serde_json::Value::Null; 54];
        private_data[37] = serde_json::json!(true);

        let player = resolve_player(
            &arr,
            591735977,
            &tables,
            591735977, // owner
            None,
            Some(&private_data),
        );

        assert!(player.is_self);
        assert_eq!(player.afk, Some(true));
    }

    #[test]
    fn resolve_player_afk_false_from_private_data() {
        let tables = make_tables_from_fixture();
        let arr = make_arr(540);
        let mut private_data: Vec<serde_json::Value> = vec![serde_json::Value::Null; 54];
        private_data[37] = serde_json::json!(false);

        let player = resolve_player(
            &arr,
            591735977,
            &tables,
            591735977,
            None,
            Some(&private_data),
        );
        assert_eq!(player.afk, Some(false));
    }

    // ── Fixture-based integration tests ───────────────────────────────────────

    /// Parse the real trimmed fixture and assert key fields.
    #[test]
    fn fixture_decode_resolves_owner_and_players() {
        let tables = Tables::load(&constants_path(), &ship_index_min_path()).unwrap();
        let fixture_str =
            std::fs::read_to_string(battle_results_fixture_path()).expect("fixture must exist");
        let br: serde_json::Value = serde_json::from_str(&fixture_str).unwrap();

        let common_list = br["commonList"].as_array().unwrap();
        let common_map: HashMap<&str, &serde_json::Value> = tables
            .common_results
            .iter()
            .enumerate()
            .filter_map(|(i, name)| common_list.get(i).map(|v| (name.as_str(), v)))
            .collect();

        let winner_team: Option<i64> = common_map
            .get("winner_team_id")
            .and_then(|v| v.as_i64())
            .filter(|&w| w >= 0);
        let owner_db_id: i64 = br["accountDBID"].as_i64().unwrap();
        let private_list = br["privateDataList"].as_array();

        let players_obj = br["playersPublicInfo"].as_object().unwrap();

        let mut found_owner = false;
        for (db_id_str, arr_val) in players_obj {
            let arr = arr_val.as_array().unwrap();
            let db_id: i64 = db_id_str.parse().unwrap();
            let is_owner = db_id == owner_db_id;
            let private = if is_owner { private_list } else { None };

            let player = resolve_player(
                arr,
                db_id,
                &tables,
                owner_db_id,
                winner_team,
                private.map(|v| v.as_slice()),
            );

            // Verify account_db_id matches key
            assert_eq!(player.account_db_id, db_id);
            // is_self correct
            assert_eq!(player.is_self, is_owner);

            if is_owner {
                found_owner = true;
                // Owner should be FrankDrake in this fixture
                assert_eq!(player.player_name.as_deref(), Some("FrankDrake"));
                // Owner's ship: PFSB709 Battleship T9
                assert_eq!(player.ship_name.as_deref(), Some("PFSB709"));
                assert_eq!(player.ship_tier, Some(9));
                assert_eq!(player.ship_class, Some(ShipClass::Battleship));
                // Known values from real replay
                assert_eq!(player.exp, Some(1314));
                assert_eq!(player.raw_exp, Some(876));
                assert_eq!(player.damage_dealt, Some(75898));
                assert_eq!(player.frags, Some(0));
                assert_eq!(player.shots_fired, Some(60)); // 54+0+6
                assert_eq!(player.hits, Some(31)); // 30+0+1
                assert_eq!(player.damage_potential, Some(977400)); // 862200+115200+0+0
                assert_eq!(player.survived, Some(false));
                // won: winner_team(1) == team_id(1) → true
                assert_eq!(player.won, Some(true));
                // afk: from privateDataList[37]
                assert_eq!(player.afk, Some(false)); // real value from fixture
            }
        }

        assert!(found_owner, "owner player must be found in fixture");
    }

    #[test]
    fn fixture_winner_team_and_duration() {
        let tables = Tables::load(&constants_path(), &ship_index_min_path()).unwrap();
        let fixture_str =
            std::fs::read_to_string(battle_results_fixture_path()).expect("fixture must exist");
        let br: serde_json::Value = serde_json::from_str(&fixture_str).unwrap();

        let common_list = br["commonList"].as_array().unwrap();
        let common_map: HashMap<&str, &serde_json::Value> = tables
            .common_results
            .iter()
            .enumerate()
            .filter_map(|(i, name)| common_list.get(i).map(|v| (name.as_str(), v)))
            .collect();

        let winner_team = common_map
            .get("winner_team_id")
            .and_then(|v| v.as_i64())
            .filter(|&w| w >= 0);
        let duration = common_map
            .get("duration_sec")
            .and_then(|v| to_i64_tolerant(v));

        // From real data: winner_team_id=1, duration_sec=1010
        assert_eq!(winner_team, Some(1));
        assert_eq!(duration, Some(1010));
    }

    // ── Error mapping tests ───────────────────────────────────────────────────

    /// Missing sidecar path → SidecarMissing error.
    #[test]
    fn decode_error_sidecar_missing() {
        let tables = Tables::load(&constants_path(), &ship_index_min_path()).unwrap();
        let cfg = DecodeConfig {
            sidecar_path: PathBuf::from("/nonexistent/replayshark.exe"),
            game_dir: PathBuf::from("/nonexistent/game"),
            constants_path: constants_path(),
            ship_index_path: ship_index_min_path(),
            timeout: Duration::from_secs(30),
        };
        let result = decode_battle_result(Path::new("/nonexistent.wowsreplay"), &cfg, &tables);
        assert!(matches!(result, Err(DecodeError::SidecarMissing(_))));
    }

    /// Missing constants.json → Resources error.
    #[test]
    fn decode_error_resources_missing_constants() {
        let result = Tables::load(
            Path::new("/nonexistent/constants.json"),
            &ship_index_min_path(),
        );
        assert!(matches!(result, Err(DecodeError::Resources(_))));
    }

    /// JSONL with no BattleResults line → NoBattleResults.
    #[test]
    fn decode_no_battle_results_error() {
        let tables = Tables::load(&constants_path(), &ship_index_min_path()).unwrap();
        let jsonl = r#"{"matchGroup":"pvp","clientVersionFromExe":"15,4,0,1"}"#.to_string();
        // Call parse_jsonl_and_build directly (no sidecar needed)
        let result = parse_jsonl_and_build(
            jsonl,
            "abc123".into(),
            Path::new("test.wowsreplay"),
            &tables,
        );
        assert!(matches!(result, Err(DecodeError::NoBattleResults)));
    }

    /// Malformed inner BattleResults JSON → Malformed error.
    #[test]
    fn decode_malformed_inner_json_error() {
        let tables = Tables::load(&constants_path(), &ship_index_min_path()).unwrap();
        let jsonl =
            r#"{"packet_type":34,"clock":1.0,"payload":{"BattleResults":"{not valid json"}}"#
                .to_string();
        let result = parse_jsonl_and_build(
            jsonl,
            "abc123".into(),
            Path::new("test.wowsreplay"),
            &tables,
        );
        assert!(matches!(result, Err(DecodeError::Malformed(_))));
    }

    /// JSONL with valid BattleResults but no players → empty players vec, no error.
    #[test]
    fn decode_empty_players_no_error() {
        let tables = Tables::load(&constants_path(), &ship_index_min_path()).unwrap();
        let inner = serde_json::json!({
            "arenaUniqueID": 12345,
            "accountDBID": 999,
            "commonList": [12345, 0, 0, 1, 0, 0, "regular", 0, 600, 0, "domination", 0, 7, "", {}, 0, 0, 0],
            "playersPublicInfo": {},
            "privateDataList": []
        });
        let inner_str = serde_json::to_string(&inner).unwrap();
        let jsonl = format!(
            "{{\"matchGroup\":\"pvp\",\"clientVersionFromExe\":\"15,4,0,1\"}}\n{{\"packet_type\":34,\"clock\":1.0,\"payload\":{{\"BattleResults\":{inner_str:?}}}}}",
        );
        let result = parse_jsonl_and_build(
            jsonl,
            "abc123".into(),
            Path::new("test.wowsreplay"),
            &tables,
        );
        let data = result.expect("should succeed with empty players");
        assert!(data.players.is_empty());
        assert_eq!(data.meta.schema_version, "1.0");
    }

    // ── Meta fields and warnings system ──────────────────────────────────────

    /// parse_jsonl_and_build correctly wires meta fields from the sidecar line-0
    /// (game_version_short, match_group, map_name with spaces/ stripped) and
    /// pushes a stale-version warning for versions not in KNOWN_GOOD.
    #[test]
    fn parse_jsonl_meta_fields_and_stale_version_warning() {
        let tables = Tables::load(&constants_path(), &ship_index_min_path()).unwrap();
        let inner = serde_json::json!({
            "arenaUniqueID": 99001,
            "accountDBID": 1111,
            "commonList": [99001, 0, 1700000000i64, 0, 0, 0, "regular", 0, 600, 0, "domination", 0, 7, "", {}, 0, 0, 0],
            "playersPublicInfo": {},
            "privateDataList": []
        });
        let inner_str = serde_json::to_string(&inner).unwrap();
        // clientVersionFromExe "15,5,0,1" → short "15.5" — NOT in KNOWN_GOOD → should warn.
        // mapName "spaces/23_Shards" → map_name should be "23_Shards" (strip prefix).
        // matchGroup "ranked" → match_group should be Some("ranked").
        let meta_line = r#"{"matchGroup":"ranked","clientVersionFromExe":"15,5,0,1","mapName":"spaces/23_Shards"}"#;
        let jsonl = format!(
            "{meta_line}\n{{\"packet_type\":34,\"clock\":1.0,\"payload\":{{\"BattleResults\":{inner_str:?}}}}}"
        );
        let data = parse_jsonl_and_build(
            jsonl,
            "hash123".into(),
            Path::new("test.wowsreplay"),
            &tables,
        )
        .expect("should parse successfully");

        assert_eq!(
            data.meta.map_name, "23_Shards",
            "spaces/ prefix must be stripped"
        );
        assert_eq!(
            data.meta.game_version_short,
            Some("15.5".into()),
            "game_version_short must be parsed from clientVersionFromExe"
        );
        assert_eq!(
            data.meta.match_group,
            Some("ranked".into()),
            "match_group must be taken from meta matchGroup"
        );
        // 15.5 is not in KNOWN_GOOD → must have a stale-version warning.
        assert!(
            !data.meta.warnings.is_empty(),
            "stale version 15.5 must produce a warning"
        );
        assert!(
            data.meta.warnings.iter().any(|w| w.contains("15.5")),
            "warning must mention the version; got: {:?}",
            data.meta.warnings
        );
    }

    // ── E2E integration test (real sidecar, real replays) ─────────────────────

    /// End-to-end test: decode N=10 real replays (5 from 15.3 + 5 from 15.4) with
    /// the real sidecar and compare against the Python-extracted reference JSONs.
    ///
    /// Gated on `TFD_DECODE_E2E=1` environment variable.
    /// Run with: `TFD_DECODE_E2E=1 cargo test -p bridge-core decode_e2e_vs_python_reference -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn decode_e2e_vs_python_reference() {
        if std::env::var("TFD_DECODE_E2E").as_deref() != Ok("1") {
            eprintln!("Skipping e2e test: TFD_DECODE_E2E!=1");
            return;
        }

        let sidecar =
            PathBuf::from(r"C:\Users\fhelm\code\wows-toolkit\target\release\replayshark.exe");
        let game_dir = PathBuf::from(r"C:\Games\World_of_Warships");
        let extracted_dir = PathBuf::from(
            r"C:\Users\fhelm\Documents\tfd-bridge\private-sync\notes\xp-analysis\extracted",
        );
        let archive_dir = PathBuf::from(r"C:\Users\fhelm\Documents\wows-replay-archive");
        let wows_replays = PathBuf::from(r"C:\Games\World_of_Warships\replays");

        // Pre-flight checks.
        assert!(sidecar.exists(), "sidecar not found: {}", sidecar.display());
        assert!(
            game_dir.exists(),
            "game dir not found: {}",
            game_dir.display()
        );
        assert!(
            extracted_dir.exists(),
            "extracted dir not found: {}",
            extracted_dir.display()
        );

        let si_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("src-tauri/resources/ship_index.json");
        let tables = Tables::load(&constants_path(), &si_path).expect("tables must load for e2e");

        let cfg = DecodeConfig {
            sidecar_path: sidecar,
            game_dir,
            constants_path: constants_path(),
            ship_index_path: si_path,
            timeout: Duration::from_secs(120),
        };

        // Helper: locate source replay for a reference JSON entry.
        let locate_replay = |ref_json: &serde_json::Value| -> Option<PathBuf> {
            let file_name = ref_json["file"].as_str().unwrap_or("");
            let donor = ref_json["donor"].as_str().unwrap_or("");
            if file_name.is_empty() || donor.is_empty() {
                return None;
            }
            let candidate = archive_dir.join(donor).join(file_name);
            if candidate.exists() {
                return Some(candidate);
            }
            // Try nested under wows_replays/<version>/
            std::fs::read_dir(&wows_replays)
                .ok()?
                .flatten()
                .find_map(|e| {
                    let sub = e.path().join(file_name);
                    if sub.exists() {
                        Some(sub)
                    } else {
                        None
                    }
                })
        };

        // Collect reference JSONs sorted by file name so iteration is deterministic.
        let mut all_entries: Vec<(String, PathBuf)> = std::fs::read_dir(&extracted_dir)
            .expect("read extracted dir")
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                (name, e.path())
            })
            .collect();
        all_entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Collect up to 5 from each version bucket (15.3 and 15.4).
        let mut v15_3: Vec<(PathBuf, serde_json::Value)> = Vec::new();
        let mut v15_4: Vec<(PathBuf, serde_json::Value)> = Vec::new();
        for (_, path) in &all_entries {
            if v15_3.len() >= 5 && v15_4.len() >= 5 {
                break;
            }
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let ref_json: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let ver = ref_json["meta"]["clientVersionFromExe"]
                .as_str()
                .unwrap_or("");
            let short = parse_version_short(ver).unwrap_or_default();
            let replay_path = match locate_replay(&ref_json) {
                Some(p) => p,
                None => continue,
            };
            if short == "15.3" && v15_3.len() < 5 {
                v15_3.push((replay_path, ref_json));
            } else if short == "15.4" && v15_4.len() < 5 {
                v15_4.push((replay_path, ref_json));
            }
        }

        let n_15_3 = v15_3.len();
        let n_15_4 = v15_4.len();
        let candidates: Vec<(PathBuf, serde_json::Value)> =
            v15_3.into_iter().chain(v15_4).collect();

        eprintln!(
            "E2E: collected {} candidates (15.3: {}, 15.4: {})",
            candidates.len(),
            n_15_3,
            n_15_4
        );

        // Both version buckets must be represented so the test covers both
        // constants/sidecar compatibility across the known-good set.
        assert!(
            n_15_3 >= 1 && n_15_4 >= 1,
            "E2E requires replays from both 15.3 and 15.4; got 15.3={n_15_3}, 15.4={n_15_4}"
        );
        assert!(
            candidates.len() >= 5,
            "could not locate enough source replays; found {}",
            candidates.len()
        );

        // Decode and compare each.
        let mut failures: Vec<String> = Vec::new();
        let mut total_players_checked: usize = 0;
        let mut total_field_checks: usize = 0;

        for (replay_path, ref_json) in &candidates {
            let fname = replay_path.file_name().unwrap().to_string_lossy();
            let ver = parse_version_short(
                ref_json["meta"]["clientVersionFromExe"]
                    .as_str()
                    .unwrap_or(""),
            )
            .unwrap_or_default();

            eprintln!("  decoding {} ({})", fname, ver);

            let battle_data = decode_battle_result(replay_path, &cfg, &tables)
                .unwrap_or_else(|e| panic!("decode failed for {}: {e}", fname));

            if !battle_data.meta.warnings.is_empty() {
                eprintln!("    warnings: {:?}", battle_data.meta.warnings);
            }

            let ref_players = ref_json["players"]
                .as_object()
                .expect("ref players must be object");
            let ref_common = &ref_json["common"];

            // Check common: winner_team_id.
            let ref_winner = ref_common["winner_team_id"].as_i64();
            total_field_checks += 1;
            if battle_data.meta.winner_team != ref_winner {
                failures.push(format!(
                    "{}: winner_team: got {:?} expected {:?}",
                    fname, battle_data.meta.winner_team, ref_winner
                ));
            }

            // Check common: duration_sec.
            let ref_duration = ref_common["duration_sec"].as_i64();
            total_field_checks += 1;
            if battle_data.meta.duration_seconds != ref_duration {
                failures.push(format!(
                    "{}: duration_seconds: got {:?} expected {:?}",
                    fname, battle_data.meta.duration_seconds, ref_duration
                ));
            }

            // Per-player field checks.
            for (db_id_str, ref_player) in ref_players {
                let db_id: i64 = db_id_str.parse().unwrap_or(0);
                let got = battle_data
                    .players
                    .iter()
                    .find(|p| p.account_db_id == db_id);
                let got = match got {
                    Some(p) => p,
                    None => {
                        failures.push(format!(
                            "{}: player {db_id_str} not found in decoded output",
                            fname
                        ));
                        continue;
                    }
                };

                total_players_checked += 1;

                // Closure to compare i64 fields (also handles float-in-JSON via as_i64).
                let mut check = |field_got: Option<i64>,
                                 ref_field: &str,
                                 out_field: &str|
                 -> bool {
                    total_field_checks += 1;
                    let ref_val = ref_player[ref_field].as_i64().or_else(|| {
                        // Handle floats stored in ref JSON (e.g. 0.0 → 0)
                        ref_player[ref_field].as_f64().map(|f| f as i64)
                    });
                    if field_got != ref_val {
                        failures.push(format!(
                            "{}: player {db_id_str} {out_field}: got {field_got:?} expected {ref_val:?}",
                            fname
                        ));
                        false
                    } else {
                        true
                    }
                };

                check(got.raw_exp, "raw_exp", "raw_exp");
                check(got.exp, "exp", "exp");
                check(got.damage_dealt, "damage", "damage_dealt");
                check(got.frags, "ships_killed", "frags");
                check(got.team_id, "team_id", "team_id");
                check(got.ship_id, "vehicle_type_id", "ship_id");

                // survived: is_alive (bool in reference).
                total_field_checks += 1;
                let ref_alive = ref_player["is_alive"].as_bool();
                if got.survived != ref_alive {
                    failures.push(format!(
                        "{}: player {db_id_str} survived: got {:?} expected {:?}",
                        fname, got.survived, ref_alive
                    ));
                }

                // ── VERIFY-status composite fields (previously unchecked) ─────
                // shots_fired = shots_main_ap + shots_main_cs + shots_main_he
                {
                    total_field_checks += 1;
                    let ref_val = [
                        ref_player["shots_main_ap"].as_f64().unwrap_or(0.0),
                        ref_player["shots_main_cs"].as_f64().unwrap_or(0.0),
                        ref_player["shots_main_he"].as_f64().unwrap_or(0.0),
                    ]
                    .iter()
                    .copied()
                    .map(|v| v as i64)
                    .sum::<i64>();
                    let expected = Some(ref_val);
                    if got.shots_fired != expected {
                        failures.push(format!(
                            "{}: player {db_id_str} shots_fired: got {:?} expected {:?}",
                            fname, got.shots_fired, expected
                        ));
                    }
                }

                // hits = hits_main_ap + hits_main_cs + hits_main_he
                {
                    total_field_checks += 1;
                    let ref_val = [
                        ref_player["hits_main_ap"].as_f64().unwrap_or(0.0),
                        ref_player["hits_main_cs"].as_f64().unwrap_or(0.0),
                        ref_player["hits_main_he"].as_f64().unwrap_or(0.0),
                    ]
                    .iter()
                    .copied()
                    .map(|v| v as i64)
                    .sum::<i64>();
                    let expected = Some(ref_val);
                    if got.hits != expected {
                        failures.push(format!(
                            "{}: player {db_id_str} hits: got {:?} expected {:?}",
                            fname, got.hits, expected
                        ));
                    }
                }

                // damage_potential = agro_art + agro_tpd + agro_air + agro_dbomb
                {
                    total_field_checks += 1;
                    let ref_val = [
                        ref_player["agro_art"].as_f64().unwrap_or(0.0),
                        ref_player["agro_tpd"].as_f64().unwrap_or(0.0),
                        ref_player["agro_air"].as_f64().unwrap_or(0.0),
                        ref_player["agro_dbomb"].as_f64().unwrap_or(0.0),
                    ]
                    .iter()
                    .copied()
                    .map(|v| v as i64)
                    .sum::<i64>();
                    let expected = Some(ref_val);
                    if got.damage_potential != expected {
                        failures.push(format!(
                            "{}: player {db_id_str} damage_potential: got {:?} expected {:?}",
                            fname, got.damage_potential, expected
                        ));
                    }
                }

                // ribbons_hits = RIBBON_MAIN_CALIBER
                {
                    total_field_checks += 1;
                    let ref_val = ref_player["RIBBON_MAIN_CALIBER"].as_f64().map(|v| v as i64);
                    // ref_val is None only if the field is absent in the oracle; in that
                    // case skip rather than fail (the oracle should always have it).
                    if let Some(rv) = ref_val {
                        let expected = Some(rv);
                        if got.ribbons_hits != expected {
                            failures.push(format!(
                                "{}: player {db_id_str} ribbons_hits (RIBBON_MAIN_CALIBER): got {:?} expected {:?}",
                                fname, got.ribbons_hits, expected
                            ));
                        }
                    }
                }

                // ribbons_torpedo_hits = RIBBON_TORPEDO
                {
                    total_field_checks += 1;
                    let ref_val = ref_player["RIBBON_TORPEDO"].as_f64().map(|v| v as i64);
                    if let Some(rv) = ref_val {
                        let expected = Some(rv);
                        if got.ribbons_torpedo_hits != expected {
                            failures.push(format!(
                                "{}: player {db_id_str} ribbons_torpedo_hits (RIBBON_TORPEDO): got {:?} expected {:?}",
                                fname, got.ribbons_torpedo_hits, expected
                            ));
                        }
                    }
                }

                // ribbons_plane_kills = RIBBON_PLANE
                {
                    total_field_checks += 1;
                    let ref_val = ref_player["RIBBON_PLANE"].as_f64().map(|v| v as i64);
                    if let Some(rv) = ref_val {
                        let expected = Some(rv);
                        if got.ribbons_plane_kills != expected {
                            failures.push(format!(
                                "{}: player {db_id_str} ribbons_plane_kills (RIBBON_PLANE): got {:?} expected {:?}",
                                fname, got.ribbons_plane_kills, expected
                            ));
                        }
                    }
                }
            }

            eprintln!(
                "    {} players, {} field checks so far, {} failures",
                ref_players.len(),
                total_field_checks,
                failures.len()
            );
        }

        let matched = total_field_checks - failures.len();
        eprintln!(
            "\nE2E summary: {} replays, {} players, {}/{} field checks passed (100% = {})",
            candidates.len(),
            total_players_checked,
            matched,
            total_field_checks,
            failures.is_empty()
        );

        if !failures.is_empty() {
            panic!(
                "E2E match failures ({}/{} field checks failed):\n{}",
                failures.len(),
                total_field_checks,
                failures.join("\n")
            );
        }
    }
}
