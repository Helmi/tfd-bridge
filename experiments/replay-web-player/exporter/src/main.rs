use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::fs::File;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use clap::Parser as ClapParser;
use serde::Serialize;
use sha2::{Digest, Sha256};
use wows_battle_world::scan::{WorldScanCollector, scan_replay_world};
use wows_battle_world::view::BattleView;
use wows_minimap_renderer::MINIMAP_SIZE;
use wows_minimap_renderer::assets::{
    load_map_image, load_map_info, load_plane_icons, load_powerup_icons,
};
use wows_minimap_renderer::map_data::MapInfo;
use wows_replays::ReplayFile;
use wows_replays::game_constants::GameConstants;
use wows_replays::nested_property_path::{PropertyNestLevel, UpdateAction};
use wows_replays::packet2::{Packet, PacketType};
use wows_replays::types::{EntityId, GameClock, WorldPos};
use wowsunpack::data::{ResourceLoader, Version};
use wowsunpack::game_data;
use wowsunpack::game_params::provider::GameMetadataProvider;
use wowsunpack::game_params::types::{AmmoType, GameParamProvider, PlaneCategory, Species};
use wowsunpack::game_types::{CollisionType, ShellHitType};
use wowsunpack::rpc::typedefs::ArgValue;

#[derive(ClapParser, Debug)]
#[command(about = "Export a WoWS replay as an experimental semantic TFD battle scene")]
struct Args {
    /// World of Warships installation directory.
    #[arg(long)]
    game: PathBuf,

    /// Finalized .wowsreplay file.
    #[arg(long)]
    replay: PathBuf,

    /// Output directory for scene.json and map.png.
    #[arg(long)]
    output: PathBuf,

    /// Inline the map image as a `data:image/png;base64,...` URL in
    /// `map.imageUrl` instead of writing `map.png` to the output directory.
    #[arg(long)]
    inline_assets: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReplayScene {
    schema: &'static str,
    version: u32,
    replay: ReplayInfo,
    map: MapDescriptor,
    assets: SceneAssets,
    teams: Vec<TeamDescriptor>,
    entities: Vec<EntityDescriptor>,
    aviation: BTreeMap<String, PlaneDescriptor>,
    wards: Vec<WardRecord>,
    tracks: SceneTracks,
    events: SceneEvents,
    coverage: Coverage,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SceneAssets {
    powerup_icons: BTreeMap<String, String>,
    plane_icons: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReplayInfo {
    id: String,
    name: String,
    source_sha256: String,
    game_build: String,
    duration_ms: i64,
    battle_start_ms: i64,
    perspective: Perspective,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Perspective {
    player_name: String,
    team_id: String,
    entity_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MapDescriptor {
    name: String,
    image_url: Option<String>,
    coordinate_space: &'static str,
    space_size: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TeamDescriptor {
    id: String,
    name: String,
    color: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct EntityDescriptor {
    id: String,
    team_id: String,
    relation: String,
    player_name: String,
    clan: Option<String>,
    ship_name: String,
    ship_code: String,
    species: String,
    max_hp: f32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SceneTracks {
    ships: BTreeMap<String, Vec<ShipSample>>,
    scores: Vec<ScoreSample>,
    caps: Vec<CapSample>,
    buffs: Vec<BuffSample>,
    smoke: Vec<SmokeSample>,
    planes: Vec<PlaneSample>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ShipSample {
    t: i64,
    x: f32,
    y: f32,
    heading_deg: f32,
    course_deg: f32,
    hp: f32,
    max_hp: f32,
    alive: bool,
    visible: bool,
    last_known: bool,
    detected_by_enemy: bool,
    submerged: bool,
    hp_stale: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ScoreSample {
    t: i64,
    teams: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct CapSample {
    t: i64,
    id: String,
    x: Option<f32>,
    y: Option<f32>,
    radius: f32,
    owner_team_id: String,
    invader_team_id: String,
    progress: f64,
    time_remaining: f64,
    has_invaders: bool,
    contested: bool,
    enabled: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct BuffSample {
    t: i64,
    id: String,
    x: f32,
    y: f32,
    radius: f32,
    active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    activation_at: Option<i64>,
    team_id: Option<String>,
    marker_name: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SceneEvents {
    salvos: Vec<SalvoEvent>,
    torpedoes: Vec<TorpedoTrack>,
    kills: Vec<KillEvent>,
    hits: Vec<HitEvent>,
    consumables: Vec<ConsumableEvent>,
    chat: Vec<ChatEvent>,
    pickups: Vec<PickupEvent>,
}

/// One main-battery shell striking a ship: who fired, who was hit, the shell
/// type, and the penetration quality. Used to attribute HP losses.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HitEvent {
    t: i64,
    attacker_id: String,
    victim_id: String,
    /// AP | HE | SAP when the hit matched a salvo whose projectile resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    ammo_type: Option<String>,
    /// penetration | citadel | overpen | shatter | ricochet | underwater.
    quality: String,
}

/// A smoke screen's puff cloud at one point in time. Puffs accumulate while
/// the screen is being laid and the whole entity disappears on dissipation.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SmokeSample {
    t: i64,
    id: String,
    active: bool,
    radius: f32,
    points: Vec<Point>,
}

/// Sparse position samples for one squadron generation. Static facts about
/// the squadron live in the scene-level `aviation` descriptor map.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct PlaneSample {
    t: i64,
    id: String,
    x: f32,
    y: f32,
    active: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaneDescriptor {
    id: String,
    owner_id: String,
    team_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    /// Game plane-icon subdirectory (controllable | consumables | airsupport).
    #[serde(skip_serializing_if = "Option::is_none")]
    icon_dir: Option<String>,
    /// Game plane-icon base name (fighter | bomber_he | torpedo_regular | …),
    /// resolved the same way the minimap renderer resolves squadron icons.
    #[serde(skip_serializing_if = "Option::is_none")]
    icon_base: Option<String>,
}

/// A stationary fighter-patrol ward: fixed circle with an add/remove lifetime.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WardRecord {
    id: String,
    owner_id: String,
    x: f32,
    y: f32,
    radius: f32,
    added_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    removed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConsumableEvent {
    t: i64,
    ship_id: String,
    name: String,
    duration_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatEvent {
    t: i64,
    sender_id: String,
    sender_name: String,
    channel: String,
    message: String,
}

/// One Arms Race pickup resolved to the collecting ship and, when the zone
/// geometry allows it, the exact zone that was collected.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PickupEvent {
    t: i64,
    owner_id: String,
    team_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    zone_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    marker_name: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SalvoEvent {
    id: String,
    t: i64,
    owner_id: String,
    /// AP | HE | SAP | raw game string, when the projectile param resolves.
    #[serde(skip_serializing_if = "Option::is_none")]
    ammo_type: Option<String>,
    projectiles: Vec<ShellEvent>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ShellEvent {
    id: String,
    origin: Point,
    target: Point,
    flight_ms: i64,
    speed: f32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TorpedoTrack {
    id: String,
    owner_id: String,
    launched_at: i64,
    ended_at: Option<i64>,
    samples: Vec<TorpedoSample>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct TorpedoSample {
    t: i64,
    x: f32,
    y: f32,
    heading_deg: f32,
    speed: f32,
    armed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KillEvent {
    t: i64,
    killer_id: String,
    victim_id: String,
    cause: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
struct Point {
    x: f32,
    y: f32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Coverage {
    mode: &'static str,
    honest_visibility: bool,
    warning: &'static str,
}

struct SceneCollector<'a> {
    metadata: &'a GameMetadataProvider,
    map_info: MapInfo,
    entities: BTreeMap<String, EntityDescriptor>,
    ships: BTreeMap<String, Vec<ShipSample>>,
    scores: Vec<ScoreSample>,
    caps: Vec<CapSample>,
    last_caps: HashMap<usize, CapSample>,
    buffs: Vec<BuffSample>,
    last_buffs: HashMap<String, BuffSample>,
    buff_activation_at: HashMap<String, i64>,
    salvos: Vec<SalvoEvent>,
    seen_salvos: HashSet<(u32, u32)>,
    torpedoes: Vec<TorpedoTrack>,
    torpedo_indexes: HashMap<(u32, u32), usize>,
    active_torpedoes: HashSet<(u32, u32)>,
    kills: Vec<KillEvent>,
    seen_kills: usize,
    hits: Vec<HitEvent>,
    seen_hits: usize,
    smoke: Vec<SmokeSample>,
    last_smoke: HashMap<String, SmokeSample>,
    planes: Vec<PlaneSample>,
    last_planes: HashMap<String, PlaneSample>,
    plane_descriptors: BTreeMap<String, PlaneDescriptor>,
    plane_generations: HashMap<u64, u32>,
    active_plane_scene_ids: HashMap<u64, String>,
    wards: Vec<WardRecord>,
    ward_indexes: HashMap<u64, usize>,
    consumables: Vec<ConsumableEvent>,
    seen_consumables: HashSet<(u32, String, i64)>,
    chat: Vec<ChatEvent>,
    seen_chat: usize,
    pickups: Vec<PickupEvent>,
    pending_pickups: Vec<(i64, Vec<u32>)>,
    buff_zone_params: HashMap<String, i64>,
    battle_start_ms: Option<i64>,
    battle_end_ms: Option<i64>,
    last_clock_ms: i64,
}

impl<'a> SceneCollector<'a> {
    fn new(map_info: MapInfo, metadata: &'a GameMetadataProvider) -> Self {
        Self {
            metadata,
            map_info,
            entities: BTreeMap::new(),
            ships: BTreeMap::new(),
            scores: Vec::new(),
            caps: Vec::new(),
            last_caps: HashMap::new(),
            buffs: Vec::new(),
            last_buffs: HashMap::new(),
            buff_activation_at: HashMap::new(),
            salvos: Vec::new(),
            seen_salvos: HashSet::new(),
            torpedoes: Vec::new(),
            torpedo_indexes: HashMap::new(),
            active_torpedoes: HashSet::new(),
            kills: Vec::new(),
            seen_kills: 0,
            hits: Vec::new(),
            seen_hits: 0,
            smoke: Vec::new(),
            last_smoke: HashMap::new(),
            planes: Vec::new(),
            last_planes: HashMap::new(),
            plane_descriptors: BTreeMap::new(),
            plane_generations: HashMap::new(),
            active_plane_scene_ids: HashMap::new(),
            wards: Vec::new(),
            ward_indexes: HashMap::new(),
            consumables: Vec::new(),
            seen_consumables: HashSet::new(),
            chat: Vec::new(),
            seen_chat: 0,
            pickups: Vec::new(),
            pending_pickups: Vec::new(),
            buff_zone_params: HashMap::new(),
            battle_start_ms: None,
            battle_end_ms: None,
            last_clock_ms: 0,
        }
    }

    fn observe_view(&mut self, packet: &Packet<'_, '_>, view: &BattleView<'_>) {
        let clock = packet.clock;
        let now = ms(clock);
        self.last_clock_ms = self.last_clock_ms.max(now);
        self.battle_start_ms = view.battle_start_clock().map(ms).or(self.battle_start_ms);
        self.battle_end_ms = view.battle_end_clock().map(ms).or(self.battle_end_ms);

        self.collect_entities(view);
        self.collect_ships(clock, view);
        self.collect_scores(now, view);
        self.collect_caps(now, view);
        self.collect_buffs(now, view);
        self.collect_salvos(view);
        self.collect_torpedoes(now, view);
        self.collect_kills(view);
        self.collect_hits(view);
        self.collect_smoke(now, view);
        self.collect_planes(now, view);
        self.collect_wards(now, view);
        self.collect_consumables(view);
        self.collect_chat(view);
        self.resolve_pickups(now);
    }

    fn collect_entities(&mut self, view: &BattleView<'_>) {
        for (entity_id, player) in view.player_entities() {
            let id = entity_id.to_string();
            if self.entities.contains_key(&id) {
                continue;
            }
            let state = player.initial_state();
            let relation = player.relation().name().to_ascii_lowercase();
            let species = player
                .vehicle()
                .species()
                .and_then(|recognized| {
                    recognized
                        .known()
                        .map(|known| known.name().to_ascii_lowercase())
                        .or_else(|| recognized.unknown().cloned())
                })
                .map(normalize_species)
                .unwrap_or_else(|| "unknown".to_string());
            let clan = (!state.clan().is_empty()).then(|| state.clan().to_string());
            self.entities.insert(
                id.clone(),
                EntityDescriptor {
                    id,
                    team_id: state.team_id().to_string(),
                    relation,
                    player_name: state.username().to_string(),
                    clan,
                    ship_name: self
                        .metadata
                        .localized_name_from_param(player.vehicle())
                        .unwrap_or_else(|| fallback_ship_name(player.vehicle().name())),
                    ship_code: player.vehicle().index().to_string(),
                    species,
                    max_hp: state.max_health() as f32,
                },
            );
        }
    }

    fn collect_ships(&mut self, clock: GameClock, view: &BattleView<'_>) {
        let world_positions = view.positions();
        let minimap_positions = view.minimap_positions();
        let props = view.vehicle_props_all();
        let dead = view.dead_ships();
        let now = ms(clock);

        for entity in self.entities.values() {
            let entity_id = match entity.id.parse::<u32>() {
                Ok(raw) => EntityId::from(raw),
                Err(_) => continue,
            };
            let world = world_positions.get(&entity_id).copied();
            let minimap = minimap_positions.get(&entity_id).copied();
            if world.is_none() && minimap.is_none() {
                continue;
            }

            let world_fresh = world.is_some_and(|t| same_clock(t.last_updated, clock));
            let minimap_fresh = minimap.is_some_and(|m| same_clock(m.last_updated, clock));
            // Match the toolkit renderer: use smooth world positions while the
            // ship is visible, and the preserved minimap position otherwise.
            let use_minimap = minimap.is_some_and(|m| world.is_none() || !m.visible);
            let point = if use_minimap {
                minimap.map(|m| self.project_minimap(&m.pos))
            } else {
                world
                    .map(|t| self.project_world(t.pos))
                    .or_else(|| minimap.map(|m| self.project_minimap(&m.pos)))
            };
            let Some(point) = point else { continue };

            // Minimap heading is compass-style: 0=north/up, clockwise positive.
            // World yaw uses the same compass convention.
            // MinimapPlacement is the toolkit's authoritative icon heading.
            // Do not alternate it with Transform3d yaw as packets from those
            // two sources arrive at different times.
            let heading_deg = minimap
                .map(|m| normalize_degrees(m.heading.0))
                .or_else(|| world.map(|t| normalize_degrees(t.yaw.0.to_degrees())))
                .unwrap_or(0.0);

            let relation_is_enemy = entity.relation == "enemy";
            let visible = minimap.map(|m| m.visible).unwrap_or(!relation_is_enemy);
            let last_known = minimap.is_some() && !visible;
            let vehicle = props.get(&entity_id).copied();
            let previous = self.ships.get(&entity.id).and_then(|track| track.last());
            let max_hp = vehicle
                .map(|v| v.max_health())
                .filter(|v| *v > 0.0)
                .unwrap_or(entity.max_hp.max(1.0));
            let hp = vehicle
                .map(|v| v.health())
                .filter(|v| *v >= 0.0)
                .or_else(|| previous.map(|p| p.hp))
                .unwrap_or(max_hp);
            let alive =
                !dead.contains_key(&entity_id) && vehicle.map(|v| v.is_alive()).unwrap_or(true);
            let detected_by_enemy =
                !relation_is_enemy && vehicle.map(|v| v.visibility_flags() != 0).unwrap_or(false);
            let submerged = vehicle.map(|v| v.is_invisible()).unwrap_or(false);

            let course_deg = derive_course(previous, now, point, heading_deg, visible);
            let sample = ShipSample {
                t: now,
                x: point.x,
                y: point.y,
                heading_deg,
                course_deg,
                hp,
                max_hp,
                alive,
                visible,
                last_known,
                detected_by_enemy,
                submerged,
                hp_stale: relation_is_enemy && !visible,
            };

            let source_updated = world_fresh || minimap_fresh;
            let semantic_changed = previous.is_none_or(|old| {
                old.hp != sample.hp
                    || old.max_hp != sample.max_hp
                    || old.alive != sample.alive
                    || old.visible != sample.visible
                    || old.last_known != sample.last_known
                    || old.detected_by_enemy != sample.detected_by_enemy
                    || old.submerged != sample.submerged
            });
            let motion_changed = previous.is_none_or(|old| {
                distance(old.x, old.y, sample.x, sample.y) > 0.000_01
                    || angular_distance(old.heading_deg, sample.heading_deg) > 0.05
            });
            if previous.is_none() || semantic_changed || (source_updated && motion_changed) {
                let track = self.ships.entry(entity.id.clone()).or_default();
                if track.last().is_some_and(|old| old.t == now) {
                    *track.last_mut().expect("track has a last sample") = sample;
                } else {
                    track.push(sample);
                }
            }
        }
    }

    fn collect_scores(&mut self, now: i64, view: &BattleView<'_>) {
        let teams: BTreeMap<String, i64> = view
            .team_scores()
            .iter()
            .map(|score| (score.team_index.to_string(), score.score))
            .collect();
        if teams.is_empty() {
            return;
        }
        if self.scores.last().is_none_or(|last| last.teams != teams) {
            self.scores.push(ScoreSample { t: now, teams });
        }
    }

    fn collect_caps(&mut self, now: i64, view: &BattleView<'_>) {
        for cap in view.capture_points() {
            let point = cap.position.map(|position| self.project_world(position));
            let sample = CapSample {
                t: now,
                id: cap_label(cap.index),
                x: point.map(|p| p.x),
                y: point.map(|p| p.y),
                radius: self
                    .map_info
                    .world_distance_to_minimap(cap.radius, MINIMAP_SIZE)
                    / MINIMAP_SIZE as f32,
                owner_team_id: cap.team_id.to_string(),
                invader_team_id: cap.invader_team.to_string(),
                progress: cap.progress.0,
                time_remaining: cap.progress.1,
                has_invaders: cap.has_invaders,
                contested: cap.both_inside,
                enabled: cap.is_enabled,
            };
            let changed = self.last_caps.get(&cap.index).is_none_or(|last| {
                let mut comparable = sample.clone();
                comparable.t = last.t;
                *last != comparable
            });
            if changed {
                self.last_caps.insert(cap.index, sample.clone());
                self.caps.push(sample);
            }
        }
    }

    fn collect_buffs(&mut self, now: i64, view: &BattleView<'_>) {
        let zones = view.buff_zones();
        let current_ids: HashSet<String> = zones.keys().map(ToString::to_string).collect();

        for (entity_id, zone) in zones {
            let id = entity_id.to_string();
            if let Some(params_id) = zone.drop_params_id {
                self.buff_zone_params
                    .insert(id.clone(), params_id.raw() as i64);
            }
            let point = self.project_world(zone.position);
            let marker_name = zone.drop_params_id.and_then(|params_id| {
                let param = GameParamProvider::game_param_by_id(self.metadata, params_id)?;
                let drop = param.drop_data()?;
                Some(if zone.team_id >= 0 {
                    drop.marker_name_active().to_string()
                } else {
                    drop.marker_name_inactive().to_string()
                })
            });
            let sample = BuffSample {
                t: now,
                id: id.clone(),
                x: point.x,
                y: point.y,
                radius: self
                    .map_info
                    .world_distance_to_minimap(zone.radius, MINIMAP_SIZE)
                    / MINIMAP_SIZE as f32,
                active: zone.is_active,
                activation_at: self.buff_activation_at.get(&id).copied(),
                team_id: (zone.team_id >= 0).then(|| zone.team_id.to_string()),
                marker_name,
            };
            let changed = self.last_buffs.get(&id).is_none_or(|last| {
                let mut comparable = sample.clone();
                comparable.t = last.t;
                *last != comparable
            });
            if changed {
                self.last_buffs.insert(id, sample.clone());
                self.buffs.push(sample);
            }
        }

        // Consumed powerups leave the entity stream. Preserve that exact
        // disappearance as a stepped state transition for browser playback.
        let consumed: Vec<String> = self
            .last_buffs
            .iter()
            .filter(|(id, sample)| sample.active && !current_ids.contains(*id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in consumed {
            if let Some(previous) = self.last_buffs.get(&id).cloned() {
                let sample = BuffSample {
                    t: now,
                    active: false,
                    ..previous
                };
                self.last_buffs.insert(id, sample.clone());
                self.buffs.push(sample);
            }
        }
    }

    fn collect_buff_activation_times(&mut self, packet: &Packet<'_, '_>) {
        let PacketType::PropertyUpdate(update) = &packet.payload else {
            return;
        };
        if update.property != "state"
            || update.update_cmd.levels.len() != 2
            || !matches!(
                update.update_cmd.levels.as_slice(),
                [
                    PropertyNestLevel::DictKey("drop"),
                    PropertyNestLevel::DictKey("data")
                ]
            )
        {
            return;
        }
        let UpdateAction::SetRange { values, .. } = &update.update_cmd.action else {
            return;
        };
        for value in values {
            let dictionary = match value {
                ArgValue::FixedDict(dictionary) => Some(dictionary),
                ArgValue::NullableFixedDict(Some(dictionary)) => Some(dictionary),
                _ => None,
            };
            let Some(dictionary) = dictionary else {
                continue;
            };
            let Some(zone_id) = dictionary.get("zoneId").and_then(ArgValue::as_i64) else {
                continue;
            };
            let Some(start_time) = dictionary.get("startTime").and_then(ArgValue::as_f64) else {
                continue;
            };
            self.buff_activation_at.insert(
                EntityId::from(zone_id as i32).to_string(),
                (start_time * 1_000.0).round() as i64,
            );
        }
    }

    /// `state -> drop -> picked` carries paramsId + collecting owners but no
    /// zone id; stash it for `resolve_pickups`, which joins it against the
    /// active zones once the view for this packet is available.
    fn collect_buff_pickups(&mut self, packet: &Packet<'_, '_>) {
        let PacketType::PropertyUpdate(update) = &packet.payload else {
            return;
        };
        if update.property != "state"
            || !matches!(
                update.update_cmd.levels.as_slice(),
                [
                    PropertyNestLevel::DictKey("drop"),
                    PropertyNestLevel::DictKey("picked")
                ]
            )
        {
            return;
        }
        let UpdateAction::SetRange { values, .. } = &update.update_cmd.action else {
            return;
        };
        for value in values {
            let dictionary = match value {
                ArgValue::FixedDict(dictionary) => Some(dictionary),
                ArgValue::NullableFixedDict(Some(dictionary)) => Some(dictionary),
                _ => None,
            };
            let Some(dictionary) = dictionary else {
                continue;
            };
            let Some(params_id) = dictionary.get("paramsId").and_then(ArgValue::as_i64) else {
                continue;
            };
            let Some(ArgValue::Array(owners)) = dictionary.get("owners") else {
                continue;
            };
            let owners: Vec<u32> = owners
                .iter()
                .filter_map(|owner| owner.as_i64().map(|id| id as u32))
                .collect();
            if !owners.is_empty() {
                self.pending_pickups.push((params_id, owners));
            }
        }
    }

    fn resolve_pickups(&mut self, now: i64) {
        for (params_id, owners) in std::mem::take(&mut self.pending_pickups) {
            let owner_id = EntityId::from(owners[0]).to_string();
            let owner_point = self
                .ships
                .get(&owner_id)
                .and_then(|track| track.last())
                .map(|sample| Point {
                    x: sample.x,
                    y: sample.y,
                });
            let team_id = self
                .entities
                .get(&owner_id)
                .map(|entity| entity.team_id.clone())
                .unwrap_or_default();

            // The picked zone is the active zone with this Drop param nearest
            // to the collecting ship — end it at the exact pickup moment
            // instead of waiting ~1.25 s for its EntityLeave.
            let zone_id = self
                .last_buffs
                .iter()
                .filter(|(id, sample)| {
                    sample.active && self.buff_zone_params.get(*id) == Some(&params_id)
                })
                .min_by(|(_, left), (_, right)| {
                    let rank = |sample: &BuffSample| {
                        owner_point
                            .map(|point| distance(point.x, point.y, sample.x, sample.y))
                            .unwrap_or(f32::MAX)
                    };
                    rank(left).total_cmp(&rank(right))
                })
                .map(|(id, _)| id.clone());
            if let Some(zone_id) = &zone_id
                && let Some(previous) = self.last_buffs.get(zone_id).cloned()
            {
                let sample = BuffSample {
                    t: now,
                    active: false,
                    ..previous
                };
                self.last_buffs.insert(zone_id.clone(), sample.clone());
                self.buffs.push(sample);
            }

            let marker_name = GameParamProvider::game_param_by_id(
                self.metadata,
                wows_replays::types::GameParamId::from(params_id as u64),
            )
            .and_then(|param| {
                param
                    .drop_data()
                    .map(|drop| drop.marker_name_active().to_string())
            });
            self.pickups.push(PickupEvent {
                t: now,
                owner_id,
                team_id,
                zone_id,
                marker_name,
            });
        }
    }

    fn collect_salvos(&mut self, view: &BattleView<'_>) {
        for active in view.active_shots() {
            let key = (active.salvo.owner_id.raw(), active.salvo.salvo_id);
            if !self.seen_salvos.insert(key) {
                continue;
            }
            let projectiles = active
                .salvo
                .shots
                .iter()
                .map(|shot| ShellEvent {
                    id: shot.shot_id.to_string(),
                    origin: self.project_world(shot.origin),
                    target: self.project_world(shot.target),
                    flight_ms: (shot.server_time_left.max(0.05) * 1_000.0).round() as i64,
                    speed: shot.speed,
                })
                .collect();
            let ammo_type =
                GameParamProvider::game_param_by_id(self.metadata, active.salvo.params_id)
                    .and_then(|param| {
                        param.projectile().map(|projectile| {
                            AmmoType::from_game_str(projectile.ammo_type())
                                .display_name()
                                .to_string()
                        })
                    });
            self.salvos.push(SalvoEvent {
                id: format!("{}:{}", key.0, key.1),
                t: ms(active.fired_at),
                owner_id: active.salvo.owner_id.to_string(),
                ammo_type,
                projectiles,
            });
        }
    }

    fn collect_torpedoes(&mut self, now: i64, view: &BattleView<'_>) {
        let mut currently_active = HashSet::new();
        for active in view.active_torpedoes() {
            let torpedo = &active.torpedo;
            let key = (torpedo.owner_id.raw(), torpedo.shot_id.raw());
            currently_active.insert(key);
            let sample_clock = ms(active.updated_at);
            let direction = &torpedo.direction;
            let speed = (direction.x * direction.x + direction.z * direction.z).sqrt();
            let sample = TorpedoSample {
                t: sample_clock,
                x: self.project_world(torpedo.origin).x,
                y: self.project_world(torpedo.origin).y,
                heading_deg: normalize_degrees(direction.x.atan2(direction.z).to_degrees()),
                speed,
                armed: torpedo.armed,
            };

            if let Some(index) = self.torpedo_indexes.get(&key).copied() {
                let track = &mut self.torpedoes[index];
                if track.samples.last().is_none_or(|last| *last != sample) {
                    track.samples.push(sample);
                }
                track.ended_at = None;
            } else {
                let index = self.torpedoes.len();
                self.torpedo_indexes.insert(key, index);
                self.torpedoes.push(TorpedoTrack {
                    id: format!("{}:{}", key.0, key.1),
                    owner_id: torpedo.owner_id.to_string(),
                    launched_at: ms(active.launched_at),
                    ended_at: None,
                    samples: vec![sample],
                });
            }
        }

        for ended in self.active_torpedoes.difference(&currently_active) {
            if let Some(index) = self.torpedo_indexes.get(ended).copied() {
                self.torpedoes[index].ended_at = Some(now);
            }
        }
        self.active_torpedoes = currently_active;
    }

    fn collect_kills(&mut self, view: &BattleView<'_>) {
        let kills = view.kills();
        for kill in kills.iter().skip(self.seen_kills) {
            self.kills.push(KillEvent {
                t: ms(kill.clock),
                killer_id: kill.killer.to_string(),
                victim_id: kill.victim.to_string(),
                cause: format!("{:?}", kill.cause),
            });
        }
        self.seen_kills = kills.len();
    }

    /// Resolved main-battery shell hits — the source that attributes HP losses
    /// to an attacker + shell type + penetration quality. Appended like kills.
    fn collect_hits(&mut self, view: &BattleView<'_>) {
        let hits = view.shot_hits();
        for hit in hits.iter().skip(self.seen_hits) {
            // Only shells that actually struck a ship (not water/ground splashes).
            let collision_hits_ship = matches!(
                hit.hit.hit_type.collision.known(),
                Some(CollisionType::HitEntity | CollisionType::HitEntityBB)
            );
            let Some(quality) = shell_hit_quality(hit.hit.hit_type.shell_hit.known()) else {
                continue;
            };
            if !collision_hits_ship {
                continue;
            }
            let victim_id = hit.victim_entity_id.to_string();
            let attacker_id = hit.hit.owner_id.to_string();
            let ammo_type = hit.salvo.as_ref().and_then(|salvo| {
                GameParamProvider::game_param_by_id(self.metadata, salvo.params_id).and_then(
                    |param| {
                        param.projectile().map(|projectile| {
                            AmmoType::from_game_str(projectile.ammo_type())
                                .display_name()
                                .to_string()
                        })
                    },
                )
            });
            self.hits.push(HitEvent {
                t: ms(hit.clock),
                attacker_id,
                victim_id,
                ammo_type,
                quality: quality.to_string(),
            });
        }
        self.seen_hits = hits.len();
    }

    fn collect_smoke(&mut self, now: i64, view: &BattleView<'_>) {
        let screens = view.smoke_screens();
        let current_ids: HashSet<String> = screens.keys().map(ToString::to_string).collect();

        for (entity_id, screen) in screens {
            let id = format!("smoke:{entity_id}");
            let sample = SmokeSample {
                t: now,
                id: id.clone(),
                active: true,
                radius: self
                    .map_info
                    .world_distance_to_minimap(screen.radius.value(), MINIMAP_SIZE)
                    / MINIMAP_SIZE as f32,
                points: screen
                    .points
                    .iter()
                    .map(|point| self.project_world(*point))
                    .collect(),
            };
            let changed = self.last_smoke.get(&id).is_none_or(|last| {
                let mut comparable = sample.clone();
                comparable.t = last.t;
                *last != comparable
            });
            if changed {
                self.last_smoke.insert(id, sample.clone());
                self.smoke.push(sample);
            }
        }

        // A dissipated screen's entity leaves the stream; preserve that exact
        // moment as the stepped inactive transition.
        let dissipated: Vec<String> = self
            .last_smoke
            .iter()
            .filter(|(id, sample)| {
                sample.active && !current_ids.contains(id.trim_start_matches("smoke:"))
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in dissipated {
            if let Some(previous) = self.last_smoke.get(&id).cloned() {
                let sample = SmokeSample {
                    t: now,
                    active: false,
                    ..previous
                };
                self.last_smoke.insert(id, sample.clone());
                self.smoke.push(sample);
            }
        }
    }

    fn collect_planes(&mut self, now: i64, view: &BattleView<'_>) {
        let active = view.active_planes();

        // Squadron ids are reused by the game once a squadron is removed, so
        // each reappearance becomes its own scene-side generation.
        let gone: Vec<u64> = self
            .active_plane_scene_ids
            .keys()
            .filter(|raw| !active.contains_key(&wows_replays::types::PlaneId::from(**raw)))
            .copied()
            .collect();
        for raw in gone {
            if let Some(scene_id) = self.active_plane_scene_ids.remove(&raw)
                && let Some(previous) = self.last_planes.get(&scene_id).cloned()
                && previous.active
            {
                let sample = PlaneSample {
                    t: now,
                    active: false,
                    ..previous
                };
                self.last_planes.insert(scene_id, sample.clone());
                self.planes.push(sample);
            }
        }

        for (plane_id, plane) in active {
            let raw = plane_id.raw();
            let scene_id = match self.active_plane_scene_ids.get(&raw) {
                Some(existing) => existing.clone(),
                None => {
                    let generation = self.plane_generations.entry(raw).or_insert(0);
                    *generation += 1;
                    let scene_id = format!("plane:{plane_id}:{generation}");
                    self.active_plane_scene_ids.insert(raw, scene_id.clone());
                    let icon = self.plane_icon(plane.params_id);
                    self.plane_descriptors.insert(
                        scene_id.clone(),
                        PlaneDescriptor {
                            id: scene_id.clone(),
                            owner_id: plane.owner_id.to_string(),
                            team_id: plane.team_id.to_string(),
                            kind: icon.kind,
                            category: icon.category,
                            icon_dir: icon.icon_dir,
                            icon_base: icon.icon_base,
                        },
                    );
                    scene_id
                }
            };

            let point = self.project_world(plane.position.to_world_pos());
            let sample = PlaneSample {
                t: now,
                id: scene_id.clone(),
                x: point.x,
                y: point.y,
                active: true,
            };
            let changed = self.last_planes.get(&scene_id).is_none_or(|last| {
                let mut comparable = sample.clone();
                comparable.t = last.t;
                *last != comparable
            });
            if changed {
                self.last_planes.insert(scene_id, sample.clone());
                self.planes.push(sample);
            }
        }
    }

    /// Resolve a squadron's display kind/category plus the game minimap icon
    /// (subdirectory + base name), mirroring the minimap renderer so we point
    /// at the real squadron assets instead of a hand-drawn silhouette.
    fn plane_icon(&self, params_id: wows_replays::types::GameParamId) -> PlaneIcon {
        let Some(param) = GameParamProvider::game_param_by_id(self.metadata, params_id) else {
            return PlaneIcon::default();
        };
        let species = param
            .species()
            .and_then(|recognized| recognized.known().cloned());
        let aircraft = param.aircraft();
        let ammo_type = aircraft.map(|a| a.ammo_type()).unwrap_or("");
        let category = aircraft
            .map(|a| a.effective_category(species.as_ref()))
            .unwrap_or(PlaneCategory::Consumable);
        let icon_dir = match category {
            PlaneCategory::Airsupport => "airsupport",
            PlaneCategory::Consumable => "consumables",
            PlaneCategory::Controllable => "controllable",
        };
        let is_consumable = matches!(category, PlaneCategory::Consumable);
        let icon_base = species
            .as_ref()
            .map(|sp| plane_icon_base(*sp, is_consumable, ammo_type));
        PlaneIcon {
            kind: species
                .as_ref()
                .map(|v| format!("{v:?}").to_ascii_lowercase()),
            category: Some(format!("{category:?}").to_ascii_lowercase()),
            icon_dir: icon_base.as_ref().map(|_| icon_dir.to_string()),
            icon_base,
        }
    }

    fn collect_wards(&mut self, now: i64, view: &BattleView<'_>) {
        let active = view.active_wards();

        for (plane_id, ward) in &active {
            let raw = plane_id.raw();
            if self.ward_indexes.contains_key(&raw) {
                continue;
            }
            let point = self.project_world(ward.position);
            self.ward_indexes.insert(raw, self.wards.len());
            self.wards.push(WardRecord {
                id: format!("ward:{plane_id}"),
                owner_id: ward.owner_id.to_string(),
                x: point.x,
                y: point.y,
                radius: self
                    .map_info
                    .world_distance_to_minimap(ward.radius.value(), MINIMAP_SIZE)
                    / MINIMAP_SIZE as f32,
                added_at: now,
                removed_at: None,
            });
        }

        let removed: Vec<u64> = self
            .ward_indexes
            .keys()
            .filter(|raw| !active.contains_key(&wows_replays::types::PlaneId::from(**raw)))
            .copied()
            .collect();
        for raw in removed {
            if let Some(index) = self.ward_indexes.remove(&raw)
                && self.wards[index].removed_at.is_none()
            {
                self.wards[index].removed_at = Some(now);
            }
        }
    }

    fn collect_consumables(&mut self, view: &BattleView<'_>) {
        for (entity_id, activations) in view.active_consumables() {
            for activation in activations {
                let name = match activation.consumable.known() {
                    Some(consumable) => format!("{consumable:?}"),
                    None => activation
                        .consumable
                        .unknown()
                        .cloned()
                        .unwrap_or_else(|| "Unknown".to_string()),
                };
                let activated_ms = ms(activation.activated_at);
                if !self
                    .seen_consumables
                    .insert((entity_id.raw(), name.clone(), activated_ms))
                {
                    continue;
                }
                self.consumables.push(ConsumableEvent {
                    t: activated_ms,
                    ship_id: entity_id.to_string(),
                    name,
                    duration_ms: (f64::from(activation.duration) * 1_000.0).round() as i64,
                });
            }
        }
    }

    fn collect_chat(&mut self, view: &BattleView<'_>) {
        let messages = view.game_chat();
        for message in messages.iter().skip(self.seen_chat) {
            let sender_id = self
                .entities
                .values()
                .find(|entity| entity.player_name == message.sender_name)
                .map(|entity| entity.id.clone())
                .unwrap_or_default();
            self.chat.push(ChatEvent {
                t: ms(message.clock),
                sender_id,
                sender_name: message.sender_name.clone(),
                channel: chat_channel_name(&message.channel),
                message: message.message.clone(),
            });
        }
        self.seen_chat = messages.len();
    }

    fn project_world(&self, position: WorldPos) -> Point {
        let point = self.map_info.world_to_minimap(position, MINIMAP_SIZE);
        Point {
            x: point.x / MINIMAP_SIZE as f32,
            y: point.y / MINIMAP_SIZE as f32,
        }
    }

    fn project_minimap(&self, position: &wows_replays::types::NormalizedPos) -> Point {
        let point = self.map_info.normalized_to_minimap(position, MINIMAP_SIZE);
        Point {
            x: point.x / MINIMAP_SIZE as f32,
            y: point.y / MINIMAP_SIZE as f32,
        }
    }

    fn normalize_times(&mut self, start: i64) {
        for track in self.ships.values_mut() {
            normalize_step_vec(track, start, |sample| &mut sample.t);
        }
        normalize_step_vec(&mut self.scores, start, |sample| &mut sample.t);
        normalize_caps(&mut self.caps, start);
        normalize_buffs(&mut self.buffs, start);
        normalize_vec(&mut self.salvos, start, |sample| &mut sample.t);
        normalize_vec(&mut self.kills, start, |sample| &mut sample.t);
        normalize_vec(&mut self.hits, start, |sample| &mut sample.t);
        normalize_grouped(
            &mut self.smoke,
            start,
            |sample| sample.id.clone(),
            |sample| &mut sample.t,
        );
        normalize_grouped(
            &mut self.planes,
            start,
            |sample| sample.id.clone(),
            |sample| &mut sample.t,
        );
        self.wards.retain_mut(|ward| {
            if ward.removed_at.is_some_and(|removed| removed <= start) {
                return false;
            }
            ward.added_at = (ward.added_at - start).max(0);
            ward.removed_at = ward.removed_at.map(|removed| (removed - start).max(0));
            true
        });
        normalize_vec(&mut self.consumables, start, |event| &mut event.t);
        normalize_vec(&mut self.pickups, start, |event| &mut event.t);
        // Countdown banter is part of the battle record: clamp instead of drop.
        for message in &mut self.chat {
            message.t = (message.t - start).max(0);
        }
        self.torpedoes.retain_mut(|track| {
            if track.launched_at < start {
                track.launched_at = 0;
            } else {
                track.launched_at -= start;
            }
            track.ended_at = track.ended_at.map(|ended| (ended - start).max(0));
            normalize_vec(&mut track.samples, start, |sample| &mut sample.t);
            !track.samples.is_empty()
        });
    }
}

impl WorldScanCollector for SceneCollector<'_> {
    fn observe(&mut self, packet: &Packet<'_, '_>, _prev_clock: GameClock, view: &BattleView<'_>) {
        self.collect_buff_activation_times(packet);
        self.collect_buff_pickups(packet);
        self.observe_view(packet, view);
    }

    fn finish(&mut self, view: &BattleView<'_>) {
        self.battle_start_ms = view.battle_start_clock().map(ms).or(self.battle_start_ms);
        self.battle_end_ms = view.battle_end_clock().map(ms).or(self.battle_end_ms);
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    fs::create_dir_all(&args.output)
        .with_context(|| format!("creating output directory {}", args.output.display()))?;

    let replay_bytes = fs::read(&args.replay)
        .with_context(|| format!("reading replay {}", args.replay.display()))?;
    let source_sha256 = hex::encode(Sha256::digest(&replay_bytes));
    let replay = ReplayFile::from_bytes(&replay_bytes)
        .map_err(|error| anyhow!("parsing replay: {error:?}"))?;
    let version = Version::from_client_exe(&replay.meta.clientVersionFromExe);

    eprintln!(
        "Loading game resources for {}",
        replay.meta.clientVersionFromExe
    );
    let resources = game_data::load_game_resources(&args.game, &version)
        .map_err(|error| anyhow!("loading game resources: {error}"))?;
    let vfs = resources.vfs;
    let game_params = GameMetadataProvider::from_vfs(&vfs)
        .map_err(|error| anyhow!("loading GameParams: {error:?}"))?;
    if let Some(build) = version.build_number() {
        let translations = game_data::translations_path(&args.game, build);
        if let Ok(file) = File::open(&translations)
            && let Ok(catalog) = gettext::Catalog::parse(file)
        {
            game_params.set_translations(catalog);
        }
    }
    let constants = GameConstants::from_vfs(&vfs);

    let map_info =
        load_map_info(&replay.meta.mapName, &vfs).unwrap_or(MapInfo { space_size: 48_000 });
    let map_file = if let Some(image) = load_map_image(&replay.meta.mapName, &vfs) {
        if args.inline_assets {
            let mut png_bytes: Vec<u8> = Vec::new();
            image::DynamicImage::ImageRgb8(image)
                .write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
                .context("encoding map image as PNG")?;
            Some(format!(
                "data:image/png;base64,{}",
                BASE64.encode(png_bytes)
            ))
        } else {
            let path = args.output.join("map.png");
            image
                .save(&path)
                .with_context(|| format!("writing {}", path.display()))?;
            Some("./map.png".to_string())
        }
    } else {
        None
    };
    let powerup_icon_images = load_powerup_icons(&vfs, 96, Some(&version));
    let plane_icon_images = load_plane_icons(&vfs, Some(&version));

    eprintln!("Scanning replay packets");
    let mut collector = SceneCollector::new(map_info.clone(), &game_params);
    scan_replay_world(
        &replay.meta,
        &game_params,
        &constants,
        version,
        &replay,
        &mut [&mut collector],
    );

    let battle_start = collector.battle_start_ms.unwrap_or(0);
    let battle_end = collector.battle_end_ms.unwrap_or(collector.last_clock_ms);
    let duration_ms = (battle_end - battle_start).max(0);
    collector.normalize_times(battle_start);
    let powerup_icons = export_powerup_icons(
        &args.output,
        &collector.buffs,
        &powerup_icon_images,
        args.inline_assets,
    )?;
    let plane_icons = export_plane_icons(
        &args.output,
        &collector.plane_descriptors,
        &plane_icon_images,
    )?;

    let entities: Vec<EntityDescriptor> = collector.entities.values().cloned().collect();
    let self_entity = entities
        .iter()
        .find(|entity| entity.relation == "self")
        .or_else(|| entities.first());
    let perspective = Perspective {
        player_name: self_entity
            .map(|entity| entity.player_name.clone())
            .unwrap_or_else(|| replay.meta.playerName.clone()),
        team_id: self_entity
            .map(|entity| entity.team_id.clone())
            .unwrap_or_default(),
        entity_id: self_entity
            .map(|entity| entity.id.clone())
            .unwrap_or_default(),
    };
    let teams = build_teams(&entities);
    let id = source_sha256[..16].to_string();
    let replay_name = args
        .replay
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("replay.wowsreplay")
        .to_string();

    let scene = ReplayScene {
        schema: "tfd-replay-scene",
        version: 1,
        replay: ReplayInfo {
            id,
            name: replay_name,
            source_sha256,
            game_build: replay.meta.clientVersionFromExe.clone(),
            duration_ms,
            battle_start_ms: battle_start,
            perspective,
        },
        map: MapDescriptor {
            name: replay.meta.mapName.clone(),
            image_url: map_file,
            coordinate_space: "normalized",
            space_size: map_info.space_size,
        },
        assets: SceneAssets {
            powerup_icons,
            plane_icons,
        },
        teams,
        entities,
        aviation: collector.plane_descriptors,
        wards: collector.wards,
        tracks: SceneTracks {
            ships: collector.ships,
            scores: collector.scores,
            caps: collector.caps,
            buffs: collector.buffs,
            smoke: collector.smoke,
            planes: collector.planes,
        },
        events: SceneEvents {
            salvos: collector.salvos,
            torpedoes: collector.torpedoes,
            kills: collector.kills,
            hits: collector.hits,
            consumables: collector.consumables,
            chat: collector.chat,
            pickups: collector.pickups,
        },
        coverage: Coverage {
            mode: "single-perspective",
            honest_visibility: true,
            warning: "Enemy positions and HP may be stale or absent while outside the recording client's observation.",
        },
    };

    let output = args.output.join("scene.json");
    fs::write(&output, serde_json::to_vec_pretty(&scene)?)
        .with_context(|| format!("writing {}", output.display()))?;
    eprintln!(
        "Wrote {} ({} entities, {} ship samples, {} buff samples, {} salvos, {} torpedoes, {} smoke samples, {} plane samples over {} squadrons, {} wards, {} consumables, {} chat, {} pickups)",
        output.display(),
        scene.entities.len(),
        scene.tracks.ships.values().map(Vec::len).sum::<usize>(),
        scene.tracks.buffs.len(),
        scene.events.salvos.len(),
        scene.events.torpedoes.len(),
        scene.tracks.smoke.len(),
        scene.tracks.planes.len(),
        scene.aviation.len(),
        scene.wards.len(),
        scene.events.consumables.len(),
        scene.events.chat.len(),
        scene.events.pickups.len(),
    );
    eprintln!("  main-battery hits resolved: {}", scene.events.hits.len());
    Ok(())
}

#[derive(Default)]
struct PlaneIcon {
    kind: Option<String>,
    category: Option<String>,
    icon_dir: Option<String>,
    icon_base: Option<String>,
}

/// Minimal snake_case matching the renderer's `convert_case::Case::Snake` for
/// the ammo strings we feed it (e.g. `AP` → `ap`, `SeaMine` → `sea_mine`).
fn snake_case(input: &str) -> String {
    let mut out = String::new();
    let mut prev_is_lower_or_digit = false;
    for ch in input.chars() {
        if ch.is_ascii_uppercase() {
            if prev_is_lower_or_digit {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            prev_is_lower_or_digit = false;
        } else if matches!(ch, '_' | '-' | ' ') {
            out.push('_');
            prev_is_lower_or_digit = false;
        } else {
            out.push(ch);
            prev_is_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out
}

/// Port of the minimap renderer's `species_to_icon_base` so exported squadron
/// descriptors reference the same game icon files.
fn plane_icon_base(species: Species, is_consumable: bool, ammo_type: &str) -> String {
    let snake = snake_case(ammo_type);
    let ammo = match snake.as_str() {
        "sea_mine" => "mine",
        "depthcharge" => "depth_charge",
        other => other,
    };
    if is_consumable {
        match species {
            Species::Dive if !ammo.is_empty() => format!("bomber_{ammo}"),
            Species::Dive => "bomber".to_string(),
            Species::Fighter => "fighter".to_string(),
            Species::Scout => "scout".to_string(),
            other => snake_case(&format!("{other:?}")),
        }
    } else {
        match species {
            Species::Fighter if !ammo.is_empty() => format!("fighter_{ammo}"),
            Species::Fighter => "fighter".to_string(),
            Species::Dive if !ammo.is_empty() => format!("bomber_{ammo}"),
            Species::Dive => "bomber".to_string(),
            Species::Bomber if ammo == "torpedo_deepwater" => "torpedo_deepwater".to_string(),
            Species::Bomber => "torpedo_regular".to_string(),
            Species::Skip if !ammo.is_empty() => format!("skip_{ammo}"),
            Species::Skip => "skip".to_string(),
            Species::Airship | Species::Auxiliary => "auxiliary".to_string(),
            _ if !ammo.is_empty() => format!("fighter_{ammo}"),
            _ => "fighter".to_string(),
        }
    }
}

/// Export the real game squadron minimap icons actually referenced by this
/// battle. Keys match the renderer's `{dir}/{base}_{relation}` scheme so the
/// web player can pick the relation variant (own/ally/enemy) per squadron.
fn export_plane_icons(
    output: &Path,
    descriptors: &BTreeMap<String, PlaneDescriptor>,
    available: &HashMap<String, image::RgbaImage>,
) -> Result<BTreeMap<String, String>> {
    let used_bases: BTreeSet<(String, String)> = descriptors
        .values()
        .filter_map(|descriptor| {
            Some((descriptor.icon_dir.clone()?, descriptor.icon_base.clone()?))
        })
        .collect();
    if used_bases.is_empty() {
        return Ok(BTreeMap::new());
    }

    let directory = output.join("planes");
    fs::create_dir_all(&directory).with_context(|| format!("creating {}", directory.display()))?;
    let mut exported = BTreeMap::new();
    for (dir, base) in used_bases {
        for suffix in ["own", "ally", "enemy"] {
            let key = format!("{dir}/{base}_{suffix}");
            let Some(image) = available.get(&key) else {
                continue;
            };
            let filename = format!("{}.png", safe_asset_name(&key));
            let path = directory.join(&filename);
            image
                .save(&path)
                .with_context(|| format!("writing {}", path.display()))?;
            exported.insert(key, format!("./planes/{filename}"));
        }
    }
    Ok(exported)
}

fn export_powerup_icons(
    output: &Path,
    samples: &[BuffSample],
    available: &HashMap<String, image::RgbaImage>,
    inline: bool,
) -> Result<BTreeMap<String, String>> {
    let mut used: BTreeSet<String> = samples
        .iter()
        .filter_map(|sample| sample.marker_name.clone())
        .collect();
    let active_variants: Vec<String> = used
        .iter()
        .filter_map(|marker| {
            marker
                .strip_suffix("_inactive")
                .map(|base| format!("{base}_active"))
        })
        .collect();
    used.extend(active_variants);
    if used.is_empty() {
        return Ok(BTreeMap::new());
    }

    let directory = output.join("powerups");
    if !inline {
        fs::create_dir_all(&directory)
            .with_context(|| format!("creating {}", directory.display()))?;
    }
    let mut exported = BTreeMap::new();
    for marker_name in used {
        let Some(image) = available.get(&marker_name) else {
            eprintln!("No game icon found for Arms Race marker {marker_name}");
            continue;
        };
        if inline {
            // Self-contained scene: base64 the icon into a data URL (mirrors the
            // map handling) so bridge mode needs no per-asset HTTP route.
            let mut png_bytes: Vec<u8> = Vec::new();
            image::DynamicImage::ImageRgba8(image.clone())
                .write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
                .with_context(|| format!("encoding powerup icon {marker_name} as PNG"))?;
            exported.insert(
                marker_name,
                format!("data:image/png;base64,{}", BASE64.encode(png_bytes)),
            );
        } else {
            let filename = format!("{}.png", safe_asset_name(&marker_name));
            let path = directory.join(&filename);
            image
                .save(&path)
                .with_context(|| format!("writing {}", path.display()))?;
            exported.insert(marker_name, format!("./powerups/{filename}"));
        }
    }
    Ok(exported)
}

fn safe_asset_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn build_teams(entities: &[EntityDescriptor]) -> Vec<TeamDescriptor> {
    let mut relations: BTreeMap<String, String> = BTreeMap::new();
    for entity in entities {
        relations
            .entry(entity.team_id.clone())
            .or_insert_with(|| entity.relation.clone());
    }
    relations
        .into_iter()
        .map(|(id, relation)| {
            // Team colors follow the tfd-engine dark theme: accent teal for
            // allies, destructive red for enemies.
            let (name, color) = if relation == "enemy" {
                ("Enemy", "#ef5a4c")
            } else {
                ("Allied", "#00d1a7")
            };
            TeamDescriptor {
                id,
                name: name.to_string(),
                color: color.to_string(),
            }
        })
        .collect()
}

fn normalize_vec<T>(items: &mut Vec<T>, start: i64, mut time: impl FnMut(&mut T) -> &mut i64) {
    items.retain_mut(|item| {
        let value = time(item);
        if *value < start {
            return false;
        }
        *value -= start;
        true
    });
}

/// Normalize state tracks without discarding the state already established
/// during the battle countdown. The last pre-start value becomes the t=0
/// baseline; later values retain their relative battle time.
fn normalize_step_vec<T>(items: &mut Vec<T>, start: i64, mut time: impl FnMut(&mut T) -> &mut i64) {
    let mut baseline = None;
    let mut normalized = Vec::with_capacity(items.len());
    for mut item in items.drain(..) {
        let original = *time(&mut item);
        if original < start {
            baseline = Some(item);
        } else {
            *time(&mut item) = original - start;
            normalized.push(item);
        }
    }
    if let Some(mut item) = baseline {
        *time(&mut item) = 0;
        normalized.insert(0, item);
    }
    *items = normalized;
}

/// Grouped-baseline normalization for multi-entity sample streams: per id,
/// the last pre-start sample becomes the t=0 baseline (matching the cap/buff
/// treatment) and later samples keep their relative battle time.
fn normalize_grouped<T: Clone>(
    items: &mut Vec<T>,
    start: i64,
    id: impl Fn(&T) -> String,
    mut time: impl FnMut(&mut T) -> &mut i64,
) {
    let mut baselines = BTreeMap::<String, T>::new();
    let mut normalized = Vec::with_capacity(items.len());
    for mut item in items.drain(..) {
        if *time(&mut item) < start {
            baselines.insert(id(&item), item);
        } else {
            *time(&mut item) -= start;
            normalized.push(item);
        }
    }
    for mut item in baselines.into_values() {
        *time(&mut item) = 0;
        normalized.push(item);
    }
    normalized.sort_by_key(|item| {
        let mut probe = item.clone();
        (*time(&mut probe), id(item))
    });
    *items = normalized;
}

fn normalize_caps(items: &mut Vec<CapSample>, start: i64) {
    let mut baselines = BTreeMap::<String, CapSample>::new();
    let mut normalized = Vec::with_capacity(items.len());
    for mut sample in items.drain(..) {
        if sample.t < start {
            baselines.insert(sample.id.clone(), sample);
        } else {
            sample.t -= start;
            normalized.push(sample);
        }
    }
    for mut sample in baselines.into_values() {
        sample.t = 0;
        normalized.push(sample);
    }
    normalized.sort_by(|left, right| left.t.cmp(&right.t).then_with(|| left.id.cmp(&right.id)));
    *items = normalized;
}

fn normalize_buffs(items: &mut Vec<BuffSample>, start: i64) {
    let mut baselines = BTreeMap::<String, BuffSample>::new();
    let mut normalized = Vec::with_capacity(items.len());
    for mut sample in items.drain(..) {
        sample.activation_at = sample
            .activation_at
            .map(|activation| (activation - start).max(0));
        if sample.t < start {
            baselines.insert(sample.id.clone(), sample);
        } else {
            sample.t -= start;
            normalized.push(sample);
        }
    }
    for mut sample in baselines.into_values() {
        sample.t = 0;
        normalized.push(sample);
    }
    normalized.sort_by(|left, right| left.t.cmp(&right.t).then_with(|| left.id.cmp(&right.id)));
    *items = normalized;
}

/// Friendly penetration-quality label for a resolved shell hit, or None when
/// the hit type is not a shell interaction we surface.
fn shell_hit_quality(shell_hit: Option<&ShellHitType>) -> Option<&'static str> {
    Some(match shell_hit? {
        ShellHitType::Normal => "penetration",
        ShellHitType::MajorHit => "citadel",
        ShellHitType::Overpenetration | ShellHitType::ExitOverpenetration => "overpen",
        ShellHitType::NoPenetration => "shatter",
        ShellHitType::Ricochet => "ricochet",
        ShellHitType::Underwater => "underwater",
        ShellHitType::None => return None,
    })
}

fn chat_channel_name(channel: &wows_replays::analyzer::battle_controller::ChatChannel) -> String {
    use wows_replays::analyzer::battle_controller::ChatChannel;
    match channel {
        ChatChannel::Division => "division".to_string(),
        ChatChannel::Global => "global".to_string(),
        ChatChannel::Team => "team".to_string(),
        ChatChannel::System => "system".to_string(),
        ChatChannel::Unknown(raw) => raw.to_ascii_lowercase(),
    }
}

fn ms(clock: GameClock) -> i64 {
    (clock.seconds() * 1_000.0).round() as i64
}

fn same_clock(left: GameClock, right: GameClock) -> bool {
    (left.seconds() - right.seconds()).abs() < 0.000_1
}

fn distance(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    (bx - ax).hypot(by - ay)
}

fn normalize_degrees(value: f32) -> f32 {
    value.rem_euclid(360.0)
}

fn angular_distance(left: f32, right: f32) -> f32 {
    ((right - left + 180.0).rem_euclid(360.0) - 180.0).abs()
}

fn derive_course(
    previous: Option<&ShipSample>,
    now: i64,
    point: Point,
    heading_deg: f32,
    visible: bool,
) -> f32 {
    let Some(previous) = previous else {
        return heading_deg;
    };
    let elapsed = (now - previous.t) as f32 / 1_000.0;
    if !visible || !previous.visible || !(0.02..=2.0).contains(&elapsed) {
        return previous.course_deg;
    }
    let dx = point.x - previous.x;
    let dy = point.y - previous.y;
    let normalized_distance = dx.hypot(dy);
    if normalized_distance < 0.000_001 {
        return previous.course_deg;
    }

    // Screen Y grows southward, hence atan2(dx, -dy) yields compass degrees
    // (0 north, 90 east). Physical speed needs a separately validated mapping
    // from BigWorld units, so this exporter intentionally does not invent it.
    normalize_degrees(dx.atan2(-dy).to_degrees())
}

fn cap_label(index: usize) -> String {
    const LABELS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    LABELS
        .get(index)
        .map(|label| (*label as char).to_string())
        .unwrap_or_else(|| format!("CAP-{index}"))
}

fn fallback_ship_name(raw: &str) -> String {
    raw.split_once('_')
        .map(|(_, name)| name.replace('_', " "))
        .unwrap_or_else(|| raw.to_string())
}

fn normalize_species(raw: String) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "aircarrier" | "aircraftcarrier" => "carrier".to_string(),
        other => other.to_string(),
    }
}

#[allow(dead_code)]
fn _is_replay(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wowsreplay"))
}
