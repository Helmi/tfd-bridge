//! Observed consumable metadata, resolved from the replay build's toolkit data.
use serde::Serialize;
use wows_battle_world::view::BattleView;
use wows_minimap_renderer::{MINIMAP_SIZE, map_data::MapInfo};
use wows_replays::types::EntityId;
use wowsunpack::game_params::types::Meters;
use wowsunpack::{data::Version, game_params::types::GameParamProvider, game_types::Consumable};

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConsumableVisual {
    #[serde(skip_serializing_if = "Option::is_none")]
    icon_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ability_variant: Option<String>,
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ship_range_meters: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    torpedo_range_meters: Option<f32>,
    /// Fraction of full map width; metres are converted by toolkit unit types.
    #[serde(skip_serializing_if = "Option::is_none")]
    ship_radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    torpedo_radius: Option<f32>,
}

fn finite_range(value: Option<f32>) -> Option<f32> {
    value.filter(|v| v.is_finite() && *v > 0.0)
}

fn normalized(value: Option<f32>, map: &MapInfo) -> Option<f32> {
    if map.space_size <= 0 {
        return None;
    }
    finite_range(value).map(|m| {
        map.world_distance_to_minimap(Meters::from(m).to_bigworld().value(), MINIMAP_SIZE)
            / MINIMAP_SIZE as f32
    })
}

fn choose(mut candidates: Vec<ConsumableVisual>, map: &MapInfo) -> Option<ConsumableVisual> {
    let mut picked = candidates.pop()?;
    // Never guess a range when multiple matching variants disagree.
    if candidates.iter().any(|c| {
        c.ship_range_meters != picked.ship_range_meters
            || c.torpedo_range_meters != picked.torpedo_range_meters
    }) {
        picked.ship_range_meters = None;
        picked.torpedo_range_meters = None;
        picked.source = "ambiguous-ability";
    }
    if !candidates.is_empty() {
        picked.ability_variant = None;
    }
    if candidates.iter().any(|c| c.icon_key != picked.icon_key) {
        picked.icon_key = None;
    }
    picked.ship_range_meters = finite_range(picked.ship_range_meters);
    picked.torpedo_range_meters = finite_range(picked.torpedo_range_meters);
    picked.ship_radius = normalized(picked.ship_range_meters, map);
    picked.torpedo_radius = normalized(picked.torpedo_range_meters, map);
    Some(picked)
}

pub(crate) fn resolve(
    metadata: &dyn GameParamProvider,
    view: &BattleView<'_>,
    entity: EntityId,
    kind: Consumable,
    version: Version,
    map: &MapInfo,
) -> Option<ConsumableVisual> {
    if !matches!(
        kind,
        Consumable::Radar
            | Consumable::HydroacousticSearch
            | Consumable::SpottingAircraft
            | Consumable::CatapultFighter
            | Consumable::CallFighters
    ) {
        return None;
    }
    let props = view.vehicle_props(entity);
    let players = view.player_entities();
    let ship_id = props
        .map(|p| p.ship_config().ship_params_id())
        .or_else(|| players.get(&entity).map(|p| p.vehicle().id()))?;
    let ship = metadata.game_param_by_id(ship_id)?;
    let vehicle = ship.vehicle()?;
    let equipped: Vec<String> = props
        .into_iter()
        .flat_map(|p| p.ship_config().abilities())
        .filter_map(|id| metadata.game_param_by_id(*id).map(|p| p.name().to_owned()))
        .collect();
    let mut candidates = Vec::new();
    for slot in vehicle.abilities().into_iter().flatten() {
        for (ability_name, variant) in slot {
            let Some(param) = metadata.game_param_by_name(ability_name) else {
                continue;
            };
            let Some(category) = param.ability().and_then(|a| a.get_category(variant)) else {
                continue;
            };
            if category.consumable_type(version).known() != Some(&kind) {
                continue;
            }
            let detection = matches!(kind, Consumable::Radar | Consumable::HydroacousticSearch);
            candidates.push(ConsumableVisual {
                icon_key: Some(ability_name.clone()),
                ability_variant: Some(variant.clone()),
                source: if equipped.contains(ability_name) {
                    "equipped-ability"
                } else {
                    "ship-definition"
                },
                ship_range_meters: detection
                    .then(|| category.detection_radius())
                    .flatten()
                    .map(|m| m.value()),
                torpedo_range_meters: (kind == Consumable::HydroacousticSearch)
                    .then(|| category.torpedo_detection_radius())
                    .flatten()
                    .map(|m| m.value()),
                ..Default::default()
            });
        }
    }
    if candidates.iter().any(|c| c.source == "equipped-ability") {
        candidates.retain(|c| c.source == "equipped-ability");
    }
    if !candidates.is_empty() {
        return choose(candidates, map);
    }

    // Same toolkit fallback used by its own renderer when ability detail is absent.
    let hull_name = props
        .and_then(|p| p.ship_config().hull())
        .and_then(|id| metadata.game_param_by_id(id).map(|p| p.name().to_owned()));
    let ranges = vehicle.resolve_ranges(Some(metadata), hull_name.as_deref(), version);
    let radius = match kind {
        Consumable::Radar => ranges.radar_m,
        Consumable::HydroacousticSearch => ranges.hydro_m,
        _ => None,
    };
    choose(
        vec![ConsumableVisual {
            source: "ship-range-fallback",
            ship_range_meters: radius.map(|m| m.value()),
            ..Default::default()
        }],
        map,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn units_and_ambiguous_variants_do_not_invent_ranges() {
        let map = MapInfo { space_size: 1200 }; // 36 km map in toolkit world units.
        let candidate = ConsumableVisual {
            ship_range_meters: Some(9000.0),
            source: "ship-definition",
            ..Default::default()
        };
        assert!(
            (choose(vec![candidate.clone()], &map)
                .unwrap()
                .ship_radius
                .unwrap()
                - 0.25)
                .abs()
                < 0.00001
        );
        let different = ConsumableVisual {
            ship_range_meters: Some(12000.0),
            ..candidate.clone()
        };
        let ambiguous = choose(vec![candidate, different], &map).unwrap();
        assert!(ambiguous.ship_radius.is_none());
        assert_eq!(ambiguous.source, "ambiguous-ability");
        assert!(normalized(Some(f32::NAN), &map).is_none());
        assert!(normalized(Some(-1.0), &map).is_none());
    }
    #[test]
    fn hydro_ship_and_torpedo_radii_remain_distinct() {
        let v = choose(
            vec![ConsumableVisual {
                ship_range_meters: Some(5000.0),
                torpedo_range_meters: Some(3500.0),
                source: "equipped-ability",
                ..Default::default()
            }],
            &MapInfo { space_size: 1200 },
        )
        .unwrap();
        assert!(v.ship_radius.unwrap() > v.torpedo_radius.unwrap());
        assert_eq!(v.source, "equipped-ability");
    }
}
