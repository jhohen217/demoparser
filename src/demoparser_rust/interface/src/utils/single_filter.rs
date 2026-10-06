//! Single filter module for the interface crate
//!
//! This module contains utility functions for filtering collections and kills.

use crate::models::collection::KillCollection;
use crate::models::kill::Kill;

/// Filter collections by killer SteamID
pub fn filter_collections_by_killer_steamid(
    collections: &[KillCollection],
    killer_steamid: &str,
) -> Vec<KillCollection> {
    collections
        .iter()
        .filter(|collection| collection.killer_steamid == killer_steamid)
        .cloned()
        .collect()
}

/// Filter collections by collection type
pub fn filter_collections_by_type(
    collections: &[KillCollection],
    collection_type: &str,
) -> Vec<KillCollection> {
    collections
        .iter()
        .filter(|collection| collection.collection_type == collection_type)
        .cloned()
        .collect()
}

/// Filter collections by map name
pub fn filter_collections_by_map(
    collections: &[KillCollection],
    map_name: &str,
) -> Vec<KillCollection> {
    collections
        .iter()
        .filter(|collection| collection.map_name == map_name)
        .cloned()
        .collect()
}

/// Filter collections by weapon
pub fn filter_collections_by_weapon(
    collections: &[KillCollection],
    weapon: &str,
) -> Vec<KillCollection> {
    collections
        .iter()
        .filter(|collection| collection.weapons.iter().any(|w| w == weapon))
        .cloned()
        .collect()
}

/// Filter collections by folder
pub fn filter_collections_by_folder(
    collections: &[KillCollection],
    folder: &str,
) -> Vec<KillCollection> {
    collections
        .iter()
        .filter(|collection| collection.folder == folder)
        .cloned()
        .collect()
}

/// Filter collections by round
pub fn filter_collections_by_round(
    collections: &[KillCollection],
    round: i32,
) -> Vec<KillCollection> {
    collections
        .iter()
        .filter(|collection| collection.round == round)
        .cloned()
        .collect()
}

/// Filter collections by killer team
pub fn filter_collections_by_killer_team(
    collections: &[KillCollection],
    killer_team: &str,
) -> Vec<KillCollection> {
    collections
        .iter()
        .filter(|collection| collection.killer_team == killer_team)
        .cloned()
        .collect()
}

/// Filter collections by victim team
pub fn filter_collections_by_victim_team(
    collections: &[KillCollection],
    victim_team: &str,
) -> Vec<KillCollection> {
    collections
        .iter()
        .filter(|collection| collection.victim_team == victim_team)
        .cloned()
        .collect()
}

/// Filter collections by killer name
pub fn filter_collections_by_killer_name(
    collections: &[KillCollection],
    killer_name: &str,
) -> Vec<KillCollection> {
    collections
        .iter()
        .filter(|collection| collection.killer_name == killer_name)
        .cloned()
        .collect()
}

/// Filter kills by killer SteamID
pub fn filter_kills_by_killer_steamid(kills: &[Kill], killer_steamid: &str) -> Vec<Kill> {
    kills
        .iter()
        .filter(|kill| kill.killer_steamid == killer_steamid)
        .cloned()
        .collect()
}

/// Filter kills by victim SteamID
pub fn filter_kills_by_victim_steamid(kills: &[Kill], victim_steamid: &str) -> Vec<Kill> {
    kills
        .iter()
        .filter(|kill| kill.victim_steamid == victim_steamid)
        .cloned()
        .collect()
}

/// Filter kills by weapon
pub fn filter_kills_by_weapon(kills: &[Kill], weapon: &str) -> Vec<Kill> {
    kills
        .iter()
        .filter(|kill| kill.weapon == weapon)
        .cloned()
        .collect()
}

/// Filter kills by round
pub fn filter_kills_by_round(kills: &[Kill], round: i32) -> Vec<Kill> {
    kills
        .iter()
        .filter(|kill| kill.round == round)
        .cloned()
        .collect()
}

/// Filter kills by headshot
pub fn filter_kills_by_headshot(kills: &[Kill], headshot: bool) -> Vec<Kill> {
    kills
        .iter()
        .filter(|kill| kill.headshot == headshot)
        .cloned()
        .collect()
}

/// Filter kills by killer team
pub fn filter_kills_by_killer_team(kills: &[Kill], killer_team: &str) -> Vec<Kill> {
    kills
        .iter()
        .filter(|kill| kill.killer_team == killer_team)
        .cloned()
        .collect()
}

/// Filter kills by victim team
pub fn filter_kills_by_victim_team(kills: &[Kill], victim_team: &str) -> Vec<Kill> {
    kills
        .iter()
        .filter(|kill| kill.victim_team == victim_team)
        .cloned()
        .collect()
}

/// Filter kills by killer name
pub fn filter_kills_by_killer_name(kills: &[Kill], killer_name: &str) -> Vec<Kill> {
    kills
        .iter()
        .filter(|kill| kill.killer_name == killer_name)
        .cloned()
        .collect()
}

/// Filter kills by victim name
pub fn filter_kills_by_victim_name(kills: &[Kill], victim_name: &str) -> Vec<Kill> {
    kills
        .iter()
        .filter(|kill| kill.victim_name == victim_name)
        .cloned()
        .collect()
}

/// Filter kills by penetrated
pub fn filter_kills_by_penetrated(kills: &[Kill], penetrated: bool) -> Vec<Kill> {
    kills
        .iter()
        .filter(|kill| kill.penetrated == penetrated)
        .cloned()
        .collect()
}

/// Filter kills by attacker blind
pub fn filter_kills_by_attacker_blind(kills: &[Kill], attacker_blind: bool) -> Vec<Kill> {
    kills
        .iter()
        .filter(|kill| kill.attacker_blind == attacker_blind)
        .cloned()
        .collect()
}

/// Filter kills by through smoke
pub fn filter_kills_by_thru_smoke(kills: &[Kill], thru_smoke: bool) -> Vec<Kill> {
    kills
        .iter()
        .filter(|kill| kill.thru_smoke == thru_smoke)
        .cloned()
        .collect()
}

/// Filter kills by no scope
pub fn filter_kills_by_no_scope(kills: &[Kill], no_scope: bool) -> Vec<Kill> {
    kills
        .iter()
        .filter(|kill| kill.no_scope == no_scope)
        .cloned()
        .collect()
}
