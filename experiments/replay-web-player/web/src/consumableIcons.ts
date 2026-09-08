import type { ConsumableActivation } from './types';
const bundled = new Set(["PCY004_Fighter","PCY005_Spotter","PCY008_SonarSearch","PCY012_FighterPremium","PCY013_SpotterPremium","PCY013_Spotter_Circle_Premium","PCY016_SonarSearchPremium","PCY016_SonarSearch_Short_Premium","PCY019_RLSSearch","PCY020_RLSSearchPremium","PCY026_FighterSuper","PCY027_SpotterSuper","PCY030_SonarSearchSuper","PCY032_RLSSearchSuper","PCY035_CallFighters","PCY035_CallFighters_Ambush","PCY035_CallFighters_Upgrade","PCY038_FighterAuto","PCY061_FighterPremium_TimeBased","PCY062_SpotterPremium_TimeBased","PCY065_SonarSearchPremium_TimeBased","PCY066_RLSSearchPremium_TimeBased","PCY069_CallFighters_TimeBased","PCY078_PlaneTacticalFighters","PCY078_PlaneTacticalFighters_Upgrade"]);
const defaults: Record<string,string> = {
 Radar:'PCY020_RLSSearchPremium', HydroacousticSearch:'PCY016_SonarSearchPremium',
 SpottingAircraft:'PCY013_SpotterPremium', CatapultFighter:'PCY012_FighterPremium', CallFighters:'PCY035_CallFighters',
};
export function consumableIconKey(activation: ConsumableActivation): string | undefined {
 const fallback = defaults[activation.name];
 if (!fallback) return undefined;
 const key = activation.visual?.iconKey;
 return key && bundled.has(key) ? key : fallback;
}
export function consumableIconUrl(key: string): string | undefined {
 return bundled.has(key) ? import.meta.env.BASE_URL + 'assets/consumables/consumable_' + key + '.png' : undefined;
}
