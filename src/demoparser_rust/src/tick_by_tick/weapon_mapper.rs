//! Weapon mapping utility for CS2 demo parsing
//!
//! This module provides utility functions for mapping weapon names to their corresponding IDs
//! and handling weapon ID normalization for CS2 demos.

use std::collections::HashMap;

/// Weapon ID constants for common weapons
pub mod weapon_ids {
    pub const MP9: u32 = 34;
    pub const MP5SD: u32 = 23;
    pub const DEFAULT_WEAPON: u32 = 0;
}

/// Create a mapping of weapon names to their numeric IDs
///
/// This function creates a HashMap for efficient weapon name to ID lookups.
/// Based on the CS2 weapon ID mappings.
pub fn create_weapon_name_to_id_map() -> HashMap<String, u32> {
    let mut map = HashMap::new();

    // Pistols
    map.insert("glock".to_string(), 4);
    map.insert("usp_silencer".to_string(), 61);
    map.insert("hkp2000".to_string(), 32);
    map.insert("p250".to_string(), 36);
    map.insert("fiveseven".to_string(), 3);
    map.insert("deagle".to_string(), 1);
    map.insert("elite".to_string(), 2);
    map.insert("tec9".to_string(), 30);
    map.insert("cz75a".to_string(), 63);
    map.insert("revolver".to_string(), 64);

    // SMGs
    map.insert("mac10".to_string(), 17);
    map.insert("mp9".to_string(), 34);
    map.insert("mp7".to_string(), 33);
    map.insert("ump45".to_string(), 24);
    map.insert("p90".to_string(), 19);
    map.insert("bizon".to_string(), 26);
    map.insert("mp5sd".to_string(), 23);

    // Rifles
    map.insert("ak47".to_string(), 7);
    map.insert("m4a1".to_string(), 16);
    map.insert("m4a4".to_string(), 16);
    map.insert("m4a1_silencer".to_string(), 60);
    map.insert("famas".to_string(), 10);
    map.insert("galilar".to_string(), 13);
    map.insert("aug".to_string(), 8);
    map.insert("sg556".to_string(), 39);

    // Snipers
    map.insert("awp".to_string(), 9);
    map.insert("ssg08".to_string(), 40);
    map.insert("g3sg1".to_string(), 11);
    map.insert("scar20".to_string(), 38);

    // Heavy
    map.insert("nova".to_string(), 35);
    map.insert("xm1014".to_string(), 25);
    map.insert("sawedoff".to_string(), 29);
    map.insert("mag7".to_string(), 27);
    map.insert("m249".to_string(), 14);
    map.insert("negev".to_string(), 28);

    // Grenades
    map.insert("hegrenade".to_string(), 44);
    map.insert("flashbang".to_string(), 43);
    map.insert("smokegrenade".to_string(), 45);
    map.insert("molotov".to_string(), 46);
    map.insert("incgrenade".to_string(), 48);
    map.insert("decoy".to_string(), 47);
    map.insert("inferno".to_string(), 48);

    // Knives (default and T-side)
    map.insert("knife".to_string(), 42);
    map.insert("knife_t".to_string(), 59);

    // Knife skins
    map.insert("bayonet".to_string(), 500);
    map.insert("knife_survival_bowie".to_string(), 514);
    map.insert("knife_butterfly".to_string(), 515);
    map.insert("knife_falchion".to_string(), 512);
    map.insert("knife_flip".to_string(), 505);
    map.insert("knife_gut".to_string(), 506);
    map.insert("knife_karambit".to_string(), 507);
    map.insert("knife_m9_bayonet".to_string(), 508);
    map.insert("knife_tactical".to_string(), 509);
    map.insert("knife_push".to_string(), 516);
    map.insert("knife_stiletto".to_string(), 522);
    map.insert("knife_ursus".to_string(), 519);
    map.insert("knife_gypsy_jackknife".to_string(), 520);
    map.insert("knife_widowmaker".to_string(), 523);
    map.insert("knife_css".to_string(), 503);
    map.insert("knife_cord".to_string(), 517);
    map.insert("knife_canis".to_string(), 518);
    map.insert("knife_outdoor".to_string(), 521);
    map.insert("knife_skeleton".to_string(), 525);

    // Other
    map.insert("taser".to_string(), 31);
    map.insert("healthshot".to_string(), 57);
    map.insert("c4".to_string(), 49);

    // Display names emitted by csgoproto::maps::WEAPINDICIES. The parser's
    // weapon_name column uses these human-readable labels while most of the
    // map above uses internal entity names.
    for (name, id) in [
        ("desert eagle", 1),
        ("dual berettas", 2),
        ("five-seven", 3),
        ("glock-18", 4),
        ("ak-47", 7),
        ("galil ar", 13),
        ("mac-10", 17),
        ("mp5-sd", 23),
        ("ump-45", 24),
        ("pp-bizon", 26),
        ("mag-7", 27),
        ("sawed-off", 29),
        ("tec-9", 30),
        ("zeus x27", 31),
        ("p2000", 32),
        ("scar-20", 38),
        ("sg 553", 39),
        ("ssg 08", 40),
        ("high explosive grenade", 44),
        ("smoke grenade", 45),
        ("decoy grenade", 47),
        ("incendiary grenade", 48),
        ("c4 explosive", 49),
        ("medi-shot", 57),
        ("m4a1-s", 60),
        ("usp-s", 61),
        ("cz75-auto", 63),
        ("r8 revolver", 64),
        ("classic knife", 503),
        ("flip knife", 505),
        ("gut knife", 506),
        ("karambit", 507),
        ("m9 bayonet", 508),
        ("huntsman knife", 509),
        ("falchion knife", 512),
        ("bowie knife", 514),
        ("butterfly knife", 515),
        ("shadow daggers", 516),
        ("paracord knife", 517),
        ("survival knife", 518),
        ("ursus knife", 519),
        ("navaja knife", 520),
        ("nomad knife", 521),
        ("stiletto knife", 522),
        ("talon knife", 523),
        ("skeleton knife", 525),
        ("kukri knife", 526),
    ] {
        map.insert(name.to_string(), id);
    }

    map
}

fn normalize_weapon_name(weapon_name: &str) -> String {
    let normalized = weapon_name.trim().to_ascii_lowercase();
    normalized
        .strip_prefix("weapon_")
        .unwrap_or(&normalized)
        .to_string()
}

/// Map weapon name to its corresponding numeric ID
///
/// # Arguments
/// * `weapon_name` - The weapon name to map
///
/// # Returns
/// * The numeric weapon ID, or 0 if weapon not recognized
fn map_weapon_name_to_id(weapon_name: &str) -> u32 {
    let map = create_weapon_name_to_id_map();
    map.get(&normalize_weapon_name(weapon_name))
        .copied()
        .unwrap_or(weapon_ids::DEFAULT_WEAPON)
}

/// Get the correct weapon ID for a given weapon name.
/// If the current ID matches the expected ID, return it.
/// Otherwise, return the expected ID from the mapping.
#[allow(dead_code)]
pub fn get_correct_weapon_id(weapon_name: &str, current_id_str: &str) -> String {
    let expected_id = map_weapon_name_to_id(weapon_name);
    if expected_id > 0 {
        let expected_id_str = expected_id.to_string();
        if current_id_str == expected_id_str {
            current_id_str.to_string()
        } else {
            expected_id_str
        }
    } else {
        current_id_str.to_string()
    }
}

/// Normalize weapon ID for CS2 compatibility
///
/// Item definition IDs emitted by the parser are already canonical. Preserve
/// them so distinct weapons (notably MP5-SD 23 and MP9 34) stay distinct.
///
/// # Arguments
/// * `raw_weapon_id` - The raw weapon ID from the demo data
///
/// # Returns
/// * The normalized weapon ID
pub fn normalize_weapon_id(raw_weapon_id: u32) -> u32 {
    raw_weapon_id
}

/// Get weapon ID with fallback logic
///
/// Attempts to get a valid weapon ID, applying normalization and fallbacks as needed.
///
/// # Arguments
/// * `raw_id` - The raw weapon ID from demo data
/// * `weapon_name` - Optional weapon name for fallback lookup
///
/// # Returns
/// * A valid weapon ID, or 0 when neither source identifies the weapon
pub fn get_weapon_id_with_fallback(raw_id: Option<u32>, weapon_name: Option<&str>) -> u32 {
    // Try raw ID first
    if let Some(id) = raw_id {
        if id > 0 {
            return normalize_weapon_id(id);
        }
    }

    // Try weapon name lookup
    if let Some(name) = weapon_name {
        let id = map_weapon_name_to_id(name);
        if id > 0 {
            return id;
        }
    }

    weapon_ids::DEFAULT_WEAPON
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_weapon_name_mapping() {
        assert_eq!(map_weapon_name_to_id("ak47"), 7);
        assert_eq!(map_weapon_name_to_id("mp9"), weapon_ids::MP9);
        assert_eq!(map_weapon_name_to_id("MP9"), weapon_ids::MP9);
        assert_eq!(map_weapon_name_to_id("weapon_mp9"), weapon_ids::MP9);
        assert_eq!(map_weapon_name_to_id("AK-47"), 7);
        assert_eq!(map_weapon_name_to_id("M4A1-S"), 60);
        assert_eq!(map_weapon_name_to_id("USP-S"), 61);
        assert_eq!(
            map_weapon_name_to_id("unknown_weapon"),
            weapon_ids::DEFAULT_WEAPON
        );
    }

    #[test]
    fn test_weapon_id_normalization() {
        assert_eq!(normalize_weapon_id(weapon_ids::MP5SD), weapon_ids::MP5SD);
        assert_eq!(normalize_weapon_id(7), 7);
        assert_eq!(normalize_weapon_id(0), weapon_ids::DEFAULT_WEAPON);
    }

    #[test]
    fn test_fallback_logic() {
        assert_eq!(get_weapon_id_with_fallback(Some(7), None), 7);
        assert_eq!(
            get_weapon_id_with_fallback(Some(23), None),
            weapon_ids::MP5SD
        );
        assert_eq!(get_weapon_id_with_fallback(None, Some("ak47")), 7);
        assert_eq!(
            get_weapon_id_with_fallback(Some(0), Some("MP9")),
            weapon_ids::MP9
        );
        assert_eq!(
            get_weapon_id_with_fallback(None, None),
            weapon_ids::DEFAULT_WEAPON
        );
    }
}
