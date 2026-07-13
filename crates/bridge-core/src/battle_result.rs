//! Replay battle-result decoder (td-28d9e7).
//!
//! Decodes a `.wowsreplay` file **in-process** with the `wows_replays` library
//! (no external process): parse the replay, load version-specific entity specs
//! from the game directory (cached per build), walk the packet stream to the
//! final `BattleResults` packet, and resolve the positional player arrays using
//! `constants.json` + `ship_index.json` + `achievement_index.json` +
//! `bonus_index.json` into a structured [`BattleData`]. `constants.json` MUST be
//! current for the game build — a stale index map silently mis-reads the RIBBON_*
//! block (see [[ribbon-fields-are-real-counts-stale-constants]]); refresh it from
//! `landaire/wows-replay-data` per build.
//!
//! # Design notes
//! - Pure Tauri-free module; all path resolution happens in the caller.
//! - `decode_battle_result` is the main entry point; `resolve_player` is
//!   `pub(crate)` so unit tests can call it directly with synthetic data.
//! - Tolerant: null / missing index / wrong type → `None`, never panics on
//!   a single bad field. The upstream packet parser can panic on
//!   malformed/incomplete replays, so the parse runs under `catch_unwind`.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use wowsunpack::data::Version;
use wowsunpack::rpc::entitydefs::EntitySpec;

// ── Known-good version set ─────────────────────────────────────────────────────

/// Pairs (major, minor) for which the bundled constants + parser are confirmed good.
const KNOWN_GOOD: &[(u32, u32)] = &[(15, 3), (15, 4), (15, 5)];

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
    /// Machine-readable trust signal for this decode. ALWAYS emitted (never
    /// skipped) so a consumer can branch on it without sniffing `warnings`.
    /// `unreliable` means a structural invariant failed (the positional layout
    /// almost certainly shifted on a new game patch) — do not trust the numbers.
    pub decode_status: DecodeStatus,
    /// Every expected-value check that did NOT hold, with its severity and a
    /// human-readable detail (expected vs actual). Empty ⇒ everything met
    /// expectations. This is the "what changed and how severe" record — a new
    /// game patch that shifts the layout lights up many `critical` checks at once.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub decode_checks: Vec<DecodeCheck>,
    /// Flat detail strings of the failing checks (back-compat with the 1.1
    /// `warnings` field; same content as `decode_checks[].detail`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Confidence in a decoded result. A future WoWS patch can shift the positional
/// field layout while the bundled `constants.json` stays stale; that produces
/// plausible-but-wrong numbers. This lets the result screen show
/// "decode unreliable / update needed" instead of rendering corrupt stats.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecodeStatus {
    /// Structural invariants hold — values are trustworthy.
    Ok,
    /// A soft check tripped (unknown game version, low ship-resolution rate, odd
    /// winner/loser XP) — probably fine, but flag it.
    Degraded,
    /// A hard invariant failed (player arrays too short, account-id anchor
    /// mismatch, or the exp/raw_exp win multiplier is wrong) — the layout almost
    /// certainly shifted; the numbers are not safe to use.
    Unreliable,
}

/// Severity of a single expected-value check.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckSeverity {
    /// Plausibility dipped (isolated outliers, unknown version) → `degraded`.
    Warn,
    /// A structural/domain expectation broke broadly → `unreliable` (layout shift).
    Critical,
}

/// One expected-value check that failed: its name, severity, and an
/// expected-vs-actual detail. The decoded `decode_status` is the worst severity
/// across all of these.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DecodeCheck {
    pub name: String,
    pub severity: CheckSeverity,
    pub detail: String,
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
    /// Aircraft this player shot down — a public per-player count present for
    /// ALL players. It is `planes_killed_by_ship` (AA) + `planes_killed_by_plane`
    /// (carrier aircraft). Replaces the former `ribbons_plane_kills`, which read
    /// the self-only `RIBBON_PLANE` ribbon — a `10000` sentinel that produced the
    /// bogus values on the post-battle plane column. (td-4b4c1a)
    pub planes_killed: Option<i64>,
    pub ribbons_hits: Option<i64>,
    pub spotting_damage: Option<i64>,
    pub damage_received: Option<i64>,
    /// Credits earned. OWNER ONLY (from privateDataList economics); `None` for
    /// every other player — a replay does not contain others' economics.
    pub credits: Option<i64>,
    pub afk: Option<bool>,
    pub survived: Option<bool>,
    pub is_self: bool,
    pub won: Option<bool>,
    /// Per-victim damage this player dealt — the attacker→victim matrix
    /// (schema 1.1). Sorted by total damage descending; only victims this player
    /// actually affected (damage, spotting, a kill, or a first-spot) are listed.
    /// "Damage RECEIVED from X" is the transpose: scan player X's `interactions`
    /// for the entry whose `target_id` equals this player's `account_db_id`.
    /// Empty when the replay carries no interaction data.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub interactions: Vec<DamageInteraction>,

    // ── Schema 1.2 additions (result-data expansion) ────────────────────────
    // Only fields validated as REAL per-player data against the replay reference
    // set are kept. `ribbons` (RIBBON_* = score sentinels), `planes` (planes_lost
    // = sentinel) and `capture` (never populated) were dropped — they emitted
    // garbage; see [[bridge-decode-patch-resilience]] / td-64aaf8.
    /// Per-player damage DEALT, bucketed with the SAME buckets as `interactions`
    /// (so the per-player total reconciles with the matrix sum, modulo buildings
    /// / environment). Always present (zero-filled) for predictable reconcile.
    pub damage_dealt_by_type: DamageDealtByType,
    /// First-spot (detection) counts, beyond the top-level `spotting_damage`.
    pub detection: Detection,
    /// Module damage this player caused to enemies.
    pub modules: Modules,

    // ── Schema 1.3 additions (battle-share cards) ───────────────────────────
    /// Main-battery damage split by shell type (he/ap/sap). Present for ALL
    /// players (zero-filled); `he + ap + sap == damage_dealt_by_type.main`.
    pub damage_main_by_shell: DamageMainByShell,
    /// OWNER-ONLY economics detail (raw scalars). `None` on every non-owner row —
    /// a replay contains only the recording player's economics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub economics: Option<Economics>,

    // ── Schema 1.4 addition (per-battle WoWS medals) ────────────────────────
    /// Achievements (WoWS medals) earned THIS battle, resolved id→name via
    /// `achievement_index.json`. Public per-player field — present for ALL
    /// players; empty `[]` when the player earned none. An id not in the
    /// bundled index (an achievement added in a newer patch) falls back to
    /// the stringified integer id so nothing is silently dropped.
    pub achievements: Vec<Achievement>,

    // ── Schema 1.5 additions (hit-quality sub-ribbons + owner detail) ────────
    /// Main-battery hit-outcome sub-ribbons — real per-player counts read from
    /// the `RIBBON_MAIN_CALIBER_*` result fields (present for ALL players,
    /// zero-filled). These approximately sum to main-caliber hits (`hits`); WG
    /// does not award an outcome sub-ribbon for every hit (HE / unclassified),
    /// so the sum runs slightly under `hits`. `citadel` is a special penetration
    /// counted separately. Correct decode depends on a current `constants.json`;
    /// a stale index map reads these as zero (see [[ribbon-fields-are-real-counts-stale-constants]]).
    pub main_hits_quality: MainHitsQuality,
    /// Secondary-battery hits this player landed (`RIBBON_SECONDARY_CALIBER`) —
    /// all players, zero-filled.
    pub secondary_hits: i64,
    /// Torpedo-protection (anti-torpedo bulge) hits absorbed *by* this player's
    /// belt is NOT this — this is hits this player's shells/torps scored against
    /// enemy bulges (`RIBBON_BULGE`). All players, zero-filled.
    pub torpedo_protection_hits: i64,
    /// OWNER-ONLY per-battle Ship Efficiency grade — `"expert"`, `"grade_1"`,
    /// `"grade_2"` or `"grade_3"` (from the private `mastery_sign`; lower int =
    /// higher grade). `None` on every non-owner row and when no grade was earned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ship_efficiency: Option<String>,
    /// OWNER-ONLY list of the economic bonuses active this battle — consumable
    /// boosters (`kind: "booster"`) and permanent ship/commander bonuses
    /// (`kind: "permanent"`), resolved from the private `subtotal_economics`
    /// modifier chains via `bonus_index.json`. `None` on non-owner rows;
    /// `Some([])` on the owner row when none were active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub economic_bonuses: Option<Vec<EconomicBonus>>,

    // ── Schema 1.6 addition (full ribbon set) ───────────────────────────────
    /// Every ribbon this player earned this battle, keyed by the game's own
    /// `RIBBON_*` constant (e.g. `{"RIBBON_BOMB": 42, "RIBBON_ASSIST": 5,
    /// "RIBBON_BASE_CAPTURE": 2}`). Present for ALL players; only non-zero
    /// ribbons are included, so a missing key means zero. This is the raw,
    /// complete ribbon set — the curated fields above (`main_hits_quality`,
    /// `secondary_hits`, `torpedo_protection_hits`, `planes_killed`, `frags`,
    /// `ribbons_torpedo_hits`) are convenience derivations of a subset. The
    /// engine maps each `RIBBON_*` key to its ribbon icon and renders anything
    /// present (bomb hits, assists, captures, crits, fires, floods, rocket
    /// hits, depth-charge hits, …). Correct decode depends on a current
    /// `constants.json`; a stale index map yields wrong/zero counts (see
    /// [[bridge-decode-patch-resilience]] / [[ribbon-fields-are-real-counts-stale-constants]]).
    pub ribbons: std::collections::BTreeMap<String, i64>,

    // ── Schema 1.7 additions (objective points + loadouts) ──────────────────
    /// WG's per-player "objective points" breakdown — every `victory_points_*`
    /// result field with a non-zero value, keyed by its WG constant name
    /// (e.g. `{"victory_points_cp_neutral_capture": 7200.0,
    /// "victory_points_kill_battleship": 6500.0,
    /// "victory_points_own_ship_kill": -2500.0}`). Sources cover cap
    /// capture/hold/block/drop, kills by victim class, victory bonuses,
    /// arms-race pickups and convoy pull/protection. Values are signed —
    /// `victory_points_own_ship_kill` is the (negative) penalty for dying.
    /// Present for ALL players; only non-zero entries are included, so a
    /// missing key means zero. Same raw-names contract as `ribbons`: the
    /// engine owns display naming.
    pub victory_points: std::collections::BTreeMap<String, f64>,
    /// The player's ship + commander loadout ([`PlayerBuild`]), read from the
    /// battle-START packet stream (arena state + vehicle-entity packets), not
    /// the results blob. Present for ALL players — a replay records every
    /// participant's loadout, enemies included. `None` when the build pass
    /// could not run (results decode is unaffected).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<PlayerBuild>,
}

/// Schema 1.2: per-player damage dealt, bucketed to mirror [`DamageInteraction`]'s
/// weapon-type buckets. All fields always serialized (zero-filled).
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct DamageDealtByType {
    pub main: i64,
    pub secondary: i64,
    pub torpedo: i64,
    pub aircraft: i64,
    pub fire: i64,
    pub flood: i64,
    pub ram: i64,
    pub depth_charge: i64,
    pub other: i64,
}

/// Schema 1.2: first-spot counts (each = `_by_ship` + `_by_plane`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct Detection {
    pub first_ships_spotted: i64,
    pub first_planes_spotted: i64,
}

/// Schema 1.2: module damage caused to enemies.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct Modules {
    pub crits: i64,
    pub major_crits: i64,
    pub breaks: i64,
    pub fires: i64,
    pub floods: i64,
}

/// Schema 1.3: main-battery damage split by shell type. `sap` is WG's "common"
/// semi-AP shell (internal name `cs`). These are public per-player fields, so the
/// split is present for ALL players (zero-filled). `he + ap + sap` equals
/// `damage_dealt_by_type.main` — it is a decomposition, not new damage.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct DamageMainByShell {
    pub he: i64,
    pub ap: i64,
    pub sap: i64,
}

/// Schema 1.3: OWNER-ONLY economics detail — raw scalars only, no precomputed
/// net/gross (the bridge stays lean; consumers derive). A replay carries
/// economics for the recording player only, so this is `Some` on the owner row
/// and `None` for every other player.
///
/// Consumer-side derivations (NOT precomputed here):
/// - net credits = `credits` (top-level, gross) − (`cost_service` + `cost_ammo`
///   + `cost_camo` + `cost_signals` + `cost_boost`).
/// - the `*_factor` fields are the premium/flag multipliers already applied.
///
/// NOT exposed: the free/ship/commander XP split. The replay's
/// `subtotal_economics` stores only bonus-multiplier breakdown objects, never the
/// final per-type XP amounts; base XP is already the top-level `raw_exp`.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Economics {
    /// Post-battle service/repair cost (COMMON_ECONOMICS `auto_repair_credits`).
    pub cost_service: i64,
    /// Ammunition auto-resupply cost (`auto_load_credits`).
    pub cost_ammo: i64,
    /// Camouflage cost (`auto_camo_credits`).
    pub cost_camo: i64,
    /// Signals/flags cost (`auto_signals_credits`).
    pub cost_signals: i64,
    /// Boosters cost (`auto_boost_credits`).
    pub cost_boost: i64,
    /// Free-XP conversion factor applied this battle (`free_exp_factor`).
    pub free_exp_factor: f64,
    /// Premium-account credit multiplier (`premium_credits_factor`).
    pub premium_credits_factor: f64,
    /// Premium-account XP multiplier (`premium_exp_factor`).
    pub premium_exp_factor: f64,
    /// WoWS-premium credit multiplier (`wows_premium_credits_factor`).
    pub wows_premium_credits_factor: f64,
    /// WoWS-premium XP multiplier (`wows_premium_exp_factor`).
    pub wows_premium_exp_factor: f64,
}

/// Schema 1.4: one achievement (WoWS medal) earned this battle, resolved from
/// the public `achievements` field ([id, count] pairs) via the bundled
/// `achievement_index.json`. `count` is almost always 1; a few achievements
/// can stack within a battle (e.g. multiple Double Strikes).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Achievement {
    /// WG's canonical achievement identifier (the GameParams entry's own
    /// `name` field, e.g. `"PCH012_Arsonist"`) — or the stringified integer
    /// id when it isn't in the bundled index. The bridge does not resolve to
    /// a display label or asset slug; the engine owns that mapping.
    pub name: String,
    pub count: i64,
}

/// Schema 1.5: main-battery hit-outcome sub-ribbons — real per-player counts from
/// the `RIBBON_MAIN_CALIBER_*` result fields. All fields always serialized
/// (zero-filled). `citadel` is `RIBBON_CITADEL` (a special penetration, counted
/// separately from `penetration`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct MainHitsQuality {
    pub penetration: i64,
    pub over_penetration: i64,
    pub no_penetration: i64,
    pub ricochet: i64,
    pub citadel: i64,
}

/// Schema 1.5: one economic bonus the OWNER had active this battle — a consumable
/// booster or a permanent ship/commander bonus. Resolved from the private
/// `subtotal_economics` modifier chains via the bundled `bonus_index.json`.
/// The bridge emits raw GameParams facts; the engine maps `index` → icon asset
/// and decides how to label the `modifiers`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EconomicBonus {
    /// GameParams index (e.g. `"PCEA021"`, `"PCEU010"`) — the engine's icon key.
    pub index: String,
    /// GameParams name (e.g. `"PCEA021_TotalXPboost_1"`).
    pub name: String,
    /// `"booster"` (consumable, `PCEA*`) or `"permanent"` (`PCEU*` ship/commander).
    pub kind: String,
    /// Rarity tier for consumable boosters (`"Common"`/`"Uncommon"`/`"Rare"`/
    /// `"Epic"`); `None` for permanent bonuses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rarity: Option<String>,
    /// Category → multiplier, e.g. `{"expFactor": 2.0}` (+100% ship XP) or
    /// `{"creditsFactor": 1.2, "expFactor": 2.0, ...}` for a multi-bonus.
    pub modifiers: std::collections::BTreeMap<String, f64>,
}

/// Schema 1.7: a player's ship + commander loadout from the battle-start
/// packets. Every value is a raw GameParams id — the bridge does NO name
/// resolution here; the engine owns id → name/icon translation (same contract
/// as `ribbons`). The ship config comes from the arena state (present for
/// every participant); the commander's learned skills travel on the vehicle
/// entity, which only spawns client-side once the ship enters the recording
/// client's awareness — so `commander_skills` can be empty for an enemy that
/// was never observed, while the rest of the build is still populated.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct PlayerBuild {
    /// The ship's own GameParams id (authoritative, from the config blob).
    pub ship_id: i64,
    /// Mounted module GameParams ids (the ship's unit slots — hull,
    /// artillery, torpedoes, fire control, engine, …) in blob order; empty
    /// slots omitted.
    pub modules: Vec<i64>,
    /// Equipped upgrade ("modernization") GameParams ids, slot order.
    pub upgrades: Vec<i64>,
    /// Mounted consumable GameParams ids.
    pub consumables: Vec<i64>,
    /// Exterior-slot GameParams ids — signal flags AND camouflage in one list,
    /// as the game stores them; the engine splits by param type.
    pub exteriors: Vec<i64>,
    /// Mounted ensign GameParams ids.
    pub ensigns: Vec<i64>,
    /// Active economic-booster GameParams ids (resolvable through the same
    /// `bonus_index.json` used for the owner's `economic_bonuses`).
    pub eco_boosts: Vec<i64>,
    /// The commander's GameParams id (identifies unique commanders). `None`
    /// when no commander data was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commander_id: Option<i64>,
    /// Learned commander-skill type ids for THIS ship's class (a commander
    /// keeps a separate skill set per class). Empty when the vehicle entity
    /// was never observed (see struct docs).
    pub commander_skills: Vec<i64>,
    /// Commander points SPENT: Σ of each learned skill's row cost (1–4) from
    /// the bundled `SKILLS_BY_SHIP_TYPE` grid. `None` when `commander_skills`
    /// is empty or any id is unknown to the bundled constants — accurate or
    /// absent, never guessed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commander_points: Option<i64>,
}

/// One attacker→victim damage record, resolved from a player's
/// `CLIENT_VEH_INTERACTION_DETAILS` array. Damage is split into weapon-type
/// buckets; zero buckets are omitted from the JSON to keep the payload small.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct DamageInteraction {
    /// Victim's `account_db_id`. Join to `players[].account_db_id` for the
    /// victim's ship (name / tier / class) and side.
    pub target_id: i64,
    /// Total HP damage dealt to this victim (sum of the weapon-type buckets).
    pub damage: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub damage_main: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub damage_secondary: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub damage_torpedo: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub damage_aircraft: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub damage_fire: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub damage_flood: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub damage_ram: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub damage_depth_charge: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub damage_other: i64,
    /// Spotting (scouting) damage credited against this victim.
    #[serde(skip_serializing_if = "is_zero")]
    pub spotting_damage: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub fires: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub floods: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub crits: i64,
    #[serde(skip_serializing_if = "is_zero")]
    pub citadels: i64,
    /// This player landed the killing blow on the victim.
    #[serde(skip_serializing_if = "is_false")]
    pub killed: bool,
    /// This player got the first-spot (detection) on the victim.
    #[serde(skip_serializing_if = "is_false")]
    pub spotted: bool,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}

// Weapon-type damage buckets, by `CLIENT_VEH_INTERACTION_DETAILS` field name.
// Summed by name (not fixed index) so the grouping survives field reordering.
const DMG_MAIN: &[&str] = &["damage_main_ap", "damage_main_cs", "damage_main_he"];
const DMG_SECONDARY: &[&str] = &[
    "damage_atba_ap",
    "damage_atba_cs",
    "damage_atba_he",
    "damage_atba_ap_manual",
    "damage_atba_cs_manual",
    "damage_atba_he_manual",
];
const DMG_TORPEDO: &[&str] = &["damage_tpd_normal", "damage_tpd_deep", "damage_tpd_alter"];
// BASE fields only. WG republishes each aircraft weapon's damage on a
// delivery-mode breakdown (_avia = carrier squadron, _alt, _airsupport =
// airstrike consumable) alongside the base field — base == whichever mode
// fired (or avia+alt when split), never additive. Summing base + variants
// double-counts the same hits (proven against 45,574 corpus rows: bucket
// sum only reconciles to WG's authoritative `damage` field when the
// variants are excluded).
const DMG_AIRCRAFT: &[&str] = &[
    "damage_bomb",
    "damage_tbomb",
    "damage_rocket",
    "damage_skip",
];
const DMG_FIRE: &[&str] = &["damage_fire"];
const DMG_FLOOD: &[&str] = &["damage_flood"];
const DMG_RAM: &[&str] = &["damage_ram"];
// damage_dbomb_airsupport dropped: it duplicates damage_adbomb (in DMG_OTHER
// below), the same airstrike depth-charge hits published twice by WG.
const DMG_DEPTH_CHARGE: &[&str] = &["damage_dbomb_direct", "damage_dbomb_splash"];
const DMG_OTHER: &[&str] = &[
    "damage_sea_mine",
    "damage_wave",
    "damage_charge_laser",
    "damage_pulse_laser",
    "damage_axis_laser",
    "damage_phaser_laser",
    "damage_event_1",
    "damage_event_2",
    "damage_adbomb", // airstrike depth-charge damage; counted once here, not in DMG_DEPTH_CHARGE
    "damage_missile",
];

/// Resolve a player's `interactions` value ({victim_id → field array}) into the
/// attacker→victim matrix. Tolerant: bad entries are skipped, not fatal.
/// Empty/no-effect interactions (zero damage, no spot, no kill) are dropped.
fn build_interactions(
    interactions_val: Option<&serde_json::Value>,
    tables: &Tables,
) -> Vec<DamageInteraction> {
    let obj = match interactions_val.and_then(|v| v.as_object()) {
        Some(o) => o,
        None => return Vec::new(),
    };
    let idx = &tables.interaction_index;
    let mut out: Vec<DamageInteraction> = Vec::with_capacity(obj.len());

    for (vid_str, varr_val) in obj {
        let target_id: i64 = match vid_str.parse() {
            Ok(i) => i,
            Err(_) => continue,
        };
        let varr = match varr_val.as_array() {
            Some(a) => a,
            None => continue,
        };
        let get = |name: &str| -> i64 {
            idx.get(name)
                .and_then(|&i| varr.get(i))
                .and_then(to_i64_tolerant)
                .unwrap_or(0)
        };
        let sum = |names: &[&str]| -> i64 { names.iter().map(|n| get(n)).sum() };
        // Flags may serialise as bool OR 0/1 int — use the bool-tolerant coercion.
        let get_bool = |name: &str| -> bool {
            idx.get(name)
                .and_then(|&i| varr.get(i))
                .and_then(to_bool_tolerant)
                .unwrap_or(false)
        };

        let damage_main = sum(DMG_MAIN);
        let damage_secondary = sum(DMG_SECONDARY);
        let damage_torpedo = sum(DMG_TORPEDO);
        let damage_aircraft = sum(DMG_AIRCRAFT);
        let damage_fire = sum(DMG_FIRE);
        let damage_flood = sum(DMG_FLOOD);
        let damage_ram = sum(DMG_RAM);
        let damage_depth_charge = sum(DMG_DEPTH_CHARGE);
        let damage_other = sum(DMG_OTHER);
        let damage = damage_main
            + damage_secondary
            + damage_torpedo
            + damage_aircraft
            + damage_fire
            + damage_flood
            + damage_ram
            + damage_depth_charge
            + damage_other;

        let spotting_damage = get("scouting_damage");
        let fires = get("fires");
        let floods = get("floods");
        let crits = get("crits");
        let citadels = get("citadels");
        let killed = get_bool("ship_killed");
        let spotted =
            get_bool("is_primary_spotted_by_ship") || get_bool("is_primary_spotted_by_plane");

        // Drop interactions with no observable effect (the array often contains a
        // slot for every enemy, most all-zero).
        if damage == 0
            && spotting_damage == 0
            && fires == 0
            && floods == 0
            && crits == 0
            && citadels == 0
            && !killed
            && !spotted
        {
            continue;
        }

        out.push(DamageInteraction {
            target_id,
            damage,
            damage_main,
            damage_secondary,
            damage_torpedo,
            damage_aircraft,
            damage_fire,
            damage_flood,
            damage_ram,
            damage_depth_charge,
            damage_other,
            spotting_damage,
            fires,
            floods,
            crits,
            citadels,
            killed,
            spotted,
        });
    }

    // Highest-damage victims first — convenient for a result screen.
    out.sort_by(|a, b| b.damage.cmp(&a.damage).then(a.target_id.cmp(&b.target_id)));
    out
}

/// HP-damage fields a player deals TO a structure, in `CLIENT_BUILDING_INTERACTION_DETAILS`.
const BUILDING_DMG: &[&str] = &[
    "building_damage_main_he",
    "building_damage_main_ap",
    "building_damage_main_cs",
    "building_damage_flood",
    "building_damage_fire",
    "building_damage_bomb_ap",
    "building_damage_bomb_he",
    "building_damage_tbomb",
];

/// Schema 1.2: total HP damage a player dealt to structures, summed over their
/// `buildingInteractions` ({building_id → CLIENT_BUILDING_INTERACTION_DETAILS array}).
/// Returns 0 when the player hit no buildings or the field is absent. Tolerant.
fn sum_building_damage(val: Option<&serde_json::Value>, tables: &Tables) -> i64 {
    let obj = match val.and_then(|v| v.as_object()) {
        Some(o) => o,
        None => return 0,
    };
    let idx = &tables.building_interaction_index;
    let mut total = 0i64;
    for (_bid, barr_val) in obj {
        let barr = match barr_val.as_array() {
            Some(a) => a,
            None => continue,
        };
        for name in BUILDING_DMG {
            if let Some(&i) = idx.get(*name) {
                total += barr.get(i).and_then(to_i64_tolerant).unwrap_or(0);
            }
        }
    }
    total
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
    /// The WoWS game install dir. Version-specific entity specs (needed to parse
    /// the packet stream) are loaded from here and cached per game build.
    pub game_dir: PathBuf,
    pub constants_path: PathBuf,
    pub ship_index_path: PathBuf,
    pub achievement_index_path: PathBuf,
    pub bonus_index_path: PathBuf,
}

pub struct Tables {
    pub public_indices: HashMap<String, usize>,
    pub common_results: Vec<String>,
    pub interaction_details: Vec<String>,
    /// name → position in `interaction_details`, for resolving the per-victim
    /// `CLIENT_VEH_INTERACTION_DETAILS` arrays (the attacker→victim matrix).
    pub interaction_index: HashMap<String, usize>,
    /// name → position in `CLIENT_BUILDING_INTERACTION_DETAILS`, for resolving a
    /// player's `buildingInteractions` arrays (schema 1.2 `damage_to_buildings`).
    pub building_interaction_index: HashMap<String, usize>,
    pub private_results: Vec<String>,
    /// INIT_ECONOMICS_INDICES (name → index) for the owner-only economics array.
    pub init_economics_indices: HashMap<String, usize>,
    /// COMMON_ECONOMICS_INDICES (name → index) for the owner-only common-economics
    /// array (schema 1.3 expenses + premium multipliers).
    pub common_economics_indices: HashMap<String, usize>,
    pub ships: HashMap<String, ShipInfo>,
    /// achievement_id (as string) → WG name, from `achievement_index.json`.
    /// Flat map — unlike `ships`, achievements carry no tier/class enrichment.
    pub achievements: HashMap<String, String>,
    /// economic-bonus GameParams id → resolved bonus info, from `bonus_index.json`
    /// (schema 1.5 owner economic bonuses).
    pub bonus_index: HashMap<i64, BonusInfo>,
    /// species ("Battleship", …) → skill-type id → point cost (1–4), from
    /// constants' `SKILLS_BY_SHIP_TYPE` (the commander grid: four rows costing
    /// 1–4 points). Used for schema 1.7 `commander_points`.
    pub skill_costs: HashMap<String, HashMap<i64, i64>>,
}

/// A resolved economic-bonus entry from `bonus_index.json` (schema 1.5).
pub struct BonusInfo {
    pub index: String,
    pub name: String,
    pub kind: String,
    pub rarity: Option<String>,
    pub modifiers: std::collections::BTreeMap<String, f64>,
}

pub struct ShipInfo {
    pub index: String,
    pub level: i64,
    pub species: String,
}

impl Tables {
    pub fn load(
        constants_path: &Path,
        ship_index_path: &Path,
        achievement_index_path: &Path,
        bonus_index_path: &Path,
    ) -> Result<Tables, DecodeError> {
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
        let interaction_details: Vec<String> = constants
            .get("CLIENT_VEH_INTERACTION_DETAILS")
            .and_then(|v| v.as_array())
            .unwrap_or(&vec![])
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();
        let interaction_index: HashMap<String, usize> = interaction_details
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), i))
            .collect();

        // CLIENT_BUILDING_INTERACTION_DETAILS: ordered array → name → index map.
        let building_interaction_index: HashMap<String, usize> = constants
            .get("CLIENT_BUILDING_INTERACTION_DETAILS")
            .and_then(|v| v.as_array())
            .unwrap_or(&vec![])
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.as_str().map(|s| (s.to_string(), i)))
            .collect();

        // PLAYER_PRIVATE_RESULTS: ordered array (is_afk at index 37)
        let private_results = constants
            .get("PLAYER_PRIVATE_RESULTS")
            .and_then(|v| v.as_array())
            .unwrap_or(&vec![])
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();

        // INIT_ECONOMICS_INDICES: object {name → index} (owner-only economics)
        let mut init_economics_indices = HashMap::new();
        if let Some(obj) = constants
            .get("INIT_ECONOMICS_INDICES")
            .and_then(|v| v.as_object())
        {
            for (k, v) in obj {
                if let Some(idx) = v.as_u64() {
                    init_economics_indices.insert(k.clone(), idx as usize);
                }
            }
        }

        // COMMON_ECONOMICS_INDICES: object {name → index} (owner-only economics —
        // expenses + premium multipliers, schema 1.3)
        let mut common_economics_indices = HashMap::new();
        if let Some(obj) = constants
            .get("COMMON_ECONOMICS_INDICES")
            .and_then(|v| v.as_object())
        {
            for (k, v) in obj {
                if let Some(idx) = v.as_u64() {
                    common_economics_indices.insert(k.clone(), idx as usize);
                }
            }
        }

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

        // ── achievement_index.json (schema 1.4) ────────────────────────────────
        // Flat {achievement_id → WG name} map — unlike ship_index.json there is
        // no tier/class enrichment to carry, just the id→name resolution.
        let achievement_bytes = fs::read(achievement_index_path).map_err(|e| {
            DecodeError::Resources(format!("cannot read achievement_index.json: {e}"))
        })?;
        let achievement_json: serde_json::Value = serde_json::from_slice(&achievement_bytes)
            .map_err(|e| {
                DecodeError::Resources(format!("achievement_index.json parse error: {e}"))
            })?;

        let mut achievements = HashMap::new();
        if let Some(obj) = achievement_json.as_object() {
            for (id_str, name_val) in obj {
                if let Some(name) = name_val.as_str() {
                    achievements.insert(id_str.clone(), name.to_string());
                }
            }
        }

        // ── bonus_index.json (schema 1.5) ──────────────────────────────────────
        // {id (as string) → {index, name, kind, rarity, modifiers}} — economic
        // boosters + permanent ship/commander bonuses, resolved from GameParams.
        let bonus_bytes = fs::read(bonus_index_path)
            .map_err(|e| DecodeError::Resources(format!("cannot read bonus_index.json: {e}")))?;
        let bonus_json: serde_json::Value = serde_json::from_slice(&bonus_bytes)
            .map_err(|e| DecodeError::Resources(format!("bonus_index.json parse error: {e}")))?;
        let mut bonus_index: HashMap<i64, BonusInfo> = HashMap::new();
        if let Some(obj) = bonus_json.as_object() {
            for (id_str, info) in obj {
                let Ok(id) = id_str.parse::<i64>() else {
                    continue;
                };
                let modifiers = info
                    .get("modifiers")
                    .and_then(|v| v.as_object())
                    .map(|m| {
                        m.iter()
                            .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                            .collect()
                    })
                    .unwrap_or_default();
                bonus_index.insert(
                    id,
                    BonusInfo {
                        index: info
                            .get("index")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: info
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        kind: info
                            .get("kind")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        rarity: info
                            .get("rarity")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        modifiers,
                    },
                );
            }
        }

        // ── SKILLS_BY_SHIP_TYPE (schema 1.7) ───────────────────────────────────
        // {species → [row0..row3]}; each row is a {column → [skill ids]} group
        // and row index + 1 is the skill's point cost (the commander grid has
        // four rows costing 1–4 points). Missing/odd shapes → empty map, which
        // makes `commander_points` resolve to None (accurate or absent).
        let mut skill_costs: HashMap<String, HashMap<i64, i64>> = HashMap::new();
        if let Some(obj) = constants
            .get("SKILLS_BY_SHIP_TYPE")
            .and_then(|v| v.as_object())
        {
            for (species, rows) in obj {
                let Some(rows) = rows.as_array() else {
                    continue;
                };
                let map = skill_costs.entry(species.clone()).or_default();
                for (row_i, row) in rows.iter().enumerate() {
                    let Some(groups) = row.as_object() else {
                        continue;
                    };
                    for ids in groups.values() {
                        let Some(ids) = ids.as_array() else { continue };
                        for id in ids {
                            if let Some(id) = id.as_i64() {
                                map.insert(id, row_i as i64 + 1);
                            }
                        }
                    }
                }
            }
        }

        Ok(Tables {
            public_indices,
            common_results,
            interaction_details,
            interaction_index,
            building_interaction_index,
            private_results,
            init_economics_indices,
            common_economics_indices,
            ships,
            achievements,
            bonus_index,
            skill_costs,
        })
    }
}

// ── Error ──────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("resource error: {0}")]
    Resources(String),
    #[error("no BattleResults packet (battle not finished / left early / non-pvp)")]
    NoBattleResults,
    #[error("malformed replay or results: {0}")]
    Malformed(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ── SHA-256 (identical to src-tauri/uploader.rs::sha256_hex) ──────────────────

/// Lowercase hex SHA-256 of `bytes` — the source_file_hash and donation dedup key.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// ── Main decode function ───────────────────────────────────────────────────────

/// Decode a `.wowsreplay` file into structured [`BattleData`], in-process.
///
/// Reads the replay bytes (for the source hash), parses the replay with the
/// `wows_replays` library, walks the packet stream to the final `BattleResults`
/// packet, and resolves players via `tables`. The parse runs under
/// `catch_unwind` because the upstream parser can panic on malformed or
/// incomplete (early-leave / in-progress) replays.
pub fn decode_battle_result(
    replay_path: &Path,
    cfg: &DecodeConfig,
    tables: &Tables,
) -> Result<BattleData, DecodeError> {
    let replay_bytes = fs::read(replay_path)?;
    let source_file_hash = sha256_hex(&replay_bytes);

    let game_dir = cfg.game_dir.clone();
    // The upstream packet parser indexes into entity-spec tables and can panic
    // on malformed/incomplete input, so isolate it behind catch_unwind and turn
    // any panic into a clean error instead of unwinding the bridge thread.
    let extracted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        extract_battle_results(&replay_bytes, &game_dir)
    }))
    .unwrap_or_else(|_| {
        Err(DecodeError::Malformed(
            "replay parser panicked (incomplete or unsupported replay)".into(),
        ))
    });

    let (meta_value, br) = extracted?;

    // Second pass — ship/commander loadouts from the battle-START packets
    // (schema 1.7). Deliberately isolated: a panic or error here degrades to
    // "no builds" (players serialize without `build`), never failing the
    // results decode that the first pass already secured.
    let builds = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        extract_builds(&replay_bytes, &game_dir)
    }))
    .unwrap_or_else(|_| {
        Err(DecodeError::Malformed(
            "build extraction panicked (incomplete or unsupported replay)".into(),
        ))
    })
    .unwrap_or_else(|e| {
        log::warn!("build extraction failed (serving results without builds): {e}");
        HashMap::new()
    });

    build_battle_data(
        Some(meta_value),
        br,
        source_file_hash,
        replay_path,
        tables,
        &builds,
    )
}

/// Parse a replay's bytes in-process and return `(meta JSON, BattleResults JSON)`.
/// Loads entity specs for the replay's version from `game_dir` (cached per build).
fn extract_battle_results(
    replay_bytes: &[u8],
    game_dir: &Path,
) -> Result<(serde_json::Value, serde_json::Value), DecodeError> {
    use wows_replays::packet2::{PacketType, Parser};
    use wows_replays::ReplayFile;

    let replay = ReplayFile::from_bytes(replay_bytes)
        .map_err(|e| DecodeError::Malformed(format!("replay parse failed: {e:?}")))?;
    let meta_value = serde_json::to_value(&replay.meta)
        .map_err(|e| DecodeError::Malformed(format!("meta serialise failed: {e}")))?;

    let version = Version::from_client_exe(replay.meta.clientVersionFromExe.as_str());
    let specs = load_specs(game_dir, &version)?;

    let mut parser = Parser::with_version(specs.as_slice(), version);
    let mut remaining = replay.packet_data.as_slice();
    let mut br_str: Option<String> = None;
    while !remaining.is_empty() {
        match parser.parse_packet(&mut remaining) {
            Ok(packet) => {
                if let PacketType::BattleResults(s) = &packet.payload {
                    br_str = Some(s.to_string()); // keep the LAST (most complete)
                }
            }
            // Truncated/incomplete stream (early-leave / in-progress): stop
            // walking and use whatever BattleResults we already saw, if any.
            Err(_) => break,
        }
    }

    let br_str = br_str.ok_or(DecodeError::NoBattleResults)?;
    let br = serde_json::from_str(&br_str)
        .map_err(|e| DecodeError::Malformed(format!("BattleResults inner JSON: {e}")))?;
    Ok((meta_value, br))
}

/// Load (and cache, per game build) the entity specs needed to parse a replay's
/// packet stream. Specs come from the user's game install — the packet parser
/// cannot walk the stream without them.
fn load_specs(game_dir: &Path, version: &Version) -> Result<Arc<Vec<EntitySpec>>, DecodeError> {
    static CACHE: OnceLock<Mutex<HashMap<u32, Arc<Vec<EntitySpec>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    // `Version::build` became `Option<NonZero<u32>>` in a newer wows-toolkit;
    // flatten it back to the plain build-number cache key (0 when unknown).
    let key = version.build.map(|build| build.get()).unwrap_or(0);

    if let Some(specs) = cache.lock().unwrap().get(&key) {
        return Ok(Arc::clone(specs));
    }
    // Load outside the lock (this is the slow part and may panic upstream).
    let resources = wowsunpack::game_data::load_game_resources(game_dir, version)
        .map_err(|e| DecodeError::Resources(format!("load game specs (build {key}): {e:?}")))?;
    let specs = Arc::new(resources.specs);
    cache.lock().unwrap().insert(key, Arc::clone(&specs));
    Ok(specs)
}

// ── Build extraction (schema 1.7) ──────────────────────────────────────────────

/// A player's raw loadout from the battle-START packet stream — intermediate
/// form, GameParams ids only. Resolved to the wire [`PlayerBuild`] (per-class
/// skill selection + point costing) by [`make_player_build`].
pub(crate) struct RawBuild {
    pub ship_id: i64,
    pub modules: Vec<i64>,
    pub upgrades: Vec<i64>,
    pub consumables: Vec<i64>,
    pub exteriors: Vec<i64>,
    pub ensigns: Vec<i64>,
    pub eco_boosts: Vec<i64>,
    /// 0 = unknown (no crew data seen for this player).
    pub commander_id: i64,
    /// species key ("Battleship", …) → learned skill-type ids. Empty when the
    /// vehicle entity was never observed (skills travel on the entity, not the
    /// arena state), or pre-populated species arrays were empty.
    pub skills_by_species: HashMap<String, Vec<i64>>,
}

/// Walk the packet stream a second time and extract every player's ship +
/// commander loadout, keyed by `account_db_id`.
///
/// Reuses the wows-toolkit building blocks end to end — no hand-rolled packet
/// matching (see [[prefer-wows-toolkit]]): `fold_props_into` parses vehicle
/// EntityCreate/CellPlayerCreate/BasePlayerCreate/EntityProperty packets into
/// per-entity `VehicleFacts` (ship config + crew skills), and the decoded
/// `onArenaStateReceived` supplies the entity_id → db_id join plus two
/// all-players fallbacks the entity packets can't guarantee: `shipConfigDump`
/// (config for never-observed enemies) and `crewParams` (commander id).
///
/// Callers run this under `catch_unwind` — like the results pass, the upstream
/// parser can panic on malformed input; a failure here degrades to "no builds"
/// and never blocks the results decode.
fn extract_builds(
    replay_bytes: &[u8],
    game_dir: &Path,
) -> Result<HashMap<i64, RawBuild>, DecodeError> {
    use wows_replays::analyzer::battle_controller::merged::{fold_props_into, VehicleFacts};
    use wows_replays::analyzer::battle_controller::EntityType;
    use wows_replays::analyzer::decoder::{DecodedPacketPayload, PacketDecoder};
    use wows_replays::game_constants::DEFAULT_GAME_CONSTANTS;
    use wows_replays::packet2::{PacketType, Parser};
    use wows_replays::types::EntityId;
    use wows_replays::ReplayFile;
    use wowsunpack::data::ship_config::{parse_ship_config, ShipConfig};
    use wowsunpack::game_types::GameParamId;

    let replay = ReplayFile::from_bytes(replay_bytes)
        .map_err(|e| DecodeError::Malformed(format!("replay parse failed: {e:?}")))?;
    let version = Version::from_client_exe(replay.meta.clientVersionFromExe.as_str());
    let specs = load_specs(game_dir, &version)?;
    let constants = &*DEFAULT_GAME_CONSTANTS;
    let decoder = PacketDecoder::builder().version(version).build();

    // Per-vehicle facts (ship config + crew skills), keyed by entity id.
    let mut facts: HashMap<EntityId, VehicleFacts> = HashMap::new();
    // Arena-state roster: db_id → (entity id, config fallback, commander id).
    struct ArenaSeed {
        entity_id: EntityId,
        config_dump: Option<Vec<u8>>,
        commander_id: i64,
    }
    let mut seeds: HashMap<i64, ArenaSeed> = HashMap::new();

    let mut parser = Parser::with_version(specs.as_slice(), version);
    let mut remaining = replay.packet_data.as_slice();
    while !remaining.is_empty() {
        let Ok(packet) = parser.parse_packet(&mut remaining) else {
            break; // truncated stream — use what we have
        };
        match &packet.payload {
            PacketType::EntityCreate(ec) => {
                if matches!(
                    ec.entity_type.parse::<EntityType>(),
                    Ok(EntityType::Vehicle)
                ) {
                    fold_props_into(&mut facts, ec.entity_id, &ec.props, version, constants);
                }
            }
            PacketType::CellPlayerCreate(cell) => {
                if matches!(
                    cell.entity_type.parse::<EntityType>(),
                    Ok(EntityType::Vehicle)
                ) {
                    fold_props_into(&mut facts, cell.entity_id, &cell.props, version, constants);
                }
            }
            PacketType::BasePlayerCreate(base) => {
                if matches!(
                    base.entity_type.parse::<EntityType>(),
                    Ok(EntityType::Vehicle)
                ) {
                    fold_props_into(&mut facts, base.entity_id, &base.props, version, constants);
                }
            }
            PacketType::EntityProperty(ep) => {
                // Fold single-property updates too: shipConfig (and crew) can
                // arrive as a later property update instead of on the create
                // packet. Same first-wins semantics as the create packets.
                let mut single = HashMap::new();
                single.insert(ep.property, ep.value.clone());
                fold_props_into(&mut facts, ep.entity_id, &single, version, constants);
            }
            PacketType::EntityMethod(em) if em.method == "onArenaStateReceived" => {
                let decoded = decoder.decode(&packet);
                if let DecodedPacketPayload::OnArenaStateReceived {
                    player_states,
                    bot_states,
                    ..
                } = decoded.payload
                {
                    for st in player_states.iter().chain(bot_states.iter()) {
                        // crewParams = [commander GameParams id, [flags]] —
                        // present for every participant in the arena state.
                        let commander_id = st
                            .raw_with_names()
                            .get("crewParams")
                            .and_then(|v| v.as_array())
                            .and_then(|a| a.first())
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        seeds.insert(
                            st.db_id().raw(),
                            ArenaSeed {
                                entity_id: st.entity_id(),
                                config_dump: st.ship_config_dump(),
                                commander_id,
                            },
                        );
                    }
                }
            }
            _ => {}
        }
    }

    if seeds.is_empty() {
        return Err(DecodeError::Malformed(
            "no onArenaStateReceived packet (cannot map builds to players)".into(),
        ));
    }

    // Join: arena roster ← vehicle facts, with arena-state fallbacks.
    let to_ids = |v: &[GameParamId]| -> Vec<i64> {
        v.iter()
            .map(|id| id.raw() as i64)
            .filter(|&x| x != 0)
            .collect()
    };
    let mut out: HashMap<i64, RawBuild> = HashMap::with_capacity(seeds.len());
    for (db_id, seed) in seeds {
        let f = facts.get(&seed.entity_id);
        // Ship config: prefer the vehicle-entity copy; fall back to the arena
        // state's shipConfigDump (present even for never-observed enemies).
        let cfg: Option<ShipConfig> = f
            .map(|f| f.ship_config.clone())
            .filter(|c| c.ship_params_id().raw() != 0)
            .or_else(|| {
                seed.config_dump
                    .as_deref()
                    .and_then(|blob| parse_ship_config(blob, &version).ok())
            });
        let Some(cfg) = cfg else { continue };

        let crew = f.map(|f| &f.crew);
        let commander_id = crew
            .map(|c| c.params_id().raw() as i64)
            .filter(|&x| x != 0)
            .unwrap_or(seed.commander_id);

        let mut skills_by_species: HashMap<String, Vec<i64>> = HashMap::new();
        if let Some(crew) = crew {
            let sk = crew.learned_skills();
            for (key, arr) in [
                ("AirCarrier", sk.aircraft_carrier()),
                ("Battleship", sk.battleship()),
                ("Cruiser", sk.cruiser()),
                ("Destroyer", sk.destroyer()),
                ("Auxiliary", sk.auxiliary()),
                ("Submarine", sk.submarine()),
            ] {
                if !arr.is_empty() {
                    skills_by_species
                        .insert(key.to_string(), arr.iter().map(|&x| x as i64).collect());
                }
            }
        }

        out.insert(
            db_id,
            RawBuild {
                ship_id: cfg.ship_params_id().raw() as i64,
                modules: to_ids(cfg.units()),
                upgrades: to_ids(cfg.modernization()),
                consumables: to_ids(cfg.abilities()),
                exteriors: to_ids(cfg.exteriors()),
                ensigns: to_ids(cfg.ensigns()),
                eco_boosts: to_ids(cfg.ecoboosts()),
                commander_id,
                skills_by_species,
            },
        );
    }
    Ok(out)
}

/// Resolve a [`RawBuild`] to the wire [`PlayerBuild`]: pick the learned-skill
/// set matching the ship's class and cost it via the `SKILLS_BY_SHIP_TYPE`
/// grid. Tolerant: unknown class / unknown skill id → `commander_points: None`.
pub(crate) fn make_player_build(
    raw: &RawBuild,
    ship_class: Option<ShipClass>,
    skill_costs: &HashMap<String, HashMap<i64, i64>>,
) -> PlayerBuild {
    let species_key = ship_class.map(|c| match c {
        ShipClass::AirCarrier => "AirCarrier",
        ShipClass::Battleship => "Battleship",
        ShipClass::Cruiser => "Cruiser",
        ShipClass::Destroyer => "Destroyer",
        ShipClass::Submarine => "Submarine",
        ShipClass::Auxiliary => "Auxiliary",
    });
    let commander_skills: Vec<i64> = species_key
        .and_then(|k| raw.skills_by_species.get(k))
        .cloned()
        .unwrap_or_default();
    // Points SPENT = Σ row cost of each learned skill. None unless every id
    // resolves through the bundled grid — accurate or absent, never guessed.
    let commander_points: Option<i64> = if commander_skills.is_empty() {
        None
    } else {
        species_key
            .and_then(|k| skill_costs.get(k))
            .and_then(|costs| {
                commander_skills
                    .iter()
                    .map(|id| costs.get(id).copied())
                    .sum::<Option<i64>>()
            })
    };
    PlayerBuild {
        ship_id: raw.ship_id,
        modules: raw.modules.clone(),
        upgrades: raw.upgrades.clone(),
        consumables: raw.consumables.clone(),
        exteriors: raw.exteriors.clone(),
        ensigns: raw.ensigns.clone(),
        eco_boosts: raw.eco_boosts.clone(),
        commander_id: (raw.commander_id != 0).then_some(raw.commander_id),
        commander_skills,
        commander_points,
    }
}

/// Test-only helper: parse a JSONL dump (sidecar-style) into the `(meta, br)`
/// pair, then delegate to [`build_battle_data`]. Production decoding uses the
/// in-process [`extract_battle_results`]; this preserves the JSONL-based unit
/// tests against [`build_battle_data`].
#[cfg(test)]
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
    // JSONL dumps exercise the results path only — no packet stream, no builds.
    build_battle_data(
        meta_obj,
        br,
        source_file_hash,
        replay_path,
        tables,
        &HashMap::new(),
    )
}

/// Structural counters gathered while resolving players, fed to the self-check.
struct LayoutStats {
    n_players: usize,
    n_arr_short: usize,
    n_anchor_checked: usize,
    n_anchor_match: usize,
    max_pub_idx: usize,
}

/// Expected-value security check: compare the decoded battle against everything
/// we expect to be true, and return each expectation that did NOT hold with a
/// severity. This is the patch-resilience tripwire — a game update that shifts
/// the positional layout breaks many of these at once. Robust, version-stable
/// expectations only (game rules / ranges / relationships, NOT economy specifics)
/// so it stays valid across patches and flags *deviation from expectation*.
fn run_self_checks(
    players: &[BattlePlayer],
    winner_team: Option<i64>,
    game_version_short: Option<&str>,
    layout: &LayoutStats,
) -> Vec<DecodeCheck> {
    let mut checks: Vec<DecodeCheck> = Vec::new();
    // Inline push macros (NOT closures): a closure capturing `checks` by &mut
    // would hold that borrow for its whole body and conflict with the other
    // pushes; a macro borrows `checks` only momentarily at each call site.
    macro_rules! warn {
        ($n:expr, $d:expr $(,)?) => {
            checks.push(DecodeCheck {
                name: $n.into(),
                severity: CheckSeverity::Warn,
                detail: $d,
            })
        };
    }
    macro_rules! crit {
        ($n:expr, $d:expr $(,)?) => {
            checks.push(DecodeCheck {
                name: $n.into(),
                severity: CheckSeverity::Critical,
                detail: $d,
            })
        };
    }
    // viol of total players → Warn for an isolated outlier, Critical when the
    // majority violate (a systematic / layout-shift signature).
    macro_rules! domain {
        ($n:expr, $viol:expr, $total:expr, $what:expr $(,)?) => {{
            let (viol, total) = ($viol, $total);
            if viol > 0 && total > 0 {
                let sev = if viol * 2 > total {
                    CheckSeverity::Critical
                } else {
                    CheckSeverity::Warn
                };
                checks.push(DecodeCheck {
                    name: $n.into(),
                    severity: sev,
                    detail: format!("{viol}/{total} players: {}", $what),
                });
            }
        }};
    }
    let n = layout.n_players;
    let count = |pred: &dyn Fn(&BattlePlayer) -> bool| players.iter().filter(|p| pred(p)).count();

    // ── Structural anchors (a global index shift breaks these) ────────────────
    if n >= 4 {
        if layout.n_arr_short * 2 > n {
            crit!(
                "array_length",
                format!(
                    "{}/{} player arrays shorter than the expected layout (need index > {})",
                    layout.n_arr_short, n, layout.max_pub_idx
                )
            );
        }
        if layout.n_anchor_checked >= 4 && layout.n_anchor_match * 2 < layout.n_anchor_checked {
            crit!(
                "account_id_anchor",
                format!(
                    "account_db_id at array[0] mismatched the player key in {}/{} players",
                    layout.n_anchor_checked - layout.n_anchor_match,
                    layout.n_anchor_checked
                )
            );
        }
    }

    // ── exp/raw_exp win multiplier (sharpest one-slot-shift detector) ─────────
    if let Some(wt) = winner_team {
        let median = |won: bool| -> Option<f64> {
            let mut rs: Vec<f64> = players
                .iter()
                .filter(|p| p.team_id.is_some() && (p.team_id == Some(wt)) == won)
                .filter_map(|p| match (p.exp, p.raw_exp) {
                    (Some(e), Some(r)) if r > 0 && e > 0 => Some(e as f64 / r as f64),
                    _ => None,
                })
                .collect();
            if rs.len() < 3 {
                return None;
            }
            rs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            Some(rs[rs.len() / 2])
        };
        let w = median(true);
        let l = median(false);
        if w.is_some_and(|m| !(1.40..=1.60).contains(&m))
            || l.is_some_and(|m| !(0.95..=1.08).contains(&m))
        {
            crit!(
                "exp_raw_multiplier",
                format!(
                    "winner exp/raw median {w:?} (expect ≈1.5), loser median {l:?} (expect ≈1.0)"
                )
            );
        }
    }

    // ── Per-field domain ranges (game-rule bounds) — pervasive ⇒ critical ─────
    domain!(
        "team_id_domain",
        count(&|p| p.team_id.is_some_and(|t| t != 0 && t != 1)),
        n,
        "team_id outside {0,1}",
    );
    domain!(
        "ship_tier_domain",
        count(&|p| p.ship_tier.is_some_and(|t| !(1..=11).contains(&t))),
        count(&|p| p.ship_tier.is_some()),
        "ship_tier outside 1..=11",
    );
    domain!(
        "frags_domain",
        count(&|p| p.frags.is_some_and(|f| !(0..=12).contains(&f))),
        n,
        "frags outside 0..=12",
    );
    domain!(
        "raw_exp_range",
        count(&|p| p.raw_exp.is_some_and(|r| !(0..=8000).contains(&r))),
        count(&|p| p.raw_exp.is_some()),
        "raw_exp outside 0..=8000",
    );
    domain!(
        "nonnegative_values",
        count(&|p| {
            [
                p.damage_dealt,
                p.spotting_damage,
                p.damage_received,
                p.exp,
                p.raw_exp,
            ]
            .iter()
            .any(|v| v.is_some_and(|x| x < 0))
        }),
        n,
        "a negative damage/xp value",
    );
    domain!(
        "hits_le_shots",
        count(&|p| matches!((p.hits, p.shots_fired), (Some(h), Some(s)) if h > s)),
        count(&|p| p.hits.is_some() && p.shots_fired.is_some()),
        "hits > shots_fired",
    );

    // ── Soft signals ──────────────────────────────────────────────────────────
    if let Some(short) = game_version_short {
        let parts: Vec<u32> = short
            .splitn(2, '.')
            .filter_map(|p| p.parse().ok())
            .collect();
        if parts.len() == 2 && !KNOWN_GOOD.contains(&(parts[0], parts[1])) {
            warn!(
                "known_good_version",
                format!("game version {short} not in known-good {KNOWN_GOOD:?}; field mapping may be stale"),
            );
        }
    }
    let with_id = count(&|p| p.ship_id.is_some());
    if with_id > 0 {
        let resolved = count(&|p| p.ship_name.is_some());
        let rate = resolved as f64 / with_id as f64;
        if rate < 0.8 {
            warn!(
                "ship_resolution_rate",
                format!("only {:.0}% of ships resolved ({resolved}/{with_id}); ship_index.json may be stale", rate * 100.0),
            );
        }
    }
    if let Some(wt) = winner_team {
        let avg = |won: bool| -> Option<f64> {
            let v: Vec<i64> = players
                .iter()
                .filter(|p| p.team_id.is_some() && (p.team_id == Some(wt)) == won)
                .filter_map(|p| p.raw_exp)
                .collect();
            (!v.is_empty()).then(|| v.iter().sum::<i64>() as f64 / v.len() as f64)
        };
        if let (Some(w), Some(l)) = (avg(true), avg(false)) {
            if w < l * 0.8 {
                warn!(
                    "winner_xp_order",
                    format!("winner avg raw_exp {w:.0} < loser avg {l:.0}")
                );
            }
        }
    }
    // Cross-field reconciliation using the interaction matrix: in a closed PvP
    // battle, total damage dealt (summed over the attacker→victim matrix) should
    // be the same order as total damage received. A big divergence means the
    // interaction parse or the received-damage fields drifted apart.
    let dealt: i64 = players
        .iter()
        .flat_map(|p| &p.interactions)
        .map(|i| i.damage)
        .sum();
    let received: i64 = players.iter().filter_map(|p| p.damage_received).sum();
    if dealt > 0 && received > 0 {
        let ratio = dealt as f64 / received as f64;
        if !(0.5..=2.0).contains(&ratio) {
            warn!(
                "damage_reconciliation",
                format!("interaction damage dealt {dealt} vs received {received} (ratio {ratio:.2}) — cross-field mismatch"),
            );
        }
    }
    if n == 0 && winner_team.is_some() {
        warn!(
            "empty_roster",
            "finished battle resolved zero players".into()
        );
    }

    checks
}

/// Worst severity across the checks → the headline `decode_status`.
fn status_from_checks(checks: &[DecodeCheck]) -> DecodeStatus {
    if checks.iter().any(|c| c.severity == CheckSeverity::Critical) {
        DecodeStatus::Unreliable
    } else if checks.is_empty() {
        DecodeStatus::Ok
    } else {
        DecodeStatus::Degraded
    }
}

/// Build [`BattleData`] from a resolved `BattleResults` JSON value plus the
/// replay meta object. Shared by the in-process decoder ([`extract_battle_results`])
/// and the test-only JSONL helper ([`parse_jsonl_and_build`]).
fn build_battle_data(
    meta_obj: Option<serde_json::Value>,
    br: serde_json::Value,
    source_file_hash: String,
    replay_path: &Path,
    tables: &Tables,
    builds: &HashMap<i64, RawBuild>,
) -> Result<BattleData, DecodeError> {
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

    // Version/range/relationship validation happens in run_self_checks() after
    // the players are resolved (single source of truth for the trust status).

    // ── Players ───────────────────────────────────────────────────────────────

    let players_public = br
        .get("playersPublicInfo")
        .and_then(|v| v.as_object())
        .ok_or_else(|| DecodeError::Malformed("missing or non-object playersPublicInfo".into()))?;

    // privateDataList is for the owner only — a flat array.
    let private_list: Option<&Vec<serde_json::Value>> =
        br.get("privateDataList").and_then(|v| v.as_array());

    let mut players: Vec<BattlePlayer> = Vec::with_capacity(players_public.len());

    // Structural-validation counters — a global positional shift (new patch with
    // stale constants) breaks array length and the account-id anchor (index 0).
    let max_pub_idx = tables.public_indices.values().copied().max().unwrap_or(0);
    let acct_idx = tables.public_indices.get("account_db_id").copied();
    let mut n_arr_short = 0usize;
    let mut n_anchor_checked = 0usize;
    let mut n_anchor_match = 0usize;

    for (db_id_str, arr_val) in players_public {
        let arr = match arr_val.as_array() {
            Some(a) => a,
            None => continue, // skip non-array entries
        };

        let db_id: i64 = match db_id_str.parse() {
            Ok(id) => id,
            Err(_) => continue,
        };

        if arr.len() <= max_pub_idx {
            n_arr_short += 1;
        }
        if let Some(ai) = acct_idx {
            n_anchor_checked += 1;
            if arr.get(ai).and_then(to_i64_tolerant) == Some(db_id) {
                n_anchor_match += 1;
            }
        }

        let is_owner = db_id == owner_db_id;
        let private_for_owner = if is_owner { private_list } else { None };

        let mut player = resolve_player(
            arr,
            db_id,
            tables,
            owner_db_id,
            winner_team,
            private_for_owner.map(|v| v.as_slice()),
        );

        // Schema 1.7: attach the loadout from the battle-start packet pass.
        // Done here (not in resolve_player) because the skill selection needs
        // the resolved ship_class and the build map is keyed by db_id.
        if let Some(raw) = builds.get(&db_id) {
            player.build = Some(make_player_build(
                raw,
                player.ship_class,
                &tables.skill_costs,
            ));
        }

        players.push(player);
    }

    // ── Schema 1.2: xp_contribution = share of the team's experience ──────────
    // Base is raw_exp (pre win-multiplier) so winners aren't inflated relative to
    // losers. Each player's value = their raw_exp / Σ raw_exp of the same team.
    // Needs all players resolved first, hence this post-pass.
    let mut team_raw_exp: HashMap<i64, i64> = HashMap::new();
    for p in &players {
        if let (Some(tid), Some(r)) = (p.team_id, p.raw_exp) {
            *team_raw_exp.entry(tid).or_insert(0) += r.max(0);
        }
    }
    for p in &mut players {
        p.xp_contribution = match (p.team_id, p.raw_exp) {
            (Some(tid), Some(r)) => match team_raw_exp.get(&tid).copied() {
                Some(total) if total > 0 => Some(r.max(0) as f64 / total as f64),
                _ => None,
            },
            _ => None,
        };
    }

    // ── Expected-value self-check (extraction-failure / patch-shift detection) ─
    // The dangerous failure mode is a new game patch shifting the positional
    // layout while the bundled field-mapping stays stale: plausible-but-WRONG
    // numbers served as a normal 200. run_self_checks compares the decode against
    // every expectation (structural anchors, exp/raw multiplier, per-field domain
    // ranges, cross-field reconciliation) and grades deviations; decode_status is
    // the worst severity. warnings mirrors the failing checks' details (back-compat).
    let layout = LayoutStats {
        n_players: players.len(),
        n_arr_short,
        n_anchor_checked,
        n_anchor_match,
        max_pub_idx,
    };
    let decode_checks = run_self_checks(
        &players,
        winner_team,
        game_version_short.as_deref(),
        &layout,
    );
    let decode_status = status_from_checks(&decode_checks);
    let warnings: Vec<String> = decode_checks.iter().map(|c| c.detail.clone()).collect();

    let _ = replay_path; // used by caller for logging if needed

    Ok(BattleData {
        meta: BattleMeta {
            schema_version: "1.7".into(),
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
            decode_status,
            decode_checks,
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

    // `ribbons_torpedo_hits` / `ribbons_hits` historically read `RIBBON_TORPEDO` /
    // `RIBBON_MAIN_CALIBER`, which are score SENTINELS (only round values, and only
    // for the recording client) — NOT per-player hit counts (td-64aaf8, confirmed
    // across 36,889 reference player-rows). Repointed to the validated public
    // hit-count fields: `hits_tpd` (real torpedo hits) and the main-caliber hit sum
    // (= the `hits` value). Field names kept for back-compat; the data is now real.
    let ribbons_torpedo_hits = get_i64("hits_tpd");
    let ribbons_hits = hits;

    // Aircraft shot down — a public per-player count for ALL players. The
    // self-only `RIBBON_PLANE` ribbon is a `10000` sentinel (0 for everyone
    // else), so it was never a usable plane-kill count. A surface ship scores
    // these via AA (`planes_killed_by_ship`); a carrier also via its aircraft
    // (`planes_killed_by_plane`). The total is their sum. (td-4b4c1a)
    let planes_killed = match (
        get_i64("planes_killed_by_ship"),
        get_i64("planes_killed_by_plane"),
    ) {
        (None, None) => None,
        (by_ship, by_plane) => Some(by_ship.unwrap_or(0) + by_plane.unwrap_or(0)),
    };

    // Spotting (scouting) damage — public, all players.
    let spotting_damage = get_i64("scouting_damage");

    // Total damage received = sum of every `received_damage_*` public field.
    let damage_received = {
        let mut sum = 0i64;
        for (name, &idx) in pub_idx.iter() {
            if name.starts_with("received_damage_") {
                if let Some(v) = arr.get(idx) {
                    sum += to_i64_tolerant(v).unwrap_or(0);
                }
            }
        }
        Some(sum)
    };

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

    // credits: owner only, from privateDataList init_economics[credits].
    // PLAYER_PRIVATE_RESULTS has an "init_economics" sub-array; its first slot
    // (INIT_ECONOMICS_INDICES["credits"]) is the credits earned. Other players'
    // economics are not present in the replay.
    let credits: Option<i64> = if is_self {
        let econ_idx = tables
            .private_results
            .iter()
            .position(|name| name == "init_economics");
        let credits_idx = tables.init_economics_indices.get("credits").copied();
        match (econ_idx, credits_idx) {
            (Some(ei), Some(ci)) => private_for_owner
                .and_then(|pd| pd.get(ei))
                .and_then(|e| e.as_array())
                .and_then(|e| e.get(ci))
                .and_then(to_i64_tolerant),
            _ => None,
        }
    } else {
        None
    };

    // Per-victim damage matrix (schema 1.1). The public "interactions" slot holds
    // {victim_id → CLIENT_VEH_INTERACTION_DETAILS array}; resolve each victim.
    let interactions = build_interactions(get_val("interactions"), tables);

    // ── Schema 1.2 result-data expansion ────────────────────────────────────
    let sum_pub = |names: &[&str]| -> i64 { names.iter().map(|n| get_i64(n).unwrap_or(0)).sum() };

    // Damage DEALT by weapon-type bucket — SAME bucket field-lists as the
    // interaction matrix, so per-player totals reconcile with the matrix sum.
    let damage_dealt_by_type = DamageDealtByType {
        main: sum_pub(DMG_MAIN),
        secondary: sum_pub(DMG_SECONDARY),
        torpedo: sum_pub(DMG_TORPEDO),
        aircraft: sum_pub(DMG_AIRCRAFT),
        fire: sum_pub(DMG_FIRE),
        flood: sum_pub(DMG_FLOOD),
        ram: sum_pub(DMG_RAM),
        depth_charge: sum_pub(DMG_DEPTH_CHARGE),
        other: sum_pub(DMG_OTHER),
    };

    // Damage to structures (was null in 1.1; now wired from buildingInteractions).
    let damage_to_buildings = Some(sum_building_damage(get_val("buildingInteractions"), tables));

    let detection = Detection {
        first_ships_spotted: get_i64("first_ships_spotted_by_ship").unwrap_or(0)
            + get_i64("first_ships_spotted_by_plane").unwrap_or(0),
        first_planes_spotted: get_i64("first_planes_spotted_by_ship").unwrap_or(0)
            + get_i64("first_planes_spotted_by_plane").unwrap_or(0),
    };

    let modules = Modules {
        crits: get_i64("module_crits").unwrap_or(0),
        major_crits: get_i64("module_major_crits").unwrap_or(0),
        breaks: get_i64("module_breaks").unwrap_or(0),
        fires: get_i64("module_fires").unwrap_or(0),
        floods: get_i64("module_floods").unwrap_or(0),
    };

    // ── Schema 1.3: main-battery damage split (he/ap/sap; sap == WG "cs"/common) ─
    // Public per-player fields → present for ALL players. These are the SAME three
    // fields `damage_dealt_by_type.main` sums (DMG_MAIN), just kept un-summed, so
    // he + ap + sap always reconciles with `.main`.
    let damage_main_by_shell = DamageMainByShell {
        he: get_i64("damage_main_he").unwrap_or(0),
        ap: get_i64("damage_main_ap").unwrap_or(0),
        sap: get_i64("damage_main_cs").unwrap_or(0),
    };

    // ── Schema 1.3: owner-only economics (raw scalars, no precompute) ───────────
    // A replay carries the recording player's economics only. Read the expense
    // scalars + premium multipliers from the `common_economics` sub-array of
    // privateDataList (indexed by COMMON_ECONOMICS_INDICES). Absent slots → 0.
    let economics: Option<Economics> = if is_self {
        let common = tables
            .private_results
            .iter()
            .position(|name| name == "common_economics")
            .and_then(|i| private_for_owner.and_then(|pd| pd.get(i)))
            .and_then(|v| v.as_array());
        common.map(|ce| {
            let slot = |name: &str| -> Option<&serde_json::Value> {
                tables
                    .common_economics_indices
                    .get(name)
                    .and_then(|&i| ce.get(i))
            };
            let int = |name: &str| -> i64 { slot(name).and_then(to_i64_tolerant).unwrap_or(0) };
            let num = |name: &str| -> f64 { slot(name).and_then(|v| v.as_f64()).unwrap_or(0.0) };
            Economics {
                cost_service: int("auto_repair_credits"),
                cost_ammo: int("auto_load_credits"),
                cost_camo: int("auto_camo_credits"),
                cost_signals: int("auto_signals_credits"),
                cost_boost: int("auto_boost_credits"),
                free_exp_factor: num("free_exp_factor"),
                premium_credits_factor: num("premium_credits_factor"),
                premium_exp_factor: num("premium_exp_factor"),
                wows_premium_credits_factor: num("wows_premium_credits_factor"),
                wows_premium_exp_factor: num("wows_premium_exp_factor"),
            }
        })
    } else {
        None
    };

    // ── Schema 1.4: per-battle achievements (WoWS medals) ───────────────────
    // Public field: a list of [id, count] pairs, e.g. [[3911377840, 1]].
    // Present for ALL players; empty/absent → []. Resolve each id via the
    // achievement_index; an id the bundled index doesn't know (a newer-patch
    // medal) falls back to the stringified id rather than being dropped.
    let achievements: Vec<Achievement> = get_val("achievements")
        .and_then(|v| v.as_array())
        .map(|pairs| {
            pairs
                .iter()
                .filter_map(|pair| {
                    let pair = pair.as_array()?;
                    let id = to_i64_tolerant(pair.first()?)?;
                    let count = pair.get(1).and_then(to_i64_tolerant).unwrap_or(1).max(1);
                    let name = tables
                        .achievements
                        .get(&id.to_string())
                        .cloned()
                        .unwrap_or_else(|| id.to_string());
                    Some(Achievement { name, count })
                })
                .collect()
        })
        .unwrap_or_default();

    // ── Schema 1.5: main-battery hit-outcome sub-ribbons (all players) ──────────
    // Real per-player counts from the RIBBON_MAIN_CALIBER_* result fields. Correct
    // only with a current constants.json (a stale index map reads these as 0 — the
    // 0.7.1 mistake). Present for all players, zero-filled.
    let main_hits_quality = MainHitsQuality {
        penetration: get_i64("RIBBON_MAIN_CALIBER_PENETRATION").unwrap_or(0),
        over_penetration: get_i64("RIBBON_MAIN_CALIBER_OVER_PENETRATION").unwrap_or(0),
        no_penetration: get_i64("RIBBON_MAIN_CALIBER_NO_PENETRATION").unwrap_or(0),
        ricochet: get_i64("RIBBON_MAIN_CALIBER_RICOCHET").unwrap_or(0),
        citadel: get_i64("RIBBON_CITADEL").unwrap_or(0),
    };
    let secondary_hits = get_i64("RIBBON_SECONDARY_CALIBER").unwrap_or(0);
    let torpedo_protection_hits = get_i64("RIBBON_BULGE").unwrap_or(0);

    // ── Schema 1.5: owner-only per-battle Ship Efficiency grade ─────────────────
    // Private `mastery_sign`: 0=Expert, 1=Grade I, 2=Grade II, 3=Grade III (lower
    // int = higher grade); null / absent = none earned this battle.
    let ship_efficiency: Option<String> = if is_self {
        tables
            .private_results
            .iter()
            .position(|name| name == "mastery_sign")
            .and_then(|i| private_for_owner.and_then(|pd| pd.get(i)))
            .and_then(to_i64_tolerant)
            .and_then(|v| match v {
                0 => Some("expert"),
                1 => Some("grade_1"),
                2 => Some("grade_2"),
                3 => Some("grade_3"),
                _ => None,
            })
            .map(|s| s.to_string())
    } else {
        None
    };

    // ── Schema 1.5: owner-only active economic bonuses ──────────────────────────
    // The mounted boosters + permanent ship/commander bonuses appear as numeric
    // GameParams ids in the `mod` lists of the private `subtotal_economics` chains
    // (six {sse, base, mod} objects). Collect the distinct ids present in the
    // bundled bonus_index; named base sources (FIRST_WIN, CLAN_SUPPLY_BONUS…) and
    // unknown ids are skipped. Owner: `Some` (possibly empty); others: `None`.
    let economic_bonuses: Option<Vec<EconomicBonus>> = if is_self {
        let mut seen = std::collections::BTreeSet::new();
        let mut list: Vec<EconomicBonus> = Vec::new();
        let chains = tables
            .private_results
            .iter()
            .position(|name| name == "subtotal_economics")
            .and_then(|i| private_for_owner.and_then(|pd| pd.get(i)))
            .and_then(|v| v.as_array());
        if let Some(chains) = chains {
            for chain in chains {
                let Some(mods) = chain.get("mod").and_then(|v| v.as_array()) else {
                    continue;
                };
                for entry in mods {
                    let Some(id) = entry
                        .as_array()
                        .and_then(|e| e.first())
                        .and_then(to_i64_tolerant)
                    else {
                        continue;
                    };
                    if !seen.insert(id) {
                        continue;
                    }
                    if let Some(info) = tables.bonus_index.get(&id) {
                        list.push(EconomicBonus {
                            index: info.index.clone(),
                            name: info.name.clone(),
                            kind: info.kind.clone(),
                            rarity: info.rarity.clone(),
                            modifiers: info.modifiers.clone(),
                        });
                    }
                }
            }
        }
        Some(list)
    } else {
        None
    };

    // ── Schema 1.6: full ribbon set (all players) ───────────────────────────────
    // Every RIBBON_* field the game records, emitted raw under its WG constant
    // name. Only non-zero counts are included (a missing key means zero). All of
    // these are validated real per-player counts on a current constants.json (a
    // stale index map yields wrong/zero counts — the 0.7.1 mistake). BTreeMap for
    // deterministic (name-sorted) output.
    let ribbons: std::collections::BTreeMap<String, i64> = pub_idx
        .iter()
        .filter(|(name, _)| name.starts_with("RIBBON_"))
        .filter_map(|(name, &i)| {
            let v = arr.get(i).and_then(to_i64_tolerant)?;
            (v != 0).then(|| (name.clone(), v))
        })
        .collect();

    // ── Schema 1.7: objective ("victory") points split (all players) ────────────
    // WG's per-player objective-points breakdown — every `victory_points_*`
    // result field with a non-zero value, keyed by its WG constant name. Same
    // raw-names + non-zero-only contract as `ribbons`. f64 because the game
    // stores these as floats and `victory_points_own_ship_kill` is negative.
    let victory_points: std::collections::BTreeMap<String, f64> = pub_idx
        .iter()
        .filter(|(name, _)| name.starts_with("victory_points"))
        .filter_map(|(name, &i)| {
            let v = arr.get(i).and_then(to_f64_tolerant)?;
            (v != 0.0).then(|| (name.clone(), v))
        })
        .collect();

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
        damage_to_buildings, // schema 1.2: wired from buildingInteractions
        damage_potential,
        shots_fired,
        hits,
        frags,
        // xp_contribution needs team totals → filled by a post-pass in
        // build_battle_data once all players are resolved.
        xp_contribution: None,
        ribbons_torpedo_hits,
        planes_killed,
        ribbons_hits,
        spotting_damage,
        damage_received,
        credits,
        afk,
        survived,
        is_self,
        won,
        interactions,
        damage_dealt_by_type,
        detection,
        modules,
        damage_main_by_shell,
        economics,
        achievements,
        main_hits_quality,
        secondary_hits,
        torpedo_protection_hits,
        ship_efficiency,
        economic_bonuses,
        ribbons,
        victory_points,
        // build needs the cross-pass loadout map (keyed by db_id) → attached
        // by build_battle_data after resolution.
        build: None,
    }
}

// ── Tolerant type coercion helpers ─────────────────────────────────────────────

/// Coerce a JSON value to f64: accepts int or float; null / non-number → None.
fn to_f64_tolerant(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
}

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

    fn achievement_index_min_path() -> PathBuf {
        fixture_dir().join("achievement_index_min.json")
    }

    fn bonus_index_min_path() -> PathBuf {
        fixture_dir().join("bonus_index_min.json")
    }

    /// The full staged bonus_index.json in src-tauri/resources/ (for e2e tests
    /// that decode real replays).
    fn bonus_index_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("src-tauri")
            .join("resources")
            .join("bonus_index.json")
    }

    fn battle_results_fixture_path() -> PathBuf {
        fixture_dir().join("battle_results_min.json")
    }

    // ── Tables load ───────────────────────────────────────────────────────────

    #[test]
    fn tables_load_succeeds() {
        let tables = Tables::load(
            &constants_path(),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        )
        .expect("tables must load");
        assert!(tables.public_indices.contains_key("account_db_id"));
        assert!(tables.public_indices.contains_key("exp"));
        assert!(tables.public_indices.contains_key("damage"));
        assert!(!tables.common_results.is_empty());
        assert!(!tables.ships.is_empty());
    }

    #[test]
    fn tables_common_results_is_array() {
        let tables = Tables::load(
            &constants_path(),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        )
        .unwrap();
        // COMMON_RESULTS must be an ordered array starting with "arena_id"
        assert_eq!(tables.common_results[0], "arena_id");
        assert_eq!(tables.common_results[3], "winner_team_id");
        assert_eq!(tables.common_results[8], "duration_sec");
    }

    #[test]
    fn tables_client_veh_interaction_details_is_array() {
        let tables = Tables::load(
            &constants_path(),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        )
        .unwrap();
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
        Tables::load(
            &constants_path(),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        )
        .unwrap()
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
                                     // ribbons_torpedo_hits now reads hits_tpd (real torpedo hits), not the
                                     // RIBBON_TORPEDO sentinel. ribbons_hits = the main-caliber hits sum (= hits).
        set_i(&mut arr, tables.public_indices["hits_tpd"], 4); // → ribbons_torpedo_hits
        set_i(&mut arr, 280, 2); // planes_killed_by_ship
        set_i(&mut arr, 281, 1); // planes_killed_by_plane → planes_killed = 3

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
        // schema 1.2: wired from buildingInteractions; this synthetic arr has none → Some(0)
        assert_eq!(player.damage_to_buildings, Some(0));
        // damage_potential = 862200 + 115200 + 0 + 0 = 977400
        assert_eq!(player.damage_potential, Some(977400));
        // shots_fired = 54 + 0 + 6 = 60
        assert_eq!(player.shots_fired, Some(60));
        // hits = 30 + 0 + 1 = 31
        assert_eq!(player.hits, Some(31));
        assert_eq!(player.frags, Some(2));
        assert_eq!(player.xp_contribution, None);
        assert_eq!(player.ribbons_torpedo_hits, Some(4)); // = hits_tpd
                                                          // planes_killed = planes_killed_by_ship(2) + planes_killed_by_plane(1)
        assert_eq!(player.planes_killed, Some(3));
        assert_eq!(player.ribbons_hits, Some(31)); // = main-caliber hits sum (= hits)
        assert_eq!(player.survived, Some(false));
        assert!(player.is_self);
        // won: winner_team(1) == team_id(1) → true
        assert_eq!(player.won, Some(true));
    }

    /// Schema 1.2: the new result groups populate from the public fields. Sets
    /// fields by NAME (via the loaded index map) so the test isn't pinned to
    /// positions, and confirms the ribbons map drops zero entries.
    #[test]
    fn resolve_player_schema_1_2_groups() {
        let tables = make_tables_from_fixture();
        let max = *tables.public_indices.values().max().unwrap();
        let mut arr = make_arr(max + 1);
        let idx = |name: &str| tables.public_indices[name];

        // damage dealt by type — one field per bucket
        arr[idx("damage_main_he")] = serde_json::json!(1000); // main
        arr[idx("damage_atba_he")] = serde_json::json!(200); // secondary
        arr[idx("damage_tpd_normal")] = serde_json::json!(3000); // torpedo
        arr[idx("damage_bomb")] = serde_json::json!(500); // aircraft
        arr[idx("damage_fire")] = serde_json::json!(700); // fire
        arr[idx("damage_flood")] = serde_json::json!(400); // flood
        arr[idx("damage_ram")] = serde_json::json!(50); // ram
        arr[idx("damage_dbomb_direct")] = serde_json::json!(80); // depth_charge
        arr[idx("damage_sea_mine")] = serde_json::json!(9); // other
                                                            // detection (each = by_ship + by_plane)
        arr[idx("first_ships_spotted_by_ship")] = serde_json::json!(2);
        arr[idx("first_ships_spotted_by_plane")] = serde_json::json!(1);
        arr[idx("first_planes_spotted_by_ship")] = serde_json::json!(5);
        // modules
        arr[idx("module_crits")] = serde_json::json!(9);
        arr[idx("module_major_crits")] = serde_json::json!(2);
        arr[idx("module_breaks")] = serde_json::json!(1);
        arr[idx("module_fires")] = serde_json::json!(3);
        arr[idx("module_floods")] = serde_json::json!(4);

        let p = resolve_player(&arr, 42, &tables, 42, Some(1), None);

        let d = &p.damage_dealt_by_type;
        assert_eq!(
            (d.main, d.secondary, d.torpedo, d.aircraft, d.fire),
            (1000, 200, 3000, 500, 700)
        );
        assert_eq!((d.flood, d.ram, d.depth_charge, d.other), (400, 50, 80, 9));

        assert_eq!(p.detection.first_ships_spotted, 3); // 2 + 1
        assert_eq!(p.detection.first_planes_spotted, 5); // 5 + 0
        assert_eq!(
            (
                p.modules.crits,
                p.modules.major_crits,
                p.modules.breaks,
                p.modules.fires,
                p.modules.floods
            ),
            (9, 2, 1, 3, 4)
        );
    }

    /// Regression: WG republishes each aircraft weapon's damage on a
    /// delivery-mode variant (_avia/_alt/_airsupport) alongside the base
    /// field, and republishes airstrike depth-charge damage as BOTH
    /// damage_adbomb (DMG_OTHER) and damage_dbomb_airsupport (would-be
    /// DMG_DEPTH_CHARGE). Summing base + variant double-counts the same
    /// hits. The aircraft/depth_charge buckets must ignore the variants.
    #[test]
    fn resolve_player_aircraft_and_depth_charge_ignore_duplicate_variants() {
        let tables = make_tables_from_fixture();
        let max = *tables.public_indices.values().max().unwrap();
        let mut arr = make_arr(max + 1);
        let idx = |name: &str| tables.public_indices[name];

        // Aircraft: base field + its _avia breakdown publish the SAME hits.
        arr[idx("damage_bomb")] = serde_json::json!(5000);
        arr[idx("damage_bomb_avia")] = serde_json::json!(5000);
        // Depth charges: direct/splash are real (non-duplicated) hits...
        arr[idx("damage_dbomb_direct")] = serde_json::json!(1000);
        // ...but the airsupport variant duplicates damage_adbomb below.
        arr[idx("damage_dbomb_airsupport")] = serde_json::json!(800);
        arr[idx("damage_adbomb")] = serde_json::json!(800);

        let p = resolve_player(&arr, 42, &tables, 42, Some(1), None);

        let d = &p.damage_dealt_by_type;
        assert_eq!(d.aircraft, 5000, "base only — _avia duplicate excluded");
        assert_eq!(
            d.depth_charge, 1000,
            "direct+splash only — airsupport dup excluded"
        );
        assert_eq!(
            d.other, 800,
            "damage_adbomb counted once, here in DMG_OTHER"
        );
    }

    /// Same double-publish scenario, but against the per-victim interaction
    /// matrix (`build_interactions`, schema 1.1+) — it shares the DMG_*
    /// bucket lists with the aggregate above, so must reconcile the same way.
    #[test]
    fn resolve_player_interaction_matrix_ignores_duplicate_variants() {
        let tables = make_tables_from_fixture();
        let ii = tables.interaction_index.clone();
        let n = tables.interaction_details.len().max(1);
        let set = |arr: &mut Vec<serde_json::Value>, name: &str, v: serde_json::Value| {
            if let Some(&i) = ii.get(name) {
                arr[i] = v;
            }
        };

        let mut va = vec![serde_json::Value::Null; n];
        set(&mut va, "damage_bomb", serde_json::json!(5000));
        set(&mut va, "damage_bomb_avia", serde_json::json!(5000));
        set(&mut va, "damage_dbomb_direct", serde_json::json!(1000));
        set(&mut va, "damage_dbomb_airsupport", serde_json::json!(800));
        set(&mut va, "damage_adbomb", serde_json::json!(800));

        let mut arr = make_arr(540);
        let interactions_idx = tables.public_indices["interactions"];
        arr[interactions_idx] = serde_json::json!({ "222": va });

        let player = resolve_player(&arr, 111, &tables, 111, Some(1), None);

        assert_eq!(player.interactions.len(), 1);
        let a = &player.interactions[0];
        assert_eq!(
            a.damage_aircraft, 5000,
            "base only — _avia duplicate excluded"
        );
        assert_eq!(
            a.damage_depth_charge, 1000,
            "direct+splash only — airsupport dup excluded"
        );
        assert_eq!(a.damage_other, 800, "damage_adbomb counted once");
        // total = aircraft(5000) + depth_charge(1000) + other(800), not 11600
        assert_eq!(a.damage, 6800);
    }

    /// Schema 1.2: damage_to_buildings sums the `building_damage_*` fields across
    /// the player's `buildingInteractions`, and EXCLUDES `vehicle_damage_*`.
    #[test]
    fn resolve_player_damage_to_buildings() {
        let tables = make_tables_from_fixture();
        let max = *tables.public_indices.values().max().unwrap();
        let mut arr = make_arr(max + 1);

        let blen = tables.building_interaction_index.values().max().unwrap() + 1;
        let mut barr = vec![serde_json::Value::Null; blen];
        barr[tables.building_interaction_index["building_damage_main_he"]] =
            serde_json::json!(1200);
        barr[tables.building_interaction_index["building_damage_fire"]] = serde_json::json!(300);
        // vehicle_damage_* in the same detail array must NOT be counted.
        if let Some(&vi) = tables
            .building_interaction_index
            .get("vehicle_damage_main_he")
        {
            barr[vi] = serde_json::json!(99999);
        }
        arr[tables.public_indices["buildingInteractions"]] = serde_json::json!({ "777": barr });

        let p = resolve_player(&arr, 42, &tables, 42, Some(1), None);
        assert_eq!(p.damage_to_buildings, Some(1500)); // 1200 + 300; vehicle_* excluded
    }

    /// Schema 1.3: main-battery damage splits into he/ap/sap (sap == the "cs"
    /// common/semi-AP field) for ALL players, and the split always reconciles
    /// with the summed `damage_dealt_by_type.main` bucket.
    #[test]
    fn resolve_player_damage_main_by_shell_reconciles() {
        let tables = make_tables_from_fixture();
        let max = *tables.public_indices.values().max().unwrap();
        let mut arr = make_arr(max + 1);
        let idx = |name: &str| tables.public_indices[name];

        arr[idx("damage_main_he")] = serde_json::json!(1200);
        arr[idx("damage_main_ap")] = serde_json::json!(8000);
        arr[idx("damage_main_cs")] = serde_json::json!(450); // SAP

        let p = resolve_player(&arr, 42, &tables, 42, Some(1), None);
        let s = &p.damage_main_by_shell;
        assert_eq!((s.he, s.ap, s.sap), (1200, 8000, 450));
        // Decomposition invariant: he + ap + sap == damage_dealt_by_type.main.
        assert_eq!(s.he + s.ap + s.sap, p.damage_dealt_by_type.main);
    }

    /// Schema 1.3: owner-only economics reads the raw expense + multiplier scalars
    /// from the `common_economics` sub-array; a non-owner row gets `None` even if a
    /// private array is (implausibly) supplied.
    #[test]
    fn resolve_player_economics_owner_only() {
        let tables = make_tables_from_fixture();
        let max = *tables.public_indices.values().max().unwrap();
        let arr = make_arr(max + 1);

        // Build a privateDataList whose `common_economics` slot carries the scalars.
        let ce_idx = &tables.common_economics_indices;
        let celen = ce_idx.values().max().unwrap() + 1;
        let mut ce = vec![serde_json::Value::Null; celen];
        ce[ce_idx["auto_repair_credits"]] = serde_json::json!(35700);
        ce[ce_idx["auto_load_credits"]] = serde_json::json!(4032);
        ce[ce_idx["auto_camo_credits"]] = serde_json::json!(0);
        ce[ce_idx["auto_signals_credits"]] = serde_json::json!(0);
        ce[ce_idx["auto_boost_credits"]] = serde_json::json!(0);
        ce[ce_idx["free_exp_factor"]] = serde_json::json!(0.1);
        ce[ce_idx["premium_credits_factor"]] = serde_json::json!(1.5);
        ce[ce_idx["premium_exp_factor"]] = serde_json::json!(1.5);
        ce[ce_idx["wows_premium_credits_factor"]] = serde_json::json!(1.5);
        ce[ce_idx["wows_premium_exp_factor"]] = serde_json::json!(1.65);

        let ce_slot = tables
            .private_results
            .iter()
            .position(|n| n == "common_economics")
            .expect("common_economics in PLAYER_PRIVATE_RESULTS");
        let mut private = vec![serde_json::Value::Null; ce_slot + 1];
        private[ce_slot] = serde_json::Value::Array(ce);

        // Owner → economics present with the raw scalars.
        let owner = resolve_player(&arr, 42, &tables, 42, Some(1), Some(private.as_slice()));
        let e = owner.economics.expect("owner has economics");
        assert_eq!(e.cost_service, 35700);
        assert_eq!(e.cost_ammo, 4032);
        assert_eq!(e.premium_exp_factor, 1.5);
        assert_eq!(e.wows_premium_exp_factor, 1.65);

        // Non-owner (db_id != owner_db_id) → economics is None.
        let other = resolve_player(&arr, 99, &tables, 42, Some(1), Some(private.as_slice()));
        assert!(other.economics.is_none());
    }

    /// Schema 1.4: the public `achievements` field ([id, count] pairs) resolves
    /// known ids to their WG name via the achievement_index, falls back to the
    /// stringified id for an unknown id (a newer-patch medal), and defaults to
    /// an empty vec when the player earned none / the field is absent.
    #[test]
    fn resolve_player_achievements_resolves_known_id_and_defaults_empty() {
        let tables = make_tables_from_fixture();
        let max = *tables.public_indices.values().max().unwrap();
        let idx = |name: &str| tables.public_indices[name];

        // Known id (in achievement_index_min.json) + an unknown one, stacked once.
        let mut arr = make_arr(max + 1);
        arr[idx("achievements")] = serde_json::json!([[4281525168i64, 1], [999999999i64, 2]]);
        let p = resolve_player(&arr, 42, &tables, 42, Some(1), None);
        assert_eq!(
            p.achievements,
            vec![
                Achievement {
                    name: "PCH012_Arsonist".into(),
                    count: 1,
                },
                Achievement {
                    name: "999999999".into(), // unknown id -> stringified fallback
                    count: 2,
                },
            ]
        );

        // Field absent (null slot) -> empty, not an error.
        let empty_arr = make_arr(max + 1);
        let p2 = resolve_player(&empty_arr, 42, &tables, 42, Some(1), None);
        assert!(p2.achievements.is_empty());
    }

    /// Drift guard: the serialized `BattlePlayer` key set IS the wire contract
    /// (notes/bridge-result-api-contract.md). If this breaks, a field was
    /// added/renamed/removed — update the contract doc, bump `schema_version`,
    /// then update this expected set.
    #[test]
    fn battle_player_wire_keys_are_stable() {
        let p = BattlePlayer {
            account_db_id: 1,
            player_name: Some("x".into()),
            clan_id: Some(2),
            clan_tag: Some("y".into()),
            ship_id: Some(3),
            ship_name: Some("z".into()),
            ship_tier: Some(9),
            ship_class: Some(ShipClass::Destroyer),
            team_id: Some(0),
            prebattle_id: Some(0),
            exp: Some(1),
            raw_exp: Some(1),
            damage_dealt: Some(1),
            damage_to_buildings: Some(0),
            damage_potential: Some(1),
            shots_fired: Some(1),
            hits: Some(1),
            frags: Some(0),
            xp_contribution: Some(0.5),
            ribbons_torpedo_hits: Some(0),
            planes_killed: Some(0),
            ribbons_hits: Some(0),
            spotting_damage: Some(0),
            damage_received: Some(0),
            credits: Some(0),
            afk: Some(false),
            survived: Some(true),
            is_self: true,
            won: Some(true),
            interactions: vec![DamageInteraction {
                target_id: 9,
                damage: 1,
                ..Default::default()
            }],
            damage_dealt_by_type: Default::default(),
            detection: Default::default(),
            modules: Default::default(),
            damage_main_by_shell: Default::default(),
            // Some(_) so the optional owner-only key is locked by this guard.
            economics: Some(Default::default()),
            achievements: vec![Achievement {
                name: "PCH012_Arsonist".into(),
                count: 1,
            }],
            main_hits_quality: Default::default(),
            secondary_hits: 0,
            torpedo_protection_hits: 0,
            // Some(_) so the optional owner-only keys are locked by this guard.
            ship_efficiency: Some("expert".into()),
            economic_bonuses: Some(vec![]),
            ribbons: std::collections::BTreeMap::from([("RIBBON_BOMB".into(), 42)]),
            victory_points: std::collections::BTreeMap::from([(
                "victory_points_kill_battleship".into(),
                6500.0,
            )]),
            // Some(_) so the optional key is locked by this guard.
            build: Some(PlayerBuild {
                ship_id: 4_293_001_168,
                commander_id: Some(4_293_043_664),
                commander_skills: vec![12, 17],
                commander_points: Some(3),
                ..Default::default()
            }),
        };
        let v = serde_json::to_value(&p).unwrap();
        let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        let mut expected = vec![
            "account_db_id",
            "player_name",
            "clan_id",
            "clan_tag",
            "ship_id",
            "ship_name",
            "ship_tier",
            "ship_class",
            "team_id",
            "prebattle_id",
            "exp",
            "raw_exp",
            "damage_dealt",
            "damage_to_buildings",
            "damage_potential",
            "shots_fired",
            "hits",
            "frags",
            "xp_contribution",
            "ribbons_torpedo_hits",
            "planes_killed",
            "ribbons_hits",
            "spotting_damage",
            "damage_received",
            "credits",
            "afk",
            "survived",
            "is_self",
            "won",
            "interactions",
            "damage_dealt_by_type",
            "detection",
            "modules",
            "damage_main_by_shell",
            "economics",
            "achievements",
            "main_hits_quality",
            "secondary_hits",
            "torpedo_protection_hits",
            "ship_efficiency",
            "economic_bonuses",
            "ribbons",
            "victory_points",
            "build",
        ];
        expected.sort();
        assert_eq!(
            keys, expected,
            "BattlePlayer wire keys changed — update the contract doc + schema_version + this test"
        );
    }

    /// Schema 1.7: victory_points map mirrors the ribbons contract — non-zero
    /// `victory_points_*` fields only, raw WG names, signed values.
    #[test]
    fn resolve_player_victory_points_nonzero_only_and_signed() {
        let tables = make_tables_from_fixture();
        let vp_kill_bb = *tables
            .public_indices
            .get("victory_points_kill_battleship")
            .expect("constants must map victory_points_kill_battleship");
        let vp_death = *tables
            .public_indices
            .get("victory_points_own_ship_kill")
            .expect("constants must map victory_points_own_ship_kill");
        let vp_hold = *tables
            .public_indices
            .get("victory_points_cp_hold")
            .expect("constants must map victory_points_cp_hold");

        let mut arr = make_arr(540);
        arr[vp_kill_bb] = serde_json::json!(6500.0);
        arr[vp_death] = serde_json::json!(-2500.0);
        arr[vp_hold] = serde_json::json!(0.0); // zero → omitted

        let p = resolve_player(&arr, 42, &tables, 42, Some(1), None);
        assert_eq!(
            p.victory_points.get("victory_points_kill_battleship"),
            Some(&6500.0)
        );
        assert_eq!(
            p.victory_points.get("victory_points_own_ship_kill"),
            Some(&-2500.0),
            "negative (death penalty) values must survive"
        );
        assert!(
            !p.victory_points.contains_key("victory_points_cp_hold"),
            "zero values must be omitted"
        );
        assert_eq!(p.victory_points.len(), 2);
    }

    /// Schema 1.7: SKILLS_BY_SHIP_TYPE parses into per-species id→cost maps with
    /// costs in the commander grid's 1–4 range for every species.
    #[test]
    fn tables_skill_costs_parsed_from_constants() {
        let tables = make_tables_from_fixture();
        for species in [
            "AirCarrier",
            "Battleship",
            "Cruiser",
            "Destroyer",
            "Submarine",
            "Auxiliary",
        ] {
            let map = tables
                .skill_costs
                .get(species)
                .unwrap_or_else(|| panic!("skill_costs missing species {species}"));
            assert!(!map.is_empty(), "{species} skill map empty");
            assert!(
                map.values().all(|&c| (1..=4).contains(&c)),
                "{species} has a cost outside 1..=4"
            );
        }
    }

    /// Schema 1.7: make_player_build picks the skill set matching the ship's
    /// class and costs it; unknown ids or a missing class degrade to None.
    #[test]
    fn make_player_build_class_selection_and_points() {
        let mut skills_by_species = HashMap::new();
        skills_by_species.insert("Destroyer".to_string(), vec![10, 20]);
        skills_by_species.insert("Battleship".to_string(), vec![30]);
        let raw = RawBuild {
            ship_id: 111,
            modules: vec![1, 2],
            upgrades: vec![3],
            consumables: vec![4],
            exteriors: vec![5],
            ensigns: vec![],
            eco_boosts: vec![6],
            commander_id: 777,
            skills_by_species,
        };
        let mut costs: HashMap<String, HashMap<i64, i64>> = HashMap::new();
        costs.insert("Destroyer".into(), HashMap::from([(10, 1), (20, 4)]));

        // Right class → right skill set, points = 1 + 4.
        let b = make_player_build(&raw, Some(ShipClass::Destroyer), &costs);
        assert_eq!(b.ship_id, 111);
        assert_eq!(b.commander_id, Some(777));
        assert_eq!(b.commander_skills, vec![10, 20]);
        assert_eq!(b.commander_points, Some(5));

        // Class whose skills aren't costed in the grid → skills kept, points None.
        let b = make_player_build(&raw, Some(ShipClass::Battleship), &costs);
        assert_eq!(b.commander_skills, vec![30]);
        assert_eq!(b.commander_points, None);

        // Unknown class → empty skills, points None (never guessed).
        let b = make_player_build(&raw, None, &costs);
        assert!(b.commander_skills.is_empty());
        assert_eq!(b.commander_points, None);

        // Unknown skill id inside a costed class → points None, not partial.
        let mut partial = costs.clone();
        partial.get_mut("Destroyer").unwrap().remove(&20);
        let b = make_player_build(&raw, Some(ShipClass::Destroyer), &partial);
        assert_eq!(b.commander_skills, vec![10, 20]);
        assert_eq!(b.commander_points, None);

        // commander_id 0 → None on the wire.
        let raw_no_cdr = RawBuild {
            commander_id: 0,
            ..RawBuild {
                ship_id: 1,
                modules: vec![],
                upgrades: vec![],
                consumables: vec![],
                exteriors: vec![],
                ensigns: vec![],
                eco_boosts: vec![],
                commander_id: 0,
                skills_by_species: HashMap::new(),
            }
        };
        let b = make_player_build(&raw_no_cdr, Some(ShipClass::Destroyer), &costs);
        assert_eq!(b.commander_id, None);
    }

    /// Lock the planes_killed aggregation edge cases (td-4b4c1a): both source
    /// fields absent → None; a single field present → the other counts as 0.
    /// Real data always carries both, but a future constants reshuffle dropping
    /// one must not silently turn the sum into a wrong value or a panic.
    #[test]
    fn resolve_player_planes_killed_none_and_partial() {
        let tables = make_tables_from_fixture();

        // Both planes_killed_by_ship (280) and planes_killed_by_plane (281)
        // absent (JSON null) → planes_killed is None.
        let arr_absent = make_arr(540);
        let p = resolve_player(&arr_absent, 42, &tables, 42, Some(1), None);
        assert_eq!(
            p.planes_killed, None,
            "both plane-kill fields absent → planes_killed = None"
        );

        // Only planes_killed_by_ship present → the missing field counts as 0.
        let mut arr_partial = make_arr(540);
        set_i(&mut arr_partial, 280, 4); // planes_killed_by_ship
        let p = resolve_player(&arr_partial, 42, &tables, 42, Some(1), None);
        assert_eq!(
            p.planes_killed,
            Some(4),
            "one plane-kill field present → other treated as 0"
        );
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
        let tables = Tables::load(
            &constants_path(),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        )
        .unwrap();
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
    fn resolve_player_builds_interaction_matrix() {
        let tables = make_tables_from_fixture();
        let ii = tables.interaction_index.clone();
        let n = tables.interaction_details.len().max(1);
        let set = |arr: &mut Vec<serde_json::Value>, name: &str, v: serde_json::Value| {
            if let Some(&i) = ii.get(name) {
                arr[i] = v;
            }
        };

        // victim A (222): main + torpedo + fire damage, a citadel, the kill, spotted, scouting.
        let mut va = vec![serde_json::Value::Null; n];
        set(&mut va, "damage_main_he", serde_json::json!(10000));
        set(&mut va, "damage_tpd_normal", serde_json::json!(5000));
        set(&mut va, "damage_fire", serde_json::json!(2000));
        set(&mut va, "citadels", serde_json::json!(1));
        set(&mut va, "ship_killed", serde_json::json!(true));
        set(&mut va, "scouting_damage", serde_json::json!(3000));
        set(&mut va, "is_primary_spotted_by_ship", serde_json::json!(1));

        // victim B (333): all-zero → must be dropped.
        let vb = vec![serde_json::Value::Null; n];

        // victim C (444): moderate main-battery damage only.
        let mut vc = vec![serde_json::Value::Null; n];
        set(&mut vc, "damage_main_ap", serde_json::json!(8000));

        let mut arr = make_arr(540);
        let interactions_idx = tables.public_indices["interactions"];
        arr[interactions_idx] = serde_json::json!({ "222": va, "333": vb, "444": vc });

        let player = resolve_player(&arr, 111, &tables, 111, Some(1), None);

        // All-zero victim dropped; remaining sorted by damage desc.
        assert_eq!(
            player.interactions.len(),
            2,
            "all-zero victim must be dropped"
        );
        let a = &player.interactions[0];
        assert_eq!(a.target_id, 222);
        assert_eq!(a.damage_main, 10000);
        assert_eq!(a.damage_torpedo, 5000);
        assert_eq!(a.damage_fire, 2000);
        assert_eq!(a.damage_secondary, 0);
        assert_eq!(a.damage, 17000, "total = main+torp+fire");
        assert_eq!(a.spotting_damage, 3000);
        assert_eq!(a.citadels, 1);
        assert!(a.killed);
        assert!(a.spotted);

        let c = &player.interactions[1];
        assert_eq!(c.target_id, 444);
        assert_eq!(c.damage, 8000);
        assert!(!c.killed);
    }

    #[test]
    fn resolve_player_no_interactions_field_is_empty() {
        let tables = make_tables_from_fixture();
        let arr = make_arr(540); // index 405 (interactions) is null
        let player = resolve_player(&arr, 1, &tables, 999, None, None);
        assert!(player.interactions.is_empty());
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
        let tables = Tables::load(
            &constants_path(),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        )
        .unwrap();
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
                                                     // New fields: spotting/received are public; credits is owner-only.
                assert!(
                    player.spotting_damage.is_some(),
                    "owner spotting_damage present"
                );
                assert!(
                    player.damage_received.unwrap_or(0) > 0,
                    "owner took damage in this battle"
                );
                assert!(
                    player.credits.unwrap_or(0) > 0,
                    "owner credits resolved from privateDataList economics"
                );
            } else {
                // Economics are owner-only: never present for other players.
                assert_eq!(player.credits, None, "credits must be owner-only");
                // Spotting/received damage are public for every player.
                assert!(
                    player.spotting_damage.is_some(),
                    "spotting_damage is public for all players"
                );
                assert!(
                    player.damage_received.is_some(),
                    "damage_received is public for all players"
                );
            }
        }

        assert!(found_owner, "owner player must be found in fixture");
    }

    #[test]
    fn fixture_winner_team_and_duration() {
        let tables = Tables::load(
            &constants_path(),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        )
        .unwrap();
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

    /// Missing replay file → Io error (read fails before any parsing).
    #[test]
    fn decode_error_missing_replay_file() {
        let tables = Tables::load(
            &constants_path(),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        )
        .unwrap();
        let cfg = DecodeConfig {
            game_dir: PathBuf::from("/nonexistent/game"),
            constants_path: constants_path(),
            ship_index_path: ship_index_min_path(),
            achievement_index_path: achievement_index_min_path(),
            bonus_index_path: bonus_index_min_path(),
        };
        let result = decode_battle_result(Path::new("/nonexistent.wowsreplay"), &cfg, &tables);
        assert!(matches!(result, Err(DecodeError::Io(_))));
    }

    /// Missing constants.json → Resources error.
    #[test]
    fn decode_error_resources_missing_constants() {
        let result = Tables::load(
            Path::new("/nonexistent/constants.json"),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        );
        assert!(matches!(result, Err(DecodeError::Resources(_))));
    }

    /// JSONL with no BattleResults line → NoBattleResults.
    #[test]
    fn decode_no_battle_results_error() {
        let tables = Tables::load(
            &constants_path(),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        )
        .unwrap();
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
        let tables = Tables::load(
            &constants_path(),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        )
        .unwrap();
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
        let tables = Tables::load(
            &constants_path(),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        )
        .unwrap();
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
        assert_eq!(data.meta.schema_version, "1.7");
    }

    // ── Meta fields and warnings system ──────────────────────────────────────

    /// parse_jsonl_and_build correctly wires meta fields from the sidecar line-0
    /// (game_version_short, match_group, map_name with spaces/ stripped) and
    /// pushes a stale-version warning for versions not in KNOWN_GOOD.
    #[test]
    fn parse_jsonl_meta_fields_and_stale_version_warning() {
        let tables = Tables::load(
            &constants_path(),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        )
        .unwrap();
        let inner = serde_json::json!({
            "arenaUniqueID": 99001,
            "accountDBID": 1111,
            "commonList": [99001, 0, 1700000000i64, 0, 0, 0, "regular", 0, 600, 0, "domination", 0, 7, "", {}, 0, 0, 0],
            "playersPublicInfo": {},
            "privateDataList": []
        });
        let inner_str = serde_json::to_string(&inner).unwrap();
        // clientVersionFromExe "15,9,0,1" → short "15.9" — a future version NOT in
        // KNOWN_GOOD → should warn. (15.3/15.4/15.5 are all known-good now.)
        // mapName "spaces/23_Shards" → map_name should be "23_Shards" (strip prefix).
        // matchGroup "ranked" → match_group should be Some("ranked").
        let meta_line = r#"{"matchGroup":"ranked","clientVersionFromExe":"15,9,0,1","mapName":"spaces/23_Shards"}"#;
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
            Some("15.9".into()),
            "game_version_short must be parsed from clientVersionFromExe"
        );
        assert_eq!(
            data.meta.match_group,
            Some("ranked".into()),
            "match_group must be taken from meta matchGroup"
        );
        // 15.9 is not in KNOWN_GOOD → must have a stale-version warning.
        assert!(
            !data.meta.warnings.is_empty(),
            "stale version 15.9 must produce a warning"
        );
        assert!(
            data.meta.warnings.iter().any(|w| w.contains("15.9")),
            "warning must mention the version; got: {:?}",
            data.meta.warnings
        );
    }

    // ── decode_status (extraction-failure detection) ──────────────────────────

    /// A full-length player array (> max public index 537) with matching anchor.
    fn make_full_player(db_id: i64, team: i64, raw_exp: i64, exp: i64) -> Vec<serde_json::Value> {
        let mut a = vec![serde_json::Value::Null; 538];
        a[0] = serde_json::json!(db_id); // account_db_id (the anchor)
        a[6] = serde_json::json!(team); // team_id
        a[403] = serde_json::json!(raw_exp);
        a[404] = serde_json::json!(exp);
        a
    }

    fn build_status_battle(players: serde_json::Value, winner: i64) -> BattleData {
        let tables = Tables::load(
            &constants_path(),
            &ship_index_min_path(),
            &achievement_index_min_path(),
            &bonus_index_min_path(),
        )
        .unwrap();
        let inner = serde_json::json!({
            "arenaUniqueID": 7777, "accountDBID": 1,
            "commonList": [7777, 0, 0, winner, 0, 0, "regular", 0, 600, 0, "domination", 0, 7, "", {}, 0, 0, 0],
            "playersPublicInfo": players, "privateDataList": []
        });
        let inner_str = serde_json::to_string(&inner).unwrap();
        let jsonl = format!(
            "{{\"matchGroup\":\"pvp\",\"clientVersionFromExe\":\"15,4,0,1\"}}\n{{\"packet_type\":34,\"payload\":{{\"BattleResults\":{inner_str:?}}}}}"
        );
        parse_jsonl_and_build(jsonl, "h".into(), Path::new("t.wowsreplay"), &tables)
            .expect("should build")
    }

    #[test]
    fn decode_status_ok_for_valid_layout() {
        // Winners ratio 1.5, losers 1.0, anchors match, full-length arrays.
        let players = serde_json::json!({
            "10": make_full_player(10, 1, 800, 1200),
            "11": make_full_player(11, 1, 700, 1050),
            "20": make_full_player(20, 0, 600, 600),
            "21": make_full_player(21, 0, 500, 500),
        });
        let data = build_status_battle(players, 1);
        assert_eq!(
            data.meta.decode_status,
            DecodeStatus::Ok,
            "warnings: {:?}",
            data.meta.warnings
        );
    }

    #[test]
    fn decode_status_unreliable_on_broken_exp_ratio() {
        // Winners with exp==raw_exp (ratio 1.0, should be ~1.5) — the index-shift
        // signature. ≥3 winners so the median check fires.
        let players = serde_json::json!({
            "10": make_full_player(10, 1, 800, 800),
            "11": make_full_player(11, 1, 700, 700),
            "12": make_full_player(12, 1, 900, 900),
            "20": make_full_player(20, 0, 600, 600),
            "21": make_full_player(21, 0, 500, 500),
            "22": make_full_player(22, 0, 550, 550),
        });
        let data = build_status_battle(players, 1);
        assert_eq!(data.meta.decode_status, DecodeStatus::Unreliable);
        assert!(
            data.meta
                .decode_checks
                .iter()
                .any(|c| c.name == "exp_raw_multiplier" && c.severity == CheckSeverity::Critical),
            "expected a critical exp_raw_multiplier check; got {:?}",
            data.meta.decode_checks
        );
    }

    #[test]
    fn decode_status_degraded_on_field_outlier() {
        // One player with hits > shots_fired (impossible) — an isolated outlier
        // ⇒ a Warn check ⇒ overall Degraded (not Unreliable).
        let mut p_bad = make_full_player(10, 1, 800, 1200);
        p_bad[35] = serde_json::json!(1); // shots_main_ap → shots_fired = 1
        p_bad[66] = serde_json::json!(50); // hits_main_ap → hits = 50
        let players = serde_json::json!({
            "10": p_bad,
            "11": make_full_player(11, 1, 700, 1050),
            "20": make_full_player(20, 0, 600, 600),
            "21": make_full_player(21, 0, 500, 500),
        });
        let data = build_status_battle(players, 1);
        assert_eq!(data.meta.decode_status, DecodeStatus::Degraded);
        assert!(
            data.meta
                .decode_checks
                .iter()
                .any(|c| c.name == "hits_le_shots" && c.severity == CheckSeverity::Warn),
            "checks: {:?}",
            data.meta.decode_checks
        );
    }

    #[test]
    fn decode_status_unreliable_on_anchor_mismatch() {
        // arr[0] (account_db_id) != the playersPublicInfo key for every player.
        let mk = |team: i64, exp: i64| {
            let mut a = make_full_player(987654321, team, 800, exp); // wrong anchor id
            a[0] = serde_json::json!(987654321);
            a
        };
        let players = serde_json::json!({
            "10": mk(1, 1200), "11": mk(1, 1050),
            "20": mk(0, 800), "21": mk(0, 800),
        });
        let data = build_status_battle(players, 1);
        assert_eq!(data.meta.decode_status, DecodeStatus::Unreliable);
    }

    // ── 1.7 example-dump generator (for the engine agent) ─────────────────────

    /// Generate a real `BattleData` (schema 1.7) JSON dump from a live replay, to
    /// hand the engine agent a concrete example of the wire format. Gated on
    /// `TFD_DUMP_1_7=1`; decodes 15.5 replays (matching the installed client)
    /// newest-first and writes the first clean decode that has the owner economics
    /// group populated AND at least one player with a non-empty `achievements`
    /// (so the new fields aren't just `[]`/`{}` in the example), falling back to
    /// the first clean owner-economics decode otherwise.
    ///
    /// Run: `TFD_DUMP_1_7=1 cargo test -p bridge-core dump_1_7_example -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn dump_1_7_example() {
        if std::env::var("TFD_DUMP_1_7").as_deref() != Ok("1") {
            eprintln!("Skipping dump: TFD_DUMP_1_7!=1");
            return;
        }

        let game_dir = PathBuf::from(r"C:\Games\World_of_Warships");
        let archive_155 = PathBuf::from(r"T:\wows-replay-archive\15.5");
        let out_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("private-sync/notes/result-data-1.7-example.json");

        let si_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("src-tauri/resources/ship_index.json");
        let ai_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("src-tauri/resources/achievement_index.json");
        let tables = Tables::load(&constants_path(), &si_path, &ai_path, &bonus_index_path())
            .expect("tables must load");
        let cfg = DecodeConfig {
            game_dir,
            constants_path: constants_path(),
            ship_index_path: si_path,
            achievement_index_path: ai_path,
            bonus_index_path: bonus_index_path(),
        };

        // Collect 15.5 replays newest-first (by filename, which is timestamp-led).
        let mut replays: Vec<PathBuf> = walkdir(&archive_155)
            .into_iter()
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("wowsreplay"))
            .collect();
        replays.sort();
        replays.reverse();
        assert!(!replays.is_empty(), "no 15.5 replays in {archive_155:?}");

        // Prefer a richer example: an owner whose main-battery damage includes AP
        // or SAP (a battleship/cruiser) AND at least one player with a non-empty
        // achievements[] (so the 1.4 field shows real data, not just `[]`).
        // Fall back to the first clean owner-economics decode otherwise.
        let mut chosen: Option<(PathBuf, BattleData)> = None;
        let mut fallback: Option<(PathBuf, BattleData)> = None;
        for rp in replays.iter().take(80) {
            let data = match decode_battle_result(rp, &cfg, &tables) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if data.meta.decode_status != DecodeStatus::Ok {
                continue;
            }
            let Some(owner) = data.players.iter().find(|p| p.is_self) else {
                continue;
            };
            if owner.economics.is_none() {
                continue;
            }
            // Prefer the richest 1.5 example: owner with a Ship Efficiency grade
            // AND at least one active economic bonus AND some main-battery hits
            // (so main_hits_quality shows real sub-ribbon counts).
            let s = &owner.damage_main_by_shell;
            let has_grade = owner.ship_efficiency.is_some();
            let has_bonus = owner
                .economic_bonuses
                .as_ref()
                .is_some_and(|b| !b.is_empty());
            let has_hits = owner.hits.unwrap_or(0) > 0;
            // 1.7: the example must show a populated owner build (with skills +
            // points) and at least one player with a victory_points entry.
            let has_build = owner
                .build
                .as_ref()
                .is_some_and(|b| b.commander_points.is_some() && !b.upgrades.is_empty());
            let has_vp = data.players.iter().any(|p| !p.victory_points.is_empty());
            if has_grade
                && has_bonus
                && has_hits
                && has_build
                && has_vp
                && (s.ap > 0 || s.sap > 0 || s.he > 0)
            {
                chosen = Some((rp.clone(), data));
                break;
            }
            if fallback.is_none() {
                fallback = Some((rp.clone(), data));
            }
        }

        let (rp, data) = chosen
            .or(fallback)
            .expect("no clean 15.5 decode with owner economics found");
        let json = serde_json::to_string_pretty(&data).expect("serialize BattleData");
        std::fs::write(&out_path, &json).expect("write dump");
        eprintln!("DUMP wrote {} bytes to {}", json.len(), out_path.display());
        eprintln!("DUMP source replay: {}", rp.display());
        let owner = data.players.iter().find(|p| p.is_self);
        eprintln!(
            "DUMP schema={} players={} owner_econ={} ship_efficiency={:?} economic_bonuses={} owner_main_quality={:?}",
            data.meta.schema_version,
            data.players.len(),
            owner.map(|p| p.economics.is_some()).unwrap_or(false),
            owner.and_then(|p| p.ship_efficiency.clone()),
            owner
                .and_then(|p| p.economic_bonuses.as_ref().map(|b| b.len()))
                .unwrap_or(0),
            owner.map(|p| &p.main_hits_quality),
        );
        eprintln!(
            "DUMP 1.7: builds={}/{} with_skills={} owner_cdr_points={:?} vp_players={}",
            data.players.iter().filter(|p| p.build.is_some()).count(),
            data.players.len(),
            data.players
                .iter()
                .filter(|p| p
                    .build
                    .as_ref()
                    .is_some_and(|b| !b.commander_skills.is_empty()))
                .count(),
            owner.and_then(|p| p.build.as_ref().and_then(|b| b.commander_points)),
            data.players
                .iter()
                .filter(|p| !p.victory_points.is_empty())
                .count(),
        );
    }

    /// Schema 1.7 corpus sweep: decode the newest N archive replays and check
    /// the build extraction's invariants across real battles — coverage (every
    /// roster player gets a build), plausibility (commander points ≤ 21, ≤ 6
    /// upgrades, ≤ 14 modules) and victory-points population. Prints a summary;
    /// fails only on hard invariant violations. Gated like the dump generator.
    ///
    /// Run: `TFD_VALIDATE_1_7=1 cargo test -p bridge-core --release validate_1_7_sweep -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn validate_1_7_sweep() {
        if std::env::var("TFD_VALIDATE_1_7").as_deref() != Ok("1") {
            eprintln!("Skipping sweep: TFD_VALIDATE_1_7!=1");
            return;
        }

        let game_dir = PathBuf::from(r"C:\Games\World_of_Warships");
        let archive_155 = PathBuf::from(r"T:\wows-replay-archive\15.5");
        let si_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("src-tauri/resources/ship_index.json");
        let ai_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("src-tauri/resources/achievement_index.json");
        let tables = Tables::load(&constants_path(), &si_path, &ai_path, &bonus_index_path())
            .expect("tables must load");
        let cfg = DecodeConfig {
            game_dir,
            constants_path: constants_path(),
            ship_index_path: si_path,
            achievement_index_path: ai_path,
            bonus_index_path: bonus_index_path(),
        };

        let mut replays: Vec<PathBuf> = walkdir(&archive_155)
            .into_iter()
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("wowsreplay"))
            .collect();
        replays.sort();
        replays.reverse();

        let (mut battles, mut players, mut with_build, mut with_skills, mut with_points) =
            (0usize, 0usize, 0usize, 0usize, 0usize);
        let mut with_vp = 0usize;
        let mut points_hist: HashMap<i64, usize> = HashMap::new();
        let mut violations: Vec<String> = Vec::new();
        for rp in replays.iter().take(40) {
            let Ok(data) = decode_battle_result(rp, &cfg, &tables) else {
                continue;
            };
            if data.meta.decode_status != DecodeStatus::Ok {
                continue;
            }
            battles += 1;
            for p in &data.players {
                players += 1;
                if !p.victory_points.is_empty() {
                    with_vp += 1;
                }
                let Some(b) = &p.build else { continue };
                with_build += 1;
                if b.ship_id == 0 {
                    violations.push(format!("{rp:?}: build with ship_id 0"));
                }
                if Some(b.ship_id) != p.ship_id && p.ship_id.is_some() {
                    violations.push(format!(
                        "{rp:?}: build.ship_id {} != results ship_id {:?}",
                        b.ship_id, p.ship_id
                    ));
                }
                if b.upgrades.len() > 6 {
                    violations.push(format!("{rp:?}: {} upgrades", b.upgrades.len()));
                }
                if b.modules.len() > 14 {
                    violations.push(format!("{rp:?}: {} modules", b.modules.len()));
                }
                if !b.commander_skills.is_empty() {
                    with_skills += 1;
                }
                if let Some(pts) = b.commander_points {
                    with_points += 1;
                    *points_hist.entry(pts).or_insert(0) += 1;
                    if !(1..=21).contains(&pts) {
                        violations.push(format!("{rp:?}: commander_points {pts}"));
                    }
                }
            }
        }

        let mut hist: Vec<(i64, usize)> = points_hist.into_iter().collect();
        hist.sort();
        eprintln!(
            "SWEEP battles={battles} players={players} with_build={with_build} \
             with_skills={with_skills} with_points={with_points} with_vp={with_vp}"
        );
        eprintln!("SWEEP commander_points histogram: {hist:?}");
        eprintln!("SWEEP violations: {}", violations.len());
        for v in violations.iter().take(10) {
            eprintln!("  {v}");
        }
        assert!(battles >= 10, "too few clean decodes to validate");
        assert!(
            violations.is_empty(),
            "build invariant violations: {violations:?}"
        );
        // Coverage floor: the arena state carries a config for every player, so
        // builds should be near-universal (bots in non-pvp modes may lack one).
        assert!(
            with_build * 100 >= players * 95,
            "build coverage below 95%: {with_build}/{players}"
        );
    }

    /// Minimal recursive file walk (no external crate) for the dump generator.
    fn walkdir(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push(p);
                }
            }
        }
        out
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

        let game_dir = PathBuf::from(r"C:\Games\World_of_Warships");
        let extracted_dir = PathBuf::from(
            r"C:\Users\fhelm\Documents\tfd-bridge\private-sync\notes\xp-analysis\extracted",
        );
        // Canonical archive lives on the T: drive, partitioned by patch:
        // T:\wows-replay-archive\<patch>\<donor>\<file>.
        let archive_dir = PathBuf::from(r"T:\wows-replay-archive");
        let wows_replays = PathBuf::from(r"C:\Games\World_of_Warships\replays");

        // Pre-flight checks.
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
        let ai_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("src-tauri/resources/achievement_index.json");
        let tables = Tables::load(&constants_path(), &si_path, &ai_path, &bonus_index_path())
            .expect("tables must load for e2e");

        let cfg = DecodeConfig {
            game_dir,
            constants_path: constants_path(),
            ship_index_path: si_path,
            achievement_index_path: ai_path,
            bonus_index_path: bonus_index_path(),
        };

        // Helper: locate source replay for a reference JSON entry.
        let locate_replay = |ref_json: &serde_json::Value| -> Option<PathBuf> {
            let file_name = ref_json["file"].as_str().unwrap_or("");
            let donor = ref_json["donor"].as_str().unwrap_or("");
            if file_name.is_empty() || donor.is_empty() {
                return None;
            }
            // Canonical layout: <archive>\<patch>\<donor>\<file>.
            let short = parse_version_short(
                ref_json["meta"]["clientVersionFromExe"]
                    .as_str()
                    .unwrap_or(""),
            )
            .unwrap_or_default();
            let candidate = archive_dir.join(&short).join(donor).join(file_name);
            if candidate.exists() {
                return Some(candidate);
            }
            // Legacy flat layout fallback: <archive>\<donor>\<file>.
            let flat = archive_dir.join(donor).join(file_name);
            if flat.exists() {
                return Some(flat);
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

        // Collect up to 5 from each version bucket (15.3, 15.4 and 15.5).
        let mut v15_3: Vec<(PathBuf, serde_json::Value)> = Vec::new();
        let mut v15_4: Vec<(PathBuf, serde_json::Value)> = Vec::new();
        let mut v15_5: Vec<(PathBuf, serde_json::Value)> = Vec::new();
        for (_, path) in &all_entries {
            if v15_3.len() >= 5 && v15_4.len() >= 5 && v15_5.len() >= 5 {
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
            } else if short == "15.5" && v15_5.len() < 5 {
                v15_5.push((replay_path, ref_json));
            }
        }

        let n_15_3 = v15_3.len();
        let n_15_4 = v15_4.len();
        let n_15_5 = v15_5.len();
        let candidates: Vec<(PathBuf, serde_json::Value)> =
            v15_3.into_iter().chain(v15_4).chain(v15_5).collect();

        eprintln!(
            "E2E: collected {} candidates (15.3: {}, 15.4: {}, 15.5: {})",
            candidates.len(),
            n_15_3,
            n_15_4,
            n_15_5
        );

        // 15.5 must be represented — it is the newest known-good version and the
        // build currently installed, so it is the one this run actually decodes
        // (older builds whose entity specs are uninstalled are skipped below).
        assert!(
            n_15_5 >= 1,
            "E2E requires 15.5 replays; got 15.3={n_15_3}, 15.4={n_15_4}, 15.5={n_15_5}"
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

        let mut skipped = 0usize;
        for (replay_path, ref_json) in &candidates {
            let fname = replay_path.file_name().unwrap().to_string_lossy();
            let ver = parse_version_short(
                ref_json["meta"]["clientVersionFromExe"]
                    .as_str()
                    .unwrap_or(""),
            )
            .unwrap_or_default();

            eprintln!("  decoding {} ({})", fname, ver);

            let battle_data = match decode_battle_result(replay_path, &cfg, &tables) {
                Ok(d) => d,
                // The installed game ships only the CURRENT build's entity specs,
                // so replays from an uninstalled older build cannot be parsed.
                // Skip them rather than failing — expected once the game updates.
                Err(DecodeError::Resources(msg)) => {
                    eprintln!("    SKIP {fname} (specs unavailable: {msg})");
                    skipped += 1;
                    continue;
                }
                Err(e) => panic!("decode failed for {fname}: {e}"),
            };

            // Real-replay smoke check for the new fields (schema 1.1): a finished
            // pvp battle on a known-good build must decode as trustworthy and
            // expose a non-empty attacker→victim matrix for active players.
            assert_eq!(
                battle_data.meta.decode_status,
                DecodeStatus::Ok,
                "{fname}: expected decode_status=ok for a real {ver} battle"
            );
            let total_interactions: usize = battle_data
                .players
                .iter()
                .map(|p| p.interactions.len())
                .sum();
            assert!(
                total_interactions > 0,
                "{fname}: expected a non-empty interaction matrix"
            );

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

                // ribbons_hits = real main-caliber hits (hits_main_ap+cs+he), NOT the
                // RIBBON_MAIN_CALIBER sentinel (td-64aaf8). Equals the `hits` field.
                {
                    total_field_checks += 1;
                    let h = |k: &str| ref_player[k].as_f64().map(|v| v as i64).unwrap_or(0);
                    let expected = Some(h("hits_main_ap") + h("hits_main_cs") + h("hits_main_he"));
                    if got.ribbons_hits != expected {
                        failures.push(format!(
                            "{}: player {db_id_str} ribbons_hits (main-caliber hits): got {:?} expected {:?}",
                            fname, got.ribbons_hits, expected
                        ));
                    }
                }

                // ribbons_torpedo_hits = real torpedo hits (hits_tpd), NOT the
                // RIBBON_TORPEDO sentinel (td-64aaf8).
                {
                    total_field_checks += 1;
                    let ref_val = ref_player["hits_tpd"].as_f64().map(|v| v as i64);
                    if let Some(rv) = ref_val {
                        let expected = Some(rv);
                        if got.ribbons_torpedo_hits != expected {
                            failures.push(format!(
                                "{}: player {db_id_str} ribbons_torpedo_hits (hits_tpd): got {:?} expected {:?}",
                                fname, got.ribbons_torpedo_hits, expected
                            ));
                        }
                    }
                }

                // planes_killed = planes_killed_by_ship + planes_killed_by_plane (td-4b4c1a)
                {
                    total_field_checks += 1;
                    let by_ship =
                        ref_player["planes_killed_by_ship"].as_f64().unwrap_or(0.0) as i64;
                    let by_plane =
                        ref_player["planes_killed_by_plane"].as_f64().unwrap_or(0.0) as i64;
                    let expected = Some(by_ship + by_plane);
                    if got.planes_killed != expected {
                        failures.push(format!(
                            "{}: player {db_id_str} planes_killed (by_ship+by_plane): got {:?} expected {:?}",
                            fname, got.planes_killed, expected
                        ));
                    }
                }

                // spotting_damage = scouting_damage
                {
                    total_field_checks += 1;
                    let ref_val = ref_player["scouting_damage"]
                        .as_i64()
                        .or_else(|| ref_player["scouting_damage"].as_f64().map(|f| f as i64));
                    if got.spotting_damage != ref_val {
                        failures.push(format!(
                            "{}: player {db_id_str} spotting_damage: got {:?} expected {:?}",
                            fname, got.spotting_damage, ref_val
                        ));
                    }
                }

                // damage_received = sum of all received_damage_* fields
                {
                    total_field_checks += 1;
                    let mut ref_val = 0i64;
                    if let Some(obj) = ref_player.as_object() {
                        for (k, v) in obj {
                            if k.starts_with("received_damage_") {
                                ref_val += v.as_f64().unwrap_or(0.0) as i64;
                            }
                        }
                    }
                    if got.damage_received != Some(ref_val) {
                        failures.push(format!(
                            "{}: player {db_id_str} damage_received: got {:?} expected {:?}",
                            fname,
                            got.damage_received,
                            Some(ref_val)
                        ));
                    }
                }
            }

            // Owner-only economics: credits from privateDataList init_economics[credits].
            {
                let owner_db_id = ref_json["accountDBID"].as_i64().unwrap_or(0);
                let econ_idx = tables
                    .private_results
                    .iter()
                    .position(|n| n == "init_economics");
                let credits_idx = tables.init_economics_indices.get("credits").copied();
                if let (Some(ei), Some(ci)) = (econ_idx, credits_idx) {
                    let ref_credits = ref_json["privateDataList"]
                        .as_array()
                        .and_then(|pd| pd.get(ei))
                        .and_then(|e| e.as_array())
                        .and_then(|e| e.get(ci))
                        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)));
                    if let Some(rc) = ref_credits {
                        total_field_checks += 1;
                        let got_credits = battle_data
                            .players
                            .iter()
                            .find(|p| p.account_db_id == owner_db_id)
                            .and_then(|p| p.credits);
                        if got_credits != Some(rc) {
                            failures.push(format!(
                                "{}: owner credits: got {:?} expected {:?}",
                                fname,
                                got_credits,
                                Some(rc)
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
        let decoded = candidates.len() - skipped;
        eprintln!(
            "\nE2E summary: {} replays ({} decoded, {} skipped — build not installed), {} players, {}/{} field checks passed (100% = {})",
            candidates.len(),
            decoded,
            skipped,
            total_players_checked,
            matched,
            total_field_checks,
            failures.is_empty()
        );
        assert!(
            decoded > 0,
            "no candidate replay was decodable on this install (all {} skipped); install a known-good build to run the oracle",
            candidates.len()
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
